// Trade review — the recorder behind the « Revue » tab.
//
// WHAT IT RECORDS. One SESSION per (ET day, ticker, mode). It opens the moment a
// reviewed strategy (Settings ▸ Revue ▸ tags, default Micro Pullback / Apex / Dip)
// first shows the ticker, or on demand when the trader files a missed-trade note on
// a ticker nothing alerted. It closes on « Libérer ». While open:
//   • every trade print and quote of the ticker is copied, as received, into the
//     session's own SQLite file (`store`) — the live SIP feed, a live Massive
//     aggregate, or whatever the Market Replay is emitting (flat files included:
//     they are deleted by the trader once used, so the session keeps its own copy);
//   • the daily and 1-minute bars are snapshotted as context (`store::snapshot_context`);
//   • the voice recorder, the journal note and the screenshots of the ticker attach
//     to the session (`voice`, `dispatch`).
//
// CATEGORIES. A session with at least one fill is a TAKEN trade (TradeTally stays the
// source of truth, TagDash adds the tape, the voice, the ms timestamps). A session
// with no fill but a note, a voice note or a screenshot is a MISSED trade — it lives
// in TagDash only. A session with neither is dropped by the sweeper.
//
// NOTHING IS INVENTED. Speed of tape and spread are read off the recorded prints and
// quotes. A session recorded without quotes (Massive, IEX, synthetic replay slices)
// is rebuilt from Alpaca's historical SIP trades/quotes (`sip`) when it is drawn for
// review, and the rebuild is kept.
//
// CLOCK. Every *_ms value is epoch milliseconds on the MARKET clock
// (`crate::time::now()`): the simulated instant during a Market Replay, so a session
// traded in replay lines up with the replayed tape and the replayed fills.

pub mod capture;
pub mod codec;
pub mod dispatch;
pub mod outbox;
pub mod sip;
pub mod store;
pub mod timing;
pub mod voice;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

use crate::config::secrets::Secrets;
use crate::config::AppConfig;
use crate::market_state::MarketState;

/// Handles the review module needs outside of a Tauri command (scanner hook, live
/// stream, STT worker, sweeper). Installed once by `init`.
pub struct Deps {
    pub app_dir: PathBuf,
    pub db: Arc<Mutex<Connection>>,
    pub config: Arc<RwLock<AppConfig>>,
    pub secrets: Arc<RwLock<Secrets>>,
    pub market: Arc<RwLock<MarketState>>,
}

static DEPS: OnceLock<Deps> = OnceLock::new();
/// symbol → open session id, per mode. Live and replay are kept apart so a replay
/// alert on a ticker also open live never writes into the live session.
static OPEN_LIVE: OnceLock<RwLock<HashMap<String, String>>> = OnceLock::new();
static OPEN_REPLAY: OnceLock<RwLock<HashMap<String, String>>> = OnceLock::new();
/// Tag of the current Market Replay run (real epoch seconds at its first session),
/// so two replays of the same day never share a session.
static REPLAY_RUN: Mutex<Option<i64>> = Mutex::new(None);

pub fn deps() -> Option<&'static Deps> {
    DEPS.get()
}

fn open_map(replay: bool) -> &'static RwLock<HashMap<String, String>> {
    if replay {
        OPEN_REPLAY.get_or_init(|| RwLock::new(HashMap::new()))
    } else {
        OPEN_LIVE.get_or_init(|| RwLock::new(HashMap::new()))
    }
}

pub fn reviews_dir(app_dir: &Path) -> PathBuf {
    crate::paths::reviews_dir(app_dir)
}

