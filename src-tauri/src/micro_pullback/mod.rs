// Micro Pullback — premarket multi-tempo "early departure" scanner.
//
// Goal (rewritten in session 41): detect, as early as possible in the premarket
// session, low-priced small caps that are *starting to move now* — before they
// show up in the top gappers. The signal core is, deliberately, only three things:
//   • trade-rate acceleration,
//   • volume acceleration,
//   • a bullish price impulse.
// There is NO mandatory "dormant" gate anymore (the previous silence→ignition→
// confirmation machine was too restrictive). Instead each candidate is compared,
// on four parallel tempos (10/20/40/60 s), against its own rolling 5-minute
// baseline; the moment any one tempo trips, the ticker is *armed* — not alerted.
//
// Final gate (the tape-rate watch): an armed ticker is handed to a per-second watch
// that measures the live trade rate (prints/sec) for up to a minute. It alerts —
// once, then locks for the session — the instant either layer confirms real tape:
//   • fast layer: a very high instantaneous rate (≥5/s over 1 s) fires immediately,
//   • slow layer: a sustained rate (≥2/s over 5 s) fires.
// If neither confirms within the minute the watch is dropped and the ticker becomes
// eligible for detection again. This replaces the old per-tempo absolute trade-count
// minimum: a price/volume departure must be *backed by tape* to reach the scanner.
//
// Multi-tempo rationale:
//   • 10s catches very fast starts,
//   • 20s catches fast-but-less-explosive starts,
//   • 40s catches progressive starts,
//   • 60s catches slower-but-powerful moves.
// A ticker may be picked up by any single tempo.
//
// Data: during premarket the Alpaca live feed subscribes the whole market's
// `trades` via the `*` wildcard, so `on_trade` builds 10-second candles (with a
// per-bar print count) for every printing symbol. The engine reads
// `closed_bars(symbol, S10)`, derives the current T-second window + the prior
// 5-minute baseline (bucketed per tempo), and evaluates the gates. Higher tempos
// are aggregated from the 10s bars.
//
// Late start / crash recovery: the live 10s ring is empty until the feed warms up,
// so the baseline is backfilled from the last few minutes of 1-minute bars (the
// smallest history Alpaca REST serves; 10s isn't available). The 1-minute medians
// are scaled to each tempo (volume/trades linearly with time, range ~√time); the
// engine switches to the exact live baseline once enough 10s buckets exist. The
// current window is always live.
//
// Engine, not a `ScanStrategy::should_alert`, because of the per-session LOCK +
// the historical backfill; it pushes AlertSignals via `scanner::push_alert`. The
// registry still carries the metadata `MicroPullback` strategy (card, toggle,
// name, priority) — see `strategies::micro_pullback`.

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex, RwLock,
};

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use tokio::time::Duration;

use crate::config::secrets::Secrets;
use crate::local_db::universe_repository;
use crate::market_state::aggregators::{Bar, Timeframe};
use crate::market_state::MarketState;
use crate::scanner::push_alert;
use crate::strategies::micro_pullback::ID as STRATEGY_ID;
use crate::types::{AlertSignal, Session, Side};

// ─── Centralised, tweakable config ─────────────────────────────────────────────
// Everything tunable lives here. The engine reads `Config::DEFAULT`; tests build
// their own `Config` so detection is fully backtestable on synthetic/historical
// 10-second bars.
#[derive(Debug, Clone, Copy)]
pub struct Config {
    // ── Candidate universe (gate 1) ──
    pub price_min: f64,
    pub price_max: f64,
    pub float_max: u64,
    /// VERY IMPORTANT toggle:
    ///   false = watch only tickers with a KNOWN float ≤ float_max.
    ///   true  = ALSO watch tickers with no known float (tagged FLOAT_UNKNOWN).
    /// Tickers with unknown float are kept only when this is true. Default: true.
    pub allow_unknown_float: bool,

    // ── Baseline floors (gate 2) — stop a near-zero baseline yielding absurd
    //    ratios. The two _10s floors scale with the tempo (×T/10); the rate and the
    //    percentage floors are intensive and stay constant across tempos.
    pub baseline_volume_floor_10s: f64,
    pub baseline_trades_floor_10s: f64,
    pub baseline_trade_rate_floor: f64,
    pub baseline_range_floor_pct:  f64,

