# Platzhirsch v1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ein schneller Speicherplatz-Analysator für Windows: NTFS-Laufwerk in Sekunden über die MFT scannen, Ergebnis als Ordnerbaum-Tabelle und Treemap in einer einzelnen .exe.

**Architecture:** Ein Rust-Paket mit Bibliothek und Programm. Zwei Scanner (`scan::mft` roh über die MFT, `scan::walk` parallel über Ordner) füllen dasselbe flache Datenmodell (`model::Tree`). Die egui-Oberfläche liest den fertigen, unveränderlichen Baum; alle Rechenlogik (Zeilenliste, Treemap-Aufteilung, Formatierung) liegt in reinen, testbaren Modulen ohne Oberfläche.

**Tech Stack:** Rust 1.99.1 (`stable-x86_64-pc-windows-msvc`), `eframe`/`egui` + `egui_extras`, `windows` (Win32-Bindings), `rayon`, `rfd` (Ordner-Dialog), dev: `tempfile`.

**Spec:** `docs/superpowers/specs/2026-11-21-speicheranalyse-design.md`. Vor jeder Aufgabe den betroffenen Abschnitt lesen.

## Global Constraints

- **Sprachänderung (Leon, 21.11.2026): Die App ist komplett englisch.** Wo dieser Plan deutsche Oberflächentexte, deutsches Zahlenformat oder deutsche `bench`-Schlüssel nennt, gilt der Abschnitt „Sprachänderung" am Ende.
- Plattform: Windows 10/11 x64. Paketname `platzhirsch`. Code-Bezeichner, Kommentare und alle Texte englisch und fest.
- **Niemals auf das gescannte Laufwerk schreiben.** Handles nur lesend öffnen. Dateiinhalte nie öffnen.
- **Kein Netzwerkzugriff**, keine Telemetrie, keine Update-Prüfung, keine Werbe-Dialoge. Keine HTTP-Crates.
- Kein Name, Logo oder Aussehen von TreeSize. Nichts dekompilieren.
- Was nicht gelesen werden konnte, wird markiert und gezählt, nie still als 0 Bytes ausgegeben.
- Zielwerte: C: (ca. 2 Mio. Einträge, NVMe) Klick bis Baum ≤ 10 s. Programm ≤ 15 MB. Arbeitsspeicher nach Scan ≤ 400 MB, Spitze ≤ 1 GB. Summe „Belegt" im Schnellweg ≤ 2 % Abweichung von (Gesamt − Frei). Modell ≤ ca. 150 Bytes pro Eintrag.
- Verfehlte Zielwerte werden gemeldet, nicht stillschweigend hingenommen.
- Nicht-Ziele aus Abschnitt 2 der Spec bleiben draußen (kein Löschen, keine Exporte, kein Live-Baum, keine Einstellungen).
- Crate-Versionen: jeweils aktuell per `cargo add`. Fremde APIs (egui, `windows`, `rfd`) gegen docs.rs prüfen, nicht aus dem Gedächtnis schreiben. NTFS-Offsets in diesem Plan gegen eine NTFS-Referenz gegenprüfen.
- Keine Rekursion über die Baumtiefe (Stapelüberlauf bei tiefen Pfaden).
- Zahlenformat deutsch: Dezimalkomma, Tausenderpunkt, Datum `TT.MM.JJJJ`.
- Commits auf Branch `v1`, nie pushen. Commit-Nachrichten enden mit `Co-Authored-By: Claude Fable 5.2 <noreply@anthropic.com>`.

## Review Focus

Fälle, die die Spec nahelegt, aber nicht ausbuchstabiert. Jeder hat einen Test in der genannten Aufgabe.

