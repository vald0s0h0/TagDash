// Micro Pullback — multi-tempo "early departure" scanner.
//
// Production v2 goal: detect where <=100M-float US-stock attention is concentrating
// *now*, then keep only a clean upward impulse. Six parallel tempos (10/20/40 s,
// 1/2/5 min) rank flow, ATR-normalised range, local surprise, continuity and
// second-volume distribution across the whole active universe. A Top 10 is always
// observable, but only a detached one-to-three-name head may reach the directional
// gate and then Qualité A+; a flat Top 10 is noise and yields zero candidates.
//
// TEMPO DEPTH. Every active tempo is evaluated, not just the first to trip. The
// fastest arms the watch; the full set travels on the alert as `tempo_depth` and
// keeps widening while the move develops. "10s, 20s and 40s all fired" is a
// materially different signal from "the 10s window spiked", and the trader can only
// weigh that if it is recorded.
//
// The previous absolute-volume/ratio detector and liquidity watch remain below as
// pure legacy fixtures while historical replay tests migrate; `MicroPullbackEngine::
// start` launches `v2` exclusively.
//
// SESSIONS. The engine runs the whole extended day (04:00–20:00 ET) and stamps each
// alert with the session it fired in. The LOCK is one alert per ticker PER SESSION,
// so a name that ran in the premarket is a fresh candidate at the open. Coverage is
// not uniform: Alpaca's market-wide `trades` tier only runs while the tape is thin
// (see `alpaca::stream::broad_mode_at`), so during the regular session that provider
// only feeds the day's watch list. Massive's `A.*` wildcard covers the universe all
// day.
//
// QUALITÉ A+ (opt-in) is preserved as the catalyst layer. Price/VWAP/session-volume/
// split/old-20M-float vetos were removed; the new universe owns selection and A+
// keeps news, offering/dilution, issuer profile and its two LLM passes.
//
// Late start / crash recovery: the live 10s ring is empty until the feed warms up,
// so the baseline is backfilled from 1-minute bars (the smallest history Alpaca REST
// serves; 10s isn't available). The 1-minute medians are scaled to each tempo
// (volume/trades linearly with time, range ~√time); the engine switches to the exact
// live baseline once enough 10s buckets exist. The current window is always live.
//
// Engine, not a `ScanStrategy::should_alert`, because of the per-session LOCK +
// the historical backfill; it pushes AlertSignals via `scanner::push_alert`. The
// registry still carries the metadata `MicroPullback` strategy (card, toggle,
// name, priority) — see `strategies::micro_pullback`.

pub mod intel_live;
pub mod live_status;
pub mod options;
pub mod preselect;
pub mod quality;
mod v2;

#[cfg(test)]
mod tests;

pub use options::MicroPullbackOptions;

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
use crate::types::{AlertSignal, Side, TapeKind};

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
    //    ratios. The volume floor scales with the tempo (×T/10); the rate and the
    //    percentage floors are intensive and stay constant across tempos.
    pub baseline_volume_floor_10s: f64,
    pub baseline_trade_rate_floor: f64,
    pub baseline_range_floor_pct: f64,

    // ── Final gate: the liquidity watch (replaces the per-tempo absolute trade
    //    minimum). When detection (gates 1-3) trips, the ticker is NOT alerted
    //    immediately: it is handed to a watch polled every second for up to
    //    `watch_max_secs`, and fires the instant a layer confirms genuine, sustained
    //    activity — otherwise it is dropped (and becomes eligible for re-detection).
    //
    //    There are TWO variants because the two providers do not deliver the same
    //    tape (see `TapeKind`); `confirm_liquidity` picks one from the live feed:
    pub watch_max_secs: i64,

    //    ── Variant A — PRINTS (Alpaca): a real trade rate is available.
    //       • fast: a very high instantaneous rate fires on the very first second,
    //       • slow: a lower but sustained rate over a longer window.
    pub watch_fast_window_secs: i64,
    pub watch_fast_rate_per_sec: f64,
    pub watch_slow_window_secs: i64,
    pub watch_slow_rate_per_sec: f64,

    //    ── Variant B — SECOND AGGREGATES (Massive): no trade count and no spread,
    //       so liquidity is judged on how *continuously* the name trades and how
    //       many dollars cross, over one sliding window.
    /// Sliding observation window.
    pub watch_agg_window_secs: i64,
    /// Seconds of the window that must have printed. A genuinely liquid name trades
    /// in nearly every second; a dead micro-cap trades in bursts.
    pub watch_agg_active_seconds_min: u32,
    /// Dollars that must cross over the whole window (slow layer).
    pub watch_agg_dollar_volume_min: f64,
    /// One single second this hot confirms immediately (fast layer).
    pub watch_agg_fast_dollar_sec_min: f64,

    //    ── Floor common to BOTH variants ──
    /// 5 prints/s of 10 shares is not liquidity. The prints path had no dollar floor
    /// at all before; on the aggregate path this is what stops a single hot second
    /// from confirming a name that is otherwise dead.
    pub watch_min_dollar_per_sec: f64,

    // ── Multi-tempo signal thresholds (gate 3) — one entry per tempo. ──
    pub tempos: [Tempo; TEMPO_COUNT],
    /// Which tempos are switched on, positionally aligned with `tempos`. Driven by
    /// `MicroPullbackOptions`; all true in `DEFAULT`. Kept as a mask rather than a
    /// `Vec<Tempo>` so `Config` stays a `const` (the flat-files pre-scan and the
    /// tests both depend on that).
    pub tempo_active: [bool; TEMPO_COUNT],
}

pub const TEMPO_COUNT: usize = 6;

/// Per-tempo detection thresholds. A tempo trips when the absolute minimums are met
/// AND at least 2 of the available acceleration ratios clear (see gate 3).
#[derive(Debug, Clone, Copy)]
pub struct Tempo {
    /// Window length in seconds (10/20/40/60/120/300). Must be a multiple of BAR_SECS.
    pub secs: i64,
    pub return_min_pct: f64,
    /// Absolute share minimum. Kept alongside the dollar minimum because a very
    /// cheap name can clear dollars on a share count that is still untradeable.
    pub volume_min: f64,
    /// Absolute dollar minimum. 10 000 shares is $10.5k at $1.05 and $240k at $24 —
    /// a factor 23 across the same price band, which the share minimum alone cannot
    /// express. Both are required.
    pub dollar_volume_min: f64,
    pub volume_ratio_min: f64,
    pub trade_rate_ratio_min: f64,
    pub range_ratio_min: f64,
}

