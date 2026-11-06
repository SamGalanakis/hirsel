use super::*;

#[test]
fn captured_anchor_bindings_match_scalar_selection_with_duplicate_and_generated_owners() {
    let mut engine = QueryEngine::new(crate::default_environment(), Default::default());
    let file = engine
        .sources
        .add(
            "duplicates.hir",
            "fn same()->u8{1u8}\nfn same()->u8{2u8}".into(),
        )
        .unwrap();
    let tokens = hirsel_lexer::lex(file, &engine.sources).tokens;
    let context = crate::default_context();
    let mut parsed = hirsel_parser::parse(file, tokens, &context, &mut engine.sources);
    assert!(parsed.diagnostics.is_empty());
    engine.index(&mut parsed.syntax, &context).unwrap();
    let mut alias = (*engine.items.values().next().unwrap()[0]).clone();
    alias.item.definition.0.name = "aaa_alias".into();
    engine
        .items
        .insert(alias.item.definition.clone(), vec![Arc::new(alias)]);
    let file = engine
        .sources
        .add("generated.hir", "    fn()->u8 {3u8}".into())
        .unwrap();
    engine.sources.item_anchor(file, 4);
    engine.sources.item_anchor(file, 9);
    for name in ["generated", "zzz_generated"] {
        engine.generated.insert(
            DefPath::core(name),
            crate::queries::generation::GeneratedSource {
                file,
                text: "    fn()->u8 {3u8}".into(),
                site: hirsel_span::Span::default(),
            },
        );
    }
    engine.sources.add("unowned.hir", "".into()).unwrap();
    let _previous = NativeRecordContext::capture(&engine);
    // Later aliases preserve the current owner; an earlier alias replaces it.
    // Both must agree with the independent scalar scan, including reverse
    // bindings for aliases that are not the selected owner.
    for name in ["zzz_alias", "000_alias"] {
        let mut alias = (*engine.items.values().next().unwrap()[0]).clone();
        alias.item.definition.0.name = name.into();
        let path = alias.item.definition.clone();
        engine.materialize_alias_syntax(&path, Arc::new(alias));
        let captured = NativeRecordContext::capture(&engine);
        for (anchor, _, _) in engine.sources.anchors() {
            let owner = scalar_owner(&engine, anchor);
            assert_eq!(captured.record_owner(anchor), owner);
            if let Some((owner, occurrence)) = owner {
                assert_eq!(
                    captured.record_source_anchor(&owner, occurrence),
                    scalar_anchor(&engine, &owner, occurrence)
                );
            }
        }
        for (path, items) in &engine.items {
            for item in items {
                assert_eq!(
                    captured.record_source_anchor(path, item.occurrence),
                    scalar_anchor(&engine, path, item.occurrence)
                );
            }
        }
    }
}

fn context(name: &str, text: &str) -> (NativeRecordContext, Anchor, hirsel_types::TypeId) {
    let mut engine = QueryEngine::new(crate::default_environment(), Default::default());
    let file = engine.sources.add(name, text.into()).unwrap();
    let tokens = hirsel_lexer::lex(file, &engine.sources).tokens;
    let context = crate::default_context();
    let mut parsed = hirsel_parser::parse(file, tokens, &context, &mut engine.sources);
    assert!(parsed.diagnostics.is_empty());
    engine.index(&mut parsed.syntax, &context).unwrap();
    let anchor = engine
        .sources
        .anchors()
        .find_map(|(anchor, _, _)| {
            (engine.sources.anchor(anchor).unwrap().0 == file)
                .then(|| engine.record_anchor_owner(anchor).map(|_| anchor))
                .flatten()
        })
        .unwrap();
    let ty = engine.types().intern(hirsel_types::Type::unit());
    (NativeRecordContext::capture(&engine), anchor, ty)
}

