// Startup pipeline. Runs once at app launch (or on-demand via command).
// Updates a shared StartupState that the frontend polls via get_startup_status.
// Never holds the DB mutex across an await point.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex, RwLock};

use crate::config::{secrets::Secrets, AppConfig};
use crate::local_db::{
    cache_repository, company_meta_repository, insert_log, universe_repository, UniverseAsset,
};

/// Once-per-ET-day freshness markers (app_meta). Stamped only on a successful
/// provider fetch, so a missing key never masks a later fetch the same day.
const FLOATS_DATE_KEY: &str = "floats_fetch_date";
const SEC_DATE_KEY: &str = "sec_fetch_date";
const SECURITY_TYPES_DATE_KEY: &str = "massive_security_types_fetch_date";

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
    pub warnings: Vec<String>,
    pub completed: bool,
}

impl Default for StartupState {
    fn default() -> Self {
        Self {
            steps: default_steps(),
            stats: UniverseStats::default(),
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

/// The one step list. Flat files and the Alpaca API are not alternative modes but
/// complementary sources, so every launch shows every step: the ones a given launch
/// cannot run (no keys, offline, nothing downloaded) report themselves skipped with
/// the reason, rather than vanishing from a second list nobody keeps in sync.
fn default_steps() -> Vec<StartupStep> {
    vec![
        StartupStep::new("load_config", "Load local config"),
        StartupStep::new("load_strategies", "Load compiled strategies"),
        StartupStep::new("load_cache", "Load universe from cache"),
        StartupStep::new("load_flat_daily", "Load daily history from flat files"),
        StartupStep::new("fetch_alpaca", "Fetch Alpaca assets"),
        StartupStep::new("fetch_massive", "Fetch Massive float data"),
        StartupStep::new("fetch_sec", "Fetch SEC company data (country · industry)"),
        StartupStep::new("load_daily", "Load daily / historical data (250d)"),
        StartupStep::new("compute_changes", "Compute multi-day price changes"),
        StartupStep::new(
            "compute_metrics",
            "Compute ATR · prev close · Pump&Dump score",
        ),
        StartupStep::new(
            "fetch_short_interest",
            "Fetch short interest (Massive bulk)",
        ),
        StartupStep::new("fetch_splits", "Fetch stock splits (corporate actions)"),
        StartupStep::new(
            "fetch_dilution",
            "Fetch dilution + financials (SEC XBRL) · scores",
        ),
        StartupStep::new("compute_universe", "Persist float & average volume"),
        StartupStep::new(
            "compute_risk_scores",
            "Score dilution capacity · need · short interest",
        ),
        StartupStep::new("build_universe", "Finalize universe (all US stocks)"),
        StartupStep::new(
            "load_replay_day",
            "Latest flat-files day available for Market Replay",
        ),
        StartupStep::new("ready", "Ready"),
    ]
}

// ─── State helpers ────────────────────────────────────────────────────────────

fn set_step(state: &Arc<RwLock<StartupState>>, id: &str, status: StepStatus, detail: Option<&str>) {
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
    app_dir: std::path::PathBuf,
) {
    let sec = secrets.read().unwrap().clone();
    let offline = config.read().unwrap().data_source.offline;
    // `offline` disables every provider at once — it is a network switch, not a mode:
    // each step below already degrades on its own when its key is missing, so there is
    // one skip path, not two.
    let key_set =
        |o: &Option<String>| !offline && o.as_deref().map(|s| !s.is_empty()).unwrap_or(false);
    let has_alpaca = key_set(&sec.alpaca_key) && key_set(&sec.alpaca_secret);
    let has_massive = key_set(&sec.massive_api_key);
    let has_fmp = key_set(&sec.fmp_api_key); // legacy fallback for floats
    let has_sec = key_set(&sec.sec_api_key);

    // ── Step 1: load_config ───────────────────────────────────────────────────
    set_step(&state, "load_config", StepStatus::Running, None);
    set_step(
        &state,
        "load_config",
        StepStatus::Success,
        Some(if offline {
            "config loaded — hors ligne (flat files uniquement)"
        } else {
            "config loaded from tagdash.toml"
        }),
    );
    let _ = log(&db, "info", "startup: config loaded");

    // ── Step 2: load_strategies ───────────────────────────────────────────────
    set_step(&state, "load_strategies", StepStatus::Running, None);
    // Strategies are compiled in (see `strategies::registry`); no dynamic loading.
    let n_strategies = crate::strategies::registry::all_strategies().len();
    set_step(
        &state,
        "load_strategies",
        StepStatus::Success,
        Some(&format!("{n_strategies} compiled strategies")),
    );

    // ── Step 3: load_cache ────────────────────────────────────────────────────
    set_step(&state, "load_cache", StepStatus::Running, None);
    let cache_count = {
        let db_guard = db.lock().unwrap();
        universe_repository::count(&db_guard).unwrap_or(0)
    };
    {
        state.write().unwrap().stats.cache_symbols = cache_count as usize;
    }
    set_step(
        &state,
        "load_cache",
        StepStatus::Success,
        Some(&format!("{cache_count} symbols in cache")),
    );
    let _ = log(
        &db,
        "info",
        &format!("startup: cache has {cache_count} symbols"),
    );

    // ── Step 4: load_flat_daily ───────────────────────────────────────────────
    // Runs on EVERY launch, before anything network: it is free, offline, and often
    // deeper than Alpaca's 250-day window. Whatever it cannot supply, `load_daily`
    // fills in below — that cascade is the whole point of merging the two modes.
    step_flat_daily(&db, &state, &app_dir).await;

    // ── Step 5: fetch_alpaca ──────────────────────────────────────────────────
    set_step(&state, "fetch_alpaca", StepStatus::Running, None);
    let alpaca_assets = if offline {
        set_step(
            &state,
            "fetch_alpaca",
            StepStatus::Success,
            Some("ignoré — hors ligne (univers depuis le cache / les flat files)"),
        );
        Vec::new()
    } else if !has_alpaca {
        push_warning(&state, "Alpaca keys not configured — using cached assets only");
        set_step(
            &state,
            "fetch_alpaca",
            StepStatus::Warning,
            Some("ignoré — clés Alpaca absentes"),
        );
        Vec::new()
    } else {
        let key = sec.alpaca_key.as_deref().unwrap_or_default();
        let secret = sec.alpaca_secret.as_deref().unwrap_or_default();
        match crate::alpaca::assets::fetch_assets(key, secret).await {
            Ok(assets) => assets,
            Err(e) => {
                push_warning(
                    &state,
                    &format!("Alpaca fetch failed: {e} — falling back to cache"),
                );
                set_step(
                    &state,
                    "fetch_alpaca",
                    StepStatus::Warning,
                    Some(&format!("fetch error: {e}")),
                );
                let db_guard = db.lock().unwrap();
                universe_repository::get_all(&db_guard)
                    .unwrap_or_default()
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
                security_type: None,
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
    if matches!(
        state
            .read()
            .unwrap()
            .steps
            .iter()
            .find(|s| s.id == "fetch_alpaca")
            .map(|s| &s.status),
        Some(StepStatus::Running)
    ) {
        set_step(
            &state,
            "fetch_alpaca",
            StepStatus::Success,
            Some(&format!("{} tradable assets", alpaca_active.len())),
        );
    }
    let _ = log(
        &db,
        "info",
        &format!("startup: {} Alpaca tradable assets", alpaca_active.len()),
    );

    // ── Steps 6+7+8 (floats · sec · daily) — all three run in parallel ─────────
    // fetch_massive and fetch_sec are independent providers; load_daily only needs
    // the symbol list + Alpaca keys, so it can start at the same time. Starting all
    // three together shaves the longest of the three off the total wall time instead
    // of stacking them.
    //
    // The bar universe is read back from the DB rather than taken from
    // `alpaca_active`: it is the union of the Alpaca asset list, whatever the cache
    // already held, and the symbols the flat daily file just seeded — so an offline
    // launch still has a universe, and an online one still covers names the flat
    // files know about but Alpaca did not return.
    let today = crate::time::et_date(Utc::now());
    let symbols_for_bars: Vec<String> = {
        let g = db.lock().unwrap();
        universe_repository::get_active_symbols(&g).unwrap_or_default()
    };
    let (floats, _, _) = tokio::join!(
        step_floats(&db, &state, &sec, &today, has_massive, has_fmp),
        step_sec(&db, &state, &sec, &today, has_sec),
        step_daily(&db, &state, &sec, &symbols_for_bars, has_alpaca, &today),
    );
    let float_map: std::collections::HashMap<String, &crate::massive::MassiveFloat> =
        floats.iter().map(|f| (f.symbol.clone(), f)).collect();
    let with_float = symbols_for_bars
        .iter()
        .filter(|s| float_map.contains_key(*s))
        .count();
    {
        state.write().unwrap().stats.with_float = with_float;
    }
    if matches!(
        state
            .read()
            .unwrap()
            .steps
            .iter()
            .find(|s| s.id == "fetch_massive")
            .map(|s| &s.status),
        Some(StepStatus::Success)
    ) {
        set_step(
            &state,
            "fetch_massive",
            StepStatus::Success,
            Some(&format!(
                "{} float records ({with_float} in universe)",
                floats.len()
            )),
        );
    }
    let _ = log(
        &db,
        "info",
        &format!("startup: {} float records (Massive/FMP)", floats.len()),
    );

    // Massive's reference-ticker type is the authoritative equity-vs-ETF/ETN/fund
    // discriminator used by Panic Mean Reversion. It is fetched separately from
    // floats because the two endpoints have different pagination and freshness.
    // Keep the previous classifications on a provider failure: falling back to the
    // name/exchange heuristic is safer than erasing known-good types.
    step_security_types(&db, &state, &sec, &today, has_massive).await;

    // ── Steps 7b–7e: compute + network pulls — all five in parallel ───────────
    // compute_changes and compute_metrics are CPU/DB passes (no network); they
    // run in async blocks so they interleave naturally with the three network
    // calls. Total wall time = max(CPU, network) instead of their sum.
    // Both CPU passes read `daily_cache`; with neither source able to fill it there
    // is nothing to recompute and they'd only overwrite good values with zeros.
    let has_daily = {
        let g = db.lock().unwrap();
        cache_repository::symbols_with_bars(&g).unwrap_or(0) > 0
    };
    tokio::join!(
        async {
            if !has_daily {
                set_step(
                    &state,
                    "compute_changes",
                    StepStatus::Success,
                    Some("ignoré (aucune barre daily)"),
                );
                return;
            }
            set_step(&state, "compute_changes", StepStatus::Running, None);
            let g = db.lock().unwrap();
            match cache_repository::recompute_multiday_changes(&g) {
                Ok(n) => {
                    drop(g);
                    set_step(
                        &state,
                        "compute_changes",
                        StepStatus::Success,
                        Some(&format!("{n} symbols")),
                    );
                    let _ = log(
                        &db,
                        "info",
                        &format!("startup: multi-day price changes recomputed for {n} symbols"),
                    );
                }
                Err(e) => {
                    drop(g);
                    set_step(
                        &state,
                        "compute_changes",
                        StepStatus::Warning,
                        Some(&format!("failed: {e}")),
                    );
                    let _ = log(
                        &db,
                        "warn",
                        &format!("startup: multi-day change recompute failed: {e}"),
                    );
                }
            }
        },
        async {
            if !has_daily {
                set_step(
                    &state,
                    "compute_metrics",
                    StepStatus::Success,
                    Some("ignoré (aucune barre daily)"),
                );
                return;
            }
            set_step(&state, "compute_metrics", StepStatus::Running, None);
            let (pc, pd) = {
                let g = db.lock().unwrap();
                (
                    cache_repository::recompute_atr_prev_close(&g).unwrap_or(0),
                    cache_repository::recompute_pump_dump_scores(&g).unwrap_or(0),
                )
            };
            set_step(
                &state,
                "compute_metrics",
                StepStatus::Success,
                Some(&format!("ATR/prev_close {pc} · Pump&Dump {pd}")),
            );
            let _ = log(
                &db,
                "info",
                &format!("startup: ATR/prev_close {pc}, Pump&Dump scored {pd}"),
            );
        },
        step_short_interest(&db, &state, &secrets, has_massive, &today),
        step_splits_bulk(&db, &state, &sec, &symbols_for_bars, has_alpaca, &today),
        step_dilution(&db, &state, &config, &today, has_sec || has_alpaca),
    );

    // ── Step 14: persist float / market cap / avg volume ──────────────────────
    // There are no more "universes": we stream the whole US market (wildcard) and
    // each strategy does its own filtering (e.g. micro_pullback gates on float).
    // This step just persists each asset's float, derived market cap and average
    // volume into `universe_assets` so the scanner and strategies can read them.
    //
    // Driven off the persisted universe and the daily cache rather than off the
    // Alpaca response: both sources have landed by now, so this is source-agnostic
    // and covers symbols only the flat files know about.
    set_step(&state, "compute_universe", StepStatus::Running, None);
    let (prev_close_map, volume_map): (
        std::collections::HashMap<String, f64>,
        std::collections::HashMap<String, i64>,
    ) = {
        let g = db.lock().unwrap();
        (
            cache_repository::latest_closes(&g)
                .unwrap_or_default()
                .into_iter()
                .collect(),
            cache_repository::avg_volumes(&g, 20)
                .unwrap_or_default()
                .into_iter()
                .collect(),
        )
    };
    let universe_rows = {
        let g = db.lock().unwrap();
        universe_repository::get_all(&g).unwrap_or_default()
    };
    {
        let now = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let db_guard = db.lock().unwrap();
        let _ = db_guard.execute_batch("BEGIN");
        for a in &universe_rows {
            let fmpf = float_map.get(&a.symbol);
            let float_shares = fmpf.map(|f| f.float_shares as i64);
            let prev_close = prev_close_map.get(&a.symbol).copied();
            // Market cap derived as outstanding shares × last close (no extra API).
            let market_cap = fmpf
                .filter(|f| f.outstanding_shares > 0.0)
                .and_then(|f| prev_close.map(|pc| (f.outstanding_shares * pc) as i64));
            let asset = UniverseAsset {
                symbol: a.symbol.clone(),
                name: a.name.clone(),
                exchange: a.exchange.clone(),
                security_type: a.security_type.clone(),
                tradable: a.tradable,
                shortable: a.shortable,
                float_shares,
                market_cap,
                avg_volume: volume_map.get(&a.symbol).copied(),
                updated_at: now.clone(),
            };
            let _ = universe_repository::upsert(&db_guard, &asset);
        }
        let _ = db_guard.execute_batch("COMMIT");
    }
    let us_stocks_count = universe_rows.len();
    {
        let mut s = state.write().unwrap();
        s.stats.final_universe = us_stocks_count;
    }
    set_step(
        &state,
        "compute_universe",
        StepStatus::Success,
        Some(&format!(
            "{us_stocks_count} US stocks · {with_float} with float"
        )),
    );
    let _ = log(
        &db,
        "info",
        &format!("startup: persisted {us_stocks_count} US stocks ({with_float} with float)"),
    );

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
        set_step(
            &state,
            "compute_risk_scores",
            StepStatus::Success,
            Some(&format!("{n} tickers scored")),
        );
        let _ = log(
            &db,
            "info",
            &format!("startup: risk scores computed for {n} tickers"),
        );
    }

    // The Panic Mean Reversion watchlist is no longer built here: it depends on the
    // premarket session's volume, so it's built once at 09:00 ET by a dedicated
    // scheduler (`crate::panic_watchlist`) — independent of when the app launched.

    // ── Step 16: finalize ─────────────────────────────────────────────────────
    // Nothing more to build — the streamable set is the whole US market.
    set_step(
        &state,
        "build_universe",
        StepStatus::Success,
        Some(&format!("All US stocks streamable: {us_stocks_count}")),
    );
    let _ = log(&db, "info", "startup: universe ready (all US stocks)");

    // ── Step 17: load_replay_day ──────────────────────────────────────────────
    // Informational in both cases: which downloaded day Market Replay can open. When
    // there is no live feed to watch (offline, or no Alpaca keys), `lib.rs` parks it
    // paused so charts populate on launch.
    set_step(&state, "load_replay_day", StepStatus::Running, None);
    match crate::flat_files::minute::latest_complete_day(&app_dir) {
        Some(day) => set_step(
            &state,
            "load_replay_day",
            StepStatus::Success,
            Some(&format!("dernier jour disponible : {day}")),
        ),
        None if offline || !has_alpaca => {
            push_warning(
                &state,
                "aucun jour minute téléchargé — ouvrez « Source de données »",
            );
            set_step(
                &state,
                "load_replay_day",
                StepStatus::Warning,
                Some("aucun jour téléchargé"),
            );
        }
        None => set_step(
            &state,
            "load_replay_day",
            StepStatus::Success,
            Some("aucun jour téléchargé — le replay se chargera via l'API"),
        ),
    }

    // ── Step 18: ready ────────────────────────────────────────────────────────
    set_step(
        &state,
        "ready",
        StepStatus::Success,
        Some(if offline {
            "univers prêt — hors ligne (replay sur flat files)"
        } else if !has_alpaca {
            "univers prêt — pas de clés Alpaca, replay hors-ligne"
        } else {
            "universe ready — starting Alpaca WebSocket"
        }),
    );
    let _ = log(&db, "info", "startup: pipeline complete");

    {
        let mut s = state.write().unwrap();
        s.completed = true;
    }
}

async fn step_security_types(
    db: &Arc<Mutex<rusqlite::Connection>>,
    state: &Arc<RwLock<StartupState>>,
    secrets: &Secrets,
    today: &str,
    has_massive: bool,
) {
    let (fresh, cached) = {
        let g = db.lock().unwrap();
        let fresh =
            cache_repository::get_app_meta(&g, SECURITY_TYPES_DATE_KEY).as_deref() == Some(today);
        let cached = g
            .query_row(
                "SELECT COUNT(*) FROM universe_assets WHERE security_type IS NOT NULL",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap_or(0);
        (fresh, cached)
    };
    if fresh && cached > 0 {
        return;
    }
    if !has_massive {
        if cached == 0 {
            push_warning(
                state,
                "Massive non configuré — Panic Mean Reversion utilise le filtre ETF de secours (nom/exchange)",
            );
        }
        return;
    }

    let Some(key) = secrets.massive_api_key.as_deref() else {
        return;
    };
    // The float endpoint may just have consumed this plan's one-request-per-window
    // allowance. Waiting here prevents the first reference page from being rejected
    // with 429 on constrained Massive plans.
    tokio::time::sleep(crate::massive::RATE_LIMIT).await;
    match crate::massive::reference::fetch_security_types(key).await {
        Ok(rows) => {
            let changed = {
                let g = db.lock().unwrap();
                let changed = universe_repository::update_security_types(&g, &rows).unwrap_or(0);
                if !rows.is_empty() {
                    let _ = cache_repository::set_app_meta(&g, SECURITY_TYPES_DATE_KEY, today);
                }
                changed
            };
            let _ = log(
                db,
                "info",
                &format!("startup: {changed} classifications Massive appliquées"),
            );
        }
        Err(e) => {
            push_warning(
                state,
                &format!("Classification Massive indisponible ({e}) — types précédents conservés"),
            );
            let _ = log(db, "warn", &format!("startup: classification Massive: {e}"));
        }
    }
}

// ─── Steps ───────────────────────────────────────────────────────────────────

/// Flat daily step (`load_flat_daily`): copy the cumulative on-disk daily file into
/// `daily_cache` and seed `universe_assets` with any symbol only it knows about.
///
/// Runs on every launch, before anything network. It costs no request, works with no
/// connection, and usually reaches further back than Alpaca's 250-day window — so it
/// is a strict addition to the API path rather than an alternative to it. Symbols
/// whose stored rows predate a split are left out; see
/// `flat_files::daily::load_into_cache`.
async fn step_flat_daily(
    db: &Arc<Mutex<rusqlite::Connection>>,
    state: &Arc<RwLock<StartupState>>,
    app_dir: &std::path::Path,
) {
    set_step(state, "load_flat_daily", StepStatus::Running, None);
    let flat_symbols = crate::flat_files::daily::symbols(app_dir);
    if flat_symbols.is_empty() {
        set_step(
            state,
            "load_flat_daily",
            StepStatus::Success,
            Some("aucun fichier daily téléchargé — historique via l'API"),
        );
        return;
    }
    let copied = {
        let g = db.lock().unwrap();
        crate::flat_files::daily::load_into_cache(app_dir, &g)
    };
    // Minimal rows for symbols the universe doesn't know yet (offline boot on an
    // empty cache). Float / volume are filled by compute_universe later.
    {
        let now = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let g = db.lock().unwrap();
        let existing: std::collections::HashSet<String> = universe_repository::get_all(&g)
            .unwrap_or_default()
            .into_iter()
            .map(|a| a.symbol)
            .collect();
        let _ = g.execute_batch("BEGIN");
        for sym in flat_symbols.iter().filter(|s| !existing.contains(*s)) {
            let _ = universe_repository::upsert(
                &g,
                &UniverseAsset {
                    symbol: sym.clone(),
                    name: Some(sym.clone()),
                    exchange: None,
                    security_type: None,
                    tradable: true,
                    shortable: false,
                    float_shares: None,
                    market_cap: None,
                    avg_volume: None,
                    updated_at: now.clone(),
                },
            );
        }
        let _ = g.execute_batch("COMMIT");
    }
    match copied {
        Ok(n) => {
            set_step(
                state,
                "load_flat_daily",
                StepStatus::Success,
                Some(&format!(
                    "{n} daily bars · {} symbols from flat files",
                    flat_symbols.len()
                )),
            );
            let _ = log(
                db,
                "info",
                &format!(
                    "startup: flat daily loaded — {n} bars, {} symbols",
                    flat_symbols.len()
                ),
            );
        }
        Err(e) => {
            push_warning(state, &format!("flat daily load failed: {e}"));
            set_step(
                state,
                "load_flat_daily",
                StepStatus::Warning,
                Some(&format!("error: {e}")),
            );
        }
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
/// successful fetch, and a fetch is forced whenever the float cache is empty.
/// Returns the provider or cached floats in effect this run.
async fn step_floats(
    db: &Arc<Mutex<rusqlite::Connection>>,
    state: &Arc<RwLock<StartupState>>,
    sec: &Secrets,
    today: &str,
    has_massive: bool,
    has_fmp: bool,
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

    let floats: Vec<crate::massive::MassiveFloat> = if floats_fresh_today && have_floats {
        set_step(
            state,
            "fetch_massive",
            StepStatus::Success,
            Some(&format!("{} float records (cached today)", cached.len())),
        );
        return cached;
    } else if !has_massive && !has_fmp {
        if have_floats {
            push_warning(
                state,
                "No float provider key (Massive/FMP) — keeping cached floats",
            );
            set_step(
                state,
                "fetch_massive",
                StepStatus::Warning,
                Some(&format!(
                    "{} cached float records (no provider key)",
                    cached.len()
                )),
            );
            return cached;
        }
        push_warning(
            state,
            "No float provider key (Massive/FMP) — no float data available",
        );
        set_step(
            state,
            "fetch_massive",
            StepStatus::Warning,
            Some("ignoré — aucune clé fournisseur et aucun cache"),
        );
        return Vec::new();
    } else if has_massive {
        let key = sec.massive_api_key.as_deref().unwrap_or_default();
        set_step(
            state,
            "fetch_massive",
            StepStatus::Running,
            Some("fetching bulk float (rate-limited ~1 req/13s)…"),
        );
        match crate::massive::fetch_float_all(key).await {
            Ok(data) => data,
            Err(e) => {
                push_warning(
                    state,
                    &format!("Massive unavailable: {e} — using cached floats"),
                );
                set_step(
                    state,
                    "fetch_massive",
                    StepStatus::Warning,
                    Some(&format!("fetch error: {e}")),
                );
                return cached;
            }
        }
    } else {
        let key = sec.fmp_api_key.as_deref().unwrap_or_default();
        match crate::fmp::fetch_shares_float_all(key).await {
            Ok(data) => data
                .into_iter()
                    .map(|f| crate::massive::MassiveFloat {
                        symbol: f.symbol,
                        float_shares: f.float_shares,
                        outstanding_shares: f.outstanding_shares,
                        free_float: f.free_float,
                    })
                    .collect(),
            Err(e) => {
                push_warning(
                    state,
                    &format!("FMP unavailable: {e} — using cached floats"),
                );
                set_step(
                    state,
                    "fetch_massive",
                    StepStatus::Warning,
                    Some(&format!("fetch error: {e}")),
                );
                return cached;
            }
        }
    };

    // Persist freshly fetched floats, stamping the marker only on a real fetch.
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
        let _ = cache_repository::set_app_meta(&g, FLOATS_DATE_KEY, today);
        let _ = g.execute_batch("COMMIT");
    }
    set_step(
        state,
        "fetch_massive",
        StepStatus::Success,
        Some(&format!("{} float records", floats.len())),
    );
    floats
}

/// SEC company-metadata step (`fetch_sec`: country of origin + SIC industry).
/// Uses a dedicated `sec_fetch_date` marker stamped only on a successful fetch.
async fn step_sec(
    db: &Arc<Mutex<rusqlite::Connection>>,
    state: &Arc<RwLock<StartupState>>,
    sec: &Secrets,
    today: &str,
    has_sec: bool,
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
        set_step(
            state,
            "fetch_sec",
            StepStatus::Success,
            Some(&format!("{count} companies (cached today)")),
        );
        return;
    }
    if !has_sec {
        set_step(
            state,
            "fetch_sec",
            StepStatus::Warning,
            Some(&format!("{count} cached (no sec-api key)")),
        );
        push_warning(
            state,
            "sec-api key not configured — country/industry from cache only",
        );
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
            set_step(
                state,
                "fetch_sec",
                StepStatus::Success,
                Some(&format!(
                    "{} companies · {with_country} with country",
                    companies.len()
                )),
            );
            let _ = log(
                db,
                "info",
                &format!("startup: {} SEC company records", companies.len()),
            );
        }
        Err(e) => {
            push_warning(
                state,
                &format!("sec-api unavailable: {e} — using cached company data"),
            );
            set_step(
                state,
                "fetch_sec",
                StepStatus::Warning,
                Some(&format!("fetch error — {count} cached")),
            );
        }
    }
}

/// Daily bars step (`load_daily`): incremental Alpaca fetch (250d first run, missing
/// days on subsequent runs), layered on top of whatever `load_flat_daily` already
/// put in the cache. No-ops with a clear reason when Alpaca is unreachable — the
/// flat rows then stand on their own.
async fn step_daily(
    db: &Arc<Mutex<rusqlite::Connection>>,
    state: &Arc<RwLock<StartupState>>,
    sec: &Secrets,
    symbols: &[String],
    has_alpaca: bool,
    today: &str,
) {
    set_step(state, "load_daily", StepStatus::Running, None);

    // No Alpaca (offline, or no keys) → stop here and keep what `load_flat_daily`
    // supplied. Invented bars must never enter `daily_cache` beside real rows.
    if !has_alpaca {
        let covered = {
            let g = db.lock().unwrap();
            cache_repository::symbols_with_bars(&g).unwrap_or(0)
        };
        set_step(
            state,
            "load_daily",
            StepStatus::Success,
            Some(&format!(
                "ignoré (Alpaca indisponible) — {covered} symboles couverts par les flat files"
            )),
        );
        return;
    }

    let desired_start = (Utc::now() - chrono::Duration::days(250))
        .format("%Y-%m-%d")
        .to_string();
    let backfill_threshold = (Utc::now() - chrono::Duration::days(235))
        .format("%Y-%m-%d")
        .to_string();
    let (latest_cached, earliest_cached) = {
        let g = db.lock().unwrap();
        (
            cache_repository::latest_bar_date(&g).unwrap_or(None),
            cache_repository::earliest_bar_date(&g).unwrap_or(None),
        )
    };
    let split_adjusted = {
        let g = db.lock().unwrap();
        cache_repository::get_app_meta(&g, "bars_adjustment").as_deref() == Some("split")
    };
    let force_readjust = !split_adjusted;
    let need_backfill = force_readjust
        || match earliest_cached.as_deref() {
            Some(d) if !d.is_empty() => d > backfill_threshold.as_str(),
            _ => true,
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
    let daily_bars = {
        let key = sec.alpaca_key.as_deref().unwrap_or_default();
        let secret = sec.alpaca_secret.as_deref().unwrap_or_default();
        match crate::alpaca::bars::fetch_daily_bars_since(key, secret, symbols, &start_date).await {
            Ok(bars) => bars,
            Err(e) => {
                daily_fetch_ok = false;
                push_warning(
                    state,
                    &format!("Alpaca bars fetch failed: {e} — keeping cached bars"),
                );
                std::collections::HashMap::new()
            }
        }
    };
    // Split reconciliation: on incremental runs refetch split-adjusted history for
    // symbols that split since the last check.
    let readjusted_bars = if need_backfill || !daily_fetch_ok {
        std::collections::HashMap::new()
    } else {
        let key = sec.alpaca_key.as_deref().unwrap_or_default();
        let secret = sec.alpaca_secret.as_deref().unwrap_or_default();
        let last_check = {
            let g = db.lock().unwrap();
            cache_repository::get_app_meta(&g, "splits_checked_through")
        }
        .unwrap_or_else(|| start_date.clone());
        match crate::alpaca::corporate_actions::fetch_recent_split_symbols(
            key,
            secret,
            symbols,
            &last_check,
        )
        .await
        {
            Ok(syms) if !syms.is_empty() => {
                let _ = log(
                    db,
                    "info",
                    &format!(
                        "startup: {} symbol(s) split since {last_check} — refetching",
                        syms.len()
                    ),
                );
                match crate::alpaca::bars::fetch_daily_bars_since(
                    key,
                    secret,
                    &syms,
                    &desired_start,
                )
                .await
                {
                    Ok(bars) => bars,
                    Err(e) => {
                        push_warning(state, &format!("split refetch failed: {e}"));
                        std::collections::HashMap::new()
                    }
                }
            }
            Ok(_) => std::collections::HashMap::new(),
            Err(e) => {
                push_warning(state, &format!("split check failed: {e}"));
                std::collections::HashMap::new()
            }
        }
    };
    // Persist bars + refresh avg_volume in ONE transaction.
    {
        let now = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let g = db.lock().unwrap();
        let _ = g.execute_batch("BEGIN");
        for sym in readjusted_bars.keys() {
            let _ = cache_repository::delete_symbol_bars(&g, sym);
        }
        for (_sym, bars) in daily_bars.iter().chain(readjusted_bars.iter()) {
            for bar in bars {
                let _ = cache_repository::upsert_daily_bar(
                    &g,
                    &cache_repository::DailyBar {
                        symbol: bar.symbol.clone(),
                        date: bar.date.clone(),
                        open: bar.open,
                        high: bar.high,
                        low: bar.low,
                        close: bar.close,
                        volume: bar.volume,
                        updated_at: now.clone(),
                    },
                );
            }
        }
        let volume_map: std::collections::HashMap<String, i64> =
            cache_repository::avg_volumes(&g, 20)
                .unwrap_or_default()
                .into_iter()
                .collect();
        for (sym, avg_vol) in &volume_map {
            let _ = g.execute(
                "UPDATE universe_assets SET avg_volume=?1 WHERE symbol=?2",
                rusqlite::params![avg_vol, sym],
            );
        }
        if daily_fetch_ok {
            let _ = cache_repository::set_app_meta(&g, "bars_adjustment", "split");
            let _ = cache_repository::set_app_meta(&g, "splits_checked_through", today);
        }
        let _ = g.execute_batch("COMMIT");
    }
    let covered = {
        let g = db.lock().unwrap();
        cache_repository::symbols_with_bars(&g).unwrap_or(0)
    };
    let mode = if incremental {
        format!("incremental since {start_date}")
    } else {
        format!("250d backfill since {start_date}")
    };
    set_step(
        state,
        "load_daily",
        StepStatus::Success,
        Some(&format!(
            "{covered} symbols with bar data · {} updated ({mode})",
            daily_bars.len()
        )),
    );
    let _ = log(
        db,
        "info",
        &format!(
            "startup: daily bars — {covered} cached, {} updated ({mode})",
            daily_bars.len()
        ),
    );
}

/// Short interest bulk step (`fetch_short_interest`): Massive whole-universe dump,
/// once per ET day. No-ops when already cached or key is missing.
async fn step_short_interest(
    db: &Arc<Mutex<rusqlite::Connection>>,
    state: &Arc<RwLock<StartupState>>,
    secrets: &Arc<RwLock<crate::config::secrets::Secrets>>,
    has_massive: bool,
    today: &str,
) {
    const SI_KEY: &str = "short_interest_fetch_date";
    set_step(state, "fetch_short_interest", StepStatus::Running, None);
    let fresh = {
        let g = db.lock().unwrap();
        cache_repository::get_app_meta(&g, SI_KEY).as_deref() == Some(today)
    };
    if fresh {
        set_step(
            state,
            "fetch_short_interest",
            StepStatus::Success,
            Some("cached today"),
        );
        return;
    }
    if !has_massive {
        push_warning(state, "No Massive key — short interest not collected");
        set_step(
            state,
            "fetch_short_interest",
            StepStatus::Warning,
            Some("no Massive key"),
        );
        return;
    }
    match crate::company_intel::collect_short_interest_bulk(db.clone(), secrets.clone()).await {
        Ok(n) => {
            {
                let g = db.lock().unwrap();
                let _ = cache_repository::set_app_meta(&g, SI_KEY, today);
            }
            set_step(
                state,
                "fetch_short_interest",
                StepStatus::Success,
                Some(&format!("{n} tickers")),
            );
            let _ = log(
                db,
                "info",
                &format!("startup: short interest bulk — {n} tickers"),
            );
        }
        Err(e) => {
            push_warning(state, &format!("short interest fetch failed: {e}"));
            set_step(
                state,
                "fetch_short_interest",
                StepStatus::Warning,
                Some(&format!("error: {e}")),
            );
        }
    }
}

/// Splits bulk step (`fetch_splits`): Alpaca corporate-actions dump, once/day.
/// Skipped when Alpaca keys are absent or when already cached today.
async fn step_splits_bulk(
    db: &Arc<Mutex<rusqlite::Connection>>,
    state: &Arc<RwLock<StartupState>>,
    sec: &Secrets,
    symbols: &[String],
    has_alpaca: bool,
    today: &str,
) {
    const SPLITS_KEY: &str = "splits_full_fetch_date";
    set_step(state, "fetch_splits", StepStatus::Running, None);
    let fresh = {
        let g = db.lock().unwrap();
        cache_repository::get_app_meta(&g, SPLITS_KEY).as_deref() == Some(today)
    };
    if !fresh && has_alpaca {
        let key = sec.alpaca_key.as_deref().unwrap_or_default();
        let secret = sec.alpaca_secret.as_deref().unwrap_or_default();
        let start = (Utc::now() - chrono::Duration::days(400))
            .format("%Y-%m-%d")
            .to_string();
        match crate::alpaca::corporate_actions::fetch_all_splits(key, secret, symbols, &start).await
        {
            Ok(events) => {
                let rows: Vec<cache_repository::SplitRow> = events
                    .into_iter()
                    .map(|e| cache_repository::SplitRow {
                        symbol: e.symbol,
                        ex_date: e.date,
                        label: e.label,
                        from_factor: e.from,
                        to_factor: e.to,
                    })
                    .collect();
                let n = {
                    let g = db.lock().unwrap();
                    let n = cache_repository::replace_ticker_splits(&g, &rows).unwrap_or(0);
                    let _ = cache_repository::set_app_meta(&g, SPLITS_KEY, today);
                    n
                };
                let _ = log(db, "info", &format!("startup: splits bulk — {n} events"));
            }
            Err(e) => push_warning(state, &format!("splits fetch failed: {e}")),
        }
    }
    let one_year_ago = (Utc::now() - chrono::Duration::days(365))
        .format("%Y-%m-%d")
        .to_string();
    let n = {
        let g = db.lock().unwrap();
        cache_repository::recompute_split_rollups(&g, &one_year_ago).unwrap_or(0)
    };
    set_step(
        state,
        "fetch_splits",
        StepStatus::Success,
        Some(&format!("{n} tickers with splits")),
    );
}

/// Dilution step (`fetch_dilution`): SEC XBRL bulk + score, once/day.
/// `should_fetch` is true when a real provider is available.
async fn step_dilution(
    db: &Arc<Mutex<rusqlite::Connection>>,
    state: &Arc<RwLock<StartupState>>,
    config: &Arc<RwLock<AppConfig>>,
    today: &str,
    should_fetch: bool,
) {
    const DIL_KEY: &str = "dilution_fetch_date";
    set_step(state, "fetch_dilution", StepStatus::Running, None);
    let fresh = {
        let g = db.lock().unwrap();
        cache_repository::get_app_meta(&g, DIL_KEY).as_deref() == Some(today)
    };
    if !fresh && should_fetch {
        match crate::company_intel::collect_sec_bulk(db.clone(), config.clone()).await {
            Ok((snaps, fins)) => {
                {
                    let g = db.lock().unwrap();
                    let _ = cache_repository::set_app_meta(&g, DIL_KEY, today);
                }
                let _ = log(
                    db,
                    "info",
                    &format!("startup: SEC bulk — {snaps} snapshots, {fins} financials"),
                );
            }
            Err(e) => push_warning(state, &format!("SEC bulk fetch failed: {e}")),
        }
    }
    let n = {
        let g = db.lock().unwrap();
        cache_repository::recompute_dilution_scores(&g, today).unwrap_or(0)
    };
    set_step(
        state,
        "fetch_dilution",
        StepStatus::Success,
        Some(&format!("{n} scored")),
    );
    let _ = log(db, "info", &format!("startup: dilution scored {n} symbols"));
}
