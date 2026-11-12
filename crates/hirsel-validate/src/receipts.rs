//! Local verifier authority, separate from producer query associations. Imported
//! content can share the physical store but cannot publish through this API.
mod universe;
use crate::{SignatureLookup, VRead, Validation};
mod adopt;
mod execution_product;
mod installation;
mod outputs;
use bincode::Options;
pub use execution_product::{ExecutionProductBinding, ExecutionProductInputs};
use hirsel_artifact_store::{Action, Address, Domain, Store};
use hirsel_span::Anchor;
use hirsel_syntax::{ContentHash, DefPath, PackageId};
use hirsel_tir::Definition;
use hirsel_types::TypeInterner;
pub use installation::{AuthenticatedHeader, Installation, Registered};
pub use outputs::{
    SourceInvocation, StoredDefinitionOutput, VerifiedDefinitionOutput, VerifiedInvocations,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex, Weak},
};

/// An immutable raw owner supplied to the existing V algorithm. Neither live
/// ownership nor checked archive framing grants acceptance.
#[derive(Clone, Copy)]
pub enum DefinitionInput<'a> {
    Live(&'a hirsel_tir::DefinitionRef),
    Archived(&'a Arc<hirsel_product_codec::CheckedTir>),
}
impl<'a> From<&'a hirsel_tir::DefinitionRef> for DefinitionInput<'a> {
    fn from(value: &'a hirsel_tir::DefinitionRef) -> Self {
        Self::Live(value)
    }
}
impl<'a> From<&'a Arc<hirsel_product_codec::CheckedTir>> for DefinitionInput<'a> {
    fn from(value: &'a Arc<hirsel_product_codec::CheckedTir>) -> Self {
        Self::Archived(value)
    }
}
impl<'a> DefinitionInput<'a> {
    fn view(self) -> hirsel_product_codec::TirView<'a> {
        match self {
            Self::Live(value) => hirsel_product_codec::TirView::Live(value),
            Self::Archived(value) => hirsel_product_codec::TirView::Archived(value),
        }
    }
    fn downgrade(self) -> DefinitionBinding {
        match self {
            Self::Live(value) => DefinitionBinding::Live(Arc::downgrade(value)),
            Self::Archived(value) => DefinitionBinding::Archived(Arc::downgrade(value)),
        }
    }
}
#[derive(Clone, Debug)]
enum DefinitionBinding {
    Live(Weak<Definition>),
    Archived(Weak<hirsel_product_codec::CheckedTir>),
}
impl DefinitionBinding {
    fn holds(&self, input: DefinitionInput<'_>) -> bool {
        match (self, input) {
            (Self::Live(bound), DefinitionInput::Live(value)) => {
                bound.ptr_eq(&Arc::downgrade(value))
            }
            (Self::Archived(bound), DefinitionInput::Archived(value)) => {
                bound.ptr_eq(&Arc::downgrade(value))
            }
            _ => false,
        }
    }
}

/// Local evidence for one immutable source Definition, minted only by V.
/// A weak binding preserves retention limits; copying reads cannot create it.
/// This capability is deliberately neither serializable nor deserializable.
#[derive(Debug)]
pub struct VerifiedDefinition {
    definition: DefinitionBinding,
    owner: hirsel_syntax::DefId,
    types: Weak<TypeInterner>,
    policy: ContentHash,
    accepted: Option<AcceptedArtifact>,
    reads: Vec<VRead>,
    environment: Mutex<Weak<()>>,
}
impl Clone for VerifiedDefinition {
    fn clone(&self) -> Self {
        Self {
            definition: self.definition.clone(),
            owner: self.owner,
            types: self.types.clone(),
            policy: self.policy,
            accepted: self.accepted.clone(),
            reads: self.reads.clone(),
            environment: Mutex::new(self.environment.lock().unwrap().clone()),
        }
    }
}
impl VerifiedDefinition {
    pub fn holds<'a, 'input>(
        &self,
        definition: impl Into<DefinitionInput<'input>>,
        types: &Arc<TypeInterner>,
        policy: ContentHash,
        signatures: impl Into<crate::SignatureInputs<'a>>,
    ) -> bool {
        self.definition.holds(definition.into())
            && self.types.ptr_eq(&Arc::downgrade(types))
            && self.environment_holds(policy, signatures)
    }
    /// A clean stamp is scoped to V's exact immutable snapshot. Every new
    /// snapshot and every general lookup checks the complete observations.
    pub fn environment_holds<'a>(
        &self,
        policy: ContentHash,
        signatures: impl Into<crate::SignatureInputs<'a>>,
    ) -> bool {
        if self.policy != policy {
            return false;
        }
        let signatures = signatures.into();
        let Some(types) = self.types.upgrade() else {
            return false;
        };
        if let Some(stamp) = signatures.stamp() {
            if !Arc::ptr_eq(&types, &stamp.types) {
                return false;
            }
            if self
                .environment
                .lock()
                .unwrap()
                .ptr_eq(&Arc::downgrade(&stamp.identity))
            {
                return true;
            }
            if !self.reads.iter().all(|r| match stamp.verifier.upgrade() {
                Some(cache) => cache.read_holds(&types, r, signatures).unwrap_or(false),
                None => crate::input::read_holds_for_types(&types, r, signatures.lookup()),
            }) {
                return false;
            }
            *self.environment.lock().unwrap() = Arc::downgrade(&stamp.identity);
            true
        } else {
            self.reads
                .iter()
                .all(|r| crate::input::read_holds_for_types(&types, r, signatures.lookup()))
        }
    }
    pub fn owner(&self) -> hirsel_syntax::DefId {
        self.owner
    }
    pub fn reads(&self) -> &Vec<VRead> {
        &self.reads
    }
    /// The portable acceptance issued with this evidence. Its consumer must
    /// first check the exact immutable artifact and current environment.
    pub fn accepted_artifact(&self) -> Option<AcceptedArtifact> {
        self.accepted.clone()
    }
}

/// The ordinary receipt query, additionally binding local evidence to the
/// immutable input. Misses run the existing validator exactly once.
pub fn validate_definition<'a, 'input>(
    definition: impl Into<DefinitionInput<'input>>,
    types: Arc<TypeInterner>,
    owner: impl FnMut(Anchor) -> Option<(DefPath, u32)>,
    signatures: impl Into<crate::SignatureInputs<'a>>,
    policy: ContentHash,
    cache: Option<&VerifierCache>,
) -> Result<
    (
        Validation,
        bool,
        Option<AcceptedArtifact>,
        Option<VerifiedDefinition>,
    ),
    crate::ViewRefusal,
> {
    let definition = definition.into();
    let signatures = signatures.into();
    let mut reads = vec![];
    let artifact = Artifact::new(definition.view(), types.clone(), owner);
    let (report, ran, accepted) =
        validate_with_receipt_in(&artifact, signatures, policy, cache, &mut reads)?;
    let evidence = report.violations.is_empty().then(|| VerifiedDefinition {
        definition: definition.downgrade(),
        owner: artifact
            .definition
            .header()
            .expect("validated TIR header")
            .path,
        types: Arc::downgrade(&types),
        policy,
        accepted: accepted.clone(),
        reads,
        environment: Mutex::new(
            signatures
                .stamp()
                .map_or_else(Weak::new, |stamp| Arc::downgrade(&stamp.identity)),
        ),
    });
    Ok((report, ran, accepted, evidence))
}

const VIEW_DOMAIN: Domain = Domain {
    kind: 0x5600_0003,
    schema: 3,
};
fn view_key(digest: crate::ValidationDigest, policy: ContentHash) -> ContentHash {
    ContentHash::of_parts(
        "hirsel.validate.signature-receipt.v3",
        [digest.0.0.as_slice(), policy.0.as_slice()],
    )
}

const DOMAIN: Domain = Domain {
    kind: 0x5600_0001,
    schema: 3,
};

/// An exact artifact identity computed here from every field and reachable type
/// row. Its definition and interner stay bound to that identity for validation.
pub struct Artifact<'a> {
    definition: hirsel_product_codec::TirView<'a>,
    types: Arc<TypeInterner>,
    identity: Option<ContentHash>,
    owners: std::collections::BTreeMap<Anchor, (DefPath, u32)>,
    overlay: Option<hirsel_types::SignatureRef>,
}
impl<'a> Artifact<'a> {
    pub fn new(
        definition: impl Into<hirsel_product_codec::TirView<'a>>,
        types: Arc<TypeInterner>,
        owner: impl FnMut(Anchor) -> Option<(DefPath, u32)>,
    ) -> Self {
        Self::identify(definition.into(), types, owner, None)
    }
    /// Identify a private artifact and its exact replacement signature in one
    /// pass. The ordinary body identity is not an intermediate product.
    pub fn private(
        definition: &'a Definition,
        types: Arc<TypeInterner>,
        signature: hirsel_types::SignatureRef,
        owner: impl FnMut(Anchor) -> Option<(DefPath, u32)>,
    ) -> Self {
        Self::identify(
            hirsel_product_codec::TirView::Live(definition),
            types,
            owner,
            Some(signature),
        )
    }
    fn identify(
        definition: hirsel_product_codec::TirView<'a>,
        types: Arc<TypeInterner>,
        mut owner: impl FnMut(Anchor) -> Option<(DefPath, u32)>,
        overlay: Option<hirsel_types::SignatureRef>,
    ) -> Self {
        if let hirsel_product_codec::TirView::Archived(archive) = definition
            && (!archive.uses_types(&types) || archive.bind_types().is_none())
        {
            return Self {
                definition,
                types,
                identity: None,
                owners: Default::default(),
                overlay,
            };
        }
        let mut owners = std::collections::BTreeMap::new();
        let mut bind = |anchor| {
            let value = owner(anchor)?;
            owners.insert(anchor, value.clone());
            Some(value)
        };
        let (domain, bytes) = if overlay.is_some() {
            (
                "hirsel.validate.private-artifact.v2",
                hirsel_product_codec::canonical_bytes(
                    types.clone(),
                    &(definition, &overlay),
                    &mut bind,
                ),
            )
        } else {
            (
                "hirsel.validate.artifact.v1",
                hirsel_product_codec::canonical_bytes(types.clone(), &definition, &mut bind),
            )
        };
        let identity = bytes.map(|bytes| ContentHash::of_parts(domain, [bytes.as_slice()]));
        Self {
            definition,
            types,
            identity,
            owners,
            overlay,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Receipt {
    action: ContentHash,
    artifact: ContentHash,
    policy: ContentHash,
    reads: Vec<VRead>,
}
fn original_receipt_key(action: ContentHash) -> ContentHash {
    ContentHash::of_parts("hirsel.validate.original-receipt.v1", [action.0.as_slice()])
}
fn receipt_action(artifact: ContentHash, policy: ContentHash, reads: &[VRead]) -> ContentHash {
    // VRead's DefIds encode stable DefPaths; order is V's canonical read order.
    let encoded = bincode::serialize(reads).expect("verifier reads are encodable");
    ContentHash::of_parts(
        "hirsel.validate.action.v3",
        [
            artifact.0.as_slice(),
            policy.0.as_slice(),
            encoded.as_slice(),
        ],
    )
}
#[derive(Serialize, Deserialize)]
struct ViewReceipt {
    digest: crate::ValidationDigest,
    policy: ContentHash,
    reads: Vec<VRead>,
}
enum Backend {
    Memory(
        Arc<
            Mutex<
                HashMap<
                    ContentHash,
                    (
                        hirsel_artifact_store::Reference,
                        hirsel_artifact_store::Bytes,
                    ),
                >,
            >,
        >,
        Arc<hirsel_artifact_store::Memory>,
    ),
    Directory(Store),
    /// Host rows and contents, and for a Precompute's installation the
    /// previous registered generation it may adopt V's own rows from.
    Hosted(
        Arc<dyn VerifierRows>,
        Arc<dyn hirsel_artifact_store::Contents>,
        Option<Arc<installation::Rows>>,
    ),
    Registered(Arc<VerifierCache>, Arc<installation::Rows>),
    Transport(
        Arc<VerifierCache>,
        Arc<dyn VerifierRows>,
        Arc<TransportContents>,
    ),
}

struct TransportContents {
    selected: Arc<dyn hirsel_artifact_store::Contents>,
    request: Arc<dyn hirsel_artifact_store::Contents>,
}
impl hirsel_artifact_store::Contents for TransportContents {
    fn get_content(
        &self,
        reference: hirsel_artifact_store::Reference,
    ) -> Result<hirsel_artifact_store::Bytes, hirsel_artifact_store::ContentReadError> {
        if let Ok(bytes) = self.request.get_content(reference) {
            return Ok(bytes);
        }
        let bytes = self.selected.get_content(reference)?;
        if self.request.put_content(reference.domain, &bytes) != Some(reference) {
            return Err(hirsel_artifact_store::ContentReadError::Refused(
                "request content retention",
            ));
        }
        Ok(bytes)
    }
    fn put_content(
        &self,
        domain: Domain,
        bytes: &[u8],
    ) -> Option<hirsel_artifact_store::Reference> {
        self.put_identified_content(domain.identify(bytes)?)
    }
    fn put_identified_content(
        &self,
        content: hirsel_artifact_store::IdentifiedContent<'_>,
    ) -> Option<hirsel_artifact_store::Reference> {
        let reference = content.reference();
        if self.request.put_identified_content(content)? != reference {
            return None;
        }
        let _ = self.selected.put_identified_content(content);
        Some(reference)
    }
}
/// Retaining a local verifier fact exceeded its existing owner's allowance.
#[derive(Debug)]
pub struct FactRefusal;
/// A host-owned, bounded home for verifier rows and their content. Only this
/// cache publishes rows, so product bytes in the same host cannot supply them.
/// Either may be dropped by the host's bound; a missing row is a miss.
pub trait VerifierRows: hirsel_artifact_store::Contents {
    fn publication_batch(&self) -> Option<hirsel_artifact_store::Batch> {
        None
    }
    fn publication_request(&self) -> Option<hirsel_artifact_store::Request> {
        None
    }
    /// Reserve request-owned storage for a verifier-issued local fact. The
    /// lease releases its charge when the immutable environment is retired.
    fn retain_fact(&self, _bytes: u64) -> Result<Option<Arc<dyn Send + Sync>>, FactRefusal> {
        Ok(None)
    }
    /// Account for a decoded immutable replay through the host's existing
    /// signature-payload bound and its ordinary fact reservation.
    fn retain_replay(
        &self,
        _archive: &hirsel_product_codec::CheckedTir,
        _overlay: Option<&hirsel_types::SignatureRef>,
        _metadata: u64,
    ) -> Result<Option<Arc<dyn Send + Sync>>, FactRefusal> {
        Ok(None)
    }
    fn row(&self, key: ContentHash) -> Option<hirsel_artifact_store::Reference>;
    fn set_row(&self, key: ContentHash, reference: hirsel_artifact_store::Reference);
}
/// Only `validate` can issue a receipt. There is no producer/import/seed method.
/// Directory associations are trusted local state; exchanging output bytes
/// never exchanges these associations. Arbitrary filesystem writers are outside
/// this local-cache authority, as with the compiler's existing trusted cache.
pub struct VerifierCache(
    Backend,
    Mutex<Option<Arc<hirsel_product_codec::TypeRecords>>>,
    Arc<()>,
    crate::FactTable<ReplayKey, ReplayArtifact>,
    Mutex<Vec<EnvironmentOwner>>,
);
struct EnvironmentOwner {
    facts: Weak<crate::views::EnvironmentFacts>,
    _lease: Arc<dyn Send + Sync>,
}
impl std::fmt::Debug for VerifierCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("VerifierCache")
    }
}
impl Default for VerifierCache {
    fn default() -> Self {
        Self(
            Backend::Memory(Arc::default(), Arc::default()),
            Mutex::default(),
            Arc::new(()),
            crate::FactTable::default(),
            Mutex::default(),
        )
    }
}
impl VerifierCache {
    /// Keep the trusted namespace while giving an independent compiler
    /// operation its own physical epoch. A shared ordinary root joins the
    /// supplied operation handle; transported/hosted owners already have one.
    pub fn for_operation(self: &Arc<Self>, shared: Option<&Store>) -> Arc<Self> {
        match &self.0 {
            Backend::Memory(rows, contents) => Arc::new(Self(
                Backend::Memory(rows.clone(), contents.clone()),
                Mutex::default(),
                Arc::new(()),
                crate::FactTable::default(),
                Mutex::default(),
            )),
            Backend::Directory(store) => Arc::new(Self::directory_store(
                shared
                    .filter(|shared| shared.root() == store.root())
                    .cloned()
                    .unwrap_or_else(|| store.for_operation()),
            )),
            // DirStore reselects a registration against the current installed
            // generation for each operation. Do not retain the previous pin.
            Backend::Registered(local, _) => local.for_operation(shared),
            _ => self.clone(),
        }
    }
    /// Use the same immutable output containers as ordinary query products.
    /// The verifier's domain and address associations remain separately owned.
    pub fn directory(root: PathBuf) -> Self {
        Self::directory_store(Store::rebuildable(root))
    }
    /// Share physical publication state with ordinary products in the selected
    /// trusted local directory. This does not import verifier associations.
    pub fn directory_store(store: Store) -> Self {
        Self(
            Backend::Directory(store),
            Mutex::default(),
            Arc::new(()),
            crate::FactTable::default(),
            Mutex::default(),
        )
    }
    /// Keep rows, receipts and type records in the host's bounded storage.
    pub fn hosted<T: VerifierRows + 'static>(rows: Arc<T>) -> Self {
        Self(
            Backend::Hosted(rows.clone(), rows, None),
            Mutex::default(),
            Arc::new(()),
            crate::FactTable::default(),
            Mutex::default(),
        )
    }

