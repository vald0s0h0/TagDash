// Apex bracket arithmetic — pure, so the numbers the trader sees on the chart and
// the numbers sent with the order come from the same few lines.
//
// Both distances are ATR(14) on closed 10-second bars × their own multiplier,
// rounded to the tick. "ATR moyen" is taken literally: the plain mean of the last 14
// true ranges, not Wilder's smoothing, which would still remember bars from minutes
// ago on a tape that changes character in seconds.
//
// The take profit is optional: under the dynamic TP there is no target price at all,
// the tape decides when the trade is over.

use crate::market_state::aggregators::Bar;
use crate::types::Side;

pub const ATR_PERIOD: usize = 14;

/// $0.01 at or above $1, $0.0001 below (the sub-penny increment US rules allow).
pub fn tick_size(price: f64) -> f64 {
    if price >= 1.0 {
        0.01
    } else {
        0.0001
    }
}

pub fn round_to_tick(price: f64, tick: f64) -> f64 {
    let v = (price / tick).round() * tick;
    // Strip float dust (4.1000000000000005) so the value serialises cleanly.
    (v * 1e6).round() / 1e6
}

/// Mean true range of the last `period` bars (oldest → newest). The first bar of
/// the window uses the close before it when there is one.
pub fn atr(bars: &[Bar], period: usize) -> Option<f64> {
    if period == 0 || bars.len() < period {
        return None;
    }
    let start = bars.len() - period;
    let mut sum = 0.0;
    for i in start..bars.len() {
        let b = &bars[i];
        let mut tr = b.high - b.low;
        if i > 0 {
            let pc = bars[i - 1].close;
            tr = tr.max((b.high - pc).abs()).max((b.low - pc).abs());
        }
        if !tr.is_finite() || tr < 0.0 {
            return None;
        }
        sum += tr;
    }
    Some(sum / period as f64)
}

/// Bracket distance for a reference price, rounded to that price's tick.
pub fn distance(atr: f64, multiplier: f64, ref_price: f64) -> f64 {
    round_to_tick(atr * multiplier, tick_size(ref_price))
}

