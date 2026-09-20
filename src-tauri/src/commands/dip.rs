// Dip commands + executor.
//
// A child of `commands` for the same reason as `commands::apex`: it reuses the
// order path's private helpers verbatim (quote synthesis, venue routing, TradeTally
// mirroring, persistence), so a Dip order is placed exactly the way the Market
// button places one — only with its own levels, its own size and its own strategy id.
//
// Two tasks, started once from the setup hook:
//   • the SIGNAL consumer awaits the watch book's channel and acts on receipt — no
//     polling between the print that turns the move and the order;
//   • HOUSEKEEPING (250 ms) mirrors the displayed tickers into the watch book,
//     refreshes the band's ATR bracket once a second (that ATR is ALSO the speed
//     baseline the hot path measures the attack against), mirrors the live attack
//     readout into the status, disarms when the ground moves (replay reset, a
//     position or order appears) and notices when a Dip trade has gone flat.
//
// Locks: the runtime lock and the market/book/chart locks are never held together.
// Every write re-checks `arm_id`, so a hotkey press racing a trigger can never let a
// stale decision overwrite a newer one.

use chrono::Utc;
use tauri::{AppHandle, Emitter, Manager};

use super::{
    alpaca_active, market_prices, persist_chart, strategy_max_risk, submit_entry_to_alpaca,
    tt_sync_fill,
};
use crate::apex::{fmt_px, levels};
use crate::dip::{
    self,
    watch::{ArmedZone, DipLapse, DipParams, DipSignal, DipTrigger, LapseReason},
    DipOptions, DipPhase, DipStatus, Runtime,
};
use crate::internal_trading::InternalBook;
use crate::market_state::aggregators::Timeframe;
use crate::state::AppState;
use crate::types::{OrderStatus, OrderType, Side};

type State<'a> = tauri::State<'a, AppState>;

const HOUSEKEEPING_MS: u64 = 250;
const ATR_REFRESH_MS: i64 = 1_000;
/// With Alpaca the position appears when `trade_updates` reports the fill. Past
/// this without ever seeing it, the order did not fill and Dip returns to rest.
const VENUE_POSITION_GRACE_SECS: i64 = 20;
/// Thinnest and thickest a band may be tuned to, in dollars per side.
const HALF_MIN: f64 = 0.005;
const HALF_MAX: f64 = 2.5;

fn emit(app: &AppHandle, state: &State<'_>) {
    let _ = app.emit(dip::EVENT, state.dip.status());
}

fn side_label(side: Side) -> &'static str {
    match side {
        Side::Long => "LONG",
        Side::Short => "SHORT",
    }
}

/// What the band IS, from the side it is traded on.
fn role_label(side: Side) -> &'static str {
    match side {
        Side::Long => "support",
        Side::Short => "résistance",
    }
}

// ─── Commands ─────────────────────────────────────────────────────────────────

#[tauri::command(rename_all = "snake_case")]
pub fn get_dip_options(state: State<'_>) -> DipOptions {
    *state.dip.options.read().unwrap()
}

/// Replace Dip's options. Persisted first, then pushed into the watch book, so the
/// next print is measured with them.
#[tauri::command(rename_all = "snake_case")]
pub fn set_dip_options(
    options: DipOptions,
    app: AppHandle,
    state: State<'_>,
) -> Result<(), String> {
    let clean = options.sanitized();
    {
        let db = state.db.lock().unwrap();
        clean.save(&db)?;
    }
    *state.dip.options.write().unwrap() = clean;

    let disarmed = {
        let mut rt = state.dip.runtime.write().unwrap();
        match rt.phase {
            DipPhase::Armed if !clean.enabled => {
                rt.reset();
                rt.notice = Some(state.dip.notice("warn", "Dip désactivé — zone retirée"));
                true
            }
            DipPhase::Armed => {
                // New multipliers / minimum: re-evaluate the bracket on the next tick.
                // The band's own thickness is NOT touched: it is where the trader put it.
                rt.last_atr_at = None;
                false
            }
            _ => false,
        }
    };
    {
        let mut ms = state.market.write().unwrap();
        if disarmed {
            ms.dip.disarm();
        }
        // Off → the book drops every tracker and stops measuring altogether.
        ms.dip.configure(clean.enabled, DipParams::from_options(&clean));
    }
    emit(&app, &state);
    Ok(())
}

