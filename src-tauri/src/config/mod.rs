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
    /// Data-source policy. Flat files (`crate::flat_files`) and the Alpaca API are
    /// complementary sources rather than modes; the only setting is whether the app
    /// may use the network at all. See `DataSourceConfig`.
    #[serde(default)]
    pub data_source: DataSourceConfig,
    /// Where orders go and against which account. Independent of `data_source` and
    /// of the market-data subscription — see `BrokerConfig`.
    #[serde(default)]
    pub broker: BrokerConfig,
    /// Offline Speech-to-Text dictée pipeline (whisper.cpp) → trade notes / diary.
    /// See `crate::stt`.
    #[serde(default)]
    pub stt: SttConfig,
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
        Self {
            tags: default_journal_tags(),
        }
    }
}

fn default_journal_tags() -> Vec<String> {
    vec![
        "frd",
        "news",
        "low_float",
        "hod_break",
        "rvol_spike",
        "gap_up",
        "short_squeeze",
        "catalyst",
        "halt_resume",
        "reversal",
        "momentum",
        "pre_news",
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

fn default_true() -> bool {
    true
}
fn default_intel_ttl_hours() -> u64 {
    24
}
fn default_intel_batch_cap() -> usize {
    50
}
fn default_sec_rpm() -> f64 {
    300.0
}
fn default_massive_rpm() -> f64 {
    4.0
}
fn default_fmp_rpm() -> f64 {
    30.0
}

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

/// Data-source policy. There is no longer an "API mode" and a "flat-files mode":
/// the two sources cohabit and every read resolves to whichever can actually answer
/// — on-disk flat files first for anything historical, the Alpaca API to fill the
/// gaps, the live feed for the present. The API is the default.
///
/// `offline` is the one remaining switch, and it is narrow on purpose: it means "do
/// not reach for the network", not "use a different code path". Two reasons to want
/// it: no reachable API (no keys, no connection), and reproducible backtests — with
/// the network in play, whether a given day's gap gets filled depends on the
/// connection, so two passes over the same period would not be comparable.
/// A pre-merge `mode = "api" | "flat_files"` in an existing tagdash.toml is simply
/// ignored (serde skips unknown keys) and dropped on the next save. It is
/// deliberately NOT migrated to `offline = true`: the old flat-files mode mostly
/// meant "Alpaca isn't reachable", and that case now handles itself — every network
/// step reports itself skipped when its key is missing. Carrying it over would
/// forbid the API for a user who has keys, which is the opposite of the default.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DataSourceConfig {
    #[serde(default)]
    pub offline: bool,
    /// Manual override of the auto-detected Massive subscription tier
    /// ("unlimited" | "basic"), for when the automatic probe gets it wrong.
    /// `None` = automatic (see `crate::massive::plan::effective`).
    #[serde(default)]
    pub massive_tier_override: Option<String>,
}

// ─── Broker ─────────────────────────────────────────────────────────────────

/// Where orders go. THREE things are deliberately kept apart, because conflating
/// them is how you place a real order thinking you're in a simulator:
///
///   1. the BROKER and its account — this struct;
///   2. the trading ENVIRONMENT (paper vs real money) — `environment` here, and it
///      is the one the UI must shout about;
///   3. the MARKET DATA subscription (IEX vs SIP) — `alpaca::plan`, unrelated.
///
/// They do not constrain each other. A free IEX data plan can trade every ticker on
/// a real Alpaca account; a SIP subscription can drive a paper account. The only
/// pairing that is fixed is TagDash's own broker, which has no live counterpart
/// because it never sends an order anywhere.
///
/// Three accounts exist: `tagdash`/paper, `alpaca`/paper, `alpaca`/live.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrokerConfig {
    /// "tagdash" (internal simulator) | "alpaca".
    #[serde(default = "default_broker")]
    pub broker: String,
    /// "paper" | "live". Forced to paper for the TagDash broker.
    #[serde(default = "default_environment")]
    pub environment: String,
}

fn default_broker() -> String {
    "tagdash".into()
}
fn default_environment() -> String {
    "paper".into()
}

impl Default for BrokerConfig {
    fn default() -> Self {
        Self {
            broker: default_broker(),
            environment: default_environment(),
        }
    }
}

impl BrokerConfig {
    /// True when orders must be routed to Alpaca instead of the internal book.
    pub fn is_alpaca(&self) -> bool {
        self.broker == "alpaca"
    }

    /// True when a fill spends real money. The TagDash broker can never reach this,
    /// whatever `environment` says — it has no route to a real venue.
    pub fn is_live(&self) -> bool {
        self.is_alpaca() && self.environment == "live"
    }

    /// Stable identifier of the selected account, for logs and the UI.
    pub fn account_id(&self) -> String {
        if self.is_alpaca() {
            format!("alpaca:{}", if self.is_live() { "live" } else { "paper" })
        } else {
            "tagdash:paper".to_string()
        }
    }
}

