//! Windows bindings: volumes, elevation, time, memory and a read-only raw volume.
use std::ffi::OsString;
use std::fs::File;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::{FileExt, OpenOptionsExt};
use std::os::windows::process::CommandExt;

use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, MAX_PATH, SYSTEMTIME};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::Storage::FileSystem::{
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives,
    GetLongPathNameW, GetVolumeInformationW,
};
use windows::Win32::System::Diagnostics::Debug::{SEM_FAILCRITICALERRORS, SetThreadErrorMode, THREAD_ERROR_MODE};
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
use windows::Win32::System::WindowsProgramming::{DRIVE_FIXED, DRIVE_REMOVABLE};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW, SW_SHOWNORMAL};
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

/// Opens an Explorer window with `path` selected.
pub fn reveal_in_explorer(path: &str) {
    let _ = std::process::Command::new("explorer.exe").raw_arg(format!("/select,\"{path}\"")).spawn();
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
    if sector == 0 { done } else { done / sector * sector }
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
        let mut pos = 0usize;
        while pos < buf.len() {
            let at = offset
                .checked_add(pos as u64)
                .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "Offset too large"))?;
            match self.file.seek_read(&mut buf[pos..], at) {
                Ok(0) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "Read past the end of the volume",
                    ));
                }
                Ok(n) => {
                    let done = pos + n;
                    let next = if done >= buf.len() { done } else { resume_point(done, self.sector_size) };
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
    fn system_drive_is_listed_as_ntfs() {
        assert!(list_volumes().iter().any(|v| v.letter == 'C' && v.fs_name == "NTFS" && v.total > v.free));
    }

    #[test]
    fn resume_point_stays_on_a_sector_boundary() {
        assert_eq!(resume_point(1000, 512), 512);
        assert_eq!(resume_point(1024, 512), 1024);
        assert_eq!(resume_point(0, 4096), 0);
        assert_eq!(resume_point(300, 0), 300);
    }

    #[test]
    fn long_path_name_of_a_temp_folder() {
        let tmp = tempfile::Builder::new().prefix("Platzhirsch Long Folder Name ").tempdir().unwrap();
        let name = tmp.path().file_name().unwrap().to_owned();
        let long = long_path_name(tmp.path()).unwrap();
        assert_eq!(long.file_name().unwrap(), name);
        assert!(long_path_name(&tmp.path().join("does not exist")).is_err());
    }
}
