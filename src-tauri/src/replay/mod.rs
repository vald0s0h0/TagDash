// Market Replay — rejoue une journée historique à travers toute la plateforme.
//
// SOURCE DE DONNÉES — par défaut (`SourceChoice::Api`), même priorité qu'en
// live/paper (voir `data::Provider` et `commands::spawn_live_feed`) : Alpaca SIP
// d'abord, puis Massive Starter+, puis les flat files locaux ; chaque source
// dégrade proprement vers la suivante si elle ne peut pas répondre.
//
// Le sélecteur de la modale « Choisir un jour » peut FORCER un dataset local à la
// place — `flat_trades` (vrais prints + quotes sur les fenêtres d'ignition) ou
// `flat_minutes` (tranches 10 s synthétisées). Un mode forcé ne dégrade jamais :
// ni réseau, ni cache daily. C'est le point : comparer une même séance sur deux
// tapes n'a de sens que si le tape demandé est bien celui qui a été rejoué. Le
// choix vaut pour toute la session, « jour suivant » compris.
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
//   • chaque appel REST Alpaca hors replay-loader est borné à l'instant simulé
//     (aucune fuite de données du futur — voir `clock::rest_end_clamp`).
//
// À l'arrêt : horloge réelle restaurée, MarketState vidé, watchlist Panic
// purgée (elle datait du jour simulé), flux live relancés.

pub mod clock;
pub mod data;
pub mod tape;

use std::collections::HashMap;
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
/// Speed used by « passer à la prochaine alerte » — fast-forward aggressif.
const NEXT_ALERT_SPEED: f64 = 120.0;
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
    /// D'OÙ viennent les données — "alpaca_sip" | "massive" | "alpaca_iex" |
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
    SetSpeed(f64),
    /// Avance/recule de N secondes simulées (recul = reset + re-feed rapide).
    SeekRelative(i64),
    /// Saute à HH:MM (ET) du jour rejoué.
    SeekClock {
        minutes: u32,
    },
    /// Avance jusqu'à la prochaine alerte scanner, puis pause.
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
    pub app: tauri::AppHandle,
}

#[derive(Clone, Serialize)]
struct TickEvent {
    symbol: String,
    price: f64,
    ts: i64,
}

// ─── Source resolution ──────────────────────────────────────────────────────────

/// Pick the day-loading source for this replay session, with the SAME priority the
/// live feed uses: Alpaca SIP > Massive Starter+ > flat files (see `data::Provider`).
///
/// Read once at start and reused for every day of the session, « jour suivant »
/// included, so a replay cannot silently change tape mid-run.
///
/// Credentials stay optional, always. Nothing here can refuse to start: a key that
/// is missing simply removes its step from `data::attempt_order`, and the walk ends
/// on the flat files / daily-bars fallback — which is exactly the offline replay the
/// flat files exist for. `offline` short-circuits to the local files without a probe.
///
/// `choice` is the day-picker toggle. A forced flat-file mode skips the resolution
/// entirely and hands back credential-free sources: the load must stay on disk, and
/// the surest way to guarantee that is to give it nothing to call with.
fn resolve_sources(deps: &ReplayDeps, choice: data::SourceChoice) -> data::Sources {
    if choice.is_local() {
        return data::Sources {
            provider: Some(data::Provider::FlatFiles),
            alpaca: None,
            massive_key: None,
            choice,
        };
    }
    let offline = deps.config.read().unwrap().data_source.offline;
    let (alpaca, massive_key) = {
        let s = deps.secrets.read().unwrap();
        let alpaca = s
            .alpaca_key
            .clone()
            .zip(s.alpaca_secret.clone())
            .filter(|(k, sec)| !k.is_empty() && !sec.is_empty());
        let massive = s.massive_api_key.clone().filter(|k| !k.trim().is_empty());
        (alpaca, massive)
    };
    // The last PROBED Alpaca subscription, not a guess — `Provider::resolve` only
    // promotes to the SIP path on a positive answer.
    let alpaca_plan = {
        let conn = deps.db.lock().unwrap();
        crate::local_db::cache_repository::get_app_meta(&conn, crate::alpaca::plan::PLAN_META_KEY)
            .as_deref()
            .and_then(crate::alpaca::plan::Plan::from_meta)
    };
    let massive_tier = crate::massive::plan::effective_from_app_state(&deps.db, &deps.config);
    let provider = data::Provider::resolve(
        offline,
        alpaca.is_some(),
        alpaca_plan,
        massive_key.is_some(),
        massive_tier,
    );
    data::Sources {
        provider: Some(provider),
        alpaca: (!offline).then_some(alpaca).flatten(),
        massive_key: (!offline).then_some(massive_key).flatten(),
        choice,
    }
}

