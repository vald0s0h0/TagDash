// The premarket slice of a daily chart: ONE 04:00→09:30 ET OHLCV candle per
// trading day, so the daily pane can draw the premarket move as a ghost behind
// (or instead of) the regular-session candle.
//
// WHY THIS MODULE HAS TO EXIST — measured, not assumed. Both providers' daily
// bars are REGULAR SESSION ONLY. Verified live 2026-09-22 on AAPL 2026-09-18:
//   Alpaca  1Day       → o=337.905 h=338.49 l=332.53
//   Massive 1/day      → o=337.905 h=338.49 l=332.53
//   15-min 09:30→16:00 → o=337.91  h=338.49 l=332.53   ← identical
//   15-min 04:00→09:30 → o=337.2   h=339.1  l=335.79   ← nowhere in the daily bar
// So a premarket high above the day's range is INVISIBLE on a daily chart today,
// and no provider endpoint hands it over: it has to be folded from intraday bars.
//
// GRANULARITY — 15-minute, and that is not a precision compromise. The ET offset
// is a whole number of hours, so the UTC 15-minute grid lands exactly on 04:00 and
// on 09:30: no bucket ever straddles a session boundary. The high of a 15-minute
// bar is already the max of its minutes, so the folded OHLC is BIT-IDENTICAL to
// one folded from 1-minute bars — only the intra-session resolution is lost, and
// this module emits one candle per day anyway. Cost: 64 bars/day instead of 960.
//
// WINDOW SIZE — 60 calendar days per provider request. Massive's `limit` caps the
// number of BASE units (minutes) scanned, not bars returned; measured live on the
// two extremes of what TagDash charts:
//   AAPL 60 d → queryCount=35 585 / 50 000   (the most-traded name there is)
//   SNYR 60 d → queryCount=14 721 / 50 000   (a sub-$1 runner, 39 sessions)
// Both land well inside the ceiling, so 60 days is one call. Longer windows are
// chunked rather than truncated — `sort=desc` would silently drop the OLDEST days,
// i.e. exactly the ones a scroll-back is asking for.

use chrono::{DateTime, Duration, Utc};

use crate::market_state::aggregators::{Bar, Timeframe};

/// The intraday granularity a premarket candle is folded from (see module doc).
pub const FOLD_TF: Timeframe = Timeframe::M15;

/// Calendar days covered by ONE provider request (see module doc).
pub const CHUNK_DAYS: i64 = 60;

/// Default history the alert pre-warm pulls, in calendar days — three
/// `CHUNK_DAYS` requests. Sized to cover a daily pane's DEFAULT view (~100 bars at
/// the default bar spacing ≈ 5 months), so switching the ghost on shows something
/// at once instead of after a fetch; scrolling further back extends it on demand.
/// Keep in sync with `DEFAULT_WINDOW_DAYS` in `usePremarketBars.ts`.
pub const DEFAULT_WINDOW_DAYS: i64 = 180;

/// Which provider serves the intraday bars. Owned (not borrowed from AppState) so
/// it can cross a `spawn` boundary into the alert pre-warm task.
#[derive(Debug, Clone)]
pub enum Source {
    /// Massive — only when no Alpaca key is configured.
    Massive(String),
    /// Alpaca (key, secret) — the default: full SIP bars on every plan.
    Alpaca { key: String, secret: String },
}

