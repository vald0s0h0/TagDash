// Centralised US Eastern Time + market-session helpers.
//
// All ET / session logic lives here so the rest of the app never hardcodes the
// UTC offset. The offset follows US daylight-saving rules — EDT (UTC−4) from the
// 2nd Sunday of March, EST (UTC−5) from the 1st Sunday of November — computed from
// the UTC instant alone (no external tz database, no network dependency).
//
// Every function takes the UTC instant explicitly rather than reading the wall
// clock, so the same code drives both live mode (`Utc::now()`) and, later, the
// market-replay clock.

use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Timelike, Utc};

use crate::types::Session;

/// The app clock: real `Utc::now()` in live mode, the simulated instant while a
/// Market Replay is active. Every engine / command that needs "now" for market
/// logic (session gates, alert timestamps, fills, trade IDs, "today") must call
/// this instead of `Utc::now()`, so the whole app follows the replayed day.
/// Infrastructure timestamps (feed diagnostics, sync-queue bookkeeping…) keep
/// using `Utc::now()` on purpose.
pub fn now() -> DateTime<Utc> {
    if crate::replay::clock::is_active() {
        crate::replay::clock::sim_now()
    } else {
        Utc::now()
    }
}

/// US Eastern UTC offset in hours for `instant`: 4 during EDT, 5 during EST.
///
/// US daylight-saving transitions (current rules, since 2007):
///   • spring forward — 2nd Sunday of March, 02:00 EST → 03:00 EDT (= 07:00 UTC)
///   • fall back      — 1st Sunday of November, 02:00 EDT → 01:00 EST (= 06:00 UTC)
pub fn et_offset_hours(instant: DateTime<Utc>) -> i64 {
    let year = instant.year();
    let dst_start = nth_sunday_utc(year, 3, 2, 7); // 2nd Sunday March, 07:00 UTC
    let dst_end = nth_sunday_utc(year, 11, 1, 6); // 1st Sunday November, 06:00 UTC
    match (dst_start, dst_end) {
        (Some(start), Some(end)) if instant >= start && instant < end => 4,
        _ => 5,
    }
}

/// UTC instant of the `nth` Sunday of (`year`, `month`) at `hour`:00:00 UTC.
fn nth_sunday_utc(year: i32, month: u32, nth: u32, hour: u32) -> Option<DateTime<Utc>> {
    let first = NaiveDate::from_ymd_opt(year, month, 1)?;
    // Days from the 1st to the first Sunday (Sunday = 0 days-from-Sunday).
    let to_first_sunday = (7 - first.weekday().num_days_from_sunday()) % 7;
    let day = 1 + to_first_sunday + (nth - 1) * 7;
    let naive = NaiveDate::from_ymd_opt(year, month, day)?.and_hms_opt(hour, 0, 0)?;
    Some(Utc.from_utc_datetime(&naive))
}

/// `instant` carried as ET wall-clock in a `DateTime<Utc>` (its Y/M/D/H/M fields
/// read as Eastern local time). Mirrors the shift the app already used, with the
/// correct DST-aware offset. Use the accessors below rather than this directly.
pub fn to_et(instant: DateTime<Utc>) -> DateTime<Utc> {
    instant - Duration::hours(et_offset_hours(instant))
}

/// ET wall-clock minutes since midnight (0..=1439).
pub fn et_minutes(instant: DateTime<Utc>) -> u32 {
    let et = to_et(instant);
    et.hour() * 60 + et.minute()
}

/// ET calendar date as `YYYY-MM-DD`.
pub fn et_date(instant: DateTime<Utc>) -> String {
    to_et(instant).format("%Y-%m-%d").to_string()
}

/// UTC instant of `hour:min` ET wall-clock on `instant`'s ET calendar day
/// (DST-aware). The generic primitive behind `et_session_open_utc`: lets callers
/// pin Alpaca REST windows to any ET wall time regardless of EST/EDT — e.g. the
/// 04:00 ET premarket open or the 09:30 cash open.
pub fn et_clock_utc(instant: DateTime<Utc>, hour: u32, min: u32) -> DateTime<Utc> {
    let offset = et_offset_hours(instant);
    let et_day = (instant - Duration::hours(offset)).date_naive();
    // hh:mm ET = (hh:mm + offset) UTC on the same ET calendar day.
    let naive = et_day.and_hms_opt(hour, min, 0).expect("valid wall time");
    Utc.from_utc_datetime(&naive) + Duration::hours(offset)
}

