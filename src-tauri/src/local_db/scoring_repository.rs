// Persistence for the daily mean-reversion scores (Panic Mean Reversion pre-open
// screener). Written once per day by the startup pipeline (see `scoring`), read
// by the scanner to build the top-30 watchlist.

use rusqlite::{params, Connection, Result};
use serde::{Deserialize, Serialize};

/// One persisted scoring row. Mirrors `scoring::MeanReversionScore` but is the
/// DB/bridge-facing shape (Serialize for the debug command).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreRow {
    pub symbol:          String,
    /// Cross-sectional percent-rank momentum (0..100). DIAGNOSTIC ONLY (no longer
    /// part of display/ranking — it saturated and over-weighted noisy tails).
    pub pr_score:        f64,
    pub pr_best_days:    u8,
    pub bb_event_score:  f64,
    pub bb_best_horizon: u8,
    /// Parabolic component P (0..1).
    pub parabolic_score: f64,
    /// Volume component V (0..1) — log-scaled previous-day dollar volume.
    pub volume_score:    f64,
    /// Run component R (0..1).
    pub run_score:       f64,
    /// Length of the current same-colour candle run (days).
    pub run_len:         u8,
    /// Run direction: +1 bullish, −1 bearish, 0 none.
    pub run_dir:         i8,
    /// Continuous composite (0..100) — the ranking/display score.
    pub display_score:   f64,
    /// "MR" — composite kind tag.
    pub score_kind:      String,
    /// Previous trading day's volume (shares); None when unknown.
    pub prev_volume:     Option<i64>,
}

/// Replace the whole scores table atomically (one transaction). The set is small
/// relative to the universe and recomputed wholesale once a day, so a clear +
/// bulk insert keeps it simple and avoids stale rows for delisted symbols.
pub fn replace_all(conn: &Connection, rows: &[ScoreRow]) -> Result<()> {
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    conn.execute_batch("BEGIN")?;
    conn.execute("DELETE FROM mean_reversion_scores", [])?;
    {
        let mut stmt = conn.prepare(
            "INSERT INTO mean_reversion_scores
                 (symbol, pr_score, pr_best_days, bb_event_score, bb_best_horizon,
                  parabolic_score, volume_score, run_score, run_len, run_dir,
                  display_score, score_kind, prev_volume, updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
        )?;
        for r in rows {
            stmt.execute(params![
                r.symbol,
                r.pr_score,
                r.pr_best_days as i64,
                r.bb_event_score,
                r.bb_best_horizon as i64,
                r.parabolic_score,
                r.volume_score,
                r.run_score,
                r.run_len as i64,
                r.run_dir as i64,
                r.display_score,
                r.score_kind,
                r.prev_volume,
                now,
            ])?;
        }
    }
    conn.execute_batch("COMMIT")?;
    Ok(())
}

/// Map a row from the canonical SELECT column order into a `ScoreRow`. Both
/// `get_top` and `get_one` select the same columns in this order.
fn map_score_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ScoreRow> {
    Ok(ScoreRow {
        symbol:          row.get(0)?,
        pr_score:        row.get(1)?,
        pr_best_days:    row.get::<_, i64>(2)? as u8,
        bb_event_score:  row.get(3)?,
        bb_best_horizon: row.get::<_, i64>(4)? as u8,
        parabolic_score: row.get(5)?,
        volume_score:    row.get(6)?,
        run_score:       row.get(7)?,
        run_len:         row.get::<_, i64>(8)? as u8,
        run_dir:         row.get::<_, i64>(9)? as i8,
        display_score:   row.get(10)?,
        score_kind:      row.get(11)?,
        prev_volume:     row.get(12)?,
    })
}

/// Top `n` tickers by display score (highest first), keeping only those whose
/// previous-day volume exceeds `min_prev_volume` (shares). Ties on the score are
/// broken by previous-day volume (highest first). Pass `min_prev_volume = 0` for
/// no volume filter.
pub fn get_top(conn: &Connection, n: u32, min_prev_volume: i64) -> Result<Vec<ScoreRow>> {
    let mut stmt = conn.prepare(
        "SELECT symbol, pr_score, pr_best_days, bb_event_score, bb_best_horizon,
                parabolic_score, volume_score, run_score, run_len, run_dir,
                display_score, score_kind, prev_volume
         FROM mean_reversion_scores
         WHERE prev_volume IS NOT NULL AND prev_volume > ?2
         ORDER BY display_score DESC, prev_volume DESC
         LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![n, min_prev_volume], map_score_row)?;
    rows.collect()
}

/// One symbol's scores (None when unscored). Used for the per-zone info band.
pub fn get_one(conn: &Connection, symbol: &str) -> Result<Option<ScoreRow>> {
    let mut stmt = conn.prepare(
        "SELECT symbol, pr_score, pr_best_days, bb_event_score, bb_best_horizon,
                parabolic_score, volume_score, run_score, run_len, run_dir,
                display_score, score_kind, prev_volume
         FROM mean_reversion_scores WHERE symbol=?1",
    )?;
    let mut rows = stmt.query_map(params![symbol], map_score_row)?;
    rows.next().transpose()
}

/// Count of scored symbols.
pub fn count(conn: &Connection) -> Result<i64> {
    conn.query_row("SELECT COUNT(*) FROM mean_reversion_scores", [], |r| r.get(0))
}
