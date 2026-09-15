//! Dislocation: robust ticker-relative activity, cross-sectional attention, catalyst.
//!
//! There are deliberately no absolute volume, VWAP, split or session-liquidity
//! gates here. The ticker's own robust tempo RVOL answers "did it wake up?";
//! cross-sectional attention confirms that the print is meaningful relative to the
//! current market. This is a percentile gate, not an absolute-volume floor, so a
//! genuine 04:00 leader can still qualify while a few-share baseline artefact cannot.
//! RVOL combines a point-in-time historical time-of-day profile with the rolling
//! intraday baseline, so 04:00 and post-dormancy wakes are both observable. The
//! existing quality module owns the catalyst decision.

use std::collections::{HashMap, HashSet};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex, RwLock,
};

use chrono::{DateTime, Utc};

use crate::config::secrets::Secrets;
use crate::flat_files::minute::RvolBaseline;
use crate::market_state::{MarketState, SecondTapeBar};
use crate::dislocation::{quality, DislocationOptions};
use crate::scanner::push_alert;
use crate::strategies::dislocation::ID as STRATEGY_ID;
use crate::types::{AlertSignal, MicroAttentionEntry, Side};

const TOP_N: usize = 10;
const REFRESH_META_SECS: u64 = 300;
const LOOP_MS: u64 = 1_000;
const MAX_SERIES_SECS: i64 = 15 * 60;
const EVENT_RELEASE_SECS: i64 = 60;
/// How far back the pre-selection gate looks for a headline. Wider than the freshness
/// ceiling it then applies, so an OLD headline is reported as old (and logged as
/// such) instead of being indistinguishable from no headline at all.
const NEWS_LOOKBACK_SECS: i64 = 48 * 3600;
const LEADER_Z_MIN: f64 = 2.5;
const HEAD_Z_MIN: f64 = 1.25;
const HEAD_EXCESS_SHARE_MIN: f64 = 0.45;
const GAP_SCALE_MIN: f64 = 1.5;
const GAP_POINTS_MIN: f64 = 8.0;

#[derive(Debug, Clone, Copy, Default)]
struct Meta {
    float_shares: Option<u64>,
    atr: Option<f64>,
    prev_close: Option<f64>,
    /// Who the issuer is: hot industry, which crowd, risk domicile. Loaded with the
    /// universe rather than at qualification time, because the pre-selection filters
    /// and the deterministic score both need it BEFORE any provider call — that is
    /// the whole point of paying for it once per refresh.
    profile: crate::strategies::issuer_profile::IssuerProfile,
}

struct TapeSeries {
    bars: Vec<SecondTapeBar>,
    prefix_shares: Vec<u64>,
}

impl TapeSeries {
    fn new(bars: Vec<SecondTapeBar>) -> Self {
        let mut prefix_shares = Vec::with_capacity(bars.len() + 1);
        prefix_shares.push(0_u64);
        for b in &bars {
            let next = prefix_shares
                .last()
                .copied()
                .unwrap_or(0)
                .saturating_add(b.shares);
            prefix_shares.push(next);
        }
        Self {
            bars,
            prefix_shares,
        }
    }

    fn upper_bound(&self, sec: i64) -> usize {
        self.bars.partition_point(|b| b.sec <= sec)
    }

    fn after(&self, sec: i64) -> &[SecondTapeBar] {
        &self.bars[self.upper_bound(sec)..]
    }

    /// Shares and presence in the half-open-by-name interval `(start, end]`.
    fn window_volume(&self, start: i64, end: i64) -> (u64, bool) {
        let a = self.upper_bound(start);
        let b = self.upper_bound(end);
        (
            self.prefix_shares[b].saturating_sub(self.prefix_shares[a]),
            b > a,
        )
    }
}

#[derive(Debug, Clone)]
struct RawObservation {
    symbol: String,
    tempo_secs: i64,
    price: f64,
    volume: u64,
    dollars: f64,
    active_seconds: u32,
    share_signal: f64,
    dollar_signal: f64,
    turnover_signal: Option<f64>,
    range_signal: f64,
    /// Raw ratio, not a logarithm: 5.0 really means five times the robust baseline.
    relative_signal: Option<f64>,
    baseline_windows: u8,
    continuity: f64,
    distribution_quality: f64,
    evidence_ok: bool,
    price_return_pct: f64,
    float_shares: Option<u64>,
}

#[derive(Debug, Clone)]
struct RankedObservation {
    raw: RawObservation,
    attention: f64,
    direction: f64,
    flow_rank: f64,
    range_rank: f64,
    relative_rank: Option<f64>,
    dominance_z: f64,
    head_size: usize,
    dominant: bool,
}

#[derive(Debug, Default)]
struct SymbolMachine {
    active_event: bool,
    last_qualified_at: Option<DateTime<Utc>>,
    qualification_attempted: bool,
    last_scored_depth: usize,
    last_scored_attention: f64,
    last_scored_rvol: f64,
    event_seq: u32,
}

/// The model budget, in MARKET time so it scales with an accelerated replay exactly
/// as it does live.
///
/// A rolling window rather than a per-tick cap: a burst of six real departures at
/// 04:05 should spend the window's whole allowance on the best of them, not three
/// per tick forever. When the window rolls, the allowance resets.
#[derive(Debug, Clone, Copy)]
struct LlmBudget {
    window_start: Option<DateTime<Utc>>,
    spent: usize,
}

impl LlmBudget {
    const WINDOW_SECS: i64 = 5 * 60;

    fn new() -> Self {
        Self {
            window_start: None,
            spent: 0,
        }
    }

    /// How many calls are still affordable, rolling the window over when it expired.
    fn room(&mut self, now: DateTime<Utc>, capacity: usize) -> usize {
        let rolled = match self.window_start {
            Some(start) => (now - start).num_seconds().abs() >= Self::WINDOW_SECS,
            None => true,
        };
        if rolled {
            self.window_start = Some(now);
            self.spent = 0;
        }
        capacity.saturating_sub(self.spent)
    }

    fn spend(&mut self, n: usize) {
        self.spent += n;
    }

    /// A replay restart or a backward seek is a new session: the previous window's
    /// spend must not throttle it.
    fn reset(&mut self) {
        self.window_start = None;
        self.spent = 0;
    }
}

#[derive(Debug, Clone)]
struct Signal {
    best: RankedObservation,
    depth: Vec<i64>,
}

