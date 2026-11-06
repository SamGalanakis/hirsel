//! An installed package binds its discovery inputs and complete product closure.
//! This is ordinary untrusted content. It never imports verifier authority.
use super::*;
mod manifest;
mod semantic;
use hirsel_syntax::{FileContext, PackageId};
pub use semantic::{InstalledDefinition, InstalledDefinitionKey, InstalledDefinitionOwnership};
pub use semantic::{
    InstalledSemanticInput, InstalledSpecializations, SEMANTIC_BODY, SEMANTIC_HEADER,
    SEMANTIC_INPUT, SEMANTIC_SOURCE,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const PACKAGE: Domain = Domain {
    kind: 0x4600_0039,
    schema: 3,
};
fn address() -> Address {
    Address(ContentHash::of_bytes(b"hirsel.installed-package.v3"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn fixture(
        root: &Path,
    ) -> (
        hirsel_artifact_store::Store,
        ContentHash,
        PackageId,
        Reference,
    ) {
        let store = hirsel_artifact_store::Store::new(root.to_owned());
        let compiler = ContentHash::of_bytes(b"compiler");
        let package = PackageId {
            name: "std".into(),
            version: "1".into(),
        };
        let reference = store
            .put(
                Domain {
                    kind: 77,
                    schema: 1,
                },
                b"product",
            )
            .unwrap();
        let row = Address(ContentHash::of_bytes(b"row"));
        let action = Action(ContentHash::of_bytes(b"action"));
        store.associate(row, action, reference).unwrap();
        let records = InstallationRecords {
            rows: [(row.0, (action, reference))].into(),
            contents: [(reference.output.0, reference)].into(),
        };
        let installed = InstalledPackage::new(
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
        );
        installed.publish(&store, &records).unwrap();
        (store, compiler, package, reference)
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn required_installed_content_survives_request_publication_pressure() {
        const CHILD: &str = "HIRSEL_INSTALLED_BACKING_PRESSURE_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new("/bin/sh")
                .args(["-c", "ulimit -n 128 && exec \"$@\"", "installed-backing-law"])
                .arg(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "installed::tests::required_installed_content_survives_request_publication_pressure",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let (store, compiler, package, reference) = fixture(&directory.path().join("installed"));
        let installed = InstalledPackage::open(&store, &package, compiler).unwrap();
        let sources = installed.package.source_records().unwrap();
        let budget = hirsel_engine_admission::ProcessBudget::new(1 << 20);
        let request = RequestStore::with_scratch(
            Arc::new(DirStore::new(directory.path().join("local"))),
            budget.reserve(1 << 20).unwrap(),
            directory.path(),
            8 << 20,
        )
        .unwrap();
        let operation = request.operation_scope();
        let domain = Domain {
            kind: 78,
            schema: 1,
        };
        let mut expected = Vec::new();
        for value in 0..160u32 {
            let mut bytes = vec![7; 32 << 10];
            bytes[..4].copy_from_slice(&value.to_le_bytes());
            let reference = request.put_content(domain, &bytes).unwrap();
            expected.push((reference, value));
        }
        // Every publication fits its existing RAM/disk allowance. Keeping those
        // required outputs cannot consume the descriptors needed to read an
        // installed record. The package's original payload is the oracle.
        assert_eq!(&*sources.content(reference, 4096).unwrap(), b"product");
        drop(operation);
        for (reference, value) in expected {
            let bytes = request.get_content(reference).unwrap();
            assert_eq!(&bytes[..4], &value.to_le_bytes());
            assert!(bytes[4..].iter().all(|byte| *byte == 7));
        }
        drop(request);
        assert_eq!(budget.reserved(), 0);
        drop(sources);
        drop(installed);
        // Actual missing backing remains an explicit failure at demand and
        // installation audit; descriptor pressure never makes it absent-OK.
        std::fs::remove_file(store.output_path(reference)).unwrap();
        let reopened = InstalledPackage::open(&store.for_operation(), &package, compiler).unwrap();
        assert!(
            reopened
                .package
                .source_records()
                .unwrap()
                .content(reference, 4096)
                .unwrap_err()
                .contains("missing installed semantic content")
        );
        assert!(InstalledPackage::verify_store(store.root()).is_err());
        let records = InstallationRecords {
            rows: reopened
                .package
                .rows
                .iter()
                .map(|(address, action, reference)| (*address, (Action(*action), *reference)))
                .collect(),
            contents: reopened
                .package
                .contents
                .iter()
                .map(|reference| (reference.output.0, *reference))
                .collect(),
        };
        assert!(
            reopened
                .package
                .as_ref()
                .clone()
                .publish(&store, &records)
                .is_err()
        );
    }
    #[test]
    fn install_and_audit_reject_stale_corrupt_and_partial_products() {
        let directory = tempfile::tempdir().unwrap();
        let (store, compiler, package, _reference) = fixture(directory.path());
        let installed = InstalledPackage::open(&store, &package, compiler)
            .unwrap()
            .package;
        // The unchanged serializer and generic decoder independently define
        // the table layout and accept/reject oracle, including corrupt counts.
        let encoded = bincode::serialize(&*installed).unwrap();
        let compare = |bytes: &[u8]| {
            use bincode::Options;
            let original = bincode::DefaultOptions::new()
                .with_fixint_encoding()
                .with_limit(MAX_RECORD as u64)
                .reject_trailing_bytes()
                .deserialize::<InstalledPackage>(bytes);
            let decoded = manifest::decode(bytes);
            assert_eq!(decoded.is_ok(), original.is_ok());
            if let (Ok(decoded), Ok(original)) = (decoded, original) {
                assert_eq!(
                    bincode::serialize(&decoded).unwrap(),
                    bincode::serialize(&original).unwrap()
                );
            }
        };
        compare(&encoded);
        for end in 0..encoded.len() {
            compare(&encoded[..end]);
        }
        let mut trailing = encoded.clone();
        trailing.push(0);
        compare(&trailing);
        for offset in 0..encoded.len() {
            let mut damaged = encoded.clone();
            damaged[offset] ^= 0x80;
            compare(&damaged);
        }
        drop(installed);
        assert!(
            InstalledPackage::open(&store, &package, ContentHash::default())
                .unwrap_err()
                .contains("compiler identity mismatch")
        );
        let row = store.lookup(address()).unwrap();
        std::fs::remove_file(
            store
                .association_path(Address(ContentHash::of_bytes(b"row")))
                .unwrap(),
        )
        .unwrap();
        let error = InstalledPackage::verify_store(directory.path()).unwrap_err();
        assert!(error.contains("partial"), "{error}");
        let installed = InstalledPackage::open(&store, &package, compiler).unwrap();
        assert!(
            installed
                .records
                .lookup(Address(ContentHash::of_bytes(b"row")))
                .is_none()
        );
        let original_root = store.lookup(address()).unwrap();
        assert!(
            installed
                .package
                .as_ref()
                .clone()
                .publish(
                    &store,
                    &InstallationRecords {
                        rows: installed
                            .package
                            .rows
                            .iter()
                            .map(|(address, action, reference)| (
                                *address,
                                (Action(*action), *reference)
                            ))
                            .collect(),
                        contents: installed
                            .package
                            .contents
                            .iter()
                            .map(|r| (r.output.0, *r))
                            .collect(),
                    }
                )
                .is_err()
        );
        assert_eq!(store.lookup(address()), Some(original_root));
        let path = store.output_path(row.reference);
        let mut damaged = std::fs::read(&path).unwrap();
        *damaged.last_mut().unwrap() ^= 1;
        std::fs::write(path, damaged).unwrap();
        assert!(
            InstalledPackage::open(&store, &package, compiler)
                .unwrap_err()
                .contains("corrupt")
        );
    }
    #[test]
    fn unused_corruption_is_deferred_until_demand_and_blocks_install_publication() {
        let directory = tempfile::tempdir().unwrap();
        let (store, compiler, package, reference) = fixture(directory.path());
        let path = store.output_path(reference);
        let mut damaged = std::fs::read(&path).unwrap();
        let payload = damaged
            .windows(b"product".len())
            .position(|bytes| bytes == b"product")
            .unwrap();
        damaged[payload] ^= 1;
        std::fs::write(&path, damaged).unwrap();
        let installed = InstalledPackage::open(&store, &package, compiler).unwrap();
        assert!(installed.records.content(reference).is_none());
        assert!(InstalledPackage::verify_store(directory.path()).is_err());
        let records = InstallationRecords {
            rows: installed
                .package
                .rows
                .iter()
                .map(|(address, action, reference)| (*address, (Action(*action), *reference)))
                .collect(),
            contents: installed
                .package
                .contents
                .iter()
                .map(|r| (r.output.0, *r))
                .collect(),
        };
        let old_root = store.lookup(address()).unwrap();
        assert!(
            installed
                .package
                .as_ref()
                .clone()
                .publish(&store, &records)
                .is_err()
        );
        assert_eq!(store.lookup(address()), Some(old_root));
    }
    #[test]
    fn demanded_expanded_content_refuses_before_allocating_and_remains_usable() {
        let directory = tempfile::tempdir().unwrap();
        let (store, compiler, package, _) = fixture(directory.path());
        let installed = InstalledPackage::open(&store, &package, compiler).unwrap();
        // Empty fields cost eight transport bytes each, but every decoded field
        // owns a Vec header. The independently granted transport-sized decode
        // reservation cannot hold those headers.
        let expected = vec![Vec::<u8>::new(); 8192];
        let encoded = bincode::serialize(&expected).unwrap();
        let reference = store
            .put(
                Domain {
                    kind: 79,
                    schema: 1,
                },
                &encoded,
            )
            .unwrap();
        let mut records = InstallationRecords {
            rows: installed
                .package
                .rows
                .iter()
                .map(|(address, action, reference)| (*address, (Action(*action), *reference)))
                .collect(),
            contents: installed
                .package
                .contents
                .iter()
                .map(|r| (r.output.0, *r))
                .collect(),
        };
        records.contents.insert(reference.output.0, reference);
        installed
            .package
            .as_ref()
            .clone()
            .publish(&store, &records)
            .unwrap();
        let installed = InstalledPackage::open(&store, &package, compiler).unwrap();
        let budget = hirsel_engine_admission::ProcessBudget::new(2 << 20);
        let _transport = budget.reserve(encoded.len() as u64).unwrap();
        let small = budget.reserve(encoded.len() as u64).unwrap();
        let bytes = installed.records.content(reference).unwrap();
        let (requirement, scanned) = test_alloc::measured(|| {
            hirsel_product_codec::DecodeRequirement::<hirsel_product_codec::RunContent>::open(bytes)
                .unwrap()
        });
        assert_eq!(scanned.allocations, 0);
        let (refused, allocations) = test_alloc::measured(|| requirement.decode_content(&small));
        assert!(
            refused.is_none(),
            "valid transport bytes require decoded-container admission"
        );
        assert_eq!(
            allocations.allocations, 0,
            "demand refuses before allocating semantic containers"
        );
        assert!(allocations.peak <= small.bytes() as isize);
        let large = budget.reserve(1 << 20).unwrap();
        let requirement =
            hirsel_product_codec::DecodeRequirement::<hirsel_product_codec::RunContent>::open(
                installed.records.content(reference).unwrap(),
            )
            .unwrap();
        let (decoded, allocations) =
            test_alloc::measured(|| requirement.decode_content(&large).unwrap());
        assert_eq!(decoded, expected);
        assert!(allocations.peak <= large.bytes() as isize);
    }
    #[test]
    fn operation_pins_the_manifest_and_rows_across_atomic_publication() {
        let directory = tempfile::tempdir().unwrap();
        let (writer, compiler, package, first) = fixture(directory.path());
        let reader = DirStore::new(directory.path().join("cache"))
            .with_read_only_root(directory.path().to_owned());
        let package_owner = reader.installed_package(&package, compiler).unwrap();
        let previous = OpenedInstallation {
            records: package_owner
                .records
                .upgrade()
                .expect("the provider pins direct library reads too"),
            package: package_owner,
        };
        let row = Address(ContentHash::of_bytes(b"row"));
        let second = writer.put(first.domain, b"replacement").unwrap();
        let action = Action(ContentHash::of_bytes(b"new action"));
        writer.associate(row, action, second).unwrap();
        let records = InstallationRecords {
            rows: [(row.0, (action, second))].into(),
            contents: [(second.output.0, second)].into(),
        };
        previous
            .package
            .as_ref()
            .clone()
            .publish(&writer, &records)
            .unwrap();
        let next = InstalledPackage::open(&writer.for_operation(), &package, compiler).unwrap();
        assert_ne!(previous.package.identity, next.package.identity);
        assert_eq!(previous.records.lookup(row).unwrap().reference, first);
        assert_eq!(&*previous.records.content(first).unwrap(), b"product");
        assert_eq!(
            next.records.lookup(row).unwrap(),
            Association {
                action,
                reference: second
            }
        );
        assert_eq!(&*next.records.content(second).unwrap(), b"replacement");
        // A separately damaged association cannot supply consumed metadata.
        let path = writer.association_path(row).unwrap();
        let mut damaged = std::fs::read(&path).unwrap();
        let offset = damaged
            .windows(action.0.0.len())
            .position(|bytes| bytes == action.0.0)
            .unwrap();
        damaged[offset] ^= 1;
        std::fs::write(path, damaged).unwrap();
        let corrupted =
            InstalledPackage::open(&writer.for_operation(), &package, compiler).unwrap();
        assert!(corrupted.records.lookup(row).is_none());
        assert!(InstalledPackage::verify_store(directory.path()).is_err());
    }
    #[test]
    fn installed_source_owner_keeps_published_references_without_query_cache_access() {
        let directory = tempfile::tempdir().unwrap();
        let (store, compiler, package, _) = fixture(directory.path());
        let key = EntryKey(
            ContentHash::of_bytes(b"source"),
            ContentHash::default(),
            hirsel_query_products::QueryKind::Parse,
        );
        let first = store
            .put(super::super::entry_domain(key.2), b"original source")
            .unwrap();
        store
            .associate(Address(key.0), Action(first.output.0), first)
            .unwrap();
        let mut installed = (*InstalledPackage::open(&store, &package, compiler)
            .unwrap()
            .package)
            .clone();
        let mut records = InstallationRecords::default();
        for (address, action, reference) in &installed.rows {
            records.rows.insert(*address, (Action(*action), *reference));
        }
        for reference in &installed.contents {
            records.contents.insert(reference.output.0, *reference);
        }
        records.rows.insert(key.0, (Action(first.output.0), first));
        records.contents.insert(first.output.0, first);
        installed.publish(&store, &records).unwrap();
        let owner = InstalledPackage::open(&store, &package, compiler)
            .unwrap()
            .package
            .source_records()
            .unwrap();
        let changed = store
            .put(super::super::entry_domain(key.2), b"replacement source")
            .unwrap();
        store
            .associate(Address(key.0), Action(changed.output.0), changed)
            .unwrap();
        assert_eq!(&*owner.get(&key).unwrap(), b"original source");
        assert!(
            owner
                .get(&EntryKey(
                    key.0,
                    key.1,
                    hirsel_query_products::QueryKind::Modes
                ))
                .is_none()
        );
        assert!(InstalledPackage::verify_store(directory.path()).is_err());
    }
    #[test]
    fn duplicate_content_checks_preserve_full_reference_identity() {
        let directory = tempfile::tempdir().unwrap();
        let (store, compiler, package, reference) = fixture(directory.path());
        let installed = InstalledPackage::open(&store, &package, compiler)
            .unwrap()
            .package;
        for invalid in [
            Reference {
                len: reference.len + 1,
                ..reference
            },
            Reference {
                domain: Domain {
                    schema: reference.domain.schema + 1,
                    ..reference.domain
                },
                ..reference
            },
        ] {
            let mut changed = (*installed).clone();
            changed.contents.push(invalid);
            assert!(
                changed
                    .check_records(&store)
                    .unwrap_err()
                    .contains("corrupt package product content")
            );
        }
    }
    #[test]
    fn operation_shares_installed_inputs_and_revalidates_at_the_next_boundary() {
        let directory = tempfile::tempdir().unwrap();
        let (store, compiler, package, reference) = fixture(directory.path());
        let provider = DirStore::new(directory.path().join("cache"))
            .with_read_only_root(directory.path().to_owned());
        let operation = provider.for_operation().unwrap();
        let first = operation.installed_package(&package, compiler).unwrap();
        let second = operation.installed_package(&package, compiler).unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        assert!(
            operation
                .installed_package(&package, ContentHash::default())
                .unwrap_err()
                .contains("compiler identity mismatch")
        );
        let held = operation.get_content(reference).unwrap();
        assert_eq!(&*held, b"product");
        let path = store.output_path(reference);
        let original = std::fs::read(&path).unwrap();
        let mut damaged = original.clone();
        *damaged.last_mut().unwrap() ^= 1;
        let replacement = directory.path().join("replacement");
        std::fs::write(&path, damaged).unwrap();
        assert_eq!(&*held, b"product");
        assert!(operation.get_content(reference).is_none());
        let next = provider.for_operation().unwrap();
        next.installed_package(&package, compiler).unwrap();
        assert!(next.get_content(reference).is_none());
        assert!(
            InstalledPackage::verify_store(directory.path())
                .unwrap_err()
                .contains("corrupt")
        );
        std::fs::write(&replacement, original).unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        std::fs::remove_file(
            store
                .association_path(Address(ContentHash::of_bytes(b"row")))
                .unwrap(),
        )
        .unwrap();
        let next = provider.for_operation().unwrap();
        next.installed_package(&package, compiler).unwrap();
        assert!(next.get_content(reference).is_some());
        assert!(
            InstalledPackage::verify_store(directory.path())
                .unwrap_err()
                .contains("partial")
        );
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InstalledSource {
    pub path: PathBuf,
    pub logical: PathBuf,
    pub context: FileContext,
    /// Text is the validation input of the Parse products and their anchors.
    pub text: String,
}

#[derive(Debug)]
/// One manifest generation and immutable, verified snapshots of demanded records.
pub(crate) struct InstalledRecords {
    pub(crate) root: PathBuf,
    store: hirsel_artifact_store::Store,
    // Keep the physical index generation pinned until this operation releases it.
    _scope: hirsel_artifact_store::Batch,
    rows: Box<[(ContentHash, Association, OnceLock<Option<Association>>)]>,
    contents: Box<[Reference]>,
}
impl InstalledRecords {
    pub(crate) fn lookup(&self, address: Address) -> Option<Association> {
        let index = self
            .rows
            .binary_search_by_key(&address.0, |(key, _, _)| *key)
            .ok()?;
        let (_, expected, checked) = &self.rows[index];
        *checked.get_or_init(|| {
            let row = self.store.lookup_owned(address)?;
            (row == *expected).then_some(row)
        })
    }
    pub(crate) fn content(&self, reference: Reference) -> Option<Bytes> {
        self.contents.binary_search(&reference).ok()?;
        self.store.get_owned(reference).ok()
    }
    pub fn metadata_bytes(&self) -> usize {
        let mut weight = hirsel_query_products::retained::MetadataWeight::default();
        weight.add(
            std::mem::size_of::<Self>()
                + 2 * std::mem::size_of::<usize>()
                + self.root.as_os_str().len()
                + 2 * self.store.root().as_os_str().len(),
        );
        weight.add(std::mem::size_of_val(&*self.rows));
        weight.add(std::mem::size_of_val(&*self.contents));
        weight.bytes().try_into().unwrap_or(usize::MAX)
    }
}
/// Immutable parse references survive their operation without retaining its
/// complete integrity closure. Reads after release validate the pinned reference.
#[derive(Debug)]
pub struct InstalledSourceRecords {
    pub installation: ContentHash,
    store: hirsel_artifact_store::Store,
    rows: BTreeMap<ContentHash, Reference>,
    operation: std::sync::Weak<InstalledRecords>,
    definitions: BTreeMap<DefPath, InstalledDefinition>,
}
impl InstalledSourceRecords {
    pub fn get(&self, key: &EntryKey) -> Option<Bytes> {
        if key.2 != hirsel_query_products::QueryKind::Parse {
            return None;
        }
        let reference = *self.rows.get(&key.0)?;
        match self.operation.upgrade() {
            Some(operation) => operation.content(reference),
            None => self.store.get_owned(reference).ok(),
        }
    }
    pub fn metadata_bytes(&self) -> usize {
        let mut weight = hirsel_query_products::retained::MetadataWeight::default();
        weight.add(
            std::mem::size_of::<Self>()
                + std::mem::size_of::<InstalledRecords>()
                + 4 * std::mem::size_of::<usize>()
                + self.store.root().as_os_str().len(),
        );
        weight.tree::<ContentHash, Reference>(self.rows.len());
        weight.tree::<DefPath, InstalledDefinition>(self.definitions.len());
        for (path, definition) in &self.definitions {
            weight.path(path);
            weight.path(&definition.key.definition);
            weight.vec(&definition.key.specialization, |weight, (name, path)| {
                weight.add(name.capacity());
                weight.path(path);
            });
            weight.add(definition.ownership.source_logical.as_os_str().len());
        }
        weight.bytes().try_into().unwrap_or(usize::MAX)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InstalledPackage {
    pub compiler: ContentHash,
    /// Identity of the verified installation manifest, including all actions and references.
    #[serde(skip)]
    pub identity: ContentHash,
    pub package: PackageId,
    pub manifest: Vec<u8>,
    pub sources: Vec<InstalledSource>,
    pub dependencies: Vec<PackageId>,
    definitions: BTreeMap<DefPath, InstalledDefinition>,
    /// Physical action rows and content required by the completed Precompute.
    rows: Vec<(ContentHash, ContentHash, Reference)>,
    contents: Vec<Reference>,
    #[serde(skip)]
    records: std::sync::Weak<InstalledRecords>,
    #[serde(skip)]
    sources_owner: Option<Arc<InstalledSourceRecords>>,
}

#[derive(Debug, Clone)]
pub(crate) struct OpenedInstallation {
    pub(crate) package: Arc<InstalledPackage>,
    pub(crate) records: Arc<InstalledRecords>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct InstallationRecords {
    pub rows: BTreeMap<ContentHash, (Action, Reference)>,
    pub contents: BTreeMap<ContentHash, Reference>,
}

impl InstalledPackage {
    pub fn source_records(&self) -> Option<Arc<InstalledSourceRecords>> {
        self.sources_owner.clone()
    }
    pub fn operation_metadata_bytes(&self) -> u64 {
        let mut weight = hirsel_query_products::retained::MetadataWeight::default();
        weight.add(std::mem::size_of::<Self>() + 2 * std::mem::size_of::<usize>());
        weight.add(self.package.name.capacity() + self.package.version.capacity());
        weight.add(self.manifest.capacity());
        weight.add(
            self.rows.capacity() * std::mem::size_of::<(ContentHash, ContentHash, Reference)>(),
        );
        weight.add(self.contents.capacity() * std::mem::size_of::<Reference>());
        weight.tree::<DefPath, InstalledDefinition>(self.definitions.len());
        for (path, definition) in &self.definitions {
            weight.path(path);
            weight.path(&definition.key.definition);
            weight.vec(&definition.key.specialization, |weight, (name, path)| {
                weight.add(name.capacity());
                weight.path(path);
            });
            weight.add(definition.ownership.source_logical.as_os_str().len());
        }
        weight.add(self.sources.capacity() * std::mem::size_of::<InstalledSource>());
        for source in &self.sources {
            weight.add(source.path.as_os_str().len() + source.logical.as_os_str().len());
            weight.add(source.text.capacity());
            weight.add(
                source.context.package.name.capacity() + source.context.package.version.capacity(),
            );
            weight.add(source.context.modules.capacity() * std::mem::size_of::<String>());
            for module in &source.context.modules {
                weight.add(module.capacity());
            }
        }
        if let Some(records) = self.records.upgrade() {
            weight.add(records.metadata_bytes());
        }
        if let Some(sources) = &self.sources_owner {
            weight.add(sources.metadata_bytes());
        }
        weight.bytes()
    }
    pub fn new(
        compiler: ContentHash,
        package: PackageId,
        manifest: Vec<u8>,
        sources: Vec<InstalledSource>,
        dependencies: Vec<PackageId>,
    ) -> Self {
        Self {
            compiler,
            identity: ContentHash::default(),
            package,
            manifest,
            sources,
            dependencies,
            definitions: BTreeMap::new(),
            rows: vec![],
            contents: vec![],
            records: Default::default(),
            sources_owner: None,
        }
    }
    pub(crate) fn publish(
        mut self,
        store: &hirsel_artifact_store::Store,
        records: &InstallationRecords,
    ) -> std::io::Result<()> {
        let mut records = records.clone();
        self.index_definitions(store, &mut records)?;
        self.rows = records
            .rows
            .iter()
            .map(|(address, (action, reference))| (*address, action.0, *reference))
            .collect();
        self.contents = records.contents.values().copied().collect();
        if self.rows.is_empty() || self.contents.is_empty() {
            return Err(std::io::Error::other(
                "package precomputation published no products",
            ));
        }
        // Publish last, after fresh reads prove the complete installation exists.
        self.check_definitions().map_err(std::io::Error::other)?;
        self.check_records(store).map_err(std::io::Error::other)?;
        let bytes = bincode::serialize(&self).map_err(std::io::Error::other)?;
        let reference = store.put(PACKAGE, &bytes)?;
        store.associate(address(), Action(reference.output.0), reference)
    }
    fn check_records(
        &self,
        store: &hirsel_artifact_store::Store,
    ) -> Result<hirsel_artifact_store::VerifiedRecords, String> {
        store
            .verify_records(
                self.rows.iter().map(|&(address, action, reference)| {
                    (
                        Address(address),
                        hirsel_artifact_store::Association {
                            action: Action(action),
                            reference,
                        },
                    )
                }),
                self.contents.iter().copied(),
            )
            .map_err(|error| format!("partial or corrupt package product content or row: {error}"))
    }
    /// Audit the complete published installation, including unused products.
    pub fn verify_store(root: &Path) -> Result<(), String> {
        let store = hirsel_artifact_store::Store::new(root.to_owned());
        Self::audit(&store).map(|_| ())
    }
    pub(crate) fn audit(
        store: &hirsel_artifact_store::Store,
    ) -> Result<hirsel_artifact_store::VerifiedRecords, String> {
        let _scope = store.batch();
        let installed = Self::read(store)?;
        installed.check_records(store)
    }
    fn read(store: &hirsel_artifact_store::Store) -> Result<Self, String> {
        let row = store
            .lookup_owned(address())
            .ok_or("missing installed package product")?;
        if row.reference.domain != PACKAGE || row.action != Action(row.reference.output.0) {
            return Err("invalid installed package domain or action".into());
        }
        let bytes = store
            .get_owned(row.reference)
            .ok()
            .ok_or("corrupt installed package product")?;
        let mut installed = manifest::decode(&bytes)
            .map_err(|error| format!("corrupt installed package: {error}"))?;
        installed.identity = row.reference.output.0;
        if installed.sources.is_empty()
            || installed.rows.is_empty()
            || installed.contents.is_empty()
            || installed
                .sources
                .iter()
                .any(|source| source.context.package != installed.package)
        {
            return Err("incomplete installed package interface".into());
        }
        // Std currently has no package dependencies. Refuse a new interface
        // until its dependency closure can be attached explicitly.
        if !installed.dependencies.is_empty() {
            return Err("installed package dependency products are unavailable".into());
        }
        installed.check_definitions()?;
        Ok(installed)
    }
    pub(crate) fn open(
        store: &hirsel_artifact_store::Store,
        package: &PackageId,
        compiler: ContentHash,
    ) -> Result<OpenedInstallation, String> {
        hirsel_intern::timings::count(hirsel_intern::timings::Count::InstalledPackageOpens);
        let read = || -> Result<OpenedInstallation, String> {
            let scope = store.batch();
            let mut installed = Self::read(store)?;
            if installed.compiler != compiler {
                return Err("stale installed package: compiler identity mismatch".into());
            }
            if installed.package != *package {
                return Err("installed package identity mismatch".into());
            }
            let mut rows = BTreeMap::new();
            for &(address, action, reference) in &installed.rows {
                let row = Association {
                    action: Action(action),
                    reference,
                };
                if rows
                    .insert(address, row)
                    .is_some_and(|previous| previous != row)
                {
                    return Err("conflicting installed product rows".into());
                }
            }
            let contents: std::collections::BTreeSet<_> = installed
                .contents
                .iter()
                .copied()
                .chain(rows.values().map(|row| row.reference))
                .collect();
            let records = Arc::new(InstalledRecords {
                root: store.root().to_owned(),
                store: store.clone(),
                _scope: scope,
                rows: rows
                    .into_iter()
                    .map(|(key, row)| (key, row, OnceLock::new()))
                    .collect(),
                contents: contents.into_iter().collect(),
            });
            installed.sources_owner = Some(Arc::new(InstalledSourceRecords {
                installation: installed.identity,
                store: store.clone(),
                rows: installed
                    .rows
                    .iter()
                    .filter(|(_, _, reference)| {
                        reference.domain
                            == super::entry_domain(hirsel_query_products::QueryKind::Parse)
                    })
                    .map(|(address, _, reference)| (*address, *reference))
                    .collect(),
                operation: Arc::downgrade(&records),
                definitions: installed.definitions.clone(),
            }));
            installed.records = Arc::downgrade(&records);
            Ok(OpenedInstallation {
                package: Arc::new(installed),
                records,
            })
        };
        read().map_err(|error| format!("installed products {}: {error}", store.root().display()))
    }
}
