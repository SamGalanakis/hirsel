//! V's input: the signature views it reads, each identified by V's own digest,
//! and the reads it records against them.
//!
//! V never trusts the producer's fingerprint. Every view V reads is identified
//! by a `ValidationDigest` that this crate computes over every field of the
//! view, private shape included, so a producer that leaves a field out of its
//! fingerprint (a malformed row, a private field) still changes the digest.
//! Every lookup V makes is recorded as a `VRead`: a view by digest, a missing
//! signature, the selected heap role's selection, and the universe's errors. A
//! verdict is reusable exactly while its reads still hold (`read_holds`).
use super::*;
use hirsel_syntax::{ContentHash, DefId};
use hirsel_types::{HeapRole, SignatureKind, TypeInterner};
use std::cell::{OnceCell, RefCell};
use std::sync::{Arc, OnceLock};

/// V's identity of one signature view: a hash over every field it holds.
#[derive(
    serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd,
)]
pub struct ValidationDigest(pub ContentHash);

/// A canonical path whose in-memory names belong to the session interner.
/// The codec always writes the complete path, including package and version,
/// independently of any surrounding process-local definition numbering scope.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct HeapPath(DefId);
impl HeapPath {
    pub fn path(&self) -> &'static hirsel_syntax::DefPath {
        self.0.path()
    }
}
impl serde::Serialize for HeapPath {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serde::Serialize::serialize(self.path(), serializer)
    }
}
impl<'de> serde::Deserialize<'de> for HeapPath {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let path = <hirsel_syntax::DefPath as serde::Deserialize>::deserialize(deserializer)?;
        Ok(Self(DefId::intern(&path)))
    }
}

/// Canonical evidence for the ambient heap in one exact signature environment.
#[derive(
    serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd,
)]
pub enum HeapObservation {
    None,
    SelectedHeap {
        arena_path: HeapPath,
        operation_path: HeapPath,
        arena_view_digest: ValidationDigest,
        operation_view_digest: ValidationDigest,
    },
}
impl HeapObservation {
    pub(crate) fn retained_payload_bytes(&self) -> Option<u64> {
        // Interned path backing is session-owned, as for ordinary V view reads.
        Some(0)
    }
    pub fn role(&self) -> Option<HeapRole> {
        match self {
            Self::None => None,
            Self::SelectedHeap {
                arena_path,
                operation_path,
                ..
            } => Some(HeapRole {
                arena: arena_path.path().clone(),
                operation: operation_path.path().clone(),
            }),
        }
    }
    pub fn fingerprint(&self) -> ContentHash {
        let bytes = bincode::serialize(self).expect("heap evidence encodes paths and digests");
        ContentHash::of_parts("hirsel.validate.heap-role.v1", [bytes.as_slice()])
    }
}

pub(crate) fn path_bytes(path: &hirsel_syntax::DefPath) -> Option<u64> {
    let item = &path.0;
    let bytes = item
        .package
        .name
        .capacity()
        .checked_add(item.package.version.capacity())?
        .checked_add(item.name.capacity())?
        .checked_add(item.modules.capacity().checked_mul(size_of::<String>())?)?;
    item.modules.iter().try_fold(bytes as u64, |bytes, module| {
        bytes.checked_add(module.capacity() as u64)
    })
}

pub(crate) fn read_payload_bytes(reads: &[VRead]) -> Option<u64> {
    reads.iter().try_fold(0u64, |bytes, read| {
        bytes.checked_add(match read {
            VRead::HeapRole(heap) => {
                (size_of::<HeapObservation>() as u64).checked_add(heap.retained_payload_bytes()?)?
            }
            _ => 0,
        })
    })
}

pub(crate) fn select_heap<'a>(
    roles: impl IntoIterator<Item = (&'a hirsel_syntax::DefPath, &'a hirsel_syntax::DefPath)>,
) -> Result<Option<HeapRole>, ViewRefusal> {
    let mut selected = None;
    for role in roles {
        if selected.is_some_and(|previous| previous != role) {
            return Err(ViewRefusal::Stale {
                definition: DefId::intern(role.0),
            });
        }
        selected = Some(role);
    }
    Ok(selected.map(|(arena, operation)| HeapRole {
        arena: arena.clone(),
        operation: operation.clone(),
    }))
}

