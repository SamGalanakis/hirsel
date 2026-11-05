//! One claim and completion latch per query, adapted from rustc 1.90
//! query/job.rs:183-269. Waiters here are continuations, never sleeping workers.
//! Ready ordering follows Go ebe2feef compile.go:139-184 (longest first).
//! Worker-owned task state follows tsgo 89d5d5b checkerpool.go:305,469.
//! Pending-edge counts follow Go 33e2e22 work/exec.go:126-135,189-210.
//! Here subscriptions are installed on reads and survive cancelled claims.
//! Persistent idle stacks remain charged; unlike the former native semaphore,
//! an oversized singleton cannot exceed the budget. S0 owns semantic validity.
use super::*;
use hirsel_intern::jobs::{COMPILER_STACK_BYTES, Jobs};
use hirsel_syntax::ContentHash;
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Instant;

type Key = (u32, ContentHash);
type Outcome = Result<ProductRef, Reference>;
const REFUSED_REQUEST: u64 = 1 << 63;
// Conservative space for a node plus its tree/queue indexes, request roots,
// costs, four edges/subscriptions and temporary traversal work. These bounds
// supplement producer payload weights, whose RSS must still be measured.
const ENTRY_BYTES: u64 = 4096;

/// The engine installs a streaming observer, with separately budgeted buffers.
/// Called under the scheduler lock: do not reenter the runtime or block on work.
/// Production tracing never retains an unbounded in-runtime event history.
pub trait RuntimeObserver: Send + Sync {
    fn record(&self, event: RuntimeEvent);
}
fn key(id: QueryId) -> Key {
    (id.kind, id.address.0)
}
fn id(key: Key) -> QueryId {
    QueryId {
        kind: key.0,
        address: Address(key.1),
    }
}

/// Engine-owned immutable diagnostic records, never synthesized as successes.
#[derive(Clone, Copy)]
pub struct RuntimeErrors {
    pub resource: Reference,
    pub cycle: Reference,
    pub cancelled: Reference,
    pub protocol: Reference,
    pub panicked: Reference,
}

/// Disjoint poll costs and actual edges; wall intervals are not dependency guesses.
#[derive(Clone, Debug)]
pub enum RuntimeEvent {
    Claim(QueryId),
    Read {
        query: QueryId,
        dependency: QueryId,
    },
    Admit {
        query: QueryId,
        worker: usize,
        elapsed_ns: u64,
        charged: u64,
    },
    Release {
        query: QueryId,
        worker: usize,
        elapsed_ns: u64,
        cpu_ns: u64,
        charged: u64,
    },
    Waiting {
        query: QueryId,
        elapsed_ns: u64,
        reason: WaitReason,
    },
    Wake {
        query: QueryId,
        elapsed_ns: u64,
        reason: WakeReason,
    },
    Settled {
        query: QueryId,
        result: Outcome,
    },
    Request {
        request: RequestId,
        roots: Vec<QueryId>,
    },
    Completed(RequestId),
    Cancelled(RequestId),
}

#[derive(Clone, Copy, Debug, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitReason {
    Dependency,
    Admission,
}

#[derive(Clone, Copy, Debug, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WakeReason {
    DependencySettled,
    AdmissionProgress,
    AlreadyReady,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RuntimeStats {
    pub claims: u64,
    pub polls: u64,
    pub waits: u64,
    pub wakes: u64,
    pub edges: u64,
    pub duplicate_reads: u64,
    pub wait_tokens: u64,
    pub cycle_repair_visits: u64,
    pub peak_bytes: u64,
    pub charged_bytes: u64,
    pub active: usize,
    pub peak_live_nodes: usize,
    pub settled: u64,
    pub body_evictions: u64,
    pub bookkeeping_bytes: u64,
    pub workers: usize,
}

#[derive(Default)]
enum NodeState {
    #[default]
    Dormant,
    Ready(Box<ParkedExecution>),
    Waiting(Box<ParkedExecution>),
    Running(Box<InFlightExecution>),
    Draining(Box<InFlightExecution>),
    Cancelling(Box<LocalCleanup>),
    Settled {
        outcome: Box<Outcome>,
        retained: u64,
    },
}
#[derive(Clone)]
enum Factory {
    Send(Arc<dyn QueryFactory>),
    Local(Arc<dyn LocalQueryFactory>),
}
enum ParkedTask {
    Send(Box<dyn QueryTask>),
    LocalUnbound(Arc<dyn LocalQueryFactory>),
    LocalBound {
        factory: Arc<dyn LocalQueryFactory>,
        home: usize,
    },
}
struct Charge {
    weight: MemoryWeight,
    continuation: u64,
}
impl Charge {
    // Called only after the unique task destructor has returned or unwound.
    fn retire_continuation(&mut self) -> u64 {
        let bytes = std::mem::take(&mut self.continuation);
        self.weight.retained -= bytes;
        bytes
    }
}
struct ParkedExecution {
    task: ParkedTask,
    charge: Charge,
}
enum Dispatch {
    Send,
    Local {
        factory: Arc<dyn LocalQueryFactory>,
        home: usize,
    },
}
struct InFlightExecution {
    dispatch: Dispatch,
    charge: Charge,
}
#[derive(Clone, Copy)]
enum CleanupDisposition {
    Cancel,
    Fail(Reference),
}
struct LocalCleanup {
    home: usize,
    charge: Charge,
    disposition: CleanupDisposition,
}
enum Work {
    Send(Box<dyn QueryTask>),
    Local(Arc<dyn LocalQueryFactory>),
}
impl ParkedExecution {
    fn dispatch(self, worker: usize) -> (Work, InFlightExecution) {
        let (work, dispatch) = match self.task {
            ParkedTask::Send(task) => (Work::Send(task), Dispatch::Send),
            ParkedTask::LocalUnbound(factory) | ParkedTask::LocalBound { factory, .. } => (
                Work::Local(factory.clone()),
                Dispatch::Local {
                    factory,
                    home: worker,
                },
            ),
        };
        (
            work,
            InFlightExecution {
                dispatch,
                charge: self.charge,
            },
        )
    }
    fn schedule_local_cleanup(self, disposition: CleanupDisposition) -> Result<LocalCleanup, Self> {
        match self.task {
            ParkedTask::LocalBound { home, .. } => Ok(LocalCleanup {
                home,
                charge: self.charge,
                disposition,
            }),
            _ => Err(self),
        }
    }
}
impl InFlightExecution {
    fn wait(self, returned_task: ParkedTask, ready: bool) -> NodeState {
        let task = match self.dispatch {
            Dispatch::Send => returned_task,
            Dispatch::Local { factory, home } => ParkedTask::LocalBound { factory, home },
        };
        let execution = Box::new(ParkedExecution {
            task,
            charge: self.charge,
        });
        if ready {
            NodeState::Ready(execution)
        } else {
            NodeState::Waiting(execution)
        }
    }
}
impl NodeState {
    fn wake(self) -> Self {
        match self {
            Self::Waiting(execution) => Self::Ready(execution),
            state => state,
        }
    }
    fn retained(&self) -> u64 {
        match self {
            Self::Ready(execution) | Self::Waiting(execution) => execution.charge.weight.retained,
            Self::Running(execution) | Self::Draining(execution) => {
                execution.charge.weight.retained
            }
            Self::Cancelling(cleanup) => cleanup.charge.weight.retained,
            Self::Settled { retained, .. } => *retained,
            Self::Dormant => 0,
        }
    }
    fn scratch(&self) -> u64 {
        match self {
            Self::Ready(execution) | Self::Waiting(execution) => execution.charge.weight.scratch,
            Self::Running(execution) | Self::Draining(execution) => execution.charge.weight.scratch,
            Self::Cancelling(cleanup) => cleanup.charge.weight.scratch,
            Self::Settled { .. } | Self::Dormant => 0,
        }
    }
    fn settle(self, outcome: Outcome) -> Self {
        Self::Settled {
            retained: self.retained(),
            outcome: Box::new(outcome),
        }
    }
}
struct Node {
    state: NodeState,
    dependencies: Vec<Edge>,
    // One count per reverse subscription, including reads made during a poll.
    // Completion can race that poll's return to Waiting under the same lock.
    unsatisfied: usize,
    subscribers: Vec<Subscriber>,
    generation: u32,
    position: u32,
    visited: u32,
    live: bool,
    in_flight: bool,
    settled_at: u64,
}
struct Spilled {
    result: Outcome,
    retained: u64,
}
struct State {
    factories: BTreeMap<u32, Factory>,
    nodes: Nodes,
    spill: BTreeMap<Key, Spilled>,
    ready: BTreeSet<(Reverse<u64>, Key)>,
    cleanup: Vec<BTreeSet<Key>>,
    costs: BTreeMap<Key, u64>,
    requests: BTreeMap<u64, Vec<Key>>,
    next_request: u64,
    retained: u64,
    scratch: u64,
    stacks: u64,
    budget: u64,
    metadata: u64,
    bookkeeping_grant: u64,
    recorded: u64,
    edges: usize,
    progress_revision: u64,
    // Nodes parked inside producer admission, keyed to the progress revision
    // they observed. A bump wakes them once; the entry dies with its node.
    admission_waiters: BTreeMap<Key, u64>,
    max_entries: usize,
    errors: RuntimeErrors,
    stopped: bool,
    started: bool,
    stats: RuntimeStats,
    observer: Option<Arc<dyn RuntimeObserver>>,
    origin: Instant,
    profile_id: u64,
}
struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    caller: Mutex<()>,
}

