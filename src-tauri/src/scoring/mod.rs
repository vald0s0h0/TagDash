// Mean-reversion scoring engine (used by the "Panic Mean Reversion" pre-open
// strategy, and reusable by future ones). Computed once per day at startup from
// the daily-bar cache, persisted to `mean_reversion_scores`, then served as the
// pre-open screener's top-30 watchlist.
//
// The display_score is a CONTINUOUS, magnitude-aware composite of four components
// (each transformed to a bounded 0..1 magnitude, never a saturating percentile):
//
//   score = 100 · (0.60·B + 0.40·P) · (0.60 + 0.25·V + 0.15·R)
//
//   B — Bollinger event score / 100 (priority 1). Self-relative, 3-year history:
//       is the CURRENT move exceptional vs the ticker's OWN past, any duration?
//       Per horizon h: BB_Z_Return = (Return_h − mean)/std over the 3y Return_h
//       series; PR_BB_Return = rank of |current Z| vs history; BB_Area_h = Σ over
//       the last h days of max(0,|BB_Z_Close|−2) (area beyond either band);
//       BB_Score_h = 0.75·PR_BB_Return + 0.25·PR_BB_Area; BB = 0.80·best + 0.20·2nd.
//   P — Parabolic (priority 2, strongly weighted). Daily true ranges EXPANDING ×
//       directional efficiency (net displacement / path). Rewards a clean parabola.
//   V — Volume (priority 3). Log-scaled previous-day dollar volume ($10M→0, $2B→1).
//   R — Run (priority 4). Consecutive same-colour candles (close vs open),
//       1−exp(−L/3); its sign gives the direction (▲/▼).
//
// The (B,P) core is the extreme/directional signal; V and R only MODULATE it
// (×0.60..1.0) — they refine ordering but never rescue a name with no core. 100 is
// reached only if B=P=V=R=1 at once (≈never), so the top-30 spreads out instead of
// all pinning at 100. See the composite-tunables block for weights/constants.
//
// A cross-sectional Percent-Rank momentum score (pr_score, folded extremeness
// 2·|PR−50| over horizons 1..6) is still computed and persisted as a DIAGNOSTIC,
// but is NO LONGER part of the display/ranking: as a percentile it saturated at 100
// every day and systematically over-weighted noisy tails (glitches, illiquid names,
// split artefacts), which produced false signals and an untie-able top-30.
//
// Rules honoured: no future data (every series uses only closes ≤ t), per-ticker
// isolation (histories are never mixed), guards on zero/NaN std, scores clamped
// to 0..100. Tickers with less than 3 years of history are NOT excluded — they
// are still scored on whatever history they have; only a small `MIN_HISTORY_DAYS`
// floor is required to avoid pure noise (their self-relative scores are coarser).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::local_db::{cache_repository, scoring_repository, universe_repository};
use crate::local_db::scoring_repository::ScoreRow;

// ─── Tunables (recompile to apply) ────────────────────────────────────────────

/// Cumulative-return horizons, in trading days.
pub const HORIZONS: &[usize] = &[1, 2, 3, 4, 5, 6];
/// Classic Bollinger window + multiplier used for the BB_Z_Close / BB_Area part.
const BB_PERIOD: usize = 20;
const BB_BAND_K: f64 = 2.0;
/// Minimum usable daily closes for a ticker to be scored at all. Deliberately a
/// LOW floor: tickers with less than 3 years of history are NOT excluded (a
/// recent listing is still ranked) — this only rejects series too short for any
/// statistical meaning (the Bollinger window needs ~20, the return distribution a
/// few more). Thin-history names get coarser self-relative scores.
pub const MIN_HISTORY_DAYS: usize = 30;
/// How many trading days of history to feed the engine (≈3 years + headroom for
/// the longest horizon and the SMA20 warm-up).
pub const HISTORY_DAYS: u32 = 800;
/// Weighting of the two best per-horizon BB scores into the final event score.
const BB_BEST_WEIGHT: f64 = 0.80;
const BB_SECOND_WEIGHT: f64 = 0.20;
/// Within one horizon, the BB_Z_Return vs BB_Area split.
const BB_RETURN_WEIGHT: f64 = 0.75;
const BB_AREA_WEIGHT: f64 = 0.25;

