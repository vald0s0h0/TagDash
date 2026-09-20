// Apex commands + executor.
//
// A child of `commands` on purpose: it reuses the order path's private helpers
// verbatim (quote synthesis, venue routing, TradeTally mirroring, persistence), so
// an Apex order is placed exactly the way the Market button places one — only with
// its own levels, its own size and its own strategy id.
//
// Two tasks, started once from the setup hook:
//   • the SIGNAL consumer awaits the flow book's channel and acts on receipt — no
//     polling between the tipping print and the order, on the way in OR out;
//   • HOUSEKEEPING (250 ms) mirrors the displayed tickers into the flow book,
//     refreshes the armed ticker's ATR bracket once a second, walks the trailing
//     stop up behind an open trade, disarms when the ground moves (replay reset, a
//     position or order appears) and notices when an Apex trade has gone flat.
//
// Locks: the runtime lock and the market/book/chart locks are never held together.
// Every write re-checks `arm_id`, so a hotkey press racing a trigger can never let a
// stale decision overwrite a newer one.

use chrono::Utc;
use tauri::{AppHandle, Emitter, Manager};

use super::{
    alpaca_active, market_prices, persist_book, persist_chart, strategy_max_risk,
    submit_entry_to_alpaca, tt_sync_fill,
};
use crate::apex::{
    self,
    flow::{
        ArmedSymbol, ExitParams, ExitReason, ExitTrigger, FlowParams, FlowSignal, FlowTrigger,
        GuardedPosition,
    },
    fmt_px, levels, ApexOptions, ApexPhase, ApexStatus, Runtime,
};
use crate::internal_trading::InternalBook;
use crate::market_state::aggregators::Timeframe;
use crate::state::AppState;
use crate::types::{OrderStatus, OrderType, Side};

type State<'a> = tauri::State<'a, AppState>;

const HOUSEKEEPING_MS: u64 = 250;
const ATR_REFRESH_MS: i64 = 1_000;
/// With Alpaca the position appears when `trade_updates` reports the fill. Past
/// this without ever seeing it, the order did not fill and Apex returns to rest.
const VENUE_POSITION_GRACE_SECS: i64 = 20;

fn emit(app: &AppHandle, state: &State<'_>) {
    let _ = app.emit(apex::EVENT, state.apex.status());
}

fn side_label(side: Side) -> &'static str {
    match side {
        Side::Long => "LONG",
        Side::Short => "SHORT",
    }
}

fn reason_label(reason: ExitReason) -> &'static str {
    match reason {
        ExitReason::Flow => "TP dynamique",
        ExitReason::Block => "TP dynamique",
        ExitReason::TrailStop => "stop suiveur",
    }
}

// ─── Commands ─────────────────────────────────────────────────────────────────

#[tauri::command(rename_all = "snake_case")]
pub fn get_apex_options(state: State<'_>) -> ApexOptions {
    *state.apex.options.read().unwrap()
}

/// Replace Apex's options. Persisted first, then pushed into the flow book, so the
/// next print is measured with them.
#[tauri::command(rename_all = "snake_case")]
pub fn set_apex_options(
    options: ApexOptions,
    app: AppHandle,
    state: State<'_>,
) -> Result<(), String> {
    let clean = options.sanitized();
    {
        let db = state.db.lock().unwrap();
        clean.save(&db)?;
    }
    *state.apex.options.write().unwrap() = clean;

    let disarmed = {
        let mut rt = state.apex.runtime.write().unwrap();
        match rt.phase {
            ApexPhase::Armed if !clean.enabled => {
                rt.reset();
                rt.notice = Some(
                    state
                        .apex
                        .notice("warn", "Apex désactivé — armement retiré"),
                );
                true
            }
            ApexPhase::Armed => {
                // New multipliers / minimum: re-evaluate the bracket on the next tick.
                rt.last_atr_at = None;
                false
            }
            _ => false,
        }
    };
    {
        let mut ms = state.market.write().unwrap();
        if disarmed {
            ms.apex.disarm();
        }
        // Off → the book drops every tracker and stops measuring altogether. An open
        // Apex position keeps being watched until it is closed.
        ms.apex.configure(
            clean.enabled,
            FlowParams::from_options(&clean),
            ExitParams::from_options(&clean),
        );
    }
    emit(&app, &state);
    Ok(())
}

