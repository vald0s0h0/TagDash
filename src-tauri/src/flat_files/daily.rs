// Daily flat file — a single cumulative `daily/daily.db`.
//
// Holds daily OHLCV bars for the WHOLE US universe, appended over time. The table is
// byte-identical to `local_db`'s `daily_cache` (symbol, date, OHLC, volume) so the
// format is "the same as the database, as simple as possible". Unlike the per-day
// minute/trade snapshots this is a long-lived store: re-downloads upsert by
// (symbol, date), so overlapping ranges never duplicate.

use std::path::Path;
use std::sync::{Arc, Mutex};

use chrono::{NaiveDate, Utc};
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
    let Ok(conn) = open_db(&path) else {
        return Vec::new();
    };
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
        crate::local_db::universe_repository::get_active_symbols(&conn)
            .map_err(|e| e.to_string())?
    };
    if universe.is_empty() {
        return Err("univers vide — lance d'abord le Startup Pipeline".into());
    }

    let start_s = start.format("%Y-%m-%dT00:00:00Z").to_string();
    let end_s = end.format("%Y-%m-%dT23:59:59Z").to_string();
    let bars = data::fetch_bars_window(key, secret, &universe, "1Day", &start_s, &end_s, &|f| {
        shared.set_progress(f * 0.9)
    })
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
            .query_row("SELECT COUNT(DISTINCT date) FROM daily_cache", [], |r| {
                r.get(0)
            })
            .unwrap_or(0);
    }
    shared.set_progress(1.0);
    {
        let mut st = shared.status.write().unwrap();
        st.last_done = Some(format!("{n_dates} jours"));
    }
    Ok(())
}

// ─── Reading back into the app DB (offline daily source) ───────────────────────

/// Symbols whose flat rows are at a STALE split scale and must not be imported.
///
/// Alpaca's `adjustment=split` rescales a symbol's entire series to the factor in
/// force at the moment of the fetch. So rows downloaded into the cumulative file
/// before a split sit at a different scale than anything the API returns after it.
/// Mixing the two for one symbol splices a phantom gap of exactly the split ratio
/// into `daily_cache` — and a phantom −90 % on a 1:10 is read by the panic score as
/// a once-in-a-decade shock, putting a fabricated candidate at the top of the
/// watchlist with nothing to warn you.
///
/// The test is per symbol: an ex-date at or after the day that symbol's flat rows
/// were written means those rows predate the rescale. Such symbols are skipped
/// entirely here and supplied by the daily step instead, at one current scale.
/// Ties go to skipping — a same-day split is ambiguous and the API copy is free.
///
/// Empty (import everything) when splits were never fetched, which is the state of
/// a first launch. That case is covered anyway: an empty cache forces the daily
/// step's full backfill, which rewrites the whole window at one scale.
fn stale_scale_symbols(src: &Connection, main: &Connection) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    let Ok(mut stmt) =
        main.prepare("SELECT symbol, MAX(ex_date) FROM ticker_splits GROUP BY symbol")
    else {
        return out;
    };
    let Ok(rows) = stmt.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
    }) else {
        return out;
    };
    for (symbol, ex_date) in rows.flatten() {
        let Some(ex_date) = ex_date else { continue };
        let written: Option<String> = src
            .query_row(
                "SELECT MAX(updated_at) FROM daily_cache WHERE symbol = ?1",
                [&symbol],
                |r| r.get(0),
            )
            .ok()
            .flatten();
        let Some(written) = written else { continue };
        if ex_date.as_str() >= written.get(..10).unwrap_or("") {
            out.insert(symbol);
        }
    }
    out
}

/// Copy every daily bar from the cumulative flat file into the app's `daily_cache`
/// (idempotent upsert by symbol+date). This is the offline daily source: once
/// loaded, every `daily_cache` consumer — mean-reversion scoring, PDC/PDH levels,
/// the daily chart pane — works with no network. Returns the number of bars
/// copied; a no-op `Ok(0)` when the flat file is absent.
///
/// `main` is an already-locked handle to the app database (the startup pipeline
/// locks the guard once and calls in), so the whole copy runs under a single
/// transaction without re-locking.
pub fn load_into_cache(app_dir: &Path, main: &Connection) -> Result<usize, String> {
    let path = daily_path(app_dir);
    if !path.exists() {
        return Ok(0);
    }
    let src = open_db(&path).map_err(|e| e.to_string())?;
    let stale = stale_scale_symbols(&src, main);
    let mut stmt = src
        .prepare("SELECT symbol,date,open,high,low,close,volume FROM daily_cache")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<f64>>(2)?,
                r.get::<_, Option<f64>>(3)?,
                r.get::<_, Option<f64>>(4)?,
                r.get::<_, Option<f64>>(5)?,
                r.get::<_, Option<i64>>(6)?,
            ))
        })
        .map_err(|e| e.to_string())?;

    let now = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let mut n = 0usize;
    // RAII transaction on a shared `&Connection` (the guard derefs to one): rolls
    // back automatically if any read/insert fails partway, so we never leave the
    // app DB inside a dangling transaction.
    let tx = main.unchecked_transaction().map_err(|e| e.to_string())?;
    {
        let mut ins = tx
            .prepare(
                "INSERT INTO daily_cache (symbol,date,open,high,low,close,volume,updated_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
                 ON CONFLICT(symbol,date) DO UPDATE SET
                     open=excluded.open, high=excluded.high, low=excluded.low,
                     close=excluded.close, volume=excluded.volume,
                     updated_at=excluded.updated_at",
            )
            .map_err(|e| e.to_string())?;
        for row in rows {
            let (sym, date, o, h, l, c, v) = row.map_err(|e| e.to_string())?;
            if stale.contains(&sym) {
                continue; // split since these rows were written — see `stale_scale_symbols`
            }
            ins.execute(rusqlite::params![sym, date, o, h, l, c, v, now])
                .map_err(|e| e.to_string())?;
            n += 1;
        }
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(n)
}

