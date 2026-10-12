// Tauri commands of the « Revue » tab and of the review recorder's UI hooks.
//
// A REVIEW ITEM is either a TradeTally trade whose tags or setup match the review
// tags (TradeTally is the source of truth: P&L, notes, executions), or a MISSED
// trade — a TagDash session with a note / voice / screenshot and no fill.
//
// Drawing order (`review_next`): never-reviewed first, then the rest; inside each
// tier, when Alpaca's historical SIP is not reachable, the items that already hold
// recorded market data come first (they can be replayed without it); random
// otherwise. There is no daily limit — « Suivant » closes the review and draws again.

use std::path::Path;

use chrono::{DateTime, Utc};
use rand::seq::SliceRandom;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::ReviewConfig;
use crate::local_db::journal_repository::{self, JournalEntry};
use crate::review::outbox::{self, append_block, split_marker};
use crate::review::{self, store, SessionRow};
use crate::state::AppState;

type State<'a> = tauri::State<'a, AppState>;

/// Context the replay shows around a trade, in bars of its timeframe.
const BARS_BEFORE: i64 = 50;
const BARS_AFTER: i64 = 10;

/// Timeframe the replay opens on, which also sizes the context it needs: 10-second
/// bars for the Micro Pullback family (Apex and Dip are its entries), 1 minute
/// otherwise. Mirrored by `defaultTf` in the frontend.
fn review_tf_secs(setup: Option<&str>) -> i64 {
    let s = setup.unwrap_or_default().trim().to_lowercase();
    if s.contains("micro") || s == "apex" || s == "dip" {
        10
    } else {
        60
    }
}

/// (start, end) of what is reviewed: the trade's entry and exit, or for a missed
/// trade the alert and the end of the last voice note left on it.
fn anchors(conn: &Connection, trade: Option<&Value>, session: Option<&SessionRow>) -> Option<(i64, i64)> {
    if let Some(t) = trade {
        if let Some(entry) = iso_ms(t.get("entry_time")) {
            let exit = iso_ms(t.get("exit_time")).unwrap_or(entry).max(entry);
            return Some((entry, exit));
        }
    }
    let s = session?;
    let voice_end: Option<i64> = conn
        .query_row(
            "SELECT MAX(end_ms) FROM review_voice WHERE session_id = ?1",
            params![s.session_id],
            |r| r.get(0),
        )
        .ok()
        .flatten();
    Some((s.opened_ms, voice_end.unwrap_or(s.opened_ms).max(s.opened_ms)))
}

/// What a SIP rebuild fetches around a trade: the review windows (Settings ▸
/// Revue), never less than the 50 bars before / 10 bars after the replay shows at
/// its opening timeframe, plus one bar of margin each side.
fn rebuild_window(cfg: &ReviewConfig, setup: Option<&str>, a: i64, b: i64) -> (i64, i64) {
    let tf_ms = review_tf_secs(setup) * 1000;
    let before = (cfg.pre_window_min.max(0) * 60_000).max(BARS_BEFORE * tf_ms) + tf_ms;
    let after = (cfg.post_window_min.max(0) * 60_000).max(BARS_AFTER * tf_ms) + tf_ms;
    (a - before, b + after)
}

// ─── Store upgrade (runs once per session file, at launch) ─────────────────────

/// Background upgrade of the session files, at launch:
///   • a config still on the old 10/10 min windows moves to 9/2 min;
///   • a SIP-rebuilt (backfill) session keeps only the new windows of the
///     TradeTally trades of its ticker's day — once;
///   • every file still holding rows is packed (see `store`).
pub fn spawn_store_upgrade(
    app_dir: std::path::PathBuf,
    db: std::sync::Arc<std::sync::Mutex<Connection>>,
    config: std::sync::Arc<std::sync::RwLock<crate::config::AppConfig>>,
) {
    std::thread::spawn(move || upgrade_stores(&app_dir, &db, &config));
}

fn upgrade_stores(
    app_dir: &Path,
    db: &std::sync::Mutex<Connection>,
    config: &std::sync::RwLock<crate::config::AppConfig>,
) {
    {
        {
            let mut c = config.write().unwrap();
            if c.review.upgrade_window_defaults() {
                if let Err(e) = crate::config::save(&crate::config::config_path(app_dir), &c) {
                    eprintln!("[tagdash] review: fenêtres par défaut non enregistrées : {e}");
                }
            }
        }
        let cfg = config.read().unwrap().review.clone();
        let sessions = {
            let conn = db.lock().unwrap();
            review::sessions_where(&conn, "1 = 1", &[])
        };
        let size = |p: &Path| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        let (mut before, mut after, mut done) = (0u64, 0u64, 0usize);
        for s in sessions {
            let path = std::path::PathBuf::from(&s.store_path);
            if !path.exists() {
                continue;
            }
            let trim = s.mode == "backfill" && !store::flag(&path, "windows_v1");
            if !trim && !store::is_unpacked(&path) {
                continue;
            }
            let was = size(&path);
            let res = if trim {
                let windows = {
                    let conn = db.lock().unwrap();
                    tt_windows(&conn, &cfg, &s.day, &s.symbol)
                };
                let r = if windows.is_empty() {
                    store::compact(&path)
                } else {
                    store::keep_windows(&path, &windows).and_then(|_| {
                        // Rebuilt with wider windows before: each new one is covered.
                        windows.iter().try_for_each(|&(a, b)| store::mark_enriched(&path, a, b))
                    })
                };
                r.and_then(|_| store::set_flag(&path, "windows_v1"))
            } else {
                store::compact(&path)
            };
            // A live session rebuilt before the windows were remembered.
            if res.is_ok() && s.mode != "backfill" && s.enriched_ms.is_some() {
                if let Some((a, b)) = store::origin_span(&store::load(&path), "sip_rest") {
                    if !store::enriched_covers(&path, a, b) {
                        let _ = store::mark_enriched(&path, a, b);
                    }
                }
            }
            match res {
                Ok(()) => {
                    before += was;
                    after += size(&path);
                    done += 1;
                }
                Err(e) => eprintln!("[tagdash] review: conversion {} : {e}", s.session_id),
            }
        }
        if done > 0 {
            eprintln!(
                "[tagdash] review: {done} session(s) converties — {:.1} Mo → {:.1} Mo",
                before as f64 / 1e6,
                after as f64 / 1e6
            );
        }
    }
}