#[tauri::command(rename_all = "snake_case")]
pub fn get_apex_status(state: State<'_>) -> ApexStatus {
    state.apex.status()
}

/// The hotkey. Idle → arm on `symbol`; Armed → disarm (whatever zone the mouse is
/// over, so a second press is always the safe action); Firing / InTrade → ignored.
/// Async only to stay off the main thread: it takes the market lock the feed holds.
#[tauri::command(rename_all = "snake_case")]
pub async fn apex_toggle(
    zone_id: String,
    symbol: String,
    app: AppHandle,
    state: State<'_>,
) -> Result<ApexStatus, String> {
    let symbol = symbol.trim().to_uppercase();

    // ── Already armed / trading ──
    let (phase, arm_id, armed_symbol) = {
        let rt = state.apex.runtime.read().unwrap();
        (rt.phase, rt.arm_id, rt.symbol.clone().unwrap_or_default())
    };
    match phase {
        ApexPhase::Armed => {
            disarm_if(
                &state,
                arm_id,
                "info",
                format!("Apex désarmé ({armed_symbol})"),
            );
            emit(&app, &state);
            return Ok(state.apex.status());
        }
        ApexPhase::Firing | ApexPhase::InTrade => {
            {
                let mut rt = state.apex.runtime.write().unwrap();
                rt.notice = Some(state.apex.notice(
                    "warn",
                    format!("Trade Apex en cours sur {armed_symbol} — touche ignorée"),
                ));
            }
            emit(&app, &state);
            return Ok(state.apex.status());
        }
        ApexPhase::Idle | ApexPhase::Disabled => {}
    }

    // ── Arm ──
    let opts = *state.apex.options.read().unwrap();
    if !opts.enabled {
        return Err("Apex est désactivé (Réglages → Stratégies → Micro Pullback)".into());
    }
    if !opts.long_enabled && !opts.short_enabled {
        return Err("aucune direction Apex active (Long / Short)".into());
    }
    if symbol.is_empty() {
        return Err("aucun ticker dans cette zone".into());
    }
    if position_open(&state, &symbol) {
        return Err(format!("position déjà ouverte sur {symbol}"));
    }
    if entry_order_pending(&state, &symbol) {
        return Err(format!("ordre en attente sur {symbol}"));
    }
    let (bid, ask) = market_prices(&state, &symbol);
    if bid <= 0.0 || ask <= 0.0 {
        return Err(format!("pas de cotation pour {symbol}"));
    }
    let atr = atr_for(&state, &symbol).ok_or_else(|| {
        format!(
            "pas assez de barres 10 s pour l'ATR({}) sur {symbol}",
            levels::ATR_PERIOD
        )
    })?;
    let (sl_distance, tp_distance, tradeable) = bracket_distances(&opts, atr, (bid + ask) / 2.0);

    let arm_id = state.apex.next_id();
    // Runtime first, flow book second: a burst already under way may fire the very
    // next print, and the executor only accepts a trigger the runtime expects.
    {
        let mut rt = state.apex.runtime.write().unwrap();
        *rt = Runtime {
            phase: ApexPhase::Armed,
            arm_id,
            symbol: Some(symbol.clone()),
            zone_id: Some(zone_id),
            atr: Some(atr),
            sl_distance: Some(sl_distance),
            tp_distance,
            tradeable,
            replay_generation: crate::replay::clock::generation(),
            replay_active: crate::replay::clock::is_active(),
            last_atr_at: Some(Utc::now()),
            notice: Some(if tradeable {
                state.apex.notice(
                    "info",
                    format!(
                        "Apex armé sur {symbol} — SL {} $ · TP {}",
                        fmt_px(sl_distance),
                        tp_distance
                            .map(|d| format!("{} $", fmt_px(d)))
                            .unwrap_or_else(|| "dynamique".into()),
                    ),
                )
            } else {
                state.apex.notice(
                    "warn",
                    format!(
                        "Apex armé sur {symbol}, mais une distance est sous le minimum de {} $ : aucun trade tant que l'ATR reste sous le seuil",
                        fmt_px(opts.min_distance())
                    ),
                )
            }),
            ..Default::default()
        };
    }
    let armed = state.market.write().unwrap().apex.arm(ArmedSymbol {
        arm_id,
        symbol,
        tradeable,
        not_before_ms: 0,
    });
    if !armed {
        state.apex.runtime.write().unwrap().reset();
        return Err("moteur Apex inactif".into());
    }
    emit(&app, &state);
    Ok(state.apex.status())
}

