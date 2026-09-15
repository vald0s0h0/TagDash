// Dislocation runtime options — the Settings → Stratégies → Dislocation panel.
//
// Deliberately NOT a generic per-strategy channel. These knobs are specific to this
// engine (which tempos to run, the score a candidate must reach, how many model
// calls it may spend), they are not all booleans, and no other strategy has an
// equivalent. A generic bool-only channel would have forced the threshold into a
// second mechanism anyway.
//
// Persisted as one JSON blob in `app_meta` under `dislocation_options`, held in
// `AppState.dislocation_options`, and re-read by the engine **inside its loop** so
// a change takes effect on the next tick without a restart.
//
// MIGRATION. This engine used to be "Micro Pullback v2" and wrote its blob under
// `micro_pullback_options`. `load` still reads that key when the new one is absent,
// so a user who had tuned the old panel keeps their settings exactly once, after
// which the blob lives under the new name.
//
// `#[serde(default)]` on the struct is load-bearing: a blob written by an older
// build is missing the newer fields, and they must fall back to their defaults
// rather than failing the whole parse and silently resetting every option.

use serde::{Deserialize, Serialize};

/// The tempos the UI exposes, in ascending order. The engine builds its active
/// `Vec<Tempo>` by filtering `Config::ALL_TEMPOS` through `enabled_tempos`.
pub const TEMPO_SECS: [i64; 6] = [10, 20, 40, 60, 120, 300];

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DislocationOptions {
    // ── Candidate universe ─────────────────────────────────────────────────────
    /// Current market-event price band. One source of truth for selection and A+;
    /// the quality layer deliberately adds no second, hidden band.
    pub price_min: f64,
    pub price_max: f64,
    /// Free-float ceiling, in shares. A ticker whose float is KNOWN and above this
    /// is not a candidate; an unknown float is still admitted (and marked
    /// `FLOAT_INCONNU` on the alert) because most genuine micro-caps are missing
    /// from the float providers on the day they matter.
    pub float_max: u64,
    /// Only watch tickers whose industry belongs to a "hot" category — biotech /
    /// pharma, space / aerospace / defence, crypto, quantum, cannabis, AI. These
    /// are the sectors that structurally pull a premarket crowd, so the same
    /// catalyst buys far more participation. Off by default: a hot industry is a
    /// tailwind, not a precondition, and switching this on WILL cost real runners
    /// in the sectors it excludes.
    ///
    /// Classification comes from `strategies::issuer_profile::hot_industry`, i.e.
    /// the SEC SIC title in `company_meta` first, then a keyword fallback for the
    /// categories the 1938-vintage SIC schema has no code for. A ticker whose
    /// industry we have never collected is NOT hot, so it is excluded too — that
    /// is the sharpest edge of this switch.
    pub hot_industry_only: bool,
    /// Drop tickers domiciled in the four jurisdictions over-represented in US
    /// small-cap pump & dump schemes — China (incl. Hong Kong), the Cayman Islands
    /// and the British Virgin Islands. Off by default: without it those names are
    /// still watched and simply carry a −25 penalty, which a genuinely strong
    /// catalyst can overcome. With it they are not watched at all.
    ///
    /// Like `hot_industry_only`, this reads `company_meta`. An issuer whose domicile
    /// was never collected is NOT flagged, so it survives the filter — the opposite
    /// edge from the hot-industry switch, and the safe direction here: we never
    /// exclude a ticker on a gap in our own data.
    pub exclude_risk_domicile: bool,

    // ── LLM ────────────────────────────────────────────────────────────────────
    /// Qualité A+ — and now ONLY that: the second, deeper LLM pass (fundamentals,
    /// dilution, filings), run after the news pass including when the news pass
    /// rejects, because the deep pass is the final decision rather than an
    /// enrichment reserved for already-approved names.
    ///
    /// OFF by default. It costs ~10-25 s and a `MODEL_PRO` call per candidate, and
    /// its dossier is only as good as what the cache holds — empty filings read as
    /// a clean issuer.
    ///
    /// This switch NO LONGER GATES THE PIPELINE. The news pass owns a third of the
    /// score, so it runs whenever there is a fresh headline and budget for it,
    /// A+ on or off.
    pub quality_aplus: bool,
    /// Keep watching for a headline after qualification and re-score the moment one
    /// lands — including for a candidate whose news third was a measured zero.
    pub news_watch: bool,
    /// false (default) = the LLM score gates the alert: nothing is pushed until the
    /// final configured pass returns above the threshold.
    /// true = the alert is pushed as soon as activity + relative participation
    /// confirm and the scores update it in place afterwards.
    pub alert_before_score: bool,
    /// Total score — the mean of the three thirds — a candidate must reach to be
    /// pushed. Clamped to 0..=100 on write.
    ///
    /// Read it against the arithmetic: with no headline in the window the news third
    /// is a measured zero, so a candidate caps at 66 however violent the tape.
    pub alert_score_threshold: f64,
    /// How many candidates may be sent to the model per rolling five minutes of
    /// market time.
    ///
    /// This is the budget that replaced the old magnitude gates. Candidates are
    /// ranked by `score::Scores::tape` — the two thirds that exist before any model
    /// speaks — and the budget is spent from the top; anything that does not fit stays eligible and is reconsidered on the
    /// next tick, so nothing is lost — it is deferred behind something better.
    /// Raising it costs model calls and lowers the average quality of what is
    /// scored; lowering it makes the strategy pickier, never blinder.
    pub llm_budget_per_5min: u8,

    // ── Push notification ──────────────────────────────────────────────────────
    /// Send every alert to the phone via ntfy at maximum priority. Only what clears
    /// the score threshold is ever pushed. Needs a topic in Settings → API Keys —
    /// without one the flag is inert rather than an error.
    pub ntfy_alerts: bool,

    // ── Active tempos ──────────────────────────────────────────────────────────
    pub tempo_10s: bool,
    pub tempo_20s: bool,
    pub tempo_40s: bool,
    pub tempo_1m: bool,
    pub tempo_2m: bool,
    pub tempo_5m: bool,
}

