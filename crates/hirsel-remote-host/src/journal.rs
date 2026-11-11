//! The host's call journal seam (RUN-11 reconciliation, RUN-13). A journal that is durable
//! records each call key before its callee may run and each decision before its outcome is
//! sent, so a restarted host still knows every call it ever decided: its status answers
//! prove a call never ran, and a call admitted but never decided is `InDoubt(Abandoned())`.
//!
//! - `journal.none@1` (the default): nothing survives the process; a restarted host is a
//!   new incarnation that callers learn of when they reconnect.
//! - `journal.dir@1` (`{"path"}`): an append-only [`frame`] file of sequenced, checksummed
//!   records, synced before each record is acknowledged.
//! - `journal.sim@1`: the same bytes in memory shared with the test, which scripts failed
//!   and partial appends.
//!
//! **Recovery** (determinism.md, Frames and recovery) runs when a durable journal is opened,
//! through the shared frame classifier. Only an incomplete final frame (a short header, or a
//! valid header whose body runs past the end of the file) is a torn tail and is cut; it was
//! never durable, so nothing was acted on from it. Any other damage, including a complete
//! last record that fails a checksum, breaks the sequence or does not decode, refuses
//! recovery as `Corrupt` and leaves the file as it is. A file header that is itself short
//! was never durable: the journal is created again.
//!
//! **Poisoning.** An append that fails, with an error or after writing part of its record,
//! poisons the journal: every later append fails, so nothing follows a torn record and a
//! host never acts on a record it failed to write.
//!
//! **Generations and compaction.** The file header's generation counts the journal's
//! compactions. Generation 0 is the file as created; its frames are records. Every later
//! generation starts with a checkpoint record (frame 0) holding its base, the records all
//! earlier generations held (so a record's journal-wide sequence is the base plus its
//! frame's), and the count of records that follow it as the checkpoint: the records that
//! recover the host's state when it compacted. Appends follow them. A journal is due for
//! compaction once this generation holds twice its checkpoint, and at least
//! [`COMPACT_AFTER`] records. Compaction writes the next generation whole to
//! `calls.journal.next`, syncs it, renames it over `calls.journal` and syncs the directory
//! before anything is appended to it, so a crash leaves one whole generation: the old one,
//! or the new one. Opening removes a leftover `calls.journal.next`, which was never
//! published. A failed compaction poisons the journal as a failed append does.
use hirsel_run_schema::CallOutcome;
use hirsel_run_schema::frame::{self, FileError, Format, Opening};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};

/// A stream's identity at a host: the caller's name and the stream bytes it opened.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct StreamKey {
    pub caller: String,
    pub stream: [u8; 32],
}

/// One journal record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Entry {
    /// A stream's first session: the caller run instance it belongs to.
    Stream {
        stream: StreamKey,
        instance: [u8; 16],
    },
    /// A request at `position` passed every check and may run.
    Admitted {
        stream: StreamKey,
        position: u64,
        key: [u8; 32],
        digest: [u8; 32],
    },
    /// The position's decision.
    Decided {
        stream: StreamKey,
        position: u64,
        key: [u8; 32],
        digest: [u8; 32],
        end: CallOutcome,
        callee: Option<[u8; 16]>,
    },
    /// The caller holds every outcome up to `position`.
    Acknowledged { stream: StreamKey, position: u64 },
}

/// A host's call journal. Opening a durable journal recovers it.
pub trait Journal: Send {
    /// Whether records survive a crash of the host process.
    fn durable(&self) -> bool;
    /// The host incarnation: fixed for a durable journal's whole life, fresh otherwise.
    fn incarnation(&self) -> [u8; 16];
    /// Append `entry`; when this returns Ok, a durable journal keeps it through a crash.
    /// An error poisons the journal: every later append fails.
    fn append(&mut self, entry: &Entry) -> io::Result<()>;
    /// Every record a previous incarnation of this journal kept, in order. The host takes
    /// them once, when it starts.
    fn recover(&mut self) -> io::Result<Vec<Entry>>;
    /// Whether the current generation has grown enough to compact.
    fn due(&self) -> bool;
    /// Publish the next generation, whose checkpoint is `live`: records whose recovery
    /// gives the host's present state. When this returns Ok, only the new generation
    /// survives a crash; an error poisons the journal.
    fn compact(&mut self, live: &[Entry]) -> io::Result<()>;
}