1. **Laufwerk verschwindet oder Lesefehler mitten im Scan** (F: hängt an USB): Fehlermeldung, kein Absturz. Test in Aufgabe 6.
2. **Abbruch während des Scans oder Fenster schließen**: Scanner endet zügig mit `Cancelled`, kein Hänger. Tests in Aufgabe 6 und 8.
3. **Sehr tiefe Verschachtelung** (100.000 Ebenen): kein Stapelüberlauf in Summen, Pfad, Zeilenliste. Tests in Aufgabe 3 und 10.
4. **Leerer Ordner oder Summe 0** (leeres Laufwerk, nur OneDrive-Platzhalter bei „Belegt"): keine Division durch 0, leere Treemap, „0,0 %". Tests in Aufgabe 2 und 9.
5. **Namen mit ungültigem UTF-16** (einzelne Surrogate): verlustbehaftet umwandeln, kein Absturz. Tests in Aufgabe 5 und 8.

## Dateien

```
Cargo.toml, .gitignore
src/lib.rs               Modulliste
src/main.rs              Start, Argumente (--scan), Fenster
src/bin/bench.rs         Diagnose-Programm für die Prüfung (wird nicht ausgeliefert)
src/format.rs            deutsche Formatierung von Bytes, Zahlen, Prozent, Datum
src/model.rs             TreeBuilder, Tree, Summen, Sortierung
src/scan/mod.rs          ScanTarget, Progress, ScanResult, ScanError, choose_method
src/scan/ntfs/mod.rs     NtfsError
src/scan/ntfs/boot.rs    Bootsektor
src/scan/ntfs/runs.rs    Datenläufe
src/scan/ntfs/record.rs  Fixups, Attribute eines MFT-Records
src/scan/ntfs/testkit.rs nur für Tests: RecordBuilder, FakeVolume
src/scan/mft.rs          VolumeReader, MFT finden, lesen, Baum bauen
src/scan/walk.rs         Rückfallweg
src/treemap.rs           Squarified-Aufteilung, Treffer-Test
src/win.rs               Laufwerke, Admin-Status, Neustart, RawVolume, Explorer
src/ui/mod.rs            App, Phasen, Scan-Threads
src/ui/rows.rs           TreeView: sichtbare Zeilen, Aufklappen, Tastatur (reine Logik)
src/ui/bars.rs           Leiste oben, Statuszeile
src/ui/table.rs          Tabelle
src/ui/treemap_view.rs   Treemap zeichnen, Maus
tests/anti_goals.rs      prüft Quelltext und Cargo.lock auf verbotene Aufrufe
tests/walk_tempdir.rs    Rückfallweg gegen temporären Ordnerbaum
```

---

### Task 1: Projektgerüst, Git, Anti-Ziel-Wächter

**Files:** Create `Cargo.toml`, `.gitignore`, `src/lib.rs`, `src/main.rs`, `src/bin/bench.rs`, `tests/anti_goals.rs`

**Interfaces:** Produces: Paket `platzhirsch` mit Bibliothek, Programm `platzhirsch` und Programm `bench`.

- [ ] **Step 1:** `git init -b main`, `.gitignore` mit `/target` und `/bench-*.txt`, Spec und Plan committen, dann `git switch -c v1`.
- [ ] **Step 2:** `cargo init --lib --name platzhirsch`. Abhängigkeiten per `cargo add`: `eframe`, `egui_extras`, `rayon`, `rfd`, `windows` (benötigte Win32-Features nach Bedarf je Aufgabe), dev `tempfile`. eframe mit dem kleinsten Renderer bauen, der funktioniert (Programmgröße), und ohne das Feature `persistence` (das Programm speichert nichts). `[profile.release]`: `opt-level = 3`, `lto = "fat"`, `codegen-units = 1`, `strip = true`, `panic = "abort"`.
- [ ] **Step 3:** `src/main.rs` öffnet ein leeres eframe-Fenster mit Titel `Platzhirsch`, in Release ohne Konsolenfenster. `src/bin/bench.rs` gibt vorerst `bench` aus.
- [ ] **Step 4: Wächter-Test schreiben** in `tests/anti_goals.rs`:

```rust
#[test] fn no_write_or_delete_calls_in_source() {
    // alle *.rs unter src/ lesen; keine Datei darf eines dieser Wörter enthalten
    for word in ["GENERIC_WRITE", "FILE_WRITE_DATA", "WriteFile", "DeleteFileW",
                 "RemoveDirectoryW", "MoveFileW", "MoveFileExW", "remove_file", "remove_dir"] { /* assert */ }
}
#[test] fn no_network_crates_in_lockfile() {
    for name in ["reqwest", "hyper", "ureq", "curl", "tungstenite"] { /* Cargo.lock: kein `name = "<name>"` */ }
}
```

- [ ] **Step 5:** `cargo test` → 2 Tests grün. `cargo build --release` → baut (bestätigt den MSVC-Linker). Schlägt der Linker fehl: stoppen und melden, nicht auf eine andere Toolchain ausweichen.
- [ ] **Step 6:** Commit `chore: project skeleton and anti-goal guards`.

### Task 2: Formatierung (`format.rs`)

**Files:** Create `src/format.rs`

**Interfaces:** Produces:
```rust
pub fn format_bytes(n: u64) -> String;
pub fn format_count(n: u64) -> String;
pub fn format_percent(part: u64, whole: u64) -> String;
pub fn format_ymd(y: u16, m: u8, d: u8) -> String;
```

- [ ] **Step 1: Tests schreiben**

```rust
assert_eq!(format_bytes(0), "0 Bytes");
assert_eq!(format_bytes(1023), "1.023 Bytes");
assert_eq!(format_bytes(1024), "1,0 KB");
assert_eq!(format_bytes(1536), "1,5 KB");
assert_eq!(format_bytes(820_124_835_840), "763,8 GB");
assert_eq!(format_bytes(2_748_779_069_440), "2,5 TB");
assert_eq!(format_bytes(u64::MAX), "16.384,0 PB");
assert_eq!(format_count(1_683_667), "1.683.667");
assert_eq!(format_count(999), "999");
assert_eq!(format_percent(724, 1000), "72,4 %");
assert_eq!(format_percent(5, 0), "0,0 %");          // Review Focus 4
assert_eq!(format_ymd(2026, 11, 21), "21.11.2026");
```

- [ ] **Step 2:** `cargo test format` → schlägt fehl. **Step 3:** Umsetzen. Einheiten binär (1024), eine Nachkommastelle, größte Einheit PB. **Step 4:** grün. **Step 5:** Commit `feat: german number formatting`.

### Task 3: Datenmodell (`model.rs`)

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

pub struct TreeBuilder { /* privat */ }
impl TreeBuilder {
    pub fn with_capacity(entries: usize, name_bytes: usize) -> Self;
    /// Ids werden in Reihenfolge der Aufrufe vergeben (0, 1, 2 …).
    /// `parent` darf auf eine Id zeigen, die erst später angelegt wird.
    pub fn push(&mut self, name: &str, parent: NodeId, size: u64, allocated: u64, mtime: i64, flags: u8) -> NodeId;
    pub fn finish(self, root: NodeId) -> (Tree, BuildStats);
}
pub struct Tree { /* privat, Send + Sync */ }
impl Tree {
    pub fn root(&self) -> NodeId;
    pub fn len(&self) -> usize;
    pub fn name(&self, id: NodeId) -> &str;
    pub fn parent(&self, id: NodeId) -> Option<NodeId>;
    pub fn flags(&self, id: NodeId) -> u8;
    pub fn is_dir(&self, id: NodeId) -> bool;
    pub fn size(&self, id: NodeId) -> u64;        // Ordner: Summe des Teilbaums
    pub fn allocated(&self, id: NodeId) -> u64;   // Ordner: Summe des Teilbaums
    pub fn metric(&self, id: NodeId, m: Metric) -> u64;
    pub fn file_count(&self, id: NodeId) -> u32;  // Dateien im Teilbaum, Datei selbst: 0
    pub fn dir_count(&self, id: NodeId) -> u32;   // Unterordner im Teilbaum ohne sich selbst
    pub fn mtime(&self, id: NodeId) -> i64;       // FILETIME; Ordner: neueste im Teilbaum inkl. eigener
    pub fn children(&self, id: NodeId) -> &[NodeId];
    pub fn path(&self, id: NodeId) -> String;     // Namen ab Wurzel mit '\' verbunden
    /// Ordner zuerst, dann Dateien; jede Gruppe nach `key` sortiert. Name ohne Groß/Klein.
    pub fn sorted_children(&self, id: NodeId, key: SortKey, desc: bool) -> Vec<NodeId>;
}
```

Aufbau: flache Arrays, Namen in einem gemeinsamen UTF-8-Puffer, Kinder als zusammenhängender Bereich pro Ordner. Erreichbarkeit ab `root` per Breitensuche; nicht erreichbare Einträge (Eltern fehlt, Kreis) bleiben als Id gültig, hängen in keiner Kinderliste und zählen in `orphaned`. Summen in umgekehrter Breitensuch-Reihenfolge.

- [ ] **Step 1: Tests schreiben**

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
    b.push("lost", 99, 7, 7, 7, 0);                        // 7: Eltern fehlt
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
#[test] fn parent_may_come_later() { /* push("f", 1, 3,3,0,0) vor push("root", NO_PARENT,…F_DIR) als Id 1; size(1) == 3 */ }
#[test] fn cycle_is_orphaned() { /* zwei Ordner, die sich gegenseitig als Eltern haben: orphaned == 2, kein Hänger */ }
#[test] fn deep_chain_no_stack_overflow() {   // Review Focus 3
    /* 100_000 verschachtelte Ordner, ganz unten Datei mit size 1:
       size(root) == 1, dir_count(root) == 99_999, path(tiefste).len() > 100_000 */
}
```

- [ ] **Step 2:** `cargo test model` → schlägt fehl. **Step 3:** Umsetzen. **Step 4:** grün. **Step 5:** Commit `feat: flat tree model with aggregates`.

### Task 4: Bootsektor und Datenläufe

**Files:** Create `src/scan/mod.rs` (vorerst nur `pub mod ntfs;`), `src/scan/ntfs/mod.rs`, `src/scan/ntfs/boot.rs`, `src/scan/ntfs/runs.rs`

**Interfaces:** Produces:
```rust
#[derive(Debug, PartialEq)] pub enum NtfsError { BadSignature, BadFixup, Truncated, Malformed(&'static str) }
pub struct BootSector { pub bytes_per_sector: u32, pub cluster_size: u32, pub mft_lcn: u64, pub record_size: u32, pub total_sectors: u64 }
pub fn parse_boot_sector(buf: &[u8]) -> Result<BootSector, NtfsError>;
#[derive(Debug, PartialEq, Clone, Copy)] pub struct Run { pub lcn: Option<u64>, pub clusters: u64 }  // None = Lücke
pub fn decode_runs(bytes: &[u8]) -> Result<Vec<Run>, NtfsError>;
```

Bootsektor-Offsets: `0x03` OEM-Id `"NTFS    "` (8 Bytes), `0x0B` Bytes pro Sektor (u16), `0x0D` Sektoren pro Cluster (u8; Werte über `0x80` als i8 lesen, Cluster = 2^(−v) Sektoren), `0x28` Sektoren gesamt (u64), `0x30` MFT-LCN (u64), `0x40` Record-Größe (i8; negativ: 2^(−v) Bytes, positiv: v Cluster).

Datenlauf: Kopfbyte, unteres Halbbyte = Anzahl Längen-Bytes, oberes = Anzahl Offset-Bytes; Offset vorzeichenbehaftet und relativ zum vorigen LCN; 0 Offset-Bytes = Lücke; Kopfbyte `0x00` = Ende.

- [ ] **Step 1: Tests schreiben**

```rust
// Bootsektor: 512 B/Sektor, 8 Sektoren/Cluster, MFT-LCN 786432, Byte 0x40 = 0xF6
assert_eq!((b.bytes_per_sector, b.cluster_size, b.mft_lcn, b.record_size), (512, 4096, 786432, 1024));
// Byte 0x40 = 0xF4  → record_size 4096;  Byte 0x40 = 0x01 → record_size 4096 (1 Cluster)
// OEM-Id "FAT32   " → Err(BadSignature);  Puffer mit 100 Bytes → Err(Truncated)

assert_eq!(decode_runs(&[0x21,0x18,0x34,0x56,0x00]).unwrap(), vec![Run{lcn:Some(0x5634),clusters:0x18}]);
assert_eq!(decode_runs(&[0x31,0x38,0x73,0x25,0x34, 0x32,0x14,0x01,0xE5,0x11,0x02, 0x31,0x42,0xAA,0x00,0x03, 0x00]).unwrap(),
    vec![Run{lcn:Some(0x342573),clusters:0x38}, Run{lcn:Some(0x363758),clusters:0x114}, Run{lcn:Some(0x393802),clusters:0x42}]);
assert_eq!(decode_runs(&[0x11,0x30,0x60, 0x21,0x10,0x00,0x01, 0x11,0x20,0xE0, 0x00]).unwrap(),   // negativer Sprung
    vec![Run{lcn:Some(0x60),clusters:0x30}, Run{lcn:Some(0x160),clusters:0x10}, Run{lcn:Some(0x140),clusters:0x20}]);
assert_eq!(decode_runs(&[0x01,0x30, 0x11,0x08,0x10, 0x00]).unwrap(),                           // Lücke
    vec![Run{lcn:None,clusters:0x30}, Run{lcn:Some(0x10),clusters:0x08}]);
assert_eq!(decode_runs(&[0x21,0x18,0x34]), Err(NtfsError::Truncated));
```

- [ ] **Step 2:** schlägt fehl. **Step 3:** Umsetzen. **Step 4:** grün. **Step 5:** Commit `feat: ntfs boot sector and data run decoding`.

### Task 5: MFT-Record auswerten

**Files:** Create `src/scan/ntfs/record.rs`, `src/scan/ntfs/testkit.rs` (`#[cfg(test)]`)

**Interfaces:**
- Consumes: `NtfsError`, `Run`, `decode_runs`.
- Produces:
```rust
pub struct NameInfo { pub parent: u64, pub name: String }   // parent = Record-Nummer (untere 48 Bit)
pub struct RecordInfo {
    pub in_use: bool, pub is_dir: bool,
    pub base_record: u64,            // 0 = Basis-Record, sonst Nummer des Basis-Records
    pub name: Option<NameInfo>,      // erstes $FILE_NAME, das kein reiner DOS-Name ist
    pub mtime: i64,                  // 0 wenn $STANDARD_INFORMATION fehlt
    pub size: Option<u64>,           // logische Länge des unbenannten $DATA (resident oder Stück mit Start-VCN 0)
    pub allocated: u64,              // Summe über alle $DATA in diesem Record (resident = 0, nur Stücke mit Start-VCN 0)
    pub is_link: bool,               // $REPARSE_POINT mit Name-Surrogate-Tag
    pub data_runs: Vec<(u64, Vec<Run>)>,  // nur unbenanntes $DATA: (Start-VCN, Läufe); nur gefüllt wenn `want_runs`
    pub attr_list: Option<AttrListLoc>,
}
pub enum AttrListLoc { Resident(Vec<u8>), NonResident { runs: Vec<Run>, len: u64 } }
pub fn apply_fixups(record: &mut [u8], bytes_per_sector: usize) -> Result<(), NtfsError>;
pub fn parse_record(record: &[u8], record_no: u64, want_runs: bool) -> Result<RecordInfo, NtfsError>;
pub struct AttrListEntry { pub attr_type: u32, pub name_len: u8, pub start_vcn: u64, pub record_no: u64 }
pub fn parse_attr_list(bytes: &[u8]) -> Result<Vec<AttrListEntry>, NtfsError>;
```
- Testkit (für Aufgaben 5 und 6):
```rust
pub struct RecordBuilder;  // erzeugt gültige Records inkl. Fixups
impl RecordBuilder {
    pub fn new(record_size: usize) -> Self;         // in_use, Datei, Basis-Record
    pub fn not_in_use(self) -> Self; pub fn dir(self) -> Self; pub fn base(self, record_no: u64) -> Self;
    pub fn std_info(self, mtime: i64) -> Self;
    pub fn file_name(self, parent: u64, name: &str, namespace: u8) -> Self;
    pub fn file_name_utf16(self, parent: u64, name: &[u16], namespace: u8) -> Self;
    pub fn data_resident(self, stream: &str, len: u32) -> Self;
    pub fn data_nonresident(self, stream: &str, start_vcn: u64, runs: &[u8], allocated: u64, real: u64, flags: u16, total_allocated: u64) -> Self;
    pub fn reparse(self, tag: u32) -> Self;
    pub fn attr_list_resident(self, entries: &[(u32, u64, u64)]) -> Self;  // (Typ, Start-VCN, Record-Nr)
    pub fn build(self, bytes_per_sector: usize) -> Vec<u8>;
}
```

Offsets Record-Kopf: `0x00` `"FILE"`, `0x04` USA-Offset (u16), `0x06` USA-Anzahl (u16), `0x14` erstes Attribut (u16), `0x16` Merker (u16; `0x01` in Benutzung, `0x02` Ordner), `0x18` benutzte Größe (u32), `0x20` Basis-Record (u64, untere 48 Bit).
Attribut-Kopf: `0x00` Typ (u32; `0xFFFFFFFF` Ende), `0x04` Länge (u32), `0x08` nicht-resident (u8), `0x09` Namenslänge in Zeichen (u8), `0x0A` Namens-Offset (u16), `0x0C` Merker (u16; `0x0001` komprimiert, `0x8000` sparse). Resident: `0x10` Wertlänge (u32), `0x14` Wert-Offset (u16). Nicht-resident: `0x10` Start-VCN (u64), `0x20` Lauf-Offset (u16), `0x28` belegte Größe (u64), `0x30` echte Größe (u64), `0x40` tatsächlich belegt (u64, nur wenn komprimiert oder sparse; dann gilt dieses Feld als „belegt").
Typen: `0x10` `$STANDARD_INFORMATION` (Änderungszeit bei `0x08`), `0x20` `$ATTRIBUTE_LIST`, `0x30` `$FILE_NAME` (Eltern `0x00`, Namenslänge `0x40`, Namensraum `0x41`: 2 = nur DOS, Name ab `0x42` UTF-16), `0x80` `$DATA`, `0xC0` `$REPARSE_POINT` (Tag u32 bei `0x00`; Name-Surrogate = Bit `0x20000000`).
`$ATTRIBUTE_LIST`-Eintrag: `0x00` Typ (u32), `0x04` Eintragslänge (u16), `0x06` Namenslänge (u8), `0x08` Start-VCN (u64), `0x10` Record-Referenz (u64, untere 48 Bit).

Regeln: Gilt `record_no == 8` oder `base_record == 8` (`$BadClus`), werden benannte `$DATA`-Ströme ignoriert. Jede Attributlänge wird gegen das Record-Ende geprüft; Länge 0 oder Überlauf ergibt `Malformed`, nie Endlosschleife oder Panik.

- [ ] **Step 1: Tests schreiben** (alle mit `RecordBuilder::new(1024)`, Sektor 512, wo nicht anders genannt)

```rust
#[test] fn fixup_roundtrip()      { /* build → apply_fixups Ok; ein Byte am Sektorende verfälschen → Err(BadFixup) */ }
#[test] fn signature()            { /* "BAAD" statt "FILE" → Err(BadSignature) */ }
#[test] fn resident_file()        { /* file_name(5,"tiny",1).std_info(77).data_resident("",100)
                                       → name=("tiny",5), mtime=77, size=Some(100), allocated=0, !is_dir, base_record=0 */ }
#[test] fn nonresident_file()     { /* data_nonresident("",0,&[0x21,0x02,0x10,0x00,0x00],8192,5000,0,0) → size=Some(5000), allocated=8192 */ }
#[test] fn sparse_uses_total()    { /* flags 0x8000, allocated 1<<30, total_allocated 4096 → allocated=4096 */ }
#[test] fn compressed_uses_total(){ /* flags 0x0001, allocated 65536, total_allocated 16384 → allocated=16384 */ }
#[test] fn named_stream_adds_allocated_only() {
    /* unbenannt sparse real 20000 total 0 + "WofCompressedData" nonres allocated 8192 → size=Some(20000), allocated=8192 */ }
#[test] fn later_extent_ignored() { /* data_nonresident("",16,…,999,999,0,0) → size=None, allocated=0 */ }
#[test] fn extension_record()     { /* base(27) + $DATA VCN 0 real 1_000_000 alloc 1_003_520 → base_record=27, size=Some(1_000_000) */ }
#[test] fn dos_name_skipped()     { /* file_name(5,"LONGNA~1",2).file_name(5,"long name.txt",1) → name "long name.txt" */ }
#[test] fn first_of_two_names()   { /* file_name(5,"hl1",1).file_name(24,"hl2",1) → ("hl1",5) */ }
#[test] fn directory_and_link()   { /* dir().reparse(0xA0000003) → is_dir, is_link; reparse(0x9000001A) → !is_link */ }
#[test] fn badclus_named_ignored(){ /* record_no 8, data_resident("",0) + "$Bad" nonres allocated 1<<40 → allocated=0 */ }
#[test] fn invalid_utf16_name()   { /* file_name_utf16(5,&[0x61,0xD800,0x62],1) → Ok, name enthält 'a' und 'b' */ }   // Review Focus 5
#[test] fn record_4k()            { /* RecordBuilder::new(4096), build(512) → wie resident_file */ }
#[test] fn malformed_attr_len()   { /* Attributlänge 0 und Länge über Record-Ende → Err(Malformed(_)) */ }
#[test] fn not_in_use()           { /* not_in_use() → in_use == false */ }
#[test] fn attr_list_entries()    { /* attr_list_resident(&[(0x80,0,0),(0x80,8,15)]) → parse_attr_list liefert beide, record_no 0 und 15 */ }
#[test] fn runs_returned_on_request() { /* want_runs=true → data_runs == vec![(0, vec![Run{lcn:Some(0x10),clusters:2}])] */ }
```

- [ ] **Step 2:** schlägt fehl. **Step 3:** Testkit und Parser umsetzen. **Step 4:** grün. **Step 5:** Commit `feat: mft record parser`.

### Task 6: MFT-Scan über eine Lese-Schnittstelle

**Files:** Create `src/scan/mft.rs`; Modify `src/scan/mod.rs`, `src/scan/ntfs/testkit.rs`

**Interfaces:**
- Consumes: Aufgaben 3 bis 5.
- Produces in `scan/mod.rs`:
```rust
pub enum ScanTarget { Volume(char), Folder(std::path::PathBuf) }
#[derive(Clone, Copy, PartialEq, Eq, Debug)] pub enum ScanMethod { Mft, Walk }
#[derive(Default)] pub struct Progress { pub done: AtomicU64, pub total: AtomicU64, pub cancel: AtomicBool }  // total 0 = unbekannt
pub struct ScanResult { pub tree: Tree, pub method: ScanMethod, pub duration: Duration,
                        pub skipped: u64, pub unreadable: u64, pub online_only: u64 }
#[derive(Debug)] pub enum ScanError { Cancelled, AccessDenied, NotNtfs,
    MftIncomplete { found_bytes: u64, expected_bytes: u64 }, Io(std::io::Error), Ntfs(NtfsError) }
impl std::fmt::Display for ScanError  // deutsche Meldungen
```
- Produces in `scan/mft.rs`:
```rust
pub trait VolumeReader: Send { fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> std::io::Result<()>; }
pub struct MftLayout { pub runs: Vec<Run>, pub byte_len: u64, pub record_count: u64 }
pub fn locate_mft<R: VolumeReader>(r: &mut R, boot: &BootSector) -> Result<MftLayout, ScanError>;
pub fn scan_mft_from<R: VolumeReader>(r: &mut R, root_name: &str, progress: &Progress) -> Result<ScanResult, ScanError>;
```
- Testkit: `pub struct FakeVolume { pub bytes: Vec<u8>, pub fail_at: Option<u64> }` (implementiert `VolumeReader`; liefert `Err` bei Lesezugriff, der `fail_at` überdeckt) und `pub fn fake_volume(record_size: u32, mft_runs: &[(u64, u64)], records: Vec<(u64, Vec<u8>)>) -> FakeVolume` (nimmt fertig gebaute Record-Bytes; Bootsektor mit 512 B/Sektor, 4096 B/Cluster; nicht genannte Record-Plätze bleiben Nullen).

`locate_mft`: Record 0 bei `mft_lcn × cluster_size` lesen und mit `want_runs` auswerten. `byte_len` = echte Größe seines unbenannten `$DATA`. Hat Record 0 ein `$ATTRIBUTE_LIST` (bei Bedarf über dessen Läufe vom Datenträger lesen): für jeden Eintrag vom Typ `0x80` ohne Namen mit Record-Nummer ≠ 0 den genannten Record über die bisher bekannten Läufe lesen und dessen Läufe in VCN-Reihenfolge anhängen. **Pflichtprüfung:** Summe der Cluster × Clustergröße < `byte_len` → `MftIncomplete`.

`scan_mft_from`: MFT blockweise über die Läufe lesen (Leseoffsets und -längen sind Vielfache der Sektorgröße), Records parallel auswerten, `progress.total` = Record-Anzahl, `progress.done` laufend, `cancel` pro Block prüfen. Werte aus Erweiterungs-Records dem Basis-Record gutschreiben (`allocated` addieren; `size` und `name` nur, wenn der Basis-Record keine hat). Dann Baum bauen: Record 5 ist die Wurzel, bekommt den Namen `root_name` und `NO_PARENT` (sein `$FILE_NAME` zeigt auf sich selbst). `F_DIR` aus `is_dir`, `F_LINK` aus `is_link`. `skipped` = Records mit Signatur `FILE`, die an Fixup oder Auswertung scheitern, plus in Benutzung befindliche Basis-Records ohne Namen, plus `BuildStats::orphaned`. Records ohne Signatur `FILE` (leere Plätze) zählen nicht.

- [ ] **Step 1: Tests schreiben**

```rust
fn image() -> FakeVolume {  // Record 1024 B, MFT bei LCN 4, 8 Cluster = 32 Records
    // jeder RecordBuilder unten wird mit .build(512) zu Bytes gemacht (hier der Kürze halber weggelassen)
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
      (31, /* RecordBuilder::new(1024).file_name(5,"kaputt",1).build(512), danach Byte 510 verfälscht */),
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
    let docs = /* Kind von root mit Namen "docs" */;
    assert_eq!((t.size(docs), t.file_count(docs), t.mtime(docs)), (5100, 2, 900));   // hl2 zählt nicht hier
}
#[test] fn fragmented_mft_via_attr_list() {
    /* MFT in zwei Stücken [(4,4),(40,4)]; Record 0: $DATA nur Lauf (4,4), real 32768,
       attr_list_resident(&[(0x80,0,0),(0x80,4,15)]); Record 15: base(0) + $DATA Start-VCN 4, Lauf (40,4).
       Datei in Record 20 (liegt im zweiten Stück) → locate_mft liefert 2 Läufe, Datei erscheint im Baum */ }
#[test] fn truncated_mft_is_error() {
    /* Record 0: real 65536, Läufe decken 32768, keine Attributliste
       → Err(ScanError::MftIncomplete { found_bytes: 32768, expected_bytes: 65536 }) */ }
#[test] fn cancel_stops() { /* progress.cancel = true vor dem Aufruf → Err(ScanError::Cancelled) */ }           // Review Focus 2
#[test] fn read_error_is_reported() { /* fail_at = Some(Offset mitten in der MFT) → Err(ScanError::Io(_)), keine Panik */ } // Review Focus 1
#[test] fn not_ntfs() { /* Bootsektor mit OEM-Id "EXFAT   " → Err(ScanError::NotNtfs) */ }
```

- [ ] **Step 2:** schlägt fehl. **Step 3:** Umsetzen. **Step 4:** grün. **Step 5:** Commit `feat: mft scan over volume reader`.

### Task 7: Windows-Anbindung, Wegwahl, Messung am echten Laufwerk

**Files:** Create `src/win.rs`; Modify `src/scan/mod.rs`, `src/scan/mft.rs`, `src/format.rs`, `src/bin/bench.rs`

**Interfaces:**
- Produces in `win.rs`:
```rust
pub struct VolumeInfo { pub letter: char, pub fs_name: String, pub total: u64, pub free: u64, pub removable: bool }
pub fn list_volumes() -> Vec<VolumeInfo>;            // feste und Wechsel-Laufwerke mit eingelegtem Medium
pub fn volume_info(letter: char) -> Option<VolumeInfo>;
pub fn is_elevated() -> bool;
pub fn relaunch_elevated(args: &[String]) -> std::io::Result<()>;   // ShellExecuteW "runas"; Err wenn UAC abgelehnt
pub fn reveal_in_explorer(path: &str);
pub fn filetime_to_local_ymd(ft: i64) -> Option<(u16, u8, u8)>;      // None bei ft <= 0
pub fn memory_mb() -> (u64, u64);                                    // (aktuell, Spitze) Working Set
pub struct RawVolume; impl RawVolume { pub fn open(letter: char) -> std::io::Result<Self>; }
impl VolumeReader for RawVolume
```
  `RawVolume::open` öffnet `\\.\X:` nur mit Leserecht, Freigabe für Lesen, Schreiben und Löschen durch andere.
- Produces in `scan/mod.rs`:
```rust
#[derive(Debug, PartialEq, Eq)] pub enum MethodChoice { Mft, Walk, AskElevation }
pub fn choose_method(target: &ScanTarget, elevated: bool, fs_name: &str) -> MethodChoice;
```
- Produces in `scan/mft.rs`: `pub fn scan_mft(drive: char, progress: &Progress) -> Result<ScanResult, ScanError>;` (Wurzelname `"C:"`; Zugriff verweigert → `ScanError::AccessDenied`).
- Produces in `format.rs`: `pub fn format_date(ft: i64) -> String;` (leer bei `None`).
- `bench`: `bench mft <X:> [--out <Datei>]` gibt Zeilen `schluessel: wert` aus: `methode`, `dauer_s`, `eintraege`, `dateien`, `ordner`, `groesse`, `belegt`, `uebersprungen`, `volume_belegt` (Gesamt − Frei), `abweichung_prozent`, `speicher_nach_scan_mb`, `speicher_spitze_mb`. `--out` schreibt diese Zeilen in eine Datei im Arbeitsverzeichnis; das ist die einzige Schreibstelle im ganzen Paket und liegt nur im Diagnose-Programm.

- [ ] **Step 1: Tests schreiben**

```rust
let vol = ScanTarget::Volume('C'); let dir = ScanTarget::Folder("C:\\DRIVE".into());
assert_eq!(choose_method(&vol, true,  "NTFS"),  MethodChoice::Mft);
assert_eq!(choose_method(&vol, false, "NTFS"),  MethodChoice::AskElevation);
assert_eq!(choose_method(&vol, true,  "exFAT"), MethodChoice::Walk);
assert_eq!(choose_method(&vol, false, "FAT32"), MethodChoice::Walk);
assert_eq!(choose_method(&dir, true,  "NTFS"),  MethodChoice::Walk);
assert_eq!(choose_method(&ScanTarget::Folder("\\\\nas\\share".into()), true, ""), MethodChoice::Walk);

const UNIX_EPOCH_FT: i64 = 116_444_736_000_000_000;
assert_eq!(format_date(UNIX_EPOCH_FT + 1_795_262_400 * 10_000_000), "21.11.2026");  // 21.11.2026 12:00 UTC
assert_eq!(format_date(0), "");
assert!(list_volumes().iter().any(|v| v.letter == 'C' && v.fs_name == "NTFS" && v.total > v.free));
// ohne Admin-Rechte (so läuft cargo test):
assert!(matches!(scan_mft('C', &Progress::default()), Err(ScanError::AccessDenied)) || is_elevated());
```

- [ ] **Step 2:** schlägt fehl. **Step 3:** Umsetzen. **Step 4:** `cargo test` grün, `tests/anti_goals.rs` weiterhin grün.
- [ ] **Step 5: Messung mit Leon.** Vorher ankündigen, dass eine UAC-Abfrage kommt. `cargo build --release`, dann je Laufwerk:
  `Start-Process -Verb RunAs -Wait .\target\release\bench.exe -ArgumentList 'mft','C:','--out','bench-c.txt'` (ebenso `F:` → `bench-f.txt`). Dateien lesen.
  Bestanden, wenn für C: gilt: `dauer_s ≤ 10`, `|abweichung_prozent| ≤ 2`, `speicher_nach_scan_mb ≤ 400`, `speicher_spitze_mb ≤ 1024`, und `dateien` in der Größenordnung von 1,68 Mio. liegt.
  Nicht bestanden: Ursache suchen (zuerst `$BadClus`-Regel, weitere NTFS-Systemdateien, Attributliste der MFT), beheben, erneut messen. Ergebnis mit Zahlen an Leon melden, auch wenn ein Zielwert verfehlt bleibt.
- [ ] **Step 6:** Commit `feat: windows bindings, method choice, bench tool`.

### Task 8: Rückfallweg (`walk.rs`)

**Files:** Create `src/scan/walk.rs`, `tests/walk_tempdir.rs`; Modify `src/bin/bench.rs`

**Interfaces:**
- Produces:
```rust
#[derive(Debug, PartialEq, Eq)] pub enum DirAction { Descend, Link, OnlineOnly }
pub fn classify_dir(attributes: u32, reparse_tag: u32) -> DirAction;
pub fn scan_walk(root: &std::path::Path, progress: &Progress) -> Result<ScanResult, ScanError>;
```
- `bench walk <Pfad> [--out]` (gleiche Zeilen, `methode: Walk`) und `bench compare <Ordner> [--out]`: scannt das Laufwerk per MFT und den Ordner per Walk, gibt für beide `dateien`, `groesse`, `belegt` des Ordners aus und `gleich: ja|nein` (Dateizahl und Größe).

Ordner lesen mit `GetFileInformationByHandleEx` und `FileIdBothDirectoryInfo` (liefert `EndOfFile`, `AllocationSize`, Attribute, Änderungszeit; bei Reparse-Punkten steht das Tag im Feld `EaSize`, gegen die Microsoft-Doku prüfen). Pfade mit `\\?\`-Präfix. `classify_dir`: Attribut `0x400` (Reparse) und Tag mit Bit `0x20000000` → `Link`; sonst Attribut `0x00040000` (`RECALL_ON_OPEN`) → `OnlineOnly`; sonst `Descend`. `Link` und `OnlineOnly` werden als Ordner mit `F_LINK` bzw. `F_ONLINE_ONLY` eingetragen und nicht betreten. Ordner, die sich nicht öffnen lassen, bekommen `F_UNREADABLE`. `progress.done` zählt Einträge, `total` bleibt 0. Wurzelname = übergebener Pfad ohne abschließenden `\`. Thread-Aufteilung frei; kein Stapel-Rekursionsabstieg.

- [ ] **Step 1: Tests schreiben**

```rust
// Unit
assert_eq!(classify_dir(0x10, 0), DirAction::Descend);
assert_eq!(classify_dir(0x410, 0xA0000003), DirAction::Link);        // Junction
assert_eq!(classify_dir(0x410, 0xA000000C), DirAction::Link);        // Symlink
assert_eq!(classify_dir(0x410, 0x9000001A), DirAction::Descend);     // OneDrive-Ordner, lokal vorhanden
assert_eq!(classify_dir(0x0004_0410, 0x9000001A), DirAction::OnlineOnly);

// tests/walk_tempdir.rs — Baum: a\f1 (1000 B), a\f2 (0 B), a\b\f3 (5000 B), Junction a\j → a\b (cmd /c mklink /J)
let r = scan_walk(tmp.path(), &Progress::default()).unwrap(); let t = &r.tree;
assert_eq!((t.size(t.root()), t.file_count(t.root()), t.dir_count(t.root())), (6000, 3, 3));
let j = /* Knoten "j" */; assert!(t.flags(j) & F_LINK != 0); assert!(t.children(j).is_empty());
assert_eq!(r.method, ScanMethod::Walk);
#[test] fn long_path()   { /* 30 verschachtelte Ordner mit je 20 Zeichen, unten Datei 7 B → size == 7 */ }
#[test] fn unicode_name(){ /* Datei "größe – 日本.txt" → Name im Baum identisch */ }            // Review Focus 5
#[test] fn cancel()      { /* cancel = true → Err(ScanError::Cancelled) */ }                   // Review Focus 2
#[test] fn missing_root(){ /* nicht existierender Pfad → Err(ScanError::Io(_)) */ }
#[test] fn unreadable_dir_is_marked() {
    /* Ordner "zu" mit Datei 500 B; `icacls zu /deny *S-1-1-0:(RD)`; scannen;
       danach `icacls zu /remove:d *S-1-1-0` (auch bei Fehlschlag, sonst scheitert das Aufräumen)
       → Knoten "zu" hat F_UNREADABLE, keine Kinder, r.unreadable == 1 */ }
```

- [ ] **Step 2:** schlägt fehl. **Step 3:** Umsetzen. **Step 4:** grün.
- [ ] **Step 5: Vergleich mit Leon** (UAC ankündigen): `Start-Process -Verb RunAs -Wait .\target\release\bench.exe -ArgumentList 'compare','C:\DRIVE\Photos','--out','bench-compare.txt'`. Bestanden bei `gleich: ja`. Abweichung von „Belegt" nur berichten. Bei `gleich: nein` Ursache suchen und melden.
- [ ] **Step 6:** Commit `feat: parallel directory walk fallback`.

### Task 9: Treemap-Aufteilung (`treemap.rs`)

**Files:** Create `src/treemap.rs`

**Interfaces:** Consumes `Tree`, `Metric`, `NodeId`. Produces:
```rust
#[derive(Clone, Copy, Debug, PartialEq)] pub struct Rect { pub x: f32, pub y: f32, pub w: f32, pub h: f32 }
pub struct Tile { pub node: NodeId, pub rect: Rect, pub depth: u8 }
pub const MIN_TILE_PX: f32 = 4.0;   // schmalere Flächen werden nicht weiter unterteilt
pub const HEADER_PX: f32 = 14.0;    // Platz für den Ordnernamen, wenn die Fläche mindestens 40×28 px hat
pub const PAD_PX: f32 = 1.0;
/// Squarified-Verfahren (Bruls, Huizing, van Wijk). Reihenfolge: Eltern vor Kindern.
pub fn layout(tree: &Tree, root: NodeId, metric: Metric, bounds: Rect) -> Vec<Tile>;   // = layout_with(…, HEADER_PX, PAD_PX)
pub fn layout_with(tree: &Tree, root: NodeId, metric: Metric, bounds: Rect, header_px: f32, pad_px: f32) -> Vec<Tile>;
pub fn hit_test(tiles: &[Tile], x: f32, y: f32) -> Option<NodeId>;   // tiefste Fläche unter dem Punkt
```
Die erste Fläche ist immer `root` selbst (Tiefe 0, ganze `bounds`). Einträge mit Wert 0 bekommen keine Fläche. Ohne Rekursion über die Baumtiefe.

- [ ] **Step 1: Tests schreiben**

```rust
#[test] fn classic_example() {
    /* Ordner mit Dateien der Größen 6,6,4,3,2,2,1; layout_with(…, bounds 600×400, 0.0, 0.0)
       → 7 Kind-Flächen, Fläche_i / Gesamtfläche == Größe_i / 24 (±1 %), größtes Seitenverhältnis ≤ 3,0 */ }