pub(crate) fn observe_heap(
    types: Option<&TypeInterner>,
    signatures: &dyn SignatureLookup,
) -> Result<HeapObservation, ViewRefusal> {
    heap_evidence(
        types,
        signatures,
        |_| {},
        |t, d, s, lookup| check_view(t, d, s, lookup),
    )
}
pub(crate) fn prepare_heap(
    signatures: &dyn SignatureLookup,
    store: &mut ValidatedSignatures,
    old: Option<&ValidatedSignatures>,
) -> Result<HeapObservation, ViewRefusal> {
    heap_evidence(
        None,
        signatures,
        |_| {},
        |t, d, _, lookup| store.verdict_from(t, d, lookup, old).map(|v| (*v).clone()),
    )
}
fn heap_evidence(
    types: Option<&TypeInterner>,
    signatures: &dyn SignatureLookup,
    mut record: impl FnMut(VRead),
    mut verdict: impl FnMut(
        &TypeInterner,
        DefId,
        &Signature,
        &dyn SignatureLookup,
    ) -> Result<ViewVerdict, ViewRefusal>,
) -> Result<HeapObservation, ViewRefusal> {
    let Some(role) = signatures.heap_role()? else {
        return Ok(HeapObservation::None);
    };
    let arena = DefId::intern(&role.arena);
    let operation = DefId::intern(&role.operation);
    let evidence = |d| {
        signatures
            .view(d)?
            .ok_or(ViewRefusal::Stale { definition: d })
    };
    let a = evidence(arena)?;
    let o = evidence(operation)?;
    if !std::ptr::eq(a.types, o.types) || types.is_some_and(|t| !a.for_types(t)) {
        return Err(ViewRefusal::Stale { definition: arena });
    }
    let a_signature = a.signature()?;
    let o_signature = o.signature()?;
    let arena_valid = matches!(&a_signature.kind,
        SignatureKind::Type(declaration) if declaration.generics.is_empty());
    let operation_valid = matches!(&o_signature.kind,
        SignatureKind::Function(function) if function.generics.is_empty()
            && function.parameters.is_empty() && function.error.is_none()
            && a.types.get(function.result).is_some_and(|ty|
                ty.allocation_target == Some(hirsel_types::AllocationTarget::Heap)
                && matches!(&ty.kind, TypeKind::Named { definition, arguments }
                    if *definition == arena && arguments.is_empty())));
    if !arena_valid || a_signature.definition != arena {
        return Err(ViewRefusal::Stale { definition: arena });
    }
    if !operation_valid || o_signature.definition != operation {
        return Err(ViewRefusal::Stale {
            definition: operation,
        });
    }
    // Declaration evidence is checked here in the observing environment.
    for (d, signature) in [(arena, &*a_signature), (operation, &*o_signature)] {
        let verdict = verdict(a.types, d, signature, signatures)?;
        if verdict.error.is_some() {
            return Err(ViewRefusal::Stale { definition: d });
        }
        for read in verdict.reads {
            record(read);
        }
    }
    Ok(HeapObservation::SelectedHeap {
        arena_path: HeapPath(arena),
        operation_path: HeapPath(operation),
        arena_view_digest: a.digest(),
        operation_view_digest: o.digest(),
    })
}

/// An immutable signature independently identified by V. No producer digest
/// enters this constructor, and the signature cannot be edited through it.
///
/// A held signature keeps its payload. A sourced one keeps only its identity
/// (definition, digest and a weak handle) and reads the payload back from its
/// cache through a scoped view, so the cache can evict an unpinned row.
#[derive(Clone, Debug)]
pub struct IdentifiedSignature {
    types: Arc<TypeInterner>,
    definition: DefId,
    payload: Payload,
    digest: ValidationDigest,
    /// Part of the identity: universes select their selected heap role without
    /// reading every payload again.
    heap: Option<(HeapPath, HeapPath)>,
    retention: crate::memory::ControlRetention,
    payload_retention: crate::memory::ControlRetention,
}
/// The cache a sourced view reads its payload back from. A miss recomputes
/// through the cache's ordinary producer; the result must have the same
/// V digest.
pub trait SignatureSource: Send + Sync {
    fn reread(&self, d: DefId) -> Result<hirsel_types::SignatureRef, hirsel_types::QueryError>;
    /// The source's own Arc allocation and owned control buffers. Shared row
    /// payloads keep the source cache's existing admission owners.
    fn retained_control_bytes(&self) -> Option<u64>;
}

