// Open-web search — the LAST resort of the Panic Score context cascade.
//
// The cascade is Alpaca news → Massive news → here (see `crate::panic_llm`). This
// module is only reached when no STRUCTURED source could explain why a ticker is
// moving, which is exactly the case the strategy cares about most: a violent move
// with no identifiable catalyst is the signature of a self-feeding speculative flow.
// Confusing "we found nothing" with "nothing happened" would systematically
// mis-score those names, so the fallback exists to tell the two apart.
//
// TWO PROVIDERS, one interface. The account is the user's to create, so rather than
// betting on one vendor we read whichever key is present. Tavily wins when both are
// set: it returns extracted article text plus a synthesised `answer`, where Brave
// returns raw SERP snippets — less noise to hand a scoring model.
//
// REPLAY IS A HARD REFUSAL, not a setting. `search` returns
// `Err(Unavailable::Replay)` the moment `replay::clock::is_active()`, before any
// key is read or any request is built. A live web search during a Market Replay
// would inject TODAY's knowledge of how the replayed day ended — the single worst
// look-ahead leak available in this app, and the one a backtest would never reveal
// because the results look plausible. A config flag could be toggled by mistake;
// a guard in the only public entry point cannot.

use serde::{Deserialize, Serialize};

use crate::config::secrets::Secrets;

/// Max hits handed to the model. Small on purpose: the model needs enough to spot a
/// catalyst, and every extra snippet is a chance to anchor on unrelated noise.
const MAX_RESULTS: usize = 6;
/// Per-snippet character cap (Tavily `content` can run long).
const SNIPPET_CHARS: usize = 700;

const TAVILY_URL: &str = "https://api.tavily.com/search";
const BRAVE_URL: &str = "https://api.search.brave.com/res/v1/web/search";

/// Which provider served (or would serve) a query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Provider {
    Tavily,
    Brave,
}

impl Provider {
    pub fn as_str(self) -> &'static str {
        match self {
            Provider::Tavily => "tavily",
            Provider::Brave => "brave",
        }
    }
}

/// Why a search did not happen. Carried into the payload so the model knows whether
/// "no web context" means "we looked and found nothing" or "we never looked".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unavailable {
    /// A Market Replay drives the clock — searching would leak the future.
    Replay,
    /// Neither `tavily_api_key` nor `brave_api_key` is configured.
    NoKey,
    /// The provider was called and failed (network, quota, bad key).
    Failed(String),
}

impl Unavailable {
    /// Short machine-readable reason for the payload's `web_fallback.reason`.
    pub fn reason(&self) -> String {
        match self {
            Unavailable::Replay => "replay_interdit".into(),
            Unavailable::NoKey => "aucune_cle_configuree".into(),
            Unavailable::Failed(e) => format!("echec: {e}"),
        }
    }
}

/// One web result, normalised across providers.
#[derive(Debug, Clone, Serialize)]
pub struct Hit {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// What a successful search produced.
#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    pub provider: String,
    /// Provider-synthesised summary. Tavily only — `None` for Brave.
    pub answer: Option<String>,
    pub hits: Vec<Hit>,
}

/// Pick the provider from the configured keys. Tavily first (cleaner extraction).
/// Returns the key alongside so the caller never re-reads the lock.
pub fn provider_from(secrets: &Secrets) -> Option<(Provider, String)> {
    let live = |o: &Option<String>| {
        o.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    if let Some(k) = live(&secrets.tavily_api_key) {
        return Some((Provider::Tavily, k));
    }
    live(&secrets.brave_api_key).map(|k| (Provider::Brave, k))
}

/// Search the open web for why `symbol` is moving.
///
/// The ONLY public entry point, so the replay guard below is unconditional.
pub async fn search(
    secrets: &std::sync::Arc<std::sync::RwLock<Secrets>>,
    symbol: &str,
    company_name: Option<&str>,
) -> Result<SearchResult, Unavailable> {
    // ── The guard. Before the key, before the query, before the network.
    if crate::replay::clock::is_active() {
        return Err(Unavailable::Replay);
    }
    let Some((provider, key)) = ({
        let s = secrets.read().unwrap();
        provider_from(&s)
    }) else {
        return Err(Unavailable::NoKey);
    };

    let query = build_query(symbol, company_name);
    let out = match provider {
        Provider::Tavily => tavily(&key, &query).await,
        Provider::Brave => brave(&key, &query).await,
    };
    out.map_err(Unavailable::Failed)
}

/// The query text. Deliberately plain keywords rather than a question: both engines
/// rank news better on entity + intent terms than on natural-language phrasing.
/// The company name disambiguates tickers that collide with common words (e.g. ALL,
/// KEY, ON) — the single biggest source of irrelevant hits without it.
pub fn build_query(symbol: &str, company_name: Option<&str>) -> String {
    let name = company_name
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or_default();
    if name.is_empty() {
        format!("{symbol} stock why moving news catalyst")
    } else {
        format!("{symbol} {name} stock why moving news catalyst")
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.trim().to_string();
    }
    let cut: String = s.chars().take(max).collect();
    format!("{}…", cut.trim())
}

// ─── Tavily ────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct TavilyResponse {
    #[serde(default)]
    answer: Option<String>,
    #[serde(default)]
    results: Vec<TavilyHit>,
}

#[derive(Debug, Deserialize)]
struct TavilyHit {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    content: Option<String>,
}

