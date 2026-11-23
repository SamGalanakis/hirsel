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
mod dispatch;
mod exec;
mod managed;
pub mod run;
pub(crate) mod schedule;
mod unix;
pub(crate) mod writer;
pub(crate) use dispatch::{exec_rules, narrow_exec, narrow_net, net_rules, owns, source};
pub use hirsel_tape_format as format;
#[cfg(test)]
mod schedule_cases;
#[cfg(test)]
mod tests;

use crate::{
    provider::*,
    retention::{RETENTION, Retained},
    unix_net::{UnixOperation, UnixReply},
};
use format::*;
use std::{collections::BTreeMap, ptr, sync::Mutex};

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
    /// The sink's write failed.
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

/// Internal refusal status returned by the tape provider on fault/divergence.
pub(crate) const STATUS_REFUSED: u8 = 2;

/// An operation that the tape refused.
pub const REFUSED: Outcome = Outcome {
    value: 0,
    error: IoError::OK,
    written: 0,
    status: STATUS_REFUSED,
    kind: 0,
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
    /// A file or listing still owned by its mechanical carrier.
    open_resource: bool,
}

/// Where a recorder's closed blocks go: the writer thread for a file of the
/// OS provider, else the Dir's provider, in the recording task.
enum Sink {
    Provider(ProviderFile),
    Writer(std::sync::Arc<writer::Stream>),
    Closed,
}
impl Sink {
    fn close(&mut self) -> Result<(), IoError> {
        match std::mem::replace(self, Sink::Closed) {
            Sink::Provider(mut f) => f.close(),
            Sink::Writer(_) | Sink::Closed => Ok(()),
        }
    }
}

struct Recorder {
    sink: Sink,
    /// The writer's queue was full at a block: the recording ends there with
    /// a TRUNCATED marker and records nothing more.
    truncated: bool,
    tape_id: [u8; 16],
    encoder: Encoder,
    /// The block being filled: `BLOCK_HEAD` reserved bytes, then frames.
    buffer: Vec<u8>,
    seq: u64,
    /// The two most recent consecutive, literal CHOOSE frames, newest last.
    chooses: [Option<(u64, u64)>; 2],
    repeat: u64,
    frames: usize,
    /// Complete frames written to `sink` before any storage failure.
    written_frames: u64,
    /// First storage failure during recording: `(last_complete_frame, error)`.
    /// Once set, recording stops writing while the program keeps running unchanged.
    failed: Option<(u64, IoError)>,
}

/// Launcher replay abort hook: ends the replay process immediately at the first
/// divergence or fault, writing one verdict line and exiting 1 without running
/// any further program code, cleanup or finalizer.
#[derive(Clone, Copy)]
pub(crate) struct ReplayAbort {
    pub(crate) stderr: i32,
    pub(crate) sources: *const crate::abi_sites::SourceTable,
    pub(crate) diagnose: bool,
}
// SAFETY: `sources` is immutable and pinned for the replay process's lifetime.
unsafe impl Send for ReplayAbort {}
// SAFETY: `sources` is immutable and pinned for the replay process's lifetime.
unsafe impl Sync for ReplayAbort {}

struct Replayer {
    source: ProviderFile,
    blocks: Blocks,
    retained_block: Option<Retained<'static>>,
    decoder: Decoder,
    frames: std::vec::IntoIter<Frame>,
    lookahead: Option<Frame>,
    /// Frames taken so far: the position the next divergence names.
    taken: u64,
    comparison: Option<String>,
}

struct Live {
    tape_id: [u8; 16],
    schedule: schedule::Schedule,
    waiting: BTreeMap<usize, u64>,
    pending: BTreeMap<usize, Pending>,
    mode: Mode,
    fault: Option<Fault>,
    replay_abort: Option<ReplayAbort>,
    /// Index ordinal - 1 (section 4.3: one sequence per tape, from 1).
    objects: Vec<Object>,
    /// Replay Net grants retain an inert rule list without an inner provider.
    replay_net_rules: BTreeMap<usize, u64>,
    exec_rules: BTreeMap<usize, std::sync::Arc<hirsel_core_net::exec::ExecRules>>,
    typed_resources: BTreeMap<usize, Resource>,

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
    /// The wrapper refused the call: the inner provider still gets its own
    /// outcome once, and the caller gets the refusal.
    refused: bool,
}

struct State {
    generation: u32,
    live: Option<Live>,
    /// Recorded files and listings still owned at tape end. Their cleanup
    /// closes the inner resource without writing a frame.
    orphans: BTreeMap<u64, Inner>,
    /// Record's SYNC and SYNC_DIR the inner provider accepted, by request
    /// address. An issued sync cannot be aborted, so its result is consumed
    /// even after a fault or the tape's end; only then does it refuse.
    accepted: BTreeMap<usize, Inner>,
    /// By request address: a request is pinned through its delivery.
    delivering: BTreeMap<usize, Delivery>,
}

/// One slot of the table. Cells are never freed; a reused cell has a new
/// generation, so a handle from its earlier tape stays ended.
pub struct Cell {
    state: Mutex<State>,
    provider: Provider,
    hooks: &'static dyn dispatch::Hooks,
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
            accepted: BTreeMap::new(),
            delivering: BTreeMap::new(),
        }),
        provider: Provider {
            state: ptr::null_mut(),
            wait,
            issue: dispatch::issue,
            resume,
            abort,
            deliver,
        },
        hooks: &schedule::TapeHooks,
    }));
    c.provider.state = (c as *mut Cell).cast();
    c
}

enum Seal<'a> {
    Unsealed,
    Explicit(&'a Ending),
    Deadlock,
}
/// An open tape. Dropping it without `end` writes its buffered frames and
/// nothing else (section 8: equivalent to a crash at that point).
pub struct Tape {
    cell: &'static Cell,
    generation: u32,
    /// An across open of a tape whose revision is not the opener's.
    differs: bool,
    // The common registry can drop an open tape without linking the codec.
    drop_unsealed: fn(&Self),
}

enum TapeInput<'a> {
    Record {
        dir: Inner,
        name: &'a [u8],
        header: &'a Header,
    },
    Replay(ReplayInput),
}

struct ReplayInput {
    source: ProviderFile,
    blocks: Blocks,
    stored: Header,
}

impl ReplayInput {
    unsafe fn open(dir: Inner, name: &[u8]) -> Result<Self, OpenError> {
        writer::flush();
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
        Ok(Self {
            source,
            blocks,
            stored,
        })
    }
}

struct Startup {
    arg: Vec<u8>,
    net_rules: Vec<hirsel_core_net::GrantRule>,
    secret_grants: Vec<hirsel_core_net::SecretGrant>,
    env: Option<hirsel_core_net::EnvSnapshot>,
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

pub(crate) fn format_verdict(fault: Fault, at_corrupt: u64) -> String {
    match fault {
        Fault::Diverged(d) => format!("replay: Diverged({:?}) at {}", d.kind, d.at),
        Fault::Truncated(at) => format!("replay: Truncated at {at}"),
        Fault::Damaged(block) => format!("replay: Damaged at {block}"),
        Fault::Corrupt => format!("replay: Corrupt at {at_corrupt}"),
        other => format!("replay: {other}"),
    }
}

impl Live {
    /// If this is a launcher replay, write `replay: VERDICT at N` (plus the source
    /// position under `--diagnose`) and exit 1 immediately so no program code,
    /// cleanup or finalizer runs after the divergence or fault.
    pub(super) fn abort_replay(&self, fault: Fault, site: u64) {
        let Some(abort) = self.replay_abort else {
            return;
        };
        let mut line = format_verdict(fault, self.replayer.as_ref().map_or(0, |r| r.taken + 1));
        if abort.diagnose {
            // SAFETY: `abort.sources` is pinned for the process lifetime.
            if let Some(pos) = unsafe { crate::abi_sites::format_site(site, abort.sources) } {
                line.push_str(" (");
                line.push_str(&pos);
                line.push(')');
            }
        }
        if let Fault::Diverged(_) = fault {
            use std::fmt::Write as _;
            let _ = write!(line, " [event {}: ", self.schedule.events);
            line.push_str(
                self.replayer
                    .as_ref()
                    .and_then(|r| r.comparison.as_deref())
                    .unwrap_or("frame unavailable; expected unavailable; observed unavailable"),
            );
            line.push(']');
        }
        line.push('\n');
        // SAFETY: `abort.stderr` is the launcher's stderr fd.
        unsafe {
            crate::os::diagnostic(abort.stderr, line.as_ptr(), line.len());
            libc::_exit(1);
        }
    }
}

impl Recorder {
    /// Keep period-two CHOOSE runs in a REPEAT. Seals fall between frames,
    /// including between a frame and its DATA continuations. Frames encode
    /// straight into the block being filled, after its reserved head.
    fn frame(&mut self, f: &Frame) -> Result<(), IoError> {
        if self.truncated || self.failed.is_some() {
            return Ok(());
        }
        if let Frame::Choose { gap, index } = *f {
            let choose = Some((gap, index));
            if self.chooses[(self.repeat % 2) as usize] == choose {
                self.repeat += 1;
            } else {
                self.flush_repeat();
                put_choose(&mut self.buffer, gap, index);
                self.chooses = [self.chooses[1], choose];
            }
        } else {
            self.flush_repeat();
            self.chooses = [None, None];
            let mut left = match f {
                Frame::Done { out, .. } | Frame::Op { out, .. } => out.payload.len(),
                Frame::Grant {
                    kind: crate::entry::EXEC,
                    args: Some(snapshot),
                    ..
                } => snapshot.len(),
                _ => 0,
            };
            self.room(left);
            let mut rest = Vec::new();
            self.encoder.frame_into(f, &mut self.buffer, &mut rest);
            for data in rest {
                left = left.saturating_sub(PAYLOAD_CHUNK);
                self.frames += 1;
                if self.full() {
                    let _ = self.close(0);
                    if self.truncated || self.failed.is_some() {
                        return Ok(());
                    }
                    self.room(left);
                }
                format::put_data(&mut self.buffer, data);
            }
        }
        self.frames += 1;
        if matches!(f, Frame::End { .. }) {
            let _ = self.close(format::END);
        } else if self.full() {
            let _ = self.close(0);
        }
        Ok(())
    }

