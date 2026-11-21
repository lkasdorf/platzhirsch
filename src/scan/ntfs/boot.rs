//! NTFS boot sector parsing.
use super::NtfsError;

/// Geometry values taken from the NTFS boot sector.
#[derive(Debug, PartialEq, Clone, Copy)]
pub struct BootSector {
    pub bytes_per_sector: u32,
    pub cluster_size: u32,
    pub mft_lcn: u64,
    pub record_size: u32,
    pub total_sectors: u64,
}

const BOOT_SECTOR_LEN: usize = 512;

fn le_u64(buf: &[u8], at: usize) -> Result<u64, NtfsError> {
    let bytes: [u8; 8] = buf
        .get(at..at + 8)
        .and_then(|s| s.try_into().ok())
        .ok_or(NtfsError::Truncated)?;
    Ok(u64::from_le_bytes(bytes))
}

/// Parses the NTFS boot sector (the first 512 bytes of the volume).
///
/// Offsets: 0x03 OEM id, 0x0B bytes per sector (u16), 0x0D sectors per
/// cluster (u8), 0x28 total sectors (u64), 0x30 MFT LCN (u64), 0x40 clusters
/// per file record segment (i8).
pub fn parse_boot_sector(buf: &[u8]) -> Result<BootSector, NtfsError> {
    if buf.len() < BOOT_SECTOR_LEN {
        return Err(NtfsError::Truncated);
    }
    if &buf[0x03..0x0B] != b"NTFS    " {
        return Err(NtfsError::BadSignature);
    }

    let bytes_per_sector = u32::from(u16::from_le_bytes([buf[0x0B], buf[0x0C]]));
    if !bytes_per_sector.is_power_of_two() {
        return Err(NtfsError::Malformed("bytes per sector not a power of two"));
    }

    let spc_raw = buf[0x0D];
    let sectors_per_cluster: u32 = if spc_raw == 0 {
        return Err(NtfsError::Malformed("sectors per cluster is zero"));
    } else if spc_raw <= 0x80 {
        u32::from(spc_raw)
    } else {
        // Values above 0x80 are a negative i8: 2^(-v) sectors.
        let exp = 256 - u32::from(spc_raw);
        1u32.checked_shl(exp)
            .ok_or(NtfsError::Malformed("sectors per cluster exponent too large"))?
    };
    if !sectors_per_cluster.is_power_of_two() {
        return Err(NtfsError::Malformed("sectors per cluster not a power of two"));
    }
    let cluster_size = bytes_per_sector
        .checked_mul(sectors_per_cluster)
        .ok_or(NtfsError::Malformed("cluster size overflows"))?;

    let record_raw = buf[0x40] as i8;
    let record_size: u32 = if record_raw > 0 {
        cluster_size
            .checked_mul(record_raw as u32)
            .ok_or(NtfsError::Malformed("record size overflows"))?
    } else if record_raw == 0 {
        return Err(NtfsError::Malformed("record size is zero"));
    } else {
        // Negative: 2^(-v) bytes.
        let exp = u32::from(record_raw.unsigned_abs());
        1u32.checked_shl(exp)
            .ok_or(NtfsError::Malformed("record size exponent too large"))?
    };
    if !record_size.is_power_of_two() {
        return Err(NtfsError::Malformed("record size not a power of two"));
    }

    Ok(BootSector {
        bytes_per_sector,
        cluster_size,
        mft_lcn: le_u64(buf, 0x30)?,
        record_size,
        total_sectors: le_u64(buf, 0x28)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(record_byte: u8) -> Vec<u8> {
        let mut b = vec![0u8; 512];
        b[0x03..0x0B].copy_from_slice(b"NTFS    ");
        b[0x0B..0x0D].copy_from_slice(&512u16.to_le_bytes());
        b[0x0D] = 8;
        b[0x28..0x30].copy_from_slice(&1_000_000u64.to_le_bytes());
        b[0x30..0x38].copy_from_slice(&786_432u64.to_le_bytes());
        b[0x40] = record_byte;
        b
    }

    #[test]
    fn parses_standard_layout() {
        let b = parse_boot_sector(&sample(0xF6)).unwrap();
        assert_eq!(
            (b.bytes_per_sector, b.cluster_size, b.mft_lcn, b.record_size),
            (512, 4096, 786432, 1024)
        );
        assert_eq!(b.total_sectors, 1_000_000);
    }

    #[test]
    fn record_size_variants() {
        assert_eq!(parse_boot_sector(&sample(0xF4)).unwrap().record_size, 4096);
        assert_eq!(parse_boot_sector(&sample(0x01)).unwrap().record_size, 4096);
        assert_eq!(parse_boot_sector(&sample(0x02)).unwrap().record_size, 8192);
    }

    #[test]
    fn rejects_bad_signature_and_short_buffer() {
        let mut b = sample(0xF6);
        b[0x03..0x0B].copy_from_slice(b"FAT32   ");
        assert_eq!(parse_boot_sector(&b), Err(NtfsError::BadSignature));
        assert_eq!(parse_boot_sector(&[0u8; 100]), Err(NtfsError::Truncated));
        assert_eq!(parse_boot_sector(&sample(0xF6)[..511]), Err(NtfsError::Truncated));
    }

    #[test]
    fn rejects_malformed_geometry() {
        let mut b = sample(0xF6);
        b[0x0B..0x0D].copy_from_slice(&0u16.to_le_bytes());
        assert!(matches!(parse_boot_sector(&b), Err(NtfsError::Malformed(_))));
        let mut b = sample(0xF6);
        b[0x0B..0x0D].copy_from_slice(&500u16.to_le_bytes());
        assert!(matches!(parse_boot_sector(&b), Err(NtfsError::Malformed(_))));
        let mut b = sample(0xF6);
        b[0x0D] = 0;
        assert!(matches!(parse_boot_sector(&b), Err(NtfsError::Malformed(_))));
        let mut b = sample(0xF6);
        b[0x0D] = 3;
        assert!(matches!(parse_boot_sector(&b), Err(NtfsError::Malformed(_))));
        // record size byte 0, huge shift, non power-of-two cluster count
        assert!(matches!(parse_boot_sector(&sample(0x00)), Err(NtfsError::Malformed(_))));
        assert!(matches!(parse_boot_sector(&sample(0x80)), Err(NtfsError::Malformed(_))));
        assert!(matches!(parse_boot_sector(&sample(0x03)), Err(NtfsError::Malformed(_))));
        let mut b = sample(0xF6);
        b[0x0D] = 0x81; // 2^127 sectors per cluster
        assert!(matches!(parse_boot_sector(&b), Err(NtfsError::Malformed(_))));
    }

    #[test]
    fn large_cluster_via_negative_exponent() {
        let mut b = sample(0xF6);
        b[0x0D] = 0xF9; // 2^7 = 128 sectors
        assert_eq!(parse_boot_sector(&b).unwrap().cluster_size, 65536);
    }

    #[test]
    fn never_panics_on_any_geometry_bytes() {
        for spc in 0..=255u8 {
            for rec in 0..=255u8 {
                let mut b = sample(rec);
                b[0x0D] = spc;
                let _ = parse_boot_sector(&b);
            }
        }
    }
}
