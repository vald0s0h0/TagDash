// Market Replay — historical data loading for one trading day.
//
// Builds the full, time-sorted event list the replay engine then emits against
// the simulated clock.
//
// WHICH PROVIDER — the same priority the LIVE feed uses, so a replayed session is
// driven by the same tape the trader actually watches (see `Provider::resolve`,
// which mirrors `commands::spawn_live_feed` step for step):
//
//   1. Alpaca SIP    — the paid consolidated tape. Full-market minute bars, plus
//                      real trades/quotes wherever a local tape was recorded.
//   2. Massive Starter+ — per-second and per-minute aggregates over the whole US
//                      stock population. Delayed by 15 minutes LIVE, which is
//                      irrelevant here: a replayed day is history either way, so
//                      in replay Massive is a first-class source, not a fallback.
//   3. Flat files    — the local, offline copy (`crate::flat_files`).
//
// Alpaca on the free IEX plan keeps its historical place BEHIND the flat files:
// its REST bars only carry IEX's own prints (a few percent of the tape), so a
// downloaded day is strictly better than a live IEX refetch.
//
// Every step degrades gracefully into the next one (`attempt_order`), ending at
// the daily-bars-only session (`load_day_from_daily_cache`) rather than refusing
// to replay — so a missing key or a provider outage costs fidelity, never the
// session.
//
// WHAT EACH LOAD PRODUCES:
//   • previous closes + activity filter: daily bars around the day;
//   • premarket fine granularity: the local trade tape when one was recorded
//     that day (see `replay::tape`), else synthetic 10-second slices derived
//     from 1-minute bars (volume & trade count preserved per minute, so
//     the Micro Pullback ratios hold at the 60s tempo and degrade gracefully on
//     the shorter ones). On the Massive path a broad, permissive minute ignition
//     pass promotes likely wakes (plus chart focus) to genuine per-SECOND
//     aggregates, which is finer than any synthesis;
//   • regular session + after hours: synthetic slices from 1-minute bars (the
//     live feed only streams minute bars broadly there anyway);
//   • news: the day's headlines (Alpaca REST, else Massive reference news),
//     emitted at each headline's real `created_at` so there is no publication
//     leak — this is what lets Micro Pullback's A+ catalyst layer, and its LLM
//     calls, run in replay exactly as they do live.
//
// IMPORTANT — these fetchers are deliberately raw (no replay end-clamp): they
// load the WHOLE day up front, privately. Nothing reaches MarketState before the
// simulated clock passes the event's own timestamp. Every other REST call in the
// app goes through `alpaca::bars` / `alpaca::news` / `massive::news`, which ARE
// clamped to the simulated instant while replay is active.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, NaiveDate, Utc};
use serde::Deserialize;

use crate::types::NewsHeadline;

/// Cap on the number of symbols replayed (most active first).
pub(crate) const MAX_SYMBOLS: usize = 2_000;
/// 10-second synthetic slices per 1-minute bar.
const SLICES_PER_MIN: i64 = 6;

// ─── Event model ────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum Event {
    Trade {
        symbol: String,
        price: f64,
        size: u64,
        prints: u64,
    },
    Quote {
        symbol: String,
        bid: f64,
        ask: f64,
    },
    News(NewsHeadline),
}

#[derive(Debug)]
pub struct TimedEvent {
    pub ts_ms: i64,
    pub ev: Event,
}

pub struct DayData {
    /// Sorted by `ts_ms` ascending.
    pub events: Vec<TimedEvent>,
    /// Previous trading day's close per symbol (change% seed).
    pub prev_closes: HashMap<String, f64>,
    /// GRANULARITY of the price path: "tape" (real recorded trades) | "seconds"
    /// (real per-second aggregates) | "minutes" (synthetic from 1-min bars) |
    /// "daily" (synthetic from the day's own OHLCV bar only — no intraday flat
    /// file and no credentials, see `load_day_from_daily_cache`).
    pub source: &'static str,
    /// WHERE the data came from, for the toolbar badge and the logs — see
    /// `Provider::as_str`. Distinct from `source`: a "minutes" day can come from
    /// Alpaca, from Massive or from a flat file, and which one it was is exactly
    /// what the trader needs to know when a replayed session behaves oddly.
    pub provider: &'static str,
    pub symbols: usize,
}

// ─── Raw Alpaca REST (loader-private, unclamped) ───────────────────────────────

#[derive(Debug, Deserialize)]
struct BarsResponse {
    #[serde(default)]
    bars: HashMap<String, Vec<RawBar>>,
    #[serde(default)]
    next_page_token: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawBar {
    pub(crate) t: String,
    pub(crate) o: f64,
    pub(crate) h: f64,
    pub(crate) l: f64,
    pub(crate) c: f64,
    pub(crate) v: i64,
    #[serde(default)]
    pub(crate) n: Option<i64>,
    #[serde(default)]
    pub(crate) vw: Option<f64>,
}

pub(crate) struct MinBar {
    pub(crate) time: DateTime<Utc>,
    pub(crate) open: f64,
    pub(crate) high: f64,
    pub(crate) low: f64,
    pub(crate) close: f64,
    pub(crate) volume: u64,
    pub(crate) trades: u64,
    pub(crate) vwap: Option<f64>,
}

/// True when `s` looks like a symbol Alpaca's REST bars endpoint accepts.
/// Unlike the live WebSocket (which subscribes with the `*` wildcard and never
/// enumerates symbols), REST requests list every symbol — and a single exotic
/// entry in the universe table (digits, unit/when-issued suffixes…) gets the
/// whole request rejected with `400 invalid symbol`. Equity symbols are
/// upper-case letters with an optional `.`/`-` class separator; anything else
/// (e.g. "TEST2") is dropped before the request.
fn is_rest_symbol(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 10
        && s.chars().next().is_some_and(|c| c.is_ascii_uppercase())
        && s.chars()
            .all(|c| c.is_ascii_uppercase() || c == '.' || c == '-')
}

/// Extract the offending symbol from an Alpaca `400 invalid symbol: XXX` body.
fn invalid_symbol_of(body: &str) -> Option<String> {
    let idx = body.find("invalid symbol:")?;
    let rest = &body[idx + "invalid symbol:".len()..];
    let sym: String = rest
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '.' || *c == '-' || *c == '/')
        .collect();
    (!sym.is_empty()).then_some(sym)
}

pub(crate) async fn fetch_bars_window(
    key: &str,
    secret: &str,
    symbols: &[String],
    timeframe: &str,
    start: &str,
    end: &str,
    progress: &(dyn Fn(f32) + Sync),
) -> Result<HashMap<String, Vec<RawBar>>, String> {
    let client = crate::http::client();
    let mut out: HashMap<String, Vec<RawBar>> = HashMap::new();
    let chunks: Vec<Vec<String>> = symbols
        .iter()
        .filter(|s| is_rest_symbol(s))
        .cloned()
        .collect::<Vec<_>>()
        .chunks(200)
        .map(|c| c.to_vec())
        .collect();
    let total = chunks.len().max(1);
    for (i, mut chunk) in chunks.into_iter().enumerate() {
        let mut page_token: Option<String> = None;
        loop {
            if chunk.is_empty() {
                break;
            }
            let sym_str = chunk.join(",");
            let mut url = format!(
                "https://data.alpaca.markets/v2/stocks/bars?symbols={sym_str}\
                 &timeframe={timeframe}&start={start}&end={end}&limit=10000&adjustment=split"
            );
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
                // `400 invalid symbol: XXX` — the pre-filter missed one Alpaca
                // doesn't know. Drop the offender and retry the chunk from its
                // first page so one bad ticker can't sink the whole load.
                if status.as_u16() == 400 {
                    if let Some(bad) = invalid_symbol_of(&body) {
                        let before = chunk.len();
                        chunk.retain(|s| s != &bad);
                        if chunk.len() < before {
                            eprintln!("[tagdash] replay: symbole rejeté par Alpaca, ignoré: {bad}");
                            page_token = None;
                            continue;
                        }
                    }
                }
                return Err(format!("Alpaca replay bars HTTP {status}: {body}"));
            }
            let raw: BarsResponse = resp.json().await.map_err(|e| e.to_string())?;
            for (sym, bars) in raw.bars {
                out.entry(sym).or_default().extend(bars);
            }
            match raw.next_page_token {
                Some(tok) if !tok.is_empty() => page_token = Some(tok),
                _ => break,
            }
        }
        progress((i + 1) as f32 / total as f32);
    }
    for v in out.values_mut() {
        v.sort_by(|a, b| a.t.cmp(&b.t));
        // A chunk retried after a 400 restarts from its first page — dedup by
        // timestamp in case any page had already been collected.
        v.dedup_by(|a, b| a.t == b.t);
    }
    Ok(out)
}

// ─── Raw trades + quotes (loader-private, unclamped) ───────────────────────────
// Only used by the TRADE flat files: real tick data fetched on the small
// [alert−1min, alert+10min] windows the pre-scan flags, so Micro Pullback can replay
// on genuine prints instead of synthetic slices.

