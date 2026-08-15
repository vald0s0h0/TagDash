// Persistence for the Panic Mean Reversion pre-open watchlist (`panic_watchlist_v2`)
// and its per-day build ledger (`panic_watchlist_build`).
//
// Everything here is DAY-SCOPED. That is the whole point: a list built for day X is
// invisible while the app clock sits on day Y, so a Market Replay can never surface
// the live day's list (or vice versa), and a slow build that finishes after the clock
// moved degrades to "shows nothing" rather than "shows the wrong day".
//
// Written by `crate::scoring::build_and_store` through the single entry point
// `crate::panic_watchlist::ensure_day`; read by the scanner (screener rows) and the
// chart info band.

use rusqlite::{params, Connection, Result};
use serde::{Deserialize, Serialize};

/// How many distinct days of watchlist rows to keep on disk.
const KEEP_DAYS: usize = 30;

/// One persisted watchlist row: a ticker whose panic score cleared the thresholds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreRow {
    pub symbol: String,
    /// ET trading day this row was computed FOR.
    pub day: String,
    /// Extension score: sigma × sqrt(ATR expansion) × bounded stability/flow quality.
    pub score: f64,
    /// Which window produced the score; 0 means current premarket.
    pub window_days: u8,
    /// Move direction over that window: +1 up, −1 down.
    pub direction: i8,
    /// |return| normalised by the pre-window volatility of returns — scale-free.
    pub sigma: f64,
    /// Window true range / pre-window ATR20 — the volatility expansion.
    pub atr_exp: f64,
    /// Size bucket used for the neutral ranking; None when the market cap is unknown.
    pub cap_bin: Option<String>,
    /// Previous trading day's volume (shares); None when unknown. Shown on the card.
    pub prev_volume: Option<i64>,
    /// Origin of the extension: `multiday` or `premarket`.
    pub setup_kind: String,
    /// Signed move that produced the extension score, in percent.
    pub move_pct: f64,
    /// 0..100: how quiet and regular the pre-shock regime was.
    pub stability_score: f64,
    /// Move-window volume / pre-shock median volume (premarket: PM / ADV).
    pub volume_expansion: Option<f64>,
    /// |net path| / sum(|steps|), 0..1. High means one-way/forced-looking flow.
    pub directional_efficiency: Option<f64>,
    /// Signed consecutive bars/days ending at the observation.
    pub consecutive_bars: Option<i32>,
}

/// One row per day the build ran — the durable replacement for the old in-memory
/// `built_day` + `app_config` marker.
#[derive(Debug, Clone)]
pub struct BuildLedger {
    pub day: String,
    /// ET minutes on the APP clock when the build completed (so a replay records the
    /// simulated minute, which is what the 09:25 promotion rule compares).
    pub built_min: u32,
    pub row_count: usize,
    pub attempts: u32,
    /// Produced while a Market Replay drove the clock.
    pub simulated: bool,
    pub generation: u64,
    /// 0 rows because inputs were missing (retry) rather than no survivors (final).
    pub retryable: bool,
}

// ─── Watchlist rows ────────────────────────────────────────────────────────────

const COLS: &str =
    "symbol, day, score, window_days, direction, sigma, atr_exp, cap_bin, prev_volume,
     setup_kind, move_pct, stability_score, volume_expansion,
     directional_efficiency, consecutive_bars";

fn map_score_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ScoreRow> {
    Ok(ScoreRow {
        symbol: row.get(0)?,
        day: row.get(1)?,
        score: row.get(2)?,
        window_days: row.get::<_, i64>(3)? as u8,
        direction: row.get::<_, i64>(4)? as i8,
        sigma: row.get(5)?,
        atr_exp: row.get(6)?,
        cap_bin: row.get(7)?,
        prev_volume: row.get(8)?,
        setup_kind: row.get(9)?,
        move_pct: row.get(10)?,
        stability_score: row.get(11)?,
        volume_expansion: row.get(12)?,
        directional_efficiency: row.get(13)?,
        consecutive_bars: row.get(14)?,
    })
}

