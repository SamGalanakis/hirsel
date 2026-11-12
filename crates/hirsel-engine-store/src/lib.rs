//! The persistent result store (design §1d): a content-keyed seam with four
//! implementations the host selects by name. `none@1` stores nothing,
//! `mem@1` is shared by every session of the process, `dir@1` (the CLI
//! default) persists under the user cache directory, and `sim@1` is a
//! deterministic in-memory store with injectable faults for tests. No choice
//! changes what a program means: a hit is used only when every recorded read
//! still observes its value, so the store changes only how much is recomputed.
#[cfg(test)]
hirsel_guard::install!(test_alloc::Counting: test_alloc::Counting);
pub use hirsel_artifact_store::{
    Action, Address, Association, Bytes, ContentReadError, Contents, Domain, IdentifiedContent,
    MAX_RECORD, Output, Reference,
};
#[cfg(test)]
use hirsel_engine_admission::test_alloc;
use hirsel_query_products::reads::Reads;
use hirsel_query_products::{LoweringPolicy, QueryKind};
use hirsel_syntax::{ContentHash, DefPath};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

mod dir;
mod installed;
mod remote;
mod request;
pub mod scratch;
pub use dir::{DirStore, FORMAT, GcReport};
pub use installed::InstalledSourceRecords;
pub use installed::{
    InstalledDefinition, InstalledDefinitionKey, InstalledDefinitionOwnership, InstalledInputs,
    InstalledPackage, InstalledSource,
};
pub use installed::{
    InstalledSemanticInput, InstalledSpecializations, SEMANTIC_BODY, SEMANTIC_HEADER,
    SEMANTIC_INPUT, SEMANTIC_SOURCE,
};
pub use remote::RemoteStore;
pub use request::RequestStore;

macro_rules! product_backing {
    ($($provider:ty),* $(,)?) => { $(
        impl hirsel_engine_runtime::backing::ProductBacking for $provider {
            fn get_content(&self, reference: Reference) -> Option<Bytes> { ResultStore::get_content(self, reference) }
            fn admit_product_memory(&self, bytes: u64) -> Option<Arc<dyn Send + Sync>> { ResultStore::admit_product_memory(self, bytes) }
        }
    )* };
}
product_backing!(
    NoneStore,
    MemStore,
    SimStore,
    DirStore,
    RemoteStore,
    RequestStore,
    dyn ResultStore + '_
);

/// Failure to read or copy a complete archive under request admission.
#[derive(Debug)]
pub enum ProductArchiveError {
    Content(ContentReadError),
    Unavailable,
    InvalidContent,
    UnsupportedProvider,
    LayoutOverflow,
    Admission {
        stage: &'static str,
        requested: u64,
        retained: u64,
        limit: u64,
    },
    CopyBound {
        required: u64,
        reserved: u64,
    },
    InvalidArchive(std::io::Error),
}
impl From<ProductArchiveError> for hirsel_artifact_store::ContentReadError {
    fn from(error: ProductArchiveError) -> Self {
        use hirsel_artifact_store::ContentReadError;
        match error {
            ProductArchiveError::Content(error) => error,
            ProductArchiveError::Unavailable => ContentReadError::NotFound,
            ProductArchiveError::InvalidContent => ContentReadError::InvalidContent,
            ProductArchiveError::InvalidArchive(error) => ContentReadError::Unreadable(error),
            ProductArchiveError::Admission {
                stage,
                requested,
                retained,
                limit,
            } => ContentReadError::Admission {
                stage,
                requested,
                retained,
                limit,
            },
            ProductArchiveError::UnsupportedProvider => {
                ContentReadError::Refused("provider has no archive-copy admission")
            }
            ProductArchiveError::LayoutOverflow => {
                ContentReadError::Refused("content layout overflow")
            }
            ProductArchiveError::CopyBound { .. } => {
                ContentReadError::Refused("archive copy exceeds its working reservation")
            }
        }
    }
}
impl std::fmt::Display for ProductArchiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Content(error) => error.fmt(f),
            Self::Unavailable => {
                f.write_str("archive backing is unavailable (missing, corrupt or unreadable)")
            }
            Self::InvalidContent => {
                f.write_str("archive backing differs from its complete reference")
            }
            Self::UnsupportedProvider => f.write_str("provider has no archive-copy admission"),
            Self::LayoutOverflow => f.write_str("archive backing layout overflow"),
            Self::Admission {
                stage,
                requested,
                retained,
                limit,
            } => write!(
                f,
                "{stage} admission refused: requested {requested} bytes, retained {retained}, limit {limit}"
            ),
            Self::CopyBound { required, reserved } => write!(
                f,
                "archive copy needs {required} bytes, exceeds its {reserved}-byte working reservation"
            ),
            Self::InvalidArchive(error) => write!(f, "archive validation failed: {error}"),
        }
    }
}
impl std::error::Error for ProductArchiveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Content(error) => Some(error),
            Self::InvalidArchive(error) => Some(error),
            _ => None,
        }
    }
}

/// Stable logical address plus explicit source inputs. The address does not
/// change when the body changes; variants bind inputs and dependency outputs
/// into complete actions before a payload can be adopted.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct EntryKey(pub ContentHash, pub ContentHash, pub QueryKind);
impl EntryKey {
    pub fn new(
        environment: ContentHash,
        definition: &DefPath,
        text: ContentHash,
        kind: QueryKind,
        policy: LoweringPolicy,
    ) -> Self {
        Self(
            ContentHash::of_parts(
                "hirsel.result.address.v3",
                [
                    environment.0.as_slice(),
                    definition.fingerprint().0.as_slice(),
                    &[kind as u8],
                    &[policy as u8],
                ],
            ),
            text,
            kind,
        )
    }
    pub fn hex(&self) -> String {
        self.0.0.iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// Why a producer encodes portable archive content. Required spills are
/// admitted memory-pressure boundaries, independent of backend persistence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArchiveBoundary {
    Ordinary,
    Export,
    RequiredSpill,
}
/// Actual byte-provider outcome. Storage acknowledges the current write only;
/// future cache eviction needs a separate retention scope.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WriteOutcome {
    Refused,
    Stored(Reference),
}

/// A content-keyed byte store. Implementations never fail a compile: a
/// missing, unreadable or corrupt entry is a miss.
pub trait ResultStore: Send + Sync {
    /// Observe encoding at the producer, including request-local content.
    fn archive_encoded(&self, _boundary: ArchiveBoundary) {}