/// Install the shared handles, restore today's open live sessions, start the capture
/// writer and the sweeper. Called once from the Tauri setup hook.
pub fn init(deps: Deps) {
    let _ = std::fs::create_dir_all(reviews_dir(&deps.app_dir));
    {
        let conn = deps.db.lock().unwrap();
        let today = crate::time::et_date(chrono::Utc::now());
        // A replay session can only be open while its replay runs; a live one only
        // during its own day. Anything else was left open by a crash or a restart.
        let _ = conn.execute(
            "UPDATE review_sessions SET status = 'closed',
                    closed_ms = COALESCE(closed_ms, opened_ms)
              WHERE status = 'open' AND (mode != 'live' OR day != ?1)",
            params![today],
        );
        let rows: Vec<(String, String)> = conn
            .prepare(
                "SELECT symbol, session_id FROM review_sessions
                  WHERE status = 'open' AND mode = 'live' AND day = ?1",
            )
            .and_then(|mut stmt| {
                stmt.query_map(params![today], |r| Ok((r.get(0)?, r.get(1)?)))
                    .map(|it| it.filter_map(|r| r.ok()).collect())
            })
            .unwrap_or_default();
        let mut map = open_map(false).write().unwrap();
        for (sym, id) in rows {
            map.insert(sym, id);
        }
    }
    if DEPS.set(deps).is_err() {
        return;
    }
    capture::init();
    dispatch::spawn_sweeper();
    outbox::spawn_worker();
}

// ─── Session identity ───────────────────────────────────────────────────────────

fn is_replay() -> bool {
    crate::replay::clock::is_active()
}

fn mode_str(replay: bool) -> &'static str {
    if replay {
        "replay"
    } else {
        "live"
    }
}

fn replay_run_tag() -> i64 {
    let mut run = REPLAY_RUN.lock().unwrap();
    *run.get_or_insert_with(|| chrono::Utc::now().timestamp())
}

fn session_id_for(day: &str, symbol: &str, mode: &str) -> String {
    let d = day.replace('-', "");
    match mode {
        "replay" => format!("RV-{d}-{symbol}-R{}", replay_run_tag()),
        "backfill" => format!("RV-{d}-{symbol}-B"),
        _ => format!("RV-{d}-{symbol}"),
    }
}

/// True for an id minted by this module (a missed-trade "trade id").
pub fn is_session_id(id: &str) -> bool {
    id.starts_with("RV-")
}

pub fn store_path(app_dir: &Path, day: &str, session_id: &str) -> PathBuf {
    reviews_dir(app_dir).join(day).join(format!("{session_id}.db"))
}

// ─── Session rows ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct SessionRow {
    pub session_id: String,
    pub day: String,
    pub symbol: String,
    pub mode: String,
    pub strategy_id: Option<String>,
    pub setup: Option<String>,
    pub alert_price: Option<f64>,
    pub opened_ms: i64,
    pub closed_ms: Option<i64>,
    pub status: String,
    pub data_source: Option<String>,
    pub enriched_ms: Option<i64>,
    pub store_path: String,
}

const SESSION_COLS: &str = "session_id, day, symbol, mode, strategy_id, setup, alert_price,
     opened_ms, closed_ms, status, data_source, enriched_ms, store_path";

fn row_to_session(r: &rusqlite::Row<'_>) -> rusqlite::Result<SessionRow> {
    Ok(SessionRow {
        session_id: r.get(0)?,
        day: r.get(1)?,
        symbol: r.get(2)?,
        mode: r.get(3)?,
        strategy_id: r.get(4)?,
        setup: r.get(5)?,
        alert_price: r.get(6)?,
        opened_ms: r.get(7)?,
        closed_ms: r.get(8)?,
        status: r.get(9)?,
        data_source: r.get(10)?,
        enriched_ms: r.get(11)?,
        // Stored relative to the user folder; every consumer wants the real file.
        store_path: crate::paths::from_stored(&r.get::<_, String>(12)?)
            .to_string_lossy()
            .into_owned(),
    })
}

pub fn get_session(conn: &Connection, session_id: &str) -> Option<SessionRow> {
    conn.query_row(
        &format!("SELECT {SESSION_COLS} FROM review_sessions WHERE session_id = ?1"),
        params![session_id],
        row_to_session,
    )
    .optional()
    .ok()
    .flatten()
}

