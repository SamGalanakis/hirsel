//! Tapes: Record and Replay of provider operations (REPLAY-SPEC v2.4 S1/S2).
//!
//! A tape is a provider over the four-state ABI (section 3). `on_tape` turns a
//! capability's (provider, handle) into a substitute: the tape's provider with
//! a handle `(generation << 32) | ordinal`, which the run registers as a
//! dynamic grant like any other (host_file.rs). Every operation on it, and on
//! every HostFile or Dir made through it, reaches the tape at issue.
//!
//! Record forwards operations and records their issue and production. S2 also
//! records waiting, external cancellation and non-FIFO scheduling at semantic
//! event coordinates. Replay has no inner provider and supplies those inputs
//! at scheduler turns; malformed pairings and positions give Schedule.
//!
//! The tape's state lives in a process slot table (section 8). Ending or
//! dropping a tape advances its cell's generation, so every handle made
//! through it then fails before issue and never uses its source.
pub mod abi;
pub mod run;
pub use hirsel_tape_format as format;
pub(crate) mod schedule;
#[cfg(test)]
mod schedule_cases;
#[cfg(test)]
mod tests;

use crate::provider::*;
use format::*;
use std::{
    collections::{BTreeMap, VecDeque},
    ptr,
    sync::Mutex,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    Record,
    Replay,
}

/// Section 4.2's divergence kinds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mismatch {
    Op,
    Object,
    Arg,
    Missing,
    Extra,
    /// The replaying domain's outcome differs from END's (v2.3 R9).
    Outcome,
    Schedule,
}

/// Where a replay diverged: `at` is the 1-based position of the frame it was
/// checked against, counting the frames after block 0 (one past the last frame
/// when the tape has none left).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Divergence {
    pub at: u64,
    pub kind: Mismatch,
}

/// Why a tape stopped. Every later operation on it fails the same way.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Fault {
    Diverged(Divergence),
    /// The tape is cut at the frame in this position.
    Truncated(u64),
    Damaged(u64),
    Corrupt,
    /// The sink's write failed (S1 has no barriers; S3 poisons instead).
    Io(IoError),
    /// The tape was ended or dropped (section 8).
    Ended,
}
impl Fault {
    /// The fault a block or frame stop gives at frame position `at`.
    fn stopped(s: Stop, at: u64) -> Self {
        match s {
            Stop::Missing => Fault::Diverged(Divergence {
                at,
                kind: Mismatch::Missing,
            }),
            Stop::Truncated => Fault::Truncated(at),
            Stop::Damaged(k) => Fault::Damaged(k),
            Stop::Corrupt => Fault::Corrupt,
        }
    }
}

impl From<Corrupt> for Fault {
    fn from(_: Corrupt) -> Self {
        Fault::Corrupt
    }
}

/// Section 5.3: open refuses before anything runs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Refusal {
    Code,
    Revision,
    Arg,
    Limit,
    Damaged(u64),
    HeaderTooLarge,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpenError {
    Refused(Refusal),
    /// The tape file could not be created, read or written.
    Io(IoError),
    /// The header block itself is cut or breaks the grammar.
    Unreadable(Fault),
}

/// What a Replay opener compares with the stored header.
pub struct Expect<'a> {
    pub identity: &'a [u8],
    pub charge_policy: u32,
    pub limit: u64,
    /// None: the opener receives the stored argument.
    pub arg: Option<&'a [u8]>,
    /// Open a tape of another revision (REPLAY-SPEC v2.3's "across" open,
    /// Replay only): it runs, and the tape says its revision differs.
    pub across: bool,
}

/// An operation that the tape refused: the task fails before issue with cause
/// `Tape` (the runtime records it, `STATUS_REFUSED`).
pub const REFUSED: Outcome = Outcome {
    value: 0,
    error: IoError::OK,
    written: 0,
    status: STATUS_REFUSED,
};

thread_local! {
    /// The reason of the latest refusal on this thread (a run's tasks share
    /// one): the printed `Tape` cause renders it (D-B4).
    static LAST: std::cell::Cell<Option<Fault>> = const { std::cell::Cell::new(None) };
}
/// Refuse with `f`, keeping it as the reason a printed `Tape` cause names.
pub(crate) fn refuse(f: Fault) -> Fault {
    LAST.set(Some(f));
    f
}
/// The reason of the latest refusal on this thread.
pub(crate) fn last_fault() -> Option<Fault> {
    LAST.get()
}
impl std::fmt::Display for Fault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Fault::Diverged(d) => write!(f, "diverged at {}, {:?}", d.at, d.kind),
            Fault::Truncated(at) => write!(f, "truncated at {at}"),
            Fault::Damaged(k) => write!(f, "block {k} damaged"),
            Fault::Corrupt => f.write_str("corrupt"),
            Fault::Io(e) => write!(f, "tape write failed ({}, {})", e.tag, e.code),
            Fault::Ended => f.write_str("ended"),
        }
    }
}

/// The inner (provider, provider-local handle) an object designates.
#[derive(Clone, Copy)]
struct Inner {
    provider: Provider,
    handle: u64,
}
// SAFETY: providers are run-pinned; the tape calls them only from the run.
unsafe impl Send for Inner {}

