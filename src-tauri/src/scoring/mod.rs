// Panic Mean Reversion watchlist engine.
//
// Builds the pre-open watchlist for the "Panic Mean Reversion" screener, once per
// trading day (triggered at 09:00 ET by `crate::panic_watchlist`, persisted to the
// `panic_watchlist` DB table so a crash/restart reuses the day's list). The job has
// three stages:
//
//   1. HARD PRE-FILTER (universe entry gate). A ticker is a candidate only if its
//      latest daily close is > $1 AND it shows real liquidity, via ANY of:
//        • premarket volume        > 100k shares, OR
//        • premarket dollar volume > $500k,        OR
//        • average daily $ volume over 20 days > $5M.
//      Premarket volume/$volume are aggregated from today's 1-minute bars
//      (04:00 ET → 09:30 cap) fetched from Alpaca; the 20-day average $ volume comes
//      from the local daily cache. (Names already clearing the $5M average don't
//      need a premarket fetch, so we only fetch premarket bars for the rest.)
//
//   2. TWO RANKINGS over the surviving candidates (daily bars only):
//        • BB AREA — cumulative Bollinger extension. BBZ = (Close − SMA20)/StdDev20.
//          With a SOFT band of 1.7 (not 2): BBZ_Excess_UP = max(0, BBZ − 1.7),
//          BBZ_Excess_DOWN = max(0, −BBZ − 1.7). BBZ_Area_{UP,DOWN}_6D = the sum of
//          those excesses over the 6 most recent completed days (J-1..J-6). The list
//          value = max(area_up, area_down); direction = the dominant side. This
//          surfaces tickers that stayed STRETCHED for several days, not just a single
//          big last-day BBZ, and catches the move earlier than a strict 2σ break.
//        • MOVE SINCE LAST SMA20 CONTACT — how far price has travelled since it last
//          touched (or gapped through) its SMA20, normalised by ATR20 (true range,
//          gaps included). We scan back to the last bar that touched the SMA20
//          (low ≤ SMA20 ≤ high) OR crossed it in a gap (the franchissement bar serves
//          as the implicit contact); the reference is that bar's SMA20 value. The
//          list value = |close − reference_SMA20| / ATR20; direction = its sign.
//
//   3. MERGE — take the top 10 of each list. A ticker counted in both appears once,
//      in the list where it ranks better. The final set (≤20 rows) is ordered by
//      interleaving the two lists 1-for-1 by rank (BB#1, MA#1, BB#2, MA#2, …) via a
//      `display_score`, and each row keeps its own metric value + list tag for the UI
//      ("BB 4.2 ▲" / "MA 3.1 ▲").
//
// Rules honoured: no future data (every series uses only closes ≤ t, today's forming
// bar is absent pre-open), per-ticker isolation (histories never mixed), guards on
// zero/NaN std and degenerate ATR.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};


use crate::config::secrets::Secrets;
use crate::local_db::{cache_repository, scoring_repository, universe_repository};
use crate::local_db::scoring_repository::ScoreRow;

// ─── Tunables (recompile to apply) ────────────────────────────────────────────

/// How many trailing daily bars to load per candidate. Must comfortably exceed the
/// SMA20 warm-up + the longest realistic "days since last SMA20 contact" run.
pub const HISTORY_DAYS: u32 = 120;
/// Minimum usable daily bars for a ticker to be evaluated (SMA20 warm-up + the
/// 6-day Bollinger area window).
pub const MIN_BARS: usize = SMA_PERIOD + AREA_DAYS;
/// Bollinger / SMA window.
const SMA_PERIOD: usize = 20;
/// Soft Bollinger band (σ) used for the area excess — earlier than a strict 2σ.
const BBZ_SOFT: f64 = 1.7;
/// Number of trailing days summed for the Bollinger area (J-1..J-6).
const AREA_DAYS: usize = 6;
/// ATR window (true range, gaps included) normalising the move-since-contact.
const ATR_PERIOD: usize = 20;

