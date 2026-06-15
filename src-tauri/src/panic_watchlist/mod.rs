// Panic Mean Reversion watchlist scheduler.
//
// A background task that builds the day's Panic Mean Reversion watchlist once, at
// 09:00 ET, and on a late launch (app started after 09:00) runs it immediately. The
// build itself (premarket pre-filter + the BB-area / move-since-SMA20 rankings +
// merge) lives in `crate::scoring::build_and_store`; this module only owns the
// scheduling, the once-per-day gate and the crash-resilient reuse of a list already
// persisted earlier today.
//
// Why a dedicated task and not the startup pipeline: the pre-filter needs the
// premarket session's volume, so the list can only be built from ~09:00 ET onward —
// regardless of when (or how early) the app launched. The result is persisted to the
// `panic_watchlist` table, so a restart later in the day reuses it instead of
// refetching. The scanner reads that table to surface the screener.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex, RwLock,
};


use crate::config::secrets::Secrets;
use crate::local_db::{cache_repository, scoring_repository};

/// ET wall-clock minute at which the day's watchlist is built (09:00 ET = 540).
const TRIGGER_MIN: u32 = 9 * 60;
/// How often the scheduler checks the clock / readiness (seconds).
const LOOP_SECS: u64 = 30;
/// Don't attempt a build until the daily cache holds at least this many symbols —
/// avoids racing the startup pipeline's daily-bar load on a fresh launch.
const MIN_CACHE_SYMBOLS: i64 = 100;
/// App-meta key holding the ET date of the last successful build (crash-resilient
/// reuse: a restart on the same day skips the rebuild if the table is non-empty).
const MARKER_KEY: &str = "panic_watchlist_date";

pub struct PanicWatchlistEngine;

impl PanicWatchlistEngine {
    /// Spawn the scheduler. Returns immediately.
    pub fn start(
        running: Arc<AtomicBool>,
        db:      Arc<Mutex<rusqlite::Connection>>,
        secrets: Arc<RwLock<Secrets>>,
    ) {
        tauri::async_runtime::spawn(async move {
            // Hydrate "already built today" from the DB so a same-day restart reuses
            // the persisted list rather than rebuilding (only when rows actually
            // exist — a stale marker over an empty table still rebuilds).
            let mut built_day: Option<String> = {
                let conn = db.lock().unwrap();
                let marked = cache_repository::get_app_meta(&conn, MARKER_KEY);
                let have_rows = scoring_repository::count(&conn).unwrap_or(0) > 0;
                match marked {
                    Some(d) if have_rows => Some(d),
                    _ => None,
                }
            };

            while running.load(Ordering::Relaxed) {
                // App clock: during a Market Replay "today" is the simulated day,
                // so the watchlist is (re)built for the replayed date — its data
                // sources are themselves capped at the simulated instant.
                let now = crate::time::now();
                let today = crate::time::et_date(now);
                let past_trigger = crate::time::et_minutes(now) >= TRIGGER_MIN;
                let already_today = built_day.as_deref() == Some(today.as_str());

                if past_trigger && !already_today {
                    let cache_ready = {
                        let conn = db.lock().unwrap();
                        cache_repository::symbols_with_bars(&conn).unwrap_or(0) >= MIN_CACHE_SYMBOLS
                    };
                    if cache_ready {
                        match crate::scoring::build_and_store(&db, &secrets).await {
                            Ok(n) if n > 0 => {
                                {
                                    let conn = db.lock().unwrap();
                                    let _ = cache_repository::set_app_meta(&conn, MARKER_KEY, &today);
                                }
                                built_day = Some(today.clone());
                                eprintln!("[tagdash] panic watchlist: built {n} rows for {today}");
                            }
                            Ok(_) => {
                                // No survivors yet (e.g. premarket still thin / fetch
                                // failed) — leave the day unmarked so we retry.
                                eprintln!("[tagdash] panic watchlist: 0 rows for {today} — will retry");
                            }
                            Err(e) => {
                                eprintln!("[tagdash] panic watchlist build failed: {e}");
                            }
                        }
                    }
                }

                crate::replay::clock::scaled_sleep(LOOP_SECS * 1000).await;
            }
        });
    }
}
