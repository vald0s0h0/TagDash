// FMP (Financial Modeling Prep) client.
// Endpoint: GET https://financialmodelingprep.com/stable/shares-float-all?apikey={key}
// Returns bulk float data for all US stocks.

use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct FmpFloat {
    pub symbol: String,
    pub float_shares: f64,
    pub outstanding_shares: f64,
    pub free_float: f64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawFmpFloat {
    symbol: String,
    float_shares: Option<f64>,
    outstanding_shares: Option<f64>,
    free_float: Option<f64>,
}

pub async fn fetch_shares_float_all(api_key: &str) -> Result<Vec<FmpFloat>, String> {
    let client = crate::http::client();
    let url = format!(
        "https://financialmodelingprep.com/stable/shares-float-all?apikey={api_key}"
    );
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if status.as_u16() == 429 {
            return Err("FMP rate limited — free tier quota exhausted".into());
        }
        return Err(format!("FMP HTTP {status}: {body}"));
    }

    let raw: Vec<RawFmpFloat> = resp.json().await.map_err(|e| e.to_string())?;
    Ok(raw
        .into_iter()
        .filter_map(|r| {
            Some(FmpFloat {
                symbol: r.symbol,
                float_shares: r.float_shares?,
                outstanding_shares: r.outstanding_shares.unwrap_or(0.0),
                free_float: r.free_float.unwrap_or(0.0),
            })
        })
        .collect())
}
