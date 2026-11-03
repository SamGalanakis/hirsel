use super::*;
use crate::store::Domain;
use crate::{DefinitionSummary, GenericParameterRef, ParameterGrowth, QueryEngine, ReadsBuilder};
use hirsel_syntax::LexicalOwner;
use hirsel_types::{NameSite, TypeParamId, TypeParameterRef};

const SELECTED: Domain = Domain {
    kind: 0x4600_0020,
    schema: 1,
};

fn output(product: &SourceGrowthIdentity) -> ContentHash {
    product.output
}

fn output_pool() -> crate::runtime::MemoryReservationPool {
    let budget = crate::runtime::ProcessBudget::new(64 << 20);
    crate::runtime::MemoryReservationPool::new(budget.reserve(64 << 20).unwrap())
        .ok()
        .unwrap()
}

fn parameter(id: DefId) -> (DefId, GenericParameterRef) {
    (
        id,
        GenericParameterRef::Type(TypeParameterRef::body(
            LexicalOwner::first(id.path()).site(NameSite::Item),
            TypeParamId(0),
        )),
    )
}

fn context() -> (QueryEngine, NativeRecordContext, Vec<DefId>, Vec<Span>) {
    context_with("fn a(){}\nfn b(){}\nfn c(){}\n")
}

fn context_with(text: &str) -> (QueryEngine, NativeRecordContext, Vec<DefId>, Vec<Span>) {
    let mut engine = QueryEngine::new(crate::default_environment(), Default::default());
    let file = engine.sources.add("components.hir", text.into()).unwrap();
    let tokens = hirsel_lexer::lex(file, &engine.sources).tokens;
    let context = crate::default_context();
    let parsed = hirsel_parser::parse(file, tokens, &context, &mut engine.sources);
    assert!(parsed.diagnostics.is_empty());
    for syntax in parsed.syntax.items {
        engine
            .items
            .entry(syntax.item.definition.clone())
            .or_default()
            .push(syntax);
    }
    let ids = engine.items.keys().map(DefId::intern).collect();
    let sites = engine
        .items
        .values()
        .map(|items| items[0].item.origin.span)
        .collect();
    let source = NativeRecordContext::capture(&engine);
    (engine, source, ids, sites)
}

fn summary(owner: DefId, edges: Vec<crate::GrowthEdge>) -> Arc<CheckedSummary> {
    Arc::new(CheckedSummary {
        summary: DefinitionSummary {
            owner,
            frames: [].into(),
            calls: [].into(),
            staged: [].into(),
        },
        growth: ParameterGrowth {
            edges: edges.into(),
        },
    })
}

fn settle(
    producer: &SourceGrowthProducer<'_>,
    mut facts: Vec<SourceGrowthFact>,
) -> Option<SourceGrowthFacets> {
    facts.sort_by_key(|fact| (fact.owner, fact.definition.0.0));
    let membership = producer.membership(&facts)?;
    let components = membership
        .components
        .iter()
        .map(|component| {
            let facts: Vec<_> = facts
                .iter()
                .filter(|fact| component.inputs.contains(&fact.definition.0))
                .map(|fact| SourceGrowthFact {
                    owner: fact.owner,
                    calls: fact.calls.clone(),
                    growth: fact.growth.clone(),
                    definition: fact.definition,
                    selected: fact.selected,
                    observed: fact.observed,
                })
                .collect();
            producer.component(&component.members, &facts)
        })
        .collect::<Option<Vec<_>>>()?;
    let mut pages = SourceGrowthComponents::default();
    let count = components.len();
    for (index, component) in components.into_iter().enumerate() {
        pages.push(component, producer.output_pool, count - index)?;
    }
    producer.accept(membership.product, pages)
}

