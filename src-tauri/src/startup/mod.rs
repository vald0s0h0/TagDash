// Startup pipeline. Runs once at app launch (or on-demand via command).
// Updates a shared StartupState that the frontend polls via get_startup_status.
// Never holds the DB mutex across an await point.

use std::sync::{Arc, Mutex, RwLock};
use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::config::{AppConfig, secrets::Secrets};
use crate::local_db::{
    cache_repository, company_meta_repository, insert_log, universe_repository, UniverseAsset,
};

// ─── Public types (cross the Tauri bridge) ───────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    Running,
    Success,
    Warning,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartupStep {
    pub id: String,
    pub label: String,
    pub status: StepStatus,
    pub detail: Option<String>,
}

impl StartupStep {
    fn new(id: &str, label: &str) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            status: StepStatus::Pending,
            detail: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UniverseStats {
    pub cache_symbols: usize,
    pub alpaca_active: usize,
    pub with_float: usize,
    /// Total streamable US-stock count (all tradable equities).
    pub final_universe: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartupState {
    pub steps: Vec<StartupStep>,
    pub stats: UniverseStats,
    pub mock_mode: bool,
    pub warnings: Vec<String>,
    pub completed: bool,
}

impl Default for StartupState {
    fn default() -> Self {
        Self {
            steps: default_steps(),
            stats: UniverseStats::default(),
            mock_mode: false,
            warnings: vec![],
            completed: false,
        }
    }
}

/// A symbol retained in the final streamable universe.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamableSymbol {
    pub symbol: String,
    pub exchange: Option<String>,
    pub tradable: bool,
    pub shortable: bool,
    pub float_shares: Option<i64>,
    pub market_cap: Option<i64>,
    pub avg_volume: Option<i64>,
    /// Country of origin of the business (sec-api.io), not the listing venue.
    pub country: Option<String>,
    /// English industry name (SEC SIC classification).
    pub industry: Option<String>,
}

fn default_steps() -> Vec<StartupStep> {
    vec![
        StartupStep::new("load_config",      "Load local config"),
        StartupStep::new("load_strategies",  "Load compiled strategies"),
        StartupStep::new("load_cache",       "Load universe from cache"),
        StartupStep::new("fetch_alpaca",     "Fetch Alpaca assets"),
        StartupStep::new("fetch_massive",    "Fetch Massive float data"),
        StartupStep::new("fetch_sec",        "Fetch SEC company data (country · industry)"),
        StartupStep::new("load_daily",       "Load daily / historical data (250d)"),
        StartupStep::new("compute_universe", "Persist float & average volume"),
        StartupStep::new("compute_scores",   "Compute mean-reversion scores (PR · Bollinger)"),
        StartupStep::new("build_universe",   "Finalize universe (all US stocks)"),
        StartupStep::new("ready",            "Ready for WebSocket"),
    ]
}

// ─── State helpers ────────────────────────────────────────────────────────────

fn set_step(
    state: &Arc<RwLock<StartupState>>,
    id: &str,
    status: StepStatus,
    detail: Option<&str>,
) {
    let mut s = state.write().unwrap();
    if let Some(step) = s.steps.iter_mut().find(|st| st.id == id) {
        step.status = status;
        step.detail = detail.map(String::from);
    }
}

fn push_warning(state: &Arc<RwLock<StartupState>>, msg: &str) {
    state.write().unwrap().warnings.push(msg.into());
}

// ─── Pipeline ─────────────────────────────────────────────────────────────────

pub async fn run_pipeline(
    db: Arc<Mutex<rusqlite::Connection>>,
    _config: Arc<RwLock<AppConfig>>,
    secrets: Arc<RwLock<Secrets>>,
    state: Arc<RwLock<StartupState>>,
) {
    let sec = secrets.read().unwrap().clone();
    let key_set = |o: &Option<String>| o.as_deref().map(|s| !s.is_empty()).unwrap_or(false);
    let mock_alpaca  = !(key_set(&sec.alpaca_key) && key_set(&sec.alpaca_secret));
    let has_massive  = key_set(&sec.massive_api_key);
    let has_fmp      = key_set(&sec.fmp_api_key); // legacy fallback for floats
    let mock_float   = !(has_massive || has_fmp);
    let mock_sec     = !key_set(&sec.sec_api_key);

    {
        state.write().unwrap().mock_mode = mock_alpaca || mock_float || mock_sec;
    }

    // ── Step 1: load_config ───────────────────────────────────────────────────
    set_step(&state, "load_config", StepStatus::Running, None);
    set_step(&state, "load_config", StepStatus::Success, Some("config loaded from tagdash.toml"));
    let _ = log(&db, "info", "startup: config loaded");

    // ── Step 2: load_strategies ───────────────────────────────────────────────
    set_step(&state, "load_strategies", StepStatus::Running, None);
    // Strategies are compiled in (see `strategies::registry`); no dynamic loading.
    let n_strategies = crate::strategies::registry::all_strategies().len();
    set_step(&state, "load_strategies", StepStatus::Success,
        Some(&format!("{n_strategies} compiled strategies")));

    // ── Step 3: load_cache ────────────────────────────────────────────────────
    set_step(&state, "load_cache", StepStatus::Running, None);
    let cache_count = {
        let db_guard = db.lock().unwrap();
        universe_repository::count(&db_guard).unwrap_or(0)
    };
    {
        state.write().unwrap().stats.cache_symbols = cache_count as usize;
    }
    set_step(&state, "load_cache", StepStatus::Success, Some(&format!("{cache_count} symbols in cache")));
    let _ = log(&db, "info", &format!("startup: cache has {cache_count} symbols"));

    // ── Step 4: fetch_alpaca ──────────────────────────────────────────────────
    set_step(&state, "fetch_alpaca", StepStatus::Running, None);
    let alpaca_assets = if mock_alpaca {
        push_warning(&state, "Alpaca keys not configured — using mock assets");
        crate::alpaca::assets::mock_assets()
    } else {
        let key = sec.alpaca_key.as_deref().unwrap_or_default();
        let secret = sec.alpaca_secret.as_deref().unwrap_or_default();
        match crate::alpaca::assets::fetch_assets(key, secret).await {
            Ok(assets) => assets,
            Err(e) => {
                push_warning(&state, &format!("Alpaca fetch failed: {e} — falling back to cache"));
                set_step(&state, "fetch_alpaca", StepStatus::Warning, Some(&format!("fetch error: {e}")));
                let db_guard = db.lock().unwrap();
                universe_repository::get_all(&db_guard).unwrap_or_default()
                    .into_iter()
                    .map(|a| crate::alpaca::assets::AlpacaAsset {
                        symbol: a.symbol,
                        name: a.name,
                        exchange: a.exchange.unwrap_or_default(),
                        tradable: a.tradable,
                        shortable: a.shortable,
                        status: "active".into(),
                    })
                    .collect()
            }
        }
    };
    let alpaca_active: Vec<_> = alpaca_assets.iter().filter(|a| a.tradable).collect();
    {
        state.write().unwrap().stats.alpaca_active = alpaca_active.len();
    }
    // Persist alpaca assets to SQLite
    {
        let now = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let db_guard = db.lock().unwrap();
        for a in &alpaca_assets {
            let asset = UniverseAsset {
                symbol: a.symbol.clone(),
                name: a.name.clone(),
                exchange: Some(a.exchange.clone()),
                tradable: a.tradable,
                shortable: a.shortable,
                float_shares: None,
                market_cap: None,
                avg_volume: None,
                updated_at: now.clone(),
            };
            let _ = universe_repository::upsert(&db_guard, &asset);
        }
    }
    if matches!(state.read().unwrap().steps.iter().find(|s| s.id == "fetch_alpaca").map(|s| &s.status), Some(StepStatus::Running)) {
        set_step(&state, "fetch_alpaca", StepStatus::Success, Some(&format!("{} tradable assets", alpaca_active.len())));
    }
    let _ = log(&db, "info", &format!("startup: {} Alpaca tradable assets", alpaca_active.len()));

    // ── Step 5: fetch_massive (floats, at most once per calendar day) ─────────
    // Massive is the active float provider; FMP is kept as a legacy fallback.
    // Free tier (~1 req/13 s) makes this expensive, so we reuse today's cache.
    //
    // Freshness is tracked by a dedicated `floats_fetch_date` marker, NOT by the
    // fundamentals_cache timestamp: the old FMP path stamped that table too, so
    // a tiny FMP dump from earlier today would otherwise mask the full Massive
    // fetch (it once silently kept the float count at ~160).
    set_step(&state, "fetch_massive", StepStatus::Running, None);
    // ET date (DST-aware) for the once-per-day markers, so "a new day" flips at
    // Eastern midnight — consistent with the screener dismissals + the engines.
    let today = crate::time::et_date(Utc::now());
    const FLOATS_DATE_KEY: &str = "floats_fetch_date";
    let floats_fresh_today = {
        let db_guard = db.lock().unwrap();
        cache_repository::get_app_meta(&db_guard, FLOATS_DATE_KEY).as_deref() == Some(today.as_str())
    };
    let mut float_fetch_ok = false;
    let cached_floats = || -> Vec<crate::massive::MassiveFloat> {
        let db_guard = db.lock().unwrap();
        cache_repository::all_fundamentals(&db_guard)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|f| {
                Some(crate::massive::MassiveFloat {
                    symbol: f.symbol,
                    float_shares: f.float_shares? as f64,
                    outstanding_shares: f.outstanding_shares.unwrap_or(0) as f64,
                    free_float: f.free_float.unwrap_or(0.0),
                })
            })
            .collect()
    };
    let floats: Vec<crate::massive::MassiveFloat> = if floats_fresh_today {
        let cached = cached_floats();
        set_step(&state, "fetch_massive", StepStatus::Success, Some(&format!("{} float records (cached today)", cached.len())));
        cached
    } else if mock_float {
        push_warning(&state, "No float provider key (Massive/FMP) — using mock float data");
        crate::massive::mock_float_all()
    } else if has_massive {
        let key = sec.massive_api_key.as_deref().unwrap_or_default();
        set_step(&state, "fetch_massive", StepStatus::Running, Some("fetching bulk float (rate-limited ~1 req/13s)…"));
        match crate::massive::fetch_float_all(key).await {
            Ok(data) => { float_fetch_ok = true; data }
            Err(e) => {
                push_warning(&state, &format!("Massive unavailable: {e} — using cached floats"));
                set_step(&state, "fetch_massive", StepStatus::Warning, Some(&format!("fetch error: {e}")));
                cached_floats()
            }
        }
    } else {
        // FMP legacy fallback.
        let key = sec.fmp_api_key.as_deref().unwrap_or_default();
        match crate::fmp::fetch_shares_float_all(key).await {
            Ok(data) => {
                float_fetch_ok = true;
                data.into_iter()
                    .map(|f| crate::massive::MassiveFloat {
                        symbol: f.symbol,
                        float_shares: f.float_shares,
                        outstanding_shares: f.outstanding_shares,
                        free_float: f.free_float,
                    })
                    .collect()
            }
            Err(e) => {
                push_warning(&state, &format!("FMP unavailable: {e} — using cached floats"));
                set_step(&state, "fetch_massive", StepStatus::Warning, Some(&format!("fetch error: {e}")));
                cached_floats()
            }
        }
    };
    // Build float lookup map.
    let float_map: std::collections::HashMap<String, &crate::massive::MassiveFloat> =
        floats.iter().map(|f| (f.symbol.clone(), f)).collect();
    let with_float = alpaca_active.iter().filter(|a| float_map.contains_key(&a.symbol)).count();
    {
        state.write().unwrap().stats.with_float = with_float;
    }
    // Persist floats to fundamentals_cache (skip when reusing today's cache).
    // Only stamp the once-per-day marker on a *successful* fetch, so a failed
    // fetch that fell back to cache will retry on the next launch.
    if !floats_fresh_today {
        let now = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let db_guard = db.lock().unwrap();
        for f in &floats {
            let fund = cache_repository::FundamentalCache {
                symbol: f.symbol.clone(),
                float_shares: Some(f.float_shares as i64),
                outstanding_shares: Some(f.outstanding_shares as i64),
                free_float: Some(f.free_float),
                prev_close: None,
                avg_volume: None,
                atr: None,
                updated_at: now.clone(),
            };
            let _ = cache_repository::upsert_fundamental(&db_guard, &fund);
        }
        if float_fetch_ok {
            let _ = cache_repository::set_app_meta(&db_guard, FLOATS_DATE_KEY, &today);
        }
    }
    if matches!(state.read().unwrap().steps.iter().find(|s| s.id == "fetch_massive").map(|s| &s.status), Some(StepStatus::Running)) {
        set_step(&state, "fetch_massive", StepStatus::Success, Some(&format!("{} float records ({with_float} in universe)", floats.len())));
    }
    let _ = log(&db, "info", &format!("startup: {} float records (Massive/FMP)", floats.len()));

