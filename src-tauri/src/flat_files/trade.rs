// Trade flat files — one `trade/trade-YYYY-MM-DD.db` per ET trading day.
//
// Micro Pullback reasons on seconds/trades, but storing the whole day's tape for the
// whole low-float universe is far too much. Instead we keep real trades AND quotes only
// inside the [alert−1min, alert+10min] windows where a minute-resolution PRE-SCAN says
// the strategy would have ignited that day. The replay engine then overlays these true
// prints on top of the broad minute slices (see `minute::read_day`).
//
// The float is a Micro Pullback FILTERING condition and it drifts over time, so we don't
// trust today's float for a historical day: after the pre-scan narrows down a small set
// of ignition tickers we fetch each one's float AS OF the download day (Massive carries
// an `effective_date`) and drop any that weren't actually low-float back then. The value
// used is recorded in `float_snapshot`.
//
// Discovery is a CHICKEN-AND-EGG problem (knowing the alerts needs fine data) resolved by
// the coarse minute pre-scan: it is self-contained and works for any past day, at the
// cost of a candidate set that is close to — but not strictly identical to — the live
// alerts. The genuine alerts are still produced by the real engine during replay, now on
// real ticks.
//
// The pre-scan's candidate universe is read straight from the day's already-downloaded
// `minute-<day>.db` (its top-2000-by-volume active set, day-correct — not today's
// possibly-stale `universe_assets` snapshot, and not float-filtered upfront: a symbol
// with no known float is admitted to the scan, mirroring micro_pullback's
// `allow_unknown_float` gate — see `admit_float`). The current float is only used as a
// generous heuristic prefilter on symbols that DO have one on record.
//
// News is fetched per (symbol, ignition window) over the NEWS_LOOKBACK_MS days
// strictly BEFORE that alert's `alert_ms` — never after, to avoid leaking news a
// trader reviewing the alert offline couldn't have known about at the time.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, NaiveDate, Utc};
use rusqlite::Connection;

use super::{
    day_of_file, get_meta, kind_dir, set_meta, tmp_path, trade_path, writer_pragmas, FlatFileDay,
    FlatFilesShared, Kind, SCHEMA_VERSION,
};
use crate::replay::data;

// ── Pre-scan tuning (coarse minute-resolution approximation of the ignition gate) ──
/// Generous current-float ceiling for the candidate fetch. Wider than the strategy's
/// `float_max` so a name that has DILUTED since the day (float grew) is still fetched;
/// the as-of float is the final arbiter. Names with NO known float are admitted
/// unconditionally (see `admit_float`) — mirrors `allow_unknown_float`.
const PREFILTER_FLOAT_MULT: u64 = 3;
/// A minute ignites when volume or range clears these multiples of the trailing
/// 5-minute baseline (and the absolute volume floor below).
const VOL_RATIO_TRIG: f64 = 4.0;
const RANGE_RATIO_TRIG: f64 = 3.0;
const MIN_IGNITION_VOL: i64 = 20_000;
const BASELINE_VOL_FLOOR: f64 = 2_000.0;
const BASELINE_RANGE_FLOOR_PCT: f64 = 0.3;
const MIN_BASELINE_MIN: usize = 2;
/// Window around an ignition minute: −1 min, +10 min.
const WIN_BEFORE_MS: i64 = 60_000;
const WIN_AFTER_MS: i64 = 600_000;
/// Cap on ignition symbols per day (bounds the per-ticker as-of float calls — Massive's
/// free tier is ~1 req/13s — and the trade/quote downloads). 120 ≈ 26 min of as-of
/// calls/day.
const MAX_WINDOW_SYMBOLS: usize = 120;
/// News lookback before each ignition alert — strictly BEFORE the alert, never after:
/// a trader reviewing the alert offline must only see what was knowable at that
/// moment (no look-ahead / data leakage).
const NEWS_LOOKBACK_MS: i64 = 3 * 24 * 60 * 60 * 1_000;

// ─── Availability + calendar ────────────────────────────────────────────────────

