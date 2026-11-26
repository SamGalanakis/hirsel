//! Recording blocks leave the workers. A recorder fills a block in memory it
//! owns and, when the block closes, hands the whole buffer to the process's
//! one writer thread, which appends it to the recording's file. A worker
//! never waits for a file write.
//!
//! One ordered path: a recording is one file holding one stream per unit,
//! and each stream's blocks enter the queue in sequence order (one recorder
//! per stream). The single writer takes them in queue order, so each stream
//! holds its blocks in sequence. The writer batches the queued blocks of one
//! file into one `writev`.
//!
//! The queue is bounded at `BOUND` bytes. A block that does not fit is not
//! queued: its stream is truncated there, and no other. The recorder queues
//! a TRUNCATED marker block in that block's place (the marker may exceed the
//! bound by its own few bytes) and records nothing more on that stream.
//! Replay reads the marker as "truncated at" that point, never as a complete
//! recording. A failed file write ends the file's output where it failed,
//! and truncates each stream with a block at or after that point. The
//! program is never stalled or failed by its recording. Every truncation is
//! kept for the launcher's report, one line per stream (`take_cuts`).
//!
//! `flush` waits until every queued block is written. Replay calls it before
//! opening a recording, and a recording launch calls it before the process
//! reports and exits.
use std::collections::VecDeque;
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use super::{IoError, TAPE_COUNTS};

/// Queued bytes at most: 128 MiB, or `HIRSEL_TAPE_QUEUE_MB` (0 truncates
/// every recording right after its header block, for testing the policy).
fn bound() -> usize {
    static B: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *B.get_or_init(|| {
        std::env::var("HIRSEL_TAPE_QUEUE_MB")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .map_or(128 << 20, |mb| mb << 20)
    })
}
/// Emptied buffers kept for the next blocks, so a recorder does not map and
/// unmap a fresh block buffer per block.
const SPARE: usize = 64;

/// One recording's file, written only by the writer thread.
struct File {
    fd: OwnedFd,
    /// The first failed write, after which nothing more is written.
    failed: Mutex<Option<IoError>>,
}

/// One unit's stream in a recording's file.
pub(crate) struct Stream {
    file: Arc<File>,
    /// The unit's spawn-index path: empty for the root.
    path: Vec<u64>,
    /// Frames in this stream's queued blocks, and in its written blocks.
    queued: AtomicU64,
    written: AtomicU64,
    cut: AtomicBool,
}
impl Stream {
    /// The root stream of a recording written to `fd`.
    pub(crate) fn new(fd: OwnedFd) -> Arc<Self> {
        Self::of(
            Arc::new(File {
                fd,
                failed: Mutex::new(None),
            }),
            Vec::new(),
        )
    }
    fn of(file: Arc<File>, path: Vec<u64>) -> Arc<Self> {
        Arc::new(Self {
            file,
            path,
            queued: AtomicU64::new(0),
            written: AtomicU64::new(0),
            cut: AtomicBool::new(false),
        })
    }
    /// The stream, in the same file, of this unit's helper with spawn `index`.
    pub(crate) fn helper(&self, index: u64) -> Arc<Self> {
        let mut path = self.path.clone();
        path.push(index);
        Self::of(self.file.clone(), path)
    }
    pub(crate) fn path(&self) -> &[u64] {
        &self.path
    }
    /// Truncate this stream at frame `at`, once: later cuts are its own
    /// marker or the file failing after it, which change nothing.
    fn cut(&self, at: u64, why: Cut) {
        if !self.cut.swap(true, Relaxed) {
            cut(self.path.clone(), at, why);
        }
    }
}

