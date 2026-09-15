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

use self::alert_engine::AlertEngine;
use crate::local_db::{
    alarm_repository, alarm_repository::PriceAlarm, scoring_repository,
    scoring_repository::ScoreRow, universe_repository,
};
use crate::market_state::MarketState;
use crate::strategies::{panic_mean_reversion, registry, StrategyKind};
use crate::types::{AlertSignal, ScreenerMatch, Session, StrategyContext};

/// How many top-scored tickers the Panic Mean Reversion screener surfaces. Public
/// because `panic_llm`'s catch-up wave reads the SAME depth: if the two drifted, the
/// analysis would either miss visible tickers or score invisible ones.
pub const MR_TOP_N: u32 = 30;

/// How often the per-symbol float + average-volume maps are reloaded from the
/// universe table (so in-script float/rvol filters use fresh values).
const FLOAT_REFRESH: Duration = Duration::from_secs(300);
/// Watchlist builds refresh throughout premarket; poll its tiny day-scoped table
/// independently so a new shock appears within seconds, not one float cycle later.
const MR_REFRESH: Duration = Duration::from_secs(2);
/// How often the armed price-alarm set is reloaded from the DB.
const ALARM_REFRESH: Duration = Duration::from_secs(5);
/// How often the Panic Score (LLM) readings are reloaded. Short because the user
/// triggers the analysis by hand and watches the list reorder as it lands.
const LLM_SCORE_REFRESH: Duration = Duration::from_secs(2);

pub struct ScannerEngine;

