//! Reconstruct runtime observations by monotonic time. Producers drain
//! independently, so the file's chunk order is not a scheduling order.
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use rusqlite::{Connection, params};

use crate::profile::{Result, Unsigned};

const MAGIC: &[u8; 8] = b"HIRSOBS1";
const END: u16 = u16::MAX - 1;
const LOSS: u16 = u16::MAX;
pub(crate) const MAPPING: u16 = 12;

pub(crate) struct Record {
    pub ts: u64,
    pub tid: u32,
    pub kind: u16,
    pub words: Vec<u64>,
    pub bytes: Vec<u8>,
}

pub(crate) struct Observation {
    pub records: Vec<Record>,
    pub lost: u64,
    pub complete: bool,
}

impl Observation {
    pub(crate) fn read(path: &Path) -> Result<Self> {
        let mut file = BufReader::new(File::open(path)?);
        let mut magic = [0; 8];
        file.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err("invalid runtime observation header".into());
        }
        let mut trace = Self {
            records: Vec::new(),
            lost: 0,
            complete: false,
        };
        let mut header = [0u8; 20];
        loop {
            if !read_record_part(&mut file, &mut header)? {
                break;
            }
            let ts = u64::from_le_bytes(header[..8].try_into()?);
            let tid = u32::from_le_bytes(header[8..12].try_into()?);
            let kind = u16::from_le_bytes(header[12..14].try_into()?);
            let words = usize::from(u16::from_le_bytes(header[14..16].try_into()?));
            let bytes = u32::from_le_bytes(header[16..20].try_into()?) as usize;
            if (0..=12).contains(&kind) && kind != MAPPING && bytes != 0
                || words * 8 + bytes > 64 * 1024 - header.len()
            {
                return Err("invalid runtime observation record length".into());
            }
            let mut data = vec![0; words * 8 + bytes];
            if !read_record_part(&mut file, &mut data)? {
                break;
            }
            let words = data[..words * 8]
                .chunks_exact(8)
                .map(|word| u64::from_le_bytes(word.try_into().unwrap()))
                .collect::<Vec<_>>();
            match (kind, words.as_slice(), bytes) {
                (END, [], 0) => {
                    if file.read(&mut [0])? != 0 {
                        return Err("data after runtime observation end".into());
                    }
                    trace.complete = true;
                    break;
                }
                (LOSS, [lost], 0) => {
                    trace.lost = trace
                        .lost
                        .checked_add(*lost)
                        .ok_or("runtime loss count overflows")?
                }
                (LOSS | END, _, _) => return Err("invalid runtime observation footer".into()),
                _ => {
                    let bytes = data.split_off(words.len() * 8);
                    trace.records.push(Record {
                        ts,
                        tid,
                        kind,
                        words,
                        bytes,
                    });
                }
            }
        }
        // Stable order breaks a timestamp tie only within one worker. The
        // producer preserves that worker's chunk order.
        trace.records.sort_by_key(|record| record.ts);
        Ok(trace)
    }

    pub(crate) fn store(&self, db: &Connection) -> Result<()> {
        let mut import = Import::new(db);
        for record in &self.records {
            if !crate::memory::MemoryImport::accepts(record.kind) {
                import.record(record)?;
            }
        }
        for (token, timing) in std::mem::take(&mut import.io) {
            import.io_timing(token, timing)?;
        }
        for (key, value) in [
            (
                "runtime_events",
                self.records
                    .iter()
                    .filter(|record| record.kind != MAPPING)
                    .count() as u64,
            ),
            ("runtime_events_lost", self.lost),
            ("runtime_trace_complete", u64::from(self.complete)),
            ("runtime_unpaired_events", import.unpaired),
        ] {
            db.execute(
                "INSERT INTO run_meta VALUES (?1,?2)",
                (key, Unsigned(value)),
            )?;
        }
        attribute_host_samples(db)?;
        Ok(())
    }
}

fn read_record_part(input: &mut impl Read, bytes: &mut [u8]) -> std::io::Result<bool> {
    match input.read_exact(bytes) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => Ok(false),
        Err(error) => Err(error),
    }
}

fn attribute_host_samples(db: &Connection) -> Result<()> {
    db.execute_batch(
        "UPDATE cpu_sample AS s SET host_stack_interval_id = (
             SELECT h.id FROM host_stack_interval h JOIN thread t ON t.id = s.thread_id
             WHERE h.tid = t.tid AND s.ts_ns >= h.start_ns AND s.ts_ns < h.end_ns
             ORDER BY h.start_ns DESC LIMIT 1
         ) WHERE s.stack_root = 'host';
         UPDATE cpu_sample SET task_id = (
             SELECT task_id FROM host_stack_interval WHERE id = cpu_sample.host_stack_interval_id
         ) WHERE host_stack_interval_id IS NOT NULL;",
    )?;
    Ok(())
}

