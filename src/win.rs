//! Windows bindings: volumes, elevation, time, memory and a read-only raw volume.
use std::ffi::OsString;
use std::fs::File;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::{FileExt, OpenOptionsExt};
use std::os::windows::process::CommandExt;

use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, HWND, LPARAM, LRESULT, MAX_PATH, SYSTEMTIME, WPARAM};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::Storage::FileSystem::{
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives,
    GetLongPathNameW, GetVolumeInformationW, QueryDosDeviceW,
};
use windows::Win32::System::Diagnostics::Debug::{SEM_FAILCRITICALERRORS, SetThreadErrorMode, THREAD_ERROR_MODE};
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows::Win32::System::SystemInformation::GetSystemWindowsDirectoryW;
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
use windows::Win32::System::WindowsProgramming::{DRIVE_FIXED, DRIVE_REMOVABLE};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    MB_ICONERROR, MB_OK, MessageBoxW, SW_SHOWNORMAL, SWP_NOSIZE, WINDOWPOS, WM_NCDESTROY, WM_WINDOWPOSCHANGING,
};
use windows::core::{HSTRING, PCWSTR, w};

use crate::scan::mft::VolumeReader;

pub struct VolumeInfo {
    pub letter: char,
    pub fs_name: String,
    pub total: u64,
    pub free: u64,
}

/// Closes a kernel handle when dropped.
struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: the handle was opened by us, is valid and is closed only here.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// Suppresses the "no disk in drive" system dialog on this thread while alive.
struct NoErrorDialog(THREAD_ERROR_MODE);

impl NoErrorDialog {
    fn new() -> Self {
        let mut old = THREAD_ERROR_MODE(0);
        // SAFETY: `old` is a valid out pointer for the duration of the call.
        let _ = unsafe { SetThreadErrorMode(SEM_FAILCRITICALERRORS, Some(&mut old)) };
        NoErrorDialog(old)
    }
}

impl Drop for NoErrorDialog {
    fn drop(&mut self) {
        // SAFETY: restores the mode saved in `new`; no out pointer is passed.
        let _ = unsafe { SetThreadErrorMode(self.0, None) };
    }
}

fn drive_root(letter: char) -> Option<HSTRING> {
    letter.is_ascii_alphabetic().then(|| HSTRING::from(format!("{}:\\", letter.to_ascii_uppercase())))
}

/// Fixed and removable drives with a readable volume, in letter order.
pub fn list_volumes() -> Vec<VolumeInfo> {
    // SAFETY: takes no arguments and only returns a bit mask.
    let mask = unsafe { GetLogicalDrives() };
    (0..26u8)
        .filter(|i| mask & (1 << i) != 0)
        .map(|i| char::from(b'A' + i))
        .filter(|&letter| {
            let Some(root) = drive_root(letter) else { return false };
            // SAFETY: `root` is a valid, NUL-terminated wide string that outlives the call.
            let kind = unsafe { GetDriveTypeW(&root) };
            kind == DRIVE_FIXED || kind == DRIVE_REMOVABLE
        })
        .filter_map(volume_info)
        .collect()
}

/// File system and space of drive `letter`; `None` if it has no readable volume.
pub fn volume_info(letter: char) -> Option<VolumeInfo> {
    let root = drive_root(letter)?;
    let _guard = NoErrorDialog::new();
    let mut fs_buf = [0u16; MAX_PATH as usize + 1];
    // SAFETY: `root` is NUL-terminated and `fs_buf` is a writable buffer whose length the binding passes.
    unsafe { GetVolumeInformationW(&root, None, None, None, None, Some(&mut fs_buf)) }.ok()?;
    let fs_len = fs_buf.iter().position(|&c| c == 0).unwrap_or(fs_buf.len());
    let (mut total, mut free) = (0u64, 0u64);
    // SAFETY: `root` is NUL-terminated; both out pointers point to live `u64`s.
    unsafe { GetDiskFreeSpaceExW(&root, None, Some(&mut total), Some(&mut free)) }.ok()?;
    Some(VolumeInfo {
        letter: letter.to_ascii_uppercase(),
        fs_name: String::from_utf16_lossy(&fs_buf[..fs_len]),
        total,
        free,
    })
}

