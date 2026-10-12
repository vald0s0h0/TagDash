// Market Replay — rejoue une journée historique à travers toute la plateforme.
//
// SOURCE DE DONNÉES — le jour préparé sur disque par `crate::replay_data`, à partir
// de ce que les stratégies choisies déclarent en avoir besoin : vrais prints et
// quotes SIP autour de leurs opportunités, tranches 10 s synthétisées des barres
// 1 min ailleurs. Le moteur lui-même ne touche jamais au réseau ; un jour non
// préparé est une erreur que la modale résout en le préparant d'abord.
//
// UN JOUR ENCORE EN PRÉPARATION se rejoue dès que ses premières opportunités sont
// sur disque : le reste se télécharge pendant la séance (`replay_data::prepare`), le
// moteur relit le jour à mesure (`absorb`) et ne dépasse jamais l'instant jusqu'où
// tout est connu (`cap`) — arrivé là, il attend la suite.
//
// Quand un replay est actif :
//   • les flux live (WebSocket données + news) sont arrêtés ;
//   • l'horloge de l'app (`crate::time::now()`) devient l'horloge simulée ;
//   • le moteur émet les événements historiques (trades du tape ou tranches 10 s
//     synthétisées des barres 1 min, quotes, news à leur heure de publication)
//     dans le MÊME MarketState que le live — détection, trading interne, journal,
//     screenshots, alarmes… fonctionnent à l'identique, datés du jour simulé ;
//   • les moteurs de stratégie tournent inchangés : ils lisent `time::now()`,
//     dorment via `scaled_sleep` (cadence sim ≈ cadence live malgré
//     l'accélération) et se réinitialisent quand `generation()` change ;
//   • les stratégies pour lesquelles la séance a été préparée tournent même si
//     leur interrupteur live est coupé (`strategies::runs`) ;
//   • chaque appel REST Alpaca hors replay-loader est borné à l'instant simulé
//     (aucune fuite de données du futur — voir `clock::rest_end_clamp`).
//
// À l'arrêt : horloge réelle restaurée, MarketState vidé, watchlist Panic
// purgée (elle datait du jour simulé), flux live relancés.

pub mod clock;
pub mod data;
pub mod tape;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex, RwLock,
};
use std::time::Instant;

use chrono::{DateTime, Duration as ChronoDuration, NaiveDate, Utc};
use serde::Serialize;
use tauri::Emitter;
use tokio::sync::{mpsc, watch};

use crate::config::secrets::Secrets;
use crate::config::AppConfig;
use crate::market_state::MarketState;
use crate::types::AlertSignal;

/// Real interval of the engine tick.
const TICK_MS: u64 = 100;
/// Events emitted per MarketState write-lock acquisition (see `emit_until`).
const EMIT_BATCH: usize = 5_000;
/// « Prochaine alerte » sans opportunité préparée à viser : avance rapide jusqu'à
/// ce qu'une alerte tombe.
const NEXT_ALERT_SPEED: f64 = 120.0;
/// « Prochaine alerte » on a prepared day jumps straight to this long before the
/// next opportunity — no fast-forward through the time in between: the engine arms
/// at most a watch (60 s) before it alerts, and must see that arm happen rather
/// than be dropped past it.
pub(crate) const NEXT_ALERT_LEAD_MS: i64 = 75_000;
/// …and gives it this long after the prepared time to fire before moving on (the
/// engine can alert later than the pre-scan: measured up to +46 s).
pub(crate) const NEXT_ALERT_TAIL_MS: i64 = 90_000;
/// Speed of that last stretch, advanced one market second at a time so the
/// engines see every second of it. Measured on 2026-06-29 (`replay_fidelity`): the
/// engine fires 11 of the 11 prepared opportunities stepping by one second, 9 —
/// late — stepping by the 12 s a ×120 fast-forward covers per tick.
const NEXT_ALERT_APPROACH_SPEED: f64 = 20.0;
/// An alert on the symbol this close to a prepared opportunity IS that opportunity.
const SAME_OPPORTUNITY_MS: i64 = 5 * 60_000;
/// How often the engine looks at the preparation of the day it replays.
const WATCH_EVERY: std::time::Duration = std::time::Duration::from_millis(500);
/// A day still downloading is read again once the replay is this close (real
/// seconds at the current speed) to the end of what it holds…
const RELOAD_RUNWAY_SECS: f64 = 60.0;
/// …and otherwise this often while more of it lands, so what the toolbar shows as
/// replayable — and where a jump may go — is never far behind the download.
const RELOAD_EVERY: std::time::Duration = std::time::Duration::from_secs(20);
/// Min interval between pushed market-tick events per focus symbol (real ms).
const TICK_THROTTLE_MS: u128 = 100;
/// How long a day load waits for the Panic watchlist before reporting "paused".
/// Past this the build keeps running in the background rather than freezing the UI.
const WATCHLIST_BUDGET: tokio::time::Duration = tokio::time::Duration::from_secs(45);

// ─── Status (polled by the frontend toolbar) ────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct ReplayStatus {
    pub active: bool,
    /// idle | loading | playing | paused | ended | error
    pub state: String,
    /// ET date being replayed (YYYY-MM-DD).
    pub day: Option<String>,
    /// "tape" (trades réels enregistrés) | "seconds" (agrégats 1 s réels) |
    /// "minutes" (barres 1 min synthétisées) | "daily" (barres journalières).
    pub source: Option<String>,
    /// D'OÙ viennent les données — "alpaca_sip" | "massive" |
    /// "flat_files" | "daily_cache". Voir `data::Provider`.
    pub provider: Option<String>,
    pub sim_time: Option<DateTime<Utc>>,
    pub speed: f64,
    pub playing: bool,
    /// 0..1 pendant le chargement.
    pub progress: f32,
    pub symbols: usize,
    pub events_total: usize,
    pub events_done: usize,
    pub error: Option<String>,
    pub next_alert_armed: bool,
    /// The strategies the session was prepared for.
    pub strategies: Vec<String>,
    /// On a day still being prepared: how far it can be replayed for now.
    pub ready_until: Option<DateTime<Utc>>,
    /// Standing at `ready_until`, waiting for the download to get further.
    pub buffering: bool,
    /// One line for the toolbar (e.g. no opportunity left today).
    pub notice: Option<String>,
}

impl Default for ReplayStatus {
    fn default() -> Self {
        Self {
            active: false,
            state: "idle".into(),
            day: None,
            source: None,
            provider: None,
            sim_time: None,
            speed: 1.0,
            playing: false,
            progress: 0.0,
            symbols: 0,
            events_total: 0,
            events_done: 0,
            error: None,
            next_alert_armed: false,
            strategies: Vec::new(),
            ready_until: None,
            buffering: false,
            notice: None,
        }
    }
}

