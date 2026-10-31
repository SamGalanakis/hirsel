//! Shared ordinary Lower owners and explicit checked archive inputs. Raw
//! ownership grants no V/native acceptance; source and policy checks stay local.
use crate::{
    DefinitionQueries, NativeRecordContext, NativeRecordSource, QueryEngine, QueryError,
    SignatureSnapshot, Verify,
};
use hirsel_syntax::{ContentHash, DefPath};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};

/// A settled front Lower result. The ordinary payload is always available;
/// an optional opaque V output can avoid rechecking only in its own context.
/// These observations are data and grant no native C authority.
#[derive(Clone)]
pub struct NativeOriginalProduct {
    pub definition: DefPath,
    pub implementation_revision: ContentHash,
    pub signature: ContentHash,
    pub body: NativeOriginalHandle,
    pub verified: Option<hirsel_validate::VerifiedDefinitionOutput>,
}

impl NativeOriginalProduct {
    /// Complete ordinary identity across Lower eviction and recomputation.
    /// Source binding and V acceptance still belong to the context's reader.
    pub fn same_binding(&self, other: &Self) -> bool {
        self.definition == other.definition
            && self.implementation_revision == other.implementation_revision
            && self.signature == other.signature
            && self.verified == other.verified
            && match (&self.body.input, &other.body.input) {
                (OriginalInput::Bound(a), OriginalInput::Bound(b)) => {
                    a.reference == b.reference && a.context.bindings_hold(&b.context)
                }
                (OriginalInput::Archive(a), OriginalInput::Archive(b)) => {
                    a.reference() == b.reference()
                }
                (OriginalInput::Scalar(a), OriginalInput::Scalar(b)) => Arc::ptr_eq(a, b),
                _ => false,
            }
    }
}

