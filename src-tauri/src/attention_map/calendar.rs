// NYSE trading calendar, computed — no network, so it answers for a replayed day
// as well as for today.
//
// Full closures and the three 13:00 early closes follow the exchange's standing
// rules. A closure the rules cannot know (a day of mourning, a weather closure)
// simply returns no bars; the engine then steps the previous session back one day
// rather than drawing an empty after-hours.

use chrono::{Datelike, Duration, NaiveDate, Weekday};

/// Regular close, ET minutes since midnight.
pub const CLOSE_MIN: u32 = 16 * 60;
pub const EARLY_CLOSE_MIN: u32 = 13 * 60;

fn nth_weekday(year: i32, month: u32, wd: Weekday, nth: u32) -> Option<NaiveDate> {
    let first = NaiveDate::from_ymd_opt(year, month, 1)?;
    let offset = (7 + wd.num_days_from_monday() - first.weekday().num_days_from_monday()) % 7;
    first.checked_add_signed(Duration::days((offset + (nth - 1) * 7) as i64))
}

fn last_weekday(year: i32, month: u32, wd: Weekday) -> Option<NaiveDate> {
    let next = if month == 12 {
        NaiveDate::from_ymd_opt(year + 1, 1, 1)?
    } else {
        NaiveDate::from_ymd_opt(year, month + 1, 1)?
    };
    let mut d = next - Duration::days(1);
    while d.weekday() != wd {
        d -= Duration::days(1);
    }
    Some(d)
}

/// Easter Sunday (Gregorian, anonymous algorithm) — Good Friday is two days before.
fn easter(year: i32) -> Option<NaiveDate> {
    let a = year % 19;
    let b = year / 100;
    let c = year % 100;
    let d = b / 4;
    let e = b % 4;
    let f = (b + 8) / 25;
    let g = (b - f + 1) / 3;
    let h = (19 * a + b - d - g + 15) % 30;
    let i = c / 4;
    let k = c % 4;
    let l = (32 + 2 * e + 2 * i - h - k) % 7;
    let m = (a + 11 * h + 22 * l) / 451;
    let month = (h + l - 7 * m + 114) / 31;
    let day = (h + l - 7 * m + 114) % 31 + 1;
    NaiveDate::from_ymd_opt(year, month as u32, day as u32)
}

/// A fixed-date holiday as the exchange observes it: Saturday → the Friday before,
/// Sunday → the Monday after.
fn observed(d: NaiveDate) -> NaiveDate {
    match d.weekday() {
        Weekday::Sat => d - Duration::days(1),
        Weekday::Sun => d + Duration::days(1),
        _ => d,
    }
}

fn is_holiday(d: NaiveDate) -> bool {
    let y = d.year();
    let fixed = |m: u32, day: u32| NaiveDate::from_ymd_opt(y, m, day);
    // New Year's Day on a Saturday is NOT observed on the Friday (it would fall
    // in the previous year's books) — the one exception to `observed`.
    let new_year = fixed(1, 1).and_then(|n| match n.weekday() {
        Weekday::Sat => None,
        Weekday::Sun => Some(n + Duration::days(1)),
        _ => Some(n),
    });
    let mut days = vec![
        new_year,
        nth_weekday(y, 1, Weekday::Mon, 3),              // Martin Luther King Jr.
        nth_weekday(y, 2, Weekday::Mon, 3),              // Presidents' Day
        easter(y).map(|e| e - Duration::days(2)),        // Good Friday
        last_weekday(y, 5, Weekday::Mon),                // Memorial Day
        fixed(7, 4).map(observed),                       // Independence Day
        nth_weekday(y, 9, Weekday::Mon, 1),              // Labor Day
        nth_weekday(y, 11, Weekday::Thu, 4),             // Thanksgiving
        fixed(12, 25).map(observed),                     // Christmas
    ];
    if y >= 2022 {
        days.push(fixed(6, 19).map(observed)); // Juneteenth
    }
    days.into_iter().flatten().any(|h| h == d)
}

pub fn is_trading_day(d: NaiveDate) -> bool {
    !matches!(d.weekday(), Weekday::Sat | Weekday::Sun) && !is_holiday(d)
}

