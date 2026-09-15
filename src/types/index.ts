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
  /** Micro Pullback and Dislocation: every tempo (seconds) that fired, ascending.
   *  Depth is signal — a move that trips 10s/20s/40s has more behind it than one
   *  that only spikes the 10s. The list widens in place as the move develops. */
  tempo_depth: number[] | null;
}

/** Micro Pullback's own Settings panel (mirrors `micro_pullback::options`).
 *
 *  The boolean-cascade detector: six tempos against each ticker's own rolling
 *  baseline, an n-of-three acceleration rule, then a tape watch. No model in the
 *  decision path. */
export interface MicroPullbackOptions {
  // ── Univers ──
  price_min: number;
  price_max: number;
  /** Free-float ceiling in shares. A KNOWN float above this excludes the ticker. */
  float_max: number;
  /** Watch tickers whose float we have never collected; the alert carries a
   *  `FLOAT_UNKNOWN` tag so the decision stays with the trader. */
  allow_unknown_float: boolean;
  /** Only watch structurally crowded industries. An industry we have never
   *  collected is NOT hot, so it is excluded too — the sharpest edge of this. */
  hot_industry_only: boolean;
  /** Drop the four pump & dump base-rate jurisdictions. A domicile we have never
   *  collected is NOT flagged, so it survives. */
  exclude_risk_domicile: boolean;

  // ── Seuils de détection: multiplicateurs sur la grille compilée ──
  /** × on every tempo's minimum price return. 1 = the compiled grid. */
  return_min_scale: number;
  /** × on every tempo's absolute share AND dollar minimum. */
  volume_min_scale: number;
  /** × on every tempo's three acceleration ratio minimums. */
  ratio_min_scale: number;
  /** How many of the three ratios must clear for a tempo to trip. 2 is the
   *  historical rule; on a feed with no trade count only two ratios exist and a
   *  value of 3 means "both of the two there are". */
  ratios_required: number;

  // ── Montre de tape ──
  /** How long an armed ticker may wait for the tape before the watch is dropped. */
  watch_max_secs: number;
  /** Instantaneous rate (1-second window) that fires on the spot. */
  watch_fast_rate_per_sec: number;
  /** Sustained rate (5-second window). */
  watch_slow_rate_per_sec: number;
  /** Dollars per second the window must carry whichever layer confirms. */
  watch_min_dollar_per_sec: number;

  // ── Scoring IA ──
  /** When on, every fresh departure gets the fast News-only LLM pass automatically
   *  (budgeted below). When off, a button on the alert triggers it manually — never
   *  both at once. */
  llm_auto: boolean;
  /** Rolling budget for the auto path only: at most this many calls in any trailing
   *  5 minutes. A manual click is a human's own budget and ignores this. */
  llm_budget_per_5min: number;

  ntfy_alerts: boolean;

  tempo_10s: boolean;
  tempo_20s: boolean;
  tempo_40s: boolean;
  tempo_1m: boolean;
  tempo_2m: boolean;
  tempo_5m: boolean;
}

/** Dislocation's own Settings panel (mirrors `dislocation::options`).
 *
 *  The scoring engine, formerly "Micro Pullback v2": one 0–100 number made of three
 *  equal thirds — RVOL, attention, news — and a bounded model budget. */
