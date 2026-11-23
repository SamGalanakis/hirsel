//! Bounded observations of the serial replay runtime, and per-worker sampled
//! backing lifetimes for a live run.
use crate::mem_sampler::Sampler;
use crate::prof::{self, Kind};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

const TOP_SCOPES: usize = 20;
const DEFAULT_ENTRIES: usize = 65_536;
const METRIC_VERSION: u64 = 2;

#[derive(Clone, Copy)]
pub(crate) enum Backing {
    Block = 0,
    TaskHeap = 1,
    Region = 2,
    TaskStorage = 3,
    Channel = 4,
    CauseCell = 5,
}

#[derive(Clone, Copy, Debug, Default)]
struct Peak {
    bytes: u64,
    timestamp: u64,
    epoch: u64,
}

impl Peak {
    fn sample(&mut self, bytes: u64, epoch: u64) -> bool {
        if self.timestamp == 0 || bytes > self.bytes {
            *self = Self {
                bytes,
                timestamp: prof::now_ns(),
                epoch,
            };
            return true;
        }
        false
    }
}

struct Allocation {
    id: u64,
}

struct LiveRegion {
    pointer: *const crate::region::Region,
    /// A region can suspend with an exclusive Rust borrow during admission or
    /// refund. Its tail cannot change until the borrow is handed back.
    suspended_tail: Option<u64>,
}

#[derive(Clone, Copy)]
struct Scope {
    id: u64,
    task: u64,
    parent: u64,
    kind: u64,
    slot: usize,
    limit: u64,
    peak: Peak,
    enter: u64,
    exit: u64,
    refused: bool,
    physical: [Peak; 2],
}

impl Scope {
    fn near_miss(self) -> bool {
        crate::mem_sampler::near_miss(self.peak.bytes, self.limit)
    }

    fn words(self) -> [u64; 10] {
        [
            self.id,
            self.task,
            self.parent,
            self.kind,
            self.limit,
            self.peak.bytes,
            self.enter,
            self.exit,
            self.id,
            u64::from(self.refused),
        ]
    }

    fn usage(self) -> (u128, u128) {
        (u128::from(self.peak.bytes), u128::from(self.limit))
    }

    fn emit_peaks(self) {
        emit_peak(0, self.id, self.peak);
        for (metric, peak) in self.physical.into_iter().enumerate() {
            emit_peak(metric as u64 + 1, self.id, peak);
        }
    }
}

struct Run {
    capacity: usize,
    sampler: Option<Sampler>,
    root: usize,
    limit: u64,
    epoch: u64,
    retained: u64,
    peaks: [Peak; 3],
    allocations: HashMap<usize, Allocation>,
    regions: HashMap<usize, LiveRegion>,
    scopes: Vec<Scope>,
    near: Vec<Scope>,
    next_id: u64,
    state_drops: u64,
    scope_refusals: HashMap<u64, (u64, u64)>,
}

thread_local! {
    static RUN: RefCell<Option<Run>> = const { RefCell::new(None) };
}

pub(crate) fn enabled() -> bool {
    LIVE.get().is_some() || RUN.with_borrow(Option::is_some)
}

pub(crate) struct Settings {
    pub(crate) sampled: bool,
    capacity: usize,
}

pub(crate) fn read_settings() -> Option<Settings> {
    let mode = std::env::var("HIRSEL_PROFILE_MEM").ok()?;
    let capacity = std::env::var("HIRSEL_PROFILE_MEM_ENTRIES")
        .ok()
        .and_then(|n| n.parse().ok())
        .filter(|&size| (1..=1_048_576).contains(&size))
        .unwrap_or(DEFAULT_ENTRIES);
    // SAFETY: the launcher calls this before any program threads or grant
    // snapshots. The profiler's observation settings are host-only inputs.
    unsafe {
        std::env::remove_var("HIRSEL_PROFILE_MEM");
        std::env::remove_var("HIRSEL_PROFILE_MEM_ENTRIES");
    }
    let sampled = match mode.as_str() {
        "exact" => false,
        "sampled" => true,
        _ => return None,
    };
    Some(Settings { sampled, capacity })
}

