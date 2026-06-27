// HOD Drive — stateful, multi-timeframe "clean drive then pullback toward the HOD"
// engine for the regular session (Open tab).
//
// Goal: spot tickers that take off cleanly after the open, then pull back in an
// exploitable way with a good risk/reward toward the HOD. Every structural decision
// is built on CLOSED bars of the engine's own timeframe — the forming bar is never
// used. The pipeline, run at each bar close from the 3rd closed bar onward:
//
//   Gate 1 — Universe   : price ≥ 2$, price > open, and enough volume / dollar
//                         volume since the open.
//   Gate 2 — Risk Ratio : the last closed bar offers a shallow pullback (≤ 60 %
//                         retracement of the open range) with reward/risk ≥ 2 toward
//                         the HOD.
//   Gate 3 — Clear Pattern : the run off the open is wide enough, the longest bullish
//                         sequence carries most of the range, and it is "powerful"
//                         (body-dominated, power score ≥ 60 %).
//   Gate 4 — Live liquidity : a 5-second real-time confirmation (tight spread, real
//                         dollar volume) before the ticker is sent to the Open scanner.
//
// The strategy is multi-timeframe by design (30s / 1m / 2m / 5m / 10m), each timeframe
// owning its own config and recomputing only at its own bar closes. 30s reasons on
// trade data; 1–10m on 1-minute bars. THIS V1 SHIPS THE 5-MINUTE ENGINE ONLY — the
// other timeframes (V2) and the short variant (V3) plug into the same pure pipeline.
//
// Why an engine and not a `ScanStrategy::should_alert`: the gates need the whole
// session structure (open range, HOD/LOD, the longest bullish sequence) plus per-symbol
// anti-spam + the 5s live-liquidity hold, none of which fit the stateless per-tick
// contract. So this engine runs in its own tokio task and pushes AlertSignals straight
// into the active-alert list via `scanner::push_alert`. The registry still carries a
// metadata `HodDrive` strategy (card, toggle, name, priority) — see
// `strategies::hod_drive`.
//
// Bars: during the regular session Alpaca streams 1-minute bars for the whole universe
// (MarketState::on_bar → M1 ring); we read those closed M1 bars and aggregate the
// 5-minute series ourselves (bounded to ≥ the 09:30 ET cash open).

use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, RwLock,
};

use chrono::{DateTime, Duration as ChronoDuration, TimeZone, Utc};

use crate::market_state::aggregators::{Bar, Timeframe};
use crate::market_state::MarketState;
use crate::scanner::push_alert;
use crate::strategies::hod_drive::ID as STRATEGY_ID;
use crate::types::{AlertSignal, Session, Side};

// ═══════════════════════════════════════════════════════════════════════════════
//  USER SETTINGS — per-timeframe config blocks (the future UI edits these).
//  Each timeframe has its own engine and recomputes only at its own bar closes.
//  V1 ships the 5-minute block only; V2 fills the others, V3 adds the short side.
// ═══════════════════════════════════════════════════════════════════════════════

/// All tunables for one timeframe's HOD Drive engine. One `TfConfig` per timeframe
/// so each can be tuned independently (and surfaced as its own UI section).
#[derive(Debug, Clone, Copy)]
pub struct TfConfig {
    /// Display + alert label ("5m") and the bucket length in seconds.
    pub label:        &'static str,
    pub bucket_secs:  i64,
    /// Master on/off for this timeframe (only 5m is on by default in V1).
    pub enabled:      bool,

    // ── Active window ──
    /// Bars that must have closed before the pipeline runs (spec: 3).
    pub min_closed_bars:   usize,
    /// Active duration since the open = min(timeframe × 20, 120 min). Precomputed
    /// here in seconds (5m → 100 min).
    pub active_secs:       i64,

    // ── Gate 1 — Universe ──
    pub min_price:            f64,   // ≥ 2$
    pub min_volume_since_open: u64,  // ≥ 200k shares
    pub min_dollar_volume_since_open: f64, // OR ≥ 200k$

    // ── Gate 2 — Risk Ratio (closed bars only) ──
    pub max_retracement: f64, // ≤ 0.60
    pub min_risk_ratio:  f64, // ≥ 2.0

    // ── Gate 3 — Clear Pattern ──
    pub min_range_dollars:    f64, // (HOD-LOD) ≥ 0.50$ ...
    pub min_range_pct:        f64, // ... OR (HOD-LOD)/open ≥ 5%
    /// A red bar is tolerated inside the bullish sequence iff its body ≤ this share
    /// of its range, its low holds the previous low, and its close holds within
    /// `red_close_tol` × previous range of the previous close.
    pub red_body_max_ratio:   f64, // ≤ 0.30
    pub red_close_tol:        f64, // 0.25
    /// Tolerance caps: at most 1 red OR ≤ this fraction of the sequence red.
    pub red_max_fraction:     f64, // 0.20
    pub min_series_share:     f64, // series_range / open_range ≥ 0.50
    pub min_power_score:      f64, // ≥ 0.60

