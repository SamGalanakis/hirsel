use crate::package_content::{Id, Library, PackageArchive, Root, check_identity};
use crate::package_manifest::SourceLocation;
use crate::package_store::{Entry, PackageStore};
use crate::package_workspace::{Edge, Member, Workspace};
use bytes::Bytes;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

type Failure = Box<dyn std::error::Error>;
pub(crate) type Libraries = BTreeMap<Root, Library>;
type Declarations = BTreeMap<Root, (Id, SourceLocation)>;
pub(crate) type Members = BTreeMap<Id, Member>;
type Encoded = BTreeMap<Id, (Root, Library)>;

/// Fetches the bundle a declared source holds for `root`, verified against it and kept
/// wherever the caller keeps it.
pub(crate) type Fetch<'a> =
    dyn FnMut(&Id, Root, &SourceLocation) -> Result<Libraries, Failure> + 'a;

pub(crate) struct Resolution {
    pub(crate) workspace: Workspace,
    pub(crate) libraries: Libraries,
    /// The toolchain packages other than std that the workspace's closure binds.
    pub(crate) toolchain: BTreeSet<Id>,
    pub(crate) published: BTreeMap<Id, Root>,
    pub(crate) store: PackageStore,
    declarations: Declarations,
    encoded: BTreeMap<Id, (Root, Library)>,
}

#[derive(Clone, Eq, Ord, PartialEq, PartialOrd)]
enum Node {
    Member(Id),
    Published(Id, Root),
}

impl Node {
    fn id(&self) -> &Id {
        match self {
            Self::Member(id) | Self::Published(id, _) => id,
        }
    }
}

type Paths = BTreeMap<Node, Vec<Id>>;

fn path_order(left: &[Id], right: &[Id]) -> std::cmp::Ordering {
    left.len().cmp(&right.len()).then_with(|| left.cmp(right))
}

fn path_text(path: &[Id]) -> String {
    path.iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" -> ")
}

fn best_paths(members: &Members, libraries: &Libraries) -> Paths {
    let bound: BTreeSet<_> = members
        .values()
        .flat_map(|member| {
            member
                .edges
                .iter()
                .filter_map(|(id, edge)| matches!(edge, Edge::Member).then_some(id))
        })
        .collect();
    let mut queue: BTreeSet<_> = members
        .keys()
        .filter(|id| !bound.contains(*id))
        .map(|id| (1, vec![id.clone()], Node::Member(id.clone())))
        .collect();
    let mut paths = BTreeMap::new();
    while let Some((_, path, node)) = queue.pop_first() {
        if paths.contains_key(&node) {
            continue;
        }
        let outgoing: Vec<_> =
            match &node {
                Node::Member(id) => members[id]
                    .edges
                    .iter()
                    .filter_map(|(id, edge)| match edge {
                        Edge::Toolchain => None,
                        Edge::Member => Some(Node::Member(id.clone())),
                        Edge::Published(root, _) => Some(Node::Published(id.clone(), *root)),
                    })
                    .collect(),
                Node::Published(_, root) => libraries
                    .get(root)
                    .into_iter()
                    .flat_map(|library| {
                        library.content.bindings.iter().filter_map(|(id, root)| {
                            root.map(|root| Node::Published(id.clone(), root))
                        })
                    })
                    .collect(),
            };
        for child in outgoing {
            if !paths.contains_key(&child) {
                let mut child_path = path.clone();
                child_path.push(child.id().clone());
                queue.insert((child_path.len(), child_path, child));
            }
        }
        paths.insert(node, path);
    }
    paths
}

fn incoming(members: &Members, libraries: &Libraries) -> BTreeSet<(Id, Root)> {
    let direct = members.values().flat_map(|member| {
        member
            .edges
            .iter()
            .filter_map(|(id, binding)| match binding {
                Edge::Published(root, _) => Some((id.clone(), *root)),
                _ => None,
            })
    });
    let transitive = libraries.values().flat_map(|library| {
        library
            .content
            .bindings
            .iter()
            .filter_map(|(id, root)| root.map(|root| (id.clone(), root)))
    });
    direct.chain(transitive).collect()
}