/// 13:00 ET closes: the day after Thanksgiving, Christmas Eve and July 3rd — each
/// only when it is itself a trading day (July 3rd also needs the 4th on a weekday).
pub fn is_early_close(d: NaiveDate) -> bool {
    if !is_trading_day(d) {
        return false;
    }
    let y = d.year();
    let after_thanksgiving =
        nth_weekday(y, 11, Weekday::Thu, 4).map(|t| t + Duration::days(1)) == Some(d);
    let christmas_eve = d.month() == 12 && d.day() == 24;
    let july_3 = d.month() == 7
        && d.day() == 3
        && NaiveDate::from_ymd_opt(y, 7, 4)
            .is_some_and(|f| !matches!(f.weekday(), Weekday::Sat | Weekday::Sun));
    after_thanksgiving || christmas_eve || july_3
}

pub fn close_minute(d: NaiveDate) -> u32 {
    if is_early_close(d) {
        EARLY_CLOSE_MIN
    } else {
        CLOSE_MIN
    }
}

pub fn prev_trading_day(d: NaiveDate) -> NaiveDate {
    let mut p = d - Duration::days(1);
    while !is_trading_day(p) {
        p -= Duration::days(1);
    }
    p
}

pub fn next_trading_day(d: NaiveDate) -> NaiveDate {
    let mut n = d + Duration::days(1);
    while !is_trading_day(n) {
        n += Duration::days(1);
    }
    n
}

/// The `n` trading days strictly before `d`, most recent first. Early closes are
/// left out: their after-hours runs 13:00–17:00 and would be compared slot for slot
/// with sessions that were still in regular trading.
pub fn sessions_before(d: NaiveDate, n: usize) -> Vec<NaiveDate> {
    let mut out = Vec::with_capacity(n);
    let mut cur = d;
    // Bounded walk: 60 sessions never span more than ~100 calendar days.
    for _ in 0..(n * 2 + 30) {
        if out.len() >= n {
            break;
        }
        cur = prev_trading_day(cur);
        if !is_early_close(cur) {
            out.push(cur);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn holidays_2026() {
        for closed in [
            d(2026, 1, 1),
            d(2026, 1, 19),
            d(2026, 2, 16),
            d(2026, 4, 3), // Good Friday
            d(2026, 5, 25),
            d(2026, 6, 19),
            d(2026, 7, 3), // July 4th is a Saturday → observed Friday
            d(2026, 9, 7),
            d(2026, 11, 26),
            d(2026, 12, 25),
        ] {
            assert!(!is_trading_day(closed), "{closed} should be closed");
        }
        // Columbus Day and Veterans Day are bank holidays, not exchange ones.
        assert!(is_trading_day(d(2026, 10, 12)));
        assert!(is_trading_day(d(2026, 11, 11)));
    }

    #[test]
    fn new_year_on_a_saturday_is_not_observed() {
        // 2022-01-01 was a Saturday: Friday 2021-12-31 traded.
        assert!(is_trading_day(d(2021, 12, 31)));
        // 2023-01-01 was a Sunday: Monday the 2nd was closed.
        assert!(!is_trading_day(d(2023, 1, 2)));
    }

    #[test]
    fn early_closes() {
        assert!(is_early_close(d(2026, 11, 27)));
        assert!(is_early_close(d(2026, 12, 24)));
        // July 3rd 2026 is the observed holiday itself, and the 2nd is a full day.
        assert!(!is_early_close(d(2026, 7, 3)));
        assert!(!is_early_close(d(2026, 7, 2)));
        assert!(is_early_close(d(2025, 7, 3)));
        assert_eq!(close_minute(d(2026, 11, 27)), 780);
        assert_eq!(close_minute(d(2026, 10, 12)), 960);
    }

    #[test]
    fn neighbours_skip_weekends_and_holidays() {
        // Monday after Good Friday 2026 → previous session is Thursday.
        assert_eq!(prev_trading_day(d(2026, 4, 6)), d(2026, 4, 2));
        assert_eq!(next_trading_day(d(2026, 4, 2)), d(2026, 4, 6));
        assert_eq!(prev_trading_day(d(2026, 10, 12)), d(2026, 10, 9));
    }

    #[test]
    fn sessions_before_drops_early_closes() {
        let s = sessions_before(d(2026, 12, 1), 5);
        assert_eq!(s.len(), 5);
        assert!(!s.contains(&d(2026, 11, 27)), "day after Thanksgiving is a half day");
        assert!(!s.contains(&d(2026, 11, 26)));
        assert_eq!(s[0], d(2026, 11, 30));
    }
}
