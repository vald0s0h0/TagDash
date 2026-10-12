// The attention map's arithmetic, all pure: medians from the archive, the snapshot
// the engine publishes and the journal stores, and the view drawn from a snapshot
// AS OF an instant.
//
// One code path serves the live screen, a Market Replay and « Revue »: the
// snapshot holds what is known about a frame, and `build_view` cuts it at the slot
// the clock has reached. A replay or a review can therefore never show a slot from
// its own future, and the live screen is simply the same cut at the data horizon.
//
// WHAT COUNTS. A slot under `min_dollars_5m / 5` is drawn but never sets a price
// extreme or a pattern: a 40-share print at an off price is not a move. A slot
// under `min_dollars_5m` never sets an activity peak. The first after-hours slot is
// excluded from peaks too — it carries the closing auction, which is settlement,
// not attention.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::frame::{Frame, Part, Slot, N_AH, N_SLOTS, SLOT_SECS};
use super::options::AttentionOptions;
use super::store::ArchivePart;

/// Sessions of history below which a ratio is not shown as a comparison.
pub const MIN_HISTORY: u16 = 5;
pub const TARGET_HISTORY: usize = 60;
/// Tickers sent to the screen at most (it draws traces for far fewer).
const MAX_TICKERS: usize = 400;
const X_MAX: f32 = 100_000.0;

// ─── Universe ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Group {
    /// Common stock / ADR under the small-cap ceiling.
    Small,
    /// Common stock / ADR at or above it.
    Big,
    /// Common stock / ADR whose market cap was never collected.
    Unknown,
    /// ETF, ETN, exchange-traded vehicle, closed-end fund — never mixed with stocks.
    Etf,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Meta {
    pub name: Option<String>,
    pub group: Group,
    pub cap: Option<f64>,
    /// None when unknown — a float of zero in the table is "never collected".
    pub float: Option<f64>,
}

/// Group of a universe row, or None for what the map does not track at all
/// (warrants, units, rights, preferreds, structured products).
pub fn group_of(security_type: Option<&str>, cap: Option<f64>, small_cap_max: f64) -> Option<Group> {
    match security_type.map(|s| s.trim().to_ascii_uppercase()).as_deref() {
        Some("ETF" | "ETN" | "ETV" | "ETS" | "FUND") => Some(Group::Etf),
        Some("CS" | "ADRC") | None | Some("") => Some(match cap.filter(|c| *c > 0.0) {
            Some(c) if c < small_cap_max => Group::Small,
            Some(_) => Group::Big,
            None => Group::Unknown,
        }),
        Some(_) => None,
    }
}

// ─── Baselines ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct WeatherSlot {
    /// Dollars traded by small caps in the slot.
    pub small: f32,
    /// Dollars traded by everything tracked (stocks and funds).
    pub all: f32,
    /// Stocks that traded at least `min_dollars_5m` in the slot.
    pub active: u16,
    /// Share of the stocks' dollars taken by the five busiest.
    pub top5: f32,
}

#[derive(Debug, Clone, Default)]
pub struct Baselines {
    /// Symbol → expected dollars per frame slot: the slot's own median, never
    /// under the name's usual per-slot pace over that session. Symbols absent here
    /// expect nothing anywhere (the absolute floor then stands in).
    pub med: HashMap<String, Vec<f32>>,
    /// Sessions behind the medians, per part [ah, pm].
    pub sessions: [u16; 2],
    /// Median weather per frame slot over the same sessions.
    pub weather: Vec<WeatherSlot>,
}

/// Median of `n` values of which `vals` are the non-zero ones (the rest are zero):
/// a session where the name did not trade the slot counts as a zero, because
/// dormancy at that hour is exactly what the comparison is about.
fn median_zero_filled(vals: &mut [f32], n: usize) -> f32 {
    let n = n.max(vals.len());
    if n == 0 {
        return 0.0;
    }
    vals.sort_by(f32::total_cmp);
    let zeros = n - vals.len();
    let at = |j: usize| if j < zeros { 0.0 } else { vals[j - zeros] };
    if n % 2 == 1 {
        at(n / 2)
    } else {
        (at(n / 2 - 1) + at(n / 2)) / 2.0
    }
}

/// Per-slot weather of one traded part.
fn weather_of(
    rows: impl Iterator<Item = (Group, u8, f32)>,
    len: usize,
    min_dollars: f32,
) -> Vec<WeatherSlot> {
    let mut out = vec![WeatherSlot::default(); len];
    let mut stock = vec![0.0f32; len];
    let mut top: Vec<[f32; 5]> = vec![[0.0; 5]; len];
    for (group, slot, d) in rows {
        let s = slot as usize;
        if s >= len || !(d > 0.0) {
            continue;
        }
        out[s].all += d;
        if group == Group::Etf {
            continue;
        }
        stock[s] += d;
        if group == Group::Small {
            out[s].small += d;
        }
        if d >= min_dollars {
            out[s].active = out[s].active.saturating_add(1);
        }
        // Keep the five largest, smallest first.
        if d > top[s][0] {
            top[s][0] = d;
            top[s].sort_by(f32::total_cmp);
        }
    }
    for s in 0..len {
        if stock[s] > 0.0 {
            out[s].top5 = top[s].iter().sum::<f32>() / stock[s];
        }
    }
    out
}

