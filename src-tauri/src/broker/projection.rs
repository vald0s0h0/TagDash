// Present the venue's book in the shapes the UI already renders.
//
// The rule this file exists to enforce: when Alpaca is the active broker, every
// position and order on screen is one that exists on Alpaca's servers. Nothing here
// invents, completes, or smooths anything — a field the venue does not supply stays
// `None` rather than being computed into something that looks authoritative.
//
// The mapping is possible because `ChartState` keys its trade context by SYMBOL and
// sets `zone_id == symbol`, so a venue position (which knows only the symbol) can
// recover its zone, tradeID and strategy without TagDash having to have placed it.
// That matters: a position opened from Alpaca's own web UI shows up here correctly.

use chrono::{DateTime, Utc};

use crate::broker::journal::{levels_from_mirror, BrokerJournal};
use crate::broker::{BrokerMirror, BrokerOrder, BrokerOrderStatus, BrokerPosition};
use crate::chart_state::ChartState;
use crate::types::{InternalOrder, OrderStatus, OrderType, Position, Side};

/// The venue's open positions, as `Position` rows.
pub fn positions(
    mirror: &BrokerMirror,
    chart: &ChartState,
    journal: &BrokerJournal,
) -> Vec<Position> {
    let mut out: Vec<Position> = mirror
        .positions
        .values()
        .filter(|p| p.quantity != 0.0)
        .map(|p| position_of(p, mirror, chart, journal))
        .collect();
    out.sort_by(|a, b| a.symbol.cmp(&b.symbol));
    out
}

fn position_of(
    p: &BrokerPosition,
    mirror: &BrokerMirror,
    chart: &ChartState,
    journal: &BrokerJournal,
) -> Position {
    let ctx = chart.get_context(&p.symbol);
    let trade = journal.open.get(&p.symbol);
    // Exits are whatever is actually resting at the venue right now — not what the
    // chart happens to be drawing.
    let (stop_loss, take_profit) = levels_from_mirror(mirror, &p.symbol);

    Position {
        // The journal's id is the `client_order_id` we sent, so TagDash, Alpaca and
        // TradeTally all name the trade identically. A position we never opened
        // (placed from Alpaca's web UI, or surviving a reinstall) has no journal
        // entry; fall back to the chart's tradeID, then to a venue-derived one.
        trade_id: trade
            .map(|t| t.trade_id.clone())
            .or_else(|| ctx.as_ref().and_then(|c| c.trade_id.clone()))
            .unwrap_or_else(|| format!("alpaca-{}", p.symbol)),
        zone_id: p.symbol.clone(),
        symbol: p.symbol.clone(),
        strategy_id: ctx.map(|c| c.strategy_id).unwrap_or_default(),
        side: if p.is_short() {
            Side::Short
        } else {
            Side::Long
        },
        quantity: p.quantity.round() as i64,
        avg_entry_price: p.avg_entry_price,
        stop_loss,
        take_profit,
        // Alpaca's own number. Recomputing it locally would let the screen disagree
        // with the account statement.
        unrealized_pnl: p.unrealized_pnl,
        r_multiple: r_multiple(p, stop_loss),
        // Only the journal knows when we opened it; a position adopted from elsewhere
        // has no known open time, and `now` is the honest placeholder for "as of this
        // sync" — it is display-only and never feeds sizing or risk.
        opened_at: trade.map(|t| t.opened_at).unwrap_or_else(Utc::now),
        high_water: trade.map(|t| t.high_water).unwrap_or(p.avg_entry_price),
        low_water: trade.map(|t| t.low_water).unwrap_or(p.avg_entry_price),
        // Auto-BE and auto-TP move levels by themselves. They belong to the internal
        // simulator; against a real venue the resting orders are Alpaca's, and TagDash
        // does not silently reprice them.
        auto_be_applied: false,
        auto_tp_applied: false,
        // The venue doesn't expose a per-leg fill breakdown here — stay honest
        // and don't fabricate a partial-fill count for an Alpaca position.
        filled_entries: 0,
        pending_entries: 0,
    }
}

