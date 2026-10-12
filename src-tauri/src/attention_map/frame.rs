// The frame: which two extended sessions the map is drawing, and the 5-minute
// grid they are cut into.
//
//   slots  0..48   after-hours of the previous session, from its regular close
//   slots 48..114  premarket of the target day, 04:00 → 09:30 ET
//
// The frame rolls at the regular close: from 16:00 on, the session that just ended
// becomes "the previous session" and its after-hours is the live part. During the
// regular session the frame stands still, premarket complete.
//
// Every boundary is an ET wall-clock time resolved through `crate::time`, each on
// its own day, so a frame that straddles a daylight-saving weekend keeps both
// sessions at the right UTC instants.

use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use super::calendar;

pub const SLOT_SECS: i64 = 300;
pub const N_AH: usize = 48; // 4 h after the close
pub const N_PM: usize = 66; // 04:00 → 09:30
pub const N_SLOTS: usize = N_AH + N_PM;

/// Which half of the frame a slot belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Part {
    Ah,
    Pm,
}

impl Part {
    pub fn as_str(self) -> &'static str {
        match self {
            Part::Ah => "ah",
            Part::Pm => "pm",
        }
    }

    pub fn of_slot(i: usize) -> Part {
        if i < N_AH {
            Part::Ah
        } else {
            Part::Pm
        }
    }

    pub fn len(self) -> usize {
        match self {
            Part::Ah => N_AH,
            Part::Pm => N_PM,
        }
    }

    /// First slot index of the part on the frame grid.
    pub fn offset(self) -> usize {
        match self {
            Part::Ah => 0,
            Part::Pm => N_AH,
        }
    }
}

/// One traded 5-minute slot of one symbol. A slot that did not trade has no entry:
/// inside the covered range, absence IS the information (nothing printed).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Slot {
    /// Index on the frame grid (0..N_SLOTS).
    pub i: u8,
    pub o: f32,
    pub h: f32,
    pub l: f32,
    pub c: f32,
    /// Shares.
    pub v: u32,
    /// Dollars (bar VWAP × volume), capped per trade so a negotiated block does
    /// not read as a crowd — see `fetch::BLOCK_TRADE_CAP`.
    pub d: f32,
    /// Trades in the bar.
    pub n: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Frame {
    /// Session whose after-hours is drawn.
    pub prev_day: NaiveDate,
    /// Session whose premarket is drawn.
    pub day: NaiveDate,
    pub ah_start: DateTime<Utc>,
    pub pm_start: DateTime<Utc>,
}

/// `hh:mm` ET on calendar day `d`, as a UTC instant (DST-aware).
pub fn et_instant(d: NaiveDate, minute_of_day: u32) -> DateTime<Utc> {
    let noon = crate::replay::data::noon_utc(d);
    crate::time::et_clock_utc(noon, minute_of_day / 60, minute_of_day % 60)
}

impl Frame {
    /// The frame whose premarket is `day`'s.
    pub fn for_day(day: NaiveDate) -> Frame {
        Frame::between(calendar::prev_trading_day(day), day)
    }

    pub fn between(prev_day: NaiveDate, day: NaiveDate) -> Frame {
        Frame {
            prev_day,
            day,
            ah_start: et_instant(prev_day, calendar::close_minute(prev_day)),
            pm_start: et_instant(day, 4 * 60),
        }
    }

    /// The frame in force at `now`.
    pub fn at(now: DateTime<Utc>) -> Frame {
        let today = crate::time::to_et(now).date_naive();
        if calendar::is_trading_day(today) {
            if crate::time::et_minutes(now) >= calendar::close_minute(today) {
                Frame::between(today, calendar::next_trading_day(today))
            } else {
                Frame::for_day(today)
            }
        } else {
            Frame::for_day(calendar::next_trading_day(today))
        }
    }

    /// The same target day with the previous session stepped back once — used when
    /// the computed one turns out to have no bars at all (unscheduled closure).
    pub fn step_prev_back(&self) -> Frame {
        Frame::between(calendar::prev_trading_day(self.prev_day), self.day)
    }

    pub fn part_start(&self, part: Part) -> DateTime<Utc> {
        match part {
            Part::Ah => self.ah_start,
            Part::Pm => self.pm_start,
        }
    }

    pub fn part_end(&self, part: Part) -> DateTime<Utc> {
        self.part_start(part) + Duration::seconds(SLOT_SECS * part.len() as i64)
    }

    pub fn slot_start(&self, i: usize) -> DateTime<Utc> {
        let part = Part::of_slot(i);
        self.part_start(part) + Duration::seconds(SLOT_SECS * (i - part.offset()) as i64)
    }

    pub fn slot_end(&self, i: usize) -> DateTime<Utc> {
        self.slot_start(i) + Duration::seconds(SLOT_SECS)
    }