    // ── Gate 4 — Live liquidity (5-second hold) ──
    pub liquidity_hold_secs:   i64, // 5
    pub max_spread_pct:        f64, // ≤ 2%
    pub min_dollar_volume_5s:  f64, // ≥ 10k$ over the hold
}

/// 5-minute timeframe (the only one wired in V1). active = min(5m×20, 120m) = 100 min.
pub const CFG_5M: TfConfig = TfConfig {
    label:        "5m",
    bucket_secs:  300,
    enabled:      true,

    min_closed_bars:   3,
    active_secs:       100 * 60,

    min_price:                        2.0,
    min_volume_since_open:            200_000,
    min_dollar_volume_since_open:     200_000.0,

    max_retracement: 0.60,
    min_risk_ratio:  2.0,

    min_range_dollars:  0.50,
    min_range_pct:      0.05,
    red_body_max_ratio: 0.30,
    red_close_tol:      0.25,
    red_max_fraction:   0.20,
    min_series_share:   0.50,
    min_power_score:    0.60,

    liquidity_hold_secs:  5,
    max_spread_pct:       0.02,
    min_dollar_volume_5s: 10_000.0,
};

// V2 — placeholders kept here so the per-timeframe layout is already visible. Each
// will get its own engine task once implemented (30s reasons on trade data).
//
// pub const CFG_30S: TfConfig = TfConfig { label: "30s", bucket_secs: 30, enabled: false, active_secs: 10*60, .. };
// pub const CFG_1M:  TfConfig = TfConfig { label: "1m",  bucket_secs: 60,  enabled: false, active_secs: 20*60, .. };
// pub const CFG_2M:  TfConfig = TfConfig { label: "2m",  bucket_secs: 120, enabled: false, active_secs: 40*60, .. };
// pub const CFG_10M: TfConfig = TfConfig { label: "10m", bucket_secs: 600, enabled: false, active_secs: 120*60, .. };

// ── Engine loop / priority / risk (shared identity, kept in sync with the card) ──
/// Alert priority (spec: 3 = "Normal-high").
pub const PRIORITY: u8 = 3;
/// Max risk dollars per trade (spec: 100$).
pub const MAX_RISK_DOLLARS: f64 = 100.0;
/// Anti-spam: one fire per closed bar per symbol; after a fire, no second alert for
/// the same symbol for this long unless a new bar re-qualifies.
pub const COOLDOWN_SECS: u64 = 600;

// ── Suggested-trade offsets (fractions of the pullback bar's risk = high−low) ──
const ENTRY_OFFSET: f64 = 0.05; // limit entry slightly above last.high
const SL_OFFSET:    f64 = 0.10; // SL slightly below last.low
const TP_OFFSET:    f64 = 0.10; // TP slightly below HOD

/// How often the engine evaluates (seconds). Structural decisions only change on a
/// new closed bar; a short loop keeps the 5-second liquidity hold responsive.
const LOOP_INTERVAL_SECS: u64 = 2;
/// Tradeable price ceiling (the floor is Gate 1's `min_price`).
const PRICE_MAX: f64 = 1000.0;
/// Minimum closed M1 bars before we bother aggregating the timeframe series.
const MIN_M1_BARS: usize = 3;
/// Drop a per-symbol state that hasn't seen a new bar in this many seconds.
const STATE_STALE_SECS: i64 = 30 * 60;

/// Regular cash session in ET wall-clock minutes since midnight: 09:30–16:00.
const SESSION_START_MIN: u32 = 9 * 60 + 30; // 570
const SESSION_END_MIN:   u32 = 16 * 60;     // 960

// ═══════════════════════════════════════════════════════════════════════════════
//  Pure pipeline — computed once per symbol from its closed timeframe bars. Reused
//  by the engine (to fire) and by the overlay command (to display the same numbers).
// ═══════════════════════════════════════════════════════════════════════════════

/// The display metrics + chart-marker data the overlay shows for a qualifying drive.
#[derive(Debug, Clone)]
pub struct HodDriveEval {
    // Gate 2/overlay levels.
    pub open_price:        f64,
    pub hod:               f64,
    pub lod:               f64,
    pub hod_bar_idx:       usize,
    pub lod_bar_idx:       usize,
    pub retracement:       f64,
    pub risk_ratio:        f64,

