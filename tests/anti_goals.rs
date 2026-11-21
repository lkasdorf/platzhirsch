//! Guards for the anti-goals: the program never writes to or deletes from a
//! scanned drive and never uses the network. The source is searched line by
//! line; the few sanctioned lines are listed explicitly.
use std::fs;
use std::path::Path;

/// Text that must not appear in `src/`: writing, deleting, changing files or
/// talking to devices directly.
const BANNED_WRITE: &[&str] = &[
    "GENERIC_WRITE",
    "GENERIC_ALL",
    "FILE_WRITE_DATA",
    "FILE_APPEND_DATA",
    "WriteFile",
    "DeleteFileW",
    "RemoveDirectoryW",
    "MoveFileW",
    "MoveFileExW",
    "CopyFileW",
    "SetFileAttributesW",
    "SHFileOperationW",
    "DeviceIoControl",
    "NtCreateFile",
    "CreateFile",
    "remove_file",
    "remove_dir",
    "fs::write",
    "fs::rename",
    "fs::copy",
    "File::create",
    "create_dir",
    "set_len",
    "set_permissions",
    ".write(true)",
    ".append(true)",
    ".create(true)",
    ".create_new(true)",
    ".truncate(true)",
    "\"cmd\"",
    "cmd.exe",
    "powershell",
];

/// Network access: banned in `src/` and among the `windows` crate features.
const BANNED_NETWORK: &[&str] = &["std::net", "Win32_Networking"];

/// Banned only as a whole identifier: `DELETE` access is banned,
/// `FILE_SHARE_DELETE` (letting others delete) is not.
const BANNED_WORDS: &[&str] = &["DELETE"];

/// The sanctioned exceptions: (file below the crate root, exact line without
/// leading and trailing whitespace). Every entry must still match a line.
const ALLOWED: &[(&str, &str)] = &[
    // `bench --out <file>` writes its report to the file the user names.
    ("src/bin/bench.rs", "&& let Err(e) = std::fs::write(path, &text)"),
    // Test fixture in `#[cfg(test)]` code: a file in a fresh temp folder.
    ("src/scan/walk.rs", "std::fs::write(&file, b\"abc\").unwrap();"),];

struct Rules<'a> {
    substrings: &'a [&'a str],
    words: &'a [&'a str],
}

/// True if `word` occurs in `line` with no identifier character on either side.
fn contains_word(line: &str, word: &str) -> bool {
    let ident = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
    line.match_indices(word)
        .any(|(i, _)| !ident(line[..i].chars().next_back()) && !ident(line[i + word.len()..].chars().next()))
}

/// The banned text in `text` (the contents of `file`) as `file:line: word`,
/// except on lines in `allowed`; sets `used[i]` for every allowed entry hit.
fn check(file: &str, text: &str, rules: &Rules, allowed: &[(&str, &str)], used: &mut [bool]) -> Vec<String> {
    let mut found = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let hits: Vec<&str> = rules
            .substrings
            .iter()
            .filter(|w| line.contains(**w))
            .chain(rules.words.iter().filter(|w| contains_word(line, w)))
            .copied()
            .collect();
        if hits.is_empty() {
            continue;
        }
        if let Some(i) = allowed.iter().position(|&(f, l)| f == file && l == line.trim()) {
            used[i] = true;
            continue;
        }
        found.extend(hits.into_iter().map(|w| format!("{file}:{}: `{w}`", n + 1)));
    }
    found
}

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
fn no_write_delete_or_network_calls_in_source() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    collect_rs_files(&root.join("src"), &mut files);
    assert!(!files.is_empty(), "no source files found under src/");
    let substrings: Vec<&str> = BANNED_WRITE.iter().chain(BANNED_NETWORK).copied().collect();
    let rules = Rules { substrings: &substrings, words: BANNED_WORDS };
    let mut used = vec![false; ALLOWED.len()];
    let mut found = Vec::new();
    for file in &files {
        let name = file.strip_prefix(root).expect("below the crate root").to_string_lossy().replace('\\', "/");
        let text = fs::read_to_string(file).expect("read source");
        found.extend(check(&name, &text, &rules, ALLOWED, &mut used));
    }
    assert!(found.is_empty(), "banned text in src/:\n{}", found.join("\n"));
    for (entry, used) in ALLOWED.iter().zip(&used) {
        assert!(used, "allowlist entry matches no banned line any more: {entry:?}");
    }
}

#[test]
fn no_network_features_in_manifest() {
    let manifest = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")).expect("read Cargo.toml");
    assert!(manifest.contains("name = \"platzhirsch\""), "Cargo.toml looks wrong");
    let rules = Rules { substrings: BANNED_NETWORK, words: &[] };
    let found = check("Cargo.toml", &manifest, &rules, &[], &mut []);
    assert!(found.is_empty(), "network feature in Cargo.toml:\n{}", found.join("\n"));
}

#[test]
fn checker_reports_banned_text_and_skips_allowed_lines() {
    let rules = Rules { substrings: BANNED_WRITE, words: BANNED_WORDS };
    let allowed = [("src/x.rs", "std::fs::write(p, b\"ok\")?;")];
    let text = "fn a() {\n    std::fs::write(p, b\"ok\")?;\n    let f = File::create(p)?;\n    let m = DELETE;\n    let s = FILE_SHARE_DELETE;\n}\n";
    let mut used = [false];
    let found = check("src/x.rs", text, &rules, &allowed, &mut used);
    assert_eq!(found, ["src/x.rs:3: `File::create`", "src/x.rs:4: `DELETE`"]);
    assert!(used[0]);

    // The same line in another file is not allowed.
    let mut used = [false];
    let found = check("src/y.rs", "    std::fs::write(p, b\"ok\")?;", &rules, &allowed, &mut used);
    assert_eq!(found, ["src/y.rs:1: `fs::write`"]);
    assert!(!used[0]);

    assert!(contains_word("x = DELETE | y", "DELETE"));
    assert!(!contains_word("FILE_SHARE_DELETE", "DELETE"));
    assert!(!contains_word("DELETED", "DELETE"));
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