struct Object {
    /// Record: the source. Replay: none, so nothing can reach the host.
    inner: Option<Inner>,
    /// A HostFile still open: closed by the tape's end, with no frame.
    open_file: bool,
}

struct Recorder {
    sink: ProviderFile,
    tape_id: [u8; 16],
    encoder: Encoder,
    buffer: Vec<u8>,
    seq: u64,
}

struct Replayer {
    source: ProviderFile,
    blocks: Blocks,
    decoder: Decoder,
    frames: VecDeque<Frame>,
    /// Frames taken so far: the position the next divergence names.
    taken: u64,
}

struct Live {
    schedule: schedule::Schedule,
    waiting: BTreeMap<usize, u64>,
    pending: BTreeMap<usize, Pending>,
    mode: Mode,
    fault: Option<Fault>,
    /// Index ordinal - 1 (section 4.3: one sequence per tape, from 1).
    objects: Vec<Object>,
    /// The previous value of each clock op on each object (section 5.4).
    clocks: BTreeMap<(u8, u64), u64>,
    recorder: Option<Recorder>,
    replayer: Option<Replayer>,
}

/// A Record operation between issue and deliver: deliver runs the inner
/// provider's own deliver on its own outcome, then maps a new handle.
struct Pending {
    inner: Inner,
    task: u64,
    before: usize,
}

struct Delivery {
    inner: Inner,
    outcome: Outcome,
    created: Option<u64>,
}

struct State {
    generation: u32,
    live: Option<Live>,
    /// Record's HostFiles still open when their tape ended, by substitute
    /// handle: their cleanup closes the inner file with no frame.
    orphans: BTreeMap<u64, Inner>,
    /// By request address: a request is pinned through its delivery.
    delivering: BTreeMap<usize, Delivery>,
}

/// One slot of the table. Cells are never freed; a reused cell has a new
/// generation, so a handle from its earlier tape stays ended.
pub struct Cell {
    state: Mutex<State>,
    provider: Provider,
}

// SAFETY: the cell's mutable state is behind its mutex; the provider's state
// pointer is the cell itself and its callbacks are plain functions.
unsafe impl Sync for Cell {}
unsafe impl Send for Cell {}

thread_local! {
    /// Ended cells, reused by the next open on this thread (a run's tasks
    /// share one thread).
    static FREE: std::cell::RefCell<Vec<&'static Cell>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn cell() -> &'static Cell {
    if let Some(c) = FREE.with_borrow_mut(Vec::pop) {
        return c;
    }
    let c: &'static mut Cell = Box::leak(Box::new(Cell {
        state: Mutex::new(State {
            generation: 0,
            live: None,
            orphans: BTreeMap::new(),
            delivering: BTreeMap::new(),
        }),
        provider: Provider {
            state: ptr::null_mut(),
            wait,
            issue,
            resume,
            abort,
            deliver,
        },
    }));
    c.provider.state = (c as *mut Cell).cast();
    c
}

/// An open tape. Dropping it without `end` writes its buffered frames and
/// nothing else (section 8: equivalent to a crash at that point).
pub struct Tape {
    cell: &'static Cell,
    generation: u32,
    /// An across open of a tape whose revision is not the opener's.
    differs: bool,
}

fn handle(generation: u32, ordinal: usize) -> u64 {
    (u64::from(generation) << 32) | ordinal as u64
}
fn empty_buffer() -> Buffer {
    Buffer {
        data: ptr::null_mut(),
        len: 0,
        capacity: 0,
    }
}
fn request(operation: u8, mode: u8, handle: u64, path: &[u8], buffer: Buffer) -> Request {
    Request {
        operation,
        mode,
        handle,
        path: path.as_ptr(),
        path_len: path.len(),
        buffer,
        site: 0,
    }
}
/// The recorder's own I/O through the Dir's provider (section 6.1): never
/// recorded. It runs in the opener's task; a provider that does not answer
/// at once is refused as an I/O failure.
unsafe fn sink(p: Inner, mut r: Request) -> Outcome {
    r.handle = p.handle;
    // SAFETY: the provider is the run's; the request's operands are pinned.
    unsafe {
        let w = (p.provider.wait)(p.provider.state, &mut r, 0);
        if w.kind != WAIT_READY {
            return Outcome {
                error: IoError { tag: 5, code: 0 },
                ..Outcome::default()
            };
        }
        let step = (p.provider.issue)(p.provider.state, &mut r, 0);
        if step.kind != STEP_DONE {
            crate::process::stop(b"TapePending");
        }
        (p.provider.deliver)(p.provider.state, &mut r, step.outcome)
    }
}
fn failed(o: &Outcome) -> Option<IoError> {
    if o.status != 0 {
        Some(IoError { tag: 5, code: 0 })
    } else if o.error != IoError::OK {
        Some(o.error)
    } else {
        None
    }
}
/// Owns a file from successful OPEN until CLOSE. The provider must remain
/// pinned in its run for the owner's lifetime.
struct ProviderFile(Option<Inner>);

impl ProviderFile {
    unsafe fn open(dir: Inner, mode: u8, name: &[u8]) -> Result<Self, IoError> {
        // SAFETY: the opener owns the pinned provider.
        let o = unsafe { sink(dir, request(OPEN, mode, 0, name, empty_buffer())) };
        match failed(&o) {
            Some(e) => Err(e),
            None => Ok(Self(Some(Inner {
                provider: dir.provider,
                handle: o.value,
            }))),
        }
    }

