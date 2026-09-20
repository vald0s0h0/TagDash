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

/// Live-feed subscription scope this strategy needs, beyond the charts on screen
/// and any symbol carrying an order/position (always subscribed regardless).
/// Declarative: read by `massive::stream::reconcile` to decide between the budgeted
/// watch-list and the `A.*` wildcard.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StreamingScope {
    /// Nothing extra — today's behaviour.
    None,
    /// + this strategy's current watchlist, within the provider's budget.
    Watchlist,
    /// + the whole market, when the active provider supports a wildcard.
    Universe,
}

/// What the live feed is actually delivering into `MarketState::ingest_trade`.
///
/// Both providers land on the same ingestion path — deliberately, so candle
/// aggregation, VWAP and fill windows are written once — but they do not carry
/// the same information, and a gate that reads the tape must know which it has:
///   • `Prints` (Alpaca) — one message per real trade, so prints/second is a
///     genuine trade rate, and quotes (bid/ask/spread) exist.
///   • `SecondAggregates` (Massive) — one message per symbol per second carrying
///     that second's close and volume. There is NO trade count (every second
///     counts as one print) and NO quote, so neither a trade rate nor a spread can
///     be computed. Volume-per-second and tape continuity are what remain.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TapeKind {
    Prints,
    SecondAggregates,
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
    /// Every detection tempo (in seconds) that fired for this ticker, ascending.
    /// Micro Pullback only; None everywhere else.
    ///
    /// Depth matters: a move that trips the 10s, 20s AND 40s windows has far more
    /// behind it than one that only spikes the 10s. The list keeps growing after the
    /// alert (the engine re-publishes the same `alert_id`), so the badge widens on
    /// screen as the move proves itself.
    #[serde(default)]
    pub tempo_depth: Option<Vec<u32>>,
}

/// One news headline received from the Alpaca news WebSocket (premarket news
/// investor). Stored in RAM (MarketState), keyed by symbol for the micro_pullback
/// correlation engine and kept in a flat log for the debug panel. Crosses the
/// Tauri bridge for the news debug modal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewsHeadline {
    /// Alpaca news id.
    pub id: i64,
    pub headline: String,
    pub summary: Option<String>,
    pub url: Option<String>,
    pub source: Option<String>,
    /// Tickers this headline references (upper-cased).
    pub symbols: Vec<String>,
    pub created_at: DateTime<Utc>,
    /// When the news arrived in our system (used for retention pruning).
    pub received_at: DateTime<Utc>,
}

/// One live screener row (pre-open watchlist). Unlike an AlertSignal, this is a
/// snapshot of a *currently matching* ticker — it is recomputed every scan pass
/// and disappears the instant the ticker no longer meets the strategy criteria.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenerMatch {
    pub symbol: String,
    pub strategy_id: String,
    pub strategy_name: String,
    /// Strategy priority (1..=5), so the UI can badge the zone without knowing
    /// strategies. Derived from the registry by the scanner.
    pub priority: u8,
    pub price: Option<f64>,
    /// Premarket gap vs the previous daily close, in percent.
    pub gap_pct: Option<f64>,
    /// Relative volume (day volume / average daily volume).
    pub rvol: Option<f64>,
    /// Cumulative session volume since 04:00 ET today (same live counter as
    /// `TickerLiveState::volume_day`), live-updating. None when the symbol has no
    /// live ticker state yet (no data); Some(0) is a genuine zero-volume session.
    pub volume: Option<u64>,
    pub float_shares: Option<u64>,
    /// SIC industry + country of origin (sec-api company_meta), for the screener
    /// card's industry/country display. None until sec-api metadata is cached.
    pub industry: Option<String>,
    pub country: Option<String>,
    /// Ranking score (0..100) for score-based screeners (Panic Mean Reversion).
    /// None for plain gap/volume screeners. The list sorts on this when present.
    pub score: Option<f64>,
    /// Short label for the score, e.g. "BB 97" / "PR 95". None when no score.
    pub score_label: Option<String>,
    /// Panic Score 0..100 from the LLM analysis (`crate::panic_llm`). None until the
    /// user runs the analysis, for a ticker that surfaced after the last run, or for
    /// one the analysis could not score (N/A — see `llm_eligibility`). **This is the
    /// primary sort key of the list**, descending, with every None ranked last.
    pub llm_panic_score: Option<f64>,
    /// The one-sentence explanation from the same analysis, for the card's tooltip.
    pub llm_one_liner: Option<String>,
    /// faible | moyenne | forte — reversibility, for the card's badge colour.
    pub llm_reversibility: Option<String>,
    /// Confidence-aware contextual ranking (0..100), distinct from raw reversibility.
    pub llm_ranking_score: Option<f64>,
    pub llm_confidence_score: Option<f64>,
    /// eligible | review | veto | n_a (analysed but not scorable).
    pub llm_eligibility: Option<String>,
    /// extending | exhausting | reversing | unknown (informational only).
    pub llm_timing_state: Option<String>,
    /// long | short, derived from the extension direction.
    pub llm_trade_direction: Option<String>,
    pub llm_veto_reason: Option<String>,
    /// Panic score from the most recent PRIOR day this symbol was analysed
    /// (persisted per (symbol, day) in `panic_llm_scores`, overwritten in place if
    /// re-run for that day). None when it was never scored on an earlier day.
    pub llm_panic_score_prev1: Option<f64>,
    /// Panic score from the day before that.
    pub llm_panic_score_prev2: Option<f64>,
    pub updated_at: DateTime<Utc>,
}

