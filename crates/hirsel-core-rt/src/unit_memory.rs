//! Idle mappings in the unit pool. Workers hold bounded per-worker stocks of
//! guarded unit-stack and root-task-stack pairs, refilled in batches on the
//! memory thread. Reclamation runs under its own bounded budget so releases
//! never stall cold-start refills.
use crate::{
    fiber::Stack,
    task_heap::slabs::{Extent, Slab},
};
use hirsel_core_ids::launch::Metadata;
use std::{
    collections::VecDeque,
    ptr::NonNull,
    sync::{
        Condvar, Mutex, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    thread::{self, JoinHandle},
};

#[path = "unit_retention.rs"]
mod retention;
#[path = "unit_stacks.rs"]
mod stacks;
use retention::{Retention, Stock};
pub(super) use stacks::{AdmittedStacks, StackPair, UnitStack};

const BURST_BATCH_MAX: usize = 4;
const BG_REFILL_CHUNK: usize = 2;
const MAP_BURST_BEFORE_RECLAIM: usize = 8;
const RECLAIM_TURN_ITEMS: usize = 8;
const RECLAIM_TURN_BYTES: usize = 4 << 20;

#[derive(Default)]
struct WorkerStock {
    pairs: VecDeque<StackPair>,
    waiters: VecDeque<super::UP>,
    bytes: usize,
    refilling: bool,
    bg_refilling: bool,
    hits: usize,
    misses: usize,
    refills: usize,
}

type Job = Box<dyn FnOnce() + Send>;

enum Reclaim {
    UnitStack(UnitStack),
    TaskStack(Stack),
    StackPair(StackPair),
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
            Self::StackPair(pair) => drop(pair),
            Self::Slab(slab) => slab.dispose(),
            Self::Page(page) => drop(page),
            #[cfg(feature = "record")]
            Self::Tape(bytes) => drop(bytes),
        }
    }
}

#[cfg(test)]
#[derive(Default)]
pub(super) struct MemoryTrace {
    pub(super) map_queue_peak: usize,
    pub(super) releases_disposed: usize,
}

struct Work {
    maps: VecDeque<Job>,
    bg_refills: VecDeque<Job>,
    releases: VecDeque<(usize, Reclaim)>,
    pending_bytes: usize,
    pending_slots: usize,
    sync_dispose_count: usize,
    sync_dispose_bytes: usize,
    purge_idle: bool,
    peak_pending_bytes: usize,
    closing: bool,
    #[cfg(test)]
    trace: MemoryTrace,
}

