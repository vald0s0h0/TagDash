// Micro Pullback's replay data needs, and its minute-bar pre-scan.
//
// The live engine detects on closed 10-second bars; a replay download only has
// 1-minute bars to decide where real ticks are worth fetching. This pre-scan runs
// the ENGINE'S OWN gate — same `Config` built from the saved options, same
// `window_metrics`, `live_baseline`, `gate3_trips` — on minute bars, and leans
// towards recall wherever minutes blur the picture:
//
//   • tempos ≥ 60 s are evaluated on whole minutes, exactly as the engine buckets
//     them — only the 10-second sliding of the window is lost;
//   • tempos < 60 s cannot be seen inside a minute, so the minute stands in as an
//     UPPER BOUND of any sub-window (its volume, trades, range ≥ those of any 10 s
//     slice) against the minute baseline scaled down the engine's way
//     (`seed_baseline`: linear for volume/trades, √ for range);
//   • the return is bounded by the window's full high–low swing, not open→close;
//   • every threshold is then relaxed (`TUNING`).
//
// A superset is the point: a hit that the engine would not have confirmed costs a
// few MB of ticks; a missed one replays a real setup on synthetic slices. The slack
// is tuned against live-recorded alerts (the recall harness), and any change to the
// LOGIC here must bump `PRESCAN_VERSION` so prepared days are re-scanned.

use chrono::Duration as ChronoDuration;
use serde::{Deserialize, Serialize};

use super::{
    baseline_secs, confirm_liquidity, eval_tempo, gate3_trips, live_baseline, seed_baseline,
    seed_from_minutes, window_metrics, Config, MicroPullbackOptions, TapeShape, TapeWindow, Tempo,
    TempoMetrics, BAR_SECS,
};
use crate::market_state::aggregators::Bar;
use crate::review::codec::TradeRow;
use crate::types::TapeKind;
use crate::replay_data::{
    DataProfile, EtWindow, Hit, ReplayDataNeeds, SymbolScanner, TickNeeds, UniverseFilter,
    WindowPad,
};
use crate::strategies::micro_pullback::ID as STRATEGY_ID;

pub const PRESCAN_VERSION: u32 = 2;

/// How far the pre-scan leans towards recall. Calibrated against live-recorded
/// alerts (`prescan_calibration`); changing it is a LOGIC change → bump
/// `PRESCAN_VERSION`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Tuning {
    /// Absolute minimums (return, shares, dollars) × this.
    pub abs_slack: f64,
    /// Acceleration-ratio minimums × this.
    pub ratio_slack: f64,
    /// Sub-minute tempos only: the MINUTE must also show this share of the tempo's
    /// volume-ratio minimum against the plain minute baseline. 0 = the pure upper
    /// bound (every burst's minute passes), which lets ordinary busy minutes
    /// through: a 10 s spike of 8× its baseline is only ~1.3× at minute scale.
    pub submin_minute_ratio: f64,
}

pub(crate) const TUNING: Tuning = Tuning {
    abs_slack: 0.8,
    ratio_slack: 0.6,
    submin_minute_ratio: 0.0,
};
/// Minute history a sub-minute tempo's baseline is read from.
const SUBMINUTE_BASELINE_MIN: i64 = 20;
/// Real prints fetched around a minute hit to confirm it: 6 minutes before (the
/// minute before the hit, where the arm may already fall, plus the short tempos'
/// live baseline), 3 after (the hit minute, the next one — a 10 s window
/// straddles minutes — and the tape watch that follows an arm).
const PROBE: WindowPad = WindowPad {
    before_ms: 6 * 60_000,
    after_ms: 3 * 60_000,
};
/// Arms attributed to a hit: 10 s closes from one minute BEFORE the hit minute
/// (measured: DCOY armed at 08:25:4x, flagged at the 08:26 minute) to two minutes
/// after its start. The next probe of the symbol resumes at the end.
const ARM_LEAD_MS: i64 = 60_000;
const ARM_HORIZON_MS: i64 = 2 * 60_000;

/// The settings the pre-scan depends on — and nothing else, so toggling the LLM or a
/// notification does not mark prepared days stale. Float and the session window are
/// in the profile itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Detection {
    price_min: f64,
    price_max: f64,
    return_min_scale: f64,
    volume_min_scale: f64,
    ratio_min_scale: f64,
    ratios_required: u8,
    tempo_10s: bool,
    tempo_20s: bool,
    tempo_40s: bool,
    tempo_1m: bool,
    tempo_2m: bool,
    tempo_5m: bool,
}

impl Detection {
    fn of(o: &MicroPullbackOptions) -> Self {
        Self {
            price_min: o.price_min,
            price_max: o.price_max,
            return_min_scale: o.return_min_scale,
            volume_min_scale: o.volume_min_scale,
            ratio_min_scale: o.ratio_min_scale,
            ratios_required: o.ratios_required,
            tempo_10s: o.tempo_10s,
            tempo_20s: o.tempo_20s,
            tempo_40s: o.tempo_40s,
            tempo_1m: o.tempo_1m,
            tempo_2m: o.tempo_2m,
            tempo_5m: o.tempo_5m,
        }
    }

