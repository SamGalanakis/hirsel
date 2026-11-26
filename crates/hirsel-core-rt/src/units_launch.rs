use super::{
    plan::{HostEnvelope, ReactorChoice},
    *,
};

/// Run `run` as `k` root units (a program has one; the reference bench two) on
/// `w` workers; the worst status wins.
pub(crate) fn launch(
    k: usize,
    w: usize,
    input: &crate::entry::OsConfig,
    run: &(dyn Fn(&crate::entry::OsConfig) -> u8 + Sync),
) -> u8 {
    match try_launch(k, w, input, run) {
        Ok(status) => status,
        Err(error) => {
            crate::os::write_all(
                input.stderr_fd,
                format!("not started: {error}\n").as_bytes(),
            );
            crate::process::NOT_STARTED
        }
    }
}

fn try_launch(
    k: usize,
    w: usize,
    input: &crate::entry::OsConfig,
    run: &(dyn Fn(&crate::entry::OsConfig) -> u8 + Sync),
) -> Result<u8, String> {
    let spin_ns = env::idle_spin_ns();
    let host = HostEnvelope::observe().map_err(|e| format!("untied host envelope: {e}"))?;
    let choice = ReactorChoice::from_env();
    let mut plan = UntiedPlan::derive(k, w, input.root_limit, host, choice)?;
    // One poller per worker slot for the descriptors its units register.
    let pollers = hirsel_core_net::slot_pollers(w, plan.reactor, here)
        .map_err(|e| format!("untied pollers for {w} workers: {e}"))?;
    let uring = pollers[0].reactor().completes_files();
    if !uring && !plan.reactor.force_epoll {
        plan = UntiedPlan::derive(k, w, input.root_limit, host, ReactorChoice::Readiness)?;
    }
    launch_planned(plan, pollers, input, run, spin_ns)
}

pub(super) fn launch_planned(
    plan: UntiedPlan,
    pollers: Vec<Arc<hirsel_core_net::Poller>>,
    input: &crate::entry::OsConfig,
    run: &(dyn Fn(&crate::entry::OsConfig) -> u8 + Sync),
    spin_ns: u64,
) -> Result<u8, String> {
    let (k, w) = (plan.initial_units, plan.workers);
    let _ = POOL.set(new_pool(plan, pollers, spin_ns));
    // SAFETY: the launch joins every worker before `run` goes out of scope.
    let run: &'static (dyn Fn(&crate::entry::OsConfig) -> u8 + Sync) =
        unsafe { std::mem::transmute(run) };
    let _ = RUN.set(RunFn(run));
    let _ = BASE.set(Base(*input));
    LIVE.store(k, Ordering::SeqCst);
    enable_pool();
    let p = pool();
    let memory_thread = p
        .memory
        .start(p.plan.memory_stack_bytes)
        .map_err(|error| format!("untied memory thread: {error}"))?;
    let start = (std::sync::Mutex::new(None), std::sync::Condvar::new());
    std::thread::scope(|scope| {
        let (ready, changed) = &start;
        let result = (0..w).try_for_each(|ix| {
            std::thread::Builder::new()
                .name(format!("hirsel-w{ix}"))
                .stack_size(p.plan.worker_stack_bytes)
                .spawn_scoped(scope, move || {
                    let go = changed
                        .wait_while(ready.lock().unwrap(), |go| go.is_none())
                        .unwrap()
                        .unwrap();
                    if go {
                        worker_main(p, ix);
                    }
                })
                .map(drop)
        });
        // Start no program work until every worker exists.
        if result.is_ok() {
            for i in 0..k {
                create_unit(i as u32);
            }
        }
        *ready.lock().unwrap() = Some(result.is_ok());
        changed.notify_all();
        result
    })
    .map_err(|error| format!("untied workers ({w} requested): {error}"))?;
    drop(memory_thread);
    // Every recording block is in its file before the run reports.
    #[cfg(feature = "record")]
    crate::tape::writer::flush();
    if std::env::var_os("HIRSEL_XSTATS").is_some() {
        report::print(p);
    }
    Ok(WORST.load(Ordering::Relaxed))
}

pub(super) fn new_pool(
    plan: UntiedPlan,
    pollers: Vec<Arc<hirsel_core_net::Poller>>,
    spin_ns: u64,
) -> Pool {
    let w = plan.workers;
    let uring = pollers[0].reactor().completes_files();
    Pool {
        w,
        locals: (0..w).map(|_| Mutex::new(VecDeque::new())).collect(),
        lifo: (0..w)
            .map(|_| AtomicPtr::new(std::ptr::null_mut()))
            .collect(),
        lifo_runs: (0..w).map(|_| AtomicU32::new(0)).collect(),
        plan,
        units: UnitDirectory::new(plan.initial_units, plan.max_units),
        helpers: Helpers::default(),
        injector: Mutex::new(VecDeque::new()),
        memory: Memory::new(plan),
        queued: AtomicUsize::new(0),
        searching: AtomicUsize::new(0),
        idle: Mutex::new(Vec::with_capacity(w)),
        nsleep: AtomicUsize::new(0),
        handed: (0..w).map(|_| AtomicBool::new(false)).collect(),
        uring,
        slots: pollers.into_iter().map(Slot::new).collect(),
        net_used: AtomicBool::new(false),
        stop: AtomicBool::new(false),
        spin_ns,
    }
}