// ─── Composite-score tunables ───────────────────────────────────────────────────
//
// The display score is a CONTINUOUS, magnitude-aware composite (not a percentile),
// so the top-30 spreads out instead of all pinning at 100:
//
//   score = 100 · (W_BOLLINGER·B + W_PARABOLIC·P) · (MULT_BASE + MULT_VOLUME·V + MULT_RUN·R)
//
//   B — Bollinger event score / 100 (self-relative; engine unchanged).   priority 1
//   P — parabolic: expanding daily true ranges × directional efficiency.  priority 2
//   V — log-scaled previous-day dollar volume.                            priority 3
//   R — consecutive same-colour candle run (close vs open).               priority 4
//
// The (B,P) core is the extreme/directional signal (Bollinger-dominant, parabolic
// strongly weighted); V and R only MODULATE it (×MULT_BASE..1.0) — they refine the
// ordering but never rescue a name with no core. 100 needs B=P=V=R=1 at once
// (≈never), so the score never saturates.

/// Parabolic window in trading days (true range uses each day's prior close, so the
/// engine reads PARA_WINDOW+1 bars).
const PARA_WINDOW: usize = 4;
/// tanh reference for the true-range expansion ratio: R_exp−1 of E_REF → tanh(1)≈0.76.
const PARA_E_REF: f64 = 1.0;
/// Dollar-volume log-scale anchors: ≈$10M → 0, ≈$2B → 1.
const VOL_DOLLAR_MIN: f64 = 10e6;
const VOL_DOLLAR_MAX: f64 = 2e9;
/// Consecutive-run reference length: 1−exp(−L/REF). L=3 → 0.63, L=6 → 0.86.
const RUN_REF: f64 = 3.0;
/// Core weights (sum to 1): Bollinger dominant, parabolic strongly rewarded.
const W_BOLLINGER: f64 = 0.60;
const W_PARABOLIC: f64 = 0.40;
/// Liquidity+persistence multiplier ∈ [MULT_BASE, MULT_BASE+MULT_VOLUME+MULT_RUN].
const MULT_BASE: f64 = 0.60;
const MULT_VOLUME: f64 = 0.25;
const MULT_RUN: f64 = 0.15;

// ─── Input ───────────────────────────────────────────────────────────────────────

/// One day's OHLCV. The scorer reads date-ASCENDING series of these.
#[derive(Debug, Clone, Copy)]
pub struct Bar {
    pub open:   f64,
    pub high:   f64,
    pub low:    f64,
    pub close:  f64,
    pub volume: i64,
}

// ─── Output ────────────────────────────────────────────────────────────────────

/// One ticker's computed scores. Persisted to `mean_reversion_scores`.
#[derive(Debug, Clone)]
pub struct MeanReversionScore {
    pub symbol:          String,
    /// Cross-sectional percent-rank momentum score (0..100). DIAGNOSTIC ONLY — no
    /// longer part of the display/ranking (it saturates at 100 and over-weights
    /// noisy tails); kept for comparison/debug.
    pub pr_score:        f64,
    /// Horizon (days) that achieved the best percent rank (diagnostic).
    pub pr_best_days:    u8,
    /// Self-relative Bollinger event score (0..100). Composite component B.
    pub bb_event_score:  f64,
    /// Horizon (days) that achieved the best per-horizon BB score.
    pub bb_best_horizon: u8,
    /// Parabolic component P (0..1): expanding true ranges × directional efficiency.
    pub parabolic_score: f64,
    /// Volume component V (0..1): log-scaled previous-day dollar volume.
    pub volume_score:    f64,
    /// Run component R (0..1): consecutive same-colour candle run.
    pub run_score:       f64,
    /// Length of the current same-colour run (days).
    pub run_len:         u8,
    /// Run direction: +1 bullish (green), −1 bearish (red), 0 none.
    pub run_dir:         i8,
    /// The continuous composite (0..100) — what the screener ranks + shows.
    pub display_score:   f64,
    /// "MR" — composite kind tag (kept for the existing label/UI plumbing).
    pub score_kind:      String,
}