// ─── Engine ─────────────────────────────────────────────────────────────────────

/// Start a replay of `day` at `start_min` ET minutes (240/420/570), reading it via
/// `choice` (ranked API walk, or a forced local flat-file dataset). Returns an error
/// when a replay is already active or the date is malformed.
pub fn start(
    shared: Arc<ReplayShared>,
    deps: ReplayDeps,
    day: String,
    start_min: u32,
    choice: data::SourceChoice,
) -> Result<(), String> {
    {
        let st = shared.status.read().unwrap();
        if st.active {
            return Err("un replay est déjà actif".into());
        }
    }
    NaiveDate::parse_from_str(&day, "%Y-%m-%d").map_err(|_| "date invalide".to_string())?;
    let sources = resolve_sources(&deps, choice);
    eprintln!(
        "[tagdash] replay {day}: mode = {}, source prioritaire = {}",
        choice.as_str(),
        sources
            .provider
            .map_or("flat_files", data::Provider::as_str),
    );

    let (tx, rx) = mpsc::unbounded_channel::<ReplayCmd>();
    *shared.cmd_tx.lock().unwrap() = Some(tx);
    {
        let mut st = shared.status.write().unwrap();
        *st = ReplayStatus::default();
        st.active = true;
        st.state = "loading".into();
        st.day = Some(day.clone());
    }

    tauri::async_runtime::spawn(engine(shared, deps, sources, day, start_min, rx));
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn engine(
    shared: Arc<ReplayShared>,
    deps: ReplayDeps,
    sources: data::Sources,
    mut day: String,
    start_min: u32,
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

    let mut day_data: Option<data::DayData> = None;
    let mut cursor: usize = 0;
    let mut sim: DateTime<Utc> = Utc::now();
    let mut day_end: DateTime<Utc> = sim;
    let mut speed: f64 = 1.0;
    let mut playing = false;
    let mut next_alert_baseline: Option<(usize, f64)> = None; // (history len, prev speed)
    let mut last_tick_emit: HashMap<String, Instant> = HashMap::new();

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
            }
            let shared_p = shared.clone();
            let focus_now: Vec<String> = deps.focus_rx.borrow().clone();
            let loaded = data::load_day(
                &deps.app_dir,
                &deps.db,
                &sources,
                &d,
                &focus_now,
                move |f| {
                    shared_p.status.write().unwrap().progress = f;
                },
            )
            .await;
            match loaded {
                Ok(dd) => {
                    day = d.clone();
                    let nd = NaiveDate::parse_from_str(&d, "%Y-%m-%d").unwrap();
                    let noon = data::noon_utc(nd);
                    let start_at = crate::time::et_clock_utc(noon, start_min / 60, start_min % 60);
                    day_end = crate::time::et_clock_utc(noon, 20, 0);

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
            tokio::select! {
                cmd = rx.recv() => {
                    let Some(cmd) = cmd else { break 'main };
                    match cmd {
                        ReplayCmd::Play => {
                            if day_data.is_some() {
                                playing = true;
                                let mut st = shared.status.write().unwrap();
                                st.state = "playing".into();
                                st.playing = true;
                            }
                        }
                        ReplayCmd::Pause => {
                            playing = false;
                            if let Some((_, prev)) = next_alert_baseline.take() {
                                speed = prev;
                                clock::set_speed(speed);
                            }
                            let mut st = shared.status.write().unwrap();
                            st.state = "paused".into();
                            st.playing = false;
                            st.next_alert_armed = false;
                            st.speed = speed;
                        }
                        ReplayCmd::SetSpeed(s) => {
                            speed = s.clamp(0.1, 600.0);
                            // Une vitesse choisie à la main remplace celle du mode alerte.
                            if next_alert_baseline.is_some() {
                                next_alert_baseline = next_alert_baseline.map(|(b, _)| (b, speed));
                            }
                            clock::set_speed(speed);
                            shared.status.write().unwrap().speed = speed;
                        }
                        ReplayCmd::SeekRelative(delta_secs) => {
                            if let Some(dd) = &day_data {
                                let target = sim + ChronoDuration::seconds(delta_secs);
                                let (ns, nc) = seek(&deps, dd, sim, cursor, target, day_end, &mut last_tick_emit);
                                sim = ns; cursor = nc;
                                {
                                    let mut st = shared.status.write().unwrap();
                                    st.sim_time = Some(sim);
                                    st.events_done = cursor;
                                }
                                // A seek can cross 09:25 — see SeekClock below.
                                crate::panic_watchlist::request_build();
                            }
                        }
                        ReplayCmd::SeekClock { minutes } => {
                            if let Some(dd) = &day_data {
                                let nd = NaiveDate::parse_from_str(&day, "%Y-%m-%d").unwrap();
                                let noon = data::noon_utc(nd);
                                let target = crate::time::et_clock_utc(noon, minutes / 60, minutes % 60);
                                let (ns, nc) = seek(&deps, dd, sim, cursor, target, day_end, &mut last_tick_emit);
                                sim = ns; cursor = nc;
                                {
                                    let mut st = shared.status.write().unwrap();
                                    st.sim_time = Some(sim);
                                    st.events_done = cursor;
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
                            if day_data.is_some() && next_alert_baseline.is_none() {
                                let baseline = deps.alert_history.read().unwrap().len();
                                next_alert_baseline = Some((baseline, speed));
                                speed = NEXT_ALERT_SPEED;
                                clock::set_speed(speed);
                                playing = true;
                                let mut st = shared.status.write().unwrap();
                                st.state = "playing".into();
                                st.playing = true;
                                st.speed = speed;
                                st.next_alert_armed = true;
                            }
                        }
                        ReplayCmd::NextBar => {
                            if let Some(dd) = &day_data {
                                let cur_s = sim.timestamp();
                                let next_min = cur_s - (cur_s % 60) + 60;
                                let target = chrono::TimeZone::timestamp_opt(&Utc, next_min, 0)
                                    .single()
                                    .unwrap_or(sim);
                                let target = target.min(day_end);
                                let (ns, nc) = seek(&deps, dd, sim, cursor, target, day_end, &mut last_tick_emit);
                                sim = ns;
                                cursor = nc;
                                playing = false;
                                {
                                    let mut st = shared.status.write().unwrap();
                                    st.state = "paused".into();
                                    st.playing = false;
                                    st.sim_time = Some(sim);
                                    st.events_done = cursor;
                                }
                                crate::panic_watchlist::request_build();
                            }
                        }
                        ReplayCmd::NextDay => {
                            let next = next_weekday(&day);
                            // keep_fills = true: an open swing position carries into the
                            // new session and its SL/TP must be evaluated against it.
                            let _ = load_day!(next, true);
                        }
                        ReplayCmd::Stop => break 'main,
                    }
                }

                _ = ticker.tick() => {
                    if !playing { continue; }
                    let Some(dd) = &day_data else { continue };

                    sim += ChronoDuration::milliseconds((TICK_MS as f64 * speed) as i64);
                    if sim > day_end { sim = day_end; }
                    clock::set_sim(sim);
                    cursor = emit_until(&deps, dd, cursor, sim, &mut last_tick_emit);

                    // Mode « prochaine alerte » : pause dès qu'une nouvelle alerte tombe.
                    if let Some((baseline, prev_speed)) = next_alert_baseline {
                        if deps.alert_history.read().unwrap().len() > baseline {
                            next_alert_baseline = None;
                            speed = prev_speed;
                            clock::set_speed(speed);
                            playing = false;
                            let mut st = shared.status.write().unwrap();
                            st.state = "paused".into();
                            st.playing = false;
                            st.next_alert_armed = false;
                            st.speed = speed;
                        }
                    }

                    // Fin de journée.
                    if cursor >= dd.events.len() && sim >= day_end {
                        playing = false;
                        if let Some((_, prev)) = next_alert_baseline.take() {
                            speed = prev;
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
    {
        let mut ms = deps.market.write().unwrap();
        while cursor < dd.events.len() && dd.events[cursor].ts_ms <= until_ms {
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
                } => {
                    ms.on_replay_trade(symbol, *price, *size, *prints, event_time);
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
