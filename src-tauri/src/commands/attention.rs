// Commands of the detached « Attention du marché » screen — see
// `crate::attention_map`. The window itself, its options, the map as cut at the
// current clock (live, replay or a reviewed trade), and the two bridges between
// the windows: a ticker sent to the main one, a reviewed trade followed here.

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use crate::attention_map::view::{self, Detail, MapView, Snapshot};
use crate::attention_map::window::{self, WindowState};
use crate::attention_map::{store, AttentionOptions, EngineStatus, ReviewContext};
use crate::state::AppState;

/// Sent to the main window: open this ticker there.
pub const PROPAGATE_EVENT: &str = "attention://propagate";
/// Sent to the attention window: « Revue » moved (or ended).
pub const REVIEW_EVENT: &str = "attention://review";

/// `async` so the window is created off the IPC thread (same reason as the flash
/// overlay: building a window from a sync command deadlocks on Windows).
#[tauri::command(rename_all = "snake_case")]
pub async fn attention_window_set(enabled: bool, app: AppHandle) -> Result<WindowState, String> {
    if enabled {
        window::open(&app)?;
    } else {
        window::close(&app);
    }
    Ok(window::state(&app))
}

#[tauri::command(rename_all = "snake_case", async)]
pub fn attention_window_state(app: AppHandle) -> WindowState {
    window::state(&app)
}

#[tauri::command(rename_all = "snake_case", async)]
pub fn get_attention_options(state: State<'_, AppState>) -> AttentionOptions {
    *state.attention_map.options.read().unwrap()
}

#[tauri::command(rename_all = "snake_case", async)]
pub fn set_attention_options(
    options: AttentionOptions,
    state: State<'_, AppState>,
) -> Result<AttentionOptions, String> {
    let clean = options.sanitized();
    {
        let conn = state.db.lock().unwrap();
        clean.save(&conn)?;
    }
    *state.attention_map.options.write().unwrap() = clean;
    state.attention_map.wake.notify_one();
    Ok(clean)
}

#[derive(Serialize)]
pub struct MapReply {
    /// Identity of what `map` shows. Sent back as `known`, it lets an unchanged
    /// poll return without the map.
    pub key: String,
    /// live | replay | review
    pub mode: &'static str,
    pub status: EngineStatus,
    pub map: Option<MapView>,
    pub review: Option<ReviewContext>,
    /// A trade is under review but its day was never recorded.
    pub review_missing: bool,
}

fn options_tag(opts: &AttentionOptions) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    format!("{opts:?}").hash(&mut h);
    h.finish()
}

/// The recorded day of a reviewed trade, decoded once and kept while the review
/// stays on that day.
fn review_snapshot(state: &AppState, day: &str) -> Option<Arc<Snapshot>> {
    {
        let cache = state.attention_map.review_cache.lock().unwrap();
        if let Some((d, snap)) = cache.as_ref() {
            if d == day {
                return Some(snap.clone());
            }
        }
    }
    let snap = Arc::new(store::get_snapshot(&state.db, day)?);
    *state.attention_map.review_cache.lock().unwrap() = Some((day.to_string(), snap.clone()));
    Some(snap)
}

/// What the screen should draw right now, and from which snapshot.
enum Source {
    Review(ReviewContext, Option<Arc<Snapshot>>),
    Engine(Option<Arc<Snapshot>>, bool),
}

fn source(state: &AppState, follow_review: bool) -> Source {
    let review = state.attention_map.review.read().unwrap().clone();
    match review.filter(|_| follow_review) {
        Some(ctx) => {
            // The map is filed under the day whose premarket it holds. A trade
            // taken after the close belongs to the NEXT day's frame (its
            // after-hours is that frame's first half), so the frame is resolved
            // from the instant rather than from the trade's calendar day.
            let day = chrono::DateTime::from_timestamp_millis(ctx.at_ms)
                .map(|t| crate::attention_map::frame::Frame::at(t).day.to_string())
                .unwrap_or_else(|| ctx.day.clone());
            let snap = review_snapshot(state, &day);
            Source::Review(ctx, snap)
        }
        None => Source::Engine(
            state.attention_map.current.read().unwrap().clone(),
            crate::replay::clock::is_active(),
        ),
    }
}