    // Gate 3 / overlay.
    /// series_range / (HOD-LOD), 0..1.
    pub series_share:      f64,
    pub series_volume:     u64,
    pub pullback_volume:   u64,
    /// pullback_volume / series_volume (1.0 = equal, 0.5 = half, 2.0 = double).
    pub pullback_vol_ratio: f64,
    pub power_score:       f64,
    pub directional_efficiency: f64,

    /// Indices (into the closed-bar slice) of bars belonging to the green series —
    /// each gets a small cross under it on the chart.
    pub series_bar_idxs:   Vec<usize>,

    /// Did every structural gate (1-3) pass? Gate 4 is the live 5s hold, handled
    /// statefully by the engine and not part of this pure pass.
    pub gates_pass:        bool,
    /// The closed bar this evaluation keys on (anti-spam: one fire per bar).
    pub last_bar_time:     DateTime<Utc>,

    // ── Suggested trade levels (R-based offsets from the pullback bar) ──
    pub suggested_entry:   Option<f64>,
    pub suggested_sl:      Option<f64>,
    pub suggested_tp:      Option<f64>,
    pub suggested_rr:      Option<f64>,
}

/// The longest bullish sequence found off the open.
struct Series {
    start:        usize,
    end:          usize, // inclusive
    high:         f64,
    low:          f64,
    volume:       u64,
    power_score:  f64,
    idxs:         Vec<usize>,
}

/// Run Gates 1-3 on a symbol's CLOSED timeframe bars. `now`, `price`, `open_price`
/// come from the live snapshot. Returns the full evaluation (with `gates_pass`) when
/// there is enough structure, else None (not enough bars / degenerate range).
pub fn evaluate(
    cfg:         &TfConfig,
    bars:        &[Bar],
    price:       f64,
    volume_since_open:        u64,
    dollar_volume_since_open: f64,
) -> Option<HodDriveEval> {
    if bars.len() < cfg.min_closed_bars {
        return None;
    }
    let open_price = bars[0].open;
    let last = bars.last().unwrap();

    // HOD / LOD since open, on CLOSED bars only (Gate 2 rule).
    let mut hod = f64::MIN;
    let mut lod = f64::MAX;
    let mut hod_bar_idx = 0;
    let mut lod_bar_idx = 0;
    for (i, b) in bars.iter().enumerate() {
        if b.high > hod { hod = b.high; hod_bar_idx = i; }
        if b.low  < lod { lod = b.low;  lod_bar_idx = i; }
    }
    let open_range = hod - lod;
    if open_range <= 0.0 {
        return None;
    }

    // ── Gate 1 — Universe ──
    let g1 = price >= cfg.min_price
        && price > open_price
        && (volume_since_open >= cfg.min_volume_since_open
            || dollar_volume_since_open >= cfg.min_dollar_volume_since_open);

    // ── Gate 2 — Risk Ratio (last closed bar) ──
    let retracement = (hod - last.low) / open_range;
    let risk = last.high - last.low;
    let reward = hod - last.high;
    let risk_ratio = if risk > 0.0 { reward / risk } else { 0.0 };
    let g2 = retracement <= cfg.max_retracement && risk_ratio >= cfg.min_risk_ratio;

    // ── Gate 3 — Clear Pattern ──
    let g3_range = open_range >= cfg.min_range_dollars
        || (open_range / open_price) >= cfg.min_range_pct;
    let series = longest_bullish_series(cfg, bars);
    let (series_share, series_volume, power_score, series_idxs, series_end) = match &series {
        Some(s) => {
            let series_range = s.high - s.low;
            (series_range / open_range, s.volume, s.power_score, s.idxs.clone(), s.end)
        }
        None => (0.0, 0, 0.0, Vec::new(), 0),
    };
    let g3 = g3_range
        && series.is_some()
        && series_share >= cfg.min_series_share
        && power_score >= cfg.min_power_score;

    // Pullback = bars after the series end → last closed bar.
    let pullback_volume: u64 = if series.is_some() && series_end + 1 < bars.len() {
        bars[series_end + 1..].iter().map(|b| b.volume).sum()
    } else {
        0
    };
    let pullback_vol_ratio = if series_volume > 0 {
        pullback_volume as f64 / series_volume as f64
    } else {
        0.0
    };

    // Directional efficiency = |close_last - open| / Σ true_range(since open).
    let sum_tr = sum_true_range(bars);
    let directional_efficiency = if sum_tr > 0.0 {
        (last.close - open_price).abs() / sum_tr
    } else {
        0.0
    };

    // Suggested trade levels: R-based offsets from the pullback bar.
    let base_risk = last.high - last.low;
    let (s_entry, s_sl, s_tp, s_rr) = if base_risk > 0.0 {
        let entry = last.high + ENTRY_OFFSET * base_risk;
        let sl    = last.low  - SL_OFFSET * base_risk;
        let tp    = hod       - TP_OFFSET * base_risk;
        let risk  = entry - sl;
        let rr    = if risk > 0.0 && tp > entry { (tp - entry) / risk } else { 0.0 };
        (Some(entry), Some(sl), Some(tp), Some(rr))
    } else {
        (None, None, None, None)
    };

    Some(HodDriveEval {
        open_price,
        hod,
        lod,
        hod_bar_idx,
        lod_bar_idx,
        retracement,
        risk_ratio,
        series_share,
        series_volume,
        pullback_volume,
        pullback_vol_ratio,
        power_score,
        directional_efficiency,
        series_bar_idxs: series_idxs,
        gates_pass: g1 && g2 && g3,
        last_bar_time: last.time,
        suggested_entry: s_entry,
        suggested_sl:    s_sl,
        suggested_tp:    s_tp,
        suggested_rr:    s_rr,
    })
}

