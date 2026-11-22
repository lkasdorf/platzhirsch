# Platzhirsch v0.2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Resolve the ten GitHub issues opened after the 0.1.0 reviews (#1 to #10) and release the result as version 0.2.0.

**Architecture:** No structural change. Each task takes one issue and fixes its items in the module that owns them; the scanners, the model and the pure UI logic stay separate and testable without a window.

**Tech Stack:** As in 0.1.0 (Rust 1.95+, `eframe`/`egui` 0.37, `windows`, `rayon`, `rfd`). New in this plan: the `accesskit` feature of `eframe` (Task 8), the build dependency `winresource` (Task 9), and optionally the dev dependency `egui_kittest` (Task 5).

**Spec:** `docs/superpowers/specs/2026-11-21-disk-space-analyzer-design.md`. The issue texts are at https://github.com/lkasdorf/platzhirsch/issues; each task below repeats its issue's items and adds the decision for every item that needed one.

## Global Constraints

- Platform: Windows 10/11 x64. Package `platzhirsch`. The app is English-only: UI texts, messages, comments, test names. Number format `1,683,667` and `763.8 GB`, dates `DD/MM/YYYY`, always through `src/format.rs`.
- **Never write to a scanned drive, never open file contents, no network access.** Volumes and folders are opened read-only.
- **No silent wrong numbers.** What cannot be read is marked and counted; the counters must not raise false alarms either.
- Code that reads bytes from disk never panics, never reads out of bounds and never loops forever.
- No recursion over tree depth. The tree can hold 2 million entries: nothing proportional to the entry count runs per frame on the UI thread.
- No name, logo or look of TreeSize.
- Behaviour that a task does not mention must not change. Existing tests keep their meaning; a changed expectation is named in the task report.
- Check every third-party API (`windows`, `egui`, `eframe`, `egui_extras`, `winresource`, `egui_kittest`) against the crate source under `~/.cargo/registry` or its documentation, not from memory. `unsafe` blocks stay small, each with a `// SAFETY:` line; handles are closed on all paths.
- `tests/anti_goals.rs` stays green. If a task needs a line that the guard flags, it says so in its report instead of weakening the guard; only Task 10 changes the guard.
- Every user-visible change gets an entry under `## [Unreleased]` in `CHANGELOG.md` (`### Added`, `### Changed` or `### Fixed`) in the same commit, and `README.md` is corrected where the change makes it wrong.
- Targets from the design still hold: fast scan of the reference drive at most 10 s, at most 400 MB after the scan and 1 GB at peak, executable at most 15 MB, allocated total within 2 % of the drive's used space.
- Work on branch `v0.2`. Implementers never push; stage files by name. Commit messages end with the Co-Authored-By line of the session that wrote the commit.

## Review Focus

Cases the issues imply but do not spell out. Each has a test in the named task.

1. **A drive with an MFT whose initialized size is smaller than its data size:** records beyond the initialized size must not become entries. Test in Task 3.
2. **A scan root given as a link, a relative path, or a bare drive letter:** the online-only check and the root name work on the resolved absolute path. Tests in Task 4.
3. **Collapse and expand of a folder with many children while a deeper row is selected:** the selection stays valid and the row list equals a full rebuild. Test in Task 5.
4. **Cancel pressed twice, or a result that arrives after Cancel:** one transition to idle, no stale result shown. Test in Task 6.
5. **A treemap of a folder whose children are all too small to draw:** one tile marked as elided, no division by zero, and a click still selects the folder. Test in Task 7.

## Files

```
src/format.rs              Task 1
src/scan/mod.rs            Tasks 1, 3, 4
src/win.rs                 Tasks 1, 2, 6
src/bin/bench.rs           Tasks 2, 6
src/scan/ntfs/boot.rs      Tasks 2, 3
src/scan/ntfs/record.rs    Task 3
src/scan/ntfs/testkit.rs   Task 3
src/scan/mft.rs            Tasks 2, 3
src/model.rs               Task 3 (tests only)
src/scan/walk.rs           Task 4
tests/walk_tempdir.rs      Task 4
src/ui/rows.rs             Task 5
src/ui/table.rs            Tasks 5, 8
src/ui/mod.rs              Tasks 5, 6, 7, 8
src/ui/bars.rs             Tasks 6, 8
src/treemap.rs             Task 7
src/ui/treemap_view.rs     Tasks 7, 8
Cargo.toml                 Tasks 5, 6, 8, 9
build.rs, assets/          Task 9
.github/workflows/*.yml    Task 9
tests/anti_goals.rs        Task 10
```

