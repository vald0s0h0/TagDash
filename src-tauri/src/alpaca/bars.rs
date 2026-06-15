// Alpaca REST: bulk daily bars for a list of symbols.
// Real endpoint: GET https://data.alpaca.markets/v2/stocks/bars
// Returns HashMap<symbol, Vec<BarData>> (up to `limit` bars per symbol).

use std::collections::HashMap;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BarData {
    pub symbol: String,
    pub date: String,      // YYYY-MM-DD
    pub open: Option<f64>,
    pub high: Option<f64>,
    pub low: Option<f64>,
    pub close: Option<f64>,
    pub volume: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct BarsResponse {
    #[serde(default)]
    bars: HashMap<String, Vec<RawBar>>,
    #[serde(default)]
    next_page_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawBar {
    t: String, // RFC3339 timestamp
    o: f64,
    h: f64,
    l: f64,
    c: f64,
    v: i64,
    /// Trade count (`n`) — present on minute bars, absent on some aggregations.
    #[serde(default)]
    n: Option<i64>,
    /// Volume-weighted average price (`vw`) — present on minute bars.
    #[serde(default)]
    vw: Option<f64>,
}

/// Market-Replay leak guard: the effective `end` query parameter for a bars
/// request of `tf_secs` granularity. In live mode this is just the caller's
/// requested end (or none). While a replay is active the end is clamped to the
/// simulated instant minus one bucket, so a bar overlapping the simulated
/// "future" — including the replay day's own daily bar, whose close would leak —
/// can never be returned. The earlier of (requested, clamp) wins.
fn effective_end(tf_secs: i64, requested: Option<&str>) -> Option<String> {
    let clamp = crate::replay::clock::rest_end_clamp(tf_secs);
    match (requested, clamp) {
        (None, c) => c,
        (Some(r), None) => Some(r.to_string()),
        (Some(r), Some(c)) => {
            let rp = chrono::DateTime::parse_from_rfc3339(r)
                .map(|d| d.with_timezone(&chrono::Utc));
            let cp = chrono::DateTime::parse_from_rfc3339(&c)
                .map(|d| d.with_timezone(&chrono::Utc));
            match (rp, cp) {
                (Ok(r0), Ok(c0)) if r0 <= c0 => Some(r.to_string()),
                _ => Some(c),
            }
        }
    }
}

/// Append `&end=…` to a bars URL when an effective end applies.
fn push_end(url: &mut String, tf_secs: i64, requested: Option<&str>) {
    if let Some(end) = effective_end(tf_secs, requested) {
        url.push_str(&format!("&end={end}"));
    }
}

/// Fetch the last `limit_days` daily bars per symbol.
/// Alpaca caps requests at 1 000 symbols and paginates the response with
/// `next_page_token`, so we chunk the symbol list and follow pagination.
pub async fn fetch_daily_bars(
    key: &str,
    secret: &str,
    symbols: &[String],
    limit_days: u32,
) -> Result<HashMap<String, Vec<BarData>>, String> {
    use chrono::Duration;

    // Go back enough calendar days to cover `limit_days` trading days
    // (weekends + holidays), then truncate to the most recent ones below.
    // App clock (`time::now`): in replay the window is relative to the simulated
    // day, in live mode it is identical to Utc::now().
    let start = (crate::time::now() - Duration::days((limit_days as i64).max(1) * 2 + 10))
        .format("%Y-%m-%d")
        .to_string();
    let mut result = fetch_daily_bars_since(key, secret, symbols, &start).await?;

    // Keep only the most recent `limit_days` per symbol (bars are date-ascending).
    let keep = limit_days as usize;
    for bars in result.values_mut() {
        if bars.len() > keep {
            let drop = bars.len() - keep;
            bars.drain(0..drop);
        }
    }
    Ok(result)
}

/// Fetch every daily bar from `start_date` (YYYY-MM-DD, inclusive) to now, with
/// no per-symbol truncation. This is the incremental primitive the startup
/// pipeline uses: on the first run `start_date` is ~250 trading days back; on
/// later runs it is the last cached bar date, so only the missing days come over
/// the wire and are upserted into the daily cache.
pub async fn fetch_daily_bars_since(
    key: &str,
    secret: &str,
    symbols: &[String],
    start_date: &str,
) -> Result<HashMap<String, Vec<BarData>>, String> {
    if symbols.is_empty() {
        return Ok(HashMap::new());
    }
    let client = reqwest::Client::new();
    let start = format!("{start_date}T00:00:00Z");

    let mut result: HashMap<String, Vec<BarData>> = HashMap::new();

    // ≤200 symbols/request keeps the URL short; pagination handles bar volume.
    for chunk in symbols.chunks(200) {
        let sym_str = chunk.join(",");
        let mut page_token: Option<String> = None;
        loop {
            let mut url = format!(
                "https://data.alpaca.markets/v2/stocks/bars?symbols={sym_str}\
                 &timeframe=1Day&start={start}&limit=10000&adjustment=split"
            );
            push_end(&mut url, 86_400, None); // replay guard: no future daily bar
            if let Some(tok) = &page_token {
                url.push_str(&format!("&page_token={tok}"));
            }
            let resp = client
                .get(&url)
                .header("APCA-API-KEY-ID", key)
                .header("APCA-API-SECRET-KEY", secret)
                .send()
                .await
                .map_err(|e| e.to_string())?;

            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                return Err(format!("Alpaca bars HTTP {status}: {body}"));
            }

            let raw: BarsResponse = resp.json().await.map_err(|e| e.to_string())?;
            for (sym, bars) in raw.bars {
                let entry = result.entry(sym.clone()).or_default();
                for b in bars {
                    entry.push(BarData {
                        symbol: sym.clone(),
                        date: b.t.get(..10).unwrap_or(&b.t).to_string(),
                        open: Some(b.o),
                        high: Some(b.h),
                        low: Some(b.l),
                        close: Some(b.c),
                        volume: Some(b.v),
                    });
                }
            }

            match raw.next_page_token {
                Some(tok) if !tok.is_empty() => page_token = Some(tok),
                _ => break,
            }
        }
    }

    Ok(result)
}

/// Fetch today's 1-minute bars (from 09:30 ET to now) per symbol, as engine
/// `Bar`s (time-ascending). Used by the Perfect Pullback engine to recover today's
/// 09:30 open (and the morning's bars) when TagDash starts after the cash open, so
/// the gap/gate state is reconstructed instead of missing the opening bar. The
/// session start is resolved DST-aware via `crate::time`, correct in both EDT/EST.
pub async fn fetch_intraday_bars_today(
    key: &str,
    secret: &str,
    symbols: &[String],
) -> Result<HashMap<String, Vec<crate::market_state::aggregators::Bar>>, String> {
    // Session start = today's 09:30 ET regular-session open (DST-aware), so Alpaca
    // returns bars from the cash open regardless of EST/EDT. App clock: in replay
    // "today" is the simulated day (and the end is clamped to the sim instant).
    let start = crate::time::et_session_open_utc(crate::time::now())
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    fetch_minute_bars_since(key, secret, symbols, &start).await
}

/// Fetch 1-minute bars for each symbol from `start` (RFC3339, inclusive) to now,
/// as engine `Bar`s (time-ascending, with trade_count + vwap populated from
/// Alpaca's `n`/`vw`). The general primitive behind `fetch_intraday_bars_today`;
/// also used by the Micro Pullback engine to backfill a premarket dormancy
/// baseline at a late start (the live 10s ring is empty until the feed warms up,
/// but the last few minutes of 1-minute bars reconstruct the sleep baseline so the
/// engine can arm immediately instead of waiting ~5 minutes).
pub async fn fetch_minute_bars_since(
    key: &str,
    secret: &str,
    symbols: &[String],
    start: &str,
) -> Result<HashMap<String, Vec<crate::market_state::aggregators::Bar>>, String> {
    use chrono::{DateTime, Utc};
    use crate::market_state::aggregators::Bar;

    if symbols.is_empty() {
        return Ok(HashMap::new());
    }
    let client = reqwest::Client::new();
    let mut out: HashMap<String, Vec<Bar>> = HashMap::new();

    for chunk in symbols.chunks(200) {
        let sym_str = chunk.join(",");
        let mut page_token: Option<String> = None;
        loop {
            let mut url = format!(
                "https://data.alpaca.markets/v2/stocks/bars?symbols={sym_str}\
                 &timeframe=1Min&start={start}&limit=10000&adjustment=split"
            );
            push_end(&mut url, 60, None); // replay guard: nothing past the sim clock
            if let Some(tok) = &page_token {
                url.push_str(&format!("&page_token={tok}"));
            }
            let resp = client
                .get(&url)
                .header("APCA-API-KEY-ID", key)
                .header("APCA-API-SECRET-KEY", secret)
                .send()
                .await
                .map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                return Err(format!("Alpaca intraday bars HTTP {status}: {body}"));
            }
            let raw: BarsResponse = resp.json().await.map_err(|e| e.to_string())?;
            for (sym, bars) in raw.bars {
                let entry = out.entry(sym).or_default();
                for b in bars {
                    let Ok(time) = b.t.parse::<DateTime<Utc>>() else { continue };
                    entry.push(Bar {
                        time,
                        open:        b.o,
                        high:        b.h,
                        low:         b.l,
                        close:       b.c,
                        volume:      b.v.max(0) as u64,
                        vwap:        b.vw,
                        trade_count: b.n.map(|n| n.max(0) as u64),
                    });
                }
            }
            match raw.next_page_token {
                Some(tok) if !tok.is_empty() => page_token = Some(tok),
                _ => break,
            }
        }
    }

    for v in out.values_mut() {
        v.sort_by_key(|b| b.time);
    }
    Ok(out)
}

