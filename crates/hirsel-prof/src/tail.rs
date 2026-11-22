use rusqlite::Connection;
use serde_json::{Value, json};

use crate::profile::{Result, Unsigned};
use crate::query::unsigned;

pub(crate) fn tail(db: &Connection, limit: usize) -> Result<Value> {
    let (Unsigned(completed), Unsigned(censored), p50, p95, p99) = db.query_row(
        "WITH duration AS (
             SELECT u64_elapsed(end_ns, start_ns) AS latency FROM task
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
        crate::profile::register_elapsed(&db).unwrap();
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

    #[test]
    fn task_wait_chain_round_trips_exact_timestamps() {
        use std::time::Duration;

        use crate::profile::{Profile, ProfileWriter, RunMeta, Timestamp};
        use crate::query::{Query, query};

        // Exercise ordinary SQLite integers above 2^53, the decimal width
        // transition, and adjacent unsigned timestamps near u64::MAX.
        for base in [1 << 53, 9_999_999_999_999_999_980, u64::MAX - 100] {
            let at = |offset| Timestamp(base + offset);
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("tasks.hprof");
            let writer = ProfileWriter::create(&path).unwrap();
            let db = writer.connection();
            db.execute_batch(
                "INSERT INTO run_meta VALUES ('runtime_trace_complete',1), ('runtime_events_lost',0);
                 INSERT INTO task(id,unit_id,runtime_id) VALUES (1,1,1), (2,1,2);",
            )
            .unwrap();
            db.execute(
                "UPDATE task SET start_ns=?1,end_ns=?2 WHERE id=1",
                (at(1), at(81)),
            )
            .unwrap();
            db.execute(
                "UPDATE task SET start_ns=?1,end_ns=?2 WHERE id=2",
                (at(2), at(63)),
            )
            .unwrap();
            for (id, task, state, reason, object, start, end) in [
                (
                    1,
                    1,
                    "waiting",
                    Some("channel_recv"),
                    Some(42),
                    10,
                    Some(41),
                ),
                (2, 1, "ready", None, None, 41, Some(43)),
                (3, 1, "running", None, None, 43, Some(49)),
                (
                    4,
                    1,
                    "waiting",
                    Some("channel_recv"),
                    Some(43),
                    49,
                    Some(54),
                ),
                (5, 1, "ready", None, None, 54, Some(56)),
                (6, 1, "running", None, None, 56, Some(62)),
                (
                    7,
                    1,
                    "waiting",
                    Some("channel_recv"),
                    Some(44),
                    62,
                    Some(68),
                ),
                (8, 1, "ready", None, None, 68, Some(69)),
                (
                    9,
                    1,
                    "waiting",
                    Some("channel_recv"),
                    Some(45),
                    70,
                    Some(74),
                ),
                (10, 1, "ready", None, None, 74, Some(75)),
                (
                    11,
                    1,
                    "waiting",
                    Some("channel_recv"),
                    Some(46),
                    75,
                    Some(79),
                ),
                (12, 1, "ready", None, None, 79, Some(80)),
                (13, 1, "waiting", Some("channel_recv"), Some(47), 80, None),
                (14, 2, "provider", Some("timer"), Some(51), 15, Some(27)),
                (15, 2, "ready", None, None, 27, Some(30)),
                (16, 2, "running", None, None, 30, Some(45)),
                (17, 2, "running", None, None, 50, Some(55)),
                (18, 2, "waiting", Some("join"), Some(1), 55, Some(61)),
                (19, 2, "ready", None, None, 61, Some(63)),
            ] {
                db.execute(
                    "INSERT INTO task_slice(id,task_id,state,wait_reason,object_id,start_ns,end_ns)
                     VALUES (?1,?2,?3,?4,?5,?6,?7)",
                    (id, task, state, reason, object, at(start), end.map(at)),
                )
                .unwrap();
            }
            for (kind, src, dst, object, time) in [
                ("timer", None, 2, 51, 26),
                ("task", Some(2), 1, 42, 40),
                ("task", Some(2), 1, 43, 53),
                ("task", Some(1), 2, 1, 60),
                ("task", Some(2), 1, 44, 62),
                ("task", Some(2), 1, 99, 72),
                ("task", Some(2), 1, 46, 76),
                ("cancel", None, 1, 46, 77),
            ] {
                db.execute(
                    "INSERT INTO causal_edge(src_kind,src_id,dst_task_id,object_id,ts_ns)
                     VALUES (?1,?2,?3,?4,?5)",
                    (kind, src, dst, object, at(time)),
                )
                .unwrap();
            }
            db.execute(
                "UPDATE run_meta SET value=1 WHERE key='runtime_events_lost'",
                [],
            )
            .unwrap();
            let incomplete: String = db
                .query_row(
                    "SELECT boundary FROM task_wait_episode WHERE id=1",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(incomplete, "incomplete_trace");
            db.execute(
                "UPDATE run_meta SET value=0 WHERE key='runtime_events_lost'",
                [],
            )
            .unwrap();
            writer
                .finish(&RunMeta {
                    capture_mode: "replay",
                    target: "x86_64-unknown-linux-gnu",
                    cmdline: "fixture",
                    build_id: "",
                    image_digest: "",
                    symbol_tier: "offsets_only",
                    clock: "CLOCK_MONOTONIC",
                    primary_event: "cycles",
                    primary_unit: "cycles",
                    freq_hz: 0,
                    fallback_reason: "",
                    start_ns: base,
                    duration_ns: 81,
                    samples_lost: 0,
                })
                .unwrap();

            let profile = Profile::open(&path).unwrap();
            let stamp: (Timestamp, String) = profile
                .connection()
                .query_row(
                    "SELECT start_ns,typeof(start_ns) FROM task_slice WHERE id=1",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert_eq!(stamp.0.0, base + 10);
            assert_eq!(
                stamp.1,
                if base <= i64::MAX as u64 {
                    "integer"
                } else {
                    "text"
                }
            );
            let sql = |statement| {
                query(
                    &path,
                    Query::Sql {
                        statement,
                        timeout: Duration::from_secs(5),
                    },
                    20,
                )
                .unwrap()
                .result
            };
            let report = sql(
                "SELECT start_ns,end_ns,depth,task_id,state,boundary,u64_elapsed(end_ns,start_ns)
                 FROM task_wait_critical_path WHERE root_id=1 ORDER BY start_ns",
            );
            let exact_text = |offset: u64| {
                let timestamp = base + offset;
                if timestamp <= i64::MAX as u64 {
                    timestamp.to_string()
                } else {
                    format!("{timestamp:020}")
                }
            };
            assert_eq!(
                report["rows"],
                json!([
                    [
                        exact_text(10),
                        exact_text(15),
                        0,
                        1,
                        "waiting",
                        "missing_predecessor",
                        5
                    ],
                    [
                        exact_text(15),
                        exact_text(26),
                        1,
                        2,
                        "provider",
                        "timer",
                        11
                    ],
                    [
                        exact_text(26),
                        exact_text(27),
                        0,
                        1,
                        "waiting",
                        "missing_predecessor",
                        1
                    ],
                    [exact_text(27), exact_text(30), 1, 2, "ready", null, 3],
                    [exact_text(30), exact_text(40), 1, 2, "running", null, 10],
                ])
            );
            assert_eq!(
                sql(
                    "SELECT id,boundary FROM task_wait_episode WHERE boundary IS NOT NULL ORDER BY id"
                )["rows"],
                json!([
                    [7, "timestamp_tie"],
                    [9, "missing_wake"],
                    [11, "ambiguous_wake"],
                    [13, "open_wait"],
                    [14, "timer"]
                ]),
            );
            assert_eq!(
                sql("SELECT root_id,depth,task_id,state,u64_elapsed(end_ns,start_ns)
                     FROM task_wait_critical_path WHERE root_id IN (4,18) ORDER BY root_id,start_ns")["rows"],
                json!([[4, 0, 1, "waiting", 1], [4, 1, 2, "running", 3],
                    [18, 1, 1, "ready", 1], [18, 1, 1, "running", 4]]),
            );
            let tail = query(&path, Query::Tail, 20).unwrap().result;
            assert_eq!(tail["p50_ns"], 61);
            assert_eq!(tail["p95_ns"], 80);
        }
    }
}
