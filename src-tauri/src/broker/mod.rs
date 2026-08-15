// Broker layer: where orders go, and what the app believes about them.
//
// THE RULE, for the Alpaca broker: the venue is the only source of truth. Nothing
// in here invents a fill, an average price, or a position size. Every row the UI
// shows was read back from Alpaca — either from a REST reconcile or from a
// `trade_updates` event. The internal TagDash broker keeps its synthetic fills, but
// those are its own and never touch an Alpaca account.
//
// Truth is kept fresh two ways, on purpose:
//
//   • `trade_updates` websocket — the low-latency path. Alpaca pushes every
//     lifecycle event (new, partial_fill, fill, canceled, rejected …) with the full
//     order object, so the UI reflects an execution the instant the venue books it.
//   • periodic REST reconcile — the correctness path. A websocket can drop, miss a
//     frame, or reconnect having lost events; a full `GET /v2/orders` +
//     `GET /v2/positions` every `RECONCILE_EVERY` re-establishes the truth wholesale.
//     This is what makes a missed frame a latency problem instead of a wrong
//     position on screen.
//
// A stream event never *replaces* the reconcile — it advances the mirror between
// reconciles and kicks one when something filled.

pub mod alpaca_rest;
pub mod alpaca_stream;
pub mod journal;
pub mod projection;
pub mod protection;
pub mod types;

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

use crate::config::{secrets::Secrets, AppConfig};
use alpaca_rest::Creds;
pub use types::*;

/// Full REST reconcile cadence. Short enough that a dropped websocket frame is
/// corrected before it matters, long enough not to hammer the venue.
const RECONCILE_EVERY: std::time::Duration = std::time::Duration::from_secs(15);

/// What the venue currently holds, as last observed.
///
/// Every field is server-derived. `stale` is exposed rather than hidden: a UI that
/// silently shows a five-minute-old position is more dangerous than one that says
/// it lost contact.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BrokerMirror {
    pub account: Option<BrokerAccount>,
    /// Live orders, keyed by the VENUE's id.
    pub orders: HashMap<String, BrokerOrder>,
    /// Open positions, keyed by symbol.
    pub positions: HashMap<String, BrokerPosition>,
    /// When a full REST reconcile last succeeded.
    pub last_reconcile: Option<DateTime<Utc>>,
    /// When the `trade_updates` stream last delivered anything.
    pub last_event: Option<DateTime<Utc>>,
    /// True while the trade-updates websocket is authenticated and listening.
    pub stream_connected: bool,
    /// Last failure, kept so the UI can say why it is out of date.
    pub last_error: Option<String>,
}

impl BrokerMirror {
    /// True when the mirror has never synced or has lost contact long enough that
    /// its contents should not be trusted for a trading decision.
    pub fn is_stale(&self, now: DateTime<Utc>) -> bool {
        match self.last_reconcile {
            None => true,
            Some(t) => (now - t).num_seconds() > 3 * RECONCILE_EVERY.as_secs() as i64,
        }
    }

    /// Apply one order the venue told us about. Terminal orders are dropped from the
    /// live set — the mirror holds what is WORKING, and a filled/cancelled row would
    /// otherwise linger and be cancellable from the UI.
    pub fn apply_order(&mut self, order: BrokerOrder) {
        if order.status.is_terminal() {
            self.orders.remove(&order.id);
        } else {
            self.orders.insert(order.id.clone(), order);
        }
    }
}

/// Shared handle: the mirror plus a kick to force an immediate reconcile.
pub struct BrokerEngine {
    pub mirror: Arc<RwLock<BrokerMirror>>,
    /// Open trades being accumulated for TradeTally — see `journal`.
    pub journal: Arc<RwLock<journal::BrokerJournal>>,
    /// Durable TP/SL plans used while Alpaca disallows advanced orders outside RTH.
    pub protection: Arc<RwLock<protection::ProtectionBook>>,
    reconcile_now: Arc<Notify>,
}

impl Default for BrokerEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl BrokerEngine {
    pub fn new() -> Self {
        Self {
            mirror: Arc::new(RwLock::new(BrokerMirror::default())),
            journal: Arc::new(RwLock::new(journal::BrokerJournal::default())),
            protection: Arc::new(RwLock::new(protection::ProtectionBook::default())),
            reconcile_now: Arc::new(Notify::new()),
        }
    }

