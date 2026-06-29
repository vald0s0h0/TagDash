// TradeTally integration. All communication is asynchronous and out-of-band.
// Commands only write to the SQLite queue; the background worker drains it.
// The live scanner path never touches this module.

pub mod client;
pub mod payloads;
pub mod worker;

pub use client::TtClient;

use chrono::Utc;
use rand::Rng;
use rusqlite::Connection;
use serde_json::json;

use crate::config::AppConfig;
use crate::local_db::tradetally_queue_repository::{self, SyncQueueRow};
use crate::types::{Fill, Side};

// ─── Tags ────────────────────────────────────────────────────────────────────

use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct TagsApiResponse {
    // TradeTally may return {tags: [...]} or directly [...]
    tags: Option<Vec<TagObject>>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum TagObject {
    Named { name: String },
    Plain(String),
}

impl TagObject {
    fn into_string(self) -> String {
        match self {
            TagObject::Named { name } => name,
            TagObject::Plain(s) => s,
        }
    }
}

/// Fetch tags from TradeTally API. Returns names only.
pub async fn fetch_tags(token: &str, base_url: &str) -> Result<Vec<String>, String> {
    let client = crate::http::client();
    let url = format!("{base_url}/api/tags");
    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {token}"))
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        return Err(format!("TradeTally HTTP {}", resp.status()));
    }

    let text = resp.text().await.map_err(|e| e.to_string())?;

    // Try array-of-objects first, then array-of-strings, then wrapped object
    if let Ok(tags) = serde_json::from_str::<Vec<TagObject>>(&text) {
        return Ok(tags.into_iter().map(TagObject::into_string).collect());
    }
    if let Ok(wrapper) = serde_json::from_str::<TagsApiResponse>(&text) {
        if let Some(tags) = wrapper.tags {
            return Ok(tags.into_iter().map(TagObject::into_string).collect());
        }
    }
    Err(format!("unrecognised tags response: {}", &text[..text.len().min(200)]))
}

pub fn mock_tags() -> Vec<String> {
    vec![
        "frd".into(), "news".into(), "low_float".into(), "hod_break".into(),
        "rvol_spike".into(), "gap_up".into(), "short_squeeze".into(), "catalyst".into(),
        "halt_resume".into(), "reversal".into(), "momentum".into(), "pre_news".into(),
    ]
}

// ─── Queue event enqueue helpers ─────────────────────────────────────────────

fn new_event_id() -> String {
    let ms  = Utc::now().timestamp_millis();
    let rnd: u32 = rand::thread_rng().gen();
    format!("{ms}-{rnd:08x}")
}

/// Low-level enqueue: stores a JSON payload in the sync queue.
pub fn enqueue_event(
    conn:       &Connection,
    event_type: &str,
    trade_id:   &str,
    symbol:     &str,
    endpoint:   &str,
    payload:    serde_json::Value,
) {
    let now = Utc::now().to_rfc3339();
    let row = SyncQueueRow {
        event_id:        new_event_id(),
        timestamp:       now.clone(),
        trade_id:        trade_id.to_string(),
        symbol:          symbol.to_string(),
        event_type:      event_type.to_string(),
        endpoint:        endpoint.to_string(),
        payload_summary: payload.to_string(),
        status:          "pending".into(),
        error_message:   None,
        attempts:        0,
        created_at:      now,
    };
    let _ = tradetally_queue_repository::enqueue(conn, &row);
}

// ─── Typed event helpers (TradeTally REST API v1) ───────────────────────────
//
// A TradeTally trade is a single record (POST /api/v1/trades) with an
// `executions` array. The write API expects camelCase field names. All update
// events PUT to /api/v1/trades/{TT_ID}; the worker resolves {TT_ID} from the
// local→server id mapping once the trade has been created.
//
// Lifecycle: first fill → trade_created (POST, yields the server id) →
// scale-in fills → fill_added (PUT) → SL/TP moves → levels_updated (PUT) →
// position flat → trade_closed (PUT, pnl auto-computed by TradeTally).

const V1_CREATE: &str = "/api/v1/trades";
const V1_UPDATE: &str = "/api/v1/trades/{TT_ID}";

/// Per-share commission for a given share count. No fixed per-trade fee:
/// `|shares| * commission_per_share`.
fn commission_for(shares: i64, cfg: &AppConfig) -> f64 {
    shares.unsigned_abs() as f64 * cfg.trading.commission_per_share
}

/// Round a price level to 2 decimals for the journal (e.g. 12.2315 → 12.23).
/// `None` (no level set) passes through untouched.
fn round_level(price: Option<f64>) -> Option<f64> {
    price.map(|p| (p * 100.0).round() / 100.0)
}

/// Build the `executions` array from the internal fills. Side maps directly:
/// Long fill = "buy", Short fill = "sell". Commission is per-share.
fn executions_json(fills: &[Fill], cfg: &AppConfig) -> Vec<serde_json::Value> {
    fills
        .iter()
        .map(|f| {
            let action = if f.side == Side::Long { "buy" } else { "sell" };
            json!({
                "action":     action,
                "price":      f.fill_price,
                "quantity":   f.quantity,
                "datetime":   f.filled_at.to_rfc3339(),
                "commission": commission_for(f.quantity, cfg),
                "fees":       cfg.trading.default_fees,
            })
        })
        .collect()
}

