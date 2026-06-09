// Alpaca REST: recent news articles for one symbol (with full content).
// Endpoint: GET https://data.alpaca.markets/v1beta1/news
//
// Used by the panic mean-reversion LLM read (button-triggered): we pull the last
// few days of headlines + article bodies for the ticker and hand them to Deepseek
// as grounding. `include_content=true` returns the full HTML body; the caller
// strips the markup before prompting.

use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;

const NEWS_URL: &str = "https://data.alpaca.markets/v1beta1/news";

/// One news article (the fields we use).
#[derive(Debug, Clone)]
pub struct NewsArticle {
    pub created_at: DateTime<Utc>,
    pub headline:   String,
    pub summary:    String,
    /// Full article body, still HTML (caller strips tags).
    pub content:    String,
    pub source:     String,
}

#[derive(Debug, Deserialize)]
struct NewsResponse {
    #[serde(default)]
    news: Vec<RawArticle>,
}

#[derive(Debug, Deserialize)]
struct RawArticle {
    headline:   Option<String>,
    summary:    Option<String>,
    content:    Option<String>,
    source:     Option<String>,
    created_at: Option<String>,
}

/// Fetch up to `limit` of the most recent articles for `symbol` published within
/// the last `days` days (newest first). Returns an empty Vec when the keys are
/// missing or the request fails (the caller degrades gracefully).
pub async fn fetch_recent_news(
    key: &str,
    secret: &str,
    symbol: &str,
    days: i64,
    limit: u32,
) -> Result<Vec<NewsArticle>, String> {
    let start = (Utc::now() - Duration::days(days.max(1)))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();

    let url = reqwest::Url::parse_with_params(
        NEWS_URL,
        &[
            ("symbols", symbol),
            ("start", start.as_str()),
            ("limit", &limit.to_string()),
            ("sort", "desc"),
            ("include_content", "true"),
            ("exclude_contentless", "false"),
        ],
    )
    .map_err(|e| e.to_string())?;

    let client = reqwest::Client::new();
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
        let snippet = &body[..body.len().min(200)];
        return Err(format!("Alpaca news HTTP {status}: {snippet}"));
    }

    let raw: NewsResponse = resp.json().await.map_err(|e| e.to_string())?;
    let articles = raw
        .news
        .into_iter()
        .filter_map(|a| {
            let headline = a.headline.filter(|h| !h.trim().is_empty())?;
            let created_at = a
                .created_at
                .as_deref()
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|d| d.with_timezone(&Utc))
                .unwrap_or_else(Utc::now);
            Some(NewsArticle {
                created_at,
                headline,
                summary: a.summary.unwrap_or_default(),
                content: a.content.unwrap_or_default(),
                source:  a.source.unwrap_or_default(),
            })
        })
        .collect();
    Ok(articles)
}
