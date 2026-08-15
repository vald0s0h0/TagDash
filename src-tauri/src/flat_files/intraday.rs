// On-demand intraday flat file — 1-minute bars, aggregated on read.
//
// WHY THIS EXISTS, given `minute/` already holds 1-minute bars: the minute files
// snapshot only the day's 2000 most-traded symbols (`replay::data::MAX_SYMBOLS`).
// Panic Mean Reversion candidates are, by construction, NOT the day's most-traded
// names — they are names that just shocked. Measured on real watchlists, roughly half
// the rows had no bar at all in that day's minute file (2026-06-15: 9 of 19), so the
// execution pane came up empty for exactly the tickers the strategy asks you to
// trade. This store is filled per (symbol, day) instead of as a market-wide snapshot.
//
//   flat_files/intraday/i1m-YYYY-MM-DD.db
//
// STORED AT 1 MINUTE, SERVED AT ANY TIMEFRAME. One download feeds the 1m, 2m, 5m and
// 15m panes: `read_symbol_day` buckets the minutes into whatever the chart asked for.
// That is what makes a minute-cadence refresh possible during a replay at no extra
// cost — the 5-minute candle simply grows as simulated minutes elapse.
//
// FREE-TIER NOTE: Alpaca's Basic plan serves historical SIP bars (1Min included,
// premarket included, years of depth) as long as the request's `end` is at least 15
// minutes old — always true for a replayed past day. Only REAL-TIME SIP is gated, so
// Market Replay works entirely on the free plan.
//
// Whole sessions (04:00 → 20:00 ET) are stored regardless of where a replay clock
// sits; the look-ahead guard is applied at READ time via `until_ms`, never by
// truncating what is stored. `symbol_days` records which (symbol, day) pairs were
// fetched, so a name that genuinely did not trade is remembered as "downloaded,
// empty" and never re-requested.

use std::path::{Path, PathBuf};

use chrono::{NaiveDate, Utc};
use rusqlite::Connection;

use super::{kind_dir, FlatFileDay, Kind};
use crate::market_state::aggregators::{Bar, Timeframe};

/// Bar granularity stored here, as Alpaca names it.
const TIMEFRAME: &str = "1Min";
/// Seconds per STORED bar. Also the look-ahead step: a chart may only see minutes
/// that have fully elapsed in simulated time, matching `rest_end_clamp(60)`.
pub const BUCKET_SECS: i64 = 60;

/// Sessions of intraday history an execution pane loads: the current day plus the
/// four before it. A Panic Mean Reversion candidate is judged on a 2-to-5-day shock
/// window (`scoring::MAX_WINDOW`), so the pane has to show the whole move that put
/// the name on the watchlist — one session shows the aftermath with no context.
pub const HISTORY_DAYS: usize = 5;

/// Final path of the intraday file for one ET day.
pub fn path(app_dir: &Path, day: &str) -> PathBuf {
    kind_dir(app_dir, Kind::Intraday).join(format!("i1m-{day}.db"))
}

fn open_db(p: &Path) -> rusqlite::Result<Connection> {
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let conn = Connection::open(p)?;
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         CREATE TABLE IF NOT EXISTS bars (
             symbol TEXT NOT NULL,
             t_ms   INTEGER NOT NULL,
             o REAL NOT NULL, h REAL NOT NULL, l REAL NOT NULL, c REAL NOT NULL,
             v INTEGER NOT NULL,
             n INTEGER,
             vw REAL,
             PRIMARY KEY (symbol, t_ms)
         );
         CREATE TABLE IF NOT EXISTS symbol_days (
             symbol     TEXT PRIMARY KEY NOT NULL,
             bar_count  INTEGER NOT NULL,
             fetched_at TEXT NOT NULL
         );",
    )?;
    Ok(conn)
}

/// Timeframes this store can serve. Sub-minute frames are built from trade ticks by
/// the live aggregator and cannot be reconstructed from minute bars.
pub fn serves(tf: Timeframe) -> bool {
    tf.seconds() >= BUCKET_SECS && tf != Timeframe::Daily
}

// ─── Reading ───────────────────────────────────────────────────────────────────

