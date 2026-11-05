//! Structural Check, independent of module proof and Modes publication.
//! Port of rustc 1.91 query dep-node validation,
//! compiler/rustc_query_system/src/dep_graph/graph.rs:959-990.
//! A retained result is reused only after every consumed provider facet matches.
//! Here the dependencies are exact provider results rather than dep-node colors.
//! Signatures passed to Check contain declaration fields, not inferred ownership.
use super::*;
use hirsel_types::{DependencyRead, Signature, TypeInterner};
use std::hash::{Hash, Hasher};
use std::sync::{Mutex, OnceLock, Weak};

type ResultBody = Result<QueryOutput<TypedBody>, QueryError>;
#[derive(Clone, Debug, PartialEq, Eq)]
enum Facet {
    Signature(DefId, Result<SignatureRef, QueryError>),
    Lookup(FileContext, hirsel_syntax::Path, hirsel_types::LookupOutput),
    Name(DefPath, Option<Span>),
    Unique(
        DefPath,
        hirsel_types::UniqueClaim,
        Option<hirsel_types::UniqueFailure>,
    ),
}
struct Input {
    types: Arc<TypeInterner>,
    checker: Arc<dyn CheckerPhase>,
    syntax: Arc<ItemSyntax>,
    resolution: Resolution,
    signature: SignatureRef,
}
impl PartialEq for Input {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.types, &other.types)
            && Arc::ptr_eq(&self.checker, &other.checker)
            && self.syntax == other.syntax
            && self.resolution == other.resolution
            && self.signature == other.signature
    }
}
impl Eq for Input {}
impl std::fmt::Debug for Input {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StructuralInput")
            .field("types", &Arc::as_ptr(&self.types))
            .field("checker", &Arc::as_ptr(&self.checker))
            .field("syntax", &self.syntax)
            .field("resolution", &self.resolution)
            .field("declaration", &self.signature)
            .finish()
    }
}
#[derive(Debug)]
struct Key {
    input: Input,
    facets: Vec<Facet>,
    bytes: u64,
}
impl PartialEq for Key {
    fn eq(&self, other: &Self) -> bool {
        self.input == other.input && self.facets == other.facets
    }
}
impl Eq for Key {}
impl Hash for Key {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Collisions compare the complete typed input and every provider result.
        self.input.signature.definition.hash(state);
    }
}
type Table = hirsel_validate::FactTable<Arc<Key>, QueryOutput<TypedBody>, QueryError>;
#[derive(Default)]
pub(in crate::queries) struct Checks {
    facts: OnceLock<Table>,
    recipes: Mutex<BTreeMap<DefId, Vec<Weak<Key>>>>,
}
impl Checks {
    fn facts(&self, graph: Arc<hirsel_validate::FactGraph>) -> &Table {
        self.facts.get_or_init(|| Table::new(graph))
    }
    pub(in crate::queries) fn evict(&self) -> bool {
        // Weak promotion uses this lock too. Keep it through eviction so no
        // reader can acquire a key between the pin check and lease release.
        let mut recipes = self.recipes.lock().unwrap();
        let removed = std::mem::take(&mut *recipes);
        let any = !removed.is_empty();
        drop(removed);
        self.facts.get().map_or(0, |table| {
            table.evict_ready_entries_when(|key, result| {
                Arc::strong_count(key) == 1
                    && result
                        .as_ref()
                        .map_or(true, |value| Arc::strong_count(value) == 1)
            })
        }) != 0
            || any
    }
}
fn retain(result: &Result<Arc<QueryOutput<TypedBody>>, QueryError>) -> bool {
    result.is_ok() || result.as_ref().is_err_and(|error| error.cacheable())
}
fn declaration(types: &TypeInterner, signature: &Signature) -> Result<SignatureRef, QueryError> {
    let mut signature = signature.clone();
    signature.metadata.inferred_unique = Default::default();
    signature.fingerprint =
        hirsel_types::encoding::fingerprint(types, &signature, &[]).map_err(|error| {
            header_products::source_refusal(format!("structural signature: {error:?}"))
        })?;
    Ok(Arc::new(signature))
}
struct Provider<'a> {
    inner: &'a mut dyn SignatureProvider,
    types: &'a TypeInterner,
    facets: Vec<Facet>,
    actual: BTreeMap<DefId, ContentHash>,
}
impl SignatureProvider for Provider<'_> {
    fn signature(&mut self, id: DefId) -> Result<SignatureRef, QueryError> {
        let result = self.inner.signature(id).and_then(|signature| {
            self.actual.insert(id, signature.fingerprint);
            declaration(self.types, &signature)
        });
        self.facets.push(Facet::Signature(id, result.clone()));
        result
    }
    fn lookup(
        &mut self,
        context: &FileContext,
        path: &hirsel_syntax::Path,
    ) -> hirsel_types::LookupOutput {
        let result = self.inner.lookup(context, path);
        self.facets
            .push(Facet::Lookup(context.clone(), path.clone(), result.clone()));
        result
    }
    fn cite_name(&mut self, path: &DefPath) -> Option<Span> {
        let result = self.inner.cite_name(path);
        self.facets.push(Facet::Name(path.clone(), result));
        result
    }
    fn cite_unique_failure(
        &mut self,
        path: &DefPath,
        claim: hirsel_types::UniqueClaim,
    ) -> Option<hirsel_types::UniqueFailure> {
        let result = self.inner.cite_unique_failure(path, claim);
        self.facets
            .push(Facet::Unique(path.clone(), claim, result.clone()));
        result
    }
}
impl Provider<'_> {
    fn replay(&mut self, facets: &[Facet]) -> bool {
        for facet in facets {
            match facet {
                Facet::Signature(id, _) => {
                    let _ = self.signature(*id);
                }
                Facet::Lookup(context, path, _) => {
                    self.lookup(context, path);
                }
                Facet::Name(path, _) => {
                    self.cite_name(path);
                }
                Facet::Unique(path, claim, _) => {
                    self.cite_unique_failure(path, *claim);
                }
            }
            if self.facets.last() != Some(facet) {
                return false;
            }
        }
        true
    }
    fn restore(&self, mut result: ResultBody) -> ResultBody {
        if let Ok(output) = &mut result {
            for dependency in &mut output.dependencies {
                dependency.fingerprint =
                    *self.actual.get(&dependency.definition).ok_or_else(|| {
                        header_products::source_refusal(
                            "Check dependency was not a consumed signature",
                        )
                    })?;
            }
            output.value.checked.resolution.reads = Arc::new(
                output
                    .value
                    .checked
                    .resolution
                    .reads
                    .iter()
                    .map(|read| match read.as_ref() {
                        DependencyRead::Signature {
                            definition,
                            fingerprint: Some(_),
                        } => {
                            let fingerprint =
                                self.actual.get(definition).copied().ok_or_else(|| {
                                    header_products::source_refusal(
                                        "Check resolution has an unobserved signature",
                                    )
                                })?;
                            Ok(DependencyRead::Signature {
                                definition: *definition,
                                fingerprint: Some(fingerprint),
                            }
                            .shared())
                        }
                        _ => Ok(read.clone()),
                    })
                    .collect::<Result<_, QueryError>>()?,
            );
        }
        result
    }
}
impl Worker<'_> {
    pub(super) fn structural_check(
        &self,
        syntax: Arc<ItemSyntax>,
        mut resolution: Resolution,
        signature: &SignatureRef,
        provider: &mut dyn SignatureProvider,
    ) -> ResultBody {
        let checker = self.checker.ok_or(QueryError::NotImplemented)?;
        let mut actual = BTreeMap::from([(signature.definition, signature.fingerprint)]);
        // Resolver signature receipts are metadata, not part of the checked
        // relation. Normalize them before comparing the structural input.
        resolution.reads = Arc::new(
            resolution
                .reads
                .iter()
                .map(|read| match read.as_ref() {
                    DependencyRead::Signature {
                        definition,
                        fingerprint: Some(_),
                    } => {
                        let signature = self
                            .snapshot
                            .body_view(&syntax.item.definition.0.package, *definition)
                            .ok_or(QueryError::NotImplemented)??;
                        actual.insert(*definition, signature.fingerprint);
                        Ok(DependencyRead::Signature {
                            definition: *definition,
                            fingerprint: Some(declaration(self.types, &signature)?.fingerprint),
                        }
                        .shared())
                    }
                    _ => Ok(read.clone()),
                })
                .collect::<Result<_, QueryError>>()?,
        );
        let declared = declaration(self.types, signature)?;
        let headers = self.snapshot.header_products();
        let cache = headers.and_then(|headers| headers.structural_checks(checker));
        let input = cache.as_ref().map(|(_, checker)| Input {
            types: self.snapshot.types().clone(),
            checker: checker.clone(),
            syntax: syntax.clone(),
            resolution: resolution.clone(),
            signature: declared.clone(),
        });
        if let (Some((checks, _)), Some(input)) = (&cache, &input) {
            let candidates = checks
                .recipes
                .lock()
                .unwrap()
                .get(&signature.definition)
                .into_iter()
                .flatten()
                .filter_map(Weak::upgrade)
                .collect::<Vec<_>>();
            for candidate in candidates {
                if candidate.input != *input {
                    continue;
                }
                // Probe without changing the consuming stage's receipt on a
                // miss. On a hit replay through its recorder to bind the full,
                // current fingerprints and citations to this consumer.
                let mut reads = ReadsBuilder::new();
                let mut frozen = FrozenProvider::for_definition(
                    self.snapshot,
                    signature.definition,
                    self.citations,
                    &mut reads,
                );
                let mut probe = Provider {
                    inner: &mut frozen,
                    types: self.types,
                    facets: Vec::new(),
                    actual: actual.clone(),
                };
                if !probe.replay(&candidate.facets) || frozen.refused().is_some() {
                    continue;
                }
                let mut current = Provider {
                    inner: provider,
                    types: self.types,
                    facets: Vec::new(),
                    actual: actual.clone(),
                };
                if !current.replay(&candidate.facets) {
                    return Err(header_products::source_refusal(
                        "immutable structural provider changed while binding its result",
                    ));
                }
                let headers = headers.expect("structural cache owner");
                let table = checks.facts(headers.fact_graph());
                let result = table.get_or_try_init(
                    candidate.clone(),
                    || headers.validation_lease(candidate.bytes),
                    || {
                        current.facets.clear();
                        let result = checker.check(
                            self.types,
                            self.memo,
                            candidate.input.syntax.clone(),
                            candidate.input.resolution.clone(),
                            &candidate.input.signature,
                            &mut current,
                        );
                        if current.facets != candidate.facets {
                            return Err(header_products::source_refusal(
                                "structural recipe changed while producing its result",
                            ));
                        }
                        if retained_bytes(&candidate.input, &current.facets, &result)
                            > candidate.bytes
                        {
                            return Err(header_products::source_refusal(
                                "structural result exceeds its admitted recipe",
                            ));
                        }
                        result.map(Arc::new)
                    },
                    retain,
                );
                return current.restore(result.map(|output| (*output).clone()));
            }
        }
        let mut current = Provider {
            inner: provider,
            types: self.types,
            facets: Vec::new(),
            actual,
        };
        let result = checker.check(
            self.types,
            self.memo,
            syntax,
            resolution,
            &declared,
            &mut current,
        );
        let output = current.restore(result.clone());
        if let (Some((checks, _)), Some(input), Some(headers)) = (cache, input, headers)
            && (result.is_ok() || result.as_ref().is_err_and(|error| error.cacheable()))
            && !current
                .facets
                .iter()
                .any(|facet| matches!(facet, Facet::Signature(_, Err(error)) if !error.cacheable()))
        {
            current.facets.shrink_to_fit();
            let bytes = retained_bytes(&input, &current.facets, &result);
            let key = Arc::new(Key {
                input,
                facets: current.facets.clone(),
                bytes,
            });
            let result = checks.facts(headers.fact_graph()).get_or_try_init(
                key.clone(),
                || headers.validation_lease(bytes),
                || result.map(Arc::new),
                retain,
            );
            if retain(&result) {
                let mut recipes = checks.recipes.lock().unwrap();
                let variants = recipes.entry(signature.definition).or_default();
                variants.retain(|recipe| recipe.strong_count() != 0);
                if !variants
                    .iter()
                    .filter_map(Weak::upgrade)
                    .any(|old| *old == *key)
                {
                    variants.push(Arc::downgrade(&key));
                }
            }
            return current.restore(result.map(|output| (*output).clone()));
        }
        output
    }
}
fn retained_bytes(input: &Input, facets: &[Facet], result: &ResultBody) -> u64 {
    // Include a sparse recipe-tree root, minimum hash-table allocation, weak
    // recipe cells, pending row and Arc headers even for the first fact.
    let mut bytes =
        crate::memo::retained::retained_structural_bytes(&input.syntax, &input.resolution, result)
            .saturating_add(crate::memo::retained::retained_signature_payload_bytes(
                &input.signature,
            ))
            .saturating_add(
                (size_of::<Key>()
                    + size_of::<QueryOutput<TypedBody>>()
                    + 128 * size_of::<usize>()
                    + size_of_val(facets)) as u64,
            );
    for facet in facets {
        bytes = bytes.saturating_add(match facet {
            Facet::Signature(_, Ok(signature)) => {
                crate::memo::retained::retained_signature_payload_bytes(signature)
            }
            Facet::Signature(_, Err(error)) => {
                crate::memo::retained::retained_query_error_bytes(error)
            }
            Facet::Lookup(context, path, output) => {
                // Path/context ownership uses the same retained read visitor.
                crate::memo::retained::retained_structural_lookup_bytes(context, path, output)
            }
            Facet::Unique(path, _, failure) => {
                crate::memo::retained::retained_structural_path_bytes(path).saturating_add(
                    failure
                        .as_ref()
                        .map_or(0, |failure| failure.message.capacity() as u64),
                )
            }
            Facet::Name(path, _) => crate::memo::retained::retained_structural_path_bytes(path),
        });
    }
    bytes
}
