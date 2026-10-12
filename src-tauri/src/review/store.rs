// Per-session market data — one small SQLite file per review session.
//
// Kept apart from the databases so a session is self-contained (the trader deletes
// flat files and day tapes once used; the session keeps its own copy) and so a
// dropped session is one `rm`.
//
// The tape lives PACKED (`packed`: one zstd block of delta-encoded columns for the
// prints, one for the quotes — see `codec`), about ten times smaller than rows.
// Rows arriving live first land in the unindexed staging tables below, and are
// packed when the session closes, after a SIP rebuild, or once STAGE_LIMIT rows
// have piled up. Readers merge both.
//
//   trades(ts, price, size, prints, exch, cond, origin)      — staging
//       One row per print. `prints` > 1 marks an AGGREGATE (Massive second bar,
//       synthetic replay slice) rather than a real print — the replay's speed of
//       tape counts real prints only when the session has them.
//   quotes(ts, bid, bid_size, ask, ask_size, bid_x, ask_x, origin) — staging
//       NBBO as received. Sizes/exchanges are NULL when the source had none.
//   packed(kind, n, data)          — "trades" | "quotes" blocks
//   summary(key, value)            — counts of the packed blocks (no decoding
//                                    needed to know a session holds data)
//   enriched(from_ms, to_ms)       — windows already rebuilt from SIP
//   bars(tf, t, o, h, l, c, v, n, origin)      — context: "1d" and "1m" (t = unix s)
//
// `origin` says where each row came from: sip_live | iex_live | massive_live |
// tape_live (pre-window copy of the day tape) | replay:<source>:<provider> | sip_rest.

use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::Serialize;

use super::codec::{self, QuoteRow, TradeRow};
use super::Deps;

/// Staged rows past which the live capture packs the session.
pub const STAGE_LIMIT: i64 = 50_000;