// Hard pre-filter thresholds.
/// Minimum latest daily close (USD).
pub const MIN_PRICE: f64 = 1.0;
/// Minimum premarket volume (shares).
pub const PM_VOLUME_MIN: i64 = 100_000;
/// Minimum premarket dollar volume (USD).
pub const PM_DOLLAR_MIN: f64 = 500_000.0;
/// Minimum 20-day average dollar volume (USD) — the liquidity branch that needs no
/// premarket data.
pub const AVG_DOLLAR_MIN: f64 = 5_000_000.0;

/// How many tickers each of the two lists contributes before the merge.
pub const TOP_PER_LIST: usize = 10;

// ─── Input shapes ──────────────────────────────────────────────────────────────

/// One day's OHLCV (date-ASCENDING series). `volume` in shares.
#[derive(Debug, Clone, Copy)]
pub struct Bar {
    pub open:   f64,
    pub high:   f64,
    pub low:    f64,
    pub close:  f64,
    pub volume: i64,
}

/// Aggregated premarket activity for one ticker (04:00 ET → 09:30 cap, today).
#[derive(Debug, Clone, Copy, Default)]
pub struct PremarketStat {
    pub volume:        i64,
    pub dollar_volume: f64,
}

/// Which list a ticker ranked in.
const LIST_BB: &str = "BB";
const LIST_MA: &str = "MA";

// ─── Math helpers ──────────────────────────────────────────────────────────────

/// Mean + population standard deviation of a slice; None on empty/zero/NaN std.
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

/// Rolling (SMA, population std) over `period` aligned to each index — None for the
/// first `period-1` warm-up bars or a degenerate window.
fn rolling_sma_std(closes: &[f64], period: usize) -> Vec<Option<(f64, f64)>> {
    let n = closes.len();
    let mut out = vec![None; n];
    if n < period {
        return out;
    }
    for t in (period - 1)..n {
        out[t] = mean_std(&closes[t + 1 - period..=t]);
    }
    out
}

/// BBZ at index t given its (sma, std): (close − sma) / std.
fn bbz_at(close: f64, sma: f64, std: f64) -> f64 {
    (close - sma) / std
}

/// Current Bollinger Z of `price` against the SMA20/σ20 of the last `SMA_PERIOD`
/// daily closes (date-ASCENDING). None with fewer than `SMA_PERIOD` closes or a
/// degenerate σ. Used by the common chart info bar to show how stretched the live
/// price is versus its 20-day Bollinger basis, for every strategy.
pub fn current_bbz(closes_asc: &[f64], price: f64) -> Option<f64> {
    let n = closes_asc.len();
    if n < SMA_PERIOD {
        return None;
    }
    let (sma, std) = mean_std(&closes_asc[n - SMA_PERIOD..])?;
    Some(bbz_at(price, sma, std))
}

// ─── BB area (cumulative Bollinger extension over 6 days) ───────────────────────

/// Cumulative soft-Bollinger excess over the last `AREA_DAYS` completed bars, both
/// directions. Returns (area_up, area_down) or None when the SMA isn't defined for
/// every one of those bars. Excess uses the soft band: max(0, |BBZ| − 1.7) split by
/// sign.
pub fn bbz_area_6d(closes: &[f64]) -> Option<(f64, f64)> {
    let n = closes.len();
    if n < SMA_PERIOD + AREA_DAYS {
        return None;
    }
    let sma_std = rolling_sma_std(closes, SMA_PERIOD);
    let mut up = 0.0;
    let mut down = 0.0;
    for t in (n - AREA_DAYS)..n {
        let (sma, std) = sma_std[t]?;
        let z = bbz_at(closes[t], sma, std);
        up += (z - BBZ_SOFT).max(0.0);
        down += (-z - BBZ_SOFT).max(0.0);
    }
    Some((up, down))
}

