// Dashboard (moodboard) persistence: the local mirror of TradeTally trades that
// feeds the KPI cards, plus the local copy of sent diary entries. TradeTally is
// the source of truth — `tt_trades` is rebuilt from the API on every tab open.

use rusqlite::{params, Connection, Result};

use crate::dashboard::DashboardTrade;

/// Upsert one trade (keyed by the TradeTally id). Re-syncing simply overwrites
/// the cached row with the latest upstream values.
pub fn upsert_trade(conn: &Connection, t: &DashboardTrade) -> Result<()> {
    let tags_json = serde_json::to_string(&t.tags).unwrap_or_else(|_| "[]".into());
    let raw_json = serde_json::to_string(&t.raw).unwrap_or_else(|_| "{}".into());
    conn.execute(
        "INSERT INTO tt_trades
            (tt_id, symbol, side, quantity, entry_price, exit_price, pnl, pnl_percent,
             entry_date, exit_date, commission, fees, status, setup, strategy, broker,
             tags_json, raw_json, synced_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18, datetime('now'))
         ON CONFLICT(tt_id) DO UPDATE SET
            symbol=excluded.symbol, side=excluded.side, quantity=excluded.quantity,
            entry_price=excluded.entry_price, exit_price=excluded.exit_price,
            pnl=excluded.pnl, pnl_percent=excluded.pnl_percent,
            entry_date=excluded.entry_date, exit_date=excluded.exit_date,
            commission=excluded.commission, fees=excluded.fees, status=excluded.status,
            setup=excluded.setup, strategy=excluded.strategy, broker=excluded.broker,
            tags_json=excluded.tags_json, raw_json=excluded.raw_json,
            synced_at=datetime('now')",
        params![
            t.tt_id,
            t.symbol,
            t.side,
            t.quantity,
            t.entry_price,
            t.exit_price,
            t.pnl,
            t.pnl_percent,
            t.entry_date,
            t.exit_date,
            t.commission,
            t.fees,
            t.status,
            t.setup,
            t.strategy,
            t.broker,
            tags_json,
            raw_json,
        ],
    )?;
    Ok(())
}

/// Reconcile the whole cache against a full API fetch, in one transaction:
/// upsert every incoming trade, then delete any cached row whose id was absent
/// from the fetch. Returns `(upserted, deleted)`.
///
/// The prune is what makes an upstream deletion visible here. TradeTally is the
/// source of truth but exposes no "deleted trades" feed, so a trade the user
/// removed there can only be detected by its absence from a complete sync —
/// without this, a deleted trade stayed in the KPI cards forever.
///
/// Guard: an empty `trades` slice deletes nothing. A full page-through that
/// yields zero trades is far more likely to be an auth or transport failure than
/// a user who deleted their entire journal, and wiping the cache on that would
/// be unrecoverable without a working API.
pub fn sync_trades_bulk(conn: &mut Connection, trades: &[DashboardTrade]) -> Result<(usize, usize)> {
    let tx = conn.transaction()?;

    // Ids seen in this fetch. A temp table (rather than a giant `NOT IN (?,?,…)`)
    // keeps the delete a single statement no matter how large the journal grows.
    tx.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS tt_sync_keep (tt_id TEXT PRIMARY KEY NOT NULL);
         DELETE FROM tt_sync_keep;",
    )?;

    for t in trades {
        let tags_json = serde_json::to_string(&t.tags).unwrap_or_else(|_| "[]".into());
        let raw_json = serde_json::to_string(&t.raw).unwrap_or_else(|_| "{}".into());
        tx.execute(
            "INSERT INTO tt_trades
                (tt_id, symbol, side, quantity, entry_price, exit_price, pnl, pnl_percent,
                 entry_date, exit_date, commission, fees, status, setup, strategy, broker,
                 tags_json, raw_json, synced_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18, datetime('now'))
             ON CONFLICT(tt_id) DO UPDATE SET
                symbol=excluded.symbol, side=excluded.side, quantity=excluded.quantity,
                entry_price=excluded.entry_price, exit_price=excluded.exit_price,
                pnl=excluded.pnl, pnl_percent=excluded.pnl_percent,
                entry_date=excluded.entry_date, exit_date=excluded.exit_date,
                commission=excluded.commission, fees=excluded.fees, status=excluded.status,
                setup=excluded.setup, strategy=excluded.strategy, broker=excluded.broker,
                tags_json=excluded.tags_json, raw_json=excluded.raw_json,
                synced_at=datetime('now')",
            params![
                t.tt_id, t.symbol, t.side, t.quantity, t.entry_price, t.exit_price, t.pnl,
                t.pnl_percent, t.entry_date, t.exit_date, t.commission, t.fees, t.status,
                t.setup, t.strategy, t.broker, tags_json, raw_json,
            ],
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO tt_sync_keep (tt_id) VALUES (?1)",
            params![t.tt_id],
        )?;
    }

    let deleted = if trades.is_empty() {
        0
    } else {
        tx.execute(
            "DELETE FROM tt_trades WHERE tt_id NOT IN (SELECT tt_id FROM tt_sync_keep)",
            [],
        )?
    };

    tx.commit()?;
    Ok((trades.len(), deleted))
}

/// All cached trades, oldest first (by exit then entry date) so the frontend can
/// build cumulative curves directly.
pub fn get_all_trades(conn: &Connection) -> Result<Vec<DashboardTrade>> {
    let mut stmt = conn.prepare(
        "SELECT tt_id, symbol, side, quantity, entry_price, exit_price, pnl, pnl_percent,
                entry_date, exit_date, commission, fees, status, setup, strategy, broker,
                tags_json, raw_json
         FROM tt_trades
         ORDER BY COALESCE(exit_date, entry_date) ASC, tt_id ASC",
    )?;
    let rows = stmt.query_map([], |row| {
        let tags_json: String = row.get(16)?;
        let raw_json: String = row.get(17)?;
        Ok(DashboardTrade {
            tt_id: row.get(0)?,
            symbol: row.get(1)?,
            side: row.get(2)?,
            quantity: row.get(3)?,
            entry_price: row.get(4)?,
            exit_price: row.get(5)?,
            pnl: row.get(6)?,
            pnl_percent: row.get(7)?,
            entry_date: row.get(8)?,
            exit_date: row.get(9)?,
            commission: row.get(10)?,
            fees: row.get(11)?,
            status: row.get(12)?,
            setup: row.get(13)?,
            strategy: row.get(14)?,
            broker: row.get(15)?,
            tags: serde_json::from_str(&tags_json).unwrap_or_default(),
            raw: serde_json::from_str(&raw_json).unwrap_or(serde_json::Value::Null),
        })
    })?;
    rows.collect()
}

/// Store a local copy of a diary entry that was queued for TradeTally.
pub fn insert_diary_local(
    conn: &Connection,
    id: &str,
    entry_date: &str,
    title: &str,
    content: &str,
) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO diary_entries_local (id, entry_date, title, content)
         VALUES (?1, ?2, ?3, ?4)",
        params![id, entry_date, title, content],
    )?;
    Ok(())
}
