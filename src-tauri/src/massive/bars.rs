// Massive REST: historical aggregate bars, used to fill gaps Alpaca's free (IEX)
// plan leaves in chart history — see `commands::preferred_massive_bars_key` for the
// decision of WHEN this module is preferred over `alpaca::bars`.
//
// Endpoint: GET https://api.massive.com/v2/aggs/ticker/{SYM}/range/{mult}/{span}/{from_ms}/{to_ms}
//   → {"status":"OK"|"DELAYED","results":[{"o","h","l","c","v","vw","t","n"}],...}
// Verified live against a real Starter key on 2026-08-20:
//   - `timespan=second` with `multiplier=5|10` buckets server-side (no client-side
//     aggregation needed) — the only historical source for TagDash's 5s/10s charts,
//     which Alpaca's REST bars never serve at any tier.
//   - `timespan=minute` with `multiplier=1|2|5|15` returns dense, trade-count-rich
//     data even for thinly-traded names (tested on a sub-$2 low-float ticker),
//     unlike Alpaca's free-tier bars which only reflect IEX's own prints (a few
//     percent of total volume — see `crate::alpaca::plan`'s module doc).
//   - `status:"DELAYED"` only on the last ~15 minutes (Starter tier); older history
//     answers "OK" normally.
// `from`/`to` accept Unix milliseconds (not just whole dates), so the exact same
// lookback-window + `sort=desc&limit` + reverse pattern as `alpaca::bars` applies.
//
// This module is only ever reached when the caller has already confirmed we are
// NOT in a Market Replay / offline session (see `intraday_is_historical` at each
// call site) — there is no separate replay-leak guard here because that path is
// simply never taken during a replay.

use serde::Deserialize;

use crate::market_state::aggregators::{Bar, Timeframe};

use super::{with_key, BASE_URL};

/// The Massive aggregate bucket for each internal timeframe. Total, unlike
/// `alpaca::bars::alpaca_timeframe` (which returns `None` for 5s/10s) — Massive can
/// serve every timeframe TagDash displays.
pub fn massive_timespan(tf: Timeframe) -> (u32, &'static str) {
    match tf {
        Timeframe::S5 => (5, "second"),
        Timeframe::S10 => (10, "second"),
        Timeframe::M1 => (1, "minute"),
        Timeframe::M2 => (2, "minute"),
        Timeframe::M5 => (5, "minute"),
        Timeframe::M15 => (15, "minute"),
        Timeframe::Daily => (1, "day"),
    }
}

#[derive(Debug, Deserialize)]
struct AggsResponse {
    #[serde(default)]
    results: Vec<RawAgg>,
}

#[derive(Debug, Deserialize)]
struct RawAgg {
    o: f64,
    h: f64,
    l: f64,
    c: f64,
    #[serde(default)]
    v: Option<f64>,
    #[serde(default)]
    vw: Option<f64>,
    #[serde(default)]
    n: Option<u64>,
    /// Bar start, Unix milliseconds.
    t: i64,
}

impl RawAgg {
    fn into_bar(self) -> Bar {
        Bar {
            time: parse_ts_ms(self.t),
            open: self.o,
            high: self.h,
            low: self.l,
            close: self.c,
            volume: self.v.unwrap_or(0.0).max(0.0) as u64,
            vwap: self.vw,
            trade_count: self.n,
        }
    }
}

fn parse_ts_ms(ms: i64) -> chrono::DateTime<chrono::Utc> {
    chrono::TimeZone::timestamp_millis_opt(&chrono::Utc, ms)
        .single()
        .unwrap_or_else(chrono::Utc::now)
}

fn aggs_url(
    symbol: &str,
    tf: Timeframe,
    from_ms: i64,
    to_ms: i64,
    limit: u32,
    api_key: &str,
) -> String {
    let (mult, span) = massive_timespan(tf);
    with_key(
        &format!(
            "{BASE_URL}/v2/aggs/ticker/{symbol}/range/{mult}/{span}/{from_ms}/{to_ms}\
             ?adjusted=true&sort=desc&limit={limit}"
        ),
        api_key,
    )
}

