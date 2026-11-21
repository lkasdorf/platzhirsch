# Platzhirsch v1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A fast disk-space analyzer for Windows: scan an NTFS drive in seconds via the MFT, show the result as a folder-tree table and a treemap, in a single .exe.

**Architecture:** One Rust package with a library and a program. Two scanners (`scan::mft` raw via the MFT, `scan::walk` in parallel via folders) fill the same flat data model (`model::Tree`). The egui UI reads the finished, immutable tree; all computation logic (row list, treemap layout, formatting) lives in pure, testable modules without UI.

**Tech Stack:** Rust 1.99.1 (`stable-x86_64-pc-windows-msvc`), `eframe`/`egui` + `egui_extras`, `windows` (Win32 bindings), `rayon`, `rfd` (folder dialog), dev: `tempfile`.

**Spec:** `docs/superpowers/specs/2026-11-21-disk-space-analyzer-design.md`. Before each task, read the affected section.

## Global Constraints

- The app is English-only (decided by Leon on 2026-11-21): UI texts, messages, code comments, test names, the changelog and the `bench` output. The texts are fixed as listed in the tasks.
- Platform: Windows 10/11 x64. Package name `platzhirsch`.
- **Never write to the scanned drive.** Open handles read-only. Never open file contents.
- **No network access**, no telemetry, no update check, no promotional dialogs. No HTTP crates.
- No name, logo or look of TreeSize. Decompile nothing.
- Whatever could not be read is marked and counted, never silently reported as 0 bytes.
- Targets: C: (approx. 2 million entries, NVMe) click to tree ≤ 10 s. Program ≤ 15 MB. Memory after scan ≤ 400 MB, peak ≤ 1 GB. Sum of "Allocated" on the fast path ≤ 2 % deviation from (total − free). Model ≤ approx. 150 bytes per entry.
- Missed targets are reported, not silently accepted.
- Non-goals from section 2 of the spec stay out (no deleting, no exports, no live tree, no settings).
- Crate versions: current in each case via `cargo add`. Check third-party APIs (egui, `windows`, `rfd`) against docs.rs, do not write them from memory. Cross-check the NTFS offsets in this plan against an NTFS reference.
- No recursion over the tree depth (stack overflow on deep paths).
- Number format: thousands separator `,`, decimal separator `.`; dates as `DD/MM/YYYY`.
- Commits on branch `v1`, never push. Commit messages end with `Co-Authored-By: Claude Fable 5.2 <noreply@anthropic.com>`.

## Review Focus

Cases the spec implies but does not spell out. Each has a test in the named task.

1. **Drive disappears or read error in the middle of the scan** (F: is on USB): error message, no crash. Test in Task 6.
2. **Cancel during the scan or closing the window**: scanner ends promptly with `Cancelled`, no hang. Tests in Task 6 and 8.
3. **Very deep nesting** (100,000 levels): no stack overflow in totals, path, row list. Tests in Task 3 and 10.
4. **Empty folder or total 0** (empty drive, only OneDrive placeholders under "Allocated"): no division by 0, empty treemap, "0.0 %". Tests in Task 2 and 9.
5. **Names with invalid UTF-16** (lone surrogates): convert lossily, no crash. Tests in Task 5 and 8.

## Files

```
Cargo.toml, .gitignore
src/lib.rs               module list
src/main.rs              startup, arguments (--scan), window
src/bin/bench.rs         diagnostic program for verification (not shipped)
src/format.rs            formatting of bytes, numbers, percent, date
src/model.rs             TreeBuilder, Tree, totals, sorting
src/scan/mod.rs          ScanTarget, Progress, ScanResult, ScanError, choose_method
src/scan/ntfs/mod.rs     NtfsError
src/scan/ntfs/boot.rs    boot sector
src/scan/ntfs/runs.rs    data runs
src/scan/ntfs/record.rs  fixups, attributes of an MFT record
src/scan/ntfs/testkit.rs tests only: RecordBuilder, FakeVolume
src/scan/mft.rs          VolumeReader, find MFT, read it, build tree
src/scan/walk.rs         fallback path
src/treemap.rs           squarified layout, hit test
src/win.rs               drives, admin status, restart, RawVolume, Explorer
src/ui/mod.rs            App, phases, scan threads
src/ui/rows.rs           TreeView: visible rows, expanding, keyboard (pure logic)
src/ui/bars.rs           top bar, status bar
src/ui/table.rs          table
src/ui/treemap_view.rs   draw treemap, mouse
tests/anti_goals.rs      checks source and Cargo.lock for forbidden calls
tests/walk_tempdir.rs    fallback path against a temporary folder tree
```

---

### Task 1: Project skeleton, Git, anti-goal guards

**Files:** Create `Cargo.toml`, `.gitignore`, `src/lib.rs`, `src/main.rs`, `src/bin/bench.rs`, `tests/anti_goals.rs`

**Interfaces:** Produces: package `platzhirsch` with a library, program `platzhirsch` and program `bench`.

- [ ] **Step 1:** `git init -b main`, `.gitignore` with `/target` and `/bench-*.txt`, commit spec and plan, then `git switch -c v1`.
- [ ] **Step 2:** `cargo init --lib --name platzhirsch`. Dependencies via `cargo add`: `eframe`, `egui_extras`, `rayon`, `rfd`, `windows` (required Win32 features as needed per task), dev `tempfile`. Build eframe with the smallest renderer that works (program size), and without the feature `persistence` (the program stores nothing). `[profile.release]`: `opt-level = 3`, `lto = "fat"`, `codegen-units = 1`, `strip = true`, `panic = "abort"`.
- [ ] **Step 3:** `src/main.rs` opens an empty eframe window titled `Platzhirsch`, in release without a console window. `src/bin/bench.rs` prints `bench` for now.
- [ ] **Step 4: Write the guard test** in `tests/anti_goals.rs`:

```rust
#[test] fn no_write_or_delete_calls_in_source() {
    // read all *.rs under src/; no file may contain any of these words
    for word in ["GENERIC_WRITE", "FILE_WRITE_DATA", "WriteFile", "DeleteFileW",
                 "RemoveDirectoryW", "MoveFileW", "MoveFileExW", "remove_file", "remove_dir"] { /* assert */ }
}
#[test] fn no_network_crates_in_lockfile() {
    for name in ["reqwest", "hyper", "ureq", "curl", "tungstenite"] { /* Cargo.lock: no `name = "<name>"` */ }
}
```

- [ ] **Step 5:** `cargo test` → 2 tests green. `cargo build --release` → builds (confirms the MSVC linker). If the linker fails: stop and report, do not switch to another toolchain.
- [ ] **Step 6:** Commit `chore: project skeleton and anti-goal guards`.

### Task 2: Formatting (`format.rs`)

**Files:** Create `src/format.rs`

**Interfaces:** Produces:
```rust
pub fn format_bytes(n: u64) -> String;
pub fn format_count(n: u64) -> String;
pub fn format_percent(part: u64, whole: u64) -> String;
pub fn format_ymd(y: u16, m: u8, d: u8) -> String;
```

- [ ] **Step 1: Write tests**

```rust
assert_eq!(format_bytes(0), "0 Bytes");
assert_eq!(format_bytes(1023), "1,023 Bytes");
assert_eq!(format_bytes(1024), "1.0 KB");
assert_eq!(format_bytes(1536), "1.5 KB");
assert_eq!(format_bytes(820_124_835_840), "763.8 GB");
assert_eq!(format_bytes(2_748_779_069_440), "2.5 TB");
assert_eq!(format_bytes(u64::MAX), "16,384.0 PB");
assert_eq!(format_count(1_683_667), "1,683,667");
assert_eq!(format_count(999), "999");
assert_eq!(format_percent(724, 1000), "72.4 %");
assert_eq!(format_percent(5, 0), "0.0 %");          // Review Focus 4
assert_eq!(format_ymd(2026, 11, 21), "21/11/2026");
```

