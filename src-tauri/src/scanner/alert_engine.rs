// Alert engine: enforces per-(symbol, strategy) cooldowns and deduplication.
// Must not block the scanner loop — all operations are O(1) HashMap lookups.
//
// Cooldowns are measured on the APP clock (`crate::time::now()`): identical to
// the wall clock live, and the simulated clock during a Market Replay — so an
// accelerated replay expires cooldowns at the same *market-time* pace as live.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::types::AlertSignal;

pub struct AlertEngine {
    /// Last alert time per (symbol, strategy_id) key (app-clock instant).
    cooldowns: Mutex<HashMap<(String, String), DateTime<Utc>>>,
}

impl AlertEngine {
    pub fn new() -> Self {
        Self {
            cooldowns: Mutex::new(HashMap::new()),
        }
    }

    /// Returns the signal if it passes the cooldown check, None if suppressed.
    /// Thread-safe; takes the mutex briefly.
    pub fn process(&self, signal: &AlertSignal, cooldown: Duration) -> Option<AlertSignal> {
        let key = (signal.symbol.clone(), signal.strategy_id.clone());
        let now = crate::time::now();

        let mut map = self.cooldowns.lock().unwrap();
        if let Some(&last) = map.get(&key) {
            let elapsed = (now - last).num_milliseconds().max(0) as u128;
            if elapsed < cooldown.as_millis() {
                return None;
            }
        }
        map.insert(key, now);
        Some(signal.clone())
    }

    /// Reset cooldown for a symbol (called when user closes the chart zone).
    /// Starts the 2-min post-close anti-spam window immediately.
    pub fn reset(&self, symbol: &str, strategy_id: &str, _post_close_window: Duration) {
        let key = (symbol.to_string(), strategy_id.to_string());
        self.cooldowns.lock().unwrap().insert(key, crate::time::now());
    }
}
