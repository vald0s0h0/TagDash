// Company intelligence — collection + storage layer.
//
// Collects per-ticker "company intelligence" (short interest, financial health,
// registered-S-3 / dilution filings, ownership / locked shares) from isolated
// providers (SEC EDGAR, Massive, FMP) and stores it normalized in SQLite. The data
// is PURELY ADDITIVE: it never touches float / fundamentals, which keep being owned
// by the startup pipeline.
//
// ── Designed to become a background worker ──────────────────────────────────────
// Everything here is self-contained and DB-handle-driven. The single entry point a
// future OS-launched background service needs is `run_collection_job(db, config,
// secrets)` (or `refresh_company_intel_batch` for an explicit ticker list). There
// are no Tauri / UI dependencies inside this module, no network calls triggered by
// the UI, and the live scanner is never blocked: this runs on its own spawned task,
// only ever taking the SQLite mutex for short, await-free critical sections.
//
// ── Reliability ────────────────────────────────────────────────────────────────
// Per-provider rate limiting (configurable req/min), retry with exponential
// backoff on transient failures, and per-section error capture: when one source is
// unavailable, only that section is skipped and its previous good value is
// preserved (see the repository's per-section upserts).

pub mod catalog;
pub mod error;
pub mod http;
pub mod model;
pub mod ondemand;
pub mod providers;
pub mod rate_limit;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use chrono::Utc;
use serde::Serialize;

use crate::config::{secrets::Secrets, AppConfig, CompanyIntelConfig};
use crate::local_db::{
    cache_repository, company_intel_repository as repo, insert_log, universe_repository,
};

pub use catalog::{catalog, IntelField};
pub use error::{IntelError, IntelResult};
pub use model::{CompanyIntel, TickerTableRow};

use error::IntelError as E;
use http::{Http, RetryPolicy};
use providers::{fmp_fin, massive_si, sec_edgar};
use rate_limit::RateLimiter;

/// Max recent dilution/ownership filings stored per ticker per run.
const FILINGS_CAP: usize = 40;
/// Max >5% holders surfaced per ticker.
const HOLDERS_CAP: usize = 15;
/// Fetch + keyword-scan the latest dilution document (one extra SEC request per
/// ticker that has a shelf filing). SEC's budget is generous, so it's worth it.
const SCAN_LATEST_DILUTION_DOC: bool = true;

/// 13D/13G >5% holder lookup via EDGAR full-text search is DISABLED: the
/// efts.sec.gov endpoint rejects our `forms=…&ciks=…` query (it requires a `q`
/// term and 500s on the encoded `/A` amendment forms), and a 500 is retryable so
/// it spammed the log 3× per ticker. Left off until a supported query is wired;
/// the ownership-holders section stays uncollected meanwhile (degrades cleanly).
const ENABLE_HOLDERS_FTS: bool = false;

/// Outcome of a collection run, returned for logging / the manual command.
#[derive(Debug, Clone, Default, Serialize)]
pub struct JobSummary {
    pub processed: usize,
    pub succeeded: usize,
    pub with_errors: usize,
}

// ─── Provider bundle (rate limiters + HTTP clients, shared across a batch) ──────

struct Providers {
    sec_http: Http,
    sec_rl: RateLimiter,
    massive_http: Http,
    massive_rl: RateLimiter,
    fmp_http: Http,
    fmp_rl: RateLimiter,
    policy: RetryPolicy,
}

impl Providers {
    fn new(cfg: &CompanyIntelConfig) -> Self {
        Self {
            sec_http: Http::new("sec_edgar"),
            sec_rl: RateLimiter::per_minute(cfg.sec_rpm),
            massive_http: Http::new("massive"),
            massive_rl: RateLimiter::per_minute(cfg.massive_rpm),
            fmp_http: Http::new("fmp"),
            fmp_rl: RateLimiter::per_minute(cfg.fmp_rpm),
            policy: RetryPolicy::default(),
        }
    }
}

