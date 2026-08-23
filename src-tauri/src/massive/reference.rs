//! Massive reference-ticker classification.
//!
//! Alpaca's `us_equity` asset class contains common stocks, ETFs, ETNs, funds and
//! other exchange-traded instruments. Massive's daily reference endpoint exposes a
//! finer `type` code (`CS`, `ETF`, `ETN`, ...), which lets Panic Mean Reversion keep
//! operating equities without guessing from the exchange or issuer name.

use serde::Deserialize;

use super::{with_key, BASE_URL, RATE_LIMIT};

const PAGE_LIMIT: u32 = 1000;
const MAX_PAGES: usize = 30;

#[derive(Debug, Deserialize)]
struct Response {
    #[serde(default)]
    results: Vec<RawTicker>,
    #[serde(default)]
    next_url: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawTicker {
    ticker: String,
    #[serde(rename = "type")]
    security_type: Option<String>,
}

/// All active US stock-market ticker type codes, sorted/paginated by Massive.
/// Skips the inter-page throttle when `unlimited` (a confirmed Starter+ tier, see
/// `plan::Tier`) says the account has no rate limit to respect.
pub async fn fetch_security_types(
    api_key: &str,
    unlimited: bool,
) -> Result<Vec<(String, String)>, String> {
    if api_key.trim().is_empty() {
        return Err("clé Massive absente".into());
    }
    let client = crate::http::client();
    let mut url = with_key(
        &format!(
            "{BASE_URL}/v3/reference/tickers?market=stocks&locale=us&active=true\
             &limit={PAGE_LIMIT}&sort=ticker&order=asc"
        ),
        api_key,
    );
    let mut out = Vec::new();

    for page in 0..MAX_PAGES {
        if page > 0 && !unlimited {
            tokio::time::sleep(RATE_LIMIT).await;
        }
        let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!(
                "Massive reference HTTP {status}: {}",
                &body[..body.len().min(200)],
            ));
        }
        let parsed: Response = resp.json().await.map_err(|e| e.to_string())?;
        if parsed.status.as_deref().is_some_and(|s| s != "OK") {
            return Err(format!("Massive reference status: {:?}", parsed.status));
        }
        out.extend(parsed.results.into_iter().filter_map(|r| {
            let symbol = r.ticker.trim().to_ascii_uppercase();
            let kind = r.security_type?.trim().to_ascii_uppercase();
            (!symbol.is_empty() && !kind.is_empty()).then_some((symbol, kind))
        }));
        match parsed.next_url {
            Some(next) if !next.is_empty() => url = with_key(&next, api_key),
            _ => return Ok(out),
        }
    }
    Err(format!("Massive reference: pagination > {MAX_PAGES} pages"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_reference_security_types() {
        let raw = r#"{
            "status":"OK",
            "results":[
                {"ticker":"AAPL","type":"CS"},
                {"ticker":"SPY","type":"ETF"},
                {"ticker":"UNKNOWN"}
            ]
        }"#;
        let parsed: Response = serde_json::from_str(raw).unwrap();
        let rows: Vec<(String, String)> = parsed
            .results
            .into_iter()
            .filter_map(|r| Some((r.ticker, r.security_type?)))
            .collect();
        assert_eq!(
            rows,
            vec![("AAPL".into(), "CS".into()), ("SPY".into(), "ETF".into())]
        );
    }
}
