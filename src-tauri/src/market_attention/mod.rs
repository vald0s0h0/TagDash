// Market Attention Gate — direction-agnostic ticker SELECTION engine.
//
// Goal: between 09:30 and 12:30 ET, surface the tickers the market is paying the
// most attention to over a rolling 5-minute window. This engine does NOT look for
// a long/short setup; it only answers "which tickers are the most watched/traded
// right now?" so the strategies (Perfect Pullback) know which names to analyse.
//
// Once a minute it recomputes a ranked top-10 list and publishes it into a shared
// `Vec<AttentionEntry>`. The Perfect Pullback engine reads that list and MEMORISES
// the symbols (a ticker can drop off the list when its volume falls during a
// pullback, but it stays worth watching), so this engine is purely additive — it
// never fires an alert and never trades.
//
// Data source: the 1-minute closed bars already held in MarketState. During the
// regular session Alpaca streams 1-minute bars for the whole universe
// (MarketState::on_bar → M1 ring, carrying volume, trade_count `n` and vwap `vw`),
// so every per-symbol 5-minute statistic is read straight from RAM — no live API
// call. Float comes from the DB (`universe_assets`, populated by the startup
// pipeline); the only network calls are the lazy, bounded historical-baseline
// fetches for the relative-attention component.
//
// The gates run cheapest-first (Gate 0 session → Gate 1 liquidity → Gate 2
// persistence hard filter → Gate 3 cross-sectional ranks → Gate 4 market-share
// groups → Gate 5 relative attention (soft) → Gate 6 acceleration bonus); see the
// per-gate comments below. The final AttentionScore stays direction-agnostic.

use std::collections::{HashMap, HashSet};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex, RwLock,
};

use chrono::{DateTime, Utc};
use tokio::time::Duration;

use crate::config::secrets::Secrets;
use crate::local_db::universe_repository;
use crate::market_state::aggregators::{Bar, Timeframe};
use crate::market_state::MarketState;
use crate::types::AttentionEntry;

// ─── Tunable parameters (recompile to apply) ──────────────────────────────────

/// Gate 0 — active window in ET wall-clock minutes: 09:30 (570) → 12:30 (750).
const SESSION_START_MIN: u32 = 9 * 60 + 30; // 570
const SESSION_END_MIN:   u32 = 12 * 60 + 30; // 750

/// Rolling analysis window (seconds) — the last 5 one-minute buckets.
const WINDOW_SECS: i64 = 300;
/// Loop cadence (seconds of market time) between session/throttle checks. The
/// ranking itself is recomputed at most once per (simulated) minute boundary.
const LOOP_INTERVAL_SECS: u64 = 5;
/// How often the per-symbol float map is reloaded from the DB.
const FLOAT_REFRESH_SECS: u64 = 300;

// ── Gate 1 — minimum live liquidity (cheap, runs on every streamed ticker) ────
const MIN_DOLLAR_VOL_5M:  f64 = 300_000.0;
const MIN_VOLUME_5M:      u64 = 100_000;
const MIN_TRADE_COUNT_5M: u64 = 100;

// ── Gate 2 — persistence hard filter (rejects single-minute spikes) ───────────
/// Minimum minutes (of the last 5) with significant volume.
const MIN_ACTIVE_MINUTES:   u8  = 3;
/// A minute counts as "active" when its volume is at least this fraction of the
/// window's mean minute volume.
const ACTIVE_BAR_FRACTION:  f64 = 0.10;
/// Reject when one minute carries more than this share of the 5-minute volume.
const MAX_1M_VOLUME_SHARE:  f64 = 0.80;

// ── Gate 4 — market-share groups ──────────────────────────────────────────────
/// Float below this counts as a small cap (its own market-share group).
const SMALLCAP_FLOAT_MAX: u64 = 100_000_000;
/// Price below this counts as "under 20$" (its own market-share group).
const UNDER20_PRICE:      f64 = 20.0;

// ── Final score weights (direction-agnostic) ──────────────────────────────────
const W_PR_DOLLAR: f64 = 0.35;
const W_PR_VOLUME: f64 = 0.20;
const W_REL:       f64 = 0.25; // dropped (and the rest renormalised) when unknown
const W_GROUP:     f64 = 0.10;
const W_ACCEL:     f64 = 0.10;