    // ── Step 6: fetch_sec (country of origin + SIC industry, once per day) ────
    set_step(&state, "fetch_sec", StepStatus::Running, None);
    let sec_fresh_today = {
        let db_guard = db.lock().unwrap();
        company_meta_repository::last_date(&db_guard)
            .map(|d| d.starts_with(&today))
            .unwrap_or(false)
    };
    if sec_fresh_today {
        let n = { let db_guard = db.lock().unwrap(); company_meta_repository::count(&db_guard).unwrap_or(0) };
        set_step(&state, "fetch_sec", StepStatus::Success, Some(&format!("{n} companies (cached today)")));
    } else if mock_sec {
        push_warning(&state, "sec-api key not configured — using mock company data");
        let companies = crate::sec_api::mock_companies();
        persist_company_meta(&db, companies.values());
        set_step(&state, "fetch_sec", StepStatus::Success, Some(&format!("{} companies (mock)", companies.len())));
    } else {
        let token = sec.sec_api_key.as_deref().unwrap_or_default();
        match crate::sec_api::fetch_all(token).await {
            Ok(companies) => {
                let with_country = companies.values().filter(|c| c.country.is_some()).count();
                persist_company_meta(&db, companies.values());
                set_step(&state, "fetch_sec", StepStatus::Success,
                    Some(&format!("{} companies · {with_country} with country", companies.len())));
                let _ = log(&db, "info", &format!("startup: {} SEC company records", companies.len()));
            }
            Err(e) => {
                let n = { let db_guard = db.lock().unwrap(); company_meta_repository::count(&db_guard).unwrap_or(0) };
                push_warning(&state, &format!("sec-api unavailable: {e} — using cached company data"));
                set_step(&state, "fetch_sec", StepStatus::Warning, Some(&format!("fetch error — {n} cached")));
            }
        }
    }

