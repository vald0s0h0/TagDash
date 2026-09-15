// Micro Pullback — metadata-only ScanStrategy entry.
//
// The real detection logic is NOT here: it is a stateful per-ticker engine living in
// `crate::micro_pullback`, running in its own tokio task across the whole extended
// trading day. It measures each ticker against ITS OWN rolling baseline on six
// parallel tempos (10s/20s/40s/1m/2m/5m), trips a tempo when the absolute minimums
// are met and n of the acceleration ratios clear, then hands the armed ticker to a
// tape watch that must see the prints before anything reaches the screen. One alert
// per ticker per session, then locked.
//
// NO MODEL IN THE PATH. That is the design, not an omission: both halves of the
// question are measurable from the tape alone. The catalyst read is a button the
// trader presses afterwards from the info bar. The engine that scores a catalyst is
// Dislocation, next door.
//
// That can't fit the stateless per-ticker `should_alert(ctx)` contract, so the
// engine pushes its AlertSignals straight into the active-alert list (the same
// escape hatch Perfect Pullback and the price-alarm watcher use).
//
// This file exists so the strategy still has a proper identity in the registry: a
// Settings on/off toggle, name + priority + risk, the session gate, and the identity
// card the UI uses to lay out the chart panes + indicators + info band (and the
// async enrichment + LLM reads) when an alert lands. `should_alert` therefore always
// returns None — the engine is the only firer.

use std::time::Duration;

use crate::strategies::ScanStrategy;
use crate::types::{
    AlertSignal, EnrichmentProvider, EnrichmentSpec, IndicatorKind, InfoField, InfoSource,
    PaneIndicator, PaneSpec, Session, StrategyCard, StrategyContext, StrategyRiskConfig,
    StreamingScope, UniverseKey,
};

// ─── Identity (kept in sync with the engine's Config) ─────────────────────────
pub const ID: &str = "micro_pullback";
const NAME: &str = "Micro Pullback";
/// Activated by default.
const ENABLED: bool = true;
/// Priority 5 = "Critical" on the 1..=5 scale.
const PRIORITY: u8 = 5;
const MAX_RISK_DOLLARS: f64 = 100.0;
/// The whole extended trading day. The engine restricts firing to 04:00–20:00 ET and
/// stamps each alert with the session it actually fired in, so it lands in that tab.
///
/// Premarket remains the primary use; the regular session is the second; afterhours
/// is rare but legitimate. Coverage is NOT uniform across them on the Alpaca feed —
/// the market-wide `trades` tier only runs when the tape is thin (premarket and
/// afterhours), so during the regular session detection is limited to the day's watch
/// list on that provider. See `alpaca::stream::broad_mode_at`.
const SESSIONS: &[Session] = &[
    Session::Premarket,
    Session::PreOpen,
    Session::Open,
    Session::Afterhours,
];

pub struct MicroPullback;

impl ScanStrategy for MicroPullback {
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
        // Unused: v2 owns an event lifecycle + release hysteresis and is not routed
        // through the scanner's stateless AlertEngine.
        Duration::from_secs(0)
    }

    fn risk_config(&self) -> StrategyRiskConfig {
        StrategyRiskConfig {
            max_risk_dollars: MAX_RISK_DOLLARS,
            ..Default::default()
        }
    }

    fn card(&self) -> StrategyCard {
        StrategyCard {
            // Whole-US-stock discovery; the runtime applies <=100M known float and
            // the user's current-price band (unknown float stays explicitly tagged).
            universe: UniverseKey::LowFloat,
            // The whole market, wherever the provider offers a wildcard. This engine
            // is a discovery scanner: it has to see names nobody is watching yet.
            //
            // Massive Starter's aggregate has no trade count; v2 uses continuity,
            // volume distribution and market-relative ranks, all supported by A.*.
            streaming_scope: StreamingScope::Universe,
            // Left column (stacked): daily context (SMA 200/20, volume, red dots on
            // split days) on top, a 5-minute context chart (volume) below — both
            // read-only. Right column: the 10s execution pane (volume) the engine
            // reasons on — interactive (SL/TP, orders), and the pane that carries the
            // strategy info overlay. The daily pane's split markers are supplied by
            // the enrichment payload; all bars load through the unified path.
            panes: vec![
                PaneSpec {
                    timeframe: "daily".into(),
                    symbol: None,
                    indicators: vec![
                        PaneIndicator {
                            kind: IndicatorKind::Sma,
                            period: Some(200),
                        },
                        PaneIndicator {
                            kind: IndicatorKind::Sma,
                            period: Some(20),
                        },
                        PaneIndicator {
                            kind: IndicatorKind::Volume,
                            period: None,
                        },
                    ],
                    interactive: false,
                    column: Some(0),
                },
                PaneSpec {
                    timeframe: "5m".into(),
                    symbol: None,
                    indicators: vec![PaneIndicator {
                        kind: IndicatorKind::Volume,
                        period: None,
                    }],
                    interactive: false,
                    column: Some(0),
                },
                PaneSpec {
                    timeframe: "10s".into(),
                    symbol: None,
                    indicators: vec![PaneIndicator {
                        kind: IndicatorKind::Volume,
                        period: None,
                    }],
                    interactive: true,
                    column: Some(1),
                },
            ],
            // Strategy-specific overlay fields (shown top-left on the left pane).
            // Values are filled by the enrichment pipeline; the UI shows a loading
            // state until each lands. The COMMON fields — name, BBZ, premarket /
            // current volume, news presence + IA analysis (context/verdict) — live
            // in the shared info bar and are NOT listed here.
            info_fields: vec![
                InfoField {
                    key: "float_shares".into(),
                    label: "Float".into(),
                    source: InfoSource::Enrichment,
                },
                InfoField {
                    key: "country".into(),
                    label: "Pays".into(),
                    source: InfoSource::Enrichment,
                },
                InfoField {
                    key: "industry".into(),
                    label: "Industrie".into(),
                    source: InfoSource::Enrichment,
                },
                InfoField {
                    key: "days_since_split".into(),
                    label: "Split".into(),
                    source: InfoSource::Enrichment,
                },
                InfoField {
                    key: "classification".into(),
                    label: "Profil".into(),
                    source: InfoSource::Enrichment,
                },
            ],
            // No generic "Analyse IA" button: Micro Pullback owns its own catalyst
            // read now (`micro_pullback::quality`) — automatic when `llm_auto` is on
            // in Settings, a dedicated button on the alert when it's off. Declaring
            // `llm` here would show the OLD free-text button alongside the new
            // structured score, which is exactly the double-button confusion this
            // replaced.
            llm: None,
            // Supplementary data: Massive (splits + news), sec-api (country + industry).
            enrichments: vec![
                EnrichmentSpec {
                    provider: EnrichmentProvider::Massive,
                    produces: vec![
                        "split_label".into(),
                        "days_since_split".into(),
                        "news_title".into(),
                    ],
                },
                EnrichmentSpec {
                    provider: EnrichmentProvider::SecApi,
                    produces: vec!["country".into(), "industry".into()],
                },
            ],
        }
    }

    fn should_alert(&self, _ctx: &StrategyContext) -> Option<AlertSignal> {
        // The dedicated engine (crate::micro_pullback) is the sole firer.
        None
    }
}