/// Shared handle stored in AppState.
pub struct ReplayShared {
    pub status: RwLock<ReplayStatus>,
    pub cmd_tx: Mutex<Option<mpsc::UnboundedSender<ReplayCmd>>>,
}

impl Default for ReplayShared {
    fn default() -> Self {
        Self {
            status: RwLock::new(ReplayStatus::default()),
            cmd_tx: Mutex::new(None),
        }
    }
}

impl ReplayShared {
    pub fn send(&self, cmd: ReplayCmd) -> Result<(), String> {
        match self.cmd_tx.lock().unwrap().as_ref() {
            Some(tx) => tx
                .send(cmd)
                .map_err(|_| "replay engine stopped".to_string()),
            None => Err("aucun replay actif".to_string()),
        }
    }
}

#[derive(Debug)]
pub enum ReplayCmd {
    Play,
    Pause,
    /// Lecture ↔ pause (raccourci clavier / manette : l'appelant ne connaît pas
    /// l'état).
    Toggle,
    SetSpeed(f64),
    /// Avance/recule de N secondes simulées (recul = reset + re-feed rapide).
    SeekRelative(i64),
    /// Saute à HH:MM (ET) du jour rejoué.
    SeekClock {
        minutes: u32,
    },
    /// Passe à la prochaine opportunité préparée et laisse la lecture tourner une
    /// fois l'alerte tombée.
    NextAlert,
    /// Avance jusqu'au prochain close de barre 1 min, puis pause.
    NextBar,
    /// Charge la prochaine séance (jour ouvré suivant) à l'heure de départ.
    NextDay,
    Stop,
}

/// Everything the engine task needs (cloned Arcs from AppState + Tauri handle).
pub struct ReplayDeps {
    pub app_dir: PathBuf,
    pub market: Arc<RwLock<MarketState>>,
    pub db: Arc<Mutex<rusqlite::Connection>>,
    pub config: Arc<RwLock<AppConfig>>,
    pub secrets: Arc<RwLock<Secrets>>,
    pub internal_book: Arc<RwLock<crate::internal_trading::InternalBook>>,
    pub broker_mirror: Arc<RwLock<crate::broker::BrokerMirror>>,
    pub strategy_enabled: Arc<RwLock<std::collections::HashMap<String, bool>>>,
    pub chart: Arc<RwLock<crate::chart_state::ChartState>>,
    pub live_feed_running: Arc<AtomicBool>,
    pub news_feed_running: Arc<AtomicBool>,
    /// Receiver used to emit market-tick events for displayed symbols.
    pub focus_rx: watch::Receiver<Vec<String>>,
    /// Spare receiver handed back to the live feed when replay stops.
    pub focus_rx_restart: watch::Receiver<Vec<String>>,
    pub active_alerts: Arc<RwLock<Vec<AlertSignal>>>,
    pub alert_history: Arc<RwLock<Vec<AlertSignal>>>,
    /// The preparation queue: a day replayed before it is finished keeps
    /// downloading there.
    pub replay_data: Arc<crate::replay_data::prepare::ReplayDataShared>,
    pub app: tauri::AppHandle,
}

#[derive(Clone, Serialize)]
struct TickEvent {
    symbol: String,
    price: f64,
    ts: i64,
}

// ─── The day as prepared so far ─────────────────────────────────────────────────

/// What the engine needs to follow a day's preparation, off the engine task too.
#[derive(Clone)]
struct PrepLink {
    app_dir: PathBuf,
    db: Arc<Mutex<rusqlite::Connection>>,
    config: Arc<RwLock<AppConfig>>,
    secrets: Arc<RwLock<Secrets>>,
    replay_data: Arc<crate::replay_data::prepare::ReplayDataShared>,
}

impl PrepLink {
    fn of(deps: &ReplayDeps) -> Self {
        Self {
            app_dir: deps.app_dir.clone(),
            db: deps.db.clone(),
            config: deps.config.clone(),
            secrets: deps.secrets.clone(),
            replay_data: deps.replay_data.clone(),
        }
    }
}

struct Progress {
    /// How far the day can be replayed while its preparation runs (UTC millis).
    /// None = no limit: the day is ready, or nothing more is coming.
    cap: Option<i64>,
    /// The session's prepared opportunities, time-ordered: (alert time, symbol).
    hits: Vec<(i64, String)>,
}

/// How far `day` is prepared for the session's strategies. With `resume` (a day
/// being loaded), one whose preparation was left unfinished goes back in the queue,
/// ahead of everything else, so the rest downloads while the session runs — once:
/// a preparation that then fails is not asked for again behind the trader's back.
fn day_progress(link: &PrepLink, day: &str, ids: &[String], resume: bool) -> Progress {
    use crate::replay_data::{plan::DayState, prepare};
    let profiles: Vec<crate::replay_data::DataProfile> = {
        let conn = link.db.lock().unwrap();
        crate::replay_data::needs_registry()
            .into_iter()
            .filter(|n| ids.is_empty() || ids.iter().any(|i| i == n.id()))
            .map(|n| n.profile(&conn))
            .collect()
    };
    let status = prepare::day_status(&link.app_dir, day, &profiles);
    let complete = status.state == DayState::Ready;
    // Only what was prepared under the current settings is resumed: a scan the
    // settings have since left behind is redone from the picker, not under a
    // session already reading it.
    let resumable = resume && status.strategies.iter().all(|s| s.scanned || s.partial);
    if !complete && resumable && !link.replay_data.pending(day) && !link.config.read().unwrap().data_source.offline {
        if let Some(creds) = crate::review::sip::creds(&link.secrets.read().unwrap()) {
            let deps = prepare::PrepareDeps {
                app_dir: link.app_dir.clone(),
                db: link.db.clone(),
                creds,
            };
            prepare::enqueue(&link.replay_data, deps, vec![day.to_string()], ids.to_vec(), false, true);
        }
    }
    Progress {
        cap: (!complete && link.replay_data.pending(day))
            .then_some(status.ready_until_ms)
            .flatten(),
        hits: prepare::opportunities(&link.app_dir, day, ids),
    }
}

/// A re-read of the day, done off the engine task.
struct Reload {
    /// The day load it was started under — a later load makes it stale.
    seq: u64,
    /// The preparation's revision it reflects at least.
    rev: u64,
    progress: Progress,
    data: Result<data::DayData, String>,
}

fn spawn_reload(link: PrepLink, day: String, ids: Vec<String>, seq: u64, tx: mpsc::UnboundedSender<Reload>) {
    tokio::task::spawn_blocking(move || {
        // Progress first, data second: what is read holds at least what it says.
        let rev = link.replay_data.revision(&day);
        let progress = day_progress(&link, &day, &ids, false);
        let data = crate::replay_data::read::read_day(&link.app_dir, &day);
        let _ = tx.send(Reload { seq, rev, progress, data });
    });
}