/// Snapshot of the keys the job needs (taken once; never hold the lock across an
/// await).
struct Keys {
    massive: Option<String>,
    fmp: Option<String>,
}

fn nonempty(o: &Option<String>) -> Option<String> {
    o.as_deref().filter(|s| !s.trim().is_empty()).map(String::from)
}

// ─── Public API ────────────────────────────────────────────────────────────────

/// Refresh the company intel for a single ticker. Convenience wrapper over the
/// batch path (fetches the ticker→CIK map only if the CIK isn't already cached).
pub async fn refresh_company_intel(
    db: Arc<Mutex<rusqlite::Connection>>,
    config: Arc<RwLock<AppConfig>>,
    secrets: Arc<RwLock<Secrets>>,
    symbol: String,
) -> JobSummary {
    let cfg = config.read().unwrap().company_intel.clone();
    refresh_company_intel_batch(db, cfg, secrets, vec![symbol]).await
}

/// Refresh the company intel for a list of tickers, reusing one set of rate-limited
/// provider clients and one ticker→CIK map for the whole batch.
pub async fn refresh_company_intel_batch(
    db: Arc<Mutex<rusqlite::Connection>>,
    cfg: CompanyIntelConfig,
    secrets: Arc<RwLock<Secrets>>,
    tickers: Vec<String>,
) -> JobSummary {
    let mut summary = JobSummary::default();
    if tickers.is_empty() {
        return summary;
    }

    let providers = Providers::new(&cfg);
    let keys = {
        let s = secrets.read().unwrap();
        Keys { massive: nonempty(&s.massive_api_key), fmp: nonempty(&s.fmp_api_key) }
    };

    // Resolve the ticker→CIK map only if at least one ticker has no cached CIK.
    let need_map = {
        let conn = db.lock().unwrap();
        tickers
            .iter()
            .any(|t| repo::get_cik(&conn, t).filter(|c| !c.is_empty()).is_none())
    };
    let cik_map: HashMap<String, String> = if need_map {
        match sec_edgar::fetch_ticker_cik_map(&providers.sec_http, &providers.sec_rl, &providers.policy)
            .await
        {
            Ok(m) => m,
            Err(e) => {
                log(&db, "warn", &format!("company_intel: ticker→CIK map unavailable: {e}"));
                HashMap::new()
            }
        }
    } else {
        HashMap::new()
    };

    for symbol in &tickers {
        match refresh_one(&db, &providers, &keys, &cik_map, symbol).await {
            true => summary.succeeded += 1,
            false => summary.with_errors += 1,
        }
        summary.processed += 1;
    }
    summary
}

/// Autonomous collection job — the single entry point a future background worker
/// (Mac/Windows login service) would call. Picks the stale tickers from the
/// universe (TTL-gated), bounds the run, and refreshes them. Never blocks the
/// scanner and makes no UI calls.
pub async fn run_collection_job(
    db: Arc<Mutex<rusqlite::Connection>>,
    config: Arc<RwLock<AppConfig>>,
    secrets: Arc<RwLock<Secrets>>,
) {
    let cfg = { config.read().unwrap().company_intel.clone() };
    if !cfg.enabled {
        log(&db, "info", "company_intel: collection disabled in config");
        return;
    }

    // Candidate set = the tradable universe. TTL-filter against what's already
    // cached, then bound to the per-run cap so a startup pass stays light; later
    // runs (or the future worker) pick up the rest.
    let candidates: Vec<String> = {
        let conn = db.lock().unwrap();
        universe_repository::get_all(&conn)
            .unwrap_or_default()
            .into_iter()
            .filter(|a| a.tradable)
            .map(|a| a.symbol)
            .collect()
    };
    let last_updated = {
        let conn = db.lock().unwrap();
        repo::all_last_updated(&conn).unwrap_or_default()
    };
    let cutoff = (Utc::now() - chrono::Duration::hours(cfg.ttl_hours.max(0) as i64))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    let mut stale: Vec<String> = candidates
        .into_iter()
        .filter(|s| match last_updated.get(s) {
            Some(ts) if !ts.is_empty() => ts.as_str() < cutoff.as_str(),
            _ => true, // never collected
        })
        .collect();
    stale.truncate(cfg.max_tickers_per_run.max(1));

    if stale.is_empty() {
        log(&db, "info", "company_intel: nothing stale to collect");
        return;
    }
    log(&db, "info", &format!("company_intel: collecting {} ticker(s)", stale.len()));
    let summary = refresh_company_intel_batch(db.clone(), cfg, secrets, stale).await;
    log(
        &db,
        "info",
        &format!(
            "company_intel: done — {} processed, {} ok, {} with errors",
            summary.processed, summary.succeeded, summary.with_errors
        ),
    );
}

