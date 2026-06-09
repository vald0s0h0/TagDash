// Internal trading engine. Stores orders, positions, and fills in RAM.
// No real broker order is ever sent in V1.

pub mod fills;
pub mod risk;

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use rand::Rng;

use crate::types::{
    Fill, InternalOrder, OrderStatus, OrderType, Position, RiskSizingResult,
    Side, Trade, TradeLifecycle,
};

// ─── ID helpers ──────────────────────────────────────────────────────────────

fn gen_id() -> String {
    let n: u64 = rand::thread_rng().gen();
    format!("{:016x}", n)
}

/// Max adverse / favorable excursion in dollars (positive magnitudes), given the
/// position side, average entry, share count, and the high/low price watermarks
/// observed while open. Returns `(mae, mfe)`.
fn excursions(side: Side, entry: f64, qty: f64, high: f64, low: f64) -> (f64, f64) {
    let up   = (high - entry).max(0.0) * qty; // gain if long / loss if short
    let down = (entry - low).max(0.0)  * qty; // loss if long / gain if short
    match side {
        Side::Long  => (down, up), // adverse = drop below entry, favorable = rise
        Side::Short => (up, down),
    }
}

// ─── InternalBook ─────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct InternalBook {
    pub trades:    HashMap<String, Trade>,
    pub orders:    HashMap<String, InternalOrder>,
    pub positions: HashMap<String, Position>, // keyed by symbol
    pub fills:     Vec<Fill>,
}

impl InternalBook {
    pub fn new() -> Self {
        Self::default()
    }

    // ── Risk sizing ───────────────────────────────────────────────────────────

    pub fn compute_risk_sizing(
        entry_price: f64,
        stop_loss:   f64,
        max_risk:    f64,
        min_size:    u32,
        max_size:    u32,
    ) -> RiskSizingResult {
        risk::compute(entry_price, stop_loss, max_risk, min_size, max_size)
    }

    // ── Create a pending limit order ──────────────────────────────────────────

    pub fn create_limit_order(
        &mut self,
        trade_id:    Option<String>,
        zone_id:     String,
        symbol:      String,
        strategy_id: String,
        side:        Side,
        quantity:    i64,
        limit_price: f64,
        stop_loss:   Option<f64>,
        take_profit: Option<f64>,
    ) -> InternalOrder {
        let order_id = gen_id();
        let now      = Utc::now();

        // OCO indicator: both SL and TP configured
        let oco_group = if stop_loss.is_some() && take_profit.is_some() {
            trade_id.clone().or_else(|| Some(gen_id()))
        } else {
            None
        };

        // Ensure a Trade record exists for this tradeID
        if let Some(ref tid) = trade_id {
            self.trades.entry(tid.clone()).or_insert_with(|| Trade {
                trade_id:    tid.clone(),
                symbol:      symbol.clone(),
                strategy_id: strategy_id.clone(),
                side:        Some(side),
                stop_loss,
                take_profit,
                opened_at:   None,
                closed_at:   None,
                entry_price: None,
                quantity:    None,
                notes:       None,
                confidence:  None,
                tags:        vec![],
                mae:         None,
                mfe:         None,
            });
        }

        let order = InternalOrder {
            order_id: order_id.clone(),
            trade_id,
            zone_id,
            symbol,
            side,
            order_type:  OrderType::Limit,
            limit_price: Some(limit_price),
            stop_price:  None,
            quantity,
            stop_loss,
            take_profit,
            status:      OrderStatus::Pending,
            oco_group,
            reduce_only: false,
            created_at:  now,
        };

        self.orders.insert(order_id, order.clone());
        order
    }

    // ── Execute a market fill immediately ─────────────────────────────────────