/// How many tickers the published list holds.
const TOP_N: usize = 10;

// ── Gate 5 — historical relative-attention baseline ───────────────────────────
/// Average the same-time-of-day 5-minute dollar volume over at most this many
/// trading days.
const HIST_DAYS: usize = 20;
/// Calendar lookback to cover HIST_DAYS trading days (weekends + holidays).
const HIST_LOOKBACK_CAL_DAYS: i64 = 28;
/// At most this many per-symbol baseline fetches are scheduled per recompute, to
/// bound REST traffic (the cache fills over the first few minutes of the session).
const HIST_FETCH_PER_MINUTE: usize = 8;

/// Per-symbol historical baseline: 5-min-of-day bucket (start minute, ET) → average
/// dollar volume across the last HIST_DAYS sessions at that time of day.
type Baseline = HashMap<u32, f64>;

// ─── Per-symbol 5-minute window statistics (pure, testable) ───────────────────

#[derive(Debug, Clone, Copy)]
struct WindowStats {
    volume_5m:             u64,
    dollar_volume_5m:      f64,
    trade_count_5m:        u64,
    active_minutes_5m:     u8,
    max_1m_volume_share:   f64,
    /// Dollar volume of the prior 5-minute window (−10..−5 min) for acceleration.
    prev_dollar_volume_5m: f64,
}

/// One streamed ticker's snapshot for this recompute.
struct Row {
    symbol:       String,
    price:        f64,
    stats:        WindowStats,
    float_shares: Option<u64>,
}

// ─── Engine ───────────────────────────────────────────────────────────────────

pub struct MarketAttentionEngine;

impl MarketAttentionEngine {
    /// Spawn the background loop. Returns immediately.
    pub fn start(
        running:   Arc<AtomicBool>,
        market:    Arc<RwLock<MarketState>>,
        db:        Arc<Mutex<rusqlite::Connection>>,
        secrets:   Arc<RwLock<Secrets>>,
        attention: Arc<RwLock<Vec<AttentionEntry>>>,
    ) {
        // Tauri-managed runtime so this can be launched from the sync `setup` hook.
        tauri::async_runtime::spawn(async move {
            let mut floats = load_floats(&db);
            let mut floats_loaded = std::time::Instant::now();

            // Historical baselines (symbol → time-of-day profile), filled lazily by
            // background fetches and shared with them. `requested` dedups the fetches.
            let hist: Arc<RwLock<HashMap<String, Baseline>>> = Arc::new(RwLock::new(HashMap::new()));
            let requested: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
            let mut hist_day: Option<String> = None;

            // Recompute throttle (simulated-minute granularity) + replay reset watch.
            let mut last_minute: Option<i64> = None;
            let mut replay_gen = crate::replay::clock::generation();

            while running.load(Ordering::Relaxed) {
                // Market Replay reset: drop the cache + published list so the
                // replayed session is rebuilt from the simulated clock.
                {
                    let g = crate::replay::clock::generation();
                    if g != replay_gen {
                        replay_gen = g;
                        hist.write().unwrap().clear();
                        requested.lock().unwrap().clear();
                        hist_day = None;
                        attention.write().unwrap().clear();
                        last_minute = None;
                    }
                }

                if floats_loaded.elapsed() >= Duration::from_secs(FLOAT_REFRESH_SECS) {
                    floats = load_floats(&db);
                    floats_loaded = std::time::Instant::now();
                }

                let now = crate::time::now();

                // New ET day → rebuild the historical cache for this session.
                let today = crate::time::et_date(now);
                if hist_day.as_deref() != Some(today.as_str()) {
                    hist.write().unwrap().clear();
                    requested.lock().unwrap().clear();
                    hist_day = Some(today.clone());
                }

                // Gate 0 — only between 09:30 and 12:30 ET. Outside, publish nothing.
                let m = crate::time::et_minutes(now);
                if !(SESSION_START_MIN..SESSION_END_MIN).contains(&m) {
                    if !attention.read().unwrap().is_empty() {
                        attention.write().unwrap().clear();
                    }
                    crate::replay::clock::scaled_sleep(LOOP_INTERVAL_SECS * 1000).await;
                    continue;
                }

                // Throttle to once per (simulated) minute.
                let minute = now.timestamp() / 60;
                if last_minute == Some(minute) {
                    crate::replay::clock::scaled_sleep(LOOP_INTERVAL_SECS * 1000).await;
                    continue;
                }
                last_minute = Some(minute);

                let entries = recompute(&market, &floats, &hist, now);

                // Schedule lazy baseline fetches for survivors we don't have yet.
                schedule_baseline_fetches(&entries, &hist, &requested, &secrets, now);

                eprintln!(
                    "[tagdash] market_attention: top {} published (baselines cached: {})",
                    entries.len(),
                    hist.read().unwrap().len(),
                );
                *attention.write().unwrap() = entries;

                crate::replay::clock::scaled_sleep(LOOP_INTERVAL_SECS * 1000).await;
            }
        });
    }
}

