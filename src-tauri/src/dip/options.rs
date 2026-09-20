// Dip runtime options — the "Dip" block of Settings → Stratégies → Micro Pullback.
//
// Persisted as one JSON blob in `app_meta` under `dip_v1_options`, held in
// `DipShared::options`, and pushed into the market-state watch book on every write
// so a change lands on the very next print without a restart.
//
// Its own key, like Apex's and for the same reason: Dip belongs to Micro Pullback
// but runs on the ingest hot path, not the one-second scanner loop, and a Micro
// Pullback settings write must never move or drop a placed zone.
//
// `#[serde(default)]` on the struct is load-bearing: a blob written by an older
// build is missing the newer fields, and they must fall back to their defaults
// rather than failing the parse and resetting everything.

use serde::{Deserialize, Serialize};

pub const APP_META_KEY: &str = "dip_v1_options";

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DipOptions {
    /// Master switch. Off, the watch book does no work at all — not even the
    /// background price path of the displayed tickers — and the hotkey refuses.
    pub enabled: bool,
    /// Directions Dip may open. A support whose side is off is never armed: the
    /// role comes from where the price is, not from the trade the trader wants.
    pub long_enabled: bool,
    pub short_enabled: bool,

    // ── La zone ────────────────────────────────────────────────────────────────
    /// Total thickness (cents) a freshly placed zone gets — half above the level
    /// the trader pointed at, half below.
    pub thickness_cents: f64,
    /// What one press of the thinner / thicker hotkey changes the TOTAL thickness
    /// by: half of it on each side. 2 = one cent per side.
    pub step_cents: f64,

    // ── Bracket ────────────────────────────────────────────────────────────────
    /// TP distance = this × ATR(14) on 10-second bars — the very same reading Apex
    /// brackets on (`apex::levels`).
    pub tp_atr_multiplier: f64,
    /// SL distance = this × ATR(14). It is also what the position is sized on.
    pub sl_atr_multiplier: f64,
    /// Below this distance (in cents) the trade is not taken: the bracket would sit
    /// inside the spread noise.
    pub min_distance_cents: f64,
    /// Share of Micro Pullback's max risk ($) a Dip trade may lose at its stop.
    pub risk_pct: f64,

    // ── L'attaque ──────────────────────────────────────────────────────────────
    /// Sliding window the approach speed is measured over. Micro-structure: this is
    /// a fraction of a 10-second bar, not a bar count.
    pub speed_window_ms: u32,
    /// How much faster than usual the push (or the flush) has to be to count as an
    /// attack. 1.0 = the ticker's own ordinary travel over the same window, read off
    /// ATR(14) on 10-second bars; 3.0 = three times that.
    pub min_speed_ratio: f64,
    /// …and how much faster it may be before TagDash gives up: past this the move is
    /// out of reach, the order would be filled somewhere else entirely.
    pub max_speed_ratio: f64,
    /// How deep into the zone (cents, from the edge it entered by) the attack has to
    /// bite. A single print grazing the edge is not a penetration.
    pub min_penetration_cents: f64,
    /// How long an attack may last before it stops being one. Past this the price is
    /// simply sitting in the zone, and the setup waits for it to leave and come back.
    pub attack_max_ms: u32,

    // ── Le demi-tour ───────────────────────────────────────────────────────────
    /// Move back off the extreme (cents) that counts as the turn. Small on purpose:
    /// the order goes out as the price STARTS turning, not once it has turned.
    pub reversal_cents: f64,
    /// Past the far edge by this many cents the level is simply gone: the zone is
    /// dropped instead of being traded into a break.
    pub break_cents: f64,
}

impl Default for DipOptions {
    fn default() -> Self {
        Self {
            enabled: true,
            long_enabled: true,
            short_enabled: true,

            thickness_cents: 6.0,
            step_cents: 2.0,

            tp_atr_multiplier: 1.0,
            sl_atr_multiplier: 1.0,
            min_distance_cents: 6.0,
            risk_pct: 50.0,

            speed_window_ms: 1_500,
            min_speed_ratio: 3.0,
            max_speed_ratio: 20.0,
            min_penetration_cents: 1.0,
            attack_max_ms: 10_000,

            reversal_cents: 2.0,
            break_cents: 3.0,
        }
    }
}

