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
mod arguments;
mod close_replay;
mod dispatch;
mod error;
mod exec;
mod file;
pub(crate) mod helper;
mod managed;
mod record_delivery;
mod recorder;
mod retirement;
pub(crate) use retirement::{Retirement, ServedPool, register_udp_retirements, served_udp_pools};
mod replay_delivery;
pub mod run;
pub(crate) mod schedule;
mod secret;
mod tls;
mod typed;
pub(crate) use typed::{socket_parts, typed_socket};
pub(crate) mod units;
mod unix;
pub(crate) mod writer;
pub(crate) use dispatch::{exec_rules, narrow_exec, narrow_net, net_rules, owns, source};
pub use error::{Divergence, Fault, Mismatch, OpenError, Refusal};
pub use hirsel_tape_format as format;
#[cfg(test)]
mod schedule_cases;
#[cfg(test)]
mod tests;

use crate::net::{address_bytes, denial};
use crate::{
    provider::*,
    retention::{RETENTION, Retained},
    unix_net::{UnixOperation, UnixReply},
};
use arguments::canonical;
use close_replay::{CloseKey, Closes, is_close, replay_close};
use file::ProviderFile;
use format::*;
use record_delivery::record_out;
use replay_delivery::{ReadSpace, replay_out};
use std::{collections::BTreeMap, ptr, sync::Mutex};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    Record,
    Replay,
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
    /// The stream number every block's head carries: 0 for the root.
    stream: u32,
    /// The helpers this unit's spawns started, numbering the next one's index.
    spawns: u64,
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

struct FrameReader {
    source: ProviderFile,
    blocks: StreamBlocks,
    retained_block: Option<Retained<'static>>,
    decoder: Decoder,
    frames: std::vec::IntoIter<Frame>,
}

struct Replayer {
    reader: FrameReader,
    closes: Closes,
    lookahead: Option<Frame>,
    /// Frames taken so far: the position the next divergence names.
    taken: u64,
    comparison: Option<String>,
    pending_reads: BTreeMap<usize, ReadSpace>,
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
    udp_retirements: BTreeMap<u64, retirement::Pool>,

