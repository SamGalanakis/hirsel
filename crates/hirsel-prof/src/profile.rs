use std::path::{Path, PathBuf};

use rusqlite::functions::{Aggregate, Context, FunctionFlags};
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSqlOutput, ValueRef};
use rusqlite::{Connection, OpenFlags, ToSql};
use tempfile::NamedTempFile;

const APPLICATION_ID: i64 = 0x48505246;
const HPROF_VERSION: i64 = 5;

pub(crate) type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// A counter or other unsigned field. SQLite INTEGER is signed, and converting
/// a larger integer to REAL would lose bits.
#[derive(Clone, Copy, Default)]
pub(crate) struct Unsigned(pub u64);

impl FromSql for Unsigned {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        match value {
            ValueRef::Integer(number) => u64::try_from(number)
                .map(Self)
                .map_err(|_| FromSqlError::OutOfRange(number)),
            ValueRef::Text(_) => value
                .as_str()?
                .parse()
                .map(Self)
                .map_err(FromSqlError::other),
            _ => Err(FromSqlError::InvalidType),
        }
    }
}

impl ToSql for Unsigned {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(match i64::try_from(self.0) {
            Ok(integer) => integer.into(),
            Err(_) => self.0.to_string().into(),
        })
    }
}

/// Nanoseconds in numeric sort order across SQLite's integer and text ranges.
#[derive(Clone, Copy)]
pub(crate) struct Timestamp(pub u64);

impl FromSql for Timestamp {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        Unsigned::column_result(value).map(|Unsigned(number)| Self(number))
    }
}

impl ToSql for Timestamp {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(match i64::try_from(self.0) {
            Ok(integer) => integer.into(),
            Err(_) => format!("{:020}", self.0).into(),
        })
    }
}

pub(crate) fn register_elapsed(db: &Connection) -> rusqlite::Result<()> {
    db.create_scalar_function(
        "u64_elapsed",
        2,
        FunctionFlags::SQLITE_DETERMINISTIC | FunctionFlags::SQLITE_DIRECTONLY,
        |cx| {
            let end = cx.get::<Timestamp>(0)?.0;
            let start = cx.get::<Timestamp>(1)?.0;
            end.checked_sub(start).map(Timestamp).ok_or_else(|| {
                rusqlite::Error::UserFunctionError("profile interval ends before it starts".into())
            })
        },
    )
}

pub(crate) struct UnsignedSum;

impl Aggregate<u64, Unsigned> for UnsignedSum {
    fn init(&self, _: &mut Context<'_>) -> rusqlite::Result<u64> {
        Ok(0)
    }

    fn step(&self, cx: &mut Context<'_>, accumulator: &mut u64) -> rusqlite::Result<()> {
        *accumulator = accumulator
            .checked_add(cx.get::<Unsigned>(0)?.0)
            .ok_or_else(|| {
                rusqlite::Error::UserFunctionError("profile counter exceeds u64".into())
            })?;
        Ok(())
    }

    fn finalize(
        &self,
        _: &mut Context<'_>,
        accumulator: Option<u64>,
    ) -> rusqlite::Result<Unsigned> {
        Ok(Unsigned(accumulator.unwrap_or_default()))
    }
}

pub(crate) fn register_totals(db: &Connection) -> rusqlite::Result<()> {
    db.create_aggregate_function(
        "u64_sum",
        1,
        FunctionFlags::SQLITE_DETERMINISTIC,
        UnsignedSum,
    )
}

/// Capture provenance. Sample-derived counters are computed during finalization.
pub struct RunMeta<'a> {
    pub capture_mode: &'a str,
    pub target: &'a str,
    pub cmdline: &'a str,
    pub build_id: &'a str,
    pub image_digest: &'a str,
    pub symbol_tier: &'a str,
    pub clock: &'a str,
    pub primary_event: &'a str,
    pub primary_unit: &'a str,
    pub freq_hz: u64,
    pub fallback_reason: &'a str,
    pub start_ns: u64,
    pub duration_ns: u64,
    pub samples_lost: u64,
}

/// A private capture database published atomically only after finalization.
pub struct ProfileWriter {
    db: Connection,
    scratch: NamedTempFile,
    path: PathBuf,
}

