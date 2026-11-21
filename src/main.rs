#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;

use platzhirsch::scan::ScanTarget;

/// `--scan X:` scans a drive, `--scan <folder>` a folder; anything else starts idle.
pub fn parse_args(args: &[String]) -> Option<ScanTarget> {
    let [flag, target] = args else { return None };
    if flag != "--scan" || target.is_empty() {
        return None;
    }
    let trimmed = target.strip_suffix(['\\', '/']).unwrap_or(target);
    let mut chars = trimmed.chars();
    if let (Some(letter), Some(':'), None) = (chars.next(), chars.next(), chars.next())
        && letter.is_ascii_alphabetic()
    {
        return Some(ScanTarget::Volume(letter.to_ascii_uppercase()));
    }
    Some(ScanTarget::Folder(PathBuf::from(target)))
}

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    platzhirsch::ui::run(parse_args(&args))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_argument_selects_volume_or_folder() {
        assert!(matches!(parse_args(&["--scan".into(), "C:".into()]), Some(ScanTarget::Volume('C'))));
        assert!(matches!(parse_args(&["--scan".into(), "c:\\".into()]), Some(ScanTarget::Volume('C'))));
        assert!(matches!(parse_args(&["--scan".into(), "C:\\DRIVE".into()]), Some(ScanTarget::Folder(_))));
        assert!(parse_args(&[]).is_none());
    }

    #[test]
    fn unknown_arguments_start_idle() {
        assert!(parse_args(&["--scan".into()]).is_none());
        assert!(parse_args(&["--foo".into(), "C:".into()]).is_none());
        assert!(parse_args(&["--scan".into(), "C:".into(), "extra".into()]).is_none());
    }
}