#[derive(Debug, Deserialize)]
struct TradesResponse {
    #[serde(default)]
    trades: HashMap<String, Vec<RawTrade>>,
    #[serde(default)]
    next_page_token: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawTrade {
    pub(crate) t: String,
    pub(crate) p: f64,
    #[serde(default)]
    pub(crate) s: i64,
}

#[derive(Debug, Deserialize)]
struct QuotesResponse {
    #[serde(default)]
    quotes: HashMap<String, Vec<RawQuote>>,
    #[serde(default)]
    next_page_token: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawQuote {
    pub(crate) t: String,
    #[serde(default)]
    pub(crate) bp: f64,
    #[serde(default)]
    pub(crate) ap: f64,
    #[serde(default)]
    pub(crate) bs: i64,
    #[serde(rename = "as", default)]
    pub(crate) as_size: i64,
}

/// Every trade for `symbols` in [start, end] (RFC3339). One paginated query; intended
/// for short windows so the result stays bounded.
pub(crate) async fn fetch_trades_window(
    key: &str,
    secret: &str,
    symbols: &[String],
    start: &str,
    end: &str,
) -> Result<HashMap<String, Vec<RawTrade>>, String> {
    let client = crate::http::client();
    let mut out: HashMap<String, Vec<RawTrade>> = HashMap::new();
    let mut syms: Vec<String> = symbols
        .iter()
        .filter(|s| is_rest_symbol(s))
        .cloned()
        .collect();
    let mut page_token: Option<String> = None;
    loop {
        if syms.is_empty() {
            break;
        }
        let sym_str = syms.join(",");
        let mut url = format!(
            "https://data.alpaca.markets/v2/stocks/trades?symbols={sym_str}\
             &start={start}&end={end}&limit=10000"
        );
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
            if status.as_u16() == 400 {
                if let Some(bad) = invalid_symbol_of(&body) {
                    let before = syms.len();
                    syms.retain(|s| s != &bad);
                    if syms.len() < before {
                        page_token = None;
                        continue;
                    }
                }
            }
            return Err(format!("Alpaca trades HTTP {status}: {body}"));
        }
        let raw: TradesResponse = resp.json().await.map_err(|e| e.to_string())?;
        for (sym, trades) in raw.trades {
            out.entry(sym).or_default().extend(trades);
        }
        match raw.next_page_token {
            Some(tok) if !tok.is_empty() => page_token = Some(tok),
            _ => break,
        }
    }
    for v in out.values_mut() {
        v.sort_by(|a, b| a.t.cmp(&b.t));
    }
    Ok(out)
}

/// Every quote (NBBO) for `symbols` in [start, end] (RFC3339). Same shape as
/// `fetch_trades_window`.
pub(crate) async fn fetch_quotes_window(
    key: &str,
    secret: &str,
    symbols: &[String],
    start: &str,
    end: &str,
) -> Result<HashMap<String, Vec<RawQuote>>, String> {
    let client = crate::http::client();
    let mut out: HashMap<String, Vec<RawQuote>> = HashMap::new();
    let mut syms: Vec<String> = symbols
        .iter()
        .filter(|s| is_rest_symbol(s))
        .cloned()
        .collect();
    let mut page_token: Option<String> = None;
    loop {
        if syms.is_empty() {
            break;
        }
        let sym_str = syms.join(",");
        let mut url = format!(
            "https://data.alpaca.markets/v2/stocks/quotes?symbols={sym_str}\
             &start={start}&end={end}&limit=10000"
        );
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
            if status.as_u16() == 400 {
                if let Some(bad) = invalid_symbol_of(&body) {
                    let before = syms.len();
                    syms.retain(|s| s != &bad);
                    if syms.len() < before {
                        page_token = None;
                        continue;
                    }
                }
            }
            return Err(format!("Alpaca quotes HTTP {status}: {body}"));
        }
        let raw: QuotesResponse = resp.json().await.map_err(|e| e.to_string())?;
        for (sym, quotes) in raw.quotes {
            out.entry(sym).or_default().extend(quotes);
        }
        match raw.next_page_token {
            Some(tok) if !tok.is_empty() => page_token = Some(tok),
            _ => break,
        }
    }
    for v in out.values_mut() {
        v.sort_by(|a, b| a.t.cmp(&b.t));
    }
    Ok(out)
}

#[derive(Debug, Deserialize)]
struct NewsResponse {
    #[serde(default)]
    news: Vec<RawNews>,
    #[serde(default)]
    next_page_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawNews {
    id: Option<i64>,
    headline: Option<String>,
    summary: Option<String>,
    url: Option<String>,
    source: Option<String>,
    #[serde(default)]
    symbols: Vec<String>,
    created_at: Option<String>,
}

/// Every Alpaca headline published in [start, end], ascending.
/// When `symbols` is non-empty, only headlines mentioning at least one of them.
pub(crate) async fn fetch_news_window(
    key: &str,
    secret: &str,
    start: &str,
    end: &str,
    symbols: &[String],
) -> Result<Vec<NewsHeadline>, String> {
    let client = crate::http::client();
    let mut out: Vec<NewsHeadline> = Vec::new();
    let mut page_token: Option<String> = None;
    loop {
        let mut url = format!(
            "https://data.alpaca.markets/v1beta1/news?start={start}&end={end}\
             &limit=50&sort=asc&include_content=false"
        );
        if !symbols.is_empty() {
            url.push_str(&format!("&symbols={}", symbols.join(",")));
        }
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
            return Err(format!("Alpaca replay news HTTP {status}: {body}"));
        }
        let raw: NewsResponse = resp.json().await.map_err(|e| e.to_string())?;
        for n in raw.news {
            let Some(created) = n
                .created_at
                .as_deref()
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|d| d.with_timezone(&Utc))
            else {
                continue;
            };
            let Some(headline) = n.headline.filter(|h| !h.trim().is_empty()) else {
                continue;
            };
            out.push(NewsHeadline {
                id: n.id.unwrap_or(0),
                headline,
                summary: n.summary,
                url: n.url,
                source: n.source,
                symbols: n.symbols.iter().map(|s| s.to_uppercase()).collect(),
                created_at: created,
                // At replay emission the headline "arrives" exactly when it was
                // published — `received_at` is what the correlation engine uses.
                received_at: created,
            });
        }
        match raw.next_page_token {
            Some(tok) if !tok.is_empty() => page_token = Some(tok),
            _ => break,
        }
    }
    Ok(out)
}

// ─── Synthetic 10-second slices from a 1-minute bar ────────────────────────────

/// Decompose one minute bar into SLICES_PER_MIN synthetic trades following a
/// plausible O→L→H→C (green) / O→H→L→C (red) path. Total volume and trade count
/// are preserved, so windowed ratios are exact at the 60-second horizon.
pub(crate) fn slices_of(bar: &MinBar) -> Vec<(i64, f64, u64, u64)> {
    let (o, h, l, c) = (bar.open, bar.high, bar.low, bar.close);
    let path: [f64; SLICES_PER_MIN as usize] = if c >= o {
        [o, (o + l) / 2.0, l, (l + h) / 2.0, h, c]
    } else {
        [o, (o + h) / 2.0, h, (h + l) / 2.0, l, c]
    };
    let n = SLICES_PER_MIN as u64;
    let base_vol = bar.volume / n;
    let rem_vol = bar.volume % n;
    let base_tr = bar.trades / n;
    let rem_tr = bar.trades % n;
    let t0 = bar.time.timestamp_millis();
    path.iter()
        .enumerate()
        .map(|(i, &price)| {
            let iu = i as u64;
            let vol = base_vol + u64::from(iu < rem_vol);
            let prints = base_tr + u64::from(iu < rem_tr);
            (t0 + (i as i64) * 10_000, price, vol, prints)
        })
        .filter(|(_, _, vol, prints)| *vol > 0 || *prints > 0)
        .collect()
}

// ─── Synthetic path from a single DAILY bar (no intraday data available) ───────

/// Decompose one full trading day's OHLCV into `SLICES_PER_MIN` synthetic
/// prints, spread evenly across the regular session (`session_start_ms` →
/// `session_end_ms`) rather than across one minute — the same plausible
/// O→L→H→C / O→H→L→C path as `slices_of`, but at daily-bar fidelity: nothing
/// finer than the day's own open/high/low/close is known. Used only by
/// `load_day_from_daily_cache`, when neither an intraday flat file nor Alpaca
/// credentials are available for the replayed day. Unlike `slices_of`, points
/// are never dropped for zero volume — the open/close anchors must survive
/// regardless of how the day's real volume happens to divide across them, since
/// this IS the whole price path for the day.
fn daily_path_slices(
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    volume: i64,
    session_start_ms: i64,
    session_end_ms: i64,
) -> Vec<(i64, f64, u64, u64)> {
    let path: [f64; SLICES_PER_MIN as usize] = if close >= open {
        [
            open,
            (open + low) / 2.0,
            low,
            (low + high) / 2.0,
            high,
            close,
        ]
    } else {
        [
            open,
            (open + high) / 2.0,
            high,
            (high + low) / 2.0,
            low,
            close,
        ]
    };
    let n = SLICES_PER_MIN as u64;
    let vol = volume.max(0) as u64;
    let base_vol = vol / n;
    let rem_vol = vol % n;
    let step_ms = (session_end_ms - session_start_ms) / (SLICES_PER_MIN - 1).max(1);
    path.iter()
        .enumerate()
        .map(|(i, &price)| {
            let iu = i as u64;
            let vol_i = base_vol + u64::from(iu < rem_vol);
            (session_start_ms + (i as i64) * step_ms, price, vol_i, 1u64)
        })
        .collect()
}

// ─── Raw Massive REST (loader-private, unclamped) ──────────────────────────────
// Same contract as the Alpaca fetchers above: the whole day is pulled up front and
// privately, and nothing reaches MarketState before the simulated clock passes the
// event's own timestamp. `massive::bars` deliberately keeps its "never reached
// during a replay" invariant — these are the replay loader's own calls.

