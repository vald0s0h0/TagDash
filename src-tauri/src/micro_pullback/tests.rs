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
        rvol: Some(4.2),
    }
}

fn input(bars: Vec<Bar>) -> Input {
    Input {
        symbol: "TEST".to_string(),
        bars,
        meta: meta(true),
    }
}

fn tape_only(_: &str) -> (String, bool) {
    ("TAPE_ONLY".to_string(), false)
}

/// Run `evaluate` on the Alpaca-style prints tape (the historical default) and hand
/// back whatever alert it produced — a fresh fire or a depth re-render alike, since
/// most tests only care that something reached the screen.
fn eval_prints(
    m: &mut Machine,
    inp: &Input,
    now: DateTime<Utc>,
    seed: Option<&Seed>,
    tape: &dyn Fn(i64) -> TapeWindow,
) -> Option<AlertSignal> {
    eval_outcome(m, inp, &CFG, now, seed, tape).map(alert_of)
}

fn eval_outcome(
    m: &mut Machine,
    inp: &Input,
    cfg: &Config,
    now: DateTime<Utc>,
    seed: Option<&Seed>,
    tape: &dyn Fn(i64) -> TapeWindow,
) -> Option<Outcome> {
    evaluate(m, inp, cfg, now, seed, PRINTS, &tape_only, tape)
}

fn alert_of(o: Outcome) -> AlertSignal {
    match o {
        Outcome::Fired(alert) | Outcome::DepthUpdated(alert) => alert,
    }
}

/// A real-trade tape: one message per print, so the count is native.
const PRINTS: TapeShape = TapeShape {
    kind: TapeKind::Prints,
    counts_trades: true,
};
/// A per-second aggregate tape that cannot say how many trades a second held —
/// the pre-`z` Massive frame, and the only case where the dollars-and-continuity
/// fallback is the right answer.
const AGG_NO_COUNT: TapeShape = TapeShape {
    kind: TapeKind::SecondAggregates,
    counts_trades: false,
};
/// A per-second aggregate tape carrying `z`, so the count is reconstructed and the
/// engine takes exactly the same path as on prints.
const AGG_WITH_COUNT: TapeShape = TapeShape {
    kind: TapeKind::SecondAggregates,
    counts_trades: true,
};

// ── Final-gate stubs ──
// Every window carries plenty of dollars unless the test is specifically about the
// dollar floor, so these isolate the layer under test.
fn window(prints: u64, active_seconds: u32, dollars: f64, max_second: f64) -> TapeWindow {
    TapeWindow {
        prints,
        active_seconds,
        dollars,
        max_second_dollars: max_second,
    }
}

/// No prints at all → neither watch layer ever confirms.
fn cold_tape(_secs: i64) -> TapeWindow {
    TapeWindow::default()
}
/// 6 prints/sec on every window → clears the fast (1s) layer immediately.
fn hot_tape(secs: i64) -> TapeWindow {
    window(
        (secs * 6) as u64,
        secs as u32,
        20_000.0 * secs as f64,
        20_000.0,
    )
}
/// 0/sec on the 1s fast window, 3/sec on the 5s slow window → only the slow layer.
fn slow_tape(secs: i64) -> TapeWindow {
    let prints = if secs <= 1 { 0 } else { (secs * 3) as u64 };
    window(prints, secs as u32, 20_000.0 * secs as f64, 20_000.0)
}
/// A busy trade rate on near-worthless size — clears the rate layers but not the
/// dollar floor.
fn penny_tape(secs: i64) -> TapeWindow {
    window((secs * 6) as u64, secs as u32, 100.0 * secs as f64, 100.0)
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
    let (vol, dollars, trades, range_pct, ret) = window_metrics(&win);
    assert_eq!(vol, 3000.0);
    assert_eq!(trades, 30);
    // Dollars use each bar's own VWAP, not the window's last price.
    let expect_dollars = win[0].vwap.unwrap() * 1000.0 + win[1].vwap.unwrap() * 2000.0;
    assert!((dollars - expect_dollars).abs() < 1e-6);
    // return = (5.10 - 5.00) / 5.00 = 2%
    assert!((ret - 2.0).abs() < 1e-9);
    // range = (5.12 - 5.00) / avg(5.04, 5.10) ≈ 0.12 / 5.07 * 100
    assert!((range_pct - 0.12 / 5.07 * 100.0).abs() < 1e-6);
}