    fn config(&self) -> Config {
        let opts = MicroPullbackOptions {
            price_min: self.price_min,
            price_max: self.price_max,
            return_min_scale: self.return_min_scale,
            volume_min_scale: self.volume_min_scale,
            ratio_min_scale: self.ratio_min_scale,
            ratios_required: self.ratios_required,
            tempo_10s: self.tempo_10s,
            tempo_20s: self.tempo_20s,
            tempo_40s: self.tempo_40s,
            tempo_1m: self.tempo_1m,
            tempo_2m: self.tempo_2m,
            tempo_5m: self.tempo_5m,
            ..Default::default()
        }
        .sanitized();
        Config::DEFAULT.with_options(&opts)
    }
}

pub struct MicroPullbackNeeds;

impl ReplayDataNeeds for MicroPullbackNeeds {
    fn id(&self) -> &'static str {
        STRATEGY_ID
    }

    fn label(&self) -> &'static str {
        "Micro Pullback"
    }

    fn profile(&self, conn: &rusqlite::Connection) -> DataProfile {
        profile_of(&MicroPullbackOptions::load(conn))
    }

    fn scanner(&self, profile: &DataProfile) -> Box<dyn SymbolScanner> {
        let cfg = serde_json::from_value::<Detection>(profile.detection.clone())
            .map(|d| d.config())
            .unwrap_or(Config::DEFAULT);
        Box::new(MinuteScanner { cfg })
    }
}

/// The profile the saved options define.
pub fn profile_of(opts: &MicroPullbackOptions) -> DataProfile {
    let det = Detection::of(opts);
    let cfg = det.config();
    // No tempo can trip on a day that never trades its smallest minimum. With every
    // tempo off, nothing is reachable: the saturating cast makes that u64::MAX.
    let min_volume = cfg
        .active_tempos()
        .map(|t| t.volume_min * TUNING.abs_slack)
        .fold(f64::INFINITY, f64::min);
    DataProfile {
        strategy_id: STRATEGY_ID.to_string(),
        prescan_version: PRESCAN_VERSION,
        sessions: vec![EtWindow {
            start_min: opts.session_start_min,
            end_min: opts.session_end_min,
        }],
        universe: UniverseFilter {
            price_min: opts.price_min,
            price_max: opts.price_max,
            float_max: Some(opts.float_max),
            allow_unknown_float: opts.allow_unknown_float,
            min_session_volume: min_volume as u64,
        },
        // Spread, aggression and the Apex/Dip entries all read quotes.
        ticks: TickNeeds::TradesAndQuotes,
        pad: WindowPad {
            before_ms: i64::from(opts.replay_before_min) * 60_000,
            after_ms: i64::from(opts.replay_after_min) * 60_000,
        },
        max_symbols_per_day: usize::from(opts.replay_max_symbols),
        probe: Some(PROBE),
        probe_step_ms: ARM_HORIZON_MS,
        // The engine locks a ticker after its alert until the next session segment
        // (`time::session_at`: premarket, pre-open, open, after hours).
        lock_segments: vec![
            EtWindow { start_min: 240, end_min: 570 },
            EtWindow { start_min: 570, end_min: 590 },
            EtWindow { start_min: 590, end_min: 960 },
            EtWindow { start_min: 960, end_min: 1200 },
        ],
        max_alerts_per_symbol: 4,
        // Per lock segment, each probe covering 2 minutes of hits: 48 follows a
        // runner that trips minute after minute for ~1 h 30. Measured on 2026-06-29
        // (07:00–09:30) against the engine replayed on every print: at 16, UPC and
        // DCOY — 102 and 73 minute hits — ran out before their real alert.
        max_probes_per_symbol: 48,
        detection: serde_json::to_value(&det).unwrap_or_default(),
    }
}

struct MinuteScanner {
    cfg: Config,
}

impl SymbolScanner for MinuteScanner {
    fn scan(&self, bars: &[Bar]) -> Vec<Hit> {
        scan_minutes(&self.cfg, bars, TUNING)
    }

    fn confirm(&self, minutes: &[Bar], hit: &Hit, trades: &[TradeRow]) -> Option<Hit> {
        confirm_on_prints(&self.cfg, minutes, hit, trades)
    }

    fn confirm_lead_ms(&self) -> Option<i64> {
        Some(ARM_LEAD_MS)
    }
}

/// Replay the engine on real prints: 10-second bars built from `trades`, gates 2–3
/// at every close in the hit's horizon (EXACT thresholds — this is the tape the
/// engine reads), then the liquidity watch second by second. As in the engine,
/// detection pauses while a watch runs and resumes when it times out.
pub(crate) fn confirm_on_prints(
    cfg: &Config,
    minutes: &[Bar],
    hit: &Hit,
    trades: &[TradeRow],
) -> Option<Hit> {
    confirm_span(
        cfg,
        minutes,
        hit.alert_ms - ARM_LEAD_MS,
        hit.alert_ms + ARM_HORIZON_MS,
        trades,
    )
}

