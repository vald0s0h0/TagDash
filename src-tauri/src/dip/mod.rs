// Dip — the anticipated level, part of the Micro Pullback strategy.
//
// Where Apex reads the tape and asks "which way is this packet of prints going?",
// Dip reads a PLACE the trader has already chosen and asks "did the strike land
// where it was expected?". The trader points at a price with a hotkey (G by
// default) and a thick green band is laid there: a level with a tolerance, a few
// cents of it, which he then tunes one cent at a time on each side. Where the price
// is when he places it decides what the band is — under the price it is a support,
// over it a resistance — and therefore which way the trade goes.
//
// From that moment the band is a target and the price is a snake. The setup is
// worth a trade only if all three happen, in this order:
//   • the price PENETRATES the band, violently — measured against the ticker's own
//     very recent travel, ATR(14) on 10-second bars, not against an absolute number
//     of cents (this is micro-structure: a 1.5 s window inside one bar);
//   • it does not go straight through the far edge — that is not an attack on a
//     level, that is a level that no longer exists;
//   • it TURNS. The order leaves as the turn starts, not once it is confirmed.
// A strike too fast to follow is deliberately let go: TagDash would send an order
// into a price that has already gone. Between "nothing happened" and "out of
// reach" there is a band of speeds that can actually be traded, and both of its
// edges are settings.
//
// The bracket is the same arithmetic as Apex's — ATR(14) on 10-second bars, one
// multiplier for the stop and one for the target (`apex::levels`) — sized at a
// fraction of Micro Pullback's max risk. No dynamic exit: both legs rest at the
// broker, so a Dip trade survives TagDash being closed.
//
// LIFECYCLE — one zone at a time, across every ticker:
//   Idle ──G──▶ Armed ──turn──▶ Firing ──order ok──▶ InTrade ──flat──▶ Idle
//                │  ▲                 │
//                │  │ attack lapsed   └─ order refused ─▶ Idle
//                ▼  └── (still armed: too fast, or it just sat there)
//   G inside the band disarms; G anywhere else moves the band there. G while
//   Firing / InTrade does nothing: the open trade is managed by its bracket.
//
// TRADETALLY — the trade is filed under strategy id `dip`, whose setup name is
// "Dip", never "Micro Pullback" (see `strategies::setup_name`). Its risk budget is
// still Micro Pullback's (`OWNER_STRATEGY_ID`).
//
// DATA — works on every tape the app ingests, live or replayed: Alpaca SIP prints,
// the trade flat files and Massive second aggregates all arrive as prints on the
// same `MarketState` ingest calls that feed the watch book.

pub mod options;
pub mod watch;

pub use options::DipOptions;

use std::sync::{
    atomic::{AtomicU64, Ordering},
    RwLock,
};

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::types::Side;

/// Strategy id stamped on Dip trades and chart contexts.
pub const STRATEGY_ID: &str = "dip";
/// Setup name sent to TradeTally.
pub const SETUP_NAME: &str = "Dip";
/// The strategy Dip belongs to — its risk ($) is the base Dip sizes from.
pub const OWNER_STRATEGY_ID: &str = crate::strategies::micro_pullback::ID;
/// Tauri event carrying `DipStatus` on every change.
pub const EVENT: &str = "dip-status";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DipPhase {
    Disabled,
    #[default]
    Idle,
    Armed,
    Firing,
    InTrade,
}

#[derive(Debug, Clone, Serialize)]
pub struct DipNotice {
    /// Monotonic, so the UI shows each notice once.
    pub seq: u64,
    /// "info" | "warn" | "error"
    pub level: &'static str,
    pub message: String,
}

/// What the UI renders: the band on the chart and the toasts.
#[derive(Debug, Clone, Serialize)]
pub struct DipStatus {
    pub phase: DipPhase,
    pub symbol: Option<String>,
    pub zone_id: Option<String>,
    /// The price the trader pointed at — the middle of the band.
    pub level: Option<f64>,
    pub low: Option<f64>,
    pub high: Option<f64>,
    /// Total thickness of the band in cents, as the hotkeys tuned it.
    pub thickness_cents: Option<f64>,
    /// Long = the band is a support (it sits under the price), Short = resistance.
    pub side: Option<Side>,
    pub atr: Option<f64>,
    pub sl_distance: Option<f64>,
    pub tp_distance: Option<f64>,
    pub min_distance: f64,
    /// False while a distance is under `min_distance`: the band is drawn and
    /// watched, but nothing fires.
    pub tradeable: bool,
    /// The price is inside the band right now — an attack is under way.
    pub attacking: bool,
    /// Fastest approach of that attack, in ticker-ordinary travels.
    pub speed_ratio: f64,
    pub long_enabled: bool,
    pub short_enabled: bool,
    pub trade_id: Option<String>,
    pub stop_price: Option<f64>,
    pub take_profit: Option<f64>,
    pub notice: Option<DipNotice>,
}