    pub fn execute_market_fill(
        &mut self,
        trade_id:    Option<String>,
        zone_id:     String,
        symbol:      String,
        strategy_id: String,
        side:        Side,
        quantity:    i64,
        fill_price:  f64,
        stop_loss:   Option<f64>,
        take_profit: Option<f64>,
    ) -> Fill {
        let order_id = gen_id();
        let fill_id  = gen_id();
        let now      = Utc::now();

        let effective_tid = trade_id.clone().unwrap_or_else(|| {
            format!(
                "{}-{}-{}",
                now.format("%y%m%d%H%M%S"),
                symbol.to_uppercase(),
                strategy_id.to_uppercase().replace('-', "_")
            )
        });

        // Ensure trade record exists
        let trade = self.trades.entry(effective_tid.clone()).or_insert_with(|| Trade {
            trade_id:    effective_tid.clone(),
            symbol:      symbol.clone(),
            strategy_id: strategy_id.clone(),
            side:        Some(side),
            stop_loss,
            take_profit,
            opened_at:   Some(now),
            closed_at:   None,
            entry_price: Some(fill_price),
            quantity:    Some(quantity),
            notes:       None,
            confidence:  None,
            tags:        vec![],
            mae:         None,
            mfe:         None,
        });
        if trade.opened_at.is_none() {
            trade.opened_at = Some(now);
        }
        if trade.entry_price.is_none() {
            trade.entry_price = Some(fill_price);
        }

        // Filled order record
        self.orders.insert(order_id.clone(), InternalOrder {
            order_id:    order_id.clone(),
            trade_id:    Some(effective_tid.clone()),
            zone_id:     zone_id.clone(),
            symbol:      symbol.clone(),
            side,
            order_type:  OrderType::Market,
            limit_price: None,
            stop_price:  None,
            quantity,
            stop_loss,
            take_profit,
            status:      OrderStatus::Filled,
            oco_group:   None,
            reduce_only: false,
            created_at:  now,
        });

        let fill = Fill {
            fill_id:    fill_id.clone(),
            order_id,
            trade_id:   effective_tid.clone(),
            symbol:     symbol.clone(),
            side,
            quantity,
            fill_price,
            filled_at:  now,
        };
        self.fills.push(fill.clone());

        // Apply to position
        self.apply_fill(
            &symbol, &effective_tid, &zone_id, &strategy_id,
            side, quantity, fill_price, stop_loss, take_profit, now,
        );

        fill
    }

    // ── Apply fill to position (shared between market + limit fill paths) ─────

    fn apply_fill(
        &mut self,
        symbol:      &str,
        trade_id:    &str,
        zone_id:     &str,
        strategy_id: &str,
        side:        Side,
        quantity:    i64,
        fill_price:  f64,
        stop_loss:   Option<f64>,
        take_profit: Option<f64>,
        now:         DateTime<Utc>,
    ) {
        // Signed qty: Long = +qty, Short = -qty
        let signed = match side {
            Side::Long  =>  quantity.abs(),
            Side::Short => -quantity.abs(),
        };

        if let Some(pos) = self.positions.get_mut(symbol) {
            // The fill price itself is a price the trade traded at — fold it into
            // the MAE/MFE watermarks (an exit fill, e.g. an SL hit, is often the
            // extreme).
            pos.high_water = pos.high_water.max(fill_price);
            pos.low_water  = pos.low_water.min(fill_price);

            let old_qty = pos.quantity;
            let new_qty = old_qty + signed;

            if new_qty == 0 {
                // Position gone flat — capture MAE/MFE then close the trade.
                let (mae, mfe) = excursions(
                    pos.side, pos.avg_entry_price, old_qty.unsigned_abs() as f64,
                    pos.high_water, pos.low_water,
                );
                if let Some(t) = self.trades.get_mut(trade_id) {
                    t.closed_at = Some(now);
                    t.mae = Some(mae);
                    t.mfe = Some(mfe);
                }
                self.positions.remove(symbol);
                return;
            }

            // Averaging in (same direction) → weighted avg entry
            if old_qty.signum() == new_qty.signum() {
                let old_cost = pos.avg_entry_price * old_qty.abs() as f64;
                let new_cost = fill_price * signed.unsigned_abs() as f64;
                pos.avg_entry_price = (old_cost + new_cost) / new_qty.abs() as f64;
            }
            pos.quantity = new_qty;
            pos.side     = if new_qty > 0 { Side::Long } else { Side::Short };
            if stop_loss.is_some()   { pos.stop_loss   = stop_loss; }
            if take_profit.is_some() { pos.take_profit = take_profit; }
        } else {
            // New position
            self.positions.insert(symbol.to_string(), Position {
                trade_id:        trade_id.to_string(),
                zone_id:         zone_id.to_string(),
                symbol:          symbol.to_string(),
                strategy_id:     strategy_id.to_string(),
                side:            if signed > 0 { Side::Long } else { Side::Short },
                quantity:        signed,
                avg_entry_price: fill_price,
                stop_loss,
                take_profit,
                unrealized_pnl:  Some(0.0),
                r_multiple:      None,
                opened_at:       now,
                high_water:      fill_price,
                low_water:       fill_price,
            });
        }
    }

