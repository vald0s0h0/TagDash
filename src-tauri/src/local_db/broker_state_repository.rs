//! Durable, account-scoped broker runtime state.
//!
//! JSON is intentional here: both objects are small (only open positions/plans),
//! and replacing the whole snapshot atomically avoids half-written trade state.

use rusqlite::{params, Connection, OptionalExtension, Result};
use serde::{de::DeserializeOwned, Serialize};

fn load<T: DeserializeOwned>(
    conn: &Connection,
    account_id: &str,
    column: &str,
) -> Result<Option<T>> {
    debug_assert!(matches!(column, "journal_json" | "protection_json"));
    let sql = format!("SELECT {column} FROM broker_runtime_state WHERE account_id=?1");
    let raw: Option<String> = conn
        .query_row(&sql, params![account_id], |r| r.get(0))
        .optional()?
        .flatten();
    Ok(raw.and_then(|s| serde_json::from_str(&s).ok()))
}

fn save<T: Serialize>(conn: &Connection, account_id: &str, column: &str, value: &T) -> Result<()> {
    debug_assert!(matches!(column, "journal_json" | "protection_json"));
    let json = serde_json::to_string(value)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    let sql = format!(
        "INSERT INTO broker_runtime_state(account_id,{column},updated_at)
         VALUES(?1,?2,datetime('now'))
         ON CONFLICT(account_id) DO UPDATE SET
         {column}=excluded.{column}, updated_at=excluded.updated_at"
    );
    conn.execute(&sql, params![account_id, json])?;
    Ok(())
}

pub fn load_journal<T: DeserializeOwned>(conn: &Connection, account_id: &str) -> Result<Option<T>> {
    load(conn, account_id, "journal_json")
}

pub fn save_journal<T: Serialize>(conn: &Connection, account_id: &str, value: &T) -> Result<()> {
    save(conn, account_id, "journal_json", value)
}

pub fn load_protection<T: DeserializeOwned>(
    conn: &Connection,
    account_id: &str,
) -> Result<Option<T>> {
    load(conn, account_id, "protection_json")
}

pub fn save_protection<T: Serialize>(conn: &Connection, account_id: &str, value: &T) -> Result<()> {
    save(conn, account_id, "protection_json", value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_snapshots_are_isolated_and_each_column_is_preserved() {
        let conn = Connection::open_in_memory().unwrap();
        crate::local_db::schema::migrate(&conn).unwrap();
        save_journal(&conn, "alpaca:paper", &vec!["AAPL"]).unwrap();
        save_protection(&conn, "alpaca:paper", &vec!["AAPL-stop"]).unwrap();
        save_journal(&conn, "alpaca:live", &vec!["TSLA"]).unwrap();
        assert_eq!(
            load_journal::<Vec<String>>(&conn, "alpaca:paper")
                .unwrap()
                .unwrap(),
            vec!["AAPL"]
        );
        assert_eq!(
            load_protection::<Vec<String>>(&conn, "alpaca:paper")
                .unwrap()
                .unwrap(),
            vec!["AAPL-stop"]
        );
        assert_eq!(
            load_journal::<Vec<String>>(&conn, "alpaca:live")
                .unwrap()
                .unwrap(),
            vec!["TSLA"]
        );
    }
}