/// Map an internal Timeframe to the Alpaca bars `timeframe` parameter. Returns
/// None for sub-minute timeframes (5s/10s) which Alpaca's REST bars don't serve.
pub fn alpaca_timeframe(tf: crate::market_state::aggregators::Timeframe) -> Option<&'static str> {
    use crate::market_state::aggregators::Timeframe;
    match tf {
        Timeframe::S5 | Timeframe::S10 => None,
        Timeframe::M1    => Some("1Min"),
        Timeframe::M2    => Some("2Min"),
        Timeframe::M5    => Some("5Min"),
        Timeframe::M15   => Some("15Min"),
        Timeframe::Daily => Some("1Day"),
    }
}

/// Fetch the most recent `limit` bars of a given timeframe for ONE symbol,
/// returned oldest → newest as engine `Bar`s. Uses `sort=desc` + `limit` to get
/// the latest bars, then reverses. An explicit `start` window is required: with
/// no `start`, Alpaca's daily (`1Day`) endpoint defaults to today only — which is
/// why the daily chart used to show just the current bar. The window is sized
/// generously from the timeframe × `limit` (plus a weekend/overnight buffer); the
/// `limit` + `sort=desc` still cap the count to the most recent bars.
pub async fn fetch_recent_bars(
    key: &str,
    secret: &str,
    symbol: &str,
    tf: crate::market_state::aggregators::Timeframe,
    limit: u32,
) -> Result<Vec<crate::market_state::aggregators::Bar>, String> {
    use chrono::{DateTime, Duration, Utc};
    use crate::market_state::aggregators::Bar;

    let Some(timeframe) = alpaca_timeframe(tf) else {
        return Ok(vec![]); // sub-minute frames Alpaca's REST bars don't serve
    };

    // Cover `limit` bars of trading time, inflated ×3 for overnight/weekend gaps
    // and padded by 5 days so even a pre-open call still reaches prior sessions.
    // App clock: relative to the simulated day during a replay.
    let lookback_secs = tf.seconds() * (limit.max(1) as i64) * 3 + 5 * 86_400;
    let start = (crate::time::now() - Duration::seconds(lookback_secs))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();

    let client = reqwest::Client::new();
    let mut url = format!(
        "https://data.alpaca.markets/v2/stocks/bars?symbols={symbol}\
         &timeframe={timeframe}&start={start}&limit={limit}&sort=desc&adjustment=split"
    );
    push_end(&mut url, tf.seconds(), None); // replay guard: charts never see the future
    let resp = client
        .get(&url)
        .header("APCA-API-KEY-ID", key)
        .header("APCA-API-SECRET-KEY", secret)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("Alpaca recent bars HTTP {status}: {body}"));
    }

    let raw: BarsResponse = resp.json().await.map_err(|e| e.to_string())?;
    let mut out: Vec<Bar> = raw
        .bars
        .get(symbol)
        .map(|bars| {
            bars.iter()
                .filter_map(|b| {
                    let time = b.t.parse::<DateTime<Utc>>().ok()?;
                    Some(Bar {
                        time,
                        open:        b.o,
                        high:        b.h,
                        low:         b.l,
                        close:       b.c,
                        volume:      b.v.max(0) as u64,
                        vwap:        b.vw,
                        trade_count: b.n.map(|n| n.max(0) as u64),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    // `sort=desc` returns newest first — flip to oldest → newest for the chart.
    out.sort_by_key(|b| b.time);
    Ok(out)
}

/// Fetch up to `limit` bars of `tf` for ONE symbol ending at/before `end`
/// (RFC3339), returned oldest → newest. Uses `end` + `sort=desc` + `limit` so
/// Alpaca returns the most recent `limit` bars before the cutoff — the primitive
/// the chart uses to lazily back-fill older history as the user scrolls/zooms
/// into the past. Sub-minute frames Alpaca's REST bars don't serve → empty.
pub async fn fetch_bars_before(
    key: &str,
    secret: &str,
    symbol: &str,
    tf: crate::market_state::aggregators::Timeframe,
    end: &str,
    limit: u32,
) -> Result<Vec<crate::market_state::aggregators::Bar>, String> {
    use chrono::{DateTime, Utc};
    use crate::market_state::aggregators::Bar;

    let Some(timeframe) = alpaca_timeframe(tf) else {
        return Ok(vec![]);
    };

    let client = reqwest::Client::new();
    // Replay guard: keep the caller's `end` unless the simulated clock is earlier.
    let end = effective_end(tf.seconds(), Some(end)).unwrap_or_else(|| end.to_string());
    let url = format!(
        "https://data.alpaca.markets/v2/stocks/bars?symbols={symbol}\
         &timeframe={timeframe}&end={end}&limit={limit}&sort=desc&adjustment=split"
    );
    let resp = client
        .get(&url)
        .header("APCA-API-KEY-ID", key)
        .header("APCA-API-SECRET-KEY", secret)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("Alpaca older bars HTTP {status}: {body}"));
    }

    let raw: BarsResponse = resp.json().await.map_err(|e| e.to_string())?;
    let mut out: Vec<Bar> = raw
        .bars
        .get(symbol)
        .map(|bars| {
            bars.iter()
                .filter_map(|b| {
                    let time = b.t.parse::<DateTime<Utc>>().ok()?;
                    Some(Bar {
                        time,
                        open:        b.o,
                        high:        b.h,
                        low:         b.l,
                        close:       b.c,
                        volume:      b.v.max(0) as u64,
                        vwap:        b.vw,
                        trade_count: b.n.map(|n| n.max(0) as u64),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort_by_key(|b| b.time);
    Ok(out)
}

/// Mock daily bars: 50 days of synthetic OHLCV per symbol.
pub fn mock_daily_bars(symbols: &[String]) -> HashMap<String, Vec<BarData>> {
    const DAYS: u32 = 50;
    let mut map = HashMap::new();
    for (i, sym) in symbols.iter().enumerate() {
        let seed = (i % 50) as f64;
        let base_price = 1.5 + seed * 0.4;
        let base_vol: i64 = 200_000 + (i as i64 % 40) * 50_000;
        let bars: Vec<BarData> = (0..DAYS)
            .map(|d| {
                let day_offset = DAYS - d;
                // Simple synthetic OHLCV
                let jitter = (d as f64 * 0.03 + seed * 0.001).sin() * base_price * 0.04;
                let close = (base_price + jitter).max(0.5);
                let open = close * (1.0 - 0.01 * (d as f64 % 3.0));
                let high = close * 1.05;
                let low = close * 0.97;
                let vol = base_vol + (d as i64 % 5) * 20_000;
                BarData {
                    symbol: sym.clone(),
                    date: mock_date(day_offset),
                    open: Some(f2(open)),
                    high: Some(f2(high)),
                    low: Some(f2(low)),
                    close: Some(f2(close)),
                    volume: Some(vol),
                }
            })
            .collect();
        map.insert(sym.clone(), bars);
    }
    map
}

fn mock_date(days_ago: u32) -> String {
    use chrono::{Duration, Utc};
    let d = Utc::now().date_naive() - Duration::days(days_ago as i64);
    d.format("%Y-%m-%d").to_string()
}

fn f2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}