impl ScannerEngine {
    /// Spawn the background scan loop. Returns immediately.
    pub fn start(
        running: Arc<AtomicBool>,
        active_alerts: Arc<RwLock<Vec<AlertSignal>>>,
        alert_history: Arc<RwLock<Vec<AlertSignal>>>,
        screener: Arc<RwLock<Vec<ScreenerMatch>>>,
        strategy_enabled: Arc<RwLock<HashMap<String, bool>>>,
        market: Arc<RwLock<MarketState>>,
        db: Arc<Mutex<rusqlite::Connection>>,
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
            let mut floats = load_floats(&db);
            let mut avg_volumes = load_avg_volumes(&db);
            let mut company_meta = load_company_meta(&db);
            let mut market_caps = load_market_caps(&db);
            let mut inst_ownership = load_institutional_ownership(&db);
            let mut floats_loaded = Instant::now();

            // Panic Mean Reversion watchlist rows (pre-open screener). Refreshed in
            // premarket by `crate::panic_watchlist`; reloaded on its own short cadence.
            // The Panic strategy's
            // display name + priority come from the registry.
            //
            // `mr_day` is the day the cached rows belong to. Without it this cache —
            // refreshed only every 5 minutes — would keep rendering the live list
            // after a Market Replay of another day starts, defeating the day-scoping
            // in the table. It also covers the plain ET midnight rollover.
            let mut mr_day = String::new();
            let mut mr_scores: Vec<ScoreRow> = Vec::new();
            let mut mr_loaded = Instant::now();
            // Panic Score (LLM) readings for the same day, keyed by symbol. Reloaded
            // on the same cadence as `mr_scores` so a run finishing mid-session is
            // picked up without a restart. Stays empty until the user presses the
            // analysis button — nothing here is ever produced automatically.
            let mut llm_scores: HashMap<
                String,
                crate::local_db::panic_llm_repository::PanicLlmScore,
            > = HashMap::new();
            let mut llm_loaded = Instant::now();
            // Panic score from each symbol's previous 1-2 SCORED sessions (not
            // necessarily yesterday/day-before-yesterday by the calendar — weekends
            // and gaps are skipped, see `load_llm_history`). Reloaded on the same
            // cadence as `llm_scores`; index 0 = most recent prior day.
            let mut llm_history: HashMap<String, [Option<f64>; 2]> = HashMap::new();
            let panic_priority = strategies
                .iter()
                .find(|s| s.id() == panic_mean_reversion::ID)
                .map(|s| s.priority())
                .unwrap_or(5);
            let panic_name = "Panic".to_string();

            // Armed price alarms (watched for level crossings) + last seen price
            // per alarm, so we fire on a genuine crossing rather than on first
            // sight. Reset list is rebuilt from the DB every ALARM_REFRESH.
            let mut alarms = load_untriggered_alarms(&db);
            let mut alarms_loaded = Instant::now();
            let mut alarm_prev: HashMap<String, f64> = HashMap::new();
            // Symbols already queued for on-demand "capacité à diluer" collection this
            // session, so we don't re-request the screener list every scan pass.
            let mut intel_requested: std::collections::HashSet<String> =
                std::collections::HashSet::new();

            while running.load(Ordering::Relaxed) {
                // App clock: the simulated day during a Market Replay. Resolved first
                // because the watchlist cache is keyed by it.
                let today = crate::time::et_date(crate::time::now());
                {
                    let g = crate::replay::clock::generation();
                    if g != replay_gen {
                        replay_gen = g;
                        engine = AlertEngine::new();
                        alarm_prev.clear();
                        mr_day.clear(); // force a reload for the (possibly new) day
                    }
                }
                if today != mr_day {
                    // Day changed (replay start/stop/next-day, or ET midnight): drop
                    // the previous day's rows immediately rather than showing them.
                    mr_day = today.clone();
                    mr_scores = load_top_scores(&db, &mr_day);
                    mr_loaded = Instant::now();
                    llm_scores = load_llm_scores(&db, &mr_day);
                    llm_history = load_llm_history(&db, &mr_day);
                } else if mr_loaded.elapsed() >= MR_REFRESH {
                    mr_scores = load_top_scores(&db, &mr_day);
                    mr_loaded = Instant::now();
                }
                if floats_loaded.elapsed() >= FLOAT_REFRESH {
                    floats = load_floats(&db);
                    avg_volumes = load_avg_volumes(&db);
                    company_meta = load_company_meta(&db);
                    market_caps = load_market_caps(&db);
                    inst_ownership = load_institutional_ownership(&db);
                    floats_loaded = Instant::now();
                }
                if alarms_loaded.elapsed() >= ALARM_REFRESH {
                    alarms = load_untriggered_alarms(&db);
                    alarms_loaded = Instant::now();
                }
                // Panic Score refresh, on its OWN short cadence rather than the 5-min
                // float one: the user presses the button and watches the list reorder,
                // so a five-minute lag would read as the feature not working. One
                // indexed SELECT over ~20 rows every 2 s is negligible.
                if llm_loaded.elapsed() >= LLM_SCORE_REFRESH {
                    llm_scores = load_llm_scores(&db, &mr_day);
                    llm_history = load_llm_history(&db, &mr_day);
                    llm_loaded = Instant::now();
                }
                // App clock: simulated instant during a Market Replay.
                let now = crate::time::now();
                let session = crate::time::session_at(now);
                // Snapshot the runtime on/off map once per pass (cheap clone; a
                // handful of entries) so toggling a strategy in Settings takes
                // effect live without locking per ticker.
                let enabled_now = strategy_enabled.read().unwrap().clone();

                // Snapshot tickers with a brief read lock — no strategy logic inside.
                // Trade-acceleration and news correlation used to be precomputed
                // here for the old
                // micro_pullback; it is now a stateful engine — see
                // `crate::micro_pullback` — so the scanner no longer carries that.)
                let tickers = {
                    let ms = market.read().unwrap();
                    ms.tickers.values().cloned().collect::<Vec<_>>()
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
                        symbol: ticker.symbol.clone(),
                        price: ticker.last_price,
                        bid: ticker.bid,
                        ask: ticker.ask,
                        spread: ticker.spread,
                        volume_day: ticker.volume_day,
                        vwap: ticker.vwap,
                        high_day: ticker.high_day,
                        low_day: ticker.low_day,
                        previous_close: ticker.previous_close,
                        change_day_pct: ticker.change_day_pct,
                        rvol,
                        float_shares: floats.get(&ticker.symbol).copied(),
                        market_cap: market_caps.get(&ticker.symbol).copied(),
                        institutional_ownership_pct: inst_ownership.get(&ticker.symbol).copied(),
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
                                        symbol: ctx.symbol.clone(),
                                        strategy_id: strategy.id().to_string(),
                                        strategy_name: strategy.name().to_string(),
                                        priority: strategy.priority(),
                                        price: ctx.price,
                                        gap_pct: ctx.change_day_pct,
                                        rvol: ctx.rvol,
                                        volume: Some(ctx.volume_day),
                                        float_shares: ctx.float_shares,
                                        industry: company_meta
                                            .get(&ctx.symbol)
                                            .and_then(|m| m.0.clone()),
                                        country: company_meta
                                            .get(&ctx.symbol)
                                            .and_then(|m| m.1.clone()),
                                        score: None,
                                        score_label: None,
                                        llm_panic_score: None,
                                        llm_one_liner: None,
                                        llm_reversibility: None,
                                        llm_ranking_score: None,
                                        llm_confidence_score: None,
                                        llm_eligibility: None,
                                        llm_timing_state: None,
                                        llm_trade_direction: None,
                                        llm_veto_reason: None,
                                        llm_panic_score_prev1: None,
                                        llm_panic_score_prev2: None,
                                        updated_at: now,
                                    });
                                }
                            }
                            // Alert strategies: session-gated + cooldown'd events.
                            StrategyKind::Alert => {
                                if !strategy.sessions().contains(&session) {
                                    continue;
                                }
                                if let Some(mut signal) = strategy.should_alert(&ctx) {
                                    signal.session = session;
                                    if let Some(alert) =
                                        engine.process(&signal, strategy.cooldown())
                                    {
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
                // Pinned tickers are surfaced even when they no longer score (that is
                // the point of a pin: "keep showing me this"). Re-read cheaply on the
                // same cadence as the watchlist so a pin placed in the UI appears
                // without waiting for the float refresh.
                let pinned: Vec<String> = {
                    let conn = db.lock().unwrap();
                    crate::local_db::screener_repository::pins(
                        &conn,
                        crate::replay::clock::is_active(),
                    )
                };

                if panic_on && (!mr_scores.is_empty() || !pinned.is_empty()) {
                    let by_sym: HashMap<&str, &crate::market_state::TickerLiveState> =
                        tickers.iter().map(|t| (t.symbol.as_str(), t)).collect();
                    // A pinned symbol that IS on today's list keeps its real score;
                    // one that dropped off gets a synthesized row below.
                    let scored: std::collections::HashSet<&str> =
                        mr_scores.iter().map(|r| r.symbol.as_str()).collect();
                    for sym in pinned.iter().filter(|s| !scored.contains(s.as_str())) {
                        let t = by_sym.get(sym.as_str());
                        screener_now.push(ScreenerMatch {
                            symbol: sym.clone(),
                            strategy_id: panic_mean_reversion::ID.to_string(),
                            strategy_name: panic_name.clone(),
                            priority: panic_priority,
                            price: t.and_then(|t| t.last_price),
                            gap_pct: t.and_then(|t| t.change_day_pct),
                            rvol: t.and_then(|t| {
                                avg_volumes
                                    .get(sym)
                                    .filter(|&&v| v > 0)
                                    .map(|&v| t.volume_day as f64 / v as f64)
                            }),
                            volume: t.map(|t| t.volume_day),
                            float_shares: floats.get(sym).copied(),
                            industry: company_meta.get(sym).and_then(|m| m.0.clone()),
                            country: company_meta.get(sym).and_then(|m| m.1.clone()),
                            // No score today — the UI shows the pin, not a ranking.
                            score: None,
                            score_label: Some("PIN".to_string()),
                            // A pinned ticker CAN still have been analysed: the run
                            // scores whatever the user had visible, pins included.
                            llm_panic_score: llm_scores.get(sym).and_then(|s| s.panic_score),
                            llm_one_liner: llm_scores.get(sym).and_then(|s| s.one_liner.clone()),
                            llm_reversibility: llm_scores
                                .get(sym)
                                .and_then(|s| s.reversibility.clone()),
                            llm_ranking_score: llm_scores.get(sym).and_then(|s| s.ranking_score),
                            llm_confidence_score: llm_scores
                                .get(sym)
                                .and_then(|s| s.confidence_score),
                            llm_eligibility: llm_scores
                                .get(sym)
                                .and_then(|s| s.eligibility.clone()),
                            llm_timing_state: llm_scores
                                .get(sym)
                                .and_then(|s| s.timing_state.clone()),
                            llm_trade_direction: llm_scores
                                .get(sym)
                                .and_then(|s| s.trade_direction.clone()),
                            llm_veto_reason: llm_scores
                                .get(sym)
                                .and_then(|s| s.veto_reason.clone()),
                            llm_panic_score_prev1: llm_history.get(sym).and_then(|h| h[0]),
                            llm_panic_score_prev2: llm_history.get(sym).and_then(|h| h[1]),
                            updated_at: now,
                        });
                    }
                    for row in &mr_scores {
                        let t = by_sym.get(row.symbol.as_str());
                        let expected_side = if row.direction > 0 { "short" } else { "long" };
                        // A same-day PM refresh can replace a downward shock with an
                        // upward one (or vice versa). Never attach a contextual score
                        // produced for the opposite trade direction.
                        let analysis = llm_scores.get(&row.symbol).filter(|s| {
                            s.trade_direction
                                .as_deref()
                                .is_none_or(|v| v == expected_side)
                        });
                        screener_now.push(ScreenerMatch {
                            symbol: row.symbol.clone(),
                            strategy_id: panic_mean_reversion::ID.to_string(),
                            strategy_name: panic_name.clone(),
                            priority: panic_priority,
                            price: t.and_then(|t| t.last_price),
                            gap_pct: t.and_then(|t| t.change_day_pct),
                            // Live cumulative volume since 04:00 ET vs the symbol's average
                            // daily volume — same formula as the generic per-tick screener
                            // path, and the same live counter the "volume" field below uses,
                            // so the card's Vol and RVol agree with every other volume
                            // readout in the UI (chart info bar, on-chart overlay).
                            rvol: t.and_then(|t| {
                                avg_volumes
                                    .get(&row.symbol)
                                    .filter(|&&v| v > 0)
                                    .map(|&v| t.volume_day as f64 / v as f64)
                            }),
                            volume: t.map(|t| t.volume_day),
                            float_shares: floats.get(&row.symbol).copied(),
                            industry: company_meta.get(&row.symbol).and_then(|m| m.0.clone()),
                            country: company_meta.get(&row.symbol).and_then(|m| m.1.clone()),
                            score: Some(row.score),
                            llm_panic_score: analysis.and_then(|s| s.panic_score),
                            llm_one_liner: analysis.and_then(|s| s.one_liner.clone()),
                            llm_reversibility: analysis.and_then(|s| s.reversibility.clone()),
                            llm_ranking_score: analysis.and_then(|s| s.ranking_score),
                            llm_confidence_score: analysis.and_then(|s| s.confidence_score),
                            llm_eligibility: analysis.and_then(|s| s.eligibility.clone()),
                            llm_timing_state: analysis.and_then(|s| s.timing_state.clone()),
                            llm_trade_direction: analysis.and_then(|s| s.trade_direction.clone()),
                            llm_veto_reason: analysis.and_then(|s| s.veto_reason.clone()),
                            llm_panic_score_prev1: llm_history.get(&row.symbol).and_then(|h| h[0]),
                            llm_panic_score_prev2: llm_history.get(&row.symbol).and_then(|h| h[1]),
                            score_label: {
                                // Both factors + the winning window + the move's
                                // direction, e.g. "σ4.2 ×2.1 · 2j ▼": how exceptional
                                // the move is for THIS name, by how much its ATR
                                // expanded, and over how many days. The arrow is the
                                // direction of the MOVE (a fade would be the opposite
                                // side).
                                let arrow = match row.direction {
                                    d if d > 0 => " ▲",
                                    d if d < 0 => " ▼",
                                    _ => "",
                                };
                                let horizon = if row.setup_kind == "premarket" {
                                    "PM".to_string()
                                } else {
                                    format!("{}j", row.window_days)
                                };
                                let flow = row
                                    .volume_expansion
                                    .map(|v| format!(" · V{v:.1}"))
                                    .unwrap_or_default();
                                Some(format!(
                                    "σ{:.1} ×{:.1} · {horizon}{arrow} · S{:.0}{flow}",
                                    row.sigma, row.atr_exp, row.stability_score,
                                ))
                            },
                            updated_at: now,
                        });
                    }
                }

                // Replace the live screener list.
                //
                // PANIC SCORE DESCENDING is the ranking, full stop. A ticker with a
                // score outranks any ticker without one, whatever its quant extension:
                // once the analysis has run, the answer to "is this move reversible?"
                // is what the list is for, and an unscored row has no answer to give.
                // Rows with no score — never analysed, or analysed and returned N/A —
                // fall to the bottom and keep their quant order among themselves.
                //
                // The composite blend this replaces (55% extension + 45% contextual
                // rank, with a neutral 50 imputed for unanalysed rows) let a strongly
                // extended unanalysed ticker outrank an analysed one, which made a
                // partial analysis look like a complete ranking.
                screener_now.sort_by(by_panic_score);
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
                    &db,
                    &mut alarms,
                    &mut alarm_prev,
                    &price_map,
                    &active_alerts,
                    &alert_history,
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
    let session = alert.session;
    let symbol = alert.symbol.clone();
    let is_new = {
        let mut active = active_alerts.write().unwrap();
        // Micro Pullback deliberately republishes the same `alert_id` when its tempo
        // depth or A+ score lands. That is an update, not another alert: it must not
        // flash/ring again or start the enrichment pipeline again.
        let is_new = !active.iter().any(|a| a.alert_id == alert.alert_id);
        // Keep only one entry per (symbol, strategy) in the active list
        active.retain(|a| !(a.symbol == alert.symbol && a.strategy_id == alert.strategy_id));
        active.insert(0, alert.clone());
        if active.len() > 100 {
            active.truncate(100);
        }
        is_new
    };
    {
        let mut history = alert_history.write().unwrap();
        // Re-render the existing history item in place instead of consuming several
        // of the 500 history slots for triage/deep/depth versions of one event.
        history.retain(|a| a.alert_id != alert.alert_id);
        history.insert(0, alert);
        if history.len() > 500 {
            history.truncate(500);
        }
    }
    if is_new {
        // Just-in-time "capacité à diluer" collection for an alerted ticker
        // (premarket dilution readiness). Non-blocking; worker-side TTL still guards
        // other request sources.
        crate::company_intel::ondemand::request(&symbol);
        // Every strategy + the alarm watcher funnels through here, already cooldown-
        // gated. Only a genuinely new id deserves the attention cue.
        crate::notify::on_alert(session);
    }
}

/// The screener ordering: **panic score descending, unscored last.**
///
/// A ticker WITH a score outranks any ticker without one, whatever its quant
/// extension — once the analysis has run, "is this move reversible?" is the question
/// the list exists to answer, and an unscored row has no answer to give. Rows with no
/// score (never analysed, or analysed and returned N/A) fall to the bottom and keep
/// their quant order among themselves.
fn by_panic_score(a: &ScreenerMatch, b: &ScreenerMatch) -> std::cmp::Ordering {
    let desc = |x: Option<f64>, y: Option<f64>| {
        y.unwrap_or(f64::MIN)
            .partial_cmp(&x.unwrap_or(f64::MIN))
            .unwrap_or(std::cmp::Ordering::Equal)
    };
    let panic = |m: &ScreenerMatch| m.llm_panic_score.filter(|v| v.is_finite());
    match (panic(a), panic(b)) {
        (Some(x), Some(y)) => y.partial_cmp(&x).unwrap_or(std::cmp::Ordering::Equal),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
    // Equal panic scores: the confidence-aware rank separates them.
    .then_with(|| desc(a.llm_ranking_score, b.llm_ranking_score))
    .then_with(|| desc(a.score, b.score))
    .then_with(|| b.volume.cmp(&a.volume))
    .then_with(|| desc(a.gap_pct, b.gap_pct))
}

/// Load the per-symbol float (shares) map from the universe table. Symbols with
/// no known float are omitted (the strategy decides how to treat `None`).
fn load_floats(db: &Arc<Mutex<rusqlite::Connection>>) -> HashMap<String, u64> {
    let conn = db.lock().unwrap();
    universe_repository::get_all(&conn)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|a| {
            a.float_shares
                .filter(|f| *f > 0)
                .map(|f| (a.symbol, f as u64))
        })
        .collect()
}

/// Load the per-symbol average daily volume from the universe table (used to
/// compute relative volume). Symbols with no known average are omitted.
fn load_avg_volumes(db: &Arc<Mutex<rusqlite::Connection>>) -> HashMap<String, u64> {
    let conn = db.lock().unwrap();
    universe_repository::get_all(&conn)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|a| {
            a.avg_volume
                .filter(|v| *v > 0)
                .map(|v| (a.symbol, v as u64))
        })
        .collect()
}

/// Load the per-symbol market cap (dollars) from the universe table. Computed
/// once at startup (`shares_outstanding × prev_close`, see `startup::run`) and
/// barely changes intraday, so the same slow `FLOAT_REFRESH` cadence as
/// `load_floats` is enough. Symbols with no known cap are omitted — callers
/// treat absence as "unknown", never as "zero".
fn load_market_caps(db: &Arc<Mutex<rusqlite::Connection>>) -> HashMap<String, i64> {
    let conn = db.lock().unwrap();
    universe_repository::get_all(&conn)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|a| a.market_cap.filter(|c| *c > 0).map(|c| (a.symbol, c)))
        .collect()
}

