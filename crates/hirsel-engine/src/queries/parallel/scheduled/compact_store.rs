//! Ordinary body-free P projections use the same canonical facts as hot carry.
//! Source text and all reads must still match; Lower retains independent V.
use super::*;

#[derive(serde::Serialize, serde::Deserialize)]
struct Record {
    key: Key,
    body: Option<Reference>,
    reads: Reads,
    #[serde(
        serialize_with = "hirsel_product_codec::serialize_payload",
        deserialize_with = "hirsel_product_codec::deserialize_payload"
    )]
    facts: Vec<u8>,
}
#[derive(serde::Serialize)]
struct RecordTail<'a> {
    #[serde(serialize_with = "hirsel_product_codec::serialize_payload")]
    facts: &'a [u8],
}
fn encode_record(
    head: (Key, Option<Reference>),
    reads: &Reads,
    reads_bytes: u64,
    tail: &RecordTail<'_>,
    encoded: u64,
) -> Option<Vec<u8>> {
    // bincode 1.3.3 src/ser/mod.rs:155-185 leaves tuples/structs unframed.
    // Preserve Record's field order and write its admitted HRD3 only once.
    let mut bytes = Vec::with_capacity(usize::try_from(encoded).ok()?);
    bincode::serialize_into(&mut bytes, &head).ok()?;
    bincode::serialize_into(&mut bytes, &reads_bytes).ok()?;
    reads.encode(&mut bytes);
    bincode::serialize_into(&mut bytes, tail).ok()?;
    Some(bytes)
}
impl hirsel_product_codec::decode_schema::Walk for Record {
    fn walk(scan: &mut hirsel_product_codec::decode_schema::Scan<'_>) -> Option<()> {
        use hirsel_product_codec::decode_schema::{field, record_decode_requirement};
        let _: fn(&Self) = |v| {
            let Self {
                key: _,
                body: _,
                reads: _,
                facts: _,
            } = v;
        };
        field(|v: &Self| &v.key, scan)?;
        field(|v: &Self| &v.body, scan)?;
        field(|v: &Self| &v.reads, scan)?;
        let n = scan.length()?;
        scan.add(n)?;
        let required = record_decode_requirement::<carry::Facts>(scan.take(n)?)?;
        scan.add(usize::try_from(required).ok()?)
    }
}
fn decode_record(
    store: &dyn ResultStore,
    bytes: hirsel_artifact_store::Bytes,
    scratch: u64,
) -> Option<Record> {
    let requirement = hirsel_product_codec::DecodeRequirement::<Record>::plain(bytes)?;
    if requirement.required_bytes() > scratch {
        return None;
    }
    requirement.decode_reserved(|bytes| store.try_admit_product_memory(bytes), |_, _| None)
}
fn address(key: Key) -> hirsel_artifact_store::Address {
    hirsel_artifact_store::Address(ContentHash::of_parts(
        "hirsel.front.compact.address.v2",
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
) -> Option<(hirsel_engine_store::Variant, u64)> {
    let key = storage_key(key);
    let bytes = store.get(&key)?;
    let index_bytes = (bytes.len() as u64).checked_mul(8)?;
    let remaining = scratch.checked_sub(index_bytes)?;
    let mut entry = hirsel_engine_store::Entry::decode_for(&key, &bytes)?;
    entry.variants.retain(|variant| {
        variant
            .output
            .len
            .checked_mul(8)
            .is_some_and(|bytes| bytes <= remaining)
    });
    entry
        .resolve(store, &key)
        .map(|variant| (variant, index_bytes))
}
/// Scalar Summary demand consumes the same checked compact facts written by
/// scheduled P. It does not reconstruct or execute the separate body product.
pub(in crate::queries) fn load_publication(
    engine: &mut QueryEngine,
    path: &DefPath,
) -> Option<PublishedResult> {
    if engine.verify_store == hirsel_engine_store::VerifyStore::Recompute {
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
        (hirsel_engine_store::MAX_RECORD as u64).checked_mul(8)?,
    )?;
    let record = decode_record(
        &*store,
        variant.payload.clone(),
        (hirsel_engine_store::MAX_RECORD as u64)
            .checked_mul(8)?
            .checked_sub(index_bytes)?,
    )?;
    if record.key != key.0
        || record.body.is_some_and(|body| {
            body.domain != hirsel_engine_store::product_domain(QueryKind::Modes)
                || body.len > hirsel_engine_store::MAX_RECORD as u64
        })
        || (variant.payload.len() as u64)
            .checked_mul(8)?
            .checked_add(index_bytes)?
            > (hirsel_engine_store::MAX_RECORD as u64).checked_mul(8)?
        || record.reads != variant.reads
    {
        return None;
    }
    let records = engine.type_records()?;
    let requirement = records.record_requirement::<carry::Facts>(record.facts.into())?;
    if requirement.required_bytes().checked_add(index_bytes)?
        > (hirsel_engine_store::MAX_RECORD as u64).checked_mul(8)?
    {
        return None;
    }
    let facts: carry::Facts = requirement.decode_reserved(
        |bytes| store.try_admit_product_memory(bytes),
        |path, occurrence| engine.record_anchor(path, occurrence),
    )?;
    let (encoded, roots) =
        hirsel_types::with_table_collection(|| bincode::serialize_into(std::io::sink(), &facts));
    encoded.ok()?;
    if !roots.iter().all(|id| engine.types.get(*id).is_some()) {
        return None;
    }
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
    /// Persist a completed record at the store boundary. A request-local
    /// store keeps the live outcome itself; nothing reads an encoded copy.
    pub(super) fn store_compact(&self, key: Key, outcome: &mut FrontOutcome, scratch: u64) {
        if !self.store.persistence().publishes() {
            return;
        }
        self.store
            .archive_encoded(hirsel_engine_store::ArchiveBoundary::Ordinary);
        if let Some(bytes) = self.encode_terminal(key, outcome, scratch) {
            let storage = storage_key(&self.inputs[&key].key);
            outcome.terminal = if self.terminal_cache.is_some() {
                // The provider lease keeps these immutable bytes through this
                // input view. Reuse their reference even before the optional
                // query index is published, without a second transport owner.
                self.store
                    .cache_query(&storage, (*outcome.reads).clone(), &bytes)
            } else {
                let reference = self.store.put_content(
                    hirsel_engine_store::product_domain(QueryKind::Summary),
                    &bytes,
                );
                self.store
                    .cache_query(&storage, (*outcome.reads).clone(), &bytes);
                reference
            };
        }
    }

    fn encode_terminal(&self, key: Key, outcome: &FrontOutcome, scratch: u64) -> Option<Vec<u8>> {
        let temporary = outcome
            .dead_arms
            .as_ref()
            .map_or(0, LintFacts::materialize_bytes)
            .checked_add(hirsel_engine_admission::retained_arc_bytes(
                &std::mem::MaybeUninit::<PublishedSummary>::uninit(),
            ))?;
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
                Some(
                    publication
                        .live_value::<crate::products::LiveSummaryProduct>()?
                        .value_arc(),
                ),
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
        let head = (self.inputs.get(&key)?.key.0, archive);
        let tail = RecordTail {
            facts: &facts_bytes,
        };
        let reads_bytes = outcome.reads.encoded_bytes()?;
        let encoded = bincode::serialized_size(&head)
            .ok()?
            .checked_add(8)?
            .checked_add(reads_bytes)?
            .checked_add(bincode::serialized_size(&tail).ok()?)?;
        if encoded
            .checked_mul(8)?
            .checked_add(decode_bytes)?
            .checked_add(temporary)?
            > scratch
        {
            return None;
        }
        encode_record(head, &outcome.reads, reads_bytes, &tail, encoded)
    }
}
impl BodyTask {
    pub(super) fn checkpoint<T>(&self, stage: &str, value: Option<T>) -> Option<T> {
        if value.is_none() {
            operation_owner_trace(format_args!(
                "OWNER CHECKPOINT {} {stage}; scratch {}",
                self.queries.inputs[&self.input].definition.path(),
                self.weight.scratch,
            ));
        }
        value
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
        let (variant, index_bytes) = load_record(&*queries.store, &input.key, self.weight.scratch)?;
        let record = decode_record(
            &*queries.store,
            variant.payload.clone(),
            self.weight.scratch.checked_sub(index_bytes)?,
        )?;
        if record.key != self.queries.inputs[&self.input].key.0
            || record.body.is_some_and(|body| {
                body.domain != hirsel_engine_store::product_domain(QueryKind::Modes)
                    || body.len > self.weight.scratch
            })
            || (variant.payload.len() as u64)
                .checked_mul(8)?
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
            let required = queries
                .records
                .record_requirement::<carry::Facts>(record.facts.clone().into())?
                .required_bytes();
            self.remember_cached_carry(record.body, reads, record.facts, required);
        }
        Some(completed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bincode::Options;

    #[derive(Default)]
    struct DecodeStore {
        inner: hirsel_engine_store::MemStore,
        refuse: std::sync::atomic::AtomicBool,
        requested: std::sync::atomic::AtomicU64,
    }
    impl ResultStore for DecodeStore {
        fn admit_product_memory(&self, bytes: u64) -> Option<Arc<dyn Send + Sync>> {
            use std::sync::atomic::Ordering;
            self.requested.fetch_max(bytes, Ordering::Relaxed);
            if self.refuse.load(Ordering::Relaxed) {
                return None;
            }
            self.inner.admit_product_memory(bytes)
        }
        fn retention_scope(&self) -> Option<Box<dyn Send + Sync>> {
            self.inner.retention_scope()
        }
        fn capability(&self) -> hirsel_engine_store::ProviderCapability {
            self.inner.capability()
        }
        fn get(&self, key: &EntryKey) -> Option<hirsel_engine_store::Bytes> {
            self.inner.get(key)
        }
        fn put(&self, key: &EntryKey, bytes: &[u8]) -> hirsel_engine_store::WriteOutcome {
            self.inner.put(key, bytes)
        }
        fn name(&self) -> &'static str {
            "decode-demand-law"
        }
    }

    #[test]
    fn scalar_summary_adopts_checked_facts_without_a_body_and_refuses_substitutions() {
        for verify in [
            hirsel_engine_store::VerifyStore::Off,
            hirsel_engine_store::VerifyStore::Recompute,
        ] {
            for fault in 0..11 {
                let store = Arc::new(DecodeStore::default());
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
                        hirsel_engine_store::product_domain(QueryKind::Modes)
                            .reference(b"absent body")
                            .unwrap(),
                    ),
                    reads: Reads::empty(),
                    facts: engine.encode_record(&facts).unwrap(),
                };
                if fault == 6 {
                    record.reads =
                        Reads::from_vec(vec![(ReadKey::Environment, ContentHash::default())])
                            .unwrap();
                }
                if fault == 8 {
                    record.facts.truncate(8);
                }
                if fault == 9 {
                    record.body.as_mut().unwrap().domain =
                        hirsel_engine_store::product_domain(QueryKind::Summary);
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
                let mut entry = hirsel_engine_store::Entry::default();
                entry.insert(variant);
                store.put(&storage, &entry.encode_for(&storage));
                assert!(store.get_content(record.body.unwrap()).is_none());
                store
                    .refuse
                    .store(fault == 10, std::sync::atomic::Ordering::Relaxed);
                engine.verify_store = verify;
                let hit = engine.load_publication(path);
                let accepted = fault == 0 && verify == hirsel_engine_store::VerifyStore::Off;
                assert_eq!(hit.is_some(), accepted, "fault {fault}, {verify:?}");
                if fault == 10 && verify == hirsel_engine_store::VerifyStore::Off {
                    assert!(
                        store.requested.load(std::sync::atomic::Ordering::Relaxed) > 0,
                        "the persisted size claim cannot bypass decode admission"
                    );
                }
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
        struct Sequence {
            key: Key,
            body: Option<Reference>,
            reads: Reads,
            facts: Vec<u8>,
        }
        for len in [0, 1, 127, 8192] {
            let key = ContentHash::of_bytes(b"compact-owner");
            let sequence = Sequence {
                key,
                body: Some(
                    hirsel_engine_store::product_domain(QueryKind::Modes)
                        .reference(b"body")
                        .unwrap(),
                ),
                reads: Reads::from_vec(vec![(ReadKey::Environment, key)]).unwrap(),
                facts: (0..len).map(|n| n as u8).collect(),
            };
            let bytes = bincode::serialize(&sequence).unwrap();
            let options = || {
                bincode::DefaultOptions::new()
                    .with_fixint_encoding()
                    .with_limit(bytes.len() as u64)
                    .reject_trailing_bytes()
            };
            let record: Record = options().deserialize(&bytes).unwrap();
            assert_eq!(record.facts, sequence.facts);
            assert_eq!(record.reads, sequence.reads);
            assert_eq!(bincode::serialize(&record).unwrap(), bytes);
            let tail = RecordTail {
                facts: &record.facts,
            };
            let streamed = encode_record(
                (record.key, record.body),
                &record.reads,
                record.reads.encoded_bytes().unwrap(),
                &tail,
                bytes.len() as u64,
            )
            .unwrap();
            assert_eq!(streamed, bytes);
            for end in 0..bytes.len() {
                assert!(options().deserialize::<Record>(&bytes[..end]).is_err());
            }
            let mut trailing = bytes.clone();
            trailing.push(0);
            assert!(options().deserialize::<Record>(&trailing).is_err());
        }
    }
}
