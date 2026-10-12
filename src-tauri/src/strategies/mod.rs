// Compiled Rust strategies. One file per strategy, all registered in registry.rs.
// A build.rs / macro will later auto-discover strategies at compile time.

use std::time::Duration;

use crate::types::{AlertSignal, Session, StrategyCard, StrategyContext, StrategyRiskConfig};

pub mod backside;
pub mod dislocation;
pub mod hod_drive;
pub mod issuer_profile;
pub mod micro_pullback;
pub mod panic_mean_reversion;
pub mod perfect_pullback;
pub mod registry;

/// The setup name a trade is filed under in TradeTally, from its strategy id.
/// Apex and Dip are not in the registry (they are hotkey engines inside Micro
/// Pullback, not scanners) but have their own setup names; an unknown id is passed
/// through rather than invented.
pub fn setup_name(strategy_id: &str) -> String {
    if strategy_id == crate::apex::STRATEGY_ID {
        return crate::apex::SETUP_NAME.to_string();
    }
    if strategy_id == crate::dip::STRATEGY_ID {
        return crate::dip::SETUP_NAME.to_string();
    }
    registry::all_strategies()
        .iter()
        .find(|s| s.id() == strategy_id)
        .map(|s| s.name().to_string())
        .unwrap_or_else(|| strategy_id.to_string())
}

/// Whether a strategy's engine runs right now: its Settings toggle — or, during a
/// Market Replay, its being one of the strategies the session was prepared for. A
/// session prepared for a strategy must alert on it even when the trader keeps that
/// strategy switched off live; the replay is a sandbox and leaves the toggle alone.
pub fn runs(enabled: &std::collections::HashMap<String, bool>, strategy_id: &str) -> bool {
    crate::replay::clock::session_runs(strategy_id)
        || enabled.get(strategy_id).copied().unwrap_or(true)
}

/// Letters and digits only, lower-cased: "Micro Pullback", "micro_pullback" and
/// "MICRO-PULLBACK" are the same label.
fn label_key(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// Every strategy id with its TradeTally setup name, the hotkey engines included.
fn ids_and_names() -> Vec<(&'static str, &'static str)> {
    let mut out: Vec<(&'static str, &'static str)> = registry::all_strategies()
        .iter()
        .map(|s| (s.id(), s.name()))
        .collect();
    out.push((crate::apex::STRATEGY_ID, crate::apex::SETUP_NAME));
    out.push((crate::dip::STRATEGY_ID, crate::dip::SETUP_NAME));
    out
}

/// The SCANNER a strategy id belongs to: itself, except the hotkey engines, whose
/// tickers are always their owner's (an Apex or Dip trade is taken on a ticker
/// Micro Pullback alerted).
pub fn scanner_of_id(strategy_id: &str) -> &str {
    if strategy_id == crate::apex::STRATEGY_ID {
        crate::apex::OWNER_STRATEGY_ID
    } else if strategy_id == crate::dip::STRATEGY_ID {
        crate::dip::OWNER_STRATEGY_ID
    } else {
        strategy_id
    }
}

/// The scanner behind a label as a trade carries it — a setup name, a tag or an id,
/// in any casing ("Apex", "micro_pullback", "HOD Drive"). None for anything else.
pub fn scanner_of_label(label: &str) -> Option<&'static str> {
    let key = label_key(label);
    ids_and_names()
        .into_iter()
        .find(|(id, name)| label_key(id) == key || label_key(name) == key)
        .map(|(id, _)| scanner_of_id(id))
}

/// The scanner behind a trade id TagDash generated (`chart_state::gen_trade_id`
/// embeds the strategy id it was opened under). Unlike a setup or a tag, the id is
/// never edited afterwards. None for an id of another shape.
pub fn scanner_of_trade_id(trade_id: &str) -> Option<&'static str> {
    // `<time>-<SYMBOL>-<STRATEGY>-<seq>`, read from the end: a symbol may hold a
    // dash, the last two parts cannot.
    let strategy = trade_id.rsplitn(3, '-').nth(1)?;
    ids_and_names()
        .into_iter()
        .find(|(id, _)| id.to_uppercase().replace('-', "_") == strategy)
        .map(|(id, _)| scanner_of_id(id))
}