    /// Attach an explicitly selected installed package, independently of cache
    /// hits. `admit` is charged what the package costs its holder before that
    /// is allocated: an open charges its header before reading it and the
    /// rest before reading any section; a package already held charges its
    /// recorded total. A refusal fails the attach and leaves nothing held.
    fn installed_package(
        &self,
        package: &hirsel_syntax::PackageId,
        _compiler: ContentHash,
        _admit: &mut dyn FnMut(u64) -> bool,
    ) -> Result<Arc<InstalledPackage>, String> {
        Err(format!(
            "installed products for {}@{} are not configured",
            package.name, package.version
        ))
    }
    /// Explicit durable installation output. Installation-only records do not
    /// need a second copy in the request's temporary source transport. The
    /// manifest is still published only after the completed Precompute root.
    fn installation_output(&self) -> Option<Arc<dyn ResultStore>> {
        None
    }
    /// Retain an immutable live value in this request's existing partition.
    /// Its charge survives the producer and returns after the final reader.
    fn admit_product_memory(&self, bytes: u64) -> Option<Arc<dyn Send + Sync>>;
    /// The process budget this request's memory comes from, for the evictable
    /// charges a request keeps (checked layout tables); None outside a request.
    fn product_budget(&self) -> Option<Arc<hirsel_engine_admission::ProcessBudget>> {
        None
    }
    /// Attempt live retention before a required spill without exhausting the request.
    fn try_admit_product_memory(&self, bytes: u64) -> Option<Arc<dyn Send + Sync>> {
        self.admit_product_memory(bytes)
    }
    /// Independent operation handle for a provider with mutable physical
    /// epoch state. Nested producers use the returned provider directly.
    fn for_operation(&self) -> Option<Arc<dyn ResultStore>> {
        None
    }
    /// Own every selected provider epoch and installed-root pin until query
    /// completion and materialization. Drop aborts the unpublished tail.
    fn operation_scope(&self) -> StoreOperation {
        StoreOperation {
            persistence: Arc::new(OperationPersistence::new(self.persistence())),
            request_store: false,
            requests: [
                self.local_product_store().map(|store| store.request()),
                self.verifier()
                    .and_then(|cache| cache.publication_request()),
            ]
            .into_iter()
            .flatten()
            .collect(),
            reads: self.read_scope().into_iter().collect(),
        }
    }
    /// The authenticated program-record domain of this operation's selected
    /// installation registrations (native rerun design §3). None for every
    /// provider without one: such an operation neither reads nor publishes B.
    fn program_authority(&self) -> Option<hirsel_validate::programs::ProgramAuthority> {
        None
    }
    /// Bind optional local installation authority to this exact compiler.
    /// Explicit providers retain their configured authority by default.
    fn for_compiler(&self, _identity: ContentHash) -> Option<Arc<dyn ResultStore>> {
        None
    }
    /// Coalesce ordinary product and verifier cache publication while an
    /// admitted producer runs. Storage's existing bounded batches spill under
    /// pressure; a scope never changes query validity or verifier authority.
    fn publication_scope(&self) -> Option<Box<dyn Send + Sync>> {
        let products = self.local_product_store().map(|store| store.request());
        let verifier = self
            .verifier()
            .and_then(|cache| cache.publication_request());
        if products.is_none() && verifier.is_none() {
            return None;
        }
        Some(Box::new(PublicationScope { products, verifier }))
    }
    /// Retain bounded installed-product read state for one compiler operation.
    /// Dropping the guard refreshes the next operation; it never publishes or
    /// supplies validator authority. Writable transactions remain independent.
    fn read_scope(&self) -> Option<Box<dyn Send + Sync>> {
        None
    }
    /// Explicit host-local writable storage for independently owned validator
    /// associations. Ordinary product bytes and read-only roots confer no
    /// authority; only the relevant validator may issue its accepted record.
    fn local_product_store(&self) -> Option<hirsel_artifact_store::Store> {
        None
    }
    /// Local verifier capability, never imported through producer bytes. Custom
    /// and remote providers default to no authority and require first V.
    fn verifier(&self) -> Option<Arc<hirsel_validate::VerifierCache>> {
        None
    }
    /// The provider's content ownership and this request's publication policy.
    /// Installed content and ordinary bytes confer no local verifier authority.
    fn capability(&self) -> ProviderCapability;
    /// What survives this request. Physical capability does not change when an
    /// admitted request switches between Cache and Complete publication.
    fn persistence(&self) -> Persistence {
        self.capability().persistence()
    }
    fn get(&self, key: &EntryKey) -> Option<Bytes>;
    fn put(&self, key: &EntryKey, value: &[u8]) -> WriteOutcome;
    /// The caller accepted `key`'s current variant from its last `get` after
    /// that query's own checks. An installation producer publishes exactly
    /// the variants it adopted or wrote, never one a lookup only touched.
    fn adopt(&self, _key: &EntryKey) {}
    /// Content a later hit may read back but no product row references, such
    /// as a hit's own verifier sidecar. An installation never lists it.
    fn put_cache_content(&self, domain: Domain, bytes: &[u8]) -> Option<Reference> {
        self.put_content(domain, bytes)
    }
    /// Prevent reclamation of the complete transported closure through the
    /// held operation. Unsupported providers refuse Complete publication.
    fn retention_scope(&self) -> Option<Box<dyn Send + Sync>>;
    /// The rows a request expects later revisions to adopt first, replacing
    /// any earlier demand. A bounded provider evicts others before them;
    /// this changes retention only, never what a lookup returns.
    fn demand(&self, _keys: &[EntryKey]) {}
    /// Best-effort publication for a projection no active request depends on.
    /// Request transports forward this directly to the selected cache, without
    /// pinning optional bytes or associations in their required-output budget.
    fn cache_product(&self, address: Address, action: Action, domain: Domain, bytes: &[u8]) {
        if self.persistence().retains()
            && let Some(reference) = self.put_content(domain, bytes)
        {
            self.associate_written_product(address, action, reference);
        }
    }
    /// Best-effort query variants use the ordinary bounded index without
    /// claiming an active request's required-output transport.
    fn cache_query(&self, key: &EntryKey, reads: Reads, bytes: &[u8]) -> Option<Reference> {
        if !self.persistence().retains() {
            return None;
        }
        let output = self.put_content(product_domain(key.2), bytes)?;
        let variant = VariantRef::for_output(key, reads, output);
        let mut entry = self
            .get(key)
            .and_then(|bytes| Entry::decode_for(key, &bytes))
            .unwrap_or_default();
        entry.insert(variant);
        self.put_query(key.2, key, entry.action(key), &entry.encode_for(key));
        Some(output)
    }
    /// Generic product associations share this provider with front queries.
    /// Their registered domains are independent of front QueryKind tags. An
    /// association supplies integrity only, never verifier/native authority.
    fn lookup_product(&self, address: Address) -> Option<Association> {
        if self.capability() == ProviderCapability::Absent {
            return None;
        }
        let bytes = self.get(&product_association_key(address))?;
        if bytes.len() > 1024 {
            return None;
        }
        let (owner, action, reference): (ContentHash, ContentHash, Reference) =
            serde_json::from_slice(&bytes).ok()?;
        (owner == address.0 && reference.len <= hirsel_artifact_store::MAX_RECORD as u64).then_some(
            Association {
                action: Action(action),
                reference,
            },
        )
    }
    /// Publish only after immutable output publication. Eviction or a failed
    /// write leaves a miss; read-only roots never receive an association.
    fn associate_product(&self, address: Address, action: Action, reference: Reference) {
        if matches!(
            self.capability(),
            ProviderCapability::Absent | ProviderCapability::ReadOnly
        ) || self.get_content(reference).is_none()
        {
            return;
        }
        self.associate_written_product(address, action, reference);
    }
    /// Associate a reference just returned by this provider's `put_content`,
    /// or whose bytes this caller has already read and checked. This publishes
    /// only index metadata; it does not reread the completed payload.
    fn associate_written_product(&self, address: Address, action: Action, reference: Reference) {
        if matches!(
            self.capability(),
            ProviderCapability::Absent | ProviderCapability::ReadOnly
        ) {
            return;
        }
        let bytes = serde_json::to_vec(&(address.0, action.0, reference))
            .expect("fixed product association serializes");
        if bytes.len() <= 1024 {
            self.put(&product_association_key(address), &bytes);
        }
    }
    /// Make every record this request transported durable in the provider
    /// that retains it, republishing any record a retaining write refused.
    /// True when a fresh open of the retaining provider reads all of them;
    /// a provider without request-local transport has nothing to move.
    fn materialize(&self) -> bool {
        true
    }
    /// Publish a request root: the association a later or restarted request
    /// opens to adopt this request's result. Under `Complete`, every record
    /// the request transported is made durable first and its deferred
    /// associations become visible before the root; if any record stays
    /// request-local nothing becomes visible. Returns whether a fresh open of
    /// the retaining provider finds this root.
    fn publish_root(&self, address: Address, action: Action, reference: Reference) -> bool {
        self.associate_product(address, action, reference);
        self.persistence().retains()
            && self
                .lookup_product(address)
                .is_some_and(|association| association.reference == reference)
    }
    /// Import a checked archive through this provider's existing admission.
    /// Providers without an admitted request partition require the caller's
    /// explicit grant through `ProductArchive::open` instead.
    fn open_product_archive(
        &self,
        _reference: Reference,
    ) -> Option<hirsel_engine_runtime::ProductArchive> {
        None
    }
    /// Transfer a producer's complete immutable encoded backing into this
    /// provider's existing admission, without reading the transport again.
    fn admit_product_archive(
        &self,
        _reference: Reference,
        _bytes: Bytes,
    ) -> Option<hirsel_engine_runtime::ProductArchive> {
        None
    }
    /// Copy a checked payload into independently admitted compact backing.
    /// This avoids retaining a provider's larger packed container.
    fn copy_product_archive(
        &self,
        _reference: Reference,
    ) -> Result<hirsel_engine_runtime::ProductArchive, ProductArchiveError> {
        Err(ProductArchiveError::UnsupportedProvider)
    }
    /// Read backing for an archive copy, preserving admission failures from
    /// request transport. Other providers report unavailable backing without
    /// inferring a cause from a second read.
    fn read_product_content(&self, reference: Reference) -> Result<Bytes, ProductArchiveError> {
        self.get_content(reference)
            .ok_or(ProductArchiveError::Unavailable)
    }
    /// Read one immutable range. Request-local content tables stream integrity
    /// checks with bounded scratch and retain only the demanded byte window.
    fn read_product_window(
        &self,
        reference: Reference,
        range: std::ops::Range<u64>,
    ) -> Result<Bytes, ProductArchiveError> {
        hirsel_artifact_store::window_read_bytes(reference, range.clone())
            .ok_or(ProductArchiveError::InvalidContent)?;
        self.read_product_content(reference)?
            .range(range.start, range.end - range.start, 1)
            .ok_or(ProductArchiveError::InvalidContent)
    }

