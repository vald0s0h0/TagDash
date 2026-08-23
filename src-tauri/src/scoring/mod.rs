// Panic Mean Reversion watchlist engine.
//
// Builds and refreshes the premarket watchlist (final promotion at 09:25 ET through
// `crate::panic_watchlist::ensure_day`, persisted to `panic_watchlist_v2` keyed by
// day). Four stages:
//
//   1. LIQUIDITY GATE. Latest daily close > $1 AND real liquidity, via ANY of:
//      premarket volume > 100k shares, premarket dollar volume > $500k, or 20-day
//      average dollar volume > $5M. ETFs / ETNs / closed-end funds / preferred /
//      warrants are excluded outright (`is_etf_like`), primarily from Massive's
//      reference type and only heuristically when that field is unavailable — a leveraged or buffer product
//      mechanically tracks an index, so its "extension" carries none of the
//      exhaustion this strategy trades.
//
//   2. PANIC SCORE over the daily bars. For each window N in 2..=5 days:
//
//        ret_N     = (close[t] − close[t−N]) / close[t−N]
//        sigma_N   = |ret_N| / (robust σ(daily returns over the 20 days ENDING AT t−N) × √N)
//        atr_exp_N = mean(TR over the window) / ATR20 ENDING AT t−N
//        extension = sigma_N × sqrt(atr_exp_N) × stability/flow quality
//
//      The robust σ winsorises isolated old gaps with a median/MAD band; an old
//      corporate/news discontinuity can no longer hide today's event. The winning
//      window is argmax(score_N). Both factors are measured against the
//      state BEFORE the window, which is the whole point:
//        • `sigma` is a "sigma spike": how exceptional the move is relative to the
//          name's OWN volatility. It is scale-free, so a $50M and a $50B name are
//          directly comparable — measured, a raw volume-expansion term instead
//          returned 84.7% nano-caps against 5.3% in the universe.
//        • `atr_exp` is "the ATR was stable, now it has expanded" — the factor that
//          separates a genuine panic from a slow, steady grind. The two previous
//          metrics (Bollinger area, move-since-SMA20-contact) measured DISTANCE only
//          and could not make that distinction at all: a 6-day drift scored like a
//          one-session crash.
//
//      Quiet baselines are regularised at 0.5% instead of excluded. ABSOLUTE gates
//      require |move| ≥ 5%, sigma ≥ 3.0 and atr_exp ≥ 1.3.
//
//   3. CURRENT PREMARKET SHOCK. Whole-universe snapshots discover new ≥5% moves;
//      detailed minute bars then measure their range, volume and directional path.
//      The best of the multiday and current-PM extensions wins. A reversal never
//      erases the extension that created the opportunity.
//
//   4. GLOBAL QUALITY RANKING. Market cap is retained as context, never a quota.
//      Stable-before/extreme-now regimes and one-way volume-backed paths receive a
//      modest quality adjustment before the global candidate cap.
//
// Rules honoured: no future data (every series uses only bars strictly before the
// build day; the day's own forming bar is absent), per-ticker isolation, guards on
// zero/NaN σ and degenerate ATR.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use crate::config::secrets::Secrets;
use crate::local_db::scoring_repository::ScoreRow;
use crate::local_db::{cache_repository, scoring_repository, universe_repository};

// ─── Tunables (recompile to apply) ────────────────────────────────────────────

/// Trailing daily bars loaded per candidate. Only `MIN_BARS` are strictly needed;
/// the surplus is headroom for the rolling windows.
pub const HISTORY_DAYS: u32 = 60;
/// Shortest / longest panic window, in trading days.
pub const MIN_WINDOW: usize = 2;
pub const MAX_WINDOW: usize = 5;
/// Lookback used for the pre-window volatility baseline (σ of returns and ATR).
const BASELINE: usize = 20;
/// Minimum usable daily bars: the longest window plus its baseline, plus one bar to
/// seed the first return / true range.
pub const MIN_BARS: usize = MAX_WINDOW + BASELINE + 1;
/// ATR window (true range, gaps included).
const ATR_PERIOD: usize = 20;
/// Bollinger window kept only for `current_bbz` (chart info bar, not the watchlist).
const SMA_PERIOD: usize = 20;

/// Numerical floor used to regularise very quiet pre-shock regimes. Unlike the old
/// 1.5% ATR gate this does not exclude stable assets; it only prevents a near-zero
/// denominator from manufacturing an infinite score.
pub const VOL_FLOOR_PCT: f64 = 0.5;
/// A statistically unusual but economically tiny move is not a panic opportunity.
pub const MIN_ABS_MOVE_PCT: f64 = 5.0;
/// Absolute panic thresholds — both must clear.
pub const MIN_SIGMA: f64 = 3.0;
pub const MIN_ATR_EXP: f64 = 1.3;

// Hard pre-filter thresholds.
pub const MIN_PRICE: f64 = 1.0;
pub const PM_VOLUME_MIN: i64 = 100_000;
pub const PM_DOLLAR_MIN: f64 = 500_000.0;
pub const AVG_DOLLAR_MIN: f64 = 5_000_000.0;

/// Global candidate budget before the contextual/fundamental ranking. A wider first
/// stage lets the second stage rescue structurally sound candidates instead of
/// discarding them because four more volatile peers happened to share their cap bin.
pub const MAX_WATCHLIST: usize = 30;

/// Named size buckets, ascending by upper bound.
pub const CAP_BINS: &[(&str, f64)] = &[
    ("<300M", 3.0e8),
    ("300M-2Md", 2.0e9),
    ("2-10Md", 1.0e10),
    (">10Md", f64::INFINITY),
];

/// Label used when the market cap cannot be resolved. Unknown cap is context only;
/// it is neither promoted nor demoted by a separate quota.
pub const CAP_BIN_UNKNOWN: &str = "inconnu";

/// Resolve a market cap into its bucket label.
pub fn cap_bin_of(market_cap: Option<f64>) -> String {
    match market_cap {
        Some(mc) if mc.is_finite() && mc > 0.0 => CAP_BINS
            .iter()
            .find(|(_, hi)| mc < *hi)
            .map(|(name, _)| (*name).to_string())
            .unwrap_or_else(|| CAP_BIN_UNKNOWN.to_string()),
        _ => CAP_BIN_UNKNOWN.to_string(),
    }
}

// ─── Input shapes ──────────────────────────────────────────────────────────────

/// One day's OHLCV (date-ASCENDING series). `volume` in shares.
#[derive(Debug, Clone, Copy)]
pub struct Bar {
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: i64,
}