/// Take over a re-read of the day: `new` holds everything `old` did plus what was
/// prepared since. The symbols it brings that the replay never fed arrive with
/// their whole past — previous close, then every event up to `sim` — as they would
/// have at load; everything else only gained events ahead of the clock. Returns
/// the cursor into `new`.
fn absorb(
    market: &RwLock<MarketState>,
    old: &data::DayData,
    new: &data::DayData,
    sim: DateTime<Utc>,
) -> usize {
    let sim_ms = sim.timestamp_millis();
    let upto = new.events.partition_point(|e| e.ts_ms <= sim_ms);
    let fresh: HashSet<&str> = new
        .kept
        .iter()
        .filter(|s| !old.kept.contains(*s))
        .map(String::as_str)
        .collect();
    let late: Vec<&data::TimedEvent> = new.events[..upto]
        .iter()
        .filter(|te| match &te.ev {
            data::Event::Trade { symbol, .. } | data::Event::Quote { symbol, .. } => {
                fresh.contains(symbol.as_str())
            }
            data::Event::News(h) => !old.news_ids.contains(&h.id),
        })
        .collect();
    let mut ms = market.write().unwrap();
    for sym in &fresh {
        if let Some(close) = new.prev_closes.get(*sym) {
            ms.set_previous_close(sym, *close, sim);
        }
    }
    for te in late {
        let at = chrono::TimeZone::timestamp_millis_opt(&Utc, te.ts_ms)
            .single()
            .unwrap_or(sim);
        match &te.ev {
            data::Event::Trade {
                symbol,
                price,
                size,
                prints,
                synthetic,
            } => ms.on_replay_trade(symbol, *price, *size, *prints, at, *synthetic),
            data::Event::Quote { symbol, bid, ask } => ms.on_quote(symbol, *bid, *ask, at, at),
            data::Event::News(h) => ms.on_news(h.clone()),
        }
    }
    upto
}

// ─── « Prochaine alerte » ───────────────────────────────────────────────────────

/// « Prochaine alerte » in progress. On a prepared day the opportunities are known:
/// the replay jumps to `NEXT_ALERT_LEAD_MS` before the next one and walks that last
/// stretch second by second, so the engine fires it as it would in real time.
struct Hunt {
    /// Alert-history length when it started: a longer list is the alert.
    baseline: usize,
    /// Speed and transport to return to.
    prev_speed: f64,
    was_playing: bool,
    /// Only opportunities after this are aimed at.
    after_ms: i64,
    /// The opportunity being approached, once the replay stands before it.
    target: Option<i64>,
    /// No prepared opportunity to aim at: fast-forward until anything alerts.
    legacy: bool,
}

/// The next prepared opportunity after `after_ms` whose ticker is not already on
/// screen for it.
fn next_opportunity(history: &[AlertSignal], hits: &[(i64, String)], after_ms: i64) -> Option<i64> {
    hits.iter()
        .find(|(ms, sym)| {
            *ms > after_ms
                && !history.iter().any(|a| {
                    a.symbol == *sym && (a.timestamp.timestamp_millis() - ms).abs() <= SAME_OPPORTUNITY_MS
                })
        })
        .map(|(ms, _)| *ms)
}

fn utc_ms(ms: i64) -> DateTime<Utc> {
    chrono::TimeZone::timestamp_millis_opt(&Utc, ms)
        .single()
        .unwrap_or_else(Utc::now)
}

// ─── Engine ─────────────────────────────────────────────────────────────────────

