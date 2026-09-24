//! Date and time helpers.
//!
//! Bartelang reports *local* wall-clock time, so that a script's own timestamps
//! line up with what `date` prints in the shell commands it runs.  `std` has no
//! time-zone support, so on Unix this defers to `localtime_r`; elsewhere it
//! falls back to UTC.
//!
//! Two families of function live here and it matters which is which:
//!
//! * [`local_time`] converts an epoch second count into *local* fields, zone and
//!   all.  It is what `Now()` and `FileDateTime()` use.
//! * [`naive_seconds`] / [`from_naive_seconds`] convert between fields and a
//!   zone-free second count, which is what date *arithmetic* needs: adding a day
//!   to a calendar date should not depend on where the clock is.

use std::time::{SystemTime, UNIX_EPOCH};

const MONTH_NAMES: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

const DAY_NAMES: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

/// The calendar fields of a timestamp.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LocalTime {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

impl LocalTime {
    pub fn date(&self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    pub fn time(&self) -> String {
        format!("{:02}:{:02}:{:02}", self.hour, self.minute, self.second)
    }

    pub fn date_time(&self) -> String {
        format!("{} {}", self.date(), self.time())
    }

    /// 0 = Sunday, as in VB6's `Weekday`.
    pub fn weekday(&self) -> usize {
        let days = days_from_civil(self.year, self.month, self.day);
        // 1970-01-01 was a Thursday.
        ((days + 4).rem_euclid(7)) as usize
    }

    pub fn month_name(&self) -> &'static str {
        MONTH_NAMES[(self.month.clamp(1, 12) - 1) as usize]
    }

    pub fn day_name(&self) -> &'static str {
        DAY_NAMES[self.weekday()]
    }

    /// Day of the year, 1-based.
    pub fn day_of_year(&self) -> i64 {
        days_from_civil(self.year, self.month, self.day) - days_from_civil(self.year, 1, 1) + 1
    }
}

/// The current local time.
pub(crate) fn now() -> LocalTime {
    local_time(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs() as i64)
            .unwrap_or(0),
    )
}

/// A file timestamp rendered the same way `Now()` is.
pub(crate) fn format_timestamp(instant: SystemTime) -> String {
    let seconds = instant
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0);
    local_time(seconds).date_time()
}

/// Splits seconds-since-the-epoch into local calendar fields.
#[cfg(unix)]
pub(crate) fn local_time(seconds: i64) -> LocalTime {
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let stamp = seconds as libc::time_t;
    // SAFETY: `tm` is a valid, properly aligned, writable `libc::tm`, and
    // `localtime_r` writes into it and reads nothing else of ours.
    let converted = unsafe { libc::localtime_r(&stamp, &mut tm) };
    if converted.is_null() {
        return from_naive_seconds(seconds);
    }
    LocalTime {
        year: tm.tm_year as i64 + 1900,
        month: tm.tm_mon as u32 + 1,
        day: tm.tm_mday as u32,
        hour: tm.tm_hour as u32,
        minute: tm.tm_min as u32,
        second: tm.tm_sec as u32,
    }
}

#[cfg(not(unix))]
pub(crate) fn local_time(seconds: i64) -> LocalTime {
    from_naive_seconds(seconds)
}

/// Fields back to a zone-free second count - the inverse of
/// [`from_naive_seconds`].
pub(crate) fn naive_seconds(time: &LocalTime) -> i64 {
    days_from_civil(time.year, time.month, time.day) * 86_400
        + time.hour as i64 * 3600
        + time.minute as i64 * 60
        + time.second as i64
}

/// A zone-free second count back to fields - the inverse of [`naive_seconds`].
pub(crate) fn from_naive_seconds(seconds: i64) -> LocalTime {
    let days = seconds.div_euclid(86_400);
    let within_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    LocalTime {
        year,
        month,
        day,
        hour: (within_day / 3600) as u32,
        minute: ((within_day % 3600) / 60) as u32,
        second: (within_day % 60) as u32,
    }
}

/// Parses the shape `Now()` and `FileDateTime()` produce: `YYYY-MM-DD`, or
/// `YYYY-MM-DD HH:MM:SS`.
pub(crate) fn parse_timestamp(text: &str) -> Option<LocalTime> {
    let text = text.trim();
    let (date, clock) = match text.split_once(' ') {
        Some((date, clock)) => (date, Some(clock)),
        None => (text, None),
    };

    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.trim().parse().ok()?;
    let month: u32 = date_parts.next()?.parse().ok()?;
    let day: u32 = date_parts.next()?.parse().ok()?;
    if date_parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    let (hour, minute, second) = match clock {
        Some(clock) => {
            let mut pieces = clock.split(':');
            let hour: u32 = pieces.next()?.parse().ok()?;
            let minute: u32 = pieces.next()?.parse().ok()?;
            let second: u32 = match pieces.next() {
                Some(seconds) => seconds.parse().ok()?,
                None => 0,
            };
            if hour > 23 || minute > 59 || second > 60 {
                return None;
            }
            (hour, minute, second)
        }
        None => (0, 0, 0),
    };

    Some(LocalTime {
        year,
        month,
        day,
        hour,
        minute,
        second,
    })
}

