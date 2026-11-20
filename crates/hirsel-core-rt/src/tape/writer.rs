//! Recording blocks leave the workers. A recorder fills a block in memory it
//! owns and, when the block closes, hands the whole buffer to the process's
//! one writer thread, which appends it to the recording's file. A worker
//! never waits for a file write.
//!
//! One ordered path: each recording's blocks enter the queue in sequence
//! order (one recorder per recording) and the single writer takes them in
//! queue order, so a file holds its blocks in sequence. The writer batches
//! the queued blocks of one file into one `writev`.
//!
//! The queue is bounded at `BOUND` bytes. A block that does not fit is not
//! queued: its recording is truncated there. The recorder queues a
//! TRUNCATED marker block in that block's place (the marker may exceed the
//! bound by its own few bytes) and records nothing more. Replay reads the
//! marker as "truncated at" that point, never as a complete recording. A
//! failed file write also ends that file's output where it failed; the file
//! then lacks its END block and replay refuses it. The program is never
//! stalled or failed by its recording.
//!
//! `flush` waits until every queued block is written. Replay calls it before
//! opening a recording, and a recording launch calls it before the process
//! reports and exits.
use std::collections::VecDeque;
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use super::TAPE_COUNTS;

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
pub(crate) struct Stream {
    fd: OwnedFd,
    failed: AtomicBool,
}
impl Stream {
    pub(crate) fn new(fd: OwnedFd) -> Arc<Self> {
        Arc::new(Self {
            fd,
            failed: AtomicBool::new(false),
        })
    }
}

#[derive(Default)]
struct Queue {
    jobs: VecDeque<(Arc<Stream>, Vec<u8>)>,
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

/// Queue a closed block. False when it does not fit the bound; the caller
/// then truncates its recording with `mark`. A recording's first block (its
/// header) is always queued, so even a recording cut at once reads back as
/// truncated rather than as a damaged file.
pub(crate) fn push(stream: &Arc<Stream>, block: Vec<u8>, first: bool) -> bool {
    let q = lock(&QUEUE);
    if !first && q.bytes + block.len() > bound() {
        return false;
    }
    enqueue(q, stream, block);
    true
}

/// Queue a recording's TRUNCATED marker, past the bound.
pub(crate) fn mark(stream: &Arc<Stream>, marker: Vec<u8>) {
    TAPE_COUNTS[super::C_TRUNCATED].fetch_add(1, Relaxed);
    enqueue(lock(&QUEUE), stream, marker);
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
            crate::process::stop(b"TapeWriter");
        }
    }
}

fn enqueue(mut q: MutexGuard<'static, Queue>, stream: &Arc<Stream>, block: Vec<u8>) {
    q.bytes += block.len();
    q.jobs.push_back((stream.clone(), block));
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
    let idle = libc::sched_param { sched_priority: 0 };
    // SAFETY: plain syscall on the calling thread with a valid parameter.
    unsafe { libc::sched_setscheduler(0, libc::SCHED_IDLE, &idle) };
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
fn write_run(batch: &mut VecDeque<(Arc<Stream>, Vec<u8>)>) -> usize {
    const MAX_IOV: usize = 64;
    let stream = batch[0].0.clone();
    let n = batch
        .iter()
        .take(MAX_IOV)
        .take_while(|(s, _)| Arc::ptr_eq(s, &stream))
        .count();
    let run: Vec<_> = batch.drain(..n).map(|(_, b)| b).collect();
    let total: usize = run.iter().map(Vec::len).sum();
    if !stream.failed.load(Relaxed) && !write_all(stream.fd.as_raw_fd(), &run) {
        stream.failed.store(true, Relaxed);
        TAPE_COUNTS[super::C_WRITE_FAILED].fetch_add(1, Relaxed);
    }
    TAPE_COUNTS[super::C_WRITER_CALLS].fetch_add(1, Relaxed);
    let mut spares = lock(&SPARES);
    for mut b in run {
        if spares.len() < SPARE && b.capacity() <= 4 * super::format::CLOSE_AT {
            b.clear();
            spares.push(b);
        }
    }
    total
}

fn write_all(fd: i32, bufs: &[Vec<u8>]) -> bool {
    let mut iov: Vec<libc::iovec> = bufs
        .iter()
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
            if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return false;
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
    true
}