/// Sum of true range across all closed bars (Wilder TR; first bar = high−low).
fn sum_true_range(bars: &[Bar]) -> f64 {
    let mut sum = 0.0;
    for i in 0..bars.len() {
        let tr = if i == 0 {
            bars[i].high - bars[i].low
        } else {
            let pc = bars[i - 1].close;
            (bars[i].high - bars[i].low)
                .max((bars[i].high - pc).abs())
                .max((bars[i].low - pc).abs())
        };
        sum += tr;
    }
    sum
}

// ── MACD (12/26/9) on close prices ─────────────────────────────────────────

/// EMA seeded with SMA of the first `period` values.
fn ema(values: &[f64], period: usize) -> Vec<f64> {
    if values.len() < period || period == 0 {
        return vec![];
    }
    let k = 2.0 / (period as f64 + 1.0);
    let sma: f64 = values[..period].iter().sum::<f64>() / period as f64;
    let mut out = Vec::with_capacity(values.len() - period + 1);
    out.push(sma);
    for &v in &values[period..] {
        let prev = *out.last().unwrap();
        out.push(v * k + prev * (1.0 - k));
    }
    out
}

/// MACD status for the overlay: whether the trend is "open" (histogram > 0) or
/// "closed" (exhausted), plus a 0..1 strength normalised against the session's
/// peak histogram magnitude.
pub struct MacdStatus {
    pub open:     bool,
    pub strength: f64,
}

pub fn macd_status(closes: &[f64]) -> Option<MacdStatus> {
    const FAST: usize = 12;
    const SLOW: usize = 26;
    const SIG:  usize = 9;

    if closes.len() < SLOW {
        return None;
    }
    let fast = ema(closes, FAST);
    let slow = ema(closes, SLOW);
    // Align: fast starts at index FAST-1, slow at SLOW-1. The MACD line uses
    // the overlapping tail: the last `slow.len()` elements of fast.
    let offset = fast.len() - slow.len();
    let macd_line: Vec<f64> = fast[offset..]
        .iter()
        .zip(&slow)
        .map(|(f, s)| f - s)
        .collect();
    if macd_line.len() < SIG {
        return None;
    }
    let signal = ema(&macd_line, SIG);
    let hist_offset = macd_line.len() - signal.len();
    let histogram: f64 = *macd_line.last()? - *signal.last()?;
    let max_abs = macd_line[hist_offset..]
        .iter()
        .zip(&signal)
        .map(|(m, s)| (m - s).abs())
        .fold(0.0_f64, f64::max);
    let strength = if max_abs > 0.0 {
        (histogram.abs() / max_abs).min(1.0)
    } else {
        0.0
    };
    Some(MacdStatus {
        open: histogram > 0.0,
        strength,
    })
}

fn is_green(b: &Bar) -> bool {
    b.close >= b.open
}

/// A red bar is tolerated inside a bullish sequence when it is a shallow, controlled
/// dip: small body relative to its range, low holding the previous low, close holding
/// within `red_close_tol` × the previous bar's range of the previous close.
fn is_tolerated_red(cfg: &TfConfig, b: &Bar, prev: &Bar) -> bool {
    if is_green(b) {
        return false;
    }
    let red_body = b.open - b.close;
    let bar_range = b.high - b.low;
    if bar_range <= 0.0 {
        return false;
    }
    red_body / bar_range <= cfg.red_body_max_ratio
        && b.low >= prev.low
        && b.close >= prev.close - cfg.red_close_tol * (prev.high - prev.low)
}

