// Alpaca `trade_updates` websocket — the authoritative execution feed.
//
// This is Alpaca's recommended way to learn about executions, and the reason the
// TagDash broker's synthetic fills are switched off entirely when Alpaca is the
// active broker: a fill is an event the VENUE emits, carrying the real price, the
// real quantity, and the resulting position size. Nothing is inferred from a chart.
//
// Endpoint is the TRADING host, not the market-data one:
//   wss://paper-api.alpaca.markets/stream   |   wss://api.alpaca.markets/stream
//
// Handshake, in order:
//   → {"action":"authenticate","data":{"key_id":…,"secret_key":…}}
//   ← {"stream":"authorization","data":{"status":"authorized"}}
//   → {"action":"listen","data":{"streams":["trade_updates"]}}
//   ← {"stream":"trade_updates","data":{"event":"fill","order":{…},…}}
//
// Every event carries the FULL order object, so the mirror is advanced from the
// venue's own view rather than patched field by field. Events that change money —
// fills — additionally kick a full REST reconcile, because the event does not carry
// the position's new average entry price.
//
// A reconnect is not a silent recovery: any event missed while disconnected is
// picked up by the periodic reconcile in `broker::spawn`, which is why that exists.

use std::sync::{Arc, RwLock};

use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::Notify;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

use super::alpaca_rest::{map_status, Creds};
use super::journal::{BrokerJournal, FillEvent};
use super::{BrokerMirror, Environment, ShortCostBasis};
use crate::chart_state::ChartState;
use crate::config::AppConfig;
use crate::types::Side;

/// Backoff between reconnect attempts. Kept short: while this socket is down,
/// executions are only seen at the reconcile cadence.
const RECONNECT_DELAY: std::time::Duration = std::time::Duration::from_secs(3);

/// Ceiling for the reconnect backoff. Reached only when the failure is persistent —
/// wrong keys being the usual case — where retrying every 3 s earns an HTTP 429 from
/// Alpaca and turns a fixable credentials problem into a rate-limited account.
const RECONNECT_DELAY_MAX: std::time::Duration = std::time::Duration::from_secs(60);

fn stream_url(env: Environment) -> &'static str {
    match env {
        Environment::Paper => "wss://paper-api.alpaca.markets/stream",
        Environment::Live => "wss://api.alpaca.markets/stream",
    }
}

