// Shared domain types. Kept in sync with src/types/index.ts on the frontend.
// Every type that crosses the Tauri bridge must be Serialize + Deserialize and
// have a matching TypeScript declaration.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Session {
    Premarket,
    PreOpen,
    Open,
    Afterhours,
}

/// Which universe the single Alpaca WebSocket connection streams.
/// Premarket → LowFloat (lighter); pre-open/open → UsStocks (full tradable set).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UniverseKey {
    UsStocks,
    LowFloat,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Long,
    Short,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OrderType {
    Limit,
    Market,
    Stop, // protective SL: triggers a market exit when price crosses stop_price
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OrderStatus {
    Pending,
    Filled,
    Cancelled,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LatencyLevel {
    Normal,
    Warning,
    Slow,
    Critical,
}

/// Alert produced by a strategy when conditions match.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertSignal {
    pub alert_id: String,
    pub timestamp: DateTime<Utc>,
    pub symbol: String,
    pub strategy_id: String,
    pub strategy_name: String,
    pub priority: u8, // 1..=5
    pub session: Session,
    pub price: Option<f64>,
    pub bid: Option<f64>,
    pub ask: Option<f64>,
    pub spread: Option<f64>,
    pub volume: Option<u64>,
    pub rvol: Option<f64>,
    pub change_day_pct: Option<f64>,
    pub float_shares: Option<u64>,
    pub news_today: bool,
    pub halted: Option<bool>,
    pub latency_ui_ms: Option<u32>,
    pub reason: String,
    /// Chart timeframe the UI should display for this alert ("1m", "5m"…). When
    /// set, it overrides the strategy card's static pane timeframe (lets one
    /// strategy fire on several timeframes — e.g. Perfect Pullback 1m vs 5m).
    #[serde(default)]
    pub display_timeframe: Option<String>,
    /// Directional bias of the alert (long/short). Drives a side badge in the UI.
    #[serde(default)]
    pub side: Option<Side>,
}

/// One news headline received from the Alpaca news WebSocket (premarket news
/// investor). Stored in RAM (MarketState), keyed by symbol for the micro_pullback
/// correlation engine and kept in a flat log for the debug panel. Crosses the
/// Tauri bridge for the news debug modal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewsHeadline {
    /// Alpaca news id.
    pub id:         i64,
    pub headline:   String,
    pub summary:    Option<String>,
    pub url:        Option<String>,
    pub source:     Option<String>,
    /// Tickers this headline references (upper-cased).
    pub symbols:    Vec<String>,
    pub created_at: DateTime<Utc>,
    /// When the news arrived in our system (used for retention pruning).
    pub received_at: DateTime<Utc>,
}

/// One live screener row (pre-open watchlist). Unlike an AlertSignal, this is a
/// snapshot of a *currently matching* ticker — it is recomputed every scan pass
/// and disappears the instant the ticker no longer meets the strategy criteria.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenerMatch {
    pub symbol:        String,
    pub strategy_id:   String,
    pub strategy_name: String,
    /// Strategy priority (1..=5), so the UI can badge the zone without knowing
    /// strategies. Derived from the registry by the scanner.
    pub priority:      u8,
    pub price:         Option<f64>,
    /// Premarket gap vs the previous daily close, in percent.
    pub gap_pct:       Option<f64>,
    /// Relative volume (day volume / average daily volume).
    pub rvol:          Option<f64>,
    pub volume:        u64,
    pub float_shares:  Option<u64>,
    /// Ranking score (0..100) for score-based screeners (Panic Mean Reversion).
    /// None for plain gap/volume screeners. The list sorts on this when present.
    pub score:         Option<f64>,
    /// Short label for the score, e.g. "BB 97" / "PR 95". None when no score.
    pub score_label:   Option<String>,
    pub updated_at:    DateTime<Utc>,
}

/// Static description of a compiled strategy (the runtime impl lives in `strategies`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Strategy {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub sessions: Vec<Session>,
    pub priority: u8,
    pub max_risk_dollars: f64,
}

