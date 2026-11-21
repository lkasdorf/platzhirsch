//! Pure parsers for NTFS on-disk structures.
pub mod boot;
pub mod runs;

/// Errors from parsing untrusted NTFS bytes.
#[derive(Debug, PartialEq)]
pub enum NtfsError {
    BadSignature,
    BadFixup,
    Truncated,
    Malformed(&'static str),
}
