//! End-to-end tests of the directory walk on a real temp directory.
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::Ordering;

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

#[test]
fn long_path() {
    let tmp = tempfile::tempdir().unwrap();
    let mut deep: PathBuf = tmp.path().to_path_buf();
    for i in 0..30 {
        deep.push(format!("verzeichnis_{i:08}")); // 20 characters
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
fn missing_root() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("gibt_es_nicht");
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
fn unreadable_dir_is_marked() {
    let tmp = tempfile::tempdir().unwrap();
    let zu = tmp.path().join("zu");
    fs::create_dir(&zu).unwrap();
    write_file(&zu.join("f"), 500);
    let guard = AclGuard(zu.clone());
    let out = Command::new("icacls").arg(&zu).args(["/deny", "*S-1-1-0:(RD)"]).output().expect("run icacls");
    assert!(out.status.success(), "icacls failed: {out:?}");
    assert!(fs::read_dir(&zu).is_err(), "icacls did not deny listing in this environment");

    let r = scan_walk(tmp.path(), &Progress::default()).unwrap();
    let t = &r.tree;
    let z = child(t, t.root(), "zu");
    assert!(t.flags(z) & F_UNREADABLE != 0);
    assert!(t.children(z).is_empty());
    assert_eq!(r.unreadable, 1);
    drop(guard);
    assert!(fs::read_dir(&zu).is_ok(), "ACL was not restored");
}