/// Why V could not read a view it holds by identity. A refusal is never a
/// verdict on the view: no check that met one keeps its result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ViewRefusal {
    /// The source could not produce the evicted payload again.
    Unreadable {
        definition: DefId,
        cause: hirsel_types::QueryError,
    },
    /// The source produced a payload with a different digest: the identity
    /// V holds is stale.
    Stale { definition: DefId },
}
impl ViewRefusal {
    pub fn definition(&self) -> DefId {
        match self {
            Self::Unreadable { definition, .. } | Self::Stale { definition } => *definition,
        }
    }
}
impl std::fmt::Display for ViewRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreadable { definition, cause } => {
                write!(
                    f,
                    "V view {} cannot be read again: {cause:?}",
                    definition.path()
                )
            }
            Self::Stale { definition } => write!(
                f,
                "V view {} recomputed to a different signature",
                definition.path()
            ),
        }
    }
}
/// A refusal is operational: the source's own resource refusal passes through
/// unchanged, and anything else names the view V could not read.
impl From<ViewRefusal> for hirsel_types::QueryError {
    fn from(refusal: ViewRefusal) -> Self {
        match refusal {
            ViewRefusal::Unreadable {
                cause: cause @ hirsel_types::QueryError::Operational(_),
                ..
            } => cause,
            refusal => Self::Operational(hirsel_types::OperationalFailure {
                operation: "V view read".into(),
                message: refusal.to_string(),
            }),
        }
    }
}
#[derive(Clone)]
enum Payload {
    Held(hirsel_types::SignatureRef),
    Sourced {
        /// Never upgraded into a long-lived handle; it keeps the allocation's
        /// address unique, so pointer identity stays exact after eviction.
        last: Arc<std::sync::Mutex<std::sync::Weak<Signature>>>,
        source: Arc<dyn SignatureSource>,
    },
}
impl std::fmt::Debug for Payload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Held(signature) => f.debug_tuple("Held").field(signature).finish(),
            Self::Sourced { .. } => f.write_str("Sourced"),
        }
    }
}
impl IdentifiedSignature {
    pub fn new(types: Arc<TypeInterner>, signature: hirsel_types::SignatureRef) -> Self {
        let digest = digest(&types, &signature);
        Self {
            types,
            definition: signature.definition,
            heap: signature.metadata.roles.heap.as_ref().map(|role| {
                (
                    HeapPath(DefId::intern(&role.arena)),
                    HeapPath(DefId::intern(&role.operation)),
                )
            }),
            retention: Default::default(),
            payload_retention: Default::default(),
            payload: Payload::Held(signature),
            digest,
        }
    }
    /// The identity of `signature`, whose payload `source` supplies again
    /// when it is next read.
    pub fn sourced(
        types: Arc<TypeInterner>,
        signature: &hirsel_types::SignatureRef,
        source: Arc<dyn SignatureSource>,
    ) -> Self {
        let digest = digest(&types, signature);
        Self {
            types,
            definition: signature.definition,
            heap: signature.metadata.roles.heap.as_ref().map(|role| {
                (
                    HeapPath(DefId::intern(&role.arena)),
                    HeapPath(DefId::intern(&role.operation)),
                )
            }),
            retention: Default::default(),
            payload_retention: Default::default(),
            payload: Payload::Sourced {
                last: Arc::new(std::sync::Mutex::new(Arc::downgrade(signature))),
                source,
            },
            digest,
        }
    }
    pub(crate) fn retained_identity_bytes(&self) -> Option<u64> {
        let mut bytes = crate::memory::arc::<Self>();
        if let Payload::Sourced { source, .. } = &self.payload {
            // The weak last-payload pointer retains the Signature Arc backing
            // after its fields are dropped. The source wrapper has its own Arc.
            bytes = bytes
                .checked_add(crate::memory::arc::<
                    std::sync::Mutex<std::sync::Weak<Signature>>,
                >())?
                .checked_add(crate::memory::arc::<Signature>())?
                .checked_add(source.retained_control_bytes()?)?;
        }
        Some(bytes)
    }
    pub fn retain_control(&self, memory: &dyn crate::ValidationMemory) -> Result<(), ViewRefusal> {
        self.retention
            .retain(memory, || self.retained_identity_bytes())?;
        if let Payload::Held(signature) = &self.payload {
            self.payload_retention
                .retain_with(|| memory.retain_signature(signature))?;
        }
        Ok(())
    }
    /// The payload retained by this identity; sourced views retain their
    /// original cache owner instead of pinning a signature payload.
    pub fn held_signature(&self) -> Option<&hirsel_types::SignatureRef> {
        match &self.payload {
            Payload::Held(signature) => Some(signature),
            Payload::Sourced { .. } => None,
        }
    }
    pub fn belongs_to(&self, types: &TypeInterner) -> bool {
        std::ptr::eq(&*self.types, types)
    }
    pub fn definition(&self) -> DefId {
        self.definition
    }
    pub(crate) fn heap(
        &self,
    ) -> Option<(
        &'static hirsel_syntax::DefPath,
        &'static hirsel_syntax::DefPath,
    )> {
        self.heap
            .map(|(arena, operation)| (arena.path(), operation.path()))
    }
    /// Whether `signature` is the exact payload this identity was made from.
    pub fn is(&self, signature: &hirsel_types::SignatureRef) -> bool {
        match &self.payload {
            Payload::Held(held) => Arc::ptr_eq(held, signature),
            Payload::Sourced { last, .. } => std::ptr::eq(
                last.lock().unwrap_or_else(|e| e.into_inner()).as_ptr(),
                Arc::as_ptr(signature),
            ),
        }
    }
    /// A scoped view of the payload: it pins the source's row only while held.
    /// An evicted row is produced again; a source that refuses, or produces
    /// a different view, refuses the read.
    pub fn signature(&self) -> Result<hirsel_types::SignatureRef, ViewRefusal> {
        match &self.payload {
            Payload::Held(signature) => Ok(signature.clone()),
            Payload::Sourced { last, source } => {
                let mut last = last.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(signature) = last.upgrade() {
                    return Ok(signature);
                }
                let definition = self.definition;
                let signature = source
                    .reread(definition)
                    .map_err(|cause| ViewRefusal::Unreadable { definition, cause })?;
                // The cache recomputed the row: it must be the same view.
                if digest(&self.types, &signature) != self.digest {
                    return Err(ViewRefusal::Stale { definition });
                }
                *last = Arc::downgrade(&signature);
                Ok(signature)
            }
        }
    }
    pub fn digest(&self) -> ValidationDigest {
        self.digest
    }
    /// V's input of this view. Its digest is known without the payload;
    /// a sourced payload is read only when the input's signature is.
    pub fn input(&self) -> ViewInput<'_> {
        ViewInput {
            types: &self.types,
            payload: match &self.payload {
                Payload::Held(signature) => ViewPayload::Borrowed(signature),
                Payload::Sourced { .. } => ViewPayload::Sourced(self),
            },
            digest: self.digest,
        }
    }
}

