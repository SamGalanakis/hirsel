//! Front query namespace in the shared artifact store. Hits retain mapped
//! ranges; no second payload allocation is made before checking query reads.
use super::{EntryKey, ProviderCapability, ResultStore};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// The store format version in the directory name.
pub const FORMAT: u32 = 15;
/// The default size cap.
pub const DEFAULT_CAP: u64 = 2 << 30;

#[derive(Clone, Debug)]
pub struct DirStore {
    root: PathBuf,
    physical: Option<hirsel_artifact_store::Store>,
    read_only: Vec<hirsel_artifact_store::Store>,
    cap: u64,
    /// The local verifier. With a registration, the verifier this store uses
    /// is the one selected against it.
    verifier: std::sync::Arc<hirsel_validate::VerifierCache>,
    registration: Option<Arc<Registration>>,
    operation: bool,
    installed_capture: Option<Arc<Mutex<Option<super::installed::OpenedInstallation>>>>,
    /// Each read-only root's installation header, opened once per operation.
    /// A root without a valid header supplies no rows.
    headers: Arc<
        Mutex<std::collections::BTreeMap<PathBuf, Option<Arc<super::installed::InstalledRecords>>>>,
    >,
    installation: Option<std::sync::Arc<std::sync::Mutex<InstallationRecorder>>>,
    /// Set once `root` exists. Clones and operations share the answer, so a
    /// put no longer re-creates an existing directory.
    root_ready: Arc<std::sync::OnceLock<()>>,
}

/// The compiler identity and private state an installation registration is
/// selected against, and what was selected. A session selects on first use;
/// each operation selects once at its entry and never keeps an earlier pin,
/// so one request authenticates one registration generation, once.
#[derive(Debug)]
struct Registration {
    identity: hirsel_syntax::ContentHash,
    state: PathBuf,
    selected: std::sync::OnceLock<Selected>,
}
/// The verifier over each root's authenticated registration, and the header
/// association each registration bound: the store opens that header for its
/// root, never a reselected one, so V's authority and the products it serves
/// are one generation's.
#[derive(Debug)]
struct Selected {
    verifier: std::sync::Arc<hirsel_validate::VerifierCache>,
    headers: std::collections::BTreeMap<
        PathBuf,
        hirsel_artifact_store::registration::AuthenticatedHeader,
    >,
    /// The program-record domain of the registrations selected here.
    programs: Option<hirsel_artifact_store::programs::ProgramAuthority>,
    /// An operation's read-only roots' index generations, pinned before their
    /// registrations are read and held while it opens their headers.
    _leases: Vec<hirsel_artifact_store::Batch>,
}

/// What an installation producer adopted or wrote. Publication projects it
/// to the installation: a function of the inputs, equal to a cold producer's.
///
/// Rows are published only for what this run's queries used: an entry it
/// wrote or adopted after a hit's own checks (`ResultStore::adopt`), a product
/// association it wrote, or one it looked up and then read checked content
/// through. A lookup alone publishes nothing. Each entry is published holding
/// only the variant for this run's key; other variants stay in the cache.
#[derive(Debug, Default)]
struct InstallationRecorder {
    entries: std::collections::BTreeMap<
        hirsel_syntax::ContentHash,
        (EntryKey, super::Action, super::Reference),
    >,
    /// The row each address's last lookup returned, until adopted.
    looked:
        std::collections::BTreeMap<hirsel_syntax::ContentHash, (super::Action, super::Reference)>,
    products:
        std::collections::BTreeMap<hirsel_syntax::ContentHash, (super::Action, super::Reference)>,
    /// Looked-up product associations, by the content whose read adopts them.
    candidates: std::collections::BTreeMap<
        hirsel_syntax::ContentHash,
        Vec<(hirsel_syntax::ContentHash, super::Action, super::Reference)>,
    >,
    contents: std::collections::BTreeMap<hirsel_syntax::ContentHash, super::Reference>,
    cache_only: std::collections::BTreeSet<hirsel_syntax::ContentHash>,
    /// Every use of a row or content, by the traced work running when it
    /// was used, so a part's rows are found by reachability over the trace's
    /// edges rather than by which reader came first.
    uses: super::Uses,
}

impl InstallationRecorder {
    /// Project to one variant per entry. Query-record contents (entry blobs
    /// and variant payloads) are listed only when a published row reaches
    /// them, so rejected variants and superseded blobs never are.
    fn project(
        &self,
        store: &hirsel_artifact_store::Store,
    ) -> std::io::Result<super::installed::InstallationRecords> {
        let mut rows = self.products.clone();
        let mut reached = std::collections::BTreeSet::new();
        let batch = store.batch();
        for (address, &(key, action, blob)) in &self.entries {
            let key = &key;
            let bytes = store
                .get_owned(blob)
                .map_err(|_| std::io::Error::other("an adopted record's content is unreadable"))?;
            // Whichever write path stored it, a query entry for this key is
            // projected; any other record (`put`) is published as written.
            let Some(entry) = super::Entry::decode_for(key, &bytes) else {
                rows.insert(*address, (action, blob));
                continue;
            };
            let variant = entry
                .current(key)
                .cloned()
                .ok_or_else(|| std::io::Error::other("an adopted entry has no current variant"))?;
            reached.insert(variant.output);
            // A stored one-variant entry is already this projection's content:
            // every writer stores `encode_for`'s canonical frame.
            if entry.variants.len() == 1 && blob.domain == super::entry_domain(key.2) {
                rows.insert(*address, (entry.action(key), blob));
                continue;
            }
            let entry = super::Entry {
                variants: vec![variant],
            };
            let reference = store.put(super::entry_domain(key.2), &entry.encode_for(key))?;
            rows.insert(*address, (entry.action(key), reference));
        }
        batch.finish()?;
        reached.extend(rows.values().map(|(_, reference)| *reference));
        let contents = self
            .contents
            .values()
            .filter(|reference| {
                !self.cache_only.contains(&reference.output.0)
                    && !super::query_record_domain(reference.domain)
            })
            .chain(&reached)
            .map(|reference| (reference.output.0, *reference))
            .collect();
        Ok(super::installed::InstallationRecords { rows, contents })
    }
}

