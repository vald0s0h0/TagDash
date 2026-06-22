// Perfect Pullback — stateful, multi-timeframe pullback-continuation engine.
//
// Goal: catch a clean trend-continuation entry. First a ticker makes a strong,
// high-relative-volume directional move (gate 1); then it pulls back in a healthy,
// low-volume, shallow way (gate 2); we fire on the close of the 2nd pullback candle
// so a continuation can be traded in the direction of the move. There is a long and
// a short side (up move → long bias, down move → short bias). The engine can run in
// parallel on the 1, 2, 5 and 10-minute timeframes, each toggled by an ENABLE_* flag
// below; for now only the 5-minute timeframe is active.
//
// Ticker SELECTION is NOT done here: it comes from the Market Attention Gate
// (`crate::market_attention`), which publishes, once a minute between 09:30 and
// 12:30 ET, the top-10 most-watched/traded tickers (direction-agnostic). This engine
// reads that list and MEMORISES the symbols for the whole session: during a pullback
// a ticker's volume falls and it can drop off the attention list, yet it stays worth
// trading — so once a symbol has been on the list it remains watched until the end of
// the session (16:00) regardless of whether it is still on the list. Gate 1 here is
// purely a move-strength check (consecutive bars + relative volume + dollar volume);
// the moving-average / VWAP location filters were removed (selection is Market
// Attention's job now).
//
// Why an engine and not a `ScanStrategy::should_alert`: the gates form a per-(symbol,
// timeframe) state machine spanning many bars (count the consecutive move bars,
// detect the colour flip, count the pullback bars, measure retracement/volume/ATR).
// That can't fit the stateless per-tick contract, so this engine runs in its own
// tokio task and pushes AlertSignals straight into the active-alert list via
// `scanner::push_alert` (the same escape hatch the price-alarm watcher uses). The
// registry still carries a metadata `PerfectPullback` strategy (card, toggle,
// name, priority) — see `strategies::perfect_pullback`.
//
// Bars: during the regular session Alpaca streams 1-minute bars for the whole
// universe (MarketState::on_bar → M1 ring), while trade ticks only flow for the
// displayed (focus) symbols. So we read the M1 closed bars and aggregate the
// 2/5/10-minute timeframes from them ourselves rather than relying
// on the per-symbol trade-built aggregators (which are empty for non-focus names).

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
/// Per-(symbol, timeframe) re-arm cooldown after a fire (a fresh move→pullback can
/// re-trigger once it elapses).
pub const COOLDOWN_SECS: u64 = 180;
/// How often the engine evaluates the gates (seconds).
const LOOP_INTERVAL_SECS: u64 = 2;
/// How often the per-symbol average-daily-volume map is reloaded from the universe
/// table (seconds).
const AVG_VOL_REFRESH_SECS: u64 = 300;

/// Tradeable price band (USD). Keeps the engine off sub-penny noise and ultra-highs.
const PRICE_MIN: f64 = 1.0;
const PRICE_MAX: f64 = 1000.0;

// ── Timeframe on/off switches ───────────────────────────────────────────────
// Flip a flag to true/false to watch / ignore that timeframe. When a timeframe is
// off the engine neither aggregates nor evaluates its bars (no gate machines are
// created for it). For now only the 5-minute timeframe is active: bars are only
// aggregated, watched and fired on the 5m.
const ENABLE_1M:  bool = false;
const ENABLE_2M:  bool = false;
const ENABLE_5M:  bool = true;
const ENABLE_10M: bool = false;

// ── Gate 1 — strong directional move ──────────────────────────────────────────
/// Minimum consecutive same-direction candles to establish the move.
const MIN_MOVE_BARS: usize = 2;
/// Relative volume of the move vs the ticker's own daily norm. The move's total
/// volume must be at least RVOL_MIN × the volume it would normally trade over the
/// same number of minutes (avg daily volume spread over a 390-min session).
const RVOL_MIN: f64 = 2.0;
/// Minimum dollar volume traded during the whole move, to avoid thin-name noise.
const MIN_MOVE_DOLLAR_VOLUME: f64 = 250_000.0;
/// Regular-session length in minutes, used to pro-rate the daily average volume
/// down to a per-bar expectation for the relative-volume calc.
const REGULAR_MINUTES_PER_DAY: f64 = 390.0;

