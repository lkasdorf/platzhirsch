//! Scanners that fill a [`Tree`] and the types they share.
pub mod mft;
pub mod ntfs;
pub mod walk;

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::time::Duration;

use crate::model::Tree;
use ntfs::NtfsError;

#[derive(Clone)]
pub enum ScanTarget {
    Volume(char),
    Folder(std::path::PathBuf),
}

impl ScanTarget {
    /// A drive root (`C:`, `C:\`, `c:/`, also in the verbatim form `\\?\C:\`)
    /// becomes the whole volume, any other path a folder.
    pub fn from_path(path: std::path::PathBuf) -> ScanTarget {
        if let Some(s) = path.to_str() {
            // `\\?\UNC\server\share` has no drive letter after the prefix and stays a folder.
            let s = s.strip_prefix(r"\\?\").unwrap_or(s);
            let mut chars = s.strip_suffix(['\\', '/']).unwrap_or(s).chars();
            if let (Some(letter), Some(':'), None) = (chars.next(), chars.next(), chars.next())
                && letter.is_ascii_alphabetic()
            {
                return ScanTarget::Volume(letter.to_ascii_uppercase());
            }
        }
        ScanTarget::Folder(path)
    }
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
    /// The volume could not be opened for the MFT scan.
    AccessDenied,
    /// The folder walk was refused access to its root folder.
    FolderAccessDenied,
    /// The walk root is a cloud folder that is not available locally.
    OnlineOnly,
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
            ScanError::FolderAccessDenied => write!(f, "Access denied"),
            ScanError::OnlineOnly => write!(
                f,
                "This folder is online-only; open it once in Explorer to make it available, then scan again."
            ),
            ScanError::NotNtfs => write!(f, "Not an NTFS volume"),
            ScanError::MftIncomplete { found_bytes, expected_bytes } => write!(
                f,
                "MFT incomplete: found {found_bytes} of {expected_bytes} bytes"
            ),
            ScanError::Io(e) => match (e.raw_os_error(), e.get_ref()) {
                // Messages built by this program are English; show them as they are.
                (None, Some(own)) => write!(f, "Read error: {own}"),
                (code, _) => {
                    // Windows' own message text is in the system language, so it is never shown.
                    let description = match e.kind() {
                        std::io::ErrorKind::NotFound => "not found",
                        std::io::ErrorKind::PermissionDenied => "access denied",
                        std::io::ErrorKind::NotADirectory => "not a folder",
                        std::io::ErrorKind::UnexpectedEof => "unexpected end of data",
                        std::io::ErrorKind::InvalidInput => "invalid input",
                        std::io::ErrorKind::InvalidData => "invalid data",
                        _ => "I/O error",
                    };
                    match code {
                        Some(code) => write!(f, "Read error: {description} (OS error {code})"),
                        None => write!(f, "Read error: {description}"),
                    }
                }
            },
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
        let dir = ScanTarget::Folder("C:\\Data".into());
        assert_eq!(choose_method(&vol, true, "NTFS"), MethodChoice::Mft);
        assert_eq!(choose_method(&vol, false, "NTFS"), MethodChoice::AskElevation);
        assert_eq!(choose_method(&vol, true, "exFAT"), MethodChoice::Walk);
        assert_eq!(choose_method(&vol, false, "FAT32"), MethodChoice::Walk);
        assert_eq!(choose_method(&dir, true, "NTFS"), MethodChoice::Walk);
        assert_eq!(choose_method(&ScanTarget::Folder("\\\\nas\\share".into()), true, ""), MethodChoice::Walk);
        assert_eq!(choose_method(&vol, true, "ntfs"), MethodChoice::Mft);
    }

    #[test]
    fn drive_roots_become_volumes() {
        let target = |s: &str| ScanTarget::from_path(s.into());
        for root in ["C:", r"C:\", "c:/", r"d:\"] {
            assert!(matches!(target(root), ScanTarget::Volume('C' | 'D')), "{root}");
        }
        assert!(matches!(target(r"c:\"), ScanTarget::Volume('C')));
        for root in [r"\\?\C:\", r"\\?\c:", r"\\?\D:/"] {
            assert!(matches!(target(root), ScanTarget::Volume('C' | 'D')), "{root}");
        }
        for folder in [
            r"C:\Data",
            r"C:\\",
            r"\\nas\share",
            r"\\?\C:\Data",
            r"\\?\UNC\server\share",
            r"\\?\\C:\",
            "1:",
            "src",
        ] {
            assert!(matches!(target(folder), ScanTarget::Folder(ref p) if p.as_os_str() == folder), "{folder}");
        }
    }

    #[test]
    fn access_messages() {
        assert_eq!(ScanError::AccessDenied.to_string(), "Access denied (administrator rights required)");
        assert_eq!(ScanError::FolderAccessDenied.to_string(), "Access denied");
        assert_eq!(
            ScanError::OnlineOnly.to_string(),
            "This folder is online-only; open it once in Explorer to make it available, then scan again."
        );
    }

    #[test]
    fn io_messages_are_english_whatever_the_system_language() {
        use std::io::{Error, ErrorKind};
        let text = |e: Error| ScanError::Io(e).to_string();
        assert_eq!(text(Error::from_raw_os_error(2)), "Read error: not found (OS error 2)");
        assert_eq!(text(Error::from_raw_os_error(5)), "Read error: access denied (OS error 5)");
        assert_eq!(text(Error::from_raw_os_error(267)), "Read error: not a folder (OS error 267)");
        assert_eq!(text(Error::from_raw_os_error(1117)), "Read error: I/O error (OS error 1117)");
        assert_eq!(text(Error::from(ErrorKind::UnexpectedEof)), "Read error: unexpected end of data");
        assert_eq!(text(Error::from(ErrorKind::InvalidInput)), "Read error: invalid input");
        assert_eq!(text(Error::from(ErrorKind::NotADirectory)), "Read error: not a folder");
        assert_eq!(text(Error::from(ErrorKind::InvalidData)), "Read error: invalid data");
        assert_eq!(text(Error::from(ErrorKind::TimedOut)), "Read error: I/O error");
        assert_eq!(
            text(Error::new(ErrorKind::InvalidData, "Malformed folder listing: entry truncated")),
            "Read error: Malformed folder listing: entry truncated"
        );
        assert_eq!(text(Error::other("Folder listing aborted")), "Read error: Folder listing aborted");
    }
}
