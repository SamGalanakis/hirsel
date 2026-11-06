//! Content-addressed source observations with no filesystem access.
use super::*;
use hirsel_syntax::{ContentHash, FileContext};
use std::{path::Path, sync::Arc};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemoryInputs {
    path: PathBuf,
    manifest_path: PathBuf,
    source: Arc<[u8]>,
    manifest: Arc<[u8]>,
}
impl MemoryInputs {
    pub(super) fn replace_sources(
        &self,
        replacements: &std::collections::BTreeMap<PathBuf, Arc<[u8]>>,
        limit: usize,
    ) -> io::Result<Self> {
        if replacements.keys().any(|path| *path != self.path) {
            return Err(io::Error::other("replacement is not a memory source"));
        }
        let mut replaced = self.clone();
        if let Some(source) = replacements.get(&self.path) {
            replaced.source = source.clone();
        }
        if replaced.retained_bytes() > limit {
            return Err(io::Error::other(hirsel_engine::loader_inputs::InputLimit));
        }
        Ok(replaced)
    }
    pub fn new(source: &str, context: &FileContext) -> Self {
        let context_bytes = serde_json::to_vec(context).expect("file context serializes");
        let identity = ContentHash::of_parts(
            "hirsel.memory-source.v1",
            [context_bytes.as_slice(), source.as_bytes()],
        );
        let hex: String = identity
            .0
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let path = PathBuf::from(format!("/$hirsel-memory/{hex}/source.hir"));
        let manifest_path = path.with_extension("hir.json");
        let dependencies = if context.package.package_is("std", "1") {
            vec![]
        } else {
            vec![
                serde_json::json!({"manifest":"/$hirsel-installed/std@1/hirsel.json", "package":{"name":"std","version":"1"}, "expose":[]}),
            ]
        };
        let manifest = serde_json::to_vec(&serde_json::json!({
            "schema":1,"package":context.package,
            "sources":[{"path":"source.hir","module":context.modules}],
            "entry":{"module":context.modules,"name":"main"},
            "dependencies":dependencies
        }))
        .expect("memory manifest serializes")
        .into();
        Self {
            path,
            manifest_path,
            source: source.as_bytes().into(),
            manifest,
        }
    }
    pub(crate) fn execution_identity(&self) -> hirsel_syntax::ContentHash {
        hirsel_syntax::ContentHash::of_parts(
            "hirsel.execution.memory-inputs.v1",
            [self.source.as_ref(), self.manifest.as_ref()],
        )
    }
    /// A logical loader key, never an external file or an ambient read.
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.path.as_os_str().len()
            + self.manifest_path.as_os_str().len()
            + self.source.len()
            + self.manifest.len()
    }
    fn unavailable(&self, path: &Path) -> io::Error {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("no memory input {}", path.display()),
        )
    }
}
impl LoaderInputs for MemoryInputs {
    fn read(&self, path: &Path) -> io::Result<Arc<[u8]>> {
        if path == self.path {
            Ok(self.source.clone())
        } else if path == self.manifest_path {
            Ok(self.manifest.clone())
        } else {
            Err(self.unavailable(path))
        }
    }
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        self.metadata(path).map(|_| path.to_owned())
    }
    fn metadata(&self, path: &Path) -> io::Result<InputKind> {
        if path == self.path || path == self.manifest_path {
            Ok(InputKind::File)
        } else {
            Err(self.unavailable(path))
        }
    }
    fn directory(&self, path: &Path) -> io::Result<Vec<DirectoryEntry>> {
        Err(self.unavailable(path))
    }
}
impl CompileRequest {
    /// An immutable source request whose identity includes its context and bytes.
    pub fn source(source: &str, context: &FileContext, operation: CompileOperation) -> Self {
        let inputs = MemoryInputs::new(source, context);
        Self {
            path: inputs.path().to_owned(),
            inputs: CompileInputs::Memory(inputs),
            operation,
        }
    }
}
