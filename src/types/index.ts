// Shared domain types — mirrors src-tauri/src/types/mod.rs.
// Keep field names and serde renames in sync (snake_case enums).

export type Session = "premarket" | "pre_open" | "open" | "afterhours";
export type Side = "long" | "short";
export type OrderType = "limit" | "market" | "stop";
export type OrderStatus = "pending" | "filled" | "cancelled";
export type LatencyLevel = "normal" | "warning" | "slow" | "critical";

export interface AlertSignal {
  alert_id: string;
  timestamp: string;
  symbol: string;
  strategy_id: string;
  strategy_name: string;
  priority: 1 | 2 | 3 | 4 | 5;
  session: Session;
  price: number | null;
  bid: number | null;
  ask: number | null;
  spread: number | null;
  volume: number | null;
  rvol: number | null;
  change_day_pct: number | null;
  float_shares: number | null;
  news_today: boolean;
  halted: boolean | null;
  latency_ui_ms: number | null;
  reason: string;
  /** Chart timeframe the UI should display ("1m"/"5m"). Overrides the strategy
   *  card's static pane timeframe (e.g. Opening Interest 1m vs 5m). */
  display_timeframe: string | null;
  /** Directional bias of the alert; drives a side badge. */
  side: Side | null;
}

export interface Strategy {
  id: string;
  name: string;
  enabled: boolean;
  sessions: Session[];
  priority: number;
  max_risk_dollars: number;
}

// ─── Live pre-open screener match (mirrors ScreenerMatch in types/mod.rs) ──────

export interface ScreenerMatch {
  symbol:        string;
  strategy_id:   string;
  strategy_name: string;
  /** Strategy priority (1..5), set by the backend from the registry. */
  priority:      number;
  price:         number | null;
  /** Premarket gap vs previous daily close, in percent. */
  gap_pct:       number | null;
  /** Relative volume (day volume / average daily volume). */
  rvol:          number | null;
  volume:        number;
  float_shares:  number | null;
  /** Ranking score (0..100) for score-based screeners (Panic Mean Reversion);
   *  null for plain gap/volume screeners. The list sorts on this when present. */
  score:         number | null;
  /** Short label for the score, e.g. "BB 97" / "PR 95"; null when no score. */
  score_label:   string | null;
  updated_at:    string;
}

/** Per-symbol info-band extras not in the live snapshot (mirrors CardInfo in
 *  commands/mod.rs): market cap, float, and the mean-reversion score. All
 *  optional ("si dispo"). */
export interface CardInfo {
  market_cap:    number | null;
  float_shares:  number | null;
  /** Watchlist metric value (BB area sum, or |move|/ATR20). */
  mr_score:      number | null;
  /** Which list retained the ticker: "BB" or "MA". */
  mr_score_kind: string | null;
  /** Extension direction: +1 up, −1 down, 0 none. */
  mr_direction:  number | null;
  /** SIC industry + country of origin (sec-api), for the manual search info band. */
  industry:      string | null;
  country:       string | null;
  // ── Common chart info-bar fields (same for every strategy) ──────────────────
  /** Bollinger Z of the live price vs its 20-day daily basis: (price−SMA20)/σ20. */
  bbz:               number | null;
  /** Today's premarket cumulative volume (04:00–09:30 ET). */
  premarket_volume:  number | null;
  /** A live news headline is on file for the symbol. */
  has_news:          boolean;
  /** Most recent live headline text, if any. */
  news_title:        string | null;
}

// ─── Strategy display config ──────────────────────────────────────────────────

export interface StrategyRiskConfig {
  max_risk_dollars: number;
}

// ─── Strategy identity card (mirrors StrategyCard in types/mod.rs) ────────────

/** Where an info-band field's value comes from. */
export type InfoSource = "alert" | "llm" | "enrichment";

export interface InfoField {
  key: string;
  label: string;
  source: InfoSource;
}

export type IndicatorKind =
  | "vwap"
  | "ema"
  | "sma"
  | "volume"
  | "previous_close"
  | "previous_high"
  | "previous_low"
  | "bollinger_bands";

export interface PaneIndicator {
  kind: IndicatorKind;
  period: number | null;
}

export interface PaneSpec {
  timeframe: Timeframe;
  /** null = the alerted symbol; a string = a fixed reference instrument. */
  symbol: string | null;
  indicators: PaneIndicator[];
  /** The pane that carries SL/TP/order/drawing interactions (e.g. the 5s pane
   *  even when it sits to the right of a daily context pane). */
  interactive: boolean;
}

export interface LlmSpec {
  prompt_template: string;
  produces: string[];
}

export type EnrichmentProvider = "massive" | "sec_api";

export interface EnrichmentSpec {
  provider: EnrichmentProvider;
  produces: string[];
}