#[derive(Debug, Deserialize)]
struct Envelope {
    stream: Option<String>,
    data: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct TradeUpdate {
    event: String,
    order: Option<OrderPayload>,
    /// Execution price of THIS fill (absent on non-fill events).
    price: Option<String>,
    /// Shares in THIS fill — a partial reports only its own slice.
    qty: Option<String>,
    /// The venue's resulting position size after the fill, signed.
    position_qty: Option<String>,
    /// When the venue booked it.
    timestamp: Option<String>,
}

/// The order object embedded in a trade update. Same shape as the REST order, but
/// only the fields the mirror needs are named.
#[derive(Debug, Deserialize)]
struct OrderPayload {
    id: String,
    client_order_id: Option<String>,
    symbol: String,
    side: Option<String>,
    #[serde(rename = "type")]
    order_type: Option<String>,
    time_in_force: Option<String>,
    qty: Option<String>,
    filled_qty: Option<String>,
    limit_price: Option<String>,
    stop_price: Option<String>,
    filled_avg_price: Option<String>,
    status: Option<String>,
    legs: Option<serde_json::Value>,
    submitted_at: Option<String>,
    filled_at: Option<String>,
    canceled_at: Option<String>,
}

fn num(s: &Option<String>) -> Option<f64> {
    s.as_ref().and_then(|v| v.parse::<f64>().ok())
}

fn ts(s: &Option<String>) -> Option<chrono::DateTime<chrono::Utc>> {
    s.as_ref()
        .and_then(|v| chrono::DateTime::parse_from_rfc3339(v).ok())
        .map(|d| d.with_timezone(&chrono::Utc))
}

/// Events that moved shares, so the position's average price changed and only a
/// REST read knows the new one.
pub fn changes_position(event: &str) -> bool {
    matches!(event, "fill" | "partial_fill")
}

/// Turn one trade update into the order the mirror should adopt.
///
/// The event's own `event` name wins over the embedded `status` when it is
/// terminal: Alpaca sometimes emits `event:"canceled"` while the nested order still
/// reads `pending_cancel`, and trusting the stale nested value would leave a
/// cancelled order showing as live.
pub fn to_order(update: &str, payload_status: Option<&str>) -> super::BrokerOrderStatus {
    match update {
        "fill" => super::BrokerOrderStatus::Filled,
        "canceled" | "expired" | "rejected" | "done_for_day" | "replaced" => {
            super::BrokerOrderStatus::Closed
        }
        _ => map_status(payload_status.unwrap_or("")),
    }
}

fn build_order(p: &OrderPayload, event: &str) -> super::BrokerOrder {
    super::BrokerOrder {
        id: p.id.clone(),
        client_order_id: p.client_order_id.clone().unwrap_or_default(),
        symbol: p.symbol.clone(),
        side: p.side.clone().unwrap_or_default(),
        order_type: p.order_type.clone().unwrap_or_default(),
        time_in_force: p.time_in_force.clone().unwrap_or_default(),
        quantity: num(&p.qty).unwrap_or(0.0),
        filled_quantity: num(&p.filled_qty).unwrap_or(0.0),
        limit_price: num(&p.limit_price),
        stop_price: num(&p.stop_price),
        filled_avg_price: num(&p.filled_avg_price),
        status: to_order(event, p.status.as_deref()),
        raw_status: p.status.clone().unwrap_or_else(|| event.to_string()),
        parent_id: None,
        submitted_at: ts(&p.submitted_at),
        filled_at: ts(&p.filled_at),
        canceled_at: ts(&p.canceled_at),
        reject_reason: (event == "rejected").then(|| "refusé par Alpaca".to_string()),
    }
}

/// Everything the journal needs to turn a venue execution into a TradeTally event.
#[derive(Clone)]
pub struct JournalDeps {
    pub journal: Arc<RwLock<BrokerJournal>>,
    pub db: Arc<std::sync::Mutex<rusqlite::Connection>>,
    pub chart: Arc<RwLock<ChartState>>,
    pub protection: Arc<RwLock<super::protection::ProtectionBook>>,
    pub locate_plans: Arc<RwLock<std::collections::HashMap<String, ShortCostBasis>>>,
}

/// Shares from this execution that actually increased a short position. Using the
/// signed position before/after the fill handles ordinary partial fills, scale-ins,
/// and a sell that first closes a long before crossing through zero.
fn newly_opened_short_quantity(side: Side, fill_qty: f64, resulting_position: f64) -> i64 {
    if side != Side::Short || fill_qty <= 0.0 {
        return 0;
    }
    let previous_position = resulting_position + fill_qty;
    ((-resulting_position).max(0.0) - (-previous_position).max(0.0))
        .max(0.0)
        .ceil() as i64
}

fn levels_from_payload(payload: &OrderPayload) -> (Option<f64>, Option<f64>) {
    let Some(legs) = payload.legs.as_ref().and_then(|v| v.as_array()) else {
        return (None, None);
    };
    let mut sl = None;
    let mut tp = None;
    for leg in legs {
        let kind = leg.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let number = |key: &str| {
            leg.get(key)
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<f64>().ok())
        };
        match kind {
            "stop" | "stop_limit" | "trailing_stop" => sl = sl.or_else(|| number("stop_price")),
            "limit" => tp = tp.or_else(|| number("limit_price")),
            _ => {}
        }
    }
    (sl, tp)
}

/// The strategy label a trade is filed under.
///
/// Taken from the chart context the user set up for the ticker, so an Alpaca trade
/// lands in TradeTally under the same setup name an internal one would. Falls back to
/// the broker's name rather than inventing a strategy.
fn strategy_name_for(chart: &Arc<RwLock<ChartState>>, symbol: &str) -> String {
    let id = chart
        .read()
        .unwrap()
        .get_context(symbol)
        .map(|c| c.strategy_id)
        .filter(|s| !s.is_empty());
    match id {
        Some(id) => crate::strategies::registry::all_strategies()
            .iter()
            .find(|s| s.id() == id)
            .map(|s| s.name().to_string())
            .unwrap_or(id),
        None => "Alpaca".to_string(),
    }
}