    // ── Step 7: load_daily (incremental: 250d first run, missing days after) ──
    set_step(&state, "load_daily", StepStatus::Running, None);
    let symbols_for_bars: Vec<String> = alpaca_active.iter().map(|a| a.symbol.clone()).collect();
    // Target ~250 calendar days of history so the mean-reversion scoring engine
    // (Panic Mean Reversion) has enough self-relative history (well above its
    // MIN_HISTORY_DAYS floor) without storing years of bars. The daily cache is
    // never pruned, so history still accumulates forward over time.
    //
    // Depth-aware backfill: we fetch the FULL 250-day window when the cache is
    // empty OR when its OLDEST bar is more recent than the desired start (a
    // shallow cache seeded by an earlier short-window build — which never got
    // backfilled because the old logic only ever extended forward). Upserts are
    // idempotent, so re-fetching the overlapping recent days is harmless; this
    // deep fetch happens once, then later runs just top up the missing new days
    // from the last cached date.
    let desired_start = (Utc::now() - chrono::Duration::days(250)).format("%Y-%m-%d").to_string();
    // Backfill is triggered when our OLDEST bar is more recent than this threshold
    // (~2 weeks inside the 250-day target). The slack is essential: Alpaca's first
    // available bar lands a few days after our requested calendar start (weekends /
    // first trading day), so comparing against `desired_start` directly would mark
    // the cache "shallow" forever and re-fetch the full window on every launch.
    let backfill_threshold = (Utc::now() - chrono::Duration::days(235)).format("%Y-%m-%d").to_string();
    let (latest_cached, earliest_cached) = {
        let db_guard = db.lock().unwrap();
        (
            cache_repository::latest_bar_date(&db_guard).unwrap_or(None),
            cache_repository::earliest_bar_date(&db_guard).unwrap_or(None),
        )
    };
    let need_backfill = match earliest_cached.as_deref() {
        Some(d) if !d.is_empty() => d > backfill_threshold.as_str(), // shallow → backfill
        _ => true,                                                   // empty → backfill
    };
    let start_date = if need_backfill {
        desired_start.clone()
    } else {
        latest_cached
            .as_deref()
            .filter(|d| !d.is_empty())
            .map(String::from)
            .unwrap_or_else(|| desired_start.clone())
    };
    let incremental = !need_backfill;
    let daily_bars = if mock_alpaca {
        crate::alpaca::bars::mock_daily_bars(&symbols_for_bars)
    } else {
        let key = sec.alpaca_key.as_deref().unwrap_or_default();
        let secret = sec.alpaca_secret.as_deref().unwrap_or_default();
        match crate::alpaca::bars::fetch_daily_bars_since(key, secret, &symbols_for_bars, &start_date).await {
            Ok(bars) => bars,
            Err(e) => {
                // Real keys configured but the fetch failed: keep the existing
                // cache untouched rather than injecting synthetic mock bars over
                // real symbols (which would corrupt the daily history and make the
                // scorer rank garbage). Nothing new is committed this run.
                push_warning(&state, &format!("Alpaca bars fetch failed: {e} — keeping cached bars"));
                std::collections::HashMap::new()
            }
        }
    };
    // Persist freshly fetched bars, then derive avg_volume / prev_close from the
    // DB so they stay correct for incremental loads. The daily cache is NOT
    // pruned: history is kept beyond the 250-day window to progressively enrich
    // the DB (only the queries that need a recent window limit themselves).
    let (volume_map, prev_close_map) = {
        let now = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let db_guard = db.lock().unwrap();
        // Wrap every write in ONE transaction. Without it each upsert and each
        // avg_volume UPDATE is its own auto-commit (an fsync in WAL), and the
        // UPDATE loop runs over the WHOLE universe (~thousands of symbols) on
        // every startup — so this step was thousands of fsyncs even on an
        // incremental run that fetched almost no new bars. One commit instead.
        let _ = db_guard.execute_batch("BEGIN");
        for (_symbol, bars) in &daily_bars {
            for bar in bars {
                let db_bar = cache_repository::DailyBar {
                    symbol: bar.symbol.clone(),
                    date: bar.date.clone(),
                    open: bar.open,
                    high: bar.high,
                    low: bar.low,
                    close: bar.close,
                    volume: bar.volume,
                    updated_at: now.clone(),
                };
                let _ = cache_repository::upsert_daily_bar(&db_guard, &db_bar);
            }
        }
        // Average daily volume over the last 20 trading days (reads see the
        // freshly-upserted rows within the open transaction).
        let volume_map: std::collections::HashMap<String, i64> =
            cache_repository::avg_volumes(&db_guard, 20).unwrap_or_default().into_iter().collect();
        let prev_close_map: std::collections::HashMap<String, f64> =
            cache_repository::latest_closes(&db_guard).unwrap_or_default().into_iter().collect();
        // Update avg_volume in universe_assets.
        for (sym, avg_vol) in &volume_map {
            let _ = db_guard.execute(
                "UPDATE universe_assets SET avg_volume=?1 WHERE symbol=?2",
                rusqlite::params![avg_vol, sym],
            );
        }
        let _ = db_guard.execute_batch("COMMIT");
        (volume_map, prev_close_map)
    };
    let covered = {
        let db_guard = db.lock().unwrap();
        cache_repository::symbols_with_bars(&db_guard).unwrap_or(0)
    };
    let mode = if incremental { format!("incremental since {start_date}") } else { format!("250d backfill since {start_date}") };
    set_step(&state, "load_daily", StepStatus::Success,
        Some(&format!("{covered} symbols with bar data · {} updated ({mode})", daily_bars.len())));
    let _ = log(&db, "info", &format!("startup: daily bars — {covered} symbols cached, {} updated ({mode})", daily_bars.len()));