type Task = (u64, u64);

#[derive(Default)]
struct Timing {
    callbacks: [u64; 5],
    pending: u64,
}

struct Import<'a> {
    db: &'a Connection,
    tasks: HashMap<Task, i64>,
    // The most recent non-running scheduler state and the active dispatch
    // have different end points. Dispatch is paired on its actual OS thread.
    states: HashMap<Task, i64>,
    dispatches: HashMap<(Task, u32, u64), i64>,
    host: HashMap<u32, (i64, Task, u64)>,
    io: HashMap<u64, Timing>,
    unpaired: u64,
}

impl<'a> Import<'a> {
    fn new(db: &'a Connection) -> Self {
        Self {
            db,
            tasks: HashMap::new(),
            states: HashMap::new(),
            dispatches: HashMap::new(),
            host: HashMap::new(),
            io: HashMap::new(),
            unpaired: 0,
        }
    }

    fn task(&mut self, key: Task) -> Result<Option<i64>> {
        if key.0 == 0 {
            return Ok(None);
        }
        if let Some(&id) = self.tasks.get(&key) {
            return Ok(Some(id));
        }
        self.db.execute(
            "INSERT INTO task(unit_id,runtime_id) VALUES (?1,?2)",
            (Unsigned(key.0), Unsigned(key.1)),
        )?;
        let id = self.db.last_insert_rowid();
        self.tasks.insert(key, id);
        Ok(Some(id))
    }

    fn close_slice(&mut self, id: i64, end: Unsigned) -> Result<()> {
        self.db
            .execute("UPDATE task_slice SET end_ns=?2 WHERE id=?1", (id, end))?;
        Ok(())
    }

