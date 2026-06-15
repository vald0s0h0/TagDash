pub mod alpaca;
pub mod chart_state;
pub mod chart_payloads;
pub mod commands;
pub mod config;
pub mod enrichment;
pub mod fmp;
pub mod internal_trading;
pub mod llm;
pub mod local_db;
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
        micro_pullback_running: Arc::new(AtomicBool::new(false)),
        panic_watchlist_running: Arc::new(AtomicBool::new(false)),
        trading_loop_running: Arc::new(AtomicBool::new(false)),
        strategy_enabled:  Arc::new(RwLock::new(strategy_enabled)),
        strategy_risk:     Arc::new(RwLock::new(strategy_risk)),
        active_alerts:     Arc::new(RwLock::new(Vec::new())),
        alert_history:     Arc::new(RwLock::new(Vec::new())),
        screener:          Arc::new(RwLock::new(Vec::new())),
        chart:             Arc::new(RwLock::new(chart_state)),
        internal_book:     Arc::new(RwLock::new(internal_book)),
        enrichments:       Arc::new(RwLock::new(std::collections::HashMap::new())),
        focus_symbols_tx,
        replay:            Arc::new(replay::ReplayShared::default()),
    };

    // ── Start Tauri ───────────────────────────────────────────────────────
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_notification::init())
        .manage(app_state)
        // Spawn background workers once the app is set up.
        .setup(|app| {
            let state = app.state::<AppState>();
            let db      = state.db.clone();
            let config  = state.config.clone();
            let secrets = state.secrets.clone();

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
                tauri::async_runtime::spawn(async move {
                    startup::run_pipeline(db.clone(), config.clone(), secrets.clone(), startup).await;
                    match commands::spawn_live_feed(market.clone(), config, secrets.clone(), db, live_feed_running, focus_rx, app_handle) {
                        Ok(n)  => eprintln!("[tagdash] live feed: {n} US-stock universe ready"),
                        Err(e) => eprintln!("[tagdash] live feed not started: {e}"),
                    }
                    // Premarket news investor — independent of the data feed.
                    if let Err(e) = commands::spawn_news_feed(market, secrets, news_feed_running) {
                        eprintln!("[tagdash] news feed not started: {e}");
                    }
                });
            }

            // 3. Perfect Pullback engine (stateful, multi-timeframe): watches every
            //    active ticker on 1/2/5/10m for a strong move (gate 1) then fires on
            //    the first healthy pullback (gate 2). Auto-started; idles outside the
            //    regular session and honours the Settings toggle.
            {
                let pp_running       = state.perfect_pullback_running.clone();
                let market           = state.market.clone();
                let db               = state.db.clone();
                let secrets          = state.secrets.clone();
                let active_alerts    = state.active_alerts.clone();
                let alert_history    = state.alert_history.clone();
                let strategy_enabled = state.strategy_enabled.clone();
                pp_running.store(true, std::sync::atomic::Ordering::Relaxed);
                perfect_pullback::PerfectPullbackEngine::start(
                    pp_running, market, db, secrets, active_alerts, alert_history, strategy_enabled,
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
            commands::dismiss_screener,
            commands::get_screener_dismissals,
            commands::start_scanner,
            commands::stop_scanner,
            commands::get_mean_reversion_scores,
            commands::force_recompute_scores,
            commands::get_card_info,
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