/// `journal.none@1`.
pub struct NoJournal {
    incarnation: [u8; 16],
}
impl NoJournal {
    pub fn new(incarnation: [u8; 16]) -> Self {
        Self { incarnation }
    }
}
impl Journal for NoJournal {
    fn durable(&self) -> bool {
        false
    }
    fn incarnation(&self) -> [u8; 16] {
        self.incarnation
    }
    fn append(&mut self, _: &Entry) -> io::Result<()> {
        Ok(())
    }
    fn recover(&mut self) -> io::Result<Vec<Entry>> {
        Ok(vec![])
    }
    fn due(&self) -> bool {
        false
    }
    fn compact(&mut self, _: &[Entry]) -> io::Result<()> {
        Ok(())
    }
}

/// The file format: its identity is the incarnation; its generation counts compactions.
pub const FORMAT: Format = Format {
    magic: b"HRJRNL04",
    identity: 16,
};
/// The kind of a generation's checkpoint record; every other kind is an entry's tag.
const CHECKPOINT: u8 = 4;
/// Record kinds: the entry tags and the checkpoint.
pub fn known(kind: u8) -> bool {
    kind <= CHECKPOINT
}
/// The fewest records a generation holds before it is due for compaction.
pub const COMPACT_AFTER: u64 = 4096;
const FILE: &str = "calls.journal";
const NEXT: &str = "calls.journal.next";

/// A durable journal's current generation: the records it recovered, and whether an
/// append has failed.
struct Log {
    recovered: Vec<Entry>,
    generation: u64,
    /// The records earlier generations held.
    base: u64,
    /// This generation's frames, its checkpoint's included: the next frame's sequence.
    next: u64,
    /// This generation's checkpoint frames (0 for generation 0).
    checkpoint: u64,
    compact_after: u64,
    poisoned: bool,
}
impl Log {
    /// Recover a journal file's `bytes`: its incarnation, its log, and the length of the
    /// valid prefix when an incomplete tail follows it; `None` when the file was never
    /// created durably (empty, or a short file header) and is to be created again.
    #[allow(clippy::type_complexity)]
    fn recover(
        bytes: &[u8],
        compact_after: u64,
    ) -> io::Result<Option<([u8; 16], Self, Option<usize>)>> {
        let corrupt = |detail: String| io::Error::new(io::ErrorKind::InvalidData, detail);
        let header = match FORMAT.open(bytes) {
            Ok(Opening::Short) => return Ok(None),
            Ok(Opening::Header(header)) => header,
            Err(FileError::Foreign) => {
                return Err(corrupt("not a hirsel call journal of this format".into()));
            }
            Err(FileError::Corrupt(e)) => {
                return Err(corrupt(format!("the call journal is {e}")));
            }
        };
        let frames = frame::recover(bytes, header.len, known)
            .map_err(|e| corrupt(format!("the call journal is {e}")))?;
        // A later generation was synced whole before it was published: its checkpoint is
        // complete, and is the only one.
        let (base, checkpoint) = match frames.frames.first() {
            _ if header.generation == 0 => (0, 0),
            Some(f) if f.kind == CHECKPOINT => {
                let mut c = Cursor(f.body);
                let (Some(base), Some(records), true) = (c.u64(), c.u64(), c.0.is_empty()) else {
                    return Err(corrupt(format!(
                        "the call journal is corrupt at byte {}: undecodable checkpoint",
                        f.range.start
                    )));
                };
                (base, records.saturating_add(1))
            }
            _ => {
                return Err(corrupt(format!(
                    "the call journal is corrupt: generation {} has no checkpoint",
                    header.generation
                )));
            }
        };
        if (frames.frames.len() as u64) < checkpoint {
            return Err(corrupt(
                "the call journal is corrupt: its checkpoint is incomplete".into(),
            ));
        }
        let skip = usize::from(checkpoint > 0);
        let recovered = frames.frames[skip..]
            .iter()
            .map(|f| {
                decode(f.kind, f.body).ok_or_else(|| {
                    corrupt(format!(
                        "the call journal is corrupt at byte {}: undecodable record",
                        f.range.start
                    ))
                })
            })
            .collect::<io::Result<Vec<_>>>()?;
        let log = Self {
            next: frames.frames.len() as u64,
            recovered,
            generation: header.generation,
            base,
            checkpoint,
            compact_after,
            poisoned: false,
        };
        let torn = (frames.valid < bytes.len()).then_some(frames.valid);
        Ok(Some((header.identity.try_into().unwrap(), log, torn)))
    }
    fn new(compact_after: u64) -> Self {
        Self {
            recovered: vec![],
            generation: 0,
            base: 0,
            next: 0,
            checkpoint: 0,
            compact_after,
            poisoned: false,
        }
    }
    fn due(&self) -> bool {
        !self.poisoned && self.next >= self.compact_after.max(2 * self.checkpoint)
    }
    /// Publish the next generation, checkpointing `live`, through `publish`, which returns
    /// once it is the journal's only generation; any failure poisons the log.
    fn compact<T>(
        &mut self,
        incarnation: &[u8; 16],
        live: &[Entry],
        publish: impl FnOnce(&[u8]) -> io::Result<T>,
    ) -> io::Result<T> {
        if self.poisoned {
            return Err(io::Error::other(
                "the call journal is poisoned by an earlier failed append",
            ));
        }
        let base = self.base + self.next;
        let mut file = FORMAT.header(incarnation, self.generation + 1);
        let mut body = base.to_le_bytes().to_vec();
        body.extend_from_slice(&(live.len() as u64).to_le_bytes());
        let framed = frame::append(&mut file, 0, CHECKPOINT, &body).and_then(|()| {
            live.iter().zip(1..).try_for_each(|(entry, seq)| {
                let (kind, body) = encode(entry);
                frame::append(&mut file, seq, kind, &body)
            })
        });
        let published = framed
            .map_err(io::Error::other)
            .and_then(|()| publish(&file));
        match published {
            Ok(published) => {
                self.generation += 1;
                self.base = base;
                self.checkpoint = live.len() as u64 + 1;
                self.next = self.checkpoint;
                Ok(published)
            }
            Err(error) => {
                self.poisoned = true;
                Err(error)
            }
        }
    }
    /// Write the next record through `write`; any failure poisons the log.
    fn append(
        &mut self,
        entry: &Entry,
        write: impl FnOnce(&[u8]) -> io::Result<()>,
    ) -> io::Result<()> {
        if self.poisoned {
            return Err(io::Error::other(
                "the call journal is poisoned by an earlier failed append",
            ));
        }
        let (kind, body) = encode(entry);
        let mut record = vec![];
        frame::append(&mut record, self.next, kind, &body).map_err(io::Error::other)?;
        match write(&record) {
            Ok(()) => {
                self.next += 1;
                Ok(())
            }
            Err(error) => {
                self.poisoned = true;
                Err(error)
            }
        }
    }
}

