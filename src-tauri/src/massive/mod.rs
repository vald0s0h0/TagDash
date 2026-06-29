// Massive (api.massive.com) client — bulk free-float data, the active float
// provider (replaces FMP, whose code is kept for fallback/legacy).
//
// Endpoint: GET https://api.massive.com/stocks/vX/float?limit=5000&apiKey={key}
//   → { status, request_id, results: [{ ticker, free_float, free_float_percent,
//        effective_date }], next_url }
// Pagination is cursor-based via `next_url` (the apiKey is NOT carried over, so
// we re-append it on each follow-up page).
//
// Free tier rate limit: 1 request / 13 s. We sleep between pages accordingly,
// which is why the startup pipeline only refreshes floats once per calendar day.

use std::time::Duration;

use serde::Deserialize;

pub mod news;
pub mod short_interest;
pub mod splits;

/// Free tier allows ~5 requests/minute → 1 request every 13 s (with margin).
pub(crate) const RATE_LIMIT: Duration = Duration::from_secs(13);
/// Max results per page (endpoint hard cap).
const PAGE_LIMIT: u32 = 5000;
/// Safety cap on pages to follow, so a misbehaving cursor can't loop forever.
const MAX_PAGES: usize = 20;
pub(crate) const BASE_URL: &str = "https://api.massive.com";

/// One symbol's float. Field names/semantics mirror `crate::fmp::FmpFloat` so the
/// startup pipeline can use either provider interchangeably:
/// - `float_shares`       = free-float shares
/// - `outstanding_shares` = derived from the free-float percentage
/// - `free_float`         = free-float **percentage** (0–100)
#[derive(Debug, Clone)]
pub struct MassiveFloat {
    pub symbol: String,
    pub float_shares: f64,
    pub outstanding_shares: f64,
    pub free_float: f64,
}

#[derive(Debug, Deserialize)]
struct FloatResponse {
    #[serde(default)]
    results: Vec<RawFloat>,
    #[serde(default)]
    next_url: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawFloat {
    // The bulk dump occasionally contains rows with no ticker — keep this
    // optional so one bad row doesn't fail the whole page.
    #[serde(default)]
    ticker: Option<String>,
    free_float: Option<f64>,
    free_float_percent: Option<f64>,
}

impl RawFloat {
    fn into_float(self) -> Option<MassiveFloat> {
        let symbol = self.ticker.filter(|t| !t.trim().is_empty())?;
        let float_shares = self.free_float?;
        let pct = self.free_float_percent.unwrap_or(0.0);
        // outstanding = float / (pct/100), only when the percentage is usable.
        let outstanding_shares = if pct > 0.0 {
            float_shares / (pct / 100.0)
        } else {
            0.0
        };
        Some(MassiveFloat {
            symbol,
            float_shares,
            outstanding_shares,
            free_float: pct,
        })
    }
}

/// Append `apiKey` to a URL that may or may not already carry query params.
pub(crate) fn with_key(url: &str, api_key: &str) -> String {
    let sep = if url.contains('?') { '&' } else { '?' };
    format!("{url}{sep}apiKey={api_key}")
}

/// Fetch the full-universe free-float dump, following `next_url` pagination.
///
/// Respects the free-tier rate limit by sleeping `RATE_LIMIT` between page
/// requests. With ~7–8k US equities at 5000/page this is ~2 pages (≈13 s).
pub async fn fetch_float_all(api_key: &str) -> Result<Vec<MassiveFloat>, String> {
    let client = crate::http::client();
    let mut out: Vec<MassiveFloat> = Vec::new();

    let mut url = with_key(
        &format!("{BASE_URL}/stocks/vX/float?limit={PAGE_LIMIT}&sort=ticker.asc"),
        api_key,
    );

    for page in 0..MAX_PAGES {
        // Throttle every request after the first to honour the free-tier limit.
        if page > 0 {
            tokio::time::sleep(RATE_LIMIT).await;
        }

        let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            if status.as_u16() == 429 {
                eprintln!("[tagdash][massive] rate limited — stopping float fetch");
                break;
            }
            let snippet = &body[..body.len().min(200)];
            return Err(format!("Massive HTTP {status}: {snippet}"));
        }

        let parsed: FloatResponse = resp.json().await.map_err(|e| e.to_string())?;
        if let Some(s) = &parsed.status {
            if s != "OK" {
                return Err(format!("Massive status: {s}"));
            }
        }

        out.extend(parsed.results.into_iter().filter_map(RawFloat::into_float));

        match parsed.next_url {
            Some(next) if !next.is_empty() => url = with_key(&next, api_key),
            _ => break,
        }
    }

