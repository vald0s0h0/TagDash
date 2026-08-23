// Broker-neutral order / position / account shapes.
//
// These mirror what a BROKER says, not what TagDash wishes were true. That
// distinction is the whole design: for the Alpaca broker every field here is copied
// from a server response, never computed locally, so what the UI shows is what the
// venue holds. The internal TagDash broker fills the same shapes from its simulated
// book, so one UI renders both.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Which broker an account belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrokerId {
    /// Internal simulator. Synthetic fills, no network, no real money — ever.
    Tagdash,
    Alpaca,
}

/// Paper money or real money. Orthogonal to the broker in the type system so the
/// UI can render the warning off one field, and orthogonal to the market-data
/// subscription entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Environment {
    Paper,
    Live,
}

impl Environment {
    pub fn is_live(self) -> bool {
        matches!(self, Environment::Live)
    }
}

/// Lifecycle of a broker order, normalised across venues.
///
/// Alpaca has more states than this (`done_for_day`, `pending_new`, `stopped`,
/// `suspended`, …); they are folded into the four that change what the UI may do.
/// `Held` covers everything that is live at the venue but not yet working.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrokerOrderStatus {
    /// Working at the venue — cancellable / replaceable.
    Open,
    /// Accepted but not yet working (queued, pending_new, accepted_for_bidding).
    Held,
    /// Terminal, fully executed.
    Filled,
    /// Terminal, no further execution (canceled, expired, rejected, done_for_day).
    Closed,
}

impl BrokerOrderStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, BrokerOrderStatus::Filled | BrokerOrderStatus::Closed)
    }
    /// Whether the order still occupies buying power / can still execute.
    pub fn is_live(self) -> bool {
        !self.is_terminal()
    }
}

/// One order as the broker holds it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrokerOrder {
    /// The BROKER's id — the only handle valid for cancel / replace.
    pub id: String,
    /// Our idempotency key, echoed back by the venue. Lets a resubmit after a lost
    /// response be recognised instead of duplicated.
    pub client_order_id: String,
    pub symbol: String,
    /// "buy" | "sell" as the venue words it.
    pub side: String,
    /// "market" | "limit" | "stop" | "stop_limit" | "trailing_stop".
    pub order_type: String,
    /// "day" | "gtc" | "opg" | "cls" | "ioc" | "fok".
    pub time_in_force: String,
    pub quantity: f64,
    pub filled_quantity: f64,
    pub limit_price: Option<f64>,
    pub stop_price: Option<f64>,
    /// Volume-weighted average of this order's executions. Authoritative entry
    /// price — never the price we asked for.
    pub filled_avg_price: Option<f64>,
    pub status: BrokerOrderStatus,
    /// The venue's own status string, kept verbatim for diagnostics.
    pub raw_status: String,
    /// Bracket / OCO / OTO parent, when the venue nests legs.
    pub parent_id: Option<String>,
    pub submitted_at: Option<DateTime<Utc>>,
    pub filled_at: Option<DateTime<Utc>>,
    pub canceled_at: Option<DateTime<Utc>>,
    /// Why the venue refused it, when it did.
    pub reject_reason: Option<String>,
}

impl BrokerOrder {
    /// Shares still working at the venue.
    pub fn remaining(&self) -> f64 {
        (self.quantity - self.filled_quantity).max(0.0)
    }
}

/// One position as the broker holds it. Quantity is signed: negative is short.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrokerPosition {
    pub symbol: String,
    pub quantity: f64,
    pub avg_entry_price: f64,
    /// Venue-computed, when it supplies them. Left `None` rather than recomputed
    /// locally — a P&L that disagrees with the broker's is worse than none.
    pub market_price: Option<f64>,
    pub market_value: Option<f64>,
    pub cost_basis: Option<f64>,
    pub unrealized_pnl: Option<f64>,
    pub unrealized_pnl_pct: Option<f64>,
}

impl BrokerPosition {
    pub fn is_short(&self) -> bool {
        self.quantity < 0.0
    }
}

/// Account snapshot: what may be traded, and whether the venue has restricted it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrokerAccount {
    pub id: String,
    pub broker: BrokerId,
    pub environment: Environment,
    /// Venue status ("ACTIVE", "ACCOUNT_UPDATED", …).
    pub status: String,
    pub currency: String,
    pub cash: Option<f64>,
    pub equity: Option<f64>,
    pub buying_power: Option<f64>,
    /// True when the venue has blocked new orders — surfaced rather than swallowed,
    /// because the alternative is orders vanishing with no explanation.
    pub trading_blocked: bool,
    pub account_blocked: bool,
    /// The account holder's own "suspend trading" switch, set in Alpaca's dashboard.
    /// Distinct from `trading_blocked`, which is the venue blocking you. Alpaca
    /// rejects every new order with code 40310000 while it is on, so it is carried
    /// here rather than left to surface as an unexplained rejection.
    pub trade_suspended_by_user: bool,
    /// Pattern-day-trader flag and remaining day trades, when the venue reports them.
    pub pattern_day_trader: bool,
    pub daytrade_count: Option<i64>,
}