pub fn has_day(app_dir: &Path, day: &str) -> bool {
    let path = trade_path(app_dir, day);
    if !path.exists() {
        return false;
    }
    Connection::open(&path)
        .ok()
        .and_then(|c| get_meta(&c, "complete"))
        .map(|v| v == "1")
        .unwrap_or(false)
}

pub fn calendar(app_dir: &Path) -> Vec<FlatFileDay> {
    let dir = kind_dir(app_dir, Kind::Trade);
    let mut out: Vec<FlatFileDay> = Vec::new();
    let Ok(entries) = std::fs::read_dir(&dir) else { return out };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(day) = day_of_file(&name, "trade-") else { continue };
        let bytes = entry.metadata().map(|m| m.len()).unwrap_or(0);
        let (symbol_count, bar_count, complete) = Connection::open(entry.path())
            .ok()
            .map(|c| {
                let sc = get_meta(&c, "symbol_count").and_then(|v| v.parse().ok()).unwrap_or(0);
                let bc = get_meta(&c, "trade_count").and_then(|v| v.parse().ok()).unwrap_or(0);
                let complete = get_meta(&c, "complete").map(|v| v == "1").unwrap_or(false);
                (sc, bc, complete)
            })
            .unwrap_or((0, 0, false));
        out.push(FlatFileDay { day, bytes, symbol_count, bar_count, complete });
    }
    out.sort_by(|a, b| a.day.cmp(&b.day));
    out
}

// ─── Writer ─────────────────────────────────────────────────────────────────────

/// One ignition window for a symbol.
struct Window {
    start_ms: i64,
    end_ms: i64,
    alert_ms: i64,
    reason: String,
}

fn open_writer(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    writer_pragmas(&conn)?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS trades (
             symbol TEXT NOT NULL, t_ms INTEGER NOT NULL, price REAL, size INTEGER
         );
         CREATE INDEX IF NOT EXISTS idx_trades_sym ON trades(symbol, t_ms);
         CREATE TABLE IF NOT EXISTS quotes (
             symbol TEXT NOT NULL, t_ms INTEGER NOT NULL,
             bid REAL, ask REAL, bid_size INTEGER, ask_size INTEGER
         );
         CREATE INDEX IF NOT EXISTS idx_quotes_sym ON quotes(symbol, t_ms);
         CREATE TABLE IF NOT EXISTS float_snapshot (
             symbol TEXT PRIMARY KEY,
             float_shares REAL,
             effective_date TEXT,
             as_of_day TEXT,
             historical INTEGER NOT NULL DEFAULT 0
         );
         CREATE TABLE IF NOT EXISTS windows (
             symbol TEXT NOT NULL, alert_ts INTEGER, start_ts INTEGER, end_ts INTEGER, reason TEXT
         );
         CREATE TABLE IF NOT EXISTS news (
             id INTEGER, headline TEXT NOT NULL, created_ms INTEGER NOT NULL, symbols_json TEXT
         );",
    )?;
    Ok(conn)
}