export interface DislocationOptions {
  /** Current event-price universe band. */
  price_min: number;
  price_max: number;
  /** Free-float ceiling in shares. A KNOWN float above this excludes the ticker; an
   *  unknown float is still admitted (and marked FLOAT_INCONNU on the alert). */
  float_max: number;
  /** Only watch "hot" industries — biotech/pharma, space/aerospace/defence, crypto,
   *  quantum, cannabis, AI. Classified from the SEC SIC title in `company_meta`,
   *  with a keyword fallback. An industry we never collected counts as NOT hot, so
   *  it is excluded too. */
  hot_industry_only: boolean;
  /** Drop tickers domiciled in the four pump & dump base-rate jurisdictions: China
   *  (incl. Hong Kong), Cayman Islands, British Virgin Islands. Off = they are
   *  watched exactly like the others; the score has no jurisdiction term. An unknown
   *  domicile is never excluded. */
  exclude_risk_domicile: boolean;
  /** Qualité A+ — and now only that: the second, deeper LLM pass (fundamentals,
   *  dilution, filings). Off by default. It no longer gates the pipeline: the news
   *  pass owns a third of the score and runs whenever there is a fresh headline. */
  quality_aplus: boolean;
  /** Keep watching for a headline after qualification and re-score when one lands —
   *  including for a candidate whose news third was a measured zero. */
  news_watch: boolean;
  /** false (default) = the LLM score gates the alert; true = publish immediately and
   *  let the scores update the row in place. */
  alert_before_score: boolean;
  /** Total score — the mean of the three thirds — required to reach the screen.
   *  With no headline in the window the news third is a measured zero, so a
   *  candidate caps at 66 however violent the tape. */
  alert_score_threshold: number;
  /** How many candidates may be sent to the model per rolling 5 minutes of market
   *  time. Candidates are ranked by priority and the budget is spent from the top;
   *  what does not fit stays eligible for the next tick rather than being lost. */
  llm_budget_per_5min: number;
  /** Push every alert to the phone via ntfy at maximum priority. Needs a topic in
   *  Settings → API Keys; without one the flag is inert rather than an error. */
  ntfy_alerts: boolean;
  tempo_10s: boolean;
  tempo_20s: boolean;
  tempo_40s: boolean;
  tempo_1m: boolean;
  tempo_2m: boolean;
  tempo_5m: boolean;
}

/** Backside's own Settings panel (mirrors `strategies::backside::BacksideOptions`).
 *  Only three knobs — no engine loop, so these are read directly by `should_alert`
 *  from a module-static rather than an AppState field. */
export interface BacksideOptions {
  /** Minimum % change vs the previous day's close to alert. Default 60. */
  change_min_pct: number;
  /** Market cap ceiling in dollars — a candidate must be BELOW this. Default
   *  150,000,000 (the actual Gate 1 rule is "< 150M", a ceiling on small caps). */
  market_cap_max: number;
  /** Minimum price in dollars — a candidate must be AT OR ABOVE this. Default 1. */
  price_min: number;
}

export interface MicroAttentionEntry {
  symbol: string;
  tempo_secs: number;
  rank: number;
  attention_score: number;
  direction_score: number;
  flow_rank: number;
  range_rank: number;
  relative_rank: number | null;
  /** Current tempo volume / median of prior same-length windows. */
  tempo_rvol: number | null;
  baseline_windows: number;
  continuity: number;
  distribution_quality: number;
  evidence_ok: boolean;
  dominance_z: number;
  head_size: number;
  dominant: boolean;
  already_alerted: boolean;
  /** RVOL third of the score, 0..100. Computed backend-side so the number shown
   *  every second and the number persisted with a verdict share one function. */
  rvol_third: number;
  /** Attention third of the score, 0..100. */
  attention_third: number;
  market_time: string;
  received_at: string;
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
  /** Cumulative session volume since 04:00 ET today, live. Null when the symbol
   *  has no live ticker data yet (not zero — genuinely unknown). */
  volume:        number | null;
  float_shares:  number | null;
  /** SIC industry + country of origin (sec-api company_meta cache). */
  industry:      string | null;
  country:       string | null;
  /** Ranking score (0..100) for score-based screeners (Panic Mean Reversion);
   *  null for plain gap/volume screeners. The list sorts on this when present. */
  score:         number | null;
  /** Short label for the score, e.g. "BB 97" / "PR 95"; null when no score. */
  score_label:   string | null;
  /** Raw reversibility score 0..100 from the contextual analysis. The final order
   *  blends extension with `llm_ranking_score`; analysis presence is neutral. */
  llm_panic_score:   number | null;
  /** The one-sentence explanation from the same analysis. */
  llm_one_liner:     string | null;
  /** "faible" | "moyenne" | "forte" — reversibility. */
  llm_reversibility: string | null;
  llm_ranking_score: number | null;
  llm_confidence_score: number | null;
  /** eligible | review | veto */
  llm_eligibility: string | null;
  /** extending | exhausting | reversing | unknown; informational only. */
  llm_timing_state: string | null;
  /** long | short */
  llm_trade_direction: string | null;
  llm_veto_reason: string | null;
  /** Panic score from the most recent PRIOR day this symbol was analysed; null if
   *  never scored on an earlier day. Shown on the card as a faded watermark badge
   *  to the left of today's score. */
  llm_panic_score_prev1: number | null;
  /** Panic score from the day before that — faded further still. */
  llm_panic_score_prev2: number | null;
  updated_at:    string;
}

