//! Ordinary syntax archives for a demanded file's written sibling headers.
//! Front admission captures validated sibling transport once. Lookup shares
//! that immutable input; no source parser or compiler phase is called here.
use super::*;
use std::sync::OnceLock;

#[derive(Default)]
pub(super) struct SiblingFile {
    references: Vec<Reference>,
    decoded: OnceLock<Result<DecodedSiblings, &'static str>>,
}
impl From<Vec<Reference>> for SiblingFile {
    fn from(references: Vec<Reference>) -> Self {
        Self {
            references,
            decoded: OnceLock::new(),
        }
    }
}
struct DecodedSiblings {
    items: Arc<[Arc<ItemSyntax>]>,
    _backing: Arc<LiveSourceBacking>,
}
pub(super) struct SiblingArchives {
    pub files: BTreeMap<FileId, SiblingFile>,
    pub sources: SourceMap,
    pub owners: Arc<crate::NativeRecordContext>,
    pub records: crate::NativeRecords,
    pub store: Arc<dyn ResultStore>,
    pub backing: Arc<FrontSourceBacking>,
    pub retention: Arc<Mutex<u64>>,
}
impl crate::snapshot::DeclarationSiblings for SiblingArchives {
    fn siblings(&self, syntax: &ItemSyntax) -> Result<Arc<[Arc<ItemSyntax>]>, QueryError> {
        let refusal = |detail: &str| {
            QueryError::Operational(hirsel_types::OperationalFailure {
                operation: "front source transport".into(),
                message: format!("{}: {detail}", syntax.item.definition),
            })
        };
        let file = self
            .sources
            .anchor(syntax.anchor)
            .ok_or_else(|| refusal("missing source anchor"))?
            .0;
        let file = self
            .files
            .get(&file)
            .ok_or_else(|| refusal("missing sibling file inventory"))?;
        // The provider belongs to this immutable front request. OnceLock
        // serializes first use without publishing a partially validated list.
        // A refusal also stays local to this request, never a query-store hit.
        file.decoded
            .get_or_init(|| self.decode(&file.references))
            .as_ref()
            .map(|decoded| decoded.items.clone())
            .map_err(|detail| refusal(detail))
    }
}
impl SiblingArchives {
    fn decode(&self, references: &[Reference]) -> Result<DecodedSiblings, &'static str> {
        let items: Arc<[Arc<ItemSyntax>]> = references
            .iter()
            .map(|reference| {
                let bytes = self
                    .store
                    .get_content(*reference)
                    .ok_or("missing sibling content")?;
                let (mut syntax, occurrence): (ItemSyntax, u32) = self
                    .records
                    .decode(&*self.owners, &bytes)
                    .ok_or("invalid sibling codec")?;
                let (owner, base) = self
                    .sources
                    .anchor(syntax.anchor)
                    .ok_or("missing sibling anchor")?;
                let text = self
                    .sources
                    .file(owner)
                    .ok_or("missing sibling source")?
                    .text();
                // Structural validation also checks that every span uses this
                // item's anchor. Resolve its file/base once for the whole tree.
                if !syntax.structurally_valid(|span| {
                    text.get(
                        base.saturating_add(span.start) as usize
                            ..base.saturating_add(span.end) as usize,
                    )
                    .is_some()
                }) {
                    return Err("invalid sibling spans");
                }
                syntax.occurrence = occurrence;
                Ok(Arc::new(syntax))
            })
            .collect::<Result<Vec<_>, _>>()?
            .into();
        let bytes = items
            .iter()
            .try_fold(
                crate::memo::retained_arc_bytes(items.as_ref()),
                |bytes, syntax| {
                    bytes.checked_add(crate::memo::retained::retained_syntax_bytes(syntax))
                },
            )
            .and_then(|bytes| {
                bytes.checked_add(crate::memo::retained_arc_bytes(&std::mem::MaybeUninit::<
                    LiveSourceBacking,
                >::uninit(
                )))
            })
            .ok_or("sibling memory size overflow")?;
        let mut remaining = self.retention.lock().unwrap_or_else(|e| e.into_inner());
        let next = remaining
            .checked_sub(bytes)
            .ok_or("sibling memory allowance exhausted")?;
        let allowance = self
            .backing
            .allowance
            .take(bytes)
            .ok_or("sibling memory allowance exhausted")?;
        *remaining = next;
        drop(remaining);
        Ok(DecodedSiblings {
            items,
            _backing: Arc::new(LiveSourceBacking {
                _source: self.backing.clone(),
                retention: self.retention.clone(),
                debit: Mutex::new(FrontDebit {
                    charge: bytes,
                    allowance,
                    observation: None,
                }),
            }),
        })
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
impl QueryEngine {
    pub(super) fn front_sibling_inputs(
        &self,
        definitions: &[DefPath],
    ) -> Result<BTreeMap<FileId, Arc<ItemSyntax>>, QueryError> {
        let mut files = BTreeMap::new();
        for definition in definitions {
            let syntax = self.snapshot(definition)?;
            let file = self
                .sources
                .anchor(syntax.anchor)
                .ok_or(QueryError::NotImplemented)?
                .0;
            files.entry(file).or_insert(syntax);
        }
        // Private generator constants share their declaration's written file.
        // That file can be absent from the ordinary P inputs during bootstrap.
        for syntax in self.items.values().flatten() {
            if generation::generator(syntax).is_some() {
                let file = self
                    .sources
                    .anchor(syntax.anchor)
                    .ok_or(QueryError::NotImplemented)?
                    .0;
                files.entry(file).or_insert_with(|| syntax.clone());
            }
        }
        Ok(files)
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
        let (files, siblings) = self.declarations.sibling_inventory();
        // Lazy files already own declaration heads and source anchors. Their
        // eventual syntax capture cannot require more rows than that index.
        let (lazy_files, lazy_siblings) = self.catalog.sibling_inventory().ok_or_else(refused)?;
        let files = files.checked_add(lazy_files).ok_or_else(refused)?;
        let siblings = siblings.checked_add(lazy_siblings).ok_or_else(refused)?;
        let mut index = crate::NativeMetadataWeight::default();
        index.tree::<FileId, SiblingFile>(files);
        let mut instance_weight = crate::NativeMetadataWeight::default();
        instance_weight
            .instance_bodies(&mut self.instance_bodies(&definitions.iter().cloned().collect()));
        let source_rows = self
            .source_paths()
            .len()
            .checked_mul(3)
            .and_then(|count| count.checked_mul(size_of::<DefId>()))
            .ok_or_else(refused)?;
        let front_inputs = FrontBodyQueries::input_bytes(definitions)
            .and_then(|bytes| bytes.checked_add(instance_weight.bytes()))
            .and_then(|bytes| bytes.checked_add(source_rows as u64))
            .and_then(|bytes| bytes.checked_add(index.bytes()))
            .and_then(|bytes| {
                bytes.checked_add(
                    (siblings as u64).checked_mul(std::mem::size_of::<Reference>() as u64)?,
                )
            })
            .ok_or_else(refused)?;
        let owner_bytes = crate::NativeRecordContext::capture(self)
            .input_bytes()
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct CountingStore {
        inner: crate::store::MemStore,
        reads: AtomicUsize,
    }
    impl ResultStore for CountingStore {
        fn name(&self) -> &'static str {
            "counting-sibling-archives"
        }
        fn get(&self, key: &EntryKey) -> Option<crate::store::Bytes> {
            self.inner.get(key)
        }
        fn put(&self, key: &EntryKey, bytes: &[u8]) {
            self.inner.put(key, bytes);
        }
        fn get_content(&self, reference: Reference) -> Option<crate::store::Bytes> {
            if reference.domain == SOURCE_INPUT {
                self.reads.fetch_add(1, Ordering::Relaxed);
            }
            self.inner.get_content(reference)
        }
    }
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
    fn fixture() -> (SiblingArchives, Vec<Arc<ItemSyntax>>, Arc<CountingStore>) {
        let (engine, file, written) = parsed();
        let store = Arc::new(CountingStore::default());
        let owners = Arc::new(crate::NativeRecordContext::capture(&engine));
        let records = crate::NativeRecords::new(engine.types.clone(), store.clone());
        let references: Vec<_> = written
            .iter()
            .map(|syntax| {
                let bytes = records
                    .encode(&*owners, &(syntax, syntax.occurrence))
                    .unwrap();
                store.put_content(SOURCE_INPUT, &bytes).unwrap()
            })
            .collect();
        let budget = ProcessBudget::new(8 << 20);
        let mut allowance = budget.reserve(8 << 20).unwrap();
        let products =
            ProductSource::admit_shared((*owners).clone(), records.clone(), &mut allowance)
                .unwrap();
        let retention = Arc::new(Mutex::new(allowance.bytes()));
        let backing = Arc::new(FrontSourceBacking {
            products,
            admitted: allowance.bytes(),
            allowance: MemoryReservationPool::new(allowance).ok().unwrap(),
            _sources: engine.sources.clone(),
            headers: None,
        });
        (
            SiblingArchives {
                files: BTreeMap::from([(file, references.into())]),
                sources: engine.sources.clone(),
                owners,
                records,
                store: store.clone(),
                backing,
                retention,
            },
            written,
            store,
        )
    }
    #[test]
    fn admitted_siblings_are_charged_before_compact_output_accounting() {
        let (engine, _, written) = parsed();
        let store = Arc::new(CountingStore::default());
        let mut engine = engine.with_result_store(store.clone());
        let definitions = written
            .iter()
            .map(|syntax| syntax.item.definition.clone())
            .collect::<Vec<_>>();
        let budget = ProcessBudget::new(16 << 20);
        let error = SOURCE_INPUT.reference(b"unused fixture error").unwrap();
        let front = engine
            .front_body_queries(
                &definitions,
                MemoryWeight {
                    retained: 1 << 20,
                    scratch: 8 << 20,
                    stack: 0,
                },
                budget.reserve(8 << 20).unwrap(),
                RuntimeErrors {
                    resource: error,
                    cycle: error,
                    cancelled: error,
                    protocol: error,
                    panicked: error,
                },
            )
            .unwrap();
        let initial = front.retained_bytes();
        assert_eq!(store.reads.load(Ordering::Relaxed), written.len());
        for syntax in &written {
            assert_eq!(
                front.declarations.try_siblings(syntax).unwrap().len(),
                written.len()
            );
            assert_eq!(
                front.retained_bytes(),
                initial,
                "sibling input bytes must not become compact observations"
            );
        }
        assert_eq!(store.reads.load(Ordering::Relaxed), written.len());
    }
    #[test]
    fn archived_siblings_decode_once_for_repeated_and_concurrent_lookups() {
        for jobs in [1, 8] {
            let (archives, written, store) = fixture();
            let barrier = std::sync::Barrier::new(jobs);
            std::thread::scope(|scope| {
                let handles: Vec<_> = (0..jobs)
                    .map(|_| {
                        scope.spawn(|| {
                            barrier.wait();
                            let mut result = None;
                            for lookup in 0..32 {
                                let siblings =
                                    archives.siblings(&written[lookup % written.len()]).unwrap();
                                assert_eq!(siblings.len(), written.len());
                                for (actual, expected) in siblings.iter().zip(&written) {
                                    let encode = |syntax: &Arc<ItemSyntax>| {
                                        archives
                                            .records
                                            .encode(&*archives.owners, &(syntax, syntax.occurrence))
                                            .unwrap()
                                    };
                                    assert_eq!(encode(actual), encode(expected));
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
                assert_eq!(
                    store.reads.load(Ordering::Relaxed),
                    written.len(),
                    "each sibling must be retrieved and decoded once at jobs={jobs}"
                );
                assert!(results.iter().all(|items| Arc::ptr_eq(items, &results[0])));
            });
        }
    }
    #[test]
    fn archived_siblings_validate_before_publication_and_remember_refusals() {
        for damage in ["missing", "codec", "spans"] {
            let (mut archives, written, store) = fixture();
            let reference = match damage {
                "missing" => SOURCE_INPUT.reference(b"absent sibling").unwrap(),
                "codec" => store.put_content(SOURCE_INPUT, b"invalid codec").unwrap(),
                "spans" => {
                    let mut invalid = (*written[1]).clone();
                    invalid.item.name_span.end = u32::MAX;
                    let bytes = archives
                        .records
                        .encode(&*archives.owners, &(&invalid, invalid.occurrence))
                        .unwrap();
                    store.put_content(SOURCE_INPUT, &bytes).unwrap()
                }
                _ => unreachable!(),
            };
            archives.files.values_mut().next().unwrap().references[1] = reference;
            let before = *archives.retention.lock().unwrap();
            for syntax in &written {
                let error = archives.siblings(syntax).unwrap_err();
                let QueryError::Operational(error) = error else {
                    panic!("expected transport refusal")
                };
                assert!(
                    error.message.contains(match damage {
                        "missing" => "missing sibling content",
                        "codec" => "invalid sibling codec",
                        "spans" => "invalid sibling spans",
                        _ => unreachable!(),
                    }),
                    "{error:?}"
                );
                assert!(
                    error
                        .message
                        .starts_with(&syntax.item.definition.to_string())
                );
            }
            assert_eq!(store.reads.load(Ordering::Relaxed), 2);
            assert_eq!(*archives.retention.lock().unwrap(), before);
        }
    }
    #[test]
    fn archived_siblings_charge_the_partition_and_release_it_with_the_request() {
        let (archives, written, store) = fixture();
        let retention = archives.retention.clone();
        let backing = archives.backing.clone();
        let before = *retention.lock().unwrap();
        let available = backing.allowance.available_bytes();
        let items = archives.siblings(&written[0]).unwrap();
        let charged = before - *retention.lock().unwrap();
        assert!(charged > crate::memo::retained::retained_syntax_bytes(&written[0]));
        assert_eq!(backing.allowance.available_bytes(), available - charged);
        drop(items);
        assert_eq!(*retention.lock().unwrap(), before - charged);
        drop(archives);
        assert_eq!(*retention.lock().unwrap(), before);
        assert_eq!(backing.allowance.available_bytes(), available);
        assert_eq!(store.reads.load(Ordering::Relaxed), 3);

        let (archives, written, store) = fixture();
        *archives.retention.lock().unwrap() = 0;
        for _ in 0..2 {
            let error = archives.siblings(&written[0]).unwrap_err();
            assert!(
                matches!(error, QueryError::Operational(error) if error.message.contains("sibling memory allowance exhausted"))
            );
        }
        assert_eq!(store.reads.load(Ordering::Relaxed), 3);
        assert_eq!(*archives.retention.lock().unwrap(), 0);
    }
}
