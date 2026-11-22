#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;

use platzhirsch::scan::ScanTarget;

/// `--scan X:` scans a drive, `--scan <folder>` a folder; anything else starts idle.
pub fn parse_args(args: &[String]) -> Option<ScanTarget> {
    let [flag, target] = args else { return None };
    if flag != "--scan" || target.is_empty() {
        return None;
    }
    Some(ScanTarget::from_path(PathBuf::from(target)))
}

fn main() {
    let args: Vec<String> = std::env::args_os().skip(1).map(|a| a.to_string_lossy().into_owned()).collect();
    if let Err(e) = platzhirsch::ui::run(parse_args(&args)) {
        // The release build has no console, so the error would go unseen.
        platzhirsch::win::show_error("Platzhirsch", &format!("Platzhirsch could not start: {e}"));
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_argument_selects_volume_or_folder() {
        assert!(matches!(parse_args(&["--scan".into(), "C:".into()]), Some(ScanTarget::Volume('C'))));
        assert!(matches!(parse_args(&["--scan".into(), "c:\\".into()]), Some(ScanTarget::Volume('C'))));
        assert!(matches!(parse_args(&["--scan".into(), "c:/".into()]), Some(ScanTarget::Volume('C'))));
        assert!(matches!(parse_args(&["--scan".into(), "C:\\Data".into()]), Some(ScanTarget::Folder(_))));
        assert!(parse_args(&[]).is_none());
    }

    #[test]
    fn unknown_arguments_start_idle() {
        assert!(parse_args(&["--scan".into()]).is_none());
        assert!(parse_args(&["--foo".into(), "C:".into()]).is_none());
        assert!(parse_args(&["--scan".into(), "C:".into(), "extra".into()]).is_none());
    }
}
