#[test]
fn regex_rx3_search_reachability_admits_nothing() {
    use hirsel_core_ir::{Callee, Identity, Inst, RtOp};

    let directory = tempfile::tempdir().unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../core");
    let inputs = CheckRequest::libraries(&root, &directory.path().join("requests")).unwrap();
    let product = Driver::default()
        .check(inputs, &mut Meter::new(false))
        .unwrap();
    let entries = [
        "is_match",
        "find",
        "captures",
        "next",
        "next_captures",
        "count",
    ];
    let roots = entries
        .map(|name| Identity::plain(DefPath::from_full(&format!("std@1/regex/{name}")).unwrap()));
    let units = product.units(roots, 65_536).unwrap();
    for unit in units.modules().unwrap() {
        let body = unit.module.functions[unit.function.index()]
            .body
            .as_ref()
            .unwrap();
        assert!(
            body.insts
                .iter()
                .all(|i| !matches!(i, Inst::Alloc { .. } | Inst::Grow { .. })),
            "regex search can allocate in {:?}",
            unit.identity,
        );
        for call in &body.calls {
            match call.callee {
                Callee::Direct(_) | Callee::Runtime(RtOp::CompareBytes | RtOp::WorkSteps) => {}
                // Mismatched cache/group arguments raise a static message. The
                // standalone representation borrows the literal's bytes.
                Callee::Runtime(RtOp::StaticString | RtOp::ReleaseStaticBytes) => {}
                other => panic!(
                    "unaudited regex search callee {other:?} in {:?}",
                    unit.identity
                ),
            }
        }
    }
}
