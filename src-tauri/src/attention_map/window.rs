// The detached « Attention du marché » window: a second, native-framed window meant
// to live on another monitor.
//
// Same SPA entry as the main window (`main.tsx` renders the attention screen when
// it finds itself in the window labelled "attention"). Native decorations on
// purpose: the OS then handles dragging across monitors, snapping and fullscreen
// with no custom title bar to keep in step.
//
// REMEMBERED: whether it is on, where it sits and how large — one JSON blob in
// `app_config`. On the next launch it reopens where it was, unless that monitor is
// gone, in which case it lands on a secondary monitor if there is one.
//
// Closing it with its own close button turns the feature off, exactly like the
// toggle in the main title bar. Quitting TagDash does not: the window comes back
// with the app.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WindowEvent};

use crate::state::AppState;

pub const LABEL: &str = "attention";
const META_KEY: &str = "attention_window_v1";
/// Sent to the main window whenever the on/off state changes.
pub const STATE_EVENT: &str = "attention://window";

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Persisted {
    pub enabled: bool,
    /// Outer position and inner size, physical pixels, of the un-maximized window.
    pub x: Option<i32>,
    pub y: Option<i32>,
    pub w: Option<u32>,
    pub h: Option<u32>,
    pub maximized: bool,
    pub fullscreen: bool,
}

static MEMORY: Mutex<Option<Persisted>> = Mutex::new(None);
/// Bumped on every move/resize; a write only goes through if nothing moved since.
static GEOMETRY_SEQ: AtomicU64 = AtomicU64::new(0);

pub fn load(conn: &rusqlite::Connection) -> Persisted {
    crate::local_db::cache_repository::get_app_meta(conn, META_KEY)
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default()
}

fn remembered(app: &AppHandle) -> Persisted {
    let mut mem = MEMORY.lock().unwrap();
    *mem.get_or_insert_with(|| {
        let state = app.state::<AppState>();
        let conn = state.db.lock().unwrap();
        load(&conn)
    })
}

fn remember(app: &AppHandle, update: impl FnOnce(&mut Persisted)) {
    // Start from what is stored: an update made before the window was ever opened
    // in this run must not wipe the geometry of the last one.
    let mut next = remembered(app);
    update(&mut next);
    let snapshot = {
        let mut mem = MEMORY.lock().unwrap();
        *mem = Some(next);
        next
    };
    let state = app.state::<AppState>();
    let conn = state.db.lock().unwrap();
    if let Ok(json) = serde_json::to_string(&snapshot) {
        let _ = crate::local_db::cache_repository::set_app_meta(&conn, META_KEY, &json);
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct WindowState {
    pub enabled: bool,
    pub open: bool,
}

pub fn state(app: &AppHandle) -> WindowState {
    WindowState {
        enabled: app.state::<AppState>().attention_map.is_enabled(),
        open: app.get_webview_window(LABEL).is_some(),
    }
}

fn announce(app: &AppHandle) {
    let _ = app.emit_to("main", STATE_EVENT, state(app));
}

/// True when a `w`×`h` window at (`x`, `y`) would have its title bar on one of the
/// monitors plugged in right now.
fn on_a_monitor(win: &tauri::WebviewWindow, x: i32, y: i32, w: u32) -> bool {
    let Ok(monitors) = win.available_monitors() else {
        return false;
    };
    monitors.iter().any(|m| {
        let (mx, my) = (m.position().x, m.position().y);
        let (mw, mh) = (m.size().width as i32, m.size().height as i32);
        // Enough of the top edge on screen to grab it.
        x + w as i32 > mx + 80 && x < mx + mw - 80 && y >= my - 10 && y < my + mh - 60
    })
}

/// Put a fresh window where it was, else on a monitor the main window is not on.
fn place(app: &AppHandle, win: &tauri::WebviewWindow, p: &Persisted) {
    if let (Some(x), Some(y), Some(w), Some(h)) = (p.x, p.y, p.w, p.h) {
        if w >= 400 && h >= 300 && on_a_monitor(win, x, y, w) {
            let _ = win.set_size(PhysicalSize::new(w, h));
            let _ = win.set_position(PhysicalPosition::new(x, y));
            return;
        }
    }
    let main_monitor = app
        .get_webview_window("main")
        .and_then(|m| m.current_monitor().ok().flatten())
        .map(|m| *m.position());
    let other = win
        .available_monitors()
        .ok()
        .and_then(|ms| ms.into_iter().find(|m| Some(*m.position()) != main_monitor));
    if let Some(m) = other {
        let (w, h) = (m.size().width, m.size().height);
        let _ = win.set_size(PhysicalSize::new(w * 9 / 10, h * 85 / 100));
        let _ = win.set_position(PhysicalPosition::new(
            m.position().x + (w / 20) as i32,
            m.position().y + (h / 14) as i32,
        ));
    }
}

/// Record the window's geometry a moment after it stops moving.
fn track_geometry(app: &AppHandle) {
    let seq = GEOMETRY_SEQ.fetch_add(1, Ordering::Relaxed) + 1;
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        if GEOMETRY_SEQ.load(Ordering::Relaxed) != seq {
            return;
        }
        let Some(win) = app.get_webview_window(LABEL) else {
            return;
        };
        if win.is_minimized().unwrap_or(false) {
            return;
        }
        let maximized = win.is_maximized().unwrap_or(false);
        let fullscreen = win.is_fullscreen().unwrap_or(false);
        let (pos, size) = (win.outer_position().ok(), win.inner_size().ok());
        remember(&app, |p| {
            p.maximized = maximized;
            p.fullscreen = fullscreen;
            // Maximized or fullscreen: keep the last free-floating geometry, so
            // leaving that state next time returns to a sensible window.
            if !maximized && !fullscreen {
                if let (Some(pos), Some(size)) = (pos, size) {
                    if size.width >= 400 && size.height >= 300 {
                        (p.x, p.y) = (Some(pos.x), Some(pos.y));
                        (p.w, p.h) = (Some(size.width), Some(size.height));
                    }
                }
            }
        });
    });
}

