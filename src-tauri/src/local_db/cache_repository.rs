use std::collections::HashMap;

use rusqlite::{params, Connection, Result};
use serde::{Deserialize, Serialize};

// ─── App-config key/value (small persisted markers) ──────────────────────────

/// Read a value from the `app_config` key/value table.
pub fn get_app_meta(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row(
        "SELECT value FROM app_config WHERE key=?1",
        params![key],
        |r| r.get::<_, String>(0),
    )
    .ok()
}

/// Upsert a value into the `app_config` key/value table.
pub fn set_app_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO app_config (key, value, updated_at) VALUES (?1, ?2, datetime('now'))
         ON CONFLICT(key) DO UPDATE SET value=excluded.value, updated_at=excluded.updated_at",
        params![key, value],
    )?;
    Ok(())
}

// ─── Fundamentals ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FundamentalCache {
    pub symbol: String,
    pub float_shares: Option<i64>,
    pub outstanding_shares: Option<i64>,
    pub free_float: Option<f64>,
    pub prev_close: Option<f64>,
    pub avg_volume: Option<i64>,
    pub atr: Option<f64>,
    pub updated_at: String,
}

pub fn upsert_fundamental(conn: &Connection, f: &FundamentalCache) -> Result<()> {
    // Only float/fundamentals columns: the multi-day change_* columns are owned
    // by `recompute_multiday_changes` and must NOT be touched here (this upsert
    // runs every startup and would otherwise wipe yesterday's computed changes).
    conn.execute(
        "INSERT INTO fundamentals_cache
             (symbol,float_shares,outstanding_shares,free_float,prev_close,
              avg_volume,atr,updated_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
         ON CONFLICT(symbol) DO UPDATE SET
             float_shares=excluded.float_shares,
             outstanding_shares=excluded.outstanding_shares,
             free_float=excluded.free_float,
             prev_close=excluded.prev_close,
             avg_volume=excluded.avg_volume,
             atr=excluded.atr,
             updated_at=excluded.updated_at",
        params![
            f.symbol, f.float_shares, f.outstanding_shares, f.free_float,
            f.prev_close, f.avg_volume, f.atr, f.updated_at,
        ],
    )?;
    Ok(())
}

pub fn get_fundamental(conn: &Connection, symbol: &str) -> Result<Option<FundamentalCache>> {
    let mut stmt = conn.prepare(
        "SELECT symbol,float_shares,outstanding_shares,free_float,prev_close,
                avg_volume,atr,updated_at
         FROM fundamentals_cache WHERE symbol=?1",
    )?;
    let mut rows = stmt.query_map(params![symbol], |row| {
        Ok(FundamentalCache {
            symbol: row.get(0)?,
            float_shares: row.get(1)?,
            outstanding_shares: row.get(2)?,
            free_float: row.get(3)?,
            prev_close: row.get(4)?,
            avg_volume: row.get(5)?,
            atr: row.get(6)?,
            updated_at: row.get(7)?,
        })
    })?;
    rows.next().transpose()
}

/// The behavioural / risk scores stored per ticker in `fundamentals_cache` (all
/// computed at startup, DB-wide percentile ranks 0..100 unless noted). `None` =
/// the inputs weren't collected for this symbol. Surfaced in the Micro Pullback
/// info overlay (and the tickers data table).
#[derive(Debug, Clone, Default, Serialize)]
pub struct RiskScores {
    /// Pump & dump behaviour (0..100, 100 = worst).
    pub pump_dump_score:        Option<f64>,
    /// Historical dilution percentile (0..100, 100 = worst).
    pub dilution_score:         Option<f64>,
    /// Capacity to dilute (0..100, 100 = worst).
    pub dilution_capacity_score: Option<f64>,
    /// Need to dilute (0..100, 100 = worst).
    pub dilution_need_score:    Option<f64>,
    /// Short-interest score (0..100, 100 = worst / most squeezy).
    pub short_interest_score:   Option<f64>,
}

/// Read the per-ticker behavioural / risk scores. Returns all-`None` when the
/// symbol has no `fundamentals_cache` row yet.
pub fn get_risk_scores(conn: &Connection, symbol: &str) -> Result<RiskScores> {
    let mut stmt = conn.prepare(
        "SELECT pump_dump_score, dilution_score, dilution_capacity_score,
                dilution_need_score, short_interest_score
         FROM fundamentals_cache WHERE symbol=?1",
    )?;
    let mut rows = stmt.query_map(params![symbol], |row| {
        Ok(RiskScores {
            pump_dump_score:         row.get(0)?,
            dilution_score:          row.get(1)?,
            dilution_capacity_score: row.get(2)?,
            dilution_need_score:     row.get(3)?,
            short_interest_score:    row.get(4)?,
        })
    })?;
    Ok(rows.next().transpose()?.unwrap_or_default())
}

/// All cached fundamentals (used to reuse FMP floats without a network call).
pub fn all_fundamentals(conn: &Connection) -> Result<Vec<FundamentalCache>> {
    let mut stmt = conn.prepare(
        "SELECT symbol,float_shares,outstanding_shares,free_float,prev_close,
                avg_volume,atr,updated_at
         FROM fundamentals_cache",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(FundamentalCache {
            symbol: row.get(0)?,
            float_shares: row.get(1)?,
            outstanding_shares: row.get(2)?,
            free_float: row.get(3)?,
            prev_close: row.get(4)?,
            avg_volume: row.get(5)?,
            atr: row.get(6)?,
            updated_at: row.get(7)?,
        })
    })?;
    rows.collect()
}

// ─── Multi-day price change (close-to-close, gaps included) ───────────────────

/// Close-to-close % change for one symbol over 1..6 trading days.
/// `changes[0]` = 1-day change (yesterday's close vs the day before),
/// `changes[5]` = 6-day cumulative change. None when history is too short.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultiDayChange {
    pub symbol: String,
    pub changes: [Option<f64>; 6],
}

/// Recompute, for every symbol with daily bars, the close-to-close percentage
/// change over the last 1..6 trading days and store it in `fundamentals_cache`
/// (`change_1d_pct` … `change_6d_pct`). The change over N days is
/// `(c0 - cN) / cN * 100`, where `c0` is the most recent cached close ("yesterday"
/// — today's bar isn't cached pre-open) and `cN` is the close N trading days
/// before it. Being close-to-close, each step spans the overnight gap, so gaps are
/// inherently included. Returns the number of symbols updated.
pub fn recompute_multiday_changes(conn: &Connection) -> Result<usize> {
    // One pass: pivot each symbol's 7 most recent closes (c0..c6) into a row.
    let rows: Vec<(String, [Option<f64>; 7])> = {
        let mut stmt = conn.prepare(
            "SELECT symbol,
                 MAX(CASE WHEN rn=1 THEN close END),
                 MAX(CASE WHEN rn=2 THEN close END),
                 MAX(CASE WHEN rn=3 THEN close END),
                 MAX(CASE WHEN rn=4 THEN close END),
                 MAX(CASE WHEN rn=5 THEN close END),
                 MAX(CASE WHEN rn=6 THEN close END),
                 MAX(CASE WHEN rn=7 THEN close END)
             FROM (
                 SELECT symbol, close,
                        ROW_NUMBER() OVER (PARTITION BY symbol ORDER BY date DESC) AS rn
                 FROM daily_cache WHERE close IS NOT NULL
             ) WHERE rn <= 7 GROUP BY symbol",
        )?;
        let mapped = stmt.query_map([], |row| {
            let symbol: String = row.get(0)?;
            let mut c: [Option<f64>; 7] = [None; 7];
            for (i, slot) in c.iter_mut().enumerate() {
                *slot = row.get::<_, Option<f64>>(i + 1)?;
            }
            Ok((symbol, c))
        })?;
        mapped.collect::<Result<Vec<_>>>()?
    };

    conn.execute_batch("BEGIN")?;
    let mut updated = 0usize;
    for (symbol, c) in &rows {
        let c0 = c[0];
        let mut changes: [Option<f64>; 6] = [None; 6];
        for (i, slot) in changes.iter_mut().enumerate() {
            *slot = match (c0, c[i + 1]) {
                (Some(now), Some(base)) if base != 0.0 => Some((now - base) / base * 100.0),
                _ => None,
            };
        }
        conn.execute(
            "INSERT INTO fundamentals_cache
                 (symbol,change_1d_pct,change_2d_pct,change_3d_pct,
                  change_4d_pct,change_5d_pct,change_6d_pct,updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7, datetime('now'))
             ON CONFLICT(symbol) DO UPDATE SET
                 change_1d_pct=excluded.change_1d_pct,
                 change_2d_pct=excluded.change_2d_pct,
                 change_3d_pct=excluded.change_3d_pct,
                 change_4d_pct=excluded.change_4d_pct,
                 change_5d_pct=excluded.change_5d_pct,
                 change_6d_pct=excluded.change_6d_pct,
                 updated_at=excluded.updated_at",
            params![
                symbol, changes[0], changes[1], changes[2],
                changes[3], changes[4], changes[5],
            ],
        )?;
        updated += 1;
    }
    conn.execute_batch("COMMIT")?;
    Ok(updated)
}