/// Longest bullish sequence off the open: a run of consecutive green bars that may
/// absorb a few tolerated red bars (≤ 1, OR ≤ `red_max_fraction` of the run). Each
/// candidate must START on a green bar. Ties broken by the run's price range.
fn longest_bullish_series(cfg: &TfConfig, bars: &[Bar]) -> Option<Series> {
    let n = bars.len();
    let mut best: Option<Series> = None;

    for start in 0..n {
        if !is_green(&bars[start]) {
            continue;
        }
        let mut red_count = 0usize;
        let mut end = start;
        let mut e = start + 1;
        while e < n {
            let len_after = e - start + 1;
            let ok = if is_green(&bars[e]) {
                true
            } else if is_tolerated_red(cfg, &bars[e], &bars[e - 1]) {
                let next_red = red_count + 1;
                let cap = 1usize.max((cfg.red_max_fraction * len_after as f64).floor() as usize);
                if next_red <= cap {
                    red_count = next_red;
                    true
                } else {
                    false
                }
            } else {
                false
            };
            if !ok {
                break;
            }
            end = e;
            e += 1;
        }

        let high = bars[start..=end].iter().map(|b| b.high).fold(f64::MIN, f64::max);
        let low  = bars[start..=end].iter().map(|b| b.low).fold(f64::MAX, f64::min);
        let range = high - low;
        let len = end - start + 1;
        let better = match &best {
            None => true,
            Some(b) => {
                let blen = b.end - b.start + 1;
                len > blen || (len == blen && range > (b.high - b.low))
            }
        };
        if better {
            let volume = bars[start..=end].iter().map(|b| b.volume).sum();
            // Power score over the sequence: Σ|close-open| / Σ(high-low).
            let body: f64 = bars[start..=end].iter().map(|b| (b.close - b.open).abs()).sum();
            let rng:  f64 = bars[start..=end].iter().map(|b| b.high - b.low).sum();
            let power_score = if rng > 0.0 { body / rng } else { 0.0 };
            best = Some(Series {
                start,
                end,
                high,
                low,
                volume,
                power_score,
                idxs: (start..=end).collect(),
            });
        }
    }
    best
}

/// Aggregate session M1 bars (ascending, bounded to ≥ session open) into CLOSED
/// timeframe bars. Only buckets whose end is at/before `now` are returned. Mirrors
/// the Perfect Pullback aggregation, generalised to any bucket length.
pub fn session_bars(cfg: &TfConfig, m1: &[Bar], now: DateTime<Utc>) -> Vec<Bar> {
    let mut bars: Vec<Bar> = Vec::new();
    for b in m1 {
        let bucket = (b.time.timestamp() / cfg.bucket_secs) * cfg.bucket_secs;
        match bars.last_mut() {
            Some(last) if last.time.timestamp() == bucket => {
                last.high = last.high.max(b.high);
                last.low = last.low.min(b.low);
                last.close = b.close;
                last.volume += b.volume;
                last.trade_count =
                    Some(last.trade_count.unwrap_or(0) + b.trade_count.unwrap_or(0));
            }
            _ => {
                bars.push(Bar {
                    time:        Utc.timestamp_opt(bucket, 0).single().unwrap_or(b.time),
                    open:        b.open,
                    high:        b.high,
                    low:         b.low,
                    close:       b.close,
                    volume:      b.volume,
                    vwap:        None,
                    trade_count: Some(b.trade_count.unwrap_or(0)),
                });
            }
        }
    }
    // Drop the last bucket while it is still forming (end past `now`).
    while let Some(last) = bars.last() {
        if now.timestamp() < last.time.timestamp() + cfg.bucket_secs {
            bars.pop();
        } else {
            break;
        }
    }
    bars
}

// ═══════════════════════════════════════════════════════════════════════════════
//  Engine — per-symbol anti-spam + Gate 4 live-liquidity hold + firing.
// ═══════════════════════════════════════════════════════════════════════════════

/// Per-symbol state read once per loop, then evaluated outside the read lock.
struct TickerInput {
    symbol:         String,
    m1:             Vec<Bar>,
    price:          f64,
    bid:            Option<f64>,
    ask:            Option<f64>,
    spread:         Option<f64>,
    volume_day:     u64,
    change_day_pct: Option<f64>,
}

#[derive(Debug, Clone, Default)]
struct SymbolState {
    /// The bar currently held in the 5-second liquidity confirmation, plus when the
    /// hold started and the day volume snapshot at that moment (to measure the 5s
    /// dollar volume).
    pending_bar:       Option<DateTime<Utc>>,
    pending_since:     Option<DateTime<Utc>>,
    pending_vol_day:   u64,
    /// The bar that last fired — so a given bar fires at most once.
    last_trigger_bar:  Option<DateTime<Utc>>,
    cooldown_until:    Option<DateTime<Utc>>,
    last_seen:         Option<DateTime<Utc>>,
}

pub struct HodDriveEngine;

