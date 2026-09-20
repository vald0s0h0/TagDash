// Dashboard (moodboard) persistence: the local mirror of TradeTally trades that
// feeds the KPI cards, plus the local copy of sent diary entries. TradeTally is
// the source of truth — `tt_trades` is rebuilt from the API on every tab open.

use rusqlite::{params, Connection, Result};

use crate::dashboard::{DashboardTrade, TradeSnapshot};

/// Upsert one trade (keyed by the TradeTally id). Re-syncing simply overwrites
/// the cached row with the latest upstream values.
pub fn upsert_trade(conn: &Connection, t: &DashboardTrade) -> Result<()> {
    let tags_json = serde_json::to_string(&t.tags).unwrap_or_else(|_| "[]".into());
    let raw_json = serde_json::to_string(&t.raw).unwrap_or_else(|_| "{}".into());
    conn.execute(
        "INSERT INTO tt_trades
            (tt_id, symbol, side, quantity, entry_price, exit_price, pnl, pnl_percent,
             entry_date, exit_date, commission, fees, status, setup, strategy, broker,
             tags_json, raw_json, synced_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18, datetime('now'))
         ON CONFLICT(tt_id) DO UPDATE SET
            symbol=excluded.symbol, side=excluded.side, quantity=excluded.quantity,
            entry_price=excluded.entry_price, exit_price=excluded.exit_price,
            pnl=excluded.pnl, pnl_percent=excluded.pnl_percent,
            entry_date=excluded.entry_date, exit_date=excluded.exit_date,
            commission=excluded.commission, fees=excluded.fees, status=excluded.status,
            setup=excluded.setup, strategy=excluded.strategy, broker=excluded.broker,
            tags_json=excluded.tags_json, raw_json=excluded.raw_json,
            synced_at=datetime('now')",
        params![
            t.tt_id,
            t.symbol,
            t.side,
            t.quantity,
            t.entry_price,
            t.exit_price,
            t.pnl,
            t.pnl_percent,
            t.entry_date,
            t.exit_date,
            t.commission,
            t.fees,
            t.status,
            t.setup,
            t.strategy,
            t.broker,
            tags_json,
            raw_json,
        ],
    )?;
    Ok(())
}

/// What one reconcile actually did. `pruned` says whether the destructive half
/// ran at all — the dashboard surfaces it so a sync that only refreshed (because
/// the fetch could not be trusted as complete) is never mistaken for one that
/// reconciled deletions.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct SyncOutcome {
    pub upserted: usize,
    pub deleted: usize,
    pub mappings_pruned: usize,
    pub mappings_restored: usize,
    pub pruned: bool,
}