/// Max symbols fetched from Massive at once. Starter has unlimited API calls but one
/// aggregate request per ticker, so the day load is bounded by concurrency, not by a
/// rate limit.
const MASSIVE_CONCURRENCY: usize = 16;
/// Coarse minute RVOL used to request second detail. It is intentionally lower than
/// Micro Pullback's final 5x threshold: a lossy one-minute observation must never
/// become a stricter veto than the second-resolution strategy it feeds.
const MASSIVE_SECOND_PREFETCH_RVOL: f64 = 2.0;
const MASSIVE_SECOND_PREFETCH_RANGE_PCT: f64 = 0.10;
const MASSIVE_SECOND_LOOKBACK_MINUTES: i64 = 20;
/// Second detail around a coarse ignition. The lead-in is longer than Micro's 15m
/// tape retention and the tail lets 10s→5m confirmations mature after the first
/// wake. Overlapping windows are merged per symbol.
const MASSIVE_SECOND_LEAD_MS: i64 = 16 * 60_000;
const MASSIVE_SECOND_TAIL_MS: i64 = 30 * 60_000;
/// Calendar days walked back looking for the previous SESSION's grouped daily bar
/// (covers a long weekend plus holidays).
const PREV_SESSION_LOOKBACK_DAYS: i64 = 7;
/// Window size for a per-SECOND aggregate fetch. Massive caps a single aggregates
/// response at 50 000 rows and, with `sort=asc`, an overflowing request silently
/// drops the END of the day — a replay that stops mid-afternoon with no error. Four
/// hours can hold at most 14 400 one-second bars, so the cap is unreachable by
/// construction rather than by hoping the symbol is quiet enough.
const SECOND_CHUNK_HOURS: i64 = 4;

#[derive(Debug, Deserialize)]
struct MassiveAggsResponse {
    #[serde(default)]
    results: Vec<MassiveAgg>,
}

#[derive(Debug, Deserialize)]
struct MassiveAgg {
    o: f64,
    h: f64,
    l: f64,
    c: f64,
    #[serde(default)]
    v: Option<f64>,
    #[serde(default)]
    vw: Option<f64>,
    #[serde(default)]
    n: Option<i64>,
    /// Bar start, Unix milliseconds.
    t: i64,
}

impl MassiveAgg {
    fn into_bar(self) -> Option<MinBar> {
        Some(MinBar {
            time: chrono::TimeZone::timestamp_millis_opt(&Utc, self.t).single()?,
            open: self.o,
            high: self.h,
            low: self.l,
            close: self.c,
            volume: self.v.unwrap_or(0.0).max(0.0) as u64,
            trades: self.n.unwrap_or(0).max(0) as u64,
            vwap: self.vw,
        })
    }
}

/// One symbol's aggregates over [from_ms, to_ms], ascending. `span` is Massive's
/// timespan unit ("second" | "minute"), `mult` its multiplier.
///
/// `from_ms` is passed as-is rather than floored onto a bucket grid the way
/// `massive::bars::aligned_from_ms` does: that alignment exists so repeated LIVE
/// polls key onto the same grid as the websocket aggregator. Here `from_ms` is the
/// day's fixed 04:00 ET anchor — it never moves between calls, and the bars are
/// decomposed into events rather than merged into the chart ring.
async fn fetch_massive_aggs(
    api_key: &str,
    symbol: &str,
    mult: u32,
    span: &str,
    from_ms: i64,
    to_ms: i64,
) -> Result<Vec<MinBar>, String> {
    let base = crate::massive::BASE_URL;
    let url = crate::massive::with_key(
        &format!(
            "{base}/v2/aggs/ticker/{symbol}/range/{mult}/{span}/{from_ms}/{to_ms}\
             ?adjusted=true&sort=asc&limit=50000"
        ),
        api_key,
    );
    let resp = crate::http::client()
        .get(&url)
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
    let parsed: MassiveAggsResponse = resp.json().await.map_err(|e| e.to_string())?;
    let mut bars: Vec<MinBar> = parsed
        .results
        .into_iter()
        .filter_map(MassiveAgg::into_bar)
        .collect();
    bars.sort_by_key(|b| b.time);
    Ok(bars)
}

/// One symbol's per-SECOND aggregates over [from_ms, to_ms], ascending, fetched in
/// `SECOND_CHUNK_HOURS` windows so no single response can hit Massive's row cap.
/// A window that fails is skipped rather than failing the symbol: losing an hour of
/// second-resolution detail is better than losing the symbol's whole day.
async fn fetch_massive_second_aggs(
    api_key: &str,
    symbol: &str,
    from_ms: i64,
    to_ms: i64,
) -> Result<Vec<MinBar>, String> {
    let chunk_ms = SECOND_CHUNK_HOURS * 3_600_000;
    let mut out: Vec<MinBar> = Vec::new();
    let mut first_error: Option<String> = None;
    let mut start = from_ms;
    while start < to_ms {
        let end = (start + chunk_ms).min(to_ms);
        match fetch_massive_aggs(api_key, symbol, 1, "second", start, end).await {
            Ok(bars) => out.extend(bars),
            Err(e) => {
                if first_error.is_none() {
                    first_error = Some(e);
                }
            }
        }
        start = end;
    }
    match first_error {
        Some(e) if out.is_empty() => Err(e),
        _ => Ok(out),
    }
}

/// Broad, direction-agnostic minute ignition used only to decide which symbols need
/// their second aggregates fetched. Missing minutes are real zero-activity minutes,
/// therefore they belong in the self-relative baseline; this is what makes a ticker
/// waking after a dormant premarket score strongly even when its first active bar is
/// around 04:00. No previous-day volume, VWAP or absolute-volume floor is used.
///
/// Looking at the complete private day here does not leak market information into a
/// replay: it only makes a higher-fidelity tape available. Events are still emitted
/// strictly at their original timestamp by the replay clock.
fn select_massive_second_windows(
    minutes: &HashMap<String, Vec<MinBar>>,
    prev_closes: &HashMap<String, f64>,
    focus: &[String],
) -> HashMap<String, Vec<(i64, i64)>> {
    let focus: HashSet<String> = focus.iter().map(|s| s.to_uppercase()).collect();
    let mut selected: HashMap<String, Vec<(i64, i64)>> = HashMap::new();

    for (symbol, bars) in minutes {
        if bars.is_empty() {
            continue;
        }
        if focus.contains(symbol) {
            let start = bars.first().unwrap().time.timestamp_millis();
            let end = bars.last().unwrap().time.timestamp_millis() + 60_000;
            selected.insert(symbol.clone(), vec![(start, end)]);
            continue;
        }
        let by_minute: HashMap<i64, &MinBar> = bars
            .iter()
            .map(|b| ((b.time.timestamp_millis() / 60_000) * 60_000, b))
            .collect();
        let mut windows = Vec::new();

        for bar in bars {
            let minute = (bar.time.timestamp_millis() / 60_000) * 60_000;
            let mut prior = (1..=MASSIVE_SECOND_LOOKBACK_MINUTES)
                .map(|i| {
                    by_minute
                        .get(&(minute - i * 60_000))
                        .map_or(0.0, |b| b.volume as f64)
                })
                .collect::<Vec<_>>();
            let baseline = median_f64(&mut prior).unwrap_or(0.0);
            let rvol = (bar.volume as f64 + 1.0) / (baseline + 1.0);
            let normalizer = prev_closes
                .get(symbol)
                .copied()
                .filter(|p| *p > 0.0)
                .unwrap_or(bar.open.max(0.01));
            let range_pct = (bar.high - bar.low).max(0.0) / normalizer * 100.0;

            if rvol >= MASSIVE_SECOND_PREFETCH_RVOL
                && range_pct >= MASSIVE_SECOND_PREFETCH_RANGE_PCT
            {
                windows.push((
                    minute - MASSIVE_SECOND_LEAD_MS,
                    minute + MASSIVE_SECOND_TAIL_MS,
                ));
            }
        }
        if !windows.is_empty() {
            windows.sort_by_key(|w| w.0);
            let mut merged: Vec<(i64, i64)> = Vec::new();
            for (start, end) in windows {
                match merged.last_mut() {
                    Some(last) if start <= last.1 => last.1 = last.1.max(end),
                    _ => merged.push((start, end)),
                }
            }
            selected.insert(symbol.clone(), merged);
        }
    }
    selected
}

fn median_f64(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() || values.iter().any(|v| !v.is_finite()) {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let n = values.len();
    Some(if n % 2 == 0 {
        (values[n / 2 - 1] + values[n / 2]) / 2.0
    } else {
        values[n / 2]
    })
}

#[derive(Debug, Deserialize)]
struct GroupedResponse {
    #[serde(default)]
    results: Vec<GroupedBar>,
}

#[derive(Debug, Deserialize)]
struct GroupedBar {
    #[serde(rename = "T")]
    ticker: String,
    #[serde(default)]
    o: f64,
    #[serde(default)]
    h: f64,
    #[serde(default)]
    l: f64,
    #[serde(default)]
    c: f64,
    #[serde(default)]
    v: f64,
}

/// Every US stock's daily OHLCV for one calendar day, in ONE request. The efficient
/// way to get both halves of what the loader needs from dailies (previous close and
/// the day-activity filter) without enumerating the universe symbol by symbol.
/// `Ok(empty)` on a non-session day (weekend/holiday) — not an error.
async fn fetch_massive_grouped_daily(
    api_key: &str,
    day: &str,
) -> Result<HashMap<String, (f64, f64, f64, f64, i64)>, String> {
    let base = crate::massive::BASE_URL;
    let url = crate::massive::with_key(
        &format!("{base}/v2/aggs/grouped/locale/us/market/stocks/{day}?adjusted=true"),
        api_key,
    );
    let resp = crate::http::client()
        .get(&url)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!(
            "Massive grouped daily HTTP {status}: {}",
            &body[..body.len().min(200)]
        ));
    }
    let parsed: GroupedResponse = resp.json().await.map_err(|e| e.to_string())?;
    Ok(parsed
        .results
        .into_iter()
        .map(|b| {
            (
                b.ticker.to_uppercase(),
                (b.o, b.h, b.l, b.c, b.v.max(0.0) as i64),
            )
        })
        .collect())
}

#[derive(Debug, Deserialize)]
struct MassiveNewsPage {
    #[serde(default)]
    results: Vec<MassiveNewsRow>,
    #[serde(default)]
    next_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MassiveNewsRow {
    #[serde(default, alias = "title", alias = "headline")]
    title: Option<String>,
    #[serde(default, alias = "article_url", alias = "url")]
    url: Option<String>,
    #[serde(
        default,
        alias = "published_utc",
        alias = "published_at",
        alias = "created_at"
    )]
    published_at: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    publisher: Option<MassiveNewsPublisher>,
    #[serde(default)]
    tickers: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct MassiveNewsPublisher {
    #[serde(default)]
    name: Option<String>,
}