fn cut_of(src: &Source) -> Option<(Arc<Snapshot>, view::Cut)> {
    match src {
        Source::Review(ctx, snap) => {
            let snap = snap.clone()?;
            let cut = snap.cut_at(ctx.at_ms / 1000, false);
            Some((snap, cut))
        }
        Source::Engine(snap, replay) => {
            let snap = snap.clone()?;
            let cut = if *replay {
                snap.cut_at(crate::time::now().timestamp(), false)
            } else {
                snap.live_cut()
            };
            Some((snap, cut))
        }
    }
}

#[tauri::command(rename_all = "snake_case", async)]
pub fn get_attention_map(
    known: Option<String>,
    follow_review: bool,
    state: State<'_, AppState>,
) -> MapReply {
    let shared = &state.attention_map;
    let opts = *shared.options.read().unwrap();
    let status = shared.status.read().unwrap().clone();
    let src = source(&state, follow_review);
    let (mode, review, review_missing) = match &src {
        Source::Review(ctx, snap) => ("review", Some(ctx.clone()), snap.is_none()),
        Source::Engine(_, true) => ("replay", None, false),
        Source::Engine(_, false) => ("live", None, false),
    };
    let cut = cut_of(&src);
    let key = match (&src, &cut) {
        (Source::Review(ctx, _), Some((_, c))) => {
            format!("review:{}:{}:{:x}", ctx.day, c.head(), options_tag(&opts))
        }
        (Source::Review(ctx, _), None) => format!("review:{}:none", ctx.day),
        (Source::Engine(..), Some((_, c))) => format!(
            "{mode}:{}:{}:{}",
            shared.version.load(std::sync::atomic::Ordering::Relaxed),
            c.head(),
            c.filling.map_or(0, |(_, f)| (f * 20.0) as u32),
        ),
        (Source::Engine(..), None) => format!("{mode}:none"),
    };
    let map = match cut {
        Some((snap, c)) if known.as_deref() != Some(key.as_str()) => {
            Some(view::build_view(&snap, &c, &opts))
        }
        _ => None,
    };
    MapReply {
        key,
        mode,
        status,
        map,
        review,
        review_missing,
    }
}

/// The 5-minute candles of one ticker of the map, cut like the map itself. No
/// request: they are the slots the map was drawn from.
#[tauri::command(rename_all = "snake_case", async)]
pub fn get_attention_detail(
    symbol: String,
    follow_review: bool,
    state: State<'_, AppState>,
) -> Option<Detail> {
    let (snap, cut) = cut_of(&source(&state, follow_review))?;
    view::build_detail(&snap, &symbol, &cut)
}

/// Second click on a ticker of the map: hand it to the main window, which opens it
/// the way it opens any ticker.
#[tauri::command(rename_all = "snake_case", async)]
pub fn attention_propagate(symbol: String, app: AppHandle) -> Result<(), String> {
    app.emit_to("main", PROPAGATE_EVENT, symbol)
        .map_err(|e| e.to_string())
}

/// « Revue » tells the detached screen which trade, and where its playhead is
/// (None when the review tab is left).
#[tauri::command(rename_all = "snake_case", async)]
pub fn attention_set_review(
    review: Option<ReviewContext>,
    state: State<'_, AppState>,
    app: AppHandle,
) {
    let changed = {
        let mut cur = state.attention_map.review.write().unwrap();
        let changed = *cur != review;
        *cur = review.clone();
        changed
    };
    if changed {
        if review.is_none() {
            *state.attention_map.review_cache.lock().unwrap() = None;
        }
        let _ = app.emit_to(window::LABEL, REVIEW_EVENT, review);
    }
}

/// Days whose map was recorded — « Revue » can tell at once whether there is one.
#[tauri::command(rename_all = "snake_case", async)]
pub fn get_attention_snapshot_days(state: State<'_, AppState>) -> Vec<String> {
    store::snapshot_days(&state.db)
}
