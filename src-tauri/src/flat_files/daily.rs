// Daily flat file — a single cumulative `daily/daily.db`.
//
// Holds daily OHLCV bars for the WHOLE US universe, appended over time. The table is
// byte-identical to `local_db`'s `daily_cache` (symbol, date, OHLC, volume) so the
// format is "the same as the database, as simple as possible". Unlike the per-day
// minute/trade snapshots this is a long-lived store: re-downloads upsert by
// (symbol, date), so overlapping ranges never duplicate.

use std::path::Path;
use std::sync::{Arc, Mutex};

use chrono::NaiveDate;
use rusqlite::Connection;

use super::{daily_path, FlatFileDay, FlatFilesShared};
use crate::replay::data;

fn open_db(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    // Same columns as local_db::daily_cache — identical format.
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         CREATE TABLE IF NOT EXISTS daily_cache (
             symbol     TEXT NOT NULL,
             date       TEXT NOT NULL,
             open       REAL,
             high       REAL,
             low        REAL,
             close      REAL,
             volume     INTEGER,
             updated_at TEXT NOT NULL DEFAULT (datetime('now')),
             PRIMARY KEY (symbol, date)
         );",
    )?;
    Ok(conn)
}

/// Distinct dates covered by the cumulative file (for the calendar).
pub fn calendar(app_dir: &Path) -> Vec<FlatFileDay> {
    let path = daily_path(app_dir);
    if !path.exists() {
        return Vec::new();
    }
    let Ok(conn) = open_db(&path) else { return Vec::new() };
    let mut out: Vec<FlatFileDay> = Vec::new();
    let Ok(mut stmt) =
        conn.prepare("SELECT date, COUNT(*) FROM daily_cache GROUP BY date ORDER BY date")
    else {
        return out;
    };
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)));
    if let Ok(rows) = rows {
        for row in rows.flatten() {
            out.push(FlatFileDay {
                day: row.0,
                bytes: 0,
                symbol_count: row.1,
                bar_count: row.1,
                complete: true,
            });
        }
    }
    out
}

/// Download daily bars of the whole universe over [start, end] and upsert them into
/// the cumulative `daily.db`.
pub async fn run(
    shared: &Arc<FlatFilesShared>,
    app_dir: &Path,
    db: &Arc<Mutex<Connection>>,
    key: &str,
    secret: &str,
    start: NaiveDate,
    end: NaiveDate,
) -> Result<(), String> {
    {
        let mut st = shared.status.write().unwrap();
        st.current_day = Some("univers complet".into());
        st.day_index = 1;
        st.progress = 0.0;
    }

    let universe: Vec<String> = {
        let conn = db.lock().unwrap();
        crate::local_db::universe_repository::get_active_symbols(&conn).map_err(|e| e.to_string())?
    };
    if universe.is_empty() {
        return Err("univers vide — lance d'abord le Startup Pipeline".into());
    }

    let start_s = start.format("%Y-%m-%dT00:00:00Z").to_string();
    let end_s = end.format("%Y-%m-%dT23:59:59Z").to_string();
    let bars = data::fetch_bars_window(
        key, secret, &universe, "1Day", &start_s, &end_s, &|f| shared.set_progress(f * 0.9),
    )
    .await?;
    if shared.cancelled() {
        shared.status.write().unwrap().state = "cancelled".into();
        return Ok(());
    }

    let path = daily_path(app_dir);
    let mut conn = open_db(&path).map_err(|e| e.to_string())?;
    let n_dates: i64;
    {
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        {
            let mut ins = tx
                .prepare(
                    "INSERT INTO daily_cache (symbol, date, open, high, low, close, volume, updated_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7, datetime('now'))
                     ON CONFLICT(symbol, date) DO UPDATE SET
                         open=excluded.open, high=excluded.high, low=excluded.low,
                         close=excluded.close, volume=excluded.volume, updated_at=datetime('now')",
                )
                .map_err(|e| e.to_string())?;
            for (sym, raw_bars) in &bars {
                for b in raw_bars {
                    let date = b.t.get(..10).unwrap_or("");
                    if date.is_empty() {
                        continue;
                    }
                    ins.execute(rusqlite::params![sym, date, b.o, b.h, b.l, b.c, b.v])
                        .map_err(|e| e.to_string())?;
                }
            }
        }
        tx.commit().map_err(|e| e.to_string())?;
        n_dates = conn
            .query_row("SELECT COUNT(DISTINCT date) FROM daily_cache", [], |r| r.get(0))
            .unwrap_or(0);
    }
    shared.set_progress(1.0);
    {
        let mut st = shared.status.write().unwrap();
        st.last_done = Some(format!("{n_dates} jours"));
    }
    Ok(())
}