/// Days in a month, leap years included.
pub(crate) fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

/// Adds a VB6 date interval.  Calendar units (`yyyy`, `q`, `m`) adjust the
/// calendar and clamp the day, so `2024-01-31` plus one month is `2024-02-29`.
pub(crate) fn add_interval(time: &LocalTime, interval: &str, amount: i64) -> Option<LocalTime> {
    let months = |count: i64| -> Option<LocalTime> {
        let total = time.year * 12 + (time.month as i64 - 1) + count;
        let year = total.div_euclid(12);
        let month = (total.rem_euclid(12) + 1) as u32;
        let day = time.day.min(days_in_month(year, month));
        Some(LocalTime {
            year,
            month,
            day,
            ..*time
        })
    };
    let seconds =
        |delta: i64| -> Option<LocalTime> { Some(from_naive_seconds(naive_seconds(time) + delta)) };

    match interval {
        "yyyy" => months(amount * 12),
        "q" => months(amount * 3),
        "m" => months(amount),
        "d" | "y" => seconds(amount * 86_400),
        "w" => seconds(amount * 7 * 86_400),
        "ww" => seconds(amount * 7 * 86_400),
        "h" => seconds(amount * 3600),
        "n" => seconds(amount * 60),
        "s" => seconds(amount),
        _ => None,
    }
}

/// The number of whole `interval` boundaries between two timestamps, negative
/// when `end` is earlier than `start`.
pub(crate) fn diff_interval(start: &LocalTime, end: &LocalTime, interval: &str) -> Option<i64> {
    let months_between = |first: &LocalTime, second: &LocalTime| -> i64 {
        (second.year * 12 + second.month as i64) - (first.year * 12 + first.month as i64)
    };
    let quarters_between = |first: &LocalTime, second: &LocalTime| -> i64 {
        let quarter = |time: &LocalTime| (time.month as i64 - 1) / 3;
        (second.year * 4 + quarter(second)) - (first.year * 4 + quarter(first))
    };
    let days_between = |first: &LocalTime, second: &LocalTime| -> i64 {
        days_from_civil(second.year, second.month, second.day)
            - days_from_civil(first.year, first.month, first.day)
    };

    // Calendar units count *boundaries crossed*, which is what VB6 did: Dec 31 to
    // Jan 1 is one year, and Jan 31 to Feb 1 is one month.
    match interval {
        "yyyy" => Some(end.year - start.year),
        "q" => Some(quarters_between(start, end)),
        "m" => Some(months_between(start, end)),
        "d" => Some(days_between(start, end)),
        "w" | "ww" => Some(days_between(start, end) / 7),
        "h" => Some((naive_seconds(end) - naive_seconds(start)) / 3600),
        "n" => Some((naive_seconds(end) - naive_seconds(start)) / 60),
        "s" => Some(naive_seconds(end) - naive_seconds(start)),
        _ => None,
    }
}

/// Extracts one field of a timestamp.  The return value is a plain integer;
/// names and weekdays are the caller's business.
pub(crate) fn part_interval(time: &LocalTime, interval: &str) -> Option<i64> {
    match interval {
        "yyyy" => Some(time.year),
        "q" => Some(((time.month as i64 - 1) / 3) + 1),
        "m" => Some(time.month as i64),
        "d" => Some(time.day as i64),
        "y" => Some(time.day_of_year()),
        "w" => Some(time.weekday() as i64),
        "ww" => Some((time.day_of_year() - 1) / 7 + 1),
        "h" => Some(time.hour as i64),
        "n" => Some(time.minute as i64),
        "s" => Some(time.second as i64),
        _ => None,
    }
}

