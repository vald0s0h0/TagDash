// Startup pipeline. Runs once at app launch (or on-demand via command).
// Updates a shared StartupState that the frontend polls via get_startup_status.
// Never holds the DB mutex across an await point.

use std::sync::{Arc, Mutex, RwLock};
use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::config::{AppConfig, DataSourceConfig, secrets::Secrets};
use crate::local_db::{
    cache_repository, company_meta_repository, insert_log, universe_repository, UniverseAsset,
};

/// Once-per-ET-day freshness markers (app_meta). Stamped ONLY on a successful real
/// fetch — never on the mock / cached fallbacks — so a missing provider key never
/// masks a later real fetch the same day (see `step_floats` / `step_sec`).
const FLOATS_DATE_KEY: &str = "floats_fetch_date";
const SEC_DATE_KEY: &str = "sec_fetch_date";

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

impl StartupState {
    /// Initial state with the step list for the active data-source mode. The
    /// flat-files pipeline shows a different set of steps (no Alpaca) — see
    /// `default_steps_flat_files`.
    pub fn for_mode(ds: &DataSourceConfig) -> Self {
        Self {
            steps: if ds.is_flat_files() { default_steps_flat_files() } else { default_steps() },
            ..Self::default()
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
        StartupStep::new("compute_changes", "Compute multi-day price changes"),
        StartupStep::new("compute_metrics",  "Compute ATR · prev close · Pump&Dump score"),
        StartupStep::new("fetch_short_interest", "Fetch short interest (Massive bulk)"),
        StartupStep::new("fetch_splits",     "Fetch stock splits (corporate actions)"),
        StartupStep::new("fetch_dilution",   "Fetch dilution + financials (SEC XBRL) · scores"),
        StartupStep::new("compute_universe", "Persist float & average volume"),
        StartupStep::new("compute_risk_scores", "Score dilution capacity · need · short interest"),
        StartupStep::new("build_universe",   "Finalize universe (all US stocks)"),
        StartupStep::new("ready",            "Ready for WebSocket"),
    ]
}

/// Steps shown in flat-files mode. Alpaca is gone (no assets / live daily / live
/// splits); the universe + daily history come from disk, and FMP/Massive/SEC are
/// still used (online) to fill float / metadata gaps. Ends by parking the latest
/// downloaded day in Market Replay (see `lib.rs`).
fn default_steps_flat_files() -> Vec<StartupStep> {
    vec![
        StartupStep::new("load_config",      "Load local config"),
        StartupStep::new("load_strategies",  "Load compiled strategies"),
        StartupStep::new("load_cache",       "Load universe from cache"),
        StartupStep::new("load_flat_daily",  "Load daily history from flat files"),
        StartupStep::new("fetch_massive",    "Float data (flat-files gap-fill)"),
        StartupStep::new("fetch_sec",        "SEC company data (country · industry)"),
        StartupStep::new("compute_changes", "Compute multi-day price changes"),
        StartupStep::new("compute_metrics",  "Compute ATR · prev close · Pump&Dump score"),
        StartupStep::new("fetch_short_interest", "Fetch short interest (Massive bulk)"),
        StartupStep::new("fetch_dilution",   "Fetch dilution + financials (SEC XBRL) · scores"),
        StartupStep::new("compute_universe", "Persist float & average volume"),
        StartupStep::new("compute_risk_scores", "Score dilution capacity · need · short interest"),
        StartupStep::new("load_replay_day",  "Load latest flat-files day into Market Replay"),
        StartupStep::new("ready",            "Ready (offline replay)"),
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
    config: Arc<RwLock<AppConfig>>,
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

    // ── Step 5: fetch_massive (floats) ────────────────────────────────────────
    // Shared with the flat-files pipeline (see `step_floats`): cache-first, real
    // fetch when stale OR empty, marker stamped only on a real success so a missing
    // provider key never masks a later fetch the same day. ET date (DST-aware) for
    // the once-per-day markers, so "a new day" flips at Eastern midnight.
    let today = crate::time::et_date(Utc::now());
    // fetch_massive + fetch_sec run concurrently (independent providers).
    let (floats, _) = tokio::join!(
        step_floats(&db, &state, &sec, &today, mock_float, has_massive),
        step_sec(&db, &state, &sec, &today, mock_sec),
    );
    let float_map: std::collections::HashMap<String, &crate::massive::MassiveFloat> =
        floats.iter().map(|f| (f.symbol.clone(), f)).collect();
    let with_float = alpaca_active.iter().filter(|a| float_map.contains_key(&a.symbol)).count();
    {
        state.write().unwrap().stats.with_float = with_float;
    }
    // Append the in-universe count, but only when the step succeeded — don't clobber
    // a Warning from a failed fetch that fell back to cache.
    if matches!(
        state.read().unwrap().steps.iter().find(|s| s.id == "fetch_massive").map(|s| &s.status),
        Some(StepStatus::Success)
    ) {
        set_step(&state, "fetch_massive", StepStatus::Success,
            Some(&format!("{} float records ({with_float} in universe)", floats.len())));
    }
    let _ = log(&db, "info", &format!("startup: {} float records (Massive/FMP)", floats.len()));

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
    // One-time migration to split-adjusted bars. The cache used to store raw
    // (unadjusted) bars, where every past split shows up as a fake gap. The first
    // run on the new code force-fetches the full window with adjustment=split so
    // the whole cache is internally consistent; the `bars_adjustment` marker keeps
    // it from repeating. (Mock runs keep their synthetic bars untouched.)
    let split_adjusted = {
        let db_guard = db.lock().unwrap();
        cache_repository::get_app_meta(&db_guard, "bars_adjustment").as_deref() == Some("split")
    };
    let force_readjust = !split_adjusted && !mock_alpaca;
    let need_backfill = force_readjust || match earliest_cached.as_deref() {
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
    let mut daily_fetch_ok = true;
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
                daily_fetch_ok = false;
                push_warning(&state, &format!("Alpaca bars fetch failed: {e} — keeping cached bars"));
                std::collections::HashMap::new()
            }
        }
    };

