// Replay data preparation — see `crate::replay_data`.
//
// Read commands never touch the network: they report what each day holds for the
// strategies the user picked, against those strategies' CURRENT settings. The one
// write command queues days for the background preparation and returns at once;
// progress is polled through `replay_data_status`.

use std::collections::HashMap;

use serde::Serialize;

use crate::replay_data::plan::DayStatus;
use crate::replay_data::prepare::{self, PrepareStatus};
use crate::replay_data::{DataProfile, EtWindow};
use crate::state::AppState;

#[derive(Debug, Clone, Serialize)]
pub struct ReplayStrategy {
    pub id: String,
    pub label: String,
    pub sessions: Vec<EtWindow>,
    pub fingerprint: String,
    /// Its live Settings toggle. A replay prepared for it runs it either way; the
    /// picker says so when it is off.
    pub enabled: bool,
}

/// The profiles of `ids` (every registered strategy when empty), from the saved
/// settings.
fn profiles(state: &tauri::State<'_, AppState>, ids: &[String]) -> Vec<DataProfile> {
    let conn = state.db.lock().unwrap();
    crate::replay_data::needs_registry()
        .into_iter()
        .filter(|n| ids.is_empty() || ids.iter().any(|i| i == n.id()))
        .map(|n| n.profile(&conn))
        .collect()
}

/// Every strategy whose replay data can be prepared.
#[tauri::command(rename_all = "snake_case", async)]
pub fn replay_data_strategies(state: tauri::State<'_, AppState>) -> Vec<ReplayStrategy> {
    let enabled = state.strategy_enabled.read().unwrap().clone();
    let conn = state.db.lock().unwrap();
    crate::replay_data::needs_registry()
        .into_iter()
        .map(|n| {
            let p = n.profile(&conn);
            ReplayStrategy {
                id: n.id().to_string(),
                label: n.label().to_string(),
                sessions: p.sessions.clone(),
                fingerprint: p.fingerprint(),
                enabled: enabled.get(n.id()).copied().unwrap_or(true),
            }
        })
        .collect()
}

/// Trades taken per ET day on a ticker one of the scanners `ids` owns (all
/// registered when empty), from the TradeTally mirror.
///
/// A trade is attributed by its SCANNER, not by its tags: an Apex or a Dip trade is
/// filed under its own setup name, yet its ticker came from Micro Pullback's
/// scanner. The id TagDash opened the trade under says which engine took it and is
/// never edited afterwards; the setup, then the tags, only stand in for a trade
/// TagDash did not place.
fn trades_by_day(conn: &rusqlite::Connection, ids: &[String]) -> HashMap<String, usize> {
    let ids: Vec<String> = if ids.is_empty() {
        crate::replay_data::needs_registry().iter().map(|n| n.id().to_string()).collect()
    } else {
        ids.to_vec()
    };
    let local_ids: HashMap<String, String> = conn
        .prepare("SELECT tt_trade_id, local_trade_id FROM tradetally_trade_ids")
        .and_then(|mut st| {
            st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .map(|rows| rows.flatten().collect())
        })
        .unwrap_or_default();
    let mut out: HashMap<String, usize> = HashMap::new();
    for t in crate::local_db::dashboard_repository::get_all_trades(conn).unwrap_or_default() {
        let marker = t
            .raw
            .get("notes")
            .and_then(|n| n.as_str())
            .and_then(crate::tradetally::local_id_from_marker);
        let scanner = local_ids
            .get(&t.tt_id)
            .cloned()
            .or(marker)
            .and_then(|id| crate::strategies::scanner_of_trade_id(&id))
            .or_else(|| t.setup.as_deref().and_then(crate::strategies::scanner_of_label))
            .or_else(|| t.tags.iter().find_map(|tag| crate::strategies::scanner_of_label(tag)));
        if !scanner.is_some_and(|s| ids.iter().any(|i| i == s)) {
            continue;
        }
        let entry = t
            .raw
            .get("entry_time")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .or(t.entry_date.clone());
        let Some(day) = entry
            .as_deref()
            .and_then(|iso| chrono::DateTime::parse_from_rfc3339(iso).ok())
            .map(|d| crate::time::et_date(d.with_timezone(&chrono::Utc)))
        else {
            continue;
        };
        *out.entry(day).or_default() += 1;
    }
    out
}

