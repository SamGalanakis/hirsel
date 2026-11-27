use crate::package_content::{Id, PathTree, Root, parse_digest, symbol_prefix, target_name};
use crate::package_dir::{ProgramRun, TestFile, module_path, path_rule};
pub(crate) mod name;
use hirsel_core_ids::DefPath;
use name::{self as names, Version};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

type Table = toml::Table;

#[derive(Deserialize)]
struct Document {
    package: Option<PackageTable>,
    #[serde(default)]
    sources: BTreeMap<String, String>,
    #[serde(default)]
    dependencies: BTreeMap<String, DependencyTable>,
    #[serde(default)]
    program: Program,
    tests: Option<TestFile>,
    #[serde(default)]
    foreign: BTreeMap<String, ForeignTable>,
    workspace: Option<WorkspaceTable>,
}

#[derive(Deserialize)]
struct PackageTable {
    name: String,
    version: String,
    charge: Charge,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Charge {
    Policy(String),
    Inherit { workspace: bool },
}

#[derive(Default, Deserialize, PartialEq)]
struct Program {
    module: Option<String>,
    function: Option<String>,
    limit: Option<u64>,
    grants: Option<Vec<String>>,
    dirs: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct DependencyTable {
    package: Option<String>,
    major: Option<u32>,
    path: Option<String>,
    root: Option<String>,
    url: Option<String>,
    archive: Option<String>,
    git: Option<String>,
    rev: Option<String>,
    #[serde(rename = "ref")]
    reference: Option<String>,
    dir: Option<String>,
    workspace: Option<bool>,
}

#[derive(Deserialize)]
struct ForeignTable {
    symbols: String,
    containment: Option<String>,
    module: Option<WasmTable>,
    archives: Option<Vec<ArchiveTable>>,
}

#[derive(Deserialize)]
struct WasmTable {
    file: String,
    sha256: String,
}

#[derive(Deserialize)]
struct ArchiveTable {
    target: String,
    file: String,
    sha256: String,
}

#[derive(Deserialize)]
struct WorkspaceTable {
    members: Vec<String>,
    package: Option<WorkspacePackage>,
    #[serde(default)]
    dependencies: BTreeMap<String, DependencyTable>,
}

#[derive(Deserialize)]
struct WorkspacePackage {
    charge: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SourceLocation {
    Archive(String),
    Url(String),
    Git(GitSource),
}

/// A commit and the package's directory in it (GIT-1). `reference` is the full ref name
/// `hirsel add` resolved; nothing but `add` reads it.
#[derive(Clone, Debug, Eq, PartialEq)]
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
    Member(Id, String),
    Published(Id, Root, Option<SourceLocation>),
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
    pub(crate) tests: Option<TestFile>,
    /// `[program]`'s run configuration, when it declares any of its keys.
    pub(crate) program: Option<ProgramRun>,
    pub(crate) foreign: BTreeMap<Vec<String>, Foreign>,
}

pub(crate) struct Workspace {
    pub(crate) members: Vec<String>,
    pub(crate) charge: Option<String>,
    pub(crate) dependencies: BTreeMap<String, Dependency>,
}

pub(crate) struct Manifest {
    pub(crate) package: Option<Package>,
    pub(crate) workspace: Option<Workspace>,
}

impl Manifest {
    pub(crate) fn parse(bytes: &[u8], path: &Path) -> Result<Self, String> {
        Self::classify(bytes, path).1
    }

    pub(crate) fn classify(bytes: &[u8], path: &Path) -> (bool, Result<Self, String>) {
        let refuse = |message| format!("{}: {message}", path.display());
        let parsed = || {
            let text = std::str::from_utf8(bytes).map_err(|error| refuse(error.to_string()))?;
            let table = text
                .parse::<Table>()
                .map_err(|error| refuse(error.to_string()))?;
            Ok((text, table))
        };
        let (text, table) = match parsed() {
            Ok(parsed) => parsed,
            Err(error) => return (false, Err(error)),
        };
        let workspace = table.contains_key("workspace");
        let result = Self::validate_table(text, &table, refuse);
        (workspace, result)
    }

    fn validate_table(
        text: &str,
        table: &Table,
        refuse: impl Fn(String) -> String,
    ) -> Result<Self, String> {
        let mut unknown = Vec::new();
        unknown_keys(table, "", Shape::Manifest, &mut unknown);
        if !unknown.is_empty() {
            unknown.sort_unstable();
            unknown.dedup();
            return Err(unknown
                .into_iter()
                .map(|key| refuse(format!("unknown key {key}")))
                .collect::<Vec<_>>()
                .join("\nhirsel: "));
        }
        if table.get("tool").is_some_and(|tool| !tool.is_table()) {
            return Err(refuse("tool: expected a table".into()));
        }
        let document: Document = toml::from_str(text).map_err(|error| refuse(error.to_string()))?;
        Self::validate(document).map_err(refuse)
    }

