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

#[path = "unit_retention.rs"]
mod retention;
use retention::{Retention, Stock};

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

type Job = Box<dyn FnOnce() + Send>;

enum Reclaim {
    UnitStack(UnitStack),
    TaskStack(Stack),
    Slab(Slab),
    Page(Extent),
    #[cfg(feature = "record")]
    Tape(Vec<u8>),
}

impl Reclaim {
    fn dispose(self) {
        match self {
            Self::UnitStack(stack) => drop(stack),
            Self::TaskStack(stack) => drop(stack),
            Self::Slab(slab) => slab.dispose(),
            Self::Page(page) => drop(page),
            #[cfg(feature = "record")]
            Self::Tape(bytes) => drop(bytes),
        }
    }
}

struct Work {
    maps: VecDeque<Job>,
    releases: VecDeque<(usize, Reclaim)>,
    pending_bytes: usize,
    pending_slots: usize,
    sync_dispose_count: usize,
    sync_dispose_bytes: usize,
    purge_idle: bool,
    peak_pending_bytes: usize,
    closing: bool,
}

pub(crate) struct Memory {
    plan: super::UntiedPlan,
    launch: OnceLock<hirsel_core_ids::launch::Metadata>,
    retention: Mutex<Retention>,
    work: Mutex<Work>,
    ready: Condvar,
}

