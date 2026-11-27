include!("../../../hirsel-cranelift/tests/support/native.rs");
include!("front_cases.rs");
impl NativeInput for hirsel_core_program_tests::SourceProgram {
    fn module(&self) -> &c::Module {
        self
    }
    fn object(&self, optimize: bool, debug: bool, observing: bool) -> Vec<u8> {
        self.cranelift_object(NATIVE_TARGET, optimize, debug, observing)
    }
    fn sites(&self) -> Vec<u64> {
        self.sites.clone()
    }
}
fn check_front_end_run_cases() {
    let mut count = 0;
    let mut failures = Vec::new();
    for file in front_run_paths() {
        let source = fs::read_to_string(&file).unwrap();
        if !source.lines().any(|l| l.starts_with("// expect: run ")) {
            continue;
        }
        println!("front-end fixture: {}", file.display());
        let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let (module, functions) =
                hirsel_core_program_tests::SourceProgram::lower(&source, None);
            let mut runs = Vec::new();
            // A run marked `evaluator-only:..cranelift..` (SITES.md, NOT-BUILT) must
            // still disagree here, so the marker cannot outlive the missing emission.
            let mut pending = Vec::new();
            for line in source
                .lines()
                .filter_map(|l| l.strip_prefix("// expect: run "))
            {
                let marked = line.split_whitespace().last().is_some_and(|m| {
                    m.strip_prefix("evaluator-only:")
                        .is_some_and(|e| e.split(',').any(|e| e == "cranelift"))
                });
                let call = line.split_once(" = ").unwrap().0;
                let (name, args) = call.strip_suffix(')').unwrap().split_once('(').unwrap();
                let f = functions[name];
                let sig = &module.signatures[module.functions[f.index()].signature.index()];
                let args = sig
                    .params
                    .iter()
                    .zip(args.split(',').filter(|a| !a.is_empty()))
                    .map(|(p, a)| {
                        let Param::Value(t) = p else {
                            panic!("scalar run parameter")
                        };
                        eval::scalar(*t, a.trim())
                    })
                    .collect();
                if marked {
                    pending.push((line, (f, args)));
                    continue;
                }
                runs.push((f, args));
                count += 1;
            }
            execute_reports_checked(
                &module,
                &runs,
                false,
                1_000_000,
                None,
                None,
                NativeCheck::Source,
            );
            for (line, run) in pending {
                let agrees = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    execute(&module, &[run], false)
                }))
                .is_ok();
                assert!(
                    !agrees,
                    "`{line}` now agrees on Cranelift: remove `cranelift` from its marker and its NOT-BUILT row"
                );
            }
        }));
        if let Err(error) = attempt {
            let message = error
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| error.downcast_ref::<&str>().copied())
                .unwrap_or("non-string panic");
            // Assembly/link failures prevent every row of this fixture from
            // running; name all of them instead of silently dropping the file.
            let rows = source
                .lines()
                .filter_map(|l| l.strip_prefix("// expect: run "))
                .map(|run| format!("{}: {run}", file.display()))
                .collect::<Vec<_>>();
            failures.push(format!("{}\n{message}", rows.join("\n")));
        }
    }
    assert!(count > 0, "the front-end corpus must execute run cases");
    println!("front-end run cases selected: {count}");
    assert!(
        failures.is_empty(),
        "front-end failures:\n{}",
        failures.join("\n\n")
    );
}

fn front_runs(
    module: &c::Module,
    functions: &std::collections::HashMap<String, FuncId>,
    source: &str,
) -> Vec<(FuncId, Vec<u64>)> {
    source
        .lines()
        .filter_map(|line| line.strip_prefix("// expect: run "))
        .map(|line| {
            let call = line.split_once(" = ").unwrap().0;
            let (name, args) = call.strip_suffix(')').unwrap().split_once('(').unwrap();
            let function = functions[name];
            let signature =
                &module.signatures[module.functions[function.index()].signature.index()];
            let args = signature
                .params
                .iter()
                .zip(args.split(',').filter(|a| !a.is_empty()))
                .map(|(p, a)| {
                    let Param::Value(t) = p else {
                        panic!("scalar run")
                    };
                    eval::scalar(*t, a.trim())
                })
                .collect();
            (function, args)
        })
        .collect()
}

