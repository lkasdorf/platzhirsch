//! Pure parsers for NTFS on-disk structures.
pub mod boot;
pub mod record;
pub mod runs;
#[cfg(test)]
pub mod testkit;

/// Errors from parsing untrusted NTFS bytes.
#[derive(Debug, PartialEq)]
pub enum NtfsError {
    BadSignature,
    BadFixup,
    Truncated,
    Malformed(&'static str),
}