#[tauri::command(rename_all = "snake_case")]
pub fn get_dip_status(state: State<'_>) -> DipStatus {
    state.dip.status()
}

/// The placement hotkey, acting on the price under the cursor.
///
/// Armed on this ticker with the cursor INSIDE the band → the band is removed (the
/// "put it back" gesture, and the only way to cancel). Anywhere else → the band is
/// laid at `price`, replacing whatever was there. Firing / InTrade → ignored, the
/// open trade belongs to its bracket.
///
/// Async only to stay off the main thread: it takes the market lock the feed holds.
#[tauri::command(rename_all = "snake_case")]
pub async fn dip_place(
    zone_id: String,
    symbol: String,
    price: f64,
    app: AppHandle,
    state: State<'_>,
) -> Result<DipStatus, String> {
    let symbol = symbol.trim().to_uppercase();

    let (phase, arm_id, armed_symbol, low, high) = {
        let rt = state.dip.runtime.read().unwrap();
        (
            rt.phase,
            rt.arm_id,
            rt.symbol.clone().unwrap_or_default(),
            rt.low,
            rt.high,
        )
    };
    match phase {
        DipPhase::Firing | DipPhase::InTrade => {
            {
                let mut rt = state.dip.runtime.write().unwrap();
                rt.notice = Some(state.dip.notice(
                    "warn",
                    format!("Trade Dip en cours sur {armed_symbol} — touche ignorée"),
                ));
            }
            emit(&app, &state);
            return Ok(state.dip.status());
        }
        DipPhase::Armed => {
            // Pointing back into the band is how a band is taken away.
            let inside = matches!((low, high), (Some(l), Some(h)) if price >= l && price <= h);
            if inside && armed_symbol == symbol {
                disarm_if(
                    &state,
                    arm_id,
                    "info",
                    format!("Zone Dip retirée ({armed_symbol})"),
                );
                emit(&app, &state);
                return Ok(state.dip.status());
            }
        }
        DipPhase::Idle | DipPhase::Disabled => {}
    }

    // ── Place ──
    let opts = *state.dip.options.read().unwrap();
    if !opts.enabled {
        return Err("Dip est désactivé (Réglages → Stratégies → Micro Pullback)".into());
    }
    if !opts.long_enabled && !opts.short_enabled {
        return Err("aucune direction Dip active (Long / Short)".into());
    }
    if symbol.is_empty() {
        return Err("aucun ticker dans cette zone".into());
    }
    if !price.is_finite() || price <= 0.0 {
        return Err("aucun prix sous le curseur".into());
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
    let mid = (bid + ask) / 2.0;

    let half = opts.half_thickness().clamp(HALF_MIN, HALF_MAX);
    let (low, high) = (price - half, price + half);
    let side = dip::role_for(mid, low, high).ok_or_else(|| {
        format!(
            "le prix ({}) est déjà dans la zone — pose-la au-dessus ou en dessous",
            fmt_px(mid)
        )
    })?;
    let direction_on = match side {
        Side::Long => opts.long_enabled,
        Side::Short => opts.short_enabled,
    };
    if !direction_on {
        return Err(format!(
            "zone placée sous le prix = {} → {} : direction désactivée",
            role_label(side),
            side_label(side)
        ));
    }
    let atr = atr_for(&state, &symbol).ok_or_else(|| {
        format!(
            "pas assez de barres 10 s pour l'ATR({}) sur {symbol}",
            levels::ATR_PERIOD
        )
    })?;
    let (sl_distance, tp_distance, tradeable) = bracket_distances(&opts, atr, mid);

    let arm_id = state.dip.next_id();
    // Runtime first, watch book second: an attack already under way may turn on the
    // very next print, and the executor only accepts a trigger the runtime expects.
    {
        let mut rt = state.dip.runtime.write().unwrap();
        *rt = Runtime {
            phase: DipPhase::Armed,
            arm_id,
            symbol: Some(symbol.clone()),
            zone_id: Some(zone_id),
            level: Some(price),
            low: Some(low),
            high: Some(high),
            half,
            side: Some(side),
            atr: Some(atr),
            sl_distance: Some(sl_distance),
            tp_distance: Some(tp_distance),
            tradeable,
            replay_generation: crate::replay::clock::generation(),
            replay_active: crate::replay::clock::is_active(),
            last_atr_at: Some(Utc::now()),
            notice: Some(if tradeable {
                state.dip.notice(
                    "info",
                    format!(
                        "Dip {symbol} — {} {}–{} ({} ¢) → {} · SL {} $ · TP {} $",
                        role_label(side),
                        fmt_px(low),
                        fmt_px(high),
                        (half * 200.0 * 10.0).round() / 10.0,
                        side_label(side),
                        fmt_px(sl_distance),
                        fmt_px(tp_distance),
                    ),
                )
            } else {
                state.dip.notice(
                    "warn",
                    format!(
                        "Dip {symbol} : zone posée, mais une distance est sous le minimum de {} $ — aucun trade tant que l'ATR reste sous le seuil",
                        fmt_px(opts.min_distance())
                    ),
                )
            }),
            ..Default::default()
        };
    }
    let armed = state.market.write().unwrap().dip.arm(ArmedZone::new(
        arm_id, symbol, side, low, high, tradeable, atr,
    ));
    if !armed {
        state.dip.runtime.write().unwrap().reset();
        return Err("moteur Dip inactif".into());
    }
    *state.dip.attack.write().unwrap() = (false, 0.0);
    emit(&app, &state);
    Ok(state.dip.status())
}

/// The thickness hotkeys: `steps` = −1 one notch thinner, +1 one notch thicker.
/// One notch is `step_cents` of TOTAL thickness, half of it on each side, so the
/// level the trader aimed at never moves. The new thickness becomes the default
/// the next band is laid with — tuning a zone is tuning the tool.
#[tauri::command(rename_all = "snake_case")]
pub async fn dip_resize(
    steps: i32,
    app: AppHandle,
    state: State<'_>,
) -> Result<DipStatus, String> {
    let opts = *state.dip.options.read().unwrap();
    let (arm_id, level, half) = {
        let rt = state.dip.runtime.read().unwrap();
        if rt.phase != DipPhase::Armed {
            return Err("aucune zone Dip posée".into());
        }
        (rt.arm_id, rt.level.unwrap_or(0.0), rt.half)
    };
    let next = (half + steps as f64 * opts.half_step()).clamp(HALF_MIN, HALF_MAX);
    if (next - half).abs() < 1e-9 {
        return Ok(state.dip.status());
    }
    let (low, high) = (level - next, level + next);
    let thickness = (next * 200.0 * 10.0).round() / 10.0;

    {
        let mut rt = state.dip.runtime.write().unwrap();
        if rt.phase != DipPhase::Armed || rt.arm_id != arm_id {
            return Ok(state.dip.status());
        }
        rt.half = next;
        rt.low = Some(low);
        rt.high = Some(high);
        rt.notice = Some(state.dip.notice(
            "info",
            format!(
                "Zone Dip {} ¢ — {}–{}",
                thickness,
                fmt_px(low),
                fmt_px(high)
            ),
        ));
    }
    // The attack under way keeps its state: the trader widened the target, he did
    // not start a new strike.
    state.market.write().unwrap().dip.resize(arm_id, low, high);

    // Persist it as the new default, without disturbing anything else.
    let next_opts = DipOptions {
        thickness_cents: thickness,
        ..opts
    }
    .sanitized();
    {
        let db = state.db.lock().unwrap();
        let _ = next_opts.save(&db);
    }
    *state.dip.options.write().unwrap() = next_opts;

    emit(&app, &state);
    Ok(state.dip.status())
}

/// Stop and target distances for one ATR reading, and whether both clear the minimum.
fn bracket_distances(opts: &DipOptions, atr: f64, ref_price: f64) -> (f64, f64, bool) {
    let sl = levels::distance(atr, opts.sl_atr_multiplier, ref_price);
    let tp = levels::distance(atr, opts.tp_atr_multiplier, ref_price);
    let min = opts.min_distance();
    (
        sl,
        tp,
        levels::meets_minimum(sl, min) && levels::meets_minimum(tp, min),
    )
}

// ─── Engine ───────────────────────────────────────────────────────────────────

pub fn start(app: AppHandle) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<DipSignal>();
    let mut focus_rx = {
        let state = app.state::<AppState>();
        let opts = *state.dip.options.read().unwrap();
        let mut ms = state.market.write().unwrap();
        ms.dip.set_sender(tx);
        ms.dip
            .configure(opts.enabled, DipParams::from_options(&opts));
        state.focus_symbols_tx.subscribe()
    };

    let signal_app = app.clone();
    tauri::async_runtime::spawn(async move {
        while let Some(signal) = rx.recv().await {
            match signal {
                DipSignal::Entry(trigger) => on_trigger(&signal_app, trigger).await,
                DipSignal::Lapse(lapse) => on_lapse(&signal_app, lapse),
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
                state.market.write().unwrap().dip.set_watch(symbols);
            }
            first = false;
            let rt = state.dip.runtime.read().unwrap().clone();
            let changed = match rt.phase {
                DipPhase::Armed => armed_tick(&state, &rt),
                DipPhase::InTrade => in_trade_tick(&state, &rt),
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
        let mut rt = state.dip.runtime.write().unwrap();
        if rt.phase != DipPhase::Armed || rt.arm_id != arm_id {
            return false;
        }
        rt.reset();
        rt.notice = Some(state.dip.notice(level, message));
    }
    state.market.write().unwrap().dip.disarm_id(arm_id);
    *state.dip.attack.write().unwrap() = (false, 0.0);
    true
}

fn armed_tick(state: &State<'_>, rt: &Runtime) -> bool {
    let sym = rt.symbol.clone().unwrap_or_default();
    let opts = *state.dip.options.read().unwrap();

    let reason = if crate::replay::clock::generation() != rt.replay_generation
        || crate::replay::clock::is_active() != rt.replay_active
    {
        Some(format!(
            "Zone Dip retirée ({sym}) : le Market Replay a changé d'état"
        ))
    } else if rt.side.is_some_and(|s| match s {
        Side::Long => !opts.long_enabled,
        Side::Short => !opts.short_enabled,
    }) {
        Some(format!(
            "Zone Dip retirée ({sym}) : sa direction a été désactivée"
        ))
    } else if position_open(state, &sym) || entry_order_pending(state, &sym) {
        Some(format!(
            "Zone Dip retirée ({sym}) : une position ou un ordre est apparu"
        ))
    } else {
        None
    };
    if let Some(message) = reason {
        return disarm_if(state, rt.arm_id, "warn", message);
    }

    // The attack readout changes on the hot path, far too often to emit from there.
    let live = state
        .market
        .read()
        .unwrap()
        .dip
        .armed()
        .filter(|z| z.arm_id == rt.arm_id)
        .map(|z| (z.attacking(), z.peak_ratio()))
        .unwrap_or((false, 0.0));
    let attack_changed = {
        let mut w = state.dip.attack.write().unwrap();
        let changed = w.0 != live.0 || (w.1 - live.1).abs() >= 1.0;
        if changed {
            *w = live;
        }
        changed
    };

    let now = Utc::now();
    if rt
        .last_atr_at
        .is_some_and(|t| (now - t).num_milliseconds() < ATR_REFRESH_MS)
    {
        return attack_changed;
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
            (Some(sl), Some(tp), ok)
        }
        None => (None, None, false),
    };

    let changed = {
        let mut w = state.dip.runtime.write().unwrap();
        if w.phase != DipPhase::Armed || w.arm_id != rt.arm_id {
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
                state
                    .dip
                    .notice("info", format!("Dip {sym} : SL {d} $ — zone active"))
            } else {
                state.dip.notice(
                    "warn",
                    format!(
                        "Dip {sym} : distance sous le minimum de {} $ — zone en veille",
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
    {
        // The ATR is the speed baseline the hot path measures the attack against,
        // so it is pushed into the book on the very same refresh.
        let mut ms = state.market.write().unwrap();
        ms.dip.set_tradeable(rt.arm_id, tradeable);
        if let Some(a) = atr {
            ms.dip.set_atr(rt.arm_id, a);
        }
    }
    changed || attack_changed
}

fn in_trade_tick(state: &State<'_>, rt: &Runtime) -> bool {
    let sym = rt.symbol.clone().unwrap_or_default();
    if position_open(state, &sym) {
        // The venue's own average entry is the truth; it only exists once the fill
        // is reported.
        if rt.position_seen {
            return false;
        }
        let entry = position_entry(state, &sym);
        let mut w = state.dip.runtime.write().unwrap();
        if w.phase != DipPhase::InTrade || w.arm_id != rt.arm_id {
            return false;
        }
        w.position_seen = true;
        if let Some(e) = entry {
            w.entry_price = Some(e);
        }
        return false;
    }

    let mut w = state.dip.runtime.write().unwrap();
    if w.phase != DipPhase::InTrade || w.arm_id != rt.arm_id {
        return false;
    }
    if w.position_seen {
        w.reset();
        w.notice = Some(
            state
                .dip
                .notice("info", format!("Trade Dip clôturé ({sym})")),
        );
        return true;
    }
    let expired = w
        .fired_at
        .is_some_and(|t| (Utc::now() - t).num_seconds() > VENUE_POSITION_GRACE_SECS);
    if expired {
        w.reset();
        w.notice = Some(state.dip.notice(
            "warn",
            format!("Dip {sym} : aucune position constatée chez le broker — Dip au repos"),
        ));
        return true;
    }
    false
}

// ─── Attacks that came to nothing ─────────────────────────────────────────────

fn on_lapse(app: &AppHandle, lapse: DipLapse) {
    let state = app.state::<AppState>();
    let headline = match lapse.reason {
        LapseReason::Broken => "niveau cassé",
        LapseReason::TooFast => "attaque hors de portée",
        LapseReason::Fizzled => "attaque sans demi-tour",
    };
    let message = format!("Dip {} — {headline} : {}", lapse.symbol, lapse.detail);
    if lapse.disarm {
        // The band is already gone from the book; only the runtime is left to clear.
        if !disarm_if(&state, lapse.arm_id, "warn", message) {
            return;
        }
    } else {
        let mut rt = state.dip.runtime.write().unwrap();
        if rt.phase != DipPhase::Armed || rt.arm_id != lapse.arm_id {
            return;
        }
        rt.notice = Some(state.dip.notice("info", message));
    }
    emit(app, &state);
}

// ─── Firing ───────────────────────────────────────────────────────────────────

struct Fired {
    trade_id: String,
    side: Side,
    qty: i64,
    entry: f64,
    stop_loss: f64,
    take_profit: f64,
    sl_distance: f64,
    tp_distance: f64,
}

enum Outcome {
    Fired(Fired),
    /// Not taken, but the band still stands: it goes back on the chart, watching.
    /// `tradeable` false when a distance is under the minimum (the ATR refresh
    /// re-enables it).
    Rearm { message: String, tradeable: bool },
    /// Not taken, and Dip returns to rest.
    Abort(String),
}

async fn on_trigger(app: &AppHandle, trigger: DipTrigger) {
    let state = app.state::<AppState>();
    let (zone_id, low, high, atr) = {
        let mut rt = state.dip.runtime.write().unwrap();
        if rt.phase != DipPhase::Armed || rt.arm_id != trigger.arm_id {
            return; // disarmed between the print and here
        }
        rt.phase = DipPhase::Firing;
        (
            rt.zone_id.clone().unwrap_or_else(|| trigger.symbol.clone()),
            rt.low.unwrap_or(0.0),
            rt.high.unwrap_or(0.0),
            rt.atr.unwrap_or(0.0),
        )
    };
    emit(app, &state);

    let outcome = fire(&state, &trigger, &zone_id).await;

    enum After {
        Rearm(bool),
        Nothing,
    }
    let after = {
        let mut rt = state.dip.runtime.write().unwrap();
        if rt.arm_id != trigger.arm_id {
            After::Nothing
        } else {
            match outcome {
                Outcome::Fired(f) => {
                    rt.phase = DipPhase::InTrade;
                    rt.side = Some(f.side);
                    rt.trade_id = Some(f.trade_id);
                    rt.entry_price = Some(f.entry);
                    rt.stop_price = Some(f.stop_loss);
                    rt.take_profit = Some(f.take_profit);
                    rt.sl_distance = Some(f.sl_distance);
                    rt.tp_distance = Some(f.tp_distance);
                    rt.fired_at = Some(Utc::now());
                    rt.position_seen = false;
                    rt.notice = Some(state.dip.notice(
                        "info",
                        format!(
                            "Dip {} {} {} @ {} · TP {} · SL {} (demi-tour à {}, {:.1}× le rythme)",
                            side_label(f.side),
                            f.qty,
                            trigger.symbol,
                            fmt_px(f.entry),
                            fmt_px(f.take_profit),
                            fmt_px(f.stop_loss),
                            fmt_px(trigger.extreme),
                            trigger.speed_ratio,
                        ),
                    ));
                    After::Nothing
                }
                Outcome::Rearm { message, tradeable } => {
                    rt.phase = DipPhase::Armed;
                    rt.tradeable = tradeable;
                    rt.notice = Some(state.dip.notice("warn", message));
                    After::Rearm(tradeable)
                }
                Outcome::Abort(message) => {
                    rt.reset();
                    rt.notice = Some(state.dip.notice("error", message));
                    After::Nothing
                }
            }
        }
    };

    match after {
        After::Rearm(tradeable) => {
            let mut zone = ArmedZone::new(
                trigger.arm_id,
                trigger.symbol.clone(),
                trigger.side,
                low,
                high,
                tradeable,
                atr,
            );
            // The turn that just fired is still the current print: don't re-fire on it.
            zone.not_before_ms = trigger.ts_ms + 1;
            if !state.market.write().unwrap().dip.arm(zone) {
                let mut rt = state.dip.runtime.write().unwrap();
                if rt.arm_id == trigger.arm_id {
                    rt.reset();
                }
            }
        }
        After::Nothing => {}
    }
    emit(app, &state);
}

async fn fire(state: &State<'_>, trigger: &DipTrigger, zone_id: &str) -> Outcome {
    let opts = *state.dip.options.read().unwrap();
    let sym = trigger.symbol.as_str();
    let side = trigger.side;

    if !opts.enabled {
        return Outcome::Abort("Dip désactivé — trade non pris".into());
    }
    let direction_on = match side {
        Side::Long => opts.long_enabled,
        Side::Short => opts.short_enabled,
    };
    if !direction_on {
        return Outcome::Abort(format!(
            "Dip {sym} : {} désactivé — trade non pris",
            side_label(side)
        ));
    }
    if position_open(state, sym) || entry_order_pending(state, sym) {
        return Outcome::Abort(format!(
            "Dip {sym} : position ou ordre déjà ouvert — trade non pris"
        ));
    }

    let (bid, ask) = market_prices(state, sym);
    if bid <= 0.0 || ask <= 0.0 {
        return Outcome::Rearm {
            message: format!("Dip {sym} : pas de cotation — trade non pris"),
            tradeable: true,
        };
    }
    let entry = crate::internal_trading::fills::price_for_side(side, bid, ask);

    let Some(atr) = atr_for(state, sym) else {
        return Outcome::Rearm {
            message: format!("Dip {sym} : ATR indisponible — trade non pris"),
            tradeable: true,
        };
    };
    let (sl_distance, tp_distance, tradeable) = bracket_distances(&opts, atr, entry);
    if !tradeable {
        return Outcome::Rearm {
            message: format!(
                "Dip {sym} : SL {} $ / TP {} $ sous le minimum de {} $ — trade non pris",
                fmt_px(sl_distance),
                fmt_px(tp_distance),
                fmt_px(opts.min_distance())
            ),
            tradeable: false,
        };
    }
    let Some(bracket) = levels::bracket(side, entry, sl_distance, Some(tp_distance)) else {
        return Outcome::Abort(format!("Dip {sym} : niveaux TP/SL invalides"));
    };
    let Some(take_profit) = bracket.take_profit else {
        return Outcome::Abort(format!("Dip {sym} : niveau TP invalide"));
    };

    let risk = strategy_max_risk(state, dip::OWNER_STRATEGY_ID) * opts.risk_pct / 100.0;
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
            "Dip {sym} : taille 0 — {risk:.0} $ de risque ne couvre pas un stop à {} $",
            fmt_px(sl_distance)
        ));
    }

    // The chart context becomes the Dip trade: its own tradeID and strategy id (so
    // TradeTally files it as "Dip"), its own levels.
    let previous = state.chart.read().unwrap().get_context(sym);
    let ctx = state.chart.write().unwrap().start_engine_trade(
        zone_id,
        sym,
        dip::STRATEGY_ID,
        bracket.stop_loss,
        Some(take_profit),
    );
    persist_chart(&state.chart, &state.db);
    let trade_id = ctx.trade_id.clone().unwrap_or_default();

    // Both legs rest at the broker either way, so nothing here depends on where the
    // order went — unlike Apex, a Dip trade needs nobody once it is on.
    let fired = || {
        Outcome::Fired(Fired {
            trade_id: trade_id.clone(),
            side,
            qty,
            entry,
            stop_loss: bracket.stop_loss,
            take_profit,
            sl_distance,
            tp_distance,
        })
    };

    if alpaca_active(state) {
        let sent =
            submit_entry_to_alpaca(state, &ctx, side, qty, OrderType::Market, Some(entry)).await;
        if let Err(e) = sent {
            state.chart.write().unwrap().restore_context(sym, previous);
            persist_chart(&state.chart, &state.db);
            return Outcome::Abort(format!("Dip {sym} : ordre refusé — {e}"));
        }
        return fired();
    }

    let fill = {
        let mut book = state.internal_book.write().unwrap();
        let fill = book.execute_market_fill(
            ctx.trade_id.clone(),
            zone_id.to_string(),
            sym.to_string(),
            dip::STRATEGY_ID.to_string(),
            side,
            qty,
            entry,
            Some(bracket.stop_loss),
            Some(take_profit),
        );
        book.sync_bracket_orders(sym);
        fill
    };
    tt_sync_fill(state, &fill);
    fired()
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

/// A working entry order on `symbol` — Dip would stack on top of it.
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
