// Tauri commands for the broker layer.
//
// Routing rule, applied here and nowhere else: if the selected broker is Alpaca the
// order goes to the venue and the answer comes back from the venue; otherwise the
// internal TagDash book handles it. Callers never choose — the account selection in
// Settings does.

use crate::broker::{
    self, BorrowStatus, BrokerAccount, BrokerId, BrokerMirror, BrokerOrder, LocateRecord,
    OrderRequest, ShortCostBasis, ShortCostZone, Shortability,
};
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

#[tauri::command(rename_all = "snake_case", async)]
pub fn get_broker_selection(state: tauri::State<'_, AppState>) -> Result<BrokerSelection, String> {
    Ok(selection_of(&state))
}

/// Auto-detected vs. manually-forced Massive subscription tier, and the value
/// actually in effect. See `crate::massive::plan`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MassiveTierStatus {
    pub detected: Option<String>,
    #[serde(rename = "override")]
    pub override_: Option<String>,
    pub effective: String,
    /// True only when Massive's delayed stream feeds the app (no Alpaca keys) —
    /// the one case where the Alpaca broker is locked. See
    /// `massive::plan::drives_live_feed`.
    pub drives_live_feed: bool,
}

fn massive_drives_live(state: &tauri::State<'_, AppState>) -> bool {
    crate::massive::plan::drives_live_feed_from_app_state(&state.secrets, &state.db, &state.config)
}

fn effective_massive_tier(state: &tauri::State<'_, AppState>) -> crate::massive::plan::Tier {
    crate::massive::plan::effective_from_app_state(&state.db, &state.config)
}

#[tauri::command(rename_all = "snake_case", async)]
pub fn get_massive_tier_status(
    state: tauri::State<'_, AppState>,
) -> Result<MassiveTierStatus, String> {
    let detected = {
        let g = state.db.lock().unwrap();
        crate::local_db::cache_repository::get_app_meta(&g, crate::massive::plan::TIER_META_KEY)
    };
    let override_ = state
        .config
        .read()
        .unwrap()
        .data_source
        .massive_tier_override
        .clone();
    let effective = effective_massive_tier(&state);
    Ok(MassiveTierStatus {
        detected,
        override_,
        effective: effective.as_str().into(),
        drives_live_feed: massive_drives_live(&state),
    })
}

/// Force the Massive tier, overriding auto-detection (e.g. when the probe gets it
/// wrong). `tier` must be `"unlimited"`, `"basic"`, or `None` to go back to
/// automatic. Re-applies the broker safety gate immediately, since forcing
/// "unlimited" without Alpaca keys must disable Alpaca just as detecting it would.
#[tauri::command(rename_all = "snake_case", async)]
pub fn set_massive_tier_override(
    tier: Option<String>,
    state: tauri::State<'_, AppState>,
) -> Result<MassiveTierStatus, String> {
    if let Some(t) = &tier {
        if crate::massive::plan::Tier::from_meta(t).is_none() {
            return Err(format!("tier Massive inconnu : {t}"));
        }
    }
    {
        let mut cfg = state.config.write().unwrap();
        cfg.data_source.massive_tier_override = tier;
        let path = crate::config::config_path(&state.app_dir);
        crate::config::save(&path, &cfg)?;
    }
    crate::massive::plan::enforce_broker_gate(
        &state.config,
        &state.app_dir,
        massive_drives_live(&state),
    );
    get_massive_tier_status(state)
}

/// Switch broker / environment. Persisted immediately: the background truth-keeper
/// picks the change up on its next pass, reconnecting the trade-updates stream to
/// the newly selected account and clearing the mirror of the old one.
#[tauri::command(rename_all = "snake_case", async)]
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
    // Massive Starter+ data is 15-minute delayed — never let it drive a real order
    // sent to Alpaca. `startup::run_pipeline` already forces TagDash when Massive
    // feeds the app; this is the second door, for a manual switch attempted
    // afterwards in the same session.
    if broker_id == "alpaca" && massive_drives_live(&state) {
        return Err(
            "Alpaca indisponible tant que Massive Starter+ alimente le flux (pas de \
             clés Alpaca, données différées de 15 min) — ajoute tes clés Alpaca dans \
             Réglages pour réactiver Alpaca"
                .into(),
        );
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
    state.broker.locate_plans.write().unwrap().clear();
    Ok(selection_of(&state))
}

