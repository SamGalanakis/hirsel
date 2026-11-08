use super::*;
use hirsel_artifact_store::Domain;

fn query(n: u64) -> QueryId {
    QueryId {
        kind: 1,
        address: Address(ContentHash::of_bytes(&n.to_le_bytes())),
    }
}
fn reference(text: &[u8]) -> Reference {
    Domain { kind: 1, schema: 1 }.reference(text).unwrap()
}
fn product(q: QueryId) -> ProductRef {
    crate::test_product(
        q.address,
        Action(q.address.0),
        Domain { kind: 1, schema: 1 },
        &q.address.0.0,
    )
}
fn errors() -> RuntimeErrors {
    RuntimeErrors {
        resource: reference(b"resource"),
        cycle: reference(b"cycle"),
        cancelled: reference(b"cancelled"),
        protocol: reference(b"protocol"),
        panicked: reference(b"panic"),
    }
}
fn runtime() -> AdmittedRuntime {
    let budget = 1 << 30;
    AdmittedRuntime::with_reservation(
        Jobs::ONE,
        hirsel_engine_admission::ProcessBudget::new(budget)
            .reserve(budget)
            .unwrap(),
        errors(),
        1 << 20,
    )
    .unwrap()
}

#[derive(Clone, Default)]
struct Spec {
    dependencies: Vec<u64>,
    consume: bool,
    panic: bool,
}
struct Factory {
    specs: BTreeMap<Key, Spec>,
    claims: Arc<Mutex<BTreeMap<Key, usize>>>,
    cycles: Arc<Mutex<Vec<Vec<QueryId>>>>,
}
impl QueryFactory for Factory {
    fn create(&self, id: QueryId) -> Result<Box<dyn QueryTask>, Reference> {
        *self.claims.lock().unwrap().entry(key(id)).or_default() += 1;
        Ok(Box::new(Task {
            id,
            spec: self.specs.get(&key(id)).ok_or(errors().protocol)?.clone(),
            cycles: self.cycles.clone(),
        }))
    }
}
struct Task {
    id: QueryId,
    spec: Spec,
    cycles: Arc<Mutex<Vec<Vec<QueryId>>>>,
}
impl QueryTask for Task {
    fn id(&self) -> QueryId {
        self.id
    }
    fn weight(&self) -> MemoryWeight {
        MemoryWeight {
            scratch: 1 << 16,
            retained: 16,
            stack: 0,
        }
    }
    fn poll(&mut self, cx: &mut dyn QueryContext) -> QueryPoll {
        assert!(!self.spec.panic, "producer panic");
        for dependency in self.spec.dependencies.iter().copied().map(query) {
            match cx.read(dependency) {
                Poll::Pending => return QueryPoll::Waiting(vec![dependency]),
                Poll::Ready(Err(error)) => {
                    if error == errors().cycle
                        && let Some(path) = cx.cycle_members(dependency, 16)
                    {
                        self.cycles.lock().unwrap().push(path);
                    }
                    return QueryPoll::Failed(error);
                }
                Poll::Ready(Ok(_)) if self.spec.consume => cx.consume(dependency),
                Poll::Ready(Ok(_)) => (),
            }
        }
        QueryPoll::Complete(product(self.id))
    }
}
type Claims = Arc<Mutex<BTreeMap<Key, usize>>>;
fn install(
    rt: &mut AdmittedRuntime,
    specs: &[(u64, Spec)],
) -> (Claims, Arc<Mutex<Vec<Vec<QueryId>>>>) {
    let claims = Claims::default();
    let cycles = Arc::new(Mutex::new(Vec::new()));
    rt.register(
        1,
        Arc::new(Factory {
            specs: specs
                .iter()
                .map(|(n, spec)| (key(query(*n)), spec.clone()))
                .collect(),
            claims: claims.clone(),
            cycles: cycles.clone(),
        }),
    )
    .unwrap();
    (claims, cycles)
}
fn deps(dependencies: &[u64]) -> Spec {
    Spec {
        dependencies: dependencies.to_vec(),
        ..Spec::default()
    }
}
fn claims(claims: &Claims, n: u64) -> usize {
    claims
        .lock()
        .unwrap()
        .get(&key(query(n)))
        .copied()
        .unwrap_or(0)
}

#[test]
fn a_read_that_closes_a_cycle_fails_with_the_cycle_error_and_its_stack_path() {
    let mut rt = runtime();
    let (_, cycles) = install(
        &mut rt,
        &[(1, deps(&[2])), (2, deps(&[3])), (3, deps(&[1]))],
    );
    let request = rt.request(&[query(1)]);
    assert_eq!(rt.wait(request), Err(errors().cycle));
    // Only the reader that closed the cycle sees the cycle error from a read
    // of a producer still being demanded; its path runs from that producer.
    assert_eq!(
        cycles.lock().unwrap().first(),
        Some(&vec![query(1), query(2), query(3)])
    );
}

#[test]
fn a_shared_dependency_runs_once_unless_its_reader_consumed_it() {
    let mut rt = runtime();
    let (seen, _) = install(
        &mut rt,
        &[
            (1, deps(&[3, 4])),
            (3, deps(&[5])),
            (4, deps(&[5])),
            (5, deps(&[])),
            (
                6,
                Spec {
                    dependencies: vec![7],
                    consume: true,
                    ..Spec::default()
                },
            ),
            (7, deps(&[])),
            (8, deps(&[7])),
        ],
    );
    let request = rt.request(&[query(1), query(6), query(8)]);
    let products = rt.wait(request).unwrap();
    assert_eq!(
        products.iter().map(|p| p.address).collect::<Vec<_>>(),
        [query(1), query(6), query(8)].map(|q| q.address)
    );
    assert_eq!(claims(&seen, 5), 1);
    // A consumed product leaves the runtime; a later reader recomputes it.
    assert_eq!(claims(&seen, 7), 2);
}

#[test]
fn a_panicking_producer_settles_as_panicked_and_later_roots_still_complete() {
    let mut rt = runtime();
    install(
        &mut rt,
        &[
            (1, deps(&[2])),
            (
                2,
                Spec {
                    panic: true,
                    ..Spec::default()
                },
            ),
            (3, deps(&[])),
        ],
    );
    let failed = rt.request(&[query(1)]);
    assert_eq!(rt.wait(failed), Err(errors().panicked));
    let next = rt.request(&[query(3)]);
    assert_eq!(rt.wait(next).unwrap()[0].address, query(3).address);
}
