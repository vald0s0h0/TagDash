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
//   └ focus tier (symbols displayed in chart zones) → `trades`+`quotes`, always,
//       so a displayed chart is tick-by-tick. Each focus tick is pushed to the
//       frontend via a throttled `market-tick` Tauri event.
//
// There is no "universe" notion anymore: the broad tier subscribes with the `*`
// wildcard (the whole US market) and each strategy filters in its own script.
// The wildcard also sidesteps the code-400 "invalid syntax" rejection that an
// enumerated 13k-symbol list could trigger on a single exotic ticker.

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

type Ws = tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
>;
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
    price:  f64,
    /// Unix seconds (matches lightweight-charts time).
    ts:     i64,
}

/// Current subscriptions on the socket, per channel.
#[derive(Default)]
struct Subs {
    trades: HashSet<String>,
    quotes: HashSet<String>,
    bars:   HashSet<String>,
}

/// Run the live feed until `running` is cleared. Reconnects on transient drops;
/// bails out (clearing `running`) on fatal auth errors so we don't hammer Alpaca.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    market: Arc<RwLock<MarketState>>,
    // Kept for call-site stability; the broad tier no longer enumerates the
    // universe (it uses the `*` wildcard), so the DB handle is unused here.
    _db: Arc<Mutex<rusqlite::Connection>>,
    feed: String,
    key: String,
    secret: String,
    warn_ms: u32,
    critical_ms: u32,
    running: Arc<AtomicBool>,
    focus_rx: watch::Receiver<Vec<String>>,
    app: AppHandle,
) {
    let url = format!("wss://stream.data.alpaca.markets/v2/{feed}");
    eprintln!("[tagdash] live feed: connecting to {url}");
    {
        let mut ms = market.write().unwrap();
        ms.feed_set_feed(&feed);
        ms.feed_state("connecting");
    }

    let mut focus_rx = focus_rx;

    while running.load(Ordering::Relaxed) {
        match connect_async(&url).await {
            Ok((ws, _resp)) => {
                let code = session(
                    ws, &key, &secret, &market, warn_ms, critical_ms, &running,
                    &mut focus_rx, &app,
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
                market.write().unwrap().feed_error(0, Some(format!("connect failed: {e}")));
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
                    && reconcile(&mut write, market, &focus, broad_mode_now(), &mut subs).await.is_err()
                {
                    return None;
                }
            }

            // Periodic broad-mode re-evaluation (catches the 09:30 switch).
            _ = reeval.tick() => {
                if authenticated
                    && reconcile(&mut write, market, &focus, broad_mode_now(), &mut subs).await.is_err()
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
                            if reconcile(&mut write, market, &focus, broad_mode_now(), &mut subs).await.is_err() {
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
                                        ms.on_quote(sym, bid, ask, now);
                                        let event_time = parse_ts(m.timestamp.as_deref());
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

/// Reconcile the three channel subscriptions. The broad surveillance tier covers
/// the whole US market via the `*` wildcard (one tiny message instead of ~13k
/// enumerated symbols), so a single exotic symbol can no longer get the frame
/// rejected with code 400. The focus tier (displayed symbols) is subscribed
/// explicitly so charts stay tick-by-tick.
async fn reconcile(
    write: &mut WsWrite,
    market: &Arc<RwLock<MarketState>>,
    focus: &HashSet<String>,
    broad_mode: &str,
    current: &mut Subs,
) -> Result<(), WsError> {
    let wildcard: HashSet<String> = std::iter::once(WILDCARD.to_string()).collect();

    // Desired sets per channel.
    let (trades_d, quotes_d, bars_d): (HashSet<String>, HashSet<String>, HashSet<String>) =
        if broad_mode == "trades" {
            // Premarket: every trade (the wildcard already covers the focus
            // symbols' trades, which drive the tick events); quotes for focus.
            (wildcard, focus.clone(), HashSet::new())
        } else {
            // Open: every 1-min bar (focus symbols' bars are ignored in the data
            // pass); trades+quotes for focus → tick-by-tick charts.
            (focus.clone(), focus.clone(), wildcard)
        };

    sync_channel(write, market, "trades", &mut current.trades, &trades_d).await?;
    sync_channel(write, market, "quotes", &mut current.quotes, &quotes_d).await?;
    sync_channel(write, market, "bars",   &mut current.bars,   &bars_d).await?;

    // Broad tier is the whole market (wildcard); report 0 as the enumerated count
    // — the panel renders this as "tout le marché (✱)".
    market.write().unwrap().feed_reconcile(broad_mode, 0, focus.len());
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
    let to_add:    Vec<String> = desired.difference(current).cloned().collect();
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
            format!("{action} {channel} msg {}/{} ({} symbols)", i + 1, total, chunk.len())
        };
        eprintln!("[tagdash] live feed: → {label} (e.g. {:?})", &chunk[..chunk.len().min(3)]);
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
fn clean_symbols<'a>(it: impl Iterator<Item = &'a String>) -> HashSet<String> {
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

/// Broad-tier channel for now: `trades` during the premarket window (ET
/// 04:00–09:30), `bars` (1-minute) otherwise. ET is DST-aware (see `crate::time`).
fn broad_mode_now() -> &'static str {
    if (240..570).contains(&crate::time::et_minutes(Utc::now())) {
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
