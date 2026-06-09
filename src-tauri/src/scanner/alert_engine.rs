// Alert engine: enforces per-(symbol, strategy) cooldowns and deduplication.
// Must not block the scanner loop — all operations are O(1) HashMap lookups.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::types::AlertSignal;

pub struct AlertEngine {
    /// Last alert time per (symbol, strategy_id) key.
    cooldowns: Mutex<HashMap<(String, String), Instant>>,
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
        let now = Instant::now();

        let mut map = self.cooldowns.lock().unwrap();
        if let Some(&last) = map.get(&key) {
            if now.duration_since(last) < cooldown {
                return None;
            }
        }
        map.insert(key, now);
        Some(signal.clone())
    }

    /// Reset cooldown for a symbol (called when user closes the chart zone).
    /// Starts the 2-min post-close anti-spam window immediately.
    pub fn reset(&self, symbol: &str, strategy_id: &str, post_close_window: Duration) {
        let key = (symbol.to_string(), strategy_id.to_string());
        // Insert a timestamp far enough in the past to expire after post_close_window
        let effective_last = Instant::now()
            .checked_sub(Duration::from_secs(
                post_close_window
                    .as_secs()
                    .saturating_sub(post_close_window.as_secs()),
            ))
            .unwrap_or(Instant::now());
        self.cooldowns.lock().unwrap().insert(key, effective_last);
    }
}