/// Reconcile the whole cache against an API fetch, in one transaction: upsert
/// every incoming trade, then — only against a snapshot proven complete — delete
/// any cached row whose id was absent from the fetch and drop any local→server id
/// mapping that now points at a trade TradeTally no longer has.
///
/// The prune is what makes an upstream deletion visible here. TradeTally is the
/// source of truth but exposes no "deleted trades" feed, so a trade the user
/// removed there can only be detected by its absence from a complete sync —
/// without this, a deleted trade stayed in the KPI cards forever.
///
/// Guards, in order of what they protect against:
/// * `snapshot.complete == false` → refresh only, delete nothing. A page-through
///   that stopped early (HTTP error, truncating proxy, a total that didn't match)
///   describes part of the journal, and pruning against a part deletes the rest.
/// * an empty `trades` slice → delete nothing, for the same reason plus the
///   obvious one: a journal that fetches as zero trades is far more likely an
///   auth or transport failure than a user who deleted everything, and wiping the
///   cache on that would be unrecoverable without a working API.
/// * the mapping prune ignores rows created at or after `fetch_started_at`, so a
///   trade pushed to TradeTally *while* this fetch was in flight (and therefore
///   legitimately absent from it) keeps its mapping.
pub fn sync_trades_bulk(
    conn: &mut Connection,
    snapshot: &TradeSnapshot,
    fetch_started_at: &str,
) -> Result<SyncOutcome> {
    let trades = &snapshot.trades;
    let tx = conn.transaction()?;

    // Ids seen in this fetch. A temp table (rather than a giant `NOT IN (?,?,…)`)
    // keeps the delete a single statement no matter how large the journal grows.
    tx.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS tt_sync_keep (tt_id TEXT PRIMARY KEY NOT NULL);
         DELETE FROM tt_sync_keep;",
    )?;

    for t in trades {
        let tags_json = serde_json::to_string(&t.tags).unwrap_or_else(|_| "[]".into());
        let raw_json = serde_json::to_string(&t.raw).unwrap_or_else(|_| "{}".into());
        tx.execute(
            "INSERT INTO tt_trades
                (tt_id, symbol, side, quantity, entry_price, exit_price, pnl, pnl_percent,
                 entry_date, exit_date, commission, fees, status, setup, strategy, broker,
                 tags_json, raw_json, synced_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18, datetime('now'))
             ON CONFLICT(tt_id) DO UPDATE SET
                symbol=excluded.symbol, side=excluded.side, quantity=excluded.quantity,
                entry_price=excluded.entry_price, exit_price=excluded.exit_price,
                pnl=excluded.pnl, pnl_percent=excluded.pnl_percent,
                entry_date=excluded.entry_date, exit_date=excluded.exit_date,
                commission=excluded.commission, fees=excluded.fees, status=excluded.status,
                setup=excluded.setup, strategy=excluded.strategy, broker=excluded.broker,
                tags_json=excluded.tags_json, raw_json=excluded.raw_json,
                synced_at=datetime('now')",
            params![
                t.tt_id, t.symbol, t.side, t.quantity, t.entry_price, t.exit_price, t.pnl,
                t.pnl_percent, t.entry_date, t.exit_date, t.commission, t.fees, t.status,
                t.setup, t.strategy, t.broker, tags_json, raw_json,
            ],
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO tt_sync_keep (tt_id) VALUES (?1)",
            params![t.tt_id],
        )?;
    }

    let may_prune = snapshot.complete && !trades.is_empty();
    let (deleted, mappings_pruned) = if may_prune {
        let deleted = tx.execute(
            "DELETE FROM tt_trades WHERE tt_id NOT IN (SELECT tt_id FROM tt_sync_keep)",
            [],
        )?;
        // A mapping to a trade that no longer exists upstream is worse than
        // useless: every later note / screenshot / close event PUTs to a dead id
        // and fails forever, and the "Sync TT" column claims the trade is mirrored.
        let mappings_pruned = tx.execute(
            "DELETE FROM tradetally_trade_ids
             WHERE created_at < ?1
               AND tt_trade_id NOT IN (SELECT tt_id FROM tt_sync_keep)",
            params![fetch_started_at],
        )?;
        (deleted, mappings_pruned)
    } else {
        (0, 0)
    };

    // Rebuild any local→server mapping that TradeTally still remembers but we do
    // not. The marker in a trade's notes is the record of which TagDash trade it
    // is, and it lives upstream, so this recovers mappings lost to a DB reset or a
    // create whose response hid the id — and with them the ability to attach later
    // notes and screenshots to the right trade instead of failing forever.
    let mut mappings_restored = 0usize;
    for t in trades {
        let Some(notes) = t.raw.get("notes").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(local_id) = crate::tradetally::local_id_from_marker(notes) else {
            continue;
        };
        mappings_restored += tx.execute(
            "INSERT OR IGNORE INTO tradetally_trade_ids (local_trade_id, tt_trade_id, created_at)
             VALUES (?1, ?2, datetime('now'))",
            params![local_id, t.tt_id],
        )?;
    }

    tx.commit()?;
    Ok(SyncOutcome {
        upserted: trades.len(),
        deleted,
        mappings_pruned,
        mappings_restored,
        pruned: may_prune,
    })
}

/// All cached trades, oldest first (by exit then entry date) so the frontend can
/// build cumulative curves directly.
pub fn get_all_trades(conn: &Connection) -> Result<Vec<DashboardTrade>> {
    let mut stmt = conn.prepare(
        "SELECT tt_id, symbol, side, quantity, entry_price, exit_price, pnl, pnl_percent,
                entry_date, exit_date, commission, fees, status, setup, strategy, broker,
                tags_json, raw_json
         FROM tt_trades
         ORDER BY COALESCE(exit_date, entry_date) ASC, tt_id ASC",
    )?;
    let rows = stmt.query_map([], |row| {
        let tags_json: String = row.get(16)?;
        let raw_json: String = row.get(17)?;
        Ok(DashboardTrade {
            tt_id: row.get(0)?,
            symbol: row.get(1)?,
            side: row.get(2)?,
            quantity: row.get(3)?,
            entry_price: row.get(4)?,
            exit_price: row.get(5)?,
            pnl: row.get(6)?,
            pnl_percent: row.get(7)?,
            entry_date: row.get(8)?,
            exit_date: row.get(9)?,
            commission: row.get(10)?,
            fees: row.get(11)?,
            status: row.get(12)?,
            setup: row.get(13)?,
            strategy: row.get(14)?,
            broker: row.get(15)?,
            tags: serde_json::from_str(&tags_json).unwrap_or_default(),
            raw: serde_json::from_str(&raw_json).unwrap_or(serde_json::Value::Null),
        })
    })?;
    rows.collect()
}

