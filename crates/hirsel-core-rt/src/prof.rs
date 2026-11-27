//! Bounded per-thread chunks for passive runtime observation.
use crate::tasks::State;
use hirsel_observation_format::{END, HEADER_BYTES, LOSS, MAGIC, RECORD_BYTES, payload_bytes};
use std::cell::{RefCell, UnsafeCell};
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const DEFAULT_CHUNK_BYTES: usize = RECORD_BYTES;
const LIVE_MEMORY_CHUNK_BYTES: usize = 512 * 1024;
const RSS_INTERVAL_NS: u64 = 10_000_000;
const MAX_CHUNK_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy)]
#[repr(u16)]
pub(crate) enum Kind {
    Task = 1,
    State = 2,
    DispatchBegin = 3,
    DispatchEnd = 4,
    Wake = 5,
    HostBegin = 6,
    HostEnd = 7,
    IoBegin = 8,
    IoCallback = 9,
    IoPending = 10,
    IoEnd = 11,
    Mapping = 12,
    /// A sampled thread's stack: its thread id, the period in nanoseconds,
    /// flags (bit 0: truncated), the interrupted PC and each return address.
    CpuSample = 13,
    /// The loaded-code frames of the sample before it (PACK-9): its thread id, then
    /// for each frame an image owns, its index in the sample's addresses, its offset
    /// in the image's text and the two words of the image's digest prefix.
    CodeFrames = 14,
    Alloc = 16,
    Peak = 17,
    BudgetScope = 18,
    ScopeStack = 19,
    MemoryMeta = 20,
    SlackMember = 21,
    ResidentSnapshot = 22,
    ResidentMember = 23,
}

thread_local! {
    static WRITER: RefCell<Option<Writer>> = const { RefCell::new(None) };
}
static RUN: Mutex<Option<Arc<Shared>>> = Mutex::new(None);

struct Chunk {
    data: UnsafeCell<Box<[MaybeUninit<u8>]>>,
    /// Zero gives the producer custody. Publishing a length passes custody to
    /// the drain thread until it has copied the prefix and reset the length.
    len: AtomicUsize,
}

// SAFETY: after publishing len the one producer leaves the bytes untouched
// until the one consumer hands them back with a release store of zero.
unsafe impl Sync for Chunk {}

impl Chunk {
    fn new(bytes: usize) -> Result<Self, std::collections::TryReserveError> {
        let mut data = Vec::new();
        data.try_reserve_exact(bytes)?;
        data.resize_with(bytes, MaybeUninit::uninit);
        Ok(Self {
            data: UnsafeCell::new(data.into_boxed_slice()),
            len: AtomicUsize::new(0),
        })
    }

    fn drain(&self, output: &mut impl Write) -> io::Result<bool> {
        let len = self.len.load(Ordering::Acquire);
        if len != 0 {
            // SAFETY: the acquire takes the published prefix before releasing
            // this chunk back to the producer.
            output.write_all(unsafe {
                std::slice::from_raw_parts((&*self.data.get()).as_ptr().cast::<u8>(), len)
            })?;
            self.len.store(0, Ordering::Release);
        }
        Ok(len != 0)
    }
}

struct Thread {
    chunks: [Chunk; 2],
    dead: AtomicBool,
    lost: AtomicU64,
    memory_emitted: AtomicU64,
    tid: u32,
}

impl Thread {
    fn new(tid: u32, bytes: usize) -> Result<Self, std::collections::TryReserveError> {
        Ok(Self {
            chunks: [Chunk::new(bytes)?, Chunk::new(bytes)?],
            dead: AtomicBool::new(false),
            lost: 0.into(),
            memory_emitted: 0.into(),
            tid,
        })
    }
}

struct Writer {
    thread: Arc<Thread>,
    index: usize,
    used: usize,
    capacity: usize,
    tasks: bool,
    lost: u64,
    memory_emitted: u64,
}