    // ── Try to fill pending limit orders (called on each market poll) ─────────

    pub fn try_fill_pending(
        &mut self,
        prices: &HashMap<String, (f64, f64)>, // symbol -> (bid, ask)
    ) -> Vec<Fill> {
        let now = Utc::now();
        let mut to_fill: Vec<(String, InternalOrder, f64)> = vec![];

        for (oid, order) in &self.orders {
            if order.status != OrderStatus::Pending {
                continue;
            }
            let Some(&(bid, ask)) = prices.get(&order.symbol) else { continue };

            let hit = match order.order_type {
                // Limit: buy fills when ask ≤ limit; sell fills when bid ≥ limit.
                // Covers both entry limits and protective TP exits.
                OrderType::Limit => {
                    let Some(limit) = order.limit_price else { continue };
                    match order.side {
                        Side::Long  => ask <= limit,
                        Side::Short => bid >= limit,
                    }
                }
                // Protective stop (SL): triggers a market exit when price crosses
                // the stop. Sell stop (exits a long) → bid ≤ stop; buy stop
                // (exits a short) → ask ≥ stop.
                OrderType::Stop => {
                    let Some(stop) = order.stop_price else { continue };
                    match order.side {
                        Side::Short => bid <= stop,
                        Side::Long  => ask >= stop,
                    }
                }
                OrderType::Market => continue,
            };
            if hit {
                let fp = fills::price_for_side(order.side, bid, ask);
                to_fill.push((oid.clone(), order.clone(), fp));
            }
        }

        let mut new_fills = vec![];

        for (oid, order, fill_price) in to_fill {
            // Skip if an OCO sibling that filled earlier in this same batch
            // already cancelled this order (prevents a double exit / phantom
            // position when SL and TP both trigger in one poll).
            match self.orders.get(&oid) {
                Some(o) if o.status == OrderStatus::Pending => {}
                _ => continue,
            }

            // Mark order filled
            if let Some(o) = self.orders.get_mut(&oid) {
                o.status = OrderStatus::Filled;
            }

            // Cancel OCO sibling orders
            if let Some(ref grp) = order.oco_group.clone() {
                for o in self.orders.values_mut() {
                    if o.oco_group.as_deref() == Some(grp.as_str())
                        && o.order_id != oid
                        && o.status == OrderStatus::Pending
                    {
                        o.status = OrderStatus::Cancelled;
                    }
                }
            }

            let fill_id = gen_id();
            let effective_tid = order.trade_id.clone().unwrap_or_else(|| gen_id());

            let fill = Fill {
                fill_id,
                order_id:   oid.clone(),
                trade_id:   effective_tid.clone(),
                symbol:     order.symbol.clone(),
                side:       order.side,
                quantity:   order.quantity,
                fill_price,
                filled_at:  now,
            };
            self.fills.push(fill.clone());

            self.apply_fill(
                &order.symbol, &effective_tid, &order.zone_id,
                "", // strategy_id not stored on order; position already has it
                order.side, order.quantity, fill_price,
                order.stop_loss, order.take_profit, now,
            );

            new_fills.push(fill);
        }

        new_fills
    }