- [ ] **Step 2:** `cargo test format` → fails. **Step 3:** Implement. Binary units (1024), one decimal place, largest unit PB. **Step 4:** green. **Step 5:** Commit `feat: german number formatting`.

### Task 3: Data model (`model.rs`)

**Files:** Create `src/model.rs`

**Interfaces:** Produces:
```rust
pub type NodeId = u32;
pub const NO_PARENT: NodeId = u32::MAX;
pub const F_DIR: u8 = 1; pub const F_UNREADABLE: u8 = 2;
pub const F_LINK: u8 = 4; pub const F_ONLINE_ONLY: u8 = 8;
#[derive(Clone, Copy, PartialEq, Eq)] pub enum Metric { Allocated, Size }
#[derive(Clone, Copy, PartialEq, Eq)] pub enum SortKey { Name, Allocated, Size, Files, Dirs, Modified }
pub struct BuildStats { pub orphaned: u64 }

pub struct TreeBuilder { /* private */ }
impl TreeBuilder {
    pub fn with_capacity(entries: usize, name_bytes: usize) -> Self;
    /// Ids are assigned in the order of the calls (0, 1, 2 …).
    /// `parent` may point to an id that is only created later.
    pub fn push(&mut self, name: &str, parent: NodeId, size: u64, allocated: u64, mtime: i64, flags: u8) -> NodeId;
    pub fn finish(self, root: NodeId) -> (Tree, BuildStats);
}
pub struct Tree { /* private, Send + Sync */ }
impl Tree {
    pub fn root(&self) -> NodeId;
    pub fn len(&self) -> usize;
    pub fn name(&self, id: NodeId) -> &str;
    pub fn parent(&self, id: NodeId) -> Option<NodeId>;
    pub fn flags(&self, id: NodeId) -> u8;
    pub fn is_dir(&self, id: NodeId) -> bool;
    pub fn size(&self, id: NodeId) -> u64;        // folder: sum of the subtree
    pub fn allocated(&self, id: NodeId) -> u64;   // folder: sum of the subtree
    pub fn metric(&self, id: NodeId, m: Metric) -> u64;
    pub fn file_count(&self, id: NodeId) -> u32;  // files in the subtree, file itself: 0
    pub fn dir_count(&self, id: NodeId) -> u32;   // subfolders in the subtree, not counting itself
    pub fn mtime(&self, id: NodeId) -> i64;       // FILETIME; folder: newest in the subtree, including its own
    pub fn children(&self, id: NodeId) -> &[NodeId];
    pub fn path(&self, id: NodeId) -> String;     // names from the root joined with '\'
    /// Folders first, then files; each group sorted by `key`. Name case-insensitive.
    pub fn sorted_children(&self, id: NodeId, key: SortKey, desc: bool) -> Vec<NodeId>;
}
```

Layout: flat arrays, names in one shared UTF-8 buffer, children as one contiguous range per folder. Reachability from `root` via breadth-first search; unreachable entries (parent missing, cycle) stay valid as ids, hang in no child list and count in `orphaned`. Totals in reverse breadth-first order.

- [ ] **Step 1: Write tests**

```rust
fn sample() -> (Tree, BuildStats) {
    let mut b = TreeBuilder::with_capacity(8, 64);
    let root = b.push("C:", NO_PARENT, 0, 0, 10, F_DIR);   // 0
    let a = b.push("a", root, 0, 0, 20, F_DIR);            // 1
    b.push("big.bin", a, 1000, 4096, 50, 0);               // 2
    b.push("Small.txt", a, 10, 0, 30, 0);                  // 3
    let bd = b.push("b", a, 0, 0, 5, F_DIR);               // 4
    b.push("x", bd, 5, 4096, 40, 0);                       // 5
    b.push("top.txt", root, 1, 4096, 1, 0);                // 6
    b.push("lost", 99, 7, 7, 7, 0);                        // 7: parent missing
    b.finish(root)
}
#[test] fn aggregates() {
    let (t, s) = sample();
    assert_eq!((t.size(0), t.allocated(0), t.file_count(0), t.dir_count(0), t.mtime(0)), (1016, 12288, 4, 2, 50));
    assert_eq!((t.size(1), t.file_count(1), t.dir_count(1)), (1015, 3, 1));
    assert_eq!(t.file_count(2), 0);
    assert_eq!(s.orphaned, 1);
    assert_eq!(t.path(5), "C:\\a\\b\\x");
    assert_eq!(t.metric(1, Metric::Allocated), 8192);
}
#[test] fn sorting_dirs_first() {
    let (t, _) = sample();
    assert_eq!(t.sorted_children(1, SortKey::Allocated, true), vec![4, 2, 3]);
    assert_eq!(t.sorted_children(1, SortKey::Name, false), vec![4, 2, 3]);   // b | big.bin, Small.txt
    assert_eq!(t.sorted_children(1, SortKey::Name, true), vec![4, 3, 2]);
}
#[test] fn parent_may_come_later() { /* push("f", 1, 3,3,0,0) before push("root", NO_PARENT,…F_DIR) as id 1; size(1) == 3 */ }
#[test] fn cycle_is_orphaned() { /* two folders that have each other as parent: orphaned == 2, no hang */ }
#[test] fn deep_chain_no_stack_overflow() {   // Review Focus 3
    /* 100_000 nested folders, at the very bottom a file with size 1:
       size(root) == 1, dir_count(root) == 99_999, path(deepest).len() > 100_000 */
}
```

- [ ] **Step 2:** `cargo test model` → fails. **Step 3:** Implement. **Step 4:** green. **Step 5:** Commit `feat: flat tree model with aggregates`.

### Task 4: Boot sector and data runs

**Files:** Create `src/scan/mod.rs` (for now only `pub mod ntfs;`), `src/scan/ntfs/mod.rs`, `src/scan/ntfs/boot.rs`, `src/scan/ntfs/runs.rs`

**Interfaces:** Produces:
```rust
#[derive(Debug, PartialEq)] pub enum NtfsError { BadSignature, BadFixup, Truncated, Malformed(&'static str) }
pub struct BootSector { pub bytes_per_sector: u32, pub cluster_size: u32, pub mft_lcn: u64, pub record_size: u32, pub total_sectors: u64 }
pub fn parse_boot_sector(buf: &[u8]) -> Result<BootSector, NtfsError>;
#[derive(Debug, PartialEq, Clone, Copy)] pub struct Run { pub lcn: Option<u64>, pub clusters: u64 }  // None = gap
pub fn decode_runs(bytes: &[u8]) -> Result<Vec<Run>, NtfsError>;
```

Boot sector offsets: `0x03` OEM id `"NTFS    "` (8 bytes), `0x0B` bytes per sector (u16), `0x0D` sectors per cluster (u8; read values above `0x80` as i8, cluster = 2^(−v) sectors), `0x28` total sectors (u64), `0x30` MFT LCN (u64), `0x40` record size (i8; negative: 2^(−v) bytes, positive: v clusters).

Data run: header byte, low nibble = number of length bytes, high nibble = number of offset bytes; offset is signed and relative to the previous LCN; 0 offset bytes = gap; header byte `0x00` = end.

- [ ] **Step 1: Write tests**

