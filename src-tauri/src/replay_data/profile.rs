// The contract between a strategy and the replay downloader.
//
// A strategy DECLARES what market data a replay of it needs — which part of the day,
// which names, which kind of tape, how much of it around each opportunity — and
// provides a cheap minute-bar pre-scan that finds those opportunities. The
// downloader only ever reads this declaration: it holds no threshold, no window, no
// strategy constant of its own. Changing a strategy's settings changes its profile,
// and with it the profile's fingerprint, which is how a prepared day knows it has to
// be completed; adding a strategy means implementing `ReplayDataNeeds` in its own
// module. The downloader is never edited for either.

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::market_state::aggregators::Bar;
use crate::review::codec::TradeRow;

/// A window of the ET trading day, in minutes since midnight: [start_min, end_min).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EtWindow {
    pub start_min: u16,
    pub end_min: u16,
}

impl EtWindow {
    pub fn contains(&self, et_min: u32) -> bool {
        (u32::from(self.start_min)..u32::from(self.end_min)).contains(&et_min)
    }
}

/// Which names a strategy can possibly trade on a day. Applied with generous
/// margins by the downloader (see `plan`) — it narrows the download, never the
/// strategy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UniverseFilter {
    pub price_min: f64,
    pub price_max: f64,
    /// Known float ceiling; None = no float constraint.
    pub float_max: Option<u64>,
    /// Admit names whose float was never collected.
    pub allow_unknown_float: bool,
    /// Volume the name must trade inside the strategy's sessions for any of its
    /// signals to be reachable at all (e.g. the smallest tempo minimum).
    pub min_session_volume: u64,
}

/// What real tape the replay needs inside each opportunity window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TickNeeds {
    /// Minute bars are enough (synthesized 10-second slices everywhere).
    None,
    Trades,
    TradesAndQuotes,
}

/// Real tape kept around each pre-scan hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowPad {
    pub before_ms: i64,
    pub after_ms: i64,
}

/// Everything the downloader needs to know about one strategy, built from its SAVED
/// settings. Serialized whole into the fingerprint, so any change here — including
/// the opaque `detection` block — marks prepared days as stale.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DataProfile {
    pub strategy_id: String,
    /// Bumped by the strategy when its pre-scan LOGIC changes (not its settings).
    pub prescan_version: u32,
    /// Where opportunities may occur. Hits outside are dropped by the planner.
    pub sessions: Vec<EtWindow>,
    pub universe: UniverseFilter,
    pub ticks: TickNeeds,
    pub pad: WindowPad,
    pub max_symbols_per_day: usize,
    /// Real trades fetched around each minute hit so the strategy can CONFIRM it on
    /// the actual tape (`SymbolScanner::confirm`) before any window is downloaded.
    /// None = minute hits are final.
    pub probe: Option<WindowPad>,
    /// How far past its hit one probe's verdict reaches: the next probe of the
    /// symbol starts there, so consecutive probes leave no gap between them.
    pub probe_step_ms: i64,
    /// After a confirmed alert the symbol stays silent until the end of the
    /// segment holding it (the engine's own lock rule). Empty = no lock.
    pub lock_segments: Vec<EtWindow>,
    /// Confirmed alerts kept per symbol and day, whatever the segments.
    pub max_alerts_per_symbol: usize,
    /// Probes spent per symbol and day before giving up on it — bounds the cost of
    /// a runner that trips minute after minute without ever confirming.
    pub max_probes_per_symbol: usize,
    /// The strategy's detection settings, opaque to the downloader. The scanner is
    /// rebuilt from THIS snapshot (never from the live settings), so a scan always
    /// matches the fingerprint it is stored under.
    pub detection: serde_json::Value,
}

impl DataProfile {
    /// Stable across runs and builds (FNV-1a over the JSON form — std's hasher is
    /// not guaranteed stable), 16 hex digits.
    pub fn fingerprint(&self) -> String {
        let json = serde_json::to_string(self).unwrap_or_default();
        format!("{:016x}", fnv1a64(json.as_bytes()))
    }

    /// The fingerprint a stored SCAN is valid under: everything but what only
    /// shapes the tick windows (pad, symbol cap), which are recomputed from the
    /// stored hits — changing those means completing ticks, not re-scanning.
    pub fn scan_fingerprint(&self) -> String {
        let mut p = self.clone();
        p.pad = WindowPad {
            before_ms: 0,
            after_ms: 0,
        };
        p.max_symbols_per_day = 0;
        p.fingerprint()
    }