/// Read the stored multi-day close-to-close % changes for one symbol.
pub fn multiday_changes(conn: &Connection, symbol: &str) -> Result<Option<MultiDayChange>> {
    let mut stmt = conn.prepare(
        "SELECT change_1d_pct,change_2d_pct,change_3d_pct,
                change_4d_pct,change_5d_pct,change_6d_pct
         FROM fundamentals_cache WHERE symbol=?1",
    )?;
    let mut rows = stmt.query_map(params![symbol], |row| {
        let mut changes: [Option<f64>; 6] = [None; 6];
        for (i, slot) in changes.iter_mut().enumerate() {
            *slot = row.get::<_, Option<f64>>(i)?;
        }
        Ok(MultiDayChange { symbol: symbol.to_string(), changes })
    })?;
    rows.next().transpose()
}

/// Most recent `updated_at` across the fundamentals cache (RFC3339-ish string),
/// used to throttle the FMP full-universe load to once per calendar day.
pub fn fundamentals_last_date(conn: &Connection) -> Option<String> {
    conn.query_row(
        "SELECT MAX(updated_at) FROM fundamentals_cache",
        [],
        |r| r.get::<_, Option<String>>(0),
    )
    .ok()
    .flatten()
}

// ─── ATR + previous close (computed at startup from the daily cache) ──────────

/// Recompute `prev_close` (latest cached close) and `atr` (14-day average true
/// range, gaps included) for every symbol with daily bars, and store them in
/// `fundamentals_cache`. Owns ONLY those two columns (mirrors
/// `recompute_multiday_changes`): the float columns are written by
/// `upsert_fundamental` and must not be touched here. Returns symbols updated.
///
/// ATR uses the canonical true range TR = max(H−L, |H−Cprev|, |L−Cprev|) averaged
/// over the most recent `ATR_PERIOD` bars (same definition as `scoring::atr20`,
/// period 14). Symbols with fewer than `ATR_PERIOD + 1` bars get prev_close only.
pub fn recompute_atr_prev_close(conn: &Connection) -> Result<usize> {
    const ATR_PERIOD: usize = 14;
    const WINDOW: u32 = (ATR_PERIOD as u32) + 1; // need one prior close for the first TR

    // Pull the most recent WINDOW bars per symbol, oldest→newest, in one query.
    let rows: Vec<(String, f64, f64, f64)> = {
        let mut stmt = conn.prepare(
            "SELECT symbol, high, low, close FROM (
                 SELECT symbol, high, low, close, date,
                        ROW_NUMBER() OVER (PARTITION BY symbol ORDER BY date DESC) AS rn
                 FROM daily_cache
                 WHERE high IS NOT NULL AND low IS NOT NULL AND close IS NOT NULL
             ) WHERE rn <= ?1 ORDER BY symbol ASC, date ASC",
        )?;
        let mapped = stmt.query_map([WINDOW], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?, r.get::<_, f64>(2)?, r.get::<_, f64>(3)?))
        })?;
        mapped.collect::<Result<Vec<_>>>()?
    };

    // Fold per symbol (rows are grouped by symbol, ascending date).
    conn.execute_batch("BEGIN")?;
    let mut updated = 0usize;
    let mut i = 0usize;
    while i < rows.len() {
        let sym = rows[i].0.clone();
        let mut bars: Vec<(f64, f64, f64)> = Vec::new(); // (high, low, close)
        while i < rows.len() && rows[i].0 == sym {
            bars.push((rows[i].1, rows[i].2, rows[i].3));
            i += 1;
        }
        let prev_close = bars.last().map(|b| b.2);
        let atr = atr_from_hlc(&bars, ATR_PERIOD);
        conn.execute(
            "INSERT INTO fundamentals_cache (symbol, prev_close, atr, updated_at)
             VALUES (?1, ?2, ?3, datetime('now'))
             ON CONFLICT(symbol) DO UPDATE SET
                 prev_close=excluded.prev_close, atr=excluded.atr, updated_at=excluded.updated_at",
            params![sym, prev_close, atr],
        )?;
        updated += 1;
    }
    conn.execute_batch("COMMIT")?;
    Ok(updated)
}

/// ATR over the last `period` bars of an ascending (high, low, close) series.
/// None when there aren't `period + 1` bars or the result is degenerate.
fn atr_from_hlc(bars: &[(f64, f64, f64)], period: usize) -> Option<f64> {
    let n = bars.len();
    if n < period + 1 {
        return None;
    }
    let mut sum = 0.0;
    for t in (n - period)..n {
        let pc = bars[t - 1].2;
        let (h, l) = (bars[t].0, bars[t].1);
        sum += (h - l).max((h - pc).abs()).max((l - pc).abs());
    }
    let atr = sum / period as f64;
    (atr.is_finite() && atr > 0.0).then_some(atr)
}

// ─── Pump & Dump score (daily-wick behaviour, DB-wide percentile) ─────────────

/// Minimum daily bars required to score a ticker's pump&dump behaviour.
const PD_MIN_BARS: usize = 40;
/// Trailing daily-bar window the score is computed over.
const PD_WINDOW: u32 = 60;
/// A bar's wick is "big" when its total wick exceeds this multiple of the window ATR.
const PD_BIG_WICK: f64 = 1.5;

/// Pure pump&dump raw metric over an ascending (open, high, low, close) series.
///
/// Captures the "pump then dump" daily footprint: candle wicks that are large
/// relative to the ticker's *typical* daily range, and *frequent*. We normalise each
/// bar's total wick by the MEDIAN true range (gaps included) — the median, not the
/// mean, because the very spikes we're trying to detect would inflate a mean and
/// blunt the signal, whereas the median stays anchored to normal behaviour. The
/// metric is `mean(total_wick / median_TR) × (1 + big_wick_fraction)` where a bar is
/// "big" when its wick exceeds `PD_BIG_WICK` median ranges. Higher = more
/// pump&dump-like. None when the series is too short or the median range is
/// degenerate.
pub fn pump_dump_raw(bars: &[(f64, f64, f64, f64)]) -> Option<f64> {
    let n = bars.len();
    if n < PD_MIN_BARS {
        return None;
    }
    // True range per bar (gaps included); median = the typical daily range.
    let mut trs: Vec<f64> = Vec::with_capacity(n - 1);
    for t in 1..n {
        let pc = bars[t - 1].3;
        let (h, l) = (bars[t].1, bars[t].2);
        trs.push((h - l).max((h - pc).abs()).max((l - pc).abs()));
    }
    trs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let med = match trs.len() {
        0 => return None,
        len if len % 2 == 1 => trs[len / 2],
        len => (trs[len / 2 - 1] + trs[len / 2]) / 2.0,
    };
    if !(med.is_finite() && med > 0.0) {
        return None;
    }
    let mut ratio_sum = 0.0;
    let mut big = 0usize;
    for &(o, h, l, c) in bars {
        let top = o.max(c);
        let bot = o.min(c);
        let wick = (h - top).max(0.0) + (bot - l).max(0.0);
        let r = wick / med;
        ratio_sum += r;
        if r > PD_BIG_WICK {
            big += 1;
        }
    }
    let mean_ratio = ratio_sum / n as f64;
    let raw = mean_ratio * (1.0 + big as f64 / n as f64);
    raw.is_finite().then_some(raw)
}

