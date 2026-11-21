//! MFT scan over an abstract, read-only volume.
//!
//! Pipeline: a reader thread reads the MFT in [`CHUNK_BYTES`] chunks along
//! its runs and hands them over a bounded channel; the calling thread parses
//! each chunk's records in parallel (rayon) into one compact [`Slot`] per
//! record. After the last chunk, extension records are merged into their
//! base records and the tree is built. The MFT is read in a single pass.
use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::Instant;

use rayon::prelude::*;

use super::ntfs::NtfsError;
use super::ntfs::boot::{BootSector, parse_boot_sector};
use super::ntfs::record::{
    AttrListLoc, FIXUP_BLOCK_SIZE, apply_fixups, header_in_use, parse_attr_list, parse_record,
};
use super::ntfs::runs::Run;
use super::{Progress, ScanError, ScanMethod, ScanResult};
use crate::model::{F_DIR, F_LINK, NO_PARENT, NodeId, TreeBuilder};

/// Read-only access to a volume. There is deliberately no way to write.
pub trait VolumeReader: Send {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> std::io::Result<()>;
}

/// Where the MFT lives on the volume.
pub struct MftLayout {
    /// Extents in VCN order; none of them is sparse.
    pub runs: Vec<Run>,
    /// Real size of the unnamed `$DATA` of record 0.
    pub byte_len: u64,
    pub record_count: u64,
}

/// Bytes read from the volume per chunk. A multiple of every valid record
/// size (at most 64 KiB) and of every valid cluster size (at most 2 MiB).
const CHUNK_BYTES: u64 = 8 << 20;
/// Chunks read ahead of the parser.
const CHUNKS_IN_FLIGHT: usize = 2;
/// Enough for every sector size; the boot sector is the first 512 bytes.
const BOOT_READ_LEN: usize = 4096;
const ROOT_RECORD: usize = 5;
const ATTR_DATA: u32 = 0x80;
/// NTFS caps `$ATTRIBUTE_LIST` at 256 KiB; anything far beyond is hostile.
const MAX_ATTR_LIST_LEN: u64 = 1 << 20;
/// Record numbers must fit a `u32`; `u32::MAX` marks "no such record".
const NO_RECORD: u32 = u32::MAX;

fn malformed(what: &'static str) -> ScanError {
    ScanError::Ntfs(NtfsError::Malformed(what))
}

/// One contiguous piece of the MFT on the volume.
struct Extent {
    mft_start: u64,
    vol_start: u64,
    len: u64,
}

/// Maps MFT byte offsets to volume offsets.
#[derive(Default)]
struct MftMap {
    runs: Vec<Run>,
    extents: Vec<Extent>,
    clusters: u64,
    end: u64,
}

impl MftMap {
    fn push(&mut self, run: Run, cluster: u64) -> Result<(), ScanError> {
        let lcn = run.lcn.ok_or(malformed("sparse run inside the MFT"))?;
        let overflow = || malformed("MFT run out of range");
        let vol_start = lcn.checked_mul(cluster).ok_or_else(overflow)?;
        let len = run.clusters.checked_mul(cluster).ok_or_else(overflow)?;
        vol_start.checked_add(len).ok_or_else(overflow)?;
        self.clusters = self.clusters.checked_add(run.clusters).ok_or_else(overflow)?;
        let mft_start = self.end;
        self.end = self.end.checked_add(len).ok_or_else(overflow)?;
        self.runs.push(run);
        if len > 0 {
            self.extents.push(Extent { mft_start, vol_start, len });
        }
        Ok(())
    }

    /// Appends `$DATA` extents (start VCN, runs); they must continue the MFT
    /// exactly where it ends so far.
    fn append(&mut self, mut segments: Vec<(u64, Vec<Run>)>, cluster: u64) -> Result<(), ScanError> {
        segments.sort_by_key(|s| s.0);
        for (vcn, runs) in segments {
            if vcn != self.clusters {
                return Err(malformed("MFT extents overlap or leave a gap"));
            }
            for run in runs {
                self.push(run, cluster)?;
            }
        }
        Ok(())
    }