impl Writer {
    fn new(thread: Arc<Thread>, capacity: usize, tasks: bool) -> Self {
        Self {
            thread,
            index: 0,
            used: 0,
            capacity,
            tasks,
            lost: 0,
            memory_emitted: 0,
        }
    }

    fn seal(&mut self) {
        if self.used != 0 {
            let chunk = &self.thread.chunks[self.index];
            chunk.len.store(self.used, Ordering::Release);
            self.used = 0;
            self.index ^= 1;
        }
    }

    fn append(&mut self, kind: Kind, words: &[u64], bytes: &[u8]) {
        self.memory_emitted += u64::from(kind as u16 >= Kind::Alloc as u16);
        if let (Ok(words_len), Ok(bytes_len), Some(payload)) = (
            u16::try_from(words.len()),
            u32::try_from(bytes.len()),
            payload_bytes(words.len(), bytes.len()),
        ) {
            let needed = HEADER_BYTES + payload;
            if needed <= self.capacity.min(RECORD_BYTES) {
                if needed > self.capacity - self.used {
                    self.seal();
                }
                let chunk = &self.thread.chunks[self.index];
                if self.used != 0 || chunk.len.load(Ordering::Acquire) == 0 {
                    // SAFETY: this writer is the only producer and owns this
                    // chunk until seal gives custody to the drain thread.
                    let data = unsafe { &mut *chunk.data.get() };
                    let dst = &mut data[self.used..self.used + needed];
                    dst[..HEADER_BYTES].write_copy_of_slice(&header(
                        now_ns(),
                        self.thread.tid,
                        kind as u16,
                        words_len,
                        bytes_len,
                    ));
                    for (dst, word) in dst[HEADER_BYTES..].chunks_exact_mut(8).zip(words) {
                        dst.write_copy_of_slice(&word.to_le_bytes());
                    }
                    dst[HEADER_BYTES + words.len() * 8..].write_copy_of_slice(bytes);
                    self.used += needed;
                    return;
                }
            }
        }
        self.lost = self.lost.saturating_add(1);
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        self.seal();
        self.thread.lost.store(self.lost, Ordering::Relaxed);
        self.thread
            .memory_emitted
            .store(self.memory_emitted, Ordering::Relaxed);
        self.thread.dead.store(true, Ordering::Release);
    }
}

fn header(ts: u64, tid: u32, kind: u16, words: u16, bytes: u32) -> [u8; HEADER_BYTES] {
    let mut header = [0; HEADER_BYTES];
    header[..8].copy_from_slice(&ts.to_le_bytes());
    header[8..12].copy_from_slice(&tid.to_le_bytes());
    header[12..14].copy_from_slice(&kind.to_le_bytes());
    header[14..16].copy_from_slice(&words.to_le_bytes());
    header[16..].copy_from_slice(&bytes.to_le_bytes());
    header
}

pub(crate) fn enabled() -> bool {
    WRITER.with(|slot| slot.borrow().is_some())
}

pub(crate) fn now_ns() -> u64 {
    crate::os::monotonic_ns()
}

pub(crate) fn append(kind: Kind, words: &[u64], bytes: &[u8]) {
    WRITER.with(|slot| {
        if let Some(writer) = slot.borrow_mut().as_mut()
            && (writer.tasks
                || matches!(kind, Kind::CpuSample | Kind::CodeFrames)
                || kind as u16 >= 16)
        {
            writer.append(kind, words, bytes);
        }
    });
}

pub(super) fn task_state([unit, id]: [u64; 2], state: State, reason: Reason, object: u64) {
    let tag = match state {
        State::Ready => 0,
        State::Running => 1,
        State::Waiting | State::ChannelDeadline | State::CellChannelDeadline => 2,
        State::Provider => 3,
        State::Ended => 4,
    };
    append(Kind::State, &[unit, id, tag, reason as u64, object], &[]);
}

struct Shared {
    tasks: bool,
    chunk_bytes: usize,
    live_memory: bool,
    /// Workers publish their buffers at startup. Hooks access only TLS.
    new_threads: Mutex<Vec<Arc<Thread>>>,
    done: AtomicBool,
}