/// An immutable source binding. Catalogue rows retain no decoded Lower body.
#[derive(Clone)]
pub struct NativeOriginalHandle {
    input: OriginalInput,
    _admission: Option<Arc<OriginalLease>>,
}
#[derive(Clone)]
enum OriginalInput {
    Bound(Arc<BoundOriginal>),
    Scalar(Arc<NativeScalarLower>),
    Archive(hirsel_product_codec::CheckedTir),
}
struct BoundOriginal {
    definition: hirsel_syntax::DefId,
    live: crate::runtime::WeakLowerProduct,
    reference: hirsel_artifact_store::Reference,
    context: Arc<NativeRecordContext>,
    source: Arc<dyn Send + Sync>,
    records: crate::NativeRecords,
    store: Arc<dyn crate::store::ResultStore>,
    working_bytes: u64,
    implementation_revision: ContentHash,
    signature: ContentHash,
    verified: Option<hirsel_validate::VerifiedDefinitionOutput>,
}
#[derive(Clone)]
pub(crate) struct OriginalAdmission {
    pub _allowance: Arc<crate::runtime::MemoryReservation>,
    pub remaining: Arc<Mutex<u64>>,
}
struct OriginalLease {
    admission: OriginalAdmission,
    bytes: u64,
}
impl Drop for OriginalLease {
    fn drop(&mut self) {
        *self
            .admission
            .remaining
            .lock()
            .unwrap_or_else(|e| e.into_inner()) += self.bytes;
    }
}
impl OriginalAdmission {
    fn reserve(&self, bytes: u64) -> Option<Arc<OriginalLease>> {
        let mut remaining = self.remaining.lock().unwrap_or_else(|e| e.into_inner());
        *remaining = remaining.checked_sub(bytes)?;
        Some(Arc::new(OriginalLease {
            admission: self.clone(),
            bytes,
        }))
    }
}
impl From<hirsel_product_codec::CheckedTir> for NativeOriginalHandle {
    fn from(value: hirsel_product_codec::CheckedTir) -> Self {
        Self {
            input: OriginalInput::Archive(value),
            _admission: None,
        }
    }
}
impl NativeOriginalHandle {
    pub(crate) fn bound(
        live: &crate::runtime::LiveLowerProduct,
        reference: hirsel_artifact_store::Reference,
        context: Arc<NativeRecordContext>,
        records: crate::NativeRecords,
        source: Arc<dyn Send + Sync>,
        working_bytes: u64,
    ) -> Self {
        let observations = live.observations();
        Self {
            input: OriginalInput::Bound(Arc::new(BoundOriginal {
                definition: live.output().value.path,
                live: live.downgrade(),
                reference,
                context,
                records,
                source,
                store: observations.store().clone(),
                working_bytes,
                implementation_revision: observations.implementation_revision,
                signature: observations.signature,
                verified: observations.verified.clone(),
            })),
            _admission: None,
        }
    }
    /// Upgrade only an already admitted live payload. The caller pins it.
    pub fn live(&self) -> Option<crate::runtime::LiveLowerProduct> {
        match &self.input {
            OriginalInput::Bound(input) => input.live.upgrade(),
            _ => None,
        }
    }
    /// Catalogue backing charged once, independently of any scoped body lease.
    pub fn retained_metadata_bytes(&self) -> Option<u64> {
        let mut weight = crate::NativeMetadataWeight::default();
        weight.add(size_of::<NativeOriginalProduct>() + 2 * size_of::<usize>());
        if let OriginalInput::Bound(input) = &self.input {
            weight.add(size_of::<BoundOriginal>() + 2 * size_of::<usize>());
            weight.add(size_of::<OriginalLease>() + 2 * size_of::<usize>());
            weight.path(input.definition.path());
            weight.add(crate::runtime::WeakLowerProduct::metadata_bytes());
            weight.add(size_of::<NativeRecordContext>() + 2 * size_of::<usize>());
            if let Some(output) = &input.verified {
                weight.add(output.retained_bytes()?.try_into().ok()?);
            }
        }
        Some(weight.bytes())
    }
    pub(crate) fn admit(mut self, admission: &OriginalAdmission) -> Option<Self> {
        self._admission = Some(admission.reserve(self.retained_metadata_bytes()?)?);
        Some(self)
    }
    fn reference(&self) -> Option<hirsel_artifact_store::Reference> {
        match &self.input {
            OriginalInput::Bound(input) => Some(input.reference),
            OriginalInput::Archive(value) => Some(value.reference()),
            OriginalInput::Scalar(_) => None,
        }
    }
    fn open(&self, admission: Option<&OriginalAdmission>) -> Option<NativeOriginalValue> {
        match &self.input {
            OriginalInput::Scalar(value) => Some(NativeOriginalValue::Scalar(value.clone())),
            OriginalInput::Archive(value) => Some(NativeOriginalValue::Archive(value.clone())),
            OriginalInput::Bound(input) => {
                if let Some(live) = input.live.upgrade() {
                    return Some(NativeOriginalValue::Live(live));
                }
                let owner_bytes = size_of::<OriginalLease>()
                    + size_of::<(Arc<OriginalLease>, Arc<dyn Send + Sync>)>()
                    + 4 * size_of::<usize>();
                let lease =
                    admission?.reserve(input.working_bytes.checked_add(owner_bytes as u64)?)?;
                // Reserve before provider access, opening, or materialization.
                let bytes = input.store.get_content(input.reference)?;
                let archive = input.records.open_tir(
                    input.context.clone(),
                    bytes,
                    input.reference,
                    Arc::new((lease, input.source.clone())),
                )?;
                Some(NativeOriginalValue::Archive(archive))
            }
        }
    }
    fn matches(&self, product: &NativeOriginalProduct, context: &NativeRecordContext) -> bool {
        match &self.input {
            OriginalInput::Bound(input) => {
                input.definition.path() == &product.definition
                    && input.reference.domain == NATIVE_ORIGINAL_BODY
                    && input.context.bindings_hold(context)
                    && input.implementation_revision == product.implementation_revision
                    && input.signature == product.signature
                    && product
                        .verified
                        .as_ref()
                        .is_none_or(|output| input.verified.as_ref() == Some(output))
            }
            OriginalInput::Scalar(value) => {
                value.definition.path.path() == &product.definition
                    && value.definition.signature_hash == product.signature
                    && value.implementation_revision == product.implementation_revision
                    && value.signature == product.signature
                    && value._context.bindings_hold(context)
            }
            OriginalInput::Archive(value) => {
                value.header().is_some_and(|header| {
                    header.path.path() == &product.definition
                        && header.signature_hash == product.signature
                }) && value.matches_bindings(context.record_types(), |path, occurrence| {
                    context.record_source_anchor(path, occurrence)
                })
            }
        }
    }
}