    /// Fills `buf` from MFT byte `mft_off` on.
    fn read<R: VolumeReader>(&self, r: &mut R, sector: u64, mft_off: u64, buf: &mut [u8]) -> Result<(), ScanError> {
        let end = mft_off.checked_add(buf.len() as u64).ok_or(malformed("MFT offset out of range"))?;
        if end > self.end {
            return Err(malformed("read beyond the known MFT"));
        }
        let mut idx = self.extents.partition_point(|e| e.mft_start + e.len <= mft_off);
        let mut pos = 0usize;
        while pos < buf.len() {
            let e = self.extents.get(idx).ok_or(malformed("read beyond the known MFT"))?;
            let cur = mft_off + pos as u64;
            let within = cur - e.mft_start; // extents are contiguous and non-empty
            let n = (e.len - within).min((buf.len() - pos) as u64) as usize;
            read_aligned(r, e.vol_start + within, &mut buf[pos..pos + n], sector)?;
            pos += n;
            idx += 1;
        }
        Ok(())
    }
}

/// Reads `buf.len()` bytes at `offset`, issuing only sector-aligned reads.
fn read_aligned<R: VolumeReader>(r: &mut R, offset: u64, buf: &mut [u8], sector: u64) -> Result<(), ScanError> {
    let start = offset / sector * sector;
    let end = offset
        .checked_add(buf.len() as u64)
        .and_then(|e| e.checked_next_multiple_of(sector))
        .ok_or(malformed("read offset out of range"))?;
    if start == offset && end - start == buf.len() as u64 {
        return r.read_at(offset, buf).map_err(ScanError::Io);
    }
    let mut tmp = vec![0u8; (end - start) as usize];
    r.read_at(start, &mut tmp).map_err(ScanError::Io)?;
    let skip = (offset - start) as usize;
    buf.copy_from_slice(&tmp[skip..skip + buf.len()]);
    Ok(())
}

/// Reads record `record_no` through the runs known so far and parses it with its runs.
fn read_record<R: VolumeReader>(
    r: &mut R,
    map: &MftMap,
    boot: &BootSector,
    record_no: u64,
) -> Result<super::ntfs::record::RecordInfo, ScanError> {
    let rs = u64::from(boot.record_size);
    let mft_off = record_no.checked_mul(rs).ok_or(malformed("record number out of range"))?;
    if mft_off.checked_add(rs).is_none_or(|end| end > map.end) {
        return Err(malformed("attribute list points outside the known MFT"));
    }
    let mut rec = vec![0u8; boot.record_size as usize];
    map.read(r, u64::from(boot.bytes_per_sector), mft_off, &mut rec)?;
    apply_fixups(&mut rec, FIXUP_BLOCK_SIZE).map_err(ScanError::Ntfs)?;
    parse_record(&rec, record_no, true).map_err(ScanError::Ntfs)
}

fn check_geometry(boot: &BootSector) -> Result<(), ScanError> {
    let rs = boot.record_size;
    if !rs.is_power_of_two() || !(512..=65536).contains(&rs) {
        return Err(malformed("record size out of range"));
    }
    let cs = boot.cluster_size;
    if !cs.is_power_of_two() || !(512..=2 << 20).contains(&cs) {
        return Err(malformed("cluster size out of range"));
    }
    if boot.bytes_per_sector == 0 {
        return Err(malformed("bytes per sector is zero"));
    }
    Ok(())
}

struct Located {
    map: MftMap,
    byte_len: u64,
    /// Records holding attributes of `$MFT` itself. Their base reference is
    /// record 0, which after masking looks like "no base", so the scan must
    /// be told they are extension records.
    ext_records: Vec<u64>,
}

fn locate_map<R: VolumeReader>(r: &mut R, boot: &BootSector) -> Result<Located, ScanError> {
    check_geometry(boot)?;
    let cluster = u64::from(boot.cluster_size);
    let sector = u64::from(boot.bytes_per_sector);

    let start = boot.mft_lcn.checked_mul(cluster).ok_or(malformed("MFT LCN out of range"))?;
    let mut rec = vec![0u8; boot.record_size as usize];
    read_aligned(r, start, &mut rec, sector)?;
    apply_fixups(&mut rec, FIXUP_BLOCK_SIZE).map_err(ScanError::Ntfs)?;
    let info = parse_record(&rec, 0, true).map_err(ScanError::Ntfs)?;
    let byte_len = info.size.ok_or(malformed("$MFT has no unnamed $DATA"))?;

    let mut map = MftMap::default();
    map.append(info.data_runs, cluster)?;

    let mut ext_records = Vec::new();
    if let Some(loc) = info.attr_list {
        let list = match loc {
            AttrListLoc::Resident(bytes) => bytes,
            AttrListLoc::NonResident { runs, len } => read_attr_list(r, &runs, len, cluster, sector)?,
        };
        let mut entries = parse_attr_list(&list).map_err(ScanError::Ntfs)?;
        entries.retain(|e| e.record_no != 0);
        ext_records = entries.iter().map(|e| e.record_no).collect();
        ext_records.sort_unstable();
        ext_records.dedup();

        entries.retain(|e| e.attr_type == ATTR_DATA && e.name_len == 0);
        entries.sort_by_key(|e| e.start_vcn);
        let mut seen = HashSet::new();
        for e in entries {
            if !seen.insert(e.record_no) {
                continue; // duplicate entry: its extents are already in
            }
            let ext = read_record(r, &map, boot, e.record_no)?;
            map.append(ext.data_runs, cluster)?;
        }
    }

    if map.end < byte_len {
        return Err(ScanError::MftIncomplete { found_bytes: map.end, expected_bytes: byte_len });
    }
    Ok(Located { map, byte_len, ext_records })
}

