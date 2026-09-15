// What the A+ layer is DOING for a symbol right now — the half of the story the
// persisted verdict cannot tell.
//
// `micro_quality_scores` records judgements, and a judgement only exists once a model
// has answered. Between the moment a candidate is handed to `run_qualification` and
// the moment a score lands there are ten to thirty seconds in which the overlay would
// otherwise be indistinguishable from "this ticker was never a candidate". The trader
// reads that pane in about ten seconds and has to know which of the two it is: an
// empty pane means "look elsewhere", a spinner means "wait two seconds".
//
// It also carries the case that actually happens on a runner: a headline lands AFTER
// the score. The verdict on screen is then genuinely stale, and the honest display is
// the raw headline plus a spinner — "we have it, the model is reading it" — rather
// than a confident number computed before the news existed.
//
// In memory on purpose. This is per-instant state with no forensic value: it never
// survives a restart, and persisting a `News` phase would resurrect a spinner for a
// call that died with the process. The verdicts themselves are in the DB.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use chrono::{DateTime, Utc};

/// Where a candidate is in the A+ pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Handed to the qualification task, waiting on one of the four slots.
    Queued,
    /// The fast pass is in flight.
    News,
    /// The deep pass is in flight.
    Deep,
    /// A headline landed after a verdict and a re-score is in flight.
    Rescoring,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Queued => "queued",
            Phase::News => "news",
            Phase::Deep => "deep",
            Phase::Rescoring => "rescoring",
        }
    }
}

/// One symbol's live A+ state, as the overlay needs it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct LiveStatus {
    pub symbol: String,
    /// None = nothing in flight; the persisted verdict (if any) is current.
    pub phase: Option<&'static str>,
    /// Headline that arrived and has not been folded into a score yet. Shown raw,
    /// because an unread headline the trader can read himself beats a stale summary.
    pub pending_headline: Option<String>,
    /// Market time (replay clock during a replay) that headline was published.
    pub pending_headline_at: Option<DateTime<Utc>>,
}

type Registry = HashMap<String, Entry>;

/// A qualification task normally clears its own entry. If one dies without doing so —
/// a panic inside a spawned task, an await that never returns — the spinner would
/// stay on that ticker's pane for the rest of the session, which is worse than
/// showing nothing: it claims a score is coming that never will. Anything older than
/// this is dropped on read. Comfortably above the worst honest case (queue wait plus
/// the 15 s news and 120 s deep budgets) and measured on the WALL clock, because
/// that is what the model calls run on even during an accelerated replay.
const STALE_AFTER: std::time::Duration = std::time::Duration::from_secs(10 * 60);

#[derive(Debug, Clone)]
struct Entry {
    phase: Option<Phase>,
    pending_headline: Option<String>,
    pending_headline_at: Option<DateTime<Utc>>,
    touched: std::time::Instant,
}

impl Default for Entry {
    fn default() -> Self {
        Self {
            phase: None,
            pending_headline: None,
            pending_headline_at: None,
            touched: std::time::Instant::now(),
        }
    }
}

fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Enter `phase` for `symbol`.
pub fn set_phase(symbol: &str, phase: Phase) {
    if let Ok(mut map) = registry().lock() {
        let e = map.entry(symbol.to_string()).or_default();
        e.phase = Some(phase);
        e.touched = std::time::Instant::now();
    }
}

/// Leave whatever phase was in flight. The pending headline is cleared too: the pass
/// that just finished is the one that read it.
pub fn clear_phase(symbol: &str) {
    if let Ok(mut map) = registry().lock() {
        map.remove(symbol);
    }
}

/// Record a headline the current verdict has not seen. Kept even while a pass runs —
/// that is exactly the "loader on an unread headline" the overlay draws.
pub fn set_pending_headline(symbol: &str, headline: &str, at: DateTime<Utc>) {
    if let Ok(mut map) = registry().lock() {
        let e = map.entry(symbol.to_string()).or_default();
        e.pending_headline = Some(headline.to_string());
        e.pending_headline_at = Some(at);
        e.touched = std::time::Instant::now();
    }
}

/// Everything currently in flight, for the overlay's poll.
pub fn snapshot() -> Vec<LiveStatus> {
    let Ok(mut map) = registry().lock() else {
        return Vec::new();
    };
    map.retain(|_, e| e.touched.elapsed() < STALE_AFTER);
    map.iter()
        .map(|(symbol, e)| LiveStatus {
            symbol: symbol.clone(),
            phase: e.phase.map(Phase::as_str),
            pending_headline: e.pending_headline.clone(),
            pending_headline_at: e.pending_headline_at,
        })
        .collect()
}

/// Drop everything. Called when a replay starts, seeks backwards or rolls to another
/// day: the spinners of the session being abandoned must not survive into the next.
pub fn reset() {
    if let Ok(mut map) = registry().lock() {
        map.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The registry is process-global, so these tests would otherwise wipe each
    /// other's entries through `reset` while running in parallel.
    static SERIAL: Mutex<()> = Mutex::new(());

    fn only(symbol: &str) -> Option<LiveStatus> {
        snapshot().into_iter().find(|s| s.symbol == symbol)
    }

    #[test]
    fn a_phase_is_visible_until_it_is_cleared() {
        let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        set_phase("AAA", Phase::News);
        assert_eq!(only("AAA").unwrap().phase, Some("news"));
        set_phase("AAA", Phase::Deep);
        assert_eq!(only("AAA").unwrap().phase, Some("deep"));
        clear_phase("AAA");
        assert!(only("AAA").is_none());
        reset();
    }

    #[test]
    fn an_unread_headline_survives_a_phase_change() {
        let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        let at = Utc::now();
        set_phase("BBB", Phase::News);
        set_pending_headline("BBB", "FDA clearance", at);
        set_phase("BBB", Phase::Deep);
        let s = only("BBB").unwrap();
        // The deep pass is running BECAUSE of that headline; dropping it here would
        // leave the overlay spinning with nothing to show for it.
        assert_eq!(s.pending_headline.as_deref(), Some("FDA clearance"));
        assert_eq!(s.phase, Some("deep"));
        reset();
    }

    #[test]
    fn an_abandoned_entry_stops_spinning() {
        let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        set_phase("DDD", Phase::Deep);
        // Simulate a task that died without clearing: only its age changes.
        if let Ok(mut map) = registry().lock() {
            let e = map.get_mut("DDD").unwrap();
            e.touched = std::time::Instant::now() - (STALE_AFTER + std::time::Duration::from_secs(1));
        }
        assert!(only("DDD").is_none(), "a leaked entry must not spin forever");
        reset();
    }

    #[test]
    fn finishing_the_pass_clears_the_headline_it_read() {
        let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        set_pending_headline("CCC", "partnership", Utc::now());
        set_phase("CCC", Phase::Rescoring);
        clear_phase("CCC");
        assert!(only("CCC").is_none());
        reset();
    }
}
