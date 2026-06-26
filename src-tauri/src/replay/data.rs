// Market Replay — historical data loading for one trading day.
//
// Builds the full, time-sorted event list the replay engine then emits against
// the simulated clock. Sources:
//   • previous closes + activity filter: Alpaca daily bars around the day;
//   • premarket fine granularity: the local trade tape when one was recorded
//     that day (see `replay::tape`), else synthetic 10-second slices derived
//     from Alpaca 1-minute bars (volume & trade count preserved per minute, so
//     the Micro Pullback ratios hold at the 60s tempo and degrade gracefully on
//     the shorter ones);
//   • regular session + after hours: synthetic slices from 1-minute bars (the
//     live feed only streams minute bars broadly there anyway);
//   • news: Alpaca news REST for the day, emitted at each headline's real
//     `created_at` so there is no publication leak.
//
// IMPORTANT — these fetchers are deliberately raw (no replay end-clamp): they
// load the WHOLE day up front, privately. Nothing reaches MarketState before the
// simulated clock passes the event's own timestamp. Every other REST call in the
// app goes through `alpaca::bars` / `alpaca::news`, which ARE clamped to the
// simulated instant while replay is active.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, NaiveDate, Utc};
use serde::Deserialize;

use crate::types::NewsHeadline;

/// Minimum day volume (shares) for a symbol to be loaded in minute-bar mode.
/// Anything quieter cannot trip a strategy gate; this bounds the REST volume.
pub(crate) const MIN_DAY_VOLUME: i64 = 50_000;
/// Cap on the number of symbols replayed (most active first).
pub(crate) const MAX_SYMBOLS: usize = 2_000;
/// 10-second synthetic slices per 1-minute bar.
const SLICES_PER_MIN: i64 = 6;

