// Flat files — on-disk, shareable, offline copies of a trading day's market data.
//
// Alpaca's REST history is the only source TagDash can re-download, and only down
// to 1-minute granularity. This module persists, per ET trading day, exactly what
// `replay::data::load_day` would otherwise fetch live (previous closes + the
// liquid active set's 1-minute bars 04:00→20:00 ET + the day's news) into one
// self-contained SQLite file `<app_dir>/flat_files/flat-YYYY-MM-DD.db`.
//
// A downloaded day can then be replayed entirely offline (see the flat-file branch
// in `replay::data::load_day`) — useful when the API quota is gone, the account is
// closed, or to share a day with another TagDash user (just copy the `.db` file
// into their `flat_files/` folder; it shows up green in the calendar).
//
// Format choice mirrors `replay::tape`: one SQLite file per day, written
// atomically (to a `.tmp` then renamed) so an interrupted download never leaves a
// file that reads as "complete". The fetchers and the minute→slice synthesis are
// reused from `replay::data` so a flat file holds the very same data the live
// replay path produces.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use chrono::{NaiveDate, Utc};
use rusqlite::Connection;
use serde::Serialize;

use crate::replay::data;
use crate::types::NewsHeadline;

/// Bumped if the on-disk schema ever changes.
const SCHEMA_VERSION: &str = "1";

// ─── Paths ────────────────────────────────────────────────────────────────────

/// Directory holding the per-day flat files.
pub fn flat_dir(app_dir: &Path) -> PathBuf {
    app_dir.join("flat_files")
}

/// Final path of the flat file for one ET day (YYYY-MM-DD).
pub fn flat_path(app_dir: &Path, day: &str) -> PathBuf {
    flat_dir(app_dir).join(format!("flat-{day}.db"))
}

/// Temp path used while a day is being written (renamed to `flat_path` on success).
fn flat_tmp_path(app_dir: &Path, day: &str) -> PathBuf {
    flat_dir(app_dir).join(format!("flat-{day}.db.tmp"))
}

/// Extract the YYYY-MM-DD day from a `flat-YYYY-MM-DD.db` file name.
fn day_of_file(name: &str) -> Option<String> {
    let stem = name.strip_prefix("flat-")?.strip_suffix(".db")?;
    NaiveDate::parse_from_str(stem, "%Y-%m-%d").ok()?;
    Some(stem.to_string())
}

// ─── Status (polled by the frontend) ───────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct FlatFilesStatus {
    pub running: bool,
    /// idle | running | done | cancelled | error
    pub state: String,
    pub current_day: Option<String>,
    pub day_index: usize,
    pub day_total: usize,
    /// 0..1 within the current day.
    pub progress: f32,
    pub error: Option<String>,
    /// Last day successfully written (YYYY-MM-DD).
    pub last_done: Option<String>,
}

impl Default for FlatFilesStatus {
    fn default() -> Self {
        Self {
            running: false,
            state: "idle".into(),
            current_day: None,
            day_index: 0,
            day_total: 0,
            progress: 0.0,
            error: None,
            last_done: None,
        }
    }
}

/// One row of the calendar: a day present on disk.
#[derive(Debug, Clone, Serialize)]
pub struct FlatFileDay {
    pub day: String,
    pub bytes: u64,
    pub symbol_count: i64,
    pub bar_count: i64,
    /// false when the file is a partial/interrupted download.
    pub complete: bool,
}

/// Shared handle stored in AppState (status polled by the UI + cancel flag).
pub struct FlatFilesShared {
    pub status: RwLock<FlatFilesStatus>,
    pub cancel: AtomicBool,
}

impl Default for FlatFilesShared {
    fn default() -> Self {
        Self { status: RwLock::new(FlatFilesStatus::default()), cancel: AtomicBool::new(false) }
    }
}