/// Raw input with its original immutable owner. Neither variant grants V or C.
#[derive(Clone)]
pub enum NativeOriginalValue {
    Live(crate::runtime::LiveLowerProduct),
    Scalar(Arc<NativeScalarLower>),
    Archive(hirsel_product_codec::CheckedTir),
}
/// Scalar queries retain their complete memo, source map and exact context.
/// Construction belongs to the ordinary QueryEngine, never an importer.
pub struct NativeScalarLower {
    definition: hirsel_tir::DefinitionRef,
    _entry: Option<Arc<dyn Send + Sync>>,
    _sources: hirsel_span::SourceMap,
    _context: Arc<NativeRecordContext>,
    implementation_revision: ContentHash,
    signature: ContentHash,
}
impl NativeOriginalValue {
    pub fn view(&self) -> hirsel_product_codec::TirView<'_> {
        match self {
            Self::Live(value) => hirsel_product_codec::TirView::Live(&value.output().value),
            Self::Scalar(value) => hirsel_product_codec::TirView::Live(&value.definition),
            Self::Archive(value) => hirsel_product_codec::TirView::Archived(value),
        }
    }
    fn definition(&self) -> Option<hirsel_tir::DefinitionRef> {
        match self {
            Self::Live(value) => Some(value.output().value.clone()),
            Self::Scalar(value) => Some(value.definition.clone()),
            Self::Archive(value) => Some(Arc::new(value.materialize()?)),
        }
    }
}

/// Source bindings captured before Lower tasks run. This contains no source
/// Definition or Lower output; sparse readers consume only settled demands.
pub struct NativeOriginalContext {
    owner: Option<Arc<dyn Send + Sync>>,
    admission: Option<OriginalAdmission>,
    anchors: Arc<NativeRecordContext>,
    signatures: Arc<SignatureSnapshot>,
    cache: Option<Arc<hirsel_validate::VerifierCache>>,
    policy: ContentHash,
    instances: Arc<hirsel_tir::InstanceBodies>,
    expected: BTreeMap<DefPath, (ContentHash, ContentHash)>,
    verify: Verify,
    reuse: bool,
}
impl NativeOriginalContext {
    pub(crate) fn retain_owner(
        mut self,
        owner: Arc<dyn Send + Sync>,
        admission: OriginalAdmission,
    ) -> Self {
        self.admission = Some(admission);
        self.owner = Some(owner);
        self
    }

