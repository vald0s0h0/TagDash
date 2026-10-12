// The attention map's background loop. It only runs while the detached window is
// enabled — no window, no request.
//
// Each tick does AT MOST ONE piece of network work, in this order, then publishes:
//
//   1. the previous session's raw closes (the Y reference);
//   2. the splits effective on the target day;
//   3. the frame's own slots, one part at a time — a poll re-reads the two
//      trailing slots, which is how corrections and cancels reach the map, and a
//      part is read once more in full 20 minutes after it ends, then archived;
//   4. one missing session of history for the 60-session medians.
//
// LIVE follows the provider's horizon (now, or now − 16 min on the Basic plan).
// A MARKET REPLAY downloads the replayed day's frame once — from the cache when it
// was already drawn — and then makes no request at all: the screen is cut at the
// simulated clock by `view::Snapshot::cut_at`, so the replay keeps its pace and
// never sees its own future.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use chrono::{DateTime, NaiveDate, Utc};

use super::calendar;
use super::fetch::{self, Creds};
use super::frame::{Frame, Part, Slot, N_SLOTS};
use super::options::AttentionOptions;
use super::store::{self, ArchivePart, FramePayload};
use super::view::{self, Baselines, Cut, Meta, Snapshot, Ticker, TARGET_HISTORY};
use super::AttentionShared;
use crate::alpaca::plan::Plan;
use crate::config::secrets::Secrets;
use crate::config::AppConfig;

/// A part is read again in full this long after it ends, then archived: late
/// prints and corrections have landed by then.
const SETTLE_SECS: i64 = 20 * 60;
/// The daily bar of a session is trusted as final this long after its close.
const CLOSE_FINAL_SECS: i64 = 30 * 60;
/// Trailing slots re-read on every poll.
const REREAD_SECS: i64 = 600;
/// Sessions of history a replayed day may download on its own. A recent day finds
/// most of its 60 in the archive already; an old one settles for fewer rather than
/// spending a thousand requests on one replay.
const REPLAY_HISTORY_FETCH: usize = 20;
/// Medians are recomputed every this many newly archived session parts.
const REBUILD_EVERY: usize = 6;
const META_REFRESH: Duration = Duration::from_secs(30 * 60);
const MAX_PREV_STEPS: u8 = 5;

pub struct Deps {
    pub shared: Arc<AttentionShared>,
    pub db: Arc<Mutex<rusqlite::Connection>>,
    pub secrets: Arc<RwLock<Secrets>>,
    pub config: Arc<RwLock<AppConfig>>,
    /// Cap on history session parts fetched per frame, whatever the mode. None in
    /// the app; the end-to-end harness sets it to stay within a few requests.
    pub history_cap: Option<usize>,
}

struct State {
    /// What the clock asked for; `frame` may point one session further back.
    nominal: Frame,
    frame: Frame,
    replay: bool,
    series: HashMap<String, Vec<Slot>>,
    refs: HashMap<String, f64>,
    refs_final: bool,
    splits: HashMap<String, f64>,
    /// 0 = never asked, 1 = asked before the premarket, 2 = asked since.
    splits_stage: u8,
    seen: [i64; 2],
    settled: [bool; 2],
    fetched_s: i64,
    prev_steps: u8,

    universe: Vec<String>,
    metas: HashMap<String, Meta>,
    float_date: Option<String>,
    metas_at: Instant,
    opts: AttentionOptions,

    baselines: Baselines,
    /// Sessions each symbol existed in over the history, per part.
    listed: [Option<HashMap<String, u16>>; 2],
    history_fetched: usize,
    history_pending: usize,
    history_exhausted: bool,

    dirty: bool,
    /// What the journal copy was last written from: data reached, history depth,
    /// tickers kept. It is rewritten when any of them moves.
    saved: (i64, [u16; 2], usize),
}

