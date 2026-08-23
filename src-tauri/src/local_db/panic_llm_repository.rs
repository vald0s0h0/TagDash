// Persistence for the Panic Score V1 LLM analysis (`panic_llm_scores`).
//
// Scoped by (day, simulated) like `scoring_repository` and `screener_repository`:
// the contextual score participates in the ranking of the pre-open watchlist, so a run made
// during a Market Replay must be invisible in live mode and vice versa. Anything
// less would let a backtest quietly reorder a real trading morning.
//
// Written only by `crate::panic_llm::run` (button-triggered — never automatic);
// read by the scanner to attach + sort the screener rows, and by the daily-pane
// report overlay.

use rusqlite::{params, Connection, Result};
use serde::{Deserialize, Serialize};

/// How many distinct days of analysis to keep on disk. Matches
/// `scoring_repository::KEEP_DAYS` so the two tables age out together.
const KEEP_DAYS: usize = 30;

/// One ticker's LLM reading. Every analytical field is optional: a model answer that
/// omitted a field must degrade to "—" in the UI rather than fail the whole run.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PanicLlmScore {
    pub symbol: String,
    pub day: String,
    pub simulated: bool,
    /// 0..100, post-calibration — raw contextual reversibility.
    pub panic_score: Option<f64>,
    /// 0..100 as first returned for this ticker alone, before calibration.
    pub panic_score_raw: Option<f64>,
    /// 0..100: how much the model trusts its own read given the data it received.
    pub confidence_score: Option<f64>,
    /// 0..100: how materially the catalyst changes the company's value.
    pub catalyst_strength: Option<f64>,
    /// Short human label for the catalyst ("FDA approval", "aucun catalyseur identifié").
    pub catalyst_label: Option<String>,
    /// Coarse family: fda | earnings | ma | offering | contract | none | …
    pub catalyst_type: Option<String>,
    /// faible | moyenne | forte — probability of a return to equilibrium.
    pub reversibility: Option<String>,
    /// The one-sentence explanation of why the price moves and how reversible it looks.
    pub one_liner: Option<String>,
    /// Short evidence chips, decoded from the stored JSON array.
    pub key_drivers: Vec<String>,
    pub forced_flow: Option<String>,
    pub euphoria: Option<String>,
    pub normalization_prob: Option<f64>,
    /// JSON blob: feed used, missing fields, look-ahead fields.
    pub data_quality: Option<String>,
    pub web_used: bool,
    pub web_provider: Option<String>,
    /// Why calibration moved the score, when it did.
    pub adjustment_reason: Option<String>,
    pub rubric_version: Option<String>,
    pub model: Option<String>,
    /// Confidence-aware reversibility score used by the final ranking (0..100).
    pub ranking_score: Option<f64>,
    /// `long` for a downward panic, `short` for an upward euphoria.
    pub trade_direction: Option<String>,
    /// extending | exhausting | reversing | unknown. Informational, never an order.
    pub timing_state: Option<String>,
    pub fundamental_veto: bool,
    pub veto_reason: Option<String>,
    /// eligible | review | veto.
    pub eligibility: Option<String>,
    /// positive | negative | neutral | unknown.
    pub catalyst_direction: Option<String>,
    pub created_at: Option<String>,
}

/// Replace the whole analysis for one (day, mode). Other days and the other mode are
/// untouched — same contract as `scoring_repository::replace_for_day`.
pub fn replace_for_day(
    conn: &Connection,
    day: &str,
    simulated: bool,
    rows: &[PanicLlmScore],
) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM panic_llm_scores WHERE day = ?1 AND simulated = ?2",
        params![day, simulated as i64],
    )?;
    write_rows(&tx, day, simulated, rows)?;
    tx.commit()
}

/// Merge rows into one (day, mode), leaving every symbol this run did not touch
/// exactly as it was.
///
/// This is what the analysis run uses, and the distinction from `replace_for_day`
/// matters: the button scores the tickers VISIBLE at the moment it is pressed, and
/// the watchlist keeps changing afterwards — the 09:25 build lands, a premarket
/// refresh replaces rows, the user dismisses a name. With a wholesale replace, a
/// second press aimed at the newly-arrived tickers DELETED the first press's scores,
/// so topping up a partial analysis merely moved the hole. Upserting per symbol is
/// what lets two presses add up to a fully analysed list.
pub fn upsert_many(
    conn: &Connection,
    day: &str,
    simulated: bool,
    rows: &[PanicLlmScore],
) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    write_rows(&tx, day, simulated, rows)?;
    tx.commit()
}