    /// Immutable content and logical action indexes use distinct addresses.
    /// Byte providers share their map; directory providers map content directly.
    fn get_content(&self, reference: Reference) -> Option<Bytes> {
        if self.capability() == ProviderCapability::Absent {
            return None;
        }
        let bytes = self.get(&content_key(reference))?;
        (reference.domain.reference(&bytes)? == reference).then_some(bytes)
    }
    fn put_content(&self, domain: Domain, bytes: &[u8]) -> Option<Reference> {
        if matches!(
            self.capability(),
            ProviderCapability::Absent | ProviderCapability::ReadOnly
        ) {
            return None;
        }
        let reference = domain.reference(bytes)?;
        match self.put(&content_key(reference), bytes) {
            WriteOutcome::Refused => None,
            WriteOutcome::Stored(_) => Some(reference),
        }
    }
    /// Carry a checked immutable borrow through layered stores without rehashing.
    fn put_identified_content(&self, content: IdentifiedContent<'_>) -> Option<Reference> {
        self.put_content(content.reference().domain, content.bytes())
            .filter(|reference| *reference == content.reference())
    }
    /// Publish a query product with its domain explicit at the provider seam.
    /// The key already binds the kind; byte-only providers share `put`.
    fn put_query(&self, kind: QueryKind, key: &EntryKey, action: Action, value: &[u8]) {
        let _scope = hirsel_intern::timings::instructions::scope(
            hirsel_intern::timings::instructions::Owner::Store,
        );
        hirsel_intern::timings::instructions::volume(
            hirsel_intern::timings::instructions::Volume::EncodedBytes,
            value.len() as u64,
        );
        assert_eq!(kind, key.2);
        let _ = action;
        self.put(key, value);
    }
    /// The provider name the run config selects it by (`dir@1`, ...).
    fn name(&self) -> &'static str;
}