pub fn open(path: &Path) -> rusqlite::Result<Connection> {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let conn = Connection::open(path)?;
    // The capture writer, the pre-window copy and a SIP rebuild can touch the same
    // file at once: wait for the lock rather than fail.
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         CREATE TABLE IF NOT EXISTS trades (
             ts     INTEGER NOT NULL,
             price  REAL NOT NULL,
             size   INTEGER NOT NULL,
             prints INTEGER NOT NULL DEFAULT 1,
             exch   TEXT,
             cond   TEXT,
             origin TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS quotes (
             ts       INTEGER NOT NULL,
             bid      REAL NOT NULL,
             bid_size INTEGER,
             ask      REAL NOT NULL,
             ask_size INTEGER,
             bid_x    TEXT,
             ask_x    TEXT,
             origin   TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS bars (
             tf     TEXT NOT NULL,
             t      INTEGER NOT NULL,
             o REAL, h REAL, l REAL, c REAL,
             v INTEGER, n INTEGER,
             origin TEXT NOT NULL,
             PRIMARY KEY (tf, t)
         );
         CREATE TABLE IF NOT EXISTS packed (
             kind TEXT PRIMARY KEY,
             n    INTEGER NOT NULL,
             data BLOB NOT NULL
         );
         CREATE TABLE IF NOT EXISTS summary (
             key   TEXT PRIMARY KEY,
             value INTEGER
         );
         CREATE TABLE IF NOT EXISTS enriched (
             from_ms INTEGER NOT NULL,
             to_ms   INTEGER NOT NULL
         );
         -- Files written before packing indexed the rows.
         DROP INDEX IF EXISTS idx_trades_ts;
         DROP INDEX IF EXISTS idx_quotes_ts;",
    )?;
    Ok(conn)
}

/// Create the file (and its day folder) if needed.
pub fn ensure(path: &Path) -> Result<(), String> {
    open(path).map(|_| ()).map_err(|e| e.to_string())
}

pub fn remove(path: &Path) {
    let _ = std::fs::remove_file(path);
    for ext in ["db-wal", "db-shm"] {
        let _ = std::fs::remove_file(path.with_extension(ext));
    }
}

/// Copy the ticker's prints (and any quotes) of `[from_ms, to_ms)` out of the live
/// day tape — the minutes BEFORE the alert, which the tape already holds because
/// the premarket broad tier records every print.
pub fn copy_from_live_tape(app_dir: &Path, path: &Path, day: &str, symbol: &str, from_ms: i64, to_ms: i64) {
    let tape = crate::replay::tape::tape_path(app_dir, day);
    if !tape.exists() || to_ms <= from_ms {
        return;
    }
    let Ok(src) = Connection::open_with_flags(&tape, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else {
        return;
    };
    let Ok(mut dst) = open(path) else { return };
    let has_cols = crate::replay::tape::has_extended_columns(&src);
    let trades: Vec<(i64, f64, i64, Option<String>, Option<String>)> = {
        let sql = if has_cols {
            "SELECT ts, price, size, exch, cond FROM trades WHERE symbol = ?1 AND ts >= ?2 AND ts < ?3 ORDER BY ts"
        } else {
            "SELECT ts, price, size, NULL, NULL FROM trades WHERE symbol = ?1 AND ts >= ?2 AND ts < ?3 ORDER BY ts"
        };
        src.prepare(sql)
            .and_then(|mut st| {
                st.query_map(params![symbol, from_ms, to_ms], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                })
                .map(|it| it.filter_map(|r| r.ok()).collect())
            })
            .unwrap_or_default()
    };
    let quotes: Vec<(i64, f64, Option<i64>, f64, Option<i64>, Option<String>, Option<String>)> = {
        let sql = if has_cols {
            "SELECT ts, bid, bid_size, ask, ask_size, bid_x, ask_x FROM quotes WHERE symbol = ?1 AND ts >= ?2 AND ts < ?3 ORDER BY ts"
        } else {
            "SELECT ts, bid, NULL, ask, NULL, NULL, NULL FROM quotes WHERE symbol = ?1 AND ts >= ?2 AND ts < ?3 ORDER BY ts"
        };
        src.prepare(sql)
            .and_then(|mut st| {
                st.query_map(params![symbol, from_ms, to_ms], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?))
                })
                .map(|it| it.filter_map(|r| r.ok()).collect())
            })
            .unwrap_or_default()
    };
    if trades.is_empty() && quotes.is_empty() {
        return;
    }
    let origin = format!("tape_{}", live_feed_label());
    let Ok(tx) = dst.transaction() else { return };
    for (ts, p, s, x, c) in &trades {
        let _ = tx.execute(
            "INSERT INTO trades (ts, price, size, prints, exch, cond, origin) VALUES (?1,?2,?3,1,?4,?5,?6)",
            params![ts, p, s, x, c, origin],
        );
    }
    for (ts, b, bs, a, as_, bx, ax) in &quotes {
        let _ = tx.execute(
            "INSERT INTO quotes (ts, bid, bid_size, ask, ask_size, bid_x, ask_x, origin) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![ts, b, bs, a, as_, bx, ax, origin],
        );
    }
    let _ = tx.commit();
}

/// "sip" | "iex" — the plan the live Alpaca feed runs on (last probe).
pub fn live_feed_label() -> &'static str {
    let Some(deps) = super::deps() else { return "sip" };
    let conn = deps.db.lock().unwrap();
    match crate::local_db::cache_repository::get_app_meta(&conn, crate::alpaca::plan::PLAN_META_KEY)
        .as_deref()
    {
        Some("iex") => "iex",
        _ => "sip",
    }
}

/// Snapshot the context bars: ~1 year of daily bars strictly before the session day
/// (from the daily cache) and every 1-minute bar RAM holds for the ticker (bounded to
/// the market clock, so a replay never leaks the future).
pub fn snapshot_context(deps: &Deps, path: &Path, symbol: &str, day: &str) {
    let daily = {
        let conn = deps.db.lock().unwrap();
        crate::local_db::cache_repository::get_daily_bars_before(&conn, symbol, day, 300)
            .unwrap_or_default()
    };
    let minute = {
        let m = deps.market.read().unwrap();
        m.get_bars(symbol, crate::market_state::aggregators::Timeframe::M1)
    };
    let Ok(mut dst) = open(path) else { return };
    let Ok(tx) = dst.transaction() else { return };
    for b in &daily {
        let (Some(o), Some(h), Some(l), Some(c)) = (b.open, b.high, b.low, b.close) else {
            continue;
        };
        let Some(t) = chrono::NaiveDate::parse_from_str(&b.date, "%Y-%m-%d")
            .ok()
            .and_then(|d| d.and_hms_opt(0, 0, 0))
            .map(|d| d.and_utc().timestamp())
        else {
            continue;
        };
        let _ = tx.execute(
            "INSERT OR IGNORE INTO bars (tf, t, o, h, l, c, v, n, origin) VALUES ('1d',?1,?2,?3,?4,?5,?6,NULL,'context')",
            params![t, o, h, l, c, b.volume.unwrap_or(0)],
        );
    }
    for b in &minute {
        let _ = tx.execute(
            "INSERT INTO bars (tf, t, o, h, l, c, v, n, origin) VALUES ('1m',?1,?2,?3,?4,?5,?6,?7,'context')
             ON CONFLICT(tf, t) DO UPDATE SET o=excluded.o, h=excluded.h, l=excluded.l, c=excluded.c,
                 v=excluded.v, n=excluded.n
             WHERE bars.origin = 'context'",
            params![b.time.timestamp(), b.open, b.high, b.low, b.close, b.volume as i64, b.trade_count.map(|n| n as i64)],
        );
    }
    let _ = tx.commit();
}

