//! Bounded per-thread chunks for passive runtime observation.
use std::cell::{RefCell, UnsafeCell};
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const MAGIC: &[u8; 8] = b"HIRSOBS1";
const DEFAULT_CHUNK_BYTES: usize = 64 * 1024;
const MAX_CHUNK_BYTES: usize = 64 * 1024 * 1024;
const MAX_RECORD_BYTES: usize = DEFAULT_CHUNK_BYTES;
const HEADER_BYTES: usize = 20;
const LOSS: u16 = u16::MAX;
const END: u16 = u16::MAX - 1;

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
    Alloc = 16,
    Peak = 17,
    BudgetScope = 18,
    ScopeStack = 19,
    MemoryMeta = 20,
    SlackMember = 21,
}

thread_local! {
    static WRITER: RefCell<Option<Writer>> = const { RefCell::new(None) };
}
static RUN: Mutex<Option<Arc<Shared>>> = Mutex::new(None);

struct Chunk {
    data: UnsafeCell<Box<[u8]>>,
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
        data.resize(bytes, 0);
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
            output.write_all(unsafe { &(&*self.data.get())[..len] })?;
            self.len.store(0, Ordering::Release);
        }
        Ok(len != 0)
    }
}

struct Thread {
    chunks: [Chunk; 2],
    dead: AtomicBool,
    lost: AtomicU64,
    tid: u32,
}

impl Thread {
    fn new(tid: u32, bytes: usize) -> Result<Self, std::collections::TryReserveError> {
        Ok(Self {
            chunks: [Chunk::new(bytes)?, Chunk::new(bytes)?],
            dead: AtomicBool::new(false),
            lost: 0.into(),
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
        if let (Ok(words_len), Ok(bytes_len)) =
            (u16::try_from(words.len()), u32::try_from(bytes.len()))
        {
            let needed = HEADER_BYTES + words.len() * 8 + bytes.len();
            if needed <= self.capacity.min(MAX_RECORD_BYTES) {
                if needed > self.capacity - self.used {
                    self.seal();
                }
                let chunk = &self.thread.chunks[self.index];
                if self.used != 0 || chunk.len.load(Ordering::Acquire) == 0 {
                    // SAFETY: this writer is the only producer and owns this
                    // chunk until seal gives custody to the drain thread.
                    let data = unsafe { &mut *chunk.data.get() };
                    let dst = &mut data[self.used..self.used + needed];
                    dst[..HEADER_BYTES].copy_from_slice(&header(
                        now_ns(),
                        self.thread.tid,
                        kind as u16,
                        words_len,
                        bytes_len,
                    ));
                    for (dst, word) in dst[HEADER_BYTES..].chunks_exact_mut(8).zip(words) {
                        dst.copy_from_slice(&word.to_le_bytes());
                    }
                    dst[HEADER_BYTES + words.len() * 8..].copy_from_slice(bytes);
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
            && (writer.tasks || kind as u16 >= 16)
        {
            writer.append(kind, words, bytes);
        }
    });
}

struct Shared {
    tasks: bool,
    chunk_bytes: usize,
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
        // SAFETY: gettid has no operands.
        let tid = unsafe { libc::syscall(libc::SYS_gettid) } as u32;
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
        // SAFETY: the generated executable calls this at process entry.
        unsafe {
            std::env::remove_var("HIRSEL_PROFILE");
            std::env::remove_var("HIRSEL_PROFILE_CHUNK_BYTES");
            std::env::remove_var("HIRSEL_PROFILE_TASKS");
        }
        let size = size.map_or(Some(DEFAULT_CHUNK_BYTES), |value| {
            value
                .to_str()?
                .parse::<usize>()
                .ok()
                .filter(|size| (DEFAULT_CHUNK_BYTES..=MAX_CHUNK_BYTES).contains(size))
        });
        let Some(size) = size else {
            eprintln!(
                "hirsel: HIRSEL_PROFILE_CHUNK_BYTES requires {DEFAULT_CHUNK_BYTES} to {MAX_CHUNK_BYTES} bytes"
            );
            return None;
        };
        match Self::open(path, size, tasks) {
            Ok(run) => {
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
    ) -> io::Result<Self> {
        let mut output = BufWriter::new(File::options().write(true).truncate(true).open(path)?);
        output.write_all(MAGIC)?;
        if let Ok(maps) = std::fs::read("/proc/self/maps") {
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
        })
    }
}

impl Drop for Run {
    fn drop(&mut self) {
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
                output.write_all(&header(now_ns(), worker.tid, LOSS, 1, 0))?;
                output.write_all(&worker.lost.load(Ordering::Relaxed).to_le_bytes())?;
                threads.swap_remove(index);
            } else {
                index += 1;
            }
        }
        if done {
            output.write_all(&header(now_ns(), 0, END, 0, 0))?;
            return output.flush();
        }
        thread::park_timeout(Duration::from_millis(1));
    }
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
    ProviderIo = 6,
    StepSlice = 7,
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