/// One full recompute pass: snapshot every streamed ticker's 5-minute window
/// stats, run gates 1→6 and the scoring, and return the sorted top-N.
fn recompute(
    market: &Arc<RwLock<MarketState>>,
    floats: &HashMap<String, u64>,
    hist:   &Arc<RwLock<HashMap<String, Baseline>>>,
    now:    DateTime<Utc>,
) -> Vec<AttentionEntry> {
    // Snapshot per-ticker window stats under a brief read lock (only arithmetic on
    // ≤10 one-minute bars per symbol; all gating runs after the lock is released).
    let rows: Vec<Row> = {
        let ms = market.read().unwrap();
        ms.tickers
            .values()
            .filter_map(|t| {
                let price = t.last_price?;
                let m1 = ms.closed_bars(&t.symbol, Timeframe::M1);
                let stats = compute_window_stats(&m1, now)?;
                Some(Row {
                    symbol:       t.symbol.clone(),
                    price,
                    stats,
                    float_shares: floats.get(&t.symbol).copied(),
                })
            })
            .collect()
    };
    if rows.is_empty() {
        return Vec::new();
    }

    // Gate 4 totals — over ALL tickers with current-window data (not just the gate
    // survivors), so market share reflects the whole market / group.
    let total_all:      f64 = rows.iter().map(|r| r.stats.dollar_volume_5m).sum();
    let total_smallcap: f64 = rows.iter().filter(|r| is_smallcap(r)).map(|r| r.stats.dollar_volume_5m).sum();
    let total_under20:  f64 = rows.iter().filter(|r| is_under20(r)).map(|r| r.stats.dollar_volume_5m).sum();

    // Gates 1 + 2 — liquidity floor and persistence hard filter.
    let survivors: Vec<&Row> = rows.iter().filter(|r| passes_gate_1_2(&r.stats)).collect();
    if survivors.is_empty() {
        return Vec::new();
    }

    // Gate 3 — cross-sectional percentile ranks among the survivors. Sorted value
    // lists let each survivor's rank be read with a binary search.
    let mut dv: Vec<f64> = survivors.iter().map(|r| r.stats.dollar_volume_5m).collect();
    let mut vv: Vec<f64> = survivors.iter().map(|r| r.stats.volume_5m as f64).collect();
    let mut tv: Vec<f64> = survivors.iter().map(|r| r.stats.trade_count_5m as f64).collect();
    dv.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    vv.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    tv.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    // Gate 4 — per-group leader shares, so a group's leader scores 1.0 and mega
    // caps can't crush very active low-price names.
    let max_all      = survivors.iter().map(|r| share(r.stats.dollar_volume_5m, total_all)).fold(0.0, f64::max);
    let max_smallcap = survivors.iter().filter(|r| is_smallcap(r)).map(|r| share(r.stats.dollar_volume_5m, total_smallcap)).fold(0.0, f64::max);
    let max_under20  = survivors.iter().filter(|r| is_under20(r)).map(|r| share(r.stats.dollar_volume_5m, total_under20)).fold(0.0, f64::max);

    // Gate 5 lookup key — the current 5-min-of-day bucket (ET start minute).
    let cur_bucket = (crate::time::et_minutes(now) / 5) * 5;
    let hist_read = hist.read().unwrap();

    let mut entries: Vec<AttentionEntry> = survivors
        .iter()
        .map(|r| {
            let s = &r.stats;
            let dvv = s.dollar_volume_5m;

            // Gate 3 ranks.
            let pr_dollar = percentile_rank(&dv, dvv);
            let pr_volume = percentile_rank(&vv, s.volume_5m as f64);
            let pr_trade  = percentile_rank(&tv, s.trade_count_5m as f64);

            // Gate 4 shares + group score.
            let small = is_smallcap(r);
            let under = is_under20(r);
            let ms_all   = share(dvv, total_all);
            let ms_small = if small { share(dvv, total_smallcap) } else { 0.0 };
            let ms_under = if under { share(dvv, total_under20) } else { 0.0 };
            let mut group = normalise(ms_all, max_all);
            if small { group = group.max(normalise(ms_small, max_smallcap)); }
            if under { group = group.max(normalise(ms_under, max_under20)); }

            // Gate 6 acceleration (bonus) + Gate 5 relative attention (soft).
            let accel = (s.prev_dollar_volume_5m > 0.0).then(|| dvv / s.prev_dollar_volume_5m);
            let rel = hist_read
                .get(&r.symbol)
                .and_then(|b| b.get(&cur_bucket))
                .copied()
                .filter(|b| *b > 0.0)
                .map(|b| dvv / b);

            let attention_score = score(pr_dollar, pr_volume, group, accel, rel);

            AttentionEntry {
                symbol: r.symbol.clone(),
                attention_score,
                dollar_volume_5m: dvv,
                volume_5m: s.volume_5m,
                trade_count_5m: s.trade_count_5m,
                pr_dollar_volume_5m: pr_dollar,
                pr_volume_5m: pr_volume,
                pr_trade_count_5m: pr_trade,
                relative_attention_5m: rel,
                market_share_5m: ms_all,
                smallcap_market_share_5m: ms_small,
                under20_market_share_5m: ms_under,
                active_minutes_5m: s.active_minutes_5m,
                max_1m_volume_share: s.max_1m_volume_share,
                acceleration_5m: accel,
                updated_at: now,
            }
        })
        .collect();
    drop(hist_read);

    entries.sort_by(|a, b| {
        b.attention_score
            .partial_cmp(&a.attention_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    entries.truncate(TOP_N);
    entries
}

// ─── Gate / scoring helpers (pure) ─────────────────────────────────────────────

/// Gate 1 (liquidity) + Gate 2 (persistence) hard filters.
fn passes_gate_1_2(s: &WindowStats) -> bool {
    s.dollar_volume_5m >= MIN_DOLLAR_VOL_5M
        && s.volume_5m >= MIN_VOLUME_5M
        && s.trade_count_5m >= MIN_TRADE_COUNT_5M
        && s.active_minutes_5m >= MIN_ACTIVE_MINUTES
        && s.max_1m_volume_share <= MAX_1M_VOLUME_SHARE
}

fn is_smallcap(r: &Row) -> bool {
    r.float_shares.map_or(false, |f| f < SMALLCAP_FLOAT_MAX)
}
fn is_under20(r: &Row) -> bool {
    r.price < UNDER20_PRICE
}

fn share(x: f64, total: f64) -> f64 {
    if total > 0.0 { x / total } else { 0.0 }
}
fn normalise(x: f64, max: f64) -> f64 {
    if max > 0.0 { (x / max).clamp(0.0, 1.0) } else { 0.0 }
}

/// Fraction of `sorted` (ascending) strictly below `x`, in [0,1]. The window
/// leader → 1.0, the laggard → 0.0; a single survivor → 1.0.
fn percentile_rank(sorted: &[f64], x: f64) -> f64 {
    let n = sorted.len();
    if n <= 1 {
        return 1.0;
    }
    let below = sorted.partition_point(|&v| v < x);
    (below as f64 / (n - 1) as f64).clamp(0.0, 1.0)
}

/// Direction-agnostic composite score, 0..100. Relative attention is a SOFT
/// component: when its historical baseline is unknown the 25% weight is dropped
/// and the remaining weights are renormalised (neutral, never penalising).
fn score(pr_dollar: f64, pr_volume: f64, group: f64, accel: Option<f64>, rel: Option<f64>) -> f64 {
    let accel_bonus = accel.map_or(0.0, |a| ((a - 1.0) / 2.0).clamp(0.0, 1.0));
    let mut num = W_PR_DOLLAR * pr_dollar
        + W_PR_VOLUME * pr_volume
        + W_GROUP * group
        + W_ACCEL * accel_bonus;
    let mut den = W_PR_DOLLAR + W_PR_VOLUME + W_GROUP + W_ACCEL;
    if let Some(r) = rel {
        let rel_score = ((r - 1.0) / 2.0).clamp(0.0, 1.0);
        num += W_REL * rel_score;
        den += W_REL;
    }
    if den <= 0.0 { 0.0 } else { (100.0 * num / den).clamp(0.0, 100.0) }
}

/// Per-symbol 5-minute window statistics from closed 1-minute bars (ascending).
/// Current window = bars in [now−WINDOW_SECS, now); previous window =
/// [now−2·WINDOW_SECS, now−WINDOW_SECS). None when the current window is empty or
/// carries no volume. Pure on timestamps (no ET clock) so it's unit-testable.
fn compute_window_stats(m1: &[Bar], now: DateTime<Utc>) -> Option<WindowStats> {
    let now_s = now.timestamp();
    let cur_start  = now_s - WINDOW_SECS;
    let prev_start = now_s - 2 * WINDOW_SECS;

    let mut volume_5m: u64 = 0;
    let mut dollar_volume_5m = 0.0;
    let mut trade_count_5m: u64 = 0;
    let mut max_bucket_vol: u64 = 0;
    let mut active_minutes_5m: u8 = 0;
    let mut prev_dollar_volume_5m = 0.0;
    // Hold the current-window per-bar volumes to derive active minutes after the
    // window total (and thus the mean minute) is known.
    let mut cur_vols: Vec<u64> = Vec::with_capacity(5);

    for b in m1 {
        let ts = b.time.timestamp();
        let price = b.vwap.unwrap_or((b.high + b.low + b.close) / 3.0);
        if ts >= cur_start && ts < now_s {
            volume_5m += b.volume;
            dollar_volume_5m += b.volume as f64 * price;
            trade_count_5m += b.trade_count.unwrap_or(0);
            max_bucket_vol = max_bucket_vol.max(b.volume);
            cur_vols.push(b.volume);
        } else if ts >= prev_start && ts < cur_start {
            prev_dollar_volume_5m += b.volume as f64 * price;
        }
    }

    if cur_vols.is_empty() || volume_5m == 0 {
        return None;
    }

    // A minute is "active" if its volume is ≥ ACTIVE_BAR_FRACTION of the window's
    // mean minute (using the actual bucket count, so early-session windows with
    // few bars are judged fairly).
    let mean_minute = volume_5m as f64 / cur_vols.len() as f64;
    let active_floor = ACTIVE_BAR_FRACTION * mean_minute;
    for v in &cur_vols {
        if *v as f64 >= active_floor {
            active_minutes_5m += 1;
        }
    }

    let max_1m_volume_share = max_bucket_vol as f64 / volume_5m as f64;

    Some(WindowStats {
        volume_5m,
        dollar_volume_5m,
        trade_count_5m,
        active_minutes_5m,
        max_1m_volume_share,
        prev_dollar_volume_5m,
    })
}

// ─── Historical baseline (relative attention) ──────────────────────────────────

/// Schedule up to HIST_FETCH_PER_MINUTE background baseline fetches for the
/// published survivors we don't yet have a baseline for. Each spawned task fills
/// the shared `hist` cache; on failure the symbol is unmarked so it can retry.
fn schedule_baseline_fetches(
    entries:   &[AttentionEntry],
    hist:      &Arc<RwLock<HashMap<String, Baseline>>>,
    requested: &Arc<Mutex<HashSet<String>>>,
    secrets:   &Arc<RwLock<Secrets>>,
    now:       DateTime<Utc>,
) {
    let mut to_fetch: Vec<String> = Vec::new();
    {
        let mut req = requested.lock().unwrap();
        let have = hist.read().unwrap();
        for e in entries {
            if to_fetch.len() >= HIST_FETCH_PER_MINUTE {
                break;
            }
            if have.contains_key(&e.symbol) {
                continue;
            }
            if req.insert(e.symbol.clone()) {
                to_fetch.push(e.symbol.clone());
            }
        }
    }

    for sym in to_fetch {
        let (hist, requested, secrets) = (hist.clone(), requested.clone(), secrets.clone());
        tauri::async_runtime::spawn(async move {
            match fetch_baseline(&secrets, &sym, now).await {
                Some(profile) => {
                    hist.write().unwrap().insert(sym, profile);
                }
                None => {
                    // Allow a later retry rather than burning the symbol for the day.
                    requested.lock().unwrap().remove(&sym);
                }
            }
        });
    }
}

/// Build one symbol's historical time-of-day profile: for each 5-min-of-day bucket
/// in the session window, the average dollar volume across the last HIST_DAYS
/// sessions (today excluded). Fetched from Alpaca split-adjusted 1-minute bars.
/// None on missing creds / REST error / no data.
async fn fetch_baseline(
    secrets: &Arc<RwLock<Secrets>>,
    symbol:  &str,
    now:     DateTime<Utc>,
) -> Option<Baseline> {
    let (key, sec) = {
        let s = secrets.read().unwrap();
        (s.alpaca_key.clone(), s.alpaca_secret.clone())
    };
    let (Some(k), Some(sc)) = (key, sec) else { return None };
    if k.is_empty() || sc.is_empty() {
        return None;
    }

    let start = (now - chrono::Duration::days(HIST_LOOKBACK_CAL_DAYS))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    let syms = [symbol.to_string()];
    let map = crate::alpaca::bars::fetch_minute_bars_since(&k, &sc, &syms, &start)
        .await
        .ok()?;
    let bars = map.get(symbol)?;
    let today = crate::time::et_date(now);

    // (bucket → (ET date → that day's dollar volume in the bucket)).
    let mut acc: HashMap<u32, HashMap<String, f64>> = HashMap::new();
    for b in bars {
        let mins = crate::time::et_minutes(b.time);
        if !(SESSION_START_MIN..SESSION_END_MIN).contains(&mins) {
            continue;
        }
        let date = crate::time::et_date(b.time);
        if date == today {
            continue; // today's partial bars must not bias the baseline
        }
        let bucket = (mins / 5) * 5;
        let price = b.vwap.unwrap_or((b.high + b.low + b.close) / 3.0);
        *acc.entry(bucket).or_default().entry(date).or_default() += b.volume as f64 * price;
    }
    if acc.is_empty() {
        return None;
    }

    let mut out: Baseline = HashMap::new();
    for (bucket, by_date) in acc {
        // Average over the most recent HIST_DAYS sessions (dates sort lexically =
        // chronologically; descending keeps the newest).
        let mut dates: Vec<(&String, &f64)> = by_date.iter().collect();
        dates.sort_by(|a, b| b.0.cmp(a.0));
        let recent: Vec<f64> = dates.iter().take(HIST_DAYS).map(|(_, v)| **v).collect();
        if !recent.is_empty() {
            out.insert(bucket, recent.iter().sum::<f64>() / recent.len() as f64);
        }
    }
    (!out.is_empty()).then_some(out)
}

/// Per-symbol float (shares) from the universe table (DB, populated by the startup
/// pipeline). Symbols with no known float are omitted. Mirrors `scanner::load_floats`.
fn load_floats(db: &Arc<Mutex<rusqlite::Connection>>) -> HashMap<String, u64> {
    let conn = db.lock().unwrap();
    universe_repository::get_all(&conn)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|a| a.float_shares.filter(|f| *f > 0).map(|f| (a.symbol, f as u64)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(base: DateTime<Utc>, offset_secs: i64) -> DateTime<Utc> {
        base - chrono::Duration::seconds(offset_secs)
    }

    fn bar(time: DateTime<Utc>, volume: u64, vwap: f64, trades: u64) -> Bar {
        Bar {
            time,
            open: vwap,
            high: vwap,
            low: vwap,
            close: vwap,
            volume,
            vwap: Some(vwap),
            trade_count: Some(trades),
        }
    }

    #[test]
    fn percentile_rank_basics() {
        let v = vec![1.0, 2.0, 3.0, 4.0];
        assert_eq!(percentile_rank(&v, 4.0), 1.0);
        assert_eq!(percentile_rank(&v, 1.0), 0.0);
        assert!((percentile_rank(&v, 3.0) - 2.0 / 3.0).abs() < 1e-9);
        // Single survivor → top rank.
        assert_eq!(percentile_rank(&[5.0], 5.0), 1.0);
    }

    #[test]
    fn score_drops_and_renormalises_when_rel_unknown() {
        // All structural inputs maxed, no acceleration, no rel baseline:
        // num = 0.35+0.20+0.10 = 0.65, den = 0.75 → 86.67.
        let s_unknown = score(1.0, 1.0, 1.0, None, None);
        assert!((s_unknown - 100.0 * 0.65 / 0.75).abs() < 1e-6);
        // With a strong relative attention (rel_score = 1): num 0.90, den 1.0 → 90.
        let s_known = score(1.0, 1.0, 1.0, None, Some(3.0));
        assert!((s_known - 90.0).abs() < 1e-6);
        // A neutral rel (=1) contributes 0 but still counts its weight: den 1.0.
        let s_neutral = score(1.0, 1.0, 1.0, None, Some(1.0));
        assert!((s_neutral - 100.0 * 0.65 / 1.0).abs() < 1e-6);
    }

    #[test]
    fn window_stats_sums_active_and_acceleration() {
        // Fixed base instant; current window = offsets 60..300, prev = 360..600.
        let base = Utc.timestamp_opt(1_700_000_000, 0).single().unwrap();
        let mut bars = Vec::new();
        // Current window: volumes [5, 120, 120, 120, 135] (sum 500), vwap 10,
        // trades 10 each. The 5-volume minute is below 10% of the mean (100) → not
        // active → active_minutes = 4. max share = 135/500 = 0.27.
        let cur = [(300, 5u64), (240, 120), (180, 120), (120, 120), (60, 135)];
        for (off, vol) in cur {
            bars.push(bar(at(base, off), vol, 10.0, 10));
        }
        // Previous window: 5 minutes of 80 volume @ vwap 10 → 4000 dollar volume.
        for off in [360, 420, 480, 540, 600] {
            bars.push(bar(at(base, off), 80, 10.0, 10));
        }
        bars.sort_by_key(|b| b.time);

        let s = compute_window_stats(&bars, base).expect("stats");
        assert_eq!(s.volume_5m, 500);
        assert_eq!(s.trade_count_5m, 50);
        assert!((s.dollar_volume_5m - 5000.0).abs() < 1e-6);
        assert_eq!(s.active_minutes_5m, 4);
        assert!((s.max_1m_volume_share - 135.0 / 500.0).abs() < 1e-9);
        assert!((s.prev_dollar_volume_5m - 4000.0).abs() < 1e-6);

        // Acceleration as the engine derives it.
        let accel = s.dollar_volume_5m / s.prev_dollar_volume_5m;
        assert!((accel - 1.25).abs() < 1e-9);
    }

    #[test]
    fn window_stats_none_when_empty() {
        let base = Utc.timestamp_opt(1_700_000_000, 0).single().unwrap();
        // A bar far in the past (outside both windows) → no current data.
        let bars = vec![bar(at(base, 10_000), 100, 5.0, 3)];
        assert!(compute_window_stats(&bars, base).is_none());
    }

    #[test]
    fn gate_1_2_filters() {
        // Liquid + persistent → passes.
        let ok = WindowStats {
            volume_5m: 200_000,
            dollar_volume_5m: 1_000_000.0,
            trade_count_5m: 500,
            active_minutes_5m: 4,
            max_1m_volume_share: 0.5,
            prev_dollar_volume_5m: 500_000.0,
        };
        assert!(passes_gate_1_2(&ok));
        // Single-minute spike (share > 0.80) → rejected by gate 2.
        let spike = WindowStats { max_1m_volume_share: 0.95, active_minutes_5m: 2, ..ok };
        assert!(!passes_gate_1_2(&spike));
        // Thin name (dollar volume below floor) → rejected by gate 1.
        let thin = WindowStats { dollar_volume_5m: 100_000.0, ..ok };
        assert!(!passes_gate_1_2(&thin));
    }
}
