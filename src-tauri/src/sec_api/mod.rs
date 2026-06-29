// sec-api.io client — company country of origin + SIC industry classification.
//
// We use the bulk "exchange listing" form of the Mapping API rather than the
// per-ticker form (which is a fuzzy substring match and would need ~13k calls):
//   GET https://api.sec-api.io/mapping/exchange/NASDAQ?token={key}
//   GET https://api.sec-api.io/mapping/exchange/NYSE?token={key}
// Two calls cover NASDAQ + NYSE + NYSEMKT + NYSEARCA (the listing for "NYSE"
// returns its sub-venues too). Each row:
//   { ticker, isDelisted, sic, sicIndustry, industry, sector, location, ... }
//
// Country of origin comes from `location` — the business HQ per SEC filings, NOT
// the exchange/marketplace country (e.g. BABA → "Hong Kong", NIO → "China").
// Industry comes from converting the `sic` code via the official SEC table
// (see `sic_codes`), falling back to the API's `sicIndustry`.

pub mod sic_codes;

use std::collections::HashMap;

use serde::Deserialize;

const BASE_URL: &str = "https://api.sec-api.io";
/// Exchanges to pull. "NYSE" also returns NYSEMKT (AMEX) and NYSEARCA rows.
const EXCHANGES: &[&str] = &["NASDAQ", "NYSE"];

/// Resolved company metadata for one symbol.
#[derive(Debug, Clone, PartialEq)]
pub struct SecCompany {
    pub symbol: String,
    /// Country of origin of the business (HQ), not the listing venue.
    pub country: Option<String>,
    /// 4-digit SIC code as returned by sec-api.
    pub sic: Option<String>,
    /// English industry name (SEC SIC title, else the API's sicIndustry).
    pub industry: Option<String>,
    /// Broad sector label (e.g. "Consumer Cyclical"). Bonus metadata.
    pub sector: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawCompany {
    ticker: String,
    #[serde(default, rename = "isDelisted")]
    is_delisted: bool,
    #[serde(default)]
    sic: String,
    #[serde(default, rename = "sicIndustry")]
    sic_industry: String,
    #[serde(default)]
    industry: String,
    #[serde(default)]
    sector: String,
    #[serde(default)]
    location: String,
}

/// Extract just the country from a sec-api `location` string.
/// `location` is either a country ("China", "Hong Kong") or "State; U.S.A"
/// (and, defensively, a longer "addr; city; state; country"). The country is
/// always the last `;`-separated segment. Returns None for an empty location.
pub fn country_from_location(location: &str) -> Option<String> {
    let country = location
        .rsplit(';')
        .map(str::trim)
        .find(|s| !s.is_empty())?;
    if country.is_empty() {
        None
    } else {
        Some(country.to_string())
    }
}

impl RawCompany {
    fn into_company(self) -> Option<SecCompany> {
        let symbol = self.ticker.trim().to_string();
        if symbol.is_empty() {
            return None;
        }
        let sic = (!self.sic.trim().is_empty()).then(|| self.sic.trim().to_string());
        // Prefer the official SEC SIC title; fall back to the API's sicIndustry,
        // then the broader `industry` label.
        let industry = sic
            .as_deref()
            .and_then(sic_codes::sic_to_industry)
            .map(str::to_string)
            .or_else(|| non_empty(&self.sic_industry))
            .or_else(|| non_empty(&self.industry));
        Some(SecCompany {
            symbol,
            country: country_from_location(&self.location),
            sic,
            industry,
            sector: non_empty(&self.sector),
        })
    }
}

fn non_empty(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}

/// Fetch one exchange's full company listing (includes delisted rows).
async fn fetch_by_exchange(token: &str, exchange: &str) -> Result<Vec<RawCompany>, String> {
    let client = crate::http::client();
    let url = format!("{BASE_URL}/mapping/exchange/{exchange}?token={token}");
    let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let snippet = &body[..body.len().min(200)];
        return Err(format!("sec-api HTTP {status}: {snippet}"));
    }
    resp.json::<Vec<RawCompany>>().await.map_err(|e| e.to_string())
}

/// Fetch all configured exchanges and resolve to a per-symbol metadata map.
///
/// Active (non-delisted) rows win over delisted ones for a given ticker; among
/// rows of equal listing status, the first seen wins. sec-api can return several
/// rows per ticker (e.g. currency variants) — they carry the same metadata.
pub async fn fetch_all(token: &str) -> Result<HashMap<String, SecCompany>, String> {
    let mut raws: Vec<RawCompany> = Vec::new();
    for ex in EXCHANGES {
        let mut page = fetch_by_exchange(token, ex).await?;
        raws.append(&mut page);
    }
    Ok(resolve(raws))
}

/// Build the symbol→metadata map, preferring active listings.
fn resolve(raws: Vec<RawCompany>) -> HashMap<String, SecCompany> {
    let mut map: HashMap<String, SecCompany> = HashMap::new();
    // Pass 1: active listings. Pass 2: delisted, only filling gaps.
    for delisted_pass in [false, true] {
        for raw in raws.iter().filter(|r| r.is_delisted == delisted_pass) {
            let symbol = raw.ticker.trim();
            if symbol.is_empty() || map.contains_key(symbol) {
                continue;
            }
            if let Some(c) = clone_into_company(raw) {
                map.insert(c.symbol.clone(), c);
            }
        }
    }
    map
}