/// The action of a raw record written by `put`, never a query entry's.
fn raw_action(key: &EntryKey) -> super::Action {
    super::Action(hirsel_syntax::ContentHash::of_parts(
        "hirsel.result.raw-action.v1",
        [key.0.0.as_slice(), key.1.0.as_slice()],
    ))
}

/// What a collection found and removed.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GcReport {
    pub entries: u64,
    pub bytes: u64,
    pub removed: u64,
    pub removed_bytes: u64,
}

impl DirStore {
    pub fn new(root: PathBuf) -> Self {
        Self::with_physical(
            root.clone(),
            hirsel_artifact_store::Store::rebuildable(root),
        )
    }
    /// An explicit package producer's output. It records the producer's
    /// complete closure and writes a strict installation store.
    pub fn installation(root: PathBuf) -> Self {
        Self {
            installation: Some(Default::default()),
            ..Self::with_physical(
                root.clone(),
                hirsel_artifact_store::Store::installation(root),
            )
        }
    }
    fn with_physical(root: PathBuf, physical: hirsel_artifact_store::Store) -> Self {
        physical.set_cache_cap(DEFAULT_CAP);
        Self {
            verifier: std::sync::Arc::new(hirsel_validate::VerifierCache::directory_store(
                physical.clone(),
            )),
            physical: Some(physical),
            root,
            registration: None,
            read_only: vec![],
            cap: DEFAULT_CAP,
            operation: false,
            installed_capture: None,
            headers: Arc::default(),
            installation: None,
            root_ready: Arc::default(),
        }
    }
    /// Select an independently constructed local verifier for installation.
    pub fn with_installation_verifier(
        mut self,
        verifier: std::sync::Arc<hirsel_validate::VerifierCache>,
    ) -> Self {
        self.verifier = verifier;
        self
    }
    /// This store, selecting `identity`'s registration in `state` on first use.
    pub(crate) fn with_registration(
        &self,
        identity: hirsel_syntax::ContentHash,
        state: PathBuf,
    ) -> Self {
        Self {
            registration: Some(Arc::new(Registration {
                identity,
                state,
                selected: Default::default(),
            })),
            ..self.clone()
        }
    }
    /// What this store selected against its registration, if it has one:
    /// each root's current registration that authenticates. An installation
    /// producer's own output root is never its authority.
    fn selection(&self) -> Option<&Selected> {
        let registration = self.registration.as_ref()?;
        Some(registration.selected.get_or_init(|| {
            let leases = if self.operation {
                self.read_only.iter().map(|store| store.batch()).collect()
            } else {
                vec![]
            };
            let verifier = self.verifier.clone();
            let mut headers = std::collections::BTreeMap::new();
            let mut authorities = vec![];
            for root in self
                .physical
                .iter()
                .filter(|_| self.installation.is_none())
                .chain(&self.read_only)
            {
                if let Some(registered) =
                    hirsel_artifact_store::registration::Installation::registered(
                        root.root(),
                        &registration.state,
                        registration.identity,
                    )
                {
                    headers.insert(root.root().to_owned(), registered.header);
                    authorities.push(registered.authority);
                }
            }
            Selected {
                verifier,
                headers,
                programs: hirsel_artifact_store::programs::ProgramAuthority::new(authorities),
                _leases: leases,
            }
        }))
    }
    fn selected_verifier(&self) -> std::sync::Arc<hirsel_validate::VerifierCache> {
        self.selection().map_or_else(
            || self.verifier.clone(),
            |selected| selected.verifier.clone(),
        )
    }
    /// The header its registration authenticated for `store`'s root. A root
    /// without one supplies no V authority and is read at its current header.
    fn header_of(
        &self,
        store: &hirsel_artifact_store::Store,
    ) -> Option<hirsel_artifact_store::registration::AuthenticatedHeader> {
        self.selection()?.headers.get(store.root()).copied()
    }
    /// Publish the installation and return the header association it
    /// published, which V's registration binds.
    pub fn publish_installation(
        &self,
        package: super::InstalledPackage,
    ) -> std::io::Result<(super::Address, hirsel_artifact_store::Association)> {
        let records = self
            .installation
            .as_ref()
            .ok_or_else(|| std::io::Error::other("installation was not recorded"))?;
        let store = hirsel_artifact_store::Store::installation(self.root.clone());
        let records = records
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .project(&store)?;
        package.publish(&store, &records)
    }
    /// The uses this installation recorded so far.
    pub fn take_installation_uses(&self) -> super::Uses {
        self.installation
            .as_ref()
            .map_or_else(super::Uses::default, |records| {
                std::mem::take(
                    &mut records
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .uses,
                )
            })
    }
    fn record(&self, change: impl FnOnce(&mut InstallationRecorder)) {
        if let Some(records) = &self.installation {
            change(&mut records.lock().unwrap_or_else(|error| error.into_inner()));
        }
    }
    fn record_content(&self, reference: super::Reference) {
        self.record(|records| {
            records.uses.content(reference.output.0);
            if !records.cache_only.contains(&reference.output.0) {
                records.contents.insert(reference.output.0, reference);
            }
            // A checked read of a looked-up association's content adopts it.
            for (address, action, candidate) in records
                .candidates
                .remove(&reference.output.0)
                .unwrap_or_default()
            {
                if candidate == reference {
                    records.uses.row(address);
                    records.products.insert(address, (action, reference));
                }
            }
        });
    }
    fn record_product(
        &self,
        address: super::Address,
        action: super::Action,
        reference: super::Reference,
    ) {
        self.record_content(reference);
        self.record(|records| {
            records.uses.row(address.0);
            records.products.insert(address.0, (action, reference));
        });
    }
    fn record_lookup(&self, key: &EntryKey, row: super::Association) {
        self.record(|records| {
            records.looked.insert(key.0, (row.action, row.reference));
        });
    }
    /// Read installed ordinary products with the same checked format. Writes,
    /// collection and verifier associations remain in this store's own root.
    pub fn with_read_only_root(mut self, root: PathBuf) -> Self {
        if root != self.root && !self.read_only.iter().any(|store| store.root() == root) {
            self.read_only.push(hirsel_artifact_store::Store::new(root));
            self.installed_capture = Some(Default::default());
        }
        self
    }
    pub fn with_cap(mut self, cap: u64) -> Self {
        self.cap = cap;
        if let Some(physical) = &self.physical {
            physical.set_cache_cap(cap);
        }
        self
    }
    /// `$XDG_CACHE_HOME/hirsel/results-v<N>`, else `$HOME/.cache/...`.
    pub fn default_location() -> Option<Self> {
        Some(
            Self::new(
                Self::cache_base()?
                    .join("hirsel")
                    .join(format!("results-v{FORMAT}")),
            )
            .with_installed_products(),
        )
    }
    fn cache_base() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_CACHE_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .filter(|v| !v.is_empty())
                    .map(|h| PathBuf::from(h).join(".cache"))
            })?;
        // Like Go's os.UserCacheDir: an absent or relative location is not
        // authority to write a cache into the current working directory.
        if !base.is_absolute() {
            return None;
        }
        Some(base)
    }
    /// V's own local associations are independent of query-result writes.
    /// Neither a run's result directory nor an installed product root selects
    /// this directory. Only V receives the writable handle.
    fn installed_verifier(installed: &Path) -> std::sync::Arc<hirsel_validate::VerifierCache> {
        Self::cache_base()
            .and_then(|base| Self::local_verifier(&base, installed))
            .map(std::sync::Arc::new)
            .unwrap_or_default()
    }
    fn local_verifier(base: &Path, installed: &Path) -> Option<hirsel_validate::VerifierCache> {
        // Canonicalize the existing cache base before appending the private
        // namespace. Refuse ambiguous/unavailable locations rather than letting
        // product selection point at the verifier's trusted associations.
        let base = base.canonicalize().ok()?;
        let installed = installed.canonicalize().ok()?;
        let root = base.join("hirsel").join("validator-v2");
        let root = root.canonicalize().unwrap_or(root);
        if root.starts_with(&installed) || installed.starts_with(&root) {
            return None;
        }
        // A symlink on the existing namespace parent also needs resolving.
        let parent = root.parent()?;
        if parent.exists() && parent.canonicalize().ok()? != parent {
            return None;
        }
        let physical = hirsel_artifact_store::Store::rebuildable(root);
        physical.set_cache_cap(DEFAULT_CAP);
        Some(hirsel_validate::VerifierCache::directory_store(physical))
    }
    /// Installed ordinary products are independent of writable-cache policy.
    pub fn installed_root() -> Option<PathBuf> {
        std::env::var_os("HIRSEL_PRODUCTS")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::current_exe()
                    .ok()
                    .map(|executable| executable.with_file_name("hirsel-std.products"))
            })
    }
    pub(crate) fn installed_only() -> Option<Self> {
        let root = Self::installed_root()?;
        let verifier = Self::installed_verifier(&root);
        Some(Self {
            read_only: vec![hirsel_artifact_store::Store::new(root.clone())],
            root,
            physical: None,
            cap: 0,
            verifier,
            registration: None,
            operation: false,
            installed_capture: Some(Default::default()),
            headers: Arc::default(),
            installation: None,
            root_ready: Arc::default(),
        })
    }
    /// Compose environment-selected local storage with installed ordinary
    /// products. Explicit library providers remain exactly caller configured.
    pub(crate) fn with_installed_products(mut self) -> Self {
        if let Some(installed) = Self::installed_root() {
            self = self.with_read_only_root(installed);
        }
        self
    }
    fn roots(&self) -> impl Iterator<Item = (bool, &hirsel_artifact_store::Store)> {
        self.physical
            .iter()
            .map(|root| (true, root))
            .chain(self.read_only.iter().map(|root| (false, root)))
    }
    /// The installation a read-only root serves rows from: the captured
    /// one when it is this root's, else the root's own header.
    fn header(
        &self,
        store: &hirsel_artifact_store::Store,
    ) -> Option<Arc<super::installed::InstalledRecords>> {
        if let Some(records) = self
            .installed_records()
            .filter(|records| records.root == store.root())
        {
            return Some(records);
        }
        self.headers
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .entry(store.root().to_owned())
            .or_insert_with(|| {
                super::InstalledPackage::open_records(store, self.header_of(store)).ok()
            })
            .clone()
    }
    fn installed_records(&self) -> Option<Arc<super::installed::InstalledRecords>> {
        let captured = self
            .installed_capture
            .as_ref()?
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        Some(captured.as_ref()?.records.clone())
    }
    fn publication_store(&self) -> Option<&hirsel_artifact_store::Store> {
        let store = self.physical.as_ref()?;
        // A batch can retain unpublished bytes before its eventual disk flush.
        // An impossible directory (notably ENOTDIR) must remain a cache miss,
        // rather than serving those buffered writes as successful local hits.
        if self.root_ready.get().is_none() {
            fs::create_dir_all(&self.root).ok()?;
            let _ = self.root_ready.set(());
        }
        Some(store)
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    /// Collect immutable outputs and action roots with bounded scratch.
    pub fn gc(&self, cap: u64) -> GcReport {
        let Some(physical) = &self.physical else {
            return GcReport::default();
        };
        let report = physical.collect(cap);
        GcReport {
            entries: report.entries,
            bytes: report.bytes,
            removed: report.removed,
            removed_bytes: report.removed_bytes,
        }
    }
    pub fn cap(&self) -> u64 {
        self.cap
    }
    fn publish(&self, key: &EntryKey, action: super::Action, value: &[u8]) -> super::WriteOutcome {
        use hirsel_artifact_store::Address;
        let Some(store) = self.publication_store() else {
            return super::WriteOutcome::Refused;
        };
        let batch = store.batch();
        let write = || -> std::io::Result<super::Reference> {
            let reference = store.put(super::entry_domain(key.2), value)?;
            store.associate(Address(key.0), action, reference)?;
            Ok(reference)
        };
        match write().and_then(|reference| batch.finish().map(|()| reference)) {
            Ok(reference) if self.get(key).is_some_and(|stored| &*stored == value) => {
                self.record(|records| {
                    records.uses.row(key.0);
                    records.entries.insert(key.0, (*key, action, reference));
                });
                super::WriteOutcome::Stored(reference)
            }
            Ok(_) => super::WriteOutcome::Refused,
            Err(_) => super::WriteOutcome::Refused,
        }
    }
}

