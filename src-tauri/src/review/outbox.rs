// Offline outbox of the « Revue » tab: what the review writes onto TradeTally
// trades (an edited note, a review appended by « Suivant ») is applied locally at
// once and queued here, then sent upstream in the background — at once when the
// network is there, otherwise as soon as it comes back. The review never waits on
// TradeTally and never loses a line to a dropped connection.
//
// The queue holds OPERATIONS, not finished notes, so that what reaches TradeTally
// is computed from the server's notes AT SEND TIME (a dictée or an edit made
// upstream meanwhile is kept, and the TagDash marker line always survives):
//   • `set_notes` — the trader's text becomes exactly this (marker kept);
//   • `append`    — a `Review YYYY-MM-DD : …` block is added at the end.
// All the pending operations of a trade are folded over its server notes and sent
// in ONE write; they leave the queue only once TradeTally has accepted it.
//
// Each row also keeps `local_notes`, the trader's text as it reads locally after
// that operation: while a trade has pending operations, that is what the sidebar
// shows when TradeTally cannot be reached (the cached mirror may have been
// refreshed from the server by a sync in between).
//
// Missed trades are TagDash-only and never go through here.

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::Duration;

use chrono::Utc;
use rusqlite::{params, Connection};
use serde::Serialize;
use serde_json::{json, Value};

use crate::local_db::journal_repository::{self, JournalEntry};
use crate::tradetally::TtClient;

/// Background retry period while operations are pending.
const RETRY_SECS: u64 = 20;
/// After a network failure, reads of TradeTally notes go straight to the local
/// copy for this long instead of waiting on a timeout each time.
const OFFLINE_HOLD_MS: i64 = 30_000;
/// Timeout of an interactive read of a trade's notes (the sidebar waits on it).
const READ_TIMEOUT_SECS: u64 = 6;

/// Real-clock ms of the last network failure reaching TradeTally (0 = none since
/// the last success).
static LAST_NET_FAIL: AtomicI64 = AtomicI64::new(0);
/// Real-clock ms of the last successful send.
static LAST_SYNC: AtomicI64 = AtomicI64::new(0);
static FLUSHING: AtomicBool = AtomicBool::new(false);
/// Something was queued while a flush ran — it runs again before stopping.
static DIRTY: AtomicBool = AtomicBool::new(false);

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

// ─── Notes text ─────────────────────────────────────────────────────────────────

/// Split TradeTally notes into what the trader wrote and the TagDash correlation
/// marker line(s), which must survive every edit (see `tradetally::marker_for`).
pub fn split_marker(notes: &str) -> (String, Vec<String>) {
    let mut text = Vec::new();
    let mut markers = Vec::new();
    for line in notes.lines() {
        if line.trim_start().starts_with("TagDash ") && line.split_whitespace().count() == 2 {
            markers.push(line.trim().to_string());
        } else {
            text.push(line);
        }
    }
    (text.join("\n").trim_end().to_string(), markers)
}

pub fn join_marker(text: &str, markers: &[String]) -> String {
    if markers.is_empty() {
        return text.to_string();
    }
    let t = text.trim_end();
    if t.is_empty() {
        markers.join("\n")
    } else {
        format!("{t}\n\n{}", markers.join("\n"))
    }
}

pub fn append_block(current: &str, block: &str) -> String {
    let c = current.trim_end();
    if c.is_empty() {
        block.to_string()
    } else {
        format!("{c}\n\n{block}")
    }
}

// ─── Queue ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Op {
    pub op_id: i64,
    pub op: String,
    pub text: String,
    pub local_notes: String,
}

/// The trader's text once `ops` are applied over `base` (the server's, marker
/// stripped).
pub fn fold(base: &str, ops: &[Op]) -> String {
    ops.iter().fold(base.to_string(), |acc, o| match o.op.as_str() {
        "set_notes" => o.text.clone(),
        "append" => append_block(&acc, &o.text),
        _ => acc,
    })
}

