// Tauri commands for the broker layer.
//
// Routing rule, applied here and nowhere else: if the selected broker is Alpaca the
// order goes to the venue and the answer comes back from the venue; otherwise the
// internal TagDash book handles it. Callers never choose — the account selection in
// Settings does.

use crate::broker::{self, BrokerAccount, BrokerMirror, BrokerOrder, OrderRequest, Shortability};
use crate::config::BrokerConfig;
use crate::local_db::universe_repository;
use crate::state::AppState;

/// The active account plus everything the UI needs to warn about it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BrokerSelection {
    pub broker: String,
    pub environment: String,
    pub account_id: String,
    /// True ⇒ fills spend REAL money. The single flag the UI keys its warnings on.
    pub is_live: bool,
    /// Whether trading credentials exist for the selected account.
    pub credentials_ready: bool,
}

fn selection_of(state: &tauri::State<'_, AppState>) -> BrokerSelection {
    let cfg = state.config.read().unwrap();
    let creds_ready = {
        let sec = state.secrets.read().unwrap();
        broker::creds_for(&cfg, &sec).is_ok()
    };
    BrokerSelection {
        broker: cfg.broker.broker.clone(),
        environment: cfg.broker.environment.clone(),
        account_id: cfg.broker.account_id(),
        is_live: cfg.broker.is_live(),
        // The internal broker needs no credentials — it is always ready.
        credentials_ready: !cfg.broker.is_alpaca() || creds_ready,
    }
}

/// No mutating venue command may cross the replay boundary. Submit already had
/// this guard; cancel/replace/close need the same protection because Tauri commands
/// can be invoked directly, independently of the currently rendered UI.
fn refuse_venue_during_replay(state: &tauri::State<'_, AppState>) -> Result<(), String> {
    // `status.active` becomes true before the potentially long day-data load;
    // `clock::is_active` only flips once that load has completed. Check both so the
    // loading interval is protected too.
    let replay_running =
        state.replay.status.read().unwrap().active || crate::replay::clock::is_active();
    if replay_running {
        Err("Market Replay : aucune modification n'est envoyée à Alpaca pendant un replay".into())
    } else {
        Ok(())
    }
}

#[tauri::command(rename_all = "snake_case")]
pub fn get_broker_selection(state: tauri::State<'_, AppState>) -> Result<BrokerSelection, String> {
    Ok(selection_of(&state))
}

/// Switch broker / environment. Persisted immediately: the background truth-keeper
/// picks the change up on its next pass, reconnecting the trade-updates stream to
/// the newly selected account and clearing the mirror of the old one.
#[tauri::command(rename_all = "snake_case")]
pub fn set_broker_selection(
    broker_id: String,
    environment: String,
    state: tauri::State<'_, AppState>,
) -> Result<BrokerSelection, String> {
    let broker_id = broker_id.trim().to_lowercase();
    let environment = environment.trim().to_lowercase();
    if !matches!(broker_id.as_str(), "tagdash" | "alpaca") {
        return Err(format!("broker inconnu : {broker_id}"));
    }
    if !matches!(environment.as_str(), "paper" | "live") {
        return Err(format!("environnement inconnu : {environment}"));
    }
    // Refuse to arm a real-money account with no credentials, rather than letting
    // the failure surface later as a rejected order.
    if broker_id == "alpaca" && environment == "live" {
        let sec = state.secrets.read().unwrap();
        let ok = sec
            .alpaca_live_key
            .as_deref()
            .zip(sec.alpaca_live_secret.as_deref())
            .map(|(k, s)| !k.is_empty() && !s.is_empty())
            .unwrap_or(false);
        if !ok {
            return Err(
                "clés de trading RÉELLES Alpaca absentes — renseigne-les avant de \
                 sélectionner le compte réel"
                    .into(),
            );
        }
    }

    let prefer_live = broker_id == "alpaca" && environment == "live";
    {
        let mut cfg = state.config.write().unwrap();
        cfg.broker = BrokerConfig {
            broker: broker_id,
            environment,
        };
        let path = crate::config::config_path(&state.app_dir);
        crate::config::save(&path, &cfg)?;
    }
    // Market data rides on the selected account's key pair, so switching accounts
    // also switches the data feed (and possibly its plan: a paper-only account is
    // capped at IEX). Re-derive and persist so the next launch agrees with this one.
    {
        let mut sec = state.secrets.write().unwrap();
        sec.resolve_market_data(prefer_live);
        crate::config::secrets::save(&state.app_dir, &sec)?;
    }
    state.broker.kick();
    Ok(selection_of(&state))
}