/// The engine's first alert with an arm at a 10 s close in `[from_ms, to_ms]`,
/// replayed on `trades` (which may start earlier: they feed the live baseline).
pub(crate) fn confirm_span(
    cfg: &Config,
    minutes: &[Bar],
    from_ms: i64,
    to_ms: i64,
    trades: &[TradeRow],
) -> Option<Hit> {
    let shape = TapeShape {
        kind: TapeKind::Prints,
        counts_trades: true,
    };
    let bars = ten_second_bars(trades);
    let idx = minutes.partition_point(|b| b.time.timestamp_millis() < from_ms);
    let seed = seed_before(minutes, idx.min(minutes.len().saturating_sub(1)));
    // Per-second (prints, dollars), for the watch.
    let mut seconds: std::collections::BTreeMap<i64, (u64, f64)> = Default::default();
    for t in trades {
        let e = seconds.entry(t.ts.div_euclid(1_000)).or_insert((0, 0.0));
        e.0 += 1;
        e.1 += t.price * t.size as f64;
    }
    let window = |now_s: i64, secs: i64| {
        let mut w = TapeWindow::default();
        for (_, (p, d)) in seconds.range(now_s - secs + 1..=now_s) {
            w.prints += p;
            w.active_seconds += 1;
            w.dollars += d;
            w.max_second_dollars = w.max_second_dollars.max(*d);
        }
        w
    };

    let mut resume_ms = from_ms;
    for j in 0..bars.len() {
        let close_ms = bars[j].time.timestamp_millis() + BAR_SECS * 1_000;
        if close_ms <= resume_ms || bars[j].time.timestamp_millis() < from_ms {
            continue;
        }
        if close_ms > to_ms {
            break;
        }
        if !(cfg.price_min..=cfg.price_max).contains(&bars[j].close) {
            continue;
        }
        let upto = &bars[..=j];
        let Some((tempo, m)) = cfg.active_tempos().find_map(|t| {
            eval_tempo(upto, t, cfg, seed.as_ref(), shape)
                .filter(|m| gate3_trips(m, t, cfg.ratios_required))
                .map(|m| (*t, m))
        }) else {
            continue;
        };
        // Armed at this close: the watch polls every second for `watch_max_secs`.
        let armed_s = close_ms / 1_000;
        for s in armed_s..=armed_s + cfg.watch_max_secs {
            if confirm_liquidity(cfg, shape, &|w| window(s, w)).is_some() {
                return Some(Hit {
                    alert_ms: s * 1_000,
                    score: m.volume_ratio,
                    reason: format!(
                        "{} · vol×{:.1} rng×{:.1} +{:.1}% · confirmé sur prints",
                        tempo_label(tempo.secs),
                        m.volume_ratio,
                        m.range_ratio,
                        m.price_return
                    ),
                });
            }
        }
        resume_ms = (armed_s + cfg.watch_max_secs) * 1_000;
    }
    None
}

/// 10-second OHLCV bars (volume-weighted, with trade counts) from prints.
fn ten_second_bars(trades: &[TradeRow]) -> Vec<Bar> {
    let mut out: Vec<Bar> = Vec::new();
    let mut dollars = 0.0;
    for t in trades {
        let start = t.ts.div_euclid(BAR_SECS * 1_000) * BAR_SECS * 1_000;
        let size = t.size.max(0) as u64;
        match out.last_mut() {
            Some(b) if b.time.timestamp_millis() == start => {
                b.high = b.high.max(t.price);
                b.low = b.low.min(t.price);
                b.close = t.price;
                b.volume += size;
                b.trade_count = Some(b.trade_count.unwrap_or(0) + 1);
                dollars += t.price * size as f64;
                b.vwap = (b.volume > 0).then(|| dollars / b.volume as f64);
            }
            _ => {
                dollars = t.price * size as f64;
                out.push(Bar {
                    time: chrono::DateTime::from_timestamp_millis(start).unwrap_or_default(),
                    open: t.price,
                    high: t.price,
                    low: t.price,
                    close: t.price,
                    volume: size,
                    vwap: Some(t.price),
                    trade_count: Some(1),
                });
            }
        }
    }
    out
}

/// One hit per minute where any active tempo trips (the strongest is kept).
pub(crate) fn scan_minutes(cfg: &Config, bars: &[Bar], tuning: Tuning) -> Vec<Hit> {
    let mut out = Vec::new();
    for i in 0..bars.len() {
        let b = &bars[i];
        if b.high < cfg.price_min || b.low > cfg.price_max {
            continue;
        }
        let mut best: Option<(f64, String)> = None;
        for tempo in cfg.active_tempos() {
            let Some(m) = minute_metrics(cfg, bars, i, tempo) else {
                continue;
            };
            if !gate3_trips(&m, &relaxed(tempo, tuning), cfg.ratios_required) {
                continue;
            }
            if tempo.secs < 60 && tuning.submin_minute_ratio > 0.0 {
                let Some(m60) = minute_level(cfg, bars, i) else {
                    continue;
                };
                if m60.volume_ratio < tempo.volume_ratio_min * tuning.submin_minute_ratio {
                    continue;
                }
            }
            if best.as_ref().map_or(true, |(s, _)| m.volume_ratio > *s) {
                best = Some((
                    m.volume_ratio,
                    format!(
                        "{} · vol×{:.1} rng×{:.1} +{:.1}%",
                        tempo_label(tempo.secs),
                        m.volume_ratio,
                        m.range_ratio,
                        m.price_return
                    ),
                ));
            }
        }
        if let Some((score, reason)) = best {
            out.push(Hit {
                alert_ms: b.time.timestamp_millis(),
                score,
                reason,
            });
        }
    }
    out
}

