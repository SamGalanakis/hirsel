use crate::cli::{Request, UpdateOption};
use crate::package_add::{acquire_any, current_scope, edit, git, lock_scope, options::Source};
use crate::package_content::{Id, Root};
use crate::package_dir::MANIFEST;
use crate::package_files::{Captured, open_directory, walk};
use crate::package_lock::{Lock, Mode};
use crate::package_manifest::{Dependency, Manifest, SourceLocation, name};
use crate::package_resolve::{Libraries, Resolution, select};
use crate::package_store::PackageStore;
use crate::package_workspace::{Workspace, archive_path};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::path::{Path, PathBuf};

mod report;
pub(crate) mod transaction;
use report::Report;
use transaction::Edit;

type Failure = Box<dyn std::error::Error>;

enum Target<'a> {
    All,
    Alias(&'a str),
    AliasVersion(&'a str, name::Version),
    Identity(&'a str, Option<u32>),
    Version(Id, name::Version),
}

impl<'a> Target<'a> {
    fn parse(text: Option<&'a str>) -> Result<Self, Failure> {
        let Some(text) = text else {
            return Ok(Self::All);
        };
        let (package, version) = text
            .split_once('@')
            .map_or((text, None), |(p, v)| (p, Some(v)));
        if name::scoped(package) {
            return Ok(match version {
                None => Self::Identity(package, None),
                Some(major) if !major.contains('.') => {
                    Self::Identity(package, Some(major.parse()?))
                }
                Some(version) => {
                    let version =
                        name::Version::parse(version).ok_or("expected ID@MAJOR or ID@VERSION")?;
                    Self::Version(
                        Id {
                            name: package.to_owned(),
                            major: version.major,
                        },
                        version,
                    )
                }
            });
        }
        if name::alias(package) {
            return Ok(match version {
                None => Self::Alias(package),
                Some(v) => Self::AliasVersion(
                    package,
                    name::Version::parse(v).ok_or("expected ALIAS@VERSION")?,
                ),
            });
        }
        Err(format!("{text:?} is not a dependency alias or scoped identity").into())
    }

    fn version(&self) -> Option<(&Id, &name::Version)> {
        match self {
            Self::Version(id, version) => Some((id, version)),
            _ => None,
        }
    }

    fn includes(&self, id: &Id, alias: &str, scope: bool) -> bool {
        match self {
            Self::All => true,
            Self::Alias(target) | Self::AliasVersion(target, _) => scope && *target == alias,
            Self::Identity(package, major) => {
                id.name == *package && major.is_none_or(|m| m == id.major)
            }
            Self::Version(target, _) => *target == *id,
        }
    }
}

#[derive(Serialize)]
struct Rebound {
    row: String,
    old: String,
    new: String,
}

struct Refresh<'a> {
    registry: crate::package_add::registry_select::Registry,
    constraints: crate::package_add::registry_select::Constraints,
    choices: BTreeMap<Id, BTreeSet<Root>>,
    editable: BTreeSet<(PathBuf, String, bool)>,
    workspace: &'a Workspace,
    store: &'a PackageStore,
    fetched: BTreeSet<String>,
    libraries: Libraries,
    rebound: Vec<Rebound>,
    captured: BTreeMap<SourceLocation, (Id, Root, SourceLocation)>,
}

impl Refresh<'_> {
    fn capture(
        &mut self,
        source: &SourceLocation,
        id: &Id,
    ) -> Result<Option<(Root, SourceLocation)>, Failure> {
        let requested = match source {
            SourceLocation::Archive(path) => Source::Archive(path.clone()),
            SourceLocation::Url(url) => Source::Url(url.clone()),
            SourceLocation::Registry(_) => unreachable!(),
            SourceLocation::Git(git) => {
                let Some(reference) = &git.reference else {
                    return Ok(None);
                };
                Source::Git(git::Request {
                    url: git.url.clone(),
                    dir: git.dir.clone(),
                    reference: Some(reference.clone()),
                })
            }
        };

        if !self.captured.contains_key(source) {
            let (bundle, fresh, _, fetched, _) = acquire_any(
                &requested,
                self.store,
                self.workspace,
                &self.workspace.directory,
            )?;
            let actual = &bundle.entries[&bundle.head].content.id;
            crate::package_content::check_identity(id, bundle.head, actual)?;
            let fresh = fresh.ok_or("a refreshed bundle must have a source")?;
            self.store.install(&bundle)?;
            self.fetched.insert(fresh.to_string());
            self.captured
                .insert(source.clone(), (actual.clone(), bundle.head, fresh));
            self.libraries.extend(bundle.entries);
            self.fetched.extend(fetched);
        }
        let (actual, root, fresh) = &self.captured[source];
        crate::package_content::check_identity(id, *root, actual)?;
        Ok(Some((*root, fresh.clone())))
    }

    fn rows(
        &mut self,
        file: &mut Edit,
        deps: &BTreeMap<String, Dependency>,
        target: &Target<'_>,
        shared: bool,
        alias_scope: bool,
    ) -> Result<(), Failure> {
        for (alias, dependency) in deps {
            let Dependency::Published(id, before, Some(source), _) = dependency else {
                continue;
            };
            if !target.includes(id, alias, alias_scope) {
                continue;
            }
            let source = match source {
                SourceLocation::Archive(path) => {
                    let root_relative = archive_path(&file.relative, path)
                        .ok_or("archive is not inside the workspace")?;
                    SourceLocation::Archive(root_relative)
                }
                _ => source.clone(),
            };
            let (root, fresh) = if let SourceLocation::Registry(origin) = &source {
                let exact = match target {
                    Target::Version(_, v) | Target::AliasVersion(_, v) => Some(v.clone()),
                    _ => None,
                };
                let query = crate::package_add::registry_select::Query {
                    name: id.name.clone(),
                    major: Some(id.major),
                    exact,
                };
                let choice = self.registry.select(
                    &query,
                    Some((origin, *before, if alias_scope { alias } else { &id.name })),
                    |r, v| self.constraints.accepts(id, r, v),
                )?;
                self.choices
                    .entry(id.clone())
                    .or_default()
                    .insert(choice.root);
                self.editable
                    .insert((file.relative.clone(), alias.clone(), shared));
                self.store
                    .install_entry(choice.root, &choice.library.encoded)?;
                self.libraries.insert(choice.root, choice.library);
                (choice.root, source.clone())
            } else {
                let Some(refresh) = self.capture(&source, id)? else {
                    continue;
                };
                refresh
            };
            if root == *before && fresh == source {
                continue;
            }
            let declared = match dependency {
                Dependency::Published(_, _, Some(SourceLocation::Archive(path)), _) => {
                    SourceLocation::Archive(path.clone())
                }
                _ => fresh.clone(),
            };
            let form = edit::Form::Published(root, Some(&declared));
            let text = if shared {
                edit::shared(&file.after.bytes, alias, id, &form, false)?
            } else {
                edit::binding(&file.after.bytes, alias, id, &form)?
            };
            file.replace(text);
            self.rebound.push(Rebound {
                row: format!(
                    "{}: {}dependencies.{alias}",
                    file.relative.join(MANIFEST).display(),
                    if shared { "workspace." } else { "" }
                ),
                old: before.to_string(),
                new: root.to_string(),
            });
        }
        Ok(())
    }
}