// ─── Bulk collectors (run from the startup pipeline, whole-universe) ────────────

/// Bulk short-interest collection: one Massive dump for the whole universe (the
/// per-ticker path only ever reached ~50 tickers/launch). Persists the latest
/// report per ticker into the `company_intel` short-interest columns. Returns the
/// number of tickers stored. Filtered to the tradable universe.
pub async fn collect_short_interest_bulk(
    db: Arc<Mutex<rusqlite::Connection>>,
    secrets: Arc<RwLock<Secrets>>,
) -> Result<usize, String> {
    let key = {
        let s = secrets.read().unwrap();
        nonempty(&s.massive_api_key)
    }
    .ok_or_else(|| "Massive API key not configured".to_string())?;

    let universe: std::collections::HashSet<String> = {
        let conn = db.lock().unwrap();
        universe_repository::get_active_symbols(&conn)
            .unwrap_or_default()
            .into_iter()
            .map(|s| s.to_uppercase())
            .collect()
    };

    let rows = crate::massive::short_interest::fetch_short_interest_all(&key).await?;
    let now = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

    let conn = db.lock().unwrap();
    let _ = conn.execute_batch("BEGIN");
    let mut n = 0usize;
    for r in &rows {
        if !universe.is_empty() && !universe.contains(&r.symbol.to_uppercase()) {
            continue;
        }
        let si = model::ShortInterest {
            short_interest: r.short_interest,
            days_to_cover: r.days_to_cover,
            settlement_date: r.settlement_date.clone(),
        };
        if repo::upsert_short_interest(&conn, &r.symbol, &si, "Massive (bulk)", &now).is_ok() {
            n += 1;
        }
    }
    let _ = conn.execute_batch("COMMIT");
    Ok(n)
}

/// Build a `FinancialHealth` from this CIK's bulk-frame data: quarterly net-income
/// series (dedup by period end, ascending), the latest annual operating cash flow,
/// and the latest cash instant.
fn build_financial_health(
    ni: Option<&Vec<(String, f64)>>,
    ocf: Option<&(String, f64)>,
    cash: Option<&(String, f64)>,
) -> model::FinancialHealth {
    let mut fh = model::FinancialHealth::default();
    if let Some(ni) = ni {
        let mut by_end: HashMap<String, f64> = HashMap::new();
        for (end, val) in ni {
            by_end.insert(end.clone(), *val);
        }
        let mut series: Vec<(String, f64)> = by_end.into_iter().collect();
        series.sort_by(|a, b| a.0.cmp(&b.0));
        if let Some((end, val)) = series.last() {
            fh.net_income_last_q = Some(*val);
            fh.period_end = Some(end.clone());
        }
        let last4: Vec<&(String, f64)> = series.iter().rev().take(4).collect();
        if last4.len() == 4 {
            fh.net_income_ttm = Some(last4.iter().map(|(_, v)| v).sum());
        }
        if !last4.is_empty() {
            fh.negative_quarters_last4 = Some(last4.iter().filter(|(_, v)| *v < 0.0).count() as i64);
        }
    }
    if let Some((_, val)) = ocf {
        fh.operating_cash_flow_ttm = Some(*val);
    }
    if let Some((end, val)) = cash {
        fh.cash_and_equivalents = Some(*val);
        if fh.period_end.is_none() {
            fh.period_end = Some(end.clone());
        }
    }
    fh
}