    fn write(&self, bytes: &[u8]) -> Result<(), IoError> {
        let mut r = request(
            WRITE,
            0,
            0,
            &[],
            Buffer {
                data: bytes.as_ptr().cast_mut(),
                len: bytes.len(),
                capacity: 0,
            },
        );
        r.path = ptr::null();
        // SAFETY: the file and the borrowed input are pinned through delivery.
        let o = unsafe { sink(self.0.unwrap(), r) };
        failed(&o).map_or(Ok(()), Err)
    }

    fn read(&self, bytes: &mut [u8]) -> Result<usize, IoError> {
        let mut r = request(
            READ,
            0,
            0,
            &[],
            Buffer {
                data: bytes.as_mut_ptr(),
                len: 0,
                capacity: bytes.len(),
            },
        );
        r.path = ptr::null();
        // SAFETY: the file and the writable buffer are pinned through delivery.
        let o = unsafe { sink(self.0.unwrap(), r) };
        failed(&o).map_or(Ok(o.value as usize), Err)
    }

    fn close(&mut self) -> Result<(), IoError> {
        let Some(file) = self.0.take() else {
            return Ok(());
        };
        // SAFETY: the file remains pinned through delivery.
        let o = unsafe { sink(file, request(CLOSE, 0, 0, &[], empty_buffer())) };
        failed(&o).map_or(Ok(()), Err)
    }
}
impl Drop for ProviderFile {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

impl Recorder {
    /// Append one frame and its DATA continuations, closing the block at
    /// 64 KiB between frames, never inside one.
    fn frame(&mut self, f: &Frame) -> Result<(), IoError> {
        let (head, rest) = self.encoder.frame(f);
        self.buffer.extend_from_slice(&head);
        for d in rest {
            if self.buffer.len() >= CLOSE_AT {
                self.close(0)?;
            }
            self.buffer.extend_from_slice(&d);
        }
        if self.buffer.len() >= CLOSE_AT {
            self.close(0)?;
        }
        Ok(())
    }
    fn close(&mut self, flags: u8) -> Result<(), IoError> {
        let b = block(&self.tape_id, flags, self.seq, &self.buffer);
        self.seq += 1;
        self.buffer.clear();
        self.encoder.new_block();
        self.sink.write(&b)
    }
}

impl Replayer {
    fn peek(&mut self) -> Result<&Frame, Fault> {
        if self.frames.is_empty() {
            let frame = self.next()?;
            self.taken -= 1;
            self.frames.push_front(frame);
        }
        Ok(self.frames.front().unwrap())
    }
    fn next(&mut self) -> Result<Frame, Fault> {
        loop {
            if let Some(f) = self.frames.pop_front() {
                self.taken += 1;
                return Ok(f);
            }
            let at = self.taken + 1;
            match self.blocks.next(|bytes| self.source.read(bytes)) {
                Err(ReadError::Format(s)) => return Err(Fault::stopped(s, at)),
                Err(ReadError::Source(e)) => return Err(Fault::Io(e)),
                Ok(None) => return Err(self.diverged_next(Mismatch::Extra)),
                Ok(Some((flags, payload))) => {
                    let frames = self.decoder.block(payload, flags & END != 0)?;
                    self.frames.extend(frames);
                }
            }
        }
    }
    /// A divergence at the frame `next` returned last.
    fn diverged(&self, kind: Mismatch) -> Fault {
        Fault::Diverged(Divergence {
            at: self.taken,
            kind,
        })
    }
    /// A divergence at the frame after the last one taken.
    fn diverged_next(&self, kind: Mismatch) -> Fault {
        Fault::Diverged(Divergence {
            at: self.taken + 1,
            kind,
        })
    }
}

/// The canonical argument bytes of an operation (REPLAY-SPEC 4.1; the
/// per-operation choice is recorded in followups.md for section 5.4).
unsafe fn canonical(r: &Request) -> Vec<u8> {
    // SAFETY: the request's operands are pinned for its issue.
    unsafe {
        let path = if r.path_len == 0 {
            &[][..]
        } else {
            std::slice::from_raw_parts(r.path, r.path_len)
        };
        let bytes = if r.buffer.len == 0 {
            &[][..]
        } else {
            std::slice::from_raw_parts(r.buffer.data, r.buffer.len)
        };
        match r.operation {
            OPEN => {
                let mut a = vec![r.mode];
                a.extend_from_slice(path);
                a
            }
            READ | RANDOM => ((r.buffer.capacity - r.buffer.len) as u64)
                .to_le_bytes()
                .to_vec(),
            WRITE | STDOUT | STDERR | MKDTEMP => bytes.to_vec(),
            MKDIR | REMOVE | REMOVE_TREE | RENAME => path.to_vec(),
            SLEEP_UNTIL => bytes.to_vec(),
            _ => vec![],
        }
    }
}

fn clock(op: u8) -> bool {
    matches!(op, WALL_MS | MONO_NS)
}

impl Tape {
    pub(crate) fn group(&self) -> schedule::Group {
        schedule::Group::root(self.cell, self.generation)
    }
    /// Open a tape at `name` in the Dir `dir` (its provider and handle), as
    /// the code whose revision is `revision`: the generated `tape.open` passes
    /// the entry object's `hirsel_code_revision`, the entry's code revision,
    /// so a program that never opens a tape never references it.
    /// Record creates the file (CreateNew) and writes block 0 from `header`;
    /// Replay reads it and refuses, in this order, a changed identity,
    /// revision (unless `expect.across`), argument (when `expect` passes
    /// one), or limit or charge policy. `header.revision` is not the
    /// caller's: open fills it from `revision`.
    /// Returns the tape and the stored argument.
    /// # Safety
    /// Called in a task of the run that owns `dir`'s provider.
    pub unsafe fn open(
        revision: [u8; 32],
        dir: (Provider, u64),
        name: &[u8],
        mode: Mode,
        header: &Header,
        expect: &Expect,
    ) -> Result<(Tape, Vec<u8>), OpenError> {
        let dir = Inner {
            provider: dir.0,
            handle: dir.1,
        };
        let mut differs = false;
        let (live, arg) = match mode {
            Mode::Record => {
                let buffer = header
                    .encode(revision)
                    .map_err(|_| OpenError::Refused(Refusal::HeaderTooLarge))?;
                // SAFETY: forwarded.
                let sink = unsafe { ProviderFile::open(dir, 3, name) }.map_err(OpenError::Io)?;
                let mut recorder = Recorder {
                    sink,
                    tape_id: header.tape_id,
                    encoder: Encoder::default(),
                    buffer,
                    seq: 0,
                };
                recorder.close(0).map_err(OpenError::Io)?;
                let live = Live {
                    waiting: BTreeMap::new(),
                    pending: BTreeMap::new(),
                    schedule: schedule::Schedule::new(header.chooser),
                    mode,
                    fault: None,
                    objects: vec![],
                    clocks: BTreeMap::new(),
                    recorder: Some(recorder),
                    replayer: None,
                };
                (live, header.arg.clone())
            }
            Mode::Replay => {
                // SAFETY: forwarded.
                let source = unsafe { ProviderFile::open(dir, 0, name) }.map_err(OpenError::Io)?;
                let (blocks, stored) = match Blocks::open(|bytes| source.read(bytes)) {
                    Ok(b) => b,
                    Err(ReadError::Source(e)) => return Err(OpenError::Io(e)),
                    Err(ReadError::Format(Stop::Damaged(k))) => {
                        return Err(OpenError::Refused(Refusal::Damaged(k)));
                    }
                    Err(ReadError::Format(s)) => {
                        return Err(OpenError::Unreadable(Fault::stopped(s, 0)));
                    }
                };
                if stored.identity != expect.identity {
                    return Err(OpenError::Refused(Refusal::Code));
                }
                if stored.revision != revision && !expect.across {
                    return Err(OpenError::Refused(Refusal::Revision));
                }
                if expect.arg.is_some_and(|a| a != stored.arg) {
                    return Err(OpenError::Refused(Refusal::Arg));
                }
                if stored.limit != expect.limit || stored.charge_policy != expect.charge_policy {
                    return Err(OpenError::Refused(Refusal::Limit));
                }
                differs = stored.revision != revision;
                let live = Live {
                    waiting: BTreeMap::new(),
                    pending: BTreeMap::new(),
                    schedule: schedule::Schedule::new(stored.chooser),
                    mode,
                    fault: None,
                    objects: vec![],
                    clocks: BTreeMap::new(),
                    recorder: None,
                    replayer: Some(Replayer {
                        source,
                        blocks,
                        decoder: Decoder::default(),
                        frames: VecDeque::new(),
                        taken: 0,
                    }),
                };
                (live, stored.arg)
            }
        };
        let cell = cell();
        let mut s = cell.state.lock().unwrap();
        s.generation = s
            .generation
            .checked_add(1)
            .unwrap_or_else(|| crate::process::stop(b"TapeSlots"));
        s.live = Some(live);
        let generation = s.generation;
        drop(s);
        crate::tasks::attach_tape(schedule::Group::root(cell, generation));
        Ok((
            Tape {
                cell,
                generation,
                differs,
            },
            arg,
        ))
    }

