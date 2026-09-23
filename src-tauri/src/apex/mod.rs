// Apex — ultra-fast print-balance entries, part of the Micro Pullback strategy.
//
// The trader arms Apex on the ticker under the mouse with a hotkey (V by default).
// From then on the flow book (`flow`, inside `MarketState`) signs every print and,
// the moment a packet of prints leans hard one way, sends a trigger; the executor
// (`commands::apex`) immediately sends a MARKET order in that direction with a
// bracket built on ATR(14) over 10-second bars — one multiplier for the stop, one
// for the target — sized at a fraction of Micro Pullback's max risk.
//
// THE EXIT is either a resting order or a decision:
//   • default — the take profit rests beside the stop and the trade needs nobody;
//   • TP dynamique — no target order at all. The flow book keeps signing prints on
//     the open position and closes it at market (a marketable limit in extended
//     hours) the instant the balance turns against it, or one oversized opposite
//     participant prints. It does not wait for a profit;
//   • SL suiveur — once the trade is `sl_trail_start_r` in profit the stop follows
//     the best price seen, one trail-ATR behind, and never moves back. With the
//     internal broker the bracket order itself moves; with Alpaca the venue stop
//     stays where it was as the safety net and TagDash closes at market when its own
//     trailing level is crossed.
// Both options need TagDash running: what they remove is a resting venue order.
//
// LIFECYCLE — one Apex at a time, across every zone:
//   Idle ──V──▶ Armed ──burst──▶ Firing ──order ok──▶ InTrade ──flat──▶ Idle
//                 │  ▲                 │
//                 V  │ skipped         └─ order refused ─▶ Idle
//                 ▼  └──────── (re-armed, e.g. no quote yet)
//               Idle
// V while Armed disarms (the anti-mistake rule). V while Firing/InTrade does
// nothing: the open Apex trade is managed by its bracket, not by the hotkey.
//
// TRADETALLY — the trade is filed under strategy id `apex`, whose setup name is
// "Apex", never "Micro Pullback" (see `strategies::setup_name`). Its risk budget
// is still Micro Pullback's (`OWNER_STRATEGY_ID`).
//
// DATA — works on every tape the app ingests: Alpaca SIP prints + quotes (quote
// rule), the trade flat files (real prints, quotes over the ignition windows) and
// Massive second aggregates, live or replayed (anchored tick rule) — see `flow`.

pub mod flow;
pub mod levels;
pub mod options;

pub use options::ApexOptions;

use std::sync::{
    atomic::{AtomicU64, Ordering},
    RwLock,
};

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::types::Side;

