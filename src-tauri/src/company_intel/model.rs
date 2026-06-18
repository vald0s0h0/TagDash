// Normalized "company intelligence" data model.
//
// These structs are the typed shape of what the providers collect and what the
// repository persists / reads back. They cross the Tauri bridge (Serialize) so the
// UI can consume them directly. Everything is optional: a field is `None` until a
// provider fills it, and a section keeps its last good value when its provider is
// unavailable on a given run (see the repository's per-section upserts).

use serde::{Deserialize, Serialize};

// ─── Section: short interest (Massive) ───────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ShortInterest {
    /// Reported short interest (shares).
    pub short_interest: Option<i64>,
    /// Days-to-cover = short_interest / average daily volume.
    pub days_to_cover: Option<f64>,
    /// Settlement (as-of) date of the report, `YYYY-MM-DD`.
    pub settlement_date: Option<String>,
}

impl ShortInterest {
    pub fn is_empty(&self) -> bool {
        self.short_interest.is_none()
            && self.days_to_cover.is_none()
            && self.settlement_date.is_none()
    }
}

// ─── Section: financial health (SEC Company Facts, FMP fallback) ──────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FinancialHealth {
    /// Net income (loss) of the most recent reported quarter, USD.
    pub net_income_last_q: Option<f64>,
    /// Net income (loss) trailing twelve months (sum of last 4 quarters), USD.
    pub net_income_ttm: Option<f64>,
    /// How many of the last 4 reported quarters had a net loss (0..=4).
    pub negative_quarters_last4: Option<i64>,
    /// Operating cash flow, trailing twelve months (best-effort), USD.
    pub operating_cash_flow_ttm: Option<f64>,
    /// Cash and cash equivalents, most recent balance sheet, USD.
    pub cash_and_equivalents: Option<f64>,
    /// End date of the most recent period these figures are based on, `YYYY-MM-DD`.
    pub period_end: Option<String>,
}

impl FinancialHealth {
    pub fn is_empty(&self) -> bool {
        self.net_income_last_q.is_none()
            && self.net_income_ttm.is_none()
            && self.negative_quarters_last4.is_none()
            && self.operating_cash_flow_ttm.is_none()
            && self.cash_and_equivalents.is_none()
    }
}

// ─── Section: dilution / S-3 filings (SEC EDGAR) ──────────────────────────────

/// Heuristic flags detected across the recent dilution filings.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct DilutionFlags {
    /// "At-the-market" offering mentioned.
    pub atm: bool,
    /// Resale / selling-stockholders registration.
    pub resale: bool,
    /// Warrants mentioned.
    pub warrants: bool,
    /// Detected offering amount in USD, when a dollar figure could be parsed.
    pub offering_amount: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct DilutionInfo {
    /// True when at least one S-3 family filing was seen in the recent window.
    pub has_recent_shelf: bool,
    /// Form type of the most recent dilution filing (e.g. "424B5").
    pub latest_form: Option<String>,
    /// Filing date of the most recent dilution filing, `YYYY-MM-DD`.
    pub latest_date: Option<String>,
    pub flags: DilutionFlags,
}

impl DilutionInfo {
    pub fn is_empty(&self) -> bool {
        !self.has_recent_shelf && self.latest_form.is_none() && self.latest_date.is_none()
    }
}

// ─── Section: ownership / locked shares (SEC 13D/13G, FMP fallback) ───────────

/// One >5% holder, parsed from a 13D/13G filing.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Holder {
    pub name: String,
    /// Ownership percentage if it could be parsed, else None.
    pub pct: Option<f64>,
    /// Filing form that disclosed the holder ("SC 13D", "SC 13G", …).
    pub form: String,
    /// Filing date, `YYYY-MM-DD`.
    pub date: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct OwnershipInfo {
    pub institutional_ownership_pct: Option<f64>,
    pub insider_ownership_pct: Option<f64>,
    /// >5% holders disclosed via Schedule 13D/13G filings.
    pub holders_5pct: Vec<Holder>,
    /// Restricted / locked / resale share count, when detectable.
    pub restricted_shares: Option<i64>,
}

impl OwnershipInfo {
    pub fn is_empty(&self) -> bool {
        self.institutional_ownership_pct.is_none()
            && self.insider_ownership_pct.is_none()
            && self.holders_5pct.is_empty()
            && self.restricted_shares.is_none()
    }
}

