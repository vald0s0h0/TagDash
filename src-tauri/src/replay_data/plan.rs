// What a day needs, decided from the declared profiles and what the store holds.
//
// Pure functions: no network, no clock. The only constants here are SAFETY MARGINS
// on the universe filters (a stock's price and float move; a margin that is too
// wide costs a few KB of minutes, one that is too narrow loses a runner). Every
// threshold that decides WHETHER something is an opportunity lives in the
// strategy's profile and pre-scan.

use std::collections::{BTreeMap, HashMap};

use serde::Serialize;

use super::intervals::{merge, subtract, Span};
use super::store::{MarketRow, TickKind, BUCKETS};
use super::{DataProfile, Hit, TickNeeds};


/// Band widening for the daily range. Alpaca's daily bar spans the whole extended
/// session in VOLUME (it is the sum of the minutes), but not in price: extended-
/// hours prints (form T) never set the day's high or low. A premarket spike can sit
/// far above the regular-session range — measured: PMAX alerted at $2.22 in
/// premarket on a day whose high reads $1.86.
const PRICE_MARGIN: f64 = 2.0;

/// Whether the day's range, widened by `PRICE_MARGIN`, reaches the profile's band.
pub fn admits_price(p: &DataProfile, m: &MarketRow) -> bool {
    m.h * PRICE_MARGIN >= p.universe.price_min && m.l / PRICE_MARGIN <= p.universe.price_max
}

/// The whole day's volume bounds any session's: below the profile's minimum, no
/// signal is reachable.
pub fn admits_volume(p: &DataProfile, m: &MarketRow) -> bool {
    m.v.max(0) as u64 >= p.universe.min_session_volume
}

/// `float`: the current float, None when never collected. Exact, no margin: the
/// replay engine gates on this very current float, so a name above the ceiling
/// could never alert in replay — its ticks would be downloaded for nothing
/// (measured on 2026-06-29: 5 of 12 confirmed names were over 30M).
pub fn admits_float(p: &DataProfile, float: Option<u64>) -> bool {
    match (p.universe.float_max, float) {
        (None, _) => true,
        (Some(_), None) => p.universe.allow_unknown_float,
        (Some(max), Some(f)) => f <= max,
    }
}

/// Volume traded in the 10-minute buckets that overlap the profile's sessions.
pub fn session_volume(p: &DataProfile, buckets: &[u64]) -> u64 {
    (0..BUCKETS.min(buckets.len()))
        .filter(|i| {
            let lo = 240 + (*i as u32) * 10;
            let hi = lo + 10;
            p.sessions
                .iter()
                .any(|w| u32::from(w.start_min) < hi && lo < u32::from(w.end_min))
        })
        .map(|i| buckets[i])
        .sum()
}

/// Candidates whose minutes are worth fetching for at least one profile.
pub fn candidates(
    profiles: &[DataProfile],
    market: &[MarketRow],
    floats: &HashMap<String, u64>,
) -> Vec<String> {
    let mut out: Vec<String> = market
        .iter()
        .filter(|m| {
            profiles.iter().any(|p| {
                admits_price(p, m)
                    && admits_volume(p, m)
                    && admits_float(p, floats.get(&m.symbol).copied())
            })
        })
        .map(|m| m.symbol.clone())
        .collect();
    out.sort();
    out
}

/// Whether a fetched candidate's minutes are kept (and scanned) for `p`.
pub fn eligible(p: &DataProfile, m: &MarketRow, float: Option<u64>, buckets: &[u64]) -> bool {
    admits_price(p, m)
        && admits_volume(p, m)
        && admits_float(p, float)
        && session_volume(p, buckets) >= p.universe.min_session_volume
}

/// The tick windows a profile wants: strongest symbols first up to its cap, each hit
/// widened by the pad (the hit minute itself included), clipped to the day and
/// snapped outward to whole minutes — the replay synthesizes whole minutes outside
/// coverage, so a partly covered minute would otherwise show neither.
pub fn windows(p: &DataProfile, hits: &[(String, Hit)], day: Span) -> BTreeMap<String, Vec<Span>> {
    let mut best: HashMap<&str, f64> = HashMap::new();
    for (sym, h) in hits {
        let e = best.entry(sym.as_str()).or_insert(f64::MIN);
        *e = e.max(h.score);
    }
    let mut ranked: Vec<(&str, f64)> = best.into_iter().collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal).then(a.0.cmp(b.0)));
    ranked.truncate(p.max_symbols_per_day);

    let mut out: BTreeMap<String, Vec<Span>> = BTreeMap::new();
    for (sym, _) in ranked {
        let spans: Vec<Span> = hits
            .iter()
            .filter(|(s, _)| s == sym)
            .map(|(_, h)| {
                (
                    ((h.alert_ms - p.pad.before_ms).div_euclid(60_000) * 60_000).max(day.0),
                    ((h.alert_ms + 60_000 + p.pad.after_ms + 59_999).div_euclid(60_000) * 60_000)
                        .min(day.1),
                )
            })
            .collect();
        out.insert(sym.to_string(), merge(spans));
    }
    out
}

