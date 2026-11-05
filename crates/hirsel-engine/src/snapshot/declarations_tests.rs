use super::*;

thread_local! {
    pub(super) static FILE_UPDATES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[test]
fn unchanged_file_placement_does_no_index_updates() {
    hirsel_intern::jobs::with_compiler_stack(|| {
        let mut engine = crate::QueryEngine::new(
            crate::default_environment(),
            crate::phases::engine_only_adapters(),
        );
        let file = engine
            .sources
            .add(
                "siblings.hir",
                "pub fn z(){()}\npub fn a(){()}\npub fn a(){()}\n".into(),
            )
            .unwrap();
        let items = engine
            .parse_file(file, &crate::default_context())
            .unwrap()
            .value
            .items;
        let mut index = engine.declarations.clone();
        let frozen = index.frozen();
        let updates = FILE_UPDATES.get();
        let mut reversed = items.clone();
        reversed.reverse();
        index.place_file(file, reversed, &engine.sources);
        index.place_file(file, items.clone(), &engine.sources);
        assert_eq!(
            FILE_UPDATES.get(),
            updates,
            "unchanged siblings require no map rewrites"
        );
        assert!(Arc::ptr_eq(&frozen, &index.frozen()));
        assert_eq!(
            frozen
                .siblings(&items[0])
                .iter()
                .map(|item| item.item.definition.0.name.as_str())
                .collect::<Vec<_>>(),
            ["z", "a", "a"]
        );
        let second_base = engine.sources.anchor(items[1].anchor).unwrap().1;
        let last_base = engine.sources.anchor(items[2].anchor).unwrap().1;
        engine.sources.move_anchor(items[1].anchor, file, last_base);
        engine
            .sources
            .move_anchor(items[2].anchor, file, second_base);
        index.place_file(file, items.clone(), &engine.sources);
        assert_eq!(
            index
                .frozen()
                .siblings(&items[0])
                .iter()
                .map(|item| item.anchor)
                .collect::<Vec<_>>(),
            [items[0].anchor, items[2].anchor, items[1].anchor],
            "retained syntax still follows moved source anchors"
        );
        index.place_file(file, vec![], &engine.sources);
        assert!(index.frozen().siblings(&items[0]).is_empty());
        assert!(
            index
                .snapshots(&engine.sources.file(file).unwrap().path)
                .is_empty()
        );
        assert_eq!(frozen.siblings(&items[0]).len(), items.len());
    });
}

fn row(path: &DefPath, anchor: Anchor) -> DeclarationRow {
    DeclarationRow {
        definition: DefId::intern(path),
        types: true,
        visibility: Visibility::PublicOpaque,
        span: Span {
            anchor,
            start: 1,
            end: 2,
        },
        metadata: DefinitionMetadata {
            public: true,
            alias: false,
        },
        site_spans: vec![
            Span {
                anchor,
                start: 1,
                end: 2,
            },
            Span {
                anchor,
                start: 7,
                end: 8,
            },
        ],
        headers: vec![
            ContentHash::of_bytes(b"first"),
            ContentHash::of_bytes(b"second"),
        ],
        candidates: ContentHash::of_bytes(b"ordered candidates"),
        identity: [1, 2, 3, 4],
    }
}

#[test]
fn declaration_rows_share_ownership_and_materialize_sites_only_on_demand() {
    hirsel_intern::jobs::with_compiler_stack(|| {
        let path = DefPath::core("compact_declaration");
        let id = DefId::intern(&path);
        let anchor = Anchor::fresh();
        let expected = row(&path, anchor);
        let mut image = DeclarationIndex::default();
        image.replace_row(&path, Some(Arc::new(Row::from(expected.clone()))));
        let mut session = DeclarationIndex::default();
        session.adopt(&image);
        assert!(Arc::ptr_eq(&image.rows[&id], &session.rows[&id]));
        assert!(image.rows[&id].sites.get().is_none());
        let frozen = session.frozen();
        assert!(
            image.rows[&id].sites.get().is_none(),
            "freezing must keep compact rows"
        );
        assert_eq!(session.identity(), image.identity());
        assert_eq!(session.candidates(&path), Some(expected.candidates));
        assert_eq!(session.headers(&path), Some(expected.headers.as_slice()));
        assert_eq!(session.paths().collect::<Vec<_>>(), [&path]);
        let sites = frozen.sites(&path);
        assert_eq!(
            sites.iter().map(|s| s.name_span).collect::<Vec<_>>(),
            expected.site_spans
        );
        assert!(sites.iter().all(|s| s.definition == path));
        assert!(Arc::ptr_eq(&sites, image.rows[&id].sites()));
        assert!(Arc::ptr_eq(&sites, &frozen.sites(&path)));
        let missing = DefPath::core("absent_compact_declaration");
        assert!(session.candidates(&missing).is_none());
        assert!(frozen.sites(&missing).is_empty());
        let replacement = row(&path, Anchor::fresh());
        session.replace_row(&path, Some(Arc::new(Row::from(replacement.clone()))));
        assert!(!Arc::ptr_eq(&image.rows[&id], &session.rows[&id]));
        assert_eq!(frozen.sites(&path)[0].name_span.anchor, anchor);
        assert_eq!(
            session.frozen().sites(&path)[0].name_span,
            replacement.site_spans[0]
        );
    });
}

#[test]
fn compact_row_roundtrip_preserves_order_visibility_and_exact_anchors() {
    hirsel_intern::jobs::with_compiler_stack(|| {
        let path = DefPath::core("compact_row_roundtrip");
        let id = DefId::intern(&path);
        let anchor = Anchor::fresh();
        let expected = row(&path, anchor);
        let records = hirsel_product_codec::TypeRecords::new(
            hirsel_types::TypeInterner::new(None),
            Arc::new(hirsel_artifact_store::Memory::default()),
        );
        let bytes = records
            .encode_record(&expected, |a| (a == anchor).then(|| (path.clone(), 0)))
            .unwrap();
        let decoded_anchor = Anchor::fresh();
        let actual: DeclarationRow = records
            .decode_record(&bytes, |p, occurrence| {
                (p == &path && occurrence == 0).then_some(decoded_anchor)
            })
            .unwrap();
        assert_eq!(actual.definition, id);
        assert_eq!(actual.visibility, expected.visibility);
        assert_eq!(actual.headers, expected.headers);
        assert_eq!(actual.candidates, expected.candidates);
        assert_eq!(actual.identity, expected.identity);
        for (actual, expected) in actual.site_spans.iter().zip(&expected.site_spans) {
            assert_eq!(actual.anchor, decoded_anchor);
            assert_eq!((actual.start, actual.end), (expected.start, expected.end));
        }
        let actual = Row::from(actual);
        assert!(actual.sites.get().is_none());
        assert_eq!(actual.entry().definition, path);
        assert_eq!(actual.entry().namespace, Namespace::Type);
        assert_eq!(actual.sites().len(), 2);
    });
}