/// Replace the rows for ONE day atomically. Other days are deliberately untouched —
/// that retention is what lets the live list survive a replay of a different day with
/// no rebuild on stop. Uses an RAII transaction so a mid-way failure rolls back
/// instead of leaving the shared long-lived connection inside an open transaction.
pub fn replace_for_day(conn: &Connection, day: &str, rows: &[ScoreRow]) -> Result<()> {
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM panic_watchlist_v2 WHERE day = ?1",
        params![day],
    )?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO panic_watchlist_v2
                 (day, symbol, score, window_days, direction, sigma, atr_exp,
                  cap_bin, prev_volume, setup_kind, move_pct, stability_score,
                  volume_expansion, directional_efficiency, consecutive_bars, updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
        )?;
        for r in rows {
            stmt.execute(params![
                day,
                r.symbol,
                r.score,
                r.window_days as i64,
                r.direction as i64,
                r.sigma,
                r.atr_exp,
                r.cap_bin,
                r.prev_volume,
                r.setup_kind,
                r.move_pct,
                r.stability_score,
                r.volume_expansion,
                r.directional_efficiency,
                r.consecutive_bars,
                now,
            ])?;
        }
    }
    tx.commit()
}

/// Top `n` rows for `day`, strongest score first.
pub fn get_top(conn: &Connection, day: &str, n: u32) -> Result<Vec<ScoreRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLS} FROM panic_watchlist_v2 WHERE day = ?1 ORDER BY score DESC LIMIT ?2"
    ))?;
    let rows = stmt.query_map(params![day, n], map_score_row)?;
    rows.collect()
}

/// One symbol's row for `day` (None when it isn't on that day's list).
pub fn get_one(conn: &Connection, day: &str, symbol: &str) -> Result<Option<ScoreRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLS} FROM panic_watchlist_v2 WHERE day = ?1 AND symbol = ?2"
    ))?;
    let mut rows = stmt.query_map(params![day, symbol], map_score_row)?;
    rows.next().transpose()
}

pub fn count_for_day(conn: &Connection, day: &str) -> Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM panic_watchlist_v2 WHERE day = ?1",
        params![day],
        |r| r.get(0),
    )
}

/// Drop one day's rows (replay start/stop purge).
pub fn delete_day(conn: &Connection, day: &str) -> Result<usize> {
    conn.execute(
        "DELETE FROM panic_watchlist_v2 WHERE day = ?1",
        params![day],
    )
}

/// Keep only the most recently WRITTEN `KEEP_DAYS` days. Ordered by `updated_at`
/// (real wall clock) rather than by `day`, so replaying an old date doesn't
/// immediately prune itself away.
pub fn prune(conn: &Connection) -> Result<usize> {
    conn.execute(
        "DELETE FROM panic_watchlist_v2 WHERE day NOT IN (
             SELECT day FROM panic_watchlist_v2
             GROUP BY day ORDER BY MAX(updated_at) DESC LIMIT ?1
         )",
        params![KEEP_DAYS as i64],
    )
}

// ─── Build ledger ──────────────────────────────────────────────────────────────

pub fn get_build(conn: &Connection, day: &str) -> Result<Option<BuildLedger>> {
    let mut stmt = conn.prepare(
        "SELECT day, built_min, row_count, attempts, simulated, generation, retryable
         FROM panic_watchlist_build WHERE day = ?1",
    )?;
    let mut rows = stmt.query_map(params![day], |r| {
        Ok(BuildLedger {
            day: r.get(0)?,
            built_min: r.get::<_, i64>(1)? as u32,
            row_count: r.get::<_, i64>(2)?.max(0) as usize,
            attempts: r.get::<_, i64>(3)?.max(0) as u32,
            simulated: r.get::<_, i64>(4)? != 0,
            generation: r.get::<_, i64>(5)?.max(0) as u64,
            retryable: r.get::<_, i64>(6)? != 0,
        })
    })?;
    rows.next().transpose()
}

