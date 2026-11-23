//! Guards for the anti-goals: the program never writes to or deletes from a
//! scanned drive and never uses the network. The source is searched line by
//! line; the few sanctioned lines are listed explicitly.
use std::fs;
use std::path::Path;

/// Text that must not appear in `src/`: writing, deleting, changing files or
/// talking to devices directly, and starting processes.
const BANNED_WRITE: &[&str] = &[
    "GENERIC_WRITE",
    "GENERIC_ALL",
    "FILE_WRITE_DATA",
    "FILE_APPEND_DATA",
    "FILE_WRITE_ATTRIBUTES",
    "FILE_WRITE_EA",
    "FILE_ADD_FILE",
    "FILE_ADD_SUBDIRECTORY",
    "FILE_DELETE_CHILD",
    "FILE_ALL_ACCESS",
    "WRITE_DAC",
    "WRITE_OWNER",
    "WriteFile",
    "DeleteFileW",
    "RemoveDirectoryW",
    "MoveFileW",
    "MoveFileExW",
    "CopyFileW",
    "SetFileAttributesW",
    "SetFileInformationByHandle",
    "NtSetInformationFile",
    "SHFileOperationW",
    "DeviceIoControl",
    "NtCreateFile",
    "CreateFile",
    "CreateDirectory",
    "CreateHardLink",
    "CreateSymbolicLink",
    "remove_file",
    "remove_dir",
    "fs::write",
    "fs::rename",
    "fs::copy",
    "fs::hard_link",
    "fs::soft_link",
    "symlink_dir",
    "symlink_file",
    "File::create",
    "create_dir",
    // A method call on a file, or `File::set_len(..)`: not any identifier
    // that merely contains the word.
    ".set_len(",
    "::set_len(",
    "set_permissions",
    ".write(true)",
    ".append(true)",
    ".create(true)",
    ".create_new(true)",
    ".truncate(true)",
    // Open options that set the access mask or the open flags directly.
    ".access_mode(",
    ".custom_flags(",
    // Whatever access the caller may get, write and delete included.
    "MAXIMUM_ALLOWED",
    // File system control requests, which can change a volume.
    "NtFsControlFile",
    // An open that deletes the file when its handle is closed.
    "FILE_DELETE_ON_CLOSE",
    "\"cmd\"",
    "cmd.exe",
    // Starting a process is allowlisted, not free (see `ALLOWED`).
    "Command::new",
    "process::Command",
    "ShellExecute",
    "CreateProcess",
    "WinExec",
];

/// Banned regardless of case (`powershell`, `PowerShell`, ...).
const BANNED_NOCASE: &[&str] = &["powershell"];

/// Network access: banned in `src/` and among the `windows` crate features.
const BANNED_NETWORK: &[&str] = &["std::net", "Win32_Networking"];

/// Banned only as a whole identifier: `DELETE` access is banned,
/// `FILE_SHARE_DELETE` (letting others delete) is not.
const BANNED_WORDS: &[&str] = &["DELETE"];

/// The sanctioned exceptions: (file below the crate root, exact line without
/// leading and trailing whitespace, number of times that line must occur in
/// the file). The test fails if the count differs in either direction, and if
/// an entry matches nothing.
const ALLOWED: &[(&str, &str, usize)] = &[
    // `bench --out <file>` writes its report to the file the user names.
    ("src/bin/bench.rs", "&& let Err(e) = std::fs::write(path, &text)", 1),
    // Test fixture in `#[cfg(test)]` code: a file in a fresh temp folder.
    ("src/scan/walk.rs", "std::fs::write(&file, b\"abc\").unwrap();", 1),
    // The one place that starts a process: Explorer, to reveal an entry.
    ("src/win.rs", "use std::os::windows::process::CommandExt;", 1), // the extension trait for `raw_arg`
    ("src/win.rs", "use windows::Win32::UI::Shell::ShellExecuteW;", 1), // import for the restart as administrator
    (
        "src/win.rs",
        "let result = unsafe { ShellExecuteW(None, w!(\"runas\"), &exe, &params, dir_ptr, SW_SHOWNORMAL) };",
        1,
    ), // restarts the program itself with administrator rights
    ("src/win.rs", "let _ = std::process::Command::new(program).raw_arg(arg).spawn();", 1),
];

