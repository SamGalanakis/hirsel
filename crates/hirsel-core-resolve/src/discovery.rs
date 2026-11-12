use hirsel_core_ids::PackageId;
use hirsel_core_syntax::{ParseMode, ParseOutput, parse};
use std::path::{Path, PathBuf};

/// Tool input, not language syntax: one explicitly mapped module per source (PROG-1).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ModuleSource {
    pub file: PathBuf,
    pub modules: Vec<String>,
    pub signatures: bool,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PackageManifest {
    pub package: PackageId,
    pub sources: Vec<ModuleSource>,
    pub dependencies: Vec<PackageId>,
}
pub struct ParsedModule {
    pub module: ModuleId,
    pub file: PathBuf,
    pub parsed: ParseOutput,
}
use crate::ModuleId;
impl PackageManifest {
    /// Explicit directory-tool convention: relative path components name modules.
    /// Drivers with another manifest format supply their own `sources` instead.
    pub fn directory(root: &Path, package: PackageId, signatures: bool) -> std::io::Result<Self> {
        fn walk(
            root: &Path,
            dir: &Path,
            out: &mut Vec<ModuleSource>,
            signatures: bool,
        ) -> std::io::Result<()> {
            let mut entries = std::fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
            entries.sort_by_key(|e| e.file_name());
            for e in entries {
                let path = e.path();
                let kind = e.file_type()?;
                if kind.is_dir() {
                    walk(root, &path, out, signatures)?;
                } else if kind.is_file() && path.extension().is_some_and(|x| x == "hirsel") {
                    let relative = path
                        .strip_prefix(root)
                        .expect("walk stays under root")
                        .with_extension("");
                    let modules = relative
                        .components()
                        .map(|c| {
                            c.as_os_str().to_str().map(str::to_owned).ok_or_else(|| {
                                std::io::Error::new(
                                    std::io::ErrorKind::InvalidData,
                                    "module path is not UTF-8",
                                )
                            })
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    out.push(ModuleSource {
                        file: path,
                        modules,
                        signatures,
                    });
                }
            }
            Ok(())
        }
        let mut sources = vec![];
        walk(root, root, &mut sources, signatures)?;
        Ok(Self {
            package,
            sources,
            dependencies: vec![],
        })
    }
    pub fn read(&self) -> std::io::Result<Vec<ParsedModule>> {
        self.sources
            .iter()
            .map(|s| {
                let text = std::fs::read_to_string(&s.file)?;
                Ok(ParsedModule {
                    module: ModuleId {
                        package: self.package.clone(),
                        modules: s.modules.clone(),
                    },
                    file: s.file.clone(),
                    parsed: parse(
                        &text,
                        if s.signatures {
                            ParseMode::Signatures
                        } else {
                            ParseMode::Source
                        },
                    ),
                })
            })
            .collect()
    }
}