fn tempo_label(secs: i64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m", secs / 60)
    }
}

fn relaxed(t: &Tempo, k: Tuning) -> Tempo {
    Tempo {
        return_min_pct: t.return_min_pct * k.abs_slack,
        volume_min: t.volume_min * k.abs_slack,
        dollar_volume_min: t.dollar_volume_min * k.abs_slack,
        volume_ratio_min: t.volume_ratio_min * k.ratio_slack,
        trade_rate_ratio_min: t.trade_rate_ratio_min * k.ratio_slack,
        range_ratio_min: t.range_ratio_min * k.ratio_slack,
        ..*t
    }
}

/// The minute `bars[i]` against the plain (unscaled) minute baseline.
fn minute_level(cfg: &Config, bars: &[Bar], i: usize) -> Option<TempoMetrics> {
    let one_minute = Tempo {
        secs: 60,
        ..cfg.tempos[0]
    };
    minute_metrics(cfg, bars, i, &one_minute)
}

/// The engine's `eval_tempo`, on minute bars ending at `bars[i]`.
fn minute_metrics(cfg: &Config, bars: &[Bar], i: usize, tempo: &Tempo) -> Option<TempoMetrics> {
    let t = tempo.secs;
    let end = bars[i].time;
    let (current, base_vol, base_trades, base_range_pct) = if t >= 60 {
        // Current window: the minutes covering the last `t` seconds.
        let cur_lo = end - ChronoDuration::seconds(t - 60);
        let cur_start = bars[..=i].partition_point(|b| b.time < cur_lo);
        let base_lo = cur_lo - ChronoDuration::seconds(baseline_secs(t));
        let base_start = bars[..cur_start].partition_point(|b| b.time < base_lo);
        let (v, tr, r) = match live_baseline(&bars[base_start..cur_start], t) {
            Some(b) => b,
            None => seed_baseline(&seed_before(bars, cur_start)?, t),
        };
        (&bars[cur_start..=i], v, tr, r)
    } else {
        // Sub-minute tempo: the minute is an upper bound of any of its slices.
        let (v, tr, r) = seed_baseline(&seed_before(bars, i)?, t);
        (&bars[i..=i], v, tr, r)
    };

    let (cur_vol, cur_dollars, cur_trades, cur_range_pct, _ret) = window_metrics(current);
    // Best upward swing the sliding 10 s window could have caught in here.
    let low = current.iter().map(|b| b.low).fold(f64::MAX, f64::min);
    let high = current.iter().map(|b| b.high).fold(f64::MIN, f64::max);
    let ret_ub = if low > 0.0 {
        (high - low) / low * 100.0
    } else {
        0.0
    };
    let counts_trades = current.iter().any(|b| b.trade_count.is_some());

    let scale = t as f64 / 10.0;
    let vol_floor = cfg.baseline_volume_floor_10s * scale;
    Some(TempoMetrics {
        current_volume: cur_vol,
        current_dollars: cur_dollars,
        current_trades: cur_trades,
        price_return: ret_ub,
        volume_ratio: cur_vol / base_vol.max(vol_floor),
        trade_rate_ratio: counts_trades.then(|| {
            (cur_trades as f64 / t as f64)
                / (base_trades / t as f64).max(cfg.baseline_trade_rate_floor)
        }),
        range_ratio: cur_range_pct / base_range_pct.max(cfg.baseline_range_floor_pct),
    })
}