pub fn record_build(conn: &Connection, l: &BuildLedger) -> Result<()> {
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    conn.execute(
        "INSERT INTO panic_watchlist_build
             (day, built_min, row_count, attempts, simulated, generation, retryable, updated_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
         ON CONFLICT(day) DO UPDATE SET
             built_min=excluded.built_min, row_count=excluded.row_count,
             attempts=excluded.attempts, simulated=excluded.simulated,
             generation=excluded.generation, retryable=excluded.retryable,
             updated_at=excluded.updated_at",
        params![
            l.day,
            l.built_min as i64,
            l.row_count as i64,
            l.attempts as i64,
            l.simulated as i64,
            l.generation as i64,
            l.retryable as i64,
            now,
        ],
    )?;
    Ok(())
}

pub fn delete_build(conn: &Connection, day: &str) -> Result<usize> {
    conn.execute(
        "DELETE FROM panic_watchlist_build WHERE day = ?1",
        params![day],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::local_db::schema::migrate(&conn).unwrap();
        conn
    }

    fn row(symbol: &str, day: &str, score: f64) -> ScoreRow {
        ScoreRow {
            symbol: symbol.into(),
            day: day.into(),
            score,
            window_days: 2,
            direction: -1,
            sigma: 4.0,
            atr_exp: 2.0,
            cap_bin: Some("<300M".into()),
            prev_volume: Some(1_000_000),
            setup_kind: "multiday".into(),
            move_pct: -18.0,
            stability_score: 80.0,
            volume_expansion: Some(4.0),
            directional_efficiency: Some(0.9),
            consecutive_bars: Some(-3),
        }
    }

    #[test]
    fn rows_are_isolated_per_day() {
        let conn = db();
        replace_for_day(&conn, "2026-06-15", &[row("AAA", "2026-06-15", 9.0)]).unwrap();
        replace_for_day(&conn, "2026-06-16", &[row("BBB", "2026-06-16", 8.0)]).unwrap();

        let d15 = get_top(&conn, "2026-06-15", 10).unwrap();
        assert_eq!(d15.len(), 1);
        assert_eq!(d15[0].symbol, "AAA");
        // The other day's list must be untouched — this is what lets the live list
        // survive a replay of a different day.
        assert_eq!(count_for_day(&conn, "2026-06-16").unwrap(), 1);
        assert!(get_one(&conn, "2026-06-16", "AAA").unwrap().is_none());
    }

    #[test]
    fn replace_only_clears_its_own_day() {
        let conn = db();
        replace_for_day(&conn, "2026-06-15", &[row("AAA", "2026-06-15", 9.0)]).unwrap();
        replace_for_day(&conn, "2026-06-16", &[row("BBB", "2026-06-16", 8.0)]).unwrap();
        replace_for_day(&conn, "2026-06-16", &[]).unwrap(); // legitimately empty day
        assert_eq!(count_for_day(&conn, "2026-06-16").unwrap(), 0);
        assert_eq!(count_for_day(&conn, "2026-06-15").unwrap(), 1);
    }

    #[test]
    fn ledger_round_trips_and_upserts() {
        let conn = db();
        assert!(get_build(&conn, "2026-06-15").unwrap().is_none());
        let mut l = BuildLedger {
            day: "2026-06-15".into(),
            built_min: 565,
            row_count: 0,
            attempts: 1,
            simulated: true,
            generation: 7,
            retryable: true,
        };
        record_build(&conn, &l).unwrap();
        let got = get_build(&conn, "2026-06-15").unwrap().unwrap();
        assert_eq!((got.built_min, got.row_count, got.attempts), (565, 0, 1));
        assert!(got.simulated && got.retryable);
        assert_eq!(got.generation, 7);

        l.attempts = 2;
        l.row_count = 12;
        l.retryable = false;
        record_build(&conn, &l).unwrap();
        let got = get_build(&conn, "2026-06-15").unwrap().unwrap();
        assert_eq!((got.attempts, got.row_count), (2, 12));
        assert!(!got.retryable);
    }

    #[test]
    fn delete_day_clears_rows_and_ledger_independently() {
        let conn = db();
        replace_for_day(&conn, "2026-06-15", &[row("AAA", "2026-06-15", 9.0)]).unwrap();
        record_build(
            &conn,
            &BuildLedger {
                day: "2026-06-15".into(),
                built_min: 565,
                row_count: 1,
                attempts: 1,
                simulated: true,
                generation: 1,
                retryable: false,
            },
        )
        .unwrap();
        delete_day(&conn, "2026-06-15").unwrap();
        delete_build(&conn, "2026-06-15").unwrap();
        assert_eq!(count_for_day(&conn, "2026-06-15").unwrap(), 0);
        assert!(get_build(&conn, "2026-06-15").unwrap().is_none());
    }
}