/// Register before executing tasks. Nested schedulers reuse the same thread's
/// chunks. The guard closes that thread's final chunk outside program code.
pub(crate) struct Worker(bool);

impl Worker {
    pub(crate) fn enter() -> Self {
        if enabled() {
            return Self(false);
        }
        let Some(shared) = RUN.lock().unwrap().clone() else {
            return Self(false);
        };
        let tid = crate::host::sys::thread_id() as u32;
        let (thread, capacity) = match Thread::new(tid, shared.chunk_bytes) {
            Ok(thread) => (thread, shared.chunk_bytes),
            Err(_) => (Thread::new(tid, 0).unwrap(), 0),
        };
        let slot = Arc::new(thread);
        shared.new_threads.lock().unwrap().push(slot.clone());
        WRITER.set(Some(Writer::new(slot, capacity, shared.tasks)));
        Self(true)
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        if self.0 {
            WRITER.set(None);
        }
    }
}

pub(crate) struct Run {
    shared: Arc<Shared>,
    drain: Option<JoinHandle<io::Result<()>>>,
    sampler: Option<(Arc<AtomicBool>, JoinHandle<()>)>,
}

/// One stack a host sampler walked: the interrupted PC, then each return
/// address, in a buffer reused across samples so that no walk allocates.
pub(crate) struct Stack {
    pub(crate) tid: u64,
    /// The chain was longer than [`Stack::DEPTH`].
    pub(crate) truncated: bool,
    pub(crate) len: usize,
    pub(crate) pcs: [u64; Stack::DEPTH],
}

impl Stack {
    pub(crate) const DEPTH: usize = 256;
}

/// Sample this process's running threads `hz` times a second as
/// [`Kind::CpuSample`] records until `stop`.
fn sample_cpu(hz: u64, stop: &AtomicBool) {
    let mut sampler = match crate::host::prof_sampler::Sampler::new() {
        Ok(sampler) => sampler,
        Err(error) => {
            eprintln!("hirsel: cannot sample CPU: {error}");
            return;
        }
    };
    let _worker = Worker::enter();
    let interval = Duration::from_nanos(1_000_000_000 / hz);
    let mut stack = Box::new(Stack {
        tid: 0,
        truncated: false,
        len: 0,
        pcs: [0; Stack::DEPTH],
    });
    let mut words = vec![0u64; 3 + Stack::DEPTH];
    let mut frames = Vec::with_capacity(1 + 4 * Stack::DEPTH);
    let mut last = now_ns();
    while !stop.load(Ordering::Acquire) {
        thread::sleep(interval);
        let now = now_ns();
        let period = now - last;
        last = now;
        sampler.round(&mut stack, &mut |stack| {
            words[0] = stack.tid & u64::from(u32::MAX);
            words[1] = period;
            words[2] = u64::from(stack.truncated);
            words[3..3 + stack.len].copy_from_slice(&stack.pcs[..stack.len]);
            append(Kind::CpuSample, &words[..3 + stack.len], &[]);
            // The pair the PC table gives now: the image may be gone when the
            // profile is read.
            frames.clear();
            frames.push(words[0]);
            if crate::code::frames(&stack.pcs[..stack.len], &mut frames) > 0 {
                append(Kind::CodeFrames, &frames, &[]);
            }
        });
    }
}

impl Run {
    #[cfg(feature = "record")]
    pub(crate) fn start_replay() -> Option<Self> {
        Self::start_with(true)
    }

    /// Call before taking the environment grant snapshot. The generated
    /// process entry owns access to the environment at that point.
    pub(crate) fn start() -> Option<Self> {
        Self::start_with(false)
    }