// ─── Event model ────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum Event {
    Trade { symbol: String, price: f64, size: u64, prints: u64 },
    Quote { symbol: String, bid: f64, ask: f64 },
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
    /// "tape" when the premarket came from the local trade tape, else "minutes".
    pub source: &'static str,
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
/// (e.g. "YZCDE2") is dropped before the request.
fn is_rest_symbol(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 10
        && s.chars().next().is_some_and(|c| c.is_ascii_uppercase())
        && s.chars().all(|c| c.is_ascii_uppercase() || c == '.' || c == '-')
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
    let client = reqwest::Client::new();
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
    let client = reqwest::Client::new();
    let mut out: HashMap<String, Vec<RawTrade>> = HashMap::new();
    let mut syms: Vec<String> = symbols.iter().filter(|s| is_rest_symbol(s)).cloned().collect();
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
    let client = reqwest::Client::new();
    let mut out: HashMap<String, Vec<RawQuote>> = HashMap::new();
    let mut syms: Vec<String> = symbols.iter().filter(|s| is_rest_symbol(s)).cloned().collect();
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
    let client = reqwest::Client::new();
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
            let Some(headline) = n.headline.filter(|h| !h.trim().is_empty()) else { continue };
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

// ─── Day loading ────────────────────────────────────────────────────────────────

/// Load everything needed to replay `day` (YYYY-MM-DD, an ET trading date).
/// `progress` receives 0..1 across the whole load (status display).
pub async fn load_day(
    app_dir: &std::path::Path,
    db: &std::sync::Arc<std::sync::Mutex<rusqlite::Connection>>,
    key: &str,
    secret: &str,
    day: &str,
    focus: &[String],
    progress: impl Fn(f32) + Sync,
) -> Result<DayData, String> {
    let nd = NaiveDate::parse_from_str(day, "%Y-%m-%d")
        .map_err(|_| format!("invalid replay date: {day}"))?;

    // Offline reuse: when a complete flat file exists for this day, replay reads it
    // straight from disk — zero network — in BOTH data-source modes. In flat-files
    // mode this is the only path (the live API may be gone).
    if crate::flat_files::has_day(app_dir, day) {
        progress(0.1);
        let dd = crate::flat_files::read_day(app_dir, day)?;
        progress(1.0);
        return Ok(dd);
    }
    // No flat file and no API credentials ⇒ flat-files mode without this day
    // downloaded. Give a clear, actionable error instead of an opaque auth failure.
    if key.is_empty() || secret.is_empty() {
        return Err(format!(
            "jour {day} non téléchargé — téléchargez-le via « Gestion Flat Files » \
             (mode flat files actif)"
        ));
    }

    // ET wall-clock anchors of the day (DST-aware via crate::time).
    let noon = noon_utc(nd);
    let pm_start = crate::time::et_clock_utc(noon, 4, 0); // 04:00 ET
    let cash_open = crate::time::et_clock_utc(noon, 9, 30); // 09:30 ET
    let day_end = crate::time::et_clock_utc(noon, 20, 0); // 20:00 ET

    // 1. Universe (symbols known to the app).
    let universe: Vec<String> = {
        let conn = db.lock().unwrap();
        crate::local_db::universe_repository::get_active_symbols(&conn)
            .map_err(|e| e.to_string())?
    };
    if universe.is_empty() {
        return Err("univers vide — lance d'abord le Startup Pipeline".into());
    }
    progress(0.02);

    // 2. Daily window around the day: previous close + day activity filter.
    let daily_start = (nd - chrono::Duration::days(10)).format("%Y-%m-%dT00:00:00Z").to_string();
    let daily_end = day_end.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let daily = fetch_bars_window(
        key, secret, &universe, "1Day", &daily_start, &daily_end,
        &|f| progress(0.02 + f * 0.18),
    )
    .await?;

    let (prev_closes, day_volume) = split_daily(&daily, day);

    // Active set: symbols that actually traded that day (bounded), plus the
    // currently displayed (focus) symbols so their charts always replay.
    let active_vec = rank_active(&day_volume, focus);
    if active_vec.is_empty() {
        return Err(format!("aucune donnée de marché pour le {day} (jour non ouvré ?)"));
    }
    progress(0.22);

    // 3. Minute bars of the day for the active set.
    let min_start = pm_start.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let min_end = day_end.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let minutes_raw = fetch_bars_window(
        key, secret, &active_vec, "1Min", &min_start, &min_end,
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
                    ev: Event::Trade { symbol, price, size, prints: 1 },
                });
            }
        }
        for (ts_ms, symbol, bid, ask) in super::tape::read_quotes(app_dir, day) {
            events.push(TimedEvent { ts_ms, ev: Event::Quote { symbol, bid, ask } });
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
            let _ = b.vwap; // session VWAP is rebuilt from the slices themselves
            for (ts_ms, price, size, prints) in slices_of(b) {
                events.push(TimedEvent {
                    ts_ms,
                    ev: Event::Trade { symbol: sym.clone(), price, size, prints },
                });
            }
        }
    }
    progress(0.88);

    // 6. News of the day (published 00:00 ET → 20:00 ET), emitted at created_at.
    let news_start = crate::time::et_clock_utc(noon, 0, 0).format("%Y-%m-%dT%H:%M:%SZ").to_string();
    match fetch_news_window(key, secret, &news_start, &min_end, &[]).await {
        Ok(list) => {
            for h in list {
                events.push(TimedEvent { ts_ms: h.created_at.timestamp_millis(), ev: Event::News(h) });
            }
        }
        Err(e) => eprintln!("[tagdash] replay: news load failed ({e}) — replay sans news"),
    }
    progress(0.95);

    events.sort_by_key(|e| e.ts_ms);
    let symbols = active_vec.len();
    progress(1.0);

    Ok(DayData {
        events,
        prev_closes,
        source: if tape_available { "tape" } else { "minutes" },
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

/// The bounded active set: symbols with day volume ≥ MIN_DAY_VOLUME (top
/// MAX_SYMBOLS by volume), plus any focus symbol that traded that day. Shared by
/// `load_day` and the flat-file downloader so a downloaded day holds exactly the
/// same symbols replay would otherwise fetch live.
pub(crate) fn rank_active(day_volume: &HashMap<String, i64>, focus: &[String]) -> Vec<String> {
    let mut ranked: Vec<(String, i64)> = day_volume
        .iter()
        .filter(|(_, v)| **v >= MIN_DAY_VOLUME)
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

    #[test]
    fn rest_symbol_filter() {
        assert!(is_rest_symbol("AAPL"));
        assert!(is_rest_symbol("BRK.A"));
        assert!(is_rest_symbol("ABR-PD"));
        assert!(!is_rest_symbol("YZCDE2")); // digit → Alpaca 400
        assert!(!is_rest_symbol("brk.a"));
        assert!(!is_rest_symbol(""));
        assert!(!is_rest_symbol(".AAPL"));
    }

    #[test]
    fn invalid_symbol_extraction() {
        let body = r#"{"message":"invalid symbol: YZCDE2"}"#;
        assert_eq!(invalid_symbol_of(body).as_deref(), Some("YZCDE2"));
        assert_eq!(invalid_symbol_of("other error"), None);
    }
}
