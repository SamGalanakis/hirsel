//! Direct demand over one immutable engine input snapshot. A read runs its
//! producer to completion on the reader's compiler stack and returns its
//! settled outcome; there is no ready queue, worker pool, subscription table
//! or parked continuation. A read of a producer still on the demand stack is
//! the cycle error. S0 owns semantic validity.
use super::*;
use hirsel_intern::jobs::COMPILER_STACK_BYTES;
use hirsel_syntax::ContentHash;
use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

type Key = (u32, ContentHash);
type Outcome = Result<ProductRef, Reference>;

/// The engine installs a streaming observer, with separately budgeted buffers.
/// Called during demand: do not reenter the runtime or block on work.
pub trait RuntimeObserver: Send + Sync {
    fn record(&self, event: RuntimeEvent);
}
fn key(id: QueryId) -> Key {
    (id.kind, id.address.0)
}
fn node(key: Key) -> hirsel_intern::attribution::Node {
    (key.0, key.1.0)
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RuntimeStats {
    /// Producer runs; a consumed or evicted product read again runs again.
    pub claims: u64,
    pub polls: u64,
    pub edges: u64,
    pub duplicate_reads: u64,
    pub peak_bytes: u64,
    pub charged_bytes: u64,
    pub active: usize,
    pub peak_live_nodes: usize,
    pub settled: u64,
    pub body_evictions: u64,
    pub workers: usize,
}

#[derive(Clone)]
enum Factory {
    Send(Arc<dyn QueryFactory>),
    Local(Arc<dyn LocalQueryFactory>),
}
enum Running {
    Send(Box<dyn QueryTask>),
    Local(Box<dyn LocalQueryTask>),
}
impl Running {
    fn run(&mut self, cx: &mut dyn QueryContext) -> Outcome {
        match self {
            Self::Send(task) => task.run(cx),
            Self::Local(task) => task.run(cx),
        }
    }
}

struct Request {
    roots: Vec<Key>,
    results: Vec<Option<Outcome>>,
}

struct State {
    factories: BTreeMap<u32, Factory>,
    /// Settled outcomes and their settlement order, oldest first for eviction.
    settled: BTreeMap<Key, (u64, Outcome)>,
    order: BTreeMap<u64, Key>,
    next_settled: u64,
    /// Producers being demanded, outermost first.
    stack: Vec<(Key, MemoryWeight)>,
    charged: u64,
    requests: BTreeMap<u64, Request>,
    next_request: u64,
    budget: u64,
    stack_bytes: u64,
    errors: RuntimeErrors,
    stopped: bool,
    started: bool,
    stats: RuntimeStats,
    observer: Option<Arc<dyn RuntimeObserver>>,
    origin: Instant,
    profile_id: u64,
}

/// Runtime for one immutable engine input snapshot. Factories validate S0 reads
/// before returning a hit. Revisions are selected by the engine, not RequestId.
pub struct AdmittedRuntime {
    shared: Arc<Mutex<State>>,
    reservation: Option<hirsel_engine_admission::MemoryReservation>,
}

fn lock(shared: &Mutex<State>) -> MutexGuard<'_, State> {
    shared.lock().unwrap_or_else(|e| e.into_inner())
}

fn drive(shared: &Arc<Mutex<State>>, request: u64) {
    hirsel_intern::jobs::with_compiler_stack(|| {
        let mut state = lock(shared);
        let mut index = 0;
        loop {
            if state.stopped {
                return;
            }
            let Some(root) = state
                .requests
                .get(&request)
                .and_then(|request| (index < request.roots.len()).then(|| request.roots[index]))
            else {
                return;
            };
            let done = state.requests[&request].results[index].is_some();
            if !done {
                state.restore_headroom();
                let outcome = state.demand(root);
                if let Some(request) = state.requests.get_mut(&request) {
                    request.results[index] = Some(outcome);
                }
            }
            index += 1;
        }
    })
}

