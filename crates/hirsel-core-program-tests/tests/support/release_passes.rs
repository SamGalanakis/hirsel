/// The case a front `// expect: run` line names, unbounded.
fn front_case(module: &Module, names: &HashMap<String, FuncId>, run: &str) -> Case {
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
}

/// Every observable field of an evaluator report agrees with the oracle's.
fn same_report(what: &str, got: &eval::Report, want: &eval::Report) {
    assert_eq!(got.outcome, want.outcome, "{what}: outcome");
    assert_eq!(got.sites, want.sites, "{what}: failure sites");
    assert_eq!(got.trace, want.trace, "{what}: cleanup and finalizer trace");
    assert_eq!(got.used, want.used, "{what}: used");
    assert_eq!(got.admissions, want.admissions, "{what}: admissions");
    assert_eq!(
        got.peak_reserved, want.peak_reserved,
        "{what}: peak reservation"
    );
    assert_eq!(got.stdout, want.stdout, "{what}: stdout");
    assert_eq!(got.stderr, want.stderr, "{what}: stderr");
}

/// The release passes (MEM-30 step contexts, then returnable waits, in the
/// order `release_native.rs` and `std_release.rs` run them) preserve every
/// front run case. The oracle is the evaluator on the untransformed module.
/// The evaluator runs each transformed module it can: the library split's
/// checked and step-free bodies, and the executable split of each entry. It
/// cannot run returnable waits, so where `returnable::apply` rewrote the
/// executable the whole module goes through LLVM instead, and its outcome,
/// failure sites and cleanup trace must still be the oracle's.
#[test]
fn every_front_run_case_survives_the_release_passes() {
    use hirsel_core_ir::{returnable, step_contexts};
    let (mut count, mut free, mut executable, mut native, mut open) = (0, 0, 0, 0, 0);
    let mut failures = Vec::new();
    for path in front_run_paths() {
        let source = std::fs::read_to_string(&path).unwrap();
        let runs = source
            .lines()
            .filter_map(|line| line.trim().strip_prefix("// expect: run "))
            .collect::<Vec<_>>();
        if runs.is_empty() {
            continue;
        }
        let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let (program, names) =
                hirsel_core_program_tests::SourceProgram::lower(&source, Some("main"));
            let module: &Module = &program;
            // `evaluator-only:..llvm..` (SITES.md, NOT-BUILT) holds only natively.
            let held = |run: &str| {
                run.split_whitespace().last().is_some_and(|m| {
                    m.strip_prefix("evaluator-only:")
                        .is_some_and(|e| e.split(',').any(|e| e == "llvm"))
                })
            };
            let cases: Vec<(&str, Case)> = runs
                .iter()
                .map(|run| (*run, front_case(module, &names, run)))
                .collect();
            let oracle = |case: &Case| eval::run(module, case.function, &case.args, case.limit);
            let wanted: Vec<eval::Report> = cases.iter().map(|(_, c)| oracle(c)).collect();
            let mut entries: Vec<FuncId> = cases.iter().map(|(_, c)| c.function).collect();
            entries.sort();
            entries.dedup();
            // The release std's split: every root keeps its checked body and,
            // where the free one differs, gains a step-free variant.
            let mut library = module.clone();
            step_contexts::specialize_library(&mut library, &entries);
            for ((run, case), want) in cases.iter().zip(&wanted) {
                let checked = eval::run(&library, case.function, &case.args, case.limit);
                same_report(&format!("library checked `{run}`"), &checked, want);
                if let Some(variant) = library.step_free.get(&case.function) {
                    let got = eval::run(&library, variant.body, &case.args, case.limit);
                    same_report(&format!("library step-free `{run}`"), &got, want);
                    free += 1;
                }
            }
            count += cases.len();
            // A launched executable's split, one entry at a time, then its
            // returnable waits.
            if !step_contexts::self_contained(module) {
                open += cases.len();
                return;
            }
            for &entry in &entries {
                let mut split = module.clone();
                step_contexts::specialize(&mut split, entry);
                let mut rewritten = split.clone();
                let applied = returnable::apply(&mut rewritten);
                if std::env::var_os("HIRSEL_RETURNABLE_REPORT").is_some() {
                    eprint!(
                        "{}: {}",
                        path.display(),
                        returnable::report(&rewritten, &applied)
                    );
                }
                let rewrote = rewritten != split;
                let mine = cases
                    .iter()
                    .zip(&wanted)
                    .filter(|((_, case), _)| case.function == entry);
                let mut natively = Vec::new();
                for ((run, case), want) in mine {
                    let got = eval::run(&split, entry, &case.args, case.limit);
                    same_report(&format!("executable `{run}`"), &got, want);
                    executable += 1;
                    if rewrote && !held(run) {
                        natively.push((
                            *run,
                            Case {
                                function: entry,
                                args: case.args.clone(),
                                limit: case.limit,
                                injection: None,
                            },
                            want,
                        ));
                    }
                }
                if natively.is_empty() {
                    continue;
                }
                // The rewritten module lowers whole, as the release image does.
                let sites = hirsel_core_ir::source::module_sites(&rewritten);
                let (runs, (lowered, wants)): (Vec<&str>, (Vec<Case>, Vec<&eval::Report>)) =
                    natively.into_iter().map(|(r, c, w)| (r, (c, w))).unzip();
                for ((run, want), got) in runs.iter().zip(wants).zip(execute(&rewritten, &lowered))
                {
                    let what = format!("returnable native `{run}`");
                    assert_eq!(got.outcome, want.outcome, "{what}: outcome");
                    let expected_sites = want
                        .sites
                        .iter()
                        .map(|a| anchor_word(&sites, *a))
                        .collect::<Vec<_>>();
                    assert_eq!(got.sites, expected_sites, "{what}: failure sites");
                    assert_eq!(got.trace, want.trace, "{what}: cleanup and finalizer trace");
                    native += 1;
                }
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
    println!(
        "release passes: {count} run cases; {free} step-free library bodies; {executable} executable splits; {native} returnable native runs; {open} in modules that are not self-contained; file failures: {}",
        failures.len()
    );
    assert!(count > 0);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
