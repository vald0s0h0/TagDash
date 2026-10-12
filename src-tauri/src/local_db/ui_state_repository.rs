// Interface state persisted by the frontend: one row per store (dashboard layout,
// hotkeys, gamepad layout, chart theme…), the value being that store's JSON as
// zustand serialises it. Opaque to the backend — it is stored, returned and packed,
// never interpreted. See `src/lib/userStorage.ts`.

use rusqlite::{params, Connection, Result};

/// Longest key / value accepted (a store's whole state is a few kilobytes).
const MAX_KEY: usize = 80;
const MAX_VALUE: usize = 4 * 1024 * 1024;

/// Every persisted store, as `(key, json)`.
pub fn all(conn: &Connection) -> Result<Vec<(String, String)>> {
    conn.prepare("SELECT key, value FROM ui_state ORDER BY key")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect()
}

pub fn set(conn: &Connection, key: &str, value: &str) -> Result<()> {
    if key.is_empty() || key.len() > MAX_KEY || value.len() > MAX_VALUE {
        return Err(rusqlite::Error::InvalidParameterName(format!("ui_state: {key}")));
    }
    conn.execute(
        "INSERT INTO ui_state (key, value, updated_at) VALUES (?1, ?2, datetime('now'))
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        params![key, value],
    )?;
    Ok(())
}

pub fn remove(conn: &Connection, key: &str) -> Result<()> {
    conn.execute("DELETE FROM ui_state WHERE key = ?1", params![key])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_db::schema;

    #[test]
    fn a_store_round_trips_and_is_replaced_in_place() {
        let conn = Connection::open_in_memory().unwrap();
        schema::migrate(&conn).unwrap();
        set(&conn, "tagdash-hotkeys", r#"{"state":{"bindings":{}},"version":4}"#).unwrap();
        set(&conn, "tagdash-hotkeys", r#"{"state":{"bindings":{"apex_arm":1}},"version":4}"#).unwrap();
        set(&conn, "tagdash-dashboard", "{}").unwrap();
        let rows = all(&conn).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].0, "tagdash-hotkeys");
        assert!(rows[1].1.contains("apex_arm"));
        remove(&conn, "tagdash-dashboard").unwrap();
        assert_eq!(all(&conn).unwrap().len(), 1);
    }

    #[test]
    fn an_oversized_value_is_refused() {
        let conn = Connection::open_in_memory().unwrap();
        schema::migrate(&conn).unwrap();
        assert!(set(&conn, "k", &"x".repeat(MAX_VALUE + 1)).is_err());
        assert!(set(&conn, "", "{}").is_err());
    }
}
