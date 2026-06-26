// HOD Drive — metadata-only ScanStrategy entry.
//
// The real detection logic is NOT here: it is a stateful, multi-timeframe gate engine
// that lives in `crate::hod_drive` and runs in its own tokio task during the regular
// session. It identifies tickers that drive cleanly off the open then pull back with a
// good risk/reward toward the HOD, via a four-gate pipeline (Universe → Risk Ratio →
// Clear Pattern → live 5s liquidity) on CLOSED bars of each timeframe. That can't fit
// the stateless per-ticker `should_alert(ctx)` contract, so the engine pushes its
// AlertSignals straight into the active-alert list (the same escape hatch Perfect
// Pullback / Micro Pullback use).
//
// This file exists so the strategy still has a proper identity in the registry: a
// Settings on/off toggle, a name + priority + risk, the regular-session gate, and the
// identity card the UI uses to lay out the chart panes + indicators + overlay.
// `should_alert` therefore always returns None — the engine is the only firer.

use std::time::Duration;

use crate::hod_drive::{COOLDOWN_SECS, MAX_RISK_DOLLARS, PRIORITY};
use crate::strategies::ScanStrategy;
use crate::types::{
    AlertSignal, IndicatorKind, InfoField, InfoSource, PaneIndicator, PaneSpec, Session,
    StrategyCard, StrategyContext, StrategyRiskConfig, UniverseKey,
};

// ─── Identity (kept in sync with the engine's constants) ──────────────────────
pub const ID: &str = "hod_drive";
const NAME: &str = "HOD Drive";
/// Activated by default.
const ENABLED: bool = true;
/// Regular session only. The engine additionally restricts firing to 09:30–16:00 ET
/// and to each timeframe's active window (min(tf×20, 120 min) after the open).
const SESSIONS: &[Session] = &[Session::Open];

pub struct HodDrive;

impl ScanStrategy for HodDrive {
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
        // Unused: the engine owns its own per-symbol cooldown.
        Duration::from_secs(COOLDOWN_SECS)
    }

    fn risk_config(&self) -> StrategyRiskConfig {
        StrategyRiskConfig {
            max_risk_dollars: MAX_RISK_DOLLARS,
        }
    }

    fn card(&self) -> StrategyCard {
        StrategyCard {
            universe: UniverseKey::UsStocks,
            // Two panes. Left: daily context (SMA 200/20 + volume), read-only. Right:
            // the timeframe that detected the signal (SMA 20 + session VWAP + volume),
            // interactive (SL/TP, orders) — it carries the HOD Drive overlay + the
            // HOD/LOD + green-series chart markers. The intraday timeframe here is a
            // placeholder seeded by the alert's `display_timeframe` (the firing tf).
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
                    column:      None,
                },
                PaneSpec {
                    timeframe:   "5m".into(),
                    symbol:      None,
                    indicators:  vec![
                        PaneIndicator { kind: IndicatorKind::Sma,    period: Some(20) },
                        PaneIndicator { kind: IndicatorKind::Vwap,   period: None },
                        PaneIndicator { kind: IndicatorKind::Volume, period: None },
                    ],
                    interactive: true,
                    column:      None,
                },
            ],
            // Live-sourced info band (no enrichment/LLM). The five strategy KPIs
            // (series share / pullback volume / power / efficiency) live in the
            // dedicated on-chart HOD Drive overlay, not here.
            info_fields: vec![
                InfoField { key: "change_day_pct".into(), label: "Gap".into(),    source: InfoSource::Alert },
                InfoField { key: "volume".into(),         label: "Vol".into(),    source: InfoSource::Alert },
                InfoField { key: "price".into(),          label: "Prix".into(),   source: InfoSource::Alert },
            ],
            llm:         None,
            enrichments: vec![],
        }
    }

    fn should_alert(&self, _ctx: &StrategyContext) -> Option<AlertSignal> {
        // The dedicated engine (crate::hod_drive) is the sole firer.
        None
    }
}