/// Ascending percentile rank (0..100) of each `(key, value)` by value: the lowest
/// value → 0, the highest → 100, ties share the same rank. Pure + deterministic.
pub fn percentile_rank(mut values: Vec<(String, f64)>) -> Vec<(String, f64)> {
    values.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    let n = values.len();
    let mut out = Vec::with_capacity(n);
    let mut i = 0usize;
    while i < n {
        let mut j = i;
        while j < n && values[j].1 == values[i].1 {
            j += 1;
        }
        // Number strictly less than this run = i; share that percentile across ties.
        let pct = if n <= 1 { 0.0 } else { i as f64 / (n - 1) as f64 * 100.0 };
        for v in &values[i..j] {
            out.push((v.0.clone(), pct));
        }
        i = j;
    }
    out
}

/// Recompute the pump&dump score for every symbol with enough daily history and
/// store the raw metric + a DB-wide percentile rank in `fundamentals_cache`
/// (`pump_dump_raw`, `pump_dump_score`; 100 = most pump&dump-like). Owns only those
/// two columns. Returns the number of symbols scored.
pub fn recompute_pump_dump_scores(conn: &Connection) -> Result<usize> {
    // Most recent PD_WINDOW bars per symbol, oldest→newest.
    let rows: Vec<(String, f64, f64, f64, f64)> = {
        let mut stmt = conn.prepare(
            "SELECT symbol, open, high, low, close FROM (
                 SELECT symbol, open, high, low, close, date,
                        ROW_NUMBER() OVER (PARTITION BY symbol ORDER BY date DESC) AS rn
                 FROM daily_cache
                 WHERE open IS NOT NULL AND high IS NOT NULL
                   AND low IS NOT NULL AND close IS NOT NULL
             ) WHERE rn <= ?1 ORDER BY symbol ASC, date ASC",
        )?;
        let mapped = stmt.query_map([PD_WINDOW], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, f64>(1)?,
                r.get::<_, f64>(2)?,
                r.get::<_, f64>(3)?,
                r.get::<_, f64>(4)?,
            ))
        })?;
        mapped.collect::<Result<Vec<_>>>()?
    };

    // Fold per symbol → raw metric.
    let mut raws: Vec<(String, f64)> = Vec::new();
    let mut i = 0usize;
    while i < rows.len() {
        let sym = rows[i].0.clone();
        let mut bars: Vec<(f64, f64, f64, f64)> = Vec::new();
        while i < rows.len() && rows[i].0 == sym {
            bars.push((rows[i].1, rows[i].2, rows[i].3, rows[i].4));
            i += 1;
        }
        if let Some(raw) = pump_dump_raw(&bars) {
            raws.push((sym, raw));
        }
    }
    let raw_map: HashMap<String, f64> = raws.iter().cloned().collect();
    let ranked = percentile_rank(raws);

    conn.execute_batch("BEGIN")?;
    for (sym, score) in &ranked {
        let raw = raw_map.get(sym).copied();
        conn.execute(
            "INSERT INTO fundamentals_cache (symbol, pump_dump_raw, pump_dump_score, updated_at)
             VALUES (?1, ?2, ?3, datetime('now'))
             ON CONFLICT(symbol) DO UPDATE SET
                 pump_dump_raw=excluded.pump_dump_raw,
                 pump_dump_score=excluded.pump_dump_score,
                 updated_at=excluded.updated_at",
            params![sym, raw, score],
        )?;
    }
    conn.execute_batch("COMMIT")?;
    Ok(ranked.len())
}

// ─── Dilution snapshots + score (split-adjusted 12-month share growth) ────────

/// Upsert a batch of historical shares-outstanding snapshots (SEC XBRL frames).
/// `rows` = (symbol, period_end YYYY-MM-DD, shares_outstanding).
pub fn upsert_dilution_snapshots(conn: &Connection, rows: &[(String, String, f64)]) -> Result<usize> {
    conn.execute_batch("BEGIN")?;
    let mut n = 0usize;
    for (symbol, period_end, shares) in rows {
        conn.execute(
            "INSERT INTO dilution_snapshots (symbol, period_end, shares_outstanding, updated_at)
             VALUES (?1, ?2, ?3, datetime('now'))
             ON CONFLICT(symbol, period_end) DO UPDATE SET
                 shares_outstanding=excluded.shares_outstanding, updated_at=excluded.updated_at",
            params![symbol, period_end, shares],
        )?;
        n += 1;
    }
    conn.execute_batch("COMMIT")?;
    Ok(n)
}

/// Split-adjusted 12-month dilution percentage. `split_factor` = ∏(to/from) over
/// the splits that went ex AFTER the past snapshot (so the past share count is
/// rescaled to today's basis before comparison): a 1:10 reverse split has
/// factor 0.1, a 4-for-1 forward split has factor 4. None on degenerate input.
pub fn dilution_pct(shares_now: f64, shares_past_raw: f64, split_factor: f64) -> Option<f64> {
    let past_adj = shares_past_raw * split_factor;
    if !(past_adj.is_finite() && past_adj > 0.0 && shares_now.is_finite() && shares_now > 0.0) {
        return None;
    }
    let pct = (shares_now - past_adj) / past_adj * 100.0;
    pct.is_finite().then_some(pct)
}

/// Recompute the 12-month split-adjusted dilution % + a DB-wide percentile rank
/// (`dilution_pct_12m`, `dilution_score`, `shares_outstanding_12m`; 100 = most
/// dilutive). Reads current shares from `fundamentals_cache.outstanding_shares`
/// (fallback: latest snapshot), the ~12-month-ago snapshot from
/// `dilution_snapshots`, and neutralises splits via `ticker_splits`. Returns the
/// number of symbols scored.
pub fn recompute_dilution_scores(conn: &Connection, today: &str) -> Result<usize> {
    // Reference window: a snapshot is a valid "~12 months ago" point when its
    // period_end is between 9 and 15 months old; we take the most recent such one.
    let ref_newest = shift_days(today, -270); // ≤ 9 months old is too recent
    let ref_oldest = shift_days(today, -460); // ≥ ~15 months old is too stale

    // current shares (Massive outstanding).
    let shares_now: HashMap<String, f64> = {
        let mut stmt = conn.prepare(
            "SELECT symbol, outstanding_shares FROM fundamentals_cache
             WHERE outstanding_shares IS NOT NULL AND outstanding_shares > 0",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as f64)))?;
        rows.collect::<Result<Vec<_>>>()?.into_iter().collect()
    };

    // all snapshots per symbol (period_end, shares), ascending.
    let mut snaps: HashMap<String, Vec<(String, f64)>> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT symbol, period_end, shares_outstanding FROM dilution_snapshots
             ORDER BY symbol ASC, period_end ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, f64>(2)?))
        })?;
        for row in rows {
            let (sym, end, sh) = row?;
            snaps.entry(sym).or_default().push((end, sh));
        }
    }

    // all splits per symbol (ex_date, from, to), ascending.
    let mut splits: HashMap<String, Vec<(String, f64, f64)>> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT symbol, ex_date, from_factor, to_factor FROM ticker_splits
             WHERE from_factor IS NOT NULL AND to_factor IS NOT NULL
             ORDER BY symbol ASC, ex_date ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, f64>(2)?, r.get::<_, f64>(3)?))
        })?;
        for row in rows {
            let (sym, ex, from, to) = row?;
            splits.entry(sym).or_default().push((ex, from, to));
        }
    }

    // Cumulative split factor (∏ to/from) for splits ex-dated strictly after `after`.
    let factor_after = |sym: &str, after: &str| -> f64 {
        splits
            .get(sym)
            .map(|evs| {
                evs.iter()
                    .filter(|(ex, _, _)| ex.as_str() > after)
                    .filter(|(_, from, to)| *from > 0.0 && *to > 0.0)
                    .fold(1.0, |acc, (_, from, to)| acc * (to / from))
            })
            .unwrap_or(1.0)
    };

    // Compute dilution % per symbol that has both endpoints.
    let mut raws: Vec<(String, f64)> = Vec::new();
    let mut past_adj_map: HashMap<String, f64> = HashMap::new();
    for (sym, series) in &snaps {
        // pick the most recent snapshot inside the [ref_oldest, ref_newest] window.
        let Some((ref_end, ref_shares)) = series
            .iter()
            .rev()
            .find(|(end, _)| end.as_str() >= ref_oldest.as_str() && end.as_str() <= ref_newest.as_str())
        else {
            continue;
        };
        // current shares (today's basis): prefer Massive's current outstanding, else
        // the latest snapshot rescaled for any split that went ex after it.
        let now = shares_now.get(sym).copied().or_else(|| {
            series.last().map(|(end, s)| *s * factor_after(sym, end))
        });
        let Some(now) = now else { continue };

        // past shares rescaled to today's basis: ref shares × splits after ref_end.
        let factor = factor_after(sym, ref_end);
        if let Some(pct) = dilution_pct(now, *ref_shares, factor) {
            raws.push((sym.clone(), pct));
            past_adj_map.insert(sym.clone(), *ref_shares * factor);
        }
    }

    let raw_map: HashMap<String, f64> = raws.iter().cloned().collect();
    let ranked = percentile_rank(raws);

    conn.execute_batch("BEGIN")?;
    for (sym, score) in &ranked {
        let pct = raw_map.get(sym).copied();
        let past_adj = past_adj_map.get(sym).copied();
        conn.execute(
            "INSERT INTO fundamentals_cache
                 (symbol, dilution_pct_12m, dilution_score, shares_outstanding_12m, updated_at)
             VALUES (?1, ?2, ?3, ?4, datetime('now'))
             ON CONFLICT(symbol) DO UPDATE SET
                 dilution_pct_12m=excluded.dilution_pct_12m,
                 dilution_score=excluded.dilution_score,
                 shares_outstanding_12m=excluded.shares_outstanding_12m,
                 updated_at=excluded.updated_at",
            params![sym, pct, score, past_adj],
        )?;
    }
    conn.execute_batch("COMMIT")?;
    Ok(ranked.len())
}

