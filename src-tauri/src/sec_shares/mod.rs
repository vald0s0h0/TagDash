// Market capitalisation from SEC EDGAR — free, keyless, authoritative.
//
// WHY: the Panic Mean Reversion watchlist ranks inside SIZE BUCKETS so no category is
// crowded out (measured, a raw volume-expansion term returned 84.7% nano-caps against
// 5.3% in the universe). That needs a market cap for the whole US universe, and no
// free commercial API covers it reliably.
//
// The SEC's XBRL `frames` endpoint does, in ONE request per quarter:
//
//   GET https://data.sec.gov/api/xbrl/frames/dei/EntityCommonStockSharesOutstanding
//       /shares/CY2026Q1I.json      → ~4600 issuers, ~600 KB
//
// market cap = shares outstanding × close.
//
// POINT-IN-TIME BY CONSTRUCTION: every quarterly frame is kept rather than
// overwritten, so `market_caps_as_of(day)` picks the newest frame whose `period_end`
// is on or before `day`, and multiplies by the last close strictly before `day`. A
// Market Replay of an old date therefore sees the cap as it was knowable then — no
// look-ahead, the same discipline as every other daily series in this app.
//
// All the transport already exists in `crate::company_intel::providers::sec_edgar`
// (User-Agent per SEC's fair-access policy, ~10 req/s limiter, retry policy); this
// module only owns the concept choice, the persistence and the as-of resolution.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rusqlite::Connection;

use crate::company_intel::http::{Http, RetryPolicy};
use crate::company_intel::providers::sec_edgar;
use crate::company_intel::rate_limit::RateLimiter;

/// SEC asks for ≤10 requests/second; stay well under.
const MIN_INTERVAL_MS: u64 = 150;
/// How many recent quarterly frames to merge. Companies file on staggered calendars,
/// so a single frame misses many issuers; four quarters gives broad coverage.
const FRAMES: usize = 4;
/// Days a quarter must be closed before its frame is likely published.
const FRAME_LAG_DAYS: i64 = 75;
/// The XBRL concept carrying shares outstanding on the cover page of every filing.
const CONCEPT: (&str, &str, &str) = ("dei", "EntityCommonStockSharesOutstanding", "shares");

// ─── Persistence ───────────────────────────────────────────────────────────────

/// Upsert the ticker→CIK map (one SEC request covers the whole market).
fn store_ticker_map(conn: &Connection, map: &HashMap<String, String>) -> rusqlite::Result<usize> {
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let tx = conn.unchecked_transaction()?;
    let mut n = 0usize;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO sec_ticker_cik (symbol, cik, updated_at) VALUES (?1,?2,?3)
             ON CONFLICT(symbol) DO UPDATE SET cik=excluded.cik, updated_at=excluded.updated_at",
        )?;
        for (symbol, cik) in map {
            stmt.execute(rusqlite::params![symbol, cik, now])?;
            n += 1;
        }
    }
    tx.commit()?;
    Ok(n)
}

/// Upsert one frame's `(cik, period_end, shares)` rows.
fn store_frame(
    conn: &Connection,
    frame: &str,
    rows: &[(String, String, f64)],
) -> rusqlite::Result<usize> {
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let tx = conn.unchecked_transaction()?;
    let mut n = 0usize;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO sec_shares_outstanding (cik, period_end, shares, frame, updated_at)
             VALUES (?1,?2,?3,?4,?5)
             ON CONFLICT(cik, period_end) DO UPDATE SET
                 shares=excluded.shares, frame=excluded.frame, updated_at=excluded.updated_at",
        )?;
        for (cik, period_end, shares) in rows {
            if !shares.is_finite() || *shares <= 0.0 {
                continue;
            }
            stmt.execute(rusqlite::params![cik, period_end, shares, frame, now])?;
            n += 1;
        }
    }
    tx.commit()?;
    Ok(n)
}

// ─── Refresh ───────────────────────────────────────────────────────────────────

