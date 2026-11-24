use crate::package_content::{Id, PathTree, Root, parse_digest, symbol_prefix, target_name};
use crate::package_dir::{TestFile, module_path, package_name, path_rule};
use hirsel_core_ids::DefPath;
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
    major: u32,
    charge: Charge,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Charge {
    Policy(String),
    Inherit { workspace: bool },
}

#[derive(Default, Deserialize)]
struct Program {
    module: Option<String>,
    function: Option<String>,
}

#[derive(Deserialize)]
struct DependencyTable {
    major: Option<u32>,
    path: Option<String>,
    root: Option<String>,
    url: Option<String>,
    archive: Option<String>,
    workspace: Option<bool>,
}

#[derive(Deserialize)]
struct ForeignTable {
    symbols: String,
    archives: Vec<ArchiveTable>,
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
}

impl std::fmt::Display for SourceLocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Archive(path) => write!(f, "archive:{path}"),
            Self::Url(url) => write!(f, "url:{url}"),
        }
    }
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

pub(crate) struct Foreign {
    pub(crate) symbols: String,
    pub(crate) rows: BTreeMap<String, Archive>,
}

pub(crate) struct Package {
    pub(crate) id: Id,
    pub(crate) inherit_charge: bool,
    pub(crate) sources: BTreeMap<String, Vec<String>>,
    pub(crate) test_sources: BTreeMap<String, Vec<String>>,
    pub(crate) dependencies: BTreeMap<String, Dependency>,
    pub(crate) entry: DefPath,
    pub(crate) tests: Option<TestFile>,
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
                || document.program.module.is_some()
                || document.program.function.is_some()
            {
                return Err("package: package tables require [package]".into());
            }
            return Ok(Self {
                package: None,
                workspace,
            });
        };
        if !package_name(&package.name) {
            return Err(format!(
                "package.name: {:?} is not [a-z][a-z0-9_]* with at most 64 bytes",
                package.name
            ));
        }
        if matches!(package.name.as_str(), "core" | "std") {
            return Err(format!(
                "package.name: {:?} is reserved for the toolchain",
                package.name
            ));
        }
        let id = Id {
            name: package.name,
            major: package.major,
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
            let mut rows = BTreeMap::new();
            for row in &module.archives {
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
            foreign.insert(
                components,
                Foreign {
                    symbols: module.symbols.clone(),
                    rows,
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
                inherit_charge,
                sources,
                test_sources,
                dependencies,
                entry,
                tests,
                foreign,
            }),
            workspace,
        })
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
        .map(|(name, binding)| {
            let field = format!("{container}.{name}");
            if !package_name(&name) {
                return Err(format!(
                    "{field}: {name:?} is not [a-z][a-z0-9_]* with at most 64 bytes"
                ));
            }
            if name == "core" {
                return Err(format!(
                    "{field}: core@1 is bound in every package and is not declared"
                ));
            }
            let bad_form = || {
                format!("{field}: give exactly one of path, root, workspace or a toolchain major")
            };
            let edge = match (
                binding.major,
                binding.path,
                binding.root,
                binding.url,
                binding.archive,
                binding.workspace,
            ) {
                (None, None, None, None, None, Some(true)) if allow_inherited => {
                    Dependency::Inherited
                }
                (Some(major), None, None, None, None, None) => {
                    let supplied =
                        crate::package_content::toolchain_major(&name).ok_or_else(bad_form)?;
                    if major != supplied {
                        return Err(format!(
                            "{field}: the toolchain has {name}@{supplied}, not {name}@{major}"
                        ));
                    }
                    Dependency::Toolchain(Id {
                        name: name.clone(),
                        major,
                    })
                }
                (Some(major), Some(path), None, None, None, None) if name != "std" => {
                    Dependency::Member(
                        Id {
                            name: name.clone(),
                            major,
                        },
                        path,
                    )
                }
                (Some(major), None, Some(root), url, archive, None)
                    if name != "std" && (url.is_none() || archive.is_none()) =>
                {
                    let root = Root::parse(&root).ok_or_else(|| {
                        format!("{field}.root: expected sha256: and 64 lower-case hex digits")
                    })?;
                    let source = match (url, archive) {
                        (Some(url), _) if !url.starts_with("https://") => {
                            return Err(format!("{field}.url: expected an HTTPS URL"));
                        }
                        (Some(url), _) => Some(SourceLocation::Url(url)),
                        (None, Some(path)) => Some(SourceLocation::Archive(path)),
                        (None, None) => None,
                    };
                    Dependency::Published(
                        Id {
                            name: name.clone(),
                            major,
                        },
                        root,
                        source,
                    )
                }
                _ => return Err(bad_form()),
            };
            Ok((name, edge))
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
            (Workspace, "package") => WorkspacePackage,
            (Manifest, "sources" | "tool")
            | (Package, "name" | "major")
            | (Charge, "workspace")
            | (Dependency, "major" | "path" | "root" | "url" | "archive" | "workspace")
            | (Program, "module" | "function")
            | (Tests, "sources" | "limit" | "grants" | "dirs")
            | (Run, "limit" | "grants" | "dirs")
            | (Module, "symbols")
            | (Archive, "target" | "file" | "sha256")
            | (Workspace, "members")
            | (WorkspacePackage, "charge") => continue,
            (Tests | Run, "grant" | "dir") => {
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