/// The shared write. `INSERT OR REPLACE` so re-scoring a ticker overwrites its row
/// (the primary key is `(symbol, day, simulated)`) instead of failing the run.
fn write_rows(conn: &Connection, day: &str, simulated: bool, rows: &[PanicLlmScore]) -> Result<()> {
    {
        let mut stmt = conn.prepare(
            "INSERT OR REPLACE INTO panic_llm_scores
                 (symbol, day, simulated, panic_score, panic_score_raw, confidence_score,
                  catalyst_strength, catalyst_label, catalyst_type, reversibility,
                  one_liner, key_drivers, forced_flow, euphoria, normalization_prob,
                  data_quality, web_used, web_provider, adjustment_reason,
                  rubric_version, model, ranking_score, trade_direction, timing_state,
                  fundamental_veto, veto_reason, eligibility, catalyst_direction)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27,?28)",
        )?;
        for r in rows {
            let drivers = serde_json::to_string(&r.key_drivers).unwrap_or_else(|_| "[]".into());
            stmt.execute(params![
                r.symbol,
                day,
                simulated as i64,
                r.panic_score,
                r.panic_score_raw,
                r.confidence_score,
                r.catalyst_strength,
                r.catalyst_label,
                r.catalyst_type,
                r.reversibility,
                r.one_liner,
                drivers,
                r.forced_flow,
                r.euphoria,
                r.normalization_prob,
                r.data_quality,
                r.web_used as i64,
                r.web_provider,
                r.adjustment_reason,
                r.rubric_version,
                r.model,
                r.ranking_score,
                r.trade_direction,
                r.timing_state,
                r.fundamental_veto as i64,
                r.veto_reason,
                r.eligibility,
                r.catalyst_direction,
            ])?;
        }
    }
    Ok(())
}

const COLUMNS: &str = "symbol, day, simulated, panic_score, panic_score_raw, confidence_score,
     catalyst_strength, catalyst_label, catalyst_type, reversibility, one_liner,
     key_drivers, forced_flow, euphoria, normalization_prob, data_quality,
     web_used, web_provider, adjustment_reason, rubric_version, model,
     ranking_score, trade_direction, timing_state, fundamental_veto, veto_reason,
     eligibility, catalyst_direction, created_at";

fn map_row(r: &rusqlite::Row) -> Result<PanicLlmScore> {
    let drivers_json: Option<String> = r.get(11)?;
    Ok(PanicLlmScore {
        symbol: r.get(0)?,
        day: r.get(1)?,
        simulated: r.get::<_, i64>(2)? != 0,
        panic_score: r.get(3)?,
        panic_score_raw: r.get(4)?,
        confidence_score: r.get(5)?,
        catalyst_strength: r.get(6)?,
        catalyst_label: r.get(7)?,
        catalyst_type: r.get(8)?,
        reversibility: r.get(9)?,
        one_liner: r.get(10)?,
        // A malformed blob degrades to no chips rather than failing the read: the
        // score and sentence are what matter, the chips are decoration.
        key_drivers: drivers_json
            .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
            .unwrap_or_default(),
        forced_flow: r.get(12)?,
        euphoria: r.get(13)?,
        normalization_prob: r.get(14)?,
        data_quality: r.get(15)?,
        web_used: r.get::<_, i64>(16)? != 0,
        web_provider: r.get(17)?,
        adjustment_reason: r.get(18)?,
        rubric_version: r.get(19)?,
        model: r.get(20)?,
        ranking_score: r.get(21)?,
        trade_direction: r.get(22)?,
        timing_state: r.get(23)?,
        fundamental_veto: r.get::<_, i64>(24)? != 0,
        veto_reason: r.get(25)?,
        eligibility: r.get(26)?,
        catalyst_direction: r.get(27)?,
        created_at: r.get(28)?,
    })
}