#[test] fn tiles_inside_parent_and_disjoint() {
    /* sample-Baum aus Aufgabe 3, 800×300: jede Fläche liegt im Rechteck ihres Elternteils;
       Geschwister überlappen nicht (Schnittfläche < 0,5 px²) */ }
#[test] fn zero_total_is_empty() { /* leerer Ordner und Ordner mit nur 0-Byte-Dateien → layout gibt nur die Wurzelfläche zurück */ }  // Review Focus 4
#[test] fn tiny_bounds()         { /* bounds 2×2 → keine Panik, höchstens 1 Fläche */ }
#[test] fn hit_deepest()         { /* Punkt in der Fläche von "x" (C:\a\b\x) → Some(5); Punkt außerhalb → None */ }
#[test] fn metric_switch()       { /* Size und Allocated ergeben unterschiedliche Flächen für "Small.txt" (10 vs 0: bei Allocated keine Fläche) */ }
```

- [ ] **Step 2:** schlägt fehl. **Step 3:** Umsetzen. **Step 4:** grün. **Step 5:** Commit `feat: squarified treemap layout`.

### Task 10: Sichtbare Zeilen (`ui/rows.rs`)

**Files:** Create `src/ui/mod.rs` (vorerst nur `pub mod rows;`), `src/ui/rows.rs`

**Interfaces:** Consumes `Tree`, `Metric`, `SortKey`. Produces:
```rust
#[derive(Clone, Copy, PartialEq, Eq, Debug)] pub struct Row { pub node: NodeId, pub depth: u16 }
pub enum NavKey { Up, Down, Left, Right }
pub struct TreeView { pub selected: NodeId, /* sonst privat */ }
impl TreeView {
    pub fn new(tree: &Tree, metric: Metric) -> Self;   // Wurzel aufgeklappt und gewählt; Sortierung nach metric absteigend
    pub fn rows(&self) -> &[Row];
    pub fn is_expanded(&self, node: NodeId) -> bool;
    pub fn toggle(&mut self, tree: &Tree, node: NodeId);
    pub fn sort(&self) -> (SortKey, bool);
    pub fn set_sort(&mut self, tree: &Tree, key: SortKey, desc: bool);
    pub fn metric(&self) -> Metric;
    pub fn set_metric(&mut self, tree: &Tree, metric: Metric);   // sortiert mit um, wenn aktuell nach Allocated/Size sortiert
    pub fn reveal(&mut self, tree: &Tree, node: NodeId) -> usize; // Vorfahren aufklappen, wählen, Zeilenindex
    pub fn key_nav(&mut self, tree: &Tree, key: NavKey) -> usize; // Zeilenindex der neuen Auswahl
}
```
Die Zeilenliste wird nur bei Änderung neu gebildet. Sortierte Kinderlisten werden pro Ordner gemerkt und bei Sortierwechsel verworfen. `Left`: zuklappen, sonst zum Elternteil. `Right`: aufklappen, sonst zum ersten Kind.

- [ ] **Step 1: Tests schreiben** (sample-Baum aus Aufgabe 3)

```rust
let mut v = TreeView::new(&t, Metric::Allocated);
assert_eq!(v.rows(), &[Row{node:0,depth:0}, Row{node:1,depth:1}, Row{node:6,depth:1}]);
v.toggle(&t, 1);
assert_eq!(v.rows().iter().map(|r| r.node).collect::<Vec<_>>(), vec![0, 1, 4, 2, 3, 6]);
v.toggle(&t, 1); assert_eq!(v.rows().len(), 3);
assert_eq!(v.reveal(&t, 5), 3); assert_eq!(v.selected, 5); assert!(v.is_expanded(4));
v.set_metric(&t, Metric::Size); assert_eq!(v.sort(), (SortKey::Size, true));
v.set_sort(&t, SortKey::Name, false); v.set_metric(&t, Metric::Allocated); assert_eq!(v.sort(), (SortKey::Name, false));
// Tastatur, mit frischer TreeView, ab Auswahl 0: Down → 1; Right → klappt 1 auf, Auswahl bleibt 1; Right → 4; Left (4 ist zu) → 1; Left → klappt 1 zu; Up → 0
#[test] fn deep_reveal() { /* Kette mit 100_000 Ordnern: reveal(tiefster) → rows().len() == 100_001, keine Panik */ }   // Review Focus 3
```

- [ ] **Step 2:** schlägt fehl. **Step 3:** Umsetzen. **Step 4:** grün. **Step 5:** Commit `feat: visible row model for tree table`.

### Task 11: Fenster, Leisten, Scan-Ablauf

**Files:** Modify `src/main.rs`, `src/ui/mod.rs`; Create `src/ui/bars.rs`

**Interfaces:**
- Consumes: `scan::*`, `win::*`, `TreeView`, `format::*`.
- Produces:
```rust
pub fn run(initial: Option<ScanTarget>) -> eframe::Result<()>;   // ui/mod.rs
pub fn parse_args(args: &[String]) -> Option<ScanTarget>;        // main.rs: ["--scan","C:"] → Volume('C'); ["--scan","C:\\x"] → Folder
pub enum Phase { Idle, AskElevation(char), Scanning { /* progress, Empfänger, Startzeit, Methode */ },
                 Done { /* Arc<ScanResult>, TreeView, VolumeInfo */ }, Failed { message: String, offer_walk: Option<ScanTarget> } }
