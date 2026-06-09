use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Timeframe {
    #[serde(rename = "5s")]
    S5,
    #[serde(rename = "10s")]
    S10,
    #[serde(rename = "1m")]
    M1,
    #[serde(rename = "2m")]
    M2,
    #[serde(rename = "5m")]
    M5,
    #[serde(rename = "15m")]
    M15,
    #[serde(rename = "daily")]
    Daily,
}

impl Timeframe {
    pub fn seconds(self) -> i64 {
        match self {
            Timeframe::S5    =>     5,
            Timeframe::S10   =>    10,
            Timeframe::M1    =>    60,
            Timeframe::M2    =>   120,
            Timeframe::M5    =>   300,
            Timeframe::M15   =>   900,
            Timeframe::Daily => 86400,
        }
    }

    pub const ALL: &'static [Timeframe] = &[
        Timeframe::S5,
        Timeframe::S10,
        Timeframe::M1,
        Timeframe::M2,
        Timeframe::M5,
        Timeframe::M15,
        Timeframe::Daily,
    ];

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "5s"    => Some(Timeframe::S5),
            "10s"   => Some(Timeframe::S10),
            "1m"    => Some(Timeframe::M1),
            "2m"    => Some(Timeframe::M2),
            "5m"    => Some(Timeframe::M5),
            "15m"   => Some(Timeframe::M15),
            "daily" => Some(Timeframe::Daily),
            _       => None,
        }
    }
}

/// One closed OHLCV candle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bar {
    pub time: DateTime<Utc>,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: u64,
    pub vwap: Option<f64>,
    /// Number of trades in the bar (Alpaca minute-bar `n`). None for bars built
    /// from trade ticks or historical sources that don't carry a trade count.
    #[serde(default)]
    pub trade_count: Option<u64>,
}

/// Builds one candle at a time from raw trade ticks.
/// Emits a closed Bar when the time bucket rolls over.
pub struct CandleAggregator {
    pub timeframe: Timeframe,
    bar_open_time: Option<DateTime<Utc>>,
    open:   f64,
    high:   f64,
    low:    f64,
    close:  f64,
    volume: u64,
    pv_sum: f64, // price × volume, for VWAP
}

impl CandleAggregator {
    pub fn new(timeframe: Timeframe) -> Self {
        Self {
            timeframe,
            bar_open_time: None,
            open:   0.0,
            high:   0.0,
            low:    0.0,
            close:  0.0,
            volume: 0,
            pv_sum: 0.0,
        }
    }

    /// Feed one trade. Returns a closed Bar when the bucket flips.
    pub fn on_trade(&mut self, price: f64, size: u64, trade_time: DateTime<Utc>) -> Option<Bar> {
        let bar_secs  = self.timeframe.seconds();
        let bar_start = bucket_start(trade_time, bar_secs);

        match self.bar_open_time {
            None => {
                self.reset(price, size, bar_start);
                None
            }
            Some(current_start) if bar_start > current_start => {
                let closed = self.close_bar(current_start);
                self.reset(price, size, bar_start);
                Some(closed)
            }
            _ => {
                self.update(price, size);
                None
            }
        }
    }

    fn reset(&mut self, price: f64, size: u64, bar_start: DateTime<Utc>) {
        self.open         = price;
        self.high         = price;
        self.low          = price;
        self.close        = price;
        self.volume       = size;
        self.pv_sum       = price * size as f64;
        self.bar_open_time = Some(bar_start);
    }

    fn update(&mut self, price: f64, size: u64) {
        self.high   = self.high.max(price);
        self.low    = self.low.min(price);
        self.close  = price;
        self.volume += size;
        self.pv_sum += price * size as f64;
    }

    /// The in-progress (not-yet-closed) bar, if any trades have arrived in the
    /// current bucket. Lets the chart render the live forming candle instead of
    /// waiting for the bucket to flip.
    pub fn current_bar(&self) -> Option<Bar> {
        self.bar_open_time.map(|t| self.close_bar(t))
    }

    fn close_bar(&self, bar_open_time: DateTime<Utc>) -> Bar {
        Bar {
            time:   bar_open_time,
            open:   self.open,
            high:   self.high,
            low:    self.low,
            close:  self.close,
            volume: self.volume,
            vwap:   if self.volume > 0 {
                        Some(self.pv_sum / self.volume as f64)
                    } else {
                        None
                    },
            // Trade ticks don't carry a per-bar trade count; only Alpaca minute
            // bars (via on_bar) populate this.
            trade_count: None,
        }
    }
}

fn bucket_start(t: DateTime<Utc>, bar_secs: i64) -> DateTime<Utc> {
    let ts     = t.timestamp();
    let bucket = (ts / bar_secs) * bar_secs;
    Utc.timestamp_opt(bucket, 0).single().unwrap_or(t)
}
