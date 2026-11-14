use super::*;

#[test]
fn empty_bounds_keep_refusal_and_injection_without_a_payer_identity() {
    let module = text::parse(
        r#"core v1
sig g0 = (u64, bool) -> (bool, u64)
inst n0 = tests@0/empty/main [] []
fn f0 = n0 g0 {
b0(v0: u64, v1: bool):
  v2: bool, v3: u64 = limit_enter v0
  branch v2, b1, b4
b1:
  branch v1, b2, b3
b2:
  v4: u64 = const 7
  limit_exit
  return v2, v3
b3:
  v5: u64 = const 8
  limit_exit
  return v2, v3
b4:
  return v2, v3
}
"#,
    )
    .unwrap();
    wf::module(&module).unwrap();
    let ir = emit_ir(&module).unwrap();
    assert!(!ir.contains("@hirsel_core_limit_enter"));
    assert!(!ir.contains("@hirsel_core_limit_exit"));
    let mut cases = Vec::new();
    for charge in [0, 16, 101] {
        for branch in [0, 1] {
            cases.push(Case {
                function: FuncId(0),
                args: vec![charge, branch],
                limit: 100,
                injection: None,
            });
        }
    }
    for later in [false, true] {
        cases.push(Case {
            function: FuncId(0),
            args: vec![16, 0],
            limit: 100,
            injection: Some((1, later)),
        });
    }
    agree_cases(&module, &cases, true);
}

#[test]
fn region_bumps_refill_and_fully_trimmed_entries_keep_the_original_payer() {
    let module = text::parse(
        r#"core v1
sig g0 = (u64, bool) -> (u64)
inst n0 = tests@0/region/main [] []
fn f0 = n0 g0 {
  addr a0 = v10 : u64
b0(v0: u64, v1: bool):
  v2: ptr = region_open
  v3: u64 = const 0
  v4: u64 = const 1
  v5: u64 = const 16
  v6: ptr = const 0x0
  jump b1(v3, v3)
b1(v7: u64, v8: u64):
  v9: bool = lt v7, v0
  branch v9, b2, b8
b2:
  v15: bool, v16: u64 = limit_enter v5
  branch v15, b3, b8
b3:
  v10: ptr, v11: u64, v12: u64 = alloc stable v5 align 8 charge v5 in v2
  v13: bool = ne v10, v6
  branch v13, b4, b7
b4:
  address a0
  store a0, v7
  v14: u64 = read a0
  branch v1, b5, b6
b5:
  trim v10 in v2 entry v12 from v5 charge v3
  jump b6
b6:
  limit_exit
  v17: u64 = wrapping_add v7, v4
  v18: u64 = wrapping_add v8, v14
  jump b1(v17, v18)
b7:
  limit_exit
  jump b8
b8:
  region_close v2
  return v8
}
"#,
    )
    .unwrap();
    wf::module(&module).unwrap();
    let ir = emit_ir(&module).unwrap();
    assert!(ir.contains("@hirsel_core_region_refill"));
    assert!(ir.contains("region_slow"));
    let mut cases = Vec::new();
    for trim in [0, 1] {
        cases.push(Case {
            function: FuncId(0),
            args: vec![3, trim],
            limit: 100,
            injection: None,
        });
    }
    for later in [false, true] {
        for nth in 1..=6 {
            cases.push(Case {
                function: FuncId(0),
                args: vec![3, 1],
                limit: 100,
                injection: Some((nth, later)),
            });
        }
    }
    agree_cases(&module, &cases, true);
    // Root allocations exercise repeated fast bumps and a physical chunk refill.
    let source = text::print(&module)
        .replace(
            "v15: bool, v16: u64 = limit_enter v5",
            "v15: bool = const true\n  v16: u64 = const 0",
        )
        .replace("  limit_exit\n", "");
    let root = text::parse(&source).unwrap();
    wf::module(&root).unwrap();
    agree_cases(
        &root,
        &[Case {
            function: FuncId(0),
            args: vec![5000, 0],
            limit: 100000,
            injection: None,
        }],
        true,
    );
}