/// Run the full TRADE pipeline for `day` and write `trade-<day>.db` atomically.
pub async fn write_day(
    shared: &Arc<FlatFilesShared>,
    app_dir: &Path,
    db: &Arc<Mutex<Connection>>,
    key: &str,
    secret: &str,
    massive_key: &str,
    day: &str,
) -> Result<usize, String> {
    let cfg = crate::micro_pullback::Config::DEFAULT;
    let nd = NaiveDate::parse_from_str(day, "%Y-%m-%d").map_err(|_| format!("invalid date: {day}"))?;
    let noon = data::noon_utc(nd);
    let pm_start = crate::time::et_clock_utc(noon, 4, 0); // 04:00 ET
    let cash_open = crate::time::et_clock_utc(noon, 9, 30); // 09:30 ET (Micro Pullback is premarket-only)

    // 1. Candidate universe + premarket bars: read straight from the day's minute
    // file (already downloaded — it holds the day's top-2000-by-volume active set,
    // day-correct, not float-filtered). No Alpaca re-fetch needed.
    if !super::minute::has_day(app_dir, day) {
        return Err(format!("le fichier minute du {day} doit être téléchargé avant le fichier trade"));
    }
    let bars = super::minute::read_premarket_bars(
        app_dir, day, pm_start.timestamp_millis(), cash_open.timestamp_millis(),
    )?;
    if bars.is_empty() {
        return Err(format!("aucune donnée minute premarket pour le {day}"));
    }
    shared.set_progress(0.15);

    // 2. Current float lookup (batch, used as a heuristic prefilter — the as-of
    // float in step 4 is the final arbiter). A symbol with no known float is
    // admitted, mirroring micro_pullback's gate1_tradeable (`allow_unknown_float`).
    let prefilter_max = cfg.float_max.saturating_mul(PREFILTER_FLOAT_MULT);
    let current_floats: HashMap<String, u64> = {
        let conn = db.lock().unwrap();
        crate::local_db::universe_repository::get_all(&conn)
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter_map(|a| a.float_shares.filter(|f| *f > 0).map(|f| (a.symbol, f as u64)))
            .collect()
    };

    // 3. Pre-scan → ignition windows per symbol (strongest first, capped).
    let mut scored: Vec<(String, f64, Vec<Window>)> = Vec::new();
    for (sym, raw) in &bars {
        if !admit_float(current_floats.get(sym).copied(), prefilter_max) {
            continue;
        }
        if let Some((strength, wins)) = scan_symbol(raw, &cfg) {
            scored.push((sym.clone(), strength, wins));
        }
    }
    let ignited_candidates = scored.len();
    let capped = ignited_candidates > MAX_WINDOW_SYMBOLS;
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(MAX_WINDOW_SYMBOLS);
    shared.set_progress(0.55);

    // 4. As-of float refine: keep only symbols low-float ON the day; record the value.
    struct FloatRec {
        float_shares: f64,
        effective_date: Option<String>,
        historical: bool,
    }
    let mut floats: HashMap<String, FloatRec> = HashMap::new();
    let mut kept: Vec<(String, Vec<Window>)> = Vec::new();
    let n_scored = scored.len().max(1) as f32;
    for (i, (sym, _strength, wins)) in scored.into_iter().enumerate() {
        if shared.cancelled() {
            break;
        }
        shared.set_progress(0.55 + (i as f32 / n_scored) * 0.20);
        // Default: current float snapshot from the universe table (historical=false).
        let current = {
            let conn = db.lock().unwrap();
            crate::local_db::universe_repository::get_by_symbol(&conn, &sym)
                .ok()
                .flatten()
                .and_then(|a| a.float_shares)
                .map(|f| f as f64)
        };
        let mut rec = FloatRec {
            float_shares: current.unwrap_or(0.0),
            effective_date: None,
            historical: false,
        };
        if !massive_key.is_empty() {
            match crate::massive::fetch_float_asof(massive_key, &sym, day).await {
                Ok(Some(asof)) => {
                    rec.float_shares = asof.float_shares;
                    rec.effective_date = asof.effective_date;
                    rec.historical = asof.historical;
                }
                Ok(None) => {}
                Err(e) => eprintln!("[tagdash] flat_files trade: float as-of {sym} {day}: {e}"),
            }
            // Honour Massive's free-tier rate limit between per-ticker calls.
            tokio::time::sleep(crate::massive::RATE_LIMIT).await;
        }
        // Drop names that were NOT low-float on the day (known float over the strategy
        // ceiling). Unknown/zero float is kept (mirrors allow_unknown_float).
        if rec.float_shares > 0.0 && rec.float_shares as u64 > cfg.float_max {
            continue;
        }
        floats.insert(sym.clone(), rec);
        kept.push((sym, wins));
    }

    // 5. Fetch real trades + quotes inside each kept window.
    let mut all_trades: Vec<(String, i64, f64, i64)> = Vec::new();
    let mut all_quotes: Vec<(String, i64, f64, f64, i64, i64)> = Vec::new();
    let n_kept = kept.len().max(1) as f32;
    for (i, (sym, wins)) in kept.iter().enumerate() {
        if shared.cancelled() {
            break;
        }
        shared.set_progress(0.75 + (i as f32 / n_kept) * 0.23);
        let one = [sym.clone()];
        for w in wins {
            let (Some(s), Some(e)) = (ms_to_rfc3339(w.start_ms), ms_to_rfc3339(w.end_ms)) else {
                continue;
            };
            match fetch_with_retry(|| data::fetch_trades_window(key, secret, &one, &s, &e)).await {
                Ok(map) => {
                    for t in map.get(sym).into_iter().flatten() {
                        if let Some(ms) = rfc3339_to_ms(&t.t) {
                            all_trades.push((sym.clone(), ms, t.p, t.s));
                        }
                    }
                }
                Err(e) => eprintln!("[tagdash] flat_files trade: fetch trades {sym}: {e}"),
            }
            match fetch_with_retry(|| data::fetch_quotes_window(key, secret, &one, &s, &e)).await {
                Ok(map) => {
                    for q in map.get(sym).into_iter().flatten() {
                        if let Some(ms) = rfc3339_to_ms(&q.t) {
                            all_quotes.push((sym.clone(), ms, q.bp, q.ap, q.bs, q.as_size));
                        }
                    }
                }
                Err(e) => eprintln!("[tagdash] flat_files trade: fetch quotes {sym}: {e}"),
            }
        }
    }
    shared.set_progress(0.93);

    // 5b. Fetch news per (symbol, alert) — strictly the NEWS_LOOKBACK_MS window
    // BEFORE each ignition, never after: a trader reviewing the alert offline must
    // only see what was knowable at the alert moment, not future-leaking news.
    let mut news_by_id: HashMap<i64, crate::types::NewsHeadline> = HashMap::new();
    for (sym, wins) in &kept {
        if shared.cancelled() {
            break;
        }
        let one = [sym.clone()];
        for w in wins {
            let (Some(s), Some(e)) =
                (ms_to_rfc3339(w.alert_ms - NEWS_LOOKBACK_MS), ms_to_rfc3339(w.alert_ms - 1_000))
            else {
                continue;
            };
            match fetch_with_retry(|| data::fetch_news_window(key, secret, &s, &e, &one)).await {
                Ok(items) => {
                    for h in items {
                        news_by_id.entry(h.id).or_insert(h);
                    }
                }
                Err(e) => eprintln!("[tagdash] flat_files trade: news {sym} {day}: {e}"),
            }
        }
    }
    let all_news: Vec<crate::types::NewsHeadline> = news_by_id.into_values().collect();
    shared.set_progress(0.98);

    // 6. Write atomically (even when empty — a complete file marks the day done).
    let final_path = trade_path(app_dir, day);
    let tmp = tmp_path(&final_path);
    let _ = std::fs::remove_file(&tmp);
    let trade_count = all_trades.len();
    {
        let mut conn = open_writer(&tmp).map_err(|e| e.to_string())?;
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        {
            let mut ins_t = tx
                .prepare("INSERT INTO trades (symbol, t_ms, price, size) VALUES (?1,?2,?3,?4)")
                .map_err(|e| e.to_string())?;
            for (s, ms, p, sz) in &all_trades {
                ins_t.execute(rusqlite::params![s, ms, p, sz]).map_err(|e| e.to_string())?;
            }
            let mut ins_q = tx
                .prepare("INSERT INTO quotes (symbol, t_ms, bid, ask, bid_size, ask_size) VALUES (?1,?2,?3,?4,?5,?6)")
                .map_err(|e| e.to_string())?;
            for (s, ms, bid, ask, bs, az) in &all_quotes {
                ins_q.execute(rusqlite::params![s, ms, bid, ask, bs, az]).map_err(|e| e.to_string())?;
            }
            let mut ins_f = tx
                .prepare("INSERT OR REPLACE INTO float_snapshot (symbol, float_shares, effective_date, as_of_day, historical) VALUES (?1,?2,?3,?4,?5)")
                .map_err(|e| e.to_string())?;
            for (s, r) in &floats {
                ins_f
                    .execute(rusqlite::params![s, r.float_shares, r.effective_date, day, r.historical as i64])
                    .map_err(|e| e.to_string())?;
            }
            let mut ins_w = tx
                .prepare("INSERT INTO windows (symbol, alert_ts, start_ts, end_ts, reason) VALUES (?1,?2,?3,?4,?5)")
                .map_err(|e| e.to_string())?;
            for (s, wins) in &kept {
                for w in wins {
                    ins_w
                        .execute(rusqlite::params![s, w.alert_ms, w.start_ms, w.end_ms, w.reason])
                        .map_err(|e| e.to_string())?;
                }
            }
            let mut ins_n = tx
                .prepare("INSERT INTO news (id, headline, created_ms, symbols_json) VALUES (?1,?2,?3,?4)")
                .map_err(|e| e.to_string())?;
            for h in &all_news {
                let syms_json = serde_json::to_string(&h.symbols).unwrap_or_else(|_| "[]".into());
                ins_n
                    .execute(rusqlite::params![h.id, h.headline, h.created_at.timestamp_millis(), syms_json])
                    .map_err(|e| e.to_string())?;
            }
        }
        set_meta(&tx, "schema_version", SCHEMA_VERSION).map_err(|e| e.to_string())?;
        set_meta(&tx, "kind", "trade").map_err(|e| e.to_string())?;
        set_meta(&tx, "day", day).map_err(|e| e.to_string())?;
        set_meta(&tx, "generated_at", &Utc::now().to_rfc3339()).map_err(|e| e.to_string())?;
        set_meta(&tx, "generator", "TagDash").map_err(|e| e.to_string())?;
        set_meta(&tx, "source", "alpaca").map_err(|e| e.to_string())?;
        set_meta(&tx, "symbol_count", &kept.len().to_string()).map_err(|e| e.to_string())?;
        set_meta(&tx, "trade_count", &trade_count.to_string()).map_err(|e| e.to_string())?;
        set_meta(&tx, "quote_count", &all_quotes.len().to_string()).map_err(|e| e.to_string())?;
        set_meta(&tx, "news_count", &all_news.len().to_string()).map_err(|e| e.to_string())?;
        set_meta(&tx, "ignited_candidates", &ignited_candidates.to_string()).map_err(|e| e.to_string())?;
        set_meta(&tx, "capped", if capped { "1" } else { "0" }).map_err(|e| e.to_string())?;
        set_meta(&tx, "complete", "1").map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        drop(conn);
    }
    let _ = std::fs::remove_file(&final_path);
    std::fs::rename(&tmp, &final_path).map_err(|e| e.to_string())?;
    shared.set_progress(1.0);
    Ok(trade_count)
}