    /// Keep this request's verifier content through best-effort cache eviction.
    /// Association lookup and publication remain in the selected verifier;
    /// transport bytes grant no authority and use the caller's existing bound.
    pub fn with_transport<T: VerifierRows + 'static>(
        self: &Arc<Self>,
        request: Arc<T>,
    ) -> Arc<Self> {
        Arc::new(Self(
            Backend::Transport(
                self.clone(),
                request.clone(),
                Arc::new(TransportContents {
                    selected: self.contents(),
                    request,
                }),
            ),
            Mutex::default(),
            Arc::new(()),
            crate::FactTable::default(),
            Mutex::default(),
        ))
    }

    /// A bounded home for new in-memory signature receipts. Durable/hosted
    /// verifier backends already own their storage. An explicit unbounded
    /// memory cache cannot receive writes charged to a demanded universe.
    pub(crate) fn for_universe(
        self: &Arc<Self>,
        memory: Arc<dyn crate::ValidationMemory>,
    ) -> Result<Arc<Self>, crate::ViewRefusal> {
        match &self.0 {
            Backend::Memory(..) => Ok(Arc::new(Self::hosted(universe::UniverseReceipts::new(
                memory,
            )?))),
            _ => Ok(self.clone()),
        }
    }
    pub fn fact_graph(&self) -> Arc<crate::FactGraph> {
        self.3.graph()
    }
    fn remember(&self, stamp: &crate::views::EnvironmentStamp, lease: &Arc<dyn Send + Sync>) {
        let mut owners = self.4.lock().unwrap();
        owners.retain(|owner| owner.facts.strong_count() != 0);
        if owners.capacity() > 4 * owners.len().max(1) {
            owners.shrink_to_fit();
        }
        let facts = Arc::downgrade(&stamp.facts);
        if !owners.iter().any(|seen| seen.facts.ptr_eq(&facts)) {
            owners.push(EnvironmentOwner {
                facts,
                _lease: lease.clone(),
            });
        }
    }
    /// Reclaim optional local facts before the host refuses its existing
    /// request allowance. Durable receipts and original issuance stay intact.
    /// Active callers retain their own leases until they finish.
    pub fn evict_facts(&self) {
        let mut count = self.3.evict_ready();
        let owners = std::mem::take(&mut *self.4.lock().unwrap());
        for owner in owners {
            if let Some(facts) = owner.facts.upgrade() {
                count += facts.reads.evict_ready()
                    + facts.execution.evict_ready()
                    + facts.replays.evict_ready();
            }
        }
        hirsel_intern::work::count_by(hirsel_intern::work::Counter::VerifierFactEvictions, count);
        hirsel_intern::timings::count_by(
            hirsel_intern::timings::Count::VerifierFactEvictions,
            count,
        );
    }
    fn retain_fact(&self, bytes: u64) -> Result<Option<Arc<dyn Send + Sync>>, FactRefusal> {
        match &self.0 {
            Backend::Transport(_, rows, _) | Backend::Hosted(rows, ..) => rows.retain_fact(bytes),
            // The in-memory verifier is already an explicit unbounded owner.
            Backend::Memory(..) => Ok(Some(Arc::new(()))),
            Backend::Directory(_) | Backend::Registered(..) => Ok(None),
        }
    }

    fn retain_receipt(&self, receipt: Receipt) -> Result<AcceptedArtifact, crate::FactError> {
        let bytes = (size_of::<OriginalReceipt>() + 4 * size_of::<usize>()) as u64
            + (receipt.reads.capacity() * size_of::<VRead>()) as u64
            + crate::input::read_payload_bytes(&receipt.reads).ok_or(crate::FactError::Refused)?;
        let lease = self
            .retain_fact(bytes)
            .map_err(|_| crate::FactError::Admission)?;
        Ok(AcceptedArtifact::new(receipt, lease))
    }
    fn restore_receipt(
        &self,
        artifact: ContentHash,
        policy: ContentHash,
        action: ContentHash,
    ) -> Result<AcceptedArtifact, crate::FactError> {
        let receipt: Receipt = self
            .load(original_receipt_key(action), DOMAIN)
            .ok_or(crate::FactError::Refused)?;
        if receipt.artifact != artifact
            || receipt.policy != policy
            || receipt.action != action
            || receipt.action != receipt_action(artifact, policy, &receipt.reads)
        {
            return Err(crate::FactError::Refused);
        }
        self.retain_receipt(receipt)
    }

    fn read_holds(
        &self,
        types: &Arc<TypeInterner>,
        read: &VRead,
        signatures: crate::SignatureInputs<'_>,
    ) -> Result<bool, crate::FactError> {
        let Some(stamp) = signatures.stamp() else {
            return Ok(crate::input::read_holds_for_types(
                types,
                read,
                signatures.lookup(),
            ));
        };
        if !Arc::ptr_eq(&stamp.types, types) {
            return Ok(false);
        }
        let bytes = (4 * size_of::<VRead>()
            + size_of::<crate::views::EnvironmentFacts>()
            + 4 * size_of::<EnvironmentOwner>()
            + 512) as u64
            + 4 * crate::input::read_payload_bytes(std::slice::from_ref(read))
                .ok_or(crate::FactError::Admission)?;
        let holds = stamp.facts.reads.get_or_try_init(
            read.clone(),
            || {
                self.retain_fact(bytes)
                    .map_err(|_| crate::FactError::Admission)
                    .map(|lease| lease.unwrap_or_else(|| Arc::new(())))
            },
            || {
                let observed = crate::input::observe_read(Some(types), read, signatures.lookup());
                hirsel_intern::work::count(hirsel_intern::work::Counter::SealedReadComparisons);
                hirsel_intern::timings::count(hirsel_intern::timings::Count::SealedReadComparisons);
                Ok(Arc::new(crate::input::compare_read(
                    read,
                    observed.as_ref(),
                )))
            },
            Result::is_ok,
        )?;
        if let Some(lease) = self
            .retain_fact((4 * size_of::<EnvironmentOwner>() + 128) as u64)
            .map_err(|_| crate::FactError::Admission)?
        {
            self.remember(stamp, &lease);
        }
        Ok(*holds)
    }
    pub(crate) fn publication_batch(&self) -> Option<hirsel_artifact_store::Batch> {
        match &self.0 {
            Backend::Directory(store) => Some(store.batch()),
            Backend::Transport(selected, _, _) | Backend::Registered(selected, _) => {
                selected.publication_batch()
            }
            Backend::Hosted(rows, ..) => rows.publication_batch(),
            Backend::Memory(..) => None,
        }
    }
    /// Join an admitted compiler operation's bounded cache publication epoch.
    /// The guard supplies no verification authority. The caller joins its
    /// producers before finishing it; ordinary bytes cannot seed V's rows.
    pub fn publication_request(&self) -> Option<hirsel_artifact_store::Request> {
        match &self.0 {
            Backend::Directory(store) => Some(store.request()),
            Backend::Transport(selected, _, _) | Backend::Registered(selected, _) => {
                selected.publication_request()
            }
            Backend::Hosted(rows, ..) => rows.publication_request(),
            _ => None,
        }
    }

    fn load<T: serde::de::DeserializeOwned>(&self, key: ContentHash, domain: Domain) -> Option<T> {
        self.load_record(key, domain).map(|(_, value)| value)
    }
    /// The reference comes from this verifier's own association and the same
    /// checked bytes that are decoded below, never from producer metadata.
    fn load_record<T: serde::de::DeserializeOwned>(
        &self,
        key: ContentHash,
        domain: Domain,
    ) -> Option<(hirsel_artifact_store::Reference, T)> {
        let (reference, bytes) = self.row_content(key, domain)?;
        let value = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit(bytes.len() as u64)
            .reject_trailing_bytes()
            .deserialize(&bytes)
            .ok()?;
        Some((reference, value))
    }
    fn row_content(
        &self,
        key: ContentHash,
        domain: Domain,
    ) -> Option<(
        hirsel_artifact_store::Reference,
        hirsel_artifact_store::Bytes,
    )> {
        let (reference, bytes) = match &self.0 {
            Backend::Memory(rows, _) => rows
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&key)
                .cloned()?,
            Backend::Directory(store) => {
                let opened = store.lookup_open(Address(key))?;
                let association = opened.association;
                if association.action != Action(key) || association.reference.domain != domain {
                    return None;
                }
                let bytes = opened.get()?;
                store.touch(Address(key), association.reference);
                (association.reference, bytes)
            }
            Backend::Hosted(rows, contents, _) => {
                let reference = rows.row(key)?;
                (reference, contents.get_content(reference).ok()?)
            }
            Backend::Registered(local, installed) => {
                if let Some(value) = local.row_content(key, domain) {
                    return Some(value);
                }
                let reference = installed.row(key)?;
                (
                    reference,
                    hirsel_artifact_store::Contents::get_content(&**installed, reference).ok()?,
                )
            }
            Backend::Transport(selected, rows, contents) => {
                if let Some(reference) = rows.row(key) {
                    (reference, contents.request.get_content(reference).ok()?)
                } else {
                    let (reference, bytes) = selected.row_content(key, domain)?;
                    if contents.request.put_content(domain, &bytes)? != reference {
                        return None;
                    }
                    rows.set_row(key, reference);
                    (reference, bytes)
                }
            }
        };
        if reference.domain != domain {
            return None;
        }
        Some((reference, bytes))
    }
    fn publish<T: Serialize>(&self, key: ContentHash, domain: Domain, receipt: &T) {
        let Ok(bytes) = bincode::serialize(receipt) else {
            return;
        };
        if bytes.len() > hirsel_artifact_store::MAX_RECORD {
            return;
        }
        self.publish_bytes(key, domain, &bytes);
    }
    fn publish_bytes(&self, key: ContentHash, domain: Domain, bytes: &[u8]) {
        let Some(content) = domain.identify(bytes) else {
            return;
        };
        self.publish_identified(key, content);
    }
    fn publish_identified(
        &self,
        key: ContentHash,
        content: hirsel_artifact_store::IdentifiedContent<'_>,
    ) {
        match &self.0 {
            Backend::Memory(rows, contents) => {
                use hirsel_artifact_store::Contents;
                let Some(reference) = contents.put_identified_content(content) else {
                    return;
                };
                let Ok(bytes) = contents.get_content(reference) else {
                    return;
                };
                rows.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(key, (reference, bytes));
            }
            Backend::Directory(store) => {
                let _batch = store.batch();
                if let Ok(reference) = store.put_identified(content) {
                    let _ = store.associate(Address(key), Action(key), reference);
                }
            }
            Backend::Hosted(rows, contents, _) => {
                if let Some(reference) = contents.put_identified_content(content) {
                    rows.set_row(key, reference);
                }
            }
            Backend::Registered(local, _) => local.publish_identified(key, content),
            Backend::Transport(selected, rows, contents) => {
                let reference = content.reference();
                if contents.request.put_identified_content(content) != Some(reference) {
                    return;
                }
                rows.set_row(key, reference);
                selected.publish_identified(key, content);
            }
        }
    }
    /// Only V publishes terminal successful signature checks. A local hit
    /// independently observes the exact view digest and every contextual read.
    pub(crate) fn view_verdict(
        &self,
        types: &TypeInterner,
        definition: hirsel_syntax::DefId,
        input: crate::ViewInput<'_>,
        signatures: &dyn SignatureLookup,
        policy: ContentHash,
    ) -> Result<Arc<crate::ViewVerdict>, crate::ViewRefusal> {
        if !input.for_types(types) {
            return Ok(Arc::new(crate::ViewVerdict {
                error: Some(crate::Violation {
                    body: None,
                    block: None,
                    instruction: None,
                    kind: crate::ViolationKind::InvalidSignature(definition.path().clone()),
                }),
                reads: vec![],
                retention: Default::default(),
            }));
        }
        let digest = input.digest();
        let key = view_key(digest, policy);
        if let Some(receipt) = self.load::<ViewReceipt>(key, VIEW_DOMAIN)
            && receipt.digest == digest
            && receipt.policy == policy
            && receipt
                .reads
                .iter()
                .all(|read| crate::input::read_holds_for_types(types, read, signatures))
        {
            return Ok(Arc::new(crate::ViewVerdict {
                error: None,
                reads: receipt.reads,
                retention: Default::default(),
            }));
        }
        let verdict = crate::check_view(types, definition, &*input.signature()?, signatures)?;
        if verdict.error.is_none() {
            self.publish(
                key,
                VIEW_DOMAIN,
                &ViewReceipt {
                    digest,
                    policy,
                    reads: verdict.reads.clone(),
                },
            );
        }
        Ok(Arc::new(verdict))
    }
    fn contents(&self) -> Arc<dyn hirsel_artifact_store::Contents> {
        match &self.0 {
            Backend::Memory(_, contents) => contents.clone(),
            Backend::Directory(store) => Arc::new(store.clone()),
            Backend::Hosted(_, contents, _) => contents.clone(),
            Backend::Transport(_, _, contents) => contents.clone(),
            Backend::Registered(local, installed) => Arc::new(installation::Contents {
                local: local.contents(),
                installed: installed.clone(),
            }),
        }
    }
    fn records(&self, types: Arc<TypeInterner>) -> Arc<hirsel_product_codec::TypeRecords> {
        // Hosted storage bounds bytes only; a cached codec would keep its type
        // maps and interner alive beyond the host's bound and revision.
        if matches!(
            &self.0,
            Backend::Hosted(..) | Backend::Transport(..) | Backend::Registered(..)
        ) {
            return hirsel_product_codec::TypeRecords::new(types, self.contents());
        }
        let mut current = self.1.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(records) = &*current
            && Arc::ptr_eq(&records.types, &types)
        {
            return records.clone();
        }
        let records = hirsel_product_codec::TypeRecords::new(types, self.contents());
        *current = Some(records.clone());
        records
    }
    fn retain_artifact(&self, artifact: &Artifact<'_>, policy: ContentHash) {
        let _batch = self.publication_batch();
        let Some(identity) = artifact.identity else {
            return;
        };
        if self
            .load::<ReplayRecord>(replay_key(identity, policy), REPLAY_DOMAIN)
            .is_some_and(|record| record.artifact == identity && record.policy == policy)
        {
            return;
        }
        let records = self.records(artifact.types.clone());
        let Some(body) = records
            .encode_tir_view(artifact.definition, |anchor| {
                artifact.owners.get(&anchor).cloned()
            })
            .and_then(|bytes| {
                self.contents()
                    .put_content(hirsel_product_codec::CheckedTir::DOMAIN, &bytes)
            })
        else {
            return;
        };
        let Some(bytes) = records.encode_record(&artifact.overlay, |anchor| {
            artifact.owners.get(&anchor).cloned()
        }) else {
            return;
        };
        self.publish(
            replay_key(identity, policy),
            REPLAY_DOMAIN,
            &ReplayRecord {
                artifact: identity,
                policy,
                body,
                bytes,
            },
        );
    }
    fn replay_rows(
        &self,
        record: &ReplayRecord,
        types: Arc<TypeInterner>,
        mut anchor: impl FnMut(&DefPath, u32) -> Option<Anchor>,
        owner: Arc<dyn Send + Sync>,
    ) -> Option<ReplayRows> {
        let records = self.records(types);
        let mut owners = std::collections::BTreeMap::new();
        let mut bind = |path: &DefPath, occurrence| {
            let site = anchor(path, occurrence)?;
            owners.insert(site, (path.clone(), occurrence));
            Some(site)
        };
        let archive = hirsel_product_codec::CheckedTir::open(
            self.contents()
                .get_content(record.body)
                .ok()?
                .retain_owner(owner),
            record.body,
            records.clone(),
            &mut bind,
        )?;
        let overlay = records.decode_record(&record.bytes, &mut bind)?;
        Some(ReplayRows {
            archive,
            overlay,
            owners,
        })
    }
    /// Read only replay data associated by this verifier after terminal V.
    /// Recompute identity after portable type/anchor decoding before exposing it.
    fn freeze_replay(
        &self,
        accepted: &AcceptedArtifact,
        types: Arc<TypeInterner>,
        anchor: &mut impl FnMut(&DefPath, u32) -> Option<Anchor>,
    ) -> Result<(ReplayKey, ReplayRecord), crate::FactError> {
        let record: ReplayRecord = self
            .load(
                replay_key(accepted.artifact, accepted.policy),
                REPLAY_DOMAIN,
            )
            .ok_or(crate::FactError::Refused)?;
        if record.artifact != accepted.artifact || record.policy != accepted.policy {
            return Err(crate::FactError::Refused);
        }
        let body = self
            .contents()
            .get_content(record.body)
            .map_err(|_| crate::FactError::Refused)?;
        let mut bindings = std::collections::BTreeMap::new();
        for (bytes, reference) in [
            (body, record.body),
            (
                hirsel_artifact_store::Bytes::from(record.bytes.clone()),
                REPLAY_DOMAIN
                    .reference(&record.bytes)
                    .ok_or(crate::FactError::Refused)?,
            ),
        ] {
            let envelope = hirsel_product_codec::IndexedEnvelope::open(bytes, reference)
                .ok_or(crate::FactError::Refused)?;
            for n in 0..envelope.anchor_count() {
                let (path, occurrence) = envelope
                    .anchor(n)
                    .ok_or(crate::FactError::Refused)?
                    .to_owned();
                bindings
                    .entry((hirsel_syntax::DefId::intern(&path), occurrence))
                    .or_insert_with(|| anchor(&path, occurrence));
            }
        }
        Ok((
            ReplayKey {
                artifact: accepted.clone(),
                types: InternerKey(types),
                bindings: bindings
                    .into_iter()
                    .map(|((path, occurrence), site)| (path, occurrence, site))
                    .collect(),
                body: record.body,
                overlay: ContentHash::of_bytes(&record.bytes),
            },
            record,
        ))
    }
    #[cfg(test)]
    fn replay_artifact(
        &self,
        accepted: &AcceptedArtifact,
        types: Arc<TypeInterner>,
        mut anchor: impl FnMut(&DefPath, u32) -> Option<Anchor>,
    ) -> Result<Arc<ReplayArtifact>, crate::FactError> {
        let (key, record) = self.freeze_replay(accepted, types.clone(), &mut anchor)?;
        self.decoded_replay(key, &record)
    }
    fn decoded_replay(
        &self,
        key: ReplayKey,
        record: &ReplayRecord,
    ) -> Result<Arc<ReplayArtifact>, crate::FactError> {
        let bytes = key.bytes().ok_or(crate::FactError::Admission)?;
        self.3.get_or_try_init(
            key.clone(),
            || {
                self.retain_fact(bytes)
                    .map_err(|_| crate::FactError::Admission)
                    .map(|lease| lease.unwrap_or_else(|| Arc::new(())))
            },
            || self.compute_replay(&key, record),
            |result| result.is_ok() || matches!(result, Err(crate::FactError::Refused)),
        )
    }
    fn compute_replay(
        &self,
        key: &ReplayKey,
        record: &ReplayRecord,
    ) -> Result<Arc<ReplayArtifact>, crate::FactError> {
        if key.bindings.iter().any(|(_, _, site)| site.is_none()) {
            hirsel_intern::work::count(hirsel_intern::work::Counter::ReplayArtifactDecodes);
            hirsel_intern::timings::count(hirsel_intern::timings::Count::ReplayArtifactDecodes);
            return Err(crate::FactError::Refused);
        }
        self.compute_replay_inner(key, record)?
            .ok_or(crate::FactError::Unavailable)
    }
    fn compute_replay_inner(
        &self,
        key: &ReplayKey,
        record: &ReplayRecord,
    ) -> Result<Option<Arc<ReplayArtifact>>, crate::FactError> {
        let accepted = &key.artifact;
        let types = key.types.0.clone();
        let mut anchor = |path: &DefPath, occurrence| key.binding(path, occurrence);
        let decode = || -> Option<ReplayArtifact> {
            hirsel_intern::work::count(hirsel_intern::work::Counter::ReplayArtifactDecodes);
            hirsel_intern::timings::count(hirsel_intern::timings::Count::ReplayArtifactDecodes);
            let ReplayRows {
                archive,
                overlay,
                owners,
            } = self.replay_rows(record, types.clone(), &mut anchor, Arc::new(()))?;
            let definition = hirsel_product_codec::TirView::Archived(&archive);
            if overlay
                .as_ref()
                .is_some_and(|s| Some(s.definition) != definition.header().map(|h| h.path))
            {
                return None;
            }
            // Decode already bound every anchor and type row. Identify that exact
            // immutable closure once, including its overlay, before retaining it.
            let identity = match &overlay {
                Some(_) => hirsel_product_codec::canonical_bytes(
                    types.clone(),
                    &(&definition, &overlay),
                    |anchor| owners.get(&anchor).cloned(),
                )
                .map(|bytes| {
                    ContentHash::of_parts("hirsel.validate.private-artifact.v2", [bytes.as_slice()])
                }),
                None => hirsel_product_codec::canonical_bytes(types, &definition, |anchor| {
                    owners.get(&anchor).cloned()
                })
                .map(|bytes| {
                    ContentHash::of_parts("hirsel.validate.artifact.v1", [bytes.as_slice()])
                }),
            };
            if identity != Some(accepted.artifact) {
                return None;
            }
            Some(ReplayArtifact {
                archive: archive.frozen_bindings()?,
                overlay,
                identity: accepted.artifact,
                policy: accepted.policy,
                owners,
                _lease: None,
            })
        };
        let Some(mut replay) = decode() else {
            return Ok(None);
        };
        let metadata = (size_of::<ReplayArtifact>() + 256) as u64
            + (replay.owners.len() * (3 * size_of::<(Anchor, (DefPath, u32))>() + 64)) as u64;
        let lease = match &self.0 {
            Backend::Memory(..) => Some(Arc::new(()) as Arc<dyn Send + Sync>),
            Backend::Transport(_, rows, _) | Backend::Hosted(rows, ..) => rows
                .retain_replay(&replay.archive, replay.overlay.as_ref(), metadata)
                .map_err(|_| crate::FactError::Admission)?,
            Backend::Directory(_) | Backend::Registered(..) => None,
        };
        replay._lease = lease;
        Ok(Some(Arc::new(replay)))
    }
    /// Keep the original local issuance separate from its refreshed context.
    /// Supplying an old capability never rolls a successful association back.
    pub fn revalidate_execution(
        &self,
        original: &ExecutionReceipt,
        types: Arc<TypeInterner>,
        anchor: impl FnMut(&DefPath, u32) -> Option<Anchor>,
        environment: &dyn ReplayEnvironment,
    ) -> Result<Option<ExecutionReceipt>, crate::FactError> {
        let Some(subject) = original.identity() else {
            return Ok(None);
        };
        match self.load::<ExecutionRecord>(execution_key(subject), EXECUTION_DOMAIN) {
            Some(record)
                if record.subject == subject
                    && record.artifacts.iter().copied().eq(original
                        .artifacts
                        .iter()
                        .map(|a| (a.artifact, a.policy, a.receipt))) => {}
            Some(_) => return Ok(None),
            None => self.publish_execution(subject, original),
        }
        self.execution_revalidated_in(subject, Some(original), types, anchor, environment)
    }
    /// Refresh only a local association, running V here against independently
    /// supplied current views. A caller cannot refresh it with cloned evidence.
    pub fn execution_revalidated(
        &self,
        subject: ContentHash,
        types: Arc<TypeInterner>,
        anchor: impl FnMut(&DefPath, u32) -> Option<Anchor>,
        environment: &dyn ReplayEnvironment,
    ) -> Result<Option<ExecutionReceipt>, crate::FactError> {
        self.execution_revalidated_in(subject, None, types, anchor, environment)
    }
    fn execution_revalidated_in(
        &self,
        subject: ContentHash,
        original: Option<&ExecutionReceipt>,
        types: Arc<TypeInterner>,
        mut anchor: impl FnMut(&DefPath, u32) -> Option<Anchor>,
        environment: &dyn ReplayEnvironment,
    ) -> Result<Option<ExecutionReceipt>, crate::FactError> {
        use crate::FactError;
        hirsel_intern::work::count(hirsel_intern::work::Counter::ExecutionValidationRequests);
        hirsel_intern::timings::count(hirsel_intern::timings::Count::ExecutionValidationRequests);
        if original.is_none()
            && self
                .row_content(execution_key(subject), EXECUTION_DOMAIN)
                .is_none()
        {
            // A refused adoption leaves the miss below; V's full revalidation
            // checks an adopted closure exactly as a current one.
            let _ = self.adopt_execution(subject, &types, environment);
        }
        let result = (|| {
            let record: ExecutionRecord = self
                .load(execution_key(subject), EXECUTION_DOMAIN)
                .or_else(|| {
                    original.map(|receipt| ExecutionRecord {
                        subject,
                        contexts: receipt.contexts.to_vec(),
                        artifacts: receipt
                            .artifacts
                            .iter()
                            .map(|a| (a.artifact, a.policy, a.receipt))
                            .collect(),
                    })
                })
                .ok_or(FactError::Refused)?;
            if record.subject != subject
                || record.artifacts.is_empty()
                || record.contexts.is_empty()
                || !record.contexts.windows(2).all(|w| w[0].0 < w[1].0)
            {
                return Err(FactError::Refused);
            }
            let contexts = record
                .contexts
                .iter()
                .map(|(package, _)| {
                    Ok((
                        package.clone(),
                        environment
                            .base_digest(package)
                            .map_err(FactError::View)?
                            .ok_or(FactError::Refused)?,
                    ))
                })
                .collect::<Result<Vec<_>, FactError>>()?;
            let changed = contexts != record.contexts;
            let mut checked = Vec::new();
            for (package, digest) in &contexts {
                if let Some(input) = environment.checked(package).map_err(FactError::View)? {
                    if !Arc::ptr_eq(&input.stamp().types, &types) || input.input_digest() != *digest
                    {
                        return Err(FactError::Refused);
                    }
                    checked.push((package, input));
                }
            }
            // All actual bindings, including absent ones, are observed before a
            // claim. The callback is never consulted by the producer afterwards.
            let mut source_bindings = std::collections::BTreeMap::new();
            let mut frozen_anchor = |path: &DefPath, occurrence| {
                *source_bindings
                    .entry((hirsel_syntax::DefId::intern(path), occurrence))
                    .or_insert_with(|| anchor(path, occurrence))
            };
            let mut keys = Vec::new();
            let mut replayed = Vec::new();
            let mut overlays = Vec::new();
            for &(artifact, policy, receipt) in &record.artifacts {
                let accepted = match original.and_then(|original| {
                    original
                        .artifacts
                        .iter()
                        .find(|a| (a.artifact, a.policy, a.receipt) == (artifact, policy, receipt))
                }) {
                    Some(held) => held.clone(),
                    None => self.restore_receipt(artifact, policy, receipt)?,
                };
                hirsel_intern::work::count(hirsel_intern::work::Counter::ExecutionReplayRequests);
                hirsel_intern::timings::count(
                    hirsel_intern::timings::Count::ExecutionReplayRequests,
                );
                let (key, replay_record) =
                    self.freeze_replay(&accepted, types.clone(), &mut frozen_anchor)?;
                let replay = self.decoded_replay(key.clone(), &replay_record);
                let overlay = if let Ok(replay) = &replay {
                    let package = replay
                        .archive
                        .header()
                        .ok_or(FactError::Refused)?
                        .path
                        .path()
                        .0
                        .package
                        .clone();
                    let base = checked
                        .iter()
                        .find(|(p, _)| **p == package)
                        .map(|(_, input)| &**input);
                    match (base, &replay.overlay) {
                        (Some(base), Some(overlay)) => {
                            let views = environment
                                .checked_overlay(&package, overlay.clone())
                                .map_err(FactError::View)?
                                .ok_or(FactError::Refused)?;
                            if views.origin_input() != base.input_digest()
                                || !Arc::ptr_eq(
                                    &crate::CheckedEnvironment::stamp(&*views).types,
                                    &types,
                                )
                                || !views.replacement(overlay.definition).is_some_and(|view| {
                                    view.digest() == crate::digest(&types, overlay)
                                })
                            {
                                return Err(FactError::Refused);
                            }
                            Some(views)
                        }
                        _ => None,
                    }
                } else {
                    None
                };
                keys.push(key);
                replayed.push(replay);
                overlays.push(overlay);
            }
            let key = ExecutionKey {
                issuer: IssuerKey(self.2.clone()),
                subject,
                contexts: contexts.clone(),
                artifacts: keys,
                overlays: overlays
                    .iter()
                    .map(|overlay| {
                        overlay
                            .as_ref()
                            .map(|views| crate::CheckedEnvironment::input_digest(&**views))
                    })
                    .collect(),
            };
            let compute = || {
                let mut artifacts = Vec::new();
                let mut covered = std::collections::BTreeSet::new();
                for ((replay_key, replay), overlay) in
                    key.artifacts.iter().zip(&replayed).zip(&overlays)
                {
                    let accepted = &replay_key.artifact;
                    if artifacts.contains(accepted) {
                        artifacts.push(accepted.clone());
                        continue;
                    }
                    let replay = match replay {
                        Ok(replay) => replay,
                        Err(error) => {
                            hirsel_intern::work::count(
                                hirsel_intern::work::Counter::ExecutionReplayComputations,
                            );
                            hirsel_intern::timings::count(
                                hirsel_intern::timings::Count::ExecutionReplayComputations,
                            );
                            return Err(error.clone());
                        }
                    };
                    let package = replay
                        .archive
                        .header()
                        .ok_or(FactError::Refused)?
                        .path
                        .path()
                        .0
                        .package
                        .clone();
                    if !contexts.iter().any(|(p, _)| *p == package) {
                        return Err(FactError::Refused);
                    }
                    covered.insert(package.clone());
                    let base = checked
                        .iter()
                        .find(|(p, _)| **p == package)
                        .map(|(_, input)| &**input);
                    let raw;
                    let inputs = match (&replay.overlay, base, overlay) {
                        (_, _, Some(overlay)) => crate::SignatureInputs::Checked(&**overlay),
                        (None, Some(base), _) => crate::SignatureInputs::Checked(base),
                        _ => {
                            raw = environment
                                .views(&package, replay.overlay.clone())
                                .map_err(FactError::View)?
                                .ok_or(FactError::Refused)?;
                            crate::SignatureInputs::Raw(&*raw)
                        }
                    };
                    let check = || {
                        hirsel_intern::work::count(
                            hirsel_intern::work::Counter::ExecutionReplayComputations,
                        );
                        hirsel_intern::timings::count(
                            hirsel_intern::timings::Count::ExecutionReplayComputations,
                        );
                        let original = &accepted.original.receipt;
                        if original.artifact != accepted.artifact
                            || original.policy != accepted.policy
                            || original.action != accepted.receipt
                            || original.action
                                != receipt_action(
                                    original.artifact,
                                    original.policy,
                                    &original.reads,
                                )
                        {
                            return Err(FactError::Refused);
                        }
                        for read in &original.reads {
                            if !self.read_holds(&types, read, inputs)? {
                                return Err(FactError::Refused);
                            }
                        }
                        let exact = Artifact {
                            definition: hirsel_product_codec::TirView::Archived(&replay.archive),
                            types: types.clone(),
                            identity: Some(replay.identity),
                            owners: replay.owners.clone(),
                            overlay: replay.overlay.clone(),
                        };
                        let (report, _, refreshed) = validate_with_receipt_in(
                            &exact,
                            inputs,
                            replay.policy,
                            Some(self),
                            &mut vec![],
                        )
                        .map_err(FactError::View)?;
                        if !report.violations.is_empty()
                            || !refreshed.as_ref().is_some_and(|current| {
                                current.artifact == accepted.artifact
                                    && current.policy == accepted.policy
                            })
                        {
                            return Err(FactError::Refused);
                        }
                        Ok(Arc::new(()))
                    };
                    if let Some(stamp) = inputs.stamp() {
                        // A receipt's issuance is authenticated above. Compatibility
                        // depends on the artifact, its exact source bindings and
                        // this immutable V input, not on the execution that used it.
                        let replay_key = ReplayCheckKey {
                            issuer: key.issuer.clone(),
                            replay: replay_key.clone(),
                        };
                        let bytes = replay_key.replay.bytes().ok_or(FactError::Admission)? + 512;
                        stamp.facts.replays.get_or_try_init(
                            replay_key,
                            || {
                                self.retain_fact(bytes)
                                    .map_err(|_| FactError::Admission)
                                    .map(|lease| lease.unwrap_or_else(|| Arc::new(())))
                            },
                            check,
                            |result| result.is_ok() || matches!(result, Err(FactError::Refused)),
                        )?;
                        if let Some(lease) = self
                            .retain_fact((4 * size_of::<EnvironmentOwner>() + 128) as u64)
                            .map_err(|_| FactError::Admission)?
                        {
                            self.remember(stamp, &lease);
                        }
                    } else {
                        check()?;
                    }
                    artifacts.push(accepted.clone());
                }
                if contexts.iter().any(|(p, _)| !covered.contains(p)) {
                    return Err(FactError::Refused);
                }
                for (package, digest) in &contexts {
                    if environment.base_digest(package).map_err(FactError::View)? != Some(*digest) {
                        return Err(FactError::Refused);
                    }
                }
                let receipt = ExecutionReceipt {
                    contexts: contexts.clone().into(),
                    artifacts: artifacts.into(),
                };
                if changed {
                    self.publish_execution(subject, &receipt);
                }
                Ok(Arc::new(receipt))
            };
            // Checked complete bases own their exact terminal results. Raw
            // adapters have no immutable identity and cannot claim a fact.
            let receipt = if checked.len() == contexts.len() {
                let stamp = checked[0].1.stamp();
                let bytes = key.bytes().ok_or(FactError::Admission)?;
                let receipt = stamp.facts.execution.get_or_try_init(
                    key.clone(),
                    || {
                        self.retain_fact(bytes)
                            .map_err(|_| FactError::Admission)
                            .map(|lease| lease.unwrap_or_else(|| Arc::new(())))
                    },
                    compute,
                    |result| result.is_ok() || matches!(result, Err(FactError::Refused)),
                )?;
                // The weak registry charges its empty environment and makes
                // optional eviction possible without a cache/environment cycle.
                if let Some(lease) = self
                    .retain_fact((size_of::<EnvironmentOwner>() * 4 + 128) as u64)
                    .map_err(|_| FactError::Admission)?
                {
                    self.remember(stamp, &lease);
                }
                receipt
            } else {
                compute()?
            };
            Ok((*receipt).clone())
        })();
        match result {
            Ok(receipt) => Ok(Some(receipt)),
            Err(error) => {
                hirsel_intern::work::count(
                    hirsel_intern::work::Counter::ExecutionValidationRefusals,
                );
                hirsel_intern::timings::count(
                    hirsel_intern::timings::Count::ExecutionValidationRefusals,
                );
                match error {
                    FactError::Refused => Ok(None),
                    error => Err(error),
                }
            }
        }
    }
    /// receipt cannot be constructed or decoded by an ordinary producer.
    pub fn publish_execution(&self, subject: ContentHash, receipt: &ExecutionReceipt) {
        self.publish(
            execution_key(subject),
            EXECUTION_DOMAIN,
            &ExecutionRecord {
                subject,
                contexts: receipt.contexts.to_vec(),
                artifacts: receipt
                    .artifacts
                    .iter()
                    .map(|a| (a.artifact, a.policy, a.receipt))
                    .collect(),
            },
        );
    }
    /// Read only the verifier's local association and independently observe
    /// every base universe used by the completed execution. No producer record
    /// can seed this association by carrying a successful-looking read list.
    pub fn execution(
        &self,
        subject: ContentHash,
        mut current: impl FnMut(&PackageId) -> Option<ContentHash>,
    ) -> Option<ExecutionReceipt> {
        let record: ExecutionRecord = self.load(execution_key(subject), EXECUTION_DOMAIN)?;
        if record.subject != subject
            || record.artifacts.is_empty()
            || record.contexts.is_empty()
            || !record.contexts.windows(2).all(|w| w[0].0 < w[1].0)
            || !record
                .contexts
                .iter()
                .all(|(package, digest)| current(package) == Some(*digest))
        {
            return None;
        }
        Some(ExecutionReceipt {
            contexts: record.contexts.into(),
            artifacts: record
                .artifacts
                .into_iter()
                .map(|(artifact, policy, receipt)| {
                    self.restore_receipt(artifact, policy, receipt).ok()
                })
                .collect::<Option<Vec<_>>>()?
                .into(),
        })
    }
}

