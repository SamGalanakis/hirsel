use super::*;
use crate::package_add::registry_select::YoungTrials;

pub(crate) fn select(
    workspace: &Workspace,
    store: &PackageStore,
    trial: (Libraries, YoungTrials),
    requested: Option<(&Id, &Version)>,
    fetched: &mut BTreeSet<String>,
    registry: &mut crate::package_add::registry_select::Registry,
    named: impl Fn(&Id) -> bool,
) -> Result<(Selections, Libraries, YoungTrials), Failure> {
    let (cached, direct_trials) = trial;
    let manifest = workspace.directory.join(crate::package_dir::MANIFEST);
    let mut selections = Selections::new();
    let graph = Graph {
        manifest: &manifest,
        members: &workspace.members,
        selections: &Selections::new(),
    };
    let mut fetch = |request: FetchRequest<'_>| match request {
        FetchRequest::Bundle(id, root, source) => Ok(store
            .fetch(&workspace.root, id, root, source, fetched)?
            .0
            .entries),
        FetchRequest::Frames(frames) => store.fetch_frames(frames, fetched),
    };
    let (mut libraries, mut declared) = acquire(&graph, store, false, cached, &mut fetch)?;
    let mut seen = BTreeSet::new();
    loop {
        let graph = Graph {
            selections: &selections,
            ..graph
        };
        let paths = best_paths(&graph, &libraries);
        let mut next = Selections::new();
        let mut young = direct_trials.clone();
        let logged = logged_floors(&graph, &libraries, &declared, &paths);
        let mut new_candidates = false;
        let mut refusal = None;
        let mut matched = false;
        for (id, edges) in requirements(graph.members, &libraries, &paths, None) {
            if graph.members.contains_key(&id) {
                continue;
            }
            let version = requested
                .filter(|(target, _)| **target == id)
                .map(|(_, v)| v);
            let roots: BTreeSet<_> = edges.iter().map(|edge| edge.root).collect();
            if logged.contains(&id)
                && edges
                    .iter()
                    .all(|e| matches!(e.binding, Binding::Floor { .. }))
            {
                let prior = workspace.selections.get(&id);
                if named(&id) {
                    let origin = registry.open(&id.name, None)?;
                    let tested = edges
                        .iter()
                        .max_by_key(|e| e.floor(id.major))
                        .expect("incoming floors")
                        .root;
                    let current = prior.map_or(tested, |(root, _)| *root);
                    let query = crate::package_add::registry_select::Query {
                        name: id.name.clone(),
                        major: Some(id.major),
                        exact: version.cloned(),
                    };
                    let selected = registry.select(
                        &query,
                        Some((&origin, current, &id.name)),
                        |root, v| edges.iter().all(|e| e.accepts(root, v)),
                    )?;
                    let selected = selected.for_trial(store, &mut young)?;
                    let root = selected.root;
                    if version.is_some() {
                        matched = true;
                    }
                    libraries.insert(root, selected.library);
                    check_versions(
                        &id,
                        &edges,
                        &libraries,
                        Some(&Choice {
                            root,
                            version: &libraries[&root].content.version,
                            by: origin.to_string(),
                            path: &edges[0].path,
                        }),
                    )?;
                    if roots.iter().any(|r| *r != root) {
                        next.insert(id, (root, Some(SourceLocation::Registry(origin))));
                        new_candidates = true;
                    }
                    continue;
                } else if let Some((root, Some(source @ SourceLocation::Registry(_)))) = prior {
                    if roots.iter().any(|r| *r != *root) {
                        next.insert(id, (*root, Some(source.clone())));
                        new_candidates = true;
                    }
                    continue;
                }
            }
            // An incoming edge of a newly selected release can name a tested
            // root hidden by another selection. Fetch it before comparing candidates.
            for root in &roots {
                if !libraries.contains_key(root) {
                    let mut tested = selections.clone();
                    tested.insert(id.clone(), (*root, None));
                    let tested_graph = Graph {
                        selections: &tested,
                        ..graph
                    };
                    let (captured, _) =
                        acquire(&tested_graph, store, false, young.inputs()?, &mut fetch)?;
                    libraries.extend(captured);
                    if !libraries.contains_key(root) {
                        return Err(format!("{id} tested root {root} is unavailable").into());
                    }
                }
            }
            let choice = roots
                .iter()
                .filter_map(|root| {
                    let actual = &libraries[root].content.version;
                    if version == Some(actual) {
                        matched = true;
                    }
                    (version.is_none_or(|v| actual == v)
                        && edges.iter().all(|edge| edge.accepts(*root, actual)))
                    .then_some((actual, *root))
                })
                .max();
            if let Some((_, root)) = choice {
                if roots.len() > 1 {
                    let source = declared
                        .get(&root)
                        .map(|(_, source)| source.clone())
                        .or_else(|| {
                            workspace
                                .selections
                                .get(&id)
                                .filter(|(old, _)| *old == root)
                                .and_then(|(_, source)| source.clone())
                        });
                    let source = source.map(|mut source| {
                        if let SourceLocation::Git(git) = &mut source {
                            git.reference = None;
                        }
                        source
                    });
                    next.insert(id.clone(), (root, source));
                }
                if let Err(error) = check_versions(&id, &edges, &libraries, None) {
                    refusal.get_or_insert(error);
                }
            } else if let Err(error) = bind(&id, &edges, &libraries, None) {
                refusal.get_or_insert(error);
            } else if let Some(version) = version {
                refusal
                    .get_or_insert_with(|| format!("no edge names {id} version {version}").into());
            }
        }
        if next == selections {
            if let Some(error) = refusal {
                return Err(error);
            }
            if let Some((id, version)) = requested
                && !matched
            {
                return Err(format!("no edge names {id} version {version}").into());
            }
            return Ok((next, libraries, young));
        }
        let roots: Vec<_> = next
            .iter()
            .map(|(id, (root, _))| (id.clone(), *root))
            .collect();
        if !seen.insert(roots) {
            return Err("dependency selections do not settle on a single closure".into());
        }
        if new_candidates {
            let next_graph = Graph {
                selections: &next,
                ..graph
            };
            let (reached, sources) =
                acquire(&next_graph, store, false, young.inputs()?, &mut fetch)?;
            libraries.extend(reached);
            declared.extend(sources);
        }
        selections = next;
    }
}

/// Logged floor edges reachable from registry carriers. An exact child can
/// carry further logged floors without itself having a release row.
fn logged_floors(
    graph: &Graph<'_>,
    libs: &Libraries,
    declared: &Declarations,
    paths: &Paths,
) -> BTreeSet<Id> {
    let mut work: Vec<Root> = declared
        .iter()
        .filter_map(|(root, (id, source))| {
            (matches!(source, SourceLocation::Registry(_))
                && paths.contains_key(&Node::Published(id.clone(), *root)))
            .then_some(*root)
        })
        .collect();
    let mut seen = BTreeSet::new();
    let mut ids = BTreeSet::new();
    while let Some(root) = work.pop() {
        if !seen.insert(root) {
            continue;
        }
        let Some(lib) = libs.get(&root) else { continue };
        for (id, edge) in lib.content.bindings.values() {
            let Some(recorded) = edge.root() else {
                continue;
            };
            let Node::Published(_, child) = graph.destination(id, recorded) else {
                continue;
            };
            if declared
                .get(&child)
                .is_some_and(|(_, s)| !matches!(s, SourceLocation::Registry(_)))
            {
                continue;
            }
            if matches!(edge, Binding::Floor { .. }) {
                ids.insert(id.clone());
            }
            work.push(child);
        }
    }
    ids
}
