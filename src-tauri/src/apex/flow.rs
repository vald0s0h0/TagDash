// Apex flow book — the hot-path half of Apex.
//
// Lives inside `MarketState` and is fed from the very ingest calls that already
// hold its write lock (`on_trade`, `on_second_aggregate`, `on_replay_trade`,
// `on_quote`), so measuring the tape costs no extra lock, no allocation after the
// first print of a symbol, and no polling delay: the print that tips the balance is
// the print that sends the trigger.
//
// ── WHAT IS MEASURED ──────────────────────────────────────────────────────────
// Every print is signed buy (+1) or sell (−1), then a sliding window keeps the
// running buy/sell print counts (and shares). The trigger is a burst of prints
// that leans hard one way:
//   • at least `min_prints` in the window,
//   • |buy − sell| / (buy + sell) ≥ `imbalance_min`,
//   • the share balance agrees (ten odd lots do not outvote one block),
//   • the print that tips it goes the same way (the burst is still going),
//   • the window rate is ≥ `burst_ratio` × the ticker's own rolling baseline — the
//     "relative" in "beaucoup de prints relatifs". Skipped until the baseline has
//     seen `WARMUP_SECS`, so a freshly watched name can still fire on the absolute
//     floor: arming is the trader saying a move is imminent, the trigger only has
//     to agree.
//
// ── HOW A PRINT IS SIGNED (one rule for every tape) ──────────────────────────
//   1. Quote rule, when a fresh two-sided quote exists (Alpaca SIP, the quotes in
//      the trade flat files): at/above the ask or above the mid → buy; at/below
//      the bid or below the mid → sell.
//   2. Tick rule anchored on the previous second's last price. Anchoring on the
//      second rather than the previous print is what keeps the synthetic replay
//      paths honest: a Massive second is replayed as O→L→H→C at 250 ms, and a plain
//      tick rule scores that walk down-up-down — net SELL on a green second. Against
//      the previous close it nets to the direction the second actually went.
//   3. Plain tick rule against the previous print.
//   4. Live Massive aggregates only: close against the second's own open.
//   5. Otherwise the last known direction (zero-tick carries).
//
// ── CONTINUOUS WATCH ──────────────────────────────────────────────────────────
// Trackers exist for the displayed (focus) tickers whenever Apex is enabled, so the
// baseline is already warm the moment the hotkey arms one. Disabled in Settings,
// the book holds no tracker at all and every hook returns on a single bool.

use std::collections::{HashMap, HashSet, VecDeque};

use tokio::sync::mpsc::UnboundedSender;

use super::options::ApexOptions;
use crate::types::Side;

/// Prices closer than this are equal.
const PRICE_EPS: f64 = 1e-9;
/// A quote older than this (relative to the print) no longer says anything about
/// which side a print hit — e.g. once a flat-file ignition window's quotes stop.
const QUOTE_MAX_AGE_MS: i64 = 5_000;
/// Time constant of the baseline print rate (EWMA over closed seconds).
const BASELINE_TAU_SECS: f64 = 30.0;
/// Seconds of baseline required before the relative (burst) gate applies.
pub const WARMUP_SECS: u32 = 10;
/// A dead ticker's baseline must not turn a trickle into a "burst".
const BASELINE_FLOOR_PER_SEC: f64 = 0.5;
/// Hard bound on the window, whatever the tape does.
const MAX_EVENTS: usize = 8_192;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FlowParams {
    pub window_ms: i64,
    pub min_prints: u64,
    /// 0..=1
    pub imbalance_min: f64,
    pub burst_ratio: f64,
    pub long_enabled: bool,
    pub short_enabled: bool,
}

impl Default for FlowParams {
    fn default() -> Self {
        Self::from_options(&ApexOptions::default())
    }
}

impl FlowParams {
    pub fn from_options(o: &ApexOptions) -> Self {
        Self {
            window_ms: o.window_ms as i64,
            min_prints: o.min_prints as u64,
            imbalance_min: o.imbalance_pct / 100.0,
            burst_ratio: o.burst_ratio,
            long_enabled: o.long_enabled,
            short_enabled: o.short_enabled,
        }
    }
}

/// What ends a position under the dynamic take profit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExitParams {
    pub window_ms: i64,
    pub min_prints: u64,
    pub imbalance_min: f64,
    pub block_ratio: f64,
    pub block_min_shares: u64,
}

impl Default for ExitParams {
    fn default() -> Self {
        Self::from_options(&ApexOptions::default())
    }
}