// ─── ATR20 (true range, gaps included) ──────────────────────────────────────────

/// Average true range over the last `ATR_PERIOD` bars. TR = max(H−L, |H−Cprev|,
/// |L−Cprev|), so overnight gaps inflate the range. None on thin/degenerate input.
pub fn atr20(bars: &[Bar]) -> Option<f64> {
    let n = bars.len();
    if n < ATR_PERIOD + 1 {
        return None;
    }
    let mut sum = 0.0;
    for t in (n - ATR_PERIOD)..n {
        let pc = bars[t - 1].close;
        let (h, l) = (bars[t].high, bars[t].low);
        let tr = (h - l).max((h - pc).abs()).max((l - pc).abs());
        sum += tr;
    }
    let atr = sum / ATR_PERIOD as f64;
    if atr.is_finite() && atr > 0.0 { Some(atr) } else { None }
}

// ─── Move since last SMA20 contact ───────────────────────────────────────────────

/// Signed price move from the SMA20 value at the last contact bar to the latest
/// close. "Contact" = the most recent bar (scanning back) that either touches the
/// SMA20 (low ≤ SMA20 ≤ high) OR is the bar on which price crossed onto its current
/// side from the opposite side — which, for a price that gapped over the SMA20
/// without touching it, is the franchissement bar (its SMA20 is the reference).
/// Returns (signed_move, direction) or None when the SMA isn't defined / price sits
/// exactly on the SMA.
pub fn move_since_sma20_contact(bars: &[Bar]) -> Option<(f64, i8)> {
    let n = bars.len();
    if n < SMA_PERIOD + 1 {
        return None;
    }
    let closes: Vec<f64> = bars.iter().map(|b| b.close).collect();
    let sma_std = rolling_sma_std(&closes, SMA_PERIOD);

    // sign of (close − sma) at index t, when the SMA is defined.
    let side = |t: usize| -> Option<i8> {
        let (sma, _) = sma_std[t]?;
        let d = closes[t] - sma;
        Some(if d > 0.0 { 1 } else if d < 0.0 { -1 } else { 0 })
    };

    let last = n - 1;
    let cur = side(last)?;
    if cur == 0 {
        return None; // exactly on the SMA → no measurable extension
    }

    let first = SMA_PERIOD - 1; // earliest index with a (possibly defined) SMA
    // Scan back from the latest bar. The contact is the most recent bar that touches
    // the SMA20, or the bar on which price crossed onto its current side (gap-over
    // included). Bars whose SMA is undefined (a degenerate, zero-std window) are
    // skipped; the earliest DEFINED bar reached is the fallback reference.
    let mut contact: Option<usize> = None;
    for t in (first..=last).rev() {
        let Some((sma_t, _)) = sma_std[t] else { continue };
        contact = Some(t); // earliest defined bar seen so far (fallback)
        let touch = bars[t].low <= sma_t && sma_t <= bars[t].high;
        if touch {
            break;
        }
        // Crossing onto the current side happened at t when the previous bar was on
        // the opposite side. `side(t-1)` is None for a degenerate window — then keep
        // scanning back rather than declaring a crossing.
        if let Some(s) = side(t.saturating_sub(1)) {
            if t > first && s != cur {
                break;
            }
        }
    }

    let contact = contact?; // None only if no bar in range had a defined SMA
    let ref_sma = sma_std[contact].expect("contact index is defined").0;
    let mv = closes[last] - ref_sma;
    let dir = if mv > 0.0 { 1 } else if mv < 0.0 { -1 } else { 0 };
    Some((mv, dir))
}

// ─── Pre-filter ─────────────────────────────────────────────────────────────────

/// Hard universe gate: latest close > $1 AND any liquidity branch clears.
pub fn passes_prefilter(price: f64, pm: Option<&PremarketStat>, avg_dollar: f64) -> bool {
    if !(price > MIN_PRICE) {
        return false;
    }
    let pm_vol = pm.map(|p| p.volume).unwrap_or(0);
    let pm_dol = pm.map(|p| p.dollar_volume).unwrap_or(0.0);
    pm_vol > PM_VOLUME_MIN || pm_dol > PM_DOLLAR_MIN || avg_dollar > AVG_DOLLAR_MIN
}

