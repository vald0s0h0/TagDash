// Scanner engine. Runs in a dedicated tokio task, polling MarketState every
// 500 ms. Per-ticker StrategyContext is built with a brief read lock; all
// strategy evaluation happens outside the lock.

pub mod alert_engine;

use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex, RwLock,
};
use std::time::Instant;

use tokio::time::Duration;

use crate::local_db::{
    alarm_repository, alarm_repository::PriceAlarm, scoring_repository,
    scoring_repository::ScoreRow, universe_repository,
};
use crate::market_state::MarketState;
use crate::strategies::{panic_mean_reversion, registry, StrategyKind};
use crate::types::{AlertSignal, ScreenerMatch, Session, StrategyContext};
use self::alert_engine::AlertEngine;

/// How many top-scored tickers the Panic Mean Reversion screener surfaces.
const MR_TOP_N: u32 = 30;

/// How often the per-symbol float + average-volume maps are reloaded from the
/// universe table (so in-script float/rvol filters use fresh values).
const FLOAT_REFRESH: Duration = Duration::from_secs(300);
/// How often the armed price-alarm set is reloaded from the DB.
const ALARM_REFRESH: Duration = Duration::from_secs(5);

pub struct ScannerEngine;

impl ScannerEngine {
    /// Spawn the background scan loop. Returns immediately.
    pub fn start(
        running:          Arc<AtomicBool>,
        active_alerts:    Arc<RwLock<Vec<AlertSignal>>>,
        alert_history:    Arc<RwLock<Vec<AlertSignal>>>,
        screener:         Arc<RwLock<Vec<ScreenerMatch>>>,
        strategy_enabled: Arc<RwLock<HashMap<String, bool>>>,
        market:           Arc<RwLock<MarketState>>,
        db:               Arc<Mutex<rusqlite::Connection>>,
    ) {
        tokio::spawn(async move {
            let strategies = registry::all_strategies();
            let mut engine = AlertEngine::new();
            // Market Replay reset watch: on a replay start / backward seek / new
            // day, drop the per-session state (cooldowns, alarm crossings) so the
            // replayed day starts clean.
            let mut replay_gen = crate::replay::clock::generation();

            // Per-symbol float (shares) + average daily volume, used by in-script
            // float/rvol filters. Refreshed periodically.
            let mut floats      = load_floats(&db);
            let mut avg_volumes = load_avg_volumes(&db);
            let mut floats_loaded = Instant::now();

            // Panic Mean Reversion watchlist rows (pre-open screener). Built once a
            // day at 09:00 ET by `crate::panic_watchlist`; reloaded on the float
            // cadence so the day's new list is picked up. The Panic strategy's
            // display name + priority come from the registry.
            let mut mr_scores = load_top_scores(&db);
            let panic_priority = strategies
                .iter()
                .find(|s| s.id() == panic_mean_reversion::ID)
                .map(|s| s.priority())
                .unwrap_or(5);
            let panic_name = "Panic Mean Reversion".to_string();

            // Armed price alarms (watched for level crossings) + last seen price
            // per alarm, so we fire on a genuine crossing rather than on first
            // sight. Reset list is rebuilt from the DB every ALARM_REFRESH.
            let mut alarms = load_untriggered_alarms(&db);
            let mut alarms_loaded = Instant::now();
            let mut alarm_prev: HashMap<String, f64> = HashMap::new();
            // Symbols already queued for on-demand "capacité à diluer" collection this
            // session, so we don't re-request the screener list every scan pass.
            let mut intel_requested: std::collections::HashSet<String> = std::collections::HashSet::new();

            while running.load(Ordering::Relaxed) {
                {
                    let g = crate::replay::clock::generation();
                    if g != replay_gen {
                        replay_gen = g;
                        engine = AlertEngine::new();
                        alarm_prev.clear();
                        mr_scores = load_top_scores(&db);
                    }
                }
                if floats_loaded.elapsed() >= FLOAT_REFRESH {
                    floats      = load_floats(&db);
                    avg_volumes = load_avg_volumes(&db);
                    mr_scores   = load_top_scores(&db);
                    floats_loaded = Instant::now();
                } else if mr_scores.is_empty() {
                    // The watchlist is built at 09:00 ET by `crate::panic_watchlist`,
                    // typically well after the scanner task started with an empty
                    // list. Poll cheaply (one indexed SELECT LIMIT 30) while empty so
                    // the pre-open screener fills the instant the list lands — rather
                    // than waiting up to the full 5-min FLOAT_REFRESH. Stops once
                    // populated.
                    mr_scores = load_top_scores(&db);
                }
                if alarms_loaded.elapsed() >= ALARM_REFRESH {
                    alarms = load_untriggered_alarms(&db);
                    alarms_loaded = Instant::now();
                }
                // App clock: simulated instant during a Market Replay.
                let now = crate::time::now();
                let session = crate::time::session_at(now);
                // Snapshot the runtime on/off map once per pass (cheap clone; a
                // handful of entries) so toggling a strategy in Settings takes
                // effect live without locking per ticker.
                let enabled_now = strategy_enabled.read().unwrap().clone();

                // Snapshot tickers with a brief read lock — no strategy logic inside.
                // Also read mock_running: in mock mode skip the session gate so the
                // feed can trigger alerts at any time of day. (Trade-acceleration and
                // news correlation used to be precomputed here for the old
                // micro_pullback; it is now a stateful engine — see
                // `crate::micro_pullback` — so the scanner no longer carries that.)
                let (tickers, is_mock) = {
                    let ms = market.read().unwrap();
                    let tickers = ms.tickers.values().cloned().collect::<Vec<_>>();
                    (tickers, ms.mock_running)
                };

                // Current price per symbol — feeds the alarm-crossing watcher.
                let price_map: HashMap<String, f64> = tickers
                    .iter()
                    .filter_map(|t| t.last_price.map(|p| (t.symbol.clone(), p)))
                    .collect();

                // Screener matches built fresh this pass; replaces the live list
                // wholesale so tickers drop off the instant they stop matching.
                let mut screener_now: Vec<ScreenerMatch> = Vec::new();

                for ticker in &tickers {
                    // Relative volume = day volume / average daily volume (universe
                    // table). None when the average isn't known yet.
                    let rvol = avg_volumes
                        .get(&ticker.symbol)
                        .filter(|&&v| v > 0)
                        .map(|&v| ticker.volume_day as f64 / v as f64);
                    let ctx = StrategyContext {
                        symbol:         ticker.symbol.clone(),
                        price:          ticker.last_price,
                        bid:            ticker.bid,
                        ask:            ticker.ask,
                        spread:         ticker.spread,
                        volume_day:     ticker.volume_day,
                        vwap:           ticker.vwap,
                        high_day:       ticker.high_day,
                        low_day:        ticker.low_day,
                        previous_close: ticker.previous_close,
                        change_day_pct: ticker.change_day_pct,
                        rvol,
                        float_shares: floats.get(&ticker.symbol).copied(),
                    };

                    for strategy in strategies {
                        // Runtime on/off (Settings toggle) → compiled default.
                        let is_enabled = enabled_now
                            .get(strategy.id())
                            .copied()
                            .unwrap_or_else(|| strategy.enabled());
                        if !is_enabled {
                            continue;
                        }

                        match strategy.kind() {
                            // Screener strategies (pre-open watchlist) are evaluated
                            // every pass regardless of the session gate, so the tab
                            // is live whenever the app runs. No cooldown/dedup: the
                            // full current match set replaces the list each pass.
                            StrategyKind::Screener => {
                                if strategy.matches(&ctx) {
                                    screener_now.push(ScreenerMatch {
                                        symbol:        ctx.symbol.clone(),
                                        strategy_id:   strategy.id().to_string(),
                                        strategy_name: strategy.name().to_string(),
                                        priority:      strategy.priority(),
                                        price:         ctx.price,
                                        gap_pct:       ctx.change_day_pct,
                                        rvol:          ctx.rvol,
                                        volume:        ctx.volume_day,
                                        float_shares:  ctx.float_shares,
                                        score:         None,
                                        score_label:   None,
                                        updated_at:    now,
                                    });
                                }
                            }
                            // Alert strategies: session-gated + cooldown'd events.
                            StrategyKind::Alert => {
                                if !is_mock && !strategy.sessions().contains(&session) {
                                    continue;
                                }
                                if let Some(mut signal) = strategy.should_alert(&ctx) {
                                    signal.session = if is_mock { Session::Open } else { session };
                                    if let Some(alert) = engine.process(&signal, strategy.cooldown()) {
                                        push_alert(&active_alerts, &alert_history, alert);
                                    }
                                }
                            }
                        }
                    }
                }

                // ── Panic Mean Reversion: precomputed daily watchlist ───────────
                // Sourced from `panic_watchlist` (not per-tick), so the watchlist
                // shows even for symbols quiet right now. Live price / volume / gap
                // are filled from RAM when available.
                let panic_on = enabled_now
                    .get(panic_mean_reversion::ID)
                    .copied()
                    .unwrap_or(true);
                if panic_on && !mr_scores.is_empty() {
                    let by_sym: HashMap<&str, &crate::market_state::TickerLiveState> =
                        tickers.iter().map(|t| (t.symbol.as_str(), t)).collect();
                    for row in &mr_scores {
                        let t = by_sym.get(row.symbol.as_str());
                        // The card shows the PREVIOUS day's volume (the move this
                        // screener is built on happened yesterday), not the (often
                        // zero pre-open) live day volume.
                        let prev_volume = row.prev_volume.unwrap_or(0).max(0) as u64;
                        screener_now.push(ScreenerMatch {
                            symbol:        row.symbol.clone(),
                            strategy_id:   panic_mean_reversion::ID.to_string(),
                            strategy_name: panic_name.clone(),
                            priority:      panic_priority,
                            price:         t.and_then(|t| t.last_price),
                            gap_pct:       t.and_then(|t| t.change_day_pct),
                            rvol:          None, // not meaningful for a prior-day ranking
                            volume:        prev_volume,
                            float_shares:  floats.get(&row.symbol).copied(),
                            score:         Some(row.display_score),
                            score_label:   {
                                // List tag + metric value + direction, e.g. "BB 4.2 ▲"
                                // / "MA 3.1 ▼". The list ("BB" Bollinger area, "MA"
                                // move since SMA20 contact) tells the user WHY the
                                // ticker is on the watchlist.
                                let arrow = match row.direction {
                                    d if d > 0 => " ▲",
                                    d if d < 0 => " ▼",
                                    _ => "",
                                };
                                Some(format!("{} {:.1}{}", row.list_kind, row.value, arrow))
                            },
                            updated_at:    now,
                        });
                    }
                }

                // Replace the live screener list. Sort by score (scored screeners
                // first, strongest first); ties broken by volume (previous-day for
                // Panic), then by gap. Keep a single row per ticker (the best-
                // scoring one) so a symbol never appears twice.
                screener_now.sort_by(|a, b| {
                    let sa = a.score.unwrap_or(f64::MIN);
                    let sb = b.score.unwrap_or(f64::MIN);
                    sb.partial_cmp(&sa)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| b.volume.cmp(&a.volume))
                        .then_with(|| {
                            b.gap_pct
                                .unwrap_or(f64::MIN)
                                .partial_cmp(&a.gap_pct.unwrap_or(f64::MIN))
                                .unwrap_or(std::cmp::Ordering::Equal)
                        })
                });
                let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
                screener_now.retain(|m| seen.insert(m.symbol.clone()));
                // Queue any newly-surfaced screener ticker for on-demand "capacité à
                // diluer" collection (premarket dilution readiness). Deduped here so we
                // don't re-request every 500 ms pass; the worker also TTL-gates.
                for m in &screener_now {
                    if intel_requested.insert(m.symbol.clone()) {
                        crate::company_intel::ondemand::request(&m.symbol);
                    }
                }
                *screener.write().unwrap() = screener_now;

                // ── Alarm watcher: fire an Open alert on a level crossing ───────
                watch_alarms(
                    &db, &mut alarms, &mut alarm_prev, &price_map,
                    &active_alerts, &alert_history,
                );

                // 500 ms of market time: real 500 ms live, divided by the speed
                // during an accelerated replay so the scan cadence in simulated
                // seconds matches the live cadence.
                crate::replay::clock::scaled_sleep(500).await;
            }
        });
    }
}

