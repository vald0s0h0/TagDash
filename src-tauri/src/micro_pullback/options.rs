// Micro Pullback runtime options — the Settings → Stratégies → Micro Pullback panel.
//
// Persisted as one JSON blob in `app_meta` under `micro_pullback_v1_options`, held
// in `AppState.micro_pullback_options`, and re-read by the engine **inside its loop**
// so a change takes effect on the next tick without a restart.
//
// A DELIBERATELY NEW KEY. `micro_pullback_options` belongs to the engine now called
// Dislocation, which still reads it as a migration fallback. Reusing it here would
// have fed one engine's blob to the other and silently reset both.
//
// ── WHY MULTIPLIERS AND NOT THIRTY-SIX FIELDS ─────────────────────────────────
// The detection grid is six tempos × six numbers. Exposing all thirty-six would be
// a spreadsheet, not a panel, and the numbers are not independent: the grid is a
// deliberate slope — the longer the window, the more absolute movement is required
// and the less relative acceleration. Tuning it one cell at a time destroys that
// slope. Three multipliers move the whole grid while keeping its shape, and the
// panel shows the resulting 10-second row so the effect is never abstract.
//
// The tape watch is the opposite case: four scalars that each mean something on
// their own, so they are edited directly.
//
// `#[serde(default)]` on the struct is load-bearing: a blob written by an older
// build is missing the newer fields, and they must fall back to their defaults
// rather than failing the whole parse and silently resetting every option.

use serde::{Deserialize, Serialize};

/// The tempos the UI exposes, in ascending order.
pub const TEMPO_SECS: [i64; 6] = [10, 20, 40, 60, 120, 300];

pub const APP_META_KEY: &str = "micro_pullback_v1_options";

/// See `MicroPullbackOptions::replay_pads_version`.
const REPLAY_PADS_VERSION: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MicroPullbackOptions {
    // ── Univers ────────────────────────────────────────────────────────────────
    pub price_min: f64,
    pub price_max: f64,
    pub float_max: u64,
    /// Watch tickers whose float we have never collected. On by default: a freshly
    /// listed name is often exactly the one that runs, and the alert carries a
    /// `FLOAT_UNKNOWN` tag so the decision stays with the trader.
    pub allow_unknown_float: bool,
    /// Watch only issuers in a structurally crowded industry — biotech, space,
    /// crypto, quantum, cannabis, AI. An industry we have never collected is NOT
    /// hot, so it is excluded too: that is the sharpest edge of this switch.
    pub hot_industry_only: bool,
    /// Drop issuers domiciled in the four jurisdictions over-represented in US
    /// small-cap pump & dump schemes — China (incl. Hong Kong), the Cayman Islands
    /// and the British Virgin Islands. A domicile we have never collected is NOT
    /// flagged, so it survives: we exclude on what we know, never on what we lack.
    pub exclude_risk_domicile: bool,

    // ── Seuils de détection ────────────────────────────────────────────────────
    /// Multiplies every tempo's minimum price return. 1.0 = the compiled grid.
    pub return_min_scale: f64,
    /// Multiplies every tempo's absolute share AND dollar minimum.
    pub volume_min_scale: f64,
    /// Multiplies every tempo's three acceleration ratio minimums.
    pub ratio_min_scale: f64,
    /// How many of the three acceleration ratios must clear for a tempo to trip.
    /// Two is the historical rule: a genuine departure rarely ticks all three at
    /// once, and requiring one lets a single large print through.
    ///
    /// On a feed with no trade count only two ratios exist, so a value of 3 would
    /// mute the strategy there; `sanitized` does not clamp it, but the engine
    /// counts what is available and this is documented in the panel.
    pub ratios_required: u8,

    // ── Montre de tape ─────────────────────────────────────────────────────────
    /// How long an armed ticker may wait for the tape to confirm before the watch
    /// is dropped and it becomes eligible for detection again.
    pub watch_max_secs: i64,
    /// Instantaneous rate (1-second window) that fires on the spot.
    pub watch_fast_rate_per_sec: f64,
    /// Sustained rate (5-second window).
    pub watch_slow_rate_per_sec: f64,
    /// Dollars per second the window must carry whichever layer confirms. A rate is
    /// meaningless on a name printing 100 shares at a time.
    pub watch_min_dollar_per_sec: f64,

    // ── Scoring IA ─────────────────────────────────────────────────────────────
    /// When on, every fresh departure gets the fast News-only LLM pass automatically
    /// (budgeted below). When off, the trader triggers it manually from the alert —
    /// never both at once, see `micro_pullback::quality`.
    pub llm_auto: bool,
    /// Rolling budget for the auto path only: at most this many calls in any
    /// trailing 5 minutes. A manual click is a human's own budget and ignores this.
    pub llm_budget_per_5min: u32,

    // ── Notification ───────────────────────────────────────────────────────────
    pub ntfy_alerts: bool,

    // ── Tempos actifs ──────────────────────────────────────────────────────────
    pub tempo_10s: bool,
    pub tempo_20s: bool,
    pub tempo_40s: bool,
    pub tempo_1m: bool,
    pub tempo_2m: bool,
    pub tempo_5m: bool,

    // ── Créneau ────────────────────────────────────────────────────────────────
    /// Active window, ET minutes since midnight, [start, end). Read by the live
    /// engine AND by the replay data profile, so the day scanned and downloaded for a
    /// replay is always the window the engine actually trades. Default 04:00–20:00,
    /// the whole extended day; 04:00–09:30 for premarket-only trading.
    pub session_start_min: u16,
    pub session_end_min: u16,

    // ── Données replay ─────────────────────────────────────────────────────────
    /// Real ticks kept around each pre-scan hit: minutes before (the ignition's
    /// start) and after (the trade's management). Outside, the replay synthesizes
    /// 10-second slices from the minute bars.
    pub replay_before_min: u16,
    pub replay_after_min: u16,
    /// Cap on symbols whose ticks are downloaded for one day, strongest first.
    pub replay_max_symbols: u16,
    /// Version of the replay pad defaults this blob was written under. Absent (0)
    /// in a blob from before the 5 / 15 min defaults: `load` then moves the old
    /// 2 / 10 defaults forward once, so a trader who never touched them gets the
    /// real tape around the whole of what a 10 s chart shows.
    #[serde(default)] // 0 when absent — not the struct default, or nothing migrates
    pub replay_pads_version: u8,
}