/// One ticker on the Market Attention top list (see `crate::market_attention`).
/// Direction-agnostic snapshot of how much attention the market is paying to a
/// ticker over the rolling 5-minute window — it answers "which tickers are the
/// most watched/traded right now?", NOT whether to go long or short. Produced
/// once a minute (09:30–12:30 ET) and consumed by the Perfect Pullback engine to
/// pick which tickers to watch. Serialised for the `get_market_attention` debug
/// command. Mirrors `src/types/index.ts`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttentionEntry {
    pub symbol: String,
    /// Composite attention score, 0..100 (higher = more attention).
    pub attention_score: f64,
    pub dollar_volume_5m: f64,
    pub volume_5m: u64,
    pub trade_count_5m: u64,
    /// Cross-sectional percentile ranks among the gate-1/2 survivors, 0..1.
    pub pr_dollar_volume_5m: f64,
    pub pr_volume_5m: f64,
    pub pr_trade_count_5m: f64,
    /// Current 5m dollar volume vs the ticker's own historical average at the same
    /// time of day (≤20 trading days). None until the historical baseline is cached.
    pub relative_attention_5m: Option<f64>,
    pub market_share_5m: f64,
    pub smallcap_market_share_5m: f64,
    pub under20_market_share_5m: f64,
    /// Minutes (of the last 5) with significant volume (persistence filter input).
    pub active_minutes_5m: u8,
    /// Largest single-minute share of the 5m volume (spike-concentration filter).
    pub max_1m_volume_share: f64,
    /// Current 5m dollar volume vs the prior 5m (−10..−5 min). None when the prior
    /// window had no dollar volume.
    pub acceleration_5m: Option<f64>,
    pub updated_at: DateTime<Utc>,
}

/// Micro Pullback v2's live Top-10 rows. Unlike `AttentionEntry`, this is
/// multi-tempo and carries the dominance/direction decisions that explain why a
/// displayed leader did or did not reach A+.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MicroAttentionEntry {
    pub symbol: String,
    pub tempo_secs: u32,
    pub rank: u8,
    pub attention_score: f64,
    pub direction_score: f64,
    pub flow_rank: f64,
    pub range_rank: f64,
    pub relative_rank: Option<f64>,
    /// Current tempo volume / robust median of prior same-length windows.
    pub tempo_rvol: Option<f64>,
    pub baseline_windows: u8,
    pub continuity: f64,
    pub distribution_quality: f64,
    pub evidence_ok: bool,
    pub dominance_z: f64,
    pub head_size: u8,
    pub dominant: bool,
    pub already_alerted: bool,
    /// The RVOL third of the score, 0..100. Computed here rather than on the front
    /// end so the number the pane shows every second and the number persisted with a
    /// verdict come from the same function.
    pub rvol_third: f64,
    /// The attention third of the score, 0..100.
    pub attention_third: f64,
    pub market_time: DateTime<Utc>,
    pub received_at: DateTime<Utc>,
}

/// Static description of a compiled strategy (the runtime impl lives in `strategies`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Strategy {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub sessions: Vec<Session>,
    pub priority: u8,
    pub risk: StrategyRiskConfig,
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
    pub order_id: String,
    pub trade_id: Option<String>,
    pub zone_id: String,
    pub symbol: String,
    pub side: Side,
    pub order_type: OrderType,
    pub limit_price: Option<f64>,
    pub stop_price: Option<f64>, // trigger price for OrderType::Stop
    pub quantity: i64,
    pub stop_loss: Option<f64>,
    pub take_profit: Option<f64>,
    pub status: OrderStatus,
    pub oco_group: Option<String>, // set when both SL & TP present
    pub reduce_only: bool,         // true for protective SL/TP bracket exits
    pub created_at: DateTime<Utc>,
    /// Which sizing button launched this order: 25 / 50 / 100 (% of the full
    /// risk-based size). Carried onto the resulting `Fill` so the chart can label
    /// the entry triangle. `None` for anything not sized by a button — protective
    /// SL/TP exits, closes, and orders restored from a database written before the
    /// column existed.
    #[serde(default)]
    pub size_pct: Option<u8>,
}