pub(crate) fn start(Settings { sampled, capacity }: Settings) {
    let sampler = sampled.then(|| Sampler::new(prof::now_ns()));
    RUN.with_borrow_mut(|run| {
        assert!(run.is_none(), "one memory profile per serial replay worker");
        *run = Some(Run {
            capacity,
            sampler,
            root: 0,
            limit: 0,
            epoch: 0,
            retained: 0,
            peaks: [Peak::default(); 3],
            allocations: HashMap::with_capacity(capacity),
            regions: HashMap::with_capacity(capacity),
            scopes: Vec::with_capacity(capacity),
            near: Vec::with_capacity(TOP_SCOPES),
            next_id: 1,
            state_drops: 0,
            scope_refusals: HashMap::with_capacity(capacity),
        });
    });
    super::prof_stack::prepare();
}

pub(crate) fn finish() {
    if let Some(live) = LIVE.get() {
        prof::append(
            Kind::MemoryMeta,
            &[METRIC_VERSION, live.lost.load(Ordering::Relaxed)],
            &[],
        );
        return;
    }
    RUN.with_borrow_mut(|slot| {
        let Some(mut run) = slot.take() else { return };
        prof::append(Kind::MemoryMeta, &[METRIC_VERSION, run.state_drops], &[]);
        if run.sampler.is_none() {
            for (metric, peak) in run.peaks.into_iter().enumerate() {
                emit_peak(metric as u64, 0, peak);
            }
        }
        for scope in std::mem::take(&mut run.scopes) {
            if run.sampler.is_none() && scope.kind == 0 {
                scope.emit_peaks();
            }
            if scope.refused {
                prof::append(Kind::BudgetScope, &scope.words(), &[]);
            } else {
                run.keep(scope);
            }
        }
        for scope in run.near {
            prof::append(Kind::BudgetScope, &scope.words(), &[]);
        }
    });
}

fn with_run(f: impl FnOnce(&mut Run)) {
    RUN.with_borrow_mut(|slot| {
        if let Some(run) = slot {
            f(run);
        }
    });
}

impl Run {
    fn id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }
    fn event(&mut self) -> u64 {
        self.epoch += 1;
        self.epoch
    }
    fn loss(&mut self) {
        self.state_drops += 1;
    }
    fn physical_peak(&mut self, metric: usize, bytes: u64) -> bool {
        if self.sampler.is_some() {
            return false;
        }
        let mut changed = self.peaks[metric].sample(bytes, self.epoch);
        let task = crate::tasks::current_id();
        for scope in self
            .scopes
            .iter_mut()
            .filter(|scope| scope.task == task && scope.kind == 0)
        {
            changed |= scope.physical[metric - 1].sample(bytes, self.epoch);
        }
        changed
    }
    fn sample_slot(&mut self, slot: usize, available: u64) {
        if self.root == slot && self.sampler.is_none() {
            self.peaks[0].sample(self.limit.wrapping_sub(available), self.epoch);
        }
        if let Some(scope) = self
            .scopes
            .iter_mut()
            .rev()
            .find(|scope| scope.slot == slot)
        {
            scope
                .peak
                .sample(scope.limit.wrapping_sub(available), self.epoch);
        }
    }
    fn allocation(&mut self, address: usize, bytes: u64, backing: Backing) {
        let probability = match &mut self.sampler {
            Some(sampler) => sampler.include(bytes),
            None => {
                self.retained += bytes;
                Some(1.0)
            }
        };
        if let Some(probability) = probability {
            if self.allocations.len() == self.capacity {
                self.loss();
            } else {
                let id = self.id();
                self.allocations.insert(address, Allocation { id });
                let epoch = self.event();
                super::prof_stack::capture(
                    Kind::Alloc,
                    &[0, id, bytes, backing as u64, probability.to_bits(), epoch],
                );
            }
        }
        self.physical_peak(1, self.retained);
    }
    fn free(&mut self, address: usize, bytes: u64) {
        if self.sampler.is_none() {
            if let Some(remaining) = self.retained.checked_sub(bytes) {
                self.retained = remaining;
            } else {
                self.loss();
            }
        }
        if let Some(a) = self.allocations.remove(&address) {
            let epoch = self.event();
            prof::append(Kind::Alloc, &[1, a.id, bytes, 0, 0, epoch], &[]);
        }
    }
    fn parent(&self, task: u64) -> u64 {
        self.scopes
            .iter()
            .rev()
            .find(|scope| scope.task == task)
            .map_or(0, |scope| scope.id)
    }
    fn keep(&mut self, scope: Scope) {
        if !scope.near_miss() {
            return;
        }
        let less = |a: Scope, b: Scope| {
            let (an, ad) = a.usage();
            let (bn, bd) = b.usage();
            (an * bd, b.id) < (bn * ad, a.id)
        };
        if self.near.len() < TOP_SCOPES {
            self.near.push(scope);
        } else {
            let mut lowest = 0;
            for i in 1..self.near.len() {
                if less(self.near[i], self.near[lowest]) {
                    lowest = i;
                }
            }
            if less(self.near[lowest], scope) {
                self.near[lowest] = scope;
            }
        }
    }
}

