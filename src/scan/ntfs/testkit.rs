//! Test-only builder for valid MFT file records (including fixups).
//!
//! Layout follows the Linux ntfs3 headers (`fs/ntfs3/ntfs.h`): record header
//! `MFT_REC`, attribute header `ATTRIB`, `ATTR_FILE_NAME`, `ATTR_LIST_ENTRY`.
use super::runs::decode_runs;

const ATTR_STD_INFO: u32 = 0x10;
const ATTR_LIST: u32 = 0x20;
const ATTR_FILE_NAME: u32 = 0x30;
const ATTR_DATA: u32 = 0x80;
const ATTR_REPARSE: u32 = 0xC0;
/// Sequence number written into the update sequence array.
const USN: u16 = 0x0007;
/// Sequence number put into the high 16 bits of built MFT references, so the
/// parser's 48-bit masking is exercised.
const REF_SEQ: u64 = 0x0005 << 48;

/// Builds a valid MFT file record. Defaults: in use, file, base record.
pub struct RecordBuilder {
    record_size: usize,
    in_use: bool,
    dir: bool,
    base: u64,
    /// (attribute type, complete attribute bytes)
    attrs: Vec<(u32, Vec<u8>)>,
}

fn align8(n: usize) -> usize {
    (n + 7) & !7
}

fn put_u16(buf: &mut [u8], off: usize, v: u16) {
    buf[off..off + 2].copy_from_slice(&v.to_le_bytes());
}

