//! Idle mappings in the unit pool. Workers hand off excess mappings and ask
//! the memory thread for fresh ones on a miss. The thread owns the syscalls.
use crate::{
    fiber::Stack,
    task_heap::slabs::{Extent, Slab},
};
use hirsel_core_ids::launch::Metadata;
use std::{
    collections::VecDeque,
    ptr::NonNull,
    sync::{Condvar, Mutex, OnceLock},
    thread::{self, JoinHandle},
};

const MIB: usize = 1 << 20;
const RELEASE_SLOTS: usize = 32_768;

pub(super) struct UnitStack {
    base: NonNull<u8>,
    len: usize,
}

// SAFETY: ownership of the mapping moves with the stack. A worker gives it
// back only after switching to its host stack for the last time.
unsafe impl Send for UnitStack {}

impl UnitStack {
    pub(super) fn new(usable_bytes: usize) -> Self {
        let guard = crate::task_heap::page_bytes();
        let len = usable_bytes + guard;
        // SAFETY: allocate a private stack and make its lowest page inaccessible.
        unsafe {
            let raw = libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE,
                -1,
                0,
            );
            if raw == libc::MAP_FAILED || libc::mprotect(raw, guard, libc::PROT_NONE) != 0 {
                crate::process::stop(b"UnitStack");
            }
            Self {
                base: NonNull::new_unchecked(raw.cast()),
                len,
            }
        }
    }

    pub(super) fn top(&self) -> *mut u8 {
        // SAFETY: the end address is within one mapping, one past its last byte.
        unsafe { self.base.as_ptr().add(self.len) }
    }
}

impl Drop for UnitStack {
    fn drop(&mut self) {
        // SAFETY: the stack owns this mapping and nobody is running on it.
        unsafe { libc::munmap(self.base.as_ptr().cast(), self.len) };
    }
}

struct Stock<T> {
    idle: Vec<(T, usize)>,
    bytes: usize,
    peak: usize,
    hits: usize,
    misses: usize,
}

struct Cache<T> {
    stock: Mutex<Stock<T>>,
    cap: usize,
}

impl<T> Cache<T> {
    fn new(cap: usize, min_bytes: usize) -> Self {
        Self {
            stock: Mutex::new(Stock {
                idle: Vec::with_capacity(cap / min_bytes),
                bytes: 0,
                peak: 0,
                hits: 0,
                misses: 0,
            }),
            cap,
        }
    }

    fn take(&self, fits: impl FnMut(&T) -> bool) -> Option<T> {
        let mut stock = self.stock.lock().unwrap();
        let mut fits = fits;
        let Some(index) = stock.idle.iter().rposition(|(value, _)| fits(value)) else {
            stock.misses += 1;
            return None;
        };
        let (value, bytes) = stock.idle.swap_remove(index);
        stock.bytes -= bytes;
        stock.hits += 1;
        Some(value)
    }

    fn put(&self, value: T, bytes: usize) -> Option<T> {
        let mut stock = self.stock.lock().unwrap();
        if bytes > self.cap - stock.bytes {
            return Some(value);
        }
        stock.bytes += bytes;
        stock.peak = stock.peak.max(stock.bytes);
        stock.idle.push((value, bytes));
        None
    }

    fn drain(&self) -> Vec<(T, usize)> {
        let mut stock = self.stock.lock().unwrap();
        stock.bytes = 0;
        std::mem::take(&mut stock.idle)
    }

    fn report(&self, name: &str) -> String {
        let stock = self.stock.lock().unwrap();
        format!(
            "{name}_cap={} {name}_peak={} {name}_hits={} {name}_misses={}",
            self.cap, stock.peak, stock.hits, stock.misses
        )
    }
}

type Job = Box<dyn FnOnce() + Send>;

enum Reclaim {
    UnitStack(UnitStack),
    TaskStack(Stack),
    Slab(Slab),
    Page(Extent),
}

impl Reclaim {
    fn dispose(self) {
        match self {
            Self::UnitStack(stack) => drop(stack),
            Self::TaskStack(stack) => drop(stack),
            Self::Slab(slab) => slab.dispose(),
            Self::Page(page) => drop(page),
        }
    }
}

struct Work {
    maps: VecDeque<Job>,
    releases: VecDeque<(usize, Reclaim)>,
    pending_bytes: usize,
    peak_pending_bytes: usize,
    closing: bool,
}

