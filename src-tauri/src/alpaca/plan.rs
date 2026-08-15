// Which Alpaca market-data subscription this key actually has.
//
// The two plans differ in ways that change what the app may do, not just how fast
// it goes, so guessing is not an option:
//
//   SIP (paid)  — full consolidated tape, no symbol limit. The stream subscribes the
//                 whole US market with the `*` wildcard and the scanner discovers
//                 movers on its own.
//   IEX (free)  — IEX's own prints only (a few percent of volume) and a hard cap of
//                 30 SUBSCRIPTIONS, counted as (symbol × channel) pairs. The wildcard
//                 is meaningless: nothing can be discovered, so the slots must be
//                 SPENT deliberately on names already chosen — Panic Mean Reversion's
//                 watchlist, the charts on screen, and anything carrying an order.
//
// Alpaca exposes no "what plan am I on" endpoint, so we ask the data API a question
// only a SIP key can answer: the latest AAPL trade on the SIP feed. A free key is
// refused with error 42210000 ("subscription does not permit querying recent SIP
// data"). AAPL is a deliberate choice — it always has a recent trade, so a 200 means
// the plan and not a quiet ticker.
//
// Live only. Market Replay reads history through REST (which the free plan serves
// fine, 15 minutes back) and never opens the stream, so none of this applies there.

use serde::{Deserialize, Serialize};

/// Alpaca's error code for "this key may not query recent SIP data". Sent by some
/// data endpoints; see `classify` for the endpoints that only send the message.
const SIP_FORBIDDEN: i64 = 42_210_000;

/// The refusal sentence, for the endpoints that omit the numeric code.
const SIP_FORBIDDEN_MESSAGE: &str = "subscription does not permit querying recent SIP data";

/// A symbol that trades constantly, so a 200 means "allowed", never "no data".
const PROBE_SYMBOL: &str = "AAPL";

/// Max SUBSCRIPTIONS the free plan will stream at once.
///
/// The unit is (symbol × channel) pairs, not distinct symbols — verified against a
/// real free key: 20 names on trades + quotes + bars is 60 against this budget and
/// Alpaca answers `code=405 "symbol limit exceeded"`, having acked only the first
/// channel. `stream::reconcile` therefore splits this figure across channels rather
/// than replicating one set on each.
pub const IEX_SYMBOL_LIMIT: usize = 30;

/// `app_meta` key holding the last successful probe, so a transient network failure
/// reuses what we knew instead of silently degrading a paying key to 30 symbols.
pub const PLAN_META_KEY: &str = "alpaca_market_data_plan";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Plan {
    /// Paid consolidated tape: wildcard subscription, no symbol cap.
    Sip,
    /// Free tier: IEX prints only, `IEX_SYMBOL_LIMIT` streamed symbols.
    Iex,
}

impl Plan {
    /// The `/v2/{feed}` websocket endpoint this plan may connect to.
    pub fn feed(self) -> &'static str {
        match self {
            Plan::Sip => "sip",
            Plan::Iex => "iex",
        }
    }

    /// `Some(n)` when the stream must not exceed `n` symbols; `None` when unlimited.
    pub fn symbol_limit(self) -> Option<usize> {
        match self {
            Plan::Sip => None,
            Plan::Iex => Some(IEX_SYMBOL_LIMIT),
        }
    }

    /// Whether the broad surveillance tier may use the `*` wildcard. False on the
    /// free plan: a wildcard there is both over the cap and useless (IEX prints
    /// cover too little of the tape to discover anything).
    pub fn allows_wildcard(self) -> bool {
        matches!(self, Plan::Sip)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Plan::Sip => "sip",
            Plan::Iex => "iex",
        }
    }

    /// Read back a value stored by `as_str` (see `PLAN_META_KEY`).
    pub fn from_meta(s: &str) -> Option<Self> {
        match s {
            "sip" => Some(Plan::Sip),
            "iex" => Some(Plan::Iex),
            _ => None,
        }
    }
}

