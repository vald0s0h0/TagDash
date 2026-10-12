// Session sweeper — copies what the trader left on a ticker onto its trades, and
// drops the sessions nothing happened in.
//
// A voice note often spans several trades on the same ticker, and a note or a
// screenshot is often taken before the first entry. Everything captured on the
// ticker during the session therefore goes to EVERY trade of the session: the typed
// missed-trade note, each transcribed voice note, each screenshot — once per
// (item, trade), tracked in `review_dispatch`, so a trade taken later in the session
// still receives what was said before it. The copies reach TradeTally through the
// usual resilient queue. A session without any trade keeps them as a MISSED trade.
//
// Runs every SWEEP_SECS. Work is gathered under the DB lock, then carried out
// without it (the note append takes the lock itself).

use std::time::Duration;

use rusqlite::{params, Connection};

use super::{SessionRow, Deps};

const SWEEP_SECS: u64 = 20;
/// Sessions older than this (by creation, real clock) are no longer swept for
/// dispatch — a replayed day can be years old, hence the real-clock bound.
const DISPATCH_HORIZON_DAYS: i64 = 3;

pub fn spawn_sweeper() {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(SWEEP_SECS)).await;
            let _ = tokio::task::spawn_blocking(sweep_once).await;
        }
    });
}

enum Work {
    Note { trade_id: String, symbol: String, text: String, item_kind: &'static str, item_id: String },
    Shot { trade_id: String, symbol: String, path: String, file_id: String, item_id: String },
}

pub fn sweep_once() {
    let Some(deps) = super::deps() else { return };
    let (work, purge) = {
        let conn = deps.db.lock().unwrap();
        let recent = super::sessions_where(
            &conn,
            "created_at >= datetime('now', ?1)",
            &[&format!("-{DISPATCH_HORIZON_DAYS} days")],
        );
        let mut work = Vec::new();
        for s in &recent {
            collect_work(&conn, s, &mut work);
        }
        (work, purgeable(&conn))
    };
    let has_creds = tt_session_creds(deps);
    for w in work {
        match w {
            Work::Note { trade_id, symbol, text, item_kind, item_id } => {
                match crate::stt::worker::append_trade_note(&deps.db, &trade_id, Some(&symbol), &text) {
                    Ok(()) => mark(deps, item_kind, &item_id, &trade_id),
                    Err(e) => eprintln!("[tagdash] review: note → {trade_id}: {e}"),
                }
            }
            Work::Shot { trade_id, symbol, path, file_id, item_id } => {
                let conn = deps.db.lock().unwrap();
                let _ = conn.execute(
                    "INSERT OR IGNORE INTO screenshot_files (id, trade_id, path, uploaded, created_at)
                     VALUES (?1, ?2, ?3, ?4, datetime('now'))",
                    params![format!("{file_id}@{trade_id}"), trade_id, path, if has_creds { 1 } else { 0 }],
                );
                if has_creds {
                    crate::tradetally::enqueue_chart_updated(&conn, &trade_id, &symbol, &path);
                }
                let _ = conn.execute(
                    "INSERT OR IGNORE INTO review_dispatch (item_kind, item_id, trade_id) VALUES ('screenshot', ?1, ?2)",
                    params![item_id, trade_id],
                );
            }
        }
    }
    if !purge.is_empty() {
        let conn = deps.db.lock().unwrap();
        for s in purge {
            eprintln!("[tagdash] review: session vide supprimée {}", s.session_id);
            super::delete_session(&conn, &s);
        }
    }
}

fn mark(deps: &Deps, kind: &str, item_id: &str, trade_id: &str) {
    let conn = deps.db.lock().unwrap();
    let _ = conn.execute(
        "INSERT OR IGNORE INTO review_dispatch (item_kind, item_id, trade_id) VALUES (?1, ?2, ?3)",
        params![kind, item_id, trade_id],
    );
}

fn dispatched(conn: &Connection, kind: &str, item_id: &str, trade_id: &str) -> bool {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM review_dispatch WHERE item_kind = ?1 AND item_id = ?2 AND trade_id = ?3)",
        params![kind, item_id, trade_id],
        |r| r.get(0),
    )
    .unwrap_or(true)
}

fn collect_work(conn: &Connection, s: &SessionRow, out: &mut Vec<Work>) {
    let trades = super::session_trade_ids(conn, s);
    if trades.is_empty() {
        return;
    }
    // Typed missed-trade note (the journal entry filed against the session id).
    let note: Option<String> = conn
        .query_row(
            "SELECT notes FROM journal_entries WHERE trade_id = ?1",
            params![s.session_id],
            |r| r.get(0),
        )
        .ok()
        .filter(|n: &String| !n.trim().is_empty());
    // Transcribed voice notes.
    let voices: Vec<(String, String)> = conn
        .prepare(
            "SELECT voice_id, cleaned_text FROM review_voice
              WHERE session_id = ?1 AND cleaned_text IS NOT NULL AND TRIM(cleaned_text) != ''
              ORDER BY start_ms",
        )
        .and_then(|mut st| {
            st.query_map(params![s.session_id], |r| Ok((r.get(0)?, r.get(1)?)))
                .map(|it| it.filter_map(|r| r.ok()).collect())
        })
        .unwrap_or_default();
    // Screenshots filed against the session.
    let shots: Vec<(String, String)> = conn
        .prepare("SELECT id, path FROM screenshot_files WHERE trade_id = ?1 ORDER BY created_at")
        .and_then(|mut st| {
            st.query_map(params![s.session_id], |r| Ok((r.get(0)?, r.get(1)?)))
                .map(|it| it.filter_map(|r| r.ok()).collect())
        })
        .unwrap_or_default();

    for t in &trades {
        if let Some(text) = &note {
            if !dispatched(conn, "note", &s.session_id, t) {
                out.push(Work::Note {
                    trade_id: t.clone(),
                    symbol: s.symbol.clone(),
                    text: text.trim().to_string(),
                    item_kind: "note",
                    item_id: s.session_id.clone(),
                });
            }
        }
        for (vid, text) in &voices {
            if !dispatched(conn, "voice", vid, t) {
                out.push(Work::Note {
                    trade_id: t.clone(),
                    symbol: s.symbol.clone(),
                    text: text.trim().to_string(),
                    item_kind: "voice",
                    item_id: vid.clone(),
                });
            }
        }
        for (fid, path) in &shots {
            let item_id = format!("{}~{fid}", s.session_id);
            if !dispatched(conn, "screenshot", &item_id, t) {
                out.push(Work::Shot {
                    trade_id: t.clone(),
                    symbol: s.symbol.clone(),
                    path: path.clone(),
                    file_id: fid.clone(),
                    item_id,
                });
            }
        }
    }
}

/// Closed sessions with no trade and nothing left on them. A live session is kept
/// until its day is over (the trader may still come back to the ticker).
fn purgeable(conn: &Connection) -> Vec<SessionRow> {
    let today = crate::time::et_date(chrono::Utc::now());
    super::sessions_where(conn, "status = 'closed'", &[])
        .into_iter()
        .filter(|s| s.mode != "live" || s.day < today)
        .filter(|s| s.mode != "backfill")
        .filter(|s| super::category(conn, s) == "empty")
        .collect()
}

fn tt_session_creds(deps: &Deps) -> bool {
    let s = deps.secrets.read().unwrap();
    s.tradetally_email.as_deref().is_some_and(|e| !e.is_empty())
        && s.tradetally_password.as_deref().is_some_and(|p| !p.is_empty())
}
