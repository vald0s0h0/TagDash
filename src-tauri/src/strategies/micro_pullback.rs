// Micro Pullback — premarket low-float NEWS↔acceleration correlation engine.
//
// It no longer fires on a price spike. Instead it watches for a *significant
// rise in trade frequency* (the rate of prints accelerating = strong trader
// interest, with no directional bias yet) AND a live news headline *for the
// same ticker* that lands within ±NEWS_WINDOW_SECS of that price move (Alpaca
// news WebSocket, premarket).
//
// The two can arrive in either order:
//   • news before the move — the headline is already on file when prints
//     accelerate; we match it if it arrived within the window.
//   • news after the move  — the scanner timestamps each acceleration as a
//     "price event"; when a headline lands shortly after, it still correlates
//     against that recent event (ctx.last_price_event), within the window.
// Either way the headline must be no more than NEWS_WINDOW_SECS (10 min) away
// from the price event, on either side — a stale same-day headline no longer
// counts.
//
// Acceleration = (prints over the recent window) rate vs (prints over the
// baseline window) rate. News is matched by ticker symbol (the store is keyed
// per-symbol, so every candidate headline genuinely references the ticker);
// there can be several headlines for one ticker — we take the one closest in
// time to the price event. All heavy context (daily classification, splits,
// Massive news, LLM risk read) is still filled asynchronously by the
// `enrichment` pipeline once the alert is shown in a zone — see card() below.

use std::time::Duration;

use chrono::Utc;

use crate::strategies::ScanStrategy;
use crate::types::{
    AlertSignal, EnrichmentProvider, EnrichmentSpec, IndicatorKind, InfoField, InfoSource, LlmSpec,
    PaneIndicator, PaneSpec, Session, StrategyCard, StrategyContext, StrategyRiskConfig,
    UniverseKey,
};

// ─── Tunable parameters (safe to tweak — recompile to apply) ──────────────────
const ENABLED: bool = true;
/// Priority 4 = "Important" on the 1..=5 scale.
const PRIORITY: u8 = 4;
const MAX_RISK_DOLLARS: f64 = 100.0;
const COOLDOWN_SECS: u64 = 120;

/// Tradeable price band (USD).
const PRICE_MIN: f64 = 2.0;
const PRICE_MAX: f64 = 20.0;
/// Low-float gate (shares). The streaming universe is no longer pre-filtered, so
/// this strategy selects its low-float candidates here. Tweak as needed.
const MAX_FLOAT: u64 = 30_000_000;

// ── Trade-acceleration tunables (replace the old spike parameters) ────────────
// The scanner reads ACCEL_RECENT_SECS / ACCEL_BASELINE_SECS to count prints over
// each window (so every accel knob lives here), then this strategy turns those
// counts into a rate ratio and applies the thresholds below.
//
/// Recent window (s): trades over the last N seconds → the "now" rate.
pub const ACCEL_RECENT_SECS: i64 = 10;
/// Baseline window (s): trades over the last N seconds → the reference rate.
/// Must be larger than the recent window.
pub const ACCEL_BASELINE_SECS: i64 = 60;
/// The recent rate must exceed ACCEL_RATIO_MIN × the baseline rate to count as
/// a significant acceleration. Lower → more alerts, raise → more selective.
const ACCEL_RATIO_MIN: f64 = 3.0;
/// Floor on the recent-window print count, so we don't fire on a couple of
/// prints off a near-zero baseline. Raise to require more conviction.
const MIN_RECENT_TRADES: u64 = 30;

/// News↔price correlation window (seconds). A headline counts only when it
/// arrived within this many seconds of the price event, *before or after* it.
/// 600 s = 10 min on each side.
pub const NEWS_WINDOW_SECS: i64 = 600;

/// Premarket only → the WS feed streams the low-float universe for this strategy
/// and the Alpaca news WebSocket is active only in the same window.
const SESSIONS: &[Session] = &[Session::Premarket];

/// Trade-acceleration ratio for `ctx`: recent print-rate ÷ baseline print-rate.
/// None until both windows have data and the baseline rate is positive.
fn accel_ratio(ctx: &StrategyContext) -> Option<f64> {
    let recent   = ctx.trades_recent? as f64;
    let baseline = ctx.trades_baseline? as f64;
    let recent_rate   = recent / ACCEL_RECENT_SECS as f64;
    let baseline_rate = baseline / ACCEL_BASELINE_SECS as f64;
    if baseline_rate <= 0.0 {
        return None;
    }
    Some(recent_rate / baseline_rate)
}

/// Whether the trade-print rate is accelerating enough to count as a "price
/// event" (strong, direction-agnostic trader interest). Single source of truth
/// for the threshold, shared by `should_alert` and the scanner — the scanner
/// timestamps each event so a headline landing afterwards can still correlate.
pub fn is_accelerating(recent: Option<u64>, baseline: Option<u64>) -> bool {
    let recent = match recent {
        Some(r) => r,
        None => return false,
    };
    if recent < MIN_RECENT_TRADES {
        return false;
    }
    let baseline = match baseline {
        Some(b) => b,
        None => return false,
    };
    let recent_rate   = recent as f64 / ACCEL_RECENT_SECS as f64;
    let baseline_rate = baseline as f64 / ACCEL_BASELINE_SECS as f64;
    baseline_rate > 0.0 && recent_rate / baseline_rate >= ACCEL_RATIO_MIN
}