pub fn meets_minimum(distance: f64, min_distance: f64) -> bool {
    distance > 0.0 && distance + 1e-9 >= min_distance
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bracket {
    pub entry: f64,
    pub stop_loss: f64,
    /// None under the dynamic take profit: no target order is placed.
    pub take_profit: Option<f64>,
    pub sl_distance: f64,
    pub tp_distance: Option<f64>,
}

/// Bracket around the entry, each side at its own distance. None when a level would
/// land at or below zero.
pub fn bracket(
    side: Side,
    entry: f64,
    sl_distance: f64,
    tp_distance: Option<f64>,
) -> Option<Bracket> {
    if !entry.is_finite() || entry <= 0.0 || !sl_distance.is_finite() || sl_distance <= 0.0 {
        return None;
    }
    let tick = tick_size(entry);
    let sl = round_to_tick(
        match side {
            Side::Long => entry - sl_distance,
            Side::Short => entry + sl_distance,
        },
        tick,
    );
    if sl <= 0.0 {
        return None;
    }
    let tp = match tp_distance {
        Some(d) if d.is_finite() && d > 0.0 => {
            let tp = round_to_tick(
                match side {
                    Side::Long => entry + d,
                    Side::Short => entry - d,
                },
                tick,
            );
            if tp <= 0.0 {
                return None;
            }
            Some(tp)
        }
        Some(_) => return None,
        None => None,
    };
    Some(Bracket {
        entry,
        stop_loss: sl,
        take_profit: tp,
        sl_distance,
        tp_distance: tp.and(tp_distance),
    })
}

/// Where the trailing stop belongs, given the best price the trade has seen.
/// None until the trade is `start_r` in profit — before that the initial stop stands.
pub fn trail_stop(
    side: Side,
    entry: f64,
    best: f64,
    risk: f64,
    start_r: f64,
    trail_distance: f64,
) -> Option<f64> {
    if ![entry, best, risk, trail_distance]
        .iter()
        .all(|v| v.is_finite())
        || risk <= 0.0
        || trail_distance <= 0.0
        || entry <= 0.0
        || best <= 0.0
    {
        return None;
    }
    let progress = match side {
        Side::Long => best - entry,
        Side::Short => entry - best,
    };
    if progress + 1e-9 < start_r.max(0.0) * risk {
        return None;
    }
    let raw = match side {
        Side::Long => best - trail_distance,
        Side::Short => best + trail_distance,
    };
    let stop = round_to_tick(raw, tick_size(entry));
    (stop > 0.0).then_some(stop)
}

/// A trailing stop only ever moves toward profit, and only when the move is worth
/// rewriting the order for.
pub fn trail_improves(side: Side, current: f64, candidate: f64, step: f64) -> bool {
    match side {
        Side::Long => candidate >= current + step,
        Side::Short => candidate <= current - step,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn bar(i: i64, h: f64, l: f64, c: f64) -> Bar {
        Bar {
            time: Utc.timestamp_opt(1_700_000_000 + i * 10, 0).unwrap(),
            open: c,
            high: h,
            low: l,
            close: c,
            volume: 1_000,
            vwap: None,
            trade_count: Some(10),
        }
    }

    #[test]
    fn atr_needs_fourteen_bars() {
        let bars: Vec<Bar> = (0..13).map(|i| bar(i, 5.1, 5.0, 5.05)).collect();
        assert_eq!(atr(&bars, ATR_PERIOD), None);
    }

    #[test]
    fn atr_is_the_plain_mean_of_true_ranges_over_the_last_fourteen() {
        let mut bars: Vec<Bar> = (0..20).map(|i| bar(i, 5.10, 5.00, 5.05)).collect();
        // An old huge bar outside the window must not count.
        bars[2] = bar(2, 9.0, 1.0, 5.05);
        let a = atr(&bars, ATR_PERIOD).unwrap();
        assert!((a - 0.10).abs() < 1e-9, "{a}");
    }

    #[test]
    fn a_gap_counts_in_the_true_range() {
        let mut bars: Vec<Bar> = (0..14).map(|i| bar(i, 5.10, 5.00, 5.05)).collect();
        bars[13] = bar(13, 5.40, 5.35, 5.38); // gapped up from 5.05
        let a = atr(&bars, ATR_PERIOD).unwrap();
        let expected = (13.0 * 0.10 + 0.35) / 14.0;
        assert!((a - expected).abs() < 1e-9);
    }

    #[test]
    fn brackets_are_symmetric_and_on_tick() {
        let d = distance(0.0734, 1.0, 4.20);
        assert_eq!(d, 0.07);
        let l = bracket(Side::Long, 4.20, d, Some(d)).unwrap();
        assert_eq!((l.stop_loss, l.take_profit), (4.13, Some(4.27)));
        let s = bracket(Side::Short, 4.20, d, Some(d)).unwrap();
        assert_eq!((s.stop_loss, s.take_profit), (4.27, Some(4.13)));
    }

    #[test]
    fn each_side_can_have_its_own_atr_multiple() {
        let sl = distance(0.05, 1.0, 4.20);
        let tp = distance(0.05, 2.0, 4.20);
        let l = bracket(Side::Long, 4.20, sl, Some(tp)).unwrap();
        assert_eq!((l.stop_loss, l.take_profit), (4.15, Some(4.30)));
        let s = bracket(Side::Short, 4.20, sl, Some(tp)).unwrap();
        assert_eq!((s.stop_loss, s.take_profit), (4.25, Some(4.10)));
    }

    #[test]
    fn a_dynamic_take_profit_leaves_no_target() {
        let b = bracket(Side::Long, 4.20, 0.07, None).unwrap();
        assert_eq!(b.take_profit, None);
        assert_eq!(b.tp_distance, None);
        assert_eq!(b.stop_loss, 4.13);
    }

    #[test]
    fn the_trail_waits_for_the_trade_to_be_in_profit() {
        // Long at 4.20, risk 0.07, trail 0.07 behind, start at 0.5 R (= 0.035).
        assert_eq!(trail_stop(Side::Long, 4.20, 4.23, 0.07, 0.5, 0.07), None);
        assert_eq!(
            trail_stop(Side::Long, 4.20, 4.24, 0.07, 0.5, 0.07),
            Some(4.17),
            "0.04 de profit dépasse 0,5 R"
        );
        // From the first tick when start_r is 0.
        assert_eq!(
            trail_stop(Side::Long, 4.20, 4.20, 0.07, 0.0, 0.07),
            Some(4.13)
        );
    }

    #[test]
    fn the_trail_follows_a_short_upside_down() {
        assert_eq!(
            trail_stop(Side::Short, 4.20, 4.16, 0.07, 0.5, 0.07),
            Some(4.23)
        );
        assert_eq!(trail_stop(Side::Short, 4.20, 4.19, 0.07, 0.5, 0.07), None);
    }

    #[test]
    fn the_trail_never_loosens_and_ignores_crumbs() {
        assert!(trail_improves(Side::Long, 4.13, 4.17, 0.01));
        assert!(
            !trail_improves(Side::Long, 4.17, 4.13, 0.01),
            "jamais en arrière"
        );
        assert!(
            !trail_improves(Side::Long, 4.17, 4.175, 0.01),
            "pas pour un demi-cent"
        );
        assert!(trail_improves(Side::Short, 4.27, 4.23, 0.01));
        assert!(!trail_improves(Side::Short, 4.23, 4.27, 0.01));
    }

    #[test]
    fn the_minimum_distance_blocks_a_tight_bracket() {
        assert!(!meets_minimum(distance(0.052, 1.0, 3.0), 0.06));
        assert!(
            meets_minimum(distance(0.058, 1.0, 3.0), 0.06),
            "0.058 arrondi à 0.06"
        );
        assert!(meets_minimum(distance(0.04, 1.5, 3.0), 0.06));
        assert!(!meets_minimum(0.0, 0.0));
    }

    #[test]
    fn a_stop_through_zero_is_refused() {
        assert!(bracket(Side::Long, 0.05, 0.06, Some(0.06)).is_none());
        assert!(trail_stop(Side::Long, 0.05, 0.05, 0.06, 0.0, 0.06).is_none());
    }
}