/// Internal trade aggregate (one tradeID = one open lifecycle).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trade {
    pub trade_id: String, // YYMMJJHHMMSS-TICKER-STRATEGY
    pub symbol: String,
    pub strategy_id: String,
    pub side: Option<Side>,
    pub stop_loss: Option<f64>,
    pub take_profit: Option<f64>,
    pub opened_at: Option<DateTime<Utc>>,
    pub closed_at: Option<DateTime<Utc>>,
    pub entry_price: Option<f64>,
    pub quantity: Option<i64>, // signed: + long, - short
    pub notes: Option<String>,
    pub confidence: Option<u8>,
    pub tags: Vec<String>,
    /// Max adverse / favorable excursion in dollars (positive magnitudes),
    /// captured when the position closed. None until the trade is flat.
    #[serde(default)]
    pub mae: Option<f64>,
    #[serde(default)]
    pub mfe: Option<f64>,
}

/// Internal limit/market order (never sent to a real broker in V1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InternalOrder {
    pub order_id:    String,
    pub trade_id:    Option<String>,
    pub zone_id:     String,
    pub symbol:      String,
    pub side:        Side,
    pub order_type:  OrderType,
    pub limit_price: Option<f64>,
    pub stop_price:  Option<f64>, // trigger price for OrderType::Stop
    pub quantity:    i64,
    pub stop_loss:   Option<f64>,
    pub take_profit: Option<f64>,
    pub status:      OrderStatus,
    pub oco_group:   Option<String>, // set when both SL & TP present
    pub reduce_only: bool,           // true for protective SL/TP bracket exits
    pub created_at:  DateTime<Utc>,
}

/// Open internal position.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub trade_id:        String,
    pub zone_id:         String,
    pub symbol:          String,
    pub strategy_id:     String,
    pub side:            Side,
    pub quantity:        i64, // signed: + long, - short
    pub avg_entry_price: f64,
    pub stop_loss:       Option<f64>,
    pub take_profit:     Option<f64>,
    pub unrealized_pnl:  Option<f64>,
    pub r_multiple:      Option<f64>,
    pub opened_at:       DateTime<Utc>,
    /// Highest / lowest price seen since entry — watermarks for MAE/MFE.
    #[serde(default)]
    pub high_water:      f64,
    #[serde(default)]
    pub low_water:       f64,
}

/// Single executed fill (internal simulation only — no broker).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fill {
    pub fill_id:    String,
    pub order_id:   String,
    pub trade_id:   String,
    pub symbol:     String,
    pub side:       Side,
    pub quantity:   i64,
    pub fill_price: f64,
    pub filled_at:  DateTime<Utc>,
}

/// The price path a symbol traced between two fill polls — a micro-bar the
/// internal book consumes to detect order crossings. Unlike a single (bid, ask)
/// snapshot it preserves the *range* (so a level the price spiked through and
/// retraced is still caught) and the *order* in which the extremes were reached
/// (so SL vs TP inside the same window resolve by which one the price actually
/// touched first — not by an arbitrary HashMap iteration order, nor a naive
/// "red bar ⇒ SL" rule). `first` is the window's opening print, used to fill at
/// the gap price when a day/session boundary opens straight through a level.
#[derive(Debug, Clone, Copy)]
pub struct FillWindow {
    pub first: f64,
    pub high:  f64,
    pub low:   f64,
    pub last:  f64,
    /// True when the low extreme was reached before the high extreme. Drives the
    /// SL/TP tie-break: a "low-side" exit (long SL / short TP) fills first iff
    /// the low came first. Ties (a single flat print) resolve `true` — the
    /// conservative worst-case assumption that the adverse level filled.
    pub low_first: bool,
}

/// Result of position-sizing calculation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskSizingResult {
    pub entry_price:               f64,
    pub stop_loss:                 f64,
    pub risk_per_share:            f64,
    pub full_position_size:        i64,
    pub size_25:                   i64,
    pub size_50:                   i64,
    pub size_100:                  i64,
    pub side:                      Side,
    pub strategy_max_risk_dollars: f64,
}

/// Full trade lifecycle returned by get_trade_lifecycle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeLifecycle {
    pub trade:    Trade,
    pub orders:   Vec<InternalOrder>,
    pub fills:    Vec<Fill>,
    pub position: Option<Position>,
}

