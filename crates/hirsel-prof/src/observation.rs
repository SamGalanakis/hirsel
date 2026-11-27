//! Reconstruct runtime observations by monotonic time. Producers drain
//! independently, so the file's chunk order is not a scheduling order.
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use hirsel_observation_format::{END, HEADER_BYTES, LOSS, MAGIC, payload_bytes};
use rusqlite::{Connection, params};

use crate::memory::MemoryEvent;
use crate::profile::{Result, Timestamp, Unsigned};

type Task = (u64, u64);

pub(crate) enum RecordKind {
    Task {
        task: Task,
        parent: Task,
    },
    State {
        task: Task,
        state: &'static str,
        reason: Option<&'static str>,
        object: u64,
    },
    DispatchBegin {
        task: Task,
        worker: i64,
    },
    DispatchEnd {
        task: Task,
        worker: i64,
    },
    Wake {
        task: Task,
        source: &'static str,
        from: Task,
        object: u64,
    },
    HostBegin {
        task: Task,
        site: u64,
    },
    HostEnd {
        task: Task,
        site: u64,
    },
    IoBegin {
        token: u64,
        task: Task,
        operation: u8,
        site: u64,
    },
    IoCallback {
        token: u64,
        phase: usize,
        elapsed: u64,
    },
    IoPending {
        token: u64,
        elapsed: u64,
    },
    IoEnd {
        token: u64,
        bytes: u64,
        tag: u8,
        code: u32,
        status: u8,
    },
    Mapping(Vec<u8>),
    /// A thread stack the runtime sampled itself: the interrupted PC, then
    /// each return address.
    CpuSample {
        tid: u32,
        period: u64,
        truncated: bool,
        pcs: Vec<u64>,
    },
    /// The loaded-code frames of the sample before it on the same thread: each
    /// frame's index in the sample's addresses, its offset in its image's text
    /// and the first 16 bytes of the image's module digest (PACK-9).
    CodeFrames {
        tid: u32,
        frames: Vec<(u32, u32, [u8; 16])>,
    },
    Memory {
        event: MemoryEvent,
        stack: Vec<u8>,
    },
    End,
    Loss {
        lost: u64,
        memory_emitted: u64,
    },
}

