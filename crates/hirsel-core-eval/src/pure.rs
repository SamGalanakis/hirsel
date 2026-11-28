//! A pure run (COMPTIME-DIRECTION 3.2): one call on the pure core with nothing
//! under it, on a thread of its own, the way a constant's initializer runs.
use super::*;

/// What a pure run ended with.
#[derive(Debug)]
pub struct PureRun {
    pub outcome: Outcome,
    /// The destination's bytes, when the result is an aggregate.
    pub destination: Option<Vec<u8>>,
    /// The bytes a returned `String` or `FrozenBytes` holds (LANG-20's static
    /// computed constants), read through its carrier before the run's storage ends.
    pub held_bytes: Option<Vec<u8>>,
    /// The bytes charged to the run's limit when it ended.
    pub used: u64,
    pub peak_reserved: u64,
    pub steps: u64,
    /// Executed IR instructions.
    pub instructions: u64,
    /// The failure's chain, primary first: each cause's kind, message words and
    /// site.
    pub chain: Vec<String>,
    /// The closed-list function ([`hirsel_core_ir::CLOSED`]) whose reach stopped
    /// the run (`Outcome::Stopped("Closed")`).
    pub reached: Option<&'static str>,
    /// The bodiless (foreign) function whose call stopped the run
    /// (`Outcome::Stopped("Foreign")`).
    pub foreign: Option<FuncId>,
    /// The calls from the entry to where the run stopped, or to the site of its
    /// failure's primary cause, innermost first; empty when it returned.
    pub calls: Vec<FuncId>,
    /// The host instructions the pure thread retired running it (G1).
    #[cfg(feature = "host-instructions")]
    pub host: u64,
}

/// What a pure run executes on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Engine {
    /// The flat engine (E3, COMPTIME-DIRECTION 3.2): what a pure run, and so a
    /// constant's initializer, runs on by default. A body with a vector value
    /// still runs on the tree walker.
    Flat,
    /// The tree walker: the flat engine's differential oracle, for tests.
    Tree,
}

/// How a pure run runs.
#[derive(Clone, Copy, Debug)]
pub struct PureInput {
    /// The run's byte limit.
    pub limit: u64,
    /// The run's call-depth bound.
    pub activations: usize,
    /// The test-mode loan and role checks.
    pub checks: bool,
    /// The run's executed-instruction budget; running past it panics.
    pub instructions: u64,
    pub engine: Engine,
    /// The run's map hash key; a fresh one when `None`.
    pub map_hash_key: Option<[u8; 16]>,
    /// MEM-34: the run's root step bound. With one, the run is a constant's:
    /// entering it takes a step, and a step past the root bound, a reservation
    /// from it larger than it has left, or a refused admission against the root
    /// limit stops the run (`OutOfTime`, `Capacity`), which nothing in it sees.
    pub steps: Option<u64>,
}

impl PureInput {
    /// MEM-34's constant budget: 2^24 steps, 64 MiB and 4,096 activations.
    pub fn constant() -> Self {
        Self {
            limit: 64 << 20,
            activations: 4096,
            steps: Some(1 << 24),
            ..Self::default()
        }
    }
}

impl Default for PureInput {
    fn default() -> Self {
        Self {
            limit: u64::MAX,
            activations: usize::MAX,
            checks: false,
            instructions: u64::MAX,
            engine: Engine::Flat,
            map_hash_key: None,
            steps: None,
        }
    }
}

/// A module ready for pure runs: its layouts, its layout constants resolved to
/// them (`native_layout::resolve`) and its items' sites. A compiler evaluating
/// constants prepares its module once and runs each initializer against it.
pub struct PureModule<'a> {
    module: std::borrow::Cow<'a, Module>,
    recipes: Vec<RecipeRow>,
    sites: Vec<u64>,
}

impl<'a> PureModule<'a> {
    pub fn new(m: &'a Module) -> Self {
        let recipes = compute(&m.types).expect("layouts");
        let module =
            hirsel_core_ir::native_layout::resolve(std::borrow::Cow::Borrowed(m), &recipes);
        let sites = hirsel_core_ir::source::module_sites(&module);
        Self {
            module,
            recipes,
            sites,
        }
    }
}