    fn full(&self) -> bool {
        self.buffer.len() - BLOCK_HEAD >= CLOSE_AT || self.frames >= CLOSE_FRAMES
    }

    fn empty(&self) -> bool {
        self.buffer.len() == BLOCK_HEAD
    }

    /// Make room for a frame carrying `payload` bytes, as much of it as the
    /// block can take, in one allocation: a recycled block buffer when the
    /// block is empty and the payload is large, else one reservation. A
    /// recorder holds no buffer between blocks (closing hands it off), so an
    /// idle unit's recording costs no block of memory.
    fn room(&mut self, payload: usize) {
        let need = payload.min(CLOSE_AT + PAYLOAD_CHUNK) + 64;
        if self.buffer.capacity() - self.buffer.len() >= need {
            return;
        }
        if self.empty() && need > 4096 {
            let spare = writer::spare();
            if spare.capacity() >= BLOCK_HEAD + need {
                self.buffer = Self::fresh(spare);
                return;
            }
        }
        self.buffer.reserve(need);
    }

    /// A block buffer with its head reserved.
    fn fresh(mut b: Vec<u8>) -> Vec<u8> {
        b.clear();
        b.resize(BLOCK_HEAD, 0);
        b
    }

    fn flush_repeat(&mut self) {
        if self.repeat != 0 {
            put_repeat(&mut self.buffer, self.repeat);
            self.repeat = 0;
            self.chooses = [None, None];
        }
    }

    fn close(&mut self, flags: u8) -> Result<(), IoError> {
        if self.truncated || self.failed.is_some() {
            return Ok(());
        }
        self.flush_repeat();
        self.chooses = [None, None];
        let block_frames = self.frames as u64;
        self.frames = 0;
        let seq = self.seq;
        self.seq += 1;
        self.encoder.new_block();
        format::seal(&self.tape_id, flags, seq, &mut self.buffer);
        TAPE_COUNTS[0].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        TAPE_COUNTS[1].fetch_add(
            self.buffer.len() as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
        let t0 = std::time::Instant::now();
        let r = match &self.sink {
            Sink::Provider(f) => {
                let r = f.write(&self.buffer);
                self.buffer = Self::fresh(Vec::new());
                r
            }
            Sink::Writer(stream) => {
                let b = std::mem::replace(&mut self.buffer, Self::fresh(Vec::new()));
                if !writer::push(stream, b, block_frames, seq == 0) {
                    self.truncated = true;
                    self.buffer = Vec::new();
                    let mut marker = Self::fresh(Vec::new());
                    format::seal(&self.tape_id, format::TRUNCATED, seq, &mut marker);
                    writer::mark(stream, marker);
                }
                Ok(())
            }
            Sink::Closed => Err(IoError { tag: 5, code: 0 }),
        };
        TAPE_COUNTS[C_WORKER_NS].fetch_add(
            t0.elapsed().as_nanos() as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
        match r {
            Ok(()) => {
                if matches!(self.sink, Sink::Provider(_)) {
                    self.written_frames += block_frames;
                }
                Ok(())
            }
            Err(e) => {
                self.failed = Some((self.written_frames, e));
                Err(e)
            }
        }
    }
}

/// Only structural metadata and allowlisted scalar arguments cross the
/// replay diagnostic boundary. Frame payloads may contain credentials.
struct FrameEvidence<'a>(&'a Frame);

impl std::fmt::Display for FrameEvidence<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            Frame::Wait { gap, task }
            | Frame::Done { gap, task, .. }
            | Frame::Wake { gap, task }
            | Frame::Cancel { gap, task } => write!(f, "task={task} gap={gap}"),
            Frame::Op {
                task,
                op,
                object,
                args,
                ..
            }
            | Frame::Issue {
                task,
                op,
                object,
                args,
            } => {
                write!(
                    f,
                    "{}",
                    IssueEvidence {
                        task: *task,
                        op: *op,
                        object: *object,
                        args
                    }
                )
            }
            Frame::Choose { gap, index } => write!(f, "index={index} gap={gap}"),
            Frame::Grant { gap, kind, .. } => write!(f, "kind={kind} gap={gap}"),
            Frame::Repeat { count } => write!(f, "count={count}"),
            Frame::End { gap, ending } => write!(f, "outcome_kind={} gap={gap}", ending.kind),
        }
    }
}

impl FrameEvidence<'_> {
    fn kind(&self) -> &'static str {
        match self.0 {
            Frame::Wait { .. } => "WAIT",
            Frame::Issue { .. } => "ISSUE",
            Frame::Done { .. } => "DONE",
            Frame::Op { .. } => "OP",
            Frame::Wake { .. } => "WAKE",
            Frame::Cancel { .. } => "CANCEL",
            Frame::Choose { .. } => "CHOOSE",
            Frame::Grant { .. } => "GRANT",
            Frame::Repeat { .. } => "REPEAT",
            Frame::End { .. } => "END",
        }
    }
}

struct IssueEvidence<'a> {
    task: u64,
    op: u8,
    object: u64,
    args: &'a Args,
}

impl std::fmt::Display for IssueEvidence<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self {
            task,
            op,
            object,
            args,
        } = self;
        write!(f, "task={task} op={op} object={object}")?;
        let field = match *op {
            READ | RANDOM | NEXT_ENTRIES => "capacity",
            SLEEP_UNTIL | SLEEP_UNTIL_WALL => "deadline",
            _ => return f.write_str(" args=redacted"),
        };
        match args {
            Args::Literal(bytes) => match bytes.as_slice().try_into() {
                Ok(raw) => write!(f, " {field}={}", u64::from_le_bytes(raw)),
                Err(_) => write!(f, " {field}=invalid({} bytes)", bytes.len()),
            },
            Args::Digest(_) => write!(f, " {field}=redacted"),
        }
    }
}

impl Replayer {
    fn keep_block_storage(&mut self) {
        self.retained_block = RETENTION.retain(self.blocks.buffer_capacity());
        if self.retained_block.is_none() {
            self.blocks.discard_buffer();
        }
    }

    fn peek(&mut self) -> Result<&Frame, Fault> {
        if self.lookahead.is_none() {
            self.lookahead = Some(self.next()?);
            self.taken -= 1;
        }
        Ok(self.lookahead.as_ref().unwrap())
    }
    fn next(&mut self) -> Result<Frame, Fault> {
        loop {
            if let Some(f) = self.lookahead.take().or_else(|| self.frames.next()) {
                if self.frames.as_slice().is_empty() {
                    self.frames = Default::default();
                }
                self.taken += 1;
                return Ok(f);
            }
            let at = self.taken + 1;
            self.retained_block = None;
            let decoded = match self.blocks.next(|bytes| self.source.read(bytes)) {
                Err(ReadError::Format(s)) => Err(Fault::stopped(s, at)),
                Err(ReadError::Source(e)) => Err(Fault::Io(e)),
                Ok(None) => Err(self.diverged_next(Mismatch::Extra)),
                Ok(Some((flags, payload))) => self
                    .decoder
                    .block(payload, flags & END != 0)
                    .map_err(Fault::from),
            };
            self.keep_block_storage();
            self.frames = decoded?.into_iter();
        }
    }
    /// A divergence at the frame `next` returned last.
    fn diverged(&self, kind: Mismatch) -> Fault {
        Fault::Diverged(Divergence {
            at: self.taken,
            kind,
        })
    }
    #[cold]
    fn compared(
        &mut self,
        kind: Mismatch,
        frame: &Frame,
        observed: std::fmt::Arguments<'_>,
    ) -> Fault {
        let expected = FrameEvidence(frame);
        self.comparison = Some(format!(
            "frame {}; expected {expected}; observed {observed}",
            expected.kind()
        ));
        self.diverged(kind)
    }

    #[cold]
    fn note_next(&mut self, observed: std::fmt::Arguments<'_>) {
        self.comparison = Some(match self.lookahead.as_ref() {
            Some(frame) => {
                let expected = FrameEvidence(frame);
                format!(
                    "frame {}; expected {expected}; observed {observed}",
                    expected.kind()
                )
            }
            None => format!("frame unavailable; expected unavailable; observed {observed}"),
        });
    }