    /// `std.<kind>.on_tape`: the substitute for the capability `source`, as
    /// (provider, provider-local handle). Record writes `GRANT{kind}`;
    /// Replay checks it is the next frame.
    pub fn on_tape(&self, kind: u8, source: (Provider, u64)) -> Result<(Provider, u64), Fault> {
        self.grant(kind, None, Some(source)).map(|(p, h, _)| (p, h))
    }
    /// The label of an across open: the tape's revision is not the
    /// opener's. False for every other open.
    pub fn revision_differs(&self) -> bool {
        self.differs
    }
    /// `std.args.on_tape`: Record writes the whole vector into the GRANT
    /// frame and returns it; Replay returns the recorded vector.
    pub fn on_tape_args(&self, args: &[&[u8]]) -> Result<Vec<Vec<u8>>, Fault> {
        let mut vector = Vec::new();
        for a in args {
            put_varint(&mut vector, a.len() as u64);
            vector.extend_from_slice(a);
        }
        let (_, _, stored) = self.grant(crate::entry::ARGS, Some(vector), None)?;
        let mut c = Cursor::new(&stored);
        let mut out = Vec::new();
        while !c.done() {
            out.push(c.bytes().map_err(|_| Fault::Corrupt)?.to_vec());
        }
        Ok(out)
    }
    fn grant(
        &self,
        kind: u8,
        args: Option<Vec<u8>>,
        source: Option<(Provider, u64)>,
    ) -> Result<(Provider, u64, Vec<u8>), Fault> {
        let mut s = self.cell.state.lock().unwrap();
        if s.generation != self.generation {
            return Err(refuse(Fault::Ended));
        }
        let generation = s.generation;
        let live = s.live.as_mut().unwrap();
        if let Some(f) = live.fault {
            return Err(refuse(f));
        }
        live.own_position();
        let stored = match live.mode {
            Mode::Record => {
                let rec = live.recorder.as_mut().unwrap();
                let frame = Frame::Grant {
                    kind,
                    args: args.clone(),
                };
                if let Err(e) = rec.frame(&frame) {
                    live.fault = Some(refuse(Fault::Io(e)));
                    return Err(Fault::Io(e));
                }
                args.unwrap_or_default()
            }
            Mode::Replay => {
                let rep = live.replayer.as_mut().unwrap();
                let stored = match rep.next() {
                    Ok(Frame::Grant { kind: k, args }) if k == kind => Ok(args.unwrap_or_default()),
                    Ok(Frame::End(_)) => Err(rep.diverged(Mismatch::Missing)),
                    Ok(
                        Frame::Wake { .. }
                        | Frame::Cancel { .. }
                        | Frame::Done { .. }
                        | Frame::Choose { .. }
                        | Frame::Wait { .. },
                    ) => Err(rep.diverged(Mismatch::Schedule)),
                    Ok(_) => Err(rep.diverged(Mismatch::Op)),
                    Err(f) => Err(f),
                };
                stored.inspect_err(|&f| live.fault = Some(refuse(f)))?
            }
        };
        let inner = match live.mode {
            Mode::Record => source.map(|(provider, handle)| Inner { provider, handle }),
            Mode::Replay => None,
        };
        live.objects.push(Object {
            inner,
            open_file: false,
        });
        let h = handle(generation, live.objects.len());
        Ok((self.cell.provider, h, stored))
    }