/// Package-name prefixes of network libraries: a lock-file package whose name
/// starts with one of these fails the test.
const NETWORK_CRATE_PREFIXES: &[&str] =
    &["reqwest", "hyper", "ureq", "curl", "tungstenite", "tokio-tungstenite", "h2", "native-tls", "rustls", "openssl"];

/// Calls whose access mask (second argument) must be a named constant.
const MASK_CALLS: &[&str] = &["NtOpenFile", "CreateFileW"];

struct Rules<'a> {
    substrings: &'a [&'a str],
    nocase: &'a [&'a str],
    words: &'a [&'a str],
}

/// True if `word` occurs in `line` with no identifier character on either side.
fn contains_word(line: &str, word: &str) -> bool {
    let ident = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
    line.match_indices(word)
        .any(|(i, _)| !ident(line[..i].chars().next_back()) && !ident(line[i + word.len()..].chars().next()))
}

/// `line` without its `//` comment. A `//` inside a string literal (plain or
/// `r"..."`) or a `'"'` character literal does not start a comment. Where
/// the scan cannot tell, it keeps the text: more is checked, never less.
fn code_of(line: &str) -> &str {
    let bytes = line.as_bytes();
    let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'/' if bytes.get(i + 1) == Some(&b'/') => return &line[..i],
            // `'"'` and `'\"'`: a quote character, not the start of a string.
            b'\'' if bytes.get(i + 1) == Some(&b'"') && bytes.get(i + 2) == Some(&b'\'') => i += 3,
            b'\'' if bytes.get(i + 1) == Some(&b'\\') && bytes.get(i + 3) == Some(&b'\'') => i += 4,
            b'"' => {
                let raw = i > 0 && bytes[i - 1] == b'r' && !(i > 1 && ident(bytes[i - 2]));
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    // A backslash takes the next character with it, except in a raw string.
                    i += if bytes[i] == b'\\' && !raw { 2 } else { 1 };
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    line
}

/// The banned text in the code of `text` (the contents of `file`) as
/// `file:line: word`, except on lines in `allowed`; adds one to `counts[i]`
/// for every line that allowed entry `i` covers. Comments are not code: a
/// comment may name a banned call.
fn check(file: &str, text: &str, rules: &Rules, allowed: &[(&str, &str, usize)], counts: &mut [usize]) -> Vec<String> {
    let mut found = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let code = code_of(line);
        let lower = code.to_lowercase();
        let hits: Vec<&str> = rules
            .substrings
            .iter()
            .filter(|w| code.contains(**w))
            .chain(rules.nocase.iter().filter(|w| lower.contains(**w)))
            .chain(rules.words.iter().filter(|w| contains_word(code, w)))
            .copied()
            .collect();
        if hits.is_empty() {
            continue;
        }
        if let Some(i) = allowed.iter().position(|&(f, l, _)| f == file && l == line.trim()) {
            counts[i] += 1;
            continue;
        }
        found.extend(hits.into_iter().map(|w| format!("{file}:{}: `{w}`", n + 1)));
    }
    found
}

/// Messages for every allowlist entry whose observed count differs from the
/// expected one (zero observed means the entry matches nothing).
fn allowlist_errors(allowed: &[(&str, &str, usize)], counts: &[usize]) -> Vec<String> {
    allowed
        .iter()
        .zip(counts)
        .filter(|((_, _, want), got)| want != *got)
        .map(|((file, line, want), got)| {
            format!("allowlist entry {file}: `{line}` expected {want} time(s), found {got}")
        })
        .collect()
}

