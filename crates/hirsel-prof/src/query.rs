use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::{Duration, Instant};

use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, Row};
use serde_json::{Map, Value, json};

use crate::profile::{Profile, Result, Unsigned, register_totals};

const JS_EXACT_U64: u64 = 1 << 53;

pub(crate) fn unsigned(value: u64) -> Value {
    if value > JS_EXACT_U64 {
        Value::String(value.to_string())
    } else {
        Value::from(value)
    }
}

fn value(reference: ValueRef<'_>) -> Result<Value> {
    Ok(match reference {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(n) if n >= 0 => unsigned(n as u64),
        ValueRef::Integer(n) if n.unsigned_abs() > JS_EXACT_U64 => Value::String(n.to_string()),
        ValueRef::Integer(n) => Value::from(n),
        ValueRef::Real(n) if n.is_finite() => Value::from(n),
        ValueRef::Real(n) => Value::String(n.to_string()),
        ValueRef::Text(text) => Value::String(std::str::from_utf8(text)?.to_owned()),
        ValueRef::Blob(bytes) => {
            use std::fmt::Write as _;
            let mut text = "0x".to_owned();
            for byte in bytes {
                write!(text, "{byte:02x}")?;
            }
            Value::String(text)
        }
    })
}

pub struct QueryResult {
    pub quality: Value,
    pub result: Value,
}

#[derive(Clone, Copy)]
pub enum By {
    SelfCost,
    Inclusive,
}

#[derive(Clone, Copy)]
pub enum Group {
    DefPath,
    Instance,
}

pub enum Query<'a> {
    Summary,
    Schema,
    Top {
        by: By,
        event: Option<&'a str>,
        group: Group,
        filter: Option<&'a str>,
    },
    Annotate {
        target: &'a str,
    },
    Callers {
        target: &'a str,
        depth: u32,
    },
    Callees {
        target: &'a str,
        depth: u32,
    },
    Diff {
        head: &'a Path,
        by: By,
        group: Group,
        min_delta_pct: f64,
    },
    Sql {
        statement: &'a str,
        timeout: Duration,
    },
}

pub fn query(path: &Path, query: Query<'_>, limit: usize) -> Result<QueryResult> {
    if limit == 0 {
        return Err("--limit must be positive".into());
    }
    let mut profile = Profile::open(path)?;
    let db = profile.connection();
    let quality = quality(db)?;
    let primary = quality["primary_event"]
        .as_str()
        .ok_or("profile primary event has invalid type")?;
    let result = match query {
        Query::Summary => summary(db, primary, limit.min(5))?,
        Query::Schema => schema(db, limit)?,
        Query::Top {
            by,
            event,
            group,
            filter,
        } => {
            let event = event.unwrap_or(primary);
            validate_event(db, primary, event)?;
            let rows = totals(db, event, by, group, filter.unwrap_or("*"), Some(limit))?;
            json!({"event": event, "rows": rows.iter().map(Total::json).collect::<Vec<_>>()})
        }
        Query::Annotate { target } => annotate(db, target, limit)?,
        Query::Callers { target, depth } => related(db, target, depth, primary, true, limit)?,
        Query::Callees { target, depth } => related(db, target, depth, primary, false, limit)?,
        Query::Diff {
            head,
            by,
            group,
            min_delta_pct,
        } => diff(db, &quality, head, by, group, min_delta_pct, limit)?,
        Query::Sql { statement, timeout } => {
            sql(profile.connection_mut(), statement, timeout, limit)?
        }
    };
    Ok(QueryResult { quality, result })
}

