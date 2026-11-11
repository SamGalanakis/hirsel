use super::*;
#[derive(Clone, Copy)]
pub(crate) struct CapturedText {
    pub(crate) cited: ContentHash,
    pub(crate) implementation: Option<ContentHash>,
    /// The digest a signature keys on: every candidate's header.
    pub(crate) header: ContentHash,
    pub(crate) candidates: ContentHash,
    /// The first candidate's file, which a signature's store key admits.
    pub(crate) file: FileId,
    pub(crate) site: Span,
}
pub(super) struct HeaderSources {
    parent: Option<Arc<HeaderSources>>,
    by_definition: hirsel_syntax::DefMap<Vec<FileId>>,
    facts: hirsel_syntax::DefMap<Arc<hirsel_query_products::SourceFacts>>,
    /// Each captured definition's cited text, implementation text and header
    /// digest, so an execution engine that has not materialized it observes
    /// the values the capturing engine observes.
    texts: hirsel_syntax::DefMap<CapturedText>,
    files: BTreeMap<FileId, Arc<SyntaxOwner>>,
    sources: SourceMap,
}
pub(crate) struct SourceInputs {
    sources: SourceMap,
    origin_catalog: catalog::CatalogIdentity,
    origin_items: Weak<BTreeMap<DefPath, Vec<Arc<ItemSyntax>>>>,
    origin_placement: u64,
    reconstructed_items: std::sync::Mutex<Weak<BTreeMap<DefPath, Vec<Arc<ItemSyntax>>>>>,
    matched_sources: std::sync::Mutex<Option<SourceMatch>>,
    pub(super) catalog: catalog::Catalog,
    pub(super) syntax: Arc<HeaderSources>,
    declarations: crate::DeclarationIndex,
    installed: BTreeSet<FileId>,
    retained: u64,
    admission: OnceLock<hirsel_engine_admission::MemoryReservation>,
}
// Both positive and negative comparisons belong to exact immutable source
// owners. Successor assemblies otherwise scan every candidate for every replay.
struct SourceMatch {
    catalog: catalog::CatalogIdentity,
    items: Weak<BTreeMap<DefPath, Vec<Arc<ItemSyntax>>>>,
    placement: u64,
    matches: bool,
    _admission: hirsel_engine_admission::MemoryReservation,
}
type SyntaxBinding = (DefId, ContentHash, hirsel_span::Anchor, u32);
fn compare_binding(left: &SyntaxBinding, right: &SyntaxBinding) -> std::cmp::Ordering {
    left.0
        .fingerprint()
        .cmp(&right.0.fingerprint())
        .then_with(|| left.cmp(right))
}
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
    declarations: crate::snapshot::DeclarationFile,
    _charge: Charge,
}
impl LiveSyntax {
    fn items(&self) -> &[Arc<ItemSyntax>] {
        self.declarations.items()
    }

    fn new(
        file: FileId,
        items: Vec<Arc<ItemSyntax>>,
        sources: &SourceMap,
        charge: impl FnOnce(u64) -> Result<Charge, QueryError>,
    ) -> Result<Self, QueryError> {
        let declarations = crate::snapshot::DeclarationFile::new(file, items, sources);
        let bytes = declarations
            .items()
            .iter()
            .try_fold(0u64, |n, syntax| {
                n.checked_add(hirsel_query_products::retained::retained_syntax_bytes(
                    syntax,
                ))
            })
            .and_then(|n| n.checked_add(declarations.retained_bytes() as u64))
            .ok_or_else(|| source_refusal("syntax size overflow"))?;
        let charge = charge(bytes)?;
        Ok(Self {
            declarations,
            _charge: charge,
        })
    }
}
pub(in crate::queries) fn source_refusal(message: impl ToString) -> QueryError {
    QueryError::Operational(hirsel_types::OperationalFailure {
        operation: "source syntax admission".into(),
        message: message.to_string(),
    })
}
pub(super) fn standalone_syntax(bytes: u64) -> Result<Charge, QueryError> {
    let bytes = bytes
        .checked_add(hirsel_engine_admission::retained_arc_bytes(
            &std::mem::MaybeUninit::<std::sync::Mutex<HeaderCharge>>::uninit(),
        ))
        .ok_or_else(|| source_refusal("syntax size overflow"))?;
    let reservation = super::standalone_budget()
        .reserve(bytes)
        .map_err(source_refusal)?;
    Ok(Charge::retained(HeaderCharge::Standalone(reservation)))
}

