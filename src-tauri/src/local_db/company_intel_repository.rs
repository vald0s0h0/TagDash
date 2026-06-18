// Storage for the company-intel job: the normalized `company_intel` table and the
// raw `company_filings` table.
//
// Two design rules make this safe to run repeatedly and partially:
//   1. ADDITIVE — these tables are independent of float / fundamentals; nothing
//      here ever touches `fundamentals_cache` or `universe_assets`.
//   2. PER-SECTION — each section (short interest, financials, dilution, ownership)
//      has its own upsert that writes ONLY its own columns. A run where one
//      provider was down simply skips that section's upsert, so the previous good
//      value is preserved. `touch` advances the overall `last_updated_at` marker.

use rusqlite::{params, Connection, Result};
use std::collections::HashMap;

use crate::company_intel::model::{
    CompanyIntel, DilutionFlags, DilutionInfo, FinancialHealth, Holder, OwnershipInfo, SecFiling,
    ShortInterest, TickerTableRow,
};

/// Ensure a row exists for `symbol` (no-op if already present).
fn ensure_row(conn: &Connection, symbol: &str) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO company_intel (symbol) VALUES (?1)",
        params![symbol],
    )?;
    Ok(())
}

// ─── CIK ──────────────────────────────────────────────────────────────────────

pub fn get_cik(conn: &Connection, symbol: &str) -> Option<String> {
    conn.query_row(
        "SELECT cik FROM company_intel WHERE symbol=?1",
        params![symbol],
        |r| r.get::<_, Option<String>>(0),
    )
    .ok()
    .flatten()
}

pub fn set_cik(conn: &Connection, symbol: &str, cik: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO company_intel (symbol, cik) VALUES (?1, ?2)
         ON CONFLICT(symbol) DO UPDATE SET cik=excluded.cik",
        params![symbol, cik],
    )?;
    Ok(())
}

// ─── Per-section upserts ───────────────────────────────────────────────────────

pub fn upsert_short_interest(
    conn: &Connection,
    symbol: &str,
    si: &ShortInterest,
    source: &str,
    updated_at: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO company_intel
             (symbol, short_interest, days_to_cover, short_interest_settlement,
              short_interest_source, short_interest_updated_at)
         VALUES (?1,?2,?3,?4,?5,?6)
         ON CONFLICT(symbol) DO UPDATE SET
             short_interest=excluded.short_interest,
             days_to_cover=excluded.days_to_cover,
             short_interest_settlement=excluded.short_interest_settlement,
             short_interest_source=excluded.short_interest_source,
             short_interest_updated_at=excluded.short_interest_updated_at",
        params![
            symbol, si.short_interest, si.days_to_cover, si.settlement_date,
            source, updated_at
        ],
    )?;
    Ok(())
}

pub fn upsert_financials(
    conn: &Connection,
    symbol: &str,
    fh: &FinancialHealth,
    source: &str,
    updated_at: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO company_intel
             (symbol, net_income_last_q, net_income_ttm, negative_quarters_last4,
              operating_cash_flow_ttm, cash_and_equivalents, financials_period_end,
              financials_source, financials_updated_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)
         ON CONFLICT(symbol) DO UPDATE SET
             net_income_last_q=excluded.net_income_last_q,
             net_income_ttm=excluded.net_income_ttm,
             negative_quarters_last4=excluded.negative_quarters_last4,
             operating_cash_flow_ttm=excluded.operating_cash_flow_ttm,
             cash_and_equivalents=excluded.cash_and_equivalents,
             financials_period_end=excluded.financials_period_end,
             financials_source=excluded.financials_source,
             financials_updated_at=excluded.financials_updated_at",
        params![
            symbol, fh.net_income_last_q, fh.net_income_ttm, fh.negative_quarters_last4,
            fh.operating_cash_flow_ttm, fh.cash_and_equivalents, fh.period_end,
            source, updated_at
        ],
    )?;
    Ok(())
}

pub fn upsert_dilution(
    conn: &Connection,
    symbol: &str,
    d: &DilutionInfo,
    source: &str,
    updated_at: &str,
) -> Result<()> {
    let flags_json = serde_json::to_string(&d.flags).unwrap_or_else(|_| "{}".into());
    conn.execute(
        "INSERT INTO company_intel
             (symbol, has_recent_shelf, latest_dilution_form, latest_dilution_date,
              dilution_flags, dilution_source, dilution_updated_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7)
         ON CONFLICT(symbol) DO UPDATE SET
             has_recent_shelf=excluded.has_recent_shelf,
             latest_dilution_form=excluded.latest_dilution_form,
             latest_dilution_date=excluded.latest_dilution_date,
             dilution_flags=excluded.dilution_flags,
             dilution_source=excluded.dilution_source,
             dilution_updated_at=excluded.dilution_updated_at",
        params![
            symbol, d.has_recent_shelf as i64, d.latest_form, d.latest_date,
            flags_json, source, updated_at
        ],
    )?;
    Ok(())
}