/// Fetch the ticker→CIK map and the last `FRAMES` quarterly shares-outstanding frames,
/// persisting both. Cheap enough to run quarterly: `FRAMES + 1` HTTP requests total.
/// Returns the number of shares rows stored.
pub async fn refresh(db: &Arc<Mutex<Connection>>) -> Result<usize, String> {
    let http = Http::new("sec_shares");
    let limiter = RateLimiter::new(std::time::Duration::from_millis(MIN_INTERVAL_MS));
    let policy = RetryPolicy::default();

    let map = sec_edgar::fetch_ticker_cik_map(&http, &limiter, &policy)
        .await
        .map_err(|e| format!("ticker→CIK map: {e}"))?;
    {
        let conn = db.lock().unwrap();
        let n = store_ticker_map(&conn, &map).map_err(|e| e.to_string())?;
        eprintln!("[tagdash] sec_shares: {n} ticker→CIK mappings stored");
    }

    // App clock: during a replay this walks back from the simulated day, so a refresh
    // triggered mid-replay still fetches frames that were published by then.
    let today = crate::time::now().date_naive();
    let frames = sec_edgar::recent_quarter_frames(today, FRAMES, FRAME_LAG_DAYS, true);

    let mut total = 0usize;
    for frame in &frames {
        match sec_edgar::fetch_concept_frame(
            &http, &limiter, &policy, CONCEPT.0, CONCEPT.1, CONCEPT.2, frame,
        )
        .await
        {
            Ok(rows) => {
                let conn = db.lock().unwrap();
                match store_frame(&conn, frame, &rows) {
                    Ok(n) => {
                        total += n;
                        eprintln!("[tagdash] sec_shares: {frame} → {n} issuers");
                    }
                    Err(e) => eprintln!("[tagdash] sec_shares: {frame} store failed: {e}"),
                }
            }
            // A frame that isn't published yet is expected, not an error.
            Err(e) => eprintln!("[tagdash] sec_shares: {frame} skipped ({e})"),
        }
    }
    if total == 0 {
        return Err("aucun frame SEC exploitable (toutes les requêtes ont échoué)".into());
    }
    Ok(total)
}

// ─── As-of resolution ──────────────────────────────────────────────────────────

/// Market cap per symbol as knowable on `day` (ET date, exclusive for the price).
///
/// Two sources, SEC first:
///   1. The newest shares-outstanding frame whose `period_end <= day`, times the last
///      daily close strictly before `day` — authoritative and point-in-time.
///   2. FALLBACK: `universe_assets.market_cap`, for symbols the SEC path can't
///      resolve (no CIK, no frame yet, or `refresh` never run). This is a static
///      snapshot, NOT point-in-time — but without it, a database with empty SEC
///      tables puts every ticker in the "unknown" context bucket and removes useful
///      structure information from the contextual analysis.
///
/// Symbols neither source resolves are absent; callers bucket them as "unknown"
/// rather than dropping them, so ADRs and foreign issuers still reach the watchlist.
pub fn market_caps_as_of(conn: &Connection, day: &str) -> HashMap<String, f64> {
    let mut out = fallback_caps(conn);
    out.extend(sec_caps_as_of(conn, day)); // SEC wins where available
    out
}

/// The static per-symbol cap stored on the universe table (Massive/FMP).
fn fallback_caps(conn: &Connection) -> HashMap<String, f64> {
    let Ok(mut stmt) = conn.prepare(
        "SELECT symbol, market_cap FROM universe_assets
          WHERE tradable=1 AND market_cap IS NOT NULL AND market_cap > 0",
    ) else {
        return HashMap::new();
    };
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?)));
    let Ok(rows) = rows else {
        return HashMap::new();
    };
    rows.flatten()
        .filter(|(_, c)| c.is_finite() && *c > 0.0)
        .collect()
}

/// The SEC path on its own (point-in-time).
fn sec_caps_as_of(conn: &Connection, day: &str) -> HashMap<String, f64> {
    let sql = "
        SELECT t.symbol, s.shares,
               (SELECT d.close FROM daily_cache d
                 WHERE d.symbol = t.symbol AND d.date < ?1 AND d.close IS NOT NULL
                 ORDER BY d.date DESC LIMIT 1) AS px
          FROM sec_ticker_cik t
          JOIN sec_shares_outstanding s ON s.cik = t.cik
         WHERE s.period_end <= ?1
           AND s.period_end = (SELECT MAX(s2.period_end)
                                 FROM sec_shares_outstanding s2
                                WHERE s2.cik = t.cik AND s2.period_end <= ?1)";
    let Ok(mut stmt) = conn.prepare(sql) else {
        return HashMap::new();
    };
    let rows = stmt.query_map(rusqlite::params![day], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, f64>(1)?,
            r.get::<_, Option<f64>>(2)?,
        ))
    });
    let Ok(rows) = rows else {
        return HashMap::new();
    };

    let mut out = HashMap::new();
    for (symbol, shares, px) in rows.flatten() {
        let Some(px) = px else { continue };
        let cap = shares * px;
        if cap.is_finite() && cap > 0.0 {
            out.insert(symbol, cap);
        }
    }
    out
}