#[test]
fn own_fast_paths_and_zero_charge_and_large_alignment_fallbacks_agree() {
    let mut source = String::from("core v1\nsig g0 = (u64) -> (bool, u64)\n");
    for (i, (kind, align)) in [
        ("for seq u8", 8),
        ("for ownfn", 8),
        ("for seq u8", 16),
        ("for ownfn", 16),
        ("for seq u8", 512),
        ("for ownfn", 512),
        ("for seq u8", 1024),
    ]
    .into_iter()
    .enumerate()
    {
        writeln!(source, "inst n{i} = tests@0/own/f{i} [] []\nfn f{i} = n{i} g0 {{\nb0(v0: u64):\n v1: u64 = const 16\n v2: ptr, v3: u64 = alloc {kind} v1 align {align} charge v0\n v4: ptr = const 0x0\n v5: bool = ne v2, v4\n branch v5, b1, b2\nb1:\n free v2 charge v0\n jump b2\nb2:\n return v5, v3\n}}").unwrap();
    }
    let module = text::parse(&source).unwrap();
    wf::module(&module).unwrap();
    let mut cases = Vec::new();
    let on = emit_ir_with_facts(&module, false, true).unwrap();
    assert!(on.contains("call nonnull ptr @hirsel_core_native_alloc_ordinary"));
    assert!(on.contains("getelementptr inbounds i8"));
    let off = emit_ir_with_facts(&module, false, false).unwrap();
    assert!(off.contains("call ptr @hirsel_core_native_alloc_ordinary"));
    assert!(!off.contains("nonnull"));
    assert!(!off.contains("inbounds"));
    for function in 0..7 {
        for charge in [0, 16, 101] {
            cases.push(Case {
                function: FuncId(function),
                args: vec![charge],
                limit: 100,
                injection: None,
            });
        }
        cases.push(Case {
            function: FuncId(function),
            args: vec![16],
            limit: 100,
            injection: Some((1, false)),
        });
        for charge in [0, 1, u64::MAX] {
            cases.push(Case {
                function: FuncId(function),
                args: vec![charge],
                limit: u64::MAX,
                injection: None,
            });
        }
    }
    agree_cases(&module, &cases, true);
}

#[test]
fn counted_hot_paths_are_inline_and_last_release_is_cold() {
    let module = text::parse(include_str!("../../../hirsel-core-ir/samples/counted.core")).unwrap();
    let ir = emit_ir(&module).unwrap();
    assert!(!ir.contains("@hirsel_core_release("));
    assert!(ir.contains("cmpxchg weak ptr"));
    assert!(ir.contains("atomicrmw sub ptr"));
    assert!(!ir.contains("@hirsel_core_counted_init"));
    assert!(ir.contains("store atomic i64 1"));
    assert!(ir.contains("call void @hirsel_core_arc_initialized(ptr %context)"));
    assert!(ir.contains("call void @hirsel_core_arc_shared"));
    assert!(ir.contains("call void @hirsel_core_arc_released"));
    assert!(ir.lines().any(
        |line| line.contains("call void @hirsel_core_release_static_last")
            && line.ends_with(" cold")
    ));
    assert!(ir.contains("!\"branch_weights\""));
}