pub fn upsert_ownership(
    conn: &Connection,
    symbol: &str,
    o: &OwnershipInfo,
    source: &str,
    updated_at: &str,
) -> Result<()> {
    let holders_json = serde_json::to_string(&o.holders_5pct).unwrap_or_else(|_| "[]".into());
    conn.execute(
        "INSERT INTO company_intel
             (symbol, institutional_ownership_pct, insider_ownership_pct,
              holders_5pct_count, holders_5pct, restricted_shares,
              ownership_source, ownership_updated_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
         ON CONFLICT(symbol) DO UPDATE SET
             institutional_ownership_pct=excluded.institutional_ownership_pct,
             insider_ownership_pct=excluded.insider_ownership_pct,
             holders_5pct_count=excluded.holders_5pct_count,
             holders_5pct=excluded.holders_5pct,
             restricted_shares=excluded.restricted_shares,
             ownership_source=excluded.ownership_source,
             ownership_updated_at=excluded.ownership_updated_at",
        params![
            symbol, o.institutional_ownership_pct, o.insider_ownership_pct,
            o.holders_5pct.len() as i64, holders_json, o.restricted_shares,
            source, updated_at
        ],
    )?;
    Ok(())
}

/// Advance the overall cache marker + record any per-section errors (JSON).
pub fn touch(conn: &Connection, symbol: &str, now: &str, last_errors: Option<&str>) -> Result<()> {
    ensure_row(conn, symbol)?;
    conn.execute(
        "UPDATE company_intel SET last_updated_at=?2, last_errors=?3 WHERE symbol=?1",
        params![symbol, now, last_errors],
    )?;
    Ok(())
}

// ─── Filings ───────────────────────────────────────────────────────────────────

pub fn upsert_filing(conn: &Connection, f: &SecFiling) -> Result<()> {
    conn.execute(
        "INSERT INTO company_filings
             (accession_number, symbol, cik, form_type, filing_date, report_date,
              primary_document, document_url, description, category,
              detected_atm, detected_resale, detected_warrants, offering_amount, fetched_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14, datetime('now'))
         ON CONFLICT(symbol, accession_number) DO UPDATE SET
             cik=excluded.cik, form_type=excluded.form_type,
             filing_date=excluded.filing_date, report_date=excluded.report_date,
             primary_document=excluded.primary_document, document_url=excluded.document_url,
             description=excluded.description, category=excluded.category,
             detected_atm=excluded.detected_atm, detected_resale=excluded.detected_resale,
             detected_warrants=excluded.detected_warrants, offering_amount=excluded.offering_amount,
             fetched_at=excluded.fetched_at",
        params![
            f.accession_number, f.symbol, f.cik, f.form_type, f.filing_date, f.report_date,
            f.primary_document, f.document_url, f.description, f.category,
            f.detected_atm as i64, f.detected_resale as i64, f.detected_warrants as i64,
            f.offering_amount
        ],
    )?;
    Ok(())
}

pub fn get_recent_filings(conn: &Connection, symbol: &str, limit: u32) -> Result<Vec<SecFiling>> {
    let mut stmt = conn.prepare(
        "SELECT accession_number, symbol, cik, form_type, filing_date, report_date,
                primary_document, document_url, description, category,
                detected_atm, detected_resale, detected_warrants, offering_amount
         FROM company_filings WHERE symbol=?1 ORDER BY filing_date DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![symbol, limit], |r| {
        Ok(SecFiling {
            accession_number: r.get(0)?,
            symbol: r.get(1)?,
            cik: r.get(2)?,
            form_type: r.get(3)?,
            filing_date: r.get(4)?,
            report_date: r.get(5)?,
            primary_document: r.get(6)?,
            document_url: r.get(7)?,
            description: r.get(8)?,
            category: r.get(9)?,
            detected_atm: r.get::<_, i64>(10)? != 0,
            detected_resale: r.get::<_, i64>(11)? != 0,
            detected_warrants: r.get::<_, i64>(12)? != 0,
            offering_amount: r.get(13)?,
        })
    })?;
    rows.collect()
}

// ─── Read-back (UI) + TTL helpers ──────────────────────────────────────────────

/// Read the full normalized record + recent filings for one symbol.
pub fn get_intel(conn: &Connection, symbol: &str) -> Result<Option<CompanyIntel>> {
    let mut stmt = conn.prepare(
        "SELECT symbol, cik,
                short_interest, days_to_cover, short_interest_settlement,
                short_interest_source, short_interest_updated_at,
                net_income_last_q, net_income_ttm, negative_quarters_last4,
                operating_cash_flow_ttm, cash_and_equivalents, financials_period_end,
                financials_source, financials_updated_at,
                has_recent_shelf, latest_dilution_form, latest_dilution_date,
                dilution_flags, dilution_source, dilution_updated_at,
                institutional_ownership_pct, insider_ownership_pct,
                holders_5pct, restricted_shares, ownership_source, ownership_updated_at,
                last_updated_at
         FROM company_intel WHERE symbol=?1",
    )?;
    let mut rows = stmt.query_map(params![symbol], |r| {
        let dilution_flags: Option<String> = r.get(18)?;
        let holders_json: Option<String> = r.get(23)?;
        Ok(CompanyIntel {
            symbol: r.get(0)?,
            cik: r.get(1)?,
            short_interest: ShortInterest {
                short_interest: r.get(2)?,
                days_to_cover: r.get(3)?,
                settlement_date: r.get(4)?,
            },
            short_interest_source: r.get(5)?,
            short_interest_updated_at: r.get(6)?,
            financials: FinancialHealth {
                net_income_last_q: r.get(7)?,
                net_income_ttm: r.get(8)?,
                negative_quarters_last4: r.get(9)?,
                operating_cash_flow_ttm: r.get(10)?,
                cash_and_equivalents: r.get(11)?,
                period_end: r.get(12)?,
            },
            financials_source: r.get(13)?,
            financials_updated_at: r.get(14)?,
            dilution: DilutionInfo {
                has_recent_shelf: r.get::<_, Option<i64>>(15)?.unwrap_or(0) != 0,
                latest_form: r.get(16)?,
                latest_date: r.get(17)?,
                flags: dilution_flags
                    .as_deref()
                    .and_then(|s| serde_json::from_str::<DilutionFlags>(s).ok())
                    .unwrap_or_default(),
            },
            dilution_source: r.get(19)?,
            dilution_updated_at: r.get(20)?,
            ownership: OwnershipInfo {
                institutional_ownership_pct: r.get(21)?,
                insider_ownership_pct: r.get(22)?,
                holders_5pct: holders_json
                    .as_deref()
                    .and_then(|s| serde_json::from_str::<Vec<Holder>>(s).ok())
                    .unwrap_or_default(),
                restricted_shares: r.get(24)?,
            },
            ownership_source: r.get(25)?,
            ownership_updated_at: r.get(26)?,
            last_updated_at: r.get(27)?,
            recent_filings: Vec::new(),
        })
    })?;
    match rows.next().transpose()? {
        Some(mut intel) => {
            drop(rows);
            drop(stmt);
            intel.recent_filings = get_recent_filings(conn, symbol, 20)?;
            Ok(Some(intel))
        }
        None => Ok(None),
    }
}

/// `symbol → last_updated_at` for every cached record. The orchestrator uses this
/// to decide which candidate tickers are stale (TTL) in a single query.
pub fn all_last_updated(conn: &Connection) -> Result<HashMap<String, String>> {
    let mut stmt =
        conn.prepare("SELECT symbol, last_updated_at FROM company_intel")?;
    let rows = stmt.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?.unwrap_or_default()))
    })?;
    rows.collect()
}

