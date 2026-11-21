//! Diagnostic tool: `bench mft <X:> [--out <file>]` measures the MFT scan.
use platzhirsch::model::SortKey;
use platzhirsch::scan::Progress;
use platzhirsch::scan::mft::scan_mft;
use platzhirsch::win::{memory_mb, volume_info};

const USAGE: &str = "Aufruf: bench mft <X:> [--out <Datei>]";
const TOP_CHILDREN: usize = 15;

fn parse_args(args: &[String]) -> Option<(char, Option<String>)> {
    let [cmd, drive, rest @ ..] = args else { return None };
    if cmd != "mft" {
        return None;
    }
    let mut chars = drive.chars();
    let letter = chars.next().filter(char::is_ascii_alphabetic)?;
    if chars.as_str() != ":" {
        return None;
    }
    let out = match rest {
        [] => None,
        [flag, path] if flag == "--out" => Some(path.clone()),
        _ => return None,
    };
    Some((letter, out))
}

/// Prints `lines` and, with `--out`, writes them to that file.
fn emit(lines: &[String], out: Option<&str>) {
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    print!("{text}");
    if let Some(path) = out
        && let Err(e) = std::fs::write(path, &text)
    {
        eprintln!("{path} konnte nicht geschrieben werden: {e}");
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((letter, out)) = parse_args(&args) else {
        eprintln!("{USAGE}");
        std::process::exit(2);
    };

    let result = match scan_mft(letter, &Progress::default()) {
        Ok(r) => r,
        Err(e) => {
            emit(&[format!("fehler: {e}")], out.as_deref());
            std::process::exit(1);
        }
    };
    let (mem_now, mem_peak) = memory_mb();

    let t = &result.tree;
    let root = t.root();
    let allocated = t.allocated(root);
    let volume_used = volume_info(letter).map_or(0, |v| v.total.saturating_sub(v.free));
    let deviation = if volume_used == 0 {
        0.0
    } else {
        (allocated as f64 - volume_used as f64) / volume_used as f64 * 100.0
    };

    let mut lines = vec![
        "methode: mft".to_string(),
        format!("dauer_s: {:.2}", result.duration.as_secs_f64()),
        format!("eintraege: {}", t.len()),
        format!("dateien: {}", t.file_count(root)),
        format!("ordner: {}", t.dir_count(root)),
        format!("groesse: {}", t.size(root)),
        format!("belegt: {allocated}"),
        format!("uebersprungen: {}", result.skipped),
        format!("volume_belegt: {volume_used}"),
        format!("abweichung_prozent: {deviation:.2}"),
        format!("speicher_nach_scan_mb: {mem_now}"),
        format!("speicher_spitze_mb: {mem_peak}"),
    ];
    for c in t.sorted_children(root, SortKey::Allocated, true).into_iter().take(TOP_CHILDREN) {
        lines.push(format!("top: {} {} {} {}", t.allocated(c), t.size(c), t.file_count(c), t.name(c)));
    }
    emit(&lines, out.as_deref());
}
