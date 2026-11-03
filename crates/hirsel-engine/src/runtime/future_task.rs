//! Drive Rust's saved, non-Send future state from the existing query latch
//! graph. As with Tokio LocalSet, polling and destruction stay at home; this
//! bridge adds no executor, worker, semantic cache or external wake queue.
use super::*;
use std::{
    cell::RefCell,
    collections::BTreeMap,
    future::{Future, poll_fn},
    pin::Pin,
    rc::Rc,
    task::{Context, Poll, Waker},
};

type Key = (u32, hirsel_syntax::ContentHash);
type Outcome = Result<ProductRef, Reference>;
fn key(query: QueryId) -> Key {
    (query.kind, query.address.0)
}
#[derive(Default)]
enum Eviction {
    #[default]
    Idle,
    Requested,
    Completed(bool),
}
#[derive(Default)]
enum Publication {
    #[default]
    Idle,
    Requested,
    Completed(Result<(), Reference>),
}
struct Reads {
    publication: Publication,
    eviction: Eviction,
    cycle: Vec<QueryId>,
    composed: Composed,
    working: MemoryWeight,
    requested: BTreeMap<Key, QueryId>,
    completed: BTreeMap<Key, Outcome>,
    consumed: Vec<QueryId>,
    limit: usize,
    resource: Reference,
}
#[derive(Default)]
enum Composed {
    #[default]
    Idle,
    Requested(Box<dyn QueryTask>),
    Completed(Outcome),
}
impl Composed {
    fn slots(&self) -> usize {
        match self {
            Self::Idle => 0,
            Self::Requested(task) => 1usize.saturating_add(
                task.weight().retained.div_ceil(512).min(usize::MAX as u64) as usize,
            ),
            Self::Completed(_) => 1,
        }
    }
}
/// Task-local dependency mailbox. Clone only shares the one bounded mailbox;
/// reads use QueryContext on the next poll and never block a compiler worker.
#[derive(Clone)]
pub struct QueryReads(Rc<RefCell<Reads>>);
impl QueryReads {
    pub(crate) fn take_cycle(&self) -> Vec<QueryId> {
        std::mem::take(&mut self.0.borrow_mut().cycle)
    }
    /// Poll one producer under this continuation's working grant. Its reads
    /// subscribe to the existing graph; the composed producer claims no node.
    /// Its frame consumes mailbox capacity until destruction.
    pub(crate) async fn compose(&self, task: Box<dyn QueryTask>) -> Outcome {
        let mut task = Some(task);
        poll_fn(|_| {
            let mut reads = self.0.borrow_mut();
            if let Some(task) = task.take() {
                let weight = task.weight();
                let composed = Composed::Requested(task);
                if !matches!(reads.composed, Composed::Idle)
                    || weight.scratch > reads.working.scratch
                    || weight.stack > reads.working.stack
                    || reads
                        .completed
                        .len()
                        .saturating_add(reads.requested.len())
                        .saturating_add(composed.slots())
                        .saturating_add(reads.cycle.len())
                        > reads.limit
                {
                    return Poll::Ready(Err(reads.resource));
                }
                reads.composed = composed;
            }
            match std::mem::take(&mut reads.composed) {
                Composed::Completed(result) => Poll::Ready(result),
                pending => {
                    reads.composed = pending;
                    Poll::Pending
                }
            }
        })
        .await
    }
    /// Retry a producer admission using the runtime's existing unpinned-body
    /// eviction policy. The request and result stay in this bounded mailbox.
    pub(crate) async fn evict_body(&self) -> bool {
        poll_fn(|_| {
            let mut reads = self.0.borrow_mut();
            match std::mem::take(&mut reads.eviction) {
                Eviction::Completed(evicted) => Poll::Ready(evicted),
                Eviction::Idle | Eviction::Requested => {
                    reads.eviction = Eviction::Requested;
                    Poll::Pending
                }
            }
        })
        .await
    }
    pub(crate) async fn publication(&self) -> Result<(), Reference> {
        poll_fn(|_| {
            let mut reads = self.0.borrow_mut();
            match std::mem::take(&mut reads.publication) {
                Publication::Completed(result) => Poll::Ready(result),
                Publication::Idle | Publication::Requested => {
                    reads.publication = Publication::Requested;
                    Poll::Pending
                }
            }
        })
        .await
    }
    pub async fn read(&self, query: QueryId) -> Outcome {
        poll_fn(|_| self.poll_read(query, false)).await
    }
    /// Consume a completed mailbox row and its scheduler claim. The caller
    /// takes the result into independently admitted state; a later read may
    /// reopen a fresh generation from its producer's retained owner or archive.
    /// Sequential dependencies retain only their concurrently live rows.
    pub async fn read_consuming(&self, query: QueryId) -> Outcome {
        poll_fn(|_| self.poll_read(query, true)).await
    }
    fn poll_read(&self, query: QueryId, consuming: bool) -> Poll<Outcome> {
        let mut reads = self.0.borrow_mut();
        let selected = if consuming {
            reads.completed.remove(&key(query))
        } else {
            reads.completed.get(&key(query)).cloned()
        };
        if let Some(result) = selected {
            if consuming {
                reads.consumed.push(query);
            }
            return Poll::Ready(result);
        }
        if !reads.requested.contains_key(&key(query)) {
            if reads.completed.len()
                + reads.requested.len()
                + reads.composed.slots()
                + reads.cycle.len()
                >= reads.limit
            {
                return Poll::Ready(Err(reads.resource));
            }
            reads.requested.insert(key(query), query);
        }
        Poll::Pending
    }
}

