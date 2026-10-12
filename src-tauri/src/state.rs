// Shared application state injected into every Tauri command via manage().
// Lives for the entire lifetime of the process.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{atomic::AtomicBool, Arc, Mutex, RwLock};

use tokio::sync::watch;

use crate::chart_state::ChartState;
use crate::config::{secrets::Secrets, AppConfig};
use crate::internal_trading::InternalBook;
use crate::market_state::MarketState;
use crate::startup::StartupState;
use crate::types::{AlertEnrichment, AlertSignal, AttentionEntry, ScreenerMatch};

pub struct AppState {
    /// The data root. Personal data lives in `user/` under it and re-downloadable
    /// data in `cache/` — every path is built by `crate::paths`.
    pub app_dir: PathBuf,
    /// Runtime config. RwLock so reads never block each other.
    pub config: Arc<RwLock<AppConfig>>,
    /// API secrets. Never serialised to the frontend.
    pub secrets: Arc<RwLock<Secrets>>,
    /// SQLite connection: the cache database with the personal one attached as
    /// `user` (see `local_db`). Mutex because rusqlite::Connection is Send but not Sync.
    pub db: Arc<Mutex<rusqlite::Connection>>,
    /// Startup pipeline progress (polled by the frontend).
    pub startup: Arc<RwLock<StartupState>>,
    /// RAM source of truth for all live market data.
    pub market: Arc<RwLock<MarketState>>,
    /// Broker engine. Holds the SERVER-side view of orders/positions when the
    /// active broker is Alpaca (see `crate::broker`); empty for the internal
    /// TagDash broker, which keeps its book in `internal_book`.
    pub broker: Arc<crate::broker::BrokerEngine>,
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
    /// Controls the HOD Drive engine task lifecycle (multi-timeframe clean-drive →
    /// pullback-toward-HOD detector; see `crate::hod_drive`).
    pub hod_drive_running: Arc<AtomicBool>,
    /// Controls the Market Attention Gate engine task lifecycle (direction-agnostic
    /// 09:30–12:30 ET ticker selection; see `crate::market_attention`).
    pub market_attention_running: Arc<AtomicBool>,
    /// Controls the Micro Pullback engine task lifecycle (cross-sectional attention
    /// → upward impulse → optional A+ catalyst qualification; see
    /// `crate::micro_pullback`).
    pub micro_pullback_running: Arc<AtomicBool>,
    pub dislocation_running: Arc<AtomicBool>,
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
    pub strategy_risk: Arc<RwLock<HashMap<String, crate::types::StrategyRiskConfig>>>,
    /// Micro Pullback's own options (universe, detection thresholds, tape watch,
    /// active tempos). Strategy-specific rather than a generic channel — see
    /// `crate::micro_pullback::options`. Persisted in `app_meta`; the engine re-reads
    /// it every tick so a Settings change lands on the next second.
    pub micro_pullback_options: Arc<RwLock<crate::micro_pullback::MicroPullbackOptions>>,
    /// Dislocation's own options (active tempos, score threshold, model budget,
    /// Qualité A+). Same contract as above, its own blob.
    pub dislocation_options: Arc<RwLock<crate::dislocation::DislocationOptions>>,
    /// Apex — Micro Pullback's print-balance entries: options + executor runtime.
    /// The per-print measuring half lives in `MarketState::apex`.
    pub apex: Arc<crate::apex::ApexShared>,
    /// Dip — Micro Pullback's anticipated-level entries: options + executor runtime.
    /// The per-print measuring half lives in `MarketState::dip`.
    pub dip: Arc<crate::dip::DipShared>,
    /// Current active alerts (newest first, capped at 100).
    pub active_alerts: Arc<RwLock<Vec<AlertSignal>>>,
    /// Full alert history for the session (newest first, capped at 500).
    pub alert_history: Arc<RwLock<Vec<AlertSignal>>>,
    /// Live pre-open screener matches (replaced wholesale every scan pass;
    /// tickers disappear the instant they stop matching). Drives the pre-open tab.
    pub screener: Arc<RwLock<Vec<ScreenerMatch>>>,
    /// Market Attention top list (direction-agnostic, top 10, refreshed once a
    /// minute 09:30–12:30 ET). Consumed by the Perfect Pullback engine as its
    /// candidate source; exposed read-only via `get_market_attention` for debug.
    pub attention: Arc<RwLock<Vec<AttentionEntry>>>,
    /// Dislocation's Top 10 for every enabled tempo, refreshed on each closed
    /// 10-second market bucket. Read by its Settings diagnostics and by the chart
    /// pane, which needs the two tape thirds live rather than as of the last verdict.
    pub dislocation_attention: Arc<RwLock<Vec<crate::types::MicroAttentionEntry>>>,
    /// Per-zone trade context (SL, TP, tradeID) stored in RAM.
    pub chart: Arc<RwLock<ChartState>>,
    /// Internal simulated order book (no real broker orders in V1).
    pub internal_book: Arc<RwLock<InternalBook>>,
    /// Progressive per-symbol alert enrichment (float, country, classification,
    /// split, news, LLM reads), filled async when an alert is shown in a zone.
    pub enrichments: Arc<RwLock<HashMap<String, AlertEnrichment>>>,
    /// Progress of the current (or last) Panic Score LLM run over the visible
    /// pre-open watchlist — see `crate::panic_llm`. Button-triggered only; nothing
    /// schedules it, so this stays `Idle` unless the user asks for an analysis.
    pub panic_run: crate::panic_llm::RunState,
    /// Market Replay shared handle: status polled by the toolbar + command
    /// channel to the (single) replay engine task. The simulated clock itself is
    /// global (see `replay::clock` / `time::now`).
    pub replay: Arc<crate::replay::ReplayShared>,
    /// Flat-files download handle: status polled by the Gestion Flat Files modal +
    /// a cancel flag for the (single) background download task. See
    /// `crate::flat_files`.
    pub flat_files: Arc<crate::flat_files::FlatFilesShared>,
    /// Replay data preparation: status polled by the day picker + cancel flag. See
    /// `crate::replay_data`.
    pub replay_data: Arc<crate::replay_data::prepare::ReplayDataShared>,
    /// Speech-to-Text dictée pipeline: recorder + persisted job queue drained by the
    /// single STT worker. See `crate::stt`.
    pub stt: Arc<crate::stt::SttShared>,
    /// « Attention du marché », the detached second-monitor screen: its options,
    /// what its engine last published and the trade « Revue » is showing. The
    /// engine only polls while the window is enabled. See `crate::attention_map`.
    pub attention_map: Arc<crate::attention_map::AttentionShared>,
}