export interface StrategyCard {
  universe: UniverseKey;
  panes: PaneSpec[];
  info_fields: InfoField[];
  llm: LlmSpec | null;
  enrichments: EnrichmentSpec[];
}

export interface Trade {
  trade_id: string;
  symbol: string;
  strategy_id: string;
  side: Side | null;
  stop_loss: number | null;
  take_profit: number | null;
  opened_at: string | null;
  closed_at: string | null;
  entry_price: number | null;
  quantity: number | null;
  notes: string | null;
  confidence: number | null;
  tags: string[];
}

export interface InternalOrder {
  order_id:    string;
  trade_id:    string | null;
  zone_id:     string;
  symbol:      string;
  side:        Side;
  order_type:  OrderType;
  limit_price: number | null;
  stop_price:  number | null;
  quantity:    number;
  stop_loss:   number | null;
  take_profit: number | null;
  status:      OrderStatus;
  oco_group:   string | null;
  reduce_only: boolean;
  created_at:  string;
}

export interface Position {
  trade_id:        string;
  zone_id:         string;
  symbol:          string;
  strategy_id:     string;
  side:            Side;
  quantity:        number;
  avg_entry_price: number;
  stop_loss:       number | null;
  take_profit:     number | null;
  unrealized_pnl:  number | null;
  r_multiple:      number | null;
  opened_at:       string;
}

export interface Fill {
  fill_id:    string;
  order_id:   string;
  trade_id:   string;
  symbol:     string;
  side:       Side;
  quantity:   number;
  fill_price: number;
  filled_at:  string;
}

export interface RiskSizingResult {
  entry_price:               number;
  stop_loss:                 number;
  risk_per_share:            number;
  full_position_size:        number;
  size_25:                   number;
  size_50:                   number;
  size_100:                  number;
  side:                      Side;
  strategy_max_risk_dollars: number;
}

export interface TradeLifecycle {
  trade:    Trade;
  orders:   InternalOrder[];
  fills:    Fill[];
  position: Position | null;
}

export interface TradeTallyEvent {
  event_id: string;
  timestamp: string;
  trade_id: string;
  symbol: string;
  event_type: string;
  endpoint: string;
  payload_summary: string;
  status: OrderStatus;
  error_message: string | null;
  attempts: number;
}

export interface LatencyStatus {
  websocket_to_ui_ms: number;
  level: LatencyLevel;
  measured_at: string;
}

// ─── Config ──────────────────────────────────────────────────────────────────

/** When a desktop attention cue (flash / foreground) fires, by trading session. */
export type AttentionMode = "off" | "premarket" | "open" | "both";

export interface AppConfig {
  trading: {
    default_broker: string;
    default_account: string;
    default_commission: number;
    default_fees: number;
    min_position_size: number;
    max_position_size: number;
  };
  alpaca: { feed: string; use_news: boolean };
  universe: {
    /** Float ceiling (shares) for the Low Float streaming universe. */
    low_float_max: number;
  };
  ui: {
    default_theme: string;
    premarket_zones_per_tab: number;
    pre_open_zones_per_tab: number;
    open_zones_per_tab: number;
    auto_create_tabs: boolean;
    /** Send a native OS notification (Windows toast / macOS Notification Center)
     *  whenever a scanner alert fires, regardless of the active tab. */
    desktop_alerts: boolean;
    /** When to flash the full-screen white overlay on a new scanner alert. */
    flash_alerts: AttentionMode;
    /** When to force the TagDash window back to the foreground on a new alert. */
    foreground_alerts: AttentionMode;
  };
  latency: { warn_ms: number; critical_ms: number };
  tradetally: { api_base_url: string };
  /** User-defined journal tags (replaces TradeTally-fetched tags). */
  journal: { tags: string[] };
}

export interface AppStatus {
  version: string;
  backend: string;
  latency: LatencyStatus;
}

// ─── Secrets (status only — never values) ────────────────────────────────────

export interface SecretsStatus {
  alpaca_key: boolean;
  alpaca_secret: boolean;
  fmp_api_key: boolean;
  massive_api_key: boolean;
  sec_api_key: boolean;
  claude_api_key: boolean;
  deepseek_api_key: boolean;
  tradetally_token: boolean;
}

// ─── Sync queue ──────────────────────────────────────────────────────────────

export interface SyncQueueRow {
  event_id: string;
  timestamp: string;
  trade_id: string;
  symbol: string;
  event_type: string;
  endpoint: string;
  payload_summary: string;
  status: "pending" | "success" | "failed";
  error_message: string | null;
  attempts: number;
  created_at: string;
}

export interface SyncQueueStatus {
  pending: number;
  success: number;
  failed: number;
  recent: SyncQueueRow[];
}

// ─── Journal ─────────────────────────────────────────────────────────────────

