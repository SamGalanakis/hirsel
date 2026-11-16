//! Source LLVM agreement uses independently checked definition units.
#![allow(dead_code)]
use hirsel_core_ir::eval;
use hirsel_core_ir::*;
use hirsel_core_layout::Target;
use hirsel_llvm::core::symbol;
use hirsel_llvm::core::*;
use std::{
    fmt::Write as _,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
hirsel_guard::install!();
include!("../../hirsel-llvm/tests/support/native.rs");
impl NativeInput for hirsel_core_program_tests::SourceProgram {
    fn module(&self) -> &Module {
        self
    }
    fn object(&self, target: Target, optimization: Optimization, observing: bool) -> Vec<u8> {
        self.llvm_object(
            target,
            matches!(optimization, Optimization::Speed),
            false,
            false,
            observing,
        )
    }
    fn site_bindings(&self) -> String {
        self.site_bindings()
    }
    fn verify(&self, target: Target, observing: bool) {
        for module in &self.modules {
            verify_ir(&emit_ir_for_test(module, target, observing, false, None).unwrap());
        }
    }
}
fn verify_ir(ir: &str) {
    use llvm_sys::{
        analysis::{LLVMVerifierFailureAction, LLVMVerifyModule},
        core::*,
        ir_reader::LLVMParseIRInContext2,
    };
    use std::{ffi::CStr, ptr};
    // SAFETY: all LLVM objects are owned in this scope; parse consumes the copied buffer.
    unsafe {
        let context = LLVMContextCreate();
        let buffer = LLVMCreateMemoryBufferWithMemoryRangeCopy(
            ir.as_ptr().cast(),
            ir.len(),
            c"test".as_ptr(),
        );
        let mut module = ptr::null_mut();
        let mut error = ptr::null_mut();
        let parsed = LLVMParseIRInContext2(context, buffer, &mut module, &mut error);
        let valid = parsed == 0
            && LLVMVerifyModule(
                module,
                LLVMVerifierFailureAction::LLVMReturnStatusAction,
                &mut error,
            ) == 0;
        let message = if error.is_null() {
            String::new()
        } else {
            let s = CStr::from_ptr(error).to_string_lossy().into_owned();
            LLVMDisposeMessage(error);
            s
        };
        if !module.is_null() {
            LLVMDisposeModule(module);
        }
        LLVMContextDispose(context);
        assert!(valid, "{message}\n{ir}");
    }
}
#[test]
fn every_front_run_case_matches_the_evaluator() {
    fn files(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                files(&path, out)
            } else if path.extension().is_some_and(|s| s == "hirsel") {
                out.push(path)
            }
        }
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../core/front");
    let mut paths = vec![];
    files(&root.join("cases"), &mut paths);
    paths.sort();
    let mut count = 0;
    let mut failures = Vec::new();
    for path in paths {
        let source = std::fs::read_to_string(&path).unwrap();
        let runs = source
            .lines()
            .filter_map(|line| line.trim().strip_prefix("// expect: run "))
            .collect::<Vec<_>>();
        if runs.is_empty() {
            continue;
        }
        let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let (module, names) = Ok::<_, String>(hirsel_core_program_tests::SourceProgram::lower(
                &source,
                Some("main"),
            ))
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            // A run marked `evaluator-only:..llvm..` (SITES.md, NOT-BUILT) must still
            // disagree here, so the marker cannot outlive the missing emission.
            let pending = |run: &str| {
                run.split_whitespace().last().is_some_and(|m| {
                    m.strip_prefix("evaluator-only:")
                        .is_some_and(|e| e.split(',').any(|e| e == "llvm"))
                })
            };
            let case = |run: &&str| {
                let (call, _) = run.split_once(" = ").unwrap();
                let (name, args) = call.strip_suffix(')').unwrap().split_once('(').unwrap();
                let function = names[name];
                let sig = &module.signatures[module.functions[function.index()].signature.index()];
                let args = args
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .zip(&sig.params)
                    .map(|(a, t)| {
                        let Param::Value(t) = t else {
                            panic!("front argument")
                        };
                        eval::scalar(*t, a)
                    })
                    .collect();
                Case {
                    function,
                    args,
                    limit: u64::MAX,
                    injection: None,
                }
            };
            let cases = runs
                .iter()
                .filter(|r| !pending(r))
                .map(case)
                .collect::<Vec<_>>();
            println!("front {}: {} native runs", path.display(), cases.len());
            count += cases.len();
            agree_cases(&module, &cases, false);
            let marked = runs
                .iter()
                .filter(|r| pending(r))
                .copied()
                .collect::<Vec<_>>();
            let held = marked.iter().map(case).collect::<Vec<_>>();
            for ((run, case), got) in marked.iter().zip(&held).zip(execute(&module, &held)) {
                let expected = eval::run(&module, case.function, &case.args, case.limit);
                assert_ne!(
                    got.outcome, expected.outcome,
                    "`{run}` now agrees on LLVM: remove `llvm` from its marker and its NOT-BUILT row"
                );
            }
        }));
        if let Err(error) = attempt {
            let message = error
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| error.downcast_ref::<&str>().copied())
                .unwrap_or("non-string panic");
            failures.push(format!("{}: {message}", path.display()));
        }
    }
    assert!(count > 0);
    println!(
        "front native executions: {count}; file failures: {}",
        failures.len()
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

fn carrier_program_cases(target: Target) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../core/front");
    for name in [
        "carrier-twins",
        "cold-call-place",
        "causes-retry",
        "grow-carrier-words",
        "list-put-word",
        "list-nested-mutate",
        "own-20-mut-anchor",
    ] {
        let path = root.join(format!("cases/body/{name}.hirsel"));
        let source = std::fs::read_to_string(&path).unwrap();
        let (module, names) = Ok::<_, String>(hirsel_core_program_tests::SourceProgram::lower(
            &source,
            Some("main"),
        ))
        .unwrap();
        let cases = source
            .lines()
            .filter_map(|line| line.trim().strip_prefix("// expect: run "))
            .map(|run| {
                let (call, _) = run.split_once(" = ").unwrap();
                let (name, args) = call.strip_suffix(')').unwrap().split_once('(').unwrap();
                let function = names[name];
                let sig = &module.signatures[module.functions[function.index()].signature.index()];
                let args = args
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .zip(&sig.params)
                    .map(|(a, t)| {
                        let Param::Value(t) = t else {
                            panic!("front argument")
                        };
                        eval::scalar(*t, a)
                    })
                    .collect();
                Case {
                    function,
                    args,
                    limit: u64::MAX,
                    injection: None,
                }
            })
            .collect::<Vec<_>>();
        let object = module.llvm_program(
            target,
            false,
            false,
            &cases.iter().map(|c| c.function).collect::<Vec<_>>(),
        );
        for callee in source
            .lines()
            .filter_map(|line| line.trim().strip_prefix("// words: "))
            .flat_map(str::split_whitespace)
        {
            let twin = format!("{}.words", symbol(&module, names[callee]));
            assert!(
                object.windows(twin.len()).any(|w| w == twin.as_bytes()),
                "{name}: {callee} takes no word path"
            );
        }
        let reports = execute_target_object(
            &module,
            &cases,
            "",
            "",
            Optimization::Speed,
            Some(object),
            target,
        );
        for (case, report) in cases.iter().zip(reports) {
            let expected = eval::run(&module, case.function, &case.args, case.limit);
            let at = format!("{name}: f{}({:?})", case.function.0, case.args);
            assert_eq!(report.outcome, expected.outcome, "{at}");
            assert_eq!(report.used, expected.used, "{at}: bytes still admitted");
        }
    }
}

