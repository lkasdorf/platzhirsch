# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- Sizes just below a unit boundary are shown in the next unit, so 1,048,575 Bytes reads
  `1.0 MB` instead of `1,024.0 KB`.
- Read errors are shown in English, with the cause and the Windows error code, instead of
  in the language of the system.
- Drives whose Master File Table extents are recorded out of order, or with sectors larger
  than 4 KB, are no longer rejected (tested on synthetic images).
- Stale records beyond the initialized part of the Master File Table no longer appear as
  entries (tested on synthetic images).
- Entries whose reparse data is stored outside their file record can now be marked as
  links (tested on synthetic images).
- Running out of memory while reading a very large Master File Table is reported as an
  error instead of ending the program.
- A link to a folder is scanned as the folder it points to. This now also holds for a
  link that was created as a file-type link (not yet run on a machine that can create
  such links).
- Device paths such as `\\.\C:` are refused with the message "Device paths are not
  supported" instead of an unclear read error.
- The check for online-only cloud folders now looks at every folder on the path of the
  scan target, not only at the chosen folder.
- Names of deeply nested entries stay readable in the folder table: the indentation takes
  at most 40 % of the Name column. Hovering a name shows the entry's full path.
- A double-click on the expand arrow of a folder expands or collapses it once instead of
  twice.
- In very deep folder trees (more than 65,535 levels), the left arrow key now selects the
  real parent folder.

## [0.1.0] - 2026-11-22

First release.

### Added

- Fast scan for NTFS drives: reads the Master File Table directly, which needs
  administrator rights. Measured on an NVMe drive with 1.87 million entries: 2.1 s and
  3.4 s in two runs, 0.36 % deviation from the drive's used space.
- Fallback scan without administrator rights: parallel folder walk for single folders,
  network paths and non-NTFS drives. "Allocated" is an approximation in this mode.
- In-memory folder tree with totals per folder for size, allocated space, file count,
  folder count and latest modification.
- Flags instead of silently wrong numbers: unreadable folders and online-only cloud
  folders are marked in the table and counted in the status bar. Links (junctions and
  symbolic links) are marked and never followed. A cloud folder that is not on this
  machine is refused as a scan target with a message, and such folders inside a scan are
  not entered.
- Squarified treemap layout as the basis for the area view.
- Formatting of sizes, counts, percentages and dates.
- Diagnostic program `bench` that measures scan duration, memory use and the deviation
  from the drive's used space. It is not shipped.
- Guard tests that fail if a call from a list of write, delete and network functions
  appears in the source, a networking feature of the Windows bindings is enabled, or a
  known network library appears among the dependencies.
- A single executable with the C runtime linked statically: no installer and no
  Visual C++ redistributable needed.
- Main window with a drive list, folder picker, scan and cancel buttons, a switch between
  allocated space and size, and a status bar with free space, file count, scan method and
  duration. The scan runs in the background and shows its progress.
- Choice of scan method per drive: without administrator rights the app offers a restart
  as administrator or the slow folder scan. If the fast scan fails, the error is shown and
  the slow scan is offered; the app never switches silently. `--scan X:` or
  `--scan <folder>` starts a scan right away. A drive chosen through "Folder…" is treated
  like a drive chosen from the list.
- If the window cannot be created, for example without OpenGL 2, a message box says why.
- Folder table after a scan: name, allocated space, size, files, folders, share of the
  parent folder and last modification, with a bar behind each name for its share of the
  parent. Click a column header to sort, click again to reverse. Folders expand and
  collapse by arrow, double-click or arrow keys; only the visible rows are drawn. Marks
  for unreadable, online-only and link entries. Right-click offers "Show in Explorer" and
  "Copy path".
- Treemap below the table: shows the folder selected in the table as nested tiles,
  coloured by level; if a file is selected, it shows that file's folder. Hovering shows
  a tile's path and its allocated space or size, whichever is chosen; clicking a tile
  selects the entry in the table and scrolls to it. The divider to the table can be
  dragged, and the "Treemap" switch hides the treemap. The layout is recomputed only when
  the folder, the metric or the size changes; tiles thinner than one pixel are left out.
- The table shows the sort triangle in exactly one column: the one whose header was
  clicked last, or the column of the chosen metric after a scan.
- Continuous integration on GitHub Actions: tests and a release build on Windows for
  every push to `main` and `v1` and for pull requests against `main`. The built
  `platzhirsch.exe` is kept as a build artifact.
- Release workflow: pushing a tag `vX.Y.Z` builds on Windows and attaches
  `platzhirsch.exe` with its SHA-256 checksum to the GitHub release.

[Unreleased]: https://github.com/lkasdorf/platzhirsch/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/lkasdorf/platzhirsch/releases/tag/v0.1.0
