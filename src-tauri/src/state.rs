// Shared application state injected into every Tauri command via manage().
// Lives for the entire lifetime of the process.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{
    atomic::AtomicBool,
    Arc, Mutex, RwLock,
};

use tokio::sync::watch;

use crate::chart_state::ChartState;
use crate::config::{secrets::Secrets, AppConfig};
use crate::internal_trading::InternalBook;
use crate::market_state::MarketState;
use crate::startup::StartupState;
use crate::types::{AlertEnrichment, AlertSignal, ScreenerMatch};

pub struct AppState {
    /// Directory where tagdash.toml, tagdash.secrets.toml and tagdash.db live.
    pub app_dir: PathBuf,
    /// Runtime config. RwLock so reads never block each other.
    pub config: Arc<RwLock<AppConfig>>,
    /// API secrets. Never serialised to the frontend.
    pub secrets: Arc<RwLock<Secrets>>,
    /// SQLite connection. Mutex because rusqlite::Connection is Send but not Sync.
    pub db: Arc<Mutex<rusqlite::Connection>>,
    /// Startup pipeline progress (polled by the frontend).
    pub startup: Arc<RwLock<StartupState>>,
    /// RAM source of truth for all live market data.
    pub market: Arc<RwLock<MarketState>>,
    /// Controls the background mock feed task lifecycle.
    pub mock_feed_running: Arc<AtomicBool>,
    /// Controls the Alpaca live WebSocket feed task lifecycle.
    pub live_feed_running: Arc<AtomicBool>,
    /// Controls the Alpaca news WebSocket feed task lifecycle (premarket news
    /// investor). Streams headlines into MarketState for the micro_pullback
    /// correlation engine and the news debug panel.
    pub news_feed_running: Arc<AtomicBool>,
    /// Symbols currently displayed in chart zones — the live feed tick-streams
    /// these (trades+quotes) on top of the broad surveillance tier. The frontend
    /// updates this when the visible zones change; the feed holds a receiver and
    /// reconciles its subscriptions on change.
    pub focus_symbols_tx: watch::Sender<Vec<String>>,
    /// Controls the scanner engine task lifecycle.
    pub scanner_running: Arc<AtomicBool>,
    /// Controls the Perfect Pullback engine task lifecycle.
    pub perfect_pullback_running: Arc<AtomicBool>,
    /// Controls the Micro Pullback engine task lifecycle (premarket dormancy →
    /// ignition → confirmation state machine; see `crate::micro_pullback`).
    pub micro_pullback_running: Arc<AtomicBool>,
    /// Controls the Panic Mean Reversion watchlist scheduler (builds the day's
    /// two-list watchlist at 09:00 ET; see `crate::panic_watchlist`).
    pub panic_watchlist_running: Arc<AtomicBool>,
    /// Controls the internal trading loop (pending limit/stop fills, bracket SL/TP
    /// reconciliation, TradeTally mirroring) — driven by market data, not UI polls.
    pub trading_loop_running: Arc<AtomicBool>,
    /// Runtime on/off state per strategy id (defaults to each strategy's compiled
    /// `enabled()` flag, overridable at runtime from Settings and persisted in the
    /// `app_config` table). The scanner reads this each pass.
    pub strategy_enabled: Arc<RwLock<HashMap<String, bool>>>,
    /// Runtime $-risk-per-trade override per strategy id (defaults to each
    /// strategy's compiled `risk_config().max_risk_dollars`, editable from
    /// Settings and persisted in the `app_config` table). Position sizing reads
    /// this live so edits take effect on the very next order.
    pub strategy_risk: Arc<RwLock<HashMap<String, f64>>>,
    /// Current active alerts (newest first, capped at 100).
    pub active_alerts: Arc<RwLock<Vec<AlertSignal>>>,
    /// Full alert history for the session (newest first, capped at 500).
    pub alert_history: Arc<RwLock<Vec<AlertSignal>>>,
    /// Live pre-open screener matches (replaced wholesale every scan pass;
    /// tickers disappear the instant they stop matching). Drives the pre-open tab.
    pub screener: Arc<RwLock<Vec<ScreenerMatch>>>,
    /// Per-zone trade context (SL, TP, tradeID) stored in RAM.
    pub chart: Arc<RwLock<ChartState>>,
    /// Internal simulated order book (no real broker orders in V1).
    pub internal_book: Arc<RwLock<InternalBook>>,
    /// Progressive per-symbol alert enrichment (float, country, classification,
    /// split, news, LLM reads), filled async when an alert is shown in a zone.
    pub enrichments: Arc<RwLock<HashMap<String, AlertEnrichment>>>,
    /// Market Replay shared handle: status polled by the toolbar + command
    /// channel to the (single) replay engine task. The simulated clock itself is
    /// global (see `replay::clock` / `time::now`).
    pub replay: Arc<crate::replay::ReplayShared>,
}
