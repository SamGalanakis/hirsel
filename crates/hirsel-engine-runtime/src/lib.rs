//! Shared admitted-query runtime. Identities describe immutable products; none
//! of these descriptors grants verifier or native-validator authority.
#[cfg(any(test, feature = "test-support"))]
use hirsel_artifact_store::Action;
use hirsel_artifact_store::{Address, Reference};
use std::task::Poll;
#[cfg(test)]
hirsel_guard::install!(test_alloc::Counting: test_alloc::Counting);
#[cfg(test)]
use hirsel_engine_admission::test_alloc;
mod future_task;
mod output_partition;
mod product;
pub use output_partition::RetainedView;
pub mod profile;
mod scheduler;
pub use future_task::{LocalFutureTask, QueryReads};

pub use product::{ArchiveAdmission, ProductArchive};
pub use scheduler::{
    AdmittedRuntime, RuntimeCompletion, RuntimeErrors, RuntimeEvent, RuntimeObserver, RuntimeStats,
};

pub mod backing;

/// Logical address, independent of source revision, worker or request order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QueryId {
    pub kind: u32,
    pub address: Address,
}

/// Session-local request token, never a durable query identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RequestId(pub u64);

mod published;
pub use published::{LiveProductValue, ProductRef};
mod dependencies;
pub use dependencies::LiveDependencyProduct;

#[cfg(any(test, feature = "test-support"))]
pub fn test_product(
    address: Address,
    action: Action,
    domain: hirsel_artifact_store::Domain,
    bytes: &[u8],
) -> ProductRef {
    struct Backing(hirsel_artifact_store::Bytes);
    impl backing::ProductBacking for Backing {
        fn get_content(&self, _: Reference) -> Option<hirsel_artifact_store::Bytes> {
            Some(self.0.clone())
        }
        fn admit_product_memory(&self, _: u64) -> Option<std::sync::Arc<dyn Send + Sync>> {
            None
        }
    }
    let store = Backing(bytes.to_vec().into());
    let reference = domain.reference(bytes).unwrap();
    let budget =
        hirsel_engine_admission::ProcessBudget::new((bytes.len() as u64).saturating_add(4096));
    let mut allowance = budget.reserve(budget.limit()).unwrap();
    let archive = ProductArchive::open(&store, reference, &mut allowance).unwrap();
    ProductRef::import_archive(address, action, archive)
}

/// Upper bounds charged before polling. Retained includes continuation state,
/// held products and mapped bytes. Suspension releases scratch and the worker;
/// pool stacks remain charged once and are reused by ready work.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MemoryWeight {
    pub scratch: u64,
    pub retained: u64,
    pub stack: u64,
}

#[derive(Clone, Debug)]
pub enum QueryPoll {
    Waiting(Vec<QueryId>),
    /// Resume when any dependency settles, so a bounded producer can release
    /// completed body claims before its remaining dependencies finish.
    WaitingAny(Vec<QueryId>),
    Complete(ProductRef),
    /// Immutable diagnostics, including phase/source-order metadata.
    Failed(Reference),
}

pub trait QueryContext {
    /// Working space already admitted for the current poll.
    fn scratch_bytes(&self) -> u64 {
        0
    }

