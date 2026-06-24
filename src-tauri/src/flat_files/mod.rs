// Flat files — on-disk, shareable, offline copies of a trading day's market data.
//
// Three KINDS of data are persisted side by side under `<app_dir>/flat_files/`,
// because the strategies need very different granularities:
//
//   flat_files/
//     daily/   daily.db                ← cumulative, mirror of `daily_cache`
//     minute/  minute-YYYY-MM-DD.db    ← one file / ET day (top-2000 by volume)
//     trade/   trade-YYYY-MM-DD.db     ← one file / ET day (Micro Pullback windows)
//
//   • DAILY  — daily OHLCV bars for the WHOLE US universe, appended into a single
//     cumulative SQLite whose table is byte-identical to `local_db`'s `daily_cache`.
//     Feeds the daily context panes (Panic MR / Perfect Pullback / Backside).
//   • MINUTE — 1-minute bars 04:00→20:00 ET of the day's 2000 most-traded symbols,
//     plus the 5 previous trading days as their own minute files (so opening a chart
//     offline always has an intraday lead-in). Previous closes + news travel with
//     each day's file so it stays self-contained / shareable. This is the broad
//     replay source (synthetic 10-second slices, see `replay::data`).
//   • TRADE  — real trades AND quotes, but only inside the [alert−1min, alert+10min]
//     windows where a minute-resolution pre-scan says Micro Pullback would ignite, so
//     the engine can replay on true tick data without storing the whole day's tape.
//     Carries an as-of float snapshot (float is a filtering condition that drifts).
//
// Format choice mirrors `replay::tape`: SQLite, written atomically (to a `.tmp` then
// renamed) so an interrupted download never leaves a file that reads as "complete".
// The fetchers and the minute→slice synthesis are reused from `replay::data` so a
// flat file holds the very same data the live replay path produces.

pub mod daily;
pub mod minute;
pub mod trade;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Once, RwLock};

use chrono::NaiveDate;
use rusqlite::Connection;
use serde::Serialize;

use crate::replay::data;

/// Bumped if the on-disk schema ever changes.
pub(crate) const SCHEMA_VERSION: &str = "2";

// ─── Kinds ──────────────────────────────────────────────────────────────────────

/// The three independent flat-file datasets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Trade,
    Minute,
    Daily,
}

impl Kind {
    pub fn from_str(s: &str) -> Option<Kind> {
        match s {
            "trade" => Some(Kind::Trade),
            "minute" => Some(Kind::Minute),
            "daily" => Some(Kind::Daily),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Trade => "trade",
            Kind::Minute => "minute",
            Kind::Daily => "daily",
        }
    }
}

// ─── Paths ────────────────────────────────────────────────────────────────────

/// Root directory holding the three per-kind subfolders.
pub fn flat_dir(app_dir: &Path) -> PathBuf {
    app_dir.join("flat_files")
}

/// Subfolder for a given kind.
pub fn kind_dir(app_dir: &Path, kind: Kind) -> PathBuf {
    flat_dir(app_dir).join(kind.as_str())
}

/// Final path of the minute file for one ET day (YYYY-MM-DD).
pub fn minute_path(app_dir: &Path, day: &str) -> PathBuf {
    kind_dir(app_dir, Kind::Minute).join(format!("minute-{day}.db"))
}

/// Final path of the trade file for one ET day (YYYY-MM-DD).
pub fn trade_path(app_dir: &Path, day: &str) -> PathBuf {
    kind_dir(app_dir, Kind::Trade).join(format!("trade-{day}.db"))
}

/// The single cumulative daily file.
pub fn daily_path(app_dir: &Path) -> PathBuf {
    kind_dir(app_dir, Kind::Daily).join("daily.db")
}

/// Temp path used while a per-day file is being written (renamed on success).
pub(crate) fn tmp_path(final_path: &Path) -> PathBuf {
    let mut p = final_path.as_os_str().to_os_string();
    p.push(".tmp");
    PathBuf::from(p)
}

/// Extract the YYYY-MM-DD day from a `<prefix>-YYYY-MM-DD.db` file name.
pub(crate) fn day_of_file(name: &str, prefix: &str) -> Option<String> {
    let stem = name.strip_prefix(prefix)?.strip_suffix(".db")?;
    NaiveDate::parse_from_str(stem, "%Y-%m-%d").ok()?;
    Some(stem.to_string())
}

// ─── Layout / one-time legacy migration ───────────────────────────────────────

static LAYOUT: Once = Once::new();

/// Create the three subfolders and migrate any pre-existing root-level
/// `flat-YYYY-MM-DD.db` (the old single-kind layout) into `minute/minute-*.db`,
/// whose schema it is compatible with. Runs at most once per process; cheap to call
/// from every public entry point.
pub fn ensure_layout(app_dir: &Path) {
    LAYOUT.call_once(|| {
        for k in [Kind::Trade, Kind::Minute, Kind::Daily] {
            let _ = std::fs::create_dir_all(kind_dir(app_dir, k));
        }
        // Migrate legacy flat-*.db → minute/minute-*.db.
        let root = flat_dir(app_dir);
        if let Ok(entries) = std::fs::read_dir(&root) {
            for entry in entries.flatten() {
                if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().to_string();
                if let Some(day) = day_of_file(&name, "flat-") {
                    let dst = minute_path(app_dir, &day);
                    if !dst.exists() {
                        let _ = std::fs::rename(entry.path(), &dst);
                    }
                }
            }
        }
    });
}