impl ProfileWriter {
    pub fn create(path: &Path) -> Result<Self> {
        let directory = path.parent().filter(|dir| !dir.as_os_str().is_empty());
        let scratch = NamedTempFile::new_in(directory.unwrap_or_else(|| Path::new(".")))?;
        let db = Connection::open_with_flags(scratch.path(), OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        db.execute_batch("PRAGMA foreign_keys = ON; BEGIN IMMEDIATE;")?;
        db.execute_batch(include_str!("schema.sql"))?;
        register_totals(&db)?;
        Ok(Self {
            db,
            scratch,
            path: path.to_owned(),
        })
    }

    pub fn connection(&self) -> &Connection {
        &self.db
    }

    pub fn finish(self, meta: &RunMeta<'_>) -> Result<()> {
        self.db.execute_batch(include_str!("summarize.sql"))?;
        {
            let mut insert = self.db.prepare("INSERT INTO run_meta VALUES (?1, ?2)")?;
            for (key, value) in [
                ("capture_mode", meta.capture_mode),
                ("target", meta.target),
                ("cmdline", meta.cmdline),
                ("build_id", meta.build_id),
                ("image_digest", meta.image_digest),
                ("symbol_tier", meta.symbol_tier),
                ("clock", meta.clock),
                ("primary_event", meta.primary_event),
                ("primary_unit", meta.primary_unit),
                ("fallback_reason", meta.fallback_reason),
            ] {
                insert.execute((&key, &value))?;
            }
            for (key, value) in [
                ("hprof_version", HPROF_VERSION as u64),
                ("freq_hz", meta.freq_hz),
                ("samples_lost", meta.samples_lost),
            ] {
                insert.execute((key, Unsigned(value)))?;
            }
            insert.execute(("start_ns", Timestamp(meta.start_ns)))?;
            insert.execute(("duration_ns", Timestamp(meta.duration_ns)))?;
        }
        self.db.execute_batch(
            "INSERT INTO run_meta SELECT 'samples_total', count(*) FROM cpu_sample;
             INSERT INTO run_meta SELECT 'stacks_truncated', count(*) FROM cpu_sample WHERE stack_root = 'truncated';
             INSERT INTO run_meta SELECT 'unknown_mapping', count(*) FROM cpu_sample WHERE unwind_status = 'unknown_mapping';
             INSERT INTO run_meta SELECT 'unwind_failed', count(*) FROM cpu_sample WHERE stack_root = 'failed';
             INSERT INTO run_meta SELECT 'host_root_samples', count(*) FROM cpu_sample WHERE stack_root = 'host';
             INSERT INTO run_meta SELECT 'kernel_samples', count(*) FROM cpu_sample WHERE stack_root = 'kernel';
             INSERT INTO run_meta SELECT 'unresolved_samples', count(*)
                 FROM cpu_sample s JOIN callsite c ON c.id = s.callsite_id
                 JOIN frame f ON f.id = c.frame_id WHERE f.function_id IS NULL;
             COMMIT;",
        )?;
        self.db.close().map_err(|(_, error)| error)?;
        self.scratch.as_file().sync_all()?;
        self.scratch.persist_noclobber(self.path)?;
        Ok(())
    }
}

/// A read-only profile with the current format markers.
pub struct Profile {
    db: Connection,
}

impl Profile {
    pub fn open(path: &Path) -> Result<Self> {
        let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        db.execute_batch("PRAGMA trusted_schema = OFF; PRAGMA query_only = ON; BEGIN DEFERRED;")?;
        let application: i64 = db.pragma_query_value(None, "application_id", |r| r.get(0))?;
        if application != APPLICATION_ID {
            return Err(format!(
                "unsupported .hprof application_id: {application:#x}, expected {APPLICATION_ID:#x}"
            )
            .into());
        }
        let version: i64 = db.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version != HPROF_VERSION {
            return Err(
                format!("unsupported .hprof version {version}, expected {HPROF_VERSION}").into(),
            );
        }
        register_elapsed(&db)?;
        register_totals(&db)?;
        Ok(Self { db })
    }

    pub fn connection(&self) -> &Connection {
        &self.db
    }

