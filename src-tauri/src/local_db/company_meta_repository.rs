use rusqlite::{params, Connection, Result};
use serde::{Deserialize, Serialize};

/// Company metadata row (country of origin + SIC industry) from sec-api.io.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompanyMeta {
    pub symbol: String,
    pub country: Option<String>,
    pub sic: Option<String>,
    pub industry: Option<String>,
    pub sector: Option<String>,
    pub updated_at: String,
}

pub fn upsert(conn: &Connection, m: &CompanyMeta) -> Result<()> {
    conn.execute(
        "INSERT INTO company_meta (symbol, country, sic, industry, sector, updated_at)
         VALUES (?1,?2,?3,?4,?5,?6)
         ON CONFLICT(symbol) DO UPDATE SET
             country=excluded.country, sic=excluded.sic,
             industry=excluded.industry, sector=excluded.sector,
             updated_at=excluded.updated_at",
        params![m.symbol, m.country, m.sic, m.industry, m.sector, m.updated_at],
    )?;
    Ok(())
}

pub fn get_all(conn: &Connection) -> Result<Vec<CompanyMeta>> {
    let mut stmt = conn.prepare(
        "SELECT symbol, country, sic, industry, sector, updated_at FROM company_meta",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(CompanyMeta {
            symbol: row.get(0)?,
            country: row.get(1)?,
            sic: row.get(2)?,
            industry: row.get(3)?,
            sector: row.get(4)?,
            updated_at: row.get(5)?,
        })
    })?;
    rows.collect()
}

/// One company's metadata by symbol (used by the alert enrichment pipeline).
pub fn get_by_symbol(conn: &Connection, symbol: &str) -> Result<Option<CompanyMeta>> {
    let mut stmt = conn.prepare(
        "SELECT symbol, country, sic, industry, sector, updated_at
         FROM company_meta WHERE symbol=?1",
    )?;
    let mut rows = stmt.query_map(params![symbol], |row| {
        Ok(CompanyMeta {
            symbol: row.get(0)?,
            country: row.get(1)?,
            sic: row.get(2)?,
            industry: row.get(3)?,
            sector: row.get(4)?,
            updated_at: row.get(5)?,
        })
    })?;
    rows.next().transpose()
}

pub fn count(conn: &Connection) -> Result<i64> {
    conn.query_row("SELECT COUNT(*) FROM company_meta", [], |r| r.get(0))
}

/// Most recent `updated_at` across company_meta — used to throttle the sec-api
/// full refresh to once per calendar day.
pub fn last_date(conn: &Connection) -> Option<String> {
    conn.query_row(
        "SELECT MAX(updated_at) FROM company_meta",
        [],
        |r| r.get::<_, Option<String>>(0),
    )
    .ok()
    .flatten()
}
