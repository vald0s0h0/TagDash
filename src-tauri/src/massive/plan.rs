// Which Massive (api.massive.com) stocks subscription this key actually has.
//
// Massive exposes no "what plan am I on" endpoint either, so — same trick as
// `crate::alpaca::plan` — we ask an endpoint only a paid key can reach: the
// single-ticker snapshot. Per Massive's docs (massive.com/docs/rest/stocks/
// snapshots/single-ticker-snapshot), the Basic (free) tier is explicitly
// excluded from snapshot access; Starter and above (unlimited API calls,
// WebSockets, second-level aggregates) can read it. AAPL always has a
// snapshot, so a 200 means "this key can reach Starter+ endpoints", not
// "quiet ticker" — the same reasoning `alpaca::plan` uses for its SIP probe.
//
// IMPORTANT SCOPE NOTE: this tier concerns PRICE/BARS data only (Massive's
// Starter tier is 15-minute delayed, not real-time — see `Tier::Unlimited`).
// It has NOTHING to do with news: Alpaca's news API is free and real-time on
// every account regardless of any data-plan tier (see
// `crate::panic_llm::mod::fill_context`, which reads Alpaca news first and is
// completely independent of this module). Do not wire news through this tier.
//
// Verified live against a real Starter key on 2026-08-20: the snapshot endpoint
// answers `200 {"ticker":{...},"status":"OK",...}` exactly as expected, and an
// invalid/missing key answers `401 {"status":"ERROR","error":"Unknown API Key" |
// "API Key was not provided"}` — never 402/403, so it can never be misread as a
// tier-gate refusal. The 200 branch and the "401 is not a verdict" safety
// property are both confirmed. The 402/403 refusal branch itself is still
// provisional (no Basic-tier key was available to test against) — see the TODO
// on that branch below.

use serde::{Deserialize, Serialize};

use super::{with_key, BASE_URL};

/// A symbol that always has a snapshot, so a 200 means "allowed", never "no data".
const PROBE_SYMBOL: &str = "AAPL";

/// `app_meta` key holding the last successful probe, so a transient network failure
/// reuses what we knew instead of silently throttling a paying key back to 13s/page.
pub const TIER_META_KEY: &str = "massive_data_tier";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Tier {
    /// Starter or above (paid): unlimited API calls, WebSockets, second-level
    /// aggregates. Still 15-minute delayed below the Advanced ($199) tier — never
    /// treat this as real-time.
    Unlimited,
    /// Basic (free): reference/EOD data only, ~5 requests/minute.
    Basic,
}

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Unlimited => "unlimited",
            Tier::Basic => "basic",
        }
    }

    /// Read back a value stored by `as_str` (see `TIER_META_KEY`) or sent by the
    /// frontend as a manual override.
    pub fn from_meta(s: &str) -> Option<Self> {
        match s {
            "unlimited" => Some(Tier::Unlimited),
            "basic" => Some(Tier::Basic),
            _ => None,
        }
    }
}

/// Classify a Massive snapshot response to the tier probe.
///
/// Split out from the request so the decision rule is testable without a network:
/// 200 is Unlimited, a tier-gate refusal is Basic, and anything else — a bad key, a
/// 500, an unparseable body — is NOT a verdict. Returning `None` there lets the
/// caller fall back to the last known tier rather than throttling a paying key
/// because a proxy hiccuped, or worse, skipping the throttle on a free key and
/// tripping a real 429.
///
/// Confirmed live on 2026-08-20 against a real Starter key: 200 returns the
/// expected snapshot body, and a bad/missing key answers 401 (never 402/403) —
/// so this function cannot misread an auth failure as a tier refusal.
///
/// TODO: the 402/403 branch itself is still provisional — no Basic-tier key
/// was available to observe the real refusal. Verify against one before
/// relying on the `Basic` verdict in production; until then a Basic-tier
/// account may simply classify as `None` (undetermined, safe: falls back to
/// throttled behaviour) if Massive's actual refusal code differs.
pub fn classify(status: u16, body: &str) -> Option<Tier> {
    if status == 200 {
        return Some(Tier::Unlimited);
    }
    // Tier-gate refusals are conventionally 402 (Payment Required) or 403
    // (Forbidden) for a plan restriction, as opposed to 401 (bad/missing key).
    if matches!(status, 402 | 403) {
        return Some(Tier::Basic);
    }
    let _ = body; // reserved for a body-based check once the real shape is known
    None
}

/// Ask Massive for AAPL's snapshot and read the tier off the answer.
///
/// `Err` means "could not determine" (network, auth, unexpected shape) — never a
/// guess. Callers keep their previous value.
pub async fn probe(api_key: &str) -> Result<Tier, String> {
    if api_key.trim().is_empty() {
        return Err("clé Massive non configurée".into());
    }
    let url = with_key(
        &format!("{BASE_URL}/v2/snapshot/locale/us/markets/stocks/tickers/{PROBE_SYMBOL}"),
        api_key,
    );
    let resp = crate::http::client()
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("sonde Massive injoignable: {e}"))?;

    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap_or_default();
    classify(status, &body)
        .ok_or_else(|| format!("réponse Massive non concluante (HTTP {status}): {body}"))
}

/// A manual override always wins (the user forces it because detection was wrong);
/// otherwise the detected/cached value; otherwise Basic — never assume Unlimited
/// without positive evidence, because wrongly skipping the throttle produces real
/// 429s, whereas wrongly keeping it only costs time.
pub fn effective(detected: Option<Tier>, override_: Option<Tier>) -> Tier {
    override_.or(detected).unwrap_or(Tier::Basic)
}

