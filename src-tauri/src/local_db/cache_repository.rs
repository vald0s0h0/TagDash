use rusqlite::{params, Connection, Result};
use serde::{Deserialize, Serialize};

// ─── App-config key/value (small persisted markers) ──────────────────────────

/// Read a value from the `app_config` key/value table.
pub fn get_app_meta(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row(
        "SELECT value FROM app_config WHERE key=?1",
        params![key],
        |r| r.get::<_, String>(0),
    )
    .ok()
}

/// Upsert a value into the `app_config` key/value table.
pub fn set_app_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO app_config (key, value, updated_at) VALUES (?1, ?2, datetime('now'))
         ON CONFLICT(key) DO UPDATE SET value=excluded.value, updated_at=excluded.updated_at",
        params![key, value],
    )?;
    Ok(())
}

// ─── Fundamentals ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FundamentalCache {
    pub symbol: String,
    pub float_shares: Option<i64>,
    pub outstanding_shares: Option<i64>,
    pub free_float: Option<f64>,
    pub prev_close: Option<f64>,
    pub avg_volume: Option<i64>,
    pub atr: Option<f64>,
    pub updated_at: String,
}

pub fn upsert_fundamental(conn: &Connection, f: &FundamentalCache) -> Result<()> {
    // Only float/fundamentals columns: the multi-day change_* columns are owned
    // by `recompute_multiday_changes` and must NOT be touched here (this upsert
    // runs every startup and would otherwise wipe yesterday's computed changes).
    conn.execute(
        "INSERT INTO fundamentals_cache
             (symbol,float_shares,outstanding_shares,free_float,prev_close,
              avg_volume,atr,updated_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
         ON CONFLICT(symbol) DO UPDATE SET
             float_shares=excluded.float_shares,
             outstanding_shares=excluded.outstanding_shares,
             free_float=excluded.free_float,
             prev_close=excluded.prev_close,
             avg_volume=excluded.avg_volume,
             atr=excluded.atr,
             updated_at=excluded.updated_at",
        params![
            f.symbol, f.float_shares, f.outstanding_shares, f.free_float,
            f.prev_close, f.avg_volume, f.atr, f.updated_at,
        ],
    )?;
    Ok(())
}

pub fn get_fundamental(conn: &Connection, symbol: &str) -> Result<Option<FundamentalCache>> {
    let mut stmt = conn.prepare(
        "SELECT symbol,float_shares,outstanding_shares,free_float,prev_close,
                avg_volume,atr,updated_at
         FROM fundamentals_cache WHERE symbol=?1",
    )?;
    let mut rows = stmt.query_map(params![symbol], |row| {
        Ok(FundamentalCache {
            symbol: row.get(0)?,
            float_shares: row.get(1)?,
            outstanding_shares: row.get(2)?,
            free_float: row.get(3)?,
            prev_close: row.get(4)?,
            avg_volume: row.get(5)?,
            atr: row.get(6)?,
            updated_at: row.get(7)?,
        })
    })?;
    rows.next().transpose()
}

/// All cached fundamentals (used to reuse FMP floats without a network call).
pub fn all_fundamentals(conn: &Connection) -> Result<Vec<FundamentalCache>> {
    let mut stmt = conn.prepare(
        "SELECT symbol,float_shares,outstanding_shares,free_float,prev_close,
                avg_volume,atr,updated_at
         FROM fundamentals_cache",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(FundamentalCache {
            symbol: row.get(0)?,
            float_shares: row.get(1)?,
            outstanding_shares: row.get(2)?,
            free_float: row.get(3)?,
            prev_close: row.get(4)?,
            avg_volume: row.get(5)?,
            atr: row.get(6)?,
            updated_at: row.get(7)?,
        })
    })?;
    rows.collect()
}

// ─── Multi-day price change (close-to-close, gaps included) ───────────────────