/// Connect, authenticate, listen, and keep the mirror advancing until the selected
/// account changes.
pub fn spawn(
    creds: Creds,
    mirror: Arc<RwLock<BrokerMirror>>,
    kick: Arc<Notify>,
    config: Arc<RwLock<AppConfig>>,
    account_id: String,
    deps: JournalDeps,
) {
    // Same runtime as the rest of the app, and safe to call from either a sync or an
    // async context — see the note in `broker::spawn`.
    tauri::async_runtime::spawn(async move {
        let url = stream_url(creds.environment);
        let mut backoff = RECONNECT_DELAY;
        loop {
            // Stop as soon as the user selects another account — a live socket must
            // never outlive the selection that authorised it.
            if config.read().unwrap().broker.account_id() != account_id {
                eprintln!("[tagdash] broker stream: compte changé, arrêt de {account_id}");
                mirror.write().unwrap().stream_connected = false;
                return;
            }

            let outcome =
                run_session(&creds, url, &mirror, &kick, &config, &account_id, &deps).await;
            // Read the flag before the error arm clears it: it is the only evidence
            // that Alpaca acknowledged the trade_updates subscription.
            let subscribed = mirror.read().unwrap().stream_connected;
            match outcome {
                Ok(()) => {}
                Err(e) => {
                    eprintln!("[tagdash] broker stream ({account_id}) : {e}");
                    let mut m = mirror.write().unwrap();
                    m.last_error = Some(e);
                }
            }
            mirror.write().unwrap().stream_connected = false;
            // A session that got as far as authenticating proves the credentials and
            // the host are fine, so the next drop deserves a fast retry. One that
            // never connected does not — back off, doubling to the cap, otherwise a
            // wrong key has us reconnecting every 3 s until Alpaca answers 429.
            backoff = if subscribed {
                RECONNECT_DELAY
            } else {
                (backoff * 2).min(RECONNECT_DELAY_MAX)
            };
            // Reconcile on the way back up: whatever was missed while down is
            // recovered from REST, not guessed.
            kick.notify_one();
            tokio::time::sleep(backoff).await;
        }
    });
}