/// The venue's current view. Empty for the internal broker, whose book is served by
/// the existing `get_internal_*` commands.
#[tauri::command(rename_all = "snake_case", async)]
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
        // HTB inventory is a per-account, single-use entitlement. Refuse locally
        // when the user has not explicitly prepared enough locate lots instead of
        // firing an order that Alpaca can only reject after the setup is armed.
        let asset = broker::alpaca_rest::shortability(&creds, &sym)
            .await
            .map_err(|e| format!("statut borrow Alpaca invérifiable pour {sym} : {e}"))?;
        if !asset.shortable || asset.borrow_status == BorrowStatus::Unavailable {
            return Err(format!(
                "{sym} n'est pas shortable chez Alpaca — ordre non envoyé"
            ));
        }
        if asset.borrow_status == BorrowStatus::Unknown {
            return Err(format!(
                "statut borrow Alpaca inconnu pour {sym} — ordre short non envoyé"
            ));
        }
        if asset.borrow_status == BorrowStatus::HardToBorrow {
            let account_id = state.config.read().unwrap().broker.account_id();
            let plan = state
                .broker
                .locate_plans
                .read()
                .unwrap()
                .get(&sym)
                .filter(|p| p.account_id == account_id)
                .cloned();
            let valid = plan.as_ref().is_some_and(|p| {
                let position_used = state
                    .broker
                    .snapshot()
                    .positions
                    .get(&sym)
                    .filter(|pos| pos.quantity < 0.0)
                    .map(|pos| pos.quantity.abs().round() as i64)
                    .unwrap_or(0);
                p.located_quantity - p.consumed_quantity.max(position_used) >= request.quantity
                    && p.expires_at
                        .is_none_or(|expiry| expiry > chrono::Utc::now())
            });
            if !valid {
                return Err(format!(
                    "{sym} est HTB — réserve d'abord {} actions via le bouton Locate",
                    round_up_lot(request.quantity)
                ));
            }
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
/// moves during the day and `borrow_status` only exists there.
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
    // Cached fallback. `borrow_status` is unknown offline; never infer ETB merely
    // because the broader universe row says the symbol is shortable.
    let cached = {
        let conn = state.db.lock().unwrap();
        universe_repository::get_by_symbol(&conn, &sym)
            .ok()
            .flatten()
    };
    match cached {
        Some(a) => Ok(Shortability {
            symbol: sym,
            broker: BrokerId::Alpaca,
            shortable: a.shortable,
            borrow_status: if a.shortable {
                BorrowStatus::Unknown
            } else {
                BorrowStatus::Unavailable
            },
            from_cache: true,
        }),
        // Unknown symbol: assume NOT shortable. A false "you can short this" leads to
        // a rejected order mid-setup; a false "you can't" only prompts a check.
        None => Ok(Shortability {
            symbol: sym,
            broker: BrokerId::Alpaca,
            shortable: false,
            borrow_status: BorrowStatus::Unavailable,
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
#[tauri::command(rename_all = "snake_case", async)]
pub fn get_non_shortable_symbols(state: tauri::State<'_, AppState>) -> Result<Vec<String>, String> {
    let conn = state.db.lock().unwrap();
    universe_repository::non_shortable_symbols(&conn).map_err(|e| e.to_string())
}

fn round_up_lot(quantity: i64) -> i64 {
    if quantity <= 0 {
        0
    } else {
        ((quantity + 99) / 100) * 100
    }
}

fn locate_idempotency_key(symbol: &str, trade_id: Option<&str>, quantity: i64) -> String {
    let logical = format!(
        "tagdash-locate-{}-{}",
        symbol.to_uppercase(),
        trade_id.unwrap_or("setup")
    );
    let clean: String = logical
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .take(100)
        .collect();
    format!("{clean}-{quantity}")
}

fn remaining_locate_quantity(locate: &LocateRecord) -> i64 {
    if !locate.status.eq_ignore_ascii_case("active") {
        return 0;
    }
    // Never infer remaining single-use inventory from the original amount. The
    // create path fills this field explicitly; historical API rows must supply it.
    locate.available_qty.unwrap_or(0).max(0)
}

fn borrow_days_now() -> u32 {
    use chrono::Datelike;
    // Alpaca charges three days for a Friday settlement date.
    if crate::time::to_et(chrono::Utc::now()).weekday() == chrono::Weekday::Fri {
        3
    } else {
        1
    }
}

fn zone_from_basis(
    basis: &ShortCostBasis,
    stop_loss: f64,
    estimate_entry: f64,
    position: Option<&crate::broker::BrokerPosition>,
) -> Result<ShortCostZone, String> {
    let (source, entry, quantity) = match position.filter(|p| p.quantity < 0.0) {
        Some(p) => (
            "position".to_string(),
            p.avg_entry_price,
            p.quantity.abs().round() as i64,
        ),
        None => (
            "estimate".to_string(),
            estimate_entry,
            basis.planned_quantity,
        ),
    };
    if quantity <= 0 || !entry.is_finite() || entry <= 0.0 {
        return Err("quantité ou prix d'entrée invalide pour la zone locate".into());
    }
    let days = borrow_days_now();
    let borrow_fee = basis.annual_borrow_rate.map(|rate| {
        // Borrow is assessed in round lots. The estimate uses the reserved lots;
        // after a fill the actual position decides how many lots are charged.
        let charged_qty = if source == "position" {
            round_up_lot(quantity)
        } else {
            basis.located_quantity
        };
        entry * charged_qty as f64 * rate * days as f64 / 360.0
    });
    let total_known_cost = basis.locate_fee + borrow_fee.unwrap_or(0.0);
    Ok(ShortCostZone {
        broker: BrokerId::Alpaca,
        symbol: basis.symbol.clone(),
        source,
        entry_price: entry,
        stop_loss,
        quantity,
        located_quantity: basis.located_quantity,
        locate_fee: basis.locate_fee,
        annual_borrow_rate: basis.annual_borrow_rate,
        borrow_days: days,
        borrow_fee,
        total_known_cost,
        break_even_price: (entry - total_known_cost / quantity as f64).max(0.0),
        costs_complete: basis.annual_borrow_rate.is_some(),
        locates: basis.locates.clone(),
    })
}

fn short_setup(
    state: &tauri::State<'_, AppState>,
    zone_id: &str,
) -> Result<(crate::chart_state::ZoneTradeContext, f64, i64), String> {
    let ctx = state
        .chart
        .read()
        .unwrap()
        .get_context_for_zone(zone_id)
        .ok_or_else(|| "zone sans contexte de trade — place d'abord le SL".to_string())?;
    let stop_loss = ctx
        .stop_loss
        .ok_or_else(|| "SL requis avant de demander un locate".to_string())?;
    let (bid, ask) = {
        let market = state.market.read().unwrap();
        let ticker = market
            .tickers
            .get(&ctx.symbol)
            .ok_or_else(|| "aucune cotation disponible".to_string())?;
        let last = ticker.last_price.unwrap_or(0.0);
        (
            ticker.bid.unwrap_or_else(|| (last * 0.999).max(0.0)),
            ticker.ask.unwrap_or_else(|| last * 1.001),
        )
    };
    let (side, entry) = crate::internal_trading::risk::executable_entry(stop_loss, bid, ask)
        .ok_or_else(|| "cotation invalide pour calculer le locate".to_string())?;
    if !matches!(side, crate::types::Side::Short) {
        return Err("le SL doit être au-dessus du prix d'entrée pour un short".into());
    }
    let max_risk = {
        let configured = state
            .strategy_risk
            .read()
            .unwrap()
            .get(&ctx.strategy_id)
            .map(|r| r.max_risk_dollars);
        configured.unwrap_or_else(|| {
            crate::strategies::registry::all_strategies()
                .iter()
                .find(|s| s.id() == ctx.strategy_id)
                .map(|s| s.risk_config().max_risk_dollars)
                .unwrap_or(100.0)
        })
    };
    let sizing = {
        let cfg = state.config.read().unwrap();
        crate::internal_trading::InternalBook::compute_risk_sizing(
            entry,
            stop_loss,
            max_risk,
            cfg.trading.min_position_size,
            cfg.trading.max_position_size,
        )
    };
    if sizing.full_position_size <= 0 {
        return Err("taille de position nulle — vérifie la distance au SL".into());
    }
    Ok((ctx, entry, sizing.full_position_size))
}

/// Explicit paid action behind the chart's Locate button. Existing active lots
/// are selected cheapest-first; only the missing round lots are purchased.
#[tauri::command(rename_all = "snake_case")]
pub async fn prepare_short_locate(
    zone_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<ShortCostZone, String> {
    refuse_venue_during_replay(&state)?;
    let selection = selection_of(&state);
    if selection.broker != "alpaca" {
        return Err("les locates sont disponibles uniquement avec le broker Alpaca".into());
    }
    let (ctx, entry, planned_quantity) = short_setup(&state, &zone_id)?;
    let creds = {
        let cfg = state.config.read().unwrap();
        let sec = state.secrets.read().unwrap();
        broker::creds_for(&cfg, &sec)?
    };
    let shortability = broker::alpaca_rest::shortability(&creds, &ctx.symbol).await?;
    if !shortability.shortable || shortability.borrow_status == BorrowStatus::Unavailable {
        return Err(format!("{} n'est pas shortable chez Alpaca", ctx.symbol));
    }
    if shortability.borrow_status == BorrowStatus::Unknown {
        return Err(format!(
            "statut borrow Alpaca inconnu pour {} — impossible de préparer le locate",
            ctx.symbol
        ));
    }

    let target_located = round_up_lot(planned_quantity);
    let cached_basis = state
        .broker
        .locate_plans
        .read()
        .unwrap()
        .get(&ctx.symbol)
        .filter(|b| {
            b.account_id == selection.account_id
                && b.expires_at
                    .is_none_or(|expiry| expiry > chrono::Utc::now())
        })
        .cloned();
    let mut selected = cached_basis
        .as_ref()
        .map(|b| b.locates.clone())
        .unwrap_or_default();
    let mut rate = cached_basis.as_ref().and_then(|b| b.annual_borrow_rate);

    if shortability.borrow_status == BorrowStatus::HardToBorrow {
        let has_short_position = state
            .broker
            .snapshot()
            .positions
            .get(&ctx.symbol)
            .is_some_and(|p| p.quantity < 0.0);
        if has_short_position && cached_basis.is_none() {
            return Err(
                "position short déjà ouverte : base de coûts locate inconnue dans cette session"
                    .into(),
            );
        }
        let mut covered = cached_basis
            .as_ref()
            .map(|b| b.located_quantity - b.consumed_quantity)
            .unwrap_or(0)
            .max(0)
            .min(target_located);

        if cached_basis.is_none() {
            let mut existing = broker::alpaca_rest::locates(&creds, &ctx.symbol).await?;
            existing.sort_by(|a, b| {
                a.price_per_share
                    .partial_cmp(&b.price_per_share)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            let mut ambiguous_active = 0usize;
            for locate in existing {
                let available = remaining_locate_quantity(&locate);
                if available <= 0 {
                    if locate.status.eq_ignore_ascii_case("active")
                        && locate
                            .expires_at
                            .is_none_or(|expiry| expiry > chrono::Utc::now())
                    {
                        ambiguous_active += 1;
                    }
                    continue;
                }
                if covered >= target_located {
                    continue;
                }
                covered += available.min(target_located - covered);
                selected.push(locate);
            }
            // Alpaca's list response documents the paid quantity but not a remaining
            // quantity. After an app restart we therefore cannot tell whether an
            // active locate is still usable. Never buy a duplicate automatically.
            if ambiguous_active > 0 && covered < target_located {
                return Err(format!(
                    "{ambiguous_active} locate(s) Alpaca actif(s) existe(nt) déjà pour {} mais l'API ne donne pas la quantité restante — achat automatique bloqué pour éviter une double facturation",
                    ctx.symbol
                ));
            }
        }

        let missing = round_up_lot((target_located - covered).max(0));
        if missing > 0 {
            let quote = broker::alpaca_rest::locate_quote(&creds, &ctx.symbol).await?;
            rate = quote.annual_borrow_rate;
            if quote.available_qty < missing {
                return Err(format!(
                    "inventaire locate insuffisant : {} actions disponibles, {} requises",
                    quote.available_qty, missing
                ));
            }
            let key = locate_idempotency_key(&ctx.symbol, ctx.trade_id.as_deref(), missing);
            let created = broker::alpaca_rest::create_locate(
                &creds,
                &ctx.symbol,
                missing,
                quote.price_per_share,
                &key,
            )
            .await?;
            if !created.status.eq_ignore_ascii_case("active") {
                return Err(format!(
                    "locate refusé par Alpaca : statut {}",
                    created.status
                ));
            }
            selected.push(created);
        }
    }

    let located_quantity = if shortability.borrow_status == BorrowStatus::EasyToBorrow {
        rate = Some(0.0);
        planned_quantity
    } else {
        selected.iter().map(remaining_locate_quantity).sum::<i64>()
    };
    let locate_fee = selected.iter().map(|l| l.total_fee).sum();
    let expires_at = selected.iter().filter_map(|l| l.expires_at).min();
    let basis = ShortCostBasis {
        account_id: selection.account_id,
        symbol: ctx.symbol.clone(),
        planned_quantity,
        located_quantity,
        consumed_quantity: cached_basis
            .as_ref()
            .map(|b| b.consumed_quantity)
            .unwrap_or(0),
        locate_fee,
        annual_borrow_rate: rate,
        expires_at,
        locates: selected,
    };
    state
        .broker
        .locate_plans
        .write()
        .unwrap()
        .insert(ctx.symbol.clone(), basis.clone());
    let position = state.broker.snapshot().positions.get(&ctx.symbol).cloned();
    zone_from_basis(&basis, ctx.stop_loss.unwrap(), entry, position.as_ref())
}

/// Pure refresh path: combines the paid locate basis with Alpaca's latest position
/// quantity and average fill, so partial fills immediately tighten the band.
#[tauri::command(rename_all = "snake_case", async)]
pub fn get_short_cost_zone(
    zone_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<Option<ShortCostZone>, String> {
    let ctx = state
        .chart
        .read()
        .unwrap()
        .get_context_for_zone(&zone_id)
        .ok_or_else(|| "zone sans contexte de trade".to_string())?;
    let stop_loss = ctx
        .stop_loss
        .ok_or_else(|| "SL requis pour afficher la zone locate".to_string())?;
    let selection = selection_of(&state);
    let basis = state
        .broker
        .locate_plans
        .read()
        .unwrap()
        .get(&ctx.symbol)
        .filter(|b| b.account_id == selection.account_id)
        .cloned();
    let Some(mut basis) = basis else {
        return Ok(None);
    };
    if basis
        .expires_at
        .is_some_and(|expiry| expiry <= chrono::Utc::now())
    {
        state
            .broker
            .locate_plans
            .write()
            .unwrap()
            .remove(&ctx.symbol);
        return Ok(None);
    }
    let position = state.broker.snapshot().positions.get(&ctx.symbol).cloned();
    if let Some(quantity) = position
        .as_ref()
        .filter(|p| p.quantity < 0.0)
        .map(|p| p.quantity.abs().round() as i64)
    {
        basis.consumed_quantity = basis.consumed_quantity.max(quantity);
        state
            .broker
            .locate_plans
            .write()
            .unwrap()
            .insert(ctx.symbol.clone(), basis.clone());
    }
    let estimate_entry = match position.as_ref().filter(|p| p.quantity < 0.0) {
        Some(p) => p.avg_entry_price,
        None => short_setup(&state, &zone_id)?.1,
    };
    zone_from_basis(&basis, stop_loss, estimate_entry, position.as_ref()).map(Some)
}

#[cfg(test)]
mod locate_tests {
    use super::*;

    #[test]
    fn locate_quantity_rounds_up_to_full_hundreds() {
        assert_eq!(round_up_lot(0), 0);
        assert_eq!(round_up_lot(1), 100);
        assert_eq!(round_up_lot(100), 100);
        assert_eq!(round_up_lot(101), 200);
    }

    #[test]
    fn locate_retry_keeps_the_same_bounded_idempotency_key() {
        let a = locate_idempotency_key("test", Some("trade/with odd chars"), 200);
        let b = locate_idempotency_key("TEST", Some("trade/with odd chars"), 200);
        assert_eq!(a, b);
        assert!(a.len() <= 128);
        assert!(a.ends_with("-200"));
    }

    #[test]
    fn partial_fill_uses_actual_quantity_for_break_even() {
        let basis = ShortCostBasis {
            account_id: "paper".into(),
            symbol: "TEST".into(),
            planned_quantity: 200,
            located_quantity: 200,
            consumed_quantity: 0,
            locate_fee: 20.0,
            annual_borrow_rate: Some(0.36),
            expires_at: None,
            locates: vec![],
        };
        let position = crate::broker::BrokerPosition {
            symbol: "TEST".into(),
            quantity: -50.0,
            avg_entry_price: 10.0,
            market_price: None,
            market_value: None,
            cost_basis: None,
            unrealized_pnl: None,
            unrealized_pnl_pct: None,
        };
        let z = zone_from_basis(&basis, 11.0, 9.9, Some(&position)).unwrap();
        assert_eq!(z.source, "position");
        assert_eq!(z.quantity, 50);
        // 100-share borrow lot * $10 * 36% / 360 = $1 for one non-Friday day;
        // locate $20 + borrow $1 => $0.42/share break-even move.
        if z.borrow_days == 1 {
            assert!((z.break_even_price - 9.58).abs() < 1e-9);
        }
    }
}
