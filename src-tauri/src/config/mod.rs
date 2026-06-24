// Runtime configuration.
// Source of truth: <app_dir>/tagdash.toml.
// Falls back to compiled defaults if the file is absent or malformed.
// Writes a default file on first run so the user can find and edit it.

pub mod secrets;

use dirs::config_dir;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

// ─── Structs ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub trading: TradingConfig,
    pub alpaca: AlpacaConfig,
    pub universe: UniverseConfig,
    pub ui: UiConfig,
    pub latency: LatencyConfig,
    pub tradetally: TradeTallyConfig,
    #[serde(default)]
    pub journal: JournalConfig,
    /// "Company intelligence" collection job (short interest, financials, dilution
    /// filings, ownership). Isolated background job — see `crate::company_intel`.
    #[serde(default)]
    pub company_intel: CompanyIntelConfig,
    /// Where market data comes from: the live Alpaca API ("api") or pre-downloaded
    /// on-disk flat files ("flat_files"). In flat-files mode there is no real-time
    /// feed — the platform runs in Market Replay against the stored days. See
    /// `crate::flat_files`.
    #[serde(default)]
    pub data_source: DataSourceConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradingConfig {
    pub default_broker: String,
    pub default_account: String,
    /// Per-share commission in dollars. Commission for an execution is
    /// `|quantity| * commission_per_share` — there is no fixed per-trade fee.
    #[serde(default = "default_commission_per_share")]
    pub commission_per_share: f64,
    #[serde(default)]
    pub default_fees: f64,
    pub min_position_size: u32,
    pub max_position_size: u32,
}