    // Recompute the close-to-close % change over 1..6 trading days for every
    // symbol, now that the daily cache reflects the latest bars. Stored in
    // fundamentals_cache (change_1d_pct … change_6d_pct); gaps are included since
    // the calculation is close-to-close off the previous day's close.
    {
        let db_guard = db.lock().unwrap();
        match cache_repository::recompute_multiday_changes(&db_guard) {
            Ok(n) => { drop(db_guard); let _ = log(&db, "info", &format!("startup: multi-day price changes recomputed for {n} symbols")); }
            Err(e) => { drop(db_guard); let _ = log(&db, "warn", &format!("startup: multi-day change recompute failed: {e}")); }
        }
    }

    // ── Step 8: persist float / market cap / avg volume ───────────────────────
    // There are no more "universes": we stream the whole US market (wildcard) and
    // each strategy does its own filtering (e.g. micro_pullback gates on float).
    // This step just persists each tradable asset's float, derived
    // market cap and average volume into `universe_assets` so the scanner and
    // strategies can read them.
    set_step(&state, "compute_universe", StepStatus::Running, None);
    {
        let now = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let db_guard = db.lock().unwrap();
        for a in &alpaca_active {
            let fmpf       = float_map.get(&a.symbol);
            let float_shares = fmpf.map(|f| f.float_shares as i64);
            let prev_close = prev_close_map.get(&a.symbol).copied();
            // Market cap derived as outstanding shares × last close (no extra API).
            let market_cap = fmpf
                .filter(|f| f.outstanding_shares > 0.0)
                .and_then(|f| prev_close.map(|pc| (f.outstanding_shares * pc) as i64));
            let asset = UniverseAsset {
                symbol:       a.symbol.clone(),
                name:         a.name.clone(),
                exchange:     Some(a.exchange.clone()),
                tradable:     a.tradable,
                shortable:    a.shortable,
                float_shares,
                market_cap,
                avg_volume:   volume_map.get(&a.symbol).copied(),
                updated_at:   now.clone(),
            };
            let _ = universe_repository::upsert(&db_guard, &asset);
        }
    }
    let us_stocks_count = alpaca_active.len();
    {
        let mut s = state.write().unwrap();
        s.stats.final_universe = us_stocks_count;
    }
    set_step(&state, "compute_universe", StepStatus::Success,
        Some(&format!("{us_stocks_count} US stocks · {with_float} with float")));
    let _ = log(&db, "info", &format!("startup: persisted {us_stocks_count} US stocks ({with_float} with float)"));

