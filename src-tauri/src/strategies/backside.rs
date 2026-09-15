// Backside — fades a small-cap parabolic spike. Fires on a low-institutional,
// small-cap name trading above a price floor that has run at least X% versus
// its previous day's close, premarket or into the open. Rebuilt from scratch
// (formerly "Backside Parabolic"); the old VWAP-cross/day-range gate is gone.
//
// Gate 1 (all evaluated in `should_alert`, unknown data always PASSES — this
// strategy would otherwise starve on the many small caps missing one data
// point or another):
//   - price >= `BacksideOptions.price_min` (settings, default $1) — the one
//     gate that does NOT pass on unknown, since `should_alert` already
//     requires a known price to alert on at all
//   - market cap < `BacksideOptions.market_cap_max` (settings, default 150M)
//   - institutional ownership < `INSTITUTIONAL_OWNERSHIP_MAX_PCT` (50%, fixed)
//   - change vs previous day's close >= `BacksideOptions.change_min_pct`
//     (settings, default 60%) — upward only, not `abs()`
//
// The change% gate reads `ctx.change_day_pct`, which `MarketState` computes
// from `TickerLiveState.previous_close` — itself bulk-seeded for the whole
// universe from `daily_cache` (the batch daily-bars step of the startup
// pipeline, see `cache_repository::latest_closes`) the moment the live feed
// starts. Deliberately NOT the true 04:00 ET open this strategy used to gate
// on: that value only exists once a symbol's first premarket tick has been
// seen by THIS process, so it silently reads as a too-late, too-high baseline
// whenever the app (re)started after 04:00 — understating the day's real move
// for exactly the small caps this strategy exists to catch. Previous close is
// final the moment the prior session ends, so it's correct regardless of when
// the app starts.
//
// Settings have no engine loop to live in (unlike dislocation/micro_pullback),
// so `BacksideOptions` lives in a module-static `OnceLock`, read directly by
// `should_alert`, rather than being threaded through `AppState`/the trait.

use std::sync::{OnceLock, RwLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::strategies::ScanStrategy;
use crate::types::{
    AlertSignal, IndicatorKind, InfoField, InfoSource, LlmSpec, PaneIndicator, PaneSpec, Session,
    StrategyCard, StrategyContext, StrategyRiskConfig, StreamingScope, UniverseKey,
};

pub const ID: &str = "backside";

const ENABLED: bool = true;
const PRIORITY: u8 = 3;
const MAX_RISK_DOLLARS: f64 = 100.0;
const COOLDOWN_SECS: u64 = 180;
const SESSIONS: &[Session] = &[Session::Premarket, Session::Open];

/// Institutional ownership ceiling. Fixed, not user-configurable — the settings
/// panel only exposes the 4am-change and market-cap thresholds.
const INSTITUTIONAL_OWNERSHIP_MAX_PCT: f64 = 50.0;

// ─── Settings ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BacksideOptions {
    /// Minimum % change vs the previous day's close to alert. Default 60.
    pub change_min_pct: f64,
    /// Market cap ceiling in dollars — a candidate must be BELOW this. Default
    /// 150,000,000. Named `_max` because that's the actual Gate 1 rule ("< 150M"),
    /// even though it's sometimes described as a "minimum" in casual speech.
    pub market_cap_max: f64,
    /// Minimum price in dollars — a candidate must be AT OR ABOVE this. Default
    /// 1.0, to keep sub-dollar names (wide spreads, frequent halts) out.
    pub price_min: f64,
}

impl Default for BacksideOptions {
    fn default() -> Self {
        Self {
            change_min_pct: 60.0,
            market_cap_max: 150_000_000.0,
            price_min: 1.0,
        }
    }
}

impl BacksideOptions {
    /// Clamp anything a caller could get wrong, so a hand-edited or corrupted
    /// blob can never mute the strategy or feed it an absurd threshold.
    pub fn sanitized(mut self) -> Self {
        let defaults = Self::default();
        if !self.change_min_pct.is_finite() || self.change_min_pct < 0.0 {
            self.change_min_pct = defaults.change_min_pct;
        }
        self.change_min_pct = self.change_min_pct.clamp(0.0, 1000.0);
        if !self.market_cap_max.is_finite() || self.market_cap_max <= 0.0 {
            self.market_cap_max = defaults.market_cap_max;
        }
        self.market_cap_max = self.market_cap_max.clamp(1_000_000.0, 100_000_000_000.0);
        if !self.price_min.is_finite() || self.price_min < 0.0 {
            self.price_min = defaults.price_min;
        }
        self.price_min = self.price_min.clamp(0.0, 10_000.0);
        self
    }

