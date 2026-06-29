// Shared HTTP helper for the company-intel providers: one reusable reqwest
// client, a per-request rate-limit gate, and retry-with-exponential-backoff for
// transient failures. Every provider goes through this so retry/backoff/logging
// behaviour is identical and lives in one place (important for lifting the whole
// `company_intel` module into a standalone background worker later).

use std::time::Duration;

use serde::de::DeserializeOwned;

use super::error::{IntelError, IntelResult};
use super::rate_limit::RateLimiter;

/// Retry policy for one logical request (the rate limiter is re-acquired before
/// each attempt, so retries still respect the provider budget).
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    pub max_retries: u32,
    pub base_backoff: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self { max_retries: 3, base_backoff: Duration::from_millis(500) }
    }
}

/// Reusable HTTP client. Cheap to clone (reqwest::Client is an Arc internally).
#[derive(Clone)]
pub struct Http {
    client: reqwest::Client,
    /// Tag used in log lines, e.g. "sec_edgar".
    provider: &'static str,
}

impl Http {
    pub fn new(provider: &'static str) -> Self {
        let client = crate::http::client();
        Self { client, provider }
    }

    /// GET `url` and decode the JSON body into `T`, honouring the rate limiter and
    /// retrying transient failures with exponential backoff. `headers` is a slice
    /// of (name, value) pairs (e.g. the SEC `User-Agent`).
    pub async fn get_json<T: DeserializeOwned>(
        &self,
        limiter: &RateLimiter,
        url: &str,
        headers: &[(&str, &str)],
        policy: &RetryPolicy,
    ) -> IntelResult<T> {
        let body = self.get_text(limiter, url, headers, policy).await?;
        serde_json::from_str::<T>(&body).map_err(|e| IntelError::Parse(e.to_string()))
    }

    /// GET `url` and return the raw response body as text, with rate limiting +
    /// retry/backoff. Used by callers that scan documents for keywords.
    pub async fn get_text(
        &self,
        limiter: &RateLimiter,
        url: &str,
        headers: &[(&str, &str)],
        policy: &RetryPolicy,
    ) -> IntelResult<String> {
        let mut attempt = 0u32;
        loop {
            limiter.acquire().await;
            match self.try_get_text(url, headers).await {
                Ok(text) => return Ok(text),
                Err(e) => {
                    if e.is_retryable() && attempt < policy.max_retries {
                        // Exponential backoff: base · 2^attempt.
                        let backoff = policy.base_backoff * 2u32.pow(attempt);
                        eprintln!(
                            "[tagdash][company_intel][{}] {url} failed ({e}); retry {}/{} in {:?}",
                            self.provider, attempt + 1, policy.max_retries, backoff
                        );
                        tokio::time::sleep(backoff).await;
                        attempt += 1;
                        continue;
                    }
                    return Err(e);
                }
            }
        }
    }

    /// One request attempt → text, classifying failures into `IntelError`.
    async fn try_get_text(&self, url: &str, headers: &[(&str, &str)]) -> IntelResult<String> {
        let mut req = self.client.get(url);
        for (name, value) in headers {
            req = req.header(*name, *value);
        }
        let resp = req.send().await.map_err(|e| IntelError::Network(e.to_string()))?;
        let status = resp.status();
        if status.as_u16() == 404 {
            return Err(IntelError::NotFound);
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(IntelError::http(status.as_u16(), body));
        }
        resp.text().await.map_err(|e| IntelError::Network(e.to_string()))
    }
}
