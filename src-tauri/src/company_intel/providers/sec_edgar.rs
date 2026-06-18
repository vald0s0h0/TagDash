// SEC EDGAR provider (FREE, no API key). The backbone of the company-intel job.
//
// IMPORTANT: this is the public SEC EDGAR data service (data.sec.gov / efts.sec.gov),
// NOT the paid sec-api.io used elsewhere for country/industry (`crate::sec_api`).
// SEC requires a descriptive `User-Agent` on every request and rate-limits to
// ~10 req/s; we stay well under via the shared rate limiter.
//
// Endpoints used:
//   • ticker→CIK map: https://www.sec.gov/files/company_tickers.json
//   • submissions:     https://data.sec.gov/submissions/CIK{cik10}.json   (filings)
//   • company facts:   https://data.sec.gov/api/xbrl/companyfacts/CIK{cik10}.json
//   • full-text search:https://efts.sec.gov/LATEST/search-index            (13D/13G)

use std::collections::HashMap;

use chrono::{Datelike, NaiveDate};
use serde::Deserialize;

use super::super::error::{IntelError, IntelResult};
use super::super::http::{Http, RetryPolicy};
use super::super::model::{DilutionFlags, DilutionInfo, FinancialHealth, Holder, SecFiling};
use super::super::rate_limit::RateLimiter;

/// Descriptive User-Agent per SEC's fair-access policy (contact included).
pub const USER_AGENT: &str = "TagDash/1.0 (etienne.fabre@ensci.com)";
const DATA_BASE: &str = "https://data.sec.gov";
const WWW_BASE: &str = "https://www.sec.gov";
const FTS_BASE: &str = "https://efts.sec.gov/LATEST/search-index";

// Only the User-Agent: reqwest's `gzip` feature negotiates + decodes compression
// automatically, but ONLY when it owns the Accept-Encoding header — so we must not
// set it ourselves (doing so disables auto-decompression and breaks JSON parsing).
fn headers() -> [(&'static str, &'static str); 1] {
    [("User-Agent", USER_AGENT)]
}

// ─── Form classification ──────────────────────────────────────────────────────

/// S-3 family + prospectus / effectiveness forms = the dilution feed.
const DILUTION_FORMS: &[&str] = &[
    "S-3", "S-3/A", "S-3ASR", "EFFECT", "424B3", "424B5", "424B7", "POS AM", "POSASR",
];

/// Beneficial-ownership schedules (>5% holders) = the ownership feed.
const OWNERSHIP_FORMS: &[&str] = &["SC 13D", "SC 13G", "SC 13D/A", "SC 13G/A"];

/// 'dilution' | 'ownership' | 'other' for a raw form type (case-insensitive,
/// whitespace-normalised).
pub fn classify_form(form: &str) -> &'static str {
    let f = form.trim().to_uppercase();
    if DILUTION_FORMS.iter().any(|d| d.eq_ignore_ascii_case(&f)) {
        "dilution"
    } else if OWNERSHIP_FORMS.iter().any(|o| o.eq_ignore_ascii_case(&f)) {
        "ownership"
    } else {
        "other"
    }
}

/// Pad a numeric CIK to the 10-digit zero-padded form EDGAR uses in URLs.
pub fn pad_cik(cik: &str) -> String {
    let digits: String = cik.trim().chars().filter(|c| c.is_ascii_digit()).collect();
    format!("{:0>10}", digits)
}

/// Build the SEC document URL for a filing's primary document.
/// `cik` may be padded or not; the Archives path uses the un-padded integer.
pub fn document_url(cik: &str, accession_number: &str, primary_document: &str) -> Option<String> {
    if primary_document.trim().is_empty() {
        return None;
    }
    let cik_int = cik.trim_start_matches('0');
    let cik_int = if cik_int.is_empty() { "0" } else { cik_int };
    let acc_nodash: String = accession_number.chars().filter(|c| c.is_ascii_digit()).collect();
    Some(format!(
        "{WWW_BASE}/Archives/edgar/data/{cik_int}/{acc_nodash}/{primary_document}"
    ))
}

