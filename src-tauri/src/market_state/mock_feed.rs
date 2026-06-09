/// Mock market feed — generates synthetic trades and quotes for testing.
/// Runs as a background tokio task; stopped via the AtomicBool flag.
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, RwLock,
};

use chrono::Utc;
use rand::{Rng, SeedableRng};
use rand::rngs::StdRng;
use tokio::time::{sleep, Duration};

use crate::config::AppConfig;
use crate::market_state::MarketState;

const SYMBOLS: &[(&str, f64, &str)] = &[
    ("ABCD",  3.42,  "NASDAQ"),
    ("WXYZ",  7.18,  "NYSE"),
    ("EFGH", 12.50,  "NASDAQ"),
    ("IJKL",  4.87,  "AMEX"),
    ("MNOP", 19.33,  "NASDAQ"),
    ("QRST",  2.15,  "NYSE"),
    ("UVWX",  8.75,  "NASDAQ"),
    ("YZAB",  5.63,  "NYSE"),
    ("BCDE", 14.20,  "NASDAQ"),
    ("FGHI",  6.40,  "AMEX"),
];

pub async fn run(
    market:  Arc<RwLock<MarketState>>,
    config:  Arc<RwLock<AppConfig>>,
    running: Arc<AtomicBool>,
) {
    let mut rng = StdRng::from_entropy();

    let (warn_ms, critical_ms) = {
        let cfg = config.read().unwrap();
        (cfg.latency.warn_ms, cfg.latency.critical_ms)
    };

    // Mutable price / bid / ask per symbol
    let mut prices: Vec<f64> = SYMBOLS.iter().map(|(_, p, _)| *p).collect();
    let mut bids:   Vec<f64> = prices.iter().map(|p| p - 0.01).collect();
    let mut asks:   Vec<f64> = prices.iter().map(|p| p + 0.01).collect();

    let mut tick: u64 = 0;

    while running.load(Ordering::Relaxed) {
        // Simulate ~5–50 ms Alpaca WebSocket lag
        let lag_ms: i64 = rng.gen_range(5..=50);
        let event_time = Utc::now() - chrono::Duration::milliseconds(lag_ms);
        let now = Utc::now();

        let idx: usize = rng.gen_range(0..SYMBOLS.len());
        let sym = SYMBOLS[idx].0;

        // Geometric random walk ±0.5 %
        let pct: f64 = rng.gen_range(-0.005..=0.005_f64);
        prices[idx] = (prices[idx] * (1.0 + pct)).max(0.10);

        let half_spread: f64 = rng.gen_range(0.005..=0.03_f64);
        bids[idx] = prices[idx] - half_spread;
        asks[idx] = prices[idx] + half_spread;

        let size: u64 = rng.gen_range(100..=9900);

        {
            let mut ms = market.write().unwrap();
            ms.on_trade(sym, prices[idx], size, event_time, now, warn_ms, critical_ms);
        }

        // Quote update every 3rd tick
        if tick % 3 == 0 {
            let mut ms = market.write().unwrap();
            ms.on_quote(sym, bids[idx], asks[idx], now);
        }

        tick += 1;
        sleep(Duration::from_millis(100)).await;
    }
}
