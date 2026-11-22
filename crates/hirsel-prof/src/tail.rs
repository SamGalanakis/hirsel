use rusqlite::Connection;
use serde_json::{Value, json};

use crate::profile::{Result, Unsigned};
use crate::query::unsigned;

pub(crate) fn tail(db: &Connection, limit: usize) -> Result<Value> {
    let (Unsigned(completed), Unsigned(censored), p50, p95, p99) = db.query_row(
        "WITH duration AS (
             SELECT end_ns - start_ns AS latency FROM task
             WHERE start_ns IS NOT NULL AND end_ns IS NOT NULL
         ),
         ordered AS (
             SELECT latency, row_number() OVER (ORDER BY latency) AS rank,
                    count(*) OVER () AS n FROM duration
         )
         SELECT count(*), (SELECT count(*) FROM task) - count(*),
                max(CASE WHEN rank = (50*n + 99)/100 THEN latency END),
                max(CASE WHEN rank = (95*n + 99)/100 THEN latency END),
                max(CASE WHEN rank = (99*n + 99)/100 THEN latency END)
         FROM ordered",
        [],
        |row| {
            Ok((
                row.get::<_, Unsigned>(0)?,
                row.get::<_, Unsigned>(1)?,
                row.get::<_, Option<Unsigned>>(2)?,
                row.get::<_, Option<Unsigned>>(3)?,
                row.get::<_, Option<Unsigned>>(4)?,
            ))
        },
    )?;
    let (Unsigned(edges_total), Unsigned(boundary_wakes)) = db.query_row(
        "SELECT count(*), count(*) FILTER (WHERE src_kind != 'task' OR src_id IS NULL)
         FROM causal_edge",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let mut query = db.prepare(
        "SELECT src_kind, src_id, dst_task_id, object_id, ts_ns
         FROM causal_edge ORDER BY ts_ns, id LIMIT ?1",
    )?;
    let edges = query
        .query_map([i64::try_from(limit)?], |row| {
            Ok(json!({
                "src_kind": row.get::<_, String>(0)?,
                "src_id": row.get::<_, Option<Unsigned>>(1)?.map(|Unsigned(value)| unsigned(value)),
                "dst_task_id": unsigned(row.get::<_, Unsigned>(2)?.0),
                "object_id": row.get::<_, Option<Unsigned>>(3)?.map(|id| unsigned(id.0)),
                "ts_ns": unsigned(row.get::<_, Unsigned>(4)?.0),
            }))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(json!({
        "completed": unsigned(completed),
        "censored": unsigned(censored),
        "p50_ns": p50.map(|Unsigned(value)| unsigned(value)),
        "p95_ns": p95.map(|Unsigned(value)| unsigned(value)),
        "p99_ns": p99.map(|Unsigned(value)| unsigned(value)),
        "causal_edges": edges,
        "causal_edges_total": unsigned(edges_total),
        "causal_edges_truncated": edges_total > edges.len() as u64,
        "causal_boundary_wakes": unsigned(boundary_wakes),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_rank_excludes_censored_tasks_and_edges_keep_their_source() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(include_str!("schema.sql")).unwrap();
        db.execute_batch(
            "INSERT INTO task(id,unit_id,runtime_id,start_ns,end_ns) VALUES
                (1,1,0,10,17), (2,1,1,14,15), (3,1,2,10,13), (4,1,3,21,26),
                (5,1,4,24,NULL), (6,1,5,NULL,14);
             INSERT INTO causal_edge(src_kind,src_id,dst_task_id,object_id,ts_ns) VALUES
                ('task',2,1,9007199254740993,15), ('timer',NULL,4,22,23);",
        )
        .unwrap();
        let report = tail(&db, 1).unwrap();
        assert_eq!(
            report,
            json!({
                "completed": 4, "censored": 2,
                "p50_ns": 3, "p95_ns": 7, "p99_ns": 7,
                "causal_edges": [{
                    "src_kind": "task", "src_id": 2, "dst_task_id": 1,
                    "object_id": "9007199254740993", "ts_ns": 15
                }],
                "causal_edges_total": 2, "causal_edges_truncated": true,
                "causal_boundary_wakes": 1,
            })
        );
    }
}
