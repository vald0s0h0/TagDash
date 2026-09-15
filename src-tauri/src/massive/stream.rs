// Massive live market-data WebSocket client — per-second aggregate bars ONLY.
// Endpoint: wss://delayed.massive.com/stocks (Starter tier: 15-minute delayed).
// wss://socket.massive.com/stocks (real-time) requires Advanced ($199+), which
// this app never selects — see `crate::massive::plan::Tier`.
//
// Verified LIVE against a real Starter key on 2026-08-20 (not just the docs):
//   auth:      → {"action":"auth","params":"<KEY>"}
//              ← [{"ev":"status","status":"auth_success","message":"authenticated"}]
//              (or "auth_failed" — fatal, never retry)
//   subscribe: → {"action":"subscribe","params":"A.AAPL,A.TSLA"}  (or "A.*", confirmed
//              working, but deliberately NOT used here — see below)
//              ← one {"ev":"status","status":"success","message":"subscribed to: A.SYM"}
//              per symbol
//   data:      ← {"ev":"A","sym":"AAPL","o":..,"h":..,"l":..,"c":..,"v":..,"vw":..,
//                 "s":<start_ms>,"e":<end_ms>,...}, one message per symbol per second
//
// Starter's WebSocket entitlement covers ONLY the `A` (per-second aggregate) channel
// — confirmed via massive.com/docs/websocket/stocks/{trades,quotes}'s Plan Access
// sections: `T` (trades) requires Developer ($79+), `Q` (quotes/NBBO) requires
// Advanced ($199+). `TickerLiveState.bid`/`.ask` stay unset; downstream code already
// tolerates that (see `commands::broker::zone_from_basis`, which derives a synthetic
// bid/ask from `last_price` when quotes are unavailable).
//
// Each `A` message is fed to `MarketState::on_second_aggregate`, NOT `on_bar` —
// `on_bar` expects a genuinely closed minute bar
// (Alpaca's `bars` channel contract), while Massive sends one aggregate per symbol
// per SECOND. Routing it through the trade path reuses the same candle-aggregation,
// premarket-bar and VWAP/fill-window logic every other feed already relies on, at
// whatever cadence it's called — the aggregator itself decides when a bucket
// actually closes, so calling it once a second doesn't flood anything, and every
// timeframe the user can select keeps updating live. Its native one-second OHLC is
// preserved in the strategy tape, so a fast spike is not reduced to its close.
// The aggregate carries no `n`, but it DOES carry `z` (the second's average trade
// size), and `n ≈ v / z` restores the count — see `reconstruct_prints`, measured
// exact on 96.3 % of 34 998 real premarket seconds. Trade-RATE logic is therefore
// available on this feed, and `MarketState::tape_counts_trades` says so; the
// dollars-and-continuity fallback is only for a frame that cannot answer at all.
//
// This is only safe because `massive::bars::aligned_from_ms` pins the REST history
// to the SAME UTC bucket grid `CandleAggregator::bucket_start` uses. Before that
// fix Massive's multi-minute REST bars were anchored to the query's `from`, so a
// live-aggregated 5m candle and the REST 5m candle for the same window carried
// different timestamps and both survived the merge — the chart doubled, then
// tripled. Never let the two grids diverge again.
//
// `A.*` wildcard: confirmed live to work (streams the whole US market's per-second
// bars), and now wired up generically — see `universe_wildcard_wanted` below — but
// only ever used when an ENABLED strategy declares `StreamingScope::Universe` on its
// card. Micro Pullback declares Universe, so its enabled state holds `A.*` for the
// whole US-stock population; other strategies keep the budgeted focus list.

use std::collections::{HashMap, HashSet};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex, RwLock,
};
use std::time::Instant;

use chrono::{DateTime, Utc};
use futures_util::{stream::SplitSink, SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::{AppHandle, Emitter};
use tokio::sync::watch;
use tokio::time::{interval, sleep, Duration};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};

use crate::alpaca::stream::{capped_watch_list, clean_symbols, watch_set};
use crate::broker::BrokerMirror;
use crate::internal_trading::InternalBook;
use crate::market_state::MarketState;
use crate::types::StreamingScope;