// ─── Watchlist build (pure) ──────────────────────────────────────────────────────

/// A pre-merge ranking entry.
struct Entry {
    symbol:    String,
    value:     f64,
    direction: i8,
}

/// Build the final ≤2·TOP_PER_LIST watchlist rows from the candidate daily bars,
/// premarket stats and 20-day average dollar volumes. Pure (no I/O) so it's unit
/// testable; the orchestration around it lives in `build_and_store`.
pub fn compute_watchlist(
    candidates: &HashMap<String, Vec<Bar>>,
    premarket:  &HashMap<String, PremarketStat>,
    avg_dollar: &HashMap<String, f64>,
    prev_vol:   &HashMap<String, i64>,
) -> Vec<ScoreRow> {
    let mut bb: Vec<Entry> = Vec::new();
    let mut ma: Vec<Entry> = Vec::new();

    for (symbol, bars) in candidates {
        if bars.len() < MIN_BARS {
            continue;
        }
        let price = bars.last().map(|b| b.close).unwrap_or(0.0);
        let adv = avg_dollar.get(symbol).copied().unwrap_or(0.0);
        if !passes_prefilter(price, premarket.get(symbol), adv) {
            continue;
        }
        let closes: Vec<f64> = bars.iter().map(|b| b.close).collect();

        // BB area list.
        if let Some((up, down)) = bbz_area_6d(&closes) {
            let (value, direction) = if up >= down { (up, 1i8) } else { (down, -1i8) };
            if value > 0.0 {
                bb.push(Entry { symbol: symbol.clone(), value, direction });
            }
        }

        // Move-since-SMA20-contact list (normalised by ATR20).
        if let (Some((mv, dir)), Some(atr)) = (move_since_sma20_contact(bars), atr20(bars)) {
            let value = mv.abs() / atr;
            if value > 0.0 && dir != 0 {
                ma.push(Entry { symbol: symbol.clone(), value, direction: dir });
            }
        }
    }

    // Rank each list by value (desc), keep the top N.
    let sort_desc = |v: &mut Vec<Entry>| {
        v.sort_by(|a, b| b.value.partial_cmp(&a.value).unwrap_or(std::cmp::Ordering::Equal));
        v.truncate(TOP_PER_LIST);
    };
    sort_desc(&mut bb);
    sort_desc(&mut ma);

    // Merge: a ticker in both lists keeps the better (lower) rank; BB wins exact
    // ties (inserted first). Stores the winning (kind, value, direction, rank).
    struct Winner { kind: &'static str, value: f64, direction: i8, rank: usize }
    let mut winners: HashMap<String, Winner> = HashMap::new();
    let consider = |entries: &[Entry], kind: &'static str, winners: &mut HashMap<String, Winner>| {
        for (i, e) in entries.iter().enumerate() {
            let rank = i + 1; // 1-based
            match winners.get(&e.symbol) {
                Some(w) if w.rank <= rank => {} // existing rank is at least as good
                _ => {
                    winners.insert(e.symbol.clone(), Winner {
                        kind, value: e.value, direction: e.direction, rank,
                    });
                }
            }
        }
    };
    consider(&bb, LIST_BB, &mut winners);
    consider(&ma, LIST_MA, &mut winners);

    // Emit rows. display_score interleaves the two lists 1-for-1 by rank
    // (BB#1, MA#1, BB#2, MA#2 …): higher display_score sorts first.
    winners
        .into_iter()
        .map(|(symbol, w)| {
            let kind_offset = if w.kind == LIST_BB { 2 } else { 1 };
            let sort_key = (2 * w.rank) as i64 - kind_offset; // smaller = better
            ScoreRow {
                symbol:        symbol.clone(),
                list_kind:     w.kind.to_string(),
                value:         w.value,
                direction:     w.direction,
                rank:          w.rank as u32,
                display_score: -(sort_key as f64),
                prev_volume:   prev_vol.get(&symbol).copied(),
            }
        })
        .collect()
}