/// Close-to-close % change for one symbol over 1..6 trading days.
/// `changes[0]` = 1-day change (yesterday's close vs the day before),
/// `changes[5]` = 6-day cumulative change. None when history is too short.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultiDayChange {
    pub symbol: String,
    pub changes: [Option<f64>; 6],
}

/// Recompute, for every symbol with daily bars, the close-to-close percentage
/// change over the last 1..6 trading days and store it in `fundamentals_cache`
/// (`change_1d_pct` … `change_6d_pct`). The change over N days is
/// `(c0 - cN) / cN * 100`, where `c0` is the most recent cached close ("yesterday"
/// — today's bar isn't cached pre-open) and `cN` is the close N trading days
/// before it. Being close-to-close, each step spans the overnight gap, so gaps are
/// inherently included. Returns the number of symbols updated.
pub fn recompute_multiday_changes(conn: &Connection) -> Result<usize> {
    // One pass: pivot each symbol's 7 most recent closes (c0..c6) into a row.
    let rows: Vec<(String, [Option<f64>; 7])> = {
        let mut stmt = conn.prepare(
            "SELECT symbol,
                 MAX(CASE WHEN rn=1 THEN close END),
                 MAX(CASE WHEN rn=2 THEN close END),
                 MAX(CASE WHEN rn=3 THEN close END),
                 MAX(CASE WHEN rn=4 THEN close END),
                 MAX(CASE WHEN rn=5 THEN close END),
                 MAX(CASE WHEN rn=6 THEN close END),
                 MAX(CASE WHEN rn=7 THEN close END)
             FROM (
                 SELECT symbol, close,
                        ROW_NUMBER() OVER (PARTITION BY symbol ORDER BY date DESC) AS rn
                 FROM daily_cache WHERE close IS NOT NULL
             ) WHERE rn <= 7 GROUP BY symbol",
        )?;
        let mapped = stmt.query_map([], |row| {
            let symbol: String = row.get(0)?;
            let mut c: [Option<f64>; 7] = [None; 7];
            for (i, slot) in c.iter_mut().enumerate() {
                *slot = row.get::<_, Option<f64>>(i + 1)?;
            }
            Ok((symbol, c))
        })?;
        mapped.collect::<Result<Vec<_>>>()?
    };

    conn.execute_batch("BEGIN")?;
    let mut updated = 0usize;
    for (symbol, c) in &rows {
        let c0 = c[0];
        let mut changes: [Option<f64>; 6] = [None; 6];
        for (i, slot) in changes.iter_mut().enumerate() {
            *slot = match (c0, c[i + 1]) {
                (Some(now), Some(base)) if base != 0.0 => Some((now - base) / base * 100.0),
                _ => None,
            };
        }
        conn.execute(
            "INSERT INTO fundamentals_cache
                 (symbol,change_1d_pct,change_2d_pct,change_3d_pct,
                  change_4d_pct,change_5d_pct,change_6d_pct,updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7, datetime('now'))
             ON CONFLICT(symbol) DO UPDATE SET
                 change_1d_pct=excluded.change_1d_pct,
                 change_2d_pct=excluded.change_2d_pct,
                 change_3d_pct=excluded.change_3d_pct,
                 change_4d_pct=excluded.change_4d_pct,
                 change_5d_pct=excluded.change_5d_pct,
                 change_6d_pct=excluded.change_6d_pct,
                 updated_at=excluded.updated_at",
            params![
                symbol, changes[0], changes[1], changes[2],
                changes[3], changes[4], changes[5],
            ],
        )?;
        updated += 1;
    }
    conn.execute_batch("COMMIT")?;
    Ok(updated)
}

