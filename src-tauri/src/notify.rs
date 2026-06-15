// Low-latency desktop attention cues fired the instant a scanner alert is pushed
// (from `scanner::push_alert`, the single choke point every strategy + the alarm
// watcher funnel through — already cooldown-gated, so one call == one fresh
// alert). Two independent, session-gated actions, both opt-in via config:
//
//   • flash      — a 500 ms full-screen white pulse. A transparent, always-on-top,
//                  click-through overlay window stays up permanently (invisible)
//                  and just pulses white on a `tagdash://flash` event, so it never
//                  steals focus from whatever app the user is in. Visible even when
//                  other windows cover TagDash.
//   • foreground — bring the main TagDash window back to the front + request user
//                  attention (taskbar flash / dock bounce) as a fallback.
//
// The AppHandle is stashed in a OnceLock at startup so `push_alert` (a plain sync
// fn with no Tauri context) can reach it without threading a handle through every
// call site.

use std::sync::OnceLock;

use tauri::{AppHandle, Emitter, Manager};

use crate::state::AppState;
use crate::types::Session;

/// Event the flash overlay window listens for to run one white pulse.
pub const FLASH_EVENT: &str = "tagdash://flash";

static APP: OnceLock<AppHandle> = OnceLock::new();

/// Stash the AppHandle once the app is set up. Called from `lib.rs`.
pub fn init(app: AppHandle) {
    let _ = APP.set(app);
}

/// True when an attention mode ("off"|"premarket"|"open"|"both") is active for the
/// session the alert belongs to. PreOpen/Afterhours never trigger (the user only
/// asked for premarket + open).
fn mode_matches(mode: &str, session: Session) -> bool {
    match mode {
        "premarket" => session == Session::Premarket,
        "open"      => session == Session::Open,
        "both"      => matches!(session, Session::Premarket | Session::Open),
        _           => false, // "off" / unknown
    }
}

/// Called from `scanner::push_alert` for every new alert. Reads the (cheap) config
/// and fires the flash and/or foreground action if enabled for this session.
pub fn on_alert(session: Session) {
    let Some(app) = APP.get() else { return };

    let (flash_mode, fg_mode) = {
        let state = app.state::<AppState>();
        let cfg = state.config.read().unwrap();
        (cfg.ui.flash_alerts.clone(), cfg.ui.foreground_alerts.clone())
    };

    if mode_matches(&flash_mode, session) {
        // Just signal the always-up overlay to pulse — no window show/activate, so
        // the user's current app keeps focus.
        let _ = app.emit_to("flash", FLASH_EVENT, ());
    }
    if mode_matches(&fg_mode, session) {
        bring_to_front(app);
    }
}

/// Bring the main window to the front and ask the OS to flag it for attention.
/// `set_focus` may not steal foreground on Windows (focus-stealing prevention),
/// so `request_user_attention` flashes the taskbar icon / bounces the dock as a
/// reliable fallback cue.
fn bring_to_front(app: &AppHandle) {
    let Some(win) = app.get_webview_window("main") else { return };
    let _ = win.unminimize();
    let _ = win.show();
    let _ = win.set_focus();
    let _ = win.request_user_attention(Some(tauri::UserAttentionType::Critical));
}
