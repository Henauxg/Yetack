//! The time Claude Code writes on its lines: UTC to the millisecond, as
//! `2026-11-10T10:48:01.799Z`.

const MILLIS_PER_SECOND: u64 = 1000;
const SECONDS_PER_MINUTE: u64 = 60;
const MINUTES_PER_HOUR: u64 = 60;
const SECONDS_PER_DAY: u64 = 24 * MINUTES_PER_HOUR * SECONDS_PER_MINUTE;

/// The milliseconds since the Unix epoch of a line's `timestamp`, or `None` for a time in another
/// form, another zone or before 1970.
#[must_use]
pub fn line_time_millis(text: &str) -> Option<u64> {
    let (date, time) = text.strip_suffix('Z')?.split_once('T')?;
    let mut date_parts = date.split('-');
    let year: i64 = number(date_parts.next()?, 4)?;
    let month: i64 = number(date_parts.next()?, 2)?;
    let day: i64 = number(date_parts.next()?, 2)?;
    if date_parts.next().is_some()
        || !(1..=12).contains(&month)
        || !(1..=days_in_month(year, month)).contains(&day)
    {
        return None;
    }
    let (clock, fraction) = match time.split_once('.') {
        Some((_, "")) => return None,
        Some(parts) => parts,
        None => (time, ""),
    };
    let mut clock_parts = clock.split(':');
    let hour: u64 = number(clock_parts.next()?, 2)?;
    let minute: u64 = number(clock_parts.next()?, 2)?;
    let second: u64 = number(clock_parts.next()?, 2)?;
    if clock_parts.next().is_some() || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let millis = fraction_millis(fraction)?;
    let days = u64::try_from(days_from_civil(year, month, day)).ok()?;
    let seconds =
        days * SECONDS_PER_DAY + (hour * MINUTES_PER_HOUR + minute) * SECONDS_PER_MINUTE + second;
    seconds.checked_mul(MILLIS_PER_SECOND)?.checked_add(millis)
}

/// A number of exactly `digits` ASCII digits.
fn number<T: std::str::FromStr>(text: &str, digits: usize) -> Option<T> {
    (text.len() == digits && text.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| text.parse().ok())
        .flatten()
}

/// The milliseconds of a fraction of a second, its first three digits: "799" and "7991" are 799,
/// "7" is 700. An empty fraction is 0.
fn fraction_millis(fraction: &str) -> Option<u64> {
    if !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let digits: String = fraction.chars().chain("000".chars()).take(3).collect();
    digits.parse().ok()
}

/// Days from 1970-01-01 to the date, in the proleptic Gregorian calendar (Howard Hinnant's
/// `days_from_civil`).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_from_march = (month + 9) % 12;
    let day_of_year = (153 * month_from_march + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The days of a month from 1 to 12, in the proleptic Gregorian calendar.
fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_claude_codes_line_times() {
        assert_eq!(line_time_millis("1970-01-01T00:00:00.000Z"), Some(0));
        assert_eq!(
            line_time_millis("2026-11-10T10:48:01.799Z"),
            Some(1_794_307_681_799)
        );
        assert_eq!(
            line_time_millis("2024-02-29T23:59:59Z"),
            Some(1_709_251_199_000)
        );
        assert_eq!(
            line_time_millis("2026-11-10T10:48:01.7Z"),
            Some(1_794_307_681_700)
        );
    }

    #[test]
    fn refuses_other_forms() {
        for text in [
            "",
            "2026-11-10",
            "2026-11-10T10:48:01.799",
            "2026-11-10T10:48:01+01:00",
            "2026-13-10T10:48:01Z",
            "2026-11-10T24:00:00Z",
            "1969-12-31T23:59:59Z",
            "2026-11-10T10:48:01.79xZ",
            "26-11-10T10:48:01Z",
            "2026-02-29T00:00:00Z",
            "2026-04-31T00:00:00Z",
            "2026-11-10T10:48:01.Z",
        ] {
            assert_eq!(line_time_millis(text), None, "{text}");
        }
    }
}
