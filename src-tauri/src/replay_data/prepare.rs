// Preparing days: the only place that reaches the network for replay data.
//
// Per day, cheapest stage first, each one skipping what the store already holds:
//
//   1. market   whole-market daily bars → the day's snapshot (once per day);
//   2. minutes  1-minute bars of the candidates the profiles' universes admit —
//               fetched once per symbol, kept only when some profile can use them;
//   3. scan     each profile's pre-scan over the kept symbols (local), then its
//               confirmation on real prints IN TIME ORDER: every opportunity found
//               gets its ticks and news at once and is stored with how far the
//               confirmation got. A replay can therefore start on the first
//               opportunities while the rest of the day is still being confirmed
//               (`plan::replayable_until`), and an interrupted day resumes where it
//               stopped. Re-run only when the profile's fingerprint changed;
//   4. ticks    whatever real SIP trades/quotes are still missing around the hits
//               (a pad changed, a scan-only day) — skipped by `scan_only`;
//   5. news     headlines for the symbols that got tick windows.
//
// Days are prepared one at a time by a single worker fed from a queue: a whole
// month can be queued, and a day asked for with priority is taken next — the day in
// progress yields at its next request and resumes afterwards.
//
// Every Alpaca call goes through `alpaca::ratelimit` (so the pace follows the
// plan the key actually has), is `feed=sip` and unadjusted (`adjustment=raw`):
// minutes and ticks must quote the same prices, and a reverse split since the day
// would otherwise shift the bars away from the prints.
//
// The clock here is always the REAL one (`Utc::now`), never `crate::time::now`: a
// day keeps being prepared while a replay runs on simulated time.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use super::intervals::Span;
use super::plan::{self, DayState, DayStatus, Opportunity, StrategyDayStatus, TickJob};
use super::store::{self, DayStore, MarketRow, PartialScan, TickKind};
use super::{DataProfile, ReplayDataNeeds};
use crate::market_state::aggregators::Bar;
use crate::review::codec::{QuoteRow, TradeRow};
use crate::review::sip::Creds;

const DATA: &str = "https://data.alpaca.markets";
/// Symbols per multi-symbol bars request.
const CHUNK: usize = 200;
/// News lookback before the day.
const NEWS_LOOKBACK_DAYS: i64 = 3;
/// Alpaca Basic serves SIP history up to 15 minutes back; a day is complete one
/// minute of safety after that past its 20:00 ET close.
const AVAILABLE_AFTER_CLOSE_MIN: i64 = 16;

// ─── Shared status (polled by the UI) ──────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct PrepareStatus {
    pub running: bool,
    /// idle | running | done | cancelled | error
    pub state: String,
    pub day: Option<String>,
    /// Days finished in this run, and finished + in progress + waiting.
    pub day_index: usize,
    pub day_total: usize,
    /// market | minutes | scan | ticks | news
    pub stage: String,
    /// 0..1 within the stage.
    pub progress: f32,
    pub message: Option<String>,
    pub error: Option<String>,
    /// Days that failed (holiday, network…) with the reason, for the summary.
    pub skipped: Vec<(String, String)>,
    /// Days still waiting, in the order they will be taken.
    pub queue: Vec<String>,
    /// Seconds left for the day in progress and the queue, from how long the days
    /// of this run took. None until one has been timed.
    pub eta_secs: Option<u64>,
}

impl Default for PrepareStatus {
    fn default() -> Self {
        Self {
            running: false,
            state: "idle".into(),
            day: None,
            day_index: 0,
            day_total: 0,
            stage: String::new(),
            progress: 0.0,
            message: None,
            error: None,
            skipped: Vec::new(),
            queue: Vec::new(),
            eta_secs: None,
        }
    }
}

struct Job {
    day: String,
    ids: Vec<String>,
    scan_only: bool,
    /// Asked for now (a session about to start): taken before the bulk days.
    priority: bool,
}

#[derive(Default)]
pub struct ReplayDataShared {
    pub status: RwLock<PrepareStatus>,
    pub cancel: AtomicBool,
    queue: Mutex<VecDeque<Job>>,
    /// A priority day arrived: the day in progress yields at its next request.
    preempt: AtomicBool,
    /// Per day, bumped whenever its store gains something a running replay can read.
    revisions: Mutex<HashMap<String, u64>>,
}

impl ReplayDataShared {
    pub fn is_running(&self) -> bool {
        self.status.read().unwrap().running
    }
    /// Stop the day in progress between two requests and drop the queue;
    /// everything already written stays usable.
    pub fn request_cancel(&self) {
        let mut q = self.queue.lock().unwrap();
        q.clear();
        if self.is_running() {
            self.cancel.store(true, Ordering::Relaxed);
        }
        self.status.write().unwrap().queue.clear();
    }
    /// Whether `day` is being prepared or waiting to be.
    pub fn pending(&self, day: &str) -> bool {
        let q = self.queue.lock().unwrap();
        let s = self.status.read().unwrap();
        (s.running && s.day.as_deref() == Some(day)) || q.iter().any(|j| j.day == day)
    }
    /// See the field: a replay watches its day's to know when to read it again.
    pub fn revision(&self, day: &str) -> u64 {
        self.revisions.lock().unwrap().get(day).copied().unwrap_or(0)
    }
    fn bump(&self, day: &str) {
        *self.revisions.lock().unwrap().entry(day.to_string()).or_default() += 1;
    }
    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
    /// The day in progress must stop here: cancelled, or a priority day is waiting.
    fn interrupted(&self) -> bool {
        self.cancelled() || self.preempt.load(Ordering::Relaxed)
    }
    fn stage(&self, stage: &str, progress: f32, message: Option<String>) {
        let mut s = self.status.write().unwrap();
        s.stage = stage.into();
        s.progress = progress.clamp(0.0, 1.0);
        if message.is_some() {
            s.message = message;
        }
    }
}

// ─── Day bounds & availability ─────────────────────────────────────────────────

fn parse_day(day: &str) -> Result<NaiveDate, String> {
    NaiveDate::parse_from_str(day, "%Y-%m-%d").map_err(|_| format!("date invalide : {day}"))
}

/// [04:00, 20:00) ET of `day`, UTC epoch millis.
pub fn day_bounds(day: &str) -> Result<Span, String> {
    let noon = crate::replay::data::noon_utc(parse_day(day)?);
    Ok((
        crate::time::et_clock_utc(noon, 4, 0).timestamp_millis(),
        crate::time::et_clock_utc(noon, 20, 0).timestamp_millis(),
    ))
}

/// Why a day cannot be prepared, if it cannot: weekend, future, or not yet 15
/// minutes past the close (Alpaca Basic's SIP delay).
pub fn unavailable_reason(day: &str, now: DateTime<Utc>) -> Option<&'static str> {
    use chrono::Datelike;
    let Ok(nd) = parse_day(day) else {
        return Some("date invalide");
    };
    if matches!(nd.weekday(), chrono::Weekday::Sat | chrono::Weekday::Sun) {
        return Some("week-end");
    }
    let (_, close) = day_bounds(day).ok()?;
    if now.timestamp_millis() < close + AVAILABLE_AFTER_CLOSE_MIN * 60_000 {
        return Some("séance pas encore disponible en SIP historique (clôture + 15 min)");
    }
    None
}

// ─── Status (read-only) ────────────────────────────────────────────────────────