/// Shift a `YYYY-MM-DD` date by `days` (negative = earlier). Falls back to the
/// input on parse failure.
fn shift_days(date: &str, days: i64) -> String {
    chrono::NaiveDate::parse_from_str(&date[..date.len().min(10)], "%Y-%m-%d")
        .map(|d| (d + chrono::Duration::days(days)).format("%Y-%m-%d").to_string())
        .unwrap_or_else(|_| date.to_string())
}

// ─── Ticker splits (bulk corporate actions) ───────────────────────────────────

/// One split event row to persist.
#[derive(Debug, Clone)]
pub struct SplitRow {
    pub symbol: String,
    pub ex_date: String,
    pub label: String,
    pub from_factor: f64,
    pub to_factor: f64,
}

/// Replace the whole `ticker_splits` table with `rows` (atomic). The table holds a
/// rolling recent window rebuilt each day, so a full replace keeps it from
/// accumulating stale events.
pub fn replace_ticker_splits(conn: &Connection, rows: &[SplitRow]) -> Result<usize> {
    conn.execute_batch("BEGIN")?;
    conn.execute("DELETE FROM ticker_splits", [])?;
    for r in rows {
        conn.execute(
            "INSERT OR REPLACE INTO ticker_splits
                 (symbol, ex_date, label, from_factor, to_factor, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))",
            params![r.symbol, r.ex_date, r.label, r.from_factor, r.to_factor],
        )?;
    }
    conn.execute_batch("COMMIT")?;
    Ok(rows.len())
}

/// Stored split events for one symbol since `since_date` (YYYY-MM-DD), ex_date
/// DESC: `(ex_date, label)`. Offline source for the daily chart's split markers in
/// flat-files mode (the live path hits Alpaca corporate-actions).
pub fn splits_for_symbol(conn: &Connection, symbol: &str, since_date: &str) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT ex_date, label FROM ticker_splits
         WHERE symbol = ?1 AND ex_date >= ?2 ORDER BY ex_date DESC",
    )?;
    let rows = stmt.query_map(params![symbol, since_date], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    rows.collect()
}

/// Roll up `ticker_splits` into the `fundamentals_cache` display columns:
/// `last_split_date`, `last_split_label`, `split_count_1y` (splits in the last 365
/// days). Resets all rows first so an aged-out split doesn't linger.
pub fn recompute_split_rollups(conn: &Connection, one_year_ago: &str) -> Result<usize> {
    conn.execute_batch("BEGIN")?;
    conn.execute(
        "UPDATE fundamentals_cache SET last_split_date=NULL, last_split_label=NULL, split_count_1y=0",
        [],
    )?;
    let agg: Vec<(String, String, Option<String>, i64)> = {
        let mut stmt = conn.prepare(
            "SELECT symbol,
                    MAX(ex_date) AS last_date,
                    (SELECT label FROM ticker_splits t2
                      WHERE t2.symbol = t.symbol ORDER BY ex_date DESC LIMIT 1) AS last_label,
                    SUM(CASE WHEN ex_date >= ?1 THEN 1 ELSE 0 END) AS count_1y
             FROM ticker_splits t GROUP BY symbol",
        )?;
        let rows = stmt.query_map(params![one_year_ago], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?, r.get::<_, i64>(3)?))
        })?;
        rows.collect::<Result<Vec<_>>>()?
    };
    let mut n = 0usize;
    for (sym, last_date, last_label, count_1y) in &agg {
        conn.execute(
            "INSERT INTO fundamentals_cache
                 (symbol, last_split_date, last_split_label, split_count_1y, updated_at)
             VALUES (?1, ?2, ?3, ?4, datetime('now'))
             ON CONFLICT(symbol) DO UPDATE SET
                 last_split_date=excluded.last_split_date,
                 last_split_label=excluded.last_split_label,
                 split_count_1y=excluded.split_count_1y,
                 updated_at=excluded.updated_at",
            params![sym, last_date, last_label, count_1y],
        )?;
        n += 1;
    }
    conn.execute_batch("COMMIT")?;
    Ok(n)
}

// ─── Risk scores (absolute per-ticker, 0..100; None when inputs missing) ──────
// These are NOT DB-wide percentiles — each is a deterministic formula over data
// already collected (SEC filings / financials / short interest). Missing inputs
// yield None (never an invented value), so the UI shows "—"/unknown.

fn clamp01(x: f64) -> f64 {
    x.max(0.0).min(1.0)
}

/// Inclusive day count `today − date` for two YYYY-MM-DD strings; None on parse fail.
fn days_since(today: &str, date: &str) -> Option<i64> {
    let t = chrono::NaiveDate::parse_from_str(&today[..today.len().min(10)], "%Y-%m-%d").ok()?;
    let d = chrono::NaiveDate::parse_from_str(&date[..date.len().min(10)], "%Y-%m-%d").ok()?;
    Some((t - d).num_days())
}