impl FlatFilesShared {
    pub fn is_running(&self) -> bool {
        self.status.read().unwrap().running
    }
    pub fn request_cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

// ─── Availability + calendar ────────────────────────────────────────────────────

/// True when a COMPLETE flat file exists for `day` (partial downloads return false
/// so replay never reads a half-written day).
pub fn has_day(app_dir: &Path, day: &str) -> bool {
    let path = flat_path(app_dir, day);
    if !path.exists() {
        return false;
    }
    Connection::open(&path)
        .ok()
        .and_then(|c| get_meta(&c, "complete"))
        .map(|v| v == "1")
        .unwrap_or(false)
}

/// Scan the flat-files directory and describe every day present (ascending).
pub fn calendar(app_dir: &Path) -> Vec<FlatFileDay> {
    let dir = flat_dir(app_dir);
    let mut out: Vec<FlatFileDay> = Vec::new();
    let Ok(entries) = std::fs::read_dir(&dir) else { return out };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(day) = day_of_file(&name) else { continue };
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

// ─── SQLite helpers ─────────────────────────────────────────────────────────────

fn open_writer(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.execute_batch(
        // MEMORY journal + no sidecar files: this writer does one bulk import then
        // the file is atomically renamed into place, so a -wal/-shm pair would only
        // get in the way of the rename. A crash mid-write just leaves a discarded
        // `.tmp` that the next run overwrites.
        "PRAGMA journal_mode = MEMORY;
         PRAGMA synchronous = OFF;
         CREATE TABLE IF NOT EXISTS minute_bars (
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
         );
         CREATE TABLE IF NOT EXISTS manifest (
             key   TEXT PRIMARY KEY,
             value TEXT
         );",
    )?;
    Ok(conn)
}

fn set_meta(conn: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO manifest (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![key, value],
    )?;
    Ok(())
}

fn get_meta(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row("SELECT value FROM manifest WHERE key = ?1", [key], |r| r.get::<_, String>(0))
        .ok()
}

// ─── Reading (offline replay source) ────────────────────────────────────────────

/// Rebuild a `DayData` from the stored flat file — the offline equivalent of
/// `replay::data::load_day`'s "minutes" path. Applies the same `slices_of`
/// synthesis so the strategy engines see identical 10-second tics.
pub fn read_day(app_dir: &Path, day: &str) -> Result<data::DayData, String> {
    let path = flat_path(app_dir, day);
    let conn = Connection::open(&path).map_err(|e| e.to_string())?;

    // Previous closes (change% seed).
    let mut prev_closes: HashMap<String, f64> = HashMap::new();
    {
        let mut stmt = conn.prepare("SELECT symbol, close FROM prev_closes").map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?)))
            .map_err(|e| e.to_string())?;
        for row in rows.flatten() {
            prev_closes.insert(row.0, row.1);
        }
    }

    // Minute bars → synthetic 10-second trade tics.
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
            let Some(time) = chrono::TimeZone::timestamp_millis_opt(&Utc, t_ms).single() else { continue };
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
            let Some(created) = chrono::TimeZone::timestamp_millis_opt(&Utc, created_ms).single() else {
                continue;
            };
            let syms: Vec<String> = symbols_json
                .and_then(|j| serde_json::from_str(&j).ok())
                .unwrap_or_default();
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

    events.sort_by_key(|e| e.ts_ms);
    Ok(data::DayData { events, prev_closes, source: "minutes", symbols: symbols.len() })
}

// ─── Downloading ────────────────────────────────────────────────────────────────

/// Number of weekdays (Mon–Fri) in [start, end] inclusive — the day total shown
/// in the status (holidays may still produce no file, but they are rare).
fn weekday_count(start: NaiveDate, end: NaiveDate) -> usize {
    let mut d = start;
    let mut n = 0;
    while d <= end {
        let wd = chrono::Datelike::weekday(&d);
        if wd != chrono::Weekday::Sat && wd != chrono::Weekday::Sun {
            n += 1;
        }
        d += chrono::Duration::days(1);
    }
    n
}