/// Open internal position.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub trade_id: String,
    pub zone_id: String,
    pub symbol: String,
    pub strategy_id: String,
    pub side: Side,
    pub quantity: i64, // signed: + long, - short
    pub avg_entry_price: f64,
    pub stop_loss: Option<f64>,
    pub take_profit: Option<f64>,
    pub unrealized_pnl: Option<f64>,
    pub r_multiple: Option<f64>,
    pub opened_at: DateTime<Utc>,
    /// Highest / lowest price seen since entry — watermarks for MAE/MFE.
    #[serde(default)]
    pub high_water: f64,
    #[serde(default)]
    pub low_water: f64,
    /// Auto BE / auto TP fire at most once per position (see
    /// `InternalBook::apply_auto_risk_management`) — once set, a level the user
    /// then drags away (deletes the TP, widens the SL past breakeven) is never
    /// silently reasserted by the next tick.
    #[serde(default)]
    pub auto_be_applied: bool,
    #[serde(default)]
    pub auto_tp_applied: bool,
    /// Entry legs filled so far / still resting for this trade — lets the
    /// sidebar show "2/3" while a scaled-in entry is still partially working,
    /// instead of a standalone order row that silently vanishes into the position.
    #[serde(default)]
    pub filled_entries: i64,
    #[serde(default)]
    pub pending_entries: i64,
}

/// Single executed fill (internal simulation only — no broker).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fill {
    pub fill_id: String,
    pub order_id: String,
    pub trade_id: String,
    pub symbol: String,
    pub side: Side,
    pub quantity: i64,
    pub fill_price: f64,
    pub filled_at: DateTime<Utc>,
    /// Sizing button behind this fill (25 / 50 / 100), inherited from its order.
    /// `None` for exits and for fills that no button sized.
    #[serde(default)]
    pub size_pct: Option<u8>,
    /// Market time of the price this fill was executed against — the timestamp of
    /// the freshest aggregate the feed had delivered for the symbol.
    ///
    /// On a 15-minute delayed plan (Massive Starter+) that is ~15 minutes behind
    /// `filled_at`: the click happens now, but the price it fills at belongs to a
    /// bar 15 minutes back. The chart places its triangle here so the marker sits
    /// on the candle that actually produced the fill price, while `filled_at`
    /// stays the true execution instant for the journal and TradeTally.
    /// `None` when the feed had no print for the symbol (fall back to `filled_at`).
    #[serde(default)]
    pub data_at: Option<DateTime<Utc>>,
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
    pub high: f64,
    pub low: f64,
    pub last: f64,
    /// True when the low extreme was reached before the high extreme. Drives the
    /// SL/TP tie-break: a "low-side" exit (long SL / short TP) fills first iff
    /// the low came first. Ties (a single flat print) resolve `true` — the
    /// conservative worst-case assumption that the adverse level filled.
    pub low_first: bool,
}

/// Result of position-sizing calculation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskSizingResult {
    pub entry_price: f64,
    pub stop_loss: f64,
    pub risk_per_share: f64,
    pub full_position_size: i64,
    pub size_25: i64,
    pub size_50: i64,
    pub size_100: i64,
    pub side: Side,
    pub strategy_max_risk_dollars: f64,
}

/// Full trade lifecycle returned by get_trade_lifecycle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeLifecycle {
    pub trade: Trade,
    pub orders: Vec<InternalOrder>,
    pub fills: Vec<Fill>,
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

/// Risk parameters defined per-strategy. All fields are persisted in the DB as
/// JSON and can be overridden at runtime without touching the compiled defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyRiskConfig {
    /// Maximum risk in dollars per trade.
    pub max_risk_dollars: f64,
    /// Default order type at entry. "market" or "limit".
    pub default_order_type: String,
    /// Automatically place a take-profit after entry?
    pub auto_tp_enabled: bool,
    /// At how many R to place the auto TP (e.g. 2.0 = 2R).
    pub auto_tp_r: f64,
    /// Automatically move SL to breakeven after entry?
    pub auto_be_enabled: bool,
    /// At how many R to trigger the auto BE (e.g. 1.0 = 1R).
    pub auto_be_r: f64,
}