/// Rebuild windows of every TradeTally trade of `symbol` on `day`.
fn tt_windows(conn: &Connection, cfg: &ReviewConfig, day: &str, symbol: &str) -> Vec<(i64, i64)> {
    let trades = crate::local_db::dashboard_repository::get_all_trades(conn).unwrap_or_default();
    trades
        .into_iter()
        .filter(|t| t.symbol.as_deref().is_some_and(|x| x.eq_ignore_ascii_case(symbol)))
        .filter(|t| t.entry_date.as_deref().and_then(et_day_of).as_deref() == Some(day))
        .filter_map(|t| {
            let raw = tt_raw(conn, &t.tt_id)?;
            let (a, b) = anchors(conn, Some(&raw), None)?;
            Some(rebuild_window(cfg, t.setup.as_deref(), a, b))
        })
        .collect()
}

// ─── Config ─────────────────────────────────────────────────────────────────────

#[tauri::command(rename_all = "snake_case", async)]
pub fn get_review_config(state: State<'_>) -> ReviewConfig {
    state.config.read().unwrap().review.clone()
}

#[tauri::command(rename_all = "snake_case", async)]
pub fn set_review_config(review: ReviewConfig, state: State<'_>) -> Result<(), String> {
    let path = crate::config::config_path(&state.app_dir);
    let mut cfg = state.config.read().unwrap().clone();
    cfg.review = review;
    crate::config::save(&path, &cfg)?;
    *state.config.write().unwrap() = cfg;
    Ok(())
}

// ─── Recorder hooks (chart zone) ────────────────────────────────────────────────

/// Session id of `symbol`, opening one when there is none — what the red journal /
/// camera buttons file a missed-trade note or screenshot against.
#[tauri::command(rename_all = "snake_case", async)]
pub fn review_ensure_session(symbol: String, strategy_id: Option<String>) -> Result<String, String> {
    review::ensure_session(&symbol, strategy_id.as_deref())
        .ok_or_else(|| "session de revue indisponible".to_string())
}

#[derive(Serialize)]
pub struct OpenSession {
    pub session_id: String,
    pub symbol: String,
    pub opened_ms: i64,
}

/// Open sessions of the current mode, newest first — drives the auto voice recorder.
#[tauri::command(rename_all = "snake_case", async)]
pub fn review_open_sessions(state: State<'_>) -> Vec<OpenSession> {
    let mode = if crate::replay::clock::is_active() { "replay" } else { "live" };
    let conn = state.db.lock().unwrap();
    let mut rows: Vec<OpenSession> =
        review::sessions_where(&conn, "status = 'open' AND mode = ?1", &[&mode])
            .into_iter()
            .map(|s| OpenSession {
                session_id: s.session_id,
                symbol: s.symbol,
                opened_ms: s.opened_ms,
            })
            .collect();
    rows.sort_by(|a, b| b.opened_ms.cmp(&a.opened_ms));
    rows
}

// ─── Items ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewRef {
    /// "tt" | "missed"
    pub kind: String,
    /// TradeTally id, or session id for a missed trade.
    pub id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReviewItem {
    pub kind: String,
    pub id: String,
    pub symbol: String,
    pub day: String,
    pub setup: Option<String>,
    /// The review tag that brought the item in (Settings ▸ Revue spelling).
    pub tag: Option<String>,
    pub pnl: Option<f64>,
    pub review_count: i64,
    pub has_data: bool,
    /// Entry (or alert, for a missed trade), epoch ms — orders a ticker's day.
    pub entry_ms: Option<i64>,
}

fn et_day_of(iso: &str) -> Option<String> {
    DateTime::parse_from_rfc3339(iso)
        .ok()
        .map(|d| crate::time::et_date(d.with_timezone(&Utc)))
}

fn review_count(conn: &Connection, kind: &str, id: &str) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM review_log WHERE target_kind = ?1 AND target_id = ?2",
        params![kind, id],
        |r| r.get(0),
    )
    .unwrap_or(0)
}

/// First review tag among the trade's tags, then its setup, then its strategy.
fn review_tag<'a>(
    cfg: &ReviewConfig,
    tags: impl IntoIterator<Item = &'a str>,
    setup: Option<&str>,
    strategy: Option<&str>,
) -> Option<String> {
    tags.into_iter()
        .find_map(|g| cfg.matched(g))
        .or_else(|| setup.and_then(|s| cfg.matched(s)))
        .or_else(|| strategy.and_then(|s| cfg.matched(s)))
}

fn session_has_data(s: &SessionRow) -> bool {
    store::has_data(Path::new(&s.store_path))
}

/// Every reviewable item (TradeTally trades matching the tags + missed trades).
fn all_items(conn: &Connection, cfg: &ReviewConfig) -> Vec<ReviewItem> {
    let mut out = Vec::new();
    let trades = crate::local_db::dashboard_repository::get_all_trades(conn).unwrap_or_default();
    for t in trades {
        let Some(tag) = review_tag(
            cfg,
            t.tags.iter().map(String::as_str),
            t.setup.as_deref(),
            t.strategy.as_deref(),
        ) else {
            continue;
        };
        let (Some(symbol), Some(entry)) = (t.symbol.clone(), t.entry_date.clone()) else {
            continue;
        };
        let day = et_day_of(&entry).unwrap_or_else(|| entry.chars().take(10).collect());
        let has_data = review::find_for_day_symbol(conn, &day, &symbol)
            .as_ref()
            .is_some_and(session_has_data);
        out.push(ReviewItem {
            kind: "tt".into(),
            review_count: review_count(conn, "tt", &t.tt_id),
            id: t.tt_id,
            symbol,
            day,
            setup: t.setup,
            tag: Some(tag),
            pnl: t.pnl,
            has_data,
            entry_ms: DateTime::parse_from_rfc3339(&entry).ok().map(|d| d.timestamp_millis()),
        });
    }
    for s in review::sessions_where(conn, "mode != 'backfill'", &[]) {
        if review::category(conn, &s) != "missed" {
            continue;
        }
        out.push(ReviewItem {
            kind: "missed".into(),
            review_count: review_count(conn, "missed", &s.session_id),
            has_data: session_has_data(&s),
            id: s.session_id,
            symbol: s.symbol,
            day: s.day,
            tag: s.setup.clone(),
            setup: s.setup,
            pnl: None,
            entry_ms: Some(s.opened_ms),
        });
    }
    out
}

fn sip_available(state: &State<'_>) -> bool {
    !state.config.read().unwrap().data_source.offline
        && crate::review::sip::creds(&state.secrets.read().unwrap()).is_some()
}

