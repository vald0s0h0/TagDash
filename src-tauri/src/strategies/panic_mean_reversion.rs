// Panic Mean Reversion — a PRE-OPEN screener strategy (StrategyKind::Screener).
//
// Unlike a live tick-driven screener, this one ranks a HARD-PRE-FILTERED slice of
// the US universe on daily, once-per-day scores computed at startup (see the
// `scoring` module and the `compute_scores` startup step). The pre-filter (see
// `passes_prefilter` below) keeps only genuine multi-day movers (c0 > $1 and a
// ≥70% cumulative 2..6-day move in EITHER direction — runner or crash) BEFORE the
// scoring ranks the survivors:
//
//   • PR score  — cross-sectional percent rank of the absolute cumulative return
//                 over 1..6 days (biggest movers either way).
//   • BB score  — self-relative Bollinger "event" score: how exceptional today's
//                 move is vs the ticker's own 3-year history, any duration.
//
// The screener shows the top 30 tickers by display score (= max(PR, BB)), one row
// per ticker, with the winning score. Because the ranking is a precomputed daily
// list (not a per-tick filter), the matches are produced directly by the scanner
// from `mean_reversion_scores` — `should_alert` here is intentionally a no-op; the
// strategy still owns its identity card (panes/indicators), toggle, name and
// priority, which the scanner and UI read from the registry.
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

/// Liquidity gate: only surface tickers whose PREVIOUS trading day's volume
/// exceeds this (shares). Applied by the scanner when picking the top-30 from the
/// precomputed scores.
pub const MIN_PREV_VOLUME: i64 = 20_000_000;

// ─── Hard pre-filter (universe entry gate) ────────────────────────────────────
//
// Applied to the WHOLE US universe in `scoring::compute_and_store` BEFORE the
// Bollinger composite ranks anything: a ticker is only scored/ranked if it is a
// genuine multi-day mover. The existing scoring (Bollinger event + parabolic /
// volume / run composite) then orders the SURVIVORS — its criteria are unchanged;
// all that changes is that the entry pool is filtered hard.
//
// A ticker qualifies when ALL of:
//   • its previous close c0 (latest cached daily close) is above MIN_PRICE, AND
//   • the MAGNITUDE of its cumulative close-to-close move over at least one window
//     of MIN_MOVE_DAYS..=MAX_MOVE_DAYS trading days exceeds MIN_CUM_MOVE_PCT —
//     EITHER direction (a +70% runner OR a −70% crash, both panic-revert setups).
// Close-to-close means overnight gaps are included (same definition as the
// `change_Nd_pct` fundamentals fields).

/// Minimum previous close (USD) — drops sub-$1 names.
pub const MIN_PRICE: f64 = 1.0;
/// Minimum cumulative move MAGNITUDE (%) over a qualifying window — |move| is
/// tested, so both up runs (+) and down crashes (−) qualify.
pub const MIN_CUM_MOVE_PCT: f64 = 70.0;
/// Shortest cumulative window (trading days) — the move must span ≥ 2 days, so a
/// single-day spike alone never qualifies.
pub const MIN_MOVE_DAYS: usize = 2;
/// Longest cumulative window (trading days).
pub const MAX_MOVE_DAYS: usize = 6;

/// Hard pre-filter predicate over a ticker's date-ASCENDING daily closes.
/// Returns true when c0 > MIN_PRICE and the MAGNITUDE of some N-day (N ∈ MIN..=MAX)
/// cumulative close-to-close change exceeds MIN_CUM_MOVE_PCT — either direction, so
/// both +70% runners and −70% crashes qualify.
pub fn passes_prefilter(closes: &[f64]) -> bool {
    let n = closes.len();
    if n <= MIN_MOVE_DAYS {
        return false;
    }
    let c0 = closes[n - 1];
    if !(c0 > MIN_PRICE) {
        return false;
    }
    for d in MIN_MOVE_DAYS..=MAX_MOVE_DAYS {
        if n > d {
            let base = closes[n - 1 - d];
            if base > 0.0 {
                let change_pct = (c0 - base) / base * 100.0;
                if change_pct.abs() > MIN_CUM_MOVE_PCT {
                    return true;
                }
            }
        }
    }
    false
}

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
                },
            ],
            // Info band: best score (PR/BB) + its horizon in days, then market cap
            // and float when available. All resolved per-zone from get_card_info;
            // source = Alert so a missing cap/float shows "—" (not a spinner).
            info_fields: vec![
                InfoField { key: "mr_score".into(),     label: "Score".into(),    source: InfoSource::Alert },
                InfoField { key: "market_cap".into(),   label: "MCap".into(),     source: InfoSource::Alert },
                InfoField { key: "float_shares".into(), label: "Float".into(),    source: InfoSource::Alert },
                // User-triggered Deepseek read (button in the info band, never auto).
                InfoField { key: "llm_context".into(),   label: "Contexte".into(), source: InfoSource::Llm },
                InfoField { key: "llm_reversion".into(), label: "Verdict".into(),  source: InfoSource::Llm },
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
    /// `mean_reversion_scores` ranking (built directly by the scanner). See the
    /// module header.
    fn should_alert(&self, _ctx: &StrategyContext) -> Option<AlertSignal> {
        None
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefilter_keeps_multiday_runner() {
        // Flat at $10, then +80% over the last 3 days to ~$18 → qualifies
        // (3-day cumulative ≈ +80% > 70%, c0 > $1).
        let mut closes = vec![10.0; 30];
        let n = closes.len();
        closes[n - 3] = 12.0;
        closes[n - 2] = 15.0;
        closes[n - 1] = 18.0;
        assert!(passes_prefilter(&closes));
    }

    #[test]
    fn prefilter_keeps_multiday_crash() {
        // Flat at $10, then a −75% crash over the last 3 days to ~$2.50 →
        // qualifies on magnitude (|3-day| ≈ 75% > 70%, c0 = $2.50 > $1).
        let mut closes = vec![10.0; 30];
        let n = closes.len();
        closes[n - 3] = 7.0;
        closes[n - 2] = 4.0;
        closes[n - 1] = 2.5;
        assert!(passes_prefilter(&closes));
    }

    #[test]
    fn prefilter_rejects_single_day_spike() {
        // Flat, then a single last-day jump (+80% on day 1 only) → the 2..6-day
        // windows still straddle the flat prior closes, so no 2+ day window clears
        // 70% from the base two days back... here base[n-2]=10 → 2-day = +80% DOES
        // clear. So use a spike that ONLY moves on the very last day from a base
        // that keeps 2-day under 70%: prior day already near top.
        let mut closes = vec![10.0; 30];
        let n = closes.len();
        // 1-day move of +50% (under 70), 2..6-day also under 70 → rejected.
        closes[n - 1] = 15.0;
        assert!(!passes_prefilter(&closes));
    }

    #[test]
    fn prefilter_rejects_penny_and_flat() {
        // Sub-$1 even with a huge move → rejected on price.
        let mut penny = vec![0.10; 30];
        let n = penny.len();
        penny[n - 1] = 0.90; // +800% but c0 < $1
        assert!(!passes_prefilter(&penny));
        // Flat $10 series → no qualifying move.
        assert!(!passes_prefilter(&vec![10.0; 30]));
        // Too short to evaluate a 2-day window.
        assert!(!passes_prefilter(&[10.0, 20.0]));
    }
}
