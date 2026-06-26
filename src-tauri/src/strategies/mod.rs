// Compiled Rust strategies. One file per strategy, all registered in registry.rs.
// A build.rs / macro will later auto-discover strategies at compile time.

use std::time::Duration;

use crate::types::{
    AlertSignal, Session, StrategyCard, StrategyContext, StrategyRiskConfig,
};

pub mod backside_parabolic;
pub mod hod_drive;
pub mod micro_pullback;
pub mod panic_mean_reversion;
pub mod perfect_pullback;
pub mod registry;

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