    // ── Split reconciliation ──────────────────────────────────────────────────
    // adjustment=split rescales the whole series to the LATEST split factor, so a
    // split that goes ex after our last cached bar leaves the older cached bars at
    // the old scale (a fake gap). On incremental runs, find symbols that split
    // since the last check and refetch their full window so the series stays
    // consistent; these override the incremental rows below. Backfill runs already
    // fetched a consistent full series, so they skip this.
    let readjusted_bars = if mock_alpaca || need_backfill || !daily_fetch_ok {
        std::collections::HashMap::new()
    } else {
        let key = sec.alpaca_key.as_deref().unwrap_or_default();
        let secret = sec.alpaca_secret.as_deref().unwrap_or_default();
        let last_check = {
            let db_guard = db.lock().unwrap();
            cache_repository::get_app_meta(&db_guard, "splits_checked_through")
        }
        .unwrap_or_else(|| start_date.clone());
        match crate::alpaca::corporate_actions::fetch_recent_split_symbols(key, secret, &symbols_for_bars, &last_check).await {
            Ok(syms) if !syms.is_empty() => {
                let _ = log(&db, "info", &format!("startup: {} symbol(s) split since {last_check} — refetching split-adjusted history", syms.len()));
                match crate::alpaca::bars::fetch_daily_bars_since(key, secret, &syms, &desired_start).await {
                    Ok(bars) => bars,
                    Err(e) => {
                        push_warning(&state, &format!("split refetch failed: {e}"));
                        std::collections::HashMap::new()
                    }
                }
            }
            Ok(_) => std::collections::HashMap::new(),
            Err(e) => {
                push_warning(&state, &format!("split check failed: {e}"));
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
        // Purge the symbols we're about to rewrite with a fresh split-adjusted
        // series so old-scale rows don't linger; the readjusted bars (chained
        // last) then override any incremental rows just fetched for them.
        for sym in readjusted_bars.keys() {
            let _ = cache_repository::delete_symbol_bars(&db_guard, sym);
        }
        for (_symbol, bars) in daily_bars.iter().chain(readjusted_bars.iter()) {
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
        // Record that the cache is now split-adjusted (suppresses the one-time
        // re-backfill) and the date through which splits have been reconciled, so
        // only newer splits are checked next run. Guarded on a successful real
        // fetch so a transient Alpaca outage doesn't falsely mark the cache fixed.
        if !mock_alpaca && daily_fetch_ok {
            let today = Utc::now().format("%Y-%m-%d").to_string();
            let _ = cache_repository::set_app_meta(&db_guard, "bars_adjustment", "split");
            let _ = cache_repository::set_app_meta(&db_guard, "splits_checked_through", &today);
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

    // ── Step 7b: compute_changes (multi-day close-to-close % changes) ──────────
    set_step(&state, "compute_changes", StepStatus::Running, None);
    {
        let db_guard = db.lock().unwrap();
        match cache_repository::recompute_multiday_changes(&db_guard) {
            Ok(n) => {
                drop(db_guard);
                set_step(&state, "compute_changes", StepStatus::Success, Some(&format!("{n} symbols")));
                let _ = log(&db, "info", &format!("startup: multi-day price changes recomputed for {n} symbols"));
            }
            Err(e) => {
                drop(db_guard);
                set_step(&state, "compute_changes", StepStatus::Warning, Some(&format!("failed: {e}")));
                let _ = log(&db, "warn", &format!("startup: multi-day change recompute failed: {e}"));
            }
        }
    }

    // ── Step 7c: compute_metrics (ATR + prev_close + Pump&Dump score) ─────────
    // CPU-only passes over the daily cache (no network). prev_close/ATR fill the
    // columns the pipeline previously left NULL; the Pump&Dump score is a DB-wide
    // percentile of the daily-wick behaviour (100 = most pump&dump-like).
    set_step(&state, "compute_metrics", StepStatus::Running, None);
    {
        let (pc, pd) = {
            let db_guard = db.lock().unwrap();
            let pc = cache_repository::recompute_atr_prev_close(&db_guard).unwrap_or(0);
            let pd = cache_repository::recompute_pump_dump_scores(&db_guard).unwrap_or(0);
            (pc, pd)
        };
        set_step(&state, "compute_metrics", StepStatus::Success,
            Some(&format!("ATR/prev_close {pc} · Pump&Dump {pd}")));
        let _ = log(&db, "info", &format!("startup: ATR/prev_close {pc}, Pump&Dump scored {pd}"));
    }

    // ── Step 7c: fetch_short_interest (Massive bulk, once/day) ────────────────
    // The whole-universe dump replaces the per-ticker company_intel path (which
    // only ever reached ~50 tickers/launch). Persisted into the company_intel
    // short-interest columns the UI already reads.
    set_step(&state, "fetch_short_interest", StepStatus::Running, None);
    const SI_DATE_KEY: &str = "short_interest_fetch_date";
    let si_fresh_today = {
        let g = db.lock().unwrap();
        cache_repository::get_app_meta(&g, SI_DATE_KEY).as_deref() == Some(today.as_str())
    };
    if si_fresh_today {
        set_step(&state, "fetch_short_interest", StepStatus::Success, Some("cached today"));
    } else if !has_massive {
        push_warning(&state, "No Massive key — short interest not collected");
        set_step(&state, "fetch_short_interest", StepStatus::Warning, Some("no Massive key"));
    } else {
        match crate::company_intel::collect_short_interest_bulk(db.clone(), secrets.clone()).await {
            Ok(n) => {
                let g = db.lock().unwrap();
                let _ = cache_repository::set_app_meta(&g, SI_DATE_KEY, &today);
                drop(g);
                set_step(&state, "fetch_short_interest", StepStatus::Success, Some(&format!("{n} tickers")));
                let _ = log(&db, "info", &format!("startup: short interest bulk — {n} tickers"));
            }
            Err(e) => {
                push_warning(&state, &format!("short interest fetch failed: {e}"));
                set_step(&state, "fetch_short_interest", StepStatus::Warning, Some(&format!("error: {e}")));
            }
        }
    }

    // ── Step 7d: fetch_splits (Alpaca corporate actions, once/day) ────────────
    // Persist the last ~13 months of splits into `ticker_splits` (display + the
    // dilution score's split-neutralisation), then roll up the display columns.
    set_step(&state, "fetch_splits", StepStatus::Running, None);
    const SPLITS_DATE_KEY: &str = "splits_full_fetch_date";
    let splits_fresh_today = {
        let g = db.lock().unwrap();
        cache_repository::get_app_meta(&g, SPLITS_DATE_KEY).as_deref() == Some(today.as_str())
    };
    if !splits_fresh_today && !mock_alpaca {
        let key = sec.alpaca_key.as_deref().unwrap_or_default();
        let secret = sec.alpaca_secret.as_deref().unwrap_or_default();
        let start = (Utc::now() - chrono::Duration::days(400)).format("%Y-%m-%d").to_string();
        match crate::alpaca::corporate_actions::fetch_all_splits(key, secret, &symbols_for_bars, &start).await {
            Ok(events) => {
                let rows: Vec<cache_repository::SplitRow> = events
                    .into_iter()
                    .map(|e| cache_repository::SplitRow {
                        symbol: e.symbol, ex_date: e.date, label: e.label,
                        from_factor: e.from, to_factor: e.to,
                    })
                    .collect();
                let n = {
                    let g = db.lock().unwrap();
                    let n = cache_repository::replace_ticker_splits(&g, &rows).unwrap_or(0);
                    let _ = cache_repository::set_app_meta(&g, SPLITS_DATE_KEY, &today);
                    n
                };
                let _ = log(&db, "info", &format!("startup: splits bulk — {n} events"));
            }
            Err(e) => push_warning(&state, &format!("splits fetch failed: {e}")),
        }
    }
    {
        let one_year_ago = (Utc::now() - chrono::Duration::days(365)).format("%Y-%m-%d").to_string();
        let n = {
            let g = db.lock().unwrap();
            cache_repository::recompute_split_rollups(&g, &one_year_ago).unwrap_or(0)
        };
        set_step(&state, "fetch_splits", StepStatus::Success, Some(&format!("{n} tickers with splits")));
    }

    // ── Step 7e: fetch_dilution (SEC XBRL frames, once/day) + dilution score ──
    // Bulk historical shares-outstanding snapshots → `dilution_snapshots`; then the
    // split-adjusted 12-month dilution % + DB-wide percentile (100 = most dilutive).
    set_step(&state, "fetch_dilution", StepStatus::Running, None);
    const DIL_DATE_KEY: &str = "dilution_fetch_date";
    let dil_fresh_today = {
        let g = db.lock().unwrap();
        cache_repository::get_app_meta(&g, DIL_DATE_KEY).as_deref() == Some(today.as_str())
    };
    if !dil_fresh_today && !mock_alpaca {
        match crate::company_intel::collect_sec_bulk(db.clone(), config.clone()).await {
            Ok((snaps, fins)) => {
                let g = db.lock().unwrap();
                let _ = cache_repository::set_app_meta(&g, DIL_DATE_KEY, &today);
                drop(g);
                let _ = log(&db, "info",
                    &format!("startup: SEC bulk — {snaps} shares snapshots, {fins} financials"));
            }
            Err(e) => push_warning(&state, &format!("SEC bulk fetch failed: {e}")),
        }
    }
    {
        let n = {
            let g = db.lock().unwrap();
            cache_repository::recompute_dilution_scores(&g, &today).unwrap_or(0)
        };
        set_step(&state, "fetch_dilution", StepStatus::Success, Some(&format!("{n} scored")));
        let _ = log(&db, "info", &format!("startup: dilution scored {n} symbols"));
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

    // ── Step 8b: compute_risk_scores (absolute per-ticker, 0..100) ────────────
    // Capacité à diluer / Besoin de diluer / Short interest score, from the SEC
    // dilution + financials sections and the bulk short interest. CPU-only; runs
    // after compute_universe (needs market_cap / float). NULL where inputs are
    // missing (never invented). Recomputed again after the per-ticker collection
    // job (lib.rs) so same-day fills are reflected.
    set_step(&state, "compute_risk_scores", StepStatus::Running, None);
    {
        let n = {
            let g = db.lock().unwrap();
            cache_repository::recompute_risk_scores(&g, &today).unwrap_or(0)
        };
        set_step(&state, "compute_risk_scores", StepStatus::Success, Some(&format!("{n} tickers scored")));
        let _ = log(&db, "info", &format!("startup: risk scores computed for {n} tickers"));
    }

    // The Panic Mean Reversion watchlist is no longer built here: it depends on the
    // premarket session's volume, so it's built once at 09:00 ET by a dedicated
    // scheduler (`crate::panic_watchlist`) — independent of when the app launched.

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

// ─── Flat-files pipeline ───────────────────────────────────────────────────────

/// Startup pipeline for flat-files mode. No Alpaca: the universe + daily history
/// come from the on-disk flat files (`flat_files/daily/daily.db`), and FMP / Massive
/// / SEC are still used (online, when reachable) to fill float / company-metadata
/// gaps — so the app works fully offline once those were loaded once. Ends by
/// reporting the latest downloaded minute day; `lib.rs` then parks it in Market
/// Replay (paused) so charts populate and the engines run on Play.
pub async fn run_pipeline_flat_files(
    db: Arc<Mutex<rusqlite::Connection>>,
    config: Arc<RwLock<AppConfig>>,
    secrets: Arc<RwLock<Secrets>>,
    state: Arc<RwLock<StartupState>>,
    app_dir: std::path::PathBuf,
) {
    let sec = secrets.read().unwrap().clone();
    let key_set = |o: &Option<String>| o.as_deref().map(|s| !s.is_empty()).unwrap_or(false);
    let has_massive = key_set(&sec.massive_api_key);
    let has_fmp = key_set(&sec.fmp_api_key);
    let mock_float = !(has_massive || has_fmp);
    let has_sec = key_set(&sec.sec_api_key);
    let mock_sec = !has_sec;
    {
        state.write().unwrap().mock_mode = mock_float || mock_sec;
    }
    let today = crate::time::et_date(Utc::now());

    // ── load_config ───────────────────────────────────────────────────────────
    set_step(&state, "load_config", StepStatus::Running, None);
    set_step(&state, "load_config", StepStatus::Success, Some("config loaded (flat-files mode)"));
    let _ = log(&db, "info", "startup: flat-files mode — config loaded");

    // ── load_strategies ───────────────────────────────────────────────────────
    set_step(&state, "load_strategies", StepStatus::Running, None);
    let n_strategies = crate::strategies::registry::all_strategies().len();
    set_step(&state, "load_strategies", StepStatus::Success,
        Some(&format!("{n_strategies} compiled strategies")));

    // ── load_cache ────────────────────────────────────────────────────────────
    set_step(&state, "load_cache", StepStatus::Running, None);
    let cache_count = { let g = db.lock().unwrap(); universe_repository::count(&g).unwrap_or(0) };
    { state.write().unwrap().stats.cache_symbols = cache_count as usize; }
    set_step(&state, "load_cache", StepStatus::Success, Some(&format!("{cache_count} symbols in cache")));

    // ── load_flat_daily: copy flat daily.db → daily_cache + seed the universe ──
    set_step(&state, "load_flat_daily", StepStatus::Running, None);
    let flat_symbols = crate::flat_files::daily::symbols(&app_dir);
    let copied = {
        let g = db.lock().unwrap();
        crate::flat_files::daily::load_into_cache(&app_dir, &g)
    };
    // Seed `universe_assets` with any symbol present in the flat daily file but not
    // yet known (offline boot with an empty universe). Minimal rows — float / volume
    // are filled by compute_universe below.
    if !flat_symbols.is_empty() {
        let now = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let g = db.lock().unwrap();
        let existing: std::collections::HashSet<String> = universe_repository::get_all(&g)
            .unwrap_or_default()
            .into_iter()
            .map(|a| a.symbol)
            .collect();
        let _ = g.execute_batch("BEGIN");
        for sym in &flat_symbols {
            if !existing.contains(sym) {
                let asset = UniverseAsset {
                    symbol: sym.clone(),
                    name: Some(sym.clone()),
                    exchange: None,
                    tradable: true,
                    shortable: false,
                    float_shares: None,
                    market_cap: None,
                    avg_volume: None,
                    updated_at: now.clone(),
                };
                let _ = universe_repository::upsert(&g, &asset);
            }
        }
        let _ = g.execute_batch("COMMIT");
    }
    match copied {
        Ok(n) => {
            set_step(&state, "load_flat_daily", StepStatus::Success,
                Some(&format!("{n} daily bars · {} symbols from flat files", flat_symbols.len())));
            let _ = log(&db, "info",
                &format!("startup: flat daily loaded — {n} bars, {} symbols", flat_symbols.len()));
        }
        Err(ref e) => {
            push_warning(&state, &format!("flat daily load failed: {e}"));
            set_step(&state, "load_flat_daily", StepStatus::Warning, Some(&format!("error: {e}")));
        }
    }

    // Active set = all universe symbols (what the strategies / scanner stream).
    let active_symbols: Vec<String> = {
        let g = db.lock().unwrap();
        universe_repository::get_active_symbols(&g).unwrap_or_default()
    };

    // ── fetch_massive (floats; gap-fill, online optional) ─────────────────────
    // fetch_massive + fetch_sec run concurrently (independent providers).
    let (floats, _) = tokio::join!(
        step_floats(&db, &state, &sec, &today, mock_float, has_massive),
        step_sec(&db, &state, &sec, &today, mock_sec),
    );
    let float_map: std::collections::HashMap<String, &crate::massive::MassiveFloat> =
        floats.iter().map(|f| (f.symbol.clone(), f)).collect();
    let active_set: std::collections::HashSet<&String> = active_symbols.iter().collect();
    let with_float = float_map.keys().filter(|s| active_set.contains(s)).count();
    { state.write().unwrap().stats.with_float = with_float; }
    if matches!(
        state.read().unwrap().steps.iter().find(|s| s.id == "fetch_massive").map(|s| &s.status),
        Some(StepStatus::Success)
    ) {
        set_step(&state, "fetch_massive", StepStatus::Success,
            Some(&format!("{} float records ({with_float} in universe)", floats.len())));
    }

    // ── compute_changes (multi-day close-to-close % changes) ──────────────────
    // Skip when daily_cache is empty (no flat daily file downloaded yet).
    let has_daily = copied.as_ref().map(|n| *n > 0).unwrap_or(false);
    if has_daily {
        set_step(&state, "compute_changes", StepStatus::Running, None);
        {
            let g = db.lock().unwrap();
            match cache_repository::recompute_multiday_changes(&g) {
                Ok(n) => {
                    drop(g);
                    set_step(&state, "compute_changes", StepStatus::Success, Some(&format!("{n} symbols")));
                }
                Err(e) => {
                    drop(g);
                    set_step(&state, "compute_changes", StepStatus::Warning, Some(&format!("failed: {e}")));
                }
            }
        }

        set_step(&state, "compute_metrics", StepStatus::Running, None);
        {
            let (pc, pd) = {
                let g = db.lock().unwrap();
                (
                    cache_repository::recompute_atr_prev_close(&g).unwrap_or(0),
                    cache_repository::recompute_pump_dump_scores(&g).unwrap_or(0),
                )
            };
            set_step(&state, "compute_metrics", StepStatus::Success,
                Some(&format!("ATR/prev_close {pc} · Pump&Dump {pd}")));
        }
    } else {
        set_step(&state, "compute_changes", StepStatus::Success, Some("skipped (no daily data)"));
        set_step(&state, "compute_metrics", StepStatus::Success, Some("skipped (no daily data)"));
    }

    // ── fetch_short_interest (Massive bulk, once/day, online optional) ────────
    set_step(&state, "fetch_short_interest", StepStatus::Running, None);
    let si_fresh = {
        let g = db.lock().unwrap();
        cache_repository::get_app_meta(&g, "short_interest_fetch_date").as_deref() == Some(today.as_str())
    };
    if si_fresh {
        set_step(&state, "fetch_short_interest", StepStatus::Success, Some("cached today"));
    } else if !has_massive {
        set_step(&state, "fetch_short_interest", StepStatus::Warning, Some("no Massive key (offline / not configured)"));
    } else {
        match crate::company_intel::collect_short_interest_bulk(db.clone(), secrets.clone()).await {
            Ok(n) => {
                { let g = db.lock().unwrap(); let _ = cache_repository::set_app_meta(&g, "short_interest_fetch_date", &today); }
                set_step(&state, "fetch_short_interest", StepStatus::Success, Some(&format!("{n} tickers")));
            }
            Err(e) => {
                push_warning(&state, &format!("short interest fetch failed: {e}"));
                set_step(&state, "fetch_short_interest", StepStatus::Warning, Some(&format!("error: {e}")));
            }
        }
    }

    // ── fetch_dilution (SEC XBRL, once/day, online optional) + scores ─────────
    set_step(&state, "fetch_dilution", StepStatus::Running, None);
    let dil_fresh = {
        let g = db.lock().unwrap();
        cache_repository::get_app_meta(&g, "dilution_fetch_date").as_deref() == Some(today.as_str())
    };
    if !dil_fresh && has_sec {
        match crate::company_intel::collect_sec_bulk(db.clone(), config.clone()).await {
            Ok((snaps, fins)) => {
                { let g = db.lock().unwrap(); let _ = cache_repository::set_app_meta(&g, "dilution_fetch_date", &today); }
                let _ = log(&db, "info", &format!("startup: SEC bulk — {snaps} snapshots, {fins} financials"));
            }
            Err(e) => push_warning(&state, &format!("SEC bulk fetch failed: {e}")),
        }
    }
    {
        let n = { let g = db.lock().unwrap(); cache_repository::recompute_dilution_scores(&g, &today).unwrap_or(0) };
        set_step(&state, "fetch_dilution", StepStatus::Success, Some(&format!("{n} scored")));
    }

    // ── compute_universe: persist float / market cap / avg volume ─────────────
    set_step(&state, "compute_universe", StepStatus::Running, None);
    let (prev_close_map, volume_map): (std::collections::HashMap<String, f64>, std::collections::HashMap<String, i64>) = {
        let g = db.lock().unwrap();
        (
            cache_repository::latest_closes(&g).unwrap_or_default().into_iter().collect(),
            cache_repository::avg_volumes(&g, 20).unwrap_or_default().into_iter().collect(),
        )
    };
    let universe_rows = { let g = db.lock().unwrap(); universe_repository::get_all(&g).unwrap_or_default() };
    {
        let now = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let g = db.lock().unwrap();
        let _ = g.execute_batch("BEGIN");
        for a in &universe_rows {
            let fmpf = float_map.get(&a.symbol);
            let float_shares = fmpf.map(|f| f.float_shares as i64);
            let prev_close = prev_close_map.get(&a.symbol).copied();
            let market_cap = fmpf
                .filter(|f| f.outstanding_shares > 0.0)
                .and_then(|f| prev_close.map(|pc| (f.outstanding_shares * pc) as i64));
            let asset = UniverseAsset {
                symbol: a.symbol.clone(),
                name: a.name.clone(),
                exchange: a.exchange.clone(),
                tradable: a.tradable,
                shortable: a.shortable,
                float_shares,
                market_cap,
                avg_volume: volume_map.get(&a.symbol).copied(),
                updated_at: now.clone(),
            };
            let _ = universe_repository::upsert(&g, &asset);
        }
        let _ = g.execute_batch("COMMIT");
    }
    let n_universe = universe_rows.len();
    { state.write().unwrap().stats.final_universe = n_universe; }
    set_step(&state, "compute_universe", StepStatus::Success,
        Some(&format!("{n_universe} symbols · {with_float} with float")));

    // ── compute_risk_scores ───────────────────────────────────────────────────
    set_step(&state, "compute_risk_scores", StepStatus::Running, None);
    {
        let n = { let g = db.lock().unwrap(); cache_repository::recompute_risk_scores(&g, &today).unwrap_or(0) };
        set_step(&state, "compute_risk_scores", StepStatus::Success, Some(&format!("{n} tickers scored")));
    }

    // ── load_replay_day: report the latest downloaded minute day (lib.rs starts it) ──
    set_step(&state, "load_replay_day", StepStatus::Running, None);
    match crate::flat_files::minute::latest_complete_day(&app_dir) {
        Some(day) => set_step(&state, "load_replay_day", StepStatus::Success,
            Some(&format!("dernier jour disponible : {day}"))),
        None => {
            push_warning(&state, "aucun jour minute téléchargé — ouvrez « Gestion Flat Files »");
            set_step(&state, "load_replay_day", StepStatus::Warning, Some("aucun jour téléchargé"));
        }
    }

    // ── ready ─────────────────────────────────────────────────────────────────
    set_step(&state, "ready", StepStatus::Success, Some("univers prêt — mode flat files (replay hors-ligne)"));
    let _ = log(&db, "info", "startup: flat-files pipeline complete");
    { state.write().unwrap().completed = true; }
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
    let _ = db_guard.execute_batch("BEGIN");
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
    let _ = db_guard.execute_batch("COMMIT");
}

// ─── Shared provider steps (used by BOTH pipelines) ────────────────────────────

/// Float provider step (`fetch_massive`). Reuses today's cache when fresh AND
/// non-empty; otherwise fetches from Massive (FMP fallback) and persists into
/// `fundamentals_cache`. The once-per-day marker is stamped ONLY on a real
/// successful fetch, and a fetch is FORCED whenever the float cache is empty — so a
/// fresh deploy (or a day that first ran in mock) always loads real floats once a
/// key is present. Returns the floats in effect this run; the caller derives the
/// in-universe count + map.
async fn step_floats(
    db: &Arc<Mutex<rusqlite::Connection>>,
    state: &Arc<RwLock<StartupState>>,
    sec: &Secrets,
    today: &str,
    mock_float: bool,
    has_massive: bool,
) -> Vec<crate::massive::MassiveFloat> {
    set_step(state, "fetch_massive", StepStatus::Running, None);
    let floats_fresh_today = {
        let g = db.lock().unwrap();
        cache_repository::get_app_meta(&g, FLOATS_DATE_KEY).as_deref() == Some(today)
    };
    let cached: Vec<crate::massive::MassiveFloat> = {
        let g = db.lock().unwrap();
        cache_repository::all_fundamentals(&g)
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
    let have_floats = !cached.is_empty();

    let mut float_fetch_ok = false;
    let floats: Vec<crate::massive::MassiveFloat> = if floats_fresh_today && have_floats {
        set_step(state, "fetch_massive", StepStatus::Success,
            Some(&format!("{} float records (cached today)", cached.len())));
        return cached;
    } else if mock_float {
        // No provider key. Keep real cached floats if we have them; only fall back to
        // mock when the cache is empty. NEVER stamp the marker (so a key added later
        // still triggers a real fetch the same day).
        if have_floats {
            push_warning(state, "No float provider key (Massive/FMP) — keeping cached floats");
            set_step(state, "fetch_massive", StepStatus::Warning,
                Some(&format!("{} cached float records (no provider key)", cached.len())));
            return cached;
        }
        push_warning(state, "No float provider key (Massive/FMP) — using mock float data");
        crate::massive::mock_float_all()
    } else if has_massive {
        let key = sec.massive_api_key.as_deref().unwrap_or_default();
        set_step(state, "fetch_massive", StepStatus::Running,
            Some("fetching bulk float (rate-limited ~1 req/13s)…"));
        match crate::massive::fetch_float_all(key).await {
            Ok(data) => { float_fetch_ok = true; data }
            Err(e) => {
                push_warning(state, &format!("Massive unavailable: {e} — using cached floats"));
                set_step(state, "fetch_massive", StepStatus::Warning, Some(&format!("fetch error: {e}")));
                return cached;
            }
        }
    } else {
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
                push_warning(state, &format!("FMP unavailable: {e} — using cached floats"));
                set_step(state, "fetch_massive", StepStatus::Warning, Some(&format!("fetch error: {e}")));
                return cached;
            }
        }
    };

    // Persist freshly fetched / mock floats, stamping the marker only on a real fetch.
    {
        let now = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let g = db.lock().unwrap();
        let _ = g.execute_batch("BEGIN");
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
            let _ = cache_repository::upsert_fundamental(&g, &fund);
        }
        if float_fetch_ok {
            let _ = cache_repository::set_app_meta(&g, FLOATS_DATE_KEY, today);
        }
        let _ = g.execute_batch("COMMIT");
    }
    set_step(state, "fetch_massive", StepStatus::Success,
        Some(&format!("{} float records", floats.len())));
    floats
}

/// SEC company-metadata step (`fetch_sec`: country of origin + SIC industry).
/// Uses a DEDICATED `sec_fetch_date` marker stamped ONLY on a real successful
/// fetch — never via the mock path, which previously poisoned the company_meta
/// timestamp and made the daily skip fire even when the data was mock. A fetch is
/// FORCED whenever `company_meta` is empty, so a fresh deploy with a key always
/// loads real country/industry.
async fn step_sec(
    db: &Arc<Mutex<rusqlite::Connection>>,
    state: &Arc<RwLock<StartupState>>,
    sec: &Secrets,
    today: &str,
    mock_sec: bool,
) {
    set_step(state, "fetch_sec", StepStatus::Running, None);
    let (count, fresh) = {
        let g = db.lock().unwrap();
        (
            company_meta_repository::count(&g).unwrap_or(0),
            cache_repository::get_app_meta(&g, SEC_DATE_KEY).as_deref() == Some(today),
        )
    };
    if count > 0 && fresh {
        set_step(state, "fetch_sec", StepStatus::Success, Some(&format!("{count} companies (cached today)")));
        return;
    }
    if mock_sec {
        // Only seed mock data when there is nothing at all (never clobber real rows),
        // and never stamp the marker — a real key added later still fetches today.
        if count == 0 {
            let companies = crate::sec_api::mock_companies();
            persist_company_meta(db, companies.values());
            set_step(state, "fetch_sec", StepStatus::Warning,
                Some(&format!("{} companies (mock — no sec-api key)", companies.len())));
        } else {
            set_step(state, "fetch_sec", StepStatus::Warning, Some(&format!("{count} cached (no sec-api key)")));
        }
        push_warning(state, "sec-api key not configured — country/industry from cache or mock");
        return;
    }
    let token = sec.sec_api_key.as_deref().unwrap_or_default();
    match crate::sec_api::fetch_all(token).await {
        Ok(companies) => {
            let with_country = companies.values().filter(|c| c.country.is_some()).count();
            persist_company_meta(db, companies.values());
            {
                let g = db.lock().unwrap();
                let _ = cache_repository::set_app_meta(&g, SEC_DATE_KEY, today);
            }
            set_step(state, "fetch_sec", StepStatus::Success,
                Some(&format!("{} companies · {with_country} with country", companies.len())));
            let _ = log(db, "info", &format!("startup: {} SEC company records", companies.len()));
        }
        Err(e) => {
            push_warning(state, &format!("sec-api unavailable: {e} — using cached company data"));
            set_step(state, "fetch_sec", StepStatus::Warning, Some(&format!("fetch error — {count} cached")));
        }
    }
}