#[test]
fn equal_settled_component_stops_changed_member_and_preserves_unrelated_outputs() {
    let (_engine, source, ids, sites) = context();
    let pool = output_pool();
    let producer = SourceGrowthProducer {
        source: &source,
        output_pool: &pool,
        namespace: Address(ContentHash::of_bytes(b"source components")),
        scratch: 8 << 20,
    };
    let reads = ReadsBuilder::new().finish().unwrap();
    let selected = SELECTED.reference(b"accepted checked output").unwrap();
    let make = |extra, grows| {
        let mut edges = vec![(parameter(ids[0]), parameter(ids[1]), grows, sites[0])];
        if extra {
            edges.push((parameter(ids[0]), parameter(ids[0]), false, sites[0]));
        }
        vec![
            producer
                .fact(
                    Address(ids[0].path().fingerprint()),
                    summary(ids[0], edges),
                    &reads,
                    selected,
                )
                .unwrap(),
            producer
                .fact(
                    Address(ids[1].path().fingerprint()),
                    summary(
                        ids[1],
                        vec![(parameter(ids[1]), parameter(ids[0]), false, sites[1])],
                    ),
                    &reads,
                    selected,
                )
                .unwrap(),
            producer
                .fact(
                    Address(ids[2].path().fingerprint()),
                    summary(
                        ids[2],
                        vec![(parameter(ids[2]), parameter(ids[2]), false, sites[2])],
                    ),
                    &reads,
                    selected,
                )
                .unwrap(),
        ]
    };
    let first = settle(&producer, make(false, false)).unwrap();
    let changed = settle(&producer, make(true, false)).unwrap();
    assert_eq!(output(&first.membership), output(&changed.membership));
    assert_ne!(
        first.components[0].product.action,
        changed.components[0].product.action
    );
    assert_eq!(
        output(&first.components[0].product),
        output(&changed.components[0].product),
        "the settled verdict is the exact consumer cutoff even when its action changed"
    );
    assert_eq!(first.components[1].product, changed.components[1].product);
    let records = crate::NativeRecords::new(
        source.record_types().clone(),
        Arc::new(crate::store::MemStore::default()),
    );
    assert_eq!(
        records.encode(&source, &first),
        records.encode(&source, &changed),
        "the acceptance archive contains settled values, not changed producer actions"
    );
    let mut graph = crate::memo::MemoGraph::default();
    let key = crate::memo::MemoKey::new(crate::QueryKind::Summary, ids[0]);
    let original = graph.insert_output(
        key,
        first.components[0].product.action.0,
        Ok(Arc::new(())),
        Arc::new(reads.clone()),
        crate::memo::Durability::Low,
        Some(crate::summary::SettledSummary::component(
            &first.components[0],
        )),
    );
    let mut edits = crate::memo::ChangeSet::default();
    edits.text(ids[0], crate::memo::Durability::Low);
    graph.apply(&edits);
    let equal = graph.insert_output(
        key,
        changed.components[0].product.action.0,
        Ok(Arc::new(())),
        Arc::new(reads.clone()),
        crate::memo::Durability::Low,
        Some(crate::summary::SettledSummary::component(
            &changed.components[0],
        )),
    );
    assert_eq!(
        original.changed_at, equal.changed_at,
        "an actual settled facet backdates across the changed member"
    );
    let rejected = settle(&producer, make(true, true)).unwrap();
    assert_ne!(
        output(&changed.components[0].product),
        output(&rejected.components[0].product)
    );
    assert_eq!(&*rejected.components[0].growing, &[sites[0]]);
    let mut reordered = make(true, true);
    reordered.reverse();
    let reordered = settle(&producer, reordered).unwrap();
    assert_eq!(reordered.membership, rejected.membership);
    for (a, b) in reordered.components.iter().zip(&rejected.components) {
        assert_eq!(a.product, b.product);
    }
}