#[test]
fn gate1_universe_respects_unknown_float_toggle() {
    // Price band.
    assert!(!gate1_tradeable(0.50, Some(1_000_000), IssuerProfile::default(), &CFG));
    assert!(!gate1_tradeable(30.0, Some(1_000_000), IssuerProfile::default(), &CFG));
    // Known float.
    assert!(gate1_tradeable(5.0, Some(10_000_000), IssuerProfile::default(), &CFG));
    assert!(!gate1_tradeable(5.0, Some(40_000_000), IssuerProfile::default(), &CFG));
    // Unknown float: admitted only when allowed.
    let mut deny = CFG;
    deny.allow_unknown_float = false;
    assert!(gate1_tradeable(5.0, None, IssuerProfile::default(), &CFG)); // default: true
    assert!(!gate1_tradeable(5.0, None, IssuerProfile::default(), &deny));
}

#[test]
fn fast_10s_departure_fires_on_10s_tempo() {
    let mut bars = calm_baseline();
    // One explosive 10s bar: +1.5%, 15k shares, 40 trades vs the calm baseline.
    bars.push(bar(30, 5.00, 5.08, 5.00, 5.075, 15_000, 40));

    let mut m = Machine::new();
    let now = bars.last().unwrap().time + ChronoDuration::seconds(BAR_SECS);
    // Detection arms the watch and the hot tape confirms on the same tick → fire.
    let a = eval_prints(&mut m, &input(bars), now, None, &hot_tape)
        .expect("a fast 10s burst on hot tape should fire");
    assert_eq!(a.strategy_id, STRATEGY_ID);
    assert_eq!(a.priority, 5);
    assert_eq!(a.side, Some(Side::Long));
    assert!(a.reason.contains("départ 10s"), "reason: {}", a.reason);
    assert!(
        a.reason.contains("tape"),
        "reason should report the confirmed tape rate: {}",
        a.reason
    );
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
    let a = eval_prints(&mut m, &input(bars), now, None, &hot_tape)
        .expect("a slow but powerful 60s ramp on hot tape should fire");
    // Labelled the way the Settings checkbox is: seconds below a minute, minutes above.
    assert!(a.reason.contains("départ 1m"), "reason: {}", a.reason);
    assert_eq!(
        a.tempo_depth.as_deref(),
        Some(&[60u32][..]),
        "only the 60s window trips"
    );
}

#[test]
fn fires_once_then_locks_for_the_session() {
    let mut bars = calm_baseline();
    bars.push(bar(30, 5.00, 5.08, 5.00, 5.075, 15_000, 40));

    let mut m = Machine::new();
    let now = bars.last().unwrap().time + ChronoDuration::seconds(BAR_SECS);
    let first = eval_prints(&mut m, &input(bars.clone()), now, None, &hot_tape)
        .expect("the first departure fires");
    assert!(m.locked);

    // Another explosive bar later. The ticker is locked, so whatever comes back is
    // never a SECOND alert: either nothing, or the same alert re-rendered because a
    // further tempo widened the depth. The `alert_id` is what makes that distinction
    // load-bearing — the sidebar places one zone per id.
    bars.push(bar(31, 5.075, 5.20, 5.07, 5.19, 18_000, 50));
    let now2 = bars.last().unwrap().time + ChronoDuration::seconds(BAR_SECS);
    if let Some(again) = eval_prints(&mut m, &input(bars), now2, None, &hot_tape) {
        assert_eq!(
            again.alert_id, first.alert_id,
            "a re-render must reuse the alert id"
        );
        assert_eq!(
            again.timestamp, first.timestamp,
            "and its original timestamp"
        );
        assert!(
            again.tempo_depth.unwrap().len() > first.tempo_depth.unwrap().len(),
            "the only reason to re-render is a widened depth"
        );
    }
}

