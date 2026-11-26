use super::*;
mod cancel_fixture;
mod clang_abi;
mod foreign_attrs;
mod host_abi;
mod memory_tests;

use hirsel_core_eval as eval;
use hirsel_core_ir::{text, wf};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
#[path = "../../../hirsel-core-rt/testdata/host_memory.rs"]
mod host_memory;

#[test]
fn cleared_scalar_carriers_do_not_drop_saved_storage_but_keep_finalizers() {
    let module = text::parse(include_str!(
        "../../tests/core_runtime/cleared-carrier.core"
    ))
    .unwrap();
    wf::module(&module).unwrap();
    let mut cases = Vec::new();
    for value in [0, 7, 255] {
        for limit in [0, 8] {
            cases.push(Case {
                function: FuncId(1),
                args: vec![value],
                limit,
                injection: None,
            });
        }
    }
    cases.push(Case {
        function: FuncId(1),
        args: vec![7],
        limit: 8,
        injection: Some((1, false)),
    });
    cases.push(Case {
        function: FuncId(2),
        args: vec![],
        limit: 8,
        injection: None,
    });
    for optimization in [Optimization::None, Optimization::Speed] {
        let got = execute_optimized(&module, &cases, "", "", optimization);
        for (case, actual) in cases.iter().zip(got) {
            let expected = eval::run_injected(
                &module,
                case.function,
                &case.args,
                case.limit,
                case.injection,
            );
            assert_eq!(actual.outcome, expected.outcome);
            assert_eq!(actual.used, expected.used);
            assert_eq!(actual.admissions, expected.admissions);
        }
    }
}

/// A carrier shadow's storage begins on an emitter temporary; scoped slot
/// homes (`%s…`) carry markers of their own.
const SHADOW_START: &str = "call void @llvm.lifetime.start.p0(ptr %t";

#[test]
fn scalar_growth_shadow_copies_back_before_success_failure_and_forwarded_return() {
    let source = include_str!("../../tests/core_runtime/carrier-shadow.core");
    let nonterminal = " v2: u64 = const 8\n address a1\n v3: u64 = read a1\n store a1, v3\n";
    for terminal in [false, true] {
        let source = if terminal {
            source.replace(nonterminal, " v2: u64 = const 8\n")
        } else {
            source.into()
        };
        for blind in [true, false] {
            let source = if blind {
                source.clone()
            } else {
                source.replace(
                    " v5: u32 = const 0",
                    " v13: bool = eq v0, v0\n v5: u32 = const 0",
                )
            };
            let module = text::parse(&source).unwrap();
            wf::module(&module).unwrap();
            let facts = hirsel_core_ir::roles::Facts::new(&module);
            assert_eq!(facts.place_blind(FuncId(1), 0), blind);
            assert!(
                !facts.sealed(FuncId(1), 0),
                "the carrier really copies between slots"
            );
            let records = facts.record(&module);
            let mut declaration = module.clone();
            declaration.functions[1].body = None;
            let no_record =
                emit_ir_relocatable_for_target(&declaration, Target::X86_64Linux, false, false)
                    .unwrap();
            assert!(!no_record.contains(SHADOW_START));
            let installed = hirsel_core_ir::roles::Facts::program(&[&declaration], &records)
                .pop()
                .unwrap();
            let restored = emit_ir_relocatable_with_role_facts_for_target(
                &declaration,
                Target::X86_64Linux,
                false,
                false,
                &installed,
            )
            .unwrap();
            assert_eq!(restored.contains(SHADOW_START), blind);
            let ir = emit_ir_with_facts(&module, false, true).unwrap();
            assert_eq!(ir.contains(SHADOW_START), blind, "{ir}");
            let off = emit_ir_with_facts(&module, false, false).unwrap();
            assert!(!off.contains(SHADOW_START));
            let cases = [0, 1].map(|mode| Case {
                function: FuncId(2),
                args: vec![mode],
                limit: u64::MAX,
                injection: None,
            });
            for opt in [Optimization::None, Optimization::Speed] {
                let reports = execute_optimized(&module, &cases, "", "", opt);
                for (case, report) in cases.iter().zip(reports) {
                    assert_eq!(
                        report.outcome,
                        eval::run(&module, case.function, &case.args, case.limit).outcome
                    );
                    if case.args[0] == 0 {
                        assert_eq!(report.outcome, eval::Outcome::Returned(vec![7, 0]));
                    }
                    assert_eq!(report.used, 0);
                }
                let context = crate::context::Context::new();
                let direct = crate::context::Module::parse(&context, "hirsel-core", &ir).unwrap();
                let machine = crate::context::TargetMachine::core(
                    hirsel_core_layout::Target::X86_64Linux,
                    matches!(opt, Optimization::Speed),
                )
                .unwrap();
                direct.set_target(&machine);
                if matches!(opt, Optimization::Speed) {
                    direct.run_core_passes(&machine).unwrap();
                }
                direct.verify().unwrap();
                assert_eq!(
                    direct.emit_object(&machine).unwrap(),
                    hirsel_llvm_lazy::compile_ir(&ir, matches!(opt, Optimization::Speed)).unwrap()
                );
            }
        }
    }
}

#[test]
fn exposed_helpers_keep_imported_carrier_facts() {
    let module = text::parse(include_str!("../../tests/core_runtime/carrier-shadow.core")).unwrap();
    wf::module(&module).unwrap();
    let facts = hirsel_core_ir::roles::Facts::new(&module);
    let mut caller = module.clone();
    caller.functions[1].body = None;
    assert_ne!(*expose::module(Cow::Borrowed(&caller), true), caller);
    assert!(facts.place_blind(FuncId(1), 0));
    assert!(!hirsel_core_ir::roles::Facts::new(&caller).place_blind(FuncId(1), 0));
    let ir = carrier_ir(&caller, &facts, Target::X86_64Linux, false);
    assert!(ir.contains(SHADOW_START));
    let mut callee = module.clone();
    callee.functions[0].body = None;
    callee.functions[2].body = None;
    let callee = carrier_ir(&callee, &facts, Target::X86_64Linux, false);
    let object = hirsel_llvm_lazy::emit_program(
        &[
            hirsel_llvm_lazy::Unit::Text(&ir),
            hirsel_llvm_lazy::Unit::Text(&callee),
        ],
        &[symbol(&module, FuncId(2))],
    )
    .unwrap();
    let cases = [0, 1].map(|mode| Case {
        function: FuncId(2),
        args: vec![mode],
        limit: u64::MAX,
        injection: None,
    });
    let reports = execute_object(&module, &cases, "", "", Optimization::Speed, Some(object));
    for (case, report) in cases.iter().zip(reports) {
        assert_eq!(
            report.outcome,
            eval::run(&module, case.function, &case.args, case.limit).outcome
        );
        assert_eq!(report.used, 0);
    }
}

#[test]
fn exposure_bounds_a_branching_recursion_by_its_growth_not_the_budget() {
    // f0 recurses three times. Depth alone allows 3^7 copies, which the 2048
    // budget would cut off. The copies f0 makes inside its own copies share
    // one 450-instruction limit in the caller. The caller's terminal call
    // keeps its forwarding boundary, so one entry is exposed.
    let module = text::parse(
        "core v1
sig g0 = (u64) -> ()
inst n0 = tests@0/expose/fan [] []
inst n1 = tests@0/expose/entry [] []
fn f0 = n0 g0 {
b0(v0: u64):
 v1: u64 = const 0
 v2: bool = eq v0, v1
 branch v2, b1, b2
b1:
 return
b2:
 v3: u64 = const 1
 v4: u64 = wrapping_sub v0, v3
 call f0 g0 (v4)
 call f0 g0 (v4)
 call f0 g0 (v4)
 return
}
fn f1 = n1 g0 {
b0(v0: u64):
 call f0 g0 (v0)
 call f0 g0 (v0)
 return
}
",
    )
    .unwrap();
    wf::module(&module).unwrap();
    let size = |m: &Module, f: usize| m.functions[f].body.as_ref().unwrap().insts.len();
    let exposed = expose::module(Cow::Borrowed(&module), false).into_owned();
    let copies = (size(&exposed, 1) - size(&module, 1)) / size(&module, 0);
    assert!(
        copies > 1,
        "the entry copy and some recursive copies are exposed"
    );
    assert!(
        copies <= 1 + 450 / size(&module, 0),
        "{copies} copies of the recursion"
    );
}

#[test]
fn storage_roles_keep_moved_buffers_in_one_scope() {
    let module = text::parse(include_str!("../../tests/core_runtime/roles.core")).unwrap();
    wf::module(&module).unwrap();
    let mut items = Items::new(&module);
    let scopes = alias::Scopes::new(
        &module,
        &hirsel_core_ir::roles::Facts::new(&module),
        FuncId(0),
        "test",
        &mut 2,
        &mut sink::TextSink::default(),
        &mut items,
    )
    .unwrap();
    assert_eq!(
        scopes.access(&[AddressId(3)]),
        scopes.access(&[AddressId(7)]),
        "a move changes the carrier, not the buffer role"
    );
    let on = emit_ir_with_facts(&module, false, true).unwrap();
    assert!(on.contains("@llvm.experimental.noalias.scope.decl(metadata !{"));
    assert!(on.contains("!alias.scope !{"));
    assert!(on.contains("!noalias !{"));
    let off = emit_ir_with_facts(&module, false, false).unwrap();
    assert!(!off.contains("!alias.scope"));
    assert!(!off.contains("!noalias"));
    assert!(!off.contains("@llvm.experimental.noalias.scope.decl"));
    agree(&module, &[(FuncId(0), vec![])]);
}

#[test]
fn loan_facts_and_diagnostic_off_preserve_abi_and_legal_aliases() {
    let module = text::parse(include_str!("../../tests/core_runtime/loans.core")).unwrap();
    wf::module(&module).unwrap();
    let on = emit_ir_with_facts(&module, false, true).unwrap();
    let off = emit_ir_with_facts(&module, false, false).unwrap();
    assert!(on.contains("ptr noalias nofree readonly dereferenceable(24) %p0"));
    assert!(on.contains("ptr noalias nofree %dest"));
    for function in [2, 3] {
        let name = symbol(&module, FuncId(function));
        let definition = on
            .lines()
            .find(|l| l.starts_with("define ") && l.contains(&format!("@{name}(")))
            .unwrap();
        assert!(definition.contains("ptr noalias nofree dereferenceable(24) %p0"));
    }
    for fact in [
        "noalias",
        "readonly",
        "dereferenceable",
        "nocapture",
        "captures(",
        "invariant",
        "!range",
    ] {
        assert!(!off.contains(fact), "fact retained: {fact}");
    }
    assert!(off.contains("sret("), "ABI return convention stays intact");
    for source in [
        include_str!("../../../hirsel-core-ir/samples/counted.core"),
        include_str!("../../../hirsel-core-ir/samples/strings.core"),
    ] {
        let module = text::parse(source).unwrap();
        let off = emit_ir_with_facts(&module, false, false).unwrap();
        for fact in ["noalias", "readonly", "invariant", "memory(", "nounwind"] {
            assert!(!off.contains(fact), "fact retained: {fact}");
        }
    }
    // Views may overlap, and a copyable take can alias another view. A view
    // can escape through a returned pointer; mut writes remain observable.
    agree(&module, &[(FuncId(0), vec![])]);
}

#[test]
fn viewed_infallible_wrappers_keep_body_tokens_in_the_body() {
    let module = text::parse(
        r#"core v1
type t0 = own "Bytes" { data: seq u8 }
sig g0 = (&view t0) -> ()
sig g1 = (&view t0) -> (u64)
inst n0 = tests@1/view_wrapper/empty [] []
inst n1 = tests@1/view_wrapper/length [] []
fn f0 = n0 g0 {
b0(v0: ptr):
  return
}
fn f1 = n1 g1 {
  addr a0 = v0 : t0
  addr a1 = a0 field 0 byte v1 : u64
b0(v0: ptr):
  v1: u64 = const 8
  address a0
  address a1
  v2: u64 = read a1
  return v2
}
"#,
    )
    .unwrap();
    wf::module(&module).unwrap();
    let ir = emit_ir_with_facts(&module, false, true).unwrap();
    verify_ir(&ir);
    for optimization in [Optimization::None, Optimization::Speed] {
        emit_object(&module, optimization).unwrap();
        hirsel_llvm_lazy::compile_ir(&ir, matches!(optimization, Optimization::Speed)).unwrap();
    }
    hirsel_llvm_lazy::emit_program(
        &[hirsel_llvm_lazy::Unit::Core {
            module: &module,
            facts: &hirsel_core_ir::roles::Facts::new(&module),
            observe_charge: false,
            debug_lines: false,
        }],
        &[symbol(&module, FuncId(0)), symbol(&module, FuncId(1))],
    )
    .unwrap();
}

