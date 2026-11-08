//! Demand-owned written siblings. Installing a graph retains declaration
//! metadata and the shared source owner, without encoding or decoding syntax.
use super::*;
use crate::snapshot::SiblingView;

#[derive(Default)]
pub(super) struct SiblingFile {
    live: Mutex<Option<SiblingView>>,
}
pub(super) struct SiblingSources {
    pub files: BTreeMap<FileId, SiblingFile>,
    pub namespace: Arc<NamespaceCapture>,
    pub backing: Arc<FrontSourceBacking>,
}
impl crate::snapshot::DeclarationSiblings for SiblingSources {
    fn siblings(&self, syntax: &ItemSyntax) -> Result<SiblingView, QueryError> {
        let refusal = |detail: &str| {
            QueryError::Operational(hirsel_types::OperationalFailure {
                operation: "front source input".into(),
                message: format!("{}: {detail}", syntax.item.definition),
            })
        };
        let file = self
            .namespace
            .sources
            .anchor(syntax.anchor)
            .and_then(|(file, _)| self.files.get(&file))
            .ok_or_else(|| refusal("missing sibling file inventory"))?;
        let mut live = file.live.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(view) = &*live {
            return Ok(view.clone());
        }
        let view = self.load(syntax)?;
        *live = Some(view.clone());
        Ok(view)
    }
}
impl SiblingSources {
    fn load(&self, syntax: &ItemSyntax) -> Result<SiblingView, QueryError> {
        let refused = |detail: &str| {
            QueryError::Operational(hirsel_types::OperationalFailure {
                operation: "front source input".into(),
                message: detail.into(),
            })
        };
        let headers = self
            .namespace
            .snapshot
            .header_products()
            .ok_or_else(|| refused("missing live header owner"))?;
        let items = self
            .namespace
            .source_siblings(syntax)?
            .iter()
            .filter(|sibling| {
                let variant = matches!(
                    &sibling.item.kind,
                    hirsel_syntax::ItemKind::Type {
                        body: hirsel_syntax::TypeBody::Variants(_),
                        ..
                    }
                );
                let value = matches!(
                    sibling.item.kind,
                    hirsel_syntax::ItemKind::Function { .. }
                        | hirsel_syntax::ItemKind::Const { .. }
                );
                let normalized = headers
                    .settled_signature(DefId::intern(&sibling.item.definition))
                    .is_some_and(|signature| {
                        matches!(
                            signature.kind,
                            hirsel_types::SignatureKind::Function(_)
                                | hirsel_types::SignatureKind::Const { .. }
                        )
                    });
                variant || (value && headers.needs_written_signatures() && !normalized)
            })
            .cloned()
            .collect::<Vec<_>>();
        self.admit(items).map_err(refused)
    }
    fn admit(&self, items: Vec<Arc<ItemSyntax>>) -> Result<SiblingView, &'static str> {
        let metadata = hirsel_engine_admission::retained_arc_bytes(items.as_slice())
            .checked_add(hirsel_engine_admission::retained_arc_bytes(
                &std::mem::MaybeUninit::<LiveSourceBacking>::uninit(),
            ))
            .and_then(|bytes| {
                bytes.checked_add(
                    hirsel_engine_admission::retained_arc_bytes(&std::mem::MaybeUninit::<(
                        Arc<NamespaceCapture>,
                        Arc<LiveSourceBacking>,
                    )>::uninit()),
                )
            })
            .ok_or("sibling memory size overflow")?;
        let bytes = items
            .iter()
            .filter(|syntax| !self.namespace.retains_written(syntax))
            .try_fold(metadata, |bytes, syntax| {
                bytes.checked_add(hirsel_query_products::retained::retained_syntax_bytes(
                    syntax,
                ))
            })
            .ok_or("sibling memory size overflow")?;
        let allowance = self
            .backing
            .request
            .reserve(bytes)
            .map_err(|_| "sibling memory allowance exhausted")?;
        let backing = Arc::new(LiveSourceBacking {
            _source: self.backing.clone(),
            debit: Mutex::new(FrontDebit {
                charge: bytes,
                allowance,
            }),
        });
        Ok(SiblingView::retained(
            items.into(),
            Arc::new((self.namespace.clone(), backing)),
        ))
    }
}