impl Default for MicroPullbackOptions {
    fn default() -> Self {
        Self {
            price_min: 1.0,
            price_max: 25.0,
            float_max: 30_000_000,
            allow_unknown_float: true,
            hot_industry_only: false,
            exclude_risk_domicile: false,

            return_min_scale: 1.0,
            volume_min_scale: 1.0,
            ratio_min_scale: 1.0,
            ratios_required: 2,

            watch_max_secs: 60,
            watch_fast_rate_per_sec: 5.0,
            watch_slow_rate_per_sec: 3.0,
            watch_min_dollar_per_sec: 3_000.0,

            llm_auto: false,
            llm_budget_per_5min: 3,

            ntfy_alerts: false,

            tempo_10s: true,
            tempo_20s: true,
            tempo_40s: true,
            tempo_1m: true,
            tempo_2m: true,
            tempo_5m: true,

            session_start_min: 240,
            session_end_min: 1200,

            replay_before_min: 5,
            replay_after_min: 15,
            replay_max_symbols: 150,
            replay_pads_version: REPLAY_PADS_VERSION,
        }
    }
}

impl MicroPullbackOptions {
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

    /// Whether `et_min` (ET minutes since midnight) falls in the active window.
    pub fn in_session(&self, et_min: u32) -> bool {
        (u32::from(self.session_start_min)..u32::from(self.session_end_min)).contains(&et_min)
    }

    /// Longest active tempo, used to size the baseline backfill. None when the user
    /// has switched every tempo off — a legitimate way to mute the strategy without
    /// disabling it.
    pub fn longest_tempo(&self) -> Option<i64> {
        self.enabled_tempos().last().copied()
    }