// ─── The tape: packed blocks + staged rows ──────────────────────────────────────

/// Every print and quote of a session, in time order.
#[derive(Debug, Default)]
pub struct Tape {
    pub trades: Vec<TradeRow>,
    pub quotes: Vec<QuoteRow>,
}

fn packed_block(conn: &Connection, kind: &str) -> rusqlite::Result<Option<Vec<u8>>> {
    conn.query_row("SELECT data FROM packed WHERE kind = ?1", params![kind], |r| r.get(0))
        .optional()
}

fn staged_trades(conn: &Connection) -> rusqlite::Result<Vec<TradeRow>> {
    let mut st = conn.prepare("SELECT ts, price, size, prints, exch, cond, origin FROM trades ORDER BY rowid")?;
    let rows = st.query_map([], |r| {
        Ok(TradeRow {
            ts: r.get(0)?,
            price: r.get(1)?,
            size: r.get(2)?,
            prints: r.get(3)?,
            exch: r.get(4)?,
            cond: r.get(5)?,
            origin: r.get(6)?,
        })
    })?;
    rows.collect()
}

fn staged_quotes(conn: &Connection) -> rusqlite::Result<Vec<QuoteRow>> {
    let mut st = conn.prepare(
        "SELECT ts, bid, bid_size, ask, ask_size, bid_x, ask_x, origin FROM quotes ORDER BY rowid",
    )?;
    let rows = st.query_map([], |r| {
        Ok(QuoteRow {
            ts: r.get(0)?,
            bid: r.get(1)?,
            bid_size: r.get(2)?,
            ask: r.get(3)?,
            ask_size: r.get(4)?,
            bid_x: r.get(5)?,
            ask_x: r.get(6)?,
            origin: r.get(7)?,
        })
    })?;
    rows.collect()
}

fn load_conn(conn: &Connection) -> Result<Tape, String> {
    let e = |e: rusqlite::Error| e.to_string();
    let mut tape = Tape::default();
    if let Some(b) = packed_block(conn, "trades").map_err(e)? {
        tape.trades = codec::decode_trades(&b)?;
    }
    if let Some(b) = packed_block(conn, "quotes").map_err(e)? {
        tape.quotes = codec::decode_quotes(&b)?;
    }
    tape.trades.extend(staged_trades(conn).map_err(e)?);
    tape.quotes.extend(staged_quotes(conn).map_err(e)?);
    // Stable: equal timestamps keep their arrival order.
    tape.trades.sort_by_key(|t| t.ts);
    tape.quotes.sort_by_key(|q| q.ts);
    Ok(tape)
}

/// The whole tape of a session (empty when the file is missing or unreadable).
pub fn load(path: &Path) -> Tape {
    if !path.exists() {
        return Tape::default();
    }
    match open(path).map_err(|e| e.to_string()).and_then(|c| load_conn(&c)) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("[tagdash] review: lecture {} : {e}", path.display());
            Tape::default()
        }
    }
}

