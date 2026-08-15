// Trade journal for the Alpaca broker → TradeTally.
//
// WHY THIS EXISTS. The TradeTally pipeline is driven by `commands::sync_fill`, which
// reads the INTERNAL book. With Alpaca selected that book is never written (the
// synthetic-fill loop stands down, by design), so without this module a real trade
// would reach TradeTally with nothing in it. This is the missing half: it turns the
// venue's own executions into the same three TradeTally events the internal broker
// emits — `trade_created`, `fill_added`, `trade_closed` — with every number taken
// from Alpaca rather than computed locally.
//
// WHAT COMES FROM THE SERVER, and nothing else:
//   • execution price  — the `trade_updates` fill event's `price`, the actual print;
//   • execution size   — its `qty`, so a PARTIAL fill is journalled as the partial it
//                        was, and a scale-in appears as several executions;
//   • position size    — its `position_qty`, the venue's resulting position, which is
//                        what decides open / add / reduce / flat;
//   • average entry    — Alpaca's own `avg_entry_price` on the position;
//   • SL and TP        — read off the live bracket LEGS held at the venue, so the
//                        journal records the protection the market actually had.
//
// WHAT IS COMPUTED HERE, because no venue reports it: MAE and MFE. They are price
// excursions relative to entry while the position is open, so they need the price
// path, not the fills. `on_price` is fed by the trading loop at its normal 500 ms
// cadence and the numbers go through `internal_trading::excursions` — the exact
// function the internal broker uses, so a trade's MAE/MFE means the same thing
// whichever broker filled it.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use super::BrokerMirror;
use crate::config::AppConfig;
use crate::internal_trading::excursions;
use crate::tradetally;
use crate::types::{Fill, FillWindow, Side};

/// One execution as the venue reported it.
#[derive(Debug, Clone)]
pub struct FillEvent {
    pub symbol: String,
    pub order_id: String,
    pub client_order_id: String,
    /// Direction of THIS execution ("buy" ⇒ Long).
    pub side: Side,
    /// The printed price — never the limit we asked for.
    pub price: f64,
    /// Shares in THIS execution. A partial fill reports only its own slice.
    pub qty: f64,
    /// The venue's resulting position size after this execution, signed.
    pub position_qty: f64,
    pub at: DateTime<Utc>,
    /// Protective levels carried by the submitted bracket or the durable
    /// extended-hours plan. The venue mirror still wins when its legs are visible.
    pub stop_loss: Option<f64>,
    pub take_profit: Option<f64>,
}

/// An open trade being accumulated for the journal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrokerTrade {
    pub trade_id: String,
    pub symbol: String,
    pub side: Side,
    pub strategy_name: String,
    pub opened_at: DateTime<Utc>,
    /// Every execution, in order — this is what becomes TradeTally's `executions`.
    pub fills: Vec<Fill>,
    /// Signed running position, as last reported by the venue.
    pub position_qty: f64,
    pub avg_entry_price: f64,
    pub stop_loss: Option<f64>,
    pub take_profit: Option<f64>,
    /// Price watermarks since entry, for MAE/MFE.
    pub high_water: f64,
    pub low_water: f64,
}

impl BrokerTrade {
    /// Total shares that entered the position (entry-side executions only).
    fn entry_quantity(&self) -> i64 {
        self.fills
            .iter()
            .filter(|f| f.side == self.side)
            .map(|f| f.quantity.abs())
            .sum()
    }

    /// MAE / MFE in dollars, from the watermarks observed while open.
    fn excursions(&self) -> (Option<f64>, Option<f64>) {
        if self.high_water <= 0.0 || self.low_water <= 0.0 {
            return (None, None); // never saw a price — report nothing rather than 0
        }
        let qty = self.entry_quantity() as f64;
        let (mae, mfe) = excursions(
            self.side,
            self.avg_entry_price,
            qty,
            self.high_water,
            self.low_water,
        );
        (Some(mae), Some(mfe))
    }
}

/// Open trades, keyed by symbol.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BrokerJournal {
    pub open: HashMap<String, BrokerTrade>,
}

