pub mod alpaca;
pub mod chart_state;
pub mod chart_payloads;
pub mod commands;
pub mod company_intel;
pub mod config;
pub mod dashboard;
pub mod enrichment;
pub mod flat_files;
pub mod fmp;
pub mod internal_trading;
pub mod llm;
pub mod local_db;
pub mod market_attention;
pub mod market_state;
pub mod massive;
pub mod micro_pullback;
pub mod notify;
pub mod panic_watchlist;
pub mod perfect_pullback;
pub mod replay;
pub mod scanner;
pub mod scoring;
pub mod screenshot;
pub mod sec_api;
pub mod startup;
pub mod state;
pub mod stt;
pub mod strategies;
pub mod time;
pub mod tradetally;
pub mod types;
pub mod universe;

use std::sync::{
    atomic::AtomicBool,
    Arc, Mutex, RwLock,
};
use state::AppState;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // ── Load config + secrets ─────────────────────────────────────────────
    let (app_dir, cfg) = config::load();
    let secrets = config::secrets::load(&app_dir);

    // ── Open SQLite + run migrations ──────────────────────────────────────
    let db = local_db::open_and_migrate(&app_dir)
        .expect("[tagdash] failed to open SQLite database");

    // ── Seed initial log ──────────────────────────────────────────────────
    let _ = local_db::insert_log(&db, "info", "TagDash started");

    // ── Strategy on/off map: compiled defaults + persisted runtime overrides ──
    let strategy_enabled: std::collections::HashMap<String, bool> = {
        let mut m: std::collections::HashMap<String, bool> = strategies::registry::all_strategies()
            .iter()
            .map(|s| (s.id().to_string(), s.enabled()))
            .collect();
        if let Some(json) = local_db::cache_repository::get_app_meta(&db, "strategy_overrides") {
            if let Ok(over) = serde_json::from_str::<std::collections::HashMap<String, bool>>(&json) {
                for (id, on) in over {
                    if m.contains_key(&id) {
                        m.insert(id, on);
                    }
                }
            }
        }
        m
    };

    // ── Strategy $-risk map: compiled defaults + persisted runtime overrides ──
    let strategy_risk: std::collections::HashMap<String, f64> = {
        let mut m: std::collections::HashMap<String, f64> = strategies::registry::all_strategies()
            .iter()
            .map(|s| (s.id().to_string(), s.risk_config().max_risk_dollars))
            .collect();
        if let Some(json) = local_db::cache_repository::get_app_meta(&db, "strategy_risk_overrides") {
            if let Ok(over) = serde_json::from_str::<std::collections::HashMap<String, f64>>(&json) {
                for (id, risk) in over {
                    if m.contains_key(&id) {
                        m.insert(id, risk);
                    }
                }
            }
        }
        m
    };

    // ── Restore the internal trading book + chart contexts ───────────────────
    // Positions can be held across several days, so the book (positions, resting
    // orders, trades, fills) and the per-ticker chart SL/TP/tradeID lines are
    // reloaded from SQLite — closing and reopening the app restores them as they
    // were. The backend trading loop then resumes filling/bracketing off live
    // prices against these restored orders.
    let internal_book = {
        let mut book = local_db::book_repository::load_book(&db);
        // Re-arm protective bracket orders for every restored open position so the
        // SL/TP exits are live the moment the trading loop starts — guarantees the
        // "open position with SL/TP ⇒ live bracket" invariant regardless of what
        // the persisted snapshot held (idempotent: cancels + recreates).
        let symbols: Vec<String> = book.positions.keys().cloned().collect();
        for sym in symbols {
            book.sync_bracket_orders(&sym);
        }
        book
    };
    let chart_state = {
        let mut cs = chart_state::ChartState::new();
        cs.import_contexts(local_db::book_repository::load_chart_contexts(&db));
        cs
    };

    // Focus symbols (displayed in chart zones) — the frontend updates this; the
    // live feed tick-streams them on top of the broad surveillance tier.
    let (focus_symbols_tx, _) = tokio::sync::watch::channel::<Vec<String>>(Vec::new());

    // Speech-to-Text shared handle (recorder + persisted job queue). Built before the
    // struct so it can borrow `app_dir` before the field shorthand below moves it.
    let stt_shared = Arc::new(stt::SttShared::new(app_dir.clone()));

    let app_state = AppState {
        app_dir,
        config:            Arc::new(RwLock::new(cfg)),
        secrets:           Arc::new(RwLock::new(secrets)),
        db:                Arc::new(Mutex::new(db)),
        startup:           Arc::new(RwLock::new(startup::StartupState::default())),
        market:            Arc::new(RwLock::new(market_state::MarketState::new())),
        mock_feed_running: Arc::new(AtomicBool::new(false)),
        live_feed_running: Arc::new(AtomicBool::new(false)),
        news_feed_running: Arc::new(AtomicBool::new(false)),
        scanner_running:   Arc::new(AtomicBool::new(false)),
        perfect_pullback_running: Arc::new(AtomicBool::new(false)),
        market_attention_running: Arc::new(AtomicBool::new(false)),
        micro_pullback_running: Arc::new(AtomicBool::new(false)),
        panic_watchlist_running: Arc::new(AtomicBool::new(false)),
        trading_loop_running: Arc::new(AtomicBool::new(false)),
        strategy_enabled:  Arc::new(RwLock::new(strategy_enabled)),
        strategy_risk:     Arc::new(RwLock::new(strategy_risk)),
        active_alerts:     Arc::new(RwLock::new(Vec::new())),
        alert_history:     Arc::new(RwLock::new(Vec::new())),
        screener:          Arc::new(RwLock::new(Vec::new())),
        attention:         Arc::new(RwLock::new(Vec::new())),
        chart:             Arc::new(RwLock::new(chart_state)),
        internal_book:     Arc::new(RwLock::new(internal_book)),
        enrichments:       Arc::new(RwLock::new(std::collections::HashMap::new())),
        focus_symbols_tx,
        replay:            Arc::new(replay::ReplayShared::default()),
        flat_files:        Arc::new(flat_files::FlatFilesShared::default()),
        stt:               stt_shared,
    };

    // ── Start Tauri ───────────────────────────────────────────────────────
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_notification::init())
        // Native Gamepad-API polyfill (WebView2/WKWebView don't expose it). Makes
        // the Xbox controller visible to navigator.getGamepads() in the frontend.
        .plugin(tauri_plugin_gamepad::init())
        // `relaunch()` after an auto-update is installed.
        .plugin(tauri_plugin_process::init())
        .manage(app_state)
        // Spawn background workers once the app is set up.
        .setup(|app| {
            // Automatic updates — deployed (release) desktop builds only. The
            // actual check / download / install / relaunch is driven from the
            // frontend at launch (first step of the startup pipeline) via
            // @tauri-apps/plugin-updater; here we just register the plugin so those
            // JS calls are available. In `tauri dev` (debug) the plugin is not
            // registered at all, so development is never interrupted (the frontend
            // also guards with import.meta.env.DEV).
            #[cfg(all(desktop, not(debug_assertions)))]
            app.handle().plugin(tauri_plugin_updater::Builder::new().build())?;

            let state = app.state::<AppState>();
            let db      = state.db.clone();
            let config  = state.config.clone();
            let secrets = state.secrets.clone();

            // Seed bundled default mood images / phrases / backgrounds into the
            // user's app-data ONCE (first launch). Never overwrites the user's own
            // files, and a one-time marker means later edits/deletions stick.
            {
                let seeded = {
                    let g = db.lock().unwrap();
                    local_db::cache_repository::get_app_meta(&g, "default_assets_seeded").as_deref()
                        == Some("1")
                };
                if !seeded {
                    if let Ok(res) =
                        app.path().resolve("resources/defaults", tauri::path::BaseDirectory::Resource)
                    {
                        dashboard::seed_default_assets(&res, &state.app_dir);
                    }
                    let g = db.lock().unwrap();
                    let _ = local_db::cache_repository::set_app_meta(&g, "default_assets_seeded", "1");
                }
            }

            // 0. Desktop attention cues for new alerts (flash overlay + foreground).
            //    Build the full-screen white flash overlay: always-on-top, transparent,
            //    click-through, no taskbar entry, NOT focused. It stays up permanently
            //    (invisible while transparent) and just pulses white on a notify event,
            //    so it never steals focus. `push_alert` reaches it through the AppHandle
            //    stashed by `notify::init`.
            {
                use tauri::{WebviewUrl, WebviewWindowBuilder};
                // Same SPA entry; main.tsx renders the flash overlay (not the app)
                // when it detects it's running in the window labelled "flash".
                match WebviewWindowBuilder::new(app.handle(), "flash", WebviewUrl::App("index.html".into()))
                    .title("")
                    .decorations(false)
                    // No drop shadow / frame: a borderless window still gets a DWM
                    // shadow on Windows, which shows as a faint outline around the
                    // (otherwise invisible) transparent overlay. Kill it.
                    .shadow(false)
                    .transparent(true)
                    .always_on_top(true)
                    .skip_taskbar(true)
                    .focused(false)
                    .visible(true)
                    .resizable(false)
                    .build()
                {
                    Ok(win) => {
                        let _ = win.set_ignore_cursor_events(true);
                        // Cover the whole primary monitor (work area + taskbar).
                        if let Ok(Some(mon)) = win.primary_monitor() {
                            let _ = win.set_size(*mon.size());
                            let _ = win.set_position(tauri::PhysicalPosition::new(0, 0));
                        }
                    }
                    Err(e) => eprintln!("[tagdash] flash overlay window not created: {e}"),
                }
            }
            notify::init(app.handle().clone());

            // 0b. Trade tape recorder: persists every live trade print (and the
            //     focus quotes) into one SQLite file per ET day, so the day can
            //     later be replayed tick-by-tick by the Market Replay module.
            replay::tape::init(state.app_dir.clone());

            // 1. TradeTally background sync worker.
            {
                let (db, config, secrets) = (db.clone(), config.clone(), secrets.clone());
                tauri::async_runtime::spawn(async move {
                    tradetally::worker::run(db, config, secrets).await;
                });
            }

            // 2. Production boot: build the universe, then connect the Alpaca live
            //    WebSocket so real-time data flows automatically at launch.
            //    (The mock feed is no longer auto-started — it stays available as a
            //    manual dev command but is disabled in production.)
            {
                let market            = state.market.clone();
                let startup           = state.startup.clone();
                let live_feed_running = state.live_feed_running.clone();
                let focus_rx          = state.focus_symbols_tx.subscribe();
                let app_handle        = app.handle().clone();
                let news_feed_running = state.news_feed_running.clone();
                // Dedicated handles for the company-intel collection job (kept as
                // its own clones so it can run after the pipeline without fighting
                // the moves above).
                let ci_db      = db.clone();
                let ci_config  = config.clone();
                let ci_secrets = secrets.clone();
                tauri::async_runtime::spawn(async move {
                    startup::run_pipeline(db.clone(), config.clone(), secrets.clone(), startup).await;
                    // In flat-files mode there is no real-time feed: the platform
                    // runs off the downloaded days via Market Replay, so neither the
                    // live data feed nor the premarket news feed are started (the
                    // Alpaca API may be unavailable entirely).
                    if config.read().unwrap().data_source.is_flat_files() {
                        eprintln!("[tagdash] mode flat files actif: flux live + news non démarrés (replay hors-ligne)");
                    } else {
                        match commands::spawn_live_feed(market.clone(), config, secrets.clone(), db, live_feed_running, focus_rx, app_handle) {
                            Ok(n)  => eprintln!("[tagdash] live feed: {n} US-stock universe ready"),
                            Err(e) => eprintln!("[tagdash] live feed not started: {e}"),
                        }
                        // Premarket news investor — independent of the data feed.
                        if let Err(e) = commands::spawn_news_feed(market, secrets, news_feed_running) {
                            eprintln!("[tagdash] news feed not started: {e}");
                        }
                    }
                    // Company-intelligence collection (short interest, financials,
                    // dilution filings, ownership). Runs ONCE here, after the
                    // universe is built, as a bounded TTL-gated pass. This is the
                    // exact entry point a future OS background worker will call —
                    // it never blocks the scanner / feed and makes no UI calls.
                    let rs_db = ci_db.clone();
                    crate::company_intel::run_collection_job(ci_db, ci_config, ci_secrets).await;
                    // Re-score dilution capacity / need now that the per-ticker SEC
                    // sections this launch collected are in the DB (the pipeline pass
                    // ran before them). Cheap CPU pass; never blocks the scanner.
                    {
                        let today = crate::time::et_date(crate::time::now());
                        let conn = rs_db.lock().unwrap();
                        let _ = crate::local_db::cache_repository::recompute_risk_scores(&conn, &today);
                    }
                });
            }

            // 3. Market Attention Gate engine (direction-agnostic ticker selection):
            //    once a minute between 09:30 and 12:30 ET it ranks the most-watched/
            //    traded tickers on a rolling 5-minute window and publishes the top 10.
            //    Perfect Pullback consumes this list (and memorises it). Never fires
            //    an alert / never trades; idles outside its window.
            {
                let ma_running = state.market_attention_running.clone();
                let market     = state.market.clone();
                let db         = state.db.clone();
                let secrets    = state.secrets.clone();
                let attention  = state.attention.clone();
                ma_running.store(true, std::sync::atomic::Ordering::Relaxed);
                market_attention::MarketAttentionEngine::start(
                    ma_running, market, db, secrets, attention,
                );
            }

            // 3a. Perfect Pullback engine (stateful, multi-timeframe): watches the
            //     tickers selected by Market Attention (memorised for the session) on
            //     1/2/5/10m for a strong move (gate 1) then fires on the first healthy
            //     pullback (gate 2). Auto-started; idles outside the regular session
            //     and honours the Settings toggle.
            {
                let pp_running       = state.perfect_pullback_running.clone();
                let market           = state.market.clone();
                let db               = state.db.clone();
                let active_alerts    = state.active_alerts.clone();
                let alert_history    = state.alert_history.clone();
                let strategy_enabled = state.strategy_enabled.clone();
                let attention        = state.attention.clone();
                pp_running.store(true, std::sync::atomic::Ordering::Relaxed);
                perfect_pullback::PerfectPullbackEngine::start(
                    pp_running, market, db, active_alerts, alert_history, strategy_enabled, attention,
                );
            }

            // 3b. Micro Pullback engine (stateful, per-ticker): watches every dormant
            //    low-float small cap in the premarket window for the first state change
            //    of the session (silence → 10s ignition → 30s confirmation) and fires
            //    one locked alert per ticker. Auto-started; idles outside premarket and
            //    honours the Settings toggle.
            {
                let mp_running       = state.micro_pullback_running.clone();
                let market           = state.market.clone();
                let db               = state.db.clone();
                let secrets          = state.secrets.clone();
                let active_alerts    = state.active_alerts.clone();
                let alert_history    = state.alert_history.clone();
                let strategy_enabled = state.strategy_enabled.clone();
                mp_running.store(true, std::sync::atomic::Ordering::Relaxed);
                micro_pullback::MicroPullbackEngine::start(
                    mp_running, market, db, secrets, active_alerts, alert_history, strategy_enabled,
                );
            }

            // 3c. Panic Mean Reversion watchlist scheduler: builds the day's two-list
            //     pre-open watchlist at 09:00 ET (premarket pre-filter + BB-area /
            //     move-since-SMA20 rankings), persisted for the scanner to surface.
            //     Runs immediately on a late launch; reuses a list already built today.
            {
                let pw_running = state.panic_watchlist_running.clone();
                let db         = state.db.clone();
                let secrets    = state.secrets.clone();
                pw_running.store(true, std::sync::atomic::Ordering::Relaxed);
                panic_watchlist::PanicWatchlistEngine::start(pw_running, db, secrets);
            }

            // 4. Internal trading loop: drives the order book (pending limit/stop
            //    fills, bracket SL/TP, TradeTally mirroring) off market data on a
            //    fixed cadence, instead of as a side effect of UI position polls.
            {
                let running       = state.trading_loop_running.clone();
                let market        = state.market.clone();
                let internal_book = state.internal_book.clone();
                let db            = state.db.clone();
                let config        = state.config.clone();
                let chart         = state.chart.clone();
                commands::spawn_trading_loop(running, market, internal_book, db, config, chart);
            }

            // 5. On-demand "capacité à diluer" collector: a background worker that
            //    refreshes a ticker's SEC dilution section the moment it surfaces on
            //    the premarket scanners (scanner calls company_intel::ondemand::request).
            //    Never blocks the scanner; interim until a real background worker exists.
            if let Some(rx) = crate::company_intel::ondemand::take_channel() {
                let (db, config) = (state.db.clone(), state.config.clone());
                tauri::async_runtime::spawn(crate::company_intel::ondemand::run_worker(db, config, rx));
            }

            // 6. Speech-to-Text worker: the single background task that drains the
            //    persisted dictée queue (VAD → whisper → Deepseek → trade note /
            //    diary). Pauses when the CPU is busy or the cash open is intense, so
            //    transcription never competes with the trading hot path.
            stt::worker::spawn(
                state.stt.clone(),
                app.handle().clone(),
                state.db.clone(),
                state.secrets.clone(),
                state.config.clone(),
                state.market.clone(),
            );

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // Status
            commands::get_app_status,
            // Config
            commands::get_local_config,
            commands::update_local_config,
            // Secrets
            commands::get_secrets_status,
            commands::update_secrets,
            // Journal tags (user-defined) + TradeTally queue
            commands::get_journal_tags,
            commands::get_sync_queue_status,
            commands::retry_tradetally_event,
            commands::retry_all_tradetally_events,
            // Journal
            commands::save_journal_entry,
            commands::get_journal_entry,
            // Screenshot
            commands::save_screenshot_local,
            // Logs
            commands::get_local_logs,
            // Bug reports (persisted)
            commands::get_bug_reports,
            commands::add_bug_report,
            commands::delete_bug_report,
            commands::clear_bug_reports,
            // Price alarms (persisted)
            commands::create_alarm,
            commands::get_alarms_for_symbol,
            commands::get_all_alarms,
            commands::delete_alarm,
            // Dev
            commands::get_mock_alerts,
            // Startup pipeline
            commands::run_startup_pipeline,
            commands::get_startup_status,
            commands::get_streamable_universe,
            // Live market feed
            commands::start_mock_market_feed,
            commands::stop_mock_market_feed,
            commands::start_live_feed,
            commands::stop_live_feed,
            commands::start_news_feed,
            commands::stop_news_feed,
            commands::set_focus_symbols,
            commands::get_market_snapshot,
            commands::get_ticker_bars,
            commands::load_chart_bars,
            commands::load_older_bars,
            commands::get_split_markers,
            commands::get_previous_day_levels,
            commands::get_latency_status,
            commands::get_feed_diagnostics,
            commands::get_news_diagnostics,
            // Scanner
            commands::get_strategies,
            commands::set_strategy_enabled,
            commands::set_strategy_risk,
            commands::get_strategy_cards,
            commands::start_alert_enrichment,
            commands::run_alert_llm,
            commands::get_alert_enrichment,
            commands::get_active_alerts,
            commands::get_alert_history,
            commands::get_screener_matches,
            commands::get_market_attention,
            commands::dismiss_screener,
            commands::get_screener_dismissals,
            commands::start_scanner,
            commands::stop_scanner,
            commands::get_mean_reversion_scores,
            commands::force_recompute_scores,
            commands::get_card_info,
            commands::get_ticker_news,
            commands::get_news_markers,
            // Company intelligence (read-only cache + background refresh trigger)
            commands::get_company_intel_catalog,
            commands::get_company_intel,
            commands::refresh_company_intel,
            commands::get_tickers_table,
            // Dashboard (moodboard)
            commands::sync_tradetally_trades,
            commands::get_dashboard_trades,
            commands::save_diary_entry,
            commands::get_daily_background,
            commands::open_backgrounds_folder,
            commands::get_mood,
            commands::open_mood_target,
            commands::get_default_dashboard,
            commands::export_dashboard_default,
            // Embedded TradeTally webview (native child webview over the tab)
            commands::tradetally_set_bounds,
            commands::tradetally_hide,
            // Chart / trade context
            commands::get_zone_trade_context,
            commands::create_or_get_trade_id_for_zone,
            commands::update_zone_sl,
            commands::update_zone_tp,
            commands::clear_zone_context,
            // Internal trading engine
            commands::create_internal_order_percent,
            commands::create_internal_market_order_percent,
            commands::cancel_internal_order,
            commands::close_internal_position,
            commands::get_internal_positions,
            commands::get_internal_orders,
            commands::get_trade_lifecycle,
            commands::get_executions_for_symbol,
            commands::create_drawing,
            commands::get_drawings_for_symbol,
            commands::update_drawing,
            commands::delete_drawing,
            commands::update_alarm_price,
            // Market Replay
            commands::replay_start,
            commands::replay_stop,
            commands::replay_set_playing,
            commands::replay_set_speed,
            commands::replay_seek_relative,
            commands::replay_seek_clock,
            commands::replay_next_alert,
            commands::replay_next_day,
            commands::get_replay_status,
            // Flat files (download / calendar / open folder)
            commands::flat_files_download,
            commands::flat_files_cancel,
            commands::get_flat_files_status,
            commands::get_flat_files_calendar,
            commands::open_flat_files_folder,
            // Speech-to-Text dictée pipeline
            commands::stt_status,
            commands::stt_download_model,
            commands::stt_start_recording,
            commands::stt_stop_recording,
            commands::stt_cancel_recording,
            commands::stt_cancel_job,
            commands::stt_retry_job,
            commands::stt_list_input_devices,
            commands::stt_test_microphone,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
