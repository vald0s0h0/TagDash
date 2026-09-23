// Cache for the premarket (04:00–09:30 ET) daily candles folded by
// `crate::premarket`. A closed day's premarket never changes, so a row written
// once is authoritative forever — the cache is what makes a second alert on the
// same ticker instant, and what keeps a scroll-back into history from re-hitting
// the provider for days it already has.
//
// The coverage table is the load-bearing half. A day on which the symbol never
// printed in premarket legitimately yields NO row, so the absence of a row cannot
// be read as "not fetched yet". Coverage records the span that was ASKED for.

use rusqlite::{params, Connection, Result};

use crate::market_state::aggregators::Bar;

/// One cached premarket candle. `date` is the NY trading day (`YYYY-MM-DD`).
#[derive(Debug, Clone)]
pub struct PremarketBar {
    pub date: String,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: i64,
    pub vwap: Option<f64>,
    pub trade_count: Option<i64>,
}

/// Store one folded premarket candle. Idempotent: re-running a fetch over an
/// already-cached day rewrites the identical values.
pub fn upsert(conn: &Connection, symbol: &str, bar: &Bar) -> Result<()> {
    conn.execute(
        "INSERT INTO premarket_daily
           (symbol,date,open,high,low,close,volume,vwap,trade_count,updated_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,datetime('now'))
         ON CONFLICT(symbol,date) DO UPDATE SET
           open=excluded.open, high=excluded.high, low=excluded.low,
           close=excluded.close, volume=excluded.volume, vwap=excluded.vwap,
           trade_count=excluded.trade_count, updated_at=excluded.updated_at",
        params![
            symbol,
            crate::time::et_date(bar.time),
            bar.open,
            bar.high,
            bar.low,
            bar.close,
            bar.volume as i64,
            bar.vwap,
            bar.trade_count.map(|n| n as i64),
        ],
    )?;
    Ok(())
}

/// Store a whole folded window in one transaction.
pub fn upsert_many(conn: &mut Connection, symbol: &str, bars: &[Bar]) -> Result<()> {
    let tx = conn.transaction()?;
    for b in bars {
        upsert(&tx, symbol, b)?;
    }
    tx.commit()
}

/// Cached premarket candles for `symbol` over [`from_date`, `to_date`] inclusive,
/// date-ascending.
pub fn get_range(
    conn: &Connection,
    symbol: &str,
    from_date: &str,
    to_date: &str,
) -> Result<Vec<PremarketBar>> {
    let mut stmt = conn.prepare(
        "SELECT date,open,high,low,close,volume,vwap,trade_count
           FROM premarket_daily
          WHERE symbol=?1 AND date>=?2 AND date<=?3
          ORDER BY date ASC",
    )?;
    let rows = stmt.query_map(params![symbol, from_date, to_date], |r| {
        Ok(PremarketBar {
            date: r.get(0)?,
            open: r.get(1)?,
            high: r.get(2)?,
            low: r.get(3)?,
            close: r.get(4)?,
            volume: r.get(5)?,
            vwap: r.get(6)?,
            trade_count: r.get(7)?,
        })
    })?;
    rows.collect()
}

/// The date span already fetched for `symbol`, as (first_date, last_date).
pub fn get_coverage(conn: &Connection, symbol: &str) -> Option<(String, String)> {
    conn.query_row(
        "SELECT first_date,last_date FROM premarket_coverage WHERE symbol=?1",
        params![symbol],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .ok()
}

/// Widen the recorded coverage to include [`first_date`, `last_date`]. Never
/// narrows it: a request for a shorter window must not forget history we hold.
pub fn extend_coverage(
    conn: &Connection,
    symbol: &str,
    first_date: &str,
    last_date: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO premarket_coverage (symbol,first_date,last_date,updated_at)
         VALUES (?1,?2,?3,datetime('now'))
         ON CONFLICT(symbol) DO UPDATE SET
           first_date=MIN(first_date,excluded.first_date),
           last_date =MAX(last_date, excluded.last_date),
           updated_at=excluded.updated_at",
        params![symbol, first_date, last_date],
    )?;
    Ok(())
}