fn cancelled_cold_word_call(target: Target, with_isolate: bool) {
    let source = r#"
fn grow_then_check(mut xs: List[u64]) {
    for n in 0u64 .. 100 {
        result.unwrap(list.push(mut xs, n))
    }
    task.check_cancel()
}

fn isolated_body() {
    let mut xs: List[u64] = list.new()
    if list.len(xs) == list.capacity(xs) {
        grow_then_check(mut xs)
    }
}

fn child(take x: u64) -> u64 {
    CALL_BODY
    x
}

fn cancelled(tasks: Tasks) -> u64 waits {
    let mut h = result.unwrap(task.spawn(tasks, bytes 65536, 41u64, child))
    task.cancel(mut h)
    match task.join(take h) {
        Ok(n) => n,
        Error(Cancelled(take c)) => {
            let _ = take c
            1
        }
        Error(Failed(take c)) => {
            let _ = take c
            2
        }
    }
}
"#;
    let source = if with_isolate {
        source.replace(
            "CALL_BODY",
            "let _ = isolate(fn() -> () { isolated_body() })",
        )
    } else {
        source.replace("CALL_BODY", "isolated_body()")
    };
    let (module, names) = Ok::<_, String>(hirsel_core_program_tests::SourceProgram::lower(
        &source,
        Some("main"),
    ))
    .unwrap();
    let object = module.llvm_program(target, false, false, &[names["cancelled"]]);
    let twin = format!("{}.words", symbol(&module, names["grow_then_check"]));
    assert!(
        object.windows(twin.len()).any(|w| w == twin.as_bytes()),
        "no {twin}"
    );
    let case = Case {
        function: names["cancelled"],
        args: vec![0],
        limit: u64::MAX,
        injection: None,
    };
    let expected = eval::run(&module, case.function, &case.args, case.limit);
    assert_eq!(expected.used, 0, "evaluator used bytes");
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
    assert_eq!(report.outcome, eval::Outcome::Returned(vec![1]));
    println!(
        "cancelled cold word call target={target:?} isolate={with_isolate}: used={} evaluator_used={}",
        report.used, expected.used
    );
    assert_eq!(report.used, expected.used);
}

