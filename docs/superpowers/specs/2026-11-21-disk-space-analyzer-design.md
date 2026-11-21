# Design: Fast Disk-Space Analyzer for Windows

As of: 2026-11-21. Working name: **Platzhirsch** (proposal, can be changed at any time).

Markers used in this document: **[Leon]** = decided that way. **[Proposal]** = set by Claude,
not agreed, may be corrected. **[Assumption, please confirm]** = Claude's assumption that
Leon should confirm.

## 1. Purpose

A tool of our own that shows which folders on a Windows drive take up how much space: folder
tree with sizes, plus a treemap.

Why: TreeSize Free takes a long time on the large drive (C: with 2.5 TB allocated,
1.68 million files, 239,000 folders) and points to the paid version for more speed (from
41 € per user and year).

Focus **[Leon]**: first of all a fast scanner, plus a lean interface.

## 2. Goals, Non-goals, Anti-goals

### Goals

1. **Speed.** A whole NTFS drive is read through the Master File Table (MFT). Target value
   **[Proposal]**: C: (NVMe SSD, about 2 million entries) from click to finished tree in at
   most 10 seconds.
2. **Lean interface.** One window: bar at the top, folder tree as a table, treemap below it,
   status line. No ribbons, no chains of dialogs.
3. **A single .exe**, no installer, no runtime dependency. Target value **[Proposal]**: at
   most 15 MB.
4. **Also works without admin rights**, then slower (fallback path).
5. **Smooth operation** even with 2 million entries: scrolling and expanding without
   stutter. Memory **[Proposal]**: at most 400 MB with 2 million entries, measured after the
   scan has finished. During the scan the peak may go up to 1 GB.
6. **Numbers you can trust.** On the fast path, the "Allocated" total of a whole drive
   deviates by at most 2 % from (total size − free space) as reported by Windows. The
   fallback path delivers "Allocated" as an approximation and says so (see 5.3).

### Non-goals for version 1 **[Proposal]**

- Duplicate search, file search, filters by type or age
- Exports (PDF, Excel, HTML), reports, command line, scheduled scans
- Deleting, moving, renaming from within the program
- Fast path for file systems other than NTFS (ReFS, exFAT, FAT32 use the fallback path)
- A tree that fills live while a scan is running
- Refreshing without a new scan (USN journal)
- Several drives at once in one tree
- Multiple languages, themes, saved settings
- Showing the second and further hardlinks of a file individually

### Anti-goals

- **Never write to the scanned drive.** The drive and files are only opened for reading.
- **No network access**, no telemetry, no update check, no advertising dialogs.
- **No copy of the name, logo or look of TreeSize.** Own name, own icon, own colors. Nothing
  from TreeSize is decompiled or taken over.
- **No silent wrong numbers.** What could not be read is visibly marked and not shown as
  0 bytes.

## 3. Constraints

Checked on the machine (2026-11-21):

- Windows 11 Pro, x64. Rust 1.99.1, toolchain `stable-x86_64-pc-windows-msvc`.
  Visual Studio directory present under `C:\Program Files (x86)`; whether the MSVC linker
  works will only be shown by the first build.
- C: NTFS, NVMe SSD, 3.8 TB. F: NTFS, SSD over USB, 3.7 TB.
- The .NET SDK is not installed (does not matter for the chosen solution).
- The development shell does **not** run as administrator. So the fast path can only be
  tested if Leon confirms the UAC prompt.
- The project folder is empty and not a Git repository.

Technical limits:

- The MFT fast path needs admin rights (read access to `\\.\C:`) and NTFS.
- The target platform is Windows 10/11 x64. Other platforms are not planned.

Who it is for **[Assumption, please confirm]**: a tool for Leon's own machines. This
means: no code signing, no installer, acceptance is done on C: and F:. The MFT reader still
takes all sizes (sector, cluster, record) from the boot sector and assumes no fixed values,
so that it is also correct on other drives.

## 4. Solution options

### What it is built with

| | Rust + native interface (egui) | Rust core + web interface (Tauri) | C# / .NET |
|---|---|---|---|
| Scan speed | fastest | equally fast | somewhat slower, more memory |
| Delivery | one .exe, about 5–10 MB | about 10 MB, needs WebView2 | 70 MB+ or the .NET runtime |
| Interface | tree and treemap drawn by ourselves | easiest to make look good | classic Windows look |
| Catch | polishing the look takes effort | 2 million entries must be passed to the interface in small batches | SDK not installed |

**Chosen [Leon]: Rust with egui.** Fits "fast and lean" best: one file, no detour between
scanner and display, Rust is installed. The treemap has to be drawn by ourselves anyway, a
web interface would add little for it.

