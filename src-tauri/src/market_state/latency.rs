use chrono::{DateTime, Utc};

use crate::types::{LatencyLevel, LatencyStatus};

/// Compute a LatencyStatus from an event timestamp vs. the current wall clock.
/// Thresholds: <300ms normal · 300-warn_ms warning · warn_ms-critical_ms slow · ≥critical_ms critical.
pub fn compute(
    event_time: DateTime<Utc>,
    now: DateTime<Utc>,
    warn_ms: u32,
    critical_ms: u32,
) -> LatencyStatus {
    let diff_ms = (now - event_time)
        .num_milliseconds()
        .clamp(0, u32::MAX as i64) as u32;

    let level = if diff_ms >= critical_ms {
        LatencyLevel::Critical
    } else if diff_ms >= warn_ms {
        LatencyLevel::Slow
    } else if diff_ms >= 300 {
        LatencyLevel::Warning
    } else {
        LatencyLevel::Normal
    };

    LatencyStatus {
        websocket_to_ui_ms: diff_ms,
        level,
        measured_at: now,
    }
}
