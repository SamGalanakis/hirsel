use super::{Failure, now, wire::Frames};
use crate::package_content::{ARCHIVE_LIMIT, Binding, Id, Library, PACKAGE_LIMIT, Root};
use crate::package_manifest::{SourceLocation, name::Version};
use crate::package_resolve::{Libraries, Resolution};
use crate::package_store::PackageStore;
use crate::package_workspace::{Edge, Workspace};
use crate::registry_route::Origin;
use crate::registry_verify::Release;
use std::collections::{BTreeMap, BTreeSet};

pub(super) struct Job {
    pub(super) id: Id,
    pub(super) version: Version,
    pub(super) root: Root,
    /// The head followed by the unlogged exact closure in digest order.
    pub(super) frames: Vec<Root>,
    /// Each of these needs a verified locator before this job may be sent.
    pub(super) logged: BTreeSet<Root>,
}

pub(super) fn order(workspace: &Workspace, all: bool) -> Result<Vec<Id>, Failure> {
    let mut reached = BTreeSet::new();
    let mut pending = if all {
        workspace.members.keys().collect()
    } else {
        vec![workspace.selected()?]
    };
    let mut remaining = BTreeMap::new();
    let mut dependents = BTreeMap::<_, Vec<_>>::new();
    while let Some(id) = pending.pop() {
        if !reached.insert(id) {
            continue;
        }
        let mut count = 0;
        for (dep, edge) in &workspace.members[id].edges {
            if *edge == Edge::Member {
                count += 1;
                dependents.entry(dep).or_default().push(id);
                pending.push(dep);
            }
        }
        remaining.insert(id, count);
    }
    let mut ready: BTreeSet<_> = remaining
        .iter()
        .filter_map(|(&id, &count)| (count == 0).then_some(id))
        .collect();
    let mut order = Vec::with_capacity(remaining.len());
    while let Some(id) = ready.pop_first() {
        order.push(id.clone());
        for next in dependents.remove(id).unwrap_or_default() {
            let count = remaining.get_mut(next).expect("visited dependent");
            *count -= 1;
            if *count == 0 {
                ready.insert(next);
            }
        }
    }
    if order.len() != reached.len() {
        return Err("workspace path bindings form a cycle".into());
    }
    if order.is_empty() {
        return Err("the workspace has no packages to publish".into());
    }
    Ok(order)
}

pub(super) fn prepare(
    resolution: &mut Resolution,
    order: Vec<Id>,
    all: bool,
) -> Result<(Vec<Job>, Frames, BTreeMap<Root, Release>), Failure> {
    let coalesced: Vec<_> = resolution
        .published
        .keys()
        .filter(|id| resolution.workspace.members.contains_key(*id))
        .cloned()
        .collect();
    for id in coalesced {
        let (_, archive) = resolution.make_archive(&id)?;
        resolution.libraries.extend(archive.entries);
    }
    let registry = registry_sources(resolution);
    let mut members = BTreeMap::<Id, Root>::new();
    let mut planned = Libraries::new();
    let mut jobs = Vec::new();
    let mut frames = Frames::new()?;
    let mut history = BTreeMap::new();
    let mut published = BTreeSet::new();
    let selected = resolution.workspace.member.as_ref();
    for id in order {
        let member = &resolution.workspace.members[&id];
        let mut content = member.content.clone();
        for (alias, child) in &member.aliases {
            let binding = match &member.edges[child] {
                Edge::Toolchain => Binding::Toolchain,
                Edge::Member => {
                    let root = members[child];
                    bind(root, &planned[&root].content.version, all)
                }
                Edge::Published(recorded, _) => {
                    let root = resolution
                        .published
                        .get(child)
                        .copied()
                        .unwrap_or(*recorded);
                    let library = resolution.libraries.get(&root).ok_or_else(|| {
                        format!("{id}: published binding to {child} {root} uses a workspace member; use a path binding to publish it with the workspace")
                    })?;
                    let origin = registry.get(&root).copied();
                    if origin.is_some() {
                        authenticate(
                            &mut history,
                            &resolution.store,
                            child,
                            &library.content.version,
                            root,
                            origin,
                        )?;
                    }
                    bind(root, &library.content.version, origin.is_some())
                }
            };
            content
                .bindings
                .insert(alias.clone(), (child.clone(), binding));
        }
        let (root, bytes) = content.inventory()?.encode()?;
        let version = content.version.clone();
        members.insert(id.clone(), root);
        planned.insert(
            root,
            Library {
                content,
                encoded: bytes.into(),
            },
        );
        if all || selected == Some(&id) {
            let mut job = Job {
                id,
                version,
                root,
                frames: vec![root],
                logged: BTreeSet::new(),
            };
            let mut graph = PublicationGraph {
                resolution,
                planned: &planned,
                registry: &registry,
                published: &published,
                history: &mut history,
            };
            graph.stage(&mut job, &mut frames)?;
            jobs.push(job);
            published.insert(root);
        }
    }
    // Drop the preflight compression buffer before sending any request.
    frames.finish_staging();
    Ok((jobs, frames, history))
}

fn bind(root: Root, version: &Version, logged: bool) -> Binding {
    if logged && version.pre.is_empty() {
        Binding::Floor {
            tested: root,
            minor: version.minor,
            patch: version.patch,
        }
    } else {
        Binding::Exact(root)
    }
}