/// Stop and target distances for one ATR reading, and whether both clear the
/// minimum. Under the dynamic take profit there is no target distance to clear.
fn bracket_distances(opts: &ApexOptions, atr: f64, ref_price: f64) -> (f64, Option<f64>, bool) {
    let sl = levels::distance(atr, opts.sl_atr_multiplier, ref_price);
    let tp = (!opts.tp_dynamic).then(|| levels::distance(atr, opts.tp_atr_multiplier, ref_price));
    let min = opts.min_distance();
    let ok = levels::meets_minimum(sl, min) && tp.map_or(true, |d| levels::meets_minimum(d, min));
    (sl, tp, ok)
}

// ─── Engine ───────────────────────────────────────────────────────────────────

pub fn start(app: AppHandle) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<FlowSignal>();
    let mut focus_rx = {
        let state = app.state::<AppState>();
        let opts = *state.apex.options.read().unwrap();
        let mut ms = state.market.write().unwrap();
        ms.apex.set_sender(tx);
        ms.apex.configure(
            opts.enabled,
            FlowParams::from_options(&opts),
            ExitParams::from_options(&opts),
        );
        state.focus_symbols_tx.subscribe()
    };

    let signal_app = app.clone();
    tauri::async_runtime::spawn(async move {
        while let Some(signal) = rx.recv().await {
            match signal {
                FlowSignal::Entry(trigger) => on_trigger(&signal_app, trigger).await,
                FlowSignal::Exit(exit) => on_exit(&signal_app, exit).await,
            }
        }
    });

    tauri::async_runtime::spawn(async move {
        let mut first = true;
        loop {
            if !first {
                tokio::time::sleep(tokio::time::Duration::from_millis(HOUSEKEEPING_MS)).await;
            }
            let state = app.state::<AppState>();
            if first || focus_rx.has_changed().unwrap_or(false) {
                let symbols: Vec<String> = focus_rx
                    .borrow_and_update()
                    .iter()
                    .map(|s| s.to_uppercase())
                    .collect();
                state.market.write().unwrap().apex.set_watch(symbols);
            }
            first = false;
            let rt = state.apex.runtime.read().unwrap().clone();
            let changed = match rt.phase {
                ApexPhase::Armed => armed_tick(&state, &rt),
                ApexPhase::InTrade => in_trade_tick(&state, &rt),
                _ => false,
            };
            if changed {
                emit(&app, &state);
            }
        }
    });
}

/// Disarm only if `arm_id` is still the armed one. Returns whether it was.
fn disarm_if(state: &State<'_>, arm_id: u64, level: &'static str, message: String) -> bool {
    {
        let mut rt = state.apex.runtime.write().unwrap();
        if rt.phase != ApexPhase::Armed || rt.arm_id != arm_id {
            return false;
        }
        rt.reset();
        rt.notice = Some(state.apex.notice(level, message));
    }
    state.market.write().unwrap().apex.disarm_id(arm_id);
    true
}