/// First fill of a trade → create the trade in TradeTally.
#[allow(clippy::too_many_arguments)]
pub fn enqueue_trade_created(
    conn:          &Connection,
    trade_id:      &str,
    symbol:        &str,
    strategy_name: &str,
    side:          Side,
    entry_price:   f64,
    quantity:      i64,
    entry_time:    &str, // RFC3339
    stop_loss:     Option<f64>,
    take_profit:   Option<f64>,
    all_fills:     &[Fill],
    cfg:           &AppConfig,
) {
    let side_str = if side == Side::Long { "long" } else { "short" };
    let payload = json!({
        "symbol":         symbol,
        "side":           side_str,
        "entryTime":      entry_time,
        "entryPrice":     entry_price,
        "quantity":       quantity,
        "commission":     commission_for(quantity, cfg),
        "fees":           cfg.trading.default_fees,
        "stopLoss":       round_level(stop_loss),
        "takeProfit":     round_level(take_profit),
        "notes":          format!("TagDash {trade_id}"),
        "setup":          strategy_name,
        "broker":         cfg.trading.default_broker,
        "instrumentType": "stock",
        "tags":           Vec::<String>::new(),
        "executions":     executions_json(all_fills, cfg),
    });
    enqueue_event(conn, "trade_created", trade_id, symbol, V1_CREATE, payload);
}

/// Subsequent entry fill (scale-in) → update executions + avg entry + net qty.
pub fn enqueue_fill_added(
    conn:      &Connection,
    trade_id:  &str,
    symbol:    &str,
    avg_entry: f64,
    quantity:  i64,
    all_fills: &[Fill],
    cfg:       &AppConfig,
) {
    let payload = json!({
        "entryPrice": avg_entry,
        "quantity":   quantity,
        "executions": executions_json(all_fills, cfg),
    });
    enqueue_event(conn, "fill_added", trade_id, symbol, V1_UPDATE, payload);
}

/// Position went flat → close the trade. TradeTally computes pnl from entry/exit.
/// `mae`/`mfe` are the trade's max adverse / favorable excursions in dollars
/// (positive magnitudes) observed while the position was open.
#[allow(clippy::too_many_arguments)]
pub fn enqueue_trade_closed(
    conn:       &Connection,
    trade_id:   &str,
    symbol:     &str,
    exit_price: f64,
    exit_time:  &str, // RFC3339
    exit_qty:   i64,
    mae:        Option<f64>,
    mfe:        Option<f64>,
    all_fills:  &[Fill],
    cfg:        &AppConfig,
) {
    let payload = json!({
        "exitPrice":      exit_price,
        "exitTime":       exit_time,
        "exitCommission": commission_for(exit_qty, cfg),
        "mae":            mae,
        "mfe":            mfe,
        "executions":     executions_json(all_fills, cfg),
    });
    enqueue_event(conn, "trade_closed", trade_id, symbol, V1_UPDATE, payload);
}

/// TP line moved after entry → update the take-profit only.
///
/// The journal's `stopLoss` is intentionally frozen at the value recorded in
/// `trade_created` (the SL at the exact moment the position opened). The SL may
/// still be moved on the chart afterwards to drive the live bracket order, but
/// that later value must never reach TradeTally — so this partial PUT carries
/// `takeProfit` only and deliberately omits `stopLoss`.
pub fn enqueue_levels_updated(
    conn:        &Connection,
    trade_id:    &str,
    symbol:      &str,
    take_profit: Option<f64>,
) {
    let payload = json!({ "takeProfit": round_level(take_profit) });
    enqueue_event(conn, "levels_updated", trade_id, symbol, V1_UPDATE, payload);
}

/// Journal saved → update notes / confidence / tags on the trade.
pub fn enqueue_note_updated(
    conn:       &Connection,
    trade_id:   &str,
    symbol:     &str,
    notes:      &str,
    confidence: Option<i32>,
    tags:       &[String],
) {
    let payload = json!({
        "notes":      notes,
        "confidence": confidence,
        "tags":       tags,
    });
    enqueue_event(conn, "note_updated", trade_id, symbol, V1_UPDATE, payload);
}

/// Journal/diary card → create-or-update today's TradeTally diary entry. Not tied
/// to a trade (uses the `/api/diary` route, `createOrUpdateEntry`). Routed through
/// the same resilient queue as trade events so a transient failure just retries.
pub fn enqueue_diary_entry(
    conn:       &Connection,
    entry_date: &str,
    title:      &str,
    content:    &str,
) {
    let payload = json!({
        "entryDate": entry_date,
        "title":     title,
        "content":   content,
    });
    enqueue_event(conn, "diary_entry", "diary", "diary", "/api/diary", payload);
}

/// Screenshot captured → upload it to the trade's image gallery. Only the local
/// file path is queued; the worker logs in (session) and POSTs the PNG at send
/// time. Uses the non-v1 /images route (the only image-upload endpoint).
pub fn enqueue_chart_updated(
    conn:       &Connection,
    trade_id:   &str,
    symbol:     &str,
    local_path: &str,
) {
    let payload = json!({ "localPath": local_path });
    enqueue_event(conn, "chart_updated", trade_id, symbol, "/api/trades/{TT_ID}/images", payload);
}