    fn start_with(replay: bool) -> Option<Self> {
        let path = std::env::var_os("HIRSEL_PROFILE")?;
        let size = std::env::var_os("HIRSEL_PROFILE_CHUNK_BYTES");
        let memory = crate::mem_prof::read_settings();
        if !replay && memory.as_ref().is_some_and(|settings| !settings.sampled) {
            eprintln!(
                "hirsel: a live memory profile is sampled; record a tape and use 'hirsel prof replay --mem=exact'"
            );
            return None;
        }
        let tasks = std::env::var_os("HIRSEL_PROFILE_TASKS").is_none_or(|value| value != "0");
        let hz = std::env::var_os("HIRSEL_PROFILE_CPU_HZ");
        // SAFETY: the generated executable calls this at process entry.
        unsafe {
            std::env::remove_var("HIRSEL_PROFILE");
            std::env::remove_var("HIRSEL_PROFILE_CHUNK_BYTES");
            std::env::remove_var("HIRSEL_PROFILE_TASKS");
            std::env::remove_var("HIRSEL_PROFILE_CPU_HZ");
        }
        let hz = match hz {
            None => None,
            Some(value) => match value.to_str().and_then(|text| text.parse::<u64>().ok()) {
                Some(hz @ 1..=1_000_000) if !replay => Some(hz),
                _ => {
                    eprintln!(
                        "hirsel: HIRSEL_PROFILE_CPU_HZ requires 1 to 1000000 samples a second"
                    );
                    return None;
                }
            },
        };
        let live_memory = !replay && memory.is_some();
        let default_size = if live_memory {
            LIVE_MEMORY_CHUNK_BYTES
        } else {
            DEFAULT_CHUNK_BYTES
        };
        let size = size.map_or(Some(default_size), |value| {
            value
                .to_str()?
                .parse::<usize>()
                .ok()
                .filter(|size| (RECORD_BYTES..=MAX_CHUNK_BYTES).contains(size))
        });
        let Some(size) = size else {
            eprintln!(
                "hirsel: HIRSEL_PROFILE_CHUNK_BYTES requires {RECORD_BYTES} to {MAX_CHUNK_BYTES} bytes"
            );
            return None;
        };
        match Self::open(path, size, tasks, live_memory) {
            Ok(mut run) => {
                if let Some(hz) = hz {
                    let stop = Arc::new(AtomicBool::new(false));
                    let flag = stop.clone();
                    match thread::Builder::new()
                        .name("hirsel-sampler".into())
                        .spawn(move || sample_cpu(hz, &flag))
                    {
                        Ok(handle) => run.sampler = Some((stop, handle)),
                        Err(error) => eprintln!("hirsel: cannot sample CPU: {error}"),
                    }
                }
                match memory {
                    Some(settings) if replay => crate::mem_prof::start(settings),
                    Some(settings) => crate::mem_prof::start_live(settings),
                    None => {}
                }
                Some(run)
            }
            Err(error) => {
                eprintln!("hirsel: cannot start runtime profile: {error}");
                None
            }
        }
    }

    fn open(
        path: impl AsRef<std::path::Path>,
        chunk_bytes: usize,
        tasks: bool,
        live_memory: bool,
    ) -> io::Result<Self> {
        let mut output = BufWriter::new(File::options().write(true).truncate(true).open(path)?);
        output.write_all(&MAGIC)?;
        if let Some(maps) = crate::host::sys::mappings() {
            let stamp = now_ns();
            let pid = std::process::id();
            for line in maps
                .split(|&byte| byte == b'\n')
                .filter(|line| !line.is_empty())
            {
                let len = u32::try_from(line.len()).map_err(io::Error::other)?;
                output.write_all(&header(stamp, pid, Kind::Mapping as u16, 0, len))?;
                output.write_all(line)?;
            }
        }
        output.flush()?;
        let shared = Arc::new(Shared {
            tasks,
            chunk_bytes,
            live_memory,
            new_threads: Mutex::default(),
            done: AtomicBool::new(false),
        });
        let reader = shared.clone();
        let drain = thread::Builder::new()
            .name("hirsel-profile".into())
            .spawn(move || drain(&reader, output))?;
        *RUN.lock().unwrap() = Some(shared.clone());
        Ok(Self {
            shared,
            drain: Some(drain),
            sampler: None,
        })
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        if let Some((stop, sampler)) = self.sampler.take() {
            stop.store(true, Ordering::Release);
            let _ = sampler.join();
        }
        // Every runtime worker has returned, so all registered chunks are sealed.
        if crate::mem_prof::enabled() {
            let _worker = Worker::enter();
            crate::mem_prof::finish();
        }
        *RUN.lock().unwrap() = None;
        self.shared.done.store(true, Ordering::Release);
        let drain = self.drain.take().unwrap();
        drain.thread().unpark();
        match drain.join() {
            Ok(Ok(())) => {}
            Ok(Err(error)) => eprintln!("hirsel: cannot finish runtime profile: {error}"),
            Err(_) => eprintln!("hirsel: runtime profile writer stopped"),
        }
    }
}