#[test]
fn depth_records_every_tempo_that_fired_not_just_the_first() {
    // A 60s ramp strong enough that the closing 10s bar also clears the 10s tempo:
    // both windows describe the same move, and the trader wants to see both.
    let mut bars = calm_baseline();
    let mut p = 5.00;
    for i in 30..35 {
        let open = p;
        p *= 1.008;
        bars.push(bar(i, open, p + 0.005, open - 0.005, p, 12_000, 20));
    }
    bars.push(bar(35, p, p * 1.02, p, p * 1.018, 30_000, 60));

    let mut m = Machine::new();
    let now = bars.last().unwrap().time + ChronoDuration::seconds(BAR_SECS);
    let a = eval_prints(&mut m, &input(bars), now, None, &hot_tape).expect("should fire");
    let depth = a
        .tempo_depth
        .clone()
        .expect("micro pullback always reports its depth");

    assert!(
        depth.len() > 1,
        "several tempos should have tripped, got {depth:?}"
    );
    assert!(
        depth.windows(2).all(|w| w[0] < w[1]),
        "depth must be sorted: {depth:?}"
    );
    // The FASTEST tripped tempo is the one that armed the watch and titles the alert.
    assert!(
        a.reason
            .contains(&format!("départ {}", fmt_tempo(depth[0] as i64))),
        "reason should lead with the fastest tempo: {}",
        a.reason
    );
    assert!(
        a.reason.contains("tempos "),
        "the depth list should be visible: {}",
        a.reason
    );
}

#[test]
fn a_tempo_switched_off_cannot_fire() {
    let mut bars = calm_baseline();
    bars.push(bar(30, 5.00, 5.08, 5.00, 5.075, 15_000, 40));

    // Same bars, but the 10s tempo is unchecked in Settings.
    let cfg = Config::DEFAULT.with_options(&MicroPullbackOptions {
        tempo_10s: false,
        ..Default::default()
    });
    let mut m = Machine::new();
    let now = bars.last().unwrap().time + ChronoDuration::seconds(BAR_SECS);
    let fired = evaluate(
        &mut m,
        &input(bars),
        &cfg,
        now,
        None,
        PRINTS,
        &tape_only,
        &hot_tape,
    );
    // The burst is a 10s-shaped event: with that tempo off, nothing else covers it.
    assert!(fired.is_none(), "a disabled tempo must not fire");
    assert!(m.depth.is_empty(), "nor be recorded in the depth list");
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
        eval_prints(&mut m, &input(bars), now, None, &hot_tape).is_none(),
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
    let mtr = eval_tempo(&bars, tempo, &CFG, None, PRINTS).unwrap();
    assert!(
        mtr.volume_ratio >= tempo.volume_ratio_min,
        "volume ratio should clear"
    );
    assert!(
        mtr.trade_rate_ratio.unwrap() < tempo.trade_rate_ratio_min,
        "trade-rate must not clear"
    );
    assert!(
        mtr.range_ratio < tempo.range_ratio_min,
        "range must not clear"
    );
    assert!(
        !gate3_trips(&mtr, tempo, CFG.ratios_required),
        "1-of-3 ratios must not trip the gate"
    );

    let mut m = Machine::new();
    let now = bars.last().unwrap().time + ChronoDuration::seconds(BAR_SECS);
    assert!(eval_prints(&mut m, &input(bars), now, None, &hot_tape).is_none());
    assert!(m.watch.is_none());
}

#[test]
fn seed_fires_without_full_live_window() {
    // Late start: only a few live 10s bars, but a 1-minute seed proves the baseline.
    let seed = {
        let mbars: Vec<Bar> = (0..5)
            .map(|i| Bar {
                time: t0() + ChronoDuration::seconds(i * 60),
                open: 5.00,
                high: 5.02,
                low: 4.99,
                close: 5.00,
                volume: 1_200,
                vwap: Some(5.00),
                trade_count: Some(18),
            })
            .collect();
        seed_from_minutes(&mbars).expect("5 flat 1-min bars → usable seed")
    };

    // Just one explosive live 10s bar (no live baseline window at all).
    let bars = vec![bar(0, 5.00, 5.08, 5.00, 5.075, 15_000, 40)];

    let mut m = Machine::new();
    let now = bars.last().unwrap().time + ChronoDuration::seconds(BAR_SECS);
    let a = eval_prints(&mut m, &input(bars.clone()), now, Some(&seed), &hot_tape)
        .expect("a seed-armed late start on hot tape should fire on the first live burst");
    assert!(a.reason.contains("départ 10s"));

    // Without the seed and no live baseline, the same single bar can't be judged.
    let mut m2 = Machine::new();
    assert!(
        eval_prints(&mut m2, &input(bars), now, None, &hot_tape).is_none(),
        "no baseline (no live window, no seed) ⇒ cannot evaluate"
    );
}