fn site_fixture(target: Target) {
    use object::{Object as _, ObjectSection as _, ObjectSymbol as _};
    let source = format!(
        "{}\nfn debug_identity(n: u64) -> u64 {{ n ^ 7 }}\nfn debug_resource(n: u64) -> R {{ match R(d: n) {{ Ok(take r) => take r, Error(_) => loop {{}} }} }}\nfn debug_resource_caller(n: u64) -> u64 {{ let r = debug_resource(n); r.d }}\n",
        include_str!("../../../core/front/cases/body/failure-sites.hirsel")
    );
    let (module, names) = Ok::<_, String>(hirsel_core_program_tests::SourceProgram::lower(
        &source,
        Some("main"),
    ))
    .unwrap();
    let cases = [
        ("add", u64::MAX),
        ("at", 3),
        ("raise", 3),
        ("relay", 3),
        ("twice", u64::MAX),
        ("first", 0),
        ("finalize", 0),
        ("both", 0),
        ("sites", 1),
        ("debug_identity", 7),
        ("debug_resource_caller", 0),
        ("debug_resource_caller", 1),
    ]
    .map(|(name, arg)| Case {
        function: names[name],
        args: vec![arg],
        limit: u64::MAX,
        injection: None,
    });
    let mut bindings = String::new();
    for (i, item) in module.items.iter().enumerate() {
        writeln!(
            bindings,
            "__asm__(\".globl {0}\\n.hidden {0}\\n.set {0}, {1}\\n\");",
            hirsel_core_ir::source::site_symbol(item),
            (i as u64 + 1) << 32
        )
        .unwrap();
    }
    for (opt, composed, debug_lines) in [
        (Optimization::None, false, true),
        (Optimization::Speed, false, true),
        (Optimization::Speed, true, true),
        (Optimization::Speed, false, false),
    ] {
        let bytes = if composed {
            module.llvm_program(
                target,
                debug_lines,
                false,
                &cases.iter().map(|c| c.function).collect::<Vec<_>>(),
            )
        } else {
            module.llvm_eager_object(target, opt, debug_lines)
        };
        let file = object::File::parse(bytes.as_slice()).unwrap();
        let mut site_relocations = 0;
        for section in file.sections() {
            for (_, relocation) in section.relocations() {
                if let object::RelocationTarget::Symbol(index) = relocation.target()
                    && file
                        .symbol_by_index(index)
                        .unwrap()
                        .name()
                        .unwrap()
                        .starts_with("hirsel_core_site_")
                {
                    assert_eq!(
                        relocation.kind(),
                        object::RelocationKind::Absolute,
                        "{target:?} {} {relocation:?}",
                        section.name().unwrap()
                    );
                    assert_eq!(relocation.size(), 64);
                    assert!(relocation.addend() >= 0);
                    site_relocations += 1;
                }
            }
        }
        assert!(site_relocations > 0);
        if !debug_lines {
            assert!(
                file.section_by_name(".debug_line").is_none(),
                "default release object contains line rows"
            );
            assert!(
                file.section_by_name(".debug_info").is_none(),
                "default release object contains debug scopes"
            );
        }
        if debug_lines {
            for name in [".debug_info", ".debug_line"] {
                assert!(
                    !file
                        .section_by_name(name)
                        .unwrap()
                        .data()
                        .unwrap()
                        .is_empty(),
                    "{name}"
                );
            }
            assert_relative_debug_rows(&bytes, &module);
            let debug_strings = file.section_by_name(".debug_str").unwrap().data().unwrap();
            let debug_info = file.section_by_name(".debug_info").unwrap().data().unwrap();
            let has_subprogram = |name: &str| {
                let linkage = format!("{name}\0");
                [debug_strings, debug_info]
                    .iter()
                    .any(|data| data.windows(linkage.len()).any(|s| s == linkage.as_bytes()))
            };
            let source_functions = module
                .functions
                .iter()
                .enumerate()
                .filter(|(_, f)| {
                    f.body.as_ref().is_some_and(|b| {
                        b.inst_anchors.iter().any(Option::is_some)
                            || b.blocks.iter().any(|b| b.anchor.is_some())
                    })
                })
                .map(|(i, _)| symbol(&module, FuncId(i as u32)))
                .collect::<Vec<_>>();
            for function in file.symbols().filter(|s| {
                s.kind() == object::SymbolKind::Text && s.size() != 0 && !s.is_undefined()
            }) {
                let name = function.name().unwrap();
                if !source_functions
                    .iter()
                    .any(|prefix| name == prefix || name.starts_with(&format!("{prefix}.")))
                {
                    continue;
                }
                let section = file
                    .section_by_index(function.section_index().unwrap())
                    .unwrap();
                let code = &section.data().unwrap()
                    [function.address() as usize..(function.address() + function.size()) as usize];
                // MergeFunctions retains public addresses through generated jump
                // thunks. The destination's real source body owns their debug rows.
                let branch = match target {
                    Target::X86_64Linux => code.len() == 5 && code[0] == 0xe9,
                    Target::Aarch64Linux => {
                        code.len() == 4
                            && u32::from_le_bytes(code.try_into().unwrap()) & 0xfc000000
                                == 0x14000000
                    }
                };
                let thunk = branch
                    && section.relocations().any(|(offset, r)| {
                        if !(function.address()..function.address() + function.size())
                            .contains(&offset)
                        {
                            return false;
                        }
                        let object::RelocationTarget::Symbol(target) = r.target() else {
                            return false;
                        };
                        let destination = file.symbol_by_index(target).unwrap();
                        destination.name().is_ok_and(|destination| {
                            destination != name && has_subprogram(destination)
                        })
                    });
                assert!(
                    has_subprogram(name) || thunk,
                    "source function has no actual-linkage subprogram: {name}, {opt:?}, composed={composed}"
                );
            }
            if matches!(opt, Optimization::Speed) {
                let strings = file.section_by_name(".debug_str").unwrap().data().unwrap();
                let symbol = format!("{}\0", symbol(&module, names["debug_identity"]));
                assert!(
                    strings
                        .windows(symbol.len())
                        .any(|bytes| bytes == symbol.as_bytes()),
                    "optimized infallible export has no subprogram: {opt:?} composed={composed}, anchors={:?}, strings={:?}",
                    module.functions[names["debug_identity"].index()]
                        .body
                        .as_ref()
                        .unwrap()
                        .inst_anchors,
                    strings
                        .split(|b| *b == 0)
                        .filter(|s| s.windows(14).any(|w| w == b"debug_identity"))
                        .map(|s| String::from_utf8_lossy(s))
                        .collect::<Vec<_>>()
                );
            }
            if !composed {
                // The eager unit harness includes drop-trace hooks. Compile exactly its
                // IR on the lazy bridge so the parity check has identical producer inputs.
                let lazy = module.llvm_object(
                    target,
                    matches!(opt, Optimization::Speed),
                    true,
                    false,
                    false,
                );
                let left = object::File::parse(bytes.as_slice()).unwrap();
                let right = object::File::parse(lazy.as_slice()).unwrap();
                let differences = left
                    .sections()
                    .filter_map(|section| {
                        let name = section.name().unwrap();
                        let a = section.data().unwrap();
                        let b = right
                            .section_by_name(name)
                            .map(|s| s.data().unwrap())
                            .unwrap_or(&[]);
                        (a != b).then(|| {
                            (
                                name.to_owned(),
                                a.len(),
                                b.len(),
                                a.iter().zip(b).position(|(a, b)| a != b),
                            )
                        })
                    })
                    .collect::<Vec<_>>();
                assert!(
                    bytes == lazy,
                    "eager/lazy {opt:?} object parity: {differences:?}"
                );
            }
        }
        let product = finish_native_object(
            bytes,
            hirsel_core_ids::ContentHash::of_bytes(b"executed native debug witness"),
            debug_lines,
        )
        .unwrap();
        assert_eq!(product.debug.is_some(), debug_lines);
        let reports = execute_target_object(
            &module,
            &cases,
            &bindings,
            "",
            opt,
            Some(product.bytes),
            target,
        );
        for (case, got) in cases.iter().zip(reports) {
            let expected = eval::run(&module, case.function, &case.args, u64::MAX);
            assert_eq!(got.outcome, expected.outcome);
            let actual = got
                .sites
                .iter()
                .map(|word| {
                    (*word != 0).then(|| {
                        (
                            module.items[((word >> 32) - 1) as usize].clone(),
                            *word as u32,
                        )
                    })
                })
                .collect::<Vec<_>>();
            let expected = expected
                .sites
                .iter()
                .map(|a| a.map(|a| (module.items[a.item.index()].clone(), a.start)))
                .collect::<Vec<_>>();
            assert_eq!(
                actual, expected,
                "resolved cause sites, function {}",
                case.function.0
            );
        }
    }
    println!(
        "native failure-site observations: {} ({} with rows, {} default release without rows)",
        cases.len() * 4,
        cases.len() * 3,
        cases.len()
    );
}