/// Classify an Alpaca data-API response to the SIP probe.
///
/// Split out from the request so the decision rule is testable without a network:
/// 200 is SIP, the documented refusal is the free tier, and anything else — a bad
/// key, a 500, an unparseable body — is NOT a verdict. Returning `None` there is the
/// whole point: it lets the caller fall back to the last known plan rather than
/// capping a paying key at 30 symbols because a proxy hiccuped.
///
/// The refusal is recognised two ways because Alpaca is not consistent about it.
/// Verified against a real free key on 2026-08-06, this very endpoint answers
/// `403 {"message":"subscription does not permit querying recent SIP data"}` with NO
/// `code` field at all, while other data endpoints do send `code: 42210000`. Keying
/// only on the numeric code would have left the free tier undetected here.
pub fn classify(status: u16, body: &str) -> Option<Plan> {
    if status == 200 {
        return Some(Plan::Sip);
    }
    let json = serde_json::from_str::<serde_json::Value>(body).ok();
    let code = json
        .as_ref()
        .and_then(|v| v.get("code"))
        .and_then(|c| c.as_i64());
    if code == Some(SIP_FORBIDDEN) {
        return Some(Plan::Iex);
    }
    // Message form. Narrow on purpose: only this sentence, and only on a refusal
    // status, so an unrelated 4xx never gets read as a plan.
    let message = json
        .as_ref()
        .and_then(|v| v.get("message"))
        .and_then(|m| m.as_str())
        .unwrap_or_default();
    let refused = matches!(status, 401 | 403) && message.contains(SIP_FORBIDDEN_MESSAGE);
    refused.then_some(Plan::Iex)
}

/// Ask the data API for AAPL's latest SIP trade and read the plan off the answer.
///
/// `Err` means "could not determine" (network, auth, unexpected shape) — never a
/// guess. Callers keep their previous value.
pub async fn probe(key: &str, secret: &str) -> Result<Plan, String> {
    if key.is_empty() || secret.is_empty() {
        return Err("clés Alpaca non configurées".into());
    }
    let url =
        format!("https://data.alpaca.markets/v2/stocks/{PROBE_SYMBOL}/trades/latest?feed=sip");
    let resp = crate::http::client()
        .get(&url)
        .header("APCA-API-KEY-ID", key)
        .header("APCA-API-SECRET-KEY", secret)
        .send()
        .await
        .map_err(|e| format!("sonde d'abonnement injoignable: {e}"))?;

    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap_or_default();
    classify(status, &body)
        .ok_or_else(|| format!("réponse d'abonnement non concluante (HTTP {status}): {body}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_200_means_the_key_may_read_sip() {
        let body = r#"{"symbol":"AAPL","trade":{"t":"2026-08-06T13:30:00Z","p":210.5,"s":100}}"#;
        assert_eq!(classify(200, body), Some(Plan::Sip));
    }

    /// The documented numeric refusal, as other data endpoints send it.
    #[test]
    fn the_documented_code_means_the_free_tier() {
        let body = r#"{"code":42210000,"message":"subscription does not permit querying recent SIP data"}"#;
        assert_eq!(classify(403, body), Some(Plan::Iex));
    }

    /// What THIS endpoint actually returns — captured verbatim from a real free key
    /// on 2026-08-06. No `code` field at all, so the numeric rule alone would have
    /// left the free tier undetected and let the stream ask for the wildcard.
    #[test]
    fn the_message_only_refusal_means_the_free_tier_too() {
        let body = r#"{"message":"subscription does not permit querying recent SIP data"}"#;
        assert_eq!(classify(403, body), Some(Plan::Iex));
    }

    /// Everything else must stay undecided. A wrong "Iex" here would cap a paying
    /// key at 30 symbols and kill wildcard scanning for the whole session.
    #[test]
    fn anything_else_is_not_a_verdict() {
        assert_eq!(
            classify(401, r#"{"message":"access key verification failed"}"#),
            None
        );
        assert_eq!(classify(500, "upstream unavailable"), None);
        assert_eq!(
            classify(403, r#"{"code":40110000,"message":"unauthorized"}"#),
            None
        );
        assert_eq!(classify(429, ""), None);
        // A different plan-ish code is still not the one we were told to match.
        assert_eq!(classify(403, r#"{"code":42210001}"#), None);
        // The right sentence on a status that isn't a refusal proves nothing.
        assert_eq!(
            classify(
                500,
                r#"{"message":"subscription does not permit querying recent SIP data"}"#
            ),
            None,
        );
    }

    #[test]
    fn each_plan_carries_its_own_stream_rules() {
        assert_eq!(Plan::Sip.feed(), "sip");
        assert_eq!(Plan::Sip.symbol_limit(), None);
        assert!(Plan::Sip.allows_wildcard());

        assert_eq!(Plan::Iex.feed(), "iex");
        assert_eq!(Plan::Iex.symbol_limit(), Some(30));
        assert!(
            !Plan::Iex.allows_wildcard(),
            "the free tier cannot stream the market"
        );
    }

    #[test]
    fn plan_round_trips_through_app_meta() {
        for p in [Plan::Sip, Plan::Iex] {
            assert_eq!(Plan::from_meta(p.as_str()), Some(p));
        }
        assert_eq!(Plan::from_meta("delayed_sip"), None);
        assert_eq!(Plan::from_meta(""), None);
    }
}