/// R multiple against the stop actually resting at the venue. `None` unless both the
/// venue's mark and a real stop are known — a guessed R is worse than a blank one.
fn r_multiple(p: &BrokerPosition, stop_loss: Option<f64>) -> Option<f64> {
    let (price, sl) = (p.market_price?, stop_loss?);
    let risk = (p.avg_entry_price - sl).abs();
    if risk < 1e-9 {
        return None;
    }
    let move_ = if p.is_short() {
        p.avg_entry_price - price
    } else {
        price - p.avg_entry_price
    };
    Some(move_ / risk)
}

/// The venue's working orders, as `InternalOrder` rows.
///
/// Terminal orders are dropped: this feeds the "what is live right now" panels, and
/// `InternalBook::get_pending_orders` has the same contract.
pub fn orders(mirror: &BrokerMirror) -> Vec<InternalOrder> {
    let mut out: Vec<InternalOrder> = mirror
        .orders
        .values()
        .filter(|o| !o.status.is_terminal())
        .map(order_of)
        .collect();
    out.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then(a.symbol.cmp(&b.symbol))
    });
    out
}

fn order_of(o: &BrokerOrder) -> InternalOrder {
    InternalOrder {
        // The VENUE's id, deliberately: it is the only handle a cancel can use, and
        // the cancel path routes to Alpaca whenever Alpaca is active.
        order_id: o.id.clone(),
        trade_id: None,
        zone_id: o.symbol.clone(),
        symbol: o.symbol.clone(),
        side: if o.side.eq_ignore_ascii_case("sell") {
            Side::Short
        } else {
            Side::Long
        },
        order_type: match o.order_type.as_str() {
            "market" => OrderType::Market,
            "stop" | "stop_limit" | "trailing_stop" => OrderType::Stop,
            _ => OrderType::Limit,
        },
        limit_price: o.limit_price,
        stop_price: o.stop_price,
        quantity: o.quantity.round() as i64,
        // A bracket leg IS the stop/target; it does not carry its own. Nesting them
        // here would draw the same level twice.
        stop_loss: None,
        take_profit: None,
        status: match o.status {
            BrokerOrderStatus::Filled => OrderStatus::Filled,
            BrokerOrderStatus::Closed => OrderStatus::Cancelled,
            _ => OrderStatus::Pending,
        },
        oco_group: o.parent_id.clone(),
        // Alpaca's bracket legs only ever reduce; a parentless order is an entry.
        reduce_only: o.parent_id.is_some(),
        created_at: o.submitted_at.unwrap_or_else(Utc::now),
    }
}

/// True when the mirror is too old to be shown as fact.
pub fn is_stale(mirror: &BrokerMirror, now: DateTime<Utc>) -> bool {
    mirror.is_stale(now)
}