/// The local id used for the mirror of TradeTally's own entry for a day. Distinct
/// from the ids `insert_diary_local` mints for outgoing drafts, and deterministic,
/// so re-mirroring the same day overwrites rather than piling up rows.
fn upstream_diary_id(entry_date: &str) -> String {
    format!("tt-{entry_date}")
}

/// Mirror TradeTally's entry for a day into the local table. TradeTally keeps one
/// entry per calendar day and updates it in place, so the day is the key.
pub fn upsert_diary_upstream(
    conn: &Connection,
    entry_date: &str,
    title: &str,
    content: &str,
) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO diary_entries_local (id, entry_date, title, content)
         VALUES (?1, ?2, ?3, ?4)",
        params![upstream_diary_id(entry_date), entry_date, title, content],
    )?;
    Ok(())
}

/// The locally mirrored entry for a day, as `(title, content)`. Prefers the copy
/// of TradeTally's own entry over an outgoing draft, then the most recent row.
/// Used as the offline fallback when the diary GET can't reach the server.
pub fn get_diary_local(conn: &Connection, entry_date: &str) -> Result<Option<(String, String)>> {
    use rusqlite::OptionalExtension;
    conn.query_row(
        "SELECT title, content FROM diary_entries_local
         WHERE entry_date = ?1
         ORDER BY (id = ?2) DESC, created_at DESC
         LIMIT 1",
        params![entry_date, upstream_diary_id(entry_date)],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()
}

/// Store a local copy of a diary entry that was queued for TradeTally.
pub fn insert_diary_local(
    conn: &Connection,
    id: &str,
    entry_date: &str,
    title: &str,
    content: &str,
) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO diary_entries_local (id, entry_date, title, content)
         VALUES (?1, ?2, ?3, ?4)",
        params![id, entry_date, title, content],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::local_db::schema::migrate(&conn).unwrap();
        conn
    }

    fn trade(id: &str, notes: Option<&str>) -> DashboardTrade {
        DashboardTrade {
            tt_id: id.into(),
            symbol: Some("AAPL".into()),
            side: Some("long".into()),
            quantity: Some(10.0),
            entry_price: Some(1.0),
            exit_price: Some(2.0),
            pnl: Some(10.0),
            pnl_percent: None,
            entry_date: Some("2026-09-01T13:30:00Z".into()),
            exit_date: Some("2026-09-01T13:35:00Z".into()),
            commission: None,
            fees: None,
            status: Some("Closed".into()),
            setup: None,
            strategy: None,
            broker: Some("TagDash".into()),
            tags: vec![],
            raw: match notes {
                Some(n) => json!({ "id": id, "notes": n }),
                None => json!({ "id": id }),
            },
        }
    }

    fn snapshot(trades: Vec<DashboardTrade>, total: Option<usize>, complete: bool) -> TradeSnapshot {
        TradeSnapshot {
            trades,
            upstream_total: total,
            complete,
        }
    }

    fn cached_ids(conn: &Connection) -> Vec<String> {
        let mut stmt = conn.prepare("SELECT tt_id FROM tt_trades ORDER BY tt_id").unwrap();
        let ids = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        ids
    }

    const LATER: &str = "2099-01-01 00:00:00";

    #[test]
    fn a_complete_fetch_prunes_trades_deleted_upstream() {
        let mut conn = db();
        sync_trades_bulk(&mut conn, &snapshot(vec![trade("a", None), trade("b", None)], Some(2), true), LATER)
            .unwrap();
        assert_eq!(cached_ids(&conn), vec!["a", "b"]);

        // "b" was deleted in TradeTally: the next complete sync no longer returns it.
        let out =
            sync_trades_bulk(&mut conn, &snapshot(vec![trade("a", None)], Some(1), true), LATER).unwrap();
        assert!(out.pruned);
        assert_eq!(out.deleted, 1);
        assert_eq!(cached_ids(&conn), vec!["a"]);
    }

    #[test]
    fn an_incomplete_fetch_refreshes_but_never_deletes() {
        let mut conn = db();
        sync_trades_bulk(&mut conn, &snapshot(vec![trade("a", None), trade("b", None)], Some(2), true), LATER)
            .unwrap();

        // Page 2 of 2 was lost: 1 trade in hand, upstream says 2. Deleting "b" here
        // would throw away a trade that still exists in TradeTally.
        let out =
            sync_trades_bulk(&mut conn, &snapshot(vec![trade("a", None)], Some(2), false), LATER).unwrap();
        assert!(!out.pruned);
        assert_eq!(out.deleted, 0);
        assert_eq!(cached_ids(&conn), vec!["a", "b"]);
    }

    #[test]
    fn an_empty_fetch_never_wipes_the_cache() {
        let mut conn = db();
        sync_trades_bulk(&mut conn, &snapshot(vec![trade("a", None)], Some(1), true), LATER).unwrap();
        let out = sync_trades_bulk(&mut conn, &snapshot(vec![], Some(0), true), LATER).unwrap();
        assert!(!out.pruned);
        assert_eq!(cached_ids(&conn), vec!["a"]);
    }

    #[test]
    fn a_complete_fetch_drops_mappings_to_trades_that_no_longer_exist() {
        let mut conn = db();
        crate::local_db::tradetally_queue_repository::save_tt_trade_id(&conn, "local-1", "a").unwrap();
        crate::local_db::tradetally_queue_repository::save_tt_trade_id(&conn, "local-2", "gone").unwrap();

        let out = sync_trades_bulk(&mut conn, &snapshot(vec![trade("a", None)], Some(1), true), LATER).unwrap();
        assert_eq!(out.mappings_pruned, 1);
        assert_eq!(
            crate::local_db::tradetally_queue_repository::get_tt_trade_id(&conn, "local-1").unwrap(),
            Some("a".into())
        );
        assert_eq!(
            crate::local_db::tradetally_queue_repository::get_tt_trade_id(&conn, "local-2").unwrap(),
            None
        );
    }

    #[test]
    fn a_mapping_created_after_the_fetch_started_survives_the_prune() {
        let mut conn = db();
        // A trade pushed while the fetch was in flight: absent from the snapshot,
        // but its mapping is newer than the fetch and must be kept.
        crate::local_db::tradetally_queue_repository::save_tt_trade_id(&conn, "local-new", "inflight")
            .unwrap();
        let out = sync_trades_bulk(
            &mut conn,
            &snapshot(vec![trade("a", None)], Some(1), true),
            "1970-01-01 00:00:00",
        )
        .unwrap();
        assert_eq!(out.mappings_pruned, 0);
        assert_eq!(
            crate::local_db::tradetally_queue_repository::get_tt_trade_id(&conn, "local-new").unwrap(),
            Some("inflight".into())
        );
    }

    #[test]
    fn a_lost_mapping_is_rebuilt_from_the_marker_in_the_upstream_notes() {
        let mut conn = db();
        let out = sync_trades_bulk(
            &mut conn,
            &snapshot(
                vec![trade("tt-42", Some("Belle gestion du risque.\n\nTagDash 260901-AAPL-HOD_DRIVE"))],
                Some(1),
                true,
            ),
            LATER,
        )
        .unwrap();
        assert_eq!(out.mappings_restored, 1);
        assert_eq!(
            crate::local_db::tradetally_queue_repository::get_tt_trade_id(
                &conn,
                "260901-AAPL-HOD_DRIVE"
            )
            .unwrap(),
            Some("tt-42".into())
        );
    }

    #[test]
    fn the_local_diary_mirror_is_one_row_per_day_and_prefers_the_upstream_copy() {
        let conn = db();
        insert_diary_local(&conn, "2026-09-20-1", "2026-09-20", "brouillon", "local").unwrap();
        upsert_diary_upstream(&conn, "2026-09-20", "v1", "amont 1").unwrap();
        upsert_diary_upstream(&conn, "2026-09-20", "v2", "amont 2").unwrap();
        assert_eq!(
            get_diary_local(&conn, "2026-09-20").unwrap(),
            Some(("v2".into(), "amont 2".into()))
        );
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM diary_entries_local WHERE entry_date='2026-09-20'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 2, "un brouillon + une copie amont, pas une par sync");
    }
}