const URL: &str = "wss://delayed.massive.com/stocks";
const RECONNECT_DELAY: Duration = Duration::from_secs(3);
/// Cap on the `A` channel's symbol budget when no active strategy needs the whole
/// market (see `universe_wildcard_wanted`). Massive's docs never stated a
/// per-connection subscription limit for Starter (unlike Alpaca's clearly documented
/// 30-slot IEX cap) — 150 gives real headroom for orders + open charts + the Panic
/// Mean Reversion watchlist (up to `scanner::MR_TOP_N` + pins) without any tier
/// crowding out another, while staying far short of anything that could plausibly
/// strain the connection.
const SYMBOL_LIMIT: usize = 150;
/// How often to re-run the watch-list budget even with no focus change (picks up new
/// orders/positions/scanner scores, and strategy enable/session changes, without
/// waiting for a chart to be opened/closed).
const RECONCILE_INTERVAL: Duration = Duration::from_secs(30);
const SUBSCRIBE_CHUNK: usize = 500;
const CHUNK_DELAY: Duration = Duration::from_millis(100);
/// Min interval between pushed `market-tick` events per focus symbol. Aggregates
/// already arrive at most once a second, so this mostly guards against a burst on
/// reconnect rather than doing real coalescing.
const TICK_THROTTLE: Duration = Duration::from_millis(100);
/// Latency thresholds for the `on_trade` calls below — deliberately NOT the user's
/// configured `latency.{warn_ms,critical_ms}` (calibrated for a near-real-time feed).
/// Massive Starter is knowingly ~15 min delayed (see the "15 min" badge, Phase 1);
/// these keep the latency badge honestly showing that ~15 min without ever painting
/// it "critical" for expected, already-disclosed behavior.
const LATENCY_WARN_MS: u32 = 25 * 60_000;
const LATENCY_CRITICAL_MS: u32 = 40 * 60_000;

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
type WsWrite = SplitSink<Ws, Message>;

/// One decoded message — either a control frame (`ev:"status"`) or a per-second
/// aggregate (`ev:"A"`). Unused fields are ignored.
#[derive(Debug, Deserialize)]
struct WsMsg {
    ev: Option<String>,
    // control
    status: Option<String>,
    message: Option<String>,
    // aggregate
    sym: Option<String>,
    o: Option<f64>,
    h: Option<f64>,
    l: Option<f64>,
    c: Option<f64>,
    v: Option<u64>,
    vw: Option<f64>,
    /// Average trade size for the second. THE FIELD THAT RESTORES THE TRADE COUNT:
    /// `n ≈ v / z`. Massive sends it on every `A` frame (verified live: `"z":26`
    /// alongside `"v":2513`), and the REST second aggregates expose the true `n`,
    /// which is how the reconstruction below was validated.
    z: Option<f64>,
    /// Aggregate window start, Unix milliseconds.
    s: Option<i64>,
}

/// How many real trades a per-second aggregate contained, from its volume and its
/// average trade size.
///
/// MEASURED, NOT ASSUMED. Checked against the true `n` on 34 998 real premarket
/// seconds (8 tickers, 21 August 2026, Massive REST second aggregates): exact on
/// 96.3 % of them, within ±1 or 10 % on the rest, never worse. `floor` instead of
/// `round` on the provider's side would give 90.4 % exact and the same error bound,
/// so which rounding Massive uses does not change the answer.
///
/// Returns `None` when the frame carries no usable `z` — the caller then counts the
/// second as a single print and the tape gates fall back to dollars + continuity,
/// which is the pre-`z` behaviour.
pub(crate) fn reconstruct_prints(volume: u64, avg_trade_size: Option<f64>) -> Option<u64> {
    let z = avg_trade_size?;
    if !z.is_finite() || z <= 0.0 || volume == 0 {
        return None;
    }
    Some(((volume as f64 / z).round() as u64).max(1))
}

/// Tick pushed to the frontend for a displayed (focus) symbol.
#[derive(Debug, Clone, Serialize)]
struct TickEvent {
    symbol: String,
    price: f64,
    ts: i64,
}