    /// Read the persisted blob, falling back to defaults when absent or unparseable.
    pub fn load(conn: &rusqlite::Connection) -> Self {
        crate::local_db::cache_repository::get_app_meta(conn, APP_META_KEY)
            .and_then(|json| serde_json::from_str::<Self>(&json).ok())
            .unwrap_or_default()
            .sanitized()
    }

    /// Persist the blob. Returns the rusqlite error as a string for the command layer.
    pub fn save(&self, conn: &rusqlite::Connection) -> Result<(), String> {
        let json = serde_json::to_string(self).map_err(|e| e.to_string())?;
        crate::local_db::cache_repository::set_app_meta(conn, APP_META_KEY, &json)
            .map_err(|e| e.to_string())
    }
}

const APP_META_KEY: &str = "backside_options";

/// `should_alert` receives no `AppState`, and `registry::all_strategies()`
/// builds every strategy as a zero-sized stateless struct — so, unlike an
/// engine-based strategy's options, these can't be threaded through anything.
/// A module-static is the pragmatic fit: seeded once at startup by
/// `init_options`, read live every tick by `should_alert`, updated in place by
/// `set_options` (called from the `set_backside_options` command right after
/// it persists).
static OPTIONS: OnceLock<RwLock<BacksideOptions>> = OnceLock::new();

/// Seed the static from the persisted blob. Call exactly once, at app startup.
pub fn init_options(conn: &rusqlite::Connection) {
    let _ = OPTIONS.set(RwLock::new(BacksideOptions::load(conn)));
}

pub fn current_options() -> BacksideOptions {
    *OPTIONS
        .get_or_init(|| RwLock::new(BacksideOptions::default()))
        .read()
        .unwrap()
}

pub fn set_options(new: BacksideOptions) {
    *OPTIONS
        .get_or_init(|| RwLock::new(BacksideOptions::default()))
        .write()
        .unwrap() = new;
}

// ─── Strategy ───────────────────────────────────────────────────────────────

pub struct Backside;