/// A signature and its verifier-derived identity, bound at construction.
/// Callers cannot pair changed bytes with an earlier or producer digest.
///
/// ```compile_fail
/// use hirsel_validate::{ViewInput, ValidationDigest};
/// fn forge(signature: &hirsel_types::Signature, digest: ValidationDigest) -> ViewInput<'_> {
///     ViewInput { signature, digest }
/// }
/// ```
#[derive(Clone)]
pub struct ViewInput<'a> {
    types: &'a TypeInterner,
    payload: ViewPayload<'a>,
    digest: ValidationDigest,
}
#[derive(Clone)]
enum ViewPayload<'a> {
    Borrowed(&'a Signature),
    Sourced(&'a IdentifiedSignature),
}
/// A view's payload: borrowed from a universe that holds it, or a scoped
/// view that pins a cache row only while this input is held.
#[derive(Clone)]
pub enum Pin<'a> {
    Borrowed(&'a Signature),
    Scoped(hirsel_types::SignatureRef),
}
impl std::ops::Deref for Pin<'_> {
    type Target = Signature;
    fn deref(&self) -> &Signature {
        match self {
            Self::Borrowed(signature) => signature,
            Self::Scoped(signature) => signature,
        }
    }
}
impl<'a> ViewInput<'a> {
    pub(crate) fn for_types(&self, types: &TypeInterner) -> bool {
        std::ptr::eq(self.types, types)
    }
    /// The view's payload: borrowed, or a scoped view of a sourced row,
    /// read again if it was evicted.
    pub fn signature(&self) -> Result<Pin<'a>, ViewRefusal> {
        match self.payload {
            ViewPayload::Borrowed(signature) => Ok(Pin::Borrowed(signature)),
            ViewPayload::Sourced(identified) => identified.signature().map(Pin::Scoped),
        }
    }
    pub fn digest(&self) -> ValidationDigest {
        self.digest
    }
}

/// One input V observed.
#[derive(
    serde::Serialize, serde::Deserialize, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd,
)]
pub enum VRead {
    /// The view of a signature V read, by its digest.
    View(DefId, ValidationDigest),
    /// A signature V looked for and did not find.
    Missing(DefId),
    /// The selected role and the exact declarations that establish it.
    HeapRole(Box<HeapObservation>),
    /// The aggregate of every signature error in the universe.
    UniverseErrors(ContentHash),
}

/// The signature views V validates against: one universe, as one package's
/// bodies see it (its own opaque types through their private views).
pub trait SignatureLookup: Sync {
    fn view(&self, d: DefId) -> Result<Option<ViewInput<'_>>, crate::ViewRefusal>;
    /// The package-visible role. Conflicting claims refuse the observation.
    fn heap_role(&self) -> Result<Option<HeapRole>, ViewRefusal>;
    fn heap_observation(&self) -> Result<HeapObservation, ViewRefusal>;
    /// Every signature error of the universe, in DefPath order, and their digest.
    fn universe_errors(&self) -> (ContentHash, &[Violation]);
}

impl<T: SignatureLookup + ?Sized> SignatureLookup for &T {
    fn view(&self, d: DefId) -> Result<Option<ViewInput<'_>>, crate::ViewRefusal> {
        (**self).view(d)
    }
    fn heap_role(&self) -> Result<Option<HeapRole>, ViewRefusal> {
        (**self).heap_role()
    }
    fn heap_observation(&self) -> Result<HeapObservation, ViewRefusal> {
        (**self).heap_observation()
    }
    fn universe_errors(&self) -> (ContentHash, &[Violation]) {
        (**self).universe_errors()
    }
}

impl<T: SignatureLookup + Send + ?Sized> SignatureLookup for Arc<T> {
    fn view(&self, d: DefId) -> Result<Option<ViewInput<'_>>, crate::ViewRefusal> {
        (**self).view(d)
    }
    fn heap_role(&self) -> Result<Option<HeapRole>, ViewRefusal> {
        (**self).heap_role()
    }
    fn heap_observation(&self) -> Result<HeapObservation, ViewRefusal> {
        (**self).heap_observation()
    }
    fn universe_errors(&self) -> (ContentHash, &[Violation]) {
        (**self).universe_errors()
    }
}

/// V's digest of a view. Every field is encoded, including private shape,
/// malformed rows and metadata a producer could leave out of its fingerprint.
///
/// The encoding uses every serialized field, followed by every referenced type
/// row. Type IDs are first-encounter positions, so interner numbering and
/// producer fingerprints cannot hide a changed view. Missing rows remain explicit.
pub fn digest(t: &TypeInterner, signature: &Signature) -> ValidationDigest {
    let mut definitions = hirsel_intern::persist::Numbering::default();
    let mut symbols = hirsel_intern::persist::Numbering::default();
    let payload = hirsel_syntax::with_def_numbering(&mut definitions, || {
        hirsel_intern::persist::with_symbol_numbering(&mut symbols, || encoded_view(t, signature))
    });
    let paths: Vec<_> = definitions.ids().iter().map(|d| d.path()).collect();
    let spellings: Vec<_> = symbols.ids().iter().map(|s| s.as_str()).collect();
    let names = bincode::serialize(&(paths, spellings)).expect("signature names serialize");
    ValidationDigest(ContentHash::of_parts(
        "hirsel.validate.view.v3",
        [names.as_slice(), payload.as_slice()],
    ))
}