#[test]
fn identical_generated_sources_rebind_old_sites_but_changed_text_stays_unowned() {
    let mut engine = QueryEngine::new(crate::default_environment(), Default::default());
    let path = DefPath::core("generated");
    let logical = "<generated:core@1/generated>";
    let text = "    fn()->u8 {3u8}";
    let mut placed = |text: &str| {
        let file = engine
            .sources
            .add_logical(logical, logical, text.into())
            .unwrap();
        let anchor = engine.sources.item_anchor(file, 4);
        (file, anchor)
    };
    let (_, old) = placed(text);
    let (_, changed) = placed("    fn()->u8 {4u8}");
    let (file, current) = placed(text);
    let unrelated_file = engine
        .sources
        .add_logical("unrelated", "unrelated", text.into())
        .unwrap();
    let unrelated = engine.sources.item_anchor(unrelated_file, 4);
    engine.generated.insert(
        path,
        crate::queries::generation::GeneratedSource {
            file,
            text: text.into(),
            site: hirsel_span::Span::default(),
        },
    );
    let ty = engine.types().intern(hirsel_types::Type::unit());
    let context = NativeRecordContext::capture(&engine);
    let records = NativeRecords::new(
        engine.types().clone(),
        Arc::new(hirsel_engine_store::MemStore::default()),
    );
    let row = |anchor| Row {
        origin: hirsel_span::Span::new(anchor, 0, 4).unwrap(),
        ty,
    };
    let encoded = records.encode(&context, &row(old)).unwrap();
    assert_eq!(encoded, records.encode(&context, &row(current)).unwrap());
    let decoded: Row = records.decode(&context, &encoded).unwrap();
    assert_eq!(decoded.origin.anchor, current);
    assert_eq!(context.record_owner(old), engine.record_anchor_owner(old));
    let (owner, occurrence) = context.record_owner(old).unwrap();
    assert_eq!(engine.record_anchor(&owner, occurrence), Some(current));
    assert!(context.record_owner(changed).is_none());
    assert!(records.encode(&context, &row(changed)).is_none());
    assert!(context.record_owner(unrelated).is_none());
    assert!(records.encode(&context, &row(unrelated)).is_none());
}
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct Row {
    origin: hirsel_span::Span,
    ty: hirsel_types::TypeId,
}
#[test]
fn source_records_rebind_real_owners_after_engine_release_and_refuse_another_context() {
    let store = Arc::new(hirsel_engine_store::MemStore::default());
    let text = "pub fn main()->u64 {7u64}";
    let (source, anchor, ty) = context("first/records.hir", text);
    let records = NativeRecords::new(source.record_types().clone(), store.clone());
    let bytes = records
        .encode(
            &source,
            &Row {
                origin: hirsel_span::Span::new(anchor, 0, 4).unwrap(),
                ty,
            },
        )
        .unwrap();
    let original: Row = records.decode(&source, &bytes).unwrap();
    assert_eq!(original.origin.anchor, anchor);
    assert_eq!((original.origin.start, original.origin.end), (0, 4));
    assert_eq!(original.ty, ty);
    let (relocated, other_anchor, _) = context("second/records.hir", text);
    assert_ne!(anchor, other_anchor);
    let other = NativeRecords::new(relocated.record_types().clone(), store.clone());
    let rebound: Row = other.decode(&relocated, &bytes).unwrap();
    assert_eq!(rebound.origin.anchor, other_anchor);
    assert_eq!((rebound.origin.start, rebound.origin.end), (0, 4));
    assert_eq!(other.encode(&relocated, &rebound).unwrap(), bytes);
    let (unrelated, _, _) = context("other/records.hir", "pub fn other()->u64 {3u64}");
    let unknown = NativeRecords::new(unrelated.record_types().clone(), store.clone());
    assert!(unknown.decode::<Row>(&unrelated, &bytes).is_none());
    let wrong = NativeRecords::new(hirsel_types::TypeInterner::new(None), store);
    assert!(
        wrong
            .encode(
                &source,
                &Row {
                    origin: hirsel_span::Span::new(anchor, 0, 4).unwrap(),
                    ty
                }
            )
            .is_none()
    );
    assert!(wrong.decode::<Row>(&source, &bytes).is_none());
}

