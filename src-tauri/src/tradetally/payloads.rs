// TradeTally API payload structs. Field names follow TradeTally's documented API
// (github.com/GeneBO98/tradetally). Adapt if the server schema differs.

use serde::{Deserialize, Serialize};

// ─── Trade creation (POST /api/trades/shell) ─────────────────────────────────

/// Shell trade: metadata only, no fills yet. Created when tradeID is first
/// generated (SL or TP placed). Fills are added separately.
#[derive(Debug, Serialize)]
pub struct ShellTradePayload {
    pub symbol:      String,
    pub side:        String,   // "long" | "short" — may be updated later
    pub notes:       String,
    pub setup:       String,   // strategy name shown in TradeTally
    pub broker:      String,
    pub account:     String,
    pub stop_loss:   Option<f64>,
    pub take_profit: Option<f64>,
    pub commission:  f64,
    pub fees:        f64,
    pub currency:    String,
    pub strategy_id: String,   // internal reference stored in notes/metadata
}

// ─── Fill (POST /api/trades/:id/fills) ───────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct FillPayload {
    pub date:        String,   // ISO 8601 — execution datetime
    pub price:       f64,
    pub quantity:    f64,
    pub side:        String,   // "buy" | "sell"
    pub commission:  f64,
    pub fees:        f64,
}

// ─── SL / TP levels (PATCH /api/trade-management/trades/:id/levels) ──────────

#[derive(Debug, Serialize)]
pub struct UpdateLevelsPayload {
    pub stop_loss:   Option<f64>,
    pub take_profit: Option<f64>,
}

// ─── Comment / note (POST /api/trades/:id/comments) ──────────────────────────

#[derive(Debug, Serialize)]
pub struct AddCommentPayload {
    pub content:    String,
    pub confidence: Option<i32>,
    pub tags:       Vec<String>,
}

// ─── Update trade metadata (PUT /api/trades/:id) ─────────────────────────────

#[derive(Debug, Serialize)]
pub struct UpdateTradePayload {
    pub notes:      Option<String>,
    pub confidence: Option<i32>,
    pub tags:       Option<Vec<String>>,
    pub mfe:        Option<f64>,
    pub mae:        Option<f64>,
}

// ─── Response shape (flexible — TradeTally may return different envelopes) ────

#[derive(Debug, Deserialize, Default)]
pub struct CreateTradeResponse {
    pub id:    Option<String>,
    pub uuid:  Option<String>,
    pub trade: Option<TradeBody>,
    pub data:  Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct TradeBody {
    pub id:   Option<String>,
    pub uuid: Option<String>,
}

impl CreateTradeResponse {
    /// Extract TradeTally's server UUID from whatever envelope shape is returned.
    pub fn extract_tt_id(&self) -> Option<String> {
        if let Some(ref id) = self.id   { return Some(id.clone()); }
        if let Some(ref u)  = self.uuid { return Some(u.clone()); }
        if let Some(ref t)  = self.trade {
            if let Some(ref id) = t.id   { return Some(id.clone()); }
            if let Some(ref u)  = t.uuid { return Some(u.clone()); }
        }
        if let Some(ref d) = self.data {
            if let Some(id) = d.get("id").and_then(|v| v.as_str()) {
                return Some(id.to_string());
            }
        }
        None
    }
}