/// Every Massive headline published in [start, end] (RFC3339), ascending. The
/// Massive-path equivalent of `fetch_news_window`, so an Alpaca-less setup still
/// replays with the catalysts that actually moved the day — without them Micro
/// Pullback's A+ layer has nothing to score and the LLM passes are pointless.
///
/// Best-effort by design: a `MAX_NEWS_PAGES` cap keeps a runaway cursor from turning
/// a day load into an unbounded crawl, and whatever landed before the cap is kept.
pub(crate) async fn fetch_massive_news_window(
    api_key: &str,
    start: &str,
    end: &str,
) -> Result<Vec<NewsHeadline>, String> {
    /// Pages of 1 000 headlines. A busy premarket runs to a few thousand market-wide.
    const MAX_NEWS_PAGES: usize = 10;
    let client = crate::http::client();
    let mut out: Vec<NewsHeadline> = Vec::new();
    let base = crate::massive::BASE_URL;
    let mut url = crate::massive::with_key(
        &format!(
            "{base}/v2/reference/news?published_utc.gte={start}\
             &published_utc.lte={end}&order=asc&sort=published_utc&limit=1000"
        ),
        api_key,
    );
    for page in 0..MAX_NEWS_PAGES {
        let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            let status = resp.status();
            // A partial crawl is worth keeping: the headlines already collected are
            // real and correctly dated.
            if page > 0 {
                break;
            }
            let body = resp.text().await.unwrap_or_default();
            return Err(format!(
                "Massive news HTTP {status}: {}",
                &body[..body.len().min(200)]
            ));
        }
        let parsed: MassiveNewsPage = resp.json().await.map_err(|e| e.to_string())?;
        for n in parsed.results {
            let Some(created) = n
                .published_at
                .as_deref()
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|d| d.with_timezone(&Utc))
            else {
                continue;
            };
            let Some(headline) = n.title.filter(|h| !h.trim().is_empty()) else {
                continue;
            };
            out.push(NewsHeadline {
                // Massive news carries no stable integer id; hash the URL (falling
                // back to the headline) so the same article keeps one identity
                // across a reload and the dedup downstream still works.
                id: stable_news_id(n.url.as_deref().unwrap_or(&headline)),
                headline,
                summary: n.description,
                url: n.url,
                source: n
                    .publisher
                    .and_then(|p| p.name)
                    .or_else(|| Some("Massive".into())),
                symbols: n.tickers.iter().map(|t| t.to_uppercase()).collect(),
                created_at: created,
                // At replay emission the headline "arrives" exactly when it was
                // published — `received_at` is what the correlation engine uses.
                received_at: created,
            });
        }
        match parsed.next_url {
            Some(next) if !next.is_empty() => url = crate::massive::with_key(&next, api_key),
            _ => break,
        }
    }
    out.sort_by_key(|h| h.created_at);
    Ok(out)
}

/// A stable, positive id for a provider that ships none. FNV-1a over the article's
/// URL: deterministic across runs, so replaying the same day twice does not produce
/// two "different" headlines for one article.
fn stable_news_id(key: &str) -> i64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in key.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    (hash >> 1) as i64
}

// ─── Provider selection ─────────────────────────────────────────────────────────

/// Which historical provider drives a replayed day, in the same priority order the
/// live feed resolves (`commands::spawn_live_feed`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    /// Alpaca on the paid SIP plan — the full consolidated tape.
    AlpacaSip,
    /// Massive Starter or above. Its 15-minute live delay does not exist in replay:
    /// a past session is history however it is fetched.
    Massive,
    /// Alpaca on the free IEX plan. Deliberately ranked BELOW the flat files: its
    /// bars carry only IEX's own prints.
    AlpacaIex,
    /// Local flat files only — `offline`, or nothing else configured.
    FlatFiles,
}

impl Provider {
    pub fn as_str(self) -> &'static str {
        match self {
            Provider::AlpacaSip => "alpaca_sip",
            Provider::Massive => "massive",
            Provider::AlpacaIex => "alpaca_iex",
            Provider::FlatFiles => "flat_files",
        }
    }

    /// Resolve the provider from what the account actually has. Pure so the priority
    /// itself is unit-testable without a network or an AppState.
    ///
    /// `alpaca_plan` is the last PROBED subscription (`alpaca::plan::PLAN_META_KEY`),
    /// not a guess: an undetermined plan must not be promoted to SIP, because doing
    /// so would spend a full-market minute-bar sweep on a key that answers a few
    /// percent of the tape.
    pub fn resolve(
        offline: bool,
        alpaca_ready: bool,
        alpaca_plan: Option<crate::alpaca::plan::Plan>,
        massive_ready: bool,
        massive_tier: crate::massive::plan::Tier,
    ) -> Provider {
        if offline {
            return Provider::FlatFiles;
        }
        if alpaca_ready && alpaca_plan == Some(crate::alpaca::plan::Plan::Sip) {
            return Provider::AlpacaSip;
        }
        if massive_ready && matches!(massive_tier, crate::massive::plan::Tier::Unlimited) {
            return Provider::Massive;
        }
        if alpaca_ready {
            return Provider::AlpacaIex;
        }
        Provider::FlatFiles
    }
}

/// One way of building a day. `attempt_order` turns a resolved `Provider` into the
/// ordered list of these to try.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Alpaca,
    Massive,
    FlatFiles,
    DailyCache,
}

/// The ordered fallbacks for a day load: the resolved provider first, then graceful
/// degradations, always ending on the daily-bars-only session so a replay is never
/// refused outright.
///
/// Two orderings, and the difference is the whole point of the ranking: on the paid
/// tiers the API is the better tape and a local flat file is the safety net, while on
/// free-tier Alpaca a downloaded day beats what IEX would answer live.
fn attempt_order(provider: Provider) -> &'static [Step] {
    match provider {
        Provider::AlpacaSip => &[
            Step::Alpaca,
            Step::Massive,
            Step::FlatFiles,
            Step::DailyCache,
        ],
        Provider::Massive => &[
            Step::Massive,
            Step::FlatFiles,
            Step::Alpaca,
            Step::DailyCache,
        ],
        Provider::AlpacaIex => &[Step::FlatFiles, Step::Alpaca, Step::DailyCache],
        Provider::FlatFiles => &[Step::FlatFiles, Step::DailyCache],
    }
}

/// Credentials + the resolved provider, assembled once by `replay::start` and handed
/// to every day load of the session (including « jour suivant »).
#[derive(Debug, Clone, Default)]
pub struct Sources {
    pub provider: Option<Provider>,
    /// `(key, secret)` — None when unset or `offline`.
    pub alpaca: Option<(String, String)>,
    /// None when unset or `offline`.
    pub massive_key: Option<String>,
}

impl Sources {
    fn provider(&self) -> Provider {
        self.provider.unwrap_or(Provider::FlatFiles)
    }
}

// ─── Day loading ────────────────────────────────────────────────────────────────

/// Load everything needed to replay `day` (YYYY-MM-DD, an ET trading date).
///
/// Walks `attempt_order` for the resolved provider, taking the first step that can
/// actually answer: the ranked source wins, and each failure (missing credential,
/// provider outage, day not downloaded) degrades into the next rather than ending
/// the replay. `progress` receives 0..1 across the whole load (status display).
///
/// The error returned when EVERY step declined lists what each one said — an opaque
/// "no data" here is what sends a user hunting through logs for a missing key.
pub async fn load_day(
    app_dir: &std::path::Path,
    db: &std::sync::Arc<std::sync::Mutex<rusqlite::Connection>>,
    sources: &Sources,
    day: &str,
    focus: &[String],
    progress: impl Fn(f32) + Sync,
) -> Result<DayData, String> {
    NaiveDate::parse_from_str(day, "%Y-%m-%d")
        .map_err(|_| format!("invalid replay date: {day}"))?;

    // Build the replay day's point-in-time RVOL profile before any events can run.
    // This reads only local minute files older than `day`; failure is non-fatal
    // because Micro Pullback retains its intraday rolling baseline fallback.
    if let Err(e) = crate::flat_files::minute::ensure_rvol_baseline(app_dir, db, day) {
        eprintln!("[tagdash] replay {day}: baseline Micro Pullback indisponible ({e})");
    }

    let provider = sources.provider();
    let mut refusals: Vec<String> = Vec::new();

    for step in attempt_order(provider) {
        progress(0.0);
        let attempt = match step {
            Step::Alpaca => match sources.alpaca.as_ref() {
                Some((key, secret)) => {
                    // Which Alpaca this is matters to whoever reads the badge: the
                    // same code path serves the full consolidated tape and the few
                    // percent of it the free plan carries.
                    let label = if provider == Provider::AlpacaSip {
                        Provider::AlpacaSip
                    } else {
                        Provider::AlpacaIex
                    };
                    load_day_from_alpaca(
                        app_dir, db, sources, key, secret, label, day, focus, &progress,
                    )
                    .await
                }
                None => Err("clés Alpaca non configurées".into()),
            },
            Step::Massive => match sources.massive_key.as_ref() {
                Some(k) => load_day_from_massive(db, k, sources, day, focus, &progress).await,
                None => Err("clé Massive non configurée".into()),
            },
            Step::FlatFiles => {
                if crate::flat_files::has_day(app_dir, day) {
                    progress(0.1);
                    crate::flat_files::read_day(app_dir, day)
                } else {
                    Err(format!("aucun flat file minute pour le {day}"))
                }
            }
            Step::DailyCache => load_day_from_daily_cache(db, day, focus),
        };
        match attempt {
            Ok(dd) => {
                // Each loader stamps its own `provider`, so a step reached as a
                // FALLBACK reports itself: the badge never claims SIP for a session
                // actually replayed off a flat file.
                if !refusals.is_empty() {
                    eprintln!(
                        "[tagdash] replay {day}: source {} retenue après {}",
                        dd.provider,
                        refusals.join(" ; "),
                    );
                }
                progress(1.0);
                return Ok(dd);
            }
            Err(e) => refusals.push(format!("{step:?}: {e}")),
        }
    }
    Err(format!(
        "aucune source de données pour le {day} — {}",
        refusals.join(" ; "),
    ))
}

