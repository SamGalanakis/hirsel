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
mod transaction;
use report::Report;
use transaction::Edit;

type Failure = Box<dyn std::error::Error>;

enum Target<'a> {
    All,
    Alias(&'a str),
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
        if version.is_none() && name::alias(text) {
            return Ok(Self::Alias(text));
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
            Self::Alias(target) => scope && *target == alias,
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
    workspace: &'a Workspace,
    store: &'a PackageStore,
    fetched: BTreeSet<String>,
    libraries: Libraries,
    rebound: Vec<Rebound>,
    captured: BTreeMap<SourceLocation, (Id, Root, SourceLocation)>,
}

impl Refresh<'_> {
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
            let requested = match &source {
                SourceLocation::Archive(path) => Source::Archive(path.clone()),
                SourceLocation::Url(url) => Source::Url(url.clone()),
                SourceLocation::Registry(_) => continue,
                SourceLocation::Git(git) => {
                    let Some(reference) = &git.reference else {
                        continue;
                    };
                    Source::Git(git::Request {
                        url: git.url.clone(),
                        dir: git.dir.clone(),
                        reference: Some(reference.clone()),
                    })
                }
            };

            if !self.captured.contains_key(&source) {
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
            let (actual, root, fresh) = &self.captured[&source];
            crate::package_content::check_identity(id, *root, actual)?;
            if root == before && fresh == &source {
                continue;
            }
            let declared = match dependency {
                Dependency::Published(_, _, Some(SourceLocation::Archive(path)), _) => {
                    SourceLocation::Archive(path.clone())
                }
                _ => fresh.clone(),
            };
            let form = edit::Form::Published(*root, Some(&declared));
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
    let mut root = Edit::new(&workspace, scope.try_clone()?, PathBuf::new())?;
    let config = Manifest::parse(&root.before.bytes, &workspace.directory.join(MANIFEST))?;
    let selected = workspace.member.as_ref().map(|id| &workspace.members[id]);
    let mut inherited = selected.is_none();
    if let Target::Alias(alias) = target {
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
        workspace: &workspace,
        store: &store,
        fetched: BTreeSet::new(),
        libraries: Libraries::new(),
        rebound: Vec::new(),
        captured: BTreeMap::new(),
    };
    if let Some(shared) = config.workspace {
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
    let (selections, libraries) = select(
        &rebound,
        &store,
        refresh.libraries,
        target.version(),
        &mut refresh.fetched,
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
    let checked = crate::package_audit::check(&trial, None)?;
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
        if let Some(error) =
            transaction::commit(cwd, &scope, &workspace, &edits, before_lock.as_ref(), lock)?
        {
            eprintln!("hirsel: {error}");
            if json {
                report.print(true, false, "partial");
            }
            return Ok(1);
        }
        if json {
            report.print(true, false, result);
        }
    }
    Ok(0)
}
