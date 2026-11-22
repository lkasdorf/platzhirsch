//! End-to-end tests of the directory walk on a real temp directory.
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::Ordering;
use std::sync::{Arc, mpsc};

use platzhirsch::model::{F_DIR, F_LINK, F_UNREADABLE, NodeId, Tree};
use platzhirsch::scan::walk::scan_walk;
use platzhirsch::scan::{Progress, ScanError, ScanMethod};

fn child(t: &Tree, dir: NodeId, name: &str) -> NodeId {
    t.child_by_name(dir, name).unwrap_or_else(|| panic!("no child {name:?}"))
}

fn write_file(path: &Path, len: usize) {
    fs::write(path, vec![b'x'; len]).expect("write test file");
}

#[test]
fn tree_with_junction() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a");
    fs::create_dir_all(a.join("b")).unwrap();
    write_file(&a.join("f1"), 1000);
    write_file(&a.join("f2"), 0);
    write_file(&a.join("b").join("f3"), 5000);
    let status = Command::new("cmd")
        .args(["/c", "mklink", "/J"])
        .arg(a.join("j"))
        .arg(a.join("b"))
        .output()
        .expect("run mklink");
    assert!(status.status.success(), "mklink failed: {status:?}");

    let r = scan_walk(tmp.path(), &Progress::default()).unwrap();
    let t = &r.tree;
    assert_eq!((t.size(t.root()), t.file_count(t.root()), t.dir_count(t.root())), (6000, 3, 3));
    let a_id = child(t, t.root(), "a");
    let j = child(t, a_id, "j");
    assert!(t.flags(j) & F_LINK != 0);
    assert!(t.flags(j) & F_DIR != 0);
    assert!(t.children(j).is_empty());
    assert_eq!(r.method, ScanMethod::Walk);
    assert_eq!((r.skipped, r.unreadable, r.online_only), (0, 0, 0));
    let f1 = child(t, a_id, "f1");
    assert_eq!(t.size(f1), 1000);
    assert!(t.mtime(f1) > 0);
    let expected_root = tmp.path().to_string_lossy().trim_end_matches('\\').to_string();
    assert_eq!(t.name(t.root()), expected_root);
}

/// Runs `mklink` with `kind` (`/J`, `/D` or none for a file symbolic link).
/// Returns false if Windows refused it (symbolic links need a privilege
/// that an unelevated shell has only with Developer Mode on). With the
/// environment variable `CI` set, a refused link fails the test instead, so
/// the test cannot pass there without asserting anything.
fn mklink(kind: Option<&str>, link: &Path, target: &Path) -> bool {
    let mut cmd = Command::new("cmd");
    cmd.args(["/c", "mklink"]);
    if let Some(kind) = kind {
        cmd.arg(kind);
    }
    let out = cmd.arg(link).arg(target).output().expect("run mklink");
    if !out.status.success() && std::env::var_os("CI").is_some() {
        panic!("mklink {kind:?} failed in CI: {out:?}");
    }
    out.status.success()
}

/// A folder `target` with one file and a subfolder, inside `tmp`.
fn link_target(tmp: &Path) -> PathBuf {
    let target = tmp.join("target");
    fs::create_dir_all(target.join("sub")).unwrap();
    write_file(&target.join("f"), 10);
    write_file(&target.join("sub").join("g"), 20);
    target
}

fn assert_scanned_as_target(root: &Path) {
    let r = scan_walk(root, &Progress::default()).unwrap_or_else(|e| panic!("{}: {e}", root.display()));
    let t = &r.tree;
    assert_eq!(t.name(t.root()), root.to_string_lossy().trim_end_matches('\\'));
    assert_eq!(t.flags(t.root()) & F_LINK, 0);
    assert_eq!((t.size(t.root()), t.file_count(t.root()), t.dir_count(t.root())), (30, 2, 1));
    assert_eq!(t.size(child(t, t.root(), "f")), 10);
    assert_eq!((r.unreadable, r.online_only), (0, 0));
}