/// Bulk SEC XBRL-frames collection — a handful of market-wide requests behind ONE
/// ticker→CIK map fetch:
///   • historical shares outstanding (instant frames) → `dilution_snapshots`
///     (feeds the dilution score), and
///   • financial health — quarterly net income, annual operating cash flow, cash —
///     → `company_intel` financials columns (feeds the "besoin de diluer" score).
/// Both are restricted to the tradable universe. Returns (snapshot rows written,
/// tickers with financials written).
pub async fn collect_sec_bulk(
    db: Arc<Mutex<rusqlite::Connection>>,
    config: Arc<RwLock<AppConfig>>,
) -> Result<(usize, usize), String> {
    let cfg = { config.read().unwrap().company_intel.clone() };
    let sec_http = Http::new("sec_edgar");
    let sec_rl = RateLimiter::per_minute(cfg.sec_rpm);
    let policy = RetryPolicy::default();

    // ticker→CIK map, inverted to CIK→[tickers] (one fetch for the whole job).
    let cik_map = sec_edgar::fetch_ticker_cik_map(&sec_http, &sec_rl, &policy)
        .await
        .map_err(|e| e.to_string())?;
    let cik_to_tickers = sec_edgar::invert_cik_map(&cik_map);

    let universe: std::collections::HashSet<String> = {
        let conn = db.lock().unwrap();
        universe_repository::get_active_symbols(&conn)
            .unwrap_or_default()
            .into_iter()
            .map(|s| s.to_uppercase())
            .collect()
    };
    let in_universe = |t: &str| universe.is_empty() || universe.contains(&t.to_uppercase());

    let today = crate::time::et_date(Utc::now());
    let today_nd = chrono::NaiveDate::parse_from_str(&today, "%Y-%m-%d")
        .unwrap_or_else(|_| Utc::now().date_naive());
    let now_ts = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

    // ── Shares outstanding (instant frames, ≈21 months) → dilution_snapshots ──
    let share_frames = sec_edgar::recent_quarter_frames(today_nd, 7, 75, true);
    let mut snapshot_rows: Vec<(String, String, f64)> = Vec::new();
    for frame in &share_frames {
        match sec_edgar::fetch_shares_outstanding_frame(&sec_http, &sec_rl, &policy, frame).await {
            Ok(entries) => {
                for (cik, end, val) in entries {
                    let Some(tickers) = cik_to_tickers.get(&cik) else { continue };
                    for t in tickers {
                        if in_universe(t) {
                            snapshot_rows.push((t.clone(), end.clone(), val));
                        }
                    }
                }
            }
            Err(e) => log(&db, "info", &format!("company_intel: shares frame {frame} skipped: {e}")),
        }
    }
    let snapshots = {
        let conn = db.lock().unwrap();
        cache_repository::upsert_dilution_snapshots(&conn, &snapshot_rows).map_err(|e| e.to_string())?
    };

    // ── Financial health (besoin de diluer) ───────────────────────────────────
    // Helper: accumulate a concept's frames into a per-CIK collection.
    async fn collect_frames(
        http: &Http, rl: &RateLimiter, policy: &RetryPolicy,
        concept: &str, frames: &[String],
    ) -> Vec<(String, String, f64)> {
        let mut out = Vec::new();
        for frame in frames {
            if let Ok(entries) =
                sec_edgar::fetch_concept_frame(http, rl, policy, "us-gaap", concept, "USD", frame).await
            {
                out.extend(entries);
            }
        }
        out
    }

    // Net income: 5 quarterly DURATION frames → per-CIK series.
    let ni_frames = sec_edgar::recent_quarter_frames(today_nd, 5, 50, false);
    let ni_rows = collect_frames(&sec_http, &sec_rl, &policy, "NetIncomeLoss", &ni_frames).await;
    let mut ni_by_cik: HashMap<String, Vec<(String, f64)>> = HashMap::new();
    for (cik, end, val) in ni_rows {
        ni_by_cik.entry(cik).or_default().push((end, val));
    }

    // Operating cash flow: 2 ANNUAL frames (newest first) → latest per CIK.
    let ocf_frames = sec_edgar::recent_annual_frames(today_nd, 2, 120);
    let ocf_rows = collect_frames(&sec_http, &sec_rl, &policy, "NetCashProvidedByUsedInOperatingActivities", &ocf_frames).await;
    let mut ocf_by_cik: HashMap<String, (String, f64)> = HashMap::new();
    for (cik, end, val) in ocf_rows {
        ocf_by_cik.entry(cik).or_insert((end, val)); // first seen = newest frame
    }

    // Cash: 2 instant frames, primary concept then fallback, latest per CIK.
    let cash_frames = sec_edgar::recent_quarter_frames(today_nd, 2, 50, true);
    let mut cash_by_cik: HashMap<String, (String, f64)> = HashMap::new();
    for concept in ["CashAndCashEquivalentsAtCarryingValue", "CashCashEquivalentsRestrictedCashAndRestrictedCashEquivalents"] {
        let rows = collect_frames(&sec_http, &sec_rl, &policy, concept, &cash_frames).await;
        for (cik, end, val) in rows {
            cash_by_cik.entry(cik).or_insert((end, val));
        }
    }

    let mut fin_written = 0usize;
    {
        let conn = db.lock().unwrap();
        let _ = conn.execute_batch("BEGIN");
        let mut ciks: std::collections::HashSet<&String> = std::collections::HashSet::new();
        ciks.extend(ni_by_cik.keys());
        ciks.extend(ocf_by_cik.keys());
        ciks.extend(cash_by_cik.keys());
        for cik in ciks {
            let Some(tickers) = cik_to_tickers.get(cik) else { continue };
            let fh = build_financial_health(ni_by_cik.get(cik), ocf_by_cik.get(cik), cash_by_cik.get(cik));
            if fh.is_empty() {
                continue;
            }
            for t in tickers {
                if in_universe(t)
                    && repo::upsert_financials(&conn, t, &fh, "SEC frames (bulk)", &now_ts).is_ok()
                {
                    fin_written += 1;
                }
            }
        }
        let _ = conn.execute_batch("COMMIT");
    }

    Ok((snapshots, fin_written))
}

