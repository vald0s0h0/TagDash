// One prepared day on disk: `<app>/replay/day-YYYY-MM-DD.db`.
//
// Written incrementally (WAL), every step idempotent, so an interrupted preparation
// simply resumes. Nothing here knows about strategies beyond the ids and
// fingerprints it is handed; what to store is decided by `plan`.
//
//   market       whole-market daily snapshot of the day (prev close + OHLCV), the
//                source every universe filter is evaluated against, locally;
//   symbols      every candidate whose minutes were fetched, kept or not, with its
//                volume per 10-minute bucket — so a widened filter can re-decide
//                without re-fetching;
//   minute_bars  1-minute bars of the kept symbols, 04:00–20:00 ET, unadjusted;
//   scans/hits   one pre-scan per strategy, under the fingerprint it was run with.
//                While a scan is still being confirmed, its hits land as they are
//                found and `manifest` holds how far it got (`PartialScan`) — which is
//                what lets a replay start on the first opportunities of a day whose
//                preparation is still running, and an interrupted one resume;
//   ticks        real trades/quotes blocks (`review::codec`), each covering exactly
//                [start_ms, end_ms) — COVERAGE IS THESE BLOCKS, there is no separate
//                table to drift from them;
//   news         headlines strictly before each alert.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDate, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use super::intervals::{merge, Span};
use super::Hit;
use crate::market_state::aggregators::Bar;
use crate::review::codec::{self, QuoteRow, TradeRow};
use crate::types::NewsHeadline;

pub const SCHEMA_VERSION: &str = "1";
/// 10-minute buckets from 04:00 to 20:00 ET.
pub const BUCKETS: usize = 96;

pub fn replay_dir(app_dir: &Path) -> PathBuf {
    crate::paths::replay_dir(app_dir)
}

pub fn day_path(app_dir: &Path, day: &str) -> PathBuf {
    replay_dir(app_dir).join(format!("day-{day}.db"))
}

/// Days that have a store on disk, ascending.
pub fn days_on_disk(app_dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(replay_dir(app_dir))
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().to_string();
                    let day = name.strip_prefix("day-")?.strip_suffix(".db")?.to_string();
                    NaiveDate::parse_from_str(&day, "%Y-%m-%d").ok()?;
                    Some(day)
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

#[derive(Debug, Clone, PartialEq)]
pub struct MarketRow {
    pub symbol: String,
    pub prev_close: Option<f64>,
    pub o: f64,
    pub h: f64,
    pub l: f64,
    pub c: f64,
    pub v: i64,
}

/// A scan under way: its hits are in `hits`, confirmed in time order up to here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PartialScan {
    /// The scan fingerprint it runs under — another one means starting over.
    pub fingerprint: String,
    /// Every minute hit before this instant has been probed.
    pub frontier_ms: i64,
    /// Every alert before this instant is known (and stored): a probe still to come
    /// can only confirm at or after it.
    pub final_ms: i64,
    /// The confirmation's own per-symbol state, to resume exactly where it stopped.
    #[serde(default)]
    pub state: serde_json::Value,
}

fn partial_key(strategy_id: &str) -> String {
    format!("partial:{strategy_id}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TickKind {
    Trades,
    Quotes,
}

impl TickKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TickKind::Trades => "trades",
            TickKind::Quotes => "quotes",
        }
    }
}

pub struct DayStore {
    conn: Connection,
}

impl DayStore {
    /// Open (creating if needed) the store for `day`.
    pub fn open(app_dir: &Path, day: &str) -> Result<Self, String> {
        std::fs::create_dir_all(replay_dir(app_dir)).map_err(|e| e.to_string())?;
        let conn = Connection::open(day_path(app_dir, day)).map_err(|e| e.to_string())?;
        let store = Self { conn };
        store.init().map_err(|e| e.to_string())?;
        Ok(store)
    }

    /// Open only if the day was ever prepared — never creates a file.
    pub fn open_existing(app_dir: &Path, day: &str) -> Option<Self> {
        let path = day_path(app_dir, day);
        if !path.exists() {
            return None;
        }
        let conn = Connection::open(path).ok()?;
        let store = Self { conn };
        store.init().ok()?;
        Some(store)
    }

