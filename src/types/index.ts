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
  risk: StrategyRiskConfig;
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

/** One ticker on the Market Attention top list (mirrors AttentionEntry in
 *  src-tauri/src/types/mod.rs). Direction-agnostic snapshot of how much attention
 *  the market is paying to a ticker over the rolling 5-minute window (09:30–12:30
 *  ET). Surfaced only via the get_market_attention debug command. */
export interface AttentionEntry {
  symbol:                   string;
  /** Composite attention score, 0..100. */
  attention_score:          number;
  dollar_volume_5m:         number;
  volume_5m:                number;
  trade_count_5m:           number;
  /** Cross-sectional percentile ranks among gate-1/2 survivors, 0..1. */
  pr_dollar_volume_5m:      number;
  pr_volume_5m:             number;
  pr_trade_count_5m:        number;
  /** Current 5m $vol vs the ticker's own historical average at this time of day;
   *  null until the historical baseline is cached. */
  relative_attention_5m:    number | null;
  market_share_5m:          number;
  smallcap_market_share_5m: number;
  under20_market_share_5m:  number;
  active_minutes_5m:        number;
  max_1m_volume_share:      number;
  /** Current 5m $vol vs the prior 5m (−10..−5 min); null when prior was empty. */
  acceleration_5m:          number | null;
  updated_at:               string;
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
  // ── Micro Pullback overlay: behavioural / risk scores (0..100, 100 = worst;
  //    null = inputs not collected). ──────────────────────────────────────────
  short_interest_score:    number | null;
  dilution_capacity_score: number | null;
  dilution_need_score:     number | null;
  dilution_score:          number | null;
  pump_dump_score:         number | null;
  /** Real-time liquidity: shares traded in the last 60s (rolling). null = no
   *  intraday bars yet. Drives the overlay's Vol bar (not the cumulative session). */
  live_volume:             number | null;
}

/** HOD Drive on-chart overlay payload (mirrors HodDriveOverlay in
 *  commands/mod.rs). The five KPIs shown top-right plus the HOD/LOD levels (+ bar
 *  times) and the green-series bar times used to draw the chart points/crosses.
 *  All KPI fields are null until the symbol has enough session structure. Ratios
 *  are 0..1 fractions except `pullback_vol_ratio` (1.0 = equal, 2.0 = double). */
export interface HodDriveOverlay {
  timeframe:              string;
  /** series_range / (HOD−LOD), 0..1. */
  series_share:           number | null;
  pullback_volume:        number | null;
  /** pullback_volume / series_volume (1.0 = equal, 0.5 = half, 2.0 = double). */
  pullback_vol_ratio:     number | null;
  power_score:            number | null;
  directional_efficiency: number | null;
  hod:                    number | null;
  lod:                    number | null;
  /** Unix seconds of the HOD / LOD bars. */
  hod_time:               number | null;
  lod_time:               number | null;
  /** Unix seconds of every bar in the green series. */
  series_bar_times:       number[];
  /** True when Gates 1-3 currently pass. */
  gates_pass:             boolean;
  /** (HOD−LOD) / avg range of green daily candles. 1.0 = identical, 0.5 = half. */
  range_vs_green_atr:     number | null;
  /** Suggested limit-entry price (slightly above last bar high). */
  suggested_entry:        number | null;
  /** Suggested stop-loss price (slightly below last bar low). */
  suggested_sl:           number | null;
  /** Suggested take-profit price (slightly below HOD). */
  suggested_tp:           number | null;
  /** R/R of the suggested trade. */
  suggested_rr:           number | null;
  /** MACD trend: true = healthy (histogram > 0), false = exhausted. */
  macd_open:              boolean | null;
  /** 0..1 normalised histogram magnitude vs session peak. */
  macd_strength:          number | null;
}

/** One headline for the Micro Pullback overlay (mirrors CardNews in
 *  commands/mod.rs), fetched per displayed ticker via Alpaca REST. `created_at`
 *  is the publish time (RFC 3339). */
export interface CardNews {
  headline:   string;
  created_at: string;
  source:     string | null;
}

// ─── Strategy display config ──────────────────────────────────────────────────