#[test]
fn generated_source_owner_is_independent_of_codec_identity_and_selector_position() {
    fn generated(text: &str) -> (NativeRecordContext, DefPath, Anchor, hirsel_types::TypeId) {
        let mut engine = QueryEngine::new(crate::default_environment(), Default::default());
        let path = DefPath::core("generated");
        let file = engine.sources.add("generated.hir", text.into()).unwrap();
        let anchor = engine.sources.item_anchor(file, 4);
        engine.generated.insert(
            path.clone(),
            crate::queries::generation::GeneratedSource {
                file,
                text: text.into(),
                site: hirsel_span::Span::default(),
            },
        );
        let ty = engine.types().intern(hirsel_types::Type::unit());
        (NativeRecordContext::capture(&engine), path, anchor, ty)
    }
    let (context, path, anchor, ty) = generated("    fn(x:u64)->u64 {9u64/x}");
    assert_eq!(context.source_definition(anchor), Some(&path));
    let (codec_owner, occurrence) = context.record_owner(anchor).unwrap();
    assert_ne!(codec_owner, path);
    assert!(
        codec_owner
            .0
            .name
            .starts_with("$generated-source:generated:")
    );
    assert_eq!(occurrence, 4);
    assert_eq!(
        context.record_source_anchor(&codec_owner, occurrence),
        Some(anchor)
    );
    assert_eq!(context.record_source_anchor(&path, 0), None);
    assert_eq!(context.source_definition(Anchor(u32::MAX)), None);
    let store = Arc::new(hirsel_engine_store::MemStore::default());
    let records = NativeRecords::new(context.record_types().clone(), store.clone());
    let bytes = records
        .encode(
            &context,
            &Row {
                origin: hirsel_span::Span::new(anchor, 0, 2).unwrap(),
                ty,
            },
        )
        .unwrap();
    let same = records.decode::<Row>(&context, &bytes).unwrap();
    assert_eq!(same.origin.anchor, anchor);
    let (changed, changed_path, changed_anchor, _) = generated("    fn(x:u64)->u64 {8u64/x}");
    assert_eq!(changed_path, path);
    assert_eq!(changed.source_definition(changed_anchor), Some(&path));
    assert_ne!(
        changed.record_owner(changed_anchor),
        Some((codec_owner, occurrence))
    );
    let other = NativeRecords::new(changed.record_types().clone(), store);
    assert!(
        other.decode::<Row>(&changed, &bytes).is_none(),
        "a declaration association cannot admit a different generated text"
    );
}

fn scalar_owner(engine: &QueryEngine, anchor: Anchor) -> Option<(DefPath, u32)> {
    super::super::persistent::artifact_anchor_owner(
        &engine.items,
        &engine.sources,
        &engine.generated,
        anchor,
    )
    .or_else(|| {
        engine
            .catalog
            .bindings()
            .find_map(|(site, owner)| (site == anchor).then(|| (owner.0.clone(), owner.1)))
    })
}

fn scalar_anchor(engine: &QueryEngine, path: &DefPath, occurrence: u32) -> Option<Anchor> {
    engine
        .items
        .get(path)
        .and_then(|items| items.iter().find(|item| item.occurrence == occurrence))
        .map(|item| item.anchor)
        .or_else(|| engine.catalog.anchor(path, occurrence))
        .or_else(|| {
            let (_, generated) = engine.generated.iter().find(|(owner, source)| {
                super::super::persistent::generated_codec_owner(owner, &source.text) == *path
            })?;
            engine.sources.anchors().find_map(|(anchor, file, base)| {
                (file == generated.file && base == occurrence).then_some(anchor)
            })
        })
        .or_else(|| {
            engine.sources.anchors().find_map(|(anchor, _, _)| {
                (scalar_owner(engine, anchor) == Some((path.clone(), occurrence))).then_some(anchor)
            })
        })
}

