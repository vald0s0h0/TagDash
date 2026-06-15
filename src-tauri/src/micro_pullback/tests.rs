// Unit tests for the Micro Pullback multi-tempo engine. Drive the pure per-ticker
// evaluator (`evaluate`) over synthetic 10-second bars — no tokio loop, no live
// market — which is also how the engine is backtested on historical 10s candles.

use super::*;
use chrono::{Duration as ChronoDuration, TimeZone, Utc};

const CFG: Config = Config::DEFAULT;

/// 04:30 ET (08:30 UTC, EDT) — inside the premarket window.
fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 8, 12, 30, 0).unwrap()
}

/// Build one 10s bar at `i*10s` after `t0()`.
fn bar(i: i64, open: f64, high: f64, low: f64, close: f64, volume: u64, trades: u64) -> Bar {
    Bar {
        time: t0() + ChronoDuration::seconds(i * BAR_SECS),
        open,
        high,
        low,
        close,
        volume,
        vwap: Some((open + high + low + close) / 4.0),
        trade_count: Some(trades),
    }
}

/// A flat, low-activity baseline bar around `price`.
fn calm_bar(i: i64, price: f64) -> Bar {
    bar(i, price, price + 0.003, price - 0.003, price, 200, 3)
}

fn meta(float_known: bool) -> Meta {
    Meta {
        price: 5.30,
        bid: Some(5.29),
        ask: Some(5.31),
        spread: Some(0.02),
        volume_day: 500_000,
        change_day_pct: Some(8.0),
        float_shares: if float_known { Some(12_000_000) } else { None },
        float_known,
    }
}

fn input(bars: Vec<Bar>) -> Input {
    Input { symbol: "TEST".to_string(), bars, meta: meta(true) }
}

fn tape_only(_: &str) -> (String, bool) {
    ("TAPE_ONLY".to_string(), false)
}

// ── Tape-rate (final gate) stubs ──
/// No prints at all → neither watch layer ever confirms.
fn cold_tape(_secs: i64) -> u64 {
    0
}
/// 6 prints/sec on every window → clears the fast (1s) layer immediately.
fn hot_tape(secs: i64) -> u64 {
    (secs * 6) as u64
}
/// 0/sec on the 1s fast window, 3/sec on the 5s slow window → only the slow layer.
fn slow_tape(secs: i64) -> u64 {
    if secs <= 1 { 0 } else { (secs * 3) as u64 }
}

/// 30 flat baseline bars at $5.00 (indices 0..=29) → a calm 5-minute baseline.
fn calm_baseline() -> Vec<Bar> {
    (0..30).map(|i| calm_bar(i, 5.00)).collect()
}

#[test]
fn median_handles_odd_and_even() {
    assert_eq!(median(&mut [3.0, 1.0, 2.0]), 2.0);
    assert_eq!(median(&mut [4.0, 1.0, 3.0, 2.0]), 2.5);
    assert_eq!(median(&mut []), 0.0);
}

#[test]
fn window_metrics_basic() {
    let win = [
        bar(0, 5.00, 5.05, 5.00, 5.04, 1000, 10),
        bar(1, 5.04, 5.12, 5.03, 5.10, 2000, 20),
    ];
    let (vol, trades, range_pct, ret) = window_metrics(&win);
    assert_eq!(vol, 3000.0);
    assert_eq!(trades, 30);
    // return = (5.10 - 5.00) / 5.00 = 2%
    assert!((ret - 2.0).abs() < 1e-9);
    // range = (5.12 - 5.00) / avg(5.04, 5.10) ≈ 0.12 / 5.07 * 100
    assert!((range_pct - 0.12 / 5.07 * 100.0).abs() < 1e-6);
}

#[test]
fn gate1_universe_respects_unknown_float_toggle() {
    // Price band.
    assert!(!gate1_tradeable(0.50, Some(1_000_000), &CFG));
    assert!(!gate1_tradeable(30.0, Some(1_000_000), &CFG));
    // Known float.
    assert!(gate1_tradeable(5.0, Some(10_000_000), &CFG));
    assert!(!gate1_tradeable(5.0, Some(40_000_000), &CFG));
    // Unknown float: admitted only when allowed.
    let mut deny = CFG;
    deny.allow_unknown_float = false;
    assert!(gate1_tradeable(5.0, None, &CFG)); // default: true
    assert!(!gate1_tradeable(5.0, None, &deny));
}

#[test]
fn fast_10s_departure_fires_on_10s_tempo() {
    let mut bars = calm_baseline();
    // One explosive 10s bar: +1.5%, 15k shares, 40 trades vs the calm baseline.
    bars.push(bar(30, 5.00, 5.08, 5.00, 5.075, 15_000, 40));

    let mut m = Machine::new();
    let now = bars.last().unwrap().time + ChronoDuration::seconds(BAR_SECS);
    // Detection arms the watch and the hot tape confirms on the same tick → fire.
    let a = evaluate(&mut m, &input(bars), &CFG, now, None, &tape_only, &hot_tape)
        .expect("a fast 10s burst on hot tape should fire");
    assert_eq!(a.strategy_id, STRATEGY_ID);
    assert_eq!(a.priority, 5);
    assert_eq!(a.side, Some(Side::Long));
    assert!(a.reason.contains("départ 10s"), "reason: {}", a.reason);
    assert!(a.reason.contains("tape"), "reason should report the confirmed tape rate: {}", a.reason);
    assert!(m.locked);
}

