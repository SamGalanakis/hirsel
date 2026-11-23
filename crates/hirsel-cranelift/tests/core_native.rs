//! CoreIR native conformance through an independently compiled C caller.
const NATIVE_TARGET: &str = "x86_64-unknown-linux-gnu";
hirsel_guard::install!();
include!("support/native.rs");
#[path = "support/providers.rs"]
mod providers;

#[test]
fn map_placement_operations_match_fixed_key_vectors() {
    mod vectors {
        include!("support/map-placement-vectors.rs");
    }
    let module = text::parse(include_str!("support/map-placement.core")).unwrap();
    let arguments: Vec<_> = vectors::CASES.iter().map(|&(_, arg, _)| [arg]).collect();
    let reports: Vec<_> = vectors::CASES
        .iter()
        .zip(&arguments)
        .map(|(&(f, _, expected), args)| {
            let key = std::array::from_fn(|i| i as u8);
            let (report, _) = eval::run_map_hash(&module, FuncId(f), args, 0, key);
            assert_eq!(report.outcome, Outcome::Returned(expected.to_vec()));
            report
        })
        .collect();
    let cases: Vec<_> = vectors::CASES
        .iter()
        .zip(&arguments)
        .zip(&reports)
        .map(|((&(f, _, _), args), report)| NativeCase {
            function: FuncId(f),
            args,
            limit: 0,
            injection: None,
            report: Some(report),
        })
        .collect();
    execute_cases_with_bindings(
        &module,
        true,
        &cases,
        false,
        "#define hirsel_core_test_context_new hirsel_core_test_context_new_map_vectors\n",
        NativeCheck::Exact,
    );
}

#[test]
fn native_allocator_refusals_are_reported_run_stops() {
    host_memory::check_with_site(
        |module| {
            emit_object_with_layouts(
                module,
                &hirsel_core_layout::compute(&module.types).unwrap(),
                &Options {
                    target: "x86_64-unknown-linux-gnu",
                    optimize: false,
                    debug_lines: false,
                },
                hirsel_core_ids::ContentHash([0x90; 32]),
            )
            .unwrap()
            .bytes
        },
        |module, function| {
            module
                .identity(module.functions[function.index()].instance)
                .symbol()
        },
    );
}

#[test]
fn compare_convert_sample_speed_matches_evaluator_through_c_abi() {
    let text = include_str!("../../hirsel-core-ir/samples/compare-convert.core");
    let module = text::parse(text).unwrap();
    let runs = sample_runs(&module, text);
    assert_eq!(runs.len(), 32);
    execute(&module, &runs, true);
}

#[test]
fn slots_loops_calls_and_c_struct_returns_match_evaluator() {
    check_c_struct_calls();
}

#[test]
fn integer_modes_and_float_conversion_boundaries_match_evaluator() {
    let mut declarations = String::from("core v0\n");
    let mut bodies = String::new();
    let mut runs = Vec::new();
    let mut id = 0;
    for kind in ["i8", "u8", "i16", "u16", "i32", "u32", "i64", "u64"] {
        let scalar = Scalar::from_name(kind).unwrap();
        let t = Ty::Scalar(scalar);
        let (signed, width) = t.int().unwrap();
        let mask = if width == 64 {
            u64::MAX
        } else {
            (1u64 << width) - 1
        };
        let min = 1u64 << (width - 1);
        let max = if signed { min - 1 } else { mask };
        for mode in ["wrapping", "saturating", "checked"] {
            for op in ["add", "sub", "mul", "div"] {
                if op == "div" && mode != "checked" {
                    continue;
                }
                let checked = mode == "checked";
                let results = if checked {
                    format!("{kind}, bool")
                } else {
                    kind.into()
                };
                writeln!(declarations,"sig g{id} = ({kind}, {kind}) -> ({results})\ninst n{id} = test@0/numeric/f{id} [] []").unwrap();
                let flags = if checked { ", v3: bool" } else { "" };
                let returned = if checked { ", v3" } else { "" };
                writeln!(bodies,"fn f{id} = n{id} g{id} {{\nb0(v0: {kind}, v1: {kind}):\n  v2: {kind}{flags} = {mode}_{op} v0, v1\n  return v2{returned}\n}}").unwrap();
                for args in [[0, 0], [max, 1], [max, 2], [min, mask], [mask, 2], [min, 0]] {
                    runs.push((FuncId(id), args.into()));
                }
                id += 1;
            }
        }
        // A rotate that drops the wrapped bits, rotates right, or reads the count
        // signed or unreduced would place Map keys in other buckets without changing
        // any program output; Rust's `rotate_left` is the oracle for the evaluator.
        writeln!(
            declarations,
            "sig g{id} = ({kind}, {kind}) -> ({kind})\ninst n{id} = test@0/numeric/f{id} [] []"
        )
        .unwrap();
        writeln!(bodies,"fn f{id} = n{id} g{id} {{\nb0(v0: {kind}, v1: {kind}):\n  v2: {kind} = rotl v0, v1\n  return v2\n}}").unwrap();
        for x in [min | 1, 0xa5c3_0f96_5a3c_f069 & mask] {
            for count in [
                0,
                1,
                u64::from(width) - 1,
                width.into(),
                u64::from(width) + 3,
                mask,
            ] {
                let rotated = match width {
                    8 => u64::from((x as u8).rotate_left(count as u32)),
                    16 => u64::from((x as u16).rotate_left(count as u32)),
                    32 => u64::from((x as u32).rotate_left(count as u32)),
                    _ => x.rotate_left(count as u32),
                };
                let eval::Outcome::Returned(got) = eval::run(
                    &text::parse(&format!("{declarations}{bodies}")).unwrap(),
                    FuncId(id),
                    &[x, count],
                    u64::MAX,
                )
                .outcome
                else {
                    panic!("rotl {kind} {x:#x} {count}");
                };
                assert_eq!(got[0] & mask, rotated, "rotl {kind} {x:#x} by {count}");
                runs.push((FuncId(id), vec![x, count]));
            }
        }
        id += 1;
        writeln!(
            declarations,
            "sig g{id} = (f64) -> ({kind}, u8)\ninst n{id} = test@0/numeric/f{id} [] []"
        )
        .unwrap();
        writeln!(bodies,"fn f{id} = n{id} g{id} {{\nb0(v0: f64):\n  v1: {kind}, v2: u8 = float_to_int v0\n  return v1, v2\n}}").unwrap();
        let upper = (2f64).powi(width as i32 - i32::from(signed));
        let lower = if signed { -upper } else { 0.0 };
        for value in [
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            -0.9,
            0.0,
            2.9,
            -2.9,
            lower,
            upper,
            upper - f64::EPSILON * upper,
            lower - 0.5,
            lower - 1.0,
        ] {
            runs.push((FuncId(id), vec![value.to_bits()]));
        }
        id += 1;
    }
    declarations.push_str(&bodies);
    let m = text::parse(&declarations).unwrap();
    wf::module(&m).unwrap();
    execute(&m, &runs, false);
}

