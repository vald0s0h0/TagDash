//! Durable extended-hours protection for Alpaca equity positions.
//!
//! Alpaca accepts extended-hours equity orders only as plain limit day/GTC orders;
//! bracket/OTO/OCO classes are regular-session only. Therefore:
//!   1. persist a TP/SL plan before sending the extended-hours entry;
//!   2. while extended hours are open, detect level crossings from the same ordered
//!      price windows as the simulator and submit one idempotent marketable limit;
//!   3. at 09:30 ET, hand an open position to a venue-owned GTC OCO.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use chrono::{DateTime, Utc};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use super::{alpaca_rest, BrokerEngine, OrderRequest};
use crate::config::{secrets::Secrets, AppConfig};
use crate::local_db::broker_state_repository;
use crate::types::{FillWindow, OrderType, Side};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtectionStatus {
    EntryPending,
    LocalActive,
    ExitPending,
    VenueOco,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtectionPlan {
    pub trade_id: String,
    pub symbol: String,
    pub side: Side,
    pub requested_quantity: i64,
    pub stop_loss: Option<f64>,
    pub take_profit: Option<f64>,
    pub entry_client_order_id: String,
    pub exit_client_order_id: String,
    pub oco_client_order_id: String,
    pub status: ProtectionStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProtectionBook {
    pub account_id: String,
    pub plans: HashMap<String, ProtectionPlan>,
}

fn persist(book: &ProtectionBook, conn: &Connection) -> Result<(), String> {
    broker_state_repository::save_protection(conn, &book.account_id, book)
        .map_err(|e| format!("persistance protection Alpaca: {e}"))
}

pub fn load(book: &mut ProtectionBook, conn: &Connection, account_id: &str) {
    *book = broker_state_repository::load_protection(conn, account_id)
        .ok()
        .flatten()
        .unwrap_or_else(|| ProtectionBook {
            account_id: account_id.into(),
            plans: HashMap::new(),
        });
    // Old/corrupt snapshots must never bleed from one account into another.
    book.account_id = account_id.to_string();
}

#[allow(clippy::too_many_arguments)]
pub fn register(
    book: &mut ProtectionBook,
    conn: &Connection,
    account_id: &str,
    trade_id: &str,
    symbol: &str,
    side: Side,
    quantity: i64,
    stop_loss: Option<f64>,
    take_profit: Option<f64>,
    entry_client_order_id: &str,
) -> Result<(), String> {
    let sym = symbol.trim().to_uppercase();
    if book.account_id != account_id {
        load(book, conn, account_id);
    }
    if book.plans.contains_key(&sym) {
        return Err(format!(
            "{sym} possède déjà un plan de protection Alpaca actif"
        ));
    }
    if stop_loss.is_none() || take_profit.is_none() {
        return Err(
            "extended hours exige un SL et un TP pour activer la protection persistée".into(),
        );
    }
    let now = Utc::now();
    book.plans.insert(
        sym.clone(),
        ProtectionPlan {
            trade_id: trade_id.into(),
            symbol: sym,
            side,
            requested_quantity: quantity,
            stop_loss,
            take_profit,
            entry_client_order_id: entry_client_order_id.into(),
            exit_client_order_id: alpaca_rest::prefixed_client_order_id("tagdash-xh-exit"),
            oco_client_order_id: alpaca_rest::prefixed_client_order_id("tagdash-rth-oco"),
            status: ProtectionStatus::EntryPending,
            created_at: now,
            updated_at: now,
        },
    );
    persist(book, conn)
}

pub fn unregister(book: &mut ProtectionBook, conn: &Connection, symbol: &str) {
    book.plans.remove(&symbol.trim().to_uppercase());
    let _ = persist(book, conn);
}

fn trigger_rank(plan: &ProtectionPlan, w: &FillWindow) -> Option<u8> {
    let mut ranks = Vec::with_capacity(2);
    if let Some(sl) = plan.stop_loss {
        let (hit, touch_low) = match plan.side {
            Side::Long => (w.low <= sl, true),
            Side::Short => (w.high >= sl, false),
        };
        if hit {
            ranks.push(u8::from(touch_low != w.low_first));
        }
    }
    if let Some(tp) = plan.take_profit {
        let (hit, touch_low) = match plan.side {
            Side::Long => (w.high >= tp, false),
            Side::Short => (w.low <= tp, true),
        };
        if hit {
            ranks.push(u8::from(touch_low != w.low_first));
        }
    }
    ranks.into_iter().min()
}

/// Submit at a limit beyond the latest print. The cap still prevents an unbounded
/// market-order fill; the 5% collar is intentionally explicit and deterministic.
fn marketable_exit_limit(side: Side, last: f64) -> f64 {
    match side {
        Side::Long => (last * 0.95).max(0.0001), // long exit is a sell limit
        Side::Short => last * 1.05,              // short exit is a buy limit
    }
}

/// Evaluate extended-hours price paths. State is persisted as `exit_pending`
/// before the network call; the stable client id makes a retry/restart idempotent.
pub async fn process_windows(
    engine: &Arc<BrokerEngine>,
    config: &Arc<RwLock<AppConfig>>,
    secrets: &Arc<RwLock<Secrets>>,
    db: &Arc<Mutex<Connection>>,
    windows: &HashMap<String, FillWindow>,
) {
    let local_window = crate::time::is_premarket_or_afterhours(crate::time::now());
    let (account_id, creds) = {
        let cfg = config.read().unwrap();
        let sec = secrets.read().unwrap();
        (cfg.broker.account_id(), super::creds_for(&cfg, &sec))
    };
    let Ok(creds) = creds else { return };
    let mirror = engine.snapshot();

    enum Action {
        Submit {
            symbol: String,
            side: Side,
            limit: f64,
            client_id: String,
            extended_hours: bool,
        },
        Replace {
            symbol: String,
            order_id: String,
            limit: f64,
        },
    }
    let actions =
        {
            let mut book = engine.protection.write().unwrap();
            if book.account_id != account_id {
                let conn = db.lock().unwrap();
                load(&mut book, &conn, &account_id);
            }
            let mut out = Vec::new();
            for (symbol, plan) in &mut book.plans {
                let Some(pos) = mirror.positions.get(symbol) else {
                    continue;
                };
                let Some(w) = windows.get(symbol) else {
                    continue;
                };
                let qty = pos.quantity.abs().floor() as i64;
                if qty <= 0 {
                    continue;
                }

                if plan.status == ProtectionStatus::ExitPending {
                    // A fast panic can run through the first collar before Alpaca books
                    // it. Chase only in the risk-reducing direction, never away from the
                    // market, while retaining a bounded limit order.
                    if let Some(order) = mirror.orders.values().find(|o| {
                        o.client_order_id == plan.exit_client_order_id && o.status.is_live()
                    }) {
                        let desired = marketable_exit_limit(plan.side, w.last);
                        let chase = match plan.side {
                            Side::Long => order.limit_price.is_some_and(|p| desired < p),
                            Side::Short => order.limit_price.is_some_and(|p| desired > p),
                        };
                        if chase {
                            out.push(Action::Replace {
                                symbol: symbol.clone(),
                                order_id: order.id.clone(),
                                limit: desired,
                            });
                        }
                    } else if (Utc::now() - plan.updated_at).num_seconds() >= 2 {
                        // The previous protective order is terminal/absent but the
                        // position still exists. The exit intent survives sessions:
                        // re-submit with the fresh id minted by reconciliation.
                        out.push(Action::Submit {
                            symbol: symbol.clone(),
                            side: plan.side,
                            limit: marketable_exit_limit(plan.side, w.last),
                            client_id: plan.exit_client_order_id.clone(),
                            extended_hours: local_window,
                        });
                        plan.updated_at = Utc::now();
                    }
                    continue;
                }
                if !matches!(
                    plan.status,
                    ProtectionStatus::EntryPending | ProtectionStatus::LocalActive
                ) {
                    continue;
                }
                if !local_window {
                    continue;
                }
                if mirror
                    .orders
                    .values()
                    .any(|o| o.symbol == *symbol && o.parent_id.is_some() && o.status.is_live())
                {
                    continue; // venue OCO cancellation has not fully settled yet
                }
                plan.status = ProtectionStatus::LocalActive;
                if trigger_rank(plan, w).is_none() {
                    continue;
                }
                plan.status = ProtectionStatus::ExitPending;
                plan.updated_at = Utc::now();
                out.push(Action::Submit {
                    symbol: symbol.clone(),
                    side: plan.side,
                    limit: marketable_exit_limit(plan.side, w.last),
                    client_id: plan.exit_client_order_id.clone(),
                    extended_hours: true,
                });
            }
            let conn = db.lock().unwrap();
            let _ = persist(&book, &conn);
            out
        };

    for action in actions {
        match action {
            Action::Submit {
                symbol,
                side,
                limit,
                client_id,
                extended_hours,
            } => {
                // Alpaca has no reduce_only flag for equity orders. Refresh the
                // position immediately before sending, and refuse if it disappeared
                // or flipped since the price-window snapshot.
                let fresh = match alpaca_rest::position(&creds, &symbol).await {
                    Ok(Some(p)) if (p.quantity < 0.0) == (side == Side::Short) => p,
                    Ok(_) => {
                        engine.kick();
                        continue;
                    }
                    Err(e) => {
                        eprintln!("[tagdash] vérification pré-sortie {symbol}: {e}");
                        engine.kick();
                        continue;
                    }
                };
                let qty = fresh.quantity.abs().floor() as i64;
                if qty <= 0 {
                    engine.kick();
                    continue;
                }
                let req = OrderRequest {
                    symbol: symbol.clone(),
                    quantity: qty,
                    side: match side {
                        Side::Long => Side::Short,
                        Side::Short => Side::Long,
                    },
                    order_type: OrderType::Limit,
                    limit_price: Some(limit),
                    stop_price: None,
                    stop_loss: None,
                    take_profit: None,
                    time_in_force: Some("day".into()),
                    extended_hours,
                    client_order_id: Some(client_id),
                    reduce_only: false,
                };
                if let Err(e) = super::submit(engine, &creds, &req).await {
                    // Keep exit_pending until REST reconciliation settles the
                    // ambiguous response. If Alpaca did accept it, retrying here
                    // would create a duplicate-exit race.
                    eprintln!("[tagdash] protection extended hours {symbol}: {e}");
                    engine.kick();
                }
            }
            Action::Replace {
                symbol,
                order_id,
                limit,
            } => {
                match alpaca_rest::replace(&creds, &order_id, None, Some(limit), None).await {
                    Ok(order) => engine.mirror.write().unwrap().apply_order(order),
                    Err(e) => eprintln!("[tagdash] suivi de limite protectrice {symbol}: {e}"),
                }
                engine.kick();
            }
        }
    }
}

/// Reconcile plan lifecycle and transfer local protection to venue OCO in RTH.
pub async fn reconcile_account(
    engine: &Arc<BrokerEngine>,
    creds: &alpaca_rest::Creds,
    account_id: &str,
    db: &Arc<Mutex<Connection>>,
) {
    let snapshot = engine.snapshot();
    let extended = crate::time::is_premarket_or_afterhours(crate::time::now());
    // Outside an executable extended-hours window, keep protection at Alpaca as a
    // GTC OCO. Overnight this is queued for the next RTH and survives TagDash being
    // stopped; at 04:00/16:00 ET it is canceled before local limit protection takes over.
    let venue_owned = !extended;
    let mut oco = Vec::new();
    let mut cancellations = Vec::new();
    {
        let mut book = engine.protection.write().unwrap();
        if book.account_id != account_id {
            let conn = db.lock().unwrap();
            load(&mut book, &conn, account_id);
        }
        book.plans.retain(|symbol, plan| {
            let Some(pos) = snapshot.positions.get(symbol) else {
                let entry_live = snapshot.orders.values().any(|o| {
                    o.symbol == *symbol
                        && o.client_order_id == plan.entry_client_order_id
                        && o.status.is_live()
                });
                // A manual flatten can leave an unrelated protective order working;
                // explicitly pull every order that belongs to this plan.
                cancellations.extend(
                    snapshot
                        .orders
                        .values()
                        .filter(|o| {
                            o.symbol == *symbol
                                && (o.client_order_id == plan.exit_client_order_id
                                    || o.client_order_id == plan.oco_client_order_id)
                                && o.status.is_live()
                        })
                        .map(|o| o.id.clone()),
                );
                return entry_live
                    || (plan.status == ProtectionStatus::EntryPending
                        && (Utc::now() - plan.created_at).num_minutes() < 5);
            };
            let exit_live = snapshot
                .orders
                .values()
                .any(|o| o.client_order_id == plan.exit_client_order_id && o.status.is_live());
            let oco_live = snapshot
                .orders
                .values()
                .find(|o| o.client_order_id == plan.oco_client_order_id && o.status.is_live());
            let oco_legs: Vec<_> = snapshot
                .orders
                .values()
                .filter(|o| o.symbol == *symbol && o.parent_id.is_some() && o.status.is_live())
                .collect();
            if oco_live.is_some() {
                plan.status = ProtectionStatus::VenueOco;
            }
            if plan.status == ProtectionStatus::EntryPending {
                plan.status = ProtectionStatus::LocalActive;
            }
            if plan.status == ProtectionStatus::ExitPending && !exit_live {
                // Terminal/rejected/expired partial exit with a remaining position:
                // re-arm with a fresh idempotency key for the next live price window.
                plan.exit_client_order_id =
                    alpaca_rest::prefixed_client_order_id("tagdash-xh-exit");
                plan.updated_at = Utc::now();
            }
            if plan.status == ProtectionStatus::VenueOco {
                if extended {
                    if let Some(order) = oco_live {
                        cancellations.push(order.id.clone());
                    } else if !oco_legs.is_empty() {
                        cancellations.extend(oco_legs.iter().map(|o| o.id.clone()));
                    } else {
                        plan.status = ProtectionStatus::LocalActive;
                    }
                } else if venue_owned && oco_live.is_none() {
                    plan.status = ProtectionStatus::LocalActive;
                    plan.oco_client_order_id =
                        alpaca_rest::prefixed_client_order_id("tagdash-rth-oco");
                }
            }
            if venue_owned && matches!(plan.status, ProtectionStatus::LocalActive) {
                if let (Some(sl), Some(tp)) = (plan.stop_loss, plan.take_profit) {
                    let qty = pos.quantity.abs().floor() as i64;
                    if qty > 0 {
                        oco.push((
                            symbol.clone(),
                            plan.side,
                            qty,
                            sl,
                            tp,
                            plan.oco_client_order_id.clone(),
                        ));
                    }
                }
            }
            true
        });
        let conn = db.lock().unwrap();
        let _ = persist(&book, &conn);
    }

    let canceled_any = !cancellations.is_empty();
    for order_id in cancellations {
        if let Err(e) = alpaca_rest::cancel(creds, &order_id).await {
            eprintln!("[tagdash] annulation protection avant changement de session: {e}");
        }
    }
    if canceled_any {
        engine.kick();
    }

    for (symbol, side, _snapshot_qty, sl, tp, coid) in oco {
        let fresh_qty = match alpaca_rest::position(creds, &symbol).await {
            Ok(Some(p)) if (p.quantity < 0.0) == (side == Side::Short) => {
                p.quantity.abs().floor() as i64
            }
            Ok(_) => {
                engine.kick();
                continue;
            }
            Err(e) => {
                eprintln!("[tagdash] vérification avant OCO {symbol}: {e}");
                continue;
            }
        };
        if fresh_qty <= 0 {
            continue;
        }
        match alpaca_rest::submit_oco_exit(creds, &symbol, side, fresh_qty, sl, tp, &coid).await {
            Ok(orders) => {
                {
                    let mut mirror = engine.mirror.write().unwrap();
                    for o in orders {
                        mirror.apply_order(o);
                    }
                }
                let mut book = engine.protection.write().unwrap();
                if let Some(p) = book.plans.get_mut(&symbol) {
                    p.status = ProtectionStatus::VenueOco;
                    p.updated_at = Utc::now();
                }
                let conn = db.lock().unwrap();
                let _ = persist(&book, &conn);
                engine.kick();
            }
            Err(e) => eprintln!("[tagdash] transfert OCO Alpaca {symbol}: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(side: Side, sl: f64, tp: f64) -> ProtectionPlan {
        ProtectionPlan {
            trade_id: "t".into(),
            symbol: "XYZ".into(),
            side,
            requested_quantity: 10,
            stop_loss: Some(sl),
            take_profit: Some(tp),
            entry_client_order_id: "e".into(),
            exit_client_order_id: "x".into(),
            oco_client_order_id: "o".into(),
            status: ProtectionStatus::LocalActive,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn same_window_uses_the_observed_path_order() {
        let p = plan(Side::Long, 9.0, 11.0);
        let low_first = FillWindow {
            first: 10.0,
            high: 11.5,
            low: 8.5,
            last: 10.0,
            low_first: true,
        };
        let high_first = FillWindow {
            low_first: false,
            ..low_first
        };
        assert_eq!(trigger_rank(&p, &low_first), Some(0));
        assert_eq!(trigger_rank(&p, &high_first), Some(0));
    }

    #[test]
    fn exit_limit_has_a_bounded_marketable_collar() {
        assert_eq!(marketable_exit_limit(Side::Long, 10.0), 9.5);
        assert_eq!(marketable_exit_limit(Side::Short, 10.0), 10.5);
    }
}
