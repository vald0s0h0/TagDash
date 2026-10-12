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
use crate::market_state::aggregators::{Bar, Timeframe};
use crate::replay::data;

/// How many previous trading days to guarantee alongside a downloaded day (chart
/// intraday lead-in).
const HISTORY_DAYS: usize = 5;
const RVOL_HISTORY_DAYS: usize = 20;
const RVOL_BUCKET_MINUTES: u32 = 10;
const RVOL_SESSION_START: u32 = 4 * 60;
const RVOL_SESSION_END: u32 = 20 * 60;

#[derive(Debug, Clone, Copy)]
pub struct RvolBaseline {
    pub median_volume_10m: f64,
    pub mad_volume_10m: f64,
    pub sample_days: u8,
}

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

/// The most recent COMPLETE minute day on disk (YYYY-MM-DD), or None when none has
/// been downloaded. Used to pick the day to park in Market Replay on a flat-files boot.
pub fn latest_complete_day(app_dir: &Path) -> Option<String> {
    calendar(app_dir)
        .into_iter()
        .filter(|d| d.complete)
        .map(|d| d.day)
        .max()
}

pub fn calendar(app_dir: &Path) -> Vec<FlatFileDay> {
    let dir = kind_dir(app_dir, Kind::Minute);
    let mut out: Vec<FlatFileDay> = Vec::new();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(day) = day_of_file(&name, "minute-") else {
            continue;
        };
        let bytes = entry.metadata().map(|m| m.len()).unwrap_or(0);
        let (symbol_count, bar_count, complete) = Connection::open(entry.path())
            .ok()
            .map(|c| {
                let sc = get_meta(&c, "symbol_count")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                let bc = get_meta(&c, "bar_count")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                let complete = get_meta(&c, "complete").map(|v| v == "1").unwrap_or(false);
                (sc, bc, complete)
            })
            .unwrap_or((0, 0, false));
        out.push(FlatFileDay {
            day,
            bytes,
            symbol_count,
            bar_count,
            complete,
        });
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
    let bars = write_one_day(app_dir, db, key, secret, day, &|f| {
        shared.set_progress(f * main_span)
    })
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
            if let Err(e) = write_one_day(app_dir, db, key, secret, pday, &|f| {
                shared.set_progress(lo + f * (hi - lo))
            })
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
    let nd =
        NaiveDate::parse_from_str(day, "%Y-%m-%d").map_err(|_| format!("invalid date: {day}"))?;
    let noon = data::noon_utc(nd);
    let pm_start = crate::time::et_clock_utc(noon, 4, 0); // 04:00 ET
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

    // 2. Daily window: previous close + this-day volume (activity filter).
    let daily_start = (nd - chrono::Duration::days(10))
        .format("%Y-%m-%dT00:00:00Z")
        .to_string();
    let daily_end = day_end.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let daily = data::fetch_bars_window(
        key,
        secret,
        &universe,
        "1Day",
        &daily_start,
        &daily_end,
        &|f| progress(f * 0.18),
    )
    .await?;
    let (prev_closes, day_volume) = data::split_daily(&daily, day);

    // The day's 2000 most-traded symbols (no focus symbols here).
    let active = data::rank_active(&day_volume, &[]);
    if active.is_empty() {
        return Err(format!(
            "aucune donnée de marché pour le {day} (jour non ouvré ?)"
        ));
    }

    // 3. Minute bars 04:00→20:00 ET.
    let min_start = pm_start.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let min_end = day_end.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let minutes =
        data::fetch_bars_window(key, secret, &active, "1Min", &min_start, &min_end, &|f| {
            progress(0.18 + f * 0.72)
        })
        .await?;

    // 4. News of the day (published 00:00 ET → 20:00 ET).
    let news_start = crate::time::et_clock_utc(noon, 0, 0)
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    let news = data::fetch_news_window(key, secret, &news_start, &min_end, &[])
        .await
        .unwrap_or_else(|e| {
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
                    let Ok(t) = b.t.parse::<chrono::DateTime<Utc>>() else {
                        continue;
                    };
                    ins.execute(rusqlite::params![
                        sym,
                        t.timestamp_millis(),
                        b.o,
                        b.h,
                        b.l,
                        b.c,
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
                ins_pc
                    .execute(rusqlite::params![sym, close])
                    .map_err(|e| e.to_string())?;
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
    // Any newly downloaded historical day can improve a previously cached
    // point-in-time RVOL profile. Invalidate the derived cache; it is rebuilt once
    // by the startup/replay pipeline, never in the hot scanner loop.
    if let Ok(conn) = db.lock() {
        let _ = conn.execute("DELETE FROM micro_rvol_baselines", []);
    }
    progress(1.0);
    Ok(bar_count)
}

/// The `n` previous weekday dates before `day` (most recent first). Holidays are not
/// excluded here — they simply fail to download and are skipped by the caller.
fn prior_trading_days(day: &str, n: usize) -> Vec<String> {
    let Ok(mut d) = NaiveDate::parse_from_str(day, "%Y-%m-%d") else {
        return Vec::new();
    };
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

/// Build (if absent) and return a point-in-time, robust time-of-day volume profile
/// for `target_day`. Only complete minute files strictly older than the target are
/// read, so replaying an old session cannot see future volume.
///
/// The provider files contain only bars that traded. For every symbol present in a
/// day we therefore explicitly retain missing 10-minute buckets as zero: dormancy is
/// information and dropping it would make a 04:00 wake-up look ordinary.
pub fn ensure_rvol_baseline(
    app_dir: &Path,
    db: &Arc<Mutex<Connection>>,
    target_day: &str,
) -> Result<HashMap<(String, u32), RvolBaseline>, String> {
    {
        let conn = db.lock().unwrap();
        let existing = conn
            .query_row(
                "SELECT COUNT(*) FROM micro_rvol_baselines WHERE target_day=?1",
                [target_day],
                |r| r.get::<_, i64>(0),
            )
            .unwrap_or(0);
        if existing > 0 {
            return read_rvol_baseline(&conn, target_day);
        }
    }

    let eligible: HashSet<String> = {
        let conn = db.lock().unwrap();
        crate::local_db::universe_repository::get_all(&conn)
            .unwrap_or_default()
            .into_iter()
            .filter(|a| a.tradable)
            .filter(|a| a.float_shares.is_none_or(|f| f > 0 && f <= 100_000_000))
            .map(|a| a.symbol)
            .collect()
    };

    let mut days = calendar(app_dir)
        .into_iter()
        .filter(|d| d.complete && d.day.as_str() < target_day)
        .map(|d| d.day)
        .collect::<Vec<_>>();
    days.sort();
    days.reverse();
    days.truncate(RVOL_HISTORY_DAYS);
    if days.is_empty() {
        return Ok(HashMap::new());
    }

    // Observed daily 10m totals and number of source days in which each symbol was
    // actually part of the top-2 000 file. Zero-fill happens after all files load.
    let mut samples: HashMap<(String, u32), Vec<f64>> = HashMap::new();
    let mut symbol_days: HashMap<String, usize> = HashMap::new();
    for day in &days {
        let conn = Connection::open(minute_path(app_dir, day)).map_err(|e| e.to_string())?;
        let mut daily: HashMap<(String, u32), f64> = HashMap::new();
        let mut present: HashSet<String> = HashSet::new();
        let mut stmt = conn
            .prepare("SELECT symbol,t_ms,v FROM minute_bars")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        for row in rows.flatten() {
            let (symbol, t_ms, volume) = row;
            if !eligible.contains(&symbol) {
                continue;
            }
            let Some(ts) = chrono::TimeZone::timestamp_millis_opt(&Utc, t_ms).single() else {
                continue;
            };
            let minute = crate::time::et_minutes(ts);
            if !(RVOL_SESSION_START..RVOL_SESSION_END).contains(&minute) {
                continue;
            }
            let bucket = (minute / RVOL_BUCKET_MINUTES) * RVOL_BUCKET_MINUTES;
            *daily.entry((symbol.clone(), bucket)).or_default() += volume.max(0) as f64;
            present.insert(symbol);
        }
        for symbol in present {
            *symbol_days.entry(symbol).or_default() += 1;
        }
        for (key, volume) in daily {
            samples.entry(key).or_default().push(volume);
        }
    }

    let mut built: HashMap<(String, u32), RvolBaseline> = HashMap::new();
    for (symbol, n_days) in symbol_days {
        for bucket in (RVOL_SESSION_START..RVOL_SESSION_END).step_by(RVOL_BUCKET_MINUTES as usize) {
            let key = (symbol.clone(), bucket);
            let mut values = samples.remove(&key).unwrap_or_default();
            values.resize(n_days, 0.0);
            let median = robust_median(&mut values).unwrap_or(0.0);
            let mut deviations = values
                .iter()
                .map(|v| (v - median).abs())
                .collect::<Vec<_>>();
            let mad = robust_median(&mut deviations).unwrap_or(0.0);
            built.insert(
                key,
                RvolBaseline {
                    median_volume_10m: median,
                    mad_volume_10m: mad,
                    sample_days: n_days.min(u8::MAX as usize) as u8,
                },
            );
        }
    }

    {
        let mut conn = db.lock().unwrap();
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        tx.execute(
            "DELETE FROM micro_rvol_baselines WHERE target_day=?1",
            [target_day],
        )
        .map_err(|e| e.to_string())?;
        {
            let mut insert = tx
                .prepare(
                    "INSERT INTO micro_rvol_baselines
                     (target_day,symbol,et_bucket_minute,median_volume_10m,mad_volume_10m,sample_days)
                     VALUES (?1,?2,?3,?4,?5,?6)",
                )
                .map_err(|e| e.to_string())?;
            for ((symbol, bucket), baseline) in &built {
                insert
                    .execute(rusqlite::params![
                        target_day,
                        symbol,
                        *bucket as i64,
                        baseline.median_volume_10m,
                        baseline.mad_volume_10m,
                        baseline.sample_days as i64,
                    ])
                    .map_err(|e| e.to_string())?;
            }
        }
        tx.commit().map_err(|e| e.to_string())?;
    }
    Ok(built)
}

pub fn read_rvol_baseline(
    conn: &Connection,
    target_day: &str,
) -> Result<HashMap<(String, u32), RvolBaseline>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT symbol,et_bucket_minute,median_volume_10m,mad_volume_10m,sample_days
             FROM micro_rvol_baselines WHERE target_day=?1",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([target_day], |r| {
            Ok((
                (r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u32),
                RvolBaseline {
                    median_volume_10m: r.get(2)?,
                    mad_volume_10m: r.get(3)?,
                    sample_days: r.get::<_, i64>(4)?.clamp(0, u8::MAX as i64) as u8,
                },
            ))
        })
        .map_err(|e| e.to_string())?;
    Ok(rows.flatten().collect())
}

fn robust_median(values: &mut [f64]) -> Option<f64> {
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

// ─── Reader (offline chart source: per-symbol OHLC bars) ───────────────────────

/// Read `symbol`'s intraday OHLC bars from the minute flat files, aggregated to
/// `tf` (M1/M2/M5/M15), oldest→newest. This is the offline chart source: it serves
/// `load_chart_bars` / `load_older_bars` straight from disk when there is no live
/// feed. Returns at most `limit` bars; with `before_ms` set, only bars strictly
/// before that instant (lazy back-fill of older history). Days / symbols absent
/// from the files simply don't contribute — the chart renders the gap.
///
/// Daily and sub-minute (5s/10s) timeframes are NOT handled here: daily comes from
/// `daily_cache`, and sub-minute bars can't be rebuilt from 1-minute data.
pub fn read_symbol_bars(
    app_dir: &Path,
    symbol: &str,
    tf: Timeframe,
    limit: usize,
    before_ms: Option<i64>,
) -> Vec<Bar> {
    let tf_minutes = (tf.seconds() / 60).max(1) as usize;
    if limit == 0 {
        return Vec::new();
    }
    // Raw 1-minute bars needed to build `limit` aggregated bars (+ one tf of slack
    // for partial buckets / gaps). Bounds how many day files we open.
    let need_minutes = limit.saturating_mul(tf_minutes).saturating_add(tf_minutes);
    const MAX_FILES: usize = 40;

    // ET date upper bound when back-filling older history: skip whole day files we
    // know are entirely at/after the cursor before even opening them.
    let before_date: Option<String> = before_ms.and_then(|ms| {
        chrono::TimeZone::timestamp_millis_opt(&Utc, ms)
            .single()
            .map(crate::time::et_date)
    });

    // Complete day files, newest first.
    let mut days: Vec<String> = calendar(app_dir)
        .into_iter()
        .filter(|d| d.complete)
        .map(|d| d.day)
        .collect();
    days.sort();
    days.reverse();

    type RawMin = (i64, f64, f64, f64, f64, i64, i64, Option<f64>);
    let mut raw: Vec<RawMin> = Vec::new();
    let mut files_read = 0usize;
    for day in days {
        if let Some(bd) = &before_date {
            if day.as_str() > bd.as_str() {
                continue; // day entirely after the back-fill cursor
            }
        }
        let path = minute_path(app_dir, &day);
        let Ok(conn) = Connection::open(&path) else {
            continue;
        };
        let Ok(mut stmt) =
            conn.prepare("SELECT t_ms,o,h,l,c,v,n,vw FROM minute_bars WHERE symbol=?1")
        else {
            continue;
        };
        let rows = stmt.query_map([symbol], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, f64>(1)?,
                r.get::<_, f64>(2)?,
                r.get::<_, f64>(3)?,
                r.get::<_, f64>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, i64>(6)?,
                r.get::<_, Option<f64>>(7)?,
            ))
        });
        if let Ok(rows) = rows {
            for row in rows.flatten() {
                if let Some(b) = before_ms {
                    if row.0 >= b {
                        continue;
                    }
                }
                raw.push(row);
            }
        }
        files_read += 1;
        if raw.len() >= need_minutes || files_read >= MAX_FILES {
            break;
        }
    }
    if raw.is_empty() {
        return Vec::new();
    }
    raw.sort_by_key(|b| b.0);

    // Aggregate 1-minute bars into `tf` buckets (clock-aligned via epoch floor).
    let bucket_ms = tf.seconds() * 1000;
    let mut out: Vec<Bar> = Vec::new();
    let mut vwnum: Vec<f64> = Vec::new(); // volume-weighted price numerator per bar
    for (t_ms, o, h, l, c, v, n, vw) in raw {
        let bucket = (t_ms / bucket_ms) * bucket_ms;
        let vol = v.max(0) as u64;
        let contrib = vw.map(|x| x * vol as f64).unwrap_or(0.0);
        match out.last_mut() {
            Some(last) if last.time.timestamp_millis() == bucket => {
                last.high = last.high.max(h);
                last.low = last.low.min(l);
                last.close = c;
                last.volume += vol;
                last.trade_count = Some(last.trade_count.unwrap_or(0) + n.max(0) as u64);
                if let Some(num) = vwnum.last_mut() {
                    *num += contrib;
                }
            }
            _ => {
                let Some(time) = chrono::TimeZone::timestamp_millis_opt(&Utc, bucket).single()
                else {
                    continue;
                };
                out.push(Bar {
                    time,
                    open: o,
                    high: h,
                    low: l,
                    close: c,
                    volume: vol,
                    vwap: None,
                    trade_count: Some(n.max(0) as u64),
                });
                vwnum.push(contrib);
            }
        }
    }
    for (b, num) in out.iter_mut().zip(vwnum) {
        if b.volume > 0 && num > 0.0 {
            b.vwap = Some(num / b.volume as f64);
        }
    }

    if out.len() > limit {
        out.drain(0..out.len() - limit);
    }
    out
}