/// True if `arg` contains a numeric-literal token: a digit that starts a
/// token, i.e. is not preceded by an identifier character or `.` (so
/// `access.0` and `mask2` are clean, `A | 0x2`, `(0x1)`, `1 << 30` are not).
fn contains_numeric_literal(arg: &str) -> bool {
    let mut prev: Option<char> = None;
    for c in arg.chars() {
        if c.is_ascii_digit() && !prev.is_some_and(|p| p.is_alphanumeric() || p == '_' || p == '.') {
            return true;
        }
        prev = Some(c);
    }
    false
}

/// True if `arg` builds an access mask from a number instead of named
/// rights: a numeric literal, or the largest value of an integer type
/// (`u32::MAX`, `u32::max_value()`), which sets every bit.
fn is_literal_mask(arg: &str) -> bool {
    contains_numeric_literal(arg) || arg.contains("::MAX") || arg.contains("max_value")
}

/// Every call of `name` in `text`: the number of the line it starts on and
/// the text after its opening parenthesis, to the end of `text`, without
/// `//` comments. The name may be followed by spaces or a line break before
/// the parenthesis, and the arguments may span any number of lines.
fn calls(text: &str, name: &str) -> Vec<(usize, String)> {
    let code: String = text.lines().map(code_of).collect::<Vec<_>>().join("\n");
    let ident = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
    let mut out = Vec::new();
    for (at, _) in code.match_indices(name) {
        if ident(code[..at].chars().next_back()) {
            continue;
        }
        let after = code[at + name.len()..].trim_start();
        if let Some(args) = after.strip_prefix('(') {
            out.push((code[..at].matches('\n').count() + 1, args.to_string()));
        }
    }
    out
}

/// True if `arg` is a numeric literal: `0x..`, `0b..`, `0o..` or decimal, with
/// optional `_` separators and an optional integer suffix such as `u32`.
fn is_numeric_literal(arg: &str) -> bool {
    const SUFFIXES: [&str; 10] = ["u8", "u16", "u32", "u64", "usize", "i8", "i16", "i32", "i64", "isize"];
    let digits = |s: &str, ok: fn(char) -> bool| s.chars().any(ok) && s.chars().all(|c| ok(c) || c == '_');
    let without_suffix = |s: &str, ok: fn(char) -> bool| {
        digits(s, ok) || SUFFIXES.iter().any(|suf| s.strip_suffix(suf).is_some_and(|r| digits(r, ok)))
    };
    let a = arg.trim();
    if let Some(h) = a.strip_prefix("0x").or_else(|| a.strip_prefix("0X")) {
        return without_suffix(h, |c| c.is_ascii_hexdigit());
    }
    if let Some(b) = a.strip_prefix("0b") {
        return without_suffix(b, |c| c == '0' || c == '1');
    }
    if let Some(o) = a.strip_prefix("0o") {
        return without_suffix(o, |c| ('0'..='7').contains(&c));
    }
    without_suffix(a, |c| c.is_ascii_digit())
}

/// The arguments of the call whose text after the opening parenthesis is
/// `args`, split at top-level commas; stops at the closing parenthesis.
fn call_args(args: &str) -> Vec<String> {
    let (mut depth, mut cur, mut out) = (0usize, String::new(), Vec::new());
    for c in args.chars() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' if depth == 0 => break,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                out.push(std::mem::take(&mut cur));
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    out.push(cur);
    out
}

/// Reports a call to `NtOpenFile` or `CreateFileW` whose access argument (the
/// second one) is built from a number, alone or combined with names (see
/// `is_literal_mask`). A number in any other argument or in a comment is not
/// reported.
fn numeric_access_masks(file: &str, text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for call in MASK_CALLS {
        for (line, args) in calls(text, call) {
            if call_args(&args).get(1).is_some_and(|a| is_literal_mask(a)) {
                found.push((line, format!("{file}:{line}: numeric access mask in `{call}`")));
            }
        }
    }
    found.sort();
    found.into_iter().map(|(_, message)| message).collect()
}