// ── Gate 2 — healthy pullback ──────────────────────────────────────────────────
/// Minimum pullback candles before the alert can fire (fires at the close of the
/// 2nd one at the earliest).
const MIN_PULLBACK_BARS: usize = 2;
/// Maximum retracement of the move allowed for a still-healthy pullback (0.60 = 60%).
const MAX_RETRACE: f64 = 0.60;
/// The last closed (pullback) candle's true range must be below this multiple of the
/// average true range of the move candles — rejects a violent reversal bar.
const ATR_MAX_MULT: f64 = 2.0;

/// Regular cash session in ET wall-clock minutes since midnight: 09:30–16:00. Perfect
/// Pullback watches and fires across the whole session; Market Attention only feeds
/// NEW candidate names during its own 09:30–12:30 window, but a memorised ticker
/// stays tradeable here until 16:00.
const SESSION_START_MIN: u32 = 9 * 60 + 30; // 570
const SESSION_END_MIN:   u32 = 16 * 60;     // 960

/// Drop a gate machine that hasn't seen a new bar in this many seconds (memory
/// bound — keeps the map to genuinely active names).
const GATE_STALE_SECS: i64 = 30 * 60;

/// The four timeframes, as (label, bucket seconds, enabled). Only the ones whose
/// flag is true are aggregated, watched and fired on.
const TIMEFRAMES: &[(&str, i64, bool)] = &[
    ("1m",  60,  ENABLE_1M),
    ("2m",  120, ENABLE_2M),
    ("5m",  300, ENABLE_5M),
    ("10m", 600, ENABLE_10M),
];

/// Per-symbol snapshot read from MarketState once per loop, then fed to every
/// timeframe's gate machine (so we hold the read lock only briefly).
struct TickerInput {
    symbol:         String,
    /// Closed 1-minute bars (oldest → newest); higher timeframes are aggregated.
    m1:             Vec<Bar>,
    price:          f64,
    volume_day:     u64,
    change_day_pct: Option<f64>,
    avg_vol:        u64,
}

// ─── Per-(symbol, timeframe) gate state ───────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// No move under way — waiting for the first directional candle.
    Idle,
    /// Inside a directional run (gate 1), counting consecutive same-colour bars.
    Move,
    /// A colour flip happened — counting pullback candles (gate 2).
    Pullback,
}

#[derive(Debug, Clone)]
struct GateState {
    phase:          Phase,
    /// Direction of the current move (Long = up, Short = down). Discovered when the
    /// run starts; meaningless in Idle.
    side:           Side,
    last_bar_time:  Option<DateTime<Utc>>,
    /// Consecutive same-direction candles forming the move (gate 1).
    move_bars:      Vec<Bar>,
    /// Counter-direction candles forming the pullback (gate 2).
    pullback_bars:  Vec<Bar>,
    cooldown_until: Option<DateTime<Utc>>,
}

impl GateState {
    fn new() -> Self {
        Self {
            phase:          Phase::Idle,
            side:           Side::Long,
            last_bar_time:  None,
            move_bars:      Vec::new(),
            pullback_bars:  Vec::new(),
            cooldown_until: None,
        }
    }

    fn reset_idle(&mut self) {
        self.phase = Phase::Idle;
        self.move_bars.clear();
        self.pullback_bars.clear();
    }

    /// Begin a fresh move from `b` in the given direction.
    fn start_move(&mut self, b: &Bar, side: Side) {
        self.side = side;
        self.move_bars = vec![b.clone()];
        self.pullback_bars.clear();
        self.phase = Phase::Move;
    }
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
            // Per-(symbol, timeframe) gate machines.
            let mut gates: HashMap<(String, String), GateState> = HashMap::new();
            // Per-symbol average daily volume (relative-volume base), refreshed
            // periodically from the universe table.
            let mut avg_volumes = load_avg_volumes(&db);
            let mut avg_vol_loaded = std::time::Instant::now();
            // Memorised candidate set: every symbol Market Attention has selected
            // today (symbol → first time seen). A ticker stays here for the whole
            // session even after it leaves the attention list, so a pullback whose
            // falling volume drops it off the list never loses the candidate.
            // `watched_day` is the ET date the set is built for (cleared at a new day).
            let mut watched: HashMap<String, DateTime<Utc>> = HashMap::new();
            let mut watched_day: Option<String> = None;
            // Market Replay reset watch: replay start / backward seek / new day →
            // drop gate machines and the memorised candidate set so the replayed
            // session is rebuilt from the simulated clock.
            let mut replay_gen = crate::replay::clock::generation();

