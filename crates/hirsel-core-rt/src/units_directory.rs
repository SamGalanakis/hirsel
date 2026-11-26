//! Unit identity publication and reuse. Records outlive their registrations.
use super::*;

const CHUNK: usize = 1024;
const CHUNKS_PER_GROUP: usize = 64;
type Chunk = [AtomicPtr<Unit>; CHUNK];
type Group = [OnceLock<Box<Chunk>>; CHUNKS_PER_GROUP];

pub(super) struct UnitDirectory {
    groups: Box<[OnceLock<Box<Group>>]>,
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
    pub(super) fn new(initial: usize, capacity: u32) -> Self {
        let groups = (capacity as usize).div_ceil(CHUNK * CHUNKS_PER_GROUP);
        Self {
            groups: (0..groups).map(|_| OnceLock::new()).collect(),
            capacity,
            claims: Mutex::new(Claims {
                next: initial as u32,
                spare: Vec::new(),
            }),
        }
    }

    fn cell(&self, id: u32) -> Option<&OnceLock<Box<Chunk>>> {
        let chunk = id as usize / CHUNK;
        let group = self.groups.get(chunk / CHUNKS_PER_GROUP)?.get()?;
        Some(&group[chunk % CHUNKS_PER_GROUP])
    }

    pub(super) fn slot(&self, id: u32) -> Option<&AtomicPtr<Unit>> {
        if id >= self.capacity {
            return None;
        }
        Some(&self.cell(id)?.get()?[id as usize % CHUNK])
    }

    pub(super) fn get(&self, id: u32) -> Option<&Unit> {
        let p = self.slot(id)?.load(Ordering::Acquire);
        // SAFETY: once published, the leaked record outlives the directory.
        unsafe { p.as_ref() }
    }

    fn install(&self, unit: &'static Unit) {
        let index = unit.id as usize;
        let chunk = index / CHUNK;
        let group = self.groups[chunk / CHUNKS_PER_GROUP]
            .get_or_init(|| Box::new([const { OnceLock::new() }; CHUNKS_PER_GROUP]));
        let slots = group[chunk % CHUNKS_PER_GROUP]
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
            crate::process::stop(b"UnitIdTaken");
        }
    }

    fn claim(&self) -> Result<Claim, usize> {
        let mut claims = self.claims.lock().unwrap();
        let live = LIVE.load(Ordering::SeqCst);
        if live == 0 {
            crate::process::stop(b"UnitAfterStop");
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
            let unit = new_unit(id);
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
            std::thread::Builder::new()
                .name("hirsel-unit".into())
                .spawn(job)
                .unwrap_or_else(|_| crate::process::stop(b"HostThreads"));
        }
        Err(SpawnErr::Exhausted {
            live,
            max_units,
            physical,
        }) => {
            crate::os::write_all(
                crate::fiber::stop_fd(),
                format!("untied unit limit: live={live} max_units={max_units}\n").as_bytes(),
            );
            crate::process::stop(if physical {
                b"HostMemory"
            } else {
                b"UnitIdSpace"
            });
        }
    }
}

pub(super) fn create_unit(id: u32) {
    publish_unit(new_unit(id));
}

// A helper's job is set by its spawner before publication. Keeping the job
// out of the shared constructor also keeps its vtable out of the root path.
fn new_unit(id: u32) -> Box<Unit> {
    let t0 = now();
    let config = BASE.get().unwrap().0;
    Box::new(Unit {
        id,
        #[cfg(feature = "testing")]
        observed_queue_ns: AtomicU64::new(0),
        #[cfg(feature = "testing")]
        observed_queue_cpu: AtomicU32::new(u32::MAX),
        sp: UnsafeCell::new(std::ptr::null_mut()),
        host: UnsafeCell::new(std::ptr::null_mut()),
        stack: UnsafeCell::new(None),
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
        timer_slot: AtomicUsize::new(0),
        alone: std::sync::OnceLock::new(),
        alone_depth: Cell::new(0),
        config,
        job: Cell::new(None),
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
