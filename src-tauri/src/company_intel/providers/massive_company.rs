// Massive ticker details — `GET {BASE}/v3/reference/tickers/{ticker}`.
//
// Fills `company_meta`, the table the whole "who is this issuer" read runs on: hot
// industry (`strategies::issuer_profile::hot_industry`) and risk domicile. It is the
// first source, ahead of the paid sec-api.io bulk pipeline, which is the point.
//
// ── WHAT IT GIVES AND WHAT IT DOES NOT, MEASURED ──────────────────────────────
// Probed against the live API on 2026-08-22:
//   • common stock (JUNS): `sic_code` 2834, `sic_description` "PHARMACEUTICAL
//     PREPARATIONS", `type` "CS", a US business address — but NO country field;
//   • ADRs (BP, VALE, NVO): `sic_code`, `sic_description` and `address` are ALL
//     null. Massive knows the ticker type and nothing else about the issuer;
//   • the bulk list endpoint (`/v3/reference/tickers?...`) carries no SIC at all,
//     so a whole-universe refresh would be one request per ticker — 14 563 of them.
//
// So Massive supplies the INDUSTRY for US common stock and nothing else. The
// domicile — which is what the risk-jurisdiction filter reads — has to come from
// EDGAR's submissions feed, and so does everything about an ADR. `super::run` calls
// this first and completes the gaps from `sec_edgar::fetch_company_profile`, which
// is free and already being fetched for the filings.
//
// `type` is worth carrying on its own: it is the clean discriminator between common
// stock and an ADR, and an ADR has no business in a ≤50 M-float micro-cap scanner.

use serde::Deserialize;

use super::super::error::{IntelError, IntelResult};
use super::super::http::{Http, RetryPolicy};
use super::super::rate_limit::RateLimiter;

#[derive(Debug, Deserialize)]
struct Response {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    results: Option<RawTicker>,
}

#[derive(Debug, Default, Deserialize)]
struct RawTicker {
    #[serde(default)]
    sic_code: Option<String>,
    #[serde(default)]
    sic_description: Option<String>,
    #[serde(rename = "type", default)]
    security_type: Option<String>,
}

/// What Massive knows about an issuer. Every field optional — for an ADR they are
/// all absent except the type, and an absent field must stay absent rather than be
/// filled with a plausible guess.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MassiveCompany {
    pub sic: Option<String>,
    pub industry: Option<String>,
    /// "CS", "ADRC", "ETF", …
    pub security_type: Option<String>,
}

impl MassiveCompany {
    pub fn is_empty(&self) -> bool {
        self.sic.is_none() && self.industry.is_none() && self.security_type.is_none()
    }
}

fn clean(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

pub async fn fetch_company(
    http: &Http,
    limiter: &RateLimiter,
    policy: &RetryPolicy,
    api_key: &str,
    ticker: &str,
) -> IntelResult<MassiveCompany> {
    if api_key.trim().is_empty() {
        return Err(IntelError::MissingKey);
    }
    let path = format!(
        "{}/v3/reference/tickers/{}",
        crate::massive::BASE_URL,
        ticker
    );
    let url = crate::massive::with_key(&path, api_key);
    let resp: Response = http.get_json(limiter, &url, &[], policy).await?;
    if let Some(s) = &resp.status {
        if s != "OK" && s != "DELAYED" {
            return Err(IntelError::Unavailable(format!("Massive status: {s}")));
        }
    }
    let raw = resp.results.unwrap_or_default();
    let out = MassiveCompany {
        sic: clean(raw.sic_code),
        industry: clean(raw.sic_description),
        security_type: clean(raw.security_type),
    };
    if out.is_empty() {
        Err(IntelError::NotFound)
    } else {
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(body: &str) -> MassiveCompany {
        let raw = serde_json::from_str::<Response>(body)
            .unwrap()
            .results
            .unwrap_or_default();
        MassiveCompany {
            sic: clean(raw.sic_code),
            industry: clean(raw.sic_description),
            security_type: clean(raw.security_type),
        }
    }

    /// The real answer for JUNS. The SIC title comes back in the exact wording
    /// `issuer_profile::HOT_SIC_TITLES` matches on, which is why that table needs no
    /// change to work off this provider.
    #[test]
    fn common_stock_carries_the_sic_title_verbatim() {
        let c = parse(
            r#"{"status":"OK","results":{"ticker":"JUNS","type":"CS",
                 "sic_code":"2834","sic_description":"PHARMACEUTICAL PREPARATIONS"}}"#,
        );
        assert_eq!(c.sic.as_deref(), Some("2834"));
        assert_eq!(c.industry.as_deref(), Some("PHARMACEUTICAL PREPARATIONS"));
        assert_eq!(c.security_type.as_deref(), Some("CS"));
        assert_eq!(
            crate::strategies::issuer_profile::hot_industry(c.industry.as_deref(), None),
            (true, Some("biotech"))
        );
    }

    /// The real answer for BP: the type, and nothing else. Reporting an empty
    /// industry as "not hot" is correct; inventing one would be worse. The gap is
    /// filled from EDGAR by the caller.
    #[test]
    fn an_adr_yields_only_its_type() {
        let c = parse(r#"{"status":"OK","results":{"ticker":"BP","type":"ADRC","name":"BP p.l.c."}}"#);
        assert_eq!(c.security_type.as_deref(), Some("ADRC"));
        assert_eq!(c.industry, None);
        assert_eq!(c.sic, None);
        assert!(!c.is_empty(), "the type alone is still worth storing");
    }

    #[test]
    fn blank_strings_are_absent_not_present() {
        let c = parse(r#"{"status":"OK","results":{"sic_code":"  ","sic_description":""}}"#);
        assert!(c.is_empty());
    }
}