// ─── Math helpers ──────────────────────────────────────────────────────────────

/// Percent rank of `x` within `values`: share of values ≤ x, in 0..100. Returns
/// 0.0 when the set is empty. Ties count as ≤ (so the max value ranks 100).
fn percent_rank(values: &[f64], x: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let le = values.iter().filter(|&&v| v <= x).count();
    (le as f64 / values.len() as f64) * 100.0
}

/// Percent rank of `x` within an ASCENDING-sorted slice (binary search). Same
/// semantics as `percent_rank` but O(log n) — used for the cross-sectional pools
/// which are pre-sorted once per pass.
fn percent_rank_sorted(sorted: &[f64], x: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let le = sorted.partition_point(|&v| v <= x);
    (le as f64 / sorted.len() as f64) * 100.0
}

/// Mean + population standard deviation of a slice. Returns None on empty input or
/// a non-finite / zero std (so callers can skip a degenerate horizon).
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

/// Cumulative log-return over `h` days ending at index `t`: ln(close_t / close_{t-h}).
/// CLOSE-to-CLOSE, so it measures the full move amplitude from the close `h` days
/// earlier — overnight gaps are inherently included (the gap is part of the move
/// from the prior close to the current close). None when out of range or a close
/// is non-positive.
fn log_return_at(closes: &[f64], t: usize, h: usize) -> Option<f64> {
    if t < h {
        return None;
    }
    let (a, b) = (closes[t - h], closes[t]);
    if a <= 0.0 || b <= 0.0 {
        return None;
    }
    let r = (b / a).ln();
    if r.is_finite() {
        Some(r)
    } else {
        None
    }
}

/// Rolling SMA + population std of `closes` over `period`, aligned to each index
/// (None for the first `period-1` warm-up bars or when std is degenerate).
fn rolling_bbz_close(closes: &[f64], period: usize) -> Vec<Option<f64>> {
    let n = closes.len();
    let mut out = vec![None; n];
    if n < period {
        return out;
    }
    for t in (period - 1)..n {
        let window = &closes[t + 1 - period..=t];
        if let Some((mean, std)) = mean_std(window) {
            out[t] = Some((closes[t] - mean) / std);
        }
    }
    out
}

// ─── Per-ticker Bollinger event score ──────────────────────────────────────────

/// Compute the self-relative Bollinger event score for one ticker from its
/// date-ascending daily closes. Returns (score 0..100, best horizon days) or None
/// when there isn't enough usable history.
pub fn bb_event_score(closes: &[f64]) -> Option<(f64, u8)> {
    if closes.len() < MIN_HISTORY_DAYS {
        return None;
    }
    let n = closes.len();
    let last = n - 1;

    // Rolling |BB_Z_Close| once (shared by every horizon's area).
    let bbz_close = rolling_bbz_close(closes, BB_PERIOD);
    let abs_bbz_close: Vec<Option<f64>> =
        bbz_close.iter().map(|o| o.map(|z| z.abs())).collect();

    let mut per_horizon: Vec<(f64, u8)> = Vec::new(); // (BB_Score_h, h)

    for &h in HORIZONS {
        // ── BB_Z_Return: standardize the 3y Return_h series, rank |current|. ──
        let returns_h: Vec<f64> = (h..n)
            .filter_map(|t| log_return_at(closes, t, h))
            .collect();
        let Some(current_return) = log_return_at(closes, last, h) else { continue };
        let Some((mean_r, std_r)) = mean_std(&returns_h) else { continue };
        let abs_z_hist: Vec<f64> = returns_h
            .iter()
            .map(|r| ((r - mean_r) / std_r).abs())
            .collect();
        let current_abs_z = ((current_return - mean_r) / std_r).abs();
        let pr_bb_return = percent_rank(&abs_z_hist, current_abs_z);

        // ── BB_Area_h: rolling h-day sum of max(0, |BB_Z_Close| − 2). ─────────
        // Built only where every bar in the window has a defined |BB_Z_Close|.
        let mut area_series: Vec<f64> = Vec::new();
        let excess = |z: f64| (z - BB_BAND_K).max(0.0);
        for t in 0..n {
            if t + 1 < h {
                continue;
            }
            let window = &abs_bbz_close[t + 1 - h..=t];
            if window.iter().any(|o| o.is_none()) {
                continue;
            }
            let area: f64 = window.iter().map(|o| excess(o.unwrap())).sum();
            area_series.push(area);
        }
        let pr_bb_area = match area_series.last() {
            Some(&current_area) => percent_rank(&area_series, current_area),
            None => 0.0,
        };

        let bb_score_h = BB_RETURN_WEIGHT * pr_bb_return + BB_AREA_WEIGHT * pr_bb_area;
        per_horizon.push((bb_score_h.clamp(0.0, 100.0), h as u8));
    }

    if per_horizon.is_empty() {
        return None;
    }
    // Best + second-best horizon scores → the weighted event score.
    per_horizon.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let (best, best_h) = per_horizon[0];
    let second = per_horizon.get(1).map(|x| x.0).unwrap_or(best);
    let event = (BB_BEST_WEIGHT * best + BB_SECOND_WEIGHT * second).clamp(0.0, 100.0);
    Some((event, best_h))
}