    /// Clamp anything a caller could get wrong. Applied on load and on write, so a
    /// hand-edited or corrupted blob can never feed the engine an absurd threshold.
    pub fn sanitized(mut self) -> Self {
        let d = Self::default();

        if !self.price_min.is_finite() || self.price_min <= 0.0 {
            self.price_min = d.price_min;
        }
        self.price_min = self.price_min.clamp(0.01, 9_999.99);
        if !self.price_max.is_finite() || self.price_max <= self.price_min {
            self.price_max = d.price_max.max(self.price_min + 0.01);
        }
        self.price_max = self.price_max.clamp(self.price_min + 0.01, 10_000.0);

        // A zero ceiling would silently mute the whole strategy.
        if self.float_max == 0 {
            self.float_max = d.float_max;
        }
        self.float_max = self.float_max.clamp(100_000, 1_000_000_000);

        // A scale of zero removes a gate entirely rather than loosening it, which is
        // never what a slider labelled "×" is asking for.
        for (v, def) in [
            (&mut self.return_min_scale, d.return_min_scale),
            (&mut self.volume_min_scale, d.volume_min_scale),
            (&mut self.ratio_min_scale, d.ratio_min_scale),
        ] {
            if !v.is_finite() || *v <= 0.0 {
                *v = def;
            }
            *v = v.clamp(0.1, 10.0);
        }
        self.ratios_required = self.ratios_required.clamp(1, 3);

        self.watch_max_secs = self.watch_max_secs.clamp(5, 600);
        for (v, def) in [
            (
                &mut self.watch_fast_rate_per_sec,
                d.watch_fast_rate_per_sec,
            ),
            (
                &mut self.watch_slow_rate_per_sec,
                d.watch_slow_rate_per_sec,
            ),
        ] {
            if !v.is_finite() || *v <= 0.0 {
                *v = def;
            }
            *v = v.clamp(0.1, 100.0);
        }
        if !self.watch_min_dollar_per_sec.is_finite() || self.watch_min_dollar_per_sec < 0.0 {
            self.watch_min_dollar_per_sec = d.watch_min_dollar_per_sec;
        }
        self.watch_min_dollar_per_sec = self.watch_min_dollar_per_sec.clamp(0.0, 10_000_000.0);

        if self.llm_budget_per_5min == 0 {
            self.llm_budget_per_5min = d.llm_budget_per_5min;
        }
        self.llm_budget_per_5min = self.llm_budget_per_5min.clamp(1, 20);

        // The extended day is 04:00–20:00; an empty or inverted window would mute
        // the strategy without saying so.
        self.session_start_min = self.session_start_min.clamp(240, 1199);
        self.session_end_min = self.session_end_min.clamp(241, 1200);
        if self.session_end_min <= self.session_start_min {
            self.session_start_min = d.session_start_min;
            self.session_end_min = d.session_end_min;
        }
        self.replay_before_min = self.replay_before_min.min(30);
        self.replay_after_min = self.replay_after_min.clamp(1, 120);
        self.replay_max_symbols = self.replay_max_symbols.clamp(1, 500);
        self
    }

    /// Read the persisted blob, falling back to defaults when absent or unparseable.
    pub fn load(conn: &rusqlite::Connection) -> Self {
        crate::local_db::cache_repository::get_app_meta(conn, APP_META_KEY)
            .and_then(|json| serde_json::from_str::<Self>(&json).ok())
            .map(Self::migrated)
            .unwrap_or_default()
            .sanitized()
    }

    /// Bring an older blob's untouched defaults forward (see `replay_pads_version`).
    /// A value the trader chose is kept.
    fn migrated(mut self) -> Self {
        if self.replay_pads_version < REPLAY_PADS_VERSION {
            if (self.replay_before_min, self.replay_after_min) == (2, 10) {
                self.replay_before_min = 5;
                self.replay_after_min = 15;
            }
            self.replay_pads_version = REPLAY_PADS_VERSION;
        }
        self
    }