/// Resolve the effective tier straight from persisted state: the last probe result
/// cached in `app_meta` plus any manual override in config. Convenience wrapper
/// around `effective()` for the read-only call sites (the broker safety gate, live-
/// feed provider selection, chart bar enrichment) that would otherwise each
/// duplicate the same two lookups.
pub fn effective_from_app_state(
    db: &std::sync::Mutex<rusqlite::Connection>,
    config: &std::sync::RwLock<crate::config::AppConfig>,
) -> Tier {
    let detected = {
        let g = db.lock().unwrap();
        crate::local_db::cache_repository::get_app_meta(&g, TIER_META_KEY)
            .and_then(|s| Tier::from_meta(&s))
    };
    let override_ = config
        .read()
        .unwrap()
        .data_source
        .massive_tier_override
        .as_deref()
        .and_then(Tier::from_meta);
    effective(detected, override_)
}

/// Whether Massive's delayed stream is what feeds the app live.
///
/// Only when NO Alpaca key pair is configured. With Alpaca keys, Alpaca always wins:
/// its free Basic plan serves the full SIP history over REST (15 minutes back, the
/// same delay as Massive Starter) and real-time IEX over the stream — Massive stays a
/// reference source (float, short interest, security types, 5s/10s bars) and is never
/// promoted over it.
pub fn drives_live_feed(has_alpaca: bool, tier: Tier) -> bool {
    !has_alpaca && matches!(tier, Tier::Unlimited)
}

/// Whether an Alpaca key pair is configured — the input `drives_live_feed` keys on.
/// Deliberately blind to the offline switch: going offline stops every feed, it
/// must not flip the broker gate as if Massive had taken over.
pub fn has_alpaca_keys(sec: &crate::config::secrets::Secrets) -> bool {
    let set = |o: &Option<String>| o.as_deref().map(|s| !s.trim().is_empty()).unwrap_or(false);
    set(&sec.alpaca_key) && set(&sec.alpaca_secret)
}

/// `drives_live_feed` resolved from persisted state (keys + effective tier), for the
/// broker gate and the settings UI.
pub fn drives_live_feed_from_app_state(
    secrets: &std::sync::RwLock<crate::config::secrets::Secrets>,
    db: &std::sync::Mutex<rusqlite::Connection>,
    config: &std::sync::RwLock<crate::config::AppConfig>,
) -> bool {
    let has_alpaca = has_alpaca_keys(&secrets.read().unwrap());
    drives_live_feed(has_alpaca, effective_from_app_state(db, config))
}

/// Data differed by 15 minutes must never drive a real order sent to Alpaca. When
/// Massive's delayed stream feeds the app (`drives_live_feed`) and the Alpaca broker
/// is currently selected, force the internal TagDash broker instead and persist the
/// change. Returns whether a switch actually happened, so the caller can warn the
/// user.
pub fn enforce_broker_gate(
    config: &std::sync::Arc<std::sync::RwLock<crate::config::AppConfig>>,
    app_dir: &std::path::Path,
    delayed_live: bool,
) -> bool {
    if !delayed_live {
        return false;
    }
    let mut cfg = config.write().unwrap();
    if cfg.broker.broker != "alpaca" {
        return false;
    }
    cfg.broker.broker = "tagdash".into();
    let path = crate::config::config_path(app_dir);
    let _ = crate::config::save(&path, &cfg);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_200_means_unlimited() {
        let body = r#"{"status":"OK","ticker":{"day":{"c":210.5}}}"#;
        assert_eq!(classify(200, body), Some(Tier::Unlimited));
    }

    #[test]
    fn a_tier_gate_refusal_means_basic() {
        assert_eq!(
            classify(403, r#"{"error":"upgrade required"}"#),
            Some(Tier::Basic)
        );
        assert_eq!(
            classify(402, r#"{"error":"payment required"}"#),
            Some(Tier::Basic)
        );
    }

    #[test]
    fn anything_else_is_not_a_verdict() {
        assert_eq!(classify(401, r#"{"error":"invalid api key"}"#), None);
        assert_eq!(classify(500, "upstream unavailable"), None);
        assert_eq!(classify(429, ""), None);
    }

    #[test]
    fn tier_round_trips_through_app_meta() {
        for t in [Tier::Unlimited, Tier::Basic] {
            assert_eq!(Tier::from_meta(t.as_str()), Some(t));
        }
        assert_eq!(Tier::from_meta("premium"), None);
        assert_eq!(Tier::from_meta(""), None);
    }

    #[test]
    fn override_always_wins_over_detected() {
        assert_eq!(
            effective(Some(Tier::Basic), Some(Tier::Unlimited)),
            Tier::Unlimited
        );
        assert_eq!(
            effective(Some(Tier::Unlimited), Some(Tier::Basic)),
            Tier::Basic
        );
    }

    #[test]
    fn detected_wins_when_no_override() {
        assert_eq!(effective(Some(Tier::Unlimited), None), Tier::Unlimited);
        assert_eq!(effective(Some(Tier::Basic), None), Tier::Basic);
    }

    #[test]
    fn alpaca_keys_always_win_over_massive() {
        assert!(!drives_live_feed(true, Tier::Unlimited));
        assert!(!drives_live_feed(true, Tier::Basic));
        assert!(drives_live_feed(false, Tier::Unlimited));
        assert!(!drives_live_feed(false, Tier::Basic));
    }

    #[test]
    fn basic_is_the_safe_default_with_nothing_known() {
        assert_eq!(effective(None, None), Tier::Basic);
    }
}
