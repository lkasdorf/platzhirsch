//! Diagnostic tool:
//! - `bench mft <X:> [--out <file>]` measures the MFT scan,
//! - `bench walk <path> [--out <file>]` measures the directory walk,
//! - `bench compare <folder> [--out <file>]` compares both for one folder,
//! - `bench message-box` shows the box a failed start of the program shows.
use std::path::{Component, Path, PathBuf, Prefix};

use platzhirsch::model::{SortKey, Tree};
use platzhirsch::scan::mft::scan_mft;
use platzhirsch::scan::walk::scan_walk;
use platzhirsch::scan::{Progress, ScanError, ScanResult, ScanTarget, io_error_text};
use platzhirsch::win::{long_path_name, memory_mb, show_error, volume_info};

const USAGE: &str = "Usage: bench mft <X:> [--out <file>]\n       bench walk <path> [--out <file>]\n       bench compare <folder> [--out <file>]
       bench message-box";
const TOP_CHILDREN: usize = 15;

enum Command {
    Mft(char),
    Walk(PathBuf),
    Compare(PathBuf),
}

/// `X:` as a drive letter.
fn drive_letter(s: &str) -> Option<char> {
    let mut chars = s.chars();
    let letter = chars.next().filter(char::is_ascii_alphabetic)?;
    (chars.as_str() == ":").then_some(letter)
}

/// The drive letter if `path` is a drive root (`X:\`, `X:` or `\\?\X:\`).
fn drive_root_of(path: &Path) -> Option<char> {
    match ScanTarget::from_path(path.to_path_buf()) {
        ScanTarget::Volume(letter) => Some(letter),
        ScanTarget::Folder(_) => None,
    }
}

fn parse_args(args: &[String]) -> Option<(Command, Option<String>)> {
    let [cmd, target, rest @ ..] = args else { return None };
    let out = match rest {
        [] => None,
        [flag, path] if flag == "--out" => Some(path.clone()),
        _ => return None,
    };
    let command = match cmd.as_str() {
        "mft" => Command::Mft(drive_letter(target)?),
        // A bare `X:` would mean the current directory on X; the root is meant.
        "walk" => Command::Walk(match drive_letter(target) {
            Some(l) => PathBuf::from(format!("{l}:\\")),
            None => PathBuf::from(target),
        }),
        "compare" => Command::Compare(PathBuf::from(target)),
        _ => return None,
    };
    Some((command, out))
}

/// The line for an output file that could not be written. Like every error
/// this tool prints it is English, whatever the system language.
fn write_failure(path: &str, e: &std::io::Error) -> String {
    format!("error: could not write {path}: {}", io_error_text(e))
}

/// Prints `lines` and, with `--out`, writes them to that file.
fn emit(lines: &[String], out: Option<&str>) {
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    print!("{text}");
    if let Some(path) = out
        && let Err(e) = std::fs::write(path, &text)
    {
        eprintln!("{}", write_failure(path, &e));
        std::process::exit(1);
    }
}

fn fail(msg: String, out: Option<&str>) -> ! {
    emit(&[format!("error: {msg}")], out);
    std::process::exit(1);
}

/// The lines shared by `mft` and `walk`. `volume` adds the comparison with
/// the volume's used space.
fn report(method: &str, result: &ScanResult, volume: Option<char>, walk_counts: bool) -> Vec<String> {
    let (mem_now, mem_peak) = memory_mb();
    let t = &result.tree;
    let root = t.root();
    let allocated = t.allocated(root);
    let mut lines = vec![
        format!("method: {method}"),
        format!("duration_s: {:.2}", result.duration.as_secs_f64()),
        format!("entries: {}", t.len()),
        format!("files: {}", t.file_count(root)),
        format!("folders: {}", t.dir_count(root)),
        format!("size: {}", t.size(root)),
        format!("allocated: {allocated}"),
        format!("skipped: {}", result.skipped),
    ];
    if walk_counts {
        lines.push(format!("unreadable: {}", result.unreadable));
        lines.push(format!("online_only: {}", result.online_only));
    }
    if let Some(letter) = volume {
        // Without volume information there is nothing to compare with: say so.
        match volume_info(letter).map(|v| v.total.saturating_sub(v.free)).filter(|&used| used > 0) {
            Some(used) => {
                let deviation = (allocated as f64 - used as f64) / used as f64 * 100.0;
                lines.push(format!("volume_used: {used}"));
                lines.push(format!("deviation_percent: {deviation:.2}"));
            }
            None => {
                lines.push("volume_used: n/a".to_string());
                lines.push("deviation_percent: n/a".to_string());
            }
        }
    }
    lines.push(format!("memory_after_scan_mb: {mem_now}"));
    lines.push(format!("memory_peak_mb: {mem_peak}"));
    for c in t.sorted_children(root, SortKey::Allocated, true).into_iter().take(TOP_CHILDREN) {
        lines.push(format!("top: {} {} {} {}", t.allocated(c), t.size(c), t.file_count(c), t.name(c)));
    }
    lines
}