    /// Bind exact front observations without opening an ordinary source body.
    /// Unknown/stale source identities and duplicate demands are refused.
    pub fn reader(
        &self,
        products: impl IntoIterator<Item = NativeOriginalProduct>,
    ) -> Result<NativeOriginalReader, QueryError> {
        let mut selected = BTreeMap::new();
        let mut ordinary = BTreeMap::new();
        let mut seen = BTreeSet::new();
        for product in products {
            let path = product.definition.clone();
            if !seen.insert(path.clone())
                || self.expected.get(&path)
                    != Some(&(product.implementation_revision, product.signature))
                || !product.body.matches(&product, &self.anchors)
                || product
                    .verified
                    .as_ref()
                    .is_some_and(|output| output.owner().path() != &path)
            {
                return Err(source_transport(
                    "settled Lower differs from its source snapshot",
                ));
            }
            // Retain the raw owner independently of best-effort verifier storage.
            ordinary.insert(
                path.clone(),
                OrdinarySource {
                    value: product.body,
                    implementation_revision: product.implementation_revision,
                    signature: product.signature,
                },
            );
            if self.verify == Verify::On
                && self.reuse
                && self.cache.is_some()
                && let Some(output) = product.verified.filter(|output| {
                    output.environment_holds(
                        self.anchors.record_types(),
                        self.policy,
                        self.signatures.validation(&path.0.package),
                    )
                })
            {
                selected.insert(
                    path,
                    Selected {
                        output,
                        implementation_revision: product.implementation_revision,
                        signature: product.signature,
                    },
                );
            }
        }
        Ok(NativeOriginalReader {
            owner: self.owner.clone(),
            admission: self.admission.clone(),
            verified: NativeLowerReader {
                anchors: self.anchors.clone(),
                signatures: self.signatures.clone(),
                cache: self.cache.clone(),
                policy: self.policy,
                selected,
                instances: self.instances.clone(),
            },
            ordinary,
            verify: self.verify,
            reuse: self.reuse,
        })
    }
}
impl QueryEngine {
    /// Freeze current source/private observations without demanding Lower.
    pub fn native_original_context(
        &mut self,
        paths: &[DefPath],
    ) -> Result<NativeOriginalContext, QueryError> {
        let paths: BTreeSet<_> = paths.iter().cloned().collect();
        let mut expected = BTreeMap::new();
        for path in &paths {
            let syntax = self.parse_definition(path)?.value;
            let signature = self.item_signature(path)?;
            if crate::queries::has_errors(&signature.diagnostics) {
                return Err(QueryError::Diagnostics(signature.diagnostics));
            }
            if crate::queries::generation::executable(&syntax) {
                expected.insert(
                    path.clone(),
                    (syntax.text_hash, signature.value.fingerprint),
                );
            }
        }
        Ok(NativeOriginalContext {
            owner: None,
            admission: None,
            anchors: Arc::new(NativeRecordContext::capture(self)),
            signatures: self.signature_snapshot(),
            cache: self.product_store().and_then(|store| store.verifier()),
            policy: self.source_verification_policy(),
            instances: Arc::new(self.instance_bodies(&paths)),
            expected,
            verify: self.verify,
            reuse: self.native_query_policy().reuse,
        })
    }
}