#[test]
fn view_invariants_end_once_at_the_shared_return() {
    let module = text::parse(
        r#"core v1
type t0 = own "Bytes" { data: seq u8 }
sig g0 = (&view t0, bool) -> ()
sig g1 = (&view t0, bool) -> (u64)
inst n0 = tests@1/view_exit/empty [] []
inst n1 = tests@1/view_exit/value [] []
fn f0 = n0 g0 {
b0(v0: ptr, v1: bool):
  branch v1, b1, b2
b1:
  return
b2:
  return
}
fn f1 = n1 g1 {
b0(v0: ptr, v1: bool):
  branch v1, b1, b2
b1:
  v2: u64 = const 7
  return v2
b2:
  v3: u64 = const 9
  return v3
}
"#,
    )
    .unwrap();
    wf::module(&module).unwrap();
    let ir = emit_ir_with_facts(&module, false, true).unwrap();
    verify_ir(&ir);

    let start = "call ptr @llvm.invariant.start.p0(";
    let end = "call void @llvm.invariant.end.p0(";
    assert_eq!(ir.matches(start).count(), 2, "{ir}");
    assert_eq!(ir.matches(end).count(), 2, "{ir}");
    for body in ir.split("\n}\n").filter(|body| body.contains(start)) {
        assert_eq!(body.matches(start).count(), 1, "{body}");
        assert_eq!(body.matches("\n  ret ").count(), 1, "{body}");
        let exit = body.rsplit_once("\nexit:\n").unwrap().1;
        assert!(exit.contains(end), "{body}");
    }
}

#[test]
fn scalar_checked_add_keeps_wrapped_results_and_failure_edges() {
    let mut source = String::from("core v1\n");
    for (i, t) in ["u8", "i8"].iter().enumerate() {
        writeln!(source, "sig g{i} = ({t}, {t}) -> ({t})").unwrap();
    }
    source.push_str("sig g2 = (u8, u8) -> (u8, bool)\nsig g3 = (u8, u8) -> (u8, bool)\n");
    for i in 0..8 {
        writeln!(source, "inst n{i} = llvm@1/checked/op{i} [] []").unwrap();
    }
    for (i, op) in ["add", "sub", "mul"].iter().cycle().take(6).enumerate() {
        let t = if i < 3 { "u8" } else { "i8" };
        writeln!(source, "fn f{i} = n{i} g{} {{\nb0(v0: {t}, v1: {t}):\n  v2: {t} = {op} v0, v1\n  return v2\n}}", i / 3).unwrap();
    }
    source.push_str("fn f6 = n6 g2 {\nb0(v0: u8, v1: u8):\n  v2: u8, v3: bool = checked_add v0, v1\n  return v2, v3\n}\n");
    source.push_str("fn f7 = n7 g3 {\nb0(v0: u8, v1: u8):\n  v2: u8, v3: bool = checked_sub v0, v1\n  return v2, v3\n}\n");
    let module = text::parse(&source).unwrap();
    wf::module(&module).unwrap();
    let mut cases = Vec::new();
    for function in 0..8 {
        for a in [0, 1, 127, 128, 255] {
            for b in [0, 1, 127, 128, 255] {
                cases.push((FuncId(function), vec![a, b]));
            }
        }
    }
    // The explicit checked row returns wrapped bits even on overflow. Binary
    // rows instead leave via Overflow before the result can be used.
    agree(&module, &cases);
}

#[test]
fn native_allocator_refusals_are_reported_run_stops() {
    host_memory::check_with_site(
        |module| emit_object(module, Optimization::Speed).unwrap(),
        symbol,
    );
}

include!("../../tests/support/native.rs");
#[test]
fn compare_convert_sample_matches_evaluator() {
    let source = include_str!("../../../hirsel-core-ir/samples/compare-convert.core");
    let module = text::parse(source).unwrap();
    wf::module(&module).unwrap();
    let mut cases = vec![];
    for line in source.lines() {
        let Some(run) = line.trim().strip_prefix("; run: f") else {
            continue;
        };
        let (f, rest) = run.split_once('(').unwrap();
        let (args, _) = rest.split_once(')').unwrap();
        let f = FuncId(f.parse().unwrap());
        let sig = &module.signatures[module.functions[f.index()].signature.index()];
        let args = args
            .split(',')
            .zip(&sig.params)
            .map(|(arg, t)| {
                let Param::Value(t) = t else { unreachable!() };
                eval::scalar(*t, arg.trim())
            })
            .collect();
        cases.push((f, args));
    }
    assert_eq!(cases.len(), 32);
    agree(&module, &cases);
}

#[test]
fn infallible_arithmetic_sample_matches_evaluator() {
    let module = text::parse(include_str!("../../../hirsel-core-ir/samples/arith.core")).unwrap();

    let cases = vec![
        (FuncId(0), vec![3f64.to_bits(), 4f64.to_bits()]),
        (FuncId(0), vec![1e308f64.to_bits(), 1e308f64.to_bits()]),
        (FuncId(2), vec![200]),
        (FuncId(2), vec![1]),
        (FuncId(3), vec![200]),
        (FuncId(3), vec![154]),
        (FuncId(4), vec![155]),
        (FuncId(4), vec![156]),
        (FuncId(5), vec![0]),
        (FuncId(5), vec![1]),
        (FuncId(5), vec![u32::MAX.into()]),
    ];
    agree(&module, &cases);
}

/// A rotate that drops the wrapped bits, rotates right, or reads the count signed or
/// unreduced places Map keys in other buckets without changing any program output.
/// The evaluator's `rotl` is checked against Rust's `rotate_left` in the Cranelift
/// integer differential.
#[test]
fn rotl_matches_evaluator_at_every_width() {
    let mut text = String::from("core v1\n");
    let kinds = ["i8", "u8", "i16", "u16", "i32", "u32", "i64", "u64"];
    for (id, kind) in kinds.iter().enumerate() {
        text += &format!(
            "sig g{id} = ({kind}, {kind}) -> ({kind})\ninst n{id} = test@0/rotl/f{id} [] []\n"
        );
    }
    let mut cases = Vec::new();
    for (id, kind) in kinds.iter().enumerate() {
        text += &format!(
            "fn f{id} = n{id} g{id} {{\nb0(v0: {kind}, v1: {kind}):\n  v2: {kind} = rotl v0, v1\n  return v2\n}}\n"
        );
        let width = 8u64 << (id / 2);
        let mask = u64::MAX >> (64 - width);
        for x in [1 << (width - 1) | 1, 0xa5c3_0f96_5a3c_f069 & mask] {
            for count in [0, 1, width - 1, width, width + 3, mask] {
                cases.push((FuncId(id as u32), vec![x, count]));
            }
        }
    }
    let module = text::parse(&text).unwrap();
    wf::module(&module).unwrap();
    agree(&module, &cases);
}

/// The range check reads the operand untruncated: a strict bound one below the minimum
/// where that is representable, else the minimum itself. A wrong choice rejects
/// -128.5 as an i8 or accepts -2^31 - 1 as an i32 (f64), or misjudges an f32 bound.
#[test]
fn float_to_int_matches_evaluator_at_every_width() {
    let mut text = String::from("core v1\n");
    let kinds = ["i8", "u8", "i16", "u16", "i32", "u32", "i64", "u64"];
    let mut id = 0u32;
    let mut bodies = String::new();
    let mut cases = Vec::new();
    for (k, kind) in kinds.iter().enumerate() {
        let width = 8i32 << (k / 2);
        let signed = k % 2 == 0;
        let upper = (2f64).powi(width - i32::from(signed));
        let lower = if signed { -upper } else { 0.0 };
        for float in ["f32", "f64"] {
            text += &format!(
                "sig g{id} = ({float}) -> ({kind}, u8)\ninst n{id} = test@0/to_int/f{id} [] []\n"
            );
            bodies += &format!(
                "fn f{id} = n{id} g{id} {{\nb0(v0: {float}):\n  v1: {kind}, v2: u8 = float_to_int v0\n  return v1, v2\n}}\n"
            );
            for value in [
                f64::NAN,
                f64::INFINITY,
                f64::NEG_INFINITY,
                -0.9,
                2.9,
                lower,
                lower - 0.5,
                lower - 1.0,
                upper,
                upper - f64::EPSILON * upper,
            ] {
                let bits = if float == "f32" {
                    u64::from((value as f32).to_bits())
                } else {
                    value.to_bits()
                };
                cases.push((FuncId(id), vec![bits]));
            }
            id += 1;
        }
    }
    text += &bodies;
    let module = text::parse(&text).unwrap();
    wf::module(&module).unwrap();
    agree(&module, &cases);
}

#[test]
fn c_results_and_indirect_calls_match_evaluator() {
    let module = text::parse(
        r#"core v1
sig g0 = (u32, u32) -> (u32, u32)
sig g1 = (u32, u32) -> (u32)
sig g2 = (u8, u8, u8) -> (u8, u8, u8)
sig g3 = (f32, f32, f64) -> (f32, f32, f64)
sig g4 = (u64, u64, u64) -> (u64, u64, u64)
inst n0 = tests@0/abi/pair [] []
inst n1 = tests@0/abi/caller [] []
inst n2 = tests@0/abi/small [] []
inst n3 = tests@0/abi/floats [] []
inst n4 = tests@0/abi/large [] []
fn f0 = n0 g0 {
b0(v0: u32, v1: u32):
 return v1, v0
}
fn f1 = n1 g1 {
b0(v0: u32, v1: u32):
 v2: ptr = fnref f0
 v3: u32, v4: u32 = call v2 g0 (v0, v1)
 v5: u32 = wrapping_sub v3, v4
 return v5
}
fn f2 = n2 g2 {
b0(v0: u8, v1: u8, v2: u8):
 return v2, v0, v1
}
fn f3 = n3 g3 {
b0(v0: f32, v1: f32, v2: f64):
 return v1, v0, v2
}
fn f4 = n4 g4 {
b0(v0: u64, v1: u64, v2: u64):
 return v2, v1, v0
}
"#,
    )
    .unwrap();
    wf::module(&module).unwrap();
    agree(
        &module,
        &[
            (FuncId(0), vec![11, 900]),
            (FuncId(1), vec![11, 900]),
            (FuncId(2), vec![1, 2, 255]),
            (
                FuncId(3),
                vec![
                    u64::from(3.5f32.to_bits()),
                    u64::from((-0f32).to_bits()),
                    8.25f64.to_bits(),
                ],
            ),
            (FuncId(4), vec![u64::MAX, 3, 9]),
        ],
    );
}

#[test]
fn failure_edges_precede_later_scalar_and_address_definitions() {
    let module = text::parse(
        r#"core v1
sig g0 = (u64, u64) -> (u64, u64)
inst n0 = tests@0/homes/failing_prefix [] []
fn f0 = n0 g0 {
 slot s0 = u64
 addr a0 = s0 : u64
b0(v0: u64, v1: u64):
 live s0
 v2: u64, v3: bool = checked_add v0, v1
 v4: bool = const false
 v5: bool = eq v3, v4
 v6: u64 = add v0, v1 fail b1
 v7: u64 = const 1
 v8: u64 = wrapping_add v0, v7
 address a0
 store a0, v8
 jump b4()
b1():
 branch v5, b2(), b3()
b2():
 dead s0
 propagate
b3():
 dead s0
 propagate
b4():
 v9: u64 = read a0
 dead s0
 return v8, v9
}
"#,
    )
    .unwrap();
    wf::module(&module).unwrap();
    agree(
        &module,
        &[
            (FuncId(0), vec![0, 0]),
            (FuncId(0), vec![2, 3]),
            (FuncId(0), vec![u64::MAX, 1]),
        ],
    );
}