#[test]
fn changed_observations_bind_live_actions_before_equal_fact_cutoff() {
    let (_engine, source, ids, _) = context();
    let pool = output_pool();
    let producer = SourceGrowthProducer {
        source: &source,
        output_pool: &pool,
        namespace: Address(ContentHash::of_bytes(b"observations")),
        scratch: 8 << 20,
    };
    let selected = SELECTED.reference(b"accepted checked output").unwrap();
    for key in [
        crate::ReadKey::PrivateSummary(ids[0]),
        crate::ReadKey::Lookup(crate::memo::LookupKey::of(
            crate::memo::LookupSource::Provider,
            &crate::default_context(),
            None,
            &["missing".into()],
        )),
        crate::ReadKey::Lookup(crate::memo::LookupKey::of(
            crate::memo::LookupSource::Manifest,
            &crate::default_context(),
            None,
            &["module".into()],
        )),
    ] {
        let make = |value| {
            let mut reads = ReadsBuilder::new();
            reads.record(key, ContentHash::of_bytes(value));
            producer
                .fact(
                    Address(ids[0].path().fingerprint()),
                    summary(ids[0], vec![]),
                    &reads.finish().unwrap(),
                    selected,
                )
                .unwrap()
        };
        let a = make(b"before");
        let b = make(b"after");
        assert_ne!(a.observed, b.observed);
        assert_eq!(a.calls, b.calls);
        assert_eq!(a.growth, b.growth);
        let a = settle(&producer, vec![a]).unwrap();
        let b = settle(&producer, vec![b]).unwrap();
        let a = &a.components[0].product;
        let b = &b.components[0].product;
        assert_ne!(a.action, b.action);
        assert_eq!(output(a), output(b));
    }
}

#[test]
fn independently_selected_roots_sharing_a_lexical_owner_all_contribute() {
    let (_engine, source, ids, sites) = context();
    let pool = output_pool();
    let producer = SourceGrowthProducer {
        source: &source,
        output_pool: &pool,
        namespace: Address(ContentHash::of_bytes(b"shared lexical owner")),
        scratch: 8 << 20,
    };
    let reads = ReadsBuilder::new().finish().unwrap();
    let selected = SELECTED.reference(b"accepted checked output").unwrap();
    let a = producer
        .fact(
            Address(ids[0].path().fingerprint()),
            summary(ids[0], vec![]),
            &reads,
            selected,
        )
        .unwrap();
    let b = producer
        .fact(
            Address(ids[1].path().fingerprint()),
            summary(
                ids[0],
                vec![(parameter(ids[0]), parameter(ids[0]), true, sites[0])],
            ),
            &reads,
            selected,
        )
        .unwrap();
    // The former store-poisoning vector now has no archive lookup path. Keep
    // its single-root source verdict in the live producer law.
    let single = producer
        .fact(
            Address(ids[0].path().fingerprint()),
            summary(
                ids[0],
                vec![(parameter(ids[0]), parameter(ids[0]), true, sites[0])],
            ),
            &reads,
            selected,
        )
        .unwrap();
    let single = settle(&producer, vec![single]).unwrap();
    assert_eq!(&*single.components[0].growing, &[sites[0]]);
    assert_ne!(a.definition, b.definition);
    let facets = settle(&producer, vec![a, b]).unwrap();
    assert_eq!(facets.components.len(), 1);
    assert_eq!(&*facets.components[0].growing, &[sites[0]]);
    let bounded = SourceGrowthProducer {
        scratch: 1,
        ..producer
    };
    assert!(settle(&bounded, vec![]).is_none());
}

#[test]
fn reordered_sources_preserve_canonical_settled_component_values() {
    let mut previous = None;
    for text in ["fn a(){}\nfn b(){}\n", "fn b(){}\nfn a(){}\n"] {
        let (_engine, source, ids, sites) = context_with(text);
        let pool = output_pool();
        let producer = SourceGrowthProducer {
            source: &source,
            output_pool: &pool,
            namespace: Address(ContentHash::of_bytes(b"canonical source order")),
            scratch: 8 << 20,
        };
        let reads = ReadsBuilder::new().finish().unwrap();
        let selected = SELECTED.reference(b"accepted checked output").unwrap();
        let facts: Vec<_> = (0..2)
            .map(|i| {
                producer
                    .fact(
                        Address(ids[i].path().fingerprint()),
                        summary(
                            ids[i],
                            vec![(parameter(ids[i]), parameter(ids[1 - i]), true, sites[i])],
                        ),
                        &reads,
                        selected,
                    )
                    .unwrap()
            })
            .collect();
        let facets = settle(&producer, facts).unwrap();
        assert_eq!(facets.components.len(), 1);
        let component = &facets.components[0];
        let owners: Vec<_> = component
            .growing
            .iter()
            .map(|site| source.record_owner(site.anchor).unwrap().0.0.name)
            .collect();
        assert_eq!(owners, ["a", "b"]);
        if let Some(previous) = previous {
            assert_eq!(output(&component.product), previous);
        }
        previous = Some(output(&component.product));
    }
}