impl ResultStore for DirStore {
    fn admit_product_memory(&self, _bytes: u64) -> Option<std::sync::Arc<dyn Send + Sync>> {
        // Disk capacity provides no admitted RAM. RequestStore supplies live
        // leases from the caller's process reservation.
        None
    }

    fn retention_scope(&self) -> Option<Box<dyn Send + Sync>> {
        Some(Box::new(self.physical.as_ref()?.retention_scope().ok()?))
    }

    fn capability(&self) -> ProviderCapability {
        match &self.physical {
            Some(_) => ProviderCapability::Writable,
            None => ProviderCapability::ReadOnly,
        }
    }

    fn installation_output(&self) -> Option<Arc<dyn ResultStore>> {
        self.installation
            .as_ref()
            .map(|_| Arc::new(self.clone()) as Arc<dyn ResultStore>)
    }

    fn installed_package(
        &self,
        package: &hirsel_syntax::PackageId,
        compiler: hirsel_syntax::ContentHash,
        admit: &mut dyn FnMut(u64) -> bool,
    ) -> Result<std::sync::Arc<super::InstalledPackage>, String> {
        let mut captured = self
            .installed_capture
            .as_ref()
            .map(|slot| slot.lock().unwrap_or_else(|error| error.into_inner()));
        if let Some(installed) = captured.as_deref().and_then(Option::as_ref)
            && installed.package.package == *package
            && installed.package.compiler == compiler
        {
            if !admit(installed.package.operation_metadata_bytes()) {
                return Err("installed package exceeds request admission".into());
            }
            return Ok(installed.package.clone());
        }
        // Each root's attempt is charged as it opens; a refused admission
        // fails before that root allocates and captures nothing.
        let mut failures = Vec::new();
        for root in &self.read_only {
            match super::InstalledPackage::open_header(
                root,
                self.header_of(root),
                package,
                compiler,
                admit,
            ) {
                Ok(package) => {
                    if let Some(captured) = &mut captured {
                        **captured = Some(package.clone());
                    }
                    return Ok(package.package);
                }
                Err(error) => failures.push(error),
            }
        }
        if failures.is_empty() {
            Err(format!(
                "installed products {}: no installed package root attached",
                self.root.display()
            ))
        } else {
            Err(failures.join("; "))
        }
    }
    fn for_operation(&self) -> Option<std::sync::Arc<dyn ResultStore>> {
        if self.operation {
            return None;
        }
        let physical = self.physical.as_ref().map(|store| store.for_operation());
        let selected = Self {
            root: self.root.clone(),
            verifier: self.verifier.for_operation(physical.as_ref()),
            physical,
            read_only: self
                .read_only
                .iter()
                .map(|store| store.for_operation())
                .collect(),
            cap: self.cap,
            operation: true,
            installed_capture: Some(Default::default()),
            headers: Arc::default(),
            registration: None,
            installation: self.installation.clone(),
            root_ready: self.root_ready.clone(),
        };
        let selected = match &self.registration {
            Some(registration) => {
                let selected =
                    selected.with_registration(registration.identity, registration.state.clone());
                selected.selection();
                selected
            }
            None => selected,
        };
        Some(std::sync::Arc::new(selected))
    }
    fn program_authority(&self) -> Option<hirsel_artifact_store::programs::ProgramAuthority> {
        // Only an operation's own selection; a store without a registration,
        // or an installation producer's output root, has no domain.
        if !self.operation || self.installation.is_some() {
            return None;
        }
        self.selection()?.programs.clone()
    }
    fn for_compiler(
        &self,
        identity: hirsel_syntax::ContentHash,
    ) -> Option<std::sync::Arc<dyn ResultStore>> {
        if self
            .registration
            .as_ref()
            .is_some_and(|registration| registration.identity == identity)
        {
            return None;
        }
        let state = hirsel_artifact_store::registration::Installation::default_state()?;
        Some(std::sync::Arc::new(self.with_registration(identity, state)))
    }
    fn read_scope(&self) -> Option<Box<dyn Send + Sync>> {
        if self.read_only.is_empty() {
            return None;
        }
        Some(Box::new(
            self.read_only
                .iter()
                .map(|store| store.batch())
                .collect::<Vec<_>>(),
        ))
    }
    fn local_product_store(&self) -> Option<hirsel_artifact_store::Store> {
        self.physical.clone()
    }
    fn lookup_product(&self, address: super::Address) -> Option<super::Association> {
        // A front QueryId shares its logical address with its variant row.
        // Keep the product index disjoint, as the byte providers do.
        let address = super::Address(super::product_association_key(address).0);
        let row = self.roots().find_map(|(local, store)| {
            if local {
                store.lookup(address)
            } else {
                self.header(store)?.lookup(address)
            }
        })?;
        self.record(|records| {
            records
                .candidates
                .entry(row.reference.output.0)
                .or_default()
                .push((address.0, row.action, row.reference));
        });
        Some(row)
    }
    fn associate_product(
        &self,
        address: super::Address,
        action: super::Action,
        reference: super::Reference,
    ) {
        // Shared immutable roots may supply ordinary content, but local writes
        // and GC remain in the writable root. A missing reference is a miss.
        if self.get_content(reference).is_some() {
            self.associate_written_product(address, action, reference);
        }
    }
    fn associate_written_product(
        &self,
        address: super::Address,
        action: super::Action,
        reference: super::Reference,
    ) {
        let Some(store) = self.publication_store() else {
            return;
        };
        let address = super::Address(super::product_association_key(address).0);
        let _ = store.associate(address, action, reference);
        self.record_product(address, action, reference);
    }
    fn verifier(&self) -> Option<std::sync::Arc<hirsel_validate::VerifierCache>> {
        Some(self.selected_verifier())
    }
    fn read_product_content(
        &self,
        reference: super::Reference,
    ) -> Result<super::Bytes, super::ProductArchiveError> {
        if let Some(bytes) = self
            .installed_records()
            .and_then(|records| records.content(reference))
        {
            self.record_content(reference);
            return Ok(bytes);
        }
        let mut refusal = super::ContentReadError::NotFound;
        for (local, store) in self.roots() {
            if !local
                && self
                    .installed_records()
                    .is_some_and(|records| records.root == store.root())
            {
                continue;
            }
            let read = if local {
                hirsel_artifact_store::Contents::get_content(store, reference)
            } else {
                store.get_owned(reference)
            };
            match read {
                Ok(bytes) => {
                    if local {
                        store.touch(
                            hirsel_artifact_store::Address(reference.output.0),
                            reference,
                        );
                    }
                    self.record_content(reference);
                    return Ok(bytes);
                }
                Err(super::ContentReadError::NotFound) => (),
                Err(error) => refusal = error,
            }
        }
        Err(super::ProductArchiveError::Content(refusal))
    }
    fn get_content(&self, reference: super::Reference) -> Option<super::Bytes> {
        self.read_product_content(reference).ok()
    }
    fn put_content(&self, domain: super::Domain, bytes: &[u8]) -> Option<super::Reference> {
        self.put_identified_content(domain.identify(bytes)?)
    }
    fn put_identified_content(
        &self,
        content: super::IdentifiedContent<'_>,
    ) -> Option<super::Reference> {
        let store = self.publication_store()?;
        let reference = store.put_identified(content).ok()?;
        store.get_identified(content)?;
        self.record_content(reference);
        Some(reference)
    }
    fn get(&self, key: &EntryKey) -> Option<super::Bytes> {
        for (local, store) in self.roots() {
            if !local {
                // A read-only root is an installation: its header is the only
                // row source, and its listed content is checked on read.
                let Some(records) = self.header(store) else {
                    continue;
                };
                let Some(row) = records.lookup(hirsel_artifact_store::Address(key.0)) else {
                    continue;
                };
                if row.reference.domain != super::entry_domain(key.2) {
                    continue;
                }
                let bytes = records.content(row.reference)?;
                self.record_lookup(key, row);
                return Some(bytes);
            }
            let Some(opened) = store.lookup_open(hirsel_artifact_store::Address(key.0)) else {
                if local
                    && let Some(path) =
                        store.association_path(hirsel_artifact_store::Address(key.0))
                {
                    let _ = fs::remove_file(path);
                }
                continue;
            };
            let previous = opened.association;
            // Query code validates prior input/output identities before decoding.
            if previous.reference.domain != super::entry_domain(key.2) {
                continue;
            }
            let bytes = self
                .installed_records()
                .and_then(|records| records.content(previous.reference))
                .or_else(|| opened.get());
            let Some(bytes) = bytes else {
                if local {
                    let _ = fs::remove_file(store.output_path(previous.reference));
                }
                continue;
            };
            if local {
                store.touch(hirsel_artifact_store::Address(key.0), previous.reference);
            }
            self.record_lookup(key, previous);
            return Some(bytes);
        }
        None
    }
    fn put(&self, key: &EntryKey, value: &[u8]) -> crate::WriteOutcome {
        self.publish(key, raw_action(key), value)
    }
    fn adopt(&self, key: &EntryKey) {
        self.record(|records| {
            if let Some((action, reference)) = records.looked.get(&key.0).copied() {
                records.uses.row(key.0);
                records.entries.insert(key.0, (*key, action, reference));
            }
        });
    }
    fn put_cache_content(&self, domain: super::Domain, bytes: &[u8]) -> Option<super::Reference> {
        let store = self.publication_store()?;
        let reference = store.put(domain, bytes).ok()?;
        if !store.get(reference).is_some_and(|stored| &*stored == bytes) {
            return None;
        }
        self.record(|records| {
            records.cache_only.insert(reference.output.0);
        });
        Some(reference)
    }
    fn put_query(
        &self,
        kind: hirsel_query_products::QueryKind,
        key: &EntryKey,
        action: super::Action,
        value: &[u8],
    ) {
        assert_eq!(kind, key.2);
        self.publish(key, action, value);
    }
    fn name(&self) -> &'static str {
        if self.physical.is_some() {
            "dir@1"
        } else {
            "none@1"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Domain, ResultStore};