/// True when `symbol` has already been fetched for `day` — including a fetch that
/// legitimately produced zero bars. Callers use this to avoid re-requesting.
pub fn has_symbol(app_dir: &Path, day: &str, symbol: &str) -> bool {
    let p = path(app_dir, day);
    if !p.exists() {
        return false;
    }
    let Ok(conn) = open_db(&p) else { return false };
    conn.query_row(
        "SELECT 1 FROM symbol_days WHERE symbol = ?1",
        [symbol],
        |_| Ok(()),
    )
    .is_ok()
}

/// Stored bars for (day, symbol) aggregated into `tf`, ascending.
///
/// `until_ms`, when set, drops every 1-minute bar OPENING at or after it — the
/// Market-Replay look-ahead guard. Callers pass `sim_now − 60s` so only fully elapsed
/// minutes are visible; the last aggregated bucket is then a partially-formed candle,
/// exactly like a live chart, with no future information in it.
pub fn read_symbol_day(
    app_dir: &Path,
    day: &str,
    symbol: &str,
    tf: Timeframe,
    until_ms: Option<i64>,
) -> Vec<Bar> {
    let p = path(app_dir, day);
    if !p.exists() {
        return Vec::new();
    }
    let Ok(conn) = open_db(&p) else {
        return Vec::new();
    };
    let Ok(mut stmt) =
        conn.prepare("SELECT t_ms,o,h,l,c,v,n,vw FROM bars WHERE symbol = ?1 ORDER BY t_ms")
    else {
        return Vec::new();
    };
    let rows = stmt.query_map([symbol], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, f64>(1)?,
            r.get::<_, f64>(2)?,
            r.get::<_, f64>(3)?,
            r.get::<_, f64>(4)?,
            r.get::<_, i64>(5)?,
            r.get::<_, Option<i64>>(6)?,
            r.get::<_, Option<f64>>(7)?,
        ))
    });
    let Ok(rows) = rows else { return Vec::new() };

    let bucket_ms = tf.seconds() * 1000;
    let mut out: Vec<Bar> = Vec::new();
    // Volume-weighted price numerator for the bucket currently being built.
    let mut vw_num = 0.0f64;
    let mut cur_bucket = i64::MIN;

    for (t_ms, o, h, l, c, v, n, vw) in rows.flatten() {
        if let Some(cap) = until_ms {
            if t_ms >= cap {
                break; // ordered by t_ms — nothing later can qualify
            }
        }
        let bucket = t_ms.div_euclid(bucket_ms) * bucket_ms;
        let vol = v.max(0) as u64;
        let px = vw.unwrap_or(c);

        if bucket != cur_bucket {
            // Close the previous bucket's VWAP before starting a new one.
            if let Some(prev) = out.last_mut() {
                prev.vwap = (prev.volume > 0).then(|| vw_num / prev.volume as f64);
            }
            let Some(time) = chrono::TimeZone::timestamp_millis_opt(&Utc, bucket).single() else {
                continue;
            };
            cur_bucket = bucket;
            vw_num = px * vol as f64;
            out.push(Bar {
                time,
                open: o,
                high: h,
                low: l,
                close: c,
                volume: vol,
                vwap: None,
                trade_count: n.map(|x| x.max(0) as u64),
            });
        } else if let Some(b) = out.last_mut() {
            b.high = b.high.max(h);
            b.low = b.low.min(l);
            b.close = c;
            b.volume += vol;
            b.trade_count = match (b.trade_count, n) {
                (Some(a), Some(x)) => Some(a + x.max(0) as u64),
                (a, _) => a,
            };
            vw_num += px * vol as f64;
        }
    }
    if let Some(prev) = out.last_mut() {
        prev.vwap = (prev.volume > 0).then(|| vw_num / prev.volume as f64);
    }
    out
}

/// Stored bars for `symbol` across several ET days, aggregated into `tf`, ascending.
///
/// `days` must be ascending. Each day is read from its own file and the results are
/// concatenated: no aggregation bucket can straddle a session, since every intraday
/// timeframe divides 86 400 evenly and the 20:00 → 04:00 ET gap always falls on a
/// bucket boundary. Days with no file (never downloaded, or a holiday that slipped
/// into the list) contribute nothing — the chart renders the gap. `until_ms` is an
/// absolute instant, so it naturally bites only on the last day.
pub fn read_symbol_days(
    app_dir: &Path,
    days: &[String],
    symbol: &str,
    tf: Timeframe,
    until_ms: Option<i64>,
) -> Vec<Bar> {
    let mut out: Vec<Bar> = Vec::new();
    for day in days {
        out.extend(read_symbol_day(app_dir, day, symbol, tf, until_ms));
    }
    out
}