pub fn count(conn: &Connection) -> Result<i64> {
    conn.query_row("SELECT COUNT(*) FROM company_intel", [], |r| r.get(0))
}

// ─── Tickers data table (wide reporting join) ──────────────────────────────────

/// The full tickers data table: every tradable universe asset LEFT-JOINed with its
/// fundamentals, company meta and company intel, plus the filings count. `news_count`
/// is left at 0 here (it lives in RAM, not the DB) and is filled by the caller.
///
/// LIGHTWEIGHT BY DESIGN — the full universe is ~10k rows × ~40 columns, which is
/// too large to ship to the UI at once (it crashed the webview). So this returns
/// only a bounded EXTRACT:
///   • empty `query`  → the most recently collected intel rows (proof that data
///                      exists), capped at `limit`;
///   • non-empty `query` → tickers whose symbol starts with, or name contains,
///                      the query (case-insensitive), capped at `limit`.
pub fn tickers_overview(conn: &Connection, query: &str, limit: u32) -> Result<Vec<TickerTableRow>> {
    let limit = limit.clamp(1, 1000);
    let q = query.trim();

    const SELECT_BODY: &str = "SELECT
            u.symbol, u.name, u.exchange, u.tradable, u.shortable,
            u.float_shares, u.market_cap, u.avg_volume,
            f.outstanding_shares, f.free_float, f.prev_close, f.atr,
            f.change_1d_pct, f.change_2d_pct, f.change_3d_pct,
            f.change_4d_pct, f.change_5d_pct, f.change_6d_pct,
            m.country, m.industry, m.sector, m.sic,
            ci.short_interest, ci.days_to_cover, ci.short_interest_settlement,
            ci.net_income_last_q, ci.net_income_ttm, ci.negative_quarters_last4,
            ci.operating_cash_flow_ttm, ci.cash_and_equivalents, ci.financials_period_end,
            ci.has_recent_shelf, ci.latest_dilution_form, ci.latest_dilution_date, ci.dilution_flags,
            ci.institutional_ownership_pct, ci.insider_ownership_pct,
            ci.holders_5pct_count, ci.restricted_shares, ci.last_updated_at,
            (SELECT COUNT(*) FROM company_filings cf WHERE cf.symbol = u.symbol) AS filings_count,
            f.pump_dump_score, f.dilution_score, f.dilution_pct_12m, f.shares_outstanding_12m,
            f.last_split_date, f.last_split_label, f.split_count_1y,
            f.dilution_capacity_score, f.dilution_need_score, f.short_interest_score
         FROM universe_assets u
         LEFT JOIN fundamentals_cache f ON f.symbol = u.symbol
         LEFT JOIN company_meta       m ON m.symbol = u.symbol
         LEFT JOIN company_intel     ci ON ci.symbol = u.symbol";

    if q.is_empty() {
        let sql = format!(
            "{SELECT_BODY}
             WHERE u.tradable = 1 AND ci.last_updated_at IS NOT NULL
             ORDER BY ci.last_updated_at DESC
             LIMIT ?1"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![limit], map_overview_row)?;
        rows.collect()
    } else {
        let sql = format!(
            "{SELECT_BODY}
             WHERE u.tradable = 1 AND (u.symbol LIKE ?1 OR u.name LIKE ?2)
             ORDER BY (u.symbol = ?3) DESC, length(u.symbol), u.symbol
             LIMIT ?4"
        );
        let prefix = format!("{}%", q.to_uppercase());
        let contains = format!("%{q}%");
        let exact = q.to_uppercase();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![prefix, contains, exact, limit], map_overview_row)?;
        rows.collect()
    }
}

