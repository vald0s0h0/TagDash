// Perfect Pullback — metadata-only ScanStrategy entry.
//
// The real detection logic is NOT here: it is a stateful, multi-timeframe gate
// engine that lives in `crate::perfect_pullback` and runs in its own tokio task
// during the regular session. It watches every active premarket gapper (open vs
// previous close gapped ≥ ±10%) on its enabled timeframes (the 5m by default;
// 1m/2m/10m toggleable via ENABLE_* flags in the engine) for a strong directional
// move with high relative volume (gate 1), then fires on the first healthy pullback
// into it (gate 2). That can't fit the stateless per-ticker `should_alert(ctx)`
// contract, so the engine pushes its AlertSignals straight into the active-alert
// list (the same escape hatch the price-alarm watcher uses).
//
// This file exists so the strategy still has a proper identity in the registry: a
// Settings on/off toggle, a name + priority, and the identity card the UI uses to
// lay out the chart panes + indicators + info band when an alert lands.
// `should_alert` therefore always returns None — the engine is the only firer.

use std::time::Duration;

use crate::strategies::ScanStrategy;
use crate::types::{
    AlertSignal, IndicatorKind, InfoField, InfoSource, PaneIndicator, PaneSpec, Session,
    StrategyCard, StrategyContext, StrategyRiskConfig, UniverseKey,
};

// ─── Identity (kept in sync with the engine's constants) ──────────────────────
pub const ID: &str = "perfect_pullback";
const NAME: &str = "Perfect Pullback";
/// Activated by default.
const ENABLED: bool = true;
/// Priority 2 = "Normal" on the 1..=5 scale.
const PRIORITY: u8 = 2;
const MAX_RISK_DOLLARS: f64 = 100.0;
/// Regular session only. The engine additionally restricts firing to 09:30–16:00 ET.
const SESSIONS: &[Session] = &[Session::Open];

pub struct PerfectPullback;

impl ScanStrategy for PerfectPullback {
    fn id(&self) -> &'static str {
        ID
    }

    fn name(&self) -> &'static str {
        NAME
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
        // Unused: the engine owns its own per-variant cooldown.
        Duration::from_secs(crate::perfect_pullback::COOLDOWN_SECS)
    }

    fn risk_config(&self) -> StrategyRiskConfig {
        StrategyRiskConfig {
            max_risk_dollars: MAX_RISK_DOLLARS,
        }
    }

    fn card(&self) -> StrategyCard {
        StrategyCard {
            universe: UniverseKey::UsStocks,
            // Two panes. Left: daily context (SMA 200/20 + volume), read-only.
            // Right: the intraday execution pane (SMA 20 + session VWAP + volume),
            // interactive (SL/TP, orders). The intraday timeframe here is a
            // placeholder: every alert carries `display_timeframe` ("1m"/"2m"/
            // "5m"/"10m" — the triggering timeframe), which the UI uses to seed the
            // actual chart timeframe.
            panes: vec![
                PaneSpec {
                    timeframe:   "daily".into(),
                    symbol:      None,
                    indicators:  vec![
                        PaneIndicator { kind: IndicatorKind::Sma,    period: Some(200) },
                        PaneIndicator { kind: IndicatorKind::Sma,    period: Some(20) },
                        PaneIndicator { kind: IndicatorKind::Volume, period: None },
                    ],
                    interactive: false,
                },
                PaneSpec {
                    timeframe:   "1m".into(),
                    symbol:      None,
                    indicators:  vec![
                        PaneIndicator { kind: IndicatorKind::Sma,    period: Some(20) },
                        PaneIndicator { kind: IndicatorKind::Vwap,   period: None },
                        PaneIndicator { kind: IndicatorKind::Volume, period: None },
                    ],
                    interactive: true,
                },
            ],
            // Live-sourced info band (no enrichment/LLM for this strategy).
            info_fields: vec![
                InfoField { key: "change_day_pct".into(), label: "Gap".into(),  source: InfoSource::Alert },
                InfoField { key: "rvol".into(),           label: "RVOL".into(), source: InfoSource::Alert },
                InfoField { key: "vwap".into(),           label: "VWAP".into(), source: InfoSource::Alert },
                InfoField { key: "volume".into(),         label: "Vol".into(),  source: InfoSource::Alert },
            ],
            llm:         None,
            enrichments: vec![],
        }
    }

    fn should_alert(&self, _ctx: &StrategyContext) -> Option<AlertSignal> {
        // The dedicated engine is the sole firer for this strategy.
        None
    }

}