    /// `tape.end`: Record writes the END block with the domain's outcome
    /// `ending`; Replay requires END next, with that outcome (else
    /// `Diverged(Outcome)` at the END frame), and nothing after it. Either
    /// way the tape's handles end here.
    pub fn end(self, ending: &Ending) -> Result<(), Fault> {
        let result = self.finish(Some(ending));
        std::mem::forget(self);
        result
    }
    fn finish(&self, end: Option<&Ending>) -> Result<(), Fault> {
        let mut s = self.cell.state.lock().unwrap();
        if s.generation != self.generation {
            return Err(Fault::Ended);
        }
        let generation = s.generation;
        let mut live = s.live.take().unwrap();
        s.generation = generation
            .checked_add(1)
            .unwrap_or_else(|| crate::process::stop(b"TapeSlots"));
        for (i, o) in live.objects.iter().enumerate() {
            if let (true, Some(inner)) = (o.open_file, o.inner) {
                s.orphans.insert(handle(generation, i + 1), inner);
            }
        }
        drop(s);
        let mut result = live.fault.map_or(Ok(()), Err);
        if end.is_some() && result.is_ok() && (!live.waiting.is_empty() || !live.pending.is_empty())
        {
            result = Err(Fault::Diverged(Divergence {
                at: live.replayer.as_ref().map_or(1, |r| r.taken + 1),
                kind: Mismatch::Schedule,
            }));
        }
        if let Some(rec) = live.recorder.as_mut() {
            let written = if let Some(e) = end {
                rec.frame(&Frame::End(e.clone()))
                    .and_then(|()| rec.close(format::END))
            } else if rec.buffer.is_empty() {
                Ok(())
            } else {
                rec.close(0)
            };
            let closed = rec.sink.close();
            if result.is_ok() {
                result = written.and(closed).map_err(Fault::Io);
            }
        }
        if let (Some(ending), Ok(()), Some(rep)) = (end, result, live.replayer.as_mut()) {
            result = match rep.next() {
                Ok(Frame::End(e)) if e != *ending => Err(rep.diverged(Mismatch::Outcome)),
                Ok(Frame::End(_)) => rep.next().map_or_else(
                    |f| match f {
                        Fault::Diverged(Divergence {
                            kind: Mismatch::Extra,
                            ..
                        }) => Ok(()),
                        f => Err(f),
                    },
                    |_| Err(Fault::Corrupt),
                ),
                Ok(
                    Frame::Wake { .. }
                    | Frame::Cancel { .. }
                    | Frame::Done { .. }
                    | Frame::Choose { .. }
                    | Frame::Wait { .. },
                ) => Err(rep.diverged(Mismatch::Schedule)),
                Ok(_) => Err(rep.diverged(Mismatch::Extra)),
                Err(f) => Err(f),
            };
        }
        if let Some(rep) = live.replayer.as_mut() {
            let closed = rep.source.close().map_err(Fault::Io);
            result = result.and(closed);
        }
        FREE.with_borrow_mut(|f| f.push(self.cell));
        result
    }
}
impl Drop for Tape {
    fn drop(&mut self) {
        let _ = self.finish(None);
    }
}

fn cell_of(state: *mut u8) -> &'static Cell {
    // SAFETY: a tape provider's state is its leaked cell.
    unsafe { &*state.cast::<Cell>() }
}
fn generation_of(h: u64) -> u32 {
    (h >> 32) as u32
}
fn ordinal_of(h: u64) -> usize {
    (h & 0xffff_ffff) as usize
}

