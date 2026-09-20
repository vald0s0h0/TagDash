// Alpaca live market-data WebSocket client.
// Endpoint: wss://stream.data.alpaca.markets/v2/{feed}  (feed = "sip" | "iex" | "delayed_sip")
//
// ONE connection. Two-tier subscription on the same socket:
//
//   ┌ broad surveillance tier (whole US market via the `*` wildcard), switched
//   │   by ET time:
//   │   04:00–09:30 (premarket) → `trades`  (aggregated to 5s/10s candles for
//   │                                         the scanner; keeps the 10s spike)
//   │   09:30 →      (open/AH)   → `bars`    (1-minute bars; light enough at scale)
//   └ focus tier (symbols displayed in chart zones, UNION any symbol carrying a
//       pending order or open position in the internal book) → `trades`+`quotes`,
//       always, so a displayed chart — and any resting limit/SL/TP — is watched
//       tick-by-tick. Each focus tick is pushed to the frontend via a throttled
//       `market-tick` Tauri event. The order/position union matters because
//       "Libérer" (releasing a zone) drops a symbol from the displayed set but
//       must never degrade a still-pending order down to 1-minute-bar
//       granularity — see `strict_watch_symbols`.
//
// There is no "universe" notion anymore: the broad tier subscribes with the `*`
// wildcard (the whole US market) and each strategy filters in its own script.
// The wildcard also sidesteps the code-400 "invalid syntax" rejection that an
// enumerated 13k-symbol list could trigger on a single exotic ticker.
//
// ALL OF THE ABOVE DESCRIBES THE PAID (SIP) PLAN. `alpaca::plan::probe` asks Alpaca
// which subscription the key holds before this module connects, and the free (IEX)
// plan gets a different shape entirely: no broad tier, and a hard budget of 30
// SUBSCRIPTIONS. That unit is the trap — Alpaca counts (symbol × channel) pairs, not
// distinct symbols, so 20 names on trades+quotes+bars is 60 and the venue answers
// `code=405 "symbol limit exceeded"` after acking only the first channel.
//
// The budget is therefore SPLIT, never replicated: `bars` is dropped (the aggregator
// builds candles from trades anyway), `quotes` are reserved for names an order could
// be sent on, and `trades` take the rest. Priority inside each is
// `capped_watch_list`: orders and positions first, then the charts on screen, then
// the day's Panic Mean Reversion watchlist and pins. Dismissing a row frees its slot
// on the next reconcile pass, via the same subscribe/unsubscribe diff
// (`sync_channel`) that has always driven the focus tier.
//
// None of this touches Market Replay: the stream never runs under a simulated clock,
// and replayed history comes from flat files + REST.

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

use crate::alpaca::plan::Plan;
use crate::broker::BrokerMirror;
use crate::internal_trading::InternalBook;
use crate::market_state::MarketState;

/// Alpaca's wildcard token: subscribes a channel to every symbol in the market.
/// Alpaca staff confirm this streams at the same rate as an explicit symbol list
/// with no server overhead — and, crucially, removes the "one malformed symbol
/// rejects the whole subscribe frame with code 400" failure mode.
const WILDCARD: &str = "*";
/// Alpaca caps subscribe payloads; chunk symbols to stay well under any limit.
const SUBSCRIBE_CHUNK: usize = 500;
/// Small pause between chunked subscribe messages, in case the broker rate-limits
/// rapid control frames.
const CHUNK_DELAY: Duration = Duration::from_millis(100);
/// Backoff between reconnect attempts after a transient drop.
const RECONNECT_DELAY: Duration = Duration::from_secs(3);
/// How often to re-evaluate the broad-tier mode (catches the 09:30 boundary).
const BROAD_REEVAL: Duration = Duration::from_secs(30);
/// Min interval between pushed ticks per focus symbol (coalesce to the latest).
const TICK_THROTTLE: Duration = Duration::from_millis(100);

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
type WsWrite = SplitSink<Ws, Message>;

/// Deserialize a field that is a number in some message types but a non-number
/// in others, without failing the whole frame. Alpaca reuses the `c` key: it's a
/// bar's CLOSE (number) but a trade/quote's CONDITIONS (array of strings). A
/// plain `Option<f64>` makes serde reject every trade/quote frame (→ silently 0
/// trades/quotes while bars work); this keeps the number and ignores anything else.
fn de_lenient_f64<'de, D>(d: D) -> Result<Option<f64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = Option::<serde_json::Value>::deserialize(d)?;
    Ok(v.and_then(|v| v.as_f64()))
}