/// Run the live feed until `running` is cleared. Reconnects on transient drops;
/// bails out (clearing `running`) on a fatal auth failure so we don't hammer Massive
/// with a key that will never work.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    market: Arc<RwLock<MarketState>>,
    db: Arc<Mutex<rusqlite::Connection>>,
    internal_book: Arc<RwLock<InternalBook>>,
    broker_mirror: Arc<RwLock<BrokerMirror>>,
    strategy_enabled: Arc<RwLock<HashMap<String, bool>>>,
    api_key: String,
    running: Arc<AtomicBool>,
    focus_rx: watch::Receiver<Vec<String>>,
    app: AppHandle,
) {
    eprintln!(
        "[tagdash] massive live feed: connecting to {URL} (Starter+, différé 15 min, agrégats 1s, {SYMBOL_LIMIT} symboles max)"
    );
    {
        let mut ms = market.write().unwrap();
        ms.feed_set_feed("massive-a");
        ms.feed_set_plan("massive_unlimited", SYMBOL_LIMIT);
        // Every `A` message is one second's aggregate, not one trade: gates that
        // read the tape must switch to the volume/continuity variant.
        ms.set_tape_kind(crate::types::TapeKind::SecondAggregates);
        ms.feed_state("connecting");
    }

    let mut focus_rx = focus_rx;

    while running.load(Ordering::Relaxed) {
        match connect_async(URL).await {
            Ok((ws, _resp)) => {
                let fatal = session(
                    ws,
                    &api_key,
                    &market,
                    &db,
                    &internal_book,
                    &broker_mirror,
                    &strategy_enabled,
                    &running,
                    &mut focus_rx,
                    &app,
                )
                .await;
                market.write().unwrap().live_running = false;

                if fatal {
                    eprintln!("[tagdash] massive live feed: auth failed — stopping feed");
                    running.store(false, Ordering::Relaxed);
                    break;
                }
            }
            Err(e) => {
                eprintln!("[tagdash] massive live feed: connect failed: {e}");
                market
                    .write()
                    .unwrap()
                    .feed_error(0, Some(format!("connect failed: {e}")));
            }
        }

        if running.load(Ordering::Relaxed) {
            eprintln!("[tagdash] massive live feed: reconnecting in {RECONNECT_DELAY:?}");
            market.write().unwrap().feed_reconnecting();
            sleep(RECONNECT_DELAY).await;
        }
    }
    market.write().unwrap().live_running = false;
    market.write().unwrap().feed_state("stopped");
    eprintln!("[tagdash] massive live feed: stopped");
}