/// Floor `ms` to the timeframe's bucket size. **Load-bearing, not cosmetic.**
///
/// Massive anchors a multi-unit aggregate grid to the `from` parameter, not to an
/// absolute clock grid. Verified live 2026-08-20 (FDMT, 5-minute): holding `to`
/// fixed and advancing `from` one minute at a time returned bars stamped
/// 16:18 → 16:14 → 16:15 → 16:16 — a completely different ~46-bar set each minute,
/// each with its own OHLC. Since `from_ms` was derived from `now`, every refresh a
/// minute apart re-bucketed the SAME session under fresh timestamps, and
/// `MarketState::merge_history_bars` keys intraday bars by exact open time — so each
/// poll spliced a second (then third, then fourth) copy of the day into the ring.
/// That is the "bars double, then triple" the 5m/2m/15m panes showed.
///
/// With `from` floored the answer is byte-identical on every call (16:15 / 16:10 /
/// 16:05 …) AND lands on the same UTC grid `CandleAggregator::bucket_start` uses,
/// so REST history and live websocket candles finally share one key space instead
/// of fighting over two.
///
/// 1-minute never drifted (Massive floors the anchor to the minute, so a 1× grid is
/// aligned by construction) — which is exactly why only the multi-minute panes broke.
/// Daily is left alone: a day bucket cannot shift within a day, and Massive already
/// stamps them at NY midnight (verified 04:00Z under EDT) — the very stamp
/// `crate::time::et_midnight_utc` produces for the daily aggregator.
fn aligned_from_ms(ms: i64, tf: Timeframe) -> i64 {
    if tf == Timeframe::Daily {
        return ms;
    }
    let grid = tf.seconds() * 1000;
    ms.div_euclid(grid) * grid
}

/// Translate "I want `bars` candles" into Massive's `limit`, which caps the number
/// of BASE units scanned (1 second / 1 minute / 1 day) rather than the number of
/// aggregated bars returned. Verified live: a 5-minute request with `limit=400`
/// answered `queryCount=400, resultsCount=97`. Passing the caller's bar count
/// straight through therefore delivered a fifth of the requested history on a 5m
/// chart (a fifteenth on 15m), which is why zoom-out crawled. 50 000 is the
/// endpoint's documented ceiling.
fn scan_limit(bars: u32, tf: Timeframe) -> u32 {
    let (mult, _) = massive_timespan(tf);
    bars.max(1).saturating_mul(mult).min(50_000)
}

async fn fetch(url: &str) -> Result<Vec<Bar>, String> {
    let resp = crate::http::client()
        .get(url)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!(
            "Massive aggs HTTP {status}: {}",
            &body[..body.len().min(200)]
        ));
    }
    let parsed: AggsResponse = resp.json().await.map_err(|e| e.to_string())?;
    let mut out: Vec<Bar> = parsed.results.into_iter().map(RawAgg::into_bar).collect();
    // `sort=desc` returns newest first — flip to oldest → newest for the chart.
    out.sort_by_key(|b| b.time);
    Ok(out)
}

/// Fetch the most recent `limit` bars of `tf` for ONE symbol, oldest → newest.
/// Mirrors `alpaca::bars::fetch_recent_bars`'s lookback-window sizing.
pub async fn fetch_recent_bars(
    api_key: &str,
    symbol: &str,
    tf: Timeframe,
    limit: u32,
) -> Result<Vec<Bar>, String> {
    let to_ms = chrono::Utc::now().timestamp_millis();
    let lookback_ms = tf.seconds() * (limit.max(1) as i64) * 3 * 1000 + 5 * 86_400 * 1000;
    let from_ms = aligned_from_ms(to_ms - lookback_ms, tf);
    fetch(&aggs_url(
        symbol,
        tf,
        from_ms,
        to_ms,
        scan_limit(limit, tf),
        api_key,
    ))
    .await
}