pub fn status_text(result: &ScanResult, volume: Option<&VolumeInfo>) -> String;   // bars.rs
```

Verhalten nach Spec 5.4 und 5.5. Scan läuft in einem eigenen Thread, Ergebnis über einen Kanal; während des Scans regelmäßig neu zeichnen und Fortschritt zeigen (Schnellweg: Balken aus `done / total`; Rückfallweg: `{n} Einträge gefunden`). „Abbrechen" und Fenster schließen setzen `progress.cancel`. Wegwahl über `choose_method`; bei `AskElevation` Hinweis in der Leiste. „Als Administrator neu starten" ruft `relaunch_elevated(["--scan", "X:"])` und beendet das Programm bei `Ok`; bei `Err` (UAC abgelehnt) bleibt der Hinweis stehen. Scheitert der Schnellweg, zeigt `Failed` die Meldung und bietet den Rückfallweg an; nie stiller Wechsel. Eigenes, einfaches Fenster-Icon (im Code erzeugt, kein Baum-Motiv).

Feste Texte: `Scannen`, `Abbrechen`, `Ordner…`, `Anzeige:`, `Belegt`, `Größe`, `Treemap`, `Der Schnellscan braucht Administratorrechte.`, `Als Administrator neu starten`, `Langsam scannen`, `Schnellscan fehlgeschlagen: {Meldung}`, `Laufwerk wählen und „Scannen" drücken.`
Statuszeile, Teile mit ` | ` getrennt: `Frei: {frei} von {gesamt}` (nur bei Laufwerk), `{n} Dateien`, `MFT-Scan, {s} s` oder `Ordner-Scan, Belegt genähert, {s} s`, und jeweils nur wenn > 0: `{n} übersprungen`, `{n} nicht lesbar`, `{n} nur online`. Dauer mit einer Nachkommastelle und Komma.