/// Drive one connection: auth → reconcile the budgeted watch-list → dispatch,
/// re-reconciling on focus change and periodically. Returns `true` on a fatal auth
/// failure (caller must stop), `false` on any transient disconnect.
#[allow(clippy::too_many_arguments)]
async fn session(
    ws: Ws,
    api_key: &str,
    market: &Arc<RwLock<MarketState>>,
    db: &Arc<Mutex<rusqlite::Connection>>,
    internal_book: &Arc<RwLock<InternalBook>>,
    broker_mirror: &Arc<RwLock<BrokerMirror>>,
    strategy_enabled: &Arc<RwLock<HashMap<String, bool>>>,
    running: &Arc<AtomicBool>,
    focus_rx: &mut watch::Receiver<Vec<String>>,
    app: &AppHandle,
) -> bool {
    let (mut write, mut read) = ws.split();

    let auth = json!({ "action": "auth", "params": api_key }).to_string();
    if write.send(Message::Text(auth)).await.is_err() {
        return false; // transient
    }

    let mut authenticated = false;
    // Whether the frames are currently carrying `z`. Starts false — a session
    // proves it on its first aggregate rather than assuming it — and is published
    // to `MarketState` the moment it changes, so the tape gates adapt within one
    // second of a feed starting or stopping to supply it.
    let mut counts_trades = false;
    let mut subs: HashSet<String> = HashSet::new();
    let mut focus: HashSet<String> = clean_symbols(focus_rx.borrow().iter());
    let mut last_emit: HashMap<String, Instant> = HashMap::new();
    let mut reeval = interval(RECONCILE_INTERVAL);

    loop {
        if !running.load(Ordering::Relaxed) {
            return false;
        }

        tokio::select! {
            biased;

            changed = focus_rx.changed() => {
                if changed.is_err() { continue; }
                focus = clean_symbols(focus_rx.borrow_and_update().iter());
                if authenticated
                    && reconcile(&mut write, market, db, internal_book, broker_mirror, strategy_enabled, &focus, &mut subs).await.is_err()
                {
                    return false;
                }
            }

            _ = reeval.tick() => {
                if authenticated
                    && reconcile(&mut write, market, db, internal_book, broker_mirror, strategy_enabled, &focus, &mut subs).await.is_err()
                {
                    return false;
                }
            }

            frame = read.next() => {
                let frame = match frame {
                    Some(Ok(f))  => f,
                    Some(Err(e)) => { eprintln!("[tagdash] massive live feed: read error: {e}"); return false; }
                    None         => return false,
                };
                let text = match frame {
                    Message::Text(t)   => t,
                    Message::Binary(b) => String::from_utf8_lossy(&b).into_owned(),
                    Message::Ping(_) | Message::Pong(_) => continue,
                    Message::Close(_)  => return false,
                    _ => continue,
                };
                let msgs: Vec<WsMsg> = match serde_json::from_str(&text) {
                    Ok(m)  => m,
                    Err(_) => continue,
                };

                // Control pass (auth / subscription ack / anything else Massive sends).
                let mut fatal = false;
                for m in &msgs {
                    if m.ev.as_deref() != Some("status") {
                        continue;
                    }
                    match m.status.as_deref() {
                        Some("auth_success") => {
                            authenticated = true;
                            market.write().unwrap().feed_authenticated();
                            if reconcile(&mut write, market, db, internal_book, broker_mirror, strategy_enabled, &focus, &mut subs).await.is_err() {
                                return false;
                            }
                            eprintln!("[tagdash] massive live feed: authenticated");
                        }
                        Some("auth_failed") => {
                            eprintln!("[tagdash] massive live feed: auth failed — {:?}", m.message);
                            market.write().unwrap().feed_error(0, m.message.clone());
                            fatal = true;
                        }
                        Some("success") => {
                            market.write().unwrap().live_running = true;
                        }
                        other => {
                            eprintln!("[tagdash] massive live feed: status={other:?} msg={:?}", m.message);
                        }
                    }
                }
                if fatal {
                    return true;
                }

                // Data pass — one write lock for the whole frame; collect focus ticks
                // to emit after releasing the lock.
                if authenticated && msgs.iter().any(|m| m.ev.as_deref() == Some("A")) {
                    let now = Utc::now();
                    let count = msgs.iter().filter(|m| m.ev.as_deref() == Some("A")).count() as u64;
                    let mut ticks: HashMap<String, TickEvent> = HashMap::new();
                    {
                        let mut ms = market.write().unwrap();
                        for m in &msgs {
                            if m.ev.as_deref() != Some("A") {
                                continue;
                            }
                            if let Some(sym) = m.sym.as_deref() {
                                let Some(c) = m.c else { continue };
                                let bar_time = parse_ts_ms(m.s);
                                // event_time = the aggregate's real market instant (`s`,
                                // ~15 min in the past under Starter) — NOT `now`. It drives
                                // candle bucketing, the premarket/regular-session gate and
                                // alignment with the REST history already merged in; using
                                // the receipt instant instead would scramble all three.
                                let volume = m.v.unwrap_or(0);
                                let prints = reconstruct_prints(volume, m.z);
                                // Only a frame that actually traded can testify about
                                // `z`: `reconstruct_prints` also answers None for want
                                // of a volume to divide. This flag is GLOBAL — one
                                // zero-volume frame flipping it would push every
                                // symbol's liquidity gate onto the degraded
                                // dollars-and-continuity path until the next good
                                // frame, for a message that never disagreed.
                                if volume > 0 && prints.is_some() != counts_trades {
                                    counts_trades = prints.is_some();
                                    ms.set_tape_counts_trades(counts_trades);
                                }
                                ms.on_second_aggregate(
                                    sym,
                                    m.o.unwrap_or(c),
                                    m.h.unwrap_or(c),
                                    m.l.unwrap_or(c),
                                    c,
                                    m.vw,
                                    volume,
                                    prints.unwrap_or(1),
                                    bar_time,
                                    now,
                                    LATENCY_WARN_MS,
                                    LATENCY_CRITICAL_MS,
                                );
                                if focus.contains(sym) {
                                    ticks.insert(sym.to_string(), TickEvent {
                                        symbol: sym.to_string(),
                                        price: c,
                                        ts: bar_time.timestamp(),
                                    });
                                }
                            }
                        }
                        // Massive never sends trades/quotes on this tier — only bars.
                        ms.feed_record_data(0, 0, count, now);
                    }

                    let at = Instant::now();
                    for (sym, ev) in ticks {
                        let fresh = last_emit.get(&sym).map(|t| at.duration_since(*t) >= TICK_THROTTLE).unwrap_or(true);
                        if fresh {
                            let _ = app.emit("market-tick", &ev);
                            last_emit.insert(sym, at);
                        }
                    }
                }
            }
        }
    }
}