// ─── Parabolic / volume / run components ────────────────────────────────────────

/// Parabolic score ∈ [0,1] from date-ascending bars: rewards daily true ranges that
/// EXPAND while the move PROGRESSES in one direction (a clean parabola), and is low
/// for either flat-range trends or choppy moves. Needs PARA_WINDOW+1 bars (each TR
/// uses the prior close). Returns 0 on thin/degenerate input.
pub fn parabolic_score(bars: &[Bar]) -> f64 {
    let k = PARA_WINDOW;
    if bars.len() < k + 1 {
        return 0.0;
    }
    let n = bars.len();

    // TR% for each of the last k days (normalised by the prior close).
    let mut trp: Vec<f64> = Vec::with_capacity(k);
    for t in (n - k)..n {
        let prev_close = bars[t - 1].close;
        if prev_close <= 0.0 {
            return 0.0;
        }
        let (hi, lo) = (bars[t].high, bars[t].low);
        let tr = (hi - lo)
            .max((hi - prev_close).abs())
            .max((lo - prev_close).abs());
        trp.push(tr / prev_close);
    }

    // Expansion: recent half vs earlier half of the window.
    let half = k / 2;
    if half == 0 {
        return 0.0;
    }
    let earlier: f64 = trp[..half].iter().sum::<f64>() / half as f64;
    let recent: f64 = trp[k - half..].iter().sum::<f64>() / half as f64;
    if earlier <= 0.0 {
        return 0.0;
    }
    let r_exp = recent / earlier;
    let e_exp = (((r_exp - 1.0).max(0.0)) / PARA_E_REF).tanh();

    // Directional efficiency (Kaufman): net displacement / total path over the
    // window's closes. 1 = perfectly straight move, →0 = chop.
    let first_close = bars[n - k - 1].close;
    let last_close = bars[n - 1].close;
    let net = (last_close - first_close).abs();
    let mut path = 0.0;
    for t in (n - k)..n {
        path += (bars[t].close - bars[t - 1].close).abs();
    }
    let dir = if path > 0.0 { (net / path).clamp(0.0, 1.0) } else { 0.0 };

    (e_exp * dir).clamp(0.0, 1.0)
}

/// Consecutive same-colour candle run ending at the last bar. Colour =
/// sign(close − open) (the literal chandelier colour). Returns
/// (run_score 0..1, direction +1/−1/0, run length days).
pub fn run_score(bars: &[Bar]) -> (f64, i8, u8) {
    let color = |b: &Bar| -> i8 {
        if b.close > b.open {
            1
        } else if b.close < b.open {
            -1
        } else {
            0
        }
    };
    let Some(last) = bars.last() else { return (0.0, 0, 0) };
    let dir = color(last);
    if dir == 0 {
        return (0.0, 0, 0);
    }
    let mut len: u8 = 0;
    for b in bars.iter().rev() {
        if color(b) == dir {
            len = len.saturating_add(1);
        } else {
            break;
        }
    }
    let score = 1.0 - (-(len as f64) / RUN_REF).exp();
    (score.clamp(0.0, 1.0), dir, len)
}