---

### Task 1: Issue #6, number formatting and error messages

**Files:** Modify `src/format.rs`, `src/scan/mod.rs`, `src/win.rs`, `src/scan/walk.rs`, `src/scan/mft.rs` (messages only)

- [ ] **`format_bytes` picks the unit after rounding.** A value that rounds to 1,024.0 of a unit moves to the next unit, except at PB.

```rust
assert_eq!(format_bytes(1_048_575), "1.0 MB");
assert_eq!(format_bytes(1_023), "1,023 Bytes");
assert_eq!(format_bytes(1_024 * 1_024 - 51), "1.0 MB");      // 1,023.9502 KB rounds up to 1,024.0
assert_eq!(format_bytes(1_024 * 1_024 - 52), "1,023.9 KB");  // 1,023.9492 KB rounds down
assert_eq!(format_bytes(u64::MAX), "16,384.0 PB");            // largest unit keeps the overflow
```

- [ ] **Error messages do not embed Windows' own message text**, which is in the system language. `ScanError::Io` displays as `Read error: {description} (OS error {code})`, where `{description}` is an English text chosen from `std::io::ErrorKind` (`NotFound` → `not found`, `PermissionDenied` → `access denied`, `NotADirectory` → `not a folder`, `UnexpectedEof` → `unexpected end of data`, `InvalidInput` → `invalid input`, anything else → `I/O error`). Without an OS error code the parenthesis is omitted. Messages the program itself builds (for example `This folder is online-only; …`) are shown as they are.
- [ ] **One capitalisation rule:** every message starts with a capital letter and has no trailing period unless it is more than one sentence. Fix the outliers (`src/win.rs`, `src/scan/walk.rs`, `src/scan/ntfs/mod.rs`). `bench` keys stay lower-case.
- [ ] **Clippy:** fix `manual_is_multiple_of` in `src/format.rs`; `cargo clippy --all-targets` reports nothing for this file.
- [ ] **Date test without a time-zone assumption:** `format_ymd` is tested as a pure function; the test of `format_date` accepts `20/11/2026`, `21/11/2026` or `22/11/2026` for the fixed instant and checks the `DD/MM/YYYY` shape.
- [ ] Tests first, then the change; full `cargo test` green; commit `fix: unit choice after rounding, English error texts`.

### Task 2: Issue #8, bench and Windows bindings

**Files:** Modify `src/bin/bench.rs`, `src/win.rs`, `src/scan/ntfs/boot.rs`, `src/scan/mft.rs`