/// One outbound event in the TradeTally queue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeTallyEvent {
    pub event_id: String,
    pub timestamp: DateTime<Utc>,
    pub trade_id: String,
    pub symbol: String,
    pub event_type: String, // open_trade, modify_sl, modify_tp, note, screenshot, close
    pub endpoint: String,
    pub payload_summary: String,
    pub status: OrderStatus, // reused: pending/filled(=success)/cancelled(=failed)
    pub error_message: Option<String>,
    pub attempts: u32,
}

/// Latency status displayed in the UI (Alpaca timestamp → UI rendering).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatencyStatus {
    pub websocket_to_ui_ms: u32,
    pub level: LatencyLevel,
    pub measured_at: DateTime<Utc>,
}

/// Risk parameters defined per-strategy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyRiskConfig {
    pub max_risk_dollars: f64,
}

// ─── Strategy identity card ───────────────────────────────────────────────────
// The "carte d'identité" returned by ScanStrategy::card(): everything the
// scanner + UI need about a strategy beyond its alert-matching logic. Lives in
// each strategy file next to its rules. Replaces the old StrategyDisplayConfig.

/// Where an info-band field's value comes from.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InfoSource {
    /// Present in the AlertSignal / live snapshot at fire time (shown immediately).
    Alert,
    /// Produced by the LLM call — UI shows a loading state until it lands.
    Llm,
    /// Produced by a supplementary API call (Massive, sec-api…).
    Enrichment,
}

/// One strategy-specific field shown in the thin info band above the charts.
/// The common fields (strategy name + priority badge) are added by the UI and
/// are NOT listed here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InfoField {
    /// Stable key resolved to a value on the frontend (e.g. "rvol", "llm_bias").
    pub key:    String,
    /// Short human label, e.g. "RVOL".
    pub label:  String,
    pub source: InfoSource,
}

/// Kind of indicator overlaid on a pane. Maps to a lightweight-charts series.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IndicatorKind {
    Vwap,           // session VWAP (uses Bar.vwap)
    Ema,            // exponential moving average (needs period)
    Sma,            // simple moving average (needs period)
    Volume,         // volume histogram pinned to the bottom
    PreviousClose,  // horizontal line at the previous trading day's close
    PreviousHigh,   // horizontal line at the previous trading day's high
    PreviousLow,    // horizontal line at the previous trading day's low
    BollingerBands, // upper/basis/lower bands from the pane's closes (period, k=2)
}

/// One indicator request for a pane.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaneIndicator {
    pub kind:   IndicatorKind,
    pub period: Option<u16>, // for ema/sma; ignored otherwise
}

/// One chart pane inside a zone. A strategy declares 1..=3 of these.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaneSpec {
    /// Timeframe string matching the frontend Timeframe union ("1m", "5m", …).
    pub timeframe:  String,
    /// None = the alerted symbol; Some("SPY") = a fixed reference instrument.
    pub symbol:     Option<String>,
    pub indicators: Vec<PaneIndicator>,
    /// The pane that carries SL/TP/order/drawing interactions. Lets the tradeable
    /// pane be other than the left-most one (e.g. daily left, 5s right & live).
    /// If no pane is flagged, the UI falls back to the first pane.
    #[serde(default)]
    pub interactive: bool,
    /// Optional layout column. Panes sharing a column are STACKED vertically (in
    /// declaration order); columns are laid left-to-right. `None` = the pane gets
    /// its own column (legacy side-by-side behaviour). E.g. Micro Pullback puts
    /// daily + 5m in column 0 (left, stacked) and the 10s pane in column 1 (right).
    #[serde(default)]
    pub column: Option<u8>,
}

/// LLM enrichment requested by a strategy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmSpec {
    /// Static prompt template; {placeholders} are filled at call time.
    pub prompt_template: String,
    /// info-field keys this call fills (matches InfoField.key, source = Llm).
    pub produces:        Vec<String>,
}

/// Supplementary data provider for API enrichment.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EnrichmentProvider {
    Massive,
    SecApi,
}

/// Supplementary API enrichment requested by a strategy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrichmentSpec {
    pub provider: EnrichmentProvider,
    /// info-field keys this call fills (matches InfoField.key, source = Enrichment).
    pub produces: Vec<String>,
}

