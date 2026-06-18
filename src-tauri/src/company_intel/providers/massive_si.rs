// Massive short-interest provider. Massive (api.massive.com) mirrors the
// Polygon.io API shape (cursor pagination, `apiKey` query param, `status:"OK"`),
// so the short-interest endpoint follows the documented Polygon short-interest
// schema. Best-effort + defensive: a wrong field name or a 404 degrades to
// `NotFound`/`Unavailable` rather than aborting the whole ticker.
//
// Endpoint (most-recent settlement first). NOTE the version is `v1` (float uses
// `vX`, but short interest 404s on `vX`):
//   GET {BASE}/stocks/v1/short-interest?ticker={t}&limit=1&sort=settlement_date.desc

use serde::Deserialize;

use super::super::error::{IntelError, IntelResult};
use super::super::http::{Http, RetryPolicy};
use super::super::model::ShortInterest;
use super::super::rate_limit::RateLimiter;

#[derive(Debug, Deserialize)]
struct Response {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    results: Vec<RawShortInterest>,
}

#[derive(Debug, Deserialize)]
struct RawShortInterest {
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
    fn into_model(self) -> ShortInterest {
        let short_interest = self.short_interest.map(|v| v as i64);
        // Use the API's days-to-cover when present, else derive it.
        let days_to_cover = self.days_to_cover.or_else(|| {
            match (self.short_interest, self.avg_daily_volume) {
                (Some(si), Some(adv)) if adv > 0.0 => Some(si / adv),
                _ => None,
            }
        });
        ShortInterest {
            short_interest,
            days_to_cover,
            settlement_date: self.settlement_date.filter(|s| !s.trim().is_empty()),
        }
    }
}

/// Fetch the most recent short-interest report for `ticker` from Massive.
pub async fn fetch_short_interest(
    http: &Http,
    limiter: &RateLimiter,
    policy: &RetryPolicy,
    api_key: &str,
    ticker: &str,
) -> IntelResult<ShortInterest> {
    if api_key.trim().is_empty() {
        return Err(IntelError::MissingKey);
    }
    let path = format!(
        "{}/stocks/v1/short-interest?ticker={}&limit=1&sort=settlement_date.desc",
        crate::massive::BASE_URL,
        ticker
    );
    let url = crate::massive::with_key(&path, api_key);
    let resp: Response = http.get_json(limiter, &url, &[], policy).await?;
    if let Some(s) = &resp.status {
        if s != "OK" && s != "DELAYED" {
            return Err(IntelError::Unavailable(format!("Massive status: {s}")));
        }
    }
    match resp.results.into_iter().next() {
        Some(r) => {
            let si = r.into_model();
            if si.is_empty() { Err(IntelError::NotFound) } else { Ok(si) }
        }
        None => Err(IntelError::NotFound),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_derives_days_to_cover() {
        let body = r#"{"status":"OK","results":[
            {"settlement_date":"2026-05-15","short_interest":12000000,"avg_daily_volume":3000000}
        ]}"#;
        let resp: Response = serde_json::from_str(body).unwrap();
        let si = resp.results.into_iter().next().unwrap().into_model();
        assert_eq!(si.short_interest, Some(12_000_000));
        assert_eq!(si.days_to_cover, Some(4.0));
        assert_eq!(si.settlement_date.as_deref(), Some("2026-05-15"));
    }

    #[test]
    fn prefers_api_days_to_cover() {
        let body = r#"{"status":"OK","results":[
            {"settlement_date":"2026-05-15","short_interest":9000000,"avg_daily_volume":3000000,"days_to_cover":2.5}
        ]}"#;
        let resp: Response = serde_json::from_str(body).unwrap();
        let si = resp.results.into_iter().next().unwrap().into_model();
        assert_eq!(si.days_to_cover, Some(2.5));
    }
}