/// Aggregate today's premarket session (04:00 ET → 09:30 ET cap) into ONE
/// daily-style OHLCV bar for `symbol`, stamped at NY midnight so it lands on
/// today's daily slot. Built from Massive 1-minute bars — mirrors
/// `alpaca::bars::fetch_premarket_daily_bar` exactly, but sourced from Massive
/// so it inherits the same fuller-than-IEX coverage as the rest of this module.
/// Returns None when the symbol hasn't printed in premarket yet.
pub async fn fetch_premarket_daily_bar(api_key: &str, symbol: &str) -> Result<Option<Bar>, String> {
    let now = chrono::Utc::now();
    let start = crate::time::et_clock_utc(now, 4, 0);
    // Cap at the 09:30 cash open so a call right after the open still measures only
    // the premarket session (the regular candle takes over once premarket ends).
    let cap = crate::time::et_session_open_utc(now);
    let to = now.min(cap);

    let tf = Timeframe::M1;
    let from_ms = aligned_from_ms(start.timestamp_millis(), tf);
    let mbars = fetch(&aggs_url(
        symbol,
        tf,
        from_ms,
        to.timestamp_millis(),
        scan_limit(500, tf),
        api_key,
    ))
    .await?;
    let mut bars = mbars.iter().filter(|b| b.time < cap);
    let Some(first) = bars.next() else {
        return Ok(None);
    };
    let (mut high, mut low, mut close) = (first.high, first.low, first.close);
    let open = first.open;
    let mut volume: u64 = first.volume;
    let mut pv: f64 = first.vwap.unwrap_or(first.close) * first.volume as f64;
    let mut trades: u64 = first.trade_count.unwrap_or(0);
    for b in bars {
        high = high.max(b.high);
        low = low.min(b.low);
        close = b.close;
        volume += b.volume;
        pv += b.vwap.unwrap_or(b.close) * b.volume as f64;
        trades += b.trade_count.unwrap_or(0);
    }

    Ok(Some(Bar {
        time: crate::time::et_midnight_utc(now),
        open,
        high,
        low,
        close,
        volume,
        vwap: if volume > 0 {
            Some(pv / volume as f64)
        } else {
            None
        },
        trade_count: Some(trades),
    }))
}

/// Fetch up to `limit` bars of `tf` for ONE symbol ending at/before `end` (RFC3339),
/// oldest → newest. The primitive the chart uses to lazily back-fill older history.
pub async fn fetch_bars_before(
    api_key: &str,
    symbol: &str,
    tf: Timeframe,
    end: &str,
    limit: u32,
) -> Result<Vec<Bar>, String> {
    let to_ms = chrono::DateTime::parse_from_rfc3339(end)
        .map(|d| d.timestamp_millis())
        .unwrap_or_else(|_| chrono::Utc::now().timestamp_millis());
    let lookback_ms = tf.seconds() * (limit.max(1) as i64) * 3 * 1000 + 5 * 86_400 * 1000;
    let from_ms = aligned_from_ms(to_ms - lookback_ms, tf);
    fetch(&aggs_url(
        symbol,
        tf,
        from_ms,
        to_ms,
        scan_limit(limit, tf),
        api_key,
    ))
    .await
}