impl Default for DislocationOptions {
    fn default() -> Self {
        Self {
            price_min: 1.0,
            price_max: 50.0,
            float_max: 50_000_000,
            hot_industry_only: false,
            exclude_risk_domicile: false,
            quality_aplus: false,
            news_watch: true,
            alert_before_score: false,
            alert_score_threshold: 60.0,
            llm_budget_per_5min: 3,
            ntfy_alerts: false,
            tempo_10s: true,
            tempo_20s: true,
            tempo_40s: true,
            tempo_1m: true,
            tempo_2m: true,
            tempo_5m: true,
        }
    }
}

impl DislocationOptions {
    /// The tempo lengths (seconds) currently switched on, ascending.
    pub fn enabled_tempos(&self) -> Vec<i64> {
        let flags = [
            self.tempo_10s,
            self.tempo_20s,
            self.tempo_40s,
            self.tempo_1m,
            self.tempo_2m,
            self.tempo_5m,
        ];
        TEMPO_SECS
            .iter()
            .zip(flags)
            .filter_map(|(secs, on)| on.then_some(*secs))
            .collect()
    }

    /// Longest active tempo, used to size the baseline backfill. None when the user
    /// has switched every tempo off (the engine then detects nothing, which is a
    /// legitimate way to mute the strategy without disabling it).
    pub fn longest_tempo(&self) -> Option<i64> {
        self.enabled_tempos().last().copied()
    }

    /// Clamp anything a caller could get wrong. Applied on load and on write, so a
    /// hand-edited or corrupted blob can never feed the engine an absurd threshold.
    pub fn sanitized(mut self) -> Self {
        self.alert_score_threshold = if self.alert_score_threshold.is_finite() {
            self.alert_score_threshold.clamp(0.0, 100.0)
        } else {
            Self::default().alert_score_threshold
        };
        let defaults = Self::default();
        if !self.price_min.is_finite() || self.price_min <= 0.0 {
            self.price_min = defaults.price_min;
        }
        self.price_min = self.price_min.clamp(0.01, 9_999.99);
        if !self.price_max.is_finite() || self.price_max <= self.price_min {
            self.price_max = defaults.price_max.max(self.price_min + 0.01);
        }
        self.price_max = self.price_max.clamp(self.price_min + 0.01, 10_000.0);
        // A zero ceiling would silently mute the whole strategy, and no float above
        // a billion shares is a micro-cap under any reading of the word.
        if self.float_max == 0 {
            self.float_max = defaults.float_max;
        }
        self.float_max = self.float_max.clamp(100_000, 1_000_000_000);
        // Zero would mute A+ entirely while leaving the checkbox on, which reads as
        // a bug rather than a setting.
        self.llm_budget_per_5min = self.llm_budget_per_5min.clamp(1, 30);
        self
    }