/// True if the DOS device a drive letter stands for is a path, as `subst`
/// makes it (`\??\C:\Data`), and not a volume (`\Device\HarddiskVolume3`).
fn is_subst_target(device: &str) -> bool {
    device.starts_with(r"\??\")
}

/// True if drive `letter` was made with `subst`: a folder under a drive
/// letter. Only the name of the drive's device is read; false if it has none.
pub fn is_subst_drive(letter: char) -> bool {
    if !letter.is_ascii_alphabetic() {
        return false;
    }
    let name = HSTRING::from(format!("{}:", letter.to_ascii_uppercase()));
    // A `subst` target is `\??\` plus a path, so it can be long.
    let mut buf = vec![0u16; 1024];
    // SAFETY: `name` is NUL-terminated and outlives the call; `buf` is writable and the binding passes its length.
    let n = unsafe { QueryDosDeviceW(&name, Some(&mut buf)) } as usize;
    // The buffer holds a list of NUL-terminated strings; the first is the current target.
    let first = buf[..n.min(buf.len())].split(|&c| c == 0).next().unwrap_or(&[]);
    is_subst_target(&String::from_utf16_lossy(first))
}

/// The long form of `path` (8.3 short names expanded, letter case as stored).
/// The path must exist; the file's contents are not opened.
pub fn long_path_name(path: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    if wide.contains(&0) {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "Path contains a NUL character"));
    }
    wide.push(0);
    let mut buf = vec![0u16; MAX_PATH as usize + 1];
    loop {
        // SAFETY: `wide` is NUL-terminated and outlives the call; `buf` is writable and the binding passes its length.
        let n = unsafe { GetLongPathNameW(PCWSTR(wide.as_ptr()), Some(&mut buf)) } as usize;
        if n == 0 {
            return Err(std::io::Error::last_os_error());
        }
        if n < buf.len() {
            return Ok(OsString::from_wide(&buf[..n]).into());
        }
        // Too small: `n` is the required size including the terminating NUL.
        buf.resize(n, 0);
    }
}

/// True if this process runs with an elevated (admin) token.
pub fn is_elevated() -> bool {
    let mut token = HANDLE::default();
    // SAFETY: the current-process pseudo handle needs no closing; `token` is a valid out pointer.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.is_err() {
        return false;
    }
    let token = OwnedHandle(token);
    let mut elevation = TOKEN_ELEVATION::default();
    let mut len = 0u32;
    // SAFETY: the buffer is a live TOKEN_ELEVATION and its exact size is passed.
    let ok = unsafe {
        GetTokenInformation(
            token.0,
            TokenElevation,
            Some((&raw mut elevation).cast()),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        )
    };
    ok.is_ok() && elevation.TokenIsElevated != 0
}

/// Quotes one argument by the rules of `CommandLineToArgvW`.
fn quote_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        return arg.to_string();
    }
    let mut out = String::from("\"");
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
    out
}

/// The working directory to give the elevated copy: `dir` only when it lies on a local
/// fixed drive (`drive_type` gives the type of a drive letter's root). A mapped network
/// drive is usually not visible to the elevated process, and a removable drive or a UNC
/// path may not be there either; then no directory is passed and Windows picks one.
fn elevation_dir(dir: &std::path::Path, drive_type: impl FnOnce(char) -> u32) -> Option<&std::path::Path> {
    use std::path::{Component, Prefix};
    let letter = match dir.components().next()? {
        Component::Prefix(p) => match p.kind() {
            Prefix::Disk(l) | Prefix::VerbatimDisk(l) => char::from(l),
            _ => return None,
        },
        _ => return None,
    };
    (drive_type(letter) == DRIVE_FIXED).then_some(dir)
}

/// The type of drive `letter`'s root (`DRIVE_FIXED`, `DRIVE_REMOTE`, ...).
fn drive_type(letter: char) -> u32 {
    // SAFETY: `root` is a valid, NUL-terminated wide string that outlives the call.
    drive_root(letter).map_or(0, |root| unsafe { GetDriveTypeW(&root) })
}