    /// Whether `ms` (UTC epoch millis) falls in one of the profile's sessions.
    pub fn in_sessions(&self, ms: i64) -> bool {
        let Some(t) = chrono::DateTime::from_timestamp_millis(ms) else {
            return false;
        };
        let m = crate::time::et_minutes(t);
        self.sessions.iter().any(|w| w.contains(m))
    }
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// One opportunity found by a pre-scan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hit {
    /// Start of the minute the signal was seen in (UTC epoch millis).
    pub alert_ms: i64,
    /// Ranking key when the day's symbol cap bites — bigger is stronger.
    pub score: f64,
    /// Human-readable why, shown in the day picker.
    pub reason: String,
}

/// A pre-scan bound to one profile's detection snapshot.
pub trait SymbolScanner: Send + Sync {
    /// `bars`: one symbol's 1-minute bars for the whole day (04:00–20:00 ET),
    /// ascending. Must be RECALL-FIRST: a superset of what the live engine would
    /// fire on, since a missed window is a replay with synthetic data where the
    /// real setup was.
    fn scan(&self, bars: &[Bar]) -> Vec<Hit>;

    /// Second stage, on the real prints of the profile's `probe` span around `hit`
    /// (time-ordered): the alert the engine would actually have fired there, or
    /// None. Only called when the profile declares a probe.
    fn confirm(&self, minutes: &[Bar], hit: &Hit, trades: &[TradeRow]) -> Option<Hit> {
        let _ = (minutes, trades);
        Some(hit.clone())
    }

    /// How far BEFORE its minute hit `confirm` may place the alert. Hits are
    /// confirmed in time order, so every alert earlier than "next hit to probe −
    /// this" is already known — which is what lets a replay start on a day still
    /// being prepared. None = anywhere in the probe span.
    fn confirm_lead_ms(&self) -> Option<i64> {
        None
    }
}

/// Implemented once per strategy, in the strategy's own module.
pub trait ReplayDataNeeds: Send + Sync {
    fn id(&self) -> &'static str;
    fn label(&self) -> &'static str;
    /// The profile as the strategy's saved settings define it right now.
    fn profile(&self, conn: &Connection) -> DataProfile;
    /// The pre-scan for `profile` — built from `profile.detection` alone.
    fn scanner(&self, profile: &DataProfile) -> Box<dyn SymbolScanner>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile() -> DataProfile {
        DataProfile {
            strategy_id: "x".into(),
            prescan_version: 1,
            sessions: vec![EtWindow {
                start_min: 240,
                end_min: 570,
            }],
            universe: UniverseFilter {
                price_min: 1.0,
                price_max: 25.0,
                float_max: Some(30_000_000),
                allow_unknown_float: true,
                min_session_volume: 10_000,
            },
            ticks: TickNeeds::TradesAndQuotes,
            pad: WindowPad {
                before_ms: 120_000,
                after_ms: 600_000,
            },
            max_symbols_per_day: 80,
            probe: None,
            probe_step_ms: 0,
            lock_segments: Vec::new(),
            max_alerts_per_symbol: 4,
            max_probes_per_symbol: 8,
            detection: serde_json::json!({ "scale": 1.0 }),
        }
    }

    #[test]
    fn the_fingerprint_is_stable_and_follows_every_setting() {
        let a = profile();
        assert_eq!(a.fingerprint(), profile().fingerprint());
        assert_eq!(a.fingerprint().len(), 16);

        let mut b = profile();
        b.detection = serde_json::json!({ "scale": 1.2 });
        assert_ne!(a.fingerprint(), b.fingerprint(), "a detection setting");

        let mut c = profile();
        c.sessions[0].end_min = 600;
        assert_ne!(a.fingerprint(), c.fingerprint(), "a session window");

        let mut d = profile();
        d.prescan_version = 2;
        assert_ne!(a.fingerprint(), d.fingerprint(), "the pre-scan logic");

        // The window shape is not a scan input.
        let mut e = profile();
        e.pad.after_ms = 900_000;
        e.max_symbols_per_day = 10;
        assert_ne!(a.fingerprint(), e.fingerprint());
        assert_eq!(a.scan_fingerprint(), e.scan_fingerprint());
        assert_ne!(a.scan_fingerprint(), c.scan_fingerprint());
    }

    #[test]
    fn sessions_are_half_open() {
        let w = EtWindow {
            start_min: 240,
            end_min: 570,
        };
        assert!(w.contains(240) && w.contains(569));
        assert!(!w.contains(570) && !w.contains(239));
    }
}
