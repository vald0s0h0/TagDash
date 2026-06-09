// Background TradeTally sync worker.
// Drains the SQLite queue every 30 seconds.
// Never blocks the live scanner or the UI.

use std::sync::{Arc, Mutex, RwLock};
use tokio::time::{sleep, Duration};

use crate::config::{secrets::Secrets, AppConfig};
use crate::local_db::tradetally_queue_repository;
use super::client::TtClient;

const DRAIN_INTERVAL_SECS: u64 = 5;
const MAX_ATTEMPTS: u32 = 5;

pub async fn run(
    db:      Arc<Mutex<rusqlite::Connection>>,
    config:  Arc<RwLock<AppConfig>>,
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
    db:      &Arc<Mutex<rusqlite::Connection>>,
    config:  &Arc<RwLock<AppConfig>>,
    secrets: &Arc<RwLock<Secrets>>,
) {
    let (base_url, token, mock_mode, mock_fail, mock_delay, tt_email, tt_password) = {
        let cfg = config.read().unwrap();
        let sec = secrets.read().unwrap();
        (
            cfg.tradetally.api_base_url.clone(),
            sec.tradetally_token.clone().unwrap_or_default(),
            cfg.tradetally.mock_mode,
            cfg.tradetally.mock_fail,
            cfg.tradetally.mock_delay_ms,
            sec.tradetally_email.clone(),
            sec.tradetally_password.clone(),
        )
    };

    if token.is_empty() && !mock_mode {
        return; // No credentials and not in mock mode — skip
    }

    let client = TtClient::new(base_url, token, mock_mode)
        .with_mock_options(mock_fail, mock_delay)
        .with_session_creds(tt_email, tt_password);

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
                &conn, &event.event_id, "max attempts reached",
            );
            continue;
        }

        // Resolve {TT_ID} placeholder if present
        let needs_tt_id = event.endpoint.contains("{TT_ID}");
        let tt_id_opt: Option<String> = if needs_tt_id {
            let conn = db.lock().unwrap();
            tradetally_queue_repository::get_tt_trade_id(&conn, &event.trade_id)
                .ok()
                .flatten()
        } else {
            None
        };

        if needs_tt_id && tt_id_opt.is_none() {
            // The parent trade has not been created in TradeTally yet.
            // Leave this event as pending — it will be picked up once
            // trade_id_created succeeds.
            continue;
        }

        let resolved_endpoint = match tt_id_opt {
            Some(ref tt_id) => event.endpoint.replace("{TT_ID}", tt_id),
            None            => event.endpoint.clone(),
        };

        let result = dispatch(
            &client,
            &event.event_type,
            &resolved_endpoint,
            &event.payload_summary,
        ).await;

        let conn = db.lock().unwrap();
        match result {
            Ok(maybe_tt_id) => {
                if let Some(ref tt_id) = maybe_tt_id {
                    let _ = tradetally_queue_repository::save_tt_trade_id(
                        &conn, &event.trade_id, tt_id,
                    );
                }
                let _ = tradetally_queue_repository::mark_success(&conn, &event.event_id);
            }
            Err(ref e) => {
                let _ = tradetally_queue_repository::mark_failed(&conn, &event.event_id, e);
            }
        }
    }
}

// Returns Ok(Some(tt_id)) for trade creation events (so caller can persist the mapping).
async fn dispatch(
    client:       &TtClient,
    event_type:   &str,
    endpoint:     &str,
    payload_json: &str,
) -> Result<Option<String>, String> {
    let payload: serde_json::Value = serde_json::from_str(payload_json)
        .map_err(|e| format!("invalid payload JSON: {e}"))?;

    match event_type {
        // Creates the trade and returns the server id (persisted as the mapping).
        "trade_created" => {
            let resp = client.post_json(endpoint, &payload).await?;
            Ok(extract_tt_id(&resp))
        }
        // All updates PUT the (partial) trade body to /api/v1/trades/{TT_ID}.
        "fill_added" | "trade_closed" | "levels_updated" | "note_updated" => {
            client.put_json(endpoint, &payload).await?;
            Ok(None)
        }
        // Screenshot: read the local PNG and upload it via a session login
        // (multipart) to /api/trades/{id}/images.
        "chart_updated" => {
            let local_path = payload.get("localPath").and_then(|v| v.as_str()).unwrap_or_default();
            client.upload_images_session(endpoint, local_path).await?;
            Ok(None)
        }
        other => Err(format!("unknown event_type: {other}")),
    }
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
