// Alpaca REST: list all US equity assets.
// Real endpoint: GET https://paper-api.alpaca.markets/v2/assets?status=active&asset_class=us_equity

use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct AlpacaAsset {
    pub symbol: String,
    pub name: Option<String>,
    pub exchange: String,
    pub tradable: bool,
    pub shortable: bool,
    pub status: String,
}

#[derive(Debug, Deserialize)]
struct RawAsset {
    symbol: String,
    name: Option<String>,
    exchange: String,
    tradable: bool,
    shortable: bool,
    status: String,
}

pub async fn fetch_assets(key: &str, secret: &str) -> Result<Vec<AlpacaAsset>, String> {
    let client = crate::http::client();
    let url = "https://paper-api.alpaca.markets/v2/assets?status=active&asset_class=us_equity";
    let resp = client
        .get(url)
        .header("APCA-API-KEY-ID", key)
        .header("APCA-API-SECRET-KEY", secret)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("Alpaca HTTP {status}: {body}"));
    }

    let raw: Vec<RawAsset> = resp.json().await.map_err(|e| e.to_string())?;
    Ok(raw
        .into_iter()
        .map(|r| AlpacaAsset {
            symbol: r.symbol,
            name: r.name,
            exchange: r.exchange,
            tradable: r.tradable,
            shortable: r.shortable,
            status: r.status,
        })
        .collect())
}
