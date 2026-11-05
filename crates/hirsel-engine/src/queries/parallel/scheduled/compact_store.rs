//! Ordinary body-free P projections use the same canonical facts as hot carry.
//! Source text and all reads must still match; Lower retains independent V.
use super::*;
use bincode::Options;

#[derive(serde::Serialize, serde::Deserialize)]
struct Record {
    key: Key,
    body: Option<Reference>,
    reads: Reads,
    decode_bytes: u64,
    #[serde(
        serialize_with = "hirsel_product_codec::serialize_payload",
        deserialize_with = "hirsel_product_codec::deserialize_payload"
    )]
    facts: Vec<u8>,
}
#[derive(serde::Serialize)]
struct RecordRef<'a> {
    key: Key,
    body: Option<Reference>,
    reads: &'a Reads,
    decode_bytes: u64,
    #[serde(serialize_with = "hirsel_product_codec::serialize_payload")]
    facts: &'a [u8],
}
fn address(key: Key) -> hirsel_artifact_store::Address {
    hirsel_artifact_store::Address(ContentHash::of_parts(
        "hirsel.front.compact.address.v1",
        [key.0.as_slice()],
    ))
}
fn storage_key(key: &EntryKey) -> EntryKey {
    EntryKey(address(key.0).0, key.1, QueryKind::Summary)
}
fn load_record(
    store: &dyn ResultStore,
    key: &EntryKey,
    scratch: u64,
    observe: impl FnMut(&ReadKey) -> Option<ContentHash>,
) -> Option<(crate::store::Variant, u64)> {
    let key = storage_key(key);
    let bytes = store.get(&key)?;
    let index_bytes = (bytes.len() as u64).checked_mul(8)?;
    let remaining = scratch.checked_sub(index_bytes)?;
    let mut entry = crate::store::Entry::decode_for(&key, &bytes)?;
    entry.variants.retain(|variant| {
        variant
            .output
            .len
            .checked_mul(8)
            .is_some_and(|bytes| bytes <= remaining)
    });
    entry
        .resolve_current(store, &key, observe)
        .map(|variant| (variant, index_bytes))
}
/// Scalar Summary demand consumes the same checked compact facts written by
/// scheduled P. It does not reconstruct or execute the separate body product.
pub(in crate::queries) fn load_publication(
    engine: &mut QueryEngine,
    path: &DefPath,
) -> Option<PublishedResult> {
    if engine.verify_store == crate::store::VerifyStore::Recompute {
        return None;
    }
    let store = engine
        .result_store
        .clone()
        .filter(|s| s.persistence().retains())?;
    let key = engine.store_key(path, QueryKind::Modes)?;
    let (variant, index_bytes) = load_record(
        &*store,
        &key,
        (crate::store::MAX_RECORD as u64).checked_mul(8)?,
        |read| engine.observe(path, read),
    )?;
    let record: Record = bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(variant.payload.len() as u64)
        .reject_trailing_bytes()
        .deserialize(&variant.payload)
        .ok()?;
    if record.key != key.0
        || record.body.is_some_and(|body| {
            body.domain != crate::store::product_domain(QueryKind::Modes)
                || body.len > crate::store::MAX_RECORD as u64
        })
        || (variant.payload.len() as u64)
            .checked_mul(8)?
            .checked_add(record.decode_bytes)?
            .checked_add(index_bytes)?
            > (crate::store::MAX_RECORD as u64).checked_mul(8)?
        || record.reads != variant.reads
    {
        return None;
    }
    let facts: carry::Facts = engine.decode_record(&record.facts)?;
    let summary = facts.summary?;
    if summary.storage.is_some() || has_errors(&facts.diagnostics) {
        return None;
    }
    let checked = facts.checked?;
    engine.adopt_publication(
        path,
        persistent::PublicationRecord {
            summary,
            diagnostics: facts.diagnostics,
            phases: facts.diagnostic_phases,
            detail: DependencyReads {
                signatures: checked.2.values()?,
                ..Default::default()
            },
            dead_arms: facts.dead_arms,
            lowering: facts.lowering,
        },
        Arc::new(record.reads),
        (checked.0, checked.1),
    )
}