/// `journal.dir@1`: `<path>/calls.journal`, created with a random incarnation in its header.
pub struct DirJournal {
    dir: std::path::PathBuf,
    file: File,
    incarnation: [u8; 16],
    log: Log,
}
impl DirJournal {
    /// Open or create the journal in `dir`, whose incarnation is `fresh` when it is
    /// created, and recover it: a torn tail is cut, and earlier damage is `Corrupt`
    /// (`InvalidData`), leaving the file untouched.
    pub fn open(dir: &Path, fresh: [u8; 16]) -> io::Result<Self> {
        // An unpublished next generation: the published one is whole.
        match fs::remove_file(dir.join(NEXT)) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => (),
        }
        let path = dir.join(FILE);
        let mut file = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .open(&path)?;
        let mut bytes = vec![];
        file.read_to_end(&mut bytes)?;
        let (incarnation, log) = match Log::recover(&bytes, COMPACT_AFTER)? {
            None => {
                file.set_len(0)?;
                file.write_all(&FORMAT.header(&fresh, 0))?;
                file.sync_all()?;
                File::open(dir)?.sync_all()?;
                (fresh, Log::new(COMPACT_AFTER))
            }
            Some((incarnation, log, torn)) => {
                if let Some(valid) = torn {
                    // The torn tail was never acknowledged: appends follow the last valid record.
                    file.set_len(valid as u64)?;
                    file.sync_all()?;
                }
                (incarnation, log)
            }
        };
        Ok(Self {
            dir: dir.to_path_buf(),
            file,
            incarnation,
            log,
        })
    }
}
impl Journal for DirJournal {
    fn durable(&self) -> bool {
        true
    }
    fn incarnation(&self) -> [u8; 16] {
        self.incarnation
    }
    fn append(&mut self, entry: &Entry) -> io::Result<()> {
        let file = &mut self.file;
        self.log.append(entry, |frame| {
            file.write_all(frame)?;
            file.sync_data()
        })
    }
    fn recover(&mut self) -> io::Result<Vec<Entry>> {
        Ok(std::mem::take(&mut self.log.recovered))
    }
    fn due(&self) -> bool {
        self.log.due()
    }
    fn compact(&mut self, live: &[Entry]) -> io::Result<()> {
        let dir = &self.dir;
        self.file = self.log.compact(&self.incarnation, live, |bytes| {
            let next = dir.join(NEXT);
            let mut file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&next)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            fs::rename(&next, dir.join(FILE))?;
            File::open(dir)?.sync_all()?;
            // Appends follow the checkpoint, at the end of the written file.
            Ok(file)
        })?;
        Ok(())
    }
}