    // ── Final gate: tape-rate watch (replaces the per-tempo absolute trade minimum) ──
    // When detection (gates 1-3) trips, the ticker is NOT alerted immediately. It is
    // handed to a tape-rate watch, polled every second for up to `watch_max_secs`,
    // and fires the instant either layer confirms genuine, sustained tape activity —
    // otherwise it is dropped (and becomes eligible for re-detection). Two layers:
    //   • fast: a very high instantaneous rate fires on the very first second,
    //   • slow: a lower but sustained rate over a longer window.
    pub watch_max_secs:          i64,
    pub watch_fast_window_secs:  i64,
    pub watch_fast_rate_per_sec: f64,
    pub watch_slow_window_secs:  i64,
    pub watch_slow_rate_per_sec: f64,

    // ── Multi-tempo signal thresholds (gate 3) — one entry per tempo. ──
    pub tempos: [Tempo; 4],
}

/// Per-tempo detection thresholds. A tempo trips when the absolute minimums are met
/// AND at least 2 of the 3 acceleration ratios clear their thresholds (see gate 3).
#[derive(Debug, Clone, Copy)]
pub struct Tempo {
    /// Window length in seconds (10/20/40/60). Must be a multiple of BAR_SECS.
    pub secs:                 i64,
    pub return_min_pct:       f64,
    pub volume_min:           f64,
    pub volume_ratio_min:     f64,
    pub trade_rate_ratio_min: f64,
    pub range_ratio_min:      f64,
}

impl Config {
    pub const DEFAULT: Config = Config {
        price_min: 1.0,
        price_max: 25.0,
        float_max: 30_000_000,
        allow_unknown_float: true,

        baseline_volume_floor_10s: 500.0,
        baseline_trades_floor_10s: 2.0,
        baseline_trade_rate_floor: 0.2,
        baseline_range_floor_pct:  0.10,

        watch_max_secs:          60,
        watch_fast_window_secs:  1,
        watch_fast_rate_per_sec: 5.0,
        watch_slow_window_secs:  5,
        watch_slow_rate_per_sec: 2.0,

        tempos: [
            Tempo {
                secs: 10, return_min_pct: 1.0, volume_min: 10_000.0,
                volume_ratio_min: 8.0, trade_rate_ratio_min: 8.0, range_ratio_min: 4.0,
            },
            Tempo {
                secs: 20, return_min_pct: 1.8, volume_min: 20_000.0,
                volume_ratio_min: 7.0, trade_rate_ratio_min: 7.0, range_ratio_min: 4.0,
            },
            Tempo {
                secs: 40, return_min_pct: 2.8, volume_min: 35_000.0,
                volume_ratio_min: 6.0, trade_rate_ratio_min: 6.0, range_ratio_min: 3.5,
            },
            Tempo {
                secs: 60, return_min_pct: 3.5, volume_min: 50_000.0,
                volume_ratio_min: 5.0, trade_rate_ratio_min: 5.0, range_ratio_min: 3.0,
            },
        ],
    };
}

// ─── Engine-loop constants (not detection thresholds) ──────────────────────────
/// 1-second cadence: detection is throttled to once per freshly-closed 10s bar, but
/// the final tape-rate watch is polled every second (see `Config::watch_*`).
const LOOP_INTERVAL_SECS: u64 = 1;
const FLOAT_REFRESH_SECS: u64 = 300;
/// Premarket window in ET wall-clock minutes since midnight: 04:00–09:30. Matches
/// the live feed's `trades` broad-tier window, so the 10s bars actually flow.
const PREMARKET_START_MIN: u32 = 240;
const PREMARKET_END_MIN: u32 = 570;
/// Detection timeframe + bucket length.
const BAR_TF: Timeframe = Timeframe::S10;
const BAR_SECS: i64 = 10;
/// Rolling baseline length (the "last 5 minutes").
const BASELINE_WINDOW_SECS: i64 = 300;
/// Minimum bucket count for a robust live baseline median; below this a tempo falls
/// back to the 1-minute seed (or is skipped if there's no seed yet).
const MIN_BASELINE_BUCKETS: usize = 3;
/// Admit a candidate from this many live 10s bars (at least one full 10s current
/// window + something to anchor; the absolute gate-3 minimums reject true noise).
const MIN_CANDIDATE_BARS: usize = 1;
/// Drop an UNLOCKED machine that hasn't been evaluated in this long (memory bound);
/// locked machines are kept until the session reset so they can't re-alert.
const MACHINE_STALE_SECS: i64 = 20 * 60;

