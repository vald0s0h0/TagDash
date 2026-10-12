// One-time separation of a database written before personal data had its own file.
//
// Such a database holds the personal tables in `main` (now the cache database).
// Left there they would SHADOW the tables of the attached `user` database — an
// unqualified name resolves in `main` first — so this runs inside `schema::migrate`,
// before anything reads a row.
//
// Per table: copy the rows across, check that none is missing, then RENAME the
// original to `legacy_split_<table>`. Nothing is ever deleted here. The renamed
// originals stay in the cache database until the user purges them from Réglages ▸
// Mes données (`purge_legacy`), which is the only destructive step and is theirs.
//
// A table whose copy cannot be verified is left exactly where it was: it keeps
// shadowing, the app keeps reading and writing it as it always did, and the report
// says so. Slower to converge, but a row never becomes invisible.
//
// Idempotent: a table already renamed is no longer in `main` under its name, and a
// run interrupted between the copy and the rename finds identical rows on retry.

use rusqlite::{params, Connection, OptionalExtension, Result};

use super::schema::{USER_CONFIG_KEYS, USER_TABLES};

/// Prefix of a pre-separation table kept in the cache database.
pub const LEGACY_PREFIX: &str = "legacy_split_";
/// Prefix of a pre-separation personal key kept in `main.app_config`.
pub const LEGACY_KEY_PREFIX: &str = "legacy_split:";

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SplitReport {
    /// (table, rows copied) for every table moved by this run.
    pub moved: Vec<(String, i64)>,
    /// (table, reason) for every table left in the cache database.
    pub failed: Vec<(String, String)>,
    /// Personal keys moved out of `main.app_config`.
    pub keys_moved: usize,
}

impl SplitReport {
    pub fn is_noop(&self) -> bool {
        self.moved.is_empty() && self.failed.is_empty() && self.keys_moved == 0
    }
}

fn main_has_table(conn: &Connection, name: &str) -> Result<bool> {
    conn.query_row(
        "SELECT 1 FROM main.sqlite_master WHERE type = 'table' AND name = ?1",
        params![name],
        |_| Ok(()),
    )
    .optional()
    .map(|r| r.is_some())
}