export interface StrategyRiskConfig {
  max_risk_dollars: number;
  default_order_type: "market" | "limit";
  auto_tp_enabled: boolean;
  auto_tp_r: number;
  auto_be_enabled: boolean;
  auto_be_r: number;
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
  /** Optional layout column. Panes sharing a column are stacked vertically (in
   *  declaration order); columns lay out left-to-right. null/undefined = the pane
   *  gets its own column (legacy side-by-side). */
  column?: number | null;
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

// ─── Dashboard (moodboard) ────────────────────────────────────────────────────

/** One trade mirrored from TradeTally (mirrors DashboardTrade in dashboard/mod.rs).
 *  `raw` is the full upstream object so future cards can read un-mapped fields. */
export interface DashboardTrade {
  tt_id:       string;
  symbol:      string | null;
  side:        string | null;
  quantity:    number | null;
  entry_price: number | null;
  exit_price:  number | null;
  pnl:         number | null;
  pnl_percent: number | null;
  entry_date:  string | null;
  exit_date:   string | null;
  commission:  number | null;
  fees:        number | null;
  status:      string | null;
  setup:       string | null;
  strategy:    string | null;
  broker:      string | null;
  tags:        string[];
  raw:         unknown;
}

/** The daily background photo + its folder (mirrors DailyBackground in
 *  dashboard/mod.rs). `data_url` is null when the folder has no images. */
export interface DailyBackground {
  dir:       string;
  file_name: string | null;
  data_url:  string | null;
}

/** One random inspiration image (mirrors MoodImage in dashboard/mod.rs). */
export interface MoodImage {
  file_name: string;
  data_url:  string;
}

/** A fresh random mood pick (mirrors Mood in dashboard/mod.rs): one image + one
 *  short phrase + one long phrase. Each field is null when its folder/file is
 *  empty. The paths are returned so the UI can open them. */
export interface Mood {
  images_dir:   string;
  short_path:   string;
  long_path:    string;
  image:        MoodImage | null;
  short_phrase: string | null;
  /** Second short phrase, distinct from short_phrase — drives the H1 card. */
  heading_phrase: string | null;
  long_phrase:  string | null;
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
    /** When to play a sound on a new scanner alert, by trading session. */
    alert_sound_mode: AttentionMode;
    /** Which notification sound to play (see notifSounds.ts for the catalog). */
    alert_sound: string;
  };
  latency: { warn_ms: number; critical_ms: number };
  tradetally: { api_base_url: string };
  /** User-defined journal tags (replaces TradeTally-fetched tags). */
  journal: { tags: string[] };
  /** Where market data comes from: live Alpaca API or on-disk flat files. In
   *  flat-files mode there is no real-time feed — Market Replay only. */
  data_source: { mode: "api" | "flat_files" };
  /** Offline Speech-to-Text dictée pipeline (whisper.cpp). */
  stt: SttConfig;
  risk_management: {
    default_order_type: "limit" | "market";
    auto_be_enabled: boolean;
    auto_be_r: number;
  };
}

// ─── Speech-to-Text (offline dictée → trade notes / diary) ───────────────────────

export interface SttConfig {
  enabled: boolean;
  /** Whisper model size. */
  model: "small" | "medium";
  /** Forced transcription language (ISO). */
  language: string;
  /** Trading vocabulary fed to whisper as the initial prompt (bias detection). */
  jargon: string[];
  /** Pause the worker while global CPU usage is above this percentage. */
  pause_cpu_pct: number;
  /** Pause the worker during the first N minutes after the 09:30 ET cash open. */
  pause_market_open_minutes: number;
  /** Preferred input device name; null = system default. */
  input_device: string | null;
}

export type SttJobKind = "trade" | "diary";
export type SttJobState = "queued" | "running" | "done" | "error" | "cancelled";

export interface SttJob {
  id: string;
  kind: SttJobKind;
  trade_id: string | null;
  symbol: string | null;
  state: SttJobState;
  attempts: number;
  error: string | null;
  text: string | null;
  created_at: string;
}

export interface SttStatus {
  enabled: boolean;
  model: string;
  model_present: boolean;
  downloading: boolean;
  download_progress: number;
  recording: boolean;
  recording_kind: SttJobKind | null;
  worker_state: string;
  paused_reason: string | null;
  jobs: SttJob[];
  error: string | null;
}

export interface MicTestResult {
  ok: boolean;
  level: number;
  device: string | null;
  error: string | null;
}

/** Payload of the `stt-spectrum` event emitted while recording. */
export interface SttSpectrum {
  bins: number[];
  level: number;
}