// ── 1-minute backfill seed (late-start / crash recovery) ───────────────────────
/// A candidate is seed-eligible once it has at least this many live 10s bars (so we
/// only backfill genuinely-active young names, not one-off prints).
const MIN_SEED_LIVE_BARS: usize = 2;
/// Enough live 10s bars to cover a full baseline window across all tempos → the
/// seed is no longer needed (exact live baseline available). 5 min + a 1-min cushion.
const FULL_LIVE_BARS: usize = ((BASELINE_WINDOW_SECS + 60) / BAR_SECS) as usize;
/// How far back the 1-minute backfill reaches (5-min baseline + 1-min cushion).
const SEED_LOOKBACK_SECS: i64 = BASELINE_WINDOW_SECS + 60;
/// Minimum span of returned 1-min history required to trust a seed (≈4 min).
const SEED_MIN_SPAN_SECS: i64 = 4 * 60;
/// Throttle + cap on backfill batches (Alpaca multi-symbol bars cap is ~200/req).
const SEED_ATTEMPT_INTERVAL_SECS: u64 = 5;
const SEED_BATCH_MAX: usize = 200;

// ─── Per-ticker state ──────────────────────────────────────────────────────────
// A ticker is LOCKED (already alerted this session) or not, plus a watermark so
// detection runs at most once per freshly-closed 10s bar. Once detection trips, the
// ticker is not alerted right away: it carries a `Watch` (the final tape-rate gate)
// until that confirms (→ alert + lock) or times out (→ watch cleared, re-eligible).
#[derive(Debug, Clone)]
struct Machine {
    locked:        bool,
    last_eval_bar: Option<DateTime<Utc>>,
    watch:         Option<Watch>,
}

impl Machine {
    fn new() -> Self {
        Self { locked: false, last_eval_bar: None, watch: None }
    }
}

/// Final-gate state: detection tripped at `started`; we now poll the live tape rate
/// every second until a layer confirms or `watch_max_secs` elapses. The triggering
/// tempo + metrics are kept so the eventual alert still describes the departure.
#[derive(Debug, Clone, Copy)]
struct Watch {
    started: DateTime<Utc>,
    tempo:   Tempo,
    metrics: TempoMetrics,
}

/// 1-minute backfill seed: the per-1-minute medians of the last few minutes, scaled
/// to each tempo on demand (volume/trades linearly with time, range ~√time).
#[derive(Debug, Clone, Copy)]
struct Seed {
    median_volume_1m:    f64,
    median_trades_1m:    f64,
    median_range_pct_1m: f64,
}

/// Everything the alert needs that isn't a bar — snapshotted under the read lock.
#[derive(Debug, Clone)]
struct Meta {
    price:          f64,
    bid:            Option<f64>,
    ask:            Option<f64>,
    spread:         Option<f64>,
    volume_day:     u64,
    change_day_pct: Option<f64>,
    float_shares:   Option<u64>,
    /// Whether the float is known (drives the FLOAT_UNKNOWN tag).
    float_known:    bool,
}

/// One candidate's per-loop snapshot (read lock held only to build these).
struct Input {
    symbol: String,
    bars:   Vec<Bar>,
    meta:   Meta,
}

// ─── Engine ────────────────────────────────────────────────────────────────────

pub struct MicroPullbackEngine;