#[test]
fn failure_sites_and_relative_debug_rows_match_the_evaluator() {
    site_fixture(Target::X86_64Linux);
}

#[test]
#[ignore = "explicit AArch64 runtime integration target"]
fn aarch64_failure_sites_and_relative_debug_rows_match_the_evaluator() {
    site_fixture(Target::Aarch64Linux);
}

fn growth_shadow_words(target: Target) {
    let source = include_str!("../../../core/front/cases/body/grow-carrier-words.hirsel");
    let (module, names) = Ok::<_, String>(hirsel_core_program_tests::SourceProgram::lower(
        source,
        Some("main"),
    ))
    .unwrap();
    let cases: Vec<_> = [
        ("grown", 131328),
        ("refused", 130816512904),
        ("regrown", 523776),
    ]
    .into_iter()
    .map(|(name, want)| {
        let function = names[name];
        let report = eval::run(&module, function, &[], 1_000_000);
        assert_eq!(report.outcome, eval::Outcome::Returned(vec![want]));
        assert_eq!(report.used, 0);
        Case {
            function,
            args: vec![],
            limit: 1_000_000,
            injection: None,
        }
    })
    .collect();
    for speed in [false, true] {
        let object = module.llvm_object(
            target,
            speed,
            false,
            false,
            std::env::var_os("HIRSEL_TEST_CHARGE_OBSERVATION").is_some(),
        );
        let reports = execute_target_object_checked(
            &module,
            &cases,
            include_str!("../../hirsel-core-rt/testdata/moving_realloc.c"),
            "",
            if speed {
                Optimization::Speed
            } else {
                Optimization::None
            },
            Some(object),
            target,
            true,
        );
        for (case, actual) in cases.iter().zip(reports) {
            let expected = eval::run(&module, case.function, &case.args, case.limit);
            assert_eq!(actual.outcome, expected.outcome);
            assert_eq!(actual.used, expected.used);
            assert_eq!(actual.admissions, expected.admissions);
            assert_eq!(actual.trace, expected.trace);
            if let Some(peak) = actual.peak_reserved {
                assert_eq!(peak, expected.peak_reserved);
            }
        }
    }
}

