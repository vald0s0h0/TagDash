// Alpaca Trading API (REST) client.
//
// Follows Alpaca's documented order lifecycle rather than reinventing one:
//
//   • Every submit carries a `client_order_id`. It is Alpaca's own idempotency
//     mechanism: if the response is lost in flight, resubmitting the SAME id is
//     rejected as a duplicate instead of opening a second position. Generating it
//     client-side is what makes a retry safe.
//   • Protective exits are submitted as a `bracket` order class, not as three
//     separate orders. The venue then owns the OCO relationship — the stop and the
//     target cancel each other server-side, so an app crash cannot leave a position
//     naked or double-exit it.
//   • Cancel and replace address the order by the venue's own id.
//   • Every response is parsed into `BrokerOrder` / `BrokerPosition` and returned as
//     the new truth. Nothing here writes to a local book.
//
// Errors keep Alpaca's message. A refused order says why ("insufficient buying
// power", "asset is not shortable") and that sentence is what the trader needs.

use serde::Deserialize;
use serde_json::{json, Value};

use super::types::*;
use crate::types::{OrderType, Side};

/// Trading-API host per environment. Note this is NOT `data.alpaca.markets` — the
/// trading and market-data APIs are different services with different credentials.
pub fn base_url(env: Environment) -> &'static str {
    match env {
        Environment::Paper => "https://paper-api.alpaca.markets",
        Environment::Live => "https://api.alpaca.markets",
    }
}

/// Credentials + target account for one set of calls.
#[derive(Debug, Clone)]
pub struct Creds {
    pub key: String,
    pub secret: String,
    pub environment: Environment,
}

impl Creds {
    pub fn is_complete(&self) -> bool {
        !self.key.is_empty() && !self.secret.is_empty()
    }
}

fn request(creds: &Creds, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
    crate::http::client()
        .request(method, format!("{}{path}", base_url(creds.environment)))
        .header("APCA-API-KEY-ID", &creds.key)
        .header("APCA-API-SECRET-KEY", &creds.secret)
}

/// Turn a non-2xx response into Alpaca's own sentence.
///
/// The venue's refusal text is the single most useful thing a trader can see when
/// an order doesn't appear, so it is passed through rather than replaced by a
/// generic failure.
async fn check(resp: reqwest::Response, what: &str) -> Result<String, String> {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if status.is_success() {
        return Ok(body);
    }
    let detail = serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(String::from))
        .unwrap_or_else(|| body.chars().take(300).collect());
    Err(format!(
        "{what} refusé par Alpaca (HTTP {}) : {detail}",
        status.as_u16()
    ))
}