/// What a day holds for `profiles`, without touching the network or the store.
pub fn day_status(app_dir: &Path, day: &str, profiles: &[DataProfile]) -> DayStatus {
    let mut status = DayStatus {
        day: day.to_string(),
        state: DayState::NotPrepared,
        strategies: Vec::new(),
        opportunities: Vec::new(),
        missing_ms: 0,
        estimate_bytes: 0,
        estimate_requests: 0,
        bytes_on_disk: file_bytes(&store::day_path(app_dir, day)),
        ready_hits: 0,
        ready_until_ms: None,
        launchable: false,
        trades: 0,
    };
    if unavailable_reason(day, Utc::now()).is_some() {
        status.state = DayState::Unavailable;
        return status;
    }
    let Some(store) = DayStore::open_existing(app_dir, day) else {
        return status;
    };
    if store.meta("closed").as_deref() == Some("1") {
        status.state = DayState::Unavailable;
        return status;
    }
    if !store.has_market() {
        return status;
    }
    let Ok(bounds) = day_bounds(day) else {
        return status;
    };
    let covered = store.all_coverage();
    let mut all_jobs: Vec<TickJob> = Vec::new();
    let mut all_scanned = true;
    // How far a replay can run: the earliest of what each strategy still lacks.
    let mut until = i64::MAX;
    for p in profiles {
        let fp = p.scan_fingerprint();
        let scanned = store.scan_fingerprint(&p.strategy_id).as_deref() == Some(fp.as_str());
        let partial = (!scanned)
            .then(|| store.partial(&p.strategy_id))
            .flatten()
            .filter(|x| x.fingerprint == fp);
        all_scanned &= scanned;
        let hits = if scanned || partial.is_some() {
            store.hits(&p.strategy_id)
        } else {
            Vec::new()
        };
        let wins = plan::windows(p, &hits, bounds);
        let jobs = plan::tick_jobs(&[(p.ticks, wins.clone())], &covered);
        until = until.min(match (&partial, scanned) {
            (_, true) => i64::MAX,
            (Some(x), false) => plan::replayable_until(p, x.final_ms),
            (None, false) => bounds.0,
        });
        if let Some(first) = jobs.iter().map(|j| j.span.0).min() {
            until = until.min(first - 60_000);
        }
        status.ready_hits += hits
            .iter()
            .filter(|(sym, h)| plan::hit_ready(p, sym, h, &wins, &jobs))
            .count();
        status.strategies.push(StrategyDayStatus {
            strategy_id: p.strategy_id.clone(),
            scanned,
            partial: partial.is_some(),
            hits: hits.len(),
            symbols: wins.len(),
            missing_ms: jobs.iter().map(|j| j.span.1 - j.span.0).sum(),
        });
        status.opportunities.extend(hits.into_iter().map(|(symbol, h)| Opportunity {
            strategy_id: p.strategy_id.clone(),
            symbol,
            alert_ms: h.alert_ms,
            score: h.score,
            reason: h.reason,
        }));
        all_jobs.extend(jobs);
    }
    status.opportunities.sort_by_key(|o| o.alert_ms);
    let (bytes, ms) = store.tick_totals();
    let rate = if ms > 0 {
        bytes as f64 / ms as f64
    } else {
        plan::DEFAULT_BYTES_PER_MS
    };
    let (eb, er) = plan::estimate(&all_jobs, rate);
    status.missing_ms = all_jobs.iter().map(|j| j.span.1 - j.span.0).sum();
    status.estimate_bytes = eb;
    status.estimate_requests = er;
    status.state = if all_scanned && all_jobs.is_empty() {
        DayState::Ready
    } else {
        DayState::Incomplete
    };
    if status.state == DayState::Ready {
        status.launchable = true;
    } else {
        let until = until.clamp(bounds.0, bounds.1);
        status.ready_until_ms = Some(until);
        // Startable on its first opportunities: they are on disk, the replay can run
        // at least up to the first one, and what is there was prepared under the
        // current settings (a stale scan has to be redone first).
        status.launchable = status.ready_hits >= plan::QUICK_START_HITS
            && status.opportunities.first().is_some_and(|o| until > o.alert_ms)
            && status.strategies.iter().all(|s| s.scanned || s.partial);
    }
    status
}

/// The opportunities of `ids` stored for `day`, time-ordered, whatever settings
/// they were scanned under: (alert time, symbol).
pub fn opportunities(app_dir: &Path, day: &str, ids: &[String]) -> Vec<(i64, String)> {
    let Some(store) = DayStore::open_existing(app_dir, day) else {
        return Vec::new();
    };
    let mut out: Vec<(i64, String)> = super::needs_registry()
        .iter()
        .filter(|n| ids.is_empty() || ids.iter().any(|i| i == n.id()))
        .flat_map(|n| store.hits(n.id()))
        .map(|(sym, h)| (h.alert_ms, sym))
        .collect();
    out.sort();
    out
}

fn file_bytes(path: &Path) -> u64 {
    let wal = PathBuf::from(format!("{}-wal", path.display()));
    [path.to_path_buf(), wal]
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .sum()
}

// ─── Preparation ───────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct PrepareDeps {
    pub app_dir: PathBuf,
    pub db: Arc<Mutex<rusqlite::Connection>>,
    pub creds: Creds,
}

/// Queue `days` for the strategies `ids` (all registered when empty) and make sure
/// the worker runs. A day already waiting is replaced by the new request. With
/// `priority` the days go to the head of the queue, in the order given, and the day
/// in progress yields to them.
pub fn enqueue(
    shared: &Arc<ReplayDataShared>,
    deps: PrepareDeps,
    days: Vec<String>,
    ids: Vec<String>,
    scan_only: bool,
    priority: bool,
) {
    let mut q = shared.queue.lock().unwrap();
    let mut status = shared.status.write().unwrap();
    for (i, day) in days.into_iter().enumerate() {
        // A full preparation already waiting is not downgraded to a scan.
        let mut job = Job {
            day,
            ids: ids.clone(),
            scan_only,
            priority,
        };
        if let Some(at) = q.iter().position(|j| j.day == job.day) {
            let old = q.remove(at).expect("queued job");
            job.scan_only &= old.scan_only;
            job.priority |= old.priority;
        }
        if priority {
            let at = i.min(q.len());
            q.insert(at, job);
        } else {
            q.push_back(job);
        }
    }
    let waiting_first = q.front().is_some_and(|j| j.priority);
    if status.running {
        // The day in progress steps aside unless it is itself what was asked for.
        if waiting_first && q.front().map(|j| &j.day) != status.day.as_ref() {
            shared.preempt.store(true, Ordering::Relaxed);
        }
        status.queue = q.iter().map(|j| j.day.clone()).collect();
        status.day_total = status.day_index + 1 + q.len();
        return;
    }
    shared.cancel.store(false, Ordering::Relaxed);
    shared.preempt.store(false, Ordering::Relaxed);
    *status = PrepareStatus {
        running: true,
        state: "running".into(),
        day_total: q.len(),
        queue: q.iter().map(|j| j.day.clone()).collect(),
        ..Default::default()
    };
    tauri::async_runtime::spawn(worker(deps, shared.clone()));
}

