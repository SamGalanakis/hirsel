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

// LANG-8: a `cold fn` stays out of line in release IR and the branch into its
// call is the unlikely arm; `string.push_scalar`, which core marks cold, keeps
// its own body under an inlined `push_char`.
#[test]
fn cold_callee_stays_out_of_line_on_the_unlikely_branch() {
    let source = r#"
cold fn slow(x: u64) -> u64 {
    x / 3u64
}
fn fast(x: u64) -> u64 {
    if x > 1000u64 { slow(x) } else { x }
}
fn append(mut s: String, c: char) {
    string.push_char(mut s, c)
}
"#;
    let (program, names) = hirsel_core_program_tests::SourceProgram::lower(source, None);
    let slow = symbol(&program, names["slow"]);
    let push_scalar = (0..program.functions.len())
        .map(|f| FuncId(f as u32))
        .find(|&f| {
            let instance = program.functions[f.index()].instance;
            program.instances[instance.index()].definition.to_string()
                == "core@1/string/push_scalar"
        })
        .map(|f| symbol(&program, f))
        .unwrap();
    // The cold arm of a `!prof !0` (likely) branch in `fast` reaches the call
    // to `slow` along its own fall-through chain (past the step check).
    let ir =
        emit_ir_for_test_with_facts(&program, Target::X86_64Linux, false, false, true).unwrap();
    let body = emitted_body(&ir, &symbol(&program, names["fast"]));
    fn calls(text: &str, callee: &str) -> bool {
        text.contains(&format!("@{callee}(")) || text.contains(&format!("@{callee}."))
    }
    // The instructions of one block: indented lines up to the next label.
    fn block<'a>(body: &'a str, label: &str) -> Option<&'a str> {
        let (_, rest) = body.split_once(&format!("\n{label}:\n"))?;
        let end = rest.match_indices('\n').find(|&(i, _)| !rest[i + 1..].starts_with(' '));
        Some(&rest[..end.map_or(rest.len(), |(i, _)| i)])
    }
    // Whether the chain of first successors from `label` calls `callee`.
    fn reaches<'a>(body: &'a str, mut label: &'a str, callee: &str) -> bool {
        for _ in 0..4 {
            let Some(text) = block(body, label) else { return false };
            if calls(text, callee) {
                return true;
            }
            let Some(next) = text.lines().last().and_then(|l| l.split("label %").nth(1)) else {
                return false;
            };
            label = next.split([',', ' ']).next().unwrap();
        }
        false
    }
    let unlikely: Vec<&str> = body
        .lines()
        .filter(|l| l.trim_start().starts_with("br i1") && l.ends_with("!prof !0"))
        .filter_map(|l| l.split("label %").nth(2)?.split(',').next())
        .collect();
    assert!(unlikely.iter().any(|&label| reaches(body, label, &slow)), "{body}");
    let optimized = emit_optimized_ir_for_test(&program, Target::X86_64Linux).unwrap();
    for (caller, callee) in [("fast", &slow), ("append", &push_scalar)] {
        let body = emitted_body(&optimized, &symbol(&program, names[caller]));
        assert!(calls(&body, callee), "{caller}: {body}");
    }
}