#[test]
fn slow_60s_departure_fires_on_60s_not_10s() {
    let mut bars = calm_baseline();
    // A gradual 6-bar ramp: each bar +0.6% (below the 10s 1.0% min) and 9k shares
    // (below the 10s 10k min), but the cumulative 60s window is +3.6% on 54k shares
    // → only the 60s tempo trips. Tape activity is now judged by the final gate.
    let mut p = 5.00;
    for i in 30..36 {
        let open = p;
        p *= 1.006;
        bars.push(bar(i, open, p + 0.005, open - 0.005, p, 9_000, 13));
    }

    let mut m = Machine::new();
    let now = bars.last().unwrap().time + ChronoDuration::seconds(BAR_SECS);
    let a = evaluate(&mut m, &input(bars), &CFG, now, None, &tape_only, &hot_tape)
        .expect("a slow but powerful 60s ramp on hot tape should fire");
    assert!(a.reason.contains("départ 60s"), "reason: {}", a.reason);
}

#[test]
fn fires_once_then_locks_for_the_session() {
    let mut bars = calm_baseline();
    bars.push(bar(30, 5.00, 5.08, 5.00, 5.075, 15_000, 40));

    let mut m = Machine::new();
    let now = bars.last().unwrap().time + ChronoDuration::seconds(BAR_SECS);
    assert!(evaluate(&mut m, &input(bars.clone()), &CFG, now, None, &tape_only, &hot_tape).is_some());
    assert!(m.locked);

    // Another explosive bar later — locked, so no second alert.
    bars.push(bar(31, 5.075, 5.20, 5.07, 5.19, 18_000, 50));
    let now2 = bars.last().unwrap().time + ChronoDuration::seconds(BAR_SECS);
    assert!(
        evaluate(&mut m, &input(bars), &CFG, now2, None, &tape_only, &hot_tape).is_none(),
        "a locked ticker must not re-alert this session"
    );
}

#[test]
fn weak_volume_spike_without_price_impulse_does_not_fire() {
    let mut bars = calm_baseline();
    // Big volume + trades, but a flat price (return ≈ 0 < 1.0%) → absolute min fails,
    // so detection never even arms the watch (tape is irrelevant here).
    bars.push(bar(30, 5.00, 5.01, 4.99, 5.001, 20_000, 60));

    let mut m = Machine::new();
    let now = bars.last().unwrap().time + ChronoDuration::seconds(BAR_SECS);
    assert!(
        evaluate(&mut m, &input(bars), &CFG, now, None, &tape_only, &hot_tape).is_none(),
        "a volume spike with no price impulse must not fire"
    );
    assert!(m.watch.is_none(), "detection must not arm the watch");
}

#[test]
fn two_of_three_ratio_rule_blocks_single_ratio() {
    // Absolute mins all pass (return 1.5%, 15k shares) and the volume ratio is huge —
    // but the trade-rate and range ratios stay near baseline, so only 1 of 3 ratios
    // clears and the 2-of-3 rule blocks detection.
    let mut bars = calm_baseline();
    bars.push(bar(30, 5.00, 5.078, 5.072, 5.075, 15_000, 20));

    let tempo = &CFG.tempos[0]; // 10s
    let mtr = eval_tempo(&bars, tempo, &CFG, None).unwrap();
    assert!(mtr.volume_ratio >= tempo.volume_ratio_min, "volume ratio should clear");
    assert!(mtr.trade_rate_ratio < tempo.trade_rate_ratio_min, "trade-rate must not clear");
    assert!(mtr.range_ratio < tempo.range_ratio_min, "range must not clear");
    assert!(!gate3_trips(&mtr, tempo), "1-of-3 ratios must not trip the gate");

    let mut m = Machine::new();
    let now = bars.last().unwrap().time + ChronoDuration::seconds(BAR_SECS);
    assert!(evaluate(&mut m, &input(bars), &CFG, now, None, &tape_only, &hot_tape).is_none());
    assert!(m.watch.is_none());
}