impl MicroPullbackEngine {
    /// Spawn the background loop. Returns immediately.
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        running:          Arc<AtomicBool>,
        market:           Arc<RwLock<MarketState>>,
        db:               Arc<Mutex<rusqlite::Connection>>,
        secrets:          Arc<RwLock<Secrets>>,
        active_alerts:    Arc<RwLock<Vec<AlertSignal>>>,
        alert_history:    Arc<RwLock<Vec<AlertSignal>>>,
        strategy_enabled: Arc<RwLock<HashMap<String, bool>>>,
    ) {
        // Tauri-managed runtime so it can be launched from the sync `setup` hook.
        tauri::async_runtime::spawn(async move {
            let cfg = Config::DEFAULT;
            let mut machines: HashMap<String, Machine> = HashMap::new();
            let mut floats = load_floats(&db);
            let mut floats_loaded = std::time::Instant::now();
            // ET day the state belongs to — a new premarket session resets everything
            // (states, locks, triggered list, session caches) at 04:00 ET.
            let mut session_day: Option<String> = None;
            // 1-minute dormancy/baseline seed per symbol. Some(s) = usable; None =
            // attempted but no usable history (don't refetch). Used only while a name
            // has < FULL_LIVE_BARS live 10s bars, then ignored in favour of the live
            // baseline.
            let mut seeds: HashMap<String, Option<Seed>> = HashMap::new();
            let mut last_seed_attempt: Option<std::time::Instant> = None;
            // Market Replay reset watch: replay start / backward seek / new day →
            // clear machines + locks so the replayed session detects from scratch.
            let mut replay_gen = crate::replay::clock::generation();

            while running.load(Ordering::Relaxed) {
                {
                    let g = crate::replay::clock::generation();
                    if g != replay_gen {
                        replay_gen = g;
                        machines.clear();
                        seeds.clear();
                        session_day = None;
                    }
                }
                let enabled = strategy_enabled
                    .read()
                    .unwrap()
                    .get(STRATEGY_ID)
                    .copied()
                    .unwrap_or(true);
                if !enabled {
                    crate::replay::clock::scaled_sleep(LOOP_INTERVAL_SECS * 1000).await;
                    continue;
                }

                if floats_loaded.elapsed() >= Duration::from_secs(FLOAT_REFRESH_SECS) {
                    floats = load_floats(&db);
                    floats_loaded = std::time::Instant::now();
                }

                // App clock: simulated instant during a Market Replay.
                let now = crate::time::now();
                let mock = market.read().unwrap().mock_running;
                let in_window = mock || in_premarket(now);
                if !in_window {
                    if !machines.is_empty() || !seeds.is_empty() {
                        machines.clear();
                        seeds.clear();
                        session_day = None;
                    }
                    crate::replay::clock::scaled_sleep(LOOP_INTERVAL_SECS * 1000).await;
                    continue;
                }

                // New ET day → reset all per-session state (one alert *per session*).
                let today = crate::time::et_date(now);
                if session_day.as_deref() != Some(today.as_str()) {
                    machines.clear();
                    seeds.clear();
                    session_day = Some(today.clone());
                }

                // Snapshot candidate inputs under one brief read lock. Gate 1
                // universe: price in band, float ≤ max (or unknown when allowed),
                // not locked, and at least MIN_CANDIDATE_BARS of live history.
                let inputs: Vec<Input> = {
                    let ms = market.read().unwrap();
                    ms.tickers
                        .values()
                        .filter_map(|t| {
                            let price = t.last_price?;
                            let float = floats.get(&t.symbol).copied();
                            if !gate1_tradeable(price, float, &cfg) {
                                return None;
                            }
                            // Already locked this session → never re-evaluate.
                            if machines.get(&t.symbol).map(|m| m.locked).unwrap_or(false) {
                                return None;
                            }
                            let bars = ms.closed_bars(&t.symbol, BAR_TF);
                            if bars.len() < MIN_CANDIDATE_BARS {
                                return None;
                            }
                            Some(Input {
                                symbol: t.symbol.clone(),
                                bars,
                                meta: Meta {
                                    price,
                                    bid:            t.bid,
                                    ask:            t.ask,
                                    spread:         t.spread,
                                    volume_day:     t.volume_day,
                                    change_day_pct: t.change_day_pct,
                                    float_shares:   float,
                                    float_known:    float.is_some(),
                                },
                            })
                        })
                        .collect()
                };

                // ── 1-minute backfill: seed the baseline for active young names that
                // don't yet have a full live window. Only once enough premarket
                // history exists to backfill (≥ baseline window into the session),
                // never in mock, each symbol attempted at most once, paced.
                // Never during a Market Replay either: the replay feed injects the
                // whole session from 04:00, so the live rings already hold the full
                // baseline — and a REST seed would be redundant.
                let seed_window_open = !mock
                    && !crate::replay::clock::is_active()
                    && crate::time::et_minutes(now)
                        >= PREMARKET_START_MIN + (BASELINE_WINDOW_SECS / 60) as u32;
                let seed_due = last_seed_attempt
                    .map(|t| t.elapsed() >= Duration::from_secs(SEED_ATTEMPT_INTERVAL_SECS))
                    .unwrap_or(true);
                if seed_window_open && seed_due {
                    let to_seed: Vec<String> = inputs
                        .iter()
                        .filter(|inp| {
                            inp.bars.len() >= MIN_SEED_LIVE_BARS
                                && inp.bars.len() < FULL_LIVE_BARS
                                && !seeds.contains_key(&inp.symbol)
                        })
                        .map(|inp| inp.symbol.clone())
                        .take(SEED_BATCH_MAX)
                        .collect();
                    if !to_seed.is_empty() {
                        last_seed_attempt = Some(std::time::Instant::now());
                        for s in &to_seed {
                            seeds.entry(s.clone()).or_insert(None);
                        }
                        let creds = {
                            let s = secrets.read().unwrap();
                            (s.alpaca_key.clone(), s.alpaca_secret.clone())
                        };
                        if let (Some(k), Some(sc)) = creds {
                            if !k.is_empty() && !sc.is_empty() {
                                let start = (now - ChronoDuration::seconds(SEED_LOOKBACK_SECS))
                                    .format("%Y-%m-%dT%H:%M:%SZ")
                                    .to_string();
                                match crate::alpaca::bars::fetch_minute_bars_since(
                                    &k, &sc, &to_seed, &start,
                                )
                                .await
                                {
                                    Ok(map) => {
                                        for (sym, mbars) in map {
                                            if let Some(s) = seed_from_minutes(&mbars) {
                                                seeds.insert(sym, Some(s));
                                            }
                                        }
                                    }
                                    Err(e) => eprintln!(
                                        "[tagdash] micro_pullback: seed backfill failed: {e}"
                                    ),
                                }
                            }
                        }
                    }
                }

                // News resolver (informational only — never required, never blocks).
                // Read lazily at fire time; injected so detection stays unit-testable.
                let news_fn = |sym: &str| news_tag(&market, sym, now);

                let mut fires: Vec<AlertSignal> = Vec::new();
                for inp in &inputs {
                    let seed = seeds.get(&inp.symbol).and_then(|o| o.as_ref());
                    let m = machines.entry(inp.symbol.clone()).or_insert_with(Machine::new);
                    // Live tape rate resolver for the final gate (trade prints over the
                    // last `secs` seconds; 0 when the symbol has never printed).
                    let tape_fn = |secs: i64| {
                        market.read().unwrap().trades_in_last(&inp.symbol, secs, now).unwrap_or(0)
                    };
                    if let Some(fire) = evaluate(m, inp, &cfg, now, seed, &news_fn, &tape_fn) {
                        fires.push(fire);
                    }
                }

                // Evict stale machines. Keep locked (until the session reset) and
                // actively-watching ones; otherwise drop those idle past the bound.
                machines.retain(|_, m| {
                    m.locked
                        || m.watch.is_some()
                        || m.last_eval_bar
                            .map(|t| (now - t).num_seconds() <= MACHINE_STALE_SECS)
                            .unwrap_or(true)
                });

                for fire in fires {
                    push_alert(&active_alerts, &alert_history, fire);
                }

                crate::replay::clock::scaled_sleep(LOOP_INTERVAL_SECS * 1000).await;
            }
        });
    }
}