#[test]
fn local_homes_keep_ssa_addresses_across_blocks_and_entry_back_edges() {
    let module = text::parse(
        r#"core v1
sig g0 = (u64) -> (u64)
sig g1 = (bool, u64) -> (u64)
inst n0 = tests@0/homes/local [] []
inst n1 = tests@0/homes/identity [] []
inst n2 = tests@0/homes/inactive [] []
inst n3 = tests@0/homes/entry_loop [] []
fn f0 = n0 g0 {
 slot s0 = u64
 addr a0 = s0 : u64
b0(v0: u64):
 live s0
 address a0
 store a0, v0
 v1: u64 = read a0
 v2: u64 = call f1 g0 (v1)
 dead s0
 return v2
}
fn f1 = n1 g0 {
b0(v0: u64):
 return v0
}
fn f2 = n2 g1 {
 slot s0 = u64
 addr a0 = s0 : u64
b0(v0: bool, v1: u64):
 live s0
 address a0
 store a0, v1
 branch v0, b1(), b2()
b1():
 jump b2()
b2():
 branch v0, b3(), b4()
b3():
 v2: u64 = read a0
 dead s0
 return v2
b4():
 v3: u64 = const 0
 dead s0
 return v3
}
fn f3 = n3 g0 {
b0(v0: u64):
 v1: u64 = const 0
 v2: bool = ne v0, v1
 branch v2, b1(), b2()
b1():
 v3: u64 = const 1
 v4: u64 = wrapping_sub v0, v3
 jump b0(v4)
b2():
 return v0
}
"#,
    )
    .unwrap();
    wf::module(&module).unwrap();
    let raw = emit_ir(&module).unwrap();
    let local = raw
        .split("define ")
        .find(|f| f.contains(&format!("@{}", symbol(&module, FuncId(0)))))
        .unwrap();
    assert!(
        !local.contains("%a0 = alloca"),
        "local address is an SSA value"
    );
    assert!(
        !local.contains("%v1 = alloca"),
        "local call operand is an SSA value"
    );
    agree(
        &module,
        &[
            (FuncId(0), vec![42]),
            (FuncId(0), vec![u64::MAX]),
            (FuncId(2), vec![0, 42]),
            (FuncId(2), vec![1, 42]),
            (FuncId(3), vec![0]),
            (FuncId(3), vec![4]),
        ],
    );
}

#[test]
fn loop_edges_transfer_parameters_in_parallel() {
    let module = text::parse(
        r#"core v1
sig g0 = (u32) -> (u32, u32)
inst n0 = tests@0/loop/swap [] []
fn f0 = n0 g0 {
 slot s0 = u32
 addr a0 = s0 : u32
b0(v0: u32):
 live s0
 address a0
 store a0, v0
 v1: u32 = const 1
 v2: u32 = const 2
 v3: u32 = const 0
 jump b1(v1, v2)
b1(v4: u32, v5: u32):
 v6: u32 = read a0
 v7: bool = ne v6, v3
 branch v7, b2, b3
b2:
 v8: u32 = wrapping_sub v6, v1
 store a0, v8
 jump b1(v5, v4)
b3:
 dead s0
 return v4, v5
}
"#,
    )
    .unwrap();
    wf::module(&module).unwrap();
    agree(
        &module,
        &[
            (FuncId(0), vec![0]),
            (FuncId(0), vec![1]),
            (FuncId(0), vec![2]),
            (FuncId(0), vec![7]),
        ],
    );
}

#[test]
fn float_intrinsics_preserve_nan_and_signed_zero() {
    let mut source = String::from("core v1\n");
    let mut cases = vec![];
    for kind in ["f32", "f64"] {
        let bits = |n: f64| {
            if kind == "f32" {
                u64::from((n as f32).to_bits())
            } else {
                n.to_bits()
            }
        };
        for op in [
            "min",
            "max",
            "min_number",
            "max_number",
            "sqrt",
            "floor",
            "ceil",
            "trunc",
            "round",
            "abs",
        ] {
            let binary = op.starts_with("min") || op.starts_with("max");
            let i = source.matches("\nfn ").count();
            let result = if binary { 2 } else { 1 };
            writeln!(source,"sig g{i} = ({kind}{}) -> ({kind})\ninst n{i} = tests@0/float/{kind}_{op} [] []\nfn f{i} = n{i} g{i} {{\nb0(v0: {kind}{}):\n v{result}: {kind} = {op} v0{}\n return v{result}\n}}",if binary {format!(", {kind}")} else {String::new()},if binary {format!(", v1: {kind}")} else {String::new()},if binary {", v1"} else {""}).unwrap();
            if binary {
                for (a, b) in [
                    (0.0, -0.0),
                    (-0.0, 0.0),
                    (f64::NAN, 2.5),
                    (2.5, f64::NAN),
                    (f64::NAN, f64::NAN),
                    (-3.0, 2.5),
                ] {
                    cases.push((FuncId(i as u32), vec![bits(a), bits(b)]));
                }
            } else {
                for a in [0.0, -0.0, f64::NAN, f64::INFINITY, -3.5, 2.5] {
                    cases.push((FuncId(i as u32), vec![bits(a)]));
                }
            }
        }
    }
    let module = text::parse(&source).unwrap();
    wf::module(&module).unwrap();
    agree(&module, &cases);
}

#[test]
fn numerical_returns_keep_failure_status_and_canonical_bits() {
    let module = text::parse(include_str!("../../tests/core_runtime/numeric-status.core")).unwrap();
    wf::module(&module).unwrap();
    let mut cases = Vec::new();
    for function in [FuncId(0), FuncId(1)] {
        for index in [0, 7, u64::MAX - 1, u64::MAX] {
            for bits in [
                0,
                (-0.0f64).to_bits(),
                1.0f64.to_bits(),
                f64::INFINITY.to_bits(),
                0x7ff0_0000_0000_0001,
                0xfff8_0000_0000_0042,
            ] {
                cases.push(Case {
                    function,
                    args: vec![index, bits],
                    limit: u64::MAX,
                    injection: None,
                });
            }
        }
    }
    for facts in [false, true] {
        let ir = emit_ir_with_facts(&module, false, facts).unwrap();
        for speed in [false, true] {
            let object = hirsel_llvm_lazy::compile_ir(&ir, speed).unwrap();
            let context = crate::context::Context::new();
            let direct = crate::context::Module::parse(&context, "hirsel-core", &ir).unwrap();
            let machine =
                crate::context::TargetMachine::core(hirsel_core_layout::Target::X86_64Linux, speed)
                    .unwrap();
            direct.set_target(&machine);
            if speed {
                direct.run_core_passes(&machine).unwrap();
            }
            assert_eq!(object, direct.emit_object(&machine).unwrap());
            let results = execute_object(
                &module,
                &cases,
                "",
                "",
                if speed {
                    Optimization::Speed
                } else {
                    Optimization::None
                },
                Some(object),
            );
            assert_eq!(results.len(), cases.len());
            for (case, actual) in cases.iter().zip(results) {
                let expected = eval::run(&module, case.function, &case.args, case.limit);
                assert_eq!(actual.outcome, expected.outcome, "{:?}", case.args);
                assert_eq!(actual.used, expected.used);
            }
        }
    }
}

#[test]
fn numerical_chains_observe_bits_after_memory_copies_and_calls() {
    let mut source = String::from("core v1\n");
    let mut cases = Vec::new();
    for (kind, word, inputs) in [
        (
            "f32",
            "u32",
            vec![
                0,
                0x8000_0000,
                1,
                0x0080_0000,
                0x3f80_0000,
                0xbf80_0000,
                0x7f80_0000,
                0xff80_0000,
                0x7fc0_0001,
                0xffc0_0042,
                0x7f80_0001,
            ],
        ),
        (
            "f64",
            "u64",
            vec![
                0,
                0x8000_0000_0000_0000,
                1,
                0x0010_0000_0000_0000,
                0x3ff0_0000_0000_0000,
                0xbff0_0000_0000_0000,
                0x7ff0_0000_0000_0000,
                0xfff0_0000_0000_0000,
                0x7ff8_0000_0000_0001,
                0xfff8_0000_0000_0042,
                0x7ff0_0000_0000_0001,
            ],
        ),
    ] {
        let identity = source.matches("\nfn ").count();
        writeln!(source, "sig g{identity} = ({kind}) -> ({kind})\ninst n{identity} = tests@0/numeric/{kind}_identity [] []\nfn f{identity} = n{identity} g{identity} {{\nb0(v0: {kind}):\n return v0\n}}").unwrap();
        let bytes = if kind == "f32" { 4 } else { 8 };
        for op in [
            "add",
            "sub",
            "mul",
            "div",
            "min",
            "max",
            "min_number",
            "max_number",
        ] {
            let i = source.matches("\nfn ").count();
            // Memory, byte copies, edges and calls carry floats transparently.
            // Typed float reads followed by reinterpret observe their bits.
            writeln!(source, "sig g{i} = ({kind}, {kind}) -> ({kind}, {word}, {word}, {word}, {word})\ninst n{i} = tests@0/numeric/{kind}_{op} [] []\nfn f{i} = n{i} g{i} {{\n slot s0 = {kind}\n slot s1 = {kind}\n slot s2 = {kind}\n slot s3 = {kind}\n addr a0 = s0 : {kind}\n addr a1 = v9 index v8 : {kind}\n addr a2 = s1 : {kind}\n addr a3 = s2 : {kind}\n addr a4 = s3 : {kind}\nb0(v0: {kind}, v1: {kind}):\n v2: {kind} = {op} v0, v1\n jump b1(v2)\nb1(v3: {kind}):\n v4: {kind} = neg v3\n v5: {kind} = abs v4\n v6: {kind} = sqrt v5\n v7: {kind} = mul_add v6, v6, v3\n live s0\n address a0\n store a0, v7\n live s1\n live s2\n live s3\n address a2\n address a3\n address a4\n store a2, v7\n store a3, v7\n v13: u64 = const {bytes}\n copy a4, a3, v13\n v14: {kind} = read a2\n v15: {word} = reinterpret v14\n v16: {kind} = read a4\n v17: {word} = reinterpret v16\n v8: u64 = const 0\n v9: ptr = pointer a0\n address a1\n v10: {kind} = read a1\n v18: {word} = reinterpret v10\n v11: {word} = reinterpret v7\n v12: {kind} = call f{identity} g{identity} (v7)\n dead s3\n dead s2\n dead s1\n dead s0\n return v12, v18, v11, v15, v17\n}}").unwrap();
            for &a in &inputs {
                for &b in &inputs {
                    cases.push(Case {
                        function: FuncId(i as u32),
                        args: vec![a, b],
                        limit: u64::MAX,
                        injection: None,
                    });
                }
            }
        }
    }
    let module = text::parse(&source).unwrap();
    wf::module(&module).unwrap();
    let expected: Vec<_> = cases
        .iter()
        .map(|c| eval::run(&module, c.function, &c.args, c.limit).outcome)
        .collect();
    for optimization in [Optimization::None, Optimization::Speed] {
        let reports = execute_optimized(&module, &cases, "", "", optimization);
        assert_eq!(reports.len(), expected.len());
        for (got, want) in reports.iter().zip(&expected) {
            assert_eq!(&got.outcome, want);
        }
    }
}

fn sample_cases(module: &Module, source: &str) -> Vec<Case> {
    source
        .lines()
        .filter_map(|line| {
            let line = line.trim().strip_prefix(';')?.trim().strip_prefix("run")?;
            let (header, rest) = line.split_once(':')?;
            let mut injection = None;
            let mut header = header.trim();
            if header.split_whitespace().next() == Some("constant") {
                return None;
            }
            let limit = if let Some(value) = header.strip_prefix("limit ") {
                let (value, suffix) = value.split_once(' ').unwrap_or((value, ""));
                header = suffix.trim();
                if let Some(value) = header.strip_prefix("refuse ") {
                    let later = value.ends_with('+');
                    let value = value.trim_end_matches('+');
                    injection = Some((value.parse().unwrap(), later));
                }
                value.parse().unwrap()
            } else {
                u64::MAX
            };
            let (f, args) = rest.trim().strip_prefix('f')?.split_once('(')?;
            let (args, _) = args.split_once(')')?;
            let function = FuncId(f.parse().unwrap());
            let sig = &module.signatures[module.functions[function.index()].signature.index()];
            let args = args
                .split(',')
                .filter(|s| !s.trim().is_empty())
                .zip(&sig.params)
                .map(|(v, t)| {
                    let Param::Value(t) = t else {
                        panic!("sample destination")
                    };
                    eval::scalar(*t, v.trim())
                })
                .collect();
            Some(Case {
                function,
                args,
                limit,
                injection,
            })
        })
        .collect()
}
#[test]
fn every_whole_v0_sample_matches_the_evaluator() {
    let mut count = 0;
    for source in [
        include_str!("../../../hirsel-core-ir/samples/arith.core"),
        include_str!("../../../hirsel-core-ir/samples/compare-convert.core"),
        include_str!("../../../hirsel-core-ir/samples/control.core"),
        include_str!("../../../hirsel-core-ir/samples/slots.core"),
        include_str!("../../../hirsel-core-ir/samples/calls.core"),
    ] {
        let module = text::parse(source).unwrap();
        wf::module(&module).unwrap();
        let cases = sample_cases(&module, source);
        assert!(!cases.is_empty());
        count += cases.len();
        agree_cases(&module, &cases, false);
    }
    println!("whole-v0 sample executions: {count}");
}