fn default_commission_per_share() -> f64 {
    0.0007
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlpacaConfig {
    pub feed: String,
    pub use_news: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UniverseConfig {
    /// Float ceiling (shares) for the "Low Float" streaming universe.
    /// No market-cap / price / volume filter — float only.
    #[serde(default = "default_low_float_max")]
    pub low_float_max: u64,
}

fn default_low_float_max() -> u64 {
    30_000_000
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiConfig {
    pub default_theme: String,
    pub premarket_zones_per_tab: u8,
    pub pre_open_zones_per_tab: u8,
    pub open_zones_per_tab: u8,
    pub auto_create_tabs: bool,
    /// Send a native OS notification (Windows toast / macOS Notification Center)
    /// whenever a scanner alert fires, regardless of the active tab. Opt-in.
    /// `#[serde(default)]` so configs written before this field still load.
    #[serde(default)]
    pub desktop_alerts: bool,
    /// When to flash the full-screen white overlay on a new scanner alert, so the
    /// user notices it even with other windows covering TagDash. One of
    /// "off" | "premarket" | "open" | "both" (matched against the alert's session).
    #[serde(default = "default_attention_mode")]
    pub flash_alerts: String,
    /// When to force the TagDash window back to the foreground on a new scanner
    /// alert. Same scale as `flash_alerts`.
    #[serde(default = "default_attention_mode")]
    pub foreground_alerts: String,
    /// When to play a notification sound on a new scanner alert. Same scale as
    /// `flash_alerts`. The sound itself is synthesized & played in the frontend.
    #[serde(default = "default_attention_mode")]
    pub alert_sound_mode: String,
    /// Which notification sound to play (id from the frontend sound catalog).
    #[serde(default = "default_alert_sound")]
    pub alert_sound: String,
}

fn default_attention_mode() -> String {
    "off".into()
}

fn default_alert_sound() -> String {
    "soft_chime".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatencyConfig {
    pub warn_ms: u32,
    pub critical_ms: u32,
}

/// Journal settings. `tags` is a user-defined list shown in the Journal modal
/// (replaces the tags previously fetched from TradeTally at startup).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalConfig {
    #[serde(default = "default_journal_tags")]
    pub tags: Vec<String>,
}

impl Default for JournalConfig {
    fn default() -> Self {
        Self { tags: default_journal_tags() }
    }
}

fn default_journal_tags() -> Vec<String> {
    vec![
        "frd", "news", "low_float", "hod_break", "rvol_spike", "gap_up",
        "short_squeeze", "catalyst", "halt_resume", "reversal", "momentum", "pre_news",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// Settings for the company-intelligence collection job. Per-provider rate
/// limits (requests/minute), the cache TTL (how stale a ticker may be before it's
/// recollected) and a per-run cap (so a startup pass never tries the whole
/// universe at once). All `#[serde(default)]` so older configs still load.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompanyIntelConfig {
    /// Master switch for the background collection job.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Re-collect a ticker only when its cached record is older than this many
    /// hours (TTL). 0 = always refresh.
    #[serde(default = "default_intel_ttl_hours")]
    pub ttl_hours: u64,
    /// Max tickers processed per collection run (bounds a startup pass; the rest
    /// are picked up on later runs / by the future background worker).
    #[serde(default = "default_intel_batch_cap")]
    pub max_tickers_per_run: usize,
    /// SEC EDGAR budget (req/min). SEC allows ~10 req/s; stay well under.
    #[serde(default = "default_sec_rpm")]
    pub sec_rpm: f64,
    /// Massive budget (req/min). Free tier ≈ 1 req / 13 s ⇒ ~5 req/min.
    #[serde(default = "default_massive_rpm")]
    pub massive_rpm: f64,
    /// FMP budget (req/min). Free tier is small; keep it gentle.
    #[serde(default = "default_fmp_rpm")]
    pub fmp_rpm: f64,
}

fn default_true() -> bool { true }
fn default_intel_ttl_hours() -> u64 { 24 }
fn default_intel_batch_cap() -> usize { 50 }
fn default_sec_rpm() -> f64 { 300.0 }
fn default_massive_rpm() -> f64 { 4.0 }
fn default_fmp_rpm() -> f64 { 30.0 }

impl Default for CompanyIntelConfig {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            ttl_hours: default_intel_ttl_hours(),
            max_tickers_per_run: default_intel_batch_cap(),
            sec_rpm: default_sec_rpm(),
            massive_rpm: default_massive_rpm(),
            fmp_rpm: default_fmp_rpm(),
        }
    }
}

/// Data-source selection. `mode` is "api" (live Alpaca feed) or "flat_files"
/// (offline replay from `<app_dir>/flat_files/flat-YYYY-MM-DD.db`). Stored in
/// tagdash.toml so the choice survives a restart.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataSourceConfig {
    #[serde(default = "default_data_mode")]
    pub mode: String,
}

fn default_data_mode() -> String {
    "api".into()
}

impl Default for DataSourceConfig {
    fn default() -> Self {
        Self { mode: default_data_mode() }
    }
}

impl DataSourceConfig {
    /// True when running off pre-downloaded flat files (no live feed).
    pub fn is_flat_files(&self) -> bool {
        self.mode == "flat_files"
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeTallyConfig {
    pub api_base_url:  String,
    /// Set true to simulate TradeTally responses without real HTTP calls.
    #[serde(default)]
    pub mock_mode:     bool,
    /// In mock mode, simulate HTTP failures.
    #[serde(default)]
    pub mock_fail:     bool,
    /// In mock mode, add artificial latency (ms).
    #[serde(default)]
    pub mock_delay_ms: u64,
}

// ─── Defaults ───────────────────────────────────────────────────────────────

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            trading: TradingConfig {
                default_broker: "TagDash".into(),
                default_account: "TagDash Sim".into(),
                commission_per_share: default_commission_per_share(),
                default_fees: 0.0,
                min_position_size: 1,
                max_position_size: 10_000,
            },
            alpaca: AlpacaConfig {
                feed: "sip".into(),
                use_news: true,
            },
            universe: UniverseConfig {
                low_float_max: 30_000_000,
            },
            ui: UiConfig {
                default_theme: "dark".into(),
                premarket_zones_per_tab: 1,
                pre_open_zones_per_tab: 1,
                open_zones_per_tab: 4,
                auto_create_tabs: true,
                desktop_alerts: false,
                flash_alerts: default_attention_mode(),
                foreground_alerts: default_attention_mode(),
                alert_sound_mode: default_attention_mode(),
                alert_sound: default_alert_sound(),
            },
            latency: LatencyConfig {
                warn_ms: 1_000,
                critical_ms: 2_000,
            },
            tradetally: TradeTallyConfig {
                api_base_url:  "https://trade.fabrelexos.synology.me".into(),
                mock_mode:     false,
                mock_fail:     false,
                mock_delay_ms: 0,
            },
            journal: JournalConfig::default(),
            company_intel: CompanyIntelConfig::default(),
            data_source: DataSourceConfig::default(),
        }
    }
}

// ─── Paths ──────────────────────────────────────────────────────────────────

/// Returns `<system_config_dir>/tagdash/`, creating it if absent.
pub fn app_dir() -> PathBuf {
    let base = config_dir()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let dir = base.join("tagdash");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn config_path(app_dir: &Path) -> PathBuf {
    app_dir.join("tagdash.toml")
}

// ─── Load / Save ────────────────────────────────────────────────────────────

/// Load config from file, create defaults on first run, return (app_dir, config).
pub fn load() -> (PathBuf, AppConfig) {
    let dir = app_dir();
    let path = config_path(&dir);

    if !path.exists() {
        let config = AppConfig::default();
        let _ = save(&path, &config);
        return (dir, config);
    }

    match std::fs::read_to_string(&path) {
        Ok(content) => match toml::from_str::<AppConfig>(&content) {
            Ok(cfg) => (dir, cfg),
            Err(_) => {
                eprintln!("[tagdash] tagdash.toml parse error, using defaults");
                (dir, AppConfig::default())
            }
        },
        Err(_) => (dir, AppConfig::default()),
    }
}

/// Persist config to disk. Called by update_local_config command.
pub fn save(path: &Path, config: &AppConfig) -> Result<(), String> {
    let content = toml::to_string_pretty(config).map_err(|e| e.to_string())?;
    std::fs::write(path, content).map_err(|e| e.to_string())
}