fn quality(db: &Connection) -> Result<Value> {
    let mut rows = db.prepare("SELECT key,value FROM run_meta ORDER BY key")?;
    let mut rows = rows.query([])?;
    let mut header = Map::new();
    while let Some(row) = rows.next()? {
        header.insert(row.get(0)?, value(row.get_ref(1)?)?);
    }
    let count = |key: &str| -> Result<u64> {
        match header.get(key) {
            Some(Value::String(s)) => Ok(s.parse()?),
            Some(v) => v
                .as_u64()
                .ok_or_else(|| format!("invalid profile counter {key}").into()),
            None => Err(format!("profile is missing counter {key}").into()),
        }
    };
    let total = count("samples_total")?;
    let unresolved = count("unresolved_samples")?;
    let host = count("host_root_samples")?;
    let mut warnings = Vec::new();
    for (key, label) in [
        ("samples_lost", "lost_samples"),
        ("stacks_truncated", "truncated_stacks"),
        ("unwind_failed", "unwind_failures"),
        ("unresolved_samples", "unresolved_symbols"),
        ("host_root_samples", "host_stacks"),
    ] {
        if count(key)? != 0 {
            warnings.push(label);
        }
    }
    header.insert("unresolved_pct".into(), fraction(unresolved, total, 100.0));
    header.insert("host_stack_share".into(), fraction(host, total, 1.0));
    header.insert(
        "verdict".into(),
        json!(if total == 0 {
            "empty"
        } else if warnings.is_empty() {
            "healthy"
        } else {
            "degraded"
        }),
    );
    header.insert("warnings".into(), json!(warnings));
    Ok(Value::Object(header))
}

fn fraction(numerator: u64, denominator: u64, multiplier: f64) -> Value {
    if denominator == 0 {
        Value::Null
    } else {
        json!(numerator as f64 / denominator as f64 * multiplier)
    }
}

fn events(db: &Connection, primary: &str) -> Result<BTreeSet<String>> {
    let mut stmt = db.prepare("SELECT DISTINCT event FROM sample_counter ORDER BY event")?;
    let mut names = stmt
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<BTreeSet<_>>>()?;
    names.insert(primary.to_owned());
    Ok(names)
}

fn validate_event(db: &Connection, primary: &str, name: &str) -> Result<()> {
    if !events(db, primary)?.contains(name) {
        return Err(format!("event {name:?} is not in this profile").into());
    }
    Ok(())
}

fn summary(db: &Connection, primary: &str, limit: usize) -> Result<Value> {
    let mut hotspots = Map::new();
    for event in events(db, primary)? {
        let rows = totals(db, &event, By::SelfCost, Group::DefPath, "*", Some(limit))?;
        hotspots.insert(event, Value::Array(rows.iter().map(Total::json).collect()));
    }
    Ok(json!({"hotspots": hotspots}))
}

const TABLES: &[&str] = &[
    "run_meta",
    "mapping",
    "thread",
    "function",
    "frame",
    "callsite",
    "cpu_sample",
    "sample_counter",
    "source_snippet",
    "fn_cpu_summary",
    "line_cpu_summary",
    "codegen_remark",
];

