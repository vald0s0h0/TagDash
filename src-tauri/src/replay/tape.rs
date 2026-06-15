// Trade tape — fine-granularity recording of the live WebSocket feed.
//
// Alpaca's REST history only goes down to 1-minute bars, so the raw premarket
// trade prints (which the Micro Pullback engine consumes as 10-second candles)
// cannot be re-downloaded later. This module records every trade (and the focus
// symbols' quotes) streamed by the live feed into one small SQLite file per ET
// trading day (`<app_dir>/tape/tape-YYYY-MM-DD.db`). A day recorded this way can
// then be replayed with full tick granularity; days without a tape fall back to
// 1-minute bars (see `replay::data`).
//
// Design constraints:
//   • zero work on the stream's hot path beyond an unbounded channel send;
//   • writes batched in one transaction every flush interval;
//   • recording is disabled while a replay is active (the live feed is stopped
//     then anyway — this is just a belt-and-braces guard against loops).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use chrono::{DateTime, Utc};
use rusqlite::Connection;
use tokio::sync::mpsc;

/// Flush the pending batch to SQLite at most this often.
const FLUSH_MS: u64 = 750;

enum Rec {
    Trade { ts_ms: i64, symbol: String, price: f64, size: u64 },
    Quote { ts_ms: i64, symbol: String, bid: f64, ask: f64 },
}

static TX: OnceLock<mpsc::UnboundedSender<Rec>> = OnceLock::new();

/// Directory holding the per-day tape files.
pub fn tape_dir(app_dir: &Path) -> PathBuf {
    app_dir.join("tape")
}

/// Path of the tape for one ET day (YYYY-MM-DD).
pub fn tape_path(app_dir: &Path, day: &str) -> PathBuf {
    tape_dir(app_dir).join(format!("tape-{day}.db"))
}

/// True when a tape file exists for `day` and holds at least one trade.
pub fn has_tape(app_dir: &Path, day: &str) -> bool {
    let path = tape_path(app_dir, day);
    if !path.exists() {
        return false;
    }
    Connection::open(&path)
        .and_then(|c| c.query_row("SELECT COUNT(*) FROM trades", [], |r| r.get::<_, i64>(0)))
        .map(|n| n > 0)
        .unwrap_or(false)
}

/// Start the background tape writer. Called once from the Tauri setup hook.
pub fn init(app_dir: PathBuf) {
    let (tx, mut rx) = mpsc::unbounded_channel::<Rec>();
    if TX.set(tx).is_err() {
        return; // already initialised
    }
    tauri::async_runtime::spawn(async move {
        let _ = std::fs::create_dir_all(tape_dir(&app_dir));
        // Current open file: (ET day, connection).
        let mut current: Option<(String, Connection)> = None;
        let mut batch: Vec<Rec> = Vec::new();
        let mut ticker = tokio::time::interval(tokio::time::Duration::from_millis(FLUSH_MS));
        loop {
            tokio::select! {
                rec = rx.recv() => {
                    match rec {
                        Some(r) => batch.push(r),
                        None => break, // app shutdown
                    }
                }
                _ = ticker.tick() => {
                    if !batch.is_empty() {
                        flush(&app_dir, &mut current, &mut batch);
                    }
                }
            }
        }
        if !batch.is_empty() {
            flush(&app_dir, &mut current, &mut batch);
        }
    });
}