- [ ] **Step 1: Tests schreiben**

```rust
assert!(matches!(parse_args(&["--scan".into(), "C:".into()]), Some(ScanTarget::Volume('C'))));
assert!(matches!(parse_args(&["--scan".into(), "c:\\".into()]), Some(ScanTarget::Volume('C'))));
assert!(matches!(parse_args(&["--scan".into(), "C:\\DRIVE".into()]), Some(ScanTarget::Folder(_))));
assert!(parse_args(&[]).is_none());
// status_text: MFT, 4,2 s, 1_683_667 Dateien, skipped 3, Volume frei 1_407_374_883_553 von 4_090_260_848_640
assert_eq!(s, "Frei: 1,3 TB von 3,7 TB | 1.683.667 Dateien | MFT-Scan, 4,2 s | 3 übersprungen");
// Walk ohne Volume, 0,8 s, 12 Dateien, unreadable 2, online_only 1
assert_eq!(s, "12 Dateien | Ordner-Scan, Belegt genähert, 0,8 s | 2 nicht lesbar | 1 nur online");
```

- [ ] **Step 2:** schlägt fehl. **Step 3:** Umsetzen; im Zustand `Done` vorerst nur Statuszeile und Wurzelsumme anzeigen. **Step 4:** Tests grün.
- [ ] **Step 5: Von Hand prüfen** (`cargo run --release`): Laufwerksliste zeigt C: und F: mit Größe; Ordner-Scan von `C:\Users\LeonKasdorf\dev` läuft durch und zeigt die Statuszeile; „Abbrechen" während eines Ordner-Scans von `C:\` stoppt binnen 1 s; Hinweis mit beiden Knöpfen erscheint bei C: ohne Admin. Den Neustart als Administrator prüft Leon in Aufgabe 14.
- [ ] **Step 6:** Commit `feat: app shell, bars, scan lifecycle`.

