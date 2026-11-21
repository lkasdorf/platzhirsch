//! NTFS data run list decoding.
use super::NtfsError;

/// One extent of a non-resident attribute. `lcn == None` is a sparse hole.
#[derive(Debug, PartialEq, Clone, Copy)]
pub struct Run {
    pub lcn: Option<u64>,
    pub clusters: u64,
}

/// Decodes an NTFS data run list. The list ends at a `0x00` header byte or at
/// the end of the slice. Offsets are signed and relative to the previous
/// non-sparse LCN.
pub fn decode_runs(bytes: &[u8]) -> Result<Vec<Run>, NtfsError> {
    let mut runs = Vec::new();
    let mut pos = 0usize;
    let mut prev_lcn: i64 = 0;

    while let Some(&header) = bytes.get(pos) {
        if header == 0 {
            break;
        }
        let len_n = usize::from(header & 0x0F);
        let off_n = usize::from(header >> 4);
        if len_n == 0 {
            return Err(NtfsError::Malformed("run without length bytes"));
        }
        if len_n > 8 || off_n > 8 {
            return Err(NtfsError::Malformed("run field longer than 8 bytes"));
        }
        let body_start = pos + 1;
        let body_end = body_start + len_n + off_n; // at most 17 added, cannot overflow
        let body = bytes
            .get(body_start..body_end)
            .ok_or(NtfsError::Truncated)?;
        let (len_bytes, off_bytes) = body.split_at(len_n);

        let mut clusters = 0u64;
        for (i, &b) in len_bytes.iter().enumerate() {
            clusters |= u64::from(b) << (8 * i);
        }

        let lcn = if off_n == 0 {
            None
        } else {
            let mut raw = 0u64;
            for (i, &b) in off_bytes.iter().enumerate() {
                raw |= u64::from(b) << (8 * i);
            }
            if off_bytes[off_n - 1] & 0x80 != 0 && off_n < 8 {
                raw |= u64::MAX << (8 * off_n); // sign-extend
            }
            let delta = raw as i64;
            let next = prev_lcn
                .checked_add(delta)
                .filter(|v| *v >= 0)
                .ok_or(NtfsError::Malformed("LCN out of range"))?;
            prev_lcn = next;
            Some(next as u64)
        };

        runs.push(Run { lcn, clusters });
        pos = body_end;
    }
    Ok(runs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_run() {
        assert_eq!(
            decode_runs(&[0x21, 0x18, 0x34, 0x56, 0x00]).unwrap(),
            vec![Run { lcn: Some(0x5634), clusters: 0x18 }]
        );
    }

    #[test]
    fn multiple_runs_relative_offsets() {
        assert_eq!(
            decode_runs(&[
                0x31, 0x38, 0x73, 0x25, 0x34, 0x32, 0x14, 0x01, 0xE5, 0x11, 0x02, 0x31, 0x42,
                0xAA, 0x00, 0x03, 0x00
            ])
            .unwrap(),
            vec![
                Run { lcn: Some(0x342573), clusters: 0x38 },
                Run { lcn: Some(0x363758), clusters: 0x114 },
                Run { lcn: Some(0x393802), clusters: 0x42 },
            ]
        );
    }

    #[test]
    fn negative_jump() {
        assert_eq!(
            decode_runs(&[0x11, 0x30, 0x60, 0x21, 0x10, 0x00, 0x01, 0x11, 0x20, 0xE0, 0x00]).unwrap(),
            vec![
                Run { lcn: Some(0x60), clusters: 0x30 },
                Run { lcn: Some(0x160), clusters: 0x10 },
                Run { lcn: Some(0x140), clusters: 0x20 },
            ]
        );
    }

    #[test]
    fn sparse_hole() {
        assert_eq!(
            decode_runs(&[0x01, 0x30, 0x11, 0x08, 0x10, 0x00]).unwrap(),
            vec![Run { lcn: None, clusters: 0x30 }, Run { lcn: Some(0x10), clusters: 0x08 }]
        );
    }

    #[test]
    fn sparse_does_not_change_previous_lcn() {
        assert_eq!(
            decode_runs(&[0x11, 0x01, 0x10, 0x01, 0x05, 0x11, 0x01, 0x02, 0x00]).unwrap(),
            vec![
                Run { lcn: Some(0x10), clusters: 1 },
                Run { lcn: None, clusters: 5 },
                Run { lcn: Some(0x12), clusters: 1 },
            ]
        );
    }

    #[test]
    fn ends_at_slice_end_or_terminator() {
        assert_eq!(decode_runs(&[]).unwrap(), vec![]);
        assert_eq!(decode_runs(&[0x00, 0x11, 0x01, 0x01]).unwrap(), vec![]);
        assert_eq!(
            decode_runs(&[0x11, 0x01, 0x10]).unwrap(),
            vec![Run { lcn: Some(0x10), clusters: 1 }]
        );
    }

    #[test]
    fn truncated() {
        assert_eq!(decode_runs(&[0x21, 0x18, 0x34]), Err(NtfsError::Truncated));
        assert_eq!(decode_runs(&[0x11]), Err(NtfsError::Truncated));
    }

    #[test]
    fn malformed_headers() {
        // zero length bytes
        assert!(matches!(decode_runs(&[0x10, 0x05, 0x00]), Err(NtfsError::Malformed(_))));
        // more than 8 length bytes
        assert!(matches!(decode_runs(&[0x09; 12]), Err(NtfsError::Malformed(_))));
        // more than 8 offset bytes
        assert!(matches!(decode_runs(&[0x91; 20]), Err(NtfsError::Malformed(_))));
        // negative LCN
        assert!(matches!(decode_runs(&[0x11, 0x01, 0xF0, 0x00]), Err(NtfsError::Malformed(_))));
    }

    #[test]
    fn extreme_values_do_not_overflow() {
        // two runs with i64::MAX deltas: the sum overflows
        let mut v = vec![0x88];
        v.extend_from_slice(&[0xFF; 8]);
        v.extend_from_slice(&i64::MAX.to_le_bytes());
        v.push(0x88);
        v.extend_from_slice(&[0x01; 8]);
        v.extend_from_slice(&i64::MAX.to_le_bytes());
        v.push(0x00);
        assert!(matches!(decode_runs(&v), Err(NtfsError::Malformed(_))));
        // i64::MIN delta from zero
        let mut v = vec![0x81, 0x01];
        v.extend_from_slice(&i64::MIN.to_le_bytes());
        assert!(matches!(decode_runs(&v), Err(NtfsError::Malformed(_))));
    }
}
