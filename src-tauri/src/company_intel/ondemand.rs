// On-demand "capacité à diluer" collector.
//
// The SEC dilution section (recent S-3 / 424B filings → has_recent_shelf, latest
// form/date, ATM/resale/warrants flags) can't be cheaply bulk-fetched market-wide
// (it lives in each issuer's own filing feed). It IS fast per ticker though — one
// `submissions` request (+ one optional document scan), ~0.2–0.5 s, free. So we
// collect it JUST-IN-TIME: when a ticker surfaces on the premarket scanners we drop
// its symbol into a channel; a single background worker drains the channel, refreshes
// that ticker's dilution section, and updates its `dilution_capacity_score`.
//
// This never blocks the scanner: `request()` is a non-blocking channel send, the work
// runs on its own task, and the SQLite mutex is only taken for short await-free
// critical sections. It's the interim until a real background worker exists.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use chrono::Utc;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

use crate::config::AppConfig;
use crate::local_db::{cache_repository, company_intel_repository as repo, insert_log};

use super::http::{Http, RetryPolicy};
use super::providers::sec_edgar;
use super::rate_limit::RateLimiter;

/// Max recent filings pulled per ticker (same cap as the batch job).
const FILINGS_CAP: usize = 40;
/// Scan the latest dilution document for ATM/resale/warrant keywords + amount.
const SCAN_LATEST_DILUTION_DOC: bool = true;
/// Skip re-collecting a ticker whose dilution section was refreshed within this many
/// hours (cross-session TTL; within a session an in-memory set also dedups).
const TTL_HOURS: i64 = 18;

/// Global request channel, set by `init`. `request()` is a no-op until then.
static SENDER: OnceLock<UnboundedSender<String>> = OnceLock::new();

/// Queue a ticker for on-demand dilution collection. Non-blocking; safe to call from
/// the scanner hot path. Deduplication + TTL gating happen in the worker.
pub fn request(symbol: &str) {
    if symbol.trim().is_empty() {
        return;
    }
    if let Some(tx) = SENDER.get() {
        let _ = tx.send(symbol.to_uppercase());
    }
}

/// Create the request channel + store the sender; returns the receiver to be driven
/// by `run_worker` on a runtime-managed task. Returns None if already initialised.
/// Kept separate from the spawn so this module stays Tauri-free — `lib.rs` spawns the
/// returned future on the Tauri/Tokio runtime.
pub fn take_channel() -> Option<UnboundedReceiver<String>> {
    let (tx, rx) = unbounded_channel::<String>();
    if SENDER.set(tx).is_err() {
        return None; // already initialised
    }
    Some(rx)
}

/// Drain the request channel forever: per surfaced ticker, refresh its SEC dilution
/// section + capacity score. Sequential (one ticker at a time) so it self-paces; the
/// SEC rate limiter adds the per-request budget. Owns the SEC HTTP client and a
/// lazily-fetched ticker→CIK map reused for the session.
pub async fn run_worker(
    db: Arc<Mutex<rusqlite::Connection>>,
    config: Arc<RwLock<AppConfig>>,
    mut rx: UnboundedReceiver<String>,
) {
    let cfg = { config.read().unwrap().company_intel.clone() };
    let http = Http::new("sec_edgar");
    let rl = RateLimiter::per_minute(cfg.sec_rpm);
    let policy = RetryPolicy::default();

    let mut seen: HashSet<String> = HashSet::new();
    let mut cik_map: Option<std::collections::HashMap<String, String>> = None;

    while let Some(symbol) = rx.recv().await {
        // Session-level dedup.
        if !seen.insert(symbol.clone()) {
            continue;
        }
        // Cross-session TTL: skip if recently refreshed.
        if dilution_fresh(&db, &symbol) {
            continue;
        }
        // Resolve CIK: DB cache → ticker map (fetched once, lazily).
        let cik = match resolve_cik(&db, &mut cik_map, &http, &rl, &policy, &symbol).await {
            Some(c) => c,
            None => continue, // not an SEC filer we can map
        };

        if let Err(e) = collect_one(&db, &http, &rl, &policy, &symbol, &cik).await {
            log(&db, "info", &format!("company_intel ondemand: {symbol} failed: {e}"));
        }
    }
}

/// True when the symbol's dilution section was refreshed within the TTL.
fn dilution_fresh(db: &Arc<Mutex<rusqlite::Connection>>, symbol: &str) -> bool {
    let cutoff = (Utc::now() - chrono::Duration::hours(TTL_HOURS))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    let conn = db.lock().unwrap();
    conn.query_row(
        "SELECT dilution_updated_at FROM company_intel WHERE symbol=?1",
        rusqlite::params![symbol],
        |r| r.get::<_, Option<String>>(0),
    )
    .ok()
    .flatten()
    .map(|ts| ts.as_str() >= cutoff.as_str())
    .unwrap_or(false)
}

/// Resolve a CIK from the DB cache, else from the (lazily fetched) ticker→CIK map.
async fn resolve_cik(
    db: &Arc<Mutex<rusqlite::Connection>>,
    cik_map: &mut Option<std::collections::HashMap<String, String>>,
    http: &Http,
    rl: &RateLimiter,
    policy: &RetryPolicy,
    symbol: &str,
) -> Option<String> {
    {
        let conn = db.lock().unwrap();
        if let Some(c) = repo::get_cik(&conn, symbol).filter(|s| !s.is_empty()) {
            return Some(c);
        }
    }
    if cik_map.is_none() {
        match sec_edgar::fetch_ticker_cik_map(http, rl, policy).await {
            Ok(m) => *cik_map = Some(m),
            Err(_) => return None,
        }
    }
    let resolved = cik_map.as_ref()?.get(&symbol.to_uppercase())?.clone();
    let conn = db.lock().unwrap();
    let _ = repo::set_cik(&conn, symbol, &resolved);
    Some(resolved)
}

/// Refresh one ticker's dilution section + its capacity score. The dilution row is
/// upserted EVEN WHEN no shelf/filing is found, so "collected, nothing" scores 0
/// (low capacity) rather than staying unknown.
async fn collect_one(
    db: &Arc<Mutex<rusqlite::Connection>>,
    http: &Http,
    rl: &RateLimiter,
    policy: &RetryPolicy,
    symbol: &str,
    cik: &str,
) -> Result<(), String> {
    let now = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let filings = sec_edgar::fetch_recent_filings(http, rl, policy, symbol, cik, FILINGS_CAP)
        .await
        .map_err(|e| e.to_string())?;
    {
        let conn = db.lock().unwrap();
        for f in &filings {
            let _ = repo::upsert_filing(&conn, f);
        }
    }
    let dil = sec_edgar::summarize_dilution(http, rl, policy, &filings, SCAN_LATEST_DILUTION_DOC).await;
    let today = crate::time::et_date(crate::time::now());
    {
        let conn = db.lock().unwrap();
        // Always upsert (even empty) so the section is marked collected → capacity 0.
        let _ = repo::upsert_dilution(&conn, symbol, &dil, "SEC EDGAR (on-demand)", &now);
        let _ = cache_repository::recompute_capacity_for_symbol(&conn, symbol, &today);
    }
    Ok(())
}

fn log(db: &Arc<Mutex<rusqlite::Connection>>, level: &str, msg: &str) {
    if let Ok(conn) = db.lock() {
        let _ = insert_log(&conn, level, msg);
    }
}
