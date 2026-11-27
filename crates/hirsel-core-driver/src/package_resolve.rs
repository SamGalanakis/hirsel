use crate::package_content::{Binding, Id, Library, PackageArchive, Root, check_identity};
use crate::package_manifest::{Selections, SourceLocation, name::Version};
use crate::package_store::{Entry, PackageStore};
use crate::package_workspace::{Edge, Member, Workspace};
use crate::registry_route::Origin;
use bytes::Bytes;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;

type Failure = Box<dyn std::error::Error>;
pub(crate) type Libraries = BTreeMap<Root, Library>;
type Declarations = BTreeMap<Root, (Id, SourceLocation)>;
pub(crate) type Members = BTreeMap<Id, Member>;
type Encoded = BTreeMap<Id, (Root, Library)>;

pub(crate) struct Frame<'a> {
    pub(crate) id: &'a Id,
    pub(crate) root: Root,
    pub(crate) carrier: &'a Id,
    pub(crate) origin: &'a Origin,
    /// The nearest logged floor on the path from the carrier, if any.
    pub(crate) floor: Option<&'a Id>,
}

pub(crate) enum FetchRequest<'a> {
    Bundle(&'a Id, Root, &'a SourceLocation),
    Frames(&'a [Frame<'a>]),
}

pub(crate) type Fetch<'a> = dyn FnMut(FetchRequest<'_>) -> Result<Libraries, Failure> + 'a;

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

struct Graph<'a> {
    members: &'a Members,
    selections: &'a Selections,
    manifest: &'a Path,
}

impl Graph<'_> {
    fn destination(&self, id: &Id, recorded: Root) -> Node {
        if self.members.contains_key(id) {
            Node::Member(id.clone())
        } else {
            let root = self.selections.get(id).map_or(recorded, |(root, _)| *root);
            Node::Published(id.clone(), root)
        }
    }

    fn children(&self, node: &Node, libraries: &Libraries) -> Vec<Node> {
        match node {
            Node::Member(id) => self.members[id]
                .edges
                .iter()
                .filter_map(|(id, edge)| match edge {
                    Edge::Toolchain => None,
                    Edge::Member => Some(Node::Member(id.clone())),
                    Edge::Published(root, _) => Some(self.destination(id, *root)),
                })
                .collect(),
            Node::Published(_, root) => libraries
                .get(root)
                .into_iter()
                .flat_map(|library| {
                    library
                        .content
                        .edges()
                        .into_iter()
                        .filter_map(|(id, root)| root.map(|root| self.destination(&id, root)))
                })
                .collect(),
        }
    }

    fn closure(&self, head: &Id, libraries: &Libraries) -> BTreeSet<Node> {
        let mut reached = BTreeSet::new();
        let mut pending = vec![Node::Member(head.clone())];
        while let Some(node) = pending.pop() {
            if reached.insert(node.clone()) {
                pending.extend(self.children(&node, libraries));
            }
        }
        reached
    }
}

