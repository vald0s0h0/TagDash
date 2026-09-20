// Dip watch book — the hot-path half of Dip.
//
// Lives inside `MarketState` and is fed from the very ingest calls that already
// hold its write lock (`on_trade`, `on_second_aggregate`, `on_replay_trade`), so
// watching the price path costs no extra lock, no allocation after the first print
// of a symbol, and no polling delay: the print that turns the move is the print
// that sends the trigger.
//
// ── WHAT IS MEASURED ──────────────────────────────────────────────────────────
// The trader draws one horizontal zone — a level plus a thickness — on a ticker,
// and the side it is traded from is decided by where the price already is: a zone
// UNDER the price is a support (it is bought back), a zone OVER it is a resistance
// (it is sold back). Then the book waits for one shape, the snake's strike:
//
//        outside ──enters the zone──▶ inside ──turns back──▶ ENTRY
//           ▲                           │  │
//           │                           │  └─ too fast / too long ─▶ spent ─┐
//           └──── leaves the zone ──────┴──────────────────────────────────-┘
//                                       │
//                            past the far edge ─▶ the level is gone (disarm)
//
// The strike has to be VIOLENT, and violence is relative to the ticker itself: the
// travel over the measuring window is compared with what that ticker ordinarily
// travels in the same time, read off ATR(14) on 10-second bars (the executor
// refreshes it once a second — see `commands::dip`). Ten-second bars are the unit
// because this is micro-structure: a 1.5 s window is a seventh of one bar.
//
// Both ends of that ratio matter. Under the floor nothing happened worth trading.
// Over the ceiling the move is out of reach — by the time an order is acknowledged
// the price is somewhere else entirely — so the attack is abandoned rather than
// chased. That is the "juste milieu": fast enough to snap back, slow enough to catch.
//
// ── CONTINUOUS WATCH ──────────────────────────────────────────────────────────
// Trackers exist for the displayed (focus) tickers whenever Dip is enabled, so the
// price path is already long enough to measure a speed the moment a zone is placed.
// Disabled in Settings, the book holds no tracker at all and every hook returns on
// a single bool.

use std::collections::{HashMap, HashSet, VecDeque};

use tokio::sync::mpsc::UnboundedSender;

use super::options::DipOptions;
use crate::types::Side;

/// Prices closer than this are equal.
const PRICE_EPS: f64 = 1e-9;
/// Hard bound on the retained price path, whatever the tape does.
const MAX_POINTS: usize = 4_096;
/// The ATR the speed baseline is read from is a 10-second bar's range.
const BASELINE_BAR_MS: f64 = 10_000.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DipParams {
    pub speed_window_ms: i64,
    pub min_speed_ratio: f64,
    pub max_speed_ratio: f64,
    pub min_penetration: f64,
    pub attack_max_ms: i64,
    pub reversal: f64,
    pub break_through: f64,
}

impl Default for DipParams {
    fn default() -> Self {
        Self::from_options(&DipOptions::default())
    }
}

impl DipParams {
    pub fn from_options(o: &DipOptions) -> Self {
        Self {
            speed_window_ms: o.speed_window_ms as i64,
            min_speed_ratio: o.min_speed_ratio,
            max_speed_ratio: o.max_speed_ratio,
            min_penetration: o.min_penetration(),
            attack_max_ms: o.attack_max_ms as i64,
            reversal: o.reversal(),
            break_through: o.break_through(),
        }
    }
}

/// What the watch book hands the executor when an attack turns.
#[derive(Debug, Clone)]
pub struct DipTrigger {
    pub arm_id: u64,
    pub symbol: String,
    pub side: Side,
    /// The print that turned — where the order is decided.
    pub price: f64,
    /// Furthest the attack reached inside the zone.
    pub extreme: f64,
    /// How deep that was, from the edge it entered by ($).
    pub penetration: f64,
    /// Fastest approach seen during the attack, in ticker-ordinary travels.
    pub speed_ratio: f64,
    pub ts_ms: i64,
}

/// Why an attack (or the whole zone) came to nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LapseReason {
    /// The price went through the far edge: the level itself is gone.
    Broken,
    /// Out of reach — TagDash has no latency budget for this one.
    TooFast,
    /// The price entered and simply stayed. That is not a strike.
    Fizzled,
}