#[test]
fn membership_record_reopens_through_the_same_native_codec_as_components() {
    let (engine, source, ids, _) = context();
    let pool = output_pool();
    let producer = SourceGrowthProducer {
        source: &source,
        output_pool: &pool,
        namespace: Address(ContentHash::of_bytes(b"roundtrip")),
        scratch: 8 << 20,
    };
    let selected = SELECTED.reference(b"checked").unwrap();
    let fact = producer
        .fact(
            Address(ids[0].path().fingerprint()),
            summary(ids[0], vec![]),
            &Reads::empty(),
            selected,
        )
        .unwrap();
    let membership = producer.membership(&[fact]).unwrap();
    let store = Arc::new(crate::store::MemStore::default());
    let records = crate::NativeRecords::new(engine.types().clone(), store);
    let product = membership.product;
    let data = crate::runtime::SourceGrowthQueryData::Membership {
        value: Arc::new(membership),
        reads: Arc::new(Reads::empty()),
    };
    let bytes = records.encode(&source, &data).unwrap();
    let record: SourceGrowthQueryRecord = records
        .decode(&source, &bytes)
        .expect("membership archive must decode to its durable record");
    let SourceGrowthQueryRecord::Membership {
        product: restored,
        components,
        reads,
    } = record
    else {
        panic!("membership tag changed");
    };
    assert_eq!(restored, product);
    assert_eq!(components.len(), 1);
    assert!(reads.is_empty());
}

#[test]
fn exact_component_facet_rejects_a_changed_verdict_with_the_same_digest() {
    let (_engine, source, ids, sites) = context();
    let pool = output_pool();
    let producer = SourceGrowthProducer {
        source: &source,
        output_pool: &pool,
        namespace: Address(ContentHash::of_bytes(b"collision")),
        scratch: 8 << 20,
    };
    let action = Action(ContentHash::default());
    let accepted = producer
        .component_value(vec![ids[0]].into_boxed_slice(), Box::new([]), action)
        .unwrap();
    let mut rejected = producer
        .component_value(
            vec![ids[0]].into_boxed_slice(),
            vec![sites[0]].into_boxed_slice(),
            action,
        )
        .unwrap();
    Arc::get_mut(&mut rejected).unwrap().product.output = accepted.product.output;
    assert!(
        !crate::summary::SettledSummary::component(&accepted)
            .equivalent(&crate::summary::SettledSummary::component(&rejected))
    );
}

#[test]
fn compact_source_fact_releases_checked_input_and_admits_growth_backing() {
    let (_engine, source, ids, sites) = context();
    let output_pool = output_pool();
    let producer = SourceGrowthProducer {
        source: &source,
        output_pool: &output_pool,
        namespace: Address(ContentHash::of_bytes(b"compact fact ownership")),
        scratch: 8 << 20,
    };
    let edge = (parameter(ids[0]), parameter(ids[0]), true, sites[0]);
    let checked = summary(ids[0], vec![edge]);
    let old_owner = Arc::downgrade(&checked);
    let fact = producer
        .fact(
            Address(ids[0].path().fingerprint()),
            checked.clone(),
            &Reads::empty(),
            SELECTED.reference(b"checked compact growth").unwrap(),
        )
        .unwrap();
    drop(checked);
    assert!(
        old_owner.upgrade().is_none(),
        "a growth fact must release the complete checked input"
    );
    assert_eq!(
        fact.input_bytes(),
        (std::mem::size_of::<SourceGrowthFact>() + std::mem::size_of::<DefId>()) as u64
            + crate::memo::retained::retained_arc_bytes(&[edge][..]),
        "the compact fact admits its surviving edge allocation",
    );
    let settled = settle(&producer, vec![fact]).unwrap();
    assert_eq!(settled.components[0].growing.as_ref(), &[sites[0]]);
}
