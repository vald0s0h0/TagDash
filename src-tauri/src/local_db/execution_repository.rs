// Persistence for trade executions (internal simulated fills), keyed by ticker.
// Trades can span several days and survive restarts, so the chart's execution
// markers are reconstructed from these rows rather than from the in-RAM book.

use rusqlite::{params, Connection, OptionalExtension, Result};

use crate::types::{Fill, Side};

/// One persisted execution row. `quantity` is the SIGNED share delta of the fill
/// (+ for a long/buy, − for a short/sell).
#[derive(Debug, Clone)]
pub struct ExecutionRow {
    pub trade_id: String,
    pub quantity: i64,
    pub fill_price: f64,
    pub filled_at: String, // RFC3339 — the true execution instant
    /// Market time of the price this fill executed against (RFC3339). Differs from
    /// `filled_at` by the data plan's delay; the chart places its marker here so the
    /// triangle lands on the candle that produced the price. `None` on rows written
    /// before the column existed, and whenever the feed had no print for the symbol.
    pub data_at: Option<String>,
    /// Sizing button behind an entry: 25 / 50 / 100. `None` for exits, for
    /// strategy-driven fills, and for rows predating the column.
    pub size_pct: Option<u8>,
}

/// Persist a fill. Idempotent on `fill_id`. The stored quantity is signed from
/// the fill's side so the position/P&L can be rebuilt without the Side enum.
pub fn insert_fill(conn: &Connection, fill: &Fill) -> Result<()> {
    let signed = match fill.side {
        Side::Long => fill.quantity.abs(),
        Side::Short => -fill.quantity.abs(),
    };
    conn.execute(
        "INSERT OR IGNORE INTO executions
             (fill_id, trade_id, symbol, quantity, fill_price, filled_at, data_at, size_pct)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            fill.fill_id,
            fill.trade_id,
            fill.symbol,
            signed,
            fill.fill_price,
            fill.filled_at.to_rfc3339(),
            fill.data_at.map(|d| d.to_rfc3339()),
            fill.size_pct,
        ],
    )?;
    Ok(())
}

/// Record the trade's ORIGINAL (launch-time) stop loss. First write wins: the
/// upsert only fills the column when it is still NULL, so later SL modifications
/// never overwrite the level the journal / R:R is based on. The row itself may
/// already exist (the TP stamp below shares it), hence the ON CONFLICT rather
/// than a plain `INSERT OR IGNORE`.
pub fn set_original_sl(conn: &Connection, trade_id: &str, symbol: &str, sl: f64) -> Result<()> {
    conn.execute(
        "INSERT INTO trade_levels (trade_id, symbol, original_sl) VALUES (?1, ?2, ?3)
         ON CONFLICT(trade_id) DO UPDATE
           SET original_sl = COALESCE(trade_levels.original_sl, excluded.original_sl)",
        params![trade_id, symbol, sl],
    )?;
    Ok(())
}

/// Record the trade's ORIGINAL (launch-time) take profit — same first-write-wins
/// semantics as [`set_original_sl`], sharing the trade's `trade_levels` row.
pub fn set_original_tp(conn: &Connection, trade_id: &str, symbol: &str, tp: f64) -> Result<()> {
    conn.execute(
        "INSERT INTO trade_levels (trade_id, symbol, original_tp) VALUES (?1, ?2, ?3)
         ON CONFLICT(trade_id) DO UPDATE
           SET original_tp = COALESCE(trade_levels.original_tp, excluded.original_tp)",
        params![trade_id, symbol, tp],
    )?;
    Ok(())
}

/// Original (launch-time) stop loss for a single trade, if one was stamped.
pub fn original_sl_for_trade(conn: &Connection, trade_id: &str) -> Result<Option<f64>> {
    conn.query_row(
        "SELECT original_sl FROM trade_levels WHERE trade_id = ?1 AND original_sl IS NOT NULL",
        params![trade_id],
        |row| row.get(0),
    )
    .optional()
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

/// Original take profit per trade for a symbol, as (trade_id, original_tp) pairs.
pub fn original_tps_for_symbol(conn: &Connection, symbol: &str) -> Result<Vec<(String, f64)>> {
    let mut stmt = conn.prepare(
        "SELECT trade_id, original_tp FROM trade_levels
         WHERE symbol = ?1 AND original_tp IS NOT NULL",
    )?;
    let rows = stmt.query_map(params![symbol], |row| Ok((row.get(0)?, row.get(1)?)))?;
    rows.collect()
}

/// All executions for a symbol, oldest → newest.
pub fn get_for_symbol(conn: &Connection, symbol: &str) -> Result<Vec<ExecutionRow>> {
    let mut stmt = conn.prepare(
        "SELECT trade_id, quantity, fill_price, filled_at, data_at, size_pct
         FROM executions WHERE symbol = ?1 ORDER BY filled_at ASC, rowid ASC",
    )?;
    let rows = stmt.query_map(params![symbol], |row| {
        Ok(ExecutionRow {
            trade_id: row.get(0)?,
            quantity: row.get(1)?,
            fill_price: row.get(2)?,
            filled_at: row.get(3)?,
            data_at: row.get(4)?,
            size_pct: row.get(5)?,
        })
    })?;
    rows.collect()
}