    Ok(out)
}

/// Fetch the float for a single ticker (used by tests / on-demand lookups).
pub async fn fetch_float(api_key: &str, ticker: &str) -> Result<Option<MassiveFloat>, String> {
    let client = crate::http::client();
    let url = with_key(
        &format!("{BASE_URL}/stocks/vX/float?ticker={ticker}"),
        api_key,
    );
    let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("Massive HTTP {}", resp.status()));
    }
    let parsed: FloatResponse = resp.json().await.map_err(|e| e.to_string())?;
    Ok(parsed.results.into_iter().filter_map(RawFloat::into_float).next())
}

/// One symbol's float as it stood on (or just before) a requested past date.
#[derive(Debug, Clone)]
pub struct AsOfFloat {
    pub float_shares: f64,
    /// The `effective_date` of the value Massive returned (YYYY-MM-DD), if any.
    pub effective_date: Option<String>,
    /// true when the value is actually effective on/before the requested day; false
    /// when no dated history was available and we fell back to the latest float.
    pub historical: bool,
}

#[derive(Debug, Deserialize)]
struct AsOfResponse {
    #[serde(default)]
    results: Vec<RawAsOf>,
}

#[derive(Debug, Deserialize, Clone)]
struct RawAsOf {
    free_float: Option<f64>,
    #[serde(default)]
    effective_date: Option<String>,
}

/// Fetch the float for `ticker` AS OF `day` (YYYY-MM-DD). The float is a Micro Pullback
/// filtering condition and drifts over time, so for a historical TRADE download we want
/// the value effective back then, not today's. Massive carries an `effective_date`; we
/// ask with a `date` filter and pick the most recent row whose `effective_date ≤ day`.
/// If no dated history comes back we fall back to the latest float (flagged
/// `historical = false`). Returns None when the ticker has no usable float.
pub async fn fetch_float_asof(
    api_key: &str,
    ticker: &str,
    day: &str,
) -> Result<Option<AsOfFloat>, String> {
    let client = crate::http::client();
    let url = with_key(
        &format!("{BASE_URL}/stocks/vX/float?ticker={ticker}&date={day}&limit=50"),
        api_key,
    );
    let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("Massive HTTP {}", resp.status()));
    }
    let parsed: AsOfResponse = resp.json().await.map_err(|e| e.to_string())?;
    Ok(pick_asof(parsed.results, day))
}

/// Choose the float row effective on/before `day` (most recent such), else the latest
/// row available (flagged non-historical). Pure so it is unit-testable.
fn pick_asof(rows: Vec<RawAsOf>, day: &str) -> Option<AsOfFloat> {
    let rows: Vec<RawAsOf> = rows.into_iter().filter(|r| r.free_float.is_some()).collect();
    // Historical candidates: effective_date present and ≤ day. Lexicographic compare is
    // valid for zero-padded YYYY-MM-DD.
    let best_hist = rows
        .iter()
        .filter(|r| r.effective_date.as_deref().map(|e| e <= day).unwrap_or(false))
        .max_by(|a, b| a.effective_date.cmp(&b.effective_date));
    if let Some(r) = best_hist {
        return Some(AsOfFloat {
            float_shares: r.free_float.unwrap(),
            effective_date: r.effective_date.clone(),
            historical: true,
        });
    }
    // Fallback: the latest row Massive returned.
    rows.into_iter().max_by(|a, b| a.effective_date.cmp(&b.effective_date)).map(|r| AsOfFloat {
        float_shares: r.free_float.unwrap(),
        effective_date: r.effective_date,
        historical: false,
    })
}