```rust
// Boot sector: 512 B/sector, 8 sectors/cluster, MFT LCN 786432, byte 0x40 = 0xF6
assert_eq!((b.bytes_per_sector, b.cluster_size, b.mft_lcn, b.record_size), (512, 4096, 786432, 1024));
// Byte 0x40 = 0xF4  → record_size 4096;  byte 0x40 = 0x01 → record_size 4096 (1 cluster)
// OEM id "FAT32   " → Err(BadSignature);  buffer with 100 bytes → Err(Truncated)

assert_eq!(decode_runs(&[0x21,0x18,0x34,0x56,0x00]).unwrap(), vec![Run{lcn:Some(0x5634),clusters:0x18}]);
assert_eq!(decode_runs(&[0x31,0x38,0x73,0x25,0x34, 0x32,0x14,0x01,0xE5,0x11,0x02, 0x31,0x42,0xAA,0x00,0x03, 0x00]).unwrap(),
    vec![Run{lcn:Some(0x342573),clusters:0x38}, Run{lcn:Some(0x363758),clusters:0x114}, Run{lcn:Some(0x393802),clusters:0x42}]);
assert_eq!(decode_runs(&[0x11,0x30,0x60, 0x21,0x10,0x00,0x01, 0x11,0x20,0xE0, 0x00]).unwrap(),   // negative jump
    vec![Run{lcn:Some(0x60),clusters:0x30}, Run{lcn:Some(0x160),clusters:0x10}, Run{lcn:Some(0x140),clusters:0x20}]);
assert_eq!(decode_runs(&[0x01,0x30, 0x11,0x08,0x10, 0x00]).unwrap(),                           // gap
    vec![Run{lcn:None,clusters:0x30}, Run{lcn:Some(0x10),clusters:0x08}]);
assert_eq!(decode_runs(&[0x21,0x18,0x34]), Err(NtfsError::Truncated));
```

- [ ] **Step 2:** fails. **Step 3:** Implement. **Step 4:** green. **Step 5:** Commit `feat: ntfs boot sector and data run decoding`.

### Task 5: Parse an MFT record

**Files:** Create `src/scan/ntfs/record.rs`, `src/scan/ntfs/testkit.rs` (`#[cfg(test)]`)

**Interfaces:**
- Consumes: `NtfsError`, `Run`, `decode_runs`.
- Produces:
```rust
pub struct NameInfo { pub parent: u64, pub name: String }   // parent = record number (lower 48 bits)
pub struct RecordInfo {
    pub in_use: bool, pub is_dir: bool,
    pub base_record: u64,            // 0 = base record, otherwise number of the base record
    pub name: Option<NameInfo>,      // first $FILE_NAME that is not a pure DOS name
    pub mtime: i64,                  // 0 if $STANDARD_INFORMATION is missing
    pub size: Option<u64>,           // logical length of the unnamed $DATA (resident, or piece with start VCN 0)
    pub allocated: u64,              // sum over all $DATA in this record (resident = 0, only pieces with start VCN 0)
    pub is_link: bool,               // $REPARSE_POINT with name-surrogate tag
    pub data_runs: Vec<(u64, Vec<Run>)>,  // unnamed $DATA only: (start VCN, runs); only filled if `want_runs`
    pub attr_list: Option<AttrListLoc>,
}
pub enum AttrListLoc { Resident(Vec<u8>), NonResident { runs: Vec<Run>, len: u64 } }
pub fn apply_fixups(record: &mut [u8], bytes_per_sector: usize) -> Result<(), NtfsError>;
pub fn parse_record(record: &[u8], record_no: u64, want_runs: bool) -> Result<RecordInfo, NtfsError>;
pub struct AttrListEntry { pub attr_type: u32, pub name_len: u8, pub start_vcn: u64, pub record_no: u64 }
pub fn parse_attr_list(bytes: &[u8]) -> Result<Vec<AttrListEntry>, NtfsError>;
```
- Testkit (for Tasks 5 and 6):
```rust
pub struct RecordBuilder;  // produces valid records including fixups
impl RecordBuilder {
    pub fn new(record_size: usize) -> Self;         // in_use, file, base record
    pub fn not_in_use(self) -> Self; pub fn dir(self) -> Self; pub fn base(self, record_no: u64) -> Self;
    pub fn std_info(self, mtime: i64) -> Self;
    pub fn file_name(self, parent: u64, name: &str, namespace: u8) -> Self;
    pub fn file_name_utf16(self, parent: u64, name: &[u16], namespace: u8) -> Self;
    pub fn data_resident(self, stream: &str, len: u32) -> Self;
    pub fn data_nonresident(self, stream: &str, start_vcn: u64, runs: &[u8], allocated: u64, real: u64, flags: u16, total_allocated: u64) -> Self;
    pub fn reparse(self, tag: u32) -> Self;
    pub fn attr_list_resident(self, entries: &[(u32, u64, u64)]) -> Self;  // (type, start VCN, record no.)
    pub fn build(self, bytes_per_sector: usize) -> Vec<u8>;
}
```

Record header offsets: `0x00` `"FILE"`, `0x04` USA offset (u16), `0x06` USA count (u16), `0x14` first attribute (u16), `0x16` flags (u16; `0x01` in use, `0x02` directory), `0x18` used size (u32), `0x20` base record (u64, lower 48 bits).
Attribute header: `0x00` type (u32; `0xFFFFFFFF` end), `0x04` length (u32), `0x08` non-resident (u8), `0x09` name length in characters (u8), `0x0A` name offset (u16), `0x0C` flags (u16; `0x0001` compressed, `0x8000` sparse). Resident: `0x10` value length (u32), `0x14` value offset (u16). Non-resident: `0x10` start VCN (u64), `0x20` run offset (u16), `0x28` allocated size (u64), `0x30` real size (u64), `0x40` total allocated (u64, only if compressed or sparse; then this field counts as "allocated").
Types: `0x10` `$STANDARD_INFORMATION` (modification time at `0x08`), `0x20` `$ATTRIBUTE_LIST`, `0x30` `$FILE_NAME` (parent `0x00`, name length `0x40`, namespace `0x41`: 2 = DOS only, name from `0x42` UTF-16), `0x80` `$DATA`, `0xC0` `$REPARSE_POINT` (tag u32 at `0x00`; name surrogate = bit `0x20000000`).
`$ATTRIBUTE_LIST` entry: `0x00` type (u32), `0x04` entry length (u16), `0x06` name length (u8), `0x08` start VCN (u64), `0x10` record reference (u64, lower 48 bits).

Rules: If `record_no == 8` or `base_record == 8` (`$BadClus`), named `$DATA` streams are ignored. Every attribute length is checked against the record end; length 0 or overflow gives `Malformed`, never an endless loop or panic.

- [ ] **Step 1: Write tests** (all with `RecordBuilder::new(1024)`, sector 512, unless stated otherwise)