    #[test]
    fn installed_audit_checks_unique_references_once_and_reads_verify_on_demand() {
        use crate::{
            Action, Address, InstalledPackage, InstalledSource, installed::InstallationRecords,
        };
        use hirsel_syntax::{ContentHash, FileContext, PackageId};
        let directory = tempfile::tempdir().unwrap();
        let installed = directory.path().join("installed");
        let writer = hirsel_artifact_store::Store::rebuildable(installed.clone());
        let compiler = ContentHash::of_bytes(b"compiler");
        let package = PackageId {
            name: "std".into(),
            version: "1".into(),
        };
        let mut records = InstallationRecords::default();
        let mut inputs = Vec::new();
        let batch = writer.batch();
        // More packed references than integrity slots, and more standalone
        // regions than the bounded region cache can retain.
        for n in 0u64..150 {
            let bytes = if n < 80 {
                n.to_le_bytes().to_vec()
            } else {
                vec![n as u8; 5000]
            };
            let reference = writer
                .put(
                    crate::entry_domain(hirsel_query_products::QueryKind::Parse),
                    &bytes,
                )
                .unwrap();
            let address = Address(ContentHash::of_parts(
                "installed-cost-law",
                [&n.to_le_bytes()[..]],
            ));
            let action = Action(ContentHash::of_bytes(&n.to_le_bytes()));
            for address in [address, Address(crate::product_association_key(address).0)] {
                writer.associate(address, action, reference).unwrap();
                records.rows.insert(address.0, (action, reference));
            }
            records.contents.insert(reference.output.0, reference);
            inputs.push((address, action, reference, bytes));
        }
        batch.finish().unwrap();
        InstalledPackage::new(
            compiler,
            package.clone(),
            b"manifest".to_vec(),
            vec![InstalledSource {
                path: "/absent/source.hir".into(),
                logical: "std@1/source.hir".into(),
                context: FileContext {
                    package: package.clone(),
                    modules: vec![],
                },
                text: "pub fn value()->u8{1u8}".into(),
            }],
            vec![],
            Default::default(),
        )
        .publish(&writer, &records)
        .unwrap();
        let provider = DirStore::new(directory.path().join("local")).with_read_only_root(installed);
        let local = provider.physical.as_ref().unwrap();
        let batch = local.batch();
        for (address, action, reference, bytes) in &inputs {
            assert_eq!(local.put(reference.domain, bytes).unwrap(), *reference);
            for address in [
                *address,
                Address(crate::product_association_key(*address).0),
            ] {
                local.associate(address, *action, *reference).unwrap();
            }
        }
        batch.finish().unwrap();
        let local_statistics = local.enable_statistics();
        let statistics = provider.read_only[0].enable_statistics();
        let audit = InstalledPackage::audit(&provider.read_only[0]).unwrap();
        // The whole-store audit retains the existing full-closure work law:
        // each input once, plus what open reads (the header and its row,
        // content and index sections and the one source text).
        let interface = 5;
        let checks = inputs.len() as u64 + interface;
        assert_eq!(statistics.snapshot().counters["ContentChecks"], checks);
        let opened = statistics.snapshot();
        for _ in 0..2 {
            for (_, _, reference, expected) in &inputs {
                let content = audit.get(*reference).unwrap();
                assert_eq!(&*content, expected);
                assert!(content.matches_reference(*reference));
            }
        }
        let consumed = statistics.snapshot();
        for counter in ["ContentChecks", "SegmentOpens", "StandaloneOpens"] {
            assert_eq!(
                consumed.counters[counter], opened.counters[counter],
                "{counter}"
            );
        }
        assert!(consumed.counters["RetainedRegionPeakCount"] <= 64);
        assert!(consumed.counters["RetainedRegionPeakBytes"] <= 32 << 20);
        InstalledPackage::audit(&provider.read_only[0]).unwrap();
        assert_eq!(statistics.snapshot().counters["ContentChecks"], 2 * checks);

        let before = statistics.snapshot();
        let operation = provider.for_operation().unwrap();
        let _scope = operation.read_scope();
        let package_owner = operation
            .installed_package(&package, compiler, &mut |_| true)
            .unwrap();
        assert_eq!(
            statistics.snapshot().counters["ContentChecks"],
            before.counters["ContentChecks"] + interface,
            "opening verifies the interface, not the unused closure"
        );
        let source = package_owner.source_records().unwrap();
        assert!(source.metadata_bytes() >= inputs.len() * size_of::<crate::Reference>());
        #[cfg(unix)]
        assert!(
            source.metadata_bytes() < 70 * 5000,
            "file handles must not retain all payload mappings"
        );
        for _ in 0..2 {
            for (address, action, reference, expected) in &inputs {
                let key = EntryKey(
                    address.0,
                    ContentHash::default(),
                    hirsel_query_products::QueryKind::Parse,
                );
                let content = operation.get_content(*reference).unwrap();
                assert_eq!(&*content, expected);
                assert!(content.matches_reference(*reference));
                assert_eq!(&*operation.get(&key).unwrap(), expected);
                assert_eq!(&*source.get(&key).unwrap(), expected);
                assert_eq!(
                    operation.lookup_product(*address),
                    Some(crate::Association {
                        action: *action,
                        reference: *reference
                    })
                );
            }
        }
        assert_eq!(local_statistics.snapshot().counters["ContentChecks"], 0);
        // An independent operation verifies its interface before demand too.
        let consumed = statistics.snapshot();
        let next = provider.for_operation().unwrap();
        next.installed_package(&package, compiler, &mut |_| true)
            .unwrap();
        assert_eq!(
            statistics.snapshot().counters["ContentChecks"],
            consumed.counters["ContentChecks"] + interface
        );

        // Integrity ownership uses the existing transport grant once, and
        // source snapshots cannot keep that grant alive after the request.
        let budget = hirsel_engine_admission::ProcessBudget::new(1 << 20);
        let request = super::super::RequestStore::with_scratch(
            Arc::new(provider.clone()),
            budget.reserve(1 << 20).unwrap(),
            &std::env::temp_dir(),
            1 << 30,
        )
        .unwrap();
        let installed = request
            .installed_package(&package, compiler, &mut |_| true)
            .unwrap();
        let charged = request.retained_bytes();
        assert!(charged >= installed.operation_metadata_bytes());
        assert!(charged > 0);
        request
            .installed_package(&package, compiler, &mut |_| true)
            .unwrap();
        assert_eq!(request.retained_bytes(), charged);
        let sources = installed.source_records().unwrap();
        drop(request);
        assert_eq!(budget.reserved(), 0);
        for (address, _, _, expected) in &inputs {
            let key = EntryKey(
                address.0,
                ContentHash::default(),
                hirsel_query_products::QueryKind::Parse,
            );
            assert_eq!(&*sources.get(&key).unwrap(), expected);
        }
        let refused =
            super::super::RequestStore::in_memory(Arc::new(provider), budget.reserve(0).unwrap());
        assert!(
            refused
                .installed_package(&package, compiler, &mut |_| true)
                .is_err()
        );
        assert!(refused.exhausted());
    }