/// Evaluate one ticker. Two stages:
///   1. Detection (gates 1-3, throttled to once per freshly-closed 10s bar): the
///      first tempo that trips *arms* the tape-rate watch — it does NOT alert.
///   2. Final gate (every tick while armed): poll the live tape rate; fire + lock the
///      instant the fast (1s) or slow (5s) layer confirms, or drop the watch once it
///      has run past `watch_max_secs`.
/// Arming and confirming may happen on the same tick (an already-hot ticker fires
/// immediately). `news_fn`/`tape_fn` are injected so detection stays unit-testable.
fn evaluate(
    m:       &mut Machine,
    inp:     &Input,
    cfg:     &Config,
    now:     DateTime<Utc>,
    seed:    Option<&Seed>,
    news_fn: &dyn Fn(&str) -> (String, bool),
    tape_fn: &dyn Fn(i64) -> u64,
) -> Option<AlertSignal> {
    if m.locked {
        return None;
    }

    // ── Stage 1: detection → arm the watch (no alert yet). Skipped while a watch is
    // already running, and throttled to one evaluation per freshly-closed 10s bar.
    if m.watch.is_none() {
        let bars = &inp.bars;
        let latest = bars.last()?.time;
        if m.last_eval_bar != Some(latest) {
            m.last_eval_bar = Some(latest);
            for tempo in &cfg.tempos {
                if let Some(metrics) = eval_tempo(bars, tempo, cfg, seed) {
                    if gate3_trips(&metrics, tempo) {
                        m.watch = Some(Watch { started: now, tempo: *tempo, metrics });
                        break;
                    }
                }
            }
        }
    }

    // ── Stage 2: tape-rate watch (final gate), polled every tick. ──
    if let Some(w) = m.watch {
        // Timed out unconfirmed → drop the watch; the ticker stays re-eligible.
        if (now - w.started).num_seconds() > cfg.watch_max_secs {
            m.watch = None;
            return None;
        }
        let fast = tape_fn(cfg.watch_fast_window_secs) as f64 / cfg.watch_fast_window_secs as f64;
        let slow = tape_fn(cfg.watch_slow_window_secs) as f64 / cfg.watch_slow_window_secs as f64;
        let confirmed = if fast >= cfg.watch_fast_rate_per_sec {
            Some(fast)
        } else if slow >= cfg.watch_slow_rate_per_sec {
            Some(slow)
        } else {
            None
        };
        if let Some(tape_rate) = confirmed {
            let (news, news_today) = news_fn(&inp.symbol);
            let alert = make_alert(inp, &w.tempo, &w.metrics, tape_rate, &news, news_today, now);
            m.locked = true;
            m.watch = None;
            return Some(alert);
        }
    }
    None
}