#[test]
fn growth_shadow_copies_back_data_and_capacity() {
    growth_shadow_words(Target::X86_64Linux);
}

#[test]
#[ignore = "explicit AArch64 runtime integration target"]
fn aarch64_growth_shadow_copies_back_data_and_capacity() {
    growth_shadow_words(Target::Aarch64Linux);
}

#[test]
fn carrier_twins_match_the_evaluator_in_program_builds() {
    carrier_program_cases(Target::X86_64Linux);
    cancelled_cold_word_call(Target::X86_64Linux, false);
    cancelled_cold_word_call(Target::X86_64Linux, true);
}
#[test]
#[ignore = "explicit AArch64 runtime integration target"]
fn aarch64_carrier_twins_match_the_evaluator_in_program_builds() {
    carrier_program_cases(Target::Aarch64Linux);
    cancelled_cold_word_call(Target::Aarch64Linux, false);
    cancelled_cold_word_call(Target::Aarch64Linux, true);
}

#[test]
fn step_counter_survives_inlined_mutations_and_exact_exhaustion() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../core/core");
    let source = r#"
fn append(mut xs: List[u64], n: u64) {
    result.unwrap(list.push(mut xs, n))
}
fn probe(s: u64) -> u64 {
    match (bound steps s {
        let mut xs: List[u64] = result.unwrap(list.with_capacity(8))
        for i in 0u64 .. 8u64 { append(mut xs, i) }
        list.len(xs)
    }) {
        Ok(n) => 1000 + n,
        Error(e) => e.steps
    }
}
"#;
    let (module, names) =
        hirsel_core_program_tests::SourceProgram::lower_with_core(source, None, &root);
    let cases: Vec<_> = (0..=48)
        .map(|s| Case {
            function: names["probe"],
            args: vec![s],
            limit: 1_000_000,
            injection: None,
        })
        .collect();
    assert_eq!(
        eval::run(&module, names["probe"], &[48], 1_000_000).outcome,
        eval::Outcome::Returned(vec![1008])
    );
    for facts in [false, true] {
        let ir =
            emit_ir_for_test_with_facts(&module, Target::X86_64Linux, false, false, facts).unwrap();
        verify_ir(&ir);
        for opt in [Optimization::None, Optimization::Speed] {
            let object =
                hirsel_llvm_lazy::compile_ir(&ir, matches!(opt, Optimization::Speed)).unwrap();
            let got = execute_object(&module, &cases, "", "", opt, Some(object));
            for (case, actual) in cases.iter().zip(got) {
                assert_eq!(
                    actual.outcome,
                    eval::run(&module, case.function, &case.args, case.limit).outcome,
                    "facts={facts}, s={}",
                    case.args[0]
                );
            }
        }
    }
}