impl HodDriveEngine {
    /// Spawn the background loop. Returns immediately.
    pub fn start(
        running:          Arc<AtomicBool>,
        market:           Arc<RwLock<MarketState>>,
        active_alerts:    Arc<RwLock<Vec<AlertSignal>>>,
        alert_history:    Arc<RwLock<Vec<AlertSignal>>>,
        strategy_enabled: Arc<RwLock<HashMap<String, bool>>>,
    ) {
        tauri::async_runtime::spawn(async move {
            let mut states: HashMap<String, SymbolState> = HashMap::new();
            let mut replay_gen = crate::replay::clock::generation();

            while running.load(Ordering::Relaxed) {
                {
                    let g = crate::replay::clock::generation();
                    if g != replay_gen {
                        replay_gen = g;
                        states.clear();
                    }
                }
                // Respect the Settings on/off toggle (compiled default if absent).
                let enabled = strategy_enabled
                    .read()
                    .unwrap()
                    .get(STRATEGY_ID)
                    .copied()
                    .unwrap_or(true);
                if !enabled {
                    crate::replay::clock::scaled_sleep(2_000).await;
                    continue;
                }

                // V1: only the 5-minute timeframe is wired.
                let cfg = &CFG_5M;

                let now = crate::time::now();
                // Gate 4 (the 5s live-liquidity hold) needs the real Alpaca feed:
                // spread + last-5s dollar volume only mean something when live quotes/
                // trades are flowing. It runs whenever the Alpaca WebSocket is
                // connected (`live_running`) — including during a Market Replay if the
                // live API is still attached. With no live feed it is SKIPPED entirely
                // (gates 1-3 fire directly).
                let (mock, live_active) = {
                    let ms = market.read().unwrap();
                    (ms.mock_running, ms.live_running)
                };
                let in_session = mock || {
                    let m = crate::time::et_minutes(now);
                    m >= SESSION_START_MIN && m < SESSION_END_MIN
                };
                if !in_session {
                    crate::replay::clock::scaled_sleep(LOOP_INTERVAL_SECS * 1000).await;
                    continue;
                }

                // Today's 09:30 ET cash open (UTC): bounds every series to the current
                // regular session (no prior-day / premarket bar feeds a signal).
                let session_open = crate::time::et_session_open_utc(now);
                let active_until = session_open + ChronoDuration::seconds(cfg.active_secs);
                // Past the active window for every symbol → nothing to do.
                if !mock && now > active_until {
                    crate::replay::clock::scaled_sleep(LOOP_INTERVAL_SECS * 1000).await;
                    continue;
                }

                // Snapshot per-symbol inputs under a brief read lock.
                let inputs: Vec<TickerInput> = {
                    let ms = market.read().unwrap();
                    ms.tickers
                        .values()
                        .filter_map(|t| {
                            let price = t.last_price?;
                            if !(cfg.min_price..=PRICE_MAX).contains(&price) {
                                return None;
                            }
                            let m1 = ms.closed_bars(&t.symbol, Timeframe::M1);
                            if m1.len() < MIN_M1_BARS {
                                return None;
                            }
                            Some(TickerInput {
                                symbol: t.symbol.clone(),
                                m1,
                                price,
                                bid: t.bid,
                                ask: t.ask,
                                spread: t.spread,
                                volume_day: t.volume_day,
                                change_day_pct: t.change_day_pct,
                            })
                        })
                        .collect()
                };

                let mut fires: Vec<AlertSignal> = Vec::new();
                for inp in inputs {
                    let m1: Vec<Bar> =
                        inp.m1.iter().filter(|b| b.time >= session_open).cloned().collect();
                    let bars = session_bars(cfg, &m1, now);
                    if bars.is_empty() {
                        continue;
                    }
                    let st = states.entry(inp.symbol.clone()).or_default();
                    st.last_seen = bars.last().map(|b| b.time);

                    if let Some(fire) =
                        step(cfg, st, &inp, &bars, session_open, active_until, now, mock, live_active)
                    {
                        fires.push(fire);
                    }
                }

                // Prune stale per-symbol state.
                states.retain(|_, s| {
                    s.last_seen
                        .map(|t| (now - t).num_seconds() <= STATE_STALE_SECS)
                        .unwrap_or(false)
                });

                for fire in fires {
                    push_alert(&active_alerts, &alert_history, fire);
                }

                crate::replay::clock::scaled_sleep(LOOP_INTERVAL_SECS * 1000).await;
            }
        });
    }
}

