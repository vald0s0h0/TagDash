// Persistence for user chart drawings (trend lines + text annotations), keyed by
// ticker so they reappear on every chart/zone showing that symbol and survive
// restarts. A 'line' uses both points; a 'text' uses the first point + `text`.

use rusqlite::{params, Connection, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Drawing {
    pub id:     String,
    pub symbol: String,
    pub kind:   String, // "line" | "text" | "emoji"
    pub t1:     f64,
    pub p1:     f64,
    pub t2:     Option<f64>,
    pub p2:     Option<f64>,
    pub text:   Option<String>,
    /// "intraday" (shown only on intraday panes) | "daily" (daily pane only).
    #[serde(default = "default_scope")]
    pub scope:  String,
    // Style — None means "use the renderer's default".
    #[serde(default)]
    pub color:      Option<String>,
    #[serde(default)]
    pub opacity:    Option<f64>,
    #[serde(default)]
    pub width:      Option<f64>,
    #[serde(default)]
    pub line_style: Option<String>, // "solid" | "dashed" | "dotted"
    #[serde(default)]
    pub font_size:  Option<f64>,
}

fn default_scope() -> String { "intraday".to_string() }

/// Insert OR replace — used for both create and update (style/position edits).
pub fn insert(conn: &Connection, d: &Drawing) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO chart_drawings
             (id, symbol, kind, t1, p1, t2, p2, text, scope, color, opacity, width, line_style, font_size)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            d.id, d.symbol, d.kind, d.t1, d.p1, d.t2, d.p2, d.text,
            d.scope, d.color, d.opacity, d.width, d.line_style, d.font_size,
        ],
    )?;
    Ok(())
}

pub fn get_for_symbol(conn: &Connection, symbol: &str) -> Result<Vec<Drawing>> {
    let mut stmt = conn.prepare(
        "SELECT id, symbol, kind, t1, p1, t2, p2, text, scope, color, opacity, width, line_style, font_size
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
            scope:  row.get::<_, Option<String>>(8)?.unwrap_or_else(default_scope),
            color:      row.get(9)?,
            opacity:    row.get(10)?,
            width:      row.get(11)?,
            line_style: row.get(12)?,
            font_size:  row.get(13)?,
        })
    })?;
    rows.collect()
}

pub fn delete(conn: &Connection, id: &str) -> Result<()> {
    conn.execute("DELETE FROM chart_drawings WHERE id = ?1", params![id])?;
    Ok(())
}