/// One decoded message from the data stream. Unused fields are ignored.
#[derive(Debug, Deserialize)]
struct StreamMsg {
    #[serde(rename = "T")]
    kind: String,
    #[serde(rename = "S")]
    symbol: Option<String>,
    // trade
    #[serde(rename = "p")]
    price: Option<f64>,
    #[serde(rename = "s")]
    size: Option<u64>,
    // quote
    #[serde(rename = "bp")]
    bid_price: Option<f64>,
    #[serde(rename = "ap")]
    ask_price: Option<f64>,
    // bar (minute) — o/h/l/c/v/vw
    #[serde(rename = "o")]
    open: Option<f64>,
    #[serde(rename = "h")]
    high: Option<f64>,
    #[serde(rename = "l")]
    low: Option<f64>,
    // `c` is the bar close (number) but the trade/quote conditions (array): parse
    // leniently so a trade/quote frame doesn't fail to deserialize entirely.
    #[serde(rename = "c", default, deserialize_with = "de_lenient_f64")]
    close: Option<f64>,
    #[serde(rename = "v")]
    bar_volume: Option<u64>,
    #[serde(rename = "vw")]
    bar_vwap: Option<f64>,
    // bar trade count (number of trades aggregated into the minute bar)
    #[serde(rename = "n")]
    bar_trades: Option<u64>,
    // shared timestamp (RFC3339, nanos)
    #[serde(rename = "t")]
    timestamp: Option<String>,
    // control messages
    msg: Option<String>,
    code: Option<i64>,
    // subscription ack — Alpaca echoes the full active symbol list per channel.
    #[serde(rename = "trades")]
    sub_trades: Option<Vec<String>>,
    #[serde(rename = "quotes")]
    sub_quotes: Option<Vec<String>>,
    #[serde(rename = "bars")]
    sub_bars: Option<Vec<String>>,
}

/// Tick pushed to the frontend for a displayed (focus) symbol.
#[derive(Debug, Clone, Serialize)]
struct TickEvent {
    symbol: String,
    price: f64,
    /// Unix seconds (matches lightweight-charts time).
    ts: i64,
}

/// Current subscriptions on the socket, per channel.
#[derive(Default)]
struct Subs {
    trades: HashSet<String>,
    quotes: HashSet<String>,
    bars: HashSet<String>,
}

/// Run the live feed until `running` is cleared. Reconnects on transient drops;
/// bails out (clearing `running`) on fatal auth errors so we don't hammer Alpaca.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    market: Arc<RwLock<MarketState>>,
    // Read on the free plan to spend its 30 slots on the day's watchlist; unused on
    // SIP, where the broad tier is the `*` wildcard.
    db: Arc<Mutex<rusqlite::Connection>>,
    internal_book: Arc<RwLock<InternalBook>>,
    broker_mirror: Arc<RwLock<BrokerMirror>>,
    feed: String,
    plan: Plan,
    key: String,
    secret: String,
    warn_ms: u32,
    critical_ms: u32,
    running: Arc<AtomicBool>,
    focus_rx: watch::Receiver<Vec<String>>,
    app: AppHandle,
) {
    let url = format!("wss://stream.data.alpaca.markets/v2/{feed}");
    eprintln!(
        "[tagdash] live feed: connecting to {url} (abonnement {}, {})",
        plan.as_str(),
        match plan.symbol_limit() {
            Some(n) => format!("{n} symboles max"),
            None => "sans limite de symboles".to_string(),
        },
    );
    {
        let mut ms = market.write().unwrap();
        ms.feed_set_feed(&feed);
        ms.feed_set_plan(plan.as_str(), plan.symbol_limit().unwrap_or(0));
        // Real prints: prints/second is a genuine trade rate here.
        ms.set_tape_kind(crate::types::TapeKind::Prints);
        ms.feed_state("connecting");
    }

    let mut focus_rx = focus_rx;

    while running.load(Ordering::Relaxed) {
        match connect_async(&url).await {
            Ok((ws, _resp)) => {
                let code = session(
                    ws,
                    &key,
                    &secret,
                    &market,
                    &db,
                    &internal_book,
                    &broker_mirror,
                    plan,
                    warn_ms,
                    critical_ms,
                    &running,
                    &mut focus_rx,
                    &app,
                )
                .await;
                market.write().unwrap().live_running = false;

                if let Some(c) = code {
                    if matches!(c, 402 | 403 | 404 | 406 | 409) {
                        eprintln!("[tagdash] live feed: fatal error code={c} — stopping feed");
                        running.store(false, Ordering::Relaxed);
                        break;
                    }
                }
            }
            Err(e) => {
                eprintln!("[tagdash] live feed: connect failed: {e}");
                market
                    .write()
                    .unwrap()
                    .feed_error(0, Some(format!("connect failed: {e}")));
            }
        }

        if running.load(Ordering::Relaxed) {
            eprintln!("[tagdash] live feed: reconnecting in {RECONNECT_DELAY:?}");
            market.write().unwrap().feed_reconnecting();
            sleep(RECONNECT_DELAY).await;
        }
    }
    market.write().unwrap().live_running = false;
    market.write().unwrap().feed_state("stopped");
    eprintln!("[tagdash] live feed: stopped");
}