/// What TagDash asks a broker to do. Deliberately smaller than Alpaca's full order
/// schema: the fields the app actually uses, so an unsupported combination cannot
/// be expressed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderRequest {
    pub symbol: String,
    /// Always positive; direction lives in `side`.
    pub quantity: i64,
    pub side: crate::types::Side,
    pub order_type: crate::types::OrderType,
    pub limit_price: Option<f64>,
    pub stop_price: Option<f64>,
    /// Attached protective legs. Present ⇒ the order is submitted as a bracket, so
    /// the venue owns the exits even if TagDash is closed.
    pub stop_loss: Option<f64>,
    pub take_profit: Option<f64>,
    /// "day" | "gtc" — defaults to day.
    pub time_in_force: Option<String>,
    /// Eligible for Alpaca's 04:00–20:00 ET sessions. Alpaca only accepts plain
    /// limit day/GTC orders in this mode; advanced bracket/OTO/OCO orders are RTH.
    #[serde(default)]
    pub extended_hours: bool,
    /// Caller-supplied idempotency key. Generated when absent.
    pub client_order_id: Option<String>,
    /// Reduce-only exits must never flip a position; the venue is told explicitly.
    #[serde(default)]
    pub reduce_only: bool,
}

/// Whether a symbol may be sold short, as the venue reports it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Shortability {
    pub symbol: String,
    /// Broker whose rules produced this answer. Borrow classifications are not
    /// portable between venues, so the UI always displays this alongside ETB/HTB.
    pub broker: BrokerId,
    /// False ⇒ the venue will reject a short. This is Alpaca's `shortable` flag.
    pub shortable: bool,
    /// Venue-native borrow classification, normalised only for display/routing.
    pub borrow_status: BorrowStatus,
    /// True when the answer comes from a cached asset row rather than a live call.
    pub from_cache: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BorrowStatus {
    EasyToBorrow,
    HardToBorrow,
    Unavailable,
    Unknown,
}

/// One Alpaca locate quote. At present Alpaca returns one account-specific,
/// aggregated quote per symbol (not a lender-by-lender marketplace).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocateQuote {
    pub broker: BrokerId,
    pub symbol: String,
    pub available_qty: i64,
    pub price_per_share: f64,
    pub quoted_at: Option<DateTime<Utc>>,
    /// Reserved for broker responses that expose the annualised HTB rate. Alpaca's
    /// public examples currently omit it; `None` must never be treated as zero.
    pub annual_borrow_rate: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocateRecord {
    pub id: String,
    pub broker: BrokerId,
    pub symbol: String,
    pub requested_qty: i64,
    pub located_qty: i64,
    /// Remaining quantity when supplied by the venue. A just-created locate falls
    /// back to `located_qty`; historical rows without this field do not.
    pub available_qty: Option<i64>,
    pub price_per_share: f64,
    pub total_fee: f64,
    pub status: String,
    pub created_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
}

/// Cost basis captured when the user explicitly reserves locate inventory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShortCostBasis {
    pub account_id: String,
    pub symbol: String,
    pub planned_quantity: i64,
    pub located_quantity: i64,
    /// Highest quantity observed as consumed by a short. Locates are single-use;
    /// this never decreases when the position is partially or fully covered.
    pub consumed_quantity: i64,
    pub locate_fee: f64,
    pub annual_borrow_rate: Option<f64>,
    pub expires_at: Option<DateTime<Utc>>,
    pub locates: Vec<LocateRecord>,
}

/// Horizontal loss band shown on a short chart. For a short, prices between
/// `break_even_price` and `entry_price` are still loss-making after known costs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShortCostZone {
    pub broker: BrokerId,
    pub symbol: String,
    /// `estimate` before the first fill, `position` once Alpaca reports a position.
    pub source: String,
    pub entry_price: f64,
    pub stop_loss: f64,
    pub quantity: i64,
    pub located_quantity: i64,
    pub locate_fee: f64,
    pub annual_borrow_rate: Option<f64>,
    pub borrow_days: u32,
    pub borrow_fee: Option<f64>,
    pub total_known_cost: f64,
    pub break_even_price: f64,
    pub costs_complete: bool,
    pub locates: Vec<LocateRecord>,
}
