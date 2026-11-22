//! Fallback scanner: walks directories in parallel with ordinary Win32 calls.
//!
//! Used without admin rights, on non-NTFS volumes, on network paths and for
//! single folders. Directories are opened for listing only; files are never
//! opened. The walk uses an explicit work queue, never recursion: worker
//! threads list directories, the calling thread alone fills the tree and
//! hands out new directories as jobs.
use std::io;
use std::mem::offset_of;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::Path;
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
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_RECALL_ON_OPEN, FILE_ATTRIBUTE_REPARSE_POINT, FILE_ID_BOTH_DIR_INFO,
    FILE_LIST_DIRECTORY, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FIND_FIRST_EX_FLAGS,
    FileIdBothDirectoryInfo, FindClose, FindExInfoBasic, FindExSearchNameMatch, FindFirstFileExW, GetFileAttributesW,
    GetFileInformationByHandleEx, INVALID_FILE_ATTRIBUTES, SYNCHRONIZE, WIN32_FIND_DATAW,
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
const MIN_THREADS: usize = 4;

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

/// A directory handle opened for listing only; closed when dropped.
struct DirHandle(HANDLE);

impl Drop for DirHandle {
    fn drop(&mut self) {
        // SAFETY: the handle was opened by `open_dir`, is valid and is closed only here.
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
    let mut options = FILE_DIRECTORY_FILE.0 | FILE_SYNCHRONOUS_IO_NONALERT.0 | FILE_OPEN_FOR_BACKUP_INTENT.0;
    if !follow {
        options |= FILE_OPEN_REPARSE_POINT.0;
    }
    let mut handle = HANDLE::default();
    let mut iosb = IO_STATUS_BLOCK::default();
    // SAFETY: all pointers point to live locals; `name` and `nt` outlive the call.
    let status = unsafe {
        NtOpenFile(
            &mut handle,
            (FILE_LIST_DIRECTORY | SYNCHRONIZE).0,
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
/// Reads the attributes only; the object is not opened for any data access.
fn ensure_directory(path: &[u16]) -> io::Result<()> {
    let mut z = Vec::with_capacity(path.len() + 1);
    z.extend_from_slice(path);
    z.push(0);
    // SAFETY: `z` is NUL-terminated and outlives the call.
    let attributes = unsafe { GetFileAttributesW(PCWSTR(z.as_ptr())) };
    if attributes == INVALID_FILE_ATTRIBUTES {
        return Err(io::Error::last_os_error());
    }
    if attributes & FILE_ATTRIBUTE_DIRECTORY.0 == 0 {
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

/// `root` as an absolute path with `\\?\` (or `\\?\UNC\`) prefix, so long
/// paths work. `\\?\` and `\\.\` paths are kept as they are.
fn verbatim_path(root: &Path) -> io::Result<Vec<u16>> {
    let abs = std::path::absolute(root)?;
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
    let abs = std::path::absolute(root)?;
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

/// Whether a walk may list a root with these attributes (from its parent's listing).
fn root_access(attributes: u32) -> RootAccess {
    if attributes & FILE_ATTRIBUTE_RECALL_ON_OPEN.0 != 0 { RootAccess::Refuse } else { RootAccess::Proceed }
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

/// The attributes of `path`'s own entry as its parent's listing reports
/// them. Unlike `GetFileAttributesW`, a directory listing carries
/// `FILE_ATTRIBUTE_RECALL_ON_OPEN`. Lists the parent filtered to this one
/// name; `path` itself is neither opened nor listed. `None` for a drive or
/// share root, or if the parent cannot be listed.
fn entry_attributes(path: &[u16]) -> Option<u32> {
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
    Some(data.dwFileAttributes)
}

/// Walks the directory `root` (a folder, drive root or UNC path) in parallel.
/// The root gets its absolute path without a trailing backslash as its name.
pub fn scan_walk(root: &Path, progress: &Progress) -> Result<ScanResult, ScanError> {
    let started = Instant::now();
    if progress.cancel.load(Ordering::Relaxed) {
        return Err(ScanError::Cancelled);
    }
    progress.done.store(0, Ordering::Relaxed);
    progress.total.store(0, Ordering::Relaxed);
    let root_path = verbatim_path(root).map_err(scan_error)?;
    ensure_directory(&root_path).map_err(scan_error)?;
    if entry_attributes(&root_path).map(root_access) == Some(RootAccess::Refuse) {
        return Err(ScanError::OnlineOnly);
    }
    let root_name = root_name(root).map_err(scan_error)?;

    let mut b = TreeBuilder::with_capacity(4096, 64 * 1024);
    let root_id = b.push(&root_name, NO_PARENT, 0, 0, 0, F_DIR);
    let queue = Queue { state: Mutex::new((Vec::new(), false)), ready: Condvar::new() };
    queue.push(vec![Job { dir: root_id, path: root_path, follow: true }]);
    let threads = std::thread::available_parallelism().map_or(MIN_THREADS, |n| n.get()).clamp(MIN_THREADS, MAX_THREADS);
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
                break Err(ScanError::Io(io::Error::other("folder listing aborted")));
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
        assert_eq!(root_access(0x10), RootAccess::Proceed);
        assert_eq!(root_access(0x410), RootAccess::Proceed); // a link the user chose
        assert_eq!(root_access(0x0040_0410), RootAccess::Proceed); // recall on data access only
        assert_eq!(root_access(0x0004_0410), RootAccess::Refuse);
        assert_eq!(root_access(0x0004_0010), RootAccess::Refuse);
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
        assert_ne!(attributes & FILE_ATTRIBUTE_DIRECTORY.0, 0);
        assert_eq!(root_access(attributes), RootAccess::Proceed);
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