impl AdmittedRuntime {
    /// Queries run on the driving compiler thread, one at a time, whatever
    /// the configured jobs; request reservations are sized for this many.
    pub const WORKERS: usize = 1;
    pub fn new(
        reservation: hirsel_engine_admission::MemoryReservation,
        errors: RuntimeErrors,
    ) -> std::io::Result<Self> {
        Self::with_reservation(reservation, errors, COMPILER_STACK_BYTES)
    }
    /// Construct a runtime whose producers must fit `stack`.
    pub fn with_reservation(
        reservation: hirsel_engine_admission::MemoryReservation,
        errors: RuntimeErrors,
        stack: usize,
    ) -> std::io::Result<Self> {
        let budget = reservation.bytes();
        hirsel_intern::timings::count_peak(
            hirsel_intern::timings::Count::RuntimeBudgetBytes,
            budget,
        );
        let stack_bytes = stack as u64;
        if stack_bytes == 0 || budget < stack_bytes {
            return Err(std::io::Error::other(
                "query budget cannot reserve one compiler stack",
            ));
        }
        Ok(Self {
            shared: Arc::new(Mutex::new(State {
                factories: BTreeMap::new(),
                settled: BTreeMap::new(),
                order: BTreeMap::new(),
                next_settled: 0,
                stack: Vec::new(),
                charged: 0,
                requests: BTreeMap::new(),
                next_request: 0,
                budget,
                stack_bytes,
                errors,
                stopped: false,
                started: false,
                stats: RuntimeStats {
                    workers: Self::WORKERS,
                    ..RuntimeStats::default()
                },
                observer: None,
                origin: Instant::now(),
                profile_id: super::profile::runtime_id(),
            })),
            reservation: Some(reservation),
        })
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
        let mut state = lock(&self.shared);
        if state.started {
            return Err("query dispatch is frozen after first request");
        }
        if state.factories.contains_key(&kind) {
            return Err("query kind already registered");
        }
        state.factories.insert(kind, factory);
        Ok(())
    }
    /// Observer ownership belongs to the engine, not request clients.
    pub fn observe(&mut self, observer: Arc<dyn RuntimeObserver>) -> Result<(), &'static str> {
        let mut state = lock(&self.shared);
        if state.started {
            return Err("query observation is frozen after first request");
        }
        state.observer = Some(observer);
        Ok(())
    }
    pub fn stats(&self) -> RuntimeStats {
        lock(&self.shared).stats
    }
    pub fn shutdown(&mut self) {
        drop(self.shutdown_with_reservation());
    }
    /// Retire every settled product before returning the original process
    /// charge. Subsequent shutdown calls return None and never release it.
    pub fn shutdown_with_reservation(
        &mut self,
    ) -> Option<hirsel_engine_admission::MemoryReservation> {
        let (settled, factories, observer) = {
            let mut state = lock(&self.shared);
            state.stopped = true;
            state.requests.clear();
            state.order.clear();
            (
                std::mem::take(&mut state.settled),
                std::mem::take(&mut state.factories),
                state.observer.take(),
            )
        };
        drop((settled, factories, observer));
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
        let mut state = lock(&self.shared);
        let request = RequestId(state.next_request);
        state.next_request = state
            .next_request
            .checked_add(1)
            .expect("request token exhausted");
        state.started = true;
        if state.stopped {
            return request;
        }
        state.event(RuntimeEvent::Request {
            request,
            roots: roots.to_vec(),
        });
        state.requests.insert(
            request.0,
            Request {
                roots: roots.iter().copied().map(key).collect(),
                results: vec![None; roots.len()],
            },
        );
        request
    }
    fn wait(&mut self, request: RequestId) -> OutcomeList {
        drive(&self.shared, request.0);
        let mut state = lock(&self.shared);
        let Some(entry) = state.requests.remove(&request.0) else {
            return Err(state.errors.cancelled);
        };
        let Some(results) = entry.results.into_iter().collect::<Option<Vec<_>>>() else {
            // Stopped before every root settled.
            return Err(state.errors.cancelled);
        };
        state.event(RuntimeEvent::Completed(request));
        // A failed early root does not suppress legally required diagnostic roots.
        results.into_iter().collect()
    }
    fn cancel(&mut self, request: RequestId) {
        let mut state = lock(&self.shared);
        if state.requests.remove(&request.0).is_some() {
            state.event(RuntimeEvent::Cancelled(request));
        }
    }
    fn evict_to(&mut self, retained_bytes: u64) {
        let mut state = lock(&self.shared);
        if retained_bytes == 0 {
            state.settled.clear();
            state.order.clear();
        }
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
    fn settle(&mut self, query: Key, outcome: Outcome) -> Outcome {
        let order = self.next_settled;
        self.next_settled += 1;
        self.order.insert(order, query);
        if let Some((previous, _)) = self.settled.insert(query, (order, outcome.clone())) {
            self.order.remove(&previous);
        }
        self.stats.settled += 1;
        self.stats.peak_live_nodes = self
            .stats
            .peak_live_nodes
            .max(self.settled.len() + self.stack.len());
        self.event(RuntimeEvent::Settled {
            query: id(query),
            result: outcome.clone(),
        });
        outcome
    }
    fn forget(&mut self, query: Key) {
        if let Some((order, _)) = self.settled.remove(&query) {
            self.order.remove(&order);
        }
    }
    fn create(&mut self, query: Key) -> Result<(Running, MemoryWeight), Reference> {
        let errors = self.errors;
        let factory = self.factories.get(&query.0).cloned();
        let created = catch_unwind(AssertUnwindSafe(|| match factory {
            Some(Factory::Send(factory)) => {
                let task = factory.create(id(query))?;
                if task.id() != id(query) {
                    return Err(errors.protocol);
                }
                let weight = task.weight();
                Ok((Running::Send(task), weight))
            }
            Some(Factory::Local(factory)) => {
                let weight = factory.weight(id(query))?;
                let task = factory.create(id(query))?;
                if task.id() != id(query) || task.weight() != weight {
                    return Err(errors.protocol);
                }
                Ok((Running::Local(task), weight))
            }
            None => Err(errors.protocol),
        }))
        .unwrap_or(Err(errors.panicked))?;
        // An oversized singleton is refused, never admitted beyond the grant.
        let fits = created.1.stack <= self.stack_bytes
            && created
                .1
                .scratch
                .checked_add(created.1.retained)
                .is_some_and(|bytes| bytes <= self.budget - self.stack_bytes);
        if fits {
            Ok(created)
        } else {
            Err(errors.resource)
        }
    }
    /// Run `query` to completion on this stack unless it already settled.
    fn demand(&mut self, query: Key) -> Outcome {
        if let Some((_, outcome)) = self.settled.get(&query) {
            self.stats.duplicate_reads += 1;
            return outcome.clone();
        }
        if let Some(Factory::Send(factory)) = self.factories.get(&query.0)
            && let Some(outcome) = factory.completed(id(query))
        {
            self.stats.duplicate_reads += 1;
            return outcome;
        }
        if self.stack.iter().any(|(active, _)| *active == query) {
            return Err(self.errors.cycle);
        }
        if self.stopped {
            return Err(self.errors.cancelled);
        }
        self.stats.claims += 1;
        self.event(RuntimeEvent::Claim(id(query)));
        let (mut task, weight) = match self.create(query) {
            Ok(created) => created,
            Err(error) => return self.settle(query, Err(error)),
        };
        let charge = weight.scratch.saturating_add(weight.retained);
        self.stack.push((query, weight));
        self.charged = self.charged.saturating_add(charge);
        self.stats.active = self.stack.len();
        self.stats.charged_bytes = self.charged;
        self.stats.peak_bytes = self.stats.peak_bytes.max(self.charged);
        hirsel_intern::timings::count_peak(
            hirsel_intern::timings::Count::RuntimeChargedBytes,
            self.charged,
        );
        let outcome = {
            self.stats.polls += 1;
            let elapsed_ns = self.elapsed();
            let charged = self.charged;
            self.event(RuntimeEvent::Admit {
                query: id(query),
                worker: 0,
                elapsed_ns,
                charged,
            });
            let start = thread_cpu_ns();
            let mut cx = Context {
                state: self,
                query,
                scratch: weight.scratch,
            };
            let ran = catch_unwind(AssertUnwindSafe(|| {
                let _running = hirsel_intern::attribution::run_as(node(query));
                task.run(&mut cx)
            }));
            let cpu_ns = thread_cpu_ns().saturating_sub(start);
            let elapsed_ns = self.elapsed();
            let charged = self.charged;
            self.event(RuntimeEvent::Release {
                query: id(query),
                worker: 0,
                elapsed_ns,
                cpu_ns,
                charged,
            });
            match ran {
                Ok(Ok(product)) if product.address == Address(query.1) => Ok(product),
                Ok(Ok(_)) => Err(self.errors.protocol),
                Ok(Err(error)) => Err(error),
                Err(_) => Err(self.errors.panicked),
            }
        };
        let dropped = catch_unwind(AssertUnwindSafe(|| drop(task)));
        self.stack.pop();
        self.charged = self.charged.saturating_sub(charge);
        self.stats.active = self.stack.len();
        self.stats.charged_bytes = self.charged;
        let outcome = match dropped {
            Ok(()) => outcome,
            Err(_) => Err(self.errors.panicked),
        };
        self.settle(query, outcome)
    }
    /// Only between top-level roots: no task is running or holds guards.
    fn restore_headroom(&self) {
        debug_assert!(self.stack.is_empty());
        for factory in self.factories.values() {
            if let Factory::Send(factory) = factory {
                factory.restore_headroom();
            }
        }
    }
    fn evict_body(&mut self, scratch: u64) -> bool {
        let mut evicted = None;
        for (&order, &query) in &self.order {
            let Some((_, Ok(product))) = self.settled.get(&query) else {
                continue;
            };
            let body = product.cache_only();
            if !body && !product.evictable() {
                continue;
            }
            let released = if body {
                product.prepare_eviction(scratch)
            } else {
                matches!(
                    self.factories.get(&query.0),
                    Some(Factory::Send(factory)) if factory.evict(id(query), product, scratch)
                )
            };
            if released {
                evicted = Some((order, query));
                break;
            }
        }
        match evicted {
            Some((order, query)) => {
                self.order.remove(&order);
                self.settled.remove(&query);
                self.stats.body_evictions += 1;
                true
            }
            None => self.factories.values().any(|factory| match factory {
                Factory::Send(factory) => factory.evict_cache(scratch),
                Factory::Local(_) => false,
            }),
        }
    }
}