// ─── One ticker ────────────────────────────────────────────────────────────────

/// Collect + store every section for one ticker. Returns true when no section hit
/// a genuine (non-NotFound, non-MissingKey) error.
async fn refresh_one(
    db: &Arc<Mutex<rusqlite::Connection>>,
    p: &Providers,
    keys: &Keys,
    cik_map: &HashMap<String, String>,
    symbol: &str,
) -> bool {
    let now = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let mut errors: HashMap<String, String> = HashMap::new();

    // Resolve CIK (cached → ticker map). SEC sections need it; Massive doesn't.
    let cik: Option<String> = resolve_cik(db, cik_map, symbol);

    // ── Section 1: short interest (Massive) ──────────────────────────────────
    if let Some(key) = &keys.massive {
        match massive_si::fetch_short_interest(&p.massive_http, &p.massive_rl, &p.policy, key, symbol)
            .await
        {
            Ok(si) => with_db(db, |c| { let _ = repo::upsert_short_interest(c, symbol, &si, "Massive", &now); }),
            Err(E::NotFound) | Err(E::MissingKey) => {}
            Err(e) => { errors.insert("short_interest".into(), e.to_string()); }
        }
    }

    // ── Section 2: financial health (SEC Company Facts → FMP fallback) ────────
    let mut fin_done = false;
    if let Some(cik) = &cik {
        match sec_edgar::fetch_financials(&p.sec_http, &p.sec_rl, &p.policy, cik).await {
            Ok(fh) => {
                with_db(db, |c| { let _ = repo::upsert_financials(c, symbol, &fh, "SEC Company Facts", &now); });
                fin_done = true;
            }
            Err(E::NotFound) => {}
            Err(e) => { errors.insert("financials".into(), e.to_string()); }
        }
    }
    if !fin_done {
        if let Some(key) = &keys.fmp {
            match fmp_fin::fetch_financials(&p.fmp_http, &p.fmp_rl, &p.policy, key, symbol).await {
                Ok(fh) => {
                    with_db(db, |c| { let _ = repo::upsert_financials(c, symbol, &fh, "FMP", &now); });
                    errors.remove("financials"); // FMP recovered it
                }
                Err(E::NotFound) | Err(E::MissingKey) => {}
                Err(e) => { errors.entry("financials".into()).or_insert_with(|| e.to_string()); }
            }
        }
    }

    // ── Section 3: dilution / S-3 filings (SEC EDGAR) ─────────────────────────
    if let Some(cik) = &cik {
        match sec_edgar::fetch_recent_filings(&p.sec_http, &p.sec_rl, &p.policy, symbol, cik, FILINGS_CAP)
            .await
        {
            Ok(filings) => {
                with_db(db, |c| {
                    for f in &filings {
                        let _ = repo::upsert_filing(c, f);
                    }
                });
                let dil = sec_edgar::summarize_dilution(
                    &p.sec_http, &p.sec_rl, &p.policy, &filings, SCAN_LATEST_DILUTION_DOC,
                )
                .await;
                if !dil.is_empty() {
                    with_db(db, |c| { let _ = repo::upsert_dilution(c, symbol, &dil, "SEC EDGAR", &now); });
                }
            }
            Err(E::NotFound) => {}
            Err(e) => { errors.insert("dilution".into(), e.to_string()); }
        }
    }

    // ── Section 4: ownership — >5% holders via 13D/13G (SEC EDGAR) ────────────
    // Disabled (see ENABLE_HOLDERS_FTS): the efts endpoint 500s on this query.
    if ENABLE_HOLDERS_FTS {
        if let Some(cik) = &cik {
            match sec_edgar::fetch_holders(&p.sec_http, &p.sec_rl, &p.policy, cik, HOLDERS_CAP).await {
                Ok(holders) if !holders.is_empty() => {
                    let own = model::OwnershipInfo { holders_5pct: holders, ..Default::default() };
                    with_db(db, |c| { let _ = repo::upsert_ownership(c, symbol, &own, "SEC EDGAR", &now); });
                }
                Ok(_) | Err(E::NotFound) => {}
                Err(e) => { errors.insert("ownership".into(), e.to_string()); }
            }
        }
    }

    // ── Overall marker + per-section errors ───────────────────────────────────
    let err_json = if errors.is_empty() {
        None
    } else {
        serde_json::to_string(&errors).ok()
    };
    with_db(db, |c| { let _ = repo::touch(c, symbol, &now, err_json.as_deref()); });
    errors.is_empty()
}

