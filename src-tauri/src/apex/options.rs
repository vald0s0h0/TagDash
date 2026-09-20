// Apex runtime options — the "Apex" block of Settings → Stratégies → Micro Pullback.
//
// Persisted as one JSON blob in `app_meta` under `apex_v1_options`, held in
// `ApexShared::options`, and pushed into the market-state flow book on every write
// so a change lands on the very next print without a restart.
//
// Its own key, not a field of `MicroPullbackOptions`: Apex belongs to the strategy
// but runs on a different path entirely (the ingest hot path, not the one-second
// scanner loop), and a Micro Pullback settings write must never re-arm or reset it.
//
// `#[serde(default)]` on the struct is load-bearing, same as Micro Pullback's: a
// blob written by an older build is missing the newer fields, and they must fall
// back to their defaults rather than failing the parse and resetting everything.

use serde::{Deserialize, Serialize};

pub const APP_META_KEY: &str = "apex_v1_options";

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ApexOptions {
    /// Master switch. Off, the flow book does no work at all — not even the
    /// background balance on the displayed tickers — and the hotkey refuses to arm.
    pub enabled: bool,
    /// Directions Apex may open. A burst in a disabled direction is ignored.
    pub long_enabled: bool,
    pub short_enabled: bool,

    // ── Bracket ────────────────────────────────────────────────────────────────
    /// TP distance = this × ATR(14) on 10-second bars. Independent from the stop's
    /// multiplier: a scalp that pays 1.5 ATR for 1 ATR of risk is a different trade
    /// from a symmetric one, and both are legitimate here.
    pub tp_atr_multiplier: f64,
    /// SL distance = this × ATR(14). It is also what the position is sized on.
    pub sl_atr_multiplier: f64,
    /// Below this distance (in cents) the trade is not taken: the bracket would sit
    /// inside the spread noise. Applies to the stop, and to the take profit unless
    /// `tp_dynamic` is on (there is no TP price then).
    pub min_distance_cents: f64,
    /// Share of Micro Pullback's max risk ($) an Apex trade may lose at its stop.
    pub risk_pct: f64,

    // ── TP dynamique ───────────────────────────────────────────────────────────
    /// On: NO take-profit order is placed. The position is closed at market (a
    /// marketable limit in extended hours) the instant the tape turns against it —
    /// either a print balance running the other way, or one oversized opposite
    /// participant. The exit does not wait for a profit: a scalp whose tape has
    /// turned is over, whatever the open PnL.
    ///
    /// It only works while TagDash is running: nothing at the venue replaces the
    /// take-profit order this removes. The stop stays where it is.
    pub tp_dynamic: bool,
    /// Opposite-side print imbalance (%) that closes the position, over the same
    /// window as the entry trigger.
    pub exit_imbalance_pct: f64,
    /// Prints the window must hold before that balance is trusted. Lower than the
    /// entry's: getting out early is cheap, getting out late is not.
    pub exit_min_prints: u32,
    /// One opposite print this many times the ticker's own average print size ends
    /// the trade on its own — the "gros participant" rule.
    pub exit_block_ratio: f64,
    /// …provided it is at least this many shares, so a thin tape's tiny average
    /// cannot turn an ordinary lot into a block.
    pub exit_block_min_shares: u64,

    // ── SL suiveur ─────────────────────────────────────────────────────────────
    /// On: once the trade is `sl_trail_start_r` in profit, the stop follows the best
    /// price seen, `sl_trail_atr_multiplier` ATR behind it, and never moves back.
    pub sl_trailing: bool,
    /// Profit (in R, the initial stop distance) the trade must reach before the stop
    /// starts following. 0 = from the first tick.
    pub sl_trail_start_r: f64,
    /// Distance kept behind the best price, in ATR.
    pub sl_trail_atr_multiplier: f64,
    /// Smallest move (cents) worth rewriting the stop for.
    pub sl_trail_step_cents: f64,

    // ── Déclencheur (balance des prints) ───────────────────────────────────────
    /// Sliding window the print balance is measured over.
    pub window_ms: u32,
    /// Prints the window must hold before a balance means anything.
    pub min_prints: u32,
    /// |buy − sell| / (buy + sell), in percent. 50 % = a 75/25 split.
    pub imbalance_pct: f64,
    /// Window print rate over the ticker's own rolling baseline rate.
    pub burst_ratio: f64,
}

impl Default for ApexOptions {
    fn default() -> Self {
        Self {
            enabled: true,
            long_enabled: true,
            short_enabled: true,

            tp_atr_multiplier: 1.0,
            sl_atr_multiplier: 1.0,
            min_distance_cents: 6.0,
            risk_pct: 50.0,

            // Off by default: both replace a resting order with a decision TagDash
            // has to be alive to make.
            tp_dynamic: false,
            exit_imbalance_pct: 50.0,
            exit_min_prints: 5,
            exit_block_ratio: 8.0,
            exit_block_min_shares: 1_000,

            sl_trailing: false,
            sl_trail_start_r: 0.5,
            sl_trail_atr_multiplier: 1.0,
            sl_trail_step_cents: 1.0,

            // Deliberately permissive: arming is the trader's decision that a move
            // is imminent, so the trigger only has to confirm the tape agrees.
            window_ms: 1_500,
            min_prints: 8,
            imbalance_pct: 50.0,
            burst_ratio: 2.0,
        }
    }
}