/// Carrier words: only a program build (`emit_program`) gives a place-blind List or
/// String parameter its word twin on cold calls, so these cases run there and must
/// match the evaluator in result and in bytes still admitted, on both status edges.

/// A carrier beside three results with no status, on a cold call: the results return
/// through `sret`, which must return void. A `mut` header therefore stays in memory (an
/// `sret` twin returning words is invalid IR); a view header goes as words, and the word
/// call keeps the `sret`. f0 returns the old length, capacity and first element of the
/// list it is lent, and the `mut` f0 also empties it: [5, 6] has length 2 and capacity
/// 4, so f1 returns 2 + 40 + 500 + 1000 * (length after: 0 lent `mut`, 2 as a view).
fn carrier_sret_case(target: Target) {
    for (lent, emptied, expected) in [("&mut t0", "  store a2, v1\n", 542), ("&view t0", "", 2542)]
    {
        let source = r#"core v1
type t0 = own "List[u64]" { items: seq u64 }
sig g0 = (&mut t0) -> (u64, u64, u64)
sig g1 = () -> (u64)
inst n0 = samples@1/twin/empty [] []
inst n1 = samples@1/twin/entry [] []
fn f0 = n0 g0 {
  addr a0 = v0 : t0
  addr a1 = a0 field 0 byte v1 : ptr
  addr a2 = a0 field 0 byte v2 : u64
  addr a3 = a0 field 0 byte v3 : u64
  addr a4 = v4 byte v1 : u64
b0(v0: ptr):
  v1: u64 = const 0
  v2: u64 = const 8
  v3: u64 = const 16
  address a0
  address a1
  address a2
  address a3
  v4: ptr = read a1
  v5: u64 = read a2
  v6: u64 = read a3
  address a4
  v7: u64 = read a4
  store a2, v1
  return v5, v6, v7
}
fn f1 = n1 g1 {
  slot s0 = t0
  addr a0 = s0 : t0
  addr a1 = s0 field 0 byte v0 : ptr
  addr a2 = s0 field 0 byte v1 : u64
  addr a3 = s0 field 0 byte v2 : u64
  addr a4 = v4 byte v0 : u64
  addr a5 = v4 byte v1 : u64
b0:
  v0: u64 = const 0
  v1: u64 = const 8
  v2: u64 = const 16
  v3: u64 = const 32
  v4: ptr, v5: u64 = alloc for seq u64 v3 align 8 charge v3
  v6: ptr = const 0x0
  v7: bool = ne v4, v6
  branch v7, b1, b2
b1:
  live s0
  address a0
  address a1
  address a2
  address a3
  address a4
  address a5
  v8: u64 = const 5
  v9: u64 = const 6
  store a4, v8
  store a5, v9
  v10: u64 = const 2
  v11: u64 = const 4
  store a1, v4
  store a2, v10
  store a3, v11
  v12: u64, v13: u64, v14: u64 = call f0 g0 (&a0)
  v15: u64 = read a2
  v16: u64 = const 10
  v17: u64 = const 100
  v18: u64 = const 1000
  v19: u64 = mul v13, v16
  v20: u64 = mul v14, v17
  v21: u64 = mul v15, v18
  v22: u64 = add v12, v19
  v23: u64 = add v22, v20
  v24: u64 = add v23, v21
  drop a0 t0
  return v24
b2:
  return v0
}
"#
        .replace("&mut t0", lent)
        .replace("  store a2, v1\n", emptied)
        .replace("call f0 g0 (&a0)", "call f0 g0 (&a0) cold");
        let module = text::parse(&source).unwrap();
        wf::module(&module).unwrap();
        let case = Case {
            function: FuncId(1),
            args: vec![],
            limit: u64::MAX,
            injection: None,
        };
        let oracle = eval::run(&module, case.function, &case.args, case.limit);
        assert_eq!(oracle.outcome, eval::Outcome::Returned(vec![expected]));
        let facts = hirsel_core_ir::roles::Facts::program(&[&module], &[]);
        let ir = carrier_ir(&module, &facts[0], target, false);
        let object = hirsel_llvm_lazy::emit_program_for_target(
            &[hirsel_llvm_lazy::Unit::Text(&ir)],
            &[symbol(&module, case.function)],
            target,
        )
        .unwrap();
        // f0 is infallible, so its body and twin carry the `.infallible` name.
        let twin = format!("{}.infallible.words", symbol(&module, FuncId(0)));
        assert_eq!(
            object.windows(twin.len()).any(|w| w == twin.as_bytes()),
            lent == "&view t0",
            "{lent}: the word path"
        );
        let report = execute_target_object(
            &module,
            std::slice::from_ref(&case),
            "",
            "",
            Optimization::Speed,
            Some(object),
            target,
        )
        .remove(0);
        assert_eq!(report.outcome, oracle.outcome, "{lent}");
        assert_eq!(report.used, oracle.used, "{lent}");
    }
}

/// The release IR of `module` with its program role facts, `relocatable` as a cached
/// library's sites are.
fn carrier_ir(
    module: &Module,
    facts: &hirsel_core_ir::roles::Facts,
    target: Target,
    relocatable: bool,
) -> String {
    emit_ir_target_with_role_facts(
        module,
        target,
        false,
        facts_enabled(),
        SourceEmission {
            batch_steps: true,
            test_hooks: true,
            relocatable_sites: relocatable,
            debug_lines: false,
            fixed_sites: &[],
        },
        Some(facts),
    )
    .unwrap()
}

/// A task cancelled before it runs observes the cancel inside a cold call that took
/// its `mut` header as words, after the callee grew it by 100 pushes. The Cancelled
/// edge out of the twin must store the grown header back: the child's cleanup then
/// frees the live block, not the empty one it lent, so join reports Cancelled (1) and
/// every byte is refunded. The evaluator runs a child at its spawn, before any
/// cancel, so the expected run is derived by hand.

#[test]
fn carrier_twins_match_the_evaluator_in_program_builds() {
    carrier_sret_case(Target::X86_64Linux);
}

#[test]
#[ignore = "explicit AArch64 runtime integration target"]
fn aarch64_carrier_twins_match_the_evaluator_in_program_builds() {
    carrier_sret_case(Target::Aarch64Linux);
}

#[test]
fn every_whole_v1_memory_sample_matches_the_evaluator() {
    let mut count = 0;
    for source in [
        include_str!("../../../hirsel-core-ir/samples/admission.core"),
        include_str!("../../../hirsel-core-ir/samples/cleanup.core"),
        include_str!("../../../hirsel-core-ir/samples/counted.core"),
        include_str!("../../../hirsel-core-ir/samples/failure.core"),
        include_str!("../../../hirsel-core-ir/samples/variants.core"),
        include_str!("../../../hirsel-core-ir/samples/stable-keys.core"),
        include_str!("../../../hirsel-core-ir/samples/kernel.core"),
    ] {
        let module = text::parse(source).unwrap();
        wf::module(&module).unwrap();
        let cases = sample_cases(&module, source);
        assert!(!cases.is_empty());
        count += cases.len();
        agree_cases(&module, &cases, true);
    }
    println!("whole-v1 memory sample executions: {count}");
}

/// Bug: LLVM emits code for a `mark` (FORM v1c), which has no run-time effect, or
/// lets one change what the rest of the body becomes (an empty bound scope it
/// breaks up, a value numbering it shifts).
#[test]
fn marks_emit_nothing() {
    let module = text::parse(include_str!("../../../hirsel-core-ir/samples/kernel.core")).unwrap();
    let stripped = wf::without_marks(Cow::Borrowed(&module)).into_owned();
    assert_ne!(stripped, module, "the sample has marks");
    for opt in [Optimization::None, Optimization::Speed] {
        assert!(
            emitted_ir(&module, opt).unwrap() == emitted_ir(&stripped, opt).unwrap(),
            "a mark changed the IR at {opt:?}"
        );
    }
}

#[test]
fn metered_kernel_marks_and_unmarks_do_not_change_exposure() {
    let source = include_str!("../../../hirsel-core-ir/samples/kernel.core")
        .replace("  call f3 ", "  step\n  call f3 ");
    for marker in ["mark", "unmark"] {
        let source = source.replace("  mark ", &format!("  {marker} "));
        let module = text::parse(&source).unwrap();
        wf::module(&module).unwrap();
        let stripped = wf::without_marks(Cow::Borrowed(&module)).into_owned();
        assert_ne!(stripped, module);
        for opt in [Optimization::None, Optimization::Speed] {
            assert!(
                emitted_ir(&module, opt).unwrap() == emitted_ir(&stripped, opt).unwrap(),
                "a metered {marker} changed the IR at {opt:?}"
            );
        }
    }
}

#[test]
fn every_guarded_admission_refusal_matches_the_evaluator() {
    let admission = include_str!("../../../hirsel-core-ir/samples/admission.core");
    let resource = include_str!("../../tests/core_runtime/injection.core");
    let mut executions = 0;
    for source in [admission, resource] {
        let module = text::parse(source).unwrap();
        wf::module(&module).unwrap();
        let baselines = if source == resource {
            vec![Case {
                function: FuncId(0),
                args: vec![],
                limit: 256,
                injection: None,
            }]
        } else {
            sample_cases(&module, source)
                .into_iter()
                .filter(|c| c.function.0 <= 1)
                .collect()
        };
        let mut cases = Vec::new();
        for case in baselines {
            let expected = eval::run(&module, case.function, &case.args, case.limit);
            assert!(expected.admissions > 0);
            for nth in 1..=expected.admissions {
                for later in [false, true] {
                    cases.push(Case {
                        function: case.function,
                        args: case.args.clone(),
                        limit: case.limit,
                        injection: Some((nth, later)),
                    });
                }
            }
        }
        executions += cases.len();
        agree_cases(&module, &cases, true);
    }
    println!("guarded injected executions: {executions}");
}

#[test]
fn every_compact_sequence_sample_matches_the_evaluator() {
    let source = include_str!("../../../hirsel-core-ir/samples/sequences.core");
    let module = text::parse(source).unwrap();
    wf::module(&module).unwrap();
    let cases = sample_cases(&module, source);
    assert_eq!(cases.len(), 11);
    agree_cases(&module, &cases, true);
}

#[test]
fn owning_callable_and_failure_message_transfer_match_the_evaluator() {
    let source = include_str!("../../tests/core_runtime/callable.core");
    let module = text::parse(source).unwrap();
    wf::module(&module).unwrap();
    let mut cases = vec![
        Case {
            function: FuncId(0),
            args: vec![7, 0],
            limit: 256,
            injection: None,
        },
        Case {
            function: FuncId(0),
            args: vec![7, 1],
            limit: 256,
            injection: None,
        },
        Case {
            function: FuncId(2),
            args: vec![],
            limit: 256,
            injection: None,
        },
    ];
    for later in [false, true] {
        cases.push(Case {
            function: FuncId(0),
            args: vec![7, 1],
            limit: 256,
            injection: Some((1, later)),
        });
    }
    let got = execute(&module, &cases);
    for (case, got) in cases.iter().zip(got) {
        let expected = eval::run_injected(
            &module,
            case.function,
            &case.args,
            case.limit,
            case.injection,
        );
        assert_eq!(got.outcome, expected.outcome);
        assert_eq!(got.used, expected.used);
        assert_eq!(got.admissions, expected.admissions);
        assert_eq!(
            got.messages,
            if case.function.0 == 2 {
                vec!["686921".to_string()]
            } else {
                vec![]
            }
        );
    }
    let source = source.replace("  raise fail a0", "  drop a0 t1\n  return");
    let module = text::parse(&source).unwrap();
    agree_cases(
        &module,
        &[Case {
            function: FuncId(2),
            args: vec![],
            limit: 256,
            injection: None,
        }],
        true,
    );
}