### Task 12: Tabelle (`ui/table.rs`)

**Files:** Create `src/ui/table.rs`; Modify `src/ui/mod.rs`

**Interfaces:** Consumes `TreeView`, `Tree`, `format::*`, `win::reveal_in_explorer`. Produces:
```rust
pub struct TableResponse { pub selection_changed: bool }
pub fn show(ui: &mut egui::Ui, tree: &Tree, view: &mut TreeView, scroll_to: Option<usize>) -> TableResponse;
pub fn bar_fraction(tree: &Tree, node: NodeId, metric: Metric) -> f32;   // Anteil am Elternordner, Wurzel 1.0, Eltern 0 → 0.0
pub fn marker_text(flags: u8) -> Option<&'static str>;
```

Spalten in dieser Reihenfolge mit festen Köpfen: `Name`, `Belegt`, `Größe`, `Dateien`, `Ordner`, `% Eltern`, `Geändert`. Nur sichtbare Zeilen zeichnen (virtualisierte Zeilen von `egui_extras`). Balken hinter dem Namen nach `bar_fraction`. Klick auf Kopf: sortiert, zweiter Klick dreht die Richtung; `% Eltern` sortiert nach der gewählten Zahl. Datei-Zeilen lassen `Dateien` und `Ordner` leer. Klick wählt, Klick auf den Pfeil klappt, Doppelklick auf einen Ordner klappt. Pfeiltasten über `key_nav`, Auswahl scrollt in Sicht. Rechtsklick-Menü genau: `Im Explorer anzeigen`, `Pfad kopieren`. Markierung hinter dem Namen in gedämpfter Farbe.