    /// Ask the background task to reconcile at once (after a submit, a cancel, or a
    /// fill event). `notify_one` stores a permit, so a kick raised while a reconcile
    /// is already running is honoured on the next loop rather than lost.
    pub fn kick(&self) {
        self.reconcile_now.notify_one();
    }

    pub fn snapshot(&self) -> BrokerMirror {
        self.mirror.read().unwrap().clone()
    }
}

/// Should orders leave the machine?
///
/// Two conditions, and the second is not a detail: **Market Replay is a simulation by
/// definition**. A replayed day cannot be traded at a live venue — the order would be
/// sized off a historical price and executed against today's market, on a real
/// account. So a replay always runs the internal book, whatever broker is selected,
/// and the broker choice applies to live trading only.
///
/// Every path that could reach the venue asks this: order entry, cancel, close, and
/// the trading loop that decides whether the simulator fills.
pub fn routes_to_venue(config: &AppConfig, replay_active: bool) -> bool {
    config.broker.is_alpaca() && !replay_active
}

/// Resolve the trading credentials for the account currently selected.
///
/// Paper and live keys live in separate fields and are never substituted for one
/// another: a live selection with only paper keys configured is an error, not a
/// quiet fallback that would place orders on the wrong account.
pub fn creds_for(config: &AppConfig, secrets: &Secrets) -> Result<Creds, String> {
    if !config.broker.is_alpaca() {
        return Err("le broker actif n'est pas Alpaca".into());
    }
    let live = config.broker.is_live();
    let (key, secret) = if live {
        (
            secrets.alpaca_live_key.clone(),
            secrets.alpaca_live_secret.clone(),
        )
    } else {
        (
            secrets.alpaca_paper_key.clone(),
            secrets.alpaca_paper_secret.clone(),
        )
    };
    let creds = Creds {
        key: key.unwrap_or_default(),
        secret: secret.unwrap_or_default(),
        environment: if live {
            Environment::Live
        } else {
            Environment::Paper
        },
    };
    if !creds.is_complete() {
        return Err(format!(
            "clés de trading Alpaca {} manquantes — Réglages → Broker",
            if live { "RÉELLES" } else { "paper" },
        ));
    }
    Ok(creds)
}

/// One full REST reconcile: account, working orders, open positions.
///
/// Wholesale replacement, not a merge. Merging would let an order the venue has
/// forgotten survive in the mirror forever; replacing means the mirror can only be
/// as wrong as the last round trip.
pub async fn reconcile(creds: &Creds, mirror: &Arc<RwLock<BrokerMirror>>) -> Result<(), String> {
    let (acct, orders, pos) = tokio::join!(
        alpaca_rest::account(creds),
        alpaca_rest::open_orders(creds),
        alpaca_rest::positions(creds),
    );
    // An error on any leg leaves the previous mirror in place and records why —
    // showing nothing would read as "flat", which is the most dangerous lie here.
    let (acct, orders, pos) = match (acct, orders, pos) {
        (Ok(a), Ok(o), Ok(p)) => (a, o, p),
        (a, o, p) => {
            let err = a.err().or(o.err()).or(p.err()).unwrap_or_default();
            let mut m = mirror.write().unwrap();
            m.last_error = Some(err.clone());
            return Err(err);
        }
    };

    let mut m = mirror.write().unwrap();
    m.account = Some(acct);
    m.orders = orders
        .into_iter()
        .filter(|o| o.status.is_live())
        .map(|o| (o.id.clone(), o))
        .collect();
    m.positions = pos.into_iter().map(|p| (p.symbol.clone(), p)).collect();
    m.last_reconcile = Some(Utc::now());
    m.last_error = None;
    Ok(())
}