/// "Capacité à diluer" — legal/filing readiness to dilute fast, from the SEC
/// dilution section already stored. `collected` = the dilution section was actually
/// fetched for this ticker (else None = unknown, not 0). Recency of the latest
/// filing scales the raw score.
#[allow(clippy::too_many_arguments)]
pub fn dilution_capacity_score(
    collected: bool,
    has_recent_shelf: bool,
    latest_form: Option<&str>,
    latest_date: Option<&str>,
    atm: bool,
    resale: bool,
    warrants: bool,
    offering_amount: Option<f64>,
    market_cap: Option<f64>,
    today: &str,
) -> Option<f64> {
    if !collected {
        return None; // never collected → unknown
    }
    let mut score: f64 = 0.0;
    if has_recent_shelf {
        score += 20.0;
    }
    if let Some(f) = latest_form {
        score += match f.trim().to_uppercase().as_str() {
            "S-3" | "S-3/A" => 15.0,
            "S-3ASR" => 25.0,
            "EFFECT" | "POS AM" | "POSASR" => 20.0,
            "424B3" | "424B5" | "424B7" => 35.0,
            _ => 0.0,
        };
    }
    if atm {
        score += 20.0;
    }
    if resale {
        score += 10.0;
    }
    if warrants {
        score += 10.0;
    }
    if let (Some(off), Some(mc)) = (offering_amount, market_cap) {
        if mc > 0.0 {
            let ratio = off / mc;
            if ratio >= 0.30 {
                score += 20.0;
            } else if ratio >= 0.10 {
                score += 12.0;
            } else if ratio >= 0.03 {
                score += 6.0;
            }
        }
    }
    // Recency multiplier from the latest filing date (no date → leave score as-is).
    let mult: f64 = match latest_date.and_then(|d| days_since(today, d)) {
        Some(days) if days <= 7 => 1.2,
        Some(days) if days <= 30 => 1.0,
        Some(days) if days <= 180 => 0.75,
        Some(days) if days <= 365 => 0.5,
        Some(_) => 0.25,
        None => 1.0,
    };
    Some((score * mult).clamp(0.0, 100.0))
}

/// "Besoin de diluer" — apparent need for cash, from the financial-health section.
/// `collected` = the financials section was fetched (else None = unknown).
#[allow(clippy::too_many_arguments)]
pub fn dilution_need_score(
    collected: bool,
    net_income_last_q: Option<f64>,
    net_income_ttm: Option<f64>,
    negative_quarters_last4: Option<i64>,
    operating_cash_flow_ttm: Option<f64>,
    cash_and_equivalents: Option<f64>,
    market_cap: Option<f64>,
) -> Option<f64> {
    if !collected {
        return None;
    }
    let mut score: f64 = 0.0;
    if net_income_ttm.map(|v| v < 0.0).unwrap_or(false) {
        score += 20.0;
    }
    if net_income_last_q.map(|v| v < 0.0).unwrap_or(false) {
        score += 10.0;
    }
    if let Some(nq) = negative_quarters_last4 {
        score += match nq {
            1 => 5.0,
            2 => 10.0,
            3 => 15.0,
            n if n >= 4 => 25.0,
            _ => 0.0,
        };
    }
    if operating_cash_flow_ttm.map(|v| v < 0.0).unwrap_or(false) {
        score += 20.0;
    }
    // Cash runway (only meaningful with a negative operating cash flow).
    if let (Some(ocf), Some(cash)) = (operating_cash_flow_ttm, cash_and_equivalents) {
        if ocf < 0.0 && cash >= 0.0 {
            let quarterly_burn = ocf.abs() / 4.0;
            if quarterly_burn > 0.0 {
                let runway = cash / quarterly_burn;
                if runway < 2.0 {
                    score += 25.0;
                } else if runway < 4.0 {
                    score += 18.0;
                } else if runway < 8.0 {
                    score += 10.0;
                }
            }
        }
    }
    // Cash thin vs market cap (optional booster).
    if let (Some(cash), Some(mc)) = (cash_and_equivalents, market_cap) {
        if mc > 0.0 {
            let r = cash / mc;
            if r < 0.05 {
                score += 10.0;
            } else if r < 0.10 {
                score += 5.0;
            }
        }
    }
    Some(score.clamp(0.0, 100.0))
}

/// "Short interest score" — short crowding / squeeze fuel. short%float weighted 70,
/// days-to-cover weighted 30. None when neither component is computable; a single
/// missing component contributes 0 rather than inventing a value.
pub fn short_interest_score(
    short_interest: Option<i64>,
    float_shares: Option<i64>,
    days_to_cover: Option<f64>,
) -> Option<f64> {
    let sf = match (short_interest, float_shares) {
        (Some(si), Some(fl)) if fl > 0 && si >= 0 => {
            let pct = si as f64 / fl as f64 * 100.0;
            Some(clamp01((pct - 5.0) / (30.0 - 5.0)) * 70.0)
        }
        _ => None,
    };
    let dtc = days_to_cover.map(|d| clamp01((d - 1.0) / (7.0 - 1.0)) * 30.0);
    match (sf, dtc) {
        (None, None) => None,
        _ => Some((sf.unwrap_or(0.0) + dtc.unwrap_or(0.0)).clamp(0.0, 100.0)),
    }
}

/// Recompute the three absolute risk scores for every tradable ticker from data
/// already in `universe_assets` + `company_intel`, storing them in
/// `fundamentals_cache` (owns only those three columns). Run AFTER `compute_universe`
/// (needs market_cap / float) and after short-interest collection. Returns rows
/// written.
pub fn recompute_risk_scores(conn: &Connection, today: &str) -> Result<usize> {
    struct Row {
        symbol: String,
        market_cap: Option<f64>,
        float_shares: Option<i64>,
        has_recent_shelf: bool,
        latest_form: Option<String>,
        latest_date: Option<String>,
        dilution_flags: Option<String>,
        dilution_collected: bool,
        net_income_last_q: Option<f64>,
        net_income_ttm: Option<f64>,
        negative_quarters_last4: Option<i64>,
        operating_cash_flow_ttm: Option<f64>,
        cash_and_equivalents: Option<f64>,
        financials_collected: bool,
        short_interest: Option<i64>,
        days_to_cover: Option<f64>,
    }

    let rows: Vec<Row> = {
        let mut stmt = conn.prepare(
            "SELECT u.symbol, u.market_cap, u.float_shares,
                    ci.has_recent_shelf, ci.latest_dilution_form, ci.latest_dilution_date,
                    ci.dilution_flags, ci.dilution_updated_at,
                    ci.net_income_last_q, ci.net_income_ttm, ci.negative_quarters_last4,
                    ci.operating_cash_flow_ttm, ci.cash_and_equivalents, ci.financials_updated_at,
                    ci.short_interest, ci.days_to_cover
             FROM universe_assets u
             LEFT JOIN company_intel ci ON ci.symbol = u.symbol
             WHERE u.tradable = 1",
        )?;
        let mapped = stmt.query_map([], |r| {
            Ok(Row {
                symbol: r.get(0)?,
                market_cap: r.get::<_, Option<i64>>(1)?.map(|v| v as f64),
                float_shares: r.get(2)?,
                has_recent_shelf: r.get::<_, Option<i64>>(3)?.unwrap_or(0) != 0,
                latest_form: r.get(4)?,
                latest_date: r.get(5)?,
                dilution_flags: r.get(6)?,
                dilution_collected: r.get::<_, Option<String>>(7)?.is_some(),
                net_income_last_q: r.get(8)?,
                net_income_ttm: r.get(9)?,
                negative_quarters_last4: r.get(10)?,
                operating_cash_flow_ttm: r.get(11)?,
                cash_and_equivalents: r.get(12)?,
                financials_collected: r.get::<_, Option<String>>(13)?.is_some(),
                short_interest: r.get(14)?,
                days_to_cover: r.get(15)?,
            })
        })?;
        mapped.collect::<Result<Vec<_>>>()?
    };

    conn.execute_batch("BEGIN")?;
    let mut n = 0usize;
    for row in &rows {
        // Parse the dilution flags JSON ({atm,resale,warrants,offering_amount}).
        let (atm, resale, warrants, offering) = row
            .dilution_flags
            .as_deref()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
            .map(|v| {
                (
                    v.get("atm").and_then(|x| x.as_bool()).unwrap_or(false),
                    v.get("resale").and_then(|x| x.as_bool()).unwrap_or(false),
                    v.get("warrants").and_then(|x| x.as_bool()).unwrap_or(false),
                    v.get("offering_amount").and_then(|x| x.as_f64()),
                )
            })
            .unwrap_or((false, false, false, None));

        let capacity = dilution_capacity_score(
            row.dilution_collected,
            row.has_recent_shelf,
            row.latest_form.as_deref(),
            row.latest_date.as_deref(),
            atm,
            resale,
            warrants,
            offering,
            row.market_cap,
            today,
        );
        let need = dilution_need_score(
            row.financials_collected,
            row.net_income_last_q,
            row.net_income_ttm,
            row.negative_quarters_last4,
            row.operating_cash_flow_ttm,
            row.cash_and_equivalents,
            row.market_cap,
        );
        let short = short_interest_score(row.short_interest, row.float_shares, row.days_to_cover);

        conn.execute(
            "INSERT INTO fundamentals_cache
                 (symbol, dilution_capacity_score, dilution_need_score, short_interest_score, updated_at)
             VALUES (?1, ?2, ?3, ?4, datetime('now'))
             ON CONFLICT(symbol) DO UPDATE SET
                 dilution_capacity_score=excluded.dilution_capacity_score,
                 dilution_need_score=excluded.dilution_need_score,
                 short_interest_score=excluded.short_interest_score,
                 updated_at=excluded.updated_at",
            params![row.symbol, capacity, need, short],
        )?;
        n += 1;
    }
    conn.execute_batch("COMMIT")?;
    Ok(n)
}