fn emit_peak(metric: u64, scope: u64, peak: Peak) {
    prof::append(
        Kind::Peak,
        &[metric, scope, peak.timestamp, peak.bytes, peak.epoch],
        &[],
    );
}

pub(crate) fn root(slot: usize, available: u64) {
    with_run(|run| {
        run.root = slot;
        run.limit = available;
        run.sample_slot(slot, available);
        run.physical_peak(1, 0);
        run.physical_peak(2, 0);
    });
}

pub(crate) fn slot(slot: usize, available: u64) {
    with_run(|run| run.sample_slot(slot, available));
}

pub(crate) fn forward(old: usize, new: usize) {
    with_run(|run| {
        if run.root == old {
            run.root = new;
        }
        for scope in run.scopes.iter_mut().filter(|scope| scope.slot == old) {
            scope.slot = new;
        }
    });
}

pub(crate) fn alloc(pointer: *const u8, bytes: usize, backing: Backing) {
    if let Some(live) = LIVE.get() {
        return live.allocate(pointer.addr(), bytes as u64, backing);
    }
    with_run(|run| run.allocation(pointer.addr(), bytes as u64, backing));
}

pub(crate) fn resize(pointer: *const u8, old_bytes: usize, bytes: usize) {
    if let Some(live) = LIVE.get() {
        return live.resize(pointer.addr(), bytes as u64);
    }
    with_run(|run| {
        let bytes = bytes as u64;
        if run.sampler.is_none() {
            run.retained = run.retained - old_bytes as u64 + bytes;
        }
        if let Some(a) = run.allocations.get(&pointer.addr()) {
            let id = a.id;
            let epoch = run.event();
            prof::append(Kind::Alloc, &[2, id, bytes, 0, 0, epoch], &[]);
        }
        run.physical_peak(1, run.retained);
    });
}

pub(crate) fn free(pointer: *const u8, bytes: usize) {
    if let Some(live) = LIVE.get() {
        return live.free(pointer.addr(), bytes as u64);
    }
    with_run(|run| run.free(pointer.addr(), bytes as u64));
}

/// The pinned ABI handle remains valid until matching close.
pub(crate) fn region_open(region: *const crate::region::Region) {
    with_run(|run| {
        if run.regions.len() == run.capacity {
            run.loss();
        } else {
            run.regions.insert(
                region.addr(),
                LiveRegion {
                    pointer: region,
                    suspended_tail: None,
                },
            );
        }
    });
}