pub fn sessions_where(conn: &Connection, clause: &str, args: &[&dyn rusqlite::ToSql]) -> Vec<SessionRow> {
    let sql = format!("SELECT {SESSION_COLS} FROM review_sessions WHERE {clause}");
    let Ok(mut stmt) = conn.prepare(&sql) else {
        return Vec::new();
    };
    stmt.query_map(args, row_to_session)
        .map(|it| it.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
}

/// The session a (day, symbol) pair already has, whatever its mode — the review
/// uses it to attach a TradeTally trade to what was recorded while it was taken.
/// Prefers the one holding the most data (live over replay over backfill on ties).
pub fn find_for_day_symbol(conn: &Connection, day: &str, symbol: &str) -> Option<SessionRow> {
    let mut rows = sessions_where(conn, "day = ?1 AND symbol = ?2", &[&day, &symbol]);
    rows.sort_by_key(|s| match s.mode.as_str() {
        "live" => 0,
        "replay" => 1,
        _ => 2,
    });
    rows.into_iter().next()
}

// ─── Open / close ───────────────────────────────────────────────────────────────

/// The open session of `symbol` in the current mode, if any.
pub fn open_session_for(symbol: &str) -> Option<String> {
    open_map(is_replay()).read().unwrap().get(symbol).cloned()
}

/// Symbols with an open LIVE session — the live feed keeps them on trades+quotes
/// even when no chart shows them (see `alpaca::stream::watch_set`).
pub fn open_live_symbols() -> Vec<String> {
    open_map(false).read().unwrap().keys().cloned().collect()
}

/// Open (or reopen) the session of `symbol` for the current mode and market day.
/// Idempotent: an already-open session is returned untouched.
pub fn open_session(
    symbol: &str,
    strategy_id: Option<&str>,
    setup: Option<&str>,
    alert_price: Option<f64>,
) -> Option<String> {
    let d = deps()?;
    let symbol = symbol.trim().to_uppercase();
    if symbol.is_empty() {
        return None;
    }
    let replay = is_replay();
    if let Some(id) = open_map(replay).read().unwrap().get(&symbol) {
        return Some(id.clone());
    }
    let now = crate::time::now();
    let now_ms = now.timestamp_millis();
    let day = crate::time::et_date(now);
    let mode = mode_str(replay);
    let session_id = session_id_for(&day, &symbol, mode);
    let path = store_path(&d.app_dir, &day, &session_id);
    {
        let conn = d.db.lock().unwrap();
        let existing = get_session(&conn, &session_id);
        match existing {
            Some(_) => {
                let _ = conn.execute(
                    "UPDATE review_sessions SET status = 'open', closed_ms = NULL,
                            strategy_id = COALESCE(strategy_id, ?2),
                            setup = COALESCE(setup, ?3),
                            alert_price = COALESCE(alert_price, ?4)
                      WHERE session_id = ?1",
                    params![session_id, strategy_id, setup, alert_price],
                );
            }
            None => {
                let _ = conn.execute(
                    "INSERT INTO review_sessions
                        (session_id, day, symbol, mode, strategy_id, setup, alert_price,
                         opened_ms, status, store_path)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'open', ?9)",
                    params![
                        session_id,
                        day,
                        symbol,
                        mode,
                        strategy_id,
                        setup,
                        alert_price,
                        now_ms,
                        crate::paths::to_stored(&path)
                    ],
                );
            }
        }
    }
    open_map(replay)
        .write()
        .unwrap()
        .insert(symbol.clone(), session_id.clone());

    // Store file, pre-window copy and context bars off the caller's path (the
    // scanner's `push_alert` holds no lock here, but it must never wait on disk).
    let (sid, sym, day2) = (session_id.clone(), symbol.clone(), day.clone());
    tauri::async_runtime::spawn_blocking(move || {
        let Some(deps) = deps() else { return };
        let path = store_path(&deps.app_dir, &day2, &sid);
        if let Err(e) = store::ensure(&path) {
            eprintln!("[tagdash] review: store {sid}: {e}");
            return;
        }
        if !replay {
            let pre_ms = deps.config.read().unwrap().review.pre_window_min.max(0) * 60_000;
            store::copy_from_live_tape(&deps.app_dir, &path, &day2, &sym, now_ms - pre_ms, now_ms);
        }
        store::snapshot_context(deps, &path, &sym, &day2);
    });
    eprintln!("[tagdash] review: session ouverte {session_id}");
    Some(session_id)
}