/// Load per-symbol institutional ownership (0..100) from the company_intel
/// cache (SEC 13D/13G, FMP/Massive fallback). Slow-changing fundamental data —
/// refreshed on the same cadence as `load_floats`, no day-rollover needed.
/// Symbols never collected are omitted, so callers read absence as "unknown"
/// rather than "0% owned".
fn load_institutional_ownership(db: &Arc<Mutex<rusqlite::Connection>>) -> HashMap<String, f64> {
    let conn = db.lock().unwrap();
    crate::local_db::company_intel_repository::all_institutional_ownership(&conn)
        .unwrap_or_default()
}

/// Load per-symbol (industry, country) from the sec-api company_meta cache, for
/// the screener card's industry/country display. Refreshed on the same slow
/// cadence as `load_floats` — this metadata barely changes intraday.
fn load_company_meta(
    db: &Arc<Mutex<rusqlite::Connection>>,
) -> HashMap<String, (Option<String>, Option<String>)> {
    let conn = db.lock().unwrap();
    crate::local_db::company_meta_repository::get_all(&conn)
        .unwrap_or_default()
        .into_iter()
        .map(|m| (m.symbol, (m.industry, m.country)))
        .collect()
}

/// Load the Panic Mean Reversion watchlist rows for `day`, strongest panic first.
/// Empty until that day's 09:25 ET build has run — and, crucially, empty rather than
/// wrong when the app clock sits on a day the list wasn't built for.
fn load_top_scores(db: &Arc<Mutex<rusqlite::Connection>>, day: &str) -> Vec<ScoreRow> {
    let conn = db.lock().unwrap();
    scoring_repository::get_top(&conn, day, MR_TOP_N).unwrap_or_default()
}