fn directory(workspace: &Workspace, relative: &Path) -> Result<File, Failure> {
    if relative.as_os_str().is_empty() {
        return Ok(workspace.root.try_clone()?);
    }
    let dir = walk(&workspace.root, relative)
        .and_then(|item| item.directory())
        .map_err(|err| err.explain("workspace", "a directory"))?;
    Ok(open_directory(&crate::host::fd_path(&dir)?)?)
}

fn overlay(workspace: &Workspace, cwd: &Path, edits: &[Edit]) -> Result<Workspace, Failure> {
    let edits: Vec<_> = edits
        .iter()
        .filter(|edit| edit.changed())
        .map(|edit| (&edit.dir, &edit.after))
        .collect();
    workspace
        .with_manifests(cwd, &edits, false)?
        .ok_or("workspace changed while hirsel update ran".into())
}

pub(crate) fn run(request: Request<UpdateOption>) -> Result<i32, Failure> {
    let mut path = None;
    let mut dry_run = false;
    let mut json = false;
    for option in request.options {
        match option {
            UpdateOption::Locked => return Err("hirsel update refuses --locked".into()),
            UpdateOption::Frozen => return Err("hirsel update refuses --frozen".into()),
            UpdateOption::AddDryRun => dry_run = true,
            UpdateOption::Json => json = true,
            UpdateOption::PackageStore(store) => path = Some(PathBuf::from(store)),
        }
    }
    run_at(
        Target::parse(request.positional.as_deref())?,
        PackageStore::new(path),
        &std::env::current_dir()?,
        dry_run,
        json,
    )
}