impl ScanStrategy for Backside {
    fn id(&self) -> &'static str {
        ID
    }

    fn name(&self) -> &'static str {
        "Backside"
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
            ..Default::default()
        }
    }

    fn card(&self) -> StrategyCard {
        StrategyCard {
            universe: UniverseKey::UsStocks,
            streaming_scope: StreamingScope::None,
            panes: vec![
                // Left — daily context, showing the size of the parabolic move.
                PaneSpec {
                    timeframe: "daily".into(),
                    symbol: None,
                    indicators: vec![PaneIndicator {
                        kind: IndicatorKind::Vwap,
                        period: None,
                    }],
                    interactive: false,
                    column: None,
                },
                // Right — 1-minute execution view.
                PaneSpec {
                    timeframe: "1m".into(),
                    symbol: None,
                    indicators: vec![
                        PaneIndicator {
                            kind: IndicatorKind::Vwap,
                            period: None,
                        },
                        PaneIndicator {
                            kind: IndicatorKind::Volume,
                            period: None,
                        },
                    ],
                    interactive: true,
                    column: None,
                },
            ],
            // Deliberately minimal — "afficher que l'essentiel": change% and
            // rotation (volume vs float) only. Market cap / ownership / change-4am
            // are gate inputs, not display fields.
            info_fields: vec![
                InfoField {
                    key: "change_day_pct".into(),
                    label: "Chg".into(),
                    source: InfoSource::Alert,
                },
                InfoField {
                    key: "rotation".into(),
                    label: "Rot".into(),
                    source: InfoSource::Alert,
                },
            ],
            // Reuses Panic's exact LLM call (enrichment::run_panic_llm) — same
            // prompt, same scoring, not a new one. Declaring `llm` here is what
            // activates the enrichment band and the `run_llm` hotkey for this
            // strategy; the actual call is manual (button-triggered), same as Panic.
            llm: Some(LlmSpec {
                prompt_template: "Contexte + verdict de mean-reversion pour {symbol} \
                                  (réutilise l'analyse Panic, déclenché manuellement)"
                    .into(),
                produces: vec!["llm_context".into(), "llm_reversion".into()],
            }),
            enrichments: vec![],
        }
    }

    fn should_alert(&self, ctx: &StrategyContext) -> Option<AlertSignal> {
        let opts = current_options();
        let price = ctx.price?;

        // Gate: price >= floor. Known by construction (`price` above), so this is
        // the one Gate 1 check that actually excludes rather than passing on
        // missing data.
        if price < opts.price_min {
            return None;
        }

        // Gate: market cap < ceiling. Unknown → passes.
        if let Some(mcap) = ctx.market_cap {
            if mcap as f64 >= opts.market_cap_max {
                return None;
            }
        }

        // Gate: institutional ownership < 50%. Unknown → passes.
        if let Some(own) = ctx.institutional_ownership_pct {
            if own >= INSTITUTIONAL_OWNERSHIP_MAX_PCT {
                return None;
            }
        }

        // Gate: change vs previous day's close >= threshold, upward only.
        // Unknown → passes (per product decision: treat like the other two,
        // never exclude on missing data — even though in practice this is the
        // strategy's core signal, so a ticker alerting on cap+ownership alone
        // here means its previous close was simply never cached).
        if let Some(chg) = ctx.change_day_pct {
            if chg < opts.change_min_pct {
                return None;
            }
        }

        let now = crate::time::now();
        Some(AlertSignal {
            alert_id: format!("{}-{}-{}", now.timestamp_millis(), ctx.symbol, self.id()),
            timestamp: now,
            symbol: ctx.symbol.clone(),
            strategy_id: self.id().to_string(),
            strategy_name: self.name().to_string(),
            priority: PRIORITY,
            session: Session::Open, // overwritten by the scanner right after
            price: Some(price),
            bid: ctx.bid,
            ask: ctx.ask,
            spread: ctx.spread,
            volume: Some(ctx.volume_day),
            rvol: ctx.rvol,
            change_day_pct: ctx.change_day_pct,
            float_shares: ctx.float_shares,
            news_today: false,
            halted: Some(false),
            latency_ui_ms: None,
            reason: format!(
                "MCap {} — Chg veille {}",
                ctx.market_cap
                    .map(|m| format!("${:.0}M", m as f64 / 1e6))
                    .unwrap_or_else(|| "inconnue".into()),
                ctx.change_day_pct
                    .map(|c| format!("+{:.0}%", c))
                    .unwrap_or_else(|| "inconnu".into()),
            ),
            display_timeframe: None,
            side: None,
            tempo_depth: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Exercises should_alert against the DEFAULT options (60% / $150M / $1) rather
    // than `set_options`: `OPTIONS` is a process-wide static, so mutating it
    // from a test would race with every other test in this binary.
    fn base_ctx() -> StrategyContext {
        StrategyContext {
            symbol: "TEST".into(),
            price: Some(5.0),
            bid: None,
            ask: None,
            spread: None,
            volume_day: 100_000,
            vwap: None,
            high_day: None,
            low_day: None,
            previous_close: None,
            // Passes every Gate 1 criterion by default.
            change_day_pct: Some(70.0),
            rvol: None,
            float_shares: None,
            market_cap: Some(100_000_000),
            institutional_ownership_pct: Some(20.0),
        }
    }

    #[test]
    fn alerts_when_every_gate_1_criterion_is_met() {
        assert!(Backside.should_alert(&base_ctx()).is_some());
    }

    #[test]
    fn excludes_a_market_cap_at_or_above_the_ceiling() {
        let ctx = StrategyContext { market_cap: Some(200_000_000), ..base_ctx() };
        assert!(Backside.should_alert(&ctx).is_none());
    }

    #[test]
    fn excludes_institutional_ownership_at_or_above_50_percent() {
        let ctx = StrategyContext { institutional_ownership_pct: Some(60.0), ..base_ctx() };
        assert!(Backside.should_alert(&ctx).is_none());
    }

    #[test]
    fn excludes_a_change_since_previous_close_below_the_threshold() {
        let ctx = StrategyContext { change_day_pct: Some(10.0), ..base_ctx() };
        assert!(Backside.should_alert(&ctx).is_none());
    }

    #[test]
    fn excludes_a_price_below_the_floor() {
        let ctx = StrategyContext { price: Some(0.5), ..base_ctx() };
        assert!(Backside.should_alert(&ctx).is_none());
    }

    #[test]
    fn admits_a_price_exactly_at_the_floor() {
        let ctx = StrategyContext { price: Some(1.0), ..base_ctx() };
        assert!(Backside.should_alert(&ctx).is_some());
    }

    #[test]
    fn unknown_market_cap_passes_the_gate() {
        let ctx = StrategyContext { market_cap: None, ..base_ctx() };
        assert!(Backside.should_alert(&ctx).is_some());
    }

    #[test]
    fn unknown_institutional_ownership_passes_the_gate() {
        let ctx = StrategyContext { institutional_ownership_pct: None, ..base_ctx() };
        assert!(Backside.should_alert(&ctx).is_some());
    }

    #[test]
    fn unknown_change_since_previous_close_passes_the_gate() {
        let ctx = StrategyContext { change_day_pct: None, ..base_ctx() };
        assert!(Backside.should_alert(&ctx).is_some());
    }

    #[test]
    fn options_sanitized_falls_back_to_defaults_on_bad_input() {
        let bad = BacksideOptions {
            change_min_pct: f64::NAN,
            market_cap_max: -5.0,
            price_min: f64::NAN,
        }
        .sanitized();
        assert_eq!(bad.change_min_pct, 60.0);
        assert_eq!(bad.market_cap_max, 150_000_000.0);
        assert_eq!(bad.price_min, 1.0);
    }
}