/// Scanner hook — every alert funnels through `scanner::push_alert`. A NEW alert
/// from a reviewed strategy opens the ticker's session.
pub fn on_alert(symbol: &str, strategy_id: &str, strategy_name: &str, price: Option<f64>) {
    let Some(deps) = deps() else { return };
    let setup = crate::strategies::setup_name(strategy_id);
    let reviewed = {
        let cfg = deps.config.read().unwrap();
        cfg.review.matches(&setup) || cfg.review.matches(strategy_name)
    };
    if !reviewed {
        return;
    }
    open_session(symbol, Some(strategy_id), Some(&setup), price);
}

/// The session a note / dictée / screenshot on `symbol` should attach to: the open
/// one, else one opened now (a missed-trade note on a ticker nothing alerted).
pub fn ensure_session(symbol: &str, strategy_id: Option<&str>) -> Option<String> {
    let symbol = symbol.trim().to_uppercase();
    if let Some(id) = open_session_for(&symbol) {
        return Some(id);
    }
    let setup = strategy_id.map(crate::strategies::setup_name);
    open_session(&symbol, strategy_id, setup.as_deref(), None)
}

/// « Libérer » — close the ticker's session in the current mode.
pub fn close_session(symbol: &str) {
    let symbol = symbol.trim().to_uppercase();
    let replay = is_replay();
    let Some(session_id) = open_map(replay).write().unwrap().remove(&symbol) else {
        return;
    };
    close_by_id(&session_id, crate::time::now().timestamp_millis());
}

fn close_by_id(session_id: &str, at_ms: i64) {
    let Some(d) = deps() else { return };
    let row = {
        let conn = d.db.lock().unwrap();
        let _ = conn.execute(
            "UPDATE review_sessions SET status = 'closed', closed_ms = ?2 WHERE session_id = ?1",
            params![session_id, at_ms],
        );
        get_session(&conn, session_id)
    };
    if let Some(s) = row {
        // Last context snapshot (the 1-minute bars now cover the whole session),
        // then pack the recorded tape.
        tauri::async_runtime::spawn_blocking(move || {
            if let Some(deps) = deps() {
                let path = Path::new(&s.store_path);
                store::snapshot_context(deps, path, &s.symbol, &s.day);
                if let Err(e) = store::compact(path) {
                    eprintln!("[tagdash] review: compactage {} : {e}", s.session_id);
                }
            }
        });
    }
    eprintln!("[tagdash] review: session fermée {session_id}");
}

/// Market Replay is stopping (still on the simulated clock): close every replay
/// session and forget the run tag.
pub fn on_replay_stop() {
    let at = crate::time::now().timestamp_millis();
    let ids: Vec<String> = open_map(true).write().unwrap().drain().map(|(_, v)| v).collect();
    for id in ids {
        close_by_id(&id, at);
    }
    *REPLAY_RUN.lock().unwrap() = None;
}