struct Selected {
    output: hirsel_validate::VerifiedDefinitionOutput,
    implementation_revision: ContentHash,
    signature: ContentHash,
}
/// Source query inputs for native workers. There are no DefinitionRefs here;
/// the signature snapshot is the existing immutable independently checked view.
/// A handle cannot be supplied from arbitrary persisted associations or tokens.
pub struct NativeLowerReader {
    anchors: Arc<NativeRecordContext>,
    signatures: Arc<SignatureSnapshot>,
    cache: Option<Arc<hirsel_validate::VerifierCache>>,
    policy: ContentHash,
    selected: BTreeMap<DefPath, Selected>,
    instances: Arc<hirsel_tir::InstanceBodies>,
}
/// One demanded original source body, carrying only the actual selected V
/// output and source observations. It grants no native Value/Ready authority.
pub struct NativeLowerBody {
    definition: hirsel_tir::DefinitionRef,
    output: hirsel_validate::VerifiedDefinitionOutput,
    implementation_revision: ContentHash,
}
impl NativeLowerBody {
    pub fn definition(&self) -> &hirsel_tir::DefinitionRef {
        &self.definition
    }
    pub fn output(&self) -> &hirsel_validate::VerifiedDefinitionOutput {
        &self.output
    }
    pub fn implementation_revision(&self) -> ContentHash {
        self.implementation_revision
    }
}
impl NativeLowerReader {
    fn archive(
        &self,
        path: &DefPath,
        owner: Arc<dyn Send + Sync>,
    ) -> Option<hirsel_product_codec::CheckedTir> {
        let selected = self.selected.get(path)?;
        let archive = self.cache.as_ref()?.read_definition_archive(
            &selected.output,
            self.anchors.record_types().clone(),
            |owner, occurrence| self.anchors.record_source_anchor(owner, occurrence),
            self.policy,
            self.signatures.validation(&path.0.package),
            owner,
        )?;
        (archive.header()?.signature_hash == selected.signature).then_some(archive)
    }
    /// Only a selected current source query can provide this body's envelope.
    /// V rechecks its exact artifact and private input observations after type
    /// and source rebinding. CT5 uses are checked by the same source validator
    /// as the live native source attachment. No native stage is inferred.
    pub fn read(&self, path: &DefPath) -> Option<NativeLowerBody> {
        let selected = self.selected.get(path)?;
        let archive = self.archive(path, self.anchors.clone())?;
        let header = archive.header()?;
        if header.path.path() != path
            || header.signature_hash != selected.signature
            || !hirsel_validate::validate_instance_rows(
                self.anchors.record_types(),
                &self.instances,
                path,
                hirsel_product_codec::TirView::Archived(&archive),
            )?
            .is_empty()
        {
            return None;
        }
        Some(NativeLowerBody {
            definition: Arc::new(archive.materialize()?),
            output: selected.output.clone(),
            implementation_revision: selected.implementation_revision,
        })
    }
    /// Observations of a current source-query-selected envelope, without
    /// decoding its body. Query metadata alone is not a verification identity.
    pub fn input(
        &self,
        path: &DefPath,
    ) -> Option<(
        &hirsel_validate::VerifiedDefinitionOutput,
        ContentHash,
        ContentHash,
    )> {
        let selected = self.selected.get(path)?;
        Some((
            &selected.output,
            selected.implementation_revision,
            selected.signature,
        ))
    }
    /// An inventory lookup opens no source or native body.
    pub fn contains(&self, path: &DefPath) -> bool {
        self.selected.contains_key(path)
    }
}
impl QueryEngine {
    /// Capture source-query-selected envelopes without decoding source bodies.
    /// Availability is explicit: VerifyOff, recomputation policy, absent local
    /// verifier transport returns None. An unavailable individual envelope
    /// remains absent from the reader and cannot be materialized.
    /// The ordinary source producer must run for such a miss; no proof is
    /// reconstructed from ordinary product-store bytes.
    pub fn native_lower_reader(
        &mut self,
        paths: &[DefPath],
    ) -> Result<Option<NativeLowerReader>, QueryError> {
        if self.verify != Verify::On || !self.native_query_policy().reuse {
            return Ok(None);
        }
        let Some(cache) = self.product_store().and_then(|store| store.verifier()) else {
            return Ok(None);
        };
        let mut selected = BTreeMap::new();
        let paths: BTreeSet<_> = paths.iter().cloned().collect();
        for path in &paths {
            let source = self.native_definition_product(path)?;
            if !crate::queries::generation::executable(&source.source.syntax) {
                continue;
            }
            let Some(output) = source.lower else {
                continue;
            };
            selected.insert(
                path.clone(),
                Selected {
                    output,
                    implementation_revision: source.source.implementation_revision,
                    signature: source.signature.fingerprint,
                },
            );
        }
        Ok(Some(NativeLowerReader {
            anchors: Arc::new(NativeRecordContext::capture(self)),
            signatures: self.signature_snapshot(),
            cache: Some(cache),
            policy: self.source_verification_policy(),
            selected,
            instances: Arc::new(self.instance_bodies(&paths)),
        }))
    }
}

/// Selected raw data and its immutable owner, independent of V authority.
struct OrdinarySource {
    value: NativeOriginalHandle,
    implementation_revision: ContentHash,
    signature: ContentHash,
}
pub const NATIVE_ORIGINAL_BODY: hirsel_artifact_store::Domain =
    hirsel_product_codec::CheckedTir::DOMAIN;
