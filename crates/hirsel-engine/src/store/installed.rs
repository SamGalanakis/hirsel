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
        assert!(InstalledPackage::open(directory.path(), &package, compiler).is_ok());
        assert!(
            InstalledPackage::open(directory.path(), &package, ContentHash::default())
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
        let error = InstalledPackage::open(directory.path(), &package, compiler).unwrap_err();
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
            InstalledPackage::open(directory.path(), &package, compiler)
                .unwrap_err()
                .contains("corrupt")
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
}

#[derive(Debug, Default)]
pub(super) struct InstallationRecords {
    pub rows: BTreeMap<ContentHash, (Action, Reference)>,
    pub contents: BTreeMap<ContentHash, Reference>,
}

impl InstalledPackage {
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
    fn check_records(&self, store: &hirsel_artifact_store::Store) -> Result<(), String> {
        for &(address, action, reference) in &self.rows {
            if !store
                .lookup(Address(address))
                .is_some_and(|row| row.action.0 == action && row.reference == reference)
                || store.get(reference).is_none()
            {
                return Err("partial or corrupt package product row".into());
            }
        }
        for &reference in &self.contents {
            if store.get(reference).is_none() {
                return Err("partial or corrupt package product content".into());
            }
        }
        Ok(())
    }
    pub(super) fn open(
        root: &Path,
        package: &PackageId,
        compiler: ContentHash,
    ) -> Result<Arc<Self>, String> {
        let read = || -> Result<Self, String> {
            let store = hirsel_artifact_store::Store::new(root.to_owned());
            let _read = store.batch();
            let row = store
                .lookup(address())
                .ok_or("missing installed package product")?;
            if row.reference.domain != PACKAGE {
                return Err("invalid installed package domain".into());
            }
            let bytes = store
                .get(row.reference)
                .ok_or("corrupt installed package product")?;
            let installed: Self = bincode::DefaultOptions::new()
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
            installed.check_records(&store)?;
            Ok(installed)
        };
        read()
            .map(Arc::new)
            .map_err(|error| format!("installed products {}: {error}", root.display()))
    }
}
