use super::*;

/// Run `jobs` as helper units beside the running pooled unit and park it until
/// every one has returned (CONC-14 flow bodies). Returns false, running
/// nothing, when no pool runs here or a host callback is on this stack: the
/// caller runs the jobs itself.
/// # Safety
/// No job may park, and each touches only memory that no other job, and
/// nothing on this unit, touches until this returns.
pub(crate) unsafe fn run_beside(jobs: &mut [&mut (dyn FnMut() + '_)]) -> bool {
    struct Left {
        count: AtomicUsize,
        owner: &'static crate::units::Unit,
    }
    struct Job(*mut (dyn FnMut() + 'static), *const Left);
    // SAFETY: by the contract, the job's memory is its own until it returns.
    unsafe impl Send for Job {}
    impl Job {
        fn run(self) {
            // SAFETY: the owner stays parked, keeping `Left` and the job
            // alive, until the count reaches zero. The owner's record is
            // never freed, so it is read before the last decrement.
            unsafe {
                (*self.0)();
                let left = &*self.1;
                let owner = left.owner;
                if left.count.fetch_sub(1, Ordering::AcqRel) == 1 {
                    crate::units::wake(owner);
                }
            }
        }
    }
    let Some(owner) = crate::units::running() else {
        return false;
    };
    if !CALLBACK_TASK.get().is_null() {
        return false;
    }
    let helpers = jobs.len().min(crate::units::pool().w.saturating_sub(1));
    let (parallel, local) = jobs.split_at_mut(helpers);
    let left = Left {
        count: AtomicUsize::new(helpers),
        owner,
    };
    for job in parallel {
        let job: *mut (dyn FnMut() + '_) = &mut **job;
        // SAFETY: only the lifetime is erased. The wait below outlives every use.
        let job = unsafe {
            std::mem::transmute::<*mut (dyn FnMut() + '_), *mut (dyn FnMut() + 'static)>(job)
        };
        let beside = Job(job, &left);
        if crate::units::spawn_job(Box::new(move || beside.run())).is_err() {
            // The directory rejected this reservation before publishing it.
            // SAFETY: this job's memory is exclusively ours by the contract.
            unsafe { (*job)() };
            left.count.fetch_sub(1, Ordering::AcqRel);
        }
    }
    for job in local {
        job();
    }
    if left.count.load(Ordering::Acquire) == 0 {
        return true;
    }
    let task = CURRENT.get();
    leave_task_tls();
    crate::limit::preserve_binding(|| {
        while left.count.load(Ordering::Acquire) != 0 {
            crate::units::park(None);
        }
    });
    if !task.is_null() {
        // SAFETY: the running task pins itself across the park.
        unsafe { enter_task_tls(task) };
    }
    true
}