/** Payload of the `stt-diary-result` event (worker → dashboard journal card). */
export interface SttDiaryResult {
  block: string;
  title: string | null;
}

/** Which flat-files dataset: trades+quotes, minute bars, or daily bars. */
export type FlatFilesKind = "trade" | "minute" | "daily";

/** Background flat-files download progress (polled while running). */
export interface FlatFilesStatus {
  running:     boolean;
  /** Which dataset is downloading: trade | minute | daily. */
  kind:        string;
  /** idle | running | done | cancelled | error */
  state:       string;
  current_day: string | null;
  day_index:   number;
  day_total:   number;
  /** 0..1 within the current day (or whole-range chunk for daily). */
  progress:    number;
  error:       string | null;
  last_done:   string | null;
}

/** One day present on disk (downloaded or imported), shown in the calendar. For
 *  daily, rows are the distinct dates covered by the cumulative file. */
export interface FlatFileDay {
  day:          string;
  bytes:        number;
  /** symbols stored (minute) / with windows (trade) / on the date (daily). */
  symbol_count: number;
  /** bars (minute/daily) or trades (trade) stored. */
  bar_count:    number;
  /** false for a partial/interrupted download. */
  complete:     boolean;
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
  tradetally_email: boolean;
  tradetally_password: boolean;
}

/** Which secret keys can be set from the UI. */
export type SecretKey = keyof SecretsStatus;

/** Partial secrets update — only the keys the user typed (non-empty) are sent. */
export type SecretsUpdate = Partial<Record<SecretKey, string>>;

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

export interface TodoTrade {
  trade_id:       string;
  symbol:         string;
  open:           boolean;
  pnl:            number;
  has_screenshot: boolean;
  has_journal:    boolean;
}

export interface TradeDbRow {
  trade_id:             string;
  symbol:               string;
  side:                 string;
  open:                 boolean;
  pnl:                  number;
  fills:                number;
  first_fill_at:        string;
  last_fill_at:         string;
  has_note:             boolean;
  has_screenshot:       boolean;
  sent_to_tradetally:   boolean;
  synced_on_tradetally: boolean;
}

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

/** One news pastille for the chart overlay (Alpaca news REST). */
export interface NewsMarker {
  time: number;     // unix seconds (publish time)
  headline: string;
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

// ─── Tickers data table (mirrors TickerTableRow in company_intel/model.rs) ───────
// One flattened row: universe asset + every enrichment (fundamentals, company
// meta, company intel) + news/filings counts. All-Option = "not collected".
export interface TickerTableRow {
  symbol: string;
  name: string | null;
  exchange: string | null;
  tradable: boolean;
  shortable: boolean;
  float_shares: number | null;
  market_cap: number | null;
  avg_volume: number | null;
  outstanding_shares: number | null;
  free_float: number | null;
  prev_close: number | null;
  atr: number | null;
  change_1d_pct: number | null;
  change_2d_pct: number | null;
  change_3d_pct: number | null;
  change_4d_pct: number | null;
  change_5d_pct: number | null;
  change_6d_pct: number | null;
  pump_dump_score: number | null;
  dilution_score: number | null;
  dilution_pct_12m: number | null;
  shares_outstanding_12m: number | null;
  dilution_capacity_score: number | null;
  dilution_need_score: number | null;
  short_interest_score: number | null;
  last_split_date: string | null;
  last_split_label: string | null;
  split_count_1y: number | null;
  country: string | null;
  industry: string | null;
  sector: string | null;
  sic: string | null;
  short_interest: number | null;
  days_to_cover: number | null;
  short_interest_settlement: string | null;
  net_income_last_q: number | null;
  net_income_ttm: number | null;
  negative_quarters_last4: number | null;
  operating_cash_flow_ttm: number | null;
  cash_and_equivalents: number | null;
  financials_period_end: string | null;
  has_recent_shelf: boolean;
  latest_dilution_form: string | null;
  latest_dilution_date: string | null;
  dilution_atm: boolean;
  dilution_resale: boolean;
  dilution_warrants: boolean;
  offering_amount: number | null;
  institutional_ownership_pct: number | null;
  insider_ownership_pct: number | null;
  holders_5pct_count: number | null;
  restricted_shares: number | null;
  filings_count: number;
  news_count: number;
  intel_updated_at: string | null;
}