/// A first non-ready answer opens W explicitly. Subsequent callbacks keep
/// that WAIT open until the scheduler records WAKE, or cancellation closes W.
unsafe extern "C" fn wait(state: *mut u8, r: *mut Request, token: u64) -> Wait {
    let cell = cell_of(state);
    // SAFETY: the driver pins this request for the entire operation.
    let h = unsafe { (*r).handle };
    let key = r as usize;
    let mut s = cell.state.lock().unwrap();
    if s.generation != generation_of(h) {
        return Wait::default();
    }
    let generation = s.generation;
    let Some(live) = s.live.as_mut() else {
        return Wait::default();
    };
    if live.fault.is_some() {
        return Wait::default();
    }
    if !schedule::member(cell, generation) {
        let live = s.live.as_mut().unwrap();
        live.fault = Some(refuse(Fault::Diverged(Divergence {
            at: live.replayer.as_ref().map_or(1, |r| r.taken + 1),
            kind: Mismatch::Schedule,
        })));
        return Wait::default();
    }
    let live = s.live.as_mut().unwrap();
    let task = schedule::task(cell);
    if live.mode == Mode::Replay {
        if live.waiting.contains_key(&key) {
            return Wait {
                kind: WAIT_PARK,
                deadline_ns: 0,
            };
        }
        let rep = live.replayer.as_mut().unwrap();
        let result = match rep.peek() {
            Ok(Frame::Wait { task: t }) if *t == task => {
                rep.next().unwrap();
                live.waiting.insert(key, task);
                return Wait {
                    kind: WAIT_PARK,
                    deadline_ns: 0,
                };
            }
            Ok(Frame::Op { task: t, .. } | Frame::Issue { task: t, .. }) if *t == task => {
                return Wait::default();
            }
            Ok(_) => rep.diverged_next(Mismatch::Schedule),
            Err(f) => f,
        };
        live.fault = Some(refuse(result));
        return Wait::default();
    }
    let inner = live.objects[ordinal_of(h) - 1].inner.unwrap();
    drop(s);
    // SAFETY: forward with the source's handle and restore the pinned request.
    let w = unsafe {
        (*r).handle = inner.handle;
        let w = (inner.provider.wait)(inner.provider.state, r, token);
        (*r).handle = h;
        w
    };
    if w.kind != WAIT_READY {
        let mut s = cell.state.lock().unwrap();
        let live = s.live.as_mut().unwrap();
        if live.waiting.insert(key, task).is_none() {
            if let Err(e) = live.recorder.as_mut().unwrap().frame(&Frame::Wait { task }) {
                live.fault = Some(refuse(Fault::Io(e)));
                return Wait::default();
            }
        }
    }
    w
}

unsafe extern "C" fn issue(state: *mut u8, r: *mut Request, token: u64) -> Step {
    let cell = cell_of(state);
    // SAFETY: the driver's pinned request, in the task.
    unsafe { issue_in(cell, &mut *r, token) }
}

fn done(outcome: Outcome) -> Step {
    Step {
        kind: STEP_DONE,
        outcome,
        deadline_ns: 0,
    }
}
fn pending() -> Step {
    Step {
        kind: STEP_PENDING,
        outcome: Outcome::default(),
        deadline_ns: 0,
    }
}