fn armed_tick(state: &State<'_>, rt: &Runtime) -> bool {
    let sym = rt.symbol.clone().unwrap_or_default();
    let opts = *state.apex.options.read().unwrap();

    let reason = if crate::replay::clock::generation() != rt.replay_generation
        || crate::replay::clock::is_active() != rt.replay_active
    {
        Some(format!(
            "Apex désarmé ({sym}) : le Market Replay a changé d'état"
        ))
    } else if !opts.long_enabled && !opts.short_enabled {
        Some("Apex désarmé : aucune direction active".to_string())
    } else if position_open(state, &sym) || entry_order_pending(state, &sym) {
        Some(format!(
            "Apex désarmé ({sym}) : une position ou un ordre est apparu"
        ))
    } else {
        None
    };
    if let Some(message) = reason {
        return disarm_if(state, rt.arm_id, "warn", message);
    }

    let now = Utc::now();
    if rt
        .last_atr_at
        .is_some_and(|t| (now - t).num_milliseconds() < ATR_REFRESH_MS)
    {
        return false;
    }
    let (bid, ask) = market_prices(state, &sym);
    let ref_px = if bid > 0.0 && ask > 0.0 {
        (bid + ask) / 2.0
    } else {
        0.0
    };
    let atr = atr_for(state, &sym);
    let (sl_distance, tp_distance, tradeable) = match atr {
        Some(a) => {
            let (sl, tp, ok) = bracket_distances(&opts, a, ref_px);
            (Some(sl), tp, ok)
        }
        None => (None, None, false),
    };

    let changed = {
        let mut w = state.apex.runtime.write().unwrap();
        if w.phase != ApexPhase::Armed || w.arm_id != rt.arm_id {
            return false;
        }
        w.last_atr_at = Some(now);
        let changed = w.atr != atr
            || w.sl_distance != sl_distance
            || w.tp_distance != tp_distance
            || w.tradeable != tradeable;
        if w.tradeable != tradeable {
            let d = sl_distance.map(fmt_px).unwrap_or_else(|| "—".into());
            w.notice = Some(if tradeable {
                state.apex.notice(
                    "info",
                    format!("Apex {sym} : SL ±{d} $ — déclenchement actif"),
                )
            } else {
                state.apex.notice(
                    "warn",
                    format!(
                        "Apex {sym} : distance sous le minimum de {} $ — déclenchement suspendu",
                        fmt_px(opts.min_distance())
                    ),
                )
            });
        }
        w.atr = atr;
        w.sl_distance = sl_distance;
        w.tp_distance = tp_distance;
        w.tradeable = tradeable;
        changed
    };
    state
        .market
        .write()
        .unwrap()
        .apex
        .set_tradeable(rt.arm_id, tradeable);
    changed
}

fn in_trade_tick(state: &State<'_>, rt: &Runtime) -> bool {
    let sym = rt.symbol.clone().unwrap_or_default();
    let open = position_open(state, &sym);
    if open {
        let mut changed = false;
        // The venue's own average entry is the truth the R is measured from; it only
        // exists once the fill is reported.
        if !rt.position_seen {
            let entry = position_entry(state, &sym);
            let mut w = state.apex.runtime.write().unwrap();
            if w.phase != ApexPhase::InTrade || w.arm_id != rt.arm_id {
                return false;
            }
            w.position_seen = true;
            if let Some(e) = entry {
                w.entry_price = Some(e);
            }
        }
        changed |= trail_tick(state, rt, &sym);
        return changed;
    }

    let mut w = state.apex.runtime.write().unwrap();
    if w.phase != ApexPhase::InTrade || w.arm_id != rt.arm_id {
        return false;
    }
    if w.position_seen {
        w.reset();
        w.notice = Some(state.apex.notice(
            "info",
            format!("Trade Apex clôturé ({sym}) — V pour réarmer"),
        ));
        drop(w);
        state.market.write().unwrap().apex.unguard_id(rt.arm_id);
        return true;
    }
    let expired = w
        .fired_at
        .is_some_and(|t| (Utc::now() - t).num_seconds() > VENUE_POSITION_GRACE_SECS);
    if expired {
        w.reset();
        w.notice = Some(state.apex.notice(
            "warn",
            format!("Apex {sym} : aucune position constatée chez le broker — Apex au repos"),
        ));
        drop(w);
        state.market.write().unwrap().apex.unguard_id(rt.arm_id);
        return true;
    }
    false
}