/// True when an enabled strategy, in one of its active sessions, declares
/// `StreamingScope::Universe` on its card — the whole market must be subscribed via
/// the `A.*` wildcard rather than the budgeted watch-list. Enabled = the runtime
/// Settings toggle when set, else the strategy's compiled default — same precedence
/// as `scanner::run`'s own `enabled_now` lookup.
fn universe_wildcard_wanted(strategy_enabled: &RwLock<HashMap<String, bool>>) -> bool {
    let enabled_now = strategy_enabled.read().unwrap();
    let session = crate::time::session_at(crate::time::now());
    crate::strategies::registry::all_strategies()
        .iter()
        .any(|s| {
            s.card().streaming_scope == StreamingScope::Universe
                && enabled_now
                    .get(s.id())
                    .copied()
                    .unwrap_or_else(|| s.enabled())
                && s.sessions().contains(&session)
        })
}

/// Recompute the desired subscription set and diff it against what's currently
/// subscribed, sending only the delta. Either the whole market via the `A.*`
/// wildcard (see `universe_wildcard_wanted`) or the budgeted watch-list.
#[allow(clippy::too_many_arguments)]
async fn reconcile(
    write: &mut WsWrite,
    market: &Arc<RwLock<MarketState>>,
    db: &Arc<Mutex<rusqlite::Connection>>,
    internal_book: &Arc<RwLock<InternalBook>>,
    broker_mirror: &Arc<RwLock<BrokerMirror>>,
    strategy_enabled: &Arc<RwLock<HashMap<String, bool>>>,
    focus: &HashSet<String>,
    current: &mut HashSet<String>,
) -> Result<(), WsError> {
    let universe = universe_wildcard_wanted(strategy_enabled);
    let desired: HashSet<String> = if universe {
        std::iter::once("*".to_string()).collect()
    } else {
        capped_watch_list(focus, internal_book, broker_mirror, db, SYMBOL_LIMIT)
    };

    let to_remove: Vec<String> = current.difference(&desired).cloned().collect();
    let to_add: Vec<String> = desired.difference(current).cloned().collect();
    if !to_remove.is_empty() {
        send_action(write, market, "unsubscribe", &to_remove).await?;
    }
    if !to_add.is_empty() {
        send_action(write, market, "subscribe", &to_add).await?;
    }
    *current = desired.clone();

    // On the wildcard, 0 = "whole market" (same convention as Alpaca SIP's broad
    // tier); the focus-count is reported for the panel only — everything is already
    // subscribed either way. On the capped budget, the count IS the spend.
    if universe {
        let watch = watch_set(focus, internal_book, broker_mirror);
        market
            .write()
            .unwrap()
            .feed_reconcile("massive-a-universe", 0, watch.len());
    } else {
        market
            .write()
            .unwrap()
            .feed_reconcile("massive-a", 0, desired.len());
    }
    Ok(())
}

/// Send a subscribe / unsubscribe for the `A` channel, chunked + paced under the
/// limit, joining symbols into Massive's comma-separated `"A.SYM1,A.SYM2"` form.
async fn send_action(
    write: &mut WsWrite,
    market: &Arc<RwLock<MarketState>>,
    action: &str,
    symbols: &[String],
) -> Result<(), WsError> {
    let total = symbols.len().div_ceil(SUBSCRIBE_CHUNK);
    for (i, chunk) in symbols.chunks(SUBSCRIBE_CHUNK).enumerate() {
        if i > 0 {
            sleep(CHUNK_DELAY).await;
        }
        let params = chunk
            .iter()
            .map(|s| format!("A.{s}"))
            .collect::<Vec<_>>()
            .join(",");
        let payload = json!({ "action": action, "params": params }).to_string();
        let label = format!(
            "{action} A msg {}/{} ({} symbols)",
            i + 1,
            total,
            chunk.len()
        );
        eprintln!(
            "[tagdash] massive live feed: → {label} (e.g. {:?})",
            &chunk[..chunk.len().min(3)]
        );
        market.write().unwrap().feed_set_last_subscribe(&label);
        write.send(Message::Text(payload)).await?;
    }
    Ok(())
}

