//! Idle mappings in the unit pool. Workers hand off excess mappings and ask
//! the memory thread for fresh ones on a miss. The thread owns the syscalls.
use crate::{
    fiber::Stack,
    task_heap::{Mapping, slabs::Slab},
};
use hirsel_core_ids::launch::Metadata;
use std::{
    collections::VecDeque,
    ptr::NonNull,
    sync::{Condvar, Mutex, OnceLock, mpsc},
    thread::{self, JoinHandle},
};

const MIB: usize = 1 << 20;
const UNIT_STACK_BYTES: usize = MIB;
const RELEASE_SLOTS: usize = 32_768;

pub(super) struct UnitStack {
    base: NonNull<u8>,
    len: usize,
}

// SAFETY: ownership of the mapping moves with the stack. A worker gives it
// back only after switching to its host stack for the last time.
unsafe impl Send for UnitStack {}

impl UnitStack {
    fn new() -> Self {
        let guard = crate::task_heap::page_bytes();
        let len = UNIT_STACK_BYTES + guard;
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
    Page(Mapping),
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
    launch: OnceLock<hirsel_core_ids::launch::Metadata>,
    unit_stacks: Cache<UnitStack>,
    task_stacks: Cache<Stack>,
    slabs: Cache<Slab>,
    pages: Cache<Mapping>,
    work: Mutex<Work>,
    ready: Condvar,
    release_room: Condvar,
}

/// The storage operations a running task can request from its unit pool.
pub(crate) trait SharedMemory: Sync {
    fn native_metadata(&self, target: &str, n_place: u64, describe: bool) -> Metadata;
    fn signal_stack(&self) -> Stack;
    fn start_sync(&self, start: fn() -> Result<(), i32>) -> Result<(), i32>;
    fn take_task_stack(&self) -> Stack;
    fn put_task_stack(&self, stack: Stack);
    fn take_slab(&self) -> Option<Slab>;
    fn put_slab(&self, slab: Slab);
    fn dispose_slab(&self, slab: Slab);
    fn aligned_page(
        &self,
        bytes: usize,
        space: usize,
        align: usize,
        headroom: usize,
    ) -> Option<Mapping>;
    fn put_page(&self, page: Mapping);
}

impl Memory {
    pub(super) fn new() -> Self {
        Self {
            launch: OnceLock::new(),
            unit_stacks: Cache::new(64 * MIB, UNIT_STACK_BYTES),
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
            release_room: Condvar::new(),
        }
    }

    pub(super) fn start(&'static self) -> MemoryThread {
        let thread = thread::Builder::new()
            .name("hirsel-memory".into())
            .stack_size(256 << 10)
            .spawn(|| self.run())
            .unwrap();
        MemoryThread {
            memory: self,
            thread: Some(thread),
        }
    }

    pub(crate) fn map<T: Send + 'static>(&self, map: impl FnOnce() -> T + Send + 'static) -> T {
        let (send, receive) = mpsc::sync_channel(1);
        self.work.lock().unwrap().maps.push_back(Box::new(move || {
            let _ = send.send(map());
        }));
        self.ready.notify_one();
        match receive.try_recv() {
            Ok(value) => value,
            Err(mpsc::TryRecvError::Empty) => {
                let _blocking = super::blocking();
                receive.recv().unwrap()
            }
            Err(mpsc::TryRecvError::Disconnected) => panic!("memory thread disconnected"),
        }
    }

    fn release(&self, bytes: usize, release: Reclaim) {
        let mut work = self.work.lock().unwrap();
        while work.releases.len() == RELEASE_SLOTS {
            let _blocking = super::blocking();
            work = self.release_room.wait(work).unwrap();
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
                if work.releases.len() == RELEASE_SLOTS {
                    self.release_room.notify_all();
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

    pub(super) fn take_unit_stack(&self) -> UnitStack {
        self.unit_stacks
            .take(|_| true)
            .unwrap_or_else(|| self.map(UnitStack::new))
    }

    pub(super) fn put_unit_stack(&self, stack: UnitStack) {
        let bytes = stack.len;
        if let Some(stack) = self.unit_stacks.put(stack, bytes) {
            self.release(bytes, Reclaim::UnitStack(stack));
        }
    }

    pub(crate) fn take_page(&self, fits: impl FnMut(&Mapping) -> bool) -> Option<Mapping> {
        self.pages.take(fits)
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
        self.launch
            .get_or_init(|| {
                let target = target.to_owned();
                self.map(move || {
                    let launch = Metadata::native(&target, n_place, describe);
                    let _ = launch.digest();
                    launch
                })
            })
            .clone()
    }

    fn signal_stack(&self) -> Stack {
        self.map(Stack::new)
    }

    fn start_sync(&self, start: fn() -> Result<(), i32>) -> Result<(), i32> {
        self.map(start)
    }

    fn take_task_stack(&self) -> Stack {
        self.task_stacks
            .take(|_| true)
            .unwrap_or_else(|| self.map(Stack::new))
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

    fn aligned_page(
        &self,
        bytes: usize,
        space: usize,
        align: usize,
        headroom: usize,
    ) -> Option<Mapping> {
        Mapping::pooled_page(self, bytes, space, align, headroom)
    }

    fn put_page(&self, page: Mapping) {
        let bytes = page.committed_bytes();
        if let Some(page) = self.pages.put(page, bytes) {
            self.release(bytes, Reclaim::Page(page));
        }
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