/// The subset of `days` for which `symbol` has never been fetched.
pub fn missing_days(app_dir: &Path, days: &[String], symbol: &str) -> Vec<String> {
    days.iter()
        .filter(|d| !has_symbol(app_dir, d, symbol))
        .cloned()
        .collect()
}

// ─── Writing ───────────────────────────────────────────────────────────────────

type RawRow = (i64, f64, f64, f64, f64, i64, Option<i64>, Option<f64>);

/// Upsert several symbols' bars for one `day` in a single transaction, marking each
/// fetched. A symbol's slice may be empty — the marker is still written, so a name
/// that did not trade is not re-requested forever.
///
/// Batched per day (not per symbol) because a range warm-up writes the whole
/// watchlist across every session at once: one file open and one transaction per
/// day instead of one per (symbol, day).
fn store_day(
    app_dir: &Path,
    day: &str,
    per_symbol: &[(String, Vec<RawRow>)],
) -> Result<(), String> {
    let p = path(app_dir, day);
    let mut conn = open_db(&p).map_err(|e| e.to_string())?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    {
        let mut ins = tx
            .prepare(
                "INSERT INTO bars (symbol,t_ms,o,h,l,c,v,n,vw) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)
                 ON CONFLICT(symbol,t_ms) DO UPDATE SET
                     o=excluded.o, h=excluded.h, l=excluded.l, c=excluded.c,
                     v=excluded.v, n=excluded.n, vw=excluded.vw",
            )
            .map_err(|e| e.to_string())?;
        let mut mark = tx
            .prepare(
                "INSERT INTO symbol_days (symbol,bar_count,fetched_at) VALUES (?1,?2,?3)
                 ON CONFLICT(symbol) DO UPDATE SET
                     bar_count=excluded.bar_count, fetched_at=excluded.fetched_at",
            )
            .map_err(|e| e.to_string())?;
        let now = Utc::now().to_rfc3339();
        for (symbol, bars) in per_symbol {
            for (t_ms, o, h, l, c, v, n, vw) in bars {
                ins.execute(rusqlite::params![symbol, t_ms, o, h, l, c, v, n, vw])
                    .map_err(|e| e.to_string())?;
            }
            mark.execute(rusqlite::params![symbol, bars.len() as i64, now])
                .map_err(|e| e.to_string())?;
        }
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(())
}

/// Single-symbol convenience over `store_day`, for the tests below.
#[cfg(test)]
fn store(app_dir: &Path, day: &str, symbol: &str, bars: &[RawRow]) -> Result<(), String> {
    store_day(app_dir, day, &[(symbol.to_string(), bars.to_vec())])
}

// ─── Fetching ──────────────────────────────────────────────────────────────────

/// Download the full 04:00 → 20:00 ET session of 1-minute bars for `symbols` on `day`
/// and persist them, skipping any (symbol, day) already fetched. Returns the number
/// of symbols newly downloaded.
///
/// Deliberately NOT clamped to a replay's simulated instant: the whole session is
/// stored and `read_symbol_day` caps on read, so one download serves a replay
/// wherever its clock sits and the day then replays entirely offline.
pub async fn fetch_and_store(
    app_dir: &Path,
    key: &str,
    secret: &str,
    day: &str,
    symbols: &[String],
) -> Result<usize, String> {
    fetch_and_store_days(app_dir, key, secret, &[day.to_string()], symbols).await
}

/// Same, over a contiguous span of ET days (ascending) — the execution pane's
/// `HISTORY_DAYS` lead-in.
///
/// Issued as ONE Alpaca range request per symbol chunk rather than one per day: the
/// REST endpoint pages a whole `[start, end]` window, so five sessions of a
/// twenty-name watchlist cost the same round trips as one. Bars are then split back
/// into per-day files by their own ET calendar date, keeping each file exactly what
/// a single-day fetch would have produced.
///
/// A symbol is refetched over the whole span as soon as ANY day of it is missing;
/// the days it already had are re-upserted, which is idempotent and cheap next to a
/// second network round trip.
pub async fn fetch_and_store_days(
    app_dir: &Path,
    key: &str,
    secret: &str,
    days: &[String],
    symbols: &[String],
) -> Result<usize, String> {
    if key.is_empty() || secret.is_empty() {
        return Err("clés Alpaca non configurées — nécessaires pour les barres intraday".into());
    }
    let (Some(first), Some(last)) = (days.first(), days.last()) else {
        return Ok(0);
    };
    let nd_first = NaiveDate::parse_from_str(first, "%Y-%m-%d")
        .map_err(|_| format!("date invalide: {first}"))?;
    let nd_last = NaiveDate::parse_from_str(last, "%Y-%m-%d")
        .map_err(|_| format!("date invalide: {last}"))?;
    super::ensure_layout(app_dir);

    let todo: Vec<String> = symbols
        .iter()
        .filter(|s| days.iter().any(|d| !has_symbol(app_dir, d, s)))
        .cloned()
        .collect();
    if todo.is_empty() {
        return Ok(0);
    }

    // 04:00 ET of the first day → 20:00 ET of the last, DST-aware at both ends.
    let start = crate::time::et_clock_utc(crate::replay::data::noon_utc(nd_first), 4, 0)
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    let end = crate::time::et_clock_utc(crate::replay::data::noon_utc(nd_last), 20, 0)
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();

    let fetched = crate::replay::data::fetch_bars_window(
        key,
        secret,
        &todo,
        TIMEFRAME,
        &start,
        &end,
        &|_| {},
    )
    .await?;

    // symbol → day → rows. Bars outside 04:00-20:00 ET are dropped so a multi-day
    // window stores exactly what per-day fetches did (the range spans the overnight
    // gaps, which some feeds do print into).
    let mut by_day: std::collections::HashMap<String, Vec<(String, Vec<RawRow>)>> =
        days.iter().map(|d| (d.clone(), Vec::new())).collect();
    for sym in &todo {
        let mut split: std::collections::HashMap<String, Vec<RawRow>> =
            days.iter().map(|d| (d.clone(), Vec::new())).collect();
        for b in fetched.get(sym).map(Vec::as_slice).unwrap_or_default() {
            let Ok(t) = b.t.parse::<chrono::DateTime<Utc>>() else {
                continue;
            };
            if t < crate::time::et_clock_utc(t, 4, 0) || t >= crate::time::et_clock_utc(t, 20, 0) {
                continue;
            }
            let Some(slot) = split.get_mut(&crate::time::et_date(t)) else {
                continue;
            };
            slot.push((t.timestamp_millis(), b.o, b.h, b.l, b.c, b.v, b.n, b.vw));
        }
        for (day, rows) in split {
            // Pushed even when empty: the marker prevents an endless re-fetch loop for
            // a symbol that simply did not trade that session.
            if let Some(bucket) = by_day.get_mut(&day) {
                bucket.push((sym.clone(), rows));
            }
        }
    }

    let mut n = 0usize;
    for day in days {
        if let Some(per_symbol) = by_day.get(day) {
            store_day(app_dir, day, per_symbol)?;
            n += per_symbol.len();
        }
    }
    Ok(n)
}

// ─── Calendar ──────────────────────────────────────────────────────────────────

/// Describe every day present on disk for this kind (ascending).
pub fn calendar(app_dir: &Path) -> Vec<FlatFileDay> {
    let dir = kind_dir(app_dir, Kind::Intraday);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<FlatFileDay> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(day) = super::day_of_file(&name, "i1m-") else {
            continue;
        };
        let bytes = entry.metadata().map(|m| m.len()).unwrap_or(0);
        let (symbol_count, bar_count) = open_db(&entry.path())
            .and_then(|c| {
                let s: i64 = c.query_row("SELECT COUNT(*) FROM symbol_days", [], |r| r.get(0))?;
                let b: i64 = c.query_row("SELECT COUNT(*) FROM bars", [], |r| r.get(0))?;
                Ok((s, b))
            })
            .unwrap_or((0, 0));
        out.push(FlatFileDay {
            day,
            bytes,
            symbol_count,
            bar_count,
            complete: true,
        });
    }
    out.sort_by(|a, b| a.day.cmp(&b.day));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("tagdash-i1m-{n}"));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// 09:30:00Z-aligned base + `i` minutes.
    const BASE: i64 = 1_781_530_200_000; // 2026-06-15T13:30:00Z
    fn minute(i: i64, o: f64, h: f64, l: f64, c: f64, v: i64) -> RawRow {
        (
            BASE + i * 60_000,
            o,
            h,
            l,
            c,
            v,
            Some(10),
            Some((h + l) / 2.0),
        )
    }

    #[test]
    fn one_minute_bars_aggregate_into_five_minute_candles() {
        let dir = tmp();
        let day = "2026-06-15";
        // Six minutes: the first five form one 5m candle, the sixth opens the next.
        let rows = vec![
            minute(0, 10.0, 10.5, 9.8, 10.2, 100),
            minute(1, 10.2, 10.9, 10.1, 10.8, 200),
            minute(2, 10.8, 11.0, 10.0, 10.4, 300),
            minute(3, 10.4, 10.6, 10.3, 10.5, 400),
            minute(4, 10.5, 10.7, 10.4, 10.6, 500),
            minute(5, 10.6, 12.0, 10.6, 11.9, 600),
        ];
        store(&dir, day, "MTEN", &rows).unwrap();

        let m5 = read_symbol_day(&dir, day, "MTEN", Timeframe::M5, None);
        assert_eq!(m5.len(), 2, "6 minutes span two 5-minute buckets");
        // First candle: OHLC assembled across the five minutes.
        assert_eq!(m5[0].open, 10.0);
        assert_eq!(m5[0].high, 11.0);
        assert_eq!(m5[0].low, 9.8);
        assert_eq!(m5[0].close, 10.6);
        assert_eq!(m5[0].volume, 100 + 200 + 300 + 400 + 500);
        assert_eq!(m5[1].open, 10.6);
        assert_eq!(m5[1].close, 11.9);

        // The same data at 1m is untouched.
        let m1 = read_symbol_day(&dir, day, "MTEN", Timeframe::M1, None);
        assert_eq!(m1.len(), 6);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lookahead_cap_yields_a_partial_forming_candle() {
        let dir = tmp();
        let day = "2026-06-15";
        let rows: Vec<RawRow> = (0..6)
            .map(|i| minute(i, 10.0, 10.0 + i as f64, 10.0, 10.0 + i as f64, 100))
            .collect();
        store(&dir, day, "AAA", &rows).unwrap();

        // Simulated clock three minutes in: only the elapsed minutes are visible, and
        // the 5-minute candle is legitimately still forming.
        let m5 = read_symbol_day(&dir, day, "AAA", Timeframe::M5, Some(BASE + 3 * 60_000));
        assert_eq!(m5.len(), 1);
        assert_eq!(m5[0].volume, 300, "only the 3 elapsed minutes count");
        assert_eq!(m5[0].close, 12.0, "close of the last elapsed minute");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn vwap_is_volume_weighted_across_the_bucket() {
        let dir = tmp();
        let day = "2026-06-15";
        // Two minutes, vw 10 (vol 100) and vw 20 (vol 300) → weighted 17.5.
        let rows = vec![
            (BASE, 10.0, 10.0, 10.0, 10.0, 100i64, Some(1i64), Some(10.0)),
            (
                BASE + 60_000,
                20.0,
                20.0,
                20.0,
                20.0,
                300,
                Some(1),
                Some(20.0),
            ),
        ];
        store(&dir, day, "VW", &rows).unwrap();
        let m5 = read_symbol_day(&dir, day, "VW", Timeframe::M5, None);
        assert_eq!(m5.len(), 1);
        let vwap = m5[0].vwap.expect("vwap computed");
        assert!((vwap - 17.5).abs() < 1e-9, "expected 17.5, got {vwap}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_fetch_is_remembered_so_it_is_not_re_requested() {
        let dir = tmp();
        store(&dir, "2026-06-15", "QUIET", &[]).unwrap();
        assert!(has_symbol(&dir, "2026-06-15", "QUIET"));
        assert!(!has_symbol(&dir, "2026-06-15", "OTHER"));
        assert!(read_symbol_day(&dir, "2026-06-15", "QUIET", Timeframe::M5, None).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn upsert_refreshes_a_bar_in_place() {
        let dir = tmp();
        let day = "2026-06-16";
        store(&dir, day, "AAA", &[minute(0, 1.0, 1.0, 1.0, 1.0, 10)]).unwrap();
        store(&dir, day, "AAA", &[minute(0, 2.0, 2.0, 2.0, 2.0, 20)]).unwrap();
        let bars = read_symbol_day(&dir, day, "AAA", Timeframe::M1, None);
        assert_eq!(bars.len(), 1, "same timestamp must not duplicate");
        assert_eq!(bars[0].close, 2.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn history_spans_several_sessions_in_chronological_order() {
        let dir = tmp();
        // Three consecutive sessions, one minute each, 24h apart.
        let days = ["2026-06-15", "2026-06-16", "2026-06-17"];
        for (i, day) in days.iter().enumerate() {
            let t = BASE + i as i64 * 86_400_000;
            store(
                &dir,
                day,
                "PMR",
                &[(t, 1.0, 1.0, 1.0, 1.0 + i as f64, 100, None, None)],
            )
            .unwrap();
        }
        let owned: Vec<String> = days.iter().map(|d| d.to_string()).collect();

        let bars = read_symbol_days(&dir, &owned, "PMR", Timeframe::M5, None);
        assert_eq!(bars.len(), 3, "one candle per session");
        assert!(
            bars.windows(2).all(|w| w[0].time < w[1].time),
            "sessions must concatenate ascending",
        );
        // The shock that put the name on the watchlist is visible, not just its aftermath.
        assert_eq!(bars[0].close, 1.0);
        assert_eq!(bars[2].close, 3.0);

        // A single session still reads exactly as before.
        assert_eq!(
            read_symbol_day(&dir, days[2], "PMR", Timeframe::M5, None).len(),
            1
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn history_tolerates_sessions_that_were_never_downloaded() {
        let dir = tmp();
        store(
            &dir,
            "2026-06-17",
            "PMR",
            &[minute(0, 1.0, 1.0, 1.0, 1.0, 100)],
        )
        .unwrap();
        let days: Vec<String> = ["2026-06-15", "2026-06-16", "2026-06-17"]
            .iter()
            .map(|d| d.to_string())
            .collect();
        // Two missing files → a gap, not an error.
        assert_eq!(
            read_symbol_days(&dir, &days, "PMR", Timeframe::M5, None).len(),
            1
        );
        assert_eq!(
            missing_days(&dir, &days, "PMR"),
            vec!["2026-06-15", "2026-06-16"]
        );
        // Nothing left to fetch once every session is marked.
        assert!(missing_days(&dir, &days[2..], "PMR").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lookahead_cap_applies_across_a_multi_session_read() {
        let dir = tmp();
        let days: Vec<String> = ["2026-06-15", "2026-06-16"]
            .iter()
            .map(|d| d.to_string())
            .collect();
        store(&dir, &days[0], "AAA", &[minute(0, 1.0, 1.0, 1.0, 1.0, 100)]).unwrap();
        store(
            &dir,
            &days[1],
            "AAA",
            &[(BASE + 86_400_000, 2.0, 2.0, 2.0, 2.0, 100, None, None)],
        )
        .unwrap();

        // Clock sits mid-way through the first session: the past session is fully
        // visible, the simulated future is not.
        let bars = read_symbol_days(&dir, &days, "AAA", Timeframe::M5, Some(BASE + 60_000));
        assert_eq!(bars.len(), 1, "the later session must stay invisible");
        assert_eq!(bars[0].close, 1.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn store_day_writes_every_symbol_in_one_pass() {
        let dir = tmp();
        let day = "2026-06-15";
        store_day(
            &dir,
            day,
            &[
                ("AAA".into(), vec![minute(0, 1.0, 1.0, 1.0, 1.0, 100)]),
                ("BBB".into(), vec![minute(0, 2.0, 2.0, 2.0, 2.0, 200)]),
                ("QUIET".into(), vec![]), // did not trade — still marked
            ],
        )
        .unwrap();
        assert!(has_symbol(&dir, day, "AAA"));
        assert!(has_symbol(&dir, day, "BBB"));
        assert!(has_symbol(&dir, day, "QUIET"));
        assert_eq!(
            read_symbol_day(&dir, day, "BBB", Timeframe::M1, None)[0].close,
            2.0
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn serves_only_minute_and_coarser_intraday() {
        assert!(serves(Timeframe::M1));
        assert!(serves(Timeframe::M5));
        assert!(serves(Timeframe::M15));
        assert!(
            !serves(Timeframe::S10),
            "sub-minute frames need trade ticks"
        );
        assert!(
            !serves(Timeframe::Daily),
            "daily comes from the daily cache"
        );
    }
}