#[test]
fn seed_scales_per_tempo() {
    let seed = Seed {
        median_volume_1m: 1_200.0,
        median_trades_1m: 18.0,
        median_range_pct_1m: 0.6,
    };
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
    let inp = Input {
        symbol: "TEST".into(),
        bars,
        meta: meta(false),
    };

    let mut m = Machine::new();
    let now = inp.bars.last().unwrap().time + ChronoDuration::seconds(BAR_SECS);
    let a = eval_prints(&mut m, &inp, now, None, &hot_tape).unwrap();
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
        eval_prints(&mut m, &input(bars), now, None, &cold_tape).is_none(),
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
    let a = eval_prints(&mut m, &input(bars), now, None, &slow_tape)
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
    assert!(eval_prints(&mut m, &input(bars.clone()), now, None, &cold_tape).is_none());
    assert!(m.watch.is_some());

    // Past the watch window with the tape still cold → the watch is dropped, the
    // ticker stays unlocked (eligible for re-detection).
    let later = now + ChronoDuration::seconds(CFG.watch_max_secs + 1);
    assert!(eval_prints(&mut m, &input(bars), later, None, &cold_tape).is_none());
    assert!(
        m.watch.is_none(),
        "an unconfirmed watch must be dropped after the window"
    );
    assert!(!m.locked);
}

// ─── Final gate: the dollar floor (both variants) ──────────────────────────────

#[test]
fn a_busy_tape_of_worthless_size_is_not_liquidity() {
    // 6 prints/s clears the fast rate layer outright, but 6 prints of ~$17 each is
    // $100/s — two orders of magnitude under the floor. Before the floor existed
    // this fired.
    let mut bars = calm_baseline();
    bars.push(bar(30, 5.00, 5.08, 5.00, 5.075, 15_000, 40));

    let mut m = Machine::new();
    let now = bars.last().unwrap().time + ChronoDuration::seconds(BAR_SECS);
    assert!(
        eval_prints(&mut m, &input(bars), now, None, &penny_tape).is_none(),
        "a high print rate on no dollars must not confirm"
    );
    assert!(
        m.watch.is_some(),
        "detection still armed — it is the final gate that refused"
    );
}

// ─── Final gate: the second-aggregate variant (Massive) ────────────────────────

/// Drive `confirm_liquidity` directly on the aggregate tape.
fn confirm_agg(win: TapeWindow) -> Option<LiquidityProof> {
    confirm_liquidity(&CFG, AGG_NO_COUNT, &|_| win)
}

#[test]
fn aggregate_gate_confirms_on_continuity_plus_dollars() {
    // 7 of the last 10 seconds printed, $40k crossed → a name genuinely trading.
    let p = confirm_agg(window(7, 7, 40_000.0, 9_000.0))
        .expect("continuous tape with real dollars should confirm");
    assert_eq!(p.source, TapeKind::SecondAggregates);
    assert_eq!(p.active_seconds, Some(7));
    assert!(
        p.rate_per_sec.is_none(),
        "there is no trade rate on this feed"
    );
    assert!((p.dollar_per_sec - 4_000.0).abs() < 1e-9);
}

#[test]
fn aggregate_gate_refuses_a_bursty_tape() {
    // The same $40k, but concentrated in 4 of 10 seconds and no single second hot
    // enough for the fast layer. This is the pattern the continuity rule exists for.
    assert!(
        confirm_agg(window(4, 4, 40_000.0, 15_000.0)).is_none(),
        "4 active seconds out of 10 is not a continuous tape"
    );
}