/// Read the stored multi-day close-to-close % changes for one symbol.
pub fn multiday_changes(conn: &Connection, symbol: &str) -> Result<Option<MultiDayChange>> {
    let mut stmt = conn.prepare(
        "SELECT change_1d_pct,change_2d_pct,change_3d_pct,
                change_4d_pct,change_5d_pct,change_6d_pct
         FROM fundamentals_cache WHERE symbol=?1",
    )?;
    let mut rows = stmt.query_map(params![symbol], |row| {
        let mut changes: [Option<f64>; 6] = [None; 6];
        for (i, slot) in changes.iter_mut().enumerate() {
            *slot = row.get::<_, Option<f64>>(i)?;
        }
        Ok(MultiDayChange { symbol: symbol.to_string(), changes })
    })?;
    rows.next().transpose()
}

/// Most recent `updated_at` across the fundamentals cache (RFC3339-ish string),
/// used to throttle the FMP full-universe load to once per calendar day.
pub fn fundamentals_last_date(conn: &Connection) -> Option<String> {
    conn.query_row(
        "SELECT MAX(updated_at) FROM fundamentals_cache",
        [],
        |r| r.get::<_, Option<String>>(0),
    )
    .ok()
    .flatten()
}

// ─── Daily bars ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyBar {
    pub symbol: String,
    pub date: String,
    pub open: Option<f64>,
    pub high: Option<f64>,
    pub low: Option<f64>,
    pub close: Option<f64>,
    pub volume: Option<i64>,
    pub updated_at: String,
}

pub fn upsert_daily_bar(conn: &Connection, bar: &DailyBar) -> Result<()> {
    conn.execute(
        "INSERT INTO daily_cache (symbol,date,open,high,low,close,volume,updated_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
         ON CONFLICT(symbol,date) DO UPDATE SET
             open=excluded.open, high=excluded.high, low=excluded.low,
             close=excluded.close, volume=excluded.volume,
             updated_at=excluded.updated_at",
        params![bar.symbol, bar.date, bar.open, bar.high, bar.low, bar.close, bar.volume, bar.updated_at],
    )?;
    Ok(())
}

/// Latest cached daily close per symbol — used to seed `previous_close` so live
/// change% is meaningful from the first trade.
pub fn latest_closes(conn: &Connection) -> Result<Vec<(String, f64)>> {
    let mut stmt = conn.prepare(
        "SELECT d.symbol, d.close FROM daily_cache d
         WHERE d.date = (SELECT MAX(x.date) FROM daily_cache x WHERE x.symbol = d.symbol)
           AND d.close IS NOT NULL",
    )?;
    let rows = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?)))?;
    rows.collect()
}

/// Latest cached daily VOLUME per symbol (the previous trading day's volume,
/// since today's bar isn't cached pre-open). Mirrors `latest_closes`. Used by the
/// mean-reversion scoring to gate (>20M), tie-break and display.
pub fn latest_volumes(conn: &Connection) -> Result<Vec<(String, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT d.symbol, d.volume FROM daily_cache d
         WHERE d.date = (SELECT MAX(x.date) FROM daily_cache x WHERE x.symbol = d.symbol)
           AND d.volume IS NOT NULL",
    )?;
    let rows = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))?;
    rows.collect()
}

/// Most recent bar date (YYYY-MM-DD) cached across all symbols, or None when the
/// daily cache is empty. Used to fetch only the missing days on later startups.
pub fn latest_bar_date(conn: &Connection) -> Result<Option<String>> {
    conn.query_row("SELECT MAX(date) FROM daily_cache", [], |r| {
        r.get::<_, Option<String>>(0)
    })
}

/// Oldest bar date (YYYY-MM-DD) cached across all symbols, or None when empty.
/// Used to detect a shallow cache that needs a deeper historical backfill (the
/// mean-reversion scoring needs ~3 years), independent of how recent the newest
/// bar is.
pub fn earliest_bar_date(conn: &Connection) -> Result<Option<String>> {
    conn.query_row("SELECT MIN(date) FROM daily_cache", [], |r| {
        r.get::<_, Option<String>>(0)
    })
}

/// Count of distinct symbols that have at least one cached daily bar.
pub fn symbols_with_bars(conn: &Connection) -> Result<i64> {
    conn.query_row("SELECT COUNT(DISTINCT symbol) FROM daily_cache", [], |r| r.get(0))
}