/// Open the window (no-op when it already is), and turn the engine on.
pub fn open(app: &AppHandle) -> Result<(), String> {
    let shared = app.state::<AppState>().attention_map.clone();
    if let Some(win) = app.get_webview_window(LABEL) {
        let _ = win.unminimize();
        let _ = win.show();
        shared.set_enabled(true);
        return Ok(());
    }
    let p = remembered(app);
    let mut builder = tauri::WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("index.html".into()))
        .title("TagDash — Attention du marché")
        .inner_size(1360.0, 860.0)
        .min_inner_size(760.0, 480.0)
        .resizable(true)
        .decorations(true)
        .theme(Some(tauri::Theme::Dark))
        // Shown once placed, so it never flashes on the wrong monitor.
        .visible(false);
    // WebView2 requires every webview sharing a data folder to use the same
    // browser arguments: mirror the main window's.
    if let Some(args) = app
        .config()
        .app
        .windows
        .first()
        .and_then(|w| w.additional_browser_args.clone())
    {
        builder = builder.additional_browser_args(&args);
    }
    let win = builder.build().map_err(|e| e.to_string())?;
    place(app, &win, &p);
    if p.maximized {
        let _ = win.maximize();
    }
    if p.fullscreen {
        let _ = win.set_fullscreen(true);
    }
    let _ = win.show();

    let handle = app.clone();
    win.on_window_event(move |event| match event {
        WindowEvent::Moved(_) | WindowEvent::Resized(_) => track_geometry(&handle),
        // The window's own close button: the feature is switched off. A close
        // decided by the app goes through `destroy`, which does not pass here.
        WindowEvent::CloseRequested { .. } => {
            handle.state::<AppState>().attention_map.set_enabled(false);
            remember(&handle, |p| p.enabled = false);
        }
        WindowEvent::Destroyed => announce(&handle),
        _ => {}
    });

    shared.set_enabled(true);
    remember(app, |p| p.enabled = true);
    announce(app);
    Ok(())
}

/// Switch the feature off from the main window's toggle.
pub fn close(app: &AppHandle) {
    app.state::<AppState>().attention_map.set_enabled(false);
    remember(app, |p| p.enabled = false);
    if let Some(win) = app.get_webview_window(LABEL) {
        let _ = win.destroy();
    }
    announce(app);
}

/// The main window is going away: take the detached one with it, but leave the
/// feature on so it reopens with the app.
pub fn on_main_closed(app: &AppHandle) {
    if let Some(win) = app.get_webview_window(LABEL) {
        let _ = win.destroy();
    }
}

/// Reopen at launch when it was on at the last exit.
pub fn restore(app: &AppHandle) {
    if remembered(app).enabled {
        if let Err(e) = open(app) {
            eprintln!("[tagdash] attention_map: fenêtre non restaurée ({e})");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blob_from_before_a_field_existed_still_loads() {
        let p: Persisted = serde_json::from_str(r#"{"enabled":true,"x":-1900,"y":40}"#).unwrap();
        assert!(p.enabled);
        assert_eq!((p.x, p.y, p.w), (Some(-1900), Some(40), None));
        assert!(!p.fullscreen);
    }
}