fn assert_scalar_bindings(engine: &QueryEngine) {
    let captured = NativeRecordContext::capture(engine);
    for (anchor, _, _) in engine.sources.anchors() {
        let owner = scalar_owner(engine, anchor);
        assert_eq!(captured.record_owner(anchor), owner);
        if let Some((path, occurrence)) = owner {
            assert_eq!(
                captured.record_source_anchor(&path, occurrence),
                scalar_anchor(engine, &path, occurrence)
            );
        }
    }
    for (path, items) in &engine.items {
        for item in items {
            assert_eq!(
                captured.record_source_anchor(path, item.occurrence),
                scalar_anchor(engine, path, item.occurrence)
            );
        }
    }
    assert!(captured.record_owner(Anchor::fresh()).is_none());
    assert!(
        captured
            .record_source_anchor(&DefPath::core("foreign-owner"), 0)
            .is_none()
    );
}

#[test]
fn ownership_product_tracks_removal_reordering_relocation_and_parse_eviction() {
    let mut engine = QueryEngine::new(crate::default_environment(), Default::default());
    let context = crate::default_context();
    let index = |engine: &mut QueryEngine, name: &str, text: &str| {
        let file = engine.sources.add(name, text.into()).unwrap();
        let tokens = hirsel_lexer::lex(file, &engine.sources).tokens;
        let mut parsed = hirsel_parser::parse(file, tokens, &context, &mut engine.sources);
        assert!(parsed.diagnostics.is_empty());
        engine.index(&mut parsed.syntax, &context).unwrap();
        engine.parsed.clear();
        assert_scalar_bindings(engine);
    };
    index(
        &mut engine,
        "a.hir",
        "fn same()->u8{1u8}\nfn same()->u8{2u8}\nfn gone()->u8{3u8}",
    );
    let old = engine.source_records();
    let gone = engine
        .items
        .iter()
        .find(|(p, _)| p.0.name == "gone")
        .unwrap()
        .1[0]
        .anchor;
    index(
        &mut engine,
        "a.hir",
        "fn same()->u8{2u8}\nfn same()->u8{1u8}",
    );
    assert!(engine.record_anchor_owner(gone).is_none());
    assert!(old.record_owner(gone).is_some());
    index(&mut engine, "a.hir", "");
    index(
        &mut engine,
        "b.hir",
        "fn same()->u8{2u8}\nfn same()->u8{1u8}",
    );
    let first = engine.source_records();
    let second = engine.source_records();
    assert!(Arc::ptr_eq(&first.bindings, &second.bindings));
    let bindings = Arc::downgrade(&first.bindings);
    drop(first);
    drop(second);
    index(&mut engine, "b.hir", "fn same()->u8{4u8}");
    assert!(
        bindings.upgrade().is_none(),
        "retired products are released after their last worker"
    );
    assert!(old.record_owner(gone).is_some());
    let store = Arc::new(hirsel_engine_store::MemStore::default());
    let create = || {
        QueryEngine::new(
            crate::default_environment(),
            crate::PhaseAdapters {
                lexer: Some(Arc::new(crate::LexerAdapter)),
                parser: Some(Arc::new(crate::ParserAdapter)),
                ..Default::default()
            },
        )
        .with_result_store(store.clone())
    };
    let mut producer = create();
    let file = producer
        .sources
        .add("hydrated.hir", "fn body()->u8{9u8}".into())
        .unwrap();
    producer.index_source(file, &context).unwrap();
    let (path, syntax) = producer
        .items
        .iter()
        .next()
        .map(|(path, items)| (path.clone(), items[0].clone()))
        .unwrap();
    let encoded = NativeRecords::new(producer.types().clone(), store.clone())
        .encode(&producer.source_records(), &syntax)
        .unwrap();
    let mut consumer = create();
    consumer.sources = producer.sources.clone();
    consumer.index_source(file, &context).unwrap();
    assert!(
        !consumer.items.contains_key(&path),
        "fixture restores declaration heads without bodies"
    );
    let before = consumer.source_records();
    let syntax: Arc<hirsel_syntax::ItemSyntax> =
        NativeRecords::new(consumer.types().clone(), store.clone())
            .decode(&before, &encoded)
            .unwrap();
    let bytes = before.input_bytes().unwrap();
    let budget = hirsel_engine_admission::ProcessBudget::new(bytes);
    let mut allowance = budget.reserve(bytes).unwrap();
    before.admit(&mut allowance).unwrap();
    consumer.materialize_bound_syntax(&path, syntax.clone(), &before);
    assert_scalar_bindings(&consumer);
    assert_eq!(
        consumer.source_records().unadmitted_bytes(),
        Some(0),
        "hydrating identical bindings requires no second ownership reservation"
    );
    consumer.items.remove(&path);
    assert_scalar_bindings(&consumer);
    let mut alias = (*syntax).clone();
    alias.item.definition.0.name = "aaa_alias".into();
    let alias_path = alias.item.definition.clone();
    consumer.materialize_bound_syntax(&alias_path, Arc::new(alias), &before);
    assert_scalar_bindings(&consumer);
    assert_eq!(
        before.record_owner(syntax.anchor),
        Some((path.clone(), syntax.occurrence))
    );
    consumer.items.clear();
    assert_scalar_bindings(&consumer);
}