/// Prepare the queued days one after the other until the queue is empty or the run
/// is cancelled, reporting through `shared`. A day that fails is recorded in
/// `skipped` and the run moves on.
async fn worker(deps: PrepareDeps, shared: Arc<ReplayDataShared>) {
    // The pace must be the plan's the key has NOW: ask once per run. An
    // inconclusive answer keeps whatever was known.
    if let Ok(plan) = crate::alpaca::plan::probe(&deps.creds.key, &deps.creds.secret).await {
        crate::alpaca::ratelimit::set_plan(plan);
        let key = crate::alpaca::plan::PLAN_META_KEY;
        let conn = deps.db.lock().unwrap();
        if crate::local_db::cache_repository::get_app_meta(&conn, key).as_deref() != Some(plan.as_str()) {
            let _ = crate::local_db::cache_repository::set_app_meta(&conn, key, plan.as_str());
        }
    }
    let mut done = 0usize;
    let mut failed = 0usize;
    let mut was_cancelled = false;
    // How long the days that needed real work took, for the estimate.
    let mut timed: Vec<f64> = Vec::new();
    loop {
        let job = {
            let mut q = shared.queue.lock().unwrap();
            let mut s = shared.status.write().unwrap();
            let Some(job) = q.pop_front() else {
                s.running = false;
                s.day_index = done + failed;
                s.eta_secs = None;
                s.state = if was_cancelled {
                    "cancelled".into()
                } else if failed > 0 && done == 0 {
                    s.error = s.skipped.first().map(|(_, e)| e.clone());
                    "error".into()
                } else {
                    "done".into()
                };
                return;
            };
            shared.preempt.store(false, Ordering::Relaxed);
            was_cancelled = false;
            s.day = Some(job.day.clone());
            s.day_index = done + failed;
            s.day_total = done + failed + 1 + q.len();
            s.queue = q.iter().map(|j| j.day.clone()).collect();
            s.stage = String::new();
            s.progress = 0.0;
            s.message = None;
            let avg = (!timed.is_empty()).then(|| timed.iter().sum::<f64>() / timed.len() as f64);
            s.eta_secs = avg.map(|a| (a * (1 + q.len()) as f64) as u64);
            job
        };
        let needs: Vec<Box<dyn ReplayDataNeeds>> = super::needs_registry()
            .into_iter()
            .filter(|n| job.ids.is_empty() || job.ids.iter().any(|i| i == n.id()))
            .collect();
        // One snapshot of the settings per day: a setting changed mid-day shows up
        // as "à compléter" afterwards rather than mixing two scans.
        let profiles: Vec<DataProfile> = {
            let conn = deps.db.lock().unwrap();
            needs.iter().map(|n| n.profile(&conn)).collect()
        };
        let t0 = std::time::Instant::now();
        let day = job.day.clone();
        match prepare_day(&deps, &shared, &job.day, &needs, &profiles, job.scan_only).await {
            Ok(true) => {
                done += 1;
                let secs = t0.elapsed().as_secs_f64();
                if secs > 5.0 && !job.scan_only {
                    timed.push(secs);
                }
            }
            Ok(false) => {
                if shared.cancel.swap(false, Ordering::Relaxed) {
                    was_cancelled = true;
                } else {
                    // Yielded to a priority day: back in line right behind them.
                    let mut q = shared.queue.lock().unwrap();
                    let at = q.iter().take_while(|j| j.priority).count();
                    q.insert(at, job);
                }
            }
            Err(e) => {
                eprintln!("[tagdash] replay_data: {}: {e}", job.day);
                failed += 1;
                shared.status.write().unwrap().skipped.push((job.day.clone(), e));
            }
        }
        shared.bump(&day);
    }
}

