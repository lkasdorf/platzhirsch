//! German number and date formatting (decimal comma, thousands dot).

const UNITS: [&str; 5] = ["KB", "MB", "GB", "TB", "PB"];

/// Groups digits in threes with a dot, e.g. `1683667` -> `1.683.667`.
fn group_thousands(n: u128) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push('.');
        }
        out.push(c);
    }
    out
}

/// Formats a value given in tenths as `12.345,6` (one decimal, German style).
fn format_tenths(tenths: u128) -> String {
    format!("{},{}", group_thousands(tenths / 10), tenths % 10)
}

/// Formats a byte count with binary units (1024), e.g. `1,5 KB`.
/// Below 1 KB the exact number is shown as `1.023 Bytes`.
pub fn format_bytes(n: u64) -> String {
    if n < 1024 {
        return format!("{} Bytes", group_thousands(u128::from(n)));
    }
    let mut unit_index = 0;
    while unit_index + 1 < UNITS.len() && n >= 1u64 << (10 * (unit_index as u32 + 2)) {
        unit_index += 1;
    }
    let unit = 1u128 << (10 * (unit_index as u32 + 1));
    let tenths = (u128::from(n) * 10 + unit / 2) / unit;
    format!("{} {}", format_tenths(tenths), UNITS[unit_index])
}

/// Formats a count with thousands dots, e.g. `1.683.667`.
pub fn format_count(n: u64) -> String {
    group_thousands(u128::from(n))
}

/// Formats `part / whole` as a percentage with one decimal, e.g. `72,4 %`.
/// A `whole` of zero yields `0,0 %`.
pub fn format_percent(part: u64, whole: u64) -> String {
    if whole == 0 {
        return "0,0 %".to_string();
    }
    let whole = u128::from(whole);
    let tenths = (u128::from(part) * 1000 + whole / 2) / whole;
    format!("{} %", format_tenths(tenths))
}

/// Formats a date as `DD.MM.YYYY`.
pub fn format_ymd(y: u16, m: u8, d: u8) -> String {
    format!("{d:02}.{m:02}.{y:04}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_below_one_kb_are_whole_numbers() {
        assert_eq!(format_bytes(0), "0 Bytes");
        assert_eq!(format_bytes(1023), "1.023 Bytes");
    }

    #[test]
    fn bytes_use_binary_units_with_one_decimal() {
        assert_eq!(format_bytes(1024), "1,0 KB");
        assert_eq!(format_bytes(1536), "1,5 KB");
        assert_eq!(format_bytes(820_124_835_840), "763,8 GB");
        assert_eq!(format_bytes(2_748_779_069_440), "2,5 TB");
        assert_eq!(format_bytes(u64::MAX), "16.384,0 PB");
    }

    #[test]
    fn count_has_thousands_separators() {
        assert_eq!(format_count(1_683_667), "1.683.667");
        assert_eq!(format_count(999), "999");
    }

    #[test]
    fn percent_has_decimal_comma_and_handles_zero_whole() {
        assert_eq!(format_percent(724, 1000), "72,4 %");
        assert_eq!(format_percent(5, 0), "0,0 %");
    }

    #[test]
    fn date_is_day_month_year() {
        assert_eq!(format_ymd(2026, 11, 21), "21.11.2026");
    }
}