#[test]
fn ownership_projection_is_charged_and_generated_changes_retire_both_directions() {
    let mut engine = QueryEngine::new(crate::default_environment(), Default::default());
    for name in ["a", "b", "c"] {
        let file = engine.sources.add(name, "fn()->u8{1u8}".into()).unwrap();
        engine.generated.insert(
            DefPath::core(name),
            crate::queries::generation::GeneratedSource {
                file,
                text: "fn()->u8{1u8}".into(),
                site: Default::default(),
            },
        );
    }
    assert_scalar_bindings(&engine);
    let old = engine.source_records();
    let file = engine.sources.add("extra", "fn()->u8{2u8}".into()).unwrap();
    let anchor = engine.sources.item_anchor(file, 0);
    assert!(engine.record_anchor_owner(anchor).is_none());
    engine.generated.insert(
        DefPath::core("a"),
        crate::queries::generation::GeneratedSource {
            file,
            text: "fn()->u8{2u8}".into(),
            site: Default::default(),
        },
    );
    assert_scalar_bindings(&engine);
    let current = engine.source_records();
    for (owner, occurrence) in old
        .bindings
        .owners
        .iter()
        .map(|(_, owner)| owner)
        .filter(|(owner, _)| owner.path().0.name.starts_with("$generated-source:a:"))
    {
        assert!(
            current
                .record_source_anchor(owner.path(), *occurrence)
                .is_none()
        );
    }
    let bound = current.input_bytes().unwrap();
    assert!(bound > size_of::<NativeRecordContext>() as u64);
    engine.generated.clear();
    let (next, allocated) = crate::test_alloc::measured_independent(|| engine.source_records());
    assert!(allocated.live as u64 <= next.input_bytes().unwrap());
    assert!(next.bindings.owners.is_empty());
    assert!(current.record_owner(anchor).is_some());
}

#[test]
fn ownership_charge_covers_populated_maps_and_releases_the_last_generation() {
    let mut engine = QueryEngine::new(crate::default_environment(), Default::default());
    let text = (0..64)
        .map(|i| format!("fn owner_with_a_long_name_{i}()->u64{{{i}u64}}\n"))
        .collect();
    let file = engine.sources.add("charged.hir", text).unwrap();
    let tokens = hirsel_lexer::lex(file, &engine.sources).tokens;
    let context = crate::default_context();
    let mut parsed = hirsel_parser::parse(file, tokens, &context, &mut engine.sources);
    assert!(parsed.diagnostics.is_empty());
    engine.index(&mut parsed.syntax, &context).unwrap();
    for path in engine.items.keys() {
        DefId::intern(path);
    }
    let (owners, allocated) = crate::test_alloc::measured_independent(|| engine.source_records());
    assert_eq!(owners.bindings.owners.len(), 64);
    assert!(allocated.live > 0);
    assert!(allocated.live as u64 <= owners.input_bytes().unwrap());
    let old = Arc::downgrade(&owners.bindings);
    engine.items.clear();
    assert!(engine.source_records().bindings.owners.is_empty());
    assert!(old.upgrade().is_some());
    drop(owners);
    assert!(old.upgrade().is_none());
}