#[allow(clippy::too_many_arguments)]
async fn run_session(
    creds: &Creds,
    url: &str,
    mirror: &Arc<RwLock<BrokerMirror>>,
    kick: &Arc<Notify>,
    config: &Arc<RwLock<AppConfig>>,
    account_id: &str,
    deps: &JournalDeps,
) -> Result<(), String> {
    let (ws, _) = connect_async(url)
        .await
        .map_err(|e| format!("connexion refusée : {e}"))?;
    let (mut write, mut read) = ws.split();

    write
        .send(Message::Text(
            json!({
                "action": "authenticate",
                "data": { "key_id": creds.key, "secret_key": creds.secret }
            })
            .to_string(),
        ))
        .await
        .map_err(|e| format!("authentification impossible : {e}"))?;

    let mut listening = false;

    while let Some(frame) = read.next().await {
        if config.read().unwrap().broker.account_id() != account_id {
            return Ok(()); // selection changed — drop the socket
        }
        let text = match frame {
            Ok(Message::Text(t)) => t,
            Ok(Message::Binary(b)) => String::from_utf8_lossy(&b).to_string(),
            Ok(Message::Close(_)) => return Err("flux fermé par Alpaca".into()),
            Ok(_) => continue,
            Err(e) => return Err(format!("flux interrompu : {e}")),
        };
        let Ok(env) = serde_json::from_str::<Envelope>(&text) else {
            continue;
        };

        match env.stream.as_deref() {
            Some("authorization") => {
                let status = env
                    .data
                    .as_ref()
                    .and_then(|d| d.get("status"))
                    .and_then(|s| s.as_str())
                    .unwrap_or("");
                if status != "authorized" {
                    // A refusal here is almost always paper keys on the live host
                    // (or the reverse) — say so instead of retrying forever.
                    return Err(format!(
                        "authentification refusée ({status}) — vérifie que les clés \
                         correspondent bien au compte {} sélectionné",
                        if creds.environment.is_live() {
                            "RÉEL"
                        } else {
                            "paper"
                        },
                    ));
                }
                write
                    .send(Message::Text(
                        json!({ "action": "listen", "data": { "streams": ["trade_updates"] } })
                            .to_string(),
                    ))
                    .await
                    .map_err(|e| format!("abonnement impossible : {e}"))?;
                listening = true;
            }
            Some("trade_updates") => {
                let Some(data) = env.data else { continue };
                let Ok(update) = serde_json::from_value::<TradeUpdate>(data) else {
                    continue;
                };
                let Some(payload) = update.order.as_ref() else {
                    continue;
                };
                let order = build_order(payload, &update.event);
                let nested_bracket = payload.legs.is_some();
                {
                    let mut m = mirror.write().unwrap();
                    m.apply_order(order);
                    m.last_event = Some(chrono::Utc::now());
                }

                // An execution → journal it for TradeTally with the venue's own
                // numbers. Done here, on the authoritative event, so a partial fill
                // is recorded as the partial it was.
                if changes_position(&update.event) {
                    if let (Some(price), Some(qty), Some(position_qty)) = (
                        num(&update.price),
                        num(&update.qty),
                        num(&update.position_qty),
                    ) {
                        let ev = FillEvent {
                            symbol: payload.symbol.clone(),
                            order_id: payload.id.clone(),
                            client_order_id: payload.client_order_id.clone().unwrap_or_default(),
                            side: match payload.side.as_deref() {
                                Some("sell") => Side::Short,
                                _ => Side::Long,
                            },
                            price,
                            qty,
                            position_qty,
                            at: ts(&update.timestamp).unwrap_or_else(chrono::Utc::now),
                            stop_loss: {
                                let payload_levels = levels_from_payload(payload).0;
                                payload_levels.or_else(|| {
                                    deps.protection
                                        .read()
                                        .unwrap()
                                        .plans
                                        .get(&payload.symbol)
                                        .and_then(|p| p.stop_loss)
                                })
                            },
                            take_profit: {
                                let payload_levels = levels_from_payload(payload).1;
                                payload_levels.or_else(|| {
                                    deps.protection
                                        .read()
                                        .unwrap()
                                        .plans
                                        .get(&payload.symbol)
                                        .and_then(|p| p.take_profit)
                                })
                            },
                        };
                        let strategy = strategy_name_for(&deps.chart, &ev.symbol);
                        let snapshot = mirror.read().unwrap().clone();
                        let cfg = config.read().unwrap().clone();
                        let consumed =
                            newly_opened_short_quantity(ev.side, ev.qty, ev.position_qty);
                        if consumed > 0 {
                            let mut plans = deps.locate_plans.write().unwrap();
                            if let Some(plan) = plans.get_mut(&ev.symbol) {
                                if plan.account_id == account_id {
                                    plan.consumed_quantity += consumed;
                                }
                            }
                        }
                        let conn = deps.db.lock().unwrap();
                        deps.journal
                            .write()
                            .unwrap()
                            .on_fill(&ev, &snapshot, &conn, &cfg, &strategy, account_id);
                    } else {
                        // The documented fill shape includes all three. Treat a
                        // malformed frame as incomplete, never as position_qty=0:
                        // zero would fabricate a close in the journal/TradeTally.
                        eprintln!(
                            "[tagdash] broker stream: exécution {} incomplète, journal ignoré et reconcile forcé",
                            payload.id,
                        );
                    }
                }
                // A fill moves the position and its average price, which this event
                // does not carry; a bracket parent filling also SPAWNS its legs. Both
                // need the venue's own view.
                if changes_position(&update.event) || nested_bracket {
                    kick.notify_one();
                }
            }
            Some("listening") => {
                let has_trade_updates = env
                    .data
                    .as_ref()
                    .and_then(|d| d.get("streams"))
                    .and_then(|v| v.as_array())
                    .is_some_and(|streams| {
                        streams.iter().any(|s| s.as_str() == Some("trade_updates"))
                    });
                if !has_trade_updates {
                    return Err("Alpaca n'a pas confirmé l'abonnement trade_updates".into());
                }
                mirror.write().unwrap().stream_connected = true;
                eprintln!("[tagdash] broker stream: à l'écoute des exécutions ({account_id})");
            }
            _ => {}
        }
    }
    if listening {
        Err("flux terminé".into())
    } else {
        Err("flux terminé avant l'authentification".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broker::BrokerOrderStatus;

    #[test]
    fn the_two_environments_stream_from_different_hosts() {
        assert_eq!(
            stream_url(Environment::Paper),
            "wss://paper-api.alpaca.markets/stream"
        );
        assert_eq!(
            stream_url(Environment::Live),
            "wss://api.alpaca.markets/stream"
        );
    }

    /// Alpaca can emit `event:"canceled"` while the nested order still reads
    /// `pending_cancel`. Trusting the nested value would keep a dead order on screen
    /// as cancellable.
    #[test]
    fn a_terminal_event_wins_over_a_lagging_nested_status() {
        assert_eq!(
            to_order("canceled", Some("pending_cancel")),
            BrokerOrderStatus::Closed
        );
        assert_eq!(
            to_order("fill", Some("partially_filled")),
            BrokerOrderStatus::Filled
        );
        assert_eq!(to_order("rejected", Some("new")), BrokerOrderStatus::Closed);
    }

    /// Non-terminal events defer to the venue's own status, so a partial fill stays
    /// working rather than being read as done.
    #[test]
    fn a_partial_fill_stays_working() {
        assert_eq!(
            to_order("partial_fill", Some("partially_filled")),
            BrokerOrderStatus::Open
        );
        assert_eq!(to_order("new", Some("new")), BrokerOrderStatus::Open);
        assert!(to_order("partial_fill", Some("partially_filled")).is_live());
    }

    #[test]
    fn only_share_moving_events_force_a_position_refresh() {
        assert!(changes_position("fill"));
        assert!(changes_position("partial_fill"));
        assert!(!changes_position("new"));
        assert!(!changes_position("canceled"));
        assert!(!changes_position("pending_new"));
    }

    #[test]
    fn locate_consumption_counts_only_new_short_exposure() {
        assert_eq!(newly_opened_short_quantity(Side::Short, 40.0, -40.0), 40);
        assert_eq!(newly_opened_short_quantity(Side::Short, 25.0, -65.0), 25);
        assert_eq!(newly_opened_short_quantity(Side::Short, 100.0, -50.0), 50);
        assert_eq!(newly_opened_short_quantity(Side::Short, 25.0, 25.0), 0);
        assert_eq!(newly_opened_short_quantity(Side::Long, 20.0, -30.0), 0);
    }

    /// A real Alpaca fill frame, parsed end to end into the row the mirror adopts.
    #[test]
    fn a_fill_event_yields_the_venue_price_and_quantity() {
        let raw = r#"{
            "stream":"trade_updates",
            "data":{
                "event":"fill",
                "price":"210.55",
                "qty":"10",
                "position_qty":"10",
                "order":{
                    "id":"o-1","client_order_id":"tagdash-abc","symbol":"AAPL",
                    "side":"buy","type":"limit","time_in_force":"day",
                    "qty":"10","filled_qty":"10","limit_price":"210.60",
                    "filled_avg_price":"210.55","status":"filled",
                    "submitted_at":"2026-08-06T13:30:00Z","filled_at":"2026-08-06T13:30:02Z"
                }
            }
        }"#;
        let env: Envelope = serde_json::from_str(raw).unwrap();
        let update: TradeUpdate = serde_json::from_value(env.data.unwrap()).unwrap();
        let order = build_order(update.order.as_ref().unwrap(), &update.event);

        assert_eq!(order.id, "o-1");
        assert_eq!(order.client_order_id, "tagdash-abc");
        assert_eq!(order.status, BrokerOrderStatus::Filled);
        assert_eq!(order.filled_quantity, 10.0);
        assert_eq!(
            order.filled_avg_price,
            Some(210.55),
            "the executed price, not the limit we asked for",
        );
        assert!(order.filled_at.is_some());

        // …and it leaves the live set, because it is done.
        let mut m = BrokerMirror::default();
        m.apply_order(order);
        assert!(m.orders.is_empty());
    }

    #[test]
    fn a_rejection_carries_a_reason_and_closes_the_order() {
        let raw = r#"{"event":"rejected","order":{"id":"o-2","symbol":"XYZ","status":"rejected","qty":"5"}}"#;
        let update: TradeUpdate = serde_json::from_str(raw).unwrap();
        let order = build_order(update.order.as_ref().unwrap(), &update.event);
        assert_eq!(order.status, BrokerOrderStatus::Closed);
        assert!(order.reject_reason.is_some());
    }

    /// A frame we don't model must not take the session down.
    #[test]
    fn an_unknown_event_is_survivable() {
        let raw = r#"{"event":"calculated","order":{"id":"o-3","symbol":"AAPL","status":"held","qty":"1"}}"#;
        let update: TradeUpdate = serde_json::from_str(raw).unwrap();
        let order = build_order(update.order.as_ref().unwrap(), &update.event);
        assert!(
            order.status.is_live(),
            "unknown ⇒ still live, never silently terminal"
        );
    }
}
