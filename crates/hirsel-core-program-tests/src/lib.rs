//! Whole-program laws consume the driver's checked products and reached units.
#[cfg(test)]
hirsel_guard::install!();

use hirsel_core_driver::{CheckProduct, CheckRequest, Driver, LibraryInput, Meter};
use hirsel_core_ids::DefPath;
use hirsel_core_resolve::{Corpus, CorpusManifest, ModuleSource, PackageManifest};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}
pub fn package(name: &str) -> hirsel_core_ids::PackageId {
    hirsel_core_driver::package(name)
}

fn std_manifest() -> PackageManifest {
    let mut manifest =
        PackageManifest::directory(&repo().join("core/std"), package("std"), false).unwrap();
    manifest.dependencies.push(package("core"));
    manifest
}

/// A fixture opts into std when its diagnostic exercises codec derivation.
pub(crate) fn source_manifest(
    source: &str,
    path: PathBuf,
    core: &Path,
    module: &str,
) -> CorpusManifest {
    let mut dependencies = vec![package("core")];
    let mut packages = vec![PackageManifest::directory(core, package("core"), true).unwrap()];
    if source.lines().any(|line| line == "// library: std") {
        dependencies.push(package("std"));
        packages.push(std_manifest());
    }
    packages.push(PackageManifest {
        package: package("app"),
        dependencies,
        sources: vec![ModuleSource {
            file: path,
            modules: vec![module.into()],
            signatures: false,
        }],
    });
    CorpusManifest {
        core: package("core"),
        packages,
    }
}

/// A source request and its immutable products, keeping their private store alive.
/// Native and evaluator laws use the same request and instance collection.
pub struct Checked {
    pub product: Arc<CheckProduct>,
    pub corpus: Corpus,
    _files: tempfile::TempDir,
}
impl Checked {
    pub fn source(source: &str, entry: Option<DefPath>) -> Self {
        Self::source_with_core(source, entry, &repo().join("core/core"))
    }
    pub fn source_with_core(source: &str, entry: Option<DefPath>, core: &Path) -> Self {
        let files = tempfile::tempdir().unwrap();
        let path = files.path().join("main.hirsel");
        std::fs::write(&path, source).unwrap();
        let inputs = source_manifest(source, path, core, "main");
        Self::check(inputs, files, entry)
    }
    pub fn corpus(corpus: &Corpus) -> Self {
        let files = tempfile::tempdir().unwrap();
        let mut inputs = corpus.manifest.clone();
        // A corpus control can replace bytes without changing its declaration's
        // module. Publish those exact bytes to this request's private source files.
        for (i, source) in corpus.sources.iter().enumerate() {
            let path = files.path().join(format!("source-{i}.hirsel"));
            std::fs::write(&path, &source.text).unwrap();
            let package = inputs
                .packages
                .iter_mut()
                .find(|p| p.package == source.module.package)
                .unwrap();
            let item = package
                .sources
                .iter_mut()
                .find(|s| s.modules == source.module.modules)
                .unwrap();
            item.file = path;
        }
        Self::check(inputs, files, None)
    }
    fn check(inputs: CorpusManifest, files: tempfile::TempDir, entry: Option<DefPath>) -> Self {
        let corpus = inputs.clone().load().unwrap();
        let product = Driver::default()
            .check(
                CheckRequest {
                    manifest: None,
                    inputs,
                    store: files.path().join("store"),
                    library: LibraryInput::Sources,
                    entry,
                },
                &mut Meter::new(false),
            )
            .unwrap();
        Self {
            product,
            corpus,
            _files: files,
        }
    }
}

#[cfg(test)]
mod cases;
#[cfg(test)]
mod spec_tests;
#[cfg(test)]
mod std_seed;

mod source;
pub use source::{SourceProgram, counted_loops};

#[cfg(test)]
pub(crate) fn checked_inputs(
    index: &hirsel_core_resolve::NameIndex,
    modules: &[hirsel_core_front::ModuleInput],
) -> Checked {
    use std::collections::BTreeMap;
    let files = tempfile::tempdir().unwrap();
    let core = package("core");
    let mut packages = BTreeMap::<_, PackageManifest>::new();
    for (i, module) in modules.iter().enumerate() {
        let path = index.module_path(module.id);
        let file = files.path().join(format!("source-{i:05}.hirsel"));
        std::fs::write(&file, module.syntax.text()).unwrap();
        let package = packages
            .entry(path.package.clone())
            .or_insert_with(|| PackageManifest {
                dependencies: if path.package == core {
                    vec![]
                } else {
                    vec![core.clone()]
                },
                package: path.package.clone(),
                sources: vec![],
            });
        package.sources.push(ModuleSource {
            file,
            modules: path.modules,
            signatures: path.package == core,
        });
    }
    if !packages.contains_key(&core) {
        packages.insert(
            core.clone(),
            PackageManifest::directory(&repo().join("core/core"), core.clone(), true).unwrap(),
        );
    }
    Checked::check(
        CorpusManifest {
            core,
            packages: packages.into_values().collect(),
        },
        files,
        None,
    )
}

#[cfg(test)]
mod lowering;

#[cfg(test)]
mod core_ops;

#[cfg(test)]
mod maps;

#[cfg(test)]
mod ordered;

#[cfg(test)]
mod slots;

/// The W3c socket law is shared by the evaluator and both native source suites.
pub mod wire_socket {
    include!("../tests/support/wire_socket.rs");
}