/// Starts this program again with admin rights (UAC prompt), in the current working
/// directory if that lies on a local fixed drive. `Err` if declined or failed.
pub fn relaunch_elevated(args: &[String]) -> std::io::Result<()> {
    let exe = HSTRING::from(std::env::current_exe()?.as_os_str());
    let params = HSTRING::from(args.iter().map(|a| quote_arg(a)).collect::<Vec<_>>().join(" "));
    let cwd = std::env::current_dir().ok();
    let dir = cwd.as_deref().and_then(|d| elevation_dir(d, drive_type)).map(|d| HSTRING::from(d.as_os_str()));
    let dir_ptr = dir.as_ref().map_or(PCWSTR::null(), |d| PCWSTR(d.as_ptr()));
    // SAFETY: all strings are NUL-terminated and outlive the call (`dir` lives until the end
    // of the function); no window handle is passed.
    let result = unsafe { ShellExecuteW(None, w!("runas"), &exe, &params, dir_ptr, SW_SHOWNORMAL) };
    // ShellExecuteW reports success with a value above 32.
    if result.0 as usize > 32 {
        Ok(())
    } else {
        Err(std::io::Error::other(format!("Restart with administrator rights failed (code {})", result.0 as usize)))
    }
}

/// Shows a native error message box and waits until it is closed.
pub fn show_error(title: &str, text: &str) {
    let (title, text) = (HSTRING::from(title), HSTRING::from(text));
    // SAFETY: both strings are NUL-terminated and outlive the call; no owner window is passed.
    let _ = unsafe { MessageBoxW(None, &text, &title, MB_OK | MB_ICONERROR) };
}

/// The width and height a window gets when neither may be more than `max`.
fn limited_size(cx: i32, cy: i32, max: i32) -> (i32, i32) {
    (cx.min(max), cy.min(max))
}

/// Keeps the window `hwnd` at most `max` pixels wide and high, whoever sets
/// its size and however: the user, another program, a change of the display
/// scale. Call it on the thread that owns the window. Returns whether the
/// limit is in place.
///
/// The window library answers size changes itself without asking Windows to
/// check them, so a size limit given to it holds for dragging only. A
/// drawing surface larger than the graphics driver supports can end the
/// program inside the driver.
pub fn limit_window_size(hwnd: isize, max: i32) -> bool {
    let Ok(limit) = usize::try_from(max) else { return false };
    if limit == 0 {
        return false;
    }
    // SAFETY: `size_limit_proc` matches the subclass signature and stays valid
    // for the life of the program; an unknown window handle makes the call fail.
    unsafe { SetWindowSubclass(HWND(hwnd as *mut _), Some(size_limit_proc), SIZE_LIMIT_ID, limit).as_bool() }
}

/// Identifies `size_limit_proc` among the subclasses of a window.
const SIZE_LIMIT_ID: usize = 1;