/// Dollar-volume score ∈ [0,1]: log-scaled previous-day dollar volume between
/// VOL_DOLLAR_MIN (→0) and VOL_DOLLAR_MAX (→1).
pub fn volume_score(dollar_volume: f64) -> f64 {
    if dollar_volume <= 0.0 {
        return 0.0;
    }
    let lo = VOL_DOLLAR_MIN.log10();
    let hi = VOL_DOLLAR_MAX.log10();
    ((dollar_volume.log10() - lo) / (hi - lo)).clamp(0.0, 1.0)
}

// ─── Universe-wide computation ──────────────────────────────────────────────────

/// Compute every ticker's mean-reversion scores from the universe's daily bars.
///
/// `bars_by_symbol` maps symbol → date-ASCENDING daily OHLCV. Symbols with fewer
/// than `MIN_HISTORY_DAYS` usable bars (a small floor) are skipped; short (<3y)
/// histories are otherwise scored on whatever data they have.
///
/// Two passes: (1) per-ticker Bollinger event score + parabolic/volume/run
/// components + each ticker's signed current return per horizon; (2) cross-
/// sectional percent rank of those returns (DIAGNOSTIC pr_score), then the
/// continuous composite display score (see the composite-tunables header).
pub fn compute_universe(
    bars_by_symbol: &HashMap<String, Vec<Bar>>,
) -> Vec<MeanReversionScore> {
    // Pass 1 — per ticker: BB score, P/V/R components + signed current return.
    struct Partial {
        symbol:          String,
        bb_event_score:  f64,
        bb_best_horizon: u8,
        parabolic_score: f64,
        volume_score:    f64,
        run_score:       f64,
        run_len:         u8,
        run_dir:         i8,
        signed_returns:  Vec<Option<f64>>, // indexed parallel to HORIZONS
    }
    let mut partials: Vec<Partial> = Vec::new();
    // Per-horizon pool of SIGNED current returns across the universe (ranked).
    let mut pools: Vec<Vec<f64>> = vec![Vec::new(); HORIZONS.len()];

    for (symbol, bars) in bars_by_symbol {
        if bars.len() < MIN_HISTORY_DAYS {
            continue;
        }
        let closes: Vec<f64> = bars.iter().map(|b| b.close).collect();
        let last = closes.len() - 1;
        let signed_returns: Vec<Option<f64>> = HORIZONS
            .iter()
            .map(|&h| log_return_at(&closes, last, h))
            .collect();
        // Need at least one usable horizon to rank cross-sectionally.
        if signed_returns.iter().all(|o| o.is_none()) {
            continue;
        }
        // BB may be unavailable on very thin / degenerate history → contributes 0
        // (the ticker is still scored on the other components).
        let (bb, bb_h) = bb_event_score(&closes).unwrap_or((0.0, HORIZONS[0] as u8));
        let parabolic = parabolic_score(bars);
        let (run, run_dir, run_len) = run_score(bars);
        // Previous-day dollar volume = latest cached bar's volume × its close.
        let last_bar = bars[last];
        let dollar_volume = (last_bar.volume.max(0) as f64) * last_bar.close;
        let volume = volume_score(dollar_volume);
        for (i, r) in signed_returns.iter().enumerate() {
            if let Some(v) = r {
                pools[i].push(*v);
            }
        }
        partials.push(Partial {
            symbol: symbol.clone(),
            bb_event_score: bb,
            bb_best_horizon: bb_h,
            parabolic_score: parabolic,
            volume_score: volume,
            run_score: run,
            run_len,
            run_dir,
            signed_returns,
        });
    }

    // Pre-sort each horizon pool for the O(log n) percent-rank binary search.
    for pool in &mut pools {
        pool.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    }

    // Pass 2 — cross-sectional PR (diagnostic) + continuous composite.
    partials
        .into_iter()
        .map(|p| {
            // Diagnostic PR: best signed extremeness across horizons.
            let mut pr_score = 0.0_f64;
            let mut pr_best_days = HORIZONS[0] as u8;
            for (i, r) in p.signed_returns.iter().enumerate() {
                if let Some(v) = r {
                    let pr = percent_rank_sorted(&pools[i], *v);
                    let extremeness = 2.0 * (pr - 50.0).abs();
                    if extremeness > pr_score {
                        pr_score = extremeness;
                        pr_best_days = HORIZONS[i] as u8;
                    }
                }
            }

            // Continuous composite (the ranking score).
            let b = (p.bb_event_score / 100.0).clamp(0.0, 1.0);
            let core = W_BOLLINGER * b + W_PARABOLIC * p.parabolic_score;
            let mult = MULT_BASE + MULT_VOLUME * p.volume_score + MULT_RUN * p.run_score;
            let display_score = (100.0 * core * mult).clamp(0.0, 100.0);

            MeanReversionScore {
                symbol:          p.symbol,
                pr_score,
                pr_best_days,
                bb_event_score:  p.bb_event_score,
                bb_best_horizon: p.bb_best_horizon,
                parabolic_score: p.parabolic_score,
                volume_score:    p.volume_score,
                run_score:       p.run_score,
                run_len:         p.run_len,
                run_dir:         p.run_dir,
                display_score,
                score_kind:      "MR".to_string(),
            }
        })
        .collect()
}