export interface JournalEntry {
  trade_id:   string;
  symbol:     string;
  notes:      string;
  confidence: number | null;
  tags:       string[];
  updated_at: string;
}

// ─── Logs ────────────────────────────────────────────────────────────────────

export interface LocalLogEntry {
  id: number;
  level: "info" | "warn" | "error";
  message: string;
  created_at: string;
}

// ─── Startup pipeline ────────────────────────────────────────────────────────

export type StepStatus = "pending" | "running" | "success" | "warning" | "failed";

export interface StartupStep {
  id: string;
  label: string;
  status: StepStatus;
  detail: string | null;
}

/** Which universe the single live feed streams. */
export type UniverseKey = "us_stocks" | "low_float";

export interface UniverseStats {
  cache_symbols: number;
  alpaca_active: number;
  with_float: number;
  /** Total streamable US-stock count (all tradable equities). */
  final_universe: number;
}

export interface StartupState {
  steps: StartupStep[];
  stats: UniverseStats;
  mock_mode: boolean;
  warnings: string[];
  completed: boolean;
}

export interface StreamableSymbol {
  symbol: string;
  exchange: string | null;
  tradable: boolean;
  shortable: boolean;
  float_shares: number | null;
  market_cap: number | null;
  avg_volume: number | null;
  /** Country of origin of the business (sec-api.io), not the listing venue. */
  country: string | null;
  /** English industry name (SEC SIC classification). */
  industry: string | null;
}

// ─── Layout ────────────────────────────────────────────────────────────────────

export interface ZoneAssignment {
  zone_id: string;
  symbol: string | null;
  alert_id: string | null;
  strategy_id: string | null;
  strategy_name: string | null;
  priority: 1 | 2 | 3 | 4 | 5 | null;
  reason: string | null;
  price: number | null;
  placed_at: string | null;
  llm_status: "idle" | "loading" | "done" | "error" | null;
  llm_summary: string | null;
  /** Per-alert chart timeframe override (Opening Interest 1m/5m). */
  display_timeframe: string | null;
  /** Directional bias carried from the alert. */
  side: Side | null;
}

export interface LayoutTab {
  tab_id: string;
  session: Session;
  label: string;
  zones: ZoneAssignment[];
}

// ─── Live market feed ─────────────────────────────────────────────────────────

export type Timeframe = "5s" | "10s" | "1m" | "2m" | "5m" | "15m" | "daily";

// ─── Chart / zone trade context ───────────────────────────────────────────────

export interface ZoneTradeContext {
  zone_id:     string;
  symbol:      string;
  strategy_id: string;
  trade_id:    string | null;
  stop_loss:   number | null;
  take_profit: number | null;
  /** True once the trade closed; the tradeID is kept for journal/screenshots
   *  until a new SL/TP is placed (which starts a fresh trade). */
  closed:      boolean;
}

// ─── Bug reports (persisted) ──────────────────────────────────────────────────

/** 1 = low, 2 = medium, 3 = high. */
export type BugPriority = 1 | 2 | 3;

export interface BugReport {
  id:         string;
  text:       string;
  priority:   BugPriority;
  created_at: string;
}

// ─── Price alarms (persisted; triggering not wired yet) ───────────────────────

export interface PriceAlarm {
  id:          string;
  symbol:      string;
  strategy_id: string | null;
  price:       number;
  created_at:  string;
}

/** A stored alarm enriched (backend) with its strategy's display name +
 *  priority, for the sidebar Alarmes list. */
export interface AlarmView {
  id:            string;
  symbol:        string;
  strategy_id:   string | null;
  strategy_name: string;
  priority:      1 | 2 | 3 | 4 | 5;
  price:         number;
  created_at:    string;
  triggered_at:  string | null;
}

export interface Bar {
  time: string;
  open: number;
  high: number;
  low: number;
  close: number;
  volume: number;
  vwap: number | null;
  /** Number of trades in the bar (Alpaca minute-bar `n`); null otherwise. */
  trade_count?: number | null;
}

/** Previous trading day's reference levels (PDC/PDH/PDL) relative to today. */
export interface PrevDayLevels {
  date: string;
  close: number;
  high: number;
  low: number;
}

/** A user chart drawing persisted per ticker (trend line or text annotation).
 *  'line' uses both points; 'text' uses (t1,p1)+text. Times are chart seconds. */
export interface ChartDrawing {
  id: string;
  symbol: string;
  kind: "line" | "text" | "emoji";
  t1: number;
  p1: number;
  t2: number | null;
  p2: number | null;
  text: string | null;
  /** Timeframe class the drawing belongs to: shown only on matching panes. */
  scope: "intraday" | "daily";
  color?: string | null;
  opacity?: number | null;
  width?: number | null;
  line_style?: "solid" | "dashed" | "dotted" | null;
  font_size?: number | null;
}