#[derive(Debug, Clone)]
pub struct DipLapse {
    pub arm_id: u64,
    pub symbol: String,
    pub reason: LapseReason,
    /// One phrase for the notice, built where the numbers are.
    pub detail: String,
    /// True when the zone itself is finished, not just this attack.
    pub disarm: bool,
    pub ts_ms: i64,
}

#[derive(Debug, Clone)]
pub enum DipSignal {
    Entry(DipTrigger),
    Lapse(DipLapse),
}

/// Where an armed zone stands with respect to the price.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Attack {
    /// The price is out of the zone: the next entry starts an attack.
    Outside,
    /// The price is in the zone, since `entered_ms`.
    Inside {
        entered_ms: i64,
        /// Furthest price reached — the low for a support, the high for a resistance.
        extreme: f64,
        /// Fastest approach measured since the zone was entered.
        peak_ratio: f64,
    },
    /// This attack is over (too fast, or too long) but the zone still stands: it
    /// re-arms when the price leaves.
    Spent,
}

/// The one zone the trader has placed.
#[derive(Debug, Clone)]
pub struct ArmedZone {
    pub arm_id: u64,
    pub symbol: String,
    /// Long for a support (the price is above it), Short for a resistance.
    pub side: Side,
    pub low: f64,
    pub high: f64,
    /// False while the ATR bracket is under the minimum distance: the zone is
    /// watched and drawn, but nothing fires.
    pub tradeable: bool,
    /// ATR(14) on 10-second bars, refreshed by the executor — the speed baseline.
    pub atr: f64,
    /// No trigger before this event time (ms).
    pub not_before_ms: i64,
    state: Attack,
}

impl ArmedZone {
    pub fn new(
        arm_id: u64,
        symbol: String,
        side: Side,
        low: f64,
        high: f64,
        tradeable: bool,
        atr: f64,
    ) -> Self {
        Self {
            arm_id,
            symbol,
            side,
            low,
            high,
            tradeable,
            atr,
            not_before_ms: 0,
            state: Attack::Outside,
        }
    }

    /// True while the price is inside the zone with a live attack running.
    pub fn attacking(&self) -> bool {
        matches!(self.state, Attack::Inside { .. })
    }

    /// Fastest approach of the attack under way, in ticker-ordinary travels.
    pub fn peak_ratio(&self) -> f64 {
        match self.state {
            Attack::Inside { peak_ratio, .. } => peak_ratio,
            _ => 0.0,
        }
    }

    /// The edge an attack comes in by — the one nearest the price before it.
    fn near_edge(&self) -> f64 {
        match self.side {
            Side::Long => self.high,
            Side::Short => self.low,
        }
    }
}

/// Recent price path of one ticker. Every operation is O(1) amortised.
#[derive(Debug, Default)]
pub struct PathTracker {
    /// (event time ms, price), oldest → newest.
    points: VecDeque<(i64, f64)>,
    /// Newest event time seen (ms). The window slides on this, never on the wall
    /// clock, so a delayed or replayed tape is measured in its own time.
    now_ms: i64,
}

impl PathTracker {
    pub fn push(&mut self, ts_ms: i64, price: f64, window_ms: i64) {
        self.now_ms = self.now_ms.max(ts_ms);
        self.points.push_back((ts_ms, price));
        // Keep exactly one point older than the window — it IS the reference price,
        // so the lookup below stays O(1) however fast the tape runs.
        let cutoff = self.now_ms - window_ms.max(1);
        while self.points.len() >= 2 && self.points[1].0 <= cutoff {
            self.points.pop_front();
        }
        while self.points.len() > MAX_POINTS {
            self.points.pop_front();
        }
    }

    /// The price one window ago. None until the path reaches that far back — a
    /// speed cannot be claimed over a window the ticker has not been watched for.
    pub fn price_at(&self, window_ms: i64) -> Option<f64> {
        let cutoff = self.now_ms - window_ms.max(1);
        self.points
            .front()
            .filter(|(ts, _)| *ts <= cutoff)
            .map(|(_, p)| *p)
    }

    /// Travel toward the zone over the window, as a multiple of what this ticker
    /// ordinarily travels in the same time (its ATR on 10-second bars, prorated).
    /// 0 when the price is moving away from the zone; None with no baseline yet.
    pub fn speed_ratio(&self, window_ms: i64, atr: f64, side: Side, price: f64) -> Option<f64> {
        let baseline = atr * window_ms.max(1) as f64 / BASELINE_BAR_MS;
        if !baseline.is_finite() || baseline <= 0.0 {
            return None;
        }
        let reference = self.price_at(window_ms)?;
        let travel = match side {
            // A support is attacked from above: the flush comes down.
            Side::Long => reference - price,
            // A resistance is attacked from below: the push comes up.
            Side::Short => price - reference,
        };
        Some((travel / baseline).max(0.0))
    }
}

