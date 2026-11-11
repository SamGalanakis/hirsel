//! Shared admitted-query runtime. Identities describe immutable products; none
//! of these descriptors grants verifier or native-validator authority.
#[cfg(any(test, feature = "test-support"))]
use hirsel_artifact_store::Action;
use hirsel_artifact_store::{Address, Reference};
#[cfg(test)]
hirsel_guard::install!(test_alloc::Counting: test_alloc::Counting);
#[cfg(test)]
use hirsel_engine_admission::test_alloc;
mod output_partition;
mod product;
mod reads;
pub use output_partition::RetainedView;
pub mod profile;
mod scheduler;
pub use reads::{LocalFnTask, QueryReads};

pub use product::{ArchiveAdmission, ProductArchive};
pub use scheduler::{AdmittedRuntime, RuntimeErrors, RuntimeEvent, RuntimeObserver, RuntimeStats};

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

/// Upper bounds charged before a producer runs. Retained includes held
/// products and mapped bytes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MemoryWeight {
    pub scratch: u64,
    pub retained: u64,
    pub stack: u64,
}

/// A producer's settled product, or its immutable diagnostics (including
/// phase/source-order metadata).
pub type QueryOutcome = Result<ProductRef, Reference>;

pub trait QueryContext {
    /// Working space already admitted for the running producer.
    fn scratch_bytes(&self) -> u64 {
        0
    }
    /// The caller owns the selected result independently of the runtime.
    /// Release the settled product; a later read runs its producer again.
    fn consume(&mut self, _query: QueryId) {}
    /// Evict the oldest settled, unpinned Front facts, analyzed or Lower body
    /// outside the demand stack. Producers call this before retrying a
    /// refused admission.
    fn evict_body(&mut self) -> bool {
        false
    }
    /// The path closed by a read that returned the runtime's cycle error.
    fn cycle_members(&self, _dependency: QueryId, _limit: usize) -> Option<Vec<QueryId>> {
        None
    }
    /// Runs the dependency to completion on this stack and records the edge.
    fn read(&mut self, key: QueryId) -> QueryOutcome;
}

pub trait QueryTask: Send {
    fn id(&self) -> QueryId;
    fn weight(&self) -> MemoryWeight;
    fn run(&mut self, cx: &mut dyn QueryContext) -> QueryOutcome;
}

/// Installed by the engine for one immutable input snapshot, never by clients.
pub trait QueryFactory: Send + Sync {
    /// A completed product retained by this immutable factory's admitted owner.
    /// This lookup executes no producer or dependency.
    fn completed(&self, _key: QueryId) -> Option<QueryOutcome> {
        None
    }
    /// Release reconstructible source caches, independently of a settled node.
    fn evict_cache(&self, _scratch: u64) -> bool {
        false
    }
    /// An eviction point between top-level roots, with no task running:
    /// evict optional facts until the process headroom is free again.
    fn restore_headroom(&self) {}

    /// Drop a settled compact cache entry after archiving its complete facts
    /// within the active producer's already admitted scratch bound.
    fn evict(&self, _key: QueryId, _product: &ProductRef, _scratch: u64) -> bool {
        false
    }
    /// Constructs a producer without executing its dependencies.
    fn create(&self, key: QueryId) -> Result<Box<dyn QueryTask>, Reference>;
}

/// A non-Send producer, created, run and destroyed on the demanding stack.
/// This permits the interpreter's Rc/RefCell state without an unsafe Send
/// assertion.
pub trait LocalQueryTask {
    fn id(&self) -> QueryId;
    fn weight(&self) -> MemoryWeight;
    fn run(&mut self, cx: &mut dyn QueryContext) -> QueryOutcome;
}

/// Only immutable construction inputs cross threads. The task itself never does.
pub trait LocalQueryFactory: Send + Sync {
    /// Bounded metadata read; reserves the full producer before creation.
    fn weight(&self, key: QueryId) -> Result<MemoryWeight, Reference>;
    /// Called after admission; never eagerly runs dependencies.
    fn create(&self, key: QueryId) -> Result<Box<dyn LocalQueryTask>, Reference>;
}

pub trait QueryRuntime {
    fn request(&mut self, roots: &[QueryId]) -> RequestId;
    /// Completes every required root and legal transaction before returning,
    /// retaining root order. Producer completion alone cannot publish names.
    fn wait(&mut self, request: RequestId) -> Result<Vec<ProductRef>, Reference>;
    fn cancel(&mut self, request: RequestId);
    /// Changes retention only, never validity or verifier authority.
    fn evict_to(&mut self, retained_bytes: u64);
}
