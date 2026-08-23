// Massive financial statements — `GET {BASE}/vX/reference/financials`.
//
// WHY THIS EXISTS. The financial-health section used to come from SEC EDGAR's XBRL
// `companyfacts`/`frames` endpoints, with FMP as a fallback. Both work, but each
// quarter costs several requests and the frames API answers with a whole market's
// worth of data to extract one company's number. Massive returns the same figures
// pre-assembled, one request per ticker, already grouped by fiscal period — and the
// account is already paid for.
//
// WHAT IT DOES **NOT** REPLACE. Massive's `/v1/reference/sec/filings` indexes only
// the XBRL-bearing periodic reports (10-K, 10-Q, and some 8-K). Measured against the
// live API: JUNS returns 7 filings, all 10-K/10-Q; a census of the 200 most recent
// filings market-wide is 100 % 10-Q and 10-K. There is no S-1, S-3, S-3ASR or 424B5
// in it — which is precisely the set the dilution read is built on (shelf registered,
// ATM active, offering priced in the last 24 h). **The dilution / disclosures section
// therefore stays on SEC EDGAR**, which is free and needs no key. Do not "finish the
// migration" without re-checking that coverage.
//
// `cash_and_equivalents` is left `None` when the balance sheet has no cash line
// (common on small issuers, whose XBRL rolls it into current assets). An unknown cash
// balance means an unknown runway, which means the armed-ATM penalty never fires —
// the documented safe direction: we never penalise a company for a gap in our data.

use serde::Deserialize;

use super::super::error::{IntelError, IntelResult};
use super::super::http::{Http, RetryPolicy};
use super::super::model::FinancialHealth;
use super::super::rate_limit::RateLimiter;

/// Quarters pulled per ticker. Four are needed for the TTM sums and the
/// negative-quarter count; a fifth covers a late restatement without a second call.
const QUARTERS: usize = 5;

#[derive(Debug, Deserialize)]
struct Response {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    results: Vec<Report>,
}

#[derive(Debug, Deserialize)]
struct Report {
    #[serde(default)]
    end_date: Option<String>,
    #[serde(default)]
    financials: Statements,
}

#[derive(Debug, Default, Deserialize)]
struct Statements {
    #[serde(default)]
    income_statement: Section,
    #[serde(default)]
    balance_sheet: Section,
    #[serde(default)]
    cash_flow_statement: Section,
}

/// A statement is a map of `{ line_item: { value, unit, label, order } }`. Only the
/// value is used; the rest is Massive's own presentation metadata.
#[derive(Debug, Default, Deserialize)]
#[serde(transparent)]
struct Section(std::collections::HashMap<String, Line>);

#[derive(Debug, Deserialize)]
struct Line {
    #[serde(default)]
    value: Option<f64>,
}

impl Section {
    /// First line item present among `keys`, in order. XBRL naming varies between
    /// filers, so the caller lists the synonyms it accepts rather than guessing one.
    fn any(&self, keys: &[&str]) -> Option<f64> {
        keys.iter()
            .find_map(|k| self.0.get(*k).and_then(|l| l.value))
            .filter(|v| v.is_finite())
    }
}

/// Fold the reported quarters, newest first, into the health summary.
///
/// Pure and separate from the request so the arithmetic is testable against a real
/// captured payload — which is the half that actually goes wrong.
fn summarize(reports: &[Report]) -> FinancialHealth {
    let quarters: Vec<&Report> = reports.iter().take(4).collect();
    let net_incomes: Vec<f64> = quarters
        .iter()
        .filter_map(|r| {
            r.financials
                .income_statement
                .any(&["net_income_loss", "net_income_loss_attributable_to_parent"])
        })
        .collect();

    FinancialHealth {
        net_income_last_q: net_incomes.first().copied(),
        // Only a genuine four-quarter run is a TTM. Summing three and calling it one
        // would understate the burn by a quarter, on exactly the small issuers where
        // the number decides whether an ATM is armed.
        net_income_ttm: (net_incomes.len() == 4).then(|| net_incomes.iter().sum()),
        negative_quarters_last4: (!net_incomes.is_empty())
            .then(|| net_incomes.iter().filter(|v| **v < 0.0).count() as i64),
        operating_cash_flow_ttm: {
            let ocf: Vec<f64> = quarters
                .iter()
                .filter_map(|r| {
                    r.financials.cash_flow_statement.any(&[
                        "net_cash_flow_from_operating_activities",
                        "net_cash_flow_from_operating_activities_continuing",
                    ])
                })
                .collect();
            (ocf.len() == 4).then(|| ocf.iter().sum())
        },
        cash_and_equivalents: quarters.first().and_then(|r| {
            r.financials
                .balance_sheet
                .any(&["cash", "cash_and_cash_equivalents_at_carrying_value"])
        }),
        period_end: quarters.first().and_then(|r| r.end_date.clone()),
    }
}

