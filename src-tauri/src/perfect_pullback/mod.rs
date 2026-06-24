// Perfect Pullback — stateful 5-minute pullback-continuation engine.
//
// Once a ticker is WATCHED (selected by the Market Attention Gate — see
// `crate::market_attention` — and memorised for the session), this engine runs a
// four-gate pipeline on its 5-minute aggregates to fire a clean pullback-continuation
// signal. The gates are evaluated every loop (≤ once per 1-minute close in practice),
// but every structural decision is built on CLOSED 5-minute bars:
//
//   Gate 1 — Direction : a clean directional regime (price vs session VWAP, EMA9 vs
//            EMA20, EMA20 slope, Kaufman efficiency, few VWAP crosses, low chop). Picks
//            the side (long/short); choppy names are rejected here.
//   Gate 2 — Impulse   : a real impulse leg exists — last swing pivot → extreme move
//            ≥ 1.5 × ATR, impulse volume above the recent average, HH/HL intact.
//   Gate 3 — Pullback  : a healthy breather — retracement 20–55 % (vertical) or a very
//            compressed shallow drift (time-based), volume below the impulse, ranges
//            compressing, price holding above EMA20/VWAP, the higher low unbroken, no
//            violent counter bar (> 1 ATR).
//   Gate 4 — Trigger   : fire when the pullback has just closed its 3rd counter-trend
//            5-minute bar AND that bar is small (true range ≤ 0.6 × ATR).
//
// Anti-spam: after a trigger a 10-minute cooldown applies, lifted early only by a new
// significant extreme (a fresh impulse beyond the last one by ≥ 1 ATR) or a side flip
// (structure reset). Each 5-minute bar can trigger at most once.
//
// Scope: 5-minute timeframe only for now (other timeframes will be added later).
//
// Why an engine and not a `ScanStrategy::should_alert`: the gates need the recent
// 5-minute structure (pivots, impulse leg, multi-bar pullback) plus per-symbol
// anti-spam state, which can't fit the stateless per-tick contract. So this engine
// runs in its own tokio task and pushes AlertSignals straight into the active-alert
// list via `scanner::push_alert`. The registry still carries a metadata
// `PerfectPullback` strategy (card, toggle, name, priority) — see
// `strategies::perfect_pullback`.
//
// Bars: during the regular session Alpaca streams 1-minute bars for the whole
// universe (MarketState::on_bar → M1 ring); we read those closed M1 bars and build the
// 5-minute series (and reconstruct the cumulative session VWAP per bar) ourselves.

use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex, RwLock,
};

use chrono::{DateTime, Duration as ChronoDuration, TimeZone, Utc};
use tokio::time::Duration;

use crate::local_db::universe_repository;
use crate::market_state::aggregators::{Bar, Timeframe};
use crate::market_state::MarketState;
use crate::scanner::push_alert;
use crate::strategies::perfect_pullback::ID as STRATEGY_ID;
use crate::types::{AlertSignal, AttentionEntry, Session, Side};

// ─── Tunable parameters (recompile to apply) ──────────────────────────────────
/// Anti-spam cooldown after a trigger (seconds): no second alert for the same symbol
/// for 10 minutes unless a new significant extreme / side flip re-arms it.
pub const COOLDOWN_SECS: u64 = 600;
/// How often the engine evaluates the gates (seconds). The structural decisions only
/// change on a new closed 5-minute bar, so a short loop just keeps it responsive.
const LOOP_INTERVAL_SECS: u64 = 2;
/// How often the per-symbol average-daily-volume map is reloaded from the universe
/// table (seconds). Only feeds the DISPLAYED relative volume.
const AVG_VOL_REFRESH_SECS: u64 = 300;

/// Tradeable price band (USD). Keeps the engine off sub-penny noise and ultra-highs.
const PRICE_MIN: f64 = 1.0;
const PRICE_MAX: f64 = 1000.0;
/// Minimum closed M1 bars a watched ticker needs before we bother building 5m bars.
const MIN_M1_BARS: usize = 5;

/// 5-minute bucket (the only timeframe for now) and its display label.
const BUCKET_SECS: i64 = 300;
const TF_LABEL: &str = "5m";

// ── Indicators ────────────────────────────────────────────────────────────────
/// Minimum closed 5m bars before any gate is evaluated.
const MIN_BARS_5M: usize = 6;
const EMA_FAST: usize = 9;
const EMA_SLOW: usize = 20;
const ATR_PERIOD: usize = 14;
/// EMA20 slope is measured as the last value minus this many bars back.
const SLOPE_LOOKBACK: usize = 1;
/// Hard floor: ignore ultra-quiet names whose 5m ATR is below this (dollars).
const MIN_ATR: f64 = 0.10;

