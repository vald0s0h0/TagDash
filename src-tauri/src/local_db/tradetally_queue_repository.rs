use rusqlite::{params, Connection, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncQueueRow {
    pub event_id: String,
    pub timestamp: String,
    pub trade_id: String,
    pub symbol: String,
    pub event_type: String,
    pub endpoint: String,
    pub payload_summary: String,
    pub status: String,
    pub error_message: Option<String>,
    pub attempts: u32,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SyncQueueStatus {
    pub pending: u32,
    pub success: u32,
    pub failed: u32,
    pub recent: Vec<SyncQueueRow>,
}

pub fn enqueue(conn: &Connection, row: &SyncQueueRow) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO tradetally_sync_queue
             (event_id,timestamp,trade_id,symbol,event_type,endpoint,payload,status,created_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,'pending',?8)",
        params![
            row.event_id, row.timestamp, row.trade_id, row.symbol,
            row.event_type, row.endpoint, row.payload_summary, row.created_at,
        ],
    )?;
    Ok(())
}

pub fn mark_success(conn: &Connection, event_id: &str) -> Result<()> {
    conn.execute(
        "UPDATE tradetally_sync_queue SET status='success' WHERE event_id=?1",
        params![event_id],
    )?;
    Ok(())
}

pub fn mark_failed(conn: &Connection, event_id: &str, error: &str) -> Result<()> {
    conn.execute(
        "UPDATE tradetally_sync_queue
         SET status='failed', error_message=?2, attempts=attempts+1
         WHERE event_id=?1",
        params![event_id, error],
    )?;
    Ok(())
}

pub fn get_pending(conn: &Connection) -> Result<Vec<SyncQueueRow>> {
    get_by_status(conn, "pending", 100)
}

pub fn get_status(conn: &Connection, recent_limit: u32) -> Result<SyncQueueStatus> {
    let pending: u32 = conn.query_row(
        "SELECT COUNT(*) FROM tradetally_sync_queue WHERE status='pending'",
        [],
        |r| r.get(0),
    )?;
    let success: u32 = conn.query_row(
        "SELECT COUNT(*) FROM tradetally_sync_queue WHERE status='success'",
        [],
        |r| r.get(0),
    )?;
    let failed: u32 = conn.query_row(
        "SELECT COUNT(*) FROM tradetally_sync_queue WHERE status='failed'",
        [],
        |r| r.get(0),
    )?;
    let recent = get_recent(conn, recent_limit)?;
    Ok(SyncQueueStatus { pending, success, failed, recent })
}

fn get_recent(conn: &Connection, limit: u32) -> Result<Vec<SyncQueueRow>> {
    let mut stmt = conn.prepare(
        "SELECT event_id,timestamp,trade_id,symbol,event_type,endpoint,
                payload,status,error_message,attempts,created_at
         FROM tradetally_sync_queue
         ORDER BY created_at DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![limit], row_mapper)?;
    rows.collect()
}

fn get_by_status(conn: &Connection, status: &str, limit: u32) -> Result<Vec<SyncQueueRow>> {
    let mut stmt = conn.prepare(
        "SELECT event_id,timestamp,trade_id,symbol,event_type,endpoint,
                payload,status,error_message,attempts,created_at
         FROM tradetally_sync_queue WHERE status=?1
         ORDER BY created_at ASC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![status, limit], row_mapper)?;
    rows.collect()
}

fn row_mapper(row: &rusqlite::Row<'_>) -> rusqlite::Result<SyncQueueRow> {
    Ok(SyncQueueRow {
        event_id: row.get(0)?,
        timestamp: row.get(1)?,
        trade_id: row.get(2)?,
        symbol: row.get(3)?,
        event_type: row.get(4)?,
        endpoint: row.get(5)?,
        payload_summary: row.get(6)?,
        status: row.get(7)?,
        error_message: row.get(8)?,
        attempts: row.get(9)?,
        created_at: row.get(10)?,
    })
}

/// Remove not-yet-delivered events that predate the v1 API rewrite — they use
/// retired event types or old (non-v1) endpoints and can never succeed. Runs
/// once at startup. Note: the current `chart_updated` event legitimately uses a
/// non-v1 (/api/trades/:id/images) endpoint, so it is matched by type, not URL.
pub fn purge_legacy(conn: &Connection) -> Result<usize> {
    conn.execute(
        "DELETE FROM tradetally_sync_queue
         WHERE status != 'success'
           AND (
                 event_type IN ('trade_id_created','sl_updated','tp_updated','trade_opened','capture_added')
              OR (event_type IN ('trade_closed','note_updated') AND endpoint NOT LIKE '/api/v1/%')
           )",
        [],
    )
}

// ─── Retry helpers ───────────────────────────────────────────────────────────

pub fn reset_to_pending(conn: &Connection, event_id: &str) -> Result<()> {
    conn.execute(
        "UPDATE tradetally_sync_queue
         SET status='pending', error_message=NULL, attempts=0
         WHERE event_id=?1",
        params![event_id],
    )?;
    Ok(())
}

pub fn reset_all_failed_to_pending(conn: &Connection) -> Result<()> {
    conn.execute(
        "UPDATE tradetally_sync_queue
         SET status='pending', error_message=NULL, attempts=0
         WHERE status='failed'",
        [],
    )?;
    Ok(())
}

// ─── TradeTally UUID mapping ─────────────────────────────────────────────────

pub fn get_tt_trade_id(conn: &Connection, local_trade_id: &str) -> Result<Option<String>> {
    use rusqlite::OptionalExtension;
    conn.query_row(
        "SELECT tt_trade_id FROM tradetally_trade_ids WHERE local_trade_id=?1",
        params![local_trade_id],
        |r| r.get(0),
    )
    .optional()
}

pub fn save_tt_trade_id(conn: &Connection, local_trade_id: &str, tt_trade_id: &str) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO tradetally_trade_ids (local_trade_id, tt_trade_id, created_at)
         VALUES (?1, ?2, datetime('now'))",
        params![local_trade_id, tt_trade_id],
    )?;
    Ok(())
}