// ─── Panic Score (LLM analysis of the pre-open watchlist) ──────────────────────

/** Progress of a Panic Score run (mirrors PanicRunState in panic_llm/state.rs). */
export interface PanicRunState {
  phase:       "idle" | "collecting" | "scoring" | "calibrating" | "done" | "error";
  running:     boolean;
  day:         string;
  simulated:   boolean;
  /** Tickers this run has to score: the visible list minus the ones already scored
   *  under the current rubric. */
  total:       number;
  /** Tickers whose score was kept from an earlier press of the same day. */
  carried:     number;
  done:        number;
  /** Symbols that end the run without a score — they are written as N/A at the
   *  bottom of the ranking rather than dropped. */
  failed:      string[];
  /** Set when the run as a whole could not proceed (no key, no visible tickers). */
  error:       string | null;
  /** True when the calibration pass adjusted at least one score. */
  calibrated:  boolean;
  started_at:  string | null;
  finished_at: string | null;
}

/** One ticker's LLM reading (mirrors PanicLlmScore in panic_llm_repository.rs). */
export interface PanicLlmScore {
  symbol:             string;
  day:                string;
  simulated:          boolean;
  /** 0..100 post-calibration — raw contextual reversibility. */
  panic_score:        number | null;
  /** 0..100 as first returned for this ticker alone, before calibration. */
  panic_score_raw:    number | null;
  confidence_score:   number | null;
  catalyst_strength:  number | null;
  catalyst_label:     string | null;
  catalyst_type:      string | null;
  /** "faible" | "moyenne" | "forte" */
  reversibility:      string | null;
  one_liner:          string | null;
  key_drivers:        string[];
  forced_flow:        string | null;
  euphoria:           string | null;
  normalization_prob: number | null;
  /** JSON blob: feed used, missing fields, look-ahead fields. */
  data_quality:       string | null;
  web_used:           boolean;
  web_provider:       string | null;
  adjustment_reason:  string | null;
  rubric_version:     string | null;
  model:              string | null;
  created_at:         string | null;
  ranking_score:      number | null;
  trade_direction:    string | null;
  timing_state:       string | null;
  fundamental_veto:   boolean;
  veto_reason:        string | null;
  eligibility:        string | null;
  catalyst_direction: string | null;
}

/** One Dislocation judgement (mirrors QualityRow in quality_repository.rs).
 *
 *  `red_flags`, `key_drivers` and `tempo_depth` cross the bridge as JSON strings,
 *  exactly as they are stored — the overlay parses them defensively. */
export interface MicroQualityRow {
  symbol:         string;
  day:            string;
  /** "premarket" | "open" | … — the A+ lock is per session, so one ticker can hold
   *  two judgements in one day and they are about two different moves. */
  session:        string;
  simulated:      boolean;
  /** The news pass's own note on the headline, kept even when the deep pass
   *  overrode it. NOT the news third: this one is not decayed by the age. */
  pass1_score:    number | null;
  pass2_score:    number | null;
  /** The mean of the three thirds, 0..100. The verdict. */
  final_score:    number | null;
  threshold:      number | null;
  passed:         boolean;
  /** The three thirds as one readable line, for the audit view. */
  adjustments:    string | null;
  /** RVOL third at the instant of the verdict, 0..100. The pane prefers the live
   *  attention board for this one — it moves every second. */
  rvol_score:      number | null;
  /** Attention third at the instant of the verdict, 0..100. */
  attention_score: number | null;
  /** News third: the model's note decayed by the headline's age, 0..100. This one
   *  does NOT move afterwards, and it is what the chart pastille shows. */
  news_score:      number | null;
  confidence:     number | null;
  sustain_prob:   number | null;
  /** "solide" | "faible" | "bluff" | "aucune" */
  news_verdict:   string | null;
  /** "breaking" | "fraiche" | "recente" | "ancienne" | "posterieure" | "aucune" */
  news_freshness: string | null;
  catalyst_type:  string | null;
  catalyst_label: string | null;
  /** "faible" | "moyen" | "eleve" | "critique" */
  dilution_risk:  string | null;
  dilution_read:  string | null;
  financial_read: string | null;
  /** JSON array. */
  red_flags:      string | null;
  /** JSON array. */
  key_drivers:    string | null;
  one_liner:      string | null;
  entry_note:     string | null;
  /** JSON array of tempo seconds. */
  tempo_depth:    string | null;
  rejection:      string | null;
  rubric_version: string | null;
  /** "news" | "deep" — which pass produced `final_score`. */
  model:          string | null;
  /** MARKET time of the verdict (replay clock in a replay). Compared with a
   *  headline's publication time to tell a current score from a stale one. */
  scored_at:      string | null;
}