pub(crate) struct BorrowedRegion(usize);

pub(crate) fn region_borrow(region: &crate::region::Region) -> BorrowedRegion {
    let address = std::ptr::from_ref(region).addr();
    with_run(|run| {
        if let Some(state) = run.regions.get_mut(&address) {
            state.suspended_tail = Some((region.physical_bytes() - region.occupied) as u64);
        }
    });
    BorrowedRegion(address)
}

impl Drop for BorrowedRegion {
    fn drop(&mut self) {
        with_run(|run| {
            if let Some(state) = run.regions.get_mut(&self.0) {
                state.suspended_tail = None;
            }
        });
    }
}

pub(crate) fn region_refill(region: &crate::region::Region, pointer: *const u8, bytes: usize) {
    if let Some(live) = LIVE.get() {
        return live.allocate(pointer.addr(), bytes as u64, Backing::Region);
    }
    with_run(|run| {
        run.allocation(pointer.addr(), bytes as u64, Backing::Region);
        observe_slack(run, Some(region));
    });
}

fn observe_slack(run: &mut Run, active: Option<&crate::region::Region>) {
    if run.sampler.is_some() {
        return;
    }
    let epoch = run.event();
    let tail = |state: &LiveRegion| {
        if let Some(bytes) = state.suspended_tail {
            return bytes;
        }
        let region = match active {
            Some(active) if std::ptr::eq(active, state.pointer) => active,
            // SAFETY: observed handles remain pinned until close. Replay runs
            // serially, and any active exclusive borrow is supplied above.
            _ => unsafe { &*state.pointer },
        };
        (region.physical_bytes() - region.occupied) as u64
    };
    let total = run.regions.values().map(tail).sum();
    if run.physical_peak(2, total) {
        let mut unknown = 0;
        for (&address, state) in &run.regions {
            let bytes = tail(state);
            if bytes != 0 {
                if let Some(allocation) = run.allocations.get(&address) {
                    prof::append(Kind::SlackMember, &[epoch, allocation.id, bytes], &[]);
                } else {
                    unknown += bytes;
                }
            }
        }
        if unknown != 0 {
            prof::append(Kind::SlackMember, &[epoch, 0, unknown], &[]);
        }
    }
}

pub(crate) fn region_close(region: *const crate::region::Region) {
    with_run(|run| {
        run.regions.remove(&region.addr());
    });
}

pub(crate) fn capacity_raise(requested: u64, available: u64) {
    with_run(|run| {
        let task = crate::tasks::current_id();
        if run.scope_refusals.remove(&task) != Some((requested, available)) {
            open_scope(run, 2, 0, 0, true);
        }
    });
}

pub(crate) fn byte_scope(slot: usize, bytes: u64, error: Option<&crate::Capacity>) {
    with_run(|run| {
        if let Some(error) = error {
            let task = crate::tasks::current_id();
            if run.scope_refusals.len() < run.capacity {
                run.scope_refusals
                    .insert(task, (error.requested, error.available));
            } else {
                run.loss();
            }
        }
        open_scope(run, 0, slot, bytes, error.is_some());
    });
}

pub(crate) fn scope_enter(kind: u64, slot: usize, limit: u64, refused: bool) {
    with_run(|run| open_scope(run, kind, slot, limit, refused));
}

fn open_scope(run: &mut Run, kind: u64, slot: usize, limit: u64, refused: bool) {
    let id = run.id();
    let task = crate::tasks::current_id();
    let parent = if kind == 2 { 0 } else { run.parent(task) };
    let scope = Scope {
        id,
        task,
        parent,
        kind,
        slot,
        limit,
        peak: Peak {
            timestamp: prof::now_ns(),
            epoch: run.epoch,
            bytes: 0,
        },
        enter: prof::now_ns(),
        exit: 0,
        refused,
        physical: [Peak::default(); 2],
    };
    if refused {
        super::prof_stack::capture(Kind::BudgetScope, &scope.words());
    } else if run.scopes.len() == run.capacity {
        run.loss();
    } else {
        super::prof_stack::capture(Kind::ScopeStack, &[id]);
        run.scopes.push(scope);
        if kind == 0 {
            run.physical_peak(1, run.retained);
            observe_slack(run, None);
        }
    }
}