pub fn start(deps: Deps) {
    tauri::async_runtime::spawn(async move {
        let mut st: Option<State> = None;
        let mut pruned: Option<String> = None;
        loop {
            if !deps.shared.is_enabled() {
                if let Some(s) = st.take() {
                    save_snapshot(&deps, &s);
                }
                *deps.shared.current.write().unwrap() = None;
                set_status(&deps, "off", None, None);
                tokio::select! {
                    _ = deps.shared.wake.notified() => {}
                    _ = tokio::time::sleep(Duration::from_secs(2)) => {}
                }
                continue;
            }
            // No watch on the replay generation: a seek inside a replayed day needs
            // nothing from here — the snapshot holds the whole frame and the view is
            // cut at the simulated clock. Only a new frame or a change of mode
            // reopens the state (see `tick`).

            let today = crate::time::et_date(Utc::now());
            if pruned.as_deref() != Some(today.as_str()) {
                store::prune(&deps.db, &today);
                pruned = Some(today);
            }

            let wait_ms = tick(&deps, &mut st).await;
            tokio::select! {
                _ = deps.shared.wake.notified() => {}
                _ = crate::replay::clock::scaled_sleep(wait_ms) => {}
            }
        }
    });
}

fn set_status(deps: &Deps, state: &str, message: Option<String>, st: Option<&State>) {
    let mut status = deps.shared.status.write().unwrap();
    status.state = state.to_string();
    status.message = message;
    if let Some(s) = st {
        status.history_done = s.baselines.sessions[0].min(s.baselines.sessions[1]) as u32;
        status.history_target = TARGET_HISTORY as u32;
        status.history_building = s.history_pending > 0 && !s.history_exhausted;
    }
}

/// One step of work. Returns how long to wait before the next, in market
/// milliseconds.
async fn tick(deps: &Deps, st: &mut Option<State>) -> u64 {
    let now = crate::time::now();
    let replay = crate::replay::clock::is_active();
    let nominal = Frame::at(now);
    let opts = *deps.shared.options.read().unwrap();

    if st.as_ref().is_none_or(|s| s.nominal != nominal || s.replay != replay) {
        if let Some(old) = st.take() {
            save_snapshot(deps, &old);
        }
        let fresh = State::open(deps, nominal, replay, opts).await;
        *st = Some(fresh);
    }
    let s = st.as_mut().expect("state just ensured");

    if s.opts != opts {
        let regroup = s.opts.small_cap_max != opts.small_cap_max;
        s.opts = opts;
        s.dirty = true;
        if regroup {
            s.load_universe(deps);
        }
    }
    if s.metas_at.elapsed() >= META_REFRESH || s.universe.is_empty() {
        s.load_universe(deps);
    }
    if s.universe.is_empty() {
        // First launch: the startup pipeline has not built the universe yet.
        set_status(
            deps,
            "loading",
            Some("Univers en cours de construction (pipeline de démarrage)".into()),
            Some(&*s),
        );
        return 5_000;
    }

    let offline = deps.config.read().unwrap().data_source.offline;
    let creds = fetch::creds(&deps.secrets);
    let Some(creds) = creds.filter(|_| !offline) else {
        // No provider: draw what the cache holds and say why it will not move.
        s.publish(deps);
        let (state, msg) = if offline {
            ("offline", "Mode hors ligne — dernières données en cache")
        } else {
            ("no_creds", "Clés Alpaca absentes — Réglages ▸ API Keys")
        };
        set_status(deps, state, Some(msg.into()), Some(&*s));
        return 5_000;
    };
    let plan = fetch::plan(&deps.db);
    let provider = fetch::provider_horizon(plan);

    match s.work(deps, &creds, plan, provider, now).await {
        Ok(Work::Fetched(msg)) => {
            s.publish(deps);
            let ready = deps.shared.current.read().unwrap().is_some() && !s.refs.is_empty();
            set_status(deps, if ready { "ready" } else { "loading" }, Some(msg), Some(&*s));
            // More may be due right away (the other part, the next session).
            if plan == Plan::Sip {
                150
            } else {
                1_200
            }
        }
        Ok(Work::Waiting(msg)) => {
            s.publish(deps);
            set_status(deps, "loading", Some(msg), Some(&*s));
            5_000
        }
        Ok(Work::Idle) => {
            s.publish(deps);
            set_status(deps, "ready", None, Some(&*s));
            5_000
        }
        Err(e) => {
            eprintln!("[tagdash] attention_map: {e}");
            s.publish(deps);
            set_status(deps, "error", Some(e), Some(&*s));
            15_000
        }
    }
}

enum Work {
    /// Something was read; call again soon.
    Fetched(String),
    /// Nothing can be read yet (the close has not printed, the session has not begun).
    Waiting(String),
    Idle,
}