- [ ] **`bench` does not print a fake perfect result.** When the volume information cannot be read after a scan it prints `volume_used: n/a` and `deviation_percent: n/a`.
- [ ] **A failed `--out` write fails the run:** message `error: could not write {path}: …` on stderr and exit code 1.
- [ ] **Drive roots in verbatim form** (`\\?\C:\`) are recognised as drive roots by `bench walk`. Use the shared `ScanTarget::from_path` after stripping the verbatim prefix; add that case to its tests.
- [ ] **`bench compare` resolves the folder path first** (`std::path::absolute`, then the long form of an 8.3 name via `GetLongPathNameW`), so short names are found. Test the helper with a temp folder whose long name is known.
- [ ] **`duration_s` includes opening the volume:** `scan_mft` measures from before `RawVolume::open`.
- [ ] **Short reads from the raw volume stay sector-aligned.** After a short read, `RawVolume::read_at` continues from the last sector boundary that was fully read. The sector size comes from the caller: add `RawVolume::set_sector_size(&mut self, bytes: u32)` called by the scanner once the boot sector is parsed (512 until then). Test the pure helper that computes the next offset and remaining length: `resume_point(done: usize, sector: usize) -> usize` with `resume_point(1000, 512) == 512`, `resume_point(1024, 512) == 1024`, `resume_point(0, 4096) == 0`.
- [ ] **File-system name buffer** is `MAX_PATH + 1` code units.
- [ ] **Unused fields:** remove `VolumeInfo.removable`. Use `BootSector.total_sectors`: the MFT scan rejects a boot sector whose MFT start lies outside the volume (`mft_lcn * cluster_size >= total_sectors * bytes_per_sector`) with `NtfsError::Malformed`; test with a fake volume.
- [ ] Full `cargo test` green; `cargo build --release`; unelevated `bench mft C:` still prints the access-denied error and exits 1; commit `fix: bench output and raw volume robustness`.

### Task 3: Issue #1, MFT scan robustness

**Files:** Modify `src/scan/ntfs/record.rs`, `src/scan/ntfs/testkit.rs`, `src/scan/mft.rs`, `src/scan/ntfs/boot.rs` (only if needed), `src/model.rs` (tests only)

- [ ] **Extension records are recognised by the unmasked reference.** `RecordInfo` gains `pub is_extension: bool`, true when the 64-bit base reference is non-zero; `base_record` stays the 48-bit record number. Extension records of `$MFT` (base record 0, sequence number 1) are then extension records like any other. Remove the workaround in `mft.rs` that reclassifies the records named in record 0's attribute list. The existing test `fragmented_mft_via_attr_list` keeps passing with `skipped == 0`.
- [ ] **`$MFT` extents may be spread over records in any order.** `locate_mft` collects every `(start_vcn, runs)` piece of the unnamed `$DATA` from record 0 and from the records its attribute list names, sorts them by start VCN, and only then checks that they are contiguous. Test: record X holds the pieces at VCN 4 and VCN 12, record Y holds VCN 8; the scan finds a file in each piece.
- [ ] **Initialized size bounds the MFT.** `RecordInfo` gains the initialized size of the unnamed non-resident `$DATA` (attribute offset `0x38`); `MftLayout.byte_len` is the smaller of data size and initialized size. Test (Review Focus 1): data size 64 records, initialized size 32 records, a stale record with a `FILE` signature at index 40 does not become an entry and is not counted as skipped.
- [ ] **A compressed or sparse attribute needs the long header.** If the compressed (`0x0001`) or sparse (`0x8000`) flag is set and the run-list offset is below `0x48`, the attribute is `Malformed`. Test.
- [ ] **Non-resident `$REPARSE_POINT`:** when the attribute is non-resident, take the reparse tag from the record's `$FILE_NAME` attribute instead: if the file-attribute flags at `$FILE_NAME` offset `0x38` contain `0x400` (reparse point), the tag is the `u32` at offset `0x3C`. Verify both offsets against the Linux `ntfs3` headers before coding and say so in the report; if they cannot be confirmed, leave such entries unmarked and report that. Test with a builder method for a non-resident reparse attribute.
- [ ] **A record caught in the middle of being allocated** (first sector still marked free) stays "empty, not counted". This is the accepted consequence of "a free record is never skipped". Add a comment at the decision point that says so and names the trade-off; no behaviour change.
- [ ] **Sectors larger than 4096 bytes:** read the boot sector with 4096 bytes and, if that read fails, once more with 65536 bytes. Test with a `VolumeReader` that rejects reads that are not multiples of 8192.
- [ ] **Allocation failures are errors, not aborts.** Reserve the slot table once with `try_reserve_exact(record_count)`; reserve the name buffer with `try_reserve` as it grows; on failure return `ScanError::Ntfs(NtfsError::Malformed("not enough memory for the file table"))`.
- [ ] **Tests that were missing:** a named resident stream leaves `size` alone; a compressed extent with a start VCN other than 0 adds nothing; a non-resident `$ATTRIBUTE_LIST` is read through its runs and cut to its length; in `model.rs`, a parent cycle yields a `path()` that ends (assert its exact value) and a child whose parent is a file is an orphan.
- [ ] Tests first where a test can fail first; full `cargo test` green; commit `fix: MFT scan handles unusual layouts`.

### Task 4: Issue #2, fallback scan edge cases

**Files:** Modify `src/scan/walk.rs`, `tests/walk_tempdir.rs`, `src/scan/mod.rs` (only for an error variant)

- [ ] **The online-only check covers the whole path.** Before listing anything:
  1. Make the root absolute. A bare drive letter with a colon (`X:`) means the drive root `X:\`.
  2. Resolve links: open the root for attribute access only (no data access, following links) and take the final path with `GetFinalPathNameByHandleW`. If that fails, continue with the absolute path.
  3. Walk the resolved path from the drive or share root down. For each component, read its own entry from the parent with `FindFirstFileExW` on the full name (the existing helper). If the entry has `FILE_ATTRIBUTE_RECALL_ON_OPEN`, refuse with `ScanError::OnlineOnly`. A parent is only queried after it passed the check itself, so no unpopulated folder is ever enumerated.
  4. If a level cannot be queried (access denied), stop checking and proceed with the scan as before.
  The decision logic is a pure function over a list of `(component, attributes or error)` and is unit-tested (Review Focus 2): all present → proceed; third component online-only → refuse; second component not readable → proceed; empty list (drive root) → proceed. A scan root that is a junction or a symlink to a folder is scanned as the folder it points to; its root name is the absolute path as given.
- [ ] **A file-type symlink that points to a folder** is accepted as a root, through the same link resolution. Test with `mklink /D` if the test environment allows creating it; otherwise test the resolution helper with a junction and say so in the report.
- [ ] **Device paths** (starting with `\\.\`) are refused with `ScanError::Io` of kind `InvalidInput` and the message `Device paths are not supported`. Test.
- [ ] **An entry with an empty name** in a directory listing is `Malformed` (the directory is marked unreadable and counted, like any other malformed listing). Test with a hand-built buffer.
- [ ] **Worker threads:** `available_parallelism()` clamped to 2..=16.
- [ ] **The threaded cancel test cannot hang the test run:** the scan runs in a spawned thread and the test waits with `recv_timeout(10 s)`; a timeout fails the test.
- [ ] **The ACL test fails loudly if cleanup fails:** after the guard restored the ACL, the test closes the temp directory explicitly and asserts that it succeeded.
- [ ] Not in this task: the "cancelling" indication is Task 6; trying the scan on real FAT/exFAT volumes, network shares and cloud placeholders needs hardware and is left to a follow-up issue.
- [ ] Tests first where a test can fail first; full `cargo test` green; unelevated `bench walk` on the project's `src` folder still matches; commit `fix: fallback scan checks the whole path and refuses device paths`.

### Task 5: Issue #3, folder table and row model

**Files:** Modify `src/ui/rows.rs`, `src/ui/table.rs`, `src/ui/mod.rs`, `Cargo.toml` (dev dependency only)

Row model (`src/ui/rows.rs`):

- [ ] **`Left` uses the tree, not the depth:** the parent is `tree.parent(selected)`, and its row comes from a node-to-row map that is kept with the row list. No backward scan, correct beyond 65,535 levels. Test with a chain of 70,000 folders: `reveal` the deepest, `key_nav(Left)` selects its real parent.
- [ ] **Expand and collapse change the row list in place.** Expanding inserts the folder's visible descendants after it; collapsing removes them. Test (Review Focus 3): for a tree with several expanded levels, after every toggle the row list equals the one a full rebuild produces, and the selection rule on collapse still holds. Sorting a folder's children on first expansion stays on the calling thread; it happens once per folder and sort order.
- [ ] **Collapsing drops the cached sorted lists** of the folders whose rows it removes.
- [ ] **Parent walks have a step limit** of `tree.len()`; on a parent cycle `reveal` falls back to the root instead of hanging. Test with a tree built to contain a cycle.
- [ ] **`set_sort` clears the "% of Parent was clicked" flag;** the table sets the flag after `set_sort` when that header was clicked. Test: flag set, `set_sort(Name)`, flag is false.

Table (`src/ui/table.rs`):

- [ ] **Deep rows keep their name:** the indentation is capped at 40 % of the Name column's width, and hovering a name shows the entry's full path as a tooltip.
- [ ] **A double-click on the expand arrow toggles once:** the second click of a double-click on the arrow is ignored.
- [ ] **The context menu belongs to the entry:** when the row list changes while a menu is open, the menu closes.
- [ ] **Remove `TableResponse`;** `table::show` returns nothing.
- [ ] **Remove the redundant pieces:** the `Col::Percent` arm that repeats `metric_key` (call `metric_key`), the test that repeats another's assertions, the unused `select_row` call in a test.
- [ ] **Tests that drive `table::show` without a window.** Use `egui_kittest` as a dev dependency if it runs headless without a GPU in this environment; otherwise drive `egui::Context::run` with synthetic `RawInput` events. Whichever is used, check it against the crate source first. Cover: a click on a row selects it and a following ArrowDown moves from that row; a click on the expand arrow expands without selecting; a header click sorts and a second click reverses; a double-click on the arrow leaves the folder toggled once.
- [ ] Full `cargo test` green, no warnings; `cargo build --release`; commit `fix: table and row model polish`.

### Task 6: Issue #5, main window and scan lifecycle

**Files:** Modify `src/ui/mod.rs`, `src/ui/bars.rs`, `src/win.rs`, `src/scan/mod.rs`, `src/bin/bench.rs`, `Cargo.toml`

- [ ] **No continuous repaint while scanning:** replace the `Spinner` widgets with a text indicator that changes every 250 ms (one to three dots after `Scanning`), driven by the existing 250 ms repaint request. New fixed text: `Scanning`.
- [ ] **Cancel is visible:** after Cancel the bar shows `Cancelling…` and the Cancel button is disabled until the scan thread has ended. A second Cancel does nothing. New fixed text: `Cancelling…` (with U+2026).
- [ ] **The elevation hint does not discard a result.** The hint (drive letter plus the optional failure text below) is kept beside the phase, not as a phase: a shown result stays visible under the hint row. Changing the target or starting a scan clears the hint.
- [ ] **A declined elevation says so:** the hint row shows `Administrator rights were not granted.` after `relaunch_elevated` fails. New fixed text.
- [ ] **The elevated copy starts in the current working directory** (`lpDirectory` of `ShellExecuteW`).
- [ ] **The drive list is re-read each time the drive list is opened.**
- [ ] **The phase logic is a pure, tested function.** Move the decision "scan result or error arrived → next phase" out of `poll_scan` into a function without egui types and test (Review Focus 4): MFT failure → failed with the slow scan offered; walk failure → failed without offer; `Cancelled` → idle; success → done; a result that arrives after Cancel was pressed → idle, the result is dropped; thread ended without a result → failed with `The scan ended unexpectedly.`
- [ ] **Release builds unwind:** remove `panic = "abort"` from the release profile so that a panic in a scan thread reaches "The scan ended unexpectedly." instead of ending the process. Report the size of the executable before and after.
- [ ] **`ScanTarget` derives `Clone`;** remove the field-by-field clone.
- [ ] **The startup error box can be looked at:** add the diagnostic subcommand `bench message-box` that shows the same box as a failed start (`win::error_box` with the title `Platzhirsch` and the text `Platzhirsch could not start: test`). It is not part of the shipped program. Whether the window library returns an error instead of panicking on a machine without OpenGL 2 cannot be tried here and goes to a follow-up issue.
- [ ] Update the fixed-text tests; full `cargo test` green; `cargo build --release`; start the program unelevated on the project's `src` folder and confirm it is alive after ten seconds; commit `fix: scan lifecycle feedback and tested phase logic`.

### Task 7: Issue #4, treemap drawing and resizing

**Files:** Modify `src/treemap.rs`, `src/ui/treemap_view.rs`, `src/ui/mod.rs`

- [ ] **One mesh per layout.** When the layout is computed, the fills and outlines of all tiles are built into one `egui::Mesh` (or as few as the vertex limit needs) and cached with the tiles. Each frame draws the cached mesh, moved to the panel's position, plus the header texts, the hover highlight and the selection outline. No shape is emitted per tile per frame. State in the report how the mesh is positioned each frame and what that costs.
- [ ] **Resizing is debounced.** While the panel size changes from frame to frame, the cached mesh is drawn scaled to the new size and no layout is computed; when the size has been stable for 150 ms the layout is computed once. `TreemapCache::computations()` proves it: a test feeds ten different sizes in a row and then the same size after the delay, and expects one computation for the burst. The time source is passed in, so the test needs no sleeping.
- [ ] **Elided content is visible.** `Tile` gains `pub elided: bool`, true for a folder tile that has children with a non-zero value of which none was laid out. Such tiles are drawn in a darker shade of their level colour, and their tooltip gets a third line `{n} entries too small to draw`, where `{n}` is the folder's file count plus folder count, formatted with `format_count`. Test (Review Focus 5): a folder of 100,000 one-byte files in a 300×200 panel yields its tile with `elided == true`, no child tiles, no NaN, and `hit_test` in it returns the folder.
- [ ] **A way back up.** A click on the header strip of the root tile (the folder being shown) returns that folder's parent, so the treemap then shows the parent. The tooltip over that strip has a third line `Click to go up`. At the tree's root the strip does nothing. `hit_test` stays as it is; the view decides by position. Test the pure decision function: point in the root header and a parent exists → parent; no parent → the folder itself.
- [ ] **Hover lightens only what is under the pointer:** over a folder's header strip only the strip is lightened, over a file tile the tile.
- [ ] **Words match the units:** the constants and comments say "points", not "pixels"; `MIN_SIDE_PX` and friends keep their names if renaming would touch tests in other modules, otherwise rename to `_PT`.
- [ ] **No zero or negative tile length:** clamp the last item's length to at least 0 and skip tiles that end up below the minimum side.
- [ ] **The geometry test checks the inner rectangle:** children lie inside the parent's rectangle minus padding and, where a header is reserved, minus the header. Correct the `hit_test` comment (siblings are disjoint up to rounding).
- [ ] Full `cargo test` green; `cargo build --release`; start the program on the project folder unelevated, alive after ten seconds; commit `fix: treemap draws one mesh, debounced resize, way back up`.

### Task 8: Issue #10, accessibility

**Files:** Modify `Cargo.toml`, `src/ui/table.rs`, `src/ui/treemap_view.rs`, `src/ui/bars.rs`, `src/ui/mod.rs`

- [ ] **Enable the `accesskit` feature of `eframe`.** Report the size of the executable before and after; it must stay under 15 MB.
- [ ] **Accessible names and roles.** Every interactive element has a name a screen reader can read: the drive list (`Drive or folder to scan`), the buttons, the `Show` choices, the `Treemap` switch, each column header (`Sort by {column}`), each table row (`{name}, {allocated} allocated, {size} size, {percent} of parent`, plus `folder, expanded` or `folder, collapsed` for folders and the marker text if any), and the treemap (`Treemap of {path}`). Use egui's `widget_info` mechanism; check how custom-painted responses expose it in 0.37.
- [ ] **Keyboard reaches what the mouse reaches** in the table: the Menu key and Shift+F10 open the context menu for the selected row; Enter toggles the selected folder; Ctrl+C copies the selected entry's path; the column headers can be reached with Tab and activated with Space or Enter. Test the key handling through the same headless harness as Task 5 where it allows.
- [ ] **Not in this task, and stated in the README under "Known limits":** the treemap has no keyboard navigation of its own (it follows the table's selection), and the divider between table and treemap cannot be moved with the keyboard.
- [ ] Replace the README line "The window cannot be read by screen readers" with what is now true, including the two limits above and that it was checked through the Windows UI Automation tree, not with every screen reader.
- [ ] Full `cargo test` green; `cargo build --release`; start the program on the project's `src` folder unelevated, alive after ten seconds. The controller inspects the UI Automation tree of the running window afterwards.
- [ ] Commit `feat: accessible names and keyboard access for the table`.

### Task 9: Issue #9, packaging and workflows

**Files:** Create `build.rs`, `assets/platzhirsch.ico`; Modify `Cargo.toml`, `src/ui/mod.rs` (only if the icon source is shared)

- [ ] **The executable carries an icon and a version resource.** Add the build dependency `winresource` and a `build.rs` that, for the Windows MSVC target only, embeds `assets/platzhirsch.ico` and the version information (product name `Platzhirsch`, file description from `Cargo.toml`, version from `CARGO_PKG_VERSION`, copyright `Copyright (c) 2026 Leon Kasdorf`, license `MIT`). The icon file shows the same pie as the window icon in sizes 16, 32, 48 and 256; generate it with a small test or example that renders the existing pie drawing and writes the `.ico`, commit the file, and say in the report how to regenerate it. Only `platzhirsch.exe` gets the resources, not `bench.exe`.
- [ ] Verify: `cargo build --release` works on this machine (the resource compiler `rc.exe` from the Windows SDK must be found; if it is not, stop and report); the built executable contains the version strings (search its bytes for the UTF-16 text `Platzhirsch` in the version block and report what you find) and is under 15 MB.
- [ ] Full `cargo test` green; commit `build: embed icon and version information`.

The workflow changes of this issue are made by the controller in the same range and reviewed with this task:

- [ ] Actions pinned to full commit hashes, with the version as a trailing comment.
- [ ] Every multi-line `run:` step starts with `$ErrorActionPreference = 'Stop'` and `$PSNativeCommandUseErrorActionPreference = $true`.
- [ ] `release.yml`: when the release already exists, its notes are updated too: an existing `SHA-256:` line is replaced, otherwise the Download section is appended.
- [ ] `ci.yml`: the push trigger is `main` only; pull requests against `main` stay.
- [ ] Code signing needs a certificate that the project does not have; it goes to a follow-up issue.

### Task 10: Issue #7, guard tests

**Files:** Modify `tests/anti_goals.rs`

- [ ] **Add to the banned write list:** `CreateDirectoryW`, `SetFileInformationByHandle`, `NtSetInformationFile`, `fs::hard_link`, `fs::soft_link`, `symlink_dir`, `symlink_file`, `CreateHardLinkW`, `CreateSymbolicLinkW`, `FILE_WRITE_ATTRIBUTES`, `WRITE_DAC`, `WRITE_OWNER`.
- [ ] **Spawning processes is allowlisted, not free:** `Command::new` is banned in `src/`; the one call that opens Explorer is on the allowlist.
- [ ] **Numeric access masks:** a call to `NtOpenFile` or `CreateFileW` whose access argument is a numeric literal is reported. Implement as a line check (the call name and a `0x` or decimal literal as the access argument on the same or the next two lines); test it with samples of both spellings and with the real call sites, which use named constants.
- [ ] **`set_len` is matched as a method call on a file** (`.set_len(`) and no longer flags an identifier that merely contains the word; add a sample proving that a `Vec` call named differently is not flagged and that `file.set_len(0)` is.
- [ ] **The allowlist is exact:** each entry is a file, the trimmed line and the number of times it must occur in that file (1). More or fewer occurrences fail the test.
- [ ] **Network crates are matched by prefix:** a lock-file package whose name starts with `reqwest`, `hyper`, `ureq`, `curl`, `tungstenite`, `tokio-tungstenite`, `h2`, `native-tls`, `rustls` or `openssl` fails the test. If the current `Cargo.lock` contains such a package for a non-Windows target only, do not weaken the list: report it, and the controller decides.
- [ ] The checker's own tests cover every new rule with a sample that is reported and one that is not.
- [ ] Full `cargo test` green; commit `test: close the gaps in the write and network guards`.

### Task 11: Release 0.2.0 (controller)

- [ ] Final whole-branch review, one fix wave if needed, one scoped re-review.
- [ ] Version `0.2.0` in `Cargo.toml` and `Cargo.lock`; changelog section `[0.2.0]` with date and compare links; README checked against the code (features, known limits, numbers).
- [ ] Measurement with administrator rights on the reference machine (the owner confirms one UAC prompt): `bench mft` on both drives and `bench compare` on the two folders used for 0.1.0. Pass: duration at most 10 s, deviation at most 2 %, `skipped` 0, memory within the targets, `equal: yes`.
- [ ] Look at the running window: table, treemap, hint row, cancelling, the message box from `bench message-box`, and the UI Automation tree.
- [ ] Pull request from `v0.2` to `main` that closes #1 to #10; CI green on the pull request; merge; tag `v0.2.0` on `main`; the Release workflow attaches the executable and its checksum; verify the download against the checksum.
- [ ] Follow-up issue for what needs hardware or a certificate: the fallback scan on real FAT/exFAT volumes, network shares and cloud placeholders; a start on a machine without OpenGL 2; code signing.