pub fn ops_for(conn: &Connection, tt_id: &str) -> Vec<Op> {
    conn.prepare(
        "SELECT op_id, op, text, local_notes FROM review_outbox WHERE tt_id = ?1 ORDER BY op_id",
    )
    .and_then(|mut st| {
        st.query_map(params![tt_id], |r| {
            Ok(Op {
                op_id: r.get(0)?,
                op: r.get(1)?,
                text: r.get(2)?,
                local_notes: r.get(3)?,
            })
        })
        .map(|it| it.filter_map(|r| r.ok()).collect())
    })
    .unwrap_or_default()
}

/// The trader's text of `tt_id` as it reads locally: the last pending operation's,
/// else the cached mirror's.
pub fn local_notes(conn: &Connection, tt_id: &str) -> String {
    if let Some(o) = ops_for(conn, tt_id).pop() {
        return o.local_notes;
    }
    let raw: Option<String> = conn
        .query_row(
            "SELECT json_extract(raw_json, '$.notes') FROM tt_trades WHERE tt_id = ?1",
            params![tt_id],
            |r| r.get(0),
        )
        .ok()
        .flatten();
    split_marker(&raw.unwrap_or_default()).0
}

/// Queue one operation and apply it to the local copies at once. Returns the
/// trader's text as it now reads locally.
pub fn enqueue(conn: &Connection, tt_id: &str, op: &str, text: &str) -> Result<String, String> {
    let base = local_notes(conn, tt_id);
    let local = fold(
        &base,
        &[Op {
            op_id: 0,
            op: op.to_string(),
            text: text.to_string(),
            local_notes: String::new(),
        }],
    );
    conn.execute(
        "INSERT INTO review_outbox (tt_id, op, text, local_notes, created_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![tt_id, op, text, local, now_ms()],
    )
    .map_err(|e| e.to_string())?;
    // The mirror keeps its marker lines; the server's are re-read at send time.
    let raw: Option<String> = conn
        .query_row(
            "SELECT json_extract(raw_json, '$.notes') FROM tt_trades WHERE tt_id = ?1",
            params![tt_id],
            |r| r.get(0),
        )
        .ok()
        .flatten();
    let (_, markers) = split_marker(&raw.unwrap_or_default());
    mirror(conn, tt_id, &join_marker(&local, &markers), &local);
    Ok(local)
}

/// Mirror a notes change locally: the cached upstream object and, for a trade
/// TagDash placed, its journal entry (the next dictée appends to the edited text
/// rather than bringing the old one back).
pub fn mirror(conn: &Connection, tt_id: &str, full_notes: &str, user_text: &str) {
    let _ = conn.execute(
        "UPDATE tt_trades SET raw_json = json_set(raw_json, '$.notes', ?2) WHERE tt_id = ?1",
        params![tt_id, full_notes],
    );
    let locals: Vec<String> = conn
        .prepare("SELECT local_trade_id FROM tradetally_trade_ids WHERE tt_trade_id = ?1")
        .and_then(|mut st| {
            st.query_map(params![tt_id], |r| r.get::<_, String>(0))
                .map(|it| it.filter_map(|r| r.ok()).collect())
        })
        .unwrap_or_default();
    for local in locals {
        let existing = journal_repository::get(conn, &local).ok().flatten();
        let entry = JournalEntry {
            trade_id: local.clone(),
            symbol: existing.as_ref().map(|e| e.symbol.clone()).unwrap_or_default(),
            notes: user_text.to_string(),
            confidence: existing.as_ref().and_then(|e| e.confidence),
            tags: existing.map(|e| e.tags).unwrap_or_default(),
            updated_at: String::new(),
        };
        let _ = journal_repository::save(conn, &entry);
    }
}

/// Drop every pending operation of a trade (the trader gave up on sending them).
pub fn discard(conn: &Connection, tt_id: &str) -> Result<(), String> {
    conn.execute("DELETE FROM review_outbox WHERE tt_id = ?1", params![tt_id])
        .map(|_| ())
        .map_err(|e| e.to_string())
}