#[derive(Clone, Debug)]
struct IssuerKey(Arc<()>);
impl PartialEq for IssuerKey {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for IssuerKey {}
impl std::hash::Hash for IssuerKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::hash::Hash::hash(&Arc::as_ptr(&self.0), state);
    }
}
#[derive(Clone)]
struct InternerKey(Arc<TypeInterner>);
impl PartialEq for InternerKey {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for InternerKey {}
impl std::hash::Hash for InternerKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::hash::Hash::hash(&Arc::as_ptr(&self.0), state);
    }
}
impl std::fmt::Debug for InternerKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Interner({:p})", Arc::as_ptr(&self.0))
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
struct ReplayKey {
    artifact: AcceptedArtifact,
    types: InternerKey,
    bindings: Vec<(hirsel_syntax::DefId, u32, Option<Anchor>)>,
    body: hirsel_artifact_store::Reference,
    overlay: ContentHash,
}
impl std::hash::Hash for ReplayKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.artifact.hash(state);
        self.types.hash(state);
        self.bindings.hash(state);
        self.overlay.hash(state);
    }
}
impl ReplayKey {
    fn binding(&self, path: &DefPath, occurrence: u32) -> Option<Anchor> {
        let id = hirsel_syntax::DefId::intern(path);
        self.bindings
            .binary_search_by_key(&(id, occurrence), |(id, n, _)| (*id, *n))
            .ok()
            .and_then(|at| self.bindings[at].2)
    }
    fn bytes(&self) -> Option<u64> {
        (size_of::<Self>() as u64 + 512)
            .checked_add(size_of_val(self.bindings.as_slice()) as u64 * 4)
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub(crate) struct ReplayCheckKey {
    issuer: IssuerKey,
    replay: ReplayKey,
}
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub(crate) struct ExecutionKey {
    issuer: IssuerKey,
    subject: ContentHash,
    contexts: Vec<(PackageId, ContentHash)>,
    artifacts: Vec<ReplayKey>,
    overlays: Vec<Option<ContentHash>>,
}
impl ExecutionKey {
    fn bytes(&self) -> Option<u64> {
        let mut bytes = (size_of::<Self>() + 1024) as u64;
        for (package, _) in &self.contexts {
            bytes = bytes.checked_add(
                (size_of::<(PackageId, ContentHash)>()
                    + package.name.capacity()
                    + package.version.capacity()) as u64
                    * 4,
            )?;
        }
        for key in &self.artifacts {
            bytes = bytes.checked_add(key.bytes()?)?;
        }
        bytes.checked_add(size_of_val(self.overlays.as_slice()) as u64 * 4)
    }
}

/// Immutable independent signature input for replay. Implementations supply
/// views, never verdicts. Overlay bytes come only from the verifier's exact
/// locally retained artifact, and are independently checked again by V.
pub trait ReplayEnvironment {
    /// General adapters have no checked immutable owner and never share facts.
    fn checked(
        &self,
        _package: &PackageId,
    ) -> Result<Option<Box<dyn crate::CheckedEnvironment + '_>>, crate::ViewRefusal> {
        Ok(None)
    }
    fn base_digest(&self, package: &PackageId) -> Result<Option<ContentHash>, crate::ViewRefusal>;
    fn checked_overlay(
        &self,
        _package: &PackageId,
        _overlay: hirsel_types::SignatureRef,
    ) -> Result<Option<Arc<crate::PackageViews>>, crate::ViewRefusal> {
        Ok(None)
    }
    fn views(
        &self,
        package: &PackageId,
        overlay: Option<hirsel_types::SignatureRef>,
    ) -> Result<Option<Box<dyn SignatureLookup + '_>>, crate::ViewRefusal>;
}

const REPLAY_DOMAIN: Domain = Domain {
    kind: 0x5600_0004,
    schema: 3,
};
fn replay_key(artifact: ContentHash, policy: ContentHash) -> ContentHash {
    ContentHash::of_parts(
        "hirsel.validate.replay.v3",
        [artifact.0.as_slice(), policy.0.as_slice()],
    )
}
#[derive(Serialize, Deserialize)]
struct ReplayRecord {
    artifact: ContentHash,
    policy: ContentHash,
    body: hirsel_artifact_store::Reference,
    #[serde(
        serialize_with = "hirsel_product_codec::serialize_payload",
        deserialize_with = "hirsel_product_codec::deserialize_payload"
    )]
    bytes: Vec<u8>,
}
struct ReplayRows {
    archive: hirsel_product_codec::CheckedTir,
    overlay: Option<hirsel_types::SignatureRef>,
    owners: std::collections::BTreeMap<Anchor, (DefPath, u32)>,
}
/// Construction is verifier-private. These bytes alone are not acceptance.
struct ReplayArtifact {
    archive: hirsel_product_codec::CheckedTir,
    overlay: Option<hirsel_types::SignatureRef>,
    identity: ContentHash,
    policy: ContentHash,
    owners: std::collections::BTreeMap<Anchor, (DefPath, u32)>,
    _lease: Option<Arc<dyn Send + Sync>>,
}
const EXECUTION_DOMAIN: Domain = Domain {
    kind: 0x5600_0002,
    schema: 3,
};
fn execution_key(subject: ContentHash) -> ContentHash {
    ContentHash::of_parts("hirsel.validate.execution.v3", [subject.0.as_slice()])
}
/// A terminal successful V decision. There is intentionally no Deserialize or
/// producer constructor: only validate_with_receipt can mint this capability.
#[derive(Clone, Debug)]
pub struct AcceptedArtifact {
    artifact: ContentHash,
    policy: ContentHash,
    receipt: ContentHash,
    original: Arc<OriginalReceipt>,
}
struct OriginalReceipt {
    receipt: Receipt,
    _lease: Option<Arc<dyn Send + Sync>>,
}
impl std::fmt::Debug for OriginalReceipt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.receipt.fmt(f)
    }
}
impl AcceptedArtifact {
    fn new(receipt: Receipt, lease: Option<Arc<dyn Send + Sync>>) -> Self {
        Self {
            artifact: receipt.artifact,
            policy: receipt.policy,
            receipt: receipt.action,
            original: Arc::new(OriginalReceipt {
                receipt,
                _lease: lease,
            }),
        }
    }
}
impl PartialEq for AcceptedArtifact {
    fn eq(&self, other: &Self) -> bool {
        (self.artifact, self.policy, self.receipt) == (other.artifact, other.policy, other.receipt)
    }
}
impl Eq for AcceptedArtifact {}
impl std::hash::Hash for AcceptedArtifact {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::hash::Hash::hash(&(self.artifact, self.policy, self.receipt), state);
    }
}
/// Composed execution evidence holds original V observations, not source bodies.
/// Its complete independent base contexts guard private overlays and callees.
#[derive(Clone, Debug)]
pub struct ExecutionReceipt {
    contexts: Arc<[(PackageId, ContentHash)]>,
    artifacts: Arc<[AcceptedArtifact]>,
}
impl ExecutionReceipt {
    /// Content identity for a local execution association. This digest carries
    /// no authority; restoring the association still requires this verifier.
    pub fn identity(&self) -> Option<ContentHash> {
        let artifacts: Vec<_> = self
            .artifacts
            .iter()
            .map(|accepted| (accepted.artifact, accepted.policy, accepted.receipt))
            .collect();
        let bytes = bincode::serialize(&(self.contexts.as_ref(), artifacts)).ok()?;
        Some(ContentHash::of_parts(
            "hirsel.validate.execution-receipt.v2",
            [bytes.as_slice()],
        ))
    }
    /// Actual compact context/artifact ownership, without source body bytes.
    pub fn retained_bytes(&self) -> Option<u64> {
        let slots = (std::mem::size_of::<Self>() + 4 * std::mem::size_of::<usize>()) as u64;
        let originals = self.artifacts.iter().try_fold(0u64, |bytes, artifact| {
            bytes
                .checked_add(
                    (size_of::<OriginalReceipt>()
                        + 4 * size_of::<usize>()
                        + artifact.original.receipt.reads.capacity() * size_of::<VRead>())
                        as u64,
                )?
                .checked_add(crate::input::read_payload_bytes(
                    &artifact.original.receipt.reads,
                )?)
        })?;
        slots
            .checked_add(originals)?
            .checked_add(
                (self.contexts.len() as u64)
                    .checked_mul(std::mem::size_of::<(PackageId, ContentHash)>() as u64)?,
            )?
            .checked_add(bincode::serialized_size(self.contexts.as_ref()).ok()?)?
            .checked_add(
                (self.artifacts.len() as u64)
                    .checked_mul(std::mem::size_of::<AcceptedArtifact>() as u64)?,
            )
    }
    pub fn compose(
        contexts: &[(PackageId, ContentHash)],
        artifacts: &[AcceptedArtifact],
    ) -> Option<Self> {
        if artifacts.is_empty()
            || contexts.is_empty()
            || !contexts.windows(2).all(|w| w[0].0 < w[1].0)
        {
            return None;
        }
        Some(Self {
            contexts: contexts.to_vec().into(),
            artifacts: artifacts.to_vec().into(),
        })
    }
    pub fn contexts(&self) -> &[(PackageId, ContentHash)] {
        &self.contexts
    }
    pub fn artifacts(&self) -> &[AcceptedArtifact] {
        &self.artifacts
    }
}
#[derive(Serialize, Deserialize)]
struct ExecutionRecord {
    subject: ContentHash,
    contexts: Vec<(PackageId, ContentHash)>,
    artifacts: Vec<(ContentHash, ContentHash, ContentHash)>,
}