// ─── Gate 1 ────────────────────────────────────────────────────────────────────

/// Gate 1 — tradability: price in band and a float that qualifies (known ≤ max, or
/// unknown when `allow_unknown_float`).
fn gate1_tradeable(price: f64, float: Option<u64>, cfg: &Config) -> bool {
    if !(cfg.price_min..=cfg.price_max).contains(&price) {
        return false;
    }
    match float {
        Some(f) => f <= cfg.float_max,
        None => cfg.allow_unknown_float,
    }
}

// ─── Gates 2 + 3 (per tempo) ───────────────────────────────────────────────────

/// Metrics for one tempo: the current T-second window vs its 5-minute baseline.
#[derive(Debug, Clone, Copy)]
struct TempoMetrics {
    current_volume: f64,
    current_trades: u64,
    price_return:   f64,
    volume_ratio:     f64,
    trade_rate_ratio: f64,
    range_ratio:      f64,
}

/// Build the metrics for `tempo` from the live 10s bars: the current T-second window
/// (gate 3 numerator) and the prior 5-minute baseline (gate 2 denominator, live
/// buckets if enough exist, else the 1-minute seed). None when the current window
/// isn't fully covered or no baseline is available.
fn eval_tempo(bars: &[Bar], tempo: &Tempo, cfg: &Config, seed: Option<&Seed>) -> Option<TempoMetrics> {
    let t = tempo.secs;
    let need = (t / BAR_SECS) as usize;
    let latest = bars.last()?.time;

    // Current window = the last T seconds of bars (must be fully covered).
    let cur_cutoff = latest - ChronoDuration::seconds(t - BAR_SECS);
    let cur_start = bars.partition_point(|b| b.time < cur_cutoff);
    let current = &bars[cur_start..];
    if current.len() < need {
        return None;
    }
    let (cur_vol, cur_trades, cur_range_pct, cur_return) = window_metrics(current);
    let cur_trade_rate = cur_trades as f64 / t as f64;

    // Baseline = the 5 minutes before the current window, bucketed per tempo.
    let base_lo = cur_cutoff - ChronoDuration::seconds(BASELINE_WINDOW_SECS);
    let base_start = bars.partition_point(|b| b.time < base_lo);
    let baseline_bars = &bars[base_start..cur_start];
    let (base_vol, base_trades, base_range_pct) =
        match live_baseline(baseline_bars, t) {
            Some(b) => b,
            None => seed_baseline(seed?, t),
        };
    let base_trade_rate = base_trades / t as f64;

    // Floors (the _10s ones scale with the tempo; rate/% floors are constant).
    let scale = t as f64 / 10.0;
    let vol_floor   = cfg.baseline_volume_floor_10s * scale;
    let rate_floor  = cfg.baseline_trade_rate_floor;
    let range_floor = cfg.baseline_range_floor_pct;

    Some(TempoMetrics {
        current_volume: cur_vol,
        current_trades: cur_trades,
        price_return:   cur_return,
        volume_ratio:     cur_vol / base_vol.max(vol_floor),
        trade_rate_ratio: cur_trade_rate / base_trade_rate.max(rate_floor),
        range_ratio:      cur_range_pct / base_range_pct.max(range_floor),
    })
}

