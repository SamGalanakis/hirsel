//! One admitted request's transport over its selected best-effort durable store.
//! This changes physical retention only, never semantic query invalidation or
//! verifier authority. All producers receive the same Arc<RequestStore>.
use super::*;

use std::collections::BTreeMap;
use std::sync::Weak;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
mod scratch;

// Initial content-tree reservation and archive-copy working overhead.
const ENTRY_CHARGE: u64 = 2048;
fn association_bytes(entries: usize) -> u64 {
    if entries == 0 {
        return 0;
    }
    let mut weight = hirsel_query_products::retained::MetadataWeight::default();
    weight.tree::<ContentHash, (Association, bool)>(entries);
    weight.bytes()
}
fn content_tree_bytes(entries: usize) -> u64 {
    if entries == 0 {
        return 0;
    }
    let mut weight = hirsel_query_products::retained::MetadataWeight::default();
    weight.tree::<ContentHash, (Reference, Bytes)>(entries);
    weight.bytes().max(ENTRY_CHARGE)
}
struct State {
    used: u64,
    installed: Option<Arc<InstalledPackage>>,
    contents: BTreeMap<ContentHash, (Reference, Bytes)>,
    /// Each selected association, and whether its backend publication is
    /// deferred to the next `publish_root` (only under `Complete`).
    associations: BTreeMap<ContentHash, (Association, bool)>,
}
struct Admission {
    memory: hirsel_engine_admission::MemoryReservation,
    used: AtomicU64,
    exhausted: AtomicBool,
    scratch: OnceLock<Arc<scratch::Root>>,
    verifier: OnceLock<Weak<hirsel_validate::VerifierCache>>,
    reads: OnceLock<Weak<scratch::ReadCache>>,
}
impl Admission {
    fn scratch_bytes(&self) -> u64 {
        self.scratch.get().map_or(0, |root| root.retained_bytes())
    }
    fn retained_bytes(&self) -> u64 {
        self.used.load(Ordering::Acquire) + self.scratch_bytes()
    }
    fn limit_bytes(&self) -> u64 {
        self.memory.bytes() + self.scratch_bytes()
    }
    fn try_charge(&self, bytes: u64) -> bool {
        let reserve = || {
            self.used
                .try_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                    used.checked_add(bytes)
                        .filter(|next| *next <= self.memory.bytes())
                })
                .is_ok()
        };
        if reserve() {
            return true;
        }
        if let Some(reads) = self.reads.get().and_then(Weak::upgrade) {
            reads.evict();
            if reserve() {
                return true;
            }
        }
        if let Some(verifier) = self.verifier.get().and_then(Weak::upgrade) {
            verifier.evict_facts();
            if reserve() {
                return true;
            }
        }
        false
    }
    fn charge(&self, bytes: u64) -> bool {
        let admitted = self.try_charge(bytes);
        if !admitted {
            self.exhausted.store(true, Ordering::Release);
        }
        admitted
    }
    fn archive_charge(&self, bytes: u64, stage: &'static str) -> Result<(), ProductArchiveError> {
        if self.charge(bytes) {
            Ok(())
        } else {
            Err(ProductArchiveError::Admission {
                stage,
                requested: bytes,
                retained: self.retained_bytes(),
                limit: self.limit_bytes(),
            })
        }
    }
    fn release(&self, bytes: u64) {
        let previous = self.used.fetch_sub(bytes, Ordering::AcqRel);
        debug_assert!(previous >= bytes);
    }
}
pub struct RequestStore {
    backend: Arc<dyn ResultStore>,
    /// The backend's policy, chosen once for this request.
    baseline: Persistence,
    active_policy: Mutex<Weak<OperationPersistence>>,
    verifier: Option<Arc<hirsel_validate::VerifierCache>>,
    memory: Arc<Admission>,
    state: Mutex<State>,
}
impl std::fmt::Debug for RequestStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        f.debug_struct("RequestStore")
            .field("retained_bytes", &self.retained_bytes())
            .field("metadata_bytes", &state.used)
            .field("associations", &state.associations.len())
            .field("contents", &state.contents.len())
            .finish()
    }
}
impl RequestStore {
    pub fn new(
        backend: Arc<dyn ResultStore>,
        memory: hirsel_engine_admission::MemoryReservation,
    ) -> Self {
        let backend = backend.for_operation().unwrap_or(backend);
        Self::admitted(
            backend,
            Arc::new(Admission {
                memory,
                used: AtomicU64::new(0),
                exhausted: AtomicBool::new(false),
                scratch: OnceLock::new(),
                verifier: OnceLock::new(),
                reads: OnceLock::new(),
            }),
        )
    }
    /// Explicit request-local scratch transport. The selected backend and its
    /// verifier/local-store authority remain unchanged. Metadata and live read
    /// buffers share the original RAM reservation; physical files have the
    /// caller's disk bound and are deleted after the last transported view.
    pub fn with_scratch(
        backend: Arc<dyn ResultStore>,
        mut memory: hirsel_engine_admission::MemoryReservation,
        parent: &std::path::Path,
        disk_bytes: u64,
    ) -> std::io::Result<Self> {
        let backend = backend.for_operation().unwrap_or(backend);
        let root = scratch::Root::new(&mut memory, parent)?;
        let admission = Arc::new(Admission {
            memory,
            used: AtomicU64::new(0),
            exhausted: AtomicBool::new(false),
            scratch: OnceLock::new(),
            verifier: OnceLock::new(),
            reads: OnceLock::new(),
        });
        let backend = Arc::new(scratch::ScratchStore::new(
            backend,
            admission.clone(),
            root,
            disk_bytes,
        )?);
        Ok(Self::admitted(backend, admission))
    }
    /// Keep private source files alive without retaining query transport or
    /// its working reservation. The directory owns its separate charge.
    pub fn source_files_owner(&self) -> Option<Arc<dyn Send + Sync>> {
        self.memory
            .scratch
            .get()
            .map(|root| root.clone() as Arc<dyn Send + Sync>)
    }
    /// Keep verifier content and associations in admitted request transport.
    /// A backend without a verifier uses request-local rows. Only V issues
    /// associations and opaque acceptance capabilities.
    pub fn with_scratch_and_verifier_transport(
        backend: Arc<dyn ResultStore>,
        memory: hirsel_engine_admission::MemoryReservation,
        parent: &std::path::Path,
        disk_bytes: u64,
    ) -> std::io::Result<Self> {
        let mut request = Self::with_scratch(backend, memory, parent, disk_bytes)?;
        let metadata = (size_of::<RequestVerifierRows>()
            + size_of::<hirsel_validate::VerifierCache>()
            + 8 * size_of::<usize>()) as u64;
        if !request.memory.charge(metadata) {
            return Err(std::io::Error::other(
                "verifier owner metadata exceeds request transport",
            ));
        }
        let rows = Arc::new(RequestVerifierRows {
            backend: request.backend.clone(),
            admission: request.memory.clone(),
            rows: Mutex::new(BTreeMap::new()),
            metadata,
        });
        request.verifier = Some(match &request.verifier {
            Some(verifier) => verifier.with_transport(rows),
            None => Arc::new(hirsel_validate::VerifierCache::hosted(rows)),
        });
        let _ = request
            .memory
            .verifier
            .set(Arc::downgrade(request.verifier.as_ref().unwrap()));
        Ok(request)
    }
    /// A request whose verifier is `session`'s when it is over the same store
    /// and verify mode, and its own otherwise.
    pub fn with_scratch_and_session_verifier(
        session: &SessionVerifier,
        backend: Arc<dyn ResultStore>,
        verify: crate::VerifyStore,
        memory: hirsel_engine_admission::MemoryReservation,
        parent: &std::path::Path,
        disk_bytes: u64,
    ) -> std::io::Result<Self> {
        if !session.serves(&backend, verify) {
            return Self::with_scratch_and_verifier_transport(backend, memory, parent, disk_bytes);
        }
        let mut request = Self::with_scratch(backend, memory, parent, disk_bytes)?;
        request.verifier = session
            .owner
            .as_ref()
            .and_then(|owner| owner.verifier.clone());
        Ok(request)
    }
    fn admitted(backend: Arc<dyn ResultStore>, memory: Arc<Admission>) -> Self {
        let verifier = backend.verifier();
        let baseline = backend.persistence();
        Self {
            baseline,
            active_policy: Mutex::new(Weak::new()),
            backend,
            verifier,
            memory,
            state: Mutex::new(State {
                used: 0,
                installed: None,
                contents: BTreeMap::new(),
                associations: BTreeMap::new(),
            }),
        }
    }
    /// The native task reports the existing deterministic resource failure
    /// before settling a product when its admitted transport allowance is full.
    pub fn exhausted(&self) -> bool {
        self.memory.exhausted.load(Ordering::Acquire)
    }
    pub fn retained_bytes(&self) -> u64 {
        self.memory.retained_bytes()
    }
    fn charge(&self, state: &mut State, bytes: u64) -> bool {
        if !self.memory.charge(bytes) {
            return false;
        }
        state.used += bytes;
        true
    }
}
/// One session's verifier, owned beside its carries so that the authority a
/// carried row holds lives as long as the row. Its rows, facts and content
/// are charged to the session's own fixed reservation and spool, never to a
/// request's, so a request's end releases nothing V published. V publishes a
/// row only after storing its content, as every store-owned verifier does.
/// A store with operation epochs issues authority per operation, so its
/// requests keep their own verifiers.
pub struct SessionVerifier {
    /// The store a request must be over to share this verifier.
    backend: Arc<dyn ResultStore>,
    verify: crate::VerifyStore,
    /// Kept for its verifier; it serves no products. None over a store with
    /// operation epochs.
    owner: Option<RequestStore>,
}
impl SessionVerifier {
    pub fn new(
        backend: Arc<dyn ResultStore>,
        verify: crate::VerifyStore,
        memory: hirsel_engine_admission::MemoryReservation,
        parent: &std::path::Path,
        disk_bytes: u64,
    ) -> std::io::Result<Self> {
        let owner = match backend.for_operation() {
            Some(_) => None,
            None => Some(RequestStore::with_scratch_and_verifier_transport(
                backend.clone(),
                memory,
                parent,
                disk_bytes,
            )?),
        };
        Ok(Self {
            backend,
            verify,
            owner,
        })
    }
    /// The store this session's requests use when the caller selects none.
    pub fn backend(&self) -> &Arc<dyn ResultStore> {
        &self.backend
    }
    /// A different store identity (and so a different compiler's store) or
    /// verify mode has its own authority.
    fn serves(&self, backend: &Arc<dyn ResultStore>, verify: crate::VerifyStore) -> bool {
        self.owner.is_some()
            && std::ptr::addr_eq(Arc::as_ptr(&self.backend), Arc::as_ptr(backend))
            && self.verify == verify
    }
    pub fn retained_bytes(&self) -> u64 {
        self.owner.as_ref().map_or(0, RequestStore::retained_bytes)
    }
}
struct ArchiveLease {
    memory: Arc<Admission>,
    bytes: u64,
}
impl Drop for ArchiveLease {
    fn drop(&mut self) {
        self.memory.release(self.bytes);
    }
}
struct RequestVerifierRows {
    backend: Arc<dyn ResultStore>,
    admission: Arc<Admission>,
    rows: Mutex<BTreeMap<ContentHash, Reference>>,
    metadata: u64,
}
fn verifier_row_bytes(entries: usize) -> u64 {
    if entries == 0 {
        return 0;
    }
    let mut weight = hirsel_query_products::retained::MetadataWeight::default();
    weight.tree::<ContentHash, Reference>(entries);
    weight.bytes()
}
impl Drop for RequestVerifierRows {
    fn drop(&mut self) {
        let entries = self
            .rows
            .get_mut()
            .unwrap_or_else(|error| error.into_inner())
            .len();
        self.admission
            .release(verifier_row_bytes(entries) + self.metadata);
    }
}
impl hirsel_artifact_store::Contents for RequestVerifierRows {
    fn get_content(
        &self,
        reference: Reference,
    ) -> Result<Bytes, hirsel_artifact_store::ContentReadError> {
        self.backend
            .read_product_content(reference)
            .map_err(Into::into)
    }
    fn put_content(&self, domain: Domain, bytes: &[u8]) -> Option<Reference> {
        self.backend.put_content(domain, bytes)
    }
    fn put_identified_content(&self, content: IdentifiedContent<'_>) -> Option<Reference> {
        self.backend.put_identified_content(content)
    }
}
impl hirsel_validate::VerifierRows for RequestVerifierRows {
    fn retain_fact(
        &self,
        bytes: u64,
    ) -> Result<Option<Arc<dyn Send + Sync>>, hirsel_validate::FactRefusal> {
        let bytes = bytes
            .checked_add((size_of::<ArchiveLease>() + 2 * size_of::<usize>()) as u64)
            .ok_or(hirsel_validate::FactRefusal)?;
        if !self.admission.charge(bytes) {
            return Err(hirsel_validate::FactRefusal);
        }
        Ok(Some(Arc::new(ArchiveLease {
            memory: self.admission.clone(),
            bytes,
        })))
    }