/// One evaluation step for a symbol: active-window gate → Gates 1-3 → the Gate-4
/// 5-second live-liquidity hold → fire. Returns Some(alert) only when the hold
/// completes on a still-qualifying bar. When `live_active` is false (no Alpaca feed),
/// Gate 4 is skipped: gates 1-3 fire directly.
#[allow(clippy::too_many_arguments)]
fn step(
    cfg:          &TfConfig,
    st:           &mut SymbolState,
    inp:          &TickerInput,
    bars:         &[Bar],
    session_open: DateTime<Utc>,
    active_until: DateTime<Utc>,
    now:          DateTime<Utc>,
    mock:         bool,
    live_active:  bool,
) -> Option<AlertSignal> {
    // Active window: ≥ 3 closed bars AND within active_duration since the open.
    if bars.len() < cfg.min_closed_bars {
        return None;
    }
    if !mock && now > active_until {
        return None;
    }
    let _ = session_open;

    // Volume / dollar volume since the open (current forming bar included via the
    // live day total would over-count other sessions, so sum the session M1 → here
    // the aggregated closed bars already exclude premarket).
    let volume_since_open: u64 = bars.iter().map(|b| b.volume).sum();
    // Dollar volume ≈ Σ typical_price × volume over the closed session bars.
    let dollar_volume_since_open: f64 = bars
        .iter()
        .map(|b| ((b.high + b.low + b.close) / 3.0) * b.volume as f64)
        .sum();

    let eval = evaluate(cfg, bars, inp.price, volume_since_open, dollar_volume_since_open)?;
    let bar_time = eval.last_bar_time;

    // One fire per bar; respect the post-fire cooldown.
    if st.last_trigger_bar == Some(bar_time) {
        return None;
    }
    if let Some(until) = st.cooldown_until {
        if now < until {
            return None;
        }
    }

    if !eval.gates_pass {
        // Structure broke before the hold completed → drop any pending hold.
        if st.pending_bar == Some(bar_time) {
            st.pending_bar = None;
            st.pending_since = None;
        }
        return None;
    }

    // ── Gate 4 — live-liquidity confirmation ──
    // Only runs when the Alpaca live feed is connected (spread + last-5s dollar
    // volume need real quotes/trades). Without it, fire on gates 1-3 directly.
    if !live_active {
        st.pending_bar = None;
        st.pending_since = None;
        st.last_trigger_bar = Some(bar_time);
        st.cooldown_until = Some(now + ChronoDuration::seconds(COOLDOWN_SECS as i64));
        return Some(make_alert(
            cfg, &inp.symbol, &eval, inp.price, inp.volume_day, inp.change_day_pct, now,
        ));
    }

    // The 5-second hold (live feed attached).
    match st.pending_bar {
        Some(pb) if pb == bar_time => {
            let held = st
                .pending_since
                .map(|s| (now - s).num_seconds())
                .unwrap_or(0);
            if held < cfg.liquidity_hold_secs {
                return None; // still confirming
            }
            // Spread tightness.
            let spread_ok = match (inp.spread, Some(inp.price)) {
                (Some(spr), Some(px)) if px > 0.0 => (spr / px) <= cfg.max_spread_pct,
                _ => match (inp.bid, inp.ask) {
                    (Some(b), Some(a)) if a > 0.0 => ((a - b) / a) <= cfg.max_spread_pct,
                    _ => true, // unknown spread → don't block
                },
            };
            // Dollar volume traded during the hold (Δ day volume × price). Trade count
            // over 5s isn't available from minute bars in V1, so it's not gated here.
            let dvol_5s =
                inp.volume_day.saturating_sub(st.pending_vol_day) as f64 * inp.price;
            let liquidity_ok = spread_ok
                && (dvol_5s >= cfg.min_dollar_volume_5s || mock);

            st.pending_bar = None;
            st.pending_since = None;
            if !liquidity_ok {
                return None;
            }
            st.last_trigger_bar = Some(bar_time);
            st.cooldown_until = Some(now + ChronoDuration::seconds(COOLDOWN_SECS as i64));
            Some(make_alert(cfg, &inp.symbol, &eval, inp.price, inp.volume_day, inp.change_day_pct, now))
        }
        _ => {
            // Start the 5-second hold for this bar.
            st.pending_bar = Some(bar_time);
            st.pending_since = Some(now);
            st.pending_vol_day = inp.volume_day;
            None
        }
    }
}