/// Gate 3 — a tempo trips when the absolute minimums are met AND at least 2 of the 3
/// acceleration ratios clear their thresholds (loose enough to catch starts, the
/// 2-of-3 limits the noise). The absolute trade-count minimum was removed in favour
/// of the final tape-rate watch (see `evaluate`).
fn gate3_trips(mtr: &TempoMetrics, tempo: &Tempo) -> bool {
    if mtr.price_return < tempo.return_min_pct
        || mtr.current_volume < tempo.volume_min
    {
        return false;
    }
    let ratios = [
        mtr.volume_ratio >= tempo.volume_ratio_min,
        mtr.trade_rate_ratio >= tempo.trade_rate_ratio_min,
        mtr.range_ratio >= tempo.range_ratio_min,
    ];
    ratios.iter().filter(|&&x| x).count() >= 2
}

// ─── Baseline helpers ──────────────────────────────────────────────────────────

/// Live baseline for a tempo: aggregate the baseline 10s bars into T-second buckets
/// and take robust medians (volume, trade count, range %). None when there aren't
/// enough buckets for a stable median (caller falls back to the seed).
fn live_baseline(baseline_bars: &[Bar], bucket_secs: i64) -> Option<(f64, f64, f64)> {
    let buckets = aggregate_buckets(baseline_bars, bucket_secs);
    if buckets.len() < MIN_BASELINE_BUCKETS {
        return None;
    }
    let mut vols: Vec<f64> = buckets.iter().map(|b| b.0).collect();
    let mut trs: Vec<f64> = buckets.iter().map(|b| b.1).collect();
    let mut rngs: Vec<f64> = buckets.iter().map(|b| b.2).collect();
    Some((median(&mut vols), median(&mut trs), median(&mut rngs)))
}

/// 1-minute seed scaled to a tempo: volume/trades scale linearly with the window
/// length, range scales ~√(window) (volatility grows with √time).
fn seed_baseline(seed: &Seed, tempo_secs: i64) -> (f64, f64, f64) {
    let frac = tempo_secs as f64 / 60.0;
    (
        seed.median_volume_1m * frac,
        seed.median_trades_1m * frac,
        seed.median_range_pct_1m * frac.sqrt(),
    )
}

/// Aggregate 10s bars into `bucket_secs` buckets (aligned to bucket boundaries),
/// returning (volume, trade_count, range_pct) per bucket.
fn aggregate_buckets(bars: &[Bar], bucket_secs: i64) -> Vec<(f64, f64, f64)> {
    let mut out: Vec<(i64, Vec<&Bar>)> = Vec::new();
    for b in bars {
        let key = b.time.timestamp() / bucket_secs;
        match out.last_mut() {
            Some((k, group)) if *k == key => group.push(b),
            _ => out.push((key, vec![b])),
        }
    }
    out.iter()
        .map(|(_, group)| {
            let (vol, trades, range_pct, _ret) = window_metrics_refs(group);
            (vol, trades as f64, range_pct)
        })
        .collect()
}

/// (volume, trade_count, range_pct, return_pct) over a slice of contiguous bars.
fn window_metrics(win: &[Bar]) -> (f64, u64, f64, f64) {
    window_metrics_iter(win.iter())
}

fn window_metrics_refs(win: &[&Bar]) -> (f64, u64, f64, f64) {
    window_metrics_iter(win.iter().copied())
}

fn window_metrics_iter<'a>(it: impl Iterator<Item = &'a Bar> + Clone) -> (f64, u64, f64, f64) {
    let mut vol = 0.0;
    let mut trades = 0u64;
    let mut high = f64::MIN;
    let mut low = f64::MAX;
    let mut close_sum = 0.0;
    let mut n = 0usize;
    let mut first_open = None;
    let mut last_close = 0.0;
    for b in it {
        vol += b.volume as f64;
        trades += b.trade_count.unwrap_or(0);
        high = high.max(b.high);
        low = low.min(b.low);
        close_sum += b.close;
        if first_open.is_none() {
            first_open = Some(b.open);
        }
        last_close = b.close;
        n += 1;
    }
    if n == 0 {
        return (0.0, 0, 0.0, 0.0);
    }
    let avg = close_sum / n as f64;
    let range_pct = if avg > 0.0 { (high - low) / avg * 100.0 } else { 0.0 };
    let ret = match first_open {
        Some(o) if o > 0.0 => (last_close - o) / o * 100.0,
        _ => 0.0,
    };
    (vol, trades, range_pct, ret)
}

