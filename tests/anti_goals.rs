use std::fs;
use std::path::Path;

fn collect_rs_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in fs::read_dir(dir).expect("read_dir") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn no_write_or_delete_calls_in_source() {
    let mut files = Vec::new();
    collect_rs_files(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files);
    assert!(!files.is_empty(), "no source files found under src/");
    for word in [
        "GENERIC_WRITE",
        "FILE_WRITE_DATA",
        "WriteFile",
        "DeleteFileW",
        "RemoveDirectoryW",
        "MoveFileW",
        "MoveFileExW",
        "remove_file",
        "remove_dir",
    ] {
        for file in &files {
            let text = fs::read_to_string(file).expect("read source");
            assert!(!text.contains(word), "banned word `{word}` found in {}", file.display());
        }
    }
}

#[test]
fn no_network_crates_in_lockfile() {
    let lock = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.lock"))
        .expect("read Cargo.lock");
    assert!(lock.contains("name = \"platzhirsch\""), "Cargo.lock looks wrong");
    for name in ["reqwest", "hyper", "ureq", "curl", "tungstenite"] {
        let needle = format!("name = \"{name}\"");
        assert!(!lock.contains(&needle), "network crate `{name}` in Cargo.lock");
    }
}