// ─── Ticker → CIK map ───────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct TickerRow {
    cik_str: serde_json::Value, // number in the feed
    ticker: String,
}

/// Fetch the full ticker→CIK map (uppercase ticker → 10-digit padded CIK).
/// One request; reused across the whole batch.
pub async fn fetch_ticker_cik_map(
    http: &Http,
    limiter: &RateLimiter,
    policy: &RetryPolicy,
) -> IntelResult<HashMap<String, String>> {
    let url = format!("{WWW_BASE}/files/company_tickers.json");
    // The feed is a JSON object keyed by an arbitrary index ("0","1",…).
    let raw: HashMap<String, TickerRow> =
        http.get_json(limiter, &url, &headers(), policy).await?;
    let mut map = HashMap::with_capacity(raw.len());
    for row in raw.into_values() {
        let cik = match &row.cik_str {
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::String(s) => s.clone(),
            _ => continue,
        };
        let ticker = row.ticker.trim().to_uppercase();
        if !ticker.is_empty() {
            map.insert(ticker, pad_cik(&cik));
        }
    }
    if map.is_empty() {
        return Err(IntelError::Parse("empty ticker→CIK map".into()));
    }
    Ok(map)
}

// ─── Submissions (recent filings) ──────────────────────────────────────────────

#[derive(Debug, Default, Deserialize)]
struct SubmissionsResponse {
    #[serde(default)]
    filings: Filings,
}

#[derive(Debug, Default, Deserialize)]
struct Filings {
    #[serde(default)]
    recent: RecentFilings,
}

#[derive(Debug, Default, Deserialize)]
struct RecentFilings {
    #[serde(default, rename = "accessionNumber")]
    accession_number: Vec<String>,
    #[serde(default, rename = "filingDate")]
    filing_date: Vec<String>,
    #[serde(default, rename = "reportDate")]
    report_date: Vec<String>,
    #[serde(default)]
    form: Vec<String>,
    #[serde(default, rename = "primaryDocument")]
    primary_document: Vec<String>,
    #[serde(default, rename = "primaryDocDescription")]
    primary_doc_description: Vec<String>,
}

/// Fetch and classify recent dilution + ownership filings for a ticker (issuer's
/// own filing feed). Returns the matching filings newest-first, capped at `cap`.
/// Forms outside the dilution / ownership sets are dropped.
pub async fn fetch_recent_filings(
    http: &Http,
    limiter: &RateLimiter,
    policy: &RetryPolicy,
    symbol: &str,
    cik: &str,
    cap: usize,
) -> IntelResult<Vec<SecFiling>> {
    let url = format!("{DATA_BASE}/submissions/CIK{}.json", pad_cik(cik));
    let resp: SubmissionsResponse = http.get_json(limiter, &url, &headers(), policy).await?;
    let r = &resp.filings.recent;
    let n = r.form.len();
    let at = |v: &[String], i: usize| v.get(i).cloned().unwrap_or_default();

    let mut out = Vec::new();
    for i in 0..n {
        let form = at(&r.form, i);
        let category = classify_form(&form);
        if category == "other" {
            continue;
        }
        let accession = at(&r.accession_number, i);
        let primary = at(&r.primary_document, i);
        let description = at(&r.primary_doc_description, i);
        let flags = flags_from_text(&format!("{form} {description}"));
        out.push(SecFiling {
            accession_number: accession.clone(),
            symbol: symbol.to_string(),
            cik: Some(pad_cik(cik)),
            form_type: form,
            filing_date: non_empty(at(&r.filing_date, i)),
            report_date: non_empty(at(&r.report_date, i)),
            document_url: document_url(cik, &accession, &primary),
            primary_document: non_empty(primary),
            description: non_empty(description),
            category: category.to_string(),
            detected_atm: flags.atm,
            detected_resale: flags.resale,
            detected_warrants: flags.warrants,
            offering_amount: flags.offering_amount,
        });
        if out.len() >= cap {
            break; // `recent` is already newest-first
        }
    }
    Ok(out)
}