- [ ] **Step 1: Tests schreiben**

```rust
assert_eq!(marker_text(F_DIR), None);
assert_eq!(marker_text(F_DIR | F_UNREADABLE), Some("nicht lesbar"));
assert_eq!(marker_text(F_DIR | F_LINK), Some("Verknüpfung"));
assert_eq!(marker_text(F_DIR | F_ONLINE_ONLY), Some("nur online"));
// sample-Baum: bar_fraction(t, 0, Allocated) == 1.0; bar_fraction(t, 1, Allocated) ≈ 8192/12288; leerer Elternordner → 0.0
```

- [ ] **Step 2:** schlägt fehl. **Step 3:** Umsetzen. **Step 4:** grün.
- [ ] **Step 5: Von Hand prüfen** mit einem Ordner-Scan von `C:\Users\LeonKasdorf` (ohne Admin): Summen plausibel, Sortieren je Spalte, Pfeiltasten, Rechtsklick-Menü öffnet den Explorer am richtigen Eintrag und kopiert den Pfad, Scrollen ruckelfrei, nachdem per Pfeil rechts viele Ordner aufgeklappt wurden.
- [ ] **Step 6:** Commit `feat: tree table`.

### Task 13: Treemap-Anzeige (`ui/treemap_view.rs`)

**Files:** Create `src/ui/treemap_view.rs`; Modify `src/ui/mod.rs`