    // ── Step 8b: compute mean-reversion scores (once per calendar day) ────────
    // Percent-rank momentum + self-relative Bollinger event score for the whole
    // universe, persisted to `mean_reversion_scores`. Drives the Panic Mean
    // Reversion pre-open screener. Heavy (reads ~3y of closes per symbol), so it
    // runs only on the first launch of the day — gated by a date marker.
    set_step(&state, "compute_scores", StepStatus::Running, None);
    // Versioned key: bump the suffix whenever the score schema/content changes so
    // a stale same-day marker can't suppress the first recompute on the new shape
    // (v2 added prev_volume; v3 added the hard multi-day-runner pre-filter — the v2
    // table held the WHOLE unfiltered universe). Old markers are simply ignored.
    const SCORES_DATE_KEY: &str = "scores_compute_date_v3";
    // "Fresh today" requires BOTH the date marker AND a non-empty table. The
    // table check is what makes this self-healing: if a prior run stamped today's
    // date but produced no rows (e.g. an earlier build with a stricter history
    // requirement), we still recompute instead of staying empty all day.
    let scores_fresh_today = {
        let db_guard = db.lock().unwrap();
        let marked = cache_repository::get_app_meta(&db_guard, SCORES_DATE_KEY).as_deref()
            == Some(today.as_str());
        let have_rows = crate::local_db::scoring_repository::count(&db_guard).unwrap_or(0) > 0;
        marked && have_rows
    };
    if scores_fresh_today {
        let n = { let db_guard = db.lock().unwrap(); crate::local_db::scoring_repository::count(&db_guard).unwrap_or(0) };
        set_step(&state, "compute_scores", StepStatus::Success, Some(&format!("{n} scored (cached today)")));
    } else {
        match crate::scoring::compute_and_store(&db) {
            Ok(n) => {
                // Only mark "done for today" when we actually scored something, so
                // a launch with no usable daily history yet retries next time
                // instead of being locked out for the day.
                if n > 0 {
                    let db_guard = db.lock().unwrap();
                    let _ = cache_repository::set_app_meta(&db_guard, SCORES_DATE_KEY, &today);
                    drop(db_guard);
                    set_step(&state, "compute_scores", StepStatus::Success, Some(&format!("{n} tickers scored")));
                    let _ = log(&db, "info", &format!("startup: mean-reversion scores computed for {n} tickers"));
                } else {
                    set_step(&state, "compute_scores", StepStatus::Warning, Some("0 tickers scored (no daily history yet — will retry next launch)"));
                }
            }
            Err(e) => {
                push_warning(&state, &format!("scoring failed: {e}"));
                set_step(&state, "compute_scores", StepStatus::Warning, Some(&format!("error: {e}")));
            }
        }
    }