fn put_u32(buf: &mut [u8], off: usize, v: u32) {
    buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

fn put_u64(buf: &mut [u8], off: usize, v: u64) {
    buf[off..off + 8].copy_from_slice(&v.to_le_bytes());
}

fn put_name(buf: &mut [u8], off: usize, name: &[u16]) {
    for (i, c) in name.iter().enumerate() {
        put_u16(buf, off + 2 * i, *c);
    }
}

/// A resident attribute: 0x18-byte header, name, value (8-byte aligned).
fn resident_attr(attr_type: u32, name: &[u16], value: &[u8], res_flags: u8) -> Vec<u8> {
    let name_off = 0x18;
    let value_off = align8(name_off + 2 * name.len());
    let len = align8(value_off + value.len());
    let mut a = vec![0u8; len];
    put_u32(&mut a, 0x00, attr_type);
    put_u32(&mut a, 0x04, len as u32);
    a[0x08] = 0; // resident
    a[0x09] = name.len() as u8;
    put_u16(&mut a, 0x0A, name_off as u16);
    put_u16(&mut a, 0x0C, 0);
    put_u32(&mut a, 0x10, value.len() as u32);
    put_u16(&mut a, 0x14, value_off as u16);
    a[0x16] = res_flags;
    put_name(&mut a, name_off, name);
    a[value_off..value_off + value.len()].copy_from_slice(value);
    a
}

impl RecordBuilder {
    pub fn new(record_size: usize) -> Self {
        RecordBuilder { record_size, in_use: true, dir: false, base: 0, attrs: Vec::new() }
    }

    pub fn not_in_use(mut self) -> Self {
        self.in_use = false;
        self
    }

    pub fn dir(mut self) -> Self {
        self.dir = true;
        self
    }

    pub fn base(mut self, record_no: u64) -> Self {
        self.base = record_no;
        self
    }

    pub fn std_info(mut self, mtime: i64) -> Self {
        // NTFS 3.x $STANDARD_INFORMATION is 0x48 bytes; m_time at 0x08.
        let mut v = vec![0u8; 0x48];
        put_u64(&mut v, 0x08, mtime as u64);
        self.attrs.push((ATTR_STD_INFO, resident_attr(ATTR_STD_INFO, &[], &v, 0)));
        self
    }

    pub fn file_name(self, parent: u64, name: &str, namespace: u8) -> Self {
        let utf16: Vec<u16> = name.encode_utf16().collect();
        self.file_name_utf16(parent, &utf16, namespace)
    }

    pub fn file_name_utf16(mut self, parent: u64, name: &[u16], namespace: u8) -> Self {
        let mut v = vec![0u8; 0x42 + 2 * name.len()];
        put_u64(&mut v, 0x00, parent | REF_SEQ);
        v[0x40] = name.len() as u8;
        v[0x41] = namespace;
        put_name(&mut v, 0x42, name);
        // $FILE_NAME is indexed (resident flag 1).
        self.attrs.push((ATTR_FILE_NAME, resident_attr(ATTR_FILE_NAME, &[], &v, 1)));
        self
    }

    pub fn data_resident(mut self, stream: &str, len: u32) -> Self {
        let name: Vec<u16> = stream.encode_utf16().collect();
        let v = vec![0xABu8; len as usize];
        self.attrs.push((ATTR_DATA, resident_attr(ATTR_DATA, &name, &v, 0)));
        self
    }

    #[allow(clippy::too_many_arguments)]
    pub fn data_nonresident(
        mut self,
        stream: &str,
        start_vcn: u64,
        runs: &[u8],
        allocated: u64,
        real: u64,
        flags: u16,
        total_allocated: u64,
    ) -> Self {
        let name: Vec<u16> = stream.encode_utf16().collect();
        // Compressed or sparse attributes carry the extra total_size field,
        // making the header 0x48 instead of 0x40 bytes (ntfs3 SIZEOF_NONRESIDENT_EX).
        let extended = flags & (0x0001 | 0x8000) != 0;
        let name_off = if extended { 0x48 } else { 0x40 };
        let run_off = align8(name_off + 2 * name.len());
        let len = align8(run_off + runs.len() + 1); // + terminator byte
        let clusters: u64 = decode_runs(runs)
            .map(|r| r.iter().map(|x| x.clusters).sum())
            .unwrap_or(0);
        let mut a = vec![0u8; len];
        put_u32(&mut a, 0x00, ATTR_DATA);
        put_u32(&mut a, 0x04, len as u32);
        a[0x08] = 1; // non-resident
        a[0x09] = name.len() as u8;
        put_u16(&mut a, 0x0A, name_off as u16);
        put_u16(&mut a, 0x0C, flags);
        put_u64(&mut a, 0x10, start_vcn);
        put_u64(&mut a, 0x18, (start_vcn + clusters).saturating_sub(1));
        put_u16(&mut a, 0x20, run_off as u16);
        a[0x22] = if flags & 0x0001 != 0 { 4 } else { 0 }; // compression unit
        put_u64(&mut a, 0x28, allocated);
        put_u64(&mut a, 0x30, real);
        put_u64(&mut a, 0x38, real);
        if extended {
            put_u64(&mut a, 0x40, total_allocated);
        }
        put_name(&mut a, name_off, &name);
        a[run_off..run_off + runs.len()].copy_from_slice(runs);
        self.attrs.push((ATTR_DATA, a));
        self
    }

    pub fn reparse(mut self, tag: u32) -> Self {
        let mut v = vec![0u8; 8];
        put_u32(&mut v, 0x00, tag);
        self.attrs.push((ATTR_REPARSE, resident_attr(ATTR_REPARSE, &[], &v, 0)));
        self
    }

    /// Entries are (attribute type, start VCN, record number).
    pub fn attr_list_resident(mut self, entries: &[(u32, u64, u64)]) -> Self {
        let entry_len = 0x20; // 0x1A rounded up to 8
        let mut v = vec![0u8; entry_len * entries.len()];
        for (i, &(attr_type, vcn, record_no)) in entries.iter().enumerate() {
            let e = i * entry_len;
            put_u32(&mut v, e, attr_type);
            put_u16(&mut v, e + 0x04, entry_len as u16);
            v[e + 0x06] = 0; // name length
            v[e + 0x07] = 0x1A; // name offset
            put_u64(&mut v, e + 0x08, vcn);
            put_u64(&mut v, e + 0x10, record_no | REF_SEQ);
            put_u16(&mut v, e + 0x18, i as u16); // attribute id
        }
        self.attrs.push((ATTR_LIST, resident_attr(ATTR_LIST, &[], &v, 0)));
        self
    }

    /// Serializes the record and protects it with fixups at every
    /// `bytes_per_sector` boundary, like NTFS does before writing. Real NTFS
    /// always uses 512 here (`record::FIXUP_BLOCK_SIZE`), also for 4 KB
    /// records; other values only build deliberately mismatched records.
    pub fn build(self, bytes_per_sector: usize) -> Vec<u8> {
        let size = self.record_size;
        assert!(bytes_per_sector >= 2 && size.is_multiple_of(bytes_per_sector));
        let sectors = size / bytes_per_sector;
        let usa_off = 0x30;
        let usa_count = sectors + 1;
        let attr_off = align8(usa_off + 2 * usa_count);

        let mut attrs = self.attrs;
        attrs.sort_by_key(|(t, _)| *t); // stable: keeps order within a type

        let mut r = vec![0u8; size];
        r[0..4].copy_from_slice(b"FILE");
        put_u16(&mut r, 0x04, usa_off as u16);
        put_u16(&mut r, 0x06, usa_count as u16);
        put_u64(&mut r, 0x08, 0x1234); // LSN
        put_u16(&mut r, 0x10, 1); // sequence number
        put_u16(&mut r, 0x12, 1); // hard links
        put_u16(&mut r, 0x14, attr_off as u16);
        let flags = u16::from(self.in_use) | if self.dir { 0x02 } else { 0 };
        put_u16(&mut r, 0x16, flags);
        put_u32(&mut r, 0x1C, size as u32);
        put_u64(&mut r, 0x20, if self.base == 0 { 0 } else { self.base | REF_SEQ });
        put_u16(&mut r, 0x28, attrs.len() as u16); // next attribute id

        let mut pos = attr_off;
        for (id, (_, mut a)) in attrs.into_iter().enumerate() {
            put_u16(&mut a, 0x0E, id as u16);
            assert!(pos + a.len() + 8 <= size, "attributes do not fit into the record");
            r[pos..pos + a.len()].copy_from_slice(&a);
            pos += a.len();
        }
        put_u32(&mut r, pos, 0xFFFF_FFFF);
        pos += 8;
        put_u32(&mut r, 0x18, pos as u32); // used size

        put_u16(&mut r, usa_off, USN);
        for s in 0..sectors {
            let end = (s + 1) * bytes_per_sector - 2;
            let saved = u16::from_le_bytes([r[end], r[end + 1]]);
            put_u16(&mut r, usa_off + 2 * (s + 1), saved);
            put_u16(&mut r, end, USN);
        }
        r
    }
}