/// Rewrite the packed blocks with `tape` and empty the staging tables — inside the
/// caller's transaction, so a concurrent capture write lands before or after,
/// never in between.
fn store_tape(tx: &Connection, tape: &Tape) -> Result<(), String> {
    let e = |e: rusqlite::Error| e.to_string();
    let t = codec::encode_trades(&tape.trades)?;
    let q = codec::encode_quotes(&tape.quotes)?;
    tx.execute(
        "INSERT OR REPLACE INTO packed (kind, n, data) VALUES ('trades', ?1, ?2), ('quotes', ?3, ?4)",
        params![tape.trades.len() as i64, t, tape.quotes.len() as i64, q],
    )
    .map_err(e)?;
    tx.execute_batch("DELETE FROM trades; DELETE FROM quotes;").map_err(e)?;
    let real = tape.trades.iter().filter(|t| t.prints == 1).count() as i64;
    tx.execute(
        "INSERT OR REPLACE INTO summary (key, value) VALUES ('real_prints', ?1), ('quotes', ?2)",
        params![real, tape.quotes.len() as i64],
    )
    .map_err(e)?;
    Ok(())
}

/// Give the space freed by the staging rows back to the disk.
fn shrink(conn: &Connection) {
    let _ = conn.execute_batch("VACUUM; PRAGMA wal_checkpoint(TRUNCATE);");
}

/// Apply `edit` to the whole tape and store it packed (read, edit and write under
/// one write lock).
fn rewrite(path: &Path, edit: impl FnOnce(&mut Tape)) -> Result<(), String> {
    let mut conn = open(path).map_err(|e| e.to_string())?;
    {
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let mut tape = load_conn(&tx)?;
        edit(&mut tape);
        store_tape(&tx, &tape)?;
        tx.commit().map_err(|e| e.to_string())?;
    }
    shrink(&conn);
    Ok(())
}

/// Pack the staged rows (no-op when there are none).
pub fn compact(path: &Path) -> Result<(), String> {
    if !path.exists() || staged_count(path) == 0 {
        return Ok(());
    }
    rewrite(path, |_| {})
}

/// Rows waiting in the staging tables.
pub fn staged_count(path: &Path) -> i64 {
    let Ok(conn) = open(path) else { return 0 };
    conn.query_row("SELECT (SELECT COUNT(*) FROM trades) + (SELECT COUNT(*) FROM quotes)", [], |r| r.get(0))
        .unwrap_or(0)
}

/// Keep only the prints and quotes inside `windows` (context bars untouched).
pub fn keep_windows(path: &Path, windows: &[(i64, i64)]) -> Result<(), String> {
    let inside = |ts: i64| windows.iter().any(|&(a, b)| ts >= a && ts <= b);
    rewrite(path, |t| {
        t.trades.retain(|r| inside(r.ts));
        t.quotes.retain(|r| inside(r.ts));
    })
}

/// A session file still in the row format of the first versions.
pub fn is_unpacked(path: &Path) -> bool {
    let Ok(conn) = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else {
        return false;
    };
    let has_packed: bool = conn
        .query_row("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'packed'", [], |_| Ok(true))
        .optional()
        .ok()
        .flatten()
        .unwrap_or(false);
    let staged: i64 = conn
        .query_row("SELECT (SELECT COUNT(*) FROM trades) + (SELECT COUNT(*) FROM quotes)", [], |r| r.get(0))
        .unwrap_or(0);
    !has_packed || staged > 0
}

/// One-shot markers kept in `summary` (e.g. a migration already applied).
pub fn flag(path: &Path, key: &str) -> bool {
    let Ok(conn) = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else {
        return false;
    };
    conn.query_row("SELECT value FROM summary WHERE key = ?1", params![key], |r| r.get::<_, i64>(0))
        .map(|v| v != 0)
        .unwrap_or(false)
}

pub fn set_flag(path: &Path, key: &str) -> Result<(), String> {
    let conn = open(path).map_err(|e| e.to_string())?;
    conn.execute("INSERT OR REPLACE INTO summary (key, value) VALUES (?1, 1)", params![key])
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Time span of the rows of `origin` (epoch ms).
pub fn origin_span(tape: &Tape, origin: &str) -> Option<(i64, i64)> {
    let ts = tape
        .trades
        .iter()
        .filter(|t| t.origin == origin)
        .map(|t| t.ts)
        .chain(tape.quotes.iter().filter(|q| q.origin == origin).map(|q| q.ts));
    ts.fold(None, |acc, t| match acc {
        None => Some((t, t)),
        Some((a, b)) => Some((a.min(t), b.max(t))),
    })
}

/// True when the session holds any real print or quote (no decoding).
pub fn has_data(path: &Path) -> bool {
    let Ok(conn) = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else {
        return false;
    };
    let packed: i64 = conn
        .query_row(
            "SELECT COALESCE(SUM(value), 0) FROM summary WHERE key IN ('real_prints', 'quotes')",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    packed > 0
        || conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM trades WHERE prints = 1) OR EXISTS(SELECT 1 FROM quotes)",
                [],
                |r| r.get::<_, bool>(0),
            )
            .unwrap_or(false)
}