/// Runtime for one immutable engine input snapshot. Factories validate S0 reads
/// before returning a hit. Revisions are selected by the engine, not RequestId.
/// Poll is nonblocking; wait is an optional host completion wait, never a query
/// dependency wait. Every query poll must return at a bounded cancellation point.
/// Producers own ordered registration/fuel/diagnostic commit tasks; speculative
/// task completion does not authorize publication outside that ledger.
pub struct AdmittedRuntime {
    shared: Arc<Shared>,
    workers: Vec<JoinHandle<()>>,
    reservation: Option<hirsel_engine_admission::MemoryReservation>,
    caller_stack: Option<u64>,
}

/// A host completion wait independent of the runtime's outer ownership lock.
/// Clone while holding that lock, then release it before waiting. This handle
/// does not consume a result; cancellation and shutdown wake it too. A compiler
/// runtime without workers drives its admitted graph on a compiler stack here.
#[derive(Clone)]
pub struct RuntimeCompletion {
    shared: Arc<Shared>,
    caller_stack: Option<u64>,
}
impl RuntimeCompletion {
    pub fn wait(&self, request: RequestId) {
        if let Some(stack) = self.caller_stack {
            let _caller = self.shared.caller.lock().unwrap_or_else(|e| e.into_inner());
            if !self.shared.lock().request_complete(request.0) {
                hirsel_intern::jobs::with_compiler_stack(|| {
                    run_worker(self.shared.clone(), 0, stack, true)
                });
            }
        }
        let mut state = self.shared.lock();
        while !state.request_complete(request.0) && !state.stopped {
            state = self
                .shared
                .changed
                .wait(state)
                .unwrap_or_else(|e| e.into_inner());
        }
    }
}

// Same policy as Jobs::worker_count_with_cap, also used with smaller
// test stacks. Both public capacity planning and construction share it.
fn runtime_layout(jobs: Jobs, budget: u64, stack: usize) -> Option<(usize, u64)> {
    let stack = u64::try_from(stack).ok()?;
    if stack == 0 || budget < stack {
        return None;
    }
    let count = jobs
        .get()
        .min((budget / stack.checked_mul(2)?).max(1) as usize);
    Some((count, budget - count as u64 * stack))
}

