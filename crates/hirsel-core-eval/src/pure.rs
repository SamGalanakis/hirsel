//! A pure run (COMPTIME-DIRECTION 3.2): one call on the pure core with nothing
//! under it, on a thread of its own, the way a constant's initializer runs.
use super::*;

/// What a pure run ended with.
#[derive(Debug)]
pub struct PureRun {
    pub outcome: Outcome,
    /// The destination's bytes, when the result is an aggregate.
    pub destination: Option<Vec<u8>>,
    /// The bytes charged to the run's limit when it ended.
    pub used: u64,
    pub peak_reserved: u64,
    pub steps: u64,
    /// Executed IR instructions.
    pub instructions: u64,
}

/// Run `f` on the pure core alone: no scheduler, threads, network or provider,
/// so a world operation or handle stops the run. `limit` bounds the run's bytes
/// and `activations` its call depth; `checks` turns on the test-mode loan and
/// role checks.
pub fn run_pure(
    m: &Module,
    f: FuncId,
    args: &[u64],
    limit: u64,
    activations: usize,
    checks: bool,
) -> PureRun {
    let core = Core {
        checks,
        activations,
        ..Core::default()
    };
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(ROOT_STACK)
            .spawn_scoped(scope, || pure_root(m, f, args, limit, &core))
            .expect("a pure run's thread")
            .join()
            .unwrap_or_else(|payload| std::panic::resume_unwind(payload))
    })
}

fn pure_root(m: &Module, f: FuncId, args: &[u64], limit: u64, core: &Core<'_>) -> PureRun {
    let recipes = compute(&m.types).expect("layouts");
    let native = hirsel_core_ir::native_layout::resolve(m, &recipes);
    let m = native.as_ref();
    let mut mach = Machine::with_host(m, recipes, limit, core, Pure);
    mach.install_statics();
    mach.install_descriptors(None);
    let mut t = mach.root_task(0);
    let destination = mach.destination(f, None);
    let arguments: Vec<u64> = destination
        .into_iter()
        .chain(args.iter().copied())
        .collect();
    // FAIL-15 stops the run before its thread's stack runs out.
    let origin = 0u8;
    TASK_STACK_ROOM.with(|room| {
        room.set((&origin as *const u8 as usize, ROOT_STACK - STACK_HEADROOM));
    });
    let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        mach.call(&mut t, f, &arguments)
            .map(|()| std::mem::take(&mut mach.results))
    }));
    let outcome = match run {
        Ok(Ok(r)) => Outcome::Returned(r),
        Ok(Err(_)) => Outcome::Failed(t.chain.iter().map(|c| c.kind.into()).collect()),
        Err(payload) if payload.is::<RunStop>() => {
            Outcome::Stopped(payload.downcast::<RunStop>().unwrap().0)
        }
        Err(payload) => std::panic::resume_unwind(payload),
    };
    PureRun {
        outcome,
        destination: destination.map(|at| mach.mem[split(at).0].bytes.clone()),
        used: mach.root().used(),
        peak_reserved: mach.peak_reserved,
        steps: mach.steps,
        instructions: core.instructions - mach.instructions,
    }
}