    #[test]
    fn query_variants_and_product_associations_survive_each_others_publication_and_reopen() {
        use crate::{Action, Address, Entry, VariantRef};
        for kind in [
            hirsel_query_products::QueryKind::Modes,
            hirsel_query_products::QueryKind::Lower,
        ] {
            let directory = tempfile::tempdir().unwrap();
            let store = DirStore::new(directory.path().to_owned());
            let key = EntryKey::new(
                Default::default(),
                &hirsel_syntax::DefPath::core("separate-query-and-product"),
                Default::default(),
                kind,
                hirsel_query_products::LoweringPolicy::Reachable,
            );
            let variant = VariantRef::publish(
                &store,
                &key,
                hirsel_query_products::reads::Reads::empty(),
                b"body",
            )
            .unwrap();
            let mut entry = Entry::default();
            entry.insert(variant.clone());
            store.put(&key, &entry.encode_for(&key));
            let address = Address(key.0);
            let action = Action(hirsel_syntax::ContentHash::of_bytes(b"selected product"));
            store.associate_product(address, action, variant.output);
            drop(store);
            let reopened = DirStore::new(directory.path().to_owned());
            assert_eq!(Entry::load(&reopened, &key), entry);
            assert_eq!(
                reopened.lookup_product(address).unwrap().reference,
                variant.output
            );
            // Rewriting a variant row must preserve the selected association too.
            reopened.put(&key, &entry.encode_for(&key));
            let product = reopened.lookup_product(address).unwrap();
            assert_eq!(product.action, action);
            assert_eq!(product.reference, variant.output);
            assert_eq!(&*reopened.get_content(product.reference).unwrap(), b"body");
            crate::tests::install(
                directory.path(),
                &[key.0, crate::product_association_key(address).0],
                &[variant.output],
            );
            let installed = DirStore::new(directory.path().join("local"))
                .with_read_only_root(directory.path().to_owned());
            assert_eq!(Entry::load(&installed, &key), entry);
            assert_eq!(installed.lookup_product(address), Some(product));
        }
    }