#[allow(clippy::too_many_arguments)]
pub fn start(
    running: Arc<AtomicBool>,
    market: Arc<RwLock<MarketState>>,
    db: Arc<Mutex<rusqlite::Connection>>,
    secrets: Arc<RwLock<Secrets>>,
    active_alerts: Arc<RwLock<Vec<AlertSignal>>>,
    alert_history: Arc<RwLock<Vec<AlertSignal>>>,
    strategy_enabled: Arc<RwLock<HashMap<String, bool>>>,
    options: Arc<RwLock<DislocationOptions>>,
    app_config: Arc<RwLock<crate::config::AppConfig>>,
    attention: Arc<RwLock<Vec<MicroAttentionEntry>>>,
) {
    tauri::async_runtime::spawn(async move {
        let initial_day = crate::time::et_date(crate::time::now());
        // The float ceiling is a Settings value, so the universe has to be rebuilt
        // when it changes — tracked here rather than re-derived, so a five-minute
        // refresh isn't the earliest a new ceiling can take effect.
        let mut loaded_float_max = options.read().unwrap().float_max;
        let mut meta = load_meta(&db, &initial_day, loaded_float_max);
        let mut historical_rvol = load_historical_rvol(&db, &initial_day);
        let mut meta_loaded = std::time::Instant::now();
        let mut machines: HashMap<String, SymbolMachine> = HashMap::new();
        let mut last_bucket: Option<i64> = None;
        let mut last_snapshot_bucket: Option<i64> = None;
        let mut day: Option<String> = None;
        let mut budget = LlmBudget::new();
        let mut replay_gen = crate::replay::clock::generation();

        while running.load(Ordering::Relaxed) {
            let enabled = strategy_enabled
                .read()
                .unwrap()
                .get(STRATEGY_ID)
                .copied()
                .unwrap_or(true);
            if !enabled {
                attention.write().unwrap().clear();
                crate::replay::clock::scaled_sleep(LOOP_MS).await;
                continue;
            }

            // Market Replay reset watch: replay start / backward seek / new day →
            // detect the replayed session from scratch.
            let generation = crate::replay::clock::generation();
            if generation != replay_gen {
                replay_gen = generation;
                machines.clear();
                attention.write().unwrap().clear();
                last_bucket = None;
                last_snapshot_bucket = None;
                day = None;
                // Replaying the same session twice must be able to push twice.
                // Without this, `ntfy_alerts` is a Settings switch that silently
                // stops working from the second replay of a day on — the ids are
                // rebuilt identically and every send is swallowed as a duplicate.
                crate::ntfy::reset_sent();
                // Qualification tasks from the abandoned session are still running
                // and will clear their own entries, but a backward seek must not
                // leave their spinners on the pane of the session being restarted.
                super::live_status::reset();
                quality::reset_news_thirds();
                budget.reset();
            }

            let opts = *options.read().unwrap();
            if meta_loaded.elapsed().as_secs() >= REFRESH_META_SECS
                || loaded_float_max != opts.float_max
            {
                let before = day.as_deref().unwrap_or(&initial_day);
                loaded_float_max = opts.float_max;
                meta = load_meta(&db, before, loaded_float_max);
                historical_rvol = load_historical_rvol(&db, before);
                meta_loaded = std::time::Instant::now();
            }

            if opts.enabled_tempos().is_empty() {
                attention.write().unwrap().clear();
                crate::replay::clock::scaled_sleep(LOOP_MS).await;
                continue;
            }

            let Some(market_now) = market.read().unwrap().strategy_market_now() else {
                crate::replay::clock::scaled_sleep(LOOP_MS).await;
                continue;
            };
            if !super::in_active_session(market_now) {
                attention.write().unwrap().clear();
                crate::replay::clock::scaled_sleep(LOOP_MS).await;
                continue;
            }

            let current_day = crate::time::et_date(market_now);
            let new_day = day.as_deref() != Some(current_day.as_str());
            if new_day {
                // Point-in-time daily context. The startup cache can contain the
                // replayed day (and even later days); using it would leak the future
                // into ATR/previous-close normalisation.
                meta = load_meta(&db, &current_day, loaded_float_max);
                historical_rvol = load_historical_rvol(&db, &current_day);
                meta_loaded = std::time::Instant::now();
                machines.clear();
                last_bucket = None;
                last_snapshot_bucket = None;
                day = Some(current_day);
            }

            // The smallest strategy tempo is 10 seconds. Re-ranking the whole
            // universe between two closed buckets only repeats identical work.
            let bucket = market_now.timestamp() / 10;
            if last_bucket == Some(bucket) {
                crate::replay::clock::scaled_sleep(LOOP_MS).await;
                continue;
            }
            last_bucket = Some(bucket);

            let raws = snapshot(&market, &meta, &historical_rvol, &opts, market_now);
            let (ranked, debug_rows) = rank_all(raws, market_now, &active_alerts, &alert_history);
            let snapshot_bucket = market_now.timestamp() / 30;
            if last_snapshot_bucket != Some(snapshot_bucket) {
                last_snapshot_bucket = Some(snapshot_bucket);
                if let Ok(mut conn) = db.lock() {
                    let _ = crate::local_db::micro_attention_repository::insert_snapshot(
                        &mut conn,
                        &debug_rows,
                    );
                    // Bounded forensic history; run idempotently once per engine day.
                    if new_day {
                        let cutoff = (market_now - chrono::Duration::days(7)).to_rfc3339();
                        let _ = crate::local_db::micro_attention_repository::prune_snapshots(
                            &conn, &cutoff,
                        );
                    }
                }
            }
            *attention.write().unwrap() = debug_rows;
            let signals = collapse_signals(&ranked);
            let live_symbols: HashSet<String> = signals.keys().cloned().collect();

            for (symbol, machine) in machines.iter_mut() {
                if live_symbols.contains(symbol) {
                    machine.last_qualified_at = Some(market_now);
                } else if machine.active_event
                    && machine
                        .last_qualified_at
                        .is_some_and(|t| (market_now - t).num_seconds() >= EVENT_RELEASE_SECS)
                {
                    machine.active_event = false;
                }
            }

            // ── Two axes, one ranking, one budget ─────────────────────────────
            // `signals` is a HashMap, so iterating it directly processed a tick's
            // candidates in ARBITRARY order — with a bounded model budget that would
            // make "which ticker gets analysed" a hash-ordering accident. Two passes
            // instead: score every fresh candidate on the two axes that exist before
            // any model speaks, sort, then act best-first.
            //
            // Anything that does not fit the budget is left completely untouched —
            // no `active_event`, no event row — so it is reconsidered on the next
            // tick. It is deferred behind something better, never lost.
            let mut ranked: Vec<(String, Signal, super::score::Scores, Option<i64>)> = Vec::new();
            for (symbol, signal) in signals {
                let claimed = {
                    let machine = machines.entry(symbol.clone()).or_default();
                    machine.last_qualified_at = Some(market_now);
                    machine.active_event
                };
                let mut scores = scores_of(&signal.best);
                // Publish this tick's tape thirds. A qualification task spawned
                // moments ago is about to render a verdict, and it must score the
                // move as it IS rather than as it was when the task started.
                quality::record_tape(&symbol, scores.rvol, scores.attention);
                if claimed {
                    // FREE RE-SCORE. Two thirds out of three move every second, so a
                    // candidate judged just under the bar can clear it a minute later
                    // with nothing new to buy. Only the upgrade direction is taken
                    // here: a row that flickered between priorities on every tick
                    // would be unreadable, and a demotion is the verdict's own job.
                    //
                    // THIS ALSO COVERS THE NEVER-PUBLISHED CASE, which is the one
                    // that matters: in blocking mode a verdict below the threshold
                    // publishes nothing at all, so waiting for an existing row to
                    // upgrade meant waiting for something that would never exist.
                    scores.news = quality::news_third(&symbol);
                    let previous = latest_alert(&active_alerts, &alert_history, &symbol);
                    let worth_showing = scores.news.is_some()
                        && scores.total() >= opts.alert_score_threshold
                        && previous.as_ref().is_none_or(|p| p.priority < 5);
                    if worth_showing {
                        let news_count = market.read().unwrap().recent_news_count(
                            &symbol,
                            market_now,
                            24 * 3600,
                        );
                        let machine = machines.entry(symbol.clone()).or_default();
                        if previous.is_none() {
                            machine.event_seq += 1;
                        }
                        let mut upgraded = make_alert(
                            &market,
                            &signal,
                            market_now,
                            machine.event_seq,
                            previous.is_some(),
                            news_count > 0,
                        );
                        // Same id when a row already exists: this is one event getting
                        // better, and `push_alert` renders it in place rather than
                        // adding a second row.
                        if let Some(p) = &previous {
                            upgraded.alert_id = p.alert_id.clone();
                        }
                        upgraded.priority = 5;
                        upgraded.reason = format!(
                            "[{:.0} · RVOL {:.0} · attention {:.0} · news {:.0} · seuil franchi sur la bande] {}",
                            scores.total(),
                            scores.rvol,
                            scores.attention,
                            scores.news.unwrap_or(0.0),
                            upgraded.reason
                        );
                        if opts.ntfy_alerts {
                            crate::ntfy::notify_alert_once(&secrets, &upgraded);
                        }
                        persist_event(
                            &db,
                            &upgraded.alert_id,
                            &signal,
                            market_now,
                            previous.is_some(),
                            "crossed_on_tape",
                        );
                        push_alert(&active_alerts, &alert_history, upgraded);
                    }
                    continue;
                }
                let news_age = newest_news_age_minutes(&market, &symbol, market_now);
                ranked.push((symbol, signal, scores, news_age));
            }
            ranked.sort_by(|a, b| b.2.tape().total_cmp(&a.2.tape()));

            // No key, no model — and therefore no reason to hold a candidate back
            // waiting for a third that can never arrive. Without this check a user
            // with no Deepseek key would see every catalyst-carrying candidate
            // silently refused inside the qualification task.
            let has_model = secrets
                .read()
                .unwrap()
                .deepseek_api_key
                .as_deref()
                .is_some_and(|k| !k.trim().is_empty());
            let room = budget.room(market_now, opts.llm_budget_per_5min as usize);
            let mut spent = 0usize;

            for (symbol, signal, mut scores, news_age) in ranked {
                // The news third is the only one that costs money. It is worth
                // spending only on a headline still inside the freshness window —
                // outside it, the third is a measured zero, not an unknown.
                let gradable_news = super::score::worth_a_model_call(news_age) && has_model;
                let call_model = gradable_news && spent < room;
                if gradable_news && !call_model {
                    continue;
                }
                if !gradable_news {
                    // Not an unknown: either there is nothing in the window to grade,
                    // or there is no model to grade it with. Either way the third is
                    // a measured zero and the candidate is judged on its tape alone.
                    scores.news = Some(0.0);
                }
                // Without a model call the score is final right here, so the
                // threshold is the whole decision. With one, the verdict is the
                // qualification task's to make.
                if !call_model && scores.total() < opts.alert_score_threshold {
                    continue;
                }

                let machine = machines.entry(symbol.clone()).or_default();
                machine.active_event = true;
                machine.event_seq += 1;

                let news_count =
                    market
                        .read()
                        .unwrap()
                        .recent_news_count(&symbol, market_now, 24 * 3600);
                let previous = latest_alert(&active_alerts, &alert_history, &symbol);
                let already_alerted = previous.is_some();
                // News identity is normally the cache key. A visibly stronger,
                // later market event may retry a candidate that never reached the
                // screen: otherwise a weak pre-headline twitch can permanently
                // consume the day's claim and suppress the actual ignition. Once an
                // alert exists, unchanged news still reuses it without another call.
                let materially_stronger = previous.is_none()
                    && machine.qualification_attempted
                    && is_materially_stronger_event(machine, &signal);
                let score_claimed = !call_model
                    || quality::claim_score(&symbol, market_now, news_count)
                    || materially_stronger;
                let no_new_information = machine.qualification_attempted && !score_claimed;

                let mut alert = make_alert(
                    &market,
                    &signal,
                    market_now,
                    machine.event_seq,
                    already_alerted,
                    news_count > 0,
                );

                if call_model && no_new_information {
                    if let Some(old) = previous {
                        // Reuse the last visible verdict. The new event's market
                        // metrics stay in the reason; no provider or LLM call occurs.
                        alert.priority = old.priority;
                        alert.reason = format!(
                            "[Déjà alerté · score réutilisé · catalyseur inchangé] {} · nouvel événement: {}",
                            old.reason, alert.reason
                        );
                        if opts.ntfy_alerts && alert.priority >= 5 {
                            crate::ntfy::notify_alert_once(&secrets, &alert);
                        }
                        persist_event(
                            &db,
                            &alert.alert_id,
                            &signal,
                            market_now,
                            true,
                            "score_reused",
                        );
                        push_alert(&active_alerts, &alert_history, alert);
                    } else if let Ok(conn) = db.lock() {
                        let _ = crate::local_db::insert_log(
                            &conn,
                            "info",
                            &format!(
                                "dislocation {symbol}: nouvel événement, LLM non rappelé (catalyseur inchangé)"
                            ),
                        );
                        drop(conn);
                        persist_event(
                            &db,
                            &alert.alert_id,
                            &signal,
                            market_now,
                            false,
                            "news_unchanged_suppressed",
                        );
                    }
                    continue;
                }

                machine.qualification_attempted = call_model;
                if call_model {
                    machine.last_scored_depth = signal.depth.len();
                    machine.last_scored_attention = signal.best.attention;
                    machine.last_scored_rvol = signal.best.raw.relative_signal.unwrap_or_default();
                    spent += 1;
                    budget.spend(1);
                }

                if !call_model {
                    // Two thirds out of three, and they already clear the bar.
                    quality::decorate_scores(&mut alert, &scores, news_age);
                    if opts.ntfy_alerts {
                        crate::ntfy::notify_alert_once(&secrets, &alert);
                    }
                    persist_event(
                        &db,
                        &alert.alert_id,
                        &signal,
                        market_now,
                        already_alerted,
                        "published_without_news",
                    );
                    push_alert(&active_alerts, &alert_history, alert);
                    continue;
                }

                let best = &signal.best;
                let deps = quality::Deps {
                    db: db.clone(),
                    config: app_config.clone(),
                    secrets: secrets.clone(),
                    market: market.clone(),
                    active_alerts: active_alerts.clone(),
                    alert_history: alert_history.clone(),
                };
                let candidate = quality::Candidate {
                    alert,
                    tempo_secs: best.raw.tempo_secs,
                    tempo_depth: signal.depth.clone(),
                    price_return_pct: best.raw.price_return_pct,
                    volume_ratio: best.raw.relative_signal.unwrap_or(1.0),
                    trade_rate_ratio: None,
                    range_ratio: best.raw.range_signal,
                    tape_source: market.read().unwrap().tape_kind(),
                    tape_rate_per_sec: None,
                    tape_dollar_per_sec: best.raw.dollars / best.raw.tempo_secs as f64,
                    tape_active_seconds: Some(best.raw.active_seconds),
                    tape_window_secs: best.raw.tempo_secs,
                    detected_at: market_now,
                    scores,
                    news_age_minutes: news_age,
                };
                persist_event(
                    &db,
                    &candidate.alert.alert_id,
                    &signal,
                    market_now,
                    already_alerted,
                    "sent_to_news_llm",
                );
                tauri::async_runtime::spawn(quality::run_qualification(deps, candidate, opts));
            }

            crate::replay::clock::scaled_sleep(LOOP_MS).await;
        }
    });
}

