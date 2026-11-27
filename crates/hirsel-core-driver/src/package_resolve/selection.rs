use super::*;

pub(crate) fn select(
    workspace: &Workspace,
    store: &PackageStore,
    cached: Libraries,
    requested: Option<(&Id, &Version)>,
    fetched: &mut BTreeSet<String>,
) -> Result<(Selections, Libraries), Failure> {
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
    let (libraries, declared) = acquire(&graph, store, false, cached, &mut fetch)?;
    let mut seen = BTreeSet::new();
    loop {
        let graph = Graph {
            selections: &selections,
            ..graph
        };
        let paths = best_paths(&graph, &libraries);
        let mut next = Selections::new();
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
            return Ok((next, libraries));
        }
        let roots: Vec<_> = next
            .iter()
            .map(|(id, (root, _))| (id.clone(), *root))
            .collect();
        if !seen.insert(roots) {
            return Err("dependency selections do not settle on a single closure".into());
        }
        selections = next;
    }
}