/// Weather of the frame itself, on the frame grid.
pub fn frame_weather(
    series: &HashMap<String, Vec<Slot>>,
    metas: &HashMap<String, Meta>,
    opts: &AttentionOptions,
) -> Vec<WeatherSlot> {
    let rows = series.iter().flat_map(|(sym, slots)| {
        let group = metas.get(sym).map(|m| m.group);
        slots
            .iter()
            .filter_map(move |s| group.map(|g| (g, s.i, s.d)))
    });
    weather_of(rows, N_SLOTS, opts.min_dollars_5m as f32)
}

/// Medians of one part over its archived sessions. `listed(symbol)` = in how many
/// of those sessions the symbol existed (None = assume all of them).
fn part_baseline(
    part: Part,
    sessions: &[ArchivePart],
    listed: &dyn Fn(&str) -> Option<u16>,
    metas: &HashMap<String, Meta>,
    opts: &AttentionOptions,
    med: &mut HashMap<String, Vec<f32>>,
    weather: &mut [WeatherSlot],
) {
    let len = part.len();
    let n_sessions = sessions.len();
    if n_sessions == 0 {
        return;
    }
    // Per symbol, every (slot, dollars) seen across the sessions, and what each
    // session amounted to in total.
    let mut per_symbol: HashMap<&str, Vec<(u8, f32)>> = HashMap::new();
    let mut totals: HashMap<&str, Vec<f32>> = HashMap::new();
    let mut per_session_weather: Vec<Vec<WeatherSlot>> = Vec::with_capacity(n_sessions);
    for session in sessions {
        for (sym, rows) in session {
            per_symbol.entry(sym.as_str()).or_default().extend(rows.iter().copied());
            totals
                .entry(sym.as_str())
                .or_default()
                .push(rows.iter().map(|r| r.1).sum());
        }
        let rows = session.iter().flat_map(|(sym, rows)| {
            let group = metas.get(sym).map(|m| m.group);
            rows.iter().filter_map(move |(slot, d)| group.map(|g| (g, *slot, *d)))
        });
        per_session_weather.push(weather_of(rows, len, opts.min_dollars_5m as f32));
    }

    let mut buf: Vec<f32> = Vec::with_capacity(n_sessions);
    for (sym, mut rows) in per_symbol {
        let n = listed(sym).map(|l| l as usize).unwrap_or(n_sessions).min(n_sessions);
        rows.sort_by_key(|r| r.0);
        // The name's usual pace over this session, per slot. A large cap trades
        // its after-hours in scattered slots: most have a zero median, and against
        // zero any 20 k$ would read as a surge. It is compared with its own
        // ordinary evening instead (measured before this floor: MELI, PDD, VSH on
        // the map every night, price flat).
        let typical = totals
            .get_mut(sym)
            .map(|t| median_zero_filled(t, n) / len as f32)
            .unwrap_or(0.0);
        let mut any = typical > 0.0;
        let mut out = vec![typical; len];
        let mut i = 0;
        while i < rows.len() {
            let slot = rows[i].0;
            buf.clear();
            while i < rows.len() && rows[i].0 == slot {
                buf.push(rows[i].1);
                i += 1;
            }
            if (slot as usize) < len {
                let m = median_zero_filled(&mut buf, n);
                if m > typical {
                    out[slot as usize] = m;
                    any = true;
                }
            }
        }
        if any {
            let full = med.entry(sym.to_string()).or_insert_with(|| vec![0.0; N_SLOTS]);
            full[part.offset()..part.offset() + len].copy_from_slice(&out);
        }
    }

    for s in 0..len {
        let pick = |f: &dyn Fn(&WeatherSlot) -> f32| {
            let mut v: Vec<f32> = per_session_weather.iter().map(|w| f(&w[s])).collect();
            median_zero_filled(&mut v, n_sessions)
        };
        weather[part.offset() + s] = WeatherSlot {
            small: pick(&|w| w.small),
            all: pick(&|w| w.all),
            active: pick(&|w| w.active as f32).round() as u16,
            top5: pick(&|w| w.top5),
        };
    }
}

pub fn build_baselines(
    ah: &[ArchivePart],
    pm: &[ArchivePart],
    listed_ah: &dyn Fn(&str) -> Option<u16>,
    listed_pm: &dyn Fn(&str) -> Option<u16>,
    metas: &HashMap<String, Meta>,
    opts: &AttentionOptions,
) -> Baselines {
    let mut med = HashMap::new();
    let mut weather = vec![WeatherSlot::default(); N_SLOTS];
    part_baseline(Part::Ah, ah, listed_ah, metas, opts, &mut med, &mut weather);
    part_baseline(Part::Pm, pm, listed_pm, metas, opts, &mut med, &mut weather);
    Baselines {
        med,
        sessions: [ah.len() as u16, pm.len() as u16],
        weather,
    }
}