/// Reads a non-resident `$ATTRIBUTE_LIST` and cuts it to its length.
fn read_attr_list<R: VolumeReader>(
    r: &mut R,
    runs: &[Run],
    len: u64,
    cluster: u64,
    sector: u64,
) -> Result<Vec<u8>, ScanError> {
    if len > MAX_ATTR_LIST_LEN {
        return Err(malformed("attribute list too long"));
    }
    let mut out = vec![0u8; len as usize];
    let mut pos = 0usize;
    for run in runs {
        if pos == out.len() {
            break;
        }
        let lcn = run.lcn.ok_or(malformed("sparse attribute list"))?;
        let vol = lcn.checked_mul(cluster).ok_or(malformed("attribute list run out of range"))?;
        let run_len = run.clusters.saturating_mul(cluster);
        let n = run_len.min((out.len() - pos) as u64) as usize;
        read_aligned(r, vol, &mut out[pos..pos + n], sector)?;
        pos += n;
    }
    if pos < out.len() {
        return Err(malformed("attribute list runs shorter than its length"));
    }
    Ok(out)
}

/// Finds the MFT: record 0, plus the extension records its attribute list names.
pub fn locate_mft<R: VolumeReader>(r: &mut R, boot: &BootSector) -> Result<MftLayout, ScanError> {
    let Located { map, byte_len, .. } = locate_map(r, boot)?;
    Ok(MftLayout {
        runs: map.runs,
        byte_len,
        record_count: byte_len / u64::from(boot.record_size),
    })
}

const KIND_EMPTY: u8 = 0;
/// `FILE` signature, but fixups or parsing failed.
const KIND_BAD: u8 = 1;
const KIND_BASE: u8 = 2;
const KIND_EXT: u8 = 3;
/// Internal flag bits next to `F_DIR` and `F_LINK`.
const HAS_SIZE: u8 = 0x40;
const HAS_NAME: u8 = 0x80;

/// The compact per-record result (40 bytes); names live in one shared buffer.
#[derive(Clone, Copy)]
struct Slot {
    size: u64,
    allocated: u64,
    mtime: i64,
    /// Record number of the parent directory, or `NO_RECORD`.
    parent: u32,
    /// Base record of an extension record, or `NO_RECORD`.
    base: u32,
    name_off: u32,
    name_len: u16,
    kind: u8,
    flags: u8,
}

const EMPTY: Slot = Slot {
    size: 0,
    allocated: 0,
    mtime: 0,
    parent: NO_RECORD,
    base: NO_RECORD,
    name_off: 0,
    name_len: 0,
    kind: KIND_EMPTY,
    flags: 0,
};

fn record_ref(no: u64) -> u32 {
    u32::try_from(no).unwrap_or(NO_RECORD)
}

/// Parses one raw record in place. The name is returned separately.
fn parse_slot(rec: &mut [u8], record_no: u64) -> (Slot, Option<String>) {
    if rec.get(0..4) != Some(b"FILE") {
        return (EMPTY, None);
    }
    // A free record holds nothing, whatever its fixups look like (a torn
    // write to a free record is not a lost file).
    if !header_in_use(rec) {
        return (EMPTY, None);
    }
    let bad = (Slot { kind: KIND_BAD, ..EMPTY }, None);
    if apply_fixups(rec, FIXUP_BLOCK_SIZE).is_err() {
        return bad;
    }
    let Ok(info) = parse_record(rec, record_no, false) else {
        return bad;
    };
    if !info.in_use {
        return (EMPTY, None);
    }
    let mut flags = 0;
    if info.is_dir {
        flags |= F_DIR;
    }
    if info.is_link {
        flags |= F_LINK;
    }
    if info.size.is_some() {
        flags |= HAS_SIZE;
    }
    let (parent, name) = match info.name {
        Some(n) => {
            flags |= HAS_NAME;
            (record_ref(n.parent), Some(n.name))
        }
        None => (NO_RECORD, None),
    };
    let (kind, base) = if info.base_record == 0 {
        (KIND_BASE, NO_RECORD)
    } else {
        (KIND_EXT, record_ref(info.base_record))
    };
    let slot = Slot {
        size: info.size.unwrap_or(0),
        allocated: info.allocated,
        mtime: info.mtime,
        parent,
        base,
        kind,
        flags,
        ..EMPTY
    };
    (slot, name)
}

