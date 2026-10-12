// Market Replay — the event model and the day loader.
//
// A replay reads ONE place: the day prepared on disk by `crate::replay_data`, from
// what the strategies declared they need — real SIP trades and quotes around their
// opportunities, synthetic 10-second slices from the 1-minute bars everywhere else,
// and the headlines emitted at their real `created_at` (no publication leak, which
// is what lets the news-driven layers run in replay exactly as they do live).
//
// No network here and no fallback chain: a day that is not prepared is an error the
// day picker resolves by preparing it. The shared Alpaca fetchers kept below serve
// the flat-file and chart caches; they are deliberately raw (no replay end-clamp) —
// every other REST call in the app goes through `alpaca::bars` / `alpaca::news` /
// `massive::news`, which ARE clamped to the simulated instant during a replay.

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
        /// A slice synthesized from a 1-minute bar, not a real print.
        synthetic: bool,
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
    /// The symbols the day carries, and the ids of its headlines: what a later
    /// re-read of a day still being prepared is compared against, to feed only
    /// what is new (`replay::absorb`).
    pub kept: HashSet<String>,
    pub news_ids: HashSet<i64>,
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
pub(crate) fn is_rest_symbol(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 10
        && s.chars().next().is_some_and(|c| c.is_ascii_uppercase())
        && s.chars()
            .all(|c| c.is_ascii_uppercase() || c == '.' || c == '-')
}

/// Extract the offending symbol from an Alpaca `400 invalid symbol: XXX` body.
pub(crate) fn invalid_symbol_of(body: &str) -> Option<String> {
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
                 &timeframe={timeframe}&start={start}&end={end}&limit=10000&adjustment=split&feed=sip"
            );
            if let Some(tok) = &page_token {
                url.push_str(&format!("&page_token={tok}"));
            }
            crate::alpaca::ratelimit::acquire().await;
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
        crate::alpaca::ratelimit::acquire().await;
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

// ─── Day loading ────────────────────────────────────────────────────────────────

/// Load a prepared day. The store read runs off the async runtime: decoding a
/// busy day's tick blocks is real work.
pub async fn load_day(
    app_dir: &std::path::Path,
    db: &std::sync::Arc<std::sync::Mutex<rusqlite::Connection>>,
    day: &str,
) -> Result<DayData, String> {
    NaiveDate::parse_from_str(day, "%Y-%m-%d")
        .map_err(|_| format!("invalid replay date: {day}"))?;

    // Dislocation's point-in-time RVOL profile, from whatever older minute history
    // exists; non-fatal, the engine keeps its intraday rolling baseline otherwise.
    if let Err(e) = crate::flat_files::minute::ensure_rvol_baseline(app_dir, db, day) {
        eprintln!("[tagdash] replay {day}: baseline RVOL indisponible ({e})");
    }

    let app_dir = app_dir.to_path_buf();
    let day = day.to_string();
    tokio::task::spawn_blocking(move || crate::replay_data::read::read_day(&app_dir, &day))
        .await
        .map_err(|e| e.to_string())?
}

/// Split a daily-bars window into (previous close, this-day volume) per symbol.
/// Used by the flat-file downloader to derive the change% seed and the activity
/// filter.
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

    #[test]
    fn invalid_symbol_extraction() {
        let body = r#"{"message":"invalid symbol: TEST2"}"#;
        assert_eq!(invalid_symbol_of(body).as_deref(), Some("TEST2"));
        assert_eq!(invalid_symbol_of("other error"), None);
    }
}