impl DipOptions {
    /// Clamp anything a caller could get wrong. Applied on load and on write, so a
    /// hand-edited blob can never hand the engine a zero-thickness zone or a
    /// speed band whose floor sits above its ceiling.
    pub fn sanitized(mut self) -> Self {
        let d = Self::default();
        for (v, def, lo, hi) in [
            (&mut self.thickness_cents, d.thickness_cents, 1.0, 500.0),
            (&mut self.step_cents, d.step_cents, 0.2, 50.0),
            (&mut self.tp_atr_multiplier, d.tp_atr_multiplier, 0.1, 10.0),
            (&mut self.sl_atr_multiplier, d.sl_atr_multiplier, 0.1, 10.0),
            (
                &mut self.min_distance_cents,
                d.min_distance_cents,
                0.0,
                500.0,
            ),
            (&mut self.risk_pct, d.risk_pct, 1.0, 100.0),
            (&mut self.min_speed_ratio, d.min_speed_ratio, 0.0, 100.0),
            (&mut self.max_speed_ratio, d.max_speed_ratio, 0.1, 500.0),
            (
                &mut self.min_penetration_cents,
                d.min_penetration_cents,
                0.0,
                500.0,
            ),
            (&mut self.reversal_cents, d.reversal_cents, 0.1, 100.0),
            (&mut self.break_cents, d.break_cents, 0.0, 500.0),
        ] {
            if !v.is_finite() {
                *v = def;
            }
            *v = v.clamp(lo, hi);
        }
        // A ceiling under the floor would refuse every attack in silence.
        if self.max_speed_ratio < self.min_speed_ratio {
            self.max_speed_ratio = self.min_speed_ratio;
        }
        self.speed_window_ms = self.speed_window_ms.clamp(200, 10_000);
        self.attack_max_ms = self.attack_max_ms.clamp(500, 120_000);
        self
    }

    /// Default half-thickness in dollars — how far each edge sits from the level.
    pub fn half_thickness(&self) -> f64 {
        self.thickness_cents / 200.0
    }

    /// What one thinner/thicker press moves each edge by, in dollars.
    pub fn half_step(&self) -> f64 {
        self.step_cents / 200.0
    }

    /// Minimum bracket distance in dollars.
    pub fn min_distance(&self) -> f64 {
        self.min_distance_cents / 100.0
    }

    pub fn min_penetration(&self) -> f64 {
        self.min_penetration_cents / 100.0
    }

    pub fn reversal(&self) -> f64 {
        self.reversal_cents / 100.0
    }

    pub fn break_through(&self) -> f64 {
        self.break_cents / 100.0
    }

    pub fn load(conn: &rusqlite::Connection) -> Self {
        crate::local_db::cache_repository::get_app_meta(conn, APP_META_KEY)
            .and_then(|json| serde_json::from_str::<Self>(&json).ok())
            .unwrap_or_default()
            .sanitized()
    }

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
    fn defaults_match_the_spec() {
        let o = DipOptions::default();
        assert!(o.enabled && o.long_enabled && o.short_enabled);
        assert_eq!(o.tp_atr_multiplier, 1.0, "1 ATR par défaut");
        assert_eq!(o.sl_atr_multiplier, 1.0, "1 ATR par défaut");
        assert_eq!(o.thickness_cents, 6.0);
        assert!(
            (o.half_thickness() - 0.03).abs() < 1e-12,
            "6 cents = 3 de chaque côté"
        );
        assert!(
            (o.half_step() - 0.01).abs() < 1e-12,
            "un pas = un cent de chaque côté"
        );
        assert!((o.reversal() - 0.02).abs() < 1e-12);
    }

    #[test]
    fn absurd_values_are_clamped() {
        let o = DipOptions {
            thickness_cents: 0.0,
            step_cents: f64::NAN,
            sl_atr_multiplier: 0.0,
            risk_pct: 900.0,
            min_speed_ratio: 12.0,
            max_speed_ratio: 4.0,
            reversal_cents: 0.0,
            speed_window_ms: 1,
            attack_max_ms: 10,
            ..Default::default()
        }
        .sanitized();
        assert_eq!(o.thickness_cents, 1.0, "jamais une zone d'épaisseur nulle");
        assert_eq!(o.step_cents, 2.0);
        assert_eq!(o.sl_atr_multiplier, 0.1);
        assert_eq!(o.risk_pct, 100.0);
        assert_eq!(
            o.max_speed_ratio, 12.0,
            "un plafond sous le plancher refuserait tout en silence"
        );
        assert_eq!(o.reversal_cents, 0.1);
        assert_eq!(o.speed_window_ms, 200);
        assert_eq!(o.attack_max_ms, 500);
    }

    #[test]
    fn an_older_blob_keeps_what_it_had_and_defaults_the_rest() {
        let o: DipOptions = serde_json::from_str(r#"{"short_enabled":false}"#).unwrap();
        assert!(!o.short_enabled);
        assert!(o.long_enabled);
        assert_eq!(o.risk_pct, 50.0);
        assert_eq!(o.thickness_cents, 6.0);
    }
}
