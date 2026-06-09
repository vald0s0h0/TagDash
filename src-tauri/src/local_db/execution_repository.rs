// Persistence for trade executions (internal simulated fills), keyed by ticker.
// Trades can span several days and survive restarts, so the chart's execution
// markers are reconstructed from these rows rather than from the in-RAM book.

use rusqlite::{params, Connection, Result};

use crate::types::{Fill, Side};

/// One persisted execution row. `quantity` is the SIGNED share delta of the fill
/// (+ for a long/buy, − for a short/sell).
#[derive(Debug, Clone)]
pub struct ExecutionRow {
    pub trade_id:   String,
    pub quantity:   i64,
    pub fill_price: f64,
    pub filled_at:  String, // RFC3339
}

/// Persist a fill. Idempotent on `fill_id`. The stored quantity is signed from
/// the fill's side so the position/P&L can be rebuilt without the Side enum.
pub fn insert_fill(conn: &Connection, fill: &Fill) -> Result<()> {
    let signed = match fill.side {
        Side::Long  =>  fill.quantity.abs(),
        Side::Short => -fill.quantity.abs(),
    };
    conn.execute(
        "INSERT OR IGNORE INTO executions (fill_id, trade_id, symbol, quantity, fill_price, filled_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            fill.fill_id,
            fill.trade_id,
            fill.symbol,
            signed,
            fill.fill_price,
            fill.filled_at.to_rfc3339(),
        ],
    )?;
    Ok(())
}

/// Record the trade's ORIGINAL (launch-time) stop loss. Idempotent on trade_id:
/// `INSERT OR IGNORE` keeps the very first value, so later SL modifications never
/// overwrite the level the journal / R:R is based on.
pub fn set_original_sl(conn: &Connection, trade_id: &str, symbol: &str, sl: f64) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO trade_levels (trade_id, symbol, original_sl) VALUES (?1, ?2, ?3)",
        params![trade_id, symbol, sl],
    )?;
    Ok(())
}

/// Original stop loss per trade for a symbol, as (trade_id, original_sl) pairs.
pub fn original_sls_for_symbol(conn: &Connection, symbol: &str) -> Result<Vec<(String, f64)>> {
    let mut stmt = conn.prepare(
        "SELECT trade_id, original_sl FROM trade_levels
         WHERE symbol = ?1 AND original_sl IS NOT NULL",
    )?;
    let rows = stmt.query_map(params![symbol], |row| Ok((row.get(0)?, row.get(1)?)))?;
    rows.collect()
}

/// All executions for a symbol, oldest → newest.
pub fn get_for_symbol(conn: &Connection, symbol: &str) -> Result<Vec<ExecutionRow>> {
    let mut stmt = conn.prepare(
        "SELECT trade_id, quantity, fill_price, filled_at
         FROM executions WHERE symbol = ?1 ORDER BY filled_at ASC, rowid ASC",
    )?;
    let rows = stmt.query_map(params![symbol], |row| {
        Ok(ExecutionRow {
            trade_id:   row.get(0)?,
            quantity:   row.get(1)?,
            fill_price: row.get(2)?,
            filled_at:  row.get(3)?,
        })
    })?;
    rows.collect()
}
