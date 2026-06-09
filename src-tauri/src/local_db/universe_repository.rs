use rusqlite::{params, Connection, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UniverseAsset {
    pub symbol: String,
    pub name: Option<String>,
    pub exchange: Option<String>,
    pub tradable: bool,
    pub shortable: bool,
    pub float_shares: Option<i64>,
    pub market_cap: Option<i64>,
    pub avg_volume: Option<i64>,
    pub updated_at: String,
}

pub fn upsert(conn: &Connection, asset: &UniverseAsset) -> Result<()> {
    conn.execute(
        "INSERT INTO universe_assets
             (symbol, name, exchange, tradable, shortable, float_shares, market_cap, avg_volume, updated_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)
         ON CONFLICT(symbol) DO UPDATE SET
             name=excluded.name, exchange=excluded.exchange,
             tradable=excluded.tradable, shortable=excluded.shortable,
             float_shares=excluded.float_shares, market_cap=excluded.market_cap,
             avg_volume=excluded.avg_volume, updated_at=excluded.updated_at",
        params![
            asset.symbol, asset.name, asset.exchange,
            asset.tradable as i64, asset.shortable as i64,
            asset.float_shares, asset.market_cap, asset.avg_volume,
            asset.updated_at,
        ],
    )?;
    Ok(())
}

pub fn get_all(conn: &Connection) -> Result<Vec<UniverseAsset>> {
    let mut stmt = conn.prepare(
        "SELECT symbol,name,exchange,tradable,shortable,float_shares,market_cap,avg_volume,updated_at
         FROM universe_assets ORDER BY symbol",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(UniverseAsset {
            symbol: row.get(0)?,
            name: row.get(1)?,
            exchange: row.get(2)?,
            tradable: row.get::<_, i64>(3)? != 0,
            shortable: row.get::<_, i64>(4)? != 0,
            float_shares: row.get(5)?,
            market_cap: row.get(6)?,
            avg_volume: row.get(7)?,
            updated_at: row.get(8)?,
        })
    })?;
    rows.collect()
}

/// One asset by symbol (None when unknown). Used for per-zone card info
/// (market cap / float) without loading the whole universe.
pub fn get_by_symbol(conn: &Connection, symbol: &str) -> Result<Option<UniverseAsset>> {
    let mut stmt = conn.prepare(
        "SELECT symbol,name,exchange,tradable,shortable,float_shares,market_cap,avg_volume,updated_at
         FROM universe_assets WHERE symbol=?1",
    )?;
    let mut rows = stmt.query_map(params![symbol], |row| {
        Ok(UniverseAsset {
            symbol: row.get(0)?,
            name: row.get(1)?,
            exchange: row.get(2)?,
            tradable: row.get::<_, i64>(3)? != 0,
            shortable: row.get::<_, i64>(4)? != 0,
            float_shares: row.get(5)?,
            market_cap: row.get(6)?,
            avg_volume: row.get(7)?,
            updated_at: row.get(8)?,
        })
    })?;
    rows.next().transpose()
}

pub fn get_active_symbols(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt =
        conn.prepare("SELECT symbol FROM universe_assets WHERE tradable=1 ORDER BY symbol")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    rows.collect()
}

pub fn count(conn: &Connection) -> Result<i64> {
    conn.query_row("SELECT COUNT(*) FROM universe_assets", [], |r| r.get(0))
}

/// Symbols for a streaming universe.
/// - US Stocks: all active tradable equities.
/// - Low Float: tradable equities with a known float below `low_float_max`
///   (no market-cap / price / volume filter — float only).
pub fn streamable_symbols(
    conn: &Connection,
    low_float_only: bool,
    low_float_max: i64,
) -> Result<Vec<String>> {
    if low_float_only {
        let mut stmt = conn.prepare(
            "SELECT symbol FROM universe_assets
             WHERE tradable=1 AND float_shares IS NOT NULL AND float_shares < ?1
             ORDER BY symbol",
        )?;
        let rows = stmt.query_map([low_float_max], |row| row.get::<_, String>(0))?;
        rows.collect()
    } else {
        get_active_symbols(conn)
    }
}

/// Count of tradable equities with a known float below `low_float_max`.
pub fn count_low_float(conn: &Connection, low_float_max: i64) -> Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM universe_assets
         WHERE tradable=1 AND float_shares IS NOT NULL AND float_shares < ?1",
        [low_float_max],
        |r| r.get(0),
    )
}

/// Count of all tradable equities (the US Stocks universe).
pub fn count_tradable(conn: &Connection) -> Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM universe_assets WHERE tradable=1",
        [],
        |r| r.get(0),
    )
}
