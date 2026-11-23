# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- The source is formatted with rustfmt. `rustfmt.toml` holds the settings: lines of up to
  120 columns, as the code was written before. The program itself is unchanged.
  Continuous integration fails if `cargo fmt` would change a file.

### Fixed

- Folder table: in a very long list the rows were drawn some points off, 5 points at the
  end of two million expanded rows and more in longer lists, so the last row was cut off.
  Rows are now placed exactly at any length; scroll bar, wheel and arrow keys work as in
  a short list.
- Continuous integration: a run that GitHub starts late for a pull request that is already
  merged no longer cancels the run on `main`.

## [0.3.0] - 2026-11-23

### Added

- Treemap: where a folder shows only some of its entries, the space of the others is drawn
  in the darker shade that folders too small to subdivide already have. The tooltip of the
  folder says how many entries of which size are not drawn. Before, that space had the
  folder's plain colour and nothing said what it was.

### Changed

- Treemap: a treemap has at most 300,000 tiles. The largest folders are subdivided first
  and in a folder the largest entries are drawn first; what the limit leaves out is marked
  as not drawn. A drive with 1.9 million entries needs about 169,000 tiles on a panel of
  3840 x 1400 points, so this bounds only trees with millions of entries of similar size.
- A drive made with `subst` is scanned with the folder scan right away. It was offered the
  fast scan, which it cannot have: the restart with administrator rights ended in an error.
- Continuous integration also compiles the project with Rust 1.95, the minimum version it
  declares.

### Fixed

- The program no longer ends with an access violation in the graphics driver when another
  program makes its window wider or higher than the driver can draw to (16,384 pixels with
  the Intel driver this was seen with). The window now stays within the size the driver
  reports.
- "Show in Explorer" selects the entry also when the scanned folder was given with the
  `\\?\` prefix (`platzhirsch --scan \\?\C:\Data`).
- Fast scan: a junction or symbolic link is marked as a link also when its name and its
  reparse data lie in different file records (tested on synthetic images).
- The diagnostic program `bench` reports an output file it cannot write in English, like
  its other errors, instead of in the language of the system.

## [0.2.1] - 2026-11-23

### Changed

- The Name column of the folder table follows the window's width: it takes the space a
  wider window adds and gives it back when the window gets narrower, down to its minimum
  width. Until now it kept the width it had when the window opened, which left empty space
  beside the columns after maximizing. A width set by hand follows the window in the same
  way.

### Fixed

- The folder table's columns on the right were cut off when the window was narrower than
  the columns. The table now scrolls sideways, with a horizontal scroll bar at its lower
  edge.
- The focus outline of the selected table row reached beyond the last column; it now ends
  where the row's highlight ends.

## [0.2.0] - 2026-11-22

### Added

- After Cancel the top bar shows `Cancelling…` and the Cancel button stays disabled until
  the scan has stopped, which can take a moment on a network share.
- When the restart with administrator rights is declined or fails, the elevation notice
  says "Administrator rights were not granted."
- A click on the name strip at the top of the treemap goes up to the parent of the folder
  shown; the strip's tooltip says "Click to go up". At the drive or folder scanned there is
  no parent, and the click selects that entry in the table.
- Screen readers can read the window: every control has a name, each table row is read as
  one item with its name, sizes, share and whether a folder is expanded, and the treemap
  is named after the folder it shows.
- New keys in the folder table: Enter expands or collapses the selected folder, the Menu
  key or Shift+F10 opens its context menu, and Ctrl+C copies its path. Tab reaches the
  table and the column headers, which sort with Space or Enter.
- `platzhirsch.exe` carries the app's icon and version information, so Explorer shows the
  pie icon and the Details tab lists product name, description, version and copyright.

### Changed

- The notice that the fast scan needs administrator rights no longer hides a scan result
  that is on screen; the result stays visible below it.
- An unexpected fault inside a scan now ends only that scan, with the message "The scan
  ended unexpectedly.", instead of closing the program. After a fast scan that ended this
  way, the slow scan is offered.
- The copy started with administrator rights uses the same working folder as the first
  one when that folder is on a local fixed drive.
- Folders in the treemap whose content is too small to draw are shown in a darker shade;
  their tooltip says how many entries are inside.
- Hovering in the treemap lightens only what is under the pointer: over a folder its name
  strip, over a file its tile.
- The treemap is drawn as one prepared shape instead of tile by tile in every frame. While
  it is resized, the last layout is stretched to fit and laid out anew only when the size
  has stayed the same for 150 ms.
- A drive given in the long form `\\?\C:\` is scanned as the whole drive, like `C:`.
- Continuous integration runs for pushes to `main` and for pull requests against `main`.
  The actions it uses are pinned to exact commits.
- The release workflow refuses a tag that is not on `main`, a tag that does not match the
  package version, and a version without a section in this changelog. The release notes
  are taken from that section.

### Fixed

- "Copy path" now copies to the Windows clipboard; in 0.1.0 the path never left the
  program.
- Tab no longer stops at every table cell; the table is one stop, and while it has the
  focus the arrow keys move the selection instead of the focus. While a context menu is
  open the arrow keys move within the menu only.
- The drive list is read again each time it is opened, so a drive plugged in after the
  start appears without a restart.
- The window no longer redraws continuously while a scan runs; the progress indicator is
  a text that changes four times a second.
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
- If the memory for the tables of a very large Master File Table cannot be reserved, the
  scan ends with an error instead of ending the program.
- A Master File Table that is recorded as lying outside the drive is rejected as damaged
  instead of being read.
- A junction or directory symbolic link chosen as the scan target is scanned as the folder
  it points to, also when the link sits higher up in the path.
- Device paths such as `\\.\C:` are refused with the message "Device paths are not
  supported" instead of an unclear read error.
- The check for online-only cloud folders now looks at every folder on the path of the
  scan target, not only at the chosen folder. For a `subst` drive or a mapped network
  drive it also looks at the folder the drive points to.
- "Show in Explorer" starts Explorer from the Windows folder only, and starts nothing for
  a name that contains a double quote.
- A folder scan whose file names would need more than 4 GiB of memory stops with an error.
- Names of deeply nested entries stay readable in the folder table: the indentation takes
  at most 40 % of the Name column. Hovering a name shows the entry's full path.
- A double-click on the expand arrow of a folder expands or collapses it once instead of
  twice.
- In very deep folder trees (more than 65,535 levels), the left arrow key now selects the
  real parent folder.
- An open context menu closes when the rows of the table change, so it can no longer act
  on a row that has moved.

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

[Unreleased]: https://github.com/lkasdorf/platzhirsch/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/lkasdorf/platzhirsch/compare/v0.2.1...v0.3.0
[0.2.1]: https://github.com/lkasdorf/platzhirsch/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/lkasdorf/platzhirsch/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/lkasdorf/platzhirsch/releases/tag/v0.1.0