/// Fold intraday bars into one premarket daily candle per NY trading day.
///
/// Only bars whose ET wall time is in [04:00, 09:30) contribute; each day's candle
/// is stamped at NY midnight (`time::et_midnight_utc`) — the very stamp both
/// providers' daily bars carry, so a premarket candle lands on the SAME time slot
/// as the regular candle it belongs under. Without that the ghost would sit in its
/// own column on the chart's time scale instead of behind the day it describes.
///
/// Input order does not matter (bars are sorted first); output is date-ascending.
pub fn fold_premarket_daily(bars: &[Bar]) -> Vec<Bar> {
    use std::collections::BTreeMap;

    // Accumulator per NY date. ISO dates sort lexicographically == chronologically,
    // so a BTreeMap keyed by the date string comes out ascending for free.
    struct Acc {
        bar: Bar,
        /// Σ price×volume, for the volume-weighted average of the whole session.
        pv: f64,
    }
    let mut by_day: BTreeMap<String, Acc> = BTreeMap::new();

    let mut window: Vec<&Bar> = bars
        .iter()
        .filter(|b| crate::time::is_premarket(b.time))
        .collect();
    window.sort_by_key(|b| b.time);

    for b in window {
        let day = crate::time::et_date(b.time);
        let pv = b.vwap.unwrap_or(b.close) * b.volume as f64;
        by_day
            .entry(day)
            .and_modify(|a| {
                a.bar.high = a.bar.high.max(b.high);
                a.bar.low = a.bar.low.min(b.low);
                a.bar.close = b.close; // bars are ascending → the last one wins
                a.bar.volume += b.volume;
                a.bar.trade_count =
                    Some(a.bar.trade_count.unwrap_or(0) + b.trade_count.unwrap_or(0));
                a.pv += pv;
            })
            .or_insert_with(|| Acc {
                bar: Bar {
                    time: crate::time::et_midnight_utc(b.time),
                    open: b.open,
                    high: b.high,
                    low: b.low,
                    close: b.close,
                    volume: b.volume,
                    vwap: None, // filled below, once the whole session is known
                    trade_count: b.trade_count,
                },
                pv,
            });
    }

    by_day
        .into_values()
        .map(|a| Bar {
            vwap: (a.bar.volume > 0).then(|| a.pv / a.bar.volume as f64),
            ..a.bar
        })
        .collect()
}

/// Premarket daily candles for `symbol` over [`start`, `end`], one per trading day
/// that actually printed in premarket. Chunked into `CHUNK_DAYS` provider requests
/// (see module doc) and folded. Days with no premarket print simply produce no
/// candle — the caller must remember the range it ASKED for, never infer coverage
/// from the rows it got back, or an illiquid name is re-fetched forever.
pub async fn fetch_window(
    src: &Source,
    symbol: &str,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Result<Vec<Bar>, String> {
    if end <= start {
        return Ok(vec![]);
    }
    let mut out: Vec<Bar> = Vec::new();
    let mut cursor = start;
    while cursor < end {
        let chunk_end = (cursor + Duration::days(CHUNK_DAYS)).min(end);
        let raw = match src {
            Source::Massive(key) => {
                crate::massive::bars::fetch_range(
                    key,
                    symbol,
                    FOLD_TF,
                    cursor.timestamp_millis(),
                    chunk_end.timestamp_millis(),
                )
                .await?
            }
            Source::Alpaca { key, secret } => {
                crate::alpaca::bars::fetch_bars_range(
                    key, secret, symbol, FOLD_TF, cursor, chunk_end,
                )
                .await?
            }
        };
        out.extend(fold_premarket_daily(&raw));
        cursor = chunk_end;
    }
    out.sort_by_key(|b| b.time);
    out.dedup_by_key(|b| b.time);
    Ok(out)
}

/// Make sure the premarket cache covers [`from_date`, `to_date`] for `symbol`,
/// fetching only the part it does not already hold.
///
/// Gaps are computed against the range that was ASKED for, recorded in
/// `premarket_coverage` — never against the rows that came back. A day on which the
/// symbol never printed in premarket legitimately yields no row, and reading that
/// as "not fetched yet" would re-hit the provider on every chart open for exactly
/// the illiquid names this feature is most useful on.
///
/// Shared by the chart command (which asks for the window on screen) and the alert
/// pre-warm (which asks for the default window the moment a ticker is alerted), so
/// the two can never drift apart. Never call it offline: it reaches the network
/// unconditionally. During a replay the caller must bound `to_date` by the
/// simulated clock — the provider windows are NOT clamped here, and only the
/// premarket fold keeps a same-day fetch after 09:30 from carrying later bars.
pub async fn ensure_cached(
    src: &Source,
    db: &std::sync::Arc<std::sync::Mutex<rusqlite::Connection>>,
    symbol: &str,
    from_date: &str,
    to_date: &str,
) {
    use crate::local_db::premarket_repository as repo;

    let coverage = {
        let conn = db.lock().unwrap();
        repo::get_coverage(&conn, symbol)
    };
    for (gap_from, gap_to) in missing_ranges(coverage.as_ref(), from_date, to_date) {
        // The provider window runs from 00:00 ET of the first day to 00:00 ET of
        // the day AFTER the last, so `gap_to` is fully included.
        let (Some(start), Some(end)) = (
            day_start_utc(&gap_from),
            day_start_utc(&shift_date(&gap_to, 1)),
        ) else {
            continue;
        };
        match fetch_window(src, symbol, start, end).await {
            Ok(bars) => {
                let mut conn = db.lock().unwrap();
                if let Err(e) = repo::upsert_many(&mut conn, symbol, &bars) {
                    // Never claim coverage for a window we failed to store, or the
                    // gap is silently lost until the cache is wiped.
                    eprintln!("[tagdash] premarket cache write {symbol} failed: {e}");
                    continue;
                }
                let _ = repo::extend_coverage(&conn, symbol, &gap_from, &gap_to);
            }
            Err(e) => {
                eprintln!("[tagdash] premarket window {symbol} {gap_from}→{gap_to} failed: {e}")
            }
        }
    }
}

/// The sub-ranges of [`from_date`, `to_date`] that `coverage` does not already
/// hold, as inclusive (from, to) date pairs.
///
/// Coverage is ONE contiguous span per symbol, so a request can only stick out on
/// the left (scrolling further into history), on the right (a new session), or
/// both — never as a hole in the middle. Keeping it contiguous is what makes this
/// answer cheap and, more importantly, what makes "no row for that day" mean "the
/// symbol did not trade in premarket" instead of "we never looked".
fn missing_ranges(
    coverage: Option<&(String, String)>,
    from_date: &str,
    to_date: &str,
) -> Vec<(String, String)> {
    if from_date > to_date {
        return vec![];
    }
    let Some((first, last)) = coverage else {
        return vec![(from_date.to_string(), to_date.to_string())];
    };
    let mut gaps = Vec::new();
    if from_date < first.as_str() {
        // Stop one day short of what we hold, never overlap it.
        gaps.push((from_date.to_string(), shift_date(first, -1)));
    }
    if to_date > last.as_str() {
        gaps.push((shift_date(last, 1), to_date.to_string()));
    }
    gaps
}

/// `date` (`YYYY-MM-DD`) shifted by `days`, same format. An unparseable input is
/// returned unchanged — a malformed date must not silently become another day.
pub fn shift_date(date: &str, days: i64) -> String {
    match chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d") {
        Ok(d) => (d + Duration::days(days)).format("%Y-%m-%d").to_string(),
        Err(_) => date.to_string(),
    }
}

