// Massive SEC-filings index — `GET {BASE}/v1/reference/sec/filings`.
//
// ── WHAT IT ACTUALLY COVERS, MEASURED ─────────────────────────────────────────
// Probed against the live API on 2026-08-22, with the real key:
//   • per-ticker filter: `entities.company_data.ticker=XXXX` (NOT `cik=`, which is
//     silently ignored and returns the whole market's latest filings — a trap that
//     would have every ticker inherit some other company's dossier);
//   • JUNS returns 7 filings, all 10-K / 10-Q. BIVI 33 (10-Q, 10-K, 8-K).
//     AMPG 27 (same three forms). SNSC: zero.
//   • a census of the 200 most recent filings market-wide is 100 % 10-Q and 10-K;
//   • `type=424B5` returns zero results across the whole index.
//
// So this endpoint indexes the XBRL-bearing periodic reports and some 8-K, and
// **carries none of the dilution forms** (S-1, S-3, S-3ASR, 424B*) — which are the
// entire basis of the "shelf registered / ATM active / offering priced" read. EDGAR's
// own `submissions` JSON for JUNS, by contrast, lists 424B3, 424B4 and 424B5.
//
// THEREFORE THIS IS A SUPPLEMENT, NOT A REPLACEMENT. Massive is queried first and
// its rows are merged with EDGAR's by accession number; EDGAR is still always
// consulted, because "fall back to SEC only when Massive fails" would mean never —
// Massive almost always answers — and dilution detection would die silently while
// every log line looked healthy. There is nothing to save by dropping EDGAR anyway:
// `data.sec.gov` is free, keyless and rate-generous. What costs credits is
// sec-api.io, which this module has nothing to do with.

use serde::Deserialize;

use super::super::error::{IntelError, IntelResult};
use super::super::http::{Http, RetryPolicy};
use super::super::model::SecFiling;
use super::super::rate_limit::RateLimiter;
use super::sec_edgar::classify_form;

#[derive(Debug, Deserialize)]
struct Response {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    results: Vec<RawFiling>,
}

#[derive(Debug, Deserialize)]
struct RawFiling {
    #[serde(default)]
    accession_number: Option<String>,
    #[serde(rename = "type", default)]
    form_type: Option<String>,
    /// `YYYYMMDD`, unlike EDGAR's `YYYY-MM-DD`.
    #[serde(default)]
    filing_date: Option<String>,
    #[serde(default)]
    period_of_report_date: Option<String>,
    #[serde(default)]
    source_url: Option<String>,
    #[serde(default)]
    main_file_url: Option<String>,
    #[serde(default)]
    entities: Vec<Entity>,
}

#[derive(Debug, Deserialize)]
struct Entity {
    #[serde(default)]
    company_data: Option<CompanyData>,
}

#[derive(Debug, Deserialize)]
struct CompanyData {
    #[serde(default)]
    cik: Option<String>,
}

/// `YYYYMMDD` → `YYYY-MM-DD`, so a Massive row and an EDGAR row sort and compare as
/// the same kind of date. Anything that is not eight digits is dropped rather than
/// reformatted into a plausible-looking lie.
fn iso_date(raw: &str) -> Option<String> {
    let d = raw.trim();
    if d.len() == 8 && d.bytes().all(|b| b.is_ascii_digit()) {
        Some(format!("{}-{}-{}", &d[0..4], &d[4..6], &d[6..8]))
    } else if d.len() == 10 && d.as_bytes()[4] == b'-' {
        Some(d.to_string())
    } else {
        None
    }
}

impl RawFiling {
    fn into_model(self, symbol: &str) -> Option<SecFiling> {
        let accession_number = self.accession_number.filter(|a| !a.trim().is_empty())?;
        let form_type = self.form_type.filter(|f| !f.trim().is_empty())?;
        let cik = self
            .entities
            .iter()
            .find_map(|e| e.company_data.as_ref().and_then(|c| c.cik.clone()));
        Some(SecFiling {
            category: classify_form(&form_type).to_string(),
            accession_number,
            symbol: symbol.to_string(),
            cik,
            form_type,
            filing_date: self.filing_date.as_deref().and_then(iso_date),
            report_date: self.period_of_report_date.as_deref().and_then(iso_date),
            primary_document: None,
            document_url: self.main_file_url.or(self.source_url),
            description: None,
            // The keyword scan runs on the document text, which this index does not
            // carry. Left false rather than guessed: `summarize_dilution` reads these
            // and a false positive would invent an ATM.
            detected_atm: false,
            detected_resale: false,
            detected_warrants: false,
            offering_amount: None,
        })
    }
}