```rust
#[test] fn fixup_roundtrip()      { /* build → apply_fixups Ok; corrupt one byte at the sector end → Err(BadFixup) */ }
#[test] fn signature()            { /* "BAAD" instead of "FILE" → Err(BadSignature) */ }
#[test] fn resident_file()        { /* file_name(5,"tiny",1).std_info(77).data_resident("",100)
                                       → name=("tiny",5), mtime=77, size=Some(100), allocated=0, !is_dir, base_record=0 */ }
#[test] fn nonresident_file()     { /* data_nonresident("",0,&[0x21,0x02,0x10,0x00,0x00],8192,5000,0,0) → size=Some(5000), allocated=8192 */ }
#[test] fn sparse_uses_total()    { /* flags 0x8000, allocated 1<<30, total_allocated 4096 → allocated=4096 */ }
#[test] fn compressed_uses_total(){ /* flags 0x0001, allocated 65536, total_allocated 16384 → allocated=16384 */ }
#[test] fn named_stream_adds_allocated_only() {
    /* unnamed sparse real 20000 total 0 + "WofCompressedData" nonres allocated 8192 → size=Some(20000), allocated=8192 */ }
#[test] fn later_extent_ignored() { /* data_nonresident("",16,…,999,999,0,0) → size=None, allocated=0 */ }
#[test] fn extension_record()     { /* base(27) + $DATA VCN 0 real 1_000_000 alloc 1_003_520 → base_record=27, size=Some(1_000_000) */ }
#[test] fn dos_name_skipped()     { /* file_name(5,"LONGNA~1",2).file_name(5,"long name.txt",1) → name "long name.txt" */ }
#[test] fn first_of_two_names()   { /* file_name(5,"hl1",1).file_name(24,"hl2",1) → ("hl1",5) */ }
#[test] fn directory_and_link()   { /* dir().reparse(0xA0000003) → is_dir, is_link; reparse(0x9000001A) → !is_link */ }
#[test] fn badclus_named_ignored(){ /* record_no 8, data_resident("",0) + "$Bad" nonres allocated 1<<40 → allocated=0 */ }
#[test] fn invalid_utf16_name()   { /* file_name_utf16(5,&[0x61,0xD800,0x62],1) → Ok, name contains 'a' and 'b' */ }   // Review Focus 5
#[test] fn record_4k()            { /* RecordBuilder::new(4096), build(512) → same as resident_file */ }
#[test] fn malformed_attr_len()   { /* attribute length 0 and length past the record end → Err(Malformed(_)) */ }
#[test] fn not_in_use()           { /* not_in_use() → in_use == false */ }
#[test] fn attr_list_entries()    { /* attr_list_resident(&[(0x80,0,0),(0x80,8,15)]) → parse_attr_list returns both, record_no 0 and 15 */ }
#[test] fn runs_returned_on_request() { /* want_runs=true → data_runs == vec![(0, vec![Run{lcn:Some(0x10),clusters:2}])] */ }
```

- [ ] **Step 2:** fails. **Step 3:** Implement testkit and parser. **Step 4:** green. **Step 5:** Commit `feat: mft record parser`.

### Task 6: MFT scan over a read interface

**Files:** Create `src/scan/mft.rs`; Modify `src/scan/mod.rs`, `src/scan/ntfs/testkit.rs`

**Interfaces:**
- Consumes: Tasks 3 to 5.
- Produces in `scan/mod.rs`:
```rust
pub enum ScanTarget { Volume(char), Folder(std::path::PathBuf) }
#[derive(Clone, Copy, PartialEq, Eq, Debug)] pub enum ScanMethod { Mft, Walk }
#[derive(Default)] pub struct Progress { pub done: AtomicU64, pub total: AtomicU64, pub cancel: AtomicBool }  // total 0 = unknown
pub struct ScanResult { pub tree: Tree, pub method: ScanMethod, pub duration: Duration,
                        pub skipped: u64, pub unreadable: u64, pub online_only: u64 }
#[derive(Debug)] pub enum ScanError { Cancelled, AccessDenied, NotNtfs,
    MftIncomplete { found_bytes: u64, expected_bytes: u64 }, Io(std::io::Error), Ntfs(NtfsError) }
impl std::fmt::Display for ScanError  // English messages
```
- Produces in `scan/mft.rs`:
```rust
pub trait VolumeReader: Send { fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> std::io::Result<()>; }
pub struct MftLayout { pub runs: Vec<Run>, pub byte_len: u64, pub record_count: u64 }
pub fn locate_mft<R: VolumeReader>(r: &mut R, boot: &BootSector) -> Result<MftLayout, ScanError>;
pub fn scan_mft_from<R: VolumeReader>(r: &mut R, root_name: &str, progress: &Progress) -> Result<ScanResult, ScanError>;
```
- Testkit: `pub struct FakeVolume { pub bytes: Vec<u8>, pub fail_at: Option<u64> }` (implements `VolumeReader`; returns `Err` for a read that covers `fail_at`) and `pub fn fake_volume(record_size: u32, mft_runs: &[(u64, u64)], records: Vec<(u64, Vec<u8>)>) -> FakeVolume` (takes ready-built record bytes; boot sector with 512 B/sector, 4096 B/cluster; record slots not listed stay zeros).

`locate_mft`: read record 0 at `mft_lcn × cluster_size` and parse it with `want_runs`. `byte_len` = real size of its unnamed `$DATA`. If record 0 has an `$ATTRIBUTE_LIST` (read it from the volume via its runs if needed): for each entry of type `0x80` without a name and with record number ≠ 0, read the named record via the runs known so far and append its runs in VCN order. **Mandatory check:** sum of clusters × cluster size < `byte_len` → `MftIncomplete`.

`scan_mft_from`: read the MFT in blocks via the runs (read offsets and lengths are multiples of the sector size), parse records in parallel, `progress.total` = record count, `progress.done` continuously, check `cancel` per block. Credit values from extension records to the base record (add `allocated`; `size` and `name` only if the base record has none). Then build the tree: record 5 is the root, gets the name `root_name` and `NO_PARENT` (its `$FILE_NAME` points to itself). `F_DIR` from `is_dir`, `F_LINK` from `is_link`. `skipped` = records with signature `FILE` that fail at fixup or parsing, plus in-use base records without a name, plus `BuildStats::orphaned`. Records without signature `FILE` (empty slots) do not count.

- [ ] **Step 1: Write tests**