/// UTC instant of 00:00 ET on `date` (`YYYY-MM-DD`), DST-aware.
pub fn day_start_utc(date: &str) -> Option<DateTime<Utc>> {
    let d = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
    // Noon UTC is inside that NY calendar day whatever the offset, so
    // `et_midnight_utc` resolves the right stamp from it (00:00 ET would not:
    // under EDT it lands on the previous NY day).
    Some(crate::time::et_midnight_utc(d.and_hms_opt(12, 0, 0)?.and_utc()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    /// A bar at `h:m` ET on 2026-09-18 (EDT, UTC-4).
    fn et_bar(h: u32, m: u32, o: f64, high: f64, low: f64, c: f64, v: u64) -> Bar {
        Bar {
            time: Utc
                .with_ymd_and_hms(2026, 9, 18, h + 4, m, 0)
                .single()
                .unwrap(),
            open: o,
            high,
            low,
            close: c,
            volume: v,
            vwap: None,
            trade_count: Some(1),
        }
    }

    #[test]
    fn folds_one_candle_per_day_from_the_premarket_window_only() {
        // 04:00 and 09:15 are premarket; 09:30 and 16:00 are not and must not leak
        // into the candle — that leak is the whole bug this module exists to avoid.
        let bars = vec![
            et_bar(4, 0, 10.0, 11.0, 9.5, 10.5, 100),
            et_bar(9, 15, 10.5, 12.0, 10.4, 11.8, 200),
            et_bar(9, 30, 11.8, 30.0, 1.0, 20.0, 999), // regular session
            et_bar(16, 0, 20.0, 40.0, 0.5, 21.0, 999), // after hours
        ];
        let out = fold_premarket_daily(&bars);
        assert_eq!(out.len(), 1);
        let pm = &out[0];
        assert_eq!(pm.open, 10.0, "open = the 04:00 bar's open");
        assert_eq!(pm.high, 12.0, "high must ignore the 09:30 bar's 30.0");
        assert_eq!(pm.low, 9.5, "low must ignore the 16:00 bar's 0.5");
        assert_eq!(pm.close, 11.8, "close = the last bar before 09:30");
        assert_eq!(pm.volume, 300);
        assert_eq!(pm.trade_count, Some(2));
    }

    #[test]
    fn stamps_the_candle_at_ny_midnight_so_it_shares_the_daily_slot() {
        let out = fold_premarket_daily(&[et_bar(7, 0, 1.0, 1.0, 1.0, 1.0, 1)]);
        // NY midnight on an EDT day = 04:00Z — the stamp both providers' daily bars
        // carry, and the one `et_midnight_utc` produces.
        assert_eq!(
            out[0].time,
            Utc.with_ymd_and_hms(2026, 9, 18, 4, 0, 0).unwrap()
        );
    }

    #[test]
    fn unordered_input_still_folds_open_and_close_correctly() {
        let bars = vec![
            et_bar(9, 15, 10.5, 12.0, 10.4, 11.8, 200),
            et_bar(4, 0, 10.0, 11.0, 9.5, 10.5, 100),
        ];
        let out = fold_premarket_daily(&bars);
        assert_eq!(out[0].open, 10.0);
        assert_eq!(out[0].close, 11.8);
    }

    #[test]
    fn vwap_is_volume_weighted_over_the_whole_session() {
        let mut a = et_bar(4, 0, 10.0, 10.0, 10.0, 10.0, 100);
        a.vwap = Some(10.0);
        let mut b = et_bar(5, 0, 20.0, 20.0, 20.0, 20.0, 300);
        b.vwap = Some(20.0);
        let out = fold_premarket_daily(&[a, b]);
        // (10×100 + 20×300) / 400 = 17.5 — not the 15.0 a naive mean would give.
        assert_eq!(out[0].vwap, Some(17.5));
    }

    #[test]
    fn a_day_with_no_premarket_print_yields_no_candle() {
        let out = fold_premarket_daily(&[et_bar(11, 0, 1.0, 1.0, 1.0, 1.0, 5)]);
        assert!(
            out.is_empty(),
            "no premarket bar → no candle, never a zero candle"
        );
    }

    fn cov(a: &str, b: &str) -> (String, String) {
        (a.to_string(), b.to_string())
    }

    #[test]
    fn an_uncached_symbol_fetches_the_whole_window() {
        assert_eq!(
            missing_ranges(None, "2026-07-01", "2026-09-01"),
            vec![cov("2026-07-01", "2026-09-01")],
        );
    }

    #[test]
    fn a_fully_covered_window_fetches_nothing() {
        let c = cov("2026-06-01", "2026-10-01");
        assert!(missing_ranges(Some(&c), "2026-07-01", "2026-09-01").is_empty());
        // Exact edges count as covered — an off-by-one here re-fetches a full
        // chunk on every single chart open.
        assert!(missing_ranges(Some(&c), "2026-06-01", "2026-10-01").is_empty());
    }

    #[test]
    fn scrolling_back_fetches_only_the_days_before_what_we_hold() {
        let c = cov("2026-07-01", "2026-09-01");
        assert_eq!(
            missing_ranges(Some(&c), "2026-05-01", "2026-09-01"),
            vec![cov("2026-05-01", "2026-06-30")],
            "must stop the day before the covered span, never overlap it",
        );
    }

    #[test]
    fn a_new_session_fetches_only_the_tail() {
        let c = cov("2026-07-01", "2026-09-01");
        assert_eq!(
            missing_ranges(Some(&c), "2026-07-01", "2026-09-03"),
            vec![cov("2026-09-02", "2026-09-03")],
        );
    }

    #[test]
    fn a_window_wider_on_both_sides_fetches_both_ends_and_not_the_middle() {
        let c = cov("2026-07-01", "2026-09-01");
        assert_eq!(
            missing_ranges(Some(&c), "2026-06-01", "2026-09-30"),
            vec![cov("2026-06-01", "2026-06-30"), cov("2026-09-02", "2026-09-30")],
        );
    }

    #[test]
    fn an_inverted_window_fetches_nothing() {
        // Happens before the 09:30 open: the cacheable range ends yesterday while a
        // caller asks from today. It is empty, not an error, and must never be sent
        // to the provider as a backwards range.
        assert!(missing_ranges(None, "2026-09-18", "2026-09-17").is_empty());
    }

    #[test]
    fn separate_days_fold_separately_and_come_out_ascending() {
        let day1 = et_bar(5, 0, 1.0, 2.0, 0.5, 1.5, 10);
        let mut day2 = et_bar(5, 0, 3.0, 4.0, 2.5, 3.5, 20);
        day2.time += Duration::days(1);
        let out = fold_premarket_daily(&[day2.clone(), day1]);
        assert_eq!(out.len(), 2);
        assert!(out[0].time < out[1].time);
        assert_eq!(out[0].high, 2.0);
        assert_eq!(out[1].high, 4.0);
    }
}