/// Retry a fallible fetch up to 2 extra times (3 attempts total) with a short fixed
/// backoff, so a transient API hiccup doesn't silently zero out a kept symbol's window.
async fn fetch_with_retry<T, F, Fut>(mut f: F) -> Result<T, String>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, String>>,
{
    let mut last_err = String::new();
    for attempt in 0..3 {
        if attempt > 0 {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
        match f().await {
            Ok(v) => return Ok(v),
            Err(e) => last_err = e,
        }
    }
    Err(last_err)
}

/// Mirrors micro_pullback::gate1_tradeable's float admission: known float must clear
/// the (generous) prefilter ceiling; unknown float is admitted (the real gate's
/// `allow_unknown_float` default — the pre-scan must not be narrower than the gate it
/// approximates).
fn admit_float(float: Option<u64>, prefilter_max: u64) -> bool {
    match float {
        Some(f) => f <= prefilter_max,
        None => true,
    }
}

/// Minute-resolution ignition pre-scan for one symbol's premarket bars. Returns the
/// strongest volume ratio seen (ranking key) and the merged ignition windows, or None
/// when nothing fired. Coarse approximation of `micro_pullback`'s gate.
fn scan_symbol(
    raw: &[(i64, f64, f64, f64, f64, i64)],
    cfg: &crate::micro_pullback::Config,
) -> Option<(f64, Vec<Window>)> {
    // Sort the minute bars (t, o, h, l, c, v).
    let mut bars: Vec<(i64, f64, f64, f64, f64, i64)> = raw.to_vec();
    bars.sort_by_key(|b| b.0);
    if bars.len() <= MIN_BASELINE_MIN {
        return None;
    }

    let mut wins: Vec<Window> = Vec::new();
    let mut max_ratio = 0.0f64;
    for i in MIN_BASELINE_MIN..bars.len() {
        let (t_ms, o, h, l, c, v) = bars[i];
        if !(cfg.price_min..=cfg.price_max).contains(&c) {
            continue;
        }
        if v < MIN_IGNITION_VOL {
            continue;
        }
        let base = &bars[i.saturating_sub(5)..i];
        if base.len() < MIN_BASELINE_MIN {
            continue;
        }
        let base_vol = base.iter().map(|b| b.5 as f64).sum::<f64>() / base.len() as f64;
        let base_range = base
            .iter()
            .map(|b| if b.1 > 0.0 { (b.2 - b.3) / b.1 * 100.0 } else { 0.0 })
            .sum::<f64>()
            / base.len() as f64;
        let cur_range = if o > 0.0 { (h - l) / o * 100.0 } else { 0.0 };
        let vol_ratio = v as f64 / base_vol.max(BASELINE_VOL_FLOOR);
        let range_ratio = cur_range / base_range.max(BASELINE_RANGE_FLOOR_PCT);
        if vol_ratio >= VOL_RATIO_TRIG || range_ratio >= RANGE_RATIO_TRIG {
            max_ratio = max_ratio.max(vol_ratio);
            let start = t_ms - WIN_BEFORE_MS;
            let end = t_ms + WIN_AFTER_MS;
            let reason = format!("vol×{vol_ratio:.1} range×{range_ratio:.1}");
            match wins.last_mut() {
                // Merge with the previous window when they overlap/touch.
                Some(w) if start <= w.end_ms => w.end_ms = w.end_ms.max(end),
                _ => wins.push(Window { start_ms: start, end_ms: end, alert_ms: t_ms, reason }),
            }
        }
    }
    if wins.is_empty() {
        None
    } else {
        Some((max_ratio, wins))
    }
}

// ─── Reading (replay overlay) ───────────────────────────────────────────────────

/// The trade-file overlay consumed by `minute::read_day`: the windows (to suppress the
/// synthetic minute slices inside them) and the real trade/quote events to inject.
pub struct TradeOverlay {
    pub windows: HashMap<String, Vec<(i64, i64)>>,
    pub events: Vec<data::TimedEvent>,
}

impl TradeOverlay {
    /// True when `[lo, hi]` overlaps any window for `symbol`.
    pub fn covers(&self, symbol: &str, lo: i64, hi: i64) -> bool {
        self.windows
            .get(symbol)
            .map(|ws| ws.iter().any(|(s, e)| *s <= hi && *e >= lo))
            .unwrap_or(false)
    }
}

/// Read the trade file for `day` into an overlay, or None when there is no (complete)
/// trade file.
pub fn read_overlay(app_dir: &Path, day: &str) -> Option<TradeOverlay> {
    if !has_day(app_dir, day) {
        return None;
    }
    let conn = Connection::open(trade_path(app_dir, day)).ok()?;

    let mut windows: HashMap<String, Vec<(i64, i64)>> = HashMap::new();
    {
        let mut stmt = conn.prepare("SELECT symbol, start_ts, end_ts FROM windows").ok()?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)))
            .ok()?;
        for row in rows.flatten() {
            windows.entry(row.0).or_default().push((row.1, row.2));
        }
    }

    let mut events: Vec<data::TimedEvent> = Vec::new();
    {
        let mut stmt = conn.prepare("SELECT symbol, t_ms, price, size FROM trades").ok()?;
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, f64>(2)?, r.get::<_, i64>(3)?))
            })
            .ok()?;
        for row in rows.flatten() {
            let (symbol, t_ms, price, size) = row;
            events.push(data::TimedEvent {
                ts_ms: t_ms,
                ev: data::Event::Trade { symbol, price, size: size.max(0) as u64, prints: 1 },
            });
        }
    }
    {
        let mut stmt = conn.prepare("SELECT symbol, t_ms, bid, ask FROM quotes").ok()?;
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, f64>(2)?, r.get::<_, f64>(3)?))
            })
            .ok()?;
        for row in rows.flatten() {
            let (symbol, t_ms, bid, ask) = row;
            events.push(data::TimedEvent { ts_ms: t_ms, ev: data::Event::Quote { symbol, bid, ask } });
        }
    }

    // News (may be absent in older trade files).
    if let Ok(mut stmt) =
        conn.prepare("SELECT id, headline, created_ms, symbols_json FROM news")
    {
        if let Ok(rows) = stmt.query_map([], |r| {
            Ok((
                r.get::<_, Option<i64>>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        }) {
            for row in rows.flatten() {
                let (id, headline, created_ms, symbols_json) = row;
                let Some(created) =
                    chrono::TimeZone::timestamp_millis_opt(&Utc, created_ms).single()
                else {
                    continue;
                };
                let syms: Vec<String> =
                    symbols_json.and_then(|j| serde_json::from_str(&j).ok()).unwrap_or_default();
                events.push(data::TimedEvent {
                    ts_ms: created_ms,
                    ev: data::Event::News(crate::types::NewsHeadline {
                        id: id.unwrap_or(0),
                        headline,
                        summary: None,
                        url: None,
                        source: None,
                        symbols: syms,
                        created_at: created,
                        received_at: created,
                    }),
                });
            }
        }
    }

    Some(TradeOverlay { windows, events })
}

// ─── Time helpers ───────────────────────────────────────────────────────────────

fn ms_to_rfc3339(ms: i64) -> Option<String> {
    chrono::TimeZone::timestamp_millis_opt(&Utc, ms)
        .single()
        .map(|d| d.format("%Y-%m-%dT%H:%M:%SZ").to_string())
}

fn rfc3339_to_ms(s: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(s).ok().map(|d| d.with_timezone(&Utc).timestamp_millis())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::micro_pullback::Config;

    fn bar(t: &str, o: f64, h: f64, l: f64, c: f64, v: i64) -> (i64, f64, f64, f64, f64, i64) {
        (rfc3339_to_ms(t).unwrap(), o, h, l, c, v)
    }

    #[test]
    fn quiet_tape_produces_no_window() {
        let bars: Vec<(i64, f64, f64, f64, f64, i64)> = (0..10)
            .map(|i| {
                let t = format!("2026-06-10T08:{:02}:00Z", i);
                bar(&t, 2.0, 2.01, 1.99, 2.0, 1_000)
            })
            .collect();
        assert!(scan_symbol(&bars, &Config::DEFAULT).is_none());
    }

    #[test]
    fn volume_spike_fires_a_window() {
        let mut bars: Vec<(i64, f64, f64, f64, f64, i64)> = (0..6)
            .map(|i| bar(&format!("2026-06-10T08:{:02}:00Z", i), 2.0, 2.02, 1.98, 2.0, 1_000))
            .collect();
        // A sudden 50k-share, wide-range minute well in the price band.
        bars.push(bar("2026-06-10T08:06:00Z", 2.0, 2.4, 2.0, 2.35, 50_000));
        let (strength, wins) = scan_symbol(&bars, &Config::DEFAULT).expect("ignition");
        assert!(strength >= VOL_RATIO_TRIG);
        assert_eq!(wins.len(), 1);
        // Window spans −1min..+10min around the spike minute.
        let alert = rfc3339_to_ms("2026-06-10T08:06:00Z").unwrap();
        assert_eq!(wins[0].start_ms, alert - WIN_BEFORE_MS);
        assert_eq!(wins[0].end_ms, alert + WIN_AFTER_MS);
    }

    #[test]
    fn overlay_covers_overlapping_range() {
        let mut windows = HashMap::new();
        windows.insert("AAA".to_string(), vec![(1_000, 2_000)]);
        let ov = TradeOverlay { windows, events: Vec::new() };
        assert!(ov.covers("AAA", 1_500, 1_600));
        assert!(ov.covers("AAA", 900, 1_100));
        assert!(!ov.covers("AAA", 2_100, 2_200));
        assert!(!ov.covers("BBB", 1_500, 1_600));
    }

    #[test]
    fn admit_float_keeps_unknown_and_low_known() {
        let ceiling = 90_000_000u64;
        assert!(admit_float(None, ceiling), "unknown float must be admitted");
        assert!(admit_float(Some(50_000_000), ceiling));
        assert!(!admit_float(Some(100_000_000), ceiling));
    }
}