#[test]
fn junction_root_is_scanned_as_its_target() {
    let tmp = tempfile::tempdir().unwrap();
    let target = link_target(tmp.path());
    let j = tmp.path().join("j");
    assert!(mklink(Some("/J"), &j, &target), "mklink /J failed");
    assert_scanned_as_target(&j);
    // A link higher up in the path is resolved as well.
    let outer = tmp.path().join("outer");
    assert!(mklink(Some("/J"), &outer, tmp.path()), "mklink /J failed");
    assert_scanned_as_target(&outer.join("target"));
}

#[test]
fn symbolic_link_roots_are_scanned_as_their_target() {
    let tmp = tempfile::tempdir().unwrap();
    let target = link_target(tmp.path());
    // A directory symbolic link, and a file-type one that points to a folder.
    for (kind, name) in [(Some("/D"), "dirlink"), (None, "filelink")] {
        let link = tmp.path().join(name);
        if !mklink(kind, &link, &target) {
            eprintln!("skipped {name}: this environment may not create symbolic links");
            continue;
        }
        assert_scanned_as_target(&link);
    }
}

#[test]
fn long_path() {
    let tmp = tempfile::tempdir().unwrap();
    let mut deep: PathBuf = tmp.path().to_path_buf();
    for i in 0..30 {
        deep.push(format!("directories_{i:08}")); // 20 characters
    }
    fs::create_dir_all(&deep).unwrap();
    write_file(&deep.join("f"), 7);
    assert!(deep.as_os_str().len() > 600);

    let r = scan_walk(tmp.path(), &Progress::default()).unwrap();
    assert_eq!(r.tree.size(r.tree.root()), 7);
    assert_eq!(r.tree.dir_count(r.tree.root()), 30);
    assert_eq!(r.unreadable, 0);
}

#[test]
fn unicode_name() {
    let tmp = tempfile::tempdir().unwrap();
    let name = "größe – 日本.txt";
    write_file(&tmp.path().join(name), 3);
    let r = scan_walk(tmp.path(), &Progress::default()).unwrap();
    let t = &r.tree;
    let kids = t.children(t.root());
    assert_eq!(kids.len(), 1);
    assert_eq!(t.name(kids[0]), name);
}

#[test]
fn cancel() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir(tmp.path().join("x")).unwrap();
    let progress = Progress::default();
    progress.cancel.store(true, Ordering::Relaxed);
    assert!(matches!(scan_walk(tmp.path(), &progress), Err(ScanError::Cancelled)));
}

#[test]
fn cancel_while_walking() {
    // 20 chains of 25 nested directories: 500 directories, mostly listed one
    // after another, so the walk runs long enough for the cancel to land.
    let tmp = tempfile::tempdir().unwrap();
    for chain in 0..20 {
        let mut p = tmp.path().join(format!("c{chain}"));
        for depth in 0..25 {
            p.push(format!("d{depth}"));
        }
        fs::create_dir_all(&p).unwrap();
        write_file(&p.join("f"), 1);
    }
    let limit = std::time::Duration::from_secs(10);
    let mut cancelled = false;
    for _attempt in 0..5 {
        let progress = Arc::new(Progress::default());
        let (tx, rx) = mpsc::channel();
        // The scan runs in its own thread, so a scan that never returns fails
        // this test after the time limit instead of hanging the test run.
        let (root, scan_progress) = (tmp.path().to_path_buf(), Arc::clone(&progress));
        std::thread::spawn(move || {
            let _ = tx.send(scan_walk(&root, &scan_progress));
        });
        let started = std::time::Instant::now();
        while progress.done.load(Ordering::Relaxed) == 0 && started.elapsed() < limit {
            std::hint::spin_loop();
        }
        progress.cancel.store(true, Ordering::Relaxed);
        let result = rx.recv_timeout(limit).expect("scan did not return in time");
        match result {
            Err(ScanError::Cancelled) => {
                cancelled = true;
                break;
            }
            // The walk won the race; try again.
            Ok(_) => {}
            Err(e) => panic!("unexpected error: {e}"),
        }
    }
    assert!(cancelled, "the walk never saw the cancel");
}

#[test]
fn file_root_is_not_opened() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("file.txt");
    write_file(&file, 10);
    match scan_walk(&file, &Progress::default()) {
        Err(ScanError::Io(e)) => assert_eq!(e.kind(), std::io::ErrorKind::NotADirectory),
        other => panic!("expected Io(NotADirectory), got {:?}", other.err()),
    }
}