#[test]
fn seed_fires_without_full_live_window() {
    // Late start: only a few live 10s bars, but a 1-minute seed proves the baseline.
    let seed = {
        let mbars: Vec<Bar> = (0..5)
            .map(|i| Bar {
                time: t0() + ChronoDuration::seconds(i * 60),
                open: 5.00, high: 5.02, low: 4.99, close: 5.00,
                volume: 1_200, vwap: Some(5.00), trade_count: Some(18),
            })
            .collect();
        seed_from_minutes(&mbars).expect("5 flat 1-min bars → usable seed")
    };

    // Just one explosive live 10s bar (no live baseline window at all).
    let bars = vec![bar(0, 5.00, 5.08, 5.00, 5.075, 15_000, 40)];

    let mut m = Machine::new();
    let now = bars.last().unwrap().time + ChronoDuration::seconds(BAR_SECS);
    let a = evaluate(&mut m, &input(bars.clone()), &CFG, now, Some(&seed), &tape_only, &hot_tape)
        .expect("a seed-armed late start on hot tape should fire on the first live burst");
    assert!(a.reason.contains("départ 10s"));

    // Without the seed and no live baseline, the same single bar can't be judged.
    let mut m2 = Machine::new();
    assert!(
        evaluate(&mut m2, &input(bars), &CFG, now, None, &tape_only, &hot_tape).is_none(),
        "no baseline (no live window, no seed) ⇒ cannot evaluate"
    );
}

#[test]
fn seed_scales_per_tempo() {
    let seed = Seed { median_volume_1m: 1_200.0, median_trades_1m: 18.0, median_range_pct_1m: 0.6 };
    // 10s = 1/6 of a minute: volume/trades ÷6, range ×√(1/6).
    let (v10, t10, r10) = seed_baseline(&seed, 10);
    assert!((v10 - 200.0).abs() < 1e-6);
    assert!((t10 - 3.0).abs() < 1e-6);
    assert!((r10 - 0.6 * (1.0_f64 / 6.0).sqrt()).abs() < 1e-9);
    // 60s = a full minute: unchanged.
    let (v60, t60, r60) = seed_baseline(&seed, 60);
    assert!((v60 - 1_200.0).abs() < 1e-6);
    assert!((t60 - 18.0).abs() < 1e-6);
    assert!((r60 - 0.6).abs() < 1e-9);
}

#[test]
fn float_unknown_tag_in_reason() {
    let mut bars = calm_baseline();
    bars.push(bar(30, 5.00, 5.08, 5.00, 5.075, 15_000, 40));
    let inp = Input { symbol: "TEST".into(), bars, meta: meta(false) };

    let mut m = Machine::new();
    let now = inp.bars.last().unwrap().time + ChronoDuration::seconds(BAR_SECS);
    let a = evaluate(&mut m, &inp, &CFG, now, None, &tape_only, &hot_tape).unwrap();
    assert!(a.reason.contains("FLOAT_UNKNOWN"), "reason: {}", a.reason);
    assert!(a.reason.contains("TAPE_ONLY"));
    assert!(!a.news_today);
}

// ─── Final gate: tape-rate watch ───────────────────────────────────────────────

#[test]
fn detection_arms_watch_but_cold_tape_does_not_fire() {
    // A real 10s departure trips detection, but with no prints the watch can't
    // confirm: armed, not locked, no alert.
    let mut bars = calm_baseline();
    bars.push(bar(30, 5.00, 5.08, 5.00, 5.075, 15_000, 40));

    let mut m = Machine::new();
    let now = bars.last().unwrap().time + ChronoDuration::seconds(BAR_SECS);
    assert!(
        evaluate(&mut m, &input(bars), &CFG, now, None, &tape_only, &cold_tape).is_none(),
        "cold tape must not confirm the watch"
    );
    assert!(m.watch.is_some(), "detection should have armed the watch");
    assert!(!m.locked);
}

#[test]
fn slow_layer_confirms_without_fast() {
    // Detection arms the watch; the fast (1s) layer stays cold but the sustained
    // 5s rate (≥2/s) confirms.
    let mut bars = calm_baseline();
    bars.push(bar(30, 5.00, 5.08, 5.00, 5.075, 15_000, 40));

    let mut m = Machine::new();
    let now = bars.last().unwrap().time + ChronoDuration::seconds(BAR_SECS);
    let a = evaluate(&mut m, &input(bars), &CFG, now, None, &tape_only, &slow_tape)
        .expect("the slow layer should confirm a sustained tape");
    assert!(a.reason.contains("départ 10s"));
    assert!(m.locked);
}

#[test]
fn watch_times_out_when_tape_never_confirms() {
    let mut bars = calm_baseline();
    bars.push(bar(30, 5.00, 5.08, 5.00, 5.075, 15_000, 40));

    // Tick 1: detection arms the watch (cold tape → no fire).
    let mut m = Machine::new();
    let now = bars.last().unwrap().time + ChronoDuration::seconds(BAR_SECS);
    assert!(evaluate(&mut m, &input(bars.clone()), &CFG, now, None, &tape_only, &cold_tape).is_none());
    assert!(m.watch.is_some());

    // Past the watch window with the tape still cold → the watch is dropped, the
    // ticker stays unlocked (eligible for re-detection).
    let later = now + ChronoDuration::seconds(CFG.watch_max_secs + 1);
    assert!(evaluate(&mut m, &input(bars), &CFG, later, None, &tape_only, &cold_tape).is_none());
    assert!(m.watch.is_none(), "an unconfirmed watch must be dropped after the window");
    assert!(!m.locked);
}