#[test]
fn aggregate_gate_fast_layer_needs_more_than_one_hot_second() {
    // One $25k second clears `watch_agg_fast_dollar_sec_min`… but the window totals
    // only $25k, i.e. $2.5k/s, under the common floor. The two rules compose: a
    // single hot print in an otherwise dead name is not liquidity.
    assert!(
        confirm_agg(window(1, 1, 25_000.0, 25_000.0)).is_none(),
        "one hot second inside a dead window must not confirm"
    );
    // Same hot second, but the window is alive around it → confirmed immediately,
    // without waiting for the 6-of-10 continuity rule.
    let p = confirm_agg(window(5, 5, 60_000.0, 25_000.0))
        .expect("a hot second in a live window confirms on the fast layer");
    assert_eq!(
        p.active_seconds,
        Some(5),
        "confirmed below the continuity threshold"
    );
}

#[test]
fn gate3_needs_both_ratios_when_the_trade_rate_is_unavailable() {
    let tempo = &CFG.tempos[0]; // 10s
    let base = TempoMetrics {
        current_volume: 15_000.0,
        current_dollars: 80_000.0,
        current_trades: 40,
        price_return: 1.5,
        volume_ratio: 10.0,
        trade_rate_ratio: None,
        range_ratio: 5.0,
    };
    assert!(
        gate3_trips(&base, tempo, CFG.ratios_required),
        "both available ratios clear → trips"
    );

    // With the trade rate missing, ONE of the two remaining ratios failing is enough
    // to refuse — whereas on the prints tape the trade rate could have carried it.
    let one_short = TempoMetrics {
        range_ratio: 1.0,
        ..base
    };
    assert!(
        !gate3_trips(&one_short, tempo, CFG.ratios_required),
        "2-of-2 is required on the aggregate tape"
    );
    let rescued = TempoMetrics {
        trade_rate_ratio: Some(20.0),
        ..one_short
    };
    assert!(
        gate3_trips(&rescued, tempo, CFG.ratios_required),
        "on prints, the trade rate is the third vote"
    );
}

#[test]
fn gate3_requires_both_the_share_and_the_dollar_minimum() {
    let tempo = &CFG.tempos[0]; // 10s: 10 000 shares AND $25 000
    let ok = TempoMetrics {
        current_volume: 15_000.0,
        current_dollars: 80_000.0,
        current_trades: 40,
        price_return: 1.5,
        volume_ratio: 10.0,
        trade_rate_ratio: Some(10.0),
        range_ratio: 5.0,
    };
    assert!(gate3_trips(&ok, tempo, CFG.ratios_required));
    // A $1.20 name: 15 000 shares is only $18k — plenty of shares, no size.
    assert!(!gate3_trips(
        &TempoMetrics {
            current_dollars: 18_000.0,
            ..ok
        },
        tempo,
        CFG.ratios_required
    ));
    // A $24 name: $80k is only 3 300 shares — plenty of dollars, no shares.
    assert!(!gate3_trips(
        &TempoMetrics {
            current_volume: 3_300.0,
            ..ok
        },
        tempo,
        CFG.ratios_required
    ));
}

#[test]
fn baseline_stretches_so_long_tempos_get_enough_buckets() {
    // Short tempos keep the historical 5-minute baseline…
    for t in [10, 20, 40, 60] {
        assert_eq!(baseline_secs(t), 300, "tempo {t}s");
    }
    // …and the long ones stretch, so a median over MIN_BASELINE_BUCKETS is possible
    // at all. A fixed 300s would give the 5-minute tempo a single bucket.
    assert_eq!(baseline_secs(120), 360);
    assert_eq!(baseline_secs(300), 900);
    for t in [10, 60, 120, 300] {
        assert!(
            baseline_secs(t) / t >= MIN_BASELINE_BUCKETS as i64,
            "tempo {t}s must afford at least {MIN_BASELINE_BUCKETS} buckets"
        );
    }
}

#[test]
fn seed_reach_follows_the_longest_active_tempo() {
    let all = Config::DEFAULT;
    // 5m tempo on: 300s window + 900s baseline + 60s cushion.
    assert_eq!(seed_lookback_secs(&all), 300 + 900 + 60);

    // With the two long tempos off, the reach collapses back to the old 6 minutes.
    let short = Config::DEFAULT.with_options(&MicroPullbackOptions {
        tempo_2m: false,
        tempo_5m: false,
        ..Default::default()
    });
    assert_eq!(seed_lookback_secs(&short), 60 + 300 + 60);
    assert!(full_live_bars(&short) < full_live_bars(&all));
}

