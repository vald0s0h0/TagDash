// Attention du marché — thresholds, edited from the screen's own settings popover.
//
// One JSON blob in `app_config` under `attention_map_v1_options`, same contract as
// the strategy option blobs: `#[serde(default)]` so a blob written by an older
// build keeps its values and only gains the new fields.

use serde::{Deserialize, Serialize};

pub const APP_META_KEY: &str = "attention_map_v1_options";

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AttentionOptions {
    // ── Entrée sur la carte ────────────────────────────────────────────────────
    /// Relative activity (slot dollars / historical median) a ticker must reach
    /// once to be remembered.
    pub min_rel: f64,
    /// Or: absolute move since the previous close, in percent.
    pub min_move_pct: f64,
    /// Dollars a 5-minute slot must carry to count as activity at all. Below it a
    /// slot is drawn but never sets a peak, an extreme or a pattern.
    pub min_dollars_5m: f64,
    /// Floor of the denominator: a name that normally trades nothing at that hour
    /// is compared with this many dollars instead of with zero.
    pub baseline_floor: f64,
    /// Micro/small versus mid/big.
    pub small_cap_max: f64,

    // ── Météo — every gauge is a ratio to its own historical median at that hour ─
    pub heat_low: f64,
    pub heat_high: f64,
    pub heat_extreme: f64,
    pub breadth_low: f64,
    pub breadth_high: f64,
    pub concentration_low: f64,
    pub concentration_high: f64,
}

impl Default for AttentionOptions {
    fn default() -> Self {
        Self {
            min_rel: 3.0,
            min_move_pct: 5.0,
            min_dollars_5m: 25_000.0,
            baseline_floor: 5_000.0,
            small_cap_max: 2_000_000_000.0,
            heat_low: 0.7,
            heat_high: 1.5,
            heat_extreme: 2.5,
            breadth_low: 0.75,
            breadth_high: 1.35,
            concentration_low: 0.8,
            concentration_high: 1.25,
        }
    }
}

impl AttentionOptions {
    /// Clamp every field into a range the engine can divide by. A hand-edited blob
    /// or a cleared input must never produce a zero floor or inverted thresholds.
    pub fn sanitized(mut self) -> Self {
        let d = Self::default();
        let fix = |v: f64, lo: f64, hi: f64, def: f64| {
            if v.is_finite() {
                v.clamp(lo, hi)
            } else {
                def
            }
        };
        self.min_rel = fix(self.min_rel, 1.0, 1_000.0, d.min_rel);
        self.min_move_pct = fix(self.min_move_pct, 0.5, 500.0, d.min_move_pct);
        self.min_dollars_5m = fix(self.min_dollars_5m, 1_000.0, 50_000_000.0, d.min_dollars_5m);
        self.baseline_floor = fix(self.baseline_floor, 100.0, 10_000_000.0, d.baseline_floor);
        self.small_cap_max = fix(self.small_cap_max, 1e7, 1e12, d.small_cap_max);
        self.heat_low = fix(self.heat_low, 0.05, 1.0, d.heat_low);
        self.heat_high = fix(self.heat_high, 1.0, 20.0, d.heat_high);
        self.heat_extreme = fix(self.heat_extreme, self.heat_high, 50.0, d.heat_extreme);
        self.breadth_low = fix(self.breadth_low, 0.05, 1.0, d.breadth_low);
        self.breadth_high = fix(self.breadth_high, 1.0, 20.0, d.breadth_high);
        self.concentration_low = fix(self.concentration_low, 0.05, 1.0, d.concentration_low);
        self.concentration_high = fix(self.concentration_high, 1.0, 20.0, d.concentration_high);
        self
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
    fn an_older_blob_keeps_its_values_and_gains_the_rest() {
        let o: AttentionOptions = serde_json::from_str(r#"{"min_rel":4.5}"#).unwrap();
        assert_eq!(o.min_rel, 4.5);
        assert_eq!(o.baseline_floor, AttentionOptions::default().baseline_floor);
    }

    #[test]
    fn sanitize_never_leaves_a_zero_floor_or_inverted_heat() {
        let o = AttentionOptions {
            baseline_floor: 0.0,
            min_dollars_5m: f64::NAN,
            heat_high: 3.0,
            heat_extreme: 2.0,
            ..Default::default()
        }
        .sanitized();
        assert!(o.baseline_floor >= 100.0);
        assert_eq!(o.min_dollars_5m, AttentionOptions::default().min_dollars_5m);
        assert!(o.heat_extreme >= o.heat_high);
    }
}
