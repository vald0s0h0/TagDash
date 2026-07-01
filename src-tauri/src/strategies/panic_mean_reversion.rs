// Panic Mean Reversion — a PRE-OPEN screener strategy (StrategyKind::Screener).
//
// The watchlist is built once per trading day at 09:00 ET by `crate::panic_watchlist`
// (the heavy logic lives in `crate::scoring`): a premarket-liquidity pre-filter
// (close > $1 AND premarket volume > 100k OR premarket $ volume > $500k OR 20-day
// average $ volume > $5M), then two rankings over the survivors —
//
//   • BB list — cumulative soft-Bollinger "area" (BBZ excess beyond 1.7σ summed over
//               the last 6 days), surfacing tickers stretched for several days.
//   • MA list — move since the last SMA20 contact (touch or gap-cross), normalised
//               by ATR20.
//
// The top 10 of each list are merged (a ticker kept once, in its better-ranked list)
// into a ≤20-row watchlist persisted to `panic_watchlist`. The scanner reads that
// table and surfaces one row per ticker, tagged "BB 4.2 ▲" / "MA 3.1 ▼".
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
    StrategyCard, StrategyContext, StrategyRiskConfig, UniverseKey,
};

/// Stable strategy id (shared with the scanner's screener-building block).
pub const ID: &str = "panic_mean_reversion";

const ENABLED: bool = true;
const PRIORITY: u8 = 5;
const MAX_RISK_DOLLARS: f64 = 100.0;

// The universe pre-filter (premarket liquidity) and the BB-area / move-since-SMA20
// rankings live in `crate::scoring`; this file only carries the strategy's identity
// (card, toggle, name, priority) read from the registry.

/// Pre-open watchlist strategy.
const SESSIONS: &[Session] = &[Session::PreOpen];

pub struct PanicMeanReversion;

impl ScanStrategy for PanicMeanReversion {
    fn id(&self) -> &'static str {
        ID
    }

    fn name(&self) -> &'static str {
        "Panic Mean Reversion"
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
            panes: vec![
                // Left — daily context with Bollinger bands.
                PaneSpec {
                    timeframe:   "daily".into(),
                    symbol:      None,
                    indicators:  vec![PaneIndicator {
                        kind:   IndicatorKind::BollingerBands,
                        period: Some(20),
                    }],
                    interactive: false,
                    column:      None,
                },
                // Right — 5-minute execution view; this is the tradeable pane.
                PaneSpec {
                    timeframe:   "5m".into(),
                    symbol:      None,
                    indicators:  vec![
                        PaneIndicator { kind: IndicatorKind::Vwap,          period: None },
                        PaneIndicator { kind: IndicatorKind::PreviousClose, period: None },
                        PaneIndicator { kind: IndicatorKind::PreviousHigh,  period: None },
                        PaneIndicator { kind: IndicatorKind::PreviousLow,   period: None },
                    ],
                    interactive: true,
                    column:      None,
                },
            ],
            // Strategy-specific overlay fields (top-left on the left pane): best
            // score (BB/MA) + its kind, then market cap and float when available.
            // Resolved per-zone from get_card_info; source = Alert so a missing
            // cap/float shows "—" (not a spinner). The user-triggered Deepseek read
            // (context/verdict) now lives in the shared info bar, not here.
            info_fields: vec![
                InfoField { key: "mr_score".into(),     label: "Score".into(),    source: InfoSource::Alert },
                InfoField { key: "market_cap".into(),   label: "MCap".into(),     source: InfoSource::Alert },
                InfoField { key: "float_shares".into(), label: "Float".into(),    source: InfoSource::Alert },
            ],
            // Declared so the enrichment band activates (float/news/classification
            // auto-fill); the actual LLM call is manual (see `run_alert_llm`), not
            // run by the auto enrichment pipeline. The template is documentary —
            // the prompt is built in `enrichment::run_panic_llm`.
            llm: Some(LlmSpec {
                prompt_template: "Contexte + verdict de mean-reversion pour {symbol} \
                                  (déclenché manuellement)".into(),
                produces:        vec!["llm_context".into(), "llm_reversion".into()],
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