/// Age in minutes of the newest headline the in-memory store holds for `symbol`, or
/// `None` when it holds none. The store is fed by the live news websocket and, in a
/// replay, by the day's loaded headlines stamped with their real publication time —
/// so the same number means the same thing in both modes.
fn newest_news_age_minutes(
    market: &Arc<RwLock<MarketState>>,
    symbol: &str,
    now: DateTime<Utc>,
) -> Option<i64> {
    market
        .read()
        .unwrap()
        .recent_news(symbol, now, NEWS_LOOKBACK_SECS)
        .into_iter()
        .map(|n| (now - n.at).num_minutes())
        .min()
}

fn load_meta(
    db: &Arc<Mutex<rusqlite::Connection>>,
    before_date: &str,
    float_max: u64,
) -> HashMap<String, Meta> {
    let conn = db.lock().unwrap();
    // Issuer identity for the whole universe in one read. `company_meta` is written
    // by the startup pipeline and changes at most once a day, so joining it here
    // costs one query per five-minute refresh instead of one per candidate.
    let profiles = crate::local_db::company_meta_repository::get_all(&conn)
        .unwrap_or_default()
        .into_iter()
        .map(|m| {
            let profile = crate::strategies::issuer_profile::IssuerProfile::from_meta(
                m.country.as_deref(),
                m.industry.as_deref(),
                m.sector.as_deref(),
            );
            (m.symbol, profile)
        })
        .collect::<HashMap<_, _>>();
    let fundamentals = crate::local_db::cache_repository::all_fundamentals(&conn)
        .unwrap_or_default()
        .into_iter()
        .map(|f| (f.symbol.clone(), f))
        .collect::<HashMap<_, _>>();

    let mut out = crate::local_db::universe_repository::get_all(&conn)
        .unwrap_or_default()
        .into_iter()
        .filter(|a| a.tradable)
        .filter(|a| {
            !crate::scoring::is_etf_like(
                a.security_type.as_deref(),
                a.name.as_deref(),
                a.exchange.as_deref(),
            )
        })
        .filter_map(|a| {
            let f = fundamentals.get(&a.symbol);
            let float = a
                .float_shares
                .or_else(|| f.and_then(|x| x.float_shares))
                .filter(|v| *v > 0)
                .map(|v| v as u64);
            if float.is_some_and(|v| v > float_max) {
                return None;
            }
            let profile = profiles.get(&a.symbol).copied().unwrap_or_default();
            Some((
                a.symbol,
                Meta {
                    float_shares: float,
                    atr: f.and_then(|x| x.atr).filter(|v| *v > 0.0),
                    prev_close: f.and_then(|x| x.prev_close).filter(|v| *v > 0.0),
                    profile,
                },
            ))
        })
        .collect::<HashMap<_, _>>();

    // Replay-safe ATR14 + previous close for the full universe in one query. This
    // deliberately excludes `before_date`; fundamentals_cache is a startup snapshot
    // and can already contain the day currently being replayed.
    let rows = (|| -> rusqlite::Result<Vec<(String, f64, f64, f64)>> {
        let mut stmt = conn.prepare(
            "SELECT symbol, high, low, close FROM (
                 SELECT symbol, high, low, close, date,
                        ROW_NUMBER() OVER (PARTITION BY symbol ORDER BY date DESC) AS rn
                 FROM daily_cache
                 WHERE date < ?1 AND high IS NOT NULL AND low IS NOT NULL AND close IS NOT NULL
             ) WHERE rn <= 15 ORDER BY symbol ASC, date ASC",
        )?;
        let mapped = stmt.query_map([before_date], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?;
        mapped.collect()
    })()
    .unwrap_or_default();
    let mut i = 0usize;
    while i < rows.len() {
        let symbol = rows[i].0.clone();
        let mut bars = Vec::new();
        while i < rows.len() && rows[i].0 == symbol {
            bars.push((rows[i].1, rows[i].2, rows[i].3));
            i += 1;
        }
        if let Some(m) = out.get_mut(&symbol) {
            m.prev_close = bars.last().map(|b| b.2);
            m.atr = atr14(&bars);
        }
    }
    out
}