/// Roll the classified filings up into the dilution summary, optionally scanning
/// the single most-recent dilution document for ATM/resale/warrant keywords and an
/// offering amount (bounded: one extra request per ticker).
pub async fn summarize_dilution(
    http: &Http,
    limiter: &RateLimiter,
    policy: &RetryPolicy,
    filings: &[SecFiling],
    scan_latest_doc: bool,
) -> DilutionInfo {
    let dilution: Vec<&SecFiling> = filings.iter().filter(|f| f.category == "dilution").collect();
    let Some(latest) = dilution.first() else {
        return DilutionInfo::default();
    };
    // Union the metadata-level flags across all recent dilution filings.
    let mut flags = DilutionFlags::default();
    for f in &dilution {
        flags.atm |= f.detected_atm;
        flags.resale |= f.detected_resale;
        flags.warrants |= f.detected_warrants;
        if flags.offering_amount.is_none() {
            flags.offering_amount = f.offering_amount;
        }
    }
    // Optionally deepen with the latest document's text (best-effort, never fatal).
    if scan_latest_doc {
        if let Some(url) = &latest.document_url {
            if let Ok(text) = http.get_text(limiter, url, &headers(), policy).await {
                let doc = flags_from_text(&text);
                flags.atm |= doc.atm;
                flags.resale |= doc.resale;
                flags.warrants |= doc.warrants;
                flags.offering_amount = flags.offering_amount.or(doc.offering_amount);
            }
        }
    }
    DilutionInfo {
        has_recent_shelf: true,
        latest_form: Some(latest.form_type.clone()),
        latest_date: latest.filing_date.clone(),
        flags,
    }
}

// ─── Dilution keyword / amount detection (pure) ────────────────────────────────

/// Detect ATM / resale / warrant signals + an offering dollar amount in a blob of
/// text (a filing description or document body). Case-insensitive.
pub fn flags_from_text(text: &str) -> DilutionFlags {
    let lower = text.to_lowercase();
    DilutionFlags {
        atm: lower.contains("at-the-market") || lower.contains("at the market") || lower.contains("atm offering"),
        resale: lower.contains("resale") || lower.contains("selling stockholder") || lower.contains("selling shareholder"),
        warrants: lower.contains("warrant"),
        offering_amount: parse_offering_amount(&lower),
    }
}

/// Best-effort extraction of an offering dollar amount like "$50,000,000" or
/// "$50.0 million" / "up to $100 million". Returns the FIRST plausible amount.
pub fn parse_offering_amount(lower: &str) -> Option<f64> {
    let bytes = lower.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' {
            // Read the number after the '$' (digits, commas, dot).
            let mut j = i + 1;
            while j < bytes.len() && bytes[j] == b' ' {
                j += 1;
            }
            let num_start = j;
            while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == b',' || bytes[j] == b'.') {
                j += 1;
            }
            if j > num_start {
                let num_str: String = lower[num_start..j].chars().filter(|c| *c != ',').collect();
                if let Ok(mut val) = num_str.parse::<f64>() {
                    // Scale word immediately following (e.g. "$50 million").
                    let tail = lower[j..].trim_start();
                    if tail.starts_with("billion") {
                        val *= 1e9;
                    } else if tail.starts_with("million") {
                        val *= 1e6;
                    }
                    if val >= 100_000.0 {
                        return Some(val);
                    }
                }
            }
        }
        i += 1;
    }
    None
}

// ─── Company facts (XBRL financial health) ─────────────────────────────────────

#[derive(Debug, Default, Deserialize)]
struct CompanyFacts {
    #[serde(default)]
    facts: Facts,
}