#[test]
fn a_foreign_source_map_cannot_restore_unplaced_declaration_anchors() {
    let mut engine = QueryEngine::new(crate::default_environment(), Default::default());
    let file = engine
        .sources
        .add("current.hir", "pub fn main()->u64{7u64}".into())
        .unwrap();
    let tokens = hirsel_lexer::lex(file, &engine.sources).tokens;
    let context = crate::default_context();
    let mut parsed = hirsel_parser::parse(file, tokens, &context, &mut engine.sources);
    assert!(parsed.diagnostics.is_empty());
    engine.index(&mut parsed.syntax, &context).unwrap();
    let syntax = &parsed.syntax.items[0];
    let path = syntax.item.definition.clone();
    let anchor = syntax.anchor;
    let records = NativeRecords::new(
        engine.types().clone(),
        Arc::new(hirsel_engine_store::MemStore::default()),
    );
    let ty = engine.types().intern(hirsel_types::Type::unit());
    let bytes = records
        .encode(
            &engine,
            &Row {
                origin: hirsel_span::Span::new(anchor, 0, 4).unwrap(),
                ty,
            },
        )
        .unwrap();
    let mut foreign = hirsel_span::SourceMap::default();
    foreign
        .add("foreign.hir", "pub fn other()->u64{9u64}".into())
        .unwrap();
    engine.sources = foreign;
    let current = engine.source_records();
    assert!(current.record_owner(anchor).is_none());
    assert!(current.record_source_anchor(&path, 0).is_none());
    assert!(records.decode::<Row>(&current, &bytes).is_none());
}

struct LazyRowsContents {
    memory: hirsel_artifact_store::Memory,
    reads: std::sync::atomic::AtomicUsize,
    corrupt: std::sync::Mutex<Option<hirsel_artifact_store::Reference>>,
}
impl hirsel_artifact_store::Contents for LazyRowsContents {
    fn get_content(
        &self,
        reference: hirsel_artifact_store::Reference,
    ) -> Option<hirsel_artifact_store::Bytes> {
        use std::sync::atomic::Ordering;
        self.reads.fetch_add(1, Ordering::Relaxed);
        let bytes = self.memory.get_content(reference)?;
        if *self.corrupt.lock().unwrap() == Some(reference) {
            let mut bytes = bytes.to_vec();
            bytes[0] ^= 1;
            Some(bytes.into())
        } else {
            Some(bytes)
        }
    }
    fn put_content(
        &self,
        domain: hirsel_artifact_store::Domain,
        bytes: &[u8],
    ) -> Option<hirsel_artifact_store::Reference> {
        self.memory.put_content(domain, bytes)
    }
}
struct LazyRowsFixture {
    store: Arc<LazyRowsContents>,
    eager: Arc<hirsel_types::TypeInterner>,
    records: NativeRecords,
    context: NativeRecordContext,
    ids: Vec<hirsel_types::TypeId>,
    references: Vec<hirsel_product_codec::TypeRef>,
}
fn lazy_rows_fixture() -> LazyRowsFixture {
    use hirsel_product_codec::{BaseIndex, SharedTypes};
    use hirsel_types::{Type, TypeInterner, TypeKind};
    let store = Arc::new(LazyRowsContents {
        memory: Default::default(),
        reads: Default::default(),
        corrupt: Default::default(),
    });
    let eager = TypeInterner::new(None);
    let unit = eager.intern(Type::unit());
    let cycle = eager.intern_graph(2, |ids| {
        vec![
            Type::with_kind(TypeKind::Box(ids[1])),
            Type::with_kind(TypeKind::Tuple(vec![ids[0], unit].into())),
        ]
    });
    let large = eager.intern(Type::with_kind(TypeKind::Named {
        definition: hirsel_syntax::DefId::intern(&DefPath::core(&"x".repeat(32768))),
        arguments: Default::default(),
    }));
    let boolean = eager.intern(Type::boolean());
    let roots = [unit, cycle[0], cycle[1], large, boolean, unit];
    let records = TypeRecords::new(eager.clone(), store.clone());
    let references = records.encode(&roots).unwrap();
    let index = BaseIndex::capture(&records, &roots).unwrap();
    let shared = SharedTypes::open(index, store.clone()).unwrap();
    let ids = (0..roots.len())
        .map(|p| shared.base.id_at(p as u32).unwrap())
        .collect();
    let lazy = TypeInterner::new(Some(shared.base));
    let engine =
        QueryEngine::new(crate::default_environment(), Default::default()).with_types(lazy.clone());
    LazyRowsFixture {
        store,
        eager,
        records: NativeRecords::new(lazy, Arc::new(hirsel_engine_store::MemStore::default())),
        context: NativeRecordContext::capture(&engine),
        ids,
        references,
    }
}