/// Executor-side state behind `DipStatus`.
#[derive(Debug, Clone, Default)]
pub struct Runtime {
    pub phase: DipPhase,
    pub arm_id: u64,
    pub symbol: Option<String>,
    pub zone_id: Option<String>,
    pub level: Option<f64>,
    pub low: Option<f64>,
    pub high: Option<f64>,
    /// Half-thickness ($) — each edge sits this far from the level.
    pub half: f64,
    pub side: Option<Side>,
    pub atr: Option<f64>,
    pub sl_distance: Option<f64>,
    pub tp_distance: Option<f64>,
    pub tradeable: bool,
    /// A position or a working entry order stands on the ticker — whoever opened
    /// it. The arming SURVIVES it (Apex and Dip are armed independently and either
    /// may fire first) but its trigger is held: the book nets one position per
    /// ticker, so a second entry would stack onto the first instead of standing on
    /// its own. Mirrored into `tradeable`, and cleared as soon as the ticker is flat.
    pub held: bool,
    pub trade_id: Option<String>,
    // ── Open trade ──
    pub entry_price: Option<f64>,
    pub stop_price: Option<f64>,
    pub take_profit: Option<f64>,
    /// Replay clock state at placement — any change (start, stop, seek) disarms.
    pub replay_generation: u64,
    pub replay_active: bool,
    /// When the order went out (wall clock) and whether its position was seen yet;
    /// with Alpaca the position only appears once `trade_updates` reports the fill.
    pub fired_at: Option<DateTime<Utc>>,
    pub position_seen: bool,
    pub last_atr_at: Option<DateTime<Utc>>,
    pub notice: Option<DipNotice>,
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

    /// Total thickness of the band in cents, rounded to the tenth the hotkeys move it by.
    pub fn thickness_cents(&self) -> f64 {
        (self.half * 200.0 * 10.0).round() / 10.0
    }
}

pub struct DipShared {
    pub options: RwLock<DipOptions>,
    pub runtime: RwLock<Runtime>,
    /// Live attack readout, refreshed from the watch book by the executor's
    /// housekeeping — it changes on the hot path, far too often to emit.
    pub attack: RwLock<(bool, f64)>,
    seq: AtomicU64,
}

impl DipShared {
    pub fn new(options: DipOptions) -> Self {
        Self {
            options: RwLock::new(options),
            runtime: RwLock::new(Runtime::default()),
            attack: RwLock::new((false, 0.0)),
            seq: AtomicU64::new(1),
        }
    }

    pub fn next_id(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::Relaxed)
    }

    pub fn notice(&self, level: &'static str, message: impl Into<String>) -> DipNotice {
        DipNotice {
            seq: self.next_id(),
            level,
            message: message.into(),
        }
    }

    pub fn status(&self) -> DipStatus {
        let o = *self.options.read().unwrap();
        let rt = self.runtime.read().unwrap().clone();
        let (attacking, speed_ratio) = *self.attack.read().unwrap();
        let phase = if !o.enabled && rt.phase == DipPhase::Idle {
            DipPhase::Disabled
        } else {
            rt.phase
        };
        let thickness_cents = rt.low.is_some().then(|| rt.thickness_cents());
        DipStatus {
            phase,
            symbol: rt.symbol,
            zone_id: rt.zone_id,
            level: rt.level,
            low: rt.low,
            high: rt.high,
            thickness_cents,
            side: rt.side,
            atr: rt.atr,
            sl_distance: rt.sl_distance,
            tp_distance: rt.tp_distance,
            min_distance: o.min_distance(),
            tradeable: rt.tradeable,
            attacking: attacking && phase == DipPhase::Armed,
            speed_ratio,
            long_enabled: o.long_enabled,
            short_enabled: o.short_enabled,
            trade_id: rt.trade_id,
            stop_price: rt.stop_price,
            take_profit: rt.take_profit,
            notice: rt.notice,
        }
    }
}

/// Which way a band placed at `level` is traded, from where the price is now.
/// None while the price is inside the band: the band has no role yet, and guessing
/// one would be inventing the trade the setup exists to qualify.
pub fn role_for(price: f64, low: f64, high: f64) -> Option<Side> {
    if !price.is_finite() || price <= 0.0 {
        return None;
    }
    if price > high {
        Some(Side::Long) // a support under the price: it is bought back
    } else if price < low {
        Some(Side::Short) // a resistance over it: it is sold back
    } else {
        None
    }
}

#[cfg(test)]
mod wiring_tests {
    //! The wiring, end to end: prints enter through the real `MarketState` ingest
    //! calls and the trigger comes out of the executor's channel.

    use chrono::{DateTime, TimeZone, Utc};

    use super::watch::{ArmedZone, DipParams, DipSignal, DipTrigger};
    use crate::market_state::MarketState;
    use crate::types::Side;

