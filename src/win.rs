//! Windows bindings: volumes, elevation, time, memory and a read-only raw volume.
use std::fs::File;
use std::os::windows::fs::{FileExt, OpenOptionsExt};
use std::os::windows::process::CommandExt;

use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, SYSTEMTIME};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::Storage::FileSystem::{
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives,
    GetVolumeInformationW,
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
    pub removable: bool,
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
    let mut fs_buf = [0u16; 64];
    // SAFETY: `root` is NUL-terminated and `fs_buf` is a writable buffer whose length the binding passes.
    unsafe { GetVolumeInformationW(&root, None, None, None, None, Some(&mut fs_buf)) }.ok()?;
    let fs_len = fs_buf.iter().position(|&c| c == 0).unwrap_or(fs_buf.len());
    let (mut total, mut free) = (0u64, 0u64);
    // SAFETY: `root` is NUL-terminated; both out pointers point to live `u64`s.
    unsafe { GetDiskFreeSpaceExW(&root, None, Some(&mut total), Some(&mut free)) }.ok()?;
    // SAFETY: `root` is a valid, NUL-terminated wide string.
    let kind = unsafe { GetDriveTypeW(&root) };
    Some(VolumeInfo {
        letter: letter.to_ascii_uppercase(),
        fs_name: String::from_utf16_lossy(&fs_buf[..fs_len]),
        total,
        free,
        removable: kind == DRIVE_REMOVABLE,
    })
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

/// Starts this program again with admin rights (UAC prompt). `Err` if declined or failed.
pub fn relaunch_elevated(args: &[String]) -> std::io::Result<()> {
    let exe = HSTRING::from(std::env::current_exe()?.as_os_str());
    let params = HSTRING::from(args.iter().map(|a| quote_arg(a)).collect::<Vec<_>>().join(" "));
    // SAFETY: all strings are NUL-terminated and outlive the call; no window handle is passed.
    let result = unsafe { ShellExecuteW(None, w!("runas"), &exe, &params, PCWSTR::null(), SW_SHOWNORMAL) };
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
        Ok(RawVolume { file })
    }
}

impl VolumeReader for RawVolume {
    /// `offset` and `buf.len()` are sector-aligned (the scanner guarantees it).
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
                Ok(n) => pos += n,
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
    fn system_drive_is_listed_as_ntfs() {
        assert!(list_volumes().iter().any(|v| v.letter == 'C' && v.fs_name == "NTFS" && v.total > v.free));
    }
}
