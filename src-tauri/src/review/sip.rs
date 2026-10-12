// Rebuild a session's tape from Alpaca's historical SIP data.
//
// Used when a session was traded on a source that carries no quotes or no real
// prints (Massive Starter aggregates, IEX, synthetic replay slices), or when a
// TradeTally trade was never recorded at all. `feed=sip` is forced on every call:
// the review must show the consolidated tape the trader was actually facing, never
// the IEX sliver a free key would default to. Historical SIP older than 15 minutes
// is served even on Alpaca's free plan.

use chrono::{DateTime, Utc};
use serde::Deserialize;

const DATA: &str = "https://data.alpaca.markets";
const PAGE: usize = 10_000;
/// Safety cap per window — a 40-minute window on a halted-then-squeezing low float
/// can print a lot, but never this much.
const MAX_ROWS: usize = 600_000;
/// Pauses before retrying a page after a transient failure (dropped connection,
/// timeout, 429, 5xx): a rebuild pages through tens of requests, and one blip must
/// not throw away the pages already fetched. The last one outlasts a 429.
const BACKOFF_S: [u64; 3] = [1, 4, 15];

pub struct SipTrade {
    pub ts_ms: i64,
    pub price: f64,
    pub size: i64,
    pub exch: Option<String>,
    pub cond: Option<String>,
}

pub struct SipQuote {
    pub ts_ms: i64,
    pub bid: f64,
    pub bid_size: Option<i64>,
    pub ask: f64,
    pub ask_size: Option<i64>,
    pub bid_x: Option<String>,
    pub ask_x: Option<String>,
}

pub struct SipBar {
    /// Unix seconds.
    pub t: i64,
    pub o: f64,
    pub h: f64,
    pub l: f64,
    pub c: f64,
    pub v: i64,
    pub n: Option<i64>,
}

#[derive(Deserialize)]
struct TradesPage {
    #[serde(default)]
    trades: Option<Vec<RawTrade>>,
    next_page_token: Option<String>,
}
#[derive(Deserialize)]
struct RawTrade {
    t: String,
    p: f64,
    s: i64,
    #[serde(default)]
    x: Option<String>,
    #[serde(default)]
    c: Option<Vec<String>>,
}
#[derive(Deserialize)]
struct QuotesPage {
    #[serde(default)]
    quotes: Option<Vec<RawQuote>>,
    next_page_token: Option<String>,
}
#[derive(Deserialize)]
struct RawQuote {
    t: String,
    bp: f64,
    #[serde(default)]
    bs: Option<i64>,
    ap: f64,
    #[serde(rename = "as", default)]
    as_: Option<i64>,
    #[serde(default)]
    bx: Option<String>,
    #[serde(default)]
    ax: Option<String>,
}
#[derive(Deserialize)]
struct BarsPage {
    #[serde(default)]
    bars: Option<Vec<RawBar>>,
    next_page_token: Option<String>,
}
#[derive(Deserialize)]
struct RawBar {
    t: String,
    o: f64,
    h: f64,
    l: f64,
    c: f64,
    v: i64,
    #[serde(default)]
    n: Option<i64>,
}

#[derive(Clone)]
pub struct Creds {
    pub key: String,
    pub secret: String,
}

/// Market-data credentials (the data API takes either the plain or the paper key).
pub fn creds(s: &crate::config::secrets::Secrets) -> Option<Creds> {
    let pairs = [
        (&s.alpaca_key, &s.alpaca_secret),
        (&s.alpaca_paper_key, &s.alpaca_paper_secret),
        (&s.alpaca_live_key, &s.alpaca_live_secret),
    ];
    pairs.iter().find_map(|(k, v)| {
        let k = k.as_deref().filter(|x| !x.is_empty())?;
        let v = v.as_deref().filter(|x| !x.is_empty())?;
        Some(Creds {
            key: k.to_string(),
            secret: v.to_string(),
        })
    })
}

fn ms(ts: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|d| d.with_timezone(&Utc).timestamp_millis())
}

fn rfc(ms: i64) -> String {
    chrono::TimeZone::timestamp_millis_opt(&Utc, ms)
        .single()
        .unwrap_or_else(Utc::now)
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Pooled: every page goes to the same host, and a client per page paid a DNS
/// lookup and a TLS handshake each time.
fn client() -> reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(crate::http::client).clone()
}

/// A reqwest failure with its root cause — its own text stops at "error sending
/// request for url (…)", which names neither a timeout nor a dropped connection.
fn describe(e: reqwest::Error) -> String {
    let e = e.without_url();
    let kind = if e.is_timeout() {
        "délai dépassé"
    } else if e.is_connect() {
        "connexion impossible"
    } else {
        "requête interrompue"
    };
    let mut root: &dyn std::error::Error = &e;
    while let Some(source) = root.source() {
        root = source;
    }
    format!("Alpaca SIP : {kind} ({root})")
}

