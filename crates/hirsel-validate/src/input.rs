//! V's input: the signature views it reads, each identified by V's own digest,
//! and the reads it records against them.
//!
//! V never trusts the producer's fingerprint. Every view V reads is identified
//! by a `ValidationDigest` that this crate computes over every field of the
//! view, private shape included, so a producer that leaves a field out of its
//! fingerprint (a malformed row, a private field) still changes the digest.
//! Every lookup V makes is recorded as a `VRead`: a view by digest, a missing
//! signature, the first heap role's selection, and the universe's errors. A
//! verdict is reusable exactly while its reads still hold (`read_holds`).
use super::*;
use crate::hashed::HashMap;
use hirsel_syntax::{ContentHash, DefId};
use hirsel_types::{HeapRole, TypeInterner};
use std::cell::RefCell;
use std::sync::{Arc, OnceLock};

/// V's identity of one signature view: a hash over every field it holds.
#[derive(
    serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd,
)]
pub struct ValidationDigest(pub ContentHash);

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
    /// Part of the identity: universes select their first heap role without
    /// reading every payload again.
    heap: Option<HeapRole>,
}
/// The cache a sourced view reads its payload back from. A miss recomputes
/// through the cache's ordinary producer; the result must have the same
/// V digest.
pub trait SignatureSource: Send + Sync {
    fn reread(&self, d: DefId) -> Result<hirsel_types::SignatureRef, String>;
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
            heap: signature.metadata.roles.heap.clone(),
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
            heap: signature.metadata.roles.heap.clone(),
            payload: Payload::Sourced {
                last: Arc::new(std::sync::Mutex::new(Arc::downgrade(signature))),
                source,
            },
            digest,
        }
    }
    pub fn belongs_to(&self, types: &TypeInterner) -> bool {
        std::ptr::eq(&*self.types, types)
    }
    pub fn definition(&self) -> DefId {
        self.definition
    }
    pub fn heap(&self) -> Option<&HeapRole> {
        self.heap.as_ref()
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
    pub fn signature(&self) -> hirsel_types::SignatureRef {
        match &self.payload {
            Payload::Held(signature) => signature.clone(),
            Payload::Sourced { last, source } => {
                let mut last = last.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(signature) = last.upgrade() {
                    return signature;
                }
                let d = self.definition;
                let signature = source.reread(d).unwrap_or_else(|error| {
                    panic!("V view {} cannot be read again: {error}", d.path())
                });
                // The cache recomputed the row: it must be the same view.
                assert_eq!(
                    digest(&self.types, &signature),
                    self.digest,
                    "V view {} recomputed to a different signature",
                    d.path()
                );
                *last = Arc::downgrade(&signature);
                signature
            }
        }
    }
    pub fn digest(&self) -> ValidationDigest {
        self.digest
    }
    pub fn input(&self) -> ViewInput<'_> {
        ViewInput {
            types: &self.types,
            signature: match &self.payload {
                Payload::Held(signature) => Pin::Borrowed(signature),
                Payload::Sourced { .. } => Pin::Scoped(self.signature()),
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
    signature: Pin<'a>,
    digest: ValidationDigest,
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
    pub fn signature(&self) -> &Signature {
        &self.signature
    }
    pub fn digest(&self) -> ValidationDigest {
        self.digest
    }
    pub fn into_parts(self) -> (Pin<'a>, ValidationDigest) {
        (self.signature, self.digest)
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
    /// Which signature supplies the first heap role, if any.
    FirstHeap(Option<DefId>),
    /// The aggregate of every signature error in the universe.
    UniverseErrors(ContentHash),
}

/// The signature views V validates against: one universe, as one package's
/// bodies see it (its own opaque types through their private views).
pub trait SignatureLookup: Sync {
    fn view(&self, d: DefId) -> Option<ViewInput<'_>>;
    /// The heap role of the first signature in DefPath order that has one.
    fn first_heap(&self) -> Option<(DefId, HeapRole)>;
    /// Every signature error of the universe, in DefPath order, and their digest.
    fn universe_errors(&self) -> (ContentHash, &[Violation]);
}

impl<T: SignatureLookup + ?Sized> SignatureLookup for &T {
    fn view(&self, d: DefId) -> Option<ViewInput<'_>> {
        (**self).view(d)
    }
    fn first_heap(&self) -> Option<(DefId, HeapRole)> {
        (**self).first_heap()
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

/// Whether `read` observes the same value in `signatures` now.
pub fn read_holds(read: &VRead, signatures: &dyn SignatureLookup) -> bool {
    match read {
        VRead::View(d, digest) => signatures
            .view(*d)
            .is_some_and(|view| view.digest() == *digest),
        VRead::Missing(d) => signatures.view(*d).is_none(),
        VRead::FirstHeap(selected) => signatures.first_heap().map(|(d, _)| d) == *selected,
        VRead::UniverseErrors(digest) => signatures.universe_errors().0 == *digest,
    }
}

/// The same witness, also bound to the type universe actually being checked.
/// A canonical digest from a foreign interner is not permission to interpret
/// that interner's numeric TypeIds in this one.
pub(crate) fn read_holds_for_types(
    t: &TypeInterner,
    read: &VRead,
    signatures: &dyn SignatureLookup,
) -> bool {
    match read {
        VRead::View(d, digest) => signatures
            .view(*d)
            .is_some_and(|view| view.for_types(t) && view.digest() == *digest),
        _ => read_holds(read, signatures),
    }
}

/// The recording lookup V's checks read through, for one validation.
pub(crate) struct Reader<'a> {
    types: &'a TypeInterner,
    signatures: &'a dyn SignatureLookup,
    /// A set: a check reads the same views many times, and V's verdict reads
    /// each once.
    reads: RefCell<crate::hashed::HashSet<VRead>>,
    foreign: RefCell<BTreeSet<DefId>>,
    /// Scoped views this validation read: each pins its cache row until the
    /// validation ends, and never longer.
    pins: RefCell<crate::hashed::HashMap<DefId, hirsel_types::SignatureRef>>,
}
impl<'a> Reader<'a> {
    pub(crate) fn new(types: &'a TypeInterner, signatures: &'a dyn SignatureLookup) -> Self {
        Self {
            types,
            signatures,
            reads: RefCell::default(),
            foreign: RefCell::default(),
            pins: RefCell::default(),
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
            Some(view) if !view.for_types(self.types) => {
                self.foreign.borrow_mut().insert(d);
                self.reads
                    .borrow_mut()
                    .insert(VRead::View(d, view.digest()));
                None
            }
            Some(view) => {
                let (signature, digest) = view.into_parts();
                self.reads.borrow_mut().insert(VRead::View(d, digest));
                Some(match signature {
                    Pin::Borrowed(signature) => signature,
                    Pin::Scoped(signature) => self.pin(d, signature),
                })
            }
            None => {
                self.reads.borrow_mut().insert(VRead::Missing(d));
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
    /// The first heap role. The selection is a read, and so is the view that
    /// supplies the role.
    pub(crate) fn first_heap(&self) -> Option<HeapRole> {
        let first = self.signatures.first_heap();
        let selected = first.as_ref().map(|(d, _)| *d);
        self.reads.borrow_mut().insert(VRead::FirstHeap(selected));
        if let Some(d) = selected {
            self.get(d);
        }
        first.map(|(_, role)| role)
    }
    pub(crate) fn universe_errors(&self) -> &'a [Violation] {
        let (digest, errors) = self.signatures.universe_errors();
        self.reads
            .borrow_mut()
            .insert(VRead::UniverseErrors(digest));
        errors
    }
    /// The reads made so far, sorted and without duplicates.
    pub(crate) fn finish(self, reads: &mut Vec<VRead>) {
        let mut own: Vec<_> = self.reads.into_inner().into_iter().collect();
        own.sort_unstable();
        reads.extend(own);
    }
}

/// V's verdict on one view: the error it found, if any, and what it read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ViewVerdict {
    pub error: Option<Violation>,
    pub reads: Vec<VRead>,
}

/// V's verdict on the view of `d`, read through `signatures`.
pub fn check_view(
    t: &TypeInterner,
    d: DefId,
    signature: &Signature,
    signatures: &dyn SignatureLookup,
) -> ViewVerdict {
    let reader = Reader::new(t, signatures);
    let error = signature_error(t, d, signature, &reader)
        .or_else(|| reader.input_errors().into_iter().next());
    let mut reads = vec![];
    reader.finish(&mut reads);
    ViewVerdict { error, reads }
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
#[derive(Clone, Default)]
pub struct ValidatedSignatures {
    verdicts: HashMap<ValidationDigest, Vec<Arc<ViewVerdict>>>,
    cache: Option<Arc<crate::VerifierCache>>,
    policy: ContentHash,
}
impl ValidatedSignatures {
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
    ) -> Arc<ViewVerdict> {
        self.verdict_from(t, d, signatures, None)
    }
    /// The verdict on view `d` of `signatures`: one this store or `previous`
    /// holds whose reads still hold, or a new one.
    pub fn verdict_from(
        &mut self,
        t: &TypeInterner,
        d: DefId,
        signatures: &dyn SignatureLookup,
        previous: Option<&ValidatedSignatures>,
    ) -> Arc<ViewVerdict> {
        let input = signatures.view(d).expect("a verdict names a view");
        if !input.for_types(t) {
            return Arc::new(ViewVerdict {
                error: Some(Violation {
                    body: None,
                    block: None,
                    instruction: None,
                    kind: ViolationKind::InvalidSignature(d.path().clone()),
                }),
                reads: vec![],
            });
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
            return verdict.clone();
        }
        let verdict = previous
            .filter(|previous| previous.policy == self.policy)
            .and_then(|previous| previous.verdicts.get(&digest))
            .and_then(|kept| kept.iter().find(holds).cloned())
            .unwrap_or_else(|| {
                self.cache.as_ref().map_or_else(
                    || Arc::new(check_view(t, d, input.signature(), signatures)),
                    |cache| cache.view_verdict(t, d, input.clone(), signatures, self.policy),
                )
            });
        self.verdicts
            .entry(digest)
            .or_default()
            .push(verdict.clone());
        verdict
    }
    /// The signature errors of the universe `views` (every view of
    /// `signatures`, in DefPath order).
    pub fn universe(
        &mut self,
        t: &TypeInterner,
        views: impl IntoIterator<Item = DefId>,
        signatures: &dyn SignatureLookup,
    ) -> UniverseErrors {
        let _batch = self.publication_batch();
        let errors = views
            .into_iter()
            .filter_map(|d| self.verdict(t, d, signatures).error.clone())
            .collect();
        UniverseErrors::new(errors)
    }
    /// How many verdicts the store holds.
    pub fn len(&self) -> usize {
        self.verdicts.values().map(Vec::len).sum()
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
    fn view(&self, d: DefId) -> Option<ViewInput<'_>> {
        self.views.get(&d).map(|v| {
            let digest = *v.digest.get_or_init(|| digest(&self.types, &v.signature));
            ViewInput {
                types: &self.types,
                signature: Pin::Borrowed(&v.signature),
                digest,
            }
        })
    }
    fn first_heap(&self) -> Option<(DefId, HeapRole)> {
        self.views.iter().find_map(|(d, v)| {
            v.signature
                .metadata
                .roles
                .heap
                .clone()
                .map(|role| (*d, role))
        })
    }
    fn universe_errors(&self) -> (ContentHash, &[Violation]) {
        let universe = self.errors.get_or_init(|| {
            UniverseErrors::new(
                self.views
                    .iter()
                    .filter_map(|(d, v)| check_view(&self.types, *d, &v.signature, self).error)
                    .collect(),
            )
        });
        (universe.digest, &universe.errors)
    }
}