/// Background truth-keeper for the Alpaca broker: the trade-updates stream for
/// latency, a periodic full reconcile for correctness.
///
/// Both are spawned here and both write the same mirror. Stops when the selected
/// broker is no longer Alpaca, so switching back to the internal book tears the
/// connection down instead of leaving a live socket on a real account.
pub fn spawn(
    engine: Arc<BrokerEngine>,
    config: Arc<RwLock<AppConfig>>,
    secrets: Arc<RwLock<Secrets>>,
    deps: alpaca_stream::JournalDeps,
) {
    let mirror = engine.mirror.clone();
    let kick = engine.reconcile_now.clone();
    let journal = engine.journal.clone();

    // `tauri::async_runtime::spawn`, NOT `tokio::spawn`: this is called synchronously
    // from Tauri's `setup`, where no Tokio runtime is installed on the calling thread
    // yet. `tokio::spawn` panics there — and because `setup` runs inside an
    // `extern "C"` Cocoa callback that cannot unwind, the panic aborts the process
    // instead of surfacing as an error.
    tauri::async_runtime::spawn(async move {
        let mut streaming_for: Option<String> = None;
        loop {
            let creds = {
                let c = config.read().unwrap();
                let s = secrets.read().unwrap();
                creds_for(&c, &s)
            };

            match creds {
                Ok(creds) => {
                    // (Re)start the trade-updates stream when the account changes.
                    let account_id = config.read().unwrap().broker.account_id();
                    if streaming_for.as_deref() != Some(account_id.as_str()) {
                        // Persist the old account before swapping the RAM view, then
                        // restore this account's multi-session journal/protection.
                        if let Some(previous) = streaming_for.as_deref() {
                            let journal_snapshot = journal.read().unwrap().clone();
                            let protection_snapshot = engine.protection.read().unwrap().clone();
                            let conn = deps.db.lock().unwrap();
                            let _ = crate::local_db::broker_state_repository::save_journal(
                                &conn,
                                previous,
                                &journal_snapshot,
                            );
                            let _ = crate::local_db::broker_state_repository::save_protection(
                                &conn,
                                previous,
                                &protection_snapshot,
                            );
                        }
                        let (restored_journal, restored_protection) = {
                            let conn = deps.db.lock().unwrap();
                            let restored_journal =
                                crate::local_db::broker_state_repository::load_journal(
                                    &conn,
                                    &account_id,
                                )
                                .ok()
                                .flatten()
                                .unwrap_or_default();
                            let mut restored_protection = protection::ProtectionBook::default();
                            protection::load(&mut restored_protection, &conn, &account_id);
                            (restored_journal, restored_protection)
                        };
                        *journal.write().unwrap() = restored_journal;
                        *engine.protection.write().unwrap() = restored_protection;
                        streaming_for = Some(account_id.clone());
                        alpaca_stream::spawn(
                            creds.clone(),
                            mirror.clone(),
                            kick.clone(),
                            config.clone(),
                            account_id.clone(),
                            deps.clone(),
                        );
                    }
                    if let Err(e) = reconcile(&creds, &mirror).await {
                        eprintln!("[tagdash] broker: réconciliation échouée : {e}");
                    }
                    protection::reconcile_account(&engine, &creds, &account_id, &deps.db).await;
                    // Safety net for an exit we never saw an event for — filled while
                    // the app was closed, or flattened from Alpaca's own web UI.
                    // Without this the journal would hold the trade open forever and
                    // never close it in TradeTally.
                    let orphaned = {
                        let snapshot = mirror.read().unwrap().clone();
                        journal.read().unwrap().orphaned(&snapshot)
                    };
                    for t in orphaned {
                        let after = t.fills.last().map(|f| f.filled_at).unwrap_or(t.opened_at);
                        match alpaca_rest::fills_since(&creds, after, &t.symbol).await {
                            Ok(fills) => {
                                let cfg = config.read().unwrap().clone();
                                let conn = deps.db.lock().unwrap();
                                if journal.write().unwrap().recover_closed(
                                    &t.symbol,
                                    &fills,
                                    &conn,
                                    &cfg,
                                    &account_id,
                                ) {
                                    eprintln!(
                                        "[tagdash] broker: sortie hors-ligne {} récupérée depuis les fills Alpaca",
                                        t.symbol,
                                    );
                                }
                            }
                            Err(e) => eprintln!(
                                "[tagdash] broker: reprise historique {} impossible: {e}",
                                t.symbol,
                            ),
                        }
                    }
                    // Watermarks MAE/MFE change without fills; checkpoint them every
                    // reconcile so a crash loses at most this short interval.
                    {
                        let conn = deps.db.lock().unwrap();
                        let _ = crate::local_db::broker_state_repository::save_journal(
                            &conn,
                            &account_id,
                            &*journal.read().unwrap(),
                        );
                    }
                }
                Err(e) => {
                    // Not on Alpaca (or unconfigured): clear the mirror so a stale
                    // Alpaca position can never be read as belonging to another
                    // account.
                    if let Some(previous) = streaming_for.take() {
                        let journal_snapshot = journal.read().unwrap().clone();
                        let protection_snapshot = engine.protection.read().unwrap().clone();
                        let conn = deps.db.lock().unwrap();
                        let _ = crate::local_db::broker_state_repository::save_journal(
                            &conn, &previous, &journal_snapshot,
                        );
                        let _ = crate::local_db::broker_state_repository::save_protection(
                            &conn, &previous, &protection_snapshot,
                        );
                    }
                    journal.write().unwrap().open.clear();
                    *engine.protection.write().unwrap() = protection::ProtectionBook::default();
                    let mut m = mirror.write().unwrap();
                    if m.account.is_some() || !m.orders.is_empty() || !m.positions.is_empty() {
                        *m = BrokerMirror::default();
                    }
                    m.last_error = Some(e);
                }
            }

            tokio::select! {
                _ = tokio::time::sleep(RECONCILE_EVERY) => {}
                _ = kick.notified() => {}
            }
        }
    });
}

