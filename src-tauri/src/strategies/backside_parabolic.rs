// Backside Parabolic — fires when a stock has made a large intraday range
// (high/low spread > 8 %) and price has crossed back below VWAP, signalling
// the parabolic move is exhausted. Short-biased setup.

use std::time::Duration;


use crate::strategies::ScanStrategy;
use crate::types::{
    AlertSignal, IndicatorKind, InfoField, InfoSource, PaneIndicator, PaneSpec, Session,
    StrategyCard, StrategyContext, StrategyRiskConfig, UniverseKey,
};

const ENABLED: bool = true;
const PRIORITY: u8 = 4;
const MAX_RISK_DOLLARS: f64 = 150.0;
const COOLDOWN_SECS: u64 = 180;

const SESSIONS: &[Session] = &[Session::Open];

pub struct BacksideParabolic;

impl ScanStrategy for BacksideParabolic {
    fn id(&self) -> &'static str {
        "backside_parabolic"
    }

    fn name(&self) -> &'static str {
        "Backside Parabolic"
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
        Duration::from_secs(COOLDOWN_SECS)
    }

    fn risk_config(&self) -> StrategyRiskConfig {
        StrategyRiskConfig {
            max_risk_dollars: MAX_RISK_DOLLARS,
        }
    }

    fn card(&self) -> StrategyCard {
        StrategyCard {
            // Short-biased exhaustion setup on liquid movers → full US universe.
            universe: UniverseKey::UsStocks,
            panes: vec![
                PaneSpec {
                    timeframe:   "1m".into(),
                    symbol:      None,
                    indicators:  vec![
                        PaneIndicator { kind: IndicatorKind::Vwap,   period: None },
                        PaneIndicator { kind: IndicatorKind::Volume, period: None },
                    ],
                    interactive: true,
                    column:      None,
                },
                PaneSpec {
                    timeframe:   "2m".into(),
                    symbol:      None,
                    indicators:  vec![PaneIndicator { kind: IndicatorKind::Vwap, period: None }],
                    interactive: false,
                    column:      None,
                },
            ],
            info_fields: vec![
                InfoField { key: "change_day_pct".into(), label: "Chg".into(),  source: InfoSource::Alert },
                InfoField { key: "rvol".into(),           label: "RVOL".into(), source: InfoSource::Enrichment },
            ],
            llm:         None,
            enrichments: vec![],
        }
    }

    fn should_alert(&self, ctx: &StrategyContext) -> Option<AlertSignal> {
        let price = ctx.price?;
        let high  = ctx.high_day?;
        let low   = ctx.low_day?;
        let vwap  = ctx.vwap?;
        if low <= 0.0 || vwap <= 0.0 {
            return None;
        }

        // Large intraday range: price moved at least 8 % from low to high
        let day_range_pct = (high - low) / low;
        if day_range_pct < 0.08 {
            return None;
        }

        // Price is now below VWAP — backside of the move
        if price >= vwap {
            return None;
        }

        // Require some volume
        if ctx.volume_day < 30_000 {
            return None;
        }

        let now = crate::time::now();
        Some(AlertSignal {
            alert_id:      format!("{}-{}-{}", now.timestamp_millis(), ctx.symbol, self.id()),
            timestamp:     now,
            symbol:        ctx.symbol.clone(),
            strategy_id:   self.id().to_string(),
            strategy_name: self.name().to_string(),
            priority:      PRIORITY,
            session:       Session::Open,
            price:         ctx.price,
            bid:           ctx.bid,
            ask:           ctx.ask,
            spread:        ctx.spread,
            volume:        Some(ctx.volume_day),
            rvol:          ctx.rvol,
            change_day_pct: ctx.change_day_pct,
            float_shares:  ctx.float_shares,
            news_today:    false,
            halted:        Some(false),
            latency_ui_ms: None,
            reason:        format!(
                "Backside VWAP ${:.2} — range {:.1}% HOD ${:.2}",
                vwap,
                day_range_pct * 100.0,
                high,
            ),
            display_timeframe: None,
            side:          None,
        })
    }

}