pub(crate) fn step_remaining(left: u64) {
    with_run(|run| {
        let task = crate::tasks::current_id();
        if let Some(scope) = run
            .scopes
            .iter_mut()
            .rev()
            .find(|s| s.task == task && s.kind == 1)
        {
            scope
                .peak
                .sample(scope.limit.saturating_sub(left), run.epoch);
        }
    });
}

pub(crate) fn step_refused() {
    with_run(|run| {
        let task = crate::tasks::current_id();
        if let Some(scope) = run
            .scopes
            .iter_mut()
            .rev()
            .find(|s| s.task == task && s.kind == 1)
        {
            scope.peak.sample(scope.limit, run.epoch);
            scope.refused = true;
        }
    });
}

pub(crate) fn scope_exit(kind: u64) {
    with_run(|run| {
        let task = crate::tasks::current_id();
        let Some(index) = run
            .scopes
            .iter()
            .rposition(|s| s.task == task && s.kind == kind)
        else {
            return;
        };
        let mut scope = run.scopes.remove(index);
        scope.exit = prof::now_ns();
        if run.sampler.is_none() && scope.kind == 0 {
            scope.emit_peaks();
        }
        if scope.refused {
            prof::append(Kind::BudgetScope, &scope.words(), &[]);
        } else {
            run.keep(scope);
        }
    });
}

/// Workers draw independent geometric byte gaps. The sample stays with the
/// address when ownership moves to a different worker.
static LIVE: OnceLock<Live> = OnceLock::new();
const WORKER_SHIFT: u32 = 40;

struct Live {
    samples: Mutex<HashMap<usize, LiveSample>>,
    capacity: usize,
    // Each sampled address marks its bucket for its whole lifetime. Collisions
    // can cause an extra lookup. An unmarked bucket always rules out a sample.
    marks: Box<[AtomicU32]>,
    /// Sampled allocations the table could not hold, and frees of sampled
    /// allocations on a thread with no runtime stream.
    lost: AtomicU64,
    workers: AtomicU64,
}

struct LiveSample {
    id: u64,
    bytes: u64,
}

struct LiveWorker {
    sampler: Sampler,
    prefix: u64,
    next: u64,
}

thread_local! {
    static LIVE_WORKER: RefCell<Option<LiveWorker>> = const { RefCell::new(None) };
}

pub(crate) fn start_live(Settings { capacity, .. }: Settings) {
    let slots = (capacity * 2).next_power_of_two();
    let live = Live {
        samples: Mutex::new(HashMap::new()),
        capacity,
        marks: (0..slots).map(|_| AtomicU32::new(0)).collect(),
        lost: AtomicU64::new(0),
        workers: AtomicU64::new(1),
    };
    assert!(
        LIVE.set(live).is_ok(),
        "one live memory profile per process"
    );
}

impl Live {
    #[inline]
    fn mark(&self, address: usize) -> &AtomicU32 {
        let hash = (address >> 3) ^ (address >> 19);
        &self.marks[hash & (self.marks.len() - 1)]
    }