fn load_historical_rvol(
    db: &Arc<Mutex<rusqlite::Connection>>,
    target_day: &str,
) -> HashMap<(String, u32), RvolBaseline> {
    let conn = db.lock().unwrap();
    crate::flat_files::minute::read_rvol_baseline(&conn, target_day).unwrap_or_default()
}

fn atr14(bars: &[(f64, f64, f64)]) -> Option<f64> {
    const PERIOD: usize = 14;
    if bars.len() < PERIOD + 1 {
        return None;
    }
    let start = bars.len() - PERIOD;
    let sum = (start..bars.len())
        .map(|i| {
            let previous_close = bars[i - 1].2;
            let (high, low) = (bars[i].0, bars[i].1);
            (high - low)
                .max((high - previous_close).abs())
                .max((low - previous_close).abs())
        })
        .sum::<f64>();
    let value = sum / PERIOD as f64;
    (value.is_finite() && value > 0.0).then_some(value)
}

fn snapshot(
    market: &Arc<RwLock<MarketState>>,
    meta: &HashMap<String, Meta>,
    historical_rvol: &HashMap<(String, u32), RvolBaseline>,
    opts: &DislocationOptions,
    now: DateTime<Utc>,
) -> Vec<RawObservation> {
    let ms = market.read().unwrap();
    let mut out = Vec::new();
    let tempos = opts.enabled_tempos();
    for (symbol, m) in meta {
        // Hot-industry-only is a UNIVERSE rule, applied here rather than at
        // qualification: excluding a ticker from the ranking keeps it out of the
        // percentiles every other candidate is measured against, which is the
        // whole reason the switch is worth having. An unknown industry is not hot,
        // so it is excluded too — the sharpest edge of this option.
        if opts.hot_industry_only && !m.profile.sector_hot {
            continue;
        }
        // Same reasoning, opposite edge: an UNKNOWN domicile is not flagged, so it
        // survives here. We exclude on what we know, never on what we are missing.
        if opts.exclude_risk_domicile && m.profile.country_risk {
            continue;
        }
        let Some(ticker) = ms.tickers.get(symbol) else {
            continue;
        };
        let Some(price) = ticker.last_price else {
            continue;
        };
        if !(opts.price_min..=opts.price_max).contains(&price) {
            continue;
        }
        let series = TapeSeries::new(ms.tape_series(symbol, MAX_SERIES_SECS, now));
        let effective_meta = Meta {
            // MarketState is authoritative in replay: reset_market seeds the real
            // previous session close loaded with the replay day.
            prev_close: ticker.previous_close.or(m.prev_close),
            ..*m
        };
        for &tempo in &tempos {
            if let Some(obs) = observe(
                symbol,
                price,
                &effective_meta,
                historical_rvol,
                tempo,
                now,
                &series,
            ) {
                out.push(obs);
            }
        }
    }
    out
}

fn observe(
    symbol: &str,
    price: f64,
    meta: &Meta,
    historical_rvol: &HashMap<(String, u32), RvolBaseline>,
    tempo: i64,
    now: DateTime<Utc>,
    series: &TapeSeries,
) -> Option<RawObservation> {
    let cutoff = now.timestamp() - tempo;
    let cur = series.after(cutoff);
    if cur.len() < 2 {
        return None;
    }
    let first = cur.first()?;
    let last = cur.last()?;
    let volume = cur.iter().map(|b| b.shares).sum::<u64>();
    if volume == 0 || first.open <= 0.0 {
        return None;
    }
    let dollars = cur.iter().map(|b| b.dollars).sum::<f64>();
    let high = cur.iter().map(|b| b.high).fold(f64::NEG_INFINITY, f64::max);
    let low = cur.iter().map(|b| b.low).fold(f64::INFINITY, f64::min);
    let net = last.close - first.open;
    let range = (high - low).max(0.0);
    let max_second_share = cur.iter().map(|b| b.shares).max().unwrap_or(0) as f64 / volume as f64;
    let distribution_quality = if cur.len() <= 1 {
        0.0
    } else {
        ((1.0 - max_second_share) / (1.0 - 1.0 / cur.len() as f64)).clamp(0.0, 1.0)
    };
    let target_active = (tempo as f64).sqrt().ceil().max(2.0);
    let continuity = (cur.len() as f64 / target_active).clamp(0.0, 1.0);
    let atr = meta.atr.filter(|v| *v > 0.0);
    let normalizer =
        atr.unwrap_or_else(|| meta.prev_close.filter(|v| *v > 0.0).unwrap_or(first.open));
    let range_signal = range / normalizer.max(1e-9);
    let evidence_ok = has_market_evidence(
        range_signal,
        atr.is_some(),
        tempo,
        continuity,
        distribution_quality,
    );

    let rolling = relative_activity(series, now.timestamp(), tempo, volume);
    let historical = historical_activity(historical_rvol, symbol, now, tempo, volume);
    let relative = match (rolling, historical) {
        (Some(a), Some(b)) => Some(if a.ratio >= b.ratio { a } else { b }),
        (a, b) => a.or(b),
    };
    Some(RawObservation {
        symbol: symbol.to_string(),
        tempo_secs: tempo,
        price,
        volume,
        dollars,
        active_seconds: cur.len() as u32,
        share_signal: (volume as f64).ln_1p(),
        dollar_signal: dollars.max(0.0).ln_1p(),
        turnover_signal: meta
            .float_shares
            .filter(|f| *f > 0)
            .map(|f| volume as f64 / f as f64),
        range_signal,
        relative_signal: relative.map(|r| r.ratio),
        baseline_windows: relative.map_or(0, |r| r.windows),
        continuity,
        distribution_quality,
        evidence_ok,
        price_return_pct: net / first.open * 100.0,
        float_shares: meta.float_shares,
    })
}

