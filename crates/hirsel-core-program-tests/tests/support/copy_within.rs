// `list.copy_within` and `inline.copy_within` against hand-derived results.
#[test]
fn copy_within_lowers_to_one_memmove_and_agrees_on_overlap_and_edges() {
    let source = r#"
fn shift_list(mut xs: List[u16], src: u64, dst: u64, count: u64) {
    list.copy_within(mut xs, src, dst, count)
}
fn shift_inline(mut a: Inline[u64, 8], src: u64, dst: u64, count: u64) {
    inline.copy_within(mut a, src, dst, count)
}
fn on_list(src: u64, dst: u64, count: u64) -> u64 {
    let mut xs: List[u16] = list.with_capacity(5)
    for i in 0u64 .. 5u64 {
        list.push(mut xs, int.narrow(i + 1u64))
    }
    list.copy_within(mut xs, src, dst, count)
    let mut out = 0u64
    for x in xs {
        out = out * 10u64 + int.widen(x)
    }
    out
}
fn on_inline(src: u64, dst: u64, count: u64) -> u64 {
    let mut a: Inline[u16, 5] = [1, 2, 3, 4, 5]
    inline.copy_within(mut a, src, dst, count)
    let mut out = 0u64
    for i in 0u64 .. 5u64 {
        out = out * 10u64 + int.widen(a[i])
    }
    out
}
"#;
    let (program, names) = hirsel_core_program_tests::SourceProgram::lower(source, None);
    let ir =
        emit_ir_for_test_with_facts(&program, Target::X86_64Linux, false, false, true).unwrap();
    verify_ir(&ir);
    for name in ["shift_list", "shift_inline"] {
        let body = emitted_body(&ir, &symbol(&program, names[name]));
        assert_eq!(body.matches("call void @llvm.memmove").count(), 1, "{body}");
        assert!(!body.contains(" phi "), "{body}");
    }
    // [1, 2, 3, 4, 5] by hand: dst after src, dst before src, an empty copy at
    // dst == len, the full length, then a source past the end, an empty copy
    // whose src lies past the end, and an end that wraps below the length.
    let runs: [((u64, u64, u64), Option<u64>); 7] = [
        ((0, 1, 3), Some(11235)),
        ((2, 0, 3), Some(34545)),
        ((0, 5, 0), Some(12345)),
        ((0, 0, 5), Some(12345)),
        ((1, 0, 5), None),
        ((6, 0, 0), None),
        ((1, 2, u64::MAX), None),
    ];
    let functions = [names["on_list"], names["on_inline"]];
    let mut cases = Vec::new();
    let mut expected = Vec::new();
    for function in functions {
        for ((src, dst, count), out) in runs {
            cases.push(Case {
                function,
                args: vec![src, dst, count],
                limit: 4096,
                injection: None,
            });
            expected.push(match out {
                Some(v) => eval::Outcome::Returned(vec![v]),
                None => eval::Outcome::Failed(vec!["Bounds".into()]),
            });
        }
    }
    for (case, outcome) in cases.iter().zip(&expected) {
        assert_eq!(
            eval::run(&program, case.function, &case.args, case.limit).outcome,
            *outcome,
            "{:?}",
            case.args
        );
    }
    let cranelift = program.cranelift_object("x86_64-unknown-linux-gnu", false, false, false);
    let llvm = program.llvm_program(Target::X86_64Linux, false, false, &functions);
    for (mode, object) in [(Optimization::None, cranelift), (Optimization::Speed, llvm)] {
        let reports = execute_object(&program, &cases, "", "", mode, Some(object));
        for ((report, outcome), case) in reports.into_iter().zip(&expected).zip(&cases) {
            assert_eq!(&report.outcome, outcome, "{mode:?} {:?}", case.args);
        }
    }
}