    /// Read the persisted blob, falling back to defaults when absent or unparseable.
    pub fn load(conn: &rusqlite::Connection) -> Self {
        crate::local_db::cache_repository::get_app_meta(conn, APP_META_KEY)
            .or_else(|| crate::local_db::cache_repository::get_app_meta(conn, LEGACY_APP_META_KEY))
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

pub const APP_META_KEY: &str = "dislocation_options";
/// The key this blob was written under while the engine was called "Micro Pullback
/// v2". Read once, as a fallback, so a rename does not silently reset the panel.
const LEGACY_APP_META_KEY: &str = "micro_pullback_options";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_run_every_tempo_with_a_blocking_llm_at_60() {
        let o = DislocationOptions::default();
        assert_eq!(o.enabled_tempos(), vec![10, 20, 40, 60, 120, 300]);
        assert_eq!(o.longest_tempo(), Some(300));
        assert!(!o.alert_before_score, "the score gates the alert by default");
        assert!(
            !o.quality_aplus,
            "A+ is now exactly the deep pass, and it is opt-in"
        );
        // Sixty, not seventy: the score is the mean of three thirds, and a candidate
        // with no headline in the window caps at 66.
        assert_eq!(o.alert_score_threshold, 60.0);
        assert_eq!((o.price_min, o.price_max), (1.0, 50.0));
        assert_eq!(o.float_max, 50_000_000);
        assert_eq!(o.llm_budget_per_5min, 3);
        assert!(
            !o.hot_industry_only,
            "a hot industry is a tailwind, never a precondition, until asked for"
        );
        assert!(
            !o.exclude_risk_domicile,
            "a risk domicile is a base rate, not a ban, until asked for"
        );
    }

    #[test]
    fn the_model_budget_can_never_be_zero() {
        let mk = |b: u8| DislocationOptions {
            llm_budget_per_5min: b,
            ..Default::default()
        };
        assert_eq!(mk(0).sanitized().llm_budget_per_5min, 1);
        assert_eq!(mk(255).sanitized().llm_budget_per_5min, 30);
        assert_eq!(mk(5).sanitized().llm_budget_per_5min, 5);
    }

    #[test]
    fn the_float_ceiling_can_never_mute_the_strategy() {
        let mk = |f: u64| DislocationOptions {
            float_max: f,
            ..Default::default()
        };
        // Zero would exclude every ticker with a known float — a silent mute.
        assert_eq!(mk(0).sanitized().float_max, 50_000_000);
        assert_eq!(mk(1).sanitized().float_max, 100_000);
        assert_eq!(mk(u64::MAX).sanitized().float_max, 1_000_000_000);
        assert_eq!(mk(12_000_000).sanitized().float_max, 12_000_000);
    }

    #[test]
    fn unchecking_tempos_removes_exactly_those() {
        let o = DislocationOptions {
            tempo_10s: false,
            tempo_2m: false,
            ..Default::default()
        };
        assert_eq!(o.enabled_tempos(), vec![20, 40, 60, 300]);
        assert_eq!(o.longest_tempo(), Some(300));
    }

    #[test]
    fn every_tempo_off_is_legal_and_detects_nothing() {
        let o = DislocationOptions {
            tempo_10s: false,
            tempo_20s: false,
            tempo_40s: false,
            tempo_1m: false,
            tempo_2m: false,
            tempo_5m: false,
            ..Default::default()
        };
        assert!(o.enabled_tempos().is_empty());
        assert_eq!(o.longest_tempo(), None);
    }

    #[test]
    fn a_blob_from_an_older_build_keeps_its_settings_and_defaults_the_rest() {
        // The blob predates the 2m/5m tempos and the threshold field.
        let json = r#"{"quality_aplus":true,"tempo_10s":false}"#;
        let o: DislocationOptions = serde_json::from_str(json).unwrap();
        assert!(o.quality_aplus, "stored value survives");
        assert!(!o.tempo_10s, "stored value survives");
        assert!(
            o.tempo_5m,
            "unknown-at-the-time field falls back to its default"
        );
        assert_eq!(o.alert_score_threshold, 60.0);
        assert!(
            !o.ntfy_alerts,
            "the phone stays quiet until it is asked for"
        );
    }

    #[test]
    fn threshold_is_clamped_not_trusted() {
        let mk = |t: f64| DislocationOptions {
            alert_score_threshold: t,
            ..Default::default()
        };
        assert_eq!(mk(150.0).sanitized().alert_score_threshold, 100.0);
        assert_eq!(mk(-20.0).sanitized().alert_score_threshold, 0.0);
        assert_eq!(mk(f64::NAN).sanitized().alert_score_threshold, 60.0);
    }

    #[test]
    fn price_band_is_sanitized_and_ordered() {
        let bad = DislocationOptions {
            price_min: -1.0,
            price_max: f64::NAN,
            ..Default::default()
        }
        .sanitized();
        assert_eq!((bad.price_min, bad.price_max), (1.0, 50.0));

        let reversed = DislocationOptions {
            price_min: 60.0,
            price_max: 20.0,
            ..Default::default()
        }
        .sanitized();
        assert!(reversed.price_max > reversed.price_min);

        let huge = DislocationOptions {
            price_min: 50_000.0,
            price_max: 60_000.0,
            ..Default::default()
        }
        .sanitized();
        assert_eq!((huge.price_min, huge.price_max), (9_999.99, 10_000.0));
    }
}