/// Runs before the window's own procedure: cuts every new size to the limit
/// in `max`, then passes the message on.
unsafe extern "system" fn size_limit_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM, _id: usize, max: usize) -> LRESULT {
    if msg == WM_WINDOWPOSCHANGING && lparam.0 != 0 {
        // SAFETY: for this message Windows passes a pointer to a WINDOWPOS the
        // receiver may change.
        let pos = unsafe { &mut *(lparam.0 as *mut WINDOWPOS) };
        if !pos.flags.contains(SWP_NOSIZE) {
            (pos.cx, pos.cy) = limited_size(pos.cx, pos.cy, i32::try_from(max).unwrap_or(i32::MAX));
        }
    } else if msg == WM_NCDESTROY {
        // SAFETY: the window is being destroyed; the subclass must go before it is.
        let _ = unsafe { RemoveWindowSubclass(hwnd, Some(size_limit_proc), SIZE_LIMIT_ID) };
    }
    // SAFETY: passes the unchanged message on to the next procedure of this window.
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

/// The argument that makes Explorer select `path`, or `None` if `path`
/// contains a `"`: it would end the quoted argument and turn the rest into
/// further Explorer arguments. Win32 cannot address such a name anyway.
fn explorer_select_arg(path: &str) -> Option<String> {
    (!path.contains('"')).then(|| format!("/select,\"{path}\""))
}

/// `<Windows directory>\explorer.exe`, with the directory from the system
/// (not from an environment variable); `None` if it cannot be read. A full
/// path, so no `explorer.exe` in this program's own folder is started.
fn explorer_program() -> Option<std::path::PathBuf> {
    let mut buf = vec![0u16; MAX_PATH as usize + 1];
    for _ in 0..2 {
        // SAFETY: `buf` is a live, writable buffer whose length the binding passes.
        let n = unsafe { GetSystemWindowsDirectoryW(Some(&mut buf)) } as usize;
        if n == 0 {
            return None;
        }
        if n < buf.len() {
            return Some(std::path::PathBuf::from(OsString::from_wide(&buf[..n])).join("explorer.exe"));
        }
        // Too small: `n` is the size needed, including the NUL.
        buf = vec![0u16; n];
    }
    None
}

/// `path` in the form Explorer takes: without the `\\?\` prefix a path may
/// carry when it was given that way on the command line. Explorer selects
/// nothing for `\\?\C:\x`; it wants `C:\x`, and `\\server\share\x` for
/// `\\?\UNC\server\share\x`. Any other path is returned as it is.
fn explorer_path(path: &str) -> std::borrow::Cow<'_, str> {
    use std::borrow::Cow;
    let Some(rest) = path.strip_prefix(r"\\?\") else { return Cow::Borrowed(path) };
    match rest.get(..4) {
        Some(unc) if unc.eq_ignore_ascii_case(r"UNC\") => Cow::Owned(format!(r"\\{}", &rest[4..])),
        _ if rest.as_bytes().get(1) == Some(&b':') => Cow::Borrowed(rest),
        // A volume name (`\\?\Volume{...}\`) has no other spelling.
        _ => Cow::Borrowed(path),
    }
}

/// Opens an Explorer window with `path` selected. Starts nothing if `path`
/// contains a `"` or the Windows directory cannot be read.
pub fn reveal_in_explorer(path: &str) {
    let path = explorer_path(path);
    let (Some(arg), Some(program)) = (explorer_select_arg(&path), explorer_program()) else { return };
    let _ = std::process::Command::new(program).raw_arg(arg).spawn();
}

/// Converts a FILETIME (100 ns since 1601, UTC) to a local date; `None` if `ft <= 0`.
pub fn filetime_to_local_ymd(ft: i64) -> Option<(u16, u8, u8)> {
    if ft <= 0 {
        return None;
    }
    let ft = FILETIME { dwLowDateTime: ft as u32, dwHighDateTime: (ft >> 32) as u32 };
    let mut utc = SYSTEMTIME::default();
    let mut local = SYSTEMTIME::default();
    // SAFETY: input and output point to live, properly typed structs.
    unsafe { FileTimeToSystemTime(&ft, &mut utc) }.ok()?;
    // SAFETY: `None` selects the current time zone; both pointers point to live structs.
    unsafe { SystemTimeToTzSpecificLocalTime(None, &utc, &mut local) }.ok()?;
    Some((local.wYear, local.wMonth as u8, local.wDay as u8))
}

/// Working set of this process in MiB: (current, peak). `(0, 0)` if unknown.
pub fn memory_mb() -> (u64, u64) {
    let mut pmc = PROCESS_MEMORY_COUNTERS::default();
    let cb = size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
    // SAFETY: the pseudo handle needs no closing; `pmc` is live and `cb` is its exact size.
    if unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut pmc, cb) }.is_err() {
        return (0, 0);
    }
    ((pmc.WorkingSetSize >> 20) as u64, (pmc.PeakWorkingSetSize >> 20) as u64)
}

/// A volume opened for reading only. Other programs may keep reading,
/// writing and deleting on it.
pub struct RawVolume {
    file: File,
    sector_size: usize,
}