/// UTC instant of the 09:30 ET regular-session open on `instant`'s ET day
/// (DST-aware). Lets callers pin Alpaca REST windows to the cash open regardless
/// of EST/EDT — e.g. 13:30Z in summer, 14:30Z in winter.
pub fn et_session_open_utc(instant: DateTime<Utc>) -> DateTime<Utc> {
    et_clock_utc(instant, 9, 30)
}

/// Market session at `instant`, from ET wall time. Boundaries are unchanged from
/// the previous `scanner::current_session`:
///   • 04:00–09:29 premarket · 09:30–09:49 pre-open · 09:50–15:59 open · else AH.
pub fn session_at(instant: DateTime<Utc>) -> Session {
    match et_minutes(instant) {
        240..=569 => Session::Premarket,
        570..=589 => Session::PreOpen,
        590..=959 => Session::Open,
        _ => Session::Afterhours,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    #[test]
    fn dst_offset_summer_and_winter() {
        assert_eq!(et_offset_hours(utc("2026-06-08T12:00:00Z")), 4); // EDT
        assert_eq!(et_offset_hours(utc("2026-01-15T12:00:00Z")), 5); // EST
    }

    #[test]
    fn dst_transition_instants_2026() {
        // 2026 spring forward: 2nd Sunday of March = March 8, 07:00 UTC.
        assert_eq!(et_offset_hours(utc("2026-03-08T06:59:00Z")), 5);
        assert_eq!(et_offset_hours(utc("2026-03-08T07:00:00Z")), 4);
        // 2026 fall back: 1st Sunday of November = Nov 1, 06:00 UTC.
        assert_eq!(et_offset_hours(utc("2026-11-01T05:59:00Z")), 4);
        assert_eq!(et_offset_hours(utc("2026-11-01T06:00:00Z")), 5);
    }

    #[test]
    fn nth_sunday_resolves_2026() {
        assert_eq!(nth_sunday_utc(2026, 3, 2, 7).unwrap(), utc("2026-03-08T07:00:00Z"));
        assert_eq!(nth_sunday_utc(2026, 11, 1, 6).unwrap(), utc("2026-11-01T06:00:00Z"));
    }

    #[test]
    fn session_boundaries_hold_in_both_offsets() {
        // EDT: 09:30 ET = 13:30 UTC.
        assert_eq!(session_at(utc("2026-06-08T11:59:00Z")), Session::Premarket); // 07:59 ET
        assert_eq!(session_at(utc("2026-06-08T13:30:00Z")), Session::PreOpen); // 09:30 ET
        assert_eq!(session_at(utc("2026-06-08T13:50:00Z")), Session::Open); // 09:50 ET
        assert_eq!(session_at(utc("2026-06-08T20:30:00Z")), Session::Afterhours); // 16:30 ET
        // EST: the same wall-clock sessions hold one UTC hour later.
        assert_eq!(session_at(utc("2026-01-15T14:30:00Z")), Session::PreOpen); // 09:30 ET
        assert_eq!(session_at(utc("2026-01-15T14:50:00Z")), Session::Open); // 09:50 ET
    }

    #[test]
    fn et_date_rolls_at_eastern_midnight() {
        // 23:00 EST = 04:00 UTC next day, but the ET date is still the prior day.
        assert_eq!(et_date(utc("2026-01-16T04:00:00Z")), "2026-01-15");
    }

    #[test]
    fn session_open_utc_is_dst_aware() {
        assert_eq!(et_session_open_utc(utc("2026-06-08T12:00:00Z")), utc("2026-06-08T13:30:00Z"));
        assert_eq!(et_session_open_utc(utc("2026-01-15T12:00:00Z")), utc("2026-01-15T14:30:00Z"));
    }
}