/// Recompute ONLY the `dilution_capacity_score` for a single symbol (used by the
/// on-demand collector after it refreshes that ticker's SEC dilution section). Reads
/// the row's dilution inputs + market cap; writes nothing else.
pub fn recompute_capacity_for_symbol(conn: &Connection, symbol: &str, today: &str) -> Result<()> {
    use rusqlite::OptionalExtension;
    let row = conn
        .query_row(
            "SELECT u.market_cap, ci.has_recent_shelf, ci.latest_dilution_form,
                    ci.latest_dilution_date, ci.dilution_flags, ci.dilution_updated_at
             FROM universe_assets u
             LEFT JOIN company_intel ci ON ci.symbol = u.symbol
             WHERE u.symbol = ?1",
            params![symbol],
            |r| {
                Ok((
                    r.get::<_, Option<i64>>(0)?.map(|v| v as f64),
                    r.get::<_, Option<i64>>(1)?.unwrap_or(0) != 0,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, Option<String>>(5)?.is_some(),
                ))
            },
        )
        .optional()?;
    let Some((market_cap, has_shelf, form, date, flags, collected)) = row else {
        return Ok(());
    };
    let (atm, resale, warrants, offering) = flags
        .as_deref()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .map(|v| {
            (
                v.get("atm").and_then(|x| x.as_bool()).unwrap_or(false),
                v.get("resale").and_then(|x| x.as_bool()).unwrap_or(false),
                v.get("warrants").and_then(|x| x.as_bool()).unwrap_or(false),
                v.get("offering_amount").and_then(|x| x.as_f64()),
            )
        })
        .unwrap_or((false, false, false, None));
    let score = dilution_capacity_score(
        collected, has_shelf, form.as_deref(), date.as_deref(),
        atm, resale, warrants, offering, market_cap, today,
    );
    conn.execute(
        "INSERT INTO fundamentals_cache (symbol, dilution_capacity_score, updated_at)
         VALUES (?1, ?2, datetime('now'))
         ON CONFLICT(symbol) DO UPDATE SET
             dilution_capacity_score=excluded.dilution_capacity_score,
             updated_at=excluded.updated_at",
        params![symbol, score],
    )?;
    Ok(())
}

// ─── Daily bars ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyBar {
    pub symbol: String,
    pub date: String,
    pub open: Option<f64>,
    pub high: Option<f64>,
    pub low: Option<f64>,
    pub close: Option<f64>,
    pub volume: Option<i64>,
    pub updated_at: String,
}

pub fn upsert_daily_bar(conn: &Connection, bar: &DailyBar) -> Result<()> {
    conn.execute(
        "INSERT INTO daily_cache (symbol,date,open,high,low,close,volume,updated_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
         ON CONFLICT(symbol,date) DO UPDATE SET
             open=excluded.open, high=excluded.high, low=excluded.low,
             close=excluded.close, volume=excluded.volume,
             updated_at=excluded.updated_at",
        params![bar.symbol, bar.date, bar.open, bar.high, bar.low, bar.close, bar.volume, bar.updated_at],
    )?;
    Ok(())
}

/// Latest cached daily close per symbol — used to seed `previous_close` so live
/// change% is meaningful from the first trade.
pub fn latest_closes(conn: &Connection) -> Result<Vec<(String, f64)>> {
    let mut stmt = conn.prepare(
        "SELECT d.symbol, d.close FROM daily_cache d
         WHERE d.date = (SELECT MAX(x.date) FROM daily_cache x WHERE x.symbol = d.symbol)
           AND d.close IS NOT NULL",
    )?;
    let rows = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?)))?;
    rows.collect()
}

/// Latest cached daily VOLUME per symbol STRICTLY BEFORE `before_date`
/// (YYYY-MM-DD) — i.e. the previous trading day's volume relative to that date.
/// Mirrors `latest_closes`. Used by the mean-reversion scoring to gate, tie-break
/// and display. The date bound makes the query correct during a Market Replay
/// (the cache holds bars AFTER the replayed day, which must not leak); in live
/// mode `before_date` = today, which the pre-open cache never contains anyway.
pub fn latest_volumes(conn: &Connection, before_date: &str) -> Result<Vec<(String, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT d.symbol, d.volume FROM daily_cache d
         WHERE d.date = (SELECT MAX(x.date) FROM daily_cache x
                         WHERE x.symbol = d.symbol AND x.date < ?1)
           AND d.date < ?1
           AND d.volume IS NOT NULL",
    )?;
    let rows = stmt.query_map(params![before_date], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    rows.collect()
}

/// Most recent bar date (YYYY-MM-DD) cached across all symbols, or None when the
/// daily cache is empty. Used to fetch only the missing days on later startups.
pub fn latest_bar_date(conn: &Connection) -> Result<Option<String>> {
    conn.query_row("SELECT MAX(date) FROM daily_cache", [], |r| {
        r.get::<_, Option<String>>(0)
    })
}

/// Oldest bar date (YYYY-MM-DD) cached across all symbols, or None when empty.
/// Used to detect a shallow cache that needs a deeper historical backfill (the
/// mean-reversion scoring needs ~3 years), independent of how recent the newest
/// bar is.
pub fn earliest_bar_date(conn: &Connection) -> Result<Option<String>> {
    conn.query_row("SELECT MIN(date) FROM daily_cache", [], |r| {
        r.get::<_, Option<String>>(0)
    })
}

/// Count of distinct symbols that have at least one cached daily bar.
pub fn symbols_with_bars(conn: &Connection) -> Result<i64> {
    conn.query_row("SELECT COUNT(DISTINCT symbol) FROM daily_cache", [], |r| r.get(0))
}

/// Delete every cached daily bar for one symbol. Used when a split invalidates
/// the symbol's split-adjusted history (adjustment=split rescales the whole
/// series to the latest factor) so it can be refetched in full at the new scale.
pub fn delete_symbol_bars(conn: &Connection, symbol: &str) -> Result<usize> {
    let n = conn.execute("DELETE FROM daily_cache WHERE symbol = ?1", params![symbol])?;
    Ok(n)
}

/// Drop bars older than `cutoff_date` (YYYY-MM-DD) to bound the cache to the
/// recent window we actually use (~50 trading days).
pub fn prune_before(conn: &Connection, cutoff_date: &str) -> Result<usize> {
    let n = conn.execute("DELETE FROM daily_cache WHERE date < ?1", params![cutoff_date])?;
    Ok(n)
}

/// Average daily volume per symbol over the most recent `days` cached bars
/// (default trading-volume window = 20 days). Computed in SQL so it stays
/// correct whether the last fetch was a full 50-day load or an incremental one.
pub fn avg_volumes(conn: &Connection, days: u32) -> Result<Vec<(String, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT symbol, CAST(AVG(volume) AS INTEGER) FROM (
             SELECT symbol, volume,
                    ROW_NUMBER() OVER (PARTITION BY symbol ORDER BY date DESC) AS rn
             FROM daily_cache WHERE volume IS NOT NULL
         ) WHERE rn <= ?1 GROUP BY symbol",
    )?;
    let rows = stmt.query_map([days], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    rows.collect()
}

