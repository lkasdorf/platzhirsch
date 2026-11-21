# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Fast scan for NTFS drives: reads the Master File Table directly, which needs
  administrator rights. Measured on 2026-11-21: drive C: with 1.87 million entries in
  2.1 s, 0.36 % deviation from the drive's used space.
- Fallback scan without administrator rights: parallel folder walk for single folders,
  network paths and non-NTFS drives. "Allocated" is an approximation in this mode.
- In-memory folder tree with totals per folder for size, allocated space, file count,
  folder count and latest modification.
- Flags instead of silently wrong numbers: unreadable folders, links (junctions and
  symbolic links) and online-only cloud folders are marked and counted. Links are never
  followed.
- Squarified treemap layout as the basis for the area view.
- Formatting of sizes, counts, percentages and dates.
- Diagnostic program `bench` that measures scan duration, memory use and the deviation
  from the drive's used space. It is not shipped.
- Guard tests that forbid write access to scanned drives and network libraries in the
  code.
- Main window with a drive list, folder picker, scan and cancel buttons, a switch between
  allocated space and size, and a status bar with free space, file count, scan method and
  duration. The scan runs in the background and shows its progress.
- Choice of scan method per drive: without administrator rights the app offers a restart
  as administrator or the slow folder scan. If the fast scan fails, the error is shown and
  the slow scan is offered; the app never switches silently. `--scan X:` or
  `--scan <folder>` starts a scan right away.
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