/// How the scanner treats a strategy's matches.
/// - `Alert`    → cooldown'd, deduplicated, appended to the alert list (an event
///                that "just happened"). Used by premarket / open strategies.
/// - `Screener` → re-evaluated every pass; the full set of *currently* matching
///                tickers replaces the live screener list (so a ticker disappears
///                the instant it stops matching). Used by pre-open strategies that
///                build a watchlist for the open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrategyKind {
    Alert,
    Screener,
}

/// Contract every compiled strategy must implement.
pub trait ScanStrategy: Send + Sync {
    /// Stable lowercase snake_case identifier.
    fn id(&self) -> &'static str;
    /// Human-readable display name.
    fn name(&self) -> &'static str;
    /// Master on/off toggle defined at the top of each strategy file.
    fn enabled(&self) -> bool;
    /// Sessions during which this strategy is active.
    fn sessions(&self) -> &'static [Session];
    /// Alert priority 1 (low) … 5 (critical).
    fn priority(&self) -> u8;
    /// Risk parameters (max risk dollars per trade).
    fn risk_config(&self) -> StrategyRiskConfig;
    /// Full identity card: universe, panes, indicators, info band, LLM &
    /// enrichment needs. Co-located with the strategy's rules in its file.
    fn card(&self) -> StrategyCard;
    /// Evaluate one ticker. Returns Some(alert) when conditions are met.
    fn should_alert(&self, ctx: &StrategyContext) -> Option<AlertSignal>;
    /// Whether this ticker currently matches — used by Screener-kind strategies,
    /// for which the scanner builds the `ScreenerMatch` straight from the context
    /// (so no `AlertSignal` is constructed). Defaults to `should_alert(...).is_some()`
    /// for Alert strategies; screeners override it with a cheap boolean predicate.
    fn matches(&self, ctx: &StrategyContext) -> bool {
        self.should_alert(ctx).is_some()
    }
    /// Minimum time between two alerts for the same symbol via this strategy.
    fn cooldown(&self) -> Duration;
    /// Whether this strategy feeds the live screener (pre-open watchlist) or the
    /// cooldown'd alert list. Defaults to `Alert`; pre-open strategies override.
    fn kind(&self) -> StrategyKind {
        StrategyKind::Alert
    }
}

#[cfg(test)]
mod setup_name_tests {
    #[test]
    fn apex_is_filed_as_apex_not_micro_pullback() {
        assert_eq!(super::setup_name("apex"), "Apex");
        assert_eq!(super::setup_name("dip"), "Dip");
        assert_eq!(
            super::setup_name(super::micro_pullback::ID),
            super::registry::all_strategies()
                .iter()
                .find(|s| s.id() == super::micro_pullback::ID)
                .unwrap()
                .name()
        );
        assert_eq!(super::setup_name("inconnue"), "inconnue");
    }

    #[test]
    fn apex_and_dip_trades_belong_to_the_micro_pullback_scanner() {
        let mp = super::micro_pullback::ID;
        assert_eq!(super::scanner_of_label("Apex"), Some(mp));
        assert_eq!(super::scanner_of_label("dip"), Some(mp));
        assert_eq!(super::scanner_of_label("Micro Pullback"), Some(mp));
        assert_eq!(super::scanner_of_label("micro_pullback"), Some(mp));
        assert_eq!(super::scanner_of_label("HOD Drive"), Some(super::hod_drive::ID));
        assert_eq!(super::scanner_of_label("fomo"), None, "a mood tag is not a strategy");
        // The id TagDash filed the trade under, whatever its tags became since.
        assert_eq!(super::scanner_of_trade_id("260629122255-NXTS-APEX-5"), Some(mp));
        assert_eq!(super::scanner_of_trade_id("260624112907-FRTT-DIP-c"), Some(mp));
        assert_eq!(super::scanner_of_trade_id("260629114902-MNTS-MICRO_PULLBACK-2"), Some(mp));
        assert_eq!(super::scanner_of_trade_id("260629122255-DIP-HOD_DRIVE-5"), Some(super::hod_drive::ID));
        assert_eq!(super::scanner_of_trade_id("b796775e-d1d1-41d1"), None);
    }
}