/// Reports `FILE_ACCESS_RIGHTS(` with a number in it (see `is_literal_mask`):
/// building an access mask from a number, wherever the mask is later used.
fn numeric_access_rights(file: &str, text: &str) -> Vec<String> {
    calls(text, "FILE_ACCESS_RIGHTS")
        .into_iter()
        .filter(|(_, args)| call_args(args).first().is_some_and(|a| is_literal_mask(a)))
        .map(|(line, _)| format!("{file}:{line}: numeric literal in `FILE_ACCESS_RIGHTS(`"))
        .collect()
}

/// Names of lock-file packages that start with a network-library prefix.
fn network_crates(lock: &str) -> Vec<String> {
    lock.lines()
        .filter_map(|l| l.trim().strip_prefix("name = \"")?.strip_suffix('"'))
        .filter(|name| NETWORK_CRATE_PREFIXES.iter().any(|p| name.starts_with(p)))
        .map(str::to_string)
        .collect()
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
    let rules = Rules { substrings: &substrings, nocase: BANNED_NOCASE, words: BANNED_WORDS };
    let mut counts = vec![0usize; ALLOWED.len()];
    let mut found = Vec::new();
    for file in &files {
        let name = file.strip_prefix(root).expect("below the crate root").to_string_lossy().replace('\\', "/");
        let text = fs::read_to_string(file).expect("read source");
        found.extend(check(&name, &text, &rules, ALLOWED, &mut counts));
        found.extend(numeric_access_masks(&name, &text));
        found.extend(numeric_access_rights(&name, &text));
    }
    assert!(found.is_empty(), "banned text in src/:\n{}", found.join("\n"));
    let errors = allowlist_errors(ALLOWED, &counts);
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}

#[test]
fn no_network_features_in_manifest() {
    let manifest =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")).expect("read Cargo.toml");
    assert!(manifest.contains("name = \"platzhirsch\""), "Cargo.toml looks wrong");
    let rules = Rules { substrings: BANNED_NETWORK, nocase: &[], words: &[] };
    let found = check("Cargo.toml", &manifest, &rules, &[], &mut []);
    assert!(found.is_empty(), "network feature in Cargo.toml:\n{}", found.join("\n"));
}

