// SQLite persistence for the attention map: the rolling per-session archive, the
// short-lived frame cache and the per-day journal snapshot.
//
// Every payload is JSON under zstd. The blobs are compressed and decoded OUTSIDE
// the connection lock — the app shares one connection and a megabyte of zstd must
// never sit between a chart and its bars.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rusqlite::{params, Connection, OptionalExtension};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use super::frame::{Frame, Part, Slot};
use super::view::Snapshot;

type Db = Arc<Mutex<Connection>>;

const ZSTD_LEVEL: i32 = 6;
/// Frames kept in the cache (the live one, yesterday's, and a few replayed days).
const FRAMES_KEPT: i64 = 6;
/// Archive depth kept on disk — 60 for the medians plus room for a replay of the
/// last few weeks to find 60 sessions before ITS day.
const ARCHIVE_DAYS_KEPT: i64 = 140;
/// A snapshot with no trade session filed on its day is dropped after this long.
const SNAPSHOT_ORPHAN_DAYS: i64 = 21;

fn pack<T: Serialize>(value: &T) -> Result<Vec<u8>, String> {
    let json = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    zstd::encode_all(json.as_slice(), ZSTD_LEVEL).map_err(|e| e.to_string())
}

fn unpack<T: DeserializeOwned>(blob: &[u8]) -> Result<T, String> {
    let json = zstd::decode_all(blob).map_err(|e| e.to_string())?;
    serde_json::from_slice(&json).map_err(|e| e.to_string())
}

// ─── Archive ────────────────────────────────────────────────────────────────────

/// One session part of the archive: per symbol, `(slot within the part, dollars)`
/// for the slots that traded.
pub type ArchivePart = Vec<(String, Vec<(u8, f32)>)>;

/// Reduce a frame's universe series to the archive form of one part.
pub fn archive_of(series: &HashMap<String, Vec<Slot>>, part: Part) -> ArchivePart {
    let (lo, hi) = (part.offset(), part.offset() + part.len());
    series
        .iter()
        .filter_map(|(sym, slots)| {
            let rows: Vec<(u8, f32)> = slots
                .iter()
                .filter(|s| (lo..hi).contains(&(s.i as usize)) && s.d > 0.0)
                .map(|s| (s.i - lo as u8, s.d))
                .collect();
            (!rows.is_empty()).then(|| (sym.clone(), rows))
        })
        .collect()
}