    /// The continuation has retained this dependency's complete observations
    /// and released its raw body. Keep the semantic edge and subscriptions;
    /// only its in-flight body pin ends, until the next read.
    fn release_body(&mut self, _query: QueryId) {}
    /// The caller owns the selected result, its complete observations and exact
    /// facets independently of the execution slot. Release this read's claim;
    /// a later read may reopen a fresh execution generation. This works without
    /// persistence. Pending reads must be consumed only after they settle.
    fn consume(&mut self, query: QueryId) {
        self.release_body(query);
    }
    /// Workers that can run ready tasks of this graph at once. A producer
    /// that consumes independent dependencies in order demands at most this
    /// many ahead, so one worker keeps today's one-at-a-time schedule.
    fn workers(&self) -> usize {
        1
    }
    /// Evict the oldest settled, unpinned Front facts, analyzed or Lower body outside unfinished
    /// roots and active continuations. Producers call this before retrying a
    /// refused admission.
    fn evict_body(&mut self) -> bool {
        false
    }
    fn poll_evict_body(&mut self) -> Poll<bool> {
        Poll::Ready(self.evict_body())
    }
    /// The path closed by a read that returned the runtime's cycle error.
    /// Callers bound the returned identities by their existing mailbox grant.
    fn cycle_members(&self, _dependency: QueryId, _limit: usize) -> Option<Vec<QueryId>> {
        None
    }
    /// Admit an ordered side effect only on the first unfinished logical
    /// root/dependency path. The runtime rechecks liveness at acceptance.
    fn poll_publication(&mut self) -> Poll<Result<(), Reference>> {
        Poll::Ready(Ok(()))
    }
    /// Records an actual dependency. Pending never blocks a worker thread.
    fn read(&mut self, key: QueryId) -> Poll<Result<ProductRef, Reference>>;
}

pub trait QueryTask: Send {
    fn id(&self) -> QueryId;
    fn weight(&self) -> MemoryWeight;
    /// Part of `weight().retained` owned solely by this continuation. Retire
    /// only after its destructor runs. Shared products/mappings must retain
    /// their independent reservation or remain in the non-continuation charge.
    fn continuation_bytes(&self) -> u64 {
        0
    }
    /// Retain continuation state on Pending; no provisional publication.
    fn poll(&mut self, cx: &mut dyn QueryContext) -> QueryPoll;
}

/// Installed by the engine for one immutable input snapshot, never by clients.
pub trait QueryFactory: Send + Sync {
    /// Release reconstructible source caches, independently of a settled node.
    fn evict_cache(&self, _scratch: u64) -> bool {
        false
    }

    /// Drop a settled compact cache entry after archiving its complete facts
    /// within the active producer's already admitted scratch bound.
    fn evict(&self, _key: QueryId, _product: &ProductRef, _scratch: u64) -> bool {
        false
    }
    /// Constructs a continuation without executing its dependencies.
    fn create(&self, key: QueryId) -> Result<Box<dyn QueryTask>, Reference>;
}

/// A non-Send continuation, created, resumed and destroyed on its home worker.
/// Its wait releases execution admission; ordinary Send work can still use the
/// home worker. This permits the interpreter's Rc/RefCell state without an
/// unsafe Send assertion or replaying the computation after a dependency miss.
pub trait LocalQueryTask {
    fn id(&self) -> QueryId;
    fn weight(&self) -> MemoryWeight;
    /// Part of `weight().retained` owned solely by this continuation. Retire
    /// only after its destructor runs. Shared products/mappings must retain
    /// their independent reservation or remain in the non-continuation charge.
    fn continuation_bytes(&self) -> u64 {
        0
    }
    fn poll(&mut self, cx: &mut dyn QueryContext) -> QueryPoll;
}

/// Only immutable construction inputs cross threads. The task itself never does.
pub trait LocalQueryFactory: Send + Sync {
    /// Bounded metadata read; reserves the full continuation before creation.
    fn weight(&self, key: QueryId) -> Result<MemoryWeight, Reference>;
    /// Called on the home worker after admission; never eagerly runs dependencies.
    fn create(&self, key: QueryId) -> Result<Box<dyn LocalQueryTask>, Reference>;
}

pub trait QueryRuntime {
    fn request(&mut self, roots: &[QueryId]) -> RequestId;
    /// Completes every required root and legal transaction before returning,
    /// retaining root order. Producer completion alone cannot publish names.
    fn poll(&mut self, request: RequestId) -> Poll<Result<Vec<ProductRef>, Reference>>;
    fn cancel(&mut self, request: RequestId);
    /// Changes retention only, never validity or verifier authority.
    fn evict_to(&mut self, retained_bytes: u64);
}