/// Recent filings Massive holds for `ticker`, newest first.
pub async fn fetch_recent_filings(
    http: &Http,
    limiter: &RateLimiter,
    policy: &RetryPolicy,
    api_key: &str,
    symbol: &str,
    cap: usize,
) -> IntelResult<Vec<SecFiling>> {
    if api_key.trim().is_empty() {
        return Err(IntelError::MissingKey);
    }
    let path = format!(
        "{}/v1/reference/sec/filings?entities.company_data.ticker={}&limit={}\
         &sort=filing_date&order=desc",
        crate::massive::BASE_URL,
        symbol,
        cap.clamp(1, 200),
    );
    let url = crate::massive::with_key(&path, api_key);
    let resp: Response = http.get_json(limiter, &url, &[], policy).await?;
    if let Some(s) = &resp.status {
        if s != "OK" && s != "DELAYED" {
            return Err(IntelError::Unavailable(format!("Massive status: {s}")));
        }
    }
    let out: Vec<SecFiling> = resp
        .results
        .into_iter()
        .filter_map(|r| r.into_model(symbol))
        .collect();
    if out.is_empty() {
        Err(IntelError::NotFound)
    } else {
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed from the real answer for JUNS on 2026-08-22.
    const CAPTURED: &str = r#"{"status":"OK","results":[
      {"id":"0001493152-26-038463","accession_number":"0001493152-26-038463","type":"10-Q",
       "filing_date":"20260814","period_of_report_date":"20260630",
       "source_url":"https://www.sec.gov/Archives/edgar/data/1679628/000149315226038463/0001493152-26-038463.txt",
       "main_file_url":"https://api.massive.com/v1/reference/sec/filings/0001493152-26-038463/files/form10-q.htm",
       "entities":[{"relation":"filer","company_data":{"name":"JUPITER NEUROSCIENCES","cik":"0001679628","ticker":"JUNS"}}]},
      {"accession_number":"0001493152-26-023164","type":"10-Q","filing_date":"20260514","entities":[]}
    ]}"#;

    #[test]
    fn a_real_payload_maps_onto_the_edgar_shape() {
        let resp: Response = serde_json::from_str(CAPTURED).unwrap();
        let out: Vec<SecFiling> = resp
            .results
            .into_iter()
            .filter_map(|r| r.into_model("JUNS"))
            .collect();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].form_type, "10-Q");
        // Massive dates are YYYYMMDD; EDGAR's are YYYY-MM-DD. They must agree or the
        // merged list sorts wrong and the freshness reads are nonsense.
        assert_eq!(out[0].filing_date.as_deref(), Some("2026-08-14"));
        assert_eq!(out[0].report_date.as_deref(), Some("2026-06-30"));
        assert_eq!(out[0].cik.as_deref(), Some("0001679628"));
        assert_eq!(out[0].category, "other");
        assert_eq!(out[1].cik, None, "a row with no entity keeps a null cik");
    }

    /// The document text is not in this index, so the keyword flags cannot be
    /// derived. They must stay false — `summarize_dilution` reads them, and a
    /// guessed `detected_atm` would invent a dilution mechanism.
    #[test]
    fn keyword_flags_are_never_guessed() {
        let resp: Response = serde_json::from_str(CAPTURED).unwrap();
        for f in resp.results.into_iter().filter_map(|r| r.into_model("JUNS")) {
            assert!(!f.detected_atm && !f.detected_resale && !f.detected_warrants);
            assert_eq!(f.offering_amount, None);
        }
    }

    #[test]
    fn a_malformed_date_is_dropped_not_reshaped() {
        assert_eq!(iso_date("20260814").as_deref(), Some("2026-08-14"));
        assert_eq!(iso_date("2026-08-14").as_deref(), Some("2026-08-14"));
        assert_eq!(iso_date(""), None);
        assert_eq!(iso_date("2026081"), None);
        assert_eq!(iso_date("not a date"), None);
    }

    /// A dilution form must classify identically whichever provider supplied it, or
    /// the merged list would categorise the same filing two different ways.
    #[test]
    fn classification_is_shared_with_the_edgar_provider() {
        let body = r#"{"results":[{"accession_number":"a","type":"424B5","filing_date":"20260814","entities":[]}]}"#;
        let f = serde_json::from_str::<Response>(body)
            .unwrap()
            .results
            .remove(0)
            .into_model("X")
            .unwrap();
        assert_eq!(f.category, "dilution");
    }
}