fn has_market_evidence(
    range_signal: f64,
    atr_normalized: bool,
    tempo: i64,
    continuity: f64,
    distribution_quality: f64,
) -> bool {
    let tempo_scale = (tempo as f64 / 10.0).sqrt();
    let range_floor = if atr_normalized { 0.02 } else { 0.002 } * tempo_scale;
    range_signal >= range_floor && continuity >= 0.25 && distribution_quality >= 0.05
}

#[derive(Debug, Clone, Copy)]
struct TempoRvol {
    ratio: f64,
    windows: u8,
}

fn relative_activity(series: &TapeSeries, now: i64, tempo: i64, current: u64) -> Option<TempoRvol> {
    // Never invent zero windows beyond the retained tape. Short tempos get up to
    // eight robust buckets; 5m gets the two real prior windows available in 15m.
    let window_count = ((MAX_SERIES_SECS / tempo) - 1).clamp(1, 8);
    let baseline_start = now - (window_count + 1) * tempo;
    let baseline_end = now - tempo;
    if !series.window_volume(baseline_start, baseline_end).1 {
        return None;
    }
    let mut windows = Vec::new();
    for i in 1..=window_count {
        let end = now - i * tempo;
        let start = end - tempo;
        let volume = series.window_volume(start, end).0;
        windows.push(volume as f64);
    }
    let baseline = median(&mut windows)?;
    // The floor is what makes this number a RATIO. Without it a dormant premarket
    // name whose prior windows are empty divides by ~1 and reports its raw share
    // count as a multiplier — measured on 20 sessions, half of all premarket
    // baselines are exactly zero. See `score::RVOL_BASELINE_FLOOR_PER_MIN`.
    let expected = super::score::floor_expected(baseline, tempo);
    Some(TempoRvol {
        // Not a logarithm: the payload and the UI expect an actual multiplier, and
        // "5x" must remain 5.0 rather than ln(5).
        ratio: current as f64 / expected,
        windows: window_count as u8,
    })
}

fn historical_activity(
    profile: &HashMap<(String, u32), RvolBaseline>,
    symbol: &str,
    now: DateTime<Utc>,
    tempo: i64,
    current: u64,
) -> Option<TempoRvol> {
    let minute = crate::time::et_minutes(now);
    let bucket = (minute / 10) * 10;
    let baseline = profile.get(&(symbol.to_string(), bucket))?;
    // One or two days are useful forensic context, but not a robust distribution.
    if baseline.sample_days < 3 {
        return None;
    }
    // Median + half a robust sigma avoids a single historically active day making
    // the expected value zero, while preserving genuine zero-volume dormancy.
    let typical_10m = baseline.median_volume_10m + 0.5 * 1.4826 * baseline.mad_volume_10m;
    let expected = super::score::expected_volume(typical_10m, tempo);
    Some(TempoRvol {
        ratio: current as f64 / expected,
        windows: baseline.sample_days,
    })
}

fn rank_all(
    raws: Vec<RawObservation>,
    market_now: DateTime<Utc>,
    active_alerts: &Arc<RwLock<Vec<AlertSignal>>>,
    alert_history: &Arc<RwLock<Vec<AlertSignal>>>,
) -> (Vec<RankedObservation>, Vec<MicroAttentionEntry>) {
    let received_at = Utc::now();
    let mut alerted: HashSet<String> = active_alerts
        .read()
        .unwrap()
        .iter()
        .filter(|a| a.strategy_id == STRATEGY_ID)
        .map(|a| a.symbol.clone())
        .collect();
    alerted.extend(
        alert_history
            .read()
            .unwrap()
            .iter()
            .filter(|a| a.strategy_id == STRATEGY_ID)
            .map(|a| a.symbol.clone()),
    );
    let mut all = Vec::new();
    let mut debug = Vec::new();
    for tempo in super::options::TEMPO_SECS {
        let group: Vec<RawObservation> = raws
            .iter()
            .filter(|r| r.tempo_secs == tempo)
            .cloned()
            .collect();
        if group.is_empty() {
            continue;
        }
        let mut shares = group.iter().map(|r| r.share_signal).collect::<Vec<_>>();
        let mut dollars = group.iter().map(|r| r.dollar_signal).collect::<Vec<_>>();
        let mut turnovers = group
            .iter()
            .filter_map(|r| r.turnover_signal)
            .collect::<Vec<_>>();
        let mut ranges = group.iter().map(|r| r.range_signal).collect::<Vec<_>>();
        let mut relatives = group
            .iter()
            .filter_map(|r| r.relative_signal)
            .collect::<Vec<_>>();
        for v in [
            &mut shares,
            &mut dollars,
            &mut turnovers,
            &mut ranges,
            &mut relatives,
        ] {
            v.sort_by(f64::total_cmp);
        }

        let mut ranked = group
            .into_iter()
            .map(|raw| {
                let mut flow_num =
                    percentile(&shares, raw.share_signal) + percentile(&dollars, raw.dollar_signal);
                let mut flow_den = 2.0;
                if let Some(v) = raw.turnover_signal {
                    flow_num += percentile(&turnovers, v);
                    flow_den += 1.0;
                }
                let flow_rank = flow_num / flow_den;
                let range_rank = percentile(&ranges, raw.range_signal);
                // Kept for the forensic row only. It used to feed `attention` with
                // a 0.15 weight, which made the attention third and the RVOL third
                // of the score partly the same number.
                let relative_rank = raw.relative_signal.map(|v| percentile(&relatives, v));
                // "Beaucoup de volume, beaucoup de tape, beaucoup de volume par
                // seconde." Flow carries the volume and the dollars, continuity and
                // distribution carry the tape, range says the volume moved the price.
                let attention = 100.0
                    * (0.45 * flow_rank
                        + 0.20 * range_rank
                        + 0.20 * raw.continuity
                        + 0.15 * raw.distribution_quality);
                RankedObservation {
                    raw,
                    attention,
                    // Kept in the wire/DB schema for backward-compatible forensic
                    // rows. V2.1 no longer computes or gates on directional
                    // efficiency; zero explicitly means "not evaluated".
                    direction: 0.0,
                    flow_rank,
                    range_rank,
                    relative_rank,
                    dominance_z: 0.0,
                    head_size: 0,
                    dominant: false,
                }
            })
            .collect::<Vec<_>>();
        apply_dominance(&mut ranked);
        ranked.sort_by(|a, b| b.attention.total_cmp(&a.attention));

        for (idx, r) in ranked.iter().take(TOP_N).enumerate() {
            let thirds = scores_of(r);
            debug.push(MicroAttentionEntry {
                symbol: r.raw.symbol.clone(),
                tempo_secs: tempo as u32,
                rank: (idx + 1) as u8,
                attention_score: r.attention,
                direction_score: r.direction,
                flow_rank: r.flow_rank,
                range_rank: r.range_rank,
                relative_rank: r.relative_rank,
                tempo_rvol: r.raw.relative_signal,
                baseline_windows: r.raw.baseline_windows,
                continuity: r.raw.continuity,
                distribution_quality: r.raw.distribution_quality,
                evidence_ok: r.raw.evidence_ok,
                dominance_z: r.dominance_z,
                head_size: r.head_size as u8,
                dominant: r.dominant,
                already_alerted: alerted.contains(&r.raw.symbol),
                rvol_third: thirds.rvol,
                attention_third: thirds.attention,
                market_time: market_now,
                received_at,
            });
        }
        // The Top 10 is a capacity boundary, not a concentration test. A candidate
        // still needs its own >=5x tempo RVOL and real price evidence below.
        all.extend(ranked.into_iter().take(TOP_N));
    }
    (all, debug)
}