/** What the A+ pipeline is doing for one symbol right now. Absent from the list
 *  means nothing is in flight and the persisted verdict is current. */
export interface MicroQualityLive {
  symbol:              string;
  /** "queued" | "news" | "deep" | "rescoring", or null once the pass ends. */
  phase:               string | null;
  /** A headline that landed after the displayed score and has not been folded in. */
  pending_headline:    string | null;
  pending_headline_at: string | null;
}

/** One poll of the A+ board: the day's judgements and what is still moving. */
export interface MicroQualityBoard {
  rows: MicroQualityRow[];
  live: MicroQualityLive[];
}

/** One Micro Pullback catalyst read (mirrors MicroPullbackQualityRow in
 *  micro_pullback_quality_repository.rs). Deliberately lighter than
 *  {@link MicroQualityRow}: Micro Pullback has no RVOL/attention thirds and no
 *  pass/fail threshold — `score` IS the model's `quality_score`, shown as-is. */
export interface MicroPullbackQualityRow {
  symbol:     string;
  day:        string;
  session:    string;
  simulated:  boolean;
  /** The model's quality_score, 0..100. No blending, no threshold. */
  score:      number | null;
  /** "solide" | "faible" | "bluff" | "aucune" */
  verdict:    string | null;
  /** "breaking" | "fraiche" | "recente" | "ancienne" | "posterieure" | "aucune" */
  freshness:  string | null;
  /** JSON array. */
  red_flags:  string | null;
  one_liner:  string | null;
  model:      string | null;
  scored_at:  string | null;
}

