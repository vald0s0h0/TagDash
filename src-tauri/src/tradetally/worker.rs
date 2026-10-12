// Background TradeTally sync worker.
// Drains the SQLite queue every 30 seconds.
// Never blocks the live scanner or the UI.

use std::sync::{Arc, Mutex, RwLock};
use tokio::time::{sleep, Duration};

use super::client::TtClient;
use crate::config::{secrets::Secrets, AppConfig};
use crate::local_db::tradetally_queue_repository;

const DRAIN_INTERVAL_SECS: u64 = 5;
const MAX_ATTEMPTS: u32 = 5;

pub async fn run(
    db: Arc<Mutex<rusqlite::Connection>>,
    config: Arc<RwLock<AppConfig>>,
    secrets: Arc<RwLock<Secrets>>,
) {
    // One-time cleanup of pre-v1 queued events (legacy endpoints can't be sent).
    {
        let conn = db.lock().unwrap();
        let _ = tradetally_queue_repository::purge_legacy(&conn);
    }
    // Drain immediately on startup, then on a short interval so trade events
    // reach TradeTally continuously without ever blocking the scanner/UI.
    loop {
        drain_once(&db, &config, &secrets).await;
        sleep(Duration::from_secs(DRAIN_INTERVAL_SECS)).await;
    }
}

async fn drain_once(
    db: &Arc<Mutex<rusqlite::Connection>>,
    config: &Arc<RwLock<AppConfig>>,
    secrets: &Arc<RwLock<Secrets>>,
) {
    let (base_url, token, tt_email, tt_password) = {
        let cfg = config.read().unwrap();
        let sec = secrets.read().unwrap();
        (
            cfg.tradetally.api_base_url.clone(),
            sec.tradetally_token.clone().unwrap_or_default(),
            sec.tradetally_email.clone(),
            sec.tradetally_password.clone(),
        )
    };

    if token.is_empty() {
        return;
    }

    // The /images route rejects the API token, so screenshots need the session
    // login. Known here, before the credentials move into the client.
    let has_session_creds = tt_email.as_deref().is_some_and(|e| !e.is_empty())
        && tt_password.as_deref().is_some_and(|p| !p.is_empty());

    let client = TtClient::new(base_url, token).with_session_creds(tt_email, tt_password);

    // Snapshot pending events (holds lock only briefly)
    let pending = {
        let conn = db.lock().unwrap();
        tradetally_queue_repository::get_pending(&conn).unwrap_or_default()
    };

    for event in pending {
        // Permanently exhaust events that have failed too many times
        if event.attempts >= MAX_ATTEMPTS {
            let conn = db.lock().unwrap();
            let _ = tradetally_queue_repository::mark_failed(
                &conn,
                &event.event_id,
                "max attempts reached",
            );
            continue;
        }

        // The trade's server id, when it already has one. Looked up for creates too:
        // a mapping on file means the trade exists upstream, so the create must not
        // be sent again whatever this event believes (see `dispatch`).
        let (tt_id_opt, create_still_queued) = {
            let conn = db.lock().unwrap();
            let mapped = tradetally_queue_repository::get_tt_trade_id(&conn, &event.trade_id)
                .ok()
                .flatten();
            let queued = mapped.is_none()
                && event.event_type != "trade_created"
                && tradetally_queue_repository::has_pending_create(&conn, &event.trade_id)
                    .unwrap_or(false);
            (mapped, queued)
        };

        let needs_tt_id = event.endpoint.contains("{TT_ID}");
        let mut tt_id_opt = tt_id_opt;
        if needs_tt_id && tt_id_opt.is_none() {
            if create_still_queued {
                // Legitimately early: the parent's create is queued behind this
                // event and will run in this same drain or the next one. Leave it
                // pending, untouched — no attempt spent on waiting.
                continue;
            }
            // No mapping, and no create is coming to produce one. The trade may still
            // exist upstream — the mapping can be lost to a DB reset or to a create
            // whose response hid the id — so ask the source of truth, by the marker
            // the trade carries in its notes, before giving up on this event.
            match client
                .find_trade_id_by_marker(&event.symbol, &super::marker_for(&event.trade_id))
                .await
            {
                Ok(Some(found)) => {
                    let conn = db.lock().unwrap();
                    let _ = tradetally_queue_repository::save_tt_trade_id(
                        &conn,
                        &event.trade_id,
                        &found,
                    );
                    tt_id_opt = Some(found);
                }
                Ok(None) => {
                    // Not upstream either: this event can never be addressed. Fail it
                    // rather than skip it, because `get_pending` reads the oldest 100
                    // pending rows — a growing set of permanently unaddressable events
                    // would fill that window and starve every new event behind it.
                    let conn = db.lock().unwrap();
                    let _ = tradetally_queue_repository::mark_failed(
                        &conn,
                        &event.event_id,
                        "trade introuvable dans TradeTally (supprimé ou marqueur TagDash écrasé)",
                    );
                    continue;
                }
                Err(e) => {
                    // Couldn't ask (offline, auth). Record the failure rather than
                    // skipping: a failed event is visible in Réglages → Sync and can be
                    // replayed from there, whereas a skipped one spins silently forever
                    // and eats a slot in the pending window.
                    let conn = db.lock().unwrap();
                    let _ = tradetally_queue_repository::mark_failed(
                        &conn,
                        &event.event_id,
                        &format!("id TradeTally non résolu: {e}"),
                    );
                    continue;
                }
            }
        }

        let resolved_endpoint = match tt_id_opt {
            Some(ref tt_id) => event.endpoint.replace("{TT_ID}", tt_id),
            None => event.endpoint.clone(),
        };

        let result = dispatch(
            &client,
            &event.event_type,
            &resolved_endpoint,
            &event.payload_summary,
            &event.trade_id,
            &event.symbol,
            tt_id_opt.as_deref(),
        )
        .await;

        let conn = db.lock().unwrap();
        match result {
            Ok(maybe_tt_id) => {
                if let Some(ref tt_id) = maybe_tt_id {
                    let _ = tradetally_queue_repository::save_tt_trade_id(
                        &conn,
                        &event.trade_id,
                        tt_id,
                    );
                }
                let _ = tradetally_queue_repository::mark_success(&conn, &event.event_id);
                // The trade now EXISTS upstream, so anything written about it while it
                // was still a plan can finally be addressed to it.
                if event.event_type == "trade_created" {
                    flush_deferred_for_trade(
                        &conn,
                        &event.trade_id,
                        &event.symbol,
                        has_session_creds,
                    );
                }
            }
            Err(ref e) => {
                let _ = tradetally_queue_repository::mark_failed(&conn, &event.event_id, e);
            }
        }
    }
}