impl AdmittedRuntime {
    pub fn new(
        jobs: Jobs,
        reservation: hirsel_engine_admission::MemoryReservation,
        errors: RuntimeErrors,
    ) -> std::io::Result<Self> {
        Self::with_reservation(jobs, reservation, errors, COMPILER_STACK_BYTES)
    }
    /// Reserve execution slots for the live frontier within the supplied
    /// runtime grant. Recorded reads, request-local spill, worker stacks and
    /// task memory share its remainder without another process reservation.
    /// A one-worker compiler runs on its caller. If no additional worker can
    /// start, completion waits drive the same admitted graph on the caller.
    pub fn with_query_capacity(
        jobs: Jobs,
        reservation: hirsel_engine_admission::MemoryReservation,
        errors: RuntimeErrors,
        entries: usize,
    ) -> std::io::Result<Self> {
        Self::with_metadata(
            jobs,
            reservation,
            errors,
            COMPILER_STACK_BYTES,
            Some(entries),
            true,
        )
    }
    /// Conservative allocation bound shared by admission planning and the
    /// constructor. Includes node indexes, four edges, roots and traversal.
    pub fn query_capacity_bytes(entries: usize) -> Option<u64> {
        u64::try_from(entries).ok()?.checked_mul(ENTRY_BYTES)
    }
    /// Bytes left after the worker stacks this reservation actually admits.
    /// Planners subtract query bookkeeping from this same layout before
    /// assigning producer scratch and continuation grants.
    pub fn working_bytes(jobs: Jobs, runtime_bytes: u64) -> Option<u64> {
        runtime_layout(jobs, runtime_bytes, COMPILER_STACK_BYTES).map(|(_, bytes)| bytes)
    }
    /// Original constructor capacity, including its effective worker-stack
    /// selection. Hosts can preserve subscriber/factory headroom when a small
    /// query inventory would otherwise reserve fewer rows than `new`.
    pub fn default_query_capacity(jobs: Jobs, runtime_bytes: u64) -> Option<usize> {
        let working = Self::working_bytes(jobs, runtime_bytes)?;
        Some(((working / 16) / ENTRY_BYTES).min(usize::MAX as u64) as usize)
    }
    #[cfg(test)]
    fn with_stack(
        jobs: Jobs,
        budget: u64,
        errors: RuntimeErrors,
        stack: usize,
    ) -> std::io::Result<Self> {
        Self::with_reservation(
            jobs,
            hirsel_engine_admission::ProcessBudget::new(budget).reserve(budget)?,
            errors,
            stack,
        )
    }
    /// Construct a runtime with an explicit physical worker-stack reservation.
    /// Callers must provide the stack required by their registered phases;
    /// ordinary compiler callers use `new` and its compiler-stack default.
    pub fn with_reservation(
        jobs: Jobs,
        reservation: hirsel_engine_admission::MemoryReservation,
        errors: RuntimeErrors,
        stack: usize,
    ) -> std::io::Result<Self> {
        Self::with_metadata(jobs, reservation, errors, stack, None, false)
    }
    fn with_metadata(
        jobs: Jobs,
        reservation: hirsel_engine_admission::MemoryReservation,
        errors: RuntimeErrors,
        stack: usize,
        entries: Option<usize>,
        compiler: bool,
    ) -> std::io::Result<Self> {
        let budget = reservation.bytes();
        let (count, working) = runtime_layout(jobs, budget, stack).ok_or_else(|| {
            std::io::Error::other("query budget cannot reserve one compiler stack")
        })?;
        let metadata = match entries {
            None => working / 16,
            Some(entries) => Self::query_capacity_bytes(entries)
                .filter(|bytes| entries != 0 && *bytes <= working)
                .ok_or_else(|| std::io::Error::other("query inventory exceeds runtime grant"))?,
        };
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                factories: BTreeMap::new(),
                nodes: Nodes::default(),
                spill: BTreeMap::new(),
                ready: BTreeSet::new(),
                cleanup: (0..count).map(|_| BTreeSet::new()).collect(),
                costs: BTreeMap::new(),
                requests: BTreeMap::new(),
                next_request: 0,
                retained: 0,
                scratch: 0,
                stacks: count as u64 * stack as u64,
                budget,
                metadata,
                bookkeeping_grant: metadata,
                recorded: 0,
                edges: 0,
                progress_revision: 1,
                admission_waiters: BTreeMap::new(),
                max_entries: (metadata / ENTRY_BYTES).min(usize::MAX as u64) as usize,
                errors,
                stopped: false,
                started: false,
                stats: RuntimeStats::default(),
                observer: None,
                origin: Instant::now(),
                profile_id: super::profile::runtime_id(),
            }),
            changed: Condvar::new(),
            caller: Mutex::new(()),
        });
        let mut runtime = Self {
            shared,
            workers: Vec::new(),
            reservation: Some(reservation),
            caller_stack: (compiler && jobs == Jobs::ONE).then_some(stack as u64),
        };
        for worker in 0..count {
            if runtime.caller_stack.is_some() {
                break;
            }
            let shared = runtime.shared.clone();
            match std::thread::Builder::new()
                .name("hirsel-query".into())
                .stack_size(stack)
                .spawn(move || run_worker(shared, worker, stack as u64, false))
            {
                Ok(handle) => runtime.workers.push(handle),
                Err(error) => {
                    if compiler {
                        hirsel_intern::jobs::warn_spawn_failure(
                            &error,
                            runtime.workers.len().max(1),
                        );
                        if runtime.workers.is_empty() {
                            runtime.caller_stack = Some(stack as u64);
                        }
                    }
                    let mut state = runtime.shared.lock();
                    state.stacks = runtime
                        .workers
                        .len()
                        .max(usize::from(runtime.caller_stack.is_some()))
                        as u64
                        * stack as u64;
                    if runtime.workers.is_empty() && runtime.caller_stack.is_none() {
                        return Err(error);
                    }
                    break;
                }
            }
        }
        {
            let mut state = runtime.shared.lock();
            state.stats.workers = runtime
                .workers
                .len()
                .max(usize::from(runtime.caller_stack.is_some()));
            state.stacks = state.stats.workers as u64 * stack as u64;
            state.account();
        }
        Ok(runtime)
    }
    /// Register each producer kind exactly once before any request starts.
    pub fn register(
        &mut self,
        kind: u32,
        factory: Arc<dyn QueryFactory>,
    ) -> Result<(), &'static str> {
        self.register_factory(kind, Factory::Send(factory))
    }
    pub fn register_local(
        &mut self,
        kind: u32,
        factory: Arc<dyn LocalQueryFactory>,
    ) -> Result<(), &'static str> {
        self.register_factory(kind, Factory::Local(factory))
    }
    fn register_factory(&mut self, kind: u32, factory: Factory) -> Result<(), &'static str> {
        let mut state = self.shared.lock();
        if state.started {
            return Err("query dispatch is frozen after first request");
        }
        if state.factories.len() >= state.max_entries.max(1) {
            return Err("query factory allowance exhausted");
        }
        if state.factories.contains_key(&kind) {
            return Err("query kind already registered");
        }
        state.factories.insert(kind, factory);
        Ok(())
    }
    /// Measured disjoint CPU cost from a prior comparable query, used only for
    /// ready priority. Neither identity nor semantic accounting depends on it.
    pub fn set_cost(&mut self, query: QueryId, cpu_ns: u64) -> Result<(), &'static str> {
        let mut state = self.shared.lock();
        let query = key(query);
        if !state.costs.contains_key(&query) && state.costs.len() >= state.max_entries {
            return Err("query cost allowance exhausted");
        }
        state.unqueue(query);
        state.costs.insert(query, cpu_ns.max(1));
        if state
            .nodes
            .get(&query)
            .is_some_and(|n| matches!(n.state, NodeState::Ready(_)))
        {
            state.queue(query);
        }
        Ok(())
    }
    /// Observer ownership belongs to the engine, not request clients.
    pub fn observe(&mut self, observer: Arc<dyn RuntimeObserver>) -> Result<(), &'static str> {
        let mut state = self.shared.lock();
        if state.started {
            return Err("query observation is frozen after first request");
        }
        state.observer = Some(observer);
        Ok(())
    }
    pub fn stats(&self) -> RuntimeStats {
        self.shared.lock().stats
    }
    /// Inspect allocated metadata capacities, sparse indexes and traversal
    /// scratch. Task/product payloads retain their independent MemoryWeights.
    /// This explicit inspection never runs in progress/stat counter reads.
    pub fn metadata_capacity_bytes(&self) -> u64 {
        let state = self.shared.lock();
        (size_of::<Shared>()
            + 2 * size_of::<usize>()
            + self.workers.capacity() * size_of::<JoinHandle<()>>()) as u64
            + state.nodes.allocated_bytes()
            + tree_bytes::<Key, Spilled>(state.spill.len()) as u64
            + (tree_bytes::<(Reverse<u64>, Key), ()>(state.ready.len())
                + tree_bytes::<Key, u64>(state.costs.len())
                + tree_bytes::<u32, Factory>(state.factories.len())
                + tree_bytes::<u64, Vec<Key>>(state.requests.len())
                + state.cleanup.capacity() * size_of::<BTreeSet<Key>>()
                + state
                    .requests
                    .values()
                    .map(|r| r.capacity() * size_of::<Key>())
                    .sum::<usize>()
                + state
                    .cleanup
                    .iter()
                    .map(|q| tree_bytes::<Key, ()>(q.len()))
                    .sum::<usize>()) as u64
    }
    pub fn completion(&self) -> RuntimeCompletion {
        RuntimeCompletion {
            shared: self.shared.clone(),
            caller_stack: self.caller_stack,
        }
    }
    pub fn wait(&mut self, request: RequestId) -> OutcomeList {
        let completion = self.completion();
        loop {
            if let Poll::Ready(result) = self.poll(request) {
                return result;
            }
            completion.wait(request);
        }
    }
    pub fn shutdown(&mut self) {
        drop(self.shutdown_with_reservation());
    }
    /// Join every worker and retire scheduler ownership before returning the
    /// original process charge. A staged caller may join it into its parent
    /// reservation and lend it to another round without a second admission.
    /// Subsequent shutdown calls return None and never release that charge.
    pub fn shutdown_with_reservation(
        &mut self,
    ) -> Option<hirsel_engine_admission::MemoryReservation> {
        {
            let mut state = self.shared.lock();
            state.stopped = true;
            state.requests.clear();
            state.prune();
            self.shared.changed.notify_all();
        }
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
        let _caller = self.shared.caller.lock().unwrap_or_else(|e| e.into_inner());
        let mut state = self.shared.lock();
        state.nodes.clear();
        state.edges = 0;
        state.spill.clear();
        state.ready.clear();
        state.retained = 0;
        state.stacks = 0;
        state.metadata = 0;
        state.bookkeeping_grant = 0;
        state.recorded = 0;
        state.costs.clear();
        state.factories.clear();
        state.observer = None;
        state.account();
        self.reservation.take()
    }
}
type OutcomeList = Result<Vec<ProductRef>, Reference>;
impl Drop for AdmittedRuntime {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl QueryRuntime for AdmittedRuntime {
    fn request(&mut self, roots: &[QueryId]) -> RequestId {
        let mut state = self.shared.lock();
        let request = RequestId(state.next_request);
        state.next_request = state
            .next_request
            .checked_add(1)
            .filter(|n| *n < REFUSED_REQUEST)
            .expect("request token exhausted");
        state.started = true;
        if state.stopped {
            return request;
        }
        let root_count: usize = state.requests.values().map(Vec::len).sum();
        if state.requests.len() >= state.max_entries
            || roots.len() > state.max_entries.saturating_sub(root_count)
        {
            return RequestId(request.0 | REFUSED_REQUEST);
        }
        let missing = roots
            .iter()
            .map(|r| key(*r))
            .collect::<BTreeSet<_>>()
            .iter()
            .filter(|r| !state.nodes.has_slot(r))
            .count();
        if missing > state.max_entries.saturating_sub(state.nodes.len()) {
            let idle: Vec<_> = state
                .nodes
                .iter()
                .filter_map(|(&k, n)| {
                    (!n.live
                        && matches!(n.state, NodeState::Settled { .. })
                        && !roots.iter().any(|r| key(*r) == k))
                    .then_some(k)
                })
                .collect();
            for k in idle {
                let source = state.nodes.node_id(k).expect("settled node");
                if state.record_claims(source, None) {
                    state.nodes.release_claims(source);
                    state.remove(k);
                }
            }
            if missing > state.max_entries.saturating_sub(state.nodes.len()) {
                return RequestId(request.0 | REFUSED_REQUEST);
            }
        }
        let inventory = (root_count + roots.len()).max(state.requests.len() + 1);
        let metadata = state
            .bookkeeping(state.recorded, state.nodes.arena_len(), state.edges)
            .max(
                (inventory as u64)
                    .saturating_mul(ENTRY_BYTES)
                    .saturating_add(state.recorded),
            );
        if metadata.saturating_sub(state.metadata)
            > state
                .budget
                .saturating_sub(state.stacks + state.metadata + state.retained + state.scratch)
        {
            return RequestId(request.0 | REFUSED_REQUEST);
        }
        state.metadata = metadata;
        let roots: Vec<_> = roots.iter().copied().map(key).collect();
        state.event(RuntimeEvent::Request {
            request,
            roots: roots.iter().copied().map(id).collect(),
        });
        state.requests.insert(request.0, roots.clone());
        for root in roots {
            state.ensure(root).expect("root table pre-admitted");
        }
        state.mark_live();
        self.shared.changed.notify_all();
        request
    }
    fn poll(&mut self, request: RequestId) -> Poll<OutcomeList> {
        if self.caller_stack.is_some() {
            self.completion().wait(request);
        }
        let mut state = self.shared.lock();
        if request.0 & REFUSED_REQUEST != 0 {
            return Poll::Ready(Err(state.errors.resource));
        }
        let Some(roots) = state.requests.get(&request.0) else {
            return Poll::Ready(Err(state.errors.cancelled));
        };
        // A failed early root does not suppress legally required diagnostic roots.
        let results: Option<Vec<Outcome>> = roots.iter().map(|root| state.result(*root)).collect();
        let Some(results) = results else {
            return Poll::Pending;
        };
        state.requests.remove(&request.0);
        state.event(RuntimeEvent::Completed(request));
        state.prune();
        self.shared.changed.notify_all();
        Poll::Ready(results.into_iter().collect())
    }
    fn cancel(&mut self, request: RequestId) {
        let mut state = self.shared.lock();
        if state.requests.remove(&request.0).is_some() {
            state.event(RuntimeEvent::Cancelled(request));
            state.prune();
            self.shared.changed.notify_all();
        }
    }
    fn evict_to(&mut self, retained_bytes: u64) {
        let mut state = self.shared.lock();
        let idle: Vec<_> = state
            .nodes
            .iter()
            .filter_map(|(&key, node)| {
                (!node.live && matches!(node.state, NodeState::Settled { .. })).then_some(key)
            })
            .collect();
        for key in idle {
            if retained_bytes != 0 && state.retained <= retained_bytes {
                break;
            }
            let source = state.nodes.node_id(key).expect("settled node");
            if state.record_claims(source, None) {
                state.nodes.release_claims(source);
                state.remove(key);
            }
        }
        state.account();
        self.shared.changed.notify_all();
    }
}
impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}
impl State {
    fn event(&mut self, event: RuntimeEvent) {
        super::profile::record(self.profile_id, &event);
        if let Some(observer) = &self.observer {
            observer.record(event);
        }
    }
    fn elapsed(&self) -> u64 {
        self.origin.elapsed().as_nanos().min(u64::MAX as u128) as u64
    }
    fn bookkeeping(&self, recorded: u64, rows: usize, edges: usize) -> u64 {
        let inventory = rows
            .max(edges.div_ceil(4))
            .max(self.costs.len())
            .max(self.requests.len())
            .max(self.requests.values().map(Vec::len).sum())
            .max(self.factories.len());
        self.bookkeeping_grant.max(
            (inventory as u64)
                .saturating_mul(ENTRY_BYTES)
                .saturating_add(recorded),
        )
    }
    fn reserve_bookkeeping(&mut self, recorded: u64, rows: usize, edges: usize) -> bool {
        let metadata = self.bookkeeping(recorded, rows, edges);
        if metadata.saturating_sub(self.metadata)
            > self
                .budget
                .saturating_sub(self.stacks + self.metadata + self.retained + self.scratch)
        {
            return false;
        }
        self.recorded = recorded;
        self.metadata = metadata;
        true
    }
    fn account(&mut self) {
        self.metadata = self.bookkeeping(self.recorded, self.nodes.arena_len(), self.edges);
        let bytes = self.stacks + self.metadata + self.retained + self.scratch;
        self.stats.bookkeeping_bytes = self.metadata;
        self.stats.charged_bytes = bytes;
        self.stats.peak_bytes = self.stats.peak_bytes.max(bytes);
        debug_assert!(bytes <= self.budget);
    }
    fn advance_progress(&mut self) {
        self.progress_revision = self
            .progress_revision
            .checked_add(1)
            .expect("scheduler progress revision exhausted");
        if self.admission_waiters.is_empty() {
            return;
        }
        let waiters = self.admission_waiters.keys().copied().collect::<Vec<_>>();
        for query in waiters {
            let Some(node) = self.nodes.get(&query) else {
                self.admission_waiters.remove(&query);
                continue;
            };
            if node.unsatisfied == 0 && matches!(node.state, NodeState::Waiting(_)) {
                let node = self.nodes.get_mut(&query).unwrap();
                node.state = std::mem::take(&mut node.state).wake();
                self.queue(query);
                self.stats.wakes += 1;
                self.event(RuntimeEvent::Wake {
                    query: id(query),
                    elapsed_ns: self.elapsed(),
                    reason: WakeReason::AdmissionProgress,
                });
            } else if !matches!(
                node.state,
                NodeState::Ready(_)
                    | NodeState::Running(_)
                    | NodeState::Draining(_)
                    | NodeState::Waiting(_)
            ) {
                self.admission_waiters.remove(&query);
            }
        }
    }
    fn result(&self, key: Key) -> Option<Outcome> {
        match &self.nodes.get(&key)?.state {
            NodeState::Settled {
                outcome: result, ..
            } => Some((**result).clone()),
            _ => None,
        }
    }
    fn request_complete(&self, request: u64) -> bool {
        self.requests
            .get(&request)
            .is_none_or(|roots| roots.iter().all(|k| self.result(*k).is_some()))
    }
    fn queue(&mut self, key: Key) {
        self.ready
            .insert((Reverse(self.costs.get(&key).copied().unwrap_or(1)), key));
    }
    fn unqueue(&mut self, key: Key) {
        self.ready
            .remove(&(Reverse(self.costs.get(&key).copied().unwrap_or(1)), key));
    }
    fn ensure(&mut self, query: Key) -> Result<NodeId, Reference> {
        let known = self.nodes.lookup(query);
        self.ensure_known(query, known)
    }
    fn ensure_known(&mut self, query: Key, known: Option<NodeId>) -> Result<NodeId, Reference> {
        if let Some(id) = known
            && let Some(node) = self.nodes.get_id_mut(id)
        {
            node.live = true;
            return Ok(id);
        }
        if !self.nodes.can_insert(query, self.max_entries) {
            self.retire_oldest();
        }
        if !self.nodes.can_insert(query, self.max_entries) {
            return Err(self.errors.resource);
        }
        if !self.reserve_bookkeeping(self.recorded, self.nodes.next_arena_len(query), self.edges) {
            return Err(self.errors.resource);
        }
        self.stats.claims += 1;
        self.event(RuntimeEvent::Claim(id(query)));
        let spilled = self.spill.remove(&query);
        if spilled.is_some() {
            self.recorded -= 512;
            self.metadata =
                self.bookkeeping(self.recorded, self.nodes.next_arena_len(query), self.edges);
        }
        let precharged = spilled.is_some();
        let factory = self.factories.get(&query.0).cloned();
        let node_state = if let Some(spilled) = spilled {
            NodeState::Settled {
                outcome: Box::new(spilled.result),
                retained: spilled.retained,
            }
        } else {
            let created = catch_unwind(AssertUnwindSafe(|| match factory {
                Some(Factory::Send(factory)) => {
                    let task = factory.create(id(query))?;
                    if task.id() != id(query) {
                        return Err(self.errors.protocol);
                    }
                    let weight = task.weight();
                    let continuation = task.continuation_bytes();
                    if continuation > weight.retained {
                        return Err(self.errors.protocol);
                    }
                    Ok(ParkedExecution {
                        task: ParkedTask::Send(task),
                        charge: Charge {
                            weight,
                            continuation,
                        },
                    })
                }
                Some(Factory::Local(factory)) => {
                    let weight = factory.weight(id(query))?;
                    Ok(ParkedExecution {
                        task: ParkedTask::LocalUnbound(factory),
                        charge: Charge {
                            weight,
                            continuation: 0,
                        },
                    })
                }
                None => Err(self.errors.protocol),
            }))
            .unwrap_or(Err(self.errors.panicked));
            match created {
                Ok(execution)
                    if execution
                        .charge
                        .weight
                        .scratch
                        .checked_add(execution.charge.weight.retained)
                        .is_some_and(|n| {
                            n <= self.budget.saturating_sub(self.stacks + self.metadata)
                        })
                        && execution.charge.weight.retained
                            <= self.budget.saturating_sub(
                                self.stacks + self.metadata + self.retained + self.scratch,
                            ) =>
                {
                    NodeState::Ready(Box::new(execution))
                }
                Ok(_) => NodeState::Settled {
                    outcome: Box::new(Err(self.errors.resource)),
                    retained: 0,
                },
                Err(error) => NodeState::Settled {
                    outcome: Box::new(Err(error)),
                    retained: 0,
                },
            }
        };
        if !precharged {
            self.retained += node_state.retained();
        }
        let ready = matches!(node_state, NodeState::Ready(_));
        let node_id = self.nodes.insert(
            query,
            Node {
                state: node_state,
                dependencies: Vec::new(),
                unsatisfied: 0,
                subscribers: Vec::new(),
                generation: 0,
                position: 0,
                visited: 0,
                live: true,
                in_flight: false,
                settled_at: 0,
            },
        );
        if ready {
            self.queue(query);
        }
        self.stats.peak_live_nodes = self.stats.peak_live_nodes.max(self.nodes.len());
        self.account();
        Ok(node_id)
    }
    fn record_claims(&mut self, source: NodeId, target: Option<Key>) -> bool {
        let bytes = self.nodes.recorded_bytes(source, target);
        // Slot retirement recycles the original bookkeeping reservation first.
        // Only records beyond that reservation debit additional working bytes.
        self.reserve_bookkeeping(
            self.recorded.saturating_add(bytes),
            self.nodes.arena_len(),
            self.edges,
        )
    }
    fn retire_node(&mut self, query: Key, spill: bool) -> bool {
        if self.requests.values().any(|roots| roots.contains(&query))
            || !self.nodes.reclaimable(query)
        {
            return false;
        }
        let source = self.nodes.node_id(query).expect("settled node");
        let extra = if spill { 512 } else { 0 };
        let bytes = self
            .nodes
            .recorded_bytes(source, None)
            .saturating_add(extra);
        if !self.reserve_bookkeeping(
            self.recorded.saturating_add(bytes),
            self.nodes.arena_len(),
            self.edges,
        ) {
            return false;
        }
        self.nodes.release_claims(source);
        if spill
            && let NodeState::Settled { outcome, retained } =
                &mut self.nodes.get_mut(&query).unwrap().state
        {
            self.spill.insert(
                query,
                Spilled {
                    result: (**outcome).clone(),
                    retained: std::mem::take(retained),
                },
            );
        }
        self.remove(query);
        self.costs.remove(&query);
        self.stats.body_evictions += 1;
        self.account();
        true
    }
    fn evict_spill(&mut self, scratch: u64) -> bool {
        let selected = self.spill.iter().find_map(|(&query, spilled)| {
            let product = spilled.result.as_ref().ok()?;
            let body = product.prepare_eviction(scratch);
            let archived = match self.factories.get(&query.0) {
                Some(Factory::Send(factory)) if !body => factory.evict(id(query), product, scratch),
                _ => false,
            };
            (body || archived).then_some(query)
        });
        let Some(query) = selected else {
            return false;
        };
        let spilled = self.spill.remove(&query).expect("selected spill");
        self.retained -= spilled.retained;
        self.recorded -= 512;
        drop(spilled);
        self.stats.body_evictions += 1;
        self.account();
        true
    }
    fn retire_oldest(&mut self) -> bool {
        // Settled continuations have independently owned results. Under node
        // pressure they cancel their remaining execution claims while keeping
        // every recorded read, including reads of failed producers.
        let terminal = self
            .nodes
            .iter()
            .filter_map(|(&key, node)| {
                (matches!(node.state, NodeState::Settled { .. }) && !node.dependencies.is_empty())
                    .then_some(key)
            })
            .collect::<Vec<_>>();
        for query in terminal {
            let source = self.nodes.node_id(query).expect("settled node");
            if self.record_claims(source, None) {
                self.nodes.release_claims(source);
            }
        }
        self.edges = self.nodes.edge_count();
        let candidate = self
            .nodes
            .iter()
            .filter(|(key, _)| {
                self.nodes.reclaimable(**key)
                    && !self.requests.values().any(|roots| roots.contains(key))
            })
            .min_by_key(|(key, node)| (node.settled_at, **key))
            .map(|(key, _)| *key);
        candidate.is_some_and(|query| self.retire_node(query, true))
    }
    fn mark_live(&mut self) {
        self.nodes
            .mark_live(self.requests.values().flatten().copied());
    }
    fn remove(&mut self, query: Key) {
        self.unqueue(query);
        self.admission_waiters.remove(&query);
        self.recorded -= self.nodes.cancelled_record_bytes(query);
        if let Some(removed) = self.nodes.remove(&query) {
            let retained = removed.retained();
            drop(removed);
            self.retained -= retained;
        }
        self.edges = self.nodes.edge_count();
        self.costs.remove(&query);
    }
    fn restart(&mut self, query: Key) {
        // Incoming recorded reads retain the semantic slot. Pending tokens are
        // explicitly transferred only after the discarded poll has drained.
        self.remove(query);
        self.ensure(query)
            .expect("replacement uses its admitted slot");
        self.nodes.transfer_subscriptions(query);
        if let Some(result) = self.result(query) {
            self.settle(query, result);
        }
    }
    fn prune(&mut self) {
        self.mark_live();
        let mut remove = Vec::new();
        let mut progress = false;
        for key in self.nodes.iter().map(|(k, _)| *k).collect::<Vec<_>>() {
            let node = self.nodes.get_mut(&key).unwrap();
            if !node.live {
                node.state = match std::mem::take(&mut node.state) {
                    NodeState::Ready(execution) | NodeState::Waiting(execution) => {
                        match execution.schedule_local_cleanup(CleanupDisposition::Cancel) {
                            Ok(cleanup) => {
                                self.cleanup[cleanup.home].insert(key);
                                progress = true;
                                NodeState::Cancelling(Box::new(cleanup))
                            }
                            Err(execution) => {
                                remove.push(key);
                                NodeState::Ready(Box::new(execution))
                            }
                        }
                    }
                    NodeState::Running(execution) => {
                        progress = true;
                        NodeState::Draining(execution)
                    }
                    state => state,
                };
            }
        }
        for key in remove {
            self.remove(key);
            progress = true;
        }
        if progress {
            self.advance_progress();
        }
        self.account();
    }
    fn retire_continuation(&mut self, query: Key) {
        if let Some(node) = self.nodes.get_mut(&query) {
            self.retained -= match &mut node.state {
                NodeState::Running(execution) | NodeState::Draining(execution) => {
                    execution.charge.retire_continuation()
                }
                NodeState::Cancelling(cleanup) => cleanup.charge.retire_continuation(),
                _ => 0,
            };
        }
    }
    fn settle(&mut self, query: Key, result: Outcome) {
        let node = self.nodes.get_mut(&query).expect("claimed query");
        node.state = std::mem::take(&mut node.state).settle(result.clone());
        node.settled_at = self.stats.settled;
        self.nodes.settle_records(query);
        self.stats.settled += 1;
        self.admission_waiters.remove(&query);
        self.event(RuntimeEvent::Settled {
            query: id(query),
            result,
        });
        let producer = self.nodes.node_id(query).expect("claimed query");
        // Reverse records also retain settled/failed semantic edges. The token
        // generation, rather than the mere presence of an edge, grants a wake.
        for i in 0..self.nodes.subscribers(producer).len() {
            if let Some(waiter) = self.nodes.consume(producer, i) {
                self.queue(waiter);
                self.stats.wakes += 1;
                self.event(RuntimeEvent::Wake {
                    query: id(waiter),
                    elapsed_ns: self.elapsed(),
                    reason: WakeReason::DependencySettled,
                });
            }
        }
    }
}