// ─── DB orchestration ──────────────────────────────────────────────────────────

/// Read the universe's daily closes from the cache, compute every ticker's
/// mean-reversion scores, and replace the `mean_reversion_scores` table. Returns
/// the number of scored tickers. The (brief) read holds the DB lock; the heavy
/// per-ticker math runs lock-free; a final lock writes the result. Safe to call
/// at startup or on demand (force-recompute command).
pub fn compute_and_store(db: &Arc<Mutex<rusqlite::Connection>>) -> Result<usize, String> {
    // 1. Collect each active symbol's date-ascending OHLCV (≥ MIN_HISTORY_DAYS)
    //    plus the previous trading day's volume (latest cached bar) for the gate /
    //    tie-break / display.
    let (mut bars_by_symbol, volume_by_symbol): (HashMap<String, Vec<Bar>>, HashMap<String, i64>) = {
        let conn = db.lock().unwrap();
        let symbols = universe_repository::get_active_symbols(&conn).map_err(|e| e.to_string())?;
        let mut map = HashMap::with_capacity(symbols.len());
        for sym in symbols {
            if let Ok(rows) = cache_repository::ohlcv_ascending(&conn, &sym, HISTORY_DAYS) {
                if rows.len() >= MIN_HISTORY_DAYS {
                    let bars: Vec<Bar> = rows
                        .into_iter()
                        .map(|(open, high, low, close, volume)| Bar { open, high, low, close, volume })
                        .collect();
                    map.insert(sym, bars);
                }
            }
        }
        let volumes: HashMap<String, i64> = cache_repository::latest_volumes(&conn)
            .unwrap_or_default()
            .into_iter()
            .collect();
        (map, volumes)
    };
    eprintln!(
        "[tagdash] scoring: {} symbols with >= {} daily bars",
        bars_by_symbol.len(),
        MIN_HISTORY_DAYS,
    );

    // Hard pre-filter (Panic Mean Reversion entry gate): keep only genuine
    // multi-day movers (c0 > $1 and a ≥70% cumulative 2..6-day move in EITHER
    // direction — runner or crash) BEFORE the Bollinger composite ranks anything.
    // The scoring criteria are unchanged — only the entry pool is filtered. See
    // `panic_mean_reversion::passes_prefilter`.
    let before = bars_by_symbol.len();
    bars_by_symbol.retain(|_sym, bars| {
        let closes: Vec<f64> = bars.iter().map(|b| b.close).collect();
        crate::strategies::panic_mean_reversion::passes_prefilter(&closes)
    });
    eprintln!(
        "[tagdash] scoring: pre-filter kept {} / {} movers (c0 > ${:.0}, |move| ≥{:.0}% over {}..{}d)",
        bars_by_symbol.len(),
        before,
        crate::strategies::panic_mean_reversion::MIN_PRICE,
        crate::strategies::panic_mean_reversion::MIN_CUM_MOVE_PCT,
        crate::strategies::panic_mean_reversion::MIN_MOVE_DAYS,
        crate::strategies::panic_mean_reversion::MAX_MOVE_DAYS,
    );

    // 2. Compute (CPU-bound, no DB lock held), then attach the previous-day volume.
    let rows: Vec<ScoreRow> = compute_universe(&bars_by_symbol)
        .into_iter()
        .map(|s| {
            let prev_volume = volume_by_symbol.get(&s.symbol).copied();
            ScoreRow {
                symbol:          s.symbol,
                pr_score:        s.pr_score,
                pr_best_days:    s.pr_best_days,
                bb_event_score:  s.bb_event_score,
                bb_best_horizon: s.bb_best_horizon,
                parabolic_score: s.parabolic_score,
                volume_score:    s.volume_score,
                run_score:       s.run_score,
                run_len:         s.run_len,
                run_dir:         s.run_dir,
                display_score:   s.display_score,
                score_kind:      s.score_kind,
                prev_volume,
            }
        })
        .collect();
    let n = rows.len();

    eprintln!("[tagdash] scoring: {n} tickers scored");

    // 3. Persist (replace the whole table atomically).
    {
        let conn = db.lock().unwrap();
        scoring_repository::replace_all(&conn, &rows).map_err(|e| e.to_string())?;
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Doji bars (open=high=low=close) from a close series — lets the close-driven
    /// BB/PR tests run through `compute_universe` without exercising parabolic/run
    /// (both 0 for a doji), which are tested directly below.
    fn dojis_from_closes(closes: &[f64]) -> Vec<Bar> {
        closes
            .iter()
            .map(|&c| Bar { open: c, high: c, low: c, close: c, volume: 1_000_000 })
            .collect()
    }

    #[test]
    fn percent_rank_basics() {
        let v = vec![1.0, 2.0, 3.0, 4.0];
        assert_eq!(percent_rank(&v, 4.0), 100.0);
        assert_eq!(percent_rank(&v, 1.0), 25.0);
        assert_eq!(percent_rank(&[], 1.0), 0.0);
    }

    #[test]
    fn mean_std_guards_zero() {
        assert!(mean_std(&[5.0, 5.0, 5.0]).is_none()); // zero std
        assert!(mean_std(&[]).is_none());
        let (m, s) = mean_std(&[1.0, 3.0]).unwrap();
        assert!((m - 2.0).abs() < 1e-9);
        assert!(s > 0.0);
    }

    #[test]
    fn below_floor_history_is_skipped() {
        // Below MIN_HISTORY_DAYS → no BB score (pure noise guard). A <3y but
        // above-floor history is NOT skipped (covered by short_history_is_scored).
        let closes: Vec<f64> = (0..MIN_HISTORY_DAYS - 5).map(|i| 10.0 + i as f64 * 0.1).collect();
        assert!(bb_event_score(&closes).is_none());
    }

    #[test]
    fn short_history_is_scored() {
        // Just above the floor (well under 3 years) still produces scores — short
        // histories are included, not excluded.
        let mut closes: Vec<f64> = (0..MIN_HISTORY_DAYS + 5)
            .map(|i| 10.0 + ((i as f64) * 0.1).sin() * 0.2)
            .collect();
        *closes.last_mut().unwrap() = 18.0;
        let map: HashMap<String, Vec<Bar>> =
            [("AAA".to_string(), dojis_from_closes(&closes))].into_iter().collect();
        assert_eq!(compute_universe(&map).len(), 1, "short history must be scored");
    }

    #[test]
    fn a_recent_spike_scores_high() {
        // Flat history then a violent final-day jump → exceptional move.
        let mut closes: Vec<f64> = (0..MIN_HISTORY_DAYS + 200)
            .map(|i| 10.0 + ((i as f64) * 0.01).sin() * 0.05)
            .collect();
        *closes.last_mut().unwrap() = 20.0; // +100% on the last day
        let (score, _h) = bb_event_score(&closes).expect("should score");
        assert!(score > 90.0, "expected exceptional score, got {score}");
    }

    #[test]
    fn percent_rank_is_bidirectional() {
        // Diagnostic PR (still computed, out of the display). Universe of names
        // whose only move is on the last day, spanning a smooth −1%..+1% range,
        // plus a crash and a rip far outside it. The mid name (~0% move) sits at the
        // median → low extremeness; BOTH the crash and the rip land at a tail →
        // high extremeness (the percent rank works both directions).
        const N: usize = 99;
        let len = MIN_HISTORY_DAYS + 50;
        let make = |last: f64| -> Vec<Bar> {
            let mut c = vec![10.0; len];
            *c.last_mut().unwrap() = last;
            dojis_from_closes(&c)
        };
        let mut map: HashMap<String, Vec<Bar>> = HashMap::new();
        for k in 0..N {
            let pct = (k as f64 - (N as f64 - 1.0) / 2.0) / ((N as f64 - 1.0) / 2.0) * 0.01;
            map.insert(format!("M{k}"), make(10.0 * (1.0 + pct)));
        }
        map.insert("CRASH".into(), make(5.0));  // −50% last day
        map.insert("RIP".into(), make(20.0));   // +100% last day

        let scores: HashMap<String, f64> = compute_universe(&map)
            .into_iter()
            .map(|s| (s.symbol, s.pr_score))
            .collect();
        let mid = scores["M49"]; // ~0% move → median
        assert!(scores["CRASH"] > 90.0, "crash must rank extreme (got {})", scores["CRASH"]);
        assert!(scores["RIP"] > 90.0, "rip must rank extreme (got {})", scores["RIP"]);
        assert!(mid < 20.0, "a near-zero mover must rank low (got {mid})");
    }

    #[test]
    fn parabolic_rewards_expanding_directional_move() {
        // Strictly rising closes with each day's range bigger than the prior →
        // expansion high, directional efficiency = 1 → high parabolic score.
        let mut bars = vec![Bar { open: 10.0, high: 10.0, low: 9.9, close: 10.0, volume: 0 }];
        let mut price = 10.0;
        for r in [0.2, 0.4, 0.8, 1.6] {
            let open = price;
            let close = price + r;
            bars.push(Bar { open, high: close, low: open, close, volume: 0 });
            price = close;
        }
        let p = parabolic_score(&bars);
        assert!(p > 0.6, "expanding directional move should score high, got {p}");
    }

    #[test]
    fn parabolic_low_for_chop() {
        // Alternating up/down of equal size → net≈0, path large → efficiency ≈ 0.
        let mut bars = vec![Bar { open: 10.0, high: 10.1, low: 9.9, close: 10.0, volume: 0 }];
        for i in 0..6 {
            let open = bars.last().unwrap().close;
            let close = if i % 2 == 0 { open + 0.2 } else { open - 0.2 };
            bars.push(Bar {
                open,
                high: open.max(close) + 0.05,
                low: open.min(close) - 0.05,
                close,
                volume: 0,
            });
        }
        assert!(parabolic_score(&bars) < 0.3, "choppy move should score low");
    }

    #[test]
    fn run_counts_consecutive_same_colour() {
        let green = |o: f64, c: f64| Bar { open: o, high: c, low: o, close: c, volume: 0 };
        let red = |o: f64, c: f64| Bar { open: o, high: o, low: c, close: c, volume: 0 };
        // Two red days then five green days → run = 5 green, ending bullish.
        let mut bars = vec![red(11.0, 10.0), red(10.5, 10.0)];
        for _ in 0..5 {
            bars.push(green(10.0, 10.5));
        }
        let (score, dir, len) = run_score(&bars);
        assert_eq!(dir, 1);
        assert_eq!(len, 5);
        let expected = 1.0 - (-(5.0_f64) / RUN_REF).exp();
        assert!((score - expected).abs() < 1e-9, "got {score}, expected {expected}");
    }

    #[test]
    fn volume_score_log_scaled() {
        assert_eq!(volume_score(0.0), 0.0);
        assert_eq!(volume_score(5e6), 0.0, "below the floor clamps to 0");
        assert_eq!(volume_score(5e9), 1.0, "above the ceiling clamps to 1");
        // Geometric mid of the $10M..$2B log range → ~0.5.
        let mid = volume_score((VOL_DOLLAR_MIN * VOL_DOLLAR_MAX).sqrt());
        assert!((mid - 0.5).abs() < 0.02, "geometric mid should be ~0.5, got {mid}");
    }
}