fn best_paths(graph: &Graph, libraries: &Libraries) -> Paths {
    let members = graph.members;
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
        for child in graph.children(&node, libraries) {
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
            .values()
            .filter_map(|(id, binding)| binding.root().map(|root| (id.clone(), root)))
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
                .any(|(_, binding)| binding.root() == Some(root))
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

fn carrier<'a>(
    root: Root,
    graph: &Graph<'_>,
    libraries: &'a Libraries,
    declared: &Declarations,
    paths: &Paths,
) -> Option<(Root, Option<&'a Id>)> {
    let mut visited = BTreeSet::from([root]);
    let mut frontier = BTreeMap::from([(root, None)]);
    while !frontier.is_empty() {
        let mut binders = BTreeMap::new();
        for (root, library) in libraries {
            if !visited.insert(*root) {
                continue;
            }
            if let Some(floor) = library.content.bindings.values().find_map(|(id, binding)| {
                let (recorded, is_floor) = match binding {
                    Binding::Toolchain => return None,
                    Binding::Exact(root) => (*root, false),
                    Binding::Floor { tested, .. } => (*tested, true),
                };
                let Node::Published(_, child) = graph.destination(id, recorded) else {
                    return None;
                };
                frontier
                    .get(&child)
                    .map(|floor: &Option<&Id>| floor.or(is_floor.then_some(id)))
            }) {
                binders.insert(*root, floor);
            } else {
                visited.remove(root);
            }
        }
        if let Some((&nearest, &floor)) = binders
            .iter()
            .filter(|(root, _)| declared.contains_key(root))
            .min_by(|(a, _), (b, _)| {
                let path =
                    |root| &paths[&Node::Published(libraries[&root].content.id.clone(), root)];
                path_order(path(**a), path(**b)).then_with(|| a.cmp(b))
            })
        {
            return Some((nearest, floor));
        }
        frontier = binders;
    }
    None
}

fn fill_sources<'a>(
    pending: &[(Id, Root, &str)],
    cached: &mut Libraries,
    fetch: &mut Fetch<'_>,
    mut locate: impl FnMut(Root) -> Option<(&'a Id, Root, &'a SourceLocation, Option<&'a Id>)>,
) -> Result<(), Failure> {
    let mut frames = Vec::new();
    for (id, root, _) in pending {
        if cached.contains_key(root) {
            continue;
        }
        let Some((binder, carrier, source, floor)) = locate(*root) else {
            continue;
        };
        if let SourceLocation::Registry(origin) = source {
            frames.push(Frame {
                id,
                root: *root,
                carrier: binder,
                origin,
                floor,
            });
        } else {
            if !frames.is_empty() {
                cached.extend(fetch(FetchRequest::Frames(&frames))?);
                frames.clear();
            }
            if !cached.contains_key(root) {
                cached.extend(fetch(FetchRequest::Bundle(binder, carrier, source))?);
            }
        }
    }
    if !frames.is_empty() {
        cached.extend(fetch(FetchRequest::Frames(&frames))?);
    }
    Ok(())
}

/// Resolution step 3 over a member graph: every published root the members reach, from
/// `cached`, then the store, then its declared source, then its nearest carrier's.
fn acquire(
    graph: &Graph,
    store: &PackageStore,
    frozen: bool,
    mut cached: Libraries,
    fetch: &mut Fetch<'_>,
) -> Result<(Libraries, Declarations), Failure> {
    let mut declared = BTreeMap::new();
    for member in graph.members.values() {
        for (id, edge) in &member.edges {
            if let Edge::Published(root, Some(source)) = edge
                && !graph.members.contains_key(id)
                && graph
                    .selections
                    .get(id)
                    .is_none_or(|(selected, _)| selected == root)
            {
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
        let reached: BTreeSet<_> = incoming(graph.members, &libraries)
            .into_iter()
            .map(|(id, root)| graph.destination(&id, root))
            .collect();
        let reached: Vec<_> = reached.into_iter().collect();
        for nodes in reached.chunks(32) {
            let roots: Vec<_> = nodes
                .iter()
                .filter_map(|node| match node {
                    Node::Published(_, root)
                        if !libraries.contains_key(root)
                            && !cached.contains_key(root)
                            && seen.insert(*root) =>
                    {
                        Some(*root)
                    }
                    _ => None,
                })
                .collect();
            let mut entries = store.read_batch(&roots);
            for node in nodes.iter().cloned() {
                let Node::Published(id, root) = node else {
                    continue;
                };
                if let Some((_, Some(source))) = graph.selections.get(&id) {
                    declared
                        .entry(root)
                        .or_insert_with(|| (id.clone(), source.clone()));
                }
                if let Some(library) = libraries.get(&root) {
                    check_identity(&id, root, &library.content.id)?;
                    continue;
                }
                let entry = match cached.remove(&root) {
                    Some(library) => Entry::Present(library),
                    None => match entries.remove(&root) {
                        Some(entry) => entry?,
                        None => continue,
                    },
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

        fill_sources(&pending, &mut cached, fetch, |root| {
            declared
                .get(&root)
                .map(|(id, source)| (id, root, source, None))
        })?;
        let mut acquired = opened;
        for (id, root, _) in &pending {
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
        let paths = best_paths(graph, &libraries);
        fill_sources(&pending, &mut cached, fetch, |root| {
            let (nearest, floor) = carrier(root, graph, &libraries, &declared, &paths)?;
            let (binder, source) = &declared[&nearest];
            Some((binder, nearest, source, floor))
        })?;
        if !pending.iter().any(|(_, root, _)| cached.contains_key(root)) {
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
    graph: &Graph,
    libraries: &Libraries,
    encoded: &mut Encoded,
    packing: Option<&Id>,
) -> Result<(BTreeSet<Id>, BTreeMap<Id, Root>), Failure> {
    let members = graph.members;
    let paths = best_paths(graph, libraries);
    let packed = packing.map(|id| graph.closure(id, libraries));
    let bound: Vec<Id> = members
        .values()
        .flat_map(|member| {
            member
                .edges
                .iter()
                .filter_map(|(id, edge)| matches!(edge, Edge::Toolchain).then(|| id.clone()))
        })
        .chain(libraries.values().flat_map(|library| {
            library
                .content
                .bindings
                .values()
                .filter_map(|(id, binding)| binding.root().is_none().then(|| id.clone()))
        }))
        .collect();
    let toolchain = crate::package_content::toolchain_closure(&bound);
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
    let edges = requirements(members, libraries, &paths, packed.as_ref());
    let mut published = BTreeMap::new();
    for (id, requirements) in edges {
        let chosen = if let Some(member) = members.get(&id) {
            let dir = member.directory.to_str().unwrap();
            Some(Choice {
                root: member_root(members, encoded, &id)?,
                version: &member.content.version,
                by: format!("the member {}", if dir.is_empty() { "." } else { dir }),
                path: &paths[&Node::Member(id.clone())],
            })
        } else {
            graph.selections.get(&id).map(|(root, _)| Choice {
                root: *root,
                version: &libraries[root].content.version,
                by: format!("selected in {}", graph.manifest.display()),
                path: &requirements
                    .iter()
                    .find(|e| e.root == *root)
                    .unwrap_or(&requirements[0])
                    .path,
            })
        };
        let root = bind(&id, &requirements, libraries, chosen)?;
        published.insert(id, root);
    }
    for (id, (root, _)) in graph.selections {
        if !published.contains_key(id) {
            eprintln!(
                "hirsel: {}: selections.{id} selects {root}, which no edge of the closure names; hirsel update removes it",
                graph.manifest.display()
            );
        }
    }
    Ok((toolchain, published))
}

fn requirements(
    members: &Members,
    libraries: &Libraries,
    paths: &Paths,
    packed: Option<&BTreeSet<Node>>,
) -> BTreeMap<Id, Vec<Requirement>> {
    let mut edges = BTreeMap::<Id, Vec<Requirement>>::new();
    for (node, path) in paths {
        let bindings: BTreeMap<_, _> = match node {
            Node::Member(id) => members[id]
                .edges
                .iter()
                .filter_map(|(id, edge)| match edge {
                    Edge::Published(root, _) => Some((id, Binding::Exact(*root))),
                    _ => None,
                })
                .collect(),
            Node::Published(_, root) => libraries[root]
                .content
                .bindings
                .values()
                .map(|(id, binding)| (id, *binding))
                .collect(),
        };
        for (id, binding) in bindings {
            if let Some(root) = binding.root() {
                let mut path = path.clone();
                path.push(id.clone());
                edges.entry(id.clone()).or_default().push(Requirement {
                    path,
                    binding,
                    root,
                    packed: packed.is_some_and(|closure| closure.contains(node)),
                });
            }
        }
    }
    for requirements in edges.values_mut() {
        requirements.sort_by(|a, b| path_order(&a.path, &b.path));
    }
    edges
}

mod selection;
pub(crate) use selection::select;

struct Requirement {
    path: Vec<Id>,
    binding: Binding,
    root: Root,
    packed: bool,
}

impl Requirement {
    fn floor(&self, major: u32) -> Option<Version> {
        match self.binding {
            Binding::Floor { minor, patch, .. } => Some(Version {
                major,
                minor,
                patch,
                pre: String::new(),
            }),
            _ => None,
        }
    }

    fn accepts(&self, root: Root, version: &Version) -> bool {
        root == self.root
            || self.floor(version.major).is_some_and(|bound| {
                version.pre.is_empty()
                    && version >= &bound
                    && (version.major != 0 || version.minor == bound.minor)
            })
    }
}

struct Choice<'a> {
    root: Root,
    version: &'a Version,
    by: String,
    path: &'a [Id],
}

fn check_versions(
    id: &Id,
    edges: &[Requirement],
    libraries: &Libraries,
    choice: Option<&Choice>,
) -> Result<(), Failure> {
    let mut claims = Vec::new();
    if let Some(choice) = choice {
        claims.push((choice.path, choice.root, choice.version.clone()));
    }
    for edge in edges {
        if let Some(bound) = edge.floor(id.major) {
            claims.push((edge.path.as_slice(), edge.root, bound));
        }
        if let Some(library) = libraries.get(&edge.root)
            && library.content.id == *id
        {
            claims.push((
                edge.path.as_slice(),
                edge.root,
                library.content.version.clone(),
            ));
        }
    }
    claims.sort_by(|a, b| path_order(a.0, b.0));
    let mut versions = BTreeMap::new();
    for (path, root, version) in claims {
        let (first_path, first_root) = *versions.entry(version.clone()).or_insert((path, root));
        if first_root != root {
            return Err(format!(
                "{id} {version} is two roots:\n  {} = {first_root}\n  {} = {root}",
                path_text(first_path),
                path_text(path)
            )
            .into());
        }
    }
    Ok(())
}

/// Requirements arrive in refusal path order. A displaced exact pin has no known version.
fn bind(
    id: &Id,
    edges: &[Requirement],
    libraries: &Libraries,
    chosen: Option<Choice>,
) -> Result<Root, Failure> {
    check_versions(id, edges, libraries, chosen.as_ref())?;
    if let Some(Choice {
        root, version, by, ..
    }) = chosen
    {
        for edge in edges {
            if edge.accepts(root, version) {
                continue;
            }
            let path = path_text(&edge.path);
            if let Some(bound) = edge.floor(id.major) {
                let requires = if id.major == 0 {
                    format!("{bound} or a later 0.{}.x", bound.minor)
                } else {
                    format!("at least {bound}")
                };
                return Err(format!(
                    "{id} is bound to {version} {root} ({by}), but {path} requires {requires}"
                )
                .into());
            }
            if edge.packed {
                let binder = &edge.path[edge.path.len() - 2];
                return Err(pack_displaced(binder, id, edge.root, root).into());
            }
            return Err(format!(
                "{id} is bound to {root} ({by}), but {path} pins {} exactly",
                edge.root
            )
            .into());
        }
        return Ok(root);
    }
    let roots: BTreeSet<_> = edges.iter().map(|edge| edge.root).collect();
    if roots.len() == 1 {
        return Ok(edges[0].root);
    }
    let mut refusal = format!("{id} is bound to more than one root:");
    for edge in edges {
        let kind = edge
            .floor(id.major)
            .map_or_else(|| "exact".into(), |v| format!("floor {v}"));
        refusal.push_str(&format!(
            "\n  {} = {} {kind}",
            path_text(&edge.path),
            edge.root
        ));
    }
    if roots.iter().any(|root| {
        edges
            .iter()
            .all(|edge| edge.accepts(*root, &libraries[root].content.version))
    }) {
        refusal.push_str(&format!(
            "\nto choose one root for every edge, run: hirsel update {id}"
        ));
    } else {
        let mut seen = BTreeSet::new();
        let exact: Vec<_> = edges
            .iter()
            .filter(|e| matches!(e.binding, Binding::Exact(_)))
            .map(|e| &e.path[e.path.len() - 2])
            .filter(|id| seen.insert(*id))
            .map(ToString::to_string)
            .collect();
        refusal.push_str(&format!(
            "\nno single root satisfies every edge; the exact edges of {} must name one root",
            exact.join(", ")
        ));
    }
    Err(refusal.into())
}

fn pack_displaced(binder: &Id, id: &Id, recorded: Root, effective: Root) -> String {
    format!(
        "pack: {binder} binds {id} {recorded} exactly, but this workspace builds it with {effective}; republish {binder} against {effective} or select {recorded}"
    )
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
        // `pack` writes every binding as exact (section 1). A member with a bare name is a
        // toolchain package under development, which the toolchain supplies.
        content.bindings = member
            .aliases
            .iter()
            .map(|(alias, id)| {
                let binding = match &member.edges[id] {
                    Edge::Toolchain => Binding::Toolchain,
                    Edge::Member if crate::package_manifest::name::identifier(&id.name) => {
                        Binding::Toolchain
                    }
                    Edge::Member => Binding::Exact(encoded[id].0),
                    Edge::Published(pin, _) => Binding::Exact(*pin),
                };
                (alias.clone(), (id.clone(), binding))
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
    published: &BTreeMap<Id, Root>,
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
        for (id, recorded) in content.content.edges() {
            if let Some(recorded) = recorded {
                if let Some(effective) = published.get(&id)
                    && *effective != recorded
                {
                    return Err(pack_displaced(
                        &content.content.id,
                        &id,
                        recorded,
                        *effective,
                    ));
                }
                pending.push_back(recorded);
            }
        }
        reached.insert(
            root,
            Library {
                content: content.content.clone(),
                encoded: content.encoded.clone(),
            },
        );
    }
    let bytes = Bytes::from(PackageArchive::encode(head, &reached)?);
    // A toolchain package's manifest may carry its bare name (section 2), so its pack
    // decodes as the toolchain's archives do.
    let toolchain = crate::package_manifest::name::identifier(&id.name);
    let decoded = PackageArchive::decode_as(bytes.clone(), toolchain)?;
    Ok((bytes, decoded))
}

/// Packs member `id` of a member graph read from a commit (GIT-2 steps 5 and 6): acquires
/// what it reaches through `fetch`, checks the graph as resolution does, and returns the
/// archive bytes and the decoded bundle, verified by the caller.
pub(crate) fn pack_members(
    members: &Members,
    store: &PackageStore,
    selections: &Selections,
    manifest: &Path,
    fetch: &mut Fetch<'_>,
    id: &Id,
) -> Result<(Bytes, PackageArchive), Failure> {
    let graph = Graph {
        members,
        selections,
        manifest,
    };
    let (libraries, _) = acquire(&graph, store, false, BTreeMap::new(), fetch)?;
    let mut encoded = BTreeMap::new();
    let (_, published) = settle(&graph, &libraries, &mut encoded, None)?;
    Ok(assemble(&published, members, &mut encoded, &libraries, id)?)
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
        workspace: Workspace,
        store: PackageStore,
        frozen: bool,
        cached: Libraries,
        fetched: &mut BTreeSet<String>,
    ) -> Result<Self, Failure> {
        Self::resolve_with(workspace, store, frozen, cached, fetched, None)
    }

    pub(crate) fn resolve_pack(
        workspace: Workspace,
        store: PackageStore,
        frozen: bool,
        selected: &Id,
    ) -> Result<Self, Failure> {
        Self::resolve_with(
            workspace,
            store,
            frozen,
            BTreeMap::new(),
            &mut BTreeSet::new(),
            Some(selected),
        )
    }

    fn resolve_with(
        mut workspace: Workspace,
        store: PackageStore,
        frozen: bool,
        cached: Libraries,
        fetched: &mut BTreeSet<String>,
        packing: Option<&Id>,
    ) -> Result<Self, Failure> {
        let manifest = workspace.directory.join(crate::package_dir::MANIFEST);
        let graph = Graph {
            manifest: &manifest,
            members: &workspace.members,
            selections: &workspace.selections,
        };
        let mut inputs = Vec::new();
        let (libraries, declarations) = {
            let directory = &workspace.root;
            let mut fetch = |request: FetchRequest<'_>| match request {
                FetchRequest::Bundle(id, root, source) => {
                    let (archive, identity) = store.fetch(directory, id, root, source, fetched)?;
                    inputs.extend(identity);
                    Ok(archive.entries)
                }
                FetchRequest::Frames(frames) => store.fetch_frames(frames, fetched),
            };
            acquire(&graph, &store, frozen, cached, &mut fetch)?
        };
        let mut encoded = BTreeMap::new();
        let (toolchain, published) = settle(&graph, &libraries, &mut encoded, packing)?;
        for identity in inputs {
            workspace.record_input(identity);
        }
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

    /// The selected member first, then each workspace member that reaches it.
    pub(crate) fn pack_checks<'a>(&'a self, selected: &'a Id) -> Vec<&'a Id> {
        let members = &self.workspace.members;
        let mut dependents: BTreeMap<&Id, Vec<&Id>> = BTreeMap::new();
        for (id, member) in members {
            for dependency in member.edges.keys() {
                dependents.entry(dependency).or_default().push(id);
            }
        }
        for (id, root) in &self.published {
            if members.contains_key(id) {
                continue;
            }
            for (dependency, _) in self.libraries[root].content.bindings.values() {
                dependents.entry(dependency).or_default().push(id);
            }
        }
        let mut reached = BTreeSet::new();
        let mut pending = vec![selected];
        while let Some(id) = pending.pop() {
            if reached.insert(id)
                && let Some(parents) = dependents.get(id)
            {
                pending.extend(parents);
            }
        }
        reached.remove(selected);
        std::iter::once(selected)
            .chain(reached.into_iter().filter(|id| members.contains_key(*id)))
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn member_root(&mut self, id: &Id) -> Result<Root, String> {
        member_root(&self.workspace.members, &mut self.encoded, id)
    }

    pub(crate) fn make_archive(&mut self, id: &Id) -> Result<(Bytes, PackageArchive), String> {
        let (bytes, decoded) = assemble(
            &self.published,
            &self.workspace.members,
            &mut self.encoded,
            &self.libraries,
            id,
        )?;
        decoded.verify(id, decoded.head, "pack output")?;
        Ok((bytes, decoded))
    }
}