**Interfaces:** Consumes `treemap::{layout, hit_test, Tile, Rect}`, `TreeView`. Produces:
```rust
#[derive(Default)] pub struct TreemapCache { /* Schlüssel: (Knoten, Metric, Breite, Höhe gerundet), Flächen */ }
pub fn show(ui: &mut egui::Ui, tree: &Tree, view: &TreeView, cache: &mut TreemapCache) -> Option<NodeId>;  // angeklickter Knoten
pub fn depth_color(depth: u8, dark: bool) -> egui::Color32;   // eigene Palette, 8 Stufen, danach wiederholen
```

Zeigt den gewählten Eintrag; ist eine Datei gewählt, deren Elternordner. Aufteilung nur neu berechnen, wenn sich der Cache-Schlüssel ändert. Ordnername im Kopf der Fläche, wenn Platz ist. Mauszeiger: Tooltip `{Pfad}` und in der zweiten Zeile `{formatierter Wert}`. Klick: `view.reveal` in `ui/mod.rs`, Tabelle scrollt dorthin. Treemap in einem in der Höhe verschiebbaren unteren Bereich; Schalter `Treemap` in der Leiste blendet sie aus.

- [ ] **Step 1: Test schreiben**

```rust
assert_ne!(depth_color(0, false), depth_color(1, false));
assert_eq!(depth_color(0, false), depth_color(8, false));
/* Cache: zweimal derselbe Schlüssel → layout wird einmal gerechnet (Zähler im Cache, `pub fn computations(&self) -> u32`);
   anderer Knoten oder andere Größe → zweimal */
```

- [ ] **Step 2:** schlägt fehl. **Step 3:** Umsetzen. **Step 4:** grün.
- [ ] **Step 5: Von Hand prüfen:** Treemap folgt der Auswahl, Tooltip stimmt mit der Tabelle überein, Klick wählt und scrollt, Umschalten Belegt/Größe ändert die Flächen, Trennlinie verschiebbar, Ausblenden geht, Fenster vergrößern ruckelt nicht.
- [ ] **Step 6:** Commit `feat: treemap view`.

### Task 14: Abnahme

**Files:** keine neuen; Korrekturen nach Bedarf.

- [ ] **Step 1:** `cargo test` → alles grün. `cargo build --release` → ohne Warnungen aus eigenem Code.
- [ ] **Step 2:** Größe von `target\release\platzhirsch.exe` messen. Bestanden bei ≤ 15 MB.
- [ ] **Step 3: Mit Leon am echten Laufwerk** (UAC ankündigen). `platzhirsch.exe` starten, C: wählen, `Scannen`, `Als Administrator neu starten`, UAC bestätigen. Prüfen und notieren:
  - Scan startet nach dem Neustart von selbst; Zeit vom Klick bis zum Baum ≤ 10 s (Statuszeile).
  - Statuszeile zeigt `MFT-Scan`; Anzahl übersprungener Einträge notieren.
  - Summe „Belegt" der Wurzel gegen `Frei … von …`: Abweichung ≤ 2 %.
  - Alles aufklappen in `C:\Users`, scrollen: flüssig. Arbeitsspeicher im Task-Manager ≤ 400 MB.
  - Treemap-Klick, Rechtsklick-Menü, Sortieren, Umschalter funktionieren.
  - F: scannen: läuft durch.
  - UAC einmal ablehnen: Programm bleibt offen, Hinweis bleibt stehen.
  - `Langsam scannen` auf C: starten und abbrechen: stoppt binnen 1 s.
- [ ] **Step 4:** Ergebnis als Liste an Leon: jeder Zielwert mit gemessener Zahl und „erreicht" oder „verfehlt". Verfehlte Werte nicht beschönigen.
- [ ] **Step 5:** Commit `chore: v1 acceptance fixes` (falls Korrekturen nötig waren).

---

## Sprachänderung (Task 11b)

Am 21.11.2026 hat Leon während der Umsetzung entschieden: **Die App ist komplett englisch**, ebenso das Changelog. Dieser Abschnitt ersetzt alle deutschen Texte, das deutsche Zahlenformat und die deutschen `bench`-Schlüssel in den Aufgaben oben. Er ist zugleich der Auftrag für Task 11b (nach Task 11, vor Task 12) und die Vorgabe für die Texte in Task 12 bis 14.

### Number and date format (`format.rs`)

Thousands separator `,`, decimal separator `.`, date `DD/MM/YYYY`.

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
assert_eq!(format_percent(5, 0), "0.0 %");
assert_eq!(format_ymd(2026, 11, 21), "21/11/2026");
assert_eq!(format_date(UNIX_EPOCH_FT + 1_795_262_400 * 10_000_000), "21/11/2026");
```

### UI texts

| Was (German) | Now (English) |
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

Status bar examples (replace the ones in Task 11):

```rust
assert_eq!(s, "Free: 1.3 TB of 3.7 TB | 1,683,667 files | MFT scan, 4.2 s | 3 skipped");
assert_eq!(s, "12 files | Folder scan, allocated approximate, 0.8 s | 2 unreadable | 1 online-only");
```

### Messages and diagnostics

- `ScanError` `Display` and every other message a user can see: short English sentences, e.g. `Access denied (administrator rights required)`, `Not an NTFS volume`, `MFT incomplete: found {found} of {expected} bytes`, `Read error: {error}`, `Not a folder`.
- `bench` keys: `method`, `duration_s`, `entries`, `files`, `folders`, `size`, `allocated`, `skipped`, `unreadable`, `online_only`, `volume_used`, `deviation_percent`, `memory_after_scan_mb`, `memory_peak_mb`, `top:`, `error:`. Values of `method`: `MFT`, `Walk`. `bench compare`: `mft_files`, `mft_folders`, `mft_size`, `mft_allocated`, `walk_files`, `walk_folders`, `walk_size`, `walk_allocated`, `equal: yes|no`, `error: folder not found in MFT tree`.
- Comments, doc comments, test names and assertion messages in `src/` and `tests/`: English.
- The working name `Platzhirsch` (window title, package, exe) stays.
