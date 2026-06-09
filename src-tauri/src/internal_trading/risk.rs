use crate::types::{RiskSizingResult, Side};

/// Compute position sizing from entry, SL, max-risk, and size bounds.
///
/// Direction is inferred from SL position:
///   SL < entry → Long (buy to open)
///   SL > entry → Short (sell to open)
pub fn compute(
    entry_price: f64,
    stop_loss:   f64,
    max_risk:    f64,
    min_size:    u32,
    max_size:    u32,
) -> RiskSizingResult {
    let side          = if stop_loss < entry_price { Side::Long } else { Side::Short };
    let risk_per_share = (entry_price - stop_loss).abs();

    let full_size = if risk_per_share > 1e-8 {
        let raw = (max_risk / risk_per_share).floor() as i64;
        raw.clamp(min_size as i64, max_size as i64)
    } else {
        0
    };

    // Floor each bucket; ensure at least min_size when the full bucket is non-zero
    let bucket = |frac: f64| -> i64 {
        let s = (full_size as f64 * frac).floor() as i64;
        if s == 0 && full_size > 0 { min_size as i64 } else { s }
    };

    RiskSizingResult {
        entry_price,
        stop_loss,
        risk_per_share,
        full_position_size:        full_size,
        size_25:                   bucket(0.25),
        size_50:                   bucket(0.50),
        size_100:                  full_size,
        side,
        strategy_max_risk_dollars: max_risk,
    }
}