/// One content capability per provider, independent of local V authority.
/// Only admitted request content carries a request-specific publication policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderCapability {
    /// No ordinary result content is read, captured, encoded or written.
    Absent,
    /// Installed or imported immutable content, without a writable backing.
    ReadOnly,
    /// New content owned by the request's admitted memory or bounded scratch.
    /// Disabled persistence still permits this request's producers and readers
    /// to exchange content; Complete publishes the closure atomically.
    RequestLocal(Persistence),
    /// A backing provider accepts new content beyond this request, best-effort.
    Writable,
}
impl ProviderCapability {
    pub fn persistence(self) -> Persistence {
        match self {
            Self::Absent => Persistence::Disabled,
            Self::ReadOnly => Persistence::Installed,
            Self::Writable => Persistence::Cache,
            Self::RequestLocal(policy) => policy,
        }
    }
}

/// What a request leaves behind in its selected provider. Each provider
/// derives its policy from its capability; a `StoreOperation` captures it once
/// for the request it scopes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Persistence {
    /// Nothing survives the request: no query variant, cached product or
    /// association is encoded or restored. A request transport still carries
    /// the request's live bytes.
    Disabled,
    /// Installed immutable content is restored; nothing this request
    /// publishes outlives it, so its producers keep their live values.
    Installed,
    /// Ordinary products and query variants publish best-effort; eviction or
    /// a failed write is a later miss.
    Cache,
    /// Publication materializes the complete demanded component closure before
    /// any association becomes visible (Precompute, export and restart): a
    /// request transport defers every association to `publish_root`, which
    /// first makes each transported record durable. An abort before the root
    /// leaves content without associations, never a root naming missing content.
    Complete,
}
impl Persistence {
    /// Whether stored content is restored and this request's records reach
    /// its store operation.
    pub fn retains(self) -> bool {
        self != Self::Disabled
    }
    /// Whether new content this request encodes can outlive it.
    pub fn publishes(self) -> bool {
        matches!(self, Self::Cache | Self::Complete)
    }
}

struct OperationPersistence {
    base: Persistence,
    complete: AtomicBool,
    retention: Mutex<Option<Box<dyn Send + Sync>>>,
}
impl OperationPersistence {
    fn new(base: Persistence) -> Self {
        Self {
            base,
            complete: AtomicBool::new(false),
            retention: Mutex::new(None),
        }
    }
    fn persistence(&self) -> Persistence {
        if self.base.publishes() && self.complete.load(Ordering::Acquire) {
            Persistence::Complete
        } else {
            self.base
        }
    }
    fn completing(&self) {
        if self.base.publishes() {
            self.complete.store(true, Ordering::Release);
        }
    }
}

/// The caller joins producers and materializes the result before finishing.
/// Completed pressure prefixes remain durable on abort, as in Request.
pub struct StoreOperation {
    persistence: Arc<OperationPersistence>,
    request_store: bool,
    requests: Vec<hirsel_artifact_store::Request>,
    reads: Vec<Box<dyn Send + Sync>>,
}
impl StoreOperation {
    /// Promise the complete demanded closure for this request's roots
    /// (Precompute and export). A backend that persists nothing stays
    /// `Disabled`: there is nothing for a later request to open.
    pub fn completing(self) -> Self {
        self.persistence.completing();
        self
    }
    /// Unpublished physical batches need an independent abort owner until
    /// completion. Read guards alone do not prevent graph subscribers sharing
    /// the same immutable products.
    pub fn has_publication(&self) -> bool {
        !self.requests.is_empty() || self.request_store && self.persistence().retains()
    }
    /// The policy captured when this operation's scope opened.
    pub fn persistence(&self) -> Persistence {
        self.persistence.persistence()
    }
    /// Joining keeps this operation's captured policy.
    pub fn join(&mut self, mut other: Self) {
        self.requests.append(&mut other.requests);
        self.reads.append(&mut other.reads);
        self.reads.push(Box::new(other.persistence));
        self.request_store |= other.request_store;
    }
    pub fn finish(mut self) -> std::io::Result<()> {
        // Cache publication failure leaves live admitted transport intact.
        let mut result = Ok(());
        for request in self.requests.drain(..) {
            if let Err(error) = request.finish() {
                result = Err(error);
            }
        }
        result
    }
}

struct PublicationScope {
    products: Option<hirsel_artifact_store::Request>,
    verifier: Option<hirsel_artifact_store::Request>,
}
impl Drop for PublicationScope {
    fn drop(&mut self) {
        // Query failures may retain valid completed subproducts. Publication
        // failure is only a cache miss; RequestStore owns live transport.
        for request in [self.products.take(), self.verifier.take()]
            .into_iter()
            .flatten()
        {
            let _ = request.finish();
        }
    }
}

fn product_association_key(address: Address) -> EntryKey {
    // Interpret is the byte-provider transport slot, as for content_key below;
    // it is never a semantic query kind or a scheduler dispatch conversion.
    EntryKey(
        ContentHash::of_parts("hirsel.product.association.v1", [address.0.0.as_slice()]),
        ContentHash::default(),
        QueryKind::Interpret,
    )
}

fn content_key(reference: Reference) -> EntryKey {
    EntryKey(
        reference.output.0,
        ContentHash::default(),
        QueryKind::Interpret,
    )
}
/// The domain of the record a provider holds at an entry key: a query
/// entry, or a product association under its transport slot.
fn entry_domain(kind: QueryKind) -> Domain {
    Domain {
        kind: kind as u32 + 256,
        schema: FORMAT,
    }
}
/// Whether `domain` holds a query entry or a query variant's payload. An
/// installation lists such content only where a published row reaches it.
fn query_record_domain(domain: Domain) -> bool {
    let kinds = QueryKind::SemanticBody as u32 + 1;
    (domain.schema == FORMAT && domain.kind.checked_sub(256).is_some_and(|k| k < kinds))
        || (domain.schema == product_domain(QueryKind::Lex).schema
            && domain.kind.checked_sub(16).is_some_and(|k| k < kinds))
}
/// Ordinary phase-result domain, shared by producers and retaining providers.
pub fn product_domain(kind: QueryKind) -> Domain {
    Domain {
        kind: kind as u32 + 16,
        schema: 9,
    }
}
/// The type records a result payload cites.
pub const TYPE_RECORDS: Domain = hirsel_product_codec::TypeRecords::DOMAIN;
/// Whether `domain` holds result content: a payload a stored row's variants
/// reference (`VariantRef::valid`), or a type record such a payload cites.
pub fn is_result_content(domain: Domain) -> bool {
    domain == TYPE_RECORDS
        || domain.schema == product_domain(QueryKind::Check).schema
            && (16..=QueryKind::SemanticBody as u32 + 16).contains(&domain.kind)
}