/// Start a background download of every weekday in [start_day, end_day]. Already
/// complete days are skipped. Errors on individual days (e.g. market holidays with
/// no data) are recorded but never abort the whole range. Honours the cancel flag
/// between days.
pub fn start_download(
    shared: Arc<FlatFilesShared>,
    app_dir: PathBuf,
    db: Arc<Mutex<Connection>>,
    key: String,
    secret: String,
    start_day: String,
    end_day: String,
) -> Result<(), String> {
    if shared.is_running() {
        return Err("un téléchargement est déjà en cours".into());
    }
    let start = NaiveDate::parse_from_str(&start_day, "%Y-%m-%d")
        .map_err(|_| "date de début invalide".to_string())?;
    let end = NaiveDate::parse_from_str(&end_day, "%Y-%m-%d")
        .map_err(|_| "date de fin invalide".to_string())?;
    if start > end {
        return Err("la date de début est postérieure à la date de fin".into());
    }
    if key.is_empty() || secret.is_empty() {
        return Err("clés Alpaca non configurées — nécessaires pour télécharger".into());
    }

    let total = weekday_count(start, end);
    {
        let mut st = shared.status.write().unwrap();
        *st = FlatFilesStatus::default();
        st.running = true;
        st.state = "running".into();
        st.day_total = total;
    }
    shared.cancel.store(false, Ordering::Relaxed);

    tauri::async_runtime::spawn(async move {
        let _ = std::fs::create_dir_all(flat_dir(&app_dir));
        let mut d = start;
        let mut index = 0usize;
        while d <= end {
            if shared.cancel.load(Ordering::Relaxed) {
                let mut st = shared.status.write().unwrap();
                st.running = false;
                st.state = "cancelled".into();
                return;
            }
            let wd = chrono::Datelike::weekday(&d);
            if wd == chrono::Weekday::Sat || wd == chrono::Weekday::Sun {
                d += chrono::Duration::days(1);
                continue;
            }
            let day = d.format("%Y-%m-%d").to_string();
            index += 1;
            {
                let mut st = shared.status.write().unwrap();
                st.current_day = Some(day.clone());
                st.day_index = index;
                st.progress = 0.0;
            }

            if has_day(&app_dir, &day) {
                // Already downloaded — count it done and move on.
                let mut st = shared.status.write().unwrap();
                st.progress = 1.0;
                st.last_done = Some(day.clone());
            } else {
                let sp = shared.clone();
                let res = write_day(&app_dir, &db, &key, &secret, &day, &|f| {
                    sp.status.write().unwrap().progress = f;
                })
                .await;
                match res {
                    Ok(_) => {
                        let mut st = shared.status.write().unwrap();
                        st.progress = 1.0;
                        st.last_done = Some(day.clone());
                    }
                    Err(e) => {
                        // Non-fatal (holiday / no data): record and keep going.
                        eprintln!("[tagdash] flat_files: {day} ignoré ({e})");
                        shared.status.write().unwrap().error = Some(format!("{day}: {e}"));
                    }
                }
            }
            d += chrono::Duration::days(1);
        }
        let mut st = shared.status.write().unwrap();
        st.running = false;
        st.state = "done".into();
        st.current_day = None;
    });

    Ok(())
}

/// Fetch + persist one ET day. Mirrors the Alpaca path of `replay::data::load_day`
/// (universe → daily window → liquid active set → 1-minute bars → news) and writes
/// it atomically to `flat-<day>.db`. Returns the number of minute bars stored.
async fn write_day(
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

    // Bulk download targets the liquid set (no focus symbols here).
    let active = data::rank_active(&day_volume, &[]);
    if active.is_empty() {
        return Err(format!("aucune donnée de marché pour le {day} (jour non ouvré ?)"));
    }

    // 3. Minute bars 04:00→20:00 ET (premarket + regular + after-hours).
    let min_start = pm_start.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let min_end = day_end.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let minutes = data::fetch_bars_window(
        key, secret, &active, "1Min", &min_start, &min_end, &|f| progress(0.18 + f * 0.72),
    )
    .await?;

    // 4. News of the day (published 00:00 ET → 20:00 ET).
    let news_start = crate::time::et_clock_utc(noon, 0, 0).format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let news = data::fetch_news_window(key, secret, &news_start, &min_end)
        .await
        .unwrap_or_else(|e| {
            eprintln!("[tagdash] flat_files: {day} news ignorées ({e})");
            Vec::new()
        });
    progress(0.95);

    // 5. Write everything to a temp DB, then atomically rename so an interrupted
    //    download never leaves a file that reads as complete.
    let tmp = flat_tmp_path(app_dir, day);
    let _ = std::fs::remove_file(&tmp);
    let final_path = flat_path(app_dir, day);

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
                        sym,
                        t.timestamp_millis(),
                        b.o, b.h, b.l, b.c,
                        b.v,
                        b.n.unwrap_or(0),
                        b.vw,
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
                        h.id,
                        h.headline,
                        h.summary,
                        h.url,
                        h.source,
                        syms_json,
                        h.created_at.timestamp_millis(),
                    ])
                    .map_err(|e| e.to_string())?;
            }
        }
        // Manifest last, with complete=1, so the file only reads as complete once
        // everything above committed.
        set_meta(&tx, "schema_version", SCHEMA_VERSION).map_err(|e| e.to_string())?;
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