fn via(libraries: &Libraries, root: Root) -> Option<&Id> {
    libraries
        .values()
        .filter(|library| {
            library
                .content
                .bindings
                .values()
                .any(|bound| *bound == Some(root))
        })
        .map(|library| &library.content.id)
        .min()
}

fn source(declared: &Declarations, libraries: &Libraries, root: Root) -> String {
    if let Some((_, source)) = declared.get(&root) {
        source.to_string()
    } else if let Some(id) = via(libraries, root) {
        format!("via:{id}")
    } else {
        "store".into()
    }
}

fn carrier(
    root: Root,
    libraries: &Libraries,
    declared: &Declarations,
    paths: &Paths,
) -> Option<Root> {
    let mut visited = BTreeSet::from([root]);
    let mut frontier = BTreeSet::from([root]);
    while !frontier.is_empty() {
        let binders: BTreeSet<_> = libraries
            .iter()
            .filter(|(root, library)| {
                !visited.contains(root)
                    && library
                        .content
                        .bindings
                        .values()
                        .any(|child| child.is_some_and(|child| frontier.contains(&child)))
            })
            .map(|(root, _)| *root)
            .collect();
        if let Some(nearest) = binders
            .iter()
            .filter(|root| declared.contains_key(root))
            .min_by(|a, b| {
                let path =
                    |root| &paths[&Node::Published(libraries[&root].content.id.clone(), root)];
                path_order(path(**a), path(**b)).then_with(|| a.cmp(b))
            })
        {
            return Some(*nearest);
        }
        visited.extend(binders.iter().copied());
        frontier = binders;
    }
    None
}

/// Resolution step 3 over a member graph: every published root the members reach, from
/// `cached`, then the store, then its declared source, then its nearest carrier's.
pub(crate) fn acquire(
    members: &Members,
    store: &PackageStore,
    frozen: bool,
    mut cached: Libraries,
    fetch: &mut Fetch<'_>,
) -> Result<(Libraries, Declarations), Failure> {
    let mut declared = BTreeMap::new();
    for member in members.values() {
        for (id, edge) in &member.edges {
            if let Edge::Published(root, Some(source)) = edge {
                declared
                    .entry(*root)
                    .or_insert_with(|| (id.clone(), source.clone()));
            }
        }
    }
    let mut libraries = BTreeMap::new();
    loop {
        let mut pending = Vec::new();
        let mut seen = BTreeSet::new();
        let mut opened = false;
        for (id, root) in incoming(members, &libraries) {
            if let Some(library) = libraries.get(&root) {
                check_identity(&id, root, &library.content.id)?;
                continue;
            }
            if !seen.insert(root) {
                continue;
            }
            let entry = match cached.remove(&root) {
                Some(library) => Entry::Present(library),
                None => store.read(root)?,
            };
            match entry {
                Entry::Present(library) => {
                    check_identity(&id, root, &library.content.id)?;
                    libraries.insert(root, library);
                    opened = true;
                }
                Entry::Missing => pending.push((id, root, "is not in")),
                Entry::Damaged => pending.push((id, root, "is damaged in")),
            }
        }
        if frozen {
            if let Some((id, root, status)) = pending.first() {
                return Err(format!("{id} {root} {status} the package store, and --frozen does not fetch (source {})",
                    source(&declared, &libraries, *root)).into());
            }
            if opened {
                continue;
            }
            break;
        }

        let mut acquired = opened;
        for (id, root, _) in &pending {
            if !cached.contains_key(root)
                && let Some((binder, source)) = declared.get(root)
            {
                cached.extend(fetch(binder, *root, source)?);
            }
            if let Some(library) = cached.remove(root) {
                check_identity(id, *root, &library.content.id)?;
                libraries.insert(*root, library);
                acquired = true;
            }
        }
        if acquired {
            continue;
        }
        let Some((missing, root, _)) = pending.first() else {
            break;
        };
        let paths = best_paths(members, &libraries);
        let mut found_carrier = false;
        for (_, root, _) in &pending {
            if !cached.contains_key(root)
                && let Some(nearest) = carrier(*root, &libraries, &declared, &paths)
            {
                let (binder, source) = &declared[&nearest];
                cached.extend(fetch(binder, nearest, source)?);
                found_carrier = true;
            }
        }
        if !found_carrier {
            return Err(format!(
                "{missing} {root} is not in the package store and its binding names no source"
            )
            .into());
        }
    }
    Ok((libraries, declared))
}

