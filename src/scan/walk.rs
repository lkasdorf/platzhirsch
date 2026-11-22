//! Fallback scanner: walks directories in parallel with ordinary Win32 calls.
//!
//! Used without admin rights, on non-NTFS volumes, on network paths and for
//! single folders. Directories are opened for listing only; file contents are
//! never opened. To resolve where the scan target points if it is a link (or
//! lies behind one), one handle with attribute access only is opened on it
//! before the walk starts. The walk uses an explicit work queue, never recursion: worker
//! threads list directories, the calling thread alone fills the tree and
//! hands out new directories as jobs.
use std::io;
use std::mem::offset_of;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex, mpsc};
use std::time::Instant;

use windows::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows::Wdk::Storage::FileSystem::{
    FILE_DIRECTORY_FILE, FILE_OPEN_FOR_BACKUP_INTENT, FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT,
    NtOpenFile,
};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_FILE_NOT_FOUND, ERROR_NO_MORE_FILES, HANDLE, OBJ_CASE_INSENSITIVE, RtlNtStatusToDosError, UNICODE_STRING,
};
use windows::Win32::Storage::FileSystem::{
    FILE_ACCESS_RIGHTS, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_RECALL_ON_OPEN, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_ID_BOTH_DIR_INFO, FILE_LIST_DIRECTORY, FILE_NAME_NORMALIZED, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, FIND_FIRST_EX_FLAGS, FileIdBothDirectoryInfo, FindClose, FindExInfoBasic,
    FindExSearchNameMatch, FindFirstFileExW, GETFINALPATHNAMEBYHANDLE_FLAGS, GetFileAttributesW,
    GetFileInformationByHandleEx, GetFinalPathNameByHandleW, INVALID_FILE_ATTRIBUTES, SYNCHRONIZE, VOLUME_NAME_DOS,
    WIN32_FIND_DATAW,
};
use windows::Win32::System::IO::IO_STATUS_BLOCK;
use windows::core::{PCWSTR, PWSTR};

use super::{Progress, ScanError, ScanMethod, ScanResult};
use crate::model::{F_DIR, F_LINK, F_ONLINE_ONLY, F_UNREADABLE, NO_PARENT, NodeId, TreeBuilder};

/// Reparse tags with this bit (junctions, symbolic links) point elsewhere.
const REPARSE_NAME_SURROGATE: u32 = 0x2000_0000;
/// Bytes per listing call; a multiple of 8 so the buffer is 8-byte aligned.
const LIST_BUF_BYTES: usize = 64 * 1024;
const MAX_THREADS: usize = 16;
const MIN_THREADS: usize = 2;
/// Workers when the number of processors cannot be determined.
const FALLBACK_THREADS: usize = 4;

// Field offsets of FILE_ID_BOTH_DIR_INFO, taken from the crate's layout
// (MS-FSCC 2.4.17 FileIdBothDirectoryInformation).
const OFF_NEXT: usize = offset_of!(FILE_ID_BOTH_DIR_INFO, NextEntryOffset);
const OFF_MTIME: usize = offset_of!(FILE_ID_BOTH_DIR_INFO, LastWriteTime);
const OFF_EOF: usize = offset_of!(FILE_ID_BOTH_DIR_INFO, EndOfFile);
const OFF_ALLOC: usize = offset_of!(FILE_ID_BOTH_DIR_INFO, AllocationSize);
const OFF_ATTR: usize = offset_of!(FILE_ID_BOTH_DIR_INFO, FileAttributes);
const OFF_NAME_LEN: usize = offset_of!(FILE_ID_BOTH_DIR_INFO, FileNameLength);
/// Holds the reparse tag when the reparse-point attribute is set (MS-FSCC 2.4.17).
const OFF_EA_SIZE: usize = offset_of!(FILE_ID_BOTH_DIR_INFO, EaSize);
const OFF_NAME: usize = offset_of!(FILE_ID_BOTH_DIR_INFO, FileName);

#[derive(Debug, PartialEq, Eq)]
pub enum DirAction {
    Descend,
    Link,
    OnlineOnly,
}

/// What to do with a directory entry: links are never followed, and
/// directories that are not fully present locally are never entered.
pub fn classify_dir(attributes: u32, reparse_tag: u32) -> DirAction {
    if attributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 && reparse_tag & REPARSE_NAME_SURROGATE != 0 {
        DirAction::Link
    } else if attributes & FILE_ATTRIBUTE_RECALL_ON_OPEN.0 != 0 {
        DirAction::OnlineOnly
    } else {
        DirAction::Descend
    }
}

/// One record of a directory listing, without `.` and `..`.
#[derive(Debug)]
struct Entry {
    name: String,
    /// The exact UTF-16 name, kept only for directories that will be entered.
    wide_name: Option<Vec<u16>>,
    size: u64,
    allocated: u64,
    mtime: i64,
    attributes: u32,
    reparse_tag: u32,
}

fn u32_at(rec: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(rec[off..off + 4].try_into().expect("4 bytes"))
}

fn i64_at(rec: &[u8], off: usize) -> i64 {
    i64::from_le_bytes(rec[off..off + 8].try_into().expect("8 bytes"))
}

fn bad_listing(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("Malformed folder listing: {what}"))
}