pub(crate) struct Memory {
    plan: super::UntiedPlan,
    stats: bool,
    launch: OnceLock<hirsel_core_ids::launch::Metadata>,
    retention: Mutex<Retention>,
    workers: Vec<Mutex<WorkerStock>>,
    retained_used: AtomicUsize,
    retained_peak: AtomicUsize,
    admitted_bytes: AtomicUsize,
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
        let stats = super::xstats_enabled();
        Self {
            plan,
            stats,
            launch: OnceLock::new(),
            retention: Mutex::new(Retention::new(stats)),
            workers: (0..plan.workers)
                .map(|_| Mutex::new(WorkerStock::default()))
                .collect(),
            retained_used: AtomicUsize::new(0),
            retained_peak: AtomicUsize::new(0),
            admitted_bytes: AtomicUsize::new(0),
            work: Mutex::new(Work {
                maps: VecDeque::new(),
                bg_refills: VecDeque::new(),
                releases: VecDeque::with_capacity(plan.reclaim_slots.min(64)),
                pending_bytes: 0,
                pending_slots: 0,
                sync_dispose_count: 0,
                sync_dispose_bytes: 0,
                purge_idle: false,
                peak_pending_bytes: 0,
                closing: false,
                #[cfg(test)]
                trace: MemoryTrace::default(),
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

    fn try_reserve_retained(&self, bytes: usize) -> bool {
        let cap = self.plan.retained_bytes;
        let Ok(prev) =
            self.retained_used
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                    used.checked_add(bytes).filter(|&next| next <= cap)
                })
        else {
            return false;
        };
        if self.stats {
            self.retained_peak
                .fetch_max(prev + bytes, Ordering::Relaxed);
        }
        true
    }

    fn release_retained(&self, bytes: usize) {
        self.retained_used.fetch_sub(bytes, Ordering::AcqRel);
    }

    fn pair_retained_bytes(&self) -> usize {
        let page = crate::task_heap::page_bytes();
        (self.plan.unit_stack_bytes + page) + (crate::fiber::commit_for_page(page) + page)
    }

    fn stock_headroom_pairs(&self) -> usize {
        let pair_bytes = self.pair_retained_bytes();
        let used = self.retained_used.load(Ordering::Acquire);
        self.plan.retained_bytes.saturating_sub(used) / pair_bytes
    }

    pub(super) fn record_admitted(&self, bytes: usize) {
        self.admitted_bytes.fetch_add(bytes, Ordering::AcqRel);
    }

    /// Run `job` on the memory thread at urgent priority.
    pub(super) fn post(&self, job: impl FnOnce() + Send + 'static) {
        // The checker models no memory thread: a scheduled thread runs the
        // job in place, at the point it would be posted.
        #[cfg(test)]
        if super::check::modelled() {
            return job();
        }
        let mut work = self.work.lock().unwrap();
        work.maps.push_back(Box::new(job));
        #[cfg(test)]
        {
            work.trace.map_queue_peak = work
                .trace
                .map_queue_peak
                .max(work.maps.len() + work.bg_refills.len());
        }
        drop(work);
        self.ready.notify_one();
    }

    /// Run a speculative stock refill after any urgent map or waiter jobs.
    fn post_bg(&self, job: impl FnOnce() + Send + 'static) {
        #[cfg(test)]
        if super::check::modelled() {
            return job();
        }
        let mut work = self.work.lock().unwrap();
        work.bg_refills.push_back(Box::new(job));
        #[cfg(test)]
        {
            work.trace.map_queue_peak = work
                .trace
                .map_queue_peak
                .max(work.maps.len() + work.bg_refills.len());
        }
        drop(work);
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
        let (item, bytes) = self.retention.lock().unwrap().take(select, fits)?;
        self.release_retained(bytes);
        Some(item)
    }

    fn take_shared_pair(&self) -> Option<StackPair> {
        let (unit, root, bytes) = {
            let mut stock = self.retention.lock().unwrap();
            if stock.unit_stacks.is_empty() || stock.task_stacks.is_empty() {
                return None;
            }
            let (unit, u_bytes) = stock.take(|s| &mut s.unit_stacks, |_| true)?;
            let (root, r_bytes) = stock.take(|s| &mut s.task_stacks, |_| true)?;
            (unit, root, u_bytes + r_bytes)
        };
        self.release_retained(bytes);
        root.trim();
        Some(StackPair { unit, root })
    }

    fn evict_worker_pair(&self) -> Option<(usize, StackPair)> {
        for worker in &self.workers {
            let mut stock = worker.lock().unwrap();
            if let Some(pair) = stock.pairs.pop_front() {
                let bytes = pair.bytes();
                stock.bytes -= bytes;
                drop(stock);
                self.release_retained(bytes);
                return Some((bytes, pair));
            }
        }
        None
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
            if self.try_reserve_retained(bytes) {
                self.retention.lock().unwrap().keep(select, item, bytes);
                return;
            }
            let evicted = {
                let mut stock = self.retention.lock().unwrap();
                stock.pop_oldest(u64::MAX)
            };
            if let Some((old_bytes, removed)) = evicted {
                self.release_retained(old_bytes);
                self.release(old_bytes, removed);
                continue;
            }
            if let Some((old_bytes, pair)) = self.evict_worker_pair() {
                self.release(old_bytes, Reclaim::StackPair(pair));
                continue;
            }
            self.release(bytes, reclaim(item));
            return;
        }
    }