/// `(name, declared type)` of a table's columns, in order.
fn columns(conn: &Connection, schema: &str, table: &str) -> Result<Vec<(String, String)>> {
    conn.prepare(&format!("PRAGMA {schema}.table_info(\"{table}\")"))?
        .query_map([], |r| Ok((r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?
        .collect()
}

fn quoted(cols: &[String]) -> String {
    cols.iter().map(|c| format!("\"{c}\"")).collect::<Vec<_>>().join(", ")
}

/// A free `legacy_split_<table>` name in `main`.
fn legacy_name(conn: &Connection, table: &str) -> Result<String> {
    let base = format!("{LEGACY_PREFIX}{table}");
    if !main_has_table(conn, &base)? {
        return Ok(base);
    }
    for n in 2.. {
        let candidate = format!("{base}_{n}");
        if !main_has_table(conn, &candidate)? {
            return Ok(candidate);
        }
    }
    unreachable!()
}

/// Move one table. `Ok(Some(rows))` moved, `Ok(None)` nothing to move, `Err` left
/// in place (the caller records the reason).
fn move_table(conn: &Connection, table: &str) -> std::result::Result<Option<i64>, String> {
    let e = |err: rusqlite::Error| err.to_string();
    if !main_has_table(conn, table).map_err(e)? {
        return Ok(None);
    }
    let old_cols = columns(conn, "main", table).map_err(e)?;
    let new_names: Vec<String> =
        columns(conn, "user", table).map_err(e)?.into_iter().map(|(n, _)| n).collect();
    // A column the current schema no longer declares still holds the user's data:
    // carry it over rather than drop it on the floor.
    for (name, ty) in &old_cols {
        if !new_names.contains(name) {
            conn.execute(&format!("ALTER TABLE user.\"{table}\" ADD COLUMN \"{name}\" {ty}"), [])
                .map_err(e)?;
        }
    }
    let cols = quoted(&old_cols.into_iter().map(|(n, _)| n).collect::<Vec<_>>());

    let tx = conn.unchecked_transaction().map_err(e)?;
    let already: i64 = tx
        .query_row(&format!("SELECT COUNT(*) FROM user.\"{table}\""), [], |r| r.get(0))
        .map_err(e)?;
    let copied = tx
        .execute(
            &format!("INSERT OR IGNORE INTO user.\"{table}\" ({cols}) SELECT {cols} FROM main.\"{table}\""),
            [],
        )
        .map_err(e)? as i64;
    let missing: i64 = tx
        .query_row(
            &format!(
                "SELECT COUNT(*) FROM (SELECT {cols} FROM main.\"{table}\"
                                       EXCEPT SELECT {cols} FROM user.\"{table}\")"
            ),
            [],
            |r| r.get(0),
        )
        .map_err(e)?;
    // Into an empty table every row must arrive. One that did not was refused by a
    // constraint, and renaming the source now would hide it from the app.
    if missing > 0 && already == 0 {
        return Err(format!("{missing} ligne(s) refusée(s) à la copie — table laissée en place"));
    }
    let legacy = legacy_name(&tx, table).map_err(e)?;
    tx.execute(&format!("ALTER TABLE main.\"{table}\" RENAME TO \"{legacy}\""), [])
        .map_err(e)?;
    tx.commit().map_err(e)?;
    Ok(Some(copied))
}

/// Move the personal keys of the key/value bag. Returns how many moved.
fn move_keys(conn: &Connection) -> Result<usize> {
    if !main_has_table(conn, "app_config")? {
        return Ok(0);
    }
    let tx = conn.unchecked_transaction()?;
    let mut moved = 0;
    for key in USER_CONFIG_KEYS {
        let found = tx.execute(
            "INSERT OR IGNORE INTO user.user_config (key, value, updated_at)
             SELECT key, value, updated_at FROM main.app_config WHERE key = ?1",
            params![key],
        )?;
        let renamed = tx.execute(
            "UPDATE OR REPLACE main.app_config SET key = ?2 WHERE key = ?1",
            params![key, format!("{LEGACY_KEY_PREFIX}{key}")],
        )?;
        moved += found.max(renamed);
    }
    tx.commit()?;
    Ok(moved)
}

/// Rewrite the file paths an older build stored as absolute into the relative
/// form (`crate::paths`). Cheap enough to run on every startup: once converted,
/// no row matches.
pub fn normalize_stored_paths(conn: &Connection) -> Result<usize> {
    let mut changed = 0;
    for (table, column) in [
        ("screenshot_files", "path"),
        ("review_sessions", "store_path"),
        ("review_voice", "audio_path"),
    ] {
        let rows: Vec<(i64, String)> = conn
            .prepare(&format!(
                "SELECT rowid, {column} FROM user.{table}
                  WHERE {column} LIKE '/%' OR {column} LIKE '_:%' OR {column} LIKE '%\\%' ESCAPE '|'"
            ))?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .filter_map(|r| r.ok())
            .collect();
        for (rowid, stored) in rows {
            let fixed = crate::paths::normalize_stored(&stored);
            if fixed != stored {
                changed += conn.execute(
                    &format!("UPDATE user.{table} SET {column} = ?1 WHERE rowid = ?2"),
                    params![fixed, rowid],
                )?;
            }
        }
    }
    // Screenshot uploads still waiting in the TradeTally queue carry the file path
    // in their payload.
    let queued: Vec<(String, String)> = conn
        .prepare(
            "SELECT event_id, payload FROM user.tradetally_sync_queue
              WHERE event_type = 'chart_updated' AND status != 'success'",
        )?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .filter_map(|r| r.ok())
        .collect();
    for (event_id, payload) in queued {
        let Ok(mut json) = serde_json::from_str::<serde_json::Value>(&payload) else {
            continue;
        };
        let Some(stored) = json.get("localPath").and_then(|v| v.as_str()).map(str::to_string) else {
            continue;
        };
        let fixed = crate::paths::normalize_stored(&stored);
        if fixed != stored {
            json["localPath"] = serde_json::Value::String(fixed);
            changed += conn.execute(
                "UPDATE user.tradetally_sync_queue SET payload = ?1 WHERE event_id = ?2",
                params![json.to_string(), event_id],
            )?;
        }
    }
    Ok(changed)
}

/// Run the separation. Called by `schema::migrate` once the personal schema exists.
pub fn run(conn: &Connection) -> Result<SplitReport> {
    let mut report = SplitReport::default();
    for table in USER_TABLES {
        match move_table(conn, table) {
            Ok(Some(rows)) => report.moved.push((table.to_string(), rows)),
            Ok(None) => {}
            Err(reason) => report.failed.push((table.to_string(), reason)),
        }
    }
    report.keys_moved = move_keys(conn)?;
    normalize_stored_paths(conn)?;
    if !report.is_noop() {
        let rows: i64 = report.moved.iter().map(|(_, n)| n).sum();
        eprintln!(
            "[tagdash] données personnelles séparées du cache : {} table(s), {rows} ligne(s), {} réglage(s)",
            report.moved.len(),
            report.keys_moved,
        );
        for (table, reason) in &report.failed {
            eprintln!("[tagdash] séparation : {table} — {reason}");
        }
    }
    Ok(report)
}

// ─── What is left behind ────────────────────────────────────────────────────────

/// Pre-separation originals still kept in the cache database.
#[derive(Debug, Clone, serde::Serialize)]
pub struct LegacyLeftovers {
    /// Renamed tables (`legacy_split_*`).
    pub tables: Vec<String>,
    pub rows: i64,
    /// Renamed keys of `app_config`.
    pub keys: i64,
    /// Personal tables that could NOT be moved and still live in the cache database.
    pub unmoved: Vec<String>,
}

fn legacy_tables(conn: &Connection) -> Result<Vec<String>> {
    conn.prepare(
        "SELECT name FROM main.sqlite_master
          WHERE type = 'table' AND name LIKE 'legacy|_split|_%' ESCAPE '|' ORDER BY name",
    )?
    .query_map([], |r| r.get::<_, String>(0))?
    .collect()
}

pub fn leftovers(conn: &Connection) -> Result<LegacyLeftovers> {
    let tables = legacy_tables(conn)?;
    let mut rows = 0i64;
    for t in &tables {
        rows += conn.query_row(&format!("SELECT COUNT(*) FROM main.\"{t}\""), [], |r| r.get::<_, i64>(0))?;
    }
    let keys = if main_has_table(conn, "app_config")? {
        conn.query_row(
            "SELECT COUNT(*) FROM main.app_config WHERE key LIKE 'legacy|_split:%' ESCAPE '|'",
            [],
            |r| r.get(0),
        )?
    } else {
        0
    };
    let mut unmoved = Vec::new();
    for t in USER_TABLES {
        if main_has_table(conn, t)? {
            unmoved.push(t.to_string());
        }
    }
    Ok(LegacyLeftovers { tables, rows, keys, unmoved })
}

/// Drop the pre-separation originals. The one destructive step of the separation,
/// and only ever run on the user's explicit request. Returns the tables dropped.
pub fn purge_legacy(conn: &Connection) -> Result<usize> {
    let tables = legacy_tables(conn)?;
    let tx = conn.unchecked_transaction()?;
    for t in &tables {
        tx.execute(&format!("DROP TABLE main.\"{t}\""), [])?;
    }
    tx.execute("DELETE FROM main.app_config WHERE key LIKE 'legacy|_split:%' ESCAPE '|'", [])?;
    tx.commit()?;
    Ok(tables.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_db::schema;

    /// A database as a pre-separation build left it: personal tables and personal
    /// keys in `main`, next to cache tables.
    fn legacy_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE app_config (key TEXT PRIMARY KEY NOT NULL, value TEXT NOT NULL,
                                      updated_at TEXT NOT NULL DEFAULT (datetime('now')));
             INSERT INTO app_config (key, value) VALUES
                 ('strategy_overrides', '{\"apex\":true}'),
                 ('dip_v1_options', '{\"x\":1}'),
                 ('floats_fetch_date', '2026-10-12');
             CREATE TABLE daily_cache (symbol TEXT NOT NULL, date TEXT NOT NULL, open REAL, high REAL,
                                       low REAL, close REAL, volume INTEGER,
                                       updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                                       PRIMARY KEY (symbol, date));
             INSERT INTO daily_cache (symbol, date, close) VALUES ('AAPL', '2026-10-09', 200.0);
             -- An OLD shape of `executions`: no data_at / size_pct yet.
             CREATE TABLE executions (fill_id TEXT PRIMARY KEY NOT NULL, trade_id TEXT NOT NULL,
                                      symbol TEXT NOT NULL, quantity INTEGER NOT NULL,
                                      fill_price REAL NOT NULL, filled_at TEXT NOT NULL);
             CREATE INDEX idx_executions_symbol ON executions(symbol);
             INSERT INTO executions VALUES ('f1', 't1', 'AAPL', 100, 1.5, '2026-10-09T13:31:00Z'),
                                           ('f2', 't1', 'AAPL', -100, 1.8, '2026-10-09T13:40:00Z');
             CREATE TABLE journal_entries (trade_id TEXT PRIMARY KEY NOT NULL, symbol TEXT NOT NULL DEFAULT '',
                                           notes TEXT NOT NULL DEFAULT '', confidence INTEGER,
                                           tags_json TEXT NOT NULL DEFAULT '[]',
                                           created_at TEXT NOT NULL DEFAULT (datetime('now')),
                                           updated_at TEXT NOT NULL DEFAULT (datetime('now')));
             INSERT INTO journal_entries (trade_id, symbol, notes) VALUES ('t1', 'AAPL', 'belle sortie');
             CREATE TABLE screenshot_files (id TEXT PRIMARY KEY NOT NULL, trade_id TEXT, path TEXT NOT NULL,
                                            uploaded INTEGER NOT NULL DEFAULT 0,
                                            created_at TEXT NOT NULL DEFAULT (datetime('now')));
             INSERT INTO screenshot_files (id, trade_id, path) VALUES
                 ('s1', 't1', '/Users/x/Library/Application Support/tagdash/screenshots/s1.png');",
        )
        .unwrap();
        conn
    }

    fn count(conn: &Connection, table: &str) -> i64 {
        conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn personal_tables_move_and_the_originals_are_kept() {
        let conn = legacy_db();
        schema::migrate(&conn).unwrap();

        // The app's own (unqualified) queries now land in the personal database.
        assert_eq!(count(&conn, "executions"), 2);
        assert_eq!(count(&conn, "user.executions"), 2);
        assert_eq!(count(&conn, "user.journal_entries"), 1);
        // The originals are still there, renamed, untouched.
        assert_eq!(count(&conn, "main.legacy_split_executions"), 2);
        assert_eq!(count(&conn, "main.legacy_split_journal_entries"), 1);
        // Cache data did not move.
        assert_eq!(count(&conn, "main.daily_cache"), 1);

        let left = leftovers(&conn).unwrap();
        assert!(left.unmoved.is_empty(), "{:?}", left.unmoved);
        assert_eq!(left.tables.len(), 3);
    }

    #[test]
    fn personal_keys_move_and_cache_markers_stay() {
        let conn = legacy_db();
        schema::migrate(&conn).unwrap();
        let get = crate::local_db::cache_repository::get_app_meta;
        assert_eq!(get(&conn, "strategy_overrides").as_deref(), Some("{\"apex\":true}"));
        assert_eq!(get(&conn, "floats_fetch_date").as_deref(), Some("2026-10-12"));
        assert_eq!(count(&conn, "user.user_config"), 2);
        // The marker stayed in the cache bag; the personal keys are parked, renamed.
        let keys: Vec<String> = conn
            .prepare("SELECT key FROM main.app_config ORDER BY key")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert_eq!(
            keys,
            ["floats_fetch_date", "legacy_split:dip_v1_options", "legacy_split:strategy_overrides"]
        );
    }

    #[test]
    fn stored_paths_become_relative() {
        let conn = legacy_db();
        schema::migrate(&conn).unwrap();
        let path: String =
            conn.query_row("SELECT path FROM screenshot_files WHERE id = 's1'", [], |r| r.get(0)).unwrap();
        assert_eq!(path, "screenshots/s1.png");
    }

    #[test]
    fn the_split_runs_once() {
        let conn = legacy_db();
        schema::migrate(&conn).unwrap();
        conn.execute(
            "INSERT INTO executions (fill_id, trade_id, symbol, quantity, fill_price, filled_at)
             VALUES ('f3', 't2', 'MSFT', 5, 9.0, '2026-10-12T13:31:00Z')",
            [],
        )
        .unwrap();
        // Second launch: nothing left to move, and the new row is neither lost nor doubled.
        assert!(run(&conn).unwrap().is_noop());
        schema::migrate(&conn).unwrap();
        assert_eq!(count(&conn, "user.executions"), 3);
        assert_eq!(count(&conn, "main.legacy_split_executions"), 2);
    }

    #[test]
    fn a_refused_row_leaves_the_table_where_it_was() {
        let conn = legacy_db();
        // A row the current schema would refuse (NOT NULL symbol): legacy tables
        // were created without that guarantee on some installs.
        conn.execute_batch(
            "CREATE TABLE trade_levels (trade_id TEXT PRIMARY KEY NOT NULL, symbol TEXT, original_sl REAL);
             INSERT INTO trade_levels VALUES ('t1', NULL, 1.2);",
        )
        .unwrap();
        schema::migrate(&conn).unwrap();
        // Still readable through the name the app uses, from the place it always was.
        assert_eq!(count(&conn, "trade_levels"), 1);
        assert_eq!(count(&conn, "user.trade_levels"), 0);
        assert_eq!(leftovers(&conn).unwrap().unmoved, ["trade_levels"]);
    }

    #[test]
    fn purging_drops_only_the_parked_originals() {
        let conn = legacy_db();
        schema::migrate(&conn).unwrap();
        assert_eq!(purge_legacy(&conn).unwrap(), 3);
        let left = leftovers(&conn).unwrap();
        assert!(left.tables.is_empty());
        assert_eq!(left.keys, 0);
        assert_eq!(count(&conn, "user.executions"), 2);
        assert_eq!(count(&conn, "main.daily_cache"), 1);
        assert_eq!(count(&conn, "main.app_config"), 1);
    }
}