/// Decodes a buffer of FILE_ID_BOTH_DIR_INFO records chained by
/// `NextEntryOffset`. Every record and name is checked against `buf`.
fn parse_records(buf: &[u8], out: &mut Vec<Entry>) -> io::Result<()> {
    let mut off = 0usize;
    loop {
        let rec = buf.get(off..).filter(|r| r.len() >= OFF_NAME).ok_or_else(|| bad_listing("entry truncated"))?;
        let next = u32_at(rec, OFF_NEXT) as usize;
        let name_len = u32_at(rec, OFF_NAME_LEN) as usize;
        let name_end = OFF_NAME.checked_add(name_len).filter(|&e| e <= rec.len() && name_len.is_multiple_of(2));
        let name_end = name_end.ok_or_else(|| bad_listing("name truncated"))?;
        if name_len == 0 {
            return Err(bad_listing("empty name"));
        }
        if next != 0 && next < name_end {
            return Err(bad_listing("entries overlap"));
        }
        let (units, _) = rec[OFF_NAME..name_end].as_chunks::<2>();
        let wide: Vec<u16> = units.iter().map(|&c| u16::from_le_bytes(c)).collect();
        let dot = u16::from(b'.');
        if wide != [dot] && wide != [dot, dot] {
            let attributes = u32_at(rec, OFF_ATTR);
            let reparse_tag =
                if attributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 { u32_at(rec, OFF_EA_SIZE) } else { 0 };
            let is_dir = attributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0;
            let descend = is_dir && classify_dir(attributes, reparse_tag) == DirAction::Descend;
            out.push(Entry {
                name: String::from_utf16_lossy(&wide),
                wide_name: descend.then_some(wide),
                size: i64_at(rec, OFF_EOF).max(0) as u64,
                allocated: i64_at(rec, OFF_ALLOC).max(0) as u64,
                mtime: i64_at(rec, OFF_MTIME),
                attributes,
                reparse_tag,
            });
        }
        if next == 0 {
            return Ok(());
        }
        off += next;
    }
}

/// Converts a `windows` error to an `io::Error` with the plain Win32 code.
fn to_io(e: windows::core::Error) -> io::Error {
    let code = e.code().0 as u32;
    if code & 0xFFFF_0000 == 0x8007_0000 {
        io::Error::from_raw_os_error((code & 0xFFFF) as i32)
    } else {
        io::Error::from_raw_os_error(code as i32)
    }
}

/// A handle opened by `nt_open` (for listing or attribute access only); closed when dropped.
struct DirHandle(HANDLE);

impl Drop for DirHandle {
    fn drop(&mut self) {
        // SAFETY: the handle was opened by `nt_open`, is valid and is closed only here.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// The NT form (`\??\…`) of a `\\?\` or `\\.\` path.
fn nt_path(path: &[u16]) -> io::Result<Vec<u16>> {
    let wide = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
    if !path.starts_with(&wide(r"\\?\")) && !path.starts_with(&wide(r"\\.\")) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "Path without \\\\?\\ prefix"));
    }
    let mut nt = wide(r"\??\");
    nt.extend_from_slice(&path[4..]);
    Ok(nt)
}

/// Opens a directory with list access only. `path` has no trailing NUL.
/// Without `follow`, a reparse point is opened itself instead of its target.
///
/// `FILE_DIRECTORY_FILE` makes the file system refuse the open with
/// `STATUS_NOT_A_DIRECTORY` if the object is not a directory, so no handle
/// to a file is ever created, even if a directory was replaced by a file
/// since it was listed. (`FILE_LIST_DIRECTORY` has the same bit as read-data
/// access on files, which is why this matters.)
fn open_dir(path: &[u16], follow: bool) -> io::Result<DirHandle> {
    let mut options = FILE_DIRECTORY_FILE.0 | FILE_SYNCHRONOUS_IO_NONALERT.0 | FILE_OPEN_FOR_BACKUP_INTENT.0;
    if !follow {
        options |= FILE_OPEN_REPARSE_POINT.0;
    }
    nt_open(path, FILE_LIST_DIRECTORY | SYNCHRONIZE, options)
}

/// Opens `path` (verbatim, no NUL) with `NtOpenFile`, sharing everything.
fn nt_open(path: &[u16], access: FILE_ACCESS_RIGHTS, options: u32) -> io::Result<DirHandle> {
    let nt = nt_path(path)?;
    let bytes = u16::try_from(nt.len() * 2)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Path too long"))?;
    // The buffer is only read; the binding's type is `PWSTR` nonetheless.
    let name = UNICODE_STRING { Length: bytes, MaximumLength: bytes, Buffer: PWSTR(nt.as_ptr().cast_mut()) };
    let attributes = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        ObjectName: &name,
        Attributes: OBJ_CASE_INSENSITIVE,
        ..Default::default()
    };
    let mut handle = HANDLE::default();
    let mut iosb = IO_STATUS_BLOCK::default();
    // SAFETY: all pointers point to live locals; `name` and `nt` outlive the call.
    let status = unsafe {
        NtOpenFile(
            &mut handle,
            access.0,
            &attributes,
            &mut iosb,
            (FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE).0,
            options,
        )
    };
    if status.0 < 0 {
        // SAFETY: plain value conversion without pointers.
        let code = unsafe { RtlNtStatusToDosError(status) };
        return Err(io::Error::from_raw_os_error(code as i32));
    }
    Ok(DirHandle(handle))
}

/// Fails with `NotADirectory` if `path` (verbatim, no NUL) is not a directory.
/// With `allow_link`, a reparse point also passes: a symbolic link reports its
/// own attributes, and whether its target is a folder is checked after it was
/// resolved. Reads the attributes only; the object is not opened for any data access.
fn ensure_directory(path: &[u16], allow_link: bool) -> io::Result<()> {
    let mut z = Vec::with_capacity(path.len() + 1);
    z.extend_from_slice(path);
    z.push(0);
    // SAFETY: `z` is NUL-terminated and outlives the call.
    let attributes = unsafe { GetFileAttributesW(PCWSTR(z.as_ptr())) };
    if attributes == INVALID_FILE_ATTRIBUTES {
        return Err(io::Error::last_os_error());
    }
    let link = allow_link && attributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0;
    if attributes & FILE_ATTRIBUTE_DIRECTORY.0 == 0 && !link {
        return Err(io::Error::new(io::ErrorKind::NotADirectory, "Not a folder"));
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
enum ListEnd {
    /// The listing is complete.
    Complete,
    /// The listing broke off.
    Failed,
}

/// What a failed listing call on an opened directory handle means.
/// `os_error` is its Win32 code, `returned_any` whether an earlier call on
/// the same handle returned records.
///
/// A directory without `.` and `..` (a drive root, an empty folder on
/// FAT/exFAT) answers the first call with `ERROR_FILE_NOT_FOUND` when it is
/// empty. Errors from opening the directory never come here.
fn listing_end(os_error: Option<i32>, returned_any: bool) -> ListEnd {
    match os_error {
        Some(code) if code == ERROR_NO_MORE_FILES.0 as i32 => ListEnd::Complete,
        Some(code) if code == ERROR_FILE_NOT_FOUND.0 as i32 && !returned_any => ListEnd::Complete,
        _ => ListEnd::Failed,
    }
}

enum Listing {
    Complete(Vec<Entry>),
    /// The directory could not be opened or listed to the end.
    Failed(Vec<Entry>, io::Error),
    Cancelled,
}

fn list_dir(path: &[u16], follow: bool, cancel: &AtomicBool) -> Listing {
    let dir = match open_dir(path, follow) {
        Ok(d) => d,
        Err(e) => return Listing::Failed(Vec::new(), e),
    };
    let mut buf = vec![0u64; LIST_BUF_BYTES / 8];
    let mut entries = Vec::new();
    let mut returned_any = false;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Listing::Cancelled;
        }
        buf.fill(0);
        // SAFETY: `buf` is a live, 8-byte aligned buffer of exactly LIST_BUF_BYTES bytes.
        let r = unsafe {
            GetFileInformationByHandleEx(
                dir.0,
                FileIdBothDirectoryInfo,
                buf.as_mut_ptr().cast(),
                LIST_BUF_BYTES as u32,
            )
        };
        if let Err(e) = r {
            let e = to_io(e);
            return match listing_end(e.raw_os_error(), returned_any) {
                ListEnd::Complete => Listing::Complete(entries),
                ListEnd::Failed => Listing::Failed(entries, e),
            };
        }
        returned_any = true;
        // SAFETY: `u64` has no padding, so its memory is valid as bytes; the view ends before `buf` is used again.
        let bytes = unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), LIST_BUF_BYTES) };
        if let Err(e) = parse_records(bytes, &mut entries) {
            return Listing::Failed(entries, e);
        }
    }
}

