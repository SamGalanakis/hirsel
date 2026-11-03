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
    indexed: BTreeMap<FileId, Vec<std::sync::Weak<ItemSyntax>>>,
}
impl HeaderSources {
    fn needs_live(engine: &QueryEngine, syntax: &ItemSyntax) -> bool {
        syntax.item.generated.is_some()
            || engine.adapters.lexer.is_none()
            || engine.adapters.parser.is_none()
            || !engine.indexed_sources.contains_key(&engine.item_file(syntax))
    }
    pub(super) fn capture(engine: &QueryEngine) -> Self {
        let mut files = BTreeMap::<_, Vec<_>>::new();
        let mut indexed = BTreeMap::<_, Vec<_>>::new();
        let mut by_definition = hirsel_syntax::DefMap::<Vec<FileId>>::default();
        for (path, candidates) in &engine.items {
            let mut selected = BTreeSet::new();
            for syntax in candidates {
                let file = engine.item_file(syntax);
                selected.insert(file);
                if Self::needs_live(engine, syntax) {
                    files.entry(file).or_default().push(syntax.clone());
                } else {
                    indexed.entry(file).or_default().push(Arc::downgrade(syntax));
                }
            }
            if !selected.is_empty() {
                by_definition.insert(DefId::intern(path), selected.into_iter().collect());
            }
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
            parent: engine
                .header_products
                .as_ref()
                .map(|headers| headers.syntax.clone()),
            by_definition,
            facts,
            texts,
            files,
            indexed,
        }
    }
    pub(super) fn facts(&self, id: DefId) -> Option<catalog::SourceFacts> {
        self.facts
            .get(&id)
            .map(|facts| (**facts).clone())
            .or_else(|| self.parent.as_ref()?.facts(id))
    }
    pub(super) fn text(&self, id: DefId) -> Option<CapturedText> {
        self.texts
            .get(&id)
            .copied()
            .or_else(|| self.parent.as_ref()?.text(id))
    }
    pub(super) fn facts_index_bytes(definitions: usize) -> u64 {
        let mut weight = crate::NativeMetadataWeight::default();
        weight.tree::<DefId, Arc<catalog::SourceFacts>>(definitions);
        weight.tree::<DefId, CapturedText>(definitions);
        weight.add(definitions.saturating_mul(size_of::<std::sync::Weak<ItemSyntax>>()));
        weight.bytes()
    }
    pub(super) fn written_bytes(engine: &QueryEngine) -> u64 {
        fn charge(seen: &mut BTreeMap<usize, u64>, pointer: usize, bytes: u64) -> u64 {
            let old = seen.entry(pointer).or_default();
            let added = bytes.saturating_sub(*old);
            *old = (*old).max(bytes);
            added
        }
        fn add(seen: &mut BTreeMap<usize, u64>, syntax: &Arc<ItemSyntax>) -> u64 {
            charge(seen, Arc::as_ptr(syntax) as usize, crate::memo::retained::retained_syntax_bytes(syntax))
        }
        fn indexed(seen: &mut BTreeMap<usize, u64>, pointer: usize) -> u64 {
            charge(seen, pointer, crate::memo::retained_arc_bytes(&std::mem::MaybeUninit::<ItemSyntax>::uninit()))
        }
        fn inherited(owner: &HeaderSources, seen: &mut BTreeMap<usize, u64>) -> u64 {
            let live = owner.files.values().flatten().fold(0u64, |n, syntax| n.saturating_add(add(seen, syntax)));
            let weak = owner.indexed.values().flatten().fold(0u64, |n, syntax| n.saturating_add(indexed(seen, syntax.as_ptr() as usize)));
            live.saturating_add(weak).saturating_add(owner.parent.as_ref().map_or(0, |parent| inherited(parent, seen)))
        }
        let mut seen = BTreeMap::new();
        engine.items.values().flatten().fold(0u64, |n, syntax| {
            n.saturating_add(if Self::needs_live(engine, syntax) { add(&mut seen, syntax) }
                else { indexed(&mut seen, Arc::as_ptr(syntax) as usize) })
        }).saturating_add(engine.header_products.as_ref().map_or(0, |headers| inherited(&headers.syntax, &mut seen)))
    }
    pub(super) fn owns(&self, syntax: &Arc<ItemSyntax>) -> bool {
        self.files
            .values()
            .flatten()
            .any(|written| Arc::ptr_eq(written, syntax))
            || self
                .parent
                .as_ref()
                .is_some_and(|parent| parent.owns(syntax))
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
            return self
                .parent
                .as_ref()
                .map_or(Ok(false), |parent| parent.materialize(engine, path));
        };
        let refused = || {
            QueryError::Operational(hirsel_types::OperationalFailure {
                operation: "header source transport".into(),
                message: format!("cannot decode indexed source for {path}"),
            })
        };
        for file in files {
            hirsel_intern::timings::count(hirsel_intern::timings::Count::HeaderSourceFiles);
            let mut syntaxes = self.files.get(file).cloned().unwrap_or_default();
            let indexed = self.indexed.get(file).map(|written| written.iter().map(std::sync::Weak::upgrade).collect::<Option<Vec<_>>>());
            match indexed {
                Some(Some(written)) => syntaxes.extend(written),
                Some(None) if syntaxes.iter().any(|syntax| syntax.item.definition == *path) => {
                    engine.materialize_source(*file)?;
                    syntaxes.extend(engine.items.values().flatten().filter(|syntax| engine.item_file(syntax) == *file).cloned());
                }
                Some(None) => return Ok(false),
                None => {}
            }
            if syntaxes.is_empty() { return Err(refused()); }
            for syntax in &syntaxes {
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
                .place_file(*file, syntaxes, &engine.sources);
        }
        Ok(true)
    }
}