/// Prepare `days` and wait for the run to end — the tests' and harnesses' entry.
#[cfg(test)]
pub async fn prepare_days(
    deps: PrepareDeps,
    shared: Arc<ReplayDataShared>,
    days: Vec<String>,
    ids: Vec<String>,
    scan_only: bool,
) {
    enqueue(&shared, deps, days, ids, scan_only, false);
    while shared.is_running() {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}

/// `HH:MM` ET of an instant, for the progress messages.
fn et_hm(ms: i64) -> String {
    let m = DateTime::from_timestamp_millis(ms).map(crate::time::et_minutes).unwrap_or(0);
    format!("{:02}:{:02}", m / 60, m % 60)
}

/// Ok(true) when the day went through, Ok(false) when it stopped early (cancelled,
/// or yielding to a priority day) with everything fetched so far on disk.
async fn prepare_day(
    deps: &PrepareDeps,
    shared: &ReplayDataShared,
    day: &str,
    needs: &[Box<dyn ReplayDataNeeds>],
    profiles: &[DataProfile],
    scan_only: bool,
) -> Result<bool, String> {
    if let Some(why) = unavailable_reason(day, Utc::now()) {
        return Err(why.to_string());
    }
    let bounds = day_bounds(day)?;
    let mut store = DayStore::open(&deps.app_dir, day)?;
    if store.meta("closed").as_deref() == Some("1") {
        return Err("pas de séance ce jour-là".into());
    }

    // 1. Market snapshot.
    if !store.has_market() {
        shared.stage("market", 0.0, Some("instantané du marché".into()));
        let symbols = {
            let conn = deps.db.lock().unwrap();
            crate::local_db::universe_repository::get_active_symbols(&conn).map_err(|e| e.to_string())?
        };
        let rows = fetch_market(&deps.creds, &symbols, day, shared).await?;
        if shared.interrupted() {
            return Ok(false);
        }
        if rows.is_empty() {
            store.set_meta("closed", "1")?;
            return Err("pas de séance ce jour-là (férié ?)".into());
        }
        store.put_market(&rows)?;
    }
    let market: HashMap<String, MarketRow> =
        store.market().into_iter().map(|m| (m.symbol.clone(), m)).collect();

    // 2–3. Minutes, minute pre-scan and confirmation on prints — only for the
    // profiles whose fingerprint changed (a new day: all of them).
    let floats: HashMap<String, u64> = {
        let conn = deps.db.lock().unwrap();
        crate::local_db::universe_repository::get_all(&conn)
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter_map(|a| a.float_shares.filter(|f| *f > 0).map(|f| (a.symbol, f as u64)))
            .collect()
    };
    let stale: Vec<usize> = (0..profiles.len())
        .filter(|&i| {
            store.scan_fingerprint(&profiles[i].strategy_id).as_deref()
                != Some(profiles[i].scan_fingerprint().as_str())
        })
        .collect();
    if !stale.is_empty() {
        let rows: Vec<MarketRow> = market.values().cloned().collect();
        let stale_profiles: Vec<&DataProfile> = stale.iter().map(|&i| &profiles[i]).collect();
        let eligible_for = |p: &DataProfile, sym: &str, buckets: &[u64]| {
            market
                .get(sym)
                .is_some_and(|m| plan::eligible(p, m, floats.get(sym).copied(), buckets))
        };
        let stats = store.symbol_stats();
        // Stored minutes are read back; everything else a stale profile could use
        // is fetched (never fetched, or fetched and dropped under older settings).
        let mut minutes: HashMap<String, Vec<Bar>> = HashMap::new();
        for (sym, (kept, buckets)) in &stats {
            if *kept && stale_profiles.iter().any(|p| eligible_for(p, sym, buckets)) {
                minutes.insert(sym.clone(), store.minutes(sym));
            }
        }
        let owned: Vec<DataProfile> = stale_profiles.iter().map(|p| (*p).clone()).collect();
        let to_fetch: Vec<String> = plan::candidates(&owned, &rows, &floats)
            .into_iter()
            .filter(|s| match stats.get(s) {
                None => true,
                Some((kept, buckets)) => {
                    !kept && stale_profiles.iter().any(|p| eligible_for(p, s, buckets))
                }
            })
            .collect();

        // Minute pre-scan, chunk by chunk so a whole market's minutes never sit in
        // memory at once: only symbols with a minute hit keep their bars.
        let scanners: Vec<Box<dyn super::SymbolScanner>> =
            stale.iter().map(|&i| needs[i].scanner(&profiles[i])).collect();
        let mut raw: Vec<BTreeMap<String, Vec<super::Hit>>> = vec![BTreeMap::new(); stale.len()];
        let scan_into = |raw: &mut Vec<BTreeMap<String, Vec<super::Hit>>>, sym: &str, bars: &[Bar]| {
            let buckets = store::buckets_of(bars);
            let mut any = false;
            for (k, p) in stale_profiles.iter().enumerate() {
                if !eligible_for(p, sym, &buckets) {
                    continue;
                }
                let hits: Vec<super::Hit> = scanners[k]
                    .scan(bars)
                    .into_iter()
                    .filter(|h| p.in_sessions(h.alert_ms))
                    .collect();
                if !hits.is_empty() {
                    any = true;
                    raw[k].insert(sym.to_string(), hits);
                }
            }
            any
        };
        let stored: Vec<String> = minutes.keys().cloned().collect();
        for sym in &stored {
            if !scan_into(&mut raw, sym, &minutes[sym]) {
                minutes.remove(sym);
            }
        }
        // …a few requests' worth of symbols at a time (the plan's bulk concurrency).
        let batches: Vec<&[String]> = to_fetch
            .chunks(CHUNK * crate::alpaca::ratelimit::bulk_concurrency())
            .collect();
        let n_batches = batches.len().max(1) as f32;
        for (bi, chunk) in batches.iter().enumerate() {
            if shared.interrupted() {
                return Ok(false);
            }
            shared.stage(
                "minutes",
                bi as f32 / n_batches,
                Some(format!("barres 1 min · {} candidats", to_fetch.len())),
            );
            let step = |f: f32| shared.stage("minutes", (bi as f32 + f) / n_batches, None);
            let bars = fetch_bars_with(&deps.creds, chunk, "1Min", bounds, shared, &step).await?;
            if shared.interrupted() {
                // A batch cut short is not recorded: its symbols are fetched again.
                return Ok(false);
            }
            for sym in chunk.iter() {
                let b = bars.get(sym).cloned().unwrap_or_default();
                // Recorded as seen (volume profile only) until confirmation decides.
                store.put_symbol(sym, &store::buckets_of(&b), false, &[])?;
                if scan_into(&mut raw, sym, &b) {
                    minutes.insert(sym.clone(), b);
                }
            }
        }

        // Confirmation on real prints, in time order. Each step's opportunities are
        // stored with their minutes, ticks and news BEFORE the scan's progress moves
        // past them: whatever the store says is confirmed is replayable.
        for (k, &i) in stale.iter().enumerate() {
            let p = &profiles[i];
            let fp = p.scan_fingerprint();
            let label = needs[i].label();
            let resume = store.partial(&p.strategy_id).filter(|x| x.fingerprint == fp);
            let mut partial = match resume {
                Some(x) => x,
                None => {
                    let fresh = PartialScan {
                        fingerprint: fp.clone(),
                        frontier_ms: bounds.0,
                        final_ms: bounds.0,
                        state: serde_json::Value::Null,
                    };
                    store.begin_partial(&p.strategy_id, &fresh)?;
                    shared.bump(day);
                    fresh
                }
            };
            let Some(mut confirmer) =
                Confirmer::new(scanners[k].as_ref(), p, &raw[k], &minutes, bounds, &partial.state)
            else {
                // No probe: the minute hits are final.
                let all: Vec<(String, super::Hit)> = raw[k]
                    .iter()
                    .flat_map(|(s, hs)| hs.iter().map(move |h| (s.clone(), h.clone())))
                    .collect();
                for (sym, _) in &all {
                    if let Some(b) = minutes.get(sym) {
                        store.put_symbol(sym, &store::buckets_of(b), true, b)?;
                    }
                }
                partial.frontier_ms = bounds.1;
                partial.final_ms = bounds.1;
                store.add_hits(&p.strategy_id, &all, &partial)?;
                store.finish_scan(&p.strategy_id, &fp, &serde_json::to_string(p).unwrap_or_default())?;
                shared.bump(day);
                continue;
            };
            loop {
                if shared.interrupted() {
                    return Ok(false);
                }
                shared.stage(
                    "scan",
                    confirmer.progress(),
                    Some(format!(
                        "confirmation {label} · {} ET · {} opportunités",
                        et_hm(confirmer.frontier()),
                        store.hits(&p.strategy_id).len()
                    )),
                );
                let Some(found) = confirmer.step(&deps.creds).await? else {
                    break;
                };
                for (sym, _) in &found {
                    if let Some(b) = minutes.get(sym) {
                        store.put_symbol(sym, &store::buckets_of(b), true, b)?;
                    }
                }
                if !scan_only && !found.is_empty() {
                    let mut known = store.hits(&p.strategy_id);
                    known.extend(found.iter().cloned());
                    let wins = plan::windows(p, &known, bounds);
                    let mut jobs = plan::tick_jobs(&[(p.ticks, wins)], &store.all_coverage());
                    jobs.sort_by_key(|j| j.span.0);
                    for batch in jobs.chunks(crate::alpaca::ratelimit::bulk_concurrency()) {
                        if shared.interrupted() {
                            return Ok(false);
                        }
                        shared.stage("ticks", confirmer.progress(), Some(format!("ticks {}", batch[0].symbol)));
                        // Fetch first, then write: the store must not be borrowed
                        // across an await.
                        let rows = fetch_batch(&deps.creds, batch).await?;
                        put_batch(&store, batch, rows)?;
                    }
                    let seen: std::collections::HashSet<String> = store.news_symbols().into_iter().collect();
                    let syms: Vec<String> = found
                        .iter()
                        .map(|(s, _)| s.clone())
                        .filter(|s| !seen.contains(s))
                        .collect::<std::collections::BTreeSet<_>>()
                        .into_iter()
                        .collect();
                    if !syms.is_empty() {
                        let items = fetch_news(&deps.creds, &syms, bounds, day).await;
                        if let Some(items) = items {
                            store.put_news(&syms, &items)?;
                        }
                    }
                }
                partial.frontier_ms = confirmer.frontier();
                partial.final_ms = confirmer.final_before();
                partial.state = confirmer.state();
                store.add_hits(&p.strategy_id, &found, &partial)?;
                shared.bump(day);
            }
            store.finish_scan(&p.strategy_id, &fp, &serde_json::to_string(p).unwrap_or_default())?;
            shared.bump(day);
        }
    }
    if scan_only {
        return Ok(true);
    }

    // 4. Ticks: whatever is still uncovered in every profile's windows, earliest
    // first — a replay already running on this day reads them in that order.
    let wanted: Vec<(super::TickNeeds, BTreeMap<String, Vec<Span>>)> = profiles
        .iter()
        .map(|p| (p.ticks, plan::windows(p, &store.hits(&p.strategy_id), bounds)))
        .collect();
    let mut jobs = plan::tick_jobs(&wanted, &store.all_coverage());
    jobs.sort_by_key(|j| j.span.0);
    let n_jobs = jobs.len().max(1) as f32;
    let conc = crate::alpaca::ratelimit::bulk_concurrency();
    for (i, batch) in jobs.chunks(conc).enumerate() {
        if shared.interrupted() {
            return Ok(false);
        }
        shared.stage(
            "ticks",
            (i * conc) as f32 / n_jobs,
            Some(format!("ticks {} · {}/{}", batch[0].symbol, i * conc + 1, jobs.len())),
        );
        let rows = fetch_batch(&deps.creds, batch).await?;
        put_batch(&store, batch, rows)?;
        shared.bump(day);
    }

    // 5. News for the symbols that got tick windows.
    let done: std::collections::HashSet<String> = store.news_symbols().into_iter().collect();
    let news_syms: Vec<String> = wanted
        .iter()
        .flat_map(|(_, w)| w.keys().cloned())
        .filter(|s| !done.contains(s))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    if !news_syms.is_empty() {
        shared.stage("news", 0.0, Some(format!("news · {} tickers", news_syms.len())));
        for chunk in news_syms.chunks(50) {
            if let Some(items) = fetch_news(&deps.creds, chunk, bounds, day).await {
                store.put_news(chunk, &items)?;
            }
        }
    }
    store.set_meta("prepared_ms", &Utc::now().timestamp_millis().to_string())?;
    shared.bump(day);
    Ok(true)
}

/// Headlines for `symbols` from a few days before the day to its close. News is
/// context, not tape: a failure is logged and reads as None, never fatal.
async fn fetch_news(
    c: &Creds,
    symbols: &[String],
    bounds: Span,
    day: &str,
) -> Option<Vec<crate::types::NewsHeadline>> {
    crate::alpaca::ratelimit::bulk_pause().await;
    let start = rfc(bounds.0 - NEWS_LOOKBACK_DAYS * 86_400_000);
    let end = rfc(bounds.1);
    match crate::replay::data::fetch_news_window(&c.key, &c.secret, &start, &end, symbols).await {
        Ok(items) => Some(items),
        Err(e) => {
            eprintln!("[tagdash] replay_data: news {day}: {e}");
            None
        }
    }
}

enum JobRows {
    Trades(Vec<TradeRow>),
    Quotes(Vec<QuoteRow>),
}

async fn fetch_job(c: &Creds, job: &TickJob) -> Result<JobRows, String> {
    let (s, e) = job.span;
    Ok(match job.kind {
        TickKind::Trades => JobRows::Trades(
            crate::review::sip::trades(c, &job.symbol, s, e)
                .await?
                .into_iter()
                .map(|t| TradeRow {
                    ts: t.ts_ms,
                    price: t.price,
                    size: t.size,
                    prints: 1,
                    exch: t.exch,
                    cond: t.cond,
                    origin: "sip".into(),
                })
                .collect(),
        ),
        TickKind::Quotes => JobRows::Quotes(
            crate::review::sip::quotes(c, &job.symbol, s, e)
                .await?
                .into_iter()
                .map(|q| QuoteRow {
                    ts: q.ts_ms,
                    bid: q.bid,
                    bid_size: q.bid_size,
                    ask: q.ask,
                    ask_size: q.ask_size,
                    bid_x: q.bid_x,
                    ask_x: q.ask_x,
                    origin: "sip".into(),
                })
                .collect(),
        ),
    })
}

/// Several jobs at once (the plan's bulk concurrency), each retried when Alpaca
/// answers "too many requests".
async fn fetch_batch(c: &Creds, jobs: &[TickJob]) -> Result<Vec<JobRows>, String> {
    futures_util::future::join_all(jobs.iter().map(|job| async move {
        let mut attempts = 0;
        loop {
            crate::alpaca::ratelimit::bulk_pause().await;
            match fetch_job(c, job).await {
                Err(e) if e.contains("429") && attempts < 3 => {
                    attempts += 1;
                    tokio::time::sleep(std::time::Duration::from_secs(10 * attempts)).await;
                }
                other => return other,
            }
        }
    }))
    .await
    .into_iter()
    .collect()
}

fn put_batch(store: &DayStore, jobs: &[TickJob], rows: Vec<JobRows>) -> Result<(), String> {
    for (job, rows) in jobs.iter().zip(rows) {
        match rows {
            JobRows::Trades(rows) => store.put_trades(&job.symbol, job.span, &rows)?,
            JobRows::Quotes(rows) => store.put_quotes(&job.symbol, job.span, &rows)?,
        }
    }
    Ok(())
}

// ─── Confirmation on real prints ───────────────────────────────────────────────

/// Probes sharing a request: their spans start within this of the first one's.
const PROBE_SLOT_MS: i64 = 5 * 60_000;
/// Symbols per multi-symbol trades request.
const PROBE_SYMBOLS: usize = 100;

/// Where one symbol's confirmation stands: where its next probe may start, the
/// probes spent in the current lock segment, that segment, and the alerts
/// confirmed. The probe budget is per segment: a runner that spends it all in
/// premarket gets a fresh one at the open.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
struct SymState(i64, usize, Option<usize>, usize);

/// Confirms each symbol's minute hits on the real prints of the profile's probe
/// span, EARLIEST FIRST: every step probes the symbols whose next unseen hit comes
/// first, one request per time slot for all the symbols probed in it. A confirmed
/// alert silences the symbol until its lock segment ends; a symbol stops after its
/// probe budget. Going through the day in order is what gives the scan a frontier
/// (`frontier`, `final_before`) a replay can start behind.
pub(crate) struct Confirmer<'a> {
    scanner: &'a dyn super::SymbolScanner,
    p: &'a DataProfile,
    raw: &'a BTreeMap<String, Vec<super::Hit>>,
    minutes: &'a HashMap<String, Vec<Bar>>,
    day: Span,
    pad: super::WindowPad,
    st: HashMap<&'a str, SymState>,
    /// First and last minute hit, for the progress bar.
    span: Span,
}