impl FrontBodyQueries {
    pub(super) fn archive_facts(
        &self,
        key: Key,
        outcome: &FrontOutcome,
        existing: Option<Arc<FrontObservations>>,
        scratch: u64,
    ) -> Option<ArchivedFront> {
        let observations = if let Some(existing) = existing {
            existing
        } else {
            self.archive_observations(outcome, scratch)?
        };
        // Replace the live terminal's charge with its compact representation.
        // This consumes no second required-transport record. If compaction
        // cannot fit, leave the live terminal in place and refuse admission.
        self.store
            .archive_encoded(crate::store::ArchiveBoundary::RequiredSpill);
        let bytes = self.encode_terminal(key, outcome, scratch)?;
        let charge = (bytes.len() as u64)
            .checked_add(crate::memo::retained_arc_bytes(&std::mem::MaybeUninit::<
                TerminalFront,
            >::uninit()))?
            .checked_add(crate::memo::retained_arc_bytes(&std::mem::MaybeUninit::<
                LiveSourceBacking,
            >::uninit()))?;
        let backing = outcome._backing.split(charge)?;
        Some(ArchivedFront {
            facts: Some(Arc::new(TerminalFront {
                bytes: bytes.into_boxed_slice(),
                _backing: backing,
            })),
            observations,
        })
    }
    fn encode_terminal(&self, key: Key, outcome: &FrontOutcome, scratch: u64) -> Option<Vec<u8>> {
        let temporary = outcome
            .dead_arms
            .as_ref()
            .map_or(0, LintFacts::materialize_bytes)
            .checked_add(crate::memo::retained_arc_bytes(&std::mem::MaybeUninit::<
                PublishedSummary,
            >::uninit()))?;
        if temporary > scratch {
            return None;
        }
        let dead_arms = outcome.dead_arms.as_ref().map(LintFacts::materialize);
        let (summary, diagnostics, lowering, archive) = match &outcome.state {
            FrontState::Failed { .. } => return None,
            FrontState::Settled {
                archive,
                publication,
                diagnostics,
                lowering,
                ..
            } => (
                Some(publication.summary_value()?.value_arc()),
                diagnostics,
                Some(lowering),
                *archive,
            ),
        };
        let facts = carry::FactsRef {
            summary,
            checked: &outcome.checked,
            dead_arms: &dead_arms,
            diagnostics,
            diagnostic_phases: &outcome.diagnostic_phases,
            lowering,
            table_keys: outcome.table_keys.as_ref(),
        };
        drop(self.records.prepare_record_encoding(
            &facts,
            |anchor| self.namespace.anchors.record_owner_ref(anchor),
            scratch,
            temporary,
        )?);
        let (facts_bytes, outer) = self
            .records
            .encode_record_with_decode_bytes(&facts, |anchor| {
                self.citations.record_owner(anchor)
            })?;
        let decode_bytes = outer.checked_add(facts.bytes(true))?;
        let record = RecordRef {
            key: self.inputs.get(&key)?.key.0,
            body: archive,
            reads: &outcome.reads,
            decode_bytes,
            facts: &facts_bytes,
        };
        if bincode::serialized_size(&record)
            .ok()?
            .checked_mul(8)?
            .checked_add(decode_bytes)?
            .checked_add(temporary)?
            > scratch
        {
            return None;
        }
        bincode::serialize(&record).ok()
    }
}
impl BodyTask {
    pub(super) fn load_evicted(
        &self,
        cx: &mut dyn QueryContext,
    ) -> Option<Result<ProductRef, Reference>> {
        let facts = match self
            .queries
            .slots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&self.input)?
        {
            FrontSlot::Live { .. } => return None,
            FrontSlot::Archived(archived) => archived.facts.clone()?,
        };
        // Restoring optional compact facts can decline decoding or admission.
        // Recompute from the immutable source on a miss, as for other caches.
        self.load_terminal(cx, &facts.bytes)
    }
    fn load_terminal(
        &self,
        cx: &mut dyn QueryContext,
        bytes: &[u8],
    ) -> Option<Result<ProductRef, Reference>> {
        if (bytes.len() as u64).checked_mul(8)? > self.weight.scratch {
            return None;
        }
        let record: Record = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit(bytes.len() as u64)
            .reject_trailing_bytes()
            .deserialize(bytes)
            .ok()?;
        if record.key != self.queries.inputs[&self.input].key.0
            || (bytes.len() as u64)
                .checked_mul(8)?
                .checked_add(record.decode_bytes)?
                > self.weight.scratch
            || record.body.is_some_and(|body| {
                body.domain != crate::store::product_domain(QueryKind::Modes)
                    || body.len > self.weight.scratch
            })
        {
            return None;
        }
        let reads = Arc::new(record.reads);
        let facts = self.decode_facts(&record.facts, &reads)?;
        self.install_cached_facts(cx, record.body, reads, facts)
    }
    pub(super) fn load_compact(
        &self,
        cx: &mut dyn QueryContext,
    ) -> Option<Result<ProductRef, Reference>> {
        let queries = &self.queries;
        if !queries.store.persistence().retains() {
            return None;
        }
        let input = &queries.inputs[&self.input];
        let (variant, index_bytes) =
            load_record(&*queries.store, &input.key, self.weight.scratch, |read| {
                queries.observe(input.definition.path(), read)
            })?;
        let record: Record = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit(variant.payload.len() as u64)
            .reject_trailing_bytes()
            .deserialize(&variant.payload)
            .ok()?;
        if record.key != self.queries.inputs[&self.input].key.0
            || record.body.is_some_and(|body| {
                body.domain != crate::store::product_domain(QueryKind::Modes)
                    || body.len > self.weight.scratch
            })
            || (variant.payload.len() as u64)
                .checked_mul(8)?
                .checked_add(record.decode_bytes)?
                .checked_add(index_bytes)?
                > self.weight.scratch
            || record.reads != variant.reads
        {
            return None;
        }
        let reads = Arc::new(record.reads);
        let facts = self.decode_facts(&record.facts, &reads)?;
        let completed = self.install_cached_facts(cx, record.body, reads.clone(), facts)?;
        if completed.is_ok() {
            self.remember_cached_carry(record.body, reads, record.facts, record.decode_bytes);
        }
        Some(completed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_summary_adopts_checked_facts_without_a_body_and_refuses_substitutions() {
        for verify in [
            crate::store::VerifyStore::Off,
            crate::store::VerifyStore::Recompute,
        ] {
            for fault in 0..10 {
                let store = Arc::new(crate::store::MemStore::default());
                let mut engine =
                    QueryEngine::new(crate::default_environment(), PhaseAdapters::default())
                        .with_result_store(store.clone());
                let file = engine
                    .sources
                    .add("summary-cache.hir", "fn main(){}".into())
                    .unwrap();
                let tokens = hirsel_lexer::lex(file, &engine.sources).tokens;
                let context = crate::default_context();
                let mut parsed = hirsel_parser::parse(file, tokens, &context, &mut engine.sources);
                assert!(parsed.diagnostics.is_empty());
                engine.index(&mut parsed.syntax, &context).unwrap();
                let syntax = &parsed.syntax.items[0];
                let path = &syntax.item.definition;
                let key = engine.store_key(path, QueryKind::Modes).unwrap();
                let checked_summary = Arc::new(CheckedSummary {
                    summary: DefinitionSummary {
                        owner: syntax.owner().def,
                        frames: vec![hirsel_query_products::Frame {
                            site: hirsel_types::NameSite::Item,
                            parent: None,
                            kind: hirsel_query_products::FrameKind::Item,
                            generic: false,
                            size_bindings: Box::new([]),
                        }]
                        .into(),
                        calls: [].into(),
                        staged: [].into(),
                    },
                    growth: Default::default(),
                });
                let reads = Arc::new(Reads::empty());
                let mut summary = PublishedSummary {
                    definition: checked_summary.summary.owner,
                    summary: checked_summary.summary.clone(),
                    growth: checked_summary.growth.clone(),
                    storage: None,
                    checked: Some(checked_summary.clone()),
                    reads: reads.clone(),
                    text: engine.key_text(path, QueryKind::Summary).unwrap(),
                };
                let mut checked = Some((
                    checked_summary,
                    reads.clone(),
                    Arc::new(OrderedSignatureReads::new(&[])),
                ));
                match fault {
                    1 => summary.text = ContentHash::default(),
                    2 => summary.checked = None,
                    3 => summary.summary.frames[0].parent = Some(0),
                    4 => checked = None,
                    5 => {
                        Arc::make_mut(&mut checked.as_mut().unwrap().0)
                            .summary
                            .owner = DefId::intern(&DefPath::core("substituted"))
                    }
                    _ => (),
                }
                let facts = carry::FactsRef {
                    summary: Some(Arc::new(summary)),
                    checked: &checked,
                    dead_arms: &None,
                    diagnostics: &vec![],
                    diagnostic_phases: &vec![],
                    lowering: None,
                    table_keys: None,
                };
                let mut record = Record {
                    key: key.0,
                    // Body-free publication must not fetch this absent archive.
                    body: Some(
                        crate::store::product_domain(QueryKind::Modes)
                            .reference(b"absent body")
                            .unwrap(),
                    ),
                    reads: Reads::empty(),
                    decode_bytes: 0,
                    facts: engine.encode_record(&facts).unwrap(),
                };
                if fault == 6 {
                    record.reads =
                        Reads::from_vec(vec![(ReadKey::Environment, ContentHash::default())])
                            .unwrap();
                }
                if fault == 8 {
                    record.decode_bytes = u64::MAX;
                }
                if fault == 9 {
                    record.body.as_mut().unwrap().domain =
                        crate::store::product_domain(QueryKind::Summary);
                }
                let storage = storage_key(&key);
                let mut variant = VariantRef::publish(
                    &*store,
                    &storage,
                    record.reads.clone(),
                    &bincode::serialize(&record).unwrap(),
                )
                .unwrap();
                if fault == 7 {
                    variant.inputs = ContentHash::default();
                }
                let mut entry = crate::store::Entry::default();
                entry.insert(variant);
                store.put(&storage, &entry.encode_for(&storage));
                assert!(store.get_content(record.body.unwrap()).is_none());
                engine.verify_store = verify;
                let hit = engine.load_publication(path);
                let accepted = fault == 0 && verify == crate::store::VerifyStore::Off;
                assert_eq!(hit.is_some(), accepted, "fault {fault}, {verify:?}");
                assert_eq!(engine.store_adoptions(), u64::from(accepted));
                assert_eq!(engine.summaries.is_empty(), !accepted);
                assert_eq!(engine.checked_summaries.is_empty(), !accepted);
                for kind in [
                    QueryKind::Resolve,
                    QueryKind::Check,
                    QueryKind::Modes,
                    QueryKind::Summary,
                ] {
                    assert_eq!(engine.recompute_count(path, kind), 0);
                }
            }
        }
    }

    #[test]
    fn compact_fact_bytes_preserve_the_stored_sequence_and_its_bounds() {
        #[derive(serde::Serialize, serde::Deserialize)]
        struct Legacy {
            key: Key,
            body: Option<Reference>,
            reads: Reads,
            decode_bytes: u64,
            facts: Vec<u8>,
        }
        for len in [0, 1, 127, 8192] {
            let key = ContentHash::of_bytes(b"compact-owner");
            let legacy = Legacy {
                key,
                body: Some(
                    crate::store::product_domain(QueryKind::Modes)
                        .reference(b"body")
                        .unwrap(),
                ),
                reads: Reads::from_vec(vec![(ReadKey::Environment, key)]).unwrap(),
                decode_bytes: 12345,
                facts: (0..len).map(|n| n as u8).collect(),
            };
            let bytes = bincode::serialize(&legacy).unwrap();
            let options = || {
                bincode::DefaultOptions::new()
                    .with_fixint_encoding()
                    .with_limit(bytes.len() as u64)
                    .reject_trailing_bytes()
            };
            let record: Record = options().deserialize(&bytes).unwrap();
            assert_eq!(record.facts, legacy.facts);
            assert_eq!(record.reads, legacy.reads);
            assert_eq!(bincode::serialize(&record).unwrap(), bytes);
            let borrowed = RecordRef {
                key: record.key,
                body: record.body,
                reads: &record.reads,
                decode_bytes: record.decode_bytes,
                facts: &record.facts,
            };
            assert_eq!(bincode::serialize(&borrowed).unwrap(), bytes);
            for end in 0..bytes.len() {
                assert!(options().deserialize::<Record>(&bytes[..end]).is_err());
            }
            let mut trailing = bytes.clone();
            trailing.push(0);
            assert!(options().deserialize::<Record>(&trailing).is_err());
        }
    }
}
