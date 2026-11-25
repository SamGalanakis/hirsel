use super::*;

/// A callee whose header is refused has an Error result (PROG-8 poison), which
/// can supply neither a pattern payload nor a value, so a caller matching on it
/// is not emitted. A callee refused only in its body keeps its declared result
/// (69ec0b5f) and does not stop its callers.
#[test]
fn refused_callee_results_stop_emission_on_cold_cached_and_edited_requests() {
    for body in [
        "match broken() { Yes(n) => n, No => 0u64 }",
        "let f = broken; match f() { Yes(n) => n, No => 0u64 }",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let bad = format!(
            "type Choice = Yes(u64) | No\nfn broken() -> Chioce {{ Yes(0u64) }}\nfn answer() -> u64 {{ {body} }}\n"
        );
        let good = bad.replace("Chioce", "Choice");
        for source in [&bad, &bad, &good, &good, &bad, &good] {
            let product = Driver::default()
                .check(request(source, dir.path()), &mut Meter::new(false))
                .unwrap();
            let refused = source == &bad;
            assert_eq!(product.accepted(), !refused, "{:?}", product.diagnostics);
            assert_eq!(
                product
                    .definitions
                    .values()
                    .find(|d| d.definition.0.name == "answer")
                    .unwrap()
                    .check
                    .function
                    .is_none(),
                refused
            );
            if refused {
                assert!(product.diagnostics.iter().any(|r| r.rule == "LANG-7"));
            }
        }
    }
}

#[test]
fn unresolved_closure_patterns_do_not_emit_or_use_undeclared_binders() {
    let dir = tempfile::tempdir().unwrap();
    let source = "fn nested() { let f = scoped fn(o: Option[(u64, u64)]) -> u64 { match o { Some((x, _)) => x } } }\nfn captured(x: Missing) -> u64 { match x { Some(y) => { let f = scoped fn() -> u64 { y }; f() } } }\nfn flags(a: bool) -> u64 { 0u64 }\nfn missing_args(a: List[u64], b: List[u64]) -> u64 { flags(a == b) }\nfn peek(h: Missing) -> u64 { flags(h) }\n";
    for _ in 0..2 {
        let product = Driver::default()
            .check(request(source, dir.path()), &mut Meter::new(false))
            .unwrap();
        assert!(!product.accepted());
        assert!(product.diagnostics.iter().any(|r| r.rule == "LANG-7"));
        assert!(
            product
                .definitions
                .values()
                .filter(|d| d.definition.0.name != "flags")
                .all(|d| d.check.function.is_none())
        );
    }
}

#[test]
fn unresolved_bodies_suppress_cascades_without_hiding_independent_errors() {
    let dir = tempfile::tempdir().unwrap();
    // The refused body's own independent error (`let k`) is reported cold, from
    // its record when warm, and follows a change to the signature it reads (PROG-8
    // poison).
    let source = |other: &str| {
        format!(
            "type Choice = Yes | No(u64)\nfn other() -> {other} {{ {other}_value() }}\nfn pattern(x: Choice) -> u64 {{ let k: u64 = other(); match x {{ Yes => 1u64, Missing(_) => 0u64 }} }}\nfn bool_value() -> bool {{ true }}\nfn u64_value() -> u64 {{ 0u64 }}\n"
        )
    };
    for (attempt, other, expected) in [
        (0, "bool", &[("LANG-11", 2), ("LANG-7", 2)][..]),
        (1, "bool", &[("LANG-11", 2), ("LANG-7", 2)][..]),
        (2, "u64", &[("LANG-7", 2)][..]),
    ] {
        let product = Driver::default()
            .check(request(&source(other), dir.path()), &mut Meter::new(false))
            .unwrap();
        assert!(!product.accepted());
        let mut rules: Vec<_> = product
            .diagnostics
            .iter()
            .map(|r| (r.rule.as_str(), r.primary.item))
            .collect();
        rules.sort_unstable();
        assert_eq!(
            rules, expected,
            "attempt {attempt}: {:?}",
            product.diagnostics
        );
        assert!(product.incomplete.is_empty());
    }
}
