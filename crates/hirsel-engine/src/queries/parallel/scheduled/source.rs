//! Demand-owned written siblings. Installing a graph retains declaration
//! metadata and the shared source owner, without encoding or decoding syntax.
use super::*;
use std::sync::OnceLock;

#[derive(Default)]
pub(super) struct SiblingFile {
    live: OnceLock<Result<LiveSiblings, QueryError>>,
}
struct LiveSiblings {
    items: Arc<[Arc<ItemSyntax>]>,
    _backing: Arc<LiveSourceBacking>,
}
pub(super) struct SiblingSources {
    pub files: BTreeMap<FileId, SiblingFile>,
    pub namespace: Arc<NamespaceCapture>,
    pub backing: Arc<FrontSourceBacking>,
    pub retention: Arc<Mutex<u64>>,
}
impl crate::snapshot::DeclarationSiblings for SiblingSources {
    fn siblings(&self, syntax: &ItemSyntax) -> Result<Arc<[Arc<ItemSyntax>]>, QueryError> {
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
        file.live
            .get_or_init(|| self.load(syntax))
            .as_ref()
            .map(|live| live.items.clone())
            .map_err(Clone::clone)
    }
}
impl SiblingSources {
    fn load(&self, syntax: &ItemSyntax) -> Result<LiveSiblings, QueryError> {
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
        let mut engine = headers.execution();
        engine.materialize(&syntax.item.definition)?;
        let declarations = engine.frozen_declarations();
        let items = declarations.siblings(syntax).iter().filter(|sibling| {
            let variant = matches!(&sibling.item.kind, hirsel_syntax::ItemKind::Type {
                body: hirsel_syntax::TypeBody::Variants(_), ..
            });
            let value = matches!(sibling.item.kind, hirsel_syntax::ItemKind::Function { .. } | hirsel_syntax::ItemKind::Const { .. });
            let normalized = headers.settled_signature(DefId::intern(&sibling.item.definition))
                .is_some_and(|signature| matches!(signature.kind, hirsel_types::SignatureKind::Function(_) | hirsel_types::SignatureKind::Const { .. }));
            variant || (value && !normalized)
        }).cloned().collect::<Vec<_>>().into();
        self.admit(items).map_err(refused)
    }
    fn admit(&self, items: Arc<[Arc<ItemSyntax>]>) -> Result<LiveSiblings, &'static str> {
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
        Ok(LiveSiblings {
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
            .and_then(|bytes| bytes.checked_add(header_products::HeaderProducts::written_source_bytes(self)))
            .and_then(|bytes| bytes.checked_add(source_rows as u64))
            .and_then(|bytes| bytes.checked_add(index.bytes()))
            .and_then(|bytes| {
                bytes.checked_add(
                    (siblings as u64).checked_mul(std::mem::size_of::<Arc<ItemSyntax>>() as u64)?,
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
        let mut engine = engine.with_result_store(Arc::new(crate::store::MemStore::default()));
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
        let budget = ProcessBudget::new(8 << 20);
        let mut allowance = budget.reserve(8 << 20).unwrap();
        let products =
            ProductSource::admit_shared((*owners).clone(), records.clone(), &mut allowance)
                .unwrap();
        let retention = Arc::new(Mutex::new(allowance.bytes()));
        let backing = Arc::new(FrontSourceBacking {
            products,
            admitted: allowance.bytes(),
            _inputs: allowance.split(0).unwrap(),
            allowance: Arc::new(MemoryReservationPool::new(allowance).ok().unwrap()),
            _sources: engine.sources.clone(),
            headers: None,
        });
        (
            SiblingSources {
                files: BTreeMap::from([(file, SiblingFile::default())]),
                namespace,
                backing,
                retention,
            },
            written,
        )
    }
    #[test]
    fn live_siblings_share_once_for_repeated_and_concurrent_lookups() {
        for jobs in [1, 8] {
            let (sources, written) = fixture();
            let barrier = std::sync::Barrier::new(jobs);
            std::thread::scope(|scope| {
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
                assert!(results.iter().all(|items| Arc::ptr_eq(items, &results[0])));
            });
        }
    }
    #[test]
    fn live_siblings_charge_the_partition_and_release_it_with_the_request() {
        let (sources, written) = fixture();
        let retention = sources.retention.clone();
        let backing = sources.backing.clone();
        let before = *retention.lock().unwrap();
        let available = backing.allowance.available_bytes();
        let items = sources.siblings(&written[0]).unwrap();
        let charged = before - *retention.lock().unwrap();
        assert!(charged > crate::memo::retained::retained_syntax_bytes(&written[0]));
        assert_eq!(backing.allowance.available_bytes(), available - charged);
        drop(items);
        assert_eq!(*retention.lock().unwrap(), before - charged);
        drop(sources);
        assert_eq!(*retention.lock().unwrap(), before);
        assert_eq!(backing.allowance.available_bytes(), available);

        let (sources, written) = fixture();
        *sources.retention.lock().unwrap() = 0;
        for _ in 0..2 {
            let error = sources.siblings(&written[0]).unwrap_err();
            assert!(
                matches!(error, QueryError::Operational(error) if error.message.contains("sibling memory allowance exhausted"))
            );
        }
        assert_eq!(*sources.retention.lock().unwrap(), 0);
    }
}