/// Prepend alert to active list (dedup by symbol+strategy) and to history.
pub fn push_alert(
    active_alerts: &Arc<RwLock<Vec<AlertSignal>>>,
    alert_history: &Arc<RwLock<Vec<AlertSignal>>>,
    alert: AlertSignal,
) {
    // Every strategy + the alarm watcher funnel through here, already cooldown-
    // gated, so this is the right place to fire the low-latency desktop attention
    // cue (white flash / foreground) for a genuinely new alert.
    let session = alert.session;
    // Just-in-time "capacité à diluer" collection for an alerted ticker (premarket
    // dilution readiness). Non-blocking; the worker dedups + TTL-gates.
    crate::company_intel::ondemand::request(&alert.symbol);
    {
        let mut active = active_alerts.write().unwrap();
        // Keep only one entry per (symbol, strategy) in the active list
        active.retain(|a| {
            !(a.symbol == alert.symbol && a.strategy_id == alert.strategy_id)
        });
        active.insert(0, alert.clone());
        if active.len() > 100 {
            active.truncate(100);
        }
    }
    {
        let mut history = alert_history.write().unwrap();
        history.insert(0, alert);
        if history.len() > 500 {
            history.truncate(500);
        }
    }
    crate::notify::on_alert(session);
}

/// Load the per-symbol float (shares) map from the universe table. Symbols with
/// no known float are omitted (the strategy decides how to treat `None`).
fn load_floats(db: &Arc<Mutex<rusqlite::Connection>>) -> HashMap<String, u64> {
    let conn = db.lock().unwrap();
    universe_repository::get_all(&conn)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|a| a.float_shares.filter(|f| *f > 0).map(|f| (a.symbol, f as u64)))
        .collect()
}