/// Every row for one (day, mode), **panic score descending**, with unscored (N/A)
/// tickers last. That is the ranking the user asked for and the one the screener
/// renders, so it is the one this read returns: a missing score means "could not be
/// established", never zero, so it sorts to the bottom instead of competing with a
/// genuine 0. The confidence-aware `ranking_score` only breaks ties.
pub fn get_for_day(conn: &Connection, day: &str, simulated: bool) -> Result<Vec<PanicLlmScore>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM panic_llm_scores
         WHERE day = ?1 AND simulated = ?2
         ORDER BY panic_score IS NULL, panic_score DESC,
                  ranking_score IS NULL, ranking_score DESC",
    ))?;
    let rows = stmt.query_map(params![day, simulated as i64], |r| map_row(r))?;
    rows.collect()
}

/// The `limit` most recent distinct days with at least one analysed row, strictly
/// before `day` (same mode), most recent first. Used to find "the previous 1-2
/// sessions this ticker was scored on" without assuming yesterday = today−1
/// calendar day (which breaks over weekends/holidays — a Monday's "veille" is
/// Friday, not Sunday).
pub fn distinct_days_before(
    conn: &Connection,
    day: &str,
    simulated: bool,
    limit: u32,
) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT day FROM panic_llm_scores
         WHERE day < ?1 AND simulated = ?2
         ORDER BY day DESC LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![day, simulated as i64, limit], |r| {
        r.get::<_, String>(0)
    })?;
    rows.collect()
}

/// One ticker's reading for a (day, mode); None when it was not analysed.
pub fn get_one(
    conn: &Connection,
    day: &str,
    simulated: bool,
    symbol: &str,
) -> Result<Option<PanicLlmScore>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM panic_llm_scores
         WHERE day = ?1 AND simulated = ?2 AND symbol = ?3",
    ))?;
    let mut rows = stmt.query_map(params![day, simulated as i64, symbol], |r| map_row(r))?;
    rows.next().transpose()
}

