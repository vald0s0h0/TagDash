// Millisecond timing of every fill, both brokers → `fill_timing`.
//
// TradeTally stores whole seconds at best, so these live in TagDash only. Four
// instants per fill, all epoch ms:
//   sent_ms   — TagDash sent the order (market clock: simulated during a replay);
//   ack_ms    — the broker acknowledged it (Alpaca REST response; = sent for the
//               internal simulated broker, which acknowledges synchronously);
//   filled_ms — the execution instant as the broker reports it;
//   recv_ms   — TagDash received the fill (REAL clock, always).
// sent → ack and filled → recv are the two latencies that matter once TagDash runs
// co-located; the order registry below is what connects an order to its fills.

use std::sync::Mutex;

use rusqlite::{params, Connection};

use crate::types::{Fill, Side};

/// order id → (sent_ms, ack_ms). Pruned past MAX_ORDERS (oldest first).
static ORDERS: Mutex<Vec<(String, Option<i64>, Option<i64>)>> = Mutex::new(Vec::new());
const MAX_ORDERS: usize = 2_000;

fn upsert(order_id: &str, sent: Option<i64>, ack: Option<i64>) {
    if order_id.is_empty() {
        return;
    }
    let mut v = ORDERS.lock().unwrap();
    if let Some(slot) = v.iter_mut().find(|(id, _, _)| id == order_id) {
        slot.1 = slot.1.or(sent);
        slot.2 = slot.2.or(ack);
        return;
    }
    v.push((order_id.to_string(), sent, ack));
    if v.len() > MAX_ORDERS {
        let drop = v.len() - MAX_ORDERS;
        v.drain(..drop);
    }
}

/// TagDash is about to send an order (market clock).
pub fn order_sent(order_id: &str, at_ms: i64) {
    upsert(order_id, Some(at_ms), None);
}

/// The broker acknowledged an order. `sent_ms` is carried along for brokers whose
/// order id only exists once acknowledged (Alpaca assigns it in the response).
pub fn order_acked(order_id: &str, sent_ms: Option<i64>, ack_ms: i64) {
    upsert(order_id, sent_ms, Some(ack_ms));
}

fn lookup(order_id: &str) -> (Option<i64>, Option<i64>) {
    ORDERS
        .lock()
        .unwrap()
        .iter()
        .find(|(id, _, _)| id == order_id)
        .map(|(_, s, a)| (*s, *a))
        .unwrap_or((None, None))
}

/// Persist one fill's timing. Idempotent on `fill_id`.
pub fn record_fill(conn: &Connection, fill: &Fill, broker: &str) {
    let (sent, ack) = lookup(&fill.order_id);
    let signed = match fill.side {
        Side::Long => fill.quantity.abs(),
        Side::Short => -fill.quantity.abs(),
    };
    let _ = conn.execute(
        "INSERT OR IGNORE INTO fill_timing
             (fill_id, trade_id, symbol, broker, quantity, price, sent_ms, ack_ms,
              filled_ms, recv_ms, data_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            fill.fill_id,
            fill.trade_id,
            fill.symbol,
            broker,
            signed,
            fill.fill_price,
            sent,
            ack,
            fill.filled_at.timestamp_millis(),
            chrono::Utc::now().timestamp_millis(),
            fill.data_at.map(|d| d.timestamp_millis()),
        ],
    );
}
