# Platzhirsch

A fast disk space analyzer for Windows. It shows which folders use how much space, as a
folder tree with sizes and as a treemap.

Platzhirsch reads the NTFS Master File Table directly instead of asking Windows for every
folder one by one. On the development machine, a full scan of an NVMe drive with
1.87 million entries takes 2.1 seconds.

> **Status:** in development. Version 0.1.0 is being built on the `v1` branch and has not
> been released yet. The scanners are finished and measured, and the user interface for
> 0.1.0 is complete: a folder table with a treemap below it. See the
> [changelog](CHANGELOG.md) for what exists today.

## What it does

- **Fast scan** of a whole NTFS drive through the Master File Table. Needs administrator
  rights.
- **Fallback scan** without administrator rights: a parallel folder walk for single
  folders, network paths and drives that are not NTFS.
- **Size and allocated space** per folder, plus file count, folder count and the latest
  modification in the subtree.
- **Honest numbers:** folders that could not be read, links (junctions and symbolic
  links) and online-only cloud folders are marked and counted, never shown silently as
  0 bytes. Links are never followed.
- **Folder table** with a share bar per entry, sorting by any column, keyboard
  navigation, and "Show in Explorer" / "Copy path" on right-click. Only the visible rows
  are drawn.
- **Treemap** of the folder selected in the table, coloured by level. Hover for path and
  size, click a tile to select it in the table. The divider can be dragged and the
  treemap can be hidden.
- **A single .exe**, no installer, no runtime to install.

## What it never does

- It never writes to the drive it scans. Volumes and folders are opened read-only, and
  file contents are never opened.
- It has no network access: no telemetry, no update check, no advertising.

Two guard tests in `tests/anti_goals.rs` fail the build if write calls or network
libraries appear in the code.

## Measured

Taken on 2026-11-21 with the diagnostic program `bench`:

| | Result |
|---|---|
| Fast scan, NVMe drive, 1.87 million entries | 2.1 s |
| Deviation of "allocated" from the drive's used space | 0.36 % |
| Memory after the scan | 147 MB |
| Peak memory during the scan | 246 MB |
| Fallback scan of the same drive, without administrator rights | 42 s |

For a folder with 141,418 files, the fast scan and the fallback scan report the same
file count and the same size.

## Build

You need Windows 10 or 11 (x64) and a stable Rust toolchain with the MSVC target.

```powershell
cargo build --release
```

The program is `target\release\platzhirsch.exe`.

```powershell
cargo test
```

The tests run without administrator rights.

## Use

Start `platzhirsch.exe`, choose a drive or a folder and press "Scan". For a whole NTFS
drive the app offers to restart as administrator for the fast scan, or to scan slowly
without it.

A scan can also be started from the command line:

```powershell
platzhirsch.exe --scan C:
```

## How it works

- `src/scan/mft.rs` locates the Master File Table on the volume, reads it in large
  chunks and parses the file records in parallel.
- `src/scan/walk.rs` is the fallback: it lists folders in parallel with ordinary Win32
  calls.
- Both fill the same flat in-memory tree in `src/model.rs`, which stores about 53 bytes
  per entry plus the name.
- `src/treemap.rs` computes the squarified treemap layout.
- `src/ui/` is the egui user interface.

The design document and the implementation plan are in
[`docs/superpowers/`](docs/superpowers/).

## Known limits

- The fast scan works on NTFS only. Other file systems use the fallback scan.
- A file with several hard links is counted once and shown under one of its names, so
  folder totals inside `Windows` can differ from tools that count every name.
- In the fallback scan, "allocated" is an approximation and hard links are counted once
  per name.

## License

[MIT](LICENSE)
