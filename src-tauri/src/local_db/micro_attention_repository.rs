use rusqlite::{params, Connection, Result};

use crate::types::MicroAttentionEntry;

pub fn insert(
    conn: &Connection,
    event_id: &str,
    row: &MicroAttentionEntry,
    tempo_depth: &[i64],
    outcome: &str,
) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO micro_attention_events
         (event_id,symbol,market_time,received_at,tempo_secs,tempo_depth,
          attention_score,direction_score,dominance_z,head_size,flow_rank,
          range_rank,relative_rank,tempo_rvol,baseline_windows,continuity,distribution_quality,evidence_ok,
          already_alerted,outcome)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20)",
        params![
            event_id,
            row.symbol,
            row.market_time.to_rfc3339(),
            row.received_at.to_rfc3339(),
            row.tempo_secs,
            serde_json::to_string(tempo_depth).unwrap_or_else(|_| "[]".into()),
            row.attention_score,
            row.direction_score,
            row.dominance_z,
            row.head_size,
            row.flow_rank,
            row.range_rank,
            row.relative_rank,
            row.tempo_rvol,
            row.baseline_windows,
            row.continuity,
            row.distribution_quality,
            row.evidence_ok as i64,
            row.already_alerted as i64,
            outcome,
        ],
    )?;
    Ok(())
}

pub fn insert_snapshot(conn: &mut Connection, rows: &[MicroAttentionEntry]) -> Result<()> {
    let tx = conn.transaction()?;
    {
        let mut stmt = tx.prepare(
            "INSERT OR REPLACE INTO micro_attention_snapshots
             (market_time,tempo_secs,rank,symbol,attention_score,direction_score,
              dominance_z,head_size,dominant,flow_rank,range_rank,relative_rank,
              tempo_rvol,baseline_windows,continuity,distribution_quality,evidence_ok,already_alerted,received_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)",
        )?;
        for r in rows {
            stmt.execute(params![
                r.market_time.to_rfc3339(),
                r.tempo_secs,
                r.rank,
                r.symbol,
                r.attention_score,
                r.direction_score,
                r.dominance_z,
                r.head_size,
                r.dominant as i64,
                r.flow_rank,
                r.range_rank,
                r.relative_rank,
                r.tempo_rvol,
                r.baseline_windows,
                r.continuity,
                r.distribution_quality,
                r.evidence_ok as i64,
                r.already_alerted as i64,
                r.received_at.to_rfc3339(),
            ])?;
        }
    }
    tx.commit()?;
    Ok(())
}

pub fn prune_snapshots(conn: &Connection, before_market_time: &str) -> Result<usize> {
    conn.execute(
        "DELETE FROM micro_attention_snapshots WHERE market_time < ?1",
        params![before_market_time],
    )
}

pub fn update_outcome(conn: &Connection, event_id: &str, outcome: &str) -> Result<usize> {
    conn.execute(
        "UPDATE micro_attention_events SET outcome=?2 WHERE event_id=?1",
        params![event_id, outcome],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn row() -> MicroAttentionEntry {
        let t = Utc.timestamp_opt(1_700_000_000, 0).single().unwrap();
        MicroAttentionEntry {
            symbol: "TEST".into(),
            tempo_secs: 10,
            rank: 1,
            attention_score: 92.0,
            direction_score: 81.0,
            flow_rank: 1.0,
            range_rank: 0.9,
            relative_rank: None,
            tempo_rvol: Some(7.5),
            baseline_windows: 8,
            continuity: 0.8,
            distribution_quality: 0.7,
            evidence_ok: true,
            dominance_z: 4.0,
            head_size: 1,
            dominant: true,
            already_alerted: false,
            market_time: t,
            received_at: t + chrono::Duration::minutes(15),
        }
    }

    #[test]
    fn event_and_snapshot_survive_the_real_schema() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::local_db::schema::migrate(&conn).unwrap();
        let r = row();
        insert(&conn, "event-1", &r, &[10, 20], "sent_to_a_plus").unwrap();
        insert_snapshot(&mut conn, &[r]).unwrap();
        let events: i64 = conn
            .query_row("SELECT COUNT(*) FROM micro_attention_events", [], |q| {
                q.get(0)
            })
            .unwrap();
        let snapshots: i64 = conn
            .query_row("SELECT COUNT(*) FROM micro_attention_snapshots", [], |q| {
                q.get(0)
            })
            .unwrap();
        assert_eq!((events, snapshots), (1, 1));
    }
}
