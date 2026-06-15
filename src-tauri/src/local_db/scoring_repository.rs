// Persistence for the Panic Mean Reversion pre-open watchlist (table
// `panic_watchlist`). Written once per trading day by the watchlist engine (see
// `crate::scoring::build_and_store`, triggered by `crate::panic_watchlist`), read by
// the scanner to surface the merged two-list watchlist and by the info band.

use rusqlite::{params, Connection, Result};
use serde::{Deserialize, Serialize};

/// One persisted watchlist row — a ticker retained by one of the two rankings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreRow {
    pub symbol:        String,
    /// Which ranking retained the ticker: "BB" (Bollinger area) or "MA" (move since
    /// last SMA20 contact, ATR-normalised).
    pub list_kind:     String,
    /// The list's metric value (BB area sum, or |move|/ATR20).
    pub value:         f64,
    /// Extension direction: +1 up, −1 down, 0 none.
    pub direction:     i8,
    /// 1-based rank within its list.
    pub rank:          u32,
    /// Global ordering key (higher first) — interleaves the two lists 1-for-1 by rank.
    pub display_score: f64,
    /// Previous trading day's volume (shares); None when unknown. Shown on the card.
    pub prev_volume:   Option<i64>,
}

/// Replace the whole watchlist table atomically (one transaction). The set is small
/// (≤20 rows) and recomputed wholesale once a day.
pub fn replace_all(conn: &Connection, rows: &[ScoreRow]) -> Result<()> {
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    conn.execute_batch("BEGIN")?;
    conn.execute("DELETE FROM panic_watchlist", [])?;
    {
        let mut stmt = conn.prepare(
            "INSERT INTO panic_watchlist
                 (symbol, list_kind, value, direction, rank, display_score, prev_volume, updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        )?;
        for r in rows {
            stmt.execute(params![
                r.symbol,
                r.list_kind,
                r.value,
                r.direction as i64,
                r.rank as i64,
                r.display_score,
                r.prev_volume,
                now,
            ])?;
        }
    }
    conn.execute_batch("COMMIT")?;
    Ok(())
}

/// Map a row from the canonical SELECT column order into a `ScoreRow`.
fn map_score_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ScoreRow> {
    Ok(ScoreRow {
        symbol:        row.get(0)?,
        list_kind:     row.get(1)?,
        value:         row.get(2)?,
        direction:     row.get::<_, i64>(3)? as i8,
        rank:          row.get::<_, i64>(4)? as u32,
        display_score: row.get(5)?,
        prev_volume:   row.get(6)?,
    })
}

/// Top `n` watchlist rows in global (interleaved) order. The optional
/// `min_prev_volume` gate is kept for API compatibility — pass 0 for no filter (the
/// premarket pre-filter is the real liquidity gate now).
pub fn get_top(conn: &Connection, n: u32, min_prev_volume: i64) -> Result<Vec<ScoreRow>> {
    let mut stmt = conn.prepare(
        "SELECT symbol, list_kind, value, direction, rank, display_score, prev_volume
         FROM panic_watchlist
         WHERE (?2 = 0 OR (prev_volume IS NOT NULL AND prev_volume > ?2))
         ORDER BY display_score DESC
         LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![n, min_prev_volume], map_score_row)?;
    rows.collect()
}

/// One symbol's watchlist row (None when not on today's list). Backs the info band.
pub fn get_one(conn: &Connection, symbol: &str) -> Result<Option<ScoreRow>> {
    let mut stmt = conn.prepare(
        "SELECT symbol, list_kind, value, direction, rank, display_score, prev_volume
         FROM panic_watchlist WHERE symbol=?1",
    )?;
    let mut rows = stmt.query_map(params![symbol], map_score_row)?;
    rows.next().transpose()
}

/// Count of watchlist rows.
pub fn count(conn: &Connection) -> Result<i64> {
    conn.query_row("SELECT COUNT(*) FROM panic_watchlist", [], |r| r.get(0))
}