#[test]
fn the_ui_tempo_list_matches_the_engines() {
    let engine: Vec<i64> = Config::DEFAULT.tempos.iter().map(|t| t.secs).collect();
    assert_eq!(
        engine,
        options::TEMPO_SECS.to_vec(),
        "Settings checkboxes and engine tempos must stay aligned"
    );
    // And each tempo must be a whole number of detection bars.
    assert!(engine.iter().all(|s| s % BAR_SECS == 0));
}

// ─── The tape shape drives the gate, not the provider ─────────────────────────

/// The same aggregate window, judged twice: once by a feed that cannot count trades
/// and once by one that can. This is the whole point of `TapeShape` — Massive's `A`
/// frame carries `z`, so "aggregates" and "no trade count" stopped being the same
/// statement and the engine must stop treating them as one.
/// END-TO-END on the shape Massive Starter actually delivers, through a real
/// `MarketState` instead of a stub window.
///
/// Every other final-gate test here injects a `TapeWindow` directly, which proves the
/// gate's arithmetic and nothing about the WIRING it depends on: `z` → reconstructed
/// count → `on_second_aggregate` → per-second buckets → `tape_window_stats`. That
/// chain is what has to hold for this provider to ever fire the last gate, and until
/// now no test crossed it.
///
/// The aggregates are stamped 15 minutes before their receipt instant, as Starter's
/// delayed feed sends them. The engine evaluates on the FEED clock (the event
/// watermark, see `MarketState::strategy_market_now` and its own test) — spelled out
/// here as `last_event` rather than read back, because the replay clock it consults
/// is process-global and another test may hold it.
#[test]
fn a_massive_second_stream_confirms_the_final_gate_through_market_state() {
    use crate::market_state::MarketState;
    use crate::massive::stream::reconstruct_prints;

    let first = t0();
    let receipt = first + ChronoDuration::minutes(15);

    let mut ms = MarketState::new();
    ms.set_tape_kind(TapeKind::SecondAggregates);

    // Ten consecutive traded seconds: 800 shares at ~$5.30 with an average trade
    // size of 100 — i.e. 8 real trades a second, a plausible premarket runner.
    let mut last_event = first;
    for i in 0..10 {
        let event = first + ChronoDuration::seconds(i);
        let prints = reconstruct_prints(800, Some(100.0));
        assert_eq!(prints, Some(8), "z must restore the count");
        if ms.tape_counts_trades() != prints.is_some() {
            ms.set_tape_counts_trades(prints.is_some());
        }
        ms.on_second_aggregate(
            "TEST",
            5.30,
            5.34,
            5.29,
            5.32,
            Some(5.30),
            800,
            prints.unwrap_or(1),
            event,
            receipt + ChronoDuration::seconds(i),
            1_000,
            2_000,
        );
        last_event = event;
    }

    let now = last_event;
    let shape = TapeShape::of(&ms);
    assert!(
        shape.counts_trades,
        "the aggregate tape carries z, so the gate must take the rate path"
    );

    let tape_fn = |secs: i64| {
        let s = ms.tape_window_stats("TEST", secs, now).unwrap_or_default();
        TapeWindow {
            prints: s.prints,
            active_seconds: s.active_seconds,
            dollars: s.dollars,
            max_second_dollars: s.max_second_dollars,
        }
    };

    // The count survives ingestion: 8 prints in the current second, 40 over five.
    assert_eq!(tape_fn(1).prints, 8);
    assert_eq!(tape_fn(5).prints, 40);
    assert_eq!(tape_fn(5).active_seconds, 5);
    // Notional comes from the aggregate's VWAP, not from its close.
    assert!((tape_fn(5).dollars - 5.30 * 800.0 * 5.0).abs() < 1e-6);

    let proof = confirm_liquidity(&CFG, shape, &tape_fn)
        .expect("8 prints/s and $4 240/s must clear the final gate");
    assert_eq!(proof.source, TapeKind::SecondAggregates);
    // 8/s clears the fast layer (5/s) on the very first second.
    assert_eq!(proof.window_secs, CFG.watch_fast_window_secs);
    assert_eq!(proof.rate_per_sec, Some(8.0));
    // The dollar floor always reads the slow window, even when fast confirmed.
    assert!((proof.dollar_per_sec - 4_240.0).abs() < 1e-6);
}

