// Panic Mean Reversion watchlist scheduling.
//
// This module owns WHEN the day's watchlist gets built; the build itself lives in
// `crate::scoring::build_and_store`. Everything goes through ONE entry point,
// `ensure_day`, because three callers can race for it: the background scheduler, the
// Market Replay engine loading a day, and the manual rebuild command.
//
// The list refreshes every five simulated ET minutes from 04:00; 09:25 is the final
// promotion point before the regular open.
//
// STATE LIVES IN THE DATABASE, not in memory. `panic_watchlist_build` records one row
// per day (when, how many rows, attempts, whether a replay clock produced it, the
// replay generation). The previous design kept an in-memory `built_day`, which made
// the live list unrecoverable for the rest of the session after replaying today's
// date — the ledger has no such failure mode, and a restart resumes exactly where it
// left off.
//
// DAY-CORRECTNESS is structural, not defensive: rows are keyed by day and every read
// filters on it, so a list built for day X is simply invisible while the clock is on
// day Y. A build that finishes after the clock moved therefore degrades to "shows
// nothing" instead of "shows the wrong day".

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

use crate::config::secrets::Secrets;
use crate::local_db::cache_repository;
use crate::local_db::scoring_repository::{self, BuildLedger};

/// ET wall-clock minute at which the day's watchlist is built (09:25 ET).
pub const TRIGGER_MIN: u32 = 9 * 60 + 25;
/// Premarket shocks are live opportunities, not a 09:25-only input. Snapshot-based
/// discovery keeps this refresh cheap while detailed minute bars stay candidate-only.
const LIVE_REFRESH_MIN: u32 = 5;
const PREMARKET_START_MIN: u32 = 4 * 60;
/// Idle poll interval; every event that can invalidate the list also sends a kick,
/// so this is only a safety net.
const LOOP_SECS: u64 = 30;
/// Don't attempt a build until the daily cache holds at least this many symbols —
/// avoids racing the startup pipeline's daily-bar load on a fresh launch.
const MIN_CACHE_SYMBOLS: i64 = 100;
/// Retry budget when a build produced 0 rows because its INPUTS were missing.
const MAX_ATTEMPTS: u32 = 3;
/// Simulated minutes between two such retries.
const RETRY_GAP_MIN: u32 = 10;
/// Real-time floor between two automatic builds of the same day. At 600× replay speed
/// `RETRY_GAP_MIN` elapses in a second, so the attempt cap alone would still allow a
/// burst of premarket fetches.
const MIN_REAL_GAP: Duration = Duration::from_secs(60);

/// Process-wide exclusion for the heavy build. `tokio::sync::Mutex` because the
/// critical section spans an `.await` (the premarket fetch) — a std guard would make
/// the future `!Send` and fail to spawn.
static BUILD_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
/// Wakes the scheduler out of its idle sleep (replay load / seek / stop).
static KICK: OnceLock<tokio::sync::Notify> = OnceLock::new();
/// Real-time stamp of the last completed automatic build.
static LAST_BUILD: Mutex<Option<Instant>> = Mutex::new(None);

fn kick() -> &'static tokio::sync::Notify {
    KICK.get_or_init(tokio::sync::Notify::new)
}

/// Ask the scheduler to re-evaluate immediately. `notify_one` stores a permit when
/// nobody is waiting, so a kick sent while a build is in flight is not lost.
pub fn request_build() {
    kick().notify_one();
}

/// How hard the caller wants the build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Force {
    /// Build only if the ledger says it is needed (scheduler, replay load).
    Auto,
    /// The user asked explicitly — queue behind any in-flight build, then rebuild.
    Rebuild,
}

#[derive(Debug, Clone)]
pub enum EnsureResult {
    Built {
        day: String,
        rows: usize,
    },
    UpToDate,
    /// Another build is already running (Auto only — never queues a duplicate).
    Busy,
    /// The daily cache isn't loaded enough yet. NOT recorded as an attempt.
    NotReady,
    Failed(String),
}