// ─── Status ─────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct OutboxTarget {
    pub tt_id: String,
    pub symbol: Option<String>,
    pub day: Option<String>,
    pub ops: i64,
    pub attempts: i64,
    pub last_error: Option<String>,
}

#[derive(Serialize)]
pub struct OutboxStatus {
    /// Pending operations, all trades together.
    pub pending: i64,
    pub targets: Vec<OutboxTarget>,
    /// TradeTally is unreachable right now (network failure, or offline mode).
    pub offline: bool,
    pub flushing: bool,
    pub last_sync_ms: Option<i64>,
}

pub fn status(conn: &Connection, offline_mode: bool) -> OutboxStatus {
    let targets: Vec<OutboxTarget> = conn
        .prepare(
            "SELECT o.tt_id, COUNT(*), MAX(o.attempts),
                    (SELECT last_error FROM review_outbox e
                      WHERE e.tt_id = o.tt_id AND e.last_error IS NOT NULL
                      ORDER BY e.op_id DESC LIMIT 1),
                    json_extract(t.raw_json, '$.symbol'),
                    substr(COALESCE(json_extract(t.raw_json, '$.entry_time'),
                                    json_extract(t.raw_json, '$.trade_date')), 1, 10)
               FROM review_outbox o
               LEFT JOIN tt_trades t ON t.tt_id = o.tt_id
              GROUP BY o.tt_id
              ORDER BY MIN(o.op_id)",
        )
        .and_then(|mut st| {
            st.query_map([], |r| {
                Ok(OutboxTarget {
                    tt_id: r.get(0)?,
                    ops: r.get(1)?,
                    attempts: r.get(2)?,
                    last_error: r.get(3)?,
                    symbol: r.get(4)?,
                    day: r.get(5)?,
                })
            })
            .map(|it| it.filter_map(|r| r.ok()).collect())
        })
        .unwrap_or_default();
    let last = LAST_SYNC.load(Ordering::SeqCst);
    OutboxStatus {
        pending: targets.iter().map(|t| t.ops).sum(),
        targets,
        offline: offline_mode || recently_offline(),
        flushing: FLUSHING.load(Ordering::SeqCst),
        last_sync_ms: (last > 0).then_some(last),
    }
}

// ─── Network ────────────────────────────────────────────────────────────────────

/// `TtClient` flattens an HTTP rejection into "HTTP <status>: …"; anything else is
/// the request not reaching the server at all.
fn is_network_error(e: &str) -> bool {
    !e.starts_with("HTTP ") && !e.starts_with("JSON parse")
}

fn note_result<T>(r: &Result<T, String>) {
    match r {
        Ok(_) => LAST_NET_FAIL.store(0, Ordering::SeqCst),
        Err(e) if is_network_error(e) => LAST_NET_FAIL.store(now_ms(), Ordering::SeqCst),
        Err(_) => {}
    }
}

/// TradeTally failed to answer at the network level a moment ago.
pub fn recently_offline() -> bool {
    let t = LAST_NET_FAIL.load(Ordering::SeqCst);
    t > 0 && now_ms() - t < OFFLINE_HOLD_MS
}

fn client() -> Result<TtClient, String> {
    let deps = super::deps().ok_or("revue non initialisée")?;
    let (base_url, token, email, password) = {
        let cfg = deps.config.read().unwrap();
        let sec = deps.secrets.read().unwrap();
        (
            cfg.tradetally.api_base_url.clone(),
            sec.tradetally_token.clone().unwrap_or_default(),
            sec.tradetally_email.clone(),
            sec.tradetally_password.clone(),
        )
    };
    if token.is_empty() {
        return Err("TradeTally token not set".into());
    }
    Ok(TtClient::new(base_url, token).with_session_creds(email, password))
}

fn offline_mode() -> bool {
    super::deps().is_some_and(|d| d.config.read().unwrap().data_source.offline)
}