/// Speech-to-Text settings. The whisper model is downloaded on first use into
/// `<app_dir>/models/`; `jargon` biases recognition toward trading terms (fed to
/// whisper as the `initial_prompt`), and the two `pause_*` thresholds let the
/// single STT worker step aside when the machine is busy or the cash open is
/// intense, so transcription never competes with the trading hot path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SttConfig {
    /// Master switch for the dictée pipeline.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Whisper model size: "small" (default, ~466 MB) or "medium" (~1.5 GB).
    #[serde(default = "default_stt_model")]
    pub model: String,
    /// Forced transcription language (ISO code). French by default; whisper still
    /// passes through the occasional English term (tickers, jargon).
    #[serde(default = "default_stt_language")]
    pub language: String,
    /// Trading vocabulary fed to whisper as the `initial_prompt` to bias detection
    /// (e.g. "halt" not "hault", "ticker" not "tiqueur").
    #[serde(default = "default_stt_jargon")]
    pub jargon: Vec<String>,
    /// Pause the worker while the global CPU usage is above this percentage.
    #[serde(default = "default_stt_pause_cpu")]
    pub pause_cpu_pct: f32,
    /// Pause the worker during the first N minutes after the 09:30 ET cash open
    /// (the most latency-sensitive window).
    #[serde(default = "default_stt_pause_open_min")]
    pub pause_market_open_minutes: u32,
    /// Preferred input device name; `None` = system default.
    #[serde(default)]
    pub input_device: Option<String>,
}

fn default_stt_model() -> String {
    "small".into()
}
fn default_stt_language() -> String {
    "fr".into()
}
fn default_stt_pause_cpu() -> f32 {
    85.0
}
fn default_stt_pause_open_min() -> u32 {
    30
}

fn default_stt_jargon() -> Vec<String> {
    [
        "halt",
        "halted",
        "resume",
        "HOD",
        "LOD",
        "VWAP",
        "float",
        "low float",
        "short",
        "squeeze",
        "bid",
        "ask",
        "spread",
        "ticker",
        "premarket",
        "gap up",
        "breakout",
        "pullback",
        "dilution",
        "RVOL",
        "stop loss",
        "take profit",
        "long",
        "short",
        "scalp",
        "runner",
        "catalyst",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

impl Default for SttConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            model: default_stt_model(),
            language: default_stt_language(),
            jargon: default_stt_jargon(),
            pause_cpu_pct: default_stt_pause_cpu(),
            pause_market_open_minutes: default_stt_pause_open_min(),
            input_device: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeTallyConfig {
    pub api_base_url: String,
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
                api_base_url: "https://trade.fabrelexos.synology.me".into(),
            },
            journal: JournalConfig::default(),
            company_intel: CompanyIntelConfig::default(),
            data_source: DataSourceConfig::default(),
            broker: BrokerConfig::default(),
            stt: SttConfig::default(),
        }
    }
}

// ─── Paths ──────────────────────────────────────────────────────────────────

/// Returns `<system_config_dir>/tagdash/`, creating it if absent.
pub fn app_dir() -> PathBuf {
    let base = config_dir().unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let dir = base.join("tagdash");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

pub fn config_path(app_dir: &Path) -> PathBuf {
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
            Ok(cfg) => {
                // Rewrite through the current schema so obsolete keys from older
                // releases cannot remain active or reappear in copied configs.
                let _ = save(&path, &cfg);
                (dir, cfg)
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// An existing tagdash.toml still carries `mode = "api" | "flat_files"`. It must
    /// load without erroring, and it must leave the app ONLINE: the API is the
    /// default, and a missing key is what disables a provider now — not a mode.
    #[test]
    fn a_legacy_mode_key_loads_and_is_ignored() {
        for legacy in [r#"mode = "flat_files""#, r#"mode = "api""#] {
            let ds: DataSourceConfig = toml::from_str(legacy).expect("legacy config must load");
            assert!(!ds.offline, "{legacy} must not force offline");
        }
    }

    #[test]
    fn a_fresh_config_defaults_to_online() {
        let ds: DataSourceConfig = toml::from_str("").unwrap();
        assert!(!ds.offline);
        assert!(!DataSourceConfig::default().offline);
    }

    /// An explicit `offline` survives a save/load round trip, and the legacy key is
    /// gone from what we write back.
    #[test]
    fn offline_round_trips_and_drops_the_legacy_key() {
        let ds: DataSourceConfig = toml::from_str("offline = true\nmode = \"flat_files\"").unwrap();
        assert!(ds.offline);
        let out = toml::to_string(&ds).unwrap();
        assert!(out.contains("offline = true"));
        assert!(
            !out.contains("mode"),
            "legacy key must not be written back: {out}"
        );
        assert!(toml::from_str::<DataSourceConfig>(&out).unwrap().offline);
    }
}