impl Config {
    pub const DEFAULT: Config = Config {
        price_min: 1.0,
        price_max: 25.0,
        float_max: 30_000_000,
        allow_unknown_float: true,

        baseline_volume_floor_10s: 500.0,
        baseline_trade_rate_floor: 0.2,
        baseline_range_floor_pct: 0.10,

        watch_max_secs: 60,
        watch_fast_window_secs: 1,
        watch_fast_rate_per_sec: 5.0,
        watch_slow_window_secs: 5,
        watch_slow_rate_per_sec: 3.0,

        watch_agg_window_secs: 10,
        watch_agg_active_seconds_min: 6,
        watch_agg_dollar_volume_min: 30_000.0,
        watch_agg_fast_dollar_sec_min: 20_000.0,

        watch_min_dollar_per_sec: 3_000.0,

        tempos: [
            Tempo {
                secs: 10,
                return_min_pct: 1.0,
                volume_min: 10_000.0,
                dollar_volume_min: 25_000.0,
                volume_ratio_min: 8.0,
                trade_rate_ratio_min: 8.0,
                range_ratio_min: 4.0,
            },
            Tempo {
                secs: 20,
                return_min_pct: 1.8,
                volume_min: 20_000.0,
                dollar_volume_min: 45_000.0,
                volume_ratio_min: 7.0,
                trade_rate_ratio_min: 7.0,
                range_ratio_min: 4.0,
            },
            Tempo {
                secs: 40,
                return_min_pct: 2.8,
                volume_min: 35_000.0,
                dollar_volume_min: 75_000.0,
                volume_ratio_min: 6.0,
                trade_rate_ratio_min: 6.0,
                range_ratio_min: 3.5,
            },
            Tempo {
                secs: 60,
                return_min_pct: 3.5,
                volume_min: 50_000.0,
                dollar_volume_min: 100_000.0,
                volume_ratio_min: 5.0,
                trade_rate_ratio_min: 5.0,
                range_ratio_min: 3.0,
            },
            Tempo {
                secs: 120,
                return_min_pct: 5.0,
                volume_min: 90_000.0,
                dollar_volume_min: 175_000.0,
                volume_ratio_min: 4.0,
                trade_rate_ratio_min: 4.0,
                range_ratio_min: 2.5,
            },
            Tempo {
                secs: 300,
                return_min_pct: 7.0,
                volume_min: 180_000.0,
                dollar_volume_min: 350_000.0,
                volume_ratio_min: 3.0,
                trade_rate_ratio_min: 3.0,
                range_ratio_min: 2.0,
            },
        ],
        tempo_active: [true; TEMPO_COUNT],
    };

    /// The switched-on tempos, ascending.
    fn active_tempos(&self) -> impl Iterator<Item = &Tempo> {
        self.tempos
            .iter()
            .zip(self.tempo_active)
            .filter_map(|(t, on)| on.then_some(t))
    }

    /// Apply the user's Settings choices. Only the tempo mask is user-driven today;
    /// the thresholds themselves stay compiled.
    fn with_options(mut self, opts: &MicroPullbackOptions) -> Self {
        let enabled = opts.enabled_tempos();
        for (i, t) in self.tempos.iter().enumerate() {
            self.tempo_active[i] = enabled.contains(&t.secs);
        }
        self
    }
}

// ─── Engine-loop constants (not detection thresholds) ──────────────────────────
/// 1-second cadence: detection is throttled to once per freshly-closed 10s bar, but
/// the final tape-rate watch is polled every second (see `Config::watch_*`).
const LOOP_INTERVAL_SECS: u64 = 1;
const FLOAT_REFRESH_SECS: u64 = 300;
/// Active window in ET wall-clock minutes since midnight: 04:00–20:00, i.e. the
/// whole extended trading day (premarket → pre-open → open → afterhours).
///
/// It used to be premarket only, matching the Alpaca `trades` broad tier. That tier
/// is still premarket/afterhours only — see `alpaca::stream::broad_mode_at` — so
/// during the regular session this engine sees 10s bars for the day's watch list
/// rather than the whole market on that provider. On Massive the `A.*` wildcard
/// covers the universe all day.
const SESSION_START_MIN: u32 = 240;
const SESSION_END_MIN: u32 = 1200;
/// Detection timeframe + bucket length.
const BAR_TF: Timeframe = Timeframe::S10;
const BAR_SECS: i64 = 10;
/// Rolling baseline length (the "last 5 minutes") for the short tempos.
const BASELINE_WINDOW_SECS: i64 = 300;

/// Baseline length for a given tempo. A fixed 5 minutes would yield a SINGLE bucket
/// on the 5-minute tempo, below `MIN_BASELINE_BUCKETS`, so that tempo would fall
/// back to the 1-minute seed forever and never use a live baseline. The window
/// therefore stretches to always hold at least the minimum number of buckets:
///   10s/20s/40s/60s → 300s · 120s → 360s · 300s → 900s
fn baseline_secs(tempo_secs: i64) -> i64 {
    (tempo_secs * MIN_BASELINE_BUCKETS as i64).max(BASELINE_WINDOW_SECS)
}
/// Minimum bucket count for a robust live baseline median; below this a tempo falls
/// back to the 1-minute seed (or is skipped if there's no seed yet).
const MIN_BASELINE_BUCKETS: usize = 3;
/// Admit a candidate from this many live 10s bars (at least one full 10s current
/// window + something to anchor; the absolute gate-3 minimums reject true noise).
const MIN_CANDIDATE_BARS: usize = 1;
/// Drop an UNLOCKED machine that hasn't been evaluated in this long (memory bound);
/// locked machines are kept until the session reset so they can't re-alert.
const MACHINE_STALE_SECS: i64 = 20 * 60;
/// How long after its alert a locked ticker keeps being evaluated purely to widen
/// its tempo-depth list (stage 3 of `evaluate`).
const DEPTH_TRACK_SECS: i64 = 15 * 60;
/// Cap on depth-driven republishes per ticker, so a runner that keeps tripping
/// tempos all morning can't churn the active alert list.
const MAX_DEPTH_REPUBLISH: u8 = 5;