    fn validate(document: Document) -> Result<Self, String> {
        let workspace = document
            .workspace
            .map(|workspace| {
                let mut dirs = BTreeSet::new();
                for path in &workspace.members {
                    path_rule(path, "").map_err(|rule| {
                        format!("workspace.members: {path:?} breaks the path rules: {rule}")
                    })?;
                    if !dirs.insert(path) {
                        return Err(format!("workspace.members: {path:?} is listed twice"));
                    }
                }
                let charge = workspace.package.and_then(|p| p.charge);
                if let Some(policy) = &charge {
                    charge_policy(policy, "workspace.package.charge")?;
                }
                let dependencies =
                    dependencies(workspace.dependencies, "workspace.dependencies", false)?;
                Ok(Workspace {
                    members: workspace.members,
                    charge,
                    dependencies,
                })
            })
            .transpose()?;
        let Some(package) = document.package else {
            if workspace.is_none() {
                return Err("package: expected [package] or [workspace]".into());
            }
            if !document.sources.is_empty()
                || !document.dependencies.is_empty()
                || !document.foreign.is_empty()
                || document.tests.is_some()
                || document.program != Program::default()
            {
                return Err("package: package tables require [package]".into());
            }
            return Ok(Self {
                package: None,
                workspace,
            });
        };
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
        let inherit_charge = match package.charge {
            Charge::Inherit { workspace: true } => true,
            Charge::Policy(policy) => {
                charge_policy(&policy, "package.charge")?;
                false
            }
            Charge::Inherit { workspace: false } => {
                return Err("package.charge.workspace: expected true".into());
            }
        };
        let dependencies = dependencies(document.dependencies, "dependencies", true)?;
        let module = document.program.module.as_deref().unwrap_or("main");
        let function = document.program.function.as_deref().unwrap_or("main");
        let modules = module_path(module)
            .ok_or_else(|| format!("program.module: {module:?} is not a module path"))?;
        if module_path(function).is_none_or(|parts| parts.len() != 1) {
            return Err(format!(
                "program.function: {function:?} is not an identifier"
            ));
        }
        let entry = DefPath::from_full(&format!("{id}/{}/{function}", modules.join("/"))).unwrap();
        if document.sources.is_empty() {
            return Err("sources: a package lists at least one source".into());
        }
        let sources = source_paths(document.sources, "sources")?;
        let mut tests = document.tests;
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
        for (name, module) in &document.foreign {
            let field = format!("foreign.{name}");
            let contained = module.contained(&field)?;
            let components = module_path(name)
                .ok_or_else(|| format!("{field}: {name:?} is not a module path"))?;
            let count = sources.values().filter(|path| **path == components).count();
            if count != 1 {
                return Err(format!("{field}: module {name} is formed by {count} files"));
            }
            if test_sources.values().any(|path| *path == components) {
                return Err(format!("{field}: a test-only file joins module {name}"));
            }
            if !symbol_prefix(&module.symbols) || !prefixes.insert(&module.symbols) {
                return Err(format!(
                    "{field}.symbols: bad symbol prefix {:?}",
                    module.symbols
                ));
            }
            let code = if let Some(row) = contained {
                path_rule(&row.file, ".wasm").map_err(|rule| {
                    format!("{field}: {:?} breaks the path rules: {rule}", row.file)
                })?;
                if !archive_paths.insert(&row.file) {
                    return Err(format!("{field}: {:?} is named by two rows", row.file));
                }
                listed.push((&row.file, field.clone()));
                let pin = parse_digest(&row.sha256).ok_or_else(|| {
                    format!("{field}.module: sha256 must be 64 lower-case hex digits")
                })?;
                Code::Wasm(Archive {
                    file: row.file.clone(),
                    pin,
                })
            } else {
                let mut rows = BTreeMap::new();
                for row in module.archives.as_deref().unwrap_or_default() {
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
            };
            foreign.insert(
                components,
                Foreign {
                    symbols: module.symbols.clone(),
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
        Ok(Self {
            package: Some(Package {
                id,
                version,
                inherit_charge,
                sources,
                test_sources,
                dependencies,
                entry,
                tests,
                program: ProgramRun::declared(
                    document.program.limit,
                    document.program.grants,
                    document.program.dirs,
                ),
                foreign,
            }),
            workspace,
        })
    }
}

impl ForeignTable {
    /// FFI-10's containment key: the contained module's row, or `None` for a
    /// native module, whose archives the caller reads.
    fn contained(&self, field: &str) -> Result<Option<&WasmTable>, String> {
        match self.containment.as_deref() {
            None if self.module.is_some() => {
                Err(format!("{field}: module requires containment = \"wasm\""))
            }
            None if self.archives.is_none() => Err(format!("{field}: expected archives")),
            None => Ok(None),
            Some("wasm") if self.archives.is_some() => Err(format!(
                "{field}: archives is forbidden with containment = \"wasm\""
            )),
            Some("wasm") => self
                .module
                .as_ref()
                .map(Some)
                .ok_or_else(|| format!("{field}: containment = \"wasm\" requires module")),
            Some(_) => Err(format!("{field}: containment: expected \"wasm\"")),
        }
    }
}

fn charge_policy(value: &str, field: &str) -> Result<(), String> {
    if value != "charge-v1" {
        return Err(format!("{field}: unknown charge policy {value:?}"));
    }
    Ok(())
}

fn source_paths(
    sources: BTreeMap<String, String>,
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

fn dependencies(
    input: BTreeMap<String, DependencyTable>,
    container: &str,
    allow_inherited: bool,
) -> Result<BTreeMap<String, Dependency>, String> {
    input
        .into_iter()
        .map(|(alias, binding)| {
            let field = format!("{container}.{alias}");
            if alias == "core" {
                return Err(format!(
                    "{field}: core@1 is bound in every package and is not declared"
                ));
            }
            if !names::alias(&alias) {
                return Err(format!(
                    "{field}: {alias:?} is not an alias: [a-z][a-z0-9_]* with at most 64 bytes"
                ));
            }
            let bad_form = || {
                format!("{field}: give exactly one of path, root, workspace or a toolchain major")
            };
            let toolchain = crate::package_content::toolchain_major(&alias);
            // The member and published forms name their package; the toolchain form never does.
            let package = |binding: &DependencyTable| -> Result<String, String> {
                binding.package.clone().ok_or_else(|| match toolchain {
                    Some(_) => format!("{field}: a member or published binding gives package"),
                    None => format!(
                        "{field}: {alias:?} is not a toolchain package; give package = \"<scoped name>\""
                    ),
                })
            };
            let located = [
                &binding.url,
                &binding.archive,
                &binding.git,
                &binding.rev,
                &binding.reference,
                &binding.dir,
            ]
            .iter()
            .any(|field| field.is_some());
            let edge = match (
                binding.major,
                &binding.path,
                &binding.root,
                binding.workspace,
                located,
            ) {
                (None, None, None, Some(true), false)
                    if allow_inherited && binding.package.is_none() =>
                {
                    Dependency::Inherited
                }
                (Some(major), None, None, None, false) => {
                    let Some(supplied) = toolchain else {
                        package(&binding)?;
                        return Err(bad_form());
                    };
                    if binding.package.is_some() {
                        return Err(format!("{field}: a toolchain binding takes no package"));
                    }
                    if major != supplied {
                        return Err(format!(
                            "{field}: the toolchain has {alias}@{supplied}, not {alias}@{major}"
                        ));
                    }
                    Dependency::Toolchain(Id {
                        name: alias.clone(),
                        major,
                    })
                }
                (Some(major), Some(path), None, None, false) => {
                    let name = package(&binding)?;
                    let bare = names::identifier(&name);
                    if !(names::scoped(&name) || names::local(&name) || bare) {
                        return Err(format!(
                            "{field}.package: {name:?} is not a scoped or local name"
                        ));
                    }
                    // A toolchain package's alias is always its bare name (section 2).
                    if bare && name != alias {
                        return Err(format!(
                            "{field}: {name} is a toolchain package's name, bound only as {name}"
                        ));
                    }
                    Dependency::Member(Id { name, major }, path.clone())
                }
                (Some(major), None, Some(root), None, _) => {
                    let name = package(&binding)?;
                    if names::local(&name) {
                        return Err(format!(
                            "{field}: {name} is a local name, which only a workspace member has"
                        ));
                    }
                    if !names::scoped(&name) {
                        return Err(format!("{field}.package: {name:?} is not a scoped name"));
                    }
                    let root = Root::parse(root).ok_or_else(|| {
                        format!("{field}.root: expected sha256: and 64 lower-case hex digits")
                    })?;
                    let sources = [&binding.url, &binding.archive, &binding.git];
                    if sources.iter().filter(|source| source.is_some()).count() > 1 {
                        return Err(format!("{field}: give at most one of url, archive or git"));
                    }
                    if binding.git.is_none() {
                        for (key, value) in [
                            ("rev", &binding.rev),
                            ("ref", &binding.reference),
                            ("dir", &binding.dir),
                        ] {
                            if value.is_some() {
                                return Err(format!("{field}: {key} needs git"));
                            }
                        }
                    }
                    let source = match (binding.url, binding.archive, binding.git) {
                        (Some(url), _, _) if !url.starts_with("https://") => {
                            return Err(format!("{field}.url: expected an HTTPS URL"));
                        }
                        (Some(url), _, _) => Some(SourceLocation::Url(url)),
                        (None, Some(path), _) => Some(SourceLocation::Archive(path)),
                        (None, None, Some(url)) => {
                            git_url_rule(&url).map_err(|rule| format!("{field}.git: {rule}"))?;
                            let rev = binding
                                .rev
                                .ok_or_else(|| format!("{field}: git needs rev"))?;
                            if !git_rev_rule(&rev) {
                                return Err(format!("{field}.rev: {rev:?} is not a full commit id of 40 or 64 lower-case hex digits"));
                            }
                            if let Some(reference) = &binding.reference
                                && !git_ref_rule(reference)
                            {
                                return Err(format!("{field}.ref: {reference:?} is not refs/heads/<name> or refs/tags/<name>"));
                            }
                            if let Some(dir) = &binding.dir {
                                git_dir_rule(dir).map_err(|rule| {
                                    format!("{field}.dir: {dir:?} breaks the path rules: {rule}")
                                })?;
                            }
                            Some(SourceLocation::Git(GitSource {
                                url,
                                rev,
                                dir: binding.dir,
                                reference: binding.reference,
                            }))
                        }
                        (None, None, None) => None,
                    };
                    Dependency::Published(Id { name, major }, root, source)
                }
                _ => return Err(bad_form()),
            };
            Ok((alias, edge))
        })
        .collect()
}

#[derive(Clone, Copy)]
enum Shape {
    Manifest,
    Package,
    Charge,
    Dependencies,
    Dependency,
    Program,
    Tests,
    Runs,
    Run,
    Foreign,
    Module,
    Archive,
    Wasm,
    Workspace,
    WorkspacePackage,
}

fn unknown_keys(table: &Table, prefix: &str, shape: Shape, unknown: &mut Vec<String>) {
    use Shape::*;
    for (key, value) in table {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        let nested = match (shape, key.as_str()) {
            (Manifest, "package") => Package,
            (Manifest, "dependencies") | (Workspace, "dependencies") => Dependencies,
            (Manifest, "program") => Program,
            (Manifest, "tests") => Tests,
            (Manifest, "foreign") => Foreign,
            (Manifest, "workspace") => Workspace,
            (Package, "charge") => Charge,
            (Dependencies, _) => Dependency,
            (Tests, "runs") => Runs,
            (Runs, _) => Run,
            (Foreign, _) => Module,
            (Module, "archives") => Archive,
            (Module, "module") => Wasm,
            (Workspace, "package") => WorkspacePackage,
            (Manifest, "sources" | "tool")
            | (Package, "name" | "version")
            | (Charge, "workspace")
            | (
                Dependency,
                "package" | "major" | "path" | "root" | "url" | "archive" | "git" | "rev" | "ref"
                | "dir" | "workspace",
            )
            | (Program, "module" | "function" | "limit" | "grants" | "dirs")
            | (Tests, "sources" | "limit" | "grants" | "dirs")
            | (Run, "limit" | "grants" | "dirs")
            | (Module, "symbols" | "containment")
            | (Archive, "target" | "file" | "sha256")
            | (Wasm, "file" | "sha256")
            | (Workspace, "members")
            | (WorkspacePackage, "charge") => continue,
            (Tests | Run | Program, "grant" | "dir") => {
                unknown.push(format!("{path}. Use `{key}s`"));
                continue;
            }
            _ => {
                unknown.push(path);
                continue;
            }
        };
        if let Some(table) = value.as_table() {
            unknown_keys(table, &path, nested, unknown);
        } else if matches!(nested, Archive)
            && let Some(rows) = value.as_array()
        {
            for table in rows.iter().filter_map(toml::Value::as_table) {
                unknown_keys(table, &path, nested, unknown);
            }
        }
    }
}