/// `journal.sim@1`: the journal's bytes in memory, shared with the test that owns
/// [`SimDisk`].
pub struct SimJournal {
    disk: SimDisk,
    incarnation: [u8; 16],
    log: Log,
}
/// The simulated medium: the journal file's bytes, all synced, and the test's scripted
/// append and compaction failures. A next generation that was never published is
/// dropped, as opening `journal.dir@1` removes it.
#[derive(Clone, Default)]
pub struct SimDisk(Arc<Mutex<SimMedium>>);
#[derive(Default)]
struct SimMedium {
    bytes: Vec<u8>,
    /// Appends and compactions attempted on this medium, by every journal over it.
    appends: u64,
    /// The fewest records before compaction; [`COMPACT_AFTER`] when unset.
    compact_after: Option<u64>,
    /// Append attempt -> how many of its record's bytes reach the medium before it fails.
    faults: std::collections::BTreeMap<u64, usize>,
}
impl SimDisk {
    /// Open a journal over this medium, recovering it as `journal.dir@1` does; the first
    /// opener fixes its incarnation.
    pub fn journal(&self, fresh: [u8; 16]) -> io::Result<SimJournal> {
        let mut medium = self.0.lock().unwrap();
        let compact_after = medium.compact_after.unwrap_or(COMPACT_AFTER);
        let (incarnation, log) = match Log::recover(&medium.bytes, compact_after)? {
            None => {
                medium.bytes = FORMAT.header(&fresh, 0);
                (fresh, Log::new(compact_after))
            }
            Some((incarnation, log, torn)) => {
                if let Some(valid) = torn {
                    medium.bytes.truncate(valid);
                }
                (incarnation, log)
            }
        };
        Ok(SimJournal {
            disk: self.clone(),
            incarnation,
            log,
        })
    }
    /// Make append or compaction attempt `attempt` (counted from 0 over this medium's life)
    /// fail after writing the first `written` bytes of its record or next generation: 0
    /// writes nothing, and a count past the length writes all of it and still fails, as a
    /// failed sync does. A compaction that writes all of its generation fails before it
    /// publishes it, and one given a count past its length publishes it and then fails, as
    /// a failed directory sync does.
    pub fn fail_append(&self, attempt: u64, written: usize) {
        self.0.lock().unwrap().faults.insert(attempt, written);
    }
    /// Journals opened from now on compact once a generation holds `records` records.
    pub fn compact_after(&self, records: u64) {
        self.0.lock().unwrap().compact_after = Some(records);
    }
    /// Append attempts made on this medium so far.
    pub fn appends(&self) -> u64 {
        self.0.lock().unwrap().appends
    }
    /// The medium's raw file bytes.
    pub fn bytes(&self) -> Vec<u8> {
        self.0.lock().unwrap().bytes.clone()
    }
}
impl Journal for SimJournal {
    fn durable(&self) -> bool {
        true
    }
    fn incarnation(&self) -> [u8; 16] {
        self.incarnation
    }
    fn append(&mut self, entry: &Entry) -> io::Result<()> {
        let disk = &self.disk;
        self.log.append(entry, |frame| {
            let mut medium = disk.0.lock().unwrap();
            let attempt = medium.appends;
            medium.appends += 1;
            match medium.faults.remove(&attempt) {
                None => {
                    medium.bytes.extend_from_slice(frame);
                    Ok(())
                }
                Some(written) => {
                    medium
                        .bytes
                        .extend_from_slice(&frame[..written.min(frame.len())]);
                    Err(io::Error::other("simulated append failure"))
                }
            }
        })
    }
    fn recover(&mut self) -> io::Result<Vec<Entry>> {
        Ok(std::mem::take(&mut self.log.recovered))
    }
    fn due(&self) -> bool {
        self.log.due()
    }
    fn compact(&mut self, live: &[Entry]) -> io::Result<()> {
        let disk = &self.disk;
        self.log.compact(&self.incarnation, live, |file| {
            let mut medium = disk.0.lock().unwrap();
            let attempt = medium.appends;
            medium.appends += 1;
            match medium.faults.remove(&attempt) {
                None => {
                    medium.bytes = file.to_vec();
                    Ok(())
                }
                Some(written) if written <= file.len() => {
                    Err(io::Error::other("simulated compaction failure"))
                }
                Some(_) => {
                    medium.bytes = file.to_vec();
                    Err(io::Error::other("simulated directory sync failure"))
                }
            }
        })
    }
}

