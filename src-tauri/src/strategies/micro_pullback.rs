// Micro Pullback — metadata-only ScanStrategy entry.
//
// The real detection logic is NOT here: it is a stateful per-ticker state machine
// (COLD → ARMED → [ignition] → CONFIRMING → ALERTED/LOCKED) that lives in
// `crate::micro_pullback` and runs in its own tokio task during the premarket
// window. It watches every dormant low-float small cap for the *first* state change
// of the session — measured silence → a 10s ignition spike → a 30s confirmation —
// and fires exactly one alert per ticker per session, then locks it. That can't fit
// the stateless per-ticker `should_alert(ctx)` contract, so the engine pushes its
// AlertSignals straight into the active-alert list (the same escape hatch Perfect
// Pullback and the price-alarm watcher use).
//
// This file exists so the strategy still has a proper identity in the registry: a
// Settings on/off toggle, name + priority + risk, the premarket session gate, and
// the identity card the UI uses to lay out the chart panes + indicators + info band
// (and the async enrichment + LLM reads) when an alert lands. `should_alert`
// therefore always returns None — the engine is the only firer.

use std::time::Duration;

use crate::strategies::ScanStrategy;
use crate::types::{
    AlertSignal, EnrichmentProvider, EnrichmentSpec, IndicatorKind, InfoField, InfoSource, LlmSpec,
    PaneIndicator, PaneSpec, Session, StrategyCard, StrategyContext, StrategyRiskConfig,
    UniverseKey,
};

// ─── Identity (kept in sync with the engine's Config) ─────────────────────────
pub const ID: &str = "micro_pullback";
const NAME: &str = "Micro Pullback";
/// Activated by default.
const ENABLED: bool = true;
/// Priority 5 = "Critical" on the 1..=5 scale.
const PRIORITY: u8 = 5;
const MAX_RISK_DOLLARS: f64 = 100.0;
/// Premarket only — the engine additionally restricts firing to 04:00–09:30 ET, the
/// window during which the live feed streams the whole market's trades (so the 10s
/// bars the engine reasons on actually flow).
const SESSIONS: &[Session] = &[Session::Premarket];

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
        // Unused: the engine fires at most once per ticker per session (LOCK), with
        // no cooldown — it isn't routed through the scanner's AlertEngine.
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
            // Premarket low-float observation, tick to tick.
            universe: UniverseKey::LowFloat,
            // Left column (stacked): daily context (SMA 200/20, volume, red dots on
            // split days) on top, a 5-minute context chart (volume) below — both
            // read-only. Right column: the 10s execution pane (volume) the engine
            // reasons on — interactive (SL/TP, orders), and the pane that carries the
            // strategy info overlay. The daily pane's split markers are supplied by
            // the enrichment payload; all bars load through the unified path.
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
                    column:      Some(0),
                },
                PaneSpec {
                    timeframe:   "5m".into(),
                    symbol:      None,
                    indicators:  vec![PaneIndicator { kind: IndicatorKind::Volume, period: None }],
                    interactive: false,
                    column:      Some(0),
                },
                PaneSpec {
                    timeframe:   "10s".into(),
                    symbol:      None,
                    indicators:  vec![PaneIndicator { kind: IndicatorKind::Volume, period: None }],
                    interactive: true,
                    column:      Some(1),
                },
            ],
            // Strategy-specific overlay fields (shown top-left on the left pane).
            // Values are filled by the enrichment pipeline; the UI shows a loading
            // state until each lands. The COMMON fields — name, BBZ, premarket /
            // current volume, news presence + IA analysis (context/verdict) — live
            // in the shared info bar and are NOT listed here.
            info_fields: vec![
                InfoField { key: "float_shares".into(),    label: "Float".into(),    source: InfoSource::Enrichment },
                InfoField { key: "country".into(),         label: "Pays".into(),     source: InfoSource::Enrichment },
                InfoField { key: "industry".into(),        label: "Industrie".into(),source: InfoSource::Enrichment },
                InfoField { key: "days_since_split".into(),label: "Split".into(),    source: InfoSource::Enrichment },
                InfoField { key: "classification".into(),  label: "Profil".into(),   source: InfoSource::Enrichment },
            ],
            // Declared so the info-bar "Analyse IA" button shows for this strategy.
            // Two Deepseek calls (prompts FR), user-triggered only — see
            // `enrichment::run_micro_pullback_llm` (the template below is documentary).
            llm: Some(LlmSpec {
                prompt_template: "1) Si une news est détectée pour {symbol}, lis-la et dis très \
                    brièvement en français si elle est du bluff ou solide (mots-clés, arguments). \
                    2) Quels sont les risques récents autour de {symbol}, surtout la dilution \
                    (contexte: scalping intraday, pas d'analyse long terme) ? Réponds en quelques \
                    mots en français."
                    .into(),
                produces: vec!["llm_news".into(), "llm_dilution".into()],
            }),
            // Supplementary data: Massive (splits + news), sec-api (country + industry).
            enrichments: vec![
                EnrichmentSpec {
                    provider: EnrichmentProvider::Massive,
                    produces: vec!["split_label".into(), "days_since_split".into(), "news_title".into()],
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