/// Fetch every bar of `tf` for ONE symbol over [`from_ms`, `to_ms`], oldest →
/// newest. The range counterpart of `fetch_recent_bars`: both ends are pinned and
/// the count is not capped by a caller-chosen bar budget — `limit` goes straight to
/// the endpoint's documented 50 000 ceiling, which measures BASE units scanned (see
/// `scan_limit`). The premarket folder sizes its own windows so that ceiling is
/// never reached (60 calendar days = ~35 600 base minutes on AAPL, the worst case
/// TagDash charts); it must, because `sort=desc` truncation drops the OLDEST bars —
/// exactly the days a scroll-back into history is asking for.
pub async fn fetch_range(
    api_key: &str,
    symbol: &str,
    tf: Timeframe,
    from_ms: i64,
    to_ms: i64,
) -> Result<Vec<Bar>, String> {
    if to_ms <= from_ms {
        return Ok(vec![]);
    }
    fetch(&aggs_url(
        symbol,
        tf,
        aligned_from_ms(from_ms, tf),
        to_ms,
        50_000,
        api_key,
    ))
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn massive_serves_every_timeframe_alpaca_and_more() {
        assert_eq!(massive_timespan(Timeframe::S5), (5, "second"));
        assert_eq!(massive_timespan(Timeframe::S10), (10, "second"));
        assert_eq!(massive_timespan(Timeframe::M1), (1, "minute"));
        assert_eq!(massive_timespan(Timeframe::Daily), (1, "day"));
    }

    #[test]
    fn parses_and_reverses_to_ascending() {
        let body = r#"{"status":"OK","results":[
            {"o":10.0,"h":10.5,"l":9.9,"c":10.2,"v":100,"vw":10.1,"n":5,"t":2000},
            {"o":9.0,"h":9.5,"l":8.9,"c":9.2,"v":50,"n":2,"t":1000}
        ]}"#;
        let parsed: AggsResponse = serde_json::from_str(body).unwrap();
        let mut out: Vec<Bar> = parsed.results.into_iter().map(RawAgg::into_bar).collect();
        out.sort_by_key(|b| b.time);
        assert_eq!(out.len(), 2);
        assert!(out[0].time < out[1].time);
        assert_eq!(out[0].close, 9.2);
        assert_eq!(out[1].close, 10.2);
        assert_eq!(out[1].trade_count, Some(5));
    }

    // The bug that made 5m/2m/15m panes duplicate their bars once a minute: an
    // unaligned `from` moves Massive's whole bucket grid with it, so the same
    // session comes back under fresh timestamps on every call.
    #[test]
    fn from_ms_is_floored_onto_the_bucket_grid() {
        // 5-minute grid = 300_000 ms. Any instant inside a bucket must collapse to
        // that bucket's start, so `from` is identical for a whole 5 minutes.
        let base = 1_787_242_500_000i64; // verified live: an exact 5-min boundary
        assert_eq!(base % 300_000, 0);
        for offset in [0, 1, 59_999, 60_000, 137_000, 299_999] {
            assert_eq!(
                aligned_from_ms(base + offset, Timeframe::M5),
                base,
                "offset {offset} must not shift the 5-minute grid",
            );
        }
        // …and the next bucket does advance, exactly once, on the boundary.
        assert_eq!(
            aligned_from_ms(base + 300_000, Timeframe::M5),
            base + 300_000
        );
    }

    #[test]
    fn daily_from_ms_is_left_untouched() {
        // Massive already stamps day bars at NY midnight (= `time::et_midnight_utc`);
        // flooring to a UTC 86 400 s grid would drag them onto the wrong day.
        let ms = 1_787_198_400_000; // 04:00Z = NY midnight under EDT
        assert_eq!(aligned_from_ms(ms, Timeframe::Daily), ms);
    }

    // Massive's `limit` caps BASE units scanned, not bars returned — verified live
    // (5-min, limit=400 → queryCount=400, resultsCount=97).
    #[test]
    fn scan_limit_scales_with_the_multiplier() {
        assert_eq!(scan_limit(400, Timeframe::M1), 400); // 1× → unchanged
        assert_eq!(scan_limit(400, Timeframe::M5), 2_000); // 5× the base minutes
        assert_eq!(scan_limit(400, Timeframe::M15), 6_000);
        assert_eq!(scan_limit(400, Timeframe::S10), 4_000);
        assert_eq!(scan_limit(600, Timeframe::Daily), 600); // 1 day per bar
        assert_eq!(
            scan_limit(u32::MAX, Timeframe::M15),
            50_000,
            "clamped, never overflows"
        );
    }

    #[test]
    fn missing_volume_defaults_to_zero() {
        let body = r#"{"results":[{"o":1.0,"h":1.0,"l":1.0,"c":1.0,"t":1000}]}"#;
        let parsed: AggsResponse = serde_json::from_str(body).unwrap();
        let bar = RawAgg::into_bar(parsed.results.into_iter().next().unwrap());
        assert_eq!(bar.volume, 0);
        assert_eq!(bar.vwap, None);
        assert_eq!(bar.trade_count, None);
    }
}
