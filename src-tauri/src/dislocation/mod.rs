// Dislocation — the cross-sectional attention engine.
//
// One question, asked of the whole eligible universe every ten seconds: **which
// ticker is the market dislocating onto right now, and is there something real
// behind it?** The answer is a single 0-100 score made of three equal thirds —
//
//   RVOL       the ticker against its own past          (`score::rvol_score`)
//   ATTENTION  the ticker against the market right now  (`score::attention_score`)
//   NEWS       the catalyst, graded by a model          (`score::news_score`)
//
// — and nothing else. No bonuses, no penalties, no correcting terms. Whatever the
// tape does not say and the model does not say is simply not in the number.
//
// HISTORY, BECAUSE IT EXPLAINS THE LAYOUT. This engine grew inside
// `micro_pullback` as "v2" and shared its module, its options blob and its strategy
// id with a completely different detector. They are not variants of one idea: Micro
// Pullback asks "does this ticker clear my bars?" and answers with a boolean
// cascade; Dislocation asks "which of all these tickers is best right now?" and
// answers with a ranking. Two questions, two engines, two names — as of today the
// second one is called Dislocation and nothing else.
//
// WHAT LIVES WHERE
//   engine.rs      the tick loop: observe, rank, spend the model budget, publish
//   score.rs       the three thirds, pure and testable
//   quality.rs     the qualification task: news pass, optional deep pass, verdict
//   live_status.rs what the pane shows while a model call is in flight
//   intel_live.rs  the provider gather behind the deep pass
//   options.rs     the Settings panel's persisted blob

pub mod engine;
pub mod intel_live;
pub mod live_status;
pub mod options;
pub mod quality;
pub mod score;

pub use options::DislocationOptions;

use chrono::{DateTime, Utc};

/// Active window in ET wall-clock minutes since midnight: 04:00–20:00, the whole
/// extended trading day (premarket → pre-open → open → afterhours).
const SESSION_START_MIN: u32 = 240;
const SESSION_END_MIN: u32 = 1200;

/// Whether `now` falls inside the window the engine runs in.
pub fn in_active_session(now: DateTime<Utc>) -> bool {
    (SESSION_START_MIN..SESSION_END_MIN).contains(&crate::time::et_minutes(now))
}

/// A tempo length as the trader writes it: `10s`, `40s`, `2m`.
pub fn fmt_tempo(secs: i64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m", secs / 60)
    }
}

/// Spawn the engine loop. Returns immediately.
pub struct DislocationEngine;

impl DislocationEngine {
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        running: std::sync::Arc<std::sync::atomic::AtomicBool>,
        market: std::sync::Arc<std::sync::RwLock<crate::market_state::MarketState>>,
        db: std::sync::Arc<std::sync::Mutex<rusqlite::Connection>>,
        secrets: std::sync::Arc<std::sync::RwLock<crate::config::secrets::Secrets>>,
        active_alerts: std::sync::Arc<std::sync::RwLock<Vec<crate::types::AlertSignal>>>,
        alert_history: std::sync::Arc<std::sync::RwLock<Vec<crate::types::AlertSignal>>>,
        strategy_enabled: std::sync::Arc<std::sync::RwLock<std::collections::HashMap<String, bool>>>,
        options: std::sync::Arc<std::sync::RwLock<DislocationOptions>>,
        app_config: std::sync::Arc<std::sync::RwLock<crate::config::AppConfig>>,
        attention: std::sync::Arc<std::sync::RwLock<Vec<crate::types::MicroAttentionEntry>>>,
    ) {
        engine::start(
            running,
            market,
            db,
            secrets,
            active_alerts,
            alert_history,
            strategy_enabled,
            options,
            app_config,
            attention,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn the_window_is_the_whole_extended_day() {
        let at = |h, m| {
            crate::time::et_clock_utc(
                Utc.with_ymd_and_hms(2026, 8, 21, 12, 0, 0).unwrap(),
                h,
                m,
            )
        };
        assert!(!in_active_session(at(3, 59)));
        assert!(in_active_session(at(4, 0)));
        assert!(in_active_session(at(9, 30)));
        assert!(in_active_session(at(19, 59)));
        assert!(!in_active_session(at(20, 0)));
    }

    #[test]
    fn tempo_labels_read_the_way_the_trader_writes_them() {
        assert_eq!(fmt_tempo(10), "10s");
        assert_eq!(fmt_tempo(40), "40s");
        assert_eq!(fmt_tempo(60), "1m");
        assert_eq!(fmt_tempo(300), "5m");
    }
}