/// True for a device path (`\\.\…`, also written with forward slashes).
fn is_device_path(root: &Path) -> bool {
    let s: Vec<u16> = root.as_os_str().encode_wide().take(4).collect();
    let slash = |c: u16| c == u16::from(b'\\') || c == u16::from(b'/');
    s.len() == 4 && slash(s[0]) && slash(s[1]) && s[2] == u16::from(b'.') && slash(s[3])
}

/// `root` made absolute. A bare drive letter with a colon (`X:`) is the
/// drive root `X:\`, not the current folder on that drive.
fn absolute_root(root: &Path) -> io::Result<PathBuf> {
    let s: Vec<u16> = root.as_os_str().encode_wide().collect();
    if let [letter, colon] = s[..]
        && colon == u16::from(b':')
        && u8::try_from(letter).is_ok_and(|l| l.is_ascii_alphabetic())
    {
        let mut drive = root.as_os_str().to_owned();
        drive.push("\\");
        return Ok(PathBuf::from(drive));
    }
    std::path::absolute(root)
}

/// `root` as an absolute path with `\\?\` (or `\\?\UNC\`) prefix, so long
/// paths work. `\\?\` and `\\.\` paths are kept as they are.
fn verbatim_path(root: &Path) -> io::Result<Vec<u16>> {
    let abs = absolute_root(root)?;
    let s: Vec<u16> = abs.as_os_str().encode_wide().collect();
    let bs = u16::from(b'\\');
    let prefix = |p: &str| s.starts_with(&p.encode_utf16().collect::<Vec<_>>());
    if prefix(r"\\?\") || prefix(r"\\.\") {
        Ok(s)
    } else if s.len() > 2 && s[0] == bs && s[1] == bs {
        Ok(r"\\?\UNC\".encode_utf16().chain(s[2..].iter().copied()).collect())
    } else {
        Ok(r"\\?\".encode_utf16().chain(s).collect())
    }
}

fn join(base: &[u16], name: &[u16]) -> Vec<u16> {
    let mut p = Vec::with_capacity(base.len() + 1 + name.len());
    p.extend_from_slice(base);
    if p.last() != Some(&u16::from(b'\\')) {
        p.push(u16::from(b'\\'));
    }
    p.extend_from_slice(name);
    p
}

struct Job {
    dir: NodeId,
    path: Vec<u16>,
    /// Only the root is opened through reparse points (the user chose it).
    follow: bool,
}

struct Done {
    job: Job,
    listing: Listing,
}

/// Directories waiting to be listed. LIFO keeps the queue short.
struct Queue {
    state: Mutex<(Vec<Job>, bool)>,
    ready: Condvar,
}

impl Queue {
    fn pop(&self) -> Option<Job> {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if s.1 {
                return None;
            }
            if let Some(job) = s.0.pop() {
                return Some(job);
            }
            s = self.ready.wait(s).unwrap_or_else(|e| e.into_inner());
        }
    }

    fn push(&self, jobs: Vec<Job>) {
        if jobs.is_empty() {
            return;
        }
        self.state.lock().unwrap_or_else(|e| e.into_inner()).0.extend(jobs);
        self.ready.notify_all();
    }

    fn close(&self) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).1 = true;
        self.ready.notify_all();
    }
}

/// Closes the queue if a worker panics, so the others stop and the scan ends.
struct CloseOnPanic<'a>(&'a Queue);

impl Drop for CloseOnPanic<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.close();
        }
    }
}

fn worker(queue: &Queue, tx: mpsc::Sender<Done>, cancel: &AtomicBool) {
    let _guard = CloseOnPanic(queue);
    while let Some(job) = queue.pop() {
        let listing = list_dir(&job.path, job.follow, cancel);
        if tx.send(Done { job, listing }).is_err() {
            return;
        }
    }
}

fn scan_error(e: io::Error) -> ScanError {
    if e.kind() == io::ErrorKind::PermissionDenied { ScanError::FolderAccessDenied } else { ScanError::Io(e) }
}

/// The root's name: its absolute path without a trailing backslash, so a
/// drive root is `C:`.
fn root_name(root: &Path) -> io::Result<String> {
    let abs = absolute_root(root)?;
    let full = abs.to_string_lossy();
    let trimmed = full.trim_end_matches(['\\', '/']);
    Ok(if trimmed.is_empty() { full.into_owned() } else { trimmed.to_string() })
}

#[derive(Debug, PartialEq, Eq)]
enum RootAccess {
    Proceed,
    /// The folder is online-only: listing it would make the cloud provider
    /// download it and write placeholders.
    Refuse,
}