#[test]
fn checker_reports_banned_text_and_skips_allowed_lines() {
    let rules = Rules { substrings: BANNED_WRITE, nocase: BANNED_NOCASE, words: BANNED_WORDS };
    let allowed = [("src/x.rs", "std::fs::write(p, b\"ok\")?;", 1)];
    let text = "fn a() {\n    std::fs::write(p, b\"ok\")?;\n    let f = File::create(p)?;\n    let m = DELETE;\n    let s = FILE_SHARE_DELETE;\n}\n";
    let mut counts = [0];
    let found = check("src/x.rs", text, &rules, &allowed, &mut counts);
    assert_eq!(found, ["src/x.rs:3: `File::create`", "src/x.rs:4: `DELETE`"]);
    assert_eq!(counts, [1]);
    assert!(allowlist_errors(&allowed, &counts).is_empty());

    // The same line in another file is not allowed.
    let mut counts = [0];
    let found = check("src/y.rs", "    std::fs::write(p, b\"ok\")?;", &rules, &allowed, &mut counts);
    assert_eq!(found, ["src/y.rs:1: `fs::write`"]);
    assert_eq!(counts, [0]);
    assert_eq!(allowlist_errors(&allowed, &counts).len(), 1, "an entry that matches nothing fails");

    // A comment may name a banned call; code before or after a `//` in a string may not.
    for comment in [
        "// ShellExecuteW reports success with a value above 32.",
        "    let n = 1; // WriteFile is never called",
        "/// Unlike `File::create`, this opens for reading.",
        r#"    let quote = '"'; // CreateFile"#,
        r#"    let prefix = r"\\?\"; // DeleteFileW"#,
    ] {
        assert!(check("src/x.rs", comment, &rules, &[], &mut []).is_empty(), "reported: {comment}");
    }
    for (code, word) in [
        (r#"let s = "//"; WriteFile(h);"#, "WriteFile"),
        (r#"is_device_path(Path::new("//./C:")); DeleteFileW(p);"#, "DeleteFileW"),
        (r#"let s = "a \" // b"; RemoveDirectoryW(p); // done"#, "RemoveDirectoryW"),
        (r#"let c = '"'; let s = "x // y"; MoveFileW(a, b);"#, "MoveFileW"),
        ("MoveFileExW(a, b, f); // moves", "MoveFileExW"),
    ] {
        let found = check("src/x.rs", code, &rules, &[], &mut []);
        assert!(found.contains(&format!("src/x.rs:1: `{word}`")), "not reported: {code}");
    }
    assert_eq!(code_of(r#"let a = "x"; // "quoted" comment"#), r#"let a = "x"; "#);
    assert_eq!(code_of(r#"let url = "http://x";"#), r#"let url = "http://x";"#);

    assert!(contains_word("x = DELETE | y", "DELETE"));
    assert!(!contains_word("FILE_SHARE_DELETE", "DELETE"));
    assert!(!contains_word("DELETED", "DELETE"));
}

#[test]
fn allowlist_counts_must_match_exactly() {
    let rules = Rules { substrings: BANNED_WRITE, nocase: BANNED_NOCASE, words: BANNED_WORDS };
    let allowed = [("src/x.rs", "std::fs::write(p, b)?;", 1)];
    let line = "    std::fs::write(p, b)?;\n";
    for (text, ok) in [(line.to_string(), true), (line.repeat(2), false), (String::new(), false)] {
        let mut counts = [0];
        let found = check("src/x.rs", &text, &rules, &allowed, &mut counts);
        assert!(found.is_empty());
        assert_eq!(allowlist_errors(&allowed, &counts).is_empty(), ok, "{} occurrence(s)", counts[0]);
    }
}

#[test]
fn checker_flags_new_write_and_process_rules() {
    let rules = Rules { substrings: BANNED_WRITE, nocase: BANNED_NOCASE, words: BANNED_WORDS };
    let hit = |line: &str| check("src/x.rs", line, &rules, &[], &mut []).len();
    for banned in [
        "CreateDirectoryW(p, None)",
        "SetFileInformationByHandle(h)",
        "NtSetInformationFile(h)",
        "std::fs::hard_link(a, b)",
        "std::fs::soft_link(a, b)",
        "std::os::windows::fs::symlink_dir(a, b)",
        "std::os::windows::fs::symlink_file(a, b)",
        "CreateHardLinkW(a, b)",
        "CreateSymbolicLinkW(a, b)",
        "let a = FILE_WRITE_ATTRIBUTES;",
        "let a = WRITE_DAC;",
        "let a = WRITE_OWNER;",
        "let c = Command::new(\"x\");",
        "file.set_len(0)?;",
        "File::set_len(&f, 0)?;",
    ] {
        assert!(hit(banned) >= 1, "not reported: {banned}");
    }
    for fine in
        ["let n = v.reset_len(3);", "let n = v.set_length(3);", "let a = FILE_READ_ATTRIBUTES;", "let c = Cmd::now();"]
    {
        assert_eq!(hit(fine), 0, "reported: {fine}");
    }
}

#[test]
fn numeric_access_mask_rule() {
    let hex = "let s = NtOpenFile(&mut h, 0x80100000, &a, &mut io, 7, 0);";
    let dec = "unsafe { CreateFileW(name, 1179785, share, None, mode, flags, None) }";
    let split = "NtOpenFile(\n    &mut h,\n    0x1,\n    &a,\n)";
    let named = "NtOpenFile(&mut handle, access.0, &attributes, &mut iosb, 7, options)";
    let consts = "NtOpenFile(\n    &mut h,\n    (FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | SYNCHRONIZE).0,\n    &a,\n    0x7,\n)";
    let later_arg = "CreateFileW(name, FILE_READ_ATTRIBUTES.0, 0x7, None, mode, flags, None)";
    let in_comment = "NtOpenFile(&mut h, access.0, &a, &mut io, 7, 0); // was 0x80100000";
    let comment_line = "NtOpenFile(\n    &mut h,\n    // 0x80100000\n    access.0,\n)";
    for bad in [hex, dec, split] {
        assert_eq!(numeric_access_masks("src/x.rs", bad).len(), 1, "not reported: {bad}");
    }
    for ok in [named, consts, later_arg, in_comment, comment_line, "// NtOpenFile(&mut h, 0x1, &a)", "let x = 0x1;"] {
        assert!(numeric_access_masks("src/x.rs", ok).is_empty(), "reported: {ok}");
    }
    assert_eq!(
        numeric_access_masks("src/x.rs", "a\nb\n  NtOpenFile(&mut h, 5, &a)"),
        ["src/x.rs:3: numeric access mask in `NtOpenFile`"]
    );
    for masked in [
        "NtOpenFile(&mut h, FILE_READ_ATTRIBUTES.0 | 0x100000, &a)",
        "NtOpenFile(&mut h, (A | 0x2).0, &a)",
        "NtOpenFile(&mut h, 0x1 as u32, &a)",
        "NtOpenFile(&mut h, (0x1), &a)",
        "NtOpenFile(&mut h, 1 << 30, &a)",
    ] {
        assert_eq!(numeric_access_masks("src/x.rs", masked).len(), 1, "not reported: {masked}");
    }
    for clean in [
        "NtOpenFile(&mut h, access.0, &a)",
        "NtOpenFile(&mut h, FILE_LIST_DIRECTORY | SYNCHRONIZE, &a)",
        "NtOpenFile(&mut h, rights.0 | other.0, &a)",
        "NtOpenFile(&mut h, mask2, &a)",
        "NtOpenFile(&mut h, FILE_SHARE_READ2, &a)",
    ] {
        assert!(numeric_access_masks("src/x.rs", clean).is_empty(), "reported: {clean}");
    }
    // A space or a line break before the parenthesis, the mask many lines down,
    // and the largest value of a type, which sets every bit.
    for masked in [
        "NtOpenFile (&mut h, 5, &a)",
        "NtOpenFile\n    (&mut h, 5, &a)",
        "NtOpenFile(\n    &mut h,\n\n\n\n    0x7,\n    &a,\n)",
        "NtOpenFile(&mut h, u32::MAX, &a)",
        "NtOpenFile(&mut h, u32::max_value(), &a)",
        "CreateFileW(name, !0, share, None, mode, flags, None)",
    ] {
        assert_eq!(numeric_access_masks("src/x.rs", masked).len(), 1, "not reported: {masked}");
    }
    // Another function whose name only ends like this one is not meant.
    assert!(numeric_access_masks("src/x.rs", "MyNtOpenFile(&mut h, 5, &a)").is_empty());
    assert!(is_numeric_literal("0x80_10u32") && is_numeric_literal("12") && is_numeric_literal("0b101"));
    assert!(
        !is_numeric_literal("FILE_LIST_DIRECTORY.0") && !is_numeric_literal("access.0") && !is_numeric_literal("0xZZ")
    );
}

#[test]
fn access_rights_literal_and_new_bans() {
    assert_eq!(numeric_access_rights("src/x.rs", "let a = FILE_ACCESS_RIGHTS(0x2);").len(), 1);
    assert_eq!(numeric_access_rights("src/x.rs", "let a = FILE_ACCESS_RIGHTS(1 << 3);").len(), 1);
    assert!(numeric_access_rights("src/x.rs", "let a = FILE_ACCESS_RIGHTS(bits);").is_empty());
    assert!(numeric_access_rights("src/x.rs", "let a = FILE_ACCESS_RIGHTS(self.0);").is_empty());
    for (numeric, line) in [
        ("let a = FILE_ACCESS_RIGHTS (0x2);", 1),
        ("let a = FILE_ACCESS_RIGHTS(\n    0x2,\n);", 1),
        ("let a =\n    FILE_ACCESS_RIGHTS\n    (\n        1 << 3\n    );", 2),
        ("let a = FILE_ACCESS_RIGHTS(u32::MAX);", 1),
        ("let a = FILE_ACCESS_RIGHTS(u32::max_value());", 1),
        ("let a = FILE_ACCESS_RIGHTS(!0);", 1),
    ] {
        let expected = format!("src/x.rs:{line}: numeric literal in `FILE_ACCESS_RIGHTS(`");
        assert_eq!(numeric_access_rights("src/x.rs", numeric), [expected], "{numeric}");
    }
    for clean in [
        "let a = FILE_ACCESS_RIGHTS(\n    // 0x2\n    bits,\n);",
        "fn rights(access: FILE_ACCESS_RIGHTS, options: u32) -> u32 { 7 }",
        "// FILE_ACCESS_RIGHTS(0x2)",
    ] {
        assert!(numeric_access_rights("src/x.rs", clean).is_empty(), "reported: {clean}");
    }

    let rules = Rules { substrings: BANNED_WRITE, nocase: BANNED_NOCASE, words: BANNED_WORDS };
    let hit = |line: &str| check("src/x.rs", line, &rules, &[], &mut []).len();
    for banned in [
        "ShellExecuteW(None)",
        "CreateProcessW(a)",
        "WinExec(a, 1)",
        "use std::process::Command;",
        "CreateDirectoryExW(a)",
        "CreateHardLinkA(a)",
        "CreateSymbolicLinkW(a)",
        "FILE_WRITE_EA",
        "FILE_ADD_FILE",
        "FILE_ADD_SUBDIRECTORY",
        "FILE_DELETE_CHILD",
        "FILE_ALL_ACCESS",
        "run(\"powershell\")",
        "run(\"PowerShell\")",
    ] {
        assert!(hit(banned) >= 1, "not reported: {banned}");
    }
    for fine in ["let p = Process::new();", "let a = FILE_ADD;", "let s = \"shell\";"] {
        assert_eq!(hit(fine), 0, "reported: {fine}");
    }
}

#[test]
fn checker_flags_raw_open_options_and_device_control() {
    let rules = Rules { substrings: BANNED_WRITE, nocase: BANNED_NOCASE, words: BANNED_WORDS };
    let hit = |line: &str| check("src/x.rs", line, &rules, &[], &mut []);
    for (banned, word) in [
        ("let f = OpenOptions::new().access_mode(0x4000_0000).open(p)?;", ".access_mode("),
        ("let f = OpenOptions::new().read(true).custom_flags(0x0400_0000).open(p)?;", ".custom_flags("),
        ("let access = MAXIMUM_ALLOWED;", "MAXIMUM_ALLOWED"),
        (
            "let s = unsafe { NtFsControlFile(h, None, None, None, &mut io, code, None, 0, None, 0) };",
            "NtFsControlFile",
        ),
        ("let options = FILE_DELETE_ON_CLOSE.0;", "FILE_DELETE_ON_CLOSE"),
    ] {
        assert_eq!(hit(banned), [format!("src/x.rs:1: `{word}`")], "{banned}");
    }
}

#[test]
fn network_crate_prefixes_in_lock_text() {
    let lock = |names: &[&str]| {
        names.iter().map(|n| format!("[[package]]\nname = \"{n}\"\nversion = \"1.0.0\"\n")).collect::<String>()
    };
    for bad in [
        "reqwest",
        "hyper-util",
        "ureq",
        "curl-sys",
        "tungstenite",
        "tokio-tungstenite",
        "h2",
        "native-tls",
        "rustls-pemfile",
        "openssl-sys",
    ] {
        assert_eq!(network_crates(&lock(&["serde", bad])), [bad], "not reported: {bad}");
    }
    assert!(network_crates(&lock(&["serde", "ahash", "egui", "windows", "chrono"])).is_empty());
}

#[test]
fn no_network_crates_in_lockfile() {
    let lock = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.lock")).expect("read Cargo.lock");
    assert!(lock.contains("name = \"platzhirsch\""), "Cargo.lock looks wrong");
    let found = network_crates(&lock);
    assert!(found.is_empty(), "network crate(s) in Cargo.lock: {found:?}");
}