/// Drop bars older than `cutoff_date` (YYYY-MM-DD) to bound the cache to the
/// recent window we actually use (~50 trading days).
pub fn prune_before(conn: &Connection, cutoff_date: &str) -> Result<usize> {
    let n = conn.execute("DELETE FROM daily_cache WHERE date < ?1", params![cutoff_date])?;
    Ok(n)
}

/// Average daily volume per symbol over the most recent `days` cached bars
/// (default trading-volume window = 20 days). Computed in SQL so it stays
/// correct whether the last fetch was a full 50-day load or an incremental one.
pub fn avg_volumes(conn: &Connection, days: u32) -> Result<Vec<(String, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT symbol, CAST(AVG(volume) AS INTEGER) FROM (
             SELECT symbol, volume,
                    ROW_NUMBER() OVER (PARTITION BY symbol ORDER BY date DESC) AS rn
             FROM daily_cache WHERE volume IS NOT NULL
         ) WHERE rn <= ?1 GROUP BY symbol",
    )?;
    let rows = stmt.query_map([days], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    rows.collect()
}

/// Date-ASCENDING daily closes for one symbol, most recent `limit` bars. Used by
/// the mean-reversion scoring engine (which wants oldest→newest series). Skips
/// rows with a NULL close.
pub fn closes_ascending(conn: &Connection, symbol: &str, limit: u32) -> Result<Vec<f64>> {
    let mut stmt = conn.prepare(
        "SELECT close FROM (
             SELECT close, date FROM daily_cache
             WHERE symbol = ?1 AND close IS NOT NULL
             ORDER BY date DESC LIMIT ?2
         ) ORDER BY date ASC",
    )?;
    let rows = stmt.query_map(params![symbol, limit], |row| row.get::<_, f64>(0))?;
    rows.collect()
}

/// Date-ASCENDING daily OHLCV for one symbol, most recent `limit` bars. Mirrors
/// `closes_ascending` but returns the full bar (open, high, low, close, volume) so
/// the mean-reversion scoring can compute candle colour (close vs open), daily true
/// range (parabolic expansion) and dollar volume. Rows with any NULL OHLC are
/// skipped (a missing close already excludes the bar); a NULL volume becomes 0.
pub fn ohlcv_ascending(
    conn: &Connection,
    symbol: &str,
    limit: u32,
) -> Result<Vec<(f64, f64, f64, f64, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT open, high, low, close, volume FROM (
             SELECT open, high, low, close, volume, date FROM daily_cache
             WHERE symbol = ?1
               AND open IS NOT NULL AND high IS NOT NULL
               AND low IS NOT NULL AND close IS NOT NULL
             ORDER BY date DESC LIMIT ?2
         ) ORDER BY date ASC",
    )?;
    let rows = stmt.query_map(params![symbol, limit], |row| {
        Ok((
            row.get::<_, f64>(0)?,
            row.get::<_, f64>(1)?,
            row.get::<_, f64>(2)?,
            row.get::<_, f64>(3)?,
            row.get::<_, Option<i64>>(4)?.unwrap_or(0),
        ))
    })?;
    rows.collect()
}

pub fn get_daily_bars(conn: &Connection, symbol: &str, limit: u32) -> Result<Vec<DailyBar>> {
    let mut stmt = conn.prepare(
        "SELECT symbol,date,open,high,low,close,volume,updated_at
         FROM daily_cache WHERE symbol=?1 ORDER BY date DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![symbol, limit], |row| {
        Ok(DailyBar {
            symbol: row.get(0)?,
            date: row.get(1)?,
            open: row.get(2)?,
            high: row.get(3)?,
            low: row.get(4)?,
            close: row.get(5)?,
            volume: row.get(6)?,
            updated_at: row.get(7)?,
        })
    })?;
    rows.collect()
}
