// Pre-open screener user state: DISMISSALS and PINS.
//
// The two are deliberately scoped differently, because they mean opposite things:
//
//   • DISMISSAL — "not today". Keyed by (symbol, ET day), pruned on read, so a
//     dismissed ticker reappears the next trading day.
//   • PIN — "keep showing me this until I say otherwise". Keyed by
//     (symbol, simulated), with NO day column: it persists across days until the user
//     removes it.
//
// The `simulated` flag separates pins placed while a Market Replay drives the clock
// from live ones. Without it a backtest of some February day would pollute the real
// watchlist (and vice versa). Note the asymmetry with dismissals, which DO prune by
// day: `crate::time::now()` follows the simulated clock, so the dismissal prune
// (`WHERE day <> today`) already wipes the live day's dismissals when a replay
// starts. That behaviour is pre-existing and harmless for a same-day-only feature —
// but it must never be replicated for pins, where losing user state would be a
// visible bug.

use rusqlite::{params, Connection, Result};

// ─── Dismissals (day-scoped) ───────────────────────────────────────────────────

pub fn dismiss(conn: &Connection, symbol: &str, day: &str) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO screener_dismissals (symbol, day) VALUES (?1, ?2)",
        params![symbol, day],
    )?;
    Ok(())
}

/// Symbols dismissed on `day`. Other days are pruned on read, which is what makes
/// dismissals reset each trading day without a scheduler.
pub fn dismissals(conn: &Connection, day: &str) -> Vec<String> {
    let _ = conn.execute(
        "DELETE FROM screener_dismissals WHERE day <> ?1",
        params![day],
    );
    let Ok(mut stmt) = conn.prepare("SELECT symbol FROM screener_dismissals WHERE day = ?1") else {
        return Vec::new();
    };
    stmt.query_map(params![day], |r| r.get::<_, String>(0))
        .map(|it| it.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
}

// ─── Pins (persistent) ─────────────────────────────────────────────────────────

pub fn pin(conn: &Connection, symbol: &str, simulated: bool) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO screener_pins (symbol, simulated) VALUES (?1, ?2)",
        params![symbol, simulated as i64],
    )?;
    Ok(())
}

pub fn unpin(conn: &Connection, symbol: &str, simulated: bool) -> Result<()> {
    conn.execute(
        "DELETE FROM screener_pins WHERE symbol = ?1 AND simulated = ?2",
        params![symbol, simulated as i64],
    )?;
    Ok(())
}