struct Context<'a> {
    shared: &'a Shared,
    query: Key,
    generation: NodeId,
    pending: BTreeSet<u32>,
    admission_wait: bool,
}
impl QueryContext for Context<'_> {
    fn scratch_bytes(&self) -> u64 {
        self.shared
            .lock()
            .nodes
            .get(&self.query)
            .map_or(0, |node| node.state.scratch())
    }

    fn workers(&self) -> usize {
        self.shared.lock().stats.workers.max(1)
    }
    fn release_body(&mut self, dependency: QueryId) {
        self.shared
            .lock()
            .nodes
            .release_body(self.generation, key(dependency));
    }
    fn consume(&mut self, dependency: QueryId) {
        let mut state = self.shared.lock();
        let dependency = key(dependency);
        let Some(producer) = state.nodes.node_id(dependency) else {
            return;
        };
        if state.nodes.get_id(self.generation).is_none()
            || state.nodes.edge_to(self.generation, producer).is_none()
            || state.nodes.result(producer).is_none()
        {
            return;
        }
        if state.record_claims(self.generation, Some(dependency))
            && state.nodes.consume_read(self.generation, dependency)
        {
            state.edges = state.nodes.edge_count();
            let keep_lower = state
                .result(dependency)
                .is_some_and(|result| result.is_ok_and(|product| product.lower_value().is_some()));
            state.retire_node(dependency, keep_lower);
            state.advance_progress();
            state.account();
            self.shared.changed.notify_all();
        }
    }
    fn evict_body(&mut self) -> bool {
        let mut state = self.shared.lock();
        let scratch = state
            .nodes
            .get(&self.query)
            .map_or(0, |node| node.state.scratch());
        if state.evict_spill(scratch) {
            self.shared.changed.notify_all();
            return true;
        }
        let roots = state
            .requests
            .values()
            .flatten()
            .copied()
            .filter(|key| {
                state
                    .nodes
                    .get(key)
                    .is_some_and(|n| !matches!(n.state, NodeState::Settled { .. }))
            })
            .collect::<Vec<_>>();
        state.nodes.mark_in_flight(roots.into_iter());
        let scratch = state
            .nodes
            .get(&self.query)
            .map_or(0, |node| node.state.scratch());
        let mut previous = None;
        let key = loop {
            let oldest = state
                .nodes
                .iter()
                .filter_map(|(&key, node)| {
                    if node.in_flight || state.requests.values().any(|roots| roots.contains(&key)) {
                        return None;
                    }
                    let order = (node.settled_at, key);
                    if previous.is_some_and(|previous| order <= previous) {
                        return None;
                    }
                    let NodeState::Settled {
                        outcome: result, ..
                    } = &node.state
                    else {
                        return None;
                    };
                    let product = result.as_ref().as_ref().ok()?;
                    let body = product.cache_only();
                    let summary = product.evictable();
                    (body || summary).then_some((order, body))
                })
                .min();
            let Some(((settled, key), body)) = oldest else {
                return false;
            };
            previous = Some((settled, key));
            if body {
                let NodeState::Settled { outcome, .. } = &state.nodes[&key].state else {
                    continue;
                };
                if outcome
                    .as_ref()
                    .as_ref()
                    .ok()
                    .and_then(|product| product.lower_value())
                    .is_none_or(|body| body.prepare_eviction(scratch))
                {
                    break key;
                }
                continue;
            }
            let Some(Factory::Send(factory)) = state.factories.get(&key.0) else {
                continue;
            };
            let NodeState::Settled {
                outcome: result, ..
            } = &state.nodes[&key].state
            else {
                continue;
            };
            if result
                .as_ref()
                .as_ref()
                .ok()
                .is_some_and(|product| factory.evict(id(key), product, scratch))
            {
                break key;
            }
        };
        let source = state.nodes.node_id(key).expect("settled node");
        if !state.record_claims(source, None) {
            return false;
        }
        state.nodes.release_claims(source);
        state.remove(key);
        state.stats.body_evictions += 1;
        state.account();
        self.shared.changed.notify_all();
        true
    }

    fn poll_evict_body(&mut self) -> Poll<bool> {
        if self.evict_body() {
            self.shared.lock().admission_waiters.remove(&self.query);
            self.admission_wait = false;
            return Poll::Ready(true);
        }
        let mut state = self.shared.lock();
        let parked = state.admission_waiters.get(&self.query).copied();
        if parked.is_some_and(|revision| revision < state.progress_revision) {
            // Woken by admitted progress: retry the reservation once. A still
            // failing retry re-parks or refuses on the next call.
            state.admission_waiters.remove(&self.query);
            self.admission_wait = false;
            return Poll::Ready(true);
        }
        let can_progress = state.nodes.iter().any(|(&query, node)| {
            query != self.query
                && !state.admission_waiters.contains_key(&query)
                && matches!(
                    node.state,
                    NodeState::Ready(_) | NodeState::Running(_) | NodeState::Draining(_)
                )
        }) || state.cleanup.iter().any(|queue| !queue.is_empty());
        if can_progress {
            let progress_revision = state.progress_revision;
            state
                .admission_waiters
                .insert(self.query, progress_revision);
            self.admission_wait = true;
            Poll::Pending
        } else {
            state.admission_waiters.remove(&self.query);
            self.admission_wait = false;
            Poll::Ready(false)
        }
    }

    fn poll_publication(&mut self) -> Poll<Result<(), Reference>> {
        let mut state = self.shared.lock();
        if state.stopped
            || state
                .nodes
                .get_id(self.generation)
                .is_none_or(|node| !node.live || matches!(node.state, NodeState::Draining(_)))
        {
            return Poll::Ready(Err(state.errors.cancelled));
        }
        let first = state
            .requests
            .values()
            .flatten()
            .copied()
            .find(|root| state.result(*root).is_none());
        if first.is_some_and(|root| state.nodes.publication_frontier(root, self.generation)) {
            state.admission_waiters.remove(&self.query);
            self.admission_wait = false;
            Poll::Ready(Ok(()))
        } else {
            let revision = state.progress_revision;
            state.admission_waiters.insert(self.query, revision);
            self.admission_wait = true;
            Poll::Pending
        }
    }

    fn cycle_members(&self, dependency: QueryId, limit: usize) -> Option<Vec<QueryId>> {
        let state = self.shared.lock();
        let scratch = state.nodes.get_id(self.generation)?.state.scratch();
        // The slot path and its converted identities coexist during collection.
        // Vec growth is bounded by twice their lengths.
        let bytes_per_member = 512;
        let limit = limit.min(usize::try_from(scratch / bytes_per_member).ok()?);
        let path = state
            .nodes
            .recorded_path(self.query, key(dependency), limit)?;
        Some(path.into_iter().map(id).collect())
    }

    fn read(&mut self, dependency: QueryId) -> Poll<Outcome> {
        let mut state = self.shared.lock();
        if state.stopped
            || state
                .nodes
                .get_id(self.generation)
                .is_none_or(|n| !n.live || matches!(n.state, NodeState::Draining(_)))
        {
            return Poll::Ready(Err(state.errors.cancelled));
        }
        let dependency = key(dependency);
        let known = state.nodes.lookup(dependency);
        let existing = known.and_then(|target| state.nodes.edge_to(self.generation, target));
        if existing.is_none() && state.edges >= state.max_entries * 4 {
            return Poll::Ready(Err(state.errors.resource));
        }
        let producer = match state.ensure_known(dependency, known) {
            Ok(producer) => producer,
            Err(error) => return Poll::Ready(Err(error)),
        };
        if existing.is_none() {
            let recorded = state.recorded;
            let rows = state.nodes.arena_len();
            let edges = state.edges + 1;
            if !state.reserve_bookkeeping(recorded, rows, edges) {
                return Poll::Ready(Err(state.errors.resource));
            }
        }
        let edge = if let Some(edge) = existing {
            state.stats.duplicate_reads += 1;
            edge
        } else {
            let before = state.nodes.repair_visits;
            let inserted = state.nodes.insert_edge(self.generation, producer);
            state.stats.cycle_repair_visits += state.nodes.repair_visits - before;
            let Some(edge) = inserted else {
                return Poll::Ready(Err(state.errors.cycle));
            };
            state.stats.edges += 1;
            state.edges += 1;
            state.event(RuntimeEvent::Read {
                query: id(self.query),
                dependency: id(dependency),
            });
            edge
        };
        state.nodes.retain_body(self.generation, edge);
        if let Some(result) = state.nodes.result(producer) {
            return Poll::Ready(result);
        }
        if state.nodes.subscribe(self.generation, edge, producer) {
            state.stats.wait_tokens += 1;
        }
        self.pending.insert(producer.slot);
        self.shared.changed.notify_all();
        Poll::Pending
    }
}

