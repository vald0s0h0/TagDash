// Persistence for Micro Pullback's A+ quality scores.
//
// Mirrors `panic_llm_repository`, with one deliberate difference in the key: the
// LOCK is per (day, session), so the same ticker legitimately qualifies twice in one
// day — once in the premarket, once at the open — and those are two different
// judgements about two different moves. Keying on the day alone would have the second
// silently overwrite the first.
//
// BOTH PASSES WRITE THE SAME ROW. `pass1_score` and `pass2_score` are kept side by
// side rather than collapsed into one number, because the question worth answering
// after a few weeks is whether the deep pass ever changed the outcome — and that is
// only answerable if the fast pass's opinion survived.
//
// `upsert`, never `replace_for_day`: the same mistake `panic_llm_repository` documents
// at length — a wholesale replace made a second run delete the first run's rows.

use rusqlite::{params, Connection, Result};

/// Rows older than this are pruned; live and simulated rows count together.
const KEEP_DAYS: i64 = 30;

/// One persisted judgement. Serialised straight to the front end: the A+ overlay
/// draws the model's own words, and re-deriving them anywhere else would let the
/// screen and the audit trail drift apart.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct QualityRow {
    pub symbol: String,
    pub day: String,
    pub session: String,
    pub simulated: bool,
    pub pass1_score: Option<f64>,
    pub pass2_score: Option<f64>,
    pub final_score: Option<f64>,
    pub threshold: Option<f64>,
    pub passed: bool,
    /// The three thirds, as one auditable line. Kept alongside the numeric columns
    /// so a row still reads correctly in a SQL client.
    pub adjustments: Option<String>,
    /// The RVOL third, 0..100, as measured by the tick that nominated the candidate.
    pub rvol_score: Option<f64>,
    /// The attention third, 0..100, same tick.
    pub attention_score: Option<f64>,
    /// The news third, 0..100: the model's note decayed by the headline's age.
    pub news_score: Option<f64>,
    pub confidence: Option<f64>,
    pub sustain_prob: Option<f64>,
    pub news_verdict: Option<String>,
    pub news_freshness: Option<String>,
    pub catalyst_type: Option<String>,
    pub catalyst_label: Option<String>,
    pub dilution_risk: Option<String>,
    pub dilution_read: Option<String>,
    pub financial_read: Option<String>,
    /// JSON array.
    pub red_flags: Option<String>,
    /// JSON array.
    pub key_drivers: Option<String>,
    pub one_liner: Option<String>,
    pub entry_note: Option<String>,
    /// JSON array of tempo seconds.
    pub tempo_depth: Option<String>,
    pub rejection: Option<String>,
    pub rubric_version: Option<String>,
    pub model: Option<String>,
    /// Market time of this verdict (the replay clock during a replay). The overlay
    /// compares it with the newest headline to show "scored before that news".
    pub scored_at: Option<String>,
}