pub struct NativeOriginalReader {
    owner: Option<Arc<dyn Send + Sync>>,
    admission: Option<OriginalAdmission>,
    verified: NativeLowerReader,
    ordinary: BTreeMap<DefPath, OrdinarySource>,
    verify: Verify,
    reuse: bool,
}
pub struct NativeOriginalBody {
    _owner: Option<Arc<dyn Send + Sync>>,
    _raw: Option<NativeOriginalValue>,
    definition: hirsel_tir::DefinitionRef,
    output: Option<hirsel_validate::VerifiedDefinitionOutput>,
    implementation_revision: ContentHash,
    verification_ran: bool,
}
impl std::ops::Deref for NativeOriginalBody {
    type Target = hirsel_tir::Definition;
    fn deref(&self) -> &Self::Target {
        &self.definition
    }
}
impl NativeOriginalBody {
    pub fn definition(&self) -> &hirsel_tir::DefinitionRef {
        &self.definition
    }
    pub fn output(&self) -> Option<&hirsel_validate::VerifiedDefinitionOutput> {
        self.output.as_ref()
    }
    pub fn implementation_revision(&self) -> ContentHash {
        self.implementation_revision
    }
    pub fn verification_ran(&self) -> bool {
        self.verification_ran
    }
}
impl NativeOriginalReader {
    /// The owning immutable signature/private-view snapshot, for binding
    /// settled source metadata without decoding an original body.
    pub fn signatures(&self) -> &Arc<SignatureSnapshot> {
        &self.verified.signatures
    }
    /// Current CT-5 catalogue captured with this original source snapshot.
    /// It contains selected declaration/actual/path data, never native code or
    /// a substitute source/V witness for the selected monomorphic definition.
    pub fn instance_bodies(&self) -> &hirsel_tir::InstanceBodies {
        &self.verified.instances
    }

    /// Copied reader maps and path buffers only. Shared source/signature/V,
    /// interner, CT-5 and store payloads retain their original admission owners.
    pub fn retained_metadata_bytes(&self) -> u64 {
        let mut weight = crate::NativeMetadataWeight::default();
        weight.add(std::mem::size_of::<Self>());
        weight.map(&self.verified.selected, |weight, path, _| weight.path(path));
        weight.map(&self.ordinary, |weight, path, source| {
            weight.path(path);
            if source.value._admission.is_none()
                && matches!(&source.value.input, OriginalInput::Bound(_))
            {
                weight.add(
                    source
                        .value
                        .retained_metadata_bytes()
                        .and_then(|bytes| usize::try_from(bytes).ok())
                        .unwrap_or(usize::MAX),
                );
            }
            if let OriginalInput::Scalar(value) = &source.value.input {
                weight.arc(value);
                weight.source_snapshot(&value._sources);
            }
        });
        weight.bytes()
    }

    pub fn record_context(&self) -> Arc<NativeRecordContext> {
        self.verified.anchors.clone()
    }
    pub fn verification(&self) -> Verify {
        self.verify
    }
    pub fn query_policy(&self) -> crate::NativeQueryPolicy {
        crate::NativeQueryPolicy {
            source: self.verified.policy,
            reuse: self.reuse && !self.has_ordinary(),
        }
    }