// ─── Status (polled by the frontend) ───────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct FlatFilesStatus {
    pub running: bool,
    /// Which dataset is downloading: trade | minute | daily.
    pub kind: String,
    /// idle | running | done | cancelled | error
    pub state: String,
    pub current_day: Option<String>,
    pub day_index: usize,
    pub day_total: usize,
    /// 0..1 within the current unit (day, or whole-range chunk for daily).
    pub progress: f32,
    pub error: Option<String>,
    /// Last day successfully written (YYYY-MM-DD).
    pub last_done: Option<String>,
}

impl Default for FlatFilesStatus {
    fn default() -> Self {
        Self {
            running: false,
            kind: String::new(),
            state: "idle".into(),
            current_day: None,
            day_index: 0,
            day_total: 0,
            progress: 0.0,
            error: None,
            last_done: None,
        }
    }
}

/// One row of a calendar: a day present on disk for a given kind. For DAILY the rows
/// are the distinct dates covered by the cumulative file.
#[derive(Debug, Clone, Serialize)]
pub struct FlatFileDay {
    pub day: String,
    pub bytes: u64,
    /// Symbols stored (minute), symbols with windows (trade), symbols on the date (daily).
    pub symbol_count: i64,
    /// Bars (minute/daily) or trades (trade) stored.
    pub bar_count: i64,
    /// false when the file is a partial/interrupted download.
    pub complete: bool,
}

/// Shared handle stored in AppState (status polled by the UI + cancel flag).
pub struct FlatFilesShared {
    pub status: RwLock<FlatFilesStatus>,
    pub cancel: AtomicBool,
}

impl Default for FlatFilesShared {
    fn default() -> Self {
        Self { status: RwLock::new(FlatFilesStatus::default()), cancel: AtomicBool::new(false) }
    }
}

impl FlatFilesShared {
    pub fn is_running(&self) -> bool {
        self.status.read().unwrap().running
    }
    pub fn request_cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
    pub(crate) fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
    pub(crate) fn set_progress(&self, f: f32) {
        self.status.write().unwrap().progress = f;
    }
}

// ─── Availability + calendar (per kind) ────────────────────────────────────────

/// True when a COMPLETE minute file exists for `day` — the broad replay source.
/// (Kept under this name because `replay::data::load_day` calls it.)
pub fn has_day(app_dir: &Path, day: &str) -> bool {
    ensure_layout(app_dir);
    minute::has_day(app_dir, day)
}

/// Describe every day present on disk for `kind` (ascending).
pub fn calendar(app_dir: &Path, kind: Kind) -> Vec<FlatFileDay> {
    ensure_layout(app_dir);
    match kind {
        Kind::Minute => minute::calendar(app_dir),
        Kind::Trade => trade::calendar(app_dir),
        Kind::Daily => daily::calendar(app_dir),
    }
}

// ─── Reading (offline replay source) ────────────────────────────────────────────

/// Rebuild a `DayData` from the stored flat files — the offline equivalent of
/// `replay::data::load_day`'s "minutes" path. Reads the minute file (synthetic 10s
/// slices + news + prev closes) and, when a trade file exists, overlays the real
/// trades+quotes on the pre-scan windows (suppressing the synthetic slices there so
/// the same minutes aren't counted twice) so Micro Pullback replays on true ticks.
pub fn read_day(app_dir: &Path, day: &str) -> Result<data::DayData, String> {
    ensure_layout(app_dir);
    let overlay = trade::read_overlay(app_dir, day);
    minute::read_day(app_dir, day, overlay)
}

// ─── SQLite meta helpers (shared by the writers) ───────────────────────────────

pub(crate) fn set_meta(conn: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO manifest (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![key, value],
    )?;
    Ok(())
}

pub(crate) fn get_meta(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row("SELECT value FROM manifest WHERE key = ?1", [key], |r| r.get::<_, String>(0))
        .ok()
}

/// SQLite pragmas + a manifest table, shared by the per-day writers (minute/trade).
/// MEMORY journal + no sidecar files: each writer does one bulk import then the file
/// is atomically renamed into place.
pub(crate) fn writer_pragmas(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "PRAGMA journal_mode = MEMORY;
         PRAGMA synchronous = OFF;
         CREATE TABLE IF NOT EXISTS manifest (key TEXT PRIMARY KEY, value TEXT);",
    )
}

// ─── Downloading (dispatch) ─────────────────────────────────────────────────────

