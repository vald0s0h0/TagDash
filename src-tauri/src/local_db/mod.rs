// Local SQLite store, in two files behind ONE connection:
//
//   <root>/cache/cache.db  market / reference data, logs — re-downloadable
//   <root>/user/user.db    personal data (trades, review, settings, queues…),
//                          attached as schema `user`
//
// See `schema` for what lives where and why the code keeps naming tables bare.
// MUST never sit on the live critical path (Alpaca → RAM → scanner → UI).
// All writes on the live path are fire-and-forget via a tokio channel.

pub mod alarm_repository;
pub mod book_repository;
pub mod broker_state_repository;
pub mod bug_repository;
pub mod cache_repository;
pub mod company_intel_repository;
pub mod company_meta_repository;
pub mod dashboard_repository;
pub mod drawing_repository;
pub mod execution_repository;
pub mod journal_repository;
pub mod llm_repository;
pub mod micro_attention_repository;
pub mod micro_pullback_quality_repository;
pub mod panic_llm_repository;
pub mod premarket_repository;
pub mod quality_repository;
pub mod schema;
pub mod scoring_repository;
pub mod split;
pub mod screener_repository;
pub mod tags_repository;
pub mod ui_state_repository;
pub mod tradetally_queue_repository;
pub mod universe_repository;
pub mod usage_repository;

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

/// Open the cache database with the personal one attached as `user`. Every
/// connection that reads personal tables must come from here — a bare
/// `Connection::open(cache.db)` simply does not see them.
pub fn open(app_dir: &Path) -> Result<Connection> {
    let cache_db = crate::paths::cache_db(app_dir);
    let user_db = crate::paths::user_db(app_dir);
    for file in [&cache_db, &user_db] {
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
    }
    let conn = Connection::open(&cache_db)?;
    conn.execute(
        "ATTACH DATABASE ?1 AS user",
        rusqlite::params![user_db.to_string_lossy()],
    )?;
    Ok(conn)
}

/// Open (or create) both databases and apply all migrations.
pub fn open_and_migrate(app_dir: &Path) -> Result<Connection> {
    let mut conn = open(app_dir)?;
    schema::migrate(&conn)?;
    universe_repository::purge_legacy_seed_data(&mut conn)?;
    Ok(conn)
}

/// Connection for the manual harnesses (`TAGDASH_E2E_DB`), which point at a copy
/// of the market database — `cache/cache.db`, or a `tagdash.db` taken before the
/// separation. The file itself is opened READ-ONLY. The personal database beside it
/// (`../user/user.db`) is attached when there is one; otherwise an in-memory one,
/// seeded with the personal settings a pre-separation copy still keeps in its own
/// key/value bag, so a harness runs with the options the copy was taken with.
#[cfg(test)]
pub fn open_harness(db: &Path) -> Connection {
    // `mode=ro` on the file, not on the connection: a read-only CONNECTION would
    // make the in-memory database attached below read-only too.
    let encoded: String = db
        .to_string_lossy()
        .replace('\\', "/")
        .chars()
        .map(|c| match c {
            ' ' => "%20".to_string(),
            '%' => "%25".to_string(),
            '?' => "%3F".to_string(),
            '#' => "%23".to_string(),
            c => c.to_string(),
        })
        .collect();
    let lead = if encoded.starts_with('/') { "" } else { "/" };
    let conn = Connection::open(format!("file:{lead}{encoded}?mode=ro"))
        .unwrap_or_else(|e| panic!("{} : {e}", db.display()));
    let beside = db
        .parent()
        .and_then(Path::parent)
        .map(|root| crate::paths::user_db(root))
        .filter(|p| p.exists());
    match beside {
        Some(user_db) => {
            conn.execute("ATTACH DATABASE ?1 AS user", rusqlite::params![user_db.to_string_lossy()])
                .unwrap();
        }
        None => {
            schema::create_user_schema(&conn).unwrap();
            for key in schema::USER_CONFIG_KEYS {
                let _ = conn.execute(
                    "INSERT OR IGNORE INTO user.user_config (key, value)
                     SELECT ?1, value FROM main.app_config WHERE key IN (?1, ?2)",
                    rusqlite::params![key, format!("{}{key}", split::LEGACY_KEY_PREFIX)],
                );
            }
        }
    }
    conn
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