#[test]
fn atomic_helpers_use_checked_cas_and_release_acquire() {
    let module = Module::default();
    let body = Body::default();
    let failures = failure::Failures::new(&module);
    let mut intrinsics = BTreeSet::new();
    let mut e = FunctionEmitter {
        function: None,
        append_fast: false,
        module: &module,
        body: &body,
        recipes: &[],
        abi: abi::Return::new(&[]),
        has_status: false,
        failures: &failures,
        constants: &[],
        observe_charge: false,
        facts: true,
        aliases: None,
        caller_site: "0",
        debug_location: None,
        anchor: None,
        relocatable_sites: false,
        value_slots: Vec::new(),
        empty_bounds: empty::EmptyBounds::default(),
        code: String::new(),
        allocations: String::new(),
        next: 0,
        intrinsics: &mut intrinsics,
    };
    e.share_node("%node", true);
    e.release_fast("%node", 24, Some("@descriptor"), true);
    let mut ir = format!(
        "define void @test(ptr %context, ptr %node) {{\nentry:\n{}{}  ret void\n}}\ndeclare zeroext i1 @recipe(ptr, ptr, ptr, ptr)\n@descriptor = private constant {{ ptr, i64, i64 }} {{ ptr @recipe, i64 24, i64 8 }}\n",
        e.allocations, e.code
    );
    for decl in intrinsics {
        writeln!(ir, "{decl}").unwrap();
    }
    assert!(ir.contains("cmpxchg weak ptr"));
    assert!(ir.contains("atomicrmw sub ptr"));
    assert!(ir.contains("fence acquire"));
    assert!(!ir.contains("atomicrmw add"));
    let context = Context::new();
    LlvmModule::parse(&context, "atomic-helpers", &ir)
        .unwrap()
        .verify()
        .unwrap();
}

#[test]
fn list_push_without_growth_uses_only_compare_store_and_increment() {
    let module = text::parse(
        r#"core v1
sig g0 = (ptr, u64, u64, u64) -> (u64)
inst n0 = tests@0/push/main [] []
inst n1 = tests@0/push/grow [] []
fn f0 = n0 g0 {
  addr a0 = v0 index v1 : u64
b0(v0: ptr, v1: u64, v2: u64, v3: u64):
  v5: u64 = const 1
  v6: u64 = wrapping_add v1, v5
  v4: bool = le v6, v2
  branch v4, b1, b2
b1:
  address a0
  store a0, v3
  return v6
b2:
  v7: u64 = call f1 g0 (v0, v1, v2, v3)
  return v7
}
fn f1 = n1 g0 extern
"#,
    )
    .unwrap();
    wf::module(&module).unwrap();
    let ir = emitted_ir(&module, Optimization::Speed).unwrap();
    let name = symbol(&module, FuncId(0));
    let definition = ir
        .split(&format!("@{name}("))
        .nth(1)
        .unwrap()
        .split("\n}")
        .next()
        .unwrap();
    let mut hot = false;
    for block in definition.split("\n\n") {
        if block.contains("store i64") {
            hot = true;
            assert!(!block.contains("call "), "{block}");
        }
    }
    assert!(definition.contains("add "));
    assert!(hot, "{definition}");
    assert!(definition.contains(&format!("@{}", symbol(&module, FuncId(1)))));
    assert!(!definition.contains("@hirsel_core_"));
}

#[test]
fn arc_initialization_share_release_and_unwrap_preserve_provider_events() {
    let source = include_str!("../../../hirsel-core-ir/samples/counted.core");
    let module = text::parse(source).unwrap();
    wf::module(&module).unwrap();
    let case = Case {
        function: FuncId(2),
        args: vec![5],
        limit: 1000,
        injection: None,
    };
    let reports = execute(&module, &[case]);
    assert_eq!(
        reports[0].arc_events,
        vec![(10, 1), (11, 2), (12, 0), (12, 1)]
    );
    assert_eq!(reports[0].outcome, eval::Outcome::Returned(vec![]));
    assert_eq!(reports[0].used, 0);
    // Reuse the same ownership/unwrapping program with Arc carriers and operations.
    let source = source.replace(" rc", " arc");
    let module = text::parse(&source).unwrap();
    wf::module(&module).unwrap();
    let reports = execute(
        &module,
        &[Case {
            function: FuncId(1),
            args: vec![5],
            limit: 1000,
            injection: None,
        }],
    );
    assert_eq!(
        reports[0].arc_events,
        vec![(10, 1), (11, 2), (13, 0), (12, 0), (13, 1)]
    );
    assert_eq!(
        reports[0].outcome,
        eval::Outcome::Returned(vec![1, 0, 1, 5])
    );
    assert_eq!(reports[0].used, 0);
}