/// Aggregated premarket activity for one ticker (04:00 ET → 09:30 cap).
#[derive(Debug, Clone, Copy, Default)]
pub struct PremarketStat {
    pub volume: i64,
    pub dollar_volume: f64,
    /// Last premarket trade price — drives the stage-2 gap re-rank.
    pub last_price: Option<f64>,
    pub open: Option<f64>,
    pub high: Option<f64>,
    pub low: Option<f64>,
    pub directional_efficiency: Option<f64>,
    pub consecutive_bars: Option<i32>,
}

/// The winning window's panic reading for one ticker.
#[derive(Debug, Clone, Copy)]
pub struct PanicScore {
    pub score: f64,
    pub window_days: u8,
    pub direction: i8,
    pub sigma: f64,
    pub atr_exp: f64,
    pub move_pct: f64,
    pub stability_score: f64,
    pub volume_expansion: Option<f64>,
    pub directional_efficiency: Option<f64>,
    pub consecutive_bars: Option<i32>,
    pub setup_kind: &'static str,
}

/// Result of one watchlist build, rich enough for the caller to tell "no survivors"
/// (a final, legitimate empty list) from "inputs were missing" (retry).
#[derive(Debug, Clone)]
pub struct BuildOutcome {
    pub day: String,
    pub rows: usize,
    /// Symbols that had enough daily history to be evaluated at all.
    pub candidates: usize,
    /// The premarket fetch succeeded, or was not needed.
    pub premarket_ok: bool,
}

impl BuildOutcome {
    /// True when an empty result is explained by missing inputs rather than by a
    /// genuinely calm market — the caller retries only in that case.
    pub fn inputs_missing(&self) -> bool {
        self.candidates == 0 || !self.premarket_ok
    }
}

// ─── Math helpers ──────────────────────────────────────────────────────────────

/// Mean + population standard deviation; None on empty/zero/NaN std.
fn mean_std(values: &[f64]) -> Option<(f64, f64)> {
    if values.is_empty() {
        return None;
    }
    let n = values.len() as f64;
    let mean = values.iter().sum::<f64>() / n;
    let var = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n;
    let std = var.sqrt();
    if !std.is_finite() || std <= 0.0 {
        return None;
    }
    Some((mean, std))
}

/// Current Bollinger Z of `price` against the SMA20/σ20 of the last `SMA_PERIOD`
/// daily closes (date-ASCENDING). Used by the shared chart info bar for EVERY
/// strategy — unrelated to the watchlist ranking, which is why it survives the
/// removal of the Bollinger-area metric.
pub fn current_bbz(closes_asc: &[f64], price: f64) -> Option<f64> {
    let n = closes_asc.len();
    if n < SMA_PERIOD {
        return None;
    }
    let (sma, std) = mean_std(&closes_asc[n - SMA_PERIOD..])?;
    Some((price - sma) / std)
}

/// True range at index `i` (needs the previous close, so `i >= 1`).
fn true_range(bars: &[Bar], i: usize) -> f64 {
    let pc = bars[i - 1].close;
    let (h, l) = (bars[i].high, bars[i].low);
    (h - l).max((h - pc).abs()).max((l - pc).abs())
}

/// Mean true range over the inclusive index range `[a, b]`; None on a degenerate or
/// out-of-bounds range.
fn tr_mean(bars: &[Bar], a: usize, b: usize) -> Option<f64> {
    if a < 1 || b < a || b >= bars.len() {
        return None;
    }
    let sum: f64 = (a..=b).map(|i| true_range(bars, i)).sum();
    let m = sum / ((b - a + 1) as f64);
    (m.is_finite() && m > 0.0).then_some(m)
}

/// Average true range over the last `ATR_PERIOD` bars of the series. Gaps included.
pub fn atr20(bars: &[Bar]) -> Option<f64> {
    let n = bars.len();
    if n < ATR_PERIOD + 1 {
        return None;
    }
    tr_mean(bars, n - ATR_PERIOD, n - 1)
}