/// Walk the trailing stop up (down for a short) behind the best price the flow book
/// has seen. With the internal broker the bracket order itself is moved — it then
/// fills on the real price path like any stop. With Alpaca the venue stop stays put
/// as the safety net and the flow book watches TagDash's own level instead.
fn trail_tick(state: &State<'_>, rt: &Runtime, sym: &str) -> bool {
    let opts = *state.apex.options.read().unwrap();
    if !opts.sl_trailing {
        return false;
    }
    let (Some(entry), Some(risk), Some(trail), Some(side), Some(current)) = (
        rt.entry_price,
        rt.risk_per_share,
        rt.trail_distance,
        rt.side,
        rt.stop_price,
    ) else {
        return false;
    };
    let best = state
        .market
        .read()
        .unwrap()
        .apex
        .guard_best(rt.arm_id)
        .unwrap_or(0.0);
    let best = if best > 0.0 {
        best
    } else {
        let (bid, ask) = market_prices(state, sym);
        match side {
            Side::Long => bid,
            Side::Short => ask,
        }
    };
    let Some(candidate) = levels::trail_stop(side, entry, best, risk, opts.sl_trail_start_r, trail)
    else {
        return false;
    };
    if !levels::trail_improves(side, current, candidate, opts.trail_step()) {
        return false;
    }

    let take_profit = state
        .chart
        .read()
        .unwrap()
        .get_context(sym)
        .and_then(|c| c.take_profit);
    if rt.venue {
        state
            .market
            .write()
            .unwrap()
            .apex
            .set_guard_stop(rt.arm_id, Some(candidate));
    } else {
        state
            .internal_book
            .write()
            .unwrap()
            .update_protective_levels(sym, Some(candidate), take_profit);
        persist_book(&state.internal_book, &state.db);
    }
    state
        .chart
        .write()
        .unwrap()
        .sync_levels(sym, Some(candidate), take_profit);
    persist_chart(&state.chart, &state.db);

    let mut w = state.apex.runtime.write().unwrap();
    if w.phase == ApexPhase::InTrade && w.arm_id == rt.arm_id {
        w.stop_price = Some(candidate);
        w.trail_active = true;
    }
    true
}

// ─── Firing ───────────────────────────────────────────────────────────────────

struct Fired {
    trade_id: String,
    side: Side,
    qty: i64,
    entry: f64,
    stop_loss: f64,
    take_profit: Option<f64>,
    sl_distance: f64,
    tp_distance: Option<f64>,
    atr: f64,
    venue: bool,
}

enum Outcome {
    Fired(Fired),
    /// Not taken, but the setup still stands: back to Armed. `tradeable` false when
    /// a distance is under the minimum (the ATR refresh re-enables it).
    Rearm {
        message: String,
        tradeable: bool,
    },
    /// Not taken, and Apex returns to rest.
    Abort(String),
}

async fn on_trigger(app: &AppHandle, trigger: FlowTrigger) {
    let state = app.state::<AppState>();
    let zone_id = {
        let mut rt = state.apex.runtime.write().unwrap();
        if rt.phase != ApexPhase::Armed || rt.arm_id != trigger.arm_id {
            return; // disarmed between the print and here
        }
        rt.phase = ApexPhase::Firing;
        rt.side = Some(trigger.side);
        rt.zone_id.clone().unwrap_or_else(|| trigger.symbol.clone())
    };
    emit(app, &state);

    let outcome = fire(&state, &trigger, &zone_id).await;
    let opts = *state.apex.options.read().unwrap();

    enum After {
        Guard(GuardedPosition),
        Rearm(bool),
        Nothing,
    }
    let after = {
        let mut rt = state.apex.runtime.write().unwrap();
        if rt.arm_id != trigger.arm_id {
            After::Nothing
        } else {
            match outcome {
                Outcome::Fired(f) => {
                    rt.phase = ApexPhase::InTrade;
                    rt.side = Some(f.side);
                    rt.trade_id = Some(f.trade_id);
                    rt.entry_price = Some(f.entry);
                    rt.risk_per_share = Some(f.sl_distance);
                    rt.stop_price = Some(f.stop_loss);
                    rt.sl_distance = Some(f.sl_distance);
                    rt.tp_distance = f.tp_distance;
                    rt.trail_distance = opts
                        .sl_trailing
                        .then(|| levels::distance(f.atr, opts.sl_trail_atr_multiplier, f.entry));
                    rt.trail_active = false;
                    rt.venue = f.venue;
                    rt.fired_at = Some(Utc::now());
                    rt.position_seen = false;
                    rt.notice = Some(state.apex.notice(
                        "info",
                        format!(
                            "Apex {} {} {} @ {} · TP {} · SL {} (prints {}↑ / {}↓)",
                            side_label(f.side),
                            f.qty,
                            trigger.symbol,
                            fmt_px(f.entry),
                            f.take_profit
                                .map(fmt_px)
                                .unwrap_or_else(|| "dynamique".into()),
                            fmt_px(f.stop_loss),
                            trigger.buy_prints,
                            trigger.sell_prints,
                        ),
                    ));
                    if opts.tp_dynamic || opts.sl_trailing {
                        After::Guard(GuardedPosition {
                            arm_id: trigger.arm_id,
                            symbol: trigger.symbol.clone(),
                            side: f.side,
                            watch_flow: opts.tp_dynamic,
                            stop_price: None,
                            best_price: f.entry,
                        })
                    } else {
                        After::Nothing
                    }
                }
                Outcome::Rearm { message, tradeable } => {
                    rt.phase = ApexPhase::Armed;
                    rt.side = None;
                    rt.tradeable = tradeable;
                    rt.notice = Some(state.apex.notice("warn", message));
                    After::Rearm(tradeable)
                }
                Outcome::Abort(message) => {
                    rt.reset();
                    rt.notice = Some(state.apex.notice("error", message));
                    After::Nothing
                }
            }
        }
    };

    match after {
        After::Guard(position) => state.market.write().unwrap().apex.guard(position),
        After::Rearm(tradeable) => {
            let ok = state.market.write().unwrap().apex.arm(ArmedSymbol {
                arm_id: trigger.arm_id,
                symbol: trigger.symbol.clone(),
                tradeable,
                // The burst that just fired is still in the window: let it pass.
                not_before_ms: trigger.ts_ms + opts.window_ms as i64,
            });
            if !ok {
                let mut rt = state.apex.runtime.write().unwrap();
                if rt.arm_id == trigger.arm_id {
                    rt.reset();
                }
            }
        }
        After::Nothing => {}
    }
    emit(app, &state);
}