fn encoded_view(t: &TypeInterner, signature: &Signature) -> Vec<u8> {
    let (encoded, roots) =
        hirsel_types::with_table_collection(|| bincode::serialize_into(std::io::sink(), signature));
    encoded.expect("signature fields serialize in a type collection scope");
    let mut positions = std::collections::HashMap::new();
    let mut order = Vec::new();
    let mut pending = roots;
    pending.reverse();
    while let Some(id) = pending.pop() {
        let next = order.len() as u32;
        let std::collections::hash_map::Entry::Vacant(entry) = positions.entry(id) else {
            continue;
        };
        entry.insert(next);
        order.push(id);
        if let Some(row) = t.get(id) {
            let (encoded, edges) = hirsel_types::with_table_collection(|| {
                bincode::serialize_into(std::io::sink(), row)
            });
            encoded.expect("type fields serialize in a type collection scope");
            pending.extend(edges.into_iter().rev());
        }
    }
    let rows: Vec<_> = order.iter().map(|id| t.get(*id)).collect();
    hirsel_types::with_type_positions(positions, || bincode::serialize(&(signature, rows)))
        .expect("all referenced type IDs have positions")
}

/// The digest of a universe's errors, in the order V reports them.
pub fn errors_digest(errors: &[Violation]) -> ContentHash {
    let text = format!("{errors:?}");
    ContentHash::of_parts("hirsel.validate.universe-errors.v1", [text.as_bytes()])
}

/// Capture the current immutable observation before comparing it. Sourced
/// view lookup may invoke a producer, so callers hold no fact-cache lock here.
pub(crate) fn observe_read(
    types: Option<&TypeInterner>,
    read: &VRead,
    signatures: &dyn SignatureLookup,
) -> Option<VRead> {
    match read {
        VRead::View(d, _) => {
            let view = signatures.view(*d).ok()??;
            types
                .is_none_or(|t| view.for_types(t))
                .then(|| VRead::View(*d, view.digest()))
        }
        VRead::Missing(d) => matches!(signatures.view(*d), Ok(None)).then_some(VRead::Missing(*d)),
        VRead::HeapRole(_) => {
            let heap = signatures.heap_observation().ok()?;
            if let Some(types) = types
                && let Some(role) = heap.role()
            {
                for path in [&role.arena, &role.operation] {
                    if !signatures.view(DefId::intern(path)).ok()??.for_types(types) {
                        return None;
                    }
                }
            }
            Some(VRead::HeapRole(Box::new(heap)))
        }
        VRead::UniverseErrors(_) => Some(VRead::UniverseErrors(signatures.universe_errors().0)),
    }
}
pub(crate) fn compare_read(read: &VRead, observed: Option<&VRead>) -> bool {
    hirsel_intern::work::count(hirsel_intern::work::Counter::ValidationReadComparisons);
    hirsel_intern::timings::count(hirsel_intern::timings::Count::ValidationReadComparisons);
    observed == Some(read)
}
/// Whether `read` observes the same value in `signatures` now.
pub fn read_holds(read: &VRead, signatures: &dyn SignatureLookup) -> bool {
    compare_read(read, observe_read(None, read, signatures).as_ref())
}

/// The same witness, also bound to the type universe actually being checked.
/// A canonical digest from a foreign interner is not permission to interpret
/// that interner's numeric TypeIds in this one.
pub(crate) fn read_holds_for_types(
    t: &TypeInterner,
    read: &VRead,
    signatures: &dyn SignatureLookup,
) -> bool {
    compare_read(read, observe_read(Some(t), read, signatures).as_ref())
}