/// Whether a store hit is recomputed and compared byte for byte
/// (`--verify-cache`; gate campaigns force it on). A mismatch is a tool error.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum VerifyStore {
    #[default]
    Off,
    Recompute,
}

/// At most this many variants (distinct inputs) per entry.
pub const VARIANTS: usize = 4;

/// One entry: up to `VARIANTS` variants, most recently written first. The
/// entry's key is the seed S; each variant's reads are the typed
/// dependencies D its producer consumed, and the witness binds K = H(S, D) to
/// its immutable output. A consumer accepts a variant only after D validates.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Entry {
    pub variants: Vec<VariantRef>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Variant {
    pub inputs: ContentHash,
    pub reads: Reads,
    pub payload: Bytes,
}

/// The canonical digest of a variant's typed dependencies D.
fn reads_witness(reads: &Reads) -> ContentHash {
    // Reads' in-memory ordering is by interned identity. Sorting the canonical
    // row bytes makes the witness independent of DefId assignment order.
    let mut bytes = Vec::new();
    let mut rows = Vec::with_capacity(reads.len());
    for (key, value) in reads.iter() {
        let start = bytes.len();
        key.encode(&mut bytes);
        bytes.extend(value.0);
        rows.push(start..bytes.len());
    }
    rows.sort_unstable_by(|a, b| bytes[a.clone()].cmp(&bytes[b.clone()]));
    ContentHash::of_parts(
        "hirsel.result.reads.v1",
        rows.iter().map(|row| &bytes[row.clone()]),
    )
}

/// A product's complete identity K = H(S, D): its seed key and the canonical
/// digest of the typed dependencies it was produced from. Execution receipts
/// bind K and the output, so a seed-only hit never carries acceptance.
pub fn product_identity(key: &EntryKey, reads: &Reads) -> ContentHash {
    ContentHash::of_parts(
        "hirsel.product.identity.v1",
        [
            key.0.0.as_slice(),
            key.1.0.as_slice(),
            reads_witness(reads).0.as_slice(),
        ],
    )
}

pub fn query_action(key: &EntryKey, inputs: ContentHash) -> Action {
    Action(ContentHash::of_parts(
        "hirsel.result.action.v2",
        [key.0.0.as_slice(), inputs.0.as_slice()],
    ))
}

/// An action index references immutable content without reading its pages.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VariantRef {
    pub inputs: ContentHash,
    pub reads: Reads,
    pub output: Reference,
    witness: ContentHash,
}
impl VariantRef {
    pub fn publish(
        store: &dyn ResultStore,
        key: &EntryKey,
        reads: Reads,
        bytes: &[u8],
    ) -> Option<Self> {
        let _scope = hirsel_intern::timings::instructions::scope(
            hirsel_intern::timings::instructions::Owner::Store,
        );
        hirsel_intern::timings::instructions::volume(
            hirsel_intern::timings::instructions::Volume::EncodedBytes,
            bytes.len() as u64,
        );
        let output = store.put_content(product_domain(key.2), bytes)?;
        Some(Self::for_output(key, reads, output))
    }
    pub fn for_output(key: &EntryKey, reads: Reads, output: Reference) -> Self {
        let mut variant = Self {
            inputs: key.1,
            reads,
            output,
            witness: ContentHash::default(),
        };
        variant.witness = variant.binding(key);
        variant
    }
    pub fn action(&self, key: &EntryKey) -> Action {
        query_action(key, self.inputs)
    }
    // The witness binds K = H(S, D) to the output, so a well-formed frame
    // cannot substitute another product's dependencies or output.
    fn binding(&self, key: &EntryKey) -> ContentHash {
        ContentHash::of_parts(
            "hirsel.result.binding.v2",
            [
                self.action(key).0.0.as_slice(),
                reads_witness(&self.reads).0.as_slice(),
                self.output.output.0.0.as_slice(),
                self.output.domain.kind.to_le_bytes().as_slice(),
                self.output.domain.schema.to_le_bytes().as_slice(),
                self.output.len.to_le_bytes().as_slice(),
            ],
        )
    }
    pub fn valid(&self, key: &EntryKey) -> bool {
        self.output.domain == product_domain(key.2)
            && self.output.len <= hirsel_artifact_store::MAX_RECORD as u64
            && self.witness == self.binding(key)
    }
    pub fn resolve(&self, store: &dyn ResultStore, key: &EntryKey) -> Option<Variant> {
        if !self.valid(key) {
            return None;
        }
        self.resolve_bound(store)
    }
    // Only callers in this module that checked valid(key) can resolve here.
    // Provider content integrity and domain checks still run at get_content.
    fn resolve_bound(&self, store: &dyn ResultStore) -> Option<Variant> {
        Some(Variant {
            inputs: self.inputs,
            reads: self.reads.clone(),
            payload: store.get_content(self.output)?,
        })
    }
}

const ENTRY_MAGIC: &[u8; 4] = b"HRE6";

impl Entry {
    /// The variant bound to exactly this key's inputs. Its dependencies D are
    /// not compared here; the consumer validates them before adoption.
    pub fn current(&self, key: &EntryKey) -> Option<&VariantRef> {
        self.variants
            .iter()
            .find(|v| v.inputs == key.1 && v.valid(key))
    }
    /// Fetch content for the first bound candidate only; missing content is a
    /// miss, never permission to select an older variant.
    pub fn resolve(&self, store: &dyn ResultStore, key: &EntryKey) -> Option<Variant> {
        self.current(key)?.resolve_bound(store)
    }