/// Build the fire signal for a qualified HOD Drive. The five overlay metrics ride in
/// the reason string so they're visible immediately; the on-chart overlay recomputes
/// them live via the overlay command.
fn make_alert(
    cfg:            &TfConfig,
    symbol:         &str,
    eval:           &HodDriveEval,
    price:          f64,
    volume_day:     u64,
    change_day_pct: Option<f64>,
    now:            DateTime<Utc>,
) -> AlertSignal {
    AlertSignal {
        alert_id:      format!("hod-{}-{}-{}", now.timestamp_millis(), symbol, cfg.label),
        timestamp:     now,
        symbol:        symbol.to_string(),
        strategy_id:   STRATEGY_ID.to_string(),
        strategy_name: "HOD Drive".to_string(),
        priority:      PRIORITY,
        session:       Session::Open,
        price:         Some(price),
        bid:           None,
        ask:           None,
        spread:        None,
        volume:        Some(volume_day),
        rvol:          None,
        change_day_pct,
        float_shares:  None,
        news_today:    false,
        halted:        Some(false),
        latency_ui_ms: None,
        reason: format!(
            "HOD Drive {tf} — série {share:.0}% du range · power {power:.0}% · \
             eff. {eff:.0}% · R/R {rr:.1} · retrace {ret:.0}% · vol PB {pbr:.0}% — ${price:.2}",
            tf    = cfg.label,
            share = eval.series_share * 100.0,
            power = eval.power_score * 100.0,
            eff   = eval.directional_efficiency * 100.0,
            rr    = eval.risk_ratio,
            ret   = eval.retracement * 100.0,
            pbr   = eval.pullback_vol_ratio * 100.0,
        ),
        display_timeframe: Some(cfg.label.to_string()),
        side:          Some(Side::Long),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(i: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_700_000_000 + i * 300, 0).single().unwrap()
    }

    fn bar(i: i64, o: f64, h: f64, l: f64, c: f64, vol: u64) -> Bar {
        Bar { time: t(i), open: o, high: h, low: l, close: c, volume: vol, vwap: Some(c), trade_count: Some(50) }
    }

    /// A clean 4-bar drive off the open with a single shallow pullback bar passes all
    /// three structural gates with R/R ≥ 2 and a deep, body-dominated series.
    #[test]
    fn clean_drive_passes_gates() {
        let bars = vec![
            bar(0, 2.00, 2.20, 1.98, 2.18, 120_000), // green, open=2.00, LOD=1.98
            bar(1, 2.18, 2.55, 2.16, 2.52, 150_000), // green
            bar(2, 2.52, 2.95, 2.50, 2.92, 180_000), // green → HOD=2.95
            bar(3, 2.85, 2.86, 2.80, 2.81, 60_000),  // small shallow pullback (last closed)
        ];
        let vol: u64 = bars.iter().map(|b| b.volume).sum();
        let eval = evaluate(&CFG_5M, &bars, 2.81, vol, 1_000_000.0)
            .expect("enough structure");
        assert!(eval.gates_pass, "clean drive should pass gates 1-3");
        // HOD on bar 2, LOD on bar 0.
        assert_eq!(eval.hod_bar_idx, 2);
        assert_eq!(eval.lod_bar_idx, 0);
        // The last bar is a compact pullback well below the HOD: reward (HOD−low) is
        // several times its own risk (high−low), so the R/R toward the HOD clears 2.
        assert!(eval.risk_ratio >= 2.0);
        // The green series carries most of the open range.
        assert!(eval.series_share >= 0.5);
        assert!(eval.power_score >= 0.6);
        assert!(!eval.series_bar_idxs.is_empty());
    }

    /// A choppy series (alternating big-bodied red/green, no clean run) fails Gate 3.
    #[test]
    fn choppy_series_fails() {
        let bars = vec![
            bar(0, 2.00, 2.30, 1.95, 2.05, 100_000),
            bar(1, 2.05, 2.10, 1.80, 1.85, 100_000), // big red
            bar(2, 1.85, 2.25, 1.83, 2.20, 100_000),
            bar(3, 2.20, 2.22, 1.90, 1.95, 100_000), // big red (last)
        ];
        let vol: u64 = bars.iter().map(|b| b.volume).sum();
        let eval = evaluate(&CFG_5M, &bars, 1.95, vol, 1_000_000.0).unwrap();
        assert!(!eval.gates_pass, "choppy structure must not pass");
    }

    #[test]
    fn tolerated_red_extends_series() {
        // green, green, small controlled red, green → one sequence of length 4.
        let bars = vec![
            bar(0, 2.00, 2.20, 1.99, 2.18, 100_000),
            bar(1, 2.18, 2.45, 2.17, 2.43, 100_000),
            bar(2, 2.43, 2.46, 2.41, 2.42, 40_000), // tiny red, low holds, close holds
            bar(3, 2.42, 2.70, 2.41, 2.68, 100_000),
        ];
        let s = longest_bullish_series(&CFG_5M, &bars).expect("a sequence");
        assert_eq!(s.start, 0);
        assert_eq!(s.end, 3);
        assert_eq!(s.idxs, vec![0, 1, 2, 3]);
    }
}
