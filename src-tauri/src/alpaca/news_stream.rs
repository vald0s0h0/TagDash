// Alpaca news WebSocket client — the premarket "news investor".
// Endpoint: wss://stream.data.alpaca.markets/v1beta1/news
//
// One connection, subscribed to every ticker's news via the `*` wildcard. The
// feed is only active during the premarket window (ET 04:00–09:30): outside it
// the task idles (no connection) and re-checks once a minute. Headlines are
// pushed into MarketState (per-symbol for the micro_pullback correlation engine
// + a flat log for the news debug panel).
//
// Auth is identical to the market-data stream (Alpaca key/secret); the news
// stream is independent of the data feed (sip/iex/delayed_sip).

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, RwLock,
};

use chrono::{DateTime, Utc};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use tokio::time::{interval, sleep, Duration};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

use crate::market_state::MarketState;
use crate::types::NewsHeadline;

const NEWS_URL: &str = "wss://stream.data.alpaca.markets/v1beta1/news";
/// Backoff between reconnect attempts after a transient drop.
const RECONNECT_DELAY: Duration = Duration::from_secs(5);
/// How often to poll whether we're still in the premarket window while idle.
const IDLE_POLL: Duration = Duration::from_secs(60);
/// How often, while connected, to re-check the premarket window so we tear the
/// connection down promptly at 09:30.
const WINDOW_REEVAL: Duration = Duration::from_secs(30);

/// One decoded message from the news stream. Unused fields ignored.
#[derive(Debug, Deserialize)]
struct NewsMsg {
    #[serde(rename = "T")]
    kind: String,
    // news payload
    id:         Option<i64>,
    headline:   Option<String>,
    summary:    Option<String>,
    url:        Option<String>,
    source:     Option<String>,
    #[serde(default)]
    symbols:    Vec<String>,
    created_at: Option<String>,
    // control
    msg:  Option<String>,
    code: Option<i64>,
}

/// True during the premarket window (ET 04:00–09:30). ET is DST-aware (see
/// `crate::time`).
fn in_premarket() -> bool {
    (240..570).contains(&crate::time::et_minutes(Utc::now()))
}

/// Run the news feed until `running` is cleared. Connects only during premarket;
/// idles otherwise.
pub async fn run(
    market:  Arc<RwLock<MarketState>>,
    key:     String,
    secret:  String,
    running: Arc<AtomicBool>,
) {
    eprintln!("[tagdash] news feed: started (premarket only)");

    while running.load(Ordering::Relaxed) {
        if !in_premarket() {
            {
                let mut ms = market.write().unwrap();
                ms.news_set_premarket(false);
                ms.news_state("waiting_premarket");
            }
            sleep(IDLE_POLL).await;
            continue;
        }

        market.write().unwrap().news_set_premarket(true);
        market.write().unwrap().news_state("connecting");

        match connect_async(NEWS_URL).await {
            Ok((ws, _resp)) => {
                session(ws, &key, &secret, &market, &running).await;
            }
            Err(e) => {
                eprintln!("[tagdash] news feed: connect failed: {e}");
                market.write().unwrap().news_error(&format!("connect failed: {e}"));
            }
        }

        if running.load(Ordering::Relaxed) && in_premarket() {
            sleep(RECONNECT_DELAY).await;
        }
    }

    market.write().unwrap().news_state("stopped");
    eprintln!("[tagdash] news feed: stopped");
}

/// Drive one connection: auth → subscribe `news *` → ingest headlines. Returns
/// when the premarket window ends, on a disconnect, or on a fatal server error.
async fn session(
    ws:      tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    key:     &str,
    secret:  &str,
    market:  &Arc<RwLock<MarketState>>,
    running: &Arc<AtomicBool>,
) {
    let (mut write, mut read) = ws.split();

    // Authenticate.
    let auth = json!({ "action": "auth", "key": key, "secret": secret }).to_string();
    if write.send(Message::Text(auth)).await.is_err() {
        return;
    }

    let mut authenticated = false;
    let mut reeval = interval(WINDOW_REEVAL);

    loop {
        if !running.load(Ordering::Relaxed) {
            return;
        }

        tokio::select! {
            biased;

            // Tear down promptly once premarket ends.
            _ = reeval.tick() => {
                if !in_premarket() {
                    eprintln!("[tagdash] news feed: premarket window closed — disconnecting");
                    market.write().unwrap().news_set_premarket(false);
                    return;
                }
            }

            frame = read.next() => {
                let frame = match frame {
                    Some(Ok(f))  => f,
                    Some(Err(e)) => { eprintln!("[tagdash] news feed: read error: {e}"); return; }
                    None         => return,
                };
                let text = match frame {
                    Message::Text(t)   => t,
                    Message::Binary(b) => String::from_utf8_lossy(&b).into_owned(),
                    Message::Ping(_) | Message::Pong(_) => continue,
                    Message::Close(_)  => return,
                    _ => continue,
                };
                let msgs: Vec<NewsMsg> = match serde_json::from_str(&text) {
                    Ok(m)  => m,
                    Err(_) => continue,
                };

                for m in msgs {
                    match m.kind.as_str() {
                        "success" if m.msg.as_deref() == Some("authenticated") => {
                            authenticated = true;
                            market.write().unwrap().news_connected();
                            let sub = json!({ "action": "subscribe", "news": ["*"] }).to_string();
                            if write.send(Message::Text(sub)).await.is_err() {
                                return;
                            }
                            eprintln!("[tagdash] news feed: authenticated, subscribing news *");
                        }
                        "subscription" => {
                            market.write().unwrap().news_state("subscribed");
                            eprintln!("[tagdash] news feed: subscribed to news");
                        }
                        "error" => {
                            let code = m.code.unwrap_or(0);
                            eprintln!("[tagdash] news feed: server error code={code} msg={:?}", m.msg);
                            market.write().unwrap().news_error(
                                &format!("code {code}: {}", m.msg.unwrap_or_default()),
                            );
                            // Fatal auth/permission errors → stop trying this cycle.
                            if matches!(code, 402 | 403 | 404 | 406 | 409) {
                                return;
                            }
                        }
                        "n" if authenticated => {
                            if let Some(headline) = build_headline(m) {
                                market.write().unwrap().on_news(headline);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}

/// Convert a raw news message into a stored headline. Drops empty headlines and
/// upper-cases/trims the referenced symbols (matching the market data store).
fn build_headline(m: NewsMsg) -> Option<NewsHeadline> {
    let headline = m.headline.filter(|h| !h.trim().is_empty())?;
    let symbols: Vec<String> = m
        .symbols
        .into_iter()
        .map(|s| s.trim().to_uppercase())
        .filter(|s| !s.is_empty())
        .collect();
    let created_at = m
        .created_at
        .as_deref()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(Utc::now);
    Some(NewsHeadline {
        id: m.id.unwrap_or(0),
        headline,
        summary: m.summary,
        url: m.url,
        source: m.source,
        symbols,
        created_at,
        received_at: Utc::now(),
    })
}