// ── Gate 1 — Direction ─────────────────────────────────────────────────────────
/// Kaufman efficiency-ratio window (bars, ≈ 15 min) and its minimum value. Measured
/// over the last 3 bars — responsive enough to confirm direction without needing a
/// long clean run.
const ER_WINDOW: usize = 3;
const MIN_EFFICIENCY: f64 = 0.35;
/// Max VWAP crossings allowed in the recent window (too many = chop).
const VWAP_CROSS_WINDOW: usize = 6;
const MAX_VWAP_CROSSES: usize = 2;
/// Max candle-colour alternations in the recent window (too many = chop).
const CHOP_WINDOW: usize = 6;
const MAX_ALTERNATIONS: usize = 3;

// ── Gate 2 — Impulse ──────────────────────────────────────────────────────────
/// Impulse leg (pivot → extreme) must move at least this multiple of ATR.
const IMPULSE_ATR_MULT: f64 = 1.5;
/// Swing-pivot fractal span (bars on each side). 1 = a 3-bar fractal.
const PIVOT_SPAN: usize = 1;
/// Window for the "recent average volume" the impulse must beat.
const RECENT_VOL_WINDOW: usize = 12;

// ── Gate 3 — Pullback ─────────────────────────────────────────────────────────
/// Vertical pullback retracement band (fraction of the impulse).
const MIN_RETRACE: f64 = 0.20;
const MAX_RETRACE: f64 = 0.55;
/// Time-based (shallow) pullback only qualifies if its bars are this compressed
/// relative to the impulse's bars.
const COMPRESSION_MULT: f64 = 0.70;
/// A counter-trend pullback bar wider than this multiple of ATR is "violent" → reject.
const VIOLENT_ATR_MULT: f64 = 1.0;

// ── Gate 4 — Trigger ──────────────────────────────────────────────────────────
/// Number of counter-trend pullback bars required before the trigger can fire.
const TRIGGER_PB_BARS: usize = 3;
/// The triggering (3rd+) bar must be "small": its true range ≤ this multiple of ATR.
const TRIGGER_SMALL_ATR_MULT: f64 = 0.6;

// ── Anti-spam ─────────────────────────────────────────────────────────────────
/// During cooldown, a re-trigger is allowed if a NEW impulse extreme forms beyond the
/// last trigger's extreme by at least this multiple of ATR (or the side flips).
const NEW_EXTREME_ATR_MULT: f64 = 1.0;

/// Regular cash session in ET wall-clock minutes since midnight: 09:30–16:00. Perfect
/// Pullback watches and fires across the whole session; Market Attention only feeds
/// NEW candidate names during its own 09:30–12:30 window, but a memorised ticker
/// stays tradeable here until 16:00.
const SESSION_START_MIN: u32 = 9 * 60 + 30; // 570
const SESSION_END_MIN:   u32 = 16 * 60;     // 960

/// Drop a per-symbol state that hasn't seen a new bar in this many seconds.
const STATE_STALE_SECS: i64 = 30 * 60;

/// Per-symbol snapshot read from MarketState once per loop, then evaluated outside the
/// read lock.
struct TickerInput {
    symbol:         String,
    /// Closed 1-minute bars (oldest → newest); the 5-minute series is built from these.
    m1:             Vec<Bar>,
    price:          f64,
    volume_day:     u64,
    change_day_pct: Option<f64>,
    avg_vol:        u64,
}

// ─── Per-symbol anti-spam state ────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
struct SymbolState {
    cooldown_until:   Option<DateTime<Utc>>,
    /// The 5m bar that last triggered — so a given bar fires at most once.
    last_trigger_bar: Option<DateTime<Utc>>,
    /// The impulse extreme at the last trigger (re-arm reference).
    armed_extreme:    Option<f64>,
    armed_side:       Option<Side>,
    /// Last 5m bar seen for this symbol (memory-prune key).
    last_seen:        Option<DateTime<Utc>>,
}

/// A qualified impulse leg: the swing pivot and the impulse extreme it ran to.
struct Impulse {
    pivot_idx: usize,
    ext_idx:   usize,
    pivot_px:  f64, // pivot low (long) / pivot high (short)
    ext_px:    f64, // impulse high (long) / impulse low (short)
}

// ─── Engine ───────────────────────────────────────────────────────────────────

pub struct PerfectPullbackEngine;