    /// A divergence at the frame after the last one taken.
    fn diverged_next(&self, kind: Mismatch) -> Fault {
        Fault::Diverged(Divergence {
            at: self.taken + 1,
            kind,
        })
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ResourceKind {
    Proc,
    Pipe,
    UnixStream,
    UnixListener,
    Datagram,
}

struct Resource {
    kind: ResourceKind,
    parts: u8,
}

impl Resource {
    fn new(kind: ResourceKind, part: u8) -> Self {
        Self {
            kind,
            parts: 1 << part,
        }
    }
}

fn socket_op(op: u8) -> bool {
    matches!(
        op,
        CONNECT
            | LISTEN
            | ACCEPT
            | RECV_BOUNDED
            | RECV_READY
            | SEND_PAIR
            | TLS_CONNECT
            | SEND_SECRET
            | RESOLVE
            | SCRAM
    )
}

fn creates_object(op: u8) -> bool {
    matches!(
        op,
        OPEN | OPEN_DIR
            | ENTRIES
            | MKDTEMP
            | LISTEN
            | CONNECT
            | ACCEPT
            | TLS_CONNECT
            | SCRAM
            | EXEC_SPAWN
    )
}

fn waiting_io(op: u8) -> bool {
    socket_op(op) || unix_operation(op) || matches!(op, STDIN_READ | EXEC_WAIT)
}

fn waiting_io_status(error: u8) -> u8 {
    match error {
        0 => OK,
        6 => TIMED_OUT,
        7 => ABORTED,
        _ => ERROR,
    }
}

fn address_bytes(address: crate::net::Address) -> [u8; 19] {
    let mut bytes = [0; 19];
    bytes[0] = address.family;
    bytes[1..9].copy_from_slice(&address.hi.to_le_bytes());
    bytes[9..17].copy_from_slice(&address.lo.to_le_bytes());
    bytes[17..].copy_from_slice(&address.port.to_le_bytes());
    bytes
}

unsafe fn buffer_bytes(buffer: &Buffer) -> &[u8] {
    if buffer.len == 0 {
        &[]
    } else {
        // SAFETY: the caller pins the buffer's initialized prefix.
        unsafe { std::slice::from_raw_parts(buffer.data, buffer.len) }
    }
}

unsafe fn socket_args(request: &Request) -> Args {
    // SAFETY: the network adapter pins the address, deadline and carriers.
    unsafe {
        let n = &*request.path.cast::<crate::net::NetworkRequest>();
        let mut args = Vec::new();
        if matches!(request.operation, CONNECT | LISTEN) {
            args.extend_from_slice(&address_bytes(n.address));
        }
        if request.operation == LISTEN {
            args.extend_from_slice(&n.backlog.to_le_bytes());
            return Args::of(&args);
        }
        if request.operation == RECV_BOUNDED {
            let spare = request.buffer.capacity.saturating_sub(request.buffer.len) as u64;
            args.extend_from_slice(&spare.to_le_bytes());
        }
        if request.operation == RESOLVE {
            let resolve = &*request.path.cast::<crate::resolve::ResolveRequest>();
            let name = buffer_bytes(&resolve.raw_name);
            put_varint(&mut args, name.len() as u64);
            args.extend_from_slice(name);
            args.extend_from_slice(&(crate::resolve::spare(&request.buffer) as u64).to_le_bytes());
        }
        if request.operation == TLS_CONNECT {
            let tls = &*request.path.cast::<crate::tls::TlsRequest>();
            let name = crate::tls::view_bytes(&tls.name);
            put_varint(&mut args, name.len() as u64);
            args.extend_from_slice(name);
            match tls.pin() {
                hirsel_core_net::TlsTrust::System => args.push(0),
                hirsel_core_net::TlsTrust::Roots(digest) => {
                    args.push(1);
                    args.extend_from_slice(&digest);
                }
                hirsel_core_net::TlsTrust::Any => args.push(2),
            }
            let alpn = crate::tls::view_bytes(&tls.alpn);
            put_varint(&mut args, alpn.len() as u64);
            args.extend_from_slice(alpn);
        }
        if let Some((_, clock, at)) = n.until {
            put_varint(&mut args, ordinal_of(clock) as u64);
            args.extend_from_slice(&at.to_le_bytes());
        } else {
            args.push(0);
        }
        if request.operation == SCRAM {
            let scram = &*request.path.cast::<crate::scram::ScramRequest<'_>>();
            let (origin, patterns) = &scram.send.designations[0];
            let (kind, ordinal) = origin.wire();
            args.push(kind);
            put_varint(&mut args, ordinal);
            let patterns = hirsel_core_net::Pattern::list_text(patterns);
            let pieces = [
                patterns.as_bytes(),
                scram.client_first,
                scram.server_first,
                buffer_bytes(&request.buffer),
                buffer_bytes(&*n.send_tail),
            ];
            for piece in pieces {
                put_varint(&mut args, piece.len() as u64);
            }
            return Args::of_parts(&[&args, pieces[0], pieces[1], pieces[2], pieces[3], pieces[4]]);
        }
        if matches!(request.operation, SEND_PAIR | SEND_SECRET) {
            if request.operation == SEND_SECRET {
                // Each secret's designation in `secrets` order; the wire and
                // the marks follow as the buffer and the tail.
                let send = &*request.path.cast::<crate::secret::SendRequest>();
                put_varint(&mut args, send.designations.len() as u64);
                for (origin, patterns) in &send.designations {
                    let (kind, ordinal) = origin.wire();
                    args.push(kind);
                    put_varint(&mut args, ordinal);
                    let patterns = hirsel_core_net::Pattern::list_text(patterns);
                    put_varint(&mut args, patterns.len() as u64);
                    args.extend_from_slice(patterns.as_bytes());
                }
            }
            let tail = &*n.send_tail;
            let mut tail_len = Vec::new();
            put_varint(&mut args, request.buffer.len as u64);
            put_varint(&mut tail_len, tail.len as u64);
            return Args::of_parts(&[
                &args,
                buffer_bytes(&request.buffer),
                &tail_len,
                buffer_bytes(tail),
            ]);
        }
        Args::of(&args)
    }
}

/// A key operation's canonical arguments: each Secret's origin and policy,
/// never its bytes, then the operation's public operands and the spare.
unsafe fn key_args(r: &Request) -> Args {
    use crate::secret::KeyOp;
    // SAFETY: the key adapter pins its request for the issue.
    let key = unsafe { &*r.path.cast::<crate::secret::KeyRequest<'_>>() };
    let mut head = vec![r.mode];
    let mut texts = Vec::new();
    let mut secret = |head: &mut Vec<u8>, s: &crate::secret::Secret| {
        let (kind, ordinal) = s.origin.wire();
        head.push(kind);
        put_varint(head, ordinal);
        texts.push(s.policy.text().into_bytes());
    };
    secret(&mut head, key.key);
    let operand: &[u8] = match &key.op {
        KeyOp::Wrap(s) => {
            secret(&mut head, s);
            &[]
        }
        KeyOp::Unwrap { blob, uses, .. } => {
            texts.push(uses.text().into_bytes());
            blob
        }
        KeyOp::Sign { alg, msg } => {
            head.push(alg.case());
            msg
        }
    };
    put_varint(
        &mut head,
        r.buffer.capacity.saturating_sub(r.buffer.len) as u64,
    );
    for text in &texts {
        put_varint(&mut head, text.len() as u64);
    }
    put_varint(&mut head, operand.len() as u64);
    let mut parts: Vec<&[u8]> = vec![&head];
    parts.extend(texts.iter().map(Vec::as_slice));
    parts.push(operand);
    Args::of_parts(&parts)
}

/// The canonical argument bytes of an operation (REPLAY-SPEC 4.1; the
/// per-operation choice is recorded in followups.md for section 5.4).
unsafe fn canonical(r: &Request) -> Args {
    // SAFETY: the request's operands are pinned for its issue.
    unsafe {
        if matches!(r.operation, EXEC_SPAWN | EXEC_WAIT | EXEC_SIGNAL) {
            return exec::canonical(r);
        }
        if r.operation == CLOSE && r.mode == crate::exec_runtime::PROC_CLOSE {
            return Args::of(&[0]);
        }
        if r.operation == CLOSE && r.mode & !3 == crate::net::SOCKET_CLOSE {
            return Args::of(&[r.mode & 3]);
        }
        if let Some(op) = UnixOperation::describe(r.operation) {
            return unix::canonical(r, op);
        }
        if socket_op(r.operation) {
            return socket_args(r);
        }
        if r.operation == STDIN_READ {
            let read = &*r.path.cast::<crate::stdin::ReadRequest>();
            let spare = r.buffer.capacity.saturating_sub(r.buffer.len) as u64;
            let mut args = spare.to_le_bytes().to_vec();
            if let Some((_, clock, at)) = read.until {
                put_varint(&mut args, ordinal_of(clock) as u64);
                args.extend_from_slice(&at.to_le_bytes());
            } else {
                args.push(0);
            }
            return Args::of(&args);
        }
        if r.operation == SCRAM_VERIFY {
            return Args::of(buffer_bytes(&r.buffer));
        }
        if r.operation == SECRET_KDF {
            let kdf = &*r.path.cast::<crate::secret::KdfRequest<'_>>();
            let (kind, ordinal) = kdf.secret.origin.wire();
            let policy = kdf.secret.policy.text();
            let child = kdf.child.text();
            let mut head = vec![kind];
            put_varint(&mut head, ordinal);
            put_varint(&mut head, policy.len() as u64);
            put_varint(&mut head, child.len() as u64);
            put_varint(&mut head, kdf.prefix.len() as u64);
            return Args::of_parts(&[
                &head,
                policy.as_bytes(),
                child.as_bytes(),
                kdf.prefix,
                kdf.data,
            ]);
        }
        if r.operation == SECRET_MAC && r.mode >= crate::secret::KEY_WRAP {
            return key_args(r);
        }
        if r.operation == SECRET_MAC {
            let mac = &*r.path.cast::<crate::secret::MacRequest<'_>>();
            let (kind, ordinal) = mac.secret.origin.wire();
            let policy = mac.secret.policy.text();
            let mut head = vec![r.mode, kind];
            put_varint(&mut head, ordinal);
            put_varint(&mut head, policy.len() as u64);
            put_varint(&mut head, mac.data.len() as u64);
            return Args::of_parts(&[
                &head,
                policy.as_bytes(),
                mac.data,
                mac.tag.unwrap_or_default(),
            ]);
        }
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
        Args::of(&match r.operation {
            OPEN => {
                let mut a = vec![r.mode];
                a.extend_from_slice(path);
                a
            }
            READ | RANDOM | NEXT_ENTRIES => (r.buffer.capacity.saturating_sub(r.buffer.len) as u64)
                .to_le_bytes()
                .to_vec(),
            READ_AT => {
                let spare = (r.buffer.capacity.saturating_sub(r.buffer.len) as u64).to_le_bytes();
                return Args::of_parts(&[path, &spare]);
            }
            WRITE_AT => return Args::of_parts(&[path, bytes]),
            WRITE | STDOUT | STDERR | MKDTEMP => bytes.to_vec(),
            MKDIR | REMOVE | REMOVE_TREE | RENAME | OPEN_DIR | STAT => path.to_vec(),
            SLEEP_UNTIL | SLEEP_UNTIL_WALL => bytes.to_vec(),
            _ => vec![],
        })
    }
}

fn clock(op: u8) -> bool {
    matches!(op, WALL_MS | MONO_NS)
}

impl Tape {
    pub(crate) fn group(&self) -> schedule::Group {
        schedule::Group::root(self.cell, self.generation)
    }
    fn secret_mac(&self) -> crate::secret::MacTape {
        crate::secret::MacTape {
            provider: self.cell.provider,
            // Object zero is the run-owned MAC sink. The Secret itself is in
            // the canonical args, including for seals without a grant object.
            handle: handle(self.generation, 0),
        }
    }
    /// Open a recording file at `name` in the Dir `dir` (its provider and
    /// handle), as the code whose revision is `revision`: the launcher passes
    /// the entry object's `hirsel_code_revision`, the entry's code revision.
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
        // SAFETY: forwarded tape directory contract.
        let dir = Inner {
            provider: dir.0,
            handle: dir.1,
        };
        let _commit = crate::sequencer::enter();
        if mode == Mode::Replay && crate::tasks::parallel() {
            crate::process::stop(b"ReplayWorkers");
        }
        let input = match mode {
            Mode::Record => TapeInput::Record { dir, name, header },
            Mode::Replay => TapeInput::Replay(unsafe { ReplayInput::open(dir, name)? }),
        };
        let (tape, stored) = unsafe { Self::open_with_startup(revision, input, expect)? };
        Ok((tape, stored.arg))
    }

    unsafe fn open_with_startup(
        revision: [u8; 32],
        input: TapeInput<'_>,
        expect: &Expect,
    ) -> Result<(Tape, Startup), OpenError> {
        let _commit = crate::sequencer::enter();
        let mut differs = false;
        let (live, startup) = match input {
            TapeInput::Record { dir, name, header } => {
                if !matches!(
                    header.chooser,
                    format::CHOOSER_FIFO | format::CHOOSER_COMMIT
                ) {
                    return Err(OpenError::Unreadable(Fault::Corrupt));
                }
                let buffer = header
                    .encode(revision)
                    .map_err(|_| OpenError::Refused(Refusal::HeaderTooLarge))?;
                // SAFETY: forwarded.
                let mut file =
                    unsafe { ProviderFile::open(dir, 3, name) }.map_err(OpenError::Io)?;
                // SAFETY: the file is open in this run's provider.
                let sink = match unsafe {
                    crate::os::duplicate_file(&dir.provider, file.0.unwrap().handle)
                } {
                    Some(fd) => {
                        file.close().map_err(OpenError::Io)?;
                        Sink::Writer(writer::Stream::new(fd))
                    }
                    None => Sink::Provider(file),
                };
                let mut recorder = Recorder {
                    sink,
                    truncated: false,
                    tape_id: header.tape_id,
                    encoder: Encoder::default(),
                    buffer: {
                        let mut b = Recorder::fresh(Vec::with_capacity(
                            BLOCK_HEAD + buffer.len() + BLOCK_CHECK,
                        ));
                        b.extend_from_slice(&buffer);
                        b
                    },
                    seq: 0,
                    chooses: [None, None],
                    repeat: 0,
                    frames: 0,
                    written_frames: 0,
                    failed: None,
                };
                recorder.close(0).map_err(OpenError::Io)?;
                let live = Live {
                    tape_id: header.tape_id,
                    waiting: BTreeMap::new(),
                    pending: BTreeMap::new(),
                    schedule: schedule::Schedule::new(header.chooser),
                    mode: Mode::Record,
                    fault: None,
                    replay_abort: None,
                    objects: vec![],
                    replay_net_rules: BTreeMap::new(),
                    exec_rules: BTreeMap::new(),
                    typed_resources: BTreeMap::new(),
                    clocks: BTreeMap::new(),
                    recorder: Some(recorder),
                    replayer: None,
                };
                (
                    live,
                    Startup {
                        arg: header.arg.clone(),
                        net_rules: vec![],
                        secret_grants: vec![],
                        env: None,
                    },
                )
            }
            TapeInput::Replay(replay) => {
                if crate::tasks::parallel() {
                    crate::process::stop(b"ReplayWorkers");
                }
                let ReplayInput {
                    source,
                    blocks,
                    stored,
                } = replay;
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
                    tape_id: stored.tape_id,
                    waiting: BTreeMap::new(),
                    pending: BTreeMap::new(),
                    schedule: schedule::Schedule::new(stored.chooser),
                    mode: Mode::Replay,
                    fault: None,
                    replay_abort: None,
                    objects: vec![],
                    replay_net_rules: BTreeMap::new(),
                    exec_rules: BTreeMap::new(),
                    typed_resources: BTreeMap::new(),
                    clocks: BTreeMap::new(),
                    recorder: None,
                    replayer: Some({
                        let mut replay = Replayer {
                            source,
                            blocks,
                            retained_block: None,
                            decoder: Decoder::default(),
                            frames: Default::default(),
                            lookahead: None,
                            taken: 0,
                            comparison: None,
                        };
                        replay.keep_block_storage();
                        replay
                    }),
                };
                (
                    live,
                    Startup {
                        arg: stored.arg,
                        net_rules: stored.net_rules,
                        secret_grants: stored.secret_grants,
                        env: stored.env,
                    },
                )
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
                drop_unsealed: |tape| {
                    let _ = tape.finish(Seal::Unsealed);
                },
            },
            startup,
        ))
    }