/// Run `f` on the pure core alone: no scheduler, threads, network or provider,
/// so a world operation or handle stops the run.
pub fn run_pure(m: &PureModule<'_>, f: FuncId, args: &[u64], input: PureInput) -> PureRun {
    let core = Core {
        checks: input.checks,
        instructions: input.instructions,
        activations: input.activations,
        flat: input.engine == Engine::Flat,
        map_hash_key: input.map_hash_key,
        sites: &m.sites,
        ..Core::default()
    };
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(PURE_STACK)
            .spawn_scoped(scope, || pure_root(m, f, args, &input, &core))
            .expect("a pure run's thread")
            .join()
            .unwrap_or_else(|payload| std::panic::resume_unwind(payload))
    })
}

fn pure_root(
    prepared: &PureModule<'_>,
    f: FuncId,
    args: &[u64],
    input: &PureInput,
    core: &Core<'_>,
) -> PureRun {
    #[cfg(feature = "host-instructions")]
    let (counter, before) = {
        let counter = hirsel_intern::instructions::InstructionCounter::open().unwrap();
        let before = counter.read().unwrap();
        (counter, before)
    };
    let m = prepared.module.as_ref();
    let recipes = std::borrow::Cow::Borrowed(prepared.recipes.as_slice());
    let host = Pure {
        budget: input.steps.is_some(),
        reached: None,
        foreign: None,
        calls: Vec::new(),
    };
    let mut mach = Machine::with_host(m, recipes, input.limit, core, host);
    mach.install_statics();
    mach.install_descriptors(None);
    let mut t = mach.root_task(0);
    if let Some(left) = input.steps {
        let id = mach.open_steps(Some(ROOT_STEPS), left, None);
        t.step_bounds.push(StepBound { id });
    }
    let destination = mach.destination(f, None);
    let arguments: Vec<u64> = destination
        .into_iter()
        .chain(args.iter().copied())
        .collect();
    // FAIL-15 stops the run before its thread's stack runs out.
    let origin = 0u8;
    TASK_STACK_ROOM.with(|room| {
        room.set((&origin as *const u8 as usize, PURE_STACK - STACK_HEADROOM));
    });
    let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // MEM-34: entering a constant's initialiser takes a step, as a call does.
        if input.steps.is_some() {
            mach.take_steps(&mut t, 1)
                .expect("the root bound has a step");
        }
        mach.call(&mut t, f, &arguments)
            .map(|()| std::mem::take(&mut mach.results))
    }));
    let outcome = match run {
        Ok(Ok(r)) => Outcome::Returned(r),
        Ok(Err(_)) => Outcome::Failed(t.chain.iter().map(|c| c.kind.into()).collect()),
        Err(payload) if payload.is::<RunStop>() => {
            // An unwound run's activations are still on its stack.
            mach.host.calls = t.at.iter().rev().map(|h| h.f).collect();
            Outcome::Stopped(payload.downcast::<RunStop>().unwrap().0)
        }
        Err(payload) => std::panic::resume_unwind(payload),
    };
    let calls = match outcome {
        Outcome::Returned(_) => Vec::new(),
        _ => std::mem::take(&mut mach.host.calls),
    };
    #[cfg(feature = "host-instructions")]
    let host = counter.read().unwrap() - before;
    let held_bytes = match (&outcome, destination) {
        (Outcome::Returned(_), Some(at)) => mach.held_bytes(f, at),
        _ => None,
    };
    PureRun {
        #[cfg(feature = "host-instructions")]
        host,
        outcome,
        held_bytes,
        destination: destination.map(|at| mach.mem[split(at).0].bytes.clone()),
        used: mach.root().used(),
        peak_reserved: mach.peak_reserved,
        steps: mach.steps,
        instructions: core.instructions - mach.instructions,
        chain: t
            .chain
            .iter()
            .map(|c| format!("{} {:?} {:?}", c.kind, c.message, c.site))
            .collect(),
        reached: mach
            .host
            .reached
            .map(|i| hirsel_core_ir::CLOSED[usize::from(i)]),
        foreign: mach.host.foreign,
        calls,
    }
}