pub(crate) struct Memory {
    plan: super::UntiedPlan,
    launch: OnceLock<hirsel_core_ids::launch::Metadata>,
    unit_stacks: Cache<UnitStack>,
    task_stacks: Cache<Stack>,
    slabs: Cache<Slab>,
    pages: Cache<Extent>,
    work: Mutex<Work>,
    ready: Condvar,
}

/// The storage operations a running task can request from its unit pool.
pub(crate) trait SharedMemory: Sync {
    fn native_metadata(&self, target: &str, n_place: u64, describe: bool) -> Metadata;
    fn take_task_stack(&self) -> Stack;
    fn put_task_stack(&self, stack: Stack);
    fn take_slab(&self) -> Option<Slab>;
    fn put_slab(&self, slab: Slab);
    fn dispose_slab(&self, slab: Slab);
    fn take_page(&self, granules: usize, align: usize, prefix: usize) -> Option<Extent>;
    fn put_page(&self, page: Extent);
    fn map_range(&self, len: usize, align: usize, prefix: usize) -> Option<NonNull<u8>>;
}

impl Memory {
    pub(super) fn new(plan: super::UntiedPlan) -> Self {
        Self {
            plan,
            launch: OnceLock::new(),
            unit_stacks: Cache::new(64 * MIB, plan.unit_stack_bytes),
            task_stacks: Cache::new(64 * MIB, crate::fiber::COMMIT),
            slabs: Cache::new(128 * MIB, crate::task_heap::SLAB),
            pages: Cache::new(64 * MIB, crate::task_heap::page_bytes()),
            work: Mutex::new(Work {
                maps: VecDeque::new(),
                releases: VecDeque::with_capacity(RELEASE_SLOTS),
                pending_bytes: 0,
                peak_pending_bytes: 0,
                closing: false,
            }),
            ready: Condvar::new(),
        }
    }

    pub(super) fn start(&'static self, stack_bytes: usize) -> std::io::Result<MemoryThread> {
        let thread = thread::Builder::new()
            .name("hirsel-memory".into())
            .stack_size(stack_bytes)
            .spawn(|| self.run())?;
        Ok(MemoryThread {
            memory: self,
            thread: Some(thread),
        })
    }

    /// Run `job` on the memory thread.
    pub(super) fn post(&self, job: impl FnOnce() + Send + 'static) {
        // The checker models no memory thread: a scheduled thread runs the
        // job in place, at the point it would be posted.
        #[cfg(test)]
        if super::check::modelled() {
            return job();
        }
        self.work.lock().unwrap().maps.push_back(Box::new(job));
        self.ready.notify_one();
    }

    /// Map on the memory thread while the running unit parks. Off the pool
    /// (no unit to park) the caller maps in place.
    pub(crate) fn map<T: Send + 'static>(&self, map: impl FnOnce() -> T + Send + 'static) -> T {
        #[cfg(test)]
        if super::check::modelled() {
            return map();
        }
        if !super::in_unit() {
            return map();
        }
        struct Out<T>(*mut Option<T>, *const super::Reply);
        // SAFETY: the parked unit keeps both alive until the reply completes.
        unsafe impl<T: Send> Send for Out<T> {}
        let reply = super::Reply::new();
        let mut answer = None;
        let out = Out(&mut answer, &reply);
        self.post(move || {
            let out = out;
            // SAFETY: as above; nothing touches `out` after the completion.
            unsafe {
                *out.0 = Some(map());
                super::Reply::complete(out.1);
            }
        });
        reply.wait();
        answer.unwrap()
    }

    /// Queue `release` for the memory thread, or dispose of it here when the
    /// queue is full rather than wait for room.
    fn release(&self, bytes: usize, release: Reclaim) {
        let mut work = self.work.lock().unwrap();
        if work.releases.len() == RELEASE_SLOTS {
            drop(work);
            release.dispose();
            return;
        }
        work.releases.push_back((bytes, release));
        work.pending_bytes += bytes;
        work.peak_pending_bytes = work.peak_pending_bytes.max(work.pending_bytes);
        self.ready.notify_one();
    }

    fn run(&self) {
        loop {
            let (map, release) = {
                let mut work = self.work.lock().unwrap();
                while work.maps.is_empty() && work.releases.is_empty() && !work.closing {
                    work = self.ready.wait(work).unwrap();
                }
                (work.maps.pop_front(), work.releases.pop_front())
            };
            if map.is_none() && release.is_none() {
                break;
            }
            if let Some(map) = map {
                map();
            }
            if let Some((bytes, release)) = release {
                release.dispose();
                self.work.lock().unwrap().pending_bytes -= bytes;
            }
        }
        drop(self.unit_stacks.drain());
        drop(self.task_stacks.drain());
        for (slab, _) in self.slabs.drain() {
            slab.dispose();
        }
        drop(self.pages.drain());
    }

    pub(super) fn cached_unit_stack(&self) -> Option<UnitStack> {
        self.unit_stacks.take(|_| true)
    }

    pub(super) fn put_unit_stack(&self, stack: UnitStack) {
        let bytes = stack.len;
        if let Some(stack) = self.unit_stacks.put(stack, bytes) {
            self.release(bytes, Reclaim::UnitStack(stack));
        }
    }

    pub(super) fn report(&self) {
        let cache = [
            self.unit_stacks.report("unit_stack"),
            self.task_stacks.report("task_stack"),
            self.slabs.report("slab"),
            self.pages.report("page"),
        ];
        let pending = self.work.lock().unwrap().peak_pending_bytes;
        eprintln!("pool-memory {} pending_peak={pending}", cache.join(" "));
    }
}

