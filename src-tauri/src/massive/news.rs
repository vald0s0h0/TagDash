// Massive news: most recent headlines for a ticker.
//
// Official endpoint (included in every Stocks plan; hourly recency):
//   GET https://api.massive.com/v2/reference/news?ticker={T}&limit=10
//       &sort=published_utc&order=desc&apiKey={key}
//
// MARKET REPLAY. The clamp lives HERE, not at the call sites, for the same reason
// `alpaca::news` puts it inside its own fetchers: this endpoint is reached from the
// Micro Pullback A+ gather and from the Panic LLM context cascade, and an unbounded
// "most recent headlines" answer during a replay is the purest form of look-ahead —
// it tells the model how the replayed session ended. While `replay::clock` is active
// every request carries `published_utc.lte = <simulated instant>`, so a caller cannot
// forget it. The replay DAY LOADER does not come through here: it fetches the whole
// day up front and releases each headline when the simulated clock reaches its own
// `created_at` (see `replay::data::fetch_massive_news_window`).

use serde::Deserialize;

use super::{with_key, BASE_URL};

/// `published_utc.lte` bound while a Market Replay is running — the simulated
/// instant. `None` in live mode (no clamp).
fn replay_upper_bound() -> Option<String> {
    crate::replay::clock::is_active()
        .then(|| crate::time::now().format("%Y-%m-%dT%H:%M:%SZ").to_string())
}

#[derive(Debug, Clone)]
pub struct NewsItem {
    pub title: String,
    pub url: Option<String>,
    pub published_at: Option<String>,
    pub summary: Option<String>,
    pub source: Option<String>,
}

#[derive(Debug, Deserialize)]
struct NewsResponse {
    #[serde(default)]
    results: Vec<RawNews>,
}

#[derive(Debug, Deserialize)]
struct RawNews {
    #[serde(default, alias = "title", alias = "headline")]
    title: Option<String>,
    #[serde(default, alias = "article_url", alias = "url")]
    url: Option<String>,
    #[serde(
        default,
        alias = "published_utc",
        alias = "published_at",
        alias = "created_at"
    )]
    published_at: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    publisher: Option<RawPublisher>,
}

#[derive(Debug, Deserialize)]
struct RawPublisher {
    #[serde(default)]
    name: Option<String>,
}

impl RawNews {
    fn into_item(self) -> Option<NewsItem> {
        let title = self.title.filter(|t| !t.trim().is_empty())?;
        Some(NewsItem {
            title,
            url: self.url,
            published_at: self.published_at,
            summary: self.description,
            source: self.publisher.and_then(|p| p.name),
        })
    }
}

/// Fetch the most recent news items for `ticker` (newest first).
///
/// During a Market Replay the window is capped at the simulated instant, so a
/// headline published later in the replayed day can never reach a caller.
pub async fn fetch_latest_news(api_key: &str, ticker: &str) -> Result<Vec<NewsItem>, String> {
    let mut path = format!(
        "{BASE_URL}/v2/reference/news?ticker={ticker}&limit=10&sort=published_utc&order=desc"
    );
    if let Some(lte) = replay_upper_bound() {
        path.push_str(&format!("&published_utc.lte={lte}"));
    }
    let url = with_key(&path, api_key);
    let client = crate::http::client();
    let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        let status = resp.status();
        if status.as_u16() == 429 {
            return Err("Massive news rate limited".into());
        }
        return Err(format!("Massive news HTTP {status}"));
    }
    let parsed: NewsResponse = resp.json().await.map_err(|e| e.to_string())?;
    Ok(parsed
        .results
        .into_iter()
        .filter_map(RawNews::into_item)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_news_shape_keeps_catalyst_context() {
        let raw: NewsResponse = serde_json::from_str(
            r#"{"results":[{"title":"FDA approval","article_url":"https://example.test/a","published_utc":"2026-08-21T08:00:00Z","description":"Phase 3 result","publisher":{"name":"Business Wire"}}]}"#,
        )
        .unwrap();
        let item = raw.results.into_iter().next().unwrap().into_item().unwrap();
        assert_eq!(item.title, "FDA approval");
        assert_eq!(item.summary.as_deref(), Some("Phase 3 result"));
        assert_eq!(item.source.as_deref(), Some("Business Wire"));
    }
}