/// The recording lookup V's checks read through, for one validation.
pub(crate) struct Reader<'a> {
    types: &'a TypeInterner,
    signatures: &'a dyn SignatureLookup,
    /// A set: a check reads the same views many times, and V's verdict reads
    /// each once.
    reads: RefCell<crate::hashed::HashSet<VRead>>,
    heap: OnceCell<Option<HeapRole>>,
    foreign: RefCell<BTreeSet<DefId>>,
    /// Scoped views this validation read: each pins its cache row until the
    /// validation ends, and never longer.
    pins: RefCell<crate::hashed::HashMap<DefId, hirsel_types::SignatureRef>>,
    /// The first view this validation could not read. A check that met one
    /// still runs to its end, but `finish` refuses its result.
    refused: RefCell<Option<ViewRefusal>>,
}
impl<'a> Reader<'a> {
    pub(crate) fn new(types: &'a TypeInterner, signatures: &'a dyn SignatureLookup) -> Self {
        Self {
            types,
            signatures,
            reads: RefCell::default(),
            heap: OnceCell::new(),
            foreign: RefCell::default(),
            pins: RefCell::default(),
            refused: RefCell::default(),
        }
    }
    fn pin(&self, d: DefId, signature: hirsel_types::SignatureRef) -> &Signature {
        let pointer = Arc::as_ptr(self.pins.borrow_mut().entry(d).or_insert(signature));
        // SAFETY: a pinned Arc is never removed or replaced while this reader
        // lives, and an Arc's payload never moves, so it outlives `&self`.
        unsafe { &*pointer }
    }
    pub(crate) fn get(&self, d: impl Into<DefId>) -> Option<&Signature> {
        let d = d.into();
        match self.signatures.view(d) {
            Ok(Some(view)) if !view.for_types(self.types) => {
                self.foreign.borrow_mut().insert(d);
                self.reads
                    .borrow_mut()
                    .insert(VRead::View(d, view.digest()));
                None
            }
            Ok(Some(view)) => match view.signature() {
                Ok(signature) => {
                    self.reads
                        .borrow_mut()
                        .insert(VRead::View(d, view.digest()));
                    Some(match signature {
                        Pin::Borrowed(signature) => signature,
                        Pin::Scoped(signature) => self.pin(d, signature),
                    })
                }
                Err(refusal) => {
                    self.refused.borrow_mut().get_or_insert(refusal);
                    None
                }
            },
            Ok(None) => {
                self.reads.borrow_mut().insert(VRead::Missing(d));
                None
            }
            Err(refusal) => {
                self.refused.borrow_mut().get_or_insert(refusal);
                None
            }
        }
    }
    pub(crate) fn input_errors(&self) -> Vec<Violation> {
        self.foreign
            .borrow()
            .iter()
            .map(|d| Violation {
                body: None,
                block: None,
                instruction: None,
                kind: ViolationKind::InvalidSignature(d.path().clone()),
            })
            .collect()
    }
    /// Selection consumes declaration evidence, independent of equal carriers.
    pub(crate) fn heap_role(&self) -> Option<HeapRole> {
        self.heap
            .get_or_init(|| {
                match heap_evidence(
                    Some(self.types),
                    self.signatures,
                    |read| {
                        self.reads.borrow_mut().insert(read);
                    },
                    |t, d, s, lookup| check_view(t, d, s, lookup),
                ) {
                    Ok(observation) => {
                        let role = observation.role();
                        self.reads
                            .borrow_mut()
                            .insert(VRead::HeapRole(Box::new(observation)));
                        role
                    }
                    Err(refusal) => {
                        self.refused.borrow_mut().get_or_insert(refusal);
                        None
                    }
                }
            })
            .clone()
    }
    pub(crate) fn universe_errors(&self) -> &'a [Violation] {
        let (digest, errors) = self.signatures.universe_errors();
        self.reads
            .borrow_mut()
            .insert(VRead::UniverseErrors(digest));
        errors
    }
    /// The reads made so far, sorted and without duplicates, or the refusal
    /// that voids every result computed with this reader.
    pub(crate) fn finish(self, reads: &mut Vec<VRead>) -> Result<(), ViewRefusal> {
        if let Some(refusal) = self.refused.into_inner() {
            return Err(refusal);
        }
        let mut own: Vec<_> = self.reads.into_inner().into_iter().collect();
        own.sort_unstable();
        reads.extend(own);
        Ok(())
    }
}

/// V's verdict on one view: the error it found, if any, and what it read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ViewVerdict {
    pub error: Option<Violation>,
    pub reads: Vec<VRead>,
    pub(crate) retention: crate::memory::ControlRetention,
}

impl ViewVerdict {
    pub(crate) fn retained_verdict_bytes(&self) -> Option<u64> {
        crate::memory::arc::<Self>()
            .checked_add(crate::memory::vector(&self.reads)?)?
            .checked_add(read_payload_bytes(&self.reads)?)?
            .checked_add(
                self.error
                    .as_ref()
                    .map_or(Some(0), crate::memory::violation)?,
            )
    }

    pub(crate) fn retain_control(
        &self,
        memory: &dyn crate::ValidationMemory,
    ) -> Result<(), ViewRefusal> {
        self.retention
            .retain(memory, || self.retained_verdict_bytes())
    }
}

/// V's verdict on the view of `d`, read through `signatures`, or the
/// refusal of a view it had to read.
pub fn check_view(
    t: &TypeInterner,
    d: DefId,
    signature: &Signature,
    signatures: &dyn SignatureLookup,
) -> Result<ViewVerdict, ViewRefusal> {
    let reader = Reader::new(t, signatures);
    let error = signature_error(t, d, signature, &reader)
        .or_else(|| reader.input_errors().into_iter().next());
    let mut reads = vec![];
    reader.finish(&mut reads)?;
    Ok(ViewVerdict {
        error,
        reads,
        retention: Default::default(),
    })
}

/// The signature errors of one universe, with their digest.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct UniverseErrors {
    pub digest: ContentHash,
    pub errors: Vec<Violation>,
}
impl UniverseErrors {
    pub fn new(errors: Vec<Violation>) -> Self {
        Self {
            digest: errors_digest(&errors),
            errors,
        }
    }
}