// ─── Orchestration (I/O) ────────────────────────────────────────────────────────

/// Aggregate today's premarket volume / dollar volume per symbol from Alpaca
/// 1-minute bars (04:00 ET → 09:30 ET cap). Returns an empty map when Alpaca keys
/// aren't configured (the pre-filter then relies on the 20-day average branch).
async fn fetch_premarket(
    secrets: &Arc<RwLock<Secrets>>,
    symbols: &[String],
) -> HashMap<String, PremarketStat> {
    let (key, secret) = {
        let s = secrets.read().unwrap();
        (s.alpaca_key.clone(), s.alpaca_secret.clone())
    };
    let (Some(key), Some(secret)) = (key, secret) else { return HashMap::new() };
    if key.is_empty() || secret.is_empty() || symbols.is_empty() {
        return HashMap::new();
    }

    // App clock: simulated instant during a Market Replay (the fetch start lands
    // on the replayed day's premarket; the bars call itself is end-clamped).
    let now = crate::time::now();
    let start = crate::time::et_clock_utc(now, 4, 0)
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    // Cap premarket at the 09:30 cash open so a late-day rebuild still measures only
    // the true premarket session, not the regular session.
    let cap = crate::time::et_session_open_utc(now);

    let bars = match crate::alpaca::bars::fetch_minute_bars_since(&key, &secret, symbols, &start).await {
        Ok(b) => b,
        Err(e) => {
            eprintln!("[tagdash] panic watchlist: premarket fetch failed: {e}");
            return HashMap::new();
        }
    };

    let mut out: HashMap<String, PremarketStat> = HashMap::new();
    for (sym, mbars) in bars {
        let mut stat = PremarketStat::default();
        for b in mbars {
            if b.time >= cap {
                continue; // beyond the premarket window
            }
            let v = b.volume as i64;
            stat.volume += v;
            // Minute VWAP × volume is the best per-bar dollar estimate; fall back to
            // the close when VWAP is absent.
            let px = b.vwap.unwrap_or(b.close);
            stat.dollar_volume += px * b.volume as f64;
        }
        if stat.volume > 0 {
            out.insert(sym, stat);
        }
    }
    out
}