#[test]
fn counted_sequences_destroy_deep_chains_with_constant_stack() {
    let source = include_str!("../../tests/core_runtime/counted_sequence.core");
    let module = text::parse(source).unwrap();
    wf::module(&module).unwrap();
    let cases = [0, 32, 50_000].map(|n| Case {
        function: FuncId(0),
        args: vec![n],
        limit: u64::MAX,
        injection: None,
    });
    let got = execute(&module, &cases);
    for (case, got) in cases.iter().zip(got) {
        let n = case.args[0];
        assert_eq!(got.outcome, eval::Outcome::Returned(vec![n]));
        assert_eq!(got.used, 0);
        assert_eq!(got.admissions, 1 + 2 * n as usize);
        if n <= 32 {
            let expected = eval::run(&module, case.function, &case.args, case.limit);
            assert_eq!(got.outcome, expected.outcome);
            assert_eq!(got.used, expected.used);
        }
    }
}

/// LANG-19: ordinary release of a 10^6-deep `Box` chain, of one whose rows
/// carry a finalizer, of a closure whose environment captures a closure 10^6
/// deep, of a node whose List holds the next node 10^6 deep, and of a resource
/// whose field owns the next resource 10^6 deep, completes under the default
/// stack, and every block is refunded (hand-derived: n + 1 boxes, n
/// environments, n buffers, n resources and their n boxes).
#[test]
fn box_chain_releases_with_constant_stack() {
    for (source, boxes, links) in [
        (
            include_str!("../../../hirsel-core-ir/samples/box-chain.core"),
            1,
            1,
        ),
        (
            include_str!("../../../hirsel-core-ir/samples/closure-chain.core"),
            0,
            1,
        ),
        (
            include_str!("../../../hirsel-core-ir/samples/finalizer-chain.core"),
            1,
            1,
        ),
        (
            include_str!("../../../hirsel-core-ir/samples/seq-chain.core"),
            0,
            1,
        ),
        (
            include_str!("../../../hirsel-core-ir/samples/seq-box-chain.core"),
            0,
            2,
        ),
        (
            include_str!("../../../hirsel-core-ir/samples/resource-chain.core"),
            0,
            2,
        ),
    ] {
        let module = text::parse(source).unwrap();
        wf::module(&module).unwrap();
        let cases = [0, 32, 1_000_000].map(|n| Case {
            function: FuncId(0),
            args: vec![n],
            limit: u64::MAX,
            injection: None,
        });
        for (case, got) in cases.iter().zip(execute(&module, &cases)) {
            let n = case.args[0];
            assert_eq!(got.outcome, eval::Outcome::Returned(vec![n]));
            assert_eq!(got.used, 0);
            assert_eq!(got.admissions, boxes + links * n as usize);
        }
    }
}

/// LANG-19 through receivers: a value whose depth runs through channel
/// queues releases under the default stack. One chain of 10^6 receiver hops,
/// and a capacity-2 queue holding two chains of 5 * 10^5 hops each, cleaned
/// last published first (hand-derived: m (n + 1) + 1 admissions, Toks traced
/// m..1, nothing used). The small rows and every admission refusal agree
/// with the evaluator.
#[test]
fn receiver_chain_releases_with_constant_stack() {
    let source = include_str!("../../../hirsel-core-ir/samples/receiver-chain.core");
    let module = text::parse(source).unwrap();
    wf::module(&module).unwrap();
    let mut cases = Vec::new();
    for case in sample_cases(&module, source) {
        let baseline = eval::run(&module, case.function, &case.args, case.limit);
        for nth in 1..=baseline.admissions {
            for later in [false, true] {
                cases.push(Case {
                    function: case.function,
                    args: case.args.clone(),
                    limit: case.limit,
                    injection: Some((nth, later)),
                });
            }
        }
        cases.push(case);
    }
    agree_cases(&module, &cases, true);
    let cases = [(1_000_000u64, 1u64), (500_000, 2)].map(|(n, m)| Case {
        function: FuncId(0),
        args: vec![n, m],
        limit: u64::MAX,
        injection: None,
    });
    for (case, got) in cases.iter().zip(execute(&module, &cases)) {
        let (n, m) = (case.args[0], case.args[1]);
        assert_eq!(got.outcome, eval::Outcome::Returned(vec![m]));
        assert_eq!(got.used, 0);
        assert_eq!(got.trace, (1..=m).rev().collect::<Vec<_>>());
        assert_eq!(got.admissions, (m * (n + 1) + 1) as usize);
    }
}

/// A suspended fixed block is found again only through its record: an array
/// element reuses one record, and two cases of one enum suspend at the same
/// cursor with different offsets.
#[test]
fn counted_frames_recover_the_block_from_the_record() {
    let source = include_str!("../../../hirsel-core-ir/samples/counted-frames.core");
    let module = text::parse(source).unwrap();
    wf::module(&module).unwrap();
    let cases = [(1, 0), (2, 0), (2, 1)].map(|(f, k)| Case {
        function: FuncId(f),
        args: vec![k],
        limit: u64::MAX,
        injection: None,
    });
    for (case, got) in cases.iter().zip(execute(&module, &cases)) {
        assert_eq!(got.outcome, eval::Outcome::Returned(case.args.clone()));
        assert_eq!(got.used, 0);
    }
}

#[test]
fn text_fixed_span_and_pointer_views_use_packed_unaligned_bytes() {
    let source = include_str!("../../tests/core_runtime/text_span.core");
    let module = text::parse(source).unwrap();
    wf::module(&module).unwrap();
    let expected = eval::run(&module, FuncId(0), &[], 256);
    assert_eq!(
        expected.outcome,
        eval::Outcome::Returned(vec![3, 2, 22, 2_189_672])
    );
    agree_cases(
        &module,
        &[Case {
            function: FuncId(0),
            args: vec![],
            limit: 256,
            injection: None,
        }],
        true,
    );
}

#[test]
fn counted_strings_release_their_dynamic_byte_buffer() {
    let source = include_str!("../../tests/core_runtime/callable.core")
        .replace("  addr a6 = v4 byte v1 : u8", "  addr a6 = v4 byte v1 : u8\n  addr a7 = v10 byte v2 : t1")
        .replace("  raise fail a0", "  v9: u64 = const 32\n  v10: ptr, v11: u64 = alloc for rc t1 v9 align 8 charge v9\n  address a7\n  v12: u64 = const 24\n  copy a7, a0, v12\n  release v10 rc t1\n  return");
    let module = text::parse(&source).unwrap();
    wf::module(&module).unwrap();
    agree_cases(
        &module,
        &[Case {
            function: FuncId(2),
            args: vec![],
            limit: 256,
            injection: None,
        }],
        true,
    );
}

#[test]
fn address_shapes_do_not_depend_on_block_emission_order() {
    let source = r#"core v1
type t0 = own "Record" { value: u64 }
sig g0 = (u64) -> (u64)
inst n0 = llvm@1/addresses/order [] []
fn f0 = n0 g0 {
  slot s0 = t0
  addr a0 = s0 : t0
  addr a1 = a0 field 0 : u64
b0(v0: u64):
  live s0
  jump b2
b1:
  address a1
  store a1, v0
  v1: u64 = read a1
  dead s0
  return v1
b2:
  address a0
  jump b1
}
"#;
    let module = text::parse(source).unwrap();
    wf::module(&module).unwrap();
    agree(&module, &[(FuncId(0), vec![123])]);
}

#[test]
fn aggregate_destination_precedes_parameters_and_returns_failure_status() {
    let source = r#"core v1
type t0 = own "Pair" { left: u64, right: u64 }
sig g0 = (u64) -> (u64, u64)
sig g1 = (u64, u64) -> () dest t0
inst n0 = llvm@1/destination/run [] []
inst n1 = llvm@1/destination/write [] []
fn f0 = n0 g0 {
  slot s0 = t0
  addr a0 = s0 : t0
  addr a1 = s0 field 0 : u64
  addr a2 = s0 field 1 : u64
b0(v0: u64):
  live s0
  address a0
  address a1
  address a2
  v1: u64 = const 9
  call f1 g1 (&a0, v0, v1)
  v2: u64 = read a1
  v3: u64 = read a2
  dead s0
  return v2, v3
}
fn f1 = n1 g1 {
  addr a0 = v0 : t0
  addr a1 = a0 field 0 : u64
  addr a2 = a0 field 1 : u64
b0(v0: ptr, v1: u64, v2: u64):
  v3: u64 = const 0
  v4: bool = eq v1, v3
  branch v4, b1, b2
b1:
  raise unwrap
b2:
  address a0
  address a1
  address a2
  store a1, v1
  store a2, v2
  return
}
"#;
    let module = text::parse(source).unwrap();
    wf::module(&module).unwrap();
    agree(&module, &[(FuncId(0), vec![123]), (FuncId(0), vec![0])]);
}

#[test]
fn terminal_results_forward_without_abandoning_cleanup_or_local_arguments() {
    let module = text::parse(include_str!("../../tests/core_runtime/forward.core")).unwrap();
    wf::module(&module).unwrap();
    let recipes = hirsel_core_layout::compute(&module.types).unwrap();
    let failures = failure::Failures::new(&module);
    let ending = |f: usize| {
        let function = &module.functions[f];
        let body = function.body.as_ref().unwrap();
        forward::Forwarding::new(body).ending(
            &module,
            &recipes,
            &module.signatures[function.signature.index()],
            &failures.constants[f],
            0,
        )
    };
    assert!(ending(1).unwrap().frame_safe);
    assert!(!ending(3).unwrap().frame_safe);
    let ir = emit_ir(&module).unwrap();
    // Pair has two leaves, so its destination crosses the call as scalars: the
    // terminal call's results are this function's results, with no local copy.
    assert!(ir.contains(&format!(
        "tail call {{ i64, i64, i8 }} @{}(ptr %context,",
        symbol(&module, FuncId(2))
    )));
    assert!(!ir.contains(&format!(
        "tail call void @{}.infallible",
        symbol(&module, FuncId(4))
    )));
    agree(&module, &[(FuncId(0), vec![123]), (FuncId(0), vec![0])]);

    // FAIL-8/OWN-12: a pending result stays local until its caller's
    // finalizers have succeeded. Even an infallible drop ends forwarding.
    let cleanup = include_str!("../../tests/core_runtime/forward.core")
        .replace("  copy a1, a0, v2", "  drop a0 t0\n  copy a1, a0, v2");
    let cleanup = text::parse(&cleanup).unwrap();
    let body = cleanup.functions[1].body.as_ref().unwrap();
    assert!(
        forward::Forwarding::new(body)
            .ending(
                &cleanup,
                &recipes,
                &cleanup.signatures[1],
                &failures.constants[1],
                0
            )
            .is_none()
    );
    let cleanup =
        text::parse(include_str!("../../../hirsel-core-ir/samples/failure.core")).unwrap();
    let recipes = hirsel_core_layout::compute(&cleanup.types).unwrap();
    let failures = failure::Failures::new(&cleanup);
    let body = cleanup.functions[1].body.as_ref().unwrap();
    assert!(
        forward::Forwarding::new(body)
            .ending(
                &cleanup,
                &recipes,
                &cleanup.signatures[0],
                &failures.constants[1],
                1
            )
            .is_none()
    );
}

#[test]
fn infallible_direct_calls_keep_the_external_and_indirect_failure_abi() {
    let module = text::parse(include_str!("../../tests/core_runtime/infallible.core")).unwrap();
    wf::module(&module).unwrap();
    assert_eq!(
        failure::Failures::new(&module).may_fail,
        [false, false, true, true, true]
    );
    let ir = emit_ir(&module).unwrap();
    assert!(ir.contains(&format!(
        "call i64 @{}.infallible",
        symbol(&module, FuncId(1))
    )));
    let mut unknown = module.clone();
    unknown.functions[1].body = None;
    assert!(failure::Failures::new(&unknown).may_fail.iter().all(|f| *f));
    let mut cases = vec![];
    for f in [0, 1, 4] {
        for n in [0, 5, 100] {
            cases.push((FuncId(f), vec![n]));
        }
    }
    for f in [2, 3] {
        for n in [0, 1, 63, 64, u64::MAX] {
            cases.push((FuncId(f), vec![7, n]));
        }
    }
    agree(&module, &cases);
}

