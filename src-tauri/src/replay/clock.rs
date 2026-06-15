// Global simulated clock for Market Replay.
//
// When replay is ACTIVE, `crate::time::now()` returns the simulated instant
// stored here instead of `Utc::now()`, so every engine (scanner, micro/perfect
// pullback, panic watchlist, internal trading) lives on the replayed day without
// any per-engine wiring. When replay is INACTIVE these atomics are never read on
// the hot path beyond one relaxed bool load — live behaviour is unchanged.
//
// The GENERATION counter is bumped on every backward seek / day load so the
// engines (which keep per-session state: locks, cooldowns, gap maps…) know to
// reset themselves. Each engine loop samples `generation()` and clears its state
// when the value changed.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};

use chrono::{DateTime, TimeZone, Utc};

static ACTIVE: AtomicBool = AtomicBool::new(false);
/// Simulated instant in unix milliseconds (valid only while ACTIVE).
static SIM_MILLIS: AtomicI64 = AtomicI64::new(0);
/// Replay speed ×1000 (1.0× = 1000). Read by `scaled_sleep`.
static SPEED_MILLI: AtomicU64 = AtomicU64::new(1000);
/// Bumped on every state reset (backward seek, new day) — engines watch this.
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// True while a market replay is running (or paused) — i.e. the app clock is
/// the simulated one.
pub fn is_active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

/// The simulated instant. Only meaningful while `is_active()`.
pub fn sim_now() -> DateTime<Utc> {
    let ms = SIM_MILLIS.load(Ordering::Relaxed);
    Utc.timestamp_millis_opt(ms).single().unwrap_or_else(Utc::now)
}

/// Install the simulated instant (engine loop, on every tick / seek).
pub fn set_sim(t: DateTime<Utc>) {
    SIM_MILLIS.store(t.timestamp_millis(), Ordering::Relaxed);
}

/// Switch the app clock to simulated time (replay start).
pub fn activate(start: DateTime<Utc>) {
    set_sim(start);
    ACTIVE.store(true, Ordering::Relaxed);
}

/// Switch back to the real clock (replay stop).
pub fn deactivate() {
    ACTIVE.store(false, Ordering::Relaxed);
    SPEED_MILLI.store(1000, Ordering::Relaxed);
}

/// Current replay speed multiplier (1.0 when inactive).
pub fn speed() -> f64 {
    SPEED_MILLI.load(Ordering::Relaxed) as f64 / 1000.0
}

pub fn set_speed(s: f64) {
    let s = s.clamp(0.1, 600.0);
    SPEED_MILLI.store((s * 1000.0) as u64, Ordering::Relaxed);
}

/// Monotonic reset counter — engines clear their per-session state on change.
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

pub fn bump_generation() {
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

/// Sleep `base_ms` of *market* time: in live mode that is just `base_ms` of real
/// time; during an accelerated replay the real sleep is divided by the speed
/// (floored at 25 ms) so the engines keep roughly the same cadence in simulated
/// seconds as they have live. Drop-in replacement for the engines' loop sleeps.
pub async fn scaled_sleep(base_ms: u64) {
    let ms = if is_active() {
        let s = speed();
        if s > 1.0 {
            ((base_ms as f64 / s) as u64).max(25)
        } else {
            base_ms
        }
    } else {
        base_ms
    };
    tokio::time::sleep(tokio::time::Duration::from_millis(ms)).await;
}

/// Upper bound for Alpaca REST `end` parameters while replay is active: bars
/// whose bucket could overlap the simulated "future" are excluded (the forming
/// candle is served from RAM instead). `bucket_secs` is the bar length (86400
/// for daily — which also excludes the replay day's own daily bar, whose OHLC
/// would otherwise leak the day's close mid-replay). None in live mode (no
/// clamp). Formatted RFC3339 for the query string.
pub fn rest_end_clamp(bucket_secs: i64) -> Option<String> {
    if !is_active() {
        return None;
    }
    let end = sim_now() - chrono::Duration::seconds(bucket_secs.max(0));
    Some(end.format("%Y-%m-%dT%H:%M:%SZ").to_string())
}