/** Decoded `data_quality` blob. Every field optional — it is stored as free JSON. */
export interface PanicDataQuality {
  feed?:      string;
  iex_only?:  string[];
  missing?:   string[];
  lookahead?: string[];
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
  /** Cumulative volume since 04:00 ET today (premarket + regular + any
   *  after-hours so far). Backs the chart top bar's single "Vol" chip. */
  volume_since_4am:  number | null;
  /** Relative volume: today's cumulative volume over the symbol's average daily
   *  volume. Same formula as the scanner's live rvol. */
  rvol:              number | null;
  /** A live news headline is on file for the symbol. */
  has_news:          boolean;
  /** Most recent live headline text, if any. */
  news_title:        string | null;
  // ── Behavioural / risk scores (0..100, 100 = worst; null = inputs not
  //    collected). NO LONGER DRAWN: the Dislocation overlay used to render these
  //    five as unlabelled bars, three of them saying the same thing about dilution.
  //    The A+ deep pass reads the same filings and answers in one word
  //    (`MicroQualityRow.dilution_risk`) with a sentence behind it, which is what
  //    the pane shows now. Still served, still available to anything that wants a
  //    number rather than a verdict. ────────────────────────────────────────────
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

/** One headline for the Dislocation overlay (mirrors CardNews in
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
  streaming_scope: StreamingScope;
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
  /** Entry legs filled so far vs. still resting for this trade (scaled entries). */
  filled_entries:  number;
  pending_entries: number;
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
  /** Data-source policy. Flat files and the Alpaca API are not modes but
   *  complementary sources: on-disk history is read first and the API fills the
   *  gaps. `offline` is the single switch — never touch the network, so flat files
   *  only and no live feed. */
  data_source: { offline: boolean };
  /** Where orders go. Independent of `data_source` AND of the market-data plan:
   *  a free IEX subscription can trade every ticker on a real account. */
  broker: { broker: "tagdash" | "alpaca"; environment: "paper" | "live" };
  /** Offline Speech-to-Text dictée pipeline (whisper.cpp). */
  stt: SttConfig;
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
  /** Market-data credentials (bars, live stream, subscription probe). */
  alpaca_key: boolean;
  alpaca_secret: boolean;
  /** Trading credentials — one pair per account, not interchangeable. */
  alpaca_paper_key: boolean;
  alpaca_paper_secret: boolean;
  alpaca_live_key: boolean;
  alpaca_live_secret: boolean;
  fmp_api_key: boolean;
  massive_api_key: boolean;
  sec_api_key: boolean;
  claude_api_key: boolean;
  deepseek_api_key: boolean;
  /** Open-web search for the Panic Score context cascade. One of the two suffices;
   *  Tavily wins when both are set. Never called during a Market Replay. */
  tavily_api_key: boolean;
  brave_api_key: boolean;
  tradetally_token: boolean;
  tradetally_email: boolean;
  tradetally_password: boolean;
  /** ntfy push notifications. The TOPIC is a secret on the public server: anyone
   *  who knows it can read it and publish to it. The server URL and token are
   *  optional — a public ntfy.sh topic needs neither. */
  ntfy_server: boolean;
  ntfy_topic: boolean;
  ntfy_token: boolean;
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

/** Live-feed subscription scope a strategy needs beyond charts on screen + orders. */
export type StreamingScope = "none" | "watchlist" | "universe";

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
  /** Launch-time SL (immutable). Drawn as a thin red segment for the trade's
   *  duration; null when no SL was set at entry. */
  original_sl: number | null;
  /** Launch-time TP (immutable). Same thin segment in green; null when no TP
   *  was set at entry. */
  original_tp: number | null;
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
  live_running: boolean;
}

// ─── Live feed diagnostics (mirrors FeedDiagnostics in market_state/mod.rs) ───