/// Per-view verdicts keyed by digest. A verdict is reused for a view with the
/// same digest while every read it made still holds; otherwise V checks the
/// view again. A revision's store starts empty and takes what still holds
/// from the previous revision's (`verdict_from`), so it keeps only verdicts
/// its own views use.
pub struct ValidatedSignatures {
    verdicts: crate::region::Map<ValidationDigest, crate::region::Vector<Arc<ViewVerdict>>>,
    cache: Option<Arc<crate::VerifierCache>>,
    policy: ContentHash,
}
impl Default for ValidatedSignatures {
    fn default() -> Self {
        Self {
            verdicts: crate::region::Region::default().map(),
            cache: None,
            policy: ContentHash::default(),
        }
    }
}
impl ValidatedSignatures {
    pub fn admit_storage(
        &mut self,
        memory: &Arc<dyn crate::ValidationMemory>,
    ) -> Result<(), ViewRefusal> {
        self.retain_storage(memory)?;
        if let Some(cache) = &self.cache {
            self.cache = Some(cache.for_universe(memory.clone())?);
        }
        Ok(())
    }
    pub fn retain_storage(
        &self,
        memory: &Arc<dyn crate::ValidationMemory>,
    ) -> Result<(), ViewRefusal> {
        self.verdicts.allocator().retain(memory)
    }
    pub(crate) fn memory(&self) -> Option<Arc<dyn crate::ValidationMemory>> {
        self.verdicts.allocator().memory()
    }
    pub fn retain_verdicts(&self, memory: &dyn crate::ValidationMemory) -> Result<(), ViewRefusal> {
        for verdict in self.verdicts.values().flatten() {
            verdict.retain_control(memory)?;
        }
        Ok(())
    }

    pub(crate) fn verifier(&self) -> Option<&Arc<crate::VerifierCache>> {
        self.cache.as_ref()
    }
    /// Group physical writes while retaining each independent verdict and its
    /// complete current reads. The batch itself confers no verification proof.
    pub fn publication_batch(&self) -> Option<hirsel_artifact_store::Batch> {
        self.cache
            .as_ref()
            .and_then(|cache| cache.publication_batch())
    }
    /// Successful locally checked per-view reads, for inspection only.
    pub fn successful(&self) -> impl Iterator<Item = (ValidationDigest, &[VRead])> {
        self.verdicts.iter().flat_map(|(digest, verdicts)| {
            verdicts
                .iter()
                .filter(|verdict| verdict.error.is_none())
                .map(move |verdict| (*digest, verdict.reads.as_slice()))
        })
    }
    /// Local verifier authority. Product deserialization cannot construct this
    /// capability or import successful-looking read lists into it.
    pub fn with_cache(cache: Option<Arc<crate::VerifierCache>>, policy: ContentHash) -> Self {
        Self {
            cache,
            policy,
            ..Self::default()
        }
    }
    /// The verdict on view `d` of `signatures`, reused or computed.
    pub fn verdict(
        &mut self,
        t: &TypeInterner,
        d: DefId,
        signatures: &dyn SignatureLookup,
    ) -> Result<Arc<ViewVerdict>, ViewRefusal> {
        self.verdict_from(t, d, signatures, None)
    }
    /// The verdict on view `d` of `signatures`: one this store or `previous`
    /// holds whose reads still hold, or a new one. A refused read keeps
    /// nothing and refuses the verdict.
    pub fn verdict_from(
        &mut self,
        t: &TypeInterner,
        d: DefId,
        signatures: &dyn SignatureLookup,
        previous: Option<&ValidatedSignatures>,
    ) -> Result<Arc<ViewVerdict>, ViewRefusal> {
        let input = signatures.view(d)?.expect("a verdict names a view");
        if !input.for_types(t) {
            return Ok(Arc::new(ViewVerdict {
                error: Some(Violation {
                    body: None,
                    block: None,
                    instruction: None,
                    kind: ViolationKind::InvalidSignature(d.path().clone()),
                }),
                reads: vec![],
                retention: Default::default(),
            }));
        }
        let digest = input.digest();
        let holds = |v: &&Arc<ViewVerdict>| {
            v.reads
                .iter()
                .all(|read| read_holds_for_types(t, read, signatures))
        };
        if let Some(verdict) = self
            .verdicts
            .get(&digest)
            .and_then(|kept| kept.iter().find(holds))
        {
            return Ok(verdict.clone());
        }
        let kept = previous
            .filter(|previous| previous.policy == self.policy)
            .and_then(|previous| previous.verdicts.get(&digest))
            .and_then(|kept| kept.iter().find(holds).cloned());
        let verdict = match kept {
            Some(verdict) => verdict,
            None => match &self.cache {
                None => Arc::new(check_view(t, d, &*input.signature()?, signatures)?),
                Some(cache) => cache.view_verdict(t, d, input.clone(), signatures, self.policy)?,
            },
        };
        let storage = self.verdicts.allocator().clone();
        self.verdicts
            .try_reserve(1)
            .map_err(|_| storage.refusal())?;
        let kept = self
            .verdicts
            .entry(digest)
            .or_insert_with(|| storage.vector());
        storage.push(kept, verdict.clone())?;
        if let Some(memory) = storage.memory() {
            verdict.retain_control(&*memory)?;
        }
        Ok(verdict)
    }
    /// The signature errors of the universe `views` (every view of
    /// `signatures`, in DefPath order).
    pub fn universe(
        &mut self,
        t: &TypeInterner,
        views: impl IntoIterator<Item = DefId>,
        signatures: &dyn SignatureLookup,
    ) -> Result<UniverseErrors, ViewRefusal> {
        let _batch = self.publication_batch();
        let mut errors = vec![];
        for d in views {
            errors.extend(self.verdict(t, d, signatures)?.error.clone());
        }
        Ok(UniverseErrors::new(errors))
    }
    /// How many verdicts the store holds.
    pub fn len(&self) -> usize {
        self.verdicts.values().map(|v| v.len()).sum()
    }
    pub fn is_empty(&self) -> bool {
        self.verdicts.is_empty()
    }
}