/// Send what was written about a trade BEFORE the trade existed in TradeTally.
///
/// A note or a screenshot can legitimately be produced on a trade that is still only
/// a plan — levels drawn, nothing entered. Queued then, it would be addressed to a
/// trade the server has never heard of, fail permanently ("trade introuvable") and
/// the text would be lost even though it sits safely in the local tables. So those
/// two are deliberately NOT queued at write time (see `commands::save_journal_entry`
/// and `save_screenshot_local`); they wait here, and this runs the instant the
/// trade's create lands.
///
/// Idempotent: a note is re-sent as an update (same endpoint the journal already
/// uses), and a screenshot is only picked up while its row is still marked
/// un-queued.
fn flush_deferred_for_trade(
    conn: &rusqlite::Connection,
    trade_id: &str,
    symbol: &str,
    with_images: bool,
) {
    if let Ok(Some(entry)) = crate::local_db::journal_repository::get(conn, trade_id) {
        if !entry.notes.trim().is_empty() {
            super::enqueue_note_updated(
                conn,
                trade_id,
                symbol,
                &entry.notes,
                entry.confidence,
                &entry.tags,
            );
        }
    }

    if !with_images {
        return; // no session login: the /images route would reject every one of them
    }
    let paths: Vec<String> = conn
        .prepare("SELECT path FROM screenshot_files WHERE trade_id = ?1 AND uploaded = 0")
        .and_then(|mut stmt| {
            let rows = stmt.query_map(rusqlite::params![trade_id], |r| r.get::<_, String>(0))?;
            rows.collect()
        })
        .unwrap_or_default();
    for path in &paths {
        super::enqueue_chart_updated(conn, trade_id, symbol, path);
    }
    if !paths.is_empty() {
        let _ = conn.execute(
            "UPDATE screenshot_files SET uploaded = 1 WHERE trade_id = ?1 AND uploaded = 0",
            rusqlite::params![trade_id],
        );
    }
}