/// Keep the most recent `KEEP_DAYS` days. Live and replay days are counted together
/// (a day is a day), so a long backtest session cannot evict the live history faster
/// than the day budget allows.
pub fn prune(conn: &Connection) -> Result<usize> {
    conn.execute(
        "DELETE FROM panic_llm_scores WHERE day NOT IN (
             SELECT day FROM panic_llm_scores
             GROUP BY day ORDER BY MAX(created_at) DESC LIMIT ?1
         )",
        params![KEEP_DAYS as i64],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::local_db::schema::migrate(&conn).unwrap();
        conn
    }

    fn row(symbol: &str, score: f64) -> PanicLlmScore {
        PanicLlmScore {
            symbol: symbol.into(),
            panic_score: Some(score),
            panic_score_raw: Some(score - 3.0),
            confidence_score: Some(70.0),
            one_liner: Some("Mouvement sans catalyseur identifié.".into()),
            key_drivers: vec!["float 4M".into(), "SI 25%".into()],
            rubric_version: Some("v1".into()),
            ranking_score: Some(score),
            trade_direction: Some("short".into()),
            timing_state: Some("extending".into()),
            eligibility: Some("eligible".into()),
            catalyst_direction: Some("unknown".into()),
            ..Default::default()
        }
    }

    #[test]
    fn rows_round_trip_including_the_driver_chips() {
        let conn = db();
        replace_for_day(&conn, "2026-08-14", false, &[row("MTEN", 91.0)]).unwrap();
        let got = get_one(&conn, "2026-08-14", false, "MTEN")
            .unwrap()
            .unwrap();
        assert_eq!(got.panic_score, Some(91.0));
        assert_eq!(got.panic_score_raw, Some(88.0));
        assert_eq!(got.key_drivers, vec!["float 4M", "SI 25%"]);
        assert_eq!(got.ranking_score, Some(91.0));
        assert_eq!(got.trade_direction.as_deref(), Some("short"));
        assert_eq!(got.eligibility.as_deref(), Some("eligible"));
        assert!(!got.web_used);
        assert!(
            got.created_at.is_some(),
            "the default stamp must be written"
        );
    }

    /// The isolation that stops a backtest reordering a live trading morning.
    #[test]
    fn live_and_replay_analyses_never_see_each_other() {
        let conn = db();
        replace_for_day(&conn, "2026-08-14", false, &[row("LIVE", 80.0)]).unwrap();
        replace_for_day(&conn, "2026-08-14", true, &[row("SIM", 20.0)]).unwrap();

        let live = get_for_day(&conn, "2026-08-14", false).unwrap();
        let sim = get_for_day(&conn, "2026-08-14", true).unwrap();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].symbol, "LIVE");
        assert_eq!(sim.len(), 1);
        assert_eq!(sim[0].symbol, "SIM");

        // Re-running the replay analysis must not touch the live rows.
        replace_for_day(&conn, "2026-08-14", true, &[]).unwrap();
        assert!(get_for_day(&conn, "2026-08-14", true).unwrap().is_empty());
        assert_eq!(get_for_day(&conn, "2026-08-14", false).unwrap().len(), 1);
    }

    #[test]
    fn replace_only_clears_its_own_day() {
        let conn = db();
        replace_for_day(&conn, "2026-08-13", false, &[row("AAA", 50.0)]).unwrap();
        replace_for_day(&conn, "2026-08-14", false, &[row("BBB", 60.0)]).unwrap();
        replace_for_day(&conn, "2026-08-14", false, &[]).unwrap();
        assert!(get_for_day(&conn, "2026-08-14", false).unwrap().is_empty());
        assert_eq!(get_for_day(&conn, "2026-08-13", false).unwrap().len(), 1);
    }

    /// Highest panic first, and an UNSCORED ticker sorts last rather than as a zero —
    /// this is the order the screener renders, so it has to be the order here.
    #[test]
    fn unscored_rows_sort_after_scored_ones() {
        let conn = db();
        let mut missing = row("NOSCORE", 0.0);
        missing.panic_score = None;
        missing.ranking_score = None;
        replace_for_day(
            &conn,
            "2026-08-14",
            false,
            &[row("LOW", 30.0), missing, row("HIGH", 90.0)],
        )
        .unwrap();
        let order: Vec<String> = get_for_day(&conn, "2026-08-14", false)
            .unwrap()
            .into_iter()
            .map(|r| r.symbol)
            .collect();
        assert_eq!(order, vec!["HIGH", "LOW", "NOSCORE"]);
    }

    /// The top-up contract. The button scores what is VISIBLE when pressed, and the
    /// watchlist keeps growing after that, so a second press must ADD to the day's
    /// analysis. Under the old wholesale replace it deleted the first press's scores,
    /// which merely moved the hole in the ranking.
    #[test]
    fn a_second_run_tops_up_instead_of_erasing_the_first() {
        let conn = db();
        upsert_many(&conn, "2026-08-14", false, &[row("FIRST", 70.0)]).unwrap();
        upsert_many(&conn, "2026-08-14", false, &[row("SECOND", 90.0)]).unwrap();

        let order: Vec<String> = get_for_day(&conn, "2026-08-14", false)
            .unwrap()
            .into_iter()
            .map(|r| r.symbol)
            .collect();
        assert_eq!(order, vec!["SECOND", "FIRST"]);

        // Re-scoring a ticker overwrites its row in place — no duplicate, no failure.
        upsert_many(&conn, "2026-08-14", false, &[row("FIRST", 95.0)]).unwrap();
        let rows = get_for_day(&conn, "2026-08-14", false).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].symbol, "FIRST");
        assert_eq!(rows[0].panic_score, Some(95.0));
    }

    /// An upsert into one mode never leaks into the other.
    #[test]
    fn a_top_up_stays_inside_its_own_mode() {
        let conn = db();
        upsert_many(&conn, "2026-08-14", false, &[row("LIVE", 80.0)]).unwrap();
        upsert_many(&conn, "2026-08-14", true, &[row("LIVE", 10.0)]).unwrap();
        assert_eq!(
            get_one(&conn, "2026-08-14", false, "LIVE")
                .unwrap()
                .unwrap()
                .panic_score,
            Some(80.0),
        );
        assert_eq!(
            get_one(&conn, "2026-08-14", true, "LIVE")
                .unwrap()
                .unwrap()
                .panic_score,
            Some(10.0),
        );
    }

    #[test]
    fn a_malformed_driver_blob_degrades_to_no_chips() {
        let conn = db();
        replace_for_day(&conn, "2026-08-14", false, &[row("AAA", 40.0)]).unwrap();
        conn.execute(
            "UPDATE panic_llm_scores SET key_drivers = 'not json' WHERE symbol='AAA'",
            [],
        )
        .unwrap();
        let got = get_one(&conn, "2026-08-14", false, "AAA").unwrap().unwrap();
        assert!(got.key_drivers.is_empty());
        assert_eq!(got.panic_score, Some(40.0), "the score still reads back");
    }
}