```rust
fn image() -> FakeVolume {  // record 1024 B, MFT at LCN 4, 8 clusters = 32 records
    // each RecordBuilder below is turned into bytes with .build(512) (omitted here for brevity)
    fake_volume(1024, &[(4, 8)], vec![
      (0,  RecordBuilder::new(1024).file_name(5, "$MFT", 3)
              .data_nonresident("", 0, &[0x11,0x08,0x04,0x00], 32768, 32768, 0, 0)),
      (5,  RecordBuilder::new(1024).dir().file_name(5, ".", 3).std_info(1)),
      (8,  RecordBuilder::new(1024).file_name(5, "$BadClus", 3).data_resident("", 0)
              .data_nonresident("$Bad", 0, &[0x01,0x10,0x00], 1<<40, 1<<40, 0, 0)),
      (24, RecordBuilder::new(1024).dir().file_name(5, "docs", 1).std_info(100)),
      (25, RecordBuilder::new(1024).file_name(24, "a.txt", 1).std_info(900)
              .data_nonresident("", 0, &[0x21,0x02,0x40,0x00,0x00], 8192, 5000, 0, 0)),
      (26, RecordBuilder::new(1024).file_name(24, "tiny", 1).data_resident("", 100)),
      (27, RecordBuilder::new(1024).file_name(5, "frag.bin", 1)),
      (28, RecordBuilder::new(1024).base(27).data_nonresident("", 0, &[0x21,0x02,0x50,0x00,0x00], 1_003_520, 1_000_000, 0, 0)),
      (29, RecordBuilder::new(1024).file_name(5, "hl1", 1).file_name(24, "hl2", 1)
              .data_nonresident("", 0, &[0x21,0x01,0x60,0x00,0x00], 4096, 300, 0, 0)),
      (30, RecordBuilder::new(1024).file_name(5, "wof.dll", 1)
              .data_nonresident("", 0, &[0x01,0x05,0x00], 20480, 20000, 0x8000, 0)
              .data_nonresident("WofCompressedData", 0, &[0x21,0x02,0x70,0x00,0x00], 8192, 7000, 0, 0)),
      (31, /* RecordBuilder::new(1024).file_name(5,"kaputt",1).build(512), then byte 510 corrupted */),
    ])
}
#[test] fn end_to_end() {
    let r = scan_mft_from(&mut image(), "C:", &Progress::default()).unwrap();
    let t = &r.tree; let root = t.root();
    assert_eq!(t.name(root), "C:");
    assert_eq!(t.size(root), 1_058_168);
    assert_eq!(t.allocated(root), 1_056_768);
    assert_eq!((t.file_count(root), t.dir_count(root)), (7, 1));
    assert_eq!(r.skipped, 1);
    assert_eq!(r.method, ScanMethod::Mft);
    let docs = /* child of root named "docs" */;
    assert_eq!((t.size(docs), t.file_count(docs), t.mtime(docs)), (5100, 2, 900));   // hl2 does not count here
}
#[test] fn fragmented_mft_via_attr_list() {
    /* MFT in two pieces [(4,4),(40,4)]; record 0: $DATA only run (4,4), real 32768,
       attr_list_resident(&[(0x80,0,0),(0x80,4,15)]); record 15: base(0) + $DATA start VCN 4, run (40,4).
       File in record 20 (lies in the second piece) → locate_mft returns 2 runs, file appears in the tree */ }
#[test] fn truncated_mft_is_error() {
    /* record 0: real 65536, runs cover 32768, no attribute list
       → Err(ScanError::MftIncomplete { found_bytes: 32768, expected_bytes: 65536 }) */ }
#[test] fn cancel_stops() { /* progress.cancel = true before the call → Err(ScanError::Cancelled) */ }           // Review Focus 2
#[test] fn read_error_is_reported() { /* fail_at = Some(offset in the middle of the MFT) → Err(ScanError::Io(_)), no panic */ } // Review Focus 1
#[test] fn not_ntfs() { /* boot sector with OEM id "EXFAT   " → Err(ScanError::NotNtfs) */ }
```

- [ ] **Step 2:** fails. **Step 3:** Implement. **Step 4:** green. **Step 5:** Commit `feat: mft scan over volume reader`.

### Task 7: Windows bindings, method choice, measurement on the real drive

**Files:** Create `src/win.rs`; Modify `src/scan/mod.rs`, `src/scan/mft.rs`, `src/format.rs`, `src/bin/bench.rs`

**Interfaces:**
- Produces in `win.rs`:
```rust
pub struct VolumeInfo { pub letter: char, pub fs_name: String, pub total: u64, pub free: u64, pub removable: bool }
pub fn list_volumes() -> Vec<VolumeInfo>;            // fixed and removable drives with media inserted
pub fn volume_info(letter: char) -> Option<VolumeInfo>;
pub fn is_elevated() -> bool;
pub fn relaunch_elevated(args: &[String]) -> std::io::Result<()>;   // ShellExecuteW "runas"; Err if UAC declined
pub fn reveal_in_explorer(path: &str);
pub fn filetime_to_local_ymd(ft: i64) -> Option<(u16, u8, u8)>;      // None if ft <= 0
pub fn memory_mb() -> (u64, u64);                                    // (current, peak) working set
pub struct RawVolume; impl RawVolume { pub fn open(letter: char) -> std::io::Result<Self>; }
impl VolumeReader for RawVolume
```
  `RawVolume::open` opens `\\.\X:` with read access only, sharing read, write and delete with others.
- Produces in `scan/mod.rs`:
```rust
#[derive(Debug, PartialEq, Eq)] pub enum MethodChoice { Mft, Walk, AskElevation }
pub fn choose_method(target: &ScanTarget, elevated: bool, fs_name: &str) -> MethodChoice;
```
- Produces in `scan/mft.rs`: `pub fn scan_mft(drive: char, progress: &Progress) -> Result<ScanResult, ScanError>;` (root name `"C:"`; access denied → `ScanError::AccessDenied`).
- Produces in `format.rs`: `pub fn format_date(ft: i64) -> String;` (empty for `None`).
- `bench`: `bench mft <X:> [--out <file>]` prints lines `key: value`: `method`, `duration_s`, `entries`, `files`, `folders`, `size`, `allocated`, `skipped`, `volume_used` (total − free), `deviation_percent`, `memory_after_scan_mb`, `memory_peak_mb`. `--out` writes these lines to a file in the working directory; this is the only place in the whole package that writes, and it is only in the diagnostic program.

- [ ] **Step 1: Write tests**

```rust
let vol = ScanTarget::Volume('C'); let dir = ScanTarget::Folder("C:\\DRIVE".into());
assert_eq!(choose_method(&vol, true,  "NTFS"),  MethodChoice::Mft);
assert_eq!(choose_method(&vol, false, "NTFS"),  MethodChoice::AskElevation);
assert_eq!(choose_method(&vol, true,  "exFAT"), MethodChoice::Walk);
assert_eq!(choose_method(&vol, false, "FAT32"), MethodChoice::Walk);
assert_eq!(choose_method(&dir, true,  "NTFS"),  MethodChoice::Walk);
assert_eq!(choose_method(&ScanTarget::Folder("\\\\nas\\share".into()), true, ""), MethodChoice::Walk);

const UNIX_EPOCH_FT: i64 = 116_444_736_000_000_000;
assert_eq!(format_date(UNIX_EPOCH_FT + 1_795_262_400 * 10_000_000), "21/11/2026");  // 21/11/2026 12:00 UTC
assert_eq!(format_date(0), "");
assert!(list_volumes().iter().any(|v| v.letter == 'C' && v.fs_name == "NTFS" && v.total > v.free));
// without admin rights (this is how cargo test runs):
assert!(matches!(scan_mft('C', &Progress::default()), Err(ScanError::AccessDenied)) || is_elevated());
```

- [ ] **Step 2:** fails. **Step 3:** Implement. **Step 4:** `cargo test` green, `tests/anti_goals.rs` still green.
- [ ] **Step 5: Measurement with Leon.** Announce beforehand that a UAC prompt will come. `cargo build --release`, then per drive:
  `Start-Process -Verb RunAs -Wait .\target\release\bench.exe -ArgumentList 'mft','C:','--out','bench-c.txt'` (likewise `F:` → `bench-f.txt`). Read the files.
  Passed if for C:: `duration_s ≤ 10`, `|deviation_percent| ≤ 2`, `memory_after_scan_mb ≤ 400`, `memory_peak_mb ≤ 1024`, and `files` is in the order of 1.68 million.
  Not passed: look for the cause (first the `$BadClus` rule, other NTFS system files, the attribute list of the MFT), fix it, measure again. Report the result with numbers to Leon, even if a target remains missed.
- [ ] **Step 6:** Commit `feat: windows bindings, method choice, bench tool`.

### Task 8: Fallback path (`walk.rs`)

**Files:** Create `src/scan/walk.rs`, `tests/walk_tempdir.rs`; Modify `src/bin/bench.rs`

**Interfaces:**
- Produces:
```rust
#[derive(Debug, PartialEq, Eq)] pub enum DirAction { Descend, Link, OnlineOnly }
pub fn classify_dir(attributes: u32, reparse_tag: u32) -> DirAction;
pub fn scan_walk(root: &std::path::Path, progress: &Progress) -> Result<ScanResult, ScanError>;
```
- `bench walk <path> [--out]` (same lines, `method: Walk`) and `bench compare <folder> [--out]`: scans the drive via MFT and the folder via Walk, prints the folder's files, size and allocated for both (`mft_files`, `mft_size`, `mft_allocated`, `walk_files`, `walk_size`, `walk_allocated`) and `equal: yes|no` (file count and size).