/// Load the per-symbol average daily volume from the universe table (used to
/// compute relative volume). Symbols with no known average are omitted.
fn load_avg_volumes(db: &Arc<Mutex<rusqlite::Connection>>) -> HashMap<String, u64> {
    let conn = db.lock().unwrap();
    universe_repository::get_all(&conn)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|a| a.avg_volume.filter(|v| *v > 0).map(|v| (a.symbol, v as u64)))
        .collect()
}

/// Load the Panic Mean Reversion watchlist rows (interleaved BB/MA order) for the
/// pre-open screener. Empty until the 09:00 ET daily build has run.
fn load_top_scores(db: &Arc<Mutex<rusqlite::Connection>>) -> Vec<ScoreRow> {
    let conn = db.lock().unwrap();
    // No volume gate here: the premarket pre-filter (see `crate::scoring`) is the
    // liquidity filter now, and the table already holds only the merged ≤20 rows.
    scoring_repository::get_top(&conn, MR_TOP_N, 0).unwrap_or_default()
}

/// Load armed (not-yet-triggered) price alarms from the DB.
fn load_untriggered_alarms(db: &Arc<Mutex<rusqlite::Connection>>) -> Vec<PriceAlarm> {
    let conn = db.lock().unwrap();
    alarm_repository::get_untriggered(&conn).unwrap_or_default()
}