fn check_failure_sites() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../core/front");
    let source = fs::read_to_string(root.join("cases/body/failure-sites.hirsel")).unwrap();
    let (module, functions) = hirsel_core_program_tests::SourceProgram::lower(&source, None);
    let runs = front_runs(&module, &functions, &source);
    assert_eq!(runs.len(), 9);
    for optimize in [false, true] {
        execute(&module, &runs, optimize);
    }
}

fn check_float_nan_paths() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../core/front");
    let source = fs::read_to_string(root.join("cases/body/float-nan-paths.hirsel")).unwrap();
    let (module, functions) = hirsel_core_program_tests::SourceProgram::lower(&source, None);
    let runs = front_runs(&module, &functions, &source);
    assert_eq!(runs.len(), 32);
    for optimize in [false, true] {
        execute(&module, &runs, optimize);
    }
}

fn check_growth_shadow_words() {
    let source = include_str!("../../../../core/front/cases/body/grow-carrier-words.hirsel");
    let (module, names) = hirsel_core_program_tests::SourceProgram::lower(source, None);
    let runs: Vec<_> = [
        ("grown", 131328),
        ("refused", 130816512904),
        ("regrown", 523776),
    ]
    .into_iter()
    .map(|(name, want)| {
        let f = names[name];
        let report = eval::run(&module, f, &[], 1_000_000);
        assert_eq!(report.outcome, eval::Outcome::Returned(vec![want]));
        assert_eq!(report.used, 0);
        (f, Vec::new())
    })
    .collect();
    let cases: Vec<_> = runs
        .iter()
        .map(|(function, args)| NativeCase {
            function: *function,
            args,
            limit: 1_000_000,
            injection: None,
            report: None,
        })
        .collect();
    for speed in [false, true] {
        execute_cases_with_bindings(
            &module,
            speed,
            &cases,
            false,
            include_str!("../../../hirsel-core-rt/testdata/moving_realloc.c"),
            NativeCheck::Memcheck,
        );
    }
}

#[test]
fn cancelled_cold_word_call_isolate_refunds_on_both_cranelift_targets() {
    let template = r#"
fn grow_then_check(mut xs: List[u64]) {
    for n in 0u64 .. 100 { list.push(mut xs, n) }
    task.check_cancel()
}
fn isolated_body() {
    let mut xs: List[u64] = list.new()
    if list.len(xs) == list.capacity(xs) { grow_then_check(mut xs) }
}
fn child(take x: u64) -> u64 { CALL_BODY; x }
fn cancelled(tasks: Tasks) -> u64 waits {
    let mut h = task.spawn(tasks, bytes 65536, 41u64, child)
    task.cancel(mut h)
    match task.join(take h) {
        Ok(n) => n,
        Error(Cancelled(take c)) => { let _ = take c; 1 }
        Error(Failed(take c)) => { let _ = take c; 2 }
    }
}
"#;
    for with_isolate in [false, true] {
        let source = template.replace(
            "CALL_BODY",
            if with_isolate {
                "let _ = isolate(fn() -> () { isolated_body() })"
            } else {
                "isolated_body()"
            },
        );
        let (module, names) = hirsel_core_program_tests::SourceProgram::lower(&source, None);
        let evaluator = eval::run(&module, names["cancelled"], &[0], 1_000_000);
        assert_eq!(evaluator.used, 0);
        let expected = eval::Report {
            steps: 0,
            step_floor: 0,
            step_refusal: None,
            outcome: Outcome::Returned(vec![1]),
            used: 0,
            peak_reserved: 0,
            trace: Vec::new(),
            admissions: if with_isolate { 8 } else { 7 },
            stdout: Vec::new(),
            stderr: Vec::new(),
            sites: Vec::new(),
            events: Vec::new(),
        };
        execute_reports(
            &module,
            &[(names["cancelled"], vec![0])],
            true,
            1_000_000,
            None,
            Some(&[expected]),
        );
        println!(
            "cranelift cancelled cold word call isolate={with_isolate}: used=0 evaluator_used={}",
            evaluator.used
        );
    }
}

#[test]
fn simd_cranelift_matches_evaluator() {
    for source in [
        include_str!("../../../../core/front/fixtures/simd-operations.hirsel"),
        include_str!("../../../../core/front/fixtures/simd-differential.hirsel"),
    ] {
        let (module, functions) = hirsel_core_program_tests::SourceProgram::lower(source, None);
        let runs = front_runs(&module, &functions, source);
        execute(&module, &runs, false);
    }
}