Read folders with `GetFileInformationByHandleEx` and `FileIdBothDirectoryInfo` (returns `EndOfFile`, `AllocationSize`, attributes, modification time; for reparse points the tag is in the field `EaSize`, check against the Microsoft docs). Paths with `\\?\` prefix. `classify_dir`: attribute `0x400` (reparse) and tag with bit `0x20000000` → `Link`; otherwise attribute `0x00040000` (`RECALL_ON_OPEN`) → `OnlineOnly`; otherwise `Descend`. `Link` and `OnlineOnly` are entered as folders with `F_LINK` or `F_ONLINE_ONLY` respectively and not entered. Folders that cannot be opened get `F_UNREADABLE`. `progress.done` counts entries, `total` stays 0. Root name = the given path without a trailing `\`. Thread split is free; no stack-recursive descent.

- [ ] **Step 1: Write tests**

```rust
// Unit
assert_eq!(classify_dir(0x10, 0), DirAction::Descend);
assert_eq!(classify_dir(0x410, 0xA0000003), DirAction::Link);        // junction
assert_eq!(classify_dir(0x410, 0xA000000C), DirAction::Link);        // symlink
assert_eq!(classify_dir(0x410, 0x9000001A), DirAction::Descend);     // OneDrive folder, present locally
assert_eq!(classify_dir(0x0004_0410, 0x9000001A), DirAction::OnlineOnly);

// tests/walk_tempdir.rs: tree: a\f1 (1000 B), a\f2 (0 B), a\b\f3 (5000 B), junction a\j → a\b (cmd /c mklink /J)
let r = scan_walk(tmp.path(), &Progress::default()).unwrap(); let t = &r.tree;
assert_eq!((t.size(t.root()), t.file_count(t.root()), t.dir_count(t.root())), (6000, 3, 3));
let j = /* node "j" */; assert!(t.flags(j) & F_LINK != 0); assert!(t.children(j).is_empty());
assert_eq!(r.method, ScanMethod::Walk);
#[test] fn long_path()   { /* 30 nested folders of 20 characters each, at the bottom a 7 B file → size == 7 */ }
#[test] fn unicode_name(){ /* file "größe – 日本.txt" → name in the tree identical */ }            // Review Focus 5
#[test] fn cancel()      { /* cancel = true → Err(ScanError::Cancelled) */ }                   // Review Focus 2
#[test] fn missing_root(){ /* non-existent path → Err(ScanError::Io(_)) */ }
#[test] fn unreadable_dir_is_marked() {
    /* folder "zu" with a 500 B file; `icacls zu /deny *S-1-1-0:(RD)`; scan;
       afterwards `icacls zu /remove:d *S-1-1-0` (also on failure, otherwise cleanup fails)
       → node "zu" has F_UNREADABLE, no children, r.unreadable == 1 */ }
```

- [ ] **Step 2:** fails. **Step 3:** Implement. **Step 4:** green.
- [ ] **Step 5: Comparison with Leon** (announce UAC): `Start-Process -Verb RunAs -Wait .\target\release\bench.exe -ArgumentList 'compare','C:\Data\Photos','--out','bench-compare.txt'`. Passed with `equal: yes`. Only report the deviation of "Allocated". With `equal: no`, look for the cause and report it.
- [ ] **Step 6:** Commit `feat: parallel directory walk fallback`.

### Task 9: Treemap layout (`treemap.rs`)

**Files:** Create `src/treemap.rs`

**Interfaces:** Consumes `Tree`, `Metric`, `NodeId`. Produces:
```rust
#[derive(Clone, Copy, Debug, PartialEq)] pub struct Rect { pub x: f32, pub y: f32, pub w: f32, pub h: f32 }
pub struct Tile { pub node: NodeId, pub rect: Rect, pub depth: u8 }
pub const MIN_TILE_PX: f32 = 4.0;   // narrower tiles are not subdivided further
pub const HEADER_PX: f32 = 14.0;    // space for the folder name, if the tile is at least 40×28 px
pub const PAD_PX: f32 = 1.0;
/// Squarified algorithm (Bruls, Huizing, van Wijk). Order: parents before children.
pub fn layout(tree: &Tree, root: NodeId, metric: Metric, bounds: Rect) -> Vec<Tile>;   // = layout_with(…, HEADER_PX, PAD_PX)
pub fn layout_with(tree: &Tree, root: NodeId, metric: Metric, bounds: Rect, header_px: f32, pad_px: f32) -> Vec<Tile>;
pub fn hit_test(tiles: &[Tile], x: f32, y: f32) -> Option<NodeId>;   // deepest tile under the point
```
The first tile is always `root` itself (depth 0, the whole `bounds`). Entries with value 0 get no tile. No recursion over the tree depth.

- [ ] **Step 1: Write tests**

```rust
#[test] fn classic_example() {
    /* folder with files of sizes 6,6,4,3,2,2,1; layout_with(…, bounds 600×400, 0.0, 0.0)
       → 7 child tiles, area_i / total area == size_i / 24 (±1 %), largest aspect ratio ≤ 3.0 */ }
#[test] fn tiles_inside_parent_and_disjoint() {
    /* sample tree from Task 3, 800×300: every tile lies inside the rectangle of its parent;
       siblings do not overlap (intersection area < 0.5 px²) */ }