/// The full identity card of a strategy. Returned by ScanStrategy::card().
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyCard {
    /// Streaming universe this strategy watches (premarket low-float vs full
    /// US stocks). Declarative: drives which symbols must be subscribed.
    pub universe:    UniverseKey,
    /// 1..=3 chart panes shown in the zone when an alert is displayed.
    pub panes:       Vec<PaneSpec>,
    /// Strategy-specific info-band fields (name + priority are common, not here).
    pub info_fields: Vec<InfoField>,
    /// LLM call this strategy needs, if any.
    pub llm:         Option<LlmSpec>,
    /// Supplementary API enrichments this strategy needs.
    pub enrichments: Vec<EnrichmentSpec>,
}

/// Snapshot of one ticker passed to every strategy's should_alert().
/// Built by the scanner from TickerLiveState; never serialised to the bridge.
#[derive(Debug, Clone)]
pub struct StrategyContext {
    pub symbol:         String,
    pub price:          Option<f64>,
    pub bid:            Option<f64>,
    pub ask:            Option<f64>,
    pub spread:         Option<f64>,
    pub volume_day:     u64,
    pub vwap:           Option<f64>,
    pub high_day:       Option<f64>,
    pub low_day:        Option<f64>,
    pub previous_close: Option<f64>,
    pub change_day_pct: Option<f64>,
    /// Relative volume vs average. None if avg volume not yet loaded.
    pub rvol:           Option<f64>,
    /// Float shares from FMP/cache. None in mock mode.
    pub float_shares:   Option<u64>,
}

/// A live news headline reduced to what news↔price correlation needs: when it
/// arrived and its text. Always tied to a specific ticker by the per-symbol store
/// it comes from (see MarketState::recent_news). Still produced by MarketState and
/// consumed by the news debug surfaces; the micro_pullback engine reads the live
/// store directly rather than through StrategyContext.
#[derive(Debug, Clone)]
pub struct NewsRef {
    /// When the headline arrived in our system (used for the ±window match).
    pub at:       DateTime<Utc>,
    pub headline: String,
}

// ─── Alert enrichment (progressive, pushed to the info band) ──────────────────
// Filled asynchronously when an alert is displayed in a zone. Each step writes a
// partial result so the UI band fills in progressively. See `enrichment` module.

use crate::market_state::aggregators::Bar;

/// Daily price-action classification of a ticker (quality over forcing — None is
/// a valid outcome).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Classification {
    PumpDump,
    MomoFormer,
}

/// A marker drawn on the daily pane (red dot on a split day).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SplitMarker {
    pub time:  i64,    // unix seconds (UTC midnight of the split day)
    pub label: String, // e.g. "x20"
}

/// Progressive enrichment for one displayed alert, keyed by symbol.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AlertEnrichment {
    pub symbol: String,
    /// Strategy that requested the enrichment — lets the UI pick the right LLM
    /// chips/button (micro_pullback dilution/news vs panic context/verdict).
    pub strategy_id: String,
    /// "loading" while any step is still in flight, else "done".
    pub status: String,

    // ── Immediate (from the local DB) ──
    pub float_shares:    Option<f64>,
    pub country:         Option<String>,
    /// true when the issuer country is China / Hong Kong → red badge.
    pub country_flagged: bool,
    pub industry:        Option<String>,

    // ── Daily calc ──
    pub classification:  Option<Classification>,
    pub split_label:     Option<String>, // "x20"
    pub days_since_split: Option<i64>,
    pub daily_bars:      Vec<Bar>,        // ~250 daily bars for the left pane
    pub split_markers:   Vec<SplitMarker>,
    /// true once the daily fetch + classification step has completed.
    pub daily_done:      bool,

    // ── Latest news (Alpaca news WebSocket, live RAM) ──
    pub news_title:   Option<String>,
    pub news_url:     Option<String>,
    /// true once we've looked; if true and news_title is None → "no news" badge.
    pub news_checked: bool,

    // ── Deepseek LLM — micro_pullback (auto, max 2 calls) ──
    pub llm_dilution: Option<String>,
    pub llm_news:     Option<String>,

    // ── Deepseek LLM — panic_mean_reversion (manual, button-triggered) ──
    /// Very short context summary: why the stock is moving / which news pushed it.
    pub llm_context:   Option<String>,
    /// Mean-reversion verdict: is the move solid or bluff, and the probability of
    /// a return to equilibrium (faible/moyenne/forte).
    pub llm_reversion: Option<String>,

    /// true while at least one Deepseek call is still in flight.
    pub llm_pending:  bool,
}