fn schema(db: &Connection, limit: usize) -> Result<Value> {
    let mut stmt =
        db.prepare(r#"SELECT name,type,"notnull",pk FROM pragma_table_info(?1) ORDER BY cid"#)?;
    let mut tables = Vec::new();
    for name in TABLES.iter().take(limit) {
        let columns = stmt
            .query_map([name], |row| {
                Ok(json!({
                    "name": row.get::<_, String>(0)?,
                    "type": row.get::<_, String>(1)?,
                    "nullable": row.get::<_, i64>(2)? == 0 && row.get::<_, i64>(3)? == 0,
                    "primary_key": row.get::<_, i64>(3)?,
                }))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        tables
            .push(json!({"name": name, "columns": columns, "summary": name.ends_with("_summary")}));
    }
    Ok(json!({"tables": tables, "truncated": TABLES.len() > limit}))
}

struct Total {
    key: String,
    self_total: u64,
    inclusive_total: u64,
    sample_count: u64,
}

impl Total {
    fn json(&self) -> Value {
        json!({
            "function": self.key,
            "self_total": unsigned(self.self_total),
            "inclusive_total": unsigned(self.inclusive_total),
            "sample_count": unsigned(self.sample_count),
        })
    }

    fn weighted(&self, by: By) -> u64 {
        match by {
            By::SelfCost => self.self_total,
            By::Inclusive => self.inclusive_total,
        }
    }
}

const CHAIN: &str = "
    leaf_weights(leaf, weight, samples) AS (
        SELECT s.callsite_id,u64_sum(c.value),count(*)
        FROM cpu_sample s JOIN sample_counter c ON c.sample_id=s.id
        WHERE c.event=?1 GROUP BY s.callsite_id
    ),
    chain(leaf, site) AS (
        SELECT leaf,leaf FROM leaf_weights
        UNION ALL
        SELECT a.leaf,c.parent_id FROM chain a JOIN callsite c ON c.id=a.site
        WHERE c.parent_id IS NOT NULL
    )";

fn totals(
    db: &Connection,
    event: &str,
    by: By,
    group: Group,
    filter: &str,
    limit: Option<usize>,
) -> Result<Vec<Total>> {
    register_totals(db)?;
    let group = match group {
        Group::DefPath => "coalesce(f.def_path,f.instance_label)",
        Group::Instance => "f.instance_label",
    };
    let order = match by {
        By::SelfCost => "self_total",
        By::Inclusive => "inclusive_total",
    };
    // Group after forming a distinct membership per sample leaf. Summing
    // inclusive rows from separate monomorphs would count common stacks twice.
    let sql = format!(
        "WITH RECURSIVE {CHAIN}, membership(leaf, label, is_self) AS (
            SELECT a.leaf,{group},max(a.leaf=a.site)
            FROM chain a JOIN callsite c ON c.id=a.site JOIN frame fr ON fr.id=c.frame_id
            JOIN function f ON f.id=fr.function_id
            WHERE {group} GLOB ?2 OR f.name GLOB ?2 OR f.def_path GLOB ?2 GROUP BY a.leaf,{group}
        ), totals AS (
            SELECT label,u64_sum(CASE WHEN is_self THEN weight ELSE 0 END) AS self_total,
                u64_sum(weight) AS inclusive_total,u64_sum(samples) AS sample_count
            FROM membership JOIN leaf_weights USING(leaf) GROUP BY label
        ) SELECT label,self_total,inclusive_total,sample_count FROM totals
          ORDER BY length(CAST({order} AS TEXT)) DESC,CAST({order} AS TEXT) DESC,label
          LIMIT ?3"
    );
    let mut stmt = db.prepare(&sql)?;
    Ok(stmt
        .query_map(
            (
                event,
                filter,
                limit.map(i64::try_from).transpose()?.unwrap_or(-1),
            ),
            |row| {
                Ok(Total {
                    key: row.get(0)?,
                    self_total: row.get::<_, Unsigned>(1)?.0,
                    inclusive_total: row.get::<_, Unsigned>(2)?.0,
                    sample_count: row.get::<_, Unsigned>(3)?.0,
                })
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

fn annotate(db: &Connection, target: &str, limit: usize) -> Result<Value> {
    let (file, line) = match target.rsplit_once(':') {
        Some((file, line)) => match line.parse::<u32>() {
            Ok(line @ 1..) => (file, Some(line)),
            _ => (target, None),
        },
        None => (target, None),
    };
    register_totals(db)?;
    let mut stmt = db.prepare(include_str!("annotate.sql"))?;
    let mut rows = stmt.query((file, line, bounded_limit(limit)?))?;
    let mut result = Vec::new();
    while result.len() < limit {
        let Some(row) = rows.next()? else { break };
        result.push(json!({
            "file": row.get::<_, String>(0)?,
            "line": row.get::<_, u32>(1)?,
            "source": row.get::<_, Option<String>>(2)?,
            "cpu": serde_json::from_str::<Value>(row.get_ref(3)?.as_str()?)?,
            "remarks": serde_json::from_str::<Value>(row.get_ref(4)?.as_str()?)?,
        }));
    }
    Ok(json!({"rows": result, "limit": unsigned(limit as u64),
        "returned_rows": unsigned(result.len() as u64), "truncated": rows.next()?.is_some()}))
}

fn related(
    db: &Connection,
    target: &str,
    depth: u32,
    event: &str,
    callers: bool,
    limit: usize,
) -> Result<Value> {
    register_totals(db)?;
    let delta = if callers {
        "o.depth-c.depth"
    } else {
        "c.depth-o.depth"
    };
    let sql = format!("WITH RECURSIVE {CHAIN},
        anchors(leaf,depth) AS (
           SELECT DISTINCT a.leaf,c.depth FROM chain a JOIN callsite c ON c.id=a.site
           JOIN frame fr ON fr.id=c.frame_id JOIN function f ON f.id=fr.function_id
           WHERE f.def_path=?2 OR f.instance_label=?2 OR f.name=?2
        ),
        members(leaf,distance,function) AS (
           SELECT DISTINCT a.leaf,{delta},fr.function_id
           FROM chain a JOIN callsite c ON c.id=a.site JOIN anchors o ON o.leaf=a.leaf
           JOIN frame fr ON fr.id=c.frame_id
           WHERE ({delta}) BETWEEN 1 AND ?3 AND fr.function_id IS NOT NULL
        )
        SELECT distance AS depth,f.def_path,f.instance_label,f.name,u64_sum(weight) AS total,u64_sum(samples) AS sample_count
        FROM members JOIN function f ON f.id=members.function JOIN leaf_weights USING(leaf)
        GROUP BY distance,f.id ORDER BY distance,length(CAST(total AS TEXT)) DESC,total DESC,f.instance_label LIMIT ?4");
    select_result(
        db,
        &sql,
        (event, target, depth, bounded_limit(limit)?),
        limit,
    )
}

fn diff(
    db: &Connection,
    base_quality: &Value,
    head: &Path,
    by: By,
    group: Group,
    min: f64,
    limit: usize,
) -> Result<Value> {
    if !min.is_finite() || min < 0.0 {
        return Err("--min-delta-pct must be finite and nonnegative".into());
    }
    let head = Profile::open(head)?;
    let rhs = quality(head.connection())?;
    for name in ["target", "primary_event", "primary_unit", "clock"] {
        if base_quality[name] != rhs[name] {
            return Err(format!("incompatible captures have different {name}").into());
        }
    }
    if (base_quality["capture_mode"] == "replay") != (rhs["capture_mode"] == "replay") {
        return Err("incompatible live and replay captures".into());
    }
    let event = base_quality["primary_event"]
        .as_str()
        .ok_or("invalid primary event")?;
    let mut functions = BTreeMap::<String, (Option<u64>, Option<u64>)>::new();
    for row in totals(db, event, by, group, "*", None)? {
        let total = row.weighted(by);
        functions.entry(row.key).or_default().0 = Some(total);
    }
    for row in totals(head.connection(), event, by, group, "*", None)? {
        let total = row.weighted(by);
        functions.entry(row.key).or_default().1 = Some(total);
    }
    let mut result = Vec::new();
    for (function, (base, head)) in functions {
        let matched = match (base, head) {
            (None, _) => "head_only",
            (_, None) => "base_only",
            _ => "matched",
        };
        let delta = i128::from(head.unwrap_or_default()) - i128::from(base.unwrap_or_default());
        let pct = base
            .filter(|&n| n != 0)
            .map(|n| delta as f64 / n as f64 * 100.0);
        if matched == "matched" && pct.is_some_and(|p| p.abs() < min) {
            continue;
        }
        let delta_json = if delta.unsigned_abs() > JS_EXACT_U64 as u128 {
            json!(delta.to_string())
        } else {
            json!(delta as i64)
        };
        result.push((
            delta.unsigned_abs(),
            json!({"function": function,"base": base.map(unsigned),
            "head": head.map(unsigned), "match": matched, "delta": delta_json,"delta_pct": pct}),
        ));
    }
    result.sort_by(|a, b| b.0.cmp(&a.0));
    let truncated = result.len() > limit;
    let rows = result
        .into_iter()
        .take(limit)
        .map(|(_, row)| row)
        .collect::<Vec<_>>();
    Ok(
        json!({"head_quality": rhs, "event": event, "statistical_noise": event != "instructions",
        "rows": rows, "truncated": truncated}),
    )
}

fn authorize(context: AuthContext<'_>) -> Authorization {
    use AuthAction::*;
    match context.action {
        Select | Recursive => Authorization::Allow,
        // SQLite reports unqualified table reads without a database name when
        // no columns are read, for example SELECT count(*) FROM cpu_sample.
        Read { .. } if context.database_name.is_none_or(|name| name == "main") => {
            Authorization::Allow
        }
        Function { function_name }
            if !matches!(function_name, "load_extension" | "readfile" | "writefile") =>
        {
            Authorization::Allow
        }
        _ => Authorization::Deny,
    }
}

fn sql(db: &mut Connection, sql: &str, timeout: Duration, limit: usize) -> Result<Value> {
    if timeout.is_zero() {
        return Err("--timeout-ms must be positive".into());
    }
    let start = Instant::now();
    db.busy_timeout(timeout.min(Duration::from_millis(i32::MAX as u64)))?;
    db.authorizer(Some(authorize))?;
    db.progress_handler(1000, Some(move || start.elapsed() >= timeout))?;
    let result = select_result(db, sql, [], limit);
    if start.elapsed() >= timeout {
        return Err(format!("SQL query exceeded the {} ms timeout", timeout.as_millis()).into());
    }
    result
}

fn bounded_limit(limit: usize) -> Result<i64> {
    i64::try_from(limit)?
        .checked_add(1)
        .ok_or_else(|| "--limit is too large".into())
}

fn select_result(
    db: &Connection,
    sql: &str,
    params: impl rusqlite::Params,
    limit: usize,
) -> Result<Value> {
    let mut statement = db.prepare(sql)?;
    if !statement.readonly() || statement.column_count() == 0 {
        return Err("prof sql accepts one read-only statement that returns rows".into());
    }
    let columns = statement
        .column_names()
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let mut rows = statement.query(params)?;
    let mut result = Vec::new();
    let mut truncated = false;
    while let Some(row) = rows.next()? {
        if result.len() == limit {
            truncated = true;
            break;
        }
        result.push(sql_row(row, columns.len())?);
    }
    Ok(
        json!({"columns": columns, "rows": result, "limit": unsigned(limit as u64),
        "returned_rows": unsigned(result.len() as u64), "truncated": truncated}),
    )
}

fn sql_row(row: &Row<'_>, columns: usize) -> Result<Vec<Value>> {
    (0..columns)
        .map(|column| value(row.get_ref(column)?))
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::{ProfileWriter, RunMeta};

    use super::*;

    #[test]
    fn annotate_aggregates_cpu_and_remarks_before_joining_lines() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(include_str!("schema.sql")).unwrap();
        db.execute_batch(
            "INSERT INTO function VALUES (1,'app/math/f','f@u64','f','hirsel'),
                                         (2,'app/math/f','f@u8','f','hirsel'),
                                         (3,'app/math/g','g','g','hirsel');
             INSERT INTO line_cpu_summary VALUES ('math.hirsel',7,1,'instructions',20,2),
                                                ('math.hirsel',7,2,'instructions',15,1),
                                                ('math.hirsel',7,3,'instructions',90,9);
             INSERT INTO codegen_remark VALUES
                (1,'math.hirsel',7,3,'bounds_check','elided','source_remark',1),
                (2,'math.hirsel',7,3,'bounds_check','elided','source_remark',2),
                (2,'math.hirsel',7,9,'step_sync','emitted','source_remark',1),
                (2,'math.hirsel',8,1,'overflow_check','emitted','source_remark',1);",
        )
        .unwrap();

        let by_definition = annotate(&db, "app/math/f", 20).unwrap();
        assert_eq!(by_definition["returned_rows"], 2);
        assert_eq!(
            by_definition["rows"][0],
            json!({
                "file": "math.hirsel", "line": 7, "source": null,
                "cpu": [{"event": "instructions", "self_total": 35, "sample_count": 3}],
                "remarks": [
                    {"kind": "bounds_check", "outcome": "elided", "evidence_level": "source_remark", "count": 3},
                    {"kind": "step_sync", "outcome": "emitted", "evidence_level": "source_remark", "count": 1},
                ],
            })
        );
        assert_eq!(by_definition["rows"][1]["cpu"], json!([]));
        let by_line = annotate(&db, "math.hirsel:7", 20).unwrap();
        assert_eq!(by_line["returned_rows"], 1);
        assert_eq!(by_line["rows"][0]["cpu"][0]["self_total"], 125);
        assert_eq!(by_line["rows"][0]["cpu"][0]["sample_count"], 12);
    }

    fn fixture(path: &Path) {
        let writer = ProfileWriter::create(path).unwrap();
        writer.connection().execute_batch(
            "INSERT INTO thread VALUES (1,1,1,'main',0,NULL);
             INSERT INTO function VALUES (1,'app/math/recur','recur@u64','recur','hirsel'),
                                         (2,'app/math/recur','recur@u8','recur','hirsel');
             INSERT INTO frame VALUES (1,NULL,NULL,1,'math.hirsel',1,1,0),
                                      (2,NULL,NULL,2,'math.hirsel',2,1,1);
             INSERT INTO callsite VALUES (1,NULL,1,0), (2,1,2,1);
             INSERT INTO cpu_sample VALUES (1,1,1,NULL,0,2,'0x10',9007199254740993,'fiber','fiber_root');
             INSERT INTO sample_counter VALUES (1,'instructions',9007199254740993);
             INSERT INTO source_snippet VALUES ('math.hirsel',1,2,'fn recur() {\n}\n');",
        ).unwrap();
        writer
            .finish(&RunMeta {
                capture_mode: "attach",
                target: "x86_64-unknown-linux-gnu",
                cmdline: "[\"fixture\"]",
                build_id: "",
                image_digest: "",
                symbol_tier: "full_dwarf",
                clock: "CLOCK_MONOTONIC",
                primary_event: "instructions",
                primary_unit: "instructions",
                freq_hz: 997,
                fallback_reason: "",
                start_ns: 0,
                duration_ns: u64::MAX,
                samples_lost: 0,
            })
            .unwrap();
    }

    #[test]
    fn sql_is_read_only_and_json_keeps_large_unsigned_values_exact() {
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path().join("query.hprof");
        fixture(&profile);
        let sql = |statement| {
            query(
                &profile,
                Query::Sql {
                    statement,
                    timeout: Duration::from_secs(5),
                },
                1,
            )
        };
        let rows = sql("SELECT period FROM cpu_sample UNION ALL SELECT 2").unwrap();
        assert_eq!(rows.quality["samples_total"], 1);
        assert_eq!(rows.quality["duration_ns"], "18446744073709551615");
        assert_eq!(
            rows.result,
            json!({
                "columns": ["period"], "rows": [["9007199254740993"]],
                "limit": 1, "returned_rows": 1, "truncated": true,
            })
        );
        for command in [
            "UPDATE run_meta SET value=1",
            "ATTACH DATABASE ':memory:' AS second",
            "CREATE TEMP TABLE second(a)",
            "SELECT 1; ATTACH DATABASE ':memory:' AS second",
        ] {
            let error = sql(command).err().expect("write or ATTACH must fail");
            assert!(
                error.to_string().contains("not authoriz"),
                "{command} => {error}"
            );
        }
        let slow = query(
            &profile,
            Query::Sql {
                statement: "WITH RECURSIVE big(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM big) SELECT sum(x) FROM big",
                timeout: Duration::from_millis(5),
            },
            1,
        );
        assert!(slow.err().unwrap().to_string().contains("timeout"));
        assert_eq!(
            sql("SELECT count(*) FROM cpu_sample").unwrap().result["rows"],
            json!([[1]])
        );
    }

    #[test]
    fn grouping_instances_counts_an_event_once_per_definition() {
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path().join("query.hprof");
        fixture(&profile);
        let get = |group| {
            query(
                &profile,
                Query::Top {
                    by: By::Inclusive,
                    event: None,
                    group,
                    filter: Some("app/math/*"),
                },
                20,
            )
            .unwrap()
            .result
        };
        let grouped = get(Group::DefPath);
        // Two instances of recur appear in the only sample. The period
        // belongs to both instances but only once to the source definition.
        assert_eq!(
            grouped["rows"],
            json!([{
                "function": "app/math/recur", "self_total": "9007199254740993",
                "inclusive_total": "9007199254740993", "sample_count": 1,
            }])
        );
        let instances = get(Group::Instance);
        assert_eq!(instances["rows"].as_array().unwrap().len(), 2);
        assert_eq!(instances["rows"][0]["self_total"], 0);
        assert_eq!(instances["rows"][1]["self_total"], "9007199254740993");
        assert_eq!(instances["rows"][0]["inclusive_total"], "9007199254740993");
    }
}