/// Reuse only this verifier's successful terminal record, after independently
/// observing its complete view/private/absence/universe/HeapRole inputs. The
/// returned boolean says whether full V executed. An unencodable artifact still
/// receives full V; it simply cannot acquire a portable receipt.
pub fn validate_cached(
    artifact: &Artifact<'_>,
    signatures: &dyn SignatureLookup,
    policy: ContentHash,
    cache: Option<&VerifierCache>,
    reads: &mut Vec<VRead>,
) -> Result<(Validation, bool), crate::ViewRefusal> {
    let (report, ran, _) = validate_with_receipt(artifact, signatures, policy, cache, reads)?;
    Ok((report, ran))
}
/// The same validation path, retaining the opaque evidence for composition
/// with dependent private/source artifacts after successful execution.
pub fn validate_with_receipt<'a>(
    artifact: &Artifact<'_>,
    signatures: impl Into<crate::SignatureInputs<'a>>,
    policy: ContentHash,
    cache: Option<&VerifierCache>,
    reads: &mut Vec<VRead>,
) -> Result<(Validation, bool, Option<AcceptedArtifact>), crate::ViewRefusal> {
    validate_with_receipt_in(artifact, signatures.into(), policy, cache, reads)
}
fn validate_with_receipt_in(
    artifact: &Artifact<'_>,
    signatures: crate::SignatureInputs<'_>,
    policy: ContentHash,
    cache: Option<&VerifierCache>,
    reads: &mut Vec<VRead>,
) -> Result<(Validation, bool, Option<AcceptedArtifact>), crate::ViewRefusal> {
    use hirsel_intern::timings::instructions::{Owner, scope};
    // Receipt lookup and read re-observation are exact reuse; full V below
    // opens its own execution scope.
    let _reuse = scope(Owner::VReuse);
    let accept = |receipt: Receipt| match cache {
        Some(cache) => {
            cache
                .retain_receipt(receipt)
                .map_err(|error| crate::ViewRefusal::Unreadable {
                    definition: artifact
                        .definition
                        .header()
                        .expect("identified artifact header")
                        .path,
                    cause: error.into(),
                })
        }
        None => Ok(AcceptedArtifact::new(receipt, None)),
    };
    let key = artifact.identity.map(|identity| {
        ContentHash::of_parts(
            "hirsel.validate.receipt.v3",
            [identity.0.as_slice(), policy.0.as_slice()],
        )
    });
    // A current miss may find V's own receipt in the previous generation, only
    // under V's frozen environment. It passes the same checks before adoption.
    let mut staged = None;
    if let (Some(cache), Some(key), Some(identity)) = (cache, key, artifact.identity)
        && let Some(receipt) = cache.load::<Receipt>(key, DOMAIN).or_else(|| {
            signatures.stamp()?;
            let mut staging = cache.staging()?;
            let (_, receipt) = staging.record::<Receipt>(key, DOMAIN).ok()?;
            staged = Some(staging);
            Some(receipt)
        })
        && receipt.artifact == identity
        && receipt.policy == policy
        && receipt.action == receipt_action(identity, policy, &receipt.reads)
    {
        let mut holds = true;
        for read in &receipt.reads {
            if !cache
                .read_holds(&artifact.types, read, signatures)
                .map_err(|error| crate::ViewRefusal::Unreadable {
                    definition: artifact
                        .definition
                        .header()
                        .expect("identified artifact header")
                        .path,
                    cause: error.into(),
                })?
            {
                holds = false;
                break;
            }
        }
        let adopted = match staged.take() {
            Some(mut staging) if holds => staging
                .original(identity, policy, receipt.action)
                .and_then(|()| cache.adopt(staging))
                .is_ok(),
            Some(_) => false,
            None => {
                if holds {
                    cache.publish(original_receipt_key(receipt.action), DOMAIN, &receipt);
                }
                holds
            }
        };
        if adopted {
            cache.retain_artifact(artifact, policy);
            let accepted = accept(receipt.clone())?;
            reads.extend(receipt.reads);
            return Ok((Validation::default(), false, Some(accepted)));
        }
    }
    let mut own = vec![];
    let report = crate::validate_rows(
        artifact.definition,
        &artifact.types,
        signatures.lookup(),
        &mut own,
    )?
    .unwrap_or_else(|| Validation {
        violations: vec![crate::Violation {
            body: None,
            block: None,
            instruction: None,
            kind: crate::ViolationKind::InvalidTypeGraph,
        }],
    });
    let _publish = scope(Owner::Store);
    if report.violations.is_empty()
        && let (Some(cache), Some(key), Some(identity)) = (cache, key, artifact.identity)
    {
        cache.retain_artifact(artifact, policy);
        let receipt = Receipt {
            action: receipt_action(identity, policy, &own),
            artifact: identity,
            policy,
            reads: own.clone(),
        };
        cache.publish(original_receipt_key(receipt.action), DOMAIN, &receipt);
        cache.publish(key, DOMAIN, &receipt);
    }
    let accepted = report
        .violations
        .is_empty()
        .then(|| {
            artifact.identity.map(|artifact| {
                accept(Receipt {
                    artifact,
                    policy,
                    action: receipt_action(artifact, policy, &own),
                    reads: own.clone(),
                })
            })
        })
        .flatten()
        .transpose()?;
    reads.extend(own);
    Ok((report, true, accepted))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::{Fixture, owner_sig, path, session};
    use hirsel_tir::{BodyId, Constant, Rvalue};
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "hirsel-verdict-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&root).unwrap();
            Self(root)
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn artifact(f: &Fixture) -> Artifact<'_> {
        Artifact::new(&f.definition, session(), |_| Some((path("main"), 0)))
    }

    #[test]
    fn replay_record_bulk_overlay_bytes_preserve_sequence_encoding_and_decode_limits() {
        #[derive(Serialize, Deserialize)]
        struct Original {
            artifact: ContentHash,
            policy: ContentHash,
            body: hirsel_artifact_store::Reference,
            bytes: Vec<u8>,
        }
        let fixture = Fixture::unit();
        let cache = VerifierCache::default();
        let body = cache
            .records(session())
            .encode_tir(&fixture.definition, |_| Some((path("main"), 0)))
            .unwrap();
        let body = cache
            .contents()
            .put_content(hirsel_product_codec::CheckedTir::DOMAIN, &body)
            .unwrap();
        let original = Original {
            artifact: ContentHash::of_bytes(b"artifact"),
            policy: ContentHash::of_bytes(b"policy"),
            body,
            bytes: (0..=255).cycle().take(4096).collect(),
        };
        let record = ReplayRecord {
            artifact: original.artifact,
            policy: original.policy,
            body: original.body,
            bytes: original.bytes.clone(),
        };
        let bytes = bincode::serialize(&original).unwrap();
        assert_eq!(bincode::serialize(&record).unwrap(), bytes);
        let limit = bytes.len() as u64;
        let options = || {
            bincode::DefaultOptions::new()
                .with_fixint_encoding()
                .with_limit(limit)
                .reject_trailing_bytes()
        };
        let decoded: ReplayRecord = options().deserialize(&bytes).unwrap();
        assert_eq!(decoded.bytes, original.bytes);
        assert_eq!(decoded.artifact, original.artifact);
        assert_eq!(decoded.policy, original.policy);
        assert!(
            options()
                .deserialize::<ReplayRecord>(&bytes[..bytes.len() - 1])
                .is_err()
        );
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(options().deserialize::<ReplayRecord>(&trailing).is_err());
        // An impossible payload length must fail before an allocation/copy.
        let mut invalid = bytes;
        let count_at = bincode::serialized_size(&(record.artifact, record.policy, record.body))
            .unwrap() as usize;
        invalid[count_at..count_at + 8].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(options().deserialize::<ReplayRecord>(&invalid).is_err());
    }

    #[test]
    fn verifier_type_content_joins_its_own_publication_batch() {
        let dir = Directory::new();
        let cache = VerifierCache::directory(dir.0.clone());
        let batch = cache.publication_batch().unwrap();
        let contents = cache.contents();
        let reader = Store::new(dir.0.clone());
        let mut references = vec![];
        for n in 0u32..8 {
            let reference = contents.put_content(VIEW_DOMAIN, &n.to_le_bytes()).unwrap();
            assert!(contents.get_content(reference).is_ok());
            assert!(
                reader.get(reference).is_none(),
                "type content escaped its batch"
            );
            references.push(reference);
        }
        batch.finish().unwrap();
        for (n, reference) in references.into_iter().enumerate() {
            assert_eq!(&*reader.get(reference).unwrap(), &(n as u32).to_le_bytes());
        }
    }
    fn check(f: &Fixture, cache: &VerifierCache, policy: ContentHash) -> (Validation, bool) {
        validate_cached(
            &artifact(f),
            &f.signatures,
            policy,
            Some(cache),
            &mut vec![],
        )
        .unwrap()
    }
    fn key(artifact: &Artifact<'_>, policy: ContentHash) -> ContentHash {
        ContentHash::of_parts(
            "hirsel.validate.receipt.v3",
            [artifact.identity.unwrap().0.as_slice(), policy.0.as_slice()],
        )
    }

    fn view(f: &Fixture, cache: &VerifierCache, policy: ContentHash) -> Arc<crate::ViewVerdict> {
        let definition = f.definition.path;
        let signature = f.signatures.get(definition).unwrap();
        cache
            .view_verdict(
                &session(),
                definition,
                crate::IdentifiedSignature::new(session(), Arc::new(signature.clone())).input(),
                &f.signatures,
                policy,
            )
            .unwrap()
    }

    #[test]
    fn cached_nested_body_and_private_type_mutations_require_full_v() {
        let mut f = crate::support::captured_generic_copy(true, false, false);
        let cache = VerifierCache::default();
        let policy = ContentHash::default();
        let (report, ran) = check(&f, &cache, policy);
        assert!(ran && report.violations.is_empty(), "{report:?}");
        assert!(!check(&f, &cache, policy).1);
        let original = f.definition.bodies[1].clone();
        let public = f.signatures.get(f.definition.path).unwrap().fingerprint;
        let hirsel_tir::Terminator::Return { value, .. } =
            &mut f.definition.bodies[1].blocks[0].terminator
        else {
            panic!()
        };
        value.place = hirsel_tir::PlaceId(u32::MAX);
        let (report, ran) = check(&f, &cache, policy);
        assert!(ran && !report.violations.is_empty());
        f.definition.bodies[1] = original;
        f.definition.bodies[1].result =
            session().intern(hirsel_types::Type::with_kind(hirsel_types::TypeKind::Error));
        assert_eq!(
            f.signatures.get(f.definition.path).unwrap().fingerprint,
            public
        );
        let (report, ran) = check(&f, &cache, policy);
        assert!(ran && !report.violations.is_empty());
    }

    #[test]
    fn hosted_verifier_retains_no_type_records_beyond_its_callers() {
        #[derive(Default)]
        struct Rows(
            hirsel_artifact_store::Memory,
            Mutex<HashMap<ContentHash, hirsel_artifact_store::Reference>>,
            std::sync::atomic::AtomicBool,
            Arc<std::sync::atomic::AtomicUsize>,
        );
        struct Lease(Arc<std::sync::atomic::AtomicUsize>);
        impl Drop for Lease {
            fn drop(&mut self) {
                self.0.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            }
        }
        impl hirsel_artifact_store::Contents for Rows {
            fn get_content(
                &self,
                reference: hirsel_artifact_store::Reference,
            ) -> Result<hirsel_artifact_store::Bytes, hirsel_artifact_store::ContentReadError>
            {
                self.0.get_content(reference)
            }
            fn put_content(
                &self,
                domain: Domain,
                bytes: &[u8],
            ) -> Option<hirsel_artifact_store::Reference> {
                self.0.put_content(domain, bytes)
            }
        }
        impl VerifierRows for Rows {
            fn retain_fact(&self, _: u64) -> Result<Option<Arc<dyn Send + Sync>>, FactRefusal> {
                if self.2.load(std::sync::atomic::Ordering::Relaxed) {
                    Err(FactRefusal)
                } else {
                    self.3.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    Ok(Some(Arc::new(Lease(self.3.clone()))))
                }
            }
            fn retain_replay(
                &self,
                _: &hirsel_product_codec::CheckedTir,
                _: Option<&hirsel_types::SignatureRef>,
                bytes: u64,
            ) -> Result<Option<Arc<dyn Send + Sync>>, FactRefusal> {
                self.retain_fact(bytes)
            }
            fn row(&self, key: ContentHash) -> Option<hirsel_artifact_store::Reference> {
                self.1.lock().unwrap().get(&key).copied()
            }
            fn set_row(&self, key: ContentHash, reference: hirsel_artifact_store::Reference) {
                self.1.lock().unwrap().insert(key, reference);
            }
        }
        let rows = Arc::new(Rows::default());
        let cache = VerifierCache::hosted(rows.clone());
        let types = session();
        let baseline = Arc::strong_count(&types);
        let records = cache.records(types.clone());
        assert!(Arc::strong_count(&types) > baseline);
        drop(records);
        assert_eq!(Arc::strong_count(&types), baseline, "no hosted codec cache");
        let directory = Directory::new();
        let cached = VerifierCache::directory(directory.0.clone());
        drop(cached.records(types.clone()));
        assert!(
            Arc::strong_count(&types) > baseline,
            "directory caches keep their codec"
        );
        let mut f = Fixture::unit();
        owner_sig(&mut f, &[]);
        f.body().blocks[0].span.anchor = Anchor::fresh();
        let site = f.definition.bodies[0].blocks[0].span.anchor;
        let (_, _, accepted) = validate_with_receipt(
            &artifact(&f),
            &f.signatures,
            ContentHash::default(),
            Some(&cache),
            &mut vec![],
        )
        .unwrap();
        let package = path("main").0.package;
        let original = ExecutionReceipt::compose(
            &[(package.clone(), ContentHash::of_bytes(b"old"))],
            &[accepted.unwrap()],
        )
        .unwrap();
        let table = crate::ViewTable::new(
            &session(),
            f.signatures.values().cloned().map(Arc::new),
            None,
            &mut crate::ValidatedSignatures::default(),
            None,
        )
        .unwrap();
        let environment = CheckedReplay {
            package: &package,
            views: &table,
        };
        rows.2.store(true, std::sync::atomic::Ordering::Relaxed);
        for _ in 0..2 {
            assert!(matches!(
                cache.revalidate_execution(&original, session(), |_, _| Some(site), &environment),
                Err(crate::FactError::Admission)
            ));
            assert!(
                cache.3.is_empty(),
                "a refused decode reservation publishes no retained artifact"
            );
            assert!(
                crate::CheckedEnvironment::stamp(&table)
                    .facts
                    .execution
                    .is_empty()
            );
        }
        rows.2.store(false, std::sync::atomic::Ordering::Relaxed);
        assert!(
            cache
                .revalidate_execution(&original, session(), |_, _| Some(site), &environment)
                .unwrap()
                .is_some(),
            "operational refusal remains retryable"
        );
        let active = cache
            .replay_artifact(&original.artifacts[0], session(), |_, _| Some(site))
            .unwrap();
        cache.evict_facts();
        assert_eq!(
            rows.3.load(std::sync::atomic::Ordering::Relaxed),
            2,
            "eviction preserves the held issuance and active replay charges"
        );
        drop(active);
        assert_eq!(rows.3.load(std::sync::atomic::Ordering::Relaxed), 1);
        drop(original);
        assert_eq!(rows.3.load(std::sync::atomic::Ordering::Relaxed), 0);
    }

    #[test]
    fn equal_digest_from_a_foreign_type_universe_cannot_reuse_a_local_signature_verdict() {
        use crate::SignatureLookup;
        let mut f = Fixture::unit();
        owner_sig(&mut f, &[]);
        let types = session();
        let base = hirsel_types::TypeBase::build(types.len(), |_| {
            types.ids().map(|id| types.row(id).clone()).collect()
        });
        let foreign = crate::IdentifiedSignature::new(
            TypeInterner::new(Some(base)),
            Arc::new(f.signatures.get(f.definition.path).unwrap().clone()),
        );
        let original = f.signatures.view(f.definition.path).unwrap().unwrap();
        assert_eq!(original.digest(), foreign.digest());
        struct Views<'a> {
            foreign: crate::IdentifiedSignature,
            original: &'a crate::SignatureMap,
        }
        impl SignatureLookup for Views<'_> {
            fn view(
                &self,
                d: hirsel_syntax::DefId,
            ) -> Result<Option<crate::ViewInput<'_>>, crate::ViewRefusal> {
                if d == self.foreign.definition() {
                    Ok(Some(self.foreign.input()))
                } else {
                    self.original.view(d)
                }
            }
            fn heap_role(&self) -> Result<Option<hirsel_types::HeapRole>, crate::ViewRefusal> {
                self.original.heap_role()
            }
            fn heap_observation(&self) -> Result<crate::HeapObservation, crate::ViewRefusal> {
                crate::input::observe_heap(None, self)
            }
            fn universe_errors(&self) -> (ContentHash, &[crate::Violation]) {
                self.original.universe_errors()
            }
        }
        let mut verdicts = crate::ValidatedSignatures::default();
        assert!(
            verdicts
                .verdict(&types, f.definition.path, &f.signatures)
                .unwrap()
                .error
                .is_none()
        );
        let views = Views {
            foreign,
            original: &f.signatures,
        };
        let (report, _, _, evidence) = validate_definition(
            &Arc::new(f.definition.clone()),
            types.clone(),
            |_| Some((path("main"), 0)),
            &views,
            ContentHash::default(),
            None,
        )
        .unwrap();
        assert!(
            !report.violations.is_empty(),
            "foreign owner must not be treated as optional absence"
        );
        assert!(evidence.is_none());
        assert!(
            verdicts
                .verdict(&types, f.definition.path, &views)
                .unwrap()
                .error
                .is_some()
        );
        assert!(
            VerifierCache::default()
                .view_verdict(
                    &types,
                    f.definition.path,
                    views.view(f.definition.path).unwrap().unwrap(),
                    &views,
                    ContentHash::default()
                )
                .unwrap()
                .error
                .is_some()
        );
    }

    #[test]
    fn local_evidence_binds_immutable_body_types_policy_and_current_views() {
        let mut f = Fixture::unit();
        owner_sig(&mut f, &[]);
        let types = session();
        let definition = Arc::new(f.definition.clone());
        let policy = ContentHash::of_bytes(b"source policy");
        let (report, ran, accepted, evidence) = validate_definition(
            &definition,
            types.clone(),
            |_| Some((path("main"), 0)),
            &f.signatures,
            policy,
            None,
        )
        .unwrap();
        assert!(ran && report.violations.is_empty());
        assert!(
            accepted.is_some(),
            "uncached V must issue execution acceptance"
        );
        let evidence = evidence.unwrap();
        assert_eq!(evidence.accepted_artifact(), accepted);
        assert!(evidence.holds(&definition, &types, policy, &f.signatures));
        assert!(!evidence.holds(
            &Arc::new((*definition).clone()),
            &types,
            policy,
            &f.signatures
        ));
        assert!(!evidence.holds(&definition, &types, ContentHash::default(), &f.signatures));
        let mut changed = definition.clone();
        Arc::make_mut(&mut changed).entry = BodyId(u32::MAX);
        assert!(!evidence.holds(&changed, &types, policy, &f.signatures));
        let (rejected, _, accepted, rejected_evidence) = validate_definition(
            &changed,
            types.clone(),
            |_| Some((path("main"), 0)),
            &f.signatures,
            policy,
            None,
        )
        .unwrap();
        assert!(!rejected.violations.is_empty());
        assert!(accepted.is_none());
        assert!(rejected_evidence.is_none());
        assert_eq!(
            Arc::strong_count(&definition),
            1,
            "evidence must not pin bodies"
        );
        f.signatures
            .get_mut(f.definition.path)
            .unwrap()
            .metadata
            .visibility = hirsel_syntax::Visibility::Public;
        assert!(!evidence.holds(&definition, &types, policy, &f.signatures));
    }

    #[test]
    fn identified_view_never_pairs_changed_bytes_with_a_foreign_digest() {
        let mut f = Fixture::unit();
        owner_sig(&mut f, &[]);
        let original = f.signatures.get(f.definition.path).unwrap().clone();
        let old = crate::IdentifiedSignature::new(session(), Arc::new(original.clone()));
        let mut changed = original;
        changed.metadata.visibility = hirsel_syntax::Visibility::Public;
        let current = crate::IdentifiedSignature::new(session(), Arc::new(changed));
        assert_ne!(old.input().digest(), current.input().digest());
        assert_eq!(
            old.input().signature().unwrap().metadata.visibility,
            hirsel_syntax::Visibility::Private
        );
    }

    #[test]
    fn signature_receipts_reopen_bind_policy_and_refuse_corrupt_or_imported_content() {
        use hirsel_intern::work::{self, Counter};
        let mut f = Fixture::unit();
        owner_sig(&mut f, &[]);
        let directory = Directory::new();
        let policy = ContentHash::default();
        let before = work::read(Counter::ValidateSignatureRows);
        assert!(
            view(&f, &VerifierCache::directory(directory.0.clone()), policy)
                .error
                .is_none()
        );
        assert_eq!(work::read(Counter::ValidateSignatureRows), before + 1);
        let reopened = VerifierCache::directory(directory.0.clone());
        assert!(view(&f, &reopened, policy).error.is_none());
        assert_eq!(work::read(Counter::ValidateSignatureRows), before + 1);
        assert!(
            view(&f, &reopened, ContentHash::of_bytes(b"changed policy"))
                .error
                .is_none()
        );
        assert_eq!(work::read(Counter::ValidateSignatureRows), before + 2);
        let signature = f.signatures.get(f.definition.path).unwrap();
        let key = view_key(crate::digest(&session(), signature), policy);
        let store = Store::new(directory.0.clone());
        let association = store.lookup(Address(key)).unwrap();
        let bytes = store.get(association.reference).unwrap();
        let imported = Directory::new();
        Store::new(imported.0.clone())
            .put(VIEW_DOMAIN, &bytes)
            .unwrap();
        assert!(
            view(&f, &VerifierCache::directory(imported.0.clone()), policy)
                .error
                .is_none()
        );
        assert_eq!(work::read(Counter::ValidateSignatureRows), before + 3);
        std::fs::write(store.output_path(association.reference), b"truncated").unwrap();
        assert!(view(&f, &reopened, policy).error.is_none());
        assert_eq!(work::read(Counter::ValidateSignatureRows), before + 4);
    }

    #[test]
    fn signature_receipts_never_accept_producer_digests_or_publish_failure() {
        use hirsel_intern::work::{self, Counter};
        let mut f = Fixture::unit();
        owner_sig(&mut f, &[]);
        let directory = Directory::new();
        let cache = VerifierCache::directory(directory.0.clone());
        let policy = ContentHash::default();
        assert!(view(&f, &cache, policy).error.is_none());
        let signature = f.signatures.get(f.definition.path).unwrap().clone();
        let good_digest = crate::digest(&session(), &signature);
        let mut broken = signature;
        let hirsel_types::SignatureKind::Function(function) = &mut broken.kind else {
            unreachable!()
        };
        function.result =
            session().intern(hirsel_types::Type::with_kind(hirsel_types::TypeKind::Error));
        let before = work::read(Counter::ValidateSignatureRows);
        for _ in 0..2 {
            let verdict = cache.view_verdict(
                &session(),
                f.definition.path,
                crate::IdentifiedSignature::new(session(), Arc::new(broken.clone())).input(),
                &f.signatures,
                policy,
            );
            assert!(verdict.unwrap().error.is_some());
        }
        let bad_digest = crate::digest(&session(), &broken);
        assert_ne!(bad_digest, good_digest);
        for _ in 0..2 {
            let verdict = cache.view_verdict(
                &session(),
                f.definition.path,
                crate::IdentifiedSignature::new(session(), Arc::new(broken.clone())).input(),
                &f.signatures,
                policy,
            );
            assert!(verdict.unwrap().error.is_some());
        }
        assert_eq!(work::read(Counter::ValidateSignatureRows), before + 4);
        assert!(
            Store::new(directory.0.clone())
                .lookup(Address(view_key(bad_digest, policy)))
                .is_none()
        );
    }

    #[test]
    fn receipts_bind_all_artifact_bytes_policy_and_independent_views() {
        let mut f = Fixture::unit();
        owner_sig(&mut f, &[]);
        let cache = VerifierCache::default();
        let policy = ContentHash::default();
        let (report, ran) = check(&f, &cache, policy);
        assert!(ran && report.violations.is_empty(), "{report:?}");
        assert!(!check(&f, &cache, policy).1);
        assert!(check(&f, &cache, ContentHash::of_bytes(b"new V policy")).1);
        // The producer's public fingerprint stays unchanged; V observes every
        // view field independently, including metadata and private shape.
        f.signatures
            .get_mut(f.definition.path)
            .unwrap()
            .metadata
            .visibility = hirsel_syntax::Visibility::Public;
        assert!(check(&f, &cache, policy).1);
        f.body().rvalues[0] = Rvalue::Constant(Constant::Bool(true));
        for _ in 0..2 {
            let (report, ran) = check(&f, &cache, policy);
            assert!(ran && !report.violations.is_empty());
        }
    }

    #[test]
    fn directory_receipts_survive_reopen_and_corruption_requires_first_v() {
        let f = Fixture::unit();
        let directory = Directory::new();
        let policy = ContentHash::default();
        assert!(check(&f, &VerifierCache::directory(directory.0.clone()), policy).1);
        let reopened = VerifierCache::directory(directory.0.clone());
        assert!(!check(&f, &reopened, policy).1);
        let store = Store::new(directory.0.clone());
        let receipt = store.lookup(Address(key(&artifact(&f), policy))).unwrap();
        std::fs::write(store.output_path(receipt.reference), b"truncated").unwrap();
        let (report, ran) = check(&f, &reopened, policy);
        assert!(ran && report.violations.is_empty());
    }

    #[test]
    fn local_receipts_bind_exact_bytes_and_validator_build_before_reuse() {
        let mut f = Fixture::unit();
        let directory = Directory::new();
        let options = ContentHash::of_bytes(b"validation options");
        let policy = |build: &[u8]| {
            ContentHash::of_parts("local V build and options", [build, options.0.as_slice()])
        };
        let original = policy(b"V build 1");
        let cache = VerifierCache::directory(directory.0.clone());
        let (report, ran) = check(&f, &cache, original);
        assert!(ran && report.violations.is_empty());
        drop(cache);
        let reopened = VerifierCache::directory(directory.0.clone());
        assert!(!check(&f, &reopened, original).1);
        let (report, ran) = check(&f, &reopened, policy(b"V build 2"));
        assert!(
            ran && report.violations.is_empty(),
            "a new V build must run V"
        );
        f.definition.entry = hirsel_tir::BodyId(u32::MAX);
        for _ in 0..2 {
            let (report, ran) = check(&f, &reopened, original);
            assert!(
                ran && !report.violations.is_empty(),
                "changed bytes must run and fail V"
            );
        }
        assert!(
            Store::new(directory.0.clone())
                .lookup(Address(key(&artifact(&f), original)))
                .is_none(),
            "V must not publish a rejection as acceptance"
        );
    }

    #[test]
    fn execution_receipts_require_v_capabilities_and_exact_current_contexts() {
        let f = Fixture::unit();
        let directory = Directory::new();
        let cache = VerifierCache::directory(directory.0.clone());
        let policy = ContentHash::default();
        let (report, _, accepted) = validate_with_receipt(
            &artifact(&f),
            &f.signatures,
            policy,
            Some(&cache),
            &mut vec![],
        )
        .unwrap();
        assert!(report.violations.is_empty());
        let contexts = vec![(
            path("main").0.package,
            ContentHash::of_bytes(b"private and public views"),
        )];
        let accepted = accepted.unwrap();
        assert!(ExecutionReceipt::compose(&contexts, &[]).is_none());
        assert!(ExecutionReceipt::compose(&[], &[accepted.clone()]).is_none());
        let receipt = ExecutionReceipt::compose(&contexts, &[accepted]).unwrap();
        let subject = ContentHash::of_bytes(b"complete evaluated value and action");
        cache.publish_execution(subject, &receipt);
        let reopened = VerifierCache::directory(directory.0.clone());
        assert!(
            reopened
                .execution(subject, |_| Some(contexts[0].1))
                .is_some()
        );
        assert!(
            reopened
                .execution(subject, |_| Some(ContentHash::default()))
                .is_none()
        );
        assert!(
            reopened
                .execution(ContentHash::default(), |_| Some(contexts[0].1))
                .is_none()
        );
        // Even exact output bytes do not transfer the issuer's associations.
        let imported = Directory::new();
        let source = Store::new(directory.0.clone());
        let association = source.lookup(Address(execution_key(subject))).unwrap();
        let bytes = source.get(association.reference).unwrap();
        Store::new(imported.0.clone())
            .put(EXECUTION_DOMAIN, &bytes)
            .unwrap();
        assert!(
            VerifierCache::directory(imported.0.clone())
                .execution(subject, |_| Some(contexts[0].1))
                .is_none()
        );
    }

    struct ReplayViews<'a> {
        signatures: Option<&'a dyn SignatureLookup>,
        package: PackageId,
        digest: ContentHash,
        calls: std::cell::Cell<usize>,
    }
    impl ReplayEnvironment for ReplayViews<'_> {
        fn base_digest(
            &self,
            package: &PackageId,
        ) -> Result<Option<ContentHash>, crate::ViewRefusal> {
            Ok((package == &self.package).then_some(self.digest))
        }
        fn views(
            &self,
            package: &PackageId,
            _: Option<hirsel_types::SignatureRef>,
        ) -> Result<Option<Box<dyn SignatureLookup + '_>>, crate::ViewRefusal> {
            self.calls.set(self.calls.get() + 1);
            if self.base_digest(package)?.is_none() {
                return Ok(None);
            }
            Ok(self
                .signatures
                .map(|signatures| Box::new(signatures) as Box<dyn SignatureLookup>))
        }
    }

    struct CheckedReplay<'a> {
        package: &'a PackageId,
        views: &'a crate::ViewTable,
    }
    impl ReplayEnvironment for CheckedReplay<'_> {
        fn checked(
            &self,
            package: &PackageId,
        ) -> Result<Option<Box<dyn crate::CheckedEnvironment + '_>>, crate::ViewRefusal> {
            Ok((package == self.package)
                .then(|| Box::new(self.views) as Box<dyn crate::CheckedEnvironment>))
        }
        fn base_digest(
            &self,
            package: &PackageId,
        ) -> Result<Option<ContentHash>, crate::ViewRefusal> {
            Ok((package == self.package).then(|| self.views.input_digest()))
        }
        fn views(
            &self,
            package: &PackageId,
            _: Option<hirsel_types::SignatureRef>,
        ) -> Result<Option<Box<dyn SignatureLookup + '_>>, crate::ViewRefusal> {
            Ok((package == self.package).then(|| Box::new(self.views) as Box<dyn SignatureLookup>))
        }
    }

    #[test]
    fn execution_facts_compute_each_authenticated_input_once_and_reject_foreign_bindings() {
        use hirsel_intern::work::{self, Counter};
        let mut f = Fixture::unit();
        owner_sig(&mut f, &[]);
        f.body().blocks[0].span.anchor = Anchor::fresh();
        let site = f.definition.bodies[0].blocks[0].span.anchor;
        let cache = Arc::new(VerifierCache::default());
        let policy = ContentHash::default();
        let (_, _, accepted) = validate_with_receipt(
            &artifact(&f),
            &f.signatures,
            policy,
            Some(&cache),
            &mut vec![],
        )
        .unwrap();
        let accepted = accepted.unwrap();
        let package = path("main").0.package;
        let original = ExecutionReceipt::compose(
            &[(package.clone(), ContentHash::of_bytes(b"original"))],
            &[accepted.clone(), accepted.clone()],
        )
        .unwrap();
        let alternate = ExecutionReceipt::compose(
            &[(package.clone(), ContentHash::of_bytes(b"another issuance"))],
            &[accepted.clone(), accepted.clone()],
        )
        .unwrap();
        let table = |extra| {
            let mut rows = f
                .signatures
                .values()
                .cloned()
                .map(Arc::new)
                .collect::<Vec<_>>();
            if extra {
                let mut unrelated = (*rows[0]).clone();
                unrelated.definition = hirsel_syntax::DefId::intern(&path("unrelated"));
                rows.push(Arc::new(unrelated));
            }
            crate::ViewTable::new(
                &session(),
                rows,
                None,
                &mut crate::ValidatedSignatures::default(),
                None,
            )
            .unwrap()
        };
        let tables = [table(false), table(true)];
        assert_ne!(tables[0].input_digest(), tables[1].input_digest());
        // A first binding rendezvous makes every request overlap before an
        // immutable replay can be published. Binding is part of the fact key.
        let types = session();
        let mut concurrent = Vec::new();
        for valid in [true, false] {
            cache.evict_facts();
            let start = std::sync::Barrier::new(8);
            let bindings = std::sync::Barrier::new(8);
            let environment = CheckedReplay {
                package: &package,
                views: &tables[0],
            };
            let counts = std::thread::scope(|scope| {
                let workers = (0..8)
                    .map(|_| {
                        let cache = &cache;
                        let original = &original;
                        let types = &types;
                        let environment = &environment;
                        let start = &start;
                        let bindings = &bindings;
                        scope.spawn(move || {
                            let before = [
                                Counter::ReplayArtifactDecodes,
                                Counter::ExecutionReplayComputations,
                            ]
                            .map(work::read);
                            start.wait();
                            let mut first = true;
                            let result = cache
                                .revalidate_execution(
                                    original,
                                    types.clone(),
                                    |p, n| {
                                        if first {
                                            first = false;
                                            bindings.wait();
                                        }
                                        (valid && *p == path("main") && n == 0).then_some(site)
                                    },
                                    environment,
                                )
                                .unwrap();
                            assert_eq!(result.is_some(), valid);
                            let after = [
                                Counter::ReplayArtifactDecodes,
                                Counter::ExecutionReplayComputations,
                            ]
                            .map(work::read);
                            [after[0] - before[0], after[1] - before[1]]
                        })
                    })
                    .collect::<Vec<_>>();
                workers
                    .into_iter()
                    .map(|worker| worker.join().unwrap())
                    .fold([0, 0], |sum, next| [sum[0] + next[0], sum[1] + next[1]])
            });
            concurrent.push((valid, counts));
        }
        assert_eq!(
            concurrent,
            [(true, [1, 1]), (false, [1, 1])],
            "overlapping requests share their result or refusal"
        );
        cache.evict_facts();
        let verdict_key = ContentHash::of_parts(
            "hirsel.validate.receipt.v3",
            [accepted.artifact.0.as_slice(), accepted.policy.0.as_slice()],
        );
        let recorded: Receipt = cache.load(verdict_key, DOMAIN).unwrap();
        // Enumerate the fixture's authenticated inputs independently of cache
        // counters and consumer count. The recorded reads are input to replay.
        let decoded =
            std::collections::BTreeSet::from([(accepted.artifact, accepted.policy, site)]);
        let environments = (0..tables.len())
            .map(|i| (accepted.artifact, accepted.policy, i))
            .collect::<std::collections::BTreeSet<_>>();
        let reads = (0..tables.len())
            .flat_map(|i| recorded.reads.iter().cloned().map(move |read| (i, read)))
            .collect::<std::collections::BTreeSet<_>>();
        let before = [
            Counter::ReplayArtifactDecodes,
            Counter::ExecutionReplayComputations,
            Counter::SealedReadComparisons,
        ]
        .map(work::read);
        for index in [0, 1, 0, 1] {
            let environment = CheckedReplay {
                package: &package,
                views: &tables[index],
            };
            for receipt in [&original, &alternate, &original, &alternate, &original] {
                assert!(
                    cache
                        .revalidate_execution(
                            receipt,
                            session(),
                            |p, n| (*p == path("main") && n == 0).then_some(site),
                            &environment
                        )
                        .unwrap()
                        .is_some()
                );
            }
        }
        let actual = [
            Counter::ReplayArtifactDecodes,
            Counter::ExecutionReplayComputations,
            Counter::SealedReadComparisons,
        ]
        .map(work::read);
        let actual = std::array::from_fn::<_, 3, _>(|i| actual[i] - before[i]);
        assert!(
            actual[0] <= decoded.len() as u64,
            "decode work follows exact artifact/binding identities"
        );
        assert!(
            actual[1] <= environments.len() as u64,
            "compatibility follows complete immutable environments"
        );
        assert!(
            actual[2] <= reads.len() as u64,
            "read comparisons {actual:?} exceed {} exact read/observer identities",
            reads.len()
        );
        let environment = CheckedReplay {
            package: &package,
            views: &tables[1],
        };
        assert!(
            cache
                .revalidate_execution(&original, session(), |_, _| None, &environment)
                .unwrap()
                .is_none()
        );
        assert!(
            cache
                .revalidate_execution(
                    &original,
                    TypeInterner::new(None),
                    |_, _| Some(site),
                    &environment
                )
                .unwrap()
                .is_none()
        );
        let wrong = ExecutionReceipt::compose(
            original.contexts(),
            &[AcceptedArtifact {
                artifact: accepted.artifact,
                policy: ContentHash::of_bytes(b"foreign policy"),
                receipt: accepted.receipt,
                original: accepted.original.clone(),
            }],
        )
        .unwrap();
        assert!(
            cache
                .revalidate_execution(&wrong, session(), |_, _| Some(site), &environment)
                .unwrap()
                .is_none()
        );
        assert!(
            VerifierCache::default()
                .revalidate_execution(&original, session(), |_, _| Some(site), &environment)
                .unwrap()
                .is_none()
        );
        let before = work::read(Counter::VerifierFactEvictions);
        cache.evict_facts();
        assert!(work::read(Counter::VerifierFactEvictions) > before);
        let before = work::read(Counter::ReplayArtifactDecodes);
        assert!(
            cache
                .revalidate_execution(&original, session(), |_, _| Some(site), &environment)
                .unwrap()
                .is_some()
        );
        assert_eq!(
            work::read(Counter::ReplayArtifactDecodes) - before,
            decoded.len() as u64
        );
        let next = cache.for_operation(None);
        let before = work::read(Counter::ReplayArtifactDecodes);
        assert!(
            next.revalidate_execution(&original, session(), |_, _| Some(site), &environment)
                .unwrap()
                .is_some()
        );
        assert_eq!(
            work::read(Counter::ReplayArtifactDecodes) - before,
            decoded.len() as u64,
            "a new operation has its own decoded owner and issuer"
        );
    }

    #[test]
    fn execution_product_binding_refuses_substitution_and_stale_inputs() {
        let mut fixture = Fixture::string();
        let string = fixture.definition.bodies[0].locals[1].ty;
        fixture.body().result = string;
        fixture.body().blocks[0].terminator = crate::support::ret(1, 0);
        owner_sig(&mut fixture, &[]);
        let directory = Directory::new();
        let cache = VerifierCache::directory(directory.0.clone());
        let policy = ContentHash::of_bytes(b"current V policy");
        assert!(check(&fixture, &cache, policy).0.violations.is_empty());
        let inputs = ExecutionProductInputs {
            namespace: ContentHash::of_bytes(b"admitted consumer capture and demand scope"),
            observations: ContentHash::of_bytes(b"exact source bytes and directory membership"),
            imports: ContentHash::of_bytes(b"std implementation and private representations"),
            compiler: ContentHash::of_bytes(b"compiler implementation"),
            policy,
            options: ContentHash::of_bytes(b"lowering and comptime charging"),
            entry: path("main"),
        };
        let domain = Domain {
            kind: 0x4600_0011,
            schema: 1,
        };
        let value = hirsel_tir::Materialized {
            root: hirsel_tir::ValueNode {
                ty: fixture.definition.bodies[0].locals[1].ty,
                kind: hirsel_tir::ValueKind::Constant(hirsel_tir::Constant::String(
                    "original".into(),
                )),
            },
        };
        let declaration = crate::support::capability_signature("Private");
        let product = |definition: &Definition,
                       value: &hirsel_tir::Materialized,
                       private: &hirsel_types::Signature| {
            let bytes = hirsel_product_codec::canonical_bytes(
                session(),
                &(definition, value, private),
                |_| Some((path("main"), 0)),
            )
            .unwrap();
            domain.reference(&bytes).unwrap()
        };
        let original_definition = fixture.definition.clone();
        let original = product(&original_definition, &value, &declaration);
        assert!(cache.execution_product(&inputs, original).is_none());
        let source_check = domain.reference(b"completed source Check").unwrap();
        cache
            .publish_execution_product_durable(&inputs, original, source_check)
            .unwrap();
        // The same declarations admit this different body. V alone cannot
        // establish that it came from the accepted sources.
        let literal = fixture
            .body()
            .rvalues
            .iter_mut()
            .find_map(|rvalue| match rvalue {
                hirsel_tir::Rvalue::Constant(hirsel_tir::Constant::String(value)) => Some(value),
                _ => None,
            })
            .unwrap();
        *literal = "substituted".into();
        assert!(check(&fixture, &cache, policy).0.violations.is_empty());
        let substituted_body = product(&fixture.definition, &value, &declaration);
        assert_ne!(original, substituted_body);
        let mut substituted_value = value.clone();
        substituted_value.root.kind =
            hirsel_tir::ValueKind::Constant(hirsel_tir::Constant::String("substituted".into()));
        let substituted_constant = product(&original_definition, &substituted_value, &declaration);
        assert_ne!(substituted_body, substituted_constant);
        let cache = VerifierCache::directory(directory.0.clone());
        let binding = cache.execution_product(&inputs, original).unwrap();
        assert_eq!(binding.product(), original);
        assert_eq!(binding.inputs(), &inputs);
        assert_eq!(binding.source_check(), source_check);
        let mut private = declaration.clone();
        let hirsel_types::SignatureKind::Type(ty) = &mut private.kind else {
            unreachable!()
        };
        ty.representation = Some(session().intern(hirsel_types::Type::unit()));
        let mismatched_private = product(&original_definition, &value, &private);
        let mut runtime = declaration.clone();
        let hirsel_types::SignatureKind::Type(ty) = &mut runtime.kind else {
            unreachable!()
        };
        ty.layout.flat = !ty.layout.flat;
        ty.destructor_allocation = hirsel_types::Allocation::MayAllocate;
        let mismatched_runtime = product(&original_definition, &value, &runtime);
        assert_eq!(private.fingerprint, declaration.fingerprint);
        assert_eq!(runtime.fingerprint, declaration.fingerprint);
        for substituted in [
            substituted_body,
            substituted_constant,
            mismatched_private,
            mismatched_runtime,
        ] {
            assert!(cache.execution_product(&inputs, substituted).is_none());
        }
        for field in 0..7 {
            let mut stale = inputs.clone();
            let changed = ContentHash::of_bytes(b"changed input");
            match field {
                0 => stale.namespace = changed,
                1 => stale.observations = changed,
                2 => stale.imports = changed,
                3 => stale.compiler = changed,
                4 => stale.policy = changed,
                5 => stale.options = changed,
                _ => stale.entry = path("another_entry"),
            }
            assert!(cache.execution_product(&stale, original).is_none());
        }
        assert!(
            VerifierCache::default()
                .execution_product(&inputs, original)
                .is_none()
        );
        // Keep the key and framing valid while substituting the record's
        // claimed inputs or product: the loaded fields must be checked too.
        let key = inputs.key(original).unwrap();
        cache.publish(
            key,
            execution_product::DOMAIN,
            &execution_product::BindingRecord {
                inputs: inputs.clone(),
                product: substituted_body,
                source_check,
            },
        );
        assert!(cache.execution_product(&inputs, original).is_none());
    }

    #[test]
    fn execution_product_import_requires_v_even_when_invalid_tir_has_a_byte_binding() {
        let mut fixture = Fixture::unit();
        owner_sig(&mut fixture, &[]);
        let cache = VerifierCache::default();
        let inputs = ExecutionProductInputs {
            namespace: ContentHash::of_bytes(b"admitted capture"),
            observations: ContentHash::of_bytes(b"observed bytes"),
            imports: ContentHash::of_bytes(b"installed implementations"),
            compiler: ContentHash::of_bytes(b"current compiler"),
            policy: ContentHash::of_bytes(b"current V policy"),
            options: ContentHash::of_bytes(b"semantic options"),
            entry: path("main"),
        };
        let records = cache.records(session());
        let archive = |definition: &Definition| {
            let bytes = records
                .encode_tir(definition, |_| Some((path("main"), 0)))
                .unwrap();
            let reference = hirsel_product_codec::CheckedTir::DOMAIN
                .reference(&bytes)
                .unwrap();
            Arc::new(
                hirsel_product_codec::CheckedTir::open(
                    bytes.into(),
                    reference,
                    records.clone(),
                    |_, _| Some(Anchor::default()),
                )
                .unwrap(),
            )
        };
        let valid = archive(&fixture.definition);
        let source_check = Domain { kind: 1, schema: 1 }
            .reference(b"source Check")
            .unwrap();
        let import = |archive: &Arc<hirsel_product_codec::CheckedTir>| {
            cache
                .import_execution_definition(
                    &inputs,
                    archive,
                    session(),
                    |_| Some((path("main"), 0)),
                    &fixture.signatures,
                )
                .unwrap()
        };
        assert!(
            import(&valid).is_none(),
            "V-valid bytes need a source binding"
        );
        cache
            .publish_execution_product(&inputs, valid.reference(), source_check)
            .unwrap();
        let accepted = import(&valid).unwrap();
        assert!(accepted.holds(&valid, &session(), inputs.policy, &fixture.signatures));
        // The archive is well framed and its source binding matches exactly.
        // Its entry names no body, which V must reject independently of both.
        let mut invalid = fixture.definition.clone();
        invalid.entry = BodyId(u32::MAX);
        let invalid = archive(&invalid);
        cache
            .publish_execution_product(&inputs, invalid.reference(), source_check)
            .unwrap();
        assert!(
            cache
                .execution_product(&inputs, invalid.reference())
                .is_some()
        );
        assert!(import(&invalid).is_none());
    }

    #[test]
    fn execution_replay_runs_v_and_refuses_missing_wrong_or_uncovered_contexts() {
        let mut f = Fixture::unit();
        owner_sig(&mut f, &[]);
        f.body().blocks[0].span.anchor = Anchor::fresh();
        let cache = VerifierCache::default();
        let policy = ContentHash::default();
        let (_, _, accepted) = validate_with_receipt(
            &artifact(&f),
            &f.signatures,
            policy,
            Some(&cache),
            &mut vec![],
        )
        .unwrap();
        let accepted = accepted.unwrap();
        let package = path("main").0.package;
        let old = ContentHash::of_bytes(b"old universe");
        let new = ContentHash::of_bytes(b"new universe");
        let receipt =
            ExecutionReceipt::compose(&[(package.clone(), old)], &[accepted.clone()]).unwrap();
        let subject = ContentHash::of_bytes(b"replay subject");
        cache.publish_execution(subject, &receipt);
        let mut environment = ReplayViews {
            signatures: None,
            package: package.clone(),
            digest: new,
            calls: std::cell::Cell::new(0),
        };
        let anchor = f.definition.bodies[0].blocks[0].span.anchor;
        let replay = |environment: &ReplayViews<'_>| {
            cache
                .execution_revalidated(
                    subject,
                    session(),
                    |p, n| (*p == path("main") && n == 0).then_some(anchor),
                    environment,
                )
                .unwrap()
        };
        // A retained clone cannot refresh context: V must receive current views.
        assert!(replay(&environment).is_none());
        assert!(cache.execution(subject, |_| Some(new)).is_none());
        environment.package.name = "wrong-package".into();
        assert!(replay(&environment).is_none());
        environment.package = package.clone();
        let empty = crate::SignatureMap::new(session());
        environment.signatures = Some(&empty);
        assert!(replay(&environment).is_none());
        assert!(cache.execution(subject, |_| Some(new)).is_none());
        environment.signatures = Some(&f.signatures);
        assert!(replay(&environment).is_some());
        assert!(environment.calls.get() >= 2);
        assert!(cache.execution(subject, |_| Some(new)).is_some());
        environment.signatures = None;
        assert!(
            replay(&environment).is_none(),
            "an unchanged raw digest cannot replace current observations"
        );
        environment.signatures = Some(&f.signatures);
        let mut other = package.clone();
        other.name = "uncovered".into();
        let mut contexts = vec![(package, old), (other, old)];
        contexts.sort();
        cache.publish_execution(
            subject,
            &ExecutionReceipt::compose(&contexts, &[accepted]).unwrap(),
        );
        assert!(replay(&environment).is_none());

        cache.publish_execution(subject, &receipt);
        // Eviction may remove cache issuance while this operation still owns
        // V's capability. Its old observations must survive a context refresh.
        let Backend::Memory(rows, _) = &cache.0 else {
            unreachable!()
        };
        rows.lock()
            .unwrap()
            .remove(&original_receipt_key(receipt.artifacts[0].receipt));
        cache.evict_facts();
        assert!(
            cache
                .revalidate_execution(&receipt, session(), |_, _| Some(anchor), &environment,)
                .unwrap()
                .is_some()
        );
        // Successful V may republish issuance. Evict it again before testing
        // an independent cache-only restore with no held capability.
        rows.lock()
            .unwrap()
            .remove(&original_receipt_key(receipt.artifacts[0].receipt));
        cache.evict_facts();
        assert!(
            replay(&environment).is_none(),
            "cache-only restore refuses absent original issuance"
        );
    }

    #[test]
    fn execution_replay_refuses_a_supplied_but_uncovered_universe() {
        let mut f = Fixture::unit();
        owner_sig(&mut f, &[]);
        let cache = VerifierCache::default();
        let (_, _, accepted) = validate_with_receipt(
            &artifact(&f),
            &f.signatures,
            ContentHash::default(),
            Some(&cache),
            &mut vec![],
        )
        .unwrap();
        let old = ContentHash::of_bytes(b"old");
        let mut other = path("main").0.package;
        other.name = "other".into();
        let mut contexts = vec![(path("main").0.package, old), (other, old)];
        contexts.sort();
        let subject = ContentHash::of_bytes(b"uncovered subject");
        cache.publish_execution(
            subject,
            &ExecutionReceipt::compose(&contexts, &[accepted.unwrap()]).unwrap(),
        );
        struct Supplied<'a>(&'a dyn SignatureLookup);
        impl ReplayEnvironment for Supplied<'_> {
            fn base_digest(
                &self,
                _: &PackageId,
            ) -> Result<Option<ContentHash>, crate::ViewRefusal> {
                Ok(Some(ContentHash::of_bytes(b"new")))
            }
            fn views(
                &self,
                _: &PackageId,
                _: Option<hirsel_types::SignatureRef>,
            ) -> Result<Option<Box<dyn SignatureLookup + '_>>, crate::ViewRefusal> {
                Ok(Some(Box::new(self.0)))
            }
        }
        assert!(
            cache
                .execution_revalidated(
                    subject,
                    session(),
                    |_, _| Some(Anchor::default()),
                    &Supplied(&f.signatures)
                )
                .unwrap()
                .is_none()
        );
        assert!(
            cache
                .execution(subject, |_| Some(ContentHash::of_bytes(b"new")))
                .is_none()
        );
    }

    #[test]
    fn execution_replay_uses_checked_heap_evidence_across_equal_carriers() {
        use crate::support::{arena_slot, function_signature};
        let mut f = Fixture::call();
        arena_slot(&mut f, hirsel_tir::Storage::Temporary);
        owner_sig(&mut f, &[]);
        let cache = VerifierCache::default();
        let policy = ContentHash::default();
        let mut reads = vec![];
        let (report, _, accepted) = validate_with_receipt(
            &artifact(&f),
            &f.signatures,
            policy,
            Some(&cache),
            &mut reads,
        )
        .unwrap();
        assert!(report.violations.is_empty(), "{report:?}");
        assert!(reads.iter().any(|r| matches!(
            r,
            VRead::HeapRole(heap) if matches!(**heap, crate::HeapObservation::SelectedHeap { .. })
        )));
        let accepted = accepted.unwrap();
        let package = path("main").0.package;
        let old = ContentHash::of_bytes(b"original heap universe");
        let new = ContentHash::of_bytes(b"earlier equal provider universe");
        let subject = ContentHash::of_bytes(b"heap replay subject");
        let receipt = ExecutionReceipt::compose(&[(package.clone(), old)], &[accepted]).unwrap();
        let mut current = f.signatures.clone();
        let mut earlier = function_signature(
            "0provider",
            vec![],
            hirsel_types::TypeKind::Tuple(vec![].into()),
        );
        earlier.metadata.roles = current.get(path("Arena")).unwrap().metadata.roles.clone();
        current.insert(earlier.definition, earlier);
        struct MissingHeap<'a>(&'a dyn SignatureLookup);
        impl SignatureLookup for MissingHeap<'_> {
            fn view(
                &self,
                d: hirsel_syntax::DefId,
            ) -> Result<Option<crate::ViewInput<'_>>, crate::ViewRefusal> {
                self.0.view(d)
            }
            fn heap_role(&self) -> Result<Option<hirsel_types::HeapRole>, crate::ViewRefusal> {
                Ok(None)
            }
            fn heap_observation(&self) -> Result<crate::HeapObservation, crate::ViewRefusal> {
                crate::input::observe_heap(None, self)
            }
            fn universe_errors(&self) -> (ContentHash, &[crate::Violation]) {
                self.0.universe_errors()
            }
        }
        let attempt = |signatures: &dyn SignatureLookup| {
            cache.publish_execution(subject, &receipt);
            cache
                .execution_revalidated(
                    subject,
                    session(),
                    |_, _| Some(Anchor::default()),
                    &ReplayViews {
                        signatures: Some(signatures),
                        package: package.clone(),
                        digest: new,
                        calls: std::cell::Cell::new(0),
                    },
                )
                .unwrap()
        };
        // Equal carriers preserve the ordinary evidence read; replay uses the same guard.
        let old_heap = reads
            .iter()
            .find(|r| matches!(r, VRead::HeapRole(_)))
            .unwrap();
        assert!(crate::read_holds(old_heap, &current));
        assert!(attempt(&MissingHeap(&current)).is_none());
        for field in ["arena", "operation"] {
            let mut changed = current.clone();
            let role = std::sync::Arc::make_mut(
                &mut changed.get_mut(path("0provider")).unwrap().metadata.roles,
            )
            .heap
            .as_mut()
            .unwrap();
            if field == "arena" {
                role.arena = path("DifferentArena");
            } else {
                role.operation = path("different_operation");
            }
            assert!(attempt(&changed).is_none(), "changed {field}");
        }
        for field in [
            "operation declaration",
            "private shape",
            "package version",
            "absence",
        ] {
            let mut changed = current.clone();
            match field {
                "operation declaration" => {
                    changed.get_mut(path("heap")).unwrap().metadata.visibility =
                        hirsel_syntax::Visibility::Public;
                }
                "private shape" => {
                    let hirsel_types::SignatureKind::Type(declaration) =
                        &mut changed.get_mut(path("Arena")).unwrap().kind
                    else {
                        unreachable!()
                    };
                    declaration.representation = Some(session().intern(hirsel_types::Type::unit()));
                }
                "package version" => {
                    let ids: Vec<_> = changed.iter().map(|(id, _)| id).collect();
                    for id in ids {
                        let signature = changed.get_mut(id).unwrap();
                        if let Some(role) = std::sync::Arc::make_mut(&mut signature.metadata.roles)
                            .heap
                            .as_mut()
                        {
                            role.arena.0.package.version = "different".into();
                            role.operation.0.package.version = "different".into();
                        }
                    }
                }
                _ => {
                    let ids: Vec<_> = changed.iter().map(|(id, _)| id).collect();
                    for id in ids {
                        let signature = changed.get_mut(id).unwrap();
                        std::sync::Arc::make_mut(&mut signature.metadata.roles).heap = None;
                    }
                }
            }
            assert!(attempt(&changed).is_none(), "changed {field}");
        }
        let types = session();
        let base = hirsel_types::TypeBase::build(types.len(), |_| {
            types.ids().map(|id| types.row(id).clone()).collect()
        });
        let mut foreign = crate::SignatureMap::new(TypeInterner::new(Some(base)));
        foreign.extend(
            current
                .iter()
                .map(|(id, signature)| (id, signature.clone())),
        );
        assert!(attempt(&foreign).is_none(), "foreign type interner");
        let mut missing = current.clone();
        missing.remove(path("Arena"));
        assert!(attempt(&missing).is_none());
        let mut stale = current.clone();
        stale.get_mut(path("Arena")).unwrap().metadata.visibility =
            hirsel_syntax::Visibility::Public;
        assert!(attempt(&stale).is_none());
        assert!(attempt(&current).is_some());
        assert!(cache.execution(subject, |_| Some(new)).is_some());
    }

    #[test]
    fn private_overlay_identity_and_replay_associations_are_verifier_owned() {
        let mut f = Fixture::unit();
        owner_sig(&mut f, &[]);
        f.body().blocks[0].span.anchor = Anchor::fresh();
        let directory = Directory::new();
        let cache = VerifierCache::directory(directory.0.clone());
        let signature = std::sync::Arc::new(f.signatures.get(f.definition.path).unwrap().clone());
        let exact = Artifact::private(&f.definition, session(), signature.clone(), |_| {
            Some((path("main"), 0))
        });
        let expected = hirsel_product_codec::canonical_bytes(
            session(),
            &(&f.definition, &Some(signature.clone())),
            |_| Some((path("main"), 0)),
        )
        .unwrap();
        assert_eq!(
            exact.identity,
            Some(ContentHash::of_parts(
                "hirsel.validate.private-artifact.v2",
                [expected.as_slice()]
            ))
        );
        let mut changed = (*signature).clone();
        changed.metadata.visibility = hirsel_syntax::Visibility::Public;
        assert_ne!(
            exact.identity,
            Artifact::private(
                &f.definition,
                session(),
                std::sync::Arc::new(changed),
                |_| Some((path("main"), 0))
            )
            .identity
        );
        let policy = ContentHash::default();
        let (_, _, accepted) =
            validate_with_receipt(&exact, &f.signatures, policy, Some(&cache), &mut vec![])
                .unwrap();
        let accepted = accepted.unwrap();
        let anchor = f.definition.bodies[0].blocks[0].span.anchor;
        assert!(
            cache
                .replay_artifact(&accepted, session(), |_, _| Some(anchor))
                .is_ok()
        );
        assert!(
            cache
                .replay_artifact(&accepted, session(), |_, _| None)
                .is_err()
        );
        let base = Arc::new(
            crate::ViewTable::new(
                &session(),
                f.signatures.values().cloned().map(Arc::new),
                None,
                &mut crate::ValidatedSignatures::default(),
                None,
            )
            .unwrap(),
        );
        let mut unrelated = (*signature).clone();
        unrelated.definition = hirsel_syntax::DefId::intern(&path("unrelated_overlay_base"));
        let other = Arc::new(
            crate::ViewTable::new(
                &session(),
                [signature.clone(), Arc::new(unrelated)],
                None,
                &mut crate::ValidatedSignatures::default(),
                None,
            )
            .unwrap(),
        );
        let overlay = Arc::new(
            crate::PackageViews::new(
                &session(),
                other.clone(),
                [crate::View::new(&session(), signature.clone(), None)],
                &mut crate::ValidatedSignatures::default(),
                None,
            )
            .unwrap(),
        );
        struct OverlayInputs<'a> {
            base: &'a crate::ViewTable,
            overlay: Arc<crate::PackageViews>,
            package: &'a PackageId,
        }
        impl ReplayEnvironment for OverlayInputs<'_> {
            fn checked(
                &self,
                p: &PackageId,
            ) -> Result<Option<Box<dyn crate::CheckedEnvironment + '_>>, crate::ViewRefusal>
            {
                Ok((p == self.package)
                    .then(|| Box::new(self.base) as Box<dyn crate::CheckedEnvironment>))
            }
            fn base_digest(
                &self,
                p: &PackageId,
            ) -> Result<Option<ContentHash>, crate::ViewRefusal> {
                Ok((p == self.package).then(|| self.base.input_digest()))
            }
            fn checked_overlay(
                &self,
                p: &PackageId,
                _: hirsel_types::SignatureRef,
            ) -> Result<Option<Arc<crate::PackageViews>>, crate::ViewRefusal> {
                Ok((p == self.package).then(|| self.overlay.clone()))
            }
            fn views(
                &self,
                p: &PackageId,
                _: Option<hirsel_types::SignatureRef>,
            ) -> Result<Option<Box<dyn SignatureLookup + '_>>, crate::ViewRefusal> {
                Ok((p == self.package)
                    .then(|| Box::new(self.overlay.clone()) as Box<dyn SignatureLookup>))
            }
        }
        let package = path("main").0.package;
        let original = ExecutionReceipt::compose(
            &[(
                package.clone(),
                ContentHash::of_bytes(b"old overlay context"),
            )],
            &[accepted.clone()],
        )
        .unwrap();
        assert!(
            cache
                .revalidate_execution(
                    &original,
                    session(),
                    |_, _| Some(anchor),
                    &OverlayInputs {
                        base: &base,
                        overlay: overlay.clone(),
                        package: &package,
                    }
                )
                .unwrap()
                .is_none(),
            "a checked overlay cannot refresh an unrelated declared base, even if every recorded read holds"
        );
        assert!(
            cache
                .revalidate_execution(
                    &original,
                    session(),
                    |_, _| Some(anchor),
                    &OverlayInputs {
                        base: &other,
                        overlay,
                        package: &package,
                    }
                )
                .unwrap()
                .is_some(),
            "the complete matching base still replays"
        );
        let source = Store::new(directory.0.clone());
        let association = source
            .lookup(Address(replay_key(accepted.artifact, accepted.policy)))
            .unwrap();
        let bytes = source.get(association.reference).unwrap();
        let imported = Directory::new();
        Store::new(imported.0.clone())
            .put(REPLAY_DOMAIN, &bytes)
            .unwrap();
        assert!(
            VerifierCache::directory(imported.0.clone())
                .replay_artifact(&accepted, session(), |_, _| Some(anchor))
                .is_err()
        );
    }

    #[test]
    fn imported_claimed_receipt_content_confers_no_authority() {
        let mut f = Fixture::unit();
        f.definition.entry = BodyId(u32::MAX);
        let directory = Directory::new();
        let policy = ContentHash::default();
        let artifact = artifact(&f);
        let forged = Receipt {
            action: receipt_action(artifact.identity.unwrap(), policy, &[]),
            artifact: artifact.identity.unwrap(),
            policy,
            reads: vec![],
        };
        // An importer can publish checked content, including bytes claiming the
        // verifier domain. It cannot issue a verifier association through the
        // ordinary product interface.
        Store::new(directory.0.clone())
            .put(DOMAIN, &bincode::serialize(&forged).unwrap())
            .unwrap();
        let cache = VerifierCache::directory(directory.0.clone());
        let (report, ran) = check(&f, &cache, policy);
        assert!(ran && !report.violations.is_empty());
        assert!(
            Store::new(directory.0.clone())
                .lookup(Address(key(&artifact, policy)))
                .is_none()
        );
    }
    /// Publish a stand-in installation header into `root`; returns it.
    fn installation_products(
        root: &std::path::Path,
        bytes: &[u8],
    ) -> (Address, hirsel_artifact_store::Association) {
        let store = Store::new(root.to_path_buf());
        let reference = store
            .put(
                Domain {
                    kind: 31,
                    schema: 1,
                },
                bytes,
            )
            .unwrap();
        let address = Address(ContentHash::of_bytes(b"ordinary"));
        let action = Action(ContentHash::default());
        store.associate(address, action, reference).unwrap();
        (
            address,
            hirsel_artifact_store::Association { action, reference },
        )
    }
    /// The header association `root` holds now.
    fn header(root: &std::path::Path) -> (Address, hirsel_artifact_store::Association) {
        let address = Address(ContentHash::of_bytes(b"ordinary"));
        (
            address,
            Store::new(root.to_path_buf())
                .lookup_owned(address)
                .unwrap(),
        )
    }
    fn registered(
        f: &Fixture,
        root: &std::path::Path,
        state: &std::path::Path,
        build: ContentHash,
    ) -> Arc<VerifierCache> {
        let install = Installation::begin(root, state, build).unwrap();
        let cache = install.verifier();
        assert!(check(f, &cache, build).0.violations.is_empty());
        install.finish(header(root)).unwrap();
        Installation::registered(root, state, build, Arc::default())
            .unwrap()
            .verifier
    }
    #[test]
    fn registered_installation_refuses_producer_forgery_with_and_without_registration() {
        let root = Directory::new();
        let state = Directory::new();
        let state = state.0.join("private");
        let build = ContentHash::of_bytes(b"local verifier");
        let valid = Fixture::unit();
        let mut malformed = Fixture::unit();
        malformed.definition.entry = BodyId(u32::MAX);
        let exact = artifact(&malformed);
        let identity = exact.identity.unwrap();
        let receipt = Receipt {
            action: receipt_action(identity, build, &[]),
            artifact: identity,
            policy: build,
            reads: vec![],
        };
        let producer = Store::new(root.0.clone());
        let reference = producer
            .put(DOMAIN, &bincode::serialize(&receipt).unwrap())
            .unwrap();
        let address = key(&exact, build);
        producer
            .associate(Address(address), Action(address), reference)
            .unwrap();
        // The producer has correct identities, checksums and association, as in
        // 211. Selecting product bytes still supplies no verifier capability.
        assert!(Installation::registered(&root.0, &state, build, Arc::default()).is_none());
        let (fresh, ran) = check(&malformed, &VerifierCache::default(), build);
        assert!(ran && !fresh.violations.is_empty());
        installation_products(&root.0, b"products");
        let installed = registered(&valid, &root.0, &state, build);
        assert!(!check(&valid, &installed, build).1);
        let (refused, ran) = check(&malformed, &installed, build);
        assert!(ran);
        assert_eq!(
            format!("{:?}", refused.violations),
            format!("{:?}", fresh.violations)
        );
        assert!(
            Installation::registered(
                &root.0,
                &state,
                ContentHash::of_bytes(b"another verifier"),
                Arc::default()
            )
            .is_none()
        );
    }
    #[test]
    fn registration_is_bound_to_root_state_and_exact_generation() {
        let root = Directory::new();
        let other = Directory::new();
        let state = Directory::new();
        let state = state.0.join("private");
        let fresh_state = Directory::new();
        let fresh_state = fresh_state.0.join("private");
        let build = ContentHash::of_bytes(b"local verifier");
        let f = Fixture::unit();
        installation_products(&root.0, b"generation N");
        installation_products(&other.0, b"generation N");
        let pinned = registered(&f, &root.0, &state, build);
        assert!(!check(&f, &pinned, build).1);
        let copy = |source: &std::path::Path, target: &std::path::Path| {
            std::fs::create_dir_all(target).unwrap();
            for entry in std::fs::read_dir(source).unwrap() {
                let path = entry.unwrap().path();
                if path.is_file() {
                    std::fs::copy(&path, target.join(path.file_name().unwrap())).unwrap();
                }
            }
        };
        // Copy registration bytes under the new root's expected name too.
        for path in std::fs::read_dir(&state)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "registration"))
        {
            std::fs::copy(
                path,
                installation::registration_path(&state, &other.0.canonicalize().unwrap()),
            )
            .unwrap();
        }
        assert!(Installation::registered(&other.0, &state, build, Arc::default()).is_none());
        installation::private_directory(&fresh_state).unwrap();
        copy(&state, &fresh_state);
        assert!(Installation::registered(&root.0, &fresh_state, build, Arc::default()).is_none());
        installation_products(&root.0, b"modified generation N+1");
        assert!(Installation::registered(&root.0, &state, build, Arc::default()).is_none());
        // A selected request retains only N's issued associations.
        assert!(!check(&f, &pinned, build).1);
        let mut modified = Fixture::unit();
        modified.definition.entry = BodyId(u32::MAX);
        assert!(!check(&modified, &pinned, build).0.violations.is_empty());
    }
    #[test]
    fn deleting_or_corrupting_private_registration_falls_back_to_identical_v() {
        let root = Directory::new();
        let state = Directory::new();
        let state = state.0.join("private");
        let build = ContentHash::of_bytes(b"local verifier");
        let f = Fixture::unit();
        installation_products(&root.0, b"products");
        let pinned = registered(&f, &root.0, &state, build);
        let (accepted, ran) = check(&f, &pinned, build);
        assert!(!ran && accepted.violations.is_empty());
        let path = installation::registration_path(
            &state.canonicalize().unwrap(),
            &root.0.canonicalize().unwrap(),
        );
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[0] ^= 1;
        std::fs::write(&path, bytes).unwrap();
        assert!(Installation::registered(&root.0, &state, build, Arc::default()).is_none());
        std::fs::remove_dir_all(&state).unwrap();
        assert!(Installation::registered(&root.0, &state, build, Arc::default()).is_none());
        let (fresh, ran) = check(&f, &VerifierCache::default(), build);
        assert!(ran);
        assert_eq!(
            format!("{:?}", accepted.violations),
            format!("{:?}", fresh.violations)
        );
    }
    #[test]
    fn a_failure_after_the_rename_keeps_the_generation_the_registration_names() {
        let root = Directory::new();
        let state = Directory::new();
        let state = state.0.join("private");
        let build = ContentHash::of_bytes(b"local verifier");
        let f = Fixture::unit();
        installation_products(&root.0, b"products");
        let install = Installation::begin(&root.0, &state, build).unwrap();
        assert!(
            check(&f, &install.verifier(), build)
                .0
                .violations
                .is_empty()
        );
        installation::FAIL_AFTER_RENAME.set(true);
        assert!(install.finish(header(&root.0)).is_err());
        // The renamed registration is visible, so its generation must survive.
        let pinned = Installation::registered(&root.0, &state, build, Arc::default())
            .expect("the visible registration names a kept generation")
            .verifier;
        assert!(!check(&f, &pinned, build).1);
    }
    /// A header changed by a writer outside the precompute protocol between
    /// product publication and `finish`: `finish` registers the header this
    /// run published, so the registration never vouches for the other header.
    #[test]
    fn finish_registers_the_header_it_published_not_the_one_on_disk() {
        let root = Directory::new();
        let state = Directory::new();
        let state = state.0.join("private");
        let build = ContentHash::of_bytes(b"local verifier");
        let f = Fixture::unit();
        let install = Installation::begin(&root.0, &state, build).unwrap();
        assert!(
            check(&f, &install.verifier(), build)
                .0
                .violations
                .is_empty()
        );
        let published = installation_products(&root.0, b"published products");
        let other = installation_products(&root.0, b"another writer's products");
        install.finish(published).unwrap();
        assert!(Installation::registered(&root.0, &state, build, Arc::default()).is_none());
        // The root holding the published header again is what was registered.
        let store = Store::new(root.0.clone());
        store
            .associate(published.0, published.1.action, published.1.reference)
            .unwrap();
        let registered = Installation::registered(&root.0, &state, build, Arc::default())
            .expect("the published header is registered")
            .verifier;
        assert!(!check(&f, &registered, build).1);
        assert_ne!(other, published);
    }
    #[test]
    fn cooperating_precomputes_of_one_root_do_not_interleave() {
        let root = Directory::new();
        let state = Directory::new();
        let state = state.0.join("private");
        let build = ContentHash::of_bytes(b"local verifier");
        let f = Fixture::unit();
        let first = Installation::begin(&root.0, &state, build).unwrap();
        let (began, second_began) = std::sync::mpsc::channel();
        let second = {
            let (root, state) = (root.0.clone(), state.clone());
            std::thread::spawn(move || {
                let second = Installation::begin(&root, &state, build).unwrap();
                began.send(()).unwrap();
                let header = installation_products(&root, b"second products");
                second.finish(header).unwrap();
            })
        };
        let header = installation_products(&root.0, b"first products");
        assert!(check(&f, &first.verifier(), build).0.violations.is_empty());
        // Between the first run's product publication and its registration,
        // the second run has not captured the root.
        assert!(
            second_began
                .recv_timeout(std::time::Duration::from_millis(500))
                .is_err()
        );
        first.finish(header).unwrap();
        assert!(Installation::registered(&root.0, &state, build, Arc::default()).is_some());
        second_began.recv().unwrap();
        second.join().unwrap();
        assert!(Installation::registered(&root.0, &state, build, Arc::default()).is_some());
    }

    /// V's frozen environment over a fixture's signatures.
    fn frozen(f: &Fixture) -> crate::ViewTable {
        crate::ViewTable::new(
            &session(),
            f.signatures.values().cloned().map(Arc::new),
            None,
            &mut crate::ValidatedSignatures::default(),
            None,
        )
        .unwrap()
    }
    /// V under that frozen environment: report, whether V ran, acceptance, reads.
    fn frozen_check(
        f: &Fixture,
        cache: &VerifierCache,
        policy: ContentHash,
        views: &crate::ViewTable,
    ) -> (Validation, bool, Option<AcceptedArtifact>, Vec<VRead>) {
        let mut reads = vec![];
        let (report, ran, accepted) = validate_with_receipt(
            &artifact(f),
            crate::SignatureInputs::Checked(views),
            policy,
            Some(cache),
            &mut reads,
        )
        .unwrap();
        (report, ran, accepted, reads)
    }
    /// The previous generation an installation's verifier may adopt from.
    fn previous(install: &Installation) -> Arc<installation::Rows> {
        let Backend::Hosted(_, _, Some(previous)) = &install.verifier().0 else {
            panic!("no previous generation")
        };
        previous.clone()
    }
    /// A private root and state, with products published once and kept.
    fn precompute_root() -> (Directory, Directory, std::path::PathBuf, ContentHash) {
        let root = Directory::new();
        let state = Directory::new();
        let private = state.0.join("private");
        installation_products(&root.0, b"products");
        (
            root,
            state,
            private,
            ContentHash::of_bytes(b"local verifier"),
        )
    }
    fn generations(state: &std::path::Path) -> Vec<std::path::PathBuf> {
        std::fs::read_dir(state)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.is_dir())
            .collect()
    }
    /// A Precompute whose receipt V issued in the previous generation adopts it
    /// only after V's own checks, and the result is what a forced fresh V over
    /// the same frozen inputs issues. The adopted closure stands alone.
    #[test]
    fn a_precompute_adopts_its_previous_generations_receipt_as_fresh_v_issues_it() {
        let (root, _state, state, build) = precompute_root();
        let f = Fixture::call();
        let views = frozen(&f);
        let first = Installation::begin(&root.0, &state, build).unwrap();
        assert!(frozen_check(&f, &first.verifier(), build, &views).1);
        first.finish(header(&root.0)).unwrap();
        let old = generations(&state);

        let second = Installation::begin(&root.0, &state, build).unwrap();
        let (report, ran, accepted, reads) = frozen_check(&f, &second.verifier(), build, &views);
        assert!(!ran, "the unchanged receipt is adopted");
        let (fresh, fresh_ran, fresh_accepted, fresh_reads) =
            frozen_check(&f, &VerifierCache::default(), build, &views);
        assert!(fresh_ran && fresh.violations.is_empty() && report.violations.is_empty());
        assert_eq!(accepted.unwrap(), fresh_accepted.unwrap());
        assert_eq!(reads, fresh_reads);
        second.finish(header(&root.0)).unwrap();

        // The new generation holds the whole closure: reopen it with the old
        // generation gone and empty fact tables.
        for directory in old {
            std::fs::remove_dir_all(directory).unwrap();
        }
        let reopened = Installation::registered(&root.0, &state, build, Arc::default())
            .unwrap()
            .verifier;
        assert!(!frozen_check(&f, &reopened, build, &frozen(&f)).1);
    }
    /// Adoption runs V's ordinary hit checks first: a changed read, a missing
    /// original-receipt association or a forged registration runs V, and the
    /// fresh receipt is still retained for the next generation.
    #[test]
    fn a_previous_receipt_whose_checks_fail_is_not_adopted_and_v_runs() {
        let (root, _state, state, build) = precompute_root();
        let f = Fixture::call();
        let first = Installation::begin(&root.0, &state, build).unwrap();
        let (_, _, accepted, _) = frozen_check(&f, &first.verifier(), build, &frozen(&f));
        first.finish(header(&root.0)).unwrap();
        let action = accepted.unwrap().receipt;

        // The callee's view changes; the artifact itself does not.
        let mut changed = Fixture::call();
        let callee = changed.signatures.get_mut(&path("callee")).unwrap();
        *callee = crate::support::function_signature(
            "callee",
            vec![(
                hirsel_types::PassingMode::Borrow,
                hirsel_types::TypeKind::Primitive(hirsel_types::Primitive::String),
            )],
            hirsel_types::TypeKind::Tuple(vec![].into()),
        );
        assert_eq!(artifact(&changed).identity, artifact(&f).identity);
        let install = Installation::begin(&root.0, &state, build).unwrap();
        assert!(frozen_check(&changed, &install.verifier(), build, &frozen(&changed)).1);
        drop(install);

        let install = Installation::begin(&root.0, &state, build).unwrap();
        previous(&install).forget(original_receipt_key(action));
        let (report, ran, accepted, _) = frozen_check(&f, &install.verifier(), build, &frozen(&f));
        assert!(ran && report.violations.is_empty() && accepted.is_some());
        install.finish(header(&root.0)).unwrap();
        let next = Installation::begin(&root.0, &state, build).unwrap();
        assert!(!frozen_check(&f, &next.verifier(), build, &frozen(&f)).1);
        next.finish(header(&root.0)).unwrap();

        let path = installation::registration_path(
            &state.canonicalize().unwrap(),
            &root.0.canonicalize().unwrap(),
        );
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[0] ^= 1;
        std::fs::write(&path, bytes).unwrap();
        let forged = Installation::begin(&root.0, &state, build).unwrap();
        assert!(frozen_check(&f, &forged.verifier(), build, &frozen(&f)).1);
    }
    /// A definition output is adopted with its replay closure and original
    /// receipt, equal to the output V published, and reopens on its own.
    #[test]
    fn a_precompute_adopts_a_previous_definition_output_with_its_closure() {
        let (root, _state, state, build) = precompute_root();
        let mut f = Fixture::unit();
        owner_sig(&mut f, &[]);
        let definition = Arc::new(f.definition.clone());
        let views = frozen(&f);
        let subject = ContentHash::of_bytes(b"complete lower output and action");
        let publish = |cache: &VerifierCache| {
            let (_, _, _, evidence) = validate_definition(
                &definition,
                session(),
                |_| Some((path("main"), 0)),
                &views as &dyn crate::CheckedEnvironment,
                build,
                Some(cache),
            )
            .unwrap();
            cache
                .publish_definition(
                    subject,
                    &definition,
                    session(),
                    build,
                    &views as &dyn crate::CheckedEnvironment,
                    &evidence.unwrap(),
                    b"query metadata",
                )
                .unwrap()
        };
        let first = Installation::begin(&root.0, &state, build).unwrap();
        let published = publish(&first.verifier());
        first.finish(header(&root.0)).unwrap();
        let old = generations(&state);

        let second = Installation::begin(&root.0, &state, build).unwrap();
        let get = |cache: &VerifierCache, views: &crate::ViewTable| {
            cache.definition_output(
                subject,
                definition.path,
                &session(),
                build,
                views as &dyn crate::CheckedEnvironment,
            )
        };
        assert!(
            second
                .verifier()
                .definition_output(subject, definition.path, &session(), build, &f.signatures)
                .is_none(),
            "a raw lookup adopts nothing"
        );
        let adopted = get(&second.verifier(), &views).expect("adopted output");
        assert_eq!(adopted, published);
        assert_eq!(
            adopted,
            publish(&VerifierCache::default()),
            "forced fresh V agrees"
        );
        second.finish(header(&root.0)).unwrap();
        for directory in old {
            std::fs::remove_dir_all(directory).unwrap();
        }
        let reopened = Installation::registered(&root.0, &state, build, Arc::default())
            .unwrap()
            .verifier;
        let output = get(&reopened, &frozen(&f)).expect("the closure stands alone");
        assert!(
            reopened
                .read_definition(
                    &output,
                    session(),
                    |_, _| Some(Anchor::default()),
                    build,
                    &f.signatures
                )
                .is_some()
        );
    }
    /// Execution evidence is adopted with every artifact's original receipt
    /// and replay closure, all or nothing: failing any one copy, including a
    /// grandchild type component, or forging any one product, exposes nothing
    /// and leaves a miss; the full closure then reopens on its own.
    #[test]
    fn execution_adoption_is_all_or_nothing_at_every_copy() {
        let (root, _state, state, build) = precompute_root();
        let mut f = Fixture::unit();
        // A local of `((u8,),)`: its type closure reaches a grandchild.
        let byte = session().intern(crate::support::ty(hirsel_types::TypeKind::Primitive(
            hirsel_types::Primitive::U8,
        )));
        let inner = session().intern(crate::support::ty(hirsel_types::TypeKind::Tuple(
            vec![byte].into(),
        )));
        f.slot(
            hirsel_types::TypeKind::Tuple(vec![inner].into()),
            hirsel_tir::Storage::Temporary,
        );
        owner_sig(&mut f, &[]);
        f.body().blocks[0].span.anchor = Anchor::fresh();
        let site = f.definition.bodies[0].blocks[0].span.anchor;
        let views = frozen(&f);
        let package = path("main").0.package;
        let replay = CheckedReplay {
            package: &package,
            views: &views,
        };
        let first = Installation::begin(&root.0, &state, build).unwrap();
        let (_, _, accepted, _) = frozen_check(&f, &first.verifier(), build, &views);
        let receipt = ExecutionReceipt::compose(
            &[(package.clone(), views.input_digest())],
            &[accepted.unwrap()],
        )
        .unwrap();
        let subject = receipt.identity().unwrap();
        first.verifier().publish_execution(subject, &receipt);
        first.finish(header(&root.0)).unwrap();
        let old = generations(&state);
        let closure = |cache: &VerifierCache| {
            [
                execution_key(subject),
                original_receipt_key(receipt.artifacts[0].receipt),
                replay_key(receipt.artifacts[0].artifact, build),
            ]
            .map(|key| match &cache.0 {
                Backend::Hosted(rows, ..) => rows.row(key).is_some(),
                _ => unreachable!(),
            })
        };
        let revalidated = |cache: &VerifierCache| {
            cache
                .execution_revalidated(subject, session(), |_, _| Some(site), &replay)
                .ok()
                .flatten()
        };

        adopt::ADOPTED.with(|log| log.borrow_mut().clear());
        adopt::TYPE_EDGES.with(|log| log.borrow_mut().clear());
        let probe = Installation::begin(&root.0, &state, build).unwrap();
        assert!(revalidated(&probe.verifier()).is_some());
        drop(probe);
        // The execution closure is the first adoption; V's revalidation then
        // adopts its own receipt, whose refusal only makes V run fresh.
        let copies = adopt::ADOPTED.with(|log| log.borrow()[0]);
        let edges = adopt::TYPE_EDGES.with(|log| log.borrow().clone());
        assert!(
            edges
                .iter()
                .any(|&(_, child)| edges.iter().any(|&(parent, _)| parent == child)),
            "the closure reaches a grandchild type component"
        );
        for n in 0..copies {
            for fault in [&adopt::FAIL_COPY, &adopt::FORGE_COPY] {
                let install = Installation::begin(&root.0, &state, build).unwrap();
                fault.set(Some(n));
                let result = revalidated(&install.verifier());
                fault.set(None);
                assert!(result.is_none(), "copy {n} refused: a miss");
                assert_eq!(
                    closure(&install.verifier()),
                    [false; 3],
                    "copy {n}: nothing exposed"
                );
            }
        }

        let install = Installation::begin(&root.0, &state, build).unwrap();
        assert!(revalidated(&install.verifier()).is_some());
        assert_eq!(closure(&install.verifier()), [true; 3]);
        install.finish(header(&root.0)).unwrap();
        for directory in old {
            std::fs::remove_dir_all(directory).unwrap();
        }
        let reopened = Installation::registered(&root.0, &state, build, Arc::default())
            .unwrap()
            .verifier;
        assert!(revalidated(&reopened).is_some(), "the closure stands alone");
    }
}