unsafe fn issue_in(cell: &Cell, r: &mut Request, token: u64) -> Step {
    let h = r.handle;
    let key = r as *mut Request as usize;
    let mut s = cell.state.lock().unwrap();
    if s.generation != generation_of(h) || s.live.is_none() {
        let orphan = (r.operation == CLOSE)
            .then(|| s.orphans.remove(&h))
            .flatten();
        drop(s);
        if let Some(inner) = orphan {
            // SAFETY: the inner file is still the run's; nothing records it.
            let _ = unsafe { sink(inner, request(CLOSE, 0, 0, &[], empty_buffer())) };
        }
        refuse(Fault::Ended);
        return done(REFUSED);
    }
    let generation = s.generation;
    let live = s.live.as_mut().unwrap();
    if let Some(f) = live.fault {
        refuse(f);
        return done(REFUSED);
    }
    live.waiting.remove(&key);
    let ordinal = ordinal_of(h);
    let task = schedule::task(cell);
    // SAFETY: forwarded.
    let args = unsafe { canonical(r) };
    live.own_position();
    live.schedule.events += 1;
    if live.mode == Mode::Replay {
        // SAFETY: output is written only into the request's spare carrier.
        return match unsafe { replay(live, generation, ordinal, r, &args) } {
            Ok(step) => step,
            Err(f) => {
                live.fault = Some(refuse(f));
                done(REFUSED)
            }
        };
    }
    let inner = live.objects[ordinal - 1].inner.unwrap();
    let before = r.buffer.len;
    drop(s);
    r.handle = inner.handle;
    // SAFETY: source callback, in the task, with its own handle.
    let step = unsafe { (inner.provider.issue)(inner.provider.state, r, token) };
    r.handle = h;
    let mut s = cell.state.lock().unwrap();
    let live = s.live.as_mut().unwrap();
    let own = Frame::Issue {
        task,
        op: r.operation,
        object: ordinal as u64,
        args: Args::of(&args),
    };
    if step.kind == STEP_PENDING {
        if let Err(e) = live.recorder.as_mut().unwrap().frame(&own) {
            live.fault = Some(refuse(Fault::Io(e)));
            return done(REFUSED);
        }
        live.pending.insert(
            key,
            Pending {
                inner,
                task,
                before,
            },
        );
        return step;
    }
    // SAFETY: the source completed this pinned request.
    let (out, answer, delivery) =
        unsafe { record_out(live, generation, r, before, inner, step.outcome) };
    let Frame::Issue {
        task,
        op,
        object,
        args,
    } = own
    else {
        unreachable!()
    };
    let frame = Frame::Op {
        task,
        op,
        object,
        args,
        out,
    };
    if let Err(e) = live.recorder.as_mut().unwrap().frame(&frame) {
        live.fault = Some(refuse(Fault::Io(e)));
    }
    live.schedule.events += 1;
    s.delivering.insert(key, delivery);
    done(answer)
}

/// Normalize a provider outcome and assign newly created object ordinals at
/// production, before another task can observe the completion.
unsafe fn record_out(
    live: &mut Live,
    generation: u32,
    r: &Request,
    before: usize,
    inner: Inner,
    o: Outcome,
) -> (Out, Outcome, Delivery) {
    let ordinal = ordinal_of(r.handle);
    let ok = o.status == 0 && o.error == IoError::OK;
    let mut out = Out {
        status: if o.error == IoError::OK { OK } else { ERROR },
        value: o.value,
        error: (o.error.tag, u64::from(o.error.code)),
        written: o.written,
        payload: vec![],
    };
    let mut answer = o;
    let mut created = None;
    if ok && matches!(r.operation, OPEN | MKDTEMP) {
        live.objects.push(Object {
            inner: Some(Inner {
                provider: inner.provider,
                handle: o.value,
            }),
            open_file: r.operation == OPEN,
        });
        out.value = live.objects.len() as u64;
        answer.value = handle(generation, live.objects.len());
        created = Some(answer.value);
    }
    if ok && r.operation == CLOSE {
        live.objects[ordinal - 1].open_file = false;
    }
    if clock(r.operation) {
        let prev = live
            .clocks
            .insert((r.operation, ordinal as u64), o.value)
            .unwrap_or(0);
        out.value = zz(o.value.wrapping_sub(prev) as i64);
    }
    if ok && carries_payload(r.operation) && r.buffer.len > before {
        // SAFETY: source initialized these bytes in the request's spare carrier.
        out.payload =
            unsafe { std::slice::from_raw_parts(r.buffer.data.add(before), r.buffer.len - before) }
                .to_vec();
    }
    (
        out,
        answer,
        Delivery {
            inner,
            outcome: o,
            created,
        },
    )
}

/// Answer one issue from the next frame (section 4.2's issue check first).
unsafe fn replay(
    live: &mut Live,
    generation: u32,
    ordinal: usize,
    r: &mut Request,
    args: &[u8],
) -> Result<Step, Fault> {
    let task = schedule::task_from_live();
    let rep = live.replayer.as_mut().unwrap();
    let (t, op, object, stored, out) = match rep.next()? {
        Frame::Op {
            task,
            op,
            object,
            args,
            out,
        } => (task, op, object, args, Some(out)),
        Frame::Issue {
            task,
            op,
            object,
            args,
        } => (task, op, object, args, None),
        Frame::End(_) => return Err(rep.diverged(Mismatch::Missing)),
        _ => return Err(rep.diverged(Mismatch::Schedule)),
    };
    if t != task {
        return Err(rep.diverged(Mismatch::Schedule));
    }
    if op != r.operation {
        return Err(rep.diverged(Mismatch::Op));
    }
    if object != ordinal as u64 {
        return Err(rep.diverged(Mismatch::Object));
    }
    if stored != Args::of(args) {
        return Err(rep.diverged(Mismatch::Arg));
    }
    let Some(out) = out else {
        return Ok(pending());
    };
    live.schedule.events += 1;
    // SAFETY: replay's checked payload fits the caller's pinned carrier.
    unsafe { replay_out(live, generation, op, object, r, out).map(done) }
}