// ─── Raw SEC filing ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SecFiling {
    pub accession_number: String,
    pub symbol: String,
    pub cik: Option<String>,
    pub form_type: String,
    pub filing_date: Option<String>,
    pub report_date: Option<String>,
    pub primary_document: Option<String>,
    pub document_url: Option<String>,
    pub description: Option<String>,
    /// 'dilution' | 'ownership' | 'other'.
    pub category: String,
    pub detected_atm: bool,
    pub detected_resale: bool,
    pub detected_warrants: bool,
    pub offering_amount: Option<f64>,
}

// ─── Aggregate (one ticker's full intel record) ───────────────────────────────

/// The full normalized record for a ticker, as read back for the UI. Mirrors the
/// `company_intel` row plus its recent `company_filings`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompanyIntel {
    pub symbol: String,
    pub cik: Option<String>,

    pub short_interest: ShortInterest,
    pub short_interest_source: Option<String>,
    pub short_interest_updated_at: Option<String>,

    pub financials: FinancialHealth,
    pub financials_source: Option<String>,
    pub financials_updated_at: Option<String>,

    pub dilution: DilutionInfo,
    pub dilution_source: Option<String>,
    pub dilution_updated_at: Option<String>,

    pub ownership: OwnershipInfo,
    pub ownership_source: Option<String>,
    pub ownership_updated_at: Option<String>,

    pub last_updated_at: Option<String>,
    /// Most recent recent SEC filings (dilution + ownership), newest first.
    pub recent_filings: Vec<SecFiling>,
}

// ─── Wide reporting row (the "tickers data table") ─────────────────────────────

/// One flattened row of the tickers data table: the universe asset joined with
/// every enrichment source (fundamentals, company meta, company intel) plus the
/// news / filings counts. Purely a read/reporting view used to verify the data
/// exists per ticker and to sort/filter it in the UI. All fields are `Option`
/// (None = not collected) except the counts (0 = none).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TickerTableRow {
    // Identity + universe asset.
    pub symbol: String,
    pub name: Option<String>,
    pub exchange: Option<String>,
    pub tradable: bool,
    pub shortable: bool,
    pub float_shares: Option<i64>,
    pub market_cap: Option<i64>,
    pub avg_volume: Option<i64>,
    // Fundamentals cache.
    pub outstanding_shares: Option<i64>,
    pub free_float: Option<f64>,
    pub prev_close: Option<f64>,
    pub atr: Option<f64>,
    pub change_1d_pct: Option<f64>,
    pub change_2d_pct: Option<f64>,
    pub change_3d_pct: Option<f64>,
    pub change_4d_pct: Option<f64>,
    pub change_5d_pct: Option<f64>,
    pub change_6d_pct: Option<f64>,
    // Behavioural scores (computed at startup, DB-wide percentile 0..100).
    pub pump_dump_score: Option<f64>,
    pub dilution_score: Option<f64>,
    pub dilution_pct_12m: Option<f64>,
    pub shares_outstanding_12m: Option<f64>,
    // Absolute per-ticker risk scores (0..100; None = inputs not collected).
    pub dilution_capacity_score: Option<f64>,
    pub dilution_need_score: Option<f64>,
    pub short_interest_score: Option<f64>,
    // Splits (rolled up from ticker_splits).
    pub last_split_date: Option<String>,
    pub last_split_label: Option<String>,
    pub split_count_1y: Option<i64>,
    // Company meta (sec-api.io).
    pub country: Option<String>,
    pub industry: Option<String>,
    pub sector: Option<String>,
    pub sic: Option<String>,
    // Company intel — short interest.
    pub short_interest: Option<i64>,
    pub days_to_cover: Option<f64>,
    pub short_interest_settlement: Option<String>,
    // Company intel — financial health.
    pub net_income_last_q: Option<f64>,
    pub net_income_ttm: Option<f64>,
    pub negative_quarters_last4: Option<i64>,
    pub operating_cash_flow_ttm: Option<f64>,
    pub cash_and_equivalents: Option<f64>,
    pub financials_period_end: Option<String>,
    // Company intel — dilution.
    pub has_recent_shelf: bool,
    pub latest_dilution_form: Option<String>,
    pub latest_dilution_date: Option<String>,
    pub dilution_atm: bool,
    pub dilution_resale: bool,
    pub dilution_warrants: bool,
    pub offering_amount: Option<f64>,
    // Company intel — ownership.
    pub institutional_ownership_pct: Option<f64>,
    pub insider_ownership_pct: Option<f64>,
    pub holders_5pct_count: Option<i64>,
    pub restricted_shares: Option<i64>,
    // Counts + freshness.
    pub filings_count: i64,
    pub news_count: i64,
    pub intel_updated_at: Option<String>,
}