/// The engine's 1-minute seed, from the minutes just before `idx`.
fn seed_before(bars: &[Bar], idx: usize) -> Option<super::Seed> {
    let end = bars.get(idx)?.time;
    let lo = end - ChronoDuration::minutes(SUBMINUTE_BASELINE_MIN);
    let start = bars[..idx].partition_point(|b| b.time < lo);
    seed_from_minutes(&bars[start..idx])
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, TimeZone, Utc};

    fn hm_of(ms: i64) -> String {
        let m = crate::time::et_minutes(chrono::DateTime::from_timestamp_millis(ms).unwrap());
        let s = ms.div_euclid(1_000) % 60;
        format!("{:02}:{:02}:{:02}", m / 60, m % 60, s)
    }

    /// 2026-06-01 04:00 ET (EDT) = 08:00 UTC.
    fn at(min_after_4am: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 6, 1, 8, 0, 0).unwrap() + ChronoDuration::minutes(min_after_4am)
    }

    fn bar(m: i64, o: f64, h: f64, l: f64, c: f64, v: u64, n: u64) -> Bar {
        Bar {
            time: at(m),
            open: o,
            high: h,
            low: l,
            close: c,
            volume: v,
            vwap: Some((h + l) / 2.0),
            trade_count: Some(n),
        }
    }

    /// 30 quiet minutes at $5, then (optionally) one ignition minute.
    fn day(price: f64, ignite: bool) -> Vec<Bar> {
        let mut v: Vec<Bar> = (0..30)
            .map(|m| bar(m, price, price * 1.002, price * 0.999, price, 2_000, 20))
            .collect();
        if ignite {
            v.push(bar(30, price, price * 1.10, price * 0.998, price * 1.09, 60_000, 600));
        }
        v
    }

    fn scan(opts: &MicroPullbackOptions, bars: &[Bar]) -> Vec<Hit> {
        let needs = MicroPullbackNeeds;
        needs.scanner(&profile_of(opts)).scan(bars)
    }

    #[test]
    fn an_ignition_minute_is_found() {
        let hits = scan(&MicroPullbackOptions::default(), &day(5.0, true));
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].alert_ms, at(30).timestamp_millis());
        assert!(hits[0].score > 10.0);
    }

    #[test]
    fn a_quiet_tape_yields_nothing() {
        assert!(scan(&MicroPullbackOptions::default(), &day(5.0, false)).is_empty());
    }

    #[test]
    fn the_price_band_comes_from_the_settings() {
        assert!(scan(&MicroPullbackOptions::default(), &day(50.0, true)).is_empty());
        let wide = MicroPullbackOptions {
            price_max: 100.0,
            ..Default::default()
        };
        assert_eq!(scan(&wide, &day(50.0, true)).len(), 1);
    }

    #[test]
    fn switched_off_tempos_are_not_scanned() {
        let muted = MicroPullbackOptions {
            tempo_10s: false,
            tempo_20s: false,
            tempo_40s: false,
            tempo_1m: false,
            tempo_2m: false,
            tempo_5m: false,
            ..Default::default()
        };
        assert!(scan(&muted, &day(5.0, true)).is_empty());
        assert_eq!(profile_of(&muted).universe.min_session_volume, u64::MAX);
    }

    #[test]
    fn stricter_settings_can_drop_a_hit() {
        let strict = MicroPullbackOptions {
            volume_min_scale: 10.0,
            ..Default::default()
        };
        assert!(scan(&strict, &day(5.0, true)).is_empty());
    }

    /// Calibration against live-recorded Micro Pullback alerts. Ignored by default:
    ///   TAGDASH_E2E_APP=<data root> TAGDASH_E2E_DB=<copy of cache.db>
    ///   TAGDASH_E2E_OUT=<scratch dir> TAGDASH_E2E_DAY=YYYY-MM-DD
    ///   cargo test --lib prescan_calibration -- --ignored --nocapture
    /// Downloads the day's candidate minutes once (cached in OUT), then prints, per
    /// tuning: recall on the day's live alerts, symbols hit, tick minutes wanted.
    #[tokio::test]
    #[ignore]
    async fn prescan_calibration() {
        use crate::replay_data::plan;
        use crate::replay_data::prepare;
        use crate::replay_data::store::{buckets_of, MarketRow};
        use std::collections::HashMap;

        type Row = (i64, f64, f64, f64, f64, u64, Option<u64>, Option<f64>);
        let env = |k: &str| std::env::var(k).unwrap_or_else(|_| panic!("{k} manquant"));
        let day = env("TAGDASH_E2E_DAY");
        let out = std::path::PathBuf::from(env("TAGDASH_E2E_OUT"));
        let db = crate::local_db::open_harness(std::path::Path::new(&env("TAGDASH_E2E_DB")));
        let opts = MicroPullbackOptions::load(&db);
        let profile = profile_of(&opts);
        let cfg = serde_json::from_value::<Detection>(profile.detection.clone())
            .unwrap()
            .config();
        let floats: HashMap<String, u64> = crate::local_db::universe_repository::get_all(&db)
            .unwrap()
            .into_iter()
            .filter_map(|a| a.float_shares.filter(|f| *f > 0).map(|f| (a.symbol, f as u64)))
            .collect();
        let bounds = prepare::day_bounds(&day).unwrap();

        let cache = out.join(format!("calib-{day}.json"));
        let (market, bars): (Vec<(String, f64, f64, f64, f64, i64)>, HashMap<String, Vec<Row>>) =
            if let Ok(txt) = std::fs::read_to_string(&cache) {
                serde_json::from_str(&txt).unwrap()
            } else {
                let creds = crate::review::sip::creds(&crate::config::secrets::load(
                    std::path::Path::new(&env("TAGDASH_E2E_APP")),
                ))
                .unwrap();
                let shared = prepare::ReplayDataShared::default();
                let symbols = crate::local_db::universe_repository::get_active_symbols(&db).unwrap();
                let rows = prepare::fetch_market(&creds, &symbols, &day, &shared).await.unwrap();
                let cands = plan::candidates(std::slice::from_ref(&profile), &rows, &floats);
                println!("marché {} · candidats {}", rows.len(), cands.len());
                let got = prepare::fetch_bars(&creds, &cands, "1Min", bounds, &shared, "minutes")
                    .await
                    .unwrap();
                let market: Vec<_> = rows
                    .iter()
                    .map(|m| (m.symbol.clone(), m.o, m.h, m.l, m.c, m.v))
                    .collect();
                let bars: HashMap<String, Vec<Row>> = got
                    .into_iter()
                    .map(|(s, v)| {
                        let r = v
                            .into_iter()
                            .map(|b| {
                                (
                                    b.time.timestamp_millis(),
                                    b.open,
                                    b.high,
                                    b.low,
                                    b.close,
                                    b.volume,
                                    b.trade_count,
                                    b.vwap,
                                )
                            })
                            .collect();
                        (s, r)
                    })
                    .collect();
                std::fs::write(&cache, serde_json::to_string(&(&market, &bars)).unwrap()).unwrap();
                (market, bars)
            };
        let market: HashMap<String, MarketRow> = market
            .into_iter()
            .map(|(symbol, o, h, l, c, v)| {
                (
                    symbol.clone(),
                    MarketRow { symbol, prev_close: None, o, h, l, c, v },
                )
            })
            .collect();
        let bars: HashMap<String, Vec<Bar>> = bars
            .into_iter()
            .map(|(s, rows)| {
                let v = rows
                    .into_iter()
                    .map(|(t, o, h, l, c, v, n, vw)| Bar {
                        time: chrono::DateTime::from_timestamp_millis(t).unwrap(),
                        open: o,
                        high: h,
                        low: l,
                        close: c,
                        volume: v,
                        vwap: vw,
                        trade_count: n,
                    })
                    .collect();
                (s, v)
            })
            .collect();

        let alerts: Vec<(String, i64)> = {
            let mut st = db
                .prepare(
                    "SELECT symbol, opened_ms FROM review_sessions
                     WHERE mode='live' AND day=?1 AND strategy_id='micro_pullback' ORDER BY opened_ms",
                )
                .unwrap();
            st.query_map([&day], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .flatten()
                .collect()
        };
        let eligible: Vec<&String> = bars
            .keys()
            .filter(|s| {
                market.get(*s).is_some_and(|m| {
                    plan::eligible(&profile, m, floats.get(*s).copied(), &buckets_of(&bars[*s]))
                })
            })
            .collect();
        println!(
            "{day} · {} alertes live · {} symboles avec minutes · {} éligibles · sessions {:?}",
            alerts.len(),
            bars.len(),
            eligible.len(),
            profile.sessions
        );

        let variants = [
            ("actuel", TUNING),
            ("sub×0.25", Tuning { submin_minute_ratio: 0.25, ..TUNING }),
            ("sub×0.5", Tuning { submin_minute_ratio: 0.5, ..TUNING }),
            ("sub×0.75", Tuning { submin_minute_ratio: 0.75, ..TUNING }),
            ("slack .9/.8 sub×.5", Tuning { abs_slack: 0.9, ratio_slack: 0.8, submin_minute_ratio: 0.5 }),
            ("exact 1/1 sub×.5", Tuning { abs_slack: 1.0, ratio_slack: 1.0, submin_minute_ratio: 0.5 }),
            ("exact 1/1 sub×1", Tuning { abs_slack: 1.0, ratio_slack: 1.0, submin_minute_ratio: 1.0 }),
        ];
        // Per-alert diagnosis on the production tuning: is the symbol there, does it
        // hit near the alert, and where does it rank against the day's cap?
        {
            let mut hits: Vec<(String, Hit)> = Vec::new();
            for s in &eligible {
                for h in scan_minutes(&cfg, &bars[*s], TUNING) {
                    if profile.in_sessions(h.alert_ms) {
                        hits.push(((*s).clone(), h));
                    }
                }
            }
            let mut best: HashMap<&str, f64> = HashMap::new();
            for (s, h) in &hits {
                let e = best.entry(s.as_str()).or_insert(0.0);
                *e = e.max(h.score);
            }
            let mut ranked: Vec<(&str, f64)> = best.into_iter().collect();
            ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
            for (sym, opened) in &alerts {
                let near = |at: i64| {
                    hits.iter()
                        .filter(|(s, _)| s == sym)
                        .map(|(_, h)| (h.alert_ms - at) / 60_000)
                        .min_by_key(|d| d.abs())
                };
                let rank = ranked.iter().position(|(s, _)| s == sym);
                println!(
                    "   {sym:>5} {} · minutes {} · éligible {} · hits {} · plus proche (min) t={:?} t-15={:?} · rang {:?}",
                    crate::time::et_minutes(chrono::DateTime::from_timestamp_millis(*opened).unwrap()) / 60,
                    bars.contains_key(sym),
                    eligible.iter().any(|e| *e == sym),
                    hits.iter().filter(|(s, _)| s == sym).count(),
                    near(*opened),
                    near(opened - 15 * 60_000),
                    rank
                );
            }
        }
        for (name, tuning) in variants {
            let mut hits: Vec<(String, Hit)> = Vec::new();
            for s in &eligible {
                for h in scan_minutes(&cfg, &bars[*s], tuning) {
                    if profile.in_sessions(h.alert_ms) {
                        hits.push(((*s).clone(), h));
                    }
                }
            }
            let syms: std::collections::HashSet<&String> = hits.iter().map(|(s, _)| s).collect();
            let wins = plan::windows(&profile, &hits, bounds);
            let minutes: i64 = wins.values().flatten().map(|(a, b)| (b - a) / 60_000).sum();
            let covered = |sym: &str, at: i64| {
                wins.get(sym).is_some_and(|w| w.iter().any(|(a, b)| *a <= at && at < *b))
            };
            let mut missed = Vec::new();
            let mut hit = 0;
            for (sym, opened) in &alerts {
                if covered(sym, *opened) || covered(sym, opened - 15 * 60_000) {
                    hit += 1;
                } else {
                    missed.push(sym.as_str());
                }
            }
            println!(
                "{name:>20} · rappel {hit}/{} · {} symboles touchés · {} fenêtres gardées ({} min de ticks) · ratés {:?}",
                alerts.len(),
                syms.len(),
                wins.len(),
                minutes,
                missed
            );
        }

        // Production path: minute hits confirmed on real prints.
        let creds = crate::review::sip::creds(&crate::config::secrets::load(
            std::path::Path::new(&env("TAGDASH_E2E_APP")),
        ))
        .unwrap();
        let mut raw: std::collections::BTreeMap<String, Vec<Hit>> = Default::default();
        for s in &eligible {
            let hs: Vec<Hit> = scan_minutes(&cfg, &bars[*s], TUNING)
                .into_iter()
                .filter(|h| profile.in_sessions(h.alert_ms))
                .collect();
            if !hs.is_empty() {
                raw.insert((*s).clone(), hs);
            }
        }
        let scanner = MicroPullbackNeeds.scanner(&profile);
        let shared = prepare::ReplayDataShared::default();
        let t0 = std::time::Instant::now();
        let confirmed =
            prepare::confirm_hits(&creds, scanner.as_ref(), &profile, &raw, &bars, bounds, &shared)
                .await
                .unwrap();
        let wins = plan::windows(&profile, &confirmed, bounds);
        let minutes: i64 = wins.values().flatten().map(|(a, b)| (b - a) / 60_000).sum();
        let covered = |sym: &str, at: i64| {
            wins.get(sym).is_some_and(|w| w.iter().any(|(a, b)| *a <= at && at < *b))
        };
        let missed: Vec<&str> = alerts
            .iter()
            .filter(|(s, o)| !covered(s, *o) && !covered(s, o - 15 * 60_000))
            .map(|(s, _)| s.as_str())
            .collect();
        // Replay starts at 04:00 with a fresh engine; the live app may have started
        // later, so a replay alert EARLIER in the same lock segment is the expected
        // outcome, not a miss.
        let consistent = |sym: &str, a: i64| {
            let seg = plan::segment_of(&profile, a);
            confirmed.iter().any(|(s, h)| {
                s == sym && h.alert_ms <= a + 60_000 && plan::segment_of(&profile, h.alert_ms) == seg
            })
        };
        let incoherent: Vec<&str> = alerts
            .iter()
            .filter(|(s, o)| {
                let a = o - 15 * 60_000; // Massive Starter live feed: 15 min late
                !covered(s, a) && !covered(s, *o) && !consistent(s, a) && !consistent(s, *o)
            })
            .map(|(s, _)| s.as_str())
            .collect();
        let symbols: std::collections::HashSet<&String> = confirmed.iter().map(|(s, _)| s).collect();
        println!(
            "CONFIRMÉ en {:.0}s · {} requêtes · {} tickers sondés · {} alertes confirmées sur {} symboles · {} gardés · {} min de ticks · couvertes {}/{} · cohérentes {}/{} · incohérentes {:?}",
            t0.elapsed().as_secs_f32(),
            crate::alpaca::ratelimit::requests(),
            raw.len(),
            confirmed.len(),
            symbols.len(),
            wins.len(),
            minutes,
            alerts.len() - missed.len(),
            alerts.len(),
            alerts.len() - incoherent.len(),
            alerts.len(),
            incoherent
        );
        for (sym, h) in confirmed.iter().filter(|(s, _)| alerts.iter().any(|(a, _)| a == s)) {
            println!(
                "   {sym:>5} {:02}:{:02} {}",
                crate::time::et_minutes(chrono::DateTime::from_timestamp_millis(h.alert_ms).unwrap()) / 60,
                crate::time::et_minutes(chrono::DateTime::from_timestamp_millis(h.alert_ms).unwrap()) % 60,
                h.reason
            );
        }

        // Ground truth (TAGDASH_E2E_TRUTH=N): the engine replayed on EVERY print of
        // the session for N symbols with a minute hit (the confirmed ones first,
        // then the strongest others), against what the probes found.
        if let Some(n) = std::env::var("TAGDASH_E2E_TRUTH").ok().and_then(|v| v.parse::<usize>().ok()) {
            let (s0, s1) = profile
                .sessions
                .iter()
                .map(|w| {
                    (
                        bounds.0 + (i64::from(w.start_min) - 240) * 60_000,
                        bounds.0 + (i64::from(w.end_min) - 240) * 60_000,
                    )
                })
                .fold((i64::MAX, i64::MIN), |a, b| (a.0.min(b.0), a.1.max(b.1)));
            let probe_first: HashMap<&str, i64> = confirmed.iter().fold(HashMap::new(), |mut m, (s, h)| {
                m.entry(s.as_str()).or_insert(h.alert_ms);
                m
            });
            let mut sample: Vec<&String> = raw.keys().filter(|s| probe_first.contains_key(s.as_str())).collect();
            let mut others: Vec<(&String, f64)> = raw
                .iter()
                .filter(|(s, _)| !probe_first.contains_key(s.as_str()))
                .map(|(s, hs)| (s, hs.iter().map(|h| h.score).fold(0.0, f64::max)))
                .collect();
            others.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
            sample.extend(others.into_iter().map(|(s, _)| s));
            // Then the eligible names the minute pre-scan never flagged: the only
            // way to see what IT misses.
            let mut silent: Vec<&String> = eligible.iter().copied().filter(|s| !raw.contains_key(*s)).collect();
            silent.sort();
            sample.extend(silent);
            sample.truncate(n);
            let t0 = std::time::Instant::now();
            let req0 = crate::alpaca::ratelimit::requests();
            let (mut agree, mut early, mut late, mut missed, mut extra) = (0, 0, 0, Vec::new(), 0);
            for chunk in sample.chunks(10) {
                let syms: Vec<String> = chunk.iter().map(|s| (*s).clone()).collect();
                let trades = prepare::fetch_trades_multi(&creds, &syms, (s0 - 20 * 60_000, s1)).await.unwrap();
                for s in chunk {
                    let t = trades.get(*s).cloned().unwrap_or_default();
                    let truth = confirm_span(&cfg, &bars[*s], s0, s1, &t);
                    match (truth, probe_first.get(s.as_str())) {
                        (Some(h), Some(p)) if (h.alert_ms - p).abs() <= 60_000 => agree += 1,
                        (Some(h), Some(p)) if h.alert_ms < *p => {
                            early += 1;
                            println!("   tôt   {s:>5} vérité {} sonde {}", hm_of(h.alert_ms), hm_of(*p));
                        }
                        (Some(h), Some(p)) => {
                            late += 1;
                            println!("   tard  {s:>5} vérité {} sonde {}", hm_of(h.alert_ms), hm_of(*p));
                        }
                        (Some(h), None) => {
                            let near = raw.get(*s).and_then(|hs| {
                                hs.iter().map(|x| (x.alert_ms - h.alert_ms) / 1_000).min_by_key(|d| d.abs())
                            });
                            println!(
                                "   RATÉ  {s:>5} vérité {} · {} · hit minute le plus proche {:?} s · {} hits",
                                hm_of(h.alert_ms),
                                h.reason,
                                near,
                                raw.get(*s).map_or(0, Vec::len)
                            );
                            missed.push((*s).clone());
                        }
                        (None, Some(p)) => {
                            extra += 1;
                            println!("   extra {s:>5} sonde {} sans alerte en continu", hm_of(*p));
                        }
                        (None, None) => {}
                    }
                }
            }
            println!(
                "VÉRITÉ {} symboles en {:.0}s ({} requêtes) · d'accord {agree} · vérité plus tôt {early} · plus tard {late} · sondes en trop {extra} · RATÉS {} {:?}",
                sample.len(),
                t0.elapsed().as_secs_f32(),
                crate::alpaca::ratelimit::requests() - req0,
                missed.len(),
                missed
            );
        }
    }

    #[test]
    fn the_profile_follows_the_saved_settings() {
        let pm = MicroPullbackOptions {
            session_start_min: 240,
            session_end_min: 570,
            replay_after_min: 15,
            ..Default::default()
        };
        let p = profile_of(&pm);
        assert_eq!(p.sessions, vec![EtWindow { start_min: 240, end_min: 570 }]);
        assert_eq!(p.pad.after_ms, 15 * 60_000);
        assert_ne!(p.fingerprint(), profile_of(&MicroPullbackOptions::default()).fingerprint());

        // Settings the pre-scan does not read leave the fingerprint alone.
        let llm = MicroPullbackOptions {
            llm_auto: true,
            ntfy_alerts: true,
            watch_max_secs: 120,
            ..Default::default()
        };
        assert_eq!(
            profile_of(&llm).fingerprint(),
            profile_of(&MicroPullbackOptions::default()).fingerprint()
        );
    }
}