impl PerfectPullbackEngine {
    /// Spawn the background loop. Returns immediately.
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        running:          Arc<AtomicBool>,
        market:           Arc<RwLock<MarketState>>,
        db:               Arc<Mutex<rusqlite::Connection>>,
        active_alerts:    Arc<RwLock<Vec<AlertSignal>>>,
        alert_history:    Arc<RwLock<Vec<AlertSignal>>>,
        strategy_enabled: Arc<RwLock<HashMap<String, bool>>>,
        attention:        Arc<RwLock<Vec<AttentionEntry>>>,
    ) {
        // Use the Tauri-managed runtime so this can be launched from the sync
        // `setup` hook (a bare `tokio::spawn` there panics: no reactor running).
        tauri::async_runtime::spawn(async move {
            // Per-symbol anti-spam state.
            let mut states: HashMap<String, SymbolState> = HashMap::new();
            // Per-symbol average daily volume (feeds the displayed relative volume),
            // refreshed periodically from the universe table.
            let mut avg_volumes = load_avg_volumes(&db);
            let mut avg_vol_loaded = std::time::Instant::now();
            // Memorised candidate set: every symbol Market Attention has selected
            // today (symbol → first time seen). A ticker stays here for the whole
            // session even after it leaves the attention list, so a pullback whose
            // falling volume drops it off the list never loses the candidate.
            let mut watched: HashMap<String, DateTime<Utc>> = HashMap::new();
            let mut watched_day: Option<String> = None;
            // Market Replay reset watch: replay start / backward seek / new day →
            // drop per-symbol state and the memorised candidate set.
            let mut replay_gen = crate::replay::clock::generation();

            while running.load(Ordering::Relaxed) {
                {
                    let g = crate::replay::clock::generation();
                    if g != replay_gen {
                        replay_gen = g;
                        states.clear();
                        watched.clear();
                        watched_day = None;
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

                if avg_vol_loaded.elapsed() >= Duration::from_secs(AVG_VOL_REFRESH_SECS) {
                    avg_volumes = load_avg_volumes(&db);
                    avg_vol_loaded = std::time::Instant::now();
                }

                // App clock: simulated instant during a Market Replay.
                let now = crate::time::now();

                // New trading day: forget yesterday's memorised candidates.
                let today = et_date(now);
                if watched_day.as_deref() != Some(today.as_str()) {
                    watched.clear();
                    watched_day = Some(today);
                }

                let mock = market.read().unwrap().mock_running;
                let in_session = mock
                    || {
                        let m = et_minutes(now);
                        m >= SESSION_START_MIN && m < SESSION_END_MIN
                    };
                if !in_session {
                    crate::replay::clock::scaled_sleep(LOOP_INTERVAL_SECS * 1000).await;
                    continue;
                }

                // Pull in the latest Market Attention selection and memorise it.
                {
                    let list = attention.read().unwrap();
                    for e in list.iter() {
                        watched.entry(e.symbol.clone()).or_insert(now);
                    }
                }

                // Snapshot the per-symbol inputs under a brief read lock; all gate
                // logic runs outside the lock. Pre-filter to active candidates: price
                // in band, some M1 history, and a Market-Attention-memorised symbol.
                let inputs: Vec<TickerInput> = {
                    let ms = market.read().unwrap();
                    ms.tickers
                        .values()
                        .filter_map(|t| {
                            let price = t.last_price?;
                            if !(PRICE_MIN..=PRICE_MAX).contains(&price) {
                                return None;
                            }
                            if !watched.contains_key(&t.symbol) {
                                return None;
                            }
                            let m1 = ms.closed_bars(&t.symbol, Timeframe::M1);
                            if m1.len() < MIN_M1_BARS {
                                return None;
                            }
                            // 0 = unknown → no rvol shown; not a filter.
                            let avg_vol = avg_volumes.get(&t.symbol).copied().unwrap_or(0);
                            Some(TickerInput {
                                symbol: t.symbol.clone(),
                                m1,
                                price,
                                volume_day: t.volume_day,
                                change_day_pct: t.change_day_pct,
                                avg_vol,
                            })
                        })
                        .collect()
                };

                // Today's 09:30 ET cash open (UTC): bounds the detector to the current
                // regular session, so a freshly-watched ticker is seeded from this
                // morning's bars (the impulse is replayed and a fast pullback is caught)
                // while no prior day / premarket bar ever feeds a signal.
                let session_open = crate::time::et_session_open_utc(now);

                let mut fires: Vec<AlertSignal> = Vec::new();
                for inp in inputs {
                    let m1: Vec<Bar> =
                        inp.m1.iter().filter(|b| b.time >= session_open).cloned().collect();
                    let (bars, vwaps) = session_5m(&m1, now);
                    let st = states.entry(inp.symbol.clone()).or_default();
                    if let Some(fire) = evaluate(st, &inp, &bars, &vwaps, now) {
                        fires.push(fire);
                    }
                }

                // Prune stale per-symbol state to bound memory.
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

/// Run the four-gate pipeline for one symbol on its closed 5m series. Returns a fire
/// signal when a fresh pullback trigger passes every gate (and anti-spam allows it).
fn evaluate(
    st:    &mut SymbolState,
    inp:   &TickerInput,
    bars:  &[Bar],
    vwaps: &[f64],
    now:   DateTime<Utc>,
) -> Option<AlertSignal> {
    st.last_seen = bars.last().map(|b| b.time);
    if bars.len() < MIN_BARS_5M {
        return None;
    }
    let atr = atr(bars, ATR_PERIOD);
    if atr < MIN_ATR {
        return None;
    }
    let closes: Vec<f64> = bars.iter().map(|b| b.close).collect();
    let ema9 = ema_series(&closes, EMA_FAST);
    let ema20 = ema_series(&closes, EMA_SLOW);
    let vwap_now = *vwaps.last().unwrap_or(&inp.price);
    let price = inp.price;

    // Gate 1 — Direction (also picks the side; rejects chop).
    let side = direction_side(bars, &ema9, &ema20, vwaps, price, vwap_now)?;
    // Gate 2 — Impulse.
    let imp = impulse_gate(side, bars, atr)?;
    // Gate 3 — Pullback (returns the retracement fraction).
    let retrace = pullback_gate(side, bars, *ema20.last().unwrap(), vwap_now, price, &imp, atr)?;
    // Gate 4 — Trigger.
    if !trigger_ready(side, bars, &imp, atr) {
        return None;
    }

    // One fire per 5m bar.
    let trigger_bar_time = bars.last().unwrap().time;
    if st.last_trigger_bar == Some(trigger_bar_time) {
        return None;
    }

    // Anti-spam: during cooldown, only a new significant extreme or a side flip
    // (structure reset) re-arms the trigger.
    if let Some(until) = st.cooldown_until {
        if now < until {
            let new_extreme = match (side, st.armed_extreme) {
                (Side::Long, Some(e))  => imp.ext_px > e + NEW_EXTREME_ATR_MULT * atr,
                (Side::Short, Some(e)) => imp.ext_px < e - NEW_EXTREME_ATR_MULT * atr,
                _ => true,
            };
            let flip = st.armed_side.map_or(true, |s| s != side);
            if !(new_extreme || flip) {
                return None;
            }
        }
    }

    st.cooldown_until   = Some(now + ChronoDuration::seconds(COOLDOWN_SECS as i64));
    st.last_trigger_bar = Some(trigger_bar_time);
    st.armed_extreme    = Some(imp.ext_px);
    st.armed_side       = Some(side);

    let rvol = (inp.avg_vol > 0).then(|| inp.volume_day as f64 / inp.avg_vol as f64);
    let move_dollar = (imp.ext_px - imp.pivot_px).abs();
    Some(make_alert(
        &inp.symbol, side, retrace, move_dollar, atr, price,
        inp.volume_day, inp.change_day_pct, rvol, now,
    ))
}

// ─── Gate 1 — Direction ─────────────────────────────────────────────────────────

/// A clean directional regime → the side to trade, or None (no direction / choppy).
fn direction_side(
    bars:     &[Bar],
    ema9:     &[f64],
    ema20:    &[f64],
    vwaps:    &[f64],
    price:    f64,
    vwap_now: f64,
) -> Option<Side> {
    let n = bars.len();
    if n < MIN_BARS_5M || n < SLOPE_LOOKBACK + 1 {
        return None;
    }
    let closes: Vec<f64> = bars.iter().map(|b| b.close).collect();

    // Reject choppy regimes (net move weak vs path, VWAP whipsaws, colour churn).
    if efficiency_ratio(&closes, ER_WINDOW) < MIN_EFFICIENCY {
        return None;
    }
    if vwap_crosses(&closes, vwaps, VWAP_CROSS_WINDOW) > MAX_VWAP_CROSSES {
        return None;
    }
    if color_alternations(bars, CHOP_WINDOW) > MAX_ALTERNATIONS {
        return None;
    }

    let e9 = *ema9.last().unwrap();
    let e20 = *ema20.last().unwrap();
    let slope = e20 - ema20[n - 1 - SLOPE_LOOKBACK];

    if price > vwap_now && e9 > e20 && slope > 0.0 {
        return Some(Side::Long);
    }
    if price < vwap_now && e9 < e20 && slope < 0.0 {
        return Some(Side::Short);
    }
    None
}

// ─── Gate 2 — Impulse ──────────────────────────────────────────────────────────

/// A real impulse leg in the trend direction: last swing pivot → extreme move ≥
/// 1.5 ATR, impulse volume above the recent average, higher-low (long) / lower-high
/// (short) structure intact.
fn impulse_gate(side: Side, bars: &[Bar], atr: f64) -> Option<Impulse> {
    match side {
        Side::Long => {
            let pivot_idx = last_swing_low(bars, PIVOT_SPAN)?;
            let pivot_px = bars[pivot_idx].low;
            // Impulse extreme = highest high from the pivot onward.
            let (ext_idx, ext_px) = bars[pivot_idx..]
                .iter()
                .enumerate()
                .fold((pivot_idx, f64::MIN), |acc, (i, b)| {
                    if b.high > acc.1 { (pivot_idx + i, b.high) } else { acc }
                });
            if ext_idx <= pivot_idx || (ext_px - pivot_px) < IMPULSE_ATR_MULT * atr {
                return None;
            }
            // Volume of the impulse leg must beat the recent average.
            if mean_vol(&bars[pivot_idx..=ext_idx]) <= recent_mean_vol(bars, RECENT_VOL_WINDOW) {
                return None;
            }
            // Higher-low structure: this pivot ≥ the prior swing low.
            if let Some(prev) = last_swing_low(&bars[..pivot_idx], PIVOT_SPAN) {
                if bars[pivot_idx].low < bars[prev].low {
                    return None;
                }
            }
            Some(Impulse { pivot_idx, ext_idx, pivot_px, ext_px })
        }
        Side::Short => {
            let pivot_idx = last_swing_high(bars, PIVOT_SPAN)?;
            let pivot_px = bars[pivot_idx].high;
            // Impulse extreme = lowest low from the pivot onward.
            let (ext_idx, ext_px) = bars[pivot_idx..]
                .iter()
                .enumerate()
                .fold((pivot_idx, f64::MAX), |acc, (i, b)| {
                    if b.low < acc.1 { (pivot_idx + i, b.low) } else { acc }
                });
            if ext_idx <= pivot_idx || (pivot_px - ext_px) < IMPULSE_ATR_MULT * atr {
                return None;
            }
            if mean_vol(&bars[pivot_idx..=ext_idx]) <= recent_mean_vol(bars, RECENT_VOL_WINDOW) {
                return None;
            }
            // Lower-high structure: this pivot ≤ the prior swing high.
            if let Some(prev) = last_swing_high(&bars[..pivot_idx], PIVOT_SPAN) {
                if bars[pivot_idx].high > bars[prev].high {
                    return None;
                }
            }
            Some(Impulse { pivot_idx, ext_idx, pivot_px, ext_px })
        }
    }
}

// ─── Gate 3 — Pullback ─────────────────────────────────────────────────────────

/// A healthy breather after the impulse. Returns the retracement fraction when the
/// pullback qualifies (vertical 20–55 % OR a very compressed shallow time-based
/// drift), with falling volume, compressing ranges, the higher low unbroken, price
/// holding above EMA20/VWAP and no violent counter bar.
fn pullback_gate(
    side:     Side,
    bars:     &[Bar],
    ema_slow: f64,
    vwap_now: f64,
    price:    f64,
    imp:      &Impulse,
    atr:      f64,
) -> Option<f64> {
    let n = bars.len();
    if imp.ext_idx + 1 >= n {
        return None; // no pullback bar yet (the extreme is the last bar)
    }
    let pb = &bars[imp.ext_idx + 1..];
    let imp_leg = &bars[imp.pivot_idx..=imp.ext_idx];
    let amplitude = (imp.ext_px - imp.pivot_px).abs();
    if amplitude <= 0.0 {
        return None;
    }

    // Retracement from the impulse extreme.
    let retrace = match side {
        Side::Long => {
            let pb_low = pb.iter().map(|b| b.low).fold(f64::MAX, f64::min);
            (imp.ext_px - pb_low) / amplitude
        }
        Side::Short => {
            let pb_high = pb.iter().map(|b| b.high).fold(f64::MIN, f64::max);
            (pb_high - imp.ext_px) / amplitude
        }
    };
    if retrace > MAX_RETRACE {
        return None; // too deep — not a continuation breather
    }

    // Volume must fall vs the impulse.
    if mean_vol(pb) >= mean_vol(imp_leg) {
        return None;
    }
    // No violent counter bar.
    if pb.iter().any(|b| (b.high - b.low) > VIOLENT_ATR_MULT * atr) {
        return None;
    }
    // Higher-low / lower-high not broken on a close.
    match side {
        Side::Long  => if pb.iter().any(|b| b.close < imp.pivot_px) { return None; },
        Side::Short => if pb.iter().any(|b| b.close > imp.pivot_px) { return None; },
    }
    // Price still holding above EMA20 or VWAP (long) / below (short).
    match side {
        Side::Long  => if !(price > ema_slow || price > vwap_now) { return None; },
        Side::Short => if !(price < ema_slow || price < vwap_now) { return None; },
    }
    // Ranges compressing / slowing.
    let imp_range = mean_range(imp_leg);
    let pb_range = mean_range(pb);
    if pb_range >= imp_range {
        return None;
    }
    // Breathing type: vertical retracement in band, OR a shallow time-based drift
    // that is strongly compressed (price stalling near the high).
    let vertical = (MIN_RETRACE..=MAX_RETRACE).contains(&retrace);
    let time_based = retrace < MIN_RETRACE && pb_range <= COMPRESSION_MULT * imp_range;
    if !(vertical || time_based) {
        return None;
    }
    Some(retrace)
}

// ─── Gate 4 — Trigger ──────────────────────────────────────────────────────────

/// The pullback has just closed its 3rd (or later) counter-trend bar and that bar is
/// small (true range ≤ TRIGGER_SMALL_ATR_MULT × ATR).
fn trigger_ready(side: Side, bars: &[Bar], imp: &Impulse, atr: f64) -> bool {
    let n = bars.len();
    if imp.ext_idx + 1 >= n {
        return false;
    }
    let pb = &bars[imp.ext_idx + 1..];
    let counter = |b: &Bar| match side {
        Side::Long => b.close <= b.open,
        Side::Short => b.close >= b.open,
    };
    if pb.iter().filter(|b| counter(b)).count() < TRIGGER_PB_BARS {
        return false;
    }
    let last = bars.last().unwrap();
    if !counter(last) {
        return false;
    }
    (last.high - last.low) <= TRIGGER_SMALL_ATR_MULT * atr
}

// ─── Indicator helpers (pure) ──────────────────────────────────────────────────

/// EMA series (one value per input) using 2/(period+1) smoothing, seeded with the
/// first value. Empty when inputs are empty.
fn ema_series(values: &[f64], period: usize) -> Vec<f64> {
    if values.is_empty() || period == 0 {
        return Vec::new();
    }
    let k = 2.0 / (period as f64 + 1.0);
    let mut out = Vec::with_capacity(values.len());
    let mut e = values[0];
    out.push(e);
    for &v in &values[1..] {
        e = v * k + e * (1.0 - k);
        out.push(e);
    }
    out
}

/// Average true range over the last `period` bars (Wilder true range; first bar uses
/// high−low). 0 when empty.
fn atr(bars: &[Bar], period: usize) -> f64 {
    if bars.is_empty() {
        return 0.0;
    }
    let mut trs = Vec::with_capacity(bars.len());
    for i in 0..bars.len() {
        let tr = if i == 0 {
            bars[i].high - bars[i].low
        } else {
            let pc = bars[i - 1].close;
            (bars[i].high - bars[i].low)
                .max((bars[i].high - pc).abs())
                .max((bars[i].low - pc).abs())
        };
        trs.push(tr);
    }
    let take = period.min(trs.len());
    if take == 0 {
        return 0.0;
    }
    trs[trs.len() - take..].iter().sum::<f64>() / take as f64
}

/// Kaufman efficiency ratio over the last `window` steps: |net change| / Σ|step|.
/// 1.0 = perfectly directional, ~0 = choppy. 0 when flat / too little data.
fn efficiency_ratio(closes: &[f64], window: usize) -> f64 {
    let n = closes.len();
    if n < 2 {
        return 0.0;
    }
    let w = window.min(n - 1);
    let slice = &closes[n - w - 1..];
    let net = (slice[slice.len() - 1] - slice[0]).abs();
    let path: f64 = slice.windows(2).map(|p| (p[1] - p[0]).abs()).sum();
    if path <= 0.0 { 0.0 } else { net / path }
}

/// Number of times the close crossed its (cumulative session) VWAP in the last
/// `window` steps.
fn vwap_crosses(closes: &[f64], vwaps: &[f64], window: usize) -> usize {
    let n = closes.len().min(vwaps.len());
    if n < 2 {
        return 0;
    }
    let start = n.saturating_sub(window + 1);
    let mut crosses = 0;
    let mut prev = closes[start] - vwaps[start];
    for i in start + 1..n {
        let d = closes[i] - vwaps[i];
        if d != 0.0 {
            if prev != 0.0 && (d > 0.0) != (prev > 0.0) {
                crosses += 1;
            }
            prev = d;
        }
    }
    crosses
}

/// Number of candle-colour changes (green↔red) over the last `window` bars.
fn color_alternations(bars: &[Bar], window: usize) -> usize {
    let n = bars.len();
    if n < 2 {
        return 0;
    }
    let start = n.saturating_sub(window);
    let green = |b: &Bar| b.close >= b.open;
    let mut alt = 0;
    for i in start + 1..n {
        if green(&bars[i]) != green(&bars[i - 1]) {
            alt += 1;
        }
    }
    alt
}

/// Index of the most recent confirmed swing low (low ≤ the lows of `span` bars on
/// each side). None when none is confirmed.
fn last_swing_low(bars: &[Bar], span: usize) -> Option<usize> {
    if bars.len() < 2 * span + 1 {
        return None;
    }
    for i in (span..bars.len() - span).rev() {
        let lo = bars[i].low;
        if (1..=span).all(|d| bars[i - d].low >= lo && bars[i + d].low >= lo) {
            return Some(i);
        }
    }
    None
}

/// Index of the most recent confirmed swing high. None when none is confirmed.
fn last_swing_high(bars: &[Bar], span: usize) -> Option<usize> {
    if bars.len() < 2 * span + 1 {
        return None;
    }
    for i in (span..bars.len() - span).rev() {
        let hi = bars[i].high;
        if (1..=span).all(|d| bars[i - d].high <= hi && bars[i + d].high <= hi) {
            return Some(i);
        }
    }
    None
}

fn mean_vol(bars: &[Bar]) -> f64 {
    if bars.is_empty() {
        return 0.0;
    }
    bars.iter().map(|b| b.volume as f64).sum::<f64>() / bars.len() as f64
}

fn recent_mean_vol(bars: &[Bar], window: usize) -> f64 {
    let start = bars.len().saturating_sub(window);
    mean_vol(&bars[start..])
}

fn mean_range(bars: &[Bar]) -> f64 {
    if bars.is_empty() {
        return 0.0;
    }
    bars.iter().map(|b| b.high - b.low).sum::<f64>() / bars.len() as f64
}

/// Aggregate session M1 bars (ascending, already bounded to ≥ session open) into
/// closed 5-minute bars, plus the cumulative session VWAP at each 5m bar's close.
/// The VWAP is reconstructed by accumulating every M1 bar's (vwap × volume) — Alpaca's
/// per-minute `vw` — across the session, matching the broker's session VWAP. Only
/// 5-minute buckets whose end is at/before `now` are returned (closed bars).
fn session_5m(m1: &[Bar], now: DateTime<Utc>) -> (Vec<Bar>, Vec<f64>) {
    let mut bars: Vec<Bar> = Vec::new();
    let mut vwaps: Vec<f64> = Vec::new();
    let mut pv = 0.0_f64; // Σ price × volume over the session so far
    let mut vol = 0.0_f64;
    for b in m1 {
        let price = b.vwap.unwrap_or((b.high + b.low + b.close) / 3.0);
        pv += price * b.volume as f64;
        vol += b.volume as f64;
        let cum_vwap = if vol > 0.0 { pv / vol } else { price };
        let bucket = (b.time.timestamp() / BUCKET_SECS) * BUCKET_SECS;
        match bars.last_mut() {
            Some(last) if last.time.timestamp() == bucket => {
                last.high = last.high.max(b.high);
                last.low = last.low.min(b.low);
                last.close = b.close;
                last.volume += b.volume;
                last.trade_count = Some(last.trade_count.unwrap_or(0) + b.trade_count.unwrap_or(0));
                *vwaps.last_mut().unwrap() = cum_vwap;
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
                vwaps.push(cum_vwap);
            }
        }
    }
    // Drop the last bucket while it is still forming (end past `now`).
    while let Some(last) = bars.last() {
        if now.timestamp() < last.time.timestamp() + BUCKET_SECS {
            bars.pop();
            vwaps.pop();
        } else {
            break;
        }
    }
    (bars, vwaps)
}

/// Build the fire signal for a completed pullback trigger.
#[allow(clippy::too_many_arguments)]
fn make_alert(
    symbol:         &str,
    side:           Side,
    retrace:        f64,
    move_dollar:    f64,
    atr:            f64,
    price:          f64,
    volume_day:     u64,
    change_day_pct: Option<f64>,
    rvol:           Option<f64>,
    now:            DateTime<Utc>,
) -> AlertSignal {
    let side_str = if matches!(side, Side::Long) { "Long" } else { "Short" };
    let mult = if atr > 0.0 { move_dollar / atr } else { 0.0 };
    let rvol_str = rvol.map(|r| format!(" RVOL ×{r:.1},")).unwrap_or_default();
    AlertSignal {
        alert_id:      format!("pp-{}-{}-{}", now.timestamp_millis(), symbol, TF_LABEL),
        timestamp:     now,
        symbol:        symbol.to_string(),
        strategy_id:   STRATEGY_ID.to_string(),
        strategy_name: "Perfect Pullback".to_string(),
        priority:      2,
        session:       Session::Open,
        price:         Some(price),
        bid:           None,
        ask:           None,
        spread:        None,
        volume:        Some(volume_day),
        rvol,
        change_day_pct,
        float_shares:  None,
        news_today:    false,
        halted:        Some(false),
        latency_ui_ms: None,
        reason: format!(
            "Perfect Pullback {side_str} {TF_LABEL} — impulsion {move_dollar:.2}$ \
             ({mult:.1}×ATR),{rvol_str} pullback {:.0}% (3e bougie compacte) — ${price:.2}",
            retrace * 100.0,
        ),
        display_timeframe: Some(TF_LABEL.to_string()),
        side:          Some(side),
    }
}

// ─── Data loading / time helpers ───────────────────────────────────────────────

/// Per-symbol average daily volume from the universe table (relative-volume base).
/// Symbols with no known average are omitted.
fn load_avg_volumes(db: &Arc<Mutex<rusqlite::Connection>>) -> HashMap<String, u64> {
    let conn = db.lock().unwrap();
    universe_repository::get_all(&conn)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|a| a.avg_volume.filter(|v| *v > 0).map(|v| (a.symbol, v as u64)))
        .collect()
}

/// ET wall-clock minutes since midnight (DST-aware — see `crate::time`).
fn et_minutes(now: DateTime<Utc>) -> u32 {
    crate::time::et_minutes(now)
}

/// ET calendar date (YYYY-MM-DD) — the key the memorised candidate set is built for.
fn et_date(now: DateTime<Utc>) -> String {
    crate::time::et_date(now)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(i: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_700_000_000 + i * BUCKET_SECS, 0).single().unwrap()
    }

    fn bar(i: i64, o: f64, h: f64, l: f64, c: f64, vol: u64) -> Bar {
        Bar { time: t(i), open: o, high: h, low: l, close: c, volume: vol, vwap: Some(c), trade_count: Some(10) }
    }

    #[test]
    fn ema_tracks_and_fast_leads_in_uptrend() {
        let v: Vec<f64> = (0..30).map(|i| 100.0 + i as f64).collect();
        let f = ema_series(&v, 9);
        let s = ema_series(&v, 20);
        assert_eq!(f.len(), v.len());
        // In a steady uptrend the faster EMA sits above the slower one.
        assert!(*f.last().unwrap() > *s.last().unwrap());
    }

    #[test]
    fn efficiency_ratio_directional_vs_choppy() {
        let trend = vec![10.0, 11.0, 12.0, 13.0, 14.0];
        assert!((efficiency_ratio(&trend, 4) - 1.0).abs() < 1e-9);
        let chop = vec![10.0, 11.0, 10.0, 11.0, 10.0];
        assert!(efficiency_ratio(&chop, 4) < 0.35);
    }

    #[test]
    fn swing_pivots_found() {
        // lows: 100, 99.5(min), 99.6, 100.9 → swing low at index 1.
        let lows = vec![
            bar(0, 100.0, 100.2, 99.9, 100.0, 1000),
            bar(1, 100.0, 100.0, 99.5, 99.8, 1000),
            bar(2, 99.6, 101.0, 99.6, 100.9, 3000),
            bar(3, 101.0, 102.5, 100.9, 102.4, 3500),
        ];
        assert_eq!(last_swing_low(&lows, 1), Some(1));
        // highs: 100.5, 101.5(peak), 100.8 → swing high at index 1.
        let highs = vec![
            bar(0, 100.0, 100.5, 99.8, 100.2, 1000),
            bar(1, 100.2, 101.5, 100.1, 101.3, 2000),
            bar(2, 100.4, 100.8, 100.0, 100.4, 1500),
        ];
        assert_eq!(last_swing_high(&highs, 1), Some(1));
    }

    #[test]
    fn vwap_crosses_counts_sign_changes() {
        let closes = vec![10.0, 9.0, 11.0, 9.0];
        let vwaps  = vec![10.0, 10.0, 10.0, 10.0];
        // below, above, below → 2 crossings over the window.
        assert_eq!(vwap_crosses(&closes, &vwaps, 6), 2);
    }

    /// A clean long impulse + a 3-bar compact red pullback fires a Long.
    #[test]
    fn clean_long_setup_triggers() {
        let bars = vec![
            bar(0, 100.0, 100.2, 99.9, 100.0, 1000),
            bar(1, 100.0, 100.0, 99.5, 99.8, 1000),  // swing low pivot
            bar(2, 99.6, 101.0, 99.6, 100.9, 3000),  // impulse up
            bar(3, 101.0, 102.5, 100.9, 102.4, 3500),// impulse high
            bar(4, 102.4, 102.4, 102.0, 102.1, 1500),// pullback 1 (red, small)
            bar(5, 102.1, 102.2, 101.8, 101.9, 1200),// pullback 2 (red, small)
            bar(6, 101.9, 102.0, 101.7, 101.8, 1000),// pullback 3 (red, small) → trigger
        ];
        let vwaps = vec![99.0; bars.len()]; // price well above VWAP throughout
        let inp = TickerInput {
            symbol: "AAA".into(), m1: vec![], price: 101.8,
            volume_day: 0, change_day_pct: None, avg_vol: 0,
        };
        let mut st = SymbolState::default();
        let fire = evaluate(&mut st, &inp, &bars, &vwaps, t(7));
        let a = fire.expect("clean long setup should trigger");
        assert_eq!(a.side, Some(Side::Long));
        // Cooldown armed; the same bar can't fire twice.
        assert!(st.cooldown_until.is_some());
        assert!(evaluate(&mut st, &inp, &bars, &vwaps, t(7)).is_none());
    }

    /// A choppy series produces no direction → no signal.
    #[test]
    fn choppy_series_does_not_trigger() {
        let mut bars = Vec::new();
        for i in 0..8 {
            let up = i % 2 == 0;
            let (o, c): (f64, f64) = if up { (100.0, 101.0) } else { (101.0, 100.0) };
            bars.push(bar(i, o.min(c), 101.2, 99.8, c, 1500));
        }
        let vwaps = vec![100.5; bars.len()];
        let inp = TickerInput {
            symbol: "BBB".into(), m1: vec![], price: 100.0,
            volume_day: 0, change_day_pct: None, avg_vol: 0,
        };
        let mut st = SymbolState::default();
        assert!(evaluate(&mut st, &inp, &bars, &vwaps, t(9)).is_none());
    }
}