async fn fetch(c: &Creds, url: &str) -> Result<(reqwest::StatusCode, String), reqwest::Error> {
    let resp = client()
        .get(url)
        .header("APCA-API-KEY-ID", &c.key)
        .header("APCA-API-SECRET-KEY", &c.secret)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await?;
    let status = resp.status();
    Ok((status, resp.text().await?))
}

async fn get<T: for<'de> Deserialize<'de>>(c: &Creds, url: &str) -> Result<T, String> {
    let mut attempt = 0;
    loop {
        crate::alpaca::ratelimit::acquire().await;
        let err = match fetch(c, url).await {
            Ok((status, text)) if status.is_success() => {
                return serde_json::from_str(&text).map_err(|e| format!("Alpaca SIP JSON: {e}"));
            }
            Ok((status, text)) => {
                let err = format!("Alpaca SIP HTTP {status}: {}", &text[..text.len().min(200)]);
                if status.as_u16() != 429 && !status.is_server_error() {
                    return Err(err);
                }
                err
            }
            Err(e) => describe(e),
        };
        let Some(&pause) = BACKOFF_S.get(attempt) else {
            return Err(err);
        };
        attempt += 1;
        tokio::time::sleep(std::time::Duration::from_secs(pause)).await;
    }
}

pub async fn trades(c: &Creds, symbol: &str, from_ms: i64, to_ms: i64) -> Result<Vec<SipTrade>, String> {
    let mut out = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let mut url = format!(
            "{DATA}/v2/stocks/{symbol}/trades?start={}&end={}&limit={PAGE}&feed=sip",
            rfc(from_ms),
            rfc(to_ms)
        );
        if let Some(t) = &token {
            url.push_str(&format!("&page_token={t}"));
        }
        let page: TradesPage = get(c, &url).await?;
        for r in page.trades.unwrap_or_default() {
            if let Some(ts) = ms(&r.t) {
                out.push(SipTrade {
                    ts_ms: ts,
                    price: r.p,
                    size: r.s,
                    exch: r.x,
                    cond: r.c.map(|v| v.join("")),
                });
            }
        }
        token = page.next_page_token.filter(|t| !t.is_empty());
        if token.is_none() || out.len() >= MAX_ROWS {
            break;
        }
    }
    Ok(out)
}

pub async fn quotes(c: &Creds, symbol: &str, from_ms: i64, to_ms: i64) -> Result<Vec<SipQuote>, String> {
    let mut out: Vec<SipQuote> = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let mut url = format!(
            "{DATA}/v2/stocks/{symbol}/quotes?start={}&end={}&limit={PAGE}&feed=sip",
            rfc(from_ms),
            rfc(to_ms)
        );
        if let Some(t) = &token {
            url.push_str(&format!("&page_token={t}"));
        }
        let page: QuotesPage = get(c, &url).await?;
        for r in page.quotes.unwrap_or_default() {
            let Some(ts) = ms(&r.t) else { continue };
            // Consecutive identical NBBO updates add nothing to spread or depth.
            if let Some(last) = out.last() {
                if last.bid == r.bp
                    && last.ask == r.ap
                    && last.bid_size == r.bs
                    && last.ask_size == r.as_
                {
                    continue;
                }
            }
            out.push(SipQuote {
                ts_ms: ts,
                bid: r.bp,
                bid_size: r.bs,
                ask: r.ap,
                ask_size: r.as_,
                bid_x: r.bx,
                ask_x: r.ax,
            });
        }
        token = page.next_page_token.filter(|t| !t.is_empty());
        if token.is_none() || out.len() >= MAX_ROWS {
            break;
        }
    }
    Ok(out)
}

/// `timeframe` = "1Min" | "1Day".
pub async fn bars(c: &Creds, symbol: &str, timeframe: &str, from_ms: i64, to_ms: i64) -> Result<Vec<SipBar>, String> {
    let mut out = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let mut url = format!(
            "{DATA}/v2/stocks/{symbol}/bars?timeframe={timeframe}&start={}&end={}&limit={PAGE}&feed=sip&adjustment=raw",
            rfc(from_ms),
            rfc(to_ms)
        );
        if let Some(t) = &token {
            url.push_str(&format!("&page_token={t}"));
        }
        let page: BarsPage = get(c, &url).await?;
        for r in page.bars.unwrap_or_default() {
            if let Some(ts) = ms(&r.t) {
                out.push(SipBar {
                    t: ts / 1000,
                    o: r.o,
                    h: r.h,
                    l: r.l,
                    c: r.c,
                    v: r.v,
                    n: r.n,
                });
            }
        }
        token = page.next_page_token.filter(|t| !t.is_empty());
        if token.is_none() {
            break;
        }
    }
    Ok(out)
}