/// An owned universe of signatures for tools and tests, over one interner.
/// Each view's digest and the universe's errors are computed when first asked
/// for, and changing a signature forgets them.
#[derive(Clone)]
pub struct SignatureMap {
    types: Arc<TypeInterner>,
    views: BTreeMap<DefId, View>,
    errors: OnceLock<UniverseErrors>,
}
#[derive(Clone)]
struct View {
    signature: Signature,
    digest: OnceLock<ValidationDigest>,
}
impl View {
    fn new(signature: Signature) -> Self {
        Self {
            signature,
            digest: OnceLock::new(),
        }
    }
}
impl SignatureMap {
    pub fn new(types: Arc<TypeInterner>) -> Self {
        Self {
            types,
            views: BTreeMap::new(),
            errors: OnceLock::new(),
        }
    }
    pub fn types(&self) -> &Arc<TypeInterner> {
        &self.types
    }
    pub fn insert(&mut self, d: impl Into<DefId>, signature: Signature) -> Option<Signature> {
        self.errors = OnceLock::new();
        self.views
            .insert(d.into(), View::new(signature))
            .map(|old| old.signature)
    }
    pub fn remove(&mut self, d: impl Into<DefId>) -> Option<Signature> {
        self.errors = OnceLock::new();
        self.views.remove(&d.into()).map(|old| old.signature)
    }
    pub fn clear(&mut self) {
        self.errors = OnceLock::new();
        self.views.clear();
    }
    pub fn get(&self, d: impl Into<DefId>) -> Option<&Signature> {
        self.views.get(&d.into()).map(|v| &v.signature)
    }
    pub fn get_mut(&mut self, d: impl Into<DefId>) -> Option<&mut Signature> {
        self.errors = OnceLock::new();
        let view = self.views.get_mut(&d.into())?;
        view.digest = OnceLock::new();
        Some(&mut view.signature)
    }
    pub fn contains_key(&self, d: impl Into<DefId>) -> bool {
        self.views.contains_key(&d.into())
    }
    pub fn len(&self) -> usize {
        self.views.len()
    }
    pub fn is_empty(&self) -> bool {
        self.views.is_empty()
    }
    /// The views' definitions, in DefPath order.
    pub fn keys(&self) -> impl Iterator<Item = DefId> + '_ {
        self.views.keys().copied()
    }
    pub fn values(&self) -> impl Iterator<Item = &Signature> {
        self.views.values().map(|v| &v.signature)
    }
    pub fn iter(&self) -> impl Iterator<Item = (DefId, &Signature)> {
        self.views.iter().map(|(d, v)| (*d, &v.signature))
    }
    /// Every signature, keyed by its own definition.
    pub fn extend_signatures(&mut self, signatures: impl IntoIterator<Item = Signature>) {
        for signature in signatures {
            self.insert(signature.definition, signature);
        }
    }
}
impl<D: Into<DefId>> Extend<(D, Signature)> for SignatureMap {
    fn extend<I: IntoIterator<Item = (D, Signature)>>(&mut self, iter: I) {
        for (d, signature) in iter {
            self.insert(d, signature);
        }
    }
}
impl SignatureLookup for SignatureMap {
    fn view(&self, d: DefId) -> Result<Option<ViewInput<'_>>, crate::ViewRefusal> {
        Ok(self.views.get(&d).map(|v| {
            let digest = *v.digest.get_or_init(|| digest(&self.types, &v.signature));
            ViewInput {
                types: &self.types,
                payload: ViewPayload::Borrowed(&v.signature),
                digest,
            }
        }))
    }
    fn heap_role(&self) -> Result<Option<HeapRole>, ViewRefusal> {
        select_heap(
            self.views
                .values()
                .filter_map(|v| v.signature.metadata.roles.heap.as_ref())
                .map(|role| (&role.arena, &role.operation)),
        )
    }
    fn heap_observation(&self) -> Result<HeapObservation, ViewRefusal> {
        observe_heap(None, self)
    }
    fn universe_errors(&self) -> (ContentHash, &[Violation]) {
        let universe = self.errors.get_or_init(|| {
            UniverseErrors::new(
                self.views
                    .iter()
                    .filter_map(|(d, v)| {
                        check_view(&self.types, *d, &v.signature, self)
                            .expect("a held map has no sourced view to refuse")
                            .error
                    })
                    .collect(),
            )
        });
        (universe.digest, &universe.errors)
    }
}