impl Default for StrategyRiskConfig {
    fn default() -> Self {
        Self {
            max_risk_dollars: 150.0,
            default_order_type: "market".into(),
            auto_tp_enabled: false,
            auto_tp_r: 2.0,
            auto_be_enabled: false,
            auto_be_r: 1.0,
        }
    }
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
    pub key: String,
    /// Short human label, e.g. "RVOL".
    pub label: String,
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
    pub kind: IndicatorKind,
    pub period: Option<u16>, // for ema/sma; ignored otherwise
}

/// One chart pane inside a zone. A strategy declares 1..=3 of these.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaneSpec {
    /// Timeframe string matching the frontend Timeframe union ("1m", "5m", …).
    pub timeframe: String,
    /// None = the alerted symbol; Some("SPY") = a fixed reference instrument.
    pub symbol: Option<String>,
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
    pub produces: Vec<String>,
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
    pub universe: UniverseKey,
    /// Live-feed subscription scope this strategy needs — see `StreamingScope`.
    pub streaming_scope: StreamingScope,
    /// 1..=3 chart panes shown in the zone when an alert is displayed.
    pub panes: Vec<PaneSpec>,
    /// Strategy-specific info-band fields (name + priority are common, not here).
    pub info_fields: Vec<InfoField>,
    /// LLM call this strategy needs, if any.
    pub llm: Option<LlmSpec>,
    /// Supplementary API enrichments this strategy needs.
    pub enrichments: Vec<EnrichmentSpec>,
}

/// Snapshot of one ticker passed to every strategy's should_alert().
/// Built by the scanner from TickerLiveState; never serialised to the bridge.
#[derive(Debug, Clone)]
pub struct StrategyContext {
    pub symbol: String,
    pub price: Option<f64>,
    pub bid: Option<f64>,
    pub ask: Option<f64>,
    pub spread: Option<f64>,
    pub volume_day: u64,
    pub vwap: Option<f64>,
    pub high_day: Option<f64>,
    pub low_day: Option<f64>,
    pub previous_close: Option<f64>,
    pub change_day_pct: Option<f64>,
    /// Relative volume vs average. None if avg volume not yet loaded.
    pub rvol: Option<f64>,
    /// Float shares from FMP/cache. None when unavailable.
    pub float_shares: Option<u64>,
    /// Market cap in dollars, from the universe table. None when unavailable.
    pub market_cap: Option<i64>,
    /// Institutional ownership, 0..100, from SEC 13D/13G (company_intel). None
    /// when never collected — strategies gating on this should treat `None` as
    /// "unknown", not "zero".
    pub institutional_ownership_pct: Option<f64>,
}

/// A live news headline reduced to what news↔price correlation needs: when it
/// arrived and its text. Always tied to a specific ticker by the per-symbol store
/// it comes from (see MarketState::recent_news). Still produced by MarketState and
/// consumed by the news debug surfaces; the micro_pullback engine reads the live
/// store directly rather than through StrategyContext.
#[derive(Debug, Clone)]
pub struct NewsRef {
    /// When the headline arrived in our system (used for the ±window match).
    pub at: DateTime<Utc>,
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
    pub time: i64,     // unix seconds (UTC midnight of the split day)
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
    pub float_shares: Option<f64>,
    pub country: Option<String>,
    /// true when the issuer country is China / Hong Kong → red badge.
    pub country_flagged: bool,
    pub industry: Option<String>,

    // ── Daily calc ──
    pub classification: Option<Classification>,
    pub split_label: Option<String>, // "x20"
    pub days_since_split: Option<i64>,
    pub daily_bars: Vec<Bar>, // ~250 daily bars for the left pane
    pub split_markers: Vec<SplitMarker>,
    /// true once the daily fetch + classification step has completed.
    pub daily_done: bool,

    // ── Latest news (Alpaca news WebSocket, live RAM) ──
    pub news_title: Option<String>,
    pub news_url: Option<String>,
    /// true once we've looked; if true and news_title is None → "no news" badge.
    pub news_checked: bool,

    // ── Deepseek LLM — micro_pullback (auto, max 2 calls) ──
    pub llm_dilution: Option<String>,
    pub llm_news: Option<String>,

    // ── Deepseek LLM — panic_mean_reversion & backside (manual, button-triggered) ──
    /// Very short context summary: why the stock is moving / which news pushed it.
    pub llm_context: Option<String>,
    /// Mean-reversion verdict: is the move solid or bluff, and the probability of
    /// a return to equilibrium (faible/moyenne/forte).
    pub llm_reversion: Option<String>,

    /// true while at least one Deepseek call is still in flight.
    pub llm_pending: bool,
}
