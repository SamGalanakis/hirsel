//! A package directory and the manifest beside its sources (PROG-3, PROG-4):
//! the package, the entry that names `main`, and its tests' run
//! configuration. A directory without a manifest is the directory
//! convention: package `app`, one module per file, `main` in `main`.
use crate::NativeOptions;
use crate::package_content::{Content, Id, PackageArchive, Root, Source};
use hirsel_core_ids::{DefPath, PackageId};
use hirsel_core_net::{EnvPatterns, GrantRule, NetGrant, RULE_SYNTAX, SecretGrant};
use hirsel_core_resolve::{ForeignModuleCapture, ModuleSource, PackageManifest};
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
    pub unix_roots: BTreeMap<String, String>,
    pub secrets: Vec<SecretGrant>,
    pub creds: Vec<hirsel_core_net::CredGrant>,
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
        } else if value == "Cred" || value.starts_with("Cred:") {
            let grant = hirsel_core_net::CredGrant::parse(value)?;
            if self
                .creds
                .iter()
                .any(|old| old.parameter == grant.parameter)
            {
                return Err("a Cred parameter can be granted only once".into());
            }
            self.creds.push(grant);
        } else if value == "Net" {
            return Err(format!("Net requires --grant {RULE_SYNTAX}").into());
        } else if let Some(rules) = value.strip_prefix("Net=") {
            let grant = NetGrant::parse(rules.split(';'))?;
            if grant
                .roots
                .keys()
                .any(|label| self.unix_roots.contains_key(label))
            {
                return Err(format!("expected --grant {RULE_SYNTAX}: duplicate UnixRoot").into());
            }
            self.net_rules.extend(grant.rules);
            self.unix_roots.extend(grant.roots);
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
            && self.creds.is_empty()
            && self.foreign.is_empty()
            && self.exec.is_empty()
    }

    pub fn apply(&self, options: &mut NativeOptions) {
        options.directories.clone_from(&self.directories);
        options.grant_mask = self.mask;
        options.net_rules.clone_from(&self.net_rules);
        options.unix_roots.clone_from(&self.unix_roots);
        options.secret_grants.clone_from(&self.secrets);
        options.cred_grants.clone_from(&self.creds);
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

/// A manifest's `grants` and `dirs`, in `--grant` and `--dir` syntax.
fn manifest_grants(path: &Path, grants: &[String], dirs: &[String]) -> io::Result<Grants> {
    let mut out = Grants {
        directories: dirs
            .iter()
            .map(|dir| hirsel_core_ids::launch::dir_slot(dir))
            .collect::<Result<_, _>>()
            .map_err(|e| io::Error::other(format!("{}: {e}", path.display())))?,
        ..Grants::default()
    };
    for grant in grants {
        out.grant(grant)
            .map_err(|e| io::Error::other(format!("{}: {e}", path.display())))?;
    }
    Ok(out)
}

/// `[program]`'s `limit`, `grants` and `dirs` (R3), present when the manifest
/// declares any of them.
pub(crate) struct ProgramRun {
    limit: Option<u64>,
    grants: Vec<String>,
    dirs: Vec<String>,
}

impl ProgramRun {
    pub(crate) fn declared(
        limit: Option<u64>,
        grants: Option<Vec<String>>,
        dirs: Option<Vec<String>>,
    ) -> Option<Self> {
        (limit.is_some() || grants.is_some() || dirs.is_some()).then(|| Self {
            limit,
            grants: grants.unwrap_or_default(),
            dirs: dirs.unwrap_or_default(),
        })
    }
}

/// The selected package's `[program]` run configuration, read only for the
/// package a command builds: never a dependency's.
pub(crate) struct ProgramGrants {
    limit: Option<u64>,
    grants: Grants,
}

impl TestFile {
    fn runs(&self, path: &Path) -> io::Result<TestRuns> {
        let refuse = |message| io::Error::other(format!("{}: {message}", path.display()));
        let grants = |grants: &[String], dirs: &[String]| manifest_grants(path, grants, dirs);
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
    pub(crate) program: Option<ProgramGrants>,
    /// The directory of each workspace member whose identity is a toolchain
    /// package's: `trust_library_root` decides whether the toolchain supplied it.
    toolchain_members: Vec<(PackageId, PathBuf)>,
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
            program: None,
            toolchain_members: Vec::new(),
        })
    }

    /// Marks the members the invoker's library root `root` holds as supplied
    /// by the toolchain: a member with a toolchain package's identity whose
    /// directory is `root/programs/<name>` (after symlinks). The root already
    /// supplies `core@1` and `std@1`, so this is that same trust; a member
    /// anywhere else is a plain package whatever it is named (launch.md,
    /// "Private prefix").
    pub(crate) fn trust_library_root(&mut self, root: &Path) {
        let Ok(entries) = std::fs::read_dir(root.join("programs")) else {
            return;
        };
        let held: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok()?.path().canonicalize().ok())
            .collect();
        for (id, directory) in &self.toolchain_members {
            let Ok(directory) = directory.canonicalize() else {
                continue;
            };
            if !held.contains(&directory) {
                continue;
            }
            for package in std::iter::once(&mut self.package).chain(&mut self.dependencies) {
                if package.package == *id {
                    package.toolchain = true;
                }
            }
        }
    }

    pub(crate) fn from_resolution(
        resolution: &crate::package_resolve::Resolution,
        selected: &Id,
        include_tests: bool,
    ) -> io::Result<Self> {
        use crate::package_workspace::Edge;
        use std::collections::BTreeSet;

        let selected_member = resolution.workspace.members.get(selected);
        let mut toolchain_members = Vec::new();
        let mut seen = BTreeSet::new();
        let mut pending = vec![selected.clone()];
        let mut dependencies = Vec::new();
        let mut main = None;
        while let Some(id) = pending.pop() {
            if id.name == "std" || !seen.insert(id.clone()) {
                continue;
            }
            let root = resolution.published.get(&id).copied();
            let toolchain_package;
            let mut supplied = false;
            let (content, bindings) = if let Some(member) = resolution.workspace.members.get(&id) {
                if crate::package_content::toolchain_bindings(&id).is_some() {
                    toolchain_members.push((
                        id.compiler_id(),
                        resolution.workspace.directory.join(&member.directory),
                    ));
                }
                let bindings = member
                    .edges
                    .iter()
                    .map(|(id, edge)| {
                        let pin = match edge {
                            Edge::Published(root, _) => Some(*root),
                            _ => None,
                        };
                        (id, pin)
                    })
                    .collect::<Vec<_>>();
                (&member.content, bindings)
            } else {
                let content = if resolution.toolchain.contains(&id) {
                    toolchain_package = crate::installation::Installed::toolchain_package(&id)?;
                    supplied = true;
                    &toolchain_package
                } else {
                    &resolution.libraries[&root.unwrap()].content
                };
                (
                    content,
                    content
                        .bindings
                        .iter()
                        .map(|(id, pin)| (id, *pin))
                        .collect(),
                )
            };
            pending.extend(bindings.iter().map(|(id, _)| (**id).clone()));
            let tests = if include_tests && &id == selected {
                selected_member.map(|member| &member.tests)
            } else {
                None
            };
            let mut package = Self::manifest_from_content(content, selected, root, bindings, tests);
            package.toolchain = supplied;
            if &id == selected {
                main = Some(package);
            } else {
                dependencies.push(package);
            }
        }
        let tests = if include_tests {
            selected_member
                .and_then(|member| {
                    member
                        .package
                        .tests
                        .as_ref()
                        .map(|tests| tests.runs(&member.manifest_path))
                })
                .transpose()?
        } else {
            None
        };
        let program = selected_member
            .and_then(|member| {
                let run = member.package.program.as_ref()?;
                Some(
                    manifest_grants(&member.manifest_path, &run.grants, &run.dirs).map(|grants| {
                        ProgramGrants {
                            limit: run.limit,
                            grants,
                        }
                    }),
                )
            })
            .transpose()?;
        Ok(Self {
            package: main.expect("the selection is a non-toolchain package"),
            dependencies,
            entry: selected_member.map_or_else(
                || Self::archive_entry(selected),
                |member| member.package.entry.clone(),
            ),
            manifest: Some(selected_member.map_or_else(
                || resolution.workspace.directory.clone(),
                |member| member.manifest_path.parent().unwrap().to_owned(),
            )),
            tests,
            program,
            toolchain_members,
        })
    }

    fn archive_entry(id: &Id) -> DefPath {
        DefPath::from_full(&format!("{id}/main/main")).expect("a validated package identity")
    }

    pub(crate) fn from_archive(archive: &PackageArchive) -> io::Result<Self> {
        let head = &archive.entries[&archive.head].content;
        let manifest = |content: &Content, root| {
            Self::manifest_from_content(
                content,
                &head.id,
                Some(root),
                content.bindings.iter().map(|(id, pin)| (id, *pin)),
                None,
            )
        };
        let mut roots = BTreeMap::new();
        let mut dependencies = Vec::new();
        let mut bound = Vec::new();
        for (root, library) in &archive.entries {
            let content = &library.content;
            bound.extend(
                content
                    .bindings
                    .iter()
                    .filter_map(|(id, root)| root.is_none().then_some(id)),
            );
            if let Some(previous) = roots.insert(&content.id, root) {
                return Err(io::Error::other(format!(
                    "{} is bound to both {previous} and {root}",
                    content.id
                )));
            }
            if *root != archive.head {
                dependencies.push(manifest(content, *root));
            }
        }
        for id in crate::package_content::toolchain_closure(bound) {
            if roots.contains_key(&id) {
                return Err(io::Error::other(format!(
                    "{id} is bound both to the toolchain and to a published library"
                )));
            }
            let supplied = crate::installation::Installed::toolchain_package(&id)?;
            let mut package = Self::manifest_from_content(
                &supplied,
                &head.id,
                None,
                supplied.bindings.iter().map(|(id, pin)| (id, *pin)),
                None,
            );
            package.toolchain = true;
            dependencies.push(package);
        }
        Ok(Self {
            package: manifest(head, archive.head),
            dependencies,
            entry: Self::archive_entry(&head.id),
            manifest: None,
            tests: None,
            program: None,
            toolchain_members: Vec::new(),
        })
    }

    fn manifest_from_content<'a>(
        content: &Content,
        selected: &Id,
        root: Option<Root>,
        bindings: impl IntoIterator<Item = (&'a Id, Option<Root>)>,
        tests: Option<&BTreeMap<String, Source>>,
    ) -> PackageManifest {
        use hirsel_core_ids::ContentHash;
        use hirsel_core_resolve::{CapturedSource, PackageNamespace};

        let sources = content
            .files
            .iter()
            .map(|row| (row, false))
            .chain(
                tests
                    .into_iter()
                    .flat_map(|tests| tests.iter().map(|row| (row, true))),
            )
            .map(|((file, source), test)| {
                let logical = if content.id == *selected {
                    file.clone()
                } else {
                    format!("{}/{file}", content.id)
                };
                ModuleSource {
                    foreign: content
                        .foreign
                        .get(&source.module)
                        .map(ForeignModuleCapture::containment),
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
            .collect();
        let mut dependencies = vec![crate::package("core")];
        let mut published_bindings = BTreeMap::new();
        for (id, pin) in bindings {
            let id = id.compiler_id();
            if let Some(pin) = pin {
                published_bindings.insert(id.clone(), ContentHash(pin.0));
            }
            dependencies.push(id);
        }
        PackageManifest {
            namespace: Some(PackageNamespace {
                root: root.map(|root| ContentHash(root.0)),
                foreign: content.foreign_stamp(),
                foreign_inputs: content.foreign.clone(),
                published_bindings,
            }),
            package: content.id.compiler_id(),
            dependencies,
            sources,
            toolchain: false,
        }
    }

    /// R3: the grants and root limit of a `build`, `publish` or `run`. A package
    /// that declares `[program]` grants takes them from there alone, and the
    /// command line may add only Foreign and Exec grants, as for `[tests]`.
    pub(crate) fn program_grants(
        &mut self,
        command: &str,
        grants: &mut Grants,
        root_limit: &mut u64,
        explicit_limit: bool,
    ) -> Result<(), String> {
        let Some(program) = self.program.take() else {
            return Ok(());
        };
        if !matches!(command, "build" | "publish" | "run") {
            return Ok(());
        }
        let mut configured = grants.clone();
        configured.foreign.clear();
        configured.exec.clear();
        if explicit_limit || !configured.is_empty() {
            return Err("this package declares [program] grants in hirsel.toml. Set limit, dirs and grants there".into());
        }
        let mut manifest = program.grants;
        manifest.foreign.append(&mut grants.foreign);
        manifest.exec.append(&mut grants.exec);
        *grants = manifest;
        if let Some(limit) = program.limit {
            *root_limit = limit;
        }
        Ok(())
    }

    pub(crate) fn needs_std(&self) -> bool {
        std::iter::once(&self.package)
            .chain(&self.dependencies)
            .any(|package| package.dependencies.contains(&crate::package("std")))
    }
}