pub struct MicroPullback;

impl ScanStrategy for MicroPullback {
    fn id(&self) -> &'static str {
        "micro_pullback"
    }

    fn name(&self) -> &'static str {
        "Micro Pullback"
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
            // Premarket low-float observation, tick to tick.
            universe: UniverseKey::LowFloat,
            // Left: daily context (SMA 200/20, volume, red dots on split days) —
            // read-only. Right: 5s execution pane (volume) — interactive (SL/TP,
            // orders). The daily pane's data + split markers are supplied by the
            // enrichment payload, not the live ring buffer.
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
                    timeframe:   "5s".into(),
                    symbol:      None,
                    indicators:  vec![PaneIndicator { kind: IndicatorKind::Volume, period: None }],
                    interactive: true,
                },
            ],
            // Strategy-specific band fields. Values are filled by the enrichment
            // pipeline; the UI shows a loading state until each lands. Common
            // fields (name + priority badge) are added by the UI, not listed here.
            info_fields: vec![
                InfoField { key: "float_shares".into(),    label: "Float".into(),    source: InfoSource::Enrichment },
                InfoField { key: "country".into(),         label: "Pays".into(),     source: InfoSource::Enrichment },
                InfoField { key: "industry".into(),        label: "Industrie".into(),source: InfoSource::Enrichment },
                InfoField { key: "days_since_split".into(),label: "Split".into(),    source: InfoSource::Enrichment },
                InfoField { key: "classification".into(),  label: "Profil".into(),   source: InfoSource::Enrichment },
                InfoField { key: "news_title".into(),      label: "News".into(),     source: InfoSource::Enrichment },
                InfoField { key: "llm_dilution".into(),    label: "Dilution".into(), source: InfoSource::Llm },
                InfoField { key: "llm_news".into(),        label: "News?".into(),    source: InfoSource::Llm },
            ],
            // Two Deepseek calls (prompts FR). {placeholders} filled at call time.
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

    fn should_alert(&self, ctx: &StrategyContext) -> Option<AlertSignal> {
        let price = ctx.price?;

        // Tradeable price band.
        if !(PRICE_MIN..=PRICE_MAX).contains(&price) {
            return None;
        }

        // Low-float gate (the stream is no longer pre-filtered by universe).
        // Require a known float below the cap; unknown float → skip.
        if ctx.float_shares? >= MAX_FLOAT {
            return None;
        }

        let now = Utc::now();

        // Correlation requirement #1 — a price event: a significant trade
        // acceleration. It is either happening *now*, or happened on a recent
        // pass and is still inside the correlation window (so a headline landing
        // after the move can still match). No event → no alert.
        let accel_now = is_accelerating(ctx.trades_recent, ctx.trades_baseline);
        let event_time = if accel_now {
            now
        } else {
            match ctx.last_price_event {
                Some(t) if (now - t).num_seconds() <= NEWS_WINDOW_SECS => t,
                _ => return None,
            }
        };

        // Correlation requirement #2 — a live news headline for THIS ticker
        // (the list is keyed per-symbol) whose arrival is within ±NEWS_WINDOW_SECS
        // of the price event, in either order. Pick the one closest in time.
        let news = ctx
            .news
            .iter()
            .filter(|n| (n.at - event_time).num_seconds().abs() <= NEWS_WINDOW_SECS)
            .min_by_key(|n| (n.at - event_time).num_seconds().abs())?;

        // Human-readable timing of the headline relative to the price event.
        let offset = (news.at - event_time).num_seconds();
        let timing = if offset.abs() < 30 {
            "simultanée".to_string()
        } else if offset < 0 {
            format!("{}min avant", (-offset + 59) / 60)
        } else {
            format!("{}min après", (offset + 59) / 60)
        };
        // Reason adapts to the order of arrival: a live acceleration with its
        // ratio, or a headline landing on a recent prior move.
        let headline = truncate(&news.headline, 60);
        let reason = if accel_now {
            format!(
                "Accélération trades ×{:.1} + news liée ({}) : « {} » — ${:.2}",
                accel_ratio(ctx).unwrap_or(0.0),
                timing,
                headline,
                price,
            )
        } else {
            format!(
                "News liée sur accélération récente ({}) : « {} » — ${:.2}",
                timing, headline, price,
            )
        };

        Some(AlertSignal {
            alert_id:      format!("{}-{}-{}", now.timestamp_millis(), ctx.symbol, self.id()),
            timestamp:     now,
            symbol:        ctx.symbol.clone(),
            strategy_id:   self.id().to_string(),
            strategy_name: self.name().to_string(),
            priority:      PRIORITY,
            session:       Session::Premarket,
            price:         ctx.price,
            bid:           ctx.bid,
            ask:           ctx.ask,
            spread:        ctx.spread,
            volume:        Some(ctx.volume_day),
            rvol:          ctx.rvol,
            change_day_pct: ctx.change_day_pct,
            float_shares:  ctx.float_shares,
            // We fire only when a correlated headline is present.
            news_today:    true,
            halted:        Some(false),
            latency_ui_ms: None,
            reason,
            display_timeframe: None,
            side:          None,
        })
    }

}

/// Truncate a headline to `max` chars (char-boundary safe) for the alert reason,
/// appending an ellipsis when cut.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max).collect();
        format!("{cut}…")
    }
}