async fn fire(state: &State<'_>, trigger: &FlowTrigger, zone_id: &str) -> Outcome {
    let opts = *state.apex.options.read().unwrap();
    let sym = trigger.symbol.as_str();
    let side = trigger.side;

    if !opts.enabled {
        return Outcome::Abort("Apex désactivé — trade non pris".into());
    }
    let direction_on = match side {
        Side::Long => opts.long_enabled,
        Side::Short => opts.short_enabled,
    };
    if !direction_on {
        return Outcome::Rearm {
            message: format!(
                "Apex {sym} : {} désactivé — trade non pris",
                side_label(side)
            ),
            tradeable: true,
        };
    }
    if position_open(state, sym) || entry_order_pending(state, sym) {
        return Outcome::Abort(format!(
            "Apex {sym} : position ou ordre déjà ouvert — trade non pris"
        ));
    }

    let (bid, ask) = market_prices(state, sym);
    if bid <= 0.0 || ask <= 0.0 {
        return Outcome::Rearm {
            message: format!("Apex {sym} : pas de cotation — trade non pris"),
            tradeable: true,
        };
    }
    let entry = crate::internal_trading::fills::price_for_side(side, bid, ask);

    let Some(atr) = atr_for(state, sym) else {
        return Outcome::Rearm {
            message: format!("Apex {sym} : ATR indisponible — trade non pris"),
            tradeable: true,
        };
    };
    let (sl_distance, tp_distance, tradeable) = bracket_distances(&opts, atr, entry);
    if !tradeable {
        return Outcome::Rearm {
            message: format!(
                "Apex {sym} : SL {} $ / TP {} sous le minimum de {} $ — trade non pris",
                fmt_px(sl_distance),
                tp_distance
                    .map(|d| format!("{} $", fmt_px(d)))
                    .unwrap_or_else(|| "dynamique".into()),
                fmt_px(opts.min_distance())
            ),
            tradeable: false,
        };
    }
    let Some(bracket) = levels::bracket(side, entry, sl_distance, tp_distance) else {
        return Outcome::Abort(format!("Apex {sym} : niveaux TP/SL invalides"));
    };

    let risk = strategy_max_risk(state, apex::OWNER_STRATEGY_ID) * opts.risk_pct / 100.0;
    let qty = {
        let cfg = state.config.read().unwrap();
        InternalBook::compute_risk_sizing(
            entry,
            bracket.stop_loss,
            risk,
            cfg.trading.min_position_size,
            cfg.trading.max_position_size,
        )
        .full_position_size
    };
    if qty <= 0 {
        return Outcome::Abort(format!(
            "Apex {sym} : taille 0 — {risk:.0} $ de risque ne couvre pas un stop à {} $",
            fmt_px(sl_distance)
        ));
    }

    // The chart context becomes the Apex trade: its own tradeID and strategy id (so
    // TradeTally files it as "Apex"), its own levels.
    let previous = state.chart.read().unwrap().get_context(sym);
    let ctx = state.chart.write().unwrap().start_apex_trade(
        zone_id,
        sym,
        bracket.stop_loss,
        bracket.take_profit,
    );
    persist_chart(&state.chart, &state.db);
    let trade_id = ctx.trade_id.clone().unwrap_or_default();

    let fired = |venue: bool| {
        Outcome::Fired(Fired {
            trade_id: trade_id.clone(),
            side,
            qty,
            entry,
            stop_loss: bracket.stop_loss,
            take_profit: bracket.take_profit,
            sl_distance,
            tp_distance,
            atr,
            venue,
        })
    };

    if alpaca_active(state) {
        let sent =
            submit_entry_to_alpaca(state, &ctx, side, qty, OrderType::Market, Some(entry)).await;
        if let Err(e) = sent {
            state.chart.write().unwrap().restore_context(sym, previous);
            persist_chart(&state.chart, &state.db);
            return Outcome::Abort(format!("Apex {sym} : ordre refusé — {e}"));
        }
        return fired(true);
    }

    let fill = {
        let mut book = state.internal_book.write().unwrap();
        let fill = book.execute_market_fill(
            ctx.trade_id.clone(),
            zone_id.to_string(),
            sym.to_string(),
            apex::STRATEGY_ID.to_string(),
            side,
            qty,
            entry,
            Some(bracket.stop_loss),
            bracket.take_profit,
        );
        book.sync_bracket_orders(sym);
        fill
    };
    tt_sync_fill(state, &fill);
    fired(false)
}