    /// Slot holding instant `t`, when `t` falls inside one of the two sessions.
    pub fn slot_of(&self, t: DateTime<Utc>) -> Option<usize> {
        for part in [Part::Ah, Part::Pm] {
            let start = self.part_start(part);
            if t >= start && t < self.part_end(part) {
                return Some(part.offset() + ((t - start).num_seconds() / SLOT_SECS) as usize);
            }
        }
        None
    }

    /// Slots entirely elapsed at `t` — the closed part of the grid. Returns the
    /// index one past the last closed slot (0 = nothing closed yet).
    pub fn closed_until(&self, t: DateTime<Utc>) -> usize {
        let mut n = 0;
        for part in [Part::Ah, Part::Pm] {
            let start = self.part_start(part);
            if t <= start {
                break;
            }
            let done = ((t - start).num_seconds() / SLOT_SECS).clamp(0, part.len() as i64);
            n = part.offset() + done as usize;
        }
        n
    }

    pub fn early_close(&self) -> bool {
        calendar::is_early_close(self.prev_day)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }
    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn premarket_frame_points_at_the_previous_session() {
        // Monday 2026-10-12, 07:00 ET.
        let f = Frame::at(utc("2026-10-12T11:00:00Z"));
        assert_eq!((f.prev_day, f.day), (d(2026, 10, 9), d(2026, 10, 12)));
        assert_eq!(f.ah_start, utc("2026-10-09T20:00:00Z"));
        assert_eq!(f.pm_start, utc("2026-10-12T08:00:00Z"));
    }

    #[test]
    fn frame_rolls_at_the_close_and_holds_overnight_and_weekends() {
        // 15:59 ET: still the morning's frame. 16:00 ET: today becomes "previous".
        assert_eq!(Frame::at(utc("2026-10-12T19:59:00Z")).day, d(2026, 10, 12));
        let rolled = Frame::at(utc("2026-10-12T20:00:00Z"));
        assert_eq!((rolled.prev_day, rolled.day), (d(2026, 10, 12), d(2026, 10, 13)));
        // 01:00 ET the next morning and all of Saturday: unchanged targets.
        assert_eq!(Frame::at(utc("2026-10-13T05:00:00Z")), rolled);
        let weekend = Frame::at(utc("2026-10-10T15:00:00Z"));
        assert_eq!((weekend.prev_day, weekend.day), (d(2026, 10, 9), d(2026, 10, 12)));
    }

    #[test]
    fn slots_follow_the_clock_change() {
        // DST ends Sunday 2026-11-01: Friday's close is 20:00Z (EDT), Monday's 04:00
        // is 09:00Z (EST). Both must land on their ET wall times.
        let f = Frame::for_day(d(2026, 11, 2));
        assert_eq!(f.ah_start, utc("2026-10-30T20:00:00Z"));
        assert_eq!(f.pm_start, utc("2026-11-02T09:00:00Z"));
        assert_eq!(f.slot_of(utc("2026-11-02T09:07:00Z")), Some(N_AH + 1));
        assert_eq!(f.slot_start(N_AH + 65), utc("2026-11-02T14:25:00Z"));
    }

    #[test]
    fn overnight_and_regular_hours_belong_to_no_slot() {
        let f = Frame::for_day(d(2026, 10, 12));
        assert_eq!(f.slot_of(utc("2026-10-10T00:00:00Z")), None); // Fri 20:00 ET
        assert_eq!(f.slot_of(utc("2026-10-12T07:59:59Z")), None); // 03:59 ET
        assert_eq!(f.slot_of(utc("2026-10-12T13:30:00Z")), None); // 09:30 ET
        assert_eq!(f.slot_of(utc("2026-10-09T20:04:59Z")), Some(0));
        assert_eq!(f.slot_of(utc("2026-10-09T23:59:59Z")), Some(47));
    }

    #[test]
    fn closed_until_counts_only_elapsed_slots() {
        let f = Frame::for_day(d(2026, 10, 12));
        assert_eq!(f.closed_until(utc("2026-10-09T20:00:00Z")), 0);
        assert_eq!(f.closed_until(utc("2026-10-09T20:04:59Z")), 0);
        assert_eq!(f.closed_until(utc("2026-10-09T20:05:00Z")), 1);
        assert_eq!(f.closed_until(utc("2026-10-11T12:00:00Z")), N_AH); // Sunday
        assert_eq!(f.closed_until(utc("2026-10-12T08:12:00Z")), N_AH + 2);
        assert_eq!(f.closed_until(utc("2026-10-12T18:00:00Z")), N_SLOTS);
    }

    #[test]
    fn early_close_moves_the_after_hours() {
        // Friday after Thanksgiving closes at 13:00 ET (18:00Z in EST).
        let f = Frame::for_day(d(2026, 11, 30));
        assert_eq!(f.prev_day, d(2026, 11, 27));
        assert!(f.early_close());
        assert_eq!(f.ah_start, utc("2026-11-27T18:00:00Z"));
    }
}