/// The Alpaca path: daily bars for the previous close + activity filter, 1-minute
/// bars for the active set, the recorded tape for real premarket prints when the day
/// has one, and the day's news.
#[allow(clippy::too_many_arguments)]
async fn load_day_from_alpaca(
    app_dir: &std::path::Path,
    db: &std::sync::Arc<std::sync::Mutex<rusqlite::Connection>>,
    sources: &Sources,
    key: &str,
    secret: &str,
    label: Provider,
    day: &str,
    focus: &[String],
    progress: &(impl Fn(f32) + Sync),
) -> Result<DayData, String> {
    let nd = NaiveDate::parse_from_str(day, "%Y-%m-%d")
        .map_err(|_| format!("invalid replay date: {day}"))?;

    // ET wall-clock anchors of the day (DST-aware via crate::time).
    let noon = noon_utc(nd);
    let pm_start = crate::time::et_clock_utc(noon, 4, 0); // 04:00 ET
    let cash_open = crate::time::et_clock_utc(noon, 9, 30); // 09:30 ET
    let day_end = crate::time::et_clock_utc(noon, 20, 0); // 20:00 ET

    // 1. Universe (symbols known to the app).
    let universe = active_universe(db)?;
    progress(0.02);

    // 2. Daily window around the day: previous close + day activity filter.
    let daily_start = (nd - chrono::Duration::days(10))
        .format("%Y-%m-%dT00:00:00Z")
        .to_string();
    let daily_end = day_end.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let daily = fetch_bars_window(
        key,
        secret,
        &universe,
        "1Day",
        &daily_start,
        &daily_end,
        &|f| progress(0.02 + f * 0.18),
    )
    .await?;

    let (prev_closes, day_volume) = split_daily(&daily, day);

    // Active set: symbols that actually traded that day (bounded), plus the
    // currently displayed (focus) symbols so their charts always replay.
    let active_vec = rank_active(&day_volume, focus);
    if active_vec.is_empty() {
        return Err(format!(
            "aucune donnée de marché pour le {day} (jour non ouvré ?)"
        ));
    }
    progress(0.22);

    // 3. Minute bars of the day for the active set.
    let min_start = pm_start.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let min_end = day_end.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let minutes_raw = fetch_bars_window(
        key,
        secret,
        &active_vec,
        "1Min",
        &min_start,
        &min_end,
        &|f| progress(0.22 + f * 0.55),
    )
    .await?;
    let minutes: HashMap<String, Vec<MinBar>> = minutes_raw
        .into_iter()
        .map(|(sym, bars)| {
            let mb = bars
                .into_iter()
                .filter_map(|b| {
                    let time = b.t.parse::<DateTime<Utc>>().ok()?;
                    Some(MinBar {
                        time,
                        open: b.o,
                        high: b.h,
                        low: b.l,
                        close: b.c,
                        volume: b.v.max(0) as u64,
                        trades: b.n.unwrap_or(0).max(0) as u64,
                        vwap: b.vw,
                    })
                })
                .collect::<Vec<_>>();
            (sym, mb)
        })
        .collect();

    // 4. Tape (if recorded that day) → real premarket prints.
    let tape_available = super::tape::has_tape(app_dir, day);
    let cash_open_ms = cash_open.timestamp_millis();
    let mut events: Vec<TimedEvent> = Vec::new();

    if tape_available {
        for (ts_ms, symbol, price, size) in super::tape::read_trades(app_dir, day) {
            // Premarket only: during the regular session the live broad tier was
            // minute bars; the taped trades there (focus symbols) would double
            // count against the slices below.
            if ts_ms < cash_open_ms {
                events.push(TimedEvent {
                    ts_ms,
                    ev: Event::Trade {
                        symbol,
                        price,
                        size,
                        prints: 1,
                    },
                });
            }
        }
        for (ts_ms, symbol, bid, ask) in super::tape::read_quotes(app_dir, day) {
            events.push(TimedEvent {
                ts_ms,
                ev: Event::Quote { symbol, bid, ask },
            });
        }
    }
    progress(0.82);

    // 5. Synthetic slices from minute bars. With a tape, only from the cash open
    //    (premarket granularity comes from the tape); without, the whole day.
    for (sym, bars) in &minutes {
        for b in bars {
            if tape_available && b.time < cash_open {
                continue;
            }
            push_bar_events(&mut events, sym, b);
        }
    }
    progress(0.88);

    // 6. News of the day (published 00:00 ET → 20:00 ET), emitted at created_at.
    let news_start = crate::time::et_clock_utc(noon, 0, 0)
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    push_news_events(
        &mut events,
        load_day_news(sources, &news_start, &min_end).await,
    );
    progress(0.95);

    events.sort_by_key(|e| e.ts_ms);
    let symbols = active_vec.len();
    progress(1.0);

    Ok(DayData {
        events,
        prev_closes,
        source: if tape_available { "tape" } else { "minutes" },
        provider: label.as_str(),
        symbols,
    })
}

/// The Massive path: grouped daily bars for the previous close + activity filter,
/// then one aggregate request per active symbol.
///
/// Two granularities, on purpose. The broad set first gets 1-minute aggregates. A
/// permissive, directionless wake-up pass then promotes candidates (and explicit
/// chart focus) to genuine per-SECOND aggregates — Massive serves them at every tier
/// Starter and above, and they are the one thing no synthesis can reconstruct: a
/// real intra-minute path, which is precisely what the sub-minute tempos read. The
/// remaining symbols keep minute bars decomposed into the same 10-second slices as
/// the Alpaca path, preserving the complete 2 000-symbol coverage for Panic.
///
/// News is loaded by the shared `load_day_news` step, so an Alpaca-less account still
/// replays with the catalysts that moved the day and the A+ / LLM layer has something
/// to score.
async fn load_day_from_massive(
    db: &std::sync::Arc<std::sync::Mutex<rusqlite::Connection>>,
    api_key: &str,
    sources: &Sources,
    day: &str,
    focus: &[String],
    progress: &(impl Fn(f32) + Sync),
) -> Result<DayData, String> {
    use futures_util::StreamExt;

    let nd = NaiveDate::parse_from_str(day, "%Y-%m-%d")
        .map_err(|_| format!("invalid replay date: {day}"))?;
    let noon = noon_utc(nd);
    let pm_start = crate::time::et_clock_utc(noon, 4, 0);
    let day_end = crate::time::et_clock_utc(noon, 20, 0);
    let from_ms = pm_start.timestamp_millis();
    let to_ms = day_end.timestamp_millis();

    // The universe table bounds what we are willing to replay, exactly as on the
    // Alpaca path — grouped daily returns every listed security, including the
    // exotica the app deliberately never streams.
    let universe: HashSet<String> = active_universe(db)?.into_iter().collect();
    progress(0.02);

    // 1. The day itself + the previous session, one request each.
    let today = fetch_massive_grouped_daily(api_key, day).await?;
    if today.is_empty() {
        return Err(format!(
            "aucune donnée de marché pour le {day} (jour non ouvré ?)"
        ));
    }
    let mut prev_closes: HashMap<String, f64> = HashMap::new();
    let mut prev_day = nd;
    for _ in 0..PREV_SESSION_LOOKBACK_DAYS {
        prev_day -= chrono::Duration::days(1);
        let wd = chrono::Datelike::weekday(&prev_day);
        if wd == chrono::Weekday::Sat || wd == chrono::Weekday::Sun {
            continue;
        }
        let d = prev_day.format("%Y-%m-%d").to_string();
        match fetch_massive_grouped_daily(api_key, &d).await {
            Ok(rows) if !rows.is_empty() => {
                prev_closes = rows
                    .into_iter()
                    .filter(|(sym, _)| universe.contains(sym))
                    .map(|(sym, (_, _, _, c, _))| (sym, c))
                    .collect();
                break;
            }
            // A holiday answers 200 with an empty result set; keep walking back.
            Ok(_) => continue,
            Err(e) => {
                eprintln!("[tagdash] replay {day}: previous close via Massive: {e}");
                break;
            }
        }
    }
    progress(0.10);

    let day_volume: HashMap<String, i64> = today
        .iter()
        .filter(|(sym, _)| universe.contains(*sym))
        .map(|(sym, (_, _, _, _, v))| (sym.clone(), *v))
        .collect();
    let active_vec = rank_active(&day_volume, focus);
    if active_vec.is_empty() {
        return Err(format!(
            "aucune donnée de marché pour le {day} (jour non ouvré ?)"
        ));
    }

    // 2. Minute discovery over the complete, deliberately fixed top-2 000 set.
    //    This first pass is cheap enough to be broad and preserves Panic's coverage.
    //    It also prevents a circular dependency where a symbol would need to be
    //    opened/focused before Micro Pullback could see its real seconds.
    let total = active_vec.len().max(1);
    let done = std::sync::atomic::AtomicUsize::new(0);
    let mut minutes: HashMap<String, Vec<MinBar>> = HashMap::new();
    let mut fetched = 0usize;
    let mut errors = 0usize;

    let mut stream = futures_util::stream::iter(active_vec.iter().cloned())
        .map(|sym| {
            let done = &done;
            async move {
                let bars = fetch_massive_aggs(api_key, &sym, 1, "minute", from_ms, to_ms).await;
                let n = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                progress(0.10 + (n as f32 / total as f32) * 0.55);
                (sym, bars)
            }
        })
        .buffer_unordered(MASSIVE_CONCURRENCY);

    while let Some((sym, bars)) = stream.next().await {
        match bars {
            Ok(bars) if !bars.is_empty() => {
                fetched += 1;
                minutes.insert(sym, bars);
            }
            Ok(_) => {}
            Err(e) => {
                errors += 1;
                if errors <= 3 {
                    eprintln!("[tagdash] replay {day}: Massive {sym}: {e}");
                }
            }
        }
    }
    if fetched == 0 {
        return Err(format!(
            "Massive n'a renvoyé aucune barre intraday pour le {day} ({errors} erreurs)"
        ));
    }

    // 3. Promote every broad-minute ignition (plus explicit chart focus) to the
    //    provider's true one-second aggregates. The prefetch threshold is softer
    //    than the strategy's 5x RVOL and has no directional or absolute-volume
    //    gate. A failed second fetch falls back to that symbol's real minute bars.
    let second_windows = select_massive_second_windows(&minutes, &prev_closes, focus);
    let second_total = second_windows.len().max(1);
    let second_done = std::sync::atomic::AtomicUsize::new(0);
    let mut second_bars: HashMap<String, (Vec<MinBar>, Vec<(i64, i64)>)> = HashMap::new();
    let mut second_stream = futures_util::stream::iter(second_windows.into_iter())
        .map(|(sym, windows)| {
            let second_done = &second_done;
            async move {
                let mut bars = Vec::new();
                let mut covered = Vec::new();
                let mut first_error = None;
                for (start, end) in windows {
                    let start = start.max(from_ms);
                    let end = end.min(to_ms);
                    if start >= end {
                        continue;
                    }
                    match fetch_massive_second_aggs(api_key, &sym, start, end).await {
                        Ok(window_bars) if !window_bars.is_empty() => {
                            bars.extend(window_bars);
                            covered.push((start, end));
                        }
                        Ok(_) => {}
                        Err(e) if first_error.is_none() => first_error = Some(e),
                        Err(_) => {}
                    }
                }
                bars.sort_by_key(|b| b.time);
                bars.dedup_by_key(|b| b.time.timestamp_millis());
                let n = second_done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                progress(0.65 + (n as f32 / second_total as f32) * 0.20);
                (sym, bars, covered, first_error)
            }
        })
        .buffer_unordered(MASSIVE_CONCURRENCY);
    while let Some((sym, bars, covered, error)) = second_stream.next().await {
        if !bars.is_empty() {
            second_bars.insert(sym.clone(), (bars, covered));
        }
        if let Some(e) = error {
            eprintln!(
                "[tagdash] replay {day}: secondes Massive {sym} indisponibles, fallback minute ({e})"
            );
        }
    }

    let mut events: Vec<TimedEvent> = Vec::new();
    for (sym, bars) in &minutes {
        let coverage = second_bars.get(sym).map(|(_, windows)| windows.as_slice());
        for b in bars {
            let minute = b.time.timestamp_millis();
            let replaced = coverage.is_some_and(|windows| {
                windows
                    .iter()
                    .any(|(start, end)| minute >= *start && minute < *end)
            });
            if !replaced {
                push_bar_trade_events(&mut events, sym, b);
            }
        }
        if let Some((seconds, _)) = second_bars.get(sym) {
            for b in seconds {
                push_second_bar_events(&mut events, sym, b);
            }
        }
    }
    progress(0.86);

    // 4. News of the day, emitted at each headline's real publication instant.
    let news_start = crate::time::et_clock_utc(noon, 0, 0)
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    let news_end = day_end.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    push_news_events(
        &mut events,
        load_day_news(sources, &news_start, &news_end).await,
    );
    progress(0.95);

    events.sort_by_key(|e| e.ts_ms);
    let symbols = active_vec.len();
    progress(1.0);

    Ok(DayData {
        events,
        prev_closes,
        source: if second_bars.is_empty() {
            "minutes"
        } else {
            "seconds"
        },
        provider: Provider::Massive.as_str(),
        symbols,
    })
}