    /// Adds a variant first, replacing one with the same inputs and dropping
    /// the least recently written past `VARIANTS`.
    pub fn insert(&mut self, variant: VariantRef) {
        self.variants.retain(|v| v.inputs != variant.inputs);
        self.variants.insert(0, variant);
        self.variants.truncate(VARIANTS);
    }
    pub fn encode(&self) -> Vec<u8> {
        let mut out = ENTRY_MAGIC.to_vec();
        out.push(self.variants.len() as u8);
        for v in &self.variants {
            out.extend(v.inputs.0);
            v.reads.encode(&mut out);
            out.extend(v.output.output.0.0);
            out.extend(v.output.domain.kind.to_le_bytes());
            out.extend(v.output.domain.schema.to_le_bytes());
            out.extend(v.output.len.to_le_bytes());
            out.extend(v.witness.0);
        }
        out
    }
    pub fn decode(bytes: &[u8]) -> Option<Entry> {
        Self::decode_bytes(bytes.into())
    }
    fn decode_bytes(bytes: Bytes) -> Option<Entry> {
        let mut input = bytes.strip_prefix(ENTRY_MAGIC.as_slice())?;
        let (&count, rest) = input.split_first()?;
        input = rest;
        if count as usize > VARIANTS {
            return None;
        }
        let mut variants = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let (inputs, rest) = input.split_at_checked(32)?;
            let inputs = ContentHash(inputs.try_into().ok()?);
            input = rest;
            let reads = Reads::decode(&mut input)?;
            let (reference, rest) = input.split_at_checked(80)?;
            input = rest;
            variants.push(VariantRef {
                inputs,
                reads,
                output: Reference {
                    output: Output(ContentHash(reference[..32].try_into().ok()?)),
                    domain: Domain {
                        kind: u32::from_le_bytes(reference[32..36].try_into().ok()?),
                        schema: u32::from_le_bytes(reference[36..40].try_into().ok()?),
                    },
                    len: u64::from_le_bytes(reference[40..48].try_into().ok()?),
                },
                witness: ContentHash(reference[48..80].try_into().ok()?),
            });
        }
        input.is_empty().then_some(Entry { variants })
    }
    /// Bind the complete entry (including every read) to its requested key.
    /// Rustc's cache uses an expected tag and exact length before adopting a
    /// query (rustc_middle/src/query/on_disk_cache.rs:468-485). Here the existing
    /// checksum frame additionally detects damaged reads and payload bytes.
    pub fn encode_for(&self, key: &EntryKey) -> Vec<u8> {
        let _scope = hirsel_intern::timings::instructions::scope(
            hirsel_intern::timings::instructions::Owner::Store,
        );
        let mut bytes = b"HRE6".to_vec();
        bytes.extend(key.0.0);
        bytes.extend(self.encode());
        frame(&bytes)
    }
    /// Reads `key`'s entry from `store`; an undecodable entry is a miss.
    pub fn load(store: &dyn ResultStore, key: &EntryKey) -> Entry {
        let _scope = hirsel_intern::timings::instructions::scope(
            hirsel_intern::timings::instructions::Owner::Store,
        );
        store
            .get(key)
            .and_then(|bytes| Self::decode_for(key, &bytes))
            .unwrap_or_default()
    }
    /// Decodes bytes `encode_for(key)` wrote; damaged or another key's bytes
    /// are None.
    pub fn decode_for(key: &EntryKey, bytes: &Bytes) -> Option<Entry> {
        let payload = unframe(bytes)?.strip_prefix(b"HRE6")?;
        let payload = payload.strip_prefix(key.0.0.as_slice())?;
        let offset = payload.as_ptr() as usize - bytes.as_ptr() as usize;
        Entry::decode_bytes(bytes.range(offset as u64, payload.len() as u64, 1)?)
    }
    pub fn action(&self, key: &EntryKey) -> Action {
        let actions: Vec<_> = self.variants.iter().map(|v| v.action(key)).collect();
        Action(ContentHash::of_parts(
            "hirsel.result.variants.v1",
            actions.iter().map(|a| a.0.0.as_slice()),
        ))
    }
}

/// `none@1`: stores nothing.
#[derive(Debug, Default)]
pub struct NoneStore;
impl ResultStore for NoneStore {
    fn admit_product_memory(&self, _bytes: u64) -> Option<std::sync::Arc<dyn Send + Sync>> {
        None
    }

    fn retention_scope(&self) -> Option<Box<dyn Send + Sync>> {
        None
    }

    fn capability(&self) -> ProviderCapability {
        ProviderCapability::Absent
    }

    fn get(&self, _: &EntryKey) -> Option<Bytes> {
        None
    }
    fn put(&self, _: &EntryKey, _: &[u8]) -> crate::WriteOutcome {
        crate::WriteOutcome::Refused
    }
    fn name(&self) -> &'static str {
        "none@1"
    }
}