#[test]
fn lazy_archive_rows_equal_eager_bytes_and_values_without_publishing_shared_rows() {
    use std::sync::atomic::Ordering;
    let f = lazy_rows_fixture();
    let budget = hirsel_engine_admission::ProcessBudget::new(16 << 20);
    let grant = budget.reserve(16 << 20).unwrap();
    let value = &f.ids[..3];
    let mut reader = f
        .records
        .prepare_encoding(&f.context, &value, &grant, 0)
        .unwrap();
    assert_eq!(
        f.store.reads.load(Ordering::Relaxed),
        2,
        "unit and one complete SCC"
    );
    for &id in value {
        assert_eq!(reader.encoding.read_row(id).unwrap(), f.eager.row(id));
        assert!(f.records.types.get_if_loaded(id).is_none());
    }
    assert_eq!(
        reader.encoding.read_row(f.ids[5]).unwrap(),
        &hirsel_types::Type::unit()
    );
    assert_eq!(
        f.store.reads.load(Ordering::Relaxed),
        2,
        "checked once per reader"
    );
    let keys = reader
        .capture(0, value.iter().copied(), |rows| {
            value
                .iter()
                .map(|&id| rows.type_table(id))
                .collect::<Vec<_>>()
        })
        .unwrap();
    for (&id, key) in value.iter().zip(keys) {
        assert_eq!(key, hirsel_types::encode_table(&f.eager, &[id]).0);
        assert!(
            f.records.types.get_if_loaded(id).is_none(),
            "capture must use its admitted rows"
        );
    }
    let actual = reader.encode(&value).unwrap();
    let eager = TypeRecords::new(f.eager.clone(), f.store.clone());
    let expected = eager.encode_record(&value, |_| None).unwrap();
    assert_eq!(actual, expected, "complete canonical bytes");
    let decoder = TypeRecords::new(hirsel_types::TypeInterner::new(None), f.store.clone());
    let restored: Vec<hirsel_types::TypeId> = decoder.decode_record(&actual, |_, _| None).unwrap();
    assert_eq!(
        decoder.encode_record(&restored, |_| None).unwrap(),
        expected
    );
    assert_eq!(f.store.reads.load(Ordering::Relaxed), 4);
    drop(grant);
    assert_eq!(budget.reserved(), 0);
}

#[test]
fn lazy_archive_corruption_is_checked_on_demand_and_failure_does_not_poison_reader() {
    use hirsel_product_codec::ArchiveRowRefusal;
    use std::sync::atomic::Ordering;
    let f = lazy_rows_fixture();
    let budget = hirsel_engine_admission::ProcessBudget::new(16 << 20);
    let grant = budget.reserve(16 << 20).unwrap();
    *f.store.corrupt.lock().unwrap() = Some(f.references[1].reference());
    let mut reader = f
        .records
        .prepare_encoding(&f.context, &0u32, &grant, 0)
        .unwrap();
    assert_eq!(
        f.store.reads.load(Ordering::Relaxed),
        0,
        "opening leaves unrelated children unloaded"
    );
    assert_eq!(
        reader.encoding.read_row(f.ids[0]).unwrap(),
        &hirsel_types::Type::unit()
    );
    assert_eq!(
        reader.encoding.read_row(f.ids[1]),
        Err(ArchiveRowRefusal::Invalid)
    );
    assert!(f.records.types.get_if_loaded(f.ids[1]).is_none());
    *f.store.corrupt.lock().unwrap() = None;
    assert_eq!(
        reader.encoding.read_row(f.ids[1]).unwrap(),
        f.eager.row(f.ids[1])
    );
    assert_eq!(f.store.reads.load(Ordering::Relaxed), 3);
    *f.store.corrupt.lock().unwrap() = Some(f.references[1].reference());
    assert_eq!(
        reader.encoding.read_row(f.ids[2]).unwrap(),
        f.eager.row(f.ids[2])
    );
    assert_eq!(
        f.store.reads.load(Ordering::Relaxed),
        3,
        "same checked immutable component"
    );
}

