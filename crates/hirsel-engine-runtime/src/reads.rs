//! A producer's dependency reads over the running query context. A read runs
//! its producer to completion on this stack; there is no mailbox, saved
//! future or resumption.
use super::*;
use std::cell::RefCell;

/// The dependency reader handed to a local producer for one run. Clone-free:
/// pass it by reference into the producer's helpers.
pub struct QueryReads<'a> {
    cx: RefCell<&'a mut dyn QueryContext>,
    cycle: RefCell<Vec<QueryId>>,
    cycle_error: Reference,
}
impl<'a> QueryReads<'a> {
    pub fn new(cx: &'a mut dyn QueryContext, cycle_error: Reference) -> Self {
        Self {
            cx: RefCell::new(cx),
            cycle: RefCell::new(Vec::new()),
            cycle_error,
        }
    }
    /// The demand-stack path closed by the last read that failed with the
    /// cycle error, taken once.
    pub fn take_cycle(&self) -> Vec<QueryId> {
        std::mem::take(&mut self.cycle.borrow_mut())
    }
    /// Run one producer under this producer's context.
    pub fn compose(&self, mut task: Box<dyn QueryTask>) -> QueryOutcome {
        task.run(&mut **self.cx.borrow_mut())
    }
    /// Retry a producer admission using the runtime's unpinned-body eviction.
    pub fn evict_body(&self) -> bool {
        self.cx.borrow_mut().evict_body()
    }
    pub fn read(&self, query: QueryId) -> QueryOutcome {
        self.read_with(query, false)
    }
    /// Read and take the result into the caller's own state, releasing the
    /// runtime's settled copy; a later read runs the producer again.
    pub fn read_consuming(&self, query: QueryId) -> QueryOutcome {
        self.read_with(query, true)
    }
    fn read_with(&self, query: QueryId, consuming: bool) -> QueryOutcome {
        let mut cx = self.cx.borrow_mut();
        let result = cx.read(query);
        if result == Err(self.cycle_error) {
            *self.cycle.borrow_mut() = cx.cycle_members(query, usize::MAX).unwrap_or_default();
        }
        if consuming {
            cx.consume(query);
        }
        result
    }
}

type Run = Box<dyn for<'a> FnOnce(&'a QueryReads<'a>) -> QueryOutcome>;

/// A local producer that is one function of its dependency reader.
pub struct LocalFnTask {
    id: QueryId,
    weight: MemoryWeight,
    cycle_error: Reference,
    protocol: Reference,
    run: Option<Run>,
}
impl LocalFnTask {
    pub fn new(
        id: QueryId,
        weight: MemoryWeight,
        errors: RuntimeErrors,
        run: impl for<'a> FnOnce(&'a QueryReads<'a>) -> QueryOutcome + 'static,
    ) -> Self {
        Self {
            id,
            weight,
            cycle_error: errors.cycle,
            protocol: errors.protocol,
            run: Some(Box::new(run)),
        }
    }
}
impl LocalQueryTask for LocalFnTask {
    fn id(&self) -> QueryId {
        self.id
    }
    fn weight(&self) -> MemoryWeight {
        self.weight
    }
    fn run(&mut self, cx: &mut dyn QueryContext) -> QueryOutcome {
        let Some(run) = self.run.take() else {
            return Err(self.protocol);
        };
        run(&QueryReads::new(cx, self.cycle_error))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hirsel_artifact_store::{Action, Domain};
    use hirsel_syntax::ContentHash;
    fn query(n: u8) -> QueryId {
        QueryId {
            kind: 1,
            address: Address(ContentHash::of_bytes(&[n])),
        }
    }
    fn reference(text: &[u8]) -> Reference {
        Domain { kind: 1, schema: 1 }.reference(text).unwrap()
    }
    fn product(id: QueryId) -> ProductRef {
        crate::test_product(
            id.address,
            Action(id.address.0),
            Domain { kind: 1, schema: 1 },
            b"resource",
        )
    }
    #[test]
    fn a_cycle_error_keeps_its_stack_path_once_and_only_consuming_reads_release() {
        #[derive(Default)]
        struct Cx {
            consumed: Vec<QueryId>,
        }
        impl QueryContext for Cx {
            fn read(&mut self, id: QueryId) -> QueryOutcome {
                if id == query(1) {
                    Err(reference(b"cycle"))
                } else {
                    Ok(product(id))
                }
            }
            fn consume(&mut self, id: QueryId) {
                self.consumed.push(id);
            }
            fn cycle_members(&self, dependency: QueryId, _: usize) -> Option<Vec<QueryId>> {
                Some(vec![dependency, query(9)])
            }
        }
        let mut cx = Cx::default();
        {
            let reads = QueryReads::new(&mut cx, reference(b"cycle"));
            assert_eq!(reads.read_consuming(query(1)), Err(reference(b"cycle")));
            assert_eq!(reads.take_cycle(), vec![query(1), query(9)]);
            assert!(reads.take_cycle().is_empty());
            assert_eq!(reads.read_consuming(query(2)), Ok(product(query(2))));
            assert_eq!(reads.read(query(3)), Ok(product(query(3))));
        }
        // A plain read keeps the product shared.
        assert_eq!(cx.consumed, vec![query(1), query(2)]);
    }
}
