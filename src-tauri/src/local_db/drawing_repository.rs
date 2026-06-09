// Persistence for user chart drawings (trend lines + text annotations), keyed by
// ticker so they reappear on every chart/zone showing that symbol and survive
// restarts. A 'line' uses both points; a 'text' uses the first point + `text`.

use rusqlite::{params, Connection, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Drawing {
    pub id:     String,
    pub symbol: String,
    pub kind:   String, // "line" | "text"
    pub t1:     f64,
    pub p1:     f64,
    pub t2:     Option<f64>,
    pub p2:     Option<f64>,
    pub text:   Option<String>,
}

pub fn insert(conn: &Connection, d: &Drawing) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO chart_drawings (id, symbol, kind, t1, p1, t2, p2, text)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![d.id, d.symbol, d.kind, d.t1, d.p1, d.t2, d.p2, d.text],
    )?;
    Ok(())
}

pub fn get_for_symbol(conn: &Connection, symbol: &str) -> Result<Vec<Drawing>> {
    let mut stmt = conn.prepare(
        "SELECT id, symbol, kind, t1, p1, t2, p2, text
         FROM chart_drawings WHERE symbol = ?1 ORDER BY created_at ASC, rowid ASC",
    )?;
    let rows = stmt.query_map(params![symbol], |row| {
        Ok(Drawing {
            id:     row.get(0)?,
            symbol: row.get(1)?,
            kind:   row.get(2)?,
            t1:     row.get(3)?,
            p1:     row.get(4)?,
            t2:     row.get(5)?,
            p2:     row.get(6)?,
            text:   row.get(7)?,
        })
    })?;
    rows.collect()
}

pub fn delete(conn: &Connection, id: &str) -> Result<()> {
    conn.execute("DELETE FROM chart_drawings WHERE id = ?1", params![id])?;
    Ok(())
}