fn apply_dominance(rows: &mut [RankedObservation]) {
    if rows.is_empty() {
        return;
    }
    rows.sort_by(|a, b| b.attention.total_cmp(&a.attention));
    let mut scores = rows.iter().map(|r| r.attention).collect::<Vec<_>>();
    let median_score = median(&mut scores).unwrap_or(0.0);
    let mut deviations = rows
        .iter()
        .map(|r| (r.attention - median_score).abs())
        .collect::<Vec<_>>();
    let scale = (1.4826 * median(&mut deviations).unwrap_or(0.0)).max(3.0);
    for r in rows.iter_mut() {
        r.dominance_z = (r.attention - median_score) / scale;
    }

    if rows.len() < 4 {
        // At 04:00 there may literally be no peloton yet. A lone ticker with
        // strong absolute attention is concentration by definition; the separate
        // evidence + direction gates still protect this branch from one-print
        // noise. With two or three active names, require a real lead over #2.
        let detached_at_cold_open = rows.len() == 1
            || rows[0].attention - rows[1].attention >= GAP_POINTS_MIN.max(GAP_SCALE_MIN * scale);
        if rows[0].attention >= 75.0 && detached_at_cold_open {
            rows[0].head_size = 1;
            rows[0].dominant = true;
        }
        return;
    }

    let max_head = 3usize.min(rows.len() - 1);
    let (head_idx, gap) = (0..max_head)
        .map(|i| (i, rows[i].attention - rows[i + 1].attention))
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .unwrap();
    let head_size = head_idx + 1;
    let total_excess = rows
        .iter()
        .map(|r| (r.attention - median_score).max(0.0))
        .sum::<f64>();
    let head_excess = rows[..head_size]
        .iter()
        .map(|r| (r.attention - median_score).max(0.0))
        .sum::<f64>();
    let share = if total_excess > 0.0 {
        head_excess / total_excess
    } else {
        0.0
    };
    let valid = rows[0].dominance_z >= LEADER_Z_MIN
        && rows[head_size - 1].dominance_z >= HEAD_Z_MIN
        && gap >= GAP_POINTS_MIN.max(GAP_SCALE_MIN * scale)
        && share >= HEAD_EXCESS_SHARE_MIN;
    if valid {
        for r in &mut rows[..head_size] {
            r.head_size = head_size;
            r.dominant = true;
        }
    }
}

/// One row per symbol: the tempo on which it looks strongest, plus the list of
/// tempos it showed up on at all.
///
/// NO GATE HERE ANY MORE. This used to AND together `RVOL >= 5`, `flow >= p80`,
/// a cold-start branch at `attention >= 85 / p90 / p90`, and a retention rule of
/// "two tempos, or RVOL >= 10 with attention >= 85". Every one of those is a hard
/// magnitude threshold on a quantity whose distribution moves between a cold 04:00
/// tape and a hot 09:35 one — the exact thing that is not supposed to decide
/// anything here. The capacity boundary is the per-tempo Top 10 in `rank_all`; the
/// score decides the rest.
fn collapse_signals(rows: &[RankedObservation]) -> HashMap<String, Signal> {
    let mut out: HashMap<String, Signal> = HashMap::new();
    for row in rows {
        let entry = out.entry(row.raw.symbol.clone()).or_insert_with(|| Signal {
            best: row.clone(),
            depth: Vec::new(),
        });
        entry.depth.push(row.raw.tempo_secs);
        // The tempo that best represents the symbol is the one with the highest
        // tape score — the same number that then ranks it against the others.
        if tape_score(row) > tape_score(&entry.best) {
            entry.best = row.clone();
        }
    }
    for signal in out.values_mut() {
        signal.depth.sort_unstable();
        signal.depth.dedup();
    }
    out
}

/// The two thirds of the score that exist before any model is called.
fn tape_score(row: &RankedObservation) -> f64 {
    scores_of(row).tape()
}

/// The RVOL and attention thirds of one observation.
fn scores_of(row: &RankedObservation) -> super::score::Scores {
    super::score::Scores {
        rvol: super::score::rvol_score(row.raw.relative_signal),
        attention: super::score::attention_score(row.dominance_z, row.attention),
        news: None,
    }
}

fn is_materially_stronger_event(machine: &SymbolMachine, signal: &Signal) -> bool {
    signal.depth.len() > machine.last_scored_depth
        || signal.best.attention >= machine.last_scored_attention + 10.0
        || (machine.last_scored_rvol > 0.0
            && signal.best.raw.relative_signal.unwrap_or_default()
                >= machine.last_scored_rvol * 3.0)
}

fn make_alert(
    market: &Arc<RwLock<MarketState>>,
    signal: &Signal,
    now: DateTime<Utc>,
    event_seq: u32,
    already_alerted: bool,
    news_today: bool,
) -> AlertSignal {
    let best = &signal.best;
    let ms = market.read().unwrap();
    let ticker = ms.tickers.get(&best.raw.symbol);
    let lag_ms = (Utc::now() - now)
        .num_milliseconds()
        .clamp(0, u32::MAX as i64) as u32;
    let repeated = if already_alerted {
        " · DÉJÀ ALERTÉ"
    } else {
        ""
    };
    let unknown = if best.raw.float_shares.is_none() {
        " · FLOAT_INCONNU"
    } else {
        ""
    };
    let rvol = best
        .raw
        .relative_signal
        .map(|v| format!("RVOL tempo {v:.1}×"))
        .unwrap_or_else(|| "RVOL tempo: démarrage sans baseline".into());
    let reason = format!(
        "Réveil auto-relatif · {rvol} · attention {att:.0} · concentration {z:.1}σ (indicative) · {tempo} · confirmation {depth}{repeated}{unknown} — ${price:.2}",
        rvol = rvol,
        att = best.attention,
        z = best.dominance_z,
        tempo = super::fmt_tempo(best.raw.tempo_secs),
        depth = signal.depth.iter().map(|t| super::fmt_tempo(*t)).collect::<Vec<_>>().join("/"),
        price = best.raw.price,
    );
    AlertSignal {
        alert_id: format!(
            "dl-{}-{}-{event_seq}",
            now.timestamp_millis(),
            best.raw.symbol
        ),
        timestamp: now,
        symbol: best.raw.symbol.clone(),
        strategy_id: STRATEGY_ID.into(),
        strategy_name: "Dislocation".into(),
        priority: 5,
        session: crate::time::session_at(now),
        price: Some(best.raw.price),
        bid: ticker.and_then(|t| t.bid),
        ask: ticker.and_then(|t| t.ask),
        spread: ticker.and_then(|t| t.spread),
        volume: Some(ticker.map_or(best.raw.volume, |t| t.volume_day)),
        rvol: None,
        change_day_pct: ticker.and_then(|t| t.change_day_pct),
        float_shares: best.raw.float_shares,
        news_today,
        halted: Some(false),
        latency_ui_ms: Some(lag_ms),
        reason,
        // The execution pane supports 10s, not synthetic 20s/40s frames. Tempo
        // depth remains visible on the badge/reason.
        display_timeframe: Some("10s".to_string()),
        side: Some(Side::Long),
        tempo_depth: Some(signal.depth.iter().map(|t| *t as u32).collect()),
    }
}