    /// Persist the blob. Returns the rusqlite error as a string for the command layer.
    pub fn save(&self, conn: &rusqlite::Connection) -> Result<(), String> {
        let json = serde_json::to_string(self).map_err(|e| e.to_string())?;
        crate::local_db::cache_repository::set_app_meta(conn, APP_META_KEY, &json)
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_the_compiled_grid_untouched() {
        let o = MicroPullbackOptions::default();
        assert_eq!(o.enabled_tempos(), vec![10, 20, 40, 60, 120, 300]);
        assert_eq!(o.longest_tempo(), Some(300));
        assert_eq!(
            (o.return_min_scale, o.volume_min_scale, o.ratio_min_scale),
            (1.0, 1.0, 1.0),
            "1.0 must mean 'exactly what is compiled in'"
        );
        assert_eq!(o.ratios_required, 2, "deux sur trois est la règle d'origine");
        assert!(o.allow_unknown_float, "un float inconnu reste surveillé");
        assert!(!o.hot_industry_only && !o.exclude_risk_domicile);
    }

    #[test]
    fn a_scale_of_zero_becomes_the_default_not_an_open_gate() {
        let o = MicroPullbackOptions {
            return_min_scale: 0.0,
            volume_min_scale: -3.0,
            ratio_min_scale: f64::NAN,
            ..Default::default()
        }
        .sanitized();
        assert_eq!(o.return_min_scale, 1.0);
        assert_eq!(o.volume_min_scale, 1.0);
        assert_eq!(o.ratio_min_scale, 1.0);
    }

    #[test]
    fn every_knob_is_clamped_to_something_survivable() {
        let o = MicroPullbackOptions {
            price_min: -5.0,
            price_max: 0.5,
            float_max: 0,
            ratios_required: 9,
            watch_max_secs: 100_000,
            watch_fast_rate_per_sec: 0.0,
            watch_min_dollar_per_sec: -1.0,
            ratio_min_scale: 500.0,
            ..Default::default()
        }
        .sanitized();
        assert_eq!(o.price_min, 1.0);
        assert!(o.price_max > o.price_min);
        assert_eq!(o.float_max, 30_000_000);
        assert_eq!(o.ratios_required, 3);
        assert_eq!(o.watch_max_secs, 600);
        assert_eq!(o.watch_fast_rate_per_sec, 5.0);
        assert_eq!(o.watch_min_dollar_per_sec, 3_000.0);
        assert_eq!(o.ratio_min_scale, 10.0);
    }

    #[test]
    fn an_older_blob_keeps_what_it_had_and_defaults_the_rest() {
        let json = r#"{"price_max":9.0,"tempo_5m":false}"#;
        let o: MicroPullbackOptions = serde_json::from_str(json).unwrap();
        assert_eq!(o.price_max, 9.0, "valeur stockée conservée");
        assert!(!o.tempo_5m, "valeur stockée conservée");
        assert_eq!(o.ratios_required, 2, "champ inconnu à l'époque → défaut");
        assert_eq!(o.watch_fast_rate_per_sec, 5.0);
    }

    #[test]
    fn old_pad_defaults_move_forward_but_chosen_values_stay() {
        let old: MicroPullbackOptions =
            serde_json::from_str(r#"{"replay_before_min":2,"replay_after_min":10}"#).unwrap();
        let m = old.migrated();
        assert_eq!((m.replay_before_min, m.replay_after_min), (5, 15));
        let chosen: MicroPullbackOptions =
            serde_json::from_str(r#"{"replay_before_min":3,"replay_after_min":20}"#).unwrap();
        let m = chosen.migrated();
        assert_eq!((m.replay_before_min, m.replay_after_min), (3, 20));
        assert_eq!(m.replay_pads_version, REPLAY_PADS_VERSION);
    }

    #[test]
    fn an_inverted_session_falls_back_to_the_whole_day() {
        let o = MicroPullbackOptions {
            session_start_min: 600,
            session_end_min: 570,
            ..Default::default()
        }
        .sanitized();
        assert_eq!((o.session_start_min, o.session_end_min), (240, 1200));
        let pm = MicroPullbackOptions {
            session_start_min: 240,
            session_end_min: 570,
            ..Default::default()
        }
        .sanitized();
        assert!(pm.in_session(240) && pm.in_session(569));
        assert!(!pm.in_session(570) && !pm.in_session(239));
    }

    #[test]
    fn muting_every_tempo_is_a_legitimate_setting() {
        let o = MicroPullbackOptions {
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
}