/// Where to continue after `done` bytes of a read were delivered: the last
/// sector boundary that was reached completely.
fn resume_point(done: usize, sector: usize) -> usize {
    // Without a sector size there is no boundary to go back to.
    done.checked_div(sector).map_or(done, |sectors| sectors * sector)
}

impl RawVolume {
    /// Opens `\\.\X:` with read access only. Needs admin rights.
    pub fn open(letter: char) -> std::io::Result<Self> {
        if !letter.is_ascii_alphabetic() {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "Not a drive letter"));
        }
        let path = format!("\\\\.\\{}:", letter.to_ascii_uppercase());
        let file = std::fs::OpenOptions::new()
            .read(true)
            .share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE).0)
            .open(path)?;
        Ok(RawVolume { file, sector_size: 512 })
    }
}

impl VolumeReader for RawVolume {
    fn set_sector_size(&mut self, bytes: u32) {
        self.sector_size = bytes as usize;
    }

    /// `offset` and `buf.len()` are sector-aligned (the scanner guarantees it).
    /// After a short read the next read starts on a sector boundary again.
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        let file = &self.file;
        read_fully(|part, at| file.seek_read(part, at), offset, buf, self.sector_size)
    }
}

/// Fills `buf` from `offset` with as many calls of `read` as it takes; `read`
/// gets the part still to fill and where it starts, and returns how many bytes
/// it delivered. After a short read the next one starts on the last `sector`
/// boundary that was reached completely.
fn read_fully(
    mut read: impl FnMut(&mut [u8], u64) -> std::io::Result<usize>,
    offset: u64,
    buf: &mut [u8],
    sector: usize,
) -> std::io::Result<()> {
    let mut pos = 0usize;
    while pos < buf.len() {
        let at = offset
            .checked_add(pos as u64)
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "Offset too large"))?;
        match read(&mut buf[pos..], at) {
            Ok(0) => {
                return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "Read past the end of the volume"));
            }
            Ok(n) => {
                let done = pos + n;
                let next = if done >= buf.len() { done } else { resume_point(done, sector) };
                if next <= pos {
                    // Less than one sector arrived; reading on would never end.
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "The volume returned less than one sector",
                    ));
                }
                pos = next;
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elevated_copy_gets_only_a_local_fixed_working_dir() {
        use std::path::Path;
        use windows::Win32::System::WindowsProgramming::{DRIVE_CDROM, DRIVE_REMOTE, DRIVE_UNKNOWN};
        let fixed = |_| DRIVE_FIXED;
        assert_eq!(elevation_dir(Path::new(r"C:\Users\x"), fixed), Some(Path::new(r"C:\Users\x")));
        assert_eq!(elevation_dir(Path::new(r"\\?\D:\work"), fixed), Some(Path::new(r"\\?\D:\work")));
        for other in [DRIVE_REMOTE, DRIVE_REMOVABLE, DRIVE_CDROM, DRIVE_UNKNOWN] {
            assert_eq!(elevation_dir(Path::new(r"Z:\share"), |_| other), None, "{other}");
        }
        // A UNC path has no drive letter to ask about.
        for unc in [r"\\nas\share\x", r"\\?\UNC\nas\share"] {
            assert_eq!(elevation_dir(Path::new(unc), fixed), None, "{unc}");
        }
        // The letter of the path is the one asked about.
        let only_e_fixed = |l| if l == 'E' { DRIVE_FIXED } else { DRIVE_REMOTE };
        assert_eq!(elevation_dir(Path::new(r"E:\x"), only_e_fixed), Some(Path::new(r"E:\x")));
    }

    #[test]
    fn explorer_gets_the_path_only_when_it_cannot_break_out_of_the_quotes() {
        // A `"` would end the quoted argument; nothing is started.
        assert_eq!(explorer_select_arg(r#"E:\odd"name.txt"#), None);
        assert_eq!(explorer_select_arg(r#"E:\x" /root,C:\"#), None);
        for (path, arg) in [
            (r"C:\Users\x\file.txt", r#"/select,"C:\Users\x\file.txt""#),
            (r"C:\Program Files\a b.txt", r#"/select,"C:\Program Files\a b.txt""#),
            (r"D:\one,two\x,y.txt", r#"/select,"D:\one,two\x,y.txt""#),
            (r"D:\Tom & Jerry\a&b.txt", r#"/select,"D:\Tom & Jerry\a&b.txt""#),
            (r"\\nas\share\d", r#"/select,"\\nas\share\d""#),
        ] {
            assert_eq!(explorer_select_arg(path).as_deref(), Some(arg), "{path}");
        }
    }

    #[test]
    fn explorer_gets_paths_without_the_verbatim_prefix() {
        for (path, shown) in [
            (r"\\?\C:\Data\x.txt", r"C:\Data\x.txt"),
            (r"\\?\c:\", r"c:\"),
            (r"\\?\UNC\nas\share\d", r"\\nas\share\d"),
            (r"\\?\unc\nas\share", r"\\nas\share"),
            // Nothing to take off.
            (r"C:\Data\x.txt", r"C:\Data\x.txt"),
            (r"\\nas\share\d", r"\\nas\share\d"),
            (r"\\?\Volume{0000}\x", r"\\?\Volume{0000}\x"),
            (r"\\?\", r"\\?\"),
            ("", ""),
        ] {
            assert_eq!(explorer_path(path), shown, "{path}");
        }
    }

    #[test]
    fn explorer_is_started_from_the_windows_directory() {
        let program = explorer_program().expect("Windows directory");
        assert!(program.is_absolute(), "{}", program.display());
        assert!(program.ends_with("explorer.exe"), "{}", program.display());
        assert!(program.is_file(), "{}", program.display());
    }

    #[test]
    fn system_drive_is_listed_as_ntfs() {
        assert!(list_volumes().iter().any(|v| v.letter == 'C' && v.fs_name == "NTFS" && v.total > v.free));
    }

    #[test]
    fn subst_drives_are_told_from_volumes_by_their_device() {
        assert!(is_subst_target(r"\??\C:\Data"));
        assert!(is_subst_target(r"\??\UNC\nas\share"));
        assert!(!is_subst_target(r"\Device\HarddiskVolume3"));
        assert!(!is_subst_target(r"\Device\LanmanRedirector\;Z:0\nas\share"));
        assert!(!is_subst_target(""));
        // The system drive is a volume; a letter without a drive and no letter at all are neither.
        assert!(!is_subst_drive('C'));
        assert!(!is_subst_drive('c'));
        assert!(!is_subst_drive('1'));
    }

    #[test]
    fn resume_point_stays_on_a_sector_boundary() {
        assert_eq!(resume_point(1000, 512), 512);
        assert_eq!(resume_point(1024, 512), 1024);
        assert_eq!(resume_point(0, 4096), 0);
        assert_eq!(resume_point(300, 0), 300);
    }

    /// A volume of `len` bytes whose byte at offset `i` is `i % 251`. Each read
    /// delivers at most the next length from `chunks` (then everything asked
    /// for) and is recorded as (offset, length asked for).
    struct Chunked {
        len: u64,
        chunks: Vec<usize>,
        reads: Vec<(u64, usize)>,
    }

    impl Chunked {
        fn new(len: u64, chunks: &[usize]) -> Self {
            Chunked { len, chunks: chunks.iter().rev().copied().collect(), reads: Vec::new() }
        }

        fn read(&mut self, part: &mut [u8], at: u64) -> std::io::Result<usize> {
            self.reads.push((at, part.len()));
            let left = self.len.saturating_sub(at) as usize;
            let n = self.chunks.pop().unwrap_or(usize::MAX).min(part.len()).min(left);
            for (i, byte) in part[..n].iter_mut().enumerate() {
                *byte = ((at + i as u64) % 251) as u8;
            }
            Ok(n)
        }
    }

    fn expected(offset: u64, len: usize) -> Vec<u8> {
        (0..len as u64).map(|i| ((offset + i) % 251) as u8).collect()
    }

    #[test]
    fn short_reads_resume_on_a_sector_boundary() {
        // 700 bytes arrive, then 900, then the rest: the reads restart at 512 and 1024.
        let mut volume = Chunked::new(1 << 20, &[700, 900]);
        let mut buf = vec![0u8; 2048];
        read_fully(|part, at| volume.read(part, at), 4096, &mut buf, 512).unwrap();
        assert_eq!(buf, expected(4096, 2048));
        assert_eq!(volume.reads, [(4096, 2048), (4096 + 512, 1536), (4096 + 1024, 1024)]);
        // A read that delivers everything is the only one.
        let mut volume = Chunked::new(1 << 20, &[]);
        read_fully(|part, at| volume.read(part, at), 0, &mut buf, 512).unwrap();
        assert_eq!((buf.clone(), volume.reads.len()), (expected(0, 2048), 1));
        // What arrived beyond the last full sector is read again.
        let mut volume = Chunked::new(1 << 20, &[1800]);
        read_fully(|part, at| volume.read(part, at), 0, &mut buf, 512).unwrap();
        assert_eq!(buf, expected(0, 2048));
        assert_eq!(volume.reads, [(0, 2048), (1536, 512)]);
        // Sectors of 4096 bytes: 5000 bytes count as one sector.
        let mut volume = Chunked::new(1 << 20, &[5000]);
        let mut buf = vec![0u8; 8192];
        read_fully(|part, at| volume.read(part, at), 8192, &mut buf, 4096).unwrap();
        assert_eq!(buf, expected(8192, 8192));
        assert_eq!(volume.reads, [(8192, 8192), (8192 + 4096, 4096)]);
    }

    #[test]
    fn reads_that_cannot_finish_end_with_an_error() {
        let text = |r: std::io::Result<()>| r.unwrap_err().to_string();
        let mut buf = vec![0u8; 2048];
        // Less than one sector, again and again: reading on would never end.
        let mut volume = Chunked::new(1 << 20, &[100]);
        let err = read_fully(|part, at| volume.read(part, at), 0, &mut buf, 512);
        assert_eq!(text(err), "The volume returned less than one sector");
        assert_eq!(volume.reads.len(), 1);
        // The volume ends inside the buffer.
        let mut volume = Chunked::new(1024, &[]);
        let err = read_fully(|part, at| volume.read(part, at), 0, &mut buf, 512);
        assert_eq!(text(err), "Read past the end of the volume");
        // Less than one sector after a full one: the same, once the first sector is in.
        let mut volume = Chunked::new(1 << 20, &[512, 100]);
        let err = read_fully(|part, at| volume.read(part, at), 0, &mut buf, 512);
        assert_eq!(text(err), "The volume returned less than one sector");
        assert_eq!(volume.reads, [(0, 2048), (512, 1536)]);
        // The offset of the next read would not fit into 64 bits.
        let err = read_fully(|part, _| Ok(part.len().min(512)), u64::MAX - 600, &mut buf, 512);
        assert_eq!(text(err), "Offset too large");
        // Any other error is passed on; an interrupted read is tried again.
        let mut calls = 0;
        let err = read_fully(
            |_, _| {
                calls += 1;
                Err(std::io::Error::from(if calls == 1 {
                    std::io::ErrorKind::Interrupted
                } else {
                    std::io::ErrorKind::PermissionDenied
                }))
            },
            0,
            &mut buf,
            512,
        );
        assert_eq!((err.unwrap_err().kind(), calls), (std::io::ErrorKind::PermissionDenied, 2));
        // Nothing to read: no call at all.
        read_fully(|_, _| panic!("read for an empty buffer"), 0, &mut [], 512).unwrap();
    }

    #[test]
    fn long_path_name_of_a_temp_folder() {
        let tmp = tempfile::Builder::new().prefix("Platzhirsch Long Folder Name ").tempdir().unwrap();
        let name = tmp.path().file_name().unwrap().to_owned();
        let long = long_path_name(tmp.path()).unwrap();
        assert_eq!(long.file_name().unwrap(), name);
        assert!(long_path_name(&tmp.path().join("does not exist")).is_err());
    }

    #[test]
    fn a_size_is_cut_to_the_limit_in_each_direction() {
        assert_eq!(limited_size(900, 600, 16_384), (900, 600));
        assert_eq!(limited_size(16_384, 16_384, 16_384), (16_384, 16_384));
        assert_eq!(limited_size(900, 16_385, 16_384), (900, 16_384));
        assert_eq!(limited_size(20_000, 600, 16_384), (16_384, 600));
        assert_eq!(limited_size(10_686_500, i32::MAX, 16_384), (16_384, 16_384));
        // Sizes Windows would not accept anyway pass unchanged.
        assert_eq!(limited_size(0, -5, 16_384), (0, -5));
    }

    /// A window that is never shown, for the tests of the size limit. It has
    /// no frame: for a framed window Windows itself cuts a new size to the
    /// screen, which the window library of the real window switches off.
    struct Hidden(windows::Win32::Foundation::HWND);

    impl Hidden {
        fn new() -> Self {
            use windows::Win32::UI::WindowsAndMessaging::{CreateWindowExW, WINDOW_EX_STYLE, WS_POPUP};
            // SAFETY: "STATIC" is a system window class; no parent, menu or creation data is passed.
            let hwnd = unsafe {
                CreateWindowExW(WINDOW_EX_STYLE(0), w!("STATIC"), PCWSTR::null(), WS_POPUP, 0, 0, 300, 200, None, None, None, None)
            };
            Hidden(hwnd.expect("a hidden window"))
        }

        /// Asks for the outer size `cx` x `cy` the way another program would, and returns the size the window has then.
        fn resize(&self, cx: i32, cy: i32) -> (i32, i32) {
            use windows::Win32::Foundation::RECT;
            use windows::Win32::UI::WindowsAndMessaging::{GetWindowRect, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER, SetWindowPos};
            let mut rect = RECT::default();
            // SAFETY: the window is alive and `rect` is valid for the call.
            unsafe {
                SetWindowPos(self.0, None, 0, 0, cx, cy, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE).expect("SetWindowPos");
                GetWindowRect(self.0, &mut rect).expect("GetWindowRect");
            }
            (rect.right - rect.left, rect.bottom - rect.top)
        }
    }

    impl Drop for Hidden {
        fn drop(&mut self) {
            // SAFETY: the window was created by this thread and is destroyed once.
            let _ = unsafe { windows::Win32::UI::WindowsAndMessaging::DestroyWindow(self.0) };
        }
    }

    #[test]
    fn a_limited_window_cannot_be_made_larger_by_another_program() {
        let window = Hidden::new();
        // Without a limit Windows takes any size.
        assert_eq!(window.resize(900, 20_000), (900, 20_000));
        assert!(limit_window_size(window.0.0 as isize, 16_384));
        assert_eq!(window.resize(900, 20_000), (900, 16_384));
        assert_eq!(window.resize(20_000, 600), (16_384, 600));
        assert_eq!(window.resize(10_686_500, 10_686_500), (16_384, 16_384));
        // Sizes within the limit are untouched, the limit itself included.
        assert_eq!(window.resize(16_384, 16_384), (16_384, 16_384));
        assert_eq!(window.resize(900, 600), (900, 600));
        // A move without a new size is not a resize.
        use windows::Win32::UI::WindowsAndMessaging::{SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SetWindowPos};
        // SAFETY: the window is alive.
        unsafe { SetWindowPos(window.0, None, 40, 50, 99_999, 99_999, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE).expect("move") };
        assert_eq!(window.resize(900, 600), (900, 600));
    }

    #[test]
    fn a_limit_that_is_not_a_size_is_not_installed() {
        let window = Hidden::new();
        assert!(!limit_window_size(window.0.0 as isize, 0));
        assert!(!limit_window_size(window.0.0 as isize, -1));
        assert_eq!(window.resize(900, 20_000), (900, 20_000));
        // No such window.
        assert!(!limit_window_size(0, 16_384));
    }
}
