// Session capture — copies the prints and quotes of tickers with an OPEN review
// session into their session file, as the feeds deliver them.
//
// Called from the feeds' hot loops (live Alpaca stream, live Massive stream, Market
// Replay emitter). The only work on that path is a read-locked HashMap lookup; a
// ticker without an open session costs nothing more. Rows travel over an unbounded
// channel to one writer task that batches them per session every FLUSH_MS.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;

use rusqlite::{params, Connection};
use tokio::sync::mpsc;

const FLUSH_MS: u64 = 750;

pub enum Rec {
    Trade {
        ts_ms: i64,
        price: f64,
        size: u64,
        prints: u64,
        exch: Option<String>,
        cond: Option<String>,
    },
    Quote {
        ts_ms: i64,
        bid: f64,
        bid_size: Option<u64>,
        ask: f64,
        ask_size: Option<u64>,
        bid_x: Option<String>,
        ask_x: Option<String>,
    },
}

struct Item {
    session_id: String,
    origin: String,
    rec: Rec,
}

static TX: OnceLock<mpsc::UnboundedSender<Item>> = OnceLock::new();

pub fn init() {
    let (tx, mut rx) = mpsc::unbounded_channel::<Item>();
    if TX.set(tx).is_err() {
        return;
    }
    tauri::async_runtime::spawn(async move {
        let mut batch: Vec<Item> = Vec::new();
        let mut tick = tokio::time::interval(tokio::time::Duration::from_millis(FLUSH_MS));
        loop {
            tokio::select! {
                item = rx.recv() => match item {
                    Some(i) => batch.push(i),
                    None => break,
                },
                _ = tick.tick() => {
                    if !batch.is_empty() {
                        let items = std::mem::take(&mut batch);
                        let _ = tokio::task::spawn_blocking(move || flush(items)).await;
                    }
                }
            }
        }
    });
}

#[inline]
fn session_of(symbol: &str) -> Option<String> {
    super::open_session_for(symbol)
}

fn send(symbol: &str, origin: &str, rec: Rec) {
    let Some(tx) = TX.get() else { return };
    let Some(session_id) = session_of(symbol) else { return };
    let _ = tx.send(Item {
        session_id,
        origin: origin.to_string(),
        rec,
    });
}

/// One print. `prints` > 1 marks an aggregate (see `store`).
#[allow(clippy::too_many_arguments)]
pub fn trade(
    symbol: &str,
    ts_ms: i64,
    price: f64,
    size: u64,
    prints: u64,
    exch: Option<&str>,
    cond: Option<String>,
    origin: &str,
) {
    if TX.get().is_none() {
        return;
    }
    send(
        symbol,
        origin,
        Rec::Trade {
            ts_ms,
            price,
            size,
            prints,
            exch: exch.map(str::to_string),
            cond,
        },
    );
}

#[allow(clippy::too_many_arguments)]
pub fn quote(
    symbol: &str,
    ts_ms: i64,
    bid: f64,
    bid_size: Option<u64>,
    ask: f64,
    ask_size: Option<u64>,
    bid_x: Option<&str>,
    ask_x: Option<&str>,
    origin: &str,
) {
    if TX.get().is_none() {
        return;
    }
    send(
        symbol,
        origin,
        Rec::Quote {
            ts_ms,
            bid,
            bid_size,
            ask,
            ask_size,
            bid_x: bid_x.map(str::to_string),
            ask_x: ask_x.map(str::to_string),
        },
    );
}

fn flush(items: Vec<Item>) {
    let Some(deps) = super::deps() else { return };
    let mut by_session: HashMap<String, Vec<Item>> = HashMap::new();
    for it in items {
        by_session.entry(it.session_id.clone()).or_default().push(it);
    }
    for (session_id, items) in by_session {
        let path = {
            let conn = deps.db.lock().unwrap();
            let Some(s) = super::get_session(&conn, &session_id) else { continue };
            let mut origins: Vec<&str> = items.iter().map(|i| i.origin.as_str()).collect();
            origins.sort_unstable();
            origins.dedup();
            for o in origins {
                super::note_origin(&conn, &session_id, o);
            }
            PathBuf::from(s.store_path)
        };
        let Ok(mut conn) = super::store::open(&path) else {
            eprintln!("[tagdash] review: store {} inaccessible", path.display());
            continue;
        };
        if let Err(e) = write_batch(&mut conn, &items) {
            eprintln!("[tagdash] review: écriture {session_id}: {e}");
        }
        drop(conn);
        // A long session: pack what piled up rather than let the rows grow.
        if super::store::staged_count(&path) > super::store::STAGE_LIMIT {
            if let Err(e) = super::store::compact(&path) {
                eprintln!("[tagdash] review: compactage {session_id}: {e}");
            }
        }
    }
}

fn write_batch(conn: &mut Connection, items: &[Item]) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    {
        let mut ins_t = tx.prepare_cached(
            "INSERT INTO trades (ts, price, size, prints, exch, cond, origin) VALUES (?1,?2,?3,?4,?5,?6,?7)",
        )?;
        let mut ins_q = tx.prepare_cached(
            "INSERT INTO quotes (ts, bid, bid_size, ask, ask_size, bid_x, ask_x, origin) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        )?;
        for it in items {
            match &it.rec {
                Rec::Trade {
                    ts_ms,
                    price,
                    size,
                    prints,
                    exch,
                    cond,
                } => {
                    ins_t.execute(params![
                        ts_ms,
                        price,
                        *size as i64,
                        *prints as i64,
                        exch,
                        cond,
                        it.origin
                    ])?;
                }
                Rec::Quote {
                    ts_ms,
                    bid,
                    bid_size,
                    ask,
                    ask_size,
                    bid_x,
                    ask_x,
                } => {
                    ins_q.execute(params![
                        ts_ms,
                        bid,
                        bid_size.map(|v| v as i64),
                        ask,
                        ask_size.map(|v| v as i64),
                        bid_x,
                        ask_x,
                        it.origin
                    ])?;
                }
            }
        }
    }
    tx.commit()
}