/// Should we (re)build for this day, given the ledger?
fn needs_build(l: Option<&BuildLedger>, day_min: u32, gen: u64, sim: bool) -> bool {
    let Some(l) = l else { return true };

    // Rows produced under a replay clock must not survive into live mode.
    if l.simulated && !sim {
        return true;
    }
    // While replaying, a generation bump means the engine reloaded or seeked
    // backwards — the list may describe an abandoned future. In LIVE mode the
    // generation is deliberately NOT compared: a replay of another day bumps it too,
    // and must not invalidate the live list.
    if sim && l.generation != gen {
        return true;
    }
    // Refresh throughout the tradable premarket so a shock that starts at 06:17 can
    // surface around 06:20 instead of waiting for the final 09:25 promotion.
    if (PREMARKET_START_MIN..TRIGGER_MIN).contains(&day_min)
        && day_min.saturating_sub(l.built_min) >= LIVE_REFRESH_MIN
    {
        return true;
    }
    // A list built before 09:25 is provisional (thin or absent premarket); promote it
    // once the clock crosses the trigger.
    if l.built_min < TRIGGER_MIN && day_min >= TRIGGER_MIN {
        return true;
    }
    // Missing premarket inputs can leave either an empty list or a partial daily-only
    // list. Retry both; a healthy empty day remains final.
    if l.retryable
        && l.attempts < MAX_ATTEMPTS
        && day_min.saturating_sub(l.built_min) >= RETRY_GAP_MIN
    {
        return true;
    }
    false
}

/// Build the watchlist for the CURRENT app-clock day if needed. The only place
/// `crate::scoring::build_and_store` is ever called.
///
/// Lock discipline: `BUILD_LOCK` is always taken before `db`, never the reverse, and
/// the `db` guard is never held across an `.await`.
pub async fn ensure_day(
    app_dir: &std::path::Path,
    db: &Arc<Mutex<rusqlite::Connection>>,
    secrets: &Arc<RwLock<Secrets>>,
    force: Force,
) -> EnsureResult {
    // One clock snapshot for the whole call — the sim clock can move mid-build.
    let now = crate::time::now();
    let day = crate::time::et_date(now);
    let day_min = crate::time::et_minutes(now);
    let gen = crate::replay::clock::generation();
    let sim = crate::replay::clock::is_active();

    // NOTE: we deliberately build BEFORE the trigger too, producing a PROVISIONAL
    // list. The panic score itself needs only completed prior days, so it is fully
    // computable at 04:00; current-day premarket shocks join it on each refresh.
    // Refusing to build early would leave the tab empty for the simulated premarket of
    // a replay started at 04:00 — the default start time. `needs_build`'s
    // `built_min < TRIGGER_MIN && day_min >= TRIGGER_MIN` rule then promotes the
    // provisional list to the final one once the clock crosses 09:25.

    // ── First (cheap) evaluation.
    let (cache_ready, ledger) = {
        let conn = db.lock().unwrap();
        let ready = cache_repository::symbols_with_bars(&conn).unwrap_or(0) >= MIN_CACHE_SYMBOLS;
        let ledger = scoring_repository::get_build(&conn, &day).ok().flatten();
        (ready, ledger)
    };
    // Checked here rather than in the scheduler loop: otherwise a replay load on a
    // fresh install would build from an empty cache and stamp a "0 rows, final"
    // ledger entry that never retries.
    if !cache_ready {
        return EnsureResult::NotReady;
    }
    if force == Force::Auto && !needs_build(ledger.as_ref(), day_min, gen, sim) {
        return EnsureResult::UpToDate;
    }
    // Real-time floor — but ONLY for a day that has already been built. At 600×
    // replay speed the simulated retry gap elapses in a second, so this bounds the
    // premarket fetch rate. It must NOT gate a day that has never been built, or
    // "Next Day" clicked within a minute of the previous load would silently produce
    // no watchlist for the new day.
    if force == Force::Auto && ledger.is_some() {
        if let Some(last) = *LAST_BUILD.lock().unwrap() {
            if last.elapsed() < MIN_REAL_GAP {
                return EnsureResult::UpToDate;
            }
        }
    }

    // ── Acquire the guard. Auto never queues a duplicate; explicit callers wait.
    let _guard = match force {
        Force::Auto => match BUILD_LOCK.try_lock() {
            Ok(g) => g,
            Err(_) => return EnsureResult::Busy,
        },
        Force::Rebuild => BUILD_LOCK.lock().await,
    };

    // ── Double-check: another builder may have satisfied us while we waited.
    if force == Force::Auto {
        let ledger = {
            let conn = db.lock().unwrap();
            scoring_repository::get_build(&conn, &day).ok().flatten()
        };
        if !needs_build(ledger.as_ref(), day_min, gen, sim) {
            return EnsureResult::UpToDate;
        }
    }
    // ── The heavy part. `day` is passed in so the whole build is stamped with one
    //    consistent date even though the premarket fetch takes tens of seconds.
    let outcome = match crate::scoring::build_and_store(app_dir, db, secrets, &day).await {
        Ok(o) => o,
        Err(e) => {
            eprintln!("[tagdash] panic watchlist {day}: build failed: {e}");
            return EnsureResult::Failed(e);
        }
    };
    // `attempts` is a missing-input retry budget, not a count of healthy five-minute
    // refreshes. Reset it after a sound build so a 09:25 API failure can still retry.
    let attempts = if outcome.inputs_missing() {
        ledger.as_ref().map(|l| l.attempts).unwrap_or(0) + 1
    } else {
        0
    };

    // ── Record the outcome (0 rows included — that is the point of the ledger).
    {
        let conn = db.lock().unwrap();
        let _ = scoring_repository::record_build(
            &conn,
            &BuildLedger {
                day: day.clone(),
                built_min: day_min,
                row_count: outcome.rows,
                attempts,
                simulated: sim,
                generation: gen,
                retryable: outcome.inputs_missing(),
            },
        );
        let _ = scoring_repository::prune(&conn);
    }
    *LAST_BUILD.lock().unwrap() = Some(Instant::now());

    eprintln!(
        "[tagdash] panic watchlist {day}: built {} rows (attempt {attempts}, sim={sim}, retryable={})",
        outcome.rows,
        outcome.inputs_missing(),
    );
    EnsureResult::Built {
        day,
        rows: outcome.rows,
    }
}