### How it scans

| Way | Speed | Needs | Delivers |
|---|---|---|---|
| A. Read the MFT raw and parse it ourselves | seconds | admin, NTFS | names, parents, sizes, timestamps |
| B. `FSCTL_ENUM_USN_DATA` | seconds | admin, NTFS | names and parents, **no sizes** |
| C. Walk folders in parallel | minutes with 2 million files | nothing | everything, everywhere |

B is ruled out because the sizes are missing and every file would have to be queried
individually.

**Chosen [Proposal, mentioned in conversation on 21/11, not explicitly confirmed]:
A as the fast path, C as the fallback path, both in the same program.**

## 5. Structure

One Rust package with a library and a program, so that scanner and data model can be tested
without the interface **[Proposal]**:

```
src/
  main.rs        Start, arguments, window
  lib.rs
  model.rs       Tree in memory, totals, sorting
  scan/
    mod.rs       Choice of path, progress, cancel
    mft.rs       Fast path: open drive, read MFT, parse records
    walk.rs      Fallback path: walk folders in parallel
  treemap.rs     Area layout (pure calculation, no interface)
  ui/            Window, table, treemap display, bars
  win.rs         Windows calls: drive list, admin status, restart as admin
```

The scan runs in background threads. The interface gets progress through atomic counters and
the finished result as an immutable tree (`Arc`). A running scan can be cancelled.

### 5.1 Data model (`model.rs`)

Both scanners deliver the same result. To keep 2 million entries small:

- Flat arrays instead of linked nodes. Per entry: parent index, name (reference into a
  shared character buffer, UTF-8), Size, allocated space, modification time, flags
  (folder, unreadable, link).
- Child lists as one contiguous range per folder (start and count).
- Totals per folder, calculated once after the scan from the bottom up, without recursion:
  Size, allocated, number of files, number of folders, latest modification in the subtree.
- Sorting of the children only when a folder is displayed, remembered afterwards.
- Target value: on average at most about 150 bytes per entry.

Terms:

- **Size** = logical length of the file content.
- **Allocated** = space actually reserved on the disk.

### 5.2 Fast path (`scan/mft.rs`)

Procedure:

1. Open `\\.\X:` for reading (with share mode for reading and writing by others). Read the
   boot sector: sector size, cluster size, position of the MFT, record size.
2. Determine the layout of the MFT itself. It can lie on the disk in many pieces. Record 0
   (`$MFT`) alone is not always enough for this: with a heavily fragmented MFT, the list of
   data runs overflows into extension records. The reader must therefore either evaluate the
   `$ATTRIBUTE_LIST` of `$MFT` or query the pieces through `FSCTL_GET_RETRIEVAL_POINTERS`
   (the implementer chooses, check against the real drive). **Mandatory check:** If the
   pieces found cover less than the size of the MFT stated in record 0, the fast path aborts
   with an error message. A truncated MFT would otherwise silently drop files.
3. Read the MFT in large, sector-aligned blocks. Parsing runs in parallel with reading.
4. Per record: apply fixups, check signature and "in use", evaluate attributes.
5. Build the tree, calculate totals.

Attributes to evaluate:

- `$STANDARD_INFORMATION`: modification time, file attributes.
- `$FILE_NAME`: parent record and name. Pure DOS short names (8.3) are ignored.
- `$DATA`: Size and allocated space. For data stored inside the record, the length counts
  as Size and 0 as allocated. For compressed or sparse files, the field for the space
  actually allocated counts.

Cases that matter for correct numbers:

- **Extension records.** Heavily fragmented files spread their attributes over several
  records. Solution: go through all records and credit the attributes of an extension record
  to the base record. Of multi-part `$DATA` attributes, only the piece with start VCN 0
  counts, because the total sizes are stored there. `$ATTRIBUTE_LIST` does not need to be
  read for this.
- **Additional data streams.** Size = only the unnamed stream. Allocated = sum over all
  `$DATA` streams. Reason: system files compressed by Windows (CompactOS) keep their data in
  a named stream; without this rule the Windows folder would be too small. **Exception:**
  `$BadClus` (record 8) has a named stream `$Bad` that is as large as the whole drive. It is
  not counted, otherwise about 3.8 TB would be added to the root folder. Check against the
  real drive whether other NTFS system files stand out in the same way.
- **Hardlinks.** A file with several names is counted once and shown under the name that
  comes first in its record. The rule is fixed but arbitrary: for files that are linked in
  both `System32` and `WinSxS`, the two folder sizes may differ from TreeSize. The drive
  total is still correct and never exceeds the allocated space.