#[test] fn zero_total_is_empty() { /* empty folder and folder with only 0-byte files → layout returns only the root tile */ }  // Review Focus 4
#[test] fn tiny_bounds()         { /* bounds 2×2 → no panic, at most 1 tile */ }
#[test] fn hit_deepest()         { /* point in the tile of "x" (C:\a\b\x) → Some(5); point outside → None */ }
#[test] fn metric_switch()       { /* Size and Allocated give different tiles for "Small.txt" (10 vs 0: no tile with Allocated) */ }
```

- [ ] **Step 2:** fails. **Step 3:** Implement. **Step 4:** green. **Step 5:** Commit `feat: squarified treemap layout`.

### Task 10: Visible rows (`ui/rows.rs`)

**Files:** Create `src/ui/mod.rs` (for now only `pub mod rows;`), `src/ui/rows.rs`

**Interfaces:** Consumes `Tree`, `Metric`, `SortKey`. Produces:
```rust
#[derive(Clone, Copy, PartialEq, Eq, Debug)] pub struct Row { pub node: NodeId, pub depth: u16 }
pub enum NavKey { Up, Down, Left, Right }
pub struct TreeView { pub selected: NodeId, /* otherwise private */ }
impl TreeView {
    pub fn new(tree: &Tree, metric: Metric) -> Self;   // root expanded and selected; sorted by metric descending
    pub fn rows(&self) -> &[Row];
    pub fn is_expanded(&self, node: NodeId) -> bool;
    pub fn toggle(&mut self, tree: &Tree, node: NodeId);
    pub fn sort(&self) -> (SortKey, bool);
    pub fn set_sort(&mut self, tree: &Tree, key: SortKey, desc: bool);
    pub fn metric(&self) -> Metric;
    pub fn set_metric(&mut self, tree: &Tree, metric: Metric);   // re-sorts too if currently sorted by Allocated/Size
    pub fn reveal(&mut self, tree: &Tree, node: NodeId) -> usize; // expand ancestors, select, row index
    pub fn key_nav(&mut self, tree: &Tree, key: NavKey) -> usize; // row index of the new selection
}
```
The row list is only rebuilt on change. Sorted child lists are remembered per folder and discarded when the sort changes. `Left`: collapse, otherwise go to the parent. `Right`: expand, otherwise go to the first child.

- [ ] **Step 1: Write tests** (sample tree from Task 3)

```rust
let mut v = TreeView::new(&t, Metric::Allocated);
assert_eq!(v.rows(), &[Row{node:0,depth:0}, Row{node:1,depth:1}, Row{node:6,depth:1}]);
v.toggle(&t, 1);
assert_eq!(v.rows().iter().map(|r| r.node).collect::<Vec<_>>(), vec![0, 1, 4, 2, 3, 6]);
v.toggle(&t, 1); assert_eq!(v.rows().len(), 3);
assert_eq!(v.reveal(&t, 5), 3); assert_eq!(v.selected, 5); assert!(v.is_expanded(4));
v.set_metric(&t, Metric::Size); assert_eq!(v.sort(), (SortKey::Size, true));
v.set_sort(&t, SortKey::Name, false); v.set_metric(&t, Metric::Allocated); assert_eq!(v.sort(), (SortKey::Name, false));
// Keyboard, with a fresh TreeView, starting from selection 0: Down → 1; Right → expands 1, selection stays 1; Right → 4; Left (4 is collapsed) → 1; Left → collapses 1; Up → 0
#[test] fn deep_reveal() { /* chain of 100_000 folders: reveal(deepest) → rows().len() == 100_001, no panic */ }   // Review Focus 3
```

- [ ] **Step 2:** fails. **Step 3:** Implement. **Step 4:** green. **Step 5:** Commit `feat: visible row model for tree table`.

### Task 11: Window, bars, scan lifecycle

**Files:** Modify `src/main.rs`, `src/ui/mod.rs`; Create `src/ui/bars.rs`

**Interfaces:**
- Consumes: `scan::*`, `win::*`, `TreeView`, `format::*`.
- Produces:
```rust
pub fn run(initial: Option<ScanTarget>) -> eframe::Result<()>;   // ui/mod.rs
pub fn parse_args(args: &[String]) -> Option<ScanTarget>;        // main.rs: ["--scan","C:"] → Volume('C'); ["--scan","C:\\x"] → Folder
pub enum Phase { Idle, AskElevation(char), Scanning { /* progress, receiver, start time, method */ },
                 Done { /* Arc<ScanResult>, TreeView, VolumeInfo */ }, Failed { message: String, offer_walk: Option<ScanTarget> } }
pub fn status_text(result: &ScanResult, volume: Option<&VolumeInfo>) -> String;   // bars.rs
```

Behaviour per spec 5.4 and 5.5. The scan runs in its own thread, result via a channel; during the scan, repaint regularly and show progress (fast path: bar from `done / total`; fallback path: `{n} entries found`). "Cancel" and closing the window set `progress.cancel`. Method choice via `choose_method`; on `AskElevation` a notice in the bar. "Restart as administrator" calls `relaunch_elevated(["--scan", "X:"])` and exits the program on `Ok`; on `Err` (UAC declined) the notice stays. If the fast path fails, `Failed` shows the message and offers the fallback path; never a silent switch. Own simple window icon (generated in code, no tree motif).

Fixed texts: `Scan`, `Cancel`, `Folder…`, `Show:`, `Allocated`, `Size`, `Treemap`, `The fast scan needs administrator rights.`, `Restart as administrator`, `Scan slowly`, `Fast scan failed: {message}`, `Choose a drive and press "Scan".`, `The scan ended unexpectedly.` (shown if the scan thread ends without a result). While a scan runs, the elapsed time is shown as `{s} s` with one decimal place.
Status bar, parts separated by ` | `: `Free: {free} of {total}` (drive only), `{n} files`, `MFT scan, {s} s` or `Folder scan, allocated approximate, {s} s`, and each only if > 0: `{n} skipped`, `{n} unreadable`, `{n} online-only`. Duration with one decimal place and a decimal point.

- [ ] **Step 1: Write tests**

```rust
assert!(matches!(parse_args(&["--scan".into(), "C:".into()]), Some(ScanTarget::Volume('C'))));
assert!(matches!(parse_args(&["--scan".into(), "c:\\".into()]), Some(ScanTarget::Volume('C'))));
assert!(matches!(parse_args(&["--scan".into(), "C:\\DRIVE".into()]), Some(ScanTarget::Folder(_))));
assert!(parse_args(&[]).is_none());
// status_text: MFT, 4.2 s, 1_683_667 files, skipped 3, volume free 1_407_374_883_553 of 4_090_260_848_640
assert_eq!(s, "Free: 1.3 TB of 3.7 TB | 1,683,667 files | MFT scan, 4.2 s | 3 skipped");
// Walk without volume, 0.8 s, 12 files, unreadable 2, online_only 1
assert_eq!(s, "12 files | Folder scan, allocated approximate, 0.8 s | 2 unreadable | 1 online-only");
```

- [ ] **Step 2:** fails. **Step 3:** Implement; in state `Done`, show only the status bar and the root total for now. **Step 4:** tests green.
- [ ] **Step 5: Check by hand** (`cargo run --release`): the drive list shows C: and F: with size; a folder scan of `C:\Users\<user>\dev` runs through and shows the status bar; "Cancel" during a folder scan of `C:\` stops within 1 s; the notice with both buttons appears for C: without admin. Leon checks the restart as administrator in Task 14.
- [ ] **Step 6:** Commit `feat: app shell, bars, scan lifecycle`.

### Task 11b: Switch the app language to English

Tasks 1 to 11 were implemented with German UI texts, German number format and German `bench` keys before Leon's decision of 2026-11-21 that the app is English-only. This task brings the code in line with this plan. Tasks 12 to 14 are built in English from the start.

**Files:** Modify `src/format.rs`, `src/scan/mod.rs`, `src/scan/mft.rs`, `src/scan/walk.rs`, `src/win.rs`, `src/ui/mod.rs`, `src/ui/bars.rs`, `src/main.rs`, `src/bin/bench.rs`, `tests/walk_tempdir.rs`, and any other file under `src/` or `tests/` that contains German text.

UI texts:

| Find (German) | Replace with (English) |
|---|---|
| `Scannen` | `Scan` |
| `Abbrechen` | `Cancel` |
| `Ordner…` | `Folder…` |
| `Anzeige:` | `Show:` |
| `Belegt` | `Allocated` |
| `Größe` | `Size` |
| `Treemap` | `Treemap` |
| `Der Schnellscan braucht Administratorrechte.` | `The fast scan needs administrator rights.` |
| `Als Administrator neu starten` | `Restart as administrator` |
| `Langsam scannen` | `Scan slowly` |
| `Schnellscan fehlgeschlagen: {Meldung}` | `Fast scan failed: {message}` |
| `Laufwerk wählen und „Scannen" drücken.` | `Choose a drive and press "Scan".` |
| `{n} Einträge gefunden` | `{n} entries found` |
| `Scan unerwartet beendet` | `The scan ended unexpectedly.` |
| Elapsed time while scanning `{x,y} s` | `{x.y} s` |
| `Frei: {frei} von {gesamt}` | `Free: {free} of {total}` |
| `{n} Dateien` | `{n} files` |
| `MFT-Scan, {s} s` | `MFT scan, {s} s` |
| `Ordner-Scan, Belegt genähert, {s} s` | `Folder scan, allocated approximate, {s} s` |
| `{n} übersprungen` | `{n} skipped` |
| `{n} nicht lesbar` | `{n} unreadable` |
| `{n} nur online` | `{n} online-only` |
| Column headers `Name`, `Belegt`, `Größe`, `Dateien`, `Ordner`, `% Eltern`, `Geändert` | `Name`, `Allocated`, `Size`, `Files`, `Folders`, `% of Parent`, `Modified` |
| `Im Explorer anzeigen` | `Show in Explorer` |
| `Pfad kopieren` | `Copy path` |
| Markers `nicht lesbar`, `Verknüpfung`, `nur online` | `unreadable`, `link`, `online-only` |