/// Map one joined overview row. Column order MUST match `SELECT_BODY` above.
fn map_overview_row(r: &rusqlite::Row) -> Result<TickerTableRow> {
    let flags = r
        .get::<_, Option<String>>(34)?
        .as_deref()
        .and_then(|s| serde_json::from_str::<DilutionFlags>(s).ok())
        .unwrap_or_default();
    Ok(TickerTableRow {
        symbol: r.get(0)?,
        name: r.get(1)?,
        exchange: r.get(2)?,
        tradable: r.get::<_, i64>(3)? != 0,
        shortable: r.get::<_, i64>(4)? != 0,
        float_shares: r.get(5)?,
        market_cap: r.get(6)?,
        avg_volume: r.get(7)?,
        outstanding_shares: r.get(8)?,
        free_float: r.get(9)?,
        prev_close: r.get(10)?,
        atr: r.get(11)?,
        change_1d_pct: r.get(12)?,
        change_2d_pct: r.get(13)?,
        change_3d_pct: r.get(14)?,
        change_4d_pct: r.get(15)?,
        change_5d_pct: r.get(16)?,
        change_6d_pct: r.get(17)?,
        country: r.get(18)?,
        industry: r.get(19)?,
        sector: r.get(20)?,
        sic: r.get(21)?,
        short_interest: r.get(22)?,
        days_to_cover: r.get(23)?,
        short_interest_settlement: r.get(24)?,
        net_income_last_q: r.get(25)?,
        net_income_ttm: r.get(26)?,
        negative_quarters_last4: r.get(27)?,
        operating_cash_flow_ttm: r.get(28)?,
        cash_and_equivalents: r.get(29)?,
        financials_period_end: r.get(30)?,
        has_recent_shelf: r.get::<_, Option<i64>>(31)?.unwrap_or(0) != 0,
        latest_dilution_form: r.get(32)?,
        latest_dilution_date: r.get(33)?,
        dilution_atm: flags.atm,
        dilution_resale: flags.resale,
        dilution_warrants: flags.warrants,
        offering_amount: flags.offering_amount,
        institutional_ownership_pct: r.get(35)?,
        insider_ownership_pct: r.get(36)?,
        holders_5pct_count: r.get(37)?,
        restricted_shares: r.get(38)?,
        filings_count: r.get(40)?,
        news_count: 0,
        intel_updated_at: r.get(39)?,
        // Appended columns (41..47) — see SELECT_BODY.
        pump_dump_score: r.get(41)?,
        dilution_score: r.get(42)?,
        dilution_pct_12m: r.get(43)?,
        shares_outstanding_12m: r.get(44)?,
        last_split_date: r.get(45)?,
        last_split_label: r.get(46)?,
        split_count_1y: r.get(47)?,
        dilution_capacity_score: r.get(48)?,
        dilution_need_score: r.get(49)?,
        short_interest_score: r.get(50)?,
    })
}