/// Average daily DOLLAR volume per symbol over the most recent `days` cached bars
/// (close × volume, averaged). Computed in SQL alongside `avg_volumes`. Used by
/// the Panic Mean Reversion premarket pre-filter (the "avg $ volume 20d > 5M"
/// liquidity branch). Symbols with no usable bars are omitted.
/// Window bounded to bars dated strictly before `before_date` (Market Replay
/// correctness — see `latest_volumes`).
pub fn avg_dollar_volumes(
    conn: &Connection,
    days: u32,
    before_date: &str,
) -> Result<Vec<(String, f64)>> {
    let mut stmt = conn.prepare(
        "SELECT symbol, AVG(close * volume) FROM (
             SELECT symbol, close, volume,
                    ROW_NUMBER() OVER (PARTITION BY symbol ORDER BY date DESC) AS rn
             FROM daily_cache
             WHERE volume IS NOT NULL AND close IS NOT NULL AND date < ?2
         ) WHERE rn <= ?1 GROUP BY symbol",
    )?;
    let rows = stmt.query_map(params![days, before_date], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
    })?;
    rows.collect()
}

/// Date-ASCENDING daily closes for one symbol, most recent `limit` bars. Used by
/// the mean-reversion scoring engine (which wants oldest→newest series). Skips
/// rows with a NULL close.
pub fn closes_ascending(conn: &Connection, symbol: &str, limit: u32) -> Result<Vec<f64>> {
    let mut stmt = conn.prepare(
        "SELECT close FROM (
             SELECT close, date FROM daily_cache
             WHERE symbol = ?1 AND close IS NOT NULL
             ORDER BY date DESC LIMIT ?2
         ) ORDER BY date ASC",
    )?;
    let rows = stmt.query_map(params![symbol, limit], |row| row.get::<_, f64>(0))?;
    rows.collect()
}

/// Date-ASCENDING daily OHLCV for one symbol, most recent `limit` bars. Mirrors
/// `closes_ascending` but returns the full bar (open, high, low, close, volume) so
/// the mean-reversion scoring can compute candle colour (close vs open), daily true
/// range (parabolic expansion) and dollar volume. Rows with any NULL OHLC are
/// skipped (a missing close already excludes the bar); a NULL volume becomes 0.
/// Window bounded to bars dated strictly before `before_date` (Market Replay
/// correctness — see `latest_volumes`).
pub fn ohlcv_ascending(
    conn: &Connection,
    symbol: &str,
    limit: u32,
    before_date: &str,
) -> Result<Vec<(f64, f64, f64, f64, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT open, high, low, close, volume FROM (
             SELECT open, high, low, close, volume, date FROM daily_cache
             WHERE symbol = ?1
               AND date < ?3
               AND open IS NOT NULL AND high IS NOT NULL
               AND low IS NOT NULL AND close IS NOT NULL
             ORDER BY date DESC LIMIT ?2
         ) ORDER BY date ASC",
    )?;
    let rows = stmt.query_map(params![symbol, limit, before_date], |row| {
        Ok((
            row.get::<_, f64>(0)?,
            row.get::<_, f64>(1)?,
            row.get::<_, f64>(2)?,
            row.get::<_, f64>(3)?,
            row.get::<_, Option<i64>>(4)?.unwrap_or(0),
        ))
    })?;
    rows.collect()
}

/// Most recent `limit` daily bars dated strictly BEFORE `before_date`
/// (YYYY-MM-DD), date-descending. The replay-safe variant of `get_daily_bars`:
/// callers pass the app-clock "today" so a Market Replay never reads bars from
/// the simulated future (in live mode the bound is inert — the cache holds
/// nothing for today pre-open).
pub fn get_daily_bars_before(
    conn: &Connection,
    symbol: &str,
    before_date: &str,
    limit: u32,
) -> Result<Vec<DailyBar>> {
    let mut stmt = conn.prepare(
        "SELECT symbol,date,open,high,low,close,volume,updated_at
         FROM daily_cache WHERE symbol=?1 AND date < ?2 ORDER BY date DESC LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![symbol, before_date, limit], |row| {
        Ok(DailyBar {
            symbol: row.get(0)?,
            date: row.get(1)?,
            open: row.get(2)?,
            high: row.get(3)?,
            low: row.get(4)?,
            close: row.get(5)?,
            volume: row.get(6)?,
            updated_at: row.get(7)?,
        })
    })?;
    rows.collect()
}

pub fn get_daily_bars(conn: &Connection, symbol: &str, limit: u32) -> Result<Vec<DailyBar>> {
    let mut stmt = conn.prepare(
        "SELECT symbol,date,open,high,low,close,volume,updated_at
         FROM daily_cache WHERE symbol=?1 ORDER BY date DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![symbol, limit], |row| {
        Ok(DailyBar {
            symbol: row.get(0)?,
            date: row.get(1)?,
            open: row.get(2)?,
            high: row.get(3)?,
            low: row.get(4)?,
            close: row.get(5)?,
            volume: row.get(6)?,
            updated_at: row.get(7)?,
        })
    })?;
    rows.collect()
}

#[cfg(test)]
mod scoring_tests {
    use super::*;

    /// A calm series: tiny wicks, modest gaps → low pump&dump raw.
    fn calm_series(n: usize) -> Vec<(f64, f64, f64, f64)> {
        (0..n)
            .map(|i| {
                let c = 100.0 + (i as f64) * 0.05;
                // open≈close, high/low hug the body (wick ≈ 1c).
                (c - 0.01, c + 0.02, c - 0.02, c)
            })
            .collect()
    }

    /// A pump&dump series: a calm baseline with periodic huge-wick spike bars (the
    /// real footprint — most days normal, occasional violent pump-then-dump).
    fn spiky_series(n: usize) -> Vec<(f64, f64, f64, f64)> {
        (0..n)
            .map(|i| {
                let c = 5.0 + (i as f64) * 0.01;
                if i % 5 == 0 {
                    // spike bar: enormous upper + lower wick around a flat body.
                    (c, c + 4.0, c - 3.5, c)
                } else {
                    // calm bar: tight range, tiny wicks.
                    (c - 0.01, c + 0.02, c - 0.02, c)
                }
            })
            .collect()
    }

    #[test]
    fn pump_dump_rewards_big_frequent_wicks() {
        let calm = pump_dump_raw(&calm_series(60)).expect("calm scored");
        let spiky = pump_dump_raw(&spiky_series(60)).expect("spiky scored");
        assert!(spiky > calm * 5.0, "spiky {spiky} should dwarf calm {calm}");
    }

    #[test]
    fn pump_dump_needs_min_history() {
        assert!(pump_dump_raw(&calm_series(PD_MIN_BARS - 1)).is_none());
        assert!(pump_dump_raw(&calm_series(PD_MIN_BARS)).is_some());
    }

    #[test]
    fn percentile_rank_is_monotonic_and_handles_ties() {
        let ranked = percentile_rank(vec![
            ("low".into(), 1.0),
            ("mid_a".into(), 5.0),
            ("mid_b".into(), 5.0),
            ("high".into(), 9.0),
        ]);
        let m: HashMap<String, f64> = ranked.into_iter().collect();
        assert_eq!(m["low"], 0.0); // lowest → 0
        assert_eq!(m["high"], 100.0); // highest → 100
        assert_eq!(m["mid_a"], m["mid_b"]); // ties share a rank
        assert!(m["mid_a"] > m["low"] && m["mid_a"] < m["high"]);
    }

    #[test]
    fn dilution_pct_plain_growth() {
        // 50M → 100M shares, no split → +100% dilution.
        assert_eq!(dilution_pct(100_000_000.0, 50_000_000.0, 1.0), Some(100.0));
    }

    #[test]
    fn dilution_pct_neutralises_reverse_split() {
        // 1:10 reverse split (factor 0.1). Past 500M shares became 50M post-split;
        // current is 55M → a real +10% dilution, NOT a fake −89% drop.
        let pct = dilution_pct(55_000_000.0, 500_000_000.0, 0.1).unwrap();
        assert!((pct - 10.0).abs() < 1e-6, "expected ~+10%, got {pct}");
    }