    pub fn input(
        &self,
        path: &DefPath,
    ) -> Option<(
        Option<&hirsel_validate::VerifiedDefinitionOutput>,
        Option<hirsel_artifact_store::Reference>,
        ContentHash,
        ContentHash,
    )> {
        if let Some((output, revision, signature)) = self.verified.input(path) {
            return Some((Some(output), None, revision, signature));
        }
        let input = self.ordinary.get(path)?;
        Some((
            None,
            input.value.reference(),
            input.implementation_revision,
            input.signature,
        ))
    }
    /// Raw selected input retaining its original owner, without V acceptance.
    pub fn raw(&self, path: &DefPath) -> Option<NativeOriginalValue> {
        if let Some(source) = self.ordinary.get(path) {
            return source.value.open(self.admission.as_ref());
        }
        let retained: Arc<dyn Send + Sync> =
            Arc::new((self.verified.anchors.clone(), self.owner.clone()));
        self.verified
            .archive(path, retained)
            .map(NativeOriginalValue::Archive)
    }
    pub fn has_ordinary(&self) -> bool {
        self.ordinary
            .keys()
            .any(|path| !self.verified.contains(path))
    }
    pub fn read(&self, path: &DefPath) -> Option<NativeOriginalBody> {
        let Some(source) = self.ordinary.get(path) else {
            let body = self.verified.read(path)?;
            return Some(NativeOriginalBody {
                _owner: self.owner.clone(),
                _raw: None,
                definition: body.definition,
                output: Some(body.output),
                implementation_revision: body.implementation_revision,
                verification_ran: false,
            });
        };
        let raw = source.value.open(self.admission.as_ref())?;
        let view = raw.view();
        let header = view.header()?;
        if header.path.path() != path
            || header.signature_hash != source.signature
            || !hirsel_validate::validate_instance_rows(
                self.verified.anchors.record_types(),
                &self.verified.instances,
                path,
                view,
            )?
            .is_empty()
        {
            return None;
        }
        let output = if self.verify == Verify::On && self.reuse {
            if let Some((output, _, _)) = self.verified.input(path) {
                let matches = match &raw {
                    NativeOriginalValue::Archive(archive) => output.matches_archive(
                        archive,
                        self.verified.anchors.record_types().clone(),
                        |anchor| self.verified.anchors.record_owner(anchor),
                        self.verified.policy,
                        self.verified.signatures.validation(&path.0.package),
                    ),
                    _ => output.matches_definition(
                        &raw.definition()?,
                        self.verified.anchors.record_types().clone(),
                        |anchor| self.verified.anchors.record_owner(anchor),
                        self.verified.policy,
                        self.verified.signatures.validation(&path.0.package),
                    ),
                };
                if !matches {
                    return None;
                }
                Some(output.clone())
            } else {
                None
            }
        } else {
            None
        };
        let verification_ran = if output.is_none() && self.verify == Verify::On {
            let _time = hirsel_intern::timings::phase(hirsel_intern::timings::Phase::Verify);
            let artifact = match &raw {
                NativeOriginalValue::Archive(archive) => hirsel_validate::Artifact::archived(
                    archive,
                    self.verified.anchors.record_types().clone(),
                    |anchor| self.verified.anchors.record_owner(anchor),
                )?,
                NativeOriginalValue::Live(live) => hirsel_validate::Artifact::new(
                    &live.output().value,
                    self.verified.anchors.record_types().clone(),
                    |anchor| self.verified.anchors.record_owner(anchor),
                ),
                NativeOriginalValue::Scalar(value) => hirsel_validate::Artifact::new(
                    &value.definition,
                    self.verified.anchors.record_types().clone(),
                    |anchor| self.verified.anchors.record_owner(anchor),
                ),
            };
            let (report, ran, _) = hirsel_validate::validate_with_receipt(
                &artifact,
                self.verified.signatures.validation(&path.0.package),
                self.verified.policy,
                self.verified.cache.as_deref(),
                &mut vec![],
            );
            if ran {
                hirsel_intern::timings::count(hirsel_intern::timings::Count::VRuns);
            }
            if hirsel_validate::tooling_limit_error(&report).is_some()
                || !report.violations.is_empty()
            {
                return None;
            }
            ran
        } else {
            false
        };
        Some(NativeOriginalBody {
            _owner: self.owner.clone(),
            definition: raw.definition()?,
            _raw: Some(raw),
            output,
            implementation_revision: source.implementation_revision,
            verification_ran,
        })
    }
}