#[derive(Debug, Default, Deserialize)]
struct Facts {
    #[serde(default, rename = "us-gaap")]
    us_gaap: HashMap<String, Concept>,
}

#[derive(Debug, Default, Deserialize)]
struct Concept {
    #[serde(default)]
    units: HashMap<String, Vec<FactEntry>>,
}

#[derive(Debug, Clone, Deserialize)]
struct FactEntry {
    #[serde(default)]
    start: Option<String>,
    #[serde(default)]
    end: Option<String>,
    #[serde(default)]
    val: Option<f64>,
    #[serde(default)]
    frame: Option<String>,
}

/// Number of inclusive days between two `YYYY-MM-DD` dates.
fn span_days(start: &str, end: &str) -> Option<i64> {
    let s = NaiveDate::parse_from_str(&start[..start.len().min(10)], "%Y-%m-%d").ok()?;
    let e = NaiveDate::parse_from_str(&end[..end.len().min(10)], "%Y-%m-%d").ok()?;
    Some((e - s).num_days())
}

/// Quarterly (≈3-month) USD values for a duration concept, deduped by period end,
/// sorted by end date ascending. `(end_date, value)`.
fn quarterly_values(entries: &[FactEntry]) -> Vec<(String, f64)> {
    let mut by_end: HashMap<String, (f64, bool)> = HashMap::new(); // end → (val, has_frame)
    for e in entries {
        let (Some(start), Some(end), Some(val)) = (&e.start, &e.end, e.val) else { continue };
        let Some(days) = span_days(start, end) else { continue };
        if !(80..=100).contains(&days) {
            continue; // keep only ~one-quarter windows
        }
        let has_frame = e.frame.as_deref().map(is_quarter_frame).unwrap_or(false);
        // Prefer a standardized (framed) value when the same quarter is restated.
        by_end
            .entry(end.clone())
            .and_modify(|cur| {
                if has_frame && !cur.1 {
                    *cur = (val, true);
                }
            })
            .or_insert((val, has_frame));
    }
    let mut out: Vec<(String, f64)> = by_end.into_iter().map(|(k, (v, _))| (k, v)).collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Annual (≈12-month) USD values for a duration concept, sorted by end ascending.
fn annual_values(entries: &[FactEntry]) -> Vec<(String, f64)> {
    let mut by_end: HashMap<String, f64> = HashMap::new();
    for e in entries {
        let (Some(start), Some(end), Some(val)) = (&e.start, &e.end, e.val) else { continue };
        let Some(days) = span_days(start, end) else { continue };
        if (330..=400).contains(&days) {
            by_end.insert(end.clone(), val);
        }
    }
    let mut out: Vec<(String, f64)> = by_end.into_iter().collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn is_quarter_frame(frame: &str) -> bool {
    // "CY2024Q3" / "CY2024Q3I". Quarterly frames carry a 'Q'.
    frame.contains('Q')
}

/// Most recent instant USD value for a concept (e.g. cash on the balance sheet).
fn latest_instant(entries: &[FactEntry]) -> Option<(String, f64)> {
    entries
        .iter()
        .filter_map(|e| Some((e.end.clone()?, e.val?)))
        .max_by(|a, b| a.0.cmp(&b.0))
}

/// Pull the financial-health fields out of a parsed company-facts document.
fn financials_from_facts(facts: &CompanyFacts) -> FinancialHealth {
    let mut fh = FinancialHealth::default();
    let usd = |concept: &str| -> Vec<FactEntry> {
        facts
            .facts
            .us_gaap
            .get(concept)
            .and_then(|c| c.units.get("USD"))
            .cloned()
            .unwrap_or_default()
    };

    // Net income (loss): quarterly series → last Q, TTM, negative-quarter count.
    let ni = quarterly_values(&usd("NetIncomeLoss"));
    if let Some((end, val)) = ni.last() {
        fh.net_income_last_q = Some(*val);
        fh.period_end = Some(end.clone());
    }
    if ni.len() >= 4 {
        let last4 = &ni[ni.len() - 4..];
        fh.net_income_ttm = Some(last4.iter().map(|(_, v)| v).sum());
        fh.negative_quarters_last4 = Some(last4.iter().filter(|(_, v)| *v < 0.0).count() as i64);
    } else if !ni.is_empty() {
        // Fewer than 4 quarters available: still report the loss count we have.
        fh.negative_quarters_last4 = Some(ni.iter().filter(|(_, v)| *v < 0.0).count() as i64);
    }

    // Operating cash flow TTM: most recent fiscal-year value (trailing twelve
    // months as of the last annual report). Cash-flow figures in 10-Qs are
    // year-to-date, not per-quarter, so a clean annual value is the robust pick.
    let ocf = annual_values(&usd("NetCashProvidedByUsedInOperatingActivities"));
    fh.operating_cash_flow_ttm = ocf.last().map(|(_, v)| *v);

    // Cash & equivalents: most recent balance-sheet instant.
    let cash = latest_instant(&usd("CashAndCashEquivalentsAtCarryingValue"));
    let cash = cash.or_else(|| {
        latest_instant(&usd("CashCashEquivalentsRestrictedCashAndRestrictedCashEquivalents"))
    });
    fh.cash_and_equivalents = cash.map(|(_, v)| v);

    fh
}

/// Fetch + parse SEC company-facts XBRL into the financial-health section.
pub async fn fetch_financials(
    http: &Http,
    limiter: &RateLimiter,
    policy: &RetryPolicy,
    cik: &str,
) -> IntelResult<FinancialHealth> {
    let url = format!("{DATA_BASE}/api/xbrl/companyfacts/CIK{}.json", pad_cik(cik));
    let facts: CompanyFacts = http.get_json(limiter, &url, &headers(), policy).await?;
    let fh = financials_from_facts(&facts);
    if fh.is_empty() {
        Err(IntelError::NotFound)
    } else {
        Ok(fh)
    }
}

// ─── Historical shares outstanding (XBRL frames, BULK) ─────────────────────────
// One request per period returns the concept for EVERY filer, so a handful of
// quarterly frames yields a market-wide shares-outstanding time series. We use
// dei:EntityCommonStockSharesOutstanding (the cover-page count) on instant frames
// ("CY2024Q4I"). NOTE: as-reported, NOT split-adjusted — the dilution scorer
// neutralises splits separately.

#[derive(Debug, Default, Deserialize)]
struct FramesResponse {
    #[serde(default)]
    data: Vec<FrameEntry>,
}

#[derive(Debug, Deserialize)]
struct FrameEntry {
    #[serde(default)]
    cik: Option<serde_json::Value>, // number in the feed
    #[serde(default)]
    end: Option<String>,
    #[serde(default)]
    val: Option<f64>,
}

/// Build the list of recent quarterly frames newest-first, each ending at least
/// `lag_days` before `today` (so filings likely exist). `instant` → "CY{Y}Q{Q}I"
/// (balance-sheet instants like shares/cash); otherwise → "CY{Y}Q{Q}" (the
/// 3-month duration concepts like quarterly net income).
pub fn recent_quarter_frames(today: NaiveDate, count: usize, lag_days: i64, instant: bool) -> Vec<String> {
    let cutoff = today - chrono::Duration::days(lag_days);
    let quarter_end = |y: i32, q: u32| -> NaiveDate {
        let (m, d) = match q {
            1 => (3, 31),
            2 => (6, 30),
            3 => (9, 30),
            _ => (12, 31),
        };
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    };
    let suffix = if instant { "I" } else { "" };
    // Walk back from the current quarter until end <= cutoff.
    let mut y = cutoff.year();
    let mut q = ((cutoff.month() - 1) / 3) + 1;
    while quarter_end(y, q) > cutoff {
        if q == 1 {
            q = 4;
            y -= 1;
        } else {
            q -= 1;
        }
    }
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        out.push(format!("CY{y}Q{q}{suffix}"));
        if q == 1 {
            q = 4;
            y -= 1;
        } else {
            q -= 1;
        }
    }
    out
}

/// Build the list of recent ANNUAL (full-year duration) frames "CY{Y}" newest-first
/// (e.g. for operating cash flow), each year ending at least `lag_days` before
/// `today` (10-Ks land months after year end, so use a generous lag).
pub fn recent_annual_frames(today: NaiveDate, count: usize, lag_days: i64) -> Vec<String> {
    let cutoff = today - chrono::Duration::days(lag_days);
    let mut y = cutoff.year();
    while NaiveDate::from_ymd_opt(y, 12, 31).unwrap() > cutoff {
        y -= 1;
    }
    (0..count as i32).map(|i| format!("CY{}", y - i)).collect()
}

/// Fetch one XBRL frame of any concept for the whole market. Returns
/// `(padded_cik, period_end, val)` rows (NO sign filter — net income can be
/// negative). A 404 (frame not published yet) surfaces as `NotFound`.
pub async fn fetch_concept_frame(
    http: &Http,
    limiter: &RateLimiter,
    policy: &RetryPolicy,
    taxonomy: &str,
    concept: &str,
    unit: &str,
    frame: &str,
) -> IntelResult<Vec<(String, String, f64)>> {
    let url = format!("{DATA_BASE}/api/xbrl/frames/{taxonomy}/{concept}/{unit}/{frame}.json");
    let resp: FramesResponse = http.get_json(limiter, &url, &headers(), policy).await?;
    let mut out = Vec::with_capacity(resp.data.len());
    for e in resp.data {
        let (Some(cik), Some(end), Some(val)) = (e.cik, e.end, e.val) else { continue };
        if !val.is_finite() {
            continue;
        }
        let cik = match cik {
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::String(s) => s,
            _ => continue,
        };
        out.push((pad_cik(&cik), end, val));
    }
    if out.is_empty() {
        Err(IntelError::NotFound)
    } else {
        Ok(out)
    }
}

/// Fetch one instant frame of dei:EntityCommonStockSharesOutstanding (shares only,
/// positive values). Thin wrapper over `fetch_concept_frame`.
pub async fn fetch_shares_outstanding_frame(
    http: &Http,
    limiter: &RateLimiter,
    policy: &RetryPolicy,
    frame: &str,
) -> IntelResult<Vec<(String, String, f64)>> {
    let rows =
        fetch_concept_frame(http, limiter, policy, "dei", "EntityCommonStockSharesOutstanding", "shares", frame)
            .await?;
    let out: Vec<_> = rows.into_iter().filter(|(_, _, v)| *v > 0.0).collect();
    if out.is_empty() {
        Err(IntelError::NotFound)
    } else {
        Ok(out)
    }
}

/// Invert a ticker→CIK map into CIK→[tickers] (share classes share one CIK).
pub fn invert_cik_map(map: &HashMap<String, String>) -> HashMap<String, Vec<String>> {
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    for (ticker, cik) in map {
        out.entry(cik.clone()).or_default().push(ticker.clone());
    }
    out
}

// ─── Ownership: 13D/13G holders via full-text search ───────────────────────────

#[derive(Debug, Deserialize)]
struct FtsResponse {
    #[serde(default)]
    hits: FtsHits,
}
#[derive(Debug, Default, Deserialize)]
struct FtsHits {
    #[serde(default)]
    hits: Vec<FtsHit>,
}
#[derive(Debug, Deserialize)]
struct FtsHit {
    #[serde(rename = "_source", default)]
    source: FtsSource,
}
#[derive(Debug, Default, Deserialize)]
struct FtsSource {
    #[serde(default)]
    display_names: Vec<String>,
    #[serde(default)]
    file_date: Option<String>,
    #[serde(default)]
    root_form: Option<String>,
}

/// Strip the trailing "(CIK 0001234567)" from an EDGAR display name.
fn clean_display_name(name: &str) -> String {
    match name.find("(CIK") {
        Some(idx) => name[..idx].trim().to_string(),
        None => name.trim().to_string(),
    }
}

/// Find recent >5% holders (Schedule 13D/13G) naming this issuer, via EDGAR
/// full-text search filtered by the issuer CIK + the ownership forms. Best-effort:
/// percentages aren't in the index (left None), and any failure degrades to empty.
pub async fn fetch_holders(
    http: &Http,
    limiter: &RateLimiter,
    policy: &RetryPolicy,
    cik: &str,
    cap: usize,
) -> IntelResult<Vec<Holder>> {
    let cik10 = pad_cik(cik);
    let url = reqwest::Url::parse_with_params(
        FTS_BASE,
        &[
            ("forms", "SC 13D,SC 13G,SC 13D/A,SC 13G/A"),
            ("ciks", cik10.as_str()),
        ],
    )
    .map_err(|e| IntelError::Parse(e.to_string()))?;
    let resp: FtsResponse = http.get_json(limiter, url.as_str(), &headers(), policy).await?;

    let mut holders: Vec<Holder> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for hit in resp.hits.hits {
        let src = hit.source;
        // The holder is the associated entity that ISN'T the issuer. Heuristic:
        // pick the first display name whose CIK differs from the issuer's.
        let holder_name = src
            .display_names
            .iter()
            .find(|n| !n.contains(&cik10) && !n.contains(cik.trim_start_matches('0')))
            .or_else(|| src.display_names.first())
            .map(|n| clean_display_name(n))
            .unwrap_or_default();
        if holder_name.is_empty() || !seen.insert(holder_name.to_uppercase()) {
            continue;
        }
        holders.push(Holder {
            name: holder_name,
            pct: None,
            form: src.root_form.unwrap_or_default(),
            date: src.file_date.unwrap_or_default(),
        });
        if holders.len() >= cap {
            break;
        }
    }
    Ok(holders)
}

fn non_empty(s: String) -> Option<String> {
    let t = s.trim();
    if t.is_empty() { None } else { Some(t.to_string()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cik_padding() {
        assert_eq!(pad_cik("320193"), "0000320193");
        assert_eq!(pad_cik("0000320193"), "0000320193");
        assert_eq!(pad_cik(" 1234 "), "0000001234");
    }

    #[test]
    fn form_classification() {
        assert_eq!(classify_form("S-3"), "dilution");
        assert_eq!(classify_form("424B5"), "dilution");
        assert_eq!(classify_form("POS AM"), "dilution");
        assert_eq!(classify_form("sc 13g"), "ownership");
        assert_eq!(classify_form("SC 13D/A"), "ownership");
        assert_eq!(classify_form("10-K"), "other");
        assert_eq!(classify_form("8-K"), "other");
    }

    #[test]
    fn doc_url_built_from_unpadded_cik() {
        let u = document_url("0000320193", "0000320193-24-000123", "form.htm").unwrap();
        assert_eq!(u, "https://www.sec.gov/Archives/edgar/data/320193/000032019324000123/form.htm");
        assert!(document_url("320193", "x-24-1", "").is_none());
    }

    #[test]
    fn keyword_detection() {
        let f = flags_from_text("Resale of shares by the selling stockholders, including warrants");
        assert!(f.resale && f.warrants && !f.atm);
        let f = flags_from_text("At-the-Market Offering Agreement for up to $75,000,000");
        assert!(f.atm);
        assert_eq!(f.offering_amount, Some(75_000_000.0));
    }

    #[test]
    fn offering_amount_scaling() {
        assert_eq!(parse_offering_amount("up to $50 million in shares"), Some(50_000_000.0));
        assert_eq!(parse_offering_amount("aggregate of $1.5 billion"), Some(1_500_000_000.0));
        assert_eq!(parse_offering_amount("$250,000,000 shelf"), Some(250_000_000.0));
        // Too small to be an offering → ignored.
        assert_eq!(parse_offering_amount("a fee of $500"), None);
        assert_eq!(parse_offering_amount("no dollar figure here"), None);
    }

    #[test]
    fn xbrl_net_income_quarterlies() {
        // Four ~quarterly net-income entries (two negative) + one annual that must
        // be ignored by the quarterly extractor.
        let entries = vec![
            FactEntry { start: Some("2024-01-01".into()), end: Some("2024-03-31".into()), val: Some(-1_000.0), frame: Some("CY2024Q1".into()) },
            FactEntry { start: Some("2024-04-01".into()), end: Some("2024-06-30".into()), val: Some(-500.0),  frame: Some("CY2024Q2".into()) },
            FactEntry { start: Some("2024-07-01".into()), end: Some("2024-09-30".into()), val: Some(200.0),   frame: Some("CY2024Q3".into()) },
            FactEntry { start: Some("2024-10-01".into()), end: Some("2024-12-31".into()), val: Some(300.0),   frame: Some("CY2024Q4".into()) },
            FactEntry { start: Some("2024-01-01".into()), end: Some("2024-12-31".into()), val: Some(-1_000.0), frame: Some("CY2024".into()) },
        ];
        let q = quarterly_values(&entries);
        assert_eq!(q.len(), 4);
        assert_eq!(q.last().unwrap().0, "2024-12-31");

        let mut facts = CompanyFacts::default();
        let mut concept = Concept::default();
        concept.units.insert("USD".into(), entries);
        facts.facts.us_gaap.insert("NetIncomeLoss".into(), concept);
        let fh = financials_from_facts(&facts);
        assert_eq!(fh.net_income_last_q, Some(300.0));
        assert_eq!(fh.net_income_ttm, Some(-1_000.0)); // -1000-500+200+300
        assert_eq!(fh.negative_quarters_last4, Some(2));
        assert_eq!(fh.period_end.as_deref(), Some("2024-12-31"));
    }

    #[test]
    fn xbrl_cash_takes_latest_instant() {
        let mut facts = CompanyFacts::default();
        let mut concept = Concept::default();
        concept.units.insert(
            "USD".into(),
            vec![
                FactEntry { start: None, end: Some("2023-12-31".into()), val: Some(5_000.0), frame: None },
                FactEntry { start: None, end: Some("2024-09-30".into()), val: Some(9_000.0), frame: None },
            ],
        );
        facts.facts.us_gaap.insert("CashAndCashEquivalentsAtCarryingValue".into(), concept);
        let fh = financials_from_facts(&facts);
        assert_eq!(fh.cash_and_equivalents, Some(9_000.0));
    }

    #[test]
    fn display_name_cleaning() {
        assert_eq!(clean_display_name("BLACKROCK INC. (CIK 0001364742)"), "BLACKROCK INC.");
        assert_eq!(clean_display_name("Some Holder"), "Some Holder");
    }

    #[test]
    fn quarter_frames_instant_vs_duration() {
        let today = NaiveDate::from_ymd_opt(2026, 6, 18).unwrap();
        // lag 75d → cutoff 2026-04-04 → most recent quarter end ≤ cutoff = 2026 Q1.
        let inst = recent_quarter_frames(today, 3, 75, true);
        assert_eq!(inst, vec!["CY2026Q1I", "CY2025Q4I", "CY2025Q3I"]);
        let dur = recent_quarter_frames(today, 3, 75, false);
        assert_eq!(dur, vec!["CY2026Q1", "CY2025Q4", "CY2025Q3"]);
    }

    #[test]
    fn annual_frames_format() {
        let today = NaiveDate::from_ymd_opt(2026, 6, 18).unwrap();
        // lag 120d → cutoff 2026-02-18 → most recent full year ≤ cutoff = 2025.
        assert_eq!(recent_annual_frames(today, 2, 120), vec!["CY2025", "CY2024"]);
    }
}