/// Reads the MFT chunk by chunk and parses it into one slot per record.
fn read_slots<R: VolumeReader>(
    r: &mut R,
    map: &MftMap,
    boot: &BootSector,
    record_count: u64,
    names_limit: usize,
    progress: &Progress,
) -> Result<(Vec<Slot>, String), ScanError> {
    let rs = boot.record_size as usize;
    let sector = u64::from(boot.bytes_per_sector);
    let total_bytes = record_count * rs as u64; // <= byte_len, checked by the caller
    let cancelled = || progress.cancel.load(Ordering::Relaxed);

    std::thread::scope(|s| {
        let (tx, rx) = mpsc::sync_channel::<Result<(u64, Vec<u8>), ScanError>>(CHUNKS_IN_FLIGHT);
        let (free_tx, free_rx) = mpsc::channel::<Vec<u8>>();
        s.spawn(move || {
            let mut off = 0u64;
            while off < total_bytes && !cancelled() {
                let len = CHUNK_BYTES.min(total_bytes - off) as usize;
                let mut buf = free_rx.try_recv().unwrap_or_default();
                buf.resize(len, 0);
                let msg = map.read(r, sector, off, &mut buf).map(|()| (off / rs as u64, buf));
                let failed = msg.is_err();
                if tx.send(msg).is_err() || failed {
                    break;
                }
                off += len as u64;
            }
        });

        let mut slots: Vec<Slot> = Vec::new();
        let mut names = String::new();
        for msg in rx.iter() {
            if cancelled() {
                return Err(ScanError::Cancelled);
            }
            let (first, mut buf) = msg?;
            let parsed: Vec<(Slot, Option<String>)> = buf
                .par_chunks_mut(rs)
                .enumerate()
                .map(|(i, rec)| parse_slot(rec, first + i as u64))
                .collect();
            let _ = free_tx.send(buf);
            slots.try_reserve(parsed.len()).map_err(|_| malformed("MFT too large for memory"))?;
            for (mut slot, name) in parsed {
                if let Some(name) = name {
                    if names.len() + name.len() > names_limit {
                        return Err(malformed("file names exceed 4 GiB"));
                    }
                    slot.name_off = names.len() as u32;
                    slot.name_len = name.len() as u16; // at most 255 UTF-16 units = 765 bytes
                    names.push_str(&name);
                }
                slots.push(slot);
            }
            progress.done.store(slots.len() as u64, Ordering::Relaxed);
        }
        if cancelled() {
            return Err(ScanError::Cancelled);
        }
        if slots.len() as u64 != record_count {
            let found_bytes = slots.len() as u64 * rs as u64;
            return Err(ScanError::MftIncomplete { found_bytes, expected_bytes: total_bytes });
        }
        Ok((slots, names))
    })
}

