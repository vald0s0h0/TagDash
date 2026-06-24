// Minute flat files — one `minute/minute-YYYY-MM-DD.db` per ET trading day.
//
// Holds the 1-minute bars 04:00→20:00 ET (premarket + regular + after-hours) of the
// day's 2000 most-traded symbols, plus that day's previous closes (change% seed) and
// news, so the file is self-contained and shareable. This is the broad offline replay
// source: `read_day` turns each minute bar into synthetic 10-second slices exactly as
// the live replay path does.
//
// A download of day D also ENSURES the 5 previous trading days exist as their own
// minute files (downloaded if missing), so opening a chart offline on D always has an
// intraday lead-in without bloating D's own file.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex};

use chrono::{NaiveDate, Utc};
use rusqlite::Connection;

use super::{
    day_of_file, get_meta, kind_dir, minute_path, set_meta, tmp_path, writer_pragmas, FlatFileDay,
    FlatFilesShared, Kind, SCHEMA_VERSION,
};
use crate::replay::data;
use crate::types::NewsHeadline;

/// How many previous trading days to guarantee alongside a downloaded day (chart
/// intraday lead-in).
const HISTORY_DAYS: usize = 5;

// ─── Availability + calendar ────────────────────────────────────────────────────

pub fn has_day(app_dir: &Path, day: &str) -> bool {
    let path = minute_path(app_dir, day);
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
    let dir = kind_dir(app_dir, Kind::Minute);
    let mut out: Vec<FlatFileDay> = Vec::new();
    let Ok(entries) = std::fs::read_dir(&dir) else { return out };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(day) = day_of_file(&name, "minute-") else { continue };
        let bytes = entry.metadata().map(|m| m.len()).unwrap_or(0);
        let (symbol_count, bar_count, complete) = Connection::open(entry.path())
            .ok()
            .map(|c| {
                let sc = get_meta(&c, "symbol_count").and_then(|v| v.parse().ok()).unwrap_or(0);
                let bc = get_meta(&c, "bar_count").and_then(|v| v.parse().ok()).unwrap_or(0);
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

fn open_writer(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    writer_pragmas(&conn)?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS minute_bars (
             symbol TEXT NOT NULL,
             t_ms   INTEGER NOT NULL,
             o REAL, h REAL, l REAL, c REAL,
             v INTEGER, n INTEGER, vw REAL
         );
         CREATE INDEX IF NOT EXISTS idx_mb_t ON minute_bars(t_ms);
         CREATE TABLE IF NOT EXISTS prev_closes (
             symbol TEXT PRIMARY KEY,
             close  REAL NOT NULL
         );
         CREATE TABLE IF NOT EXISTS news (
             id           INTEGER,
             headline     TEXT,
             summary      TEXT,
             url          TEXT,
             source       TEXT,
             symbols_json TEXT,
             created_ms   INTEGER
         );",
    )?;
    Ok(conn)
}

/// Write the minute file for `day`, and (when `ensure_history`) guarantee the 5
/// previous trading days exist too. Progress is reported on the shared status.
pub async fn write_day(
    shared: &Arc<FlatFilesShared>,
    app_dir: &Path,
    db: &Arc<Mutex<Connection>>,
    key: &str,
    secret: &str,
    day: &str,
    ensure_history: bool,
) -> Result<usize, String> {
    let main_span = if ensure_history { 0.6 } else { 1.0 };
    let bars = write_one_day(app_dir, db, key, secret, day, &|f| shared.set_progress(f * main_span))
        .await?;

    if ensure_history {
        let prior = prior_trading_days(day, HISTORY_DAYS);
        let n = prior.len().max(1) as f32;
        for (i, pday) in prior.iter().enumerate() {
            if shared.cancelled() {
                break;
            }
            if has_day(app_dir, pday) {
                continue;
            }
            let lo = main_span + (i as f32 / n) * (1.0 - main_span);
            let hi = main_span + ((i + 1) as f32 / n) * (1.0 - main_span);
            // Holidays / no-data days simply fail here — skip them, never abort.
            if let Err(e) =
                write_one_day(app_dir, db, key, secret, pday, &|f| shared.set_progress(lo + f * (hi - lo)))
                    .await
            {
                eprintln!("[tagdash] flat_files minute: historique {pday} ignoré ({e})");
            }
        }
    }
    shared.set_progress(1.0);
    Ok(bars)
}

/// Fetch + persist ONE day's minute file. Mirrors the Alpaca path of
/// `replay::data::load_day` (universe → daily window → top-2000 active set → 1-minute
/// bars → news) and writes it atomically.
async fn write_one_day(
    app_dir: &Path,
    db: &Arc<Mutex<Connection>>,
    key: &str,
    secret: &str,
    day: &str,
    progress: &(dyn Fn(f32) + Sync),
) -> Result<usize, String> {
    let nd = NaiveDate::parse_from_str(day, "%Y-%m-%d").map_err(|_| format!("invalid date: {day}"))?;
    let noon = data::noon_utc(nd);
    let pm_start = crate::time::et_clock_utc(noon, 4, 0); // 04:00 ET
    let day_end = crate::time::et_clock_utc(noon, 20, 0); // 20:00 ET

    // 1. Universe (symbols known to the app).
    let universe: Vec<String> = {
        let conn = db.lock().unwrap();
        crate::local_db::universe_repository::get_active_symbols(&conn).map_err(|e| e.to_string())?
    };
    if universe.is_empty() {
        return Err("univers vide — lance d'abord le Startup Pipeline".into());
    }

    // 2. Daily window: previous close + this-day volume (activity filter).
    let daily_start = (nd - chrono::Duration::days(10)).format("%Y-%m-%dT00:00:00Z").to_string();
    let daily_end = day_end.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let daily = data::fetch_bars_window(
        key, secret, &universe, "1Day", &daily_start, &daily_end, &|f| progress(f * 0.18),
    )
    .await?;
    let (prev_closes, day_volume) = data::split_daily(&daily, day);

    // The day's 2000 most-traded symbols (no focus symbols here).
    let active = data::rank_active(&day_volume, &[]);
    if active.is_empty() {
        return Err(format!("aucune donnée de marché pour le {day} (jour non ouvré ?)"));
    }

    // 3. Minute bars 04:00→20:00 ET.
    let min_start = pm_start.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let min_end = day_end.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let minutes = data::fetch_bars_window(
        key, secret, &active, "1Min", &min_start, &min_end, &|f| progress(0.18 + f * 0.72),
    )
    .await?;

    // 4. News of the day (published 00:00 ET → 20:00 ET).
    let news_start = crate::time::et_clock_utc(noon, 0, 0).format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let news = data::fetch_news_window(key, secret, &news_start, &min_end).await.unwrap_or_else(|e| {
        eprintln!("[tagdash] flat_files minute: {day} news ignorées ({e})");
        Vec::new()
    });
    progress(0.95);

    // 5. Write to a temp DB, then atomically rename.
    let final_path = minute_path(app_dir, day);
    let tmp = tmp_path(&final_path);
    let _ = std::fs::remove_file(&tmp);

    let bar_count = {
        let mut conn = open_writer(&tmp).map_err(|e| e.to_string())?;
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        let mut bars = 0usize;
        let mut symbols: HashSet<String> = HashSet::new();
        {
            let mut ins = tx
                .prepare("INSERT INTO minute_bars (symbol, t_ms, o, h, l, c, v, n, vw) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)")
                .map_err(|e| e.to_string())?;
            for (sym, raw_bars) in &minutes {
                for b in raw_bars {
                    let Ok(t) = b.t.parse::<chrono::DateTime<Utc>>() else { continue };
                    ins.execute(rusqlite::params![
                        sym, t.timestamp_millis(), b.o, b.h, b.l, b.c, b.v, b.n.unwrap_or(0), b.vw,
                    ])
                    .map_err(|e| e.to_string())?;
                    bars += 1;
                }
                if !raw_bars.is_empty() {
                    symbols.insert(sym.clone());
                }
            }
            let mut ins_pc = tx
                .prepare("INSERT OR REPLACE INTO prev_closes (symbol, close) VALUES (?1, ?2)")
                .map_err(|e| e.to_string())?;
            for (sym, close) in &prev_closes {
                ins_pc.execute(rusqlite::params![sym, close]).map_err(|e| e.to_string())?;
            }
            let mut ins_news = tx
                .prepare("INSERT INTO news (id, headline, summary, url, source, symbols_json, created_ms) VALUES (?1,?2,?3,?4,?5,?6,?7)")
                .map_err(|e| e.to_string())?;
            for h in &news {
                let syms_json = serde_json::to_string(&h.symbols).unwrap_or_else(|_| "[]".into());
                ins_news
                    .execute(rusqlite::params![
                        h.id, h.headline, h.summary, h.url, h.source, syms_json,
                        h.created_at.timestamp_millis(),
                    ])
                    .map_err(|e| e.to_string())?;
            }
        }
        set_meta(&tx, "schema_version", SCHEMA_VERSION).map_err(|e| e.to_string())?;
        set_meta(&tx, "kind", "minute").map_err(|e| e.to_string())?;
        set_meta(&tx, "day", day).map_err(|e| e.to_string())?;
        set_meta(&tx, "generated_at", &Utc::now().to_rfc3339()).map_err(|e| e.to_string())?;
        set_meta(&tx, "generator", "TagDash").map_err(|e| e.to_string())?;
        set_meta(&tx, "source", "alpaca").map_err(|e| e.to_string())?;
        set_meta(&tx, "timeframe", "1Min").map_err(|e| e.to_string())?;
        set_meta(&tx, "symbol_count", &symbols.len().to_string()).map_err(|e| e.to_string())?;
        set_meta(&tx, "bar_count", &bars.to_string()).map_err(|e| e.to_string())?;
        set_meta(&tx, "complete", "1").map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        drop(conn);
        bars
    };

    let _ = std::fs::remove_file(&final_path);
    std::fs::rename(&tmp, &final_path).map_err(|e| e.to_string())?;
    progress(1.0);
    Ok(bar_count)
}

/// The `n` previous weekday dates before `day` (most recent first). Holidays are not
/// excluded here — they simply fail to download and are skipped by the caller.
fn prior_trading_days(day: &str, n: usize) -> Vec<String> {
    let Ok(mut d) = NaiveDate::parse_from_str(day, "%Y-%m-%d") else { return Vec::new() };
    let mut out = Vec::with_capacity(n);
    while out.len() < n {
        d -= chrono::Duration::days(1);
        let wd = chrono::Datelike::weekday(&d);
        if wd != chrono::Weekday::Sat && wd != chrono::Weekday::Sun {
            out.push(d.format("%Y-%m-%d").to_string());
        }
    }
    out
}

// ─── Reader (offline replay source) ─────────────────────────────────────────────

/// Rebuild a `DayData` from the minute file, optionally overlaying real trades+quotes
/// from the trade file (the synthetic slices inside an overlay window are suppressed).
pub fn read_day(
    app_dir: &Path,
    day: &str,
    overlay: Option<super::trade::TradeOverlay>,
) -> Result<data::DayData, String> {
    let path = minute_path(app_dir, day);
    let conn = Connection::open(&path).map_err(|e| e.to_string())?;
    let ov_ref = overlay.as_ref();

    // Previous closes (change% seed).
    let mut prev_closes: HashMap<String, f64> = HashMap::new();
    {
        let mut stmt =
            conn.prepare("SELECT symbol, close FROM prev_closes").map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?)))
            .map_err(|e| e.to_string())?;
        for row in rows.flatten() {
            prev_closes.insert(row.0, row.1);
        }
    }

    // Minute bars → synthetic 10-second trade tics (skipping overlay windows).
    let mut events: Vec<data::TimedEvent> = Vec::new();
    let mut symbols: HashSet<String> = HashSet::new();
    {
        let mut stmt = conn
            .prepare("SELECT symbol, t_ms, o, h, l, c, v, n, vw FROM minute_bars")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, f64>(2)?,
                    r.get::<_, f64>(3)?,
                    r.get::<_, f64>(4)?,
                    r.get::<_, f64>(5)?,
                    r.get::<_, i64>(6)?,
                    r.get::<_, i64>(7)?,
                    r.get::<_, Option<f64>>(8)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        for row in rows.flatten() {
            let (sym, t_ms, o, h, l, c, v, n, vw) = row;
            // Suppress synthetic slices for a minute fully inside an overlay window —
            // the real trades from the trade file replace them.
            if let Some(ov) = ov_ref {
                if ov.covers(&sym, t_ms, t_ms + 60_000) {
                    symbols.insert(sym);
                    continue;
                }
            }
            let Some(time) = chrono::TimeZone::timestamp_millis_opt(&Utc, t_ms).single() else {
                continue;
            };
            let bar = data::MinBar {
                time,
                open: o,
                high: h,
                low: l,
                close: c,
                volume: v.max(0) as u64,
                trades: n.max(0) as u64,
                vwap: vw,
            };
            for (ts_ms, price, size, prints) in data::slices_of(&bar) {
                events.push(data::TimedEvent {
                    ts_ms,
                    ev: data::Event::Trade { symbol: sym.clone(), price, size, prints },
                });
            }
            symbols.insert(sym);
        }
    }

    // News, emitted at its publication instant.
    {
        let mut stmt = conn
            .prepare("SELECT id, headline, summary, url, source, symbols_json, created_ms FROM news")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, Option<i64>>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, i64>(6)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        for row in rows.flatten() {
            let (id, headline, summary, url, source, symbols_json, created_ms) = row;
            let Some(headline) = headline.filter(|h| !h.trim().is_empty()) else { continue };
            let Some(created) = chrono::TimeZone::timestamp_millis_opt(&Utc, created_ms).single()
            else {
                continue;
            };
            let syms: Vec<String> =
                symbols_json.and_then(|j| serde_json::from_str(&j).ok()).unwrap_or_default();
            events.push(data::TimedEvent {
                ts_ms: created_ms,
                ev: data::Event::News(NewsHeadline {
                    id: id.unwrap_or(0),
                    headline,
                    summary,
                    url,
                    source,
                    symbols: syms,
                    created_at: created,
                    received_at: created,
                }),
            });
        }
    }

    // Real trades+quotes overlay (premarket windows).
    let source = if let Some(ov) = overlay {
        for sym in ov.windows.keys() {
            symbols.insert(sym.clone());
        }
        events.extend(ov.events);
        "trades"
    } else {
        "minutes"
    };

    events.sort_by_key(|e| e.ts_ms);
    Ok(data::DayData { events, prev_closes, source, symbols: symbols.len() })
}
