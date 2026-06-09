use crate::types::Side;

/// Conservative fill price:
///   Buy  (Long entry or Short cover) → ask  (unfavorable for buyer)
///   Sell (Short entry or Long exit)  → bid  (unfavorable for seller)
pub fn price_for_side(side: Side, bid: f64, ask: f64) -> f64 {
    match side {
        Side::Long  => ask,
        Side::Short => bid,
    }
}
