// Live progress of one Panic Score run, polled by the button in the screener header.
//
// Mirrors the `StartupState` pattern: a plain struct behind an `RwLock` in AppState,
// serialised to the frontend on a short poll. No events, no channels — the UI already
// polls the screener every 800 ms, so one more cheap read costs nothing and cannot
// desynchronise the way a missed event can.

use serde::Serialize;

/// Which stage the run is in. Serialised as-is so the button can label itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// No run has happened this session.
    Idle,
    /// Gathering the payloads (DB reads, news cascade, optional web).
    Collecting,
    /// Per-ticker scoring calls in flight.
    Scoring,
    /// The final consistency pass over every ticker's score.
    Calibrating,
    Done,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub struct PanicRunState {
    pub phase: Phase,
    /// True while anything is in flight — the button's disabled state.
    pub running: bool,
    /// ET day the run belongs to, so a stale result from another day is visible.
    pub day: String,
    /// Whether the run was made under a replay clock.
    pub simulated: bool,
    pub total: usize,
    /// Tickers whose scoring call has returned (success or failure).
    pub done: usize,
    /// Symbols whose scoring call failed. The run still completes for the rest — one
    /// bad ticker must not cost the user the whole analysis.
    pub failed: Vec<String>,
    /// Set when the run itself could not proceed (no key, no symbols, model refused).
    pub error: Option<String>,
    /// True when the calibration pass adjusted at least one score.
    pub calibrated: bool,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
}

impl Default for PanicRunState {
    fn default() -> Self {
        Self {
            phase: Phase::Idle,
            running: false,
            day: String::new(),
            simulated: false,
            total: 0,
            done: 0,
            failed: Vec::new(),
            error: None,
            calibrated: false,
            started_at: None,
            finished_at: None,
        }
    }
}

impl PanicRunState {
    /// Reset for a fresh run over `total` tickers.
    pub fn begin(&mut self, day: &str, simulated: bool, total: usize) {
        *self = Self {
            phase: Phase::Collecting,
            running: true,
            day: day.to_string(),
            simulated,
            total,
            started_at: Some(crate::time::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()),
            ..Default::default()
        };
    }

    pub fn set_phase(&mut self, phase: Phase) {
        self.phase = phase;
    }

    pub fn tick(&mut self) {
        self.done = self.done.saturating_add(1);
    }

    pub fn fail(&mut self, symbol: &str) {
        self.failed.push(symbol.to_string());
    }

    /// Terminal success. Keeps `total` / `failed` so the UI can report partial runs.
    pub fn finish(&mut self) {
        self.phase = Phase::Done;
        self.running = false;
        self.finished_at = Some(crate::time::now().format("%Y-%m-%dT%H:%M:%SZ").to_string());
    }

    /// Terminal failure of the run as a whole.
    pub fn abort(&mut self, error: impl Into<String>) {
        self.phase = Phase::Error;
        self.running = false;
        self.error = Some(error.into());
        self.finished_at = Some(crate::time::now().format("%Y-%m-%dT%H:%M:%SZ").to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_run_clears_the_previous_outcome() {
        let mut s = PanicRunState::default();
        s.abort("pas de clé Deepseek");
        assert_eq!(s.phase, Phase::Error);
        assert!(s.error.is_some());

        s.begin("2026-08-14", false, 12);
        assert_eq!(s.phase, Phase::Collecting);
        assert!(s.running);
        assert_eq!(s.total, 12);
        assert_eq!(s.done, 0);
        assert!(
            s.error.is_none(),
            "a stale error must not survive into a new run",
        );
        assert!(s.failed.is_empty());
        assert!(s.finished_at.is_none());
    }

    /// A partial run is still a run: per-ticker failures are reported alongside a
    /// Done phase rather than discarding the tickers that did score.
    #[test]
    fn per_ticker_failures_do_not_abort_the_run() {
        let mut s = PanicRunState::default();
        s.begin("2026-08-14", true, 3);
        s.set_phase(Phase::Scoring);
        s.tick();
        s.fail("BADD");
        s.tick();
        s.tick();
        s.finish();

        assert_eq!(s.phase, Phase::Done);
        assert!(!s.running);
        assert_eq!(s.done, 3);
        assert_eq!(s.failed, vec!["BADD"]);
        assert!(s.error.is_none());
        assert!(s.simulated, "the replay flag survives the run");
        assert!(s.finished_at.is_some());
    }
}
