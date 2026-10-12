// Attention du marché — the detached second-monitor screen.
//
// NOT the `market_attention` gate (09:30–12:30 top-10 feeding Perfect Pullback):
// this module draws where extended-hours money went, ticker by ticker, across the
// two sessions that frame a trading morning:
//
//   after-hours of the previous session (16:00–20:00 ET)  →  premarket (04:00–now)
//
// X = dollars traded over a 5-minute slot / the same slot's median over the last
//     60 sessions.  Y = change since the previous REGULAR close — one reference
//     for both sessions, so a name that gapped after hours and faded before the
//     open reads as one path.
//
// DATA. Alpaca SIP 5-minute bars over REST, the same source the replay pipeline
// and Micro Pullback's pre-scan read. Bars rather than prints on purpose: Alpaca
// builds them from bar-eligible trades only and rewrites them when a correction
// or a cancel lands, so the two trailing slots are simply re-read on every poll.
// On the free Basic plan the SIP history is 15 minutes late — the screen says so
// rather than pretending; on the SIP plan it follows the tape to the minute.
//
// Nothing here touches `MarketState`: the map is fed by its own polls, so a Market
// Replay keeps its tape to itself and the map just follows the simulated clock
// over a frame downloaded once (see `engine`).
//
// HISTORY. One compact row per (session, part) in `attention_archive` holds every
// symbol's dollars per slot. The live frame joins it when it completes, so after
// the first bootstrap the 60-session medians cost no request at all.
//
// JOURNAL. `attention_snapshots` keeps, per day, only the tickers that made the
// map plus the per-slot weather — enough to redraw the screen as it stood at any
// instant of that day (« Revue »), with no call to the provider.

pub mod calendar;
pub mod engine;
pub mod fetch;
pub mod frame;
pub mod options;
pub mod store;
pub mod view;
pub mod window;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use serde::{Deserialize, Serialize};

pub use options::AttentionOptions;

/// What « Revue » is showing, so the detached screen can follow it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReviewContext {
    /// ET trading day of the reviewed trade.
    pub day: String,
    /// Playhead on the market clock, epoch milliseconds.
    pub at_ms: i64,
    pub symbol: Option<String>,
}

/// Where the engine stands — polled with the map so the screen can say why it is
/// empty instead of looking broken.
#[derive(Debug, Clone, Default, Serialize)]
pub struct EngineStatus {
    /// off | loading | ready | no_creds | offline | error
    pub state: String,
    pub message: Option<String>,
    /// Sessions of history archived / wanted, for the bootstrap progress line.
    pub history_done: u32,
    pub history_target: u32,
    pub history_building: bool,
}

pub struct AttentionShared {
    /// Follows the window: no window, no polling.
    pub enabled: AtomicBool,
    pub options: RwLock<AttentionOptions>,
    /// What the engine last published (live or replay).
    pub current: RwLock<Option<Arc<view::Snapshot>>>,
    pub status: RwLock<EngineStatus>,
    /// Bumped on every publish; the screen re-reads only when it moved.
    pub version: AtomicU64,
    /// Last day decoded for « Revue », so scrubbing a trade does not re-read the DB.
    pub review_cache: Mutex<Option<(String, Arc<view::Snapshot>)>>,
    pub review: RwLock<Option<ReviewContext>>,
    /// Wakes the engine when the window opens or the options change.
    pub wake: tokio::sync::Notify,
}

impl AttentionShared {
    pub fn new(options: AttentionOptions, enabled: bool) -> Self {
        Self {
            enabled: AtomicBool::new(enabled),
            options: RwLock::new(options),
            current: RwLock::new(None),
            status: RwLock::new(EngineStatus {
                state: "off".into(),
                ..Default::default()
            }),
            version: AtomicU64::new(0),
            review_cache: Mutex::new(None),
            review: RwLock::new(None),
            wake: tokio::sync::Notify::new(),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::Relaxed);
        self.wake.notify_one();
    }

    pub fn publish(&self, snap: view::Snapshot) {
        *self.current.write().unwrap() = Some(Arc::new(snap));
        self.version.fetch_add(1, Ordering::Relaxed);
    }
}
