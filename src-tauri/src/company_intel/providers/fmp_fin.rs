// FMP (Financial Modeling Prep) financial-health fallback. Used when SEC Company
// Facts is unavailable or returns nothing for a ticker. Free-tier `stable`
// endpoints (income statement, cash-flow statement, balance sheet). Defensive
// parsing: any missing field stays None.
//
//   GET /stable/income-statement?symbol=X&period=quarter&limit=4&apikey=KEY
//   GET /stable/cash-flow-statement?symbol=X&period=annual&limit=1&apikey=KEY
//   GET /stable/balance-sheet-statement?symbol=X&period=quarter&limit=1&apikey=KEY

use serde::Deserialize;

use super::super::error::{IntelError, IntelResult};
use super::super::http::{Http, RetryPolicy};
use super::super::model::FinancialHealth;
use super::super::rate_limit::RateLimiter;

const FMP_BASE: &str = "https://financialmodelingprep.com/stable";

#[derive(Debug, Deserialize)]
struct IncomeRow {
    #[serde(default)]
    date: Option<String>,
    #[serde(rename = "netIncome", default)]
    net_income: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct CashFlowRow {
    #[serde(rename = "operatingCashFlow", default)]
    operating_cash_flow: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct BalanceRow {
    #[serde(rename = "cashAndCashEquivalents", default)]
    cash_and_cash_equivalents: Option<f64>,
}

/// Fetch the financial-health section for `ticker` from FMP.
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
    let mut fh = FinancialHealth::default();

    // Income statement — last 4 quarters (newest first).
    let income_url = format!(
        "{FMP_BASE}/income-statement?symbol={ticker}&period=quarter&limit=4&apikey={api_key}"
    );
    if let Ok(rows) = http.get_json::<Vec<IncomeRow>>(limiter, &income_url, &[], policy).await {
        let vals: Vec<f64> = rows.iter().filter_map(|r| r.net_income).collect();
        if let Some(first) = vals.first() {
            fh.net_income_last_q = Some(*first);
        }
        if let Some(latest_date) = rows.first().and_then(|r| r.date.clone()) {
            fh.period_end = Some(latest_date[..latest_date.len().min(10)].to_string());
        }
        if vals.len() >= 4 {
            fh.net_income_ttm = Some(vals.iter().take(4).sum());
        }
        if !vals.is_empty() {
            fh.negative_quarters_last4 =
                Some(vals.iter().take(4).filter(|v| **v < 0.0).count() as i64);
        }
    }

    // Cash-flow statement — most recent fiscal year (TTM-ish).
    let cf_url = format!(
        "{FMP_BASE}/cash-flow-statement?symbol={ticker}&period=annual&limit=1&apikey={api_key}"
    );
    if let Ok(rows) = http.get_json::<Vec<CashFlowRow>>(limiter, &cf_url, &[], policy).await {
        fh.operating_cash_flow_ttm = rows.first().and_then(|r| r.operating_cash_flow);
    }

    // Balance sheet — most recent quarter cash position.
    let bs_url = format!(
        "{FMP_BASE}/balance-sheet-statement?symbol={ticker}&period=quarter&limit=1&apikey={api_key}"
    );
    if let Ok(rows) = http.get_json::<Vec<BalanceRow>>(limiter, &bs_url, &[], policy).await {
        fh.cash_and_equivalents = rows.first().and_then(|r| r.cash_and_cash_equivalents);
    }

    if fh.is_empty() {
        Err(IntelError::NotFound)
    } else {
        Ok(fh)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn income_rows_parse() {
        let body = r#"[
            {"date":"2026-03-31","netIncome":-2000000},
            {"date":"2025-12-31","netIncome":-1000000},
            {"date":"2025-09-30","netIncome":500000},
            {"date":"2025-06-30","netIncome":-300000}
        ]"#;
        let rows: Vec<IncomeRow> = serde_json::from_str(body).unwrap();
        let vals: Vec<f64> = rows.iter().filter_map(|r| r.net_income).collect();
        assert_eq!(vals.len(), 4);
        assert_eq!(vals.iter().sum::<f64>(), -2_800_000.0);
        assert_eq!(vals.iter().filter(|v| **v < 0.0).count(), 3);
    }
}