/// Strategy id stamped on Apex trades and chart contexts.
pub const STRATEGY_ID: &str = "apex";
/// Setup name sent to TradeTally.
pub const SETUP_NAME: &str = "Apex";
/// The strategy Apex belongs to — its risk ($) is the base Apex sizes from.
pub const OWNER_STRATEGY_ID: &str = crate::strategies::micro_pullback::ID;
/// Tauri event carrying `ApexStatus` on every change.
pub const EVENT: &str = "apex-status";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ApexPhase {
    Disabled,
    #[default]
    Idle,
    Armed,
    Firing,
    InTrade,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApexNotice {
    /// Monotonic, so the UI shows each notice once.
    pub seq: u64,
    /// "info" | "warn" | "error"
    pub level: &'static str,
    pub message: String,
}

/// What the UI renders: the lines on the chart and the toasts.
#[derive(Debug, Clone, Serialize)]
pub struct ApexStatus {
    pub phase: ApexPhase,
    pub symbol: Option<String>,
    pub zone_id: Option<String>,
    pub atr: Option<f64>,
    /// Stop distance ($) the armed lines are drawn at.
    pub sl_distance: Option<f64>,
    /// Target distance ($) — None under the dynamic take profit.
    pub tp_distance: Option<f64>,
    pub min_distance: f64,
    /// False while a distance is under `min_distance`: armed, but nothing fires.
    pub tradeable: bool,
    pub long_enabled: bool,
    pub short_enabled: bool,
    pub tp_dynamic: bool,
    pub sl_trailing: bool,
    pub side: Option<Side>,
    pub trade_id: Option<String>,
    /// Live stop of the open trade — it moves while the trail follows.
    pub stop_price: Option<f64>,
    pub trail_active: bool,
    pub notice: Option<ApexNotice>,
}

/// Executor-side state behind `ApexStatus`.
#[derive(Debug, Clone, Default)]
pub struct Runtime {
    pub phase: ApexPhase,
    pub arm_id: u64,
    pub symbol: Option<String>,
    pub zone_id: Option<String>,
    pub atr: Option<f64>,
    pub sl_distance: Option<f64>,
    /// None under the dynamic take profit: there is no target price.
    pub tp_distance: Option<f64>,
    pub tradeable: bool,
    /// A position or a working entry order stands on the ticker — whoever opened
    /// it. The arming SURVIVES it (Apex and Dip are armed independently and either
    /// may fire first) but its trigger is held: the book nets one position per
    /// ticker, so a second entry would stack onto the first instead of standing on
    /// its own. Mirrored into `tradeable`, and cleared as soon as the ticker is flat.
    pub held: bool,
    pub side: Option<Side>,
    pub trade_id: Option<String>,
    // ── Open trade ──
    pub entry_price: Option<f64>,
    /// Initial stop distance — the R the trailing stop measures progress in.
    pub risk_per_share: Option<f64>,
    /// Where the stop is now; it moves while the trail follows.
    pub stop_price: Option<f64>,
    pub trail_distance: Option<f64>,
    pub trail_active: bool,
    /// The order went to a venue, so no position exists in the internal book.
    pub venue: bool,
    /// An exit order is out; nothing may send a second one.
    pub exit_pending: bool,
    /// Replay clock state at arming — any change (start, stop, seek) disarms.
    pub replay_generation: u64,
    pub replay_active: bool,
    /// When the order went out (wall clock) and whether its position was seen yet;
    /// with Alpaca the position only appears once `trade_updates` reports the fill.
    pub fired_at: Option<DateTime<Utc>>,
    pub position_seen: bool,
    pub last_atr_at: Option<DateTime<Utc>>,
    pub notice: Option<ApexNotice>,
}

impl Runtime {
    /// Back to Idle, keeping only the notice.
    pub fn reset(&mut self) {
        let notice = self.notice.take();
        *self = Runtime {
            notice,
            ..Default::default()
        };
    }
}

pub struct ApexShared {
    pub options: RwLock<ApexOptions>,
    pub runtime: RwLock<Runtime>,
    seq: AtomicU64,
}

impl ApexShared {
    pub fn new(options: ApexOptions) -> Self {
        Self {
            options: RwLock::new(options),
            runtime: RwLock::new(Runtime::default()),
            seq: AtomicU64::new(1),
        }
    }

    pub fn next_id(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::Relaxed)
    }

    pub fn notice(&self, level: &'static str, message: impl Into<String>) -> ApexNotice {
        ApexNotice {
            seq: self.next_id(),
            level,
            message: message.into(),
        }
    }

    pub fn status(&self) -> ApexStatus {
        let o = *self.options.read().unwrap();
        let rt = self.runtime.read().unwrap().clone();
        let phase = if !o.enabled && rt.phase == ApexPhase::Idle {
            ApexPhase::Disabled
        } else {
            rt.phase
        };
        ApexStatus {
            phase,
            symbol: rt.symbol,
            zone_id: rt.zone_id,
            atr: rt.atr,
            sl_distance: rt.sl_distance,
            tp_distance: rt.tp_distance,
            min_distance: o.min_distance(),
            tradeable: rt.tradeable,
            long_enabled: o.long_enabled,
            short_enabled: o.short_enabled,
            tp_dynamic: o.tp_dynamic,
            sl_trailing: o.sl_trailing,
            side: rt.side,
            trade_id: rt.trade_id,
            stop_price: rt.stop_price,
            trail_active: rt.trail_active,
            notice: rt.notice,
        }
    }
}

/// "0.07" for a cent-grid price or distance, "0.0735" for a sub-penny one.
pub fn fmt_px(v: f64) -> String {
    if ((v * 100.0) - (v * 100.0).round()).abs() < 1e-6 {
        format!("{v:.2}")
    } else {
        format!("{v:.4}")
    }
}

#[cfg(test)]
mod tests {
    //! The wiring, end to end: prints enter through the real `MarketState` ingest
    //! calls and the trigger comes out of the executor's channel.

    use chrono::{TimeZone, Utc};

    use super::flow::{
        ArmedSymbol, ExitParams, ExitReason, FlowParams, FlowSignal, GuardedPosition,
    };
    use crate::market_state::MarketState;
    use crate::types::Side;

    const SYM: &str = "APEX";