impl RecordKind {
    fn decode(kind: u16, words: &[u64], bytes: Vec<u8>) -> Result<Self> {
        let record = match (kind, words, bytes.is_empty()) {
            (1, &[unit, task, parent_unit, parent_task], true) => Self::Task {
                task: (unit, task),
                parent: (parent_unit, parent_task),
            },
            (2, &[unit, task, state, reason, object], true) => {
                const STATES: &[&str] = &["ready", "running", "waiting", "provider", "ended"];
                const REASONS: &[Option<&str>] = &[
                    None,
                    Some("channel_recv"),
                    Some("channel_send"),
                    Some("join"),
                    Some("timer"),
                    Some("provider_io"),
                    Some("step_slice"),
                ];
                Self::State {
                    task: (unit, task),
                    state: STATES
                        .get(usize::try_from(state)?)
                        .ok_or("invalid observed task state")?,
                    reason: *REASONS
                        .get(usize::try_from(reason)?)
                        .ok_or("invalid observed wait reason")?,
                    object,
                }
            }
            (3, &[unit, task, worker], true) => Self::DispatchBegin {
                task: (unit, task),
                worker: i64::try_from(worker)?,
            },
            (4, &[unit, task, worker], true) => Self::DispatchEnd {
                task: (unit, task),
                worker: i64::try_from(worker)?,
            },
            (5, &[unit, task, source, source_unit, source_task, object], true) => {
                const SOURCES: &[&str] = &["", "task", "timer", "net", "cancel", "unknown"];
                Self::Wake {
                    task: (unit, task),
                    source: SOURCES
                        .get(usize::try_from(source)?)
                        .filter(|s| !s.is_empty())
                        .ok_or("invalid observed wake source")?,
                    from: (source_unit, source_task),
                    object,
                }
            }
            (6, &[unit, task, site], true) => Self::HostBegin {
                task: (unit, task),
                site,
            },
            (7, &[unit, task, site], true) => Self::HostEnd {
                task: (unit, task),
                site,
            },
            (8, &[token, unit, task, operation, site], true) => Self::IoBegin {
                token,
                task: (unit, task),
                operation: u8::try_from(operation)?,
                site,
            },
            (9, &[token, phase @ 1..=5, elapsed], true) => Self::IoCallback {
                token,
                phase: (phase - 1) as usize,
                elapsed,
            },
            (9, &[_, _, _], true) => return Err("invalid observed callback phase".into()),
            (10, &[token, elapsed], true) => Self::IoPending { token, elapsed },
            (11, &[token, bytes, tag, code, status], true) => Self::IoEnd {
                token,
                bytes,
                tag: u8::try_from(tag)?,
                code: u32::try_from(code)?,
                status: u8::try_from(status)?,
            },
            (12, [], _) => Self::Mapping(bytes),
            (13, &[tid, period, flags @ 0..=1, ref pcs @ ..], true) if !pcs.is_empty() => {
                Self::CpuSample {
                    tid: u32::try_from(tid)?,
                    period,
                    truncated: flags == 1,
                    pcs: pcs.to_vec(),
                }
            }
            (14, &[tid, ref frames @ ..], true)
                if !frames.is_empty() && frames.len().is_multiple_of(4) =>
            {
                Self::CodeFrames {
                    tid: u32::try_from(tid)?,
                    frames: frames
                        .chunks_exact(4)
                        .map(|frame| {
                            let mut digest = [0; 16];
                            digest[..8].copy_from_slice(&frame[2].to_le_bytes());
                            digest[8..].copy_from_slice(&frame[3].to_le_bytes());
                            Ok((u32::try_from(frame[0])?, u32::try_from(frame[1])?, digest))
                        })
                        .collect::<Result<_>>()?,
                }
            }
            (16..=21, _, _) | (22..=23, _, true) => Self::Memory {
                event: MemoryEvent::decode(kind, words)?,
                stack: bytes,
            },
            (END, [], true) => Self::End,
            (LOSS, &[lost, memory_emitted], true) => Self::Loss {
                lost,
                memory_emitted,
            },
            (END | LOSS, _, _) => return Err("invalid runtime observation footer".into()),
            (1..=14, _, _) => {
                return Err(format!("invalid runtime observation payload for kind {kind}").into());
            }
            _ => {
                return Err(
                    format!("unassigned runtime observation kind {kind} in HIRSOBS3").into(),
                );
            }
        };
        Ok(record)
    }
}

pub(crate) struct Record {
    pub ts: u64,
    pub tid: u32,
    pub kind: RecordKind,
}

pub(crate) struct Observation {
    pub records: Vec<Record>,
    pub lost: u64,
    pub memory_emitted: u64,
    pub complete: bool,
}

