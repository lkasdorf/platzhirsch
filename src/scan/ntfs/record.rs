//! MFT file record parsing.
use super::NtfsError;
use super::runs::Run;

#[derive(Debug, Clone, PartialEq)]
pub struct NameInfo {
    pub parent: u64,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AttrListLoc {
    Resident(Vec<u8>),
    NonResident { runs: Vec<Run>, len: u64 },
}

#[derive(Debug, Clone, PartialEq)]
pub struct RecordInfo {
    pub in_use: bool,
    pub is_dir: bool,
    pub base_record: u64,
    pub name: Option<NameInfo>,
    pub mtime: i64,
    pub size: Option<u64>,
    pub allocated: u64,
    pub is_link: bool,
    pub data_runs: Vec<(u64, Vec<Run>)>,
    pub attr_list: Option<AttrListLoc>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AttrListEntry {
    pub attr_type: u32,
    pub name_len: u8,
    pub start_vcn: u64,
    pub record_no: u64,
}

const ATTR_STD_INFO: u32 = 0x10;
const ATTR_LIST: u32 = 0x20;
const ATTR_FILE_NAME: u32 = 0x30;
const ATTR_DATA: u32 = 0x80;
const ATTR_REPARSE: u32 = 0xC0;
const ATTR_END: u32 = 0xFFFF_FFFF;

const RECORD_IN_USE: u16 = 0x0001;
const RECORD_DIR: u16 = 0x0002;
const ATTR_FLAG_COMPRESSED: u16 = 0x0001;
const ATTR_FLAG_SPARSE: u16 = 0x8000;
const NAMESPACE_DOS: u8 = 2;
const REPARSE_NAME_SURROGATE: u32 = 0x2000_0000;
/// `$BadClus`: its named `$Bad` stream spans the whole volume.
const BAD_CLUS: u64 = 8;
const MFT_REF_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;

/// Size of the MFT record header up to and including the base record field.
const RECORD_HEADER_LEN: usize = 0x28;
const ATTR_HEADER_LEN: usize = 0x10;
const RESIDENT_HEADER_LEN: usize = 0x18;
const NONRESIDENT_HEADER_LEN: usize = 0x40;
const NONRESIDENT_EXT_HEADER_LEN: usize = 0x48;
const FILE_NAME_LEN: usize = 0x42;
const ATTR_LIST_ENTRY_LEN: usize = 0x1A;

fn bytes_at<const N: usize>(b: &[u8], off: usize) -> Result<[u8; N], NtfsError> {
    let end = off.checked_add(N).ok_or(NtfsError::Truncated)?;
    let s = b.get(off..end).ok_or(NtfsError::Truncated)?;
    let mut out = [0u8; N];
    out.copy_from_slice(s);
    Ok(out)
}

fn u8_at(b: &[u8], off: usize) -> Result<u8, NtfsError> {
    b.get(off).copied().ok_or(NtfsError::Truncated)
}

fn u16_at(b: &[u8], off: usize) -> Result<u16, NtfsError> {
    bytes_at::<2>(b, off).map(u16::from_le_bytes)
}

fn u32_at(b: &[u8], off: usize) -> Result<u32, NtfsError> {
    bytes_at::<4>(b, off).map(u32::from_le_bytes)
}

fn u64_at(b: &[u8], off: usize) -> Result<u64, NtfsError> {
    bytes_at::<8>(b, off).map(u64::from_le_bytes)
}

/// Returns `b[off..off + len]`, or `err` if that range is outside `b`.
fn slice<'a>(b: &'a [u8], off: usize, len: usize, err: &'static str) -> Result<&'a [u8], NtfsError> {
    off.checked_add(len)
        .and_then(|end| b.get(off..end))
        .ok_or(NtfsError::Malformed(err))
}

fn utf16_lossy(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .collect();
    String::from_utf16_lossy(&units)
}

/// The NTFS fixup stride: multi-sector protection always covers 512-byte
/// blocks, regardless of the volume's (logical or physical) sector size.
/// Callers pass this constant to [`apply_fixups`], never the boot sector's
/// bytes-per-sector.
pub const FIXUP_BLOCK_SIZE: usize = 512;

/// Undoes the NTFS multi-sector protection in place.
///
/// The update sequence array (offset at `0x04`, count at `0x06`) holds the
/// sequence number followed by one saved word per protected block. The last
/// word of every block must equal the sequence number and is replaced by the
/// saved word. `block_size` is the protection stride; pass
/// [`FIXUP_BLOCK_SIZE`]. The array must cover the record exactly
/// (`(usa_count - 1) * block_size == record.len()`), so a wrong stride or a
/// short array is `Malformed`.
pub fn apply_fixups(record: &mut [u8], block_size: usize) -> Result<(), NtfsError> {
    if block_size < 2 || !block_size.is_multiple_of(2) {
        return Err(NtfsError::Malformed("invalid fixup stride"));
    }
    if record.len() < 8 {
        return Err(NtfsError::Truncated);
    }
    if &record[0..4] != b"FILE" {
        return Err(NtfsError::BadSignature);
    }
    let usa_off = usize::from(u16_at(record, 0x04)?);
    let usa_count = usize::from(u16_at(record, 0x06)?);
    let blocks = usa_count
        .checked_sub(1)
        .filter(|n| *n > 0)
        .ok_or(NtfsError::Malformed("empty update sequence array"))?;
    if !usa_off.is_multiple_of(2) {
        return Err(NtfsError::Malformed("odd update sequence array offset"));
    }
    // blocks <= 0xFFFE and block_size <= usize::MAX: use checked math.
    let covered = blocks
        .checked_mul(block_size)
        .ok_or(NtfsError::Malformed("update sequence array too long"))?;
    if covered != record.len() {
        return Err(NtfsError::Malformed("update sequence array does not cover the record exactly"));
    }
    // The array must lie inside the first block, before its protected word.
    if usa_off + 2 * usa_count > block_size - 2 {
        return Err(NtfsError::Malformed("update sequence array outside the first block"));
    }
    let usn = u16_at(record, usa_off)?;
    // Check every block before changing anything.
    for i in 0..blocks {
        let end = (i + 1) * block_size - 2;
        if u16_at(record, end)? != usn {
            return Err(NtfsError::BadFixup);
        }
    }
    for i in 0..blocks {
        let end = (i + 1) * block_size - 2;
        let saved = bytes_at::<2>(record, usa_off + 2 * (i + 1))?;
        record[end..end + 2].copy_from_slice(&saved);
    }
    Ok(())
}

/// Reads the in-use flag of a raw record, before or without [`apply_fixups`].
///
/// The flags at `0x16` lie in the first 512-byte block, before its protected
/// last word, so fixups never change them: the answer does not depend on the
/// fixups being intact. `false` if the record is too short to hold them.
pub fn header_in_use(record: &[u8]) -> bool {
    u16_at(record, 0x16).is_ok_and(|flags| flags & RECORD_IN_USE != 0)
}

/// Parses one MFT file record on which [`apply_fixups`] has succeeded.
///
/// For a record that is not in use only the header fields are filled; its
/// attributes are not parsed, as callers skip such records.
pub fn parse_record(record: &[u8], record_no: u64, want_runs: bool) -> Result<RecordInfo, NtfsError> {
    if record.len() < 4 {
        return Err(NtfsError::Truncated);
    }
    if &record[0..4] != b"FILE" {
        return Err(NtfsError::BadSignature);
    }
    if record.len() < RECORD_HEADER_LEN {
        return Err(NtfsError::Truncated);
    }
    let attr_off = usize::from(u16_at(record, 0x14)?);
    let flags = u16_at(record, 0x16)?;
    let used = usize::try_from(u32_at(record, 0x18)?).map_err(|_| NtfsError::Malformed("used size"))?;
    let base_record = u64_at(record, 0x20)? & MFT_REF_MASK;

    let mut info = RecordInfo {
        in_use: flags & RECORD_IN_USE != 0,
        is_dir: flags & RECORD_DIR != 0,
        base_record,
        name: None,
        mtime: 0,
        size: None,
        allocated: 0,
        is_link: false,
        data_runs: Vec::new(),
        attr_list: None,
    };
    if !info.in_use {
        return Ok(info);
    }
    if used > record.len() {
        return Err(NtfsError::Malformed("used size exceeds record"));
    }
    if attr_off < RECORD_HEADER_LEN || attr_off > used {
        return Err(NtfsError::Malformed("first attribute offset out of range"));
    }
    let skip_named_data = record_no == BAD_CLUS || base_record == BAD_CLUS;
    let area = &record[..used];

    let mut pos = attr_off;
    loop {
        let attr_type = u32_at(area, pos).map_err(|_| NtfsError::Malformed("missing end marker"))?;
        if attr_type == ATTR_END {
            break;
        }
        let len = u32_at(area, pos + 4).map_err(|_| NtfsError::Malformed("attribute header past used size"))?;
        let len = usize::try_from(len).map_err(|_| NtfsError::Malformed("attribute length"))?;
        if len < ATTR_HEADER_LEN {
            return Err(NtfsError::Malformed("attribute length too small"));
        }
        let attr = slice(area, pos, len, "attribute exceeds used size")?;
        parse_attribute(attr, &mut info, skip_named_data, want_runs)?;
        pos += len; // len >= 16, so the loop always advances
    }
    Ok(info)
}

fn parse_attribute(
    attr: &[u8],
    info: &mut RecordInfo,
    skip_named_data: bool,
    want_runs: bool,
) -> Result<(), NtfsError> {
    let attr_type = u32_at(attr, 0x00)?;
    let non_resident = u8_at(attr, 0x08)? != 0;
    let name_len = usize::from(u8_at(attr, 0x09)?);
    let name_off = usize::from(u16_at(attr, 0x0A)?);
    let flags = u16_at(attr, 0x0C)?;
    slice(attr, name_off, 2 * name_len, "attribute name outside attribute")?;
    let named = name_len > 0;

    if non_resident {
        if attr.len() < NONRESIDENT_HEADER_LEN {
            return Err(NtfsError::Malformed("non-resident header too short"));
        }
        let start_vcn = u64_at(attr, 0x10)?;
        let run_off = usize::from(u16_at(attr, 0x20)?);
        let alloc_size = u64_at(attr, 0x28)?;
        let data_size = u64_at(attr, 0x30)?;
        let run_bytes = attr
            .get(run_off..)
            .filter(|_| run_off >= NONRESIDENT_HEADER_LEN)
            .ok_or(NtfsError::Malformed("run list offset out of range"))?;
        match attr_type {
            ATTR_DATA => {
                if named && skip_named_data {
                    return Ok(());
                }
                if start_vcn == 0 {
                    let counted = if flags & (ATTR_FLAG_COMPRESSED | ATTR_FLAG_SPARSE) != 0 {
                        if attr.len() < NONRESIDENT_EXT_HEADER_LEN {
                            return Err(NtfsError::Malformed("compressed/sparse header too short"));
                        }
                        u64_at(attr, 0x40)?
                    } else {
                        alloc_size
                    };
                    info.allocated = info.allocated.saturating_add(counted);
                    if !named {
                        info.size = Some(data_size);
                    }
                }
                if !named && want_runs {
                    info.data_runs.push((start_vcn, super::runs::decode_runs(run_bytes)?));
                }
            }
            ATTR_LIST if info.attr_list.is_none() => {
                let runs = super::runs::decode_runs(run_bytes)?;
                info.attr_list = Some(AttrListLoc::NonResident { runs, len: data_size });
            }
            // $STANDARD_INFORMATION, $FILE_NAME and $REPARSE_POINT headers
            // we need are resident; a non-resident one is not read.
            _ => {}
        }
        return Ok(());
    }

    if attr.len() < RESIDENT_HEADER_LEN {
        return Err(NtfsError::Malformed("resident header too short"));
    }
    let value_len = usize::try_from(u32_at(attr, 0x10)?).map_err(|_| NtfsError::Malformed("value length"))?;
    let value_off = usize::from(u16_at(attr, 0x14)?);
    let value = slice(attr, value_off, value_len, "attribute value outside attribute")?;
    match attr_type {
        ATTR_STD_INFO => {
            info.mtime = u64_at(value, 0x08).map_err(|_| NtfsError::Malformed("short $STANDARD_INFORMATION"))? as i64;
        }
        ATTR_FILE_NAME => {
            if value.len() < FILE_NAME_LEN {
                return Err(NtfsError::Malformed("short $FILE_NAME"));
            }
            let chars = usize::from(value[0x40]);
            let namespace = value[0x41];
            let name = slice(value, FILE_NAME_LEN, 2 * chars, "file name outside $FILE_NAME")?;
            if info.name.is_none() && namespace != NAMESPACE_DOS {
                info.name = Some(NameInfo {
                    parent: u64_at(value, 0x00)? & MFT_REF_MASK,
                    name: utf16_lossy(name),
                });
            }
        }
        ATTR_DATA => {
            if named && skip_named_data {
                return Ok(());
            }
            // Resident data occupies no clusters.
            if !named {
                info.size = Some(value_len as u64);
            }
        }
        ATTR_REPARSE => {
            let tag = u32_at(value, 0x00).map_err(|_| NtfsError::Malformed("short $REPARSE_POINT"))?;
            info.is_link = tag & REPARSE_NAME_SURROGATE != 0;
        }
        ATTR_LIST if info.attr_list.is_none() => {
            info.attr_list = Some(AttrListLoc::Resident(value.to_vec()));
        }
        _ => {}
    }
    Ok(())
}

/// Parses the entries of an `$ATTRIBUTE_LIST` value (exactly its logical length).
pub fn parse_attr_list(bytes: &[u8]) -> Result<Vec<AttrListEntry>, NtfsError> {
    let mut entries = Vec::new();
    let mut pos = 0usize;
    while pos < bytes.len() {
        let rest = &bytes[pos..];
        if rest.len() < ATTR_LIST_ENTRY_LEN {
            return Err(NtfsError::Truncated);
        }
        let len = usize::from(u16_at(rest, 0x04)?);
        if len < ATTR_LIST_ENTRY_LEN {
            return Err(NtfsError::Malformed("attribute list entry too short"));
        }
        let entry = slice(rest, 0, len, "attribute list entry exceeds list")?;
        let name_len = u8_at(entry, 0x06)?;
        let name_off = usize::from(u8_at(entry, 0x07)?);
        slice(entry, name_off, 2 * usize::from(name_len), "attribute list name outside entry")?;
        entries.push(AttrListEntry {
            attr_type: u32_at(entry, 0x00)?,
            name_len,
            start_vcn: u64_at(entry, 0x08)?,
            record_no: u64_at(entry, 0x10)? & MFT_REF_MASK,
        });
        pos += len; // len >= 0x1A, so the loop always advances
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::ntfs::testkit::RecordBuilder;

    const RUNS: &[u8] = &[0x21, 0x02, 0x10, 0x00, 0x00];

    fn record(b: RecordBuilder) -> Vec<u8> {
        let mut r = b.build(512);
        apply_fixups(&mut r, 512).unwrap();
        r
    }

    fn parse_as(b: RecordBuilder, record_no: u64) -> RecordInfo {
        parse_record(&record(b), record_no, false).unwrap()
    }

    fn parse(b: RecordBuilder) -> RecordInfo {
        parse_as(b, 30)
    }

    fn name_of(info: &RecordInfo) -> (&str, u64) {
        let n = info.name.as_ref().expect("name");
        (n.name.as_str(), n.parent)
    }

    /// Offset of the first attribute of a record.
    fn first_attr(r: &[u8]) -> usize {
        usize::from(u16::from_le_bytes([r[0x14], r[0x15]]))
    }

    #[test]
    fn fixup_roundtrip() {
        // resident data crosses the first sector end, so a real word is saved
        let b = || RecordBuilder::new(1024).file_name(5, "f", 1).data_resident("", 600);
        let mut r = b().build(512);
        assert_eq!(&r[510..512], &[0x07, 0x00]); // sequence number in place
        assert_eq!(apply_fixups(&mut r, 512), Ok(()));
        assert_eq!(&r[510..512], &[0xAB, 0xAB]); // original data restored
        assert_eq!(parse_record(&r, 30, false).unwrap().size, Some(600));

        let mut r = b().build(512);
        r[1023] ^= 0xFF;
        assert_eq!(apply_fixups(&mut r, 512), Err(NtfsError::BadFixup));
        let mut r = b().build(512);
        r[510] ^= 0x01;
        assert_eq!(apply_fixups(&mut r, 512), Err(NtfsError::BadFixup));
    }

    #[test]
    fn fixup_bounds() {
        let good = RecordBuilder::new(1024).build(512);
        // update sequence array claims more sectors than the record has
        let mut r = good.clone();
        r[0x06] = 4;
        assert!(apply_fixups(&mut r, 512).is_err());
        // update sequence array offset beyond the record
        let mut r = good.clone();
        r[0x04] = 0xF0;
        r[0x05] = 0x03;
        assert!(apply_fixups(&mut r, 512).is_err());
        // too short to hold a header
        let mut r = good[..0x10].to_vec();
        assert!(apply_fixups(&mut r, 512).is_err());
        // nonsense sector sizes
        let mut r = good.clone();
        assert!(apply_fixups(&mut r, 0).is_err());
        let mut r = good.clone();
        assert!(apply_fixups(&mut r, 1).is_err());
    }

    #[test]
    fn signature() {
        let mut r = record(RecordBuilder::new(1024).file_name(5, "x", 1));
        r[0..4].copy_from_slice(b"BAAD");
        assert_eq!(parse_record(&r, 30, false), Err(NtfsError::BadSignature));
        let mut r = RecordBuilder::new(1024).build(512);
        r[0..4].copy_from_slice(b"BAAD");
        assert_eq!(apply_fixups(&mut r, 512), Err(NtfsError::BadSignature));
    }

    #[test]
    fn resident_file() {
        let info = parse(RecordBuilder::new(1024).file_name(5, "tiny", 1).std_info(77).data_resident("", 100));
        assert!(info.in_use);
        assert_eq!(name_of(&info), ("tiny", 5));
        assert_eq!(info.mtime, 77);
        assert_eq!(info.size, Some(100));
        assert_eq!(info.allocated, 0);
        assert!(!info.is_dir);
        assert!(!info.is_link);
        assert_eq!(info.base_record, 0);
        assert_eq!(info.attr_list, None);
        assert!(info.data_runs.is_empty());
    }

    #[test]
    fn missing_std_info_gives_zero_mtime() {
        let info = parse(RecordBuilder::new(1024).file_name(5, "a", 1));
        assert_eq!(info.mtime, 0);
        assert_eq!(info.size, None);
    }

    #[test]
    fn nonresident_file() {
        let info = parse(RecordBuilder::new(1024).data_nonresident("", 0, RUNS, 8192, 5000, 0, 0));
        assert_eq!(info.size, Some(5000));
        assert_eq!(info.allocated, 8192);
    }

    #[test]
    fn sparse_uses_total() {
        let info = parse(RecordBuilder::new(1024).data_nonresident("", 0, RUNS, 1 << 30, 1 << 30, 0x8000, 4096));
        assert_eq!(info.size, Some(1 << 30));
        assert_eq!(info.allocated, 4096);
    }

    #[test]
    fn compressed_uses_total() {
        let info = parse(RecordBuilder::new(1024).data_nonresident("", 0, RUNS, 65536, 60000, 0x0001, 16384));
        assert_eq!(info.size, Some(60000));
        assert_eq!(info.allocated, 16384);
    }

    #[test]
    fn named_stream_adds_allocated_only() {
        let info = parse(
            RecordBuilder::new(1024)
                .data_nonresident("", 0, &[0x01, 0x05, 0x00], 20480, 20000, 0x8000, 0)
                .data_nonresident("WofCompressedData", 0, RUNS, 8192, 7000, 0, 0),
        );
        assert_eq!(info.size, Some(20000));
        assert_eq!(info.allocated, 8192);
    }

    #[test]
    fn later_extent_ignored() {
        let info = parse(RecordBuilder::new(1024).data_nonresident("", 16, RUNS, 999, 999, 0, 0));
        assert_eq!(info.size, None);
        assert_eq!(info.allocated, 0);
    }

    #[test]
    fn extension_record() {
        let info = parse(RecordBuilder::new(1024).base(27).data_nonresident("", 0, RUNS, 1_003_520, 1_000_000, 0, 0));
        assert_eq!(info.base_record, 27);
        assert_eq!(info.size, Some(1_000_000));
        assert_eq!(info.allocated, 1_003_520);
    }

    #[test]
    fn dos_name_skipped() {
        let info = parse(RecordBuilder::new(1024).file_name(5, "LONGNA~1", 2).file_name(5, "long name.txt", 1));
        assert_eq!(name_of(&info), ("long name.txt", 5));
    }

    #[test]
    fn first_of_two_names() {
        let info = parse(RecordBuilder::new(1024).file_name(5, "hl1", 1).file_name(24, "hl2", 1));
        assert_eq!(name_of(&info), ("hl1", 5));
    }

    #[test]
    fn directory_and_link() {
        let info = parse(RecordBuilder::new(1024).dir().reparse(0xA000_0003));
        assert!(info.is_dir);
        assert!(info.is_link);
        let info = parse(RecordBuilder::new(1024).reparse(0x9000_001A));
        assert!(!info.is_dir);
        assert!(!info.is_link);
    }

    #[test]
    fn badclus_named_ignored() {
        let b = || {
            RecordBuilder::new(1024)
                .data_resident("", 0)
                .data_nonresident("$Bad", 0, RUNS, 1 << 40, 1 << 40, 0, 0)
        };
        let info = parse_as(b(), 8);
        assert_eq!(info.size, Some(0));
        assert_eq!(info.allocated, 0);
        // the same rule applies to an extension record of $BadClus
        let info = parse_as(b().base(8), 40);
        assert_eq!(info.allocated, 0);
        // any other record counts the named stream
        let info = parse_as(b(), 9);
        assert_eq!(info.allocated, 1 << 40);
    }

    #[test]
    fn invalid_utf16_name() {
        let info = parse(RecordBuilder::new(1024).file_name_utf16(5, &[0x61, 0xD800, 0x62], 1));
        let (name, parent) = name_of(&info);
        assert!(name.contains('a'));
        assert!(name.contains('b'));
        assert_eq!(parent, 5);
    }

    #[test]
    fn record_4k() {
        let b = RecordBuilder::new(4096).file_name(5, "tiny", 1).std_info(77).data_resident("", 100);
        let mut r = b.build(512);
        assert_eq!(r.len(), 4096);
        apply_fixups(&mut r, 512).unwrap();
        let info = parse_record(&r, 30, false).unwrap();
        assert_eq!(name_of(&info), ("tiny", 5));
        assert_eq!(info.mtime, 77);
        assert_eq!(info.size, Some(100));
        assert_eq!(info.allocated, 0);
        assert!(!info.is_dir);
        assert_eq!(info.base_record, 0);
    }

    #[test]
    fn record_4k_uses_512_byte_blocks() {
        let b = || RecordBuilder::new(4096).file_name(5, "tiny", 1).data_resident("", 100);
        let mut r = b().build(FIXUP_BLOCK_SIZE);
        assert_eq!(u16::from_le_bytes([r[0x06], r[0x07]]), 9); // 8 blocks + USN
        assert_eq!(apply_fixups(&mut r, FIXUP_BLOCK_SIZE), Ok(()));
        assert_eq!(parse_record(&r, 30, false).unwrap().size, Some(100));

        // the volume's 4 KB sector size is the wrong stride
        let mut r = b().build(FIXUP_BLOCK_SIZE);
        assert!(matches!(apply_fixups(&mut r, 4096), Err(NtfsError::Malformed(_))));
    }

    #[test]
    fn short_update_sequence_array() {
        let mut r = RecordBuilder::new(1024).file_name(5, "a", 1).build(FIXUP_BLOCK_SIZE);
        r[0x06] = 2; // covers one 512-byte block of a 1024-byte record
        assert!(matches!(apply_fixups(&mut r, FIXUP_BLOCK_SIZE), Err(NtfsError::Malformed(_))));
    }

    #[test]
    fn malformed_attr_len() {
        let good = record(RecordBuilder::new(1024).file_name(5, "a", 1).data_resident("", 10));
        let at = first_attr(&good) + 4;

        let mut r = good.clone();
        r[at..at + 4].copy_from_slice(&0u32.to_le_bytes());
        assert!(matches!(parse_record(&r, 30, false), Err(NtfsError::Malformed(_))));

        let mut r = good.clone();
        r[at..at + 4].copy_from_slice(&2000u32.to_le_bytes());
        assert!(matches!(parse_record(&r, 30, false), Err(NtfsError::Malformed(_))));

        let mut r = good.clone();
        r[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(parse_record(&r, 30, false), Err(NtfsError::Malformed(_))));
    }

    #[test]
    fn corrupted_records_never_panic() {
        let good = record(
            RecordBuilder::new(1024)
                .std_info(1)
                .attr_list_resident(&[(0x80, 0, 0)])
                .file_name(5, "name", 1)
                .data_nonresident("", 0, RUNS, 8192, 5000, 0x8000, 4096)
                .reparse(0xA000_0003),
        );
        for len in 0..good.len() {
            let _ = parse_record(&good[..len], 30, true);
        }
        for i in 0..good.len() {
            for v in [0x00, 0x01, 0x7F, 0x80, 0xFF] {
                let mut r = good.clone();
                r[i] = v;
                let _ = parse_record(&r, 30, true);
                let _ = apply_fixups(&mut r, 512);
            }
        }
        for len in 0..64 {
            let _ = parse_attr_list(&[0xFF; 64][..len]);
            let _ = parse_attr_list(&[0x00; 64][..len]);
        }
    }

    #[test]
    fn not_in_use() {
        let info = parse(RecordBuilder::new(1024).not_in_use().file_name(5, "gone", 1));
        assert!(!info.in_use);
    }

    #[test]
    fn in_use_flag_without_fixups() {
        let mut used = RecordBuilder::new(1024).file_name(5, "a", 1).build(512);
        let mut free = RecordBuilder::new(1024).not_in_use().file_name(5, "a", 1).build(512);
        used[510] ^= 0xFF;
        free[510] ^= 0xFF;
        assert_eq!(apply_fixups(&mut used.clone(), 512), Err(NtfsError::BadFixup));
        assert!(header_in_use(&used));
        assert!(!header_in_use(&free));
        assert!(!header_in_use(&used[..0x17]));
    }

    #[test]
    fn attr_list_entries() {
        let info = parse(RecordBuilder::new(1024).attr_list_resident(&[(0x80, 0, 0), (0x80, 8, 15)]));
        let Some(AttrListLoc::Resident(bytes)) = info.attr_list else {
            panic!("expected resident attribute list, got {:?}", info.attr_list);
        };
        let entries = parse_attr_list(&bytes).unwrap();
        assert_eq!(
            entries,
            vec![
                AttrListEntry { attr_type: 0x80, name_len: 0, start_vcn: 0, record_no: 0 },
                AttrListEntry { attr_type: 0x80, name_len: 0, start_vcn: 8, record_no: 15 },
            ]
        );
    }

    #[test]
    fn attr_list_malformed() {
        let mut e = vec![0u8; 0x20];
        e[0] = 0x80;
        e[4] = 0; // entry length 0
        assert!(matches!(parse_attr_list(&e), Err(NtfsError::Malformed(_))));
        e[4] = 0x40; // entry longer than the list
        assert!(matches!(parse_attr_list(&e), Err(NtfsError::Malformed(_))));
        assert_eq!(parse_attr_list(&[]), Ok(vec![]));
    }

    #[test]
    fn runs_returned_on_request() {
        let r = record(
            RecordBuilder::new(1024)
                .data_nonresident("", 0, RUNS, 8192, 5000, 0, 0)
                .data_nonresident("named", 0, &[0x11, 0x01, 0x20, 0x00], 4096, 4096, 0, 0),
        );
        let info = parse_record(&r, 30, true).unwrap();
        assert_eq!(info.data_runs, vec![(0, vec![Run { lcn: Some(0x10), clusters: 2 }])]);
        let info = parse_record(&r, 30, false).unwrap();
        assert!(info.data_runs.is_empty());
    }
}