/// All price paths plus the (single) armed zone.
#[derive(Debug, Default)]
pub struct DipBook {
    enabled: bool,
    params: DipParams,
    watch: HashSet<String>,
    trackers: HashMap<String, PathTracker>,
    armed: Option<ArmedZone>,
    tx: Option<UnboundedSender<DipSignal>>,
}

impl DipBook {
    pub fn set_sender(&mut self, tx: UnboundedSender<DipSignal>) {
        self.tx = Some(tx);
    }

    pub fn configure(&mut self, enabled: bool, params: DipParams) {
        self.enabled = enabled;
        self.params = params;
        if !enabled {
            self.armed = None;
        }
        self.rebuild();
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Replace the continuously watched tickers (the ones on screen).
    pub fn set_watch<I: IntoIterator<Item = String>>(&mut self, symbols: I) {
        self.watch = symbols.into_iter().collect();
        self.rebuild();
    }

    fn rebuild(&mut self) {
        if !self.enabled {
            self.trackers.clear();
            return;
        }
        let armed = self.armed.as_ref().map(|z| z.symbol.clone());
        let watch = &self.watch;
        self.trackers
            .retain(|s, _| watch.contains(s) || armed.as_deref() == Some(s.as_str()));
        for s in watch.iter().cloned().chain(armed) {
            self.trackers.entry(s).or_default();
        }
    }

    pub fn arm(&mut self, zone: ArmedZone) -> bool {
        if !self.enabled {
            return false;
        }
        self.armed = Some(zone);
        self.rebuild();
        true
    }

    pub fn disarm(&mut self) {
        self.armed = None;
        self.rebuild();
    }

    /// Disarm only if `arm_id` is the one armed — a newer zone survives.
    pub fn disarm_id(&mut self, arm_id: u64) {
        if self.armed.as_ref().is_some_and(|z| z.arm_id == arm_id) {
            self.disarm();
        }
    }

    pub fn armed(&self) -> Option<&ArmedZone> {
        self.armed.as_ref()
    }

    pub fn set_tradeable(&mut self, arm_id: u64, tradeable: bool) {
        if let Some(z) = self.armed.as_mut().filter(|z| z.arm_id == arm_id) {
            z.tradeable = tradeable;
        }
    }

    /// Refresh the speed baseline (ATR(14) on 10-second bars) of the armed zone.
    pub fn set_atr(&mut self, arm_id: u64, atr: f64) {
        if let Some(z) = self.armed.as_mut().filter(|z| z.arm_id == arm_id) {
            z.atr = atr;
        }
    }

    /// Move the zone's edges without restarting its attack state — the thickness
    /// hotkeys. Keeps the same `arm_id`, so nothing in flight is invalidated.
    pub fn resize(&mut self, arm_id: u64, low: f64, high: f64) {
        if let Some(z) = self.armed.as_mut().filter(|z| z.arm_id == arm_id) {
            z.low = low;
            z.high = high;
        }
    }

    /// Market data wiped (replay start/seek, tape kind change): keep who is watched,
    /// forget every path measured.
    pub fn reset_data(&mut self) {
        for t in self.trackers.values_mut() {
            *t = PathTracker::default();
        }
    }

    /// Record a print, then decide. On the armed zone a turning attack disarms (one
    /// shot) and sends an entry; a break through the far edge drops the zone. The
    /// signal is also returned, for tests.
    #[inline]
    pub fn on_print(&mut self, symbol: &str, price: f64, ts_ms: i64) -> Option<DipSignal> {
        if !self.enabled || self.trackers.is_empty() {
            return None;
        }
        if !price.is_finite() || price <= 0.0 {
            return None;
        }
        // Copied out before the tracker is borrowed: the zone is needed to measure
        // the approach, and the tracker to measure it with.
        let live = self.armed.as_ref().and_then(|z| {
            (z.symbol == symbol && z.tradeable && ts_ms >= z.not_before_ms)
                .then_some((z.side, z.atr))
        });

        let p = self.params;
        let tracker = self.trackers.get_mut(symbol)?;
        tracker.push(ts_ms, price, p.speed_window_ms);
        let (side, atr) = live?;
        let ratio = tracker
            .speed_ratio(p.speed_window_ms, atr, side, price)
            .unwrap_or(0.0);

        let zone = self.armed.as_mut()?;
        let inside = price >= zone.low - PRICE_EPS && price <= zone.high + PRICE_EPS;
        let broken = match side {
            Side::Long => price < zone.low - p.break_through - PRICE_EPS,
            Side::Short => price > zone.high + p.break_through + PRICE_EPS,
        };

        // ── The level itself failed ──
        if broken {
            let z = self.armed.take()?;
            let edge = match side {
                Side::Long => z.low,
                Side::Short => z.high,
            };
            return self.send(DipSignal::Lapse(DipLapse {
                arm_id: z.arm_id,
                symbol: z.symbol,
                reason: LapseReason::Broken,
                detail: format!(
                    "{} traversé ({} contre {})",
                    match side {
                        Side::Long => "support",
                        Side::Short => "résistance",
                    },
                    crate::apex::fmt_px(price),
                    crate::apex::fmt_px(edge),
                ),
                disarm: true,
                ts_ms,
            }));
        }

        match zone.state {
            Attack::Spent => {
                // The zone re-arms as soon as the price is out of it again.
                if !inside {
                    zone.state = Attack::Outside;
                }
                return None;
            }
            Attack::Outside => {
                if !inside {
                    return None;
                }
                zone.state = Attack::Inside {
                    entered_ms: ts_ms,
                    extreme: price,
                    peak_ratio: ratio,
                };
            }
            Attack::Inside { .. } => {}
        }

        let Attack::Inside {
            entered_ms,
            extreme,
            peak_ratio,
        } = zone.state
        else {
            return None;
        };
        let extreme = match side {
            Side::Long => extreme.min(price),
            Side::Short => extreme.max(price),
        };
        let peak_ratio = peak_ratio.max(ratio);
        zone.state = Attack::Inside {
            entered_ms,
            extreme,
            peak_ratio,
        };

        // ── Out of reach ──
        if peak_ratio > p.max_speed_ratio {
            zone.state = Attack::Spent;
            let (arm_id, sym) = (zone.arm_id, zone.symbol.clone());
            return self.send(DipSignal::Lapse(DipLapse {
                arm_id,
                symbol: sym,
                reason: LapseReason::TooFast,
                detail: format!(
                    "attaque à {peak_ratio:.1}× le rythme habituel, au-dessus du plafond de {:.1}×",
                    p.max_speed_ratio
                ),
                disarm: false,
                ts_ms,
            }));
        }

        // ── It entered and stayed: not a strike ──
        if ts_ms - entered_ms > p.attack_max_ms {
            zone.state = Attack::Spent;
            let (arm_id, sym) = (zone.arm_id, zone.symbol.clone());
            return self.send(DipSignal::Lapse(DipLapse {
                arm_id,
                symbol: sym,
                reason: LapseReason::Fizzled,
                detail: format!(
                    "le prix est resté {:.1} s dans la zone sans demi-tour",
                    (ts_ms - entered_ms) as f64 / 1_000.0
                ),
                disarm: false,
                ts_ms,
            }));
        }

        // ── The turn ──
        // Measured from the edge the attack came in by, so an overshoot that stays
        // inside the break tolerance still counts as a deeper bite — that overshoot
        // IS the snake's strike.
        let penetration = (zone.near_edge() - extreme).abs();
        let turned = match side {
            Side::Long => price >= extreme + p.reversal - PRICE_EPS,
            Side::Short => price <= extreme - p.reversal + PRICE_EPS,
        };
        if !turned
            || peak_ratio + 1e-12 < p.min_speed_ratio
            || penetration + 1e-9 < p.min_penetration
        {
            // Back out the way it came with nothing to show — a graze, or a drift
            // too slow to qualify. The next entry is a brand new attack, measured
            // from scratch, rather than this one still latched on the deadline.
            let backed_out = match side {
                Side::Long => price > zone.high + PRICE_EPS,
                Side::Short => price < zone.low - PRICE_EPS,
            };
            if backed_out {
                zone.state = Attack::Outside;
            }
            return None;
        }
        let z = self.armed.take()?;
        self.send(DipSignal::Entry(DipTrigger {
            arm_id: z.arm_id,
            symbol: z.symbol,
            side,
            price,
            extreme,
            penetration,
            speed_ratio: peak_ratio,
            ts_ms,
        }))
    }

    fn send(&self, signal: DipSignal) -> Option<DipSignal> {
        if let Some(tx) = &self.tx {
            let _ = tx.send(signal.clone());
        }
        Some(signal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SYM: &str = "DIP";
    /// 10 cents of range per 10 s bar → 1.5 cents of ordinary travel per 1.5 s.
    const ATR: f64 = 0.10;

    fn book(side: Side, low: f64, high: f64) -> (DipBook, DipParams) {
        let mut b = DipBook::default();
        let params = DipParams::default();
        b.configure(true, params);
        b.set_watch([SYM.to_string()]);
        b.arm(ArmedZone::new(
            7,
            SYM.into(),
            side,
            low,
            high,
            true,
            ATR,
        ));
        (b, params)
    }

    /// Walk the price to `to` over `ms`, one print every 100 ms.
    fn walk(b: &mut DipBook, from: f64, to: f64, start_ms: i64, ms: i64) -> Option<DipSignal> {
        let steps = (ms / 100).max(1);
        let mut out = None;
        for i in 1..=steps {
            let price = from + (to - from) * (i as f64 / steps as f64);
            if let Some(s) = b.on_print(SYM, price, start_ms + i * 100) {
                out = Some(s);
            }
        }
        out
    }

    fn entry(signal: Option<DipSignal>) -> DipTrigger {
        match signal {
            Some(DipSignal::Entry(t)) => t,
            other => panic!("entrée attendue, reçu {other:?}"),
        }
    }

    fn lapse(signal: Option<DipSignal>) -> DipLapse {
        match signal {
            Some(DipSignal::Lapse(l)) => l,
            other => panic!("abandon attendu, reçu {other:?}"),
        }
    }

    #[test]
    fn a_violent_flush_into_a_support_fires_long_on_the_turn() {
        // Support 4.00–4.06, price idling at 4.20.
        let (mut b, _) = book(Side::Long, 4.00, 4.06);
        walk(&mut b, 4.20, 4.20, 0, 3_000); // build the path history
        // 14 cents down in 1.4 s — about 9× the ordinary travel.
        assert!(walk(&mut b, 4.20, 4.03, 3_000, 1_400).is_none());
        // …then it starts coming back.
        let t = entry(b.on_print(SYM, 4.05, 4_500));
        assert_eq!((t.arm_id, t.side), (7, Side::Long));
        assert!(t.speed_ratio > 3.0, "vitesse relative {}", t.speed_ratio);
        assert!(b.armed().is_none(), "un seul déclenchement par zone");
    }

    #[test]
    fn a_violent_push_into_a_resistance_fires_short_on_the_turn() {
        let (mut b, _) = book(Side::Short, 3.94, 4.00);
        walk(&mut b, 3.80, 3.80, 0, 3_000);
        assert!(walk(&mut b, 3.80, 3.97, 3_000, 1_400).is_none());
        let t = entry(b.on_print(SYM, 3.95, 4_500));
        assert_eq!(t.side, Side::Short);
    }

    #[test]
    fn a_gentle_drift_into_the_zone_is_not_an_attack() {
        let (mut b, _) = book(Side::Long, 4.00, 4.06);
        walk(&mut b, 4.20, 4.20, 0, 3_000);
        // The same 14 cents, but over 9 s — ordinary travel, no strike.
        assert!(walk(&mut b, 4.20, 4.03, 3_000, 9_000).is_none());
        assert!(b.on_print(SYM, 4.05, 12_100).is_none());
        assert!(b.armed().is_some(), "la zone reste posée");
    }

    #[test]
    fn a_move_too_fast_to_catch_is_abandoned_but_the_zone_stands() {
        let (mut b, _) = book(Side::Long, 4.00, 4.06);
        walk(&mut b, 4.60, 4.60, 0, 3_000);
        // 56 cents inside the measuring window: ~37× the ordinary travel, far past
        // the ceiling — by the time an order came back the price would be elsewhere.
        b.on_print(SYM, 4.30, 3_100);
        let l = lapse(b.on_print(SYM, 4.04, 3_200));
        assert_eq!(l.reason, LapseReason::TooFast);
        assert!(!l.disarm, "la zone reste posée");
        assert!(b.armed().is_some());
        // And no entry on the way back out.
        assert!(walk(&mut b, 4.04, 4.12, 3_200, 1_000).is_none());
    }

    #[test]
    fn breaking_through_the_far_edge_drops_the_zone() {
        let (mut b, _) = book(Side::Long, 4.00, 4.06);
        walk(&mut b, 4.20, 4.20, 0, 3_000);
        let l = lapse(walk(&mut b, 4.20, 3.90, 3_000, 1_500));
        assert_eq!(l.reason, LapseReason::Broken);
        assert!(l.disarm);
        assert!(b.armed().is_none());
    }

    #[test]
    fn an_attack_that_settles_in_the_zone_lapses_and_re_arms_on_the_way_out() {
        let (mut b, mut p) = book(Side::Long, 4.00, 4.06);
        p.attack_max_ms = 2_000;
        b.configure(true, p);
        b.arm(ArmedZone::new(7, SYM.into(), Side::Long, 4.00, 4.06, true, ATR));
        walk(&mut b, 4.20, 4.20, 0, 3_000);
        walk(&mut b, 4.20, 4.03, 3_000, 1_400);
        // It sits there past the deadline instead of snapping back.
        let l = lapse(walk(&mut b, 4.03, 4.035, 4_400, 3_000));
        assert_eq!(l.reason, LapseReason::Fizzled);
        assert!(b.armed().is_some());
        // Out and back in: a brand new attack, and this one turns.
        walk(&mut b, 4.035, 4.20, 7_400, 2_000);
        walk(&mut b, 4.20, 4.03, 9_400, 1_400);
        assert_eq!(entry(b.on_print(SYM, 4.05, 11_000)).side, Side::Long);
    }

    #[test]
    fn a_graze_that_leaves_the_zone_frees_it_for_the_real_attack() {
        let (mut b, _) = book(Side::Long, 4.00, 4.06);
        walk(&mut b, 4.20, 4.20, 0, 3_000);
        // A slow touch of the top edge, then back up: nothing qualified, and the
        // zone must not stay latched on the first attack's deadline.
        walk(&mut b, 4.20, 4.05, 3_000, 6_000);
        walk(&mut b, 4.05, 4.20, 9_000, 3_000);
        // The real strike, right after.
        walk(&mut b, 4.20, 4.03, 12_000, 1_400);
        assert_eq!(entry(b.on_print(SYM, 4.05, 13_600)).side, Side::Long);
    }

    #[test]
    fn a_zone_under_the_minimum_distance_measures_but_never_fires() {
        let (mut b, _) = book(Side::Long, 4.00, 4.06);
        b.set_tradeable(7, false);
        walk(&mut b, 4.20, 4.20, 0, 3_000);
        walk(&mut b, 4.20, 4.03, 3_000, 1_400);
        assert!(b.on_print(SYM, 4.05, 4_500).is_none());
        assert!(b.armed().is_some());
    }

    #[test]
    fn a_disabled_dip_measures_nothing() {
        let (mut b, p) = book(Side::Long, 4.00, 4.06);
        b.configure(false, p);
        walk(&mut b, 4.20, 4.03, 0, 1_400);
        assert!(b.on_print(SYM, 4.05, 1_500).is_none());
        assert!(b.armed().is_none());
    }

    #[test]
    fn the_speed_baseline_is_the_tickers_own_ten_second_range() {
        let mut t = PathTracker::default();
        for i in 0..=30 {
            t.push(i * 100, 4.20 - 0.005 * i as f64, 1_500);
        }
        // 1.5 s of window at 0.5 cent per 100 ms = 7.5 cents travelled; the ordinary
        // travel over that window is 0.10 × 1.5/10 = 1.5 cents → 5×.
        let r = t
            .speed_ratio(1_500, ATR, Side::Long, 4.20 - 0.005 * 30.0)
            .unwrap();
        assert!((r - 5.0).abs() < 1e-6, "{r}");
        // A move the other way scores zero, never a negative speed.
        assert_eq!(
            t.speed_ratio(1_500, ATR, Side::Short, 4.20 - 0.005 * 30.0)
                .unwrap(),
            0.0
        );
    }

    #[test]
    fn a_path_shorter_than_the_window_yields_no_speed() {
        let mut t = PathTracker::default();
        t.push(0, 4.20, 1_500);
        t.push(400, 4.05, 1_500);
        assert!(t.speed_ratio(1_500, ATR, Side::Long, 4.05).is_none());
    }
}