fn drain(shared: &Shared, mut output: impl Write) -> io::Result<()> {
    let mut threads: Vec<(Arc<Thread>, usize)> = Vec::new();
    let mut next_rss = 0;
    let mut peak = None::<Arc<crate::mem_prof::Residency>>;
    let mut last = None;
    loop {
        let done = shared.done.load(Ordering::Acquire);
        threads.extend(
            shared
                .new_threads
                .lock()
                .unwrap()
                .drain(..)
                .map(|thread| (thread, 0)),
        );
        let mut index = 0;
        while index < threads.len() {
            let (worker, next) = &mut threads[index];
            let dead = worker.dead.load(Ordering::Acquire);
            for _ in 0..2 {
                if !worker.chunks[*next].drain(&mut output)? {
                    break;
                }
                *next ^= 1;
            }
            if dead {
                output.write_all(&header(now_ns(), worker.tid, LOSS, 2, 0))?;
                output.write_all(&worker.lost.load(Ordering::Relaxed).to_le_bytes())?;
                output.write_all(&worker.memory_emitted.load(Ordering::Relaxed).to_le_bytes())?;
                threads.swap_remove(index);
            } else {
                index += 1;
            }
        }
        if done {
            for (label, snapshot) in [peak, last].into_iter().enumerate() {
                if let Some(snapshot) = snapshot {
                    write_residency(&mut output, label as u64, &snapshot)?;
                }
            }
            output.write_all(&header(now_ns(), 0, END, 0, 0))?;
            return output.flush();
        }
        if shared.live_memory {
            let now = now_ns();
            if now >= next_rss
                && let Some(read) = crate::mem_prof::residency()
            {
                next_rss = now + RSS_INTERVAL_NS;
                let read = Arc::new(read);
                if peak.as_ref().is_none_or(|peak| read.rss > peak.rss) {
                    peak = Some(read.clone());
                }
                last = Some(read);
            }
        }
        thread::park_timeout(Duration::from_millis(1));
    }
}

fn write_residency(
    output: &mut impl Write,
    label: u64,
    read: &crate::mem_prof::Residency,
) -> io::Result<()> {
    output.write_all(&header(
        read.timestamp,
        0,
        Kind::ResidentSnapshot as u16,
        2,
        0,
    ))?;
    output.write_all(&label.to_le_bytes())?;
    output.write_all(&read.rss.to_le_bytes())?;
    for &(id, bytes) in &read.samples {
        output.write_all(&header(
            read.timestamp,
            0,
            Kind::ResidentMember as u16,
            3,
            0,
        ))?;
        output.write_all(&label.to_le_bytes())?;
        output.write_all(&id.to_le_bytes())?;
        output.write_all(&bytes.to_le_bytes())?;
    }
    Ok(())
}

static NEXT_RUN: AtomicU64 = AtomicU64::new(1);

