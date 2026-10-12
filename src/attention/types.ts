// Types of the detached « Attention du marché » screen — mirror of
// `src-tauri/src/attention_map` (view.rs, options.rs, commands/attention.rs).

export type Group = "small" | "big" | "unknown" | "etf";
export type Pattern = "held" | "climb" | "spike_fade" | "relaunch" | "decline" | "neutral";

/** `[slot, x, y, $/min, flags]` — flags: 1 = counts for price, 2 = still filling. */
export type Pt = [number, number, number, number, number];

export interface PartStats {
  n: number;
  vol: number;
  dollars: number;
  hi: number | null;
  lo: number | null;
  x_peak: number | null;
  x_peak_i: number | null;
  y_max: number | null;
  y_max_i: number | null;
  y_min: number | null;
  y_min_i: number | null;
}

export interface TickerView {
  s: string;
  name: string | null;
  group: Group;
  cap: number | null;
  float: number | null;
  ref_close: number;
  split: string | null;
  /** Sessions of history behind the medians [after-hours, premarket]. */
  base_n: [number, number];
  pts: Pt[];
  ah: PartStats;
  pm: PartStats;
  pat_all: Pattern;
  pat_pm: Pattern;
  score_all: number;
  score_pm: number;
  last: number;
  accel: number | null;
}

export interface Gauge {
  value: number;
  median: number | null;
  ratio: number | null;
}

export interface Weather {
  slot: number | null;
  sessions: number;
  small: Gauge;
  global: Gauge;
  breadth: Gauge;
  concentration: Gauge;
}

export interface FrameInfo {
  day: string;
  prev_day: string;
  ah_start_s: number;
  pm_start_s: number;
  n_ah: number;
  n_pm: number;
  slot_secs: number;
  early_close: boolean;
}

export interface MapView {
  frame: FrameInfo;
  closed: number;
  filling: number | null;
  seen_s: number;
  delayed_min: number;
  float_date: string | null;
  hist_sessions: [number, number];
  hist_target: number;
  tickers: TickerView[];
  weather: Weather;
  universe: number;
  no_ref: number;
}

export interface EngineStatus {
  state: "off" | "loading" | "ready" | "no_creds" | "offline" | "error";
  message: string | null;
  history_done: number;
  history_target: number;
  history_building: boolean;
}

export interface ReviewContext {
  day: string;
  at_ms: number;
  symbol: string | null;
}

export interface MapReply {
  key: string;
  mode: "live" | "replay" | "review";
  status: EngineStatus;
  map: MapView | null;
  review: ReviewContext | null;
  review_missing: boolean;
}

export interface Candle {
  t: number;
  i: number;
  o: number;
  h: number;
  l: number;
  c: number;
  v: number;
}

export interface Detail {
  s: string;
  ref_close: number;
  candles: Candle[];
}

export interface AttentionOptions {
  min_rel: number;
  min_move_pct: number;
  min_dollars_5m: number;
  baseline_floor: number;
  small_cap_max: number;
  heat_low: number;
  heat_high: number;
  heat_extreme: number;
  breadth_low: number;
  breadth_high: number;
  concentration_low: number;
  concentration_high: number;
}

export interface AttentionWindowState {
  enabled: boolean;
  open: boolean;
}

export type Universe = "small" | "all" | "big" | "etf";
export type MapMode = "traj" | "bubbles";
