// Persistence for LLM analysis RESULTS (panic mean-reversion button read).
// Only the model outputs are stored — context summary + reversion verdict — never
// the prompt or the news/OHLC that fed it. Each call is appended; reads return the
// most recent row for a symbol so a re-opened zone shows the last verdict.

use rusqlite::{params, Connection, Result};

/// One persisted analysis result (latest read).
#[derive(Debug, Clone)]
pub struct LlmAnalysis {
    pub context:    Option<String>,
    pub verdict:    Option<String>,
    pub created_at: String,
}

/// Append one result. Stores only the outputs; skips a fully-empty result.
pub fn insert_result(
    conn: &Connection,
    symbol: &str,
    strategy_id: &str,
    context: Option<&str>,
    verdict: Option<&str>,
) -> Result<()> {
    if context.is_none() && verdict.is_none() {
        return Ok(());
    }
    conn.execute(
        "INSERT INTO llm_analysis (symbol, strategy_id, context, verdict)
         VALUES (?1, ?2, ?3, ?4)",
        params![symbol, strategy_id, context, verdict],
    )?;
    Ok(())
}

/// Most recent stored result for a symbol (None when never analysed).
pub fn get_latest(conn: &Connection, symbol: &str) -> Result<Option<LlmAnalysis>> {
    let mut stmt = conn.prepare(
        "SELECT context, verdict, created_at FROM llm_analysis
         WHERE symbol = ?1 ORDER BY created_at DESC, id DESC LIMIT 1",
    )?;
    let mut rows = stmt.query_map(params![symbol], |row| {
        Ok(LlmAnalysis {
            context:    row.get(0)?,
            verdict:    row.get(1)?,
            created_at: row.get(2)?,
        })
    })?;
    rows.next().transpose()
}