    pub(crate) fn connection_mut(&mut self) -> &mut Connection {
        &mut self.db
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finalized_profile_counts_recursion_once_and_survives_source_removal() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("input");
        std::fs::create_dir(&input).unwrap();
        let snippet = "fn recur() {}\n";
        std::fs::write(input.join("workload.hirsel"), snippet).unwrap();
        std::fs::write(input.join("workload"), b"fixture image").unwrap();
        let path = dir.path().join("workload.hprof");
        let writer = ProfileWriter::create(&path).unwrap();
        let db = writer.connection();
        db.execute_batch(
            "INSERT INTO thread VALUES (1,1,1,'main',0,NULL);
             INSERT INTO function VALUES (1,'app.recur','recur','recur','hirsel'),
                                         (2,'app.leaf','leaf','leaf','hirsel');
             INSERT INTO frame VALUES (1,NULL,NULL,1,'workload.hirsel',1,1,0),
                                      (2,NULL,NULL,2,'workload.hirsel',2,1,0);
             INSERT INTO callsite VALUES (1,NULL,1,0), (2,1,1,1), (3,2,2,2);
             INSERT INTO cpu_sample VALUES (1,1,1,NULL,NULL,0,2,'0x10','cycles',3,'fiber','fiber_root'),
                                           (2,2,1,NULL,NULL,0,3,'0x20','cycles',5,'fiber','fiber_root');",
        )
        .unwrap();
        db.execute(
            "INSERT INTO source_snippet VALUES ('workload.hirsel',1,1,?1)",
            [std::fs::read_to_string(input.join("workload.hirsel")).unwrap()],
        )
        .unwrap();
        writer
            .finish(&RunMeta {
                capture_mode: "run",
                target: "x86_64-unknown-linux-gnu",
                cmdline: "workload",
                build_id: "",
                image_digest: "",
                symbol_tier: "full_dwarf",
                clock: "CLOCK_MONOTONIC",
                primary_event: "cycles",
                primary_unit: "cycles",
                freq_hz: 997,
                fallback_reason: "",
                start_ns: 0,
                duration_ns: 3,
                samples_lost: 0,
            })
            .unwrap();
        std::fs::remove_dir_all(input).unwrap();

        let profile = Profile::open(&path).unwrap();
        let db = profile.connection();
        let totals: (i64, i64, i64) = db.query_row(
            "SELECT self_total,inclusive_total,sample_count FROM fn_cpu_summary WHERE function_id=1",
            [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).unwrap();
        // recur -> recur weighs 3, and recur -> recur -> leaf weighs 5.
        assert_eq!(totals, (3, 8, 2));
        let saved: String = db
            .query_row("SELECT text FROM source_snippet", [], |r| r.get(0))
            .unwrap();
        assert_eq!(saved, snippet);
    }

    #[test]
    fn sample_schema_requires_one_event_and_one_exact_unsigned_period() {
        use rusqlite::types::Value;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("samples.hprof");
        let writer = ProfileWriter::create(&path).unwrap();
        let db = writer.connection();
        db.execute_batch(
            "INSERT INTO thread VALUES (1,1,1,'main',0,NULL);
             INSERT INTO function VALUES (1,'app.main','main','main','hirsel');
             INSERT INTO frame VALUES (1,NULL,NULL,1,'main.hirsel',1,1,0);
             INSERT INTO callsite VALUES (1,NULL,1,0);",
        )
        .unwrap();
        let missing = db.execute(
            "INSERT INTO cpu_sample(ts_ns,thread_id,cpu,callsite_id,raw_pc_hex,period,stack_root,unwind_status)
             VALUES (1,1,0,1,'0x10',7,'fiber','fiber_root')",
            [],
        );
        assert_eq!(
            missing.unwrap_err().sqlite_error_code(),
            Some(rusqlite::ErrorCode::ConstraintViolation)
        );

        let insert = "INSERT INTO cpu_sample(ts_ns,thread_id,cpu,callsite_id,raw_pc_hex,event,period,stack_root,unwind_status)
                      VALUES (1,1,0,1,'0x10',?1,?2,'fiber','fiber_root')";
        for invalid in [
            Value::Integer(-1),
            Value::Real(1.25),
            Value::Text("9223372036854775807".into()),
            Value::Text("18446744073709551616".into()),
            Value::Text("9223372036854775808\0".into()),
        ] {
            let error = db.execute(insert, ("cycles", invalid)).unwrap_err();
            assert_eq!(
                error.sqlite_error_code(),
                Some(rusqlite::ErrorCode::ConstraintViolation)
            );
        }
        db.execute(insert, ("cycles", Unsigned(u64::MAX))).unwrap();
        db.execute(insert, ("instructions", Unsigned(7))).unwrap();
        db.execute(insert, ("instructions", Unsigned(9))).unwrap();
        writer
            .finish(&RunMeta {
                capture_mode: "run",
                target: "",
                cmdline: "",
                build_id: "",
                image_digest: "",
                symbol_tier: "",
                clock: "CLOCK_MONOTONIC",
                primary_event: "cycles",
                primary_unit: "cycles",
                freq_hz: 997,
                fallback_reason: "",
                start_ns: 0,
                duration_ns: 2,
                samples_lost: 0,
            })
            .unwrap();

        let profile = Profile::open(&path).unwrap();
        let db = profile.connection();
        for table in ["fn_cpu_summary", "line_cpu_summary"] {
            let mut rows = db
                .prepare(&format!(
                    "SELECT event,self_total,sample_count FROM {table} ORDER BY event"
                ))
                .unwrap();
            let values = rows
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Unsigned>(1)?.0,
                        r.get::<_, i64>(2)?,
                    ))
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            assert_eq!(
                values,
                [
                    ("cycles".into(), u64::MAX, 1),
                    ("instructions".into(), 16, 2)
                ]
            );
        }

        let v1 = dir.path().join("v1.hprof");
        Connection::open(&v1)
            .unwrap()
            .execute_batch("PRAGMA application_id = 0x48505246; PRAGMA user_version = 1;")
            .unwrap();
        let error = Profile::open(&v1).err().unwrap();
        assert_eq!(
            error.to_string(),
            format!("unsupported .hprof version 1, expected {HPROF_VERSION}")
        );
    }
}
