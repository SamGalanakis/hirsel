//! A package directory and the manifest beside its sources (PROG-3, PROG-4):
//! the package, the entry that names `main`, and its tests' run
//! configuration. A directory without a manifest is the directory
//! convention: package `app`, one module per file, `main` in `main`.
use crate::NativeOptions;
use hirsel_core_ids::DefPath;
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
    pub foreign: std::collections::BTreeSet<String>,
    pub exec: Vec<crate::exec_launch::Binding>,
    pub env: Option<EnvPatterns>,
}

impl Grants {
    /// Add one `--grant` value.
    pub fn grant(&mut self, value: &str) -> Result<(), Box<dyn Error>> {
        if let Some(request) = value.strip_prefix("Exec=") {
            let request = hirsel_core_net::exec::Request::parse(request)?;
            self.exec.push(crate::exec_launch::Binding {
                request,
                policy: None,
                workdir: None,
            });
        } else if value == "Exec" {
            return Err(hirsel_core_net::exec::GRANT_SYNTAX.into());
        } else if let Some(interface) = value.strip_prefix("Foreign=") {
            DefPath::from_full(interface)
                .ok_or("expected Foreign=package@major/module/interface")?;
            if !self.foreign.insert(interface.to_owned()) {
                return Err("a Foreign interface can be granted only once".into());
            }
        } else if value == "Secret" || value.starts_with("Secret:") {
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
        self.directories.is_empty()
            && self.mask == 0
            && self.secrets.is_empty()
            && self.foreign.is_empty()
            && self.exec.is_empty()
    }

    pub fn apply(&self, options: &mut NativeOptions) {
        options.directories.clone_from(&self.directories);
        options.grant_mask = self.mask;
        options.net_rules.clone_from(&self.net_rules);
        options.secret_grants.clone_from(&self.secrets);
        options.foreign_grants.clone_from(&self.foreign);
        options.exec_grants.clone_from(&self.exec);
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
pub(crate) struct TestFile {
    #[serde(default)]
    pub(crate) sources: Sources,
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

impl TestFile {
    fn runs(&self, path: &Path) -> io::Result<TestRuns> {
        let refuse = |message| io::Error::other(format!("{}: {message}", path.display()));
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
            limit: self.limit,
            grants: grants(&self.grants, &self.dirs)?,
        };
        let mut named = BTreeMap::new();
        for (name, run) in &self.runs {
            let test = DefPath::from_full(name)
                .ok_or_else(|| refuse(format!("invalid test path {name}")))?;
            named.insert(
                test,
                Run {
                    limit: run.limit.unwrap_or(package.limit),
                    grants: grants(
                        run.grants.as_deref().unwrap_or(&self.grants),
                        run.dirs.as_deref().unwrap_or(&self.dirs),
                    )?,
                },
            );
        }
        Ok(TestRuns { package, named })
    }
}

pub(crate) struct PackageDir {
    pub(crate) package: PackageManifest,
    pub(crate) dependencies: Vec<PackageManifest>,
    pub(crate) entry: DefPath,
    pub(crate) manifest: Option<PathBuf>,
    pub(crate) tests: Option<TestRuns>,
}

impl PackageDir {
    pub(crate) fn open(dir: &Path) -> io::Result<Self> {
        use crate::{
            package_resolve::Resolution, package_store::PackageStore, package_workspace::Workspace,
        };
        let workspace =
            Workspace::discover(dir).map_err(|error| io::Error::other(error.to_string()))?;
        let Some(workspace) = workspace else {
            return Self::convention(dir);
        };
        let selected = workspace.selected().map_err(io::Error::other)?.clone();
        let resolution = Resolution::resolve(workspace, PackageStore::new(None), false)
            .map_err(|error| io::Error::other(error.to_string()))?;
        Self::from_resolution(&resolution, &selected, false)
    }

    pub(crate) fn convention(dir: &Path) -> io::Result<Self> {
        let mut package = PackageManifest::directory(dir, crate::package("app"), false)?;
        package
            .dependencies
            .extend([crate::package("core"), crate::package("std")]);
        Ok(Self {
            package,
            dependencies: Vec::new(),
            entry: DefPath::from_full("app@1/main/main").unwrap(),
            manifest: None,
            tests: None,
        })
    }

    pub(crate) fn from_resolution(
        resolution: &crate::package_resolve::Resolution,
        selected: &crate::package_content::Id,
        include_tests: bool,
    ) -> io::Result<Self> {
        use hirsel_core_resolve::{CapturedSource, PackageNamespace};
        use std::collections::BTreeSet;

        let make_sources =
            |name: &crate::package_content::Id,
             sources: &BTreeMap<String, crate::package_content::Source>,
             foreign: &BTreeMap<Vec<String>, crate::package_content::ForeignModule>,
             test: bool| {
                sources
                    .iter()
                    .map(|(file, source)| {
                        let logical = if name == selected {
                            file.clone()
                        } else {
                            format!("{name}/{file}")
                        };
                        ModuleSource {
                            foreign: foreign.contains_key(&source.module),
                            file: PathBuf::from(&logical),
                            modules: source.module.clone(),
                            signatures: false,
                            captured: Some(CapturedSource {
                                bytes: source.bytes.clone(),
                                name: Some(logical),
                                test,
                            }),
                        }
                    })
                    .collect::<Vec<_>>()
            };
        let this = &resolution.workspace.members[selected];
        let mut seen = BTreeSet::new();
        let mut pending = vec![selected.clone()];
        let mut dependencies = Vec::new();
        let mut main = None;
        while let Some(id) = pending.pop() {
            if id.name == "std" || !seen.insert(id.clone()) {
                continue;
            }
            let (content, edges) = if let Some(member) = resolution.workspace.members.get(&id) {
                (&member.content, member.edges.keys().collect::<Vec<_>>())
            } else {
                let content = &resolution.libraries[&resolution.published[&id]].content;
                (content, content.bindings.keys().collect())
            };
            pending.extend(edges.iter().copied().cloned());
            let mut sources = make_sources(&id, &content.files, &content.foreign, false);
            if include_tests && &id == selected {
                sources.extend(make_sources(&id, &this.tests, &content.foreign, true));
            }
            let published_bindings = if let Some(member) = resolution.workspace.members.get(&id) {
                member
                    .edges
                    .iter()
                    .filter_map(|(id, edge)| match edge {
                        crate::package_workspace::Edge::Published(root, _) => {
                            Some((id.compiler_id(), hirsel_core_ids::ContentHash(root.0)))
                        }
                        _ => None,
                    })
                    .collect()
            } else {
                content
                    .bindings
                    .iter()
                    .filter_map(|(id, root)| {
                        root.map(|root| (id.compiler_id(), hirsel_core_ids::ContentHash(root.0)))
                    })
                    .collect()
            };
            let namespace = PackageNamespace {
                root: resolution
                    .published
                    .get(&id)
                    .map(|root| hirsel_core_ids::ContentHash(root.0)),
                foreign: content.foreign_stamp(),
                foreign_inputs: content.foreign.clone(),
                published_bindings,
            };
            let manifest = PackageManifest {
                namespace: Some(namespace),
                package: id.compiler_id(),
                dependencies: std::iter::once(crate::package("core"))
                    .chain(edges.into_iter().map(|id| id.compiler_id()))
                    .collect(),
                sources,
            };
            if &id == selected {
                main = Some(manifest);
            } else {
                dependencies.push(manifest);
            }
        }
        Ok(Self {
            package: main.unwrap(),
            dependencies,
            entry: this.package.entry.clone(),
            manifest: Some(this.manifest_path.parent().unwrap().to_owned()),
            tests: if include_tests {
                this.package
                    .tests
                    .as_ref()
                    .map(|tests| tests.runs(&this.manifest_path))
                    .transpose()?
            } else {
                None
            },
        })
    }

    pub(crate) fn needs_std(&self) -> bool {
        std::iter::once(&self.package)
            .chain(&self.dependencies)
            .any(|package| package.dependencies.contains(&crate::package("std")))
    }
}