/// Drive one connection: auth → reconcile subscriptions → dispatch, re-reconciling
/// on focus change and on the broad-mode timer. Returns `Some(code)` on a server
/// error, `None` on a transient disconnect / clean close.
#[allow(clippy::too_many_arguments)]
async fn session(
    ws: Ws,
    key: &str,
    secret: &str,
    market: &Arc<RwLock<MarketState>>,
    db: &Arc<Mutex<rusqlite::Connection>>,
    internal_book: &Arc<RwLock<InternalBook>>,
    broker_mirror: &Arc<RwLock<BrokerMirror>>,
    plan: Plan,
    warn_ms: u32,
    critical_ms: u32,
    running: &Arc<AtomicBool>,
    focus_rx: &mut watch::Receiver<Vec<String>>,
    app: &AppHandle,
) -> Option<i64> {
    let (mut write, mut read) = ws.split();

    // Authenticate.
    let auth = json!({ "action": "auth", "key": key, "secret": secret }).to_string();
    if write.send(Message::Text(auth)).await.is_err() {
        return None; // transient
    }

    let mut authenticated = false;
    let mut subs = Subs::default();
    let mut focus: HashSet<String> = clean_symbols(focus_rx.borrow().iter());
    let mut last_emit: HashMap<String, Instant> = HashMap::new();
    let mut reeval = interval(BROAD_REEVAL);

    loop {
        if !running.load(Ordering::Relaxed) {
            return None;
        }

        tokio::select! {
            biased;

            // Displayed symbols changed → reconcile focus subscriptions.
            changed = focus_rx.changed() => {
                if changed.is_err() { continue; }
                focus = clean_symbols(focus_rx.borrow_and_update().iter());
                if authenticated
                    && reconcile(&mut write, market, db, internal_book, broker_mirror, &focus, plan, broad_mode_now(), &mut subs).await.is_err()
                {
                    return None;
                }
            }

            // Periodic broad-mode re-evaluation (catches the 09:30 switch, and
            // promotes/demotes symbols as orders/positions open and close on
            // released tickers without waiting for a chart-focus change).
            _ = reeval.tick() => {
                if authenticated
                    && reconcile(&mut write, market, db, internal_book, broker_mirror, &focus, plan, broad_mode_now(), &mut subs).await.is_err()
                {
                    return None;
                }
            }

            frame = read.next() => {
                let frame = match frame {
                    Some(Ok(f))  => f,
                    Some(Err(e)) => { eprintln!("[tagdash] live feed: read error: {e}"); return None; }
                    None         => return None,
                };
                let text = match frame {
                    Message::Text(t)   => t,
                    Message::Binary(b) => String::from_utf8_lossy(&b).into_owned(),
                    Message::Ping(_) | Message::Pong(_) => continue,
                    Message::Close(_)  => return None,
                    _ => continue,
                };
                let msgs: Vec<StreamMsg> = match serde_json::from_str(&text) {
                    Ok(m)  => m,
                    Err(_) => continue,
                };

                // Control pass (auth / subscription ack / errors).
                for m in &msgs {
                    match m.kind.as_str() {
                        "success" if m.msg.as_deref() == Some("authenticated") => {
                            authenticated = true;
                            market.write().unwrap().feed_authenticated();
                            if reconcile(&mut write, market, db, internal_book, broker_mirror, &focus, plan, broad_mode_now(), &mut subs).await.is_err() {
                                return None;
                            }
                            eprintln!("[tagdash] live feed: authenticated, broad mode {}", broad_mode_now());
                        }
                        "subscription" => {
                            // Alpaca echoes the FULL active subscription per channel.
                            // This is the ground truth for whether the focus
                            // trades/quotes actually registered (vs being silently
                            // dropped for a bad symbol format).
                            let ack = format!(
                                "trades {} · quotes {} · bars {}",
                                fmt_sub(&m.sub_trades),
                                fmt_sub(&m.sub_quotes),
                                fmt_sub(&m.sub_bars),
                            );
                            eprintln!("[tagdash] live feed: subscription ack — {ack}");
                            let mut ms = market.write().unwrap();
                            ms.live_running = true;
                            ms.feed_set_subscription_ack(&ack);
                        }
                        "error" => {
                            let code = m.code.unwrap_or(0);
                            eprintln!("[tagdash] live feed: server error code={code} msg={:?}", m.msg);
                            market.write().unwrap().feed_error(code, m.msg.clone());
                            return Some(code);
                        }
                        _ => {}
                    }
                }

                // Data pass — one write lock for the whole frame; collect focus
                // ticks to emit after releasing the lock.
                if authenticated && msgs.iter().any(|m| matches!(m.kind.as_str(), "t" | "q" | "b")) {
                    let now = Utc::now();
                    let t_count = msgs.iter().filter(|m| m.kind == "t").count() as u64;
                    let q_count = msgs.iter().filter(|m| m.kind == "q").count() as u64;
                    let b_count = msgs.iter().filter(|m| m.kind == "b").count() as u64;
                    let mut ticks: HashMap<String, TickEvent> = HashMap::new();
                    {
                        let mut ms = market.write().unwrap();
                        for m in &msgs {
                            match m.kind.as_str() {
                                "t" => {
                                    if let (Some(sym), Some(price)) = (m.symbol.as_deref(), m.price) {
                                        let event_time = parse_ts(m.timestamp.as_deref());
                                        ms.on_trade(sym, price, m.size.unwrap_or(0), event_time, now, warn_ms, critical_ms);
                                        // Trade tape: fine-granularity record of the day so
                                        // it can be replayed tick-by-tick later (fire-and-
                                        // forget channel send — see replay::tape).
                                        crate::replay::tape::record_trade(sym, price, m.size.unwrap_or(0), event_time);
                                        if focus.contains(sym) {
                                            ticks.insert(sym.to_string(), TickEvent {
                                                symbol: sym.to_string(),
                                                price,
                                                ts: event_time.timestamp(),
                                            });
                                        }
                                    }
                                }
                                "q" => {
                                    if let (Some(sym), Some(bid), Some(ask)) =
                                        (m.symbol.as_deref(), m.bid_price, m.ask_price)
                                    {
                                        let event_time = parse_ts(m.timestamp.as_deref());
                                        ms.on_quote(sym, bid, ask, now, event_time);
                                        crate::replay::tape::record_quote(sym, bid, ask, event_time);
                                    }
                                }
                                "b" => {
                                    if let (Some(sym), Some(o), Some(h), Some(l), Some(c)) =
                                        (m.symbol.as_deref(), m.open, m.high, m.low, m.close)
                                    {
                                        // Always ingest the 1-min bar — including for focus
                                        // symbols. The wildcard `bars` stream is the baseline
                                        // that keeps a displayed chart moving every closed
                                        // minute even when the symbol is too quiet to print
                                        // trades; the trade ticks add finer-grained updates on
                                        // top when prints do come in.
                                        let bar_time = parse_ts(m.timestamp.as_deref());
                                        ms.on_bar(
                                            sym, bar_time, o, h, l, c,
                                            m.bar_volume.unwrap_or(0),
                                            m.bar_vwap.unwrap_or(c),
                                            m.bar_trades,
                                            now,
                                        );
                                    }
                                }
                                _ => {}
                            }
                        }
                        ms.feed_record_data(t_count, q_count, b_count, now);
                    }

                    // Push throttled ticks to the frontend (outside the lock).
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

/// Symbols that must stay tick-by-tick regardless of what's on screen: anything
/// carrying a pending order or an open position in the internal book. Without
/// this, releasing a ticker's chart zone (Libérer) drops it out of the focus
/// tier and its resting limit/SL/TP is only re-evaluated once a minute (the
/// broad tier's bar granularity) instead of against the real price path — which
/// reads as the order firing "immediately" at a stale/gapped price instead of
/// respecting the limit.
fn strict_watch_symbols(
    internal_book: &RwLock<InternalBook>,
    broker_mirror: &RwLock<BrokerMirror>,
) -> HashSet<String> {
    let book = internal_book.read().unwrap();
    let mut symbols: HashSet<String> = book
        .get_pending_orders()
        .into_iter()
        .map(|o| o.symbol)
        .chain(book.positions.keys().cloned())
        .collect();
    drop(book);
    let broker = broker_mirror.read().unwrap();
    symbols.extend(broker.positions.keys().cloned());
    symbols.extend(
        broker
            .orders
            .values()
            .filter(|o| o.status.is_live())
            .map(|o| o.symbol.clone()),
    );
    symbols
}

/// The effective focus set to subscribe tick-by-tick: displayed chart symbols
/// union `strict_watch_symbols`.
pub(crate) fn watch_set(
    focus: &HashSet<String>,
    internal_book: &RwLock<InternalBook>,
    broker_mirror: &RwLock<BrokerMirror>,
) -> HashSet<String> {
    let mut set = focus.clone();
    set.extend(strict_watch_symbols(internal_book, broker_mirror));
    set
}

/// The symbols a capped plan spends its slots on, most important first.
///
/// The free plan streams `IEX_SYMBOL_LIMIT` symbols and no more, so this is a budget
/// and the order is the whole design:
///
///   1. anything carrying a pending order or an open position — real money, never
///      traded away for a screener row;
///   2. the charts actually on screen;
///   3. the day's Panic Mean Reversion watchlist plus the user's pins, which is what
///      the budget exists for: with no wildcard the scanner discovers nothing, so
///      the remaining slots must go to names already chosen by the strategy.
///
/// Dismissed rows are excluded, which is what makes the ✕ in the pre-open panel
/// actually free a slot — `sync_channel` then sends the unsubscribe on the next pass.
///
/// Truncation is reported: over-budget at step 1 means orders themselves don't fit,
/// and silently dropping one would leave a live SL unwatched.
pub(crate) fn capped_watch_list(
    focus: &HashSet<String>,
    internal_book: &RwLock<InternalBook>,
    broker_mirror: &RwLock<BrokerMirror>,
    db: &Mutex<rusqlite::Connection>,
    limit: usize,
) -> HashSet<String> {
    let strict = strict_list(internal_book, broker_mirror);
    let focus = focus_list(focus);

    // Watchlist fill — only worth a DB round trip if slots could remain.
    let mut fill: Vec<String> = Vec::new();
    if strict.len() + focus.len() < limit {
        let day = crate::time::et_date(crate::time::now());
        let (pins, scores, dismissed) = {
            let conn = db.lock().unwrap();
            (
                crate::local_db::screener_repository::pins(&conn, false),
                crate::local_db::scoring_repository::get_top(&conn, &day, limit as u32)
                    .unwrap_or_default(),
                crate::local_db::screener_repository::dismissals(&conn, &day),
            )
        };
        let dismissed: HashSet<String> = dismissed.into_iter().collect();
        // Pins first: an explicit "keep watching this" outranks today's ranking.
        fill = pins
            .into_iter()
            .chain(scores.into_iter().map(|r| r.symbol))
            .filter(|s| !dismissed.contains(s))
            .collect();
    }

    if strict.len() > limit {
        eprintln!(
            "[tagdash] live feed: {} ordres/positions pour {limit} places — \
             au-delà de la limite du plan gratuit, {} ne seront pas suivis en direct",
            strict.len(),
            strict.len() - limit,
        );
    }
    budget([&strict, &focus, &fill], limit)
}

/// How many of a capped plan's slots are reserved for `quotes`.
///
/// A third. Quotes are only useful where an order might be sent, and that is a
/// handful of names at a time — the reservation exists to stop a screen full of
/// charts from eating the budget the watchlist needs for trades. Since `budget`
/// takes only what exists, four open charts spend four slots, not ten.
fn quote_slots(limit: usize) -> usize {
    (limit / 3).max(1)
}

/// Symbols carrying a position or a live order, as an ordered list.
fn strict_list(
    internal_book: &RwLock<InternalBook>,
    broker_mirror: &RwLock<BrokerMirror>,
) -> Vec<String> {
    strict_watch_symbols(internal_book, broker_mirror)
        .into_iter()
        .collect()
}

fn focus_list(focus: &HashSet<String>) -> Vec<String> {
    focus.iter().cloned().collect()
}

/// Spend `limit` slots across priority tiers, best first.
///
/// Normalises to Alpaca's upper-case form, drops blanks, keeps the first occurrence
/// of a repeat (so a symbol that is both held and on screen costs one slot), and
/// truncates the tail. Pure, so the priority rule is testable without a socket or a
/// database — which matters, because getting the order wrong silently trades a live
/// stop-loss for a screener row.
fn budget(tiers: [&[String]; 3], limit: usize) -> HashSet<String> {
    let mut out: HashSet<String> = HashSet::with_capacity(limit);
    for tier in tiers {
        for sym in tier {
            if out.len() >= limit {
                return out;
            }
            let s = sym.trim().to_uppercase();
            if !s.is_empty() {
                out.insert(s);
            }
        }
    }
    out
}

/// Reconcile the three channel subscriptions.
///
/// On SIP the broad surveillance tier covers the whole US market via the `*`
/// wildcard (one tiny message instead of ~13k enumerated symbols), so a single
/// exotic symbol can no longer get the frame rejected with code 400. The focus tier
/// (displayed symbols, plus anything with a live order/position — see `watch_set`)
/// is subscribed explicitly so it stays tick-by-tick.
///
/// On the free plan there is no broad tier at all. The wildcard is both over the
/// 30-symbol cap and pointless — IEX prints are too thin a slice of the tape to
/// discover a mover — so ONE budgeted set (see `capped_watch_list`) is subscribed on
/// every channel. Every symbol therefore arrives tick-by-tick, which is what Panic
/// Mean Reversion needs; what is lost is discovery, which the free plan could not do
/// anyway.
#[allow(clippy::too_many_arguments)]
async fn reconcile(
    write: &mut WsWrite,
    market: &Arc<RwLock<MarketState>>,
    db: &Arc<Mutex<rusqlite::Connection>>,
    internal_book: &Arc<RwLock<InternalBook>>,
    broker_mirror: &Arc<RwLock<BrokerMirror>>,
    focus: &HashSet<String>,
    plan: Plan,
    broad_mode: &str,
    current: &mut Subs,
) -> Result<(), WsError> {
    let watch = watch_set(focus, internal_book, broker_mirror);

    // Desired sets per channel.
    let (trades_d, quotes_d, bars_d, broad_count): (
        HashSet<String>,
        HashSet<String>,
        HashSet<String>,
        usize,
    ) = match plan.symbol_limit() {
        // Capped plan: the budget is SPLIT across channels, not replicated on each.
        //
        // Alpaca counts (symbol × channel) SUBSCRIPTIONS, not distinct symbols —
        // measured, not assumed: subscribing the same 20 names to trades, quotes and
        // bars sent 60 against a 30 budget and the venue answered
        // `code=405 "symbol limit exceeded"`, after acking only the trades. So:
        //
        //   • `bars` is dropped entirely. The aggregator already builds candles from
        //     trades, so a bars subscription buys nothing here and costs real slots.
        //   • `quotes` go only to names an order could actually be sent on (charts on
        //     screen, plus anything carrying a position or order): bid/ask matters at
        //     execution, not for surveillance.
        //   • `trades` take everything left, so the whole watchlist stays
        //     tick-by-tick — which is what Panic Mean Reversion needs.
        Some(limit) => {
            let quotes = budget(
                [
                    &strict_list(internal_book, broker_mirror),
                    &focus_list(focus),
                    &[],
                ],
                quote_slots(limit),
            );
            let trades = capped_watch_list(
                focus,
                internal_book,
                broker_mirror,
                db,
                limit.saturating_sub(quotes.len()),
            );
            let spent = trades.len() + quotes.len();
            debug_assert!(spent <= limit, "budget dépassé : {spent} > {limit}");
            (trades, quotes, HashSet::new(), spent)
        }
        None if broad_mode == "trades" => {
            // Premarket: every trade (the wildcard already covers the focus
            // symbols' trades, which drive the tick events); quotes for focus.
            (
                std::iter::once(WILDCARD.to_string()).collect(),
                watch.clone(),
                HashSet::new(),
                0,
            )
        }
        None => {
            // Open: every 1-min bar (focus symbols' bars are ignored in the data
            // pass); trades+quotes for focus → tick-by-tick charts.
            (
                watch.clone(),
                watch.clone(),
                std::iter::once(WILDCARD.to_string()).collect(),
                0,
            )
        }
    };

    sync_channel(write, market, "trades", &mut current.trades, &trades_d).await?;
    sync_channel(write, market, "quotes", &mut current.quotes, &quotes_d).await?;
    sync_channel(write, market, "bars", &mut current.bars, &bars_d).await?;

    // On SIP the broad tier is the whole market, reported as 0 — the panel renders
    // that as "tout le marché (✱)". On a capped plan the count IS the budget spent.
    market
        .write()
        .unwrap()
        .feed_reconcile(broad_mode, broad_count, watch.len());
    Ok(())
}

/// Diff `desired` against `current` for one channel and send the unsubscribe /
/// subscribe deltas, then adopt `desired` as the new current set.
async fn sync_channel(
    write: &mut WsWrite,
    market: &Arc<RwLock<MarketState>>,
    channel: &str,
    current: &mut HashSet<String>,
    desired: &HashSet<String>,
) -> Result<(), WsError> {
    let to_remove: Vec<String> = current.difference(desired).cloned().collect();
    let to_add: Vec<String> = desired.difference(current).cloned().collect();
    if !to_remove.is_empty() {
        send_channel(write, market, "unsubscribe", channel, &to_remove).await?;
    }
    if !to_add.is_empty() {
        send_channel(write, market, "subscribe", channel, &to_add).await?;
    }
    *current = desired.clone();
    Ok(())
}

/// Send a subscribe / unsubscribe for a single channel, chunked + paced under the
/// limit. Each message's channel/index/size is recorded in the diagnostics so the
/// panel reveals exactly which message Alpaca rejected when an error follows.
async fn send_channel(
    write: &mut WsWrite,
    market: &Arc<RwLock<MarketState>>,
    action: &str,
    channel: &str,
    symbols: &[String],
) -> Result<(), WsError> {
    let total = symbols.len().div_ceil(SUBSCRIBE_CHUNK);
    for (i, chunk) in symbols.chunks(SUBSCRIBE_CHUNK).enumerate() {
        if i > 0 {
            sleep(CHUNK_DELAY).await;
        }
        let mut obj = serde_json::Map::new();
        obj.insert("action".to_string(), json!(action));
        obj.insert(channel.to_string(), json!(chunk));
        let payload = serde_json::Value::Object(obj).to_string();
        let label = if chunk.iter().any(|s| s == WILDCARD) {
            format!("{action} {channel} (✱ tout le marché)")
        } else {
            format!(
                "{action} {channel} msg {}/{} ({} symbols)",
                i + 1,
                total,
                chunk.len()
            )
        };
        eprintln!(
            "[tagdash] live feed: → {label} (e.g. {:?})",
            &chunk[..chunk.len().min(3)]
        );
        market.write().unwrap().feed_set_last_subscribe(&label);
        write.send(Message::Text(payload)).await?;
    }
    Ok(())
}

/// Normalise focus symbols before subscribing: trim and upper-case (Alpaca
/// tickers are upper-case), dropping empties. The broad tier uses the `*`
/// wildcard so it doesn't care, but `trades`/`quotes` subscriptions only deliver
/// data when the symbol matches exactly — a stray space or lower-case letter
/// silently yields zero ticks with no error.
pub(crate) fn clean_symbols<'a>(it: impl Iterator<Item = &'a String>) -> HashSet<String> {
    it.map(|s| s.trim().to_uppercase())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Format one channel's confirmed subscription for the diagnostics panel:
/// the wildcard as "✱", otherwise the count plus a short sample.
fn fmt_sub(list: &Option<Vec<String>>) -> String {
    match list {
        Some(v) if v.iter().any(|s| s == WILDCARD) => "✱".to_string(),
        Some(v) if !v.is_empty() => {
            let sample: Vec<&str> = v.iter().take(4).map(String::as_str).collect();
            format!("{} {sample:?}", v.len())
        }
        _ => "0".to_string(),
    }
}

/// Broad-tier channel for now: `trades` while the consolidated tape is thin,
/// `bars` (1-minute) otherwise. ET is DST-aware (see `crate::time`).
///
/// The market-wide `trades` wildcard is what lets a discovery scanner such as Micro
/// Pullback build 10-second candles for symbols nobody is watching. It is affordable
/// in the premarket (04:00–09:30) and after the close (16:00–20:00), where the tape
/// is a trickle. It is NOT affordable during the regular session: the full SIP tape
/// at midday runs to tens of thousands of messages a second, which would swamp the
/// ingestion path for a strategy that only cares about a handful of small caps.
///
/// So between 09:30 and 16:00 the broad tier drops to 1-minute `bars` and the
/// per-second detail comes only from the focus tier — the day's watch list, orders,
/// positions and charts on screen (see `capped_watch_list`). Discovery is genuinely
/// narrower in that window on this provider; the Massive `A.*` wildcard, at one
/// message per symbol per second, carries the universe all day instead.
fn broad_mode_now() -> &'static str {
    broad_mode_at(crate::time::et_minutes(Utc::now()))
}

/// `broad_mode_now` split out on its ET minute-of-day, so the schedule is testable.
fn broad_mode_at(et_min: u32) -> &'static str {
    const PREMARKET: std::ops::Range<u32> = 240..570; // 04:00–09:30
    const AFTERHOURS: std::ops::Range<u32> = 960..1200; // 16:00–20:00
    if PREMARKET.contains(&et_min) || AFTERHOURS.contains(&et_min) {
        "trades"
    } else {
        "bars"
    }
}

/// Parse an Alpaca RFC3339 timestamp; fall back to now if absent/unparseable.
fn parse_ts(ts: Option<&str>) -> DateTime<Utc> {
    ts.and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(Utc::now)
}

#[cfg(test)]
mod broad_tier_tests {
    use super::broad_mode_at;

    // The market-wide `trades` wildcard is what lets a discovery scanner see symbols
    // nobody is watching. It is taken only where the consolidated tape is thin.
    #[test]
    fn the_whole_market_tape_is_taken_only_when_it_is_thin() {
        // Premarket 04:00–09:30 and afterhours 16:00–20:00.
        for et_min in [240, 400, 569, 960, 1100, 1199] {
            assert_eq!(broad_mode_at(et_min), "trades", "ET minute {et_min}");
        }
        // The regular session would be tens of thousands of messages a second, and
        // overnight there is nothing to stream.
        for et_min in [570, 700, 959, 1200, 0, 239] {
            assert_eq!(broad_mode_at(et_min), "bars", "ET minute {et_min}");
        }
    }

    // The boundaries are the ones the rest of the app reasons on — an off-by-one here
    // would leave micro_pullback blind for a minute at exactly the wrong time.
    #[test]
    fn boundaries_are_half_open_on_the_session_edges() {
        assert_eq!(broad_mode_at(239), "bars"); // 03:59
        assert_eq!(broad_mode_at(240), "trades"); // 04:00
        assert_eq!(broad_mode_at(569), "trades"); // 09:29
        assert_eq!(broad_mode_at(570), "bars"); // 09:30
        assert_eq!(broad_mode_at(959), "bars"); // 15:59
        assert_eq!(broad_mode_at(960), "trades"); // 16:00
        assert_eq!(broad_mode_at(1199), "trades"); // 19:59
        assert_eq!(broad_mode_at(1200), "bars"); // 20:00
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Side;

    fn book_with_pending_order(symbol: &str) -> InternalBook {
        let mut book = InternalBook::new();
        book.create_limit_order(
            None,
            "z1".into(),
            symbol.into(),
            "strat".into(),
            Side::Long,
            100,
            10.00,
            Some(9.50),
            None,
            None,
        );
        book
    }

    #[test]
    fn strict_watch_includes_pending_order_symbol() {
        let book = RwLock::new(book_with_pending_order("AAA"));
        let broker = RwLock::new(BrokerMirror::default());
        assert!(strict_watch_symbols(&book, &broker).contains("AAA"));
    }

    #[test]
    fn strict_watch_includes_open_position_symbol() {
        let mut book = InternalBook::new();
        book.execute_market_fill(
            None,
            "z1".into(),
            "BBB".into(),
            "strat".into(),
            Side::Long,
            100,
            10.00,
            Some(9.50),
            None,
        );
        let book = RwLock::new(book);
        let broker = RwLock::new(BrokerMirror::default());
        assert!(strict_watch_symbols(&book, &broker).contains("BBB"));
    }

    #[test]
    fn strict_watch_excludes_cancelled_order_symbol() {
        let mut book = book_with_pending_order("CCC");
        let order_id = book.get_pending_orders()[0].order_id.clone();
        book.cancel_order(&order_id).unwrap();
        let book = RwLock::new(book);
        let broker = RwLock::new(BrokerMirror::default());
        assert!(!strict_watch_symbols(&book, &broker).contains("CCC"));
    }

    // The exact bug reported: releasing a ticker (removing it from the
    // displayed/focus set) must not drop it out of the effective watch set
    // while an order is still pending on it — otherwise it degrades to
    // 1-minute-bar monitoring and the limit fires against a stale/gapped
    // price instead of the real path.
    #[test]
    fn watch_set_keeps_released_ticker_with_pending_order() {
        let book = RwLock::new(book_with_pending_order("DDD"));
        let broker = RwLock::new(BrokerMirror::default());
        let focus: HashSet<String> = HashSet::new(); // ticker released: not displayed
        assert!(
            watch_set(&focus, &book, &broker).contains("DDD"),
            "a released ticker with a pending order must stay tick-watched"
        );
    }

    #[test]
    fn watch_set_drops_symbol_once_flat_and_undisplayed() {
        let book = RwLock::new(InternalBook::new()); // no orders, no positions
        let broker = RwLock::new(BrokerMirror::default());
        let focus: HashSet<String> = HashSet::new();
        assert!(watch_set(&focus, &book, &broker).is_empty());
    }

    #[test]
    fn alpaca_position_is_always_tick_watched() {
        let book = RwLock::new(InternalBook::new());
        let mut mirror = BrokerMirror::default();
        mirror.positions.insert(
            "ALPA".into(),
            crate::broker::BrokerPosition {
                symbol: "ALPA".into(),
                quantity: 10.0,
                avg_entry_price: 5.0,
                market_price: None,
                market_value: None,
                cost_basis: None,
                unrealized_pnl: None,
                unrealized_pnl_pct: None,
            },
        );
        let broker = RwLock::new(mirror);
        assert!(strict_watch_symbols(&book, &broker).contains("ALPA"));
    }

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// The whole point of the budget: when the free plan's slots run out, an open
    /// position keeps its stream and a screener row loses it — never the reverse.
    #[test]
    fn orders_and_positions_outrank_everything_else() {
        let held = v(&["HELD1", "HELD2"]);
        let on_screen = v(&["CHART1", "CHART2"]);
        let watchlist = v(&["PMR1", "PMR2", "PMR3"]);

        let got = budget([&held, &on_screen, &watchlist], 3);
        assert_eq!(got.len(), 3);
        assert!(
            got.contains("HELD1") && got.contains("HELD2"),
            "money at risk stays: {got:?}"
        );
        assert!(got.contains("CHART1"), "then the charts on screen");
        assert!(!got.contains("PMR1"), "the watchlist yields the last slot");
    }

    /// A symbol that is both held and displayed must not burn two of the thirty.
    #[test]
    fn a_symbol_in_several_tiers_costs_one_slot() {
        let held = v(&["AAA"]);
        let on_screen = v(&["aaa", " AAA "]); // lower case / padded — same ticker
        let watchlist = v(&["BBB"]);
        let got = budget([&held, &on_screen, &watchlist], 2);
        assert_eq!(got, ["AAA", "BBB"].iter().map(|s| s.to_string()).collect());
    }

    /// Alpaca matches tickers exactly: a stray space or lower-case letter yields
    /// zero ticks with no error at all, so normalisation is not cosmetic.
    #[test]
    fn symbols_are_normalised_and_blanks_dropped() {
        let messy = v(&["  tsla ", "", "   ", "Nvda"]);
        let empty: Vec<String> = Vec::new();
        let got = budget([&messy, &empty, &empty], 10);
        assert_eq!(
            got,
            ["TSLA", "NVDA"].iter().map(|s| s.to_string()).collect()
        );
    }

    #[test]
    fn a_budget_that_fits_keeps_everything_and_a_zero_budget_keeps_nothing() {
        let a = v(&["AAA"]);
        let b = v(&["BBB"]);
        let c = v(&["CCC"]);
        assert_eq!(budget([&a, &b, &c], 10).len(), 3);
        assert!(budget([&a, &b, &c], 0).is_empty());
    }

    /// The paid plan has no cap, so the wildcard stays available; the free plan must
    /// never ask for it — Alpaca would reject the frame AND it discovers nothing.
    #[test]
    fn only_the_paid_plan_may_stream_the_whole_market() {
        assert!(Plan::Sip.allows_wildcard() && Plan::Sip.symbol_limit().is_none());
        assert!(!Plan::Iex.allows_wildcard());
        assert_eq!(
            Plan::Iex.symbol_limit(),
            Some(crate::alpaca::plan::IEX_SYMBOL_LIMIT)
        );
    }

    /// THE regression this file exists to prevent. Alpaca counts (symbol × channel)
    /// pairs: the first shipped version put the same 20 names on trades, quotes AND
    /// bars — 60 against a 30 budget — and the venue killed the session with
    /// `code=405 "symbol limit exceeded"`, so no data arrived at all.
    #[test]
    fn the_capped_budget_counts_subscriptions_not_distinct_symbols() {
        let limit = crate::alpaca::plan::IEX_SYMBOL_LIMIT;
        let names: Vec<String> = (0..50).map(|i| format!("SYM{i}")).collect();

        let quotes = budget([&names, &[], &[]], quote_slots(limit));
        let trades = budget([&names, &[], &[]], limit - quotes.len());

        assert!(
            trades.len() + quotes.len() <= limit,
            "{} trades + {} quotes dépasse {limit}",
            trades.len(),
            quotes.len(),
        );
        assert!(
            !trades.is_empty() && !quotes.is_empty(),
            "les deux canaux servent"
        );
    }

    /// Quotes are reserved, not greedy: a screen full of charts must not starve the
    /// watchlist of its trades subscriptions.
    #[test]
    fn quotes_never_eat_more_than_their_reservation() {
        let limit = 30;
        let many_charts: Vec<String> = (0..40).map(|i| format!("CHART{i}")).collect();
        let quotes = budget([&many_charts, &[], &[]], quote_slots(limit));
        assert_eq!(quotes.len(), 10, "un tiers de 30");
        assert_eq!(
            limit - quotes.len(),
            20,
            "il reste 20 places pour les trades"
        );
    }

    /// …and it takes only what exists: four open charts cost four slots, not ten.
    #[test]
    fn a_small_focus_set_leaves_the_rest_to_trades() {
        let limit = 30;
        let charts: Vec<String> = (0..4).map(|i| format!("CHART{i}")).collect();
        let quotes = budget([&charts, &[], &[]], quote_slots(limit));
        assert_eq!(quotes.len(), 4);
        assert_eq!(limit - quotes.len(), 26);
    }

    /// A one-slot plan must still produce a valid split rather than dividing to zero.
    #[test]
    fn a_tiny_budget_still_reserves_one_quote_slot() {
        assert_eq!(quote_slots(1), 1);
        assert_eq!(quote_slots(2), 1);
        assert_eq!(quote_slots(0), 1);
    }
}