fn median(mut values: Vec<f64>) -> Option<f64> {
    if values.is_empty() || values.iter().any(|v| !v.is_finite()) {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let n = values.len();
    Some(if n % 2 == 0 {
        (values[n / 2 - 1] + values[n / 2]) / 2.0
    } else {
        values[n / 2]
    })
}

/// Winsorised σ around median ± 4 scaled-MAD. One old gap must not inflate the
/// whole 20-day baseline and hide today's panic. A zero MAD falls back to ordinary
/// σ so small, genuine variations remain measurable.
fn robust_std(values: &[f64]) -> Option<f64> {
    let med = median(values.to_vec())?;
    let mad = median(values.iter().map(|v| (v - med).abs()).collect())?;
    let clipped: Vec<f64> = if mad > 1e-12 {
        let radius = 4.0 * 1.4826 * mad;
        values
            .iter()
            .map(|v| v.clamp(med - radius, med + radius))
            .collect()
    } else {
        values.to_vec()
    };
    mean_std(&clipped).map(|(_, sd)| sd)
}

/// Robust σ of daily close-to-close returns over the inclusive range `[a, b]`.
fn returns_std(bars: &[Bar], a: usize, b: usize) -> Option<f64> {
    if a < 1 || b < a || b >= bars.len() {
        return None;
    }
    let rets: Vec<f64> = (a..=b)
        .filter_map(|i| {
            let prev = bars[i - 1].close;
            (prev > 0.0).then(|| bars[i].close / prev - 1.0)
        })
        .collect();
    if rets.len() < (b - a + 1) {
        return None; // a zero/negative close in the window — refuse rather than guess
    }
    // A genuinely constant baseline is maximally stable, not invalid. Callers apply
    // the explicit volatility floor before division.
    Some(robust_std(&rets).unwrap_or(0.0))
}

fn directional_efficiency(bars: &[Bar], a: usize, b: usize) -> Option<f64> {
    if b <= a || b >= bars.len() {
        return None;
    }
    let net = (bars[b].close - bars[a].close).abs();
    let path: f64 = (a + 1..=b)
        .map(|i| (bars[i].close - bars[i - 1].close).abs())
        .sum();
    (path > 0.0).then_some((net / path).clamp(0.0, 1.0))
}

fn consecutive_closes(bars: &[Bar], end: usize) -> Option<i32> {
    if end == 0 || end >= bars.len() {
        return None;
    }
    let first = bars[end].close - bars[end - 1].close;
    let dir = if first > 0.0 {
        1
    } else if first < 0.0 {
        -1
    } else {
        return Some(0);
    };
    let mut run = 1;
    let mut i = end - 1;
    while i > 0 {
        let step = bars[i].close - bars[i - 1].close;
        if (dir > 0 && step <= 0.0) || (dir < 0 && step >= 0.0) {
            break;
        }
        run += 1;
        i -= 1;
    }
    Some(run * dir)
}

fn volume_expansion(
    bars: &[Bar],
    base_a: usize,
    base_b: usize,
    move_a: usize,
    move_b: usize,
) -> Option<f64> {
    if base_b < base_a || move_b < move_a || move_b >= bars.len() {
        return None;
    }
    let baseline: Vec<f64> = bars[base_a..=base_b]
        .iter()
        .filter(|b| b.volume > 0)
        .map(|b| b.volume as f64)
        .collect();
    let med = median(baseline)?;
    if med <= 0.0 {
        return None;
    }
    let movement: Vec<f64> = bars[move_a..=move_b]
        .iter()
        .filter(|b| b.volume > 0)
        .map(|b| b.volume as f64)
        .collect();
    if movement.is_empty() {
        return None;
    }
    Some((movement.iter().sum::<f64>() / movement.len() as f64) / med)
}

/// 0..100 score for a quiet, regular pre-shock regime. Low normal ATR and stable
/// daily ranges are positive: the strategy explicitly values "stable before,
/// exceptional now" rather than treating low baseline volatility as ineligible.
fn baseline_stability(bars: &[Bar], a: usize, b: usize, base_close: f64) -> Option<f64> {
    if a < 1 || b < a || b >= bars.len() || base_close <= 0.0 {
        return None;
    }
    let ranges: Vec<f64> = (a..=b)
        .map(|i| true_range(bars, i) / bars[i - 1].close.max(base_close * 0.01))
        .filter(|v| v.is_finite() && *v > 0.0)
        .collect();
    if ranges.is_empty() {
        return None;
    }
    let mean = ranges.iter().sum::<f64>() / ranges.len() as f64;
    if !mean.is_finite() || mean <= 0.0 {
        return None;
    }
    let variance = ranges.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / ranges.len() as f64;
    let sd = variance.sqrt();
    let quiet = 1.0 - (mean / 0.08).clamp(0.0, 1.0);
    let regular = 1.0 - (sd / mean.max(1e-9)).clamp(0.0, 1.0);
    Some(((quiet * 0.6 + regular * 0.4) * 100.0).clamp(0.0, 100.0))
}

fn quality_factor(stability: f64, volume_exp: Option<f64>, efficiency: Option<f64>) -> f64 {
    let stable = 0.9 + 0.2 * (stability / 100.0).clamp(0.0, 1.0);
    let flow = 0.9
        + 0.1 * efficiency.unwrap_or(0.5).clamp(0.0, 1.0)
        + 0.1
            * volume_exp
                .map(|v| (v.max(1.0).ln() / 3.0).clamp(0.0, 1.0))
                .unwrap_or(0.0);
    stable * flow
}

// ─── The panic score ───────────────────────────────────────────────────────────

/// Best `sigma × atr_exp` over the windows `MIN_WINDOW..=MAX_WINDOW`, or None when
/// the series is too short / degenerate. Both factors are measured against the state
/// BEFORE the window, so they answer "how exceptional is this move for THIS name,
/// and did its volatility actually expand".
pub fn panic_score(bars: &[Bar]) -> Option<PanicScore> {
    let n = bars.len();
    if n < MIN_BARS {
        return None;
    }
    let t = n - 1;
    let mut best: Option<PanicScore> = None;

    for w in MIN_WINDOW..=MAX_WINDOW {
        if t < w {
            continue;
        }
        let pre = t - w; // last index BEFORE the window
        if pre < BASELINE {
            continue; // not enough baseline to measure "normal"
        }
        let base_close = bars[pre].close;
        if base_close <= 0.0 {
            continue;
        }
        let ret = (bars[t].close - base_close) / base_close;
        if !ret.is_finite() || ret == 0.0 {
            continue;
        }
        let Some(sd_raw) = returns_std(bars, pre + 1 - BASELINE, pre) else {
            continue;
        };
        let Some(atr_pre) = tr_mean(bars, pre + 1 - ATR_PERIOD.min(pre), pre) else {
            continue;
        };
        let Some(atr_win) = tr_mean(bars, pre + 1, t) else {
            continue;
        };
        let move_pct = ret * 100.0;
        if move_pct.abs() < MIN_ABS_MOVE_PCT {
            continue;
        }

        let floor = base_close * VOL_FLOOR_PCT / 100.0;
        let sd = sd_raw.max(VOL_FLOOR_PCT / 100.0);
        let atr_exp = atr_win / atr_pre.max(floor);
        let sigma = ret.abs() / (sd * (w as f64).sqrt());
        if !sigma.is_finite() || !atr_exp.is_finite() {
            continue;
        }
        let stability_score =
            baseline_stability(bars, pre + 1 - ATR_PERIOD.min(pre), pre, base_close)
                .unwrap_or(50.0);
        let volume_exp = volume_expansion(bars, pre + 1 - BASELINE, pre, pre + 1, t);
        let efficiency = directional_efficiency(bars, pre, t);
        // sqrt(ATR expansion) avoids counting the same shock twice at full strength;
        // sigma remains the dominant amplitude term.
        let score =
            sigma * atr_exp.sqrt() * quality_factor(stability_score, volume_exp, efficiency);
        if best.map(|b| score > b.score).unwrap_or(true) {
            best = Some(PanicScore {
                score,
                window_days: w as u8,
                direction: if ret > 0.0 { 1 } else { -1 },
                sigma,
                atr_exp,
                move_pct,
                stability_score,
                volume_expansion: volume_exp,
                directional_efficiency: efficiency,
                consecutive_bars: consecutive_closes(bars, t),
                setup_kind: "multiday",
            });
        }
    }
    best
}

fn premarket_panic_score(bars: &[Bar], pm: &PremarketStat) -> Option<PanicScore> {
    let prev_close = bars.last()?.close;
    let last = pm.last_price?;
    if prev_close <= 0.0 || last <= 0.0 {
        return None;
    }
    let ret = last / prev_close - 1.0;
    let move_pct = ret * 100.0;
    if move_pct.abs() < MIN_ABS_MOVE_PCT || bars.len() < BASELINE + 1 {
        return None;
    }
    let n = bars.len();
    let sd = returns_std(bars, n - BASELINE, n - 1)?.max(VOL_FLOOR_PCT / 100.0);
    let atr_pre_raw = tr_mean(bars, n - ATR_PERIOD, n - 1)?;
    let atr_pre = atr_pre_raw.max(prev_close * VOL_FLOOR_PCT / 100.0);
    let high = pm.high.unwrap_or(last).max(last);
    let low = pm.low.unwrap_or(last).min(last);
    let current_tr = (high - low)
        .max((high - prev_close).abs())
        .max((low - prev_close).abs());
    let sigma = ret.abs() / sd;
    let atr_exp = current_tr / atr_pre;
    let stability_score =
        baseline_stability(bars, n - ATR_PERIOD, n - 1, prev_close).unwrap_or(50.0);
    let baseline_volume = median(
        bars[n - BASELINE..]
            .iter()
            .filter(|b| b.volume > 0)
            .map(|b| b.volume as f64)
            .collect(),
    );
    let volume_expansion = baseline_volume
        .filter(|v| *v > 0.0)
        .map(|v| pm.volume as f64 / v);
    let score = sigma
        * atr_exp.sqrt()
        * quality_factor(stability_score, volume_expansion, pm.directional_efficiency);
    Some(PanicScore {
        score,
        window_days: 0,
        direction: if ret > 0.0 { 1 } else { -1 },
        sigma,
        atr_exp,
        move_pct,
        stability_score,
        volume_expansion,
        directional_efficiency: pm.directional_efficiency,
        consecutive_bars: pm.consecutive_bars,
        setup_kind: "premarket",
    })
}

// ─── Universe filters ──────────────────────────────────────────────────────────

/// True when an asset is an ETF / ETN / closed-end fund / structured product or a
/// non-common share class, rather than an operating company.
///
/// Three signals, any one sufficient:
///   • LISTING VENUE — ARCA and BATS (Cboe BZX) are ETF listing venues; operating
///     companies list on NASDAQ / NYSE / AMEX. (A few genuine issuers sit there —
///     Cboe Global Markets itself — but they are mega caps this strategy would never
///     surface, so the trade-off is one-sided.)
///   • PRODUCT WORDS — catches products cross-listed on an equity venue, which the
///     venue test misses (e.g. "Direxion Daily MU Bear 1X ETF" on NASDAQ).
///   • ISSUER BRANDS — some leveraged products name neither "ETF" nor "fund":
///     "ProShares UltraPro Short QQQ" (SQQQ, NASDAQ) reached a real watchlist through
///     that gap. Only unambiguous ETF-sponsor brands are listed; asset managers that
///     are themselves listed operating companies (Invesco Ltd, BlackRock Inc) are
///     deliberately absent.
///
/// Deliberately NOT matched:
///   • "trust" — royalty trusts and REITs are legitimate operating issuers.
///   • "depositary" — ADRs are exactly the kind of foreign small cap this strategy
///     trades; excluding them would silently drop a large share of real candidates.
pub fn is_etf_like(
    security_type: Option<&str>,
    name: Option<&str>,
    exchange: Option<&str>,
) -> bool {
    // Massive reference data wins when present. Common/ordinary shares and common
    // ADR/GDRs are operating-equity candidates; every other known stock type is a
    // fund, preferred, warrant, unit, right or another non-common instrument.
    if let Some(kind) = security_type.map(str::trim).filter(|s| !s.is_empty()) {
        return !matches!(
            kind.to_ascii_uppercase().as_str(),
            "CS" | "ADRC" | "OS" | "GDR"
        );
    }
    if let Some(x) = exchange {
        let x = x.trim().to_ascii_uppercase();
        if x == "ARCA" || x == "BATS" {
            return true;
        }
    }
    let Some(n) = name else { return false };
    let n = n.to_ascii_lowercase();
    const PRODUCT: &[&str] = &[
        "etf",
        "etn",
        "exchange-traded",
        "exchange traded",
        " fund",
        "fund,",
        "funds",
    ];
    const ISSUER: &[&str] = &[
        "proshares",
        "direxion",
        "ishares",
        "spdr",
        "vaneck",
        "wisdomtree",
        "global x",
        "ultrapro",
        "ultrashort",
        "yieldmax",
        "xtrackers",
        "graniteshares",
        "roundhill",
    ];
    const CLASS: &[&str] = &["preferred stock", "warrant"];
    PRODUCT
        .iter()
        .chain(ISSUER)
        .chain(CLASS)
        .any(|pat| n.contains(pat))
}

/// Hard universe gate: latest close > $1 AND any liquidity branch clears.
pub fn passes_prefilter(price: f64, pm: Option<&PremarketStat>, avg_dollar: f64) -> bool {
    if !(price > MIN_PRICE) {
        return false;
    }
    let pm_vol = pm.map(|p| p.volume).unwrap_or(0);
    let pm_dol = pm.map(|p| p.dollar_volume).unwrap_or(0.0);
    pm_vol > PM_VOLUME_MIN || pm_dol > PM_DOLLAR_MIN || avg_dollar > AVG_DOLLAR_MIN
}

// ─── Watchlist build (pure) ────────────────────────────────────────────────────

/// Build the final watchlist rows. Pure (no I/O) so it is unit-testable; the
/// orchestration lives in `build_and_store`.
///
/// `excluded` holds ETF-like symbols resolved from the universe table; `market_caps`
/// is retained as context but never used as a quota.
pub fn compute_watchlist(
    day: &str,
    candidates: &HashMap<String, Vec<Bar>>,
    premarket: &HashMap<String, PremarketStat>,
    avg_dollar: &HashMap<String, f64>,
    prev_vol: &HashMap<String, i64>,
    excluded: &std::collections::HashSet<String>,
    market_caps: &HashMap<String, f64>,
) -> Vec<ScoreRow> {
    let mut out: Vec<ScoreRow> = Vec::new();

    for (symbol, bars) in candidates {
        if bars.len() < MIN_BARS || excluded.contains(symbol) {
            continue;
        }
        let previous_close = bars.last().map(|b| b.close).unwrap_or(0.0);
        let adv = avg_dollar.get(symbol).copied().unwrap_or(0.0);
        let pm = premarket.get(symbol);
        let current_price = pm.and_then(|p| p.last_price).unwrap_or(previous_close);
        if !passes_prefilter(current_price, pm, adv) {
            continue;
        }
        // The current premarket shock is a first-class candidate. It competes with
        // the completed multiday shock; it is not merely a directional multiplier.
        // A reversal therefore never erases the extension that created the setup.
        let daily = panic_score(bars).filter(|p| p.sigma >= MIN_SIGMA && p.atr_exp >= MIN_ATR_EXP);
        let today = pm
            .and_then(|p| premarket_panic_score(bars, p))
            .filter(|p| p.sigma >= MIN_SIGMA && p.atr_exp >= MIN_ATR_EXP);
        let p = match (daily, today) {
            (Some(a), Some(b)) => {
                if b.score > a.score {
                    b
                } else {
                    a
                }
            }
            (Some(a), None) => a,
            (None, Some(b)) => b,
            (None, None) => continue,
        };
        if !p.score.is_finite() {
            continue;
        }

        let cap_bin = cap_bin_of(market_caps.get(symbol).copied());
        out.push(ScoreRow {
            symbol: symbol.clone(),
            day: day.to_string(),
            score: p.score,
            window_days: p.window_days,
            direction: p.direction,
            sigma: p.sigma,
            atr_exp: p.atr_exp,
            cap_bin: Some(cap_bin),
            prev_volume: prev_vol.get(symbol).copied(),
            setup_kind: p.setup_kind.to_string(),
            move_pct: p.move_pct,
            stability_score: p.stability_score,
            volume_expansion: p.volume_expansion,
            directional_efficiency: p.directional_efficiency,
            consecutive_bars: p.consecutive_bars,
        });
    }

    // Quality wins globally. Market cap remains context, not a quota that forces a
    // weak/unstable name onto the list or suppresses a stronger stable candidate.
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out.truncate(MAX_WATCHLIST);
    out
}

// ─── Orchestration (I/O) ───────────────────────────────────────────────────────

#[derive(Debug, serde::Deserialize)]
struct SnapshotPrice {
    p: f64,
}

#[derive(Debug, serde::Deserialize)]
struct SnapshotBar {
    c: f64,
}

#[derive(Debug, serde::Deserialize)]
struct StockSnapshot {
    #[serde(rename = "latestTrade")]
    latest_trade: Option<SnapshotPrice>,
    #[serde(rename = "minuteBar")]
    minute_bar: Option<SnapshotBar>,
}

/// Cheap whole-universe discovery pass. Alpaca's multi-symbol snapshots carry the
/// latest trade/minute price without downloading every minute since 04:00. Detailed
/// bars are fetched only for names whose current gap (or prior-day extension) merits
/// inspection. Snapshots have no historical `asof`, so replay deliberately skips
/// them and uses replay-clamped minute bars instead.
async fn fetch_snapshot_prices(
    secrets: &Arc<RwLock<Secrets>>,
    symbols: &[String],
) -> (HashMap<String, f64>, bool) {
    if crate::replay::clock::is_active() {
        return (HashMap::new(), true);
    }
    let (key, secret) = {
        let s = secrets.read().unwrap();
        (s.alpaca_key.clone(), s.alpaca_secret.clone())
    };
    let (Some(key), Some(secret)) = (key, secret) else {
        return (HashMap::new(), true);
    };
    if key.is_empty() || secret.is_empty() || symbols.is_empty() {
        return (HashMap::new(), true);
    }
    let client = crate::http::client();
    let mut out = HashMap::new();
    for chunk in symbols.chunks(200) {
        let url = format!(
            "https://data.alpaca.markets/v2/stocks/snapshots?symbols={}",
            chunk.join(",")
        );
        let resp = match client
            .get(&url)
            .header("APCA-API-KEY-ID", &key)
            .header("APCA-API-SECRET-KEY", &secret)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[tagdash] panic snapshots failed: {e}");
                return (out, false);
            }
        };
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            eprintln!("[tagdash] panic snapshots HTTP {status}: {body}");
            return (out, false);
        }
        let page: HashMap<String, StockSnapshot> = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[tagdash] panic snapshots decode: {e}");
                return (out, false);
            }
        };
        for (symbol, snap) in page {
            let price = snap
                .latest_trade
                .map(|t| t.p)
                .or_else(|| snap.minute_bar.map(|b| b.c));
            if let Some(price) = price.filter(|p| p.is_finite() && *p > 0.0) {
                out.insert(symbol, price);
            }
        }
    }
    (out, true)
}