    fn retain_replay(
        &self,
        archive: &hirsel_product_codec::CheckedTir,
        overlay: Option<&hirsel_types::SignatureRef>,
        metadata: u64,
    ) -> Result<Option<Arc<dyn Send + Sync>>, hirsel_validate::FactRefusal> {
        let mut bytes = metadata
            .checked_add(
                hirsel_engine_runtime::ProductArchive::checked_import_charge(
                    archive.reference(),
                    archive.backing(),
                )
                .map_err(|_| hirsel_validate::FactRefusal)?,
            )
            .ok_or(hirsel_validate::FactRefusal)?
            .checked_add(
                archive
                    .frozen_binding_bytes()
                    .ok_or(hirsel_validate::FactRefusal)?,
            )
            .ok_or(hirsel_validate::FactRefusal)?;
        if let Some(overlay) = overlay {
            bytes = bytes
                .checked_add(
                    hirsel_query_products::retained::retained_signature_payload_bytes(overlay),
                )
                .ok_or(hirsel_validate::FactRefusal)?;
        }
        self.retain_fact(bytes)
    }
    fn row(&self, key: ContentHash) -> Option<Reference> {
        self.rows
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&key)
            .copied()
    }
    fn set_row(&self, key: ContentHash, reference: Reference) {
        let mut rows = self.rows.lock().unwrap_or_else(|error| error.into_inner());
        let additional = verifier_row_bytes(rows.len().saturating_add(1))
            .saturating_sub(verifier_row_bytes(rows.len()));
        if rows.contains_key(&key) || self.admission.charge(additional) {
            rows.insert(key, reference);
        }
    }
}
impl ResultStore for RequestStore {
    fn retention_scope(&self) -> Option<Box<dyn Send + Sync>> {
        self.backend.retention_scope()
    }

    fn capability(&self) -> ProviderCapability {
        ProviderCapability::RequestLocal(
            self.active_policy
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .upgrade()
                .map_or(self.baseline, |policy| policy.persistence()),
        )
    }
    fn archive_encoded(&self, boundary: ArchiveBoundary) {
        self.backend.archive_encoded(boundary);
    }

    fn installation_output(&self) -> Option<Arc<dyn ResultStore>> {
        self.backend.installation_output()
    }