// ─── Snapshot ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ticker {
    pub s: String,
    pub meta: Meta,
    /// Raw regular close of the previous session.
    pub ref_close: f64,
    /// Split factor new/old effective on the target day (1.0 = no split). The
    /// after-hours traded before it, the premarket after.
    pub factor: f64,
    pub split: Option<String>,
    /// Raw slots, ascending.
    pub slots: Vec<Slot>,
    /// Expected dollars of each slot of `slots`, same order; empty = none known.
    pub base: Vec<f32>,
    /// Sessions of history the symbol existed in, per part [ah, pm].
    pub base_n: [u16; 2],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub frame: Frame,
    /// Data read up to these instants (epoch seconds, 0 = nothing).
    pub ah_seen_s: i64,
    pub pm_seen_s: i64,
    /// Real clock of the last read — tells a closed slot from one still filling.
    pub fetched_s: i64,
    /// Provider delay, minutes (0 on the SIP plan).
    pub delayed_min: u32,
    /// Date the floats were collected.
    pub float_date: Option<String>,
    pub tickers: Vec<Ticker>,
    /// Per-slot weather of the frame and its historical median.
    pub weather: Vec<WeatherSlot>,
    pub weather_hist: Vec<WeatherSlot>,
    pub hist_sessions: [u16; 2],
    /// Symbols tracked / of which without a reference close.
    pub universe: u32,
    pub no_ref: u32,
}

impl Snapshot {
    /// Instant the data reaches, epoch seconds.
    pub fn seen_s(&self) -> i64 {
        let ah_end = self.frame.part_end(Part::Ah).timestamp();
        if self.ah_seen_s < ah_end.min(self.fetched_s) || self.pm_seen_s == 0 {
            self.ah_seen_s
        } else {
            self.pm_seen_s
        }
    }

    pub fn seen_ms(&self) -> i64 {
        self.seen_s() * 1000
    }

    /// The cut the live screen draws: every slot the provider has finished, plus
    /// the one still filling on a real-time plan.
    pub fn live_cut(&self) -> Cut {
        self.cut_at(self.seen_s(), true)
    }

    /// The cut at market instant `at_s` — never past what was read, and without
    /// the filling slot unless `partial` (a replay or a review shows closed slots
    /// only, which is what keeps their future out).
    pub fn cut_at(&self, at_s: i64, partial: bool) -> Cut {
        let seen = self.seen_s();
        let at = at_s.min(seen);
        let Some(t) = chrono::DateTime::from_timestamp(at, 0) else {
            return Cut::default();
        };
        let mut closed = self.frame.closed_until(t);
        let mut filling = None;
        if let Some(i) = self.frame.slot_of(t) {
            let end = self.frame.slot_end(i).timestamp();
            if at_s >= seen && end <= self.fetched_s - 30 {
                // The provider answered after the slot ended: it is complete.
                closed = closed.max(i + 1);
            } else if partial {
                let start = self.frame.slot_start(i).timestamp();
                filling = Some((i, ((at - start) as f32 / SLOT_SECS as f32).clamp(0.0, 1.0)));
            }
        }
        Cut { closed, filling }
    }
}

/// Where a view stops on the frame grid.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Cut {
    /// Slots `< closed` are complete.
    pub closed: usize,
    /// The slot still filling and the fraction of it elapsed.
    pub filling: Option<(usize, f32)>,
}

impl Cut {
    pub fn all() -> Cut {
        Cut { closed: N_SLOTS, filling: None }
    }

    fn shows(&self, i: usize) -> bool {
        i < self.closed || self.filling.is_some_and(|(f, _)| f == i)
    }

    /// Last slot on screen, for the cache key of a view.
    pub fn head(&self) -> usize {
        self.filling.map(|(i, _)| i + 1).unwrap_or(self.closed)
    }
}

// ─── Patterns ───────────────────────────────────────────────────────────────────

/// What the path looks like. Descriptive only — none of these says why.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pattern {
    /// Rose in a burst and is holding it.
    Held,
    /// Rose step after step.
    Climb,
    /// Rose, then gave at least half of it back.
    SpikeFade,
    /// Faded, then pushed again.
    Relaunch,
    /// Fell and is staying down.
    Decline,
    Neutral,
}