#[derive(Serialize)]
pub struct NextReview {
    pub item: Option<ReviewItem>,
    /// Items never reviewed, excluding the ones already seen in this sitting.
    pub remaining_unreviewed: usize,
    pub total: usize,
    pub sip_available: bool,
    /// 1-based position of the item among its ticker's trades of the day.
    pub group_pos: usize,
    pub group_len: usize,
    /// Every item of that ticker's day, chronologically (seen or not) — the
    /// replay plays them on one timeline.
    pub group: Vec<ReviewItem>,
}

/// Draw the next item to review, skipping the ones already reviewed in this sitting.
///
/// Trades are reviewed BY TICKER AND DAY, in chronological order: while the
/// group of `current` (the item on screen) still has items not seen, the next one
/// is its earliest; otherwise a group is drawn at random (unreviewed first, then
/// the ones with no tape when SIP is unavailable last) and starts at its earliest.
#[tauri::command(rename_all = "snake_case", async)]
pub fn review_next(exclude: Vec<String>, current: Option<String>, state: State<'_>) -> NextReview {
    let cfg = state.config.read().unwrap().review.clone();
    let sip = sip_available(&state);
    let items = {
        let conn = state.db.lock().unwrap();
        all_items(&conn, &cfg)
    };
    pick_next(items, &exclude, current.as_deref(), sip)
}

fn pick_next(items: Vec<ReviewItem>, exclude: &[String], current: Option<&str>, sip: bool) -> NextReview {
    let total = items.len();
    let group_of = |i: &ReviewItem| (i.day.clone(), i.symbol.clone());
    let chrono = |i: &ReviewItem| (i.entry_ms.unwrap_or(i64::MAX), i.id.clone());
    let current_group = current.and_then(|id| items.iter().find(|i| i.id == id)).map(group_of);
    let mut all_by_group = items.clone();
    all_by_group.sort_by_key(chrono);

    let mut open: Vec<ReviewItem> = items.into_iter().filter(|i| !exclude.contains(&i.id)).collect();
    let remaining_unreviewed = open.iter().filter(|i| i.review_count == 0).count();

    let pick_group = match current_group.filter(|g| open.iter().any(|i| &group_of(i) == g)) {
        Some(g) => Some(g),
        None => {
            open.shuffle(&mut rand::thread_rng());
            // Stable sort keeps the shuffle inside each tier.
            open.sort_by_key(|i| (i.review_count > 0, !sip && !i.has_data));
            open.first().map(group_of)
        }
    };
    let item = pick_group
        .as_ref()
        .and_then(|g| open.iter().filter(|i| &group_of(i) == g).min_by_key(|i| chrono(i)).cloned());
    // The ticker's day (every item of it, seen or not) and the item's place in it.
    let group: Vec<ReviewItem> = match &item {
        Some(it) => {
            let g = group_of(it);
            all_by_group.iter().filter(|i| group_of(i) == g).cloned().collect()
        }
        None => Vec::new(),
    };
    let group_pos = item
        .as_ref()
        .and_then(|it| group.iter().position(|i| i.id == it.id))
        .map_or(0, |p| p + 1);
    let group_len = group.len();
    NextReview {
        item,
        remaining_unreviewed,
        total,
        sip_available: sip,
        group_pos,
        group_len,
        group,
    }
}

// ─── Loading one item ───────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct VoiceNote {
    pub voice_id: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub speed: f64,
    pub chunks: Value,
    pub subtitles: Value,
    pub text: Option<String>,
    pub has_audio: bool,
}

#[derive(Serialize)]
pub struct ReviewPast {
    pub review_date: String,
    pub text: String,
}

#[derive(Serialize)]
pub struct ReviewDetail {
    pub item: ReviewItem,
    /// The TradeTally trade (raw upstream object) — None for a missed trade.
    pub trade: Option<Value>,
    pub session: Option<SessionRow>,
    /// Fills with millisecond timing (TagDash's own). The TradeTally executions in
    /// `trade.executions` are the fallback for trades TagDash did not place.
    pub fills: Vec<review::SessionFill>,
    pub data: store::SessionData,
    pub voices: Vec<VoiceNote>,
    /// Missed trade: the note typed on the ticker (local).
    pub missed_note: Option<String>,
    pub screenshots: Vec<String>,
    pub reviews: Vec<ReviewPast>,
    pub sip_available: bool,
    /// The recorded tape lacks quotes or real prints — a SIP rebuild would help.
    pub needs_enrichment: bool,
    /// Original SL of the TagDash trade (launch-time), for R.
    pub original_sl: Option<f64>,
}

fn tt_raw(conn: &Connection, tt_id: &str) -> Option<Value> {
    conn.query_row(
        "SELECT raw_json FROM tt_trades WHERE tt_id = ?1",
        params![tt_id],
        |r| r.get::<_, String>(0),
    )
    .optional()
    .ok()
    .flatten()
    .and_then(|s| serde_json::from_str(&s).ok())
}

fn local_ids_for_tt(conn: &Connection, tt_id: &str) -> Vec<String> {
    conn.prepare("SELECT local_trade_id FROM tradetally_trade_ids WHERE tt_trade_id = ?1")
        .and_then(|mut st| {
            st.query_map(params![tt_id], |r| r.get::<_, String>(0))
                .map(|it| it.filter_map(|r| r.ok()).collect())
        })
        .unwrap_or_default()
}