    /// The header alone defines an installation, so its mutable index may be
    /// rewritten by a later run. Bug: a reader resolving a row through that
    /// index would serve another row's valid, same-kind content. Oracle: the
    /// content each row was installed with.
    #[test]
    fn an_index_row_pointing_at_another_rows_content_never_serves_it() {
        use crate::{Action, Address, Domain, InstalledPackage, ResultStore};
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("installed");
        let seed = DirStore::new(root.clone());
        let key = |name: &str| {
            EntryKey::new(
                Default::default(),
                &hirsel_syntax::DefPath::core(name),
                Default::default(),
                hirsel_query_products::QueryKind::Signature,
                hirsel_query_products::LoweringPolicy::Reachable,
            )
        };
        let (first, second) = (key("altered-first"), key("altered-second"));
        seed.put(&first, b"first record");
        seed.put(&second, b"second record");
        let domain = Domain {
            kind: 123,
            schema: 1,
        };
        let product = Address(hirsel_syntax::ContentHash::of_bytes(b"first product"));
        let other = Address(hirsel_syntax::ContentHash::of_bytes(b"second product"));
        for (address, bytes) in [(product, b"first bytes"), (other, b"other bytes")] {
            let reference = seed.put_content(domain, bytes).unwrap();
            seed.associate_written_product(address, Action(address.0), reference);
        }
        let (product_row, other_row) = (
            crate::product_association_key(product).0,
            crate::product_association_key(other).0,
        );
        let header =
            crate::tests::install(&root, &[first.0, second.0, product_row, other_row], &[]);
        let store = hirsel_artifact_store::Store::new(root.clone());
        let installed = store.lookup(Address(product_row)).unwrap();
        let state = directory.path().join("state");
        let identity = hirsel_syntax::ContentHash::of_bytes(b"compiler");
        hirsel_artifact_store::registration::Installation::begin(&root, &state, identity)
            .unwrap()
            .finish(header)
            .unwrap();
        let registered = || {
            hirsel_artifact_store::registration::Installation::registered(&root, &state, identity)
        };
        assert!(registered().is_some());

        // Each row's index entry now names the other row's content.
        for (row, from) in [(first.0, second.0), (product_row, other_row)] {
            let replacement = store.lookup(Address(from)).unwrap();
            store
                .associate(Address(row), replacement.action, replacement.reference)
                .unwrap();
        }

        let package = hirsel_syntax::PackageId {
            name: "seed".into(),
            version: "1".into(),
        };
        let reader =
            DirStore::new(directory.path().join("local")).with_read_only_root(root.clone());
        let operation = reader.for_operation().unwrap();
        operation
            .installed_package(&package, identity, &mut |_| true)
            .unwrap();
        for reader in [&reader as &dyn ResultStore, &*operation] {
            assert_eq!(&*reader.get(&first).unwrap(), b"first record");
            assert_eq!(
                reader.lookup_product(product).unwrap().reference,
                installed.reference
            );
        }
        let opened = InstalledPackage::open(&store, &package, identity).unwrap();
        assert_eq!(opened.records.lookup(Address(product_row)), Some(installed));
        InstalledPackage::verify_store(&root).unwrap();
        // The registration binds the installed header rather than mutable
        // row associations. Rewriting an index row keeps that header;
        // publishing another header makes the registration stale.
        assert!(registered().is_some());
        let other = crate::tests::install(&root, &[first.0], &[]);
        assert_ne!(other, header);
        assert!(registered().is_none());
    }