fn run_at(
    target: Target<'_>,
    store: PackageStore,
    cwd: &Path,
    dry_run: bool,
    json: bool,
) -> Result<i32, Failure> {
    let scope = lock_scope(cwd)?;
    let workspace = current_scope(cwd, &scope, "update")?;
    let before_lock = Lock::capture(&workspace)?;
    let config = Manifest::parse(
        &workspace
            .captured_manifest(&scope)?
            .ok_or("workspace manifest disappeared")?
            .bytes,
        &workspace.directory.join(MANIFEST),
    )?;
    let selected = workspace.member.as_ref().map(|id| &workspace.members[id]);
    let mut inherited = selected.is_none();
    if let Target::Alias(alias) | Target::AliasVersion(alias, _) = target {
        match selected.and_then(|member| member.package.dependencies.get(alias)) {
            Some(Dependency::Inherited) => inherited = true,
            Some(_) => {}
            None if config
                .workspace
                .as_ref()
                .is_some_and(|w| w.dependencies.contains_key(alias)) =>
            {
                inherited = true;
            }
            None => {
                return Err(format!(
                    "no dependency alias {alias:?} in the current package or workspace"
                )
                .into());
            }
        }
    }
    let mut refresh = Refresh {
        registry: crate::package_add::registry_select::Registry::new(&store)?,
        choices: Default::default(),
        editable: Default::default(),
        constraints: Default::default(),
        workspace: &workspace,
        store: &store,
        fetched: BTreeSet::new(),
        libraries: Libraries::new(),
        rebound: Vec::new(),
        captured: BTreeMap::new(),
    };
    let alias_version = if let Target::AliasVersion(alias, version) = &target {
        let id = selected
            .and_then(|m| m.aliases.get(*alias))
            .cloned()
            .or_else(
                || match config.workspace.as_ref()?.dependencies.get(*alias)? {
                    Dependency::Published(id, ..) => Some(id.clone()),
                    _ => None,
                },
            )
            .ok_or("exact update requires a published dependency")?;
        if id.major != version.major {
            return Err(format!("{alias} binds {id}, outside {version}").into());
        }
        Some((id, version.clone()))
    } else {
        None
    };
    let mut tried = BTreeSet::new();
    let (mut edits, rebound) = loop {
        refresh.choices.clear();
        refresh.rebound.clear();
        refresh.registry.young_fix = None;
        let mut root = Edit::new(&workspace, scope.try_clone()?, PathBuf::new())?;
        if let Some(shared) = &config.workspace {
            refresh.rows(&mut root, &shared.dependencies, &target, true, inherited)?;
        }
        let mut edits = Vec::new();
        for (id, member) in &workspace.members {
            let alias_scope = !inherited && workspace.member.as_ref() == Some(id);
            if member.directory.as_os_str().is_empty() {
                refresh.rows(
                    &mut root,
                    &member.package.dependencies,
                    &target,
                    false,
                    alias_scope,
                )?;
            } else {
                let dir = directory(&workspace, &member.directory)?;
                let mut edit = Edit::new(&workspace, dir, member.directory.clone())?;
                refresh.rows(
                    &mut edit,
                    &member.package.dependencies,
                    &target,
                    false,
                    alias_scope,
                )?;
                if edit.changed() {
                    edits.push(edit);
                }
            }
        }
        edits.push(root);

        let rebound = overlay(&workspace, cwd, &edits)?;
        if refresh.choices.is_empty() {
            break (edits, rebound);
        }
        let libs = crate::package_resolve::registry_inputs(
            &rebound,
            &store,
            std::mem::take(&mut refresh.libraries),
            &mut refresh.fetched,
        )?;
        let constraints = crate::package_add::registry_select::Constraints::from_graph(
            &rebound,
            &libs,
            |id, alias| {
                let member = &rebound.members[id];
                let inherited = matches!(
                    member.package.dependencies.get(alias),
                    Some(Dependency::Inherited)
                );
                let at = if inherited {
                    PathBuf::new()
                } else {
                    member.directory.clone()
                };
                refresh
                    .editable
                    .contains(&(at, alias.to_owned(), inherited))
            },
        );
        let fits = refresh.choices.iter().all(|(id, roots)| {
            roots.iter().all(|root| {
                libs.get(root)
                    .is_none_or(|lib| constraints.accepts(id, *root, &lib.content.version))
            })
        });
        refresh.libraries = libs;
        if fits && !(tried.is_empty() && refresh.registry.young_fix.is_some()) {
            break (edits, rebound);
        }
        if !tried.insert(refresh.choices.clone()) {
            return Err("no compatible registry selection settles the dependency graph".into());
        }
        refresh.constraints = constraints;
    };
    let requested = target
        .version()
        .or_else(|| alias_version.as_ref().map(|(id, v)| (id, v)));
    let unused = |id: &Id| {
        refresh.choices.contains_key(id)
            && !rebound.members.values().any(|m| m.edges.contains_key(id))
    };
    let (selections, libraries) = select(
        &rebound,
        &store,
        refresh.libraries,
        requested.filter(|(id, _)| !unused(id)),
        &mut refresh.fetched,
        &mut refresh.registry,
        |id| target.includes(id, "", false),
    )?;
    let root = edits.last_mut().unwrap();
    let text = edit::selections(&root.after.bytes, &workspace.selections, &selections)?;
    root.replace(text);
    let overlay = overlay(&workspace, cwd, &edits)?;
    let trial = Resolution::resolve_cached(
        overlay,
        store.clone(),
        false,
        libraries,
        &mut refresh.fetched,
    )?;
    if let Target::Identity(name, major) = target
        && !trial
            .published
            .keys()
            .any(|id| id.name == name && major.is_none_or(|m| m == id.major))
        && refresh.rebound.is_empty()
    {
        return Err(format!("no dependency binds {name}").into());
    }
    let mut findings = refresh.registry.graph(&trial, None)?;
    crate::package_add::registry_select::require_active(&findings)?;
    let mut checked = crate::package_audit::check(&trial, None)?;
    if let Some(shared) = Manifest::parse(
        &edits.last().unwrap().after.bytes,
        &workspace.directory.join(MANIFEST),
    )?
    .workspace
    {
        for (alias, dep) in &shared.dependencies {
            let Dependency::Published(id, root, Some(SourceLocation::Registry(origin)), mask) = dep
            else {
                continue;
            };
            if target.includes(id, alias, inherited)
                && !trial.workspace.members.values().any(|m| {
                    matches!(
                        m.package.dependencies.get(alias),
                        Some(Dependency::Inherited)
                    )
                })
            {
                let (unused, authority) = refresh.registry.unbound(id, *root, origin, *mask)?;
                findings.extend(unused);
                checked.authority.extend(authority);
            }
        }
    }
    let old = workspace
        .with_manifests(cwd, &[], false)?
        .and_then(|before| Resolution::resolve(before, store.clone(), true).ok());
    let mut report = Report::new(
        &trial,
        &workspace.selections,
        before_lock.as_ref(),
        old.as_ref(),
        refresh.rebound,
        refresh.fetched,
    );
    report.registry = findings
        .into_iter()
        .map(|(id, f)| (id.to_string(), f))
        .collect();
    if report.has_changes() {
        let prior = old
            .as_ref()
            .filter(|old| report.matches_previous(old))
            .and_then(|old| crate::package_audit::check(old, None).ok());
        report.set_diffs(
            &prior.map_or_else(Default::default, |old| old.cards),
            &checked.cards,
        );
    }
    if !checked.authority.is_empty() {
        report.print(json, dry_run, "refused");
        return Err(checked.authority.join("\nhirsel: ").into());
    }
    if let Some(hint) = refresh.registry.young_fix {
        return Err(hint.into());
    }
    let result = if edits.iter().any(Edit::changed) {
        "updated"
    } else {
        "unchanged"
    };
    if dry_run {
        report.print(json, true, result);
    } else {
        if !json {
            report.print(false, false, result);
        }
        let lock = Lock::compare_captured(&trial, Mode::Update, before_lock.as_ref())?;
        if let Some(error) = transaction::commit(
            "update",
            cwd,
            &scope,
            &workspace,
            edits
                .iter()
                .filter(|e| e.changed())
                .map(|e| (&e.dir, &e.before, &e.after)),
            before_lock.as_ref(),
            &lock,
        )? {
            eprintln!("hirsel: {error}");
            if json {
                report.print(true, false, "partial");
            }
            return Ok(1);
        }
        if let Some((_, message)) = lock.pending() {
            eprintln!("hirsel: {message}");
        }
        if json {
            report.print(true, false, result);
        }
    }
    Ok(0)
}