// NOTE — not yet done: dragging SL/TP on the chart does not move the venue's resting
// bracket legs. The levels are correct in the positions panel (read from the mirror
// above) but the chart line is intent, not truth, until the next entry carries it.
// Closing that needs a decision on failure behaviour (does the line snap back when
// the venue refuses the replace?), so it is deliberately left out rather than
// half-wired.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broker::{BrokerOrder, BrokerPosition};

    fn pos(symbol: &str, qty: f64, entry: f64, mark: Option<f64>) -> BrokerPosition {
        BrokerPosition {
            symbol: symbol.into(),
            quantity: qty,
            avg_entry_price: entry,
            market_price: mark,
            market_value: None,
            cost_basis: None,
            unrealized_pnl: None,
            unrealized_pnl_pct: None,
        }
    }

    fn order(id: &str, symbol: &str, side: &str, kind: &str, parent: Option<&str>) -> BrokerOrder {
        BrokerOrder {
            id: id.into(),
            client_order_id: format!("cid-{id}"),
            symbol: symbol.into(),
            side: side.into(),
            order_type: kind.into(),
            time_in_force: "day".into(),
            quantity: 10.0,
            filled_quantity: 0.0,
            limit_price: Some(5.0),
            stop_price: None,
            filled_avg_price: None,
            status: BrokerOrderStatus::Open,
            raw_status: "new".into(),
            parent_id: parent.map(|p| p.to_string()),
            submitted_at: Some(Utc::now()),
            filled_at: None,
            canceled_at: None,
            reject_reason: None,
        }
    }

    fn mirror_with(positions: Vec<BrokerPosition>, orders: Vec<BrokerOrder>) -> BrokerMirror {
        let mut m = BrokerMirror::default();
        for p in positions {
            m.positions.insert(p.symbol.clone(), p);
        }
        for o in orders {
            m.orders.insert(o.id.clone(), o);
        }
        m
    }

    #[test]
    fn a_short_position_reads_as_short() {
        let m = mirror_with(vec![pos("AAPL", -30.0, 10.0, None)], vec![]);
        let out = positions(&m, &ChartState::new(), &BrokerJournal::default());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].side, Side::Short);
        assert_eq!(out[0].quantity, -30);
    }

    #[test]
    fn nothing_is_shown_that_the_venue_does_not_hold() {
        // The whole point: an empty venue book is an empty screen.
        let out = positions(
            &BrokerMirror::default(),
            &ChartState::new(),
            &BrokerJournal::default(),
        );
        assert!(out.is_empty());
    }

    #[test]
    fn a_flat_position_is_not_a_position() {
        let m = mirror_with(vec![pos("AAPL", 0.0, 10.0, None)], vec![]);
        assert!(positions(&m, &ChartState::new(), &BrokerJournal::default()).is_empty());
    }

    #[test]
    fn pnl_is_never_invented() {
        let m = mirror_with(vec![pos("AAPL", 10.0, 10.0, Some(12.0))], vec![]);
        let out = positions(&m, &ChartState::new(), &BrokerJournal::default());
        // The venue sent no unrealized P&L, so we report none rather than deriving
        // a number that could disagree with the account statement.
        assert_eq!(out[0].unrealized_pnl, None);
    }

    #[test]
    fn r_multiple_needs_a_real_stop_at_the_venue() {
        let p = pos("AAPL", 10.0, 10.0, Some(12.0));
        assert_eq!(r_multiple(&p, None), None, "no stop ⇒ no R");
        // Long from 10 with a stop at 9 (1 R of risk), now 12 ⇒ +2 R.
        assert_eq!(r_multiple(&p, Some(9.0)), Some(2.0));
    }

    #[test]
    fn a_short_measures_r_in_the_other_direction() {
        let p = pos("AAPL", -10.0, 10.0, Some(8.0));
        // Short from 10, stop at 11, now 8 ⇒ +2 R.
        assert_eq!(r_multiple(&p, Some(11.0)), Some(2.0));
    }

    #[test]
    fn orders_keep_the_venue_id_so_cancel_can_reach_them() {
        let m = mirror_with(vec![], vec![order("venue-1", "AAPL", "buy", "limit", None)]);
        let out = orders(&m);
        assert_eq!(out[0].order_id, "venue-1");
    }

    #[test]
    fn a_bracket_leg_is_reduce_only_an_entry_is_not() {
        let m = mirror_with(
            vec![],
            vec![
                order("entry", "AAPL", "buy", "limit", None),
                order("leg", "AAPL", "sell", "limit", Some("entry")),
            ],
        );
        let out = orders(&m);
        let leg = out.iter().find(|o| o.order_id == "leg").unwrap();
        let entry = out.iter().find(|o| o.order_id == "entry").unwrap();
        assert!(leg.reduce_only, "a nested leg only ever reduces");
        assert!(!entry.reduce_only, "a resting entry is not an exit");
    }

    #[test]
    fn terminal_orders_are_not_working_orders() {
        let mut filled = order("done", "AAPL", "buy", "limit", None);
        filled.status = BrokerOrderStatus::Filled;
        let m = mirror_with(vec![], vec![filled]);
        assert!(orders(&m).is_empty());
    }

    #[test]
    fn a_position_adopted_from_the_venue_still_gets_an_id() {
        // Opened from Alpaca's web UI: no journal entry, no chart context.
        let m = mirror_with(vec![pos("TSLA", 5.0, 100.0, None)], vec![]);
        let out = positions(&m, &ChartState::new(), &BrokerJournal::default());
        assert_eq!(out[0].trade_id, "alpaca-TSLA");
    }
}