pub(crate) fn run_id() -> u64 {
    NEXT_RUN.fetch_add(1, Ordering::Relaxed)
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
pub(crate) enum Reason {
    None = 0,
    ChannelRecv = 1,
    ChannelSend = 2,
    Join = 3,
    Timer = 4,
    ProviderIo = 5,
    StepSlice = 6,
}

#[derive(Clone, Copy)]
#[repr(u64)]
pub(crate) enum WakeSource {
    Task = 1,
    Timer = 2,
    Net = 3,
    Cancel = 4,
    Unknown = 5,
}

pub(crate) struct HostInterval([u64; 3]);

impl HostInterval {
    pub(crate) fn begin([unit, task]: [u64; 2], site: u64) -> Self {
        let interval = Self([unit, task, site]);
        append(Kind::HostBegin, &interval.0, &[]);
        interval
    }
}

impl Drop for HostInterval {
    fn drop(&mut self) {
        append(Kind::HostEnd, &self.0, &[]);
    }
}

#[derive(Clone, Copy)]
#[repr(u64)]
pub(crate) enum Phase {
    Wait = 1,
    Issue = 2,
    Resume = 3,
    Deliver = 4,
    Abort = 5,
}

pub(crate) struct Callback {
    token: u64,
    phase: Phase,
    start: u64,
}

impl Callback {
    pub(crate) fn begin(token: u64, phase: Phase) -> Self {
        Self {
            token,
            phase,
            start: timing_start(),
        }
    }
}

impl Drop for Callback {
    fn drop(&mut self) {
        if self.start != 0 {
            append(
                Kind::IoCallback,
                &[
                    self.token,
                    self.phase as u64,
                    now_ns().saturating_sub(self.start),
                ],
                &[],
            );
        }
    }
}

pub(crate) fn timing_start() -> u64 {
    if WRITER.with(|slot| slot.borrow().as_ref().is_some_and(|writer| writer.tasks)) {
        now_ns()
    } else {
        0
    }
}

pub(crate) fn pending(token: u64, start: u64) {
    if start != 0 {
        append(
            Kind::IoPending,
            &[token, now_ns().saturating_sub(start)],
            &[],
        );
    }
}

#[cfg(feature = "testing")]
pub mod testing {
    use super::*;

    /// Write mapping records through the production chunk writer, then end the
    /// stream cleanly. Cross-crate format tests use this to exercise the same
    /// writer the runtime uses; a record the writer refuses is absent.
    pub fn mapping_stream(chunk_bytes: usize, payloads: &[&[u8]]) -> Vec<u8> {
        let thread = Arc::new(Thread::new(3, chunk_bytes).unwrap());
        let mut writer = Writer::new(thread.clone(), chunk_bytes, true);
        for payload in payloads {
            writer.append(Kind::Mapping, &[], payload);
        }
        drop(writer);
        let mut stream = MAGIC.to_vec();
        for chunk in &thread.chunks {
            chunk.drain(&mut stream).unwrap();
        }
        stream.extend_from_slice(&header(now_ns(), 0, END, 0, 0));
        stream
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_chunks_drop_without_waiting_for_the_drain() {
        let bytes = HEADER_BYTES + 8;
        let thread = Arc::new(Thread::new(3, bytes).unwrap());
        let mut writer = Writer::new(thread.clone(), bytes, true);
        writer.append(Kind::Task, &[23], &[]);
        writer.append(Kind::Task, &[29], &[]);
        // Both chunks have unread records. No other thread is running, so
        // this call can return only by dropping the third record.
        writer.append(Kind::Task, &[31], &[]);
        let mut output = Vec::new();
        assert!(thread.chunks[0].drain(&mut output).unwrap());
        writer.append(Kind::Task, &[37], &[]);
        drop(writer);
        assert!(thread.chunks[1].drain(&mut output).unwrap());
        assert!(thread.chunks[0].drain(&mut output).unwrap());
        assert_eq!(thread.lost.load(Ordering::Relaxed), 1);
        let words: Vec<_> = output
            .chunks_exact(bytes)
            .map(|row| u64::from_le_bytes(row[HEADER_BYTES..].try_into().unwrap()))
            .collect();
        assert_eq!(words, [23, 29, 37]);
    }
}