    fn init(&self) -> rusqlite::Result<()> {
        self.conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             CREATE TABLE IF NOT EXISTS manifest(key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS market(
                symbol TEXT PRIMARY KEY, prev_close REAL,
                o REAL NOT NULL, h REAL NOT NULL, l REAL NOT NULL, c REAL NOT NULL,
                v INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS symbols(
                symbol TEXT PRIMARY KEY, kept INTEGER NOT NULL, buckets BLOB NOT NULL);
             CREATE TABLE IF NOT EXISTS minute_bars(
                symbol TEXT NOT NULL, t_ms INTEGER NOT NULL,
                o REAL NOT NULL, h REAL NOT NULL, l REAL NOT NULL, c REAL NOT NULL,
                v INTEGER NOT NULL, n INTEGER, vw REAL,
                PRIMARY KEY(symbol, t_ms)) WITHOUT ROWID;
             CREATE TABLE IF NOT EXISTS scans(
                strategy_id TEXT PRIMARY KEY, fingerprint TEXT NOT NULL,
                scanned_ms INTEGER NOT NULL, profile TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS hits(
                strategy_id TEXT NOT NULL, symbol TEXT NOT NULL, alert_ms INTEGER NOT NULL,
                score REAL NOT NULL, reason TEXT NOT NULL);
             CREATE INDEX IF NOT EXISTS hits_by_strategy ON hits(strategy_id);
             CREATE TABLE IF NOT EXISTS ticks(
                symbol TEXT NOT NULL, kind TEXT NOT NULL,
                start_ms INTEGER NOT NULL, end_ms INTEGER NOT NULL,
                rows INTEGER NOT NULL, blob BLOB NOT NULL);
             CREATE INDEX IF NOT EXISTS ticks_by_symbol ON ticks(symbol, kind, start_ms);
             CREATE TABLE IF NOT EXISTS news_symbols(symbol TEXT PRIMARY KEY);
             CREATE TABLE IF NOT EXISTS news(id INTEGER PRIMARY KEY, created_ms INTEGER NOT NULL, json TEXT NOT NULL);",
        )?;
        self.conn.execute(
            "INSERT OR IGNORE INTO manifest(key, value) VALUES ('schema_version', ?1)",
            [SCHEMA_VERSION],
        )?;
        Ok(())
    }

    // ── manifest ──

    pub fn meta(&self, key: &str) -> Option<String> {
        self.conn
            .query_row("SELECT value FROM manifest WHERE key=?1", [key], |r| r.get(0))
            .optional()
            .ok()
            .flatten()
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO manifest(key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                params![key, value],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    // ── market snapshot ──

    pub fn has_market(&self) -> bool {
        self.meta("market_done").as_deref() == Some("1")
    }

    pub fn put_market(&mut self, rows: &[MarketRow]) -> Result<(), String> {
        let tx = self.conn.transaction().map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM market", []).map_err(|e| e.to_string())?;
        {
            let mut st = tx
                .prepare("INSERT INTO market(symbol, prev_close, o, h, l, c, v) VALUES (?1,?2,?3,?4,?5,?6,?7)")
                .map_err(|e| e.to_string())?;
            for r in rows {
                st.execute(params![r.symbol, r.prev_close, r.o, r.h, r.l, r.c, r.v])
                    .map_err(|e| e.to_string())?;
            }
        }
        tx.execute(
            "INSERT INTO manifest(key, value) VALUES ('market_done', '1')
             ON CONFLICT(key) DO UPDATE SET value='1'",
            [],
        )
        .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())
    }

    pub fn market(&self) -> Vec<MarketRow> {
        let Ok(mut st) = self
            .conn
            .prepare("SELECT symbol, prev_close, o, h, l, c, v FROM market")
        else {
            return Vec::new();
        };
        st.query_map([], |r| {
            Ok(MarketRow {
                symbol: r.get(0)?,
                prev_close: r.get(1)?,
                o: r.get(2)?,
                h: r.get(3)?,
                l: r.get(4)?,
                c: r.get(5)?,
                v: r.get(6)?,
            })
        })
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
    }

    pub fn prev_closes(&self) -> HashMap<String, f64> {
        self.market()
            .into_iter()
            .filter(|m| self.is_kept(&m.symbol))
            .filter_map(|m| m.prev_close.map(|p| (m.symbol, p)))
            .collect()
    }

    // ── candidate symbols ──

    /// Every candidate whose minutes were fetched: (kept, volume per bucket).
    pub fn symbol_stats(&self) -> HashMap<String, (bool, Vec<u64>)> {
        let Ok(mut st) = self.conn.prepare("SELECT symbol, kept, buckets FROM symbols") else {
            return HashMap::new();
        };
        st.query_map([], |r| {
            let blob: Vec<u8> = r.get(2)?;
            Ok((r.get::<_, String>(0)?, (r.get::<_, i64>(1)? == 1, unpack(&blob))))
        })
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
    }

    fn is_kept(&self, symbol: &str) -> bool {
        self.conn
            .query_row("SELECT kept FROM symbols WHERE symbol=?1", [symbol], |r| {
                r.get::<_, i64>(0)
            })
            .map(|k| k == 1)
            .unwrap_or(false)
    }

    pub fn kept_symbols(&self) -> Vec<String> {
        let Ok(mut st) = self
            .conn
            .prepare("SELECT symbol FROM symbols WHERE kept=1 ORDER BY symbol")
        else {
            return Vec::new();
        };
        st.query_map([], |r| r.get(0))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default()
    }

    /// Record one fetched candidate; its bars are stored only when `kept`.
    pub fn put_symbol(&mut self, symbol: &str, buckets: &[u64], kept: bool, bars: &[Bar]) -> Result<(), String> {
        let tx = self.conn.transaction().map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT INTO symbols(symbol, kept, buckets) VALUES (?1, ?2, ?3)
             ON CONFLICT(symbol) DO UPDATE SET kept=excluded.kept, buckets=excluded.buckets",
            params![symbol, kept as i64, pack(buckets)],
        )
        .map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM minute_bars WHERE symbol=?1", [symbol])
            .map_err(|e| e.to_string())?;
        if kept {
            let mut st = tx
                .prepare(
                    "INSERT INTO minute_bars(symbol, t_ms, o, h, l, c, v, n, vw)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                )
                .map_err(|e| e.to_string())?;
            for b in bars {
                st.execute(params![
                    symbol,
                    b.time.timestamp_millis(),
                    b.open,
                    b.high,
                    b.low,
                    b.close,
                    b.volume as i64,
                    b.trade_count.map(|n| n as i64),
                    b.vwap
                ])
                .map_err(|e| e.to_string())?;
            }
        }
        tx.commit().map_err(|e| e.to_string())
    }

    pub fn minutes(&self, symbol: &str) -> Vec<Bar> {
        let Ok(mut st) = self.conn.prepare(
            "SELECT t_ms, o, h, l, c, v, n, vw FROM minute_bars WHERE symbol=?1 ORDER BY t_ms",
        ) else {
            return Vec::new();
        };
        st.query_map([symbol], |r| {
            Ok(Bar {
                time: DateTime::from_timestamp_millis(r.get(0)?).unwrap_or_default(),
                open: r.get(1)?,
                high: r.get(2)?,
                low: r.get(3)?,
                close: r.get(4)?,
                volume: r.get::<_, i64>(5)?.max(0) as u64,
                trade_count: r.get::<_, Option<i64>>(6)?.map(|n| n.max(0) as u64),
                vwap: r.get(7)?,
            })
        })
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
    }

    // ── scans ──

    pub fn scan_fingerprint(&self, strategy_id: &str) -> Option<String> {
        self.conn
            .query_row(
                "SELECT fingerprint FROM scans WHERE strategy_id=?1",
                [strategy_id],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten()
    }

    /// Replace a strategy's scan and hits.
    pub fn put_scan(
        &mut self,
        strategy_id: &str,
        fingerprint: &str,
        profile_json: &str,
        hits: &[(String, Hit)],
    ) -> Result<(), String> {
        let tx = self.conn.transaction().map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM hits WHERE strategy_id=?1", [strategy_id])
            .map_err(|e| e.to_string())?;
        {
            let mut st = tx
                .prepare("INSERT INTO hits(strategy_id, symbol, alert_ms, score, reason) VALUES (?1,?2,?3,?4,?5)")
                .map_err(|e| e.to_string())?;
            for (sym, h) in hits {
                st.execute(params![strategy_id, sym, h.alert_ms, h.score, h.reason])
                    .map_err(|e| e.to_string())?;
            }
        }
        tx.execute(
            "INSERT INTO scans(strategy_id, fingerprint, scanned_ms, profile) VALUES (?1,?2,?3,?4)
             ON CONFLICT(strategy_id) DO UPDATE SET fingerprint=excluded.fingerprint,
                 scanned_ms=excluded.scanned_ms, profile=excluded.profile",
            params![strategy_id, fingerprint, Utc::now().timestamp_millis(), profile_json],
        )
        .map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM manifest WHERE key=?1", [partial_key(strategy_id)])
            .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())
    }

    /// The scan under way for a strategy, if one was started and not finished.
    pub fn partial(&self, strategy_id: &str) -> Option<PartialScan> {
        serde_json::from_str(&self.meta(&partial_key(strategy_id))?).ok()
    }

    /// Start a strategy's scan over: its previous scan and hits are dropped, and
    /// the new one is recorded as under way.
    pub fn begin_partial(&mut self, strategy_id: &str, partial: &PartialScan) -> Result<(), String> {
        let json = serde_json::to_string(partial).map_err(|e| e.to_string())?;
        let tx = self.conn.transaction().map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM hits WHERE strategy_id=?1", [strategy_id])
            .map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM scans WHERE strategy_id=?1", [strategy_id])
            .map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT INTO manifest(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![partial_key(strategy_id), json],
        )
        .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())
    }

    /// Add the hits one confirmation step found and move the scan's progress with
    /// them — one transaction, so a resumed scan never re-finds a stored hit.
    pub fn add_hits(
        &mut self,
        strategy_id: &str,
        hits: &[(String, Hit)],
        partial: &PartialScan,
    ) -> Result<(), String> {
        let json = serde_json::to_string(partial).map_err(|e| e.to_string())?;
        let tx = self.conn.transaction().map_err(|e| e.to_string())?;
        {
            let mut st = tx
                .prepare("INSERT INTO hits(strategy_id, symbol, alert_ms, score, reason) VALUES (?1,?2,?3,?4,?5)")
                .map_err(|e| e.to_string())?;
            for (sym, h) in hits {
                st.execute(params![strategy_id, sym, h.alert_ms, h.score, h.reason])
                    .map_err(|e| e.to_string())?;
            }
        }
        tx.execute(
            "INSERT INTO manifest(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![partial_key(strategy_id), json],
        )
        .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())
    }

    /// The scan under way is complete: its hits become the strategy's scan.
    pub fn finish_scan(&mut self, strategy_id: &str, fingerprint: &str, profile_json: &str) -> Result<(), String> {
        let tx = self.conn.transaction().map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT INTO scans(strategy_id, fingerprint, scanned_ms, profile) VALUES (?1,?2,?3,?4)
             ON CONFLICT(strategy_id) DO UPDATE SET fingerprint=excluded.fingerprint,
                 scanned_ms=excluded.scanned_ms, profile=excluded.profile",
            params![strategy_id, fingerprint, Utc::now().timestamp_millis(), profile_json],
        )
        .map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM manifest WHERE key=?1", [partial_key(strategy_id)])
            .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())
    }

    /// A strategy's hits, time-ordered.
    pub fn hits(&self, strategy_id: &str) -> Vec<(String, Hit)> {
        let Ok(mut st) = self.conn.prepare(
            "SELECT symbol, alert_ms, score, reason FROM hits WHERE strategy_id=?1 ORDER BY alert_ms",
        ) else {
            return Vec::new();
        };
        st.query_map([strategy_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                Hit {
                    alert_ms: r.get(1)?,
                    score: r.get(2)?,
                    reason: r.get(3)?,
                },
            ))
        })
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
    }

    // ── ticks ──

    /// Merged coverage of one (symbol, kind).
    pub fn coverage(&self, symbol: &str, kind: TickKind) -> Vec<Span> {
        let Ok(mut st) = self
            .conn
            .prepare("SELECT start_ms, end_ms FROM ticks WHERE symbol=?1 AND kind=?2")
        else {
            return Vec::new();
        };
        let spans: Vec<Span> = st
            .query_map(params![symbol, kind.as_str()], |r| Ok((r.get(0)?, r.get(1)?)))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default();
        merge(spans)
    }

    /// Every (symbol, kind) coverage at once.
    pub fn all_coverage(&self) -> HashMap<(String, TickKind), Vec<Span>> {
        let mut out: HashMap<(String, TickKind), Vec<Span>> = HashMap::new();
        let Ok(mut st) = self.conn.prepare("SELECT symbol, kind, start_ms, end_ms FROM ticks") else {
            return out;
        };
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        });
        if let Ok(rows) = rows {
            for (sym, kind, s, e) in rows.flatten() {
                let k = if kind == "quotes" {
                    TickKind::Quotes
                } else {
                    TickKind::Trades
                };
                out.entry((sym, k)).or_default().push((s, e));
            }
        }
        for v in out.values_mut() {
            *v = merge(std::mem::take(v));
        }
        out
    }

    pub fn put_trades(&self, symbol: &str, span: Span, rows: &[TradeRow]) -> Result<(), String> {
        let blob = codec::encode_trades(rows)?;
        self.put_block(symbol, TickKind::Trades, span, rows.len(), &blob)
    }

    pub fn put_quotes(&self, symbol: &str, span: Span, rows: &[QuoteRow]) -> Result<(), String> {
        let blob = codec::encode_quotes(rows)?;
        self.put_block(symbol, TickKind::Quotes, span, rows.len(), &blob)
    }

    fn put_block(&self, symbol: &str, kind: TickKind, span: Span, rows: usize, blob: &[u8]) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO ticks(symbol, kind, start_ms, end_ms, rows, blob) VALUES (?1,?2,?3,?4,?5,?6)",
                params![symbol, kind.as_str(), span.0, span.1, rows as i64, blob],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// All trade rows of the day, per symbol (blocks decoded, time-ordered).
    pub fn trades(&self) -> HashMap<String, Vec<TradeRow>> {
        let mut out: HashMap<String, Vec<TradeRow>> = HashMap::new();
        for (sym, blob) in self.blocks(TickKind::Trades) {
            if let Ok(rows) = codec::decode_trades(&blob) {
                out.entry(sym).or_default().extend(rows);
            }
        }
        for v in out.values_mut() {
            v.sort_by_key(|r| r.ts);
            v.dedup_by(|a, b| a == b);
        }
        out
    }

    pub fn quotes(&self) -> HashMap<String, Vec<QuoteRow>> {
        let mut out: HashMap<String, Vec<QuoteRow>> = HashMap::new();
        for (sym, blob) in self.blocks(TickKind::Quotes) {
            if let Ok(rows) = codec::decode_quotes(&blob) {
                out.entry(sym).or_default().extend(rows);
            }
        }
        for v in out.values_mut() {
            v.sort_by_key(|r| r.ts);
            v.dedup_by(|a, b| a == b);
        }
        out
    }

    fn blocks(&self, kind: TickKind) -> Vec<(String, Vec<u8>)> {
        let Ok(mut st) = self
            .conn
            .prepare("SELECT symbol, blob FROM ticks WHERE kind=?1 ORDER BY symbol, start_ms")
        else {
            return Vec::new();
        };
        st.query_map([kind.as_str()], |r| Ok((r.get(0)?, r.get(1)?)))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default()
    }

    /// Stored tick bytes and covered milliseconds, for the size estimate.
    pub fn tick_totals(&self) -> (i64, i64) {
        self.conn
            .query_row(
                "SELECT COALESCE(SUM(LENGTH(blob)),0), COALESCE(SUM(end_ms-start_ms),0) FROM ticks",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap_or((0, 0))
    }

    // ── news ──

    pub fn news_symbols(&self) -> Vec<String> {
        let Ok(mut st) = self.conn.prepare("SELECT symbol FROM news_symbols") else {
            return Vec::new();
        };
        st.query_map([], |r| r.get(0))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default()
    }

    pub fn put_news(&mut self, symbols: &[String], items: &[NewsHeadline]) -> Result<(), String> {
        let tx = self.conn.transaction().map_err(|e| e.to_string())?;
        for n in items {
            let json = serde_json::to_string(n).map_err(|e| e.to_string())?;
            tx.execute(
                "INSERT OR IGNORE INTO news(id, created_ms, json) VALUES (?1,?2,?3)",
                params![n.id, n.created_at.timestamp_millis(), json],
            )
            .map_err(|e| e.to_string())?;
        }
        for s in symbols {
            tx.execute("INSERT OR IGNORE INTO news_symbols(symbol) VALUES (?1)", [s])
                .map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())
    }

    pub fn news(&self) -> Vec<NewsHeadline> {
        let Ok(mut st) = self.conn.prepare("SELECT json FROM news ORDER BY created_ms") else {
            return Vec::new();
        };
        st.query_map([], |r| r.get::<_, String>(0))
            .map(|rows| {
                rows.flatten()
                    .filter_map(|j| serde_json::from_str(&j).ok())
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Bucket index of an instant: 10-minute slots from 04:00 ET.
pub fn bucket_of(t: DateTime<Utc>) -> Option<usize> {
    let m = crate::time::et_minutes(t);
    (240..1200).contains(&m).then(|| ((m - 240) / 10) as usize)
}

/// Volume per 10-minute bucket of a day's minute bars.
pub fn buckets_of(bars: &[Bar]) -> Vec<u64> {
    let mut out = vec![0u64; BUCKETS];
    for b in bars {
        if let Some(i) = bucket_of(b.time) {
            out[i] += b.volume;
        }
    }
    out
}

fn pack(v: &[u64]) -> Vec<u8> {
    v.iter().flat_map(|x| (*x as u32).to_le_bytes()).collect()
}

fn unpack(b: &[u8]) -> Vec<u64> {
    b.chunks_exact(4)
        .map(|c| u64::from(u32::from_le_bytes([c[0], c[1], c[2], c[3]])))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "tagdash-replay-store-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn trade(ts: i64, price: f64) -> TradeRow {
        TradeRow {
            ts,
            price,
            size: 100,
            prints: 1,
            exch: Some("V".into()),
            cond: None,
            origin: "sip".into(),
        }
    }

    #[test]
    fn coverage_is_the_stored_blocks_and_ticks_round_trip() {
        let dir = tmp_dir();
        let s = DayStore::open(&dir, "2026-06-01").unwrap();
        s.put_trades("ABC", (1_000, 2_000), &[trade(1_100, 5.01), trade(1_500, 5.02)])
            .unwrap();
        s.put_trades("ABC", (2_000, 3_000), &[]).unwrap();
        assert_eq!(s.coverage("ABC", TickKind::Trades), vec![(1_000, 3_000)]);
        assert!(s.coverage("ABC", TickKind::Quotes).is_empty());
        let t = s.trades();
        assert_eq!(t["ABC"].len(), 2);
        assert_eq!(t["ABC"][1].price, 5.02);
        drop(s);
        assert!(DayStore::open_existing(&dir, "2026-06-01").is_some());
        assert!(DayStore::open_existing(&dir, "2026-06-02").is_none());
        assert_eq!(days_on_disk(&dir), vec!["2026-06-01".to_string()]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_scan_replaces_the_previous_one() {
        let dir = tmp_dir();
        let mut s = DayStore::open(&dir, "2026-06-01").unwrap();
        let hit = |ms| Hit {
            alert_ms: ms,
            score: 3.0,
            reason: "x".into(),
        };
        s.put_scan("mp", "aaaa", "{}", &[("ABC".into(), hit(10)), ("XYZ".into(), hit(20))])
            .unwrap();
        s.put_scan("mp", "bbbb", "{}", &[("ABC".into(), hit(30))]).unwrap();
        assert_eq!(s.scan_fingerprint("mp").as_deref(), Some("bbbb"));
        assert_eq!(s.hits("mp").len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_scan_under_way_keeps_its_hits_and_its_progress_together() {
        let dir = tmp_dir();
        let mut s = DayStore::open(&dir, "2026-06-01").unwrap();
        let hit = |ms| Hit {
            alert_ms: ms,
            score: 3.0,
            reason: "x".into(),
        };
        s.put_scan("mp", "old", "{}", &[("OLD".into(), hit(1))]).unwrap();
        let mut partial = PartialScan {
            fingerprint: "new".into(),
            frontier_ms: 0,
            final_ms: 0,
            state: serde_json::Value::Null,
        };
        s.begin_partial("mp", &partial).unwrap();
        assert!(s.hits("mp").is_empty(), "the previous scan's hits are gone");
        assert_eq!(s.scan_fingerprint("mp"), None, "…and it no longer counts as scanned");

        partial.frontier_ms = 500;
        partial.final_ms = 400;
        partial.state = serde_json::json!({ "ABC": [120, 1, 0, 1] });
        s.add_hits("mp", &[("ABC".into(), hit(100))], &partial).unwrap();
        assert_eq!(s.partial("mp"), Some(partial));
        assert_eq!(s.hits("mp").len(), 1);

        s.finish_scan("mp", "new", "{}").unwrap();
        assert_eq!(s.scan_fingerprint("mp").as_deref(), Some("new"));
        assert_eq!(s.partial("mp"), None);
        assert_eq!(s.hits("mp").len(), 1, "finishing keeps what was found");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn buckets_survive_the_round_trip() {
        let v: Vec<u64> = (0..BUCKETS as u64).collect();
        assert_eq!(unpack(&pack(&v)), v);
    }
}
