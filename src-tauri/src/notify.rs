// Low-latency desktop attention cues fired the instant a scanner alert is pushed
// (from `scanner::push_alert`, the single choke point every strategy + the alarm
// watcher funnel through — already cooldown-gated, so one call == one fresh
// alert). Two independent, session-gated actions, both opt-in via config:
//
//   • flash      — a 500 ms full-screen white pulse. A transparent, always-on-top,
//                  click-through overlay window pulses white on a `tagdash://flash`
//                  event, so it never steals focus from whatever app the user is in,
//                  and stays visible even when other windows cover TagDash. The
//                  overlay is built on demand from Settings (`ensure_flash_overlay`),
//                  not at startup — `on_alert` is a no-op when it isn't open.
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

/// Create the full-screen white flash overlay window — transparent, always-on-top,
/// click-through, no taskbar entry, not focused. Idempotent (no-op if it already
/// exists). Created ON DEMAND from Settings, never at startup, so it isn't a
/// permanent resident webview (a memory cost the app no longer pays by default).
pub fn ensure_flash_overlay(app: &AppHandle) -> Result<(), String> {
    use tauri::{WebviewUrl, WebviewWindowBuilder};
    if app.get_webview_window("flash").is_some() {
        return Ok(());
    }
    // Same SPA entry; main.tsx renders the flash overlay (not the app) when it
    // detects it's running in the window labelled "flash".
    let win = WebviewWindowBuilder::new(app, "flash", WebviewUrl::App("index.html".into()))
        .title("")
        .decorations(false)
        // A borderless window still gets a DWM shadow on Windows, showing as a
        // faint outline around the (otherwise invisible) transparent overlay. Kill it.
        .shadow(false)
        .transparent(true)
        .always_on_top(true)
        .skip_taskbar(true)
        .focused(false)
        .visible(true)
        .resizable(false)
        .build()
        .map_err(|e| e.to_string())?;
    let _ = win.set_ignore_cursor_events(true);
    // Cover the whole primary monitor (work area + taskbar).
    if let Ok(Some(mon)) = win.primary_monitor() {
        let _ = win.set_size(*mon.size());
        let _ = win.set_position(tauri::PhysicalPosition::new(0, 0));
    }
    Ok(())
}

/// Close the flash overlay window if it exists (Settings → flash disabled).
pub fn close_flash_overlay(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("flash") {
        let _ = win.close();
    }
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
        // Pulse the overlay if it's open (built on demand from Settings) — no window
        // show/activate, so the user's current app keeps focus. No-op otherwise.
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
