// Panic Mean Reversion — a PRE-OPEN screener strategy (StrategyKind::Screener).
//
// The watchlist refreshes during premarket and is finalised at 09:25 ET by `crate::panic_watchlist`
// (the heavy logic lives in `crate::scoring`): a premarket-liquidity pre-filter
// (close > $1 AND premarket volume > 100k OR premarket $ volume > $500k OR 20-day
// average $ volume > $5M), then one panic ranking over the survivors:
//
//   • multiday and current-PM extensions combine sigma, sqrt(ATR expansion),
//     pre-shock stability and one-way volume-backed flow;
//   • only names clearing the absolute σ and ATR-expansion thresholds survive;
//   • a current-day premarket shock can qualify independently of prior daily moves;
//   • stability and one-way volume-backed flow adjust the global quality ranking.
//
// The resulting ≤30-row watchlist is persisted by day. The scanner reads that table
// and surfaces one row per ticker with its winning window and move direction.
// `should_alert` here is intentionally a no-op; the strategy still owns its identity
// card (panes/indicators), toggle, name and priority, read from the registry.
//
// Charts (two panes):
//   • left  — daily with Bollinger bands (context: how stretched the move is).
//   • right — 5-minute execution view with VWAP, previous-day close (key level),
//             previous-day high & low (secondary levels).

use std::time::Duration;

use crate::strategies::{ScanStrategy, StrategyKind};
use crate::types::{
    AlertSignal, IndicatorKind, InfoField, InfoSource, LlmSpec, PaneIndicator, PaneSpec, Session,
    StrategyCard, StrategyContext, StrategyRiskConfig, StreamingScope, UniverseKey,
};

/// Stable strategy id (shared with the scanner's screener-building block).
pub const ID: &str = "panic_mean_reversion";

const ENABLED: bool = true;
const PRIORITY: u8 = 5;
const MAX_RISK_DOLLARS: f64 = 100.0;

// The universe pre-filter, panic score and size-bucket ranking live in
// `crate::scoring`; this file only carries the strategy's identity (card, toggle,
// name, priority) read from the registry.

/// Pre-open watchlist strategy.
const SESSIONS: &[Session] = &[Session::PreOpen];

pub struct PanicMeanReversion;

impl ScanStrategy for PanicMeanReversion {
    fn id(&self) -> &'static str {
        ID
    }

    fn name(&self) -> &'static str {
        "Panic"
    }

    fn enabled(&self) -> bool {
        ENABLED
    }

    fn sessions(&self) -> &'static [Session] {
        SESSIONS
    }

    fn priority(&self) -> u8 {
        PRIORITY
    }

    fn cooldown(&self) -> Duration {
        Duration::from_secs(60) // unused for screeners, kept for the trait
    }

    fn kind(&self) -> StrategyKind {
        StrategyKind::Screener
    }

    fn risk_config(&self) -> StrategyRiskConfig {
        StrategyRiskConfig {
            max_risk_dollars: MAX_RISK_DOLLARS,
            ..Default::default()
        }
    }

    fn card(&self) -> StrategyCard {
        StrategyCard {
            universe: UniverseKey::UsStocks,
            // capped_watch_list (alpaca::stream) already spends its 3rd tier on this
            // strategy's daily watchlist unconditionally — this documents that fact
            // rather than changing behaviour.
            streaming_scope: StreamingScope::Watchlist,
            panes: vec![
                // Left — daily context with Bollinger bands.
                PaneSpec {
                    timeframe: "daily".into(),
                    symbol: None,
                    indicators: vec![PaneIndicator {
                        kind: IndicatorKind::BollingerBands,
                        period: Some(20),
                    }],
                    interactive: false,
                    column: None,
                },
                // Right — 5-minute execution view; this is the tradeable pane.
                PaneSpec {
                    timeframe: "5m".into(),
                    symbol: None,
                    indicators: vec![
                        PaneIndicator {
                            kind: IndicatorKind::Vwap,
                            period: None,
                        },
                        PaneIndicator {
                            kind: IndicatorKind::PreviousClose,
                            period: None,
                        },
                        PaneIndicator {
                            kind: IndicatorKind::PreviousHigh,
                            period: None,
                        },
                        PaneIndicator {
                            kind: IndicatorKind::PreviousLow,
                            period: None,
                        },
                    ],
                    interactive: true,
                    column: None,
                },
            ],
            // Strategy-specific overlay fields (top-left on the left pane): best
            // panic score + winning window, then market cap and float when available.
            // Resolved per-zone from get_card_info; source = Alert so a missing
            // cap/float shows "—" (not a spinner). The user-triggered Deepseek read
            // (context/verdict) now lives in the shared info bar, not here.
            info_fields: vec![
                InfoField {
                    key: "mr_score".into(),
                    label: "Score".into(),
                    source: InfoSource::Alert,
                },
                InfoField {
                    key: "market_cap".into(),
                    label: "MCap".into(),
                    source: InfoSource::Alert,
                },
                InfoField {
                    key: "float_shares".into(),
                    label: "Float".into(),
                    source: InfoSource::Alert,
                },
            ],
            // Declared so the enrichment band activates (float/news/classification
            // auto-fill); the actual LLM call is manual (see `run_alert_llm`), not
            // run by the auto enrichment pipeline. The template is documentary —
            // the prompt is built in `enrichment::run_panic_llm`.
            llm: Some(LlmSpec {
                prompt_template: "Contexte + verdict de mean-reversion pour {symbol} \
                                  (déclenché manuellement)"
                    .into(),
                produces: vec!["llm_context".into(), "llm_reversion".into()],
            }),
            enrichments: vec![],
        }
    }

    /// No per-tick matching: the screener rows come from the precomputed daily
    /// `panic_watchlist` table (built by `crate::panic_watchlist`, surfaced by the
    /// scanner). See the module header.
    fn should_alert(&self, _ctx: &StrategyContext) -> Option<AlertSignal> {
        None
    }
}
