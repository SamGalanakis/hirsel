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

const MANIFEST: &str = "manifest.json";

pub(crate) fn scaffold_manifest(sources: impl Iterator<Item = &'static str>) -> String {
    use serde_json::json;
    let sources = sources
        .map(|file| json!({"file": file, "modules": [file.trim_end_matches(".hirsel")], "signatures": false}))
        .collect::<Vec<_>>();
    let manifest = json!({
        "packages": [{
            "package": crate::package("app"),
            "dependencies": [crate::package("core"), crate::package("std")],
            "sources": sources,
        }],
        "tests": {
            "sources": [{"file": "tests.hirsel", "modules": ["tests"], "signatures": false}],
            "limit": 64 * 1024 * 1024,
        },
    });
    format!("{manifest:#}\n")
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

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    packages: Vec<PackageManifest>,
    #[serde(default)]
    entries: Vec<EntryFile>,
    tests: Option<TestFile>,
}

/// An entry row: the tool fields beside these (workload, grants) are read
/// by the measurement tools, not here.
#[derive(serde::Deserialize)]
struct EntryFile {
    package: PackageId,
    module: String,
    function: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct TestFile {
    #[serde(default)]
    sources: Vec<ModuleSource>,
    limit: Option<u64>,
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
        let path = dir.join(MANIFEST);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Ok(Self {
                    package: PackageManifest::directory(dir, crate::package("app"), false)?,
                    entry: DefPath::from_full("app@1/main/main").unwrap(),
                    manifest: None,
                    tests: None,
                });
            }
            Err(e) => return Err(e),
        };
        let refuse = |message: String| io::Error::other(format!("{}: {message}", path.display()));
        let file: File = serde_json::from_slice(&bytes).map_err(|e| refuse(e.to_string()))?;
        let [mut package] = <[PackageManifest; 1]>::try_from(file.packages)
            .map_err(|_| refuse("a package directory's manifest lists one package".into()))?;
        let join = |sources: &mut Vec<ModuleSource>| {
            for source in sources {
                source.file = dir.join(&source.file);
            }
        };
        join(&mut package.sources);
        let mut entries = file.entries.iter().filter(|e| e.package == package.package);
        let entry = match (entries.next(), entries.next()) {
            (_, Some(_)) => return Err(refuse("the package names one entry".into())),
            (Some(e), None) => format!(
                "{}@{}/{}/{}",
                package.package.name,
                package.package.version,
                e.module,
                e.function.as_deref().unwrap_or("main")
            ),
            (None, None) => format!(
                "{}@{}/main/main",
                package.package.name, package.package.version
            ),
        };
        let entry = DefPath::from_full(&entry).ok_or_else(|| refuse(format!("entry {entry}")))?;
        let tests = file
            .tests
            .map(|mut tests| {
                join(&mut tests.sources);
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
                    limit: tests
                        .limit
                        .ok_or_else(|| refuse("tests declare their root limit".into()))?,
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
                Ok::<_, io::Error>((tests.sources, TestRuns { package, named }))
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