/// Scans a whole NTFS volume through its MFT. The root gets `root_name`.
pub fn scan_mft_from<R: VolumeReader>(r: &mut R, root_name: &str, progress: &Progress) -> Result<ScanResult, ScanError> {
    let started = Instant::now();
    let mut boot_buf = vec![0u8; BOOT_READ_LEN];
    r.read_at(0, &mut boot_buf).map_err(ScanError::Io)?;
    let boot = match parse_boot_sector(&boot_buf) {
        Ok(b) => b,
        Err(NtfsError::BadSignature) => return Err(ScanError::NotNtfs),
        Err(e) => return Err(ScanError::Ntfs(e)),
    };
    let Located { map, byte_len, ext_records } = locate_map(r, &boot)?;
    let record_count = byte_len / u64::from(boot.record_size);
    if record_count >= u64::from(NO_RECORD) {
        return Err(malformed("too many MFT records"));
    }
    progress.total.store(record_count, Ordering::Relaxed);
    progress.done.store(0, Ordering::Relaxed);

    // The tree's name buffer must stay below 4 GiB, including `root_name`.
    let names_limit = (u32::MAX as usize).saturating_sub(root_name.len());
    let (mut slots, names) = read_slots(r, &map, &boot, record_count, names_limit, progress)?;
    let n = slots.len();
    let mut skipped = 0u64;

    for no in ext_records {
        if let Some(s) = usize::try_from(no).ok().and_then(|i| slots.get_mut(i))
            && s.kind == KIND_BASE
        {
            s.kind = KIND_EXT;
            s.base = 0;
        }
    }

    // Merge extension records into their base records.
    for i in 0..n {
        let ext = slots[i];
        if ext.kind == KIND_BAD {
            skipped += 1;
        }
        if ext.kind != KIND_EXT {
            continue;
        }
        let Some(base) = slots.get_mut(ext.base as usize).filter(|b| b.kind == KIND_BASE) else {
            skipped += 1; // its clusters belong to no file we know
            continue;
        };
        base.allocated = base.allocated.saturating_add(ext.allocated);
        base.flags |= ext.flags & F_LINK;
        if base.flags & HAS_SIZE == 0 && ext.flags & HAS_SIZE != 0 {
            base.size = ext.size;
            base.flags |= HAS_SIZE;
        }
        if base.flags & HAS_NAME == 0 && ext.flags & HAS_NAME != 0 {
            base.name_off = ext.name_off;
            base.name_len = ext.name_len;
            base.parent = ext.parent;
            base.flags |= HAS_NAME;
        }
    }

    let root_ok = slots.get(ROOT_RECORD).is_some_and(|s| s.kind == KIND_BASE && s.flags & F_DIR != 0);
    if !root_ok {
        return Err(malformed("root directory record missing"));
    }

    // Node ids in record order: the root plus every named base record.
    let mut node_of = vec![NO_PARENT; n];
    let mut entries: NodeId = 0;
    let mut name_bytes = root_name.len();
    for (i, s) in slots.iter().enumerate() {
        if s.kind != KIND_BASE {
            continue;
        }
        if i == ROOT_RECORD || s.flags & HAS_NAME != 0 {
            node_of[i] = entries;
            entries += 1;
            name_bytes += usize::from(s.name_len);
        } else if s.size > 0 || s.allocated > 0 {
            // Data (extension records included) that belongs to no named
            // file. A nameless record that holds nothing, such as a reserved
            // system record, is no entry and loses nothing.
            skipped += 1;
        }
    }

    let mut b = TreeBuilder::with_capacity(entries as usize, name_bytes);
    for (i, s) in slots.iter().enumerate() {
        if node_of[i] == NO_PARENT {
            continue;
        }
        let (name, parent) = if i == ROOT_RECORD {
            (root_name, NO_PARENT)
        } else {
            let start = s.name_off as usize;
            let name = names.get(start..start + usize::from(s.name_len)).unwrap_or("");
            (name, node_of.get(s.parent as usize).copied().unwrap_or(NO_PARENT))
        };
        let size = if s.flags & HAS_SIZE != 0 { s.size } else { 0 };
        b.push(name, parent, size, s.allocated, s.mtime, s.flags & (F_DIR | F_LINK));
    }
    let root = node_of[ROOT_RECORD];
    drop((slots, names, node_of));
    let (tree, stats) = b.finish(root);
    skipped += stats.orphaned;

    Ok(ScanResult {
        tree,
        method: ScanMethod::Mft,
        duration: started.elapsed(),
        skipped,
        unreadable: 0,
        online_only: 0,
    })
}

