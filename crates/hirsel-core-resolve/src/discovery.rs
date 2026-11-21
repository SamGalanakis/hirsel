use hirsel_core_ids::PackageId;
use hirsel_core_syntax::{Derive, ParseMode, ParseOutput, Symbols, parse_bytes};
use std::path::{Path, PathBuf};

/// Tool input, not language syntax: one explicitly mapped module per source (PROG-1).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ModuleSource {
    pub file: PathBuf,
    pub modules: Vec<String>,
    pub signatures: bool,
    #[serde(default)]
    pub foreign: bool,
}

impl ModuleSource {
    pub fn mode(&self) -> ParseMode {
        if self.foreign {
            ParseMode::Foreign
        } else if self.signatures {
            ParseMode::Signatures
        } else {
            ParseMode::Source
        }
    }
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PackageManifest {
    pub package: PackageId,
    pub sources: Vec<ModuleSource>,
    pub dependencies: Vec<PackageId>,
}
pub struct ParsedModule {
    pub module: ModulePath,
    pub file: PathBuf,
    pub parsed: ParseOutput,
}
use crate::{ModuleId, ModulePath, NameIndex};
/// How the codec derive (EXPANSION section 0) names std's codec module from module
/// `modules` of `package`, whose dependencies are `dependencies`: bare inside std's codec
/// module, `codec.` elsewhere in std, `std.codec.` where std is a dependency. None in a
/// package without std.
pub fn codec_derive(
    package: &PackageId,
    dependencies: &[PackageId],
    modules: &[String],
) -> Option<Derive> {
    let codec = if package.name == "std" {
        if modules == ["codec"] { "" } else { "codec." }
    } else if dependencies.iter().any(|d| d.name == "std") {
        "std.codec."
    } else {
        return None;
    };
    let mut path = package.name.clone();
    for m in modules {
        path.push('.');
        path.push_str(m);
    }
    Some(Derive {
        codec: codec.into(),
        path,
    })
}

/// The parse of one module's bytes, with the codec derive's items when `derive` is set.
pub fn parse_module(
    bytes: &[u8],
    mode: ParseMode,
    symbols: &mut Symbols,
    derive: Option<&Derive>,
) -> ParseOutput {
    let output = parse_bytes(bytes, mode, symbols);
    match derive {
        Some(d) if mode != ParseMode::Foreign => {
            hirsel_core_syntax::derive(output, mode, symbols, d)
        }
        _ => output,
    }
}

impl PackageManifest {
    /// Explicit directory-tool convention: relative path components name modules.
    /// Drivers with another manifest format supply their own `sources` instead.
    pub fn directory(root: &Path, package: PackageId, signatures: bool) -> std::io::Result<Self> {
        fn walk(
            root: &Path,
            dir: &Path,
            out: &mut Vec<ModuleSource>,
            signatures: bool,
            library: bool,
        ) -> std::io::Result<()> {
            let mut entries = std::fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
            entries.sort_by_key(|e| e.file_name());
            for e in entries {
                let path = e.path();
                let kind = e.file_type()?;
                if kind.is_dir() {
                    // Library contract fixtures belong to explicit test requests,
                    // never to a user's dependency, regardless of their headers.
                    if library && dir == root && e.file_name() == "tests" {
                        continue;
                    }
                    walk(root, &path, out, signatures, library)?;
                } else if path.extension().is_some_and(|x| x == "hirsel")
                    && (kind.is_file() || (kind.is_symlink() && path.metadata()?.is_file()))
                {
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
                        foreign: false,
                    });
                }
            }
            Ok(())
        }
        let mut sources = vec![];
        walk(
            root,
            root,
            &mut sources,
            signatures,
            matches!(package.name.as_str(), "core" | "std"),
        )?;
        Ok(Self {
            package,
            sources,
            dependencies: vec![],
        })
    }
    pub fn read(&self, symbols: &mut Symbols) -> std::io::Result<Vec<ParsedModule>> {
        self.intern(symbols);
        self.sources
            .iter()
            .map(|s| {
                let bytes = std::fs::read(&s.file)?;
                Ok(ParsedModule {
                    module: ModulePath {
                        package: self.package.clone(),
                        modules: s.modules.clone(),
                    },
                    file: s.file.clone(),
                    parsed: parse_module(
                        &bytes,
                        s.mode(),
                        symbols,
                        codec_derive(&self.package, &self.dependencies, &s.modules).as_ref(),
                    ),
                })
            })
            .collect()
    }
}

impl PackageManifest {
    pub fn intern(&self, symbols: &mut Symbols) {
        symbols.intern(&self.package.name);
        symbols.intern(&self.package.version);
        for d in &self.dependencies {
            symbols.intern(&d.name);
            symbols.intern(&d.version);
        }
        for s in &self.sources {
            for name in &s.modules {
                symbols.intern(name);
            }
        }
    }
}
impl NameIndex {
    /// External package discovery is converted once, before header indexing.
    pub fn add_manifests(&mut self, manifests: &[PackageManifest]) -> Vec<ModuleId> {
        for m in manifests {
            self.add_package(
                self.symbols.find(&m.package.name).unwrap(),
                self.symbols.find(&m.package.version).unwrap(),
            );
        }
        for m in manifests {
            let p = self
                .package(
                    self.symbols.find(&m.package.name).unwrap(),
                    self.symbols.find(&m.package.version).unwrap(),
                )
                .unwrap();
            let deps = m
                .dependencies
                .iter()
                .map(|d| {
                    self.package(
                        self.symbols.find(&d.name).unwrap(),
                        self.symbols.find(&d.version).unwrap(),
                    )
                    .unwrap()
                })
                .collect::<Vec<_>>();
            self.set_dependencies(p, deps);
        }
        let mut modules = vec![];
        let mut path = Vec::new();
        for m in manifests {
            let p = self
                .package(
                    self.symbols.find(&m.package.name).unwrap(),
                    self.symbols.find(&m.package.version).unwrap(),
                )
                .unwrap();
            for s in &m.sources {
                path.clear();
                path.extend(s.modules.iter().map(|n| self.symbols.find(n).unwrap()));
                let module = self.add_module(p, &path);
                if s.foreign {
                    self.modules[module.0 as usize].foreign = true;
                }
                modules.push(module);
            }
        }
        modules
    }
    pub fn module(&self, path: &ModulePath) -> Option<ModuleId> {
        let p = self.package(
            self.symbols.find(&path.package.name)?,
            self.symbols.find(&path.package.version)?,
        )?;
        let mut m = self.packages[p.0 as usize].root;
        for name in &path.modules {
            m = self.child(m, self.symbols.find(name)?)?;
        }
        Some(m)
    }
}
