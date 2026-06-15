// Market Replay — rejoue une journée historique à travers toute la plateforme.
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
/// Speed used by « passer à la prochaine alerte » (assez rapide pour avancer,
/// assez lent pour que les moteurs évaluent chaque barre 10 s).
const NEXT_ALERT_SPEED: f64 = 10.0;
/// Min interval between pushed market-tick events per focus symbol (real ms).
const TICK_THROTTLE_MS: u128 = 100;

// ─── Status (polled by the frontend toolbar) ────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct ReplayStatus {
    pub active: bool,
    /// idle | loading | playing | paused | ended | error
    pub state: String,
    /// ET date being replayed (YYYY-MM-DD).
    pub day: Option<String>,
    /// "tape" (trades réels enregistrés) | "minutes" (barres 1 min synthétisées).
    pub source: Option<String>,
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
        Self { status: RwLock::new(ReplayStatus::default()), cmd_tx: Mutex::new(None) }
    }
}

impl ReplayShared {
    pub fn send(&self, cmd: ReplayCmd) -> Result<(), String> {
        match self.cmd_tx.lock().unwrap().as_ref() {
            Some(tx) => tx.send(cmd).map_err(|_| "replay engine stopped".to_string()),
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
    SeekClock { minutes: u32 },
    /// Avance jusqu'à la prochaine alerte scanner, puis pause.
    NextAlert,
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

// ─── Engine ─────────────────────────────────────────────────────────────────────

/// Start a replay of `day` at `start_min` ET minutes (240/420/570). Returns an
/// error when a replay is already active or the credentials are missing.
pub fn start(
    shared: Arc<ReplayShared>,
    deps: ReplayDeps,
    day: String,
    start_min: u32,
) -> Result<(), String> {
    {
        let st = shared.status.read().unwrap();
        if st.active {
            return Err("un replay est déjà actif".into());
        }
    }
    NaiveDate::parse_from_str(&day, "%Y-%m-%d").map_err(|_| "date invalide".to_string())?;
    let (key, secret) = {
        let s = deps.secrets.read().unwrap();
        match (s.alpaca_key.clone(), s.alpaca_secret.clone()) {
            (Some(k), Some(sec)) if !k.is_empty() && !sec.is_empty() => (k, sec),
            _ => return Err("clés Alpaca non configurées".into()),
        }
    };

    let (tx, rx) = mpsc::unbounded_channel::<ReplayCmd>();
    *shared.cmd_tx.lock().unwrap() = Some(tx);
    {
        let mut st = shared.status.write().unwrap();
        *st = ReplayStatus::default();
        st.active = true;
        st.state = "loading".into();
        st.day = Some(day.clone());
    }

    tauri::async_runtime::spawn(engine(shared, deps, key, secret, day, start_min, rx));
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn engine(
    shared: Arc<ReplayShared>,
    deps: ReplayDeps,
    key: String,
    secret: String,
    mut day: String,
    start_min: u32,
    mut rx: mpsc::UnboundedReceiver<ReplayCmd>,
) {
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
    macro_rules! load_day {
        ($d:expr) => {{
            let d: String = $d;
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
                &deps.app_dir, &deps.db, &key, &secret, &d,
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
                    let start_at =
                        crate::time::et_clock_utc(noon, start_min / 60, start_min % 60);
                    day_end = crate::time::et_clock_utc(noon, 20, 0);

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
                    {
                        let mut st = shared.status.write().unwrap();
                        st.state = "paused".into();
                        st.source = Some(dd.source.to_string());
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
                            shared.status.write().unwrap().state = "playing".into();
                        }
                    }
                    ReplayCmd::Pause => {
                        playing = false;
                        // Un Pause manuel désarme aussi le mode « prochaine alerte ».
                        if let Some((_, prev)) = next_alert_baseline.take() {
                            speed = prev;
                            clock::set_speed(speed);
                        }
                        let mut st = shared.status.write().unwrap();
                        st.state = "paused".into();
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
                            let mut st = shared.status.write().unwrap();
                            st.sim_time = Some(sim);
                            st.events_done = cursor;
                        }
                    }
                    ReplayCmd::SeekClock { minutes } => {
                        if let Some(dd) = &day_data {
                            let nd = NaiveDate::parse_from_str(&day, "%Y-%m-%d").unwrap();
                            let noon = data::noon_utc(nd);
                            let target = crate::time::et_clock_utc(noon, minutes / 60, minutes % 60);
                            let (ns, nc) = seek(&deps, dd, sim, cursor, target, day_end, &mut last_tick_emit);
                            sim = ns; cursor = nc;
                            let mut st = shared.status.write().unwrap();
                            st.sim_time = Some(sim);
                            st.events_done = cursor;
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
                            st.speed = speed;
                            st.next_alert_armed = true;
                        }
                    }
                    ReplayCmd::NextDay => {
                        let next = next_weekday(&day);
                        let _ = load_day!(next);
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
        if st.state == "error" { st.error.clone() } else { None }
    };
    *shared.cmd_tx.lock().unwrap() = None;
    clock::deactivate();
    clock::bump_generation();
    {
        let mut ms = deps.market.write().unwrap();
        ms.reset_data();
    }
    clear_alerts(&deps);
    // La watchlist Panic construite pendant le replay date du jour simulé —
    // purge + marqueur effacé pour que le scheduler la reconstruise en live.
    {
        let conn = deps.db.lock().unwrap();
        let _ = crate::local_db::scoring_repository::replace_all(&conn, &[]);
        let _ = crate::local_db::cache_repository::set_app_meta(&conn, "panic_watchlist_date", "");
    }
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
    for (sym, close) in &dd.prev_closes {
        ms.set_previous_close(sym, *close, now);
    }
}

fn clear_alerts(deps: &ReplayDeps) {
    deps.active_alerts.write().unwrap().clear();
    deps.alert_history.write().unwrap().clear();
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
        clock::bump_generation();
        clock::set_sim(target);
        let nc = emit_until(deps, dd, 0, target, last_tick_emit);
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
    let focus: std::collections::HashSet<String> =
        deps.focus_rx.borrow().iter().cloned().collect();
    let mut ticks: HashMap<String, TickEvent> = HashMap::new();
    {
        let mut ms = deps.market.write().unwrap();
        while cursor < dd.events.len() && dd.events[cursor].ts_ms <= until_ms {
            let te = &dd.events[cursor];
            let event_time = chrono::TimeZone::timestamp_millis_opt(&Utc, te.ts_ms)
                .single()
                .unwrap_or(until);
            match &te.ev {
                data::Event::Trade { symbol, price, size, prints } => {
                    ms.on_replay_trade(symbol, *price, *size, *prints, event_time);
                    if focus.contains(symbol) {
                        ticks.insert(symbol.clone(), TickEvent {
                            symbol: symbol.clone(),
                            price: *price,
                            ts: event_time.timestamp(),
                        });
                    }
                }
                data::Event::Quote { symbol, bid, ask } => {
                    ms.on_quote(symbol, *bid, *ask, event_time);
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
fn next_weekday(day: &str) -> String {
    let mut d = NaiveDate::parse_from_str(day, "%Y-%m-%d")
        .unwrap_or_else(|_| Utc::now().date_naive());
    loop {
        d += ChronoDuration::days(1);
        let wd = chrono::Datelike::weekday(&d);
        if wd != chrono::Weekday::Sat && wd != chrono::Weekday::Sun {
            return d.format("%Y-%m-%d").to_string();
        }
    }
}