/// Whether a walk may list a root, from the entries of the levels of its path
/// from the top down (the attributes from each parent's listing, `None` if a
/// level could not be queried). An online-only level refuses. A level that
/// cannot be queried ends the check, and no further level is taken from
/// `levels`, so a lazy iterator queries a parent only after it passed.
fn path_access(levels: impl IntoIterator<Item = Option<u32>>) -> RootAccess {
    for attributes in levels {
        let Some(attributes) = attributes else { return RootAccess::Proceed };
        if attributes & FILE_ATTRIBUTE_RECALL_ON_OPEN.0 != 0 {
            return RootAccess::Refuse;
        }
    }
    RootAccess::Proceed
}

/// The paths of every level of `path` (verbatim, no NUL) below its drive or
/// share root, from the top down: `\\?\C:\a\b` gives `\\?\C:\a` and
/// `\\?\C:\a\b`; `\\?\UNC\nas\share\d` gives `\\?\UNC\nas\share\d`. Empty for
/// a drive or share root and for a path without the `\\?\` prefix.
fn levels(path: &[u16]) -> Vec<Vec<u16>> {
    let bs = u16::from(b'\\');
    let prefix: Vec<u16> = r"\\?\".encode_utf16().collect();
    let Some(rest) = path.strip_prefix(&prefix[..]) else { return Vec::new() };
    let segments: Vec<&[u16]> = rest.split(|&c| c == bs).collect();
    let unc: Vec<u16> = "UNC".encode_utf16().collect();
    let is_unc = segments.first().is_some_and(|s| s.len() == 3 && s.iter().zip(&unc).all(|(&a, &b)| a & !0x20 == b));
    // `UNC`, server and share, or the drive (`C:`) or volume name.
    let root_segments = if is_unc { 3 } else { 1 };
    if segments.len() <= root_segments {
        return Vec::new();
    }
    let mut current = prefix;
    current.extend(segments[..root_segments].join(&bs));
    let mut out = Vec::new();
    for segment in segments[root_segments..].iter().filter(|s| !s.is_empty()) {
        current.push(bs);
        current.extend_from_slice(segment);
        out.push(current.clone());
    }
    out
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum LevelCheck {
    Refuse,
    Proceed,
    /// The check ended at a link (only with `stop_at_link`): what lies behind
    /// it has not been checked yet.
    ReachedLink,
}

/// Checks the levels of `path` from the top down for online-only folders.
/// With `stop_at_link`, the check ends after the first link (a junction,
/// symbolic link or mount point), because the levels behind it lie elsewhere.
fn check_levels(path: &[u16], stop_at_link: bool) -> LevelCheck {
    let mut past_link = false;
    let levels = levels(path).into_iter().map_while(|level| {
        if past_link {
            return None;
        }
        let entry = entry_attributes(&level);
        past_link = stop_at_link && entry.is_some_and(|(a, tag)| classify_dir(a, tag) == DirAction::Link);
        Some(entry.map(|(attributes, _)| attributes))
    });
    match path_access(levels) {
        RootAccess::Refuse => LevelCheck::Refuse,
        RootAccess::Proceed if past_link => LevelCheck::ReachedLink,
        RootAccess::Proceed => LevelCheck::Proceed,
    }
}

/// The path that is listed and whether it still needs a check of every
/// level, from the outcome of the first check of the path as given and the
/// result of resolving it. Whenever the first check ended at a link, the
/// listed path (the resolved one, or the given one if resolution failed) is
/// checked in full, also when resolution returned the same path.
fn plan_root(given: Vec<u16>, first: LevelCheck, resolved: Option<Vec<u16>>) -> (Vec<u16>, bool) {
    let listed = resolved.unwrap_or(given);
    (listed, first == LevelCheck::ReachedLink)
}

/// The final path of `path` (verbatim, no NUL) with links resolved, in the
/// form `verbatim_path` gives: `\\?\C:\x` or `\\?\UNC\nas\share\x`, a drive
/// or share root with one trailing backslash. Opens the object for attribute
/// access only, following links; `None` if that or the query fails.
fn final_path(path: &[u16]) -> Option<Vec<u16>> {
    // No data access is requested, so no file content is read and no
    // placeholder is hydrated. Without FILE_DIRECTORY_FILE, a file-type
    // symbolic link that points to a folder can be opened as well.
    let handle = nt_open(
        path,
        FILE_READ_ATTRIBUTES | SYNCHRONIZE,
        FILE_SYNCHRONOUS_IO_NONALERT.0 | FILE_OPEN_FOR_BACKUP_INTENT.0,
    )
    .ok()?;
    let flags = GETFINALPATHNAMEBYHANDLE_FLAGS(FILE_NAME_NORMALIZED.0 | VOLUME_NAME_DOS.0);
    let mut buf = vec![0u16; 1024];
    for _ in 0..2 {
        // SAFETY: `handle` is open and `buf` is a live buffer whose length is passed with it.
        let n = unsafe { GetFinalPathNameByHandleW(handle.0, &mut buf, flags) } as usize;
        if n == 0 {
            return None;
        }
        if n < buf.len() {
            buf.truncate(n);
            return normalize_final(buf);
        }
        // Too small: `n` is the size needed, including the NUL.
        buf = vec![0u16; n];
    }
    None
}

/// `path` from `GetFinalPathNameByHandleW` in the form the walk uses, or
/// `None` if it is not a `\\?\` path.
fn normalize_final(mut path: Vec<u16>) -> Option<Vec<u16>> {
    let bs = u16::from(b'\\');
    let prefix: Vec<u16> = r"\\?\".encode_utf16().collect();
    if !path.starts_with(&prefix) {
        return None;
    }
    while path.len() > prefix.len() && path.last() == Some(&bs) {
        path.pop();
    }
    if path.len() == prefix.len() {
        return None;
    }
    if levels(&path).is_empty() {
        // `\\?\C:` without the backslash would name the volume, not its root folder.
        path.push(bs);
    }
    Some(path)
}

/// Picks the number of worker threads from the number of processors.
fn worker_threads(processors: Option<usize>) -> usize {
    processors.map_or(FALLBACK_THREADS, |n| n.clamp(MIN_THREADS, MAX_THREADS))
}

/// `path` (verbatim, no NUL) without trailing backslashes, or `None` for a
/// drive or share root, which has no entry in a parent directory.
fn entry_path(path: &[u16]) -> Option<Vec<u16>> {
    let bs = u16::from(b'\\');
    let end = path.iter().rposition(|&c| c != bs)? + 1;
    let trimmed = &path[..end];
    let has_parent = Path::new(&std::ffi::OsString::from_wide(trimmed)).parent().is_some();
    has_parent.then(|| trimmed.to_vec())
}

/// The attributes and reparse tag of `path`'s own entry as its parent's
/// listing reports them. Unlike `GetFileAttributesW`, a directory listing
/// carries `FILE_ATTRIBUTE_RECALL_ON_OPEN`. Lists the parent filtered to this
/// one name; `path` itself is neither opened nor listed. `None` for a drive or
/// share root, or if the parent cannot be listed.
fn entry_attributes(path: &[u16]) -> Option<(u32, u32)> {
    let mut z = entry_path(path)?;
    z.push(0);
    let mut data = WIN32_FIND_DATAW::default();
    // SAFETY: `z` is NUL-terminated and `data` is a live WIN32_FIND_DATAW,
    // the type FindExInfoBasic fills; both outlive the call.
    let handle = unsafe {
        FindFirstFileExW(
            PCWSTR(z.as_ptr()),
            FindExInfoBasic,
            (&raw mut data).cast(),
            FindExSearchNameMatch,
            None,
            FIND_FIRST_EX_FLAGS(0),
        )
    }
    .ok()?;
    // SAFETY: `handle` is a valid search handle from FindFirstFileExW, closed only here.
    let _ = unsafe { FindClose(handle) };
    // `dwReserved0` holds the reparse tag when the reparse-point attribute is set.
    let tag = if data.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 { data.dwReserved0 } else { 0 };
    Some((data.dwFileAttributes, tag))
}

/// Walks the directory `root` (a folder, drive root or UNC path) in parallel.
/// The root gets its absolute path without a trailing backslash as its name.
/// A root that is a link is listed as the folder it points to.
pub fn scan_walk(root: &Path, progress: &Progress) -> Result<ScanResult, ScanError> {
    let started = Instant::now();
    if progress.cancel.load(Ordering::Relaxed) {
        return Err(ScanError::Cancelled);
    }
    progress.done.store(0, Ordering::Relaxed);
    progress.total.store(0, Ordering::Relaxed);
    if is_device_path(root) {
        return Err(ScanError::Io(io::Error::new(io::ErrorKind::InvalidInput, "Device paths are not supported")));
    }
    let given = verbatim_path(root).map_err(scan_error)?;
    // The path as given is checked before anything on it is opened: opening
    // an online-only item can already make the provider fetch it. Links on
    // the way are checked themselves; what lies behind them is checked once
    // they are resolved.
    let first = check_levels(&given, true);
    if first == LevelCheck::Refuse {
        return Err(ScanError::OnlineOnly);
    }
    ensure_directory(&given, true).map_err(scan_error)?;
    let resolved = final_path(&given);
    let (root_path, check_fully) = plan_root(given, first, resolved);
    if check_fully && check_levels(&root_path, false) == LevelCheck::Refuse {
        return Err(ScanError::OnlineOnly);
    }
    ensure_directory(&root_path, false).map_err(scan_error)?;
    let root_name = root_name(root).map_err(scan_error)?;

    let mut b = TreeBuilder::with_capacity(4096, 64 * 1024);
    let root_id = b.push(&root_name, NO_PARENT, 0, 0, 0, F_DIR);
    let queue = Queue { state: Mutex::new((Vec::new(), false)), ready: Condvar::new() };
    queue.push(vec![Job { dir: root_id, path: root_path, follow: true }]);
    let threads = worker_threads(std::thread::available_parallelism().ok().map(|n| n.get()));
    let (mut unreadable, mut online_only) = (0u64, 0u64);

    let outcome = std::thread::scope(|s| {
        let (tx, rx) = mpsc::channel::<Done>();
        for _ in 0..threads {
            let tx = tx.clone();
            let (queue, cancel) = (&queue, &progress.cancel);
            s.spawn(move || worker(queue, tx, cancel));
        }
        drop(tx);
        let mut pending = 1usize;
        let result = loop {
            // Err only if every worker is gone (a panic); the scope re-raises it.
            let Ok(Done { job, listing }) = rx.recv() else {
                break Err(ScanError::Io(io::Error::other("Folder listing aborted")));
            };
            pending -= 1;
            let entries = match listing {
                Listing::Cancelled => break Err(ScanError::Cancelled),
                Listing::Complete(entries) => entries,
                Listing::Failed(entries, e) if job.dir == root_id && entries.is_empty() => break Err(scan_error(e)),
                Listing::Failed(entries, _) => {
                    b.add_flags(job.dir, F_UNREADABLE);
                    unreadable += 1;
                    entries
                }
            };
            progress.done.fetch_add(entries.len() as u64, Ordering::Relaxed);
            let mut jobs = Vec::new();
            for e in entries {
                let link = e.attributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 && e.reparse_tag & REPARSE_NAME_SURROGATE != 0;
                if e.attributes & FILE_ATTRIBUTE_DIRECTORY.0 == 0 {
                    b.push(&e.name, job.dir, e.size, e.allocated, e.mtime, if link { F_LINK } else { 0 });
                    continue;
                }
                let extra = match classify_dir(e.attributes, e.reparse_tag) {
                    DirAction::Descend => 0,
                    DirAction::Link => F_LINK,
                    DirAction::OnlineOnly => {
                        online_only += 1;
                        F_ONLINE_ONLY
                    }
                };
                let id = b.push(&e.name, job.dir, 0, 0, e.mtime, F_DIR | extra);
                if let Some(wide) = e.wide_name {
                    jobs.push(Job { dir: id, path: join(&job.path, &wide), follow: false });
                }
            }
            pending += jobs.len();
            queue.push(jobs);
            if progress.cancel.load(Ordering::Relaxed) {
                break Err(ScanError::Cancelled);
            }
            if pending == 0 {
                break Ok(());
            }
        };
        queue.close();
        result
    });
    outcome?;

    let (tree, stats) = b.finish(root_id);
    Ok(ScanResult {
        tree,
        method: ScanMethod::Walk,
        duration: started.elapsed(),
        // Every parent is a directory pushed before its entries, so this is 0.
        skipped: stats.orphaned,
        unreadable,
        online_only,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_directory_without_dot_entries_is_complete() {
        let not_found = Some(ERROR_FILE_NOT_FOUND.0 as i32);
        let no_more = Some(ERROR_NO_MORE_FILES.0 as i32);
        assert_eq!(listing_end(not_found, false), ListEnd::Complete);
        assert_eq!(listing_end(not_found, true), ListEnd::Failed);
        assert_eq!(listing_end(no_more, false), ListEnd::Complete);
        assert_eq!(listing_end(no_more, true), ListEnd::Complete);
        assert_eq!(listing_end(Some(5), false), ListEnd::Failed); // access denied
        assert_eq!(listing_end(None, false), ListEnd::Failed);
    }

    #[test]
    fn online_only_root_is_refused() {
        let one = |a: u32| path_access([Some(a)]);
        assert_eq!(one(0x10), RootAccess::Proceed);
        assert_eq!(one(0x410), RootAccess::Proceed); // a link the user chose
        assert_eq!(one(0x0040_0410), RootAccess::Proceed); // recall on data access only
        assert_eq!(one(0x0004_0410), RootAccess::Refuse);
        assert_eq!(one(0x0004_0010), RootAccess::Refuse);
    }

    #[test]
    fn whole_path_is_checked() {
        let (dir, online) = (Some(0x10), Some(0x0004_0410));
        assert_eq!(path_access([dir, dir, dir]), RootAccess::Proceed);
        assert_eq!(path_access([dir, dir, online]), RootAccess::Refuse);
        assert_eq!(path_access([online, dir, dir]), RootAccess::Refuse);
        // The second level is not readable: the check stops and the scan proceeds.
        assert_eq!(path_access([dir, None, online]), RootAccess::Proceed);
        // A drive root has no levels.
        assert_eq!(path_access([]), RootAccess::Proceed);
    }

    #[test]
    fn levels_are_queried_top_down_and_only_after_their_parent_passed() {
        let answers = [Some(0x10), Some(0x0004_0010), Some(0x10)];
        let mut asked = 0;
        let levels = answers.iter().map(|&a| {
            asked += 1;
            a
        });
        assert_eq!(path_access(levels), RootAccess::Refuse);
        assert_eq!(asked, 2, "a level below an online-only folder was queried");

        let answers = [None, Some(0x10)];
        let mut asked = 0;
        let levels = answers.iter().map(|&a| {
            asked += 1;
            a
        });
        assert_eq!(path_access(levels), RootAccess::Proceed);
        assert_eq!(asked, 1, "a level below an unreadable one was queried");
    }

    #[test]
    fn listed_path_is_checked_in_full_after_a_link() {
        let w = |s: &str| s.encode_utf16().collect::<Vec<_>>();
        let given = w(r"\\?\C:\mnt\a");
        let other = w(r"\\?\D:\a");
        // The first check covered every level: no second check.
        assert_eq!(plan_root(given.clone(), LevelCheck::Proceed, Some(other.clone())), (other.clone(), false));
        assert_eq!(plan_root(given.clone(), LevelCheck::Proceed, None), (given.clone(), false));
        // It ended at a link and resolution failed: the given path in full.
        assert_eq!(plan_root(given.clone(), LevelCheck::ReachedLink, None), (given.clone(), true));
        // It ended at a link and resolution returned the same path: that path in full.
        assert_eq!(plan_root(given.clone(), LevelCheck::ReachedLink, Some(given.clone())), (given.clone(), true));
        // It ended at a link and resolution returned another path: the resolved one in full.
        assert_eq!(plan_root(given, LevelCheck::ReachedLink, Some(other.clone())), (other, true));
    }

    #[test]
    fn levels_below_the_drive_or_share_root() {
        let w = |s: &str| s.encode_utf16().collect::<Vec<_>>();
        let ws = |v: &[&str]| v.iter().map(|s| w(s)).collect::<Vec<_>>();
        assert_eq!(levels(&w(r"\\?\C:\a\b")), ws(&[r"\\?\C:\a", r"\\?\C:\a\b"]));
        assert_eq!(levels(&w(r"\\?\C:\a\b\")), ws(&[r"\\?\C:\a", r"\\?\C:\a\b"]));
        assert_eq!(levels(&w(r"\\?\C:\")), ws(&[]));
        assert_eq!(levels(&w(r"\\?\C:")), ws(&[]));
        assert_eq!(levels(&w(r"\\?\UNC\nas\share")), ws(&[]));
        assert_eq!(levels(&w(r"\\?\UNC\nas\share\")), ws(&[]));
        assert_eq!(levels(&w(r"\\?\UNC\nas\share\d\e")), ws(&[r"\\?\UNC\nas\share\d", r"\\?\UNC\nas\share\d\e"]));
        assert_eq!(levels(&w(r"\\?\unc\nas\share\d")), ws(&[r"\\?\unc\nas\share\d"]));
        assert_eq!(levels(&w(r"C:\a")), ws(&[]));
    }

    #[test]
    fn final_paths_are_normalized() {
        let w = |s: &str| s.encode_utf16().collect::<Vec<_>>();
        assert_eq!(normalize_final(w(r"\\?\C:\x\")), Some(w(r"\\?\C:\x")));
        assert_eq!(normalize_final(w(r"\\?\C:\x")), Some(w(r"\\?\C:\x")));
        assert_eq!(normalize_final(w(r"\\?\C:\")), Some(w(r"\\?\C:\")));
        assert_eq!(normalize_final(w(r"\\?\C:")), Some(w(r"\\?\C:\")));
        assert_eq!(normalize_final(w(r"\\?\UNC\nas\share\d")), Some(w(r"\\?\UNC\nas\share\d")));
        assert_eq!(normalize_final(w(r"\\?\UNC\nas\share")), Some(w(r"\\?\UNC\nas\share\")));
        assert_eq!(normalize_final(w(r"\Device\HarddiskVolume3\x")), None);
        assert_eq!(normalize_final(w(r"\\?\")), None);
    }

    /// Uses the links every Windows installation has, since this module
    /// creates no files of its own (see `tests/anti_goals.rs`); links made by
    /// `mklink` are tested in `tests/walk_tempdir.rs`.
    #[test]
    fn final_path_resolves_links() {
        let w = |s: &str| s.encode_utf16().collect::<Vec<_>>();
        let same = |a: &[u16], b: &str| String::from_utf16_lossy(a).eq_ignore_ascii_case(b);
        let resolve = |p: &str| final_path(&verbatim_path(Path::new(p)).unwrap());
        match std::env::var("SystemDrive") {
            Ok(drive) => {
                // A junction and a directory symbolic link.
                let links = [
                    (format!(r"{drive}\Documents and Settings"), format!(r"\\?\{drive}\Users")),
                    (format!(r"{drive}\Users\All Users"), format!(r"\\?\{drive}\ProgramData")),
                ];
                for (link, target) in &links {
                    let given = verbatim_path(Path::new(link)).unwrap();
                    let is_link = entry_attributes(&given).is_some_and(|(a, tag)| classify_dir(a, tag) == DirAction::Link);
                    if !is_link {
                        eprintln!("skipped {link}: not a link on this system");
                        continue;
                    }
                    let resolved = resolve(link).unwrap_or_else(|| panic!("{link} not resolved"));
                    assert!(same(&resolved, target), "{link} -> {}", String::from_utf16_lossy(&resolved));
                    assert_eq!(check_levels(&resolved, false), LevelCheck::Proceed);
                    assert_eq!(check_levels(&given, true), LevelCheck::ReachedLink);
                }
            }
            Err(_) => eprintln!("skipped the system links: SystemDrive is not set"),
        }
        // A folder that is no link resolves to itself, with long names.
        let tmp = tempfile::tempdir().unwrap();
        let plain = final_path(&verbatim_path(tmp.path()).unwrap()).expect("folder resolved");
        assert_eq!(final_path(&plain), Some(plain.clone()));
        // A drive root keeps its backslash.
        let drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".to_string());
        let root = w(&format!(r"\\?\{drive}\"));
        let resolved_root = final_path(&root).expect("drive root resolved");
        assert!(same(&resolved_root, &format!(r"\\?\{drive}\")));
        assert_eq!(final_path(&verbatim_path(&tmp.path().join("missing")).unwrap()), None);
    }

    #[test]
    fn worker_thread_count() {
        assert_eq!(worker_threads(Some(1)), 2);
        assert_eq!(worker_threads(Some(2)), 2);
        assert_eq!(worker_threads(Some(8)), 8);
        assert_eq!(worker_threads(Some(16)), 16);
        assert_eq!(worker_threads(Some(64)), 16);
        assert_eq!(worker_threads(None), FALLBACK_THREADS);
    }

    #[test]
    fn device_paths() {
        assert!(is_device_path(Path::new(r"\\.\C:")));
        assert!(is_device_path(Path::new(r"\\.\PhysicalDrive0")));
        assert!(is_device_path(Path::new("//./C:")));
        assert!(!is_device_path(Path::new(r"\\?\C:\x")));
        assert!(!is_device_path(Path::new(r"\\nas\share")));
        assert!(!is_device_path(Path::new(r"C:\.\x")));
        assert!(!is_device_path(Path::new(r"\\.")));
    }

    #[test]
    fn roots_have_no_parent_entry() {
        let w = |s: &str| s.encode_utf16().collect::<Vec<_>>();
        assert_eq!(entry_path(&w(r"\\?\C:\")), None);
        assert_eq!(entry_path(&w(r"\\?\C:")), None);
        assert_eq!(entry_path(&w(r"\\?\UNC\nas\share")), None);
        assert_eq!(entry_path(&w(r"\\?\UNC\nas\share\")), None);
        assert_eq!(entry_path(&w(r"\\?\C:\x\")), Some(w(r"\\?\C:\x")));
        assert_eq!(entry_path(&w(r"\\?\UNC\nas\share\d")), Some(w(r"\\?\UNC\nas\share\d")));
    }

    #[test]
    fn entry_attributes_of_a_local_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let attributes = entry_attributes(&verbatim_path(tmp.path()).unwrap()).expect("attributes");
        assert_ne!(attributes.0 & FILE_ATTRIBUTE_DIRECTORY.0, 0);
        assert_eq!(attributes.1, 0);
        assert_eq!(path_access([Some(attributes.0)]), RootAccess::Proceed);
        let mut with_slash = verbatim_path(tmp.path()).unwrap();
        with_slash.push(u16::from(b'\\'));
        assert_eq!(entry_attributes(&with_slash), Some(attributes));
        assert_eq!(entry_attributes(&verbatim_path(Path::new(r"C:\")).unwrap()), None);
    }

    #[test]
    fn root_names() {
        assert_eq!(root_name(Path::new(r"C:\")).unwrap(), "C:");
        assert_eq!(root_name(Path::new("c:/")).unwrap(), "c:");
        assert_eq!(root_name(Path::new("C:/x/y/")).unwrap(), r"C:\x\y");
        assert_eq!(root_name(Path::new(r"\\nas\share\")).unwrap(), r"\\nas\share");
    }

    #[test]
    fn classify() {
        assert_eq!(classify_dir(0x10, 0), DirAction::Descend);
        assert_eq!(classify_dir(0x410, 0xA0000003), DirAction::Link); // junction
        assert_eq!(classify_dir(0x410, 0xA000000C), DirAction::Link); // symlink
        assert_eq!(classify_dir(0x410, 0x9000001A), DirAction::Descend); // OneDrive folder, present locally
        assert_eq!(classify_dir(0x0004_0410, 0x9000001A), DirAction::OnlineOnly);
    }

    /// Appends one record; returns its start so the caller can patch `NextEntryOffset`.
    fn record(buf: &mut Vec<u8>, name: &str, attributes: u32, ea: u32, size: i64) -> usize {
        while !buf.len().is_multiple_of(8) {
            buf.push(0);
        }
        let start = buf.len();
        buf.resize(start + OFF_NAME, 0);
        let wide: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
        buf[start + OFF_MTIME..][..8].copy_from_slice(&77i64.to_le_bytes());
        buf[start + OFF_EOF..][..8].copy_from_slice(&size.to_le_bytes());
        buf[start + OFF_ALLOC..][..8].copy_from_slice(&4096i64.to_le_bytes());
        buf[start + OFF_ATTR..][..4].copy_from_slice(&attributes.to_le_bytes());
        buf[start + OFF_NAME_LEN..][..4].copy_from_slice(&(wide.len() as u32).to_le_bytes());
        buf[start + OFF_EA_SIZE..][..4].copy_from_slice(&ea.to_le_bytes());
        buf.extend_from_slice(&wide);
        start
    }

    fn link_next(buf: &mut [u8], from: usize, to: usize) {
        buf[from + OFF_NEXT..][..4].copy_from_slice(&((to - from) as u32).to_le_bytes());
    }

    #[test]
    fn layout_matches_ms_fscc() {
        assert_eq!((OFF_EOF, OFF_ALLOC, OFF_ATTR, OFF_NAME_LEN, OFF_EA_SIZE, OFF_NAME), (40, 48, 56, 60, 64, 104));
    }

    #[test]
    fn parses_chain_and_skips_dots() {
        let mut buf = Vec::new();
        let a = record(&mut buf, ".", 0x10, 0, 0);
        let b = record(&mut buf, "..", 0x10, 0, 0);
        let c = record(&mut buf, "größe.txt", 0x20, 0, -5);
        let d = record(&mut buf, "j", 0x410, 0xA000_0003, 0);
        let e = record(&mut buf, "sub", 0x10, 9, 0); // EaSize without reparse flag is no tag
        link_next(&mut buf, a, b);
        link_next(&mut buf, b, c);
        link_next(&mut buf, c, d);
        link_next(&mut buf, d, e);
        let mut out = Vec::new();
        parse_records(&buf, &mut out).unwrap();
        let names: Vec<_> = out.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["größe.txt", "j", "sub"]);
        assert_eq!((out[0].size, out[0].allocated, out[0].mtime), (0, 4096, 77));
        assert_eq!((out[1].reparse_tag, out[1].wide_name.is_none()), (0xA000_0003, true));
        assert_eq!((out[2].reparse_tag, out[2].wide_name.as_deref()), (0, Some(&[0x73u16, 0x75, 0x62][..])));
    }

    #[test]
    fn rejects_records_outside_buffer() {
        let mut buf = Vec::new();
        record(&mut buf, "abc", 0x20, 0, 1);
        let mut out = Vec::new();
        // Name runs past the end.
        assert!(parse_records(&buf[..buf.len() - 1], &mut out).is_err());
        // Header cut off.
        assert!(parse_records(&buf[..OFF_NAME - 1], &mut out).is_err());
        // Next record lies beyond the buffer.
        let mut far = buf.clone();
        far[OFF_NEXT..][..4].copy_from_slice(&4096u32.to_le_bytes());
        assert!(parse_records(&far, &mut out).is_err());
        // Next record overlaps this one's name.
        let mut overlap = buf.clone();
        overlap[OFF_NEXT..][..4].copy_from_slice(&8u32.to_le_bytes());
        assert!(parse_records(&overlap, &mut out).is_err());
        // Odd name length.
        let mut odd = buf.clone();
        odd[OFF_NAME_LEN..][..4].copy_from_slice(&5u32.to_le_bytes());
        assert!(parse_records(&odd, &mut out).is_err());
    }

    #[test]
    fn empty_name_is_malformed() {
        let mut buf = Vec::new();
        let a = record(&mut buf, "a", 0x20, 0, 1);
        let b = record(&mut buf, "", 0x20, 0, 1);
        link_next(&mut buf, a, b);
        let mut out = Vec::new();
        let err = parse_records(&buf, &mut out).expect_err("empty name accepted");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert_eq!(err.to_string(), "Malformed folder listing: empty name");
        // Entries read before it are kept.
        assert_eq!(out.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["a"]);
    }

    #[test]
    fn bare_drive_letter_is_the_drive_root() {
        let w = |s: &str| s.encode_utf16().collect::<Vec<_>>();
        assert_eq!(verbatim_path(Path::new("C:")).unwrap(), w(r"\\?\C:\"));
        assert_eq!(root_name(Path::new("C:")).unwrap(), "C:");
        assert_eq!(root_name(Path::new("d:")).unwrap(), "d:");
    }

    #[test]
    fn nt_prefix() {
        let w = |s: &str| s.encode_utf16().collect::<Vec<_>>();
        assert_eq!(nt_path(&w(r"\\?\C:\x")).unwrap(), w(r"\??\C:\x"));
        assert_eq!(nt_path(&w(r"\\?\UNC\nas\share")).unwrap(), w(r"\??\UNC\nas\share"));
        assert!(nt_path(&w(r"C:\x")).is_err());
    }

    #[test]
    fn open_dir_refuses_files() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("f.txt");
        std::fs::write(&file, b"abc").unwrap();
        for follow in [true, false] {
            let err = open_dir(&verbatim_path(&file).unwrap(), follow).err().expect("file was opened");
            assert_eq!(err.kind(), io::ErrorKind::NotADirectory);
            assert!(open_dir(&verbatim_path(tmp.path()).unwrap(), follow).is_ok());
        }
    }

    #[test]
    fn verbatim_prefixes() {
        let w = |s: &str| s.encode_utf16().collect::<Vec<_>>();
        assert_eq!(verbatim_path(Path::new(r"C:\x\y")).unwrap(), w(r"\\?\C:\x\y"));
        assert_eq!(verbatim_path(Path::new(r"C:\")).unwrap(), w(r"\\?\C:\"));
        assert_eq!(verbatim_path(Path::new(r"\\nas\share\d")).unwrap(), w(r"\\?\UNC\nas\share\d"));
        assert_eq!(verbatim_path(Path::new(r"\\?\D:\z")).unwrap(), w(r"\\?\D:\z"));
        assert_eq!(join(&w(r"\\?\C:\"), &w("a")), w(r"\\?\C:\a"));
        assert_eq!(join(&w(r"\\?\C:\x"), &w("a")), w(r"\\?\C:\x\a"));
    }
}