    fn record(&mut self, record: &Record) -> Result<()> {
        let time = Unsigned(record.ts);
        let tid = record.tid;
        match (record.kind, record.words.as_slice()) {
            (1, &[unit, task, parent_unit, parent_task]) => {
                let id = self.task((unit, task))?;
                let parent = self.task((parent_unit, parent_task))?;
                self.db.execute(
                    "UPDATE task SET start_ns=?2,parent_id=?3 WHERE id=?1",
                    (id, time, parent),
                )?;
            }
            (2, &[unit, task, state, reason, object]) => {
                const STATES: &[&str] =
                    &["ready", "running", "waiting", "flow", "provider", "ended"];
                const REASONS: &[Option<&str>] = &[
                    None,
                    Some("channel_recv"),
                    Some("channel_send"),
                    Some("join"),
                    Some("timer"),
                    Some("latest"),
                    Some("provider_io"),
                    Some("step_slice"),
                ];
                let state = STATES
                    .get(usize::try_from(state)?)
                    .ok_or("invalid observed task state")?;
                let reason = REASONS
                    .get(usize::try_from(reason)?)
                    .ok_or("invalid observed wait reason")?;
                let key = (unit, task);
                if let Some(previous) = self.states.remove(&key) {
                    self.close_slice(previous, time)?;
                }
                let id = self.task(key)?;
                if *state == "ended" {
                    self.db
                        .execute("UPDATE task SET end_ns=?2 WHERE id=?1", (id, time))?;
                } else if *state != "running" {
                    let object = (object != 0).then_some(Unsigned(object));
                    self.db.execute(
                        "INSERT INTO task_slice(task_id,tid,state,wait_reason,object_id,start_ns)
                         VALUES (?1,?2,?3,?4,?5,?6)",
                        (id, tid, state, reason, object, time),
                    )?;
                    self.states.insert(key, self.db.last_insert_rowid());
                }
            }
            (3, &[unit, task, worker]) => {
                let key = (unit, task);
                if let Some(previous) = self.states.remove(&key) {
                    self.close_slice(previous, time)?;
                }
                let id = self.task(key)?;
                self.db.execute(
                    "INSERT INTO task_slice(task_id,tid,worker_id,state,start_ns)
                     VALUES (?1,?2,?3,'running',?4)",
                    (id, tid, i64::try_from(worker)?, time),
                )?;
                if self
                    .dispatches
                    .insert((key, tid, worker), self.db.last_insert_rowid())
                    .is_some()
                {
                    self.unpaired += 1;
                }
            }
            (4, &[unit, task, worker]) => {
                match self.dispatches.remove(&((unit, task), tid, worker)) {
                    Some(id) => self.close_slice(id, time)?,
                    None => self.unpaired += 1,
                }
            }
            (5, &[unit, task, source, source_unit, source_task, object]) => {
                const SOURCES: &[&str] = &["", "task", "timer", "net", "cancel", "unknown"];
                let source = SOURCES
                    .get(usize::try_from(source)?)
                    .filter(|s| !s.is_empty())
                    .ok_or("invalid observed wake source")?;
                let from = self.task((source_unit, source_task))?;
                let to = self.task((unit, task))?;
                let object = (object != 0).then_some(Unsigned(object));
                self.db.execute(
                    "INSERT INTO causal_edge(src_kind,src_id,dst_task_id,object_id,ts_ns)
                     VALUES (?1,?2,?3,?4,?5)",
                    (source, from, to, object, time),
                )?;
            }
            (6, &[unit, task, site]) => {
                let key = (unit, task);
                let id = self.task(key)?;
                self.db.execute(
                    "INSERT INTO host_stack_interval(tid,task_id,site,start_ns) VALUES (?1,?2,?3,?4)",
                    (tid, id, Unsigned(site), time),
                )?;
                if self
                    .host
                    .insert(tid, (self.db.last_insert_rowid(), key, site))
                    .is_some()
                {
                    self.unpaired += 1;
                }
            }
            (7, &[unit, task, site]) => match self.host.remove(&tid) {
                Some((id, key, at)) if key == (unit, task) && at == site => {
                    self.db.execute(
                        "UPDATE host_stack_interval SET end_ns=?2 WHERE id=?1",
                        (id, time),
                    )?;
                }
                _ => self.unpaired += 1,
            },
            (8, &[token, unit, task, operation, site]) => {
                let id = self.task((unit, task))?;
                self.db.execute(
                    "INSERT INTO io_op(token,task_id,operation,site,start_ns) VALUES (?1,?2,?3,?4,?5)",
                    (Unsigned(token), id, u8::try_from(operation)?, Unsigned(site), time),
                )?;
                self.io.insert(token, Timing::default());
            }
            (9, &[token, phase, elapsed]) => {
                if let Some(timing) = self.io.get_mut(&token) {
                    let phase = phase
                        .checked_sub(1)
                        .and_then(|i| usize::try_from(i).ok())
                        .and_then(|i| timing.callbacks.get_mut(i))
                        .ok_or("invalid observed callback phase")?;
                    *phase = phase
                        .checked_add(elapsed)
                        .ok_or("callback time overflows")?;
                } else {
                    self.unpaired += 1;
                }
            }
            (10, &[token, elapsed]) => {
                if let Some(timing) = self.io.get_mut(&token) {
                    timing.pending = timing
                        .pending
                        .checked_add(elapsed)
                        .ok_or("pending time overflows")?;
                } else {
                    self.unpaired += 1;
                }
            }
            (11, &[token, bytes, tag, code, status]) => {
                if let Some(timing) = self.io.remove(&token) {
                    self.io_timing(token, timing)?;
                    self.db.execute(
                        "UPDATE io_op SET end_ns=?2,bytes=?3,error_tag=?4,error_code=?5,status=?6 WHERE token=?1",
                        (Unsigned(token), time, Unsigned(bytes), u8::try_from(tag)?, u32::try_from(code)?, u8::try_from(status)?),
                    )?;
                } else {
                    self.unpaired += 1;
                }
            }
            (MAPPING, []) | (16..=31, _) => {}
            _ => {
                return Err(format!(
                    "invalid runtime observation kind or payload {}",
                    record.kind
                )
                .into());
            }
        }
        Ok(())
    }

    fn io_timing(&self, token: u64, timing: Timing) -> Result<()> {
        let total = timing.callbacks.iter().try_fold(0u64, |total, &part| {
            total.checked_add(part).ok_or("callback time overflows")
        })?;
        let [wait, issue, resume, deliver, abort] = timing.callbacks.map(Unsigned);
        self.db.execute(
            "UPDATE io_op SET wait_callback_ns=?2,issue_callback_ns=?3,resume_callback_ns=?4,
                   deliver_callback_ns=?5,abort_callback_ns=?6,callback_ns=?7,pending_ns=?8 WHERE token=?1",
            params![Unsigned(token),wait,issue,resume,deliver,abort,Unsigned(total),Unsigned(timing.pending)],
        )?;
        Ok(())
    }
}