/// What a resolved member graph binds: the toolchain closure and one root per published id,
/// refusing a toolchain clash, a diamond and a member bound to a root that is not its own.
fn settle(
    members: &Members,
    libraries: &Libraries,
    encoded: &mut Encoded,
) -> Result<(BTreeSet<Id>, BTreeMap<Id, Root>), Failure> {
    let paths = best_paths(members, libraries);
    let bound = members
        .values()
        .flat_map(|member| {
            member
                .edges
                .iter()
                .filter_map(|(id, edge)| matches!(edge, Edge::Toolchain).then_some(id))
        })
        .chain(libraries.values().flat_map(|library| {
            library
                .content
                .bindings
                .iter()
                .filter_map(|(id, root)| root.is_none().then_some(id))
        }));
    let toolchain = crate::package_content::toolchain_closure(bound);
    let incoming = incoming(members, libraries);
    if let Some(id) = toolchain
        .iter()
        .find(|id| members.contains_key(id) || incoming.iter().any(|(bound, _)| bound == *id))
    {
        return Err(format!(
            "{id} is bound both to the toolchain and to a member or published library"
        )
        .into());
    }
    let mut all = BTreeMap::<Id, BTreeSet<Root>>::new();
    for (id, root) in incoming {
        all.entry(id).or_default().insert(root);
    }
    let mut published = BTreeMap::new();
    for (id, roots) in all {
        if roots.len() > 1 {
            let mut found: Vec<_> = roots
                .iter()
                .map(|root| (&paths[&Node::Published(id.clone(), *root)], root))
                .collect();
            found.sort_by(|(a, _), (b, _)| path_order(a, b));
            let mut refusal = format!("{id} is bound to more than one root:");
            for (path, root) in found {
                refusal.push_str(&format!("\n  {} = {root}", path_text(path)));
            }
            return Err(refusal.into());
        }
        let root = *roots.first().unwrap();
        if members.contains_key(&id) && member_root(members, encoded, &id)? != root {
            let member_path = &paths[&Node::Member(id.clone())];
            let root_path = &paths[&Node::Published(id.clone(), root)];
            let mut rows = [
                (member_path, "member".into()),
                (root_path, root.to_string()),
            ];
            rows.sort_by(|(a, _), (b, _)| path_order(a, b));
            return Err(format!("{id} is a member and is also bound to {root}, which is not the member's root:\n  {} = {}\n  {} = {}",
                path_text(rows[0].0), rows[0].1, path_text(rows[1].0), rows[1].1).into());
        }
        published.insert(id, root);
    }
    Ok((toolchain, published))
}

/// A member's root by section 6: its member bindings encoded as their roots, after them.
fn member_root(members: &Members, encoded: &mut Encoded, id: &Id) -> Result<Root, String> {
    let mut pending = vec![(id, false)];
    while let Some((current, ready)) = pending.pop() {
        if encoded.contains_key(current) {
            continue;
        }
        let member = &members[current];
        if !ready {
            pending.push((current, true));
            for (dep, edge) in &member.edges {
                if matches!(edge, Edge::Member) {
                    pending.push((dep, false));
                }
            }
            continue;
        }
        member.check_pins()?;
        let mut content = member.content.clone();
        content.bindings = member
            .edges
            .iter()
            .map(|(id, edge)| {
                let root = match edge {
                    Edge::Toolchain => None,
                    Edge::Member => Some(encoded[id].0),
                    Edge::Published(pin, _) => Some(*pin),
                };
                (id.clone(), root)
            })
            .collect();
        let (root, bytes) = content.inventory()?.encode()?;
        encoded.insert(
            current.clone(),
            (
                root,
                Library {
                    content,
                    encoded: bytes.into(),
                },
            ),
        );
    }
    Ok(encoded[id].0)
}