#[test]
fn memory_growth_region_and_capacity_refusal_match_evaluator() {
    let source = r#"core v1
sig g0 = (u64) -> (u64, u64)
sig g1 = () -> (bool, u64)
inst n0 = test@0/memory/growth [] []
inst n1 = test@0/memory/region [] []
inst n2 = test@0/memory/refusal [] []
fn f0 = n0 g0 {
  addr a0 = v2 : u64
  addr a1 = v7 : u64
b0(v0: u64):
  v1: u64 = const 8
  v2: ptr, v3: u64 = alloc for seq u64 v1 align 8 charge v1
  address a0
  store a0, v0
  v4: u64 = const 16
  v5: bool, v6: u64 = limit_enter v4
  v7: ptr, v8: u64 = alloc for seq u64 v4 align 8 charge v4 like v2
  address a1
  copy a1, a0, v1
  free v2 charge v1
  limit_exit
  v9: u64 = read a1
  free v7 charge v4
  v10: u64 = const 101
  v11: bool, v12: u64 = limit_enter v10
  return v9, v12
}
fn f1 = n1 g0 {
  addr a0 = v3 : u64
b0(v0: u64):
  v1: ptr = region_open
  v2: u64 = const 8
  v3: ptr, v4: u64 = alloc region v2 align 8 charge v2 in v1
  address a0
  store a0, v0
  v5: u64 = const 128
  v6: bool, v7: u64 = limit_enter v5
  v8: u64 = read a0
  region_close v1
  return v8, v7
}
fn f2 = n2 g1 {
b0:
  v0: u64 = const 200
  v5: u64 = const 8
  v1: ptr, v2: u64 = alloc for seq u8 v5 align 8 charge v0
  v3: ptr = const 0x0
  v4: bool = ne v1, v3
  return v4, v2
}
"#;
    let module = text::parse(source).unwrap();
    wf::module(&module).unwrap();
    execute_limit(
        &module,
        &[
            (FuncId(0), vec![42]),
            (FuncId(1), vec![91]),
            (FuncId(2), vec![]),
        ],
        false,
        100,
    );
}

#[test]
fn every_whole_v0_sample_matches_evaluator() {
    let samples = [
        include_str!("../../hirsel-core-ir/samples/arith.core"),
        include_str!("../../hirsel-core-ir/samples/compare-convert.core"),
        include_str!("../../hirsel-core-ir/samples/control.core"),
        include_str!("../../hirsel-core-ir/samples/slots.core"),
        include_str!("../../hirsel-core-ir/samples/calls.core"),
    ];
    for source in samples {
        let module = text::parse(source).unwrap();
        execute(&module, &sample_runs(&module, source), false);
    }
}

#[test]
fn declared_places_variants_and_destination_calls_match_evaluator() {
    let source = r#"core v1
type t0 = own "Row" { small: u8, large: u64 }
type t1 = own "Maybe" | None { } | Some { x: u64 }
type t2 = own "UnusedFile" { file: hostfile }
type t3 = own "UnusedSequence" { bytes: seq u8 }
type t4 = own "UnusedCounted" { data: rc t3 }
sig g0 = (u64) -> (u8, u64, u32)
sig g1 = (u64) -> () dest t0
inst n0 = test@0/places/run [] []
inst n1 = test@0/places/fill [] []
fn f0 = n0 g0 {
  slot s0 = t0
  slot s1 = t1
  addr a0 = s0 : t0
  addr a1 = s0 field 0 : u8
  addr a2 = s0 field 1 : u64
  addr a3 = s1 : t1
  addr a4 = s1 case 1 0 : u64
b0(v0: u64):
  live s0
  live s1
  address a0
  address a1
  address a2
  address a3
  address a4
  call f1 g1 (&a0, v0)
  set_case a3, 1
  store a4, v0
  v1: u8 = read a1
  v2: u64 = read a2
  v3: u32 = case a3
  dead s1
  dead s0
  return v1, v2, v3
}
fn f1 = n1 g1 {
  addr a0 = v0 : t0
  addr a1 = a0 field 0 : u8
  addr a2 = a0 field 1 : u64
b0(v0: ptr, v1: u64):
  address a0
  address a1
  address a2
  v2: u8 = const 7
  store a1, v2
  store a2, v1
  return
}
"#;
    let module = text::parse(source).unwrap();
    wf::module(&module).unwrap();
    execute(&module, &[(FuncId(0), vec![42])], false);
}

#[test]
fn every_whole_v1a_sample_matches_evaluator() {
    for source in [
        include_str!("../../hirsel-core-ir/samples/admission.core"),
        include_str!("../../hirsel-core-ir/samples/cleanup.core"),
        include_str!("../../hirsel-core-ir/samples/counted.core"),
        include_str!("../../hirsel-core-ir/samples/failure.core"),
        include_str!("../../hirsel-core-ir/samples/variants.core"),
        include_str!("../../hirsel-core-ir/samples/stable-keys.core"),
    ] {
        check_sample_with_refusals(source, false);
    }
}

/// Bug: Cranelift skips an own row's core finalizer (FORM v1c) in ordinary or
/// counted cleanup, or brackets it with a resource's grant switch, so a Deque drops
/// the wrong slots or leaks; or it emits code for a `mark`, which has no effect.
#[test]
fn slot_kernels_match_evaluator_and_marks_emit_nothing() {
    let source = include_str!("../../hirsel-core-ir/samples/kernel.core");
    let module = text::parse(source).unwrap();
    for optimize in [false, true] {
        execute(&module, &sample_runs(&module, source), optimize);
        let layouts = hirsel_core_layout::compute(&module.types).unwrap();
        let emit = |m: &c::Module| {
            emit_object_with_layouts(
                m,
                &layouts,
                &Options {
                    target: "x86_64-unknown-linux-gnu",
                    optimize,
                    debug_lines: false,
                },
                hirsel_core_ids::ContentHash([0x90; 32]),
            )
            .unwrap()
            .bytes
        };
        let stripped = c::wf::without_marks(&module);
        assert_ne!(stripped, module, "the sample has marks");
        assert!(
            emit(&module) == emit(&stripped),
            "a mark changed the object"
        );
    }
}

#[test]
fn capacity_at_each_admission_preserves_cleanup_chain_and_balance() {
    let module = text::parse(include_str!("support/admissions.core")).unwrap();
    let mut executions = 0;
    for argument in [0, 1] {
        let runs = [(FuncId(0), vec![argument])];
        let baseline = eval::run(&module, FuncId(0), &[argument], 1000);
        execute_limit(&module, &runs, false, 1000);
        for n in 1..=baseline.admissions {
            for later in [false, true] {
                execute_injected(&module, &runs, false, 1000, Some((n, later)));
                executions += 1;
            }
        }
    }
    assert_eq!(executions, 14);
}

#[test]
fn counted_variant_chain_uses_constant_stack_and_refunds_every_node() {
    let module = text::parse(include_str!("support/counted-chain.core")).unwrap();
    execute(
        &module,
        &[(FuncId(0), vec![0]), (FuncId(0), vec![32])],
        false,
    );
    for n in 1..=5 {
        for later in [false, true] {
            execute_injected(
                &module,
                &[(FuncId(0), vec![4])],
                false,
                1000,
                Some((n, later)),
            );
        }
    }
    // Hand-derived oracle avoids imposing the evaluator's recursive host-stack
    // implementation on the generated code's constant-stack requirement.
    let n = 100_000;
    execute_reports(
        &module,
        &[(FuncId(0), vec![n])],
        false,
        10_000_000,
        None,
        Some(&[eval::Report {
            outcome: Outcome::Returned(vec![n]),
            used: 0,
            peak_reserved: 0, // This independent fixture specifies outcomes/ledger, not its high water.
            trace: vec![],
            admissions: n as usize + 1,
            stdout: vec![],
            stderr: vec![],
            sites: vec![],
            events: vec![],
        }]),
    );
}

