//! Scanners that fill a [`Tree`] and the types they share.
pub mod mft;
pub mod ntfs;

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
            ScanError::Cancelled => write!(f, "Scan abgebrochen"),
            ScanError::AccessDenied => write!(f, "Zugriff verweigert (Administratorrechte nötig)"),
            ScanError::NotNtfs => write!(f, "Kein NTFS-Laufwerk"),
            ScanError::MftIncomplete { found_bytes, expected_bytes } => write!(
                f,
                "MFT unvollständig: {found_bytes} von {expected_bytes} Bytes gefunden"
            ),
            ScanError::Io(e) => write!(f, "Lesefehler: {e}"),
            ScanError::Ntfs(e) => {
                let detail = match e {
                    NtfsError::BadSignature => "ungültige Signatur",
                    NtfsError::BadFixup => "Prüfsumme (Fixup) falsch",
                    NtfsError::Truncated => "Daten abgeschnitten",
                    NtfsError::Malformed(what) => what,
                };
                write!(f, "NTFS-Daten fehlerhaft: {detail}")
            }
        }
    }
}