/// Write the pending batch in one transaction, rolling the file at the ET day
/// boundary (each record lands in the file of ITS OWN event day).
fn flush(app_dir: &Path, current: &mut Option<(String, Connection)>, batch: &mut Vec<Rec>) {
    for rec in batch.drain(..) {
        let ts_ms = match &rec {
            Rec::Trade { ts_ms, .. } | Rec::Quote { ts_ms, .. } => *ts_ms,
        };
        let day = crate::time::et_date(
            chrono::TimeZone::timestamp_millis_opt(&Utc, ts_ms).single().unwrap_or_else(Utc::now),
        );
        // Roll the file when the day changes.
        let roll = current.as_ref().map(|(d, _)| d != &day).unwrap_or(true);
        if roll {
            if let Some((_, conn)) = current.take() {
                let _ = conn.execute_batch("COMMIT");
            }
            match open_day(app_dir, &day) {
                Ok(conn) => {
                    let _ = conn.execute_batch("BEGIN");
                    *current = Some((day.clone(), conn));
                }
                Err(e) => {
                    eprintln!("[tagdash] tape: cannot open {day}: {e}");
                    continue;
                }
            }
        }
        if let Some((_, conn)) = current.as_ref() {
            let res = match &rec {
                Rec::Trade { ts_ms, symbol, price, size } => conn.execute(
                    "INSERT INTO trades (ts, symbol, price, size) VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params![ts_ms, symbol, price, *size as i64],
                ),
                Rec::Quote { ts_ms, symbol, bid, ask } => conn.execute(
                    "INSERT INTO quotes (ts, symbol, bid, ask) VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params![ts_ms, symbol, bid, ask],
                ),
            };
            if let Err(e) = res {
                eprintln!("[tagdash] tape: insert failed: {e}");
            }
        }
    }
    // Commit the batch and immediately reopen a transaction for the next one.
    if let Some((_, conn)) = current.as_ref() {
        let _ = conn.execute_batch("COMMIT; BEGIN");
    }
}

fn open_day(app_dir: &Path, day: &str) -> rusqlite::Result<Connection> {
    let conn = Connection::open(tape_path(app_dir, day))?;
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         CREATE TABLE IF NOT EXISTS trades (
             ts     INTEGER NOT NULL,
             symbol TEXT NOT NULL,
             price  REAL NOT NULL,
             size   INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_trades_ts ON trades(ts);
         CREATE TABLE IF NOT EXISTS quotes (
             ts     INTEGER NOT NULL,
             symbol TEXT NOT NULL,
             bid    REAL NOT NULL,
             ask    REAL NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_quotes_ts ON quotes(ts);",
    )?;
    Ok(conn)
}

/// Record one live trade print (no-op before init / during replay).
pub fn record_trade(symbol: &str, price: f64, size: u64, event_time: DateTime<Utc>) {
    if super::clock::is_active() {
        return;
    }
    if let Some(tx) = TX.get() {
        let _ = tx.send(Rec::Trade {
            ts_ms: event_time.timestamp_millis(),
            symbol: symbol.to_string(),
            price,
            size,
        });
    }
}

/// Record one live quote (focus symbols only flow live; no-op during replay).
pub fn record_quote(symbol: &str, bid: f64, ask: f64, event_time: DateTime<Utc>) {
    if super::clock::is_active() {
        return;
    }
    if let Some(tx) = TX.get() {
        let _ = tx.send(Rec::Quote {
            ts_ms: event_time.timestamp_millis(),
            symbol: symbol.to_string(),
            bid,
            ask,
        });
    }
}

/// Read a whole day's taped trades, time-ascending: (ts_ms, symbol, price, size).
pub fn read_trades(app_dir: &Path, day: &str) -> Vec<(i64, String, f64, u64)> {
    let Ok(conn) = Connection::open(tape_path(app_dir, day)) else { return Vec::new() };
    let Ok(mut stmt) = conn.prepare("SELECT ts, symbol, price, size FROM trades ORDER BY ts ASC")
    else {
        return Vec::new();
    };
    stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, f64>(2)?,
            r.get::<_, i64>(3)?.max(0) as u64,
        ))
    })
    .map(|it| it.filter_map(|r| r.ok()).collect())
    .unwrap_or_default()
}

/// Read a whole day's taped quotes, time-ascending: (ts_ms, symbol, bid, ask).
pub fn read_quotes(app_dir: &Path, day: &str) -> Vec<(i64, String, f64, f64)> {
    let Ok(conn) = Connection::open(tape_path(app_dir, day)) else { return Vec::new() };
    let Ok(mut stmt) = conn.prepare("SELECT ts, symbol, bid, ask FROM quotes ORDER BY ts ASC")
    else {
        return Vec::new();
    };
    stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, f64>(2)?,
            r.get::<_, f64>(3)?,
        ))
    })
    .map(|it| it.filter_map(|r| r.ok()).collect())
    .unwrap_or_default()
}