impl<'m, H: Host<'m>> Machine<'m, H> {
    /// The bytes the carrier at `at`, `f`'s result, holds when that result is a
    /// `String` (`{data, length, capacity}`) or a `FrozenBytes` (`{data,
    /// metadata, capacity}`, its length in the capacity word when the metadata's
    /// low bit marks static storage, else the metadata halved).
    fn held_bytes(&self, f: FuncId, at: u64) -> Option<Vec<u8>> {
        let signature = &self.m.signatures[self.m.functions[f.index()].signature.index()];
        let ty = signature.dest?;
        let frozen = match &self.m.types[ty.index()].held {
            Some(FieldTy::Seq(item))
                if **item == FieldTy::Scalar(hirsel_core_layout::Scalar::U8) =>
            {
                false
            }
            Some(FieldTy::FrozenBytes) => true,
            _ => return None,
        };
        let data = self.load(at, 8);
        let word = self.load(at + 8, 8);
        let len = match frozen {
            false => word,
            true if word & 1 != 0 => self.load(at + 16, 8),
            true => word >> 1,
        };
        if len == 0 {
            return Some(Vec::new());
        }
        Some(self.get(data, len as usize).to_vec())
    }
}

/// The id of a constant's root step bound (MEM-34), which no `bound steps`
/// scope's id reaches.
pub(super) const ROOT_STEPS: u64 = u64::MAX;

/// A step bound with `id` has too few steps left: when it is a constant's root
/// bound, the run stops (MEM-34), and nothing in it sees an `OutOfTime`.
pub(super) fn root_steps_out(id: u64) {
    if id == ROOT_STEPS {
        std::panic::resume_unwind(Box::new(RunStop("OutOfTime")));
    }
}

/// The host of a pure run: one task, no scheduler, threads, network or
/// provider. A world operation or handle stops the run.
pub(super) struct Pure {
    /// The run is a constant's (MEM-34): a refused root admission stops it.
    budget: bool,
    /// The closed-list function that stopped the run.
    reached: Option<u8>,
    /// The foreign function that stopped the run.
    foreign: Option<FuncId>,
    /// The activations, innermost first, when the primary cause was recorded.
    calls: Vec<FuncId>,
}

impl<'m> Host<'m> for Pure {
    fn current(&self) -> usize {
        0
    }
    fn event(_: &mut Machine<'m, Self>, _: u8, _: u64, _: u64) {}
    fn drain_refunds(_: &mut Machine<'m, Self>) {}
    fn defer_refund(_: &mut Machine<'m, Self>, _: &Limit, _: u64) -> bool {
        false
    }
    fn hand_over(_: &mut Machine<'m, Self>, _: &Limit, _: &Limit) {
        unreachable!("a pure run has one task")
    }
    fn slice_end(_: &mut Machine<'m, Self>) {}
    fn enter_call(_: &mut Machine<'m, Self>, _: &Task, _: usize) {}
    fn rt(_: &mut Machine<'m, Self>, _: &mut Task, op: RtOp, a: &[u64]) -> X<Vec<u64>> {
        match op {
            RtOp::StaticString => Ok(vec![a[0], 0]),
            RtOp::DropStaticStrings => Ok(vec![]),
            RtOp::ShareStaticFrozen => Ok(vec![u64::from(
                a[0] == hirsel_core_layout::FROZEN_STATIC_UNCHARGED,
            )]),
            RtOp::ByteCapacity { .. } => Ok(vec![a[0]]),
            RtOp::StaticFrozen => Ok(vec![a[0], a[1]]),
            RtOp::ReleaseStaticBytes => Ok(vec![]),
            _ => std::panic::resume_unwind(Box::new(RunStop("WorldOperation"))),
        }
    }
    fn drop_handle(_: &mut Machine<'m, Self>, _: &mut Task, _: u64, _: Handle) -> bool {
        std::panic::resume_unwind(Box::new(RunStop("WorldHandle")))
    }
    fn refused(m: &mut Machine<'m, Self>, l: &Limit) {
        if m.host.budget && Arc::ptr_eq(&l.0, &m.root().0) {
            std::panic::resume_unwind(Box::new(RunStop("Capacity")));
        }
    }
    fn closed(m: &mut Machine<'m, Self>, i: u8) {
        m.host.reached = Some(i);
        std::panic::resume_unwind(Box::new(RunStop("Closed")));
    }
    fn bodiless(m: &mut Machine<'m, Self>, _: &Task, f: FuncId) {
        m.host.foreign = Some(f);
        std::panic::resume_unwind(Box::new(RunStop("Foreign")));
    }
    fn failing(m: &mut Machine<'m, Self>, t: &Task) {
        if t.chain.is_empty() {
            m.host.calls = t.at.iter().rev().map(|h| h.f).collect();
        }
    }
}