// Returns Ok(Some(tt_id)) for trade creation events (so caller can persist the mapping).
#[allow(clippy::too_many_arguments)]
async fn dispatch(
    client: &TtClient,
    event_type: &str,
    endpoint: &str,
    payload_json: &str,
    trade_id: &str,
    symbol: &str,
    known_tt_id: Option<&str>,
) -> Result<Option<String>, String> {
    let payload: serde_json::Value =
        serde_json::from_str(payload_json).map_err(|e| format!("invalid payload JSON: {e}"))?;

    match event_type {
        // Creates the trade and returns the server id (persisted as the mapping).
        //
        // POST /api/v1/trades is not idempotent and TradeTally offers no
        // idempotency key, so every path that could send it twice is closed here
        // instead — a duplicated create is a duplicated trade in the journal, and
        // nothing downstream can tell the copy from the original:
        //
        // 1. A mapping already on file → the trade exists; don't send anything.
        // 2. A POST that errors may still have been committed upstream (a 15 s
        //    timeout, an unparsable body, a 502 from the reverse proxy). Before
        //    reporting failure — which is what "Rejouer les échecs" would later
        //    POST again — look the trade up by its TagDash marker. Found means the
        //    create landed: record the id and call it a success.
        "trade_created" => {
            if let Some(existing) = known_tt_id {
                return Ok(Some(existing.to_string()));
            }
            match client.post_json(endpoint, &payload).await {
                Ok(resp) => match extract_tt_id(&resp) {
                    Some(id) => Ok(Some(id)),
                    // Created, but the response shape hid the id. Recover it from
                    // the marker, or the trade stays unmappable and every later
                    // event for it dies waiting for an id that never comes.
                    None => recover_created_id(client, symbol, trade_id)
                        .await
                        .map(Some)
                        .map_err(|e| format!("trade créé mais id introuvable: {e}")),
                },
                Err(post_err) => match client
                    .find_trade_id_by_marker(symbol, &super::marker_for(trade_id))
                    .await
                {
                    Ok(Some(id)) => Ok(Some(id)),
                    // Not upstream → the POST really did fail; report it so it can
                    // be retried safely.
                    Ok(None) => Err(post_err),
                    // Couldn't check → report the original failure. A retry may
                    // duplicate, but guard 1 plus the marker lookup catch it on the
                    // next attempt; silently swallowing the error would lose the trade.
                    Err(_) => Err(post_err),
                },
            }
        }
        // The journal note replaces `notes` upstream, which is also where the local
        // trade id is recorded. Re-attach the marker here, at the last point before
        // the wire, so it survives every note edit — and so events queued before this
        // existed are covered too. Losing it means losing the only link to this trade
        // that outlives the local DB.
        "note_updated" => {
            let mut payload = payload;
            let notes = payload
                .get("notes")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            payload["notes"] = serde_json::Value::String(super::notes_with_marker(&notes, trade_id));
            client.put_json(endpoint, &payload).await?;
            Ok(None)
        }
        // All other updates PUT the (partial) trade body to /api/v1/trades/{TT_ID}.
        "fill_added" | "trade_closed" | "levels_updated" => {
            client.put_json(endpoint, &payload).await?;
            Ok(None)
        }
        // Diary card: create-or-update today's TradeTally diary entry. Token-first
        // with a session-login fallback (the /api/diary route uses session auth).
        "diary_entry" => {
            let entry_date = payload
                .get("entryDate")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let title = payload
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let content = payload
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            client
                .create_diary_entry(entry_date, title, content)
                .await?;
            Ok(None)
        }
        // Screenshot: read the local PNG and upload it via a session login
        // (multipart) to /api/trades/{id}/images.
        "chart_updated" => {
            let local_path = payload
                .get("localPath")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            // The queue keeps the path as the database does (relative to the
            // user folder), so a queued upload survives the data moving.
            let file = crate::paths::from_stored(local_path);
            client.upload_images_session(endpoint, &file.to_string_lossy()).await?;
            Ok(None)
        }
        other => Err(format!("unknown event_type: {other}")),
    }
}

/// Look up the id of the trade we just created, by the marker its notes carry.
async fn recover_created_id(
    client: &TtClient,
    symbol: &str,
    trade_id: &str,
) -> Result<String, String> {
    client
        .find_trade_id_by_marker(symbol, &super::marker_for(trade_id))
        .await?
        .ok_or_else(|| "marqueur TagDash absent des trades du symbole".to_string())
}

fn extract_tt_id(resp: &serde_json::Value) -> Option<String> {
    // Try the most common TradeTally response shapes
    for ptr in &["/id", "/uuid", "/data/id", "/data/uuid", "/trade/id"] {
        if let Some(id) = resp.pointer(ptr).and_then(|v| v.as_str()) {
            return Some(id.to_string());
        }
    }
    None
}