/// Distinct immutable source/header backing and front reference/slot backing.
/// The host charges shared source_headers once, alongside the selected S0
/// snapshot; front_inputs is the minimum producer reservation, before output metadata.
/// Archive byte ownership remains with the selected provider.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrontInputFootprint {
    pub source_headers: u64,
    pub front_inputs: u64,
}
impl FrontInputFootprint {
    pub fn total(self) -> Option<u64> {
        self.source_headers.checked_add(self.front_inputs)
    }
}
pub(super) fn sibling_input_bytes(files: usize) -> Option<u64> {
    let mut weight = hirsel_query_products::retained::MetadataWeight::default();
    weight.tree::<FileId, SiblingFile>(files);
    weight.add(hirsel_engine_admission::arc_bytes(
        &std::mem::MaybeUninit::<SiblingSources>::uninit(),
    ));
    Some(weight.bytes())
}
impl QueryEngine {
    pub(super) fn front_sibling_files(&self) -> usize {
        self.sources
            .anchors()
            .map(|(_, file, _)| file)
            .collect::<BTreeSet<_>>()
            .len()
    }
    /// Bound front backing from indexed metadata. Planning must not demand
    /// syntax or freeze declarations. Source owners use their existing
    /// immutable indexed product, including its path allocations.
    pub fn front_input_footprint(
        &self,
        definitions: &[DefPath],
    ) -> Result<FrontInputFootprint, QueryError> {
        let refused = || {
            QueryError::Operational(hirsel_types::OperationalFailure {
                operation: "front input admission".into(),
                message: "source input size overflow or missing syntax archive".into(),
            })
        };
        if self.result_store.is_none() {
            return Err(refused());
        }
        let sibling_bytes = sibling_input_bytes(self.front_sibling_files()).ok_or_else(refused)?;
        let mut instance_weight = hirsel_query_products::retained::MetadataWeight::default();
        instance_weight
            .instance_bodies(&mut self.instance_bodies(&definitions.iter().cloned().collect()));
        let roots = self.source_root_ids();
        let source_rows = self
            .source_paths_iter()
            .count()
            .checked_add(roots.len())
            .and_then(|count| count.checked_mul(size_of::<DefId>()))
            .ok_or_else(refused)?;
        let owner_bytes = crate::NativeRecordContext::capture(self)
            .planning_bytes()
            .ok_or_else(refused)?;
        let front_inputs = FrontBodyQueries::input_bytes(definitions)
            .and_then(|bytes| bytes.checked_add(owner_bytes))
            .and_then(|bytes| bytes.checked_add(instance_weight.bytes()))
            .and_then(|bytes| {
                bytes.checked_add(header_products::HeaderProducts::written_source_bytes(self))
            })
            .and_then(|bytes| bytes.checked_add(source_rows as u64))
            .and_then(|bytes| bytes.checked_add(sibling_bytes))
            .and_then(|bytes| {
                bytes.checked_add(header_products::SourceInputs::metadata_bytes(self)?)
            })
            .ok_or_else(refused)?;
        let source_headers = self
            .sources
            .input_bytes()
            .ok_or_else(refused)?
            .checked_add(self.declarations.observation_bytes().ok_or_else(refused)?)
            .ok_or_else(refused)?
            .checked_add(self.catalog.input_bytes().ok_or_else(refused)?)
            .ok_or_else(refused)?
            .checked_add(owner_bytes)
            .ok_or_else(refused)?
            .checked_add(header_products::HeaderProducts::source_fact_index_bytes(
                self,
                self.adapters
                    .modes
                    .as_ref()
                    .is_some_and(|modes| modes.infers_private_unique()),
            ))
            .ok_or_else(refused)?;
        Ok(FrontInputFootprint {
            source_headers,
            front_inputs,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DefinitionQueries, snapshot::DeclarationSiblings};

    fn parsed() -> (QueryEngine, FileId, Vec<Arc<ItemSyntax>>) {
        let mut engine = QueryEngine::new(
            QueryEnvironment {
                interface_version: 1,
                compiler_version: "sibling-decode".into(),
                spec_hash: Default::default(),
                package_manifest_hash: Default::default(),
                builtin_signatures_hash: Default::default(),
            },
            crate::phases::engine_only_adapters(),
        );
        let file = engine
            .sources
            .add(
                "siblings.hir",
                "type Choice = One | Two\nconst value: u8 = 1u8\npub fn answer() -> u8 { value }\n"
                    .into(),
            )
            .unwrap();
        let parsed = engine
            .parse_file(
                file,
                &FileContext {
                    package: hirsel_syntax::PackageId {
                        name: "siblings".into(),
                        version: "0".into(),
                    },
                    modules: vec![],
                },
            )
            .unwrap();
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let written = engine
            .frozen_declarations()
            .siblings(&engine.items.values().next().unwrap()[0]);
        assert_eq!(written.len(), 3);
        (engine, file, written.to_vec())
    }
    fn fixture() -> (SiblingSources, Vec<Arc<ItemSyntax>>) {
        let (engine, file, written) = parsed();
        let mut engine =
            engine.with_result_store(Arc::new(hirsel_engine_store::MemStore::default()));
        engine.adapters.lexer = None;
        engine.adapters.parser = None;
        engine.adapters.signature = None;
        let store = engine.result_store.clone().unwrap();
        let owners = Arc::new(crate::NativeRecordContext::capture(&engine));
        let records = crate::NativeRecords::new(engine.types.clone(), store);
        let definitions = written
            .iter()
            .map(|syntax| syntax.item.definition.clone())
            .collect::<Vec<_>>();
        let namespace = engine
            .capture_namespace(&definitions, &[], 8 << 20)
            .unwrap();
        // The partition holds the inputs; sibling views are charged to the
        // rest of the request's process budget.
        let budget = hirsel_engine_admission::ProcessBudget::new(16 << 20);
        let mut allowance = budget.reserve(8 << 20).unwrap();
        let products =
            ProductSource::admit_shared((*owners).clone(), records.clone(), &mut allowance)
                .unwrap();
        let backing = Arc::new(FrontSourceBacking {
            products,
            admitted: allowance.bytes(),
            request: budget.clone(),
            _inputs: allowance.split(0).unwrap(),
            allowance: Arc::new(
                hirsel_engine_admission::MemoryReservationPool::new(allowance)
                    .ok()
                    .unwrap(),
            ),
            _sources: engine.sources.clone(),
        });
        (
            SiblingSources {
                files: BTreeMap::from([(file, SiblingFile::default())]),
                namespace,
                backing,
            },
            written,
        )
    }
    #[test]
    fn live_siblings_share_once_for_repeated_and_concurrent_lookups() {
        for jobs in [1, 8] {
            let (sources, written) = fixture();
            let barrier = std::sync::Barrier::new(jobs);
            let held = std::thread::scope(|scope| {
                let handles: Vec<_> = (0..jobs)
                    .map(|_| {
                        scope.spawn(|| {
                            barrier.wait();
                            let mut result = None;
                            for lookup in 0..32 {
                                let siblings =
                                    sources.siblings(&written[lookup % written.len()]).unwrap();
                                assert_eq!(siblings.len(), written.len());
                                for (actual, expected) in siblings.iter().zip(&written) {
                                    assert!(Arc::ptr_eq(actual, expected));
                                }
                                result = Some(siblings);
                            }
                            result.unwrap()
                        })
                    })
                    .collect();
                let results: Vec<_> = handles
                    .into_iter()
                    .map(|handle| handle.join().unwrap())
                    .collect();
                assert!(results.iter().all(|items| items.same_items(&results[0])));
                results
            });
            drop(sources);
            drop(held);
        }
    }
    #[test]
    fn live_siblings_reuse_owned_sources_without_a_temporary_engine() {
        let (sources, written) = fixture();
        let assemblies = namespace::SOURCE_ENGINES.get();
        let items = sources.siblings(&written[0]).unwrap();
        assert_eq!(
            items
                .iter()
                .map(|item| &item.item.definition)
                .collect::<Vec<_>>(),
            written
                .iter()
                .map(|item| &item.item.definition)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            namespace::SOURCE_ENGINES.get(),
            assemblies,
            "owned syntax needs no temporary source engine"
        );
    }
    #[test]
    fn live_siblings_admit_syntax_derived_from_a_captured_declaration() {
        // Shaped as generation/syntax.rs Literal::finish and parse_literal make
        // a `= comptime` body and its normalized header: the written anchor,
        // a rehashed body or a renamed definition. Both stand in the captured
        // file, so they read its written siblings.
        let definitions = |items: &[Arc<ItemSyntax>]| {
            items
                .iter()
                .map(|item| item.item.definition.clone())
                .collect::<Vec<_>>()
        };
        for generated in [true, false] {
            let (sources, written) = fixture();
            let mut derived = (*written[2]).clone();
            if generated {
                derived.item.generated = Some(hirsel_syntax::Generated::Body(vec![]));
                derived.text_hash = hirsel_syntax::ContentHash::of_parts(
                    "hirsel.generated-item.v1",
                    [written[2].text_hash.0.as_slice(), b"fn() -> u8 { 1u8 }"],
                );
            } else {
                derived.item.definition.0.name = "answer$header".into();
                derived.occurrence = 0;
            }
            let items = sources
                .siblings(&derived)
                .expect("derived syntax reads its declaration's siblings");
            assert_eq!(definitions(&items), definitions(&written));
        }

        let (sources, written) = fixture();
        let mut changed = (*written[2]).clone();
        changed.text_hash = hirsel_syntax::ContentHash::of_bytes(b"pub fn answer() -> u8 { 2u8 }");
        assert!(matches!(
            sources.siblings(&changed),
            Err(QueryError::Operational(failure)) if failure.message.contains("captured sibling binding changed")
        ));
    }
}