fn latest_alert(
    active_alerts: &Arc<RwLock<Vec<AlertSignal>>>,
    alert_history: &Arc<RwLock<Vec<AlertSignal>>>,
    symbol: &str,
) -> Option<AlertSignal> {
    let active = active_alerts
        .read()
        .unwrap()
        .iter()
        .filter(|a| a.strategy_id == STRATEGY_ID && a.symbol == symbol)
        .max_by_key(|a| a.timestamp)
        .cloned();
    let historical = alert_history
        .read()
        .unwrap()
        .iter()
        .filter(|a| a.strategy_id == STRATEGY_ID && a.symbol == symbol)
        .max_by_key(|a| a.timestamp)
        .cloned();
    match (active, historical) {
        (Some(a), Some(h)) => Some(if a.timestamp >= h.timestamp { a } else { h }),
        (a, h) => a.or(h),
    }
}

fn persist_event(
    db: &Arc<Mutex<rusqlite::Connection>>,
    event_id: &str,
    signal: &Signal,
    market_now: DateTime<Utc>,
    already_alerted: bool,
    outcome: &str,
) {
    let r = &signal.best;
    let thirds = scores_of(r);
    let row = MicroAttentionEntry {
        symbol: r.raw.symbol.clone(),
        tempo_secs: r.raw.tempo_secs as u32,
        rank: 0,
        attention_score: r.attention,
        direction_score: r.direction,
        flow_rank: r.flow_rank,
        range_rank: r.range_rank,
        relative_rank: r.relative_rank,
        tempo_rvol: r.raw.relative_signal,
        baseline_windows: r.raw.baseline_windows,
        continuity: r.raw.continuity,
        distribution_quality: r.raw.distribution_quality,
        evidence_ok: r.raw.evidence_ok,
        dominance_z: r.dominance_z,
        head_size: r.head_size as u8,
        dominant: r.dominant,
        rvol_third: thirds.rvol,
        attention_third: thirds.attention,
        already_alerted,
        market_time: market_now,
        received_at: Utc::now(),
    };
    if let Ok(conn) = db.lock() {
        let _ = crate::local_db::micro_attention_repository::insert(
            &conn,
            event_id,
            &row,
            &signal.depth,
            outcome,
        );
    }
}

fn percentile(sorted: &[f64], value: f64) -> f64 {
    if sorted.len() <= 1 {
        return 1.0;
    }
    let below = sorted.partition_point(|v| *v < value);
    (below as f64 / (sorted.len() - 1) as f64).clamp(0.0, 1.0)
}