// ── 1-minute backfill seed (late-start / crash recovery) ───────────────────────
/// A candidate is seed-eligible once it has at least this many live 10s bars (so we
/// only backfill genuinely-active young names, not one-off prints).
const MIN_SEED_LIVE_BARS: usize = 2;
/// How far back a tempo needs live history to be judged without a seed: its own
/// window, plus its baseline, plus a 1-minute cushion. The longest ACTIVE tempo
/// decides — with the 5-minute tempo on that is 300 + 900 + 60 = 21 minutes, so
/// that tempo cannot fire before ~04:21 ET on live bars alone; until then the
/// 1-minute seed carries it.
fn full_history_secs(cfg: &Config) -> i64 {
    cfg.active_tempos()
        .map(|t| t.secs + baseline_secs(t.secs))
        .max()
        .unwrap_or(BASELINE_WINDOW_SECS)
        + 60
}
/// Enough live 10s bars to cover the above → the seed is no longer needed.
fn full_live_bars(cfg: &Config) -> usize {
    (full_history_secs(cfg) / BAR_SECS) as usize
}
/// How far back the 1-minute backfill reaches.
fn seed_lookback_secs(cfg: &Config) -> i64 {
    full_history_secs(cfg)
}
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
    locked: bool,
    last_eval_bar: Option<DateTime<Utc>>,
    watch: Option<Watch>,
    /// Every tempo that has tripped for this ticker this session, ascending. Keeps
    /// growing after the lock (see stage 3 of `evaluate`) because "the 10s, 20s and
    /// 40s windows all fired" is a materially different signal from "only the 10s
    /// spiked", and the trader can only see that if it is recorded.
    depth: Vec<i64>,
    /// What was published, so a later tempo re-renders that same alert rather than
    /// creating a second one.
    emitted: Option<Emitted>,
}

impl Machine {
    fn new() -> Self {
        Self {
            locked: false,
            last_eval_bar: None,
            watch: None,
            depth: Vec::new(),
            emitted: None,
        }
    }

    /// Whether a locked ticker is still worth evaluating — i.e. its depth-tracking
    /// window is open and it hasn't exhausted its republish budget.
    fn tracking_depth(&self, now: DateTime<Utc>) -> bool {
        match &self.emitted {
            Some(e) => {
                e.republished < MAX_DEPTH_REPUBLISH
                    && (now - e.timestamp).num_seconds() <= DEPTH_TRACK_SECS
            }
            None => false,
        }
    }
}

/// The alert this machine published, kept verbatim so a depth update re-renders it
/// with the SAME `alert_id` and timestamp. A new id would read as a second alert:
/// the sidebar auto-places every unseen `alert_id` into a zone, so re-rendering with
/// a fresh one would hijack a second chart for a ticker already on screen.
#[derive(Debug, Clone)]
struct Emitted {
    alert_id: String,
    timestamp: DateTime<Utc>,
    tempo: Tempo,
    metrics: TempoMetrics,
    proof: LiquidityProof,
    news: String,
    news_today: bool,
    republished: u8,
}

/// Final-gate state: detection tripped at `started`; we now poll the live tape rate
/// every second until a layer confirms or `watch_max_secs` elapses. The triggering
/// tempo + metrics are kept so the eventual alert still describes the departure.
#[derive(Debug, Clone, Copy)]
struct Watch {
    started: DateTime<Utc>,
    tempo: Tempo,
    metrics: TempoMetrics,
}

/// What the tape looked like over one trailing window. One shape for both feeds:
/// each provider fills what it can, and `confirm_liquidity` only reads the fields
/// its variant is allowed to trust.
#[derive(Debug, Clone, Copy, Default)]
pub struct TapeWindow {
    /// Trade prints. Only a real trade count on the `Prints` tape — on the
    /// aggregate tape every second counts as exactly one "print".
    pub prints: u64,
    /// Distinct seconds that printed. Means the same thing on both feeds.
    pub active_seconds: u32,
    /// Dollars traded over the window, accumulated print by print.
    pub dollars: f64,
    /// Dollars traded in the busiest single second of the window.
    pub max_second_dollars: f64,
}

/// What the final gate actually measured when it confirmed. Carried into the alert
/// text and the A+ payload so the reason a ticker passed is always inspectable.
#[derive(Debug, Clone, Copy)]
pub struct LiquidityProof {
    pub source: TapeKind,
    /// Prints per second. None on the aggregate tape, where no trade rate exists.
    pub rate_per_sec: Option<f64>,
    pub dollar_per_sec: f64,
    /// Seconds that printed over `window_secs`. Filled on the aggregate tape, where
    /// continuity is what replaces the trade rate.
    pub active_seconds: Option<u32>,
    pub window_secs: i64,
}

/// Final gate — is this thing actually liquid *right now*?
///
/// Two variants, because the two providers do not deliver the same tape:
///   • `Prints` (Alpaca): a genuine trade rate. A very high instantaneous rate over
///     1s fires immediately; failing that, a sustained rate over 5s does.
///   • `SecondAggregates` (Massive): no trade count, no spread. Liquidity is judged
///     on how *continuously* the name trades over a 10s sliding window plus how
///     many dollars cross. One very hot second confirms immediately; failing that,
///     continuity (≥6 of 10 seconds printed) together with the window's dollar
///     total does.
///
/// Both variants must additionally clear `watch_min_dollar_per_sec`. On the prints
/// path that floor is new — 5 prints/s of 10 shares was previously enough. On the
/// aggregate path it is what stops one hot second from confirming a name that is
/// otherwise dead: $20k in a single second inside an otherwise empty 10s window is
/// $2k/s, below the floor.
///
/// `tape_fn` is injected rather than reading `MarketState` directly so the gate
/// stays a pure function the tests can drive.
fn confirm_liquidity(
    cfg: &Config,
    source: TapeKind,
    tape_fn: &dyn Fn(i64) -> TapeWindow,
) -> Option<LiquidityProof> {
    match source {
        TapeKind::Prints => {
            let fast = tape_fn(cfg.watch_fast_window_secs);
            let slow = tape_fn(cfg.watch_slow_window_secs);
            // The dollar floor reads the SLOW window even when the fast layer is what
            // confirms: a 1-second dollar figure is far too jumpy to gate on, and
            // looking back over 5s costs no delay.
            let dollar_per_sec = slow.dollars / cfg.watch_slow_window_secs as f64;
            if dollar_per_sec < cfg.watch_min_dollar_per_sec {
                return None;
            }
            let fast_rate = fast.prints as f64 / cfg.watch_fast_window_secs as f64;
            let slow_rate = slow.prints as f64 / cfg.watch_slow_window_secs as f64;
            let (rate, window) = if fast_rate >= cfg.watch_fast_rate_per_sec {
                (fast_rate, cfg.watch_fast_window_secs)
            } else if slow_rate >= cfg.watch_slow_rate_per_sec {
                (slow_rate, cfg.watch_slow_window_secs)
            } else {
                return None;
            };
            Some(LiquidityProof {
                source,
                rate_per_sec: Some(rate),
                dollar_per_sec,
                active_seconds: None,
                window_secs: window,
            })
        }
        TapeKind::SecondAggregates => {
            let w = cfg.watch_agg_window_secs;
            let win = tape_fn(w);
            let dollar_per_sec = win.dollars / w as f64;
            if dollar_per_sec < cfg.watch_min_dollar_per_sec {
                return None;
            }
            let confirmed = win.max_second_dollars >= cfg.watch_agg_fast_dollar_sec_min
                || (win.active_seconds >= cfg.watch_agg_active_seconds_min
                    && win.dollars >= cfg.watch_agg_dollar_volume_min);
            if !confirmed {
                return None;
            }
            Some(LiquidityProof {
                source,
                rate_per_sec: None,
                dollar_per_sec,
                active_seconds: Some(win.active_seconds),
                window_secs: w,
            })
        }
    }
}