    fn installed_package(
        &self,
        package: &hirsel_syntax::PackageId,
        compiler: ContentHash,
    ) -> Result<Arc<InstalledPackage>, String> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(installed) = &state.installed
            && installed.package == *package
            && installed.compiler == compiler
        {
            return Ok(installed.clone());
        }
        let installed = self.backend.installed_package(package, compiler)?;
        if state.installed.is_some() {
            return Err("operation already owns another installed package".into());
        }
        if !self.charge(&mut state, installed.operation_metadata_bytes()) {
            return Err("installed integrity metadata exceeds request transport admission".into());
        }
        state.installed = Some(installed.clone());
        Ok(installed)
    }
    fn operation_scope(&self) -> StoreOperation {
        let mut operation = self.backend.operation_scope();
        operation.persistence = Arc::new(OperationPersistence::new(self.baseline));
        operation.request_store = true;
        *self
            .active_policy
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Arc::downgrade(&operation.persistence);
        for (_, deferred) in self
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .associations
            .values_mut()
        {
            *deferred = false;
        }
        operation
    }
    fn admit_product_memory(&self, bytes: u64) -> Option<Arc<dyn Send + Sync>> {
        let admitted = self.try_admit_product_memory(bytes);
        if admitted.is_none() {
            self.memory.exhausted.store(true, Ordering::Release);
        }
        admitted
    }
    fn try_admit_product_memory(&self, bytes: u64) -> Option<Arc<dyn Send + Sync>> {
        let bytes = bytes.checked_add(hirsel_engine_admission::retained_arc_bytes(
            &std::mem::MaybeUninit::<ArchiveLease>::uninit(),
        ))?;
        if !self.memory.try_charge(bytes) {
            return None;
        }
        Some(Arc::new(ArchiveLease {
            memory: self.memory.clone(),
            bytes,
        }))
    }
    fn cache_product(&self, address: Address, action: Action, domain: Domain, bytes: &[u8]) {
        self.backend.cache_product(address, action, domain, bytes);
    }
    fn cache_query(&self, key: &EntryKey, reads: Reads, bytes: &[u8]) -> Option<Reference> {
        if !self.persistence().retains() {
            return None;
        }
        self.backend.cache_query(key, reads, bytes)
    }
    fn read_scope(&self) -> Option<Box<dyn Send + Sync>> {
        self.backend.read_scope()
    }
    fn name(&self) -> &'static str {
        self.backend.name()
    }
    fn local_product_store(&self) -> Option<hirsel_artifact_store::Store> {
        self.backend.local_product_store()
    }
    fn verifier(&self) -> Option<Arc<hirsel_validate::VerifierCache>> {
        self.verifier.clone()
    }
    // A backend that retains nothing is never read or written through the
    // transport; its live bytes are the request's own.
    fn get(&self, key: &EntryKey) -> Option<Bytes> {
        self.persistence()
            .retains()
            .then(|| self.backend.get(key))
            .flatten()
    }
    fn put(&self, key: &EntryKey, value: &[u8]) -> WriteOutcome {
        if self.persistence().retains() {
            self.backend.put(key, value)
        } else {
            WriteOutcome::Refused
        }
    }
    fn put_query(&self, kind: QueryKind, key: &EntryKey, action: Action, value: &[u8]) {
        assert_eq!(kind, key.2);
        if self.persistence().retains() {
            self.backend.put_query(kind, key, action, value);
        }
    }
    fn demand(&self, keys: &[EntryKey]) {
        if self.persistence().retains() {
            self.backend.demand(keys)
        }
    }
    fn adopt(&self, key: &EntryKey) {
        if self.persistence().retains() {
            self.backend.adopt(key)
        }
    }
    fn put_cache_content(&self, domain: Domain, bytes: &[u8]) -> Option<Reference> {
        // Read back through the backend only; the request transport never
        // carries a hit's sidecar.
        self.persistence()
            .retains()
            .then(|| self.backend.put_cache_content(domain, bytes))
            .flatten()
    }
    fn open_product_archive(
        &self,
        reference: Reference,
    ) -> Option<hirsel_engine_runtime::ProductArchive> {
        let bytes = self.get_content(reference)?;
        self.admit_product_archive(reference, bytes)
    }
    fn admit_product_archive(
        &self,
        reference: Reference,
        bytes: Bytes,
    ) -> Option<hirsel_engine_runtime::ProductArchive> {
        let charge =
            hirsel_engine_runtime::ProductArchive::checked_import_charge(reference, &bytes)
                .ok()?
                .checked_add(hirsel_engine_admission::retained_arc_bytes(
                    &std::mem::MaybeUninit::<ArchiveLease>::uninit(),
                ))?;
        if !self.memory.charge(charge) {
            return None;
        }
        let owner = Arc::new(ArchiveLease {
            memory: self.memory.clone(),
            bytes: charge,
        });
        hirsel_engine_runtime::ProductArchive::from_admitted_content(
            reference, bytes, charge, owner,
        )
        .ok()
    }
    fn copy_product_archive(
        &self,
        reference: Reference,
    ) -> Result<hirsel_engine_runtime::ProductArchive, ProductArchiveError> {
        let bytes = self.read_product_content(reference)?;
        if !bytes.matches_reference(reference) {
            return Err(ProductArchiveError::InvalidContent);
        }
        // The existing transport bound covers the buffer, its region header,
        // archive header and lease before allocating the detached payload.
        let working = reference
            .len
            .checked_add(ENTRY_CHARGE)
            .ok_or(ProductArchiveError::LayoutOverflow)?;
        self.memory.archive_charge(working, "archive copy")?;
        let copied = bytes.to_vec();
        drop(bytes);
        let bytes = Bytes::from(copied);
        let charge =
            hirsel_engine_runtime::ProductArchive::checked_import_charge(reference, &bytes)
                .map_err(ProductArchiveError::InvalidArchive)
                .and_then(|bytes| {
                    bytes
                        .checked_add(hirsel_engine_admission::retained_arc_bytes(
                            &std::mem::MaybeUninit::<ArchiveLease>::uninit(),
                        ))
                        .ok_or(ProductArchiveError::LayoutOverflow)
                })
                .and_then(|charge| {
                    if charge <= working {
                        Ok(charge)
                    } else {
                        Err(ProductArchiveError::CopyBound {
                            required: charge,
                            reserved: working,
                        })
                    }
                });
        let charge = match charge {
            Ok(charge) => charge,
            Err(error) => {
                self.memory.release(working);
                return Err(error);
            }
        };
        self.memory.release(working - charge);
        let owner = Arc::new(ArchiveLease {
            memory: self.memory.clone(),
            bytes: charge,
        });
        hirsel_engine_runtime::ProductArchive::from_admitted_content(
            reference, bytes, charge, owner,
        )
        .map_err(ProductArchiveError::InvalidArchive)
    }
    fn read_product_window(
        &self,
        reference: Reference,
        range: std::ops::Range<u64>,
    ) -> Result<Bytes, ProductArchiveError> {
        hirsel_artifact_store::window_read_bytes(reference, range.clone())
            .ok_or(ProductArchiveError::InvalidContent)?;
        if let Some((actual, bytes)) = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contents
            .get(&reference.output.0)
        {
            return if *actual == reference {
                bytes
                    .range(range.start, range.end - range.start, 1)
                    .ok_or(ProductArchiveError::InvalidContent)
            } else {
                Err(ProductArchiveError::InvalidContent)
            };
        }
        self.backend.read_product_window(reference, range)
    }
    fn read_product_content(&self, reference: Reference) -> Result<Bytes, ProductArchiveError> {
        if let Some((actual, bytes)) = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contents
            .get(&reference.output.0)
        {
            return if *actual == reference {
                Ok(bytes.clone())
            } else {
                Err(ProductArchiveError::InvalidContent)
            };
        }
        self.backend.read_product_content(reference)
    }
    fn get_content(&self, reference: Reference) -> Option<Bytes> {
        if let Some((actual, bytes)) = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contents
            .get(&reference.output.0)
        {
            return (*actual == reference).then(|| bytes.clone());
        }
        self.backend.get_content(reference)
    }
    fn put_content(&self, domain: Domain, bytes: &[u8]) -> Option<Reference> {
        self.put_identified_content(domain.identify(bytes)?)
    }
    fn put_identified_content(&self, content: IdentifiedContent<'_>) -> Option<Reference> {
        if self.memory.scratch.get().is_some()
            && let Some(reference) = self.backend.put_identified_content(content)
        {
            return Some(reference);
        }
        let expected = content.reference();
        let bytes = content.bytes();
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((actual, _)) = state.contents.get(&expected.output.0) {
            return (*actual == expected).then_some(expected);
        }
        let next = state.contents.len().checked_add(1)?;
        let growth =
            content_tree_bytes(next).saturating_sub(content_tree_bytes(state.contents.len()));
        let charge = Bytes::owned_with_owner_bytes(expected.len)?.checked_add(growth)?;
        if !self.charge(&mut state, charge) {
            return None;
        }
        let bytes = Bytes::from(bytes).retain_owner(self.memory.clone());
        state
            .contents
            .insert(expected.output.0, (expected, bytes.clone()));
        drop(state);
        self.backend.put_identified_content(content);
        Some(expected)
    }
    fn lookup_product(&self, address: Address) -> Option<Association> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .associations
            .get(&address.0)
            .map(|(association, _)| *association)
            .or_else(|| self.backend.lookup_product(address))
    }
    fn associate_product(&self, address: Address, action: Action, reference: Reference) {
        // An association can never make absent/truncated bytes transferable.
        if self.get_content(reference).is_none() {
            return;
        }
        self.associate_written_product(address, action, reference);
    }
    fn associate_written_product(&self, address: Address, action: Action, reference: Reference) {
        let association = Association { action, reference };
        // A Complete request publishes no association before its root.
        let deferred = self.persistence() == Persistence::Complete;
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        // Durable associations are a shared latest-result index. Another
        // admitted request can publish a different action at the same address
        // immediately, so they cannot replace this request's selected result.
        // Retain only metadata here, under the same bounded transport charge.
        let exists = state.associations.contains_key(&address.0);
        let Some(next) = state.associations.len().checked_add(usize::from(!exists)) else {
            self.memory.exhausted.store(true, Ordering::Release);
            return;
        };
        let growth =
            association_bytes(next).saturating_sub(association_bytes(state.associations.len()));
        if exists || self.charge(&mut state, growth) {
            state
                .associations
                .insert(address.0, (association, deferred));
            drop(state);
            if !deferred {
                self.backend.associate_product(address, action, reference);
            }
        }
    }
    fn materialize(&self) -> bool {
        let Some(_retention) = self.backend.retention_scope() else {
            return false;
        };
        let local = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contents
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut durable = true;
        for (reference, bytes) in &local {
            durable &= self.backend.put_content(reference.domain, bytes) == Some(*reference);
        }
        self.backend.materialize()
            && durable
            && local.iter().all(|(reference, bytes)| {
                self.backend
                    .get_content(*reference)
                    .is_some_and(|stored| &*stored == &**bytes)
            })
    }
    fn publish_root(&self, address: Address, action: Action, reference: Reference) -> bool {
        let selected = Association { action, reference };
        self.associate_product(address, action, reference);
        let persistence = self.persistence();
        if persistence != Persistence::Complete {
            return persistence.retains() && self.backend.lookup_product(address) == Some(selected);
        }
        let Some(operation) = self
            .active_policy
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .upgrade()
        else {
            return false;
        };
        {
            let mut retention = operation
                .retention
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if retention.is_none() {
                let Some(scope) = self.backend.retention_scope() else {
                    return false;
                };
                *retention = Some(scope);
            }
        }
        if self.lookup_product(address) != Some(selected) || !self.materialize() {
            return false;
        }
        let pending = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .associations
            .iter()
            .filter_map(|(key, (association, deferred))| {
                (*deferred && *key != address.0).then_some((Address(*key), *association))
            })
            .collect::<Vec<_>>();
        // Retention holds through the operation's finish. A failed association
        // leaves the deferred set intact so a retry cannot omit a component.
        for (component, association) in &pending {
            self.backend
                .associate_product(*component, association.action, association.reference);
            if self.backend.lookup_product(*component) != Some(*association) {
                return false;
            }
        }
        self.backend.associate_product(address, action, reference);
        if self.backend.lookup_product(address) != Some(selected) {
            return false;
        }
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        for (component, published) in pending.into_iter().chain([(address, selected)]) {
            if let Some((association, deferred)) = state.associations.get_mut(&component.0)
                && *association == published
            {
                *deferred = false;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn domain() -> Domain {
        Domain {
            kind: 0x4e00001f,
            schema: 1,
        }
    }
    fn address() -> Address {
        Address(ContentHash::of_bytes(b"request transport"))
    }
    #[test]
    fn archive_views_admit_complete_backing_and_outlive_the_request_without_its_table() {
        struct Backend(Mutex<Bytes>);
        impl ResultStore for Backend {
            fn admit_product_memory(&self, _bytes: u64) -> Option<std::sync::Arc<dyn Send + Sync>> {
                None
            }

            fn retention_scope(&self) -> Option<Box<dyn Send + Sync>> {
                None
            }

            fn capability(&self) -> ProviderCapability {
                ProviderCapability::Writable
            }

            fn get(&self, _: &EntryKey) -> Option<Bytes> {
                None
            }
            fn put(&self, _: &EntryKey, _: &[u8]) -> crate::WriteOutcome {
                crate::WriteOutcome::Refused
            }
            fn name(&self) -> &'static str {
                "archive-view-test"
            }
            fn get_content(&self, _: Reference) -> Option<Bytes> {
                Some(self.0.lock().unwrap().clone())
            }
        }
        let mut backing = vec![0; 4096];
        backing[100..108].copy_from_slice(b"complete");
        let bytes = Bytes::from(backing).range(100, 8, 1).unwrap();
        let reference = domain().reference(b"complete").unwrap();
        let required =
            hirsel_engine_runtime::ProductArchive::checked_import_charge(reference, &bytes)
                .unwrap()
                + hirsel_engine_admission::retained_arc_bytes(
                    &std::mem::MaybeUninit::<ArchiveLease>::uninit(),
                );
        assert!(required > 4096);
        let backend = Arc::new(Backend(Mutex::new(bytes.clone())));
        let provider = Arc::downgrade(&backend);
        let budget = hirsel_engine_admission::ProcessBudget::new(required);
        let short = RequestStore::new(backend.clone(), budget.reserve(required - 1).unwrap());
        assert!(short.open_product_archive(reference).is_none());
        assert_eq!(short.retained_bytes(), 0);
        drop(short);
        assert_eq!(budget.reserved(), 0);
        let request = Arc::new(RequestStore::new(
            backend.clone(),
            budget.reserve(required).unwrap(),
        ));
        let producer = Arc::downgrade(&request);
        *backend.0.lock().unwrap() = Bytes::from(b"replaced".as_slice());
        assert!(request.open_product_archive(reference).is_none());
        assert_eq!(request.retained_bytes(), 0);
        assert!(
            !request.exhausted(),
            "substitution does not spend admission"
        );
        *backend.0.lock().unwrap() = bytes;
        let archive = request.open_product_archive(reference).unwrap();
        assert_eq!(archive.retained_bytes(), required);
        assert_eq!(request.retained_bytes(), required);
        *backend.0.lock().unwrap() = Bytes::from(b"evicted".as_slice());
        drop(backend);
        drop(request);
        assert!(producer.upgrade().is_none());
        assert!(provider.upgrade().is_none());
        assert_eq!(budget.reserved(), required);
        let view = archive.clone();
        drop(archive);
        assert_eq!(view.bytes(), b"complete");
        assert_eq!(view.reference(), reference);
        assert_eq!(budget.reserved(), required);
        drop(view);
        assert_eq!(budget.reserved(), 0);
    }

    #[test]
    fn encoded_archive_moves_producer_backing_without_reading_the_provider() {
        struct Backend;
        impl ResultStore for Backend {
            fn admit_product_memory(&self, _bytes: u64) -> Option<std::sync::Arc<dyn Send + Sync>> {
                None
            }

            fn retention_scope(&self) -> Option<Box<dyn Send + Sync>> {
                None
            }

            fn capability(&self) -> ProviderCapability {
                ProviderCapability::Writable
            }

            fn get(&self, _: &EntryKey) -> Option<Bytes> {
                panic!("producer transfer read the provider")
            }
            fn put(&self, _: &EntryKey, _: &[u8]) -> crate::WriteOutcome {
                crate::WriteOutcome::Refused
            }
            fn name(&self) -> &'static str {
                "encoded-archive-test"
            }
        }
        let bytes = Bytes::from(vec![7; 4096]);
        let pointer = bytes.as_ptr();
        let reference = domain().reference(&bytes).unwrap();
        let required =
            hirsel_engine_runtime::ProductArchive::checked_import_charge(reference, &bytes)
                .unwrap()
                + hirsel_engine_admission::retained_arc_bytes(
                    &std::mem::MaybeUninit::<ArchiveLease>::uninit(),
                );
        let budget = hirsel_engine_admission::ProcessBudget::new(required);
        let request = RequestStore::new(Arc::new(Backend), budget.reserve(required).unwrap());
        let archive = request.admit_product_archive(reference, bytes).unwrap();
        assert_eq!(archive.bytes().as_ptr(), pointer);
        assert_eq!(archive.retained_bytes(), required);
        drop(request);
        assert_eq!(archive.bytes(), &[7; 4096]);
        drop(archive);
        assert_eq!(budget.reserved(), 0);
    }

    #[test]
    fn compact_archive_copy_admits_its_payload_and_survives_provider_eviction() {
        struct Backend(Mutex<Bytes>);
        impl ResultStore for Backend {
            fn admit_product_memory(&self, _bytes: u64) -> Option<std::sync::Arc<dyn Send + Sync>> {
                None
            }

            fn retention_scope(&self) -> Option<Box<dyn Send + Sync>> {
                None
            }

            fn capability(&self) -> ProviderCapability {
                ProviderCapability::Writable
            }

            fn get(&self, _: &EntryKey) -> Option<Bytes> {
                None
            }
            fn put(&self, _: &EntryKey, _: &[u8]) -> crate::WriteOutcome {
                crate::WriteOutcome::Refused
            }
            fn name(&self) -> &'static str {
                "compact-archive-test"
            }
            fn get_content(&self, _: Reference) -> Option<Bytes> {
                Some(self.0.lock().unwrap().clone())
            }
        }
        let mut backing = vec![0; 1 << 20];
        backing[100..108].copy_from_slice(b"complete");
        let bytes = Bytes::from(backing).range(100, 8, 1).unwrap();
        let pointer = bytes.as_ptr();
        let reference = domain().reference(b"complete").unwrap();
        let working = reference.len + ENTRY_CHARGE;
        let backend = Arc::new(Backend(Mutex::new(bytes)));
        let budget = hirsel_engine_admission::ProcessBudget::new(working);
        let short = RequestStore::new(backend.clone(), budget.reserve(working - 1).unwrap());
        assert!(matches!(short.copy_product_archive(reference),
            Err(ProductArchiveError::Admission {
                stage: "archive copy", requested, retained: 0, limit,
            }) if requested == working && limit == working - 1));
        assert_eq!(short.retained_bytes(), 0);
        drop(short);
        let request = RequestStore::new(backend.clone(), budget.reserve(working).unwrap());
        let archive = request.copy_product_archive(reference).unwrap();
        assert_ne!(archive.bytes().as_ptr(), pointer);
        assert!(archive.retained_bytes() <= working);
        assert_eq!(request.retained_bytes(), archive.retained_bytes());
        *backend.0.lock().unwrap() = Bytes::from(b"evicted".as_slice());
        assert!(matches!(
            request.copy_product_archive(reference),
            Err(ProductArchiveError::InvalidContent)
        ));
        assert_eq!(request.retained_bytes(), archive.retained_bytes());
        drop((backend, request));
        assert_eq!(archive.bytes(), b"complete");
        assert_eq!(archive.reference(), reference);
        assert_eq!(budget.reserved(), working);
        drop(archive);
        assert_eq!(budget.reserved(), 0);
    }

    #[test]
    fn shared_directory_requests_abort_independently_and_publish_after_join() {
        let directory = tempfile::tempdir().unwrap();
        let backend = Arc::new(DirStore::new(directory.path().join("cache")));
        let physical = backend.local_product_store().unwrap();
        let budget = hirsel_engine_admission::ProcessBudget::new(128 << 10);
        let first = RequestStore::new(backend.clone(), budget.reserve(64 << 10).unwrap());
        let second = RequestStore::new(backend, budget.reserve(64 << 10).unwrap());
        let cancelled = first.operation_scope();
        let completed = second.operation_scope();
        let rejected = first.put_content(domain(), b"cancelled").unwrap();
        let accepted = second.put_content(domain(), b"completed").unwrap();
        {
            let _producer = second.publication_scope();
            assert_eq!(&*second.get_content(accepted).unwrap(), b"completed");
        }
        assert!(physical.get(accepted).is_none());
        drop(cancelled);
        completed.finish().unwrap();
        assert!(physical.get(rejected).is_none());
        assert_eq!(&*physical.get(accepted).unwrap(), b"completed");
        drop((first, second));
        assert_eq!(budget.reserved(), 0);
    }
    #[test]
    fn request_lifecycle_changes_persistence_without_replacing_its_transport() {
        let backend = Arc::new(MemStore::default());
        let budget = hirsel_engine_admission::ProcessBudget::new(128 << 10);
        let request = RequestStore::new(backend.clone(), budget.reserve(64 << 10).unwrap());

        let precompute_root = Address(ContentHash::of_bytes(b"precompute root"));
        let precompute_action = Action(ContentHash::of_bytes(b"precompute action"));
        let precompute_bytes = b"precompute result";
        let precompute = request.operation_scope().completing();
        let precompute_reference = request.put_content(domain(), precompute_bytes).unwrap();
        assert!(request.publish_root(precompute_root, precompute_action, precompute_reference));
        precompute.finish().unwrap();
        assert_eq!(
            backend.lookup_product(precompute_root),
            Some(Association {
                action: precompute_action,
                reference: precompute_reference,
            })
        );
        assert_eq!(
            &*backend.get_content(precompute_reference).unwrap(),
            precompute_bytes
        );

        let check = request.operation_scope();
        let check_address = Address(ContentHash::of_bytes(b"check result"));
        let check_action = Action(ContentHash::of_bytes(b"check action"));
        let check_bytes = b"check result bytes";
        let check_reference = request.put_content(domain(), check_bytes).unwrap();
        request.associate_product(check_address, check_action, check_reference);
        assert_eq!(&*backend.get_content(check_reference).unwrap(), check_bytes);
        assert_eq!(
            backend.lookup_product(check_address),
            Some(Association {
                action: check_action,
                reference: check_reference,
            })
        );
        check.finish().unwrap();
        assert_eq!(
            backend.lookup_product(check_address),
            Some(Association {
                action: check_action,
                reference: check_reference,
            })
        );

        let complete = request.operation_scope().completing();
        let complete_address = Address(ContentHash::of_bytes(b"complete component"));
        let complete_action = Action(ContentHash::of_bytes(b"complete action"));
        let complete_bytes = b"complete component bytes";
        let complete_reference = request.put_content(domain(), complete_bytes).unwrap();
        request.associate_product(complete_address, complete_action, complete_reference);
        assert_eq!(backend.lookup_product(complete_address), None);
        let complete_root = Address(ContentHash::of_bytes(b"complete root"));
        let complete_root_action = Action(ContentHash::of_bytes(b"complete root action"));
        let complete_root_bytes = b"complete root bytes";
        let complete_root_reference = request.put_content(domain(), complete_root_bytes).unwrap();
        assert!(request.publish_root(complete_root, complete_root_action, complete_root_reference));
        complete.finish().unwrap();
        assert_eq!(
            backend.lookup_product(complete_address),
            Some(Association {
                action: complete_action,
                reference: complete_reference,
            })
        );

        let abandoned = request.operation_scope().completing();
        let abandoned_address = Address(ContentHash::of_bytes(b"abandoned component"));
        let abandoned_action = Action(ContentHash::of_bytes(b"abandoned action"));
        let abandoned_reference = request.put_content(domain(), b"abandoned bytes").unwrap();
        request.associate_product(abandoned_address, abandoned_action, abandoned_reference);
        assert_eq!(backend.lookup_product(abandoned_address), None);
        drop(abandoned);

        let next_complete = request.operation_scope().completing();
        let next_component_address = Address(ContentHash::of_bytes(b"next component"));
        let next_component_action = Action(ContentHash::of_bytes(b"next component action"));
        let next_component_reference = request
            .put_content(domain(), b"next component bytes")
            .unwrap();
        request.associate_product(
            next_component_address,
            next_component_action,
            next_component_reference,
        );
        let next_root = Address(ContentHash::of_bytes(b"next root"));
        let next_root_action = Action(ContentHash::of_bytes(b"next root action"));
        let next_root_reference = request.put_content(domain(), b"next root bytes").unwrap();
        assert!(request.publish_root(next_root, next_root_action, next_root_reference));
        next_complete.finish().unwrap();
        assert_eq!(backend.lookup_product(abandoned_address), None);
        assert_eq!(
            backend.lookup_product(next_component_address),
            Some(Association {
                action: next_component_action,
                reference: next_component_reference,
            })
        );

        drop(request);
        assert_eq!(budget.reserved(), 0);
    }
    /// A directory backend whose durable writes of one component domain fail
    /// `refusals` times, as a full or failing disk would.
    struct Refusing {
        disk: DirStore,
        refusals: std::sync::atomic::AtomicUsize,
    }
    fn component() -> Domain {
        Domain {
            kind: 0x4e00_0020,
            schema: 1,
        }
    }
    impl ResultStore for Refusing {
        fn admit_product_memory(&self, bytes: u64) -> Option<std::sync::Arc<dyn Send + Sync>> {
            self.disk.admit_product_memory(bytes)
        }

        fn retention_scope(&self) -> Option<Box<dyn Send + Sync>> {
            self.disk.retention_scope()
        }

        fn capability(&self) -> ProviderCapability {
            self.disk.capability()
        }

        fn get(&self, key: &EntryKey) -> Option<Bytes> {
            self.disk.get(key)
        }
        fn put(&self, key: &EntryKey, value: &[u8]) -> crate::WriteOutcome {
            self.disk.put(key, value)
        }
        fn local_product_store(&self) -> Option<hirsel_artifact_store::Store> {
            self.disk.local_product_store()
        }
        fn get_content(&self, reference: Reference) -> Option<Bytes> {
            self.disk.get_content(reference)
        }
        fn put_content(&self, domain: Domain, bytes: &[u8]) -> Option<Reference> {
            let refused = domain == component()
                && self
                    .refusals
                    .try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1))
                    .is_ok();
            if refused {
                return None;
            }
            self.disk.put_content(domain, bytes)
        }
        fn lookup_product(&self, address: Address) -> Option<Association> {
            self.disk.lookup_product(address)
        }
        fn associate_product(&self, address: Address, action: Action, reference: Reference) {
            self.disk.associate_product(address, action, reference)
        }
        fn name(&self) -> &'static str {
            "refusing-directory"
        }
    }
    #[test]
    fn complete_roots_become_visible_only_with_their_whole_closure_and_aborts_leave_none() {
        // Required transport retains the original bytes even when a cache
        // rejects them or evicts a previously accepted write. A cache without
        // closure pins must refuse Complete before exposing any root.
        assert!(
            MemStore::bounded(0)
                .put_content(domain(), b"required")
                .is_none()
        );
        let lost = SimStore::with_faults(SimFaults {
            lose_puts: vec![0],
            ..Default::default()
        });
        assert!(lost.put_content(domain(), b"required").is_none());
        for scratch in [false, true] {
            for cap in [0, 512] {
                let parent = tempfile::tempdir().unwrap();
                let backend = Arc::new(MemStore::bounded(cap));
                let budget = hirsel_engine_admission::ProcessBudget::new(64 << 10);
                let allowance = budget.reserve(64 << 10).unwrap();
                let request = if scratch {
                    RequestStore::with_scratch(backend.clone(), allowance, parent.path(), 4096)
                        .unwrap()
                } else {
                    RequestStore::new(backend.clone(), allowance)
                };
                let operation = request.operation_scope().completing();
                let part = request
                    .put_content(component(), b"required component")
                    .unwrap();
                let output = request.put_content(domain(), b"root").unwrap();
                // Each competing write fits, but together they evict the
                // request's earlier content from the bounded FIFO cache.
                for bytes in [b'a', b'b', b'c', b'd'] {
                    backend.put_content(domain(), &[bytes; 256]);
                }
                assert!(backend.get_content(part).is_none());
                assert_eq!(&*request.get_content(part).unwrap(), b"required component");
                assert_eq!(&*request.get_content(output).unwrap(), b"root");
                request.associate_product(
                    Address(ContentHash::of_bytes(b"cited")),
                    Action(address().0),
                    part,
                );
                assert!(!request.publish_root(address(), Action(address().0), output));
                operation.finish().unwrap();
                assert!(backend.lookup_product(address()).is_none());
            }
        }
        for scratch in [false, true] {
            let parent = tempfile::tempdir().unwrap();
            let backend = Arc::new(DirStore::new(parent.path().join("retaining")));
            let collector = backend.local_product_store().unwrap();
            let budget = hirsel_engine_admission::ProcessBudget::new(64 << 10);
            let allowance = budget.reserve(64 << 10).unwrap();
            let request = if scratch {
                RequestStore::with_scratch(backend.clone(), allowance, parent.path(), 4096).unwrap()
            } else {
                RequestStore::new(backend.clone(), allowance)
            };
            let operation = request.operation_scope().completing();
            let part = request
                .put_content(component(), b"retained component")
                .unwrap();
            let output = request.put_content(domain(), b"retained root").unwrap();
            let at = Address(ContentHash::of_bytes(b"retained component address"));
            request.associate_product(at, Action(at.0), part);
            assert!(request.publish_root(address(), Action(address().0), output));
            collector.collect(0);
            operation.finish().unwrap();
            let fresh = DirStore::new(parent.path().join("retaining"));
            for (at, expected) in [
                (at, b"retained component".as_slice()),
                (address(), b"retained root".as_slice()),
            ] {
                let reference = fresh.lookup_product(at).unwrap().reference;
                assert_eq!(&*fresh.get_content(reference).unwrap(), expected);
            }
        }
        let cited = Address(ContentHash::of_bytes(b"cited component"));
        // The rule itself is the oracle: a fresh open of the directory finds
        // the root only when the component it cites is associated and stored,
        // and no association names content the directory cannot read.
        let reopen = |path: &std::path::Path| {
            let fresh = DirStore::new(path.to_path_buf());
            let found = [address(), cited].map(|at| {
                fresh.lookup_product(at).inspect(|association| {
                    assert!(
                        fresh.get_content(association.reference).is_some(),
                        "an association names missing content"
                    )
                })
            });
            if found[0].is_some() {
                assert!(found[1].is_some(), "a root is visible without its closure");
            }
            found.map(|association| association.is_some())
        };
        // (refusals, abort before the root, abort after it) ->
        // (root published by the request, root visible to a fresh open)
        for (refusals, abort_before, abort_after, published, visible) in [
            (0, false, false, true, true),
            (1, false, false, true, true),
            (usize::MAX, false, false, false, false),
            (0, true, false, false, false),
            (0, false, true, true, false),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("products");
            let backend = Arc::new(Refusing {
                disk: DirStore::new(path.clone()),
                refusals: refusals.into(),
            });
            let budget = hirsel_engine_admission::ProcessBudget::new(64 << 10);
            let request = RequestStore::new(backend, budget.reserve(64 << 10).unwrap());
            // Content is durable before any association is attempted.
            let contents = request.operation_scope().completing();
            assert_eq!(request.persistence(), Persistence::Complete);
            let part = request.put_content(component(), b"component").unwrap();
            let output = request.put_content(domain(), b"root").unwrap();
            contents.finish().unwrap();
            let associations = request.operation_scope().completing();
            request.associate_product(cited, Action(cited.0), part);
            // The request itself reads its deferred component immediately.
            assert!(request.lookup_product(cited).is_some());
            if abort_before {
                drop(associations);
                assert_eq!(reopen(&path), [false, false]);
                continue;
            }
            assert_eq!(
                request.publish_root(address(), Action(address().0), output),
                published,
                "refusals {refusals}"
            );
            if abort_after {
                drop(associations);
            } else {
                associations.finish().unwrap();
            }
            assert_eq!(reopen(&path), [visible, visible], "refusals {refusals}");
            drop(request);
            assert_eq!(budget.reserved(), 0);
        }
    }
    #[test]
    fn request_associations_charge_their_physical_tree_and_replacements_keep_its_grant() {
        for count in [32u32, 64, 128] {
            let backend = Arc::new(MemStore::default());
            let reference = backend.put_content(domain(), b"selected").unwrap();
            let allowance = 64 << 10;
            let budget = hirsel_engine_admission::ProcessBudget::new(allowance);
            let action = Action(address().0);
            for index in 0..count {
                let address = Address(ContentHash::of_bytes(&index.to_le_bytes()));
                backend.associate_product(address, action, reference);
            }
            let request = RequestStore::new(backend, budget.reserve(allowance).unwrap());
            let (_, actual) = crate::test_alloc::measured(|| {
                for index in 0..count {
                    let address = Address(ContentHash::of_bytes(&index.to_le_bytes()));
                    request.associate_product(address, action, reference);
                }
            });
            let charged = request.retained_bytes();
            assert!(!request.exhausted());
            assert!(actual.live > 0);
            assert!(
                charged >= actual.live as u64,
                "charged={charged}, actual={}",
                actual.live
            );
            assert!(charged >= actual.peak as u64);
            assert!(charged < u64::from(count) * ENTRY_CHARGE);
            for index in 0..count {
                let address = Address(ContentHash::of_bytes(&index.to_le_bytes()));
                assert_eq!(
                    request.lookup_product(address),
                    Some(Association { action, reference })
                );
                request.associate_product(address, action, reference);
                assert_eq!(request.retained_bytes(), charged);
            }
            drop(request);
            assert_eq!(budget.reserved(), 0);
        }
    }
    #[test]
    fn request_contents_charge_shared_tree_and_cover_physical_allocations() {
        for count in [32u32, 64, 128] {
            let allowance = 64 << 10;
            let budget = hirsel_engine_admission::ProcessBudget::new(allowance);
            let request =
                RequestStore::new(Arc::new(NoneStore), budget.reserve(allowance).unwrap());
            let mut references = Vec::with_capacity(count as usize);
            let (_, actual) = crate::test_alloc::measured(|| {
                for index in 0..count {
                    references.push(request.put_content(domain(), &index.to_le_bytes()).unwrap());
                }
            });
            let charged = request.retained_bytes();
            assert!(!request.exhausted());
            assert!(actual.live > 0);
            assert!(charged >= actual.live as u64);
            assert!(
                charged >= actual.peak as u64,
                "charged={charged}, actual={}",
                actual.peak
            );
            assert!(charged <= allowance);
            for (index, reference) in references.into_iter().enumerate() {
                let value = (index as u32).to_le_bytes();
                assert_eq!(&*request.get_content(reference).unwrap(), &value);
            }
            assert!(
                request
                    .put_content(domain(), &vec![0; allowance as usize])
                    .is_none()
            );
            assert!(request.retained_bytes() <= allowance);
            drop(request);
            assert_eq!(budget.reserved(), 0);
        }
    }
    #[test]
    fn request_transport_survives_durable_failure_and_held_child_retains_admission() {
        let allowance = ENTRY_CHARGE * 4;
        let budget = hirsel_engine_admission::ProcessBudget::new(allowance);
        let request = Arc::new(RequestStore::new(
            Arc::new(NoneStore),
            budget.reserve(allowance).unwrap(),
        ));
        let reference = request.put_content(domain(), b"required product").unwrap();
        let action = Action(ContentHash::of_bytes(b"action"));
        request.associate_product(address(), action, reference);
        assert_eq!(
            request.lookup_product(address()),
            Some(Association { action, reference })
        );
        assert_eq!(
            &*request.get_content(reference).unwrap(),
            b"required product"
        );
        assert!(!request.exhausted());
        assert!(request.verifier().is_none());
        assert!(request.local_product_store().is_none());
        let held = request
            .get_content(reference)
            .unwrap()
            .range(9, 7, 1)
            .unwrap();
        drop(request);
        assert_eq!(budget.reserved(), allowance);
        assert_eq!(&*held, b"product");
        drop(held);
        assert_eq!(budget.reserved(), 0);
        // The next request has no durable success or process-global overlay.
        let next = RequestStore::new(Arc::new(NoneStore), budget.reserve(allowance).unwrap());
        assert!(next.lookup_product(address()).is_none());
        assert!(next.get_content(reference).is_none());
    }
    #[test]
    fn request_transport_zero_tiny_and_concurrent_allowances_never_expand() {
        for limit in [0, 1, ENTRY_CHARGE, ENTRY_CHARGE + 3] {
            let budget = hirsel_engine_admission::ProcessBudget::new(limit);
            let request = RequestStore::new(Arc::new(NoneStore), budget.reserve(limit).unwrap());
            let value = request.put_content(domain(), b"1234");
            assert!(value.is_none());
            assert!(request.exhausted());
            assert!(request.retained_bytes() <= limit);
            assert!(budget.reserve(1).is_err());
        }
        // Refusing a live lease still permits the required-spill boundary.
        {
            let allowance = ENTRY_CHARGE * 4;
            let budget = hirsel_engine_admission::ProcessBudget::new(allowance);
            let parent = tempfile::tempdir().unwrap();
            let request = RequestStore::with_scratch(
                Arc::new(NoneStore),
                budget.reserve(allowance).unwrap(),
                parent.path(),
                allowance,
            )
            .unwrap();
            assert!(request.try_admit_product_memory(allowance + 1).is_none());
            assert!(!request.exhausted());
            let spilled = request.put_content(domain(), b"required spill").unwrap();
            assert_eq!(&*request.get_content(spilled).unwrap(), b"required spill");
            assert!(!request.exhausted());
            assert!(request.retained_bytes() <= allowance);
        }
        let allowance = ENTRY_CHARGE * 4;
        let budget = hirsel_engine_admission::ProcessBudget::new(allowance);
        let a = RequestStore::new(Arc::new(NoneStore), budget.reserve(allowance / 2).unwrap());
        let b = RequestStore::new(Arc::new(NoneStore), budget.reserve(allowance / 2).unwrap());
        assert!(budget.reserve(1).is_err());
        assert!(a.put_content(domain(), b"a").is_some());
        assert!(b.put_content(domain(), b"b").is_some());
        assert_eq!(budget.reserved(), allowance);
    }
    #[test]
    fn request_transport_forwards_exact_local_authority_and_checks_reference_domains() {
        let backend = Arc::new(MemStore::default());
        let expected = backend.verifier().unwrap();
        let budget = hirsel_engine_admission::ProcessBudget::new(0);
        let request = RequestStore::new(backend.clone(), budget.reserve(0).unwrap());
        assert!(Arc::ptr_eq(&request.verifier().unwrap(), &expected));
        let reference = backend.put_content(domain(), b"durable").unwrap();
        assert_eq!(request.retained_bytes(), 0);
        let mut wrong = reference;
        wrong.domain.schema += 1;
        assert!(request.get_content(wrong).is_none());
        request.associate_product(address(), Action(address().0), wrong);
        assert!(request.lookup_product(address()).is_none());
        assert!(!request.exhausted());
    }

    #[test]
    fn request_associations_remain_selected_across_interleaved_durable_publications() {
        let backend = Arc::new(MemStore::default());
        let budget = hirsel_engine_admission::ProcessBudget::new(ENTRY_CHARGE * 2);
        let a = RequestStore::new(backend.clone(), budget.reserve(ENTRY_CHARGE).unwrap());
        let b = RequestStore::new(backend.clone(), budget.reserve(ENTRY_CHARGE).unwrap());
        let first = backend.put_content(domain(), b"first request").unwrap();
        let second = backend.put_content(domain(), b"second request").unwrap();
        let action_a = Action(ContentHash::of_bytes(b"first action"));
        let action_b = Action(ContentHash::of_bytes(b"second action"));
        let selected_a = Association {
            action: action_a,
            reference: first,
        };
        let selected_b = Association {
            action: action_b,
            reference: second,
        };
        a.associate_product(address(), action_a, first);
        b.associate_product(address(), action_b, second);
        assert_eq!(a.lookup_product(address()), Some(selected_a));
        assert_eq!(b.lookup_product(address()), Some(selected_b));
        assert_eq!(backend.lookup_product(address()), Some(selected_b));
        // Repeated publication by one request spends no additional allowance
        // and cannot change the other request's settled read.
        a.associate_product(address(), action_a, first);
        assert_eq!(backend.lookup_product(address()), Some(selected_a));
        assert_eq!(b.lookup_product(address()), Some(selected_b));
        assert_eq!(a.retained_bytes(), association_bytes(1));
        assert_eq!(b.retained_bytes(), association_bytes(1));
        assert!(!a.exhausted() && !b.exhausted());
        drop(a);
        drop(b);
        assert_eq!(budget.reserved(), 0);
        let next = RequestStore::new(backend, budget.reserve(ENTRY_CHARGE).unwrap());
        assert_eq!(next.lookup_product(address()), Some(selected_a));
    }

    #[test]
    fn durable_association_pinning_obeys_the_admitted_metadata_bound() {
        let backend = Arc::new(MemStore::default());
        let budget = hirsel_engine_admission::ProcessBudget::new(ENTRY_CHARGE);
        let request = RequestStore::new(backend.clone(), budget.reserve(ENTRY_CHARGE).unwrap());
        let reference = backend.put_content(domain(), b"durable content").unwrap();
        let action = Action(address().0);
        request.associate_product(address(), action, reference);
        for index in 1u32..4 {
            let next = Address(ContentHash::of_bytes(&index.to_le_bytes()));
            request.associate_product(next, action, reference);
            assert!(request.lookup_product(next).is_some());
        }
        let other = Address(ContentHash::of_bytes(b"another address"));
        request.associate_product(other, action, reference);
        assert!(request.exhausted());
        assert_eq!(request.retained_bytes(), association_bytes(4));
        assert!(request.retained_bytes() <= ENTRY_CHARGE);
        assert!(request.lookup_product(other).is_none());
        assert!(backend.lookup_product(other).is_none());
        assert!(request.lookup_product(address()).is_some());
    }
    #[test]
    fn optional_cache_projections_never_spend_required_transport_admission() {
        let directory = tempfile::tempdir().unwrap();
        let backend = Arc::new(MemStore::default());
        let allowance = 64 << 10;
        let budget = hirsel_engine_admission::ProcessBudget::new(allowance);
        let request = RequestStore::with_scratch(
            backend.clone(),
            budget.reserve(allowance).unwrap(),
            directory.path(),
            1 << 20,
        )
        .unwrap();
        let before = request.retained_bytes();
        let action = Action(address().0);
        for index in 0u32..256 {
            let address = Address(ContentHash::of_bytes(&index.to_le_bytes()));
            request.cache_product(address, action, domain(), b"optional projection");
            assert!(backend.lookup_product(address).is_some());
            let key = EntryKey(address.0, ContentHash::default(), QueryKind::Summary);
            assert!(
                request
                    .cache_query(&key, Reads::empty(), b"optional compact facts")
                    .is_some()
            );
            assert_eq!(Entry::load(&*backend, &key).variants.len(), 1);
        }
        assert_eq!(request.retained_bytes(), before);
        assert!(!request.exhausted());
        let required = request.put_content(domain(), b"required output").unwrap();
        request.associate_product(address(), action, required);
        assert!(request.lookup_product(address()).is_some());
        assert!(!request.exhausted());
        drop(request);
        assert_eq!(budget.reserved(), 0);

        struct RefusedCache;
        impl ResultStore for RefusedCache {
            fn admit_product_memory(&self, _bytes: u64) -> Option<std::sync::Arc<dyn Send + Sync>> {
                None
            }

            fn retention_scope(&self) -> Option<Box<dyn Send + Sync>> {
                None
            }

            fn capability(&self) -> ProviderCapability {
                ProviderCapability::Writable
            }

            fn get(&self, _: &EntryKey) -> Option<Bytes> {
                None
            }
            fn put(&self, _: &EntryKey, _: &[u8]) -> crate::WriteOutcome {
                crate::WriteOutcome::Refused
            }
            fn put_content(&self, _: Domain, _: &[u8]) -> Option<Reference> {
                None
            }
            fn name(&self) -> &'static str {
                "refused-cache"
            }
        }
        let refusing = RequestStore::with_scratch(
            Arc::new(RefusedCache),
            budget.reserve(allowance).unwrap(),
            directory.path(),
            1 << 20,
        )
        .unwrap();
        let before = refusing.retained_bytes();
        let key = EntryKey(address().0, ContentHash::default(), QueryKind::Summary);
        assert!(
            refusing
                .cache_query(&key, Reads::empty(), &vec![0; 2 * allowance as usize])
                .is_none()
        );
        assert_eq!(refusing.retained_bytes(), before);
        assert!(!refusing.exhausted());
        assert!(
            refusing
                .put_content(domain(), b"required output after cache refusal")
                .is_some()
        );
        assert!(!refusing.exhausted());
        drop(refusing);
        assert_eq!(budget.reserved(), 0);

        let disabled = RequestStore::new(Arc::new(NoneStore), budget.reserve(0).unwrap());
        disabled.cache_product(address(), action, domain(), b"optional projection");
        let key = EntryKey(address().0, ContentHash::default(), QueryKind::Summary);
        assert!(
            disabled
                .cache_query(&key, Reads::empty(), b"optional compact facts")
                .is_none()
        );
        assert!(disabled.lookup_product(address()).is_none());
        assert_eq!(disabled.retained_bytes(), 0);
        assert!(!disabled.exhausted());
    }
    /// Authority is shared only over the session's own store and verify mode,
    /// and never over a store whose operations issue their own authority.
    #[test]
    fn session_verifier_serves_only_its_own_store_and_verify_mode() {
        use crate::VerifyStore::{Off, Recompute};
        let budget = hirsel_engine_admission::ProcessBudget::new(64 << 20);
        let parent = tempfile::tempdir().unwrap();
        let session = |backend: &Arc<dyn ResultStore>| {
            SessionVerifier::new(
                backend.clone(),
                Off,
                budget.reserve(4 << 20).unwrap(),
                parent.path(),
                1 << 20,
            )
            .unwrap()
        };
        let verifier = |session: &SessionVerifier, backend: &Arc<dyn ResultStore>, verify| {
            RequestStore::with_scratch_and_session_verifier(
                session,
                backend.clone(),
                verify,
                budget.reserve(4 << 20).unwrap(),
                parent.path(),
                1 << 20,
            )
            .unwrap()
            .verifier()
            .unwrap()
        };
        let empty: Arc<dyn ResultStore> = Arc::new(NoneStore);
        let shared = session(&empty);
        let first = verifier(&shared, &empty, Off);
        assert!(Arc::ptr_eq(&first, &verifier(&shared, &empty, Off)));
        let other: Arc<dyn ResultStore> = Arc::new(NoneStore);
        assert!(!Arc::ptr_eq(&first, &verifier(&shared, &other, Off)));
        assert!(!Arc::ptr_eq(&first, &verifier(&shared, &empty, Recompute)));
        let root = tempfile::tempdir().unwrap();
        let durable: Arc<dyn ResultStore> = Arc::new(crate::DirStore::new(root.path().into()));
        let per_operation = session(&durable);
        assert!(!Arc::ptr_eq(
            &verifier(&per_operation, &durable, Off),
            &verifier(&per_operation, &durable, Off)
        ));
    }
    #[test]
    fn verifier_transport_admits_its_actual_index_and_releases_all_rows() {
        use hirsel_validate::VerifierRows;
        let allowance = 64 << 10;
        let budget = hirsel_engine_admission::ProcessBudget::new(allowance);
        let admission = Arc::new(Admission {
            memory: budget.reserve(allowance).unwrap(),
            used: AtomicU64::new(0),
            exhausted: AtomicBool::new(false),
            scratch: OnceLock::new(),
            verifier: OnceLock::new(),
            reads: OnceLock::new(),
        });
        let reference = domain().reference(b"verifier-owned content").unwrap();
        let (rows, allocated) = crate::test_alloc::measured(|| {
            let rows = RequestVerifierRows {
                backend: Arc::new(NoneStore),
                admission: admission.clone(),
                rows: Mutex::new(BTreeMap::new()),
                metadata: 0,
            };
            for index in 0u32..256 {
                rows.set_row(ContentHash::of_bytes(&index.to_le_bytes()), reference);
            }
            rows
        });
        assert!(!admission.exhausted.load(Ordering::Acquire));
        assert!(allocated.live > 0);
        assert!(allocated.live as u64 <= admission.used.load(Ordering::Acquire));
        for index in 0u32..256 {
            let key = ContentHash::of_bytes(&index.to_le_bytes());
            assert_eq!(rows.row(key), Some(reference));
            let before = admission.used.load(Ordering::Acquire);
            rows.set_row(key, reference);
            assert_eq!(admission.used.load(Ordering::Acquire), before);
        }
        drop(rows);
        assert_eq!(admission.used.load(Ordering::Acquire), 0);

        // A selected store may return a small archive slice of a larger
        // backing. Retention owns that whole allocation, not only its range.
        let records = hirsel_product_codec::TypeRecords::new(
            hirsel_types::TypeInterner::new(None),
            Arc::new(hirsel_artifact_store::Memory::default()),
        );
        let definition = hirsel_tir::Definition {
            caller_bodies: vec![],
            protocol_associations: vec![],
            types: Arc::from([]),
            path: hirsel_syntax::DefId::intern(&hirsel_syntax::DefPath::core("retained-frame")),
            signature_hash: ContentHash::default(),
            bodies: vec![],
            entry: hirsel_tir::BodyId(0),
            contracts: vec![],
        };
        let encoded = records.encode_tir(&definition, |_| None).unwrap();
        let reference = hirsel_product_codec::CheckedTir::DOMAIN
            .reference(&encoded)
            .unwrap();
        let archive = |size| {
            let mut backing = vec![0; size];
            backing[100..100 + encoded.len()].copy_from_slice(&encoded);
            hirsel_product_codec::CheckedTir::open(
                Bytes::from(backing)
                    .range(100, encoded.len() as u64, 1)
                    .unwrap(),
                reference,
                records.clone(),
                |_, _| None,
            )
            .unwrap()
            .frozen_bindings()
            .unwrap()
        };
        let rows = RequestVerifierRows {
            backend: Arc::new(NoneStore),
            admission: admission.clone(),
            rows: Mutex::new(BTreeMap::new()),
            metadata: 0,
        };
        let retained = rows
            .retain_replay(&archive(8 << 10), None, 0)
            .unwrap()
            .unwrap();
        assert!(admission.used.load(Ordering::Acquire) >= 8 << 10);
        drop(retained);
        assert_eq!(admission.used.load(Ordering::Acquire), 0);
        assert!(rows.retain_replay(&archive(128 << 10), None, 0).is_err());
        assert!(admission.exhausted.load(Ordering::Acquire));
        assert_eq!(admission.used.load(Ordering::Acquire), 0);
        drop(rows);
        drop(admission);
        assert_eq!(budget.reserved(), 0);
    }
}