    fn allocate(&self, address: usize, bytes: u64, backing: Backing) {
        let Some((probability, id)) = LIVE_WORKER.with_borrow_mut(|worker| {
            if worker.is_none() {
                if !prof::enabled() {
                    return None;
                }
                *worker = Some(self.worker());
            }
            let worker = worker.as_mut().unwrap();
            let probability = worker.sampler.include(bytes)?;
            worker.next += 1;
            Some((probability, worker.prefix | worker.next))
        }) else {
            return;
        };
        if !prof::enabled() {
            return;
        }
        let mut samples = self.samples.lock().unwrap();
        if samples.len() == self.capacity {
            self.lost.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let ts = prof::now_ns();
        if let Some(old) = samples.insert(address, LiveSample { id, bytes }) {
            prof::append(Kind::Alloc, &[1, old.id, 0, 0, 0, ts], &[]);
        } else {
            self.mark(address).fetch_add(1, Ordering::Relaxed);
        }
        drop(samples);
        super::prof_stack::capture(
            Kind::Alloc,
            &[0, id, bytes, backing as u64, probability.to_bits(), ts],
        );
    }

    fn worker(&self) -> LiveWorker {
        // SAFETY: gettid has no operands.
        let tid = unsafe { libc::syscall(libc::SYS_gettid) } as u64;
        super::prof_stack::prepare();
        LiveWorker {
            sampler: Sampler::new(prof::now_ns() ^ tid.wrapping_mul(0xff51_afd7_ed55_8ccd)),
            prefix: self.workers.fetch_add(1, Ordering::Relaxed) << WORKER_SHIFT,
            next: 0,
        }
    }

    fn resize(&self, address: usize, bytes: u64) {
        if self.mark(address).load(Ordering::Relaxed) == 0 {
            return;
        }
        if let Some(sample) = self.samples.lock().unwrap().get_mut(&address) {
            sample.bytes = bytes;
            prof::append(
                Kind::Alloc,
                &[2, sample.id, bytes, 0, 0, prof::now_ns()],
                &[],
            );
        }
    }

    fn free(&self, address: usize, bytes: u64) {
        let mark = self.mark(address);
        if mark.load(Ordering::Relaxed) == 0 {
            return;
        }
        let mut samples = self.samples.lock().unwrap();
        let Some(sample) = samples.remove(&address) else {
            return;
        };
        mark.fetch_sub(1, Ordering::Relaxed);
        if prof::enabled() {
            prof::append(
                Kind::Alloc,
                &[1, sample.id, bytes, 0, 0, prof::now_ns()],
                &[],
            );
        } else {
            self.lost.fetch_add(1, Ordering::Relaxed);
        }
    }
}

pub(crate) struct Residency {
    pub(crate) timestamp: u64,
    pub(crate) rss: u64,
    pub(crate) samples: Vec<(u64, u64)>,
}

pub(crate) fn residency() -> Option<Residency> {
    let live = LIVE.get()?;
    let page = crate::task_heap::page_bytes();
    let samples = live.samples.lock().unwrap();
    let timestamp = prof::now_ns();
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let rss = statm.split_ascii_whitespace().nth(1)?.parse::<u64>().ok()? * page as u64;
    let samples = samples
        .iter()
        .filter_map(|(&address, sample)| {
            let bytes = resident_bytes(address, sample.bytes as usize, page)?;
            Some((sample.id, bytes))
        })
        .collect();
    Some(Residency {
        timestamp,
        rss,
        samples,
    })
}

fn resident_bytes(address: usize, bytes: usize, page: usize) -> Option<u64> {
    let end = address.checked_add(bytes)?;
    let mut start = address & !(page - 1);
    let mut resident = 0;
    let mut pages = [0u8; 256];
    while start < end {
        let count = (end - start).div_ceil(page).min(pages.len());
        // SAFETY: mincore looks up the virtual page range in the kernel. It
        // does not dereference that range and writes at most count result bytes.
        if unsafe { libc::mincore(start as *mut libc::c_void, count * page, pages.as_mut_ptr()) }
            != 0
        {
            return None;
        }
        for &entry in &pages[..count] {
            if entry & 1 != 0 {
                resident += (start + page).min(end) - start.max(address);
            }
            start += page;
        }
    }
    Some(resident as u64)
}