/// Aggregate premarket volume / dollar volume / last price per symbol from Alpaca
/// 1-minute bars (04:00 ET → 09:30 cap). Returns `(stats, ok)`: `ok` is false only
/// when a fetch was attempted and FAILED — missing credentials or an empty symbol
/// list are "nothing to do", not an error, so they must not trigger a retry loop.
async fn fetch_premarket(
    secrets: &Arc<RwLock<Secrets>>,
    symbols: &[String],
) -> (HashMap<String, PremarketStat>, bool) {
    let (key, secret) = {
        let s = secrets.read().unwrap();
        (s.alpaca_key.clone(), s.alpaca_secret.clone())
    };
    let (Some(key), Some(secret)) = (key, secret) else {
        return (HashMap::new(), true);
    };
    if key.is_empty() || secret.is_empty() || symbols.is_empty() {
        return (HashMap::new(), true);
    }

    // App clock: the simulated instant during a Market Replay, so the window lands on
    // the replayed day's premarket. The bars call is itself end-clamped.
    let now = crate::time::now();
    let start = crate::time::et_clock_utc(now, 4, 0)
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    // Cap at the 09:30 cash open so a late rebuild still measures only the premarket.
    let cap = crate::time::et_session_open_utc(now);

    let bars =
        match crate::alpaca::bars::fetch_minute_bars_since(&key, &secret, symbols, &start).await {
            Ok(b) => b,
            Err(e) => {
                eprintln!("[tagdash] panic watchlist: premarket fetch failed: {e}");
                return (HashMap::new(), false);
            }
        };

    let mut out: HashMap<String, PremarketStat> = HashMap::new();
    for (sym, mbars) in bars {
        let mut stat = PremarketStat::default();
        let mut last_ts = i64::MIN;
        let mut closes: Vec<f64> = Vec::new();
        for b in mbars {
            if b.time >= cap {
                continue; // beyond the premarket window
            }
            if stat.open.is_none() {
                stat.open = Some(b.open);
            }
            stat.high = Some(stat.high.map(|v| v.max(b.high)).unwrap_or(b.high));
            stat.low = Some(stat.low.map(|v| v.min(b.low)).unwrap_or(b.low));
            stat.volume += b.volume as i64;
            // Minute VWAP × volume is the best per-bar dollar estimate.
            stat.dollar_volume += b.vwap.unwrap_or(b.close) * b.volume as f64;
            let ts = b.time.timestamp();
            if ts >= last_ts {
                last_ts = ts;
                stat.last_price = Some(b.close);
            }
            closes.push(b.close);
        }
        if closes.len() >= 2 {
            let net = (closes.last().unwrap() - closes[0]).abs();
            let path: f64 = closes.windows(2).map(|w| (w[1] - w[0]).abs()).sum();
            if path > 0.0 {
                stat.directional_efficiency = Some((net / path).clamp(0.0, 1.0));
            }
            let mut run = 1_i32;
            let last_step = closes[closes.len() - 1] - closes[closes.len() - 2];
            let dir = if last_step > 0.0 {
                1
            } else if last_step < 0.0 {
                -1
            } else {
                0
            };
            if dir != 0 {
                for w in closes[..closes.len() - 1].windows(2).rev() {
                    let step = w[1] - w[0];
                    if (dir > 0 && step <= 0.0) || (dir < 0 && step >= 0.0) {
                        break;
                    }
                    run += 1;
                }
                stat.consecutive_bars = Some(run * dir);
            } else {
                stat.consecutive_bars = Some(0);
            }
        }
        if stat.volume > 0 {
            out.insert(sym, stat);
        }
    }
    (out, true)
}