/// Read the candidate universe + premarket activity, build the two-list watchlist,
/// and replace the `panic_watchlist` table. Returns the number of rows persisted.
/// Heavy (reads ~120 daily bars per symbol + a premarket minute-bar fetch), so it's
/// called once per day by `crate::panic_watchlist` (or on demand for testing).
pub async fn build_and_store(
    db: &Arc<Mutex<rusqlite::Connection>>,
    secrets: &Arc<RwLock<Secrets>>,
) -> Result<usize, String> {
    // 1. Candidate daily bars + 20-day average $ volume + previous-day volume.
    let (candidates, avg_dollar, prev_vol): (
        HashMap<String, Vec<Bar>>,
        HashMap<String, f64>,
        HashMap<String, i64>,
    ) = {
        // Daily window bounded at the app-clock "today" (exclusive): inert in
        // live mode (the cache holds nothing for today pre-open), and the leak
        // guard during a Market Replay (the cache DOES hold bars after the
        // replayed day — they must stay invisible).
        let today = crate::time::et_date(crate::time::now());
        let conn = db.lock().unwrap();
        let symbols = universe_repository::get_active_symbols(&conn).map_err(|e| e.to_string())?;
        let avg_dollar: HashMap<String, f64> = cache_repository::avg_dollar_volumes(&conn, 20, &today)
            .unwrap_or_default().into_iter().collect();
        let prev_vol: HashMap<String, i64> = cache_repository::latest_volumes(&conn, &today)
            .unwrap_or_default().into_iter().collect();
        let mut map = HashMap::with_capacity(symbols.len());
        for sym in symbols {
            if let Ok(rows) = cache_repository::ohlcv_ascending(&conn, &sym, HISTORY_DAYS, &today) {
                if rows.len() >= MIN_BARS {
                    // Drop sub-$1 names here already (cheap; avoids a premarket fetch).
                    if rows.last().map(|r| r.3 > MIN_PRICE).unwrap_or(false) {
                        let bars: Vec<Bar> = rows.into_iter()
                            .map(|(open, high, low, close, volume)| Bar { open, high, low, close, volume })
                            .collect();
                        map.insert(sym, bars);
                    }
                }
            }
        }
        (map, avg_dollar, prev_vol)
    };

    // 2. Premarket fetch — only for names that DON'T already clear the $5M average
    //    branch (they qualify regardless, and BB/MA use daily bars only, so their
    //    premarket activity is never needed downstream). Cuts the fetch set.
    let pm_symbols: Vec<String> = candidates
        .keys()
        .filter(|s| avg_dollar.get(*s).copied().unwrap_or(0.0) <= AVG_DOLLAR_MIN)
        .cloned()
        .collect();
    let premarket = fetch_premarket(secrets, &pm_symbols).await;
    eprintln!(
        "[tagdash] panic watchlist: {} candidates, premarket fetched for {} (got {} active)",
        candidates.len(), pm_symbols.len(), premarket.len(),
    );

    // 3. Build the two ranked lists + merge.
    let rows = compute_watchlist(&candidates, &premarket, &avg_dollar, &prev_vol);
    let n = rows.len();
    eprintln!("[tagdash] panic watchlist: {n} rows after merge (top {TOP_PER_LIST}/list)");

    // 4. Persist (atomic replace).
    {
        let conn = db.lock().unwrap();
        scoring_repository::replace_all(&conn, &rows).map_err(|e| e.to_string())?;
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(o: f64, h: f64, l: f64, c: f64) -> Bar {
        Bar { open: o, high: h, low: l, close: c, volume: 1_000_000 }
    }

    #[test]
    fn prefilter_branches() {
        let pm_big_vol = PremarketStat { volume: 150_000, dollar_volume: 0.0 };
        let pm_big_dol = PremarketStat { volume: 1_000, dollar_volume: 600_000.0 };
        let pm_small   = PremarketStat { volume: 1_000, dollar_volume: 1_000.0 };
        assert!(passes_prefilter(5.0, Some(&pm_big_vol), 0.0));
        assert!(passes_prefilter(5.0, Some(&pm_big_dol), 0.0));
        assert!(passes_prefilter(5.0, None, 6_000_000.0)); // avg $ vol branch
        assert!(!passes_prefilter(5.0, Some(&pm_small), 1_000_000.0)); // none clear
        assert!(!passes_prefilter(0.50, Some(&pm_big_vol), 9e9)); // sub-$1 rejected
    }

    #[test]
    fn bbz_area_rewards_sustained_extension() {
        // 20 flat days then 6 rising days well above the band → area_up > 0.
        let mut closes = vec![10.0; SMA_PERIOD];
        for i in 0..AREA_DAYS {
            closes.push(11.0 + i as f64 * 0.5);
        }
        let (up, down) = bbz_area_6d(&closes).expect("defined");
        assert!(up > 0.0, "expected upward area, got {up}");
        assert_eq!(down, 0.0);
    }

    #[test]
    fn bbz_area_symmetric_down() {
        let mut closes = vec![10.0; SMA_PERIOD];
        for i in 0..AREA_DAYS {
            closes.push(9.0 - i as f64 * 0.5);
        }
        let (up, down) = bbz_area_6d(&closes).expect("defined");
        assert_eq!(up, 0.0);
        assert!(down > 0.0, "expected downward area, got {down}");
    }

    #[test]
    fn move_measures_from_touch() {
        // 20 flat bars at 10 (SMA≈10), then a clean run up that never returns to the
        // SMA. The last bar that touched the SMA is around the breakout; the move is
        // measured from ~10 to the latest close.
        let mut bars: Vec<Bar> = (0..SMA_PERIOD).map(|_| bar(10.0, 10.1, 9.9, 10.0)).collect();
        let mut px = 10.0;
        for _ in 0..8 {
            let open = px;
            let close = px + 1.0;
            bars.push(bar(open, close + 0.1, open - 0.05, close));
            px = close;
        }
        let (mv, dir) = move_since_sma20_contact(&bars).expect("defined");
        assert_eq!(dir, 1);
        assert!(mv > 5.0, "expected a large up move from the SMA, got {mv}");
    }

    #[test]
    fn move_handles_gap_over_sma() {
        // Flat below-ish, then a GAP that jumps clear over the SMA without the candle
        // touching it, then continues up. Contact = the gap (franchissement) bar; the
        // move is still measured (positive) and never panics.
        let mut bars: Vec<Bar> = (0..SMA_PERIOD).map(|_| bar(10.0, 10.05, 9.95, 10.0)).collect();
        // Gap bar: opens and stays well above the SMA (~10) — low 12 > sma.
        bars.push(bar(12.0, 13.0, 12.0, 12.8));
        bars.push(bar(12.8, 14.0, 12.7, 13.8));
        let (mv, dir) = move_since_sma20_contact(&bars).expect("defined");
        assert_eq!(dir, 1);
        assert!(mv > 0.0, "gap-over move should be positive, got {mv}");
    }

    #[test]
    fn atr_includes_gaps() {
        // A gap makes TR exceed the intraday range.
        let mut bars: Vec<Bar> = (0..ATR_PERIOD).map(|_| bar(10.0, 10.2, 9.8, 10.0)).collect();
        bars.push(bar(12.0, 12.2, 11.9, 12.0)); // gap up: |H−Cprev|≈2.2 ≫ H−L=0.3
        let atr = atr20(&bars).expect("defined");
        assert!(atr > 0.3, "ATR should reflect the gap, got {atr}");
    }

    #[test]
    fn merge_dedupes_to_better_rank() {
        // Build candidates so AAA tops BB and is #2 in MA, BBB tops MA. AAA must
        // appear once, tagged BB (its better rank).
        let mut candidates: HashMap<String, Vec<Bar>> = HashMap::new();
        // AAA — strong sustained up extension + big move.
        let mut aaa: Vec<Bar> = (0..SMA_PERIOD).map(|_| bar(10.0, 10.1, 9.9, 10.0)).collect();
        let mut p = 10.0;
        for _ in 0..AREA_DAYS { p += 1.0; aaa.push(bar(p - 1.0, p + 0.1, p - 1.05, p)); }
        candidates.insert("AAA".into(), aaa);
        // BBB — moderate.
        let mut bbb: Vec<Bar> = (0..SMA_PERIOD).map(|_| bar(10.0, 10.1, 9.9, 10.0)).collect();
        let mut q = 10.0;
        for _ in 0..AREA_DAYS { q += 0.6; bbb.push(bar(q - 0.6, q + 0.1, q - 0.65, q)); }
        candidates.insert("BBB".into(), bbb);

        let avg_dollar: HashMap<String, f64> =
            [("AAA".into(), 9e9), ("BBB".into(), 9e9)].into_iter().collect();
        let rows = compute_watchlist(&candidates, &HashMap::new(), &avg_dollar, &HashMap::new());
        // Each symbol appears exactly once.
        let aaa_rows = rows.iter().filter(|r| r.symbol == "AAA").count();
        assert_eq!(aaa_rows, 1, "a symbol in both lists must appear once");
        assert!(rows.iter().all(|r| r.value > 0.0));
    }
}