    // ── Cancel a pending order ────────────────────────────────────────────────

    pub fn cancel_order(&mut self, order_id: &str) -> Result<(), String> {
        match self.orders.get_mut(order_id) {
            Some(o) if o.status == OrderStatus::Pending => {
                o.status = OrderStatus::Cancelled;
                Ok(())
            }
            Some(_) => Err("order is not pending".into()),
            None    => Err("order not found".into()),
        }
    }

    // ── Protective bracket orders (SL = stop, TP = limit, OCO) ────────────────
    //
    // Materialises the SL/TP of the open position into real reduce-only exit
    // orders so they trigger fills when price touches them. Idempotent: cancels
    // any existing bracket for the symbol and re-creates it to match the current
    // position size and levels. Call after every entry fill or level change.
    pub fn sync_bracket_orders(&mut self, symbol: &str) {
        // Cancel existing pending bracket (reduce-only) orders for this symbol.
        for o in self.orders.values_mut() {
            if o.symbol == symbol && o.reduce_only && o.status == OrderStatus::Pending {
                o.status = OrderStatus::Cancelled;
            }
        }

        let Some(pos) = self.positions.get(symbol).cloned() else { return };
        let qty = pos.quantity.unsigned_abs() as i64;
        if qty == 0 { return; }

        // Exit side is opposite the position direction.
        let exit_side = match pos.side {
            Side::Long  => Side::Short,
            Side::Short => Side::Long,
        };
        let oco = format!("bracket-{}", pos.trade_id);
        let now = Utc::now();
        let trade_id = Some(pos.trade_id.clone());

        // SL → protective stop (market exit on trigger).
        if let Some(sl) = pos.stop_loss {
            let oid = gen_id();
            self.orders.insert(oid.clone(), InternalOrder {
                order_id:    oid,
                trade_id:    trade_id.clone(),
                zone_id:     pos.zone_id.clone(),
                symbol:      symbol.to_string(),
                side:        exit_side,
                order_type:  OrderType::Stop,
                limit_price: None,
                stop_price:  Some(sl),
                quantity:    qty,
                stop_loss:   None,
                take_profit: None,
                status:      OrderStatus::Pending,
                oco_group:   Some(oco.clone()),
                reduce_only: true,
                created_at:  now,
            });
        }

        // TP → limit exit.
        if let Some(tp) = pos.take_profit {
            let oid = gen_id();
            self.orders.insert(oid.clone(), InternalOrder {
                order_id:    oid,
                trade_id:    trade_id.clone(),
                zone_id:     pos.zone_id.clone(),
                symbol:      symbol.to_string(),
                side:        exit_side,
                order_type:  OrderType::Limit,
                limit_price: Some(tp),
                stop_price:  None,
                quantity:    qty,
                stop_loss:   None,
                take_profit: None,
                status:      OrderStatus::Pending,
                oco_group:   Some(oco),
                reduce_only: true,
                created_at:  now,
            });
        }
    }

    /// Update the SL/TP for a symbol after the chart lines move.
    ///
    /// Two cases, both handled so a TP/SL placed on the chart always lands on a
    /// live order:
    ///  • An open position → update its levels and re-arm the bracket orders.
    ///  • A resting (not-yet-filled) entry limit order → update the SL/TP it
    ///    carries, so the bracket is correct the moment it fills. Without this a
    ///    TP dragged while the entry is still pending would be silently dropped.
    pub fn update_protective_levels(
        &mut self,
        symbol: &str,
        stop_loss: Option<f64>,
        take_profit: Option<f64>,
    ) {
        // Propagate onto pending entry orders (not the reduce-only bracket exits,
        // which sync_bracket_orders owns and rebuilds from the position).
        for o in self.orders.values_mut() {
            if o.symbol == symbol
                && o.status == OrderStatus::Pending
                && !o.reduce_only
            {
                o.stop_loss   = stop_loss;
                o.take_profit = take_profit;
            }
        }

        if let Some(pos) = self.positions.get_mut(symbol) {
            pos.stop_loss   = stop_loss;
            pos.take_profit = take_profit;
            self.sync_bracket_orders(symbol);
        }
    }