fn registry_sources(resolution: &Resolution) -> BTreeMap<Root, &Origin> {
    let active = |id, root| resolution.published.get(id) == Some(root);
    let mut sources = BTreeMap::new();
    for member in resolution.workspace.members.values() {
        for (id, edge) in &member.edges {
            if let Edge::Published(root, Some(SourceLocation::Registry(origin))) = edge
                && active(id, root)
            {
                sources.insert(*root, origin);
            }
        }
    }
    for (id, (root, source)) in &resolution.workspace.selections {
        if let Some(SourceLocation::Registry(origin)) = source
            && active(id, root)
        {
            sources.insert(*root, origin);
        }
    }
    sources
}

struct PublicationGraph<'a> {
    resolution: &'a Resolution,
    planned: &'a Libraries,
    registry: &'a BTreeMap<Root, &'a Origin>,
    // Earlier jobs in dependency order. Their locators only become available after submit.
    published: &'a BTreeSet<Root>,
    history: &'a mut BTreeMap<Root, Release>,
}

impl PublicationGraph<'_> {
    fn library(&self, root: Root) -> Option<&Library> {
        self.planned
            .get(&root)
            .or_else(|| self.resolution.libraries.get(&root))
    }

    fn stage(&mut self, job: &mut Job, frames: &mut Frames) -> Result<(), Failure> {
        let mut pending = vec![job.root];
        let mut closure = BTreeSet::new();
        let mut history_required = BTreeSet::new();
        let mut identities = BTreeMap::new();
        let mut expanded = 0u64;
        while let Some(root) = pending.pop() {
            if !closure.insert(root) {
                continue;
            }
            let library = self.library(root).expect("known closure entry");
            if identities
                .insert(&library.content.id, root)
                .is_some_and(|prior| prior != root)
            {
                return Err(format!(
                    "{}: {} appears at two roots in the publication closure",
                    job.id, library.content.id
                )
                .into());
            }
            expanded += library.encoded.len() as u64;
            if expanded > ARCHIVE_LIMIT as u64 || closure.len() > PACKAGE_LIMIT {
                return Err(format!("{}: closure exceeds the registry check limit", job.id).into());
            }
            for (id, binding) in library.content.bindings.values() {
                let Some(child) = binding.root() else {
                    continue;
                };
                let dependency = self.library(child).ok_or_else(|| {
                    format!("{} binds {id} {child}, but its tested content is absent from the checked closure", library.content.id)
                })?;
                if !self.planned.contains_key(&child)
                    && self.resolution.published.get(id) != Some(&child)
                {
                    return Err(format!(
                        "{} binds {id} {child}, which differs from the checked workspace selection",
                        library.content.id
                    )
                    .into());
                }
                crate::package_content::check_identity(id, child, &dependency.content.id)?;
                if let Binding::Floor { minor, patch, .. } = binding {
                    let v = &dependency.content.version;
                    if !v.pre.is_empty() || (v.minor, v.patch) != (*minor, *patch) {
                        return Err(format!(
                            "{id} {child}: floor version differs from tested content"
                        )
                        .into());
                    }
                    history_required.insert(child);
                }
                pending.push(child);
            }
        }
        frames.stage(job.root, &self.planned[&job.root].encoded)?;
        for root in closure.into_iter().filter(|root| *root != job.root) {
            let pin = self.registry.get(&root).copied();
            if self.published.contains(&root) {
                job.logged.insert(root);
            } else if pin.is_some() || history_required.contains(&root) {
                if !self.history.contains_key(&root) {
                    let content = &self.library(root).expect("known closure entry").content;
                    let release = resolve_release(
                        &self.resolution.store,
                        &content.id,
                        &content.version,
                        root,
                        pin,
                    )?;
                    self.history.insert(root, release);
                }
                job.logged.insert(root);
            } else {
                frames.stage(
                    root,
                    &self.library(root).expect("known closure entry").encoded,
                )?;
                job.frames.push(root);
            }
        }
        frames.check_total(&job.frames)
    }
}

fn resolve_release(
    store: &PackageStore,
    id: &Id,
    version: &Version,
    root: Root,
    pin: Option<&Origin>,
) -> Result<Release, Failure> {
    let client = store.registry(&id.name, pin)?;
    let head = now()
        .and_then(|time| client.release_checkpoint(time))
        .map_err(|error| -> Failure { error })?;
    let (_, release) = client
        .verified_hints(&head, &id.name)
        .map_err(|error| -> Failure { error })?
        .into_iter()
        .find(|(_, release)| release.version() == version)
        .ok_or_else(|| {
            format!(
                "{id} {version} {root}: no authenticated release locator at {}",
                client.route().origin()
            )
        })?;
    if release.root() != root {
        return Err(format!(
            "{id} {version}: authenticated root {} differs from {root}",
            release.root()
        )
        .into());
    }
    Ok(release)
}

fn authenticate(
    history: &mut BTreeMap<Root, Release>,
    store: &PackageStore,
    id: &Id,
    version: &Version,
    root: Root,
    pin: Option<&Origin>,
) -> Result<(), Failure> {
    if let std::collections::btree_map::Entry::Vacant(entry) = history.entry(root) {
        entry.insert(resolve_release(store, id, version, root, pin)?);
    }
    Ok(())
}