/// Status of every weekday of `month` (YYYY-MM). Opportunity lists are left out —
/// `replay_data_day` returns them for the day the user selects.
#[tauri::command(rename_all = "snake_case", async)]
pub fn replay_data_calendar(
    month: String,
    strategies: Vec<String>,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<DayStatus>, String> {
    use chrono::{Datelike, NaiveDate};
    let first = NaiveDate::parse_from_str(&format!("{month}-01"), "%Y-%m-%d")
        .map_err(|_| format!("mois invalide : {month}"))?;
    let profiles = profiles(&state, &strategies);
    let trades = trades_by_day(&state.db.lock().unwrap(), &strategies);
    let mut out = Vec::new();
    let mut d = first;
    while d.month() == first.month() {
        if !matches!(d.weekday(), chrono::Weekday::Sat | chrono::Weekday::Sun) {
            let mut s = prepare::day_status(&state.app_dir, &d.to_string(), &profiles);
            s.opportunities.clear();
            s.trades = trades.get(&s.day).copied().unwrap_or(0);
            out.push(s);
        }
        d = d.succ_opt().ok_or("date hors limites")?;
    }
    Ok(out)
}

/// Full status of one day, opportunities included.
#[tauri::command(rename_all = "snake_case", async)]
pub fn replay_data_day(
    day: String,
    strategies: Vec<String>,
    state: tauri::State<'_, AppState>,
) -> DayStatus {
    let mut s = prepare::day_status(&state.app_dir, &day, &profiles(&state, &strategies));
    s.trades = trades_by_day(&state.db.lock().unwrap(), &strategies)
        .get(&s.day)
        .copied()
        .unwrap_or(0);
    s
}

/// Queue `days` for `strategies` in the background, in the order given. `scan_only`
/// stops after the confirmation (no ticks). With `priority` the days are taken next
/// — a preparation already running steps aside for them and resumes afterwards:
/// that is how one day is started while a whole month downloads.
#[tauri::command(rename_all = "snake_case", async)]
pub fn replay_data_prepare(
    days: Vec<String>,
    strategies: Vec<String>,
    scan_only: bool,
    priority: Option<bool>,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    if state.config.read().unwrap().data_source.offline {
        return Err("hors ligne — préparation impossible".into());
    }
    let creds = crate::review::sip::creds(&state.secrets.read().unwrap())
        .ok_or("clés Alpaca non configurées")?;
    if days.is_empty() {
        return Err("aucun jour choisi".into());
    }
    let deps = prepare::PrepareDeps {
        app_dir: state.app_dir.clone(),
        db: state.db.clone(),
        creds,
    };
    prepare::enqueue(&state.replay_data, deps, days, strategies, scan_only, priority.unwrap_or(false));
    Ok(())
}

/// The Alpaca data plan the downloads are paced on.
#[derive(Debug, Clone, Serialize)]
pub struct ReplayDataBudget {
    /// "sip" (Algo Trader Plus) | "iex" (Basic, free).
    pub plan: String,
    pub requests_per_minute: u32,
}

#[tauri::command(rename_all = "snake_case", async)]
pub fn replay_data_budget() -> ReplayDataBudget {
    ReplayDataBudget {
        plan: if crate::alpaca::ratelimit::is_paid() { "sip" } else { "iex" }.into(),
        requests_per_minute: crate::alpaca::ratelimit::documented_rpm(),
    }
}

#[tauri::command(rename_all = "snake_case", async)]
pub fn replay_data_status(state: tauri::State<'_, AppState>) -> PrepareStatus {
    state.replay_data.status.read().unwrap().clone()
}

/// Stops between two requests and empties the queue; everything already written
/// stays usable.
#[tauri::command(rename_all = "snake_case", async)]
pub fn replay_data_cancel(state: tauri::State<'_, AppState>) {
    state.replay_data.request_cancel();
}

#[tauri::command(rename_all = "snake_case", async)]
pub fn open_replay_data_folder(state: tauri::State<'_, AppState>) -> Result<(), String> {
    let dir = crate::replay_data::store::replay_dir(&state.app_dir);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    crate::dashboard::open_folder(&dir)
}

#[derive(Debug, Clone, Serialize)]
pub struct ReplayDiskUsage {
    /// Days prepared for replay (`<app>/replay`).
    pub replay_bytes: u64,
    /// Legacy per-day trade files — no longer read by anything.
    pub legacy_trade_bytes: u64,
    /// Top-2000 minute files: Dislocation's RVOL profile + offline intraday charts.
    pub minute_bytes: u64,
    /// On-demand intraday chart cache (refilled when a chart opens).
    pub intraday_bytes: u64,
}

fn dir_bytes(path: &std::path::Path) -> u64 {
    std::fs::read_dir(path)
        .map(|rd| {
            rd.flatten()
                .map(|e| match e.file_type() {
                    Ok(t) if t.is_dir() => dir_bytes(&e.path()),
                    _ => e.metadata().map(|m| m.len()).unwrap_or(0),
                })
                .sum()
        })
        .unwrap_or(0)
}

fn legacy_dir(state: &tauri::State<'_, AppState>, kind: &str) -> Option<std::path::PathBuf> {
    let root = crate::flat_files::flat_dir(&state.app_dir);
    match kind {
        "trade" => Some(root.join("trade")),
        "minute" => Some(root.join("minute")),
        "intraday" => Some(root.join("intraday")),
        _ => None,
    }
}

#[tauri::command(rename_all = "snake_case", async)]
pub fn replay_data_disk_usage(state: tauri::State<'_, AppState>) -> ReplayDiskUsage {
    let root = crate::flat_files::flat_dir(&state.app_dir);
    ReplayDiskUsage {
        replay_bytes: dir_bytes(&crate::replay_data::store::replay_dir(&state.app_dir)),
        legacy_trade_bytes: dir_bytes(&root.join("trade")),
        minute_bytes: dir_bytes(&root.join("minute")),
        intraday_bytes: dir_bytes(&root.join("intraday")) + dir_bytes(&root.join("intraday5m")),
    }
}

/// Delete one legacy flat-file folder ("trade" | "minute" | "intraday") and return
/// the bytes freed. Only ever on an explicit click: it cannot be undone.
#[tauri::command(rename_all = "snake_case", async)]
pub fn purge_flat_files(kind: String, state: tauri::State<'_, AppState>) -> Result<u64, String> {
    if state.flat_files.is_running() {
        return Err("un téléchargement de flat files est en cours".into());
    }
    let dirs: Vec<std::path::PathBuf> = match kind.as_str() {
        "intraday" => {
            let root = crate::flat_files::flat_dir(&state.app_dir);
            vec![root.join("intraday"), root.join("intraday5m")]
        }
        k => vec![legacy_dir(&state, k).ok_or_else(|| format!("type inconnu : {k}"))?],
    };
    let mut freed = 0;
    for dir in dirs {
        freed += dir_bytes(&dir);
        if dir.exists() {
            std::fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
        }
    }
    // Minute and intraday folders are expected to exist by their readers.
    crate::flat_files::ensure_layout(&state.app_dir);
    for sub in ["minute", "intraday"] {
        let _ = std::fs::create_dir_all(crate::flat_files::flat_dir(&state.app_dir).join(sub));
    }
    eprintln!("[tagdash] flat files {kind} supprimés : {freed} octets libérés");
    Ok(freed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trade(conn: &rusqlite::Connection, id: &str, entry: &str, setup: &str, tags: &str, notes: &str) {
        let raw = serde_json::json!({ "entry_time": entry, "notes": notes }).to_string();
        conn.execute(
            "INSERT INTO tt_trades(tt_id, symbol, entry_date, setup, tags_json, raw_json)
             VALUES (?1, 'ABC', ?2, ?3, ?4, ?5)",
            rusqlite::params![id, entry, setup, tags, raw],
        )
        .unwrap();
    }

    /// The calendar's dot: a day counts for a strategy when a trade was taken on a
    /// ticker ITS SCANNER owns — whatever setup or tag the trade ended up filed under.
    #[test]
    fn trades_are_counted_under_the_scanner_that_owns_their_ticker() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::local_db::schema::migrate(&conn).unwrap();
        // 13:30Z = 09:30 ET: the ET day is the trade's day.
        trade(&conn, "a", "2026-06-29T13:30:00.000Z", "Apex", "[]", "");
        trade(&conn, "b", "2026-06-29T11:05:00.000Z", "Micro Pullback", "[]", "");
        trade(&conn, "c", "2026-06-29T12:00:00.000Z", "HOD Drive", "[]", "");
        // Renamed in TradeTally since: the id TagDash opened it under still says Dip.
        trade(&conn, "d", "2026-06-30T12:00:00.000Z", "Scalp du matin", "[]", "");
        conn.execute(
            "INSERT INTO tradetally_trade_ids(local_trade_id, tt_trade_id) VALUES ('260630080000-ABC-DIP-1', 'd')",
            [],
        )
        .unwrap();
        // The same through the marker TradeTally keeps in the notes.
        trade(&conn, "e", "2026-06-30T12:10:00.000Z", "", "[]", "note\n\nTagDash 260630081000-ABC-APEX-2");
        // Not placed by TagDash, no setup: a tag naming the strategy stands in.
        trade(&conn, "f", "2026-07-01T12:00:00.000Z", "", r#"["fomo","micro pullback"]"#, "");
        trade(&conn, "g", "2026-07-01T12:00:00.000Z", "", r#"["fomo"]"#, "");
        // 02:00Z is still the evening before in New York.
        trade(&conn, "h", "2026-07-03T02:00:00.000Z", "Dip", "[]", "");

        let mp = trades_by_day(&conn, &[crate::strategies::micro_pullback::ID.to_string()]);
        assert_eq!(mp.get("2026-06-29"), Some(&2), "Apex + Micro Pullback, not HOD Drive");
        assert_eq!(mp.get("2026-06-30"), Some(&2), "Dip by its id, Apex by its marker");
        assert_eq!(mp.get("2026-07-01"), Some(&1), "the tag, not the mood");
        assert_eq!(mp.get("2026-07-02"), Some(&1), "ET day, not UTC");
        assert_eq!(mp.get("2026-07-03"), None);
        // No strategy chosen = every preparable one.
        assert_eq!(trades_by_day(&conn, &[]), mp);
    }

    /// The real mirror: TAGDASH_E2E_DB=<cache.db>
    ///   cargo test --lib trade_days_dump -- --ignored --nocapture
    #[test]
    #[ignore]
    fn trade_days_dump() {
        let conn = crate::local_db::open_harness(std::path::Path::new(
            &std::env::var("TAGDASH_E2E_DB").expect("TAGDASH_E2E_DB manquant"),
        ));
        let mut days: Vec<(String, usize)> = trades_by_day(&conn, &[]).into_iter().collect();
        days.sort();
        for (day, n) in days {
            println!("{day} {n}");
        }
    }
}