/// How many symbols currently resolve to a market cap — surfaced in logs/settings so
/// a thin coverage is visible rather than silently skewing the buckets.
pub fn coverage(conn: &Connection, day: &str) -> usize {
    market_caps_as_of(conn, day).len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::local_db::schema::migrate(&conn).unwrap();
        conn
    }

    fn seed(conn: &Connection) {
        conn.execute(
            "INSERT INTO sec_ticker_cik (symbol, cik, updated_at) VALUES ('AAA','0000000001','x')",
            [],
        )
        .unwrap();
        for (pe, sh) in [("2025-12-31", 1_000_000.0), ("2026-03-31", 2_000_000.0)] {
            conn.execute(
                "INSERT INTO sec_shares_outstanding (cik, period_end, shares, frame, updated_at)
                 VALUES ('0000000001', ?1, ?2, 'F', 'x')",
                rusqlite::params![pe, sh],
            )
            .unwrap();
        }
        for (d, c) in [("2026-06-10", 10.0), ("2026-06-15", 99.0)] {
            conn.execute(
                "INSERT INTO daily_cache (symbol, date, open, high, low, close, volume, updated_at)
                 VALUES ('AAA', ?1, 1,1,1, ?2, 1, 'x')",
                rusqlite::params![d, c],
            )
            .unwrap();
        }
    }

    #[test]
    fn uses_the_newest_frame_on_or_before_the_day() {
        let conn = db();
        seed(&conn);
        // On 2026-06-15 the Q1 frame (2M shares) applies, priced at the 06-10 close.
        let caps = market_caps_as_of(&conn, "2026-06-15");
        assert_eq!(caps.get("AAA").copied(), Some(2_000_000.0 * 10.0));
    }

    #[test]
    fn never_uses_a_future_frame_or_a_future_price() {
        let conn = db();
        seed(&conn);
        // On 2026-02-01 only the 2025-12-31 frame exists, and there is no earlier
        // price at all → the SEC path yields nothing for that date.
        assert!(sec_caps_as_of(&conn, "2026-02-01").get("AAA").is_none());

        // The 06-15 close (99.0) must never leak into the 06-15 valuation.
        let caps = market_caps_as_of(&conn, "2026-06-15");
        assert_ne!(caps.get("AAA").copied(), Some(2_000_000.0 * 99.0));
    }

    #[test]
    fn missing_mapping_yields_no_entry() {
        let conn = db();
        assert!(market_caps_as_of(&conn, "2026-06-15").is_empty());
    }

    /// With empty SEC tables every ticker would otherwise land in the "unknown"
    /// bucket, collapsing the size-neutral ranking to a single bucket.
    #[test]
    fn falls_back_to_the_universe_cap_when_sec_data_is_absent() {
        let conn = db();
        conn.execute(
            "INSERT INTO universe_assets (symbol, tradable, market_cap, updated_at)
             VALUES ('ZZZ', 1, 5000000000, 'x')",
            [],
        )
        .unwrap();
        let caps = market_caps_as_of(&conn, "2026-06-15");
        assert_eq!(caps.get("ZZZ").copied(), Some(5.0e9));
    }

    /// When both sources have the symbol, the point-in-time SEC value wins.
    #[test]
    fn sec_overrides_the_static_fallback() {
        let conn = db();
        seed(&conn);
        conn.execute(
            "INSERT INTO universe_assets (symbol, tradable, market_cap, updated_at)
             VALUES ('AAA', 1, 999, 'x')",
            [],
        )
        .unwrap();
        let caps = market_caps_as_of(&conn, "2026-06-15");
        assert_eq!(caps.get("AAA").copied(), Some(2_000_000.0 * 10.0));
    }
}
