use crate::package_content::{Id, Library, PackageArchive, Root, check_identity};
use crate::package_manifest::SourceLocation;
use crate::package_store::{Entry, PackageStore};
use crate::package_workspace::{Edge, Workspace};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

type Failure = Box<dyn std::error::Error>;
type Libraries = BTreeMap<Root, Library>;
type Declarations = BTreeMap<Root, (Id, SourceLocation)>;

pub(crate) struct Resolution {
    pub(crate) workspace: Workspace,
    pub(crate) libraries: Libraries,
    pub(crate) toolchain_store: bool,
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

fn best_paths(workspace: &Workspace, libraries: &Libraries) -> Paths {
    let bound: BTreeSet<_> = workspace
        .members
        .values()
        .flat_map(|member| {
            member
                .edges
                .iter()
                .filter_map(|(id, edge)| matches!(edge, Edge::Member).then_some(id))
        })
        .collect();
    let mut queue: BTreeSet<_> = workspace
        .members
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
                Node::Member(id) => workspace.members[id]
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

fn incoming(workspace: &Workspace, libraries: &Libraries) -> BTreeSet<(Id, Root)> {
    let direct = workspace.members.values().flat_map(|member| {
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

fn fetch(
    workspace: &mut Workspace,
    store: &PackageStore,
    declaration: &(Id, SourceLocation),
    root: Root,
    cached: &mut Libraries,
) -> Result<(), Failure> {
    let (id, source) = declaration;
    let (archive, identity) = store.fetch(&workspace.root, id, root, source)?;
    if let Some(identity) = identity {
        workspace.record_input(identity);
    }
    cached.extend(archive.entries);
    Ok(())
}

impl Resolution {
    pub(crate) fn resolve(
        mut workspace: Workspace,
        store: PackageStore,
        frozen: bool,
    ) -> Result<Self, Failure> {
        let mut declared = BTreeMap::new();
        for member in workspace.members.values() {
            for (id, edge) in &member.edges {
                if let Edge::Published(root, Some(source)) = edge {
                    declared
                        .entry(*root)
                        .or_insert_with(|| (id.clone(), source.clone()));
                }
            }
        }
        let mut libraries = BTreeMap::new();
        let mut cached = BTreeMap::new();
        loop {
            let mut pending = Vec::new();
            let mut seen = BTreeSet::new();
            let mut opened = false;
            for (id, root) in incoming(&workspace, &libraries) {
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
                    && let Some(declaration) = declared.get(root)
                {
                    fetch(&mut workspace, &store, declaration, *root, &mut cached)?;
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
            let paths = best_paths(&workspace, &libraries);
            let mut found_carrier = false;
            for (_, root, _) in &pending {
                if !cached.contains_key(root)
                    && let Some(nearest) = carrier(*root, &libraries, &declared, &paths)
                {
                    fetch(
                        &mut workspace,
                        &store,
                        &declared[&nearest],
                        nearest,
                        &mut cached,
                    )?;
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

        let paths = best_paths(&workspace, &libraries);
        let store_id = Id {
            name: "store".into(),
            major: 1,
        };
        let toolchain_store = workspace
            .members
            .values()
            .any(|member| matches!(member.edges.get(&store_id), Some(Edge::Toolchain)))
            || libraries
                .values()
                .any(|library| library.content.bindings.get(&store_id) == Some(&None));
        if toolchain_store
            && (workspace.members.contains_key(&store_id)
                || incoming(&workspace, &libraries)
                    .iter()
                    .any(|(id, _)| id == &store_id))
        {
            return Err(
                "store@1 is bound both to the toolchain and to a member or published library"
                    .into(),
            );
        }
        let mut all = BTreeMap::<Id, BTreeSet<Root>>::new();
        for (id, root) in incoming(&workspace, &libraries) {
            all.entry(id).or_default().insert(root);
        }
        let mut resolution = Self {
            workspace,
            libraries,
            toolchain_store,
            store,
            declarations: declared,
            published: BTreeMap::new(),
            encoded: BTreeMap::new(),
        };
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
            if resolution.workspace.members.contains_key(&id)
                && resolution.member_root(&id)? != root
            {
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
            resolution.published.insert(id, root);
        }
        Ok(resolution)
    }

    pub(crate) fn source(&self, root: Root) -> String {
        source(&self.declarations, &self.libraries, root)
    }

    pub(crate) fn member_root(&mut self, id: &Id) -> Result<Root, String> {
        let mut pending = vec![(id, false)];
        while let Some((current, ready)) = pending.pop() {
            if self.encoded.contains_key(current) {
                continue;
            }
            let member = &self.workspace.members[current];
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
                        Edge::Member => Some(self.encoded[id].0),
                        Edge::Published(pin, _) => Some(*pin),
                    };
                    (id.clone(), root)
                })
                .collect();
            let (root, encoded) = content.inventory()?.encode()?;
            self.encoded.insert(
                current.clone(),
                (
                    root,
                    Library {
                        content,
                        encoded: encoded.into(),
                    },
                ),
            );
        }
        Ok(self.encoded[id].0)
    }

    pub(crate) fn make_archive(&mut self, id: &Id) -> Result<(Vec<u8>, PackageArchive), String> {
        let head = self.member_root(id)?;
        let member_content: BTreeMap<_, _> = self
            .encoded
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
                .unwrap_or_else(|| &self.libraries[&root]);
            pending.extend(content.content.bindings.values().flatten().copied());
            reached.insert(
                root,
                Library {
                    content: content.content.clone(),
                    encoded: content.encoded.clone(),
                },
            );
        }
        let bytes = PackageArchive::encode(head, &reached)?;
        let decoded = PackageArchive::decode(&bytes)?;
        decoded.verify(id, head, "pack output")?;
        Ok((bytes, decoded))
    }
}