/// Why a stream ends before its unit does.
#[derive(Clone, Copy)]
pub(crate) enum Cut {
    /// Writing its file failed.
    Io(IoError),
    /// The writer's queue was full at its next block.
    Queue,
    /// Its entry holds what this build does not record (named).
    Entry(&'static str),
}

static CUTS: Mutex<Vec<(Vec<u64>, u64, Cut)>> = Mutex::new(Vec::new());

/// Record that the stream at `path` ends at frame `at`.
pub(crate) fn cut(path: Vec<u64>, at: u64, why: Cut) {
    TAPE_COUNTS[super::C_TRUNCATED].fetch_add(1, Relaxed);
    lock(&CUTS).push((path, at, why));
}

/// Each truncated stream, once, by spawn-index path compared numerically.
pub(crate) fn take_cuts() -> Vec<(Vec<u64>, u64, Cut)> {
    let mut cuts = std::mem::take(&mut *lock(&CUTS));
    cuts.sort_by(|a, b| a.0.cmp(&b.0));
    cuts
}

/// A unit's name from its spawn-index path: `root`, `root.2.0`.
pub(crate) fn unit_name(path: &[u64]) -> String {
    let mut name = String::from("root");
    for index in path {
        name.push_str(&format!(".{index}"));
    }
    name
}

struct Block {
    stream: Arc<Stream>,
    bytes: Vec<u8>,
    frames: u64,
}

#[derive(Default)]
struct Queue {
    jobs: VecDeque<Block>,
    bytes: usize,
    writing: bool,
    sleeping: bool,
    started: bool,
}
static QUEUE: Mutex<Queue> = Mutex::new(Queue {
    jobs: VecDeque::new(),
    bytes: 0,
    writing: false,
    sleeping: false,
    started: false,
});
static WORK: Condvar = Condvar::new();
static IDLE: Condvar = Condvar::new();
static SPARES: Mutex<Vec<Vec<u8>>> = Mutex::new(Vec::new());

fn lock<T>(m: &'static Mutex<T>) -> MutexGuard<'static, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// An empty buffer for the next block, reusing a written one when it can.
pub(crate) fn spare() -> Vec<u8> {
    lock(&SPARES).pop().unwrap_or_default()
}

/// Queue a closed block. False when it does not fit the bound: the stream
/// is truncated there, and the caller queues its marker with `mark`. A
/// stream's first block (its header) is always queued, so even a stream cut
/// at once reads back as truncated rather than as a damaged file.
pub(crate) fn push(stream: &Arc<Stream>, block: Vec<u8>, frames: u64, first: bool) -> bool {
    let q = lock(&QUEUE);
    if !first && q.bytes + block.len() > bound() {
        drop(q);
        stream.cut(stream.queued.load(Relaxed), Cut::Queue);
        return false;
    }
    stream.queued.fetch_add(frames, Relaxed);
    enqueue(
        q,
        Block {
            stream: stream.clone(),
            bytes: block,
            frames,
        },
    );
    true
}

/// Queue a stream's TRUNCATED marker, past the bound.
pub(crate) fn mark(stream: &Arc<Stream>, marker: Vec<u8>) {
    enqueue(
        lock(&QUEUE),
        Block {
            stream: stream.clone(),
            bytes: marker,
            frames: 0,
        },
    );
}

/// Start the writer thread, once. A recording launch calls it on the host
/// stack; a block queued before that starts it from its task.
pub(crate) fn start() {
    let mut q = lock(&QUEUE);
    if !q.started {
        q.started = true;
        // The writer's stack holds only its batch descriptors.
        let spawned = std::thread::Builder::new()
            .name("hirsel-tape".into())
            .stack_size(256 << 10)
            .spawn(run);
        if spawned.is_err() {
            crate::run_stop::raise(crate::run_stop::Stop::from_static(b"TapeWriter"));
        }
    }
}

fn enqueue(mut q: MutexGuard<'static, Queue>, block: Block) {
    q.bytes += block.bytes.len();
    q.jobs.push_back(block);
    if !q.started {
        drop(q);
        start();
    } else if q.sleeping {
        q.sleeping = false;
        WORK.notify_one();
    }
}

/// Wait until every queued block is written (or its file failed).
pub(crate) fn flush() {
    let mut q = lock(&QUEUE);
    while !q.jobs.is_empty() || q.writing {
        q = IDLE.wait(q).unwrap_or_else(|e| e.into_inner());
    }
}

fn run() {
    // The writer shares the workers' CPUs; at idle priority it only takes
    // time they leave, so it never preempts a worker. Best effort: if the
    // policy is refused it runs at normal priority.
    crate::host::sys::background_thread();
    let mut batch = VecDeque::new();
    loop {
        {
            let mut q = lock(&QUEUE);
            while q.jobs.is_empty() {
                q.writing = false;
                IDLE.notify_all();
                q.sleeping = true;
                q = WORK.wait(q).unwrap_or_else(|e| e.into_inner());
            }
            q.writing = true;
            std::mem::swap(&mut q.jobs, &mut batch);
        }
        let t0 = std::time::Instant::now();
        let mut done = 0usize;
        while !batch.is_empty() {
            done += write_run(&mut batch);
        }
        TAPE_COUNTS[super::C_WRITER_NS].fetch_add(t0.elapsed().as_nanos() as u64, Relaxed);
        lock(&QUEUE).bytes -= done;
    }
}

/// Write the leading run of blocks that share one file with one `writev`
/// (or as few as it takes), then recycle their buffers. Returns their bytes.
fn write_run(batch: &mut VecDeque<Block>) -> usize {
    const MAX_IOV: usize = 64;
    let file = batch[0].stream.file.clone();
    let n = batch
        .iter()
        .take(MAX_IOV)
        .take_while(|block| Arc::ptr_eq(&block.stream.file, &file))
        .count();
    let run: Vec<_> = batch.drain(..n).collect();
    let total = run.iter().map(|block| block.bytes.len()).sum();
    let mut failed = lock_file(&file);
    let written = match *failed {
        Some(_) => 0,
        None => match write_all(file.fd.as_raw_fd(), &run) {
            Ok(()) => run.len(),
            Err((written, error)) => {
                *failed = Some(error);
                TAPE_COUNTS[super::C_WRITE_FAILED].fetch_add(1, Relaxed);
                written
            }
        },
    };
    for block in &run[..written] {
        block.stream.written.fetch_add(block.frames, Relaxed);
    }
    if let Some(error) = *failed {
        for block in &run[written..] {
            block
                .stream
                .cut(block.stream.written.load(Relaxed), Cut::Io(error));
        }
    }
    drop(failed);
    TAPE_COUNTS[super::C_WRITER_CALLS].fetch_add(1, Relaxed);
    let mut spares = lock(&SPARES);
    for Block { bytes: mut b, .. } in run {
        if spares.len() < SPARE && b.capacity() <= 4 * super::format::CLOSE_AT {
            b.clear();
            spares.push(b);
        }
    }
    total
}

fn lock_file(file: &File) -> MutexGuard<'_, Option<IoError>> {
    file.failed.lock().unwrap_or_else(|e| e.into_inner())
}

fn write_all(fd: i32, blocks: &[Block]) -> Result<(), (usize, IoError)> {
    let mut iov: Vec<libc::iovec> = blocks
        .iter()
        .map(|block| &block.bytes)
        .map(|b| libc::iovec {
            iov_base: b.as_ptr().cast_mut().cast(),
            iov_len: b.len(),
        })
        .collect();
    let mut at = 0;
    while at < iov.len() {
        // SAFETY: each iovec names live, readable bytes of `bufs`.
        let n = unsafe { libc::writev(fd, iov[at..].as_ptr(), (iov.len() - at) as i32) };
        if n < 0 {
            let code = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            if code == libc::EINTR {
                continue;
            }
            return Err((
                at,
                IoError {
                    tag: 5,
                    code: code as u32,
                },
            ));
        }
        if n == 0 {
            return Err((
                at,
                IoError {
                    tag: 5,
                    code: libc::EIO as u32,
                },
            ));
        }
        let mut n = n as usize;
        while at < iov.len() && n >= iov[at].iov_len {
            n -= iov[at].iov_len;
            at += 1;
        }
        if n > 0 {
            // SAFETY: advancing within the same buffer.
            iov[at].iov_base = unsafe { iov[at].iov_base.cast::<u8>().add(n).cast() };
            iov[at].iov_len -= n;
        }
    }
    Ok(())
}