#[test]
fn maps_clean_live_entries_in_order_and_continue_after_both_failures() {
    let module = text::parse(include_str!("../../tests/core_runtime/map.core")).unwrap();
    wf::module(&module).unwrap();
    let cases = [
        (FuncId(2), vec![0]),
        (FuncId(2), vec![1]),
        (FuncId(3), vec![]),
        (FuncId(4), vec![]),
    ]
    .map(|(function, args)| Case {
        function,
        args,
        limit: 1024,
        injection: None,
    });
    let reports = execute(&module, &cases);
    for (i, got) in reports.iter().enumerate() {
        assert_eq!(
            got.outcome,
            if i == 1 {
                eval::Outcome::Failed(vec!["Bounds".into(), "Unwrap".into()])
            } else {
                eval::Outcome::Returned(vec![])
            }
        );
        assert_eq!(got.used, if i == 1 { 64 } else { 0 });
        assert_eq!(
            got.trace,
            match i {
                0 => vec![11, 12, 30, 33],
                1 => vec![11, 12, 31, 32],
                _ => vec![],
            }
        );
        assert_eq!(
            got.admissions,
            match i {
                0 | 1 => 5,
                2 => 1,
                _ => 0,
            }
        );
    }
}

#[test]
fn counted_maps_skip_removed_entries_and_destroy_deep_chains_with_constant_stack() {
    let module = text::parse(include_str!("../../tests/core_runtime/counted_map.core")).unwrap();
    wf::module(&module).unwrap();
    let cases = [0, 32, 50_000].map(|n| Case {
        function: FuncId(0),
        args: vec![n],
        limit: u64::MAX,
        injection: None,
    });
    for (case, got) in cases.iter().zip(execute(&module, &cases)) {
        assert_eq!(got.outcome, eval::Outcome::Returned(case.args.clone()));
        assert_eq!(got.used, 0);
        assert_eq!(got.admissions, 1 + 2 * case.args[0] as usize);
    }
}

#[path = "fast_tests.rs"]
mod fast_tests;

#[test]
fn static_capacity_zero_carriers_drop_and_grow_through_runtime_cleanup() {
    let source = include_str!("../../../hirsel-core-ir/samples/strings.core");
    let module = text::parse(source).unwrap();
    wf::module(&module).unwrap();
    agree_cases(&module, &sample_cases(&module, source), true);
    let module = text::parse(include_str!(
        "../../../hirsel-cranelift/tests/support/static-carriers.core"
    ))
    .unwrap();
    wf::module(&module).unwrap();
    let cases: Vec<_> = (0..4)
        .flat_map(|f| {
            [0, 5].map(move |n| Case {
                function: FuncId(f),
                args: vec![n],
                limit: 1000,
                injection: None,
            })
        })
        .collect();
    // Map cleanup is not implemented by the evaluator. These expected balances
    // follow the literal and two declared container sizes directly from FORM #48.
    let actual = execute(&module, &cases);
    for (case, got) in cases.iter().zip(actual) {
        assert_eq!(got.outcome, eval::Outcome::Returned(vec![]));
        assert_eq!(got.used, 0);
        assert!(got.trace.is_empty());
        assert_eq!(got.admissions, [0, 1, 1, 2][case.function.index()]);
    }
}

#[test]
fn hash_read_facts_observe_intervening_writes() {
    let module = text::parse(include_str!("../../tests/core_runtime/hash-writes.core")).unwrap();
    wf::module(&module).unwrap();
    let cases = [(0, 1), (1, 0), (u64::MAX, 0), (1 << 63, u64::MAX)].map(|(a, b)| Case {
        function: FuncId(0),
        args: vec![a, b],
        limit: 0,
        injection: None,
    });
    for facts in [false, true] {
        let ir = emit_ir_with_facts(&module, false, facts).unwrap();
        for speed in [false, true] {
            let object = hirsel_llvm_lazy::compile_ir(&ir, speed).unwrap();
            for actual in execute_object(&module, &cases, "", "", Optimization::None, Some(object))
            {
                assert_eq!(actual.outcome, eval::Outcome::Returned(vec![1, 1]));
            }
        }
    }
}

#[test]
fn map_placement_operations_match_fixed_key_vectors() {
    mod vectors {
        include!("../../../hirsel-cranelift/tests/support/map-placement-vectors.rs");
    }
    let module = text::parse(include_str!(
        "../../../hirsel-cranelift/tests/support/map-placement.core"
    ))
    .unwrap();
    wf::module(&module).unwrap();
    let cases: Vec<_> = vectors::CASES
        .iter()
        .map(|&(f, arg, _)| Case {
            function: FuncId(f),
            args: vec![arg],
            limit: 0,
            injection: None,
        })
        .collect();
    let key = std::array::from_fn(|i| i as u8);
    let reports = execute_object(
        &module,
        &cases,
        "extern void hirsel_core_test_set_map_vectors_key(HirselCoreContext *);\n\
         #define SETUP_CASE(ctx) hirsel_core_test_set_map_vectors_key(ctx)\n",
        "",
        Optimization::Speed,
        None,
    );
    for ((&(_, _, expected), case), native) in vectors::CASES.iter().zip(&cases).zip(reports) {
        let (eval, _) = eval::run_map_hash(&module, case.function, &case.args, 0, key);
        assert_eq!(eval.outcome, eval::Outcome::Returned(expected.to_vec()));
        assert_eq!(native.outcome, eval.outcome);
    }
}

#[test]
fn string_literals_growth_and_byte_order_match_the_evaluator() {
    let source = include_str!("../../../hirsel-core-ir/samples/strings.core");
    let module = text::parse(source).unwrap();
    wf::module(&module).unwrap();
    let cases = sample_cases(&module, source);
    assert_eq!(cases.len(), 10);
    // Refuse each growth/probe admission without ever freeing the static carrier.
    let mut refusals = Vec::new();
    for case in &cases {
        let baseline = eval::run(&module, case.function, &case.args, case.limit);
        for nth in 1..=baseline.admissions {
            for later in [false, true] {
                refusals.push(Case {
                    function: case.function,
                    args: case.args.clone(),
                    limit: case.limit,
                    injection: Some((nth, later)),
                });
            }
        }
    }
    agree_cases(&module, &refusals, true);
    println!(
        "string sample runs: {}; refusal runs: {}",
        cases.len(),
        refusals.len()
    );
}

#[test]
fn task_custody_samples_and_admission_refusals_match_the_evaluator() {
    let source = include_str!("../../../hirsel-core-ir/samples/custody.core");
    let module = text::parse(source).unwrap();
    wf::module(&module).unwrap();
    let mut cases = Vec::new();
    for case in sample_cases(&module, source) {
        let baseline = eval::run(&module, case.function, &case.args, case.limit);
        for nth in 1..=baseline.admissions {
            for later in [false, true] {
                cases.push(Case {
                    function: case.function,
                    args: case.args.clone(),
                    limit: case.limit,
                    injection: Some((nth, later)),
                });
            }
        }
        cases.push(case);
    }
    println!("task custody executions: {}", cases.len());
    agree_cases(&module, &cases, true);
}

#[test]
fn isolate_sample_and_admission_refusals_match_the_evaluator() {
    let source = include_str!("../../../hirsel-core-ir/samples/isolate.core");
    let module = text::parse(source).unwrap();
    wf::module(&module).unwrap();
    let mut cases = Vec::new();
    for case in sample_cases(&module, source) {
        let limit = if case.limit == u64::MAX {
            1_000_000
        } else {
            case.limit
        };
        let baseline = eval::run(&module, case.function, &case.args, limit);
        cases.push(Case {
            function: case.function,
            args: case.args.clone(),
            limit,
            injection: case.injection,
        });
        for nth in 1..=baseline.admissions {
            for later in [false, true] {
                cases.push(Case {
                    function: case.function,
                    args: case.args.clone(),
                    limit,
                    injection: Some((nth, later)),
                });
            }
        }
    }
    println!("isolate executions: {}", cases.len());
    agree_cases(&module, &cases, true);
}

#[test]
fn cancelled_child_inside_isolate_passes_through_and_refunds_cell() {
    let target = if std::env::consts::OS == "macos" {
        Target::Aarch64Darwin
    } else {
        Target::X86_64Linux
    };
    cancel_fixture::cancelled_child_inside_isolate_fixture(target);
}

#[test]
#[ignore = "explicit AArch64 runtime integration target"]
fn aarch64_cancelled_child_inside_isolate_passes_through_and_refunds_cell() {
    cancel_fixture::cancelled_child_inside_isolate_fixture(Target::Aarch64Linux);
}

#[test]
fn channel_fifo_clone_full_and_closed_match_the_evaluator() {
    let module = text::parse(include_str!("channel_tests.core")).unwrap();
    wf::module(&module).unwrap();
    let baseline = eval::run(&module, FuncId(0), &[], u64::MAX);
    assert_eq!(baseline.outcome, eval::Outcome::Returned(vec![7, 8, 0, 1]));
    let mut cases = vec![Case {
        function: FuncId(0),
        args: vec![],
        limit: u64::MAX,
        injection: None,
    }];
    for nth in 1..=baseline.admissions {
        for later in [false, true] {
            cases.push(Case {
                function: FuncId(0),
                args: vec![],
                limit: u64::MAX,
                injection: Some((nth, later)),
            });
        }
    }
    println!("channel executions: {}", cases.len());
    agree_cases(&module, &cases, true);
}

#[test]
fn provider_extern_uses_form_results_and_failure_status() {
    let module = text::parse(
        r#"
core v1
sig g0 = (u64) -> (u64)
inst n0 = tests@0/provider/call [] []
inst n1 = tests@0/provider/clock [] []
inst n2 = tests@0/provider/catch [] []
fn f0 = n0 g0 {
b0(v0: u64):
 v1: u64 = call f1 g0 (v0)
 return v1
}
fn f1 = n1 g0 extern
fn f2 = n2 g0 {
b0(v0: u64):
 v1: u64 = call f1 g0 (v0) fail b1
 return v1
b1:
 v2: u64 = const 0
 v3: u64 = call f1 g0 (v2)
 propagate
}
"#,
    )
    .unwrap();
    wf::module(&module).unwrap();
    let bindings = r#"
static HirselCoreHostOutcome clock_legacy_issue(void *state, HirselCoreHostRequest *request) {
    (void)state; assert(request->operation==10); hirsel_core_test_drop_trace(7+request->handle); HirselCoreHostOutcome out={0};
    if(request->handle) out.status=1; else out.value=1234567;
    return out;
}
static HirselCoreStep clock_issue(void *state, HirselCoreHostRequest *request, uint64_t token) { (void)token; HirselCoreStep s={0}; s.outcome=clock_legacy_issue(state,request); return s; }
static HirselCoreHostOutcome clock_deliver(void *state, HirselCoreHostRequest *request, HirselCoreHostOutcome out) {
    (void)state; assert(request->operation==10); return out;
}
static HirselCoreWait clock_wait(void *s,HirselCoreHostRequest *r,uint64_t t) { (void)s;(void)r;(void)t; return (HirselCoreWait){0,0}; }
static HirselCoreStep clock_resume(void *s,HirselCoreHostRequest *r,uint64_t t) { (void)s;(void)r;(void)t; __builtin_abort(); }
static HirselCoreHostOutcome clock_abort(void *s,HirselCoreHostRequest *r,uint64_t t,uint8_t why) { (void)s;(void)r;(void)t;(void)why; __builtin_abort(); }
R1 EXTERNAL(void *context, uint64_t mode, uint64_t caller_site) {
    (void)caller_site;
    HirselCoreProvider provider={.wait=clock_wait,.issue=clock_issue,.resume=clock_resume,.abort=clock_abort,.deliver=clock_deliver};
    HirselCoreHostRequest request={.operation=10,.handle=mode};
    HirselCoreHostOutcome out=hirsel_core_host(context,&provider,&request);
    if(out.status) {
        HirselCoreCause cause=hirsel_core_cause_new(5,0); hirsel_core_raise(context,cause);
        return (R1){.r1=1};
    }
    return (R1){.r0=out.value};
}
"#
    .replace("EXTERNAL", &symbol(&module, FuncId(1)));
    let cases = [(0, 0), (0, 1), (2, 1)].map(|(f, a)| Case {
        function: FuncId(f),
        args: vec![a],
        limit: u64::MAX,
        injection: None,
    });
    let reports = execute_with_bindings(&module, &cases, &bindings);
    assert_eq!(reports[0].outcome, eval::Outcome::Returned(vec![1234567]));
    assert_eq!(
        reports[1].outcome,
        eval::Outcome::Failed(vec!["Cancelled".into()])
    );
    assert_eq!(reports[2].trace, vec![8, 7]);
    assert_eq!(
        reports[2].outcome,
        eval::Outcome::Failed(vec!["Cancelled".into()])
    );
}