    const SYM: &str = "DIPW";
    /// 10 cents of range per 10 s bar.
    const ATR: f64 = 0.10;

    fn market(
        side: Side,
        low: f64,
        high: f64,
    ) -> (
        MarketState,
        tokio::sync::mpsc::UnboundedReceiver<DipSignal>,
    ) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut ms = MarketState::new();
        ms.dip.set_sender(tx);
        ms.dip.configure(true, DipParams::default());
        ms.dip.set_watch([SYM.to_string()]);
        ms.dip.arm(ArmedZone::new(
            11,
            SYM.into(),
            side,
            low,
            high,
            true,
            ATR,
        ));
        (ms, rx)
    }

    fn at(ms: i64) -> DateTime<Utc> {
        Utc.timestamp_millis_opt(1_780_000_000_000 + ms).unwrap()
    }

    fn entry(signal: DipSignal) -> DipTrigger {
        match signal {
            DipSignal::Entry(t) => t,
            DipSignal::Lapse(l) => panic!("abandon inattendu : {l:?}"),
        }
    }

    #[test]
    fn alpaca_sip_prints_flushing_into_a_support_fire_through_market_state() {
        let (mut ms, mut rx) = market(Side::Long, 4.00, 4.06);
        // Three calm seconds at 4.20, so the path reaches back a full window.
        for i in 0..30 {
            ms.on_trade(SYM, 4.20, 300, at(i * 100), at(i * 100), 1_000, 2_000);
        }
        // Then 17 cents down in 1.4 s — about 11× the ordinary travel.
        for i in 1..=14 {
            let px = 4.20 - 0.17 * (i as f64 / 14.0);
            let t = at(3_000 + i * 100);
            ms.on_trade(SYM, px, 800, t, t, 1_000, 2_000);
        }
        assert!(rx.try_recv().is_err(), "rien tant que ça descend");
        // …and the first print back up is the order.
        ms.on_trade(SYM, 4.05, 900, at(4_500), at(4_500), 1_000, 2_000);
        let t = entry(rx.try_recv().expect("trigger envoyé sur le canal"));
        assert_eq!((t.arm_id, t.side), (11, Side::Long));
        assert!(t.speed_ratio > 3.0, "vitesse relative {}", t.speed_ratio);
        assert!(rx.try_recv().is_err(), "un seul déclenchement par zone");
    }

    #[test]
    fn replayed_prints_pushing_into_a_resistance_fire_short() {
        let (mut ms, mut rx) = market(Side::Short, 3.94, 4.00);
        for i in 0..30 {
            ms.on_replay_trade(SYM, 3.80, 300, 1, at(i * 100));
        }
        for i in 1..=14 {
            ms.on_replay_trade(SYM, 3.80 + 0.17 * (i as f64 / 14.0), 800, 1, at(3_000 + i * 100));
        }
        ms.on_replay_trade(SYM, 3.95, 900, 1, at(4_500));
        assert_eq!(entry(rx.try_recv().expect("flush")).side, Side::Short);
    }

    #[test]
    fn a_disabled_dip_measures_nothing() {
        let (mut ms, mut rx) = market(Side::Long, 4.00, 4.06);
        ms.dip.configure(false, DipParams::default());
        for i in 0..30 {
            ms.on_trade(SYM, 4.20, 300, at(i * 100), at(i * 100), 1_000, 2_000);
        }
        for i in 1..=14 {
            let px = 4.20 - 0.17 * (i as f64 / 14.0);
            let t = at(3_000 + i * 100);
            ms.on_trade(SYM, px, 800, t, t, 1_000, 2_000);
        }
        ms.on_trade(SYM, 4.05, 900, at(4_500), at(4_500), 1_000, 2_000);
        assert!(rx.try_recv().is_err());
        assert!(ms.dip.armed().is_none());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_band_under_the_price_is_a_support_and_one_over_it_a_resistance() {
        assert_eq!(role_for(4.20, 4.00, 4.06), Some(Side::Long));
        assert_eq!(role_for(3.80, 4.00, 4.06), Some(Side::Short));
    }

    #[test]
    fn a_band_the_price_is_already_inside_has_no_role() {
        assert_eq!(role_for(4.03, 4.00, 4.06), None);
        assert_eq!(role_for(4.00, 4.00, 4.06), None);
        assert_eq!(role_for(4.06, 4.00, 4.06), None);
    }

    #[test]
    fn the_thickness_readout_is_what_the_hotkeys_tuned() {
        let rt = Runtime {
            half: 0.03,
            ..Default::default()
        };
        assert_eq!(rt.thickness_cents(), 6.0);
        let rt = Runtime {
            half: 0.005,
            ..Default::default()
        };
        assert_eq!(rt.thickness_cents(), 1.0);
    }
}