/// 1-minute backfill seed: the per-1-minute medians of the last few minutes, scaled
/// to each tempo on demand (volume/trades linearly with time, range ~√time).
#[derive(Debug, Clone, Copy)]
struct Seed {
    median_volume_1m: f64,
    median_trades_1m: f64,
    median_range_pct_1m: f64,
}

/// Everything the alert needs that isn't a bar — snapshotted under the read lock.
#[derive(Debug, Clone)]
struct Meta {
    price: f64,
    bid: Option<f64>,
    ask: Option<f64>,
    spread: Option<f64>,
    volume_day: u64,
    change_day_pct: Option<f64>,
    float_shares: Option<u64>,
    /// Whether the float is known (drives the FLOAT_UNKNOWN tag).
    float_known: bool,
    /// Session volume vs the symbol's average daily volume. Was always None on this
    /// strategy's alerts, leaving an empty column on the card even though the
    /// universe table has carried `avg_volume` all along.
    rvol: Option<f64>,
}

/// One candidate's per-loop snapshot (read lock held only to build these).
struct Input {
    symbol: String,
    bars: Vec<Bar>,
    meta: Meta,
}

// ─── Engine ────────────────────────────────────────────────────────────────────

pub struct MicroPullbackEngine;

impl MicroPullbackEngine {
    /// Attention-first engine. The legacy detector remains compiled below while
    /// replay fixtures are migrated, but production starts exclusively through v2.
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        running: Arc<AtomicBool>,
        market: Arc<RwLock<MarketState>>,
        db: Arc<Mutex<rusqlite::Connection>>,
        secrets: Arc<RwLock<Secrets>>,
        active_alerts: Arc<RwLock<Vec<AlertSignal>>>,
        alert_history: Arc<RwLock<Vec<AlertSignal>>>,
        strategy_enabled: Arc<RwLock<HashMap<String, bool>>>,
        options: Arc<RwLock<MicroPullbackOptions>>,
        app_config: Arc<RwLock<crate::config::AppConfig>>,
        attention: Arc<RwLock<Vec<crate::types::MicroAttentionEntry>>>,
    ) {
        v2::start(
            running,
            market,
            db,
            secrets,
            active_alerts,
            alert_history,
            strategy_enabled,
            options,
            app_config,
            attention,
        );
    }

    /// Spawn the background loop. Returns immediately.
    #[allow(clippy::too_many_arguments)]
    #[allow(dead_code)]
    fn start_legacy(
        running: Arc<AtomicBool>,
        market: Arc<RwLock<MarketState>>,
        db: Arc<Mutex<rusqlite::Connection>>,
        secrets: Arc<RwLock<Secrets>>,
        active_alerts: Arc<RwLock<Vec<AlertSignal>>>,
        alert_history: Arc<RwLock<Vec<AlertSignal>>>,
        strategy_enabled: Arc<RwLock<HashMap<String, bool>>>,
        options: Arc<RwLock<MicroPullbackOptions>>,
        app_config: Arc<RwLock<crate::config::AppConfig>>,
    ) {
        // Tauri-managed runtime so it can be launched from the sync `setup` hook.
        tauri::async_runtime::spawn(async move {
            let mut machines: HashMap<String, Machine> = HashMap::new();
            let (mut floats, mut avg_volumes) = load_universe_meta(&db);
            let mut floats_loaded = std::time::Instant::now();
            // ET day + session segment the state belongs to. Crossing either boundary
            // resets everything (states, locks, seeds): the LOCK is per session, so a
            // ticker that ran in the premarket is eligible again at the open.
            let mut session_key: Option<String> = None;
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
                        session_key = None;
                        // Replaying the same session twice must be able to push twice,
                        // otherwise testing the phone pipeline in replay silently does
                        // nothing from the second run on.
                        crate::ntfy::reset_sent();
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

                // Settings are re-read every tick so a toggle lands on the next second
                // (same contract as `strategy_enabled` above).
                let opts = *options.read().unwrap();
                let cfg = Config::DEFAULT.with_options(&opts);
                // Every tempo switched off: a legitimate way to mute detection without
                // disabling the strategy. Nothing to evaluate.
                if opts.longest_tempo().is_none() {
                    crate::replay::clock::scaled_sleep(LOOP_INTERVAL_SECS * 1000).await;
                    continue;
                }

                if floats_loaded.elapsed() >= Duration::from_secs(FLOAT_REFRESH_SECS) {
                    (floats, avg_volumes) = load_universe_meta(&db);
                    floats_loaded = std::time::Instant::now();
                }

                // App clock: simulated instant during a Market Replay.
                let now = crate::time::now();
                if !in_active_session(now) {
                    if !machines.is_empty() || !seeds.is_empty() {
                        machines.clear();
                        seeds.clear();
                        session_key = None;
                    }
                    crate::replay::clock::scaled_sleep(LOOP_INTERVAL_SECS * 1000).await;
                    continue;
                }

                // New ET day OR new session segment → reset all per-session state. The
                // LOCK is one alert per ticker *per session*: a name that ran in the
                // premarket is a fresh candidate again at the open, which is usually
                // when the real move happens.
                let key = format!(
                    "{}#{:?}",
                    crate::time::et_date(now),
                    crate::time::session_at(now)
                );
                if session_key.as_deref() != Some(key.as_str()) {
                    machines.clear();
                    seeds.clear();
                    session_key = Some(key);
                }

                // Which tape is feeding us right now — decides the liquidity variant
                // and whether a trade-rate ratio exists at all.
                let tape_source = market.read().unwrap().tape_kind();

                // Snapshot candidate inputs under one brief read lock. Gate 1
                // universe: price in band, float ≤ max (or unknown when allowed),
                // and at least MIN_CANDIDATE_BARS of live history.
                //
                // Locked tickers are NO LONGER skipped: they are still evaluated so a
                // later tempo can widen their depth list (stage 3 of `evaluate`), but
                // only while that tracking window is open — after it closes they cost
                // nothing again.
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
                            if let Some(m) = machines.get(&t.symbol) {
                                if m.locked && !m.tracking_depth(now) {
                                    return None;
                                }
                            }
                            let machine = machines.get(&t.symbol);
                            let watching = machine.and_then(|m| m.watch).is_some();
                            let latest = ms.latest_closed_bar_time(&t.symbol, BAR_TF)?;
                            // Detection is defined on freshly-closed 10s bars. Avoid
                            // cloning and de-duplicating the full ring on nine of ten
                            // loop ticks where its watermark did not change. A live
                            // liquidity watch still needs a lightweight input every
                            // second, but stage 2 never reads its bars.
                            if !watching && machine.and_then(|m| m.last_eval_bar) == Some(latest) {
                                return None;
                            }
                            let bars = if watching {
                                Vec::new()
                            } else {
                                let bars = ms.closed_bars(&t.symbol, BAR_TF);
                                if bars.len() < MIN_CANDIDATE_BARS {
                                    return None;
                                }
                                bars
                            };
                            let avg_vol = avg_volumes.get(&t.symbol).copied().filter(|v| *v > 0.0);
                            Some(Input {
                                symbol: t.symbol.clone(),
                                bars,
                                meta: Meta {
                                    price,
                                    bid: t.bid,
                                    ask: t.ask,
                                    spread: t.spread,
                                    volume_day: t.volume_day,
                                    change_day_pct: t.change_day_pct,
                                    float_shares: float,
                                    float_known: float.is_some(),
                                    rvol: avg_vol.map(|a| t.volume_day as f64 / a),
                                },
                            })
                        })
                        .collect()
                };

                // ── 1-minute backfill: seed the baseline for active young names that
                // don't yet have a full live window. Only once enough session history
                // exists to backfill, each symbol attempted at most once, paced.
                // Never during a Market Replay either: the replay feed injects the
                // whole session from 04:00, so the live rings already hold the full
                // baseline — and a REST seed would be redundant.
                //
                // The reach scales with the LONGEST ACTIVE tempo: with the 5-minute
                // tempo on, a full live baseline is 300s + 900s of history, so the
                // seed has to cover ~21 minutes rather than the old fixed 6.
                let lookback = seed_lookback_secs(&cfg);
                let full_bars = full_live_bars(&cfg);
                let seed_window_open = !crate::replay::clock::is_active()
                    && crate::time::et_minutes(now)
                        >= SESSION_START_MIN + (BASELINE_WINDOW_SECS / 60) as u32;
                let seed_due = last_seed_attempt
                    .map(|t| t.elapsed() >= Duration::from_secs(SEED_ATTEMPT_INTERVAL_SECS))
                    .unwrap_or(true);
                if seed_window_open && seed_due {
                    let to_seed: Vec<String> = inputs
                        .iter()
                        .filter(|inp| {
                            inp.bars.len() >= MIN_SEED_LIVE_BARS
                                && inp.bars.len() < full_bars
                                && !seeds.contains_key(&inp.symbol)
                        })
                        .map(|inp| inp.symbol.clone())
                        .take(SEED_BATCH_MAX)
                        .collect();
                    if !to_seed.is_empty() {
                        last_seed_attempt = Some(std::time::Instant::now());
                        let creds = {
                            let s = secrets.read().unwrap();
                            (s.alpaca_key.clone(), s.alpaca_secret.clone())
                        };
                        if let (Some(k), Some(sc)) = creds {
                            if !k.is_empty() && !sc.is_empty() {
                                let start = (now - ChronoDuration::seconds(lookback))
                                    .format("%Y-%m-%dT%H:%M:%SZ")
                                    .to_string();
                                match crate::alpaca::bars::fetch_minute_bars_since(
                                    &k, &sc, &to_seed, &start,
                                )
                                .await
                                {
                                    Ok(map) => {
                                        // A successful request with no row for a
                                        // symbol is a real "no usable history" result;
                                        // remember it so a quiet name is not fetched
                                        // again every five seconds.
                                        for sym in &to_seed {
                                            seeds.entry(sym.clone()).or_insert(None);
                                        }
                                        for (sym, mbars) in map {
                                            if let Some(s) = seed_from_minutes(&mbars) {
                                                seeds.insert(sym, Some(s));
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        // Do not cache a transient request failure as
                                        // "this symbol has no baseline" for the rest
                                        // of the session. The paced batch will retry.
                                        eprintln!(
                                            "[tagdash] micro_pullback: seed backfill failed: {e}"
                                        );
                                    }
                                }
                            }
                        }
                    }
                }

                // News resolver (informational only — never required, never blocks).
                // Read lazily at fire time; injected so detection stays unit-testable.
                let news_fn = |sym: &str| news_tag(&market, sym, now);

                let mut fires: Vec<Outcome> = Vec::new();
                for inp in &inputs {
                    let seed = seeds.get(&inp.symbol).and_then(|o| o.as_ref());
                    let m = machines
                        .entry(inp.symbol.clone())
                        .or_insert_with(Machine::new);
                    // Live tape resolver for the final gate: everything the two
                    // liquidity variants can ask about one trailing window. A symbol
                    // that has never printed reads as an empty window, not an error.
                    let tape_fn = |secs: i64| {
                        let ms = market.read().unwrap();
                        let s = ms
                            .tape_window_stats(&inp.symbol, secs, now)
                            .unwrap_or_default();
                        TapeWindow {
                            prints: s.prints,
                            active_seconds: s.active_seconds,
                            dollars: s.dollars,
                            max_second_dollars: s.max_second_dollars,
                        }
                    };
                    if let Some(outcome) =
                        evaluate(m, inp, &cfg, now, seed, tape_source, &news_fn, &tape_fn)
                    {
                        fires.push(outcome);
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

                for outcome in fires {
                    match outcome {
                        // A+ off: straight to the screen, exactly as before — plus
                        // the phone, if asked for. With A+ ON this call belongs to the
                        // qualification task instead, which only rings for a candidate
                        // that actually cleared the bar.
                        Outcome::Fired { alert, .. } if !opts.quality_aplus => {
                            if opts.ntfy_alerts {
                                crate::ntfy::notify_alert_once(&secrets, &alert);
                            }
                            push_alert(&active_alerts, &alert_history, alert);
                        }
                        // A+ on: the qualification decides whether this reaches the
                        // screen, and owns the row from here on. It runs in its own
                        // task — the loop ticks every second and must never wait on an
                        // API call.
                        Outcome::Fired {
                            alert,
                            tempo,
                            metrics,
                            proof,
                            depth,
                        } => {
                            let cand = quality::Candidate {
                                alert,
                                tempo_secs: tempo.secs,
                                tempo_depth: depth,
                                price_return_pct: metrics.price_return,
                                volume_ratio: metrics.volume_ratio,
                                trade_rate_ratio: metrics.trade_rate_ratio,
                                range_ratio: metrics.range_ratio,
                                tape_source,
                                tape_rate_per_sec: proof.rate_per_sec,
                                tape_dollar_per_sec: proof.dollar_per_sec,
                                tape_active_seconds: proof.active_seconds,
                                tape_window_secs: proof.window_secs,
                                detected_at: now,
                                // The legacy engine has no pre-selection gate; it is
                                // dead code kept for the historical replay fixtures.
                                setup: Default::default(),
                            };
                            let deps = quality::Deps {
                                db: db.clone(),
                                config: app_config.clone(),
                                secrets: secrets.clone(),
                                market: market.clone(),
                                active_alerts: active_alerts.clone(),
                                alert_history: alert_history.clone(),
                            };
                            tauri::async_runtime::spawn(quality::run_qualification(
                                deps, cand, opts,
                            ));
                        }
                        // Under A+ the qualification task owns the row and re-publishes
                        // it on every score; pushing the raw alert here would race it
                        // and strip the A+ prefix back off. The depth known AT FIRE TIME
                        // is still carried into the qualification — only later widening
                        // is dropped, which is the cheaper of the two wrong answers.
                        Outcome::DepthUpdated(_) if opts.quality_aplus => {}
                        Outcome::DepthUpdated(alert) => {
                            push_alert(&active_alerts, &alert_history, alert);
                        }
                    }
                }

                crate::replay::clock::scaled_sleep(LOOP_INTERVAL_SECS * 1000).await;
            }
        });
    }
}