/// Scans the NTFS volume `drive` (e.g. `'c'`) through its MFT. Needs admin rights.
pub fn scan_mft(drive: char, progress: &Progress) -> Result<ScanResult, ScanError> {
    let mut volume = crate::win::RawVolume::open(drive).map_err(|e| match e.kind() {
        std::io::ErrorKind::PermissionDenied => ScanError::AccessDenied,
        _ => ScanError::Io(e),
    })?;
    let root_name = format!("{}:", drive.to_ascii_uppercase());
    scan_mft_from(&mut volume, &root_name, progress)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::ScanMethod;
    use crate::scan::ntfs::NtfsError;
    use crate::scan::ntfs::boot::parse_boot_sector;
    use crate::scan::ntfs::testkit::{FakeVolume, RecordBuilder, fake_volume};
    use std::sync::atomic::Ordering;

    const MFT_START: u64 = 4 * 4096;

    fn image() -> FakeVolume {
        // Record 1024 B, MFT at LCN 4, 8 clusters = 32 records
        let mut broken = RecordBuilder::new(1024).file_name(5, "broken", 1).build(512);
        broken[510] ^= 0xFF;
        fake_volume(
            1024,
            &[(4, 8)],
            vec![
                (0, RecordBuilder::new(1024).file_name(5, "$MFT", 3)
                        .data_nonresident("", 0, &[0x11, 0x08, 0x04, 0x00], 32768, 32768, 0, 0).build(512)),
                (5, RecordBuilder::new(1024).dir().file_name(5, ".", 3).std_info(1).build(512)),
                (8, RecordBuilder::new(1024).file_name(5, "$BadClus", 3).data_resident("", 0)
                        .data_nonresident("$Bad", 0, &[0x01, 0x10, 0x00], 1 << 40, 1 << 40, 0, 0).build(512)),
                (24, RecordBuilder::new(1024).dir().file_name(5, "docs", 1).std_info(100).build(512)),
                (25, RecordBuilder::new(1024).file_name(24, "a.txt", 1).std_info(900)
                        .data_nonresident("", 0, &[0x21, 0x02, 0x40, 0x00, 0x00], 8192, 5000, 0, 0).build(512)),
                (26, RecordBuilder::new(1024).file_name(24, "tiny", 1).data_resident("", 100).build(512)),
                (27, RecordBuilder::new(1024).file_name(5, "frag.bin", 1).build(512)),
                (28, RecordBuilder::new(1024).base(27)
                        .data_nonresident("", 0, &[0x21, 0x02, 0x50, 0x00, 0x00], 1_003_520, 1_000_000, 0, 0).build(512)),
                (29, RecordBuilder::new(1024).file_name(5, "hl1", 1).file_name(24, "hl2", 1)
                        .data_nonresident("", 0, &[0x21, 0x01, 0x60, 0x00, 0x00], 4096, 300, 0, 0).build(512)),
                (30, RecordBuilder::new(1024).file_name(5, "wof.dll", 1)
                        .data_nonresident("", 0, &[0x01, 0x05, 0x00], 20480, 20000, 0x8000, 0)
                        .data_nonresident("WofCompressedData", 0, &[0x21, 0x02, 0x70, 0x00, 0x00], 8192, 7000, 0, 0)
                        .build(512)),
                (31, broken),
            ],
        )
    }

    fn child(t: &crate::model::Tree, parent: crate::model::NodeId, name: &str) -> Option<crate::model::NodeId> {
        t.children(parent).iter().copied().find(|&c| t.name(c) == name)
    }

    fn root_dir() -> Vec<u8> {
        RecordBuilder::new(1024).dir().file_name(5, ".", 3).std_info(1).build(512)
    }

    #[test]
    fn end_to_end() {
        let progress = Progress::default();
        let r = scan_mft_from(&mut image(), "C:", &progress).unwrap();
        let t = &r.tree;
        let root = t.root();
        assert_eq!(t.name(root), "C:");
        assert_eq!(t.size(root), 1_058_168);
        assert_eq!(t.allocated(root), 1_056_768);
        assert_eq!((t.file_count(root), t.dir_count(root)), (7, 1));
        assert_eq!(r.skipped, 1);
        assert_eq!(r.method, ScanMethod::Mft);
        assert_eq!((r.unreadable, r.online_only), (0, 0));
        let docs = child(t, root, "docs").expect("docs");
        assert_eq!((t.size(docs), t.file_count(docs), t.mtime(docs)), (5100, 2, 900)); // hl2 is not counted here
        assert_eq!(progress.total.load(Ordering::Relaxed), 32);
        assert_eq!(progress.done.load(Ordering::Relaxed), 32);
    }

    const RUNS: &[u8] = &[0x21, 0x02, 0x40, 0x00, 0x00];

    /// Scans a volume holding `$MFT` (record 0), the root (record 5) and `extra`.
    fn scan_records(extra: Vec<(u64, Vec<u8>)>) -> ScanResult {
        let mut records = vec![
            (0, RecordBuilder::new(1024).file_name(5, "$MFT", 3)
                    .data_nonresident("", 0, &[0x11, 0x08, 0x04, 0x00], 32768, 32768, 0, 0).build(512)),
            (5, root_dir()),
        ];
        records.extend(extra);
        scan_mft_from(&mut fake_volume(1024, &[(4, 8)], records), "C:", &Progress::default()).unwrap()
    }

    #[test]
    fn nameless_empty_record_is_not_skipped() {
        // like the reserved records 12-15 of a real volume: in use, no name, no data
        let r = scan_records(vec![
            (12, RecordBuilder::new(1024).std_info(1).build(512)),
            (13, RecordBuilder::new(1024).std_info(1).data_resident("", 0).build(512)),
        ]);
        assert_eq!(r.skipped, 0);
        assert_eq!(r.tree.len(), 2); // the root and $MFT
    }

    #[test]
    fn nameless_record_with_data_is_skipped() {
        let r = scan_records(vec![
            (20, RecordBuilder::new(1024).data_nonresident("", 0, RUNS, 8192, 5000, 0, 0).build(512)),
            (21, RecordBuilder::new(1024).data_resident("", 100).build(512)),
        ]);
        assert_eq!(r.skipped, 2);
    }

    #[test]
    fn nameless_record_with_data_in_extension_is_skipped() {
        let r = scan_records(vec![
            (20, RecordBuilder::new(1024).std_info(1).build(512)),
            (21, RecordBuilder::new(1024).base(20).data_nonresident("", 0, RUNS, 8192, 5000, 0, 0).build(512)),
        ]);
        assert_eq!(r.skipped, 1);
    }

    #[test]
    fn free_record_with_bad_fixup_is_not_skipped() {
        let mut torn = RecordBuilder::new(1024).not_in_use().file_name(5, "gone", 1).build(512);
        torn[1022] ^= 0xFF;
        let r = scan_records(vec![(20, torn)]);
        assert_eq!(r.skipped, 0);
    }

    #[test]
    fn used_record_with_bad_fixup_is_skipped() {
        let mut torn = RecordBuilder::new(1024).file_name(5, "torn", 1).build(512);
        torn[1022] ^= 0xFF;
        let r = scan_records(vec![(20, torn)]);
        assert_eq!(r.skipped, 1);
        assert!(child(&r.tree, r.tree.root(), "torn").is_none());
    }

    #[test]
    fn extension_record_before_its_base() {
        let r = scan_records(vec![
            (20, RecordBuilder::new(1024).base(21).data_nonresident("", 0, RUNS, 8192, 5000, 0, 0).build(512)),
            (21, RecordBuilder::new(1024).file_name(5, "late.bin", 1).build(512)),
        ]);
        let t = &r.tree;
        let late = child(t, t.root(), "late.bin").expect("late.bin");
        assert_eq!((t.size(late), t.allocated(late)), (5000, 8192));
        assert_eq!(t.file_count(t.root()), 2);
        assert_eq!(r.skipped, 0);
    }

    #[test]
    fn name_only_in_extension_record() {
        let r = scan_records(vec![
            (20, RecordBuilder::new(1024).std_info(7).data_nonresident("", 0, RUNS, 8192, 5000, 0, 0).build(512)),
            (21, RecordBuilder::new(1024).base(20).file_name(5, "named.bin", 1).build(512)),
        ]);
        let t = &r.tree;
        let named = child(t, t.root(), "named.bin").expect("named.bin");
        assert_eq!((t.size(named), t.allocated(named), t.mtime(named)), (5000, 8192, 7));
        assert_eq!(t.file_count(t.root()), 2);
        assert_eq!(r.skipped, 0);
    }

    #[test]
    fn extension_record_with_invalid_base_is_skipped_once() {
        // record 20 is empty, so the extension belongs to no file
        let r = scan_records(vec![
            (21, RecordBuilder::new(1024).base(20).data_nonresident("", 0, RUNS, 8192, 5000, 0, 0).build(512)),
        ]);
        assert_eq!(r.skipped, 1);
        assert_eq!(r.tree.len(), 2);
    }

    #[test]
    fn link_flag_from_extension_record() {
        let r = scan_records(vec![
            (20, RecordBuilder::new(1024).dir().file_name(5, "junction", 1).build(512)),
            (21, RecordBuilder::new(1024).base(20).reparse(0xA000_0003).build(512)),
        ]);
        let t = &r.tree;
        let j = child(t, t.root(), "junction").expect("junction");
        assert_ne!(t.flags(j) & F_LINK, 0);
        assert_ne!(t.flags(j) & F_DIR, 0);
    }

    #[test]
    fn fragmented_mft_via_attr_list() {
        let mut v = fake_volume(
            1024,
            &[(4, 4), (40, 4)],
            vec![
                (0, RecordBuilder::new(1024).file_name(5, "$MFT", 3)
                        .attr_list_resident(&[(0x80, 0, 0), (0x80, 4, 15)])
                        .data_nonresident("", 0, &[0x11, 0x04, 0x04, 0x00], 32768, 32768, 0, 0).build(512)),
                (5, root_dir()),
                (15, RecordBuilder::new(1024).base(0)
                        .data_nonresident("", 4, &[0x11, 0x04, 0x28, 0x00], 0, 0, 0, 0).build(512)),
                (20, RecordBuilder::new(1024).file_name(5, "far.txt", 1).data_resident("", 10).build(512)),
            ],
        );
        let boot = parse_boot_sector(&v.bytes[..512]).unwrap();
        let layout = locate_mft(&mut v, &boot).unwrap();
        assert_eq!(
            layout.runs,
            vec![Run { lcn: Some(4), clusters: 4 }, Run { lcn: Some(40), clusters: 4 }]
        );
        assert_eq!((layout.byte_len, layout.record_count), (32768, 32));

        let r = scan_mft_from(&mut v, "D:", &Progress::default()).unwrap();
        let far = child(&r.tree, r.tree.root(), "far.txt").expect("far.txt");
        assert_eq!(r.tree.size(far), 10);
        assert_eq!(r.skipped, 0);
    }

    #[test]
    fn truncated_mft_is_error() {
        let mut v = fake_volume(
            1024,
            &[(4, 8)],
            vec![
                (0, RecordBuilder::new(1024).file_name(5, "$MFT", 3)
                        .data_nonresident("", 0, &[0x11, 0x08, 0x04, 0x00], 65536, 65536, 0, 0).build(512)),
                (5, root_dir()),
            ],
        );
        assert!(matches!(
            scan_mft_from(&mut v, "C:", &Progress::default()),
            Err(ScanError::MftIncomplete { found_bytes: 32768, expected_bytes: 65536 })
        ));
    }

    #[test]
    fn cancel_stops() {
        let progress = Progress::default();
        progress.cancel.store(true, Ordering::Relaxed);
        assert!(matches!(scan_mft_from(&mut image(), "C:", &progress), Err(ScanError::Cancelled)));
    }

    #[test]
    fn read_error_is_reported() {
        let mut v = image();
        v.fail_at = Some(MFT_START + 20 * 1024 + 100);
        assert!(matches!(scan_mft_from(&mut v, "C:", &Progress::default()), Err(ScanError::Io(_))));
    }

    #[test]
    fn not_ntfs() {
        let mut v = image();
        v.bytes[0x03..0x0B].copy_from_slice(b"EXFAT   ");
        assert!(matches!(scan_mft_from(&mut v, "C:", &Progress::default()), Err(ScanError::NotNtfs)));
    }

    #[test]
    fn absurd_geometry_is_malformed() {
        // record size 2^1 and 2^20 bytes, cluster size 2^30 bytes
        for (offset, value) in [(0x40, 0xFFu8), (0x40, 0xEC), (0x0D, 0xEB)] {
            let mut v = image();
            v.bytes[offset] = value;
            assert!(
                matches!(scan_mft_from(&mut v, "C:", &Progress::default()), Err(ScanError::Ntfs(NtfsError::Malformed(_)))),
                "byte {offset:#x} = {value:#x}"
            );
        }
    }

    #[test]
    fn attr_list_outside_known_runs_is_malformed() {
        // the extension record 20 lies in the second piece, which only it describes
        let mut v = fake_volume(
            1024,
            &[(4, 4), (40, 4)],
            vec![
                (0, RecordBuilder::new(1024).file_name(5, "$MFT", 3)
                        .attr_list_resident(&[(0x80, 0, 0), (0x80, 4, 20), (0x80, 4, 20)])
                        .data_nonresident("", 0, &[0x11, 0x04, 0x04, 0x00], 32768, 32768, 0, 0).build(512)),
                (5, root_dir()),
            ],
        );
        assert!(matches!(
            scan_mft_from(&mut v, "C:", &Progress::default()),
            Err(ScanError::Ntfs(NtfsError::Malformed(_)))
        ));
    }

    #[test]
    fn missing_root_is_error() {
        let mut v = image();
        let at = (MFT_START + 5 * 1024) as usize;
        v.bytes[at..at + 1024].fill(0);
        assert!(matches!(
            scan_mft_from(&mut v, "C:", &Progress::default()),
            Err(ScanError::Ntfs(NtfsError::Malformed(_)))
        ));
    }

    #[test]
    fn real_volume_needs_elevation() {
        // cargo test runs without admin rights
        assert!(
            matches!(scan_mft('C', &Progress::default()), Err(ScanError::AccessDenied)) || crate::win::is_elevated()
        );
    }

    #[test]
    fn messages_are_english() {
        let e = ScanError::MftIncomplete { found_bytes: 1, expected_bytes: 2 };
        assert_eq!(e.to_string(), "MFT incomplete: found 1 of 2 bytes");
        assert_eq!(ScanError::Cancelled.to_string(), "Scan cancelled");
    }
}