fn load_listed(
    db: &Arc<Mutex<rusqlite::Connection>>,
    days: &[String],
) -> Option<HashMap<String, u16>> {
    if days.is_empty() {
        return None;
    }
    let list = days
        .iter()
        .map(|d| format!("'{d}'"))
        .collect::<Vec<_>>()
        .join(",");
    let conn = db.lock().unwrap();
    let map: HashMap<String, u16> = conn
        .prepare(&format!(
            "SELECT symbol, COUNT(*) FROM daily_cache WHERE date IN ({list}) GROUP BY symbol"
        ))
        .and_then(|mut stmt| {
            stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u16)))
                .map(|it| it.filter_map(|r| r.ok()).collect())
        })
        .unwrap_or_default();
    // A daily cache that does not cover these days says nothing about who was
    // listed: assume everyone was, rather than reading every name as brand new.
    (map.len() >= 1_000).then_some(map)
}

/// A symbol whose suffix says preferred share, warrant, unit or right. Only
/// consulted when the security type was never collected: those would otherwise sit
/// among the stocks as "capitalisation inconnue" (measured: BA.PRA, ALB.PRA).
fn looks_non_common(symbol: &str) -> bool {
    let Some((_, suffix)) = symbol.split_once('.') else {
        return false;
    };
    suffix.starts_with("PR") || matches!(suffix, "WS" | "W" | "U" | "R" | "RT")
}

/// Split label as the rest of the app writes it: "x4" forward, "1:10" reverse.
fn split_label(factor: f64) -> String {
    let trim = |v: f64| {
        if (v - v.round()).abs() < 1e-6 {
            format!("{}", v.round() as i64)
        } else {
            format!("{v:.2}")
        }
    };
    if factor >= 1.0 {
        format!("x{}", trim(factor))
    } else {
        format!("1:{}", trim(1.0 / factor))
    }
}

impl State {
    async fn open(deps: &Deps, nominal: Frame, replay: bool, opts: AttentionOptions) -> State {
        let mut s = State {
            nominal,
            frame: nominal,
            replay,
            series: HashMap::new(),
            refs: HashMap::new(),
            refs_final: false,
            splits: HashMap::new(),
            splits_stage: 0,
            seen: [0, 0],
            settled: [false, false],
            fetched_s: 0,
            prev_steps: 0,
            universe: Vec::new(),
            metas: HashMap::new(),
            float_date: None,
            metas_at: Instant::now(),
            opts,
            baselines: Baselines::default(),
            listed: [None, None],
            history_fetched: 0,
            history_pending: 0,
            history_exhausted: false,
            dirty: true,
            saved: (0, [0, 0], 0),
        };
        s.load_universe(deps);
        let day = nominal.day.to_string();
        let db = deps.db.clone();
        let cached = tokio::task::spawn_blocking(move || store::get_frame(&db, &day))
            .await
            .ok()
            .flatten();
        if let Some(c) = cached.filter(|c| c.frame.day == nominal.day) {
            s.frame = c.frame;
            s.refs = c.refs;
            s.refs_final = c.refs_final;
            s.splits = c.splits;
            s.splits_stage = c.splits_stage;
            s.seen = [c.ah_seen_s, c.pm_seen_s];
            s.settled = [c.ah_settled, c.pm_settled];
            s.fetched_s = c.fetched_s;
            s.series = c.series.into_iter().collect();
        }
        s.rebuild_baselines(deps).await;
        s
    }

    fn load_universe(&mut self, deps: &Deps) {
        let conn = deps.db.lock().unwrap();
        let assets = crate::local_db::universe_repository::get_all(&conn).unwrap_or_default();
        self.float_date =
            crate::local_db::cache_repository::get_app_meta(&conn, "floats_fetch_date");
        drop(conn);
        self.metas.clear();
        self.universe.clear();
        for a in assets {
            if !a.tradable {
                continue;
            }
            if a.security_type.is_none() && looks_non_common(&a.symbol) {
                continue;
            }
            let cap = a.market_cap.map(|c| c as f64);
            let Some(group) =
                view::group_of(a.security_type.as_deref(), cap, self.opts.small_cap_max)
            else {
                continue;
            };
            self.universe.push(a.symbol.clone());
            self.metas.insert(
                a.symbol,
                Meta {
                    name: a.name,
                    group,
                    cap: cap.filter(|c| *c > 0.0),
                    float: a.float_shares.filter(|f| *f > 0).map(|f| f as f64),
                },
            );
        }
        self.metas_at = Instant::now();
        self.dirty = true;
    }