/// The day's headlines, from whichever news provider can answer — the SAME step on
/// every price path, because news provenance and price provenance are independent
/// questions (a Massive-driven day still reads Alpaca news when the keys are there).
///
/// Alpaca first: fuller bodies, stable ids, and it is the very feed the live session
/// listens to. Massive reference news second — it covers publishers Alpaca does not
/// carry, and on an Alpaca-less account it is the only source there is.
///
/// This is not decoration. Micro Pullback's A+ layer is a CATALYST decision and its
/// LLM passes are scored on the headlines: replaying without news scores every
/// candidate on an absence the live run would never have had. So a provider failure
/// is logged and the replay continues, but both providers get their turn first.
async fn load_day_news(sources: &Sources, start: &str, end: &str) -> Vec<NewsHeadline> {
    let mut last_error: Option<String> = None;
    if let Some((key, secret)) = sources.alpaca.as_ref() {
        match fetch_news_window(key, secret, start, end, &[]).await {
            Ok(list) if !list.is_empty() => return list,
            // Empty is not an error, but it IS worth a second opinion before
            // concluding the day carried no catalyst at all.
            Ok(_) => {}
            Err(e) => last_error = Some(format!("Alpaca: {e}")),
        }
    }
    if let Some(api_key) = sources.massive_key.as_ref() {
        match fetch_massive_news_window(api_key, start, end).await {
            Ok(list) if !list.is_empty() => return list,
            Ok(_) => {}
            Err(e) => last_error = Some(format!("Massive: {e}")),
        }
    }
    match last_error {
        Some(e) => eprintln!("[tagdash] replay: news load failed ({e}) — replay sans news"),
        None => eprintln!("[tagdash] replay: aucune news pour cette journée"),
    }
    Vec::new()
}

/// The app's streamable universe, or a clear error when the pipeline never ran.
fn active_universe(
    db: &std::sync::Arc<std::sync::Mutex<rusqlite::Connection>>,
) -> Result<Vec<String>, String> {
    let universe: Vec<String> = {
        let conn = db.lock().unwrap();
        crate::local_db::universe_repository::get_active_symbols(&conn)
            .map_err(|e| e.to_string())?
    };
    if universe.is_empty() {
        return Err("univers vide — lance d'abord le Startup Pipeline".into());
    }
    Ok(universe)
}

/// Decompose one MINUTE bar into 10-second slices, each carrying its own synthetic
/// NBBO quote.
fn push_bar_events(events: &mut Vec<TimedEvent>, symbol: &str, bar: &MinBar) {
    for (ts_ms, price, size, prints) in slices_of(bar) {
        push_trade_and_quote(events, symbol, ts_ms, price, size, prints);
    }
}

/// Massive Starter aggregates contain OHLCV, not historical NBBO quotes. Emit the
/// price path only: fabricating bid/ask would make the chart claim data the provider
/// never supplied.
fn push_bar_trade_events(events: &mut Vec<TimedEvent>, symbol: &str, bar: &MinBar) {
    for (ts_ms, price, size, prints) in slices_of(bar) {
        push_trade(events, symbol, ts_ms, price, size, prints);
    }
}

/// Emit one SECOND aggregate. Its `v` and `n` are real, so the per-second tape Micro
/// Pullback v2 reads carries genuine volume and trade counts rather than an assumed
/// one-print-per-bucket.
///
/// A flat second is one print. A second that MOVED is walked O→L→H→C (green) /
/// O→H→L→C (red) at 250 ms steps — all four still land in the same tape bucket, so
/// nothing is double counted, but `TradeCounter` sees the real high and low instead
/// of a bar whose range collapses to zero. That range is one of the ranked signals
/// (`RawObservation::range_signal`), and flattening it would quietly disable it on
/// exactly the seconds that matter. No quote is emitted: Massive Starter does not
/// provide historical NBBO data, so a made-up spread must not appear on the chart.
fn push_second_bar_events(events: &mut Vec<TimedEvent>, symbol: &str, bar: &MinBar) {
    let t0 = bar.time.timestamp_millis();
    let prints = bar.trades.max(1);
    if bar.high <= bar.low {
        push_trade(events, symbol, t0, bar.close, bar.volume, prints);
        return;
    }
    let path: [f64; 4] = if bar.close >= bar.open {
        [bar.open, bar.low, bar.high, bar.close]
    } else {
        [bar.open, bar.high, bar.low, bar.close]
    };
    let n = path.len() as u64;
    let (base_vol, rem_vol) = (bar.volume / n, bar.volume % n);
    let (base_pr, rem_pr) = (prints / n, prints % n);
    for (i, &price) in path.iter().enumerate() {
        let iu = i as u64;
        push_trade(
            events,
            symbol,
            t0 + (i as i64) * 250,
            price,
            base_vol + u64::from(iu < rem_vol),
            base_pr + u64::from(iu < rem_pr),
        );
    }
}

fn push_trade_and_quote(
    events: &mut Vec<TimedEvent>,
    symbol: &str,
    ts_ms: i64,
    price: f64,
    size: u64,
    prints: u64,
) {
    push_trade(events, symbol, ts_ms, price, size, prints);
    push_quote(events, symbol, ts_ms, price);
}

fn push_trade(
    events: &mut Vec<TimedEvent>,
    symbol: &str,
    ts_ms: i64,
    price: f64,
    size: u64,
    prints: u64,
) {
    events.push(TimedEvent {
        ts_ms,
        ev: Event::Trade {
            symbol: symbol.to_string(),
            price,
            size,
            prints,
        },
    });
}

/// The synthetic NBBO the trading engine needs to see a realistic spread instead of
/// the ±0.1% fallback in `market_prices()`.
fn push_quote(events: &mut Vec<TimedEvent>, symbol: &str, ts_ms: i64, price: f64) {
    let half_spread = (price * 0.0003).max(0.005);
    events.push(TimedEvent {
        ts_ms,
        ev: Event::Quote {
            symbol: symbol.to_string(),
            bid: price - half_spread,
            ask: price + half_spread,
        },
    });
}