    fn bind_replay_net_rules(&self, handle: u64, rules: u64) {
        let mut state = self.cell.state.lock().unwrap();
        let live = state.live.as_mut().unwrap();
        live.replay_net_rules.insert(ordinal_of(handle), rules);
    }

    pub(crate) fn set_replay_abort(&self, abort: ReplayAbort) {
        let mut state = self.cell.state.lock().unwrap();
        if let Some(live) = state.live.as_mut() {
            live.replay_abort = Some(abort);
        }
    }

    /// `std.<kind>.on_tape`: the substitute for the capability `source`, as
    /// (provider, provider-local handle). Record writes `GRANT{kind}`;
    /// Replay checks it is the next frame.
    pub fn on_tape(&self, kind: u8, source: (Provider, u64)) -> Result<(Provider, u64), Fault> {
        let args = format::is_stream_grant(kind).then(|| vec![0]);
        self.grant(kind, args, Some(source)).map(|(p, h, _)| (p, h))
    }

    fn on_tape_exec(&self, source: (Provider, u64)) -> Result<(Provider, u64), Fault> {
        let (provider, handle) = source;
        let snapshot = if handle == 0 {
            None
        } else {
            // SAFETY: the launch owner pins the Exec grant through cleanup.
            let grant = unsafe { &*(handle as *const crate::exec::Grant) };
            let crate::exec::Grant::Native { profiles, .. } = grant else {
                return Err(Fault::Corrupt);
            };
            Some(exec::snapshot(profiles))
        };
        let (provider, handle, stored) =
            self.grant(crate::entry::EXEC, snapshot, Some((provider, handle)))?;
        let rules = exec::rules(&stored)?;
        let mut state = self.cell.state.lock().unwrap();
        let live = state.live.as_mut().unwrap();
        live.exec_rules
            .insert(ordinal_of(handle), std::sync::Arc::new(rules));
        Ok((provider, handle))
    }