/// Read the candidate universe + premarket activity, build the watchlist and replace
/// the rows for `day`. `day` is passed IN (never re-derived) so the whole build is
/// stamped with one consistent date even though the premarket fetch in the middle
/// takes tens of seconds.
///
/// Heavy — call it through `crate::panic_watchlist::ensure_day`, which owns the
/// once-per-day gate and the re-entrancy guard.
pub async fn build_and_store(
    app_dir: &std::path::Path,
    db: &Arc<Mutex<rusqlite::Connection>>,
    secrets: &Arc<RwLock<Secrets>>,
    day: &str,
) -> Result<BuildOutcome, String> {
    // 1. Candidate daily bars + 20-day average $ volume + previous-day volume
    //    + the ETF exclusion set + market caps for the size buckets.
    let (candidates, avg_dollar, prev_vol, excluded, market_caps) = {
        // Every daily window is bounded at `day` (exclusive): inert in live mode (the
        // cache holds nothing for today pre-open) and the leak guard during a Market
        // Replay, where the cache DOES hold bars after the replayed day.
        let conn = db.lock().unwrap();
        let meta = universe_repository::symbol_classification(&conn).unwrap_or_default();
        let excluded: std::collections::HashSet<String> = meta
            .into_iter()
            .filter(|(_, kind, name, exch)| {
                is_etf_like(kind.as_deref(), name.as_deref(), exch.as_deref())
            })
            .map(|(sym, _, _, _)| sym)
            .collect();
        let symbols = universe_repository::get_active_symbols(&conn).map_err(|e| e.to_string())?;
        let avg_dollar: HashMap<String, f64> = cache_repository::avg_dollar_volumes(&conn, 20, day)
            .unwrap_or_default()
            .into_iter()
            .collect();
        let prev_vol: HashMap<String, i64> = cache_repository::latest_volumes(&conn, day)
            .unwrap_or_default()
            .into_iter()
            .collect();
        let market_caps = crate::sec_shares::market_caps_as_of(&conn, day);

        let mut map = HashMap::with_capacity(symbols.len());
        for sym in symbols {
            // Skip ETFs before the read: avoids ~60 daily bars for thousands of
            // excluded products.
            if excluded.contains(&sym) {
                continue;
            }
            if let Ok(rows) = cache_repository::ohlcv_ascending(&conn, &sym, HISTORY_DAYS, day) {
                if rows.len() >= MIN_BARS {
                    let bars: Vec<Bar> = rows
                        .into_iter()
                        .map(|(open, high, low, close, volume)| Bar {
                            open,
                            high,
                            low,
                            close,
                            volume,
                        })
                        .collect();
                    map.insert(sym, bars);
                }
            }
        }
        (map, avg_dollar, prev_vol, excluded, market_caps)
    };
    let candidate_count = candidates.len();

    // 2. Whole-universe latest-price discovery, then detailed minute bars only for:
    //    (a) a current >=5% premarket shock, or (b) a completed daily shock. This is
    //    what lets a quiet stock that panics TODAY enter the list without downloading
    //    millions of minute bars on every refresh.
    let all_symbols: Vec<String> = candidates.keys().cloned().collect();
    let (snapshots, snapshots_ok) = fetch_snapshot_prices(secrets, &all_symbols).await;
    let mut pm_symbols: std::collections::HashSet<String> = candidates
        .iter()
        .filter(|(_, bars)| {
            panic_score(bars).is_some_and(|p| p.sigma >= MIN_SIGMA && p.atr_exp >= MIN_ATR_EXP)
        })
        .map(|(s, _)| s.clone())
        .collect();
    if crate::replay::clock::is_active() {
        // Historical snapshots do not exist. The minute endpoint is clamped to the
        // replay clock, so this heavier path is correct and confined to replay.
        pm_symbols.extend(all_symbols.iter().cloned());
    } else {
        for (symbol, price) in &snapshots {
            let Some(prev) = candidates
                .get(symbol)
                .and_then(|b| b.last())
                .map(|b| b.close)
            else {
                continue;
            };
            if prev > 0.0
                && *price > MIN_PRICE
                && ((*price / prev - 1.0) * 100.0).abs() >= MIN_ABS_MOVE_PCT
            {
                pm_symbols.insert(symbol.clone());
            }
        }
    }
    let pm_symbols: Vec<String> = pm_symbols.into_iter().collect();
    let (premarket, detail_ok) = fetch_premarket(secrets, &pm_symbols).await;
    let premarket_ok = snapshots_ok && detail_ok;
    eprintln!(
        "[tagdash] panic watchlist {day}: {candidate_count} candidates, premarket fetched for {} (got {}, ok={premarket_ok})",
        pm_symbols.len(),
        premarket.len(),
    );

    // 3. Extension score + global quality selection.
    let rows = compute_watchlist(
        day,
        &candidates,
        &premarket,
        &avg_dollar,
        &prev_vol,
        &excluded,
        &market_caps,
    );
    let n = rows.len();
    eprintln!("[tagdash] panic watchlist {day}: {n} rows (global top {MAX_WATCHLIST})");

    // 4. Persist this day's rows (other days untouched).
    {
        let conn = db.lock().unwrap();
        scoring_repository::replace_for_day(&conn, day, &rows).map_err(|e| e.to_string())?;
    }

    // 5. Warm the intraday execution data for the whole watchlist, so clicking a
    //    candidate renders its chart instantly. Best effort.
    //
    //    Warmed over `HISTORY_DAYS` sessions, not just `day`: a candidate is selected
    //    on a 2-to-5-day shock, so the execution pane has to show the move that put
    //    it on the list — one session shows only the aftermath. The whole span goes
    //    out as a single Alpaca range request per symbol chunk, so the extra sessions
    //    cost no extra round trips.
    if !rows.is_empty() {
        let (key, secret) = {
            let s = secrets.read().unwrap();
            (
                s.alpaca_key.clone().unwrap_or_default(),
                s.alpaca_secret.clone().unwrap_or_default(),
            )
        };
        if !key.is_empty() && !secret.is_empty() {
            let back = crate::flat_files::intraday::HISTORY_DAYS.saturating_sub(1) as u32;
            let mut days = {
                let conn = db.lock().unwrap();
                crate::local_db::cache_repository::trading_days_before(&conn, day, back)
                    .unwrap_or_default()
            };
            days.push(day.to_string());
            let symbols: Vec<String> = rows.iter().map(|r| r.symbol.clone()).collect();
            match crate::flat_files::intraday::fetch_and_store_days(
                app_dir, &key, &secret, &days, &symbols,
            )
            .await
            {
                Ok(k) => eprintln!(
                    "[tagdash] panic watchlist {day}: intraday bars warmed — {k} (symbol, session) pairs over {} days",
                    days.len()
                ),
                Err(e) => eprintln!("[tagdash] panic watchlist {day}: intraday warm-up failed: {e}"),
            }
        }
    }

    Ok(BuildOutcome {
        day: day.to_string(),
        rows: n,
        candidates: candidate_count,
        premarket_ok,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(o: f64, h: f64, l: f64, c: f64) -> Bar {
        Bar {
            open: o,
            high: h,
            low: l,
            close: c,
            volume: 1_000_000,
        }
    }

    /// `n` calm bars oscillating tightly around `px`.
    fn calm(n: usize, px: f64) -> Vec<Bar> {
        (0..n)
            .map(|i| {
                let d = if i % 2 == 0 { 0.01 } else { -0.01 };
                bar(px, px * 1.01, px * 0.99, px + d)
            })
            .collect()
    }

    #[test]
    fn prefilter_branches() {
        let pm_big_vol = PremarketStat {
            volume: 150_000,
            dollar_volume: 0.0,
            last_price: None,
            ..Default::default()
        };
        let pm_big_dol = PremarketStat {
            volume: 1_000,
            dollar_volume: 600_000.0,
            last_price: None,
            ..Default::default()
        };
        let pm_small = PremarketStat {
            volume: 1_000,
            dollar_volume: 1_000.0,
            last_price: None,
            ..Default::default()
        };
        assert!(passes_prefilter(5.0, Some(&pm_big_vol), 0.0));
        assert!(passes_prefilter(5.0, Some(&pm_big_dol), 0.0));
        assert!(passes_prefilter(5.0, None, 6_000_000.0));
        assert!(!passes_prefilter(5.0, Some(&pm_small), 1_000_000.0));
        assert!(!passes_prefilter(0.50, Some(&pm_big_vol), 9e9));
    }

    #[test]
    fn etf_detection_by_venue_name_and_issuer() {
        assert!(is_etf_like(
            Some("ETF"),
            Some("Misleading Corp"),
            Some("NASDAQ")
        ));
        assert!(!is_etf_like(
            Some("CS"),
            Some("Fund Managers Inc."),
            Some("NYSE")
        ));
        assert!(is_etf_like(
            None,
            Some("Alpha Architect 1-3 Month Box ETF"),
            Some("BATS")
        ));
        assert!(is_etf_like(
            None,
            Some("Direxion Daily MU Bear 1X ETF"),
            Some("NASDAQ")
        ));
        assert!(is_etf_like(
            None,
            Some("BlackRock Municipal Income Fund, Inc."),
            Some("NYSE")
        ));
        // Names that say neither "ETF" nor "fund" — caught by the issuer brand.
        assert!(is_etf_like(
            None,
            Some("ProShares UltraPro Short QQQ"),
            Some("NASDAQ")
        ));
        // Non-common share classes.
        assert!(is_etf_like(
            None,
            Some("HPE 7.625% Series C Mandatory Convertible Preferred Stock"),
            Some("NYSE")
        ));
        assert!(is_etf_like(
            None,
            Some("Palladyne AI Corp Warrants"),
            Some("NASDAQ")
        ));
        // Operating companies survive, including REITs / royalty trusts and ADRs.
        assert!(!is_etf_like(
            None,
            Some("Catalyst Pharmaceuticals, Inc. Common Stock"),
            Some("NASDAQ")
        ));
        assert!(!is_etf_like(None, Some("MV Oil Trust"), Some("NYSE")));
        assert!(!is_etf_like(
            None,
            Some("Immuron Limited American Depositary Shares"),
            Some("NASDAQ")
        ));
        assert!(!is_etf_like(None, Some("Invesco Ltd."), Some("NYSE")));
        assert!(!is_etf_like(None, None, Some("NASDAQ")));
    }

    #[test]
    fn panic_score_fires_on_a_sharp_two_day_crash() {
        // 24 calm bars, then two violent down days with expanded range.
        let mut bars = calm(24, 10.0);
        bars.push(bar(10.0, 10.1, 8.0, 8.2));
        bars.push(bar(8.2, 8.3, 6.4, 6.6));
        let p = panic_score(&bars).expect("score defined");
        assert_eq!(p.direction, -1, "a crash must be flagged downward");
        assert!(
            p.sigma > MIN_SIGMA,
            "sigma {} should clear {MIN_SIGMA}",
            p.sigma
        );
        assert!(
            p.atr_exp > MIN_ATR_EXP,
            "atr_exp {} should clear {MIN_ATR_EXP}",
            p.atr_exp
        );
    }

    #[test]
    fn panic_score_is_symmetric_upward() {
        let mut bars = calm(24, 10.0);
        bars.push(bar(10.0, 12.0, 9.9, 11.9));
        bars.push(bar(11.9, 14.0, 11.8, 13.8));
        let p = panic_score(&bars).expect("score defined");
        assert_eq!(p.direction, 1);
        assert!(p.sigma > MIN_SIGMA);
    }

    /// The defect that motivated the whole redesign: the previous metrics measured
    /// DISTANCE, so a slow steady grind (TBRG-style) scored like a crash.
    ///
    /// A low-volatility grind is now rejected even earlier by the pre-shock ATR
    /// floor. This prevents a mathematically tiny return σ from turning an ordinary
    /// drift into an enormous z-score. If it clears that floor, ATR expansion remains
    /// the second guard against a steady move being called a panic.
    #[test]
    fn slow_grind_is_rejected_by_the_atr_expansion_factor() {
        let mut grind: Vec<Bar> = Vec::new();
        let mut px = 20.0;
        for i in 0..30 {
            let open = px;
            // ~+1%/day with a little jitter, so σ of returns is small but non-zero
            // (a PERFECTLY constant return gives σ = 0 and is excluded outright).
            px *= if i % 3 == 0 { 1.012 } else { 1.008 };
            grind.push(bar(open, px * 1.002, open * 0.998, px));
        }
        if let Some(g) = panic_score(&grind) {
            assert!(
                g.atr_exp < MIN_ATR_EXP,
                "a steady grind must fail the expansion gate"
            );
        }

        let mut crash = calm(24, 10.0);
        crash.push(bar(10.0, 10.1, 8.0, 8.2));
        crash.push(bar(8.2, 8.3, 6.4, 6.6));
        let c = panic_score(&crash).expect("crash scored");
        assert!(c.atr_exp > MIN_ATR_EXP, "the crash must expand its range");
    }

    /// A perfectly regular series is regularised, then rejected by the ATR expansion
    /// gate instead of exploding to infinity.
    #[test]
    fn zero_volatility_baseline_is_regularised_then_fails_expansion() {
        let mut bars: Vec<Bar> = Vec::new();
        let mut px = 20.0;
        for _ in 0..30 {
            let open = px;
            px *= 1.01; // exactly +1% every day → σ of returns == 0
            bars.push(bar(open, px, open, px));
        }
        let p = panic_score(&bars).expect("regularised score");
        assert!(p.sigma.is_finite());
        assert!(p.atr_exp < MIN_ATR_EXP);
    }

    #[test]
    fn best_window_is_selected_and_reported() {
        // A shock spread over four sessions should win with a longer window than a
        // single-day pop would.
        let mut bars = calm(24, 50.0);
        for _ in 0..4 {
            let last = bars.last().unwrap().close;
            let next = last * 0.90;
            bars.push(bar(last, last * 1.001, next * 0.99, next));
        }
        let p = panic_score(&bars).expect("score defined");
        assert!((MIN_WINDOW..=MAX_WINDOW).contains(&(p.window_days as usize)));
        assert_eq!(p.direction, -1);
    }

    #[test]
    fn too_short_a_series_yields_nothing() {
        assert!(panic_score(&calm(MIN_BARS - 1, 10.0)).is_none());
    }

    #[test]
    fn cap_bins_cover_the_range_and_unknowns() {
        assert_eq!(cap_bin_of(Some(1.0e8)), "<300M");
        assert_eq!(cap_bin_of(Some(1.0e9)), "300M-2Md");
        assert_eq!(cap_bin_of(Some(5.0e9)), "2-10Md");
        assert_eq!(cap_bin_of(Some(5.0e10)), ">10Md");
        assert_eq!(cap_bin_of(None), CAP_BIN_UNKNOWN);
        assert_eq!(cap_bin_of(Some(0.0)), CAP_BIN_UNKNOWN);
    }

    #[test]
    fn calm_market_produces_an_empty_watchlist() {
        // Absolute thresholds, not a fixed top-N: nothing qualifies on a quiet day.
        let mut candidates: HashMap<String, Vec<Bar>> = HashMap::new();
        candidates.insert("CALM".into(), calm(40, 25.0));
        let avg: HashMap<String, f64> = [("CALM".into(), 9e9)].into_iter().collect();
        let rows = compute_watchlist(
            "2026-06-15",
            &candidates,
            &HashMap::new(),
            &avg,
            &HashMap::new(),
            &std::collections::HashSet::new(),
            &HashMap::new(),
        );
        assert!(rows.is_empty());
    }

    #[test]
    fn excluded_names_never_rank() {
        let mut crash = calm(24, 10.0);
        crash.push(bar(10.0, 10.1, 8.0, 8.2));
        crash.push(bar(8.2, 8.3, 6.4, 6.6));

        let mut candidates: HashMap<String, Vec<Bar>> = HashMap::new();
        candidates.insert("SOXS".into(), crash.clone());
        let avg: HashMap<String, f64> = [("SOXS".into(), 9e9)].into_iter().collect();
        let excluded: std::collections::HashSet<String> =
            ["SOXS".to_string()].into_iter().collect();
        let rows = compute_watchlist(
            "2026-06-15",
            &candidates,
            &HashMap::new(),
            &avg,
            &HashMap::new(),
            &excluded,
            &HashMap::new(),
        );
        assert!(rows.is_empty(), "an excluded ETF must never rank");
    }

    #[test]
    fn watchlist_uses_one_global_quality_cap() {
        let mut crash = calm(24, 10.0);
        crash.push(bar(10.0, 10.1, 8.0, 8.2));
        crash.push(bar(8.2, 8.3, 6.4, 6.6));
        let mut candidates: HashMap<String, Vec<Bar>> = HashMap::new();
        let mut avg: HashMap<String, f64> = HashMap::new();
        let mut caps: HashMap<String, f64> = HashMap::new();
        for i in 0..(MAX_WATCHLIST + 8) {
            let s = format!("T{i}");
            let mut ranked = crash.clone();
            // Give each row a slightly different extension so truncation is global,
            // not an accidental HashMap order.
            ranked.last_mut().unwrap().close -= i as f64 * 0.01;
            candidates.insert(s.clone(), ranked);
            avg.insert(s.clone(), 9e9);
            if i % 2 == 0 {
                caps.insert(s, 1.0e8);
            }
        }
        let rows = compute_watchlist(
            "2026-06-15",
            &candidates,
            &HashMap::new(),
            &avg,
            &HashMap::new(),
            &std::collections::HashSet::new(),
            &caps,
        );
        assert_eq!(rows.len(), MAX_WATCHLIST);
        assert!(rows.windows(2).all(|w| w[0].score >= w[1].score));
    }

    #[test]
    fn current_premarket_shock_is_a_first_class_candidate() {
        let bars = calm(30, 10.0);
        let pm = PremarketStat {
            volume: 500_000,
            dollar_volume: 2_000_000.0,
            last_price: Some(8.0),
            open: Some(9.8),
            high: Some(9.9),
            low: Some(7.8),
            directional_efficiency: Some(0.9),
            consecutive_bars: Some(-5),
        };
        let p = premarket_panic_score(&bars, &pm).expect("premarket panic");
        assert_eq!(p.setup_kind, "premarket");
        assert_eq!(p.direction, -1);
        assert!(p.move_pct < -MIN_ABS_MOVE_PCT);
        assert!(p.sigma >= MIN_SIGMA && p.atr_exp >= MIN_ATR_EXP);
    }

    #[test]
    fn economically_small_premarket_move_is_not_a_panic() {
        let bars = calm(30, 10.0);
        let pm = PremarketStat {
            volume: 500_000,
            dollar_volume: 5_000_000.0,
            last_price: Some(10.30),
            high: Some(10.35),
            low: Some(9.95),
            ..Default::default()
        };
        assert!(premarket_panic_score(&bars, &pm).is_none());
    }

    #[test]
    fn a_quiet_regular_baseline_scores_as_more_stable() {
        let quiet = calm(30, 10.0);
        let mut erratic = calm(30, 10.0);
        for (i, b) in erratic.iter_mut().enumerate() {
            let width = if i % 2 == 0 { 0.02 } else { 0.35 };
            b.high = b.close * (1.0 + width);
            b.low = b.close * (1.0 - width);
        }
        let q = baseline_stability(&quiet, 5, 24, 10.0).unwrap();
        let e = baseline_stability(&erratic, 5, 24, 10.0).unwrap();
        assert!(q > e, "quiet={q}, erratic={e}");
    }

    #[test]
    fn robust_volatility_limits_one_old_outlier() {
        let mut values = vec![0.01, -0.01, 0.012, -0.008, 0.009, -0.011, 0.01, -0.01];
        let baseline = robust_std(&values).unwrap();
        values.push(0.80);
        let with_outlier = robust_std(&values).unwrap();
        let ordinary = mean_std(&values).unwrap().1;
        assert!(baseline > 0.0);
        assert!(
            with_outlier < ordinary / 5.0,
            "one old gap must be winsorised"
        );
    }
}
