use hirsel_core_ids::PackageId;
use hirsel_core_syntax::{Derive, ParseMode, ParseOutput, Symbols, parse_bytes};
use std::path::{Path, PathBuf};

/// Tool input, not language syntax: one explicitly mapped module per source (PROG-1).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ModuleSource {
    pub file: PathBuf,
    pub modules: Vec<String>,
    pub signatures: bool,
    /// A foreign interface module (FFI-1) and how its code runs.
    #[serde(default)]
    pub foreign: Option<Containment>,
    #[serde(skip)]
    pub captured: Option<CapturedSource>,
}

#[derive(Clone, Debug)]
pub struct CapturedSource {
    pub bytes: bytes::Bytes,
    pub name: Option<String>,
    pub test: bool,
}

impl ModuleSource {
    pub fn mode(&self) -> ParseMode {
        if self.foreign.is_some() {
            ParseMode::Foreign
        } else if self.signatures {
            ParseMode::Signatures
        } else {
            ParseMode::Source
        }
    }

    pub fn read(&self) -> std::io::Result<bytes::Bytes> {
        match &self.captured {
            Some(captured) => Ok(captured.bytes.clone()),
            None => std::fs::read(&self.file).map(bytes::Bytes::from),
        }
    }
    pub fn name(&self, manifest: Option<&Path>) -> std::io::Result<Option<String>> {
        if let Some(name) = self
            .captured
            .as_ref()
            .and_then(|capture| capture.name.as_ref())
        {
            return Ok(Some(name.clone()));
        }
        manifest
            .map(|root| crate::manifest_name(root, &self.file))
            .transpose()
    }
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PackageManifest {
    pub package: PackageId,
    pub sources: Vec<ModuleSource>,
    pub dependencies: Vec<PackageId>,
    #[serde(skip)]
    pub namespace: Option<PackageNamespace>,
    /// The installed toolchain supplied this package (its core-packages
    /// archive), or the invoker said so in an `--inputs` manifest. A
    /// `hirsel.toml` member or a published archive never is, whatever its name
    /// (launch.md, "Private prefix").
    #[serde(default)]
    pub toolchain: bool,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PackageNamespace {
    #[serde(default)]
    pub version: Option<String>,
    /// Workspace rows binding this package, with their diagnostic labels and kind masks.
    /// Consumer policy never enters package content, images or compiler keys.
    #[serde(skip)]
    pub authority: Vec<(String, u16)>,
    pub root: Option<hirsel_core_ids::ContentHash>,
    pub foreign: hirsel_core_ids::ContentHash,
    pub published_bindings: std::collections::BTreeMap<PackageId, hirsel_core_ids::ContentHash>,
    /// Each alias this package's source names a dependency by (packages.md
    /// section 2); two aliases may name one dependency. A dependency no alias
    /// names is named by the last `/` component of its package name.
    #[serde(default)]
    pub aliases: std::collections::BTreeMap<String, PackageId>,

    #[serde(skip)]
    pub foreign_inputs: std::collections::BTreeMap<Vec<String>, ForeignModuleCapture>,
}
#[derive(Clone, Debug)]
pub struct ForeignArchiveCapture {
    pub path: String,
    pub pin: [u8; 32],
    pub bytes: std::sync::Arc<[u8]>,
}

/// A foreign module's code: pinned archives per target (FFI-2, FFI-3), or one
/// pinned WebAssembly module that every target runs contained (FFI-10).
#[derive(Clone, Debug)]
pub enum ForeignCode {
    Native(std::collections::BTreeMap<String, ForeignArchiveCapture>),
    Wasm(ForeignArchiveCapture),
}

/// How a foreign module's code runs: linked into the program (FFI-2), or as a
/// contained WebAssembly instance (FFI-10).
#[derive(
    Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum Containment {
    Native,
    Wasm,
}

#[derive(Clone, Debug)]
pub struct ForeignModuleCapture {
    pub symbols: String,
    pub code: ForeignCode,
}

impl ForeignModuleCapture {
    pub fn containment(&self) -> Containment {
        match self.code {
            ForeignCode::Native(_) => Containment::Native,
            ForeignCode::Wasm(_) => Containment::Wasm,
        }
    }

    /// The archive rows of a native module; a contained module has none.
    pub fn archives(&self) -> Option<&std::collections::BTreeMap<String, ForeignArchiveCapture>> {
        match &self.code {
            ForeignCode::Native(archives) => Some(archives),
            ForeignCode::Wasm(_) => None,
        }
    }

    pub fn wasm(&self) -> Option<&ForeignArchiveCapture> {
        match &self.code {
            ForeignCode::Native(_) => None,
            ForeignCode::Wasm(module) => Some(module),
        }
    }
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
                        foreign: None,
                        captured: None,
                        file: path,
                        modules,
                        signatures,
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
            namespace: None,
            toolchain: false,
        })
    }
    pub fn read(&self, symbols: &mut Symbols) -> std::io::Result<Vec<ParsedModule>> {
        self.intern(symbols);
        self.sources
            .iter()
            .map(|s| {
                let bytes = s.read()?;
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

/// The name of a package's root module: the last `/` component of its name.
pub fn short_name(package: &str) -> &str {
    package.rsplit('/').next().unwrap_or(package)
}

impl PackageManifest {
    /// The names `dependency` has in this package's source: its aliases when the
    /// manifest gives any, else the last `/` component of its package name.
    pub fn aliases<'a>(&'a self, dependency: &'a PackageId) -> Vec<&'a str> {
        let aliases: Vec<&str> = self
            .namespace
            .iter()
            .flat_map(|namespace| &namespace.aliases)
            .filter(|(_, bound)| *bound == dependency)
            .map(|(alias, _)| alias.as_str())
            .collect();
        if aliases.is_empty() {
            vec![short_name(&dependency.name)]
        } else {
            aliases
        }
    }

    pub fn intern(&self, symbols: &mut Symbols) {
        symbols.intern(&self.package.name);
        symbols.intern(short_name(&self.package.name));
        symbols.intern(&self.package.version);
        for d in &self.dependencies {
            symbols.intern(&d.name);
            for alias in self.aliases(d) {
                symbols.intern(alias);
            }
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
        let packages: Vec<_> = manifests
            .iter()
            .map(|m| {
                self.add_package(
                    self.symbols.find(&m.package.name).unwrap(),
                    self.symbols.find(short_name(&m.package.name)).unwrap(),
                    self.symbols.find(&m.package.version).unwrap(),
                    m.toolchain,
                )
            })
            .collect();
        for (m, &p) in manifests.iter().zip(&packages) {
            let deps = m
                .dependencies
                .iter()
                .flat_map(|d| {
                    let key = self
                        .package(
                            self.symbols.find(&d.name).unwrap(),
                            self.symbols.find(&d.version).unwrap(),
                        )
                        .unwrap();
                    m.aliases(d).into_iter().map(move |alias| (alias, key))
                })
                .map(|(alias, key)| (self.symbols.find(alias).unwrap(), key))
                .collect::<Vec<_>>();
            self.set_dependencies(p, deps);
        }
        let source_count = manifests.iter().map(|m| m.sources.len()).sum();
        let mut modules = Vec::with_capacity(source_count);
        let mut path = Vec::new();
        for (m, p) in manifests.iter().zip(packages) {
            for s in &m.sources {
                path.clear();
                path.extend(s.modules.iter().map(|n| self.symbols.find(n).unwrap()));
                let module = self.add_module(p, &path);
                if let Some(containment) = s.foreign {
                    self.mark_foreign_module(module, containment);
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