    fn market() -> (
        MarketState,
        tokio::sync::mpsc::UnboundedReceiver<FlowSignal>,
    ) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut ms = MarketState::new();
        ms.apex.set_sender(tx);
        ms.apex
            .configure(true, FlowParams::default(), ExitParams::default());
        ms.apex.set_watch([SYM.to_string()]);
        ms.apex.arm(ArmedSymbol {
            arm_id: 42,
            symbol: SYM.into(),
            tradeable: true,
            not_before_ms: 0,
        });
        (ms, rx)
    }

    fn at(ms: i64) -> chrono::DateTime<Utc> {
        Utc.timestamp_millis_opt(1_780_000_000_000 + ms).unwrap()
    }

    fn entry(signal: FlowSignal) -> super::flow::FlowTrigger {
        match signal {
            FlowSignal::Entry(t) => t,
            FlowSignal::Exit(x) => panic!("sortie inattendue : {x:?}"),
        }
    }

    fn exit(signal: FlowSignal) -> super::flow::ExitTrigger {
        match signal {
            FlowSignal::Exit(x) => x,
            FlowSignal::Entry(t) => panic!("entrée inattendue : {t:?}"),
        }
    }

    #[test]
    fn alpaca_sip_prints_at_the_ask_fire_through_market_state() {
        let (mut ms, mut rx) = market();
        ms.on_quote(SYM, 3.00, 3.02, at(0), at(0));
        for i in 0..12 {
            ms.on_trade(
                SYM,
                3.02,
                500,
                at(10 + i * 40),
                at(10 + i * 40),
                1_000,
                2_000,
            );
        }
        let t = entry(rx.try_recv().expect("trigger envoyé sur le canal"));
        assert_eq!((t.arm_id, t.side), (42, Side::Long));
        assert!(rx.try_recv().is_err(), "un seul déclenchement par armement");
    }

    #[test]
    fn replayed_flat_file_prints_hitting_the_bid_fire_short() {
        let (mut ms, mut rx) = market();
        ms.on_quote(SYM, 3.00, 3.02, at(0), at(0));
        for i in 0..12 {
            ms.on_replay_trade(SYM, 3.00, 800, 1, at(10 + i * 30));
        }
        assert_eq!(entry(rx.try_recv().expect("flush")).side, Side::Short);
    }

    #[test]
    fn massive_second_aggregates_fire_on_reconstructed_prints() {
        let (mut ms, mut rx) = market();
        // A calm second, then a second of 40 trades closing well above it.
        ms.on_second_aggregate(
            SYM,
            3.00,
            3.01,
            2.99,
            3.00,
            None,
            1_000,
            3,
            at(0),
            at(0),
            1_000,
            2_000,
        );
        ms.on_second_aggregate(
            SYM,
            3.01,
            3.12,
            3.01,
            3.11,
            None,
            20_000,
            40,
            at(1_000),
            at(1_000),
            1_000,
            2_000,
        );
        assert_eq!(
            entry(rx.try_recv().expect("agrégat haussier")).side,
            Side::Long
        );
    }

    #[test]
    fn the_dynamic_take_profit_closes_on_a_reversed_tape() {
        let (mut ms, mut rx) = market();
        ms.apex.disarm();
        ms.on_quote(SYM, 3.00, 3.02, at(0), at(0));
        // A long opened at 3.02, now watched by the flow book.
        ms.apex.guard(GuardedPosition {
            arm_id: 42,
            symbol: SYM.into(),
            side: Side::Long,
            watch_flow: true,
            stop_price: None,
            best_price: 3.02,
        });
        for i in 0..8 {
            ms.on_trade(
                SYM,
                3.00,
                400,
                at(100 + i * 40),
                at(100 + i * 40),
                1_000,
                2_000,
            );
        }
        let x = exit(rx.try_recv().expect("le flux inversé doit sortir"));
        assert_eq!(
            (x.arm_id, x.side, x.reason),
            (42, Side::Long, ExitReason::Flow)
        );
        assert!(ms.apex.guarded().is_none());
    }

    #[test]
    fn a_disabled_apex_measures_nothing() {
        let (mut ms, mut rx) = market();
        ms.apex
            .configure(false, FlowParams::default(), ExitParams::default());
        ms.on_quote(SYM, 3.00, 3.02, at(0), at(0));
        for i in 0..30 {
            ms.on_trade(
                SYM,
                3.02,
                500,
                at(10 + i * 20),
                at(10 + i * 20),
                1_000,
                2_000,
            );
        }
        assert!(rx.try_recv().is_err());
        assert!(ms.apex.armed().is_none());
    }
}