/// Evaluate one ticker. Three stages:
///   1. Detection (gates 1-3, throttled to once per freshly-closed 10s bar). EVERY
///      active tempo is evaluated, not just the first to trip: the fastest one arms
///      the liquidity watch, and the full set is recorded in `Machine::depth`. A
///      move that trips 10s/20s/40s has visibly more behind it than one that only
///      spikes the 10s window, and that is exactly what the trader needs to see.
///   2. Final gate (every tick while armed): `confirm_liquidity` against the live
///      tape; fire + lock the instant a layer confirms, or drop the watch once it
///      has run past `watch_max_secs`.
///   3. Depth tracking (after the lock, for `DEPTH_TRACK_SECS`): a locked ticker is
///      still evaluated, but only to widen its depth list. When it does, the SAME
///      alert is re-rendered and re-pushed — same `alert_id`, same timestamp — so
///      `push_alert`'s (symbol, strategy) dedup updates the row in place instead of
///      creating a second one.
///
/// Arming and confirming may happen on the same tick (an already-hot ticker fires
/// immediately). `news_fn`/`tape_fn` are injected so this stays a pure function the
/// tests can drive over synthetic bars.
fn evaluate(
    m: &mut Machine,
    inp: &Input,
    cfg: &Config,
    now: DateTime<Utc>,
    seed: Option<&Seed>,
    source: TapeKind,
    news_fn: &dyn Fn(&str) -> (String, bool),
    tape_fn: &dyn Fn(i64) -> TapeWindow,
) -> Option<Outcome> {
    // ── Stage 1: detection. Runs for locked tickers too — for them it only feeds
    // the depth list. Skipped while a watch is already running (a watch lasts at
    // most a minute), and throttled to one evaluation per freshly-closed 10s bar.
    let mut depth_grew = false;
    if m.watch.is_none() {
        if let Some(latest) = inp.bars.last().map(|b| b.time) {
            if m.last_eval_bar != Some(latest) {
                m.last_eval_bar = Some(latest);
                let mut fastest: Option<(Tempo, TempoMetrics)> = None;
                for tempo in cfg.active_tempos() {
                    let Some(metrics) = eval_tempo(&inp.bars, tempo, cfg, seed, source) else {
                        continue;
                    };
                    if !gate3_trips(&metrics, tempo) {
                        continue;
                    }
                    if !m.depth.contains(&tempo.secs) {
                        m.depth.push(tempo.secs);
                        depth_grew = true;
                    }
                    if fastest.is_none() {
                        fastest = Some((*tempo, metrics));
                    }
                }
                m.depth.sort_unstable();
                // Only an unlocked ticker arms; a locked one is merely recording how
                // deep the move turned out to be.
                if !m.locked {
                    if let Some((tempo, metrics)) = fastest {
                        m.watch = Some(Watch {
                            started: now,
                            tempo,
                            metrics,
                        });
                    }
                }
            }
        }
    }

    // ── Stage 3: already alerted — republish only when the depth widened. ──
    if m.locked {
        return depth_grew
            .then(|| republish_with_depth(m, inp, now))
            .flatten()
            .map(Outcome::DepthUpdated);
    }

    // ── Stage 2: liquidity watch (final gate), polled every tick. ──
    if let Some(w) = m.watch {
        // Timed out unconfirmed → drop the watch; the ticker stays re-eligible.
        if (now - w.started).num_seconds() > cfg.watch_max_secs {
            m.watch = None;
            return None;
        }
        if let Some(proof) = confirm_liquidity(cfg, source, tape_fn) {
            let (news, news_today) = news_fn(&inp.symbol);
            let emitted = Emitted {
                alert_id: format!("mp-{}-{}", now.timestamp_millis(), inp.symbol),
                timestamp: now,
                tempo: w.tempo,
                metrics: w.metrics,
                proof,
                news,
                news_today,
                republished: 0,
            };
            let alert = make_alert(inp, &emitted, &m.depth);
            m.locked = true;
            m.watch = None;
            m.emitted = Some(emitted);
            return Some(Outcome::Fired {
                alert,
                tempo: w.tempo,
                metrics: w.metrics,
                proof,
                depth: m.depth.clone(),
            });
        }
    }
    None
}