struct ProviderCase {
    call: Case,
    argv: Vec<&'static str>,
    outcome: eval::Outcome,
    stdout: Vec<u8>,
}
fn provider_cases() -> Vec<ProviderCase> {
    use eval::Outcome::{Failed, Returned};
    // The eight hand-derived runs in providers.core, in their documented order.
    [
        (
            0,
            vec![0, 0, 1],
            u64::MAX,
            vec!["prog", "hello"],
            Returned(vec![2, 5]),
            b"hello".to_vec(),
        ),
        (
            0,
            vec![0, 0, 2],
            u64::MAX,
            vec!["prog", "hello"],
            Returned(vec![2, 0]),
            vec![],
        ),
        (
            0,
            vec![0, 0, 0],
            u64::MAX,
            vec![],
            Returned(vec![0, 0]),
            vec![],
        ),
        (
            1,
            vec![7],
            1000,
            vec![],
            Returned(vec![3, 2, 105, 2]),
            vec![],
        ),
        (1, vec![7], 4, vec![], Failed(vec!["Unwrap".into()]), vec![]),
        (2, vec![7, 40], 1000, vec![], Returned(vec![2, 3]), vec![]),
        (2, vec![7, 32], 1000, vec![], Returned(vec![2, 3]), vec![]),
        (
            2,
            vec![7, 31],
            1000,
            vec![],
            Failed(vec!["Bounds".into()]),
            vec![],
        ),
    ]
    .into_iter()
    .map(|(f, args, limit, argv, outcome, stdout)| ProviderCase {
        call: Case {
            function: FuncId(f),
            args,
            limit,
            injection: None,
        },
        argv,
        outcome,
        stdout,
    })
    .collect()
}

#[test]
fn provider_samples_match_the_hand_oracle_and_every_closed_row_emits() {
    let module = text::parse(include_str!(
        "../../../hirsel-core-ir/samples/providers.core"
    ))
    .unwrap();
    wf::module(&module).unwrap();
    let context = Context::new();
    LlvmModule::parse(&context, "providers", &emit_ir(&module).unwrap())
        .unwrap()
        .verify()
        .unwrap();
    for case in provider_cases() {
        let expected = eval::run_args(
            &module,
            case.call.function,
            &case.call.args,
            case.call.limit,
            &case.argv,
        );
        assert_eq!(expected.outcome, case.outcome);
        assert_eq!(expected.used, 0);
        assert_eq!(expected.stdout, case.stdout);
        assert!(expected.stderr.is_empty());
    }
    for &op in HostOp::ALL {
        let source = host_abi::provider_source(op);
        let module = text::parse(&source).unwrap();
        wf::module(&module).unwrap();
        let ir = emit_ir(&module).unwrap();
        LlvmModule::parse(&context, op.name(), &ir)
            .unwrap()
            .verify()
            .unwrap();
        // HirselCoreSpawnOutcome is a 32-byte HirselCoreHostOutcome and three u64 pipes.
        let storage = if op.address_outcome() || op == HostOp::ExecSpawn {
            "sret([56 x i8])"
        } else if op == HostOp::TlsConnect {
            "sret([64 x i8])"
        } else {
            "sret([32 x i8])"
        };
        assert!(ir.contains(storage), "{}: {storage}", op.name());
        // Every provider row branches on its failure status.
        assert!(ir.contains("icmp ne i8"), "{} status", op.name());
    }
}

#[test]
fn log_append_facts_on_and_off_never_store_through_the_handle() {
    let module = text::parse(include_str!("../../../hirsel-core-ir/samples/log.core")).unwrap();
    wf::module(&module).unwrap();
    for facts in [false, true] {
        let ir = emit_ir_with_facts(&module, false, facts).unwrap();
        let body = ir
            .split_once("define")
            .and_then(|(_, rest)| rest.split_once("define").map(|(first, _)| first))
            .unwrap_or(&ir);
        // The one-leaf handle crosses the call by value and is spilled once to
        // its home; after that, nothing may write the home.
        let spill = body
            .lines()
            .find(|line| line.trim_start().starts_with("store ptr %p0_0, ptr "))
            .unwrap_or_else(|| panic!("handle passed by value (facts={facts}): {ir}"));
        fn destination(line: &str) -> Option<&str> {
            line.trim_start()
                .starts_with("store ")
                .then(|| line.split(", ptr ").nth(1)?.split(',').next())
                .flatten()
        }
        let home = destination(spill).unwrap();
        assert!(
            !body
                .lines()
                .any(|line| line != spill && destination(line) == Some(home)),
            "log append stored through its handle (facts={facts}): {ir}"
        );
    }
}

#[test]
fn native_provider_samples_and_refusals_match_the_hand_oracle() {
    let module = text::parse(include_str!(
        "../../../hirsel-core-ir/samples/providers.core"
    ))
    .unwrap();
    wf::module(&module).unwrap();
    let mut executions = 0;
    for case in provider_cases() {
        let baseline = eval::run_args(
            &module,
            case.call.function,
            &case.call.args,
            case.call.limit,
            &case.argv,
        );
        let mut injections = vec![None];
        for nth in 1..=baseline.admissions {
            injections.extend([Some((nth, false)), Some((nth, true))]);
        }
        for injection in injections {
            let expected = if injection.is_none() {
                eval::run_args(
                    &module,
                    case.call.function,
                    &case.call.args,
                    case.call.limit,
                    &case.argv,
                )
            } else {
                assert!(case.argv.is_empty());
                eval::run_injected(
                    &module,
                    case.call.function,
                    &case.call.args,
                    case.call.limit,
                    injection,
                )
            };
            let mut call = Case {
                function: case.call.function,
                args: case.call.args.clone(),
                limit: case.call.limit,
                injection,
            };
            // World's mock handles are zero; real capabilities are configured indices.
            if call.function == FuncId(0) {
                call.args[0] = 2;
                call.args[1] = 1;
            }
            let configuration = format!(
                ",.args=scripted_args,.args_len={},.capabilities=scripted_grants,.capabilities_len=7",
                case.argv.len()
            );
            let reports = execute_with_configuration(
                &module,
                &[call],
                include_str!("provider_tests.c"),
                &configuration,
            );
            assert_eq!(reports.len(), 1);
            assert_eq!(reports[0].outcome, expected.outcome);
            assert_eq!(reports[0].used, expected.used);
            assert_eq!(reports[0].admissions, expected.admissions);
            executions += 1;
        }
    }
    println!("native provider sample/refusal executions: {executions}");
}

#[test]
fn provider_error_payload_and_nonwaiting_clocks_use_the_c_abi() {
    let module = text::parse(include_str!("provider_abi_tests.core")).unwrap();
    wf::module(&module).unwrap();
    let cases = [
        Case {
            function: FuncId(0),
            args: vec![7],
            limit: 1000,
            injection: None,
        },
        Case {
            function: FuncId(1),
            args: vec![3],
            limit: 1000,
            injection: None,
        },
    ];
    let reports = execute_with_configuration(
        &module,
        &cases,
        include_str!("provider_tests.c"),
        ",.capabilities=scripted_grants,.capabilities_len=7",
    );
    assert_eq!(
        reports[0].outcome,
        eval::Outcome::Returned(vec![0, 5, 0xdecafbad, 0])
    );
    assert_eq!(reports[1].outcome, eval::Outcome::Returned(vec![0, 1]));
    for report in reports {
        assert_eq!(report.used, 0);
        assert_eq!(report.admissions, 0);
    }
}

#[test]
fn joined_cause_views_use_core_layout() {
    let module = text::parse(include_str!("../../../hirsel-core-ir/samples/custody.core")).unwrap();
    let actual = execute(
        &module,
        &[Case {
            function: FuncId(6),
            args: vec![],
            limit: 1000,
            injection: None,
        }],
    );
    assert_eq!(actual[0].outcome, eval::Outcome::Returned(vec![1, 1, 4, 4]));
    assert_eq!(actual[0].used, 0);
    assert_eq!(actual[0].trace, vec![7]);
}

#[test]
fn lazy_component_preserves_exact_core_objects() {
    for source in [
        include_str!("../../../hirsel-core-ir/samples/arith.core"),
        include_str!("../../../hirsel-core-ir/samples/strings.core"),
        include_str!("../../../hirsel-core-ir/samples/custody.core"),
    ] {
        let module = text::parse(source).unwrap();
        for speed in [false, true] {
            let direct = emit_object(
                &module,
                if speed {
                    Optimization::Speed
                } else {
                    Optimization::None
                },
            )
            .unwrap();
            let lazy = hirsel_llvm_lazy::compile_ir(&emit_ir(&module).unwrap(), speed).unwrap();
            assert_eq!(lazy, direct, "identical LLVM objects, speed={speed}");
        }
    }
}

#[test]
fn dynamic_overflow_and_local_like_admissions_keep_exact_refusal_counts() {
    let module = text::parse(include_str!(
        "../../../hirsel-cranelift/tests/support/inline-admission.core"
    ))
    .unwrap();
    wf::module(&module).unwrap();
    let mut cases = vec![];
    for function in 0..3 {
        for charge in [0, 8, 40, 72, u64::MAX] {
            for overflow in [0, 1] {
                let args = vec![charge, overflow];
                let baseline = eval::run(&module, FuncId(function), &args, 64);
                for nth in 1..=baseline.admissions {
                    for later in [false, true] {
                        cases.push(Case {
                            function: FuncId(function),
                            args: args.clone(),
                            limit: 64,
                            injection: Some((nth, later)),
                        });
                    }
                }
                cases.push(Case {
                    function: FuncId(function),
                    args,
                    limit: 64,
                    injection: None,
                });
            }
        }
    }
    println!("ordinary admission cases: {}", cases.len());
    agree_cases(&module, &cases, true);
    // Null Like is the native ABI's Current spelling; the evaluator expects a
    // real block for Like. Its one admission and room are derived independently.
    let mut null_cases = vec![];
    for charge in [0, 8, 40, 72, u64::MAX] {
        for overflow in [0, 1] {
            for injection in [None, Some((1, false)), Some((1, true))] {
                null_cases.push(Case {
                    function: FuncId(3),
                    args: vec![charge, overflow],
                    limit: 64,
                    injection,
                });
            }
        }
    }
    let actual = execute(&module, &null_cases);
    for (case, got) in null_cases.iter().zip(actual) {
        let success = case.args[1] == 0 && case.args[0] <= 64 && case.injection.is_none();
        assert_eq!(
            got.outcome,
            eval::Outcome::Returned(vec![u64::from(success), if success { 0 } else { 64 }])
        );
        assert_eq!(got.used, 0);
        assert_eq!(got.admissions, 1);
        assert!(got.trace.is_empty());
    }
    println!("null-Like native admission cases: {}", null_cases.len());
}

#[test]
fn private_register_results_keep_the_public_destination_and_failure_abi() {
    // A 24-byte destination plus status keeps the ordinary destination ABI.
    for (source, function) in [
        (
            include_str!("../../tests/core_runtime/forward.core"),
            FuncId(0),
        ),
        (
            include_str!("../../tests/core_runtime/register.core"),
            FuncId(0),
        ),
        (
            include_str!("../../../hirsel-core-ir/samples/failure.core"),
            FuncId(1),
        ),
    ] {
        let module = text::parse(source).unwrap();
        wf::module(&module).unwrap();
        let ir = emit_ir(&module).unwrap();
        let exports = module
            .functions
            .iter()
            .enumerate()
            .map(|(i, _)| symbol(&module, FuncId(i as u32)))
            .collect::<Vec<_>>();
        let bytes =
            hirsel_llvm_lazy::emit_program(&[hirsel_llvm_lazy::Unit::Text(&ir)], &exports).unwrap();
        let cases = [0, 1, 2, 3, 123].map(|value| Case {
            function,
            args: vec![value],
            limit: u64::MAX,
            injection: None,
        });
        let got = execute_object(&module, &cases, "", "", Optimization::None, Some(bytes));
        assert_eq!(got.len(), cases.len());
        for (case, actual) in cases.iter().zip(got) {
            let expected = eval::run(&module, case.function, &case.args, case.limit);
            assert_eq!(actual.outcome, expected.outcome);
            assert_eq!(actual.used, expected.used);
            assert_eq!(actual.trace, expected.trace);
        }
    }
}