/// Record which data origin fed a session (comma list, deduplicated).
pub fn note_origin(conn: &Connection, session_id: &str, origin: &str) {
    let current: Option<String> = conn
        .query_row(
            "SELECT data_source FROM review_sessions WHERE session_id = ?1",
            params![session_id],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten()
        .flatten();
    let mut parts: Vec<String> = current
        .unwrap_or_default()
        .split(',')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    if parts.iter().any(|p| p == origin) {
        return;
    }
    parts.push(origin.to_string());
    let _ = conn.execute(
        "UPDATE review_sessions SET data_source = ?2 WHERE session_id = ?1",
        params![session_id, parts.join(",")],
    );
}

// ─── Trades of a session ────────────────────────────────────────────────────────

/// One fill of a session, from `fill_timing` (both brokers, ms precision) with the
/// internal book's `executions` as a fallback for fills recorded before it existed.
#[derive(Debug, Clone, Serialize)]
pub struct SessionFill {
    pub fill_id: String,
    pub trade_id: String,
    pub quantity: i64,
    pub price: f64,
    pub filled_ms: i64,
    pub sent_ms: Option<i64>,
    pub ack_ms: Option<i64>,
    pub recv_ms: Option<i64>,
}

/// Every fill whose TRADE started inside the session window. A trade opened during
/// the session belongs to it even if its bracket closes it after « Libérer ».
pub fn session_fills(conn: &Connection, s: &SessionRow) -> Vec<SessionFill> {
    let end = s
        .closed_ms
        .unwrap_or(i64::MAX)
        .max(s.opened_ms);
    let from = s.opened_ms - 60_000;
    let mut out: Vec<SessionFill> = Vec::new();
    if let Ok(mut stmt) = conn.prepare(
        "SELECT fill_id, trade_id, quantity, price, filled_ms, sent_ms, ack_ms, recv_ms
           FROM fill_timing
          WHERE symbol = ?1 AND trade_id IN (
                SELECT trade_id FROM fill_timing
                 WHERE symbol = ?1 GROUP BY trade_id
                HAVING MIN(filled_ms) BETWEEN ?2 AND ?3)
          ORDER BY filled_ms",
    ) {
        out = stmt
            .query_map(params![s.symbol, from, end], |r| {
                Ok(SessionFill {
                    fill_id: r.get(0)?,
                    trade_id: r.get(1)?,
                    quantity: r.get(2)?,
                    price: r.get(3)?,
                    filled_ms: r.get(4)?,
                    sent_ms: r.get(5)?,
                    ack_ms: r.get(6)?,
                    recv_ms: r.get(7)?,
                })
            })
            .map(|it| it.filter_map(|r| r.ok()).collect())
            .unwrap_or_default();
    }
    // Legacy rows (before `fill_timing`): the internal book's executions.
    let known: std::collections::HashSet<String> = out.iter().map(|f| f.fill_id.clone()).collect();
    if let Ok(mut stmt) = conn.prepare(
        "SELECT fill_id, trade_id, quantity, fill_price, filled_at FROM executions
          WHERE symbol = ?1 ORDER BY filled_at",
    ) {
        let legacy: Vec<(String, String, i64, f64, String)> = stmt
            .query_map(params![s.symbol], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
            .map(|it| it.filter_map(|r| r.ok()).collect())
            .unwrap_or_default();
        let mut first_by_trade: HashMap<String, i64> = HashMap::new();
        let parsed: Vec<(String, String, i64, f64, i64)> = legacy
            .into_iter()
            .filter_map(|(f, t, q, p, at)| {
                let ms = chrono::DateTime::parse_from_rfc3339(&at).ok()?.timestamp_millis();
                Some((f, t, q, p, ms))
            })
            .collect();
        for (_, t, _, _, ms) in &parsed {
            let e = first_by_trade.entry(t.clone()).or_insert(*ms);
            *e = (*e).min(*ms);
        }
        for (f, t, q, p, ms) in parsed {
            if known.contains(&f) {
                continue;
            }
            let first = first_by_trade.get(&t).copied().unwrap_or(ms);
            if first < from || first > end {
                continue;
            }
            out.push(SessionFill {
                fill_id: f,
                trade_id: t,
                quantity: q,
                price: p,
                filled_ms: ms,
                sent_ms: None,
                ack_ms: None,
                recv_ms: None,
            });
        }
        out.sort_by_key(|f| f.filled_ms);
    }
    out
}

/// Distinct trade ids of a session, in order of first fill.
pub fn session_trade_ids(conn: &Connection, s: &SessionRow) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    for f in session_fills(conn, s) {
        if !ids.contains(&f.trade_id) {
            ids.push(f.trade_id);
        }
    }
    ids
}

/// "taken" | "missed" | "empty".
pub fn category(conn: &Connection, s: &SessionRow) -> &'static str {
    if !session_trade_ids(conn, s).is_empty() {
        return "taken";
    }
    if has_items(conn, &s.session_id) {
        "missed"
    } else {
        "empty"
    }
}

/// Did the trader leave anything on the ticker — a typed note, a voice note, a
/// screenshot? That is what turns a session without fills into a missed trade.
pub fn has_items(conn: &Connection, session_id: &str) -> bool {
    let note: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM journal_entries WHERE trade_id = ?1 AND TRIM(notes) != '')",
            params![session_id],
            |r| r.get(0),
        )
        .unwrap_or(false);
    let voice: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM review_voice WHERE session_id = ?1)",
            params![session_id],
            |r| r.get(0),
        )
        .unwrap_or(false);
    let shot: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM screenshot_files WHERE trade_id = ?1)",
            params![session_id],
            |r| r.get(0),
        )
        .unwrap_or(false);
    note || voice || shot
}