/// Classify a path of changes since the reference close (percent), chronological.
/// `thr` is the smallest move worth a colour.
pub fn classify(ys: &[f32], thr: f32) -> Pattern {
    let Some(&last) = ys.last() else {
        return Pattern::Neutral;
    };
    let (mut y_max, mut y_min, mut i_max) = (f32::MIN, f32::MAX, 0usize);
    for (i, &y) in ys.iter().enumerate() {
        if y > y_max {
            y_max = y;
            i_max = i;
        }
        y_min = y_min.min(y);
    }
    if y_max < thr && y_min > -thr {
        return Pattern::Neutral;
    }
    // The fall is the larger of the two moves.
    if -y_min > y_max {
        return if last <= 0.5 * y_min {
            Pattern::Decline
        } else {
            Pattern::Neutral
        };
    }

    // Deepest give-back from a running high, and what came after it.
    let (mut run_max, mut peak_at_trough, mut trough, mut trough_i) = (f32::MIN, 0.0f32, 0.0f32, None);
    let mut deepest = 0.0f32;
    for (i, &y) in ys.iter().enumerate() {
        run_max = run_max.max(y);
        let dd = run_max - y;
        if run_max >= thr && dd > deepest {
            deepest = dd;
            peak_at_trough = run_max;
            trough = y;
            trough_i = Some(i);
        }
    }
    if let Some(ti) = trough_i {
        let gave_back = deepest / peak_at_trough.max(thr);
        let rebound = (last - trough) / deepest.max(f32::EPSILON);
        if gave_back >= 0.5 && ti + 1 < ys.len() && rebound >= 0.5 && last >= thr {
            return Pattern::Relaunch;
        }
    }
    if (y_max - last) / y_max.max(thr) >= 0.5 {
        return Pattern::SpikeFade;
    }

    // Still up: one burst, or a staircase? Share of the rise made by its two
    // largest steps (the first step is the gap from the reference).
    let mut steps: Vec<f32> = Vec::with_capacity(i_max + 1);
    let mut prev = 0.0f32;
    for &y in &ys[..=i_max] {
        steps.push((y - prev).max(0.0));
        prev = y;
    }
    let total: f32 = steps.iter().sum();
    steps.sort_by(|a, b| b.total_cmp(a));
    let burst = steps.iter().take(2).sum::<f32>() / total.max(f32::EPSILON);
    if i_max >= 3 && burst < 0.6 {
        Pattern::Climb
    } else {
        Pattern::Held
    }
}

// ─── View ───────────────────────────────────────────────────────────────────────

/// One point of a path: `[slot, x, y, $/min, flags]`.
/// flags: 1 = counts for price (solid), 2 = still filling.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Pt(pub u8, pub f32, pub f32, pub f32, pub u8);