#[test]
#[ignore = "explicit AArch64 runtime integration target"]
fn aarch64_memory_custody_and_calls_match_the_evaluator() {
    let target = Target::Aarch64Linux;
    let mut count = 0;
    for source in [
        include_str!("../../../hirsel-core-ir/samples/arith.core"),
        include_str!("../../../hirsel-core-ir/samples/calls.core"),
        include_str!("../../../hirsel-core-ir/samples/admission.core"),
        include_str!("../../../hirsel-core-ir/samples/cleanup.core"),
        include_str!("../../../hirsel-core-ir/samples/counted.core"),
        include_str!("../../../hirsel-core-ir/samples/failure.core"),
        include_str!("../../../hirsel-core-ir/samples/variants.core"),
        include_str!("../../../hirsel-core-ir/samples/custody.core"),
        include_str!("../../../hirsel-core-ir/samples/isolate.core"),
    ] {
        let module = text::parse(source).unwrap();
        wf::module(&module).unwrap();
        let isolate = source.contains("family: isolation");
        let mut cases = sample_cases(&module, source);
        if isolate {
            for case in &mut cases {
                if case.limit == u64::MAX {
                    case.limit = 1_000_000;
                }
            }
        }
        for case in sample_cases(&module, source) {
            let limit = if isolate && case.limit == u64::MAX {
                1_000_000
            } else {
                case.limit
            };
            let report = eval::run(&module, case.function, &case.args, limit);
            for nth in 1..=report.admissions {
                for later in [false, true] {
                    cases.push(Case {
                        function: case.function,
                        args: case.args.clone(),
                        limit,
                        injection: Some((nth, later)),
                    });
                }
            }
        }
        assert!(!cases.is_empty());
        for speed in [false, true] {
            for facts in [false, true] {
                let ir = emit_ir_target_with_facts(
                    &module,
                    target,
                    false,
                    facts,
                    SourceEmission {
                        batch_steps: true,
                        test_hooks: true,
                        relocatable_sites: false,
                        debug_lines: true,
                        fixed_sites: &[],
                    },
                )
                .unwrap();
                let object = hirsel_llvm_lazy::compile_ir_for_target(&ir, speed, target).unwrap();
                let got = execute_target_object(
                    &module,
                    &cases,
                    "",
                    "",
                    if speed {
                        Optimization::Speed
                    } else {
                        Optimization::None
                    },
                    Some(object),
                    target,
                );
                assert_eq!(got.len(), cases.len());
                for (case, got) in cases.iter().zip(got) {
                    let expected = eval::run_injected(
                        &module,
                        case.function,
                        &case.args,
                        case.limit,
                        case.injection,
                    );
                    assert_eq!(
                        got.outcome, expected.outcome,
                        "f{}({:?}),speed={speed},facts={facts}",
                        case.function.0, case.args
                    );
                    assert_eq!(got.used, expected.used, "used");
                    assert_eq!(got.trace, expected.trace, "cleanup trace");
                    assert_eq!(got.admissions, expected.admissions, "admissions");
                    count += 1;
                }
            }
        }
    }
    println!("ARM runtime sample and refusal executions: {count}");
}

fn inheritance_fixture(target: Target) {
    let module = text::parse(
        r#"core v1
item i0 = p@0/m/caller
item i1 = p@0/m/local
item i2 = core@1/m/k
sig g0 = (u64) -> (u64)
inst n0 = core@1/m/k [] []
inst n1 = p@0/m/caller [] []
inst n2 = p@0/m/local [] []
inst n3 = p@0/m/caller.$glue [] []
inst n4 = p@0/m/caller_local [] []
inst n5 = p@0/m/caller_generated [] []
inst n6 = p@0/m/caller_indirect_core [] []
inst n7 = p@0/m/caller_indirect_local [] []
fn f0 = n0 g0 {
b0(v0: u64):
  v1: u64 = const 1
  v2: u64 = add v0, v1 at i2 5..10
  return v2
}
fn f1 = n1 g0 {
b0(v0: u64):
  v1: u64 = call f0 g0 (v0) at i0 20..30
  return v1
}
fn f2 = n2 g0 {
b0(v0: u64):
  v1: u64 = const 1
  v2: u64 = add v0, v1 at i1 35..40
  return v2
}
fn f3 = n4 g0 {
b0(v0: u64):
  v1: u64 = call f2 g0 (v0) at i0 40..45
  return v1
}
fn f4 = n3 g0 {
b0(v0: u64):
  v1: u64 = const 1
  v2: u64 = add v0, v1
  return v2
}
fn f5 = n5 g0 {
b0(v0: u64):
  v1: u64 = call f4 g0 (v0) at i0 50..55
  return v1
}
fn f6 = n6 g0 {
b0(v0: u64):
  v1: ptr = fnref f0
  v2: u64 = call v1 g0 (v0) at i0 60..65
  return v2
}
fn f7 = n7 g0 {
b0(v0: u64):
  v1: ptr = fnref f2
  v2: u64 = call v1 g0 (v0) at i0 70..75
  return v2
}
"#,
    )
    .unwrap();
    hirsel_core_ir::wf::module(&module).unwrap();
    let sites = module.sites();
    let ir = emit_ir_for_target(&module, target, false, &sites).unwrap();
    let callee = symbol(&module, FuncId(2));
    let direct = ir
        .lines()
        .find(|line| line.contains("call ") && line.contains(&format!("@{callee}(")))
        .unwrap();
    assert!(
        direct.contains("i64 poison)"),
        "non-inheriting direct call materializes a site: {direct}"
    );
    let expected = [
        0,
        sites[0] + 20,
        sites[1] + 35,
        sites[1] + 35,
        0,
        sites[0] + 50,
        sites[0] + 60,
        sites[1] + 35,
    ];
    let cases = (0..8)
        .map(|f| Case {
            function: FuncId(f),
            args: vec![u64::MAX],
            limit: u64::MAX,
            injection: None,
        })
        .collect::<Vec<_>>();
    for opt in [Optimization::None, Optimization::Speed] {
        let bytes = emit_object_for_target(&module, target, opt, false, &sites).unwrap();
        let reports = execute_target_object(&module, &cases, "", "", opt, Some(bytes), target);
        for ((case, actual), site) in cases.iter().zip(reports).zip(expected) {
            let oracle = eval::run(&module, case.function, &case.args, u64::MAX);
            assert_eq!(actual.outcome, oracle.outcome);
            assert_eq!(actual.sites, vec![site], "inheritance f{}", case.function.0);
        }
    }
    println!("caller inheritance observations: 16");
}

#[test]
fn caller_sites_follow_core_generated_direct_and_indirect_inheritance() {
    inheritance_fixture(Target::X86_64Linux);
}

#[test]
#[ignore = "explicit AArch64 runtime integration target"]
fn aarch64_caller_sites_follow_core_generated_direct_and_indirect_inheritance() {
    inheritance_fixture(Target::Aarch64Linux);
}

fn check_consuming_growth(target: Target) {
    let source = include_str!("../../../hirsel-core-rt/testdata/grow.core");
    let module = text::parse(source).unwrap();
    wf::module(&module).unwrap();
    let mut cases = Vec::new();
    for case in sample_cases(&module, source) {
        let report = eval::run(&module, case.function, &case.args, case.limit);
        for nth in 1..=report.admissions {
            for later in [false, true] {
                cases.push(Case {
                    function: case.function,
                    args: case.args.clone(),
                    limit: case.limit,
                    injection: Some((nth, later)),
                });
            }
        }
        cases.push(case);
    }
    for speed in [false, true] {
        let ir = emit_ir_target_with_facts(
            &module,
            target,
            std::env::var_os("HIRSEL_TEST_CHARGE_OBSERVATION").is_some(),
            true,
            SourceEmission {
                batch_steps: true,
                test_hooks: true,
                relocatable_sites: false,
                debug_lines: false,
                fixed_sites: &[],
            },
        )
        .unwrap();
        let object = hirsel_llvm_lazy::compile_ir_for_target(&ir, speed, target).unwrap();
        let reports = execute_target_object(
            &module,
            &cases,
            "",
            "",
            if speed {
                Optimization::Speed
            } else {
                Optimization::None
            },
            Some(object),
            target,
        );
        assert_eq!(reports.len(), cases.len());
        for (case, actual) in cases.iter().zip(reports) {
            let expected = eval::run_injected(
                &module,
                case.function,
                &case.args,
                case.limit,
                case.injection,
            );
            assert_eq!(actual.outcome, expected.outcome);
            assert_eq!(actual.used, expected.used);
            assert_eq!(actual.trace, expected.trace);
            assert_eq!(actual.admissions, expected.admissions);
            if let Some(peak) = actual.peak_reserved {
                assert_eq!(peak, expected.peak_reserved);
            }
        }
    }
    println!(
        "consuming grow: {} cases per profile, both profiles",
        cases.len()
    );
}
#[test]
fn consuming_growth_keeps_the_carrier_and_original_payer_on_refusal() {
    check_consuming_growth(Target::X86_64Linux);
}
#[test]
#[ignore = "explicit AArch64 runtime integration target"]
fn aarch64_consuming_growth_keeps_the_carrier_and_original_payer_on_refusal() {
    check_consuming_growth(Target::Aarch64Linux);
}

#[test]
#[ignore = "explicit AArch64 runtime integration target"]
fn aarch64_consuming_growth_host_memory_refusal_is_a_reported_run_stop() {
    host_memory::check_aarch64(
        |module| {
            let ir = emit_ir_target_with_facts(
                module,
                Target::Aarch64Linux,
                false,
                true,
                SourceEmission {
                    batch_steps: true,
                    test_hooks: true,
                    relocatable_sites: false,
                    debug_lines: false,
                    fixed_sites: &[],
                },
            )
            .unwrap();
            hirsel_llvm_lazy::compile_ir_for_target(&ir, true, Target::Aarch64Linux).unwrap()
        },
        symbol,
    );
}

fn verify_ir(ir: &str) {
    let context = Context::new();
    let llvm = LlvmModule::parse(&context, "test", ir).unwrap();
    llvm.verify().unwrap_or_else(|e| panic!("{e}\n{ir}"));
}

#[test]
fn wire_socket_drop_and_split_execute_through_native_cleanup() {
    let source = format!(
        "{}\n{}",
        include_str!("../../../hirsel-core-ir/samples/net.core"),
        r#"
sig g3 = () -> (u64)
inst n3 = test@1/net/prepare [] []
inst n4 = test@1/net/observe [] []
inst n5 = test@1/net/native_drop [] []
inst n6 = test@1/net/native_split [] []
fn f3 = n3 g3 extern
fn f4 = n4 g3 extern
fn f5 = n5 g3 {
b0:
 v0: u64 = call f3 g3 () fail b1
 call f0 g0 (v0) fail b1
 v1: u64 = call f4 g3 () fail b1
 return v1
b1:
 propagate
}
fn f6 = n6 g3 {
b0:
 v0: u64 = call f3 g3 () fail b1
 call f1 g0 (v0) fail b1
 v1: u64 = call f4 g3 () fail b1
 return v1
b1:
 propagate
}
"#
    );
    let module = text::parse(&source).unwrap();
    wf::module(&module).unwrap();
    let bindings = format!(
        r#"
#include <dirent.h>
#include <sys/socket.h>
#include <unistd.h>
extern uint64_t hirsel_core_test_socket_adopt(HirselCoreContext *,int);
static int peer;
static size_t before_fds, before_attempts;
static uint64_t before_available;
static size_t wire_fds(void) {{ DIR *d=opendir("/dev/fd"); assert(d); size_t n=0; while(readdir(d)) n++; closedir(d); return n; }}
R3 {prepare}(void *raw,uint64_t site) {{
 HirselCoreContext *ctx=raw;
 (void)site; before_fds=wire_fds(); before_attempts=hirsel_core_test_attempts(ctx); before_available=hirsel_core_test_available(ctx);
 int fds[2]; assert(socketpair(AF_UNIX,SOCK_STREAM|SOCK_CLOEXEC,0,fds)==0); peer=fds[1];
 return (R3){{.r0=hirsel_core_test_socket_adopt(ctx,fds[0])}};
}}
R4 {observe}(void *raw,uint64_t site) {{
 HirselCoreContext *ctx=raw;
 (void)site; close(peer); assert(wire_fds()==before_fds);
 assert(hirsel_core_test_attempts(ctx)==before_attempts);
 assert(hirsel_core_test_available(ctx)==before_available);
 return (R4){{.r0=1}};
}}
"#,
        prepare = symbol(&module, FuncId(3)),
        observe = symbol(&module, FuncId(4))
    );
    let cases: Vec<_> = [5, 6]
        .into_iter()
        .map(|f| Case {
            function: FuncId(f),
            args: vec![],
            limit: 1000,
            injection: None,
        })
        .collect();
    for report in execute_with_bindings(&module, &cases, &bindings) {
        assert_eq!(report.outcome, eval::Outcome::Returned(vec![1]));
        assert_eq!(report.used, 0);
        assert_eq!(report.admissions, 0);
    }
}