impl QueryEngine {
    /// Retain ordinary Lower values with their exact scalar memo/source owners.
    /// VerifyOff/recomputation/absent verifier never manufacture a V token.
    pub fn native_original_reader(
        &mut self,
        paths: &[DefPath],
    ) -> Result<NativeOriginalReader, QueryError> {
        let cache = self.product_store().and_then(|store| store.verifier());
        let mut selected = BTreeMap::new();
        let mut ordinary = BTreeMap::new();
        let paths: BTreeSet<_> = paths.iter().cloned().collect();
        for path in &paths {
            let source = self.native_definition_product(path)?;
            if !crate::queries::generation::executable(&source.source.syntax) {
                continue;
            }
            let revision = source.source.implementation_revision;
            let signature = source.signature.fingerprint;
            let (definition, output) = self.native_definition_body_with_output(&source)?;
            if self.verify == Verify::On
                && self.native_query_policy().reuse
                && cache.is_some()
                && let Some(output) = output
            {
                selected.insert(
                    path.clone(),
                    Selected {
                        output,
                        implementation_revision: revision,
                        signature,
                    },
                );
            }
            let context = Arc::new(NativeRecordContext::capture(self));
            ordinary.insert(
                path.clone(),
                OrdinarySource {
                    value: NativeOriginalHandle {
                        input: OriginalInput::Scalar(Arc::new(NativeScalarLower {
                            definition,
                            _entry: self.native_lower_owner(path),
                            _sources: self.sources.clone(),
                            _context: context,
                            implementation_revision: revision,
                            signature,
                        })),
                        _admission: None,
                    },
                    implementation_revision: revision,
                    signature,
                },
            );
        }
        Ok(NativeOriginalReader {
            owner: None,
            admission: None,
            verified: NativeLowerReader {
                anchors: Arc::new(NativeRecordContext::capture(self)),
                signatures: self.signature_snapshot(),
                cache,
                policy: self.source_verification_policy(),
                selected,
                instances: Arc::new(self.instance_bodies(&paths)),
            },
            ordinary,
            verify: self.verify,
            reuse: self.native_query_policy().reuse,
        })
    }
}
fn source_transport(message: &str) -> QueryError {
    QueryError::Operational(hirsel_types::OperationalFailure {
        operation: "native original source transport".into(),
        message: message.into(),
    })
}

impl QueryEngine {
    /// Revalidate an admitted native snapshot into a reader with private V storage.
    /// This also supports callers whose ordinary compilation retains no products.
    pub(crate) fn native_snapshot_reader(
        &mut self,
        input: &crate::NativeInput,
    ) -> Result<NativeOriginalReader, QueryError> {
        if self.verify != Verify::On {
            return Err(source_transport("sealed source snapshots require V"));
        }
        let cache = Arc::new(hirsel_validate::VerifierCache::default());
        let anchors = Arc::new(NativeRecordContext::capture(self));
        let signatures = self.signature_snapshot();
        let policy = self.source_verification_policy();
        let mut selected = BTreeMap::new();
        for (path, definition) in &input.definitions {
            let view = signatures.validation(&path.0.package);
            let (report, _, _, evidence) = hirsel_validate::validate_definition(
                definition,
                input.types.clone(),
                |anchor| anchors.record_owner(anchor),
                view,
                policy,
                Some(cache.as_ref()),
            );
            if !report.violations.is_empty() {
                return Err(source_transport(&format!(
                    "sealed source snapshot validation: {:?}",
                    report.violations
                )));
            }
            let output = cache
                .definition_output_from_evidence(
                    definition,
                    input.types.clone(),
                    policy,
                    view,
                    &evidence.ok_or_else(|| {
                        source_transport("sealed source snapshot lacks V evidence")
                    })?,
                )
                .ok_or_else(|| source_transport("sealed source snapshot lacks V output"))?;
            selected.insert(
                path.clone(),
                Selected {
                    output,
                    implementation_revision: input.source_owners[path].implementation_revision,
                    signature: input.signatures[path].fingerprint,
                },
            );
        }
        Ok(NativeOriginalReader {
            owner: None,
            admission: None,
            verified: NativeLowerReader {
                anchors,
                signatures,
                cache: Some(cache),
                policy,
                selected,
                instances: Arc::new(input.instance_bodies.clone()),
            },
            ordinary: BTreeMap::new(),
            verify: self.verify,
            reuse: true,
        })
    }
}