/// The storage operations a running task can request from its unit pool.
pub(crate) trait SharedMemory: Sync {
    fn ready_scratch_bytes(&self) -> usize;
    #[cfg(feature = "record")]
    fn take_tape_buffer(&self) -> Vec<u8>;
    #[cfg(feature = "record")]
    fn put_tape_buffer(&self, bytes: Vec<u8>);
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
            retention: Mutex::new(Retention::new()),
            work: Mutex::new(Work {
                maps: VecDeque::new(),
                releases: VecDeque::with_capacity(plan.reclaim_slots.min(64)),
                pending_bytes: 0,
                pending_slots: 0,
                sync_dispose_count: 0,
                sync_dispose_bytes: 0,
                purge_idle: false,
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

    fn take<T>(
        &self,
        select: impl Fn(&mut Retention) -> &mut Stock<T>,
        fits: impl FnMut(&T) -> bool,
    ) -> Option<T> {
        self.retention.lock().unwrap().take(select, fits)
    }

    fn keep<T>(
        &self,
        select: impl Fn(&mut Retention) -> &mut Stock<T>,
        item: T,
        bytes: usize,
        reclaim: fn(T) -> Reclaim,
    ) {
        let cap = self.plan.retained_bytes;
        if bytes > cap {
            self.release(bytes, reclaim(item));
            return;
        }
        loop {
            let (bytes, removed) = {
                let mut stock = self.retention.lock().unwrap();
                if bytes <= cap - stock.used {
                    stock.keep(select, item, bytes);
                    return;
                }
                stock.pop_oldest(u64::MAX).unwrap()
            };
            self.release(bytes, removed);
        }
    }

    /// The pending bounds include the disposal currently in flight.
    fn release(&self, bytes: usize, release: Reclaim) {
        let mut work = self.work.lock().unwrap();
        if bytes > self.plan.reclaim_bytes - work.pending_bytes
            || work.pending_slots == self.plan.reclaim_slots
        {
            work.sync_dispose_count += 1;
            work.sync_dispose_bytes += bytes;
            drop(work);
            release.dispose();
            return;
        }
        work.releases.push_back((bytes, release));
        work.pending_slots += 1;
        work.pending_bytes += bytes;
        work.peak_pending_bytes = work.peak_pending_bytes.max(work.pending_bytes);
        self.ready.notify_one();
    }

    pub(super) fn has_retained(&self) -> bool {
        self.retention.lock().unwrap().used != 0
    }

    pub(super) fn request_idle_purge(&self) {
        self.work.lock().unwrap().purge_idle = true;
        self.ready.notify_one();
    }

    fn purge_stocks(&self) {
        // A resumed worker can recycle while purge is in flight. Leave that
        // new stock for the next idle period and return to servicing maps.
        let cutoff = self.retention.lock().unwrap().cutoff();
        loop {
            let next = self.retention.lock().unwrap().pop_oldest(cutoff);
            match next {
                Some((_, item)) => item.dispose(),
                None => return,
            }
        }
    }

    fn run(&self) {
        loop {
            let (map, release, purge) = {
                let mut work = self.work.lock().unwrap();
                while work.maps.is_empty()
                    && work.releases.is_empty()
                    && !work.purge_idle
                    && !work.closing
                {
                    work = self.ready.wait(work).unwrap();
                }
                (
                    work.maps.pop_front(),
                    work.releases.pop_front(),
                    std::mem::take(&mut work.purge_idle),
                )
            };
            if map.is_none() && release.is_none() && !purge {
                break;
            }
            if let Some(map) = map {
                map();
            }
            if let Some((bytes, release)) = release {
                release.dispose();
                let mut work = self.work.lock().unwrap();
                work.pending_bytes -= bytes;
                work.pending_slots -= 1;
            }
            if purge {
                crate::task_heap::purge();
                self.purge_stocks();
            }
        }
        self.purge_stocks();
    }

    pub(super) fn cached_unit_stack(&self) -> Option<UnitStack> {
        self.take(|stock| &mut stock.unit_stacks, |_| true)
    }

    pub(super) fn put_unit_stack(&self, stack: UnitStack) {
        let bytes = stack.len;
        self.keep(
            |stock| &mut stock.unit_stacks,
            stack,
            bytes,
            Reclaim::UnitStack,
        );
    }

    pub(super) fn report(&self) {
        let cap = self.plan.retained_bytes;
        let (cache, peak) = {
            let stock = self.retention.lock().unwrap();
            (
                [
                    stock.unit_stacks.report("unit_stack", cap),
                    stock.task_stacks.report("task_stack", cap),
                    stock.slabs.report("slab", cap),
                    stock.pages.report("page", cap),
                ],
                stock.peak,
            )
        };
        let work = self.work.lock().unwrap();
        eprintln!(
            "pool-memory {} pending_peak={} retained_cap={cap} retained_peak={peak} sync_dispose_count={} sync_dispose_bytes={}",
            cache.join(" "),
            work.peak_pending_bytes,
            work.sync_dispose_count,
            work.sync_dispose_bytes
        );
    }
}

impl SharedMemory for Memory {
    fn ready_scratch_bytes(&self) -> usize {
        self.plan.ready_scratch_bytes
    }

    #[cfg(feature = "record")]
    fn take_tape_buffer(&self) -> Vec<u8> {
        self.take(|stock| &mut stock.tapes, |_| true)
            .unwrap_or_default()
    }

    #[cfg(feature = "record")]
    fn put_tape_buffer(&self, mut bytes: Vec<u8>) {
        let capacity = bytes.capacity();
        if capacity != 0 {
            bytes.clear();
            self.keep(|stock| &mut stock.tapes, bytes, capacity, Reclaim::Tape);
        }
    }

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
        self.take(|stock| &mut stock.task_stacks, |_| true)
            .unwrap_or_else(|| self.map(move || Stack::new_with_reserve(reserve)))
    }

    fn put_task_stack(&self, stack: Stack) {
        let bytes = stack.committed_bytes();
        self.keep(
            |stock| &mut stock.task_stacks,
            stack,
            bytes,
            Reclaim::TaskStack,
        );
    }

    fn take_slab(&self) -> Option<Slab> {
        self.take(|stock| &mut stock.slabs, |_| true)
    }

    fn put_slab(&self, slab: Slab) {
        self.keep(
            |stock| &mut stock.slabs,
            slab,
            crate::task_heap::SLAB,
            Reclaim::Slab,
        );
    }

    fn dispose_slab(&self, slab: Slab) {
        self.release(crate::task_heap::SLAB, Reclaim::Slab(slab));
    }

    fn take_page(&self, granules: usize, align: usize, prefix: usize) -> Option<Extent> {
        self.take(
            |stock| &mut stock.pages,
            |extent| {
                extent.granules() == granules
                    && extent.prefix() == prefix
                    && extent.payload_aligned(align)
            },
        )
    }

    fn put_page(&self, page: Extent) {
        let bytes = page.committed_bytes();
        self.keep(|stock| &mut stock.pages, page, bytes, Reclaim::Page);
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

#[cfg(test)]
#[path = "unit_memory_tests.rs"]
mod tests;
