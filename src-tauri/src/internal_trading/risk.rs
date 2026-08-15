use crate::types::{RiskSizingResult, Side};

/// Infer direction from the stop relative to the quote midpoint, then return the
/// conservative executable side of the quote used by market/marketable-limit
/// entries. Sizing at the midpoint understates risk by half the spread.
pub fn executable_entry(stop_loss: f64, bid: f64, ask: f64) -> Option<(Side, f64)> {
    if !stop_loss.is_finite()
        || !bid.is_finite()
        || !ask.is_finite()
        || bid <= 0.0
        || ask <= 0.0
        || ask < bid
    {
        return None;
    }
    let mid = (bid + ask) / 2.0;
    let side = if stop_loss < mid {
        Side::Long
    } else {
        Side::Short
    };
    let entry = super::fills::price_for_side(side, bid, ask);
    Some((side, entry))
}

/// Compute position sizing from entry, SL, max-risk, and size bounds.
///
/// Direction is inferred from SL position:
///   SL < entry → Long (buy to open)
///   SL > entry → Short (sell to open)
pub fn compute(
    entry_price: f64,
    stop_loss: f64,
    max_risk: f64,
    min_size: u32,
    max_size: u32,
) -> RiskSizingResult {
    let side = if stop_loss < entry_price {
        Side::Long
    } else {
        Side::Short
    };
    let risk_per_share = (entry_price - stop_loss).abs();

    // `max_risk` is a hard ceiling. If even the configured minimum lot would
    // exceed it, the safe answer is zero — clamping UP to min_size would silently
    // turn a maximum into a suggestion.
    let full_size = if entry_price.is_finite()
        && stop_loss.is_finite()
        && max_risk.is_finite()
        && max_risk > 0.0
        && risk_per_share > 1e-8
        && min_size <= max_size
    {
        let raw = (max_risk / risk_per_share).floor() as i64;
        if raw < min_size as i64 {
            0
        } else {
            raw.min(max_size as i64)
        }
    } else {
        0
    };

    // A percentage bucket that cannot meet the minimum lot is unavailable. Returning
    // min_size here would make a displayed "25 %" button risk as much as "100 %".
    let bucket = |frac: f64| -> i64 {
        let s = (full_size as f64 * frac).floor() as i64;
        if s < min_size as i64 {
            0
        } else {
            s
        }
    };

    RiskSizingResult {
        entry_price,
        stop_loss,
        risk_per_share,
        full_position_size: full_size,
        size_25: bucket(0.25),
        size_50: bucket(0.50),
        size_100: full_size,
        side,
        strategy_max_risk_dollars: max_risk,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_sizing_uses_the_unfavourable_executable_price() {
        assert_eq!(
            executable_entry(9.50, 9.90, 10.10),
            Some((Side::Long, 10.10))
        );
        assert_eq!(
            executable_entry(10.50, 9.90, 10.10),
            Some((Side::Short, 9.90))
        );
    }

    #[test]
    fn invalid_or_crossed_quotes_are_refused() {
        assert!(executable_entry(9.0, 0.0, 10.0).is_none());
        assert!(executable_entry(9.0, 10.1, 10.0).is_none());
        assert!(executable_entry(f64::NAN, 9.9, 10.0).is_none());
    }

    #[test]
    fn maximum_risk_dominates_the_minimum_lot() {
        let s = compute(100.0, 80.0, 10.0, 1, 10_000);
        assert_eq!(s.full_position_size, 0, "one share would risk $20 > $10");
    }

    #[test]
    fn percentage_buttons_never_masquerade_as_a_larger_fraction() {
        let s = compute(100.0, 90.0, 10.0, 1, 10_000);
        assert_eq!(s.full_position_size, 1);
        assert_eq!(s.size_25, 0);
        assert_eq!(s.size_50, 0);
        assert_eq!(s.size_100, 1);
    }

    #[test]
    fn invalid_risk_configuration_returns_no_size() {
        assert_eq!(compute(10.0, 9.0, -1.0, 1, 100).full_position_size, 0);
        assert_eq!(compute(10.0, 9.0, 100.0, 101, 100).full_position_size, 0);
    }
}