/** One execution (fill) drawn on the chart as a triangle at (time, price). */
export interface ExecFill {
  time: string;       // RFC3339
  price: number;
  increase: boolean;  // true = position grew (▶), false = position shrank (◀)
  buy: boolean;       // true = buy fill (green), false = sell fill (red)
}

/** All executions of one trade, grouped for the connecting P&L line. */
export interface TradeExecutions {
  trade_id: string;
  long: boolean;      // green (long) / red (short) triangles
  closed: boolean;    // position flat → line coloured by pnl
  pnl: number;        // realized cash P&L when closed
  /** Launch-time SL (immutable). Drawn as a thin segment for the trade's
   *  duration; null when no SL was set at entry. */
  original_sl: number | null;
  fills: ExecFill[];
}

// ─── Alert enrichment (mirrors AlertEnrichment in types/mod.rs) ───────────────

export type Classification = "pump_dump" | "momo_former";

export interface SplitMarker {
  time: number;   // unix seconds (UTC midnight of the split day)
  label: string;  // e.g. "x20"
}

export interface AlertEnrichment {
  symbol: string;
  strategy_id: string;
  status: string; // "loading" | "done"

  // immediate (DB)
  float_shares: number | null;
  country: string | null;
  country_flagged: boolean;
  industry: string | null;

  // daily calc
  classification: Classification | null;
  split_label: string | null;
  days_since_split: number | null;
  daily_bars: Bar[];
  split_markers: SplitMarker[];
  daily_done: boolean;

  // massive news
  news_title: string | null;
  news_url: string | null;
  news_checked: boolean;

  // deepseek llm — micro_pullback (auto)
  llm_dilution: string | null;
  llm_news: string | null;
  // deepseek llm — panic_mean_reversion (manual, button-triggered)
  llm_context: string | null;
  llm_reversion: string | null;
  llm_pending: boolean;
}

export interface TickerLiveState {
  symbol: string;
  last_price: number | null;
  bid: number | null;
  ask: number | null;
  spread: number | null;
  volume_day: number;
  vwap: number | null;
  high_day: number | null;
  low_day: number | null;
  previous_close: number | null;
  change_day_pct: number | null;
  latency_ui_ms: number | null;
  updated_at: string;
}

export interface MarketSnapshot {
  tickers: Record<string, TickerLiveState>;
  latency: LatencyStatus;
  mock_running: boolean;
  live_running: boolean;
}

// ─── Live feed diagnostics (mirrors FeedDiagnostics in market_state/mod.rs) ───

export interface FeedDiagnostics {
  state: string; // idle|connecting|authenticating|authenticated|subscribed|streaming|error|reconnecting|stopped
  feed: string;
  broad_mode: string; // "trades" (premarket) | "bars" (open)
  subscribed_symbols: number; // broad-tier symbol count
  focus_symbols: number;      // tick-streamed (displayed) symbols
  invalid_symbols_dropped: number;
  trades_received: number;
  quotes_received: number;
  bars_received: number;
  last_message_at: string | null;
  last_error_code: number | null;
  last_error_msg: string | null;
  last_subscribe: string;
  /** What Alpaca confirms is actually subscribed, per channel (ground truth). */
  subscription_ack: string;
  reconnects: number;
  connected_at: string | null;
  updated_at: string;
}

// ─── News investor (Alpaca news WebSocket, premarket) ─────────────────────────

export interface NewsHeadline {
  id: number;
  headline: string;
  summary: string | null;
  url: string | null;
  source: string | null;
  symbols: string[];
  created_at: string;
  received_at: string;
}

/** Mirrors NewsDiagnostics in market_state/mod.rs. */
export interface NewsDiagnostics {
  state: string; // idle|waiting_premarket|connecting|authenticated|subscribed|streaming|error|stopped
  in_premarket: boolean;
  news_received: number;
  symbols_with_news: number;
  last_news_at: string | null;
  last_headline: string | null;
  last_symbols: string[];
  last_error: string | null;
  connected_at: string | null;
  updated_at: string;
  /** Newest-first recent headlines for the debug panel. */
  recent: NewsHeadline[];
}

// ─── Market Replay ─────────────────────────────────────────────────────────────

/** Mirrors ReplayStatus in replay/mod.rs (polled by the replay toolbar). */
export interface ReplayStatus {
  active: boolean;
  state: "idle" | "loading" | "playing" | "paused" | "ended" | "error";
  /** ET date being replayed (YYYY-MM-DD). */
  day: string | null;
  /** "tape" (recorded real trades) | "minutes" (synthesized from 1-min bars). */
  source: string | null;
  sim_time: string | null;
  speed: number;
  playing: boolean;
  /** 0..1 while loading. */
  progress: number;
  symbols: number;
  events_total: number;
  events_done: number;
  error: string | null;
  next_alert_armed: boolean;
}