impl<'a> Confirmer<'a> {
    /// None when the profile declares no probe (minute hits are final). `resume`
    /// is a previous run's `state()` under the same fingerprint, or Null.
    pub(crate) fn new(
        scanner: &'a dyn super::SymbolScanner,
        p: &'a DataProfile,
        raw: &'a BTreeMap<String, Vec<super::Hit>>,
        minutes: &'a HashMap<String, Vec<Bar>>,
        day: Span,
        resume: &serde_json::Value,
    ) -> Option<Self> {
        let pad = p.probe?;
        let saved: HashMap<String, SymState> =
            serde_json::from_value(resume.clone()).unwrap_or_default();
        let st = raw
            .keys()
            .map(|s| {
                let state = saved.get(s).copied().unwrap_or(SymState(i64::MIN, 0, None, 0));
                (s.as_str(), state)
            })
            .collect();
        let times = || raw.values().flat_map(|hs| hs.iter().map(|h| h.alert_ms));
        let span = (times().min().unwrap_or(day.0), times().max().unwrap_or(day.1));
        Some(Self {
            scanner,
            p,
            raw,
            minutes,
            day,
            pad,
            st,
            span,
        })
    }

    /// Each symbol's next hit to probe, earliest first.
    fn pending(&self) -> Vec<(&'a str, &'a super::Hit)> {
        let p = self.p;
        let mut probes: Vec<(&'a str, &'a super::Hit)> = self
            .raw
            .iter()
            .filter_map(|(sym, hits)| {
                let SymState(mut from, spent, seg, alerts) = self.st[sym.as_str()];
                if alerts >= p.max_alerts_per_symbol {
                    return None;
                }
                loop {
                    let h = plan::next_probe(hits, from)?;
                    let hs = plan::segment_of(p, h.alert_ms);
                    if hs != seg || spent < p.max_probes_per_symbol {
                        return Some((sym.as_str(), h));
                    }
                    // Budget spent for this segment: jump to the next one.
                    from = match hs {
                        Some(_) => plan::unlock_at(p, h.alert_ms, self.day),
                        None => return None,
                    };
                    if from <= h.alert_ms {
                        return None;
                    }
                }
            })
            .collect();
        probes.sort_by_key(|(sym, h)| (h.alert_ms, *sym));
        probes
    }