    /// A request selects its compiler registration at entry,
    /// against the registration current then, and serves the header that
    /// registration bound. Bugs: the compiler session and each operation both
    /// read it (two reads per request); an operation keeps the session's pin
    /// after a reinstall replaced it; an operation pairs its registration with
    /// a header reselected after a reinstall. Oracle: the rows each
    /// generation's header lists, and the reads an entry must make, by hand.
    #[test]
    fn each_operation_selects_the_registration_current_at_its_entry_once() {
        use crate::ResultStore;
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("installed");
        let seed = DirStore::new(root.clone());
        let key = |name: &str| {
            EntryKey::new(
                Default::default(),
                &hirsel_syntax::DefPath::core(name),
                Default::default(),
                hirsel_query_products::QueryKind::Signature,
                hirsel_query_products::LoweringPolicy::Reachable,
            )
        };
        let (first, second) = (key("generation-first"), key("generation-second"));
        seed.put(&first, b"first record");
        seed.put(&second, b"second record");
        let state = directory.path().join("state");
        let identity = hirsel_syntax::ContentHash::of_bytes(b"compiler");
        let register = |rows: &[hirsel_syntax::ContentHash]| {
            let header = crate::tests::install(&root, rows, &[]);
            hirsel_artifact_store::registration::Installation::begin(&root, &state, identity)
                .unwrap()
                .finish(header)
                .unwrap();
        };
        register(&[first.0]);

        let session = DirStore::new(directory.path().join("local"))
            .with_read_only_root(root.clone())
            .with_registration(identity, state.clone());
        let operation = session.for_operation().unwrap();
        operation.program_authority().unwrap();

        // A reinstall after the operation's entry: it keeps its generation.
        register(&[first.0, second.0]);
        assert_eq!(&*operation.get(&first).unwrap(), b"first record");
        assert!(operation.get(&second).is_none());

        let operation = session.for_operation().unwrap();
        assert_eq!(&*operation.get(&second).unwrap(), b"second record");
    }