/// Remember a window rebuilt from SIP (never fetched again).
pub fn mark_enriched(path: &Path, from_ms: i64, to_ms: i64) -> Result<(), String> {
    let conn = open(path).map_err(|e| e.to_string())?;
    conn.execute("INSERT INTO enriched (from_ms, to_ms) VALUES (?1, ?2)", params![from_ms, to_ms])
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Was `[from_ms, to_ms]` already rebuilt from SIP (by one rebuild)?
pub fn enriched_covers(path: &Path, from_ms: i64, to_ms: i64) -> bool {
    let Ok(conn) = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else {
        return false;
    };
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM enriched WHERE from_ms <= ?1 AND to_ms >= ?2)",
        params![from_ms, to_ms],
        |r| r.get::<_, bool>(0),
    )
    .unwrap_or(false)
}

// ─── Reading (for the review player) ────────────────────────────────────────────

/// Columnar dump of a session — compact over IPC (tens of thousands of prints).
#[derive(Debug, Default, Serialize)]
pub struct SessionData {
    pub trades_ts: Vec<i64>,
    pub trades_price: Vec<f64>,
    pub trades_size: Vec<i64>,
    pub trades_prints: Vec<i64>,
    pub trades_exch: Vec<Option<String>>,
    pub quotes_ts: Vec<i64>,
    pub quotes_bid: Vec<f64>,
    pub quotes_bid_size: Vec<Option<i64>>,
    pub quotes_ask: Vec<f64>,
    pub quotes_ask_size: Vec<Option<i64>>,
    /// [t (unix s), o, h, l, c, v]
    pub bars_1d: Vec<[f64; 6]>,
    pub bars_1m: Vec<[f64; 6]>,
    /// Origins actually present in the file.
    pub origins: Vec<String>,
}

/// The player's view of a session, from its tape (see `load`).
pub fn session_data(path: &Path, tape: &Tape) -> SessionData {
    let mut d = SessionData::default();
    let mut origins: Vec<String> = Vec::new();
    let mut note = |o: &str| {
        if !origins.iter().any(|x| x == o) {
            origins.push(o.to_string());
        }
    };
    for t in &tape.trades {
        d.trades_ts.push(t.ts);
        d.trades_price.push(t.price);
        d.trades_size.push(t.size);
        d.trades_prints.push(t.prints);
        d.trades_exch.push(t.exch.clone());
        note(&t.origin);
    }
    for q in &tape.quotes {
        d.quotes_ts.push(q.ts);
        d.quotes_bid.push(q.bid);
        d.quotes_bid_size.push(q.bid_size);
        d.quotes_ask.push(q.ask);
        d.quotes_ask_size.push(q.ask_size);
        note(&q.origin);
    }
    if let Ok(conn) = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) {
        for (tf, out) in [("1d", &mut d.bars_1d), ("1m", &mut d.bars_1m)] {
            if let Ok(mut st) = conn.prepare("SELECT t, o, h, l, c, v FROM bars WHERE tf = ?1 ORDER BY t") {
                let rows = st.query_map(params![tf], |r| {
                    Ok([
                        r.get::<_, i64>(0)? as f64,
                        r.get::<_, f64>(1)?,
                        r.get::<_, f64>(2)?,
                        r.get::<_, f64>(3)?,
                        r.get::<_, f64>(4)?,
                        r.get::<_, i64>(5)? as f64,
                    ])
                });
                if let Ok(rows) = rows {
                    out.extend(rows.flatten());
                }
            }
        }
        if let Ok(mut st) = conn.prepare("SELECT DISTINCT origin FROM bars") {
            if let Ok(rows) = st.query_map([], |r| r.get::<_, String>(0)) {
                for o in rows.flatten() {
                    note(&o);
                }
            }
        }
    }
    d.origins = origins;
    d
}

pub fn read_all(path: &Path) -> SessionData {
    session_data(path, &load(path))
}