    /// The previous value of each clock op on each object (section 5.4).
    clocks: BTreeMap<(u8, u64), u64>,
    /// Record: each grant's kind and stored snapshot by ordinal, which a
    /// helper's entry writes for a capability it holds.
    grants: BTreeMap<usize, (u8, Vec<u8>)>,
    recorder: Option<Recorder>,
    replayer: Option<Replayer>,
    /// Record: the unit's take-point count and unit-object ordinals.
    takes: units::Takes,
    /// Record: each TCP socket's and listener's snapshot by ordinal, as a
    /// unit operation that carries it to another unit writes it.
    sockets: BTreeMap<usize, units::crossing::Snapshot>,
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
    let c = Box::into_raw(Box::new(Cell {
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
    // SAFETY: leaked; writing via `c`, not a `&mut`, keeps `cell_of`'s copy valid.
    unsafe { ((*c).provider.state = c.cast(), &*c).1 }
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
    blocks: StreamBlocks,
    stored: Header,
    origin: (Inner, Vec<u8>),
}

impl ReplayInput {
    unsafe fn open(dir: Inner, name: &[u8]) -> Result<Self, OpenError> {
        writer::flush();
        // SAFETY: forwarded.
        let source = unsafe { ProviderFile::open(dir, 0, name) }.map_err(OpenError::Io)?;
        let (blocks, stored) = match Index::root(|offset, bytes| source.read_at(offset, bytes)) {
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
            origin: (dir, name.to_vec()),
        })
    }
}

struct Startup {
    arg: Vec<u8>,
    net_rules: Vec<hirsel_core_net::GrantRule>,
    secret_grants: Vec<hirsel_core_net::SecretGrant>,
    cred_grants: Vec<hirsel_core_net::CredGrant>,
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
pub(crate) fn format_verdict(fault: Fault, at_corrupt: u64) -> String {
    match fault {
        Fault::Diverged(d) => format!("replay: Diverged({:?}) at {}", d.kind, d.at),
        Fault::Truncated(at) => format!("replay: Truncated at {at}"),
        Fault::Damaged(block) => format!("replay: Damaged at block {block}"),
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

/// Only structural metadata and allowlisted scalar arguments cross the
/// replay diagnostic boundary. Frame payloads may contain credentials.
struct FrameEvidence<'a>(&'a Frame);

impl std::fmt::Display for FrameEvidence<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            Frame::Wait { gap, task }
            | Frame::Done { gap, task, .. }
            | Frame::Wake { gap, task }
            | Frame::Cancel { gap, task }
            | Frame::Retired { gap, task, .. } => write!(f, "task={task} gap={gap}"),
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
            Frame::Take { gap, count, .. } => write!(f, "gap={gap} count={count}"),
            Frame::HelperEnd { gap, ending } => {
                write!(f, "outcome_kind={} gap={gap}", ending.kind())
            }
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
            Frame::Retired { .. } => "RETIRED",
            Frame::Choose { .. } => "CHOOSE",
            Frame::Grant { .. } => "GRANT",
            Frame::Repeat { .. } => "REPEAT",
            Frame::Take { .. } => "TAKE",
            Frame::End { .. } | Frame::HelperEnd { .. } => "END",
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
        if let Some([spare, _]) = args.recorded_spare() {
            return write!(f, " {field}={spare}");
        }
        match args {
            Args::Literal(bytes) => match bytes.as_slice().try_into() {
                Ok(raw) => write!(f, " {field}={}", u64::from_le_bytes(raw)),
                Err(_) => write!(f, " {field}=invalid({} bytes)", bytes.len()),
            },
            _ => write!(f, " {field}=redacted"),
        }
    }
}

impl FrameReader {
    fn new(source: ProviderFile, blocks: StreamBlocks) -> Self {
        let mut reader = Self {
            source,
            blocks,
            retained_block: None,
            decoder: Decoder::default(),
            frames: Default::default(),
        };
        reader.keep_block_storage();
        reader
    }

    fn keep_block_storage(&mut self) {
        self.retained_block = RETENTION.retain(self.blocks.buffer_capacity());
        if self.retained_block.is_none() {
            self.blocks.discard_buffer();
        }
    }

    fn next(&mut self, at: u64) -> Result<Frame, Fault> {
        loop {
            if let Some(frame) = self.frames.next() {
                if self.frames.as_slice().is_empty() {
                    self.frames = Default::default();
                }
                return Ok(frame);
            }
            self.retained_block = None;
            let decoded = match self
                .blocks
                .next(|offset, bytes| self.source.read_at(offset, bytes))
            {
                Err(ReadError::Format(s)) => Err(Fault::stopped(s, at)),
                Err(ReadError::Source(e)) => Err(Fault::Io(e)),
                Ok(None) => Err(Fault::Diverged(Divergence {
                    at,
                    kind: Mismatch::Extra,
                })),
                Ok(Some((flags, payload))) => self
                    .decoder
                    .block(payload, flags & END != 0)
                    .map_err(Fault::from),
            };
            self.keep_block_storage();
            self.frames = decoded?.into_iter();
        }
    }
}

impl Replayer {
    fn peek(&mut self) -> Result<&Frame, Fault> {
        if self.lookahead.is_none() {
            self.lookahead = Some(self.next()?);
            self.taken -= 1;
        }
        Ok(self.lookahead.as_ref().unwrap())
    }

    fn next(&mut self) -> Result<Frame, Fault> {
        loop {
            let at = self.taken + 1;
            let frame = match self.lookahead.take() {
                Some(frame) => frame,
                None => self.reader.next(at)?,
            };
            self.taken = at;
            match CloseKey::record(&frame)? {
                Some(key) => self.closes.observe(key, at, frame, true)?,
                None => return Ok(frame),
            }
        }
    }

    fn close(&mut self, key: CloseKey) -> Result<Out, Fault> {
        while !self.closes.contains(key) && self.lookahead.is_none() {
            let at = self.taken + 1;
            let frame = self.reader.next(at)?;
            match CloseKey::record(&frame)? {
                Some(recorded) => {
                    self.taken = at;
                    self.closes.observe(recorded, at, frame, true)?;
                }
                None => self.lookahead = Some(frame),
            }
        }
        match self.closes.take(key)? {
            Some(out) => Ok(out),
            None => Err(self.diverged_next(Mismatch::Object)),
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
    Udp,
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
            | LOCAL_ADDR
            | PEER_ADDR
            | RECV_BOUNDED
            | RECV_READY
            | SEND_PAIR
            | TLS_CONNECT
            | TLS_HELLO
            | TLS_ACCEPT
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
            | MKSTEMP
            | LISTEN
            | CONNECT
            | ACCEPT
            | TLS_CONNECT
            | TLS_ACCEPT
            | SCRAM
            | EXEC_SPAWN
    )
}

fn waiting_io(op: u8) -> bool {
    socket_op(op) || unix_operation(op) || udp_operation(op) || matches!(op, STDIN_READ | EXEC_WAIT)
}

fn waiting_io_status(error: u8) -> u8 {
    match error {
        0 => OK,
        6 => TIMED_OUT,
        7 => ABORTED,
        _ => ERROR,
    }
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
        if request.operation == RESOLVE {
            let resolve = &*request.path.cast::<crate::resolve::ResolveRequest>();
            let name = buffer_bytes(&resolve.raw_name);
            put_varint(&mut args, name.len() as u64);
            args.extend_from_slice(name);
        }
        tls::args(request, &mut args);
        if let Some((_, clock, at)) = n.until {
            put_varint(&mut args, ordinal_of(clock) as u64);
            args.extend_from_slice(&at.to_le_bytes());
        } else {
            args.push(0);
        }
        if request.operation == SCRAM {
            let scram = &*request.path.cast::<crate::scram::ScramRequest<'_>>();
            let (origin, patterns) = &scram.send.designations[0];
            origin.put(&mut args);
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
                    origin.put(&mut args);
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

/// The canonical argument bytes of an operation (REPLAY-SPEC 4.1; the
/// per-operation choice is recorded in followups.md for section 5.4).
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
                let sink = match unsafe { crate::os::duplicate_file(&dir.provider, file.handle()) }
                {
                    Some(fd) => {
                        file.close().map_err(OpenError::Io)?;
                        Sink::Writer(writer::Stream::new(fd))
                    }
                    None => Sink::Provider(file),
                };
                let mut recorder = Recorder {
                    sink,
                    stream: 0,
                    spawns: 0,
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
                    takes: units::Takes::default(),
                    exec_rules: BTreeMap::new(),
                    typed_resources: BTreeMap::new(),
                    udp_retirements: BTreeMap::new(),
                    clocks: BTreeMap::new(),
                    grants: BTreeMap::new(),
                    sockets: BTreeMap::new(),
                    recorder: Some(recorder),
                    replayer: None,
                };
                (
                    live,
                    Startup {
                        arg: header.arg.clone(),
                        net_rules: vec![],
                        secret_grants: vec![],
                        cred_grants: vec![],
                        env: None,
                    },
                )
            }
            TapeInput::Replay(replay) => {
                let ReplayInput {
                    source,
                    blocks,
                    stored,
                    origin,
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
                    takes: units::Takes::default(),
                    exec_rules: BTreeMap::new(),
                    typed_resources: BTreeMap::new(),
                    udp_retirements: BTreeMap::new(),
                    clocks: BTreeMap::new(),
                    grants: BTreeMap::new(),
                    sockets: BTreeMap::new(),
                    recorder: None,
                    replayer: Some(Replayer {
                        reader: FrameReader::new(source, blocks),
                        closes: Closes::new(origin, stored.tape_id),
                        lookahead: None,
                        taken: 0,
                        comparison: None,
                        pending_reads: BTreeMap::new(),
                    }),
                };
                (
                    live,
                    Startup {
                        arg: stored.arg,
                        net_rules: stored.net_rules,
                        secret_grants: stored.secret_grants,
                        cred_grants: stored.cred_grants,
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
        let rules = exec_snapshot_rules(&stored)?;
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
        crate::tasks::before_tape_frame(self.group(), false);
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
                            | Frame::Retired { .. }
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
        if live.mode == Mode::Record {
            live.grants
                .insert(live.objects.len(), (kind, stored.clone()));
        }
        let h = handle(generation, live.objects.len());
        Ok((self.cell.provider, h, stored))
    }

    /// `tape.end`: Record writes the END block with the domain's outcome
    /// `ending`; Replay requires END next, with that outcome (else
    /// `Diverged(Outcome)` at the END frame), and nothing after it. Either
    /// way the tape's handles end here.
    pub fn end(self, ending: &Ending) -> Result<(), Fault> {
        crate::tasks::before_tape_frame(self.group(), true);
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
        crate::tasks::before_tape_frame(self.group(), true);
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
        && (!live.waiting.is_empty()
            || !live.pending.is_empty()
            || live
                .replayer
                .as_ref()
                .is_some_and(|rep| !rep.pending_reads.is_empty()))
    {
        result = Err(Fault::Diverged(Divergence {
            at: live.replayer.as_ref().map_or(1, |r| r.taken + 1),
            kind: Mismatch::Schedule,
        }));
    }
    let mut truncated = None;
    if let Some(rec) = live.recorder.as_mut() {
        if !rec.truncated && rec.failed.is_none() {
            let gap = live.schedule.events - live.schedule.last;
            if rec.stream != 0 && deadlock.is_some() {
                let _ = rec.frame(&helper::deadlock(gap, live.schedule.events));
            } else if let Some(e) = end {
                let _ = rec.frame(&Frame::End {
                    gap,
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
            Ok(Frame::End { .. }) => rep.closes.finish().and_then(|()| {
                rep.reader.next(rep.taken + 1).map_or_else(
                    |f| match f {
                        Fault::Diverged(Divergence {
                            kind: Mismatch::Extra,
                            ..
                        }) => Ok(()),
                        f => Err(f),
                    },
                    |_| Err(Fault::Corrupt),
                )
            }),
            Ok(frame) => {
                let kind = if deadlock.is_some()
                    || matches!(
                        frame,
                        Frame::Wake { .. }
                            | Frame::Cancel { .. }
                            | Frame::Retired { .. }
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
        let closed = rep.reader.source.close().map_err(Fault::Io);
        result = result.and(closed);
    }
    if let Err(f) = result {
        live.abort_replay(f, 0);
    }
    if deadlock.is_some() && result.is_ok() && live.replay_abort.is_some() {
        // Recorded deadlock matched and sealed on replay: exit 0.
        crate::coverage::exiting(0);
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

/// A first non-ready answer opens W explicitly. Subsequent callbacks keep
/// that WAIT open until the scheduler records WAKE, or cancellation closes W.
unsafe extern "C" fn wait(state: *mut u8, r: *mut Request, token: u64) -> Wait {
    // TLS sinks have only immediate frames in both modes.
    if secret::inline_sink(unsafe { (*r).operation }).is_some() {
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
        if is_close(unsafe { (*r).operation }) {
            return Wait::default();
        }
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
        let w = if denial(&*r).is_some() {
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
    let close = is_close(r.operation)
        && !(r.operation == CLOSE && r.mode == crate::udp_net::record::CLOSE_MODE);
    if !close && schedule::member(cell, generation_of(h)) {
        crate::tasks::before_commit();
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
    let task = if close { 0 } else { schedule::task(cell) };
    // SAFETY: forwarded.
    let args = unsafe { canonical(r) };
    // Dropping a handle does not contribute a scheduling event.
    if !close {
        let position = live.schedule.events;
        live.commit(task, crate::tasks::group_index);
        live.schedule.last = position;
    }
    if live.mode == Mode::Replay {
        // SAFETY: output is written only into the request's spare carrier.
        let step = if close {
            replay_close(live, generation, ordinal as u64, r, &args)
        } else {
            unsafe { replay(live, generation, ordinal, r, &args, token) }
        };
        return match step {
            Ok(step) => step,
            Err(f) => {
                live.fault = Some(refuse(f));
                live.abort_replay(f, r.site);
                done(REFUSED)
            }
        };
    }
    let inner = secret::inline_sink(r.operation)
        .map(|provider| Inner {
            provider,
            handle: 0,
        })
        .unwrap_or_else(|| live.objects[ordinal - 1].inner.unwrap());
    let before = r.buffer.len;
    drop(s);
    // SAFETY: address operations pin their permission result through delivery.
    let denied = unsafe { denial(r) };
    let step = if let Some(tag) = denied {
        done(crate::os_net::signal(tag))
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
    if unsafe { secret::designation_delivery(r) }
        && outcome.error == IoError::OK
        && outcome.status == 0
    {
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
    if !close {
        live.schedule.events += 1;
    }
    if denied.is_none() {
        s.delivering.insert(key, delivery);
    }
    done(answer)
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
            } else if stored.operands() != args.operands() {
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
    let recorded = match &frame {
        Frame::Op { args, .. } | Frame::Issue { args, .. } => args.recorded_spare(),
        _ => None,
    };
    if !matches!(op, CRED_FETCH | UDP_RECV) && recorded.is_some_and(|spare| spare[1] != 0) {
        return Err(Fault::Corrupt);
    }
    let space = ReadSpace::for_issue(recorded, args.recorded_spare(), rep.taken)?;
    let Frame::Op { out, .. } = frame else {
        if let Some(space) = space {
            rep.pending_reads.insert(r as *mut Request as usize, space);
        }
        if secret::inline_sink(op).is_some()
            || matches!(op, EXEC_SPAWN | EXEC_SIGNAL)
            || (unix_operation(op) && !unix::can_park(op))
            || (udp_operation(op) && !matches!(op, UDP_RECV | UDP_SEND))
            || (op == CLOSE
                && matches!(
                    r.mode,
                    crate::exec_runtime::PROC_CLOSE | 0x40
                        ..=0x42 | crate::udp_net::record::CLOSE_MODE
                ))
        {
            return Err(Fault::Corrupt);
        }
        // Without a scheduler nothing else readies a replayed helper, and
        // nothing else runs before its completion. Ready it only when its
        // recorded Done comes next, as `complete` will accept; any other
        // frame is the divergence `complete` would report.
        if (matches!(
            op,
            SYNC | SYNC_DIR | RESOLVE | CRED_FETCH | STDIN_READ | EXEC_WAIT | UDP_RECV | UDP_SEND
        ) || unix::can_park(op))
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
    unsafe { replay_out(live, generation, op, object, r, out, space).map(done) }
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
                let space = rep.pending_reads.remove(&key);
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
                        space,
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
