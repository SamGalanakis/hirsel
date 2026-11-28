use crate::package_content::{Id, PathTree, Root, parse_digest, symbol_prefix, target_name};
use crate::package_dir::{module_path, path_rule};
use hirsel_config_schema::{Config, pkg_v1 as cfg};
pub(crate) mod name;
use hirsel_core_ids::DefPath;
use name::{self as names, Version};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum SourceLocation {
    Archive(String),
    Url(String),
    Registry(crate::registry_route::Origin),
    Git(GitSource),
}

/// A commit and the package's directory in it (GIT-1). `reference` is the full ref name
/// `hirsel add` resolved and `hirsel update` refreshes.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct GitSource {
    pub(crate) url: String,
    pub(crate) rev: String,
    pub(crate) dir: Option<String>,
    pub(crate) reference: Option<String>,
}

impl GitSource {
    /// The snapshot path prefix: the source spelling without `/<dir>` (GIT-1).
    pub(crate) fn commit(&self) -> String {
        format!("git:{}#{}", self.url, self.rev)
    }
}

impl std::fmt::Display for SourceLocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Archive(path) => write!(f, "archive:{path}"),
            Self::Url(url) => write!(f, "url:{url}"),
            Self::Registry(origin) => write!(f, "registry:{origin}"),
            Self::Git(git) => {
                write!(f, "{}", git.commit())?;
                match &git.dir {
                    Some(dir) => write!(f, "/{dir}"),
                    None => Ok(()),
                }
            }
        }
    }
}

/// GIT-1's `git` rule: an `https://` URL with a lower-case host, no user information, query
/// or fragment, kept as written.
pub(crate) fn git_url_rule(url: &str) -> Result<(), String> {
    let Some(rest) = url.strip_prefix("https://") else {
        return Err(format!("{url:?} is not an https:// URL"));
    };
    let authority = rest.split('/').next().unwrap_or("");
    if authority.contains('@') {
        return Err("has user information".into());
    }
    if url.contains('?') {
        return Err("has a query".into());
    }
    if url.contains('#') {
        return Err("has a fragment".into());
    }
    let host = authority
        .rsplit_once(':')
        .map_or(authority, |(host, _)| host);
    if host.is_empty() || rest.len() == authority.len() {
        return Err(format!("{url:?} is not an https:// URL"));
    }
    if host.bytes().any(|b| b.is_ascii_uppercase()) {
        return Err("has a host that is not lower case".into());
    }
    Ok(())
}

