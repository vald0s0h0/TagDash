// Persistence of the internal trading book (positions, resting orders, trades,
// fills) and the per-ticker chart trade context (SL / TP / tradeID lines).
//
// Positions can be held for several days, so the app must be able to close and
// reopen with the book restored *identically*. Rather than spread the state over
// many tables, the whole book and the chart contexts are stored as JSON blobs in
// the `app_config` key/value table and rewritten after every state change. The
// book is small (a handful of open positions + their orders), so a full snapshot
// on each mutation is cheap and avoids partial-write inconsistencies.

use rusqlite::Connection;

use super::cache_repository::{get_app_meta, set_app_meta};
use crate::chart_state::ZoneTradeContext;
use crate::internal_trading::InternalBook;

const BOOK_KEY:  &str = "internal_book_v1";
const CHART_KEY: &str = "chart_contexts_v1";

/// Persist the internal trading book. Pass the result of
/// `InternalBook::persistable_snapshot()` (cancelled-order churn already pruned).
pub fn save_book(conn: &Connection, snapshot: &InternalBook) {
    if let Ok(json) = serde_json::to_string(snapshot) {
        let _ = set_app_meta(conn, BOOK_KEY, &json);
    }
}

/// Reload the internal trading book at startup. Any error (missing key, schema
/// drift, corrupt JSON) yields an empty book so the app always boots.
pub fn load_book(conn: &Connection) -> InternalBook {
    get_app_meta(conn, BOOK_KEY)
        .and_then(|j| serde_json::from_str(&j).ok())
        .unwrap_or_default()
}

/// Persist the per-ticker chart trade contexts (SL/TP/tradeID).
pub fn save_chart_contexts(conn: &Connection, contexts: &[ZoneTradeContext]) {
    if let Ok(json) = serde_json::to_string(contexts) {
        let _ = set_app_meta(conn, CHART_KEY, &json);
    }
}

/// Reload the per-ticker chart trade contexts at startup.
pub fn load_chart_contexts(conn: &Connection) -> Vec<ZoneTradeContext> {
    get_app_meta(conn, CHART_KEY)
        .and_then(|j| serde_json::from_str(&j).ok())
        .unwrap_or_default()
}