/// Background scheduler: re-evaluates on a kick or on an idle timer.
pub struct PanicWatchlistEngine;

impl PanicWatchlistEngine {
    pub fn start(
        running: Arc<AtomicBool>,
        app_dir: std::path::PathBuf,
        db: Arc<Mutex<rusqlite::Connection>>,
        secrets: Arc<RwLock<Secrets>>,
    ) {
        tauri::async_runtime::spawn(async move {
            while running.load(Ordering::Relaxed) {
                match ensure_day(&app_dir, &db, &secrets, Force::Auto).await {
                    EnsureResult::Built { day, rows } => {
                        eprintln!("[tagdash] panic watchlist: {rows} rows for {day}");
                    }
                    EnsureResult::Failed(e) => {
                        eprintln!("[tagdash] panic watchlist: {e}");
                    }
                    // UpToDate / Busy / NotReady are the normal quiet outcomes.
                    _ => {}
                }
                // Wake on either an explicit kick or the idle timer. `scaled_sleep`
                // only shortens above 1× and samples the clock at entry, so the kick
                // — not the sleep — is what gives replay its low latency.
                tokio::select! {
                    _ = kick().notified() => {}
                    _ = crate::replay::clock::scaled_sleep(LOOP_SECS * 1000) => {}
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ledger(built_min: u32) -> BuildLedger {
        BuildLedger {
            day: "2026-08-14".into(),
            built_min,
            row_count: 12,
            attempts: 1,
            simulated: false,
            generation: 1,
            retryable: false,
        }
    }

    #[test]
    fn premarket_refreshes_every_five_minutes() {
        let l = ledger(360);
        assert!(!needs_build(Some(&l), 364, 1, false));
        assert!(needs_build(Some(&l), 365, 1, false));
    }

    #[test]
    fn final_promotion_happens_at_0925_then_stays_final() {
        assert!(needs_build(Some(&ledger(560)), TRIGGER_MIN, 1, false));
        assert!(!needs_build(
            Some(&ledger(TRIGGER_MIN)),
            TRIGGER_MIN + 10,
            1,
            false
        ));
    }

    #[test]
    fn a_partial_list_retries_when_premarket_inputs_failed() {
        let mut l = ledger(TRIGGER_MIN);
        l.retryable = true;
        l.attempts = 1;
        l.row_count = 8;
        assert!(needs_build(Some(&l), TRIGGER_MIN + RETRY_GAP_MIN, 1, false,));
    }
}