    fn payload(&self) -> FramePayload {
        FramePayload {
            frame: self.frame,
            refs: self.refs.clone(),
            refs_final: self.refs_final,
            splits: self.splits.clone(),
            splits_stage: self.splits_stage,
            series: self.series.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            ah_seen_s: self.seen[0],
            pm_seen_s: self.seen[1],
            ah_settled: self.settled[0],
            pm_settled: self.settled[1],
            fetched_s: self.fetched_s,
        }
    }

    async fn save_frame(&self, deps: &Deps) {
        let (db, payload) = (deps.db.clone(), self.payload());
        let _ = tokio::task::spawn_blocking(move || store::put_frame(&db, &payload)).await;
    }

    /// The (day, part) history sessions behind this frame's medians, most recent
    /// first: after-hours of the sessions before the previous one, premarkets of
    /// the sessions before the target day.
    fn history_days(&self) -> [Vec<String>; 2] {
        let days = |d: NaiveDate| -> Vec<String> {
            calendar::sessions_before(d, TARGET_HISTORY)
                .into_iter()
                .map(|x| x.to_string())
                .collect()
        };
        [days(self.frame.prev_day), days(self.frame.day)]
    }

    async fn rebuild_baselines(&mut self, deps: &Deps) {
        let days = self.history_days();
        let (db, metas, opts) = (deps.db.clone(), self.metas.clone(), self.opts);
        let built = tokio::task::spawn_blocking(move || {
            let mut parts: [Vec<ArchivePart>; 2] = [Vec::new(), Vec::new()];
            let mut listed: [Option<HashMap<String, u16>>; 2] = [None, None];
            let mut pending = 0usize;
            for (pi, part) in [Part::Ah, Part::Pm].into_iter().enumerate() {
                let known = store::archived_days(&db, part, &days[pi]);
                pending += days[pi].iter().filter(|d| !known.contains_key(*d)).count();
                let mut used: Vec<String> = Vec::new();
                for day in &days[pi] {
                    if known.get(day).copied().unwrap_or(0) <= 0 {
                        continue;
                    }
                    if let Some(a) = store::get_archive(&db, day, part) {
                        parts[pi].push(a);
                        used.push(day.clone());
                    }
                }
                listed[pi] = load_listed(&db, &used);
            }
            let [la, lp] = &listed;
            let get = |m: &Option<HashMap<String, u16>>, s: &str| {
                m.as_ref().map(|m| m.get(s).copied().unwrap_or(0))
            };
            let baselines = view::build_baselines(
                &parts[0],
                &parts[1],
                &|s| get(la, s),
                &|s| get(lp, s),
                &metas,
                &opts,
            );
            (baselines, listed, pending)
        })
        .await;
        if let Ok((baselines, listed, pending)) = built {
            self.baselines = baselines;
            self.listed = listed;
            self.history_pending = pending;
            self.dirty = true;
        }
    }