    /// Every minute hit before this has been probed (the day's end once none is
    /// left).
    pub(crate) fn frontier(&self) -> i64 {
        self.pending().first().map_or(self.day.1, |(_, h)| h.alert_ms)
    }

    /// Every alert before this is known: a hit still to probe cannot confirm
    /// earlier than its own lead.
    pub(crate) fn final_before(&self) -> i64 {
        let frontier = self.frontier();
        if frontier >= self.day.1 {
            return self.day.1;
        }
        frontier - self.scanner.confirm_lead_ms().unwrap_or(self.pad.before_ms)
    }

    /// 0..1 through the day's minute hits.
    pub(crate) fn progress(&self) -> f32 {
        let len = (self.span.1 - self.span.0).max(1) as f32;
        ((self.frontier() - self.span.0) as f32 / len).clamp(0.0, 1.0)
    }

    /// What `new` takes back to resume here.
    pub(crate) fn state(&self) -> serde_json::Value {
        serde_json::to_value(&self.st).unwrap_or_default()
    }

    /// Probe the earliest pending hits — as many slots at once as the plan's bulk
    /// concurrency — and return the alerts they confirm, time-ordered. None once
    /// nothing is left to probe.
    pub(crate) async fn step(&mut self, c: &Creds) -> Result<Option<Vec<(String, super::Hit)>>, String> {
        let pending = self.pending();
        if pending.is_empty() {
            return Ok(None);
        }
        let slots = crate::alpaca::ratelimit::bulk_concurrency();
        let mut groups: Vec<Vec<(&'a str, &'a super::Hit)>> = Vec::new();
        for pr in pending {
            let fits = groups
                .last()
                .is_some_and(|g| pr.1.alert_ms - g[0].1.alert_ms <= PROBE_SLOT_MS && g.len() < PROBE_SYMBOLS);
            if fits {
                groups.last_mut().expect("a group").push(pr);
            } else if groups.len() == slots {
                break;
            } else {
                groups.push(vec![pr]);
            }
        }
        let (pad, day) = (self.pad, self.day);
        let fetched = futures_util::future::join_all(groups.iter().map(|g| {
            let lo = (g.iter().map(|(_, h)| h.alert_ms).min().unwrap_or(0) - pad.before_ms).max(day.0);
            let hi = (g.iter().map(|(_, h)| h.alert_ms).max().unwrap_or(0) + pad.after_ms).min(day.1);
            let syms: Vec<String> = g.iter().map(|(s, _)| s.to_string()).collect();
            async move { fetch_trades_multi(c, &syms, (lo, hi)).await }
        }))
        .await;

        let p = self.p;
        let no_bars: Vec<Bar> = Vec::new();
        let mut out: Vec<(String, super::Hit)> = Vec::new();
        for (g, trades) in groups.iter().zip(fetched) {
            let trades = trades?;
            for (sym, hit) in g {
                let (a, b) = (hit.alert_ms - pad.before_ms, hit.alert_ms + pad.after_ms);
                let rows: Vec<TradeRow> = trades
                    .get(*sym)
                    .map(|v| v.iter().filter(|t| a <= t.ts && t.ts < b).cloned().collect())
                    .unwrap_or_default();
                let e = self.st.get_mut(*sym).expect("probed symbol");
                let seg = plan::segment_of(p, hit.alert_ms);
                if seg != e.2 {
                    e.1 = 0;
                    e.2 = seg;
                }
                e.1 += 1;
                // The next probe starts where this one's verdict ends — not where its
                // trades end, or hits in between would never be looked at.
                e.0 = hit.alert_ms + p.probe_step_ms.max(60_000);
                // An arm near a session's end can confirm past it; the engine is off
                // there, so it is not an alert.
                if let Some(conf) = self
                    .scanner
                    .confirm(self.minutes.get(*sym).unwrap_or(&no_bars), hit, &rows)
                    .filter(|c| p.in_sessions(c.alert_ms))
                {
                    e.3 += 1;
                    e.0 = e.0.max(plan::unlock_at(p, conf.alert_ms, day));
                    out.push((sym.to_string(), conf));
                }
            }
        }
        out.sort_by_key(|(_, h)| h.alert_ms);
        Ok(Some(out))
    }
}

/// Every confirmed alert of the day, time-ordered — the confirmation run to its
/// end in one go (calibration harnesses).
#[cfg(test)]
pub(crate) async fn confirm_hits(
    c: &Creds,
    scanner: &dyn super::SymbolScanner,
    p: &DataProfile,
    raw: &BTreeMap<String, Vec<super::Hit>>,
    minutes: &HashMap<String, Vec<Bar>>,
    day: Span,
    shared: &ReplayDataShared,
) -> Result<Vec<(String, super::Hit)>, String> {
    let Some(mut confirmer) = Confirmer::new(scanner, p, raw, minutes, day, &serde_json::Value::Null) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    while !shared.cancelled() {
        match confirmer.step(c).await? {
            Some(found) => out.extend(found),
            None => break,
        }
    }
    out.sort_by_key(|(_, h)| h.alert_ms);
    Ok(out)
}

#[derive(Deserialize)]
struct TradesPage {
    #[serde(default)]
    trades: HashMap<String, Vec<RawTrade>>,
    #[serde(default)]
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
struct RawTrade {
    t: String,
    p: f64,
    s: f64,
    #[serde(default)]
    x: Option<String>,
    #[serde(default)]
    c: Option<Vec<String>>,
}

/// SIP prints of several symbols over one span, time-ordered per symbol.
pub(crate) async fn fetch_trades_multi(
    c: &Creds,
    symbols: &[String],
    span: Span,
) -> Result<HashMap<String, Vec<TradeRow>>, String> {
    let client = crate::http::client();
    let mut out: HashMap<String, Vec<TradeRow>> = HashMap::new();
    let mut syms: Vec<String> = symbols
        .iter()
        .filter(|s| crate::replay::data::is_rest_symbol(s))
        .cloned()
        .collect();
    let mut token: Option<String> = None;
    let mut attempts = 0;
    while !syms.is_empty() {
        let mut url = format!(
            "{DATA}/v2/stocks/trades?symbols={}&start={}&end={}&limit=10000&feed=sip",
            syms.join(","),
            rfc(span.0),
            rfc(span.1)
        );
        if let Some(t) = &token {
            url.push_str(&format!("&page_token={t}"));
        }
        crate::alpaca::ratelimit::acquire_bulk().await;
        let resp = client
            .get(&url)
            .header("APCA-API-KEY-ID", &c.key)
            .header("APCA-API-SECRET-KEY", &c.secret)
            .timeout(std::time::Duration::from_secs(60))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            if status.as_u16() == 400 {
                if let Some(bad) = crate::replay::data::invalid_symbol_of(&body) {
                    let before = syms.len();
                    syms.retain(|s| s != &bad);
                    if syms.len() < before {
                        out.clear();
                        token = None;
                        continue;
                    }
                }
            }
            if status.as_u16() == 429 && attempts < 3 {
                attempts += 1;
                tokio::time::sleep(std::time::Duration::from_secs(10 * attempts)).await;
                continue;
            }
            return Err(format!("Alpaca trades HTTP {status}: {}", &body[..body.len().min(200)]));
        }
        let page: TradesPage = resp.json().await.map_err(|e| e.to_string())?;
        for (sym, raws) in page.trades {
            let v = out.entry(sym).or_default();
            for r in raws {
                let Ok(t) = DateTime::parse_from_rfc3339(&r.t) else {
                    continue;
                };
                v.push(TradeRow {
                    ts: t.timestamp_millis(),
                    price: r.p,
                    size: r.s as i64,
                    prints: 1,
                    exch: r.x,
                    cond: r.c.map(|v| v.join("")),
                    origin: "sip".into(),
                });
            }
        }
        match page.next_page_token.filter(|t| !t.is_empty()) {
            Some(t) => token = Some(t),
            None => break,
        }
    }
    for v in out.values_mut() {
        v.sort_by_key(|t| t.ts);
    }
    Ok(out)
}

