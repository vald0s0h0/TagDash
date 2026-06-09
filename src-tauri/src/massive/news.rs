// Massive news: most recent headlines for a ticker.
//
// Assumed endpoint (modelled on the float endpoint, confirmed base + auth):
//   GET https://api.massive.com/stocks/vX/news?ticker={T}&limit=5
//        &sort=published_utc.desc&apiKey={key}
// Field names are guessed and covered by serde aliases — adjust here once the
// live response shape is confirmed. Massive has no news streaming on the free
// tier, so the enrichment pipeline polls this a bounded number of times before
// declaring "no news" (see crate::enrichment).

use serde::Deserialize;

use super::{with_key, BASE_URL};

#[derive(Debug, Clone)]
pub struct NewsItem {
    pub title:        String,
    pub url:          Option<String>,
    pub published_at: Option<String>,
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
    #[serde(default, alias = "published_utc", alias = "published_at", alias = "created_at")]
    published_at: Option<String>,
}

impl RawNews {
    fn into_item(self) -> Option<NewsItem> {
        let title = self.title.filter(|t| !t.trim().is_empty())?;
        Some(NewsItem {
            title,
            url:          self.url,
            published_at: self.published_at,
        })
    }
}

/// Fetch the most recent news items for `ticker` (newest first).
pub async fn fetch_latest_news(api_key: &str, ticker: &str) -> Result<Vec<NewsItem>, String> {
    let url = with_key(
        &format!("{BASE_URL}/stocks/vX/news?ticker={ticker}&limit=5&sort=published_utc.desc"),
        api_key,
    );
    let client = reqwest::Client::new();
    let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("Massive news HTTP {}", resp.status()));
    }
    let parsed: NewsResponse = resp.json().await.map_err(|e| e.to_string())?;
    Ok(parsed.results.into_iter().filter_map(RawNews::into_item).collect())
}