/// A suspended fixed block is found again only through its record: an array
/// element reuses one record, and two cases of one enum suspend at the same
/// cursor with different offsets.
/// LANG-19: ordinary release of a 10^6-deep `Box` chain, of one whose rows
/// carry a finalizer, of a closure whose environment captures a closure 10^6
/// deep, of a node whose List holds the next node 10^6 deep, and of a resource
/// whose field owns the next resource 10^6 deep, completes under the default
/// stack, and every block is refunded (hand-derived: n + 1 boxes, n
/// environments, n buffers, n resources and their n boxes).
#[test]
fn box_chain_releases_with_constant_stack() {
    let n = 1_000_000;
    for (source, admissions) in [
        (
            include_str!("../../hirsel-core-ir/samples/box-chain.core"),
            n as usize + 1,
        ),
        (
            include_str!("../../hirsel-core-ir/samples/closure-chain.core"),
            n as usize,
        ),
        (
            include_str!("../../hirsel-core-ir/samples/finalizer-chain.core"),
            n as usize + 1,
        ),
        (
            include_str!("../../hirsel-core-ir/samples/seq-chain.core"),
            n as usize,
        ),
        (
            include_str!("../../hirsel-core-ir/samples/seq-box-chain.core"),
            2 * n as usize,
        ),
        (
            include_str!("../../hirsel-core-ir/samples/resource-chain.core"),
            2 * n as usize,
        ),
    ] {
        let module = text::parse(source).unwrap();
        for optimize in [false, true] {
            execute(&module, &sample_runs(&module, source), optimize);
            execute_reports(
                &module,
                &[(FuncId(0), vec![n])],
                optimize,
                100_000_000,
                None,
                Some(&[eval::Report {
                    outcome: Outcome::Returned(vec![n]),
                    used: 0,
                    peak_reserved: 0,
                    trace: vec![],
                    admissions,
                    stdout: vec![],
                    stderr: vec![],
                    sites: vec![],
                    events: vec![],
                }]),
            );
        }
    }
}

/// LANG-19 through receivers: a value whose depth runs through channel
/// queues releases under the default stack. One chain of 10^6 receiver hops,
/// and a capacity-2 queue holding two chains of 5 * 10^5 hops each, cleaned
/// last published first (hand-derived: m (n + 1) + 1 admissions, Toks traced
/// m..1, nothing used).
#[test]
fn receiver_chain_releases_with_constant_stack() {
    let source = include_str!("../../hirsel-core-ir/samples/receiver-chain.core");
    let module = text::parse(source).unwrap();
    for optimize in [false, true] {
        check_sample_with_refusals(source, optimize);
        let runs = [(1_000_000u64, 1u64), (500_000, 2)];
        let reports: Vec<_> = runs
            .iter()
            .map(|&(n, m)| eval::Report {
                outcome: Outcome::Returned(vec![m]),
                used: 0,
                peak_reserved: 0,
                trace: (1..=m).rev().collect(),
                admissions: (m * (n + 1) + 1) as usize,
                stdout: vec![],
                stderr: vec![],
                sites: vec![],
                events: vec![],
            })
            .collect();
        execute_reports(
            &module,
            &runs.map(|(n, m)| (FuncId(0), vec![n, m])),
            optimize,
            1_000_000_000,
            None,
            Some(&reports),
        );
    }
}

#[test]
fn counted_frames_recover_the_block_from_the_record() {
    let source = include_str!("../../hirsel-core-ir/samples/counted-frames.core");
    let module = text::parse(source).unwrap();
    for optimize in [false, true] {
        execute(&module, &sample_runs(&module, source), optimize);
    }
}