#[test]
fn device_paths_are_refused() {
    for root in [r"\\.\C:", r"\\.\PhysicalDrive0", r"\\.\C:\Windows", "//./C:"] {
        match scan_walk(Path::new(root), &Progress::default()) {
            Err(e @ ScanError::Io(_)) => {
                let ScanError::Io(io) = &e else { unreachable!() };
                assert_eq!(io.kind(), std::io::ErrorKind::InvalidInput, "{root}");
                assert_eq!(e.to_string(), "Read error: Device paths are not supported", "{root}");
            }
            other => panic!("{root}: expected Io(InvalidInput), got {:?}", other.err()),
        }
    }
}

#[test]
fn missing_root() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("does_not_exist");
    assert!(matches!(scan_walk(&missing, &Progress::default()), Err(ScanError::Io(_))));
}

#[test]
fn progress_counts_entries() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir(tmp.path().join("d")).unwrap();
    write_file(&tmp.path().join("d").join("f"), 1);
    write_file(&tmp.path().join("g"), 1);
    let progress = Progress::default();
    scan_walk(tmp.path(), &progress).unwrap();
    assert_eq!(progress.done.load(Ordering::Relaxed), 3);
    assert_eq!(progress.total.load(Ordering::Relaxed), 0);
}

/// Removes the deny entry again when dropped, also after a failed assertion.
struct AclGuard(PathBuf);

impl Drop for AclGuard {
    fn drop(&mut self) {
        let _ = Command::new("icacls").arg(&self.0).args(["/remove:d", "*S-1-1-0"]).output();
    }
}

#[test]
fn denied_root_is_access_denied_without_admin_hint() {
    let tmp = tempfile::tempdir().unwrap();
    let locked = tmp.path().join("locked");
    fs::create_dir(&locked).unwrap();
    let guard = AclGuard(locked.clone());
    let out = Command::new("icacls").arg(&locked).args(["/deny", "*S-1-1-0:(RD)"]).output().expect("run icacls");
    assert!(out.status.success(), "icacls failed: {out:?}");
    assert!(fs::read_dir(&locked).is_err(), "icacls did not deny listing in this environment");

    match scan_walk(&locked, &Progress::default()) {
        Err(e @ ScanError::FolderAccessDenied) => assert_eq!(e.to_string(), "Access denied"),
        other => panic!("expected FolderAccessDenied, got {:?}", other.err()),
    }
    drop(guard);
    tmp.close().expect("temp folder not removed after the ACL was restored");
}

#[test]
fn relative_root_gets_absolute_name() {
    // Tests run in the package directory.
    let r = scan_walk(Path::new("src/scan"), &Progress::default()).unwrap();
    let expected = format!("{}\\src\\scan", env!("CARGO_MANIFEST_DIR").trim_end_matches('\\'));
    assert_eq!(r.tree.name(r.tree.root()), expected);
    assert!(r.tree.child_by_name(r.tree.root(), "walk.rs").is_some());
}

#[test]
fn unreadable_dir_is_marked() {
    let tmp = tempfile::tempdir().unwrap();
    let locked = tmp.path().join("locked");
    fs::create_dir(&locked).unwrap();
    write_file(&locked.join("f"), 500);
    let guard = AclGuard(locked.clone());
    let out = Command::new("icacls").arg(&locked).args(["/deny", "*S-1-1-0:(RD)"]).output().expect("run icacls");
    assert!(out.status.success(), "icacls failed: {out:?}");
    assert!(fs::read_dir(&locked).is_err(), "icacls did not deny listing in this environment");

    let r = scan_walk(tmp.path(), &Progress::default()).unwrap();
    let t = &r.tree;
    let z = child(t, t.root(), "locked");
    assert!(t.flags(z) & F_UNREADABLE != 0);
    assert!(t.children(z).is_empty());
    assert_eq!(r.unreadable, 1);
    drop(guard);
    assert!(fs::read_dir(&locked).is_ok(), "ACL was not restored");
    tmp.close().expect("temp folder not removed after the ACL was restored");
}