/// `mem@1`: one map per process, shared by every session.
#[derive(Default)]
pub struct MemStore {
    state: Arc<Mutex<MemState>>,
    cap: Option<usize>,
    verifier: Arc<hirsel_validate::VerifierCache>,
    installed: Option<Arc<dyn ResultStore>>,
}
impl std::fmt::Debug for MemStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemStore")
            .field("cap", &self.cap)
            .field("installed", &self.installed.is_some())
            .finish_non_exhaustive()
    }
}
#[derive(Debug, Default)]
struct MemState {
    entries: HashMap<ContentHash, (EntryKey, Bytes)>,
    order: std::collections::VecDeque<ContentHash>,
    bytes: usize,
    live: usize,
    retained: usize,
}
impl MemState {
    fn total_bytes(&self) -> usize {
        self.bytes + self.live
    }
}
struct MemRetention(Arc<Mutex<MemState>>);
impl Drop for MemRetention {
    fn drop(&mut self) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retained -= 1;
    }
}
struct MemLiveLease {
    state: Arc<Mutex<MemState>>,
    bytes: usize,
}
impl Drop for MemLiveLease {
    fn drop(&mut self) {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .live -= self.bytes;
    }
}
fn mem_charge(value: &[u8]) -> usize {
    value.len() + std::mem::size_of::<EntryKey>()
}
impl MemStore {
    /// Bound cache-owned payloads, keys and live leases, evicting cache entries in insertion order.
    /// Outstanding reads retain their immutable bytes independently. Like
    /// DirStore's cap, eviction is a cache miss, never query authority.
    pub fn bounded(bytes: usize) -> Self {
        Self {
            cap: Some(bytes),
            ..Self::default()
        }
    }
    /// The process's shared store.
    pub fn shared() -> Arc<MemStore> {
        static SHARED: OnceLock<Arc<MemStore>> = OnceLock::new();
        SHARED
            .get_or_init(|| Arc::new(MemStore::default().with_installed_products()))
            .clone()
    }
    pub fn with_installed_products(mut self) -> Self {
        if let Some(installed) = DirStore::installed_only() {
            let installed: Arc<dyn ResultStore> = Arc::new(installed);
            self.verifier = installed
                .verifier()
                .unwrap_or_else(|| self.verifier.clone());
            self.installed = Some(installed);
        }
        self
    }
    pub fn len(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entries
            .len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// The bytes held, for `--mem-usage`.
    pub fn bytes(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .total_bytes()
    }
    /// Empties the store, returning what it held.
    pub fn take(&self) -> HashMap<EntryKey, Bytes> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.retained != 0 {
            return HashMap::new();
        }
        state.bytes = 0;
        state.order.clear();
        std::mem::take(&mut state.entries).into_values().collect()
    }
}
impl ResultStore for MemStore {
    fn admit_product_memory(&self, bytes: u64) -> Option<std::sync::Arc<dyn Send + Sync>> {
        let bytes = usize::try_from(bytes.checked_add(
            hirsel_engine_admission::retained_arc_bytes(
                &std::mem::MaybeUninit::<MemLiveLease>::uninit(),
            ),
        )?)
        .ok()?;
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let total = state.total_bytes().checked_add(bytes)?;
        if self.cap.is_some_and(|cap| total > cap) {
            return None;
        }
        state.live = state.live.checked_add(bytes)?;
        drop(state);
        Some(Arc::new(MemLiveLease {
            state: self.state.clone(),
            bytes,
        }))
    }

    fn retention_scope(&self) -> Option<Box<dyn Send + Sync>> {
        if self.cap.is_some() {
            return None;
        }
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retained += 1;
        Some(Box::new(MemRetention(self.state.clone())))
    }

    fn capability(&self) -> ProviderCapability {
        ProviderCapability::Writable
    }

    fn installed_package(
        &self,
        package: &hirsel_syntax::PackageId,
        compiler: ContentHash,
        admit: &mut dyn FnMut(u64) -> bool,
    ) -> Result<Arc<InstalledPackage>, String> {
        self.installed
            .as_ref()
            .ok_or_else(|| {
                "installed products: no installed package root attached to mem@1".to_string()
            })?
            .installed_package(package, compiler, admit)
    }
    fn for_operation(&self) -> Option<Arc<dyn ResultStore>> {
        let installed = self.installed.as_ref()?.clone();
        let installed = installed.for_operation().unwrap_or(installed);
        let verifier = installed
            .verifier()
            .unwrap_or_else(|| self.verifier.clone());
        Some(Arc::new(Self {
            state: self.state.clone(),
            cap: self.cap,
            verifier,
            installed: Some(installed),
        }))
    }
    fn for_compiler(&self, identity: ContentHash) -> Option<Arc<dyn ResultStore>> {
        let installed = self.installed.as_ref()?.clone();
        let installed = installed
            .for_compiler(identity)
            .unwrap_or_else(|| installed.clone());
        let verifier = installed
            .verifier()
            .unwrap_or_else(|| self.verifier.clone());
        Some(Arc::new(Self {
            state: self.state.clone(),
            cap: self.cap,
            verifier,
            installed: Some(installed),
        }))
    }
    fn read_scope(&self) -> Option<Box<dyn Send + Sync>> {
        self.installed.as_ref()?.read_scope()
    }
    fn program_authority(&self) -> Option<hirsel_validate::programs::ProgramAuthority> {
        self.installed.as_ref()?.program_authority()
    }
    fn verifier(&self) -> Option<Arc<hirsel_validate::VerifierCache>> {
        Some(self.verifier.clone())
    }
    fn get(&self, key: &EntryKey) -> Option<Bytes> {
        let cached = self
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entries
            .get(&key.0)
            .map(|(_, bytes)| bytes.clone());
        cached.or_else(|| self.installed.as_ref()?.get(key))
    }
    fn get_content(&self, reference: Reference) -> Option<Bytes> {
        let key = content_key(reference);
        if let Some(bytes) = self
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entries
            .get(&key.0)
            .map(|(_, bytes)| bytes.clone())
            && reference.domain.reference(&bytes) == Some(reference)
        {
            return Some(bytes);
        }
        self.installed.as_ref()?.get_content(reference)
    }
    fn lookup_product(&self, address: Address) -> Option<Association> {
        let bytes = self
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entries
            .get(&product_association_key(address).0)
            .map(|(_, bytes)| bytes.clone());
        if let Some(bytes) = bytes
            && bytes.len() <= 1024
            && let Ok((owner, action, reference)) =
                serde_json::from_slice::<(ContentHash, ContentHash, Reference)>(&bytes)
            && owner == address.0
            && reference.len <= hirsel_artifact_store::MAX_RECORD as u64
        {
            return Some(Association {
                action: Action(action),
                reference,
            });
        }
        self.installed.as_ref()?.lookup_product(address)
    }
    fn put(&self, key: &EntryKey, value: &[u8]) -> crate::WriteOutcome {
        let Some(reference) = entry_domain(key.2).reference(value) else {
            return WriteOutcome::Refused;
        };
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let charge = mem_charge(value);
        if let Some(cap) = self.cap {
            let cap = cap.saturating_sub(state.live);
            if charge > cap {
                return WriteOutcome::Refused;
            }
            // std::collections::VecDeque supplies the FIFO; replacements keep
            // their place, so repeated writes cannot grow eviction metadata.
            while state.bytes
                - state
                    .entries
                    .get(&key.0)
                    .map_or(0, |(_, old)| mem_charge(old))
                > cap - charge
            {
                let oldest = state.order.pop_front().expect("nonempty cache");
                let (_, old) = state.entries.remove(&oldest).expect("queued cache entry");
                state.bytes -= mem_charge(&old);
            }
            if !state.entries.contains_key(&key.0) {
                state.order.push_back(key.0);
            }
        }
        let old = state
            .entries
            .get(&key.0)
            .map_or(0, |(_, bytes)| mem_charge(bytes));
        if state
            .bytes
            .checked_sub(old)
            .and_then(|bytes| bytes.checked_add(charge))
            .and_then(|bytes| bytes.checked_add(state.live))
            .is_none()
        {
            return WriteOutcome::Refused;
        }
        if let Some((_, old)) = state.entries.remove(&key.0) {
            state.bytes -= mem_charge(&old);
        }
        state.entries.insert(key.0, (*key, value.into()));
        state.bytes += charge;
        WriteOutcome::Stored(reference)
    }
    fn name(&self) -> &'static str {
        "mem@1"
    }
}