fn thread_cpu_ns() -> u64 {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: valid writable timespec, thread CPU clock has no external state.
    let ok = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut time) };
    if ok == 0 {
        (time.tv_sec as u64)
            .saturating_mul(1_000_000_000)
            .saturating_add(time.tv_nsec as u64)
    } else {
        0
    }
}
// Tokio tokio-1.48.0 task/local.rs:225,741-805,966 keeps local task
// ownership and destruction on its creating thread. Unlike Tokio, the shared
// query claim/edge table here decides readiness, with a bounded local map and
// home cleanup queue rather than an unbounded remote spawn queue.
fn run_worker(shared: Arc<Shared>, worker: usize, stack: u64, caller: bool) {
    let mut locals: BTreeMap<Key, Box<dyn LocalQueryTask>> = BTreeMap::new();
    loop {
        // Cleanup has priority and needs no scratch permit. Charge stays live
        // until the home worker acknowledges destruction of the continuation.
        let cleanup = shared.lock().cleanup[worker].pop_first();
        if let Some(query) = cleanup {
            let dropped = catch_unwind(AssertUnwindSafe(|| {
                locals.remove(&query);
            }));
            let mut state = shared.lock();
            state.retire_continuation(query);
            let Some(Node {
                state: NodeState::Cancelling(cleanup),
                live,
                ..
            }) = state.nodes.get(&query)
            else {
                continue;
            };
            let live = *live;
            let disposition = cleanup.disposition;
            if let CleanupDisposition::Fail(error) = disposition {
                let error = if dropped.is_err() {
                    state.errors.panicked
                } else {
                    error
                };
                state.settle(query, Err(error));
            } else if live && !state.stopped {
                state.restart(query);
                if dropped.is_err() {
                    let error = state.errors.panicked;
                    state.unqueue(query);
                    state.settle(query, Err(error));
                }
            } else {
                state.remove(query);
            }
            state.account();
            state.advance_progress();
            shared.changed.notify_all();
            continue;
        }
        let Some((query, work, weight)) = ({
            let mut state = shared.lock();
            loop {
                if !state.cleanup[worker].is_empty() {
                    break None;
                }
                if state.stopped {
                    return;
                }
                let first = state.ready.iter().find_map(|(_, query)| {
                    let NodeState::Ready(execution) = &state.nodes[query].state else {
                        return None;
                    };
                    let available = match execution.task {
                        ParkedTask::LocalBound { home, .. } => home == worker,
                        _ => true,
                    };
                    available.then_some((*query, execution.charge.weight))
                });
                if let Some((query, weight)) = first {
                    if weight.stack > stack {
                        state.unqueue(query);
                        let error = state.errors.resource;
                        state.settle(query, Err(error));
                        shared.changed.notify_all();
                        continue;
                    }
                    if weight.scratch
                        <= state.budget.saturating_sub(
                            state.stacks + state.metadata + state.retained + state.scratch,
                        )
                    {
                        state.unqueue(query);
                        let node = state.nodes.get_mut(&query).unwrap();
                        let NodeState::Ready(execution) = std::mem::take(&mut node.state) else {
                            continue;
                        };
                        let (work, execution) = execution.dispatch(worker);
                        node.state = NodeState::Running(Box::new(execution));
                        state.scratch += weight.scratch;
                        state.stats.active += 1;
                        state.stats.polls += 1;
                        state.account();
                        let elapsed_ns = state.elapsed();
                        let charged = state.stats.charged_bytes;
                        state.event(RuntimeEvent::Admit {
                            query: id(query),
                            worker,
                            elapsed_ns,
                            charged,
                        });
                        break Some((query, work, weight));
                    }
                    if state.stats.active == 0 {
                        state.unqueue(query);
                        let error = state.errors.resource;
                        let node = state.nodes.get_mut(&query).unwrap();
                        let NodeState::Ready(execution) = std::mem::take(&mut node.state) else {
                            continue;
                        };
                        match execution.schedule_local_cleanup(CleanupDisposition::Fail(error)) {
                            Ok(cleanup) => {
                                state.cleanup[cleanup.home].insert(query);
                                state.nodes.get_mut(&query).unwrap().state =
                                    NodeState::Cancelling(Box::new(cleanup));
                            }
                            Err(execution) => {
                                state.nodes.get_mut(&query).unwrap().state =
                                    NodeState::Ready(Box::new(execution));
                                state.settle(query, Err(error));
                            }
                        }
                        shared.changed.notify_all();
                        continue;
                    }
                }
                if caller && state.ready.is_empty() {
                    debug_assert!(
                        locals.is_empty(),
                        "idle caller retains a local continuation"
                    );
                    return;
                }
                state = shared
                    .changed
                    .wait(state)
                    .unwrap_or_else(|e| e.into_inner());
            }
        }) else {
            continue;
        };
        let start = thread_cpu_ns();
        let mut cx = Context {
            shared: &shared,
            query,
            generation: shared.lock().nodes.node_id(query).expect("running query"),
            pending: BTreeSet::new(),
            admission_wait: false,
        };
        let polled = catch_unwind(AssertUnwindSafe(|| match work {
            Work::Send(mut task) => {
                let outcome = task.poll(&mut cx);
                (outcome, ParkedTask::Send(task))
            }
            Work::Local(factory) => {
                let outcome = (|| {
                    if let std::collections::btree_map::Entry::Vacant(entry) = locals.entry(query) {
                        let task = match factory.create(id(query)) {
                            Ok(task) => task,
                            Err(error) => return QueryPoll::Failed(error),
                        };
                        if task.id() != id(query) || task.weight() != weight {
                            return QueryPoll::Failed(shared.lock().errors.protocol);
                        }
                        let continuation = task.continuation_bytes();
                        if continuation > weight.retained {
                            return QueryPoll::Failed(shared.lock().errors.protocol);
                        }
                        let mut state = shared.lock();
                        let protocol = state.errors.protocol;
                        match &mut state
                            .nodes
                            .get_mut(&query)
                            .expect("running local task")
                            .state
                        {
                            NodeState::Running(execution) | NodeState::Draining(execution) => {
                                execution.charge.continuation = continuation
                            }
                            _ => return QueryPoll::Failed(protocol),
                        }
                        drop(state);
                        entry.insert(task);
                    }
                    locals.get_mut(&query).unwrap().poll(&mut cx)
                })();
                (
                    outcome,
                    ParkedTask::LocalBound {
                        factory,
                        home: worker,
                    },
                )
            }
        }));
        let (mut outcome, mut returned_task) = match polled {
            Ok((outcome, task)) => (Ok(outcome), Some(task)),
            Err(panic) => (Err(panic), None),
        };
        // Terminal task frames are destroyed before their charge is retired.
        // Suspended Send/local continuations keep their full admitted weight.
        let keep_local = matches!(outcome, Ok(QueryPoll::Waiting(_)));
        if !keep_local
            && let Err(panic) = catch_unwind(AssertUnwindSafe(|| {
                drop(returned_task.take());
                locals.remove(&query);
            }))
        {
            outcome = Err(panic);
        }
        let cpu_ns = thread_cpu_ns().saturating_sub(start);
        let mut state = shared.lock();
        if !keep_local {
            state.retire_continuation(query);
        }
        state.scratch -= weight.scratch;
        state.stats.active -= 1;
        state.account();
        let elapsed_ns = state.elapsed();
        let charged = state.stats.charged_bytes;
        state.event(RuntimeEvent::Release {
            query: id(query),
            worker,
            elapsed_ns,
            cpu_ns,
            charged,
        });
        if !cx.admission_wait {
            state.advance_progress();
        }
        if matches!(state.nodes[&query].state, NodeState::Draining(_)) || state.stopped {
            // Drop without holding the shared graph lock. Draining retains the
            // old generation and charge while a new request can attach to it.
            drop(state);
            let _ = catch_unwind(AssertUnwindSafe(|| {
                drop(returned_task.take());
                locals.remove(&query);
            }));
            state = shared.lock();
            state.retire_continuation(query);
            let live = state.nodes.get(&query).is_some_and(|node| node.live);
            if live && !state.stopped {
                state.restart(query);
            } else {
                state.remove(query);
            }
            state.advance_progress();
        } else {
            match outcome {
                Ok(QueryPoll::Complete(product))
                    if cx.pending.is_empty() && product.address == Address(query.1) =>
                {
                    state.settle(query, Ok(product))
                }
                Ok(QueryPoll::Failed(error)) => state.settle(query, Err(error)),
                Ok(QueryPoll::Waiting(waits))
                    if (!cx.pending.is_empty()
                        && waits.iter().all(|q| state.nodes.node_id(key(*q)).is_some())
                        && waits
                            .iter()
                            .filter_map(|q| state.nodes.node_id(key(*q)).map(|id| id.slot))
                            .collect::<BTreeSet<_>>()
                            == cx.pending)
                        || (waits.is_empty()
                            && cx.admission_wait
                            && cx.pending.is_empty()
                            && state.admission_waiters.contains_key(&query)) =>
                {
                    let progress_revision = state.progress_revision;
                    let parked = state.admission_waiters.get(&query).copied();
                    let node = state.nodes.get_mut(&query).unwrap();
                    let ready = node.unsatisfied == 0
                        && (!cx.admission_wait
                            || parked.is_some_and(|revision| progress_revision > revision));
                    if let (NodeState::Running(execution), Some(task)) =
                        (std::mem::take(&mut node.state), returned_task)
                    {
                        node.state = execution.wait(task, ready);
                    }
                    state.stats.waits += 1;
                    let elapsed_ns = state.elapsed();
                    state.event(RuntimeEvent::Waiting {
                        query: id(query),
                        elapsed_ns,
                        reason: if cx.admission_wait {
                            WaitReason::Admission
                        } else {
                            WaitReason::Dependency
                        },
                    });
                    if ready {
                        state.queue(query);
                        state.stats.wakes += 1;
                        state.event(RuntimeEvent::Wake {
                            query: id(query),
                            elapsed_ns,
                            reason: WakeReason::AlreadyReady,
                        });
                    }
                }
                Err(_) => {
                    let error = state.errors.panicked;
                    state.settle(query, Err(error));
                }
                _ => {
                    drop(state);
                    let _ = catch_unwind(AssertUnwindSafe(|| {
                        drop(returned_task.take());
                        locals.remove(&query);
                    }));
                    state = shared.lock();
                    state.retire_continuation(query);
                    let error = state.errors.protocol;
                    state.settle(query, Err(error));
                }
            }
        }
        shared.changed.notify_all();
    }
}
mod nodes;
#[cfg(test)]
mod tests;
use nodes::{Edge, NodeId, Nodes, Subscriber, tree_bytes};