/// Robust 1-minute backfill seed from the last few minutes of 1-minute bars. None
/// when there isn't enough history (too few bars / too short a span — the
/// genuinely-new case, not a missed one).
fn seed_from_minutes(mbars: &[Bar]) -> Option<Seed> {
    if mbars.len() < 2 {
        return None;
    }
    let span = (mbars.last()?.time - mbars.first()?.time).num_seconds();
    if span < SEED_MIN_SPAN_SECS {
        return None;
    }
    let mut vols: Vec<f64> = mbars.iter().map(|b| b.volume as f64).collect();
    let mut trs: Vec<f64> = mbars.iter().map(|b| b.trade_count.unwrap_or(0) as f64).collect();
    let mut rngs: Vec<f64> = mbars
        .iter()
        .map(|b| {
            let avg = (b.high + b.low) / 2.0;
            if avg > 0.0 { (b.high - b.low) / avg * 100.0 } else { 0.0 }
        })
        .collect();
    Some(Seed {
        median_volume_1m:    median(&mut vols),
        median_trades_1m:    median(&mut trs),
        median_range_pct_1m: median(&mut rngs),
    })
}

/// Median of the values (mutates by sorting). 0.0 on empty.
fn median(v: &mut [f64]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

fn in_premarket(now: DateTime<Utc>) -> bool {
    (PREMARKET_START_MIN..PREMARKET_END_MIN).contains(&crate::time::et_minutes(now))
}

/// Informational news tag (never required, never blocks): NEWS « headline » when a
/// live headline exists for the symbol, else TAPE_ONLY. Returns (label, news_today).
fn news_tag(market: &Arc<RwLock<MarketState>>, symbol: &str, _now: DateTime<Utc>) -> (String, bool) {
    match market.read().unwrap().latest_news(symbol) {
        Some(n) => (format!("NEWS « {} »", truncate(&n.headline, 60)), true),
        None => ("TAPE_ONLY".to_string(), false),
    }
}

// ─── Alert construction ────────────────────────────────────────────────────────

fn make_alert(
    inp:        &Input,
    tempo:      &Tempo,
    mtr:        &TempoMetrics,
    tape_rate:  f64,
    news:       &str,
    news_today: bool,
    now:        DateTime<Utc>,
) -> AlertSignal {
    let float_tag = if inp.meta.float_known { "" } else { " · FLOAT_UNKNOWN" };
    let reason = format!(
        "Micro Pullback — départ {}s : +{:.1}% (vol ×{:.0}, trades ×{:.0}, range ×{:.0}) \
         · tape {:.1} tr/s · {} trades / {:.0} sh · {}{} — ${:.2}",
        tempo.secs,
        mtr.price_return,
        mtr.volume_ratio,
        mtr.trade_rate_ratio,
        mtr.range_ratio,
        tape_rate,
        mtr.current_trades,
        mtr.current_volume,
        news,
        float_tag,
        inp.meta.price,
    );

    AlertSignal {
        alert_id:       format!("mp-{}-{}", now.timestamp_millis(), inp.symbol),
        timestamp:      now,
        symbol:         inp.symbol.clone(),
        strategy_id:    STRATEGY_ID.to_string(),
        strategy_name:  "Micro Pullback".to_string(),
        priority:       5,
        session:        Session::Premarket,
        price:          Some(inp.meta.price),
        bid:            inp.meta.bid,
        ask:            inp.meta.ask,
        spread:         inp.meta.spread,
        volume:         Some(inp.meta.volume_day),
        rvol:           None,
        change_day_pct: inp.meta.change_day_pct,
        float_shares:   inp.meta.float_shares,
        news_today,
        halted:         Some(false),
        latency_ui_ms:  None,
        reason,
        display_timeframe: Some("10s".to_string()),
        side:           Some(Side::Long),
    }
}

// ─── Data loading ──────────────────────────────────────────────────────────────

/// Per-symbol float (shares) from the universe table; symbols with no known float
/// are omitted (treated as unknown — admitted only when `allow_unknown_float`).
fn load_floats(db: &Arc<Mutex<rusqlite::Connection>>) -> HashMap<String, u64> {
    let conn = db.lock().unwrap();
    universe_repository::get_all(&conn)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|a| a.float_shares.filter(|f| *f > 0).map(|f| (a.symbol, f as u64)))
        .collect()
}

/// Truncate a headline to `max` chars (char-boundary safe) for the alert reason.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max).collect();
        format!("{cut}…")
    }
}