/// Faults `sim@1` injects, each by operation ordinal (0-based, counted over
/// gets and puts separately), so a test names exactly which operation fails.
#[derive(Clone, Debug, Default)]
pub struct SimFaults {
    /// Puts that are silently lost (a crash before rename).
    pub lose_puts: Vec<u64>,
    /// Gets that return a corrupted payload, which the checksum rejects.
    pub corrupt_gets: Vec<u64>,
}

/// `sim@1`: a deterministic in-memory store with an operation log and
/// injectable faults. Payloads carry the same checksum framing as `dir@1`,
/// so corruption is detected the same way.
#[derive(Debug, Default)]
pub struct SimStore {
    state: Mutex<SimState>,
}
#[derive(Debug, Default)]
struct SimState {
    entries: HashMap<ContentHash, Vec<u8>>,
    faults: SimFaults,
    gets: u64,
    puts: u64,
    log: Vec<SimOp>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SimOp {
    Hit(EntryKey),
    Miss(EntryKey),
    Corrupt(EntryKey),
    Put(EntryKey),
    Lost(EntryKey),
}
impl SimStore {
    pub fn with_faults(faults: SimFaults) -> Self {
        Self {
            state: Mutex::new(SimState {
                faults,
                ..Default::default()
            }),
        }
    }
    pub fn log(&self) -> Vec<SimOp> {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .log
            .clone()
    }
    pub fn len(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entries
            .len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
impl ResultStore for SimStore {
    fn admit_product_memory(&self, _bytes: u64) -> Option<std::sync::Arc<dyn Send + Sync>> {
        None
    }

    fn retention_scope(&self) -> Option<Box<dyn Send + Sync>> {
        None
    }

    fn capability(&self) -> ProviderCapability {
        ProviderCapability::Writable
    }

    fn get(&self, key: &EntryKey) -> Option<Bytes> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let ordinal = state.gets;
        state.gets += 1;
        let Some(framed) = state.entries.get(&key.0).cloned() else {
            state.log.push(SimOp::Miss(*key));
            return None;
        };
        let mut framed = framed;
        if state.faults.corrupt_gets.contains(&ordinal)
            && let Some(last) = framed.last_mut()
        {
            *last ^= 0xff;
        }
        match unframe(&framed) {
            Some(payload) => {
                state.log.push(SimOp::Hit(*key));
                Some(payload.into())
            }
            None => {
                state.entries.remove(&key.0);
                state.log.push(SimOp::Corrupt(*key));
                None
            }
        }
    }
    fn put(&self, key: &EntryKey, value: &[u8]) -> crate::WriteOutcome {
        let Some(reference) = entry_domain(key.2).reference(value) else {
            return WriteOutcome::Refused;
        };
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let ordinal = state.puts;
        state.puts += 1;
        if state.faults.lose_puts.contains(&ordinal) {
            state.log.push(SimOp::Lost(*key));
            return WriteOutcome::Refused;
        }
        state.entries.insert(key.0, frame(value));
        state.log.push(SimOp::Put(*key));
        WriteOutcome::Stored(reference)
    }
    fn name(&self) -> &'static str {
        "sim@1"
    }
}

const FRAME_MAGIC: &[u8; 8] = b"HIRRES01";

/// A stored payload: magic, length, SHA-256 of the payload, payload.
pub fn frame(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 48);
    out.extend(FRAME_MAGIC);
    out.extend((payload.len() as u64).to_le_bytes());
    out.extend(ContentHash::of_parts("hirsel.result.payload.v1", [payload]).0);
    out.extend(payload);
    out
}
/// The payload of a framed record, or `None` on any mismatch.
pub fn unframe(bytes: &[u8]) -> Option<&[u8]> {
    let rest = bytes.strip_prefix(FRAME_MAGIC.as_slice())?;
    let (len, rest) = rest.split_at_checked(8)?;
    let (sum, payload) = rest.split_at_checked(32)?;
    (u64::from_le_bytes(len.try_into().ok()?) as usize == payload.len()
        && ContentHash::of_parts("hirsel.result.payload.v1", [payload]).0 == sum)
        .then_some(payload)
}

/// The store a run-config provider name selects: `none@1`, `mem@1`,
/// `dir@1` (at `root`, else the default cache directory) or `sim@1`.
/// An explicit `dir@1` without a location is an error. The compiler's optional
/// environment-selected result cache treats an unavailable provider as off.
pub fn store_by_name(
    name: &str,
    root: Option<std::path::PathBuf>,
) -> Result<Arc<dyn ResultStore>, String> {
    Ok(match name {
        "none@1" => match DirStore::installed_only() {
            Some(store) => Arc::new(store),
            None => Arc::new(NoneStore),
        },
        "mem@1" => MemStore::shared(),
        "dir@1" => Arc::new(match root {
            Some(root) => DirStore::new(root).with_installed_products(),
            None => DirStore::default_location()
                .ok_or("dir@1: no cache directory (set an absolute XDG_CACHE_HOME or HOME, or select none@1)")?,
        }),
        "remote@1" => {
            let root = root.ok_or("remote@1: no CAS directory")?;
            let cas = Arc::new(
                hirsel_artifact_store::reapi::DirCas::open(root)
                    .map_err(|error| format!("remote@1: {error}"))?,
            );
            Arc::new(RemoteStore::new(cas.clone(), cas))
        }
        "sim@1" => Arc::new(SimStore::default()),
        other => return Err(format!("unknown result store `{other}`")),
    })
}

#[cfg(test)]
mod tests;

/// Release shared result handles when reporting measured compiler memory.
pub fn release_shared_caches() {
    if hirsel_intern::memory::metered() {
        hirsel_intern::memory::released("result store: mem@1", MemStore::shared().take());
    }
}