// ─── Alpaca multi-symbol bars (raw, SIP) ───────────────────────────────────────

#[derive(Deserialize)]
struct BarsPage {
    #[serde(default)]
    bars: HashMap<String, Vec<RawBar>>,
    #[serde(default)]
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
struct RawBar {
    t: String,
    o: f64,
    h: f64,
    l: f64,
    c: f64,
    v: f64,
    #[serde(default)]
    n: Option<i64>,
    #[serde(default)]
    vw: Option<f64>,
}

fn rfc(ms: i64) -> String {
    DateTime::from_timestamp_millis(ms)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub(crate) async fn fetch_bars(
    c: &Creds,
    symbols: &[String],
    timeframe: &str,
    span: Span,
    shared: &ReplayDataShared,
    stage: &str,
) -> Result<HashMap<String, Vec<Bar>>, String> {
    fetch_bars_with(c, symbols, timeframe, span, shared, &|f| shared.stage(stage, f, None)).await
}

/// `fetch_bars`, reporting its own 0..1 progress to `progress` — several requests
/// in flight at once (the plan's bulk concurrency), one per chunk of symbols.
async fn fetch_bars_with(
    c: &Creds,
    symbols: &[String],
    timeframe: &str,
    span: Span,
    shared: &ReplayDataShared,
    progress: &(dyn Fn(f32) + Sync),
) -> Result<HashMap<String, Vec<Bar>>, String> {
    use futures_util::StreamExt;
    let mut out: HashMap<String, Vec<Bar>> = HashMap::new();
    // Alpaca Basic refuses any SIP window ending in the last 15 minutes — even for a
    // past day, when the range runs past now (e.g. the day after, for dailies).
    let span = (
        span.0,
        span.1
            .min(Utc::now().timestamp_millis() - AVAILABLE_AFTER_CLOSE_MIN * 60_000),
    );
    if span.1 <= span.0 {
        return Ok(out);
    }
    let usable: Vec<String> = symbols
        .iter()
        .filter(|s| crate::replay::data::is_rest_symbol(s))
        .cloned()
        .collect();
    let chunks: Vec<Vec<String>> = usable.chunks(CHUNK).map(<[String]>::to_vec).collect();
    let total = chunks.len().max(1) as f32;
    let done = std::sync::atomic::AtomicUsize::new(0);
    let done = &done;
    let parts: Vec<Result<HashMap<String, Vec<Bar>>, String>> =
        futures_util::stream::iter(chunks.into_iter().map(|chunk| async move {
            // Stopped (or yielding to a priority day): what is not started is not.
            if shared.interrupted() {
                return Ok(HashMap::new());
            }
            let part = fetch_bars_chunk(c, chunk, timeframe, span).await;
            progress((done.fetch_add(1, Ordering::Relaxed) + 1) as f32 / total);
            part
        }))
        .buffer_unordered(crate::alpaca::ratelimit::bulk_concurrency())
        .collect()
        .await;
    for part in parts {
        for (sym, bars) in part? {
            out.entry(sym).or_default().extend(bars);
        }
    }
    for v in out.values_mut() {
        v.sort_by_key(|b| b.time);
        v.dedup_by_key(|b| b.time);
    }
    Ok(out)
}

/// One multi-symbol bars request, every page of it.
async fn fetch_bars_chunk(
    c: &Creds,
    mut chunk: Vec<String>,
    timeframe: &str,
    span: Span,
) -> Result<HashMap<String, Vec<Bar>>, String> {
    let client = crate::http::client();
    let mut out: HashMap<String, Vec<Bar>> = HashMap::new();
    let mut token: Option<String> = None;
    let mut attempts = 0;
    while !chunk.is_empty() {
        let mut url = format!(
            "{DATA}/v2/stocks/bars?symbols={}&timeframe={timeframe}&start={}&end={}\
             &limit=10000&adjustment=raw&feed=sip",
            chunk.join(","),
            rfc(span.0),
            rfc(span.1)
        );
        if let Some(t) = &token {
            url.push_str(&format!("&page_token={t}"));
        }
        crate::alpaca::ratelimit::acquire_bulk().await;
        let resp = client
            .get(&url)
            .header("APCA-API-KEY-ID", &c.key)
            .header("APCA-API-SECRET-KEY", &c.secret)
            .timeout(std::time::Duration::from_secs(60))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            if status.as_u16() == 400 {
                if let Some(bad) = crate::replay::data::invalid_symbol_of(&body) {
                    let before = chunk.len();
                    chunk.retain(|s| s != &bad);
                    if chunk.len() < before {
                        // From its first page again, without the offender.
                        out.clear();
                        token = None;
                        continue;
                    }
                }
            }
            if status.as_u16() == 429 && attempts < 3 {
                attempts += 1;
                tokio::time::sleep(std::time::Duration::from_secs(10 * attempts)).await;
                continue;
            }
            return Err(format!(
                "Alpaca bars HTTP {status}: {}",
                &body[..body.len().min(200)]
            ));
        }
        let page: BarsPage = resp.json().await.map_err(|e| e.to_string())?;
        for (sym, raws) in page.bars {
            let v = out.entry(sym).or_default();
            for r in raws {
                let Ok(t) = DateTime::parse_from_rfc3339(&r.t) else {
                    continue;
                };
                v.push(Bar {
                    time: t.with_timezone(&Utc),
                    open: r.o,
                    high: r.h,
                    low: r.l,
                    close: r.c,
                    volume: r.v.max(0.0) as u64,
                    vwap: r.vw,
                    trade_count: r.n.map(|n| n.max(0) as u64),
                });
            }
        }
        match page.next_page_token.filter(|t| !t.is_empty()) {
            Some(t) => token = Some(t),
            None => break,
        }
    }
    Ok(out)
}

/// The day's whole-market snapshot: its daily bar plus the previous close.
pub(crate) async fn fetch_market(
    c: &Creds,
    symbols: &[String],
    day: &str,
    shared: &ReplayDataShared,
) -> Result<Vec<MarketRow>, String> {
    let noon = crate::replay::data::noon_utc(parse_day(day)?);
    let start = (noon - Duration::days(14)).timestamp_millis();
    let end = (noon + Duration::days(1)).timestamp_millis();
    let bars = fetch_bars(c, symbols, "1Day", (start, end), shared, "market").await?;
    let mut out = Vec::new();
    for (sym, v) in bars {
        let Some(idx) = v.iter().position(|b| crate::time::et_date(b.time) == day) else {
            continue;
        };
        let b = &v[idx];
        out.push(MarketRow {
            symbol: sym,
            prev_close: idx.checked_sub(1).map(|p| v[p].close),
            o: b.open,
            h: b.high,
            l: b.low,
            c: b.close,
            v: b.volume as i64,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn weekends_and_unfinished_sessions_are_unavailable() {
        let later = Utc.with_ymd_and_hms(2026, 7, 1, 12, 0, 0).unwrap();
        assert_eq!(unavailable_reason("2026-06-06", later), Some("week-end"));
        assert_eq!(unavailable_reason("2026-06-05", later), None);
        // 2026-06-05 20:00 ET = 00:00 UTC on the 6th; 10 minutes later is too early.
        let just_after = Utc.with_ymd_and_hms(2026, 6, 6, 0, 10, 0).unwrap();
        assert!(unavailable_reason("2026-06-05", just_after).is_some());
        let enough = Utc.with_ymd_and_hms(2026, 6, 6, 0, 16, 0).unwrap();
        assert_eq!(unavailable_reason("2026-06-05", enough), None);
    }

    /// End-to-end against the real Alpaca API. Ignored by default; run with
    ///   TAGDASH_E2E_APP=<data root, holding user/secrets.toml>
    ///   TAGDASH_E2E_DB=<a COPY of cache.db> TAGDASH_E2E_OUT=<scratch dir>
    ///   TAGDASH_E2E_DAY=YYYY-MM-DD cargo test --lib prepare_e2e -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn prepare_e2e() {
        let env = |k: &str| std::env::var(k).unwrap_or_else(|_| panic!("{k} manquant"));
        let app = PathBuf::from(env("TAGDASH_E2E_APP"));
        let out = PathBuf::from(env("TAGDASH_E2E_OUT"));
        let day = env("TAGDASH_E2E_DAY");
        let db = crate::local_db::open_harness(std::path::Path::new(&env("TAGDASH_E2E_DB")));
        let creds = crate::review::sip::creds(&crate::config::secrets::load(&app)).expect("clés Alpaca");
        let deps = PrepareDeps {
            app_dir: out.clone(),
            db: Arc::new(Mutex::new(db)),
            creds,
        };
        let shared = Arc::new(ReplayDataShared::default());
        let stages: &[(&str, bool)] = if std::env::var("TAGDASH_E2E_SCAN_ONLY").is_ok() {
            &[("scan", true)]
        } else {
            &[("scan", true), ("ticks", false), ("rerun", false)]
        };
        for &(label, scan_only) in stages {
            let t0 = std::time::Instant::now();
            prepare_days(
                PrepareDeps {
                    app_dir: deps.app_dir.clone(),
                    db: deps.db.clone(),
                    creds: Creds {
                        key: deps.creds.key.clone(),
                        secret: deps.creds.secret.clone(),
                    },
                },
                shared.clone(),
                vec![day.clone()],
                vec![],
                scan_only,
            )
            .await;
            let st = shared.status.read().unwrap().clone();
            let profiles: Vec<DataProfile> = {
                let conn = deps.db.lock().unwrap();
                super::super::needs_registry().iter().map(|n| n.profile(&conn)).collect()
            };
            let ds = day_status(&out, &day, &profiles);
            let store = DayStore::open_existing(&out, &day).unwrap();
            println!(
                "[{label}] {:.1}s state={} skipped={:?} day={:?} market={} fetched={} kept={} hits={} symbols={:?} missing_ms={} est={}B disk={}B ticks={:?}",
                t0.elapsed().as_secs_f32(),
                st.state,
                st.skipped,
                ds.state,
                store.market().len(),
                store.symbol_stats().len(),
                store.kept_symbols().len(),
                ds.opportunities.len(),
                ds.strategies.iter().map(|s| s.symbols).collect::<Vec<_>>(),
                ds.missing_ms,
                ds.estimate_bytes,
                ds.bytes_on_disk,
                store.tick_totals(),
            );
            if !scan_only {
                let t1 = std::time::Instant::now();
                let dd = super::super::read::read_day(&out, &day).unwrap();
                let trades = dd
                    .events
                    .iter()
                    .filter(|e| matches!(e.ev, crate::replay::data::Event::Trade { .. }))
                    .count();
                println!(
                    "   relecture {:.1}s · {} événements ({} trades) · source={} · {} symboles · requêtes cumulées {}",
                    t1.elapsed().as_secs_f32(),
                    dd.events.len(),
                    trades,
                    dd.source,
                    dd.symbols,
                    crate::alpaca::ratelimit::requests()
                );
            }
            for o in ds.opportunities.iter().take(15) {
                println!(
                    "   {} {} {}",
                    crate::time::et_minutes(DateTime::from_timestamp_millis(o.alert_ms).unwrap()) / 60,
                    o.symbol,
                    o.reason
                );
            }
        }
    }

    /// The quick start, against the real Alpaca API: prepares a day from nothing and
    /// prints when it becomes launchable (its first opportunities on disk), then
    /// cancels, resumes and lets it finish. Ignored by default; same variables as
    /// `prepare_e2e`:
    ///   cargo test --lib quick_start_e2e -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn quick_start_e2e() {
        let env = |k: &str| std::env::var(k).unwrap_or_else(|_| panic!("{k} manquant"));
        let app = PathBuf::from(env("TAGDASH_E2E_APP"));
        let out = PathBuf::from(env("TAGDASH_E2E_OUT"));
        let day = env("TAGDASH_E2E_DAY");
        let db = Arc::new(Mutex::new(crate::local_db::open_harness(std::path::Path::new(&env("TAGDASH_E2E_DB")))));
        let creds = crate::review::sip::creds(&crate::config::secrets::load(&app)).expect("clés Alpaca");
        let deps = PrepareDeps {
            app_dir: out.clone(),
            db: db.clone(),
            creds,
        };
        let profiles: Vec<DataProfile> = {
            let conn = db.lock().unwrap();
            super::super::needs_registry().iter().map(|n| n.profile(&conn)).collect()
        };
        let shared = Arc::new(ReplayDataShared::default());
        let t0 = std::time::Instant::now();
        let req0 = crate::alpaca::ratelimit::requests();
        let report = |label: &str| {
            let ds = day_status(&out, &day, &profiles);
            println!(
                "[{:>5.1}s · {} req] {label}: {:?} launchable={} hits={} prêts={} jusqu'à {} · {}",
                t0.elapsed().as_secs_f32(),
                crate::alpaca::ratelimit::requests() - req0,
                ds.state,
                ds.launchable,
                ds.opportunities.len(),
                ds.ready_hits,
                ds.ready_until_ms.map(et_hm).unwrap_or_else(|| "la fin".into()),
                shared.status.read().unwrap().message.clone().unwrap_or_default(),
            );
            ds
        };
        enqueue(&shared, deps.clone(), vec![day.clone()], vec![], false, true);
        // Phase 1: until the session can start.
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            if day_status(&out, &day, &profiles).launchable || !shared.is_running() {
                break;
            }
        }
        let first = report("lançable");
        // An interruption, then the resume: nothing found so far is lost.
        shared.request_cancel();
        while shared.is_running() {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        let stopped = report("arrêté");
        assert!(stopped.opportunities.len() >= first.opportunities.len());
        enqueue(&shared, deps.clone(), vec![day.clone()], vec![], false, false);
        let mut last = t0.elapsed().as_secs();
        while shared.is_running() {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            if t0.elapsed().as_secs() >= last + 30 {
                last = t0.elapsed().as_secs();
                report("en cours");
            }
        }
        let done = report("terminé");
        println!("statut : {:?}", shared.status.read().unwrap());
        for o in &done.opportunities {
            println!("   {} {} {}", et_hm(o.alert_ms), o.symbol, o.reason);
        }
        assert_eq!(done.state, DayState::Ready);
    }

    #[test]
    fn day_bounds_are_the_extended_session() {
        let (s, e) = day_bounds("2026-06-05").unwrap();
        assert_eq!(e - s, 16 * 3_600_000);
        assert_eq!(
            DateTime::from_timestamp_millis(s).unwrap(),
            Utc.with_ymd_and_hms(2026, 6, 5, 8, 0, 0).unwrap()
        );
    }
}
