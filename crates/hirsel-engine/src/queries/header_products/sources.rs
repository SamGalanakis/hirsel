use super::*;
#[derive(Clone, Copy)]
pub(crate) struct CapturedText {
    pub(crate) cited: ContentHash,
    pub(crate) implementation: Option<ContentHash>,
    /// The digest a signature keys on: every candidate's header.
    pub(crate) header: ContentHash,
    /// The first candidate's file, which a signature's store key admits.
    pub(crate) file: FileId,
    pub(crate) site: Span,
}
pub(super) struct HeaderSources {
    parent: Option<Arc<HeaderSources>>,
    by_definition: hirsel_syntax::DefMap<Vec<FileId>>,
    facts: hirsel_syntax::DefMap<Arc<catalog::SourceFacts>>,
    /// Each captured definition's cited text, implementation text and header
    /// digest, so an execution engine that has not materialized it observes
    /// the values the capturing engine observes.
    texts: hirsel_syntax::DefMap<CapturedText>,
    files: BTreeMap<FileId, Vec<Arc<ItemSyntax>>>,
}
impl HeaderSources {
    pub(super) fn capture(engine: &QueryEngine) -> Self {
        let mut files = BTreeMap::<_, Vec<_>>::new();
        let mut by_definition = hirsel_syntax::DefMap::<Vec<FileId>>::default();
        for (path, candidates) in &engine.items {
            let mut selected = BTreeSet::new();
            for syntax in candidates {
                let file = engine.item_file(syntax);
                selected.insert(file);
                files.entry(file).or_default().push(syntax.clone());
            }
            by_definition.insert(DefId::intern(path), selected.into_iter().collect());
        }
        let facts = engine
            .items
            .iter()
            .filter_map(|(path, candidates)| {
                Some((
                    DefId::intern(path),
                    engine.catalog.shared_syntax_facts(candidates.first()?),
                ))
            })
            .collect();
        let texts = engine
            .items
            .iter()
            .filter_map(|(path, candidates)| {
                Some((
                    DefId::intern(path),
                    CapturedText {
                        cited: engine.cited_value(path),
                        implementation: engine.implementation_hash(path),
                        header: engine.header_digest(path),
                        file: engine.item_file(candidates.first()?),
                        site: candidates.first()?.item.name_span,
                    },
                ))
            })
            .collect();
        Self {
            parent: engine.header_products.as_ref().map(|headers| headers.syntax.clone()),
            by_definition,
            facts,
            texts,
            files,
        }
    }
    pub(super) fn facts(&self, id: DefId) -> Option<catalog::SourceFacts> {
        self.facts.get(&id).map(|facts| (**facts).clone()).or_else(|| self.parent.as_ref()?.facts(id))
    }
    pub(super) fn text(&self, id: DefId) -> Option<CapturedText> {
        self.texts.get(&id).copied().or_else(|| self.parent.as_ref()?.text(id))
    }
    pub(super) fn facts_index_bytes(definitions: usize) -> u64 {
        let mut weight = crate::NativeMetadataWeight::default();
        weight.tree::<DefId, Arc<catalog::SourceFacts>>(definitions);
        weight.tree::<DefId, CapturedText>(definitions);
        weight.bytes()
    }
    pub(super) fn written_bytes(engine: &QueryEngine) -> u64 {
        fn add(seen: &mut BTreeSet<usize>, syntax: &Arc<ItemSyntax>) -> u64 {
            if seen.insert(Arc::as_ptr(syntax) as usize) {
                crate::memo::retained::retained_syntax_bytes(syntax)
            } else { 0 }
        }
        fn inherited(owner: &HeaderSources, seen: &mut BTreeSet<usize>) -> u64 {
            owner.files.values().flatten().fold(0u64, |n, syntax| n.saturating_add(add(seen, syntax)))
                .saturating_add(owner.parent.as_ref().map_or(0, |parent| inherited(parent, seen)))
        }
        let mut seen = BTreeSet::new();
        engine.items.values().flatten().fold(0u64, |n, syntax| n.saturating_add(add(&mut seen, syntax)))
            .saturating_add(engine.header_products.as_ref().map_or(0, |headers| inherited(&headers.syntax, &mut seen)))
    }
    pub(super) fn materialize(
        &self,
        engine: &mut QueryEngine,
        path: &DefPath,
    ) -> Result<bool, QueryError> {
        if engine.items.contains_key(path) {
            return Ok(true);
        }
        let Some(files) = self.by_definition.get(&DefId::intern(path)) else {
            return self.parent.as_ref().map_or(Ok(false), |parent| parent.materialize(engine, path));
        };
        let refused = || {
            QueryError::Operational(hirsel_types::OperationalFailure {
                operation: "header source transport".into(),
                message: format!("cannot decode indexed source for {path}"),
            })
        };
        for file in files {
            hirsel_intern::timings::count(hirsel_intern::timings::Count::HeaderSourceFiles);
            let syntaxes = self.files.get(file).ok_or_else(refused)?;
            for syntax in syntaxes {
                let candidates = engine
                    .items
                    .entry(syntax.item.definition.clone())
                    .or_default();
                if !candidates
                    .iter()
                    .any(|old| old.anchor == syntax.anchor && old.occurrence == syntax.occurrence)
                {
                    candidates.push(syntax.clone());
                    candidates.sort_by_key(|s| s.occurrence);
                }
            }
            engine
                .declarations
                .place_file(*file, syntaxes.clone(), &engine.sources);
        }
        Ok(true)
    }
}