pub fn put_archive(db: &Db, day: &str, part: Part, data: &ArchivePart) -> Result<(), String> {
    let blob = pack(data)?;
    let conn = db.lock().unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO attention_archive (day, part, symbols, payload)
         VALUES (?1, ?2, ?3, ?4)",
        params![day, part.as_str(), data.len() as i64, blob],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Days of `part` present in the archive among `days`, with their symbol count
/// (0 = known to have had no session).
pub fn archived_days(db: &Db, part: Part, days: &[String]) -> HashMap<String, i64> {
    let conn = db.lock().unwrap();
    let mut out = HashMap::new();
    let Ok(mut stmt) =
        conn.prepare("SELECT symbols FROM attention_archive WHERE day = ?1 AND part = ?2")
    else {
        return out;
    };
    for day in days {
        if let Ok(Some(n)) = stmt
            .query_row(params![day, part.as_str()], |r| r.get::<_, i64>(0))
            .optional()
        {
            out.insert(day.clone(), n);
        }
    }
    out
}

pub fn get_archive(db: &Db, day: &str, part: Part) -> Option<ArchivePart> {
    let blob: Vec<u8> = {
        let conn = db.lock().unwrap();
        conn.query_row(
            "SELECT payload FROM attention_archive WHERE day = ?1 AND part = ?2 AND symbols > 0",
            params![day, part.as_str()],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten()?
    };
    unpack(&blob).ok()
}

// ─── Frame cache ────────────────────────────────────────────────────────────────

/// Everything the engine holds for a frame, as it is cached between runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FramePayload {
    pub frame: Frame,
    /// Raw regular close of the previous session, per symbol.
    pub refs: HashMap<String, f64>,
    /// Whether `refs` was read once the daily bar was final.
    pub refs_final: bool,
    /// Split factor (new/old) for symbols whose ex-date is the target day.
    pub splits: HashMap<String, f64>,
    /// 0 = never asked, 1 = asked before the premarket, 2 = asked since.
    #[serde(default)]
    pub splits_stage: u8,
    pub series: Vec<(String, Vec<Slot>)>,
    /// Data read up to this instant, per part (epoch seconds; 0 = nothing yet).
    pub ah_seen_s: i64,
    pub pm_seen_s: i64,
    /// Part read once more in full after it ended, and archived.
    #[serde(default)]
    pub ah_settled: bool,
    #[serde(default)]
    pub pm_settled: bool,
    /// Real clock of the last read.
    #[serde(default)]
    pub fetched_s: i64,
}

pub fn put_frame(db: &Db, payload: &FramePayload) -> Result<(), String> {
    let blob = pack(payload)?;
    let conn = db.lock().unwrap();
    conn.execute(
        "INSERT INTO attention_frames (day, prev_day, payload, updated_at)
         VALUES (?1, ?2, ?3, datetime('now'))
         ON CONFLICT(day) DO UPDATE SET prev_day = excluded.prev_day,
             payload = excluded.payload, updated_at = excluded.updated_at",
        params![
            payload.frame.day.to_string(),
            payload.frame.prev_day.to_string(),
            blob
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn get_frame(db: &Db, day: &str) -> Option<FramePayload> {
    let blob: Vec<u8> = {
        let conn = db.lock().unwrap();
        conn.query_row(
            "SELECT payload FROM attention_frames WHERE day = ?1",
            params![day],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten()?
    };
    unpack(&blob).ok()
}

// ─── Journal snapshot ───────────────────────────────────────────────────────────

pub fn put_snapshot(db: &Db, snap: &Snapshot) -> Result<(), String> {
    let blob = pack(snap)?;
    let conn = db.lock().unwrap();
    conn.execute(
        "INSERT INTO attention_snapshots (day, prev_day, covered_ms, tickers, payload, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))
         ON CONFLICT(day) DO UPDATE SET prev_day = excluded.prev_day,
             covered_ms = excluded.covered_ms, tickers = excluded.tickers,
             payload = excluded.payload, updated_at = excluded.updated_at
         -- A day already recorded further (a replay stopped early, then the same
         -- day reopened at 04:00) is never overwritten by a shorter one.
         WHERE excluded.covered_ms >= attention_snapshots.covered_ms",
        params![
            snap.frame.day.to_string(),
            snap.frame.prev_day.to_string(),
            snap.seen_ms(),
            snap.tickers.len() as i64,
            blob
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn get_snapshot(db: &Db, day: &str) -> Option<Snapshot> {
    let blob: Vec<u8> = {
        let conn = db.lock().unwrap();
        conn.query_row(
            "SELECT payload FROM attention_snapshots WHERE day = ?1",
            params![day],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten()?
    };
    unpack(&blob).ok()
}

/// Days that have a snapshot, most recent first (the « Revue » can tell at once
/// whether a trade's day was recorded).
pub fn snapshot_days(db: &Db) -> Vec<String> {
    let conn = db.lock().unwrap();
    conn.prepare("SELECT day FROM attention_snapshots ORDER BY day DESC")
        .and_then(|mut s| {
            s.query_map([], |r| r.get::<_, String>(0))
                .map(|it| it.filter_map(|r| r.ok()).collect())
        })
        .unwrap_or_default()
}

// ─── Housekeeping ───────────────────────────────────────────────────────────────

/// Bound the three tables. Idempotent; run once per engine day.
pub fn prune(db: &Db, today: &str) {
    let conn = db.lock().unwrap();
    let _ = conn.execute(
        "DELETE FROM attention_frames WHERE day NOT IN
             (SELECT day FROM attention_frames ORDER BY updated_at DESC LIMIT ?1)",
        params![FRAMES_KEPT],
    );
    let _ = conn.execute(
        "DELETE FROM attention_archive WHERE day < date(?1, ?2)",
        params![today, format!("-{ARCHIVE_DAYS_KEPT} days")],
    );
    // The journal only needs the days a trade (taken or missed) was filed on.
    let _ = conn.execute(
        "DELETE FROM attention_snapshots
          WHERE day < date(?1, ?2)
            AND day NOT IN (SELECT DISTINCT day FROM review_sessions)",
        params![today, format!("-{SNAPSHOT_ORPHAN_DAYS} days")],
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Db {
        let conn = Connection::open_in_memory().unwrap();
        crate::local_db::schema::migrate(&conn).unwrap();
        Arc::new(Mutex::new(conn))
    }

    fn slot(i: u8, d: f32) -> Slot {
        Slot { i, o: 1.0, h: 1.0, l: 1.0, c: 1.0, v: 100, d, n: 3 }
    }

    #[test]
    fn archive_keeps_each_part_on_its_own_slot_numbering() {
        let mut series = HashMap::new();
        series.insert("AAA".to_string(), vec![slot(2, 500.0), slot(50, 900.0)]);
        series.insert("ZERO".to_string(), vec![slot(3, 0.0)]);
        let ah = archive_of(&series, Part::Ah);
        let pm = archive_of(&series, Part::Pm);
        assert_eq!(ah, vec![("AAA".to_string(), vec![(2, 500.0)])]);
        // Slot 50 of the frame is the 3rd premarket slot (48 after-hours slots first).
        assert_eq!(pm, vec![("AAA".to_string(), vec![(2, 900.0)])]);

        let db = db();
        put_archive(&db, "2026-10-09", Part::Ah, &ah).unwrap();
        put_archive(&db, "2026-10-08", Part::Ah, &Vec::new()).unwrap();
        assert_eq!(get_archive(&db, "2026-10-09", Part::Ah), Some(ah));
        // An empty marker is known, but is not a session.
        assert_eq!(get_archive(&db, "2026-10-08", Part::Ah), None);
        let known = archived_days(
            &db,
            Part::Ah,
            &["2026-10-09".into(), "2026-10-08".into(), "2026-10-07".into()],
        );
        assert_eq!(known.get("2026-10-09"), Some(&1));
        assert_eq!(known.get("2026-10-08"), Some(&0));
        assert!(!known.contains_key("2026-10-07"));
    }

    #[test]
    fn frame_cache_round_trips() {
        let db = db();
        let frame = Frame::for_day(chrono::NaiveDate::from_ymd_opt(2026, 10, 12).unwrap());
        let payload = FramePayload {
            frame,
            refs: HashMap::from([("AAA".to_string(), 12.5)]),
            refs_final: true,
            splits: HashMap::new(),
            splits_stage: 2,
            series: vec![("AAA".to_string(), vec![slot(0, 10.0)])],
            ah_seen_s: 1,
            pm_seen_s: 0,
            ah_settled: false,
            pm_settled: false,
            fetched_s: 2,
        };
        put_frame(&db, &payload).unwrap();
        let back = get_frame(&db, "2026-10-12").unwrap();
        assert_eq!(back.frame, frame);
        assert_eq!(back.series, payload.series);
        assert!(get_frame(&db, "2026-10-13").is_none());
    }
}