    async fn work(
        &mut self,
        deps: &Deps,
        creds: &Creds,
        plan: Plan,
        provider: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<Work, String> {
        let ah_start = self.frame.part_start(Part::Ah);
        let since_close = (provider - ah_start).num_seconds();

        // ── 1. Reference closes ──────────────────────────────────────────────
        if self.refs.is_empty() || (!self.refs_final && since_close >= CLOSE_FINAL_SECS) {
            if since_close < 180 {
                return Ok(Work::Waiting(format!(
                    "En attente de la clôture du {}",
                    self.frame.prev_day
                )));
            }
            let closes =
                fetch::daily_closes(creds, &self.universe, self.frame.prev_day, provider).await?;
            if closes.len() < 50 {
                if since_close < CLOSE_FINAL_SECS {
                    return Ok(Work::Waiting("Clôture officielle pas encore publiée".into()));
                }
                // No daily bar anywhere: that day was not a session after all.
                if self.prev_steps >= MAX_PREV_STEPS {
                    return Err(format!("aucune séance trouvée avant le {}", self.frame.day));
                }
                self.prev_steps += 1;
                self.frame = self.frame.step_prev_back();
                self.series.clear();
                self.seen = [0, 0];
                self.settled = [false, false];
                self.rebuild_baselines(deps).await;
                return Ok(Work::Fetched(format!(
                    "Séance précédente : {}",
                    self.frame.prev_day
                )));
            }
            self.refs = closes;
            self.refs_final = since_close >= CLOSE_FINAL_SECS;
            self.dirty = true;
            self.save_frame(deps).await;
            return Ok(Work::Fetched("Clôtures de référence".into()));
        }

        // ── 2. Splits effective on the target day ────────────────────────────
        let stage = if provider >= self.frame.part_start(Part::Pm) { 2 } else { 1 };
        if self.splits_stage < stage {
            let day = self.frame.day.to_string();
            let events = match crate::alpaca::corporate_actions::fetch_splits_on_date(
                &creds.key,
                &creds.secret,
                &day,
            )
            .await
            {
                Ok(ev) => ev.into_iter().map(|e| (e.symbol, e.from, e.to)).collect(),
                // The startup pipeline keeps its own table of them — better than none.
                Err(_) => {
                    let conn = deps.db.lock().unwrap();
                    conn.prepare(
                        "SELECT symbol, from_factor, to_factor FROM ticker_splits WHERE ex_date = ?1",
                    )
                    .and_then(|mut stmt| {
                        stmt.query_map([&day], |r| {
                            Ok((
                                r.get::<_, String>(0)?,
                                r.get::<_, Option<f64>>(1)?.unwrap_or(1.0),
                                r.get::<_, Option<f64>>(2)?.unwrap_or(1.0),
                            ))
                        })
                        .map(|it| it.filter_map(|r| r.ok()).collect::<Vec<_>>())
                    })
                    .unwrap_or_default()
                }
            };
            self.splits = events
                .into_iter()
                .filter(|(_, from, to)| *from > 0.0 && *to > 0.0 && (to / from - 1.0).abs() > 1e-9)
                .map(|(sym, from, to)| (sym, to / from))
                .collect();
            self.splits_stage = stage;
            self.dirty = true;
            self.save_frame(deps).await;
            return Ok(Work::Fetched("Splits du jour".into()));
        }

        // ── 3. The frame's own slots ─────────────────────────────────────────
        let step = if plan == Plan::Sip { 60 } else { super::frame::SLOT_SECS };
        for (pi, part) in [Part::Ah, Part::Pm].into_iter().enumerate() {
            let (start, end) = (self.frame.part_start(part), self.frame.part_end(part));
            // A replay takes the whole day at once; the screen is cut at the
            // simulated clock afterwards.
            let reach = if self.replay { provider } else { provider.min(now) };
            let goal = reach.min(end);
            if goal <= start {
                continue;
            }
            let (goal_s, end_s) = (goal.timestamp(), end.timestamp());
            let seen = self.seen[pi];
            let settle_due = !self.settled[pi]
                && seen >= end_s
                && (provider - end).num_seconds() >= SETTLE_SECS;
            let poll_due = seen < goal_s && (seen == 0 || goal_s - seen >= step || goal_s == end_s);
            if !settle_due && !poll_due {
                continue;
            }
            let from = if settle_due || seen == 0 {
                start
            } else {
                DateTime::from_timestamp(seen - REREAD_SECS, 0).unwrap_or(start).max(start)
            };
            let fresh =
                fetch::part_slots(creds, &self.universe, &self.frame, part, from, goal, Duration::ZERO)
                    .await?;
            // What is re-read replaces what was held, slot for slot — including a
            // slot whose every print was cancelled since, which simply is no more.
            let lo = self.frame.slot_of(from).unwrap_or(part.offset()) as u8;
            let hi = (part.offset() + part.len()) as u8;
            for slots in self.series.values_mut() {
                slots.retain(|x| x.i < lo || x.i >= hi);
            }
            self.series.retain(|_, v| !v.is_empty());
            fetch::merge(&mut self.series, fresh);
            self.seen[pi] = goal_s;
            self.fetched_s = Utc::now().timestamp();
            self.dirty = true;

            let complete = from == start && goal_s == end_s;
            if complete && (provider - end).num_seconds() >= SETTLE_SECS {
                self.settled[pi] = true;
                let session = match part {
                    Part::Ah => self.frame.prev_day,
                    Part::Pm => self.frame.day,
                };
                let data = store::archive_of(&self.series, part);
                let (db, day) = (deps.db.clone(), session.to_string());
                let _ = tokio::task::spawn_blocking(move || store::put_archive(&db, &day, part, &data))
                    .await;
            }
            self.save_frame(deps).await;
            let label = match part {
                Part::Ah => format!("After-hours du {}", self.frame.prev_day),
                Part::Pm => format!("Pré-market du {}", self.frame.day),
            };
            return Ok(Work::Fetched(label));
        }

        // ── 4. One missing session of history ────────────────────────────────
        let cap = deps
            .history_cap
            .or(self.replay.then_some(REPLAY_HISTORY_FETCH))
            .unwrap_or(usize::MAX);
        if !self.history_exhausted {
            let next = if self.history_fetched < cap {
                self.next_missing_history(deps, provider).await
            } else {
                None
            };
            if let Some((session, part)) = next {
                let frame = match part {
                    Part::Ah => Frame::between(session, calendar::next_trading_day(session)),
                    Part::Pm => Frame::for_day(session),
                };
                // Paced on the Basic plan: this is background work and the same
                // budget serves the charts.
                let pace = if plan == Plan::Sip { Duration::ZERO } else { Duration::from_millis(350) };
                let (start, end) = (frame.part_start(part), frame.part_end(part));
                let slots =
                    fetch::part_slots(creds, &self.universe, &frame, part, start, end, pace).await?;
                let data = store::archive_of(&slots, part);
                let (db, day) = (deps.db.clone(), session.to_string());
                tokio::task::spawn_blocking(move || store::put_archive(&db, &day, part, &data))
                    .await
                    .map_err(|e| e.to_string())??;
                self.history_fetched += 1;
                self.history_pending = self.history_pending.saturating_sub(1);
                if self.history_fetched % REBUILD_EVERY == 0 || self.history_pending == 0 {
                    self.rebuild_baselines(deps).await;
                }
                return Ok(Work::Fetched(format!(
                    "Historique {session} ({})",
                    if part == Part::Ah { "after-hours" } else { "pré-market" }
                )));
            }
            // Nothing left to fetch, or this frame's allowance is spent: fold in
            // whatever the last partial batch brought.
            self.history_exhausted = true;
            if self.history_fetched % REBUILD_EVERY != 0 {
                self.rebuild_baselines(deps).await;
            }
        }
        Ok(Work::Idle)
    }

    /// The most recent history session part not yet archived, whose data the
    /// provider can already serve in full.
    async fn next_missing_history(
        &self,
        deps: &Deps,
        provider: DateTime<Utc>,
    ) -> Option<(NaiveDate, Part)> {
        let days = self.history_days();
        let db = deps.db.clone();
        tokio::task::spawn_blocking(move || {
            let known = [
                store::archived_days(&db, Part::Ah, &days[0]),
                store::archived_days(&db, Part::Pm, &days[1]),
            ];
            // Most recent first, the two parts abreast: the first sessions fetched
            // are the ones that matter most to a median.
            for k in 0..TARGET_HISTORY {
                for part in [Part::Pm, Part::Ah] {
                    let pi = part as usize;
                    let Some(day) = days[pi].get(k) else { continue };
                    if known[pi].contains_key(day) {
                        continue;
                    }
                    let Ok(d) = NaiveDate::parse_from_str(day, "%Y-%m-%d") else { continue };
                    let frame = match part {
                        Part::Ah => Frame::between(d, calendar::next_trading_day(d)),
                        Part::Pm => Frame::for_day(d),
                    };
                    if (provider - frame.part_end(part)).num_seconds() >= SETTLE_SECS {
                        return Some((d, part));
                    }
                }
            }
            None
        })
        .await
        .ok()
        .flatten()
    }

    fn snapshot(&self, plan_delay: u32) -> Snapshot {
        let gate = self.opts.min_dollars_5m as f32;
        let mut no_ref = 0u32;
        let mut tickers: Vec<Ticker> = Vec::new();
        for (sym, slots) in &self.series {
            let Some(meta) = self.metas.get(sym) else { continue };
            // Cheap first cut: nothing here ever carried enough to matter.
            let (mut peak, mut total) = (0.0f32, 0.0f32);
            for s in slots {
                peak = peak.max(s.d);
                total += s.d;
            }
            if peak < gate * 0.5 && total < gate * 2.0 {
                continue;
            }
            let Some(&ref_close) = self.refs.get(sym) else {
                no_ref += 1;
                continue;
            };
            let factor = self.splits.get(sym).copied().unwrap_or(1.0);
            let base_n = [0, 1].map(|pi| {
                self.listed[pi]
                    .as_ref()
                    .map(|m| m.get(sym).copied().unwrap_or(0))
                    .unwrap_or(self.baselines.sessions[pi])
            });
            let t = Ticker {
                s: sym.clone(),
                meta: meta.clone(),
                ref_close,
                factor,
                split: self.splits.get(sym).map(|f| split_label(*f)),
                slots: slots.clone(),
                base: self
                    .baselines
                    .med
                    .get(sym)
                    .map(|m| slots.iter().map(|x| m.get(x.i as usize).copied().unwrap_or(0.0)).collect())
                    .unwrap_or_default(),
                base_n,
            };
            // Kept if it belongs on the map at ANY cut of the frame; the view then
            // decides, slot by slot, from when.
            if view::eval_ticker(&t, &Cut::all(), &self.opts).is_some() {
                tickers.push(t);
            }
        }
        tickers.sort_by(|a, b| a.s.cmp(&b.s));
        let weather_hist = if self.baselines.weather.len() == N_SLOTS {
            self.baselines.weather.clone()
        } else {
            vec![Default::default(); N_SLOTS]
        };
        Snapshot {
            frame: self.frame,
            ah_seen_s: self.seen[0],
            pm_seen_s: self.seen[1],
            fetched_s: self.fetched_s,
            delayed_min: plan_delay,
            float_date: self.float_date.clone(),
            tickers,
            weather: view::frame_weather(&self.series, &self.metas, &self.opts),
            weather_hist,
            hist_sessions: self.baselines.sessions,
            universe: self.universe.len() as u32,
            no_ref,
        }
    }

    fn publish(&mut self, deps: &Deps) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        let delay = match fetch::plan(&deps.db) {
            Plan::Sip => 0,
            Plan::Iex => fetch::BASIC_DELAY_MIN as u32,
        };
        let snap = self.snapshot(delay);
        // The journal copy follows the data and the history behind it — a median
        // built on three sessions must not be what a review reads once sixty are in.
        let key = (snap.seen_s(), snap.hist_sessions, snap.tickers.len());
        if key != self.saved && !snap.tickers.is_empty() {
            self.saved = key;
            let (db, copy) = (deps.db.clone(), snap.clone());
            let _ = tauri::async_runtime::spawn_blocking(move || {
                if let Err(e) = store::put_snapshot(&db, &copy) {
                    eprintln!("[tagdash] attention_map: snapshot non enregistré ({e})");
                }
            });
        }
        deps.shared.publish(snap);
    }
}