    // ── Close open position at market (bid/ask unfavorable) ───────────────────

    pub fn close_position(
        &mut self,
        symbol:      &str,
        bid:         f64,
        ask:         f64,
        strategy_id: String,
        zone_id:     String,
    ) -> Option<Fill> {
        let pos = self.positions.get(symbol)?.clone();

        // Closing side is opposite of position side
        let closing_side = match pos.side {
            Side::Long  => Side::Short,
            Side::Short => Side::Long,
        };
        let fp  = fills::price_for_side(closing_side, bid, ask);
        let qty = pos.quantity.unsigned_abs() as i64;

        // Cancel all pending orders for this symbol
        for o in self.orders.values_mut() {
            if o.symbol == symbol && o.status == OrderStatus::Pending {
                o.status = OrderStatus::Cancelled;
            }
        }

        let fill = self.execute_market_fill(
            Some(pos.trade_id.clone()),
            zone_id,
            symbol.to_string(),
            strategy_id,
            closing_side,
            qty,
            fp,
            None,
            None,
        );

        Some(fill)
    }

    // ── Query: open positions with live PnL ───────────────────────────────────

    pub fn positions_with_pnl(
        &mut self,
        prices: &HashMap<String, (f64, f64)>, // symbol -> (bid, ask)
    ) -> Vec<Position> {
        for (symbol, pos) in self.positions.iter_mut() {
            if let Some(&(bid, ask)) = prices.get(symbol) {
                let mid = (bid + ask) / 2.0;
                // Track price extremes for MAE/MFE while the position is open.
                pos.high_water = pos.high_water.max(mid);
                pos.low_water  = pos.low_water.min(mid);
                pos.unrealized_pnl = Some(match pos.side {
                    Side::Long  => (mid - pos.avg_entry_price) * pos.quantity.abs() as f64,
                    Side::Short => (pos.avg_entry_price - mid) * pos.quantity.abs() as f64,
                });
                pos.r_multiple = pos.stop_loss.and_then(|sl| {
                    let risk = (pos.avg_entry_price - sl).abs();
                    if risk > 1e-8 {
                        let pnl_per_share = (mid - pos.avg_entry_price)
                            * (if pos.side == Side::Long { 1.0 } else { -1.0 });
                        Some(pnl_per_share / risk)
                    } else {
                        None
                    }
                });
            }
        }
        self.positions.values().cloned().collect()
    }

    // ── Query: pending orders ─────────────────────────────────────────────────

    pub fn get_pending_orders(&self) -> Vec<InternalOrder> {
        self.orders
            .values()
            .filter(|o| o.status == OrderStatus::Pending)
            .cloned()
            .collect()
    }

    // ── Query: full trade lifecycle ───────────────────────────────────────────