/// The archive `hirsel pack` writes for member `id`: its content and every package it
/// reaches, decoded again so the decoder checks the packer.
fn assemble(
    members: &Members,
    encoded: &mut Encoded,
    libraries: &Libraries,
    id: &Id,
) -> Result<(Bytes, PackageArchive), String> {
    let head = member_root(members, encoded, id)?;
    let member_content: BTreeMap<_, _> = encoded
        .values()
        .map(|(root, library)| (*root, library))
        .collect();
    let mut pending = VecDeque::from([head]);
    let mut reached = BTreeMap::new();
    while let Some(root) = pending.pop_front() {
        if reached.contains_key(&root) {
            continue;
        }
        let content = member_content
            .get(&root)
            .copied()
            .unwrap_or_else(|| &libraries[&root]);
        pending.extend(content.content.bindings.values().flatten().copied());
        reached.insert(
            root,
            Library {
                content: content.content.clone(),
                encoded: content.encoded.clone(),
            },
        );
    }
    let bytes = Bytes::from(PackageArchive::encode(head, &reached)?);
    let decoded = PackageArchive::decode(bytes.clone())?;
    Ok((bytes, decoded))
}

/// Packs member `id` of a member graph read from a commit (GIT-2 steps 5 and 6): acquires
/// what it reaches through `fetch`, checks the graph as resolution does, and returns the
/// archive bytes and the decoded bundle, verified by the caller.
pub(crate) fn pack_members(
    members: &Members,
    store: &PackageStore,
    cached: Libraries,
    fetch: &mut Fetch<'_>,
    id: &Id,
) -> Result<(Bytes, PackageArchive), Failure> {
    let (libraries, _) = acquire(members, store, false, cached, fetch)?;
    let mut encoded = BTreeMap::new();
    settle(members, &libraries, &mut encoded)?;
    Ok(assemble(members, &mut encoded, &libraries, id)?)
}

impl Resolution {
    pub(crate) fn resolve(
        workspace: Workspace,
        store: PackageStore,
        frozen: bool,
    ) -> Result<Self, Failure> {
        Self::resolve_cached(
            workspace,
            store,
            frozen,
            BTreeMap::new(),
            &mut BTreeSet::new(),
        )
    }

    pub(crate) fn resolve_cached(
        mut workspace: Workspace,
        store: PackageStore,
        frozen: bool,
        cached: Libraries,
        fetched: &mut BTreeSet<String>,
    ) -> Result<Self, Failure> {
        let mut inputs = Vec::new();
        let (libraries, declarations) = {
            let directory = &workspace.root;
            let mut fetch = |id: &Id, root: Root, source: &SourceLocation| {
                let (archive, identity) = store.fetch(directory, id, root, source, fetched)?;
                inputs.extend(identity);
                Ok(archive.entries)
            };
            acquire(&workspace.members, &store, frozen, cached, &mut fetch)?
        };
        for identity in inputs {
            workspace.record_input(identity);
        }
        let mut encoded = BTreeMap::new();
        let (toolchain, published) = settle(&workspace.members, &libraries, &mut encoded)?;
        Ok(Self {
            workspace,
            libraries,
            toolchain,
            store,
            declarations,
            published,
            encoded,
        })
    }

    pub(crate) fn source(&self, root: Root) -> String {
        source(&self.declarations, &self.libraries, root)
    }

    #[cfg(test)]
    pub(crate) fn member_root(&mut self, id: &Id) -> Result<Root, String> {
        member_root(&self.workspace.members, &mut self.encoded, id)
    }

    pub(crate) fn make_archive(&mut self, id: &Id) -> Result<(Bytes, PackageArchive), String> {
        let (bytes, decoded) = assemble(
            &self.workspace.members,
            &mut self.encoded,
            &self.libraries,
            id,
        )?;
        decoded.verify(id, decoded.head, "pack output")?;
        Ok((bytes, decoded))
    }
}