impl ExitParams {
    pub fn from_options(o: &ApexOptions) -> Self {
        Self {
            window_ms: o.window_ms as i64,
            min_prints: o.exit_min_prints as u64,
            imbalance_min: o.exit_imbalance_pct / 100.0,
            block_ratio: o.exit_block_ratio,
            block_min_shares: o.exit_block_min_shares,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitReason {
    /// The print balance now runs against the position.
    Flow,
    /// One opposite participant far larger than this ticker's usual print.
    Block,
    /// A print went through the trailing stop (venue mode, where the stop TagDash
    /// follows is not the one resting at the broker).
    TrailStop,
}

/// What the flow book hands the executor when an armed ticker's balance tips.
#[derive(Debug, Clone)]
pub struct FlowTrigger {
    pub arm_id: u64,
    pub symbol: String,
    pub side: Side,
    pub price: f64,
    pub buy_prints: u64,
    pub sell_prints: u64,
    pub rate_per_sec: f64,
    pub baseline_per_sec: f64,
    pub ts_ms: i64,
}

/// What ends an open Apex position, sent the instant the print lands.
#[derive(Debug, Clone)]
pub struct ExitTrigger {
    pub arm_id: u64,
    pub symbol: String,
    /// The POSITION's side, not the exit order's.
    pub side: Side,
    pub reason: ExitReason,
    /// One phrase for the notice, built where the numbers are.
    pub detail: String,
    pub price: f64,
    pub ts_ms: i64,
}

#[derive(Debug, Clone)]
pub enum FlowSignal {
    Entry(FlowTrigger),
    Exit(ExitTrigger),
}

#[derive(Debug, Clone, Copy)]
struct FlowEvent {
    ts_ms: i64,
    dir: i8,
    prints: u64,
    shares: u64,
}

/// Per-symbol running balance. Every operation is O(1) amortised.
#[derive(Debug, Default)]
pub struct FlowTracker {
    events: VecDeque<FlowEvent>,
    buy_prints: u64,
    sell_prints: u64,
    buy_shares: u64,
    sell_shares: u64,
    /// Newest event time seen (ms). The window slides on this, never on the wall
    /// clock, so a delayed or replayed tape is measured in its own time.
    now_ms: i64,

    bid: f64,
    ask: f64,
    quote_ms: i64,

    last_price: f64,
    last_dir: i8,

    cur_sec: Option<i64>,
    sec_prints: u64,
    sec_shares: u64,
    prev_sec_close: f64,
    baseline: f64,
    /// EWMA of the ticker's average print size (shares per print). What "gros
    /// participant" is measured against, so it means the same thing on a 200-share
    /// tape and on a 20 000-share one.
    avg_print_shares: f64,
    seconds_seen: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FlowSnapshot {
    pub buy_prints: u64,
    pub sell_prints: u64,
    pub rate_per_sec: f64,
    pub baseline_per_sec: f64,
    pub warm: bool,
}

impl FlowTracker {
    pub fn on_quote(&mut self, bid: f64, ask: f64, ts_ms: i64) {
        if bid.is_finite() && ask.is_finite() && bid > 0.0 && ask > bid {
            self.bid = bid;
            self.ask = ask;
            self.quote_ms = ts_ms;
        }
    }

    /// Record one print (or one aggregate standing for `prints` trades) and return
    /// the direction it was assigned (0 = unclassifiable, e.g. the very first print).
    pub fn on_print(
        &mut self,
        price: f64,
        shares: u64,
        prints: u64,
        ts_ms: i64,
        open_hint: Option<f64>,
        window_ms: i64,
    ) -> i8 {
        if !price.is_finite() || price <= 0.0 {
            return 0;
        }
        let prints = prints.max(1);
        self.roll_second(ts_ms.div_euclid(1_000));
        let dir = self.classify(price, ts_ms, open_hint);

        self.last_price = price;
        if dir != 0 {
            self.last_dir = dir;
        }
        self.sec_prints += prints;
        self.sec_shares += shares;
        self.now_ms = self.now_ms.max(ts_ms);

        if dir != 0 {
            self.events.push_back(FlowEvent {
                ts_ms,
                dir,
                prints,
                shares,
            });
            self.add(dir, prints, shares);
        }
        self.evict(window_ms);
        dir
    }

    fn quote_fresh(&self, ts_ms: i64) -> bool {
        self.ask > self.bid && self.bid > 0.0 && (ts_ms - self.quote_ms).abs() <= QUOTE_MAX_AGE_MS
    }

    fn classify(&self, price: f64, ts_ms: i64, open_hint: Option<f64>) -> i8 {
        if self.quote_fresh(ts_ms) {
            if price >= self.ask - PRICE_EPS {
                return 1;
            }
            if price <= self.bid + PRICE_EPS {
                return -1;
            }
            let mid = (self.bid + self.ask) / 2.0;
            if price > mid + PRICE_EPS {
                return 1;
            }
            if price < mid - PRICE_EPS {
                return -1;
            }
        }
        if let Some(d) = tick(price, self.prev_sec_close) {
            return d;
        }
        if let Some(d) = tick(price, self.last_price) {
            return d;
        }
        if let Some(d) = open_hint.and_then(|o| tick(price, o)) {
            return d;
        }
        self.last_dir
    }

    /// Close the running second(s) into the baseline when a print opens a new one.
    fn roll_second(&mut self, sec: i64) {
        let Some(cur) = self.cur_sec else {
            self.cur_sec = Some(sec);
            return;
        };
        if sec <= cur {
            return; // same second, or a late print from an older one
        }
        let alpha = 1.0 - (-1.0 / BASELINE_TAU_SECS).exp();
        let x = self.sec_prints as f64;
        self.baseline = if self.seconds_seen == 0 {
            x
        } else {
            self.baseline + alpha * (x - self.baseline)
        };
        if self.sec_prints > 0 {
            let avg = self.sec_shares as f64 / self.sec_prints as f64;
            self.avg_print_shares = if self.avg_print_shares <= 0.0 {
                avg
            } else {
                self.avg_print_shares + alpha * (avg - self.avg_print_shares)
            };
        }
        // Silent seconds in between decay the baseline toward zero.
        let gap = (sec - cur - 1).min(3_600) as i32;
        if gap > 0 {
            self.baseline *= (1.0 - alpha).powi(gap);
        }
        self.seconds_seen = self
            .seconds_seen
            .saturating_add((sec - cur).min(u32::MAX as i64) as u32);
        self.prev_sec_close = self.last_price;
        self.sec_prints = 0;
        self.sec_shares = 0;
        self.cur_sec = Some(sec);
    }

    fn add(&mut self, dir: i8, prints: u64, shares: u64) {
        if dir > 0 {
            self.buy_prints += prints;
            self.buy_shares += shares;
        } else {
            self.sell_prints += prints;
            self.sell_shares += shares;
        }
    }

    fn remove_front(&mut self) {
        if let Some(e) = self.events.pop_front() {
            if e.dir > 0 {
                self.buy_prints = self.buy_prints.saturating_sub(e.prints);
                self.buy_shares = self.buy_shares.saturating_sub(e.shares);
            } else {
                self.sell_prints = self.sell_prints.saturating_sub(e.prints);
                self.sell_shares = self.sell_shares.saturating_sub(e.shares);
            }
        }
    }

    fn evict(&mut self, window_ms: i64) {
        let cutoff = self.now_ms - window_ms;
        while matches!(self.events.front(), Some(e) if e.ts_ms <= cutoff) {
            self.remove_front();
        }
        while self.events.len() > MAX_EVENTS {
            self.remove_front();
        }
    }

    pub fn snapshot(&self, window_ms: i64) -> FlowSnapshot {
        let total = self.buy_prints + self.sell_prints;
        FlowSnapshot {
            buy_prints: self.buy_prints,
            sell_prints: self.sell_prints,
            rate_per_sec: total as f64 * 1_000.0 / window_ms.max(1) as f64,
            baseline_per_sec: self.baseline,
            warm: self.seconds_seen >= WARMUP_SECS,
        }
    }

    /// The trigger decision for the print just recorded (`dir` = its direction).
    pub fn evaluate(&self, p: &FlowParams, dir: i8) -> Option<(Side, FlowSnapshot)> {
        let s = self.snapshot(p.window_ms);
        let total = s.buy_prints + s.sell_prints;
        if total == 0 || total < p.min_prints {
            return None;
        }
        let net = s.buy_prints as f64 - s.sell_prints as f64;
        if net == 0.0 || net.abs() / total as f64 + 1e-12 < p.imbalance_min {
            return None;
        }
        let side_dir: i8 = if net > 0.0 { 1 } else { -1 };
        if dir != side_dir {
            return None;
        }
        let share_net = self.buy_shares as f64 - self.sell_shares as f64;
        if self.buy_shares + self.sell_shares > 0 && share_net * side_dir as f64 <= 0.0 {
            return None;
        }
        let side = if side_dir > 0 {
            Side::Long
        } else {
            Side::Short
        };
        match side {
            Side::Long if !p.long_enabled => return None,
            Side::Short if !p.short_enabled => return None,
            _ => {}
        }
        if s.warm && s.rate_per_sec < p.burst_ratio * s.baseline_per_sec.max(BASELINE_FLOOR_PER_SEC)
        {
            return None;
        }
        Some((side, s))
    }
}

impl FlowTracker {
    /// The ticker's rolling average print size (shares), 0 until a second closed.
    pub fn avg_print_shares(&self) -> f64 {
        self.avg_print_shares
    }

    /// Why the print just recorded ends a `side` position — the dynamic take
    /// profit's whole decision. `shares`/`prints` are that print's own.
    pub fn evaluate_exit(
        &self,
        p: &ExitParams,
        dir: i8,
        shares: u64,
        prints: u64,
        side: Side,
    ) -> Option<(ExitReason, String)> {
        let against: i8 = match side {
            Side::Long => -1,
            Side::Short => 1,
        };
        // Only a print going AGAINST the position can end it — on either rule.
        if dir != against {
            return None;
        }

        // 1. One oversized opposite participant, judged against this ticker's own
        //    usual print. Needs a baseline, so it is silent for the first second.
        let base = self.avg_print_shares;
        let per_print = shares as f64 / prints.max(1) as f64;
        if shares >= p.block_min_shares && base > 0.0 && per_print >= p.block_ratio * base {
            return Some((
                ExitReason::Block,
                format!(
                    "gros participant : {shares} actions, {:.0}× la taille habituelle",
                    per_print / base
                ),
            ));
        }

        // 2. The balance itself has turned over the window.
        let s = self.snapshot(p.window_ms);
        let total = s.buy_prints + s.sell_prints;
        if total < p.min_prints {
            return None;
        }
        let net = s.buy_prints as f64 - s.sell_prints as f64;
        if net == 0.0 {
            return None;
        }
        let net_dir: i8 = if net > 0.0 { 1 } else { -1 };
        if net_dir != against || net.abs() / total as f64 + 1e-12 < p.imbalance_min {
            return None;
        }
        Some((
            ExitReason::Flow,
            format!(
                "flux inversé : {} prints acheteurs / {} vendeurs",
                s.buy_prints, s.sell_prints
            ),
        ))
    }
}

fn tick(price: f64, reference: f64) -> Option<i8> {
    if reference <= 0.0 {
        return None;
    }
    if price > reference + PRICE_EPS {
        Some(1)
    } else if price < reference - PRICE_EPS {
        Some(-1)
    } else {
        None
    }
}

#[derive(Debug, Clone)]
pub struct ArmedSymbol {
    pub arm_id: u64,
    pub symbol: String,
    /// False while the ATR bracket is under the minimum distance: the balance keeps
    /// being measured but nothing fires.
    pub tradeable: bool,
    /// No trigger before this event time (ms) — set when the executor re-arms after
    /// a skipped attempt, so the same burst cannot fire again on its next print.
    pub not_before_ms: i64,
}

/// An open Apex position the book keeps watching: to remember the best price the
/// trailing stop follows, and — under the dynamic take profit — to end it.
#[derive(Debug, Clone)]
pub struct GuardedPosition {
    pub arm_id: u64,
    pub symbol: String,
    pub side: Side,
    /// Dynamic take profit: exit on an opposite balance or block print.
    pub watch_flow: bool,
    /// Trailing stop TagDash owns (venue mode only — with the internal broker the
    /// bracket order is moved instead, and fills on the real price path).
    pub stop_price: Option<f64>,
    /// Best price seen since entry: the high for a long, the low for a short.
    pub best_price: f64,
}

impl GuardedPosition {
    fn record(&mut self, price: f64) {
        let better = match self.side {
            Side::Long => price > self.best_price,
            Side::Short => price < self.best_price || self.best_price <= 0.0,
        };
        if better {
            self.best_price = price;
        }
    }

    fn stop_hit(&self, price: f64) -> bool {
        self.stop_price.is_some_and(|stop| match self.side {
            Side::Long => price <= stop,
            Side::Short => price >= stop,
        })
    }
}

/// All trackers plus the (single) armed ticker.
#[derive(Debug, Default)]
pub struct FlowBook {
    enabled: bool,
    params: FlowParams,
    exit_params: ExitParams,
    watch: HashSet<String>,
    trackers: HashMap<String, FlowTracker>,
    armed: Option<ArmedSymbol>,
    guarded: Option<GuardedPosition>,
    tx: Option<UnboundedSender<FlowSignal>>,
}

impl FlowBook {
    pub fn set_sender(&mut self, tx: UnboundedSender<FlowSignal>) {
        self.tx = Some(tx);
    }

    pub fn configure(&mut self, enabled: bool, params: FlowParams, exit_params: ExitParams) {
        self.enabled = enabled;
        self.params = params;
        self.exit_params = exit_params;
        if !enabled {
            self.armed = None;
            // The guard is NOT dropped: a position opened by Apex keeps being watched
            // until it is closed, whatever the switch says afterwards.
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
        let armed = self.armed.as_ref().map(|a| a.symbol.clone());
        let guarded = self.guarded.as_ref().map(|g| g.symbol.clone());
        let watch = &self.watch;
        self.trackers.retain(|s, _| {
            watch.contains(s)
                || armed.as_deref() == Some(s.as_str())
                || guarded.as_deref() == Some(s.as_str())
        });
        for s in watch.iter().cloned().chain(armed).chain(guarded) {
            self.trackers.entry(s).or_default();
        }
    }

    pub fn arm(&mut self, armed: ArmedSymbol) -> bool {
        if !self.enabled {
            return false;
        }
        self.armed = Some(armed);
        self.rebuild();
        true
    }

    pub fn disarm(&mut self) {
        self.armed = None;
        self.rebuild();
    }

    /// Disarm only if `arm_id` is the one armed — a newer arming survives.
    pub fn disarm_id(&mut self, arm_id: u64) {
        if self.armed.as_ref().is_some_and(|a| a.arm_id == arm_id) {
            self.disarm();
        }
    }

    pub fn armed(&self) -> Option<&ArmedSymbol> {
        self.armed.as_ref()
    }

    /// Start watching an open Apex position (dynamic TP and/or trailing stop).
    pub fn guard(&mut self, position: GuardedPosition) {
        self.guarded = Some(position);
        self.rebuild();
    }

    /// Stop watching it — only if it is still the one `arm_id` opened.
    pub fn unguard_id(&mut self, arm_id: u64) {
        if self.guarded.as_ref().is_some_and(|g| g.arm_id == arm_id) {
            self.guarded = None;
            self.rebuild();
        }
    }

    pub fn guarded(&self) -> Option<&GuardedPosition> {
        self.guarded.as_ref()
    }

    /// Best price seen since entry — what the trailing stop is computed from.
    pub fn guard_best(&self, arm_id: u64) -> Option<f64> {
        self.guarded
            .as_ref()
            .filter(|g| g.arm_id == arm_id && g.best_price > 0.0)
            .map(|g| g.best_price)
    }

    /// Move the TagDash-side trailing stop (venue mode).
    pub fn set_guard_stop(&mut self, arm_id: u64, stop: Option<f64>) {
        if let Some(g) = self.guarded.as_mut().filter(|g| g.arm_id == arm_id) {
            g.stop_price = stop;
        }
    }

    pub fn set_tradeable(&mut self, arm_id: u64, tradeable: bool) {
        if let Some(a) = self.armed.as_mut().filter(|a| a.arm_id == arm_id) {
            a.tradeable = tradeable;
        }
    }

    /// Market data wiped (replay start/seek, tape kind change): keep who is watched,
    /// forget everything measured.
    pub fn reset_data(&mut self) {
        for t in self.trackers.values_mut() {
            *t = FlowTracker::default();
        }
    }

    #[inline]
    pub fn on_quote(&mut self, symbol: &str, bid: f64, ask: f64, ts_ms: i64) {
        if !self.enabled {
            return;
        }
        if let Some(t) = self.trackers.get_mut(symbol) {
            t.on_quote(bid, ask, ts_ms);
        }
    }

    /// Record a print, then decide. On the armed ticker a tipping balance disarms
    /// (one shot) and sends an entry; on a guarded position an opposite balance, a
    /// block print or a trailing-stop cross drops the guard and sends an exit. The
    /// signal is also returned, for tests.
    #[inline]
    pub fn on_print(
        &mut self,
        symbol: &str,
        price: f64,
        shares: u64,
        prints: u64,
        ts_ms: i64,
        open_hint: Option<f64>,
    ) -> Option<FlowSignal> {
        if !self.enabled || self.trackers.is_empty() {
            return None;
        }
        let (entry, exit) = {
            let t = self.trackers.get_mut(symbol)?;
            let dir = t.on_print(
                price,
                shares,
                prints,
                ts_ms,
                open_hint,
                self.params.window_ms,
            );
            let entry = self
                .armed
                .as_ref()
                .filter(|a| a.symbol == symbol && a.tradeable && ts_ms >= a.not_before_ms)
                .and_then(|_| t.evaluate(&self.params, dir));
            let exit = self
                .guarded
                .as_ref()
                .filter(|g| g.symbol == symbol && g.watch_flow)
                .and_then(|g| {
                    t.evaluate_exit(&self.exit_params, dir, shares, prints.max(1), g.side)
                });
            (entry, exit)
        };

        // The armed ticker and a guarded position are never the same trade: Apex
        // disarms the moment it fires, and re-arms only once flat.
        if let Some((side, s)) = entry {
            let armed = self.armed.take()?;
            return self.send(FlowSignal::Entry(FlowTrigger {
                arm_id: armed.arm_id,
                symbol: armed.symbol,
                side,
                price,
                buy_prints: s.buy_prints,
                sell_prints: s.sell_prints,
                rate_per_sec: s.rate_per_sec,
                baseline_per_sec: s.baseline_per_sec,
                ts_ms,
            }));
        }

        let stop_hit = match self.guarded.as_mut().filter(|g| g.symbol == symbol) {
            Some(g) => {
                g.record(price);
                g.stop_hit(price)
            }
            None => return None,
        };
        let (reason, detail) = if stop_hit {
            (ExitReason::TrailStop, "stop suiveur touché".to_string())
        } else {
            exit?
        };
        let g = self.guarded.take()?;
        self.send(FlowSignal::Exit(ExitTrigger {
            arm_id: g.arm_id,
            symbol: g.symbol,
            side: g.side,
            reason,
            detail,
            price,
            ts_ms,
        }))
    }

    fn send(&self, signal: FlowSignal) -> Option<FlowSignal> {
        if let Some(tx) = &self.tx {
            let _ = tx.send(signal.clone());
        }
        Some(signal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SYM: &str = "ABCD";

    fn params() -> FlowParams {
        FlowParams {
            window_ms: 1_500,
            min_prints: 8,
            imbalance_min: 0.5,
            burst_ratio: 2.0,
            long_enabled: true,
            short_enabled: true,
        }
    }

    fn exit_params() -> ExitParams {
        ExitParams {
            window_ms: 1_500,
            min_prints: 5,
            imbalance_min: 0.5,
            block_ratio: 8.0,
            block_min_shares: 1_000,
        }
    }

    fn book() -> FlowBook {
        let mut b = FlowBook::default();
        b.configure(true, params(), exit_params());
        b.set_watch([SYM.to_string()]);
        b
    }

    /// The entry inside a signal, or a readable failure.
    fn entry(sig: Option<FlowSignal>) -> Option<FlowTrigger> {
        match sig {
            Some(FlowSignal::Entry(t)) => Some(t),
            Some(FlowSignal::Exit(x)) => panic!("sortie inattendue : {x:?}"),
            None => None,
        }
    }

    fn exit(sig: Option<FlowSignal>) -> Option<ExitTrigger> {
        match sig {
            Some(FlowSignal::Exit(x)) => Some(x),
            Some(FlowSignal::Entry(t)) => panic!("entrée inattendue : {t:?}"),
            None => None,
        }
    }

    /// Guard a long opened at `entry_px`, as the executor does after a fill.
    fn guard_long(b: &mut FlowBook, arm_id: u64, entry_px: f64, watch_flow: bool) {
        b.guard(GuardedPosition {
            arm_id,
            symbol: SYM.into(),
            side: Side::Long,
            watch_flow,
            stop_price: None,
            best_price: entry_px,
        });
    }

    fn arm(b: &mut FlowBook, id: u64) {
        assert!(b.arm(ArmedSymbol {
            arm_id: id,
            symbol: SYM.into(),
            tradeable: true,
            not_before_ms: 0,
        }));
    }

    /// Feed a quiet, balanced tape for `secs` seconds starting at `t0` (ms):
    /// `rate` prints per second alternating around `px`.
    fn quiet(b: &mut FlowBook, t0: i64, secs: i64, rate: i64, px: f64) -> i64 {
        let mut t = t0;
        for s in 0..secs {
            for i in 0..rate {
                t = t0 + s * 1_000 + i * (1_000 / rate.max(1));
                let p = if i % 2 == 0 { px + 0.01 } else { px - 0.01 };
                assert!(b.on_print(SYM, p, 100, 1, t, None).is_none());
            }
        }
        t
    }

    #[test]
    fn prints_lifting_the_ask_fire_long() {
        let mut b = book();
        b.on_quote(SYM, 5.00, 5.02, 0);
        arm(&mut b, 1);
        let mut fired = None;
        for i in 0..10 {
            if let Some(t) = entry(b.on_print(SYM, 5.02, 200, 1, 10 + i * 50, None)) {
                fired = Some(t);
                break;
            }
        }
        let t = fired.expect("un paquet d'achats à l'ask doit déclencher");
        assert_eq!(t.side, Side::Long);
        assert_eq!(t.arm_id, 1);
        assert!(t.buy_prints >= 8 && t.sell_prints == 0);
        assert!(b.armed().is_none(), "one shot : désarmé au déclenchement");
    }

    #[test]
    fn prints_hitting_the_bid_fire_short() {
        let mut b = book();
        b.on_quote(SYM, 5.00, 5.02, 0);
        arm(&mut b, 7);
        let fired = (0..10).find_map(|i| entry(b.on_print(SYM, 5.00, 300, 1, 10 + i * 40, None)));
        assert_eq!(fired.expect("flush").side, Side::Short);
    }

    #[test]
    fn a_disabled_direction_never_fires() {
        let mut b = FlowBook::default();
        b.configure(
            true,
            FlowParams {
                short_enabled: false,
                ..params()
            },
            exit_params(),
        );
        b.set_watch([SYM.to_string()]);
        b.on_quote(SYM, 5.00, 5.02, 0);
        arm(&mut b, 1);
        assert!((0..30).all(|i| b.on_print(SYM, 5.00, 300, 1, 10 + i * 40, None).is_none()));
        assert!(b.armed().is_some(), "toujours armé");
    }

    #[test]
    fn a_balanced_tape_does_not_fire() {
        let mut b = book();
        b.on_quote(SYM, 5.00, 5.02, 0);
        arm(&mut b, 1);
        for i in 0..40 {
            let p = if i % 2 == 0 { 5.02 } else { 5.00 };
            assert!(b.on_print(SYM, p, 100, 1, 10 + i * 30, None).is_none());
        }
    }

    #[test]
    fn nothing_fires_unless_armed_and_tradeable() {
        let mut b = book();
        b.on_quote(SYM, 5.00, 5.02, 0);
        assert!((0..20).all(|i| b.on_print(SYM, 5.02, 100, 1, 10 + i * 30, None).is_none()));

        let mut b = book();
        b.on_quote(SYM, 5.00, 5.02, 0);
        b.arm(ArmedSymbol {
            arm_id: 1,
            symbol: SYM.into(),
            tradeable: false,
            not_before_ms: 0,
        });
        assert!((0..20).all(|i| b.on_print(SYM, 5.02, 100, 1, 10 + i * 30, None).is_none()));
        b.set_tradeable(1, true);
        assert!(
            entry(b.on_print(SYM, 5.02, 100, 1, 700, None)).is_some(),
            "ATR redevenu suffisant"
        );
    }

    #[test]
    fn a_busy_baseline_needs_a_real_burst() {
        let mut b = book();
        // 30 s of steady one-sided buying at 6 prints/s: imbalance is total, but the
        // rate is the ticker's normal rate — not a burst.
        let mut t = 0;
        for s in 0..30 {
            for i in 0..6 {
                t = s * 1_000 + i * 160;
                b.on_print(SYM, 5.0 + 0.001 * (s * 6 + i) as f64, 100, 1, t, None);
            }
        }
        arm(&mut b, 1);
        for i in 0..6 {
            t = 30_000 + i * 160;
            assert!(
                b.on_print(SYM, 5.2 + 0.001 * i as f64, 100, 1, t, None)
                    .is_none(),
                "même rythme que la baseline : pas de déclenchement"
            );
        }
        // Then 25 prints inside 500 ms.
        let fired = (0..25).find_map(|i| {
            entry(b.on_print(SYM, 5.3 + 0.001 * i as f64, 100, 1, t + 20 + i * 20, None))
        });
        assert_eq!(fired.expect("vraie rafale").side, Side::Long);
    }

    #[test]
    fn tick_rule_without_quotes_reads_a_flush() {
        let mut b = book();
        let t = quiet(&mut b, 0, 3, 2, 4.00);
        arm(&mut b, 1);
        let fired = (0..20).find_map(|i| {
            entry(b.on_print(
                SYM,
                3.95 - 0.005 * i as f64,
                500,
                1,
                t + 1_000 + i * 20,
                None,
            ))
        });
        assert_eq!(fired.expect("flush sans quotes").side, Side::Short);
    }

    #[test]
    fn a_stale_quote_is_ignored() {
        let mut t = FlowTracker::default();
        t.on_quote(5.00, 5.02, 0);
        assert_eq!(t.on_print(5.20, 100, 1, 1_000, None, 1_500), 1);
        // A minute later the quote says nothing any more. Trusted, 5.03 would sit
        // above its ask (buy); against the previous close of 5.20 it is a downtick.
        assert_eq!(t.on_print(5.03, 100, 1, 60_000, None, 1_500), -1);
    }

    #[test]
    fn a_replayed_green_second_nets_to_buying() {
        // Massive second replayed as O→L→H→C at 250 ms, prints split evenly.
        let mut t = FlowTracker::default();
        t.on_print(5.00, 100, 1, 0, None, 1_500); // previous second closes at 5.00
        let path = [5.01, 4.99, 5.06, 5.05];
        let mut net = 0i64;
        for (i, p) in path.iter().enumerate() {
            let d = t.on_print(*p, 250, 10, 1_000 + i as i64 * 250, None, 1_500);
            net += d as i64 * 10;
        }
        assert!(net > 0, "O→L→H→C vert doit peser acheteur (net {net})");
    }

    #[test]
    fn live_aggregates_fall_back_on_their_own_open() {
        let mut t = FlowTracker::default();
        // First aggregate ever: no previous close, no previous print — only the open.
        assert_eq!(t.on_print(5.05, 1_000, 20, 0, Some(5.00), 1_500), 1);
        // Flat against the previous close: the second's own open decides.
        assert_eq!(t.on_print(5.05, 1_000, 20, 1_000, Some(5.08), 1_500), -1);
    }

    #[test]
    fn disabled_book_holds_nothing() {
        let mut b = book();
        arm(&mut b, 1);
        b.configure(false, params(), exit_params());
        assert!(b.armed().is_none());
        assert!(b.trackers.is_empty());
        assert!(b.on_print(SYM, 5.0, 100, 1, 0, None).is_none());
        assert!(!b.arm(ArmedSymbol {
            arm_id: 2,
            symbol: SYM.into(),
            tradeable: true,
            not_before_ms: 0
        }));
    }

    #[test]
    fn arming_an_unwatched_ticker_tracks_it_until_disarmed() {
        let mut b = FlowBook::default();
        b.configure(true, params(), exit_params());
        b.arm(ArmedSymbol {
            arm_id: 1,
            symbol: "ZZZ".into(),
            tradeable: true,
            not_before_ms: 0,
        });
        assert!(b.trackers.contains_key("ZZZ"));
        b.disarm();
        assert!(!b.trackers.contains_key("ZZZ"));
    }

    #[test]
    fn an_opposite_balance_closes_a_dynamic_tp_long() {
        let mut b = book();
        b.on_quote(SYM, 5.00, 5.02, 0);
        guard_long(&mut b, 9, 5.02, true);
        let x = (0..10)
            .find_map(|i| exit(b.on_print(SYM, 5.00, 200, 1, 100 + i * 40, None)))
            .expect("le flux inversé doit sortir");
        assert_eq!(
            (x.arm_id, x.side, x.reason),
            (9, Side::Long, ExitReason::Flow)
        );
        assert!(b.guarded().is_none(), "one shot");
    }

    #[test]
    fn one_oversized_opposite_participant_closes_the_trade() {
        let mut b = book();
        b.on_quote(SYM, 5.00, 5.02, 0);
        // A 200-share average print, established over one full second.
        for i in 0..5 {
            b.on_print(SYM, 5.02, 200, 1, i * 100, None);
        }
        guard_long(&mut b, 3, 5.02, true);
        b.on_print(SYM, 5.02, 200, 1, 1_000, None); // rolls the second → baseline set
        let x = exit(b.on_print(SYM, 5.00, 9_000, 1, 1_100, None))
            .expect("un bloc vendeur doit sortir seul");
        assert_eq!(x.reason, ExitReason::Block);
        assert!(x.detail.contains("9000"), "{}", x.detail);
    }

    #[test]
    fn prints_going_our_way_never_close_the_trade() {
        let mut b = book();
        b.on_quote(SYM, 5.00, 5.02, 0);
        guard_long(&mut b, 1, 5.02, true);
        for i in 0..30 {
            assert!(b
                .on_print(SYM, 5.02, 20_000, 1, 100 + i * 30, None)
                .is_none());
        }
        assert!(b.guarded().is_some());
    }

    #[test]
    fn without_the_dynamic_tp_the_balance_is_only_watched() {
        let mut b = book();
        b.on_quote(SYM, 5.00, 5.02, 0);
        guard_long(&mut b, 1, 5.02, false);
        for i in 0..20 {
            assert!(b.on_print(SYM, 5.00, 500, 1, 100 + i * 30, None).is_none());
        }
        // …but the best price is still recorded, which is what the trail needs.
        b.on_print(SYM, 5.40, 500, 1, 2_000, None);
        assert_eq!(b.guard_best(1), Some(5.40));
    }

    #[test]
    fn a_print_through_the_trailing_stop_closes_the_trade() {
        let mut b = book();
        guard_long(&mut b, 5, 5.00, false);
        b.set_guard_stop(5, Some(5.10));
        assert!(b.on_print(SYM, 5.20, 100, 1, 100, None).is_none());
        let x = exit(b.on_print(SYM, 5.09, 100, 1, 200, None)).expect("stop suiveur");
        assert_eq!(x.reason, ExitReason::TrailStop);
        assert_eq!(b.guard_best(5), None, "garde relâchée");
    }

    #[test]
    fn the_guard_survives_apex_being_switched_off() {
        let mut b = book();
        guard_long(&mut b, 2, 5.00, true);
        b.configure(false, params(), exit_params());
        assert!(
            b.guarded().is_some(),
            "la position ouverte reste surveillée"
        );
    }

    #[test]
    fn not_before_holds_the_same_burst_back() {
        let mut b = book();
        b.on_quote(SYM, 5.00, 5.02, 0);
        b.arm(ArmedSymbol {
            arm_id: 3,
            symbol: SYM.into(),
            tradeable: true,
            not_before_ms: 2_000,
        });
        assert!((0..20).all(|i| b
            .on_print(SYM, 5.02, 100, 1, 1_000 + i * 20, None)
            .is_none()));
        assert!(entry(b.on_print(SYM, 5.02, 100, 1, 2_000, None)).is_some());
    }
}
