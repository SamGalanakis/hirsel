//! Unit identity publication and reuse. Records outlive their registrations.
use super::*;

const CHUNK: usize = 1024;
const CHUNKS_PER_GROUP: usize = 64;
type Chunk = [AtomicPtr<Unit>; CHUNK];
type Group = [OnceLock<Box<Chunk>>; CHUNKS_PER_GROUP];

pub(super) struct UnitDirectory {
    first_group: OnceLock<Box<[OnceLock<Box<Chunk>>]>>,
    first_group_len: usize,
    overflow: Box<[OnceLock<Box<Group>>]>,
    capacity: u32,
    // Serialize ID reuse and the live count with retirement so a concurrent
    // spawner never sees a full directory while a slot is being returned.
    claims: Mutex<Claims>,
}

struct Claims {
    next: u32,
    spare: Vec<UP>,
}

enum Claim {
    Fresh(u32),
    Reuse(UP),
}

impl UnitDirectory {
    pub(super) fn new(initial: usize, capacity: u32, logical_units_hint: u64) -> Self {
        let hinted_units = logical_units_hint.min(u64::from(capacity)) as usize;
        let first_group_len = hinted_units.div_ceil(CHUNK).min(CHUNKS_PER_GROUP);
        let remaining_chunks = (capacity as usize).div_ceil(CHUNK) - first_group_len;
        let groups = remaining_chunks.div_ceil(CHUNKS_PER_GROUP);
        Self {
            first_group: OnceLock::new(),
            first_group_len,
            overflow: (0..groups).map(|_| OnceLock::new()).collect(),
            capacity,
            claims: Mutex::new(Claims {
                next: initial as u32,
                spare: Vec::new(),
            }),
        }
    }

    fn chunk_position(&self, id: u32) -> (usize, usize) {
        let chunk = id as usize / CHUNK;
        match chunk.checked_sub(self.first_group_len) {
            Some(overflow) => (1 + overflow / CHUNKS_PER_GROUP, overflow % CHUNKS_PER_GROUP),
            None => (0, chunk),
        }
    }

    pub(super) fn slot(&self, id: u32) -> Option<&AtomicPtr<Unit>> {
        if id >= self.capacity {
            return None;
        }
        let (group, chunk) = self.chunk_position(id);
        let pointers: &[OnceLock<Box<Chunk>>] = if group == 0 {
            self.first_group.get()?
        } else {
            self.overflow[group - 1].get()?.as_ref()
        };
        Some(&pointers[chunk].get()?[id as usize % CHUNK])
    }

    pub(super) fn get(&self, id: u32) -> Option<&Unit> {
        let p = self.slot(id)?.load(Ordering::Acquire);
        // SAFETY: once published, the leaked record outlives the directory.
        unsafe { p.as_ref() }
    }

    fn install(&self, unit: &'static Unit) {
        let index = unit.id as usize;
        let (group, chunk) = self.chunk_position(unit.id);
        let pointers: &[OnceLock<Box<Chunk>>] = if group == 0 {
            self.first_group
                .get_or_init(|| (0..self.first_group_len).map(|_| OnceLock::new()).collect())
        } else {
            self.overflow[group - 1]
                .get_or_init(|| Box::new([const { OnceLock::new() }; CHUNKS_PER_GROUP]))
                .as_ref()
        };
        let slots = pointers[chunk]
            .get_or_init(|| Box::new([const { AtomicPtr::new(ptr::null_mut()) }; CHUNK]));
        if slots[index % CHUNK]
            .compare_exchange(
                ptr::null_mut(),
                ptr::from_ref(unit).cast_mut(),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            crate::run_stop::raise(crate::run_stop::Stop::from_static(b"UnitIdTaken"));
        }
    }

    fn claim(&self) -> Result<Claim, usize> {
        let mut claims = self.claims.lock().unwrap();
        let live = LIVE.load(Ordering::SeqCst);
        if live == 0 {
            drop(claims);
            crate::run_stop::raise(crate::run_stop::Stop::from_static(b"UnitAfterStop"));
        }
        let claim = if let Some(spare) = claims.spare.pop() {
            Claim::Reuse(spare)
        } else if claims.next < self.capacity {
            let id = claims.next;
            claims.next += 1;
            Claim::Fresh(id)
        } else {
            return Err(live);
        };
        LIVE.fetch_add(1, Ordering::SeqCst);
        Ok(claim)
    }

    /// Publish a finished helper's free record and release its live count.
    /// True when there is no further work and admission is closed.
    pub(super) fn retire(&self, up: UP) -> bool {
        let mut claims = self.claims.lock().unwrap();
        claims.spare.push(up);
        LIVE.fetch_sub(1, Ordering::SeqCst) == 1
    }
}

pub(crate) enum SpawnErr {
    NotPooled(Job),
    Exhausted {
        live: usize,
        max_units: u32,
        physical: bool,
    },
}