impl Observation {
    pub(crate) fn read(path: &Path) -> Result<Self> {
        let mut file = BufReader::new(File::open(path)?);
        let mut magic = [0; 8];
        file.read_exact(&mut magic)?;
        if magic != MAGIC {
            return Err("invalid runtime observation header".into());
        }
        let mut trace = Self {
            records: Vec::new(),
            lost: 0,
            memory_emitted: 0,
            complete: false,
        };
        let mut header = [0u8; HEADER_BYTES];
        loop {
            if !read_record_part(&mut file, &mut header)? {
                break;
            }
            let ts = u64::from_le_bytes(header[..8].try_into()?);
            let tid = u32::from_le_bytes(header[8..12].try_into()?);
            let kind = u16::from_le_bytes(header[12..14].try_into()?);
            let words = usize::from(u16::from_le_bytes(header[14..16].try_into()?));
            let bytes = u32::from_le_bytes(header[16..20].try_into()?) as usize;
            if (0..12).contains(&kind) && bytes != 0 {
                return Err("invalid runtime observation record length".into());
            }
            let word_bytes = words * 8;
            let size =
                payload_bytes(words, bytes).ok_or("invalid runtime observation record length")?;
            let mut data = vec![0; size];
            if !read_record_part(&mut file, &mut data)? {
                break;
            }
            let bytes = if bytes == 0 {
                Vec::new()
            } else {
                data.split_off(word_bytes)
            };
            let words = data
                .chunks_exact(8)
                .map(|word| u64::from_le_bytes(word.try_into().unwrap()))
                .collect::<Vec<_>>();
            match RecordKind::decode(kind, &words, bytes)? {
                RecordKind::End => {
                    if file.read(&mut [0])? != 0 {
                        return Err("data after runtime observation end".into());
                    }
                    trace.complete = true;
                    break;
                }
                RecordKind::Loss {
                    lost,
                    memory_emitted,
                } => {
                    trace.lost = trace
                        .lost
                        .checked_add(lost)
                        .ok_or("runtime loss count overflows")?;
                    trace.memory_emitted = trace
                        .memory_emitted
                        .checked_add(memory_emitted)
                        .ok_or("runtime memory record count overflows")?;
                }
                kind => trace.records.push(Record { ts, tid, kind }),
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
            import.record(record)?;
        }
        for (token, timing) in std::mem::take(&mut import.io) {
            import.io_timing(token, timing)?;
        }
        for (key, value) in [
            (
                "runtime_events",
                self.records
                    .iter()
                    .filter(|record| {
                        !matches!(
                            record.kind,
                            RecordKind::Mapping(_)
                                | RecordKind::CpuSample { .. }
                                | RecordKind::CodeFrames { .. }
                        )
                    })
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
    dispatches: HashMap<(Task, u32, i64), i64>,
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

    fn close_slice(&mut self, id: i64, end: Timestamp) -> Result<()> {
        self.db
            .execute("UPDATE task_slice SET end_ns=?2 WHERE id=?1", (id, end))?;
        Ok(())
    }

    fn record(&mut self, record: &Record) -> Result<()> {
        let time = Timestamp(record.ts);
        let tid = record.tid;
        match record.kind {
            RecordKind::Task { task, parent } => {
                let id = self.task(task)?;
                let parent = self.task(parent)?;
                self.db.execute(
                    "UPDATE task SET start_ns=?2,parent_id=?3 WHERE id=?1",
                    (id, time, parent),
                )?;
            }
            RecordKind::State {
                task: key,
                state,
                reason,
                object,
            } => {
                if let Some(previous) = self.states.remove(&key) {
                    self.close_slice(previous, time)?;
                }
                let id = self.task(key)?;
                if state == "ended" {
                    self.db
                        .execute("UPDATE task SET end_ns=?2 WHERE id=?1", (id, time))?;
                } else if state != "running" {
                    let object = (object != 0).then_some(Unsigned(object));
                    self.db.execute(
                        "INSERT INTO task_slice(task_id,tid,state,wait_reason,object_id,start_ns)
                         VALUES (?1,?2,?3,?4,?5,?6)",
                        (id, tid, state, reason, object, time),
                    )?;
                    self.states.insert(key, self.db.last_insert_rowid());
                }
            }
            RecordKind::DispatchBegin { task: key, worker } => {
                if let Some(previous) = self.states.remove(&key) {
                    self.close_slice(previous, time)?;
                }
                let id = self.task(key)?;
                self.db.execute(
                    "INSERT INTO task_slice(task_id,tid,worker_id,state,start_ns)
                     VALUES (?1,?2,?3,'running',?4)",
                    (id, tid, worker, time),
                )?;
                if self
                    .dispatches
                    .insert((key, tid, worker), self.db.last_insert_rowid())
                    .is_some()
                {
                    self.unpaired += 1;
                }
            }
            RecordKind::DispatchEnd { task, worker } => {
                match self.dispatches.remove(&(task, tid, worker)) {
                    Some(id) => self.close_slice(id, time)?,
                    None => self.unpaired += 1,
                }
            }
            RecordKind::Wake {
                task,
                source,
                from,
                object,
            } => {
                let from = self.task(from)?;
                let to = self.task(task)?;
                let object = (object != 0).then_some(Unsigned(object));
                self.db.execute(
                    "INSERT INTO causal_edge(src_kind,src_id,dst_task_id,object_id,ts_ns)
                     VALUES (?1,?2,?3,?4,?5)",
                    (source, from, to, object, time),
                )?;
            }
            RecordKind::HostBegin { task: key, site } => {
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
            RecordKind::HostEnd { task, site } => match self.host.remove(&tid) {
                Some((id, key, at)) if key == task && at == site => {
                    self.db.execute(
                        "UPDATE host_stack_interval SET end_ns=?2 WHERE id=?1",
                        (id, time),
                    )?;
                }
                _ => self.unpaired += 1,
            },
            RecordKind::IoBegin {
                token,
                task,
                operation,
                site,
            } => {
                let id = self.task(task)?;
                self.db.execute(
                    "INSERT INTO io_op(token,task_id,operation,site,start_ns) VALUES (?1,?2,?3,?4,?5)",
                    (Unsigned(token), id, operation, Unsigned(site), time),
                )?;
                self.io.insert(token, Timing::default());
            }
            RecordKind::IoCallback {
                token,
                phase,
                elapsed,
            } => {
                if let Some(timing) = self.io.get_mut(&token) {
                    let phase = &mut timing.callbacks[phase];
                    *phase = phase
                        .checked_add(elapsed)
                        .ok_or("callback time overflows")?;
                } else {
                    self.unpaired += 1;
                }
            }
            RecordKind::IoPending { token, elapsed } => {
                if let Some(timing) = self.io.get_mut(&token) {
                    timing.pending = timing
                        .pending
                        .checked_add(elapsed)
                        .ok_or("pending time overflows")?;
                } else {
                    self.unpaired += 1;
                }
            }
            RecordKind::IoEnd {
                token,
                bytes,
                tag,
                code,
                status,
            } => {
                if let Some(timing) = self.io.remove(&token) {
                    self.io_timing(token, timing)?;
                    self.db.execute(
                        "UPDATE io_op SET end_ns=?2,bytes=?3,error_tag=?4,error_code=?5,status=?6 WHERE token=?1",
                        (Unsigned(token), time, Unsigned(bytes), tag, code, status),
                    )?;
                } else {
                    self.unpaired += 1;
                }
            }
            RecordKind::Mapping(_)
            | RecordKind::CpuSample { .. }
            | RecordKind::CodeFrames { .. }
            | RecordKind::Memory { .. }
            | RecordKind::End
            | RecordKind::Loss { .. } => {}
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

#[cfg(test)]
mod tests {
    use super::*;
    use hirsel_observation_format::{PAYLOAD_BYTES, RECORD_BYTES};

    #[test]
    fn record_limit_round_trips_and_refuses_overflow() {
        let dir = tempfile::tempdir().unwrap();
        let exact_payload = vec![0x5a; PAYLOAD_BYTES];
        let exact_path = dir.path().join("exact.observations");
        std::fs::write(
            &exact_path,
            hirsel_core_rt::prof_testing::mapping_stream(RECORD_BYTES * 2, &[&exact_payload]),
        )
        .unwrap();
        let exact = Observation::read(&exact_path).unwrap();
        assert!(exact.complete);
        assert_eq!(exact.records.len(), 1);
        match &exact.records[0].kind {
            RecordKind::Mapping(bytes) => assert_eq!(bytes, &exact_payload),
            _ => panic!("the exact record is its mapping payload"),
        }

        let over_payload = vec![0x5a; PAYLOAD_BYTES + 1];
        let over_path = dir.path().join("over.observations");
        std::fs::write(
            &over_path,
            hirsel_core_rt::prof_testing::mapping_stream(RECORD_BYTES * 2, &[&over_payload]),
        )
        .unwrap();
        let over = Observation::read(&over_path).unwrap();
        assert!(over.complete);
        assert!(over.records.is_empty());
    }
}