/// Resolve a ticker's CIK from the cache, else from the ticker→CIK map (persisting
/// it for next time). DB access is short and await-free.
fn resolve_cik(
    db: &Arc<Mutex<rusqlite::Connection>>,
    cik_map: &HashMap<String, String>,
    symbol: &str,
) -> Option<String> {
    let cached = {
        let conn = db.lock().unwrap();
        repo::get_cik(&conn, symbol)
    };
    if let Some(c) = cached.filter(|s| !s.is_empty()) {
        return Some(c);
    }
    let resolved = cik_map.get(&symbol.to_uppercase())?.clone();
    let conn = db.lock().unwrap();
    let _ = repo::set_cik(&conn, symbol, &resolved);
    Some(resolved)
}

// ─── Helpers ───────────────────────────────────────────────────────────────────

/// Run a closure with the SQLite connection locked. Never call this with an
/// `.await` inside the closure (the mutex must not be held across await points).
fn with_db<F: FnOnce(&rusqlite::Connection)>(db: &Arc<Mutex<rusqlite::Connection>>, f: F) {
    let conn = db.lock().unwrap();
    f(&conn);
}

fn log(db: &Arc<Mutex<rusqlite::Connection>>, level: &str, msg: &str) {
    if let Ok(conn) = db.lock() {
        let _ = insert_log(&conn, level, msg);
    }
    eprintln!("[tagdash] {msg}");
}