export interface FeedDiagnostics {
  state: string; // idle|connecting|authenticating|authenticated|subscribed|streaming|error|reconnecting|stopped
  feed: string;
  /** What the tape physically is: "prints" (one message per real trade) or
   *  "second_aggregates" (one message per symbol per second, Massive). */
  tape_kind?: string;
  /** Whether a trade COUNT is available. Always true on prints; on Massive it turns
   *  true once a frame carries the average trade size `z`, from which the count is
   *  reconstructed. False → the tape gates fall back to dollars + continuity. */
  tape_counts_trades?: boolean;
  /** Subscription detected at connect time: "sip" (paid) | "iex" (free) |
   *  "massive_unlimited" (Massive Starter+ replacing IEX — 15-min delayed,
   *  per-second bars, no trades/quotes). */
  plan?: string;
  /** Max streamable symbols for that plan; 0 = unlimited. Free tier is 30. */
  symbol_limit?: number;
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

/**
 * Which tape a replay reads — the day-picker toggle, mirrors `SourceChoice` in
 * replay/data.rs. "api" runs the ranked walk (SIP > Massive > flat files); the two
 * flat variants pin the session to that local dataset and never fall back.
 */
export type ReplaySource = "api" | "flat_trades" | "flat_minutes";

/** Mirrors ReplayStatus in replay/mod.rs (polled by the replay toolbar). */
export interface ReplayStatus {
  active: boolean;
  state: "idle" | "loading" | "playing" | "paused" | "ended" | "error";
  /** ET date being replayed (YYYY-MM-DD). */
  day: string | null;
  /**
   * Granularity of the replayed price path: "tape" (recorded real trades) |
   * "seconds" (real 1-second aggregates) | "trades" (real prints + quotes over the
   * ignition windows, synthetic minute slices elsewhere) | "minutes" (synthesized
   * from 1-min bars) | "daily" (the day's single OHLCV bar walked across the
   * session).
   */
  source: string | null;
  /**
   * Where the day was loaded from, resolved with the same priority as the live
   * feed: "alpaca_sip" | "massive" | "alpaca_iex" | "flat_files" | "daily_cache".
   */
  provider: string | null;
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

// ─── Broker ──────────────────────────────────────────────────────────────────

/** The active account, and the one flag the UI keys its real-money warnings on. */
export interface BrokerSelection {
  broker: "tagdash" | "alpaca";
  environment: "paper" | "live";
  account_id: string;
  /** True ⇒ fills spend REAL money. */
  is_live: boolean;
  credentials_ready: boolean;
}

/** Auto-detected vs. manually-forced Massive subscription tier — mirrors
 *  src-tauri/src/commands/broker.rs::MassiveTierStatus. `"unlimited"` (Starter+,
 *  paid) is 15-minute delayed and disables the Alpaca broker; `"basic"` (free)
 *  is unaffected. */
export interface MassiveTierStatus {
  detected: "unlimited" | "basic" | null;
  override: "unlimited" | "basic" | null;
  effective: "unlimited" | "basic";
}

export type BrokerOrderStatus = "open" | "held" | "filled" | "closed";

/** One order as the BROKER holds it — never a locally-computed view. */
export interface BrokerOrder {
  id: string;
  client_order_id: string;
  symbol: string;
  side: string;
  order_type: string;
  time_in_force: string;
  quantity: number;
  filled_quantity: number;
  limit_price: number | null;
  stop_price: number | null;
  /** The executed average price. Authoritative — not what we asked for. */
  filled_avg_price: number | null;
  status: BrokerOrderStatus;
  raw_status: string;
  parent_id: string | null;
  submitted_at: string | null;
  filled_at: string | null;
  canceled_at: string | null;
  reject_reason: string | null;
}

/** One position as the broker holds it. Quantity is signed; negative is short. */
export interface BrokerPosition {
  symbol: string;
  quantity: number;
  avg_entry_price: number;
  market_price: number | null;
  market_value: number | null;
  cost_basis: number | null;
  unrealized_pnl: number | null;
  unrealized_pnl_pct: number | null;
}

export interface BrokerAccount {
  id: string;
  broker: "tagdash" | "alpaca";
  environment: "paper" | "live";
  status: string;
  currency: string;
  cash: number | null;
  equity: number | null;
  buying_power: number | null;
  trading_blocked: boolean;
  account_blocked: boolean;
  /** The holder's own "suspend trading" switch in Alpaca's dashboard. Alpaca refuses
   *  every new order with code 40310000 while it is on. */
  trade_suspended_by_user: boolean;
  pattern_day_trader: boolean;
  daytrade_count: number | null;
}

/** The venue's current view, as last observed. */
export interface BrokerMirror {
  account: BrokerAccount | null;
  orders: Record<string, BrokerOrder>;
  positions: Record<string, BrokerPosition>;
  last_reconcile: string | null;
  last_event: string | null;
  stream_connected: boolean;
  last_error: string | null;
}

/** Whether a symbol may be sold short, per the venue. */
export interface Shortability {
  symbol: string;
  broker: "tagdash" | "alpaca";
  shortable: boolean;
  borrow_status: "easy_to_borrow" | "hard_to_borrow" | "unavailable" | "unknown";
  from_cache: boolean;
}

export interface LocateRecord {
  id: string;
  broker: "tagdash" | "alpaca";
  symbol: string;
  requested_qty: number;
  located_qty: number;
  available_qty: number | null;
  price_per_share: number;
  total_fee: number;
  status: string;
  created_at: string | null;
  expires_at: string | null;
}

/** Known loss band for a short after locate and, when Alpaca exposes it, borrow. */
export interface ShortCostZone {
  broker: "tagdash" | "alpaca";
  symbol: string;
  source: "estimate" | "position";
  entry_price: number;
  stop_loss: number;
  quantity: number;
  located_quantity: number;
  locate_fee: number;
  annual_borrow_rate: number | null;
  borrow_days: number;
  borrow_fee: number | null;
  total_known_cost: number;
  break_even_price: number;
  costs_complete: boolean;
  locates: LocateRecord[];
}