fn push_news_events(events: &mut Vec<TimedEvent>, list: Vec<NewsHeadline>) {
    for h in list {
        events.push(TimedEvent {
            ts_ms: h.created_at.timestamp_millis(),
            ev: Event::News(h),
        });
    }
}

/// Build a degraded `DayData` for `day` from ONLY the local daily-bar cache — no
/// intraday tick/minute/tape data at all. This is `load_day`'s fallback when
/// flat-files mode has no per-day minute file downloaded for `day` and no Alpaca
/// credentials are configured to fetch one live: rather than refusing to replay,
/// it uses whatever `flat_files::daily` (a much lighter, whole-market cumulative
/// download) already covers. Each active symbol's day is reduced to its one real
/// OHLCV bar, walked as a synthetic path (`daily_path_slices`) across the regular
/// session, so charts populate and resting orders still have a coarse,
/// daily-bar-fidelity price path to fill against instead of none. No network.
pub fn load_day_from_daily_cache(
    db: &std::sync::Arc<std::sync::Mutex<rusqlite::Connection>>,
    day: &str,
    focus: &[String],
) -> Result<DayData, String> {
    let nd = NaiveDate::parse_from_str(day, "%Y-%m-%d")
        .map_err(|_| format!("invalid replay date: {day}"))?;
    let noon = noon_utc(nd);
    let session_start_ms = crate::time::et_clock_utc(noon, 9, 30).timestamp_millis();
    let session_end_ms = crate::time::et_clock_utc(noon, 16, 0).timestamp_millis();

    let rows = {
        let conn = db.lock().unwrap();
        crate::local_db::cache_repository::daily_bars_on(&conn, day).map_err(|e| e.to_string())?
    };
    if rows.is_empty() {
        return Err(format!("aucune barre daily locale pour le {day}"));
    }

    let mut prev_closes: HashMap<String, f64> = HashMap::new();
    let mut day_volume: HashMap<String, i64> = HashMap::new();
    let mut bars: HashMap<String, (f64, f64, f64, f64, i64)> = HashMap::new();
    for (sym, o, h, l, c, v, pc) in rows {
        if let Some(pc) = pc {
            prev_closes.insert(sym.clone(), pc);
        }
        day_volume.insert(sym.clone(), v);
        bars.insert(sym, (o, h, l, c, v));
    }

    let active_vec = rank_active(&day_volume, focus);
    if active_vec.is_empty() {
        return Err(format!(
            "aucune donnée de marché pour le {day} (jour non ouvré ?)"
        ));
    }

    let mut events: Vec<TimedEvent> = Vec::new();
    for sym in &active_vec {
        let Some(&(o, h, l, c, v)) = bars.get(sym) else {
            continue;
        };
        for (ts_ms, price, size, prints) in
            daily_path_slices(o, h, l, c, v, session_start_ms, session_end_ms)
        {
            push_trade_and_quote(&mut events, sym, ts_ms, price, size, prints);
        }
    }
    events.sort_by_key(|e| e.ts_ms);
    let symbols = active_vec.len();

    Ok(DayData {
        events,
        prev_closes,
        source: "daily",
        provider: "daily_cache",
        symbols,
    })
}

/// Split a daily-bars window into (previous close, this-day volume) per symbol.
/// Shared by `load_day` (Alpaca path) and the flat-file downloader so both derive
/// the change% seed and the activity filter identically.
pub(crate) fn split_daily(
    daily: &HashMap<String, Vec<RawBar>>,
    day: &str,
) -> (HashMap<String, f64>, HashMap<String, i64>) {
    let mut prev_closes: HashMap<String, f64> = HashMap::new();
    let mut day_volume: HashMap<String, i64> = HashMap::new();
    for (sym, bars) in daily {
        let mut prev: Option<f64> = None;
        for b in bars {
            let bdate = b.t.get(..10).unwrap_or("");
            if bdate < day {
                prev = Some(b.c);
            } else if bdate == day {
                day_volume.insert(sym.clone(), b.v);
            }
        }
        if let Some(pc) = prev {
            prev_closes.insert(sym.clone(), pc);
        }
    }
    (prev_closes, day_volume)
}

/// The bounded active set: the top `MAX_SYMBOLS` symbols that traded that day, plus
/// any focus symbol. There is deliberately NO absolute-volume threshold before the
/// top-2 000 boundary: such a threshold is most punitive at 04:00 and duplicates the
/// strategy's own ticker-relative RVOL work.
pub(crate) fn rank_active(day_volume: &HashMap<String, i64>, focus: &[String]) -> Vec<String> {
    let mut ranked: Vec<(String, i64)> = day_volume
        .iter()
        .filter(|(_, v)| **v > 0)
        .map(|(s, v)| (s.clone(), *v))
        .collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1));
    ranked.truncate(MAX_SYMBOLS);
    let mut active: HashSet<String> = ranked.into_iter().map(|(s, _)| s).collect();
    for f in focus {
        if day_volume.contains_key(f) {
            active.insert(f.clone());
        }
    }
    active.into_iter().collect()
}

