//! Tapes: Record and Replay of provider operations (REPLAY-SPEC v2 slice S1).
//!
//! A tape is a provider over the four-state ABI (section 3). `on_tape` turns a
//! capability's (provider, handle) into a substitute: the tape's provider with
//! a handle `(generation << 32) | ordinal`, which the run registers as a
//! dynamic grant like any other (host_file.rs). Every operation on it, and on
//! every HostFile or Dir made through it, reaches the tape at issue.
//!
//! Record forwards each operation to the substitute's inner provider and
//! writes one OP frame (section 4.1: nothing comes between an S1 operation's
//! issue and its delivery). Replay holds no inner provider at all: it answers
//! each issue from the next frame after comparing op, object and arguments,
//! so no operation under Replay can reach the host.
//!
//! The tape's state lives in a process slot table (section 8). Ending or
//! dropping a tape advances its cell's generation, so every handle made
//! through it then fails before issue and never uses its source.
pub mod format;
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

/// Section 4.2's divergence kinds that S1 can produce.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Divergence {
    Op,
    Object,
    Arg,
    Missing,
    Extra,
}

/// Why a tape stopped. Every later operation on it fails the same way.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Fault {
    Diverged(Divergence),
    Truncated,
    Damaged(u64),
    Corrupt,
    /// The sink's write failed (S1 has no barriers; S3 poisons instead).
    Io(IoError),
    /// The tape was ended or dropped (section 8).
    Ended,
}
impl From<Stop> for Fault {
    fn from(s: Stop) -> Self {
        match s {
            Stop::Missing => Fault::Diverged(Divergence::Missing),
            Stop::Truncated => Fault::Truncated,
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
    Build,
    Arg,
    Limit,
    Damaged(u64),
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
    pub limit: u64,
    /// None: the opener receives the stored argument.
    pub arg: Option<&'a [u8]>,
}

/// An operation that the tape refused: the task fails before issue.
pub const REFUSED: Outcome = Outcome {
    value: 0,
    error: IoError::OK,
    written: 0,
    status: 1,
};

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
    sink: Inner,
    tape_id: [u8; 16],
    encoder: Encoder,
    buffer: Vec<u8>,
    seq: u64,
}

struct Replayer {
    bytes: Vec<u8>,
    blocks: Blocks,
    decoder: Decoder,
    frames: VecDeque<Frame>,
}

struct Live {
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
unsafe fn write_all(p: Inner, bytes: &[u8]) -> Result<(), IoError> {
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
    // SAFETY: forwarded.
    let o = unsafe { sink(p, r) };
    failed(&o).map_or(Ok(()), Err)
}

impl Recorder {
    /// Append one frame and its DATA continuations, closing the block at
    /// 64 KiB between frames, never inside one.
    unsafe fn frame(&mut self, f: &Frame) -> Result<(), IoError> {
        let (head, rest) = self.encoder.frame(f);
        self.buffer.extend_from_slice(&head);
        for d in rest {
            if self.buffer.len() >= CLOSE_AT {
                // SAFETY: forwarded.
                unsafe { self.close(0)? };
            }
            self.buffer.extend_from_slice(&d);
        }
        if self.buffer.len() >= CLOSE_AT {
            // SAFETY: forwarded.
            unsafe { self.close(0)? };
        }
        Ok(())
    }
    unsafe fn close(&mut self, flags: u8) -> Result<(), IoError> {
        let b = block(&self.tape_id, flags, self.seq, &self.buffer);
        self.seq += 1;
        self.buffer.clear();
        self.encoder.new_block();
        // SAFETY: forwarded.
        unsafe { write_all(self.sink, &b) }
    }
    unsafe fn close_file(&mut self) -> Result<(), IoError> {
        // SAFETY: forwarded.
        let o = unsafe { sink(self.sink, request(CLOSE, 0, 0, &[], empty_buffer())) };
        failed(&o).map_or(Ok(()), Err)
    }
}

impl Replayer {
    fn next(&mut self) -> Result<Frame, Fault> {
        loop {
            if let Some(f) = self.frames.pop_front() {
                return Ok(f);
            }
            match self.blocks.next(&self.bytes)? {
                None => return Err(Fault::Diverged(Divergence::Extra)),
                Some((flags, payload)) => {
                    let frames = self.decoder.block(payload, flags & END != 0)?;
                    self.frames.extend(frames);
                }
            }
        }
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

unsafe extern "C" {
    /// The executable's key, defined by the driver in every linked program's
    /// entry object (REPLAY-SPEC v2.1 R1). There is no fallback: a program
    /// without it does not link.
    safe static hirsel_build_identity: [u8; 32];
}

impl Tape {
    /// Open a tape at `name` in the Dir `dir` (its provider and handle).
    /// Record creates the file (CreateNew) and writes block 0 from `header`;
    /// Replay reads it and refuses, in this order, a changed identity, build,
    /// argument (when `expect` passes one) or limit. `header.build` is not
    /// the caller's: open fills it from the running executable.
    /// Returns the tape and the stored argument.
    /// # Safety
    /// Called in a task of the run that owns `dir`'s provider.
    pub unsafe fn open(
        dir: (Provider, u64),
        name: &[u8],
        mode: Mode,
        header: &Header,
        expect: &Expect,
    ) -> Result<(Tape, Vec<u8>), OpenError> {
        // SAFETY: forwarded.
        unsafe { Self::open_built(hirsel_build_identity, dir, name, mode, header, expect) }
    }
    /// `open` as an executable whose key is `build`.
    /// # Safety
    /// As `open`.
    pub(crate) unsafe fn open_built(
        build: [u8; 32],
        dir: (Provider, u64),
        name: &[u8],
        mode: Mode,
        header: &Header,
        expect: &Expect,
    ) -> Result<(Tape, Vec<u8>), OpenError> {
        let header = &Header {
            build,
            ..header.clone()
        };
        let dir = Inner {
            provider: dir.0,
            handle: dir.1,
        };
        let (live, arg) = match mode {
            Mode::Record => {
                // SAFETY: forwarded.
                let o = unsafe { sink(dir, request(OPEN, 3, 0, name, empty_buffer())) };
                if let Some(e) = failed(&o) {
                    return Err(OpenError::Io(e));
                }
                let mut recorder = Recorder {
                    sink: Inner {
                        provider: dir.provider,
                        handle: o.value,
                    },
                    tape_id: header.tape_id,
                    encoder: Encoder::default(),
                    buffer: header.encode(),
                    seq: 0,
                };
                // SAFETY: forwarded.
                unsafe { recorder.close(0) }.map_err(OpenError::Io)?;
                let live = Live {
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
                let bytes = unsafe { read_file(dir, name) }.map_err(OpenError::Io)?;
                let (blocks, stored) = match Blocks::open(&bytes) {
                    Ok(b) => b,
                    Err(Stop::Damaged(k)) => {
                        return Err(OpenError::Refused(Refusal::Damaged(k)));
                    }
                    Err(s) => return Err(OpenError::Unreadable(s.into())),
                };
                if stored.identity != expect.identity {
                    return Err(OpenError::Refused(Refusal::Code));
                }
                if stored.build != header.build {
                    return Err(OpenError::Refused(Refusal::Build));
                }
                if expect.arg.is_some_and(|a| a != stored.arg) {
                    return Err(OpenError::Refused(Refusal::Arg));
                }
                if stored.limit != expect.limit {
                    return Err(OpenError::Refused(Refusal::Limit));
                }
                let live = Live {
                    mode,
                    fault: None,
                    objects: vec![],
                    clocks: BTreeMap::new(),
                    recorder: None,
                    replayer: Some(Replayer {
                        bytes,
                        blocks,
                        decoder: Decoder::default(),
                        frames: VecDeque::new(),
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
        Ok((Tape { cell, generation }, arg))
    }

    /// `std.<kind>.on_tape`: the substitute for the capability `source`, as
    /// (provider, provider-local handle). Record writes `GRANT{kind}`;
    /// Replay checks it is the next frame.
    pub fn on_tape(&self, kind: u8, source: (Provider, u64)) -> Result<(Provider, u64), Fault> {
        self.grant(kind, None, Some(source)).map(|(p, h, _)| (p, h))
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
            return Err(Fault::Ended);
        }
        let generation = s.generation;
        let live = s.live.as_mut().unwrap();
        if let Some(f) = live.fault {
            return Err(f);
        }
        let stored = match live.mode {
            Mode::Record => {
                let rec = live.recorder.as_mut().unwrap();
                let frame = Frame::Grant {
                    kind,
                    args: args.clone(),
                };
                // SAFETY: called in a task of the tape's run.
                if let Err(e) = unsafe { rec.frame(&frame) } {
                    live.fault = Some(Fault::Io(e));
                    return Err(Fault::Io(e));
                }
                args.unwrap_or_default()
            }
            Mode::Replay => {
                let rep = live.replayer.as_mut().unwrap();
                let stored = match rep.next() {
                    Ok(Frame::Grant { kind: k, args }) if k == kind => Ok(args.unwrap_or_default()),
                    Ok(Frame::End(_)) => Err(Fault::Diverged(Divergence::Missing)),
                    Ok(_) => Err(Fault::Diverged(Divergence::Op)),
                    Err(f) => Err(f),
                };
                stored.inspect_err(|&f| live.fault = Some(f))?
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

    /// `tape.end`: Record writes the END block and closes the file; Replay
    /// requires END next and nothing after it. Either way the tape's handles
    /// end here.
    pub fn end(self) -> Result<(), Fault> {
        let result = self.finish(true);
        std::mem::forget(self);
        result
    }
    fn finish(&self, end: bool) -> Result<(), Fault> {
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
        if let Some(rec) = live.recorder.as_mut() {
            // SAFETY: end and drop run in a task of the tape's run.
            let written = unsafe {
                if end {
                    rec.frame(&Frame::End(END_COMPLETE))
                        .and_then(|()| rec.close(format::END))
                } else if rec.buffer.is_empty() {
                    Ok(())
                } else {
                    rec.close(0)
                }
            };
            // SAFETY: as above.
            let closed = unsafe { rec.close_file() };
            if result.is_ok() {
                result = written.and(closed).map_err(Fault::Io);
            }
        }
        if let (true, Ok(()), Some(rep)) = (end, result, live.replayer.as_mut()) {
            result = match rep.next() {
                Ok(Frame::End(_)) => rep.next().map_or_else(
                    |f| {
                        if f == Fault::Diverged(Divergence::Extra) {
                            Ok(())
                        } else {
                            Err(f)
                        }
                    },
                    |_| Err(Fault::Corrupt),
                ),
                Ok(_) => Err(Fault::Diverged(Divergence::Extra)),
                Err(f) => Err(f),
            };
        }
        FREE.with_borrow_mut(|f| f.push(self.cell));
        result
    }
}
impl Drop for Tape {
    fn drop(&mut self) {
        let _ = self.finish(false);
    }
}

unsafe fn read_file(dir: Inner, name: &[u8]) -> Result<Vec<u8>, IoError> {
    // SAFETY: the sink runs in the opener's task.
    unsafe {
        let o = sink(dir, request(OPEN, 0, 0, name, empty_buffer()));
        if let Some(e) = failed(&o) {
            return Err(e);
        }
        let file = Inner {
            provider: dir.provider,
            handle: o.value,
        };
        let mut bytes: Vec<u8> = Vec::with_capacity(64 * 1024);
        let result = loop {
            if bytes.len() == bytes.capacity() {
                bytes.reserve(bytes.capacity());
            }
            let mut r = request(
                READ,
                0,
                0,
                &[],
                Buffer {
                    data: bytes.as_mut_ptr(),
                    len: bytes.len(),
                    capacity: bytes.capacity(),
                },
            );
            r.path = ptr::null();
            let o = sink(file, r);
            if let Some(e) = failed(&o) {
                break Err(e);
            }
            if o.value == 0 {
                break Ok(());
            }
            bytes.set_len(bytes.len() + o.value as usize);
        };
        let o = sink(file, request(CLOSE, 0, 0, &[], empty_buffer()));
        result?;
        failed(&o).map_or(Ok(bytes), Err)
    }
}

fn cell_of<'a>(state: *mut u8) -> &'a Cell {
    // SAFETY: a tape provider's state is its leaked cell.
    unsafe { &*state.cast::<Cell>() }
}
fn generation_of(h: u64) -> u32 {
    (h >> 32) as u32
}
fn ordinal_of(h: u64) -> usize {
    (h & 0xffff_ffff) as usize
}

/// Record forwards the wait to the inner provider (a sleep waits on the host
/// clock); Replay, an ended tape and a stopped tape are Ready at once.
unsafe extern "C" fn wait(state: *mut u8, r: *mut Request, token: u64) -> Wait {
    let cell = cell_of(state);
    // SAFETY: the driver's pinned request.
    let h = unsafe { (*r).handle };
    let inner = {
        let s = cell.state.lock().unwrap();
        match &s.live {
            Some(live)
                if s.generation == generation_of(h)
                    && live.fault.is_none()
                    && live.mode == Mode::Record =>
            {
                live.objects[ordinal_of(h) - 1].inner
            }
            _ => None,
        }
    };
    let Some(inner) = inner else {
        return Wait::default();
    };
    // SAFETY: forwarded to the source with its own handle, restored after.
    unsafe {
        (*r).handle = inner.handle;
        let w = (inner.provider.wait)(inner.provider.state, r, token);
        (*r).handle = h;
        w
    }
}

unsafe extern "C" fn issue(state: *mut u8, r: *mut Request, token: u64) -> Step {
    let cell = cell_of(state);
    // SAFETY: the driver's pinned request, in the task.
    let outcome = unsafe { issue_in(cell, &mut *r, token) };
    Step {
        kind: STEP_DONE,
        outcome,
        deadline_ns: 0,
    }
}

unsafe fn issue_in(cell: &Cell, r: &mut Request, token: u64) -> Outcome {
    let h = r.handle;
    let mut s = cell.state.lock().unwrap();
    if s.generation != generation_of(h) || s.live.is_none() {
        // Section 8: fails before issue. Cleaning a Record HostFile closes the
        // inner live file, with no frame; Replay has none.
        let orphan = (r.operation == CLOSE)
            .then(|| s.orphans.remove(&h))
            .flatten();
        drop(s);
        if let Some(inner) = orphan {
            // SAFETY: the inner file is still the run's; nothing records it.
            let _ = unsafe { sink(inner, request(CLOSE, 0, 0, &[], empty_buffer())) };
        }
        return REFUSED;
    }
    let generation = s.generation;
    let live = s.live.as_mut().unwrap();
    if live.fault.is_some() {
        return REFUSED;
    }
    let ordinal = ordinal_of(h);
    // SAFETY: forwarded.
    let args = unsafe { canonical(r) };
    match live.mode {
        Mode::Record => {
            let inner = live.objects[ordinal - 1].inner.unwrap();
            drop(s);
            let before = r.buffer.len;
            r.handle = inner.handle;
            // SAFETY: the source's own issue, in the task, with its handle.
            let step = unsafe { (inner.provider.issue)(inner.provider.state, r, token) };
            r.handle = h;
            if step.kind != STEP_DONE {
                // An operation in I is recorded with positions, slice S2.
                crate::process::stop(b"TapePending");
            }
            let o = step.outcome;
            let mut s = cell.state.lock().unwrap();
            let live = s.live.as_mut().unwrap();
            let ok = o.status == 0 && o.error == IoError::OK;
            let mut out = Out {
                status: if o.error == IoError::OK { OK } else { ERROR },
                value: o.value,
                error: (o.error.tag, u64::from(o.error.code)),
                written: o.written,
                payload: vec![],
            };
            let mut created = None;
            let mut answer = o;
            if ok && matches!(r.operation, OPEN | MKDTEMP) {
                live.objects.push(Object {
                    inner: Some(Inner {
                        provider: inner.provider,
                        handle: o.value,
                    }),
                    open_file: r.operation == OPEN,
                });
                let n = live.objects.len();
                out.value = n as u64;
                answer.value = handle(generation, n);
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
                // SAFETY: the source wrote these bytes into the pinned carrier.
                out.payload = unsafe {
                    std::slice::from_raw_parts(r.buffer.data.add(before), r.buffer.len - before)
                }
                .to_vec();
            }
            let frame = Frame::Op {
                task: 0,
                op: r.operation,
                object: ordinal as u64,
                args: Args::of(&args),
                out,
            };
            let rec = live.recorder.as_mut().unwrap();
            // SAFETY: in the task.
            if let Err(e) = unsafe { rec.frame(&frame) } {
                live.fault = Some(Fault::Io(e));
            }
            s.delivering.insert(
                r as *mut Request as usize,
                Delivery {
                    inner,
                    outcome: o,
                    created,
                },
            );
            answer
        }
        Mode::Replay => {
            // SAFETY: forwarded.
            match unsafe { replay(live, generation, ordinal, r, &args) } {
                Ok(o) => o,
                Err(f) => {
                    live.fault = Some(f);
                    REFUSED
                }
            }
        }
    }
}

/// Answer one issue from the next frame (section 4.2's issue check first).
unsafe fn replay(
    live: &mut Live,
    generation: u32,
    ordinal: usize,
    r: &mut Request,
    args: &[u8],
) -> Result<Outcome, Fault> {
    let rep = live.replayer.as_mut().unwrap();
    let (op, object, stored, out) = match rep.next()? {
        Frame::Op {
            op,
            object,
            args,
            out,
            ..
        } => (op, object, args, out),
        Frame::Issue {
            task,
            op,
            object,
            args,
        } => match rep.next()? {
            Frame::Done { task: t, out, .. } if t == task => (op, object, args, out),
            _ => return Err(Fault::Diverged(Divergence::Op)),
        },
        Frame::End(_) => return Err(Fault::Diverged(Divergence::Missing)),
        _ => return Err(Fault::Diverged(Divergence::Op)),
    };
    if op != r.operation {
        return Err(Fault::Diverged(Divergence::Op));
    }
    if object != ordinal as u64 {
        return Err(Fault::Diverged(Divergence::Object));
    }
    if stored != Args::of(args) {
        return Err(Fault::Diverged(Divergence::Arg));
    }
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

unsafe extern "C" fn resume(_: *mut u8, _: *mut Request, _: u64) -> Step {
    unreachable!("a tape answers every S1 issue at once")
}
unsafe extern "C" fn abort(_: *mut u8, _: *mut Request, _: u64, _: u8) -> Outcome {
    unreachable!("a tape answers every S1 issue at once")
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
