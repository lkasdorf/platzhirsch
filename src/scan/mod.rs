//! Scanners that fill a [`Tree`] and the types they share.
pub mod mft;
pub mod ntfs;
pub mod walk;

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::time::Duration;

use crate::model::Tree;
use ntfs::NtfsError;

pub enum ScanTarget {
    Volume(char),
    Folder(std::path::PathBuf),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ScanMethod {
    Mft,
    Walk,
}

#[derive(Debug, PartialEq, Eq)]
pub enum MethodChoice {
    Mft,
    Walk,
    /// The MFT would work, but only with admin rights.
    AskElevation,
}

/// Picks the scan method: the MFT for whole NTFS volumes, the walk otherwise.
pub fn choose_method(target: &ScanTarget, elevated: bool, fs_name: &str) -> MethodChoice {
    match target {
        ScanTarget::Folder(_) => MethodChoice::Walk,
        ScanTarget::Volume(_) if !fs_name.eq_ignore_ascii_case("NTFS") => MethodChoice::Walk,
        ScanTarget::Volume(_) if elevated => MethodChoice::Mft,
        ScanTarget::Volume(_) => MethodChoice::AskElevation,
    }
}

/// Shared between the scanner and the GUI. `total == 0` means unknown.
#[derive(Default)]
pub struct Progress {
    pub done: AtomicU64,
    pub total: AtomicU64,
    pub cancel: AtomicBool,
}

pub struct ScanResult {
    pub tree: Tree,
    pub method: ScanMethod,
    pub duration: Duration,
    /// Entries that could not be read or attributed; never counted as 0 bytes.
    pub skipped: u64,
    pub unreadable: u64,
    pub online_only: u64,
}

#[derive(Debug)]
pub enum ScanError {
    Cancelled,
    AccessDenied,
    NotNtfs,
    MftIncomplete { found_bytes: u64, expected_bytes: u64 },
    Io(std::io::Error),
    Ntfs(NtfsError),
}

impl fmt::Display for ScanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ScanError::Cancelled => write!(f, "Scan cancelled"),
            ScanError::AccessDenied => write!(f, "Access denied (administrator rights required)"),
            ScanError::NotNtfs => write!(f, "Not an NTFS volume"),
            ScanError::MftIncomplete { found_bytes, expected_bytes } => write!(
                f,
                "MFT incomplete: found {found_bytes} of {expected_bytes} bytes"
            ),
            ScanError::Io(e) => write!(f, "Read error: {e}"),
            ScanError::Ntfs(e) => {
                let detail = match e {
                    NtfsError::BadSignature => "invalid signature",
                    NtfsError::BadFixup => "checksum (fixup) mismatch",
                    NtfsError::Truncated => "data truncated",
                    NtfsError::Malformed(what) => what,
                };
                write!(f, "NTFS data is corrupt: {detail}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_choice() {
        let vol = ScanTarget::Volume('C');
        let dir = ScanTarget::Folder("C:\\DRIVE".into());
        assert_eq!(choose_method(&vol, true, "NTFS"), MethodChoice::Mft);
        assert_eq!(choose_method(&vol, false, "NTFS"), MethodChoice::AskElevation);
        assert_eq!(choose_method(&vol, true, "exFAT"), MethodChoice::Walk);
        assert_eq!(choose_method(&vol, false, "FAT32"), MethodChoice::Walk);
        assert_eq!(choose_method(&dir, true, "NTFS"), MethodChoice::Walk);
        assert_eq!(choose_method(&ScanTarget::Folder("\\\\nas\\share".into()), true, ""), MethodChoice::Walk);
        assert_eq!(choose_method(&vol, true, "ntfs"), MethodChoice::Mft);
    }
}
