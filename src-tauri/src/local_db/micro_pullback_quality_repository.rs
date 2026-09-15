// Persistence for Micro Pullback's news-only LLM score.
//
// Mirrors `quality_repository` (Dislocation's), minus everything that belongs to
// Dislocation's three-thirds scoring (RVOL/attention, the deep pass, the dilution
// dossier): Micro Pullback has no model-driven blending — the score IS the News pass's
// `quality_score`, and there is exactly one pass, so there is no `pass1`/`pass2` split
// to keep side by side.
//
// `upsert`, never a wholesale replace — same reasoning as `quality_repository`: a
// second call for the same key (a manual re-run after auto already scored it, say)
// must not silently need special-casing to avoid clobbering a legitimate row.

use rusqlite::{params, Connection, Result};

/// Rows older than this are pruned; live and simulated rows count together.
const KEEP_DAYS: i64 = 30;

/// One persisted judgement.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct MicroPullbackQualityRow {
    pub symbol: String,
    pub day: String,
    pub session: String,
    pub simulated: bool,
    /// The model's `quality_score`, 0..100. This is the whole score — no blending.
    pub score: Option<f64>,
    pub verdict: Option<String>,
    pub freshness: Option<String>,
    /// JSON array.
    pub red_flags: Option<String>,
    pub one_liner: Option<String>,
    pub model: Option<String>,
    /// Market time of this verdict (the replay clock during a replay).
    pub scored_at: Option<String>,
}

/// Write (or update) one candidate's row.
pub fn upsert(
    conn: &Connection,
    symbol: &str,
    day: &str,
    session: &str,
    simulated: bool,
    answer: &crate::quality_llm::QualityAnswer,
) -> Result<()> {
    let now = crate::time::now();
    let red_flags = serde_json::to_string(&answer.red_flags).ok();

    conn.execute(
        "INSERT INTO micro_pullback_quality_scores (
            symbol, day, session, simulated,
            score, verdict, freshness, red_flags, one_liner, model, scored_at
         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
         ON CONFLICT(symbol, day, session, simulated) DO UPDATE SET
            score      = excluded.score,
            verdict    = excluded.verdict,
            freshness  = excluded.freshness,
            red_flags  = excluded.red_flags,
            one_liner  = excluded.one_liner,
            model      = excluded.model,
            scored_at  = excluded.scored_at",
        params![
            symbol,
            day,
            session,
            simulated as i64,
            answer.quality_score,
            answer.news_verdict,
            answer.news_freshness,
            red_flags,
            answer.one_liner,
            crate::quality_llm::Pass::News.as_str(),
            now.to_rfc3339(),
        ],
    )?;
    Ok(())
}

/// Every judgement for a trading day, best score first. Unscored rows sort last.
pub fn get_for_day(
    conn: &Connection,
    day: &str,
    simulated: bool,
) -> Result<Vec<MicroPullbackQualityRow>> {
    let mut stmt = conn.prepare(
        "SELECT symbol, day, session, simulated,
                score, verdict, freshness, red_flags, one_liner, model, scored_at
           FROM micro_pullback_quality_scores
          WHERE day = ?1 AND simulated = ?2
          ORDER BY score IS NULL, score DESC",
    )?;
    let rows = stmt
        .query_map(params![day, simulated as i64], |r| {
            Ok(MicroPullbackQualityRow {
                symbol: r.get(0)?,
                day: r.get(1)?,
                session: r.get(2)?,
                simulated: r.get::<_, i64>(3)? != 0,
                score: r.get(4)?,
                verdict: r.get(5)?,
                freshness: r.get(6)?,
                red_flags: r.get(7)?,
                one_liner: r.get(8)?,
                model: r.get(9)?,
                scored_at: r.get(10)?,
            })
        })?
        .collect::<Result<Vec<_>>>()?;
    Ok(rows)
}

/// Drop rows older than `KEEP_DAYS`.
pub fn prune(conn: &Connection) -> Result<usize> {
    let cutoff = (crate::time::now() - chrono::Duration::days(KEEP_DAYS))
        .date_naive()
        .format("%Y-%m-%d")
        .to_string();
    conn.execute(
        "DELETE FROM micro_pullback_quality_scores WHERE day < ?1",
        params![cutoff],
    )
}