fn item_for(
    conn: &Connection,
    cfg: &ReviewConfig,
    r: &ReviewRef,
) -> Option<(ReviewItem, Option<Value>, Option<SessionRow>)> {
    if r.kind == "missed" {
        let s = review::get_session(conn, &r.id)?;
        let item = ReviewItem {
            kind: "missed".into(),
            id: s.session_id.clone(),
            symbol: s.symbol.clone(),
            day: s.day.clone(),
            setup: s.setup.clone(),
            tag: s.setup.clone(),
            pnl: None,
            review_count: review_count(conn, "missed", &s.session_id),
            has_data: session_has_data(&s),
            entry_ms: Some(s.opened_ms),
        };
        return Some((item, None, Some(s)));
    }
    let raw = tt_raw(conn, &r.id)?;
    let symbol = raw.get("symbol").and_then(Value::as_str)?.to_string();
    let entry = raw
        .get("entry_time")
        .or_else(|| raw.get("trade_date"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let day = et_day_of(&entry).unwrap_or_else(|| entry.chars().take(10).collect());
    let session = review::find_for_day_symbol(conn, &day, &symbol);
    let text = |k: &str| raw.get(k).and_then(Value::as_str).map(str::to_string);
    let (setup, strategy) = (text("setup"), text("strategy"));
    let tags: Vec<String> = raw
        .get("tags")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    let item = ReviewItem {
        kind: "tt".into(),
        id: r.id.clone(),
        symbol,
        day,
        setup: setup.clone(),
        tag: review_tag(cfg, tags.iter().map(String::as_str), setup.as_deref(), strategy.as_deref())
            .or(setup),
        pnl: raw.get("pnl").and_then(num),
        review_count: review_count(conn, "tt", &r.id),
        has_data: session.as_ref().is_some_and(session_has_data),
        entry_ms: DateTime::parse_from_rfc3339(&entry).ok().map(|d| d.timestamp_millis()),
    };
    Some((item, Some(raw), session))
}

fn num(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

fn iso_ms(v: Option<&Value>) -> Option<i64> {
    v.and_then(Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.timestamp_millis())
}

#[tauri::command(rename_all = "snake_case", async)]
pub fn review_load(target: ReviewRef, with_data: Option<bool>, state: State<'_>) -> Result<ReviewDetail, String> {
    let sip = sip_available(&state);
    let mut detail = review_load_meta(&target, &state, sip)?;
    let span = {
        let conn = state.db.lock().unwrap();
        anchors(&conn, detail.trade.as_ref(), detail.session.as_ref())
    };
    // The tape itself is read without holding the database lock (it can be tens of
    // thousands of rows).
    if let Some(s) = &detail.session {
        let path = std::path::PathBuf::from(&s.store_path);
        let tape = store::load(&path);
        // The trades of a ticker's day share one session: the replay takes the tape
        // once for the whole group (`with_data: false` for the others).
        if with_data.unwrap_or(true) {
            detail.data = store::session_data(&path, &tape);
        }
        // A SIP rebuild is worth it when THIS trade's window (50 bars before / 10
        // after — a session holds every trade of the ticker's day) lacks quotes or
        // real prints, or is not reached by the tape. Once a window was rebuilt,
        // never again: whatever SIP had is what there is.
        let tf_ms = review_tf_secs(detail.item.setup.as_deref()) * 1000;
        detail.needs_enrichment = span.is_some_and(|(a, b)| {
            let before = a - BARS_BEFORE * tf_ms;
            let after = b + BARS_AFTER * tf_ms;
            let historical = after < Utc::now().timestamp_millis() - 16 * 60_000;
            let cov = tape.coverage(before, after);
            let short = cov.first_print.map_or(true, |f| f > before + 30_000)
                || (historical && cov.last_print.map_or(true, |l| l < after - 30_000));
            (cov.quotes == 0 || cov.real_prints == 0 || short)
                && !store::enriched_covers(&path, before, after)
        });
    }
    Ok(detail)
}

fn review_load_meta(target: &ReviewRef, state: &State<'_>, sip: bool) -> Result<ReviewDetail, String> {
    let target = target.clone();
    let cfg = state.config.read().unwrap().review.clone();
    let conn = state.db.lock().unwrap();
    let (item, trade, session) = item_for(&conn, &cfg, &target).ok_or("élément introuvable")?;

    // Fills: the TagDash trades behind this TradeTally trade (ms timing), or every
    // fill of the missed trade's session (none, by definition — kept for symmetry).
    let mut fills: Vec<review::SessionFill> = Vec::new();
    let mut original_sl = None;
    if target.kind == "tt" {
        let locals = local_ids_for_tt(&conn, &target.id);
        if let Some(s) = &session {
            fills = review::session_fills(&conn, s)
                .into_iter()
                .filter(|f| locals.contains(&f.trade_id))
                .collect();
        }
        if fills.is_empty() && !locals.is_empty() {
            fills = fills_for_trades(&conn, &locals);
        }
        original_sl = locals.iter().find_map(|l| {
            conn.query_row(
                "SELECT original_sl FROM trade_levels WHERE trade_id = ?1",
                params![l],
                |r| r.get::<_, Option<f64>>(0),
            )
            .ok()
            .flatten()
        });
    }

    let (voices, missed_note, screenshots) = match &session {
        Some(s) => {
            let voices = voices_of(&conn, &s.session_id);
            let note = journal_repository::get(&conn, &s.session_id)
                .ok()
                .flatten()
                .map(|e| e.notes)
                .filter(|n| !n.trim().is_empty());
            let shots: Vec<String> = conn
                .prepare("SELECT path FROM screenshot_files WHERE trade_id = ?1 ORDER BY created_at")
                .and_then(|mut st| {
                    st.query_map(params![s.session_id], |r| r.get::<_, String>(0))
                        .map(|it| it.filter_map(|r| r.ok()).collect())
                })
                .unwrap_or_default();
            (voices, note, shots)
        }
        None => (Vec::new(), None, Vec::new()),
    };

    let reviews = conn
        .prepare(
            "SELECT review_date, text FROM review_log
              WHERE target_kind = ?1 AND target_id = ?2 ORDER BY reviewed_at",
        )
        .and_then(|mut st| {
            st.query_map(params![target.kind, target.id], |r| {
                Ok(ReviewPast {
                    review_date: r.get(0)?,
                    text: r.get(1)?,
                })
            })
            .map(|it| it.filter_map(|r| r.ok()).collect())
        })
        .unwrap_or_default();

    Ok(ReviewDetail {
        item,
        trade,
        session,
        fills,
        data: store::SessionData::default(),
        voices,
        missed_note,
        screenshots,
        reviews,
        sip_available: sip,
        needs_enrichment: true,
        original_sl,
    })
}

fn fills_for_trades(conn: &Connection, trade_ids: &[String]) -> Vec<review::SessionFill> {
    let mut out = Vec::new();
    for t in trade_ids {
        if let Ok(mut st) = conn.prepare(
            "SELECT fill_id, trade_id, quantity, price, filled_ms, sent_ms, ack_ms, recv_ms
               FROM fill_timing WHERE trade_id = ?1 ORDER BY filled_ms",
        ) {
            let rows = st.query_map(params![t], |r| {
                Ok(review::SessionFill {
                    fill_id: r.get(0)?,
                    trade_id: r.get(1)?,
                    quantity: r.get(2)?,
                    price: r.get(3)?,
                    filled_ms: r.get(4)?,
                    sent_ms: r.get(5)?,
                    ack_ms: r.get(6)?,
                    recv_ms: r.get(7)?,
                })
            });
            if let Ok(rows) = rows {
                out.extend(rows.flatten());
            }
        }
    }
    out.sort_by_key(|f| f.filled_ms);
    out
}

fn voices_of(conn: &Connection, session_id: &str) -> Vec<VoiceNote> {
    conn.prepare(
        "SELECT voice_id, start_ms, end_ms, speed, chunks_json, transcript_json, cleaned_text, audio_path
           FROM review_voice WHERE session_id = ?1 ORDER BY start_ms",
    )
    .and_then(|mut st| {
        st.query_map(params![session_id], |r| {
            let chunks: String = r.get(4)?;
            let subs: Option<String> = r.get(5)?;
            let path: Option<String> = r.get(7)?;
            Ok(VoiceNote {
                voice_id: r.get(0)?,
                start_ms: r.get(1)?,
                end_ms: r.get(2)?,
                speed: r.get(3)?,
                chunks: serde_json::from_str(&chunks).unwrap_or(Value::Array(vec![])),
                subtitles: subs
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or(Value::Array(vec![])),
                text: r.get(6)?,
                has_audio: path.is_some_and(|p| crate::paths::from_stored(&p).exists()),
            })
        })
        .map(|it| it.filter_map(|r| r.ok()).collect())
    })
    .unwrap_or_default()
}

/// Raw mu-law bytes of one voice note (decoded by the player).
#[tauri::command(rename_all = "snake_case", async)]
pub fn review_voice_audio(voice_id: String, state: State<'_>) -> Result<tauri::ipc::Response, String> {
    let path: Option<String> = {
        let conn = state.db.lock().unwrap();
        conn.query_row(
            "SELECT audio_path FROM review_voice WHERE voice_id = ?1",
            params![voice_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .flatten()
    };
    let path = crate::paths::from_stored(&path.ok_or("audio introuvable")?);
    let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
    Ok(tauri::ipc::Response::new(bytes))
}

/// One image of a TradeTally trade's gallery (a path from its `attachment_urls`),
/// raw bytes. Only gallery paths are accepted.
#[tauri::command(rename_all = "snake_case")]
pub async fn review_tt_image(path: String, state: State<'_>) -> Result<tauri::ipc::Response, String> {
    let ok = path.starts_with("/api/trades/")
        && path.contains("/images/")
        && !path.contains("..")
        && !path.contains("://");
    if !ok {
        return Err("chemin d'image TradeTally invalide".into());
    }
    // Kept on disk once fetched (a gallery file never changes): the review shows
    // the screenshots offline too, for every trade prepared while online.
    let cached = crate::paths::tt_images_dir(&state.app_dir)
        .join(path.trim_start_matches('/').replace('/', "_"));
    if let Ok(bytes) = std::fs::read(&cached) {
        return Ok(tauri::ipc::Response::new(bytes));
    }
    let client = crate::commands::tradetally_client(&state)?;
    let bytes = client.get_image_session(&path).await?;
    if let Some(dir) = cached.parent() {
        let _ = std::fs::create_dir_all(dir);
        let tmp = cached.with_extension("part");
        if std::fs::write(&tmp, &bytes).is_ok() {
            let _ = std::fs::rename(&tmp, &cached);
        }
    }
    Ok(tauri::ipc::Response::new(bytes))
}

/// A screenshot of the session, raw PNG bytes (the sidebar thumbnails).
#[tauri::command(rename_all = "snake_case", async)]
pub fn review_screenshot(path: String, state: State<'_>) -> Result<tauri::ipc::Response, String> {
    // Only files TagDash itself recorded may be read back.
    let known: bool = {
        let conn = state.db.lock().unwrap();
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM screenshot_files WHERE path = ?1)",
            params![path],
            |r| r.get(0),
        )
        .unwrap_or(false)
    };
    if !known {
        return Err("capture inconnue".into());
    }
    let bytes = std::fs::read(crate::paths::from_stored(&path)).map_err(|e| e.to_string())?;
    Ok(tauri::ipc::Response::new(bytes))
}

// ─── SIP rebuild ────────────────────────────────────────────────────────────────

/// Rebuild the item's tape from Alpaca's historical SIP (and create the session
/// when the trade was never recorded). Kept: the next load reads it from disk.
#[tauri::command(rename_all = "snake_case")]
pub async fn review_enrich(target: ReviewRef, state: State<'_>) -> Result<(), String> {
    let creds = crate::review::sip::creds(&state.secrets.read().unwrap())
        .ok_or("clés Alpaca absentes")?;
    if state.config.read().unwrap().data_source.offline {
        return Err("mode hors ligne".into());
    }
    let cfg = state.config.read().unwrap().review.clone();
    // Window: the trade (or the missed trade's alert and voice notes), with at least
    // the 50 bars before / 10 bars after the replay shows at its opening timeframe.
    let (session, symbol, day, from_ms, to_ms, anchor_start) = {
        let conn = state.db.lock().unwrap();
        let (item, trade, session) = item_for(&conn, &cfg, &target).ok_or("élément introuvable")?;
        let (a, b) = anchors(&conn, trade.as_ref(), session.as_ref()).ok_or("aucune heure d'entrée")?;
        let (from, to) = rebuild_window(&cfg, item.setup.as_deref(), a, b);
        (session, item.symbol, item.day, from, to, a)
    };
    // Alpaca serves SIP only once it is 15 minutes old on the free plan.
    let latest = Utc::now().timestamp_millis() - 16 * 60_000;
    let to_ms = to_ms.min(latest);
    if to_ms <= from_ms {
        return Err("trop récent pour l'historique SIP (15 min)".into());
    }
    let day_start = crate::time::et_clock_utc(
        DateTime::from_timestamp_millis(from_ms).unwrap_or_else(Utc::now),
        4,
        0,
    )
    .timestamp_millis();
    let day_end = (day_start + 16 * 3_600_000).min(latest);

    let trades = crate::review::sip::trades(&creds, &symbol, from_ms, to_ms).await?;
    let quotes = crate::review::sip::quotes(&creds, &symbol, from_ms, to_ms).await?;
    let bars_1m = crate::review::sip::bars(&creds, &symbol, "1Min", day_start, day_end)
        .await
        .unwrap_or_default();
    let bars_1d = crate::review::sip::bars(
        &creds,
        &symbol,
        "1Day",
        day_start - 420 * 86_400_000,
        day_start - 1,
    )
    .await
    .unwrap_or_default();

    // Session to hold it: the recorded one, else a backfill session.
    let session = match session {
        Some(s) => s,
        None => {
            let conn = state.db.lock().unwrap();
            create_backfill_session(&conn, &state.app_dir, &day, &symbol, anchor_start, to_ms)?
        }
    };
    let path = std::path::PathBuf::from(&session.store_path);
    store::ensure(&path)?;
    let (t, q) = (trades.len(), quotes.len());
    tokio::task::spawn_blocking(move || {
        store::replace_with_sip(&path, from_ms, to_ms, &trades, &quotes, &bars_1m, &bars_1d)
    })
    .await
    .map_err(|e| e.to_string())??;
    {
        let conn = state.db.lock().unwrap();
        review::note_origin(&conn, &session.session_id, "sip_rest");
        let _ = conn.execute(
            "UPDATE review_sessions SET enriched_ms = ?2 WHERE session_id = ?1",
            params![session.session_id, Utc::now().timestamp_millis()],
        );
    }
    eprintln!(
        "[tagdash] review: {} reconstruit depuis SIP ({t} prints, {q} quotes)",
        session.session_id
    );
    Ok(())
}

fn create_backfill_session(
    conn: &Connection,
    app_dir: &Path,
    day: &str,
    symbol: &str,
    opened_ms: i64,
    closed_ms: i64,
) -> Result<SessionRow, String> {
    let id = format!("RV-{}-{symbol}-B", day.replace('-', ""));
    let path = review::store_path(app_dir, day, &id);
    conn.execute(
        "INSERT OR IGNORE INTO review_sessions
            (session_id, day, symbol, mode, opened_ms, closed_ms, status, store_path)
         VALUES (?1, ?2, ?3, 'backfill', ?4, ?5, 'closed', ?6)",
        params![id, day, symbol, opened_ms, closed_ms, crate::paths::to_stored(&path)],
    )
    .map_err(|e| e.to_string())?;
    review::get_session(conn, &id).ok_or_else(|| "session backfill introuvable".into())
}

// ─── Notes ──────────────────────────────────────────────────────────────────────
//
// What the review writes onto a TradeTally trade goes through the offline outbox
// (`review::outbox`): applied locally at once, sent upstream in the background,
// kept until TradeTally accepts it. A missed trade stays local.

/// Notes of a trade without the TagDash marker line: TradeTally's fresh copy with
/// the edits still waiting to be sent applied over it — the local copy when
/// TradeTally cannot be reached (offline, timeout).
#[tauri::command(rename_all = "snake_case")]
pub async fn review_get_notes(target: ReviewRef, state: State<'_>) -> Result<String, String> {
    if target.kind == "missed" {
        let conn = state.db.lock().unwrap();
        return Ok(journal_repository::get(&conn, &target.id)
            .ok()
            .flatten()
            .map(|e| e.notes)
            .unwrap_or_default());
    }
    match outbox::fetch_notes_quick(&target.id).await {
        Ok(server) => {
            let user = split_marker(&server).0;
            let ops = {
                let conn = state.db.lock().unwrap();
                outbox::ops_for(&conn, &target.id)
            };
            if ops.is_empty() {
                Ok(user)
            } else {
                // Reachable again with edits waiting: send them now.
                outbox::kick();
                Ok(outbox::fold(&user, &ops))
            }
        }
        Err(_) => {
            let conn = state.db.lock().unwrap();
            Ok(outbox::local_notes(&conn, &target.id))
        }
    }
}

/// Save the notes edited in the sidebar (typos, rewording). A TradeTally trade's
/// edit is queued for upstream (marker preserved at send time); a missed trade
/// stays local.
#[tauri::command(rename_all = "snake_case")]
pub async fn review_save_notes(target: ReviewRef, notes: String, state: State<'_>) -> Result<(), String> {
    if target.kind == "missed" {
        return save_missed_note(&state, &target.id, &notes);
    }
    {
        let conn = state.db.lock().unwrap();
        outbox::enqueue(&conn, &target.id, "set_notes", &notes)?;
    }
    outbox::kick();
    Ok(())
}

fn save_missed_note(state: &State<'_>, session_id: &str, notes: &str) -> Result<(), String> {
    let conn = state.db.lock().unwrap();
    let symbol = review::get_session(&conn, session_id)
        .map(|s| s.symbol)
        .unwrap_or_default();
    let existing = journal_repository::get(&conn, session_id).ok().flatten();
    journal_repository::save(
        &conn,
        &JournalEntry {
            trade_id: session_id.to_string(),
            symbol,
            notes: notes.to_string(),
            confidence: existing.as_ref().and_then(|e| e.confidence),
            tags: existing.map(|e| e.tags).unwrap_or_default(),
            updated_at: String::new(),
        },
    )
    .map_err(|e| e.to_string())
}

/// « Suivant » — close the review. A non-empty review text is appended at the end of
/// the notes as `Review YYYY-MM-DD : …`, below any earlier reviews, never replacing
/// them (on a TradeTally trade: queued, appended to the server's notes as they read
/// when it is sent). The review is logged locally at once. Returns the notes as
/// they now read locally.
#[tauri::command(rename_all = "snake_case")]
pub async fn review_complete(target: ReviewRef, text: String, state: State<'_>) -> Result<String, String> {
    let today = crate::time::et_date(Utc::now());
    let text = text.trim().to_string();
    let block = format!("Review {today} : {text}");
    let notes_now = if text.is_empty() {
        None
    } else if target.kind == "missed" {
        let current = {
            let conn = state.db.lock().unwrap();
            journal_repository::get(&conn, &target.id)
                .ok()
                .flatten()
                .map(|e| e.notes)
                .unwrap_or_default()
        };
        let next = append_block(&current, &block);
        save_missed_note(&state, &target.id, &next)?;
        Some(next)
    } else {
        let conn = state.db.lock().unwrap();
        Some(outbox::enqueue(&conn, &target.id, "append", &block)?)
    };
    {
        let conn = state.db.lock().unwrap();
        conn.execute(
            "INSERT INTO review_log (review_id, target_kind, target_id, review_date, reviewed_at, text)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                format!("rvw-{}-{}", Utc::now().timestamp_millis(), target.id),
                target.kind,
                target.id,
                today,
                Utc::now().to_rfc3339(),
                text
            ],
        )
        .map_err(|e| e.to_string())?;
    }
    if target.kind != "missed" && !text.is_empty() {
        outbox::kick();
    }
    Ok(notes_now.unwrap_or_default())
}

// ─── Offline outbox ─────────────────────────────────────────────────────────────

/// What waits to reach TradeTally, for the sidebar's sync indicator.
#[tauri::command(rename_all = "snake_case", async)]
pub fn review_outbox_status(state: State<'_>) -> outbox::OutboxStatus {
    let offline = state.config.read().unwrap().data_source.offline;
    let conn = state.db.lock().unwrap();
    outbox::status(&conn, offline)
}

/// « Réessayer » — send what waits now, and say where it stands after.
#[tauri::command(rename_all = "snake_case")]
pub async fn review_outbox_flush(state: State<'_>) -> Result<outbox::OutboxStatus, String> {
    outbox::flush().await;
    Ok(review_outbox_status(state))
}

/// Give up on what waits for one trade (TradeTally keeps refusing it).
#[tauri::command(rename_all = "snake_case", async)]
pub fn review_outbox_discard(tt_id: String, state: State<'_>) -> Result<(), String> {
    let conn = state.db.lock().unwrap();
    outbox::discard(&conn, &tt_id)
}

// ─── Settings ▸ Trades DB ───────────────────────────────────────────────────────

/// One row of the trades table: the TradeTally mirror plus what TagDash adds, and
/// the missed trades (TagDash only).
#[derive(Serialize)]
pub struct ReviewTradeRow {
    /// "taken" | "missed"
    pub category: String,
    /// "tradetally" | "tagdash"
    pub source: String,
    pub id: String,
    pub symbol: String,
    pub day: String,
    pub side: Option<String>,
    pub setup: Option<String>,
    pub tags: Vec<String>,
    pub quantity: Option<f64>,
    pub entry_price: Option<f64>,
    pub exit_price: Option<f64>,
    pub pnl: Option<f64>,
    pub r_value: Option<f64>,
    pub entry_ms: Option<i64>,
    pub exit_ms: Option<i64>,
    /// First fill, TagDash ms timing (latency work).
    pub first_fill_ms: Option<i64>,
    pub first_sent_ms: Option<i64>,
    pub first_ack_ms: Option<i64>,
    pub first_recv_ms: Option<i64>,
    pub local_trade_ids: Vec<String>,
    pub session_id: Option<String>,
    /// Comma list of data origins recorded for the ticker that day.
    pub data_source: Option<String>,
    pub voice_notes: i64,
    pub screenshots: i64,
    pub has_note: bool,
    pub review_count: i64,
    pub last_review: Option<String>,
}

#[tauri::command(rename_all = "snake_case", async)]
pub fn get_review_trades_db(state: State<'_>) -> Vec<ReviewTradeRow> {
    let conn = state.db.lock().unwrap();
    let mut out = Vec::new();
    let trades = crate::local_db::dashboard_repository::get_all_trades(&conn).unwrap_or_default();
    for t in trades {
        let raw = &t.raw;
        let symbol = t.symbol.clone().unwrap_or_default();
        let entry_ms = iso_ms(raw.get("entry_time"));
        let day = t
            .entry_date
            .as_deref()
            .and_then(et_day_of)
            .unwrap_or_default();
        let session = review::find_for_day_symbol(&conn, &day, &symbol);
        let locals = local_ids_for_tt(&conn, &t.tt_id);
        let first = fills_for_trades(&conn, &locals).into_iter().next();
        let (voice_notes, screenshots) = session
            .as_ref()
            .map(|s| counts(&conn, &s.session_id))
            .unwrap_or((0, 0));
        let (review_count, last_review) = review_stats(&conn, "tt", &t.tt_id);
        out.push(ReviewTradeRow {
            category: "taken".into(),
            source: "tradetally".into(),
            symbol,
            day,
            side: t.side.clone(),
            setup: t.setup.clone(),
            tags: t.tags.clone(),
            quantity: t.quantity,
            entry_price: t.entry_price,
            exit_price: t.exit_price,
            pnl: t.pnl,
            r_value: raw.get("r_value").and_then(num),
            entry_ms,
            exit_ms: iso_ms(raw.get("exit_time")),
            first_fill_ms: first.as_ref().map(|f| f.filled_ms),
            first_sent_ms: first.as_ref().and_then(|f| f.sent_ms),
            first_ack_ms: first.as_ref().and_then(|f| f.ack_ms),
            first_recv_ms: first.as_ref().and_then(|f| f.recv_ms),
            local_trade_ids: locals,
            session_id: session.as_ref().map(|s| s.session_id.clone()),
            data_source: session.as_ref().and_then(|s| s.data_source.clone()),
            voice_notes,
            screenshots,
            has_note: raw
                .get("notes")
                .and_then(Value::as_str)
                .is_some_and(|n| !split_marker(n).0.trim().is_empty()),
            review_count,
            last_review,
            id: t.tt_id,
        });
    }
    for s in review::sessions_where(&conn, "mode != 'backfill'", &[]) {
        if review::category(&conn, &s) != "missed" {
            continue;
        }
        let (voice_notes, screenshots) = counts(&conn, &s.session_id);
        let (review_count, last_review) = review_stats(&conn, "missed", &s.session_id);
        let has_note = review::has_items(&conn, &s.session_id)
            && journal_repository::get(&conn, &s.session_id)
                .ok()
                .flatten()
                .is_some_and(|e| !e.notes.trim().is_empty());
        out.push(ReviewTradeRow {
            category: "missed".into(),
            source: "tagdash".into(),
            id: s.session_id.clone(),
            symbol: s.symbol.clone(),
            day: s.day.clone(),
            side: None,
            setup: s.setup.clone(),
            tags: Vec::new(),
            quantity: None,
            entry_price: s.alert_price,
            exit_price: None,
            pnl: None,
            r_value: None,
            entry_ms: Some(s.opened_ms),
            exit_ms: s.closed_ms,
            first_fill_ms: None,
            first_sent_ms: None,
            first_ack_ms: None,
            first_recv_ms: None,
            local_trade_ids: Vec::new(),
            session_id: Some(s.session_id.clone()),
            data_source: s.data_source.clone(),
            voice_notes,
            screenshots,
            has_note,
            review_count,
            last_review,
        });
    }
    out.sort_by(|a, b| b.entry_ms.cmp(&a.entry_ms));
    out
}

fn counts(conn: &Connection, session_id: &str) -> (i64, i64) {
    let v = conn
        .query_row(
            "SELECT COUNT(*) FROM review_voice WHERE session_id = ?1",
            params![session_id],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let s = conn
        .query_row(
            "SELECT COUNT(*) FROM screenshot_files WHERE trade_id = ?1",
            params![session_id],
            |r| r.get(0),
        )
        .unwrap_or(0);
    (v, s)
}

fn review_stats(conn: &Connection, kind: &str, id: &str) -> (i64, Option<String>) {
    conn.query_row(
        "SELECT COUNT(*), MAX(review_date) FROM review_log WHERE target_kind = ?1 AND target_id = ?2",
        params![kind, id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .unwrap_or((0, None))
}

/// Delete a missed trade (its session, tape copy, voice notes, note, screenshots).
#[tauri::command(rename_all = "snake_case", async)]
pub fn review_delete_missed(session_id: String, state: State<'_>) -> Result<(), String> {
    let conn = state.db.lock().unwrap();
    let s = review::get_session(&conn, &session_id).ok_or("session introuvable")?;
    if review::category(&conn, &s) == "taken" {
        return Err("ce ticker a des exécutions : ce n'est pas un trade manqué".into());
    }
    let _ = conn.execute("DELETE FROM journal_entries WHERE trade_id = ?1", params![session_id]);
    let _ = conn.execute("DELETE FROM screenshot_files WHERE trade_id = ?1", params![session_id]);
    let _ = conn.execute(
        "DELETE FROM review_log WHERE target_kind = 'missed' AND target_id = ?1",
        params![session_id],
    );
    review::delete_session(&conn, &s);
    Ok(())
}

#[cfg(test)]
mod tests {
    /// Dry run of the launch upgrade on a COPY of a real data root:
    /// `TAGDASH_HOME=/path/to/copy cargo test --lib real_store_upgrade -- --ignored --nocapture`
    /// `TAGDASH_HOME` is what makes the session paths resolve inside the copy;
    /// without it this would rewrite the REAL stores, so the test refuses to run.
    #[test]
    #[ignore]
    fn real_store_upgrade() {
        assert!(
            std::env::var_os(crate::paths::HOME_ENV).is_some(),
            "TAGDASH_HOME manquant : ce test réécrit les sessions, il lui faut une copie"
        );
        let dir = crate::config::app_dir();
        crate::user_data::layout::prepare(&dir).unwrap();
        let conn = crate::local_db::open_and_migrate(&dir).unwrap();
        let sessions = crate::review::sessions_where(&conn, "1 = 1", &[]);
        let before: Vec<_> = sessions
            .iter()
            .map(|s| super::store::load(std::path::Path::new(&s.store_path)))
            .collect();
        let db = std::sync::Mutex::new(conn);
        let mut cfg = crate::config::AppConfig::default();
        cfg.review.pre_window_min = 10;
        cfg.review.post_window_min = 10;
        cfg.review.window_defaults = 0;
        let config = std::sync::RwLock::new(cfg);
        super::upgrade_stores(&dir, &db, &config);
        assert_eq!(config.read().unwrap().review.post_window_min, 2);
        for (s, old) in sessions.iter().zip(before) {
            let path = std::path::Path::new(&s.store_path);
            let new = super::store::load(path);
            // Every row kept is identical to the original one.
            let mut i = 0;
            for t in &new.trades {
                while i < old.trades.len() && old.trades[i] != *t {
                    i += 1;
                }
                assert!(i < old.trades.len(), "{}: print altered or invented", s.session_id);
                i += 1;
            }
            println!(
                "{} : {} → {} prints, {} → {} quotes, {:.2} Mo",
                s.session_id,
                old.trades.len(),
                new.trades.len(),
                old.quotes.len(),
                new.quotes.len(),
                std::fs::metadata(path).map(|m| m.len()).unwrap_or(0) as f64 / 1e6
            );
            assert!(!super::store::is_unpacked(path));
        }
        // Second launch: nothing left to do.
        super::upgrade_stores(&dir, &db, &config);
    }

    fn it(id: &str, day: &str, sym: &str, ms: i64, reviewed: bool) -> super::ReviewItem {
        super::ReviewItem {
            kind: "tt".into(),
            id: id.into(),
            symbol: sym.into(),
            day: day.into(),
            setup: None,
            tag: None,
            pnl: None,
            review_count: reviewed as i64,
            has_data: true,
            entry_ms: Some(ms),
        }
    }

    #[test]
    fn next_follows_the_ticker_day_in_order() {
        let items = || {
            vec![
                it("b2", "2026-09-01", "AAA", 300, false),
                it("x1", "2026-09-02", "BBB", 100, true),
                it("b1", "2026-09-01", "AAA", 200, false),
                it("b3", "2026-09-01", "AAA", 400, false),
            ]
        };
        // A random draw lands on an unreviewed group, at its earliest trade.
        let first = super::pick_next(items(), &[], None, true);
        assert_eq!(first.item.as_ref().unwrap().id, "b1");
        assert_eq!((first.group_pos, first.group_len), (1, 3));
        let ids: Vec<&str> = first.group.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["b1", "b2", "b3"], "the whole ticker's day, chronologically");
        // Then the rest of the group, chronologically.
        let n = super::pick_next(items(), &["b1".into()], Some("b1"), true);
        assert_eq!(n.item.as_ref().unwrap().id, "b2");
        assert_eq!(n.group_pos, 2);
        let n = super::pick_next(items(), &["b1".into(), "b2".into()], Some("b2"), true);
        assert_eq!(n.item.as_ref().unwrap().id, "b3");
        // Group exhausted → another group.
        let n = super::pick_next(items(), &["b1".into(), "b2".into(), "b3".into()], Some("b3"), true);
        assert_eq!(n.item.as_ref().unwrap().id, "x1");
        assert_eq!((n.group_pos, n.group_len), (1, 1));
    }

    use super::*;
    use crate::review::outbox::join_marker;

    #[test]
    fn the_marker_survives_an_edit_and_reviews_stack() {
        let notes = "Entrée trop tôt.\n\nTagDash T-123";
        let (text, markers) = split_marker(notes);
        assert_eq!(text, "Entrée trop tôt.");
        assert_eq!(markers, vec!["TagDash T-123".to_string()]);
        let once = append_block(&text, "Review 2026-09-26 : ok");
        let twice = append_block(&once, "Review 2026-09-27 : mieux");
        assert!(twice.ends_with("Review 2026-09-26 : ok\n\nReview 2026-09-27 : mieux"));
        assert_eq!(
            join_marker(&twice, &markers),
            format!("{twice}\n\nTagDash T-123")
        );
    }
}