/// Noon UTC of a calendar date — a safe instant whose ET day equals that date
/// (12:00Z = 07/08:00 ET), used to anchor `et_clock_utc` on the replay day.
pub fn noon_utc(d: NaiveDate) -> DateTime<Utc> {
    chrono::TimeZone::from_utc_datetime(&Utc, &d.and_hms_opt(12, 0, 0).expect("valid"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mb(volume: u64, trades: u64, o: f64, h: f64, l: f64, c: f64) -> MinBar {
        MinBar {
            time: "2026-06-10T08:00:00Z".parse().unwrap(),
            open: o,
            high: h,
            low: l,
            close: c,
            volume,
            trades,
            vwap: None,
        }
    }

    #[test]
    fn slices_preserve_volume_and_trades() {
        let bar = mb(10_007, 23, 2.0, 2.5, 1.9, 2.4);
        let slices = slices_of(&bar);
        let vol: u64 = slices.iter().map(|s| s.2).sum();
        let prints: u64 = slices.iter().map(|s| s.3).sum();
        assert_eq!(vol, 10_007);
        assert_eq!(prints, 23);
        // 10-second spacing inside the minute.
        assert!(slices.windows(2).all(|w| w[1].0 - w[0].0 == 10_000));
    }

    #[test]
    fn slices_walk_the_candle_range() {
        let bar = mb(600, 12, 2.0, 2.5, 1.9, 2.4);
        let slices = slices_of(&bar);
        let hi = slices.iter().map(|s| s.1).fold(f64::MIN, f64::max);
        let lo = slices.iter().map(|s| s.1).fold(f64::MAX, f64::min);
        assert_eq!(hi, 2.5);
        assert_eq!(lo, 1.9);
        assert_eq!(slices.first().unwrap().1, 2.0); // open first
        assert_eq!(slices.last().unwrap().1, 2.4); // close last
    }

    #[test]
    fn zero_volume_minute_yields_no_events() {
        let bar = mb(0, 0, 2.0, 2.0, 2.0, 2.0);
        assert!(slices_of(&bar).is_empty());
    }

    // `daily_path_slices` backs the daily-bars-only replay fallback: with no
    // intraday data at all, this handful of points IS the whole simulated day, so
    // (unlike `slices_of`) the open/close anchors must survive even on a
    // low/zero-volume bar instead of being filtered away.
    #[test]
    fn daily_path_slices_keeps_anchors_on_zero_volume() {
        let slices = daily_path_slices(2.0, 2.0, 2.0, 2.0, 0, 1_000, 2_000);
        assert_eq!(slices.len(), SLICES_PER_MIN as usize);
        assert_eq!(slices.first().unwrap().1, 2.0);
        assert_eq!(slices.last().unwrap().1, 2.0);
    }

    #[test]
    fn daily_path_slices_walks_the_day_range_and_preserves_volume() {
        let slices = daily_path_slices(2.0, 2.5, 1.9, 2.4, 12_000, 0, 300_000);
        let hi = slices.iter().map(|s| s.1).fold(f64::MIN, f64::max);
        let lo = slices.iter().map(|s| s.1).fold(f64::MAX, f64::min);
        assert_eq!(hi, 2.5);
        assert_eq!(lo, 1.9);
        assert_eq!(slices.first().unwrap().1, 2.0); // open first
        assert_eq!(slices.last().unwrap().1, 2.4); // close last
        let vol: u64 = slices.iter().map(|s| s.2).sum();
        assert_eq!(vol, 12_000);
        // Spread across the full [session_start, session_end] span, not bunched
        // into one minute like the intraday slices.
        assert_eq!(slices.first().unwrap().0, 0);
        assert_eq!(slices.last().unwrap().0, 300_000);
    }

    #[test]
    fn rest_symbol_filter() {
        assert!(is_rest_symbol("AAPL"));
        assert!(is_rest_symbol("BRK.A"));
        assert!(is_rest_symbol("ABR-PD"));
        assert!(!is_rest_symbol("TEST2")); // digit → Alpaca 400
        assert!(!is_rest_symbol("brk.a"));
        assert!(!is_rest_symbol(""));
        assert!(!is_rest_symbol(".AAPL"));
    }

    // ── Per-second aggregates (Massive path) ───────────────────────────────────

    fn second_bar(o: f64, h: f64, l: f64, c: f64, volume: u64, trades: u64) -> MinBar {
        MinBar {
            time: "2026-06-10T13:30:00Z".parse().unwrap(),
            open: o,
            high: h,
            low: l,
            close: c,
            volume,
            trades,
            vwap: None,
        }
    }

    fn minute_at(minute: i64, o: f64, h: f64, l: f64, c: f64, volume: u64) -> MinBar {
        MinBar {
            time: chrono::TimeZone::timestamp_millis_opt(&Utc, minute * 60_000)
                .single()
                .unwrap(),
            open: o,
            high: h,
            low: l,
            close: c,
            volume,
            trades: volume.min(100),
            vwap: None,
        }
    }

    #[test]
    fn dormant_ticker_is_promoted_to_seconds_without_absolute_volume_floor() {
        let bars = vec![
            minute_at(100, 5.00, 5.01, 4.99, 5.00, 8),
            minute_at(101, 5.00, 5.01, 5.00, 5.00, 5),
            // No bars for several minutes: those gaps are zero attention, not
            // missing evidence. A modest 900-share wake-up with a real price range
            // must therefore receive second detail.
            minute_at(110, 5.00, 5.08, 4.99, 5.06, 900),
        ];
        let minutes = HashMap::from([("JUNS".to_string(), bars)]);
        let prev = HashMap::from([("JUNS".to_string(), 5.0)]);
        let selected = select_massive_second_windows(&minutes, &prev, &[]);
        assert!(selected.contains_key("JUNS"));
        assert_eq!(selected["JUNS"].len(), 1);
    }

    #[test]
    fn volume_without_price_evidence_does_not_consume_second_budget() {
        let bars = vec![
            minute_at(100, 5.0, 5.0, 5.0, 5.0, 10),
            minute_at(101, 5.0, 5.0, 5.0, 5.0, 100_000),
        ];
        let minutes = HashMap::from([("NOISE".to_string(), bars)]);
        let selected = select_massive_second_windows(&minutes, &HashMap::new(), &[]);
        assert!(!selected.contains_key("NOISE"));
    }

    #[test]
    fn chart_focus_is_promoted_even_without_ignition() {
        let minutes = HashMap::from([(
            "FOCUS".to_string(),
            vec![minute_at(100, 5.0, 5.0, 5.0, 5.0, 1)],
        )]);
        let selected = select_massive_second_windows(&minutes, &HashMap::new(), &["focus".into()]);
        assert!(selected.contains_key("FOCUS"));
    }

    #[test]
    fn top_2000_loader_has_no_hidden_absolute_volume_floor() {
        let day_volume = HashMap::from([("TINY".to_string(), 1), ("DORMANT".to_string(), 0)]);
        let selected = rank_active(&day_volume, &[]);
        assert_eq!(selected, vec!["TINY".to_string()]);
    }

    fn trades_of(events: &[TimedEvent]) -> Vec<(i64, f64, u64, u64)> {
        events
            .iter()
            .filter_map(|e| match &e.ev {
                Event::Trade {
                    price,
                    size,
                    prints,
                    ..
                } => Some((e.ts_ms, *price, *size, *prints)),
                _ => None,
            })
            .collect()
    }

    /// The whole reason a moving second is walked rather than reduced to its close:
    /// `TradeCounter` derives the tape bar's high/low from the prints it sees, and
    /// `RawObservation::range_signal` reads that range.
    #[test]
    fn a_moving_second_keeps_its_range_and_its_totals() {
        let bar = second_bar(10.0, 10.4, 9.8, 10.3, 1_001, 7);
        let mut events = Vec::new();
        push_second_bar_events(&mut events, "AAA", &bar);
        let trades = trades_of(&events);

        let hi = trades.iter().map(|t| t.1).fold(f64::MIN, f64::max);
        let lo = trades.iter().map(|t| t.1).fold(f64::MAX, f64::min);
        assert_eq!((hi, lo), (10.4, 9.8));
        assert_eq!(trades.first().unwrap().1, 10.0, "open first");
        assert_eq!(trades.last().unwrap().1, 10.3, "close last");

        // Nothing invented and nothing lost: the aggregate's own v/n is redistributed.
        assert_eq!(trades.iter().map(|t| t.2).sum::<u64>(), 1_001);
        assert_eq!(trades.iter().map(|t| t.3).sum::<u64>(), 7);

        // Every sub-print stays inside the SAME tape second, so the redistribution
        // cannot leak volume into the next bucket.
        let t0 = bar.time.timestamp_millis();
        assert!(trades.iter().all(|t| t.0 >= t0 && t.0 < t0 + 1_000));

        // Massive Starter did not provide NBBO here: never invent one for display.
        let quotes = events
            .iter()
            .filter(|e| matches!(e.ev, Event::Quote { .. }))
            .count();
        assert_eq!(quotes, 0);
    }

    #[test]
    fn a_flat_second_is_a_single_print() {
        let mut events = Vec::new();
        push_second_bar_events(
            &mut events,
            "AAA",
            &second_bar(10.0, 10.0, 10.0, 10.0, 300, 2),
        );
        assert_eq!(trades_of(&events), vec![(1_781_098_200_000, 10.0, 300, 2)]);
    }

    /// Massive aggregates carry no `n` on quiet buckets. One print is an honest
    /// floor — zero would make the second invisible to the tape.
    #[test]
    fn a_second_without_a_trade_count_still_prints_once() {
        let mut events = Vec::new();
        push_second_bar_events(
            &mut events,
            "AAA",
            &second_bar(10.0, 10.0, 10.0, 10.0, 100, 0),
        );
        assert_eq!(trades_of(&events).iter().map(|t| t.3).sum::<u64>(), 1);
    }

    #[test]
    fn massive_minute_path_has_prices_but_no_fabricated_quotes() {
        let mut events = Vec::new();
        push_bar_trade_events(
            &mut events,
            "AAA",
            &second_bar(10.0, 10.4, 9.8, 10.3, 1_001, 7),
        );
        assert!(!trades_of(&events).is_empty());
        assert!(events.iter().all(|e| !matches!(e.ev, Event::Quote { .. })));
    }

    // ── Source priority ────────────────────────────────────────────────────────
    // The ordering IS the feature: a replayed session must be driven by the same
    // tape the trader watches live, not by whichever source happens to answer first.

    use crate::alpaca::plan::Plan;
    use crate::massive::plan::Tier;

    #[test]
    fn sip_outranks_everything_else() {
        assert_eq!(
            Provider::resolve(false, true, Some(Plan::Sip), true, Tier::Unlimited),
            Provider::AlpacaSip,
            "a paid consolidated tape is never given up for a delayed aggregate feed",
        );
    }

    #[test]
    fn massive_starter_outranks_free_alpaca() {
        // The free IEX plan carries a few percent of the tape; Massive Starter+ has
        // the whole population. Its 15-minute live delay is irrelevant on history.
        assert_eq!(
            Provider::resolve(false, true, Some(Plan::Iex), true, Tier::Unlimited),
            Provider::Massive,
        );
        // Massive on the free tier is NOT a market-data source — back to Alpaca.
        assert_eq!(
            Provider::resolve(false, true, Some(Plan::Iex), true, Tier::Basic),
            Provider::AlpacaIex,
        );
    }

    #[test]
    fn massive_alone_still_beats_nothing() {
        assert_eq!(
            Provider::resolve(false, false, None, true, Tier::Unlimited),
            Provider::Massive,
            "an Alpaca-less account replays on Massive, not on flat files only",
        );
    }

    #[test]
    fn an_unprobed_alpaca_plan_is_not_promoted_to_sip() {
        // `None` means the probe never concluded. Treating it as SIP would spend a
        // full-market minute sweep on a key that answers a fraction of the tape.
        assert_eq!(
            Provider::resolve(false, true, None, false, Tier::Basic),
            Provider::AlpacaIex,
        );
    }

    #[test]
    fn offline_forces_the_local_files_whatever_the_keys_say() {
        assert_eq!(
            Provider::resolve(true, true, Some(Plan::Sip), true, Tier::Unlimited),
            Provider::FlatFiles,
            "`offline` means do not reach for the network — including for a replay",
        );
    }

    #[test]
    fn nothing_configured_lands_on_the_flat_files() {
        assert_eq!(
            Provider::resolve(false, false, None, false, Tier::Basic),
            Provider::FlatFiles,
        );
    }

    /// Every resolution must end somewhere: a replay is never refused for want of a
    /// source, it degrades to the daily-bars-only session.
    #[test]
    fn every_order_ends_on_the_daily_cache() {
        for p in [
            Provider::AlpacaSip,
            Provider::Massive,
            Provider::AlpacaIex,
            Provider::FlatFiles,
        ] {
            let order = attempt_order(p);
            assert_eq!(order.last(), Some(&Step::DailyCache), "{p:?}");
            assert!(!order.is_empty());
        }
    }

    /// The paid tiers lead with the API (best tape, flat file as the safety net);
    /// free-tier Alpaca leads with the flat file, because a downloaded day beats
    /// what IEX would answer live.
    #[test]
    fn the_ranked_source_is_tried_first_except_on_free_alpaca() {
        assert_eq!(
            attempt_order(Provider::AlpacaSip),
            &[
                Step::Alpaca,
                Step::Massive,
                Step::FlatFiles,
                Step::DailyCache
            ],
        );
        assert_eq!(attempt_order(Provider::Massive)[0], Step::Massive);
        assert_eq!(attempt_order(Provider::AlpacaIex)[0], Step::FlatFiles);
        assert_eq!(attempt_order(Provider::FlatFiles)[0], Step::FlatFiles);
        // A flat-files session must not silently reach the network as a fallback.
        assert!(!attempt_order(Provider::FlatFiles).contains(&Step::Alpaca));
        assert!(!attempt_order(Provider::FlatFiles).contains(&Step::Massive));
    }

    /// Massive news ships no id. Replaying the same day twice must not produce two
    /// "different" headlines for one article, or every dedup downstream breaks.
    #[test]
    fn massive_news_ids_are_stable_and_positive() {
        let a = stable_news_id("https://example.test/article-1");
        assert_eq!(a, stable_news_id("https://example.test/article-1"));
        assert_ne!(a, stable_news_id("https://example.test/article-2"));
        for key in ["", "x", "https://example.test/article-1", "FDA approval"] {
            assert!(stable_news_id(key) >= 0, "{key:?} must not hash negative");
        }
    }

    #[test]
    fn invalid_symbol_extraction() {
        let body = r#"{"message":"invalid symbol: TEST2"}"#;
        assert_eq!(invalid_symbol_of(body).as_deref(), Some("TEST2"));
        assert_eq!(invalid_symbol_of("other error"), None);
    }
}
