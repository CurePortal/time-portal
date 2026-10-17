//! Core, GUI-free logic for Time Portal: device probing, the on-drive
//! manifest, snapshot-version listing, and restore planning. Everything here is
//! testable without root, a GUI, or a real backup drive.

pub mod devices;
pub mod manifest;
pub mod restore;
pub mod versions;

pub use manifest::Manifest;
pub use versions::{scan_versions, Version};

/// btrbk long-iso timestamp, e.g. `20261016T122217-0700`.
pub const TS_LEN: usize = 20;

/// Convert a btrbk long-iso timestamp to a Unix epoch (seconds).
///
/// Format: `YYYYMMDDThhmmss±hhmm`. Implemented without a date library so the
/// core has no heavy dependencies; the civil-date math is Hinnant's algorithm.
pub fn parse_ts(ts: &str) -> Option<i64> {
    let b = ts.as_bytes();
    if b.len() != TS_LEN {
        return None;
    }
    if b[8] != b'T' {
        return None;
    }
    if !(b[15] == b'+' || b[15] == b'-') {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        ts.get(r).and_then(|s| s.parse::<i64>().ok())
    };
    let (y, mo, d) = (num(0..4)?, num(4..6)?, num(6..8)?);
    let (h, mi, s) = (num(9..11)?, num(11..13)?, num(13..15)?);
    let oh = num(16..18)?;
    let om = num(18..20)?;

    let days = days_from_civil(y, mo, d);
    let local = days * 86_400 + h * 3600 + mi * 60 + s;
    let off = oh * 3600 + om * 60;
    let off = if b[15] == b'-' { -off } else { off };
    Some(local - off)
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_known_timestamp() {
        // 2026-10-16T12:22:17-07:00
        assert_eq!(parse_ts("20261016T122217-0700"), Some(1_792_178_537));
    }

    #[test]
    fn offset_shifts_the_result() {
        let utc = parse_ts("20261016T122217+0000").unwrap();
        let pdt = parse_ts("20261016T122217-0700").unwrap();
        assert_eq!(pdt - utc, 7 * 3600);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_ts("").is_none());
        assert!(parse_ts("20261016T122217").is_none());
        assert!(parse_ts("not-a-timestamp-here!").is_none());
    }
}