#[test]
fn string_literals_grow_and_clean_in_dev_and_speed() {
    let source = include_str!("../../hirsel-core-ir/samples/strings.core");
    let module = text::parse(source).unwrap();
    let mut cases: Vec<_> = sample_runs(&module, source)
        .into_iter()
        .map(|run| (run, 1_000_000))
        .collect();
    for line in source
        .lines()
        .filter_map(|l| l.trim().strip_prefix("; run limit "))
    {
        let (limit, call) = line.split_once(": ").unwrap();
        cases.extend(
            sample_runs(&module, &format!("; run: {call}"))
                .into_iter()
                .map(|run| (run, limit.parse().unwrap())),
        );
    }
    for optimize in [false, true] {
        for (run, limit) in &cases {
            execute_limit(&module, std::slice::from_ref(run), optimize, *limit);
            let baseline = eval::run(&module, run.0, &run.1, *limit);
            for n in 1..=baseline.admissions {
                for later in [false, true] {
                    execute_injected(
                        &module,
                        std::slice::from_ref(run),
                        optimize,
                        *limit,
                        Some((n, later)),
                    );
                }
            }
        }
    }

    // The sample's fixed-size push folds its room test. Exercise a real loop,
    // then Seq/Map cleanup with nonnull static data in both count modes.
    let source = include_str!("support/literal-storage.core");
    for counted in ["rc", "arc"] {
        let source = source
            .replace("for rc t1", &format!("for {counted} t1"))
            .replace("release v12 rc", &format!("release v12 {counted}"));
        let module = text::parse(&source).unwrap();
        wf::module(&module).unwrap();
        let mut runs = Vec::new();
        for n in [0, 1, 3, 4, 12] {
            let run = (FuncId(0), vec![n]);
            let report = eval::run(&module, run.0, &run.1, 1000);
            assert_eq!(report.outcome, Outcome::Returned(vec![5 + n, 532 + 88 * n]));
            assert_eq!(report.used, 0);
            runs.push(run);
        }
        for optimize in [false, true] {
            execute(&module, &runs, optimize);
            // Three growth admissions exercise literal -> owned, owned -> owned,
            // and the non-growing push path, with the live prefix cleaned on refusal.
            let run = (FuncId(0), vec![12]);
            let baseline = eval::run(&module, run.0, &run.1, 1000);
            for n in 1..=baseline.admissions {
                for later in [false, true] {
                    execute_injected(
                        &module,
                        std::slice::from_ref(&run),
                        optimize,
                        1000,
                        Some((n, later)),
                    );
                }
            }
            // Map cleanup is not implemented by the evaluator. The independent
            // oracle is no causes, no trace, no retained charge; counted storage
            // adds one admission, whose refusal raises only Unwrap.
            for counted_drop in [false, true] {
                let run = (FuncId(1), vec![u64::from(counted_drop)]);
                execute_reports(
                    &module,
                    std::slice::from_ref(&run),
                    optimize,
                    1000,
                    None,
                    Some(&[eval::Report {
                        outcome: Outcome::Returned(vec![]),
                        used: 0,
                        peak_reserved: 0, // This independent fixture specifies outcomes/ledger, not its high water.
                        trace: vec![],
                        admissions: usize::from(counted_drop),
                        stdout: vec![],
                        stderr: vec![],
                        sites: vec![],
                        events: vec![],
                    }]),
                );
                if counted_drop {
                    for later in [false, true] {
                        execute_reports(
                            &module,
                            std::slice::from_ref(&run),
                            optimize,
                            1000,
                            Some((1, later)),
                            Some(&[eval::Report {
                                outcome: Outcome::Failed(vec!["Unwrap".into()]),
                                used: 0,
                                peak_reserved: 0, // This independent fixture specifies outcomes/ledger, not its high water.
                                trace: vec![],
                                admissions: 1,
                                stdout: vec![],
                                stderr: vec![],
                                sites: vec![],
                                events: vec![],
                            }]),
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn every_compact_sequence_sample_matches_evaluator() {
    let source = include_str!("../../hirsel-core-ir/samples/sequences.core");
    let module = text::parse(source).unwrap();
    let ordinary = sample_runs(&module, source);
    let mut cases: Vec<_> = ordinary.into_iter().map(|run| (run, 1_000_000)).collect();
    let mut count = 0;
    for line in source
        .lines()
        .filter(|l| l.trim().starts_with("; run limit "))
    {
        let (n, call) = line
            .trim()
            .strip_prefix("; run limit ")
            .unwrap()
            .split_once(": ")
            .unwrap();
        let runs = sample_runs(&module, &format!("; run: {call}"));
        let limit = n.parse().unwrap();
        count += runs.len();
        cases.extend(runs.into_iter().map(|run| (run, limit)));
    }
    assert_eq!(count, 7);
    let mut native: Vec<_> = cases
        .iter()
        .map(|(run, limit)| NativeCase {
            function: run.0,
            args: &run.1,
            limit: *limit,
            injection: None,
            report: None,
        })
        .collect();
    for (run, limit) in &cases {
        let baseline = eval::run(&module, run.0, &run.1, *limit);
        for n in 1..=baseline.admissions {
            for later in [false, true] {
                native.push(NativeCase {
                    function: run.0,
                    args: &run.1,
                    limit: *limit,
                    injection: Some((n, later)),
                    report: None,
                });
            }
        }
    }
    execute_cases(&module, false, &native);
}

#[test]
fn fixed_span_payload_pointers_and_scalar_reads_match_evaluator() {
    let module = text::parse(include_str!("support/spans.core")).unwrap();
    execute(
        &module,
        &[
            (FuncId(0), vec![0]),
            (FuncId(0), vec![1]),
            (FuncId(0), vec![257]),
        ],
        false,
    );
}

#[test]
fn owned_callable_captures_and_captureless_cleanup_use_function_row_abi() {
    let module = text::parse(include_str!("support/callable.core")).unwrap();
    let runs = [(FuncId(0), vec![12345]), (FuncId(2), vec![])];
    let reports = [
        eval::Report {
            outcome: Outcome::Returned(vec![12345]),
            used: 0,
            peak_reserved: 0, // This independent fixture specifies outcomes/ledger, not its high water.
            trace: vec![],
            admissions: 1,
            stdout: vec![],
            stderr: vec![],
            sites: vec![],
            events: vec![],
        },
        eval::Report {
            outcome: Outcome::Returned(vec![13]),
            used: 0,
            peak_reserved: 0, // This independent fixture specifies outcomes/ledger, not its high water.
            trace: vec![],
            admissions: 0,
            stdout: vec![],
            stderr: vec![],
            sites: vec![],
            events: vec![],
        },
    ];
    execute_reports(&module, &runs, false, 1000, None, Some(&reports));
}

#[test]
fn failure_message_moves_its_carrier_out_of_the_native_frame() {
    let module = text::parse(include_str!("support/message.core")).unwrap();
    execute(&module, &[(FuncId(0), vec![])], false);
}

#[test]
fn direct_ssa_handles_block_order_and_repeated_address_snapshots() {
    let module = text::parse(include_str!("support/ssa.core")).unwrap();
    execute(
        &module,
        &[
            (FuncId(0), vec![19]),
            (FuncId(1), vec![1, 11, 22]),
            (FuncId(1), vec![0, 11, 22]),
        ],
        false,
    );
}

#[test]
fn nested_counted_sequences_clean_refused_prefixes_and_deep_chains() {
    let module = text::parse(include_str!("support/counted-sequences.core")).unwrap();
    execute(
        &module,
        &[(FuncId(0), vec![0]), (FuncId(0), vec![8])],
        false,
    );
    // Every admission in a two-link construction, including the root.
    for n in 1..=7 {
        for later in [false, true] {
            execute_injected(
                &module,
                &[(FuncId(0), vec![2])],
                false,
                1000,
                Some((n, later)),
            );
        }
    }
    let n = 50_000;
    execute_reports(
        &module,
        &[(FuncId(0), vec![n])],
        false,
        10_000_000,
        None,
        Some(&[eval::Report {
            outcome: Outcome::Returned(vec![n]),
            used: 0,
            peak_reserved: 0, // This independent fixture specifies outcomes/ledger, not its high water.
            trace: vec![],
            admissions: 1 + 3 * n as usize,
            stdout: vec![],
            stderr: vec![],
            sites: vec![],
            events: vec![],
        }]),
    );
}

#[test]
fn maps_clean_live_entries_in_order_and_continue_after_finalizer_failure() {
    // The evaluator does not model Map entries. This independent C producer
    // writes FORM's frozen layout, including poisoned removed entries.
    let module = text::parse(
        r#"core v1
type t0 = resource "R" drop f2 { id: u64 }
type t1 = own "Resources" { entries: map t0 t0 }
type t2 = own "Leaf" { value: u64 }
type t3 = own "CountedMap" { entries: map rc t2 arc t2 }
sig g0 = (ptr) -> ()
sig g1 = (ptr) -> ()
inst n0 = test@0/maps/ordinary [] []
inst n1 = test@0/maps/counted [] []
inst n2 = test@0/maps/finalizer [] []
fn f0 = n0 g0 {
  addr a0 = v0 : t1
b0(v0: ptr):
  address a0
  drop a0 t1
  return
}
fn f1 = n1 g0 {
b0(v0: ptr):
  release v0 rc t3
  return
}
fn f2 = n2 g1 {
b0(v0: ptr):
  raise unwrap
}
"#,
    )
    .unwrap();
    let scratch = NativeArtifacts::new();
    let layouts = hirsel_core_layout::compute(&module.types).unwrap();
    let object = emit_object_for_test(
        &module,
        &layouts,
        &Options {
            target: "x86_64-unknown-linux-gnu",
            optimize: false,
            debug_lines: false,
        },
        hirsel_core_ids::ContentHash([0x90; 32]),
    )
    .unwrap();
    fs::write(scratch.path().join("input.o"), object.bytes).unwrap();
    let source = format!(
        r#"
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
{header}
extern HirselCoreContext *hirsel_core_test_context_new(uint64_t);
extern void hirsel_core_test_inject(HirselCoreContext *,size_t,bool);
extern uint64_t hirsel_core_test_available(const HirselCoreContext *);
extern size_t hirsel_core_test_attempts(const HirselCoreContext *);
extern uint8_t {ordinary}(void *, void *,uint64_t);
extern uint8_t {counted}(void *, void *,uint64_t);
static uint64_t trace[8]; static size_t traces;
void hirsel_core_test_drop_trace(uint64_t id) {{ if(traces==8) abort(); trace[traces++]=id; }}
static void *allocate(HirselCoreContext *ctx, size_t size, uint64_t charge, uint8_t kind) {{
  void *p=hirsel_core_alloc(ctx,0,size,8,charge,false,kind).pointer; if(!p) abort(); return p;
}}
static void *resource(HirselCoreContext *ctx, uint64_t id) {{
  uint64_t *p=hirsel_core_resource_alloc(ctx,8,8,8,0).pointer; if(!p) abort(); *p=id; return p;
}}
static void *leaf(HirselCoreContext *ctx, bool atomic) {{
  uint64_t *p=allocate(ctx,16,16,HIRSEL_CORE_NATIVE_PLAIN); hirsel_core_counted_init(p,atomic); p[1]=7; return p;
}}
int main(void) {{
  HirselCoreContext *ctx=hirsel_core_test_context_new(10000);
  uint64_t *entries=allocate(ctx,4*32+8,128,HIRSEL_CORE_NATIVE_GRAPH);
  memset(entries,0xcc,4*32+8);
  for(size_t i=0;i<3;i+=2) {{ entries[4*i+1]=1; entries[4*i+2]=(uintptr_t)resource(ctx,10*i+1); entries[4*i+3]=(uintptr_t)resource(ctx,10*i+2); }}
  entries[5]=0;
  uint64_t carrier[]={{(uintptr_t)entries,3,4}};
  hirsel_core_test_inject(ctx,1,true);
  size_t attempts=hirsel_core_test_attempts(ctx);
  if({ordinary}(ctx,carrier,0)!=1 || traces!=4 || trace[0]!=1 || trace[1]!=2 || trace[2]!=21 || trace[3]!=22) return 1;
  if(hirsel_core_test_attempts(ctx)!=attempts) return 2;
  HirselCoreCauseCell *causes=hirsel_core_causes_take(ctx);
  size_t n=0; for(const HirselCoreCauseCell *c=causes;c;c=hirsel_core_cause_next(c)) {{ if(hirsel_core_cause_kind(hirsel_core_cause_value(c))!=3) return 3; n++; }}
  if(n!=4 || hirsel_core_test_available(ctx)!=10000-128) return 4;
  hirsel_core_causes_drop(causes);
  if(hirsel_core_test_available(ctx)!=10000) return 5;
  hirsel_core_context_free(ctx);

  ctx=hirsel_core_test_context_new(10000);
  entries=allocate(ctx,4*32+8,128,HIRSEL_CORE_NATIVE_GRAPH);
  memset(entries,0xcc,4*32+8);
  for(size_t i=0;i<3;i+=2) {{ entries[4*i+1]=1; entries[4*i+2]=(uintptr_t)leaf(ctx,false); entries[4*i+3]=(uintptr_t)leaf(ctx,true); }}
  entries[5]=0;
  /* Count, carrier and one spare frame word: the map keeps pointer and length. */
  uint64_t *root=allocate(ctx,40,32,HIRSEL_CORE_NATIVE_PLAIN); hirsel_core_counted_init(root,false);
  root[1]=(uintptr_t)entries; root[2]=3; root[3]=4;
  hirsel_core_test_inject(ctx,1,true); attempts=hirsel_core_test_attempts(ctx);
  if({counted}(ctx,root,0)!=0 || hirsel_core_test_available(ctx)!=10000 || hirsel_core_test_attempts(ctx)!=attempts) return 6;
  hirsel_core_context_free(ctx);
  return 0;
}}
"#,
        header = include_str!("../../hirsel-core-rt/include/hirsel_core_rt.h"),
        ordinary = module.identity(hirsel_core_ir::InstanceId(0)).symbol(),
        counted = module.identity(hirsel_core_ir::InstanceId(1)).symbol()
    );
    assert!(link_run(&scratch, &source).is_empty());
}

#[test]
fn cancellation_runtime_status_takes_cleanup_or_propagates() {
    let module = text::parse(
        r#"core v1
sig g0 = () -> ()
sig g1 = () -> (u64)
inst n0 = test@0/cancel/cleanup [] []
inst n1 = test@0/cancel/propagate [] []
fn f0 = n0 g1 {
b0:
  call rt.check_cancel g0 () fail b1
  v0: u64 = const 3
  return v0
b1:
  v1: u64 = const 7
  return v1
}
fn f1 = n1 g1 {
b0:
  call rt.check_cancel g0 ()
  v0: u64 = const 9
  return v0
}
"#,
    )
    .unwrap();
    let scratch = NativeArtifacts::new();
    let layouts = hirsel_core_layout::compute(&module.types).unwrap();
    let object = emit_object_with_layouts(
        &module,
        &layouts,
        &Options {
            target: "x86_64-unknown-linux-gnu",
            optimize: false,
            debug_lines: false,
        },
        hirsel_core_ids::ContentHash([0x90; 32]),
    )
    .unwrap();
    fs::write(scratch.path().join("input.o"), object.bytes).unwrap();
    // Script just the C row to exercise both outcomes deterministically. The
    // real scheduler owns cancellation requests and has its own runtime tests.
    let source = format!(
        r#"
#include <stdint.h>
#include <stdlib.h>
static uint8_t cancelled; static uint64_t calls;
uint8_t hirsel_core_check_cancel(void *ctx, uint64_t site) {{
  if(ctx!=(void*)16 || site!=0) abort(); calls++; return cancelled;
}}
struct R {{ uint64_t value; uint8_t status; }};
extern struct R {cleanup}(void *,uint64_t);
extern struct R {propagate}(void *,uint64_t);
int main(void) {{
  struct R r={cleanup}((void*)16,0); if(r.status || r.value!=3) return 1;
  r={propagate}((void*)16,0); if(r.status || r.value!=9) return 2;
  cancelled=1;
  r={cleanup}((void*)16,0); if(r.status || r.value!=7) return 3;
  r={propagate}((void*)16,0); if(r.status!=1 || calls!=4) return 4;
  return 0;
}}
"#,
        cleanup = module.identity(hirsel_core_ir::InstanceId(0)).symbol(),
        propagate = module.identity(hirsel_core_ir::InstanceId(1)).symbol()
    );
    assert!(link_run(&scratch, &source).is_empty());
}

#[test]
fn cancelled_child_inside_isolate_passes_through_and_refunds_cell() {
    let mut source = include_str!("../../hirsel-core-ir/samples/custody.core").to_owned();
    source = source.replace(
        "sig g12 = (ptr) -> (ptr)",
        "sig g12 = (ptr) -> (ptr)\nsig g13 = () -> (bool, u64)\nsig g14 = () -> ()\nsig g15 = () -> (ptr)\nsig g16 = (ptr) -> ()",
    );
    let child_start = source.find("fn f1 = n1 g1 {").unwrap();
    let child_end = source[child_start..].find("fn f2 = n2 g5 {").unwrap() + child_start;
    let child = r#"fn f1 = n1 g1 {
  addr a0 = v2 : t1
b0(v0: ptr, v1: ptr, v2: ptr):
  address a0
  v3: bool, v4: u64 = call rt.isolate_enter g13 ()
  branch v3, b1, b2
b1:
  call rt.check_cancel g4 () fail b3
  call rt.isolate_leave g14 ()
  drop a0 t1
  return
b2:
  drop a0 t1
  return
b3:
  v5: ptr = call rt.isolate_catch g15 ()
  drop a0 t1
  propagate
}
"#;
    source.replace_range(child_start..child_end, child);
    source = source.replace(
        "  v4: ptr, v5: u64 = call rt.spawn[t1, t2] g2 (v1, &a1, v3, v1)",
        "  v4: ptr, v5: u64 = call rt.spawn[t1, t2] g2 (v1, &a1, v3, v1)\n  call rt.cancel g16 (v4)",
    );
    let module = text::parse(&source).unwrap();
    let scratch = NativeArtifacts::new();
    let layouts = hirsel_core_layout::compute(&module.types).unwrap();
    let object = emit_object_with_layouts(
        &module,
        &layouts,
        &Options {
            target: "x86_64-unknown-linux-gnu",
            optimize: false,
            debug_lines: false,
        },
        hirsel_core_ids::ContentHash([0x90; 32]),
    )
    .unwrap();
    fs::write(scratch.path().join("input.o"), object.bytes).unwrap();
    let source = format!(
        r#"
#define HIRSEL_TEST_WRAP_SPAWN
#include <stdint.h>
#include <stdlib.h>
#include "hirsel_core_rt.h"
HirselCoreSpawn __real_hirsel_core_spawn(HirselCoreContext *,uint64_t,uint64_t,void *,HirselCoreTaskBody,HirselCoreTaskLayout,uint64_t);
HirselCoreSpawn __wrap_hirsel_core_spawn(HirselCoreContext *ctx,uint64_t tasks,uint64_t bytes,void *input,HirselCoreTaskBody body,HirselCoreTaskLayout layout,uint64_t site) {{
  HirselCoreSpawn spawn=__real_hirsel_core_spawn(ctx,tasks,bytes,input,body,layout,site);
  if(spawn.task) hirsel_core_cancel(ctx,spawn.task);
  return spawn;
}}
struct R {{ uint64_t status, value, available, native_status; }};
extern struct R {body}(void *,uint64_t,uint64_t,uint64_t);
extern HirselCoreContext *hirsel_core_test_context_new(uint64_t);
extern uint64_t hirsel_core_test_available(const HirselCoreContext *);
extern uint8_t hirsel_core_test_run(HirselCoreContext *,HirselCoreTaskBody,void *,void *,size_t,size_t);
static uint8_t entry(HirselCoreContext *ctx,void *out,void *input) {{ *(struct R *)out={body}(ctx,7,1000,0); return 0; }}
int main(void) {{
  HirselCoreContext *ctx=hirsel_core_test_context_new(UINT64_C(1000000));
  uint64_t before=hirsel_core_test_available(ctx);
  struct R result={{0}};
  if(hirsel_core_test_run(ctx,entry,0,&result,0,sizeof result)) return 5;
  if(result.native_status || result.status!=2) return 1;
  /* The input Tok (40) is the only non-isolate charge left after the child is
     joined; its Cancelled cause cell is refunded with the child context. */
  if(hirsel_core_test_available(ctx)!=before-40) return 2;
  hirsel_core_context_free(ctx);
  return 0;
}}
"#,
        body = module.identity(hirsel_core_ir::InstanceId(2)).symbol(),
    );
    assert!(link_run(&scratch, &source).is_empty());
}

#[test]
fn every_custody_sample_and_injected_admission_matches_evaluator() {
    check_sample_with_refusals(
        include_str!("../../hirsel-core-ir/samples/custody.core"),
        false,
    );
}

#[test]
fn region_zero_charge_entry_is_not_reused_after_payer_retirement() {
    let module = text::parse(include_str!("support/region-payer.core")).unwrap();
    let run = [(FuncId(0), vec![])];
    execute_limit(&module, &run, false, 1000);
    for n in 1..=4 {
        for later in [false, true] {
            execute_injected(&module, &run, false, 1000, Some((n, later)));
        }
    }
}

#[test]
fn empty_bound_preserves_room_and_every_injected_reservation() {
    let module = text::parse(
        r#"core v1
sig g0 = (u64, u64) -> (u64, u64)
inst n0 = test@0/bound/empty [] []
fn f0 = n0 g0 {
b0(v0: u64, v1: u64):
  v2: u64 = const 0
  v3: u64 = const 1
  jump b1(v2)
b1(v4: u64):
  v5: bool = lt v4, v0
  branch v5, b2, b4
b2:
  v6: bool, v7: u64 = limit_enter v1
  branch v6, b3, b5
b3:
  limit_exit
  v8: u64 = wrapping_add v4, v3
  jump b1(v8)
b4:
  return v4, v2
b5:
  return v4, v7
}
"#,
    )
    .unwrap();
    for bytes in [0, 8, 2000] {
        let run = [(FuncId(0), vec![4, bytes])];
        execute_limit(&module, &run, false, 1000);
        let baseline = eval::run(&module, FuncId(0), &run[0].1, 1000);
        for n in 1..=baseline.admissions {
            for later in [false, true] {
                execute_injected(&module, &run, false, 1000, Some((n, later)));
            }
        }
    }
}

#[test]
fn inlined_arc_updates_keep_the_provider_event_order() {
    let module = text::parse(
        r#"core v1
 type t0 = own "Payload" { value: u64 }
 sig g0 = () -> (bool)
 inst n0 = test@0/arc/events [] []
 fn f0 = n0 g0 {
   addr a0 = v2 byte v1 : u64
 b0:
   v0: u64 = const 16
   v1: u64 = const 8
   v2: ptr, v3: u64 = alloc for arc t0 v0 align 8 charge v0
   v4: ptr = const 0x0
   v5: bool = ne v2, v4
   branch v5, b1, b2
 b1:
   v6: u64 = const 7
   address a0
   store a0, v6
   share v2 arc
   v7: bool = try_unwrap v2 arc
   release v2 arc t0
   v8: bool = try_unwrap v2 arc
   release v2 arc t0
   return v8
 b2:
   raise unwrap
 }
"#,
    )
    .unwrap();
    let scratch = NativeArtifacts::new();
    let layouts = hirsel_core_layout::compute(&module.types).unwrap();
    let object = emit_object_with_layouts(
        &module,
        &layouts,
        &Options {
            target: "x86_64-unknown-linux-gnu",
            optimize: false,
            debug_lines: false,
        },
        hirsel_core_ids::ContentHash([0x90; 32]),
    )
    .unwrap();
    fs::write(scratch.path().join("input.o"), object.bytes).unwrap();
    let source = format!(
        r#"
#include <stdint.h>
#include <stdlib.h>
#include "hirsel_core_rt.h"
struct R {{ uint8_t value,status; }};
extern struct R {function}(void*,uint64_t);
extern HirselCoreContext *hirsel_core_test_context_new(uint64_t);
extern uint64_t hirsel_core_test_available(const HirselCoreContext*);
extern uint8_t hirsel_core_test_run_events(HirselCoreContext*,uint8_t (*)(HirselCoreContext*,void*,void*),void*,void*,size_t,size_t,HirselCoreSchedulerProvider);
static uint64_t kinds[6],values[6],count;
static void event(void *state,HirselCoreEvent e) {{
 if(e.kind>=HIRSEL_CORE_EVENT_ARC_NEW && e.kind<=HIRSEL_CORE_EVENT_ARC_UNWRAP) {{ if(count==6 || e.task!=0 || e.object!=0) abort(); kinds[count]=e.kind;values[count++]=e.value; }}
}}
static uint8_t body(HirselCoreContext *ctx,void *out,void *in) {{ *(struct R*)out={function}(ctx,0);return 0; }}
int main(void) {{
 HirselCoreContext *ctx=hirsel_core_test_context_new(1000);
 HirselCoreSchedulerProvider provider={{0,event,0}};struct R out;
 if(hirsel_core_test_run_events(ctx,body,0,&out,0,sizeof out,provider) || out.status || !out.value)return 2;
 if(count!=6 || kinds[0]!=HIRSEL_CORE_EVENT_ARC_NEW || values[0]!=1 || kinds[1]!=HIRSEL_CORE_EVENT_ARC_SHARE || values[1]!=2 || kinds[2]!=HIRSEL_CORE_EVENT_ARC_UNWRAP || values[2]!=0 || kinds[3]!=HIRSEL_CORE_EVENT_ARC_RELEASE || values[3]!=0 || kinds[4]!=HIRSEL_CORE_EVENT_ARC_UNWRAP || values[4]!=1 || kinds[5]!=HIRSEL_CORE_EVENT_ARC_RELEASE || values[5]!=1)return 3;
 if(hirsel_core_test_available(ctx)!=1000)return 4;
 hirsel_core_context_free(ctx);return 0;
}}
"#,
        function = module.identity(hirsel_core_ir::InstanceId(0)).symbol()
    );
    assert!(link_run(&scratch, &source).is_empty());
}

#[test]
fn inlined_count_increment_overflow_is_the_runtime_stop() {
    for kind in ["rc", "arc"] {
        let module = text::parse(&format!("core v1\nsig g0 = (ptr) -> ()\ninst n0 = test@0/count/overflow [] []\nfn f0 = n0 g0 {{\nb0(v0: ptr):\n  share v0 {kind}\n  return\n}}\n")).unwrap();
        let scratch = NativeArtifacts::new();
        let object = emit_object(
            &module,
            &Options {
                target: "x86_64-unknown-linux-gnu",
                optimize: false,
                debug_lines: false,
            },
            hirsel_core_ids::ContentHash([0x90; 32]),
        )
        .unwrap();
        fs::write(scratch.path().join("input.o"), object.bytes).unwrap();
        // Arc share reads the live context to choose its replay path, so the
        // caller passes one, as every instance's entry ABI requires.
        let name = module.identity(hirsel_core_ir::InstanceId(0)).symbol();
        let source = format!(
            "#include \"hirsel_core_rt.h\"\nextern uint8_t {name}(HirselCoreContext*,void*,uint64_t);\nint main(void) {{ HirselCoreContext *ctx=hirsel_core_context_new(1000); uint64_t node[2]={{UINT64_MAX,7}}; {name}(ctx,node,0); hirsel_core_context_free(ctx); return 0; }}\n"
        );
        let run = Command::new(link_program(&scratch, &source))
            .output()
            .unwrap();
        assert_eq!(run.status.code(), Some(102), "{kind}: {:?}", run.status);
        assert_eq!(run.stderr, b"run stop: CountOverflow\n", "{kind}");
    }
}

#[test]
fn static_capacity_zero_carriers_drop_and_grow_through_runtime_cleanup() {
    let source = include_str!("../../hirsel-core-ir/samples/strings.core");
    let module = text::parse(source).unwrap();
    execute(
        &module,
        &[
            (FuncId(0), vec![0]),
            (FuncId(0), vec![1]),
            (FuncId(1), vec![]),
        ],
        false,
    );
    execute_limit(&module, &[(FuncId(0), vec![1])], false, 4);
    let module = text::parse(include_str!("support/static-carriers.core")).unwrap();
    let cases: Vec<_> = (0..4)
        .flat_map(|f| [0, 5].map(move |n| (FuncId(f), vec![n])))
        .collect();
    let expected: Vec<_> = cases
        .iter()
        .map(|(f, _)| eval::Report {
            outcome: Outcome::Returned(vec![]),
            used: 0,
            peak_reserved: 0, // This independent fixture specifies outcomes/ledger, not its high water.
            trace: vec![],
            admissions: [0, 1, 1, 2][f.index()],
            stdout: vec![],
            stderr: vec![],
            sites: vec![],
            events: vec![],
        })
        .collect();
    execute_reports(&module, &cases, false, 1000, None, Some(&expected));
}

#[test]
fn joined_cause_views_use_core_layout() {
    let module = text::parse(include_str!("../../hirsel-core-ir/samples/custody.core")).unwrap();
    let expected = eval::Report {
        outcome: Outcome::Returned(vec![1, 1, 4, 4]),
        used: 0,
        peak_reserved: 0, // This independent fixture specifies outcomes/ledger, not its high water.
        stdout: vec![],
        stderr: vec![],
        sites: vec![],
        events: vec![],
        trace: vec![7],
        admissions: eval::run(&module, FuncId(6), &[], 1000).admissions,
    };
    execute_reports(
        &module,
        &[(FuncId(6), vec![])],
        false,
        1000,
        None,
        Some(&[expected]),
    );
}

#[test]
fn dynamic_overflow_and_local_like_admissions_keep_exact_refusal_counts() {
    let module = text::parse(include_str!("support/inline-admission.core")).unwrap();
    let runs: Vec<_> = (0..3)
        .flat_map(|function| {
            [0, 8, 40, 72, u64::MAX]
                .into_iter()
                .flat_map(move |charge| {
                    [0, 1].map(move |overflow| (FuncId(function), vec![charge, overflow]))
                })
        })
        .collect();
    let mut cases = Vec::new();
    for (function, args) in &runs {
        cases.push(NativeCase {
            function: *function,
            args,
            limit: 64,
            injection: None,
            report: None,
        });
        let baseline = eval::run(&module, *function, args, 64);
        for nth in 1..=baseline.admissions {
            for later in [false, true] {
                cases.push(NativeCase {
                    function: *function,
                    args,
                    limit: 64,
                    injection: Some((nth, later)),
                    report: None,
                });
            }
        }
    }
    execute_cases(&module, false, &cases);
    let mut raw = Vec::new();
    // The evaluator requires a real block for Like; the native ABI also accepts
    // null as Current. Derive this plain null-Like case directly from its budget.
    for charge in [0, 8, 40, 72, u64::MAX] {
        for overflow in [0, 1] {
            for injection in [None, Some((1, false)), Some((1, true))] {
                let success = overflow == 0 && charge <= 64 && injection.is_none();
                let expected = eval::Report {
                    outcome: Outcome::Returned(vec![
                        u64::from(success),
                        if success { 0 } else { 64 },
                    ]),
                    used: 0,
                    peak_reserved: 0, // This independent fixture specifies outcomes/ledger, not its high water.
                    trace: vec![],
                    admissions: 1,
                    stdout: vec![],
                    stderr: vec![],
                    sites: vec![],
                    events: vec![],
                };
                raw.push((vec![charge, overflow], injection, expected));
            }
        }
    }
    let cases: Vec<_> = raw
        .iter()
        .map(|(args, injection, report)| NativeCase {
            function: FuncId(3),
            args,
            limit: 64,
            injection: *injection,
            report: Some(report),
        })
        .collect();
    execute_cases(&module, false, &cases);
}

#[test]
fn small_unaligned_copies_preserve_every_byte_and_both_canaries() {
    check_unaligned_copies();
}

#[test]
fn ordinary_allocator_charge_edges_and_alignment_match_evaluator() {
    let mut source = String::from("core v1\nsig g0 = (u64) -> (bool, u64)\n");
    for f in 0..3 {
        writeln!(source, "inst n{f} = test@0/ordinary/align{f} [] []").unwrap();
    }
    for (f, align) in [8, 16, 64].into_iter().enumerate() {
        writeln!(
            source,
            "fn f{f} = n{f} g0 {{
b0(v0: u64):
  v1: u64 = const 16
  v2: ptr, v3: u64 = alloc for seq u8 v1 align {align} charge v0
  v4: ptr = const 0x0
  v5: bool = ne v2, v4
  branch v5, b1, b2
b1:
  free v2 charge v0
  return v5, v3
b2:
  return v5, v3
}}"
        )
        .unwrap();
    }
    let module = text::parse(&source).unwrap();
    wf::module(&module).unwrap();
    let runs: Vec<_> = (0..3)
        .flat_map(|f| [0, 1, 8, u64::MAX].map(move |charge| (FuncId(f), vec![charge])))
        .collect();
    let mut cases = Vec::new();
    for limit in [0, 1, 8, u64::MAX] {
        for (function, args) in &runs {
            cases.push(NativeCase {
                function: *function,
                args,
                limit,
                injection: None,
                report: None,
            });
            let baseline = eval::run(&module, *function, args, limit);
            for nth in 1..=baseline.admissions {
                for later in [false, true] {
                    cases.push(NativeCase {
                        function: *function,
                        args,
                        limit,
                        injection: Some((nth, later)),
                        report: None,
                    });
                }
            }
        }
    }
    for optimize in [false, true] {
        execute_cases(&module, optimize, &cases);
    }
}

#[test]
fn byte_fill_ranges_keep_canaries_counter_and_last_address_snapshot() {
    let module = text::parse(
        r#"core v1
type t0 = own "Buffer" { data: array 35 u8 }
sig g0 = (u64, u64, u8) -> (u64, u64, u8, u8, u8)
inst n0 = test@0/fill/range [] []
fn f0 = n0 g0 {
  slot s0 = t0
  addr a0 = s0 byte v6 : u8
  addr a1 = s0 byte v3 : u8
  addr a2 = v9 byte v12 : u8
  addr a3 = s0 byte v16 : u8
  addr a4 = s0 byte v4 : u8
  addr a5 = s0 byte v5 : u8
b0(v0: u64, v1: u64, v2: u8):
  live s0
  v3: u64 = const 0
  v4: u64 = const 1
  v5: u64 = const 34
  v7: u64 = const 35
  v8: u8 = const 0
  v14: u64 = const 0
  v15: u64 = const 0
  jump b1(v3)
b1(v6: u64):
  v18: bool = lt v6, v7
  branch v18, b2, b3
b2:
  address a0
  store a0, v8
  v19: u64 = wrapping_add v6, v4
  jump b1(v19)
b3:
  address a1
  address a5
  v20: u8 = const 97
  v21: u8 = const 101
  store a1, v20
  store a5, v21
  address a4
  v9: ptr = pointer a4
  v22: bool = lt v0, v1
  branch v22, b6, b8
b4(v10: u64):
  v12: u64 = wrapping_mul v10, v4
  address a2
  store a2, v2
  v13: u64 = wrapping_add v10, v4
  v11: bool = lt v13, v1
  branch v11, b5, b7
b5:
  jump b4(v13)
b6:
  jump b4(v0)
b7:
  v23: u8 = read a2
  jump b9(v23, v13)
b8:
  jump b9(v8, v0)
b9(v24: u8, v34: u64):
  jump b10(v3, v3)
b10(v16: u64, v17: u64):
  v25: bool = lt v16, v7
  branch v25, b11, b12
b11:
  address a3
  v26: u8 = read a3
  v27: u64 = widen v26
  v28: u64 = const 257
  v29: u64 = wrapping_mul v17, v28
  v30: u64 = wrapping_add v29, v27
  v31: u64 = wrapping_add v16, v4
  jump b10(v31, v30)
b12:
  address a1
  address a5
  v32: u8 = read a1
  v33: u8 = read a5
  dead s0
  return v34, v17, v24, v32, v33
}
"#,
    )
    .unwrap();
    wf::module(&module).unwrap();
    let runs: Vec<_> = [0, 1, 3, 16, 32]
        .into_iter()
        .flat_map(|start| {
            [0, 1, 3, 16, 32].into_iter().flat_map(move |end| {
                [0, 7, 255].map(move |byte| (FuncId(0), vec![start, end, byte]))
            })
        })
        .collect();
    for optimize in [false, true] {
        execute(&module, &runs, optimize);
    }
    // Similar loops must keep their ordinary stores: non-unit stride, an
    // inclusive bound, and an extra observable store are different operations.
    let source = text::print(&module);
    let non_unit = source
        .replace("wrapping_mul v10, v4", "wrapping_mul v10, v14")
        .replace("v14: u64 = const 0", "v14: u64 = const 2");
    let inclusive = source.replace("lt v10, v1", "le v10, v1");
    let effectful = source.replace("store a2, v2", "store a2, v2\n  store a5, v2");
    let short: Vec<_> = [0, 1, 3]
        .into_iter()
        .flat_map(|start| {
            [0, 1, 3].into_iter().flat_map(move |end| {
                [0, 7, 255].map(move |byte| (FuncId(0), vec![start, end, byte]))
            })
        })
        .collect();
    for source in [non_unit, inclusive, effectful] {
        let module = text::parse(&source).unwrap();
        wf::module(&module).unwrap();
        for optimize in [false, true] {
            execute(&module, &short, optimize);
        }
    }
}

#[test]
fn isolate_sample_in_dev_and_speed_matches_evaluator() {
    for optimize in [false, true] {
        check_sample_with_refusals(
            include_str!("../../hirsel-core-ir/samples/isolate.core"),
            optimize,
        );
    }
}

#[test]
fn relative_debug_rows_and_distinct_cold_sites() {
    check_relative_debug_rows();
}

#[test]
fn consuming_growth_keeps_the_carrier_and_original_payer_on_refusal() {
    for optimize in [false, true] {
        check_sample_with_refusals(
            include_str!("../../hirsel-core-rt/testdata/grow.core"),
            optimize,
        );
    }
}

#[test]
fn inherited_failure_sites_through_core_and_generated_calls() {
    check_failure_inheritance();
}