/// Write (or update) one candidate's row from a verdict.
///
/// Which score column is written depends on the pass, so a deep re-score never
/// erases what the news pass thought — `COALESCE` keeps whichever half this call
/// isn't carrying.
pub fn upsert(
    conn: &Connection,
    symbol: &str,
    cand: &crate::dislocation::quality::Candidate,
    v: &crate::dislocation::quality::Verdict,
) -> Result<()> {
    use crate::quality_llm::Pass;

    let now = crate::time::now();
    let day = crate::time::et_date(now);
    let session = crate::dislocation::quality::session_label(now);
    let simulated = crate::replay::clock::is_active() as i64;
    let a = &v.answer;
    let (p1, p2) = match v.pass {
        Pass::News => (Some(a.quality_score), None),
        Pass::Deep => (None, Some(a.quality_score)),
    };
    let json = |v: &Vec<String>| serde_json::to_string(v).ok();

    conn.execute(
        "INSERT INTO micro_quality_scores (
            symbol, day, session, simulated,
            pass1_score, pass2_score, final_score, threshold, passed, adjustments,
            confidence, sustain_prob, news_verdict, news_freshness,
            catalyst_type, catalyst_label, dilution_risk, dilution_read, financial_read,
            red_flags, key_drivers, one_liner, entry_note, tempo_depth,
            rejection, rubric_version, model, scored_at,
            rvol_score, attention_score, news_score
         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,
                   ?20,?21,?22,?23,?24,?25,?26,?27,?28,?29,?30,?31)
         ON CONFLICT(symbol, day, session, simulated) DO UPDATE SET
            -- COALESCE(new, old): a pass only overwrites the half it produced.
            pass1_score     = COALESCE(excluded.pass1_score, pass1_score),
            pass2_score     = COALESCE(excluded.pass2_score, pass2_score),
            final_score     = excluded.final_score,
            threshold       = excluded.threshold,
            passed          = excluded.passed,
            adjustments     = excluded.adjustments,
            confidence      = COALESCE(excluded.confidence, confidence),
            sustain_prob    = COALESCE(excluded.sustain_prob, sustain_prob),
            news_verdict    = COALESCE(excluded.news_verdict, news_verdict),
            news_freshness  = COALESCE(excluded.news_freshness, news_freshness),
            catalyst_type   = COALESCE(excluded.catalyst_type, catalyst_type),
            catalyst_label  = COALESCE(excluded.catalyst_label, catalyst_label),
            dilution_risk   = COALESCE(excluded.dilution_risk, dilution_risk),
            dilution_read   = COALESCE(excluded.dilution_read, dilution_read),
            financial_read  = COALESCE(excluded.financial_read, financial_read),
            red_flags       = COALESCE(excluded.red_flags, red_flags),
            key_drivers     = COALESCE(excluded.key_drivers, key_drivers),
            one_liner       = COALESCE(excluded.one_liner, one_liner),
            entry_note      = COALESCE(excluded.entry_note, entry_note),
            tempo_depth     = excluded.tempo_depth,
            rejection       = excluded.rejection,
            rubric_version  = excluded.rubric_version,
            model           = excluded.model,
            scored_at       = excluded.scored_at,
            rvol_score      = excluded.rvol_score,
            attention_score = excluded.attention_score,
            news_score      = excluded.news_score",
        params![
            symbol,
            day,
            session,
            simulated,
            p1,
            p2,
            v.final_score,
            v.threshold,
            v.passed() as i64,
            v.scores.describe(),
            a.confidence,
            a.sustain_prob,
            a.news_verdict,
            a.news_freshness,
            a.catalyst_type,
            a.catalyst_label,
            a.dilution_risk,
            a.dilution_read,
            a.financial_read,
            json(&a.red_flags),
            json(&a.key_drivers),
            a.one_liner,
            a.entry_note,
            serde_json::to_string(&cand.tempo_depth).ok(),
            v.rejection.as_ref().map(|r| r.reason()),
            crate::quality_llm::rubric::version(),
            v.pass.as_str(),
            now.to_rfc3339(),
            v.scores.rvol,
            v.scores.attention,
            v.scores.news,
        ],
    )?;
    Ok(())
}

/// Every judgement for a trading day, best score first. Unscored rows sort last
/// rather than as a zero — the same ordering rule `panic_llm_repository` uses.
pub fn get_for_day(conn: &Connection, day: &str, simulated: bool) -> Result<Vec<QualityRow>> {
    let mut stmt = conn.prepare(
        "SELECT symbol, day, session, simulated,
                pass1_score, pass2_score, final_score, threshold, passed, adjustments,
                confidence, sustain_prob, news_verdict, news_freshness,
                catalyst_type, catalyst_label, dilution_risk, dilution_read, financial_read,
                red_flags, key_drivers, one_liner, entry_note, tempo_depth,
                rejection, rubric_version, model, scored_at,
                rvol_score, attention_score, news_score
           FROM micro_quality_scores
          WHERE day = ?1 AND simulated = ?2
          ORDER BY final_score IS NULL, final_score DESC",
    )?;
    let rows = stmt
        .query_map(params![day, simulated as i64], |r| {
            Ok(QualityRow {
                symbol: r.get(0)?,
                day: r.get(1)?,
                session: r.get(2)?,
                simulated: r.get::<_, i64>(3)? != 0,
                pass1_score: r.get(4)?,
                pass2_score: r.get(5)?,
                final_score: r.get(6)?,
                threshold: r.get(7)?,
                passed: r.get::<_, i64>(8)? != 0,
                adjustments: r.get(9)?,
                confidence: r.get(10)?,
                sustain_prob: r.get(11)?,
                news_verdict: r.get(12)?,
                news_freshness: r.get(13)?,
                catalyst_type: r.get(14)?,
                catalyst_label: r.get(15)?,
                dilution_risk: r.get(16)?,
                dilution_read: r.get(17)?,
                financial_read: r.get(18)?,
                red_flags: r.get(19)?,
                key_drivers: r.get(20)?,
                one_liner: r.get(21)?,
                entry_note: r.get(22)?,
                tempo_depth: r.get(23)?,
                rejection: r.get(24)?,
                rubric_version: r.get(25)?,
                model: r.get(26)?,
                scored_at: r.get(27)?,
                rvol_score: r.get(28)?,
                attention_score: r.get(29)?,
                news_score: r.get(30)?,
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
        "DELETE FROM micro_quality_scores WHERE day < ?1",
        params![cutoff],
    )
}