fn notes_of(v: &Value) -> String {
    let trade = v.get("data").or_else(|| v.get("trade")).unwrap_or(v);
    trade
        .get("notes")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The server's notes of a trade (marker lines included), with a short timeout:
/// the sidebar is waiting on it.
pub async fn fetch_notes_quick(tt_id: &str) -> Result<String, String> {
    if offline_mode() {
        return Err("mode hors ligne".into());
    }
    if recently_offline() {
        return Err("TradeTally injoignable".into());
    }
    let r = client()?
        .get_json_within(&format!("/api/v1/trades/{tt_id}"), READ_TIMEOUT_SECS)
        .await;
    note_result(&r);
    r.map(|v| notes_of(&v))
}

// ─── Flush ──────────────────────────────────────────────────────────────────────

/// Send the pending operations now, in the background. Several calls collapse into
/// the flush already running (it goes round once more for what was just queued).
pub fn kick() {
    DIRTY.store(true, Ordering::SeqCst);
    if FLUSHING.load(Ordering::SeqCst) {
        return;
    }
    tauri::async_runtime::spawn(async {
        flush().await;
    });
}

/// Send every pending operation, one write per trade. Returns once the queue is
/// empty or TradeTally could not be reached. Never runs twice at once.
pub async fn flush() {
    loop {
        if FLUSHING.swap(true, Ordering::SeqCst) {
            return;
        }
        let mut reached;
        loop {
            DIRTY.store(false, Ordering::SeqCst);
            reached = flush_once().await;
            if !reached || !DIRTY.load(Ordering::SeqCst) {
                break;
            }
        }
        FLUSHING.store(false, Ordering::SeqCst);
        // Queued between the last pass and the release: its `kick` saw a flush
        // running and left it to this one.
        if !reached || !DIRTY.load(Ordering::SeqCst) {
            return;
        }
    }
}

/// One pass over the queue. False when TradeTally was unreachable (the rest waits
/// for the next retry rather than failing trade after trade).
async fn flush_once() -> bool {
    let Some(deps) = super::deps() else { return false };
    if offline_mode() {
        return false;
    }
    let ids: Vec<String> = {
        let conn = deps.db.lock().unwrap();
        conn.prepare("SELECT tt_id FROM review_outbox GROUP BY tt_id ORDER BY MIN(op_id)")
            .and_then(|mut st| {
                st.query_map([], |r| r.get::<_, String>(0))
                    .map(|it| it.filter_map(|r| r.ok()).collect())
            })
            .unwrap_or_default()
    };
    if ids.is_empty() {
        return true;
    }
    let client = match client() {
        Ok(c) => c,
        Err(e) => {
            let conn = deps.db.lock().unwrap();
            let _ = conn.execute(
                "UPDATE review_outbox SET last_error = ?1",
                params![e],
            );
            return false;
        }
    };
    let mut sent = 0usize;
    for tt_id in ids {
        let ops = {
            let conn = deps.db.lock().unwrap();
            ops_for(&conn, &tt_id)
        };
        let Some(max_id) = ops.last().map(|o| o.op_id) else { continue };
        let endpoint = format!("/api/v1/trades/{tt_id}");
        // Read at write time: whatever the notes hold now upstream is what the
        // operations are applied to.
        let res = async {
            let current = notes_of(&client.get_json(&endpoint).await?);
            let (user, markers) = split_marker(&current);
            let next_user = fold(&user, &ops);
            let full = join_marker(&next_user, &markers);
            if full != current {
                client.put_json(&endpoint, &json!({ "notes": full })).await?;
            }
            Ok::<_, String>((full, next_user))
        }
        .await;
        note_result(&res);
        let conn = deps.db.lock().unwrap();
        match res {
            Ok((full, next_user)) => {
                let _ = conn.execute(
                    "DELETE FROM review_outbox WHERE tt_id = ?1 AND op_id <= ?2",
                    params![tt_id, max_id],
                );
                // Operations queued during the write stay pending; the local copy
                // keeps showing them until their own turn.
                if ops_for(&conn, &tt_id).is_empty() {
                    mirror(&conn, &tt_id, &full, &next_user);
                }
                LAST_SYNC.store(now_ms(), Ordering::SeqCst);
                sent += 1;
            }
            Err(e) => {
                let _ = conn.execute(
                    "UPDATE review_outbox SET attempts = attempts + 1, last_error = ?3
                      WHERE tt_id = ?1 AND op_id <= ?2",
                    params![tt_id, max_id, e],
                );
                if is_network_error(&e) {
                    return false;
                }
                eprintln!("[tagdash] review outbox: {tt_id} refusé par TradeTally : {e}");
            }
        }
    }
    if sent > 0 {
        eprintln!("[tagdash] review outbox: {sent} trade(s) synchronisé(s) avec TradeTally");
    }
    true
}

/// Background retry: at launch, then every `RETRY_SECS` while something waits.
pub fn spawn_worker() {
    tauri::async_runtime::spawn(async {
        loop {
            let pending = super::deps().is_some_and(|d| {
                d.db.lock()
                    .unwrap()
                    .query_row("SELECT EXISTS(SELECT 1 FROM review_outbox)", [], |r| r.get::<_, bool>(0))
                    .unwrap_or(false)
            });
            if pending {
                flush().await;
            }
            tokio::time::sleep(Duration::from_secs(RETRY_SECS)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(op: &str, text: &str) -> Op {
        Op {
            op_id: 0,
            op: op.into(),
            text: text.into(),
            local_notes: String::new(),
        }
    }

    #[test]
    fn ops_fold_over_the_server_notes_at_send_time() {
        // A dictée landed upstream while offline: the queued review goes below it.
        let server = "Entrée trop tôt.\n\nDictée : sortie propre";
        let ops = [op("append", "Review 2026-10-05 : ok")];
        assert_eq!(
            fold(server, &ops),
            "Entrée trop tôt.\n\nDictée : sortie propre\n\nReview 2026-10-05 : ok"
        );
        // An edit replaces the text; a review queued after it goes below the edit.
        let ops = [
            op("append", "Review 2026-10-04 : bof"),
            op("set_notes", "Entrée propre."),
            op("append", "Review 2026-10-05 : ok"),
        ];
        assert_eq!(fold(server, &ops), "Entrée propre.\n\nReview 2026-10-05 : ok");
    }

    #[test]
    fn network_errors_are_told_from_rejections() {
        assert!(is_network_error("error sending request for url (https://x/api)"));
        assert!(!is_network_error("HTTP 404 Not Found: {}"));
    }

    #[test]
    fn enqueue_applies_locally_and_keeps_the_marker() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE tt_trades (tt_id TEXT PRIMARY KEY, raw_json TEXT);
             CREATE TABLE tradetally_trade_ids (local_trade_id TEXT, tt_trade_id TEXT);
             CREATE TABLE review_outbox (
                op_id INTEGER PRIMARY KEY AUTOINCREMENT, tt_id TEXT NOT NULL, op TEXT NOT NULL,
                text TEXT NOT NULL, local_notes TEXT NOT NULL, created_ms INTEGER NOT NULL,
                attempts INTEGER NOT NULL DEFAULT 0, last_error TEXT);
             INSERT INTO tt_trades VALUES ('T1', '{\"notes\":\"Trop tôt.\\n\\nTagDash L-1\"}');",
        )
        .unwrap();
        assert_eq!(local_notes(&conn, "T1"), "Trop tôt.");
        let after = enqueue(&conn, "T1", "append", "Review 2026-10-05 : ok").unwrap();
        assert_eq!(after, "Trop tôt.\n\nReview 2026-10-05 : ok");
        let after = enqueue(&conn, "T1", "set_notes", "Trop tôt, revu.").unwrap();
        assert_eq!(after, "Trop tôt, revu.");
        assert_eq!(local_notes(&conn, "T1"), "Trop tôt, revu.");
        let mirrored: String = conn
            .query_row("SELECT json_extract(raw_json, '$.notes') FROM tt_trades", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mirrored, "Trop tôt, revu.\n\nTagDash L-1");
        assert_eq!(ops_for(&conn, "T1").len(), 2);
    }
}