impl SourceInputs {
    pub(crate) fn installed_owner(
        &self,
        path: &DefPath,
    ) -> Option<&Arc<hirsel_engine_store::InstalledSourceRecords>> {
        self.catalog.installed_owner(path)
    }

    pub(crate) fn retained_bytes(&self) -> u64 {
        self.retained
    }
    pub(crate) fn reusable(engine: &QueryEngine) -> Option<Arc<Self>> {
        let operation_inputs = engine
            .operation
            .inputs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(Weak::upgrade);
        operation_inputs
            .into_iter()
            .chain(engine.source_inputs.iter().cloned())
            .chain(
                engine
                    .header_products
                    .iter()
                    .map(|headers| headers.inputs.clone()),
            )
            .chain(engine.snapshot.iter().filter_map(|(_, snapshot)| {
                snapshot
                    .get()?
                    .header_products()
                    .map(|headers| headers.inputs.clone())
            }))
            .find(|inputs| inputs.matches(engine))
    }
    fn matches(&self, engine: &QueryEngine) -> bool {
        if self.declarations.revision() != engine.declarations.revision()
            || self.installed != engine.installed_sources
        {
            return false;
        }
        self.matches_sources(engine)
    }
    fn matches_sources(&self, engine: &QueryEngine) -> bool {
        if self.origin_placement == engine.sources.placement_revision()
            && self.origin_catalog.matches(&engine.catalog)
            && self.origin_items.ptr_eq(&engine.items.owner())
        {
            return true;
        }
        // Reconstruction installs this complete catalog. Materializing one of
        // its items adds an alias, not a new binding. Check those aliases while
        // the source generation and table owners prove the unchanged catalog.
        if self.sources.placement_revision() == engine.sources.placement_revision()
            && self.catalog.identity().matches(&engine.catalog)
        {
            let items = engine.items.owner();
            let mut reconstructed = self
                .reconstructed_items
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            // SourceTable mutates through Arc::make_mut, which dissociates weak
            // owners. A verified owner therefore remains an exact alias proof.
            if reconstructed.ptr_eq(&items) {
                return true;
            }
            if engine.items.values().flatten().all(|syntax| {
                let binding = (
                    DefId::intern(&syntax.item.definition),
                    syntax.text_hash,
                    syntax.anchor,
                    syntax.occurrence,
                );
                self.syntax
                    .files
                    .get(&engine.item_file(syntax))
                    .is_some_and(|owner| {
                        owner
                            .bindings
                            .binary_search_by(|candidate| compare_binding(candidate, &binding))
                            .is_ok()
                    })
            }) {
                *reconstructed = items;
                return true;
            }
        }
        if let Some(previous) = &*self
            .matched_sources
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            && previous.placement == engine.sources.placement_revision()
            && previous.catalog.matches(&engine.catalog)
            && previous.items.ptr_eq(&engine.items.owner())
        {
            return previous.matches;
        }
        let matches = self.matches_bindings(engine);
        if let Some(admission) = self.admission.get() {
            let catalog = engine.catalog.identity();
            let bytes = catalog.retained_bytes().saturating_add(
                hirsel_engine_admission::retained_arc_bytes(&std::mem::MaybeUninit::<
                    BTreeMap<DefPath, Vec<Arc<ItemSyntax>>>,
                >::uninit()),
            );
            if let Ok(admission) = admission.budget().reserve(bytes) {
                *self
                    .matched_sources
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = Some(SourceMatch {
                    catalog,
                    items: engine.items.owner(),
                    placement: engine.sources.placement_revision(),
                    matches,
                    _admission: admission,
                });
            }
        }
        matches
    }
    fn matches_bindings(&self, engine: &QueryEngine) -> bool {
        let mut observed = std::collections::HashSet::new();
        let mut files = BTreeMap::new();
        let mut binding = |id, file, body, anchor, occurrence| {
            let matches = self.syntax.files.get(&file).is_some_and(|owner| {
                owner
                    .bindings
                    .binary_search_by(|binding| {
                        compare_binding(binding, &(id, body, anchor, occurrence))
                    })
                    .is_ok()
            }) && self.sources.anchor(anchor) == engine.sources.anchor(anchor)
                && *files.entry(file).or_insert_with(|| {
                    self.sources
                        .file(file)
                        .zip(engine.sources.file(file))
                        .is_some_and(|(previous, current)| {
                            std::ptr::eq(previous, current)
                                || (previous.logical_path() == current.logical_path()
                                    && previous.anchor == current.anchor
                                    && previous.text() == current.text())
                        })
                });
            if matches {
                observed.insert((id, file, body, anchor, occurrence));
            }
            matches
        };
        for (id, file, body, anchor, occurrence) in engine.catalog.syntax_bindings() {
            if !binding(id, file, body, anchor, occurrence) {
                return false;
            }
        }
        for (path, syntaxes) in &engine.items {
            for syntax in syntaxes {
                if !binding(
                    DefId::intern(path),
                    engine.item_file(syntax),
                    syntax.text_hash,
                    syntax.anchor,
                    syntax.occurrence,
                ) {
                    return false;
                }
            }
        }
        observed.len()
            == self
                .syntax
                .files
                .values()
                .map(|owner| owner.bindings.len())
                .sum::<usize>()
    }
    pub(crate) fn capture(engine: &QueryEngine) -> Arc<Self> {
        if let Some(inputs) = Self::reusable(engine) {
            return inputs;
        }
        let origin_catalog = engine.catalog.identity();
        let origin_bytes = origin_catalog.retained_bytes();
        let inputs = Arc::new(Self {
            sources: engine.sources.clone(),
            origin_catalog,
            origin_items: engine.items.owner(),
            reconstructed_items: Default::default(),
            matched_sources: Default::default(),
            origin_placement: engine.sources.placement_revision(),
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
                                .get()?
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
            retained: Self::metadata_bytes(engine)
                .unwrap_or(u64::MAX)
                .saturating_add(origin_bytes)
                .saturating_add(hirsel_engine_admission::retained_arc_bytes(
                    &std::mem::MaybeUninit::<BTreeMap<DefPath, Vec<Arc<ItemSyntax>>>>::uninit(),
                ))
                .saturating_add(
                    (size_of::<Self>() - 288
                        + size_of::<operation_inputs::OperationOwner>()
                        + 2 * size_of::<usize>()) as u64,
                )
                .saturating_add(HeaderSources::written_bytes(engine)),
            admission: OnceLock::new(),
        });
        *engine
            .operation
            .inputs
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(Arc::downgrade(&inputs));
        inputs
    }
    pub(crate) fn metadata_bytes(engine: &QueryEngine) -> Option<u64> {
        engine
            .catalog
            .source_carry_bytes(&engine.items, &engine.indexed_sources)
            .checked_add(HeaderSources::facts_index_bytes(
                engine.source_paths_iter().count(),
            ))?
            .checked_add(engine.declarations.observation_bytes()?)?
            .checked_add((288 + 2 * size_of::<usize>()) as u64)
    }
    pub(crate) fn unadmitted_bytes(&self) -> u64 {
        if self.admission.get().is_some() {
            0
        } else {
            self.retained
        }
    }
    pub(crate) fn admit(
        &self,
        allowance: &mut hirsel_engine_admission::MemoryReservation,
    ) -> Result<(), QueryError> {
        if self.admission.get().is_none() {
            let reservation = allowance
                .split(self.retained)
                .ok_or_else(|| source_refusal("operation input reservation"))?;
            if let Err(reservation) = self.admission.set(reservation) {
                allowance
                    .join(reservation)
                    .map_err(|_| source_refusal("operation input partition"))?;
            }
        }
        Ok(())
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
    pub(crate) fn syntax(&self, path: &DefPath) -> Result<Option<Arc<ItemSyntax>>, QueryError> {
        let syntax = self.syntax.syntax(DefId::intern(path))?;
        if let Some(syntax) = &syntax
            && self.catalog.body(path).is_some_and(|expected| {
                syntax.text_hash != expected
                    || self.catalog.anchor(path, syntax.occurrence) != Some(syntax.anchor)
            })
        {
            return Err(source_refusal("captured source binding changed"));
        }
        Ok(syntax)
    }
    pub(crate) fn siblings(
        &self,
        syntax: &ItemSyntax,
    ) -> Result<Option<Arc<[Arc<ItemSyntax>]>>, QueryError> {
        self.syntax.siblings(syntax)
    }
    pub(crate) fn materialize(
        &self,
        engine: &mut QueryEngine,
        path: &DefPath,
    ) -> Result<bool, QueryError> {
        self.syntax.materialize(engine, path)
    }
    pub(crate) fn materialize_siblings(
        &self,
        engine: &mut QueryEngine,
        path: &DefPath,
    ) -> Result<bool, QueryError> {
        self.syntax.materialize_siblings(engine, path)
    }
}
impl HeaderSources {
    /// Reparseable syntax is optional once every consumer releases its item.
    /// Skip the file currently being produced: source admission can reclaim
    /// other files while that file's live lock is held.
    pub(super) fn evict(&self) -> bool {
        for owner in self.files.values().filter(|owner| owner.indexed) {
            let Ok(mut slot) = owner.live.try_lock() else {
                continue;
            };
            let Ok(Some(live)) = &*slot else {
                continue;
            };
            if live._charge.0.is_some()
                && live.declarations.unpinned()
                && live
                    .items()
                    .iter()
                    .all(|syntax| Arc::strong_count(syntax) == 1)
            {
                *slot = Ok(None);
                return true;
            }
        }
        self.parent.as_ref().is_some_and(|parent| parent.evict())
    }
    fn siblings(&self, syntax: &ItemSyntax) -> Result<Option<Arc<[Arc<ItemSyntax>]>>, QueryError> {
        let file = self
            .sources
            .anchor(syntax.anchor)
            .map(|(file, _)| file)
            .ok_or_else(|| source_refusal("sibling anchor outside captured source"))?;
        let Some(owner) = self.files.get(&file) else {
            return self
                .parent
                .as_ref()
                .map_or(Ok(None), |parent| parent.siblings(syntax));
        };
        // Siblings belong to the written declaration the requester stands at.
        // Derived syntax (a generated body, its normalized header, a generator
        // constant, a specialization) keeps that declaration's anchor but may
        // rename it; only a generated body rehashes, folding the written hash
        // into its generated text, so it is bound by its unchanged definition
        // and occurrence instead.
        let id = DefId::intern(&syntax.item.definition);
        let stands = |&(definition, body, anchor, occurrence): &SyntaxBinding| {
            anchor == syntax.anchor
                && match syntax.item.generated {
                    Some(hirsel_syntax::Generated::Body(_)) => {
                        definition == id && occurrence == syntax.occurrence
                    }
                    _ => body == syntax.text_hash,
                }
        };
        if !owner.bindings.iter().any(stands) {
            return Err(source_refusal("captured sibling binding changed"));
        }
        let guard = owner.live.lock().unwrap_or_else(|e| e.into_inner());
        Ok(guard
            .as_ref()
            .map_err(Clone::clone)?
            .as_ref()
            .map(|live| live.declarations.siblings()))
    }
    fn syntax(&self, id: DefId) -> Result<Option<Arc<ItemSyntax>>, QueryError> {
        let Some(files) = self.by_definition.get(&id) else {
            return self
                .parent
                .as_ref()
                .map_or(Ok(None), |parent| parent.syntax(id));
        };
        let mut first: Option<Arc<ItemSyntax>> = None;
        for file in files {
            let owner = self
                .files
                .get(file)
                .ok_or_else(|| source_refusal("missing syntax owner"))?;
            let guard = owner.live.lock().unwrap_or_else(|e| e.into_inner());
            let Some(live) = guard.as_ref().map_err(Clone::clone)? else {
                // The ordinary materializing snapshot API fills this owner on demand.
                return Ok(None);
            };
            for syntax in live
                .items()
                .iter()
                .filter(|syntax| syntax.item.definition == *id.path())
            {
                if owner
                    .bindings
                    .binary_search_by(|binding| {
                        compare_binding(
                            binding,
                            &(id, syntax.text_hash, syntax.anchor, syntax.occurrence),
                        )
                    })
                    .is_err()
                {
                    return Err(source_refusal("captured syntax binding changed"));
                }
                if first
                    .as_ref()
                    .is_none_or(|old| syntax.occurrence < old.occurrence)
                {
                    first = Some(syntax.clone());
                }
            }
        }
        Ok(first)
    }
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
                        .get()?
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
                bindings.sort_unstable_by(compare_binding);
                bindings.dedup();
                let inherited = reuse
                    .as_ref()
                    .and_then(|parent| parent.owner(file, &bindings, &engine.sources));
                let owner = inherited.unwrap_or_else(|| {
                    let indexed = engine.indexed_sources.contains_key(&file)
                        && engine.adapters.lexer.is_some()
                        && engine.adapters.parser.is_some();
                    let written = written.remove(&file);
                    let transport = written.as_ref().is_some_and(|items| {
                        items.iter().any(|syntax| {
                            syntax.item.generated.is_some()
                                || engine.adapters.lexer.is_none()
                                || engine.adapters.parser.is_none()
                                || !engine.indexed_sources.contains_key(&file)
                        })
                    });
                    // Reparseable sibling products must cover the full file.
                    // Sparse installed bodies retain their individual owners.
                    let written =
                        written.filter(|items| transport || items.len() == bindings.len());
                    Arc::new(SyntaxOwner {
                        indexed,
                        bindings,
                        transport,
                        live: std::sync::Mutex::new(
                            written
                                .map(|items| {
                                    LiveSyntax::new(file, items, &engine.sources, |bytes| {
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
                        candidates: engine.candidates_value(path),
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
    pub(super) fn facts(&self, id: DefId) -> Option<hirsel_query_products::SourceFacts> {
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
        let mut weight = hirsel_query_products::retained::MetadataWeight::default();
        weight.add(size_of::<Self>() + 2 * size_of::<usize>());
        // The operation ledger has at most one counter per source definition.
        weight.tree::<DefId, usize>(definitions);
        weight.tree::<DefId, Arc<hirsel_query_products::SourceFacts>>(definitions);
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
                hirsel_engine_admission::retained_arc_bytes(
                    &std::mem::MaybeUninit::<ItemSyntax>::uninit(),
                )
            } else {
                hirsel_query_products::retained::retained_syntax_bytes(syntax)
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
                        live.items().iter().fold(0u64, |n, syntax| {
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
                    live.items()
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
                .items()
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
            if syntaxes.len() == live.items().len() {
                Self::install_items(engine, live.items());
                engine.declarations.place_product(&live.declarations);
            } else {
                Self::install_file(engine, *file, &syntaxes);
            }
        }
    }
    fn install_file(engine: &mut QueryEngine, file: FileId, syntaxes: &[Arc<ItemSyntax>]) {
        let changed = Self::install_items(engine, syntaxes);
        if !syntaxes.is_empty() && (changed || !engine.declarations.has_file(file)) {
            engine
                .declarations
                .place_file(file, syntaxes.to_vec(), &engine.sources);
        }
    }
    fn install_items(engine: &mut QueryEngine, syntaxes: &[Arc<ItemSyntax>]) -> bool {
        let mut changed = false;
        let mut owners = None;
        for syntax in syntaxes {
            if engine
                .items
                .get(&syntax.item.definition)
                .is_some_and(|items| {
                    items.iter().any(|old| {
                        old.anchor == syntax.anchor && old.occurrence == syntax.occurrence
                    })
                })
            {
                continue;
            }
            if !engine.items.contains_key(&syntax.item.definition) {
                let owners =
                    owners.get_or_insert_with(|| crate::NativeRecordContext::capture(engine));
                engine.materialize_bound_syntax(&syntax.item.definition, syntax.clone(), owners);
                changed = true;
                continue;
            }
            let candidates = engine
                .items
                .entry(syntax.item.definition.clone())
                .or_default();
            candidates.push(syntax.clone());
            candidates.sort_by_key(|s| s.occurrence);
            changed = true;
        }
        changed
    }
    pub(super) fn materialize(
        &self,
        engine: &mut QueryEngine,
        path: &DefPath,
    ) -> Result<bool, QueryError> {
        self.materialize_input(engine, path, false)
    }
    pub(super) fn materialize_siblings(
        &self,
        engine: &mut QueryEngine,
        path: &DefPath,
    ) -> Result<bool, QueryError> {
        self.materialize_input(engine, path, true)
    }
    fn materialize_input(
        &self,
        engine: &mut QueryEngine,
        path: &DefPath,
        siblings: bool,
    ) -> Result<bool, QueryError> {
        if !siblings && engine.items.contains_key(path) {
            return Ok(true);
        }
        let Some(files) = self.by_definition.get(&DefId::intern(path)) else {
            return self.parent.as_ref().map_or(Ok(false), |parent| {
                parent.materialize_input(engine, path, siblings)
            });
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
                *live = Some(LiveSyntax::new(*file, items, &engine.sources, |bytes| {
                    engine.header_products.as_ref().map_or_else(
                        || standalone_syntax(bytes),
                        |headers| headers.admit_source(bytes),
                    )
                })?);
            }
            let product = live.as_ref().unwrap();
            let syntaxes = product.items();
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
            Self::install_items(engine, syntaxes);
            engine.declarations.place_product(&product.declarations);
        }
        // A closed specialization can share a written source owner without
        // being one of that file's parsed items. Its ordinary materializer
        // must still construct the requested body from the captured base.
        Ok(engine.items.contains_key(path))
    }
}