/// Distinct symbols present in the cumulative daily flat file. Used to seed
/// `universe_assets` when the app boots offline with no Alpaca-built universe.
/// Empty when the file is absent.
pub fn symbols(app_dir: &Path) -> Vec<String> {
    let path = daily_path(app_dir);
    if !path.exists() {
        return Vec::new();
    }
    let Ok(conn) = open_db(&path) else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    let Ok(mut stmt) = conn.prepare("SELECT DISTINCT symbol FROM daily_cache") else {
        return out;
    };
    if let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) {
        for s in rows.flatten() {
            out.push(s);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A flat file holding one symbol's rows, stamped `written` (RFC3339).
    fn flat_with(symbol: &str, written: &str) -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE daily_cache (
                 symbol TEXT NOT NULL, date TEXT NOT NULL,
                 open REAL, high REAL, low REAL, close REAL, volume INTEGER,
                 updated_at TEXT NOT NULL, PRIMARY KEY (symbol, date));",
        )
        .unwrap();
        c.execute(
            "INSERT INTO daily_cache VALUES (?1,'2026-01-05',1.0,1.0,1.0,1.0,100,?2)",
            rusqlite::params![symbol, written],
        )
        .unwrap();
        c
    }

    /// An app DB whose `ticker_splits` holds one ex-date for `symbol`.
    fn main_with_split(symbol: Option<(&str, &str)>) -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE ticker_splits (symbol TEXT NOT NULL, ex_date TEXT NOT NULL);",
        )
        .unwrap();
        if let Some((sym, ex_date)) = symbol {
            c.execute(
                "INSERT INTO ticker_splits VALUES (?1,?2)",
                rusqlite::params![sym, ex_date],
            )
            .unwrap();
        }
        c
    }

    #[test]
    fn rows_written_before_a_split_are_refused() {
        // Downloaded in January, split in March → the stored prices are pre-split.
        let src = flat_with("SPLT", "2026-01-06T12:00:00Z");
        let main = main_with_split(Some(("SPLT", "2026-03-10")));
        assert!(stale_scale_symbols(&src, &main).contains("SPLT"));
    }

    #[test]
    fn rows_written_after_a_split_are_kept() {
        // Re-downloaded in April, after the March split → already at the new scale.
        let src = flat_with("SPLT", "2026-04-01T12:00:00Z");
        let main = main_with_split(Some(("SPLT", "2026-03-10")));
        assert!(stale_scale_symbols(&src, &main).is_empty());
    }

    #[test]
    fn a_same_day_split_is_refused_because_the_order_is_unknowable() {
        let src = flat_with("SPLT", "2026-03-10T12:00:00Z");
        let main = main_with_split(Some(("SPLT", "2026-03-10")));
        assert!(
            stale_scale_symbols(&src, &main).contains("SPLT"),
            "ties go to skipping — the API copy is free",
        );
    }

    #[test]
    fn a_symbol_that_never_split_is_kept() {
        let src = flat_with("CALM", "2026-01-06T12:00:00Z");
        let main = main_with_split(Some(("OTHER", "2026-03-10")));
        assert!(stale_scale_symbols(&src, &main).is_empty());
    }

    /// First launch: splits were never fetched. Everything imports — the daily step's
    /// full backfill rewrites the window at one scale anyway.
    #[test]
    fn no_split_data_at_all_imports_everything() {
        let src = flat_with("SPLT", "2026-01-06T12:00:00Z");
        assert!(stale_scale_symbols(&src, &main_with_split(None)).is_empty());
        // Not even the table — the very first boot, before migrations of an old DB.
        let bare = Connection::open_in_memory().unwrap();
        assert!(stale_scale_symbols(&src, &bare).is_empty());
    }
}
