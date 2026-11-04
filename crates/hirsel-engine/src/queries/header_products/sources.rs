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
    files: BTreeMap<FileId, Arc<SyntaxOwner>>,
    sources: SourceMap,
}
pub(crate) struct SourceInputs {
    sources: SourceMap,
    catalog: catalog::Catalog,
    syntax: Arc<HeaderSources>,
    declarations: crate::DeclarationIndex,
    installed: BTreeSet<FileId>,
}
type SyntaxBinding = (DefId, ContentHash, hirsel_span::Anchor, u32);
struct SyntaxOwner {
    bindings: Vec<SyntaxBinding>,
    transport: bool,
    // Indexed payloads have their own source-inference reservation; capture
    // admission counts their control allocations, not the same payload again.
    indexed: bool,
    // A file is produced once, while the request or any header demander owns it.
    // Parsing does not recurse into headers, so demanders can share this lock.
    live: std::sync::Mutex<Result<Option<LiveSyntax>, QueryError>>,
}
struct LiveSyntax {
    items: Vec<Arc<ItemSyntax>>,
    _charge: Charge,
}
impl LiveSyntax {
    fn new(
        mut items: Vec<Arc<ItemSyntax>>,
        charge: impl FnOnce(u64) -> Result<Charge, QueryError>,
    ) -> Result<Self, QueryError> {
        items.shrink_to_fit();
        let bytes = items
            .iter()
            .try_fold(0u64, |n, syntax| {
                n.checked_add(crate::memo::retained::retained_syntax_bytes(syntax))
            })
            .and_then(|n| n.checked_add(std::mem::size_of_val(items.as_slice()) as u64))
            .ok_or_else(|| source_refusal("syntax size overflow"))?;
        let charge = charge(bytes)?;
        Ok(Self {
            items,
            _charge: charge,
        })
    }
}
pub(super) fn source_refusal(message: impl ToString) -> QueryError {
    QueryError::Operational(hirsel_types::OperationalFailure {
        operation: "source syntax admission".into(),
        message: message.to_string(),
    })
}
fn standalone_syntax(bytes: u64) -> Result<Charge, QueryError> {
    let bytes = bytes
        .checked_add(crate::memo::retained_arc_bytes(&std::mem::MaybeUninit::<
            std::sync::Mutex<HeaderCharge>,
        >::uninit()))
        .ok_or_else(|| source_refusal("syntax size overflow"))?;
    let reservation = private_unique::scheduled::standalone_budget()
        .reserve(bytes)
        .map_err(source_refusal)?;
    Ok(Charge(Arc::new(std::sync::Mutex::new(
        HeaderCharge::Standalone(reservation),
    ))))
}