- **Junctions and symbolic links** appear as separate entries without content in the MFT.
  They are shown and marked, their target is not counted.
- **OneDrive placeholders** show their full Size but 0 allocated. This is correct.
- **NTFS system files** (`$MFT`, `$LogFile` etc.) are shown in the root folder, because they
  take up real space.
- **Orphaned or broken records** (parent missing, cycle, fixup does not match, record is
  changing right now) are skipped and counted. The number is shown in the status line.
- The own overhead of folders (index data) is not counted. Falls under the 2 %.

The drive is read while it is in use. Small deviations caused by simultaneous changes are to
be accepted.

### 5.3 Fallback path (`scan/walk.rs`)

Used when there are no admin rights, the drive is not NTFS, a network path was chosen, or a
single folder is scanned.

- Read folder contents through `GetFileInformationByHandleEx` with `FileIdBothDirectoryInfo`.
  This delivers Size and allocated space of the main content per entry in one call.
- **"Allocated" is an approximation here [Proposal].** The values come from the folder
  listing. They contain no named streams (system files compressed by Windows therefore
  appear too small) and can be outdated for hardlinks. Individual queries per file would slow
  down the fallback path considerably and are left out. In this case the status line shows
  "Folder scan, allocated approximate". "Size" and the file count are exact.