/// Load the Panic Score (LLM) readings for `day`, keyed by symbol.
///
/// Mode-scoped through `replay::clock::is_active()`, so a run made during a Market
/// Replay is invisible in live mode and vice versa — the LLM score IS the ranking, so
/// leaking a backtest's scores would silently reorder a real trading morning. Empty
/// until the user presses the analysis button; nothing produces these automatically.
fn load_llm_scores(
    db: &Arc<Mutex<rusqlite::Connection>>,
    day: &str,
) -> HashMap<String, crate::local_db::panic_llm_repository::PanicLlmScore> {
    let simulated = crate::replay::clock::is_active();
    let conn = db.lock().unwrap();
    crate::local_db::panic_llm_repository::get_for_day(&conn, day, simulated)
        .unwrap_or_default()
        .into_iter()
        .map(|s| (s.symbol.clone(), s))
        .collect()
}

/// Panic score from each symbol's previous 1-2 SCORED days (see
/// `panic_llm_repository::distinct_days_before` for why this is not simply
/// `day - 1` / `day - 2`). `[prev1, prev2]`, most recent first; a symbol not
/// analysed on a given day is just absent from that slot (None).
fn load_llm_history(
    db: &Arc<Mutex<rusqlite::Connection>>,
    day: &str,
) -> HashMap<String, [Option<f64>; 2]> {
    let simulated = crate::replay::clock::is_active();
    let conn = db.lock().unwrap();
    let days =
        crate::local_db::panic_llm_repository::distinct_days_before(&conn, day, simulated, 2)
            .unwrap_or_default();

    let mut history: HashMap<String, [Option<f64>; 2]> = HashMap::new();
    for (i, d) in days.iter().enumerate() {
        let rows = crate::local_db::panic_llm_repository::get_for_day(&conn, d, simulated)
            .unwrap_or_default();
        for row in rows {
            history.entry(row.symbol).or_insert([None, None])[i] = row.panic_score;
        }
    }
    history
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
    db: &Arc<Mutex<rusqlite::Connection>>,
    alarms: &mut Vec<PriceAlarm>,
    alarm_prev: &mut HashMap<String, f64>,
    price_map: &HashMap<String, f64>,
    active_alerts: &Arc<RwLock<Vec<AlertSignal>>>,
    alert_history: &Arc<RwLock<Vec<AlertSignal>>>,
) {
    let mut triggered_ids: Vec<String> = Vec::new();

    for alarm in alarms.iter() {
        let Some(&price) = price_map.get(&alarm.symbol) else {
            continue;
        };
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
                alert_id: format!("alarm-{}-{}", alarm.id, now.timestamp_millis()),
                timestamp: now,
                symbol: alarm.symbol.clone(),
                strategy_id: alarm.strategy_id.clone().unwrap_or_default(),
                strategy_name: name,
                priority,
                // Surfaces in the Open-tab sidebar.
                session: Session::Open,
                price: Some(price),
                bid: None,
                ask: None,
                spread: None,
                volume: None,
                rvol: None,
                change_day_pct: None,
                float_shares: None,
                news_today: false,
                halted: Some(false),
                latency_ui_ms: None,
                reason: format!("Alarme déclenchée à ${:.2} — touché ${:.2}", level, price),
                display_timeframe: None,
                side: None,
                tempo_depth: None,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn alert(id: &str, reason: &str) -> AlertSignal {
        AlertSignal {
            alert_id: id.into(),
            timestamp: chrono::Utc::now(),
            symbol: "TEST".into(),
            strategy_id: "micro_pullback".into(),
            strategy_name: "Micro Pullback".into(),
            priority: 5,
            session: Session::Premarket,
            price: Some(5.0),
            bid: None,
            ask: None,
            spread: None,
            volume: Some(10_000),
            rvol: None,
            change_day_pct: None,
            float_shares: None,
            news_today: false,
            halted: Some(false),
            latency_ui_ms: None,
            reason: reason.into(),
            display_timeframe: Some("10s".into()),
            side: None,
            tempo_depth: Some(vec![10]),
        }
    }

    #[test]
    fn republishing_one_alert_updates_active_and_history_without_duplicates() {
        let old = alert("same-id", "triage");
        let active = Arc::new(RwLock::new(vec![old.clone()]));
        let history = Arc::new(RwLock::new(vec![old]));

        push_alert(&active, &history, alert("same-id", "deep"));

        let active = active.read().unwrap();
        let history = history.read().unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(history.len(), 1);
        assert_eq!(active[0].reason, "deep");
        assert_eq!(history[0].reason, "deep");
    }

    fn row(symbol: &str, panic: Option<f64>, quant: f64) -> ScreenerMatch {
        ScreenerMatch {
            symbol: symbol.into(),
            strategy_id: "panic_mean_reversion".into(),
            strategy_name: "Panic".into(),
            priority: 1,
            price: Some(10.0),
            gap_pct: Some(5.0),
            rvol: None,
            volume: Some(1_000_000),
            float_shares: None,
            industry: None,
            country: None,
            score: Some(quant),
            score_label: None,
            llm_panic_score: panic,
            llm_one_liner: None,
            llm_reversibility: None,
            llm_ranking_score: panic,
            llm_confidence_score: None,
            llm_eligibility: Some(if panic.is_some() {
                "eligible".into()
            } else {
                "n_a".into()
            }),
            llm_timing_state: None,
            llm_trade_direction: None,
            llm_veto_reason: None,
            llm_panic_score_prev1: None,
            llm_panic_score_prev2: None,
            updated_at: chrono::Utc::now(),
        }
    }

    /// The ranking the analysis produces: panic score descending over the WHOLE list.
    #[test]
    fn the_list_ranks_by_panic_score_descending() {
        let mut rows = vec![
            row("LOW", Some(22.0), 300.0),
            row("HIGH", Some(91.0), 12.0),
            row("MID", Some(57.0), 40.0),
        ];
        rows.sort_by(by_panic_score);
        let order: Vec<&str> = rows.iter().map(|r| r.symbol.as_str()).collect();
        assert_eq!(
            order,
            vec!["HIGH", "MID", "LOW"],
            "a huge quant extension must not outrank a higher panic score",
        );
    }

    /// N/A (analysed, not scorable) and never-analysed rows both sink to the bottom,
    /// where they keep their quantitative order relative to each other.
    #[test]
    fn rows_without_a_score_sink_below_every_scored_row() {
        let mut rows = vec![
            row("NA_STRONG", None, 900.0),
            row("SCORED_LOW", Some(3.0), 5.0),
            row("NA_WEAK", None, 10.0),
        ];
        rows.sort_by(by_panic_score);
        let order: Vec<&str> = rows.iter().map(|r| r.symbol.as_str()).collect();
        assert_eq!(order, vec!["SCORED_LOW", "NA_STRONG", "NA_WEAK"]);
    }

    /// A NaN score is not a score. Guards the `filter(is_finite)`: without it the
    /// comparison poisons the sort and the row's position becomes arbitrary.
    #[test]
    fn a_nan_score_is_treated_as_no_score() {
        let mut rows = vec![
            row("NAN", Some(f64::NAN), 500.0),
            row("REAL", Some(10.0), 1.0),
        ];
        rows.sort_by(by_panic_score);
        assert_eq!(rows[0].symbol, "REAL");
    }
}