impl SourceInputs {
    pub(crate) fn capture(engine: &QueryEngine) -> Arc<Self> {
        Arc::new(Self {
            sources: engine.sources.clone(),
            catalog: engine
                .catalog
                .source_carry(&engine.sources, &engine.items, &engine.indexed_sources)
                .restore(),
            syntax: Arc::new(HeaderSources::capture_with_parent(
                engine,
                engine
                    .source_inputs
                    .as_ref()
                    .map(|inputs| inputs.syntax.clone())
                    .or_else(|| {
                        engine.snapshot.as_ref().and_then(|(_, snapshot)| {
                            snapshot
                                .header_products()
                                .map(|headers| headers.syntax.clone())
                        })
                    })
                    .or_else(|| {
                        engine
                            .header_products
                            .as_ref()
                            .map(|headers| headers.syntax.clone())
                    }),
            )),
            declarations: engine.declarations.header_inputs(),
            installed: engine.installed_sources.clone(),
        })
    }
    pub(crate) fn metadata_bytes(engine: &QueryEngine) -> Option<u64> {
        engine
            .catalog
            .source_carry_bytes(&engine.items, &engine.indexed_sources)
            .checked_add(HeaderSources::facts_index_bytes(
                engine.source_paths().len(),
            ))?
            .checked_add(engine.declarations.observation_bytes()?)?
            .checked_add((size_of::<Self>() + 2 * size_of::<usize>()) as u64)
    }
    pub(crate) fn install(self: &Arc<Self>, engine: &mut QueryEngine) {
        engine.source_inputs = Some(self.clone());
        engine.sources = self.sources.clone();
        engine.catalog = self.catalog.clone();
        engine
            .catalog
            .bind_ownership(engine.source_ownership.clone());
        engine.declarations = self.declarations.clone();
        engine.installed_sources = self.installed.clone();
        self.syntax.install_live(engine);
    }
    pub(crate) fn owns_written(&self, syntax: &Arc<ItemSyntax>) -> bool {
        self.syntax.owns(syntax)
    }
    pub(crate) fn materialize(
        &self,
        engine: &mut QueryEngine,
        path: &DefPath,
    ) -> Result<bool, QueryError> {
        self.syntax.materialize(engine, path)
    }
}
impl HeaderSources {
    pub(super) fn capture(engine: &QueryEngine) -> Self {
        let parent = engine
            .source_inputs
            .as_ref()
            .map(|inputs| inputs.syntax.clone())
            .or_else(|| {
                engine
                    .header_products
                    .as_ref()
                    .map(|headers| headers.syntax.clone())
            })
            .or_else(|| {
                engine.snapshot.as_ref().and_then(|(_, snapshot)| {
                    snapshot
                        .header_products()
                        .map(|headers| headers.syntax.clone())
                })
            });
        Self::capture_with_parent(engine, parent)
    }
    fn capture_with_parent(engine: &QueryEngine, reuse: Option<Arc<Self>>) -> Self {
        let mut written = BTreeMap::<_, Vec<_>>::new();
        let mut bindings = BTreeMap::<_, Vec<_>>::new();
        let mut by_definition = hirsel_syntax::DefMap::<Vec<FileId>>::default();
        for (id, file, body, anchor, occurrence) in engine.catalog.syntax_bindings() {
            bindings
                .entry(file)
                .or_default()
                .push((id, body, anchor, occurrence));
            by_definition.entry(id).or_default().push(file);
        }
        for (path, candidates) in &engine.items {
            let id = DefId::intern(path);
            for syntax in candidates {
                let file = engine.item_file(syntax);
                bindings.entry(file).or_default().push((
                    id,
                    syntax.text_hash,
                    syntax.anchor,
                    syntax.occurrence,
                ));
                by_definition.entry(id).or_default().push(file);
                written.entry(file).or_default().push(syntax.clone());
            }
        }
        for files in by_definition.values_mut() {
            files.sort_unstable();
            files.dedup();
        }
        let files = bindings
            .into_iter()
            .map(|(file, mut bindings)| {
                bindings.sort_unstable();
                bindings.dedup();
                let inherited = reuse
                    .as_ref()
                    .and_then(|parent| parent.owner(file, &bindings, &engine.sources));
                let owner = inherited.unwrap_or_else(|| {
                    let indexed = engine.indexed_sources.contains_key(&file)
                        && engine.adapters.lexer.is_some()
                        && engine.adapters.parser.is_some();
                    Arc::new(SyntaxOwner {
                        indexed,
                        bindings,
                        transport: written.get(&file).is_some_and(|items| {
                            items.iter().any(|syntax| {
                                syntax.item.generated.is_some()
                                    || engine.adapters.lexer.is_none()
                                    || engine.adapters.parser.is_none()
                                    || !engine.indexed_sources.contains_key(&file)
                            })
                        }),
                        live: std::sync::Mutex::new(
                            written
                                .remove(&file)
                                .map(|items| {
                                    LiveSyntax::new(items, |bytes| {
                                        if indexed {
                                            standalone_syntax(bytes)
                                        } else {
                                            Ok(Charge::engine())
                                        }
                                    })
                                })
                                .transpose(),
                        ),
                    })
                });
                (file, owner)
            })
            .collect();
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
            sources: engine.sources.clone(),
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
        weight.add(size_of::<Self>() + 2 * size_of::<usize>());
        weight.tree::<DefId, Arc<catalog::SourceFacts>>(definitions);
        weight.tree::<DefId, CapturedText>(definitions);
        weight.tree::<DefId, Vec<FileId>>(definitions);
        weight.tree::<FileId, Arc<SyntaxOwner>>(definitions);
        weight.add(definitions.saturating_mul(
            size_of::<SyntaxOwner>()
                + 2 * size_of::<usize>()
                + size_of::<SyntaxBinding>()
                + size_of::<Arc<ItemSyntax>>()
                + size_of::<FileId>(),
        ));
        weight.bytes()
    }
    pub(super) fn written_bytes(engine: &QueryEngine) -> u64 {
        fn add(seen: &mut BTreeMap<usize, u64>, syntax: &Arc<ItemSyntax>, indexed: bool) -> u64 {
            let bytes = if indexed {
                crate::memo::retained_arc_bytes(&std::mem::MaybeUninit::<ItemSyntax>::uninit())
            } else {
                crate::memo::retained::retained_syntax_bytes(syntax)
            };
            let old = seen.entry(Arc::as_ptr(syntax) as usize).or_default();
            let added = bytes.saturating_sub(*old);
            *old = (*old).max(bytes);
            added
        }
        fn inherited(owner: &HeaderSources, seen: &mut BTreeMap<usize, u64>) -> u64 {
            let live = owner.files.values().fold(0u64, |n, file| {
                let live = file.live.lock().unwrap_or_else(|e| e.into_inner());
                n.saturating_add(live.as_ref().ok().and_then(|live| live.as_ref()).map_or(
                    0,
                    |live| {
                        live.items.iter().fold(0u64, |n, syntax| {
                            n.saturating_add(add(seen, syntax, file.indexed))
                        })
                    },
                ))
            });
            live.saturating_add(
                owner
                    .parent
                    .as_ref()
                    .map_or(0, |parent| inherited(parent, seen)),
            )
        }
        let mut seen = BTreeMap::new();
        engine
            .items
            .values()
            .flatten()
            .fold(0u64, |n, syntax| {
                let indexed = engine
                    .indexed_sources
                    .contains_key(&engine.item_file(syntax))
                    && engine.adapters.lexer.is_some()
                    && engine.adapters.parser.is_some();
                n.saturating_add(add(&mut seen, syntax, indexed))
            })
            .saturating_add(
                engine
                    .header_products
                    .as_ref()
                    .map_or(0, |headers| inherited(&headers.syntax, &mut seen)),
            )
    }
    pub(super) fn owns(&self, syntax: &Arc<ItemSyntax>) -> bool {
        self.files.values().any(|file| {
            file.live
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .ok()
                .and_then(|live| live.as_ref())
                .is_some_and(|live| {
                    live.items
                        .iter()
                        .any(|written| Arc::ptr_eq(written, syntax))
                })
        }) || self
            .parent
            .as_ref()
            .is_some_and(|parent| parent.owns(syntax))
    }
    fn owner(
        &self,
        file: FileId,
        bindings: &[SyntaxBinding],
        sources: &SourceMap,
    ) -> Option<Arc<SyntaxOwner>> {
        if self
            .sources
            .file(file)
            .zip(sources.file(file))
            .is_some_and(|(old, current)| {
                old.logical_path() == current.logical_path()
                    && old.anchor == current.anchor
                    && old.text() == current.text()
            })
            && let Some(owner) = self.files.get(&file)
            && owner.bindings == bindings
        {
            return Some(owner.clone());
        }
        self.parent.as_ref()?.owner(file, bindings, sources)
    }
    fn install_live(&self, engine: &mut QueryEngine) {
        if let Some(parent) = &self.parent {
            parent.install_live(engine);
        }
        for (file, owner) in &self.files {
            if !owner.transport {
                continue;
            }
            let live = owner.live.lock().unwrap_or_else(|e| e.into_inner());
            let Some(live) = live.as_ref().ok().and_then(|live| live.as_ref()) else {
                continue;
            };
            let syntaxes: Vec<_> = live
                .items
                .iter()
                .filter(|syntax| {
                    engine.catalog.body(&syntax.item.definition) == Some(syntax.text_hash)
                        && engine
                            .catalog
                            .anchor(&syntax.item.definition, syntax.occurrence)
                            == Some(syntax.anchor)
                })
                .cloned()
                .collect();
            Self::install_file(engine, *file, &syntaxes);
        }
    }
    fn install_file(engine: &mut QueryEngine, file: FileId, syntaxes: &[Arc<ItemSyntax>]) {
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
        if !syntaxes.is_empty() {
            engine
                .declarations
                .place_file(file, syntaxes.to_vec(), &engine.sources);
        }
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
            let owner = self.files.get(file).ok_or_else(refused)?;
            let mut guard = owner.live.lock().unwrap_or_else(|e| e.into_inner());
            let live = guard.as_mut().map_err(|error| error.clone())?;
            if live.is_none() {
                hirsel_intern::timings::count(hirsel_intern::timings::Count::HeaderSourceFiles);
                engine.materialize_source(*file)?;
                let items: Vec<_> = engine
                    .items
                    .values()
                    .flatten()
                    .filter(|syntax| engine.item_file(syntax) == *file)
                    .cloned()
                    .collect();
                if items.is_empty() {
                    return Err(refused());
                }
                *live = Some(LiveSyntax::new(items, |bytes| {
                    engine.header_products.as_ref().map_or_else(
                        || standalone_syntax(bytes),
                        |headers| headers.admit_source(bytes),
                    )
                })?);
            }
            let syntaxes = &live.as_ref().unwrap().items;
            if syntaxes.iter().any(|syntax| {
                syntax.occurrence == 0
                    && engine
                        .catalog
                        .body(&syntax.item.definition)
                        .is_some_and(|expected| {
                            syntax.text_hash != expected
                                || engine
                                    .catalog
                                    .anchor(&syntax.item.definition, syntax.occurrence)
                                    != Some(syntax.anchor)
                        })
            }) {
                return Ok(false);
            }
            Self::install_file(engine, *file, syntaxes);
        }
        Ok(true)
    }
}
