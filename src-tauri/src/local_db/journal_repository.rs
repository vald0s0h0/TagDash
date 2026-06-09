use rusqlite::{params, Connection, OptionalExtension, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalEntry {
    pub trade_id:   String,
    pub symbol:     String,
    pub notes:      String,
    pub confidence: Option<i32>,
    pub tags:       Vec<String>,
    pub updated_at: String,
}

/// Upsert a journal entry. Tags are stored as a JSON array.
pub fn save(conn: &Connection, entry: &JournalEntry) -> Result<()> {
    let tags_json = serde_json::to_string(&entry.tags).unwrap_or_else(|_| "[]".into());
    conn.execute(
        "INSERT INTO journal_entries (trade_id, symbol, notes, confidence, tags_json, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))
         ON CONFLICT(trade_id) DO UPDATE SET
           symbol      = excluded.symbol,
           notes       = excluded.notes,
           confidence  = excluded.confidence,
           tags_json   = excluded.tags_json,
           updated_at  = datetime('now')",
        params![
            entry.trade_id,
            entry.symbol,
            entry.notes,
            entry.confidence,
            tags_json
        ],
    )?;
    Ok(())
}

pub fn get(conn: &Connection, trade_id: &str) -> Result<Option<JournalEntry>> {
    let mut stmt = conn.prepare(
        "SELECT trade_id, symbol, notes, confidence, tags_json, updated_at
         FROM journal_entries WHERE trade_id = ?1",
    )?;
    let result = stmt
        .query_row(params![trade_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<i32>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })
        .optional()?;

    Ok(result.map(|(trade_id, symbol, notes, confidence, tags_json, updated_at)| {
        let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();
        JournalEntry { trade_id, symbol, notes, confidence, tags, updated_at }
    }))
}