unsafe fn replay_out(
    live: &mut Live,
    generation: u32,
    op: u8,
    object: u64,
    r: &mut Request,
    out: Out,
) -> Result<Outcome, Fault> {
    let mut o = Outcome {
        value: out.value,
        error: IoError {
            tag: out.error.0,
            code: u32::try_from(out.error.1).map_err(|_| Fault::Corrupt)?,
        },
        written: out.written,
        status: 0,
    };
    match out.status {
        OK => {}
        ERROR if out.error.0 != 0 => {}
        ABORTED | TIMED_OUT => {}
        _ => return Err(Fault::Corrupt),
    }
    let ok = out.status == OK;
    if clock(op) {
        let prev = live.clocks.get(&(op, object)).copied().unwrap_or(0);
        o.value = prev.wrapping_add(unzz(out.value) as u64);
        live.clocks.insert((op, object), o.value);
    }
    if ok && matches!(op, OPEN | MKDTEMP) {
        if out.value != live.objects.len() as u64 + 1 {
            return Err(Fault::Corrupt);
        }
        live.objects.push(Object {
            inner: None,
            open_file: false,
        });
        o.value = handle(generation, live.objects.len());
    }
    if carries_payload(op) {
        let spare = r.buffer.capacity - r.buffer.len;
        if out.payload.len() > spare || (op == READ && ok && out.payload.len() as u64 != out.value)
        {
            return Err(Fault::Corrupt);
        }
        // SAFETY: the payload fits the carrier's spare capacity.
        unsafe {
            ptr::copy_nonoverlapping(
                out.payload.as_ptr(),
                r.buffer.data.add(r.buffer.len),
                out.payload.len(),
            );
        }
        r.buffer.len += out.payload.len();
    }
    Ok(o)
}

unsafe extern "C" fn resume(state: *mut u8, r: *mut Request, token: u64) -> Step {
    // SAFETY: scheduler pins the operation and calls outside any task.
    unsafe { complete(state, r, token, None) }
}
unsafe extern "C" fn abort(state: *mut u8, r: *mut Request, token: u64, why: u8) -> Outcome {
    // SAFETY: scheduler pins the operation and calls outside any task.
    unsafe { complete(state, r, token, Some(why)).outcome }
}
unsafe fn complete(state: *mut u8, r: *mut Request, token: u64, why: Option<u8>) -> Step {
    let cell = cell_of(state);
    let key = r as usize;
    let mut s = cell.state.lock().unwrap();
    // SAFETY: the scheduler pins the request throughout the callback.
    if s.generation != generation_of(unsafe { (*r).handle }) {
        refuse(Fault::Ended);
        return done(REFUSED);
    }
    let generation = s.generation;
    let Some(live) = s.live.as_mut() else {
        return done(REFUSED);
    };
    if let Some(f) = live.fault {
        refuse(f);
        return done(REFUSED);
    }
    if live.mode == Mode::Replay {
        let rep = live.replayer.as_mut().unwrap();
        let frame = rep.next();
        let result = match frame {
            Ok(Frame::Done { gap, task, out })
                if task == schedule::task(cell)
                    && live.schedule.last.checked_add(gap) == Some(live.schedule.events) =>
            {
                live.schedule.last = live.schedule.events;
                live.schedule.events += 1;
                // SAFETY: external payload is checked before copying.
                unsafe {
                    replay_out(
                        live,
                        generation,
                        (*r).operation,
                        ordinal_of((*r).handle) as u64,
                        &mut *r,
                        out,
                    )
                }
            }
            _ => Err(rep.diverged(Mismatch::Schedule)),
        };
        return match result {
            Ok(o) => done(o),
            Err(f) => {
                live.fault = Some(refuse(f));
                done(REFUSED)
            }
        };
    }
    let p = live.pending.get(&key).unwrap().inner;
    drop(s);
    // SAFETY: forward with the pinned inner provider's own handle.
    let step = unsafe {
        let h = (*r).handle;
        (*r).handle = p.handle;
        let step = match why {
            Some(why) => done((p.provider.abort)(p.provider.state, r, token, why)),
            None => (p.provider.resume)(p.provider.state, r, token),
        };
        (*r).handle = h;
        step
    };
    if step.kind == STEP_PENDING {
        return step;
    }
    let mut s = cell.state.lock().unwrap();
    let live = s.live.as_mut().unwrap();
    let pending = live.pending.remove(&key).unwrap();
    // SAFETY: completed request is still pinned by the scheduler.
    let (mut out, answer, delivery) = unsafe {
        record_out(
            live,
            generation,
            &*r,
            pending.before,
            pending.inner,
            step.outcome,
        )
    };
    if let Some(why) = why {
        out.status = if why == ABORT_CANCEL {
            ABORTED
        } else {
            TIMED_OUT
        };
    }
    live.write_input(Frame::Done {
        gap: 0,
        task: pending.task,
        out,
    });
    live.schedule.events += 1;
    s.delivering.insert(key, delivery);
    done(answer)
}
/// Record runs the source's own deliver on the source's outcome, then maps a
/// handle it created to the substitute's; Replay delivers what it answered.
unsafe extern "C" fn deliver(state: *mut u8, r: *mut Request, outcome: Outcome) -> Outcome {
    let cell = cell_of(state);
    let pending = cell.state.lock().unwrap().delivering.remove(&(r as usize));
    let Some(d) = pending else {
        return outcome;
    };
    // SAFETY: the source's deliver with its own handle, in the task.
    unsafe {
        let h = (*r).handle;
        (*r).handle = d.inner.handle;
        let mut o = (d.inner.provider.deliver)(d.inner.provider.state, r, d.outcome);
        (*r).handle = h;
        if let Some(created) = d.created {
            o.value = created;
        }
        o
    }
}