/// Number of weekdays (Mon–Fri) in [start, end] inclusive — the day total shown in
/// the status (holidays may still produce no file, but they are rare).
pub(crate) fn weekday_count(start: NaiveDate, end: NaiveDate) -> usize {
    let mut d = start;
    let mut n = 0;
    while d <= end {
        let wd = chrono::Datelike::weekday(&d);
        if wd != chrono::Weekday::Sat && wd != chrono::Weekday::Sun {
            n += 1;
        }
        d += chrono::Duration::days(1);
    }
    n
}

/// Start a background download of `kind` over [start_day, end_day]. Runs on the tokio
/// runtime; poll `FlatFilesShared::status`. Honours the cancel flag between units.
#[allow(clippy::too_many_arguments)]
pub fn start_download(
    shared: Arc<FlatFilesShared>,
    app_dir: PathBuf,
    db: Arc<Mutex<Connection>>,
    key: String,
    secret: String,
    massive_key: String,
    kind: Kind,
    start_day: String,
    end_day: String,
) -> Result<(), String> {
    if shared.is_running() {
        return Err("un téléchargement est déjà en cours".into());
    }
    let start = NaiveDate::parse_from_str(&start_day, "%Y-%m-%d")
        .map_err(|_| "date de début invalide".to_string())?;
    let end = NaiveDate::parse_from_str(&end_day, "%Y-%m-%d")
        .map_err(|_| "date de fin invalide".to_string())?;
    if start > end {
        return Err("la date de début est postérieure à la date de fin".into());
    }
    if key.is_empty() || secret.is_empty() {
        return Err("clés Alpaca non configurées — nécessaires pour télécharger".into());
    }
    ensure_layout(&app_dir);

    {
        let mut st = shared.status.write().unwrap();
        *st = FlatFilesStatus::default();
        st.running = true;
        st.kind = kind.as_str().into();
        st.state = "running".into();
        st.day_total = if kind == Kind::Daily { 1 } else { weekday_count(start, end) };
    }
    shared.cancel.store(false, Ordering::Relaxed);

    tauri::async_runtime::spawn(async move {
        let res = match kind {
            Kind::Daily => daily::run(&shared, &app_dir, &db, &key, &secret, start, end).await,
            Kind::Minute => {
                run_per_day(&shared, &app_dir, &db, &key, &secret, &massive_key, start, end, Kind::Minute)
                    .await
            }
            Kind::Trade => {
                run_per_day(&shared, &app_dir, &db, &key, &secret, &massive_key, start, end, Kind::Trade)
                    .await
            }
        };
        let mut st = shared.status.write().unwrap();
        st.running = false;
        st.current_day = None;
        match res {
            Ok(_) if st.state != "cancelled" => st.state = "done".into(),
            Ok(_) => {}
            Err(e) => {
                st.state = "error".into();
                st.error = Some(e);
            }
        }
    });

    Ok(())
}

/// Iterate the weekdays of [start, end], writing one per-day file (minute or trade).
/// Already-complete days are skipped. Per-day errors (holidays / no data) are recorded
/// but never abort the range.
#[allow(clippy::too_many_arguments)]
async fn run_per_day(
    shared: &Arc<FlatFilesShared>,
    app_dir: &Path,
    db: &Arc<Mutex<Connection>>,
    key: &str,
    secret: &str,
    massive_key: &str,
    start: NaiveDate,
    end: NaiveDate,
    kind: Kind,
) -> Result<(), String> {
    let mut d = start;
    let mut index = 0usize;
    while d <= end {
        if shared.cancelled() {
            shared.status.write().unwrap().state = "cancelled".into();
            return Ok(());
        }
        let wd = chrono::Datelike::weekday(&d);
        if wd == chrono::Weekday::Sat || wd == chrono::Weekday::Sun {
            d += chrono::Duration::days(1);
            continue;
        }
        let day = d.format("%Y-%m-%d").to_string();
        index += 1;
        {
            let mut st = shared.status.write().unwrap();
            st.current_day = Some(day.clone());
            st.day_index = index;
            st.progress = 0.0;
        }

        let already = match kind {
            Kind::Minute => minute::has_day(app_dir, &day),
            Kind::Trade => trade::has_day(app_dir, &day),
            Kind::Daily => false,
        };
        if already {
            let mut st = shared.status.write().unwrap();
            st.progress = 1.0;
            st.last_done = Some(day.clone());
        } else {
            let res = match kind {
                Kind::Minute => {
                    minute::write_day(shared, app_dir, db, key, secret, &day, true).await
                }
                Kind::Trade => {
                    trade::write_day(shared, app_dir, db, key, secret, massive_key, &day).await
                }
                Kind::Daily => unreachable!(),
            };
            match res {
                Ok(_) => {
                    let mut st = shared.status.write().unwrap();
                    st.progress = 1.0;
                    st.last_done = Some(day.clone());
                }
                Err(e) => {
                    eprintln!("[tagdash] flat_files {}: {day} ignoré ({e})", kind.as_str());
                    shared.status.write().unwrap().error = Some(format!("{day}: {e}"));
                }
            }
        }
        d += chrono::Duration::days(1);
    }
    Ok(())
}