// ─── Wire shapes ─────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct RawOrder {
    id: String,
    client_order_id: String,
    symbol: String,
    side: String,
    #[serde(rename = "type")]
    order_type: String,
    time_in_force: String,
    qty: Option<String>,
    filled_qty: Option<String>,
    limit_price: Option<String>,
    stop_price: Option<String>,
    filled_avg_price: Option<String>,
    status: String,
    #[serde(default)]
    legs: Option<Vec<RawOrder>>,
    submitted_at: Option<String>,
    filled_at: Option<String>,
    canceled_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawPosition {
    symbol: String,
    qty: Option<String>,
    avg_entry_price: Option<String>,
    current_price: Option<String>,
    market_value: Option<String>,
    cost_basis: Option<String>,
    unrealized_pl: Option<String>,
    unrealized_plpc: Option<String>,
    side: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawAccount {
    id: String,
    status: String,
    currency: Option<String>,
    cash: Option<String>,
    equity: Option<String>,
    buying_power: Option<String>,
    #[serde(default)]
    trading_blocked: bool,
    #[serde(default)]
    account_blocked: bool,
    #[serde(default)]
    trade_suspended_by_user: bool,
    #[serde(default)]
    pattern_day_trader: bool,
    daytrade_count: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct RawAsset {
    symbol: String,
    #[serde(default)]
    shortable: bool,
    #[serde(default)]
    easy_to_borrow: Option<bool>,
    #[serde(default)]
    borrow_status: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawLocateQuote {
    symbol: String,
    available_qty: i64,
    price: String,
    quoted_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawLocateQuoteError {
    symbol: Option<String>,
    code: Option<String>,
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawLocateQuotes {
    #[serde(default)]
    quotes: Vec<RawLocateQuote>,
    #[serde(default)]
    errors: Vec<RawLocateQuoteError>,
}

#[derive(Debug, Deserialize)]
struct RawLocate {
    id: String,
    symbol: String,
    requested_qty: i64,
    located_qty: i64,
    #[serde(default, alias = "remaining_qty")]
    available_qty: Option<i64>,
    located_price: Option<String>,
    total_fee: Option<String>,
    status: String,
    created_at: Option<String>,
    expires_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawLocateList {
    #[serde(default)]
    locates: Vec<RawLocate>,
}

#[derive(Debug, Deserialize)]
struct RawActivity {
    id: String,
    price: String,
    qty: String,
    side: String,
    symbol: String,
    transaction_time: String,
    order_id: String,
}

#[derive(Debug, Clone)]
pub struct FillActivity {
    pub id: String,
    pub price: f64,
    pub qty: f64,
    pub side: Side,
    pub symbol: String,
    pub at: chrono::DateTime<chrono::Utc>,
    pub order_id: String,
}

/// Alpaca sends every number as a JSON string.
fn num(s: &Option<String>) -> Option<f64> {
    s.as_ref().and_then(|v| v.parse::<f64>().ok())
}

fn ts(s: &Option<String>) -> Option<chrono::DateTime<chrono::Utc>> {
    s.as_ref()
        .and_then(|v| chrono::DateTime::parse_from_rfc3339(v).ok())
        .map(|d| d.with_timezone(&chrono::Utc))
}

/// Fold Alpaca's status vocabulary into the four states the UI acts on.
///
/// Anything unrecognised is treated as `Held`, never as terminal: wrongly believing
/// an order is dead would hide a live exposure, while wrongly believing it lives
/// only shows a stale row that the next reconcile corrects.
pub fn map_status(raw: &str) -> BrokerOrderStatus {
    match raw {
        "filled" => BrokerOrderStatus::Filled,
        "canceled" | "cancelled" | "expired" | "rejected" | "done_for_day" | "replaced"
        | "calculated" => BrokerOrderStatus::Closed,
        "new" | "partially_filled" | "pending_cancel" | "pending_replace" => {
            BrokerOrderStatus::Open
        }
        // Accepted/pending orders have not started working yet. `stopped` means a
        // trade has been guaranteed but has not occurred; `suspended` is likewise
        // ineligible for execution, not terminal. Keep all of them visible.
        "accepted" | "pending_new" | "accepted_for_bidding" | "stopped" | "suspended" => {
            BrokerOrderStatus::Held
        }
        _ => BrokerOrderStatus::Held,
    }
}

fn to_order(r: RawOrder) -> BrokerOrder {
    BrokerOrder {
        id: r.id,
        client_order_id: r.client_order_id,
        symbol: r.symbol,
        side: r.side,
        order_type: r.order_type,
        time_in_force: r.time_in_force,
        quantity: num(&r.qty).unwrap_or(0.0),
        filled_quantity: num(&r.filled_qty).unwrap_or(0.0),
        limit_price: num(&r.limit_price),
        stop_price: num(&r.stop_price),
        filled_avg_price: num(&r.filled_avg_price),
        status: map_status(&r.status),
        raw_status: r.status,
        parent_id: None,
        submitted_at: ts(&r.submitted_at),
        filled_at: ts(&r.filled_at),
        canceled_at: ts(&r.canceled_at),
        reject_reason: None,
    }
}

/// Flatten a bracket order into parent + legs, stamping each leg with its parent.
///
/// Alpaca nests the stop and target inside the entry order's `legs`. Flattening
/// them means the mirror holds every live order at top level, so cancelling a
/// protective leg — or noticing one filled — needs no special case.
fn flatten(mut r: RawOrder) -> Vec<BrokerOrder> {
    let legs = r.legs.take().unwrap_or_default();
    let parent = to_order(r);
    let mut out = Vec::with_capacity(1 + legs.len());
    for leg in legs {
        let mut child = to_order(leg);
        child.parent_id = Some(parent.id.clone());
        out.push(child);
    }
    out.push(parent);
    out
}

// ─── Orders ──────────────────────────────────────────────────────────────────

fn side_str(side: Side) -> &'static str {
    match side {
        Side::Long => "buy",
        Side::Short => "sell",
    }
}

fn type_str(t: OrderType) -> &'static str {
    match t {
        OrderType::Market => "market",
        OrderType::Limit => "limit",
        OrderType::Stop => "stop",
    }
}

/// Alpaca rejects sub-penny increments: two decimals at/above $1, four below.
fn price_string(price: f64) -> String {
    if price < 1.0 {
        format!("{price:.4}")
    } else {
        format!("{price:.2}")
    }
}

fn validate_order(req: &OrderRequest) -> Result<(), String> {
    if req.symbol.trim().is_empty() {
        return Err("symbole vide".into());
    }
    if req.quantity <= 0 {
        return Err("la quantité doit être strictement positive".into());
    }
    let valid_price = |p: f64| p.is_finite() && p > 0.0;
    match req.order_type {
        OrderType::Market => {}
        OrderType::Limit if req.limit_price.is_some_and(valid_price) => {}
        OrderType::Stop if req.stop_price.is_some_and(valid_price) => {}
        OrderType::Limit => return Err("un ordre limite exige un prix limite positif".into()),
        OrderType::Stop => return Err("un ordre stop exige un prix stop positif".into()),
    }
    for (label, price) in [
        ("prix limite", req.limit_price),
        ("prix stop", req.stop_price),
        ("stop-loss", req.stop_loss),
        ("take-profit", req.take_profit),
    ] {
        if price.is_some_and(|p| !valid_price(p)) {
            return Err(format!("{label} invalide"));
        }
    }
    if let Some(tif) = req.time_in_force.as_deref() {
        if !matches!(tif, "day" | "gtc") {
            return Err(format!("time_in_force non supporté : {tif}"));
        }
    }
    if req.extended_hours {
        if !matches!(req.order_type, OrderType::Limit) {
            return Err("extended_hours exige un ordre limite chez Alpaca".into());
        }
        if !matches!(req.time_in_force.as_deref().unwrap_or("day"), "day" | "gtc") {
            return Err("extended_hours exige un time_in_force day ou gtc".into());
        }
        if req.stop_loss.is_some() || req.take_profit.is_some() {
            return Err(
                "Alpaca refuse bracket/OTO/OCO en extended hours ; utilise une protection locale persistée"
                    .into(),
            );
        }
    }
    if let Some(id) = req.client_order_id.as_deref() {
        if id.is_empty() || id.len() > 48 {
            return Err("client_order_id doit contenir entre 1 et 48 caractères".into());
        }
    }
    if req.reduce_only {
        return Err(
            "Alpaca actions ne garantit pas reduce_only sur POST /v2/orders ; \
             utilisez la fermeture de position ou une jambe bracket gérée par Alpaca"
                .into(),
        );
    }
    if let (Some(sl), Some(tp)) = (req.stop_loss, req.take_profit) {
        let ordered = match req.side {
            Side::Long => tp > sl,
            Side::Short => tp < sl,
        };
        if !ordered {
            return Err("stop-loss et take-profit sont inversés pour le sens de l'ordre".into());
        }
    }
    Ok(())
}

/// Build the JSON body for a submit.
///
/// Split from the request so the mapping — especially the bracket shape, where a
/// mistake means an unprotected position — is testable without a network.
pub fn submit_body(req: &OrderRequest, client_order_id: &str) -> Value {
    let mut body = json!({
        "symbol": req.symbol.trim().to_uppercase(),
        "qty": req.quantity.abs().to_string(),
        "side": side_str(req.side),
        "type": type_str(req.order_type),
        "time_in_force": req.time_in_force.clone().unwrap_or_else(|| "day".into()),
        "client_order_id": client_order_id,
    });
    let map = body.as_object_mut().expect("object");
    if req.extended_hours {
        map.insert("extended_hours".into(), json!(true));
    }
    if let Some(p) = req.limit_price {
        map.insert("limit_price".into(), json!(price_string(p)));
    }
    if let Some(p) = req.stop_price {
        map.insert("stop_price".into(), json!(price_string(p)));
    }

    // Protective legs → let the VENUE own the bracket. A bracket is one atomic
    // instruction: fill the entry, then work a stop and a target that cancel each
    // other. Sending three loose orders instead would leave the position naked
    // between the entry fill and the exits landing, and would double-exit if both
    // triggered while the app was down.
    //
    // `reduce_only` exits are excluded on purpose: they ARE the bracket legs, and
    // nesting a bracket inside a bracket is rejected.
    if !req.reduce_only {
        match (req.stop_loss, req.take_profit) {
            (Some(sl), Some(tp)) => {
                map.insert("order_class".into(), json!("bracket"));
                map.insert(
                    "stop_loss".into(),
                    json!({ "stop_price": price_string(sl) }),
                );
                map.insert(
                    "take_profit".into(),
                    json!({ "limit_price": price_string(tp) }),
                );
            }
            (Some(sl), None) => {
                map.insert("order_class".into(), json!("oto"));
                map.insert(
                    "stop_loss".into(),
                    json!({ "stop_price": price_string(sl) }),
                );
            }
            (None, Some(tp)) => {
                map.insert("order_class".into(), json!("oto"));
                map.insert(
                    "take_profit".into(),
                    json!({ "limit_price": price_string(tp) }),
                );
            }
            (None, None) => {}
        }
    }
    body
}

/// Submit an order. Returns the parent plus any bracket legs the venue created.
pub async fn submit(creds: &Creds, req: &OrderRequest) -> Result<Vec<BrokerOrder>, String> {
    validate_order(req)?;
    let coid = req
        .client_order_id
        .clone()
        .unwrap_or_else(|| format!("tagdash-{}", uuid_like()));
    let body = submit_body(req, &coid);
    let sent_ms = crate::time::now().timestamp_millis();
    let resp = request(creds, reqwest::Method::POST, "/v2/orders")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Alpaca injoignable à l'envoi de l'ordre : {e}"))?;
    let text = check(resp, "ordre").await?;
    let ack_ms = crate::time::now().timestamp_millis();
    let raw: RawOrder =
        serde_json::from_str(&text).map_err(|e| format!("réponse d'ordre illisible : {e}"))?;
    let orders = flatten(raw);
    // Send → ack latency of the order itself (the parent). Bracket legs trigger
    // later on their own; their fills keep no send time.
    if let Some(parent) = orders.first() {
        crate::review::timing::order_acked(&parent.id, Some(sent_ms), ack_ms);
    }
    Ok(orders)
}

/// Attach venue-owned OCO exits to an already-open position during RTH.
/// Advanced orders are deliberately kept out of extended hours; the local durable
/// protection worker owns that interval and hands off here at 09:30 ET.
pub async fn submit_oco_exit(
    creds: &Creds,
    symbol: &str,
    position_side: Side,
    quantity: i64,
    stop_loss: f64,
    take_profit: f64,
    client_order_id: &str,
) -> Result<Vec<BrokerOrder>, String> {
    if quantity <= 0
        || !stop_loss.is_finite()
        || stop_loss <= 0.0
        || !take_profit.is_finite()
        || take_profit <= 0.0
    {
        return Err("paramètres OCO invalides".into());
    }
    let correctly_ordered = match position_side {
        Side::Long => take_profit > stop_loss,
        Side::Short => take_profit < stop_loss,
    };
    if !correctly_ordered {
        return Err("niveaux OCO inversés".into());
    }
    let exit_side = match position_side {
        Side::Long => Side::Short,
        Side::Short => Side::Long,
    };
    let body = json!({
        "symbol": symbol.trim().to_uppercase(),
        "qty": quantity.to_string(),
        "side": side_str(exit_side),
        "type": "limit",
        "time_in_force": "gtc",
        "order_class": "oco",
        "client_order_id": client_order_id,
        "take_profit": { "limit_price": price_string(take_profit) },
        "stop_loss": { "stop_price": price_string(stop_loss) }
    });
    let resp = request(creds, reqwest::Method::POST, "/v2/orders")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Alpaca injoignable à l'envoi de l'OCO : {e}"))?;
    let text = check(resp, "protection OCO").await?;
    let raw: RawOrder =
        serde_json::from_str(&text).map_err(|e| format!("réponse OCO illisible : {e}"))?;
    Ok(flatten(raw))
}

/// Cancel one order by the VENUE's id. A 404/422 means it is already gone, which is
/// the outcome the caller wanted — reported as success so a double-click doesn't
/// surface a scary error.
pub async fn cancel(creds: &Creds, order_id: &str) -> Result<(), String> {
    let resp = request(
        creds,
        reqwest::Method::DELETE,
        &format!("/v2/orders/{order_id}"),
    )
    .send()
    .await
    .map_err(|e| format!("Alpaca injoignable à l'annulation : {e}"))?;
    match resp.status().as_u16() {
        200 | 204 | 404 | 422 => Ok(()),
        _ => check(resp, "annulation").await.map(|_| ()),
    }
}

/// Replace a working order's price and/or size. Alpaca issues a NEW id and marks
/// the old one `replaced`; the caller must adopt the returned order.
pub async fn replace(
    creds: &Creds,
    order_id: &str,
    quantity: Option<i64>,
    limit_price: Option<f64>,
    stop_price: Option<f64>,
) -> Result<BrokerOrder, String> {
    let mut body = serde_json::Map::new();
    if let Some(q) = quantity {
        if q <= 0 {
            return Err("la quantité de remplacement doit être strictement positive".into());
        }
        body.insert("qty".into(), json!(q.to_string()));
    }
    if let Some(p) = limit_price {
        if !p.is_finite() || p <= 0.0 {
            return Err("prix limite de remplacement invalide".into());
        }
        body.insert("limit_price".into(), json!(price_string(p)));
    }
    if let Some(p) = stop_price {
        if !p.is_finite() || p <= 0.0 {
            return Err("prix stop de remplacement invalide".into());
        }
        body.insert("stop_price".into(), json!(price_string(p)));
    }
    if body.is_empty() {
        return Err("remplacement sans changement".into());
    }
    let resp = request(
        creds,
        reqwest::Method::PATCH,
        &format!("/v2/orders/{order_id}"),
    )
    .json(&Value::Object(body))
    .send()
    .await
    .map_err(|e| format!("Alpaca injoignable au remplacement : {e}"))?;
    let text = check(resp, "remplacement").await?;
    let raw: RawOrder = serde_json::from_str(&text)
        .map_err(|e| format!("réponse de remplacement illisible : {e}"))?;
    Ok(to_order(raw))
}

/// Every order still live at the venue, bracket legs flattened out.
pub async fn open_orders(creds: &Creds) -> Result<Vec<BrokerOrder>, String> {
    let resp = request(
        creds,
        reqwest::Method::GET,
        "/v2/orders?status=open&nested=true&limit=500",
    )
    .send()
    .await
    .map_err(|e| format!("Alpaca injoignable (ordres) : {e}"))?;
    let text = check(resp, "liste des ordres").await?;
    let raw: Vec<RawOrder> =
        serde_json::from_str(&text).map_err(|e| format!("liste d'ordres illisible : {e}"))?;
    Ok(raw.into_iter().flat_map(flatten).collect())
}

/// Fetch one order by the venue's id (used to refresh after a stream event).
pub async fn get_order(creds: &Creds, order_id: &str) -> Result<BrokerOrder, String> {
    let resp = request(
        creds,
        reqwest::Method::GET,
        &format!("/v2/orders/{order_id}"),
    )
    .send()
    .await
    .map_err(|e| format!("Alpaca injoignable (ordre) : {e}"))?;
    let text = check(resp, "ordre").await?;
    let raw: RawOrder =
        serde_json::from_str(&text).map_err(|e| format!("ordre illisible : {e}"))?;
    Ok(to_order(raw))
}

/// Historical executions used to repair the local journal after TagDash was
/// offline when a position closed. Pagination is explicit; silently reading only
/// the newest 100 account fills could miss the exit on an active account.
pub async fn fills_since(
    creds: &Creds,
    after: chrono::DateTime<chrono::Utc>,
    symbol: &str,
) -> Result<Vec<FillActivity>, String> {
    let mut page_token: Option<String> = None;
    let mut out = Vec::new();
    for _ in 0..20 {
        let mut query = vec![
            ("after", after.to_rfc3339()),
            ("direction", "asc".to_string()),
            ("page_size", "100".to_string()),
        ];
        if let Some(token) = page_token.as_ref() {
            query.push(("page_token", token.clone()));
        }
        let resp = request(creds, reqwest::Method::GET, "/v2/account/activities/FILL")
            .query(&query)
            .send()
            .await
            .map_err(|e| format!("Alpaca injoignable (historique des fills) : {e}"))?;
        let text = check(resp, "historique des fills").await?;
        let raw: Vec<RawActivity> = serde_json::from_str(&text)
            .map_err(|e| format!("historique des fills illisible : {e}"))?;
        let page_len = raw.len();
        page_token = raw.last().map(|a| a.id.clone());
        out.extend(raw.into_iter().filter_map(|a| {
            if !a.symbol.eq_ignore_ascii_case(symbol) {
                return None;
            }
            Some(FillActivity {
                id: a.id,
                price: a.price.parse().ok()?,
                qty: a.qty.parse().ok()?,
                side: if a.side == "sell" {
                    Side::Short
                } else {
                    Side::Long
                },
                symbol: a.symbol,
                at: chrono::DateTime::parse_from_rfc3339(&a.transaction_time)
                    .ok()?
                    .with_timezone(&chrono::Utc),
                order_id: a.order_id,
            })
        }));
        if page_len < 100 {
            return Ok(out);
        }
    }
    Err("historique des fills Alpaca: pagination supérieure à 2 000 activités".into())
}

// ─── Positions ───────────────────────────────────────────────────────────────

fn to_position(r: RawPosition) -> BrokerPosition {
    // Alpaca reports `qty` unsigned with the direction in `side`; normalise to a
    // signed quantity so downstream code never has to remember which.
    let magnitude = num(&r.qty).unwrap_or(0.0).abs();
    let signed = match r.side.as_deref() {
        Some("short") => -magnitude,
        _ => magnitude,
    };
    BrokerPosition {
        symbol: r.symbol,
        quantity: signed,
        avg_entry_price: num(&r.avg_entry_price).unwrap_or(0.0),
        market_price: num(&r.current_price),
        market_value: num(&r.market_value),
        cost_basis: num(&r.cost_basis),
        unrealized_pnl: num(&r.unrealized_pl),
        unrealized_pnl_pct: num(&r.unrealized_plpc).map(|v| v * 100.0),
    }
}

pub async fn positions(creds: &Creds) -> Result<Vec<BrokerPosition>, String> {
    let resp = request(creds, reqwest::Method::GET, "/v2/positions")
        .send()
        .await
        .map_err(|e| format!("Alpaca injoignable (positions) : {e}"))?;
    let text = check(resp, "liste des positions").await?;
    let raw: Vec<RawPosition> =
        serde_json::from_str(&text).map_err(|e| format!("liste de positions illisible : {e}"))?;
    Ok(raw.into_iter().map(to_position).collect())
}

/// One current position, used immediately before an extended-hours protective
/// exit so a stale mirror cannot turn a close into a new opposite position.
pub async fn position(creds: &Creds, symbol: &str) -> Result<Option<BrokerPosition>, String> {
    let sym = symbol.trim().to_uppercase();
    let resp = request(creds, reqwest::Method::GET, &format!("/v2/positions/{sym}"))
        .send()
        .await
        .map_err(|e| format!("Alpaca injoignable (position {sym}) : {e}"))?;
    if resp.status().as_u16() == 404 {
        return Ok(None);
    }
    let text = check(resp, "position").await?;
    let raw: RawPosition =
        serde_json::from_str(&text).map_err(|e| format!("position illisible : {e}"))?;
    Ok(Some(to_position(raw)))
}

/// Flatten one position at market. `cancel_orders` also pulls its resting exits,
/// which is what you want when closing by hand — otherwise the bracket legs stay
/// working and can open a NEW position in the opposite direction.
pub async fn close_position(
    creds: &Creds,
    symbol: &str,
    cancel_orders: bool,
) -> Result<(), String> {
    if cancel_orders {
        // Best effort: a failure here must not stop the flatten itself.
        let _ = request(
            creds,
            reqwest::Method::DELETE,
            &format!("/v2/orders?symbol={}", symbol.trim().to_uppercase()),
        )
        .send()
        .await;
    }
    let resp = request(
        creds,
        reqwest::Method::DELETE,
        &format!("/v2/positions/{}", symbol.trim().to_uppercase()),
    )
    .send()
    .await
    .map_err(|e| format!("Alpaca injoignable (fermeture) : {e}"))?;
    match resp.status().as_u16() {
        200 | 204 | 404 => Ok(()),
        _ => check(resp, "fermeture de position").await.map(|_| ()),
    }
}

// ─── Account & assets ────────────────────────────────────────────────────────

pub async fn account(creds: &Creds) -> Result<BrokerAccount, String> {
    let resp = request(creds, reqwest::Method::GET, "/v2/account")
        .send()
        .await
        .map_err(|e| format!("Alpaca injoignable (compte) : {e}"))?;
    let text = check(resp, "compte").await?;
    let r: RawAccount =
        serde_json::from_str(&text).map_err(|e| format!("compte illisible : {e}"))?;
    Ok(BrokerAccount {
        id: r.id,
        broker: BrokerId::Alpaca,
        environment: creds.environment,
        status: r.status,
        currency: r.currency.unwrap_or_else(|| "USD".into()),
        cash: num(&r.cash),
        equity: num(&r.equity),
        buying_power: num(&r.buying_power),
        trading_blocked: r.trading_blocked,
        account_blocked: r.account_blocked,
        trade_suspended_by_user: r.trade_suspended_by_user,
        pattern_day_trader: r.pattern_day_trader,
        daytrade_count: r.daytrade_count,
    })
}

/// Ask the venue whether one symbol may be shorted right now.
pub async fn shortability(creds: &Creds, symbol: &str) -> Result<Shortability, String> {
    let sym = symbol.trim().to_uppercase();
    let resp = request(creds, reqwest::Method::GET, &format!("/v2/assets/{sym}"))
        .send()
        .await
        .map_err(|e| format!("Alpaca injoignable (asset) : {e}"))?;
    let text = check(resp, "asset").await?;
    let r: RawAsset = serde_json::from_str(&text).map_err(|e| format!("asset illisible : {e}"))?;
    let borrow_status =
        normalize_borrow_status(r.shortable, r.borrow_status.as_deref(), r.easy_to_borrow);
    Ok(Shortability {
        symbol: r.symbol,
        broker: BrokerId::Alpaca,
        shortable: r.shortable,
        borrow_status,
        from_cache: false,
    })
}

fn normalize_borrow_status(
    shortable: bool,
    borrow_status: Option<&str>,
    easy_to_borrow: Option<bool>,
) -> BorrowStatus {
    if !shortable {
        BorrowStatus::Unavailable
    } else {
        match borrow_status {
            Some("easy_to_borrow") => BorrowStatus::EasyToBorrow,
            Some("hard_to_borrow") => BorrowStatus::HardToBorrow,
            // Backward compatibility until Alpaca removes `easy_to_borrow`.
            _ => match easy_to_borrow {
                Some(true) => BorrowStatus::EasyToBorrow,
                Some(false) => BorrowStatus::HardToBorrow,
                None => BorrowStatus::Unknown,
            },
        }
    }
}

fn locate_record(r: RawLocate) -> Result<LocateRecord, String> {
    let active = r.status.eq_ignore_ascii_case("active");
    let price_per_share = r
        .located_price
        .as_deref()
        .and_then(|v| v.parse().ok())
        .filter(|v: &f64| v.is_finite() && *v >= 0.0);
    let total_fee = r
        .total_fee
        .as_deref()
        .and_then(|v| v.parse().ok())
        .filter(|v: &f64| v.is_finite() && *v >= 0.0);
    if active && (price_per_share.is_none() || total_fee.is_none() || r.expires_at.is_none()) {
        return Err("locate actif incomplet (prix, frais ou expiration absents)".into());
    }
    Ok(LocateRecord {
        id: r.id,
        broker: BrokerId::Alpaca,
        symbol: r.symbol,
        requested_qty: r.requested_qty,
        located_qty: r.located_qty,
        available_qty: r.available_qty,
        price_per_share: price_per_share.unwrap_or(0.0),
        total_fee: total_fee.unwrap_or(0.0),
        status: r.status,
        created_at: ts(&r.created_at),
        expires_at: ts(&r.expires_at),
    })
}

/// Current account-specific Alpaca locate quote for one HTB symbol.
pub async fn locate_quote(creds: &Creds, symbol: &str) -> Result<LocateQuote, String> {
    let sym = symbol.trim().to_uppercase();
    let resp = request(
        creds,
        reqwest::Method::GET,
        &format!("/v1/locates/quotes?symbols={sym}"),
    )
    .send()
    .await
    .map_err(|e| format!("Alpaca injoignable (quote locate) : {e}"))?;
    let text = check(resp, "quote locate").await?;
    let mut body: RawLocateQuotes =
        serde_json::from_str(&text).map_err(|e| format!("quote locate illisible : {e}"))?;
    if let Some(q) = body
        .quotes
        .drain(..)
        .find(|q| q.symbol.eq_ignore_ascii_case(&sym))
    {
        let price_per_share = q
            .price
            .parse::<f64>()
            .map_err(|e| format!("prix de locate illisible : {e}"))?;
        return Ok(LocateQuote {
            broker: BrokerId::Alpaca,
            symbol: q.symbol,
            available_qty: q.available_qty,
            price_per_share,
            quoted_at: ts(&q.quoted_at),
            // Alpaca documents locate price here, but not the separate daily HTB
            // borrow rate. Keep it unknown rather than guessing a fee or unit.
            annual_borrow_rate: None,
        });
    }
    let detail = body
        .errors
        .into_iter()
        .find(|e| {
            e.symbol
                .as_deref()
                .is_none_or(|s| s.eq_ignore_ascii_case(&sym))
        })
        .map(|e| {
            let code = e.code.map(|c| format!(" [{c}]")).unwrap_or_default();
            format!(
                "{}{}",
                e.message.unwrap_or_else(|| "quote indisponible".into()),
                code
            )
        })
        .unwrap_or_else(|| "aucune quote retournée".into());
    Err(format!("locate {sym} indisponible : {detail}"))
}

/// Active/recent locate records for one symbol. The venue remains authoritative
/// about remaining inventory; records without a remaining quantity are handled
/// conservatively by the planning layer.
pub async fn locates(creds: &Creds, symbol: &str) -> Result<Vec<LocateRecord>, String> {
    let sym = symbol.trim().to_uppercase();
    let resp = request(
        creds,
        reqwest::Method::GET,
        &format!("/v1/locates?symbol={sym}&limit=100"),
    )
    .send()
    .await
    .map_err(|e| format!("Alpaca injoignable (locates) : {e}"))?;
    let text = check(resp, "liste des locates").await?;
    let body: RawLocateList =
        serde_json::from_str(&text).map_err(|e| format!("locates illisibles : {e}"))?;
    body.locates.into_iter().map(locate_record).collect()
}

/// Reserve HTB inventory. The quote price is passed as a hard ceiling and the
/// idempotency key makes an ambiguous network retry safe from duplicate fees.
pub async fn create_locate(
    creds: &Creds,
    symbol: &str,
    quantity: i64,
    limit_price: f64,
    idempotency_key: &str,
) -> Result<LocateRecord, String> {
    let sym = symbol.trim().to_uppercase();
    let resp = request(creds, reqwest::Method::POST, "/v1/locates")
        .header("Idempotency-Key", idempotency_key)
        .json(&json!({
            "symbol": sym,
            "qty": quantity,
            "limit_price": price_string(limit_price),
            "all_or_none": true,
        }))
        .send()
        .await
        .map_err(|e| format!("Alpaca injoignable (création locate) : {e}"))?;
    let text = check(resp, "création du locate").await?;
    let raw: RawLocate =
        serde_json::from_str(&text).map_err(|e| format!("locate illisible : {e}"))?;
    let mut located = locate_record(raw)?;
    // The documented create response omits `available_qty`; immediately after a
    // successful all-or-none reservation the full located amount is available.
    if located.available_qty.is_none() && located.status.eq_ignore_ascii_case("active") {
        located.available_qty = Some(located.located_qty);
    }
    Ok(located)
}

/// A random, collision-free-enough suffix for a client order id.
fn uuid_like() -> String {
    use rand::Rng;
    let a: u64 = rand::thread_rng().gen();
    let b: u32 = rand::thread_rng().gen();
    format!("{a:016x}{b:08x}")
}

/// Alpaca caps caller-provided ids at 48 bytes. Preserve a readable piece of the
/// trade id while reserving enough random suffix to make rapid scale-ins distinct.
pub fn prefixed_client_order_id(prefix: &str) -> String {
    let suffix = &uuid_like()[..16];
    let max_prefix = 48 - 1 - suffix.len();
    let clean: String = prefix
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .take(max_prefix)
        .collect();
    let clean = if clean.is_empty() {
        "tagdash"
    } else {
        clean.as_str()
    };
    format!("{clean}-{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn borrow_status_never_overrides_a_non_shortable_asset() {
        // Alpaca currently returns this exact combination for several watchlist
        // names: `hard_to_borrow` describes scarcity, but `shortable=false` is the
        // final permission and must render as NS, never as actionable HTB.
        assert_eq!(
            normalize_borrow_status(false, Some("hard_to_borrow"), Some(false)),
            BorrowStatus::Unavailable,
        );
    }

    #[test]
    fn a_shortable_htb_asset_remains_distinct_from_etb() {
        assert_eq!(
            normalize_borrow_status(true, Some("hard_to_borrow"), Some(false)),
            BorrowStatus::HardToBorrow,
        );
        assert_eq!(
            normalize_borrow_status(true, Some("easy_to_borrow"), Some(true)),
            BorrowStatus::EasyToBorrow,
        );
    }

    fn base_req() -> OrderRequest {
        OrderRequest {
            symbol: " aapl ".into(),
            quantity: 10,
            side: Side::Long,
            order_type: OrderType::Limit,
            limit_price: Some(210.567),
            stop_price: None,
            stop_loss: None,
            take_profit: None,
            time_in_force: None,
            extended_hours: false,
            client_order_id: None,
            reduce_only: false,
        }
    }

    #[test]
    fn the_two_environments_are_different_hosts() {
        assert_eq!(
            base_url(Environment::Paper),
            "https://paper-api.alpaca.markets"
        );
        assert_eq!(base_url(Environment::Live), "https://api.alpaca.markets");
        assert_ne!(base_url(Environment::Paper), base_url(Environment::Live));
    }

    #[test]
    fn a_plain_order_carries_its_idempotency_key_and_normalised_symbol() {
        let b = submit_body(&base_req(), "tagdash-abc");
        assert_eq!(b["symbol"], "AAPL", "symbols are upper-cased and trimmed");
        assert_eq!(b["qty"], "10");
        assert_eq!(b["side"], "buy");
        assert_eq!(b["type"], "limit");
        assert_eq!(b["time_in_force"], "day");
        assert_eq!(b["client_order_id"], "tagdash-abc");
        assert_eq!(b["limit_price"], "210.57", "prices are rounded to the cent");
        assert!(b.get("order_class").is_none(), "no legs ⇒ no bracket");
    }

    #[test]
    fn extended_hours_is_explicit_and_rejects_advanced_or_market_orders() {
        let mut r = base_req();
        r.extended_hours = true;
        assert!(validate_order(&r).is_ok());
        assert_eq!(submit_body(&r, "k")["extended_hours"], true);

        r.stop_loss = Some(200.0);
        assert!(validate_order(&r).is_err());
        r.stop_loss = None;
        r.order_type = OrderType::Market;
        assert!(validate_order(&r).is_err());
    }

    #[test]
    fn sub_dollar_prices_keep_the_four_decimals_alpaca_accepts() {
        let mut r = base_req();
        r.limit_price = Some(0.12345);
        r.stop_loss = Some(0.09876);
        let b = submit_body(&r, "k");
        assert_eq!(b["limit_price"], "0.1235");
        assert_eq!(b["stop_loss"]["stop_price"], "0.0988");
    }

    #[test]
    fn invalid_quantities_and_missing_required_prices_are_rejected_locally() {
        let mut r = base_req();
        r.quantity = 0;
        assert!(validate_order(&r).is_err());

        r.quantity = 10;
        r.limit_price = None;
        assert!(validate_order(&r).is_err());

        r.order_type = OrderType::Market;
        assert!(validate_order(&r).is_ok());
    }

    #[test]
    fn reversed_bracket_levels_are_rejected_before_reaching_the_venue() {
        let mut long = base_req();
        long.stop_loss = Some(220.0);
        long.take_profit = Some(200.0);
        assert!(validate_order(&long).is_err());

        let mut short = base_req();
        short.side = Side::Short;
        short.stop_loss = Some(200.0);
        short.take_profit = Some(220.0);
        assert!(validate_order(&short).is_err());
    }

    #[test]
    fn unsupported_reduce_only_is_not_silently_downgraded_to_a_plain_order() {
        let mut r = base_req();
        r.reduce_only = true;
        assert!(validate_order(&r).is_err());
    }

    /// Both legs ⇒ a real bracket, so the venue owns the OCO. Getting this wrong
    /// leaves a filled position with no server-side protection.
    #[test]
    fn both_protective_legs_become_a_venue_side_bracket() {
        let mut r = base_req();
        r.stop_loss = Some(199.5);
        r.take_profit = Some(225.0);
        let b = submit_body(&r, "k");
        assert_eq!(b["order_class"], "bracket");
        assert_eq!(b["stop_loss"]["stop_price"], "199.50");
        assert_eq!(b["take_profit"]["limit_price"], "225.00");
    }

    #[test]
    fn a_single_protective_leg_becomes_an_oto() {
        let mut r = base_req();
        r.stop_loss = Some(199.5);
        let b = submit_body(&r, "k");
        assert_eq!(b["order_class"], "oto");
        assert!(b.get("take_profit").is_none());
    }

    /// A bracket leg is itself an exit; nesting a bracket inside one is rejected by
    /// the venue, so reduce-only orders must stay plain.
    #[test]
    fn a_reduce_only_exit_never_carries_its_own_bracket() {
        let mut r = base_req();
        r.reduce_only = true;
        r.stop_loss = Some(199.5);
        r.take_profit = Some(225.0);
        let b = submit_body(&r, "k");
        assert!(b.get("order_class").is_none());
        assert!(b.get("stop_loss").is_none());
        assert!(b.get("take_profit").is_none());
    }

    #[test]
    fn a_short_is_a_sell() {
        let mut r = base_req();
        r.side = Side::Short;
        assert_eq!(submit_body(&r, "k")["side"], "sell");
    }

    /// Unknown statuses must never read as terminal: believing a live order is dead
    /// hides real exposure, while the reverse only shows a row the next reconcile
    /// cleans up.
    #[test]
    fn status_mapping_is_conservative() {
        assert_eq!(map_status("filled"), BrokerOrderStatus::Filled);
        assert_eq!(map_status("partially_filled"), BrokerOrderStatus::Open);
        assert_eq!(map_status("new"), BrokerOrderStatus::Open);
        assert_eq!(map_status("canceled"), BrokerOrderStatus::Closed);
        assert_eq!(map_status("rejected"), BrokerOrderStatus::Closed);
        assert_eq!(map_status("expired"), BrokerOrderStatus::Closed);
        assert_eq!(map_status("calculated"), BrokerOrderStatus::Closed);
        assert_eq!(map_status("accepted"), BrokerOrderStatus::Held);
        assert_eq!(map_status("stopped"), BrokerOrderStatus::Held);
        assert_eq!(map_status("suspended"), BrokerOrderStatus::Held);
        assert_eq!(
            map_status("something_new_alpaca_added"),
            BrokerOrderStatus::Held
        );
        assert!(map_status("something_new_alpaca_added").is_live());
        assert!(BrokerOrderStatus::Filled.is_terminal());
    }

    #[test]
    fn generated_client_order_ids_are_valid_and_bounded() {
        let id = prefixed_client_order_id("260814093001-VERYLONGSYMBOL-panic_mean_reversion");
        assert!(id.len() <= 48);
        assert!(id.starts_with("260814093001-VERYLONGSYMBOL-"));
        assert!(id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')));
    }

    /// Alpaca reports a short as qty "5" + side "short". Losing the sign would make
    /// a short position render — and size — as a long.
    #[test]
    fn a_short_position_comes_back_signed_negative() {
        let raw = RawPosition {
            symbol: "TSLA".into(),
            qty: Some("5".into()),
            avg_entry_price: Some("300.10".into()),
            current_price: Some("295.00".into()),
            market_value: Some("-1475.00".into()),
            cost_basis: Some("-1500.50".into()),
            unrealized_pl: Some("25.50".into()),
            unrealized_plpc: Some("0.017".into()),
            side: Some("short".into()),
        };
        let p = to_position(raw);
        assert_eq!(p.quantity, -5.0);
        assert!(p.is_short());
        assert!(
            (p.unrealized_pnl_pct.unwrap() - 1.7).abs() < 1e-9,
            "pct is scaled to %"
        );
    }

    #[test]
    fn a_long_position_stays_positive() {
        let raw = RawPosition {
            symbol: "AAPL".into(),
            qty: Some("10".into()),
            avg_entry_price: Some("210.00".into()),
            current_price: None,
            market_value: None,
            cost_basis: None,
            unrealized_pl: None,
            unrealized_plpc: None,
            side: Some("long".into()),
        };
        let p = to_position(raw);
        assert_eq!(p.quantity, 10.0);
        assert!(!p.is_short());
        assert_eq!(
            p.market_price, None,
            "absent venue values stay absent, never 0"
        );
    }

    /// Bracket legs must surface as top-level orders stamped with their parent,
    /// otherwise cancelling or noticing a protective leg needs a special case.
    #[test]
    fn bracket_legs_are_flattened_and_keep_their_parent() {
        let leg = |id: &str, status: &str| RawOrder {
            id: id.into(),
            client_order_id: format!("c-{id}"),
            symbol: "AAPL".into(),
            side: "sell".into(),
            order_type: "stop".into(),
            time_in_force: "day".into(),
            qty: Some("10".into()),
            filled_qty: Some("0".into()),
            limit_price: None,
            stop_price: Some("199.50".into()),
            filled_avg_price: None,
            status: status.into(),
            legs: None,
            submitted_at: None,
            filled_at: None,
            canceled_at: None,
        };
        let parent = RawOrder {
            legs: Some(vec![leg("leg1", "held"), leg("leg2", "held")]),
            ..leg("parent", "filled")
        };
        let flat = flatten(parent);
        assert_eq!(flat.len(), 3);
        let p = flat.iter().find(|o| o.id == "parent").unwrap();
        assert!(p.parent_id.is_none());
        for id in ["leg1", "leg2"] {
            let l = flat.iter().find(|o| o.id == id).unwrap();
            assert_eq!(l.parent_id.as_deref(), Some("parent"));
        }
    }

    #[test]
    fn remaining_quantity_reflects_a_partial_fill() {
        let o = BrokerOrder {
            id: "1".into(),
            client_order_id: "c".into(),
            symbol: "A".into(),
            side: "buy".into(),
            order_type: "limit".into(),
            time_in_force: "day".into(),
            quantity: 100.0,
            filled_quantity: 40.0,
            limit_price: None,
            stop_price: None,
            filled_avg_price: Some(10.0),
            status: BrokerOrderStatus::Open,
            raw_status: "partially_filled".into(),
            parent_id: None,
            submitted_at: None,
            filled_at: None,
            canceled_at: None,
            reject_reason: None,
        };
        assert_eq!(o.remaining(), 60.0);
    }
}
