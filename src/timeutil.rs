use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{ClauckError, ErrorCode, Result};

pub const MAX_TTL_SECS: u64 = 24 * 60 * 60;

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn format_utc(unix: u64) -> String {
    let days = (unix / 86_400) as i64;
    let secs = unix % 86_400;
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

pub fn parse_ttl(input: &str) -> Result<u64> {
    let s = input.trim();
    let invalid = || {
        ClauckError::new(
            ErrorCode::InvalidInput,
            format!("invalid duration {input:?}; use a number followed by s, m, or h (e.g. 30m)"),
        )
    };
    if s.len() < 2 {
        return Err(invalid());
    }
    let (num, unit) = s.split_at(s.len() - 1);
    let n: u64 = num.parse().map_err(|_| invalid())?;
    let mult = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        _ => return Err(invalid()),
    };
    let secs = n.checked_mul(mult).ok_or_else(invalid)?;
    if !(60..=MAX_TTL_SECS).contains(&secs) {
        return Err(ClauckError::new(
            ErrorCode::InvalidInput,
            "duration must be between 1 minute and 24 hours",
        ));
    }
    Ok(secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_known_instants() {
        assert_eq!(format_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(format_utc(1_791_741_296), "2026-10-11T17:54:56Z");
    }

    #[test]
    fn parses_ttls() {
        assert_eq!(parse_ttl("30m").unwrap(), 1800);
        assert_eq!(parse_ttl("2h").unwrap(), 7200);
        assert_eq!(parse_ttl("90s").unwrap(), 90);
        for bad in [
            "",
            "m",
            "30",
            "30d",
            "-5m",
            "10s",
            "25h",
            "99999999999999999999h",
        ] {
            assert!(parse_ttl(bad).is_err(), "{bad}");
        }
    }
}