/// Fetch and summarise `ticker`'s recent quarterly statements.
pub async fn fetch_financials(
    http: &Http,
    limiter: &RateLimiter,
    policy: &RetryPolicy,
    api_key: &str,
    ticker: &str,
) -> IntelResult<FinancialHealth> {
    if api_key.trim().is_empty() {
        return Err(IntelError::MissingKey);
    }
    let path = format!(
        "{}/vX/reference/financials?ticker={}&timeframe=quarterly&limit={}\
         &sort=period_of_report_date&order=desc",
        crate::massive::BASE_URL,
        ticker,
        QUARTERS,
    );
    let url = crate::massive::with_key(&path, api_key);
    let resp: Response = http.get_json(limiter, &url, &[], policy).await?;
    if let Some(s) = &resp.status {
        if s != "OK" && s != "DELAYED" {
            return Err(IntelError::Unavailable(format!("Massive status: {s}")));
        }
    }
    if resp.results.is_empty() {
        return Err(IntelError::NotFound);
    }
    let fh = summarize(&resp.results);
    if fh.is_empty() {
        Err(IntelError::NotFound)
    } else {
        Ok(fh)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed from the real answer for JUNS on 2026-08-22 — the shape is what
    /// matters, and inventing it is how a parser passes its tests and fails live.
    const CAPTURED: &str = r#"{"status":"OK","results":[
      {"end_date":"2026-06-30","fiscal_period":"Q2","financials":{
        "income_statement":{"net_income_loss":{"value":-2274397,"unit":"USD","label":"Net Income/Loss","order":3200}},
        "balance_sheet":{"current_assets":{"value":2548806,"unit":"USD","label":"Current Assets","order":200},
                         "assets":{"value":3718600,"unit":"USD","label":"Assets","order":100}},
        "cash_flow_statement":{"net_cash_flow_from_operating_activities":{"value":-2294922,"unit":"USD","label":"OCF","order":100}}}},
      {"end_date":"2026-03-31","fiscal_period":"Q1","financials":{
        "income_statement":{"net_income_loss":{"value":-2061958}},
        "cash_flow_statement":{"net_cash_flow_from_operating_activities":{"value":-1432100}}}},
      {"end_date":"2025-12-31","fiscal_period":"Q4","financials":{
        "income_statement":{"net_income_loss":{"value":-2575031}},
        "cash_flow_statement":{"net_cash_flow_from_operating_activities":{"value":-2368023}}}},
      {"end_date":"2025-09-30","fiscal_period":"Q3","financials":{
        "income_statement":{"net_income_loss":{"value":-2288034}},
        "cash_flow_statement":{"net_cash_flow_from_operating_activities":{"value":-1154450}}}},
      {"end_date":"2025-06-30","fiscal_period":"Q2","financials":{
        "income_statement":{"net_income_loss":{"value":-2252965}}}}
    ]}"#;

    fn parsed() -> Vec<Report> {
        serde_json::from_str::<Response>(CAPTURED).unwrap().results
    }

    #[test]
    fn a_real_payload_folds_into_the_health_summary() {
        let fh = summarize(&parsed());
        assert_eq!(fh.net_income_last_q, Some(-2_274_397.0));
        // The four most recent quarters only — the fifth is there for restatements.
        assert_eq!(fh.net_income_ttm, Some(-9_199_420.0));
        assert_eq!(fh.negative_quarters_last4, Some(4));
        assert_eq!(fh.operating_cash_flow_ttm, Some(-7_249_495.0));
        assert_eq!(fh.period_end.as_deref(), Some("2026-06-30"));
    }

    /// Small issuers often roll cash into current assets. Reporting `current_assets`
    /// as cash would overstate the runway on exactly the names where the runway
    /// decides whether an ATM counts as armed.
    #[test]
    fn a_missing_cash_line_is_none_not_the_nearest_number() {
        let fh = summarize(&parsed());
        assert_eq!(fh.cash_and_equivalents, None);
    }

    /// Three quarters is not a trailing twelve months. Reporting the partial sum
    /// would understate the burn by a full quarter.
    #[test]
    fn a_partial_year_yields_no_ttm_but_still_counts_losses() {
        let short = r#"{"status":"OK","results":[
          {"end_date":"2026-06-30","financials":{"income_statement":{"net_income_loss":{"value":-100}}}},
          {"end_date":"2026-03-31","financials":{"income_statement":{"net_income_loss":{"value":-200}}}},
          {"end_date":"2025-12-31","financials":{"income_statement":{"net_income_loss":{"value":300}}}}
        ]}"#;
        let fh = summarize(&serde_json::from_str::<Response>(short).unwrap().results);
        assert_eq!(fh.net_income_last_q, Some(-100.0));
        assert_eq!(fh.net_income_ttm, None);
        assert_eq!(fh.negative_quarters_last4, Some(2));
        assert_eq!(fh.operating_cash_flow_ttm, None);
    }

    #[test]
    fn an_empty_answer_is_not_found_rather_than_a_row_of_zeros() {
        let fh = summarize(&serde_json::from_str::<Response>(r#"{"results":[]}"#).unwrap().results);
        assert!(fh.is_empty(), "no data must never look like zero profit");
    }
}