impl SharedMemory for Memory {
    fn native_metadata(&self, target: &str, n_place: u64, describe: bool) -> Metadata {
        // All units have the same target and leading launch options. Resolve
        // the lazy digest here so its file mapping is also released here.
        // The memory thread resolves it once; later jobs find it set. No
        // worker waits inside `get_or_init`.
        if let Some(launch) = self.launch.get() {
            return launch.clone();
        }
        struct Cell(*const OnceLock<Metadata>);
        // SAFETY: the pool's memory outlives the memory thread's jobs.
        unsafe impl Send for Cell {}
        let cell = Cell(&self.launch);
        let target = target.to_owned();
        self.map(move || {
            let cell = cell;
            // SAFETY: as above.
            let launch = unsafe { &*cell.0 };
            launch
                .get_or_init(|| {
                    let launch = Metadata::native(&target, n_place, describe);
                    let _ = launch.digest();
                    launch
                })
                .clone()
        })
    }

    fn take_task_stack(&self) -> Stack {
        let reserve = self.plan.task_stack_reserve;
        self.task_stacks
            .take(|_| true)
            .unwrap_or_else(|| self.map(move || Stack::new_with_reserve(reserve)))
    }

    fn put_task_stack(&self, stack: Stack) {
        let bytes = stack.committed_bytes();
        if let Some(stack) = self.task_stacks.put(stack, bytes) {
            self.release(bytes, Reclaim::TaskStack(stack));
        }
    }

    fn take_slab(&self) -> Option<Slab> {
        self.slabs.take(|_| true)
    }

    fn put_slab(&self, slab: Slab) {
        if let Some(slab) = self.slabs.put(slab, crate::task_heap::SLAB) {
            self.release(crate::task_heap::SLAB, Reclaim::Slab(slab));
        }
    }

    fn dispose_slab(&self, slab: Slab) {
        self.release(crate::task_heap::SLAB, Reclaim::Slab(slab));
    }

    fn take_page(&self, granules: usize, align: usize, prefix: usize) -> Option<Extent> {
        self.pages.take(|extent| {
            extent.granules() == granules
                && extent.prefix() == prefix
                && extent.payload_aligned(align)
        })
    }

    fn put_page(&self, page: Extent) {
        let bytes = page.committed_bytes();
        if let Some(page) = self.pages.put(page, bytes) {
            self.release(bytes, Reclaim::Page(page));
        }
    }

    fn map_range(&self, len: usize, align: usize, prefix: usize) -> Option<NonNull<u8>> {
        struct Mapped(Option<NonNull<u8>>);
        // SAFETY: the newly mapped base address has no references yet.
        unsafe impl Send for Mapped {}
        self.map(move || Mapped(crate::task_heap::slabs::map_raw_range(len, align, prefix)))
            .0
    }
}

pub(super) struct MemoryThread {
    memory: &'static Memory,
    thread: Option<JoinHandle<()>>,
}

impl Drop for MemoryThread {
    fn drop(&mut self) {
        self.memory.work.lock().unwrap().closing = true;
        self.memory.ready.notify_one();
        self.thread.take().unwrap().join().unwrap();
    }
}