/// Same wiring, a name that only LOOKS alive: it prints every single second, so the
/// continuity fallback would wave it through, but 100 shares a second at $5.30 is
/// $530/s. The dollar floor is what stops it, and it must still bite once the count
/// is available — an aggregate feed makes a dead micro-cap look busy precisely
/// because one message per second is one message per second whatever crossed.
#[test]
fn a_continuous_but_worthless_aggregate_tape_still_fails_the_dollar_floor() {
    use crate::market_state::MarketState;
    use crate::massive::stream::reconstruct_prints;

    let first = t0();
    let mut ms = MarketState::new();
    ms.set_tape_kind(TapeKind::SecondAggregates);
    ms.set_tape_counts_trades(true);

    let mut last_event = first;
    for i in 0..10 {
        let event = first + ChronoDuration::seconds(i);
        let prints = reconstruct_prints(100, Some(100.0)).unwrap_or(1);
        ms.on_second_aggregate(
            "TEST", 5.30, 5.31, 5.29, 5.30, Some(5.30), 100, prints, event, event, 1_000, 2_000,
        );
        last_event = event;
    }

    let now = last_event;
    let tape_fn = |secs: i64| {
        let s = ms.tape_window_stats("TEST", secs, now).unwrap_or_default();
        TapeWindow {
            prints: s.prints,
            active_seconds: s.active_seconds,
            dollars: s.dollars,
            max_second_dollars: s.max_second_dollars,
        }
    };
    assert_eq!(tape_fn(10).active_seconds, 10, "prints in every second");
    assert!(confirm_liquidity(&CFG, TapeShape::of(&ms), &tape_fn).is_none());
}

#[test]
fn an_aggregate_tape_that_counts_trades_takes_the_rate_path() {
    // A name printing steadily: 4 trades and $6k every second, in 5 of the last 10
    // seconds. Length-aware so the fast (1s) and slow (5s) layers see what they
    // would really see.
    let tape = |secs: i64| {
        let active = (secs as u32).min(5);
        window(4 * active as u64, active, 6_000.0 * active as f64, 6_000.0)
    };

    // Blind, this is refused: only 5 of the fallback's 10 seconds printed, $30k is
    // exactly at its minimum and no single second is fat enough to carry it alone.
    assert!(
        confirm_liquidity(&CFG, AGG_NO_COUNT, &tape).is_none(),
        "la voie de repli ne voit qu'une bande discontinue"
    );

    // Counting, the same tape is 4 prints/s sustained — over the slow layer, under
    // the fast one — and it confirms on the measurement rather than on a proxy.
    let counted = confirm_liquidity(&CFG, AGG_WITH_COUNT, &tape)
        .expect("un vrai taux confirme sur toute bande capable de le mesurer");
    assert_eq!(
        counted.rate_per_sec,
        Some(4.0),
        "20 prints sur 5 s = 4/s, mesurés et non devinés"
    );
    assert_eq!(counted.window_secs, CFG.watch_slow_window_secs);
    assert_eq!(
        counted.source,
        TapeKind::SecondAggregates,
        "la provenance reste la provenance"
    );
}

/// The pre-`z` behaviour, pinned: a tape with no count still confirms on dollars and
/// continuity, so losing `z` degrades the engine instead of muting it.
#[test]
fn losing_the_count_falls_back_instead_of_going_silent() {
    let thin = window(1, 2, 5_000.0, 3_000.0);
    assert!(
        confirm_liquidity(&CFG, AGG_NO_COUNT, &|_| thin).is_none(),
        "deux secondes actives ne sont pas une bande"
    );
    let alive = window(1, 8, 40_000.0, 9_000.0);
    assert!(
        confirm_liquidity(&CFG, AGG_NO_COUNT, &|_| alive).is_some(),
        "huit secondes sur dix et 40 k$ restent une confirmation"
    );
}

// ─── The two issuer switches ──────────────────────────────────────────────────