    /// Admit a guarded stack pair to `u` before dispatch, taking from worker
    /// `ix`'s local stock (or shared retention) on a hit, or coalescing `u`
    /// onto worker `ix`'s batched refill on a miss.
    /// # Safety
    /// `u` is dequeued by worker `ix` and has no stacks yet.
    pub(super) unsafe fn admit_or_refill(
        &'static self,
        p: &'static super::Pool,
        ix: usize,
        u: &super::Unit,
    ) -> bool {
        let may_prefill = super::LIVE.load(Ordering::Relaxed) > 1;
        if let Some((pair, trigger_refill)) = self.pop_worker_pair(ix, may_prefill) {
            if trigger_refill {
                self.post_bg_refill(p, ix);
            }
            // SAFETY: by the contract.
            unsafe { self.admit_pair_to_unit(u, pair, true) };
            return true;
        }
        if let Some(pair) = self.take_shared_pair() {
            // SAFETY: by the contract.
            unsafe { self.admit_pair_to_unit(u, pair, true) };
            return true;
        }
        let up = super::UP(u);
        let (hit, trigger_urgent, trigger_bg) = {
            let mut stock = self.workers[ix].lock().unwrap();
            if let Some(pair) = stock.pairs.pop_back() {
                let bytes = pair.bytes();
                stock.bytes -= bytes;
                if self.stats || cfg!(test) {
                    stock.hits += 1;
                }
                self.release_retained(bytes);
                let low = self
                    .plan
                    .worker_pair_stock
                    .saturating_sub(self.plan.pair_refill_batch);
                let trigger = may_prefill
                    && self.plan.pair_refill_batch > 0
                    && !stock.refilling
                    && !stock.bg_refilling
                    && stock.pairs.len() <= low
                    && self.stock_headroom_pairs() > 0;
                if trigger {
                    stock.bg_refilling = true;
                }
                (Some(pair), false, trigger)
            } else {
                stock.misses += 1;
                stock.waiters.push_back(up);
                let trigger = !stock.refilling;
                if trigger {
                    stock.refilling = true;
                }
                (None, trigger, false)
            }
        };
        if trigger_urgent {
            self.post_refill(p, ix);
        } else if trigger_bg {
            self.post_bg_refill(p, ix);
        }
        if let Some(pair) = hit {
            // SAFETY: by the contract.
            unsafe { self.admit_pair_to_unit(u, pair, true) };
            return true;
        }
        false
    }

    fn pop_worker_pair(&self, ix: usize, may_prefill: bool) -> Option<(StackPair, bool)> {
        let mut stock = self.workers[ix].lock().unwrap();
        let pair = stock.pairs.pop_back()?;
        let bytes = pair.bytes();
        stock.bytes -= bytes;
        if self.stats || cfg!(test) {
            stock.hits += 1;
        }
        self.release_retained(bytes);
        let low = self
            .plan
            .worker_pair_stock
            .saturating_sub(self.plan.pair_refill_batch);
        let trigger = may_prefill
            && self.plan.pair_refill_batch > 0
            && !stock.refilling
            && !stock.bg_refilling
            && stock.pairs.len() <= low
            && self.stock_headroom_pairs() > 0;
        if trigger {
            stock.bg_refilling = true;
        }
        Some((pair, trigger))
    }

    fn post_refill(&'static self, p: &'static super::Pool, ix: usize) {
        self.post(move || self.refill_worker(p, ix, true));
    }

    fn post_bg_refill(&'static self, p: &'static super::Pool, ix: usize) {
        self.post_bg(move || self.refill_worker(p, ix, false));
    }

    /// Attach and charge `pair` to `u` before dispatch; if `prepare_frame` is
    /// true, also write the initial entry frame now (else the dispatching
    /// worker writes it on its own core).
    /// # Safety
    /// `u` has no stacks and nobody else is touching it.
    pub(super) unsafe fn admit_pair_to_unit(
        &self,
        u: &super::Unit,
        pair: StackPair,
        prepare: bool,
    ) {
        let charged_bytes = pair.bytes();
        self.record_admitted(charged_bytes);
        // SAFETY: by the contract.
        unsafe {
            if prepare {
                *u.sp.get() = pair.unit.prepare_frame(
                    super::unit_entry,
                    (u as *const super::Unit).cast_mut().cast(),
                );
                u.sanitizer.reset();
            }
            *u.stack.get() = AdmittedStacks {
                unit: Some(pair.unit),
                root: Some(pair.root),
                charged_bytes,
            };
        }
    }

