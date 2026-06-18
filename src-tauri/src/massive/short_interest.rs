// Massive bulk short-interest dump — the active short-interest provider for the
// startup pipeline (replaces the per-ticker company_intel path, which only ever
// reached ~50 tickers/launch). Massive mirrors the Polygon API shape, so the
// short-interest endpoint follows the documented Polygon schema (cursor pagination
// via `next_url`, `apiKey` query param, `status:"OK"`).
//
// Endpoint (most-recent settlement first, no ticker filter = whole universe):
//   GET {BASE}/stocks/v1/short-interest?limit=5000&sort=settlement_date.desc
// NOTE the version is `v1` here — float uses `vX`, but short interest 404s on `vX`.
//
// The feed carries one row per (ticker, settlement_date). Because it's globally
// sorted by settlement_date descending, the FIRST time a ticker appears is its most
// recent report — so we keep the first occurrence per ticker and skip the rest. The
// latest settlement date alone usually contains the whole shortable universe in a
// page or two; we stop early once a page yields no new tickers.

use std::collections::HashSet;

use serde::Deserialize;

use super::{with_key, BASE_URL, RATE_LIMIT};

/// One ticker's most recent short-interest report.
#[derive(Debug, Clone)]
pub struct MassiveShortInterest {
    pub symbol: String,
    pub short_interest: Option<i64>,
    pub days_to_cover: Option<f64>,
    pub settlement_date: Option<String>,
}

/// Page size (5000 = the endpoint's max, so the whole shortable universe is ~2
/// rate-limited pages instead of ~7).
const PAGE_LIMIT: u32 = 5000;
/// Safety cap on pages to follow.
const MAX_PAGES: usize = 12;

#[derive(Debug, Deserialize)]
struct Response {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    results: Vec<RawShortInterest>,
    #[serde(default)]
    next_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawShortInterest {
    #[serde(default)]
    ticker: Option<String>,
    #[serde(default)]
    settlement_date: Option<String>,
    #[serde(default)]
    short_interest: Option<f64>,
    #[serde(default)]
    avg_daily_volume: Option<f64>,
    #[serde(default)]
    days_to_cover: Option<f64>,
}

impl RawShortInterest {
    fn into_model(self) -> Option<MassiveShortInterest> {
        let symbol = self.ticker.filter(|t| !t.trim().is_empty())?;
        let short_interest = self.short_interest.map(|v| v as i64);
        // Use the API's days-to-cover when present, else derive it.
        let days_to_cover = self.days_to_cover.or_else(|| {
            match (self.short_interest, self.avg_daily_volume) {
                (Some(si), Some(adv)) if adv > 0.0 => Some(si / adv),
                _ => None,
            }
        });
        // Skip rows with no usable figure at all.
        if short_interest.is_none() && days_to_cover.is_none() {
            return None;
        }
        Some(MassiveShortInterest {
            symbol,
            short_interest,
            days_to_cover,
            settlement_date: self.settlement_date.filter(|s| !s.trim().is_empty()),
        })
    }
}

/// Fetch the latest short interest for the whole universe (one row per ticker),
/// following `next_url` pagination and honouring the free-tier rate limit.
pub async fn fetch_short_interest_all(api_key: &str) -> Result<Vec<MassiveShortInterest>, String> {
    if api_key.trim().is_empty() {
        return Err("missing Massive API key".into());
    }
    let client = reqwest::Client::new();
    let mut out: Vec<MassiveShortInterest> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    let mut url = with_key(
        &format!("{BASE_URL}/stocks/v1/short-interest?limit={PAGE_LIMIT}&sort=settlement_date.desc"),
        api_key,
    );

    for page in 0..MAX_PAGES {
        if page > 0 {
            tokio::time::sleep(RATE_LIMIT).await;
        }
        let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("Massive short-interest HTTP {status}: {}", &body[..body.len().min(200)]));
        }
        let parsed: Response = resp.json().await.map_err(|e| e.to_string())?;
        if let Some(s) = &parsed.status {
            if s != "OK" && s != "DELAYED" {
                return Err(format!("Massive status: {s}"));
            }
        }
        let mut new_this_page = 0usize;
        for raw in parsed.results.into_iter().filter_map(RawShortInterest::into_model) {
            if seen.insert(raw.symbol.to_uppercase()) {
                out.push(raw);
                new_this_page += 1;
            }
        }
        match parsed.next_url {
            // Stop early once a page adds no new tickers (we've passed the latest
            // settlement date into already-seen older reports).
            Some(next) if !next.is_empty() && new_this_page > 0 => url = with_key(&next, api_key),
            _ => break,
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_derives_days_to_cover() {
        let body = r#"{"status":"OK","results":[
            {"ticker":"AAA","settlement_date":"2026-05-15","short_interest":12000000,"avg_daily_volume":3000000},
            {"ticker":"BBB","settlement_date":"2026-05-15","short_interest":9000000,"days_to_cover":2.5}
        ]}"#;
        let parsed: Response = serde_json::from_str(body).unwrap();
        let rows: Vec<MassiveShortInterest> =
            parsed.results.into_iter().filter_map(RawShortInterest::into_model).collect();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].short_interest, Some(12_000_000));
        assert_eq!(rows[0].days_to_cover, Some(4.0));
        assert_eq!(rows[1].days_to_cover, Some(2.5));
    }

    #[test]
    fn skips_rows_without_ticker_or_figures() {
        let body = r#"{"status":"OK","results":[
            {"settlement_date":"2026-05-15","short_interest":1000},
            {"ticker":"X","settlement_date":"2026-05-15"}
        ]}"#;
        let parsed: Response = serde_json::from_str(body).unwrap();
        let rows: Vec<MassiveShortInterest> =
            parsed.results.into_iter().filter_map(RawShortInterest::into_model).collect();
        assert!(rows.is_empty());
    }
}