/// Read the protective levels the VENUE is actually holding for `symbol`.
///
/// A bracket's legs live at the venue as ordinary orders, flattened into the mirror
/// with a `parent_id`. The live stop leg's trigger is the real stop-loss and the live
/// limit leg's price is the real target — which is the point: the journal records the
/// protection the position had, not what the chart happened to draw.
pub fn levels_from_mirror(mirror: &BrokerMirror, symbol: &str) -> (Option<f64>, Option<f64>) {
    let mut sl = None;
    let mut tp = None;
    for o in mirror.orders.values() {
        if o.symbol != symbol || !o.status.is_live() {
            continue;
        }
        match o.order_type.as_str() {
            "stop" | "stop_limit" | "trailing_stop" => sl = sl.or(o.stop_price),
            // Only an EXIT limit is a target; the resting entry is a limit too. A
            // bracket leg always carries a parent, which is what distinguishes them
            // without guessing from the price.
            "limit" if o.parent_id.is_some() => tp = tp.or(o.limit_price),
            _ => {}
        }
    }
    (sl, tp)
}

impl BrokerJournal {
    /// Fold one venue execution into the journal and emit the matching TradeTally
    /// event. Returns the trade id the execution belongs to.
    ///
    /// The open/add/close decision is made from the venue's `position_qty`, never
    /// from a local tally: it is the only number that stays right through a partial
    /// fill, a manual order placed in Alpaca's own web UI, or a restart.
    #[allow(clippy::too_many_arguments)]
    pub fn on_fill(
        &mut self,
        ev: &FillEvent,
        mirror: &BrokerMirror,
        conn: &Connection,
        cfg: &AppConfig,
        strategy_name: &str,
        account_id: &str,
    ) -> String {
        let (venue_sl, venue_tp) = levels_from_mirror(mirror, &ev.symbol);
        let sl = venue_sl.or(ev.stop_loss);
        let tp = venue_tp.or(ev.take_profit);
        // The venue's own average entry, when it has a position; on the closing fill
        // there is none left, so the trade keeps the average it accumulated.
        let venue_avg = mirror.positions.get(&ev.symbol).map(|p| p.avg_entry_price);

        let is_new = !self.open.contains_key(&ev.symbol);
        if is_new {
            // A fresh trade. Its id is the venue's client_order_id when we have one —
            // that is the key WE minted at submit time, so the journal, the chart
            // context and Alpaca all name the same trade.
            let trade_id = if ev.client_order_id.is_empty() {
                format!("alpaca-{}", ev.order_id)
            } else {
                ev.client_order_id.clone()
            };
            self.open.insert(
                ev.symbol.clone(),
                BrokerTrade {
                    trade_id,
                    symbol: ev.symbol.clone(),
                    side: if ev.position_qty < 0.0 {
                        Side::Short
                    } else {
                        Side::Long
                    },
                    strategy_name: strategy_name.to_string(),
                    opened_at: ev.at,
                    fills: Vec::new(),
                    position_qty: 0.0,
                    avg_entry_price: ev.price,
                    stop_loss: sl,
                    take_profit: tp,
                    high_water: ev.price,
                    low_water: ev.price,
                },
            );
        }

        let trade = self.open.get_mut(&ev.symbol).expect("just inserted");
        trade.fills.push(Fill {
            fill_id: format!("{}-{}", ev.order_id, trade.fills.len()),
            order_id: ev.order_id.clone(),
            trade_id: trade.trade_id.clone(),
            symbol: ev.symbol.clone(),
            side: ev.side,
            quantity: ev.qty.abs().round() as i64,
            fill_price: ev.price,
            filled_at: ev.at,
        });
        trade.position_qty = ev.position_qty;
        if let Some(avg) = venue_avg {
            trade.avg_entry_price = avg;
        }
        trade.stop_loss = trade.stop_loss.or(sl);
        trade.take_profit = tp.or(trade.take_profit);
        trade.high_water = trade.high_water.max(ev.price);
        trade.low_water = if trade.low_water <= 0.0 {
            ev.price
        } else {
            trade.low_water.min(ev.price)
        };

        let trade_id = trade.trade_id.clone();
        let flat = ev.position_qty.abs() < 1e-9;

        if is_new {
            let t = &*trade;
            tradetally::enqueue_trade_created(
                conn,
                &t.trade_id,
                &t.symbol,
                &t.strategy_name,
                t.side,
                t.avg_entry_price,
                ev.position_qty.abs().round() as i64,
                &t.opened_at.to_rfc3339(),
                t.stop_loss,
                t.take_profit,
                &t.fills,
                cfg,
            );
        }

        if flat {
            // Closing execution: hand TradeTally the venue's exit price and size plus
            // the excursions measured while the position lived.
            let t = self.open.remove(&ev.symbol).expect("present");
            let (mae, mfe) = t.excursions();
            tradetally::enqueue_trade_closed(
                conn,
                &t.trade_id,
                &t.symbol,
                ev.price,
                &ev.at.to_rfc3339(),
                ev.qty.abs().round() as i64,
                mae,
                mfe,
                &t.fills,
                cfg,
            );
        } else if !is_new {
            let t = &*trade;
            tradetally::enqueue_fill_added(
                conn,
                &t.trade_id,
                &t.symbol,
                t.avg_entry_price,
                ev.position_qty.abs().round() as i64,
                &t.fills,
                cfg,
            );
        }

        if let Err(e) =
            crate::local_db::broker_state_repository::save_journal(conn, account_id, self)
        {
            eprintln!("[tagdash] persistance journal Alpaca: {e}");
        }

        trade_id
    }