/// Pinned symbols for the current mode. Never pruned — a pin lives until removed.
pub fn pins(conn: &Connection, simulated: bool) -> Vec<String> {
    let Ok(mut stmt) =
        conn.prepare("SELECT symbol FROM screener_pins WHERE simulated = ?1 ORDER BY created_at")
    else {
        return Vec::new();
    };
    stmt.query_map(params![simulated as i64], |r| r.get::<_, String>(0))
        .map(|it| it.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
}

// ─── Ratings (persistent, 1..3) ────────────────────────────────────────────────
//
// The user's own read of a ticker's potential, kept purely as a memo. Nothing in the
// platform consumes it: it does not rank the screener, gate a strategy, or size a
// position. Same lifetime rules as a pin — no day column, isolated per mode.

/// Highest rating a card can carry (three dots).
pub const MAX_RATING: i64 = 3;

/// Set a rating, or clear it when `rating` is 0. Out-of-range values are clamped
/// rather than rejected: a stray value must never leave the card in a state the UI
/// cannot draw.
pub fn set_rating(conn: &Connection, symbol: &str, simulated: bool, rating: i64) -> Result<()> {
    if rating <= 0 {
        conn.execute(
            "DELETE FROM screener_ratings WHERE symbol = ?1 AND simulated = ?2",
            params![symbol, simulated as i64],
        )?;
        return Ok(());
    }
    conn.execute(
        "INSERT INTO screener_ratings (symbol, simulated, rating, updated_at)
         VALUES (?1, ?2, ?3, datetime('now'))
         ON CONFLICT(symbol, simulated) DO UPDATE SET
             rating = excluded.rating, updated_at = excluded.updated_at",
        params![symbol, simulated as i64, rating.min(MAX_RATING)],
    )?;
    Ok(())
}

/// Every rating for the current mode, as (symbol, 1..=MAX_RATING).
pub fn ratings(conn: &Connection, simulated: bool) -> Vec<(String, i64)> {
    let Ok(mut stmt) =
        conn.prepare("SELECT symbol, rating FROM screener_ratings WHERE simulated = ?1")
    else {
        return Vec::new();
    };
    stmt.query_map(params![simulated as i64], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
    })
    .map(|it| it.filter_map(|r| r.ok()).collect())
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::local_db::schema::migrate(&conn).unwrap();
        conn
    }

    #[test]
    fn dismissals_reset_on_the_next_day() {
        let conn = db();
        dismiss(&conn, "AAA", "2026-06-15").unwrap();
        assert_eq!(dismissals(&conn, "2026-06-15"), vec!["AAA"]);
        // Reading another day prunes the old one — the ticker comes back.
        assert!(dismissals(&conn, "2026-06-16").is_empty());
        assert!(dismissals(&conn, "2026-06-15").is_empty());
    }

    #[test]
    fn pins_survive_day_changes() {
        let conn = db();
        pin(&conn, "MTEN", false).unwrap();
        // Reading dismissals for other days must not disturb pins.
        let _ = dismissals(&conn, "2026-06-16");
        let _ = dismissals(&conn, "2026-07-01");
        assert_eq!(pins(&conn, false), vec!["MTEN"]);
    }

    #[test]
    fn live_and_replay_pins_are_isolated() {
        let conn = db();
        pin(&conn, "LIVE", false).unwrap();
        pin(&conn, "SIM", true).unwrap();
        assert_eq!(pins(&conn, false), vec!["LIVE"]);
        assert_eq!(pins(&conn, true), vec!["SIM"]);
        // The same symbol can be pinned in both modes independently.
        pin(&conn, "LIVE", true).unwrap();
        assert_eq!(pins(&conn, true).len(), 2);
        unpin(&conn, "LIVE", true).unwrap();
        assert_eq!(
            pins(&conn, false),
            vec!["LIVE"],
            "unpinning in replay must not touch live"
        );
    }

    #[test]
    fn a_rating_survives_day_changes_like_a_pin() {
        let conn = db();
        set_rating(&conn, "MTEN", false, 3).unwrap();
        // Day-scoped pruning elsewhere must not touch it.
        let _ = dismissals(&conn, "2026-06-16");
        let _ = dismissals(&conn, "2026-07-01");
        assert_eq!(ratings(&conn, false), vec![("MTEN".to_string(), 3)]);
    }

    #[test]
    fn rating_zero_clears_the_row_rather_than_storing_a_zero() {
        let conn = db();
        set_rating(&conn, "AAA", false, 2).unwrap();
        assert_eq!(ratings(&conn, false).len(), 1);
        set_rating(&conn, "AAA", false, 0).unwrap();
        assert!(ratings(&conn, false).is_empty(), "clearing removes the row");
        // Clearing something unrated is not an error.
        set_rating(&conn, "NOPE", false, 0).unwrap();
    }

    #[test]
    fn a_rating_is_replaced_not_duplicated_and_stays_in_range() {
        let conn = db();
        set_rating(&conn, "AAA", false, 1).unwrap();
        set_rating(&conn, "AAA", false, 3).unwrap();
        assert_eq!(ratings(&conn, false), vec![("AAA".to_string(), 3)]);
        // A value the UI could never draw is clamped, never stored as-is.
        set_rating(&conn, "AAA", false, 99).unwrap();
        assert_eq!(ratings(&conn, false), vec![("AAA".to_string(), MAX_RATING)]);
    }

    #[test]
    fn live_and_replay_ratings_are_isolated() {
        let conn = db();
        set_rating(&conn, "AAA", false, 1).unwrap();
        set_rating(&conn, "AAA", true, 3).unwrap();
        assert_eq!(ratings(&conn, false), vec![("AAA".to_string(), 1)]);
        assert_eq!(ratings(&conn, true), vec![("AAA".to_string(), 3)]);
    }

    #[test]
    fn pin_is_idempotent_and_unpin_is_forgiving() {
        let conn = db();
        pin(&conn, "AAA", false).unwrap();
        pin(&conn, "AAA", false).unwrap();
        assert_eq!(pins(&conn, false).len(), 1);
        unpin(&conn, "NOPE", false).unwrap(); // no such pin — not an error
        unpin(&conn, "AAA", false).unwrap();
        assert!(pins(&conn, false).is_empty());
    }
}