/// Place an order on the active Alpaca account and adopt the venue's answer.
pub async fn submit(
    engine: &Arc<BrokerEngine>,
    creds: &Creds,
    req: &OrderRequest,
) -> Result<Vec<BrokerOrder>, String> {
    let orders = alpaca_rest::submit(creds, req).await?;
    {
        let mut m = engine.mirror.write().unwrap();
        for o in &orders {
            m.apply_order(o.clone());
        }
    }
    // A submit changes buying power and may fill instantly; get the real picture.
    engine.kick();
    Ok(orders)
}

pub async fn cancel(
    engine: &Arc<BrokerEngine>,
    creds: &Creds,
    order_id: &str,
) -> Result<(), String> {
    alpaca_rest::cancel(creds, order_id).await?;
    // Deliberately NOT removed from the mirror here: the order is `pending_cancel`
    // until the venue confirms, and pretending otherwise would let the UI show a
    // flat book while a live order still works.
    engine.kick();
    Ok(())
}

pub async fn close_position(
    engine: &Arc<BrokerEngine>,
    creds: &Creds,
    symbol: &str,
) -> Result<(), String> {
    alpaca_rest::close_position(creds, symbol, true).await?;
    engine.kick();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::BrokerConfig;

    fn cfg_with(broker: &str, environment: &str) -> AppConfig {
        AppConfig {
            broker: BrokerConfig {
                broker: broker.into(),
                environment: environment.into(),
            },
            ..Default::default()
        }
    }

    #[test]
    fn a_replay_never_reaches_the_venue() {
        // The dangerous case: Alpaca armed on real money, user starts a backtest. An
        // order sized off a historical price would execute against today's market.
        for env in ["paper", "live"] {
            assert!(
                !routes_to_venue(&cfg_with("alpaca", env), true),
                "a replay must stay simulated on the {env} account",
            );
        }
    }

    #[test]
    fn alpaca_takes_live_orders_when_no_replay_is_running() {
        assert!(routes_to_venue(&cfg_with("alpaca", "paper"), false));
        assert!(routes_to_venue(&cfg_with("alpaca", "live"), false));
    }

    #[test]
    fn the_internal_broker_never_reaches_a_venue() {
        assert!(!routes_to_venue(&cfg_with("tagdash", "paper"), false));
        assert!(!routes_to_venue(&cfg_with("tagdash", "paper"), true));
    }

    fn order(id: &str, status: BrokerOrderStatus) -> BrokerOrder {
        BrokerOrder {
            id: id.into(),
            client_order_id: format!("c-{id}"),
            symbol: "AAPL".into(),
            side: "buy".into(),
            order_type: "limit".into(),
            time_in_force: "day".into(),
            quantity: 10.0,
            filled_quantity: 0.0,
            limit_price: Some(200.0),
            stop_price: None,
            filled_avg_price: None,
            status,
            raw_status: "new".into(),
            parent_id: None,
            submitted_at: None,
            filled_at: None,
            canceled_at: None,
            reject_reason: None,
        }
    }

    #[test]
    fn the_mirror_holds_working_orders_and_drops_terminal_ones() {
        let mut m = BrokerMirror::default();
        m.apply_order(order("a", BrokerOrderStatus::Open));
        m.apply_order(order("b", BrokerOrderStatus::Held));
        assert_eq!(m.orders.len(), 2);

        // The same order coming back filled must LEAVE the live set, otherwise the
        // UI keeps offering to cancel something that no longer exists.
        m.apply_order(order("a", BrokerOrderStatus::Filled));
        assert!(!m.orders.contains_key("a"));
        m.apply_order(order("b", BrokerOrderStatus::Closed));
        assert!(m.orders.is_empty());
    }

    #[test]
    fn a_mirror_that_never_synced_is_stale() {
        let now = Utc::now();
        let mut m = BrokerMirror::default();
        assert!(m.is_stale(now), "never reconciled ⇒ never trusted");

        m.last_reconcile = Some(now);
        assert!(!m.is_stale(now));

        m.last_reconcile = Some(now - chrono::Duration::seconds(60));
        assert!(
            m.is_stale(now),
            "lost contact ⇒ say so rather than show old rows"
        );
    }

    /// Paper and live keys are never interchangeable. A live selection backed only
    /// by paper keys must fail loudly — the quiet fallback is how a "test" order
    /// reaches a real account.
    #[test]
    fn live_selection_refuses_to_borrow_the_paper_keys() {
        let mut cfg = AppConfig::default();
        cfg.broker = BrokerConfig {
            broker: "alpaca".into(),
            environment: "live".into(),
        };
        let secrets = Secrets {
            alpaca_paper_key: Some("pk".into()),
            alpaca_paper_secret: Some("ps".into()),
            ..Default::default()
        };
        let err = creds_for(&cfg, &secrets).unwrap_err();
        assert!(
            err.contains("RÉELLES"),
            "the message must name the missing account: {err}"
        );
    }

    #[test]
    fn paper_and_live_resolve_to_their_own_keys_and_hosts() {
        let secrets = Secrets {
            alpaca_paper_key: Some("pk".into()),
            alpaca_paper_secret: Some("ps".into()),
            alpaca_live_key: Some("lk".into()),
            alpaca_live_secret: Some("ls".into()),
            ..Default::default()
        };
        let mut cfg = AppConfig::default();

        cfg.broker = BrokerConfig {
            broker: "alpaca".into(),
            environment: "paper".into(),
        };
        let c = creds_for(&cfg, &secrets).unwrap();
        assert_eq!(c.key, "pk");
        assert_eq!(c.environment, Environment::Paper);

        cfg.broker = BrokerConfig {
            broker: "alpaca".into(),
            environment: "live".into(),
        };
        let c = creds_for(&cfg, &secrets).unwrap();
        assert_eq!(c.key, "lk");
        assert_eq!(c.environment, Environment::Live);
        assert_ne!(
            alpaca_rest::base_url(Environment::Paper),
            alpaca_rest::base_url(c.environment),
        );
    }

    #[test]
    fn the_internal_broker_is_never_live_and_never_reaches_alpaca() {
        let cfg = AppConfig::default(); // defaults to tagdash/paper
        assert!(!cfg.broker.is_alpaca());
        assert!(!cfg.broker.is_live());
        assert_eq!(cfg.broker.account_id(), "tagdash:paper");
        assert!(creds_for(&cfg, &Secrets::default()).is_err());

        // Even asked for "live", the internal simulator has no real route.
        let forced = BrokerConfig {
            broker: "tagdash".into(),
            environment: "live".into(),
        };
        assert!(
            !forced.is_live(),
            "the simulator can never spend real money"
        );
    }

    #[test]
    fn the_three_accounts_have_distinct_identities() {
        let ids: Vec<String> = [
            ("tagdash", "paper"),
            ("alpaca", "paper"),
            ("alpaca", "live"),
        ]
        .iter()
        .map(|(b, e)| {
            BrokerConfig {
                broker: (*b).into(),
                environment: (*e).into(),
            }
            .account_id()
        })
        .collect();
        assert_eq!(ids, vec!["tagdash:paper", "alpaca:paper", "alpaca:live"]);
    }
}