struct Context<'a> {
    state: &'a mut State,
    query: Key,
    scratch: u64,
}

impl QueryContext for Context<'_> {
    fn scratch_bytes(&self) -> u64 {
        self.scratch
    }
    fn consume(&mut self, dependency: QueryId) {
        let dependency = key(dependency);
        let retain = matches!(
            self.state.settled.get(&dependency),
            Some((_, Ok(product))) if product.retain_after_consumption()
        );
        if !retain {
            self.state.forget(dependency);
        }
    }
    fn evict_body(&mut self) -> bool {
        self.state.evict_body(self.scratch)
    }
    fn cycle_members(&self, dependency: QueryId, limit: usize) -> Option<Vec<QueryId>> {
        // The path from `dependency` through the demand stack to this reader.
        let bytes_per_member = 512;
        let limit = limit.min(usize::try_from(self.scratch / bytes_per_member).ok()?);
        let start = self
            .state
            .stack
            .iter()
            .position(|(active, _)| *active == key(dependency))?;
        let path = &self.state.stack[start..];
        if path.is_empty() || path.len() > limit || path.last()?.0 != self.query {
            return None;
        }
        Some(path.iter().map(|(query, _)| id(*query)).collect())
    }
    fn read(&mut self, dependency: QueryId) -> Outcome {
        hirsel_intern::attribution::read(node(key(dependency)));
        self.state.stats.edges += 1;
        self.state.event(RuntimeEvent::Read {
            query: id(self.query),
            dependency,
        });
        self.state.demand(key(dependency))
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

#[cfg(test)]
mod tests;