    fn on_tape_stream(
        &self,
        kind: u8,
        source: (Provider, u64),
        terminal: bool,
    ) -> Result<(Provider, u64, bool), Fault> {
        let (p, h, stored) = self.grant(kind, Some(vec![u8::from(terminal)]), Some(source))?;
        Ok((p, h, stored[0] == 1))
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
        let _commit = crate::tasks::before_tape_frame(self.group(), false);
        let mut s = self.cell.state.lock().unwrap();
        if s.generation != self.generation {
            return Err(refuse(Fault::Ended));
        }
        let generation = s.generation;
        let live = s.live.as_mut().unwrap();
        if let Some(f) = live.fault {
            return Err(refuse(f));
        }
        let stored = match live.mode {
            Mode::Record => {
                live.write_input(Frame::Grant {
                    gap: 0,
                    kind,
                    args: args.clone(),
                });
                if let Some(fault) = live.fault {
                    return Err(fault);
                }
                args.unwrap_or_default()
            }
            Mode::Replay => {
                let rep = live.replayer.as_mut().unwrap();
                let stored = match rep.next() {
                    Ok(Frame::Grant { gap, kind: k, args })
                        if k == kind && live.schedule.at(gap) =>
                    {
                        live.schedule.last = live.schedule.events;
                        Ok(args.unwrap_or_default())
                    }
                    Ok(frame) => {
                        let mismatch = match frame {
                            Frame::End { .. } => Mismatch::Missing,
                            Frame::Grant { kind: k, .. } if k == kind => Mismatch::Schedule,
                            Frame::Wake { .. }
                            | Frame::Cancel { .. }
                            | Frame::Done { .. }
                            | Frame::Choose { .. }
                            | Frame::Wait { .. } => Mismatch::Schedule,
                            _ => Mismatch::Op,
                        };
                        Err(rep.compared(mismatch, &frame, format_args!("GRANT kind={kind}")))
                    }
                    Err(f) => Err(f),
                };
                stored.inspect_err(|&f| {
                    live.fault = Some(refuse(f));
                    live.abort_replay(f, 0);
                })?
            }
        };
        let inner = match live.mode {
            Mode::Record => source.map(|(provider, handle)| Inner { provider, handle }),
            Mode::Replay => None,
        };
        live.objects.push(Object {
            inner,
            open_resource: false,
        });
        let h = handle(generation, live.objects.len());
        Ok((self.cell.provider, h, stored))
    }

    /// `tape.end`: Record writes the END block with the domain's outcome
    /// `ending`; Replay requires END next, with that outcome (else
    /// `Diverged(Outcome)` at the END frame), and nothing after it. Either
    /// way the tape's handles end here.
    pub fn end(self, ending: &Ending) -> Result<(), Fault> {
        let _commit = crate::tasks::before_tape_frame(self.group(), true);
        let result = self.finish(Seal::Explicit(ending));
        std::mem::forget(self);
        match result {
            Ok(None) => Ok(()),
            Ok(Some((_, e))) => Err(Fault::Io(e)),
            Err(f) => Err(f),
        }
    }
    pub(crate) fn end_with_truncation(
        self,
        ending: &Ending,
    ) -> Result<Option<(u64, IoError)>, Fault> {
        let _commit = crate::tasks::before_tape_frame(self.group(), true);
        let result = self.finish(Seal::Explicit(ending));
        std::mem::forget(self);
        result
    }
    fn finish(&self, seal: Seal<'_>) -> Result<Option<(u64, IoError)>, Fault> {
        crate::tasks::detach_tape(self.group());
        finish(self.cell, self.generation, seal)
    }
}

fn finish(
    cell: &'static Cell,
    expected_generation: u32,
    seal: Seal<'_>,
) -> Result<Option<(u64, IoError)>, Fault> {
    let mut s = cell.state.lock().unwrap();
    if s.generation != expected_generation {
        return Err(Fault::Ended);
    }
    let generation = s.generation;
    let mut live = s.live.take().unwrap();
    s.generation = generation
        .checked_add(1)
        .unwrap_or_else(|| crate::process::stop(b"TapeSlots"));
    for (i, o) in live.objects.iter().enumerate() {
        if let (true, Some(inner)) = (o.open_resource, o.inner) {
            s.orphans.insert(handle(generation, i + 1), inner);
        }
    }
    drop(s);
    let deadlock = matches!(seal, Seal::Deadlock).then(|| Ending::deadlock(live.schedule.events));
    let end = match seal {
        Seal::Unsealed => None,
        Seal::Explicit(ending) => Some(ending),
        Seal::Deadlock => deadlock.as_ref(),
    };
    let mut result = live.fault.map_or(Ok(()), Err);
    if matches!(seal, Seal::Explicit(_))
        && result.is_ok()
        && (!live.waiting.is_empty() || !live.pending.is_empty())
    {
        result = Err(Fault::Diverged(Divergence {
            at: live.replayer.as_ref().map_or(1, |r| r.taken + 1),
            kind: Mismatch::Schedule,
        }));
    }
    let mut truncated = None;
    if let Some(rec) = live.recorder.as_mut() {
        if !rec.truncated && rec.failed.is_none() {
            if let Some(e) = end {
                let _ = rec.frame(&Frame::End {
                    gap: live.schedule.events - live.schedule.last,
                    ending: e.clone(),
                });
            } else if !rec.empty() {
                let _ = rec.close(0);
            }
        }
        let closed = rec.sink.close();
        if rec.failed.is_none()
            && let Err(e) = closed
        {
            rec.failed = Some((rec.written_frames, e));
        }
        truncated = rec.failed;
    }
    if let (Some(ending), Ok(()), Some(rep)) = (end, result, live.replayer.as_mut()) {
        result = match rep.next() {
            Ok(ref frame @ Frame::End { gap, .. }) if !live.schedule.at(gap) => Err(rep.compared(
                Mismatch::Schedule,
                frame,
                format_args!("END gap={}", live.schedule.events - live.schedule.last),
            )),
            Ok(ref frame @ Frame::End { ending: ref e, .. }) if e != ending => {
                let kind = if deadlock.is_some() {
                    Mismatch::Schedule
                } else {
                    Mismatch::Outcome
                };
                Err(rep.compared(
                    kind,
                    frame,
                    format_args!("END outcome_kind={} (payload redacted)", ending.kind),
                ))
            }
            Ok(Frame::End { .. }) => rep.next().map_or_else(
                |f| match f {
                    Fault::Diverged(Divergence {
                        kind: Mismatch::Extra,
                        ..
                    }) => Ok(()),
                    f => Err(f),
                },
                |_| Err(Fault::Corrupt),
            ),
            Ok(frame) => {
                let kind = if deadlock.is_some()
                    || matches!(
                        frame,
                        Frame::Wake { .. }
                            | Frame::Cancel { .. }
                            | Frame::Done { .. }
                            | Frame::Choose { .. }
                            | Frame::Wait { .. }
                    ) {
                    Mismatch::Schedule
                } else {
                    Mismatch::Extra
                };
                Err(rep.compared(
                    kind,
                    &frame,
                    format_args!("END outcome_kind={}", ending.kind),
                ))
            }
            Err(f) => Err(f),
        };
    }
    if let Some(rep) = live.replayer.as_mut() {
        let closed = rep.source.close().map_err(Fault::Io);
        result = result.and(closed);
    }
    if let Err(f) = result {
        live.abort_replay(f, 0);
    }
    if deadlock.is_some() && result.is_ok() && live.replay_abort.is_some() {
        // Recorded deadlock matched and sealed on replay: exit 0.
        // SAFETY: launcher replay sealed cleanly.
        unsafe { libc::_exit(0) };
    }
    FREE.with_borrow_mut(|f| f.push(cell));
    result.map(|()| truncated)
}
impl Drop for Tape {
    fn drop(&mut self) {
        (self.drop_unsealed)(self);
    }
}

fn cell_of(state: *mut u8) -> &'static Cell {
    // SAFETY: a tape provider's state is its leaked cell.
    unsafe { &*state.cast::<Cell>() }
}
/// The typed resources added by Exec and Unix carry CLOSE at release.
pub(crate) fn typed_socket(provider: &Provider, key: u64) -> bool {
    let state = cell_of(provider.state).state.lock().unwrap();
    state.generation == generation_of(key)
        && state
            .live
            .as_ref()
            .and_then(|live| live.typed_resources.get(&ordinal_of(key)))
            .is_some_and(|resource| resource.kind != ResourceKind::Proc)
}

pub(crate) fn socket_parts(provider: &Provider, key: u64, parts: u8) {
    let mut state = cell_of(provider.state).state.lock().unwrap();
    if state.generation == generation_of(key)
        && let Some(resource) = state
            .live
            .as_mut()
            .and_then(|live| live.typed_resources.get_mut(&ordinal_of(key)))
        && resource.kind == ResourceKind::UnixStream
    {
        resource.parts = parts;
    }
}

fn generation_of(h: u64) -> u32 {
    (h >> 32) as u32
}
fn ordinal_of(h: u64) -> usize {
    (h & 0xffff_ffff) as usize
}

unsafe fn source_handle(request: &Request, inner: Inner) -> u64 {
    let direction = if matches!(
        request.operation,
        RECV_BOUNDED | RECV_READY | SEND_PAIR | SEND_SECRET | SCRAM
    ) {
        // SAFETY: owned network operations pin this operand through delivery.
        unsafe { (*request.path.cast::<crate::net::NetworkRequest>()).direction }
    } else if request.operation == CLOSE && request.mode & !3 == crate::net::SOCKET_CLOSE {
        request.mode & 3
    } else {
        0
    };
    inner.handle | u64::from(direction)
}

unsafe fn denied(request: &Request) -> bool {
    // SAFETY: the network adapter supplies this operand for address operations.
    matches!(request.operation, CONNECT | LISTEN)
        && unsafe { (*request.path.cast::<crate::net::NetworkRequest>()).denied }
}

