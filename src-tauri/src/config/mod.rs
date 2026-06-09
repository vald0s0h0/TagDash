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