#[test]
fn lazy_archive_row_over_reservation_refuses_without_allocation_or_io_and_reader_stays_usable() {
    use hirsel_product_codec::ArchiveRowRefusal;
    use std::sync::atomic::Ordering;
    let f = lazy_rows_fixture();
    let budget = hirsel_engine_admission::ProcessBudget::new(16 << 20);
    let grant = budget.reserve(16 << 20).unwrap();
    let mut reader = f
        .records
        .prepare_encoding(&f.context, &0u32, &grant, 0)
        .unwrap();
    assert_eq!(
        reader.encoding.read_row(f.ids[0]).unwrap(),
        &hirsel_types::Type::unit()
    );
    let reads = f.store.reads.load(Ordering::Relaxed);
    let (refused, allocated) =
        crate::test_alloc::measured(|| reader.encoding.read_row(f.ids[3]).map(|_| ()));
    assert_eq!(refused, Err(ArchiveRowRefusal::Admission));
    assert_eq!(allocated.allocations, 0);
    assert_eq!(f.store.reads.load(Ordering::Relaxed), reads);
    assert_eq!(
        reader.encoding.read_row(f.ids[4]).unwrap(),
        &hirsel_types::Type::boolean()
    );
    assert_eq!(
        reader.encoding.read_row(f.ids[0]).unwrap(),
        &hirsel_types::Type::unit()
    );
    assert!(f.records.types.get_if_loaded(f.ids[3]).is_none());
}

#[test]
fn lazy_archive_reader_drop_releases_rows_and_returns_exact_borrowed_partition() {
    use std::sync::atomic::Ordering;
    let f = lazy_rows_fixture();
    let bytes = (16 << 20) + hirsel_engine_admission::MemoryReservationPool::metadata_bytes();
    let budget = hirsel_engine_admission::ProcessBudget::new(bytes);
    let pool = hirsel_engine_admission::MemoryReservationPool::new(budget.reserve(bytes).unwrap())
        .ok()
        .unwrap();
    let grant = pool.take(16 << 20).unwrap();
    let mut reader = f
        .records
        .prepare_encoding(&f.context, &0u32, &grant, 0)
        .unwrap();
    let (_, allocations) = crate::test_alloc::measured(|| {
        reader.encoding.read_row(f.ids[1]).unwrap();
    });
    assert!(allocations.live > 0);
    assert!(allocations.peak <= grant.bytes() as isize);
    let (_, released) = crate::test_alloc::measured(|| drop(reader));
    assert_eq!(
        released.live, -allocations.live,
        "all loaded rows and their index drop with the reader"
    );
    assert!(
        f.ids
            .iter()
            .all(|&id| f.records.types.get_if_loaded(id).is_none())
    );
    assert_eq!(
        pool.available_bytes(),
        0,
        "the borrowed partition remains held until its owner drops"
    );
    drop(grant);
    assert_eq!(pool.available_bytes(), 16 << 20);
    let grant = pool.take(16 << 20).unwrap();
    let mut reader = f
        .records
        .prepare_encoding(&f.context, &0u32, &grant, 0)
        .unwrap();
    reader.encoding.read_row(f.ids[1]).unwrap();
    assert_eq!(
        f.store.reads.load(Ordering::Relaxed),
        2,
        "no cross-owner row cache"
    );
    drop(reader);
    drop(grant);
    drop(pool);
    assert_eq!(budget.reserved(), 0);
}
