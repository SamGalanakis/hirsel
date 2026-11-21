//! A package directory and the manifest beside its sources (PROG-3, PROG-4):
//! the package, the entry that names `main`, and its tests' run
//! configuration. A directory without a manifest is the directory
//! convention: package `app`, one module per file, `main` in `main`.
use crate::NativeOptions;
use hirsel_core_ids::{DefPath, PackageId};
use hirsel_core_net::{EnvPatterns, GrantRule, RULE_SYNTAX, SecretGrant};
use hirsel_core_resolve::{ModuleSource, PackageManifest};
use std::collections::BTreeMap;
use std::error::Error;
use std::io;
use std::path::{Component, Path, PathBuf};

pub(crate) const MANIFEST: &str = "hirsel.toml";

pub(crate) fn scaffold_manifest(sources: impl Iterator<Item = &'static str>) -> String {
    use std::fmt::Write;
    let mut manifest = String::from(
        "[package]\nname = \"app\"\nmajor = 1\ncharge = \"charge-v1\"\n\n[dependencies]\nstd = { major = 1 }\n\n[sources]\n",
    );
    for file in sources {
        let module = file.strip_suffix(".hirsel").unwrap();
        writeln!(manifest, "{file:?} = {module:?}").unwrap();
    }
    manifest
        .push_str("\n[tests]\nlimit = 67108864\n\n[tests.sources]\n\"tests.hirsel\" = \"tests\"\n");
    manifest
}

/// One run's grants, as `--dir` and `--grant` give them or a package's
/// manifest declares them, in the same syntax.
#[derive(Clone, Default)]
pub(crate) struct Grants {
    pub directories: Vec<String>,
    pub mask: u64,
    pub net_rules: Vec<GrantRule>,
    pub secrets: Vec<SecretGrant>,
    pub env: Option<EnvPatterns>,
}

impl Grants {
    /// Add one `--grant` value.
    pub fn grant(&mut self, value: &str) -> Result<(), Box<dyn Error>> {
        if value == "Secret" || value.starts_with("Secret:") {
            let grant = SecretGrant::parse(value).ok_or(SecretGrant::SYNTAX)?;
            if self
                .secrets
                .iter()
                .any(|old| old.parameter == grant.parameter)
            {
                return Err("a Secret parameter can be granted only once".into());
            }
            self.secrets.push(grant);
        } else if value == "Net" {
            return Err(format!("Net requires --grant {RULE_SYNTAX}").into());
        } else if let Some(rules) = value.strip_prefix("Net=") {
            for rule in rules.split(';') {
                self.net_rules.push(GrantRule::parse(rule)?);
            }
            self.mask |= 1 << 1;
        } else if value == "Env" {
            return Err(EnvPatterns::SYNTAX.into());
        } else if let Some(patterns) = value.strip_prefix("Env=") {
            let patterns = EnvPatterns::parse(patterns).ok_or(EnvPatterns::SYNTAX)?;
            match &mut self.env {
                Some(granted) => granted.extend(patterns),
                None => self.env = Some(patterns),
            }
            self.mask |= 1 << 7;
        } else {
            let kind = crate::grant_kind(value)
                .filter(|kind| (1..=9).contains(kind))
                .ok_or("expected Net, Clock, Random, Stdout, Stderr, Stdin, Env, Process or Tasks; use --dir for Dir; Args is always present")?;
            self.mask |= 1 << kind;
        }
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.directories.is_empty() && self.mask == 0 && self.secrets.is_empty()
    }

    pub fn apply(&self, options: &mut NativeOptions) {
        options.directories.clone_from(&self.directories);
        options.grant_mask = self.mask;
        options.net_rules.clone_from(&self.net_rules);
        options.secret_grants.clone_from(&self.secrets);
        options.env_patterns.clone_from(&self.env);
    }
}

/// A directory grant that resolves inside the run's own working directory:
/// relative, never climbing out of it.
pub(crate) fn private_directory(path: &str) -> bool {
    Path::new(path)
        .components()
        .all(|c| matches!(c, Component::CurDir | Component::Normal(_)))
}

#[derive(Clone)]
struct Run {
    limit: u64,
    grants: Grants,
}

/// Each test's root limit and grants (PROG-4): one run for the package's
/// tests, any part of which a named test's own run replaces. Of these a
/// test is granted only what its parameters take; the launch masks the rest.
pub(crate) struct TestRuns {
    package: Run,
    named: BTreeMap<DefPath, Run>,
}