/// `into_company` on a borrowed row (resolve() iterates by reference twice).
fn clone_into_company(raw: &RawCompany) -> Option<SecCompany> {
    RawCompany {
        ticker: raw.ticker.clone(),
        is_delisted: raw.is_delisted,
        sic: raw.sic.clone(),
        sic_industry: raw.sic_industry.clone(),
        industry: raw.industry.clone(),
        sector: raw.sector.clone(),
        location: raw.location.clone(),
    }
    .into_company()
}

/// Mock metadata for the FMP/Massive mock symbols, so dev mode shows country +
/// industry without network access.
pub fn mock_companies() -> HashMap<String, SecCompany> {
    let rows = [
        ("ABCD", "U.S.A", "3711", "Consumer Cyclical"),
        ("WXYZ", "China", "7372", "Technology"),
        ("MNOP", "Canada", "1040", "Basic Materials"),
        ("QRST", "U.S.A", "2836", "Healthcare"),
        ("EFGH", "Israel", "3674", "Technology"),
    ];
    rows.iter()
        .map(|(sym, country, sic, sector)| {
            (
                sym.to_string(),
                SecCompany {
                    symbol: sym.to_string(),
                    country: Some(country.to_string()),
                    sic: Some(sic.to_string()),
                    industry: sic_codes::sic_to_industry(sic).map(str::to_string),
                    sector: Some(sector.to_string()),
                },
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn country_extraction() {
        assert_eq!(country_from_location("California; U.S.A").as_deref(), Some("U.S.A"));
        assert_eq!(country_from_location("China").as_deref(), Some("China"));
        assert_eq!(country_from_location("Hong Kong").as_deref(), Some("Hong Kong"));
        assert_eq!(country_from_location("1 Main St; NYC; New York; U.S.A").as_deref(), Some("U.S.A"));
        assert_eq!(country_from_location(""), None);
        assert_eq!(country_from_location("  ; "), None);
        // Trailing separator: still resolve the real last segment.
        assert_eq!(country_from_location("Texas; U.S.A;").as_deref(), Some("U.S.A"));
    }

    #[test]
    fn parses_and_resolves_real_payload() {
        // Two NIO rows (currency variants, active) + a delisted homonym.
        let body = r#"[
            {"ticker":"NIO","isDelisted":false,"sic":"3711","sicIndustry":"Motor Vehicles & Passenger Car Bodies","industry":"Auto Manufacturers","sector":"Consumer Cyclical","location":"China","currency":"CNY"},
            {"ticker":"NIO","isDelisted":false,"sic":"3711","sicIndustry":"Motor Vehicles & Passenger Car Bodies","industry":"Auto Manufacturers","sector":"Consumer Cyclical","location":"China","currency":"USD"},
            {"ticker":"NIO1","isDelisted":true,"sic":"","sicIndustry":"","industry":"","sector":"","location":"Illinois; U.S.A"}
        ]"#;
        let raws: Vec<RawCompany> = serde_json::from_str(body).unwrap();
        let map = resolve(raws);
        let nio = map.get("NIO").unwrap();
        assert_eq!(nio.country.as_deref(), Some("China"));
        assert_eq!(nio.sic.as_deref(), Some("3711"));
        // SEC official title from the SIC table, not the API's sicIndustry.
        assert_eq!(nio.industry.as_deref(), Some("MOTOR VEHICLES & PASSENGER CAR BODIES"));
        assert_eq!(nio.sector.as_deref(), Some("Consumer Cyclical"));
    }

    #[test]
    fn active_listing_wins_over_delisted() {
        let body = r#"[
            {"ticker":"FOO","isDelisted":true,"sic":"1000","location":"Texas; U.S.A"},
            {"ticker":"FOO","isDelisted":false,"sic":"3711","location":"China"}
        ]"#;
        let raws: Vec<RawCompany> = serde_json::from_str(body).unwrap();
        let map = resolve(raws);
        let foo = map.get("FOO").unwrap();
        assert_eq!(foo.country.as_deref(), Some("China"));
        assert_eq!(foo.sic.as_deref(), Some("3711"));
    }

    #[test]
    fn falls_back_to_api_industry_when_sic_unknown() {
        let body = r#"[
            {"ticker":"BAR","isDelisted":false,"sic":"0001","sicIndustry":"Some Sec Industry","industry":"Broad Industry","location":"Canada"}
        ]"#;
        let raws: Vec<RawCompany> = serde_json::from_str(body).unwrap();
        let map = resolve(raws);
        // 0001 isn't in the SEC table → fall back to sicIndustry.
        assert_eq!(map.get("BAR").unwrap().industry.as_deref(), Some("Some Sec Industry"));
    }

    // Live integration test — what does sec-api actually return?
    //   cargo test -p tagdash live_sec -- --ignored --nocapture
    // with SEC_API_KEY set in the environment.
    #[tokio::test]
    #[ignore = "hits live sec-api.io; set SEC_API_KEY"]
    async fn live_sec_company_meta() {
        let token = std::env::var("SEC_API_KEY").expect("set SEC_API_KEY");
        let map = fetch_all(&token).await.unwrap();
        eprintln!("resolved companies: {}", map.len());
        for sym in ["AAPL", "BABA", "NIO", "TSLA"] {
            eprintln!("{sym}: {:?}", map.get(sym));
        }
        let with_country = map.values().filter(|c| c.country.is_some()).count();
        let with_industry = map.values().filter(|c| c.industry.is_some()).count();
        eprintln!("with country: {with_country}, with industry: {with_industry}");
        assert!(map.len() > 1000);
        // BABA's business HQ is Hong Kong — not the NYSE listing country.
        if let Some(baba) = map.get("BABA") {
            assert_eq!(baba.country.as_deref(), Some("Hong Kong"));
        }
    }
}