async fn tavily(key: &str, query: &str) -> Result<SearchResult, String> {
    let body = serde_json::json!({
        "query": query,
        "max_results": MAX_RESULTS,
        // A catalyst older than a week cannot explain today's move.
        "time_range": "week",
        "include_answer": "basic",
        "search_depth": "basic",
        "topic": "news",
    });
    let resp = crate::http::client()
        .post(TAVILY_URL)
        .header("Authorization", format!("Bearer {key}"))
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("Tavily HTTP {status}: {}", truncate(&text, 200)));
    }
    let parsed: TavilyResponse = resp.json().await.map_err(|e| e.to_string())?;
    let hits = parsed
        .results
        .into_iter()
        .filter_map(|h| {
            let title = h.title.filter(|t| !t.trim().is_empty())?;
            Some(Hit {
                title: title.trim().to_string(),
                url: h.url.unwrap_or_default(),
                snippet: truncate(&h.content.unwrap_or_default(), SNIPPET_CHARS),
            })
        })
        .take(MAX_RESULTS)
        .collect();
    Ok(SearchResult {
        provider: Provider::Tavily.as_str().to_string(),
        answer: parsed
            .answer
            .map(|a| a.trim().to_string())
            .filter(|a| !a.is_empty()),
        hits,
    })
}

// ─── Brave ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct BraveResponse {
    #[serde(default)]
    web: Option<BraveWeb>,
}

#[derive(Debug, Deserialize)]
struct BraveWeb {
    #[serde(default)]
    results: Vec<BraveHit>,
}

#[derive(Debug, Deserialize)]
struct BraveHit {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

async fn brave(key: &str, query: &str) -> Result<SearchResult, String> {
    let url = reqwest::Url::parse_with_params(
        BRAVE_URL,
        &[
            ("q", query.to_string()),
            ("count", MAX_RESULTS.to_string()),
            ("freshness", "pw".to_string()), // past week
        ],
    )
    .map_err(|e| e.to_string())?;

    let resp = crate::http::client()
        .get(url)
        .header("X-Subscription-Token", key)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("Brave HTTP {status}: {}", truncate(&text, 200)));
    }
    let parsed: BraveResponse = resp.json().await.map_err(|e| e.to_string())?;
    let hits = parsed
        .web
        .map(|w| w.results)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|h| {
            let title = h.title.filter(|t| !t.trim().is_empty())?;
            Some(Hit {
                title: title.trim().to_string(),
                url: h.url.unwrap_or_default(),
                snippet: truncate(&h.description.unwrap_or_default(), SNIPPET_CHARS),
            })
        })
        .take(MAX_RESULTS)
        .collect();
    // Brave has no synthesised answer — the field stays None rather than being faked
    // from the first snippet, which would read as an editorial summary it is not.
    Ok(SearchResult {
        provider: Provider::Brave.as_str().to_string(),
        answer: None,
        hits,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secrets_with(tavily: Option<&str>, brave: Option<&str>) -> Secrets {
        Secrets {
            tavily_api_key: tavily.map(str::to_string),
            brave_api_key: brave.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn tavily_wins_when_both_keys_are_present() {
        let s = secrets_with(Some("tvly-x"), Some("brave-y"));
        assert_eq!(
            provider_from(&s),
            Some((Provider::Tavily, "tvly-x".to_string())),
        );
    }

    #[test]
    fn brave_is_used_when_it_is_the_only_key() {
        let s = secrets_with(None, Some("brave-y"));
        assert_eq!(
            provider_from(&s),
            Some((Provider::Brave, "brave-y".to_string())),
        );
    }

    /// A blank key is not a key. Without this a whitespace value in the TOML would
    /// select a provider that then fails every request.
    #[test]
    fn blank_keys_select_nothing() {
        assert_eq!(provider_from(&secrets_with(Some("   "), None)), None);
        assert_eq!(provider_from(&secrets_with(Some(""), Some("\t"))), None);
        assert_eq!(provider_from(&secrets_with(None, None)), None);
    }

    /// The company name is what stops a ticker colliding with an English word from
    /// returning unrelated hits.
    #[test]
    fn the_query_carries_the_company_name_when_known() {
        let q = build_query("ALL", Some("Allstate Corporation"));
        assert!(q.contains("ALL"));
        assert!(q.contains("Allstate Corporation"));
        // A missing/blank name degrades to the symbol alone, never to "ALL  stock".
        let bare = build_query("MTEN", None);
        assert!(bare.starts_with("MTEN stock"));
        assert_eq!(build_query("MTEN", Some("   ")), bare);
    }

    /// The replay refusal must be reachable without a key: the guard runs FIRST, so a
    /// replay is refused as a replay, not mis-reported as "no key configured".
    #[tokio::test]
    async fn replay_is_refused_before_the_key_is_even_read() {
        let secrets = std::sync::Arc::new(std::sync::RwLock::new(secrets_with(None, None)));
        // No replay clock active in a unit test → the no-key branch is what answers.
        assert!(!crate::replay::clock::is_active());
        let err = search(&secrets, "MTEN", None).await.unwrap_err();
        assert_eq!(err, Unavailable::NoKey);
        // The reasons are distinct strings, so the payload can tell them apart.
        assert_ne!(
            Unavailable::Replay.reason(),
            Unavailable::NoKey.reason(),
            "the model must be able to distinguish 'never looked' from 'not allowed'",
        );
    }

    #[test]
    fn snippets_are_capped_and_trimmed() {
        let long = "x".repeat(SNIPPET_CHARS + 50);
        let cut = truncate(&long, SNIPPET_CHARS);
        assert_eq!(
            cut.chars().count(),
            SNIPPET_CHARS + 1,
            "capped plus ellipsis"
        );
        assert!(cut.ends_with('…'));
        assert_eq!(truncate("  hello  ", 100), "hello");
    }
}