/// The venue's current view. Empty for the internal broker, whose book is served by
/// the existing `get_internal_*` commands.
#[tauri::command(rename_all = "snake_case")]
pub fn get_broker_mirror(state: tauri::State<'_, AppState>) -> Result<BrokerMirror, String> {
    Ok(state.broker.snapshot())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn get_broker_account(
    state: tauri::State<'_, AppState>,
) -> Result<Option<BrokerAccount>, String> {
    Ok(state.broker.snapshot().account)
}

/// Submit to the ACTIVE Alpaca account, with the short pre-flight.
///
/// The single door orders take when Alpaca is the broker: the `broker_submit_order`
/// command below and the in-chart order commands both come through here, so neither
/// can drift from the other on credentials or on the shortability check.
pub(crate) async fn submit_routed(
    state: &tauri::State<'_, AppState>,
    request: &OrderRequest,
) -> Result<Vec<BrokerOrder>, String> {
    // Last line of defence. Callers already route replays to the simulator, but this
    // is the only function that can put a real order on the wire, so it refuses on
    // its own rather than trusting every caller to have checked.
    refuse_venue_during_replay(state)?;
    let creds = {
        let cfg = state.config.read().unwrap();
        let sec = state.secrets.read().unwrap();
        broker::creds_for(&cfg, &sec)?
    };
    // Pre-flight a short against the CACHED asset list — instant, no round trip on
    // the order path. It only catches the obvious case; Alpaca remains the authority
    // and still rejects a name whose borrow dried up intraday, with its own message.
    if matches!(request.side, crate::types::Side::Short) && !request.reduce_only {
        let sym = request.symbol.trim().to_uppercase();
        let known_unshortable = {
            let conn = state.db.lock().unwrap();
            universe_repository::get_by_symbol(&conn, &sym)
                .ok()
                .flatten()
                .map(|a| !a.shortable)
                .unwrap_or(false)
        };
        if known_unshortable {
            return Err(format!(
                "{sym} n'est pas shortable chez Alpaca — ordre non envoyé"
            ));
        }
    }
    broker::submit(&state.broker, &creds, request).await
}

/// Submit an order to the ACTIVE Alpaca account. Errors when the internal broker is
/// selected — the caller should use the internal order commands there.
#[tauri::command(rename_all = "snake_case")]
pub async fn broker_submit_order(
    request: OrderRequest,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<BrokerOrder>, String> {
    submit_routed(&state, &request).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn broker_cancel_order(
    order_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    refuse_venue_during_replay(&state)?;
    let creds = {
        let cfg = state.config.read().unwrap();
        let sec = state.secrets.read().unwrap();
        broker::creds_for(&cfg, &sec)?
    };
    broker::cancel(&state.broker, &creds, &order_id).await
}

/// Flatten a position at market and pull its resting exits with it.
#[tauri::command(rename_all = "snake_case")]
pub async fn broker_close_position(
    symbol: String,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    refuse_venue_during_replay(&state)?;
    let creds = {
        let cfg = state.config.read().unwrap();
        let sec = state.secrets.read().unwrap();
        broker::creds_for(&cfg, &sec)?
    };
    broker::close_position(&state.broker, &creds, &symbol).await
}

/// Replace a working order's price / size. Alpaca issues a new id; the mirror adopts
/// it on the reconcile this triggers.
#[tauri::command(rename_all = "snake_case")]
pub async fn broker_replace_order(
    order_id: String,
    quantity: Option<i64>,
    limit_price: Option<f64>,
    stop_price: Option<f64>,
    state: tauri::State<'_, AppState>,
) -> Result<BrokerOrder, String> {
    refuse_venue_during_replay(&state)?;
    let creds = {
        let cfg = state.config.read().unwrap();
        let sec = state.secrets.read().unwrap();
        broker::creds_for(&cfg, &sec)?
    };
    let order =
        broker::alpaca_rest::replace(&creds, &order_id, quantity, limit_price, stop_price).await?;
    state.broker.kick();
    Ok(order)
}

/// Can this symbol be sold short?
///
/// The cached `universe_assets.shortable` flag answers instantly for every US name
/// (the startup pipeline fills it from Alpaca's asset list). When Alpaca is the
/// active broker the live asset endpoint is preferred, because borrow availability
/// moves during the day and `easy_to_borrow` only exists there.
async fn shortability_of(
    state: &tauri::State<'_, AppState>,
    symbol: &str,
) -> Result<Shortability, String> {
    let sym = symbol.trim().to_uppercase();
    let creds = {
        let cfg = state.config.read().unwrap();
        let sec = state.secrets.read().unwrap();
        broker::creds_for(&cfg, &sec).ok()
    };
    if let Some(creds) = creds {
        if let Ok(s) = broker::alpaca_rest::shortability(&creds, &sym).await {
            return Ok(s);
        }
    }
    // Cached fallback. `easy_to_borrow` is unknown offline, so it mirrors
    // `shortable` rather than claiming a borrow we haven't checked.
    let cached = {
        let conn = state.db.lock().unwrap();
        universe_repository::get_by_symbol(&conn, &sym)
            .ok()
            .flatten()
    };
    match cached {
        Some(a) => Ok(Shortability {
            symbol: sym,
            shortable: a.shortable,
            easy_to_borrow: a.shortable,
            from_cache: true,
        }),
        // Unknown symbol: assume NOT shortable. A false "you can short this" leads to
        // a rejected order mid-setup; a false "you can't" only prompts a check.
        None => Ok(Shortability {
            symbol: sym,
            shortable: false,
            easy_to_borrow: false,
            from_cache: true,
        }),
    }
}

#[tauri::command(rename_all = "snake_case")]
pub async fn get_shortability(
    symbol: String,
    state: tauri::State<'_, AppState>,
) -> Result<Shortability, String> {
    shortability_of(&state, &symbol).await
}

/// Bulk shortability straight from the cache, for list views that would otherwise
/// issue one request per row.
#[tauri::command(rename_all = "snake_case")]
pub fn get_non_shortable_symbols(state: tauri::State<'_, AppState>) -> Result<Vec<String>, String> {
    let conn = state.db.lock().unwrap();
    universe_repository::non_shortable_symbols(&conn).map_err(|e| e.to_string())
}
