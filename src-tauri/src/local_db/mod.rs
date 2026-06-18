// Local SQLite store. Universe cache, TradeTally outbound queue, tags cache, logs.
// MUST never sit on the live critical path (Alpaca → RAM → scanner → UI).
// All writes on the live path are fire-and-forget via a tokio channel.

pub mod alarm_repository;
pub mod book_repository;
pub mod bug_repository;
pub mod cache_repository;
pub mod company_intel_repository;
pub mod company_meta_repository;
pub mod drawing_repository;
pub mod execution_repository;
pub mod journal_repository;
pub mod llm_repository;
pub mod schema;
pub mod scoring_repository;
pub mod tags_repository;
pub mod tradetally_queue_repository;
pub mod universe_repository;

use rusqlite::{Connection, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub use alarm_repository::PriceAlarm;
pub use bug_repository::BugReport;
pub use cache_repository::{DailyBar, FundamentalCache};
pub use company_meta_repository::CompanyMeta;
pub use journal_repository::JournalEntry;
pub use tags_repository::get_all as get_tags;
pub use tradetally_queue_repository::{SyncQueueRow, SyncQueueStatus};
pub use universe_repository::UniverseAsset;

// ─── Log entry (also used by the command layer) ─────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalLogEntry {
    pub id: i64,
    pub level: String,
    pub message: String,
    pub created_at: String,
}

// ─── Open + migrate ─────────────────────────────────────────────────────────

/// Open (or create) the database and apply all migrations.
pub fn open_and_migrate(app_dir: &Path) -> Result<Connection> {
    let db_path = app_dir.join("tagdash.db");
    let conn = Connection::open(&db_path)?;
    schema::migrate(&conn)?;
    Ok(conn)
}

// ─── Log helpers ────────────────────────────────────────────────────────────

pub fn insert_log(conn: &Connection, level: &str, message: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO local_logs (level, message) VALUES (?1, ?2)",
        rusqlite::params![level, message],
    )?;
    Ok(())
}

pub fn get_recent_logs(conn: &Connection, limit: u32) -> Result<Vec<LocalLogEntry>> {
    let mut stmt = conn.prepare(
        "SELECT id, level, message, created_at FROM local_logs ORDER BY id DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map(rusqlite::params![limit], |row| {
        Ok(LocalLogEntry {
            id: row.get(0)?,
            level: row.get(1)?,
            message: row.get(2)?,
            created_at: row.get(3)?,
        })
    })?;
    rows.collect()
}