/// Start a replay of `day` at `start_min` ET minutes, from the day prepared on disk
/// (`crate::replay_data`), for the strategies `ids` (every preparable one when
/// empty). Returns an error when a replay is already active or the date is
/// malformed; a day that is not prepared fails in the engine, reported through the
/// status like any load error.
pub fn start(
    shared: Arc<ReplayShared>,
    deps: ReplayDeps,
    day: String,
    start_min: u32,
    ids: Vec<String>,
) -> Result<(), String> {
    {
        let st = shared.status.read().unwrap();
        if st.active {
            return Err("un replay est déjà actif".into());
        }
    }
    NaiveDate::parse_from_str(&day, "%Y-%m-%d").map_err(|_| "date invalide".to_string())?;
    let ids: Vec<String> = if ids.is_empty() {
        crate::replay_data::needs_registry().iter().map(|n| n.id().to_string()).collect()
    } else {
        ids
    };
    eprintln!("[tagdash] replay {day}: données préparées (replay_data) · stratégies {ids:?}");

    let (tx, rx) = mpsc::unbounded_channel::<ReplayCmd>();
    *shared.cmd_tx.lock().unwrap() = Some(tx);
    {
        let mut st = shared.status.write().unwrap();
        *st = ReplayStatus::default();
        st.active = true;
        st.state = "loading".into();
        st.day = Some(day.clone());
        st.strategies = ids.clone();
    }

    tauri::async_runtime::spawn(engine(shared, deps, day, start_min, ids, rx));
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn engine(
    shared: Arc<ReplayShared>,
    deps: ReplayDeps,
    mut day: String,
    start_min: u32,
    ids: Vec<String>,
    mut rx: mpsc::UnboundedReceiver<ReplayCmd>,
) {
    // Replay trading is a disposable sandbox. Preserve the live simulator and chart
    // contexts so historical fills can neither act on a live position nor survive
    // when the real-time feeds resume.
    let live_book = deps.internal_book.read().unwrap().clone();
    let live_contexts = deps.chart.read().unwrap().export_contexts();

    // ── Couper les flux live pour la durée du replay. ──
    deps.live_feed_running.store(false, Ordering::Relaxed);
    deps.news_feed_running.store(false, Ordering::Relaxed);
    // The session's strategies run for its duration, whatever their live toggle.
    clock::set_session_strategies(ids.clone());

    let mut day_data: Option<data::DayData> = None;
    let mut cursor: usize = 0;
    let mut sim: DateTime<Utc> = Utc::now();
    let mut day_end: DateTime<Utc> = sim;
    let mut speed: f64 = 1.0;
    let mut playing = false;
    let mut hunt: Option<Hunt> = None;
    let mut last_tick_emit: HashMap<String, Instant> = HashMap::new();

    // ── The day as prepared so far (see `day_progress`). ──
    let link = PrepLink::of(&deps);
    let mut hits: Vec<(i64, String)> = Vec::new();
    // What is in memory can be replayed up to here; None = to the end of the day.
    let mut cap: Option<i64> = None;
    // The preparation as last looked at: (revision, still pending, its cap).
    let mut loaded_rev: u64 = 0;
    let mut seen: (u64, bool, Option<i64>) = (0, false, None);
    let mut last_watch = Instant::now();
    let mut last_reload = Instant::now();
    let mut buffering = false;
    let mut reloading = false;
    let mut load_seq: u64 = 0;
    let (reload_tx, mut reload_rx) = mpsc::unbounded_channel::<Reload>();

    // Charge (ou recharge) une journée et positionne l'horloge au départ.
    // `$keep_fills` : false au premier chargement, true pour « Jour suivant ».
    // Voir le commentaire sur `drain_fill_windows` plus bas.
    macro_rules! load_day {
        ($d:expr) => {
            load_day!($d, false)
        };
        ($d:expr, $keep_fills:expr) => {{
            let d: String = $d;
            let keep_fills: bool = $keep_fills;
            {
                let mut st = shared.status.write().unwrap();
                st.state = "loading".into();
                st.day = Some(d.clone());
                st.progress = 0.0;
                st.error = None;
                st.notice = None;
            }
            // Progress first, data second: what is read holds at least what it says.
            let rev = deps.replay_data.revision(&d);
            let progress = day_progress(&link, &d, &ids, true);
            let loaded = data::load_day(&deps.app_dir, &deps.db, &d).await;
            match loaded {
                Ok(dd) => {
                    day = d.clone();
                    let nd = NaiveDate::parse_from_str(&d, "%Y-%m-%d").unwrap();
                    let noon = data::noon_utc(nd);
                    day_end = crate::time::et_clock_utc(noon, 20, 0);
                    hits = progress.hits;
                    cap = progress.cap;
                    loaded_rev = rev;
                    seen = (rev, deps.replay_data.pending(&d), cap);
                    load_seq += 1;
                    reloading = false;
                    buffering = false;
                    if let Some(h) = hunt.take() {
                        speed = h.prev_speed;
                    }
                    // Never past what is prepared: the minutes beyond may still
                    // turn into real ticks.
                    let mut start_at = crate::time::et_clock_utc(noon, start_min / 60, start_min % 60);
                    if let Some(c) = cap {
                        start_at = start_at.min(utc_ms(c));
                    }

                    // First day: start with a clean simulated book. `NextDay` keeps
                    // it so a replayed swing position can legitimately carry over.
                    if !keep_fills {
                        reset_replay_trading(&deps);
                    }

                    // Reset complet de l'état marché + moteurs, horloge au départ.
                    reset_market(&deps, &dd, start_at);
                    clear_alerts(&deps);
                    clock::activate(start_at);
                    clock::set_speed(speed);
                    clock::bump_generation();
                    sim = start_at;
                    cursor = 0;
                    // Rattrapage instantané : tout ce qui précède l'heure de départ
                    // (ex. 04:00→07:00) est injecté d'un bloc pour amorcer l'état.
                    cursor = emit_until(&deps, &dd, cursor, sim, &mut last_tick_emit);
                    // Premarket catch-up windows (04:00 → start time, injected in one
                    // block above).
                    //
                    // FIRST LOAD: discard them. Any position still open from the LIVE
                    // session would otherwise be filled on replayed historical prices.
                    //
                    // « JOUR SUIVANT »: KEEP them. The position was opened inside the
                    // replay and is being held as a swing; its SL/TP must be tested
                    // against the price path of the session being entered, or the day
                    // jump would step straight over the levels. Leaving the windows in
                    // place hands them to the internal trading loop on its next poll,
                    // which fills gap-aware (`w.first.min/max(level)`) — so an
                    // overnight gap through a stop fills at the OPEN, not at the stop.
                    if !keep_fills {
                        let _ = deps.market.write().unwrap().drain_fill_windows();
                    }

                    // ── Panic watchlist for the REPLAYED day. ──
                    // Must run AFTER clock::activate + bump_generation: the build
                    // derives its day and its premarket window from the app clock,
                    // so any earlier and it would build the LIVE day.
                    {
                        // Purge on replay START. Day-scoped reads already hide other
                        // days, but this also covers replaying today's own date, where
                        // the live rows would otherwise be indistinguishable. The
                        // guard is scoped so it drops before the await below.
                        let conn = deps.db.lock().unwrap();
                        let _ = crate::local_db::scoring_repository::delete_day(&conn, &d);
                        let _ = crate::local_db::scoring_repository::delete_build(&conn, &d);
                    }
                    // Spawned + timed out rather than awaited directly: the initial
                    // load runs BEFORE the select! loop, so a bare await would make
                    // ReplayCmd::Stop unobservable for the whole build. The timeout
                    // does not cancel the task — it keeps running under the build
                    // guard and stamps the ledger when it lands; the screener simply
                    // shows nothing (correct) until then.
                    {
                        let ad = deps.app_dir.clone();
                        let dbc = deps.db.clone();
                        let sc = deps.secrets.clone();
                        let h = tauri::async_runtime::spawn(async move {
                            crate::panic_watchlist::ensure_day(
                                &ad,
                                &dbc,
                                &sc,
                                crate::panic_watchlist::Force::Auto,
                            )
                            .await
                        });
                        match tokio::time::timeout(WATCHLIST_BUDGET, h).await {
                            Ok(Ok(r)) => eprintln!("[tagdash] replay {d}: watchlist {r:?}"),
                            Ok(Err(e)) => {
                                eprintln!("[tagdash] replay {d}: watchlist task failed: {e}")
                            }
                            Err(_) => eprintln!(
                                "[tagdash] replay {d}: watchlist still building, continuing"
                            ),
                        }
                    }

                    {
                        let mut st = shared.status.write().unwrap();
                        st.state = "paused".into();
                        st.playing = false;
                        st.source = Some(dd.source.to_string());
                        st.provider = Some(dd.provider.to_string());
                        st.symbols = dd.symbols;
                        st.events_total = dd.events.len();
                        st.events_done = cursor;
                        st.sim_time = Some(sim);
                        st.progress = 1.0;
                        st.speed = speed;
                        st.next_alert_armed = false;
                        st.buffering = false;
                        st.ready_until = cap.map(utc_ms);
                    }
                    playing = false;
                    day_data = Some(dd);
                    true
                }
                Err(e) => {
                    // Load failure: with a day already loaded (NextDay failed —
                    // e.g. holiday with no data) stay on the current day; with
                    // nothing loaded the replay can't run → terminal error.
                    let mut st = shared.status.write().unwrap();
                    if day_data.is_some() {
                        st.state = "paused".into();
                        st.day = Some(day.clone());
                        st.progress = 1.0;
                    } else {
                        st.state = "error".into();
                    }
                    st.error = Some(e);
                    false
                }
            }
        }};
    }

    let initial_ok = load_day!(day.clone());

    let mut ticker = tokio::time::interval(tokio::time::Duration::from_millis(TICK_MS));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    if initial_ok {
        'main: loop {
            // Where a forward move may go: the end of the day, or of what is prepared.
            let limit = cap.map_or(day_end, |c| utc_ms(c).min(day_end)).max(sim);
            tokio::select! {
                cmd = rx.recv() => {
                    let Some(cmd) = cmd else { break 'main };
                    let cmd = match cmd {
                        ReplayCmd::Toggle if playing => ReplayCmd::Pause,
                        ReplayCmd::Toggle => ReplayCmd::Play,
                        other => other,
                    };
                    match cmd {
                        ReplayCmd::Play => {
                            if day_data.is_some() {
                                playing = true;
                                let mut st = shared.status.write().unwrap();
                                st.state = "playing".into();
                                st.playing = true;
                                st.notice = None;
                            }
                        }
                        ReplayCmd::Pause | ReplayCmd::Toggle => {
                            playing = false;
                            if let Some(h) = hunt.take() {
                                speed = h.prev_speed;
                                clock::set_speed(speed);
                            }
                            let mut st = shared.status.write().unwrap();
                            st.state = "paused".into();
                            st.playing = false;
                            st.next_alert_armed = false;
                            st.speed = speed;
                        }
                        ReplayCmd::SetSpeed(s) => {
                            let s = s.clamp(0.1, 600.0);
                            match hunt.as_mut() {
                                // Approaching an opportunity: its last stretch keeps its
                                // own pace, the chosen speed takes over once it alerts.
                                Some(h) if !h.legacy => h.prev_speed = s,
                                // Une vitesse choisie à la main remplace celle de
                                // l'avance rapide.
                                Some(h) => {
                                    h.prev_speed = s;
                                    speed = s;
                                }
                                None => speed = s,
                            }
                            clock::set_speed(speed);
                            shared.status.write().unwrap().speed = speed;
                        }
                        ReplayCmd::SeekRelative(delta_secs) => {
                            if let Some(dd) = &day_data {
                                mark_seeking(&shared);
                                let target = sim + ChronoDuration::seconds(delta_secs);
                                let target = if target > sim { target.min(limit) } else { target };
                                let (ns, nc) = seek(&deps, dd, sim, cursor, target, day_end, &mut last_tick_emit);
                                sim = ns; cursor = nc;
                                {
                                    let mut st = shared.status.write().unwrap();
                                    st.sim_time = Some(sim);
                                    st.events_done = cursor;
                                    st.state = if playing { "playing" } else { "paused" }.into();
                                }
                                // A seek can cross 09:25 — see SeekClock below.
                                crate::panic_watchlist::request_build();
                            }
                        }
                        ReplayCmd::SeekClock { minutes } => {
                            if let Some(dd) = &day_data {
                                mark_seeking(&shared);
                                let nd = NaiveDate::parse_from_str(&day, "%Y-%m-%d").unwrap();
                                let noon = data::noon_utc(nd);
                                let target = crate::time::et_clock_utc(noon, minutes / 60, minutes % 60);
                                let target = if target > sim { target.min(limit) } else { target };
                                let (ns, nc) = seek(&deps, dd, sim, cursor, target, day_end, &mut last_tick_emit);
                                sim = ns; cursor = nc;
                                {
                                    let mut st = shared.status.write().unwrap();
                                    st.sim_time = Some(sim);
                                    st.events_done = cursor;
                                    st.state = if playing { "playing" } else { "paused" }.into();
                                }
                                // Jumping to 09:30 does NOT re-run the day load, so nudge
                                // the scheduler: a list built before 09:25 is provisional
                                // (thin premarket) and gets promoted once the clock
                                // crosses the trigger. Not awaited — freezing an instant
                                // jump for a heavy build would be the wrong trade; day
                                // correctness was already secured at load time.
                                crate::panic_watchlist::request_build();
                            }
                        }
                        ReplayCmd::NextAlert => {
                            if day_data.is_some() {
                                let sim_ms = sim.timestamp_millis();
                                // Pressed again while one is being approached: on to the
                                // one after it.
                                let (prev_speed, was_playing, after_ms) = match hunt.take() {
                                    Some(h) => (h.prev_speed, h.was_playing, h.target.unwrap_or(h.after_ms)),
                                    None => (speed, playing, sim_ms),
                                };
                                // Nothing prepared to aim at (and nothing coming): the
                                // fast-forward until anything alerts.
                                let legacy = hits.is_empty() && cap.is_none();
                                hunt = Some(Hunt {
                                    baseline: deps.alert_history.read().unwrap().len(),
                                    prev_speed,
                                    was_playing,
                                    after_ms,
                                    target: None,
                                    legacy,
                                });
                                speed = if legacy { NEXT_ALERT_SPEED } else { prev_speed };
                                clock::set_speed(speed);
                                playing = true;
                                let mut st = shared.status.write().unwrap();
                                st.state = "playing".into();
                                st.playing = true;
                                st.speed = speed;
                                st.next_alert_armed = true;
                                st.notice = None;
                            }
                        }
                        ReplayCmd::NextBar => {
                            if let Some(dd) = &day_data {
                                let cur_s = sim.timestamp();
                                let next_min = cur_s - (cur_s % 60) + 60;
                                let target = chrono::TimeZone::timestamp_opt(&Utc, next_min, 0)
                                    .single()
                                    .unwrap_or(sim);
                                let target = target.min(limit);
                                let (ns, nc) = seek(&deps, dd, sim, cursor, target, day_end, &mut last_tick_emit);
                                sim = ns;
                                cursor = nc;
                                playing = false;
                                if let Some(h) = hunt.take() {
                                    speed = h.prev_speed;
                                    clock::set_speed(speed);
                                }
                                {
                                    let mut st = shared.status.write().unwrap();
                                    st.state = "paused".into();
                                    st.playing = false;
                                    st.next_alert_armed = false;
                                    st.speed = speed;
                                    st.sim_time = Some(sim);
                                    st.events_done = cursor;
                                }
                                crate::panic_watchlist::request_build();
                            }
                        }
                        ReplayCmd::NextDay => {
                            // The next PREPARED day: a replay reads only what is on
                            // disk. With none after this one, the next weekday — whose
                            // load then says it must be prepared first.
                            let next = crate::replay_data::store::days_on_disk(&deps.app_dir)
                                .into_iter()
                                .find(|d| {
                                    d.as_str() > day.as_str()
                                        && crate::replay_data::store::DayStore::open_existing(&deps.app_dir, d)
                                            .is_some_and(|s| s.has_market())
                                })
                                .unwrap_or_else(|| next_weekday(&day));
                            // keep_fills = true: an open swing position carries into the
                            // new session and its SL/TP must be evaluated against it.
                            let _ = load_day!(next, true);
                            clock::set_speed(speed);
                        }
                        ReplayCmd::Stop => break 'main,
                    }
                }

                // ── The day read again, with what was prepared since. ──
                Some(reload) = reload_rx.recv() => {
                    if reload.seq != load_seq {
                        continue;
                    }
                    reloading = false;
                    last_reload = Instant::now();
                    match (day_data.take(), reload.data) {
                        (Some(old), Ok(new)) => {
                            cursor = absorb(&deps.market, &old, &new, sim);
                            hits = reload.progress.hits;
                            cap = reload.progress.cap;
                            loaded_rev = reload.rev;
                            let mut st = shared.status.write().unwrap();
                            st.source = Some(new.source.to_string());
                            st.symbols = new.symbols;
                            st.events_total = new.events.len();
                            st.events_done = cursor;
                            st.ready_until = cap.map(utc_ms);
                            day_data = Some(new);
                        }
                        (old, Err(e)) => {
                            eprintln!("[tagdash] replay {day}: relecture impossible ({e})");
                            day_data = old;
                        }
                        (None, Ok(_)) => {}
                    }
                }

                _ = ticker.tick() => {
                    // ── The day's preparation, while it is still running: read the
                    // day again when the replay is about to need what was added. ──
                    if day_data.is_some() && cap.is_some() && last_watch.elapsed() >= WATCH_EVERY {
                        last_watch = Instant::now();
                        let rev = deps.replay_data.revision(&day);
                        let pending = deps.replay_data.pending(&day);
                        if (rev, pending) != (seen.0, seen.1) {
                            seen = (rev, pending, day_progress(&link, &day, &ids, false).cap);
                        }
                        // More is on disk than in memory: further prepared, finished
                        // (no cap any more), or given up on.
                        let more = seen.0 != loaded_rev
                            && match (seen.2, cap) {
                                (Some(now), Some(have)) => now > have,
                                (None, _) => true,
                                (Some(_), None) => false,
                            };
                        let runway_secs = match cap {
                            Some(c) if playing => (c - sim.timestamp_millis()) as f64 / 1_000.0 / speed.max(0.1),
                            _ => f64::INFINITY,
                        };
                        let waiting = hunt.as_ref().is_some_and(|h| !h.legacy && h.target.is_none());
                        // Standing at the edge of what is in memory (a jump stopped there).
                        let at_edge = cap.is_some_and(|c| sim.timestamp_millis() >= c);
                        let due = seen.2.is_none()
                            || buffering
                            || waiting
                            || at_edge
                            || runway_secs < RELOAD_RUNWAY_SECS
                            || last_reload.elapsed() >= RELOAD_EVERY;
                        if more && !reloading && due {
                            reloading = true;
                            spawn_reload(link.clone(), day.clone(), ids.clone(), load_seq, reload_tx.clone());
                        }
                    }

                    if !playing { continue; }
                    let Some(dd) = &day_data else { continue };

                    // ── « Prochaine alerte » : se placer devant la prochaine
                    // opportunité préparée, sans avance rapide entre les deux. ──
                    if let Some(h) = hunt.as_mut().filter(|h| !h.legacy && h.target.is_none()) {
                        let next = next_opportunity(&deps.alert_history.read().unwrap(), &hits, h.after_ms);
                        let land = next.map(|t| utc_ms(t - NEXT_ALERT_LEAD_MS));
                        let jump = match land {
                            // Prepared that far: stand before it, then walk.
                            Some(land) if land <= limit => {
                                h.target = next;
                                speed = NEXT_ALERT_APPROACH_SPEED;
                                clock::set_speed(speed);
                                Some(land)
                            }
                            // Not downloaded that far yet — or nothing known yet while
                            // the day is still being prepared: wait at the edge.
                            Some(_) => Some(limit),
                            None if cap.is_some() => Some(limit),
                            // Nothing left today.
                            None => {
                                speed = h.prev_speed;
                                clock::set_speed(speed);
                                playing = h.was_playing;
                                hunt = None;
                                let mut st = shared.status.write().unwrap();
                                st.notice = Some("Plus d'opportunité préparée sur cette séance".into());
                                st.next_alert_armed = false;
                                st.speed = speed;
                                st.playing = playing;
                                st.state = if playing { "playing" } else { "paused" }.into();
                                None
                            }
                        };
                        if let Some(to) = jump.filter(|to| *to > sim) {
                            let (ns, nc) = seek(&deps, dd, sim, cursor, to, day_end, &mut last_tick_emit);
                            sim = ns;
                            cursor = nc;
                            crate::panic_watchlist::request_build();
                        }
                        if !playing { continue; }
                    }

                    // ── Advance. Approaching an opportunity, never more than one
                    // market second at a time: the engines must see each of them. ──
                    let step_ms = (TICK_MS as f64 * speed) as i64;
                    let approaching = hunt.as_ref().is_some_and(|h| h.target.is_some());
                    let subs = if approaching { ((step_ms + 999) / 1_000).max(1) } else { 1 };
                    buffering = false;
                    for i in 0..subs {
                        let mut next = sim + ChronoDuration::milliseconds(step_ms / subs);
                        if next >= limit {
                            next = limit;
                            // Held by the preparation, not by the end of the day.
                            buffering = cap.is_some() && limit < day_end;
                        }
                        sim = next;
                        clock::set_sim(sim);
                        cursor = emit_until(&deps, dd, cursor, sim, &mut last_tick_emit);
                        if sim >= limit {
                            break;
                        }
                        if i + 1 < subs {
                            tokio::time::sleep(tokio::time::Duration::from_millis(TICK_MS / subs as u64)).await;
                        }
                    }

                    // ── « Prochaine alerte » : l'alerte est tombée → le ticker est
                    // sur les graphiques, la lecture continue à la vitesse d'avant. ──
                    if let Some(h) = hunt.as_mut() {
                        let alerts = deps.alert_history.read().unwrap().len();
                        // The list was reset under the hunt (backward seek).
                        h.baseline = h.baseline.min(alerts);
                        if alerts > h.baseline {
                            speed = h.prev_speed;
                            clock::set_speed(speed);
                            hunt = None;
                            let mut st = shared.status.write().unwrap();
                            st.next_alert_armed = false;
                            st.speed = speed;
                        } else if h.target.is_some_and(|t| sim.timestamp_millis() > t + NEXT_ALERT_TAIL_MS) {
                            // It did not fire here: on to the one after it.
                            h.after_ms = h.target.take().unwrap_or(h.after_ms);
                        }
                    }

                    // Fin de journée.
                    if cursor >= dd.events.len() && sim >= day_end {
                        playing = false;
                        if let Some(h) = hunt.take() {
                            speed = h.prev_speed;
                            clock::set_speed(speed);
                        }
                        let mut st = shared.status.write().unwrap();
                        st.state = "ended".into();
                        st.playing = false;
                        st.next_alert_armed = false;
                    }

                    let mut st = shared.status.write().unwrap();
                    st.sim_time = Some(sim);
                    st.events_done = cursor;
                    st.speed = speed;
                    st.buffering = buffering;
                    st.ready_until = cap.map(utc_ms);
                }
            }
        }
    } // if initial_ok

    // ── Cleanup : retour au temps réel + relance des flux live. ──
    // Une erreur de chargement initiale est préservée pour que la toolbar
    // puisse l'afficher après le retour en mode live.
    let load_error = {
        let st = shared.status.read().unwrap();
        if st.state == "error" {
            st.error.clone()
        } else {
            None
        }
    };
    *shared.cmd_tx.lock().unwrap() = None;
    // Still on the simulated clock: close the replay's review sessions at the
    // replayed instant they ended.
    crate::review::on_replay_stop();
    clock::deactivate();
    clock::bump_generation();
    {
        let mut ms = deps.market.write().unwrap();
        ms.reset_data();
    }
    clear_alerts(&deps);
    // Restore the exact live trading state that existed before replay. Commands
    // issued during replay may have checkpointed the disposable book/contexts, so
    // restore SQLite as well as RAM before live prices resume.
    {
        *deps.internal_book.write().unwrap() = live_book;
        deps.chart.write().unwrap().replace_contexts(live_contexts);
        let book = deps.internal_book.read().unwrap().persistable_snapshot();
        let contexts = deps.chart.read().unwrap().export_contexts();
        let conn = deps.db.lock().unwrap();
        crate::local_db::book_repository::save_book(&conn, &book);
        crate::local_db::book_repository::save_chart_contexts(&conn, &contexts);
    }
    // La watchlist Panic du jour rejoué a été construite sur des données bornées à
    // l'instant simulé — on supprime ce jour-là et son entrée de ledger. Tous les
    // autres jours (dont le jour live) sont intacts : c'est ce qui permet à la liste
    // live de survivre à un replay d'une autre date sans reconstruction.
    // Placé APRÈS `clock::deactivate()` : l'horloge doit être revenue au temps réel.
    {
        let conn = deps.db.lock().unwrap();
        let _ = crate::local_db::scoring_repository::delete_day(&conn, &day);
        let _ = crate::local_db::scoring_repository::delete_build(&conn, &day);
    }
    crate::panic_watchlist::request_build();
    {
        let mut st = shared.status.write().unwrap();
        *st = ReplayStatus::default();
        if let Some(e) = load_error {
            st.state = "error".into();
            st.error = Some(e);
        }
    }
    match crate::commands::spawn_live_feed(
        deps.market.clone(),
        deps.config.clone(),
        deps.secrets.clone(),
        deps.db.clone(),
        deps.internal_book.clone(),
        deps.broker_mirror.clone(),
        deps.strategy_enabled.clone(),
        deps.live_feed_running.clone(),
        deps.focus_rx_restart.clone(),
        deps.app.clone(),
    ) {
        Ok(_) => eprintln!("[tagdash] replay: flux live relancé"),
        Err(e) => eprintln!("[tagdash] replay: relance du flux live impossible: {e}"),
    }
    if let Err(e) = crate::commands::spawn_news_feed(
        deps.market.clone(),
        deps.secrets.clone(),
        deps.news_feed_running.clone(),
    ) {
        eprintln!("[tagdash] replay: relance du flux news impossible: {e}");
    }
    eprintln!("[tagdash] replay: terminé, retour au temps réel");
}

/// Surface a jump through the day before doing it: it can take a moment on a busy
/// tape, and a clock that silently stands still reads as a frozen replay.
fn mark_seeking(shared: &ReplayShared) {
    shared.status.write().unwrap().state = "seeking".into();
}

/// Vide l'état marché et re-seed les previous closes du jour rejoué.
fn reset_market(deps: &ReplayDeps, dd: &data::DayData, now: DateTime<Utc>) {
    let mut ms = deps.market.write().unwrap();
    ms.reset_data();
    // Le tape rejoué porte de VRAIS compteurs de trades — le `n` de la barre 1 min
    // réparti sur ses tranches, ou le `n` de l'agrégat 1 s tel quel. Sans cette
    // déclaration, une session Massive live précédente aurait laissé
    // `SecondAggregates`, et les mesures de rythme du tape seraient lues comme
    // « on ne sait pas » alors que l'information est là.
    ms.set_tape_kind(crate::types::TapeKind::Prints);
    for (sym, close) in &dd.prev_closes {
        ms.set_previous_close(sym, *close, now);
    }
}

fn clear_alerts(deps: &ReplayDeps) {
    deps.active_alerts.write().unwrap().clear();
    deps.alert_history.write().unwrap().clear();
}

/// Drop all simulated trading state. A backward seek cannot retain orders or fills
/// created in the abandoned future; they are user actions and cannot be reconstructed
/// from the market tape.
fn reset_replay_trading(deps: &ReplayDeps) {
    *deps.internal_book.write().unwrap() = crate::internal_trading::InternalBook::new();
    deps.chart.write().unwrap().replace_contexts(Vec::new());
}

/// Seek absolu : en avant = rattrapage; en arrière = reset complet puis re-feed
/// jusqu'à la cible (les moteurs repartent de zéro via le bump de génération).
fn seek(
    deps: &ReplayDeps,
    dd: &data::DayData,
    sim: DateTime<Utc>,
    cursor: usize,
    target: DateTime<Utc>,
    day_end: DateTime<Utc>,
    last_tick_emit: &mut HashMap<String, Instant>,
) -> (DateTime<Utc>, usize) {
    let target = target.min(day_end);
    if target >= sim {
        clock::set_sim(target);
        let nc = emit_until(deps, dd, cursor, target, last_tick_emit);
        (target, nc)
    } else {
        reset_market(deps, dd, target);
        clear_alerts(deps);
        reset_replay_trading(deps);
        clock::bump_generation();
        clock::set_sim(target);
        let nc = emit_until(deps, dd, 0, target, last_tick_emit);
        // The catch-up re-emission filled the windows with the entire price
        // range from day start to target — drain them so bracket orders don't
        // fire on historical prices the trading loop hasn't seen live.
        let _ = deps.market.write().unwrap().drain_fill_windows();
        (target, nc)
    }
}

/// Injecte dans MarketState tous les événements d'horodatage ≤ `until`, à partir
/// de `cursor`. Retourne le nouveau curseur. Un seul write-lock par lot ; les
/// market-tick (symbols affichés) sont émis hors lock, throttlés en temps réel.
fn emit_until(
    deps: &ReplayDeps,
    dd: &data::DayData,
    mut cursor: usize,
    until: DateTime<Utc>,
    last_tick_emit: &mut HashMap<String, Instant>,
) -> usize {
    let until_ms = until.timestamp_millis();
    if cursor >= dd.events.len() || dd.events[cursor].ts_ms > until_ms {
        return cursor;
    }
    let focus: std::collections::HashSet<String> = deps.focus_rx.borrow().iter().cloned().collect();
    let mut ticks: HashMap<String, TickEvent> = HashMap::new();
    // Review sessions keep their own copy of what the replay emits (flat files are
    // deleted once used). The origin says which tape it was.
    let origin = format!("replay:{}:{}", dd.source, dd.provider);
    // In batches: a catch-up or a seek can cross hundreds of thousands of events,
    // and holding the MarketState write lock through all of them froze every
    // reader — charts, strategies, the toolbar's own commands — for seconds.
    while cursor < dd.events.len() && dd.events[cursor].ts_ms <= until_ms {
        let batch_end = cursor + EMIT_BATCH;
        let mut ms = deps.market.write().unwrap();
        while cursor < dd.events.len() && cursor < batch_end && dd.events[cursor].ts_ms <= until_ms {
            let te = &dd.events[cursor];
            let event_time = chrono::TimeZone::timestamp_millis_opt(&Utc, te.ts_ms)
                .single()
                .unwrap_or(until);
            match &te.ev {
                data::Event::Trade {
                    symbol,
                    price,
                    size,
                    prints,
                    synthetic,
                } => {
                    ms.on_replay_trade(symbol, *price, *size, *prints, event_time, *synthetic);
                    crate::review::capture::trade(
                        symbol, te.ts_ms, *price, *size, *prints, None, None, &origin,
                    );
                    if focus.contains(symbol) {
                        ticks.insert(
                            symbol.clone(),
                            TickEvent {
                                symbol: symbol.clone(),
                                price: *price,
                                ts: event_time.timestamp(),
                            },
                        );
                    }
                }
                data::Event::Quote { symbol, bid, ask } => {
                    ms.on_quote(symbol, *bid, *ask, event_time, event_time);
                    crate::review::capture::quote(
                        symbol, te.ts_ms, *bid, None, *ask, None, None, None, &origin,
                    );
                }
                data::Event::News(h) => {
                    ms.on_news(h.clone());
                }
            }
            cursor += 1;
        }
    }
    let at = Instant::now();
    for (sym, ev) in ticks {
        let fresh = last_tick_emit
            .get(&sym)
            .map(|t| at.duration_since(*t).as_millis() >= TICK_THROTTLE_MS)
            .unwrap_or(true);
        if fresh {
            let _ = deps.app.emit("market-tick", &ev);
            last_tick_emit.insert(sym, at);
        }
    }
    cursor
}

/// Jour ouvré suivant (saute samedi/dimanche — les jours fériés produiront
/// simplement « aucune donnée » et l'utilisateur peut re-cliquer).
pub(crate) fn next_weekday(day: &str) -> String {
    let mut d =
        NaiveDate::parse_from_str(day, "%Y-%m-%d").unwrap_or_else(|_| Utc::now().date_naive());
    loop {
        d += ChronoDuration::days(1);
        let wd = chrono::Datelike::weekday(&d);
        if wd != chrono::Weekday::Sat && wd != chrono::Weekday::Sun {
            return d.format("%Y-%m-%d").to_string();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(ms: i64) -> DateTime<Utc> {
        utc_ms(1_782_730_800_000 + ms) // 2026-06-29 07:00 ET
    }

    fn trade(ms: i64, symbol: &str, price: f64) -> data::TimedEvent {
        data::TimedEvent {
            ts_ms: at(ms).timestamp_millis(),
            ev: data::Event::Trade {
                symbol: symbol.into(),
                price,
                size: 100,
                prints: 1,
                synthetic: false,
            },
        }
    }

    fn day(mut events: Vec<data::TimedEvent>, kept: &[&str]) -> data::DayData {
        events.sort_by_key(|e| e.ts_ms);
        data::DayData {
            events,
            prev_closes: kept.iter().map(|s| (s.to_string(), 1.0)).collect(),
            source: "trades",
            provider: "alpaca_sip",
            symbols: kept.len(),
            kept: kept.iter().map(|s| s.to_string()).collect(),
            news_ids: HashSet::new(),
        }
    }

    /// A day read again while it replays: the symbol prepared since arrives with its
    /// whole past, the one already fed is not fed twice, and the cursor stands at the
    /// clock in the new day.
    #[test]
    fn a_reread_day_brings_new_symbols_with_their_past_only() {
        let old = day(vec![trade(0, "AAA", 2.0), trade(10_000, "AAA", 2.1), trade(60_000, "AAA", 2.5)], &["AAA"]);
        let new = day(
            vec![
                trade(0, "AAA", 2.0),
                trade(10_000, "AAA", 2.1),
                trade(60_000, "AAA", 2.5),
                trade(5_000, "BBB", 7.0),
                trade(20_000, "BBB", 7.4),
                trade(90_000, "BBB", 9.0),
            ],
            &["AAA", "BBB"],
        );
        let market = RwLock::new(MarketState::new());
        let sim = at(30_000);
        // What the replay had fed of the old day by `sim`.
        {
            let mut ms = market.write().unwrap();
            ms.on_replay_trade("AAA", 2.0, 100, 1, at(0), false);
            ms.on_replay_trade("AAA", 2.1, 100, 1, at(10_000), false);
        }
        let cursor = absorb(&market, &old, &new, sim);
        assert_eq!(cursor, 4, "the four events up to the clock, of both symbols");
        let ms = market.read().unwrap();
        assert_eq!(ms.tickers["AAA"].volume_day, 200, "not fed a second time");
        assert_eq!(ms.tickers["BBB"].volume_day, 200, "its past up to the clock");
        assert_eq!(ms.tickers["BBB"].last_price, Some(7.4), "…and nothing from after it");
        assert_eq!(ms.tickers["BBB"].previous_close, Some(1.0));
    }

    fn alert(symbol: &str, ms: i64) -> AlertSignal {
        serde_json::from_value(serde_json::json!({
            "alert_id": format!("mp-{ms}-{symbol}"),
            "timestamp": at(ms),
            "symbol": symbol,
            "strategy_id": "micro_pullback",
            "strategy_name": "Micro Pullback",
            "priority": 3,
            "session": "premarket",
            "price": null, "bid": null, "ask": null, "spread": null, "volume": null,
            "rvol": null, "change_day_pct": null, "float_shares": null,
            "news_today": false, "halted": null, "latency_ui_ms": null,
            "reason": "",
        }))
        .expect("an alert")
    }

    /// « Prochaine alerte » aims at the next prepared opportunity in time — past the
    /// ones whose ticker is already on screen for it.
    #[test]
    fn the_next_opportunity_skips_what_already_alerted() {
        let ms = |m: i64| at(m * 60_000).timestamp_millis();
        let hits = vec![(ms(0), "AAA".to_string()), (ms(14), "BBB".to_string()), (ms(41), "CCC".to_string())];
        assert_eq!(next_opportunity(&[], &hits, ms(0) - 1), Some(ms(0)));
        assert_eq!(next_opportunity(&[], &hits, ms(0)), Some(ms(14)), "strictly after");
        // BBB fired a few seconds after its prepared time: it is done.
        let history = vec![alert("BBB", 14 * 60_000 + 4_000)];
        assert_eq!(next_opportunity(&history, &hits, ms(1)), Some(ms(41)));
        // An alert on the same ticker hours earlier is another opportunity.
        let early = vec![alert("CCC", 0)];
        assert_eq!(next_opportunity(&early, &hits, ms(20)), Some(ms(41)));
        assert_eq!(next_opportunity(&[], &hits, ms(41)), None);
    }
}