/// A first non-ready answer opens W explicitly. Subsequent callbacks keep
/// that WAIT open until the scheduler records WAKE, or cancellation closes W.
unsafe extern "C" fn wait(state: *mut u8, r: *mut Request, token: u64) -> Wait {
    // This operation has only immediate frames, in both modes.
    if unsafe { matches!((*r).operation, SECRET_MAC | SECRET_KDF | SCRAM_VERIFY) } {
        return Wait::default();
    }
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
            Ok(Frame::Wait { gap, task: t }) if *t == task && live.schedule.at(*gap) => {
                rep.next().unwrap();
                live.schedule.last = live.schedule.events;
                live.waiting.insert(key, task);
                return Wait {
                    kind: WAIT_PARK,
                    deadline_ns: 0,
                };
            }
            Ok(Frame::Op { task: t, .. } | Frame::Issue { task: t, .. }) if *t == task => {
                return Wait::default();
            }
            Ok(_) => {
                rep.note_next(format_args!("WAIT task={task}"));
                rep.diverged_next(Mismatch::Schedule)
            }
            Err(f) => f,
        };
        live.fault = Some(refuse(result));
        live.abort_replay(result, unsafe { (*r).site });
        return Wait::default();
    }
    let inner = live.objects[ordinal_of(h) - 1].inner.unwrap();
    drop(s);
    // SAFETY: forward with the source's handle and restore the pinned request.
    let w = unsafe {
        (*r).handle = source_handle(&*r, inner);
        let w = if denied(&*r) {
            Wait::default()
        } else {
            (inner.provider.wait)(inner.provider.state, r, token)
        };
        (*r).handle = h;
        w
    };
    if w.kind != WAIT_READY {
        let mut s = cell.state.lock().unwrap();
        let live = s.live.as_mut().unwrap();
        if live.waiting.insert(key, task).is_none() {
            live.write_input(Frame::Wait { gap: 0, task });
            if live.fault.is_some() {
                return Wait::default();
            }
        }
    }
    w
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
    if schedule::member(cell, generation_of(h)) {
        let _commit = crate::tasks::before_commit();
    }
    let key = r as *mut Request as usize;
    let mut s = cell.state.lock().unwrap();
    if s.generation != generation_of(h) || s.live.is_none() {
        let orphan = matches!(r.operation, CLOSE | CLOSE_ENTRIES)
            .then(|| s.orphans.remove(&h))
            .flatten();
        drop(s);
        if let Some(inner) = orphan {
            // SAFETY: the inner file is still the run's; nothing records it.
            let _ = unsafe { sink(inner, request(r.operation, 0, 0, &[], empty_buffer())) };
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
    // CHOOSE is anchored just before issue. OP/ISSUE then anchors the gap
    // stream at that same position, before counting the issue.
    let position = live.schedule.events;
    live.commit(task, crate::tasks::group_index);
    live.schedule.last = position;
    if live.mode == Mode::Replay {
        // SAFETY: output is written only into the request's spare carrier.
        return match unsafe { replay(live, generation, ordinal, r, &args, token) } {
            Ok(step) => step,
            Err(f) => {
                live.fault = Some(refuse(f));
                live.abort_replay(f, r.site);
                done(REFUSED)
            }
        };
    }
    let inner = match r.operation {
        SECRET_MAC => Inner {
            provider: crate::secret::MAC_PROVIDER,
            handle: 0,
        },
        SECRET_KDF => Inner {
            provider: crate::secret::KDF_PROVIDER,
            handle: 0,
        },
        SCRAM_VERIFY => Inner {
            provider: crate::scram::VERIFY_PROVIDER,
            handle: 0,
        },
        _ => live.objects[ordinal - 1].inner.unwrap(),
    };
    let before = r.buffer.len;
    drop(s);
    // SAFETY: address operations pin their permission result through delivery.
    let denied = unsafe { denied(r) };
    let step = if denied {
        done(crate::net::DENIED)
    } else {
        r.handle = unsafe { source_handle(r, inner) };
        // SAFETY: source callback, in the task, with its own handle.
        let step = unsafe { crate::provider::issue(&inner.provider, r, token) };
        r.handle = h;
        step
    };
    let mut s = cell.state.lock().unwrap();
    let live = s.live.as_mut().unwrap();
    let own = Frame::Issue {
        task,
        op: r.operation,
        object: ordinal as u64,
        args,
    };
    if step.kind == STEP_PENDING {
        let _ = live.recorder.as_mut().unwrap().frame(&own);
        if matches!(r.operation, SYNC | SYNC_DIR) {
            s.accepted.insert(key, inner);
        }
        let live = s.live.as_mut().unwrap();
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
    let mut outcome = step.outcome;
    if r.operation == SECRET_KDF {
        // The new designation follows this delivery's tape position, even if
        // another unit installs its handle in the Secret table first.
        outcome.value = live.schedule.events + 1;
    }
    // SAFETY: the source completed this pinned request.
    let (out, answer, delivery) =
        unsafe { record_out(live, generation, r, before, inner, outcome) };
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
    let _ = live.recorder.as_mut().unwrap().frame(&frame);
    live.schedule.events += 1;
    if !denied {
        s.delivering.insert(key, delivery);
    }
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
        status: if r.operation == EXEC_WAIT {
            match o.error.tag {
                0 => OK,
                2 => TIMED_OUT,
                3 => ABORTED,
                _ => ERROR,
            }
        } else if waiting_io(r.operation) {
            waiting_io_status(o.error.tag)
        } else if o.error == IoError::OK {
            OK
        } else {
            ERROR
        },
        value: o.value,
        error: (o.error.tag, u64::from(o.error.code)),
        written: o.written,
        payload: vec![],
    };
    let mut answer = o;
    let mut created = None;
    let unix_reply = UnixOperation::describe(r.operation).map(|op| op.reply);
    let unix_kind = unix_reply.and_then(unix::kind);
    if ok && (creates_object(r.operation) || unix_kind.is_some()) {
        live.objects.push(Object {
            inner: Some(Inner {
                provider: inner.provider,
                handle: o.value,
            }),
            open_resource: matches!(r.operation, OPEN | ENTRIES),
        });
        out.value = live.objects.len() as u64;
        answer.value = handle(generation, live.objects.len());
        created = Some(answer.value);
        if let Some(kind) = unix_kind {
            live.typed_resources
                .insert(live.objects.len(), Resource::new(kind, 0));
        }
    }
    if ok && r.operation == EXEC_SPAWN {
        live.typed_resources
            .insert(live.objects.len(), Resource::new(ResourceKind::Proc, 0));
        // SAFETY: spawn pins its private output slots through delivery.
        let call = unsafe {
            &mut (*r
                .path
                .cast_mut()
                .cast::<crate::exec_runtime::Call<crate::exec_runtime::SpawnCall>>())
            .data
        };
        for (i, pipe) in call.pipes.iter_mut().enumerate() {
            if *pipe == 0 {
                put_varint(&mut out.payload, 0);
                continue;
            }
            let part = if i == 0 { 2 } else { 1 };
            live.objects.push(Object {
                inner: Some(Inner {
                    provider: inner.provider,
                    handle: *pipe & !3,
                }),
                open_resource: false,
            });
            live.typed_resources
                .insert(live.objects.len(), Resource::new(ResourceKind::Pipe, part));
            put_varint(&mut out.payload, live.objects.len() as u64);
            *pipe = (live.objects.len() as u64) << 2 | u64::from(part);
        }
    }
    if ok && r.operation == CLOSE && r.mode == crate::exec_runtime::PROC_CLOSE {
        live.typed_resources.remove(&ordinal);
    }
    if ok && r.operation == CLOSE && r.mode & !3 == crate::net::SOCKET_CLOSE {
        let entry = live.typed_resources.get_mut(&ordinal).unwrap();
        entry.parts &= !(1 << (r.mode & 3));
        if entry.parts == 0 {
            live.typed_resources.remove(&ordinal);
        }
        out.value = 0;
    }
    if ok && matches!(r.operation, CLOSE | CLOSE_ENTRIES) {
        live.objects[ordinal - 1].open_resource = false;
    }
    if clock(r.operation) {
        let prev = live
            .clocks
            .insert((r.operation, ordinal as u64), o.value)
            .unwrap_or(0);
        out.value = zz(o.value.wrapping_sub(prev) as i64);
    }
    if ok && matches!(r.operation, LISTEN | ACCEPT) {
        // SAFETY: the source wrote the address into the pinned network operand.
        let network = unsafe { &*r.path.cast::<crate::net::NetworkRequest>() };
        out.payload = address_bytes(network.address).to_vec();
    } else if ok && matches!(r.operation, STAT | STAT_FILE) {
        out.payload.push(o.kind);
        out.payload.extend_from_slice(&o.written.to_le_bytes());
    } else if ok && unix_reply == Some(UnixReply::Packet) {
        out.payload.push(o.kind);
        if r.buffer.len > before {
            // SAFETY: receive initialized exactly its appended bytes.
            out.payload.extend(unsafe {
                std::slice::from_raw_parts(r.buffer.data.add(before), r.buffer.len - before)
            });
        }
    } else if ok && r.operation == RESOLVE {
        // SAFETY: the resolve provider initialized these Ip elements in spare.
        for i in before..r.buffer.len {
            let ip = unsafe { r.buffer.data.cast::<hirsel_core_net::Ip>().add(i).read() };
            out.payload
                .extend_from_slice(&hirsel_core_net::ip_record(std::net::IpAddr::from(ip)));
        }
    } else if ok && r.operation == TLS_CONNECT {
        // SAFETY: the handshake pins both outputs through delivery.
        unsafe {
            let tls = &*r.path.cast::<crate::tls::TlsRequest>();
            let protocol = buffer_bytes(&*tls.protocol_out);
            let binding = tls.binding();
            out.payload.push(protocol.len() as u8);
            out.payload.extend_from_slice(protocol);
            out.payload.extend_from_slice(&*tls.peer_out);
            out.payload.push(binding.len() as u8);
            out.payload.extend_from_slice(binding);
        }
    } else if ok
        && carries_payload(r.operation)
        && r.buffer.len.saturating_sub(before) > 256
        && order_only()
    {
        // mc-units-untied order-only variant (not replayable): a payload over
        // 256 bytes is recorded as its length only (`value`), no bytes, no digest.
        TAPE_COUNTS[3].fetch_add(
            (r.buffer.len - before) as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
    } else if ok && carries_payload(r.operation) && r.buffer.len > before {
        // SAFETY: source initialized these bytes in the request's spare carrier.
        out.payload =
            unsafe { std::slice::from_raw_parts(r.buffer.data.add(before), r.buffer.len - before) }
                .to_vec();
        TAPE_COUNTS[2].fetch_add(
            out.payload.len() as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
    }
    if r.operation == NEXT_ENTRIES {
        out.written = out.payload.len() as u64;
    }
    (
        out,
        answer,
        Delivery {
            inner,
            outcome: o,
            created,
            refused: false,
        },
    )
}

/// Answer one issue from the next frame (section 4.2's issue check first).
unsafe fn replay(
    live: &mut Live,
    generation: u32,
    ordinal: usize,
    r: &mut Request,
    args: &Args,
    token: u64,
) -> Result<Step, Fault> {
    let task = schedule::task_from_live();
    let rep = live.replayer.as_mut().unwrap();
    let observed = IssueEvidence {
        task,
        op: r.operation,
        object: ordinal as u64,
        args,
    };
    let frame = rep.next().map_err(|fault| {
        if let Fault::Diverged(_) = fault {
            rep.note_next(format_args!("{observed}"));
        }
        fault
    })?;
    let mismatch = match &frame {
        Frame::Op {
            task: t,
            op,
            object,
            args: stored,
            ..
        }
        | Frame::Issue {
            task: t,
            op,
            object,
            args: stored,
        } => {
            if *t != task {
                Some(Mismatch::Schedule)
            } else if *op != r.operation {
                Some(Mismatch::Op)
            } else if *object != ordinal as u64 {
                Some(Mismatch::Object)
            } else if stored != args {
                Some(Mismatch::Arg)
            } else {
                None
            }
        }
        Frame::End { .. } => Some(Mismatch::Missing),
        _ => Some(Mismatch::Schedule),
    };
    if let Some(kind) = mismatch {
        return Err(rep.compared(kind, &frame, format_args!("{observed}")));
    }
    let op = r.operation;
    let object = ordinal as u64;
    let Frame::Op { out, .. } = frame else {
        if matches!(
            op,
            SECRET_MAC | SECRET_KDF | SCRAM_VERIFY | EXEC_SPAWN | EXEC_SIGNAL
        ) || (unix_operation(op) && !unix::can_park(op))
            || (op == CLOSE && matches!(r.mode, crate::exec_runtime::PROC_CLOSE | 0x40..=0x42))
        {
            return Err(Fault::Corrupt);
        }
        // Without a scheduler nothing else readies a replayed helper, and
        // nothing else runs before its completion. Ready it only when its
        // recorded Done comes next, as `complete` will accept; any other
        // frame is the divergence `complete` would report.
        if (matches!(op, SYNC | SYNC_DIR | RESOLVE | STDIN_READ | EXEC_WAIT) || unix::can_park(op))
            && !crate::tasks::has_provider_task()
        {
            if !matches!(rep.peek()?, Frame::Done { gap, task: t, .. }
                if *t == task && live.schedule.last.checked_add(*gap) == Some(live.schedule.events))
            {
                rep.note_next(format_args!(
                    "DONE task={task} event={}",
                    live.schedule.events
                ));
                return Err(rep.diverged(Mismatch::Schedule));
            }
            crate::tasks::hirsel_core_ready(token);
        }
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
    let unix_reply = UnixOperation::describe(op).map(|op| op.reply);
    if let Some(reply) = unix_reply {
        unix::delivery(r, reply, &out, live.objects.len() as u64)?;
    }
    if op == CLOSE && r.mode & !3 == crate::net::SOCKET_CLOSE {
        let ordinal = object as usize;
        let part = 1 << (r.mode & 3);
        let resource = live
            .typed_resources
            .get_mut(&ordinal)
            .ok_or(Fault::Corrupt)?;
        if out != Out::default()
            || resource.kind == ResourceKind::Proc
            || resource.parts & part == 0
        {
            return Err(Fault::Corrupt);
        }
        resource.parts &= !part;
        let closed = resource.parts == 0;
        if closed {
            live.typed_resources.remove(&ordinal);
        }
        return Ok(Outcome {
            value: u64::from(closed),
            ..Outcome::default()
        });
    }
    let mut spawn_pipes = [0; 3];
    if matches!(op, EXEC_SPAWN | EXEC_WAIT | EXEC_SIGNAL) {
        if op != EXEC_SPAWN
            && live
                .typed_resources
                .get(&(object as usize))
                .is_none_or(|r| r.kind != ResourceKind::Proc)
        {
            return Err(Fault::Corrupt);
        }
        spawn_pipes = exec::delivery(r, &out, live.objects.len() as u64)?;
    }
    if op == CLOSE && r.mode == crate::exec_runtime::PROC_CLOSE {
        if out != Out::default()
            || live
                .typed_resources
                .get(&(object as usize))
                .is_none_or(|r| r.kind != ResourceKind::Proc)
        {
            return Err(Fault::Corrupt);
        }
        live.typed_resources.remove(&(object as usize));
        return Ok(Outcome::default());
    }
    let mut o = Outcome {
        value: out.value,
        error: IoError {
            tag: out.error.0,
            code: u32::try_from(out.error.1).map_err(|_| Fault::Corrupt)?,
        },
        written: out.written,
        status: 0,
        kind: 0,
    };
    match out.status {
        OK => {}
        ERROR if out.error.0 != 0 => {}
        ABORTED | TIMED_OUT => {
            if op == EXEC_WAIT {
                o.error.tag = if out.status == ABORTED { 3 } else { 2 };
            } else if waiting_io(op) {
                o.error.tag = if out.status == ABORTED { 7 } else { 6 };
            }
        }
        _ => return Err(Fault::Corrupt),
    }
    let ok = out.status == OK;
    if clock(op) {
        let prev = live.clocks.get(&(op, object)).copied().unwrap_or(0);
        o.value = prev.wrapping_add(unzz(out.value) as u64);
        live.clocks.insert((op, object), o.value);
    }
    let unix_kind = unix_reply.and_then(unix::kind);
    if ok && (creates_object(op) || unix_kind.is_some()) {
        if out.value != live.objects.len() as u64 + 1 {
            return Err(Fault::Corrupt);
        }
        live.objects.push(Object {
            inner: None,
            open_resource: false,
        });
        o.value = handle(generation, live.objects.len());
        if let Some(kind) = unix_kind {
            live.typed_resources
                .insert(ordinal_of(o.value), Resource::new(kind, 0));
        }
    }
    if ok && op == EXEC_SPAWN {
        live.typed_resources
            .insert(live.objects.len(), Resource::new(ResourceKind::Proc, 0));
        // SAFETY: every ordinal and the whole pipe plan passed delivery validation.
        let call = unsafe {
            &mut (*r
                .path
                .cast_mut()
                .cast::<crate::exec_runtime::Call<crate::exec_runtime::SpawnCall>>())
            .data
        };
        for (i, pipe) in spawn_pipes.into_iter().enumerate() {
            if pipe == 0 {
                continue;
            }
            let part = if i == 0 { 2 } else { 1 };
            live.objects.push(Object {
                inner: None,
                open_resource: false,
            });
            live.typed_resources
                .insert(pipe as usize, Resource::new(ResourceKind::Pipe, part));
            call.pipes[i] = (pipe << 2) | u64::from(part);
        }
    }
    if matches!(op, EXEC_SPAWN | EXEC_WAIT | EXEC_SIGNAL) {
        return Ok(o);
    }
    if unix_reply == Some(UnixReply::Packet) {
        if ok {
            let (flag, bytes) = out.payload.split_first().unwrap();
            if !bytes.is_empty() {
                // SAFETY: Unix delivery validation proved the byte count fits.
                unsafe {
                    ptr::copy_nonoverlapping(
                        bytes.as_ptr(),
                        r.buffer.data.add(r.buffer.len),
                        bytes.len(),
                    )
                };
                r.buffer.len += bytes.len();
            }
            o.kind = *flag;
        }
        return Ok(o);
    }
    if matches!(op, LISTEN | ACCEPT) {
        if ok {
            // SAFETY: this request pins the writable address through delivery.
            let network = unsafe { &mut *r.path.cast_mut().cast::<crate::net::NetworkRequest>() };
            let mut bytes = Cursor::new(&out.payload);
            network.address = crate::net::Address {
                family: bytes.u8()?,
                hi: u64::from_le_bytes(bytes.fixed()?),
                lo: u64::from_le_bytes(bytes.fixed()?),
                port: u16::from_le_bytes(bytes.fixed()?),
            };
        }
    } else if matches!(op, STAT | STAT_FILE) {
        if ok {
            let [kind, time @ ..] = out.payload.as_slice() else {
                return Err(Fault::Corrupt);
            };
            if *kind > 3 {
                return Err(Fault::Corrupt);
            }
            o.kind = *kind;
            o.written = u64::from_le_bytes(time.try_into().map_err(|_| Fault::Corrupt)?);
        } else if !out.payload.is_empty() {
            return Err(Fault::Corrupt);
        }
    } else if op == TLS_CONNECT {
        if ok {
            let (protocol, digest, binding) = tls_connect_payload(&out.payload)?;
            // SAFETY: TLS_CONNECT pins the offer and its output buffers.
            unsafe {
                let tls = &*r.path.cast::<crate::tls::TlsRequest>();
                let buffer = &mut *tls.protocol_out;
                if protocol.len() > buffer.capacity
                    || (!protocol.is_empty() && !tls.offers(protocol))
                {
                    return Err(Fault::Corrupt);
                }
                if !protocol.is_empty() {
                    ptr::copy_nonoverlapping(protocol.as_ptr(), buffer.data, protocol.len());
                }
                buffer.len = protocol.len();
                *tls.peer_out = *digest;
                let out = &mut *tls.binding_out;
                if binding.len() > out.capacity {
                    return Err(Fault::Corrupt);
                }
                ptr::copy_nonoverlapping(binding.as_ptr(), out.data, binding.len());
                out.len = binding.len();
            }
        }
    } else if op == RESOLVE {
        if ok {
            let count = usize::try_from(out.value).map_err(|_| Fault::Corrupt)?;
            if count > crate::resolve::spare(&r.buffer) {
                return Err(Fault::Corrupt);
            }
            let addresses = hirsel_core_net::ip_records(&out.payload)
                .filter(|addresses| addresses.len() == count)
                .ok_or(Fault::Corrupt)?;
            // SAFETY: RESOLVE pins the list with at least count spare slots.
            unsafe { crate::resolve::append(&mut r.buffer, &addresses) };
        }
    } else if op == SECRET_KDF {
        if !ok || out.value != live.schedule.events {
            return Err(Fault::Corrupt);
        }
    } else if op == SECRET_MAC && r.mode >= crate::secret::KEY_WRAP {
        // Key operations: a produced blob or signature is the payload, and
        // unwrap records its outcome only; no key material is on the tape.
        let produced = r.mode != crate::secret::KEY_UNWRAP;
        let spare = r.buffer.capacity.saturating_sub(r.buffer.len);
        if if ok {
            !produced && (out.value != 0 || !out.payload.is_empty())
                || produced && (out.value != out.payload.len() as u64 || out.payload.len() > spare)
        } else {
            !(1..=4).contains(&out.error.0) || !out.payload.is_empty()
        } {
            return Err(Fault::Corrupt);
        }
        if out.error.0 == 4 {
            // A refused output's available bytes are its recorded spare.
            o.written = spare as u64;
        }
        if ok && produced {
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
    } else if op == SECRET_MAC {
        if !ok
            || if r.mode == 0 {
                out.value != 0 || out.payload.len() != 32
            } else {
                out.value > 1 || !out.payload.is_empty()
            }
        {
            return Err(Fault::Corrupt);
        }
        if r.mode == 0 {
            // SAFETY: the signing adapter pins an inline 32-byte destination.
            unsafe { ptr::copy_nonoverlapping(out.payload.as_ptr(), r.buffer.data, 32) };
            r.buffer.len = 32;
        }
    } else if carries_payload(op) {
        if op == NEXT_ENTRIES
            && (!valid_entries(&out.payload, out.value) || (!ok && !out.payload.is_empty()))
        {
            return Err(Fault::Corrupt);
        }
        if op == NEXT_ENTRIES {
            // The tape uses the byte extent to join continuation frames.
            o.written = 0;
        }
        if op == READ_AT && !ok && !out.payload.is_empty() {
            return Err(Fault::Corrupt);
        }
        let spare = r.buffer.capacity.saturating_sub(r.buffer.len);
        if out.payload.len() > spare {
            return Err(Fault::Corrupt);
        }
        if !out.payload.is_empty() {
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
    } else if op == WRITE_AT {
        if out.written > r.buffer.len as u64 || (ok && out.written != r.buffer.len as u64) {
            return Err(Fault::Corrupt);
        }
    } else if op == RECV_READY {
        // Readiness carries no count: only its status and error.
        if out.value != 0 || out.written != 0 {
            return Err(Fault::Corrupt);
        }
    } else if op == SEND_SECRET {
        if out.value != 0 || (ok && out.written != 0) {
            return Err(Fault::Corrupt);
        }
    } else if op == SCRAM_VERIFY {
        if !ok || out.value > 1 {
            return Err(Fault::Corrupt);
        }
    } else if op == SEND_PAIR && ok {
        // SAFETY: the send adapter pins the tail carrier through delivery.
        let tail = unsafe { &*(*r.path.cast::<crate::net::NetworkRequest>()).send_tail };
        if out.value > (r.buffer.len as u64).saturating_add(tail.len as u64) {
            return Err(Fault::Corrupt);
        }
    }
    Ok(o)
}

/// Refuse malformed record streams before they reach a caller's Bytes carrier.
fn valid_entries(mut bytes: &[u8], count: u64) -> bool {
    let mut records = 0;
    while let Some(([kind, lo, hi], rest)) = bytes.split_first_chunk::<3>() {
        let n = u16::from_le_bytes([*lo, *hi]) as usize;
        if *kind > 3 || !(1..=255).contains(&n) {
            return false;
        }
        let Some((name, tail)) = rest.split_at_checked(n) else {
            return false;
        };
        if matches!(name, b"." | b"..") || name.iter().any(|&b| b == 0 || b == b'/') {
            return false;
        }
        bytes = tail;
        records += 1;
    }
    bytes.is_empty() && records == count
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
    let current = s.generation == generation_of(unsafe { (*r).handle });
    if let Some(&inner) = s.accepted.get(&key)
        && !(current && s.live.as_ref().is_some_and(|live| live.fault.is_none()))
    {
        // A sync the inner provider accepted before the fault or the end:
        // wait for its result, deliver it to the inner provider, and refuse.
        let fault = s
            .live
            .as_ref()
            .filter(|_| current)
            .and_then(|live| live.fault)
            .unwrap_or(Fault::Ended);
        drop(s);
        debug_assert!(why.is_none(), "an issued sync is never aborted");
        // SAFETY: forward with the pinned inner provider's own handle.
        let step = unsafe {
            let h = (*r).handle;
            (*r).handle = source_handle(&*r, inner);
            let step = (inner.provider.resume)(inner.provider.state, r, token);
            (*r).handle = h;
            step
        };
        if step.kind == STEP_PENDING {
            return step;
        }
        let mut s = cell.state.lock().unwrap();
        s.accepted.remove(&key);
        if let Some(live) = s.live.as_mut().filter(|_| current) {
            live.pending.remove(&key);
        }
        s.delivering.insert(
            key,
            Delivery {
                inner,
                outcome: step.outcome,
                created: None,
                refused: true,
            },
        );
        refuse(fault);
        return done(REFUSED);
    }
    if !current {
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
                live.delivery(task);
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
            Ok(frame) => Err(rep.compared(
                Mismatch::Schedule,
                &frame,
                format_args!(
                    "DONE task={} event={}",
                    schedule::task(cell),
                    live.schedule.events
                ),
            )),
            Err(_) => Err(rep.diverged(Mismatch::Schedule)),
        };
        return match result {
            Ok(o) => done(o),
            Err(f) => {
                live.fault = Some(refuse(f));
                live.abort_replay(f, unsafe { (*r).site });
                done(REFUSED)
            }
        };
    }
    let p = live.pending.get(&key).unwrap().inner;
    drop(s);
    // SAFETY: forward with the pinned inner provider's own handle.
    let step = unsafe {
        let h = (*r).handle;
        (*r).handle = source_handle(&*r, p);
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
    s.accepted.remove(&key);
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
    if let Some(why) = why
        && !waiting_io(unsafe { (*r).operation })
    {
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
    live.delivery(pending.task);
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
        (*r).handle = source_handle(&*r, d.inner);
        let mut o = (d.inner.provider.deliver)(d.inner.provider.state, r, d.outcome);
        (*r).handle = h;
        if d.refused {
            return outcome;
        }
        if let Some(created) = d.created {
            o.value = created;
        }
        o
    }
}

/// mc-units-untied: `HIRSEL_TAPE_ORDER_ONLY` records large payloads as their
/// length only (a cost bound; such a tape cannot replay).
fn order_only() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("HIRSEL_TAPE_ORDER_ONLY").is_some())
}

/// mc-units-untied counters: block writes, block bytes, payload bytes
/// recorded, payload bytes skipped by the order-only variant, then the
/// `C_*` counters below.
pub(crate) static TAPE_COUNTS: [std::sync::atomic::AtomicU64; 9] =
    [const { std::sync::atomic::AtomicU64::new(0) }; 9];
/// Nanoseconds recording tasks spent handing off (or writing) closed blocks.
pub(crate) const C_WORKER_NS: usize = 4;
/// Recordings truncated because the writer's queue was full.
pub(crate) const C_TRUNCATED: usize = 5;
/// Nanoseconds the writer thread spent writing.
pub(crate) const C_WRITER_NS: usize = 6;
/// The writer's write calls (one per batch of one file's blocks).
pub(crate) const C_WRITER_CALLS: usize = 7;
/// Files whose output stopped at a failed write.
pub(crate) const C_WRITE_FAILED: usize = 8;
