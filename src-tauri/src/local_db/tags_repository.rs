use rusqlite::{params, Connection, Result};

/// Replace the entire cached tag list with a fresh one from TradeTally.
pub fn replace_all(conn: &Connection, tags: &[String]) -> Result<()> {
    conn.execute("DELETE FROM tradetally_tags", [])?;
    for tag in tags {
        conn.execute(
            "INSERT OR IGNORE INTO tradetally_tags (tag, updated_at) VALUES (?1, datetime('now'))",
            params![tag],
        )?;
    }
    Ok(())
}

pub fn get_all(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT tag FROM tradetally_tags ORDER BY tag")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    rows.collect()
}

pub fn count(conn: &Connection) -> Result<i64> {
    conn.query_row("SELECT COUNT(*) FROM tradetally_tags", [], |r| r.get(0))
}