/// Mock float data — mirrors the FMP mock set so dev/mock mode behaves the same
/// whichever provider is wired in.
pub fn mock_float_all() -> Vec<MassiveFloat> {
    crate::fmp::mock_shares_float_all()
        .into_iter()
        .map(|f| MassiveFloat {
            symbol: f.symbol,
            float_shares: f.float_shares,
            outstanding_shares: f.outstanding_shares,
            free_float: f.free_float,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_results_and_derives_outstanding() {
        // Real-shape payload captured from api.massive.com.
        let body = r#"{
            "status":"OK","request_id":"abc",
            "results":[
                {"ticker":"AAPL","free_float":13515457484,"effective_date":"2026-03-05","free_float_percent":92.1},
                {"ticker":"NOPCT","free_float":1000000,"effective_date":"2026-03-05","free_float_percent":0}
            ]
        }"#;
        let parsed: FloatResponse = serde_json::from_str(body).unwrap();
        let floats: Vec<MassiveFloat> =
            parsed.results.into_iter().filter_map(RawFloat::into_float).collect();
        assert_eq!(floats.len(), 2);
        assert_eq!(floats[0].symbol, "AAPL");
        assert_eq!(floats[0].float_shares, 13_515_457_484.0);
        // 13.515B / 0.921 ≈ 14.674B outstanding
        assert!((floats[0].outstanding_shares - 14_674_763_826.0).abs() < 1_000_000.0);
        // Zero percent → outstanding 0 (avoid divide-by-zero).
        assert_eq!(floats[1].outstanding_shares, 0.0);
    }

    #[test]
    fn with_key_handles_existing_query() {
        assert_eq!(with_key("https://x/float", "K"), "https://x/float?apiKey=K");
        assert_eq!(with_key("https://x/float?cursor=AB", "K"), "https://x/float?cursor=AB&apiKey=K");
    }

    #[test]
    fn missing_free_float_is_skipped() {
        let body = r#"{"status":"OK","results":[{"ticker":"X","free_float_percent":50.0}]}"#;
        let parsed: FloatResponse = serde_json::from_str(body).unwrap();
        let floats: Vec<MassiveFloat> =
            parsed.results.into_iter().filter_map(RawFloat::into_float).collect();
        assert!(floats.is_empty());
    }

    #[test]
    fn asof_picks_most_recent_on_or_before_day() {
        let body = r#"{"results":[
            {"free_float":1000000,"effective_date":"2026-01-10"},
            {"free_float":2000000,"effective_date":"2026-03-01"},
            {"free_float":3000000,"effective_date":"2026-06-20"}
        ]}"#;
        let parsed: AsOfResponse = serde_json::from_str(body).unwrap();
        let got = pick_asof(parsed.results, "2026-04-01").unwrap();
        assert_eq!(got.float_shares, 2_000_000.0);
        assert!(got.historical);
        assert_eq!(got.effective_date.as_deref(), Some("2026-03-01"));
    }

    #[test]
    fn asof_falls_back_to_latest_when_no_history_before_day() {
        let body = r#"{"results":[{"free_float":3000000,"effective_date":"2026-06-20"}]}"#;
        let parsed: AsOfResponse = serde_json::from_str(body).unwrap();
        let got = pick_asof(parsed.results, "2026-01-01").unwrap();
        assert_eq!(got.float_shares, 3_000_000.0);
        assert!(!got.historical);
    }

    #[test]
    fn rows_without_ticker_dont_fail_the_page() {
        // The real bulk dump contains rows with no `ticker` field — they must
        // be skipped, not abort decoding of the whole page.
        let body = r#"{"status":"OK","results":[
            {"ticker":"RGA","free_float":65274693,"free_float_percent":99.6},
            {"free_float":3950100,"effective_date":"2026-01-29","free_float_percent":20.5}
        ]}"#;
        let parsed: FloatResponse = serde_json::from_str(body).unwrap();
        let floats: Vec<MassiveFloat> =
            parsed.results.into_iter().filter_map(RawFloat::into_float).collect();
        assert_eq!(floats.len(), 1);
        assert_eq!(floats[0].symbol, "RGA");
    }

    // Live integration test — what does Massive actually return?
    //   cargo test -p tagdash live_massive -- --ignored --nocapture
    // with MASSIVE_API_KEY set in the environment.
    #[tokio::test]
    #[ignore = "hits live Massive API; set MASSIVE_API_KEY"]
    async fn live_massive_float() {
        let key = std::env::var("MASSIVE_API_KEY").expect("set MASSIVE_API_KEY");
        let one = fetch_float(&key, "AAPL").await.unwrap().expect("AAPL float");
        eprintln!("AAPL: {one:?}");
        assert!(one.float_shares > 0.0);

        let all = fetch_float_all(&key).await.unwrap();
        eprintln!("bulk float records: {}", all.len());
        eprintln!("sample: {:?}", &all[..all.len().min(3)]);
        assert!(all.len() > 1000, "expected a full-universe dump");
    }
}