- Several threads with a shared work list (one job per folder).
- Paths with the `\\?\` prefix so that long paths work.
- Junctions and symbolic links are not followed. Other reparse points (OneDrive folders) are
  walked normally, otherwise the OneDrive content would be missing.
- **Cloud folders that are not fully present locally** are not entered and are marked as
  "online-only". Listing them could otherwise cause the cloud provider to download content
  and write placeholders, which would violate "never write" and "no network". Detection
  probably through the attribute `FILE_ATTRIBUTE_RECALL_ON_OPEN`; the implementer checks this
  against the Microsoft documentation. File contents are never opened anyway.
- Folders without access are marked "unreadable" and counted in the status line.
- Known deviation: hardlinks are counted several times here.

### 5.4 Admin rights (`win.rs`) **[Proposal]**

The program starts without a UAC prompt. If a whole NTFS drive is chosen and the user is not
an administrator, a hint with two buttons appears in the bar:

- **"Restart as administrator"**: restarts the program with a UAC prompt and passes the
  chosen drive, so that the scan starts right away afterwards.
- **"Scan slowly"**: takes the fallback path.

Reason: someone who only wants to check a folder should not get a UAC prompt at every start.

Rule for choosing the path:

| Selection | Admin | File system | Path |
|---|---|---|---|
| Whole drive | yes | NTFS | Fast path |
| Whole drive | no | NTFS | Hint with both buttons |
| Whole drive | any | not NTFS | Fallback path |
| Single folder, network path | any | any | Fallback path |

If the fast path fails along the way, the error is shown and the fallback path is offered.
No silent switch.

### 5.5 Interface (`ui/`)

```
+-------------------------------------------------------------------------------------------+
| [C: 3.8 TB v] [Folder...] [Scan]   Show: (o) Allocated ( ) Size                           |
+-------------------------------------------------------------------------------------------+
| Name                        | Allocated | Size   |   Files | Folders |     % | Modified   |
| v C:\                       | 2.5 TB    | 2.7 TB |  1.68 M |   239 K | 100 % | 21/11/2026 |
|   v Data   ████████████     | 1.8 TB    | 1.8 TB | 216,840 |   7,181 |  72 % | 21/11/2026 |
|     > Photos ███████        | 1.0 TB    | 1.0 TB | 141,418 |   1,779 |  57 % | 15/11/2026 |
|     > Backup █████          | 764 GB    | 764 GB |     249 |      34 |  42 % | 21/11/2026 |
|   > Users  ██               | 378 GB    | 596 GB | 985,083 |   131 K |  15 % | 21/11/2026 |
|   ...                                                                                     |
+-------------------------------------------------------------------------------------------+
| Treemap of the selected folder                                                            |
| +------------------------------+-----------------+---------+                              |
| | Photos                       | Backup          | Users   |                              |
| | +-----------+------+         | +-------------+ | +--+--+ |                              |
| | | Mobile... | ...  |         | | extracted   | | |  |  | |                              |
| +------------------------------+-----------------+---------+                              |
+-------------------------------------------------------------------------------------------+
| Free: 1.28 TB of 3.72 TB | 1,683,667 files | MFT scan, 4.2 s | 3 skipped                  |
+-------------------------------------------------------------------------------------------+
```

**Bar at the top**

- Drive list (fixed and removable drives with size), button for folder selection,
  Scan/Cancel.
- Switch "Allocated / Size". It determines which number drives the bars, percentages,
  default sorting and treemap. Default: Allocated.
- During the scan: progress bar (fast path: share of the MFT read; fallback path: counter of
  files found).

**Table**

- Columns: Name, Allocated, Size, Files, Folders, % of Parent, Modified.
- Default sorting descending by the chosen number. A click on a column header changes the
  sorting.
- Bar behind the name, proportional to the share of the parent folder.
- The files of a folder are listed individually below the subfolders.
- Only visible rows are drawn. The list of visible rows is rebuilt when expanding and
  collapsing, not in every frame.
- Keyboard: arrow keys move and expand/collapse.
- Right click: "Show in Explorer", "Copy path". Nothing else.
- "Unreadable" entries and links are recognizable on the entry.

**Treemap**

- Shows the folder selected in the table, nested by levels (squarified method).
- Areas that are too small (below a few pixels) are not subdivided further.
- Color by level, own palette.
- Mouse over: path and size. Click: selects the entry in the table.
- The layout is only calculated on a new selection, on switching, or on a size change.
- Divider to the table can be moved, treemap can be hidden.

**Status line**: free and total space, number of files, path used and duration, number of
skipped or unreadable entries.

**Language [Leon, decided 2026-11-21]:** The app is English-only, with a fixed language. This
covers the UI, error messages, the changelog, code comments and the diagnostic program.
Numbers use the English format (`1,683,667`, `763.8 GB`). Dates are shown as `DD/MM/YYYY`
**[Proposal]**, as in Leon's TreeSize display. The exact UI texts are listed in the
implementation plan. The working name "Platzhirsch" stays.

## 6. Verification

Automatic tests (run without admin):

- MFT parsing against hand-built record bytes: fixups, data inside and outside the record,
  data runs (including gaps and negative jumps), extension records, several names, named
  streams, `$BadClus`, broken records, record size 1 KB and 4 KB, MFT layout spread over
  several records, truncated MFT leads to abort.
- Data model: totals, counts, sorting, orphaned entries.
- Treemap: areas proportional, everything inside the frame, no overlap.
- Fallback path: against a temporarily created folder tree with known sizes.

Verification on the real drive (needs Leon for the UAC prompt):

- Fast path on C: runs through, duration is measured and compared with the 10-second target.
- "Allocated" total against (total − free) from Windows, deviation at most 2 %.
- A folder without hardlinks (e.g. `C:\Data\Photos`): fast path and fallback path deliver
  the same file count and the same "Size". "Allocated" is output side by side; the deviation
  is reported but is not an acceptance criterion (very small files that lie entirely inside
  the MFT record count as 0 on the fast path and possibly as a few bytes on the fallback
  path).
- Fast path also on F: (SSD over USB).
- Usage test: expand everything, scroll, click the treemap.

If a target value is missed, this is reported and not silently accepted.

## 7. Left to the implementer

- Exact crates and versions (egui/eframe, Windows bindings, thread pool, folder dialog).
  Check the current documentation, do not work from memory.
- Block sizes, thread count, internal division of the modules.
- Exact colors, spacing, icon, wording of messages.
- Whether a small NTFS image (VHD) is used for the tests as well.
- Creating the Git repository and first commits (to be settled in the implementation plan).

## 8. Decisions made by Claude at a glance

For quick cross-checking, all changeable:

1. Working name "Platzhirsch".
2. App is English-only: decided by Leon on 2026-11-21 (Claude had first proposed German).
   Only the date format `DD/MM/YYYY` remains Claude's call.
3. Start without UAC, restart as administrator by button.
4. Single folders always use the fallback path.
5. No deleting from the program, only "Show in Explorer" and "Copy path".
6. The result appears only after the scan has finished, no live growing tree.
7. On the fast path, hardlinks are counted once, further names are not shown.
8. The default display is "Allocated", not "Size".
9. Target values: 10 s for C:, 15 MB program size, 400 MB memory after the scan (peak 1 GB),
   2 % deviation on the fast path.
10. Everything under "Non-goals" stays out of version 1.
11. Only for Leon's own machines: no code signing, no installer.
12. On the fallback path, "Allocated" is a labeled approximation.
13. Cloud folders that are not fully present locally are not entered.
14. For hardlinks, the name that comes first in the record wins.

## 9. Review

An independent reviewer read through the document on 2026-11-21. Incorporated: layout of the
MFT with heavy fragmentation (5.2 step 2), exception for `$BadClus`, accuracy of the
fallback path, cloud folders, memory target value, hardlink rule. The NTFS statements are
based on expert knowledge and have not yet been measured on a real drive; this happens in
the verification after section 6.