/// GIT-1's `rev` rule: a full commit id in lower-case hex.
pub(crate) fn git_rev_rule(rev: &str) -> bool {
    matches!(rev.len(), 40 | 64)
        && rev
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// GIT-1's `dir` rule: section 1's path components, without the `.hirsel` ending.
pub(crate) fn git_dir_rule(dir: &str) -> Result<(), String> {
    path_rule(dir, "")?;
    if dir.ends_with(".hirsel") {
        return Err("ends in .hirsel".into());
    }
    Ok(())
}

/// GIT-1's `ref` rule: `refs/heads/<name>` or `refs/tags/<name>`.
pub(crate) fn git_ref_rule(reference: &str) -> bool {
    ["refs/heads/", "refs/tags/"].iter().any(|prefix| {
        reference
            .strip_prefix(prefix)
            .is_some_and(|name| !name.is_empty())
    })
}

#[derive(Clone)]
pub(crate) enum Dependency {
    Toolchain(Id),
    Member(Id, String, u16),
    Published(Id, Root, Option<SourceLocation>, u16),
    Inherited,
}

pub(crate) struct Archive {
    pub(crate) file: String,
    pub(crate) pin: [u8; 32],
}

/// A foreign module's pinned code: archives per target, or one contained
/// WebAssembly module for every target (FFI-10).
pub(crate) enum Code {
    Native(BTreeMap<String, Archive>),
    Wasm(Archive),
}

pub(crate) struct Foreign {
    pub(crate) symbols: String,
    pub(crate) code: Code,
}

pub(crate) struct Package {
    pub(crate) id: Id,
    pub(crate) version: Version,
    pub(crate) inherit_charge: bool,
    pub(crate) sources: BTreeMap<String, Vec<String>>,
    pub(crate) test_sources: BTreeMap<String, Vec<String>>,
    pub(crate) dependencies: BTreeMap<String, Dependency>,
    pub(crate) entry: DefPath,
    pub(crate) tests: Option<cfg::Tests>,
    /// `[program]`'s run configuration, when it declares any of its keys.
    pub(crate) program: Option<cfg::Program>,
    pub(crate) foreign: BTreeMap<Vec<String>, Foreign>,
}

pub(crate) struct Workspace {
    pub(crate) members: Vec<String>,
    pub(crate) charge: Option<String>,
    pub(crate) dependencies: BTreeMap<String, Dependency>,
}

pub(crate) type Selections = BTreeMap<Id, (Root, Option<SourceLocation>)>;

pub(crate) struct Manifest {
    pub(crate) selections: Option<Selections>,
    pub(crate) package: Option<Package>,
    pub(crate) workspace: Option<Workspace>,
}

impl Manifest {
    pub(crate) fn parse(bytes: &[u8], path: &Path) -> Result<Self, String> {
        Self::classify(bytes, path).1
    }

    pub(crate) fn classify(bytes: &[u8], path: &Path) -> (bool, Result<Self, String>) {
        let mut workspace = false;
        let result = hirsel_config_schema::package_document(bytes, &mut workspace)
            .map_err(|mut error| {
                let mut message = error.to_string();
                for span in std::mem::take(&mut error.additional) {
                    error.span = span;
                    use std::fmt::Write;
                    write!(message, "\nhirsel: {}: {error}", path.display()).unwrap();
                }
                message
            })
            .and_then(|document| {
                if workspace {
                    let root =
                        cfg::Workspace::from_value(&document.value).expect("workspace schema");
                    if root
                        .root_package
                        .as_ref()
                        .is_some_and(|p| p.selections.is_some())
                    {
                        return Err(
                            "root_package.selections: declare selections on the workspace".into(),
                        );
                    }
                    let mut dirs = BTreeSet::new();
                    for member in &root.members {
                        path_rule(member, "").map_err(|e| {
                            format!("workspace.members: {member:?} breaks the path rules: {e}")
                        })?;
                        if !dirs.insert(member) {
                            return Err(format!("workspace.members: {member:?} is listed twice"));
                        }
                    }
                    let charge = match root.charge {
                        Some(cfg::Charge::WorkspaceCharge) => {
                            return Err("a workspace cannot inherit its own charge policy".into());
                        }
                        Some(cfg::Charge::ChargeV1) => Some("charge-v1".into()),
                        None => None,
                    };
                    Ok(Self {
                        workspace: Some(Workspace {
                            members: root.members,
                            charge,
                            dependencies: dependencies(
                                root.dependencies.unwrap_or_default(),
                                "workspace.dependencies",
                                false,
                            )?,
                        }),
                        selections: root.selections.map(selections).transpose()?,
                        package: root.root_package.map(Self::package).transpose()?,
                    })
                } else {
                    let mut pkg =
                        cfg::Package::from_value(&document.value).expect("package schema");
                    Ok(Self {
                        selections: pkg.selections.take().map(selections).transpose()?,
                        package: Some(Self::package(pkg)?),
                        workspace: None,
                    })
                }
            });
        (
            workspace,
            result.map_err(|e| format!("{}: {e}", path.display())),
        )
    }

    fn package(mut package: cfg::Package) -> Result<Package, String> {
        // A bare name is a toolchain package's, in the toolchain's own manifests (section 2).
        let toolchain = package.name != "std"
            && crate::package_content::toolchain_major(&package.name).is_some();
        if !(names::scoped(&package.name) || names::local(&package.name) || toolchain) {
            return Err(format!(
                "package.name: {:?} is not a scoped or local name",
                package.name
            ));
        }
        let version = Version::parse(&package.version).ok_or_else(|| {
            format!(
                "package.version: {:?} is not MAJOR.MINOR.PATCH[-PRE]",
                package.version
            )
        })?;
        let id = Id {
            name: package.name,
            major: version.major,
        };
        let inherit_charge = package.charge == cfg::Charge::WorkspaceCharge;
        let dependencies = dependencies(package.dependencies, "dependencies", true)?;
        let program = package.program;
        let module = program
            .as_ref()
            .and_then(|p| p.module.as_deref())
            .unwrap_or("main");
        let function = program
            .as_ref()
            .and_then(|p| p.function.as_deref())
            .unwrap_or("main");
        let modules = module_path(module)
            .ok_or_else(|| format!("program.module: {module:?} is not a module path"))?;
        if module_path(function).is_none_or(|parts| parts.len() != 1) {
            return Err(format!(
                "program.function: {function:?} is not an identifier"
            ));
        }
        let entry = DefPath::from_full(&format!("{id}/{}/{function}", modules.join("/"))).unwrap();
        if package.sources.is_empty() {
            return Err("sources: a package lists at least one source".into());
        }
        let sources = source_paths(package.sources, "sources")?;
        let mut tests = package.tests.take();
        let test_sources = tests
            .as_mut()
            .map(|t| source_paths(std::mem::take(&mut t.sources), "tests.sources"))
            .transpose()?
            .unwrap_or_default();
        let mut prefixes = BTreeSet::new();
        let mut archive_paths = BTreeSet::new();
        let mut foreign = BTreeMap::new();
        let mut listed = Vec::new();
        listed.extend(sources.keys().map(|p| (p.as_str(), "sources".to_owned())));
        listed.extend(
            test_sources
                .keys()
                .map(|p| (p.as_str(), "tests.sources".to_owned())),
        );
        let foreign_modules: BTreeMap<_, _> =
            package.foreign.unwrap_or_default().into_iter().collect();
        for (name, module) in &foreign_modules {
            let field = format!("foreign.{name}");
            let symbols = match module {
                cfg::ForeignModule::Native { symbols, .. }
                | cfg::ForeignModule::Contained { symbols, .. } => symbols,
            };
            let components = module_path(name)
                .ok_or_else(|| format!("{field}: {name:?} is not a module path"))?;
            let count = sources.values().filter(|path| **path == components).count();
            if count != 1 {
                return Err(format!("{field}: module {name} is formed by {count} files"));
            }
            if test_sources.values().any(|path| *path == components) {
                return Err(format!("{field}: a test-only file joins module {name}"));
            }
            if !symbol_prefix(symbols) || !prefixes.insert(symbols) {
                return Err(format!("{field}.symbols: bad symbol prefix {:?}", symbols));
            }
            let code = match module {
                cfg::ForeignModule::Contained { file, sha256, .. } => {
                    path_rule(file, ".wasm").map_err(|rule| {
                        format!("{field}: {:?} breaks the path rules: {rule}", file)
                    })?;
                    if !archive_paths.insert(file) {
                        return Err(format!("{field}: {:?} is named by two rows", file));
                    }
                    listed.push((file, field.clone()));
                    let pin = parse_digest(sha256).ok_or_else(|| {
                        format!("{field}.module: sha256 must be 64 lower-case hex digits")
                    })?;
                    Code::Wasm(Archive {
                        file: file.clone(),
                        pin,
                    })
                }
                cfg::ForeignModule::Native { archives, .. } => {
                    let mut rows = BTreeMap::new();
                    for row in archives {
                        if !target_name(&row.target) {
                            return Err(format!("{field}.archives: bad target {:?}", row.target));
                        }
                        path_rule(&row.file, ".a").map_err(|rule| {
                            format!("{field}: {:?} breaks the path rules: {rule}", row.file)
                        })?;
                        if !archive_paths.insert(&row.file) {
                            return Err(format!(
                                "{field}: {:?} is named by two archive rows",
                                row.file
                            ));
                        }
                        listed.push((&row.file, field.clone()));
                        let pin = parse_digest(&row.sha256).ok_or_else(|| {
                            format!("{field}.archives: sha256 must be 64 lower-case hex digits")
                        })?;
                        let archive = Archive {
                            file: row.file.clone(),
                            pin,
                        };
                        if rows.insert(row.target.clone(), archive).is_some() {
                            return Err(format!("{field}.archives: two rows for {}", row.target));
                        }
                    }
                    Code::Native(rows)
                }
            };
            foreign.insert(
                components,
                Foreign {
                    symbols: symbols.clone(),
                    code,
                },
            );
        }
        listed.sort_by_cached_key(|(path, _)| (path.to_ascii_lowercase(), *path));
        let mut tree = PathTree::default();
        for (path, field) in listed {
            tree.insert(path)
                .map_err(|error| format!("{field}: {error}"))?;
        }
        Ok(Package {
            id,
            version,
            inherit_charge,
            sources,
            test_sources,
            dependencies,
            entry,
            tests,
            program: program.filter(|p| p.limit.is_some() || p.grants.is_some()),
            foreign,
        })
    }
}

fn source_paths(
    sources: Vec<(String, String)>,
    field: &str,
) -> Result<BTreeMap<String, Vec<String>>, String> {
    sources
        .into_iter()
        .map(|(path, module)| {
            path_rule(&path, ".hirsel")
                .map_err(|rule| format!("{field}: {path:?} breaks the path rules: {rule}"))?;
            let parts = module_path(&module)
                .ok_or_else(|| format!("{field}: {module:?} is not a module path"))?;
            Ok((path, parts))
        })
        .collect()
}

fn selections(rows: Vec<(String, cfg::Selection)>) -> Result<Selections, String> {
    rows.into_iter()
        .map(|(key, selection)| {
            let id = key
                .split_once('@')
                .and_then(|(name, major)| {
                    let id = Id {
                        name: name.into(),
                        major: major.parse().ok()?,
                    };
                    (names::scoped(name) && id.to_string() == key).then_some(id)
                })
                .ok_or_else(|| format!("selections: {key:?} is not a scoped identity"))?;
            let (root, mut source) = published(&selection.root, selection.locator, "selections")?;
            if let Some(SourceLocation::Archive(path)) = &mut source {
                *path = crate::package_workspace::archive_path(Path::new(""), path).ok_or_else(
                    || format!("selections: archive {path:?} is not inside the workspace root"),
                )?;
            }
            Ok((id, (root, source)))
        })
        .collect()
}

fn published(
    pin: &str,
    locator: cfg::Locator,
    field: &str,
) -> Result<(Root, Option<SourceLocation>), String> {
    let root = Root::parse(pin)
        .ok_or_else(|| format!("{field}.root: expected sha256: and 64 lower-case hex digits"))?;
    let source = match locator {
        cfg::Locator::Store => None,
        cfg::Locator::Archive(path) => Some(SourceLocation::Archive(path)),
        cfg::Locator::Url(url) => {
            if !url.starts_with("https://") {
                return Err(format!("{field}.url: expected an HTTPS URL"));
            }
            Some(SourceLocation::Url(url))
        }
        cfg::Locator::Registry(url) => Some(SourceLocation::Registry(
            crate::registry_route::Origin::parse(&url)
                .map_err(|e| format!("{field}.registry: {e}"))?,
        )),
        cfg::Locator::Git {
            url,
            rev,
            reference,
            dir,
        } => {
            git_url_rule(&url).map_err(|e| format!("{field}.git: {e}"))?;
            if !git_rev_rule(&rev) {
                return Err(format!(
                    "{field}.rev: {rev:?} is not a full commit id of 40 or 64 lower-case hex digits"
                ));
            }
            if let Some(reference) = &reference
                && !git_ref_rule(reference)
            {
                return Err(format!(
                    "{field}.ref: {reference:?} is not refs/heads/<name> or refs/tags/<name>"
                ));
            }
            if let Some(dir) = &dir {
                git_dir_rule(dir)
                    .map_err(|e| format!("{field}.dir: {dir:?} breaks the path rules: {e}"))?;
            }
            Some(SourceLocation::Git(GitSource {
                url,
                rev,
                reference,
                dir,
            }))
        }
    };
    Ok((root, source))
}

fn ceiling(kinds: &[cfg::Capability], field: &str) -> Result<u16, String> {
    use cfg::Capability as C;
    use hirsel_core_ir::release_recipe::GrantKind as G;
    let mut mask = 0;
    for kind in kinds {
        let tag = match kind {
            C::Dir => G::Dir,
            C::Net => G::Net,
            C::Clock => G::Clock,
            C::Random => G::Random,
            C::Stdout => G::Stdout,
            C::Stderr => G::Stderr,
            C::Stdin => G::Stdin,
            C::Env => G::Env,
            C::Process => G::Process,
            C::Tasks => G::Tasks,
            C::Args => G::Args,
            C::Secret => G::Secret,
            C::Foreign => G::Foreign,
            C::Exec => G::Exec,
            C::Cred => G::Cred,
            C::Compiler => G::Compiler,
        };
        let bit = 1u16 << tag as u8;
        if mask & bit != 0 {
            return Err(format!("{field}.authority: names {kind:?} twice"));
        }
        mask |= bit;
    }
    Ok(mask)
}

fn dependencies(
    rows: Vec<(String, cfg::Dependency)>,
    container: &str,
    allow_inherited: bool,
) -> Result<BTreeMap<String, Dependency>, String> {
    rows.into_iter().map(|(alias, binding)| {
        let field = format!("{container}.{alias}");
        if alias == "core" { return Err(format!("{field}: core@1 is bound in every package and is not declared")); }
        if !names::alias(&alias) { return Err(format!("{field}: {alias:?} is not an alias: [a-z][a-z0-9_]* with at most 64 bytes")); }
        let dep = match binding {
            cfg::Dependency::Inherit if allow_inherited => Dependency::Inherited,
            cfg::Dependency::Inherit => return Err(format!("{field}: workspace dependencies cannot inherit")),
            cfg::Dependency::Toolchain {package, major, authority} => {
                if !authority.is_empty() { return Err(format!("{field}.authority: a toolchain binding takes no authority")); }
                let supplied = crate::package_content::toolchain_major(&alias).ok_or_else(|| format!("{field}: {alias:?} is not a toolchain package"))?;
                if package != alias { return Err(format!("{field}: toolchain package {package} is bound only as {package}")); }
                if major != supplied { return Err(format!("{field}: the toolchain has {alias}@{supplied}, not {alias}@{major}")); }
                Dependency::Toolchain(Id {name: package, major})
            }
            cfg::Dependency::Path {package, major, path, authority} => {
                if !names::any(&package) { return Err(format!("{field}.package: {package:?} is not a scoped or local name")); }
                if names::identifier(&package) && package != alias { return Err(format!("{field}: {package} is a toolchain package's name, bound only as {package}")); }
                Dependency::Member(Id {name:package, major}, path, ceiling(&authority, &field)?)
            }
            cfg::Dependency::Published {package, major, root, locator, authority} => {
                if !names::scoped(&package) { return Err(format!("{field}.package: {package:?} is not a scoped name")); }
                let (root,source) = published(&root, locator, &field)?;
                Dependency::Published(Id {name:package, major}, root, source, ceiling(&authority, &field)?)
            }
        };
        Ok((alias,dep))
    }).collect()
}