#[test]
fn fallible_step_regions_refund_partial_work_and_check_strided_remainders() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../core/core");
    let source = r#"
fn probe(s: u64) -> u64 {
    match (bound steps s {
        let first = result.unwrap(isolate(fn() -> u64 {
            let mut xs: List[u64] = result.unwrap(list.with_capacity(8))
            for i in 0u64 .. 8u64 {
                result.unwrap(list.push(mut xs, i))
                let _ = 100u64 / (5u64 - i)
            }
            list.len(xs)
        }))
        let n = match take first { Ok(n) => n, Error(take cs) => 19u64 }
        for _ in 0u64 .. 7u64 {}
        n
    }) { Ok(n) => 1000 + n, Error(e) => e.steps }
}
fn boundary_overflow(s: u64) -> u64 {
    match (bound steps s {
        for _ in 0u64 .. 65535u64 {}
        let mut k = 18446744073709551612u64
        while k <= 18446744073709551615u64 { k += 2u64 }
        k
    }) { Ok(n) => n, Error(e) => e.steps }
}
fn resetting(s: u64) -> u64 {
    match (bound steps s {
        let mut k = 1u64
        let initial = k
        while k <= 10u64 { k = initial + 1u64 }
        k
    }) { Ok(n) => 1000 + n, Error(e) => e.steps }
}
fn strided(s: u64, stride: u64) -> u64 {
    match (bound steps s {
        let mut xs: List[u64] = result.unwrap(list.with_capacity(16))
        let mut k = 1u64
        while k <= 10u64 {
            result.unwrap(list.push(mut xs, k))
            k += stride
        }
        list.len(xs)
    }) { Ok(n) => 1000 + n, Error(e) => e.steps }
}
"#;
    let (module, names) = hirsel_core_program_tests::SourceProgram::lower_with_core(source, None, &root);
    let mut cases = Vec::new();
    for s in 0..=64 {
        cases.push(Case {
            function: names["probe"],
            args: vec![s],
            limit: 1_000_000,
            injection: None,
        });
        cases.push(Case {
            function: names["resetting"],
            args: vec![s],
            limit: 1_000_000,
            injection: None,
        });
        for stride in [0, 1, 2, 3, u64::MAX] {
            cases.push(Case {
                function: names["strided"],
                args: vec![s, stride],
                limit: 1_000_000,
                injection: None,
            });
        }
    }
    for s in 65536..=65540 {
        cases.push(Case {
            function: names["boundary_overflow"],
            args: vec![s],
            limit: 1_000_000,
            injection: None,
        });
    }
    assert_eq!(
        eval::run(&module, names["probe"], &[64], 1_000_000).outcome,
        eval::Outcome::Returned(vec![1019])
    );
    // isolate call + user entry + two allocation calls, five full trips
    // at three Steps, two calls in the failing trip, outer unwrap, seven latches.
    // 1 + 1 + 2 + 5*3 + 2 + 1 + 7 = 29. Failure is caught before the tail.
    for (s, want) in [(28, 28), (29, 1019)] {
        assert_eq!(
            eval::run(&module, names["probe"], &[s], 1_000_000).outcome,
            eval::Outcome::Returned(vec![want])
        );
    }
    // Two setup calls, three Steps per checked-stride trip, then list.len.
    for (s, stride, want) in [(33, 1, 1010), (18, 2, 1005), (15, 3, 1004)] {
        assert_eq!(
            eval::run(&module, names["strided"], &[s, stride], 1_000_000).outcome,
            eval::Outcome::Returned(vec![want])
        );
    }
    for facts in [false, true] {
        let ir = emit_ir_for_test_with_facts(&module, Target::X86_64Linux, false, false, facts).unwrap();
        verify_ir(&ir);
        for optimization in [Optimization::None, Optimization::Speed] {
            let object =
                hirsel_llvm_lazy::compile_ir(&ir, matches!(optimization, Optimization::Speed))
                    .unwrap();
            let reports = execute_object(&module, &cases, "", "", optimization, Some(object));
            for (case, report) in cases.iter().zip(reports) {
                assert_eq!(
                    report.outcome,
                    eval::run(&module, case.function, &case.args, case.limit).outcome,
                    "facts={facts}, function={:?}, args={:?}",
                    case.function,
                    case.args
                );
            }
        }
    }
    let modules = module
        .functions
        .iter()
        .enumerate()
        .filter_map(|(root, f)| {
            f.body.as_ref()?;
            let mut unit = module.clone();
            for (i, f) in unit.functions.iter_mut().enumerate() {
                if i != root {
                    f.body = None;
                } else if let Some(body) = &mut f.body {
                    body.inst_anchors.fill(None);
                    for block in &mut body.blocks {
                        block.anchor = None;
                    }
                }
            }
            Some(unit)
        })
        .collect::<Vec<_>>();
    let facts = modules
        .iter()
        .map(hirsel_core_ir::roles::Facts::new)
        .collect::<Vec<_>>();
    let units = modules
        .iter()
        .zip(&facts)
        .map(|(module, facts)| hirsel_llvm_lazy::Unit::Core {
            module,
            facts,
            observe_charge: false,
            debug_lines: false,
        })
        .collect::<Vec<_>>();
    let exports = ["probe", "strided", "resetting", "boundary_overflow"]
        .map(|name| symbol(&module, names[name]));
    let object = hirsel_llvm_lazy::emit_program(&units, &exports).unwrap();
    let reports = execute_object(&module, &cases, "", "", Optimization::Speed, Some(object));
    for (case, report) in cases.iter().zip(reports) {
        assert_eq!(
            report.outcome,
            eval::run(&module, case.function, &case.args, case.limit).outcome,
            "split units, function={:?}, args={:?}",
            case.function,
            case.args
        );
    }
}