// ─── Exiting (dynamic take profit / trailing stop) ────────────────────────────

async fn on_exit(app: &AppHandle, exit: ExitTrigger) {
    let state = app.state::<AppState>();
    {
        let mut rt = state.apex.runtime.write().unwrap();
        if rt.phase != ApexPhase::InTrade || rt.arm_id != exit.arm_id || rt.exit_pending {
            return;
        }
        rt.exit_pending = true;
    }

    let result = exit_position(&state, &exit.symbol, exit.side).await;
    let failed = result.is_err();
    {
        let mut rt = state.apex.runtime.write().unwrap();
        if rt.arm_id == exit.arm_id {
            match result {
                Ok(()) => {
                    rt.notice = Some(state.apex.notice(
                        "info",
                        format!(
                            "Apex sortie {} — {} : {}",
                            exit.symbol,
                            reason_label(exit.reason),
                            exit.detail
                        ),
                    ));
                }
                Err(e) => {
                    rt.exit_pending = false;
                    rt.notice = Some(state.apex.notice(
                        "error",
                        format!("Apex {} : sortie impossible — {e}", exit.symbol),
                    ));
                }
            }
        }
    }
    if failed {
        // Keep watching: the next print gets another chance to end the trade.
        let (opts, stop_price) = {
            let opts = *state.apex.options.read().unwrap();
            let rt = state.apex.runtime.read().unwrap();
            // Only a stop the TRAIL moved is TagDash's to watch. The initial one is
            // the venue's own resting order, and watching it too would have both of
            // them close the same position.
            (
                opts,
                (rt.venue && rt.trail_active)
                    .then_some(rt.stop_price)
                    .flatten(),
            )
        };
        state.market.write().unwrap().apex.guard(GuardedPosition {
            arm_id: exit.arm_id,
            symbol: exit.symbol.clone(),
            side: exit.side,
            watch_flow: opts.tp_dynamic,
            stop_price,
            best_price: exit.price,
        });
    }
    emit(app, &state);
}