/// Howard Hinnant's days-from-civil inverse, valid for any real date.
pub(crate) fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// Howard Hinnant's days-from-civil.
pub(crate) fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if month > 2 { month - 3 } else { month + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_from_days_handles_known_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
    }

    #[test]
    fn days_and_civil_round_trip() {
        for days in [0, 1, 365, 19_723, 20_000, -1, -365] {
            let (year, month, day) = civil_from_days(days);
            assert_eq!(days_from_civil(year, month, day), days);
        }
    }

    #[test]
    fn naive_seconds_round_trip() {
        let time = LocalTime {
            year: 2026,
            month: 9,
            day: 24,
            hour: 16,
            minute: 1,
            second: 16,
        };
        assert_eq!(from_naive_seconds(naive_seconds(&time)), time);
    }

    #[test]
    fn parse_accepts_both_shapes_we_produce() {
        assert_eq!(
            parse_timestamp("2026-09-24 16:01:16"),
            Some(LocalTime {
                year: 2026,
                month: 9,
                day: 24,
                hour: 16,
                minute: 1,
                second: 16
            })
        );
        assert_eq!(
            parse_timestamp("2026-09-24"),
            Some(LocalTime {
                year: 2026,
                month: 9,
                day: 24,
                hour: 0,
                minute: 0,
                second: 0
            })
        );
        assert!(parse_timestamp("not a date").is_none());
        assert!(parse_timestamp("2026-13-01").is_none());
    }

    #[test]
    fn weekday_matches_known_days() {
        let thursday = LocalTime {
            year: 1970,
            month: 1,
            day: 1,
            hour: 0,
            minute: 0,
            second: 0,
        };
        assert_eq!(thursday.weekday(), 4);
        assert_eq!(thursday.day_name(), "Thursday");

        let sunday = LocalTime {
            year: 2024,
            month: 1,
            day: 7,
            hour: 0,
            minute: 0,
            second: 0,
        };
        assert_eq!(sunday.day_name(), "Sunday");
    }

    #[test]
    fn month_names_and_lengths() {
        let feb = LocalTime {
            year: 2024,
            month: 2,
            day: 1,
            hour: 0,
            minute: 0,
            second: 0,
        };
        assert_eq!(feb.month_name(), "February");
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2026, 2), 28);
        assert_eq!(days_in_month(2000, 2), 29);
        assert_eq!(days_in_month(1900, 2), 28);
        assert_eq!(days_in_month(2026, 4), 30);
    }

    #[test]
    fn adding_months_clamps_the_day() {
        let end_of_january = LocalTime {
            year: 2024,
            month: 1,
            day: 31,
            hour: 0,
            minute: 0,
            second: 0,
        };
        let result = add_interval(&end_of_january, "m", 1).unwrap();
        assert_eq!((result.year, result.month, result.day), (2024, 2, 29));
    }

    #[test]
    fn adding_days_crosses_months_and_years() {
        let new_years_eve = LocalTime {
            year: 2025,
            month: 12,
            day: 31,
            hour: 23,
            minute: 0,
            second: 0,
        };
        let result = add_interval(&new_years_eve, "d", 1).unwrap();
        assert_eq!((result.year, result.month, result.day), (2026, 1, 1));
        let back = add_interval(&new_years_eve, "h", -23).unwrap();
        assert_eq!(
            (back.year, back.month, back.day, back.hour),
            (2025, 12, 31, 0)
        );
    }

    #[test]
    fn diff_counts_boundaries() {
        let first = parse_timestamp("2026-01-31").unwrap();
        let second = parse_timestamp("2026-02-01").unwrap();
        // One month boundary and one day, but no whole year.
        assert_eq!(diff_interval(&first, &second, "m"), Some(1));
        assert_eq!(diff_interval(&first, &second, "d"), Some(1));
        assert_eq!(diff_interval(&first, &second, "yyyy"), Some(0));
        assert_eq!(diff_interval(&second, &first, "d"), Some(-1));

        let new_years_eve = parse_timestamp("2025-12-31").unwrap();
        let new_years_day = parse_timestamp("2026-01-01").unwrap();
        assert_eq!(
            diff_interval(&new_years_eve, &new_years_day, "yyyy"),
            Some(1)
        );
        assert_eq!(diff_interval(&new_years_eve, &new_years_day, "q"), Some(1));
    }

    #[test]
    fn part_extracts_fields() {
        let time = parse_timestamp("2026-09-24 16:01:16").unwrap();
        assert_eq!(part_interval(&time, "yyyy"), Some(2026));
        assert_eq!(part_interval(&time, "q"), Some(3));
        assert_eq!(part_interval(&time, "m"), Some(9));
        assert_eq!(part_interval(&time, "d"), Some(24));
        assert_eq!(part_interval(&time, "h"), Some(16));
        assert_eq!(part_interval(&time, "n"), Some(1));
        assert_eq!(part_interval(&time, "s"), Some(16));
        assert_eq!(part_interval(&time, "y"), Some(267));
        assert_eq!(part_interval(&time, "w"), Some(4));
        assert!(part_interval(&time, "nonsense").is_none());
    }

    #[test]
    fn local_time_is_within_a_day_of_utc() {
        // Whatever the zone, the two must agree to within the offset range.
        let seconds = 1_700_000_000;
        let local = local_time(seconds);
        let utc = from_naive_seconds(seconds);
        let local_minutes = local.hour as i64 * 60 + local.minute as i64;
        let utc_minutes = utc.hour as i64 * 60 + utc.minute as i64;
        assert!((local_minutes - utc_minutes).abs() <= 48 * 60);
        assert_eq!(local.second, utc.second);
    }
}
