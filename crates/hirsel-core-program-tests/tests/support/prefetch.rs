// `list.prefetch` and `inline.prefetch`: one read hint and no bounds check.
#[test]
fn prefetch_lowers_to_llvm_prefetch_with_no_bounds_branch() {
    // The loops do nothing but prefetch, so the only failure a body could
    // raise is a bounds check on the index (core.md, Prefetch).
    let source = r#"
fn warm_list(xs: List[u64], n: u64) {
    for i in 0u64 .. n {
        list.prefetch(xs, i)
    }
}
fn warm_inline(a: Inline[u32, 8], n: u64) {
    for i in 0u64 .. n {
        inline.prefetch(a, i)
    }
}
"#;
    let (program, names) = hirsel_core_program_tests::SourceProgram::lower(source, None);
    let ir =
        emit_ir_for_test_with_facts(&program, Target::X86_64Linux, false, false, true).unwrap();
    verify_ir(&ir);
    for name in ["warm_list", "warm_inline"] {
        let body = emitted_body(&ir, &symbol(&program, names[name]));
        // The step paths copy the loop body, so each copy holds one hint: a read
        // (0) with high locality (3) of data (1).
        let hints = body.matches("call void @llvm.prefetch.p0(ptr ").count();
        assert!(hints > 0, "{body}");
        assert_eq!(body.matches(", i32 0, i32 3, i32 1)").count(), hints, "{body}");
        assert!(!body.contains("@hirsel_core_raise"), "{body}");
    }
}