/// A compiler-owned asynchronous computation whose only suspension points are
/// QueryReads. The factory constructs this on the home worker after admission.
/// Held interpreter values and producer inputs must fit the supplied retained
/// bound in addition to this adapter's checked future/mailbox metadata bound.
pub struct LocalFutureTask {
    id: QueryId,
    weight: MemoryWeight,
    reads: QueryReads,
    future: Pin<Box<dyn Future<Output = Outcome>>>,
    protocol: Reference,
    cycle_error: Reference,
    terminal: Option<Outcome>,
}
impl LocalFutureTask {
    /// Future plus bounded mailbox allocation, excluding producer-owned data.
    /// Each row covers both B-tree nodes and temporary ready-list metadata.
    pub fn retained_bytes_for(future_bytes: usize, reads: usize) -> Option<u64> {
        let mailbox = std::mem::size_of::<Reads>().checked_add(4 * std::mem::size_of::<usize>())?;
        u64::try_from(reads)
            .ok()?
            .checked_mul(512)?
            .checked_add(u64::try_from(future_bytes).ok()?)?
            .checked_add(std::mem::size_of::<Self>() as u64)?
            .checked_add(mailbox as u64)
    }
    /// A namespace limit cannot preallocate beyond the admitted task frame.
    /// Actual reads exceeding either limit return the normal resource refusal.
    pub fn read_capacity(retained: u64, future_bytes: usize, max_reads: usize) -> Option<usize> {
        let available = retained.checked_sub(Self::retained_bytes_for(future_bytes, 0)?)?;
        Some(max_reads.min((available / 512).min(usize::MAX as u64) as usize))
    }
    pub fn new<F: Future<Output = Outcome> + 'static>(
        id: QueryId,
        weight: MemoryWeight,
        max_reads: usize,
        errors: RuntimeErrors,
        create: impl FnOnce(QueryReads) -> F,
    ) -> Result<Self, Reference> {
        let max_reads = Self::read_capacity(weight.retained, std::mem::size_of::<F>(), max_reads)
            .ok_or(errors.resource)?;
        let reads = QueryReads(Rc::new(RefCell::new(Reads {
            publication: Publication::Idle,
            eviction: Eviction::Idle,
            cycle: Vec::new(),
            composed: Composed::Idle,
            working: weight,
            requested: BTreeMap::new(),
            completed: BTreeMap::new(),
            consumed: Vec::new(),
            limit: max_reads,
            resource: errors.resource,
        })));
        let future = Box::pin(create(reads.clone()));
        Ok(Self {
            id,
            weight,
            reads,
            future,
            protocol: errors.protocol,
            cycle_error: errors.cycle,
            terminal: None,
        })
    }
    fn advance(&mut self, cx: &mut dyn QueryContext) -> QueryPoll {
        loop {
            if matches!(self.reads.0.borrow().publication, Publication::Requested) {
                match cx.poll_publication() {
                    Poll::Ready(result) => {
                        self.reads.0.borrow_mut().publication = Publication::Completed(result)
                    }
                    Poll::Pending => return QueryPoll::Waiting(Vec::new()),
                }
            }
            if matches!(self.reads.0.borrow().eviction, Eviction::Requested) {
                let evicted = match cx.poll_evict_body() {
                    Poll::Ready(evicted) => evicted,
                    Poll::Pending => return QueryPoll::Waiting(Vec::new()),
                };
                self.reads.0.borrow_mut().eviction = Eviction::Completed(evicted);
            }
            let requested = self
                .reads
                .0
                .borrow()
                .requested
                .values()
                .copied()
                .collect::<Vec<_>>();
            let mut waiting = Vec::new();
            for query in requested {
                match cx.read(query) {
                    Poll::Pending => waiting.push(query),
                    Poll::Ready(result) => {
                        let cycle = if result == Err(self.cycle_error) {
                            let reads = self.reads.0.borrow();
                            let occupied = reads.completed.len()
                                + reads.requested.len()
                                                + reads.composed.slots();
                            cx.cycle_members(query, reads.limit.saturating_sub(occupied))
                        } else {
                            None
                        };
                        let mut reads = self.reads.0.borrow_mut();
                        if let Some(cycle) = cycle {
                            reads.cycle = cycle;
                        } else if result == Err(self.cycle_error) {
                            reads.cycle.clear();
                        }
                        reads.requested.remove(&key(query));
                        reads.completed.insert(key(query), result);
                    }
                }
            }
            if !waiting.is_empty() {
                return QueryPoll::Waiting(waiting);
            }
            let composed = std::mem::take(&mut self.reads.0.borrow_mut().composed);
            let composed = match composed {
                Composed::Requested(mut task) => match task.poll(cx) {
                    QueryPoll::Waiting(dependencies) => {
                        self.reads.0.borrow_mut().composed = Composed::Requested(task);
                        return QueryPoll::Waiting(dependencies);
                    }
                    QueryPoll::Complete(product) => Composed::Completed(Ok(product)),
                    QueryPoll::Failed(error) => Composed::Completed(Err(error)),
                },
                other => other,
            };
            self.reads.0.borrow_mut().composed = composed;
            let polled = self
                .future
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()));
            // At most the already completed mailbox rows can be consumed by
            // one future poll. Flush before servicing its next requested read.
            let consumed = std::mem::take(&mut self.reads.0.borrow_mut().consumed);
            for query in consumed {
                cx.consume(query);
            }
            match polled {
                Poll::Ready(result) => {
                    self.terminal = Some(result.clone());
                    return finish(result);
                }
                Poll::Pending
                    if self.reads.0.borrow().requested.is_empty()
                        && !matches!(self.reads.0.borrow().composed, Composed::Requested(_))
                        && !matches!(self.reads.0.borrow().eviction, Eviction::Requested)
                        && !matches!(self.reads.0.borrow().publication, Publication::Requested) =>
                {
                    // External async operations cannot wake the query graph.
                    // Reject rather than leaving an unregistered sleeping task.
                    self.terminal = Some(Err(self.protocol));
                    return QueryPoll::Failed(self.protocol);
                }
                Poll::Pending => (),
            }
        }
    }
}
fn finish(result: Outcome) -> QueryPoll {
    match result {
        Ok(product) => QueryPoll::Complete(product),
        Err(error) => QueryPoll::Failed(error),
    }
}
impl LocalQueryTask for LocalFutureTask {
    fn id(&self) -> QueryId {
        self.id
    }
    fn weight(&self) -> MemoryWeight {
        self.weight
    }
    fn poll(&mut self, cx: &mut dyn QueryContext) -> QueryPoll {
        if let Some(result) = self.terminal.clone() {
            return finish(result);
        }
        self.advance(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hirsel_artifact_store::{Action, Address, Domain};
    use hirsel_syntax::ContentHash;
    fn query(n: u8) -> QueryId {
        QueryId {
            kind: 1,
            address: Address(ContentHash::of_bytes(&[n])),
        }
    }
    fn product(id: QueryId) -> ProductRef {
        crate::runtime::test_product(
            id.address,
            Action(id.address.0),
            Domain { kind: 1, schema: 1 },
            b"resource",
        )
    }
    fn error() -> Reference {
        Domain { kind: 1, schema: 1 }
            .reference(b"resource")
            .unwrap()
    }
    fn errors() -> RuntimeErrors {
        RuntimeErrors {
            resource: error(),
            cycle: error(),
            cancelled: error(),
            protocol: error(),
            panicked: error(),
        }
    }
    fn reads() -> QueryReads {
        QueryReads(Rc::new(RefCell::new(Reads {
            publication: Publication::Idle,
            eviction: Eviction::Idle,
            cycle: Vec::new(),
            composed: Composed::Idle,
            working: MemoryWeight::default(),
            requested: BTreeMap::new(),
            completed: BTreeMap::new(),
            consumed: Vec::new(),
            limit: 0,
            resource: error(),
        })))
    }
    #[test]
    fn composed_producer_uses_existing_reads_and_checks_its_grant() {
        struct Producer(MemoryWeight);
        impl QueryTask for Producer {
            fn id(&self) -> QueryId {
                query(2)
            }
            fn weight(&self) -> MemoryWeight {
                self.0
            }
            fn poll(&mut self, cx: &mut dyn QueryContext) -> QueryPoll {
                match cx.read(query(1)) {
                    Poll::Pending => QueryPoll::Waiting(vec![query(1)]),
                    Poll::Ready(result) => finish(result),
                }
            }
        }
        async fn compose(reads: QueryReads, weight: MemoryWeight) -> Outcome {
            reads.compose(Box::new(Producer(weight))).await
        }
        struct PendingOnce(bool);
        impl QueryContext for PendingOnce {
            fn read(&mut self, id: QueryId) -> Poll<Outcome> {
                assert_eq!(id, query(1), "only the original dependency is requested");
                if std::mem::replace(&mut self.0, false) {
                    Poll::Pending
                } else {
                    Poll::Ready(Ok(product(id)))
                }
            }
        }
        for (slots, scratch, accepted) in [(2, 8, true), (1, 8, false), (2, 9, false)] {
            let producer = MemoryWeight {
                retained: 64,
                scratch,
                stack: 0,
            };
            let frame = std::mem::size_of_val(&compose(reads(), producer));
            let mut task = LocalFutureTask::new(
                query(0),
                MemoryWeight {
                    retained: LocalFutureTask::retained_bytes_for(frame, slots).unwrap(),
                    scratch: 8,
                    stack: 0,
                },
                slots,
                errors(),
                move |reads| compose(reads, producer),
            )
            .unwrap();
            let mut cx = PendingOnce(true);
            if accepted {
                assert!(
                    matches!(task.poll(&mut cx), QueryPoll::Waiting(ids) if ids == vec![query(1)])
                );
                assert!(
                    matches!(task.poll(&mut cx), QueryPoll::Complete(value) if value == product(query(1)))
                );
            } else {
                assert!(matches!(task.poll(&mut cx), QueryPoll::Failed(value) if value == error()));
                assert!(cx.0, "a refused grant executes no producer work");
            }
        }
    }
    async fn computation(reads: QueryReads, count: u8) -> Outcome {
        for n in 1..=count {
            reads.read(query(n)).await?;
        }
        Ok(product(query(0)))
    }
    struct Completed;
    impl QueryContext for Completed {
        fn read(&mut self, id: QueryId) -> Poll<Outcome> {
            Poll::Ready(Ok(product(id)))
        }
    }
    #[test]
    fn cycle_members_use_the_existing_mailbox_bound_and_preserve_the_error() {
        struct Cycle;
        impl QueryContext for Cycle {
            fn read(&mut self, _: QueryId) -> Poll<Outcome> {
                Poll::Ready(Err(errors().cycle))
            }
            fn cycle_members(&self, dependency: QueryId, limit: usize) -> Option<Vec<QueryId>> {
                assert_eq!(dependency, query(1));
                (limit >= 3).then(|| vec![query(1), query(2), query(3)])
            }
        }
        async fn consume(reads: QueryReads, slots: usize) -> Outcome {
            assert_eq!(reads.read_consuming(query(1)).await, Err(errors().cycle));
            let members = reads.take_cycle();
            if slots == 4 {
                assert_eq!(members, vec![query(1), query(2), query(3)]);
            } else {
                assert!(members.is_empty(), "a short grant keeps no partial cycle");
            }
            assert!(reads.take_cycle().is_empty());
            Ok(product(query(0)))
        }
        for slots in [3, 4] {
            let frame = std::mem::size_of_val(&consume(reads(), slots));
            let mut task = LocalFutureTask::new(
                query(0),
                MemoryWeight {
                    retained: LocalFutureTask::retained_bytes_for(frame, slots).unwrap(),
                    ..Default::default()
                },
                slots,
                errors(),
                move |reads| consume(reads, slots),
            )
            .unwrap();
            assert!(
                matches!(task.poll(&mut Cycle), QueryPoll::Complete(value) if value == product(query(0)))
            );
        }
    }
    #[test]
    fn mailbox_capacity_charges_actual_frame_and_refuses_excess_real_reads() {
        let frame = std::mem::size_of_val(&computation(reads(), 0));
        for (slots, namespace, count, accepted) in
            [(2, 4096, 2, true), (2, 4096, 3, false), (5, 2, 3, false)]
        {
            let retained = LocalFutureTask::retained_bytes_for(frame, slots).unwrap();
            let mut task = LocalFutureTask::new(
                query(0),
                MemoryWeight {
                    retained,
                    ..Default::default()
                },
                namespace,
                errors(),
                move |reads| computation(reads, count),
            )
            .unwrap();
            assert_eq!(task.reads.0.borrow().limit, slots.min(namespace));
            loop {
                match task.poll(&mut Completed) {
                    QueryPoll::Waiting(_) => (),
                    QueryPoll::Complete(value) => {
                        assert!(accepted);
                        assert_eq!(value, product(query(0)));
                        break;
                    }
                    QueryPoll::Failed(reference) => {
                        assert!(!accepted);
                        assert_eq!(reference, error());
                        break;
                    }
                }
            }
        }
    }
    #[test]
    fn consuming_reads_reuse_rows_and_deliver_dependency_failures() {
        async fn consume(reads: QueryReads) -> Outcome {
            for n in 1..=64 {
                // Repeated identities go through the same settled latch too.
                for _ in 0..2 {
                    let id = query(n);
                    let result = reads.read_consuming(id).await;
                    if n == 32 {
                        assert_eq!(result, Err(error()));
                    } else {
                        assert_eq!(result, Ok(product(id)));
                    }
                    let state = reads.0.borrow();
                    assert!(state.completed.is_empty());
                    assert!(state.requested.is_empty());
                    assert_eq!(state.limit, 2);
                }
            }
            Ok(product(query(0)))
        }
        struct Settled;
        impl QueryContext for Settled {
            fn read(&mut self, id: QueryId) -> Poll<Outcome> {
                Poll::Ready(if id == query(32) {
                    Err(error())
                } else {
                    Ok(product(id))
                })
            }
        }
        let frame = std::mem::size_of_val(&consume(reads()));
        let retained = LocalFutureTask::retained_bytes_for(frame, 2).unwrap();
        let mut task = LocalFutureTask::new(
            query(0),
            MemoryWeight {
                retained,
                ..Default::default()
            },
            2,
            errors(),
            consume,
        )
        .unwrap();
        assert!(
            matches!(task.poll(&mut Settled), QueryPoll::Complete(value) if value == product(query(0)))
        );
    }
    #[test]
    fn insufficient_future_frame_refuses_before_construction_and_overflow_is_checked() {
        let frame = std::mem::size_of_val(&computation(reads(), 0));
        let required = LocalFutureTask::retained_bytes_for(frame, 0).unwrap();
        let created = std::cell::Cell::new(false);
        assert!(
            LocalFutureTask::new(
                query(0),
                MemoryWeight {
                    retained: required - 1,
                    ..Default::default()
                },
                1,
                errors(),
                |reads| {
                    created.set(true);
                    computation(reads, 0)
                }
            )
            .is_err()
        );
        assert!(!created.get());
        assert_eq!(
            LocalFutureTask::retained_bytes_for(usize::MAX, usize::MAX),
            None
        );
    }
}
