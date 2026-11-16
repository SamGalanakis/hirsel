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

/// A source request and its immutable products, keeping their private store alive.
/// Native and evaluator laws use the same request and instance collection.
pub struct Checked {
    pub product: Arc<CheckProduct>,
    pub corpus: Corpus,
    _files: tempfile::TempDir,
}
impl Checked {
    pub fn source(source: &str, entry: Option<DefPath>) -> Self {
        let files = tempfile::tempdir().unwrap();
        let path = files.path().join("main.hirsel");
        std::fs::write(&path, source).unwrap();
        let inputs = CorpusManifest {
            core: package("core"),
            packages: vec![
                PackageManifest::directory(&repo().join("core/core"), package("core"), true)
                    .unwrap(),
                PackageManifest {
                    package: package("app"),
                    dependencies: vec![package("core")],
                    sources: vec![ModuleSource {
                        file: path,
                        modules: vec!["main".into()],
                        signatures: false,
                    }],
                },
            ],
        };
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
mod std_seed;

mod source;
pub use source::SourceProgram;