/// Index of the lock segment holding `ms`, if any.
pub fn segment_of(p: &DataProfile, ms: i64) -> Option<usize> {
    let t = chrono::DateTime::from_timestamp_millis(ms)?;
    let m = crate::time::et_minutes(t);
    p.lock_segments.iter().position(|w| w.contains(m))
}

/// Where a symbol may alert again after a confirmed alert at `alert_ms`: the end of
/// the lock segment holding it, or right away when none does.
pub fn unlock_at(p: &DataProfile, alert_ms: i64, day: Span) -> i64 {
    let Some(t) = chrono::DateTime::from_timestamp_millis(alert_ms) else {
        return alert_ms;
    };
    let m = crate::time::et_minutes(t);
    p.lock_segments
        .iter()
        .find(|w| w.contains(m))
        .map(|w| day.0 + (i64::from(w.end_min) - 240) * 60_000)
        .unwrap_or(alert_ms)
}

/// The next hit worth probing for one symbol: the first one at or after `from_ms`
/// (hits time-ordered). Hits inside a previous probe's span were already seen by
/// it, so the caller passes that span's end.
pub fn next_probe<'a>(hits: &'a [Hit], from_ms: i64) -> Option<&'a Hit> {
    hits.iter().find(|h| h.alert_ms >= from_ms)
}