/// Run a new helper beside the caller. Claim its directory slot before
/// changing LIVE, allocating a record or publishing runnable work.
pub(crate) fn spawn_job(job: Job) -> Result<(), SpawnErr> {
    if !untied() || cur().is_null() {
        return Err(SpawnErr::NotPooled(job));
    }
    let p = pool();
    match p.units.claim() {
        Ok(Claim::Reuse(up)) => {
            // SAFETY: this ended record was removed from the free list.
            // A late wake of its old run is a spurious turn of the new one.
            let u = unsafe { &*up.0 };
            u.after.set(After::Run);
            u.status.store(0, Ordering::Relaxed);
            u.stop_fd.set(2);
            u.first_park.set(false);
            u.timer_at.store(u64::MAX, Ordering::Relaxed);
            u.alone_depth.set(0);
            u.job.set(Some(job));
            u.state.store(QUEUED, Ordering::SeqCst);
            bump(S_CREATED);
            enqueue(p, u);
        }
        Ok(Claim::Fresh(id)) => {
            // A helper belongs to the execution that spawns it: the spawning
            // unit's, or the calling thread's when no unit runs here.
            // SAFETY: a non-null current unit is this thread's running unit.
            let execution = match unsafe { cur().as_ref() } {
                Some(unit) => unit.execution,
                None => crate::run_stop::current_execution().unwrap(),
            };
            let unit = new_unit(id, execution);
            unit.job.set(Some(job));
            publish_unit(unit);
        }
        Err(live) => {
            return Err(SpawnErr::Exhausted {
                live,
                max_units: p.plan.max_units,
                physical: p.plan.physical_unit_limit,
            });
        }
    }
    Ok(())
}

pub(crate) fn spawn_language_job(job: Job) {
    match spawn_job(job) {
        Ok(()) => {}
        Err(SpawnErr::NotPooled(job)) => {
            crate::tasks::spawn_unit_thread(job);
        }
        Err(SpawnErr::Exhausted {
            live,
            max_units,
            physical,
        }) => {
            if !crate::run_stop::is_contained() {
                crate::os::write_all(
                    crate::fiber::stop_fd(),
                    format!("untied unit limit: live={live} max_units={max_units}\n").as_bytes(),
                );
            }
            crate::run_stop::raise(crate::run_stop::Stop::from_static(if physical {
                b"HostMemory"
            } else {
                b"UnitIdSpace"
            }));
        }
    }
}

#[cfg(test)]
pub(super) fn create_unit(id: u32, execution: &crate::run_stop::Execution) {
    publish_unit(new_unit(id, execution));
}

/// Create an initial unit with its guarded stack pair already admitted before
/// any worker dispatches it.
pub(super) fn create_initial_unit(id: u32, execution: &crate::run_stop::Execution) {
    let unit = Box::leak(new_unit(id, execution));
    let p = pool();
    let pair = StackPair::new(p.plan.unit_stack_bytes, p.plan.task_stack_reserve);
    // SAFETY: `unit` has not been published yet; nobody else holds it.
    unsafe { p.memory.admit_pair_to_unit(unit, pair, true) };
    p.units.install(unit);
    bump(S_CREATED);
    add_stat(S_CREATE_NS, now() - unit.created_ns);
    enqueue(p, unit);
}

// A helper's job is set by its spawner before publication. Keeping the job
// out of the shared constructor also keeps its vtable out of the root path.
fn new_unit(id: u32, execution: &crate::run_stop::Execution) -> Box<Unit> {
    let t0 = now();
    let config = execution.launch.as_ref().unwrap().base;
    // SAFETY: the launch joins every worker, and so drops every unit's last
    // use, before its execution record goes out of scope.
    let execution: &'static crate::run_stop::Execution = unsafe { &*std::ptr::from_ref(execution) };
    Box::new(Unit {
        id,
        #[cfg(feature = "testing")]
        observed_queue_ns: AtomicU64::new(0),
        #[cfg(feature = "testing")]
        observed_queue_cpu: AtomicU32::new(u32::MAX),
        sp: UnsafeCell::new(std::ptr::null_mut()),
        host: UnsafeCell::new(std::ptr::null_mut()),
        stack: UnsafeCell::new(AdmittedStacks::EMPTY),
        heap: UnsafeCell::new(std::cell::OnceCell::new()),
        state: AtomicU8::new(QUEUED),
        inside: AtomicBool::new(false),
        after: Cell::new(After::Run),
        xready: Mutex::new(Vec::new()),
        xpending: AtomicBool::new(false),
        xspare: UnsafeCell::new(Vec::new()),
        status: AtomicU8::new(0),
        stop_fd: Cell::new(2),
        slice_start: Cell::new(None),
        created_ns: t0,
        first_park: Cell::new(false),
        timer_at: AtomicU64::new(u64::MAX),
        timer_slot: AtomicU32::new(0),
        timer_next: AtomicU32::new(NONE),
        alone: std::sync::OnceLock::new(),
        alone_depth: Cell::new(0),
        config,
        execution,
        job: Cell::new(None),
        sanitizer: hirsel_stack::sanitizer::FiberState::new(),
    })
}

fn publish_unit(unit: Box<Unit>) {
    let unit: &'static Unit = Box::leak(unit);
    pool().units.install(unit);
    bump(S_CREATED);
    add_stat(S_CREATE_NS, now() - unit.created_ns);
    enqueue(pool(), unit);
}

#[cfg(test)]
#[path = "units_directory_tests.rs"]
mod tests;