    pub fn get_trade_lifecycle(&self, trade_id: &str) -> Option<TradeLifecycle> {
        let trade = self.trades.get(trade_id)?.clone();
        let orders = self.orders.values()
            .filter(|o| o.trade_id.as_deref() == Some(trade_id))
            .cloned()
            .collect();
        let fills = self.fills.iter()
            .filter(|f| f.trade_id == trade_id)
            .cloned()
            .collect();
        let position = self.positions.values()
            .find(|p| p.trade_id == trade_id)
            .cloned();
        Some(TradeLifecycle { trade, orders, fills, position })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prices(sym: &str, bid: f64, ask: f64) -> HashMap<String, (f64, f64)> {
        let mut m = HashMap::new();
        m.insert(sym.to_string(), (bid, ask));
        m
    }

    // TP placed AFTER a market entry (the "drag the TP line while in a position"
    // flow) must materialise a pending limit exit and fill it when crossed.
    #[test]
    fn tp_placed_after_entry_fills_on_cross() {
        let mut book = InternalBook::new();

        // Enter long 100 @ 10.00, no TP yet.
        book.execute_market_fill(
            Some("T1".into()), "z1".into(), "AAA".into(), "strat".into(),
            Side::Long, 100, 10.00, Some(9.50), None,
        );
        book.sync_bracket_orders("AAA");

        // User drags a TP line at 11.00 → update protective levels.
        book.update_protective_levels("AAA", Some(9.50), Some(11.00));

        // A pending TP limit exit must now exist.
        let tp_orders: Vec<_> = book.get_pending_orders().into_iter()
            .filter(|o| o.order_type == OrderType::Limit && o.reduce_only)
            .collect();
        assert_eq!(tp_orders.len(), 1, "expected one resting TP limit order");
        assert_eq!(tp_orders[0].limit_price, Some(11.00));

        // Price crosses the TP (bid reaches 11.00) → must fill and flatten.
        let fills = book.try_fill_pending(&prices("AAA", 11.00, 11.02));
        assert_eq!(fills.len(), 1, "TP should fill when bid >= limit");
        assert!(book.positions.get("AAA").is_none(), "position should be flat after TP");
    }

    // Pending LIMIT entry + dragging the TP line afterwards: the resting entry
    // order should reflect the latest TP so the bracket is right once it fills.
    #[test]
    fn tp_dragged_while_entry_pending_updates_entry() {
        let mut book = InternalBook::new();

        // Resting limit entry: long 100 @ 10.00, SL 9.50, no TP yet.
        book.create_limit_order(
            Some("T2".into()), "z1".into(), "AAA".into(), "strat".into(),
            Side::Long, 100, 10.00, Some(9.50), None,
        );

        // User drags a TP line at 11.00 *before* the entry fills.
        book.update_protective_levels("AAA", Some(9.50), Some(11.00));

        // Entry fills.
        let entry = book.try_fill_pending(&prices("AAA", 9.99, 10.00));
        assert_eq!(entry.len(), 1, "entry limit should fill");
        book.sync_bracket_orders("AAA");

        // The TP bracket must exist at the dragged level.
        let tp_orders: Vec<_> = book.get_pending_orders().into_iter()
            .filter(|o| o.order_type == OrderType::Limit && o.reduce_only)
            .collect();
        assert_eq!(tp_orders.len(), 1, "expected resting TP after entry fill");
        assert_eq!(tp_orders[0].limit_price, Some(11.00), "TP must reflect dragged level");
    }

    // A long that runs up then comes back to be stopped should record both the
    // favorable run-up (MFE) and the adverse drawdown (MAE) in dollars.
    #[test]
    fn mae_mfe_captured_on_close() {
        let mut book = InternalBook::new();

        // Long 100 @ 10.00, SL 9.50.
        book.execute_market_fill(
            Some("T3".into()), "z1".into(), "AAA".into(), "strat".into(),
            Side::Long, 100, 10.00, Some(9.50), None,
        );
        book.sync_bracket_orders("AAA");

        // Price runs up to 11.00 (favorable) then down to 9.40 (adverse) → polls
        // move the watermarks.
        book.positions_with_pnl(&prices("AAA", 10.99, 11.01)); // mid 11.00
        book.positions_with_pnl(&prices("AAA", 9.39, 9.41));   // mid 9.40 → SL hit

        // SL fills (bid 9.39 <= 9.50) and flattens the position.
        let fills = book.try_fill_pending(&prices("AAA", 9.39, 9.41));
        assert_eq!(fills.len(), 1, "SL should fill");

        let trade = book.trades.get("T3").unwrap();
        assert!(trade.closed_at.is_some());
        // MFE = (11.00 - 10.00) * 100 = 100.0
        assert!((trade.mfe.unwrap() - 100.0).abs() < 1e-6, "mfe={:?}", trade.mfe);
        // MAE = (10.00 - 9.39) * 100 = 61.0 (SL exit fill 9.39 is the low)
        assert!((trade.mae.unwrap() - 61.0).abs() < 1e-6, "mae={:?}", trade.mae);
    }
}