/// What a window of the tape holds — decides whether a SIP rebuild is needed.
pub struct Coverage {
    pub real_prints: i64,
    pub quotes: i64,
    /// First / last real print of the window (epoch ms).
    pub first_print: Option<i64>,
    pub last_print: Option<i64>,
}

impl Tape {
    pub fn coverage(&self, from_ms: i64, to_ms: i64) -> Coverage {
        let prints = self.trades.iter().filter(|t| t.prints == 1 && t.ts >= from_ms && t.ts <= to_ms);
        let (mut n, mut first, mut last) = (0i64, None, None);
        for t in prints {
            n += 1;
            first.get_or_insert(t.ts);
            last = Some(t.ts);
        }
        Coverage {
            real_prints: n,
            quotes: self.quotes.iter().filter(|q| q.ts >= from_ms && q.ts <= to_ms).count() as i64,
            first_print: first,
            last_print: last,
        }
    }
}

/// Replace the window's prints and quotes with a SIP rebuild. Rows recorded live
/// from SIP are kept (they are the same tape, received first-hand); everything else
/// in the window — aggregates, IEX, synthetic slices — gives way to the SIP truth.
/// The result is stored packed and the window remembered as rebuilt.
pub fn replace_with_sip(
    path: &Path,
    from_ms: i64,
    to_ms: i64,
    trades: &[super::sip::SipTrade],
    quotes: &[super::sip::SipQuote],
    bars_1m: &[super::sip::SipBar],
    bars_1d: &[super::sip::SipBar],
) -> Result<(), String> {
    let live = |o: &str| o == "sip_live" || o == "tape_sip";
    rewrite(path, |tape| {
        let in_window = |ts: i64| ts >= from_ms && ts <= to_ms;
        tape.trades.retain(|t| !in_window(t.ts) || live(&t.origin));
        tape.quotes.retain(|q| !in_window(q.ts) || live(&q.origin));
        // Where the live SIP recording already covers a stretch, do not double it.
        let span = |ts: &mut dyn Iterator<Item = i64>| {
            let v: Vec<i64> = ts.collect();
            Some((*v.iter().min()?, *v.iter().max()?))
        };
        let live_t = span(&mut tape.trades.iter().filter(|t| live(&t.origin)).map(|t| t.ts));
        let live_q = span(&mut tape.quotes.iter().filter(|q| live(&q.origin)).map(|q| q.ts));
        let inside = |s: Option<(i64, i64)>, ts: i64| s.is_some_and(|(a, b)| ts >= a && ts <= b);
        for t in trades {
            if !inside(live_t, t.ts_ms) {
                tape.trades.push(TradeRow {
                    ts: t.ts_ms,
                    price: t.price,
                    size: t.size,
                    prints: 1,
                    exch: t.exch.clone(),
                    cond: t.cond.clone(),
                    origin: "sip_rest".into(),
                });
            }
        }
        for q in quotes {
            if !inside(live_q, q.ts_ms) {
                tape.quotes.push(QuoteRow {
                    ts: q.ts_ms,
                    bid: q.bid,
                    bid_size: q.bid_size,
                    ask: q.ask,
                    ask_size: q.ask_size,
                    bid_x: q.bid_x.clone(),
                    ask_x: q.ask_x.clone(),
                    origin: "sip_rest".into(),
                });
            }
        }
        tape.trades.sort_by_key(|t| t.ts);
        tape.quotes.sort_by_key(|q| q.ts);
    })?;
    let mut conn = open(path).map_err(|e| e.to_string())?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    for (tf, bars) in [("1m", bars_1m), ("1d", bars_1d)] {
        for b in bars {
            tx.execute(
                "INSERT INTO bars (tf, t, o, h, l, c, v, n, origin) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,'sip_rest')
                 ON CONFLICT(tf, t) DO UPDATE SET o=excluded.o, h=excluded.h, l=excluded.l,
                     c=excluded.c, v=excluded.v, n=excluded.n, origin='sip_rest'",
                params![tf, b.t, b.o, b.h, b.l, b.c, b.v, b.n],
            )
            .map_err(|e| e.to_string())?;
        }
    }
    tx.execute("INSERT INTO enriched (from_ms, to_ms) VALUES (?1, ?2)", params![from_ms, to_ms])
        .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())
}