/// Delete a session and everything it owns on disk.
pub fn delete_session(conn: &Connection, s: &SessionRow) {
    let voices: Vec<Option<String>> = conn
        .prepare("SELECT audio_path FROM review_voice WHERE session_id = ?1")
        .and_then(|mut st| {
            st.query_map(params![s.session_id], |r| r.get::<_, Option<String>>(0))
                .map(|it| it.filter_map(|r| r.ok()).collect())
        })
        .unwrap_or_default();
    for p in voices.into_iter().flatten() {
        let _ = std::fs::remove_file(crate::paths::from_stored(&p));
    }
    store::remove(Path::new(&s.store_path));
    let _ = conn.execute("DELETE FROM review_voice WHERE session_id = ?1", params![s.session_id]);
    let _ = conn.execute("DELETE FROM review_dispatch WHERE item_id LIKE ?1", params![format!("{}%", s.session_id)]);
    let _ = conn.execute("DELETE FROM review_sessions WHERE session_id = ?1", params![s.session_id]);
    for map in [open_map(false), open_map(true)] {
        map.write().unwrap().retain(|_, v| v != &s.session_id);
    }
}

// ─── Voice notes ────────────────────────────────────────────────────────────────

/// Archive a finished recording of `session_id` (16 kHz PCM). Returns the voiced
/// audio to transcribe, or `None` when nothing was said.
pub fn voice_archive_recording(
    session_id: &str,
    voice_id: &str,
    pcm: &[f32],
    start_ms: i64,
    speed: f64,
) -> Result<Option<Vec<f32>>, String> {
    let Some(deps) = deps() else {
        return Ok(Some(pcm.to_vec()));
    };
    let day = {
        let conn = deps.db.lock().unwrap();
        get_session(&conn, session_id).map(|s| s.day)
    }
    .unwrap_or_else(|| crate::time::et_date(crate::time::now()));
    let Some(a) = voice::archive(&deps.app_dir, &day, voice_id, pcm, start_ms, speed)? else {
        return Ok(None);
    };
    let chunks = serde_json::to_string(&a.chunks).map_err(|e| e.to_string())?;
    let conn = deps.db.lock().unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO review_voice
             (voice_id, session_id, start_ms, end_ms, speed, audio_path, chunks_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            voice_id,
            session_id,
            start_ms,
            a.end_ms,
            speed,
            crate::paths::to_stored(&a.path),
            chunks
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(Some(a.pcm))
}

/// Store a voice note's transcript: whisper segments (offsets in the voiced audio,
/// ms) mapped onto the market clock as subtitles, plus the Deepseek-cleaned text
/// the sweeper copies onto the session's trades.
pub fn voice_transcribed(voice_id: &str, segments: &[(i64, i64, String)], cleaned: &str) {
    let Some(deps) = deps() else { return };
    let conn = deps.db.lock().unwrap();
    let row: Option<(String, f64)> = conn
        .query_row(
            "SELECT chunks_json, speed FROM review_voice WHERE voice_id = ?1",
            params![voice_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .ok()
        .flatten();
    let Some((chunks_json, speed)) = row else { return };
    let chunks: Vec<voice::Chunk> = serde_json::from_str(&chunks_json).unwrap_or_default();
    let subs: Vec<voice::Subtitle> = segments
        .iter()
        .filter(|(_, _, t)| !t.trim().is_empty())
        .map(|(a, b, t)| voice::Subtitle {
            start_ms: voice::to_market_ms(&chunks, *a, speed),
            end_ms: voice::to_market_ms(&chunks, *b, speed),
            text: t.trim().to_string(),
        })
        .collect();
    let _ = conn.execute(
        "UPDATE review_voice SET transcript_json = ?2, cleaned_text = ?3 WHERE voice_id = ?1",
        params![voice_id, serde_json::to_string(&subs).unwrap_or_default(), cleaned],
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::local_db::schema::migrate(&conn).unwrap();
        conn
    }

    fn session(conn: &Connection, id: &str, opened: i64, closed: Option<i64>) -> SessionRow {
        conn.execute(
            "INSERT INTO review_sessions (session_id, day, symbol, mode, opened_ms, closed_ms, status, store_path)
             VALUES (?1, '2026-09-25', 'ABCD', 'live', ?2, ?3, 'closed', '/nonexistent')",
            params![id, opened, closed],
        )
        .unwrap();
        get_session(conn, id).unwrap()
    }

    fn fill(conn: &Connection, fill_id: &str, trade: &str, qty: i64, at: i64) {
        conn.execute(
            "INSERT INTO fill_timing (fill_id, trade_id, symbol, broker, quantity, price, filled_ms, recv_ms)
             VALUES (?1, ?2, 'ABCD', 'tagdash', ?3, 2.5, ?4, ?4)",
            params![fill_id, trade, qty, at],
        )
        .unwrap();
    }

    #[test]
    fn a_trade_opened_in_the_session_belongs_to_it_even_if_it_closes_after_release() {
        let conn = db();
        let s = session(&conn, "RV-20260925-ABCD", 1_000_000, Some(2_000_000));
        fill(&conn, "f1", "T1", 100, 1_500_000); // entry inside the session
        fill(&conn, "f2", "T1", -100, 2_500_000); // bracket exit after « Libérer »
        fill(&conn, "f3", "T2", 100, 3_000_000); // a later trade: another session
        let fills = session_fills(&conn, &s);
        assert_eq!(fills.iter().map(|f| f.fill_id.as_str()).collect::<Vec<_>>(), ["f1", "f2"]);
        assert_eq!(session_trade_ids(&conn, &s), ["T1"]);
        assert_eq!(category(&conn, &s), "taken");
    }

    #[test]
    fn a_session_without_fills_is_missed_only_when_something_was_left_on_it() {
        let conn = db();
        let s = session(&conn, "RV-20260925-ABCD", 1_000_000, Some(2_000_000));
        assert_eq!(category(&conn, &s), "empty");
        conn.execute(
            "INSERT INTO journal_entries (trade_id, symbol, notes) VALUES (?1, 'ABCD', 'pas pris : spread trop large')",
            params![s.session_id],
        )
        .unwrap();
        assert_eq!(category(&conn, &s), "missed");
    }

    #[test]
    fn a_sip_rebuild_keeps_the_live_sip_tape_and_replaces_the_rest() {
        let dir = std::env::temp_dir().join(format!("tagdash-review-test-{}", std::process::id()));
        let path = dir.join("s.db");
        store::remove(&path);
        let conn = store::open(&path).unwrap();
        conn.execute_batch(
            "INSERT INTO trades (ts, price, size, prints, origin) VALUES (100, 1.0, 10, 1, 'sip_live');
             INSERT INTO trades (ts, price, size, prints, origin) VALUES (500, 1.1, 900, 7, 'massive_live');",
        )
        .unwrap();
        drop(conn);
        let rest = vec![
            sip::SipTrade { ts_ms: 100, price: 1.0, size: 10, exch: None, cond: None }, // covered live
            sip::SipTrade { ts_ms: 500, price: 1.1, size: 300, exch: Some("V".into()), cond: None },
        ];
        let quotes = vec![sip::SipQuote {
            ts_ms: 450, bid: 1.09, bid_size: Some(3), ask: 1.11, ask_size: Some(2), bid_x: None, ask_x: None,
        }];
        store::replace_with_sip(&path, 0, 1_000, &rest, &quotes, &[], &[]).unwrap();
        let d = store::read_all(&path);
        assert_eq!(d.trades_ts, vec![100, 500]);
        assert_eq!(d.trades_prints, vec![1, 1], "the Massive aggregate gave way to real prints");
        assert_eq!(d.quotes_ts, vec![450]);
        let c = store::load(&path).coverage(0, 1_000);
        assert_eq!((c.real_prints, c.quotes, c.first_print, c.last_print), (2, 1, Some(100), Some(500)));
        assert!(store::enriched_covers(&path, 0, 1_000), "the rebuilt window is remembered");
        assert!(!store::enriched_covers(&path, 0, 2_000));
        assert!(!store::is_unpacked(&path), "stored packed");
        assert!(store::has_data(&path));
        store::remove(&path);
        let _ = std::fs::remove_dir(&dir);
    }
}
