// Usage log: one row per user action (button click, hotkey, gamepad command).
// Raw and append-only — the counting ("most used", "by hour") is done at read
// time, so nothing is lost to an aggregation chosen too early. Written in batches
// by the frontend tracker (`src/lib/usageTracker.ts`); never read on a live path.

use chrono::{TimeZone, Utc};
use rusqlite::{params, Connection, Result};
use serde::Deserialize;

use crate::time::{et_date, et_minutes};

/// Longest text kept per column; the frontend already trims, this guards the DB.
const MAX_TEXT: usize = 160;

/// One action as sent by the frontend. The instant is stamped there, when the
/// action happens — the batch reaches the backend a moment later.
#[derive(Debug, Clone, Deserialize)]
pub struct UsageEventInput {
    /// Real clock, Unix milliseconds.
    pub ts_ms: i64,
    /// The machine's UTC offset in minutes (east positive), for local wall time.
    pub tz_offset_min: i32,
    /// click | change | hotkey | gamepad | key
    pub source: String,
    /// What was triggered: a command id, or the control's label.
    pub action: String,
    /// Where: screen region, dialog, tab, menu.
    pub context: String,
    /// How: the element kind, the key chord, the pad layer and button.
    pub input: Option<String>,
    /// The control's tooltip when it says more than its label.
    pub hint: Option<String>,
    /// Top-level view (and trading session) shown at that moment.
    pub view: String,
    /// Tauri window label (main | attention).
    pub window: String,
}

fn clip(s: &str) -> String {
    s.chars().take(MAX_TEXT).collect()
}

/// Append a batch in one transaction. `replay` and `app_version` are the same for
/// the whole batch: a batch spans well under a second.
pub fn insert_batch(
    conn: &Connection,
    events: &[UsageEventInput],
    replay: bool,
    app_version: &str,
) -> Result<()> {
    if events.is_empty() {
        return Ok(());
    }
    let tx = conn.unchecked_transaction()?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO usage_events
               (ts_ms, et_date, et_minute, local_minute, source, action, context,
                input, hint, view, window, replay, app_version)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        )?;
        for e in events {
            // Always the real clock: this is when the user acted, even while a
            // Market Replay simulates another day.
            let Some(instant) = Utc.timestamp_millis_opt(e.ts_ms).single() else {
                continue;
            };
            let local_minute =
                (e.ts_ms.div_euclid(60_000) + i64::from(e.tz_offset_min)).rem_euclid(1440);
            stmt.execute(params![
                e.ts_ms,
                et_date(instant),
                et_minutes(instant),
                local_minute,
                clip(&e.source),
                clip(&e.action),
                clip(&e.context),
                e.input.as_deref().map(clip),
                e.hint.as_deref().map(clip),
                clip(&e.view),
                clip(&e.window),
                replay,
                app_version,
            ])?;
        }
    }
    tx.commit()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_db::schema;

    fn event(ts_ms: i64, tz_offset_min: i32) -> UsageEventInput {
        UsageEventInput {
            ts_ms,
            tz_offset_min,
            source: "click".into(),
            action: "Market Replay".into(),
            context: "left-rail".into(),
            input: Some("button".into()),
            hint: None,
            view: "trading/premarket".into(),
            window: "main".into(),
        }
    }

    #[test]
    fn stamps_eastern_and_local_time() {
        let conn = Connection::open_in_memory().unwrap();
        schema::migrate(&conn).unwrap();
        // 2026-10-12 13:45:00 UTC = 09:45 EDT = 15:45 in Paris (UTC+2).
        insert_batch(&conn, &[event(1_791_812_700_000, 120)], true, "0.2.12").unwrap();

        let row: (String, i64, i64, bool, String) = conn
            .query_row(
                "SELECT et_date, et_minute, local_minute, replay, app_version FROM usage_events",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(row, ("2026-10-12".into(), 9 * 60 + 45, 15 * 60 + 45, true, "0.2.12".into()));
    }

    #[test]
    fn clips_oversized_text() {
        let conn = Connection::open_in_memory().unwrap();
        schema::migrate(&conn).unwrap();
        let mut e = event(1_791_812_700_000, 0);
        e.action = "x".repeat(MAX_TEXT * 3);
        insert_batch(&conn, &[e], false, "0.2.12").unwrap();

        let len: i64 = conn
            .query_row("SELECT length(action) FROM usage_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(len, MAX_TEXT as i64);
    }
}
