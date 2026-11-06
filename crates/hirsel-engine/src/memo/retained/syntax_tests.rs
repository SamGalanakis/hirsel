use hirsel_query_products::test_support::syntax_fixture as fixture;
use std::sync::Arc;
#[test]
fn indexed_syntax_inspection_covers_item_slots_and_owned_paths_without_query_work() {
    for width in [1, 4, 16] {
        let input = fixture(width);
        let mut engine = crate::QueryEngine::new(crate::default_environment(), Default::default());
        let (_, storage) = crate::test_alloc::measured(|| {
            for i in 0..width {
                let mut syntax = input.clone();
                syntax.item.definition.0.name = format!("item{i}");
                engine
                    .items
                    .insert(syntax.item.definition.clone(), vec![Arc::new(syntax)]);
            }
        });
        let before = engine.work_stats();
        let (bound, inspection) = crate::test_alloc::measured(|| engine.indexed_syntax_bytes());
        assert_eq!(inspection.allocations, 0);
        assert_eq!(engine.work_stats(), before);
        assert!(
            storage.live as u64 <= bound.unwrap(),
            "width {width}: actual {} exceeds {:?}",
            storage.live,
            bound
        );
        let (_, release) = crate::test_alloc::measured(|| engine.items.clear());
        assert_eq!(-release.live, storage.live);
    }
}