    #[test]
    fn dilution_pct_neutralises_forward_split() {
        // 4-for-1 forward split (factor 4). Past 10M became 40M; current 40M → 0%.
        let pct = dilution_pct(40_000_000.0, 10_000_000.0, 4.0).unwrap();
        assert!(pct.abs() < 1e-6, "expected ~0%, got {pct}");
    }

    #[test]
    fn atr_from_hlc_includes_gaps() {
        // 15 flat bars + 1 gap bar; ATR must reflect the gap (|H−Cprev| ≫ H−L).
        let mut bars: Vec<(f64, f64, f64)> = (0..15).map(|_| (10.2, 9.8, 10.0)).collect();
        bars.push((12.2, 11.9, 12.0)); // gap up
        let atr = atr_from_hlc(&bars, 14).expect("atr defined");
        assert!(atr > 0.4, "ATR should reflect the gap, got {atr}");
    }

    #[test]
    fn capacity_unknown_when_not_collected() {
        // No SEC dilution section collected → unknown (None), not 0.
        let s = dilution_capacity_score(false, false, None, None, false, false, false, None, None, "2026-06-17");
        assert_eq!(s, None);
        // Collected but nothing found → low capacity = 0, not None.
        let s = dilution_capacity_score(true, false, None, None, false, false, false, None, None, "2026-06-17");
        assert_eq!(s, Some(0.0));
    }

    #[test]
    fn capacity_high_for_fresh_atm_prospectus() {
        // Recent 424B5 (+35) + shelf (+20) + ATM (+20) + offering 40% of mkt cap (+20)
        // = 95, ×1.2 recency (filed today) → clamped to 100.
        let s = dilution_capacity_score(
            true, true, Some("424B5"), Some("2026-06-15"),
            true, false, false, Some(40_000_000.0), Some(100_000_000.0), "2026-06-17",
        )
        .unwrap();
        assert!(s >= 95.0, "fresh ATM prospectus should be very high, got {s}");
        // The same filing aged >1y collapses via the 0.25 recency multiplier.
        let old = dilution_capacity_score(
            true, true, Some("424B5"), Some("2024-01-01"),
            true, false, false, Some(40_000_000.0), Some(100_000_000.0), "2026-06-17",
        )
        .unwrap();
        assert!(old < s && old > 0.0, "aged filing should score lower, got {old}");
    }

    #[test]
    fn need_unknown_vs_distressed() {
        assert_eq!(
            dilution_need_score(false, None, None, None, None, None, None),
            None
        );
        // Loss TTM (+20) + loss last Q (+10) + 4 neg quarters (+25) + negative OCF
        // (+20) + <2q runway (+25) → ≥100.
        let s = dilution_need_score(
            true, Some(-1.0), Some(-4.0), Some(4), Some(-8_000_000.0), Some(1_000_000.0), Some(50_000_000.0),
        )
        .unwrap();
        assert!(s >= 90.0, "distressed burner should be very high, got {s}");
        // Profitable, cash-rich → 0.
        let healthy = dilution_need_score(
            true, Some(5.0), Some(20.0), Some(0), Some(15_000_000.0), Some(500_000_000.0), Some(1_000_000_000.0),
        )
        .unwrap();
        assert_eq!(healthy, 0.0);
    }

    #[test]
    fn short_interest_score_weights_and_missing() {
        // 30% short float (saturates sf=70) + 7 days to cover (saturates dtc=30) → 100.
        let s = short_interest_score(Some(30_000_000), Some(100_000_000), Some(7.0)).unwrap();
        assert!((s - 100.0).abs() < 1e-6, "max crowding → 100, got {s}");
        // Below the 5% / 1-day floors → 0 (clamped), not negative.
        let low = short_interest_score(Some(1_000_000), Some(100_000_000), Some(0.5)).unwrap();
        assert_eq!(low, 0.0);
        // Missing float → only days-to-cover contributes (≤30), not None.
        let dtc_only = short_interest_score(None, None, Some(7.0)).unwrap();
        assert!((dtc_only - 30.0).abs() < 1e-6, "dtc-only → 30, got {dtc_only}");
        // Nothing usable → None.
        assert_eq!(short_interest_score(None, None, None), None);
    }

    // End-to-end against the REAL schema: migrate an in-memory DB, insert a distressed
    // shell + a clean company + a no-intel ticker, run the recompute, read back the
    // three columns. Proves the SQL column names, JSON flag parsing, collected-flag
    // logic and the upsert all line up (not just the pure functions).
    #[test]
    fn recompute_risk_scores_end_to_end() {
        let conn = Connection::open_in_memory().unwrap();
        crate::local_db::schema::migrate(&conn).unwrap();

        // Three tradable universe assets.
        conn.execute_batch(
            "INSERT INTO universe_assets (symbol,tradable,shortable,market_cap,float_shares,updated_at)
             VALUES ('SHELL',1,0,100000000,100000000,'now'),
                    ('CLEAN',1,0,1000000000,500000000,'now'),
                    ('BARE', 1,0,50000000, 20000000, 'now');",
        )
        .unwrap();

        // SHELL: fresh 424B5 ATM prospectus + deep losses + thin cash + crowded short.
        conn.execute(
            "INSERT INTO company_intel
                 (symbol, has_recent_shelf, latest_dilution_form, latest_dilution_date,
                  dilution_flags, dilution_updated_at,
                  net_income_last_q, net_income_ttm, negative_quarters_last4,
                  operating_cash_flow_ttm, cash_and_equivalents, financials_updated_at,
                  short_interest, days_to_cover)
             VALUES ('SHELL', 1, '424B5', '2026-06-15',
                  '{\"atm\":true,\"resale\":false,\"warrants\":false,\"offering_amount\":40000000}',
                  '2026-06-17T00:00:00Z',
                  -4.0, -10.0, 4, -8000000.0, 1000000.0, '2026-06-17T00:00:00Z',
                  30000000, 7.0)",
            [],
        )
        .unwrap();

        // CLEAN: collected, profitable, no shelf, no short → low/zero (not None).
        conn.execute(
            "INSERT INTO company_intel
                 (symbol, has_recent_shelf, dilution_updated_at,
                  net_income_ttm, net_income_last_q, negative_quarters_last4,
                  operating_cash_flow_ttm, cash_and_equivalents, financials_updated_at,
                  short_interest, days_to_cover)
             VALUES ('CLEAN', 0, '2026-06-17T00:00:00Z',
                  500.0, 120.0, 0, 800.0, 200000000.0, '2026-06-17T00:00:00Z',
                  1000000, 0.5)",
            [],
        )
        .unwrap();
        // BARE: no company_intel row at all → everything unknown (None).

        let n = recompute_risk_scores(&conn, "2026-06-17").unwrap();
        assert_eq!(n, 3, "all three tradable tickers scored");

        let read = |sym: &str| -> (Option<f64>, Option<f64>, Option<f64>) {
            conn.query_row(
                "SELECT dilution_capacity_score, dilution_need_score, short_interest_score
                 FROM fundamentals_cache WHERE symbol=?1",
                [sym],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap()
        };

        // SHELL: capacity ≈100 (shelf+424B5+atm+offering, ×1.2 recency clamps to 100),
        // need =100 (losses+burn+thin cash), short =100 (30% float + 7 dtc).
        let (cap, need, si) = read("SHELL");
        assert_eq!(cap, Some(100.0));
        assert_eq!(need, Some(100.0));
        assert_eq!(si, Some(100.0));

        // CLEAN: collected → numeric (not None); all near 0 (no shelf / profitable /
        // tiny short %float below the 5% floor).
        let (cap, need, si) = read("CLEAN");
        assert_eq!(cap, Some(0.0));
        assert_eq!(need, Some(0.0));
        assert_eq!(si, Some(0.0));

        // BARE: no intel collected → capacity/need unknown; short uncomputable → None.
        let (cap, need, si) = read("BARE");
        assert_eq!(cap, None);
        assert_eq!(need, None);
        assert_eq!(si, None);
    }
}