    // ── Step 9: finalize ──────────────────────────────────────────────────────
    // Nothing more to build — the streamable set is the whole US market.
    set_step(&state, "build_universe", StepStatus::Success,
        Some(&format!("All US stocks streamable: {us_stocks_count}")));
    let _ = log(&db, "info", "startup: universe ready (all US stocks)");

    // ── Step 10: ready ────────────────────────────────────────────────────────
    set_step(&state, "ready", StepStatus::Success, Some("universe ready — starting Alpaca WebSocket"));
    let _ = log(&db, "info", "startup: pipeline complete");

    {
        let mut s = state.write().unwrap();
        s.completed = true;
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn log(db: &Arc<Mutex<rusqlite::Connection>>, level: &str, msg: &str) -> rusqlite::Result<()> {
    let conn = db.lock().unwrap();
    insert_log(&conn, level, msg)
}

/// Upsert resolved sec-api company metadata into the `company_meta` table.
fn persist_company_meta<'a>(
    db: &Arc<Mutex<rusqlite::Connection>>,
    companies: impl Iterator<Item = &'a crate::sec_api::SecCompany>,
) {
    let now = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let db_guard = db.lock().unwrap();
    for c in companies {
        let row = company_meta_repository::CompanyMeta {
            symbol: c.symbol.clone(),
            country: c.country.clone(),
            sic: c.sic.clone(),
            industry: c.industry.clone(),
            sector: c.sector.clone(),
            updated_at: now.clone(),
        };
        let _ = company_meta_repository::upsert(&db_guard, &row);
    }
}
