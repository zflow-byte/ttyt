use std::time::{SystemTime, UNIX_EPOCH};

/// UTC calendar/clock fields derived from a `SystemTime`.
///
/// Log timestamps are UTC rather than the host's local time zone: this
/// project has no date/time crate in its mandated dependency list (see
/// `outputs/2026-07-29-smart-console-design.md`), and correct local-time
/// conversion needs either an external crate or unsafe FFI into the C
/// library's tz database. UTC is a zero-dependency, zero-`unsafe`, DST-free
/// choice, and is also standard practice for logs a distributed team may
/// review across time zones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UtcParts {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

impl UtcParts {
    pub fn now() -> UtcParts {
        Self::from_system_time(SystemTime::now())
    }

    pub fn from_system_time(time: SystemTime) -> UtcParts {
        let secs = time
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        Self::from_unix_secs(secs)
    }

    pub fn from_unix_secs(secs: i64) -> UtcParts {
        let days = secs.div_euclid(86_400);
        let secs_of_day = secs.rem_euclid(86_400);
        let (year, month, day) = civil_from_days(days);
        UtcParts {
            year,
            month,
            day,
            hour: (secs_of_day / 3600) as u32,
            minute: ((secs_of_day % 3600) / 60) as u32,
            second: (secs_of_day % 60) as u32,
        }
    }

    /// `YYYY-MM-DD`, for the log directory name.
    pub fn date_string(&self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    /// `HHMMSS`, for the log file name.
    pub fn time_string(&self) -> String {
        format!("{:02}{:02}{:02}", self.hour, self.minute, self.second)
    }
}

/// Days-since-1970-01-01 -> proleptic Gregorian (year, month, day).
///
/// Public-domain algorithm by Howard Hinnant:
/// <http://howardhinnant.github.io/date_algorithms.html#civil_from_days>
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let year = if m <= 2 { y + 1 } else { y };
    (year as i32, m, d)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn epoch_is_1970_01_01_midnight() {
        let parts = UtcParts::from_unix_secs(0);
        assert_eq!(parts.date_string(), "1970-01-01");
        assert_eq!(parts.time_string(), "000000");
    }

    #[test]
    fn known_timestamp_matches_expected_utc_date_and_time() {
        // 2026-07-29T15:04:05Z (verified against Python's
        // datetime.fromtimestamp(..., UTC) before being hard-coded here)
        let parts = UtcParts::from_unix_secs(1_785_337_445);
        assert_eq!(parts.date_string(), "2026-07-29");
        assert_eq!(parts.time_string(), "150405");
    }

    #[test]
    fn leap_day_2024_02_29_round_trips() {
        // 2024-02-29T00:00:00Z
        let parts = UtcParts::from_unix_secs(1_709_164_800);
        assert_eq!(parts.date_string(), "2024-02-29");
    }
}