fn put_stream(out: &mut Vec<u8>, stream: &StreamKey) {
    put_bytes(out, stream.caller.as_bytes());
    out.extend_from_slice(&stream.stream);
}
fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    out.extend_from_slice(bytes);
}
fn put_end(out: &mut Vec<u8>, end: &CallOutcome) {
    let (tag, payload): (u8, Vec<u8>) = match end {
        CallOutcome::Ok(b) => (0, b.clone()),
        CallOutcome::Failed(b) => (1, b.clone()),
        CallOutcome::Faulted(b) => (2, b.clone()),
        CallOutcome::NotRun(r) => (3, r.to_le_bytes().to_vec()),
        CallOutcome::InDoubt(r) => (4, r.to_le_bytes().to_vec()),
        CallOutcome::Canceled(c) => (5, c.to_le_bytes().to_vec()),
    };
    out.push(tag);
    put_bytes(out, &payload);
}
/// A record's kind (the entry's tag) and body: fixed-width or length-framed fields.
fn encode(entry: &Entry) -> (u8, Vec<u8>) {
    let mut out = vec![];
    let kind = match entry {
        Entry::Stream { stream, instance } => {
            put_stream(&mut out, stream);
            out.extend_from_slice(instance);
            0
        }
        Entry::Admitted {
            stream,
            position,
            key,
            digest,
        } => {
            put_stream(&mut out, stream);
            out.extend_from_slice(&position.to_le_bytes());
            out.extend_from_slice(key);
            out.extend_from_slice(digest);
            1
        }
        Entry::Decided {
            stream,
            position,
            key,
            digest,
            end,
            callee,
        } => {
            put_stream(&mut out, stream);
            out.extend_from_slice(&position.to_le_bytes());
            out.extend_from_slice(key);
            out.extend_from_slice(digest);
            put_end(&mut out, end);
            match callee {
                None => out.push(0),
                Some(c) => {
                    out.push(1);
                    out.extend_from_slice(c);
                }
            }
            2
        }
        Entry::Acknowledged { stream, position } => {
            put_stream(&mut out, stream);
            out.extend_from_slice(&position.to_le_bytes());
            3
        }
    };
    (kind, out)
}

struct Cursor<'a>(&'a [u8]);
impl Cursor<'_> {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        if self.0.len() < n {
            return None;
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Some(head)
    }
    fn fixed<const N: usize>(&mut self) -> Option<[u8; N]> {
        self.take(N)?.try_into().ok()
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.fixed()?))
    }
    fn bytes(&mut self) -> Option<Vec<u8>> {
        let len = usize::try_from(self.u64()?).ok()?;
        Some(self.take(len)?.to_vec())
    }
    fn stream(&mut self) -> Option<StreamKey> {
        Some(StreamKey {
            caller: String::from_utf8(self.bytes()?).ok()?,
            stream: self.fixed()?,
        })
    }
    fn end(&mut self) -> Option<CallOutcome> {
        let tag = self.fixed::<1>()?[0];
        let payload = self.bytes()?;
        let small = || Some(u32::from_le_bytes(payload.as_slice().try_into().ok()?));
        Some(match tag {
            0 => CallOutcome::Ok(payload),
            1 => CallOutcome::Failed(payload),
            2 => CallOutcome::Faulted(payload),
            3 => CallOutcome::NotRun(small()?),
            4 => CallOutcome::InDoubt(small()?),
            5 => CallOutcome::Canceled(small()?),
            _ => return None,
        })
    }
}
fn decode(kind: u8, body: &[u8]) -> Option<Entry> {
    let mut c = Cursor(body);
    let entry = match kind {
        0 => Entry::Stream {
            stream: c.stream()?,
            instance: c.fixed()?,
        },
        1 => Entry::Admitted {
            stream: c.stream()?,
            position: c.u64()?,
            key: c.fixed()?,
            digest: c.fixed()?,
        },
        2 => Entry::Decided {
            stream: c.stream()?,
            position: c.u64()?,
            key: c.fixed()?,
            digest: c.fixed()?,
            end: c.end()?,
            callee: match c.fixed::<1>()?[0] {
                0 => None,
                1 => Some(c.fixed()?),
                _ => return None,
            },
        },
        3 => Entry::Acknowledged {
            stream: c.stream()?,
            position: c.u64()?,
        },
        _ => return None,
    };
    c.0.is_empty().then_some(entry)
}
