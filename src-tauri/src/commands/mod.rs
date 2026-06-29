// Tauri command surface. Thin orchestration only — logic lives in modules.
// All state access via tauri::State<AppState>.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, RwLock};

use chrono::Utc;
use serde::Serialize;

use std::collections::HashMap;

use crate::chart_state::{ChartState, ZoneTradeContext};
use crate::config::{self, AppConfig};
use crate::config::secrets::{Secrets, SecretsStatus};
use crate::internal_trading::InternalBook;
use crate::local_db::{
    alarm_repository, book_repository, bug_repository, cache_repository, company_meta_repository,
    dashboard_repository, drawing_repository, execution_repository, get_recent_logs,
    journal_repository, universe_repository, BugReport, JournalEntry, LocalLogEntry, PriceAlarm,
    SyncQueueStatus, tradetally_queue_repository,
};
use crate::local_db::drawing_repository::Drawing;
use crate::market_state::{
    aggregators::Bar,
    FeedDiagnostics, MarketSnapshot, MarketState, NewsDiagnostics,
};
use crate::scanner::ScannerEngine;
use crate::startup::{StartupState, StreamableSymbol};
use crate::state::AppState;
use crate::strategies::registry;
use crate::tradetally;
use crate::types::{
    AlertEnrichment, AlertSignal, AttentionEntry, Fill, InternalOrder, LatencyStatus, Position,
    ScreenerMatch, Session, Side, Strategy, StrategyCard, TradeLifecycle,
};

// ─── Status ──────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct AppStatus {
    pub version: &'static str,
    pub backend: &'static str,
    pub latency: LatencyStatus,
}

#[tauri::command(rename_all = "snake_case")]
pub async fn get_app_status(state: tauri::State<'_, AppState>) -> Result<AppStatus, String> {
    let market = state.market.clone();
    tokio::task::spawn_blocking(move || {
        let latency = market.read().unwrap().latency.clone();
        AppStatus {
            version: env!("CARGO_PKG_VERSION"),
            backend: "rust-tauri",
            latency,
        }
    })
    .await
    .map_err(|e| e.to_string())
}

// ─── Config ──────────────────────────────────────────────────────────────────

