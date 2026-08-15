use std::sync::OnceLock;
use std::time::Duration;

/// Budget for ordinary API calls (Alpaca bars/news/assets, Massive, SEC, FMP). Short
/// on purpose: these are latency-sensitive and a hung request must fail fast rather
/// than stall a scan pass or a chart load.
const TIMEOUT: Duration = Duration::from_secs(30);

/// Budget for LLM calls, which live in a completely different latency regime.
///
/// A reasoning model spends a hidden thinking budget before it emits anything, so one
/// Panic Score call against the full rubric was MEASURED at 34–82 s (medium effort,
/// realistic payload). Under the 30 s budget above, every single call was cut off
/// mid-body and surfaced as reqwest's opaque "error decoding response body" — which
/// is what made the whole feature fail with a message that named no cause.
///
/// 300 s is ~3.5× the worst measurement: generous enough that a slow-but-healthy call
/// completes, while still bounding a genuinely hung one. This must NOT be folded into
/// `TIMEOUT`: making the market-data client wait five minutes would turn a dead socket
/// into a five-minute stall on the paths that feed the scanner.
const LLM_TIMEOUT: Duration = Duration::from_secs(300);

pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// Cached so the LLM client is built once. `reqwest::Client` is an `Arc` internally, so
/// the clone is cheap and every caller shares one connection pool — which matters here
/// because a Panic Score run fires up to 8 concurrent requests at the same host and
/// would otherwise pay a fresh TLS handshake for each.
static LLM_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

/// Long-timeout, pooled client for LLM calls. See `LLM_TIMEOUT`.
pub fn llm_client() -> reqwest::Client {
    LLM_CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .timeout(LLM_TIMEOUT)
                .build()
                .unwrap_or_else(|_| reqwest::Client::new())
        })
        .clone()
}