#[test]
fn hot_industry_only_excludes_what_we_have_never_collected() {
    let hot = IssuerProfile {
        sector_hot: true,
        sector_bucket: Some("biotech"),
        country_risk: false,
    };
    let unknown = IssuerProfile::default();
    let mut cfg = CFG;
    cfg.hot_industry_only = true;

    assert!(gate1_tradeable(5.0, Some(10_000_000), hot, &cfg));
    assert!(
        !gate1_tradeable(5.0, Some(10_000_000), unknown, &cfg),
        "une industrie inconnue n'est pas porteuse — le bord tranchant de l'option"
    );
    // Off, it changes nothing at all.
    assert!(gate1_tradeable(5.0, Some(10_000_000), unknown, &CFG));
}

#[test]
fn excluding_risk_domiciles_never_bans_on_a_gap_in_our_data() {
    let risky = IssuerProfile {
        country_risk: true,
        ..Default::default()
    };
    let unknown = IssuerProfile::default();
    let mut cfg = CFG;
    cfg.exclude_risk_domicile = true;

    assert!(!gate1_tradeable(5.0, Some(10_000_000), risky, &cfg));
    assert!(
        gate1_tradeable(5.0, Some(10_000_000), unknown, &cfg),
        "un domicile inconnu survit — on exclut sur ce qu'on sait"
    );
}

// ─── The detection thresholds as settings ─────────────────────────────────────

#[test]
fn the_multipliers_move_the_grid_without_changing_its_shape() {
    let opts = crate::micro_pullback::MicroPullbackOptions {
        return_min_scale: 2.0,
        volume_min_scale: 0.5,
        ratio_min_scale: 0.5,
        ..Default::default()
    };
    let base = Config::DEFAULT;
    let tuned = Config::DEFAULT.with_options(&opts);
    for (b, t) in base.tempos.iter().zip(tuned.tempos.iter()) {
        assert_eq!(t.secs, b.secs);
        assert!((t.return_min_pct - b.return_min_pct * 2.0).abs() < 1e-9);
        assert!((t.volume_min - b.volume_min * 0.5).abs() < 1e-9);
        assert!((t.dollar_volume_min - b.dollar_volume_min * 0.5).abs() < 1e-9);
        assert!((t.volume_ratio_min - b.volume_ratio_min * 0.5).abs() < 1e-9);
    }
    // The slope survives: the long tempo still asks for more movement than the short.
    assert!(tuned.tempos[5].return_min_pct > tuned.tempos[0].return_min_pct);
    assert!(tuned.tempos[5].volume_ratio_min < tuned.tempos[0].volume_ratio_min);
}

#[test]
fn asking_for_more_ratios_than_the_feed_has_tightens_rather_than_mutes() {
    let tempo = &CFG.tempos[0];
    // Both available ratios clear; the trade rate does not exist.
    let both = TempoMetrics {
        current_volume: 15_000.0,
        current_dollars: 80_000.0,
        current_trades: 40,
        price_return: 1.5,
        volume_ratio: 10.0,
        trade_rate_ratio: None,
        range_ratio: 5.0,
    };
    assert!(
        gate3_trips(&both, tempo, 3),
        "3 sur 3 avec seulement deux ratios veut dire « les deux »"
    );
    let one = TempoMetrics {
        range_ratio: 1.0,
        ..both
    };
    assert!(!gate3_trips(&one, tempo, 3));
    assert!(
        gate3_trips(&one, tempo, 1),
        "un seul ratio suffit quand l'utilisateur le demande"
    );
}

#[test]
fn the_universe_settings_reach_the_engine() {
    let opts = crate::micro_pullback::MicroPullbackOptions {
        price_min: 2.0,
        price_max: 9.0,
        float_max: 5_000_000,
        allow_unknown_float: false,
        watch_fast_rate_per_sec: 12.0,
        watch_min_dollar_per_sec: 500.0,
        ..Default::default()
    };
    let cfg = Config::DEFAULT.with_options(&opts);
    assert!(!gate1_tradeable(1.5, Some(1_000_000), IssuerProfile::default(), &cfg));
    assert!(gate1_tradeable(5.0, Some(1_000_000), IssuerProfile::default(), &cfg));
    assert!(!gate1_tradeable(5.0, Some(6_000_000), IssuerProfile::default(), &cfg));
    assert!(!gate1_tradeable(5.0, None, IssuerProfile::default(), &cfg));
    assert_eq!(cfg.watch_fast_rate_per_sec, 12.0);
    assert_eq!(cfg.watch_min_dollar_per_sec, 500.0);
}