/// Check every armed alarm against the latest price. A crossing of the level
/// (price moving from one side to the other, or touching it) fires an Open-tab
/// alert and stamps the alarm as triggered so it can't re-fire. `alarm_prev`
/// holds the last seen price per alarm so we never fire on first sight.
fn watch_alarms(
    db:            &Arc<Mutex<rusqlite::Connection>>,
    alarms:        &mut Vec<PriceAlarm>,
    alarm_prev:    &mut HashMap<String, f64>,
    price_map:     &HashMap<String, f64>,
    active_alerts: &Arc<RwLock<Vec<AlertSignal>>>,
    alert_history: &Arc<RwLock<Vec<AlertSignal>>>,
) {
    let mut triggered_ids: Vec<String> = Vec::new();

    for alarm in alarms.iter() {
        let Some(&price) = price_map.get(&alarm.symbol) else { continue };
        let level = alarm.price;
        let crossed = match alarm_prev.get(&alarm.id) {
            Some(&prev) => (prev < level && price >= level) || (prev > level && price <= level),
            None => false, // first sight: record only, don't fire
        };
        alarm_prev.insert(alarm.id.clone(), price);

        if crossed {
            triggered_ids.push(alarm.id.clone());
            let (name, priority) = alarm
                .strategy_id
                .as_deref()
                .and_then(|sid| {
                    registry::all_strategies()
                        .iter()
                        .find(|s| s.id() == sid)
                        .map(|s| (s.name().to_string(), s.priority()))
                })
                .unwrap_or_else(|| ("Alarme".to_string(), 5));

            let now = crate::time::now();
            let alert = AlertSignal {
                alert_id:       format!("alarm-{}-{}", alarm.id, now.timestamp_millis()),
                timestamp:      now,
                symbol:         alarm.symbol.clone(),
                strategy_id:    alarm.strategy_id.clone().unwrap_or_default(),
                strategy_name:  name,
                priority,
                // Surfaces in the Open-tab sidebar.
                session:        Session::Open,
                price:          Some(price),
                bid:            None,
                ask:            None,
                spread:         None,
                volume:         None,
                rvol:           None,
                change_day_pct: None,
                float_shares:   None,
                news_today:     false,
                halted:         Some(false),
                latency_ui_ms:  None,
                reason:         format!("Alarme déclenchée à ${:.2} — touché ${:.2}", level, price),
                display_timeframe: None,
                side:           None,
            };
            push_alert(active_alerts, alert_history, alert);
        }
    }

    if !triggered_ids.is_empty() {
        // During a Market Replay the alert fires normally but the REAL armed
        // alarm is not stamped in the DB: a replayed price crossing must never
        // consume an alarm the user armed for the live market. (It is still
        // removed from the in-RAM list so it doesn't refire in this replay; the
        // periodic DB reload re-arms it after the session reset.)
        if !crate::replay::clock::is_active() {
            let conn = db.lock().unwrap();
            for id in &triggered_ids {
                let _ = alarm_repository::mark_triggered(&conn, id);
            }
        }
        alarms.retain(|a| !triggered_ids.contains(&a.id));
        for id in &triggered_ids {
            alarm_prev.remove(id);
        }
    }
}

// Market-session resolution now lives in `crate::time::session_at` (DST-aware,
// shared with the engines and the live/news streams).