#[tauri::command(rename_all = "snake_case")]
pub async fn get_local_config(state: tauri::State<'_, AppState>) -> Result<AppConfig, String> {
    let config = state.config.clone();
    tokio::task::spawn_blocking(move || config.read().unwrap().clone())
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn update_local_config(
    config: AppConfig,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let path = state.app_dir.join("tagdash.toml");
    let cfg_arc = state.config.clone();
    tokio::task::spawn_blocking(move || {
        config::save(&path, &config).map_err(|e| e.to_string())?;
        *cfg_arc.write().unwrap() = config;
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

// ─── Secrets (status only — values never leave Rust) ─────────────────────────

#[tauri::command(rename_all = "snake_case")]
pub async fn get_secrets_status(state: tauri::State<'_, AppState>) -> Result<SecretsStatus, String> {
    let secrets = state.secrets.clone();
    tokio::task::spawn_blocking(move || secrets.read().unwrap().status())
        .await
        .map_err(|e| e.to_string())
}

/// Set/replace API secrets from Settings → API Keys and persist them to
/// `tagdash.secrets.toml`. Only non-empty fields are applied (blank inputs leave
/// the existing value), so the user types just what they want to change. Returns
/// the refreshed status (booleans only — secret values never leave Rust).
#[tauri::command(rename_all = "snake_case")]
pub async fn update_secrets(
    updates: crate::config::secrets::SecretsUpdate,
    state: tauri::State<'_, AppState>,
) -> Result<SecretsStatus, String> {
    let secrets = state.secrets.clone();
    let app_dir = state.app_dir.clone();
    tokio::task::spawn_blocking(move || {
        let mut guard = secrets.write().unwrap();
        guard.apply_update(updates);
        config::secrets::save(&app_dir, &guard)?;
        Ok(guard.status())
    })
    .await
    .map_err(|e| e.to_string())?
}

// ─── Journal tags (user-defined, stored in tagdash.toml) ──────────────────────

#[tauri::command(rename_all = "snake_case")]
pub fn get_journal_tags(state: tauri::State<'_, AppState>) -> Vec<String> {
    state.config.read().unwrap().journal.tags.clone()
}

// ─── Sync queue ──────────────────────────────────────────────────────────────

#[tauri::command(rename_all = "snake_case")]
pub fn get_sync_queue_status(state: tauri::State<'_, AppState>) -> SyncQueueStatus {
    let db = state.db.lock().unwrap();
    tradetally_queue_repository::get_status(&db, 50).unwrap_or_default()
}

/// Reset a single failed event to pending so it is retried immediately.
#[tauri::command(rename_all = "snake_case")]
pub fn retry_tradetally_event(
    event_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let db = state.db.lock().unwrap();
    tradetally_queue_repository::reset_to_pending(&db, &event_id)
        .map_err(|e| e.to_string())
}

/// Reset ALL failed events to pending.
#[tauri::command(rename_all = "snake_case")]
pub fn retry_all_tradetally_events(state: tauri::State<'_, AppState>) -> Result<(), String> {
    let db = state.db.lock().unwrap();
    tradetally_queue_repository::reset_all_failed_to_pending(&db)
        .map_err(|e| e.to_string())
}

// ─── Journal ─────────────────────────────────────────────────────────────────

#[tauri::command(rename_all = "snake_case")]
pub fn save_journal_entry(
    trade_id:   String,
    symbol:     String,
    notes:      String,
    confidence: Option<i32>,
    tags:       Vec<String>,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let entry = JournalEntry { trade_id: trade_id.clone(), symbol: symbol.clone(), notes: notes.clone(), confidence, tags: tags.clone(), updated_at: String::new() };
    {
        let db = state.db.lock().unwrap();
        journal_repository::save(&db, &entry).map_err(|e| e.to_string())?;
        // Enqueue a note_updated event to TradeTally
        tradetally::enqueue_note_updated(&db, &trade_id, &symbol, &notes, confidence, &tags);
    }
    Ok(())
}

#[tauri::command(rename_all = "snake_case")]
pub fn get_journal_entry(
    trade_id: String,
    state: tauri::State<'_, AppState>,
) -> Option<JournalEntry> {
    let db = state.db.lock().unwrap();
    journal_repository::get(&db, &trade_id).unwrap_or(None)
}

// ─── Screenshot ──────────────────────────────────────────────────────────────

/// Save a base64-encoded PNG captured by the frontend.
/// If a tradeID is provided, the screenshot is linked and queued for TradeTally.
/// Returns the local file path.
#[tauri::command(rename_all = "snake_case")]
pub fn save_screenshot_local(
    zone_id:      String,
    trade_id:     Option<String>,
    image_base64: String,
    filename:     String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    // Decode + write to disk
    let local_path = crate::screenshot::save_to_disk(
        &state.app_dir, &filename, &image_base64,
    )?;

    // The screenshot is always saved locally. It is additionally uploaded to
    // TradeTally only when (a) session credentials are configured (the /images
    // route rejects the API token) and (b) the trade exists in TradeTally
    // (≥1 fill) so the {TT_ID} placeholder can resolve. Computed before the DB
    // lock to avoid nested locks.
    let tt: Option<(String, String)> = match &trade_id {
        Some(tid) if !tid.is_empty() => {
            let has_creds = {
                let s = state.secrets.read().unwrap();
                s.tradetally_email.as_deref().map(|e| !e.is_empty()).unwrap_or(false)
                    && s.tradetally_password.as_deref().map(|p| !p.is_empty()).unwrap_or(false)
            };
            let has_activity = state.internal_book.read().unwrap()
                .get_trade_lifecycle(tid).map(|lc| !lc.fills.is_empty()).unwrap_or(false);
            if has_creds && has_activity {
                let symbol = state.chart.read().unwrap()
                    .get_context_for_zone(&zone_id).map(|c| c.symbol).unwrap_or_default();
                Some((tid.clone(), symbol))
            } else {
                None
            }
        }
        _ => None,
    };

    // Record in screenshot_files table + (when eligible) queue the upload.
    {
        let db = state.db.lock().unwrap();
        db.execute(
            "INSERT OR IGNORE INTO screenshot_files (id, trade_id, path, uploaded, created_at)
             VALUES (?1, ?2, ?3, 0, datetime('now'))",
            rusqlite::params![&filename, &trade_id, &local_path],
        ).map_err(|e| e.to_string())?;
        if let Some((tid, symbol)) = &tt {
            tradetally::enqueue_chart_updated(&db, tid, symbol, &local_path);
        }
    }

    Ok(local_path)
}

// ─── Todo trades (missing screenshot / journal) ─────────────────────────────

#[derive(Debug, Serialize)]
pub struct TodoTrade {
    pub trade_id:       String,
    pub symbol:         String,
    pub open:           bool,
    pub pnl:            f64,
    pub has_screenshot: bool,
    pub has_journal:    bool,
}

#[tauri::command(rename_all = "snake_case")]
pub fn get_todo_trades(state: tauri::State<'_, AppState>) -> Vec<TodoTrade> {
    let db = state.db.lock().unwrap();
    let mut stmt = match db.prepare(
        "SELECT
             e.trade_id,
             e.symbol,
             SUM(e.quantity) AS net_qty,
             SUM(-CAST(e.quantity AS REAL) * e.fill_price) AS cash_flow,
             EXISTS(SELECT 1 FROM screenshot_files s WHERE s.trade_id = e.trade_id) AS has_ss,
             EXISTS(SELECT 1 FROM journal_entries j WHERE j.trade_id = e.trade_id AND j.notes != '') AS has_jr
         FROM executions e
         GROUP BY e.trade_id
         HAVING NOT has_ss OR NOT has_jr
         ORDER BY MIN(e.filled_at) DESC",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

    let prices: HashMap<String, f64> = all_market_prices(&state.market)
        .into_iter()
        .map(|(sym, (bid, ask))| (sym, (bid + ask) / 2.0))
        .collect();

    let rows = stmt.query_map([], |row| {
        let trade_id: String = row.get(0)?;
        let symbol: String   = row.get(1)?;
        let net_qty: i64     = row.get(2)?;
        let cash_flow: f64   = row.get(3)?;
        let has_ss: bool     = row.get(4)?;
        let has_jr: bool     = row.get(5)?;
        Ok((trade_id, symbol, net_qty, cash_flow, has_ss, has_jr))
    });

    let mut out = Vec::new();
    if let Ok(rows) = rows {
        for r in rows.flatten() {
            let (trade_id, symbol, net_qty, cash_flow, has_ss, has_jr) = r;
            let open = net_qty != 0;
            let pnl = if open {
                let mid = prices.get(&symbol).copied().unwrap_or(0.0);
                cash_flow + (net_qty as f64) * mid
            } else {
                cash_flow
            };
            out.push(TodoTrade {
                trade_id,
                symbol,
                open,
                pnl,
                has_screenshot: has_ss,
                has_journal: has_jr,
            });
        }
    }
    out
}

// ─── All trades DB view ─────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct TradeDbRow {
    pub trade_id:       String,
    pub symbol:         String,
    pub side:           String,
    pub open:           bool,
    pub pnl:            f64,
    pub fills:          i64,
    pub first_fill_at:  String,
    pub last_fill_at:   String,
    pub has_note:       bool,
    pub has_screenshot: bool,
    pub sent_to_tradetally: bool,
    pub synced_on_tradetally: bool,
}

#[tauri::command(rename_all = "snake_case")]
pub fn get_all_trades_db(state: tauri::State<'_, AppState>) -> Vec<TradeDbRow> {
    let db = state.db.lock().unwrap();
    let mut stmt = match db.prepare(
        "SELECT
             e.trade_id,
             e.symbol,
             SUM(e.quantity) AS net_qty,
             SUM(-CAST(e.quantity AS REAL) * e.fill_price) AS cash_flow,
             COUNT(*) AS fill_count,
             MIN(e.filled_at) AS first_fill,
             MAX(e.filled_at) AS last_fill,
             EXISTS(SELECT 1 FROM journal_entries j WHERE j.trade_id = e.trade_id AND j.notes != '') AS has_note,
             EXISTS(SELECT 1 FROM screenshot_files s WHERE s.trade_id = e.trade_id) AS has_ss,
             EXISTS(SELECT 1 FROM tradetally_sync_queue q WHERE q.trade_id = e.trade_id AND q.event_type = 'trade_created') AS sent_tt,
             EXISTS(SELECT 1 FROM tradetally_trade_ids t WHERE t.local_trade_id = e.trade_id) AS synced_tt
         FROM executions e
         GROUP BY e.trade_id
         ORDER BY MIN(e.filled_at) DESC",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

    let prices: HashMap<String, f64> = all_market_prices(&state.market)
        .into_iter()
        .map(|(sym, (bid, ask))| (sym, (bid + ask) / 2.0))
        .collect();

    let rows = stmt.query_map([], |row| {
        let trade_id: String = row.get(0)?;
        let symbol: String   = row.get(1)?;
        let net_qty: i64     = row.get(2)?;
        let cash_flow: f64   = row.get(3)?;
        let fill_count: i64  = row.get(4)?;
        let first_fill: String = row.get(5)?;
        let last_fill: String  = row.get(6)?;
        let has_note: bool   = row.get(7)?;
        let has_ss: bool     = row.get(8)?;
        let sent_tt: bool    = row.get(9)?;
        let synced_tt: bool  = row.get(10)?;
        Ok((trade_id, symbol, net_qty, cash_flow, fill_count, first_fill, last_fill, has_note, has_ss, sent_tt, synced_tt))
    });

    let mut out = Vec::new();
    if let Ok(rows) = rows {
        for r in rows.flatten() {
            let (trade_id, symbol, net_qty, cash_flow, fill_count, first_fill, last_fill, has_note, has_ss, sent_tt, synced_tt) = r;
            let open = net_qty != 0;
            let side = if net_qty > 0 { "long" } else if net_qty < 0 { "short" } else {
                // Closed: infer from first fill direction
                "closed"
            };
            let pnl = if open {
                let mid = prices.get(&symbol).copied().unwrap_or(0.0);
                cash_flow + (net_qty as f64) * mid
            } else {
                cash_flow
            };
            out.push(TradeDbRow {
                trade_id,
                symbol,
                side: side.to_string(),
                open,
                pnl,
                fills: fill_count,
                first_fill_at: first_fill,
                last_fill_at: last_fill,
                has_note,
                has_screenshot: has_ss,
                sent_to_tradetally: sent_tt,
                synced_on_tradetally: synced_tt,
            });
        }
    }
    out
}

#[tauri::command(rename_all = "snake_case")]
pub fn get_trade_days(state: tauri::State<'_, AppState>) -> Vec<String> {
    let db = state.db.lock().unwrap();
    let mut stmt = match db.prepare(
        "SELECT DISTINCT date(filled_at) AS d FROM executions ORDER BY d DESC",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    stmt.query_map([], |r| r.get::<_, String>(0))
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
}

#[tauri::command(rename_all = "snake_case")]
pub fn delete_trade_db(
    trade_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let db = state.db.lock().unwrap();
    db.execute("DELETE FROM executions WHERE trade_id = ?1", rusqlite::params![&trade_id])
        .map_err(|e| e.to_string())?;
    db.execute("DELETE FROM trade_levels WHERE trade_id = ?1", rusqlite::params![&trade_id])
        .map_err(|e| e.to_string())?;
    db.execute("DELETE FROM journal_entries WHERE trade_id = ?1", rusqlite::params![&trade_id])
        .map_err(|e| e.to_string())?;
    db.execute("DELETE FROM screenshot_files WHERE trade_id = ?1", rusqlite::params![&trade_id])
        .map_err(|e| e.to_string())?;
    db.execute("DELETE FROM tradetally_sync_queue WHERE trade_id = ?1", rusqlite::params![&trade_id])
        .map_err(|e| e.to_string())?;
    Ok(())
}

// ─── Local logs ──────────────────────────────────────────────────────────────

#[tauri::command(rename_all = "snake_case")]
pub fn get_local_logs(
    limit: Option<u32>,
    state: tauri::State<'_, AppState>,
) -> Vec<LocalLogEntry> {
    let db = state.db.lock().unwrap();
    get_recent_logs(&db, limit.unwrap_or(100)).unwrap_or_default()
}

// ─── Bug reports (persisted) ──────────────────────────────────────────────────

#[tauri::command(rename_all = "snake_case")]
pub fn get_bug_reports(state: tauri::State<'_, AppState>) -> Vec<BugReport> {
    let db = state.db.lock().unwrap();
    bug_repository::get_all(&db).unwrap_or_default()
}

#[tauri::command(rename_all = "snake_case")]
pub fn add_bug_report(
    id:       String,
    text:     String,
    priority: i64,
    state:    tauri::State<'_, AppState>,
) -> Result<Vec<BugReport>, String> {
    let db = state.db.lock().unwrap();
    bug_repository::insert(&db, &id, &text, priority).map_err(|e| e.to_string())?;
    bug_repository::get_all(&db).map_err(|e| e.to_string())
}

#[tauri::command(rename_all = "snake_case")]
pub fn delete_bug_report(
    id:    String,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<BugReport>, String> {
    let db = state.db.lock().unwrap();
    bug_repository::delete(&db, &id).map_err(|e| e.to_string())?;
    bug_repository::get_all(&db).map_err(|e| e.to_string())
}

#[tauri::command(rename_all = "snake_case")]
pub fn clear_bug_reports(state: tauri::State<'_, AppState>) -> Result<Vec<BugReport>, String> {
    let db = state.db.lock().unwrap();
    bug_repository::clear_all(&db).map_err(|e| e.to_string())?;
    Ok(vec![])
}

// ─── Price alarms (persisted; level-crossing watcher lives in the scanner) ────

#[tauri::command(rename_all = "snake_case")]
pub fn create_alarm(
    id:          String,
    symbol:      String,
    strategy_id: Option<String>,
    price:       f64,
    state:       tauri::State<'_, AppState>,
) -> Result<PriceAlarm, String> {
    let db = state.db.lock().unwrap();
    alarm_repository::insert(&db, &id, &symbol, strategy_id.as_deref(), price)
        .map_err(|e| e.to_string())
}

#[tauri::command(rename_all = "snake_case")]
pub fn get_alarms_for_symbol(
    symbol: String,
    state:  tauri::State<'_, AppState>,
) -> Vec<PriceAlarm> {
    let db = state.db.lock().unwrap();
    alarm_repository::get_for_symbol(&db, &symbol).unwrap_or_default()
}

/// A stored alarm enriched with its strategy's display name and priority
/// (derived from the registry, mirroring the alarm watcher's defaults). Lets
/// the sidebar render a priority badge without the frontend knowing strategies.
#[derive(serde::Serialize)]
pub struct AlarmView {
    pub id:            String,
    pub symbol:        String,
    pub strategy_id:   Option<String>,
    pub strategy_name: String,
    pub priority:      u8,
    pub price:         f64,
    pub created_at:    String,
    pub triggered_at:  Option<String>,
}

#[tauri::command(rename_all = "snake_case")]
pub fn get_all_alarms(state: tauri::State<'_, AppState>) -> Vec<AlarmView> {
    let db = state.db.lock().unwrap();
    let alarms = alarm_repository::get_all(&db).unwrap_or_default();
    alarms
        .into_iter()
        .map(|a| {
            let (name, priority) = a
                .strategy_id
                .as_deref()
                .and_then(|sid| {
                    registry::all_strategies()
                        .iter()
                        .find(|s| s.id() == sid)
                        .map(|s| (s.name().to_string(), s.priority()))
                })
                .unwrap_or_else(|| ("Alarme".to_string(), 5));
            AlarmView {
                id:            a.id,
                symbol:        a.symbol,
                strategy_id:   a.strategy_id,
                strategy_name: name,
                priority,
                price:         a.price,
                created_at:    a.created_at,
                triggered_at:  a.triggered_at,
            }
        })
        .collect()
}

/// Move an alarm to a new price (chart-drag from the chart area).
#[tauri::command(rename_all = "snake_case")]
pub fn update_alarm_price(
    id:    String,
    price: f64,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let db = state.db.lock().unwrap();
    alarm_repository::update_price(&db, &id, price).map_err(|e| e.to_string())
}

#[tauri::command(rename_all = "snake_case")]
pub fn delete_alarm(id: String, state: tauri::State<'_, AppState>) -> Result<(), String> {
    let db = state.db.lock().unwrap();
    alarm_repository::delete(&db, &id).map_err(|e| e.to_string())
}

// ─── Startup pipeline ────────────────────────────────────────────────────────

#[tauri::command(rename_all = "snake_case")]
pub async fn run_startup_pipeline(state: tauri::State<'_, AppState>) -> Result<(), String> {
    // The pipeline writes the daily cache from "now"-relative windows — running
    // it under the simulated clock would store a truncated history.
    if crate::replay::clock::is_active() {
        return Err("Market Replay actif — termine le replay avant de relancer le pipeline".into());
    }
    // Reset the step list to the active mode's set and run the matching pipeline.
    let ds = state.config.read().unwrap().data_source.clone();
    let is_flat = ds.is_flat_files();
    {
        let mut s = state.startup.write().unwrap();
        *s = StartupState::for_mode(&ds);
    }

    let db      = state.db.clone();
    let config  = state.config.clone();
    let secrets = state.secrets.clone();
    let startup = state.startup.clone();
    let app_dir = state.app_dir.clone();

    tokio::spawn(async move {
        if is_flat {
            crate::startup::run_pipeline_flat_files(db, config, secrets, startup, app_dir).await;
        } else {
            crate::startup::run_pipeline(db, config, secrets, startup).await;
        }
    });

    Ok(())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn get_startup_status(state: tauri::State<'_, AppState>) -> Result<StartupState, String> {
    let startup = state.startup.clone();
    tokio::task::spawn_blocking(move || startup.read().unwrap().clone())
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn get_streamable_universe(state: tauri::State<'_, AppState>) -> Result<Vec<StreamableSymbol>, String> {
    let db = state.db.clone();
    tokio::task::spawn_blocking(move || {
        let db = db.lock().unwrap();
        let assets = universe_repository::get_all(&db).unwrap_or_default();
        let meta: HashMap<String, (Option<String>, Option<String>)> =
            company_meta_repository::get_all(&db)
                .unwrap_or_default()
                .into_iter()
                .map(|m| (m.symbol, (m.country, m.industry)))
                .collect();
        assets
            .into_iter()
            .filter(|a| a.tradable)
            .map(|a| {
                let (country, industry) = meta.get(&a.symbol).cloned().unwrap_or((None, None));
                StreamableSymbol {
                    symbol:      a.symbol,
                    exchange:    a.exchange,
                    tradable:    a.tradable,
                    shortable:   a.shortable,
                    float_shares: a.float_shares,
                    market_cap:  a.market_cap,
                    avg_volume:  a.avg_volume,
                    country,
                    industry,
                }
            })
            .collect()
    })
    .await
    .map_err(|e| e.to_string())
}

// ─── Mock alerts (dev / test only) ───────────────────────────────────────────

#[tauri::command(rename_all = "snake_case")]
pub fn get_mock_alerts() -> Vec<AlertSignal> {
    let now = Utc::now();
    vec![
        AlertSignal {
            alert_id:       "mock-1".into(),
            timestamp:      now,
            symbol:         "ABCD".into(),
            strategy_id:    "premarket_frd_runner".into(),
            strategy_name:  "Premarket FRD Runner".into(),
            priority:       4,
            session:        Session::Premarket,
            price:          Some(3.42),
            bid:            Some(3.41),
            ask:            Some(3.43),
            spread:         Some(0.02),
            volume:         Some(1_250_000),
            rvol:           Some(6.2),
            change_day_pct: Some(48.7),
            float_shares:   Some(8_500_000),
            news_today:     true,
            halted:         Some(false),
            latency_ui_ms:  Some(180),
            reason:         "Premarket FRD runner: RVOL>5 + news + small float".into(),
            display_timeframe: None,
            side:           None,
        },
        AlertSignal {
            alert_id:       "mock-2".into(),
            timestamp:      now,
            symbol:         "WXYZ".into(),
            strategy_id:    "open_hod_breakout".into(),
            strategy_name:  "Open HOD Breakout".into(),
            priority:       3,
            session:        Session::Open,
            price:          Some(7.18),
            bid:            Some(7.17),
            ask:            Some(7.19),
            spread:         Some(0.02),
            volume:         Some(3_400_000),
            rvol:           Some(3.1),
            change_day_pct: Some(12.4),
            float_shares:   Some(22_000_000),
            news_today:     false,
            halted:         Some(false),
            latency_ui_ms:  Some(210),
            reason:         "Open HOD breakout with volume confirmation".into(),
            display_timeframe: None,
            side:           None,
        },
    ]
}

// ─── Live market feed ─────────────────────────────────────────────────────────

#[tauri::command(rename_all = "snake_case")]
pub async fn start_mock_market_feed(state: tauri::State<'_, AppState>) -> Result<(), String> {
    if state.mock_feed_running.load(Ordering::Relaxed) {
        return Ok(());
    }
    state.mock_feed_running.store(true, Ordering::Relaxed);
    {
        let mut ms = state.market.write().unwrap();
        ms.mock_running = true;
    }
    let market         = state.market.clone();
    let config         = state.config.clone();
    let running        = state.mock_feed_running.clone();
    let market_cleanup = market.clone();
    tokio::spawn(async move {
        crate::market_state::mock_feed::run(market, config, running).await;
        market_cleanup.write().unwrap().mock_running = false;
    });
    Ok(())
}

#[tauri::command(rename_all = "snake_case")]
pub fn stop_mock_market_feed(state: tauri::State<'_, AppState>) -> Result<(), String> {
    state.mock_feed_running.store(false, Ordering::Relaxed);
    Ok(())
}

/// Seed previous closes and spawn the Alpaca live WebSocket feed. The actual
/// subscribed symbol set is resolved inside the stream task from `universe_rx`
/// (which follows the active session). Shared by the `start_live_feed` command
/// and the launch-time auto-start in `lib.rs`. Must be called from within a
/// tokio runtime context. Returns the US Stocks universe size.
pub fn spawn_live_feed(
    market:      Arc<RwLock<MarketState>>,
    config:      Arc<RwLock<AppConfig>>,
    secrets:     Arc<RwLock<Secrets>>,
    db:          Arc<Mutex<rusqlite::Connection>>,
    running:     Arc<std::sync::atomic::AtomicBool>,
    focus_rx:    tokio::sync::watch::Receiver<Vec<String>>,
    app:         tauri::AppHandle,
) -> Result<usize, String> {
    if running.load(Ordering::Relaxed) {
        return Ok(0);
    }
    // While a Market Replay is active the live feed must stay down — it would
    // mix real-time data into the simulated MarketState. (The replay engine
    // restarts the feed itself after deactivating the simulated clock.)
    if crate::replay::clock::is_active() {
        return Err("Market Replay actif — flux live indisponible".into());
    }

    // Flat-files mode never uses the live Alpaca feed (the API may be unavailable
    // entirely): the platform runs off the downloaded days via Market Replay. This
    // guard also keeps the replay-stop cleanup from resurrecting the feed here.
    if config.read().unwrap().data_source.is_flat_files() {
        return Err("mode flat files — flux live désactivé".into());
    }

    let (key, secret) = {
        let s = secrets.read().unwrap();
        match (s.alpaca_key.clone(), s.alpaca_secret.clone()) {
            (Some(k), Some(sec)) if !k.is_empty() && !sec.is_empty() => (k, sec),
            _ => return Err("Alpaca keys not configured".into()),
        }
    };

    let (feed, warn_ms, critical_ms) = {
        let c = config.read().unwrap();
        (c.alpaca.feed.clone(), c.latency.warn_ms, c.latency.critical_ms)
    };

    // Universe size + previous closes from the cache (no await under the lock).
    let (active_count, closes) = {
        let conn = db.lock().unwrap();
        let count = universe_repository::get_active_symbols(&conn)
            .map(|v| v.len())
            .unwrap_or(0);
        let closes = cache_repository::latest_closes(&conn).unwrap_or_default();
        (count, closes)
    };

    if active_count == 0 {
        return Err("streamable universe is empty — run the startup pipeline first".into());
    }

    // Seed previous closes so change% is meaningful from the first trade.
    {
        let now = Utc::now();
        let mut ms = market.write().unwrap();
        for (sym, close) in &closes {
            ms.set_previous_close(sym, *close, now);
        }
    }

    running.store(true, Ordering::Relaxed);
    tokio::spawn(async move {
        crate::alpaca::stream::run(
            market, db, feed, key, secret, warn_ms, critical_ms, running, focus_rx, app,
        )
        .await;
    });
    Ok(active_count)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn start_live_feed(
    app:   tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    spawn_live_feed(
        state.market.clone(),
        state.config.clone(),
        state.secrets.clone(),
        state.db.clone(),
        state.live_feed_running.clone(),
        state.focus_symbols_tx.subscribe(),
        app,
    )?;
    Ok(())
}

#[tauri::command(rename_all = "snake_case")]
pub fn stop_live_feed(state: tauri::State<'_, AppState>) -> Result<(), String> {
    state.live_feed_running.store(false, Ordering::Relaxed);
    Ok(())
}

/// Spawn the Alpaca news WebSocket feed (premarket news investor). It connects
/// only during the premarket window and idles otherwise. Shared by the
/// `start_news_feed` command and the launch-time auto-start in `lib.rs`. Must be
/// called from within a tokio runtime context.
pub fn spawn_news_feed(
    market:  Arc<RwLock<MarketState>>,
    secrets: Arc<RwLock<Secrets>>,
    running: Arc<std::sync::atomic::AtomicBool>,
) -> Result<(), String> {
    if running.load(Ordering::Relaxed) {
        return Ok(());
    }
    // Same guard as the data feed: no live news into a simulated session.
    if crate::replay::clock::is_active() {
        return Err("Market Replay actif — flux news indisponible".into());
    }
    let (key, secret) = {
        let s = secrets.read().unwrap();
        match (s.alpaca_key.clone(), s.alpaca_secret.clone()) {
            (Some(k), Some(sec)) if !k.is_empty() && !sec.is_empty() => (k, sec),
            _ => return Err("Alpaca keys not configured".into()),
        }
    };
    running.store(true, Ordering::Relaxed);
    let running_task = running.clone();
    tokio::spawn(async move {
        crate::alpaca::news_stream::run(market, key, secret, running_task).await;
    });
    Ok(())
}

#[tauri::command(rename_all = "snake_case")]
pub fn start_news_feed(state: tauri::State<'_, AppState>) -> Result<(), String> {
    spawn_news_feed(
        state.market.clone(),
        state.secrets.clone(),
        state.news_feed_running.clone(),
    )
}

#[tauri::command(rename_all = "snake_case")]
pub fn stop_news_feed(state: tauri::State<'_, AppState>) -> Result<(), String> {
    state.news_feed_running.store(false, Ordering::Relaxed);
    Ok(())
}

/// Set the symbols currently displayed in chart zones. The live feed tick-streams
/// these (trades+quotes) on top of the broad surveillance tier, and pushes their
/// ticks to the frontend. Persists even if the feed isn't connected yet.
#[tauri::command(rename_all = "snake_case")]
pub fn set_focus_symbols(
    symbols: Vec<String>,
    state:   tauri::State<'_, AppState>,
) -> Result<(), String> {
    state.focus_symbols_tx.send_replace(symbols);
    Ok(())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn get_market_snapshot(state: tauri::State<'_, AppState>) -> Result<MarketSnapshot, String> {
    let market = state.market.clone();
    tokio::task::spawn_blocking(move || market.read().unwrap().snapshot())
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn get_ticker_bars(
    symbol:    String,
    timeframe: String,
    state:     tauri::State<'_, AppState>,
) -> Result<Vec<Bar>, String> {
    let tf = match crate::market_state::aggregators::Timeframe::from_str(&timeframe) {
        Some(tf) => tf,
        None     => return Ok(vec![]),
    };
    let market = state.market.clone();
    tokio::task::spawn_blocking(move || market.read().unwrap().get_bars(&symbol, tf))
        .await
        .map_err(|e| e.to_string())
}

/// Convert a cached daily bar (the offline daily source loaded from the flat files
/// at startup) into a chart `Bar`, anchored at 00:00:00 UTC of its date. `None`
/// when the OHLC is incomplete (a NULL-padded cache row).
fn daily_cache_to_bar(d: cache_repository::DailyBar) -> Option<Bar> {
    use chrono::{NaiveDate, TimeZone, Utc};
    let nd = NaiveDate::parse_from_str(d.date.get(..10)?, "%Y-%m-%d").ok()?;
    let time = Utc.from_utc_datetime(&nd.and_hms_opt(0, 0, 0)?);
    Some(Bar {
        time,
        open: d.open?,
        high: d.high?,
        low: d.low?,
        close: d.close?,
        volume: d.volume.unwrap_or(0).max(0) as u64,
        vwap: None,
        trade_count: None,
    })
}

/// Single, unified entry point for loading a chart's bars — used by every pane,
/// every strategy, every timeframe. On each call it refreshes the (symbol,
/// timeframe) history straight from Alpaca: this fills any gaps and pulls the
/// still-forming session bar (notably today's daily bar, which the startup cache
/// misses), then merges the authoritative bars into the RAM ring buffer (where
/// Alpaca wins over stale closed bars) and returns the full series. Sub-minute
/// timeframes (5s/10s) that Alpaca's REST bars can't serve — and the mock /
/// no-credential case — fall back to whatever RAM already holds.
#[tauri::command(rename_all = "snake_case")]
pub async fn load_chart_bars(
    symbol:    String,
    timeframe: String,
    state:     tauri::State<'_, AppState>,
) -> Result<Vec<Bar>, String> {
    use crate::market_state::aggregators::Timeframe;

    let tf = match Timeframe::from_str(&timeframe) {
        Some(tf) => tf,
        None     => return Ok(vec![]),
    };

    // A timeframe Alpaca's REST bars don't serve (5s/10s) → return the live RAM
    // series (built from trade ticks) without hitting the network.
    if crate::alpaca::bars::alpaca_timeframe(tf).is_none() {
        return Ok(state.market.read().unwrap().get_bars(&symbol, tf));
    }

    // Flat-files mode: no live API — serve straight from disk, tolerating missing
    // days/symbols (the chart renders the gap).
    if state.config.read().unwrap().data_source.is_flat_files() {
        let limit = if tf == Timeframe::Daily { 600 } else { 400 };
        if tf == Timeframe::Daily {
            // Daily history from the cache, clamped to strictly before the current
            // (simulated, during replay) day so an active replay never reveals the
            // future — same bound as get_previous_day_levels. The forming day shows
            // on the intraday panes.
            let clamp = crate::time::et_date(crate::time::now());
            let conn = state.db.lock().unwrap();
            let mut v: Vec<Bar> = cache_repository::get_daily_bars_before(&conn, &symbol, &clamp, limit)
                .unwrap_or_default()
                .into_iter()
                .filter_map(daily_cache_to_bar)
                .collect();
            v.reverse(); // DESC → ascending for the chart
            return Ok(v);
        }
        // Intraday: while a replay drives the sim clock, return the RAM series it is
        // feeding (already bounded to the sim instant — no look-ahead); otherwise read
        // the static minute files for offline browsing.
        if crate::replay::clock::is_active() {
            return Ok(state.market.read().unwrap().get_bars(&symbol, tf));
        }
        return Ok(crate::flat_files::minute::read_symbol_bars(
            &state.app_dir, &symbol, tf, limit as usize, None,
        ));
    }

    let (key, secret) = {
        let s = state.secrets.read().unwrap();
        (s.alpaca_key.clone(), s.alpaca_secret.clone())
    };
    let (Some(key), Some(secret)) = (key, secret) else {
        // No credentials (e.g. mock mode) — return whatever RAM holds.
        return Ok(state.market.read().unwrap().get_bars(&symbol, tf));
    };

    // Daily shows up to a full ring of history (Bollinger + visual depth); intraday
    // loads a few sessions up front so a first zoom-out has bars to show before the
    // lazy back-fill kicks in (it fills the rest as the user scrolls further back).
    let limit = if tf == Timeframe::Daily { 600 } else { 400 };

    match crate::alpaca::bars::fetch_recent_bars(&key, &secret, &symbol, tf, limit).await {
        Ok(history) => {
            {
                let mut market = state.market.write().unwrap();
                market.merge_history_bars(&symbol, tf, history);
            }
            // Daily chart during premarket: seed the provisional premarket candle from
            // Alpaca 04:00→now minute bars so it shows the full premarket range (not
            // just the slice since the feed warmed up). Dropped at the 09:30 open by
            // the regular-session path. Awaited with no lock held.
            if tf == Timeframe::Daily && crate::time::is_premarket(crate::time::now()) {
                match crate::alpaca::bars::fetch_premarket_daily_bar(&key, &secret, &symbol).await {
                    Ok(Some(bar)) => state.market.write().unwrap().seed_premarket_daily(&symbol, bar),
                    Ok(None) => {}
                    Err(e) => eprintln!("[tagdash] premarket daily bar {symbol} failed: {e}"),
                }
            }
            Ok(state.market.read().unwrap().get_bars(&symbol, tf))
        }
        Err(e) => {
            eprintln!("[tagdash] load_chart_bars {symbol} {timeframe} failed: {e}");
            // Soft-fail: still return whatever RAM holds so the chart renders.
            Ok(state.market.read().unwrap().get_bars(&symbol, tf))
        }
    }
}

/// Lazily back-fill OLDER chart history: returns up to `limit` bars of
/// (symbol, timeframe) ending before `before` (RFC3339), oldest → newest,
/// straight from Alpaca. Called by the chart when the user scrolls/zooms into the
/// past and hits the left edge of what's loaded, so the blank fills in. Batched
/// (e.g. 500 bars/call). Frontend-only history — not merged into the capped RAM
/// ring. Empty for sub-minute timeframes Alpaca's REST bars can't serve.
#[tauri::command(rename_all = "snake_case")]
pub async fn load_older_bars(
    symbol:    String,
    timeframe: String,
    before:    String,
    limit:     u32,
    state:     tauri::State<'_, AppState>,
) -> Result<Vec<Bar>, String> {
    use crate::market_state::aggregators::Timeframe;

    let tf = match Timeframe::from_str(&timeframe) {
        Some(tf) => tf,
        None     => return Ok(vec![]),
    };
    if crate::alpaca::bars::alpaca_timeframe(tf).is_none() {
        return Ok(vec![]);
    }

    // Flat-files mode: back-fill older history straight from disk (daily from the
    // daily_cache; intraday from the minute files), bounded to before `before`.
    if state.config.read().unwrap().data_source.is_flat_files() {
        let limit = limit.clamp(1, 1000);
        if tf == Timeframe::Daily {
            let before_date = before.get(..10).unwrap_or("").to_string();
            let conn = state.db.lock().unwrap();
            let mut v: Vec<Bar> = cache_repository::get_daily_bars_before(&conn, &symbol, &before_date, limit)
                .unwrap_or_default()
                .into_iter()
                .filter_map(daily_cache_to_bar)
                .collect();
            v.reverse(); // DESC → ascending
            return Ok(v);
        }
        let before_ms = chrono::DateTime::parse_from_rfc3339(&before).ok().map(|d| d.timestamp_millis());
        return Ok(crate::flat_files::minute::read_symbol_bars(
            &state.app_dir, &symbol, tf, limit as usize, before_ms,
        ));
    }

    let (key, secret) = {
        let s = state.secrets.read().unwrap();
        (s.alpaca_key.clone(), s.alpaca_secret.clone())
    };
    let (Some(key), Some(secret)) = (key, secret) else {
        return Ok(vec![]);
    };

    let limit = limit.clamp(1, 1000);
    match crate::alpaca::bars::fetch_bars_before(&key, &secret, &symbol, tf, &before, limit).await {
        Ok(bars) => Ok(bars),
        Err(e) => {
            eprintln!("[tagdash] load_older_bars {symbol} {timeframe} failed: {e}");
            Ok(vec![])
        }
    }
}

/// Historical stock-split day markers for ONE symbol over the last 2 years
/// (Alpaca corporate-actions) — red dots on the daily pane. Surfaced for EVERY
/// daily chart (not only enriched alerts, which got them via the enrichment
/// payload): the daily pane fetches this directly. We only need the ex-dates (the
/// day the price adjusts); the ratio is kept solely as the marker label. Returns
/// an empty list on missing credentials / fetch error so the chart still renders.
#[tauri::command(rename_all = "snake_case")]
pub async fn get_split_markers(
    symbol: String,
    state:  tauri::State<'_, AppState>,
) -> Result<Vec<crate::types::SplitMarker>, String> {
    use chrono::{NaiveDate, TimeZone, Utc};

    // Flat-files mode: read the stored split events (ticker_splits) instead of the
    // Alpaca corporate-actions API, so markers still show offline.
    if state.config.read().unwrap().data_source.is_flat_files() {
        let since = (Utc::now() - chrono::Duration::days(365 * 2)).format("%Y-%m-%d").to_string();
        let conn = state.db.lock().unwrap();
        let rows = cache_repository::splits_for_symbol(&conn, &symbol, &since).unwrap_or_default();
        return Ok(rows
            .into_iter()
            .filter_map(|(ex_date, label)| {
                let d = NaiveDate::parse_from_str(&ex_date, "%Y-%m-%d").ok()?;
                let time = Utc.from_utc_datetime(&d.and_hms_opt(0, 0, 0)?).timestamp();
                Some(crate::types::SplitMarker { time, label })
            })
            .collect());
    }

    let (key, secret) = {
        let s = state.secrets.read().unwrap();
        (s.alpaca_key.clone(), s.alpaca_secret.clone())
    };
    let (Some(key), Some(secret)) = (key, secret) else {
        return Ok(vec![]); // no credentials (e.g. mock mode)
    };

    match crate::alpaca::corporate_actions::fetch_splits(&key, &secret, &symbol, 2).await {
        Ok(splits) => Ok(splits
            .into_iter()
            .filter_map(|s| {
                // Stamp the marker at UTC midnight of the ex-date; the chart snaps
                // it to the nearest daily bar (which carries Alpaca's own stamp).
                let d = NaiveDate::parse_from_str(&s.date, "%Y-%m-%d").ok()?;
                let time = Utc.from_utc_datetime(&d.and_hms_opt(0, 0, 0)?).timestamp();
                Some(crate::types::SplitMarker { time, label: s.label })
            })
            .collect()),
        Err(e) => {
            eprintln!("[tagdash] get_split_markers {symbol} failed: {e}");
            Ok(vec![])
        }
    }
}

/// Previous trading day's reference levels (close / high / low) for a symbol,
/// relative to TODAY's date (ET). Drawn as the PDC/PDH/PDL lines on intraday
/// panes. Sourced from the daily cache and filtered to the most recent bar whose
/// date is strictly before today, so a cached (possibly partial) bar for the
/// current session is never mistaken for "yesterday".
#[derive(serde::Serialize)]
pub struct PrevDayLevels {
    pub date:  String,
    pub close: f64,
    pub high:  f64,
    pub low:   f64,
}

#[tauri::command(rename_all = "snake_case")]
pub async fn get_previous_day_levels(
    symbol: String,
    state:  tauri::State<'_, AppState>,
) -> Result<Option<PrevDayLevels>, String> {
    let db = state.db.clone();
    tokio::task::spawn_blocking(move || {
        let today = crate::time::et_date(crate::time::now());
        let rows = {
            let conn = db.lock().unwrap();
            cache_repository::get_daily_bars_before(&conn, &symbol, &today, 10).unwrap_or_default()
        };
        rows.into_iter().find_map(|d| {
            let date = d.date.get(..10).unwrap_or(&d.date);
            if date >= today.as_str() {
                return None;
            }
            Some(PrevDayLevels {
                date:  date.to_string(),
                close: d.close?,
                high:  d.high?,
                low:   d.low?,
            })
        })
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn get_latency_status(state: tauri::State<'_, AppState>) -> Result<LatencyStatus, String> {
    let market = state.market.clone();
    tokio::task::spawn_blocking(move || market.read().unwrap().latency.clone())
        .await
        .map_err(|e| e.to_string())
}

/// Live Alpaca feed health (connection state, subscribed count, trade/quote
/// counters, last error, reconnects) for the diagnostics panel.
#[tauri::command(rename_all = "snake_case")]
pub async fn get_feed_diagnostics(state: tauri::State<'_, AppState>) -> Result<FeedDiagnostics, String> {
    let market = state.market.clone();
    tokio::task::spawn_blocking(move || market.read().unwrap().feed.clone())
        .await
        .map_err(|e| e.to_string())
}

/// Alpaca news feed health + recent headlines (premarket news investor), for the
/// news debug panel.
#[tauri::command(rename_all = "snake_case")]
pub async fn get_news_diagnostics(state: tauri::State<'_, AppState>) -> Result<NewsDiagnostics, String> {
    let market = state.market.clone();
    tokio::task::spawn_blocking(move || market.read().unwrap().news_diagnostics())
        .await
        .map_err(|e| e.to_string())
}

// ─── Scanner ─────────────────────────────────────────────────────────────────

#[tauri::command(rename_all = "snake_case")]
pub async fn get_strategies(state: tauri::State<'_, AppState>) -> Result<Vec<Strategy>, String> {
    let se = state.strategy_enabled.clone();
    let sr = state.strategy_risk.clone();
    tokio::task::spawn_blocking(move || {
        let overrides = se.read().unwrap();
        let risk      = sr.read().unwrap();
        registry::all_strategies()
            .iter()
            .map(|s| Strategy {
                id:               s.id().to_string(),
                name:             s.name().to_string(),
                enabled:          overrides.get(s.id()).copied().unwrap_or_else(|| s.enabled()),
                sessions:         s.sessions().to_vec(),
                priority:         s.priority(),
                max_risk_dollars: risk.get(s.id()).copied()
                    .unwrap_or_else(|| s.risk_config().max_risk_dollars),
            })
            .collect()
    })
    .await
    .map_err(|e| e.to_string())
}

/// Toggle a strategy on/off at runtime (no code change needed). Persisted in the
/// `app_config` table so it survives a relaunch; the scanner picks it up live.
#[tauri::command(rename_all = "snake_case")]
pub fn set_strategy_enabled(
    strategy_id: String,
    enabled:     bool,
    state:       tauri::State<'_, AppState>,
) -> Result<(), String> {
    let json = {
        let mut m = state.strategy_enabled.write().unwrap();
        m.insert(strategy_id, enabled);
        serde_json::to_string(&*m).map_err(|e| e.to_string())?
    };
    let db = state.db.lock().unwrap();
    cache_repository::set_app_meta(&db, "strategy_overrides", &json).map_err(|e| e.to_string())
}

/// Set the $-risk-per-trade for a strategy at runtime. Persisted in the
/// `app_config` table so it survives a relaunch; position sizing reads it on the
/// next order (effet immédiat). A negative value is rejected.
#[tauri::command(rename_all = "snake_case")]
pub fn set_strategy_risk(
    strategy_id:      String,
    max_risk_dollars: f64,
    state:            tauri::State<'_, AppState>,
) -> Result<(), String> {
    if !(max_risk_dollars.is_finite() && max_risk_dollars >= 0.0) {
        return Err("risk must be a positive number".into());
    }
    let json = {
        let mut m = state.strategy_risk.write().unwrap();
        m.insert(strategy_id, max_risk_dollars);
        serde_json::to_string(&*m).map_err(|e| e.to_string())?
    };
    let db = state.db.lock().unwrap();
    cache_repository::set_app_meta(&db, "strategy_risk_overrides", &json).map_err(|e| e.to_string())
}

/// Identity card per strategy (keyed by strategy id). Static metadata used by
/// the UI to lay out panes/indicators and the info band when an alert lands.
#[tauri::command(rename_all = "snake_case")]
pub fn get_strategy_cards() -> HashMap<String, StrategyCard> {
    registry::all_strategies()
        .iter()
        .map(|s| (s.id().to_string(), s.card()))
        .collect()
}

/// Kick off async enrichment for a symbol shown in a zone. Idempotent and only
/// runs for strategies whose card declares enrichment/LLM needs. Returns at once;
/// results are polled via `get_alert_enrichment`.
#[tauri::command(rename_all = "snake_case")]
pub fn start_alert_enrichment(
    symbol:      String,
    strategy_id: String,
    state:       tauri::State<'_, AppState>,
) {
    let needs = registry::all_strategies()
        .iter()
        .find(|s| s.id() == strategy_id)
        .map(|s| {
            let c = s.card();
            !c.enrichments.is_empty() || c.llm.is_some()
        })
        .unwrap_or(false);
    if !needs {
        return;
    }
    if crate::enrichment::is_present(&state.enrichments, &symbol) {
        return;
    }
    let db = state.db.clone();
    let secrets = state.secrets.clone();
    let market = state.market.clone();
    let store = state.enrichments.clone();
    tauri::async_runtime::spawn(crate::enrichment::run(symbol, strategy_id, db, secrets, market, store));
}

/// User-triggered LLM read for the displayed alert (NOT automatic). Currently only
/// panic_mean_reversion uses it: a button in the info band fires this, which makes
/// one Deepseek call producing a short context summary + a mean-reversion verdict
/// (see `enrichment::run_panic_llm`). Ignored while a call is already in flight.
#[tauri::command(rename_all = "snake_case")]
pub fn run_alert_llm(
    symbol:      String,
    strategy_id: String,
    state:       tauri::State<'_, AppState>,
) {
    if strategy_id != crate::strategies::panic_mean_reversion::ID {
        return;
    }
    // Don't stack calls if one is already running for this symbol.
    if let Some(e) = state.enrichments.read().unwrap().get(&symbol) {
        if e.llm_pending {
            return;
        }
    }
    let db = state.db.clone();
    let secrets = state.secrets.clone();
    let store = state.enrichments.clone();
    tauri::async_runtime::spawn(crate::enrichment::run_panic_llm(symbol, db, secrets, store));
}

/// Current progressive enrichment for a symbol (polled by the info band).
#[tauri::command(rename_all = "snake_case")]
pub fn get_alert_enrichment(
    symbol: String,
    state:  tauri::State<'_, AppState>,
) -> Option<AlertEnrichment> {
    state.enrichments.read().unwrap().get(&symbol).cloned()
}

#[tauri::command(rename_all = "snake_case")]
pub async fn get_active_alerts(state: tauri::State<'_, AppState>) -> Result<Vec<AlertSignal>, String> {
    let alerts = state.active_alerts.clone();
    tokio::task::spawn_blocking(move || alerts.read().unwrap().clone())
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn get_alert_history(state: tauri::State<'_, AppState>) -> Result<Vec<AlertSignal>, String> {
    let history = state.alert_history.clone();
    tokio::task::spawn_blocking(move || history.read().unwrap().clone())
        .await
        .map_err(|e| e.to_string())
}

/// Live pre-open screener matches (currently-matching tickers, recomputed every
/// scan pass). Drives the pre-open tab sidebar.
#[tauri::command(rename_all = "snake_case")]
pub async fn get_screener_matches(state: tauri::State<'_, AppState>) -> Result<Vec<ScreenerMatch>, String> {
    let screener = state.screener.clone();
    tokio::task::spawn_blocking(move || screener.read().unwrap().clone())
        .await
        .map_err(|e| e.to_string())
}

/// Market Attention top list (direction-agnostic, top 10, refreshed once a minute
/// 09:30–12:30 ET; see `crate::market_attention`). Read-only debug/inspection
/// command — the list's primary consumer is the Perfect Pullback engine.
#[tauri::command(rename_all = "snake_case")]
pub async fn get_market_attention(state: tauri::State<'_, AppState>) -> Result<Vec<AttentionEntry>, String> {
    let attention = state.attention.clone();
    tokio::task::spawn_blocking(move || attention.read().unwrap().clone())
        .await
        .map_err(|e| e.to_string())
}

/// Today's ET trading date (DST-aware, matching the rest of the app), used to
/// scope screener dismissals to a single day. App clock: the simulated day
/// during a Market Replay.
fn et_today() -> String {
    crate::time::et_date(crate::time::now())
}

/// Persist a pre-open screener dismissal for TODAY so the ticker stays hidden
/// across restarts until the next trading day.
#[tauri::command(rename_all = "snake_case")]
pub fn dismiss_screener(symbol: String, state: tauri::State<'_, AppState>) -> Result<(), String> {
    let conn = state.db.lock().unwrap();
    conn.execute(
        "INSERT OR IGNORE INTO screener_dismissals (symbol, day) VALUES (?1, ?2)",
        rusqlite::params![symbol, et_today()],
    ).map_err(|e| e.to_string())?;
    Ok(())
}

/// Symbols dismissed from the screener TODAY. Old days are pruned on read so the
/// dismissals naturally reset each trading day.
#[tauri::command(rename_all = "snake_case")]
pub fn get_screener_dismissals(state: tauri::State<'_, AppState>) -> Vec<String> {
    let conn = state.db.lock().unwrap();
    let today = et_today();
    let _ = conn.execute("DELETE FROM screener_dismissals WHERE day <> ?1", rusqlite::params![today]);
    let mut stmt = match conn.prepare("SELECT symbol FROM screener_dismissals WHERE day = ?1") {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    let rows = stmt
        .query_map(rusqlite::params![today], |r| r.get::<_, String>(0))
        .map(|it| it.filter_map(|r| r.ok()).collect::<Vec<_>>())
        .unwrap_or_default();
    rows
}

#[tauri::command(rename_all = "snake_case")]
pub async fn start_scanner(state: tauri::State<'_, AppState>) -> Result<(), String> {
    if state.scanner_running.load(Ordering::Relaxed) {
        return Ok(());
    }
    state.scanner_running.store(true, Ordering::Relaxed);
    ScannerEngine::start(
        state.scanner_running.clone(),
        state.active_alerts.clone(),
        state.alert_history.clone(),
        state.screener.clone(),
        state.strategy_enabled.clone(),
        state.market.clone(),
        state.db.clone(),
    );
    Ok(())
}

#[tauri::command(rename_all = "snake_case")]
pub fn stop_scanner(state: tauri::State<'_, AppState>) -> Result<(), String> {
    state.scanner_running.store(false, Ordering::Relaxed);
    Ok(())
}

// ─── Mean-reversion scores (Panic Mean Reversion screener) ──────────────────────

/// Top-N mean-reversion scores (highest display score first), for debugging the
/// Panic Mean Reversion screener. Defaults to 30.
#[tauri::command(rename_all = "snake_case")]
pub fn get_mean_reversion_scores(
    limit: Option<u32>,
    state: tauri::State<'_, AppState>,
) -> Vec<crate::local_db::scoring_repository::ScoreRow> {
    let db = state.db.lock().unwrap();
    // Unfiltered (min_prev_volume = 0) for debugging — shows the full ranking
    // regardless of the strategy's liquidity gate.
    crate::local_db::scoring_repository::get_top(&db, limit.unwrap_or(30), 0).unwrap_or_default()
}

/// Per-symbol info-band data not present in the live snapshot: market cap, float,
/// and the mean-reversion score (display score + which kind + its horizon in
/// days). Backs the Panic Mean Reversion info band. All fields optional ("si
/// dispo"): a symbol may have no score, cap or float.
#[derive(Debug, Serialize)]
pub struct CardInfo {
    pub market_cap:    Option<i64>,
    pub float_shares:  Option<i64>,
    /// The watchlist metric value (BB area sum, or |move|/ATR20). None when off-list.
    pub mr_score:      Option<f64>,
    /// Which list retained the ticker: "BB" or "MA".
    pub mr_score_kind: Option<String>,
    /// Extension direction: +1 up, −1 down, 0 none.
    pub mr_direction:  Option<i8>,
    /// SIC industry + country of origin (sec-api company metadata). Surfaced in
    /// the manual ticker-search info band.
    pub industry:      Option<String>,
    pub country:       Option<String>,
    // ── Common chart info-bar fields (same for every strategy) ────────────────
    /// Bollinger Z of the live price vs its 20-day daily basis: (price − SMA20)/σ20.
    /// None until ≥20 daily bars are cached. Updates as the live price moves.
    pub bbz:               Option<f64>,
    /// Today's premarket cumulative volume (04:00–09:30 ET), summed from the live
    /// 1-minute ring buffer. None when no premarket bars are on file.
    pub premarket_volume:  Option<i64>,
    /// Whether a live news headline is on file for the symbol (Alpaca news feed).
    pub has_news:          bool,
    /// The most recent live headline text, if any (for the common "News" chip).
    pub news_title:        Option<String>,
    // ── Micro Pullback overlay: behavioural / risk scores (0..100, 100 = worst;
    //    None = inputs not collected). ──────────────────────────────────────────
    pub short_interest_score:    Option<f64>,
    pub dilution_capacity_score: Option<f64>,
    pub dilution_need_score:     Option<f64>,
    pub dilution_score:          Option<f64>,
    pub pump_dump_score:         Option<f64>,
    /// Real-time liquidity gauge: total share volume traded in the last 60 seconds
    /// (live 10s ring, forming bar included). None when no intraday bars yet. This
    /// is "right now", not the cumulative session — drives the overlay's Vol bar.
    pub live_volume:             Option<i64>,
}

/// One headline for the Micro Pullback overlay's news list (Alpaca REST, fetched
/// per displayed ticker). `created_at` is the publish time (RFC 3339) the frontend
/// turns into a freshness badge.
#[derive(Debug, Serialize)]
pub struct CardNews {
    pub headline:   String,
    pub created_at: String,
    pub source:     Option<String>,
}

#[tauri::command(rename_all = "snake_case")]
pub async fn get_card_info(symbol: String, state: tauri::State<'_, AppState>) -> Result<CardInfo, String> {
    let db_arc = state.db.clone();
    let market_arc = state.market.clone();
    tokio::task::spawn_blocking(move || {
        let db = db_arc.lock().unwrap();
        let asset = universe_repository::get_by_symbol(&db, &symbol).unwrap_or(None);
        let score = crate::local_db::scoring_repository::get_one(&db, &symbol).unwrap_or(None);
        let meta  = company_meta_repository::get_by_symbol(&db, &symbol).unwrap_or(None);
        let risk  = cache_repository::get_risk_scores(&db, &symbol).unwrap_or_default();
        let (mr_score, mr_score_kind, mr_direction) = match score {
            Some(s) => (Some(s.value), Some(s.list_kind), Some(s.direction)),
            None => (None, None, None),
        };

        let daily = cache_repository::get_daily_bars(&db, &symbol, 30).unwrap_or_default();
        let closes_asc: Vec<f64> = daily.iter().rev().filter_map(|b| b.close).collect();
        drop(db);

        let (bbz, premarket_volume, news_title, live_volume) = {
            let market = market_arc.read().unwrap();
            let price = market.last_price(&symbol).or_else(|| closes_asc.last().copied());
            let bbz = price.and_then(|p| crate::scoring::current_bbz(&closes_asc, p));

            let now    = crate::time::now();
            let today  = crate::time::et_date(now);
            let m1     = market.get_bars(&symbol, crate::market_state::aggregators::Timeframe::M1);
            let mut pm_sum: i64 = 0;
            let mut pm_any = false;
            for b in &m1 {
                if crate::time::et_date(b.time) != today { continue; }
                let mins = crate::time::et_minutes(b.time);
                if (240..570).contains(&mins) {
                    pm_sum += b.volume as i64;
                    pm_any = true;
                }
            }
            let premarket_volume = if pm_any { Some(pm_sum) } else { None };
            let news_title = market.latest_news(&symbol).map(|h| h.headline);
            let live_volume = market.volume_in_last(&symbol, 60, now);
            (bbz, premarket_volume, news_title, live_volume)
        };

        CardInfo {
            market_cap:    asset.as_ref().and_then(|a| a.market_cap),
            float_shares:  asset.as_ref().and_then(|a| a.float_shares),
            mr_score,
            mr_score_kind,
            mr_direction,
            industry:      meta.as_ref().and_then(|m| m.industry.clone()),
            country:       meta.as_ref().and_then(|m| m.country.clone()),
            bbz,
            premarket_volume,
            has_news:      news_title.is_some(),
            news_title,
            short_interest_score:    risk.short_interest_score,
            dilution_capacity_score: risk.dilution_capacity_score,
            dilution_need_score:     risk.dilution_need_score,
            dilution_score:          risk.dilution_score,
            pump_dump_score:         risk.pump_dump_score,
            live_volume,
        }
    })
    .await
    .map_err(|e| e.to_string())
}

/// The HOD Drive on-chart overlay payload for one symbol: the five KPIs plus the
/// HOD/LOD levels (+ their bar times) and the green-series bar times, so the chart can
/// draw the HOD/LOD points and a small cross under each series bar. Recomputed live
/// from the session 5-minute structure (the engine's own pure pipeline). All values
/// are `None`/empty when the symbol doesn't have enough structure yet.
#[derive(Debug, Serialize, Default)]
pub struct HodDriveOverlay {
    pub timeframe:              String,
    /// series_range / (HOD−LOD), 0..1.
    pub series_share:           Option<f64>,
    pub pullback_volume:        Option<f64>,
    /// pullback_volume / series_volume (1.0 = equal, 0.5 = half, 2.0 = double).
    pub pullback_vol_ratio:     Option<f64>,
    pub power_score:            Option<f64>,
    pub directional_efficiency: Option<f64>,
    pub hod:                    Option<f64>,
    pub lod:                    Option<f64>,
    /// Unix seconds of the HOD / LOD bars (for the chart points).
    pub hod_time:               Option<i64>,
    pub lod_time:               Option<i64>,
    /// Unix seconds of every bar in the green series (a cross is drawn under each).
    pub series_bar_times:       Vec<i64>,
    /// True when Gates 1-3 currently pass (overlay can badge the live structure).
    pub gates_pass:             bool,
    /// (HOD−LOD) / avg range of green daily candles. 1.0 = identical, 0.5 = half.
    pub range_vs_green_atr:     Option<f64>,
    // ── Suggested trade levels (R-based offsets from the pullback bar) ──
    pub suggested_entry:        Option<f64>,
    pub suggested_sl:           Option<f64>,
    pub suggested_tp:           Option<f64>,
    pub suggested_rr:           Option<f64>,
    // ── MACD trend status (computed on M1 session bars) ──
    /// True = trend still healthy (histogram > 0), false = exhausted.
    pub macd_open:              Option<bool>,
    /// 0..1 normalised magnitude of the histogram vs session peak.
    pub macd_strength:          Option<f64>,
}

#[tauri::command(rename_all = "snake_case")]
pub async fn get_hod_drive_overlay(
    symbol: String,
    state:  tauri::State<'_, AppState>,
) -> Result<HodDriveOverlay, String> {
    let market_arc = state.market.clone();
    let db_arc = state.db.clone();
    tokio::task::spawn_blocking(move || {
        let cfg = &crate::hod_drive::CFG_5M;
        let now = crate::time::now();
        let session_open = crate::time::et_session_open_utc(now);

        let (m1, price) = {
            let market = market_arc.read().unwrap();
            let m1 = market.get_bars(&symbol, crate::market_state::aggregators::Timeframe::M1);
            let price = market.last_price(&symbol);
            (m1, price)
        };
        let Some(price) = price else { return HodDriveOverlay { timeframe: cfg.label.into(), ..Default::default() } };

        let m1_session: Vec<_> = m1.into_iter().filter(|b| b.time >= session_open).collect();
        let bars = crate::hod_drive::session_bars(cfg, &m1_session, now);
        let volume_since_open: u64 = bars.iter().map(|b| b.volume).sum();
        let dollar_volume_since_open: f64 = bars
            .iter()
            .map(|b| ((b.high + b.low + b.close) / 3.0) * b.volume as f64)
            .sum();

        let Some(eval) =
            crate::hod_drive::evaluate(cfg, &bars, price, volume_since_open, dollar_volume_since_open)
        else {
            return HodDriveOverlay { timeframe: cfg.label.into(), ..Default::default() };
        };

        let range_vs_green_atr = {
            let open_range = eval.hod - eval.lod;
            if open_range > 0.0 {
                let today = Utc::now().format("%Y-%m-%d").to_string();
                let db = db_arc.lock().unwrap();
                let daily = cache_repository::ohlcv_ascending(&db, &symbol, 60, &today)
                    .unwrap_or_default();
                let green_ranges: Vec<f64> = daily
                    .iter()
                    .filter(|(o, _h, _l, c, _v)| *c > *o)
                    .map(|(_o, h, l, _c, _v)| h - l)
                    .collect();
                if !green_ranges.is_empty() {
                    let avg: f64 = green_ranges.iter().sum::<f64>() / green_ranges.len() as f64;
                    if avg > 0.0 { Some(open_range / avg) } else { None }
                } else {
                    None
                }
            } else {
                None
            }
        };

        let m1_closes: Vec<f64> = m1_session.iter().map(|b| b.close).collect();
        let macd = crate::hod_drive::macd_status(&m1_closes);

        HodDriveOverlay {
            timeframe:              cfg.label.into(),
            series_share:           Some(eval.series_share),
            pullback_volume:        Some(eval.pullback_volume as f64),
            pullback_vol_ratio:     Some(eval.pullback_vol_ratio),
            power_score:            Some(eval.power_score),
            directional_efficiency: Some(eval.directional_efficiency),
            hod:                    Some(eval.hod),
            lod:                    Some(eval.lod),
            hod_time:               bars.get(eval.hod_bar_idx).map(|b| b.time.timestamp()),
            lod_time:               bars.get(eval.lod_bar_idx).map(|b| b.time.timestamp()),
            series_bar_times:       eval
                .series_bar_idxs
                .iter()
                .filter_map(|&i| bars.get(i).map(|b| b.time.timestamp()))
                .collect(),
            gates_pass:             eval.gates_pass,
            range_vs_green_atr,
            suggested_entry:        eval.suggested_entry,
            suggested_sl:           eval.suggested_sl,
            suggested_tp:           eval.suggested_tp,
            suggested_rr:           eval.suggested_rr,
            macd_open:              macd.as_ref().map(|m| m.open),
            macd_strength:          macd.as_ref().map(|m| m.strength),
        }
    })
    .await
    .map_err(|e| e.to_string())
}

/// The most recent single-ticker headlines for `symbol` (Alpaca news REST), for the
/// Micro Pullback overlay's news list. Fetched per displayed ticker, headlines only
/// (no article body). Headlines that reference several tickers are dropped — we only
/// want news genuinely about this one. Returns up to 4, newest first. Empty on
/// missing credentials / fetch error so the overlay degrades gracefully.
#[tauri::command(rename_all = "snake_case")]
pub async fn get_ticker_news(
    symbol: String,
    state:  tauri::State<'_, AppState>,
) -> Result<Vec<CardNews>, String> {
    let (key, secret) = {
        let s = state.secrets.read().unwrap();
        (s.alpaca_key.clone(), s.alpaca_secret.clone())
    };
    let (Some(key), Some(secret)) = (key, secret) else {
        return Ok(vec![]); // no credentials (e.g. mock mode)
    };
    if key.is_empty() || secret.is_empty() {
        return Ok(vec![]);
    }

    // Fetch a generous window/limit so enough single-ticker candidates survive the
    // multi-ticker filter; then keep the 4 newest (the API already sorts desc).
    let raw = crate::alpaca::news::fetch_recent_headlines(&key, &secret, &symbol, 30, 50)
        .await
        .unwrap_or_default();
    let out: Vec<CardNews> = raw
        .into_iter()
        .filter(|n| n.symbols.len() <= 1) // drop headlines lumping several tickers
        .map(|n| CardNews {
            headline:   n.headline,
            created_at: n.created_at.to_rfc3339(),
            source:     Some(n.source).filter(|s| !s.is_empty()),
        })
        .take(4)
        .collect();
    Ok(out)
}

/// One news pastille for the chart overlay: the publish time (unix seconds) and
/// headline. Plotted as a small dot at the bottom of the pane (over the volume),
/// snapped client-side to the bar that was forming when it published.
#[derive(Debug, Serialize)]
pub struct NewsMarker {
    pub time:     i64,
    pub headline: String,
}

/// Single-ticker news timestamps for `symbol` over a wide window (Alpaca news REST),
/// so the chart can drop a small pastille on each bar that had news — on intraday
/// AND daily panes (the frontend snaps each timestamp to the nearest loaded bar, so
/// markers outside the loaded range simply don't render). One query serves every
/// timeframe. Headlines lumping several tickers are dropped. Empty on missing
/// credentials / fetch error so the chart degrades gracefully.
#[tauri::command(rename_all = "snake_case")]
pub async fn get_news_markers(
    symbol: String,
    state:  tauri::State<'_, AppState>,
) -> Result<Vec<NewsMarker>, String> {
    let (key, secret) = {
        let s = state.secrets.read().unwrap();
        (s.alpaca_key.clone(), s.alpaca_secret.clone())
    };
    let (Some(key), Some(secret)) = (key, secret) else {
        return Ok(vec![]); // no credentials (e.g. mock mode)
    };
    if key.is_empty() || secret.is_empty() {
        return Ok(vec![]);
    }

    let raw = crate::alpaca::news::fetch_recent_headlines(&key, &secret, &symbol, 365, 200)
        .await
        .unwrap_or_default();
    let out: Vec<NewsMarker> = raw
        .into_iter()
        .filter(|n| n.symbols.len() <= 1) // drop headlines lumping several tickers
        .map(|n| NewsMarker { time: n.created_at.timestamp(), headline: n.headline })
        .collect();
    Ok(out)
}

/// Force an immediate rebuild of the Panic Mean Reversion watchlist (ignores the
/// 09:00 ET / once-per-day gate) — for testing. Runs off the async runtime (it
/// fetches premarket minute bars + reads daily history) so the command returns at
/// once. Note: before ~09:00 ET there's little/no premarket data, so the premarket
/// liquidity branches won't contribute yet.
#[tauri::command(rename_all = "snake_case")]
pub fn force_recompute_scores(state: tauri::State<'_, AppState>) {
    let db = state.db.clone();
    let secrets = state.secrets.clone();
    tauri::async_runtime::spawn(async move {
        match crate::scoring::build_and_store(&db, &secrets).await {
            Ok(n) => eprintln!("[tagdash] panic watchlist rebuilt: {n} rows"),
            Err(e) => eprintln!("[tagdash] panic watchlist rebuild failed: {e}"),
        }
    });
}

// ─── Chart / zone trade context ───────────────────────────────────────────────

#[tauri::command(rename_all = "snake_case")]
pub fn get_zone_trade_context(
    zone_id: String,
    symbol:  String,
    state:   tauri::State<'_, AppState>,
) -> Option<ZoneTradeContext> {
    // Context is per-ticker: record the zone's current ticker and return that
    // ticker's SL/TP/tradeID, so it follows the ticker across zone swaps (and is
    // restored when the ticker comes back).
    state.chart.write().unwrap().load_zone_context(&zone_id, &symbol)
}

#[tauri::command(rename_all = "snake_case")]
pub fn create_or_get_trade_id_for_zone(
    zone_id:     String,
    symbol:      String,
    strategy_id: String,
    state:       tauri::State<'_, AppState>,
) -> Result<String, String> {
    let id = state
        .chart
        .write()
        .unwrap()
        .create_or_get_trade_id(&zone_id, &symbol, &strategy_id);
    persist_chart(&state.chart, &state.db);
    Ok(id)
}

/// Set (or clear) the SL for a zone. Auto-creates tradeID if price is Some.
/// Enqueues trade_id_created (first time) + sl_updated to TradeTally.
#[tauri::command(rename_all = "snake_case")]
pub fn update_zone_sl(
    zone_id:     String,
    symbol:      String,
    strategy_id: String,
    price:       Option<f64>,
    state:       tauri::State<'_, AppState>,
) -> ZoneTradeContext {
    let ctx = state
        .chart
        .write()
        .unwrap()
        .update_sl(&zone_id, &symbol, &strategy_id, price);

    // Keep the live bracket SL order (if any) in sync with the moved line.
    state.internal_book.write().unwrap()
        .update_protective_levels(&symbol, ctx.stop_loss, ctx.take_profit);

    // Persist the moved level: the chart line (context) and any re-armed bracket
    // order must both come back identically after a restart.
    persist_chart(&state.chart, &state.db);
    persist_book(&state.internal_book, &state.db);

    // Push levels to TradeTally only once the trade exists there (≥1 fill).
    // Before that, SL/TP are local and ride along in the trade_created payload.
    if let Some(ref trade_id) = ctx.trade_id {
        let has_activity = state.internal_book.read().unwrap()
            .get_trade_lifecycle(trade_id)
            .map(|lc| !lc.fills.is_empty())
            .unwrap_or(false);
        if has_activity {
            // Push the TP only. The journal SL stays at its opening value
            // (stamped in trade_created); post-entry SL moves only drive the
            // local bracket order and must not reach TradeTally.
            let db = state.db.lock().unwrap();
            tradetally::enqueue_levels_updated(&db, trade_id, &symbol, ctx.take_profit);
        }
    }

    ctx
}

/// Set (or clear) the TP for a zone. Auto-creates tradeID if price is Some.
/// Enqueues trade_id_created (first time) + tp_updated to TradeTally.
#[tauri::command(rename_all = "snake_case")]
pub fn update_zone_tp(
    zone_id:     String,
    symbol:      String,
    strategy_id: String,
    price:       Option<f64>,
    state:       tauri::State<'_, AppState>,
) -> ZoneTradeContext {
    let ctx = state
        .chart
        .write()
        .unwrap()
        .update_tp(&zone_id, &symbol, &strategy_id, price);

    // Keep the live bracket TP order (if any) in sync with the moved line.
    state.internal_book.write().unwrap()
        .update_protective_levels(&symbol, ctx.stop_loss, ctx.take_profit);

    // Persist the moved level (chart line + re-armed bracket) so it survives a restart.
    persist_chart(&state.chart, &state.db);
    persist_book(&state.internal_book, &state.db);

    if let Some(ref trade_id) = ctx.trade_id {
        let has_activity = state.internal_book.read().unwrap()
            .get_trade_lifecycle(trade_id)
            .map(|lc| !lc.fills.is_empty())
            .unwrap_or(false);
        if has_activity {
            let db = state.db.lock().unwrap();
            tradetally::enqueue_levels_updated(&db, trade_id, &symbol, ctx.take_profit);
        }
    }

    ctx
}

#[tauri::command(rename_all = "snake_case")]
pub fn clear_zone_context(
    zone_id: String,
    state:   tauri::State<'_, AppState>,
) -> Result<(), String> {
    state.chart.write().unwrap().clear_zone(&zone_id);
    Ok(())
}

// ─── Internal trading engine ──────────────────────────────────────────────────

fn market_prices(state: &tauri::State<'_, AppState>, symbol: &str) -> (f64, f64) {
    let market = state.market.read().unwrap();
    if let Some(t) = market.tickers.get(symbol) {
        let last = t.last_price.unwrap_or(0.0);
        let bid  = t.bid.unwrap_or_else(|| (last * 0.999).max(0.0));
        let ask  = t.ask.unwrap_or_else(|| last * 1.001);
        (bid, ask)
    } else {
        (0.0, 0.0)
    }
}

/// Per-symbol (bid, ask) over the whole live snapshot, synthesising a bid/ask from
/// the last price when a real quote is missing. Symbols without a real (positive)
/// last price are omitted — a (0, 0) entry would otherwise poison live PnL and,
/// historically, trip resting stops at $0. Used for the PnL refresh only; order
/// fills run off `drain_fill_windows` (the true price path), not this snapshot.
fn all_market_prices(market: &RwLock<MarketState>) -> HashMap<String, (f64, f64)> {
    market
        .read()
        .unwrap()
        .tickers
        .iter()
        .filter_map(|(sym, t)| {
            let last = t.last_price.filter(|p| *p > 0.0)?;
            let bid  = t.bid.unwrap_or_else(|| (last * 0.999).max(0.0));
            let ask  = t.ask.unwrap_or_else(|| last * 1.001);
            Some((sym.clone(), (bid, ask)))
        })
        .collect()
}

fn strategy_max_risk(state: &tauri::State<'_, AppState>, strategy_id: &str) -> f64 {
    // Runtime Settings override wins; else the strategy's compiled default.
    if let Some(r) = state.strategy_risk.read().unwrap().get(strategy_id).copied() {
        return r;
    }
    registry::all_strategies()
        .iter()
        .find(|s| s.id() == strategy_id)
        .map(|s| s.risk_config().max_risk_dollars)
        .unwrap_or(100.0)
}

fn size_for_zone(
    state:   &tauri::State<'_, AppState>,
    zone_id: &str,
    percent: u8,
) -> Result<(i64, Side, f64, f64, ZoneTradeContext), String> {
    let ctx = state.chart.read().unwrap()
        .get_context_for_zone(zone_id)
        .ok_or_else(|| "zone has no trade context".to_string())?;

    let sl = ctx.stop_loss.ok_or_else(|| "SL is required".to_string())?;
    let (bid, ask) = market_prices(state, &ctx.symbol);
    let entry = (bid + ask) / 2.0;
    if entry < 1e-6 { return Err("no market price available".into()); }

    let max_risk = strategy_max_risk(state, &ctx.strategy_id);
    let cfg      = state.config.read().unwrap();
    let sizing   = InternalBook::compute_risk_sizing(
        entry, sl, max_risk,
        cfg.trading.min_position_size,
        cfg.trading.max_position_size,
    );
    drop(cfg);

    let qty = match percent {
        25  => sizing.size_25,
        50  => sizing.size_50,
        _   => sizing.size_100,
    };
    if qty == 0 { return Err("position size is 0 — check SL distance".into()); }

    Ok((qty, sizing.side, bid, ask, ctx))
}

#[tauri::command(rename_all = "snake_case")]
pub fn create_internal_order_percent(
    zone_id: String,
    percent: u8,
    state:   tauri::State<'_, AppState>,
) -> Result<InternalOrder, String> {
    let (qty, side, bid, ask, ctx) = size_for_zone(&state, &zone_id, percent)?;
    let limit_price = crate::internal_trading::fills::price_for_side(side, bid, ask);

    let order = state.internal_book.write().unwrap().create_limit_order(
        ctx.trade_id.clone(),
        zone_id,
        ctx.symbol.clone(),
        ctx.strategy_id.clone(),
        side,
        qty,
        limit_price,
        ctx.stop_loss,
        ctx.take_profit,
    );
    // A resting entry order is part of the day's state — persist so it survives a
    // restart and the trading loop can still fill it on a price cross.
    persist_book(&state.internal_book, &state.db);
    Ok(order)
}

#[tauri::command(rename_all = "snake_case")]
pub fn create_internal_limit_order_percent(
    zone_id: String,
    percent: u8,
    limit_price: f64,
    state: tauri::State<'_, AppState>,
) -> Result<InternalOrder, String> {
    let ctx = state.chart.read().unwrap()
        .get_context_for_zone(&zone_id)
        .ok_or_else(|| "zone has no trade context".to_string())?;

    let sl = ctx.stop_loss.ok_or_else(|| "SL is required".to_string())?;
    if limit_price < 1e-6 { return Err("limit price is invalid".into()); }

    let max_risk = strategy_max_risk(&state, &ctx.strategy_id);
    let cfg = state.config.read().unwrap();
    let sizing = InternalBook::compute_risk_sizing(
        limit_price, sl, max_risk,
        cfg.trading.min_position_size,
        cfg.trading.max_position_size,
    );
    drop(cfg);

    let qty = match percent {
        25  => sizing.size_25,
        50  => sizing.size_50,
        _   => sizing.size_100,
    };
    if qty == 0 { return Err("position size is 0 — check SL distance".into()); }

    let order = state.internal_book.write().unwrap().create_limit_order(
        ctx.trade_id.clone(),
        zone_id,
        ctx.symbol.clone(),
        ctx.strategy_id.clone(),
        sizing.side,
        qty,
        limit_price,
        ctx.stop_loss,
        ctx.take_profit,
    );
    persist_book(&state.internal_book, &state.db);
    Ok(order)
}

/// Thin wrapper over `sync_fill` for the command paths (market entry / manual
/// close), which hold a `tauri::State`.
fn tt_sync_fill(state: &tauri::State<'_, AppState>, fill: &Fill) {
    sync_fill(&state.internal_book, &state.db, &state.config, &state.chart, fill);
}

/// Snapshot the trading book to SQLite so an open multi-day position and its
/// resting orders survive a restart. Called after every state-changing book
/// mutation (fills, order create/cancel, level changes). Takes a fresh snapshot
/// under a short read lock, then writes — book and db locks are never held nested.
fn persist_book(
    internal_book: &Arc<RwLock<InternalBook>>,
    db:            &Arc<Mutex<rusqlite::Connection>>,
) {
    let snapshot = internal_book.read().unwrap().persistable_snapshot();
    let db = db.lock().unwrap();
    book_repository::save_book(&db, &snapshot);
}

/// Snapshot the per-ticker chart trade contexts (SL/TP/tradeID lines) to SQLite
/// so they reappear on the chart after a restart. Called after every context
/// mutation.
fn persist_chart(
    chart: &Arc<RwLock<ChartState>>,
    db:    &Arc<Mutex<rusqlite::Connection>>,
) {
    let ctxs = chart.read().unwrap().export_contexts();
    let db = db.lock().unwrap();
    book_repository::save_chart_contexts(&db, &ctxs);
}

/// Reconcile a freshly-produced fill with TradeTally and persist its execution.
/// Free function over the shared Arcs (not `tauri::State`) so it serves both the
/// command paths and the backend trading loop (`spawn_trading_loop`).
///
/// First fill of a trade → create the trade (POST). Scale-in while still open →
/// fill_added (PUT: executions + avg entry + net qty). Position now flat →
/// trade_closed (PUT: exit + executions; pnl computed by TradeTally).
fn sync_fill(
    internal_book: &Arc<RwLock<InternalBook>>,
    db:            &Arc<Mutex<rusqlite::Connection>>,
    config:        &Arc<RwLock<AppConfig>>,
    chart:         &Arc<RwLock<crate::chart_state::ChartState>>,
    fill:          &Fill,
) {
    if fill.trade_id.is_empty() { return; }

    // Snapshot the post-fill trade state under the book read lock.
    let (all_fills, position, side, sl, tp, strategy_id, mae, mfe) = {
        let book = internal_book.read().unwrap();
        let Some(lc) = book.get_trade_lifecycle(&fill.trade_id) else { return; };
        let pos   = lc.position.clone();
        let side  = pos.as_ref().map(|p| p.side).unwrap_or(fill.side);
        let sl    = pos.as_ref().and_then(|p| p.stop_loss);
        let tp    = pos.as_ref().and_then(|p| p.take_profit);
        let strat = pos.as_ref()
            .map(|p| p.strategy_id.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| lc.trade.strategy_id.clone());
        // MAE/MFE are stamped onto the trade record when it goes flat.
        (lc.fills.clone(), pos, side, sl, tp, strat, lc.trade.mae, lc.trade.mfe)
    };

    let cfg = config.read().unwrap().clone();
    let closing = position.is_none();

    {
        let db = db.lock().unwrap();
        // Persist the execution for the chart's per-ticker markers (survives
        // restarts / multi-day trades).
        let _ = execution_repository::insert_fill(&db, fill);
        if all_fills.len() == 1 {
            // Stamp the launch-time SL once (never overwritten by later moves) so
            // the chart can draw the original-SL segment for the trade's duration.
            if let Some(sl0) = sl {
                let _ = execution_repository::set_original_sl(&db, &fill.trade_id, &fill.symbol, sl0);
            }
            let strategy_name = registry::all_strategies().iter()
                .find(|s| s.id() == strategy_id)
                .map(|s| s.name().to_string())
                .unwrap_or_else(|| strategy_id.clone());
            tradetally::enqueue_trade_created(
                &db, &fill.trade_id, &fill.symbol, &strategy_name,
                side, fill.fill_price, fill.quantity, &fill.filled_at.to_rfc3339(),
                sl, tp, &all_fills, &cfg,
            );
        } else if let Some(pos) = position {
            tradetally::enqueue_fill_added(
                &db, &fill.trade_id, &fill.symbol,
                pos.avg_entry_price, pos.quantity.abs(), &all_fills, &cfg,
            );
        } else {
            tradetally::enqueue_trade_closed(
                &db, &fill.trade_id, &fill.symbol,
                fill.fill_price, &fill.filled_at.to_rfc3339(), fill.quantity,
                mae, mfe, &all_fills, &cfg,
            );
        }
    }

    // Trade flat → retire the zone's tradeID + SL/TP so the chart lines clear
    // and a re-entry in the same zone starts a brand-new trade.
    if closing {
        chart.write().unwrap().reset_closed_trade(&fill.trade_id);
        persist_chart(chart, db);
    }

    // Checkpoint the book after every fill (entry, scale, exit) so positions and
    // resting orders are restorable identically after a restart.
    persist_book(internal_book, db);
}

/// Backend trading loop: drives the internal order book off market data instead
/// of as a side effect of UI position/order polls. Every tick it (1) drains the
/// per-symbol price path since the last tick and fills any pending limit/stop
/// orders that path crossed (range-based, so a level spiked through and retraced
/// still fills, and SL/TP inside one window resolve by which the price reached
/// first), (2) re-arms the bracket orders for symbols that just filled,
/// (3) refreshes live PnL + MAE/MFE watermarks, then (4) mirrors each new fill to
/// TradeTally. So fills happen at a steady cadence whether or not any panel is
/// open, and the getters are pure reads.
pub fn spawn_trading_loop(
    running:       Arc<std::sync::atomic::AtomicBool>,
    market:        Arc<RwLock<MarketState>>,
    internal_book: Arc<RwLock<InternalBook>>,
    db:            Arc<Mutex<rusqlite::Connection>>,
    config:        Arc<RwLock<AppConfig>>,
    chart:         Arc<RwLock<crate::chart_state::ChartState>>,
) {
    if running.load(Ordering::Relaxed) {
        return;
    }
    running.store(true, Ordering::Relaxed);
    tauri::async_runtime::spawn(async move {
        while running.load(Ordering::Relaxed) {
            // Price path since the last tick (drives fills) + a guarded snapshot
            // (drives PnL). Each market lock is released before the book is taken,
            // so there's no lock nesting.
            let windows = market.write().unwrap().drain_fill_windows();
            let prices  = all_market_prices(&market);
            let new_fills = {
                let mut book = internal_book.write().unwrap();
                let nf = book.try_fill_pending(&windows);
                // Reconcile bracket orders for any symbol that just filled (entry →
                // arm SL/TP; exit → clear leftovers).
                for f in &nf {
                    book.sync_bracket_orders(&f.symbol);
                }
                // Refresh live PnL + MAE/MFE watermarks even when no panel polls.
                let _ = book.positions_with_pnl(&prices);
                nf
            };
            for f in &new_fills {
                sync_fill(&internal_book, &db, &config, &chart, f);
            }
            // 500 ms of market time (scaled during an accelerated replay so
            // pending orders / brackets fill at the live-equivalent cadence).
            crate::replay::clock::scaled_sleep(500).await;
        }
    });
}

#[tauri::command(rename_all = "snake_case")]
pub fn create_internal_market_order_percent(
    zone_id: String,
    percent: u8,
    state:   tauri::State<'_, AppState>,
) -> Result<Fill, String> {
    let (qty, side, bid, ask, ctx) = size_for_zone(&state, &zone_id, percent)?;
    let fill_price = crate::internal_trading::fills::price_for_side(side, bid, ask);

    let fill = state.internal_book.write().unwrap().execute_market_fill(
        ctx.trade_id.clone(),
        zone_id,
        ctx.symbol.clone(),
        ctx.strategy_id.clone(),
        side,
        qty,
        fill_price,
        ctx.stop_loss,
        ctx.take_profit,
    );

    // (Re)materialise protective SL/TP bracket orders for the open position.
    state.internal_book.write().unwrap().sync_bracket_orders(&fill.symbol);

    // Mirror the fill to TradeTally (creates the trade on the first fill).
    tt_sync_fill(&state, &fill);

    Ok(fill)
}

#[tauri::command(rename_all = "snake_case")]
pub fn cancel_internal_order(
    order_id: String,
    state:    tauri::State<'_, AppState>,
) -> Result<(), String> {
    let res = state.internal_book.write().unwrap().cancel_order(&order_id);
    if res.is_ok() {
        persist_book(&state.internal_book, &state.db);
    }
    res
}

#[tauri::command(rename_all = "snake_case")]
pub fn close_internal_position(
    symbol:  String,
    zone_id: String,
    state:   tauri::State<'_, AppState>,
) -> Result<Fill, String> {
    let (bid, ask) = market_prices(&state, &symbol);
    if bid < 1e-6 { return Err("no market price available".into()); }

    let strategy_id = state.chart.read().unwrap()
        .get_context_for_zone(&zone_id)
        .map(|c| c.strategy_id)
        .unwrap_or_default();

    let fill = state.internal_book.write().unwrap()
        .close_position(&symbol, bid, ask, strategy_id, zone_id)
        .ok_or_else(|| format!("no open position for {}", symbol))?;

    // Mirror the closing fill to TradeTally (position now flat → trade_closed).
    tt_sync_fill(&state, &fill);

    Ok(fill)
}

#[tauri::command(rename_all = "snake_case")]
pub fn get_internal_positions(state: tauri::State<'_, AppState>) -> Vec<Position> {
    // Pure read: fills, bracket sync and TradeTally mirroring are owned by the
    // backend trading loop (`spawn_trading_loop`). Here we only refresh live PnL
    // (and MAE/MFE watermarks) from the latest prices and return the positions.
    let prices = all_market_prices(&state.market);
    state.internal_book.write().unwrap().positions_with_pnl(&prices)
}

#[tauri::command(rename_all = "snake_case")]
pub fn get_internal_orders(state: tauri::State<'_, AppState>) -> Vec<InternalOrder> {
    // Pure read: fills are driven by the backend trading loop, not this getter.
    state.internal_book.read().unwrap().get_pending_orders()
}

#[tauri::command(rename_all = "snake_case")]
pub fn get_trade_lifecycle(
    trade_id: String,
    state:    tauri::State<'_, AppState>,
) -> Option<TradeLifecycle> {
    state.internal_book.read().unwrap().get_trade_lifecycle(&trade_id)
}

/// One execution marker for the chart: a fill at (time, price), flagged as a
/// position increase (triangle points right) or decrease (points left).
#[derive(Serialize)]
pub struct ExecFill {
    pub time:     String, // RFC3339; the chart converts to NY for display
    pub price:    f64,
    pub increase: bool, // position grew (triangle points right) vs shrank (left)
    pub buy:      bool, // buy fill (green) vs sell fill (red)
}

/// All executions of one trade, grouped so the chart can connect them with a
/// single line. `long` drives the triangle colour (green long / red short);
/// `closed` + `pnl` drive the connecting line colour (green profit / red loss).
#[derive(Serialize)]
pub struct TradeExecutions {
    pub trade_id: String,
    pub long:     bool,
    pub closed:   bool,
    pub pnl:      f64,
    /// Launch-time SL (immutable; from `trade_levels`). Drawn as a thin segment
    /// at this price for the trade's duration. None when no SL was set at entry.
    pub original_sl: Option<f64>,
    pub fills:    Vec<ExecFill>,
}

/// Persisted executions for a ticker, grouped by trade (oldest → newest). Drives
/// the entry/scale/exit triangles + connecting P&L line on every chart of that
/// symbol. Increase/decrease and realized P&L are reconstructed from the signed
/// fill quantities (running net position + cash flow).
#[tauri::command(rename_all = "snake_case")]
pub fn get_executions_for_symbol(
    symbol: String,
    state:  tauri::State<'_, AppState>,
) -> Vec<TradeExecutions> {
    let (rows, original_sls) = {
        let conn = state.db.lock().unwrap();
        let rows = execution_repository::get_for_symbol(&conn, &symbol).unwrap_or_default();
        let sls: HashMap<String, f64> = execution_repository::original_sls_for_symbol(&conn, &symbol)
            .unwrap_or_default()
            .into_iter()
            .collect();
        (rows, sls)
    };

    // Group by trade_id, preserving first-seen (chronological) order.
    let mut order: Vec<String> = Vec::new();
    let mut by_trade: HashMap<String, Vec<execution_repository::ExecutionRow>> = HashMap::new();
    for r in rows {
        if !by_trade.contains_key(&r.trade_id) {
            order.push(r.trade_id.clone());
        }
        by_trade.entry(r.trade_id.clone()).or_default().push(r);
    }

    let mut out = Vec::new();
    for tid in order {
        let frows = by_trade.remove(&tid).unwrap_or_default();
        let mut net:  i64 = 0;        // running signed position
        let mut cash: f64 = 0.0;      // running cash flow (− on buys, + on sells)
        let mut long_dir: Option<bool> = None;
        let mut fills = Vec::new();
        for r in &frows {
            let before = net;
            net  += r.quantity;
            cash += -(r.quantity as f64) * r.fill_price;
            if long_dir.is_none() && r.quantity != 0 {
                long_dir = Some(r.quantity > 0);
            }
            fills.push(ExecFill {
                time:     r.filled_at.clone(),
                price:    r.fill_price,
                increase: net.abs() > before.abs(),
                buy:      r.quantity > 0, // signed delta: + = buy/long, − = sell/short
            });
        }
        let original_sl = original_sls.get(&tid).copied();
        out.push(TradeExecutions {
            trade_id: tid,
            long:     long_dir.unwrap_or(true),
            closed:   net == 0,
            pnl:      cash, // realized cash P&L once flat
            original_sl,
            fills,
        });
    }
    out
}

// ─── Chart drawings (persisted per ticker) ────────────────────────────────────

/// Persist a user drawing (trend line or text annotation) for a ticker so it
/// reappears on every chart/zone of that symbol and survives restarts.
#[tauri::command(rename_all = "snake_case")]
pub fn create_drawing(
    drawing: Drawing,
    state:   tauri::State<'_, AppState>,
) -> Result<(), String> {
    let conn = state.db.lock().unwrap();
    drawing_repository::insert(&conn, &drawing).map_err(|e| e.to_string())
}

/// All persisted drawings for a ticker (lines + text annotations).
#[tauri::command(rename_all = "snake_case")]
pub fn get_drawings_for_symbol(
    symbol: String,
    state:  tauri::State<'_, AppState>,
) -> Vec<Drawing> {
    let conn = state.db.lock().unwrap();
    drawing_repository::get_for_symbol(&conn, &symbol).unwrap_or_default()
}

/// Update an existing drawing (position after a drag, or style after an edit).
/// Same INSERT OR REPLACE path as create — the full row is sent each time.
#[tauri::command(rename_all = "snake_case")]
pub fn update_drawing(
    drawing: Drawing,
    state:   tauri::State<'_, AppState>,
) -> Result<(), String> {
    let conn = state.db.lock().unwrap();
    drawing_repository::insert(&conn, &drawing).map_err(|e| e.to_string())
}

#[tauri::command(rename_all = "snake_case")]
pub fn delete_drawing(
    id:    String,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let conn = state.db.lock().unwrap();
    drawing_repository::delete(&conn, &id).map_err(|e| e.to_string())
}

// ─── Market Replay ────────────────────────────────────────────────────────────

/// Start replaying `day` (YYYY-MM-DD, ET trading date) from `start_hm`
/// ("04:00" | "07:00" | "09:30"). Stops the live feeds, switches the app clock
/// to simulated time and loads the day's data (progress via get_replay_status).
#[tauri::command(rename_all = "snake_case")]
pub async fn replay_start(
    day:      String,
    start_hm: String,
    app:      tauri::AppHandle,
    state:    tauri::State<'_, AppState>,
) -> Result<(), String> {
    let start_min = parse_hm(&start_hm)?;
    let deps = crate::replay::ReplayDeps {
        app_dir:           state.app_dir.clone(),
        market:            state.market.clone(),
        db:                state.db.clone(),
        config:            state.config.clone(),
        secrets:           state.secrets.clone(),
        live_feed_running: state.live_feed_running.clone(),
        news_feed_running: state.news_feed_running.clone(),
        focus_rx:          state.focus_symbols_tx.subscribe(),
        focus_rx_restart:  state.focus_symbols_tx.subscribe(),
        active_alerts:     state.active_alerts.clone(),
        alert_history:     state.alert_history.clone(),
        app,
    };
    let replay = state.replay.clone();
    tokio::task::spawn_blocking(move || crate::replay::start(replay, deps, day, start_min))
        .await
        .map_err(|e| e.to_string())?
}

fn parse_hm(hm: &str) -> Result<u32, String> {
    let (h, m) = hm.split_once(':').ok_or("heure invalide (HH:MM)")?;
    let h: u32 = h.parse().map_err(|_| "heure invalide")?;
    let m: u32 = m.parse().map_err(|_| "heure invalide")?;
    if h > 23 || m > 59 {
        return Err("heure invalide".into());
    }
    Ok(h * 60 + m)
}

#[tauri::command(rename_all = "snake_case")]
pub fn replay_stop(state: tauri::State<'_, AppState>) -> Result<(), String> {
    state.replay.send(crate::replay::ReplayCmd::Stop)
}

#[tauri::command(rename_all = "snake_case")]
pub fn replay_set_playing(
    playing: bool,
    state:   tauri::State<'_, AppState>,
) -> Result<(), String> {
    state.replay.send(if playing {
        crate::replay::ReplayCmd::Play
    } else {
        crate::replay::ReplayCmd::Pause
    })
}

#[tauri::command(rename_all = "snake_case")]
pub fn replay_set_speed(speed: f64, state: tauri::State<'_, AppState>) -> Result<(), String> {
    if !speed.is_finite() || speed <= 0.0 {
        return Err("vitesse invalide".into());
    }
    state.replay.send(crate::replay::ReplayCmd::SetSpeed(speed))
}

/// Avance/recule le temps simulé de `delta_secs` secondes (négatif = retour en
/// arrière → l'état du marché est rejoué depuis le début de journée).
#[tauri::command(rename_all = "snake_case")]
pub fn replay_seek_relative(
    delta_secs: i64,
    state:      tauri::State<'_, AppState>,
) -> Result<(), String> {
    state.replay.send(crate::replay::ReplayCmd::SeekRelative(delta_secs))
}

/// Saute à une heure ET du jour rejoué ("04:00", "07:00", "09:30"…).
#[tauri::command(rename_all = "snake_case")]
pub fn replay_seek_clock(hm: String, state: tauri::State<'_, AppState>) -> Result<(), String> {
    let minutes = parse_hm(&hm)?;
    state.replay.send(crate::replay::ReplayCmd::SeekClock { minutes })
}

/// Avance en accéléré jusqu'à la prochaine alerte scanner, puis met en pause.
#[tauri::command(rename_all = "snake_case")]
pub fn replay_next_alert(state: tauri::State<'_, AppState>) -> Result<(), String> {
    state.replay.send(crate::replay::ReplayCmd::NextAlert)
}

/// Avance jusqu'au prochain close de barre 1 min, puis pause.
#[tauri::command(rename_all = "snake_case")]
pub fn replay_next_bar(state: tauri::State<'_, AppState>) -> Result<(), String> {
    state.replay.send(crate::replay::ReplayCmd::NextBar)
}

/// Charge la séance suivante (jour ouvré suivant) à la même heure de départ.
#[tauri::command(rename_all = "snake_case")]
pub fn replay_next_day(state: tauri::State<'_, AppState>) -> Result<(), String> {
    state.replay.send(crate::replay::ReplayCmd::NextDay)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn get_replay_status(state: tauri::State<'_, AppState>) -> Result<crate::replay::ReplayStatus, String> {
    let replay = state.replay.clone();
    tokio::task::spawn_blocking(move || replay.status.read().unwrap().clone())
        .await
        .map_err(|e| e.to_string())
}

// ─── Flat files (offline market-data download for Market Replay) ────────────────

/// Download flat files of `kind` ("trade" | "minute" | "daily") over [start_day,
/// end_day], persisting them under `<app_dir>/flat_files/<kind>/`. Runs in the
/// background — poll `get_flat_files_status`. Errors if a download is already running
/// or the Alpaca keys are missing. (Daily ignores the day granularity and writes the
/// cumulative `daily.db`; for it, start/end is just the bar range.)
#[tauri::command(rename_all = "snake_case")]
pub fn flat_files_download(
    kind:      String,
    start_day: String,
    end_day:   String,
    state:     tauri::State<'_, AppState>,
) -> Result<(), String> {
    let kind = crate::flat_files::Kind::from_str(&kind)
        .ok_or_else(|| format!("type de flat file inconnu: {kind}"))?;
    let (key, secret, massive_key) = {
        let s = state.secrets.read().unwrap();
        (
            s.alpaca_key.clone().unwrap_or_default(),
            s.alpaca_secret.clone().unwrap_or_default(),
            s.massive_api_key.clone().unwrap_or_default(),
        )
    };
    crate::flat_files::start_download(
        state.flat_files.clone(),
        state.app_dir.clone(),
        state.db.clone(),
        key,
        secret,
        massive_key,
        kind,
        start_day,
        end_day,
    )
}

/// Request cancellation of the running download (takes effect between days).
#[tauri::command(rename_all = "snake_case")]
pub fn flat_files_cancel(state: tauri::State<'_, AppState>) {
    state.flat_files.request_cancel();
}

#[tauri::command(rename_all = "snake_case")]
pub fn get_flat_files_status(
    state: tauri::State<'_, AppState>,
) -> crate::flat_files::FlatFilesStatus {
    state.flat_files.status.read().unwrap().clone()
}

/// Every day present on disk for `kind` (downloaded or imported from another user),
/// for the calendar. Picks up dropped-in files on the next call.
#[tauri::command(rename_all = "snake_case")]
pub fn get_flat_files_calendar(
    kind:  String,
    state: tauri::State<'_, AppState>,
) -> Vec<crate::flat_files::FlatFileDay> {
    let Some(kind) = crate::flat_files::Kind::from_str(&kind) else { return Vec::new() };
    crate::flat_files::calendar(&state.app_dir, kind)
}

/// Open a flat-files folder in the OS file manager (to copy/share the files). `kind`
/// opens that subfolder; an unknown kind opens the root `flat_files/`.
#[tauri::command(rename_all = "snake_case")]
pub fn open_flat_files_folder(
    kind:  String,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let dir = match crate::flat_files::Kind::from_str(&kind) {
        Some(k) => crate::flat_files::kind_dir(&state.app_dir, k),
        None => crate::flat_files::flat_dir(&state.app_dir),
    };
    crate::flat_files::ensure_layout(&state.app_dir);
    crate::dashboard::open_folder(&dir)
}

// ─── Company intelligence (read-only; collection happens in the background) ────
// These commands NEVER make network calls themselves — they only read the local
// SQLite cache the `crate::company_intel` job populates in the background. The one
// refresh command spawns a background task and returns immediately, so the UI
// never blocks and never drives a network request directly.

/// The self-describing `company_intel` catalog: every captured datum, its label,
/// section, source and type. Lets the UI render the data without hard-coding
/// field names.
#[tauri::command(rename_all = "snake_case")]
pub fn get_company_intel_catalog() -> Vec<crate::company_intel::IntelField> {
    crate::company_intel::catalog().to_vec()
}

/// The cached company-intel record for one ticker (None until collected).
#[tauri::command(rename_all = "snake_case")]
pub fn get_company_intel(
    symbol: String,
    state: tauri::State<'_, AppState>,
) -> Option<crate::company_intel::CompanyIntel> {
    crate::company_intel::get_company_intel(&state.db, &symbol)
}

/// Request a background refresh of one ticker's company intel. Spawns the
/// collection job on the async runtime and returns immediately — the network work
/// runs in the backend, never on the UI path.
#[tauri::command(rename_all = "snake_case")]
pub fn refresh_company_intel(symbol: String, state: tauri::State<'_, AppState>) {
    let db = state.db.clone();
    let config = state.config.clone();
    let secrets = state.secrets.clone();
    tauri::async_runtime::spawn(async move {
        crate::company_intel::refresh_company_intel(db, config, secrets, symbol).await;
    });
}

/// A bounded EXTRACT of the tickers data table: the universe DB joined with every
/// enrichment source (fundamentals, company meta, company intel) plus news /
/// filings counts. Empty `query` → the most recently collected rows; otherwise →
/// tickers matching the query (symbol prefix or name contains). Read-only — a
/// snapshot of the local DB for the data-table view, no network. Bounded so the UI
/// never loads the whole universe at once.
#[tauri::command(rename_all = "snake_case")]
pub fn get_tickers_table(
    query: Option<String>,
    limit: Option<u32>,
    state: tauri::State<'_, AppState>,
) -> Vec<crate::company_intel::TickerTableRow> {
    let query = query.unwrap_or_default();
    let limit = limit.unwrap_or(200);
    crate::company_intel::tickers_table(&state.db, &state.market, &query, limit)
}

// ─── Dashboard (moodboard) ────────────────────────────────────────────────────

/// Re-sync the user's trades from TradeTally (source of truth) into the local
/// `tt_trades` cache. Called every time the dashboard tab opens, and on demand via
/// the Refresh button. Returns the number of trades upserted.
#[tauri::command(rename_all = "snake_case")]
pub async fn sync_tradetally_trades(state: tauri::State<'_, AppState>) -> Result<usize, String> {
    // Build the TradeTally client the same way the background worker does.
    let (base_url, token, mock_mode, mock_fail, mock_delay, tt_email, tt_password) = {
        let cfg = state.config.read().unwrap();
        let sec = state.secrets.read().unwrap();
        (
            cfg.tradetally.api_base_url.clone(),
            sec.tradetally_token.clone().unwrap_or_default(),
            cfg.tradetally.mock_mode,
            cfg.tradetally.mock_fail,
            cfg.tradetally.mock_delay_ms,
            sec.tradetally_email.clone(),
            sec.tradetally_password.clone(),
        )
    };
    if token.is_empty() && !mock_mode {
        return Err("TradeTally token not set".into());
    }

    let client = tradetally::TtClient::new(base_url, token, mock_mode)
        .with_mock_options(mock_fail, mock_delay)
        .with_session_creds(tt_email, tt_password);

    // Network fetch happens with no DB lock held.
    let trades = crate::dashboard::sync_trades(&client).await?;

    let count = {
        let mut guard = state.db.lock().unwrap();
        let n = dashboard_repository::upsert_trades_bulk(&mut guard, &trades)
            .map_err(|e| e.to_string())?;
        let _ = cache_repository::set_app_meta(&guard, "tt_trades_synced_at", &Utc::now().to_rfc3339());
        n
    };
    Ok(count)
}

/// Cached trades for the dashboard KPI cards (oldest first). The frontend derives
/// profit factor, PnL curve, etc. from these.
#[tauri::command(rename_all = "snake_case")]
pub fn get_dashboard_trades(state: tauri::State<'_, AppState>) -> Vec<crate::dashboard::DashboardTrade> {
    let conn = state.db.lock().unwrap();
    dashboard_repository::get_all_trades(&conn).unwrap_or_default()
}

/// Journal/diary card → create-or-update today's TradeTally diary entry. Enqueued
/// on the resilient sync queue (drained by the background worker) and mirrored
/// locally. `entry_date` is today's ET calendar day.
#[tauri::command(rename_all = "snake_case")]
pub fn save_diary_entry(
    title:   String,
    content: String,
    state:   tauri::State<'_, AppState>,
) -> Result<(), String> {
    let entry_date = crate::time::et_date(crate::time::now());
    let id = format!("{}-{}", entry_date, Utc::now().timestamp_millis());
    let conn = state.db.lock().unwrap();
    tradetally::enqueue_diary_entry(&conn, &entry_date, &title, &content);
    dashboard_repository::insert_diary_local(&conn, &id, &entry_date, &title, &content)
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Today's background image (deterministic per ET day) from the user's photo
/// folder, plus the folder path so the UI can show / open it.
#[tauri::command(rename_all = "snake_case")]
pub fn get_daily_background(state: tauri::State<'_, AppState>) -> crate::dashboard::DailyBackground {
    crate::dashboard::pick_daily_background(&state.app_dir)
}

/// Open the backgrounds folder in the OS file manager.
#[tauri::command(rename_all = "snake_case")]
pub fn open_backgrounds_folder(state: tauri::State<'_, AppState>) -> Result<(), String> {
    crate::dashboard::open_folder(&state.app_dir.join("backgrounds"))
}

/// A fresh random mood pick (image + short + long phrase) from the user's `mood/`
/// folder. Re-randomises on every call (each dashboard open / refresh).
#[tauri::command(rename_all = "snake_case")]
pub fn get_mood(state: tauri::State<'_, AppState>) -> crate::dashboard::Mood {
    crate::dashboard::pick_mood(&state.app_dir)
}

/// Open a mood drop target: `"images"` folder, or the `"short"` / `"long"` phrases
/// `.txt` file (in the OS default editor).
#[tauri::command(rename_all = "snake_case")]
pub fn open_mood_target(
    state: tauri::State<'_, AppState>,
    target: String,
) -> Result<(), String> {
    crate::dashboard::open_mood_target(&state.app_dir, &target)
}

/// The bundled default dashboard layout JSON (shipped with the app). The frontend
/// uses it to seed a brand-new user's board; users with a saved layout ignore it.
#[tauri::command(rename_all = "snake_case")]
pub fn get_default_dashboard(app: tauri::AppHandle) -> Option<String> {
    use tauri::Manager;
    let dir = app
        .path()
        .resolve("resources/defaults", tauri::path::BaseDirectory::Resource)
        .ok()?;
    crate::dashboard::read_default_dashboard(&dir)
}

/// Save the current dashboard layout to `<app_dir>/dashboard-default.json`, so the
/// maintainer can capture their arrangement and bundle it as the new shipped
/// default. Returns the written file path. (Maintainer affordance.)
#[tauri::command(rename_all = "snake_case")]
pub fn export_dashboard_default(
    layout_json: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    crate::dashboard::write_dashboard_export(&state.app_dir, &layout_json)?;
    Ok(state.app_dir.join("dashboard-default.json").to_string_lossy().to_string())
}

// ─── Embedded TradeTally web app ──────────────────────────────────────────────
//
// The TradeTally site sends `X-Frame-Options` / CSP `frame-ancestors` → no
// iframe. We embed it as a real native webview that loads the site as a top-level
// document, so the frame-blocking headers don't apply.
//
// Platform split — this matters:
//   • Windows: a separate **child window** (`WebviewWindowBuilder::parent_raw`,
//     WS_CHILD) confined to and auto-following the main window's client area.
//     The earlier approach (`Window::add_child`, Tauri's `unstable` multi-webview)
//     is broken on Windows/WebView2: putting a second webview inside the main
//     window corrupts its shared compositor — the page wouldn't render, and the
//     first modal opened afterwards (a DOM dialog in the main webview) silently
//     crashed the renderer. A separate HWND with its own WebView2 controller
//     doesn't touch the main window's compositor, so both symptoms go away.
//   • macOS / Linux: `Window::add_child` (the multi-webview API is mature on
//     WKWebView / WebKitGTK), unchanged.
//
// Isolated storage: the webview gets its OWN WebView2/WKWebView data directory
// (`<app_dir>/tradetally-webview`), separate from the main app's. So TradeTally's
// cookies/cache/localStorage can't corrupt (or be corrupted by) the app's own
// localStorage (dashboard layout, hotkeys…), and "clear cache & cookies" only
// wipes TradeTally — never the app. Cookies persist across the destroy/recreate.
//
// Lifecycle = *destroy*, not hide. It exists ONLY while the TradeTally tab is the
// active view and no modal is open; the frontend creates it (`tradetally_set_bounds`)
// on enter / modal-close and destroys it (`tradetally_close`) on leave / modal-open.
//
// The commands are `async` so window/webview creation runs on the tokio runtime;
// the runtime dispatches the actual creation to the main thread without
// deadlocking the caller.

const TRADETALLY_LABEL: &str = "tradetally";

/// The TradeTally site URL (the configured base; the SPA routes to /dashboard or
/// /login itself). Same base as the REST client, so it can't carry a path.
fn tradetally_url(app: &tauri::AppHandle) -> Result<tauri::Url, String> {
    use tauri::Manager;
    let base = app
        .state::<AppState>()
        .config
        .read()
        .unwrap()
        .tradetally
        .api_base_url
        .clone();
    tauri::Url::parse(&base).map_err(|e| e.to_string())
}

/// Dedicated WebView2/WKWebView data dir so TradeTally's web storage is isolated
/// from the app's own (and from any cache corruption).
fn tradetally_data_dir(app: &tauri::AppHandle) -> std::path::PathBuf {
    use tauri::Manager;
    app.state::<AppState>().app_dir.join("tradetally-webview")
}

#[tauri::command(rename_all = "snake_case")]
pub async fn tradetally_set_bounds(
    app:    tauri::AppHandle,
    x:      f64,
    y:      f64,
    width:  f64,
    height: f64,
) -> Result<(), String> {
    tt_place(&app, x, y, width.max(1.0), height.max(1.0))
}

/// Windows: create-or-reposition the TradeTally child window (WS_CHILD over the
/// main window's client area).
#[cfg(windows)]
fn tt_place(app: &tauri::AppHandle, x: f64, y: f64, width: f64, height: f64) -> Result<(), String> {
    use tauri::Manager;
    let pos  = tauri::LogicalPosition::new(x, y);
    let size = tauri::LogicalSize::new(width, height);

    if let Some(ww) = app.get_webview_window(TRADETALLY_LABEL) {
        ww.set_position(pos).map_err(|e| e.to_string())?;
        ww.set_size(size).map_err(|e| e.to_string())?;
        ww.show().map_err(|e| e.to_string())?;
        return Ok(());
    }

    let url    = tradetally_url(app)?;
    let main   = app.get_webview_window("main").ok_or("main window not found")?;
    let parent = main.hwnd().map_err(|e| e.to_string())?;
    let app_evt = app.clone();

    match tauri::WebviewWindowBuilder::new(app, TRADETALLY_LABEL, tauri::WebviewUrl::External(url))
        .parent_raw(parent)
        .data_directory(tradetally_data_dir(app))
        .decorations(false)
        .shadow(false)
        .skip_taskbar(true)
        .focused(false)
        .resizable(false)
        .position(x, y)
        .inner_size(width, height)
        // Tell the React placeholder the page finished loading (status dot → ready).
        // Full-document loads only — SPA route changes don't fire this.
        .on_page_load(move |_w, payload| {
            if matches!(payload.event(), tauri::webview::PageLoadEvent::Finished) {
                use tauri::Emitter;
                let _ = app_evt.emit("tradetally-loaded", ());
            }
        })
        .build()
    {
        Ok(w) => w.show().map_err(|e| e.to_string())?,
        // Lost a create race (mount effects / dev StrictMode): reposition the winner.
        Err(e) => match app.get_webview_window(TRADETALLY_LABEL) {
            Some(w) => {
                w.set_position(pos).map_err(|e| e.to_string())?;
                w.set_size(size).map_err(|e| e.to_string())?;
                w.show().map_err(|e| e.to_string())?;
            }
            None => return Err(format!("failed to create tradetally window: {e}")),
        },
    }
    Ok(())
}

/// macOS / Linux: create-or-reposition the TradeTally child webview embedded in
/// the main window (the multi-webview API is stable on these platforms).
#[cfg(not(windows))]
fn tt_place(app: &tauri::AppHandle, x: f64, y: f64, width: f64, height: f64) -> Result<(), String> {
    use tauri::Manager;
    let pos  = tauri::LogicalPosition::new(x, y);
    let size = tauri::LogicalSize::new(width, height);

    if let Some(wv) = app.get_webview(TRADETALLY_LABEL) {
        wv.set_position(pos).map_err(|e| e.to_string())?;
        wv.set_size(size).map_err(|e| e.to_string())?;
        wv.show().map_err(|e| e.to_string())?;
        return Ok(());
    }

    let url    = tradetally_url(app)?;
    let window = app.get_window("main").ok_or("main window not found")?;
    let app_evt = app.clone();
    let builder = tauri::webview::WebviewBuilder::new(TRADETALLY_LABEL, tauri::WebviewUrl::External(url))
        .data_directory(tradetally_data_dir(app))
        .on_page_load(move |_wv, payload| {
            if matches!(payload.event(), tauri::webview::PageLoadEvent::Finished) {
                use tauri::Emitter;
                let _ = app_evt.emit("tradetally-loaded", ());
            }
        });

    match window.add_child(builder, pos, size) {
        Ok(wv) => wv.show().map_err(|e| e.to_string())?,
        Err(e) => match app.get_webview(TRADETALLY_LABEL) {
            Some(wv) => {
                wv.set_position(pos).map_err(|e| e.to_string())?;
                wv.set_size(size).map_err(|e| e.to_string())?;
                wv.show().map_err(|e| e.to_string())?;
            }
            None => return Err(format!("failed to create tradetally webview: {e}")),
        },
    }
    Ok(())
}

/// Destroy the embedded webview/window (tab left / modal opened). Cookies +
/// localStorage persist in the isolated data dir, so login + theme are kept.
#[tauri::command(rename_all = "snake_case")]
pub async fn tradetally_close(app: tauri::AppHandle) -> Result<(), String> {
    use tauri::Manager;
    #[cfg(windows)]
    if let Some(ww) = app.get_webview_window(TRADETALLY_LABEL) {
        ww.close().map_err(|e| e.to_string())?;
    }
    #[cfg(not(windows))]
    if let Some(wv) = app.get_webview(TRADETALLY_LABEL) {
        wv.close().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Soft reload of the embedded page (toolbar → Recharger). Keeps cache, cookies
/// and the current session — the first thing to try when the page looks stuck.
#[tauri::command(rename_all = "snake_case")]
pub async fn tradetally_reload(app: tauri::AppHandle) -> Result<(), String> {
    use tauri::Manager;
    #[cfg(windows)]
    if let Some(ww) = app.get_webview_window(TRADETALLY_LABEL) {
        ww.reload().map_err(|e| e.to_string())?;
    }
    #[cfg(not(windows))]
    if let Some(wv) = app.get_webview(TRADETALLY_LABEL) {
        wv.reload().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Clear ONLY TradeTally's cache + cookies + localStorage (its data dir is
/// isolated from the app's), then load a fresh page. Logs the user out of
/// TradeTally and resets its dark/light preference — the recovery path when a
/// corrupted cache leaves the page stuck loading. No-op if it's closed.
#[tauri::command(rename_all = "snake_case")]
pub async fn tradetally_clear_data(app: tauri::AppHandle) -> Result<(), String> {
    use tauri::Manager;
    let url = tradetally_url(&app)?;
    #[cfg(windows)]
    if let Some(ww) = app.get_webview_window(TRADETALLY_LABEL) {
        ww.clear_all_browsing_data().map_err(|e| e.to_string())?;
        ww.navigate(url).map_err(|e| e.to_string())?;
    }
    #[cfg(not(windows))]
    if let Some(wv) = app.get_webview(TRADETALLY_LABEL) {
        wv.clear_all_browsing_data().map_err(|e| e.to_string())?;
        wv.navigate(url).map_err(|e| e.to_string())?;
    }
    Ok(())
}

// ─── Flash alert overlay (built on demand, not at startup) ────────────────────

/// Create or destroy the full-screen white flash overlay window. Driven from
/// Settings: enabled when the flash cue is on (mode ≠ "off"), disabled otherwise.
/// Idempotent. `async` so window creation dispatches to the main thread without
/// deadlocking the caller.
#[tauri::command(rename_all = "snake_case")]
pub async fn set_flash_overlay(enabled: bool, app: tauri::AppHandle) -> Result<(), String> {
    if enabled {
        crate::notify::ensure_flash_overlay(&app)
    } else {
        crate::notify::close_flash_overlay(&app);
        Ok(())
    }
}

// ─── Speech-to-Text (offline dictée → trade notes / diary) ────────────────────────

/// Pipeline snapshot polled by the mic buttons + the STT modal (model presence,
/// download progress, recording flag, worker state/pause reason, the job queue).
#[tauri::command(rename_all = "snake_case")]
pub fn stt_status(state: tauri::State<'_, AppState>) -> crate::stt::SttStatus {
    let cfg = state.config.read().unwrap().stt.clone();
    state.stt.status(&cfg)
}

/// Download the configured whisper model (small/medium) if absent. Idempotent;
/// progress is surfaced through `stt_status`.
#[tauri::command(rename_all = "snake_case")]
pub async fn stt_download_model(state: tauri::State<'_, AppState>) -> Result<(), String> {
    if !crate::stt::platform_available() {
        return Err("dictée vocale non disponible sur ce Mac (macOS < 14)".into());
    }
    let model = state.config.read().unwrap().stt.model.clone();
    let shared = state.stt.clone();
    crate::stt::model::download_model(&shared, &model).await
}

/// Start capturing a dictée. `kind` is "trade" (with `trade_id`/`symbol`) or "diary".
#[tauri::command(rename_all = "snake_case")]
pub fn stt_start_recording(
    kind:     String,
    trade_id: Option<String>,
    symbol:   Option<String>,
    app:      tauri::AppHandle,
    state:    tauri::State<'_, AppState>,
) -> Result<(), String> {
    if !crate::stt::platform_available() {
        return Err("dictée vocale non disponible sur ce Mac (macOS < 14)".into());
    }
    let device = {
        let c = state.config.read().unwrap();
        if !c.stt.enabled {
            return Err("dictée vocale désactivée".into());
        }
        c.stt.input_device.clone()
    };
    let kind = match kind.as_str() {
        "trade" => crate::stt::JobKind::Trade,
        "diary" => crate::stt::JobKind::Diary,
        _ => return Err("kind invalide".into()),
    };
    crate::stt::start_recording(&state.stt, app, kind, trade_id, symbol, device)
}

/// Stop capturing, enqueue the dictée job, and return its id.
#[tauri::command(rename_all = "snake_case")]
pub fn stt_stop_recording(
    app:   tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    crate::stt::stop_recording(&state.stt, app)
}

/// Discard the in-progress recording.
#[tauri::command(rename_all = "snake_case")]
pub fn stt_cancel_recording(app: tauri::AppHandle, state: tauri::State<'_, AppState>) -> Result<(), String> {
    crate::stt::cancel_recording(&state.stt, app);
    Ok(())
}

/// Cancel a queued/running job.
#[tauri::command(rename_all = "snake_case")]
pub fn stt_cancel_job(id: String, app: tauri::AppHandle, state: tauri::State<'_, AppState>) -> Result<(), String> {
    crate::stt::cancel_job(&state.stt, &id, &app);
    Ok(())
}

/// Re-queue a failed job for another attempt.
#[tauri::command(rename_all = "snake_case")]
pub fn stt_retry_job(id: String, app: tauri::AppHandle, state: tauri::State<'_, AppState>) -> Result<(), String> {
    crate::stt::retry_job(&state.stt, &id, &app);
    Ok(())
}

/// Available input device names (for the mic-check UI).
#[tauri::command(rename_all = "snake_case")]
pub fn stt_list_input_devices() -> Vec<String> {
    crate::stt::recorder::list_input_devices()
}

/// Short blocking probe of the (configured/default) mic: returns its peak level so
/// the UI can confirm it's working.
#[tauri::command(rename_all = "snake_case")]
pub async fn stt_test_microphone(
    state: tauri::State<'_, AppState>,
) -> Result<crate::stt::MicTestResult, String> {
    let device = state.config.read().unwrap().stt.input_device.clone();
    tokio::task::spawn_blocking(move || crate::stt::recorder::test_microphone(device))
        .await
        .map_err(|e| e.to_string())
}