/// What one evaluation produced.
///
/// The distinction matters because only a FRESH fire is worth a qualification: a
/// depth update is the same event described more completely, and re-running the LLM
/// on it would spend a second call to reach the same verdict.
enum Outcome {
    /// The liquidity gate just confirmed a new departure.
    Fired {
        alert: AlertSignal,
        tempo: Tempo,
        metrics: TempoMetrics,
        proof: LiquidityProof,
        depth: Vec<i64>,
    },
    /// An already-published alert, re-rendered because another tempo tripped.
    DepthUpdated(AlertSignal),
}

/// Re-render the alert a locked ticker already published, with its widened depth
/// list. Bounded two ways: only for `DEPTH_TRACK_SECS` after the alert, and at most
/// `MAX_DEPTH_REPUBLISH` times — a runner that keeps tripping tempos all morning
/// must not churn the active list indefinitely.
fn republish_with_depth(m: &mut Machine, inp: &Input, now: DateTime<Utc>) -> Option<AlertSignal> {
    let e = m.emitted.as_mut()?;
    if e.republished >= MAX_DEPTH_REPUBLISH || (now - e.timestamp).num_seconds() > DEPTH_TRACK_SECS
    {
        return None;
    }
    e.republished += 1;
    Some(make_alert(inp, e, &m.depth))
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

/// Metrics for one tempo: the current T-second window vs its rolling baseline.
#[derive(Debug, Clone, Copy)]
struct TempoMetrics {
    current_volume: f64,
    current_dollars: f64,
    current_trades: u64,
    price_return: f64,
    volume_ratio: f64,
    /// None on the aggregate tape: a Massive `A` bar's "trade count" is really a
    /// count of seconds, so a rate built from it would be a fabricated number.
    /// Explicitly absent rather than silently 0.0 — gate 3 then judges on the two
    /// ratios that remain, and both must clear.
    trade_rate_ratio: Option<f64>,
    range_ratio: f64,
}

/// Build the metrics for `tempo` from the live 10s bars: the current T-second window
/// (gate 3 numerator) and the preceding baseline (gate 2 denominator, live buckets
/// if enough exist, else the 1-minute seed). None when the current window isn't
/// fully covered or no baseline is available.
///
/// `source` decides whether a trade-rate ratio can be computed at all — see
/// `TempoMetrics::trade_rate_ratio`.
fn eval_tempo(
    bars: &[Bar],
    tempo: &Tempo,
    cfg: &Config,
    seed: Option<&Seed>,
    source: TapeKind,
) -> Option<TempoMetrics> {
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
    let (cur_vol, cur_dollars, cur_trades, cur_range_pct, cur_return) = window_metrics(current);
    let cur_trade_rate = cur_trades as f64 / t as f64;

    // Baseline = the window before the current one, bucketed per tempo. Its length
    // scales with the tempo so even the 5-minute tempo gets enough buckets.
    let base_lo = cur_cutoff - ChronoDuration::seconds(baseline_secs(t));
    let base_start = bars.partition_point(|b| b.time < base_lo);
    let baseline_bars = &bars[base_start..cur_start];
    let (base_vol, base_trades, base_range_pct) = match live_baseline(baseline_bars, t) {
        Some(b) => b,
        None => seed_baseline(seed?, t),
    };
    let base_trade_rate = base_trades / t as f64;

    // Floors (the volume floor scales with the tempo; rate/% floors are intensive).
    let scale = t as f64 / 10.0;
    let vol_floor = cfg.baseline_volume_floor_10s * scale;
    let rate_floor = cfg.baseline_trade_rate_floor;
    let range_floor = cfg.baseline_range_floor_pct;

    Some(TempoMetrics {
        current_volume: cur_vol,
        current_dollars: cur_dollars,
        current_trades: cur_trades,
        price_return: cur_return,
        volume_ratio: cur_vol / base_vol.max(vol_floor),
        trade_rate_ratio: match source {
            TapeKind::Prints => Some(cur_trade_rate / base_trade_rate.max(rate_floor)),
            TapeKind::SecondAggregates => None,
        },
        range_ratio: cur_range_pct / base_range_pct.max(range_floor),
    })
}

/// Gate 3 — a tempo trips when the absolute minimums are met AND at least 2 of the
/// AVAILABLE acceleration ratios clear their thresholds.
///
/// Three ratios are available on the prints tape (volume, trade rate, range) and the
/// 2-of-3 rule keeps the gate loose enough to catch a start while rejecting a
/// one-dimensional spike. On the aggregate tape there is no trade rate, so only two
/// ratios exist and "at least 2 of the available" means BOTH must clear — a
/// deliberately stricter reading, since the missing dimension is exactly the one
/// that distinguishes real participation from a single large print.
///
/// The absolute trade-count minimum was removed in favour of the final liquidity
/// watch (see `evaluate`). The two absolute minimums that remain are the share
/// count and the dollar amount, and both are required: shares alone let a $1 name
/// through on $10k, dollars alone let a $24 name through on 4 000 shares.
fn gate3_trips(mtr: &TempoMetrics, tempo: &Tempo) -> bool {
    if mtr.price_return < tempo.return_min_pct
        || mtr.current_volume < tempo.volume_min
        || mtr.current_dollars < tempo.dollar_volume_min
    {
        return false;
    }
    let mut cleared = 0;
    if mtr.volume_ratio >= tempo.volume_ratio_min {
        cleared += 1;
    }
    if mtr.range_ratio >= tempo.range_ratio_min {
        cleared += 1;
    }
    if let Some(r) = mtr.trade_rate_ratio {
        if r >= tempo.trade_rate_ratio_min {
            cleared += 1;
        }
    }
    cleared >= 2
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
            let (vol, _dollars, trades, range_pct, _ret) = window_metrics_refs(group);
            (vol, trades as f64, range_pct)
        })
        .collect()
}