fn save_snapshot(deps: &Deps, s: &State) {
    let delay = match fetch::plan(&deps.db) {
        Plan::Sip => 0,
        Plan::Iex => fetch::BASIC_DELAY_MIN as u32,
    };
    let snap = s.snapshot(delay);
    if snap.tickers.is_empty() {
        return;
    }
    let db = deps.db.clone();
    let _ = tauri::async_runtime::spawn_blocking(move || {
        let _ = store::put_snapshot(&db, &snap);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// End-to-end against the real provider, on the real universe, into a throwaway
    /// database: closes → splits → frame → a few sessions of history → the view.
    ///
    ///   cargo test --lib attention_e2e -- --ignored --nocapture
    ///
    /// Reads the app's own keys and universe (read-only); writes nothing there.
    /// `TAGDASH_E2E_HISTORY` = session parts of history to fetch (default 6).
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn attention_e2e() {
        let (app_dir, cfg) = crate::config::load();
        let mut secrets = crate::config::secrets::load(&app_dir);
        secrets.resolve_market_data(cfg.broker.is_live());

        let mem = rusqlite::Connection::open_in_memory().unwrap();
        crate::local_db::schema::migrate(&mem).unwrap();
        {
            let real = rusqlite::Connection::open_with_flags(
                crate::paths::cache_db(&app_dir),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .expect("base TagDash introuvable");
            for a in crate::local_db::universe_repository::get_all(&real).unwrap() {
                crate::local_db::universe_repository::upsert(&mem, &a).unwrap();
            }
            for key in [crate::alpaca::plan::PLAN_META_KEY, "floats_fetch_date"] {
                if let Some(v) = crate::local_db::cache_repository::get_app_meta(&real, key) {
                    crate::local_db::cache_repository::set_app_meta(&mem, key, &v).unwrap();
                }
            }
        }
        let cap = std::env::var("TAGDASH_E2E_HISTORY")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(6);
        let deps = Deps {
            shared: Arc::new(AttentionShared::new(AttentionOptions::default(), true)),
            db: Arc::new(Mutex::new(mem)),
            secrets: Arc::new(RwLock::new(secrets)),
            config: Arc::new(RwLock::new(cfg)),
            history_cap: Some(cap),
        };

        let before = crate::alpaca::ratelimit::requests();
        let started = Instant::now();
        let mut st: Option<State> = None;
        for step in 0..200 {
            tick(&deps, &mut st).await;
            let status = deps.shared.status.read().unwrap().clone();
            println!(
                "[{step:>3}] {:<8} {} — historique {}/{}",
                status.state,
                status.message.clone().unwrap_or_default(),
                status.history_done,
                status.history_target,
            );
            assert_ne!(status.state, "error", "{:?}", status.message);
            if status.state == "ready" && status.message.is_none() {
                break;
            }
        }
        let snap = deps.shared.current.read().unwrap().clone().expect("rien de publié");
        let view = view::build_view(&snap, &snap.live_cut(), &AttentionOptions::default());
        println!(
            "\ncadre {} ← {} · {} requêtes · {:.0}s · univers {} · sans clôture {} · \
             carte {} tickers · historique {:?} · créneaux clos {}",
            view.frame.day,
            view.frame.prev_day,
            crate::alpaca::ratelimit::requests() - before,
            started.elapsed().as_secs_f32(),
            view.universe,
            view.no_ref,
            view.tickers.len(),
            view.hist_sessions,
            view.closed,
        );
        println!("météo {:?}", view.weather);
        for t in view.tickers.iter().take(25) {
            let part = if t.pm.n > 0 { &t.pm } else { &t.ah };
            println!(
                "{:<6} {:?} {:?} score {:>5.2} · x max {:>8.1} · y {:>+7.1}/{:>+7.1} % · \
                 {:>3} pts · {:>9.0} $ · base {:?}",
                t.s,
                t.group,
                t.pat_all,
                t.score_all,
                part.x_peak.unwrap_or(0.0),
                part.y_max.unwrap_or(0.0),
                part.y_min.unwrap_or(0.0),
                t.pts.len(),
                t.ah.dollars + t.pm.dollars,
                t.base_n,
            );
        }
        // `TAGDASH_E2E_DUMP=<file>`: the view and the leader's candles as the
        // commands would send them — a real fixture to render the screen against.
        if let Ok(path) = std::env::var("TAGDASH_E2E_DUMP") {
            let detail = view
                .tickers
                .first()
                .and_then(|t| view::build_detail(&snap, &t.s, &snap.live_cut()));
            let json = serde_json::json!({ "map": view, "detail": detail });
            std::fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
            println!("vue écrite dans {path}");
        }
        assert!(view.universe > 1_000);
        assert!(!view.tickers.is_empty());
        // The journal copy went through and reads back as what was published.
        tokio::time::sleep(Duration::from_millis(500)).await;
        let stored = store::get_snapshot(&deps.db, &view.frame.day).expect("instantané absent");
        assert_eq!(stored.tickers.len(), snap.tickers.len());
        let bytes: i64 = deps
            .db
            .lock()
            .unwrap()
            .query_row("SELECT SUM(LENGTH(payload)) FROM attention_snapshots", [], |r| r.get(0))
            .unwrap();
        println!("instantané journal : {} tickers, {} ko", stored.tickers.len(), bytes / 1024);
    }

    #[test]
    fn share_classes_are_stocks_but_preferreds_and_warrants_are_not() {
        for s in ["BA.PRA", "ALB.PRA", "XYZ.WS", "ABC.U", "DEF.RT"] {
            assert!(looks_non_common(s), "{s}");
        }
        for s in ["BRK.B", "GEF.B", "AAPL", "BF.A"] {
            assert!(!looks_non_common(s), "{s}");
        }
    }

    #[test]
    fn split_labels_match_the_rest_of_the_app() {
        assert_eq!(split_label(4.0), "x4");
        assert_eq!(split_label(0.1), "1:10");
        assert_eq!(split_label(1.5), "x1.50");
    }
}