Messages and diagnostics:

- `ScanError` `Display` and every other message a user can see: short English sentences, e.g. `Access denied (administrator rights required)`, `Not an NTFS volume`, `MFT incomplete: found {found} of {expected} bytes`, `Read error: {error}`, `Not a folder`.
- `bench` keys: `method`, `duration_s`, `entries`, `files`, `folders`, `size`, `allocated`, `skipped`, `unreadable`, `online_only`, `volume_used`, `deviation_percent`, `memory_after_scan_mb`, `memory_peak_mb`, `top:`, `error:`. Values of `method`: `MFT`, `Walk`. `bench compare`: `mft_files`, `mft_folders`, `mft_size`, `mft_allocated`, `walk_files`, `walk_folders`, `walk_size`, `walk_allocated`, `equal: yes|no`, `error: folder not found in MFT tree`.
- Comments, doc comments, test names and assertion messages in `src/` and `tests/`: English.
- The working name `Platzhirsch` (window title, package, exe) stays.

- [ ] **Step 1:** Update the existing tests to the English expectations given in Tasks 2, 7, 8 and 11 of this plan.
- [ ] **Step 2:** `cargo test` → those tests fail.
- [ ] **Step 3:** Change the code.
- [ ] **Step 4:** `cargo test` → all green, no warnings. `cargo build --release` → builds.
- [ ] **Step 5:** Search `src/` and `tests/` for remaining German (the characters ä ö ü ß and words such as `Fehler`, `Ordner`, `Datei`, `nicht`, `und`) and translate what is left. The name Platzhirsch is excluded.
- [ ] **Step 6:** Commit `refactor: switch app language to English`.

### Task 12: Table (`ui/table.rs`)

**Files:** Create `src/ui/table.rs`; Modify `src/ui/mod.rs`

**Interfaces:** Consumes `TreeView`, `Tree`, `format::*`, `win::reveal_in_explorer`. Produces:
```rust
pub struct TableResponse { pub selection_changed: bool }
pub fn show(ui: &mut egui::Ui, tree: &Tree, view: &mut TreeView, scroll_to: Option<usize>) -> TableResponse;
pub fn bar_fraction(tree: &Tree, node: NodeId, metric: Metric) -> f32;   // share of the parent folder, root 1.0, parent 0 → 0.0
pub fn marker_text(flags: u8) -> Option<&'static str>;
```

Columns in this order with fixed headers: `Name`, `Allocated`, `Size`, `Files`, `Folders`, `% of Parent`, `Modified`. Draw only visible rows (virtualized rows from `egui_extras`). Bar behind the name according to `bar_fraction`. Click on a header: sorts, a second click reverses the direction; `% of Parent` sorts by the chosen metric. Exactly one header shows the sort triangle: the one clicked last, or the chosen metric's column after a scan (added on 2026-11-21 at Leon's request, built as Task 12b). File rows leave `Files` and `Folders` empty. Click selects, click on the arrow expands or collapses, double-click on a folder expands or collapses. Arrow keys via `key_nav`, the selection scrolls into view. Context menu exactly: `Show in Explorer`, `Copy path`. Marker behind the name in a muted colour.

- [ ] **Step 1: Write tests**

```rust
assert_eq!(marker_text(F_DIR), None);
assert_eq!(marker_text(F_DIR | F_UNREADABLE), Some("unreadable"));
assert_eq!(marker_text(F_DIR | F_LINK), Some("link"));
assert_eq!(marker_text(F_DIR | F_ONLINE_ONLY), Some("online-only"));
// sample tree: bar_fraction(t, 0, Allocated) == 1.0; bar_fraction(t, 1, Allocated) ≈ 8192/12288; empty parent folder → 0.0
```

- [ ] **Step 2:** fails. **Step 3:** Implement. **Step 4:** green.
- [ ] **Step 5: Check by hand** with a folder scan of `C:\Users\<user>` (without admin): totals plausible, sorting per column, arrow keys, the context menu opens Explorer at the right entry and copies the path, scrolling is smooth after many folders were expanded with arrow right.
- [ ] **Step 6:** Commit `feat: tree table`.

### Task 13: Treemap view (`ui/treemap_view.rs`)

**Files:** Create `src/ui/treemap_view.rs`; Modify `src/ui/mod.rs`

**Interfaces:** Consumes `treemap::{layout, hit_test, Tile, Rect}`, `TreeView`. Produces:
```rust
#[derive(Default)] pub struct TreemapCache { /* key: (node, Metric, width, height rounded), tiles */ }
pub fn show(ui: &mut egui::Ui, tree: &Tree, view: &TreeView, cache: &mut TreemapCache) -> Option<NodeId>;  // clicked node
pub fn depth_color(depth: u8, dark: bool) -> egui::Color32;   // own palette, 8 levels, then repeat
```

Shows the selected entry; if a file is selected, its parent folder. Recompute the layout only when the cache key changes. Folder name in the header of the tile if there is room. Mouse hover: tooltip `{path}` and on the second line `{formatted value}`. Click: `view.reveal` in `ui/mod.rs`, the table scrolls there. Treemap in a lower area whose height can be dragged; the `Treemap` toggle in the bar hides it.

- [ ] **Step 1: Write test**

```rust
assert_ne!(depth_color(0, false), depth_color(1, false));
assert_eq!(depth_color(0, false), depth_color(8, false));
/* Cache: same key twice → layout is computed once (counter in the cache, `pub fn computations(&self) -> u32`);
   different node or different size → twice */
```

- [ ] **Step 2:** fails. **Step 3:** Implement. **Step 4:** green.
- [ ] **Step 5: Check by hand:** the treemap follows the selection, the tooltip matches the table, click selects and scrolls, switching Allocated/Size changes the tiles, the divider can be dragged, hiding works, enlarging the window does not stutter.
- [ ] **Step 6:** Commit `feat: treemap view`.

### Task 14: Acceptance

**Files:** none new; fixes as needed.

- [ ] **Step 1:** `cargo test` → all green. `cargo build --release` → no warnings from our own code.
- [ ] **Step 2:** Measure the size of `target\release\platzhirsch.exe`. Passed at ≤ 15 MB.
- [ ] **Step 3: With Leon on the real drive** (announce UAC). Start `platzhirsch.exe`, choose C:, `Scan`, `Restart as administrator`, confirm UAC. Check and note:
  - The scan starts by itself after the restart; time from click to tree ≤ 10 s (status bar).
  - The status bar shows `MFT scan`; note the number of skipped entries.
  - Root sum of "Allocated" against `Free … of …`: deviation ≤ 2 %.
  - Expand everything in `C:\Users`, scroll: smooth. Memory in Task Manager ≤ 400 MB.
  - Treemap click, context menu, sorting, toggles work.
  - Scan F:: runs through.
  - Decline UAC once: the program stays open, the notice stays.
  - Start `Scan slowly` on C: and cancel: stops within 1 s.
- [ ] **Step 4:** Result as a list to Leon: each target with the measured number and "met" or "missed". Do not gloss over missed values.
- [ ] **Step 5:** Commit `chore: v1 acceptance fixes` (if fixes were needed).
