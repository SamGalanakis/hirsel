//! Rejected source checks retain the same obligations as an independent cold check.
use hirsel_driver::session;
use hirsel_engine::CompileConfig;
use hirsel_engine::store;
use hirsel_front_phases::COMPILER;
use hirsel_intern::jobs::with_compiler_stack;
use std::sync::Arc;

#[test]
fn failed_representation_edit_withholds_other_file_lower_obligations_like_cold() {
    rejected_representation_edit_matches_cold(
        "pub opaque type Secret: data uses {} { level:u64 }\nfn view(x:Secret)->u64{x.level}\npub fn main()->u64{1}\n",
    );
}

#[test]
fn failed_callee_representation_edit_drops_callers_cached_nonretaining_proof_like_cold() {
    rejected_representation_edit_matches_cold(
        "pub opaque type Secret: data uses {} { level:u64 }\nfn view(x:Secret)->u64{x.level}\nfn relay(x:Secret)->u64{view(x)}\npub fn main()->u64{1}\n",
    );
}

fn rejected_representation_edit_matches_cold(text: &str) {
    with_compiler_stack(|| {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("main.hir");
        let manifest = directory.path().join("hirsel.json");
        std::fs::write(&source, text).unwrap();
        std::fs::write(
            directory.path().join("values.hir"),
            "const VALUE:u64=comptime 17\nfn read()->u64{VALUE}\n",
        )
        .unwrap();
        std::fs::write(&manifest, r#"{"schema":1,"package":{"name":"failed_scope","version":"1"},"sources":[{"path":"main.hir","module":[]},{"path":"values.hir","module":[]}],"dependencies":[],"entry":{"module":[],"name":"main"}}"#).unwrap();
        let config = CompileConfig {
            store: Arc::new(store::NoneStore),
            ..Default::default()
        };
        let mut warm = session::Session::open_configured(&manifest, config.clone());
        assert!(warm.check_applied().unwrap().diagnostics.is_empty());
        warm.checked_products(false).unwrap();
        let edited = text.replace("level:u64", "level:u32");
        std::fs::write(&source, &edited).unwrap();
        warm.apply(vec![session::FileChange {
            path: source.clone(),
            bytes: Some(edited.into_bytes()),
        }]);
        let actual_check = warm.check_applied().unwrap();
        assert!(!actual_check.diagnostics.is_empty());
        let actual = warm.checked_products(true).unwrap();
        let mut cold = hirsel_requests::CompilerRequests::for_host(
            COMPILER.with_config(config.clone()),
            config.clone(),
        );
        let completed = cold
            .execute(
                &manifest,
                hirsel_requests::CompileOperation::CheckedProducts {
                    lowering: config.lowering,
                    canonical: true,
                },
            )
            .unwrap();
        let hirsel_requests::CompileResult::CheckedProducts {
            checked: expected_check,
            products: expected,
        } = &completed.result
        else {
            panic!("expected cold checked products")
        };
        let render = |check: &hirsel_engine::CheckOutput| {
            check
                .diagnostics
                .iter()
                .map(|d| d.render_human(&check.sources))
                .collect::<Vec<_>>()
        };
        assert_eq!(render(&actual_check), render(expected_check));
        assert_eq!(actual_check.completion, expected_check.completion);
        assert!(
            actual.canonical == expected.canonical,
            "canonical lengths {:?}/{:?}",
            actual.canonical.as_ref().map(|text| text.len()),
            expected.canonical.as_ref().map(|text| text.len())
        );
        assert_eq!(actual.canonical_reads(), expected.canonical_reads());
        assert_eq!(actual.digest, expected.digest);
    });
}