/// Flatten the position at market — the venue's own close in the regular session, a
/// marketable limit in extended hours (Alpaca takes no market order then), the
/// simulator's close against the quote when TagDash is the broker.
async fn exit_position(state: &State<'_>, symbol: &str, side: Side) -> Result<(), String> {
    if !alpaca_active(state) {
        let (bid, ask) = market_prices(state, symbol);
        if bid <= 0.0 || ask <= 0.0 {
            return Err("pas de cotation".into());
        }
        let (strategy_id, zone_id) = state
            .chart
            .read()
            .unwrap()
            .get_context(symbol)
            .map(|c| (c.strategy_id, c.zone_id))
            .unwrap_or_else(|| (apex::STRATEGY_ID.to_string(), symbol.to_string()));
        let fill = state
            .internal_book
            .write()
            .unwrap()
            .close_position(symbol, bid, ask, strategy_id, zone_id)
            .ok_or_else(|| format!("aucune position ouverte sur {symbol}"))?;
        tt_sync_fill(state, &fill);
        return Ok(());
    }

    let creds = {
        let cfg = state.config.read().unwrap();
        let sec = state.secrets.read().unwrap();
        crate::broker::creds_for(&cfg, &sec)?
    };
    if !crate::time::is_premarket_or_afterhours(crate::time::now()) {
        return crate::broker::close_position(&state.broker, &creds, symbol).await;
    }

    // Extended hours: one marketable limit, carrying the protection plan's own exit
    // id when there is a plan — `protection::process_windows` then chases THAT order
    // instead of sending a second one at the same position.
    let position = state
        .broker
        .mirror
        .read()
        .unwrap()
        .positions
        .get(symbol)
        .cloned();
    let Some(position) = position.filter(|p| p.quantity != 0.0) else {
        return Err("position absente du miroir broker".into());
    };
    let qty = position.quantity.abs().floor() as i64;
    if qty <= 0 {
        return Err("quantité nulle".into());
    }
    let client_order_id = {
        let mut book = state.broker.protection.write().unwrap();
        let conn = state.db.lock().unwrap();
        crate::broker::protection::mark_exit_pending(&mut book, &conn, symbol)
    }
    .unwrap_or_else(|| {
        crate::broker::alpaca_rest::prefixed_client_order_id(&format!("APEXEXIT-{symbol}"))
    });

    let (bid, ask) = market_prices(state, symbol);
    let last = match side {
        Side::Long => bid,
        Side::Short => ask,
    };
    if last <= 0.0 {
        return Err("pas de cotation".into());
    }
    let request = crate::broker::OrderRequest {
        symbol: symbol.to_string(),
        quantity: qty,
        side: match side {
            Side::Long => Side::Short,
            Side::Short => Side::Long,
        },
        order_type: OrderType::Limit,
        limit_price: Some(crate::broker::protection::marketable_exit_limit(side, last)),
        stop_price: None,
        stop_loss: None,
        take_profit: None,
        time_in_force: Some("day".into()),
        extended_hours: true,
        client_order_id: Some(client_order_id),
        reduce_only: false,
    };
    crate::broker::submit(&state.broker, &creds, &request)
        .await
        .map(|_| ())
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn atr_for(state: &State<'_>, symbol: &str) -> Option<f64> {
    let bars = state
        .market
        .read()
        .unwrap()
        .closed_bars(symbol, Timeframe::S10);
    levels::atr(&bars, levels::ATR_PERIOD)
}

/// A position on `symbol` where orders actually go (venue or simulator).
fn position_open(state: &State<'_>, symbol: &str) -> bool {
    if alpaca_active(state) {
        state
            .broker
            .mirror
            .read()
            .unwrap()
            .positions
            .get(symbol)
            .is_some_and(|p| p.quantity != 0.0)
    } else {
        state
            .internal_book
            .read()
            .unwrap()
            .positions
            .contains_key(symbol)
    }
}

/// The average entry price of that position, once it exists.
fn position_entry(state: &State<'_>, symbol: &str) -> Option<f64> {
    if alpaca_active(state) {
        state
            .broker
            .mirror
            .read()
            .unwrap()
            .positions
            .get(symbol)
            .map(|p| p.avg_entry_price)
            .filter(|p| *p > 0.0)
    } else {
        state
            .internal_book
            .read()
            .unwrap()
            .positions
            .get(symbol)
            .map(|p| p.avg_entry_price)
            .filter(|p| *p > 0.0)
    }
}

/// A working entry order on `symbol` — Apex would stack on top of it.
fn entry_order_pending(state: &State<'_>, symbol: &str) -> bool {
    if alpaca_active(state) {
        state
            .broker
            .mirror
            .read()
            .unwrap()
            .orders
            .values()
            .any(|o| o.symbol == symbol && !o.status.is_terminal())
    } else {
        state
            .internal_book
            .read()
            .unwrap()
            .orders
            .values()
            .any(|o| o.symbol == symbol && o.status == OrderStatus::Pending && !o.reduce_only)
    }
}
