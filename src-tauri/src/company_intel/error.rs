// Typed error for the company-intel collection layer. Lets the orchestrator
// distinguish "this provider is unavailable right now" (degrade gracefully, keep
// the last good value) from "this ticker genuinely has no such data" (NotFound),
// and lets the HTTP helper decide what is worth retrying.

use std::fmt;

#[derive(Debug, Clone)]
pub enum IntelError {
    /// The provider's API key / credential isn't configured.
    MissingKey,
    /// Network / transport error (DNS, connect, timeout). Retryable.
    Network(String),
    /// HTTP status the server returned. `retryable` flags 5xx / 429.
    Http { status: u16, retryable: bool, body: String },
    /// Response body couldn't be parsed into the expected shape.
    Parse(String),
    /// The provider responded fine but has no record for this ticker.
    NotFound,
    /// Provider explicitly unavailable / disabled.
    Unavailable(String),
}

impl IntelError {
    /// True when retrying the same request might succeed (transient failures).
    pub fn is_retryable(&self) -> bool {
        match self {
            IntelError::Network(_) => true,
            IntelError::Http { retryable, .. } => *retryable,
            _ => false,
        }
    }

    pub fn http(status: u16, body: String) -> Self {
        // 5xx are transient and worth retrying. 429 means the provider's quota
        // is exhausted (the rate-limiter already handles per-minute pacing) —
        // retrying just hammers the API for nothing.
        let retryable = (500..600).contains(&status);
        IntelError::Http { status, retryable, body }
    }

    pub fn is_rate_limited(&self) -> bool {
        matches!(self, IntelError::Http { status: 429, .. })
    }
}

impl fmt::Display for IntelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IntelError::MissingKey => write!(f, "provider key not configured"),
            IntelError::Network(e) => write!(f, "network error: {e}"),
            IntelError::Http { status, body, .. } => {
                let snippet = &body[..body.len().min(160)];
                write!(f, "HTTP {status}: {snippet}")
            }
            IntelError::Parse(e) => write!(f, "parse error: {e}"),
            IntelError::NotFound => write!(f, "no data for ticker"),
            IntelError::Unavailable(e) => write!(f, "unavailable: {e}"),
        }
    }
}

impl std::error::Error for IntelError {}

pub type IntelResult<T> = Result<T, IntelError>;
