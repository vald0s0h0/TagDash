// Dislocation — metadata-only ScanStrategy entry.
//
// The real detection logic is NOT here: it is a stateful per-ticker engine living in
// `crate::dislocation`, running in its own tokio task across the whole extended
// trading day. It scores every eligible US common stock on six parallel tempos
// (10s/20s/40s/1m/2m/5m) with one number made of three equal thirds — RVOL against
// the ticker's own past, attention against the market right now, and a model's read
// of the catalyst — then spends a bounded model budget from the top of the ranking.
//
// FORMERLY "Micro Pullback v2". It shared a module, an options blob and a strategy
// id with a completely different detector; as of today it is called Dislocation and
// nothing else. Micro Pullback is the boolean-cascade engine it grew inside, and
// that engine is alive again under its own name.
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
pub const ID: &str = "dislocation";
const NAME: &str = "Dislocation";
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

pub struct Dislocation;

impl ScanStrategy for Dislocation {
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
            // No generic "Analyse IA" button: Dislocation scores every candidate
            // automatically (the RVOL/attention/news three-thirds pipeline in
            // `dislocation::quality`) — there is nothing left for a manual button to
            // trigger. This used to be a copy-paste `LlmSpec` left over from Micro
            // Pullback that showed a button wired to no working command (`run_alert_llm`
            // never had a `dislocation::ID` branch) — a dead click, not a real feature.
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
        // The dedicated engine (crate::dislocation) is the sole firer.
        None
    }
}