impl TestRuns {
    /// The command line's configuration, for a package that declares none.
    pub fn given(limit: u64, grants: Grants) -> Self {
        Self {
            package: Run { limit, grants },
            named: BTreeMap::new(),
        }
    }

    pub fn options(&self, test: &DefPath, base: &NativeOptions) -> NativeOptions {
        let run = self.named.get(test).unwrap_or(&self.package);
        let mut options = base.clone();
        options.root_limit = run.limit;
        run.grants.apply(&mut options);
        options
    }

    /// A configuration naming a test the package does not have is refused.
    pub fn check_names(&self, tests: &std::collections::BTreeSet<&DefPath>) -> Result<(), String> {
        match self.named.keys().find(|name| !tests.contains(name)) {
            Some(name) => Err(format!(
                "the package's test configuration names {name}, which is not one of its tests"
            )),
            None => Ok(()),
        }
    }
}

type Sources = BTreeMap<String, String>;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    package: PackageFile,
    #[serde(default)]
    sources: Sources,
    #[serde(default)]
    dependencies: BTreeMap<String, DependencyFile>,
    #[serde(default)]
    program: ProgramFile,
    tests: Option<TestFile>,
    #[serde(rename = "tool")]
    _tool: Option<toml::Table>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PackageFile {
    name: String,
    major: u32,
    charge: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DependencyFile {
    major: u32,
}

#[derive(Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ProgramFile {
    module: Option<String>,
    function: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct TestFile {
    #[serde(default)]
    sources: Sources,
    limit: u64,
    #[serde(default)]
    grants: Vec<String>,
    #[serde(default)]
    dirs: Vec<String>,
    #[serde(default)]
    runs: BTreeMap<String, RunFile>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RunFile {
    limit: Option<u64>,
    grants: Option<Vec<String>>,
    dirs: Option<Vec<String>>,
}

pub(crate) fn package_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    name.len() <= 64
        && bytes.next().is_some_and(|b| b.is_ascii_lowercase())
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

pub(crate) fn module_path(module: &str) -> Option<Vec<String>> {
    let mut symbols = hirsel_core_syntax::Symbols::default();
    let names: Vec<_> = module.split('.').collect();
    names
        .iter()
        .all(|name| {
            let lexed = hirsel_core_syntax::lex(name, &mut symbols);
            lexed.diagnostics.is_empty()
                && lexed.tokens.first().is_some_and(|token| {
                    token.kind == hirsel_core_syntax::TokenKind::Ident
                        && token.span.start == 0
                        && token.span.end as usize == name.len()
                })
        })
        .then(|| names.into_iter().map(str::to_owned).collect())
}

pub(crate) fn path_rule(path: &str, suffix: &str) -> Result<(), String> {
    if path.len() > 1024 {
        return Err("path over 1024 bytes".into());
    }
    for component in path.split('/') {
        if component.len() > 255 {
            return Err("component over 255 bytes".into());
        }
        let mut bytes = component.bytes();
        let first = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
        if !bytes.next().is_some_and(first) || !bytes.all(|b| first(b) || b".-".contains(&b)) {
            return Err(format!("bad component {component:?}"));
        }
    }
    if !path.ends_with(suffix) {
        return Err(format!("does not end in {suffix}"));
    }
    Ok(())
}

pub(crate) struct PackageDir {
    /// Its sources' paths joined to the directory, which names them.
    pub package: PackageManifest,
    pub entry: DefPath,
    /// The directory, when a manifest lists its sources.
    pub manifest: Option<PathBuf>,
    /// Sources that join the package in its test runs only, and its tests'
    /// runs, when the manifest declares them.
    pub tests: Option<(Vec<ModuleSource>, TestRuns)>,
}

impl PackageDir {
    pub fn open(dir: &Path) -> io::Result<Self> {
        let mut manifest = None;
        for parent in dir.ancestors() {
            let parent = if parent.as_os_str().is_empty() {
                Path::new(".")
            } else {
                parent
            };
            let path = parent.join(MANIFEST);
            match std::fs::read(&path) {
                Ok(bytes) => {
                    manifest = Some((parent, path, bytes));
                    break;
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        let Some((dir, path, bytes)) = manifest else {
            return Ok(Self {
                package: PackageManifest::directory(dir, crate::package("app"), false)?,
                entry: DefPath::from_full("app@1/main/main").unwrap(),
                manifest: None,
                tests: None,
            });
        };
        let refuse = |message: String| io::Error::other(format!("{}: {message}", path.display()));
        let file: File =
            toml::from_str(std::str::from_utf8(&bytes).map_err(|e| refuse(e.to_string()))?)
                .map_err(|e| refuse(e.to_string()))?;
        let name = file.package.name;
        if !package_name(&name) || matches!(name.as_str(), "core" | "std") {
            return Err(refuse(format!(
                "package.name: invalid package name {name:?}"
            )));
        }
        if file.package.charge != "charge-v1" {
            return Err(refuse(format!(
                "package.charge: unknown charge policy {:?}",
                file.package.charge
            )));
        }
        let id = PackageId {
            name,
            version: file.package.major.to_string(),
        };
        let mut dependencies = vec![crate::package("core")];
        for (name, binding) in file.dependencies {
            match name.as_str() {
                "std" if binding.major == 1 => dependencies.push(crate::package("std")),
                "std" => {
                    return Err(refuse(format!(
                        "dependencies.std: the toolchain has std@1, not std@{}",
                        binding.major
                    )));
                }
                "core" => {
                    return Err(refuse(
                        "dependencies.core: core@1 is bound in every package and is not declared"
                            .into(),
                    ));
                }
                _ => {
                    return Err(refuse(format!(
                        "dependencies.{name}: give exactly one of path, root, workspace or a toolchain major"
                    )));
                }
            }
        }
        let mut listed_paths = BTreeMap::new();
        let mut sources = |listed: Sources, field| {
            let mut out = Vec::with_capacity(listed.len());
            for (file, module) in listed {
                path_rule(&file, ".hirsel").map_err(|rule| {
                    refuse(format!("{field}: {file:?} breaks the path rules: {rule}"))
                })?;
                let modules = module_path(&module)
                    .ok_or_else(|| refuse(format!("{field}: {module:?} is not a module path")))?;
                if let Some(prior) = listed_paths.insert(file.to_ascii_lowercase(), file.clone()) {
                    return Err(refuse(format!(
                        "{field}: {prior:?} and {file:?} differ only in case"
                    )));
                }
                out.push(ModuleSource {
                    file: dir.join(file),
                    modules,
                    signatures: false,
                    foreign: false,
                });
            }
            Ok::<_, io::Error>(out)
        };
        if file.sources.is_empty() {
            return Err(refuse(
                "sources: a package lists at least one source".into(),
            ));
        }
        let package = PackageManifest {
            package: id.clone(),
            dependencies,
            sources: sources(file.sources, "sources")?,
        };
        let module = file.program.module.as_deref().unwrap_or("main");
        let modules = module_path(module)
            .ok_or_else(|| refuse(format!("program.module: {module:?} is not a module path")))?;
        let function = file.program.function.as_deref().unwrap_or("main");
        if module_path(function).is_none_or(|parts| parts.len() != 1) {
            return Err(refuse(format!(
                "program.function: {function:?} is not an identifier"
            )));
        }
        let entry = DefPath::from_full(&format!(
            "{}@{}/{}/{function}",
            id.name,
            id.version,
            modules.join("/")
        ))
        .unwrap();
        let tests = file
            .tests
            .map(|tests| {
                let test_sources = sources(tests.sources, "tests.sources")?;
                let grants = |grants: &[String], dirs: &[String]| {
                    let mut out = Grants {
                        directories: dirs.to_vec(),
                        ..Grants::default()
                    };
                    for grant in grants {
                        out.grant(grant).map_err(|e| refuse(e.to_string()))?;
                    }
                    Ok::<_, io::Error>(out)
                };
                let package = Run {
                    limit: tests.limit,
                    grants: grants(&tests.grants, &tests.dirs)?,
                };
                let mut named = BTreeMap::new();
                for (name, run) in tests.runs {
                    let test = DefPath::from_full(&name)
                        .ok_or_else(|| refuse(format!("invalid test path {name}")))?;
                    let run = Run {
                        limit: run.limit.unwrap_or(package.limit),
                        grants: grants(
                            run.grants.as_deref().unwrap_or(&tests.grants),
                            run.dirs.as_deref().unwrap_or(&tests.dirs),
                        )?,
                    };
                    named.insert(test, run);
                }
                Ok::<_, io::Error>((test_sources, TestRuns { package, named }))
            })
            .transpose()?;
        Ok(Self {
            package,
            entry,
            manifest: Some(dir.to_owned()),
            tests,
        })
    }
}