    fn deliver_batch(
        &self,
        p: &super::Pool,
        ix: usize,
        mut fresh: Vec<StackPair>,
        urgent: bool,
    ) -> (bool, bool) {
        let cap = self.plan.worker_pair_stock;
        let mut ready = Vec::new();
        let mut overflow = Vec::new();
        let (more_waiters, post_bg) = {
            let mut stock = self.workers[ix].lock().unwrap();
            if self.stats {
                stock.refills += 1;
            }
            while let Some(up) = stock.waiters.pop_front() {
                if let Some(pair) = fresh.pop() {
                    ready.push((up, pair));
                } else {
                    stock.waiters.push_front(up);
                    break;
                }
            }
            for pair in fresh {
                let bytes = pair.bytes();
                if stock.pairs.len() < cap && self.try_reserve_retained(bytes) {
                    stock.bytes += bytes;
                    stock.pairs.push_back(pair);
                } else {
                    overflow.push(pair);
                }
            }
            let more = !stock.waiters.is_empty();
            let mut bg = false;
            if urgent {
                if !more {
                    stock.refilling = false;
                    let live_multi = super::LIVE.load(Ordering::Relaxed) > 1 || stock.misses > 1;
                    let low = cap.saturating_sub(self.plan.pair_refill_batch);
                    if live_multi
                        && self.plan.pair_refill_batch > 0
                        && !stock.bg_refilling
                        && stock.pairs.len() <= low
                        && self.stock_headroom_pairs() > 0
                    {
                        stock.bg_refilling = true;
                        bg = true;
                    }
                }
            } else {
                stock.bg_refilling = false;
                let live_multi = super::LIVE.load(Ordering::Relaxed) > 1 || stock.misses > 1;
                let low = cap.saturating_sub(self.plan.pair_refill_batch);
                if !more
                    && live_multi
                    && self.plan.pair_refill_batch > 0
                    && !stock.refilling
                    && stock.pairs.len() <= low
                    && self.stock_headroom_pairs() > 0
                {
                    stock.bg_refilling = true;
                    bg = true;
                }
            }
            (more, bg)
        };
        if !ready.is_empty() {
            let mut units = Vec::with_capacity(ready.len());
            for (up, pair) in ready {
                // SAFETY: nobody else holds the dequeued unit until published.
                unsafe { self.admit_pair_to_unit(&*up.0, pair, false) };
                units.push(up);
            }
            super::publish_batch(p, units);
        }
        for pair in overflow {
            self.put_unit_stack(pair.unit);
            self.put_task_stack(pair.root);
        }
        (more_waiters, post_bg)
    }

    fn refill_worker(&'static self, p: &'static super::Pool, ix: usize, urgent: bool) {
        let cap = self.plan.worker_pair_stock;
        let count = {
            let mut stock = self.workers[ix].lock().unwrap();
            if urgent {
                let waiting = stock.waiters.len();
                if waiting == 0 {
                    stock.refilling = false;
                    return;
                }
                waiting.min(BURST_BATCH_MAX)
            } else {
                let room_slots = cap.saturating_sub(stock.pairs.len());
                let live_multi = super::LIVE.load(Ordering::Relaxed) > 1 || stock.misses > 1;
                let n = if live_multi {
                    self.plan
                        .pair_refill_batch
                        .min(BG_REFILL_CHUNK)
                        .min(room_slots)
                        .min(self.stock_headroom_pairs())
                } else {
                    0
                };
                if n == 0 {
                    stock.bg_refilling = false;
                    return;
                }
                n
            }
        };
        let fresh = StackPair::new_batch(count, self.plan.unit_stack_bytes);
        let (more_waiters, post_bg) = self.deliver_batch(p, ix, fresh, urgent);
        if urgent && more_waiters {
            self.post_refill(p, ix);
        } else if post_bg {
            self.post_bg_refill(p, ix);
        }
    }