    #[test]
    fn installed_only_verifier_is_host_local_and_never_writes_query_results() {
        let directory = tempfile::tempdir().unwrap();
        let installed = directory.path().join("installed");
        let cache = directory.path().join("cache");
        fs::create_dir_all(&installed).unwrap();
        fs::create_dir_all(&cache).unwrap();
        let root = cache.join("hirsel/validator-v2");
        let verifier = DirStore::local_verifier(&cache, &installed).unwrap();
        let reader = DirStore {
            root: installed.clone(),
            physical: None,
            read_only: vec![hirsel_artifact_store::Store::new(installed.clone())],
            cap: 0,
            verifier: std::sync::Arc::new(verifier),
            registration: None,
            operation: false,
            installed_capture: None,
            headers: Arc::default(),
            installation: None,
            root_ready: Arc::default(),
        };
        // Initialize the installed store's ordinary read lock before observing
        // query writes; lock creation is independent of query publication.
        assert!(
            reader
                .get(&EntryKey(
                    Default::default(),
                    Default::default(),
                    hirsel_query_products::QueryKind::Summary
                ))
                .is_none()
        );
        let installed_before: Vec<_> = fs::read_dir(&installed)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        let key = EntryKey::new(
            Default::default(),
            &hirsel_syntax::DefPath(hirsel_syntax::ItemPath {
                package: hirsel_syntax::PackageId {
                    name: "standalone".into(),
                    version: "0".into(),
                },
                modules: vec![],
                name: "host-local".into(),
            }),
            Default::default(),
            hirsel_query_products::QueryKind::Summary,
            hirsel_query_products::LoweringPolicy::Reachable,
        );
        reader.put(&key, b"must not persist");
        assert!(reader.get(&key).is_none());
        assert!(reader.local_product_store().is_none());
        assert!(
            reader
                .put_content(
                    Domain {
                        kind: 42,
                        schema: 1
                    },
                    b"no query content"
                )
                .is_none()
        );
        assert!(!root.exists(), "only V may publish in its namespace");
        let installed_after: Vec<_> = fs::read_dir(&installed)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(installed_after, installed_before);
        fs::create_dir_all(&root).unwrap();
        assert!(DirStore::local_verifier(&cache, &root).is_none());
        assert!(DirStore::local_verifier(&cache, &cache).is_none());
        fs::create_dir_all(root.join("program-inputs")).unwrap();
        assert!(DirStore::local_verifier(&cache, &root.join("program-inputs")).is_none());
        #[cfg(unix)]
        {
            let alias = directory.path().join("product-alias");
            std::os::unix::fs::symlink(&root, &alias).unwrap();
            assert!(DirStore::local_verifier(&cache, &alias).is_none());
        }
    }

    #[test]
    fn installed_read_scope_reuses_regions_refreshes_and_rejects_damage() {
        for forwarded in 0..3 {
            let directory = std::env::temp_dir().join(format!(
                "hirsel-installed-read-scope-{}-{:?}-{forwarded}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = fs::remove_dir_all(&directory);
            let installed = directory.join("installed");
            let local = directory.join("unused-local");
            let writer = hirsel_artifact_store::Store::new(installed.clone());
            let domain = Domain {
                kind: 0x4e0000fa,
                schema: 1,
            };
            let reference = writer.put(domain, b"installed bytes").unwrap();
            let reader = DirStore::new(local.clone()).with_read_only_root(installed);
            let statistics = reader.read_only[0].enable_statistics();
            let reader: std::sync::Arc<dyn ResultStore> = std::sync::Arc::new(reader);
            let budget = hirsel_engine_admission::ProcessBudget::new(4096);
            let reader: std::sync::Arc<dyn ResultStore> = if forwarded == 2 {
                std::sync::Arc::new(
                    crate::RequestStore::with_scratch(
                        reader,
                        budget.reserve(4096).unwrap(),
                        &directory,
                        4096,
                    )
                    .unwrap(),
                )
            } else if forwarded == 1 {
                std::sync::Arc::new(
                    crate::RequestStore::with_scratch(
                        reader,
                        budget.reserve(4096).unwrap(),
                        &std::env::temp_dir(),
                        1 << 30,
                    )
                    .unwrap(),
                )
            } else {
                reader
            };
            let scope = reader.read_scope();
            assert_eq!(&*reader.get_content(reference).unwrap(), b"installed bytes");
            let before = statistics.snapshot();
            for _ in 0..8 {
                assert_eq!(&*reader.get_content(reference).unwrap(), b"installed bytes");
            }
            let after = statistics.snapshot();
            for counter in ["IndexLoadAttempts", "IndexStamps", "SegmentOpens"] {
                assert_eq!(
                    after.counters[counter], before.counters[counter],
                    "{counter}"
                );
            }
            assert_eq!(after.counters["RegionCacheHits"], 8);
            let later = domain.reference(b"next operation").unwrap();
            assert!(reader.get_content(later).is_none());
            assert_eq!(writer.put(domain, b"next operation").unwrap(), later);
            assert!(
                reader.get_content(later).is_none(),
                "absence stays pinned within an operation"
            );
            drop(scope);
            let scope = reader.read_scope();
            assert_eq!(&*reader.get_content(later).unwrap(), b"next operation");
            drop(scope);
            fs::write(writer.output_path(reference), b"damaged installed pack").unwrap();
            let scope = reader.read_scope();
            assert!(
                reader.get_content(reference).is_none(),
                "new operation must reject damaged backing"
            );
            drop(scope);
            assert!(
                !local.exists(),
                "read scope creates no writable provider state"
            );
            drop(reader);
            assert_eq!(budget.reserved(), 0);
            fs::remove_dir_all(directory).unwrap();
        }
    }

    #[test]
    fn request_publication_scope_coalesces_cache_index_updates() {
        let directory = tempfile::tempdir().unwrap();
        let store = std::sync::Arc::new(DirStore::new(directory.path().join("products")));
        let statistics = store.physical.as_ref().unwrap().enable_statistics();
        let budget = hirsel_engine_admission::ProcessBudget::new(64 << 10);
        let request = crate::RequestStore::with_scratch(
            store.clone(),
            budget.reserve(64 << 10).unwrap(),
            directory.path(),
            64 << 10,
        )
        .unwrap();
        let scope = request.publication_scope().unwrap();
        let before = statistics.snapshot();
        let domain = Domain {
            kind: 0x4e0000fb,
            schema: 1,
        };
        let references: Vec<_> = (0u64..16)
            .map(|n| request.put_content(domain, &n.to_le_bytes()).unwrap())
            .collect();
        for reference in &references {
            assert!(request.get_content(*reference).is_some());
        }
        let pending = statistics.snapshot();
        assert_eq!(
            pending.counters["IndexPublicationAttempts"],
            before.counters["IndexPublicationAttempts"]
        );
        drop(scope);
        let committed = statistics.snapshot();
        assert!(
            committed.counters["IndexPublicationAttempts"]
                > pending.counters["IndexPublicationAttempts"]
        );
        let reopened = DirStore::new(directory.path().join("products"));
        for (n, reference) in references.iter().enumerate() {
            assert_eq!(
                &*reopened.get_content(*reference).unwrap(),
                &(n as u64).to_le_bytes()
            );
        }
    }
}