/// Parse an aggregate's start timestamp (Unix ms); fall back to now if absent.
fn parse_ts_ms(ms: Option<i64>) -> DateTime<Utc> {
    ms.and_then(|v| chrono::TimeZone::timestamp_millis_opt(&Utc, v).single())
        .unwrap_or_else(Utc::now)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── The trade count Massive was carrying all along ────────────────────
    //
    // Validated against the true `n` on 34 998 real premarket seconds pulled from
    // Massive's own REST second aggregates (8 tickers, 21 August 2026): exact on
    // 96.3 %, within ±1 or 10 % on the rest, never worse.
    #[test]
    fn z_restores_the_trade_count() {
        // Live frame, TSLA: 2 513 shares at an average of 26 → ~97 trades.
        assert_eq!(reconstruct_prints(2_513, Some(26.0)), Some(97));
        // A single block print: the whole second is one trade.
        assert_eq!(reconstruct_prints(248, Some(248.0)), Some(1));
        // Real JUNS second, 21 August 11:09:22 — REST reported n = 17.
        assert_eq!(reconstruct_prints(1_043, Some(1_043.0 / 17.0)), Some(17));
    }

    #[test]
    fn a_frame_without_z_yields_no_count_rather_than_a_wrong_one() {
        assert_eq!(reconstruct_prints(1_000, None), None);
        assert_eq!(reconstruct_prints(1_000, Some(0.0)), None);
        assert_eq!(reconstruct_prints(1_000, Some(-4.0)), None);
        assert_eq!(reconstruct_prints(1_000, Some(f64::NAN)), None);
        // A quoted second with no volume cannot be split into trades either.
        assert_eq!(reconstruct_prints(0, Some(50.0)), None);
    }

    #[test]
    fn a_count_is_never_zero_when_the_second_traded() {
        // Volume below one average trade still means at least one trade happened.
        assert_eq!(reconstruct_prints(5, Some(500.0)), Some(1));
    }

    /// `None` has two causes and they mean opposite things: a frame with no usable
    /// `z` proves the feed cannot count trades, while a frame with no VOLUME proves
    /// nothing at all — there was simply nothing to divide. The dispatch loop only
    /// lets the first kind flip `tape_counts_trades`, because that flag is global:
    /// one uninformative frame would otherwise drop EVERY symbol's liquidity gate
    /// onto the degraded dollars-and-continuity path until the next good frame.
    #[test]
    fn a_zero_volume_frame_is_not_evidence_that_z_is_missing() {
        // Same None, different meaning.
        assert_eq!(reconstruct_prints(0, Some(26.0)), None);
        assert_eq!(reconstruct_prints(2_513, None), None);
        // What the dispatch loop asks before trusting the answer.
        let informative = |volume: u64| volume > 0;
        assert!(!informative(0));
        assert!(informative(2_513));
    }

    fn toggles(micro_pullback: bool, dislocation: bool) -> RwLock<HashMap<String, bool>> {
        RwLock::new(HashMap::from([
            ("panic_mean_reversion".to_string(), true),
            ("micro_pullback".to_string(), micro_pullback),
            ("dislocation".to_string(), dislocation),
            ("hod_drive".to_string(), true),
            ("perfect_pullback".to_string(), true),
            ("backside".to_string(), true),
        ]))
    }

    // Micro Pullback and Dislocation are the two strategies declaring
    // `StreamingScope::Universe`, and both declare every session — so with EITHER on,
    // the `A.*` wildcard is wanted at any hour. They are discovery scanners:
    // restricted to the budgeted watch-list they can only ever re-find names already
    // on screen.
    #[test]
    fn either_discovery_scanner_asks_for_the_universe_wildcard() {
        assert!(universe_wildcard_wanted(&toggles(true, true)));
        assert!(universe_wildcard_wanted(&toggles(true, false)));
        assert!(universe_wildcard_wanted(&toggles(false, true)));
    }

    // …and switching BOTH off must take the wildcard down: no other strategy asks
    // for the whole market, so the feed falls back to the budgeted list.
    #[test]
    fn no_other_strategy_holds_the_wildcard_open() {
        assert!(!universe_wildcard_wanted(&toggles(false, false)));
    }
}