    pub(super) fn release_unit_stacks(&self, ix: usize, stacks: AdmittedStacks) {
        self.admitted_bytes
            .fetch_sub(stacks.charged_bytes, Ordering::AcqRel);
        match (stacks.unit, stacks.root) {
            (Some(unit), Some(root)) if !root.is_retired() => {
                self.put_pair(ix, StackPair { unit, root })
            }
            (Some(unit), Some(root)) => {
                drop(root);
                self.put_unit_stack(unit);
            }
            (Some(unit), None) => self.put_unit_stack(unit),
            (None, Some(root)) if !root.is_retired() => {
                root.trim();
                self.put_task_stack(root);
            }
            (None, Some(root)) => drop(root),
            (None, None) => {}
        }
    }

    fn put_pair(&self, ix: usize, pair: StackPair) {
        pair.root.trim();
        let bytes = pair.bytes();
        let cap = self.plan.worker_pair_stock;
        if ix < self.workers.len() && cap > 0 {
            let mut stock = self.workers[ix].lock().unwrap();
            if stock.pairs.len() < cap && self.try_reserve_retained(bytes) {
                stock.bytes += bytes;
                stock.pairs.push_back(pair);
                return;
            }
        }
        self.put_unit_stack(pair.unit);
        self.put_task_stack(pair.root);
    }

    /// The pending bounds include the disposal currently in flight.
    fn release(&self, bytes: usize, release: Reclaim) {
        let mut work = self.work.lock().unwrap();
        if bytes > self.plan.reclaim_bytes - work.pending_bytes
            || work.pending_slots == self.plan.reclaim_slots
        {
            if self.stats || cfg!(test) {
                work.sync_dispose_count += 1;
                work.sync_dispose_bytes += bytes;
            }
            drop(work);
            release.dispose();
            return;
        }
        work.releases.push_back((bytes, release));
        work.pending_slots += 1;
        work.pending_bytes += bytes;
        if self.stats {
            work.peak_pending_bytes = work.peak_pending_bytes.max(work.pending_bytes);
        }
        self.ready.notify_one();
    }

    pub(super) fn has_retained(&self) -> bool {
        self.retained_used.load(Ordering::Acquire) != 0
    }

    pub(super) fn request_idle_purge(&self) {
        self.work.lock().unwrap().purge_idle = true;
        self.ready.notify_one();
    }

    fn purge_stocks(&self, yield_to_maps: bool) {
        for worker in &self.workers {
            loop {
                if yield_to_maps && !self.work.lock().unwrap().maps.is_empty() {
                    return;
                }
                let next = {
                    let mut stock = worker.lock().unwrap();
                    stock.pairs.pop_front().map(|pair| {
                        let bytes = pair.bytes();
                        stock.bytes -= bytes;
                        (bytes, pair)
                    })
                };
                match next {
                    Some((bytes, pair)) => {
                        self.release_retained(bytes);
                        drop(pair);
                    }
                    None => break,
                }
            }
        }
        // A resumed worker can recycle while purge is in flight. Leave that
        // new stock for the next idle period and return to servicing maps.
        let cutoff = self.retention.lock().unwrap().cutoff();
        loop {
            if yield_to_maps && !self.work.lock().unwrap().maps.is_empty() {
                return;
            }
            let next = self.retention.lock().unwrap().pop_oldest(cutoff);
            match next {
                Some((bytes, item)) => {
                    self.release_retained(bytes);
                    item.dispose();
                }
                None => return,
            }
        }
    }