fn kinds(t: TickNeeds) -> &'static [TickKind] {
    match t {
        TickNeeds::None => &[],
        TickNeeds::Trades => &[TickKind::Trades],
        TickNeeds::TradesAndQuotes => &[TickKind::Trades, TickKind::Quotes],
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TickJob {
    pub symbol: String,
    pub kind: TickKind,
    pub span: Span,
}

/// Everything still missing to serve these windows, given what is covered.
pub fn tick_jobs(
    wanted: &[(TickNeeds, BTreeMap<String, Vec<Span>>)],
    covered: &HashMap<(String, TickKind), Vec<Span>>,
) -> Vec<TickJob> {
    let mut want: BTreeMap<(String, &'static str), (TickKind, Vec<Span>)> = BTreeMap::new();
    for (needs, wins) in wanted {
        for kind in kinds(*needs) {
            for (sym, spans) in wins {
                want.entry((sym.clone(), kind.as_str()))
                    .or_insert((*kind, Vec::new()))
                    .1
                    .extend(spans.iter().copied());
            }
        }
    }
    let mut out = Vec::new();
    for ((sym, _), (kind, spans)) in want {
        let have = covered.get(&(sym.clone(), kind)).cloned().unwrap_or_default();
        for span in subtract(spans, have) {
            out.push(TickJob {
                symbol: sym.clone(),
                kind,
                span,
            });
        }
    }
    out
}

// ─── A day still being prepared ─────────────────────────────────────────────────

/// Opportunities that must be fully on disk before a day still being prepared can
/// be replayed: the session starts on them while the rest downloads behind it.
pub const QUICK_START_HITS: usize = 2;

/// How far a replay may run when every alert before `final_ms` is known and has its
/// ticks: an alert still to be found gets real ticks from `pad.before` ahead of it,
/// and the replay must not already have synthesized those minutes. One more minute
/// of margin, on a whole minute like the windows themselves.
pub fn replayable_until(p: &DataProfile, final_ms: i64) -> i64 {
    (final_ms - p.pad.before_ms).div_euclid(60_000) * 60_000 - 60_000
}

/// Whether a hit's whole tick window is on disk: its symbol made the day's cut
/// (`wins`) and no job is left inside the window.
pub fn hit_ready(
    p: &DataProfile,
    symbol: &str,
    hit: &Hit,
    wins: &BTreeMap<String, Vec<Span>>,
    jobs: &[TickJob],
) -> bool {
    if kinds(p.ticks).is_empty() {
        return true;
    }
    let (lo, hi) = (hit.alert_ms - p.pad.before_ms, hit.alert_ms + 60_000 + p.pad.after_ms);
    wins.contains_key(symbol)
        && !jobs
            .iter()
            .any(|j| j.symbol == symbol && j.span.0 < hi && lo < j.span.1)
}

// ─── Status ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DayState {
    /// Everything the chosen strategies need is on disk.
    Ready,
    /// Something is missing: settings changed since the scan, or ticks.
    Incomplete,
    NotPrepared,
    /// Weekend, market holiday, future, or not yet 15 minutes past the close.
    Unavailable,
}

#[derive(Debug, Clone, Serialize)]
pub struct StrategyDayStatus {
    pub strategy_id: String,
    /// The stored scan matches the current settings.
    pub scanned: bool,
    /// A scan under the current settings is under way: its first hits are usable.
    pub partial: bool,
    pub hits: usize,
    pub symbols: usize,
    /// Tick time still to download for this strategy's windows (ms).
    pub missing_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Opportunity {
    pub strategy_id: String,
    pub symbol: String,
    pub alert_ms: i64,
    pub score: f64,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DayStatus {
    pub day: String,
    pub state: DayState,
    pub strategies: Vec<StrategyDayStatus>,
    /// Hits of the chosen strategies (current scans only), time-ordered.
    pub opportunities: Vec<Opportunity>,
    pub missing_ms: i64,
    /// Estimated download for what is missing.
    pub estimate_bytes: i64,
    pub estimate_requests: usize,
    pub bytes_on_disk: u64,
    /// Opportunities whose ticks are entirely on disk.
    pub ready_hits: usize,
    /// On a day not finished preparing: how far a replay can already run (every
    /// opportunity before it is known, with its ticks). None when the day is ready
    /// — or when nothing usable is on disk yet.
    pub ready_until_ms: Option<i64>,
    /// A replay can start now: the day is ready, or its first opportunities are and
    /// the rest can download while it runs.
    pub launchable: bool,
    /// Trades taken that day on a ticker these strategies' scanners own (filled by
    /// the command layer from the TradeTally mirror).
    pub trades: usize,
}

/// Bytes per millisecond of stored ticks when this machine has no history yet
/// (trades + quotes of an active small cap ≈ 4 KB per minute, compressed).
pub const DEFAULT_BYTES_PER_MS: f64 = 4_000.0 / 60_000.0;

/// Estimated (bytes, requests) for a set of jobs.
pub fn estimate(jobs: &[TickJob], bytes_per_ms: f64) -> (i64, usize) {
    let ms: i64 = jobs.iter().map(|j| j.span.1 - j.span.0).sum();
    ((ms as f64 * bytes_per_ms) as i64, jobs.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replay_data::{EtWindow, UniverseFilter, WindowPad};

    fn profile(start: u16, end: u16) -> DataProfile {
        DataProfile {
            strategy_id: "mp".into(),
            prescan_version: 1,
            sessions: vec![EtWindow {
                start_min: start,
                end_min: end,
            }],
            universe: UniverseFilter {
                price_min: 1.0,
                price_max: 25.0,
                float_max: Some(30_000_000),
                allow_unknown_float: true,
                min_session_volume: 8_000,
            },
            ticks: TickNeeds::TradesAndQuotes,
            pad: WindowPad {
                before_ms: 120_000,
                after_ms: 600_000,
            },
            max_symbols_per_day: 2,
            probe: None,
            probe_step_ms: 0,
            lock_segments: Vec::new(),
            max_alerts_per_symbol: 4,
            max_probes_per_symbol: 8,
            detection: serde_json::Value::Null,
        }
    }

    fn market(sym: &str, price: f64) -> MarketRow {
        MarketRow {
            symbol: sym.into(),
            prev_close: Some(price),
            o: price,
            h: price,
            l: price,
            c: price,
            v: 1_000_000,
        }
    }

    fn hit(ms: i64, score: f64) -> Hit {
        Hit {
            alert_ms: ms,
            score,
            reason: String::new(),
        }
    }

    #[test]
    fn candidates_follow_the_day_range_volume_and_float() {
        let p = profile(240, 570);
        let mut spike = market("SPIKE", 0.4);
        spike.h = 0.6; // ×2 reaches $1: a premarket spike may have
        let mut thin = market("THIN", 5.0);
        thin.v = 5_000;
        let m = vec![
            market("LOW", 0.9),
            spike,
            market("MID", 12.0),
            market("HIGH", 60.0),
            market("BIG", 5.0),
            thin,
        ];
        let floats: HashMap<String, u64> =
            [("BIG".to_string(), 500_000_000u64), ("MID".to_string(), 20_000_000)].into();
        // LOW ($0.9) and SPIKE reach the band within the margin; HIGH ($60 ÷ 2) does
        // not. MID's 20M float is under the 30M ceiling, BIG's is not. THIN never
        // traded enough.
        assert_eq!(
            candidates(&[p], &m, &floats),
            vec!["LOW".to_string(), "MID".to_string(), "SPIKE".to_string()]
        );
    }

    #[test]
    fn session_volume_reads_only_the_strategy_window() {
        let mut b = vec![0u64; BUCKETS];
        b[0] = 5_000; // 04:00–04:10
        b[33] = 9_000; // 09:30–09:40
        assert_eq!(session_volume(&profile(240, 570), &b), 5_000);
        assert_eq!(session_volume(&profile(570, 960), &b), 9_000);
        assert_eq!(session_volume(&profile(240, 1200), &b), 14_000);
    }

    #[test]
    fn windows_keep_the_strongest_symbols_and_merge() {
        let p = profile(240, 570);
        let hits = vec![
            ("A".to_string(), hit(1_000_000, 5.0)),
            ("A".to_string(), hit(1_100_000, 2.0)),
            ("B".to_string(), hit(2_000_000, 9.0)),
            ("C".to_string(), hit(3_000_000, 1.0)),
        ];
        let w = windows(&p, &hits, (0, 10_000_000));
        assert_eq!(w.keys().cloned().collect::<Vec<_>>(), vec!["A", "B"], "cap = 2");
        // A: 1_000_000−120_000 .. 1_100_000+60_000+600_000, merged, snapped to
        // whole minutes (840_000 = 14 min, 1_800_000 = 30 min).
        assert_eq!(w["A"], vec![(840_000, 1_800_000)]);
    }

    #[test]
    fn jobs_are_the_uncovered_delta_per_kind() {
        let p = profile(240, 570);
        let wins: BTreeMap<String, Vec<Span>> = [("A".to_string(), vec![(0, 100)])].into();
        let covered: HashMap<(String, TickKind), Vec<Span>> =
            [(("A".to_string(), TickKind::Trades), vec![(0, 60)])].into();
        let jobs = tick_jobs(&[(p.ticks, wins)], &covered);
        assert_eq!(
            jobs,
            vec![
                TickJob { symbol: "A".into(), kind: TickKind::Quotes, span: (0, 100) },
                TickJob { symbol: "A".into(), kind: TickKind::Trades, span: (60, 100) },
            ]
        );
    }

    #[test]
    fn a_replay_stays_a_pad_and_a_minute_behind_what_is_final() {
        let p = profile(240, 570); // pad.before = 2 min
        // Final up to 07:30:20 → an alert at 07:30:20 would want ticks from 07:28:20,
        // i.e. the 07:28 minute: the replay may run to 07:27.
        let final_ms = (7 * 60 + 30) * 60_000 + 20_000;
        assert_eq!(replayable_until(&p, final_ms), (7 * 60 + 27) * 60_000);
    }

    #[test]
    fn a_hit_is_ready_once_nothing_is_missing_in_its_window() {
        let p = profile(240, 570);
        let h = hit(1_000_000, 5.0);
        let wins = windows(&p, &[("A".to_string(), h.clone())], (0, 10_000_000));
        let job = |span| TickJob { symbol: "A".into(), kind: TickKind::Quotes, span };
        assert!(hit_ready(&p, "A", &h, &wins, &[]));
        assert!(!hit_ready(&p, "A", &h, &wins, &[job((1_500_000, 1_560_000))]));
        assert!(hit_ready(&p, "A", &h, &wins, &[job((5_000_000, 5_060_000))]), "another window's job");
        assert!(!hit_ready(&p, "B", &h, &wins, &[]), "a symbol the day's cap left out");
    }

    #[test]
    fn minutes_only_profiles_need_no_ticks() {
        let wins: BTreeMap<String, Vec<Span>> = [("A".to_string(), vec![(0, 100)])].into();
        assert!(tick_jobs(&[(TickNeeds::None, wins)], &HashMap::new()).is_empty());
    }
}