/// A cached row back as a chart `Bar`, stamped at NY midnight so it lands on the
/// same daily slot as the regular-session candle it belongs under.
pub fn to_bar(row: &PremarketBar) -> Option<Bar> {
    let naive = chrono::NaiveDate::parse_from_str(&row.date, "%Y-%m-%d").ok()?;
    // Noon UTC on the date is unambiguously inside that NY calendar day whatever
    // the DST offset, so `et_midnight_utc` resolves the right stamp from it.
    let anchor = naive.and_hms_opt(12, 0, 0)?.and_utc();
    Some(Bar {
        time: crate::time::et_midnight_utc(anchor),
        open: row.open,
        high: row.high,
        low: row.low,
        close: row.close,
        volume: row.volume.max(0) as u64,
        vwap: row.vwap,
        trade_count: row.trade_count.map(|n| n.max(0) as u64),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn bar(day: u32, open: f64, high: f64, low: f64, close: f64, v: u64) -> Bar {
        Bar {
            // NY midnight of 2026-09-`day` under EDT.
            time: chrono::Utc
                .with_ymd_and_hms(2026, 9, day, 4, 0, 0)
                .single()
                .unwrap(),
            open,
            high,
            low,
            close,
            volume: v,
            vwap: Some(1.0),
            trade_count: Some(7),
        }
    }

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::local_db::schema::migrate(&conn).unwrap();
        conn
    }

    #[test]
    fn a_stored_candle_comes_back_with_the_same_ohlc_and_daily_slot() {
        let conn = db();
        let b = bar(18, 0.0819, 0.3023, 0.0813, 0.142, 376_052_720);
        upsert(&conn, "SNYR", &b).unwrap();
        let rows = get_range(&conn, "SNYR", "2026-09-01", "2026-09-30").unwrap();
        assert_eq!(rows.len(), 1);
        let back = to_bar(&rows[0]).unwrap();
        assert_eq!(back.time, b.time, "must land on the same daily slot as the regular candle");
        assert_eq!((back.open, back.high, back.low, back.close), (b.open, b.high, b.low, b.close));
        assert_eq!(back.volume, b.volume);
        assert_eq!(back.trade_count, Some(7));
    }

    #[test]
    fn re_storing_a_day_overwrites_rather_than_duplicating_it() {
        let conn = db();
        upsert(&conn, "AAA", &bar(18, 1.0, 2.0, 0.5, 1.5, 10)).unwrap();
        upsert(&conn, "AAA", &bar(18, 1.0, 9.0, 0.5, 1.5, 99)).unwrap();
        let rows = get_range(&conn, "AAA", "2026-09-18", "2026-09-18").unwrap();
        assert_eq!(rows.len(), 1, "one candle per day, always");
        assert_eq!(rows[0].high, 9.0);
        assert_eq!(rows[0].volume, 99);
    }

    #[test]
    fn get_range_is_inclusive_on_both_ends_and_ascending() {
        let mut conn = db();
        let bars: Vec<Bar> = (17..=19).map(|d| bar(d, 1.0, 1.0, 1.0, 1.0, 1)).collect();
        upsert_many(&mut conn, "AAA", &bars).unwrap();
        let rows = get_range(&conn, "AAA", "2026-09-17", "2026-09-19").unwrap();
        assert_eq!(
            rows.iter().map(|r| r.date.as_str()).collect::<Vec<_>>(),
            ["2026-09-17", "2026-09-18", "2026-09-19"],
        );
        let narrow = get_range(&conn, "AAA", "2026-09-18", "2026-09-18").unwrap();
        assert_eq!(narrow.len(), 1);
    }

    #[test]
    fn coverage_only_ever_widens() {
        let conn = db();
        extend_coverage(&conn, "AAA", "2026-07-01", "2026-09-01").unwrap();
        // A narrower window must NOT shrink what we already hold, or the next chart
        // open re-fetches history that is already cached.
        extend_coverage(&conn, "AAA", "2026-08-01", "2026-08-15").unwrap();
        assert_eq!(
            get_coverage(&conn, "AAA"),
            Some(("2026-07-01".into(), "2026-09-01".into())),
        );
        // Scrolling back + a new session widen both ends.
        extend_coverage(&conn, "AAA", "2026-05-01", "2026-05-31").unwrap();
        extend_coverage(&conn, "AAA", "2026-09-02", "2026-09-20").unwrap();
        assert_eq!(
            get_coverage(&conn, "AAA"),
            Some(("2026-05-01".into(), "2026-09-20".into())),
        );
    }

    #[test]
    fn an_unknown_symbol_has_no_coverage_and_no_rows() {
        let conn = db();
        assert_eq!(get_coverage(&conn, "NOPE"), None);
        assert!(get_range(&conn, "NOPE", "2026-01-01", "2026-12-31").unwrap().is_empty());
    }
}