    fn run(&self) {
        enum Next {
            Map(Job),
            Reclaim(Vec<(usize, Reclaim)>),
            Purge,
            Stop,
        }
        let mut maps_since_reclaim = 0usize;
        loop {
            let next = {
                let mut work = self.work.lock().unwrap();
                while work.maps.is_empty()
                    && work.bg_refills.is_empty()
                    && work.releases.is_empty()
                    && !work.purge_idle
                    && !work.closing
                {
                    work = self.ready.wait(work).unwrap();
                }
                // Reclamation runs under its own service budget: urgent maps
                // and waiter refills run ahead of background refills and
                // releases unless reclamation queue pressure exceeds half its
                // budget and a map burst has already completed.
                let reclaim_pressured = !work.releases.is_empty()
                    && (work.pending_bytes >= self.plan.reclaim_bytes / 2
                        || work.pending_slots >= self.plan.reclaim_slots / 2);
                if !work.maps.is_empty()
                    && (!reclaim_pressured || maps_since_reclaim < MAP_BURST_BEFORE_RECLAIM)
                {
                    work.purge_idle = false;
                    maps_since_reclaim += 1;
                    Next::Map(work.maps.pop_front().unwrap())
                } else if !work.bg_refills.is_empty()
                    && (!reclaim_pressured || maps_since_reclaim < MAP_BURST_BEFORE_RECLAIM)
                {
                    work.purge_idle = false;
                    maps_since_reclaim += 1;
                    Next::Map(work.bg_refills.pop_front().unwrap())
                } else if !work.releases.is_empty() {
                    maps_since_reclaim = 0;
                    let limit = if work.maps.is_empty() && work.bg_refills.is_empty() {
                        RECLAIM_TURN_ITEMS
                    } else {
                        1
                    };
                    let mut batch = Vec::with_capacity(limit.min(work.releases.len()));
                    let mut batch_bytes = 0usize;
                    while batch.len() < limit {
                        let Some((bytes, item)) = work.releases.pop_front() else {
                            break;
                        };
                        batch_bytes += bytes;
                        batch.push((bytes, item));
                        if batch_bytes >= RECLAIM_TURN_BYTES {
                            break;
                        }
                    }
                    Next::Reclaim(batch)
                } else if work.purge_idle {
                    maps_since_reclaim = 0;
                    work.purge_idle = false;
                    Next::Purge
                } else {
                    Next::Stop
                }
            };
            match next {
                Next::Map(map) => {
                    map();
                }
                Next::Reclaim(batch) => {
                    let mut disposed_bytes = 0usize;
                    let disposed_slots = batch.len();
                    for (bytes, release) in batch {
                        release.dispose();
                        disposed_bytes += bytes;
                    }
                    let mut work = self.work.lock().unwrap();
                    #[cfg(test)]
                    {
                        work.trace.releases_disposed += disposed_slots;
                    }
                    work.pending_bytes -= disposed_bytes;
                    work.pending_slots -= disposed_slots;
                }
                Next::Purge => {
                    crate::task_heap::purge();
                    self.purge_stocks(true);
                }
                Next::Stop => break,
            }
        }
        self.purge_stocks(false);
    }

    #[cfg(test)]
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
        let cache = {
            let stock = self.retention.lock().unwrap();
            [
                stock.unit_stacks.report("unit_stack", cap),
                stock.task_stacks.report("task_stack", cap),
                stock.slabs.report("slab", cap),
                stock.pages.report("page", cap),
            ]
        };
        let (mut pair_hits, mut pair_misses, mut pair_refills) = (0, 0, 0);
        for worker in &self.workers {
            let stock = worker.lock().unwrap();
            pair_hits += stock.hits;
            pair_misses += stock.misses;
            pair_refills += stock.refills;
        }
        let peak = self.retained_peak.load(Ordering::Relaxed);
        let work = self.work.lock().unwrap();
        eprintln!(
            "pool-memory {} pair_hits={pair_hits} pair_misses={pair_misses} pair_refills={pair_refills} pending_peak={} retained_cap={cap} retained_peak={peak} sync_dispose_count={} sync_dispose_bytes={}",
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
                    let launch = crate::host::native_metadata(&target, n_place, describe);
                    let _ = launch.digest();
                    launch
                })
                .clone()
        })
    }

    fn take_task_stack(&self) -> Stack {
        self.take(|stock| &mut stock.task_stacks, |_| true)
            .unwrap_or_else(|| self.map(Stack::new))
    }

    fn put_task_stack(&self, stack: Stack) {
        if stack.is_retired() || crate::run_stop::is_ended() {
            drop(stack);
            return;
        }
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