            while running.load(Ordering::Relaxed) {
                {
                    let g = crate::replay::clock::generation();
                    if g != replay_gen {
                        replay_gen = g;
                        gates.clear();
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

                // Snapshot the per-symbol inputs (M1 closed bars + live price) under a
                // brief read lock; all gate logic runs outside the lock. Pre-filter to
                // active names: a known avg volume (so rvol is meaningful), a price in
                // band, at least a couple of M1 bars, and — the candidate gate — a
                // symbol Market Attention has selected (and we've memorised) today.
                let inputs: Vec<TickerInput> = {
                    let ms = market.read().unwrap();
                    ms.tickers
                        .values()
                        .filter_map(|t| {
                            let price = t.last_price?;
                            if !(PRICE_MIN..=PRICE_MAX).contains(&price) {
                                return None;
                            }
                            let avg_vol = *avg_volumes.get(&t.symbol).filter(|&&v| v > 0)?;
                            let m1 = ms.closed_bars(&t.symbol, Timeframe::M1);
                            if m1.len() < MIN_MOVE_BARS + 1 {
                                return None;
                            }
                            // Candidate gate: only Market-Attention-selected tickers.
                            if !watched.contains_key(&t.symbol) {
                                return None;
                            }
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

                let mut fires: Vec<AlertSignal> = Vec::new();
                for inp in inputs {
                    for &(tf_label, bucket_secs, tf_enabled) in TIMEFRAMES {
                        if !tf_enabled {
                            continue;
                        }
                        let bars = aggregate(&inp.m1, bucket_secs, now);
                        if bars.len() < MIN_MOVE_BARS + 1 {
                            continue;
                        }
                        let key = (inp.symbol.clone(), tf_label.to_string());
                        let gs = gates.entry(key).or_insert_with(GateState::new);
                        if let Some(fire) =
                            process(gs, &inp, tf_label, bucket_secs, &bars, now)
                        {
                            fires.push(fire);
                        }
                    }
                }

                // Prune stale gate machines (symbol gone quiet) to bound memory.
                gates.retain(|_, g| {
                    g.last_bar_time
                        .map(|t| (now - t).num_seconds() <= GATE_STALE_SECS)
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

/// Drive one (symbol, timeframe) gate machine over any newly-closed bars. Returns a
/// fire signal when a healthy pullback completes.
fn process(
    gs:          &mut GateState,
    inp:         &TickerInput,
    tf_label:    &str,
    bucket_secs: i64,
    bars:        &[Bar],
    now:         DateTime<Utc>,
) -> Option<AlertSignal> {
    // Cooldown: skip until it elapses, then resume watching fresh.
    if let Some(until) = gs.cooldown_until {
        if now < until {
            return None;
        }
        gs.cooldown_until = None;
        gs.reset_idle();
    }

    // Only process bars closed after the last one we handled, in order.
    let new_bars: Vec<&Bar> = match gs.last_bar_time {
        Some(t) => bars.iter().filter(|b| b.time > t).collect(),
        None => bars.iter().collect(),
    };
    let mut fire: Option<AlertSignal> = None;

    for b in new_bars {
        gs.last_bar_time = Some(b.time);
        let up = b.close > b.open;
        let down = b.close < b.open;

        match gs.phase {
            // ── Idle — wait for the first directional candle to seed a move. ──────
            Phase::Idle => {
                if up {
                    gs.start_move(b, Side::Long);
                } else if down {
                    gs.start_move(b, Side::Short);
                }
            }

            // ── Gate 1 — build the directional run; the colour flip hands off. ────
            Phase::Move => {
                let continues = match gs.side {
                    Side::Long => up,
                    Side::Short => down,
                };
                if continues {
                    gs.move_bars.push(b.clone());
                } else {
                    // Opposite (or doji) candle = potential pullback start. Only
                    // hand off to gate 2 if the move is long enough AND qualified
                    // (relative volume + dollar volume — move strength only).
                    let qualified = gs.move_bars.len() >= MIN_MOVE_BARS
                        && move_qualifies(gs, inp.avg_vol, bucket_secs);
                    if qualified {
                        gs.pullback_bars = vec![b.clone()];
                        gs.phase = Phase::Pullback;
                    } else {
                        // Move fizzled — restart detection from this candle.
                        if up {
                            gs.start_move(b, Side::Long);
                        } else if down {
                            gs.start_move(b, Side::Short);
                        } else {
                            gs.reset_idle();
                        }
                    }
                }
            }

            // ── Gate 2 — count the pullback, then fire when it's healthy. ─────────
            Phase::Pullback => {
                let resumes = match gs.side {
                    Side::Long => up,
                    Side::Short => down,
                };
                if resumes {
                    // Trend resumed before a tradeable pullback formed — treat this
                    // candle as the start of a fresh move (continuation).
                    gs.start_move(b, gs.side);
                } else {
                    gs.pullback_bars.push(b.clone());
                    if gs.pullback_bars.len() >= MIN_PULLBACK_BARS {
                        match evaluate_pullback(gs, b) {
                            PullbackVerdict::Fire { retrace } => {
                                let move_vol: u64 =
                                    gs.move_bars.iter().map(|bar| bar.volume).sum();
                                let rvol = move_rvol(
                                    move_vol, gs.move_bars.len(), bucket_secs, inp.avg_vol,
                                );
                                fire = Some(make_alert(
                                    &inp.symbol, tf_label, gs.side, rvol, retrace,
                                    gs.move_bars.len(), gs.pullback_bars.len(),
                                    inp.price, inp.volume_day, inp.change_day_pct, now,
                                ));
                                gs.cooldown_until =
                                    Some(now + ChronoDuration::seconds(COOLDOWN_SECS as i64));
                                gs.reset_idle();
                            }
                            // Too deep / volume not lost → the setup is dead.
                            PullbackVerdict::Abort => gs.reset_idle(),
                            // Calm enough setup but this candle isn't a trigger yet
                            // (e.g. an oversized bar) — keep waiting on the next one.
                            PullbackVerdict::Wait => {}
                        }
                    }
                }
            }
        }
    }

    fire
}

// ─── Gate helpers ──────────────────────────────────────────────────────────────

/// Gate 1 qualification at the colour flip: move strength only — relative volume
/// and dollar volume of the whole move. Ticker selection (which names are worth
/// watching at all) is handled upstream by the Market Attention Gate, so the old
/// moving-average / VWAP location filters were removed.
fn move_qualifies(gs: &GateState, avg_vol: u64, bucket_secs: i64) -> bool {
    // Dollar volume of the whole move.
    let move_vol: u64 = gs.move_bars.iter().map(|b| b.volume).sum();
    let dollar_volume: f64 = gs
        .move_bars
        .iter()
        .map(|b| b.volume as f64 * b.close)
        .sum();
    if dollar_volume < MIN_MOVE_DOLLAR_VOLUME {
        return false;
    }

    // Relative volume vs the ticker's own daily norm pro-rated to the move duration.
    move_rvol(move_vol, gs.move_bars.len(), bucket_secs, avg_vol).map_or(false, |r| r >= RVOL_MIN)
}

enum PullbackVerdict {
    Fire { retrace: f64 },
    Abort,
    Wait,
}

/// Gate 2 evaluation at the close of a pullback candle (≥ MIN_PULLBACK_BARS).
fn evaluate_pullback(gs: &GateState, last_bar: &Bar) -> PullbackVerdict {
    let is_long = matches!(gs.side, Side::Long);

    // Move envelope.
    let move_high = gs.move_bars.iter().map(|b| b.high).fold(f64::MIN, f64::max);
    let move_low = gs.move_bars.iter().map(|b| b.low).fold(f64::MAX, f64::min);
    let amplitude = move_high - move_low;
    if amplitude <= 0.0 {
        return PullbackVerdict::Abort;
    }

    // Retracement of the pullback into the move (from the move's far extreme).
    let depth = if is_long {
        let pb_low = gs.pullback_bars.iter().map(|b| b.low).fold(f64::MAX, f64::min);
        move_high - pb_low
    } else {
        let pb_high = gs.pullback_bars.iter().map(|b| b.high).fold(f64::MIN, f64::max);
        pb_high - move_low
    };
    let retrace = depth / amplitude;
    if retrace > MAX_RETRACE {
        return PullbackVerdict::Abort;
    }

    // Volume must be lost vs the move: total pullback volume ≤ total move volume.
    let move_vol: u64 = gs.move_bars.iter().map(|b| b.volume).sum();
    let pullback_vol: u64 = gs.pullback_bars.iter().map(|b| b.volume).sum();
    if pullback_vol > move_vol {
        return PullbackVerdict::Abort;
    }

    // The just-closed pullback candle must not be a violent bar: its true range
    // below ATR_MAX_MULT × the average true range of the move candles.
    let avg_move_tr = avg_true_range(&gs.move_bars);
    let last_tr = last_bar.high - last_bar.low;
    if avg_move_tr > 0.0 && last_tr >= ATR_MAX_MULT * avg_move_tr {
        return PullbackVerdict::Wait;
    }

    PullbackVerdict::Fire { retrace }
}

/// Relative volume of the move vs the daily norm pro-rated to the move's minutes.
/// None when the average is unknown / the move has no duration.
fn move_rvol(move_vol: u64, n_bars: usize, bucket_secs: i64, avg_vol: u64) -> Option<f64> {
    if avg_vol == 0 || n_bars == 0 {
        return None;
    }
    let minutes = n_bars as f64 * (bucket_secs as f64 / 60.0);
    let expected = avg_vol as f64 * (minutes / REGULAR_MINUTES_PER_DAY);
    if expected <= 0.0 {
        return None;
    }
    Some(move_vol as f64 / expected)
}

/// Average true range (here high−low, wicks included) of the move candles.
fn avg_true_range(bars: &[Bar]) -> f64 {
    if bars.is_empty() {
        return 0.0;
    }
    bars.iter().map(|b| b.high - b.low).sum::<f64>() / bars.len() as f64
}

/// Aggregate closed 1-min bars into closed `bucket_secs` bars (clock-aligned; 09:30
/// ET aligns since 13:30:00 UTC is divisible by 60/120/300/600). A bucket is
/// "closed" only once `now` is past its end. For the 1-minute timeframe the M1 bars
/// are already aligned closed candles, so they're returned as-is.
fn aggregate(m1: &[Bar], bucket_secs: i64, now: DateTime<Utc>) -> Vec<Bar> {
    if bucket_secs <= 60 {
        return m1.to_vec();
    }
    let mut out: Vec<Bar> = Vec::new();
    for b in m1 {
        let bucket = (b.time.timestamp() / bucket_secs) * bucket_secs;
        match out.last_mut() {
            Some(last) if last.time.timestamp() == bucket => {
                last.high = last.high.max(b.high);
                last.low = last.low.min(b.low);
                last.close = b.close;
                last.volume += b.volume;
                last.trade_count = Some(last.trade_count.unwrap_or(0) + b.trade_count.unwrap_or(0));
            }
            _ => out.push(Bar {
                time:        Utc.timestamp_opt(bucket, 0).single().unwrap_or(b.time),
                open:        b.open,
                high:        b.high,
                low:         b.low,
                close:       b.close,
                volume:      b.volume,
                vwap:        None, // unused for detection
                trade_count: Some(b.trade_count.unwrap_or(0)),
            }),
        }
    }
    out.retain(|bar| now.timestamp() >= bar.time.timestamp() + bucket_secs);
    out
}

/// Build the fire signal for a completed move→pullback.
#[allow(clippy::too_many_arguments)]
fn make_alert(
    symbol:         &str,
    tf_label:       &str,
    side:           Side,
    rvol:           Option<f64>,
    retrace:        f64,
    move_bars:      usize,
    pullback_bars:  usize,
    price:          f64,
    volume_day:     u64,
    change_day_pct: Option<f64>,
    now:            DateTime<Utc>,
) -> AlertSignal {
    let side_str = if matches!(side, Side::Long) { "Long" } else { "Short" };
    let rvol_str = rvol.map(|r| format!(" RVOL ×{r:.1},")).unwrap_or_default();
    AlertSignal {
        alert_id:      format!("pp-{}-{}-{}", now.timestamp_millis(), symbol, tf_label),
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
            "Perfect Pullback {side_str} {tf_label} — Market Attention,{rvol_str} montée \
             {move_bars} barres, pullback {pullback_bars} barres (retracement {:.0}%) — ${:.2}",
            retrace * 100.0,
            price,
        ),
        display_timeframe: Some(tf_label.to_string()),
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