impl ApexOptions {
    /// Clamp anything a caller could get wrong. Applied on load and on write, so a
    /// hand-edited blob can never hand the engine a zero-distance bracket.
    pub fn sanitized(mut self) -> Self {
        let d = Self::default();
        for (v, def, lo, hi) in [
            (&mut self.tp_atr_multiplier, d.tp_atr_multiplier, 0.1, 10.0),
            (&mut self.sl_atr_multiplier, d.sl_atr_multiplier, 0.1, 10.0),
            (
                &mut self.min_distance_cents,
                d.min_distance_cents,
                0.0,
                500.0,
            ),
            (&mut self.risk_pct, d.risk_pct, 1.0, 100.0),
            (&mut self.imbalance_pct, d.imbalance_pct, 0.0, 100.0),
            (&mut self.burst_ratio, d.burst_ratio, 0.0, 50.0),
            (
                &mut self.exit_imbalance_pct,
                d.exit_imbalance_pct,
                0.0,
                100.0,
            ),
            (&mut self.exit_block_ratio, d.exit_block_ratio, 1.0, 100.0),
            (&mut self.sl_trail_start_r, d.sl_trail_start_r, 0.0, 10.0),
            (
                &mut self.sl_trail_atr_multiplier,
                d.sl_trail_atr_multiplier,
                0.1,
                10.0,
            ),
            (
                &mut self.sl_trail_step_cents,
                d.sl_trail_step_cents,
                0.1,
                100.0,
            ),
        ] {
            if !v.is_finite() {
                *v = def;
            }
            *v = v.clamp(lo, hi);
        }
        self.exit_min_prints = self.exit_min_prints.clamp(1, 1_000);
        self.exit_block_min_shares = self.exit_block_min_shares.min(10_000_000);
        self.window_ms = self.window_ms.clamp(200, 10_000);
        self.min_prints = self.min_prints.clamp(1, 1_000);
        self
    }

    /// Minimum bracket distance in dollars.
    pub fn min_distance(&self) -> f64 {
        self.min_distance_cents / 100.0
    }

    /// Smallest trailing-stop move in dollars.
    pub fn trail_step(&self) -> f64 {
        self.sl_trail_step_cents / 100.0
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
        let o = ApexOptions::default();
        assert!(o.enabled && o.long_enabled && o.short_enabled);
        assert_eq!(o.tp_atr_multiplier, 1.0, "1 ATR par défaut");
        assert_eq!(o.sl_atr_multiplier, 1.0, "1 ATR par défaut");
        assert_eq!(o.min_distance_cents, 6.0, "6 cents minimum");
        assert_eq!(o.risk_pct, 50.0, "50 % du risque de la stratégie");
        assert!((o.min_distance() - 0.06).abs() < 1e-12);
        assert!(!o.tp_dynamic, "le TP posé d'avance reste le défaut");
        assert!(!o.sl_trailing);
        assert_eq!(o.sl_trail_start_r, 0.5);
        assert!((o.trail_step() - 0.01).abs() < 1e-12);
    }

    #[test]
    fn absurd_values_are_clamped() {
        let o = ApexOptions {
            tp_atr_multiplier: f64::NAN,
            sl_atr_multiplier: 0.0,
            min_distance_cents: -3.0,
            risk_pct: 900.0,
            window_ms: 1,
            min_prints: 0,
            imbalance_pct: 150.0,
            exit_min_prints: 0,
            exit_block_ratio: 0.2,
            sl_trail_step_cents: 0.0,
            sl_trail_atr_multiplier: 90.0,
            ..Default::default()
        }
        .sanitized();
        assert_eq!(o.tp_atr_multiplier, 1.0);
        assert_eq!(
            o.sl_atr_multiplier, 0.1,
            "jamais un bracket de largeur nulle"
        );
        assert_eq!(o.min_distance_cents, 0.0);
        assert_eq!(o.risk_pct, 100.0);
        assert_eq!(o.window_ms, 200);
        assert_eq!(o.min_prints, 1);
        assert_eq!(o.imbalance_pct, 100.0);
        assert_eq!(o.exit_min_prints, 1);
        assert_eq!(
            o.exit_block_ratio, 1.0,
            "un print ordinaire n'est pas un bloc"
        );
        assert_eq!(o.sl_trail_step_cents, 0.1);
        assert_eq!(o.sl_trail_atr_multiplier, 10.0);
    }

    #[test]
    fn an_older_blob_keeps_what_it_had_and_defaults_the_rest() {
        let o: ApexOptions = serde_json::from_str(r#"{"short_enabled":false}"#).unwrap();
        assert!(!o.short_enabled);
        assert!(o.long_enabled);
        assert_eq!(o.risk_pct, 50.0);
    }
}
