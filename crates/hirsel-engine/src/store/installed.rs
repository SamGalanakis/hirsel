//! An installed package binds its discovery inputs and complete product closure.
//! This is ordinary untrusted content. It never imports verifier authority.
use super::*;
use bincode::Options;
use hirsel_syntax::{FileContext, PackageId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const PACKAGE: Domain = Domain {
    kind: 0x4600_0039,
    schema: 1,
};
fn address() -> Address {
    Address(ContentHash::of_bytes(b"hirsel.installed-package.v1"))
}

#[cfg(test)]
mod tests {
    use super::*;

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
    #[test]
    fn installed_interface_rejects_stale_corrupt_and_partial_products() {
        let directory = tempfile::tempdir().unwrap();
        let (store, compiler, package, _reference) = fixture(directory.path());
        assert!(InstalledPackage::open(&store, &package, compiler).is_ok());
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
        let error = InstalledPackage::open(&store, &package, compiler).unwrap_err();
        assert!(error.contains("partial"), "{error}");
        assert!(
            error.contains(directory.path().to_str().unwrap()),
            "{error}"
        );
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
    fn installed_source_owner_keeps_published_references_without_query_cache_access() {
        let directory = tempfile::tempdir().unwrap();
        let (store, compiler, package, _) = fixture(directory.path());
        let key = EntryKey(
            ContentHash::of_bytes(b"source"),
            ContentHash::default(),
            crate::QueryKind::Parse,
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
                .get(&EntryKey(key.0, key.1, crate::QueryKind::Modes))
                .is_none()
        );
        assert!(InstalledPackage::open(&store, &package, compiler).is_err());
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
        let path = store.output_path(reference);
        let original = std::fs::read(&path).unwrap();
        let mut damaged = original.clone();
        *damaged.last_mut().unwrap() ^= 1;
        let replacement = directory.path().join("replacement");
        std::fs::write(&replacement, damaged).unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        assert_eq!(&*operation.get_content(reference).unwrap(), b"product");
        assert!(
            provider
                .for_operation()
                .unwrap()
                .installed_package(&package, compiler)
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
        assert!(
            next.installed_package(&package, compiler)
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
/// Complete integrity handles, retained only by the operation provider.
pub(super) struct InstalledRecords {
    pub(super) root: PathBuf,
    rows: Box<
        [(
            ContentHash,
            (Action, hirsel_artifact_store::VerifiedReference),
        )],
    >,
    contents: hirsel_artifact_store::VerifiedRecords,
    retained_bytes: usize,
}
impl InstalledRecords {
    pub(super) fn lookup(&self, address: Address) -> Option<hirsel_artifact_store::Association> {
        let index = self
            .rows
            .binary_search_by_key(&address.0, |(key, _)| *key)
            .ok()?;
        let (action, handle) = self.rows[index].1;
        Some(hirsel_artifact_store::Association {
            action,
            reference: self.contents.reference(handle)?,
        })
    }
    pub(super) fn content(&self, reference: Reference) -> Option<Bytes> {
        self.contents.get(reference)
    }
    pub(crate) fn metadata_bytes(&self) -> usize {
        self.retained_bytes
    }
    fn count_retained_bytes(&self) -> usize {
        let mut weight = crate::NativeMetadataWeight::default();
        weight.add(
            std::mem::size_of::<Self>()
                + 2 * std::mem::size_of::<usize>()
                + self.root.as_os_str().len(),
        );
        weight.add(std::mem::size_of_val(&*self.rows));
        let mut allocations = std::collections::BTreeSet::new();
        self.contents.visit_retained_allocations(|identity, bytes| {
            if allocations.insert(identity) {
                weight.add(bytes);
            }
        });
        weight.bytes().try_into().unwrap_or(usize::MAX)
    }
}
/// Immutable parse references survive their operation without retaining its
/// complete integrity closure. Reads after release validate the pinned reference.
#[derive(Debug)]
pub(crate) struct InstalledSourceRecords {
    store: hirsel_artifact_store::Store,
    rows: BTreeMap<ContentHash, Reference>,
    operation: std::sync::Weak<InstalledRecords>,
}
impl InstalledSourceRecords {
    pub(crate) fn get(&self, key: &EntryKey) -> Option<Bytes> {
        if key.2 != crate::QueryKind::Parse {
            return None;
        }
        let reference = *self.rows.get(&key.0)?;
        match self.operation.upgrade() {
            Some(operation) => operation.content(reference),
            None => self.store.get(reference),
        }
    }
    pub(crate) fn metadata_bytes(&self) -> usize {
        let mut weight = crate::NativeMetadataWeight::default();
        weight.add(
            std::mem::size_of::<Self>()
                + std::mem::size_of::<InstalledRecords>()
                + 4 * std::mem::size_of::<usize>()
                + self.store.root().as_os_str().len(),
        );
        weight.tree::<ContentHash, Reference>(self.rows.len());
        weight.bytes().try_into().unwrap_or(usize::MAX)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InstalledPackage {
    pub compiler: ContentHash,
    pub package: PackageId,
    pub manifest: Vec<u8>,
    pub sources: Vec<InstalledSource>,
    pub dependencies: Vec<PackageId>,
    /// Physical action rows and content required by the completed Precompute.
    rows: Vec<(ContentHash, ContentHash, Reference)>,
    contents: Vec<Reference>,
    #[serde(skip)]
    records: std::sync::Weak<InstalledRecords>,
    #[serde(skip)]
    sources_owner: Option<Arc<InstalledSourceRecords>>,
}

#[derive(Debug, Clone)]
pub(super) struct OpenedInstallation {
    pub(super) package: Arc<InstalledPackage>,
    pub(super) records: Arc<InstalledRecords>,
}

#[derive(Debug, Default)]
pub(super) struct InstallationRecords {
    pub rows: BTreeMap<ContentHash, (Action, Reference)>,
    pub contents: BTreeMap<ContentHash, Reference>,
}

impl InstalledPackage {
    pub(crate) fn source_records(&self) -> Option<Arc<InstalledSourceRecords>> {
        self.sources_owner.clone()
    }
    pub(crate) fn operation_metadata_bytes(&self) -> u64 {
        let mut weight = crate::NativeMetadataWeight::default();
        weight.add(std::mem::size_of::<Self>() + 2 * std::mem::size_of::<usize>());
        weight.add(self.package.name.capacity() + self.package.version.capacity());
        weight.add(self.manifest.capacity());
        weight.add(
            self.rows.capacity() * std::mem::size_of::<(ContentHash, ContentHash, Reference)>(),
        );
        weight.add(self.contents.capacity() * std::mem::size_of::<Reference>());
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
    pub(crate) fn new(
        compiler: ContentHash,
        package: PackageId,
        manifest: Vec<u8>,
        sources: Vec<InstalledSource>,
        dependencies: Vec<PackageId>,
    ) -> Self {
        Self {
            compiler,
            package,
            manifest,
            sources,
            dependencies,
            rows: vec![],
            contents: vec![],
            records: Default::default(),
            sources_owner: None,
        }
    }
    pub(super) fn publish(
        mut self,
        store: &hirsel_artifact_store::Store,
        records: &InstallationRecords,
    ) -> std::io::Result<()> {
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
    pub(super) fn open(
        store: &hirsel_artifact_store::Store,
        package: &PackageId,
        compiler: ContentHash,
    ) -> Result<OpenedInstallation, String> {
        hirsel_intern::timings::count(hirsel_intern::timings::Count::InstalledPackageOpens);
        let read = || -> Result<OpenedInstallation, String> {
            let _read = store.batch();
            let row = store
                .lookup(address())
                .ok_or("missing installed package product")?;
            if row.reference.domain != PACKAGE {
                return Err("invalid installed package domain".into());
            }
            let bytes = store
                .get(row.reference)
                .and_then(|bytes| Bytes::from(bytes.to_vec()).checked(row.reference))
                .ok_or("corrupt installed package product")?;
            let mut installed: Self = bincode::DefaultOptions::new()
                .with_fixint_encoding()
                .with_limit(MAX_RECORD as u64)
                .reject_trailing_bytes()
                .deserialize(&bytes)
                .map_err(|error| format!("corrupt installed package: {error}"))?;
            if installed.compiler != compiler {
                return Err("stale installed package: compiler identity mismatch".into());
            }
            if installed.package != *package {
                return Err("installed package identity mismatch".into());
            }
            if installed.sources.is_empty()
                || installed.rows.is_empty()
                || installed.contents.is_empty()
                || installed
                    .sources
                    .iter()
                    .any(|source| source.context.package != *package)
            {
                return Err("incomplete installed package interface".into());
            }
            // Std currently has no package dependencies. Refuse a new interface
            // until its dependency closure can be attached explicitly.
            if !installed.dependencies.is_empty() {
                return Err("installed package dependency products are unavailable".into());
            }
            let contents = installed.check_records(store)?;
            let rows: BTreeMap<_, _> = installed
                .rows
                .iter()
                .map(|&(address, action, reference)| {
                    Ok((
                        address,
                        (
                            Action(action),
                            contents.handle(reference).ok_or_else(|| {
                                "installation reference handle is unavailable".to_owned()
                            })?,
                        ),
                    ))
                })
                .collect::<Result<BTreeMap<_, _>, String>>()?;
            let mut records = InstalledRecords {
                root: store.root().to_owned(),
                rows: rows.into_iter().collect(),
                contents,
                retained_bytes: 0,
            };
            records.retained_bytes = records.count_retained_bytes();
            let records = Arc::new(records);
            installed.sources_owner = Some(Arc::new(InstalledSourceRecords {
                store: store.clone(),
                rows: installed
                    .rows
                    .iter()
                    .filter(|(_, _, reference)| {
                        reference.domain == super::entry_domain(crate::QueryKind::Parse)
                    })
                    .map(|(address, _, reference)| (*address, *reference))
                    .collect(),
                operation: Arc::downgrade(&records),
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