/// Read-only: the full intel record for a symbol (used by the read command).
pub fn get_company_intel(
    db: &Arc<Mutex<rusqlite::Connection>>,
    symbol: &str,
) -> Option<CompanyIntel> {
    let conn = db.lock().unwrap();
    repo::get_intel(&conn, symbol).ok().flatten()
}

/// Read-only: a bounded EXTRACT of the tickers data table (universe + all
/// enrichments). Empty `query` → most recently collected rows; otherwise → tickers
/// matching the query (symbol prefix or name contains). The DB join is done in SQL;
/// the RAM-only live news counts are merged in here. No network — purely a read of
/// what's already collected. Kept lightweight on purpose (shipping the whole
/// universe at once crashed the UI).
pub fn tickers_table(
    db: &Arc<Mutex<rusqlite::Connection>>,
    market: &Arc<RwLock<crate::market_state::MarketState>>,
    query: &str,
    limit: u32,
) -> Vec<TickerTableRow> {
    let mut rows = {
        let conn = db.lock().unwrap();
        repo::tickers_overview(&conn, query, limit).unwrap_or_default()
    };
    let news_counts = market.read().unwrap().all_news_counts();
    if !news_counts.is_empty() {
        for row in &mut rows {
            if let Some(n) = news_counts.get(&row.symbol) {
                row.news_count = *n as i64;
            }
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn financial_health_from_frames() {
        // 4 quarterly net incomes (two negative), one annual OCF, one cash instant.
        let ni = vec![
            ("2025-03-31".to_string(), -1_000.0),
            ("2025-06-30".to_string(), -500.0),
            ("2025-09-30".to_string(), 200.0),
            ("2025-12-31".to_string(), 300.0),
        ];
        let ocf = ("2025-12-31".to_string(), -4_000.0);
        let cash = ("2025-12-31".to_string(), 9_000.0);
        let fh = build_financial_health(Some(&ni), Some(&ocf), Some(&cash));
        assert_eq!(fh.net_income_last_q, Some(300.0));
        assert_eq!(fh.net_income_ttm, Some(-1_000.0)); // -1000-500+200+300
        assert_eq!(fh.negative_quarters_last4, Some(2));
        assert_eq!(fh.operating_cash_flow_ttm, Some(-4_000.0));
        assert_eq!(fh.cash_and_equivalents, Some(9_000.0));
        assert_eq!(fh.period_end.as_deref(), Some("2025-12-31"));
    }

    #[test]
    fn financial_health_partial_is_not_empty() {
        // Only cash known → still a usable (non-empty) record; TTM stays None (<4 q).
        let fh = build_financial_health(None, None, Some(&("2025-09-30".into(), 5_000.0)));
        assert!(!fh.is_empty());
        assert_eq!(fh.net_income_ttm, None);
        assert_eq!(fh.cash_and_equivalents, Some(5_000.0));
        // Nothing at all → empty.
        assert!(build_financial_health(None, None, None).is_empty());
    }
}