/// What one session of a ticker amounts to, as cut.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct PartStats {
    /// Slots drawn.
    pub n: u16,
    /// Shares and dollars, cumulative (shares on the target day's split basis).
    pub vol: f64,
    pub dollars: f64,
    /// Price range of the slots that count; None when none did.
    pub hi: Option<f32>,
    pub lo: Option<f32>,
    /// Peak relative activity and its slot.
    pub x_peak: Option<f32>,
    pub x_peak_i: Option<u8>,
    pub y_max: Option<f32>,
    pub y_max_i: Option<u8>,
    pub y_min: Option<f32>,
    pub y_min_i: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TickerView {
    pub s: String,
    pub name: Option<String>,
    pub group: Group,
    pub cap: Option<f64>,
    pub float: Option<f64>,
    /// Reference close on the target day's split basis.
    pub ref_close: f64,
    pub split: Option<String>,
    /// Sessions behind the medians [ah, pm].
    pub base_n: [u16; 2],
    pub pts: Vec<Pt>,
    pub ah: PartStats,
    pub pm: PartStats,
    /// Pattern and rank with the after-hours shown, and with the premarket alone.
    pub pat_all: Pattern,
    pub pat_pm: Pattern,
    pub score_all: f32,
    pub score_pm: f32,
    /// Latest slot: price, and dollars against the slot just before it (None when
    /// that slot did not trade — a ratio to nothing is not an acceleration).
    pub last: f32,
    pub accel: Option<f32>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct Gauge {
    pub value: f64,
    /// Historical median at the same slot; None with too little history.
    pub median: Option<f64>,
    pub ratio: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Weather {
    /// Slot the gauges describe (the last closed one).
    pub slot: Option<u8>,
    /// Sessions of history behind the medians at that slot.
    pub sessions: u16,
    pub small: Gauge,
    pub global: Gauge,
    pub breadth: Gauge,
    pub concentration: Gauge,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FrameInfo {
    pub day: String,
    pub prev_day: String,
    /// Session bounds, epoch seconds.
    pub ah_start_s: i64,
    pub pm_start_s: i64,
    pub n_ah: usize,
    pub n_pm: usize,
    pub slot_secs: i64,
    pub early_close: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MapView {
    pub frame: FrameInfo,
    /// Slots `< closed` are complete; `filling` is the one in progress, if any.
    pub closed: usize,
    pub filling: Option<u8>,
    /// Instant the data reaches (epoch seconds) and the provider's delay.
    pub seen_s: i64,
    pub delayed_min: u32,
    pub float_date: Option<String>,
    pub hist_sessions: [u16; 2],
    pub hist_target: usize,
    pub tickers: Vec<TickerView>,
    pub weather: Weather,
    pub universe: u32,
    pub no_ref: u32,
}

fn score(x_peak: Option<f32>, y_abs: f32) -> f32 {
    (1.0 + x_peak.unwrap_or(0.0)).ln() * (1.0 + y_abs / 10.0)
}

/// Evaluate one ticker as cut. None when it does not belong on the map.
pub fn eval_ticker(t: &Ticker, cut: &Cut, opts: &AttentionOptions) -> Option<TickerView> {
    if !(t.ref_close > 0.0) || !(t.factor > 0.0) {
        return None;
    }
    let floor = opts.baseline_floor as f32;
    let solid_min = (opts.min_dollars_5m / 5.0) as f32;
    let peak_min = opts.min_dollars_5m as f32;
    let factor = t.factor as f32;
    // On the target day's basis: what traded before the split is rescaled.
    let ref_adj = (t.ref_close / t.factor) as f32;

    let mut pts: Vec<Pt> = Vec::with_capacity(t.slots.len());
    let mut stats = [PartStats::default(), PartStats::default()];
    let mut ys: [Vec<f32>; 2] = [Vec::new(), Vec::new()];
    let mut prev: Option<&Slot> = None;
    let (mut last, mut accel) = (0.0f32, None);

    for (k, s) in t.slots.iter().enumerate() {
        let i = s.i as usize;
        if i >= N_SLOTS || !cut.shows(i) {
            continue;
        }
        let part = Part::of_slot(i);
        let pi = part as usize;
        let (pf, vf) = if part == Part::Ah { (1.0 / factor, factor) } else { (1.0, 1.0) };
        let before = prev.filter(|p| p.i + 1 == s.i && Part::of_slot(p.i as usize) == part);

        // A slot still filling is compared on a trailing five minutes: what it has
        // so far plus the matching tail of the slot before it.
        let filling = cut.filling.filter(|(f, _)| *f == i);
        let dollars = match filling {
            Some((_, frac)) => s.d + before.map_or(0.0, |p| p.d * (1.0 - frac)),
            None => s.d,
        };
        let base = t.base.get(k).copied().unwrap_or(0.0);
        let x = (dollars / base.max(floor)).clamp(0.0, X_MAX);
        let close = s.c * pf;
        let y = (close / ref_adj - 1.0) * 100.0;
        let solid = s.d >= solid_min;

        let st = &mut stats[pi];
        st.n += 1;
        st.vol += s.v as f64 * vf as f64;
        st.dollars += s.d as f64;
        if solid {
            let (hi, lo) = (s.h * pf, s.l * pf);
            st.hi = Some(st.hi.map_or(hi, |v| v.max(hi)));
            st.lo = Some(st.lo.map_or(lo, |v| v.min(lo)));
            if st.y_max.is_none_or(|v| y > v) {
                st.y_max = Some(y);
                st.y_max_i = Some(s.i);
            }
            if st.y_min.is_none_or(|v| y < v) {
                st.y_min = Some(y);
                st.y_min_i = Some(s.i);
            }
            ys[pi].push(y);
        }
        // The closing auction prints in the first after-hours slot: money, but not
        // attention. It never sets a peak.
        if s.d >= peak_min && i != 0 && filling.is_none() && st.x_peak.is_none_or(|v| x > v) {
            st.x_peak = Some(x);
            st.x_peak_i = Some(s.i);
        }

        last = close;
        accel = before.filter(|p| p.d > 0.0).map(|p| dollars / p.d);
        pts.push(Pt(
            s.i,
            x,
            y,
            dollars / 5.0,
            (solid as u8) | ((filling.is_some() as u8) << 1),
        ));
        prev = Some(s);
    }
    if pts.is_empty() {
        return None;
    }

    let thr = (opts.min_move_pct * 0.6).max(2.0) as f32;
    let [ah, pm] = stats;
    let y_abs = |st: &PartStats| st.y_max.unwrap_or(0.0).abs().max(st.y_min.unwrap_or(0.0).abs());
    let qualifies = |x_peak: Option<f32>, y: f32, dollars: f64| {
        x_peak.is_some_and(|x| x as f64 >= opts.min_rel)
            || (y as f64 >= opts.min_move_pct && dollars >= 2.0 * opts.min_dollars_5m)
    };
    let x_all = match (ah.x_peak, pm.x_peak) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    };
    let y_all = y_abs(&ah).max(y_abs(&pm));
    let q_all = qualifies(x_all, y_all, ah.dollars + pm.dollars);
    let q_pm = qualifies(pm.x_peak, y_abs(&pm), pm.dollars);
    if !q_all && !q_pm {
        return None;
    }

    let all_ys: Vec<f32> = ys[0].iter().chain(ys[1].iter()).copied().collect();
    Some(TickerView {
        s: t.s.clone(),
        name: t.meta.name.clone(),
        group: t.meta.group,
        cap: t.meta.cap,
        float: t.meta.float,
        ref_close: ref_adj as f64,
        split: t.split.clone(),
        base_n: t.base_n,
        pts,
        ah,
        pm,
        pat_all: classify(&all_ys, thr),
        pat_pm: classify(&ys[1], thr),
        score_all: if q_all { score(x_all, y_all) } else { 0.0 },
        score_pm: if q_pm { score(pm.x_peak, y_abs(&pm)) } else { 0.0 },
        last,
        accel,
    })
}

fn gauge(value: f64, median: f64, enough: bool) -> Gauge {
    let median = (enough && median > 0.0).then_some(median);
    Gauge {
        value,
        median,
        ratio: median.map(|m| value / m),
    }
}

fn weather_at(snap: &Snapshot, cut: &Cut) -> Weather {
    let Some(i) = cut.closed.checked_sub(1).filter(|i| *i < snap.weather.len()) else {
        return Weather::default();
    };
    let now = snap.weather[i];
    let hist = snap.weather_hist.get(i).copied().unwrap_or_default();
    let sessions = snap.hist_sessions[Part::of_slot(i) as usize];
    let enough = sessions >= MIN_HISTORY;
    Weather {
        slot: Some(i as u8),
        sessions,
        small: gauge(now.small as f64, hist.small as f64, enough),
        global: gauge(now.all as f64, hist.all as f64, enough),
        breadth: gauge(now.active as f64, hist.active as f64, enough),
        concentration: gauge(now.top5 as f64, hist.top5 as f64, enough),
    }
}

pub fn build_view(snap: &Snapshot, cut: &Cut, opts: &AttentionOptions) -> MapView {
    let mut tickers: Vec<TickerView> = snap
        .tickers
        .iter()
        .filter_map(|t| eval_ticker(t, cut, opts))
        .collect();
    tickers.sort_by(|a, b| {
        b.score_all
            .max(b.score_pm)
            .total_cmp(&a.score_all.max(a.score_pm))
    });
    tickers.truncate(MAX_TICKERS);
    let f = &snap.frame;
    MapView {
        frame: FrameInfo {
            day: f.day.to_string(),
            prev_day: f.prev_day.to_string(),
            ah_start_s: f.ah_start.timestamp(),
            pm_start_s: f.pm_start.timestamp(),
            n_ah: N_AH,
            n_pm: N_SLOTS - N_AH,
            slot_secs: SLOT_SECS,
            early_close: f.early_close(),
        },
        closed: cut.closed,
        filling: cut.filling.map(|(i, _)| i as u8),
        seen_s: snap.seen_s(),
        delayed_min: snap.delayed_min,
        float_date: snap.float_date.clone(),
        hist_sessions: snap.hist_sessions,
        hist_target: TARGET_HISTORY,
        weather: weather_at(snap, cut),
        tickers,
        universe: snap.universe,
        no_ref: snap.no_ref,
    }
}

// ─── Detail (the preview chart) ─────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Candle {
    /// Slot start, epoch seconds.
    pub t: i64,
    pub i: u8,
    pub o: f32,
    pub h: f32,
    pub l: f32,
    pub c: f32,
    pub v: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Detail {
    pub s: String,
    pub ref_close: f64,
    pub candles: Vec<Candle>,
}

/// The 5-minute candles of one ticker as cut, on the target day's split basis.
pub fn build_detail(snap: &Snapshot, symbol: &str, cut: &Cut) -> Option<Detail> {
    let t = snap.tickers.iter().find(|t| t.s == symbol)?;
    let factor = t.factor as f32;
    let candles = t
        .slots
        .iter()
        .filter(|s| (s.i as usize) < N_SLOTS && cut.shows(s.i as usize))
        .map(|s| {
            let ah = (s.i as usize) < N_AH;
            let (pf, vf) = if ah { (1.0 / factor, t.factor) } else { (1.0, 1.0) };
            Candle {
                t: snap.frame.slot_start(s.i as usize).timestamp(),
                i: s.i,
                o: s.o * pf,
                h: s.h * pf,
                l: s.l * pf,
                c: s.c * pf,
                v: s.v as f64 * vf,
            }
        })
        .collect();
    Some(Detail {
        s: t.s.clone(),
        ref_close: t.ref_close / t.factor,
        candles,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn slot(i: u8, c: f32, d: f32) -> Slot {
        Slot { i, o: c, h: c, l: c, c, v: (d / c) as u32, d, n: 10 }
    }

    fn ticker(slots: Vec<Slot>) -> Ticker {
        Ticker {
            s: "TEST".into(),
            meta: Meta { name: None, group: Group::Small, cap: Some(5e7), float: Some(1e7) },
            ref_close: 10.0,
            factor: 1.0,
            split: None,
            slots,
            base: Vec::new(),
            base_n: [60, 60],
        }
    }

    fn snapshot(tickers: Vec<Ticker>) -> Snapshot {
        let frame = Frame::for_day(NaiveDate::from_ymd_opt(2026, 10, 12).unwrap());
        Snapshot {
            frame,
            ah_seen_s: frame.part_end(Part::Ah).timestamp(),
            pm_seen_s: frame.part_end(Part::Pm).timestamp(),
            fetched_s: frame.part_end(Part::Pm).timestamp() + 3_600,
            delayed_min: 0,
            float_date: None,
            tickers,
            weather: vec![WeatherSlot::default(); N_SLOTS],
            weather_hist: vec![WeatherSlot::default(); N_SLOTS],
            hist_sessions: [60, 60],
            universe: 1,
            no_ref: 0,
        }
    }

    #[test]
    fn zero_filled_median_counts_dormant_sessions() {
        // Traded in 2 of 5 sessions → the median session is a zero.
        assert_eq!(median_zero_filled(&mut [100.0, 300.0], 5), 0.0);
        // Traded in 3 of 5 → the smallest of the three is the median.
        assert_eq!(median_zero_filled(&mut [300.0, 100.0, 200.0], 5), 100.0);
        // Even count: mean of the two middle values, one of them a zero.
        assert_eq!(median_zero_filled(&mut [100.0, 300.0], 4), 50.0);
        // More observations than "listed" sessions never underflows.
        assert_eq!(median_zero_filled(&mut [1.0, 2.0, 3.0], 1), 2.0);
        assert_eq!(median_zero_filled(&mut [], 0), 0.0);
    }

    #[test]
    fn baselines_respect_listing_age_and_parts() {
        let metas = HashMap::from([
            ("OLD".to_string(), Meta { name: None, group: Group::Small, cap: Some(1e8), float: None }),
            ("NEW".to_string(), Meta { name: None, group: Group::Big, cap: Some(5e9), float: None }),
        ]);
        // Three premarket sessions; OLD trades slot 2 in all, NEW in the last only.
        let pm: Vec<ArchivePart> = vec![
            vec![("OLD".into(), vec![(2, 100.0)])],
            vec![("OLD".into(), vec![(2, 300.0)])],
            vec![("OLD".into(), vec![(2, 200.0)]), ("NEW".into(), vec![(2, 900.0)])],
        ];
        let listed = |s: &str| Some(if s == "NEW" { 1 } else { 3 });
        let b = build_baselines(&[], &pm, &listed, &listed, &metas, &AttentionOptions::default());
        assert_eq!(b.sessions, [0, 3]);
        assert_eq!(b.med["OLD"][N_AH + 2], 200.0);
        // A slot it never traded is held to its usual pace: 200 $ a session over
        // the 66 premarket slots — not to zero.
        assert!((b.med["OLD"][N_AH + 9] - 200.0 / 66.0).abs() < 1e-4);
        assert_eq!(b.med["OLD"][3], 0.0, "no after-hours history, no after-hours floor");
        // Listed for one session only: its single print is its median, not a zero.
        assert_eq!(b.med["NEW"][N_AH + 2], 900.0);
        // Weather: small-cap dollars are OLD's; the median over the 3 sessions.
        assert_eq!(b.weather[N_AH + 2].small, 200.0);
        assert_eq!(b.weather[N_AH + 2].all, 300.0);
    }

    #[test]
    fn patterns_describe_the_path() {
        let thr = 3.0;
        assert_eq!(classify(&[0.5, -1.0, 1.2], thr), Pattern::Neutral);
        assert_eq!(classify(&[20.0, 22.0, 21.0, 23.0], thr), Pattern::Held);
        assert_eq!(classify(&[2.0, 5.0, 8.0, 11.0, 14.0, 17.0], thr), Pattern::Climb);
        assert_eq!(classify(&[10.0, 30.0, 18.0, 9.0, 6.0], thr), Pattern::SpikeFade);
        assert_eq!(classify(&[10.0, 30.0, 12.0, 8.0, 20.0, 27.0], thr), Pattern::Relaunch);
        assert_eq!(classify(&[-4.0, -9.0, -12.0, -11.0], thr), Pattern::Decline);
        // Fell, then recovered most of it: no longer a persistent decline.
        assert_eq!(classify(&[-12.0, -8.0, -2.0], thr), Pattern::Neutral);
        assert_eq!(classify(&[], thr), Pattern::Neutral);
    }

    #[test]
    fn the_floor_keeps_a_dormant_name_finite() {
        let opts = AttentionOptions::default();
        // No median at all: 100k$ against the 5k$ floor is 20×, not infinity.
        let v = eval_ticker(&ticker(vec![slot(50, 11.0, 100_000.0)]), &Cut::all(), &opts).unwrap();
        assert_eq!(v.pts.len(), 1);
        assert!((v.pts[0].1 - 20.0).abs() < 1e-3);
        assert!((v.pts[0].2 - 10.0).abs() < 1e-3);
        assert!((v.pts[0].3 - 20_000.0).abs() < 1e-2);
        assert_eq!(v.pm.x_peak_i, Some(50));
        // A lone slot has nothing before it: no acceleration, not an infinite one.
        assert_eq!(v.accel, None);
    }

    #[test]
    fn thin_prints_and_the_closing_auction_set_no_peak() {
        let opts = AttentionOptions::default();
        // Slot 0 is the closing auction: huge, and ignored for the peak.
        let auction = ticker(vec![slot(0, 10.0, 5_000_000.0)]);
        assert!(eval_ticker(&auction, &Cut::all(), &opts).is_none());
        // A 2k$ print at +60 %: drawn if the name is on the map, but it neither
        // qualifies it nor moves its extremes.
        let t = ticker(vec![slot(50, 16.0, 2_000.0), slot(52, 10.6, 400_000.0)]);
        let v = eval_ticker(&t, &Cut::all(), &opts).unwrap();
        assert_eq!(v.pts.len(), 2);
        assert_eq!(v.pts[0].4 & 1, 0);
        assert!((v.pm.y_max.unwrap() - 6.0).abs() < 1e-3);
        assert_eq!(v.pm.hi, Some(10.6));
    }

    #[test]
    fn a_cut_never_shows_its_future() {
        let opts = AttentionOptions::default();
        let t = ticker(vec![slot(49, 10.2, 8_000.0), slot(60, 14.0, 900_000.0)]);
        let snap = snapshot(vec![t]);
        // Before slot 60 closes the name has done nothing worth the map.
        let early = snap.cut_at(snap.frame.slot_start(55).timestamp(), false);
        assert_eq!(early, Cut { closed: 55, filling: None });
        assert!(build_view(&snap, &early, &opts).tickers.is_empty());
        let late = snap.cut_at(snap.frame.slot_end(60).timestamp(), false);
        let view = build_view(&snap, &late, &opts);
        assert_eq!(view.tickers.len(), 1);
        assert_eq!(view.tickers[0].pts.len(), 2);
        assert_eq!(build_detail(&snap, "TEST", &early).unwrap().candles.len(), 1);
    }

    #[test]
    fn live_cut_tells_a_closed_slot_from_one_still_filling() {
        let mut snap = snapshot(Vec::new());
        let start = snap.frame.slot_start(50).timestamp();
        // Real-time plan: read 2 minutes into slot 50, at that same instant.
        snap.pm_seen_s = start + 120;
        snap.fetched_s = start + 120;
        let cut = snap.live_cut();
        assert_eq!(cut.closed, 50);
        assert_eq!(cut.filling.map(|(i, _)| i), Some(50));
        // Delayed plan: the same slot asked for 16 minutes later is complete.
        snap.fetched_s = start + 120 + 16 * 60;
        assert_eq!(snap.live_cut(), Cut { closed: 51, filling: None });
    }

    #[test]
    fn a_split_this_morning_keeps_one_price_basis() {
        let opts = AttentionOptions::default();
        // 1:10 reverse split effective today: closed at 1.00, traded 1.20 after
        // hours (pre-split) and 13.00 in premarket (post-split).
        let mut t = ticker(vec![slot(2, 1.2, 60_000.0), slot(50, 13.0, 200_000.0)]);
        t.ref_close = 1.0;
        t.factor = 0.1;
        let v = eval_ticker(&t, &Cut::all(), &opts).unwrap();
        assert!((v.ref_close - 10.0).abs() < 1e-6);
        assert!((v.pts[0].2 - 20.0).abs() < 1e-3, "after-hours +20 %");
        assert!((v.pts[1].2 - 30.0).abs() < 1e-3, "premarket +30 %, not +1200 %");
        // 50 000 pre-split shares are 5 000 of today's.
        assert!((v.ah.vol - 5_000.0).abs() < 1.0);
        let snap = snapshot(vec![t]);
        let d = build_detail(&snap, "TEST", &Cut::all()).unwrap();
        assert!((d.candles[0].c - 12.0).abs() < 1e-3);
    }

    #[test]
    fn groups_keep_funds_apart_and_flag_unknown_caps() {
        assert_eq!(group_of(Some("ETF"), Some(1e9), 2e9), Some(Group::Etf));
        assert_eq!(group_of(Some("FUND"), None, 2e9), Some(Group::Etf));
        assert_eq!(group_of(Some("CS"), Some(1.9e9), 2e9), Some(Group::Small));
        assert_eq!(group_of(Some("CS"), Some(2e9), 2e9), Some(Group::Big));
        assert_eq!(group_of(Some("ADRC"), None, 2e9), Some(Group::Unknown));
        assert_eq!(group_of(Some("CS"), Some(0.0), 2e9), Some(Group::Unknown));
        assert_eq!(group_of(None, None, 2e9), Some(Group::Unknown));
        assert_eq!(group_of(Some("WARRANT"), None, 2e9), None);
    }
}