fn median(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() || values.iter().any(|v| !v.is_finite()) {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let n = values.len();
    Some(if n % 2 == 0 {
        (values[n / 2 - 1] + values[n / 2]) / 2.0
    } else {
        values[n / 2]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── The model budget ──

    fn t(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + secs, 0).unwrap()
    }

    #[test]
    fn the_budget_is_a_rolling_window_not_a_per_tick_cap() {
        let mut b = LlmBudget::new();
        // A burst of departures may spend the whole window's allowance at once.
        assert_eq!(b.room(t(0), 3), 3);
        b.spend(3);
        assert_eq!(b.room(t(10), 3), 0, "same window, nothing left");
        assert_eq!(b.room(t(299), 3), 0);
        assert_eq!(b.room(t(300), 3), 3, "the window rolled");
    }

    /// The budget counts MARKET time, so an accelerated replay is throttled exactly
    /// as the live session would be — otherwise a 10x replay would spend ten times
    /// the model calls for the same session.
    #[test]
    fn a_backward_seek_does_not_inherit_the_previous_windows_spend() {
        let mut b = LlmBudget::new();
        b.room(t(600), 3);
        b.spend(3);
        // Seeking back inside the same window would otherwise look like "already
        // spent" for the restarted session.
        b.reset();
        assert_eq!(b.room(t(400), 3), 3);
    }

    #[test]
    fn a_capacity_change_takes_effect_without_waiting_for_the_window() {
        let mut b = LlmBudget::new();
        b.room(t(0), 3);
        b.spend(3);
        assert_eq!(b.room(t(10), 3), 0);
        assert_eq!(b.room(t(10), 8), 5, "raising the setting frees room now");
    }

    fn raw(symbol: &str, attention_seed: f64) -> RankedObservation {
        RankedObservation {
            raw: RawObservation {
                symbol: symbol.into(),
                tempo_secs: 10,
                price: 5.0,
                volume: 100,
                dollars: 500.0,
                active_seconds: 5,
                share_signal: 1.0,
                dollar_signal: 1.0,
                turnover_signal: None,
                range_signal: 1.0,
                relative_signal: None,
                baseline_windows: 0,
                continuity: 1.0,
                distribution_quality: 1.0,
                evidence_ok: true,
                price_return_pct: 1.0,
                float_shares: Some(1_000_000),
            },
            attention: attention_seed,
            direction: 100.0,
            flow_rank: 1.0,
            range_rank: 1.0,
            relative_rank: None,
            dominance_z: 0.0,
            head_size: 0,
            dominant: false,
        }
    }

    #[test]
    fn detached_head_is_selected_but_flat_top_is_not() {
        let mut detached = vec![
            raw("A", 95.0),
            raw("B", 91.0),
            raw("C", 86.0),
            raw("D", 55.0),
            raw("E", 53.0),
            raw("F", 50.0),
            raw("G", 48.0),
            raw("H", 45.0),
        ];
        apply_dominance(&mut detached);
        assert_eq!(detached.iter().filter(|r| r.dominant).count(), 3);

        let mut noise = vec![
            raw("A", 72.0),
            raw("B", 70.0),
            raw("C", 69.0),
            raw("D", 68.0),
            raw("E", 67.0),
            raw("F", 66.0),
            raw("G", 65.0),
            raw("H", 64.0),
        ];
        apply_dominance(&mut noise);
        assert!(noise.iter().all(|r| !r.dominant));
    }

    #[test]
    fn a_lone_real_mover_is_not_blocked_at_the_four_am_cold_open() {
        let mut only_active_name = vec![raw("A", 82.0)];
        apply_dominance(&mut only_active_name);
        assert!(only_active_name[0].dominant);

        let mut two_undistinguished_names = vec![raw("A", 82.0), raw("B", 80.0)];
        apply_dominance(&mut two_undistinguished_names);
        assert!(two_undistinguished_names.iter().all(|r| !r.dominant));
    }

    #[test]
    fn relative_activity_is_optional_at_the_open_then_values_real_dormancy() {
        let series = vec![
            SecondTapeBar {
                sec: 71,
                shares: 1,
                ..Default::default()
            },
            SecondTapeBar {
                sec: 91,
                shares: 100,
                ..Default::default()
            },
            SecondTapeBar {
                sec: 92,
                shares: 100,
                ..Default::default()
            },
        ];
        let tape = TapeSeries::new(series);
        // 200 shares against an empty baseline used to report a ratio above 100.
        // Floored, it is what it actually is: nothing much.
        let small = relative_activity(&tape, 100, 10, 200).unwrap();
        assert!(small.ratio < 3.0, "got {}", small.ratio);
        // A genuine first impulse still reads as one.
        let real = relative_activity(&tape, 100, 10, 50_000).unwrap();
        assert!(real.ratio > 500.0, "got {}", real.ratio);
        let cold = TapeSeries::new(tape.bars[1..].to_vec());
        assert!(relative_activity(&cold, 100, 10, 200).is_none());
    }

    #[test]
    fn historical_rvol_scales_one_robust_tod_bucket_to_each_tempo() {
        let day = chrono::NaiveDate::from_ymd_opt(2026, 8, 21).unwrap();
        let now = crate::time::et_clock_utc(crate::replay::data::noon_utc(day), 7, 5);
        let bucket = (crate::time::et_minutes(now) / 10) * 10;
        let profile = HashMap::from([(
            ("JUNS".to_string(), bucket),
            RvolBaseline {
                // Expected 20-second volume = 300 000 * 20/600 = 10 000 shares,
                // comfortably above the floor, so the time-of-day scaling is what is
                // actually under test here.
                median_volume_10m: 300_000.0,
                mad_volume_10m: 0.0,
                sample_days: 8,
            },
        )]);
        let rvol = historical_activity(&profile, "JUNS", now, 20, 99_000).unwrap();
        assert!((rvol.ratio - 9.9).abs() < 1e-9, "got {}", rvol.ratio);
        assert_eq!(rvol.windows, 8);

        // A baseline below the floor is compared to the floor instead of to itself.
        let dormant = HashMap::from([(
            ("SHELL".to_string(), bucket),
            RvolBaseline {
                median_volume_10m: 30.0,
                mad_volume_10m: 0.0,
                sample_days: 8,
            },
        )]);
        let floored = historical_activity(&dormant, "SHELL", now, 20, 99_000).unwrap();
        assert!(
            (floored.ratio - 99_000.0 / (500.0 * 20.0 / 60.0)).abs() < 1e-6,
            "got {}",
            floored.ratio
        );
    }

    #[test]
    fn a_rank_leader_without_real_range_or_participation_is_still_noise() {
        assert!(!has_market_evidence(0.001, true, 10, 1.0, 1.0));
        assert!(!has_market_evidence(0.5, true, 10, 0.1, 1.0));
        assert!(!has_market_evidence(0.5, true, 10, 1.0, 0.0));
        assert!(has_market_evidence(0.05, true, 10, 0.7, 0.5));
    }

    #[test]
    fn five_minute_relative_activity_uses_only_retained_real_windows() {
        // Volumes well above the floor, so what is under test is the window
        // selection rather than the floor.
        let series = vec![
            SecondTapeBar {
                sec: 10,
                shares: 100_000,
                ..Default::default()
            },
            SecondTapeBar {
                sec: 310,
                shares: 100_000,
                ..Default::default()
            },
        ];
        let rel = relative_activity(&TapeSeries::new(series), 900, 300, 200_000).unwrap();
        assert!(
            rel.ratio > 1.9 && rel.ratio < 2.1,
            "expected 200 000/100 000, got {}",
            rel.ratio
        );
    }

    #[test]
    fn juns_replay_is_not_vetoed_by_peloton_or_chop() {
        // Forensic values observed on 2026-08-21 around 07:10 ET: JUNS led the
        // attention table, evidence was valid, but dominance was false and the old
        // direction score fell to 5 after a choppy interval. Its own activity was
        // explosively above its dormant baseline on several tempos.
        let mut twenty = raw("JUNS", 85.4);
        twenty.raw.tempo_secs = 20;
        twenty.raw.relative_signal = Some(8.0);
        twenty.raw.baseline_windows = 8;
        twenty.direction = 5.0;
        twenty.dominant = false;

        let mut sixty = raw("JUNS", 88.3);
        sixty.raw.tempo_secs = 60;
        sixty.raw.relative_signal = Some(12.0);
        sixty.raw.baseline_windows = 8;
        sixty.direction = 41.5;
        sixty.dominant = false;

        let signals = collapse_signals(&[twenty, sixty]);
        let signal = signals.get("JUNS").expect("JUNS must reach A+");
        assert_eq!(signal.depth, vec![20, 60]);
    }

    #[test]
    fn noise_is_ranked_below_the_ignition_rather_than_gated_out() {
        // Replay forensics, 2026-08-21. BTCS produced spectacular ratios against a
        // near-zero baseline while staying in the pack; JUNS's 07:09 impulse was
        // detached by four robust sigmas. The old code ANDed a flow-percentile gate
        // to tell them apart and threw BTCS away entirely. Now BOTH are ranked, and
        // the score is what separates them — which is the property that survives a
        // market whose distributions move between 04:00 and 09:35.
        let mut btcs = raw("BTCS", 76.6);
        btcs.raw.tempo_secs = 20;
        btcs.raw.relative_signal = Some(10.3);
        btcs.dominance_z = 0.6;

        let mut juns = raw("JUNS", 94.0);
        juns.raw.tempo_secs = 20;
        juns.raw.relative_signal = Some(4_378.0);
        juns.dominance_z = 4.58;

        let out = collapse_signals(&[btcs.clone(), juns.clone()]);
        assert!(out.contains_key("JUNS"));
        assert!(out.contains_key("BTCS"), "noise is ranked, not erased");
        assert!(
            tape_score(&juns) > 2.0 * tape_score(&btcs),
            "JUNS {:.0} vs BTCS {:.0}",
            tape_score(&juns),
            tape_score(&btcs)
        );
    }

    #[test]
    fn a_stronger_later_event_can_retry_an_invisible_rejection() {
        let machine = SymbolMachine {
            qualification_attempted: true,
            last_scored_depth: 2,
            last_scored_attention: 75.0,
            last_scored_rvol: 2_000.0,
            ..Default::default()
        };
        let mut best = raw("JUNS", 86.0);
        best.raw.relative_signal = Some(1_500.0);
        let signal = Signal {
            best,
            depth: vec![20, 40, 60],
        };
        assert!(is_materially_stronger_event(&machine, &signal));
    }

    /// A single marginal print still has to be cheap. It is no longer thrown away by
    /// a gate — it simply scores near nothing, so the budget never reaches it.
    #[test]
    fn one_marginal_tempo_scores_near_zero() {
        let mut noise = raw("NOISE", 70.0);
        noise.raw.relative_signal = Some(5.1);
        noise.dominance_z = 0.3;
        let s = scores_of(&noise);
        assert!(s.rvol < 10.0, "RVOL third {:.1}", s.rvol);
        // The attention third is majority-rank since the recalibration, so a row
        // sitting mid-table still collects some of it. What has to stay negligible
        // is the TOTAL: no RVOL and no catalyst is no candidate.
        let total = crate::dislocation::score::Scores {
            news: Some(0.0),
            ..s
        }
        .total();
        assert!(total < 20.0, "total {total:.1}");
    }

    /// The floor is what stops a dormant name's raw share count from masquerading as
    /// a multiplier. Measured on 20 sessions: half of all premarket baselines are
    /// exactly zero, and JUNS reported "RVOL 2 055" at 11:02 on 2 323 shares.
    #[test]
    fn a_collapsed_baseline_cannot_manufacture_an_rvol() {
        let series = TapeSeries::new(vec![
            SecondTapeBar {
                sec: 100,
                shares: 1,
                ..Default::default()
            },
            SecondTapeBar {
                sec: 160,
                shares: 1,
                ..Default::default()
            },
        ]);
        let rel = relative_activity(&series, 200, 60, 2_323).unwrap();
        assert!(
            rel.ratio < 6.0,
            "2 323 shares against an empty baseline is not a 2 000x wake, got {}",
            rel.ratio
        );
    }

    #[test]
    fn point_in_time_atr_excludes_the_replayed_session() {
        let bars = (0..15)
            .map(|i| {
                let close = 5.0 + i as f64 * 0.01;
                (close + 0.2, close - 0.2, close)
            })
            .collect::<Vec<_>>();
        let atr = atr14(&bars).unwrap();
        assert!(atr > 0.39 && atr < 0.41);
    }
}