/// (volume, dollar_volume, trade_count, range_pct, return_pct) over a slice of
/// contiguous bars. Dollars use each bar's VWAP when it has one (falling back to the
/// close), which is a truer notional than `total_shares × last_price` when the price
/// moved across the window.
fn window_metrics(win: &[Bar]) -> (f64, f64, u64, f64, f64) {
    window_metrics_iter(win.iter())
}

fn window_metrics_refs(win: &[&Bar]) -> (f64, f64, u64, f64, f64) {
    window_metrics_iter(win.iter().copied())
}

fn window_metrics_iter<'a>(it: impl Iterator<Item = &'a Bar> + Clone) -> (f64, f64, u64, f64, f64) {
    let mut vol = 0.0;
    let mut dollars = 0.0;
    let mut trades = 0u64;
    let mut high = f64::MIN;
    let mut low = f64::MAX;
    let mut close_sum = 0.0;
    let mut n = 0usize;
    let mut first_open = None;
    let mut last_close = 0.0;
    for b in it {
        vol += b.volume as f64;
        dollars += b.vwap.unwrap_or(b.close) * b.volume as f64;
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
        return (0.0, 0.0, 0, 0.0, 0.0);
    }
    let avg = close_sum / n as f64;
    let range_pct = if avg > 0.0 {
        (high - low) / avg * 100.0
    } else {
        0.0
    };
    let ret = match first_open {
        Some(o) if o > 0.0 => (last_close - o) / o * 100.0,
        _ => 0.0,
    };
    (vol, dollars, trades, range_pct, ret)
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
    let mut trs: Vec<f64> = mbars
        .iter()
        .map(|b| b.trade_count.unwrap_or(0) as f64)
        .collect();
    let mut rngs: Vec<f64> = mbars
        .iter()
        .map(|b| {
            let avg = (b.high + b.low) / 2.0;
            if avg > 0.0 {
                (b.high - b.low) / avg * 100.0
            } else {
                0.0
            }
        })
        .collect();
    Some(Seed {
        median_volume_1m: median(&mut vols),
        median_trades_1m: median(&mut trs),
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

/// The engine's active window: the whole extended trading day, 04:00–20:00 ET.
fn in_active_session(now: DateTime<Utc>) -> bool {
    (SESSION_START_MIN..SESSION_END_MIN).contains(&crate::time::et_minutes(now))
}

/// Informational news tag (never required, never blocks): NEWS « headline » when a
/// live headline exists for the symbol, else TAPE_ONLY. Returns (label, news_today).
fn news_tag(
    market: &Arc<RwLock<MarketState>>,
    symbol: &str,
    _now: DateTime<Utc>,
) -> (String, bool) {
    match market.read().unwrap().latest_news(symbol) {
        Some(n) => (format!("NEWS « {} »", truncate(&n.headline, 60)), true),
        None => ("TAPE_ONLY".to_string(), false),
    }
}

// ─── Alert construction ────────────────────────────────────────────────────────

/// Render (or re-render) the alert for a ticker the final gate confirmed.
///
/// Pure in `inp`, `e` and `depth`, and it reuses `e.alert_id`/`e.timestamp` — so
/// calling it again after the depth widened updates the row on screen instead of
/// creating a second one.
fn make_alert(inp: &Input, e: &Emitted, depth: &[i64]) -> AlertSignal {
    let float_tag = if inp.meta.float_known {
        ""
    } else {
        " · FLOAT_UNKNOWN"
    };
    // Liquidity reads differently per feed: a trade rate where one exists, tape
    // continuity where it doesn't. Both always report dollars/second.
    let liquidity = match (e.proof.rate_per_sec, e.proof.active_seconds) {
        (Some(rate), _) => format!("tape {rate:.1} tr/s"),
        (None, Some(active)) => {
            format!("tape {}/{}s actives", active, e.proof.window_secs)
        }
        (None, None) => "tape —".to_string(),
    };
    // Only worth showing once more than one tempo has fired: "10s" alone is already
    // in the "départ 10s" prefix.
    let depth_tag = if depth.len() > 1 {
        format!(
            " · tempos {}",
            depth
                .iter()
                .map(|s| fmt_tempo(*s))
                .collect::<Vec<_>>()
                .join("/")
        )
    } else {
        String::new()
    };
    // The trade-rate ratio and the raw trade count only exist on the prints tape;
    // on second aggregates they are marked absent rather than printed as zero.
    let (trade_ratio, trade_count) = match e.metrics.trade_rate_ratio {
        Some(r) => (
            format!("trades ×{r:.0}"),
            format!("{} trades / ", e.metrics.current_trades),
        ),
        None => ("trades n/d".to_string(), String::new()),
    };
    let reason = format!(
        "Micro Pullback — départ {} : +{:.1}% (vol ×{:.0}, {}, range ×{:.0}){} \
         · {} · {:.0} k$/s · {}{:.0} sh / {:.0} k$ · {}{} — ${:.2}",
        fmt_tempo(e.tempo.secs),
        e.metrics.price_return,
        e.metrics.volume_ratio,
        trade_ratio,
        e.metrics.range_ratio,
        depth_tag,
        liquidity,
        e.proof.dollar_per_sec / 1_000.0,
        trade_count,
        e.metrics.current_volume,
        e.metrics.current_dollars / 1_000.0,
        e.news,
        float_tag,
        inp.meta.price,
    );

    AlertSignal {
        alert_id: e.alert_id.clone(),
        timestamp: e.timestamp,
        symbol: inp.symbol.clone(),
        strategy_id: STRATEGY_ID.to_string(),
        strategy_name: "Micro Pullback".to_string(),
        priority: 5,
        // The engine now runs the whole extended day, so the alert carries the
        // session it actually fired in and lands in that tab.
        session: crate::time::session_at(e.timestamp),
        price: Some(inp.meta.price),
        bid: inp.meta.bid,
        ask: inp.meta.ask,
        spread: inp.meta.spread,
        volume: Some(inp.meta.volume_day),
        rvol: inp.meta.rvol,
        change_day_pct: inp.meta.change_day_pct,
        float_shares: inp.meta.float_shares,
        news_today: e.news_today,
        halted: Some(false),
        latency_ui_ms: None,
        reason,
        display_timeframe: Some("10s".to_string()),
        side: Some(Side::Long),
        tempo_depth: Some(depth.iter().map(|s| *s as u32).collect()),
    }
}

/// Tempo label as the trader reads it: seconds under a minute, minutes above.
fn fmt_tempo(secs: i64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m", secs / 60)
    }
}

// ─── Data loading ──────────────────────────────────────────────────────────────

/// Per-symbol float (shares) from the universe table; symbols with no known float
/// are omitted (treated as unknown — admitted only when `allow_unknown_float`).
fn load_universe_meta(
    db: &Arc<Mutex<rusqlite::Connection>>,
) -> (HashMap<String, u64>, HashMap<String, f64>) {
    let conn = db.lock().unwrap();
    let assets = universe_repository::get_all(&conn).unwrap_or_default();
    let mut floats = HashMap::new();
    let mut avg_volumes = HashMap::new();
    for a in assets {
        if let Some(f) = a.float_shares.filter(|f| *f > 0) {
            floats.insert(a.symbol.clone(), f as u64);
        }
        if let Some(v) = a.avg_volume.filter(|v| *v > 0) {
            avg_volumes.insert(a.symbol, v as f64);
        }
    }
    (floats, avg_volumes)
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
