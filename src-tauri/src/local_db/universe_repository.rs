use rusqlite::{params, Connection, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UniverseAsset {
    pub symbol: String,
    pub name: Option<String>,
    pub exchange: Option<String>,
    #[serde(default)]
    pub security_type: Option<String>,
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
             (symbol, name, exchange, security_type, tradable, shortable, float_shares, market_cap, avg_volume, updated_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
         ON CONFLICT(symbol) DO UPDATE SET
             name=excluded.name, exchange=excluded.exchange,
             security_type=COALESCE(excluded.security_type, universe_assets.security_type),
             tradable=excluded.tradable, shortable=excluded.shortable,
             float_shares=excluded.float_shares, market_cap=excluded.market_cap,
             avg_volume=excluded.avg_volume, updated_at=excluded.updated_at",
        params![
            asset.symbol, asset.name, asset.exchange, asset.security_type,
            asset.tradable as i64, asset.shortable as i64,
            asset.float_shares, asset.market_cap, asset.avg_volume,
            asset.updated_at,
        ],
    )?;
    Ok(())
}

pub fn get_all(conn: &Connection) -> Result<Vec<UniverseAsset>> {
    let mut stmt = conn.prepare(
        "SELECT symbol,name,exchange,security_type,tradable,shortable,float_shares,market_cap,avg_volume,updated_at
         FROM universe_assets ORDER BY symbol",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(UniverseAsset {
            symbol: row.get(0)?,
            name: row.get(1)?,
            exchange: row.get(2)?,
            security_type: row.get(3)?,
            tradable: row.get::<_, i64>(4)? != 0,
            shortable: row.get::<_, i64>(5)? != 0,
            float_shares: row.get(6)?,
            market_cap: row.get(7)?,
            avg_volume: row.get(8)?,
            updated_at: row.get(9)?,
        })
    })?;
    rows.collect()
}

/// One asset by symbol (None when unknown). Used for per-zone card info
/// (market cap / float) without loading the whole universe.
pub fn get_by_symbol(conn: &Connection, symbol: &str) -> Result<Option<UniverseAsset>> {
    let mut stmt = conn.prepare(
        "SELECT symbol,name,exchange,security_type,tradable,shortable,float_shares,market_cap,avg_volume,updated_at
         FROM universe_assets WHERE symbol=?1",
    )?;
    let mut rows = stmt.query_map(params![symbol], |row| {
        Ok(UniverseAsset {
            symbol: row.get(0)?,
            name: row.get(1)?,
            exchange: row.get(2)?,
            security_type: row.get(3)?,
            tradable: row.get::<_, i64>(4)? != 0,
            shortable: row.get::<_, i64>(5)? != 0,
            float_shares: row.get(6)?,
            market_cap: row.get(7)?,
            avg_volume: row.get(8)?,
            updated_at: row.get(9)?,
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

/// Remove generated seed rows left by older releases. The asset name is the
/// reliable provenance marker: live Alpaca assets never use this prefix. Related
/// cache rows are removed in the same transaction before the universe rows.
pub fn purge_legacy_seed_data(conn: &mut Connection) -> Result<usize> {
    let tx = conn.transaction()?;
    tx.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS legacy_seed_symbols (
             symbol TEXT PRIMARY KEY NOT NULL
         );
         DELETE FROM legacy_seed_symbols;
         INSERT INTO legacy_seed_symbols(symbol)
             SELECT symbol FROM universe_assets WHERE name LIKE 'Mock Corp %';
         DELETE FROM daily_cache
             WHERE symbol IN (SELECT symbol FROM legacy_seed_symbols);
         DELETE FROM fundamentals_cache
             WHERE symbol IN (SELECT symbol FROM legacy_seed_symbols);
         DELETE FROM company_meta
             WHERE symbol IN (SELECT symbol FROM legacy_seed_symbols);",
    )?;
    let removed = tx.execute(
        "DELETE FROM universe_assets
         WHERE symbol IN (SELECT symbol FROM legacy_seed_symbols)",
        [],
    )?;
    tx.execute(
        "DELETE FROM tradetally_trade_ids WHERE tt_trade_id LIKE 'mock-%'",
        [],
    )?;
    tx.execute_batch("DROP TABLE legacy_seed_symbols")?;
    tx.commit()?;
    Ok(removed)
}

/// (symbol, name, exchange) for every tradable asset — the input the Panic Mean
/// Reversion build uses to drop ETFs / structured products from the watchlist
/// (see `crate::scoring::is_etf_like`). Name/exchange are optional in the table,
/// so both come back as `Option`.
pub fn symbol_classification(
    conn: &Connection,
) -> Result<Vec<(String, Option<String>, Option<String>, Option<String>)>> {
    let mut stmt = conn.prepare(
        "SELECT symbol,security_type,name,exchange FROM universe_assets WHERE tradable=1",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
        ))
    })?;
    rows.collect()
}

pub fn update_security_types(conn: &Connection, rows: &[(String, String)]) -> Result<usize> {
    let mut changed = 0usize;
    for (symbol, kind) in rows {
        changed += conn.execute(
            "UPDATE universe_assets SET security_type=?2 WHERE symbol=?1",
            params![symbol, kind],
        )?;
    }
    Ok(changed)
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

/// Symbols Alpaca will NOT lend, from the asset list the startup pipeline stores.
///
/// Answering from the cache keeps a list view free of one HTTP call per row; the
/// live `/v2/assets/{symbol}` check stays for the moment an order is actually about
/// to be sent, where borrow availability may have changed intraday.
pub fn non_shortable_symbols(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt =
        conn.prepare("SELECT symbol FROM universe_assets WHERE shortable = 0 ORDER BY symbol")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_seed_purge_removes_generated_rows_and_keeps_provider_rows() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE universe_assets (symbol TEXT PRIMARY KEY, name TEXT);
             CREATE TABLE daily_cache (symbol TEXT);
             CREATE TABLE fundamentals_cache (symbol TEXT);
             CREATE TABLE company_meta (symbol TEXT);
             CREATE TABLE tradetally_trade_ids (tt_trade_id TEXT);
             INSERT INTO universe_assets VALUES ('TEST2', 'Mock Corp 1');
             INSERT INTO universe_assets VALUES ('AAPL', 'Apple Inc.');
             INSERT INTO daily_cache VALUES ('TEST2'), ('AAPL');
             INSERT INTO fundamentals_cache VALUES ('TEST2'), ('AAPL');
             INSERT INTO company_meta VALUES ('TEST2'), ('AAPL');
             INSERT INTO tradetally_trade_ids VALUES ('mock-123'), ('real-123');",
        )
        .unwrap();

        assert_eq!(purge_legacy_seed_data(&mut conn).unwrap(), 1);
        for table in [
            "universe_assets",
            "daily_cache",
            "fundamentals_cache",
            "company_meta",
        ] {
            let generated: i64 = conn
                .query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE symbol='TEST2'"),
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            let provider: i64 = conn
                .query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE symbol='AAPL'"),
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(generated, 0, "{table}");
            assert_eq!(provider, 1, "{table}");
        }
        let ids: Vec<String> = conn
            .prepare("SELECT tt_trade_id FROM tradetally_trade_ids")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert_eq!(ids, vec!["real-123"]);
    }
}