/// Drive letter and folder names below the root of an absolute local path.
fn split_drive_path(folder: &Path) -> Option<(char, Vec<String>)> {
    let abs = std::path::absolute(folder).ok()?;
    let mut components = abs.components();
    let letter = match components.next()? {
        Component::Prefix(p) => match p.kind() {
            Prefix::Disk(l) | Prefix::VerbatimDisk(l) => char::from(l),
            _ => return None,
        },
        _ => return None,
    };
    let mut names = Vec::new();
    for c in components {
        match c {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(n) => names.push(n.to_string_lossy().into_owned()),
            _ => return None,
        }
    }
    Some((letter, names))
}

/// The absolute path of `folder` with 8.3 short names expanded, so that its
/// names match the ones in the MFT tree.
fn resolve_folder(folder: &Path) -> std::io::Result<PathBuf> {
    long_path_name(&std::path::absolute(folder)?)
}

fn compare(folder: &Path, out: Option<&str>) {
    let folder = match resolve_folder(folder) {
        Ok(f) => f,
        Err(e) => fail(format!("cannot resolve {}: {}", folder.display(), ScanError::Io(e)), out),
    };
    let folder = folder.as_path();
    let Some((letter, names)) = split_drive_path(folder) else {
        fail("not a local drive path".to_string(), out);
    };
    let mft = scan_mft(letter, &Progress::default()).unwrap_or_else(|e| fail(e.to_string(), out));
    let mt: &Tree = &mft.tree;
    let mut node = mt.root();
    for name in &names {
        node = mt
            .child_by_name(node, name)
            .unwrap_or_else(|| fail("folder not found in MFT tree".to_string(), out));
    }
    let walk = scan_walk(folder, &Progress::default()).unwrap_or_else(|e| fail(e.to_string(), out));
    let wt = &walk.tree;
    let wr = wt.root();
    let same = mt.file_count(node) == wt.file_count(wr) && mt.size(node) == wt.size(wr);
    let lines = vec![
        format!("mft_files: {}", mt.file_count(node)),
        format!("mft_folders: {}", mt.dir_count(node)),
        format!("mft_size: {}", mt.size(node)),
        format!("mft_allocated: {}", mt.allocated(node)),
        format!("walk_files: {}", wt.file_count(wr)),
        format!("walk_folders: {}", wt.dir_count(wr)),
        format!("walk_size: {}", wt.size(wr)),
        format!("walk_allocated: {}", wt.allocated(wr)),
        format!("equal: {}", if same { "yes" } else { "no" }),
    ];
    emit(&lines, out);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args == ["message-box"] {
        // The same box as a failed start in `src/main.rs`, so that it can be looked at.
        show_error("Platzhirsch", "Platzhirsch could not start: test");
        return;
    }
    let Some((command, out)) = parse_args(&args) else {
        eprintln!("{USAGE}");
        std::process::exit(2);
    };
    let out = out.as_deref();
    match command {
        Command::Mft(letter) => {
            let result = scan_mft(letter, &Progress::default()).unwrap_or_else(|e| fail(e.to_string(), out));
            emit(&report("MFT", &result, Some(letter), false), out);
        }
        Command::Walk(path) => {
            let result = scan_walk(&path, &Progress::default()).unwrap_or_else(|e| fail(e.to_string(), out));
            emit(&report("Walk", &result, drive_root_of(&path), true), out);
        }
        Command::Compare(folder) => compare(&folder, out),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_write_is_reported_in_english() {
        // What writing returns when the folder of the output file does not exist.
        let e = std::io::Error::from_raw_os_error(3);
        assert_eq!(write_failure(r"X:\missing\out.txt", &e), r"error: could not write X:\missing\out.txt: not found (OS error 3)");
        let e = std::io::Error::from_raw_os_error(5);
        assert_eq!(write_failure("out.txt", &e), "error: could not write out.txt: access denied (OS error 5)");
    }

    #[test]
    fn drive_roots_in_every_form() {
        for root in [r"C:\", "C:", "c:/", r"\\?\C:\", r"\\?\c:"] {
            assert_eq!(drive_root_of(Path::new(root)), Some('C'), "{root}");
        }
        for folder in [r"C:\Users", r"\\?\C:\Users", r"\\?\UNC\server\share", r"\\nas\share"] {
            assert_eq!(drive_root_of(Path::new(folder)), None, "{folder}");
        }
    }
}