    /// Advance the MAE/MFE watermarks from the price path.
    ///
    /// Fed by the trading loop's own 500 ms window, so an excursion that spiked and
    /// retraced between two polls is still recorded — the same resolution the
    /// internal broker gets.
    pub fn on_price(&mut self, symbol: &str, w: &FillWindow) {
        if let Some(t) = self.open.get_mut(symbol) {
            t.high_water = t.high_water.max(w.high);
            t.low_water = if t.low_water <= 0.0 {
                w.low
            } else {
                t.low_water.min(w.low)
            };
        }
    }

    /// Drop trades the venue no longer has a position for.
    ///
    /// Safety net for an exit that happened where we could not see it — an order
    /// filled while the app was closed, or a position flattened from Alpaca's own
    /// web UI. Without it the journal would keep an open trade forever and never
    /// close it in TradeTally.
    pub fn orphaned(&self, mirror: &BrokerMirror) -> Vec<BrokerTrade> {
        self.open
            .values()
            .filter(|t| !mirror.positions.contains_key(&t.symbol))
            .cloned()
            .collect()
    }

    /// Close a trade from venue activity history after an offline/manual exit.
    /// The journal is removed only after a real opposite-side execution was found
    /// and the TradeTally close event was durably queued.
    pub fn recover_closed(
        &mut self,
        symbol: &str,
        activities: &[super::alpaca_rest::FillActivity],
        conn: &Connection,
        cfg: &AppConfig,
        account_id: &str,
    ) -> bool {
        let Some(mut trade) = self.open.remove(symbol) else {
            return false;
        };
        let exits: Vec<_> = activities
            .iter()
            .filter(|a| {
                a.symbol.eq_ignore_ascii_case(symbol)
                    && a.side != trade.side
                    && a.at >= trade.opened_at
            })
            .collect();
        if exits.is_empty() {
            self.open.insert(symbol.to_string(), trade);
            return false;
        }
        let total: f64 = exits.iter().map(|a| a.qty.abs()).sum();
        let notional: f64 = exits.iter().map(|a| a.qty.abs() * a.price).sum();
        for a in &exits {
            trade.fills.push(Fill {
                fill_id: a.id.clone(),
                order_id: a.order_id.clone(),
                trade_id: trade.trade_id.clone(),
                symbol: trade.symbol.clone(),
                side: a.side,
                quantity: a.qty.abs().round() as i64,
                fill_price: a.price,
                filled_at: a.at,
            });
        }
        let Some(last) = exits.last() else {
            return false;
        };
        let exit_price = if total > 0.0 {
            notional / total
        } else {
            last.price
        };
        let (mae, mfe) = trade.excursions();
        tradetally::enqueue_trade_closed(
            conn,
            &trade.trade_id,
            &trade.symbol,
            exit_price,
            &last.at.to_rfc3339(),
            total.round() as i64,
            mae,
            mfe,
            &trade.fills,
            cfg,
        );
        if let Err(e) =
            crate::local_db::broker_state_repository::save_journal(conn, account_id, self)
        {
            eprintln!("[tagdash] persistance journal Alpaca après reprise: {e}");
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broker::{BrokerOrder, BrokerOrderStatus, BrokerPosition};

    fn cfg() -> AppConfig {
        AppConfig::default()
    }

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::local_db::schema::migrate(&conn).unwrap();
        conn
    }

    fn ev(symbol: &str, side: Side, price: f64, qty: f64, position_qty: f64) -> FillEvent {
        FillEvent {
            symbol: symbol.into(),
            order_id: "o-1".into(),
            client_order_id: "tagdash-abc".into(),
            side,
            price,
            qty,
            position_qty,
            at: Utc::now(),
            stop_loss: None,
            take_profit: None,
        }
    }

    fn leg(symbol: &str, kind: &str, stop: Option<f64>, limit: Option<f64>) -> BrokerOrder {
        BrokerOrder {
            id: format!("{kind}-1"),
            client_order_id: "c".into(),
            symbol: symbol.into(),
            side: "sell".into(),
            order_type: kind.into(),
            time_in_force: "day".into(),
            quantity: 10.0,
            filled_quantity: 0.0,
            limit_price: limit,
            stop_price: stop,
            filled_avg_price: None,
            status: BrokerOrderStatus::Open,
            raw_status: "new".into(),
            parent_id: Some("parent".into()),
            submitted_at: None,
            filled_at: None,
            canceled_at: None,
            reject_reason: None,
        }
    }

    fn mirror_with(symbol: &str, qty: f64, avg: f64, legs: Vec<BrokerOrder>) -> BrokerMirror {
        let mut m = BrokerMirror::default();
        if qty != 0.0 {
            m.positions.insert(
                symbol.into(),
                BrokerPosition {
                    symbol: symbol.into(),
                    quantity: qty,
                    avg_entry_price: avg,
                    market_price: None,
                    market_value: None,
                    cost_basis: None,
                    unrealized_pnl: None,
                    unrealized_pnl_pct: None,
                },
            );
        }
        for l in legs {
            m.orders.insert(l.id.clone(), l);
        }
        m
    }

    fn queued(conn: &Connection) -> Vec<(String, String)> {
        let mut stmt = conn
            .prepare("SELECT event_type, payload FROM tradetally_sync_queue ORDER BY rowid")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect()
    }

    /// SL and TP must come off the venue's live bracket legs, not from anything local.
    #[test]
    fn protective_levels_are_read_from_the_venue_legs() {
        let m = mirror_with(
            "AAPL",
            10.0,
            210.0,
            vec![
                leg("AAPL", "stop", Some(199.5), None),
                leg("AAPL", "limit", None, Some(225.0)),
            ],
        );
        assert_eq!(levels_from_mirror(&m, "AAPL"), (Some(199.5), Some(225.0)));
    }

    /// A resting ENTRY limit carries no parent; treating it as a target would journal
    /// a take-profit that never existed.
    #[test]
    fn a_parentless_limit_order_is_not_a_take_profit() {
        let mut entry = leg("AAPL", "limit", None, Some(205.0));
        entry.parent_id = None;
        let m = mirror_with("AAPL", 0.0, 0.0, vec![entry]);
        assert_eq!(levels_from_mirror(&m, "AAPL"), (None, None));
    }

    /// The core path: open → partial add → close, all driven by the venue's numbers.
    #[test]
    fn a_partial_fill_sequence_journals_every_execution() {
        let conn = db();
        let mut j = BrokerJournal::default();
        let legs = vec![
            leg("AAPL", "stop", Some(199.5), None),
            leg("AAPL", "limit", None, Some(225.0)),
        ];

        // First partial: 6 of 10 shares at 210.10.
        let m = mirror_with("AAPL", 6.0, 210.10, legs.clone());
        j.on_fill(
            &ev("AAPL", Side::Long, 210.10, 6.0, 6.0),
            &m,
            &conn,
            &cfg(),
            "PMR",
            "alpaca:paper",
        );
        // Second partial: the remaining 4 at 210.50 → venue avg 210.26.
        let m = mirror_with("AAPL", 10.0, 210.26, legs.clone());
        j.on_fill(
            &ev("AAPL", Side::Long, 210.50, 4.0, 10.0),
            &m,
            &conn,
            &cfg(),
            "PMR",
            "alpaca:paper",
        );

        let t = j.open.get("AAPL").expect("still open");
        assert_eq!(t.fills.len(), 2, "each partial is its own execution");
        assert_eq!(t.position_qty, 10.0);
        assert_eq!(
            t.avg_entry_price, 210.26,
            "the VENUE's average, not a local mean"
        );
        assert_eq!(t.stop_loss, Some(199.5));
        assert_eq!(t.take_profit, Some(225.0));

        let q = queued(&conn);
        assert_eq!(q[0].0, "trade_created");
        assert_eq!(q[1].0, "fill_added");
        // The created payload carries the server SL/TP and the executions.
        let created: serde_json::Value = serde_json::from_str(&q[0].1).unwrap();
        assert_eq!(created["stopLoss"], 199.5);
        assert_eq!(created["takeProfit"], 225.0);
        assert_eq!(created["symbol"], "AAPL");
    }

    /// Going flat closes the trade with the venue's exit price and the excursions.
    #[test]
    fn going_flat_closes_the_trade_with_mae_and_mfe() {
        let conn = db();
        let mut j = BrokerJournal::default();
        let m = mirror_with("AAPL", 10.0, 210.0, vec![]);
        j.on_fill(
            &ev("AAPL", Side::Long, 210.0, 10.0, 10.0),
            &m,
            &conn,
            &cfg(),
            "PMR",
            "alpaca:paper",
        );

        // Price path while open: down to 205, up to 220.
        j.on_price(
            "AAPL",
            &FillWindow {
                first: 210.0,
                high: 220.0,
                low: 205.0,
                last: 218.0,
                low_first: true,
            },
        );

        // Exit at 218, position back to zero.
        let flat = mirror_with("AAPL", 0.0, 0.0, vec![]);
        j.on_fill(
            &ev("AAPL", Side::Short, 218.0, 10.0, 0.0),
            &flat,
            &conn,
            &cfg(),
            "PMR",
            "alpaca:paper",
        );

        assert!(!j.open.contains_key("AAPL"), "flat ⇒ no longer open");
        let q = queued(&conn);
        let closed = q
            .iter()
            .find(|(t, _)| t == "trade_closed")
            .expect("closed event");
        let p: serde_json::Value = serde_json::from_str(&closed.1).unwrap();
        assert_eq!(p["exitPrice"], 218.0);
        // Long from 210, low 205 ⇒ MAE 5×10 = 50 ; high 220 ⇒ MFE 10×10 = 100.
        assert_eq!(p["mae"], 50.0);
        assert_eq!(p["mfe"], 100.0);
        assert_eq!(p["executions"].as_array().unwrap().len(), 2);
    }

    /// A short's excursions are mirrored: the adverse side is UP.
    #[test]
    fn a_short_measures_its_excursions_the_other_way_round() {
        let conn = db();
        let mut j = BrokerJournal::default();
        let m = mirror_with("TSLA", -10.0, 300.0, vec![]);
        j.on_fill(
            &ev("TSLA", Side::Short, 300.0, 10.0, -10.0),
            &m,
            &conn,
            &cfg(),
            "PMR",
            "alpaca:paper",
        );
        assert_eq!(
            j.open["TSLA"].side,
            Side::Short,
            "signed position sets the side"
        );

        j.on_price(
            "TSLA",
            &FillWindow {
                first: 300.0,
                high: 310.0,
                low: 280.0,
                last: 285.0,
                low_first: false,
            },
        );

        let flat = mirror_with("TSLA", 0.0, 0.0, vec![]);
        j.on_fill(
            &ev("TSLA", Side::Long, 285.0, 10.0, 0.0),
            &flat,
            &conn,
            &cfg(),
            "PMR",
            "alpaca:paper",
        );

        let q = queued(&conn);
        let closed = q.iter().find(|(t, _)| t == "trade_closed").unwrap();
        let p: serde_json::Value = serde_json::from_str(&closed.1).unwrap();
        assert_eq!(p["mae"], 100.0, "short: adverse is the 310 high");
        assert_eq!(p["mfe"], 200.0, "short: favourable is the 280 low");
    }

    /// A position that disappears without a fill event we saw — flattened from
    /// Alpaca's web UI, or filled while the app was closed — must not leave an open
    /// trade dangling forever.
    #[test]
    fn a_position_that_vanishes_waits_for_authoritative_exit_history() {
        let conn = db();
        let mut j = BrokerJournal::default();
        let m = mirror_with("AAPL", 10.0, 210.0, vec![]);
        j.on_fill(
            &ev("AAPL", Side::Long, 210.0, 10.0, 10.0),
            &m,
            &conn,
            &cfg(),
            "PMR",
            "alpaca:paper",
        );

        let orphaned = j.orphaned(&BrokerMirror::default());
        assert_eq!(orphaned.len(), 1);
        assert_eq!(orphaned[0].symbol, "AAPL");
        assert!(
            !j.open.is_empty(),
            "orphan stays durable until a real exit fill is recovered"
        );
    }

    #[test]
    fn an_offline_exit_is_closed_from_alpaca_activity_and_persisted() {
        let conn = db();
        let mut j = BrokerJournal::default();
        let m = mirror_with("AAPL", 10.0, 210.0, vec![]);
        j.on_fill(
            &ev("AAPL", Side::Long, 210.0, 10.0, 10.0),
            &m,
            &conn,
            &cfg(),
            "PMR",
            "alpaca:paper",
        );
        let at = Utc::now() + chrono::Duration::minutes(5);
        let activity = super::super::alpaca_rest::FillActivity {
            id: "activity-1".into(),
            price: 205.0,
            qty: 10.0,
            side: Side::Short,
            symbol: "AAPL".into(),
            at,
            order_id: "exit-1".into(),
        };
        assert!(j.recover_closed("AAPL", &[activity], &conn, &cfg(), "alpaca:paper",));
        assert!(j.open.is_empty());
        assert!(queued(&conn).iter().any(|(kind, _)| kind == "trade_closed"));
        let restored: BrokerJournal =
            crate::local_db::broker_state_repository::load_journal(&conn, "alpaca:paper")
                .unwrap()
                .unwrap();
        assert!(restored.open.is_empty());
    }

    /// The trade id is the client_order_id we minted, so TagDash, the venue and the
    /// journal all name the same trade.
    #[test]
    fn the_trade_id_is_our_own_idempotency_key() {
        let conn = db();
        let mut j = BrokerJournal::default();
        let m = mirror_with("AAPL", 5.0, 100.0, vec![]);
        j.on_fill(
            &ev("AAPL", Side::Long, 100.0, 5.0, 5.0),
            &m,
            &conn,
            &cfg(),
            "PMR",
            "alpaca:paper",
        );
        assert_eq!(j.open["AAPL"].trade_id, "tagdash-abc");

        // With no client id, fall back to the venue's order id — never a random one,
        // which would break the link on a restart.
        let mut e = ev("MSFT", Side::Long, 100.0, 5.0, 5.0);
        e.client_order_id = String::new();
        e.order_id = "venue-77".into();
        let m2 = mirror_with("MSFT", 5.0, 100.0, vec![]);
        j.on_fill(&e, &m2, &conn, &cfg(), "PMR", "alpaca:paper");
        assert_eq!(j.open["MSFT"].trade_id, "alpaca-venue-77");
    }

    /// No price was ever observed ⇒ report nothing rather than a fake zero.
    #[test]
    fn excursions_are_absent_when_no_price_was_seen() {
        let t = BrokerTrade {
            trade_id: "t".into(),
            symbol: "A".into(),
            side: Side::Long,
            strategy_name: "s".into(),
            opened_at: Utc::now(),
            fills: vec![],
            position_qty: 1.0,
            avg_entry_price: 10.0,
            stop_loss: None,
            take_profit: None,
            high_water: 0.0,
            low_water: 0.0,
        };
        assert_eq!(t.excursions(), (None, None));
    }
}
