const PROG4_SOURCE: &str = r#"
fn private_answer() -> u64 { 7u64 }
fn deep() -> u64 { deep() + 1u64 }
resource type Bad { divisor: u64 } drop(r) { let _ = 1u64 / r.divisor; }
@test
fn good() -> Result[(), ()] { if private_answer() == 7u64 { Ok(()) } else { Error(()) } }
@test
fn error() -> Result[(), ()] { Error(()) }
@test
fn exit_zero() -> Result[(), Exit] { Error(Exit(status: 0)) }
@test
fn exit_seven() -> Result[(), Exit] { Error(Exit(status: 7)) }
@test
fn failed() -> Result[(), ()] { fail("test failure") }
@test
fn stopped() -> Result[(), ()] { let _ = deep(); Ok(()) }
@test
fn own_error() -> Result[(), Bad] { Error(result.unwrap(Bad(divisor: 0u64))) }
@test
fn own_success() -> Result[(), Bad] { Ok(()) }
@test
fn allocation() -> Result[(), Capacity] {
    let mut values: Bytes = list.new()
    list.try_reserve(mut values, 8u64)
}
@test
fn raised_allocation() -> Result[(), ()] { let _: Bytes = list.with_capacity(8u64); Ok(()) }
@test
fn granted(out: Stdout) -> Result[(), ()] waits { result.unwrap(stdout.write(out, "test ran\n")); Ok(()) }
"#;
fn prog4_request(source: &str, dir: &Path) -> CheckRequest {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut request = source_request(source, dir);
    request.entry = None;
    request.inputs.packages[1] = PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
    request
}

fn prog4_runs(backend: Backend) {
    let dir = tempfile::tempdir().unwrap();
    let request = prog4_request(PROG4_SOURCE, dir.path());
    // Fresh and disk-reused products must discover the same tests, without
    // retaining a live Program or checking their source a second time.
    Driver::default().check(request.clone(), &mut Meter::new(false)).unwrap();
    let product = Driver::default().check(request, &mut Meter::new(false)).unwrap();
    assert!(product.accepted(), "{:?} {:?}", product.diagnostics, product.incomplete);
    assert!(product.rechecked.is_empty());
    assert_eq!(product.tests().map(|d| d.0.name.as_str()).collect::<Vec<_>>(), ["allocation", "error", "exit_seven", "exit_zero", "failed", "good", "granted", "own_error", "own_success", "raised_allocation", "stopped"]);
    let mut options = native_options(backend, dir.path());
    let test = |name: &str| DefPath::from_full(&format!("app@1/main/{name}")).unwrap();
    for (name, limit, grants, code, stdout, stderr) in [
        // PROG-3: a test run is a run, so `Error(e)`, `E` not `Exit`, writes
        // `error: ` and `e` as Hirsel source.
        ("error", 4096, 0, 1, "", "error: ()\n"),
        ("exit_zero", 4096, 0, 1, "", ""),
        ("exit_seven", 4096, 0, 7, "", ""),
        ("failed", 4096, 0, 101, "", "Fail"),
        ("stopped", 4096, 0, 101, "", "Stack at app@1/main.hirsel:"),
        ("own_error", 4096, 0, 101, "", "Division"),
        ("own_success", 288, 0, 0, "", ""),
        // No room beyond main's 288: reserving 8 one-byte elements asks for 8.
        ("allocation", 288, 0, 1, "", "error: Capacity(requested: 8, available: 0)\n"),
        ("allocation", 296, 0, 0, "", ""),
        ("raised_allocation", 288, 0, 101, "", "Capacity(requested: 8, available: 0) at app@1/main.hirsel:"), // and the root limit's hint
        ("good", 287, 0, 103, "", "not started: the root limit is below main's startup charge of 288 bytes\n"),
        ("granted", 4096, 0, 103, "", "not started: grant `out` is not in the run's configuration\n"),
        ("granted", 4096, 1 << 4, 0, "test ran\n", ""),
        // A stop and every other failed outcome leave this last run unaffected.
        ("good", 288, 0, 0, "", ""),
    ] {
        options.output = dir.path().join(format!("{name}-{limit}-{grants}"));
        options.root_limit = limit;
        options.grant_mask = grants;
        let outcome = product.run_test(&test(name), &options).unwrap();
        assert_eq!(outcome.passed(), code == 0, "{name}: {outcome:?}");
        assert_eq!(outcome.output.status.code(), Some(code), "{name}: {outcome:?}");
        assert_eq!(outcome.output.stdout, stdout.as_bytes(), "{name}");
        if matches!(name, "failed" | "stopped" | "own_error" | "raised_allocation") {
            let lines = String::from_utf8(outcome.output.stderr).unwrap();
            assert_eq!(lines.lines().count(), 1, "{name}: {lines}");
            assert!(lines.starts_with(stderr), "{name}: {lines}");
            assert!(name != "raised_allocation" || lines.ends_with(" (root limit 288 bytes; raise it with --limit or the manifest)\n"), "{lines}");
        } else {
            assert_eq!(outcome.output.stderr, stderr.as_bytes(), "{name}");
        }
    }
    assert!(product.run_test(&test("private_answer"), &options).is_err());
}

fn prog4_pass_cache(backend: Backend) {
    use crate::{Runner, Source, TestCache};
    use std::time::Duration;

    let dir = tempfile::tempdir().unwrap();
    let check = |source: &str| {
        let product = Driver::default()
            .check(prog4_request(source, dir.path()), &mut Meter::new(false))
            .unwrap();
        assert!(
            product.accepted(),
            "{:?} {:?}",
            product.diagnostics,
            product.incomplete
        );
        product
    };
    let product = check(PROG4_SOURCE);
    let cache = TestCache::open(&dir.path().join("passes")).unwrap();
    let test = |name: &str| DefPath::from_full(&format!("app@1/main/{name}")).unwrap();
    let base = native_options(backend, dir.path());
    let run = |product: &CheckProduct, name, options: &NativeOptions, rerun| {
        product
            .run_test_reusing(
                &test(name),
                options,
                None,
                &Runner::Native,
                Some(&cache),
                rerun,
            )
            .unwrap()
    };
    for (warm, rerun) in [(false, false), (true, false), (true, true), (true, false)] {
        for (name, limit, grants, code) in [
            ("allocation", 296, 0, 0),
            ("error", 4096, 0, 1),
            ("exit_seven", 4096, 0, 7),
            ("exit_zero", 4096, 0, 1),
            ("failed", 4096, 0, 101),
            ("good", 288, 0, 0),
            ("granted", 4096, 1 << 4, 0),
            ("own_error", 4096, 0, 101),
            ("own_success", 288, 0, 0),
            ("stopped", 4096, 0, 101),
        ] {
            let options = NativeOptions {
                root_limit: limit,
                grant_mask: grants,
                ..base.clone()
            };
            let (outcome, source) = run(&product, name, &options, rerun);
            let expected = if warm && !rerun && code == 0 && grants == 0 {
                Source::Cached
            } else {
                Source::Executed
            };
            assert_eq!(source, expected, "{name}");
            assert_eq!(
                outcome.output.status.code(),
                Some(code),
                "{name}: {outcome:?}"
            );
            assert_eq!(outcome.passed(), code == 0, "{name}");
            if name == "granted" {
                assert_eq!(outcome.output.stdout, b"test ran\n");
            }
        }
    }

    let limited = NativeOptions {
        root_limit: 288,
        ..base.clone()
    };
    let undelivered = NativeOptions {
        output: dir.path().join("unused-output"),
        ..limited.clone()
    };
    let (_, source) = run(&product, "good", &undelivered, false);
    assert_eq!(source, Source::Cached);
    assert!(!undelivered.output.exists());
    // Coverage "Off is off": a plain pass is not served as a coverage pass.
    let covered = NativeOptions {
        coverage: Some([package("app")].into()),
        ..limited.clone()
    };
    let (outcome, source) = run(&product, "good", &covered, false);
    assert_eq!(source, Source::Executed, "{outcome:?}");
    assert!(outcome.passed(), "{outcome:?}");
    if backend == Backend::Cranelift {
        // A pass carries its profile (schema 5): the cached pass reports the
        // sites the executed one reached. By hand, from `good`'s source: both
        // entries, and the `if`'s `yes` arm only (`private_answer()` is 7).
        let (cached, source) = run(&product, "good", &covered, false);
        assert_eq!(source, Source::Cached, "{cached:?}");
        assert_eq!(cached.coverage, outcome.coverage);
        let item = &PROG4_SOURCE[PROG4_SOURCE.find("@test\nfn good").unwrap()..];
        let (start, text) = (item.find("private_answer() == 7u64").unwrap(), "private_answer() == 7u64");
        let decision = format!("app@1/main/good@{start}-{}.0", start + text.len());
        let reached = coverage_reached(&cached.coverage);
        for (key, hit) in [
            ("app@1/main/good#e".to_owned(), true),
            ("app@1/main/private_answer#e".to_owned(), true),
            (format!("{decision}:yes"), true),
            (format!("{decision}:no"), false),
        ] {
            assert_eq!(reached.get(&key), Some(&hit), "{key}: {reached:?}");
        }
    }

    let (outcome, source) = run(&product, "allocation", &limited, false);
    assert_eq!(source, Source::Executed);
    assert_eq!(outcome.output.status.code(), Some(1));
    let callee_edit = check(&PROG4_SOURCE.replace("-> u64 { 7u64 }", "-> u64 { 8u64 }"));
    let (outcome, source) = run(&callee_edit, "good", &limited, false);
    assert_eq!(source, Source::Executed);
    assert_eq!(outcome.output.status.code(), Some(1));
    let body_edit = check(&PROG4_SOURCE.replace(
        "fn own_success() -> Result[(), Bad] { Ok(()) }",
        "fn own_success() -> Result[(), Bad] { Error(result.unwrap(Bad(divisor: 0u64))) }",
    ));
    let (outcome, source) = run(&body_edit, "own_success", &limited, false);
    assert_eq!(source, Source::Executed);
    assert_eq!(outcome.output.status.code(), Some(101));

    let other_backend = match backend {
        Backend::Cranelift => Backend::Llvm,
        Backend::Llvm => Backend::Cranelift,
    };
    // Debug lines embed DWARF and change no object (profiling.md), so the pass
    // recorded without them is the same program's; the others change its code.
    for (label, options, identity, expected) in [
        (
            "backend",
            NativeOptions {
                backend: other_backend,
                ..limited.clone()
            },
            None,
            Source::Executed,
        ),
        (
            "optimize",
            NativeOptions {
                optimize: true,
                ..limited.clone()
            },
            None,
            Source::Executed,
        ),
        (
            "debug lines",
            NativeOptions {
                debug_lines: true,
                ..limited.clone()
            },
            None,
            Source::Cached,
        ),
        (
            "compiler",
            limited.clone(),
            Some(ContentHash::of_bytes(b"test compiler identity")),
            Source::Executed,
        ),
    ] {
        let old = crate::reuse::TOOLCHAIN.replace(identity);
        let (outcome, source) = run(&product, "good", &options, false);
        crate::reuse::TOOLCHAIN.set(old);
        assert_eq!(source, expected, "{label}");
        assert!(outcome.passed(), "{label}: {outcome:?}");
    }

    // Hold the host recipe and inputs fixed to isolate the target. The native
    // recipe cannot link an AArch64 object, so a pass would have come from reuse.
    let inputs =
        crate::tool_inputs::toolchain(&limited.link, &limited.target, &limited.runtime).unwrap();
    let foreign = NativeOptions {
        target: "aarch64-unknown-linux-gnu".into(),
        observed: Some(std::sync::Arc::new(inputs)),
        ..limited.clone()
    };
    assert!(
        product
            .run_test_reusing(
                &test("good"),
                &foreign,
                None,
                &Runner::Native,
                Some(&cache),
                false
            )
            .is_err()
    );

    let spinner = check("@test fn spins() -> Result[(), ()] { loop {} }\n");
    for _ in 0..2 {
        let (outcome, source) = spinner
            .run_test_reusing(
                &test("spins"),
                &base,
                Some(Duration::from_secs(1)),
                &Runner::Native,
                Some(&cache),
                false,
            )
            .unwrap();
        assert_eq!(source, Source::Executed);
        assert!(outcome.timed_out);
        assert!(!outcome.passed());
    }
    if backend == Backend::Cranelift {
        // Coverage "Killed run": the runner kills the spinner at its timeout,
        // and the mapped file still holds the entry flag set before the hang;
        // the run counts as failed.
        let covered = NativeOptions { coverage: Some([package("app")].into()), ..base.clone() };
        let spins = test("spins");
        let outcome = spinner
            .run_test_with_timeout(&spins, &covered, Some(Duration::from_secs(1)))
            .unwrap();
        assert!(outcome.timed_out, "{outcome:?}");
        assert_eq!(coverage_reached(&outcome.coverage).get("app@1/main/spins#e"), Some(&true));
        let mut session = crate::test_run::Session::new(&covered, false);
        session.run(0, &spins, None, outcome.passed(), &outcome.coverage);
        let profile = session.placed(&spinner, covered.coverage.as_ref().unwrap()).unwrap().0.json();
        let entry = &profile["sites"][0];
        assert_eq!((&entry["site"], &entry["passed_runs"], &entry["failed_runs"]), (&"app@1/main/spins#e".into(), &0.into(), &1.into()));
    }

    prog4_cache_damage(&product, &limited, dir.path());
}

fn prog4_cache_damage(product: &CheckProduct, options: &NativeOptions, dir: &Path) {
    use crate::{Runner, Source, TestCache};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    let run = |name, cache: &TestCache, expected| {
        let test = DefPath::from_full(&format!("app@1/main/{name}")).unwrap();
        let (outcome, source) = product
            .run_test_reusing(&test, options, None, &Runner::Native, Some(cache), false)
            .unwrap();
        assert_eq!(source, expected, "{name}: {outcome:?}");
        assert!(outcome.passed(), "{name}: {outcome:?}");
    };
    let only_record = |root: &Path| {
        let paths: Vec<_> = fs::read_dir(root)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.is_file())
            .collect();
        assert_eq!(paths.len(), 1);
        paths.into_iter().next().unwrap()
    };
    let root = dir.join("damage");
    let cache = TestCache::open(&root).unwrap();
    run("good", &cache, Source::Executed);
    let record = only_record(&root);
    let repair = || {
        run("good", &cache, Source::Executed);
        run("good", &cache, Source::Cached);
    };

    let length = fs::metadata(&record).unwrap().len();
    fs::OpenOptions::new()
        .write(true)
        .open(&record)
        .unwrap()
        .set_len(length / 2)
        .unwrap();
    repair();

    let other_root = dir.join("other-pass");
    let other_cache = TestCache::open(&other_root).unwrap();
    run("own_success", &other_cache, Source::Executed);
    fs::copy(only_record(&other_root), &record).unwrap();
    repair();

    // SAFETY: geteuid takes no arguments and reads the process identity.
    if unsafe { libc::geteuid() } != 0 {
        fs::set_permissions(&record, fs::Permissions::from_mode(0o000)).unwrap();
        run("good", &cache, Source::Executed);
    }

    let name = record.file_name().unwrap().to_str().unwrap();
    let key = ContentHash(std::array::from_fn(|i| {
        u8::from_str_radix(&name[2 * i..2 * i + 2], 16).unwrap()
    }));
    let store = Store::new(&root).unwrap();
    for payload in [
        b"garbage".to_vec(),
        encode(&(0u32, Vec::<u8>::new(), Vec::<u8>::new())).unwrap(),
    ] {
        store.put_keyed(key, &payload).unwrap();
        repair();
    }
}

const KNOWN_ARMS: &str = r#"
type Pick = PickA | PickB | PickC
fn known(n: u64, p: Pick) -> u64 {
    let mut out = 0u64
    if n == 9u64 { out = 1u64 }
    let base = match p {
        PickA => 10u64,
        PickB => 20u64,
        PickC => 30u64
    }
    if n < 5u64 && n > 0u64 { out + base } else { out + base + 1u64 }
}
type Spot: key { x: u64, y: u64 }
fn generated(n: u64) -> u64 {
    let mut out = 0u64
    if n > 1u64 {
        let mut xs: List[u64] = list.new()
        list.push(mut xs, n)
        let _ = int.checked_add(xs[0u64], n)
        let _ = Spot(x: n, y: 1u64) == Spot(x: n, y: 2u64)
        out = n
    }
    out
}
@test
fn arms() -> Result[(), ()] { if known(0u64, PickA) + known(2u64, PickB) + generated(2u64) == 33u64 { Ok(()) } else { Error(()) } }
"#;

/// Coverage "Off is off" and "Known arms" (`docs/coverage.md`, "Laws"), on
/// `backend` (an optimised build when `optimize`). A plain build after a coverage build in one store has no
/// `hirsel_cov` section (oracle: the section's absence), and the coverage
/// build's raw file reports `known`'s arms as derived by hand from its
/// source: an `if` without `else` taken only `no`, a `match` whose third arm
/// no input reaches, and `&&` whose right side always runs. `generated`'s only
/// decision is its `if`: the branches the front makes for `list.push`, the
/// index read, `int.checked_add` and record `==` are not sites. Returns each
/// site key with whether the run reached it, which "Engines agree" compares.
fn coverage_off_and_known_arms(backend: Backend, optimize: bool) -> BTreeMap<String, bool> {
    use object::{Object, ObjectSection};
    let dir = tempfile::tempdir().unwrap();
    let product = Driver::default()
        .check(prog4_request(KNOWN_ARMS, dir.path()), &mut Meter::new(false))
        .unwrap();
    assert!(product.accepted(), "{:?} {:?}", product.diagnostics, product.incomplete);
    let test = DefPath::from_full("app@1/main/arms").unwrap();
    let plain = NativeOptions { optimize, ..native_options(backend, dir.path()) };
    let scope: hirsel_core_ir::coverage::Scope = [package("app")].into();
    let covered = NativeOptions { coverage: Some(scope.clone()), output: dir.path().join("covered"), ..plain.clone() };
    let section = |options: &NativeOptions| {
        let built = product.build(&test, options).unwrap();
        let bytes = std::fs::read(&built.executable).unwrap();
        let file = object::File::parse(&*bytes).unwrap();
        let found = file.section_by_name(hirsel_core_ir::coverage::SECTION).is_some();
        (built.executable, found)
    };
    let (_, found) = section(&covered);
    assert!(found, "a coverage executable has the flag section");
    assert!(!section(&plain).1, "a plain build after a coverage build has no flag section");

    // Through the runner: the raw file read against the link's joined fragments.
    let outcome = product.run_test(&test, &covered).unwrap();
    assert!(outcome.passed(), "{outcome:?}");
    let all = coverage_reached(&outcome.coverage);
    let reached = |name: &str| {
        let item = format!("app@1/main/{name}");
        all.iter()
            .filter(|(key, _)| key.strip_prefix(&item).is_some_and(|rest| rest.starts_with(['#', '@'])))
            .map(|(key, flag)| (key.clone(), *flag))
            .collect::<BTreeMap<_, _>>()
    };
    // Keys by hand: offsets within the item of each decision's source text.
    let expected = |name: &str, arms: &[(&str, bool, bool)]| {
        let item = &KNOWN_ARMS[KNOWN_ARMS.find(&format!("fn {name}(")).unwrap()..];
        let mut expected = BTreeMap::from([(format!("app@1/main/{name}#e"), true)]);
        for &(text, yes, no) in arms {
            let start = item.find(text).unwrap();
            let key = format!("app@1/main/{name}@{start}-{}.0:", start + text.len());
            expected.insert(format!("{key}yes"), yes);
            expected.insert(format!("{key}no"), no);
        }
        expected
    };
    let known = [
        ("n == 9u64", false, true),
        ("PickA", true, true),
        ("PickB", true, false),
        ("n < 5u64", true, false),
        ("n < 5u64 && n > 0u64", true, true),
    ];
    assert_eq!(reached("known"), expected("known", &known));
    assert_eq!(reached("generated"), expected("generated", &[("n > 1u64", true, false)]));

    // Places by hand from KNOWN_ARMS's lines (line 1 is its leading break):
    // each decision's line, column and end column, and each arm's target
    // line, absent for an empty arm: the `no` of the `if` without `else`. The
    // short-circuit `no` of `n < 5u64` lands on the decision of the whole `&&`.
    let mut session = crate::test_run::Session::new(&covered, false);
    session.run(0, &test, None, true, &outcome.coverage);
    let profile = session.placed(&product, &scope).unwrap().0.json();
    let rows: BTreeMap<String, serde_json::Value> = profile["sites"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| (row["site"].as_str().unwrap().to_owned(), row.clone()))
        .collect();
    let entry = &rows["app@1/main/known#e"];
    assert_eq!([&entry["file"], &entry["line"], &entry["col"]], [&serde_json::json!("main.hirsel"), &3.into(), &1.into()]);
    for (text, line, col, end_col, yes_line, no_line) in [
        ("n == 9u64", 5, 8, 17, Some(5), None),
        ("PickA", 7, 9, 14, Some(7), Some(8)),
        ("PickB", 8, 9, 14, Some(8), Some(9)),
        ("n < 5u64", 11, 8, 16, Some(11), Some(11)),
        ("n < 5u64 && n > 0u64", 11, 8, 28, Some(11), Some(11)),
    ] {
        for (arm, arm_line) in [("yes", yes_line), ("no", no_line)] {
            let start = KNOWN_ARMS[KNOWN_ARMS.find("fn known(").unwrap()..].find(text).unwrap();
            let row = &rows[&format!("app@1/main/known@{start}-{}.0:{arm}", start + text.len())];
            let place = [&row["line"], &row["col"], &row["end_line"], &row["end_col"]];
            assert_eq!(place, [line, col, line, end_col].map(|n: u64| serde_json::json!(n)).each_ref(), "{text} {arm}");
            assert_eq!(row["arm_line"].as_u64(), arm_line, "{text} {arm}: {row}");
            assert_eq!(row.get("arm_col").is_some(), arm_line.is_some(), "{text} {arm}");
        }
    }
    all
}

/// Each site key of a run's coverage, with whether the run reached it.
fn coverage_reached(coverage: &crate::test_run::RunCoverage) -> BTreeMap<String, bool> {
    let crate::test_run::RunCoverage::Observed(records) = coverage else {
        panic!("coverage unobserved: {coverage:?}");
    };
    records
        .iter()
        .flat_map(|(record, flags)| record.sites.iter().zip(flags).map(|(site, flag)| (site.key.clone(), *flag == 1)))
        .collect()
}

#[test]
#[ignore = "explicit native integration target"]
fn prog4_independent_runs_limits_and_outcomes_cranelift() {
    prog4_runs(Backend::Cranelift);
    prog4_pass_cache(Backend::Cranelift);
    fail17_stack_laws(Backend::Cranelift);
    coverage_off_and_known_arms(Backend::Cranelift, false);
}
#[test]
#[ignore = "explicit native integration target"]
fn prog4_independent_runs_limits_and_outcomes_llvm() {
    prog4_runs(Backend::Llvm);
    prog4_pass_cache(Backend::Llvm);
    fail17_stack_laws(Backend::Llvm);
    // Engines agree: every site of the program, for one input, is reached or
    // not alike under Cranelift and both LLVM builds.
    let cranelift = coverage_off_and_known_arms(Backend::Cranelift, false);
    assert_eq!(coverage_off_and_known_arms(Backend::Llvm, false), cranelift);
    assert_eq!(coverage_off_and_known_arms(Backend::Llvm, true), cranelift);
    reordered_blocks_refused();
    release_engines_agree();
}

#[test]
fn parallel_test_cli_preserves_rows_and_isolates_shared_directory() {
    crate::openat2_required::require();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let source = r#"
@test
fn a_timeout(c: Clock) -> Result[(), ()] waits {
    host.sleep_until(c, host.mono_ns(c) + 60000000000u64)
    Ok(())
}
@test
fn b_pass(out: Stdout) -> Result[(), ()] waits {
    result.unwrap(stdout.write(out, "finished"))
    Ok(())
}
@test
fn c_error() -> Result[(), ()] { Error(()) }
@test
fn d_missing_grant(input: Stdin) -> Result[(), ()] { Ok(()) }
@test
fn e_build_refused() -> Result[(), ()] { Ok(()) }

fn write_shared(dir: Dir, clock: Clock) -> Result[(), ()] waits {
    let mut file = result.unwrap(host.open(dir, "shared", CreateNew))
    result.unwrap(host.write(mut file, string.as_bytes("test")))
    result.unwrap(host.close(take file))
    host.sleep_until(clock, host.mono_ns(clock) + 100000000u64)
    result.unwrap(host.remove(dir, "shared"))
    Ok(())
}
@test
fn f_first_writer(dir: Dir, clock: Clock) -> Result[(), ()] waits { write_shared(dir, clock) }
@test
fn g_second_writer(dir: Dir, clock: Clock) -> Result[(), ()] waits { write_shared(dir, clock) }
"#;
    let mut request = source_request(source, dir.path());
    request.inputs.packages[1] =
        PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
    let manifest = dir.path().join("inputs.json");
    std::fs::write(&manifest, serde_json::to_vec(&request.inputs).unwrap()).unwrap();
    let options = native_options(Backend::Cranelift, dir.path());
    let Link::Driver(cc) = options.link else {
        unreachable!()
    };
    let runtime = options
        .runtime
        .canonicalize()
        .expect("test runtime archive");
    let cc = cc.canonicalize().expect("test link driver");
    let grant = dir.path().join("grant");
    std::fs::create_dir(&grant).unwrap();
    // Refuse publication of one executable after its source passes checking.
    let output = dir.path().join("executables");
    let blocked = output.join(ContentHash::of_bytes(b"app@1/main/e_build_refused").to_string());
    std::fs::create_dir_all(&blocked).unwrap();
    std::fs::write(blocked.join("occupied"), b"").unwrap();
    let args = [
        "test",
        "--source-core",
        "--rerun",
        "--output",
        output.to_str().unwrap(),
        "--inputs",
        manifest.to_str().unwrap(),
        "--runtime",
        runtime.to_str().unwrap(),
        "--cc",
        cc.to_str().unwrap(),
        "--store",
        request.store.to_str().unwrap(),
        "--limit",
        "1048576",
        "--json",
        "--test-timeout",
        "1",
        "--grant",
        "Clock",
        "--grant",
        "Stdout",
        "--dir",
        grant.to_str().unwrap(),
        "-j",
    ];
    let run = |jobs| {
        let args = args
            .into_iter()
            .chain([jobs])
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let (status, stdout, stderr) = process::tape_request(&args, dir.path());
        assert_eq!(status, Some(1), "{stdout}\n{stderr}");
        let mut result: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(result["passed"], false, "{result} {stderr}");
        let rows = result["tests"].as_array_mut().unwrap();
        let expected = [
            ("a_timeout", false, Some(serde_json::Value::Null), true),
            ("b_pass", true, Some(0.into()), false),
            ("c_error", false, Some(1.into()), false),
            ("d_missing_grant", false, Some(103.into()), false),
            ("e_build_refused", false, None, false),
            ("f_first_writer", true, Some(0.into()), false),
            ("g_second_writer", true, Some(0.into()), false),
        ];
        assert_eq!(rows.len(), expected.len(), "{rows:?}");
        for (row, (name, passed, status, timed_out)) in rows.iter_mut().zip(expected) {
            assert_eq!(row["definition"], format!("app@1/main/{name}"));
            assert_eq!(row["passed"], passed, "{row}");
            assert_eq!(row.get("status"), status.as_ref(), "{row}");
            let wall_ms = row.as_object_mut().unwrap().remove("wall_ms").unwrap();
            if status.is_some() {
                assert_eq!(row["timed_out"], timed_out, "{row}");
                assert!(wall_ms.as_f64().unwrap() >= if timed_out { 1000.0 } else { 0.0 });
            } else {
                assert!(row["stopped"].is_string(), "{row}");
                assert!(wall_ms.is_null(), "{row}");
            }
        }
        assert_eq!(rows[1]["stdout"], "finished");
        assert!(std::fs::read_dir(&grant).unwrap().next().is_none());
        result
    };
    assert_eq!(run("1"), run("4"));
}

/// PROG-4: a package's manifest declares each test's run, and a test's run is
/// its own. From prog4_runs, `allocation` needs a 296-byte root limit and
/// `good` 288. A configuration change re-executes only the tests whose run it
/// changes; a name that is not one of the package's tests is refused.
#[test]
fn manifest_test_runs_key_each_test_alone() {
    let dir = tempfile::tempdir().unwrap();
    let package = dir.path().join("app");
    std::fs::create_dir(&package).unwrap();
    std::fs::write(
        package.join("main.hirsel"),
        r#"
fn private_answer() -> u64 { 7u64 }
@test
fn good() -> Result[(), ()] { if private_answer() == 7u64 { Ok(()) } else { Error(()) } }
@test
fn allocation() -> Result[(), Capacity] {
    let mut values: Bytes = list.new()
    list.try_reserve(mut values, 8u64)
}
"#,
    )
    .unwrap();
    let configure = |limit, test, test_limit| {
        let manifest = format!(
            r#"pkg_v1.Package(
    name: "local/app",
    version: "1.0.0",
    charge: ChargeV1,
    sources: [("main.hirsel", "main")],
    dependencies: [],
    tests: Some(Tests(
        sources: [],
        limit: {limit},
        grants: [],
        runs: [("local/app@1/main/{test}", TestRun(limit: Some({test_limit})))],
    )),
)
"#
        );
        std::fs::write(package.join("hirsel.hcfg"), manifest).unwrap();
    };
    let options = native_options(Backend::Cranelift, dir.path());
    let Link::Driver(cc) = options.link else {
        unreachable!()
    };
    let runtime = options
        .runtime
        .canonicalize()
        .expect("test runtime archive");
    let cc = cc.canonicalize().expect("test link driver");
    let store = dir.path().join("store");
    let libraries = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../core")
        .canonicalize()
        .unwrap();
    let args = |extra: &[&str]| {
        [
            "test",
            package.to_str().unwrap(),
            "--libraries",
            libraries.to_str().unwrap(),
            "--runtime",
            runtime.to_str().unwrap(),
            "--cc",
            cc.to_str().unwrap(),
            "--store",
            store.to_str().unwrap(),
            "--json",
        ]
        .into_iter()
        .chain(extra.iter().copied())
        .map(str::to_owned)
        .collect::<Vec<_>>()
    };
    // The pass cache lives under the cache home; a private one keeps the
    // first run's `executed` independent of earlier runs on this host.
    let cache_home = dir.path().join("cache");
    let request = |args: &[String]| {
        let output = process::tape_request_command(args, dir.path())
            .env("XDG_CACHE_HOME", &cache_home)
            .output()
            .unwrap();
        let after = |bytes: &[u8]| {
            let text = String::from_utf8_lossy(bytes);
            let at = text.find(process::CHILD_MARK).expect("request child started")
                + process::CHILD_MARK.len();
            text[at..].to_owned()
        };
        (output.status.code(), after(&output.stdout), after(&output.stderr))
    };
    let run = |expected: [(&str, &str); 2]| {
        let (status, stdout, stderr) = request(&args(&[]));
        assert_eq!(status, Some(0), "{stdout}\n{stderr}");
        let result: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        let rows: Vec<_> = result["tests"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                assert_eq!(row["passed"], true, "{row}");
                (
                    row["definition"].as_str().unwrap().to_owned(),
                    row["source"].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        let expected: Vec<_> = expected
            .iter()
            .map(|(name, source)| (format!("local/app@1/main/{name}"), (*source).to_owned()))
            .collect();
        assert_eq!(rows, expected);
    };
    configure(288, "allocation", 296);
    run([("allocation", "executed"), ("good", "executed")]);
    run([("allocation", "cached"), ("good", "cached")]);
    configure(288, "allocation", 4096);
    run([("allocation", "executed"), ("good", "cached")]);
    configure(289, "allocation", 4096);
    run([("allocation", "cached"), ("good", "executed")]);

    let (status, stdout, stderr) = request(&args(&["--limit", "4096"]));
    assert_ne!(status, Some(0), "{stdout}");
    assert!(stderr.contains("this package declares tests in hirsel.hcfg"), "{stderr}");
    configure(288, "missing", 296);
    let (status, stdout, stderr) = request(&args(&[]));
    assert_ne!(status, Some(0), "{stdout}");
    assert!(
        stderr.contains("local/app@1/main/missing, which is not one of its tests"),
        "{stderr}"
    );
}

const PACKAGE_X: &str = "sha256:9158205f94d44b9e04d00980842459973c2ea442a48b183097ad9d46fe13a7f7";
const PACKAGE_Y: &str = "sha256:cf5868df5dcd15bcbf2de1e5a6bd4b68d09919c8255e45832858b5aa82c70433";
const PACKAGE_W: &str = "sha256:c469ba7bc32d09ebf7461d8f075f6cb53ae9009432441da1ab6707229dae69f5";
const PACKAGE_D: &str = "sha256:fc4c9e66e3bb3dcee82a324ed4f2def822c8882b835f3b5177d4fe8fef897ff0";
const PACKAGE_X_SOURCE: &str = "pub fn version() -> u64 { 1u64 }\n";
const PACKAGE_Y_SOURCE: &str = "pub fn version() -> u64 { 2u64 }\n";
const PACKAGE_MAIN: &str = "pub fn main() -> Result[(), Exit] { Ok(()) }\n";

#[test]
fn inspect_foreign_header_matches_the_c_adapter() {
    use object::{Object, ObjectSection};

    let law = PackageLaw::new();
    law.manifest("ffi_laws", "local/ffi_laws", "1.0.0", r#"sources: [("laws.hirsel", "laws"), ("native.hirsel", "native")], dependencies: [], foreign: Some([("native", Native(symbols: "hfl_", archives: []))]),"#);
    law.write("ffi_laws/laws.hirsel", "pub opaque type Laws {}\npub opaque type Obj {}\n");
    law.write("ffi_laws/native.hirsel", "\
pub fn release(f: Foreign[laws.Laws], take p: foreign.Ptr[laws.Obj]) -> i32 waits
pub fn create(f: Foreign[laws.Laws]) -> Option[foreign.Ptr[laws.Obj]]
pub fn edit(f: Foreign[laws.Laws], p: foreign.Ptr[laws.Obj], src: Bytes, mut dst: Bytes, x: f32) -> f64 waits
pub fn done(f: Foreign[laws.Laws])
");
    let header = PackageLaw::pass(law.run(&[
        "inspect", law.path("ffi_laws").to_str().unwrap(), "--foreign-header", "local/ffi_laws@1/native",
    ]));
    law.write("ffi_laws/native.h", header);
    law.write("ffi_laws/native.c", r#"
#include "native.h"
HIRSEL_ABI_TEXT
static uint8_t cell;
int32_t hfl_release(void *p) { return p == &cell ? 0 : 1; }
void *hfl_create(void) { return &cell; }
double hfl_edit(void *p, const uint8_t *src, uint64_t src_len,
                uint8_t *dst, uint64_t dst_len, float x) {
    return p == &cell && src != dst ? x + src_len + dst_len : 0;
}
void hfl_done(void) {}
"#);
    let options = native_options(Backend::Cranelift, law.dir.path());
    let Link::Driver(cc) = options.link else { unreachable!() };
    let output = Command::new(cc)
        .arg("-std=c11")
        .arg("-c")
        .arg(law.path("ffi_laws/native.c"))
        .arg("-o")
        .arg(law.path("native.o"))
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let bytes = std::fs::read(law.path("native.o")).unwrap();
    let object = object::File::parse(bytes.as_slice()).unwrap();
    let section = object.section_by_name(match crate::host::HOST_TARGET.object_format() { hirsel_core_layout::ObjectFormat::Elf => ".hirsel_abi", hirsel_core_layout::ObjectFormat::MachO => "__hirsel_abi" }).unwrap();
    assert_eq!(
        section.data().unwrap(),
        b"hfl_create stack () -> new local%2Fffi_laws@1/laws/Obj\n\
hfl_done stack () -> ()\n\
hfl_edit waits (handle local%2Fffi_laws@1/laws/Obj, bytes, mut bytes, f32) -> f64\n\
hfl_release waits (take local%2Fffi_laws@1/laws/Obj) -> i32\n"
    );
}

#[test]
fn foreign_publication_pins_grants_and_walks_only_reached_functions() {
    use crate::code_admission::{admit_local, NativeSemantics};
    use crate::code_store::{CodeStore, ObjectKind};
    use hirsel_core_ir::decode::Debit;
    use hirsel_core_ir::release_recipe::{GrantKind, Recipe};
    use sha2::{Digest, Sha256};

    let law = PackageLaw::new();
    let options = native_options(Backend::Cranelift, law.dir.path());
    let Link::Driver(cc) = options.link else { unreachable!() };
    law.write("app/native.c", r#"
unsigned long long hfl_value(void) { return 1; }
static unsigned char cell;
void *hfl_make(void) { return &cell; }
void hfl_free(void *p) { (void)p; }
#define ABI "hfl_value stack () -> u64\n" \
            "hfl_make stack () -> new local%2Fapp@1/main/Obj\n" \
            "hfl_free waits (take local%2Fapp@1/main/Obj) -> ()\n"
__attribute__((used, section(".hirsel_abi")))
static const char abi[sizeof(ABI) - 1] = ABI;
"#.replace(".hirsel_abi", fixture_abi_section()));
    let compile = Command::new(&cc).current_dir(law.path("app"))
        .args(["-std=c11", "-fPIC", "-c", "native.c", "-o", "native.o"]).output().unwrap();
    assert!(compile.status.success(), "{}", String::from_utf8_lossy(&compile.stderr));
    let archive = Command::new(cc.with_file_name("llvm-ar")).current_dir(law.path("app"))
        .args(["rcs", "native.a", "native.o"]).output().unwrap();
    assert!(archive.status.success(), "{}", String::from_utf8_lossy(&archive.stderr));
    let bytes = std::fs::read(law.path("app/native.a")).unwrap();
    let pin = ContentHash(Sha256::digest(&bytes).into());
    let pin_text = pin.to_string();
    law.manifest("app", "local/app", "1.0.0", &format!(r#"sources: [("main.hirsel", "main"), ("native.hirsel", "native")], dependencies: [], foreign: Some([
    ("native", Native(
        symbols: "hfl_",
        archives: [ArchiveFile(target: "{target}", file: "native.a", sha256: "{pin_text}")],
    )),
]),"#, target = options.target));
    law.write("app/native.hirsel", "\
pub fn value(f: Foreign[main.I]) -> u64\n\
pub fn make(f: Foreign[main.I]) -> Option[foreign.Ptr[main.Obj]]\n\
pub fn free(f: Foreign[main.I], take p: foreign.Ptr[main.Obj]) waits\n");
    let main = |body| law.write("app/main.hirsel", format!(
        "pub opaque type I {{}}\npub opaque type Obj {{}}\npub fn main(f: Foreign[I]) -> Result[(), Exit] {{ {body} }}\n"
    ));
    let store = CodeStore::new(law.path("code-store"));
    let publish = || {
        let root = PackageLaw::pass(law.run(&["build", "--emit", "recipe", "app", "--grant", "Foreign=local/app@1/main/I"]));
        let root = crate::native::recipe_build::parse_root(root.trim()).unwrap();
        let recipe = std::fs::read(store.path(ObjectKind::Recipe, root)).unwrap();
        let recipe = Recipe::decode(&recipe, &Debit::unlimited()).unwrap();
        (root, recipe)
    };

    main("Ok(())");
    let (_, grant_only) = publish();
    assert!(grant_only.foreign_modules.is_empty());
    let [archive] = grant_only.foreign_archives.as_slice() else { panic!("expected one pinned archive") };
    assert_eq!((archive.hash, archive.len), (pin, bytes.len() as u64));
    assert_eq!(std::fs::read(store.path(ObjectKind::ForeignArchive, pin)).unwrap(), bytes);
    let [grant] = grant_only.launch.grants.as_slice() else { panic!("expected one grant") };
    assert_eq!((grant.kind, grant.values.as_slice(), grant.archive), (GrantKind::Foreign, ["local/app@1/main/I".to_owned()].as_slice(), Some(*archive)));

    main("let _ = native.value(f); Ok(())");
    let (root, called) = publish();
    assert_eq!(called.foreign_archives, grant_only.foreign_archives);
    let [leaf] = called.foreign_modules.as_slice() else { panic!("expected one reached module") };
    let [reached] = leaf.functions.as_slice() else { panic!("expected one reached function") };
    assert_eq!(reached.identity.definition.to_string(), "local/app@1/native/value");
    assert_eq!(reached.header, "(core@1/core/Foreign[local%2Fapp@1/main/I])->u64");
    admit_local(&store, root, &crate::request::local_admission_limits(), &NativeSemantics).unwrap();

    main("let _ = native.make(f); Ok(())");
    let (root, made) = publish();
    let [leaf] = made.foreign_modules.as_slice() else { panic!("expected one reached module") };
    let functions: BTreeSet<_> = leaf.functions.iter().map(|row| row.identity.definition.0.name.as_str()).collect();
    assert_eq!(functions, BTreeSet::from(["free", "make"]));
    admit_local(&store, root, &crate::request::local_admission_limits(), &NativeSemantics).unwrap();
}

const PROP_LAW_PACKAGE: &str = r#"
fn ok(x: u64) -> Result[(), String] { Ok(()) }
fn never(x: u64) -> bool { false }

@test
fn exhaustive() -> Result[(), ()] {
    let _ = std.prop.all(std.prop.config(), std.prop.u64_in(0, 9), ok)
    Ok(())
}

@test
fn catch_all() -> Result[(), ()] {
    let mut c = std.prop.config()
    c.cases = 5
    match std.prop.all(c, std.prop.u64_in(0, 9), ok) {
        _ => Ok(()),
    }
}

@test
fn discards() -> Result[(), ()] {
    match std.prop.all(std.prop.config(), std.prop.filter(std.prop.u64_in(0, 3), never), ok) {
        Held(_, _) => Ok(()),
        _ => Error(()),
    }
}

@test
fn falsified() -> Result[(), ()] {
    std.prop.check(std.prop.config(), std.prop.u64_in(0, 9), fn(x: u64) -> Result[(), String] {
        if x < 5 { Ok(()) } else { Error(string.from("x < 5")) }
    })
}
"#;

fn prop_law_package() -> PackageLaw {
    let law = PackageLaw::new();
    law.app("app", "dependencies: [(\"std\", Toolchain(package: \"std\", major: 1, authority: []))], tests: Some(Tests(sources: [], limit: 67108864, grants: [], runs: [])),");
    law.write("app/main.hirsel", PROP_LAW_PACKAGE);
    law
}

#[test]
fn prop_law_recorder_captures_settled_outcomes() {
    let law = prop_law_package();
    let (status, stdout, stderr) = law.run(&["test", "app", "--laws", "--json"]);
    assert_eq!(status, Some(1), "{stdout}\n{stderr}");
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let tests = json["tests"].as_array().unwrap();
    assert_eq!(tests.len(), 4);
    for (test, callee, class, numbers, bound, passed) in [
        ("catch_all", "all", "truncated", Some([5, 0]), Some("cases=5"), true),
        ("discards", "all", "vacuous", Some([0, 4]), None, true),
        ("exhaustive", "all", "held", Some([10, 0]), None, true),
        ("falsified", "check", "falsified", None, None, false),
    ] {
        let row = tests.iter().find(|r| r["definition"] == format!("local/app@1/main/{test}")).unwrap();
        assert_eq!(row["passed"], passed, "{row}");
        let [observed] = row["laws"].as_array().unwrap().as_slice() else { panic!("expected one observed law: {row}") };
        assert_eq!(observed["callee"], callee);
        assert_eq!(observed["site"], format!("local/app@1/main/{test}#1"));
        assert_eq!(observed["class"], class);
        assert_eq!(observed["bound"], serde_json::json!(bound));
        if let Some(numbers) = numbers {
            assert_eq!(observed["numbers"], serde_json::json!(numbers));
        }
    }
}

#[test]
fn prop_law_table_reports_and_reuses_rows() {
    let law = prop_law_package();
    law.write("app/main.hirsel", format!("{PROP_LAW_PACKAGE}{}", r#"
fn second_site(first: bool) {
    if first {
        let _ = std.prop.all(std.prop.config(), std.prop.u64_in(0, 9), ok)
    }
    let mut c = std.prop.config()
    c.cases = 10
    let _ = std.prop.all(c, std.prop.u64_in(0, 9), ok)
}

@test
fn exact_limit() -> Result[(), ()] {
    second_site(false)
    Ok(())
}

@test
fn step_bound() -> Result[(), ()] {
    let mut c = std.prop.config()
    c.gen_steps = 0
    let _ = std.prop.all(c, std.prop.u64_in(0, 9), ok)
    Ok(())
}
"#));
    let (status, text, stderr) = law.run(&["test", "app", "--laws"]);
    assert_eq!(status, Some(1), "{text}\n{stderr}");
    for line in [
        "all  catch_all#1  truncated (run 5, discarded 0; cases=5)",
        "all  discards#1  vacuous (run 0, discarded 4)",
        "all  exhaustive#1  held (run 10, discarded 0)",
        "all  second_site#2  held (run 10, discarded 0)",
        "all  step_bound#1  truncated (run 0, discarded 0; max_choices/gen_steps) (per build)",
    ] {
        assert!(text.lines().any(|actual| actual.trim() == line), "missing {line}:\n{text}");
    }
    let json_run = |extra: &[&str]| {
        let mut args = vec!["test", "app", "--laws", "--json", "--filter", "exact_limit"];
        args.extend_from_slice(extra);
        let stdout = PackageLaw::pass(law.run(&args));
        let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        json["tests"][0].clone()
    };
    let executed = json_run(&["--rerun"]);
    let reused = json_run(&[]);
    assert_eq!(executed["source"], "executed");
    assert_eq!(reused["source"], "cached");
    assert_eq!(reused["laws"], executed["laws"]);
    assert_eq!(reused["laws"], serde_json::json!([{
        "callee": "all", "site": "local/app@1/main/second_site#2", "class": "held",
        "numbers": [10, 0], "bound": null, "per_build": false, "witnessed": {}
    }]));
}

#[test]
fn law_lock_checks_static_columns_and_preserves_unselected_lines() {
    let law = PackageLaw::new();
    law.app("app", "dependencies: [(\"std\", Toolchain(package: \"std\", major: 1, authority: []))], tests: Some(Tests(sources: [(\"laws.hirsel\", \"main\")], limit: 67108864, grants: [], runs: [])),");
    law.write("app/laws.hirsel", PROP_LAW_PACKAGE);
    PackageLaw::pass(law.run(&["laws", "write", "app"]));
    let initial = std::fs::read_to_string(law.path("app/hirsel.laws")).unwrap();
    assert!(initial.lines().any(|line| line.starts_with("local/app@1/main/catch_all ")));
    assert!(!law.path("app/hirsel.lock").exists());
    PackageLaw::pass(law.run(&["laws", "check", "app"]));
    law.write("app/laws.hirsel", PROP_LAW_PACKAGE.replace("c.cases = 5", "c.cases = 6"));
    let (status, stdout, stderr) = law.run(&["laws", "check", "app"]);
    assert_eq!(status, Some(1), "{stdout}\n{stderr}");
    let lines = stdout.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 1, "{stdout}");
    assert!(lines[0].starts_with("local/app@1/main/catch_all: src "), "{stdout}");
    PackageLaw::pass(law.run(&["laws", "write", "app"]));
    PackageLaw::pass(law.run(&["laws", "check", "app"]));

    const OTHERS: &str = "local/app@1/main/discards src=0000000000000001 sig=0000000000000002 run=0000000000000003 bug=- oracle=- rows=all#1:vacuous:0/4\n";
    const SELECTED: &str = "local/app@1/main/catch_all src=0000000000000004 sig=0000000000000005 run=0000000000000006 bug=- oracle=-";
    let path = "app/hirsel.laws";
    law.write(path, format!("hirsel.laws 1\n{SELECTED} rows=pending\n{OTHERS}"));
    PackageLaw::pass(law.run(&["test", "app", "--laws", "--write-laws", "--filter", "catch_all"]));
    assert_eq!(std::fs::read_to_string(law.path(path)).unwrap(),
        format!("hirsel.laws 1\n{SELECTED} rows=all#1:truncated:6/0\n{OTHERS}"));
}

/// Bug (2443): a law matching a contextual constructor hashed every visible
/// constructor of that name, so one unused `Malformed` in std.proxy moved 74
/// lock rows in packages that never use it.
#[test]
fn law_signature_covers_the_resolved_constructor_type_only() {
    let law = PackageLaw::new();
    law.manifest("app", "local/app", "1.0.0", "sources: [(\"main.hirsel\", \"main\"), (\"shapes.hirsel\", \"shapes\"), (\"decoy.hirsel\", \"decoy\")], dependencies: [(\"std\", Toolchain(package: \"std\", major: 1, authority: []))], tests: Some(Tests(sources: [(\"laws.hirsel\", \"laws\")], limit: 67108864, grants: [], runs: [])),");
    law.write("app/main.hirsel", PACKAGE_MAIN);
    law.write("app/shapes.hirsel", "pub type Shape = Round(u64) | Flat\npub fn flat() -> Shape { Flat }\n");
    law.write("app/decoy.hirsel", "pub type Other = Bumpy | Lumpy\n");
    law.write("app/laws.hirsel", "@test\nfn law() -> Result[(), ()] {\n    match shapes.flat() {\n        Flat => Ok(()),\n        _ => Error(()),\n    }\n}\n");
    let columns = |law: &PackageLaw| {
        PackageLaw::pass(law.run(&["laws", "write", "app"]));
        PackageLaw::pass(law.run(&["laws", "check", "app"]));
        let lock = std::fs::read_to_string(law.path("app/hirsel.laws")).unwrap();
        let line = lock.lines().find(|line| line.starts_with("local/app@1/laws/law ")).unwrap_or_else(|| panic!("{lock}")).to_owned();
        let field = |name: &str| line.split(' ').find_map(|f| f.strip_prefix(name)).unwrap().to_owned();
        (field("src="), field("sig="))
    };
    let (src, sig) = columns(&law);
    law.write("app/decoy.hirsel", "pub type Other = Bumpy | Lumpy | Flat\n");
    assert_eq!(columns(&law), (src.clone(), sig.clone()), "an unused same-named constructor moved the law");
    law.write("app/shapes.hirsel", "pub type Shape = Round(u64) | Flat | Square\npub fn flat() -> Shape { Flat }\n");
    let (moved_src, moved_sig) = columns(&law);
    assert_eq!(moved_src, src);
    assert_ne!(moved_sig, sig, "a variant added to the resolved type kept the law's signature");
}

#[test]
fn law_baseline_compares_full_runs_and_keeps_test_results() {
    let law = PackageLaw::new();
    law.app("app", "dependencies: [(\"std\", Toolchain(package: \"std\", major: 1, authority: []))], tests: Some(Tests(sources: [], limit: 67108864, grants: [], runs: [])),");
    law.write("app/main.hirsel", r#"
fn property(x: u64) -> Result[(), String] {
    let _ = std.prop.sometimes("big", x > 100)
    Ok(())
}

@test
fn range() -> Result[(), ()] {
    match std.prop.all(std.prop.config(), std.prop.u64_in(0, 4), property) {
        Held(5, 0) => Ok(()),
        _ => Error(()),
    }
}
"#);
    let columns = "src=0000000000000001 sig=0000000000000002 run=0000000000000003 bug=- oracle=-";
    law.write("base.laws", format!("hirsel.laws 1\nlocal/app@1/main/range {columns} rows=all#1:held:10/0;sometimes:big\n"));
    let (status, stdout, stderr) = law.run(&["test", "app", "--laws-baseline", "base.laws", "--json"]);
    assert_eq!(status, Some(1), "{stdout}\n{stderr}");
    let report: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(report["passed"], false);
    assert_eq!(report["tests"][0]["passed"], true);
    assert_eq!(report["law_regressions"], serde_json::json!([
        "local/app@1/main/range all#1: held 5/0 below held 10/0",
        "local/app@1/main/range sometimes:big: witnessed 0, baseline witnessed"
    ]));
    law.write("base.laws", format!("hirsel.laws 1\nlocal/app@1/main/range {columns} rows=all#1:held:5/0\n"));
    PackageLaw::pass(law.run(&["test", "app", "--laws-baseline", "base.laws"]));

    for (option, value) in [
        ("--filter", Some("range")),
        ("--seed", Some("1")),
        ("--sim", None),
        ("--seeds", Some("2")),
        ("--property-corpus", Some("corpus")),
        ("--replay", None),
    ] {
        let mut args = vec!["test", "app", "--laws-baseline", "base.laws", option];
        args.extend(value);
        PackageLaw::refuse(law.run(&args),
            &format!("test: --laws-baseline compares a full run; drop {option}"));
    }
}

#[test]
fn prop_witness_counts_include_zero_and_spawned_tasks() {
    let law = PackageLaw::new();
    law.app("app", "dependencies: [(\"std\", Toolchain(package: \"std\", major: 1, authority: []))], tests: Some(Tests(sources: [], limit: 67108864, grants: [Tasks], runs: [])),");
    law.write("app/main.hirsel", r#"
fn sometimes_big(x: u64) -> Result[(), String] {
    if std.prop.sometimes("big", x > 100) { Error(string.from("outside range")) } else { Ok(()) }
}

@test
fn no_big_numbers() -> Result[(), ()] {
    match std.prop.all(std.prop.config(), std.prop.u64_in(0, 9), sometimes_big) {
        Held(10, 0) => Ok(()),
        _ => Error(()),
    }
}

fn observe_child(take input: u64) -> Result[(), String] {
    if std.prop.sometimes("t", true) { Ok(()) } else { Error(string.from("lost condition")) }
}

fn spawn_witness(take sample: std.prop.Case[u64]) -> Result[(), String] waits {
    let input = sample.value
    let child = task.spawn(sample.tasks, bytes 65536, take input, observe_child)
    match task.join(take child) {
        Ok(Ok(())) => Ok(()),
        _ => Error(string.from("child failed")),
    }
}

@test
fn across_tasks(tasks: Tasks) -> Result[(), ()] waits {
    let mut c = std.prop.config()
    c.cases = 4
    std.prop.check_tasks(tasks, c, std.prop.u64_in(0, 9), spawn_witness)
}
"#);
    for jobs in ["1", "2", "4"] {
        let stdout = PackageLaw::pass(law.run(&["test", "app", "--laws", "-j", jobs]));
        assert_eq!(stdout.lines().filter(|line| line.trim_start().starts_with("sometimes ")).map(str::trim).collect::<Vec<_>>(), ["sometimes t 4", "sometimes big 0"], "-j {jobs}: {stdout}");
    }
}

#[test]
fn package_run_uses_explicit_checkout_std_source() {
    let mut law = PackageLaw::new();
    law.write("core/std/source_probe.hirsel", r#"
pub type Token { amount: u64 }
pub fn token() -> Token { Token(amount: 271) }
"#);
    std::os::unix::fs::symlink(law.libraries.join("core"), law.path("core/core")).unwrap();
    law.libraries = law.path("core");
    law.app("app", "dependencies: [(\"std\", Toolchain(package: \"std\", major: 1, authority: []))],");
    law.write("app/main.hirsel", r#"
fn main() -> Result[(), ()] {
    let answer: std.source_probe.Token = std.source_probe.token()
    if answer.amount == 271 { Ok(()) } else { Error(()) }
}
"#);
    PackageLaw::pass(law.run(&["run", "app", "--source-core", "--source-std"]));
}

#[test]
fn package_run_imports_generic_core_instances_from_checkout_libraries() {
    let mut law = PackageLaw::new();
    law.write(
        "core/core/source_probe.hirsel",
        "pub fn value() -> u64 { 271u64 }\n",
    );
    for entry in std::fs::read_dir(law.libraries.join("core")).unwrap() {
        let entry = entry.unwrap();
        if entry.path().extension().is_some_and(|ext| ext == "hirsel") {
            std::os::unix::fs::symlink(entry.path(), law.path("core/core").join(entry.file_name()))
                .unwrap();
        }
    }
    std::os::unix::fs::symlink(law.libraries.join("std"), law.path("core/std")).unwrap();
    law.libraries = law.path("core");
    law.app("app", "dependencies: [(\"std\", Toolchain(package: \"std\", major: 1, authority: []))],");
    law.write(
        "app/main.hirsel",
        r#"
fn main() -> Result[(), ()] {
    let mut values: Map[(u64, u64), u64] = map.new()
    map.insert(mut values, (2u64, 3u64), source_probe.value())
    let value = option.unwrap(map.get(values, (2u64, 3u64)))
    if value == 271u64 { Ok(()) } else { Error(()) }
}
"#,
    );
    PackageLaw::pass(law.run(&["run", "app"]));
}

#[test]
fn build_names_unmatched_grant_and_entry() {
    let law = PackageLaw::new();
    law.app("app", "");
    PackageLaw::pass(law.run(&["lock", "app"]));
    for mode in [
        &["run", "app", "--backend", "cranelift"][..],
        &["build", "app", "--release", "--limit", "67108864"][..],
    ] {
        for (option, value, grant) in [
            ("--grant", "Stderr", "--grant Stderr"),
            ("--grant", "Env=FOO", "--grant Env"),
            ("--grant", "Net=Connect 127.0.0.1/32 0-65535", "--grant Net"),
            (
                "--grant",
                "Secret:unused=env:HIRSEL_UNUSED_SECRET mac",
                "--grant Secret:unused",
            ),
            ("--dir", ".", "--dir \".\""),
        ] {
            let mut args = mode.to_vec();
            args.extend([option, value]);
            let (status, stdout, stderr) = law.run(&args);
            assert_eq!(status, Some(2), "{stdout}\n{stderr}");
            assert_eq!(
                stderr,
                format!("entry: {grant} has no matching parameter of local/app@1/main/main\n")
            );
            assert!(stdout.is_empty(), "{stdout}");
        }
    }
}

#[test]
fn run_materializes_only_in_the_cache_by_default() {
    let law = PackageLaw::new();
    law.app("app", "");
    let listing = |directory| {
        let mut entries = std::fs::read_dir(law.path(directory)).unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        entries.sort();
        entries
    };
    PackageLaw::pass(law.run(&["lock", "app"]));
    for _ in 0..2 {
        PackageLaw::pass(law.run(&["run", "app"]));
        let names = |entries: &[&str]| entries.iter().map(std::ffi::OsString::from).collect::<Vec<_>>();
        assert_eq!(listing("."), names(&["app", "cache", "compiler-store", "fixtures", "packages"]));
        assert_eq!(listing("app"), names(&["hirsel.lock", "hirsel.hcfg", "main.hirsel"]));
    }
    PackageLaw::pass(law.run(&["run", "app", "-o", "custom-program"]));
    assert!(law.path("custom-program").is_file());
}

#[test]
fn package_archive_outside_root_explains_workspace() {
    let law = PackageLaw::new();
    law.app("app", &format!(
        "dependencies: [(\"json\", Published(package: \"fixture.test/json\", major: 1, root: \"{PACKAGE_X}\", locator: Archive(\"../vendor/jx.hpkg\"), authority: []))],"
    ));
    PackageLaw::refuse(
        law.run(&["lock", "app"]),
        concat!(
            "app/hirsel.hcfg: dependencies.json: archive \"../vendor/jx.hpkg\" is not inside the workspace root. ",
            "To share an archive across packages, add a parent hirsel.hcfg with a pkg_v1.Workspace and members ",
            "listing the package directories, then use a relative path within that workspace",
        ),
    );
}

include!("test_run_foreign_recipe_native.rs");

#[test]
fn inspect_reads_a_package_archive_and_a_named_dependency() {
    let law = PackageLaw::new();
    law.library("jx", "fixture.test/json", "1.0.0", PACKAGE_X_SOURCE, "");
    law.fixture("jx", "vendored.hpkg");
    law.app(".", &format!(
        "dependencies: [(\"json\", Published(package: \"fixture.test/json\", major: 1, root: \"{PACKAGE_X}\", locator: Archive(\"vendored.hpkg\"), authority: []))],"
    ));
    let expected = "fixture.test/json@1/json/version\tpub fn version() -> u64\n";
    assert_eq!(PackageLaw::pass(law.run(&["inspect", "vendored.hpkg", "--interface"])), expected);
    assert_eq!(PackageLaw::pass(law.run(&["inspect", "json"])), expected);
}

#[test]
fn package_member_root_enforces_content_file_limit() {
    use crate::package_content::{Root, Source};
    use crate::package_resolve::Resolution;
    use crate::package_store::PackageStore;
    use crate::package_workspace::Workspace;

    let law = PackageLaw::new();
    law.library("demo", "fixture.test/demo", "1.0.0", "", "");
    let mut workspace = Workspace::discover(&law.path("demo")).unwrap().unwrap();
    let id = workspace.member.clone().unwrap();
    let content = &mut workspace.members.get_mut(&id).unwrap().content;
    content.files.clear();
    let source = Source { module: vec!["m".into()], bytes: bytes::Bytes::new() };
    // C v3, fixture.test/demo 1.0.0, charge-v1, no bindings, and a 65,537-file count.
    let mut wire = b"\x03\x11fixture.test/demo\x01\x00\x00\x00\x09charge-v1\x00\x81\x80\x04".to_vec();
    for index in 0..65_537 {
        let path = format!("{index:05}.hirsel");
        wire.push(12);
        wire.extend_from_slice(path.as_bytes());
        wire.extend_from_slice(b"\x01\x01m\x00");
        content.files.insert(path, source.clone());
    }
    wire.push(0); // No foreign modules.
    assert_eq!(wire.len(), 1_114_167);
    let mut resolution = Resolution::resolve(workspace, PackageStore::new(None), false).unwrap();
    assert_eq!(
        resolution.member_root(&id).unwrap_err(),
        "over the limit of 65536 files and archive rows"
    );

    let content = &mut resolution.workspace.members.get_mut(&id).unwrap().content;
    content.files.pop_last().unwrap();
    wire[34] = 0x80; // First byte of the canonical 65,536 count.
    wire.truncate(wire.len() - 18);
    wire.push(0);
    assert_eq!(resolution.member_root(&id).unwrap(), Root::of(&wire));
}

#[test]
fn package_foreign_roots_and_decoder_domain() {
    let law = PackageLaw::new();
    law.manifest("demo", "fixture.test/demo", "1.0.0", r#"sources: [("api.hirsel", "api"), ("aa.hirsel", "aa"), ("b.hirsel", "b")], dependencies: [], foreign: Some([
    ("aa", Native(
        symbols: "ha_",
        archives: [
            ArchiveFile(
                target: "x86_64-unknown-linux-gnu",
                file: "native/aa-x86.a",
                sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            ),
            ArchiveFile(
                target: "aarch64-unknown-linux-gnu",
                file: "native/aa-arm.a",
                sha256: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            ),
        ],
    )),
    ("b", Native(
        symbols: "hb_",
        archives: [
            ArchiveFile(
                target: "x86_64-unknown-linux-gnu",
                file: "native/b.a",
                sha256: "3e23e8160039594a33894f6564e1b1348bbd7a0088d42c4acb73eeaed59c009d",
            ),
        ],
    )),
]),"#);
    law.write("demo/api.hirsel", "pub opaque type A {}\npub opaque type B {}\n");
    law.write("demo/aa.hirsel", "pub fn ping(f: Foreign[api.A]) -> u64\n");
    law.write("demo/b.hirsel", "pub fn pong(f: Foreign[api.B]) -> u64\n");
    law.write("demo/native/aa-x86.a", b"abc");
    law.write("demo/native/aa-arm.a", b"");
    law.write("demo/native/b.a", b"b");
    assert_eq!(law.fixture("demo", "demo.hpkg"), format!("fixture.test/demo@1 1.0.0 {PACKAGE_D}\n"));
    let content = law.encoded(PACKAGE_D);
    assert_eq!(content.len(), 440);
    // Section 12's item 6 frames, located by their literal wire bytes.
    let position = |pattern: &[u8]| content.windows(pattern.len()).position(|bytes| bytes == pattern).unwrap();
    let aa = position(b"\x01\x02aa\x03ha_\x00\x02");
    let b = position(b"\x01\x01b\x03hb_\x00\x01");
    let arm = position(b"\x19aarch64-unknown-linux-gnu\x0fnative/aa-arm.a");
    let x86 = position(b"\x18x86_64-unknown-linux-gnu\x0fnative/aa-x86.a");
    let swapped = [&content[..aa], &content[b..], &content[aa..b]].concat();
    assert_eq!(
        crate::package_content::Root::of(&swapped).to_string(),
        "sha256:79ed8428993d879c742ebf264780eb9cdf68d61fa4410ea7b7256d19ae64b43b"
    );
    let rows_swapped = [&content[..arm], &content[x86..b], &content[arm..x86], &content[b..]].concat();
    let mut pin_mismatch = content;
    *pin_mismatch.last_mut().unwrap() = b'c';
    law.app("app", &format!("dependencies: [(\"demo\", Published(package: \"fixture.test/demo\", major: 1, root: \"{PACKAGE_D}\", locator: Archive(\"demo.hpkg\"), authority: []))],"));
    for (encoded, refusal) in [
        (swapped, "foreign modules out of order at aa"),
        (rows_swapped, "archive rows out of order at aarch64-unknown-linux-gnu"),
        (pin_mismatch, "archive \"native/b.a\" of b for x86_64-unknown-linux-gnu does not match its pin"),
    ] {
        law.write("app/demo.hpkg", package_bundle(&[&encoded]));
        PackageLaw::refuse(law.run(&["check", "app"]), &format!("fixture.test/demo@1: archive:demo.hpkg is not a package archive: {refusal}"));
        law.empty_store();
    }
    law.fixture_json();
    let content = law.encoded(PACKAGE_X);
    let pin = |root: &str| (7..71).step_by(2).map(|i| u8::from_str_radix(&root[i..i+2], 16).unwrap()).collect::<Vec<_>>();
    let x = pin(PACKAGE_X);
    let y = pin(PACKAGE_Y);
    let util = b"\x11fixture.test/util\x01\x01".as_slice();
    let twice = [b"\x02\x02u1", util, x.as_slice(), b"\x02u2", util, y.as_slice()].concat();
    let std_pin = [b"\x01\x03std\x03std\x01\x01".as_slice(), x.as_slice()].concat();
    law.app("app", &format!("dependencies: [(\"json\", Published(package: \"fixture.test/json\", major: 1, root: \"{PACKAGE_X}\", locator: Archive(\"jx.hpkg\"), authority: []))],"));
    for (bindings, refusal) in [(twice, "bindings of fixture.test/util@1 differ"), (std_pin, "bad binding std = std@1")] {
        assert_eq!(content[33], 0, "jx's no-bindings byte");
        let bytes = [&content[..33], &bindings, &content[34..]].concat();
        law.write("app/jx.hpkg", package_bundle(&[&bytes]));
        PackageLaw::refuse(law.run(&["check", "app"]), &format!("fixture.test/json@1: archive:jx.hpkg is not a package archive: {refusal}"));
        law.empty_store();
    }
}

#[test]
fn package_l1_diamond_and_major_identity() {
    let law = PackageLaw::new();
    law.fixture_json();
    law.write("work/hirsel.hcfg", "pkg_v1.Workspace(members: [\"app\", \"a\", \"b\"])\n");
    let app_edges = "dependencies: [(\"a\", Path(package: \"local/a\", major: 1, path: \"../a\", authority: [])), (\"b\", Path(package: \"local/b\", major: 1, path: \"../b\", authority: []))],";
    law.app("work/app", app_edges);
    for (name, root, archive) in [("a", PACKAGE_X, "jx"), ("b", PACKAGE_Y, "jy")] {
        law.library(&format!("work/{name}"), &format!("local/{name}"), "1.0.0", "pub fn one() -> u64 { 1u64 }\n",
            &format!("dependencies: [(\"json\", Published(package: \"fixture.test/json\", major: 1, root: \"{root}\", locator: Archive(\"../vendor/{archive}.hpkg\"), authority: []))],"));
        law.write(&format!("work/vendor/{archive}.hpkg"), std::fs::read(law.path(&format!("vendor/{archive}.hpkg"))).unwrap());
    }
    PackageLaw::refuse(law.run(&["check", "work/app"]),
        &format!("fixture.test/json@1 is bound to more than one root:\n  local/app@1 -> local/a@1 -> fixture.test/json@1 = {PACKAGE_X} exact\n  local/app@1 -> local/b@1 -> fixture.test/json@1 = {PACKAGE_Y} exact\nno single root satisfies every edge; the exact edges of local/a@1, local/b@1 must name one root"));
    assert!(!law.path("work/hirsel.lock").exists());

    law.library("work/b", "local/b", "1.0.0", "pub fn one() -> u64 { 1u64 }\n",
        &format!("dependencies: [(\"json\", Published(package: \"fixture.test/json\", major: 1, root: \"{PACKAGE_X}\", locator: Archive(\"../vendor/jx.hpkg\"), authority: []))],"));
    PackageLaw::pass(law.run(&["check", "work/app"]));
    let lock = std::fs::read_to_string(law.path("work/hirsel.lock")).unwrap();
    assert_eq!(lock.matches("id = \"fixture.test/json@1\"").count(), 1);
    assert!(lock.contains(&format!("root = \"{PACKAGE_X}\"")));

    law.library("j2", "fixture.test/json", "2.0.0", PACKAGE_Y_SOURCE, "");
    let fixture = law.fixture("j2", "work/vendor/j2.hpkg");
    let (_, pin) = fixture.trim().rsplit_once(' ').unwrap();
    law.library("work/b", "local/b", "1.0.0", "pub fn one() -> u64 { 1u64 }\n",
        &format!("dependencies: [(\"json\", Published(package: \"fixture.test/json\", major: 2, root: \"{pin}\", locator: Archive(\"../vendor/j2.hpkg\"), authority: []))],"));
    PackageLaw::pass(law.run(&["check", "work/app"]));
    let lock = std::fs::read_to_string(law.path("work/hirsel.lock")).unwrap();
    assert_eq!(lock.matches("id = \"fixture.test/json@1\"").count(), 1);
    assert_eq!(lock.matches("id = \"fixture.test/json@2\"").count(), 1);
}

#[test]
fn package_l6_archive_closure_and_atomic_refusal() {
    let law = PackageLaw::new();
    law.fixture_json();
    law.app("app", &format!("dependencies: [(\"json\", Published(package: \"fixture.test/json\", major: 1, root: \"{PACKAGE_X}\", locator: Archive(\"vendor/jy.hpkg\"), authority: []))],"));
    let jy = std::fs::read(law.path("vendor/jy.hpkg")).unwrap();
    law.write("app/vendor/jy.hpkg", jy);
    PackageLaw::refuse(law.run(&["check", "app"]), &format!("fixture.test/json@1: archive:vendor/jy.hpkg holds {PACKAGE_Y}, but the binding pins {PACKAGE_X}"));
    law.empty_store();
    assert!(!law.path("app/hirsel.lock").exists());

    law.write("work/hirsel.hcfg", "pkg_v1.Workspace(members: [\"wrap\"])\n");
    law.write("work/vendor/jx.hpkg", std::fs::read(law.path("vendor/jx.hpkg")).unwrap());
    law.library("work/wrap", "fixture.test/wrap", "1.0.0", "pub fn one() -> u64 { 1u64 }\n",
        &format!("dependencies: [(\"json\", Published(package: \"fixture.test/json\", major: 1, root: \"{PACKAGE_X}\", locator: Archive(\"../vendor/jx.hpkg\"), authority: []))],"));
    assert_eq!(law.fixture("work/wrap", "app/vendor/wrap.hpkg"), format!("fixture.test/wrap@1 1.0.0 {PACKAGE_W}\nfixture.test/json@1 1.0.0 {PACKAGE_X}\n"));
    let w = law.encoded(PACKAGE_W);
    assert_eq!(w.len(), 141);
    let x = law.encoded(PACKAGE_X);
    let y = law.encoded(PACKAGE_Y);
    let correct = std::fs::read(law.path("app/vendor/wrap.hpkg")).unwrap();
    law.app("app", &format!("dependencies: [(\"wrap\", Published(package: \"fixture.test/wrap\", major: 1, root: \"{PACKAGE_W}\", locator: Archive(\"vendor/wrap.hpkg\"), authority: []))],"));
    let missing = format!("fixture.test/wrap@1: archive:vendor/wrap.hpkg lacks {PACKAGE_X}, which fixture.test/wrap@1 binds as fixture.test/json@1");
    for (entries, refusal) in [
        (vec![w.as_slice(), y.as_slice()], missing.as_str()),
        (vec![w.as_slice(), x.as_slice(), y.as_slice()], &format!("fixture.test/wrap@1: archive:vendor/wrap.hpkg holds {PACKAGE_Y}, which nothing in it binds")),
    ] {
        law.write("app/vendor/wrap.hpkg", package_bundle(&entries));
        PackageLaw::refuse(law.run(&["check", "app"]), refusal);
        law.empty_store();
    }
    law.write(&package_entry("packages", PACKAGE_X), &x);
    law.write("app/vendor/wrap.hpkg", package_bundle(&[&w]));
    PackageLaw::refuse(law.run(&["check", "app"]), &missing);
    assert_eq!(std::fs::read_dir(law.path("packages")).unwrap().count(), 1);
    assert!(!law.path("app/hirsel.lock").exists());

    law.write("app/vendor/wrap.hpkg", correct);
    PackageLaw::pass(law.run(&["check", "app"]));
    assert_eq!(std::fs::read_dir(law.path("packages")).unwrap().count(), 2);
    // Removing a transitive entry must reopen the carrier even when W is warm.
    std::fs::remove_file(law.path(&package_entry("packages", PACKAGE_X))).unwrap();
    PackageLaw::pass(law.run(&["check", "app"]));
    assert_eq!(std::fs::read(law.path(&package_entry("packages", PACKAGE_X))).unwrap(), x);
}

#[test]
fn package_l11_store_hits_validate_every_incoming_edge() {
    let law = PackageLaw::new();
    law.library("jx", "fixture.test/json", "1.0.0", PACKAGE_X_SOURCE, "");
    PackageLaw::pass(law.run(&["pack", "jx", "--output", "jx.hpkg"]));
    let yaml = format!("(\"yaml\", Published(package: \"fixture.test/yaml\", major: 1, root: \"{PACKAGE_X}\", locator: Store, authority: []))");
    let expected = format!("fixture.test/yaml@1: {PACKAGE_X} is fixture.test/json@1");
    law.app("app", &format!("dependencies: [{yaml}],"));
    PackageLaw::refuse(law.run(&["lock", "app"]), &expected);
    law.app("app", &format!("dependencies: [(\"json\", Published(package: \"fixture.test/json\", major: 1, root: \"{PACKAGE_X}\", locator: Archive(\"vendor/jx.hpkg\"), authority: [])), {yaml}],"));
    PackageLaw::refuse(law.run(&["lock", "app"]), &expected);
    assert!(!law.path("app/hirsel.lock").exists());
}

#[test]
fn check_reports_definition_count_on_a_request_hit() {
    let law = PackageLaw::new();
    law.app("app", "");
    law.write("app/main.hirsel", format!("type Pair {{ a: u64, b: u64 }}\n{PACKAGE_MAIN}"));
    let core = law.path("core-product");
    for _ in 0..2 {
        let stdout = PackageLaw::pass(law.run(&["check", "app", "--core-product", core.to_str().unwrap()]));
        assert_eq!(stdout, "accepted: 1 files, 2 checked definitions\n");
    }
}

#[test]
fn package_l13_explicit_image_uses_only_the_closure() {
    let law = PackageLaw::new();
    law.manifest("core_only", "local/core_only", "1.0.0", "sources: [(\"main.hirsel\", \"main\")], dependencies: [(\"std\", Toolchain(package: \"std\", major: 1, authority: []))],");
    law.write("core_only/main.hirsel", PACKAGE_MAIN);
    let mut request = CheckRequest::program(&law.path("core_only"), &law.libraries, &law.path("image")).unwrap();
    request.entry = None;
    let (_, image) = Driver::default().produce_image(request, &mut Meter::new(false)).unwrap();
    assert!(image.is_some());

    law.manifest("core_only", "local/core_only", "1.0.0", "sources: [(\"main.hirsel\", \"main\")], dependencies: [],");
    law.write("core_only/main.hirsel", "pub fn main() -> u64 { std.x }\n");
    let (status, stdout, stderr) = law.run(&["check", "core_only", "--json"]);
    assert_eq!(status, Some(1), "{stdout}\n{stderr}");
    let result: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert!(result["diagnostics"].as_array().unwrap().iter().any(|row| row["message"].as_str().is_some_and(|text| text.contains("std"))));
    PackageLaw::refuse(law.run(&["check", "core_only", "--image", "image"]), "--image image does not hold exactly this request's packages");

    let prefix = law.path("installed");
    let std = crate::PackageProduct::bundled_std(&prefix.join("images"), &mut Meter::new(false)).unwrap();
    crate::installation::register(&prefix, "std", &std.image, [], None).unwrap();
    let binary = prefix.join("bin/hirsel");
    std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
    let harness = std::env::current_exe().unwrap();
    if std::fs::hard_link(&harness, &binary).is_err() {
        std::fs::copy(&harness, &binary).unwrap();
    }
    let args = vec![
        "check".into(), "core_only".into(), "--json".into(),
        "--package-store".into(), law.path("packages").to_str().unwrap().into(),
        "--store".into(), law.path("compiler-store").to_str().unwrap().into(),
    ];
    let template = process::tape_request_command(&args, law.dir.path());
    let output = std::process::Command::new(&binary)
        .args(template.get_args())
        .envs(template.get_envs().filter_map(|(name, value)| value.map(|value| (name, value))))
        .env("XDG_CACHE_HOME", law.path("cache"))
        .current_dir(law.dir.path())
        .output().unwrap();
    assert_eq!(output.status.code(), Some(1), "{}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8(output.stdout).unwrap();
    let (_, stdout) = stdout.split_once(process::CHILD_MARK).unwrap();
    let installed: serde_json::Value = serde_json::from_str(stdout).unwrap();
    assert_eq!(installed["diagnostics"], result["diagnostics"]);
}

fn package_lock_fixture(root: &str, archive: &str) -> String {
    let version = if archive == "jy" { "1.0.1" } else { "1.0.0" };
    format!(concat!(
        "# hirsel.lock: written by hirsel; do not edit.\n",
        "version = 2\n",
        "\n[[package]]\nid = \"fixture.test/json@1\"\nversion = \"{}\"\nroot = \"{}\"\nsource = \"archive:vendor/{}.hpkg\"\nedges = []\n",
        "\n[[package]]\nid = \"local/app@1\"\nversion = \"1.0.0\"\nsource = \"member:.\"\nedges = [\"fixture.test/json@1\", \"std@1\"]\n",
        "\n[[package]]\nid = \"std@1\"\nsource = \"toolchain\"\nedges = []\n"
    ), version, root, archive)
}

#[test]
fn package_l2_stale_locks_are_diffs_of_canonical_rows() {
    let law = PackageLaw::new();
    law.fixture_json();
    for archive in ["jx", "jy"] {
        let path = format!("vendor/{archive}.hpkg");
        law.write(&format!("app/{path}"), std::fs::read(law.path(&path)).unwrap());
    }
    let bind = |root: &str, archive: &str| {
        law.app("app", &format!(r#"dependencies: [
    ("json", Published(
        package: "fixture.test/json",
        major: 1,
        root: "{root}",
        locator: Archive("vendor/{archive}.hpkg"),
        authority: [],
    )),
    ("std", Toolchain(package: "std", major: 1, authority: [])),
],"#));
    };
    let path = law.path("app/hirsel.lock");
    bind(PACKAGE_X, "jx");
    PackageLaw::pass(law.run(&["lock", "app"]));
    let original = package_lock_fixture(PACKAGE_X, "jx");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    bind(PACKAGE_Y, "jy");
    let diff = format!("  fixture.test/json@1: version 1.0.0 -> 1.0.1\n  fixture.test/json@1: root {PACKAGE_X} -> {PACKAGE_Y}\n  fixture.test/json@1: source archive:vendor/jx.hpkg -> archive:vendor/jy.hpkg");
    PackageLaw::refuse(law.run(&["check", "app", "--locked"]), &format!("hirsel.lock is stale under --locked:\n{diff}"));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    let ordinary = law.run(&["check", "app"]);
    assert_eq!(ordinary.2, format!("hirsel: updated hirsel.lock:\n{diff}\n"));
    PackageLaw::pass(ordinary);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), package_lock_fixture(PACKAGE_Y, "jy"));

    law.write("work/hirsel.hcfg", "pkg_v1.Workspace(members: [\"app\", \"util\"])\n");
    law.app("work/app", "dependencies: [(\"util\", Path(package: \"local/util\", major: 1, path: \"../util\", authority: []))],");
    for archive in ["jx", "jy"] {
        let path = format!("vendor/{archive}.hpkg");
        law.write(&format!("work/{path}"), std::fs::read(law.path(&path)).unwrap());
    }
    let util = |pin: &str, archive: &str| {
        law.library("work/util", "local/util", "1.0.0", "pub fn one() -> u64 { 1u64 }\n",
            &format!("dependencies: [(\"json\", Published(package: \"fixture.test/json\", major: 1, root: \"{pin}\", locator: Archive(\"../vendor/stale/../{archive}.hpkg\"), authority: []))],"));
    };
    util(PACKAGE_X, "jx");
    PackageLaw::pass(law.run(&["lock", "work"]));
    util(PACKAGE_Y, "jy");
    PackageLaw::refuse(law.run(&["check", "work/app", "--locked"]), &format!("hirsel.lock is stale under --locked:\n{diff}"));
}

#[test]
fn package_l3_frozen_does_not_open_an_archive() {
    let law = PackageLaw::new();
    law.fixture_json();
    let url = "https://hirsel.invalid/jx.hpkg";
    law.app("app", &format!("dependencies: [(\"json\", Published(package: \"fixture.test/json\", major: 1, root: \"{PACKAGE_X}\", locator: Url(\"{url}\"), authority: [])), (\"std\", Toolchain(package: \"std\", major: 1, authority: []))],"));
    PackageLaw::pass(law.with_store(&["lock", "app"], "fixtures"));
    let refusal = |status: &str, source: &str| format!("fixture.test/json@1 {PACKAGE_X} {status} the package store, and --frozen does not fetch (source {source})");
    PackageLaw::refuse(law.run(&["check", "app", "--frozen"]), &refusal("is not in", &format!("url:{url}")));
    law.empty_store();
    let mut damaged = law.encoded(PACKAGE_X);
    damaged[0] ^= 1;
    let store_path = package_entry("packages", PACKAGE_X);
    law.write(&store_path, damaged);
    PackageLaw::refuse(law.run(&["check", "app", "--frozen"]), &refusal("is damaged in", &format!("url:{url}")));
    std::fs::remove_file(law.path(&store_path)).unwrap();
    std::fs::remove_dir(law.path(&store_path).parent().unwrap()).unwrap();
    law.app("app", &format!(r#"dependencies: [
    ("json", Published(
        package: "fixture.test/json",
        major: 1,
        root: "{PACKAGE_X}",
        locator: Archive("vendor/jx.hpkg"),
        authority: [],
    )),
    ("std", Toolchain(package: "std", major: 1, authority: [])),
],"#));
    let archive = "app/vendor/jx.hpkg";
    law.write(archive, std::fs::read(law.path("vendor/jx.hpkg")).unwrap());
    PackageLaw::pass(law.with_store(&["lock", "app"], "fixtures"));
    PackageLaw::pass(law.with_store(&["check", "app", "--locked"], "third-store"));
    assert_eq!(std::fs::read(law.path(&package_entry("third-store", PACKAGE_X))).unwrap(), law.encoded(PACKAGE_X));
    let fifo = law.path(archive);
    std::fs::remove_file(&fifo).unwrap();
    let path = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: the path is a live C string naming a vacant leaf in our tempdir.
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    let (started, ready) = std::sync::mpsc::channel();
    let (opened, finished) = std::sync::mpsc::channel();
    let writer_path = fifo.clone();
    let writer = std::thread::spawn(move || {
        started.send(()).unwrap();
        let _file = std::fs::OpenOptions::new().write(true).open(writer_path).unwrap();
        opened.send(()).unwrap();
    });
    ready.recv().unwrap();
    let result = law.run(&["check", "app", "--frozen"]);
    let touched = finished.try_recv().is_ok();
    if !touched {
        use std::os::unix::fs::OpenOptionsExt;
        let _reader = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(fifo).unwrap();
        finished.recv().unwrap();
    }
    writer.join().unwrap();
    PackageLaw::refuse(result, &refusal("is not in", "archive:vendor/jx.hpkg"));
    assert!(!touched);
    law.empty_store();
}

#[test]
fn package_l10_coalesced_member_keeps_the_published_obligation() {
    let law = PackageLaw::new();
    law.fixture_json();
    law.write("work/hirsel.hcfg", "pkg_v1.Workspace(members: [\"app\", \"json\"])\n");
    for archive in ["jx", "jy"] {
        let path = format!("vendor/{archive}.hpkg");
        law.write(&format!("work/{path}"), std::fs::read(law.path(&path)).unwrap());
    }
    let bind = |root: &str, archive: &str, source: &str| {
        let version = if archive == "jy" { "1.0.1" } else { "1.0.0" };
        law.library("work/json", "fixture.test/json", version, source, "");
        law.app("work/app", &format!("dependencies: [(\"json\", Published(package: \"fixture.test/json\", major: 1, root: \"{root}\", locator: Archive(\"../vendor/{archive}.hpkg\"), authority: []))],"));
    };
    bind(PACKAGE_X, "jx", PACKAGE_X_SOURCE);
    PackageLaw::pass(law.run(&["lock", "work"]));
    let lock = law.path("work/hirsel.lock");
    let original = format!(concat!(
        "# hirsel.lock: written by hirsel; do not edit.\nversion = 2\n",
        "\n[[package]]\nid = \"fixture.test/json@1\"\nversion = \"1.0.0\"\nroot = \"{}\"\nsource = \"member:json\"\nedges = []\n",
        "\n[[package]]\nid = \"local/app@1\"\nversion = \"1.0.0\"\nsource = \"member:app\"\nedges = [\"fixture.test/json@1\"]\n"
    ), PACKAGE_X);
    assert_eq!(std::fs::read_to_string(&lock).unwrap(), original);

    bind(PACKAGE_Y, "jy", PACKAGE_Y_SOURCE);
    PackageLaw::refuse(law.run(&["check", "work/app", "--locked"]), &format!("hirsel.lock is stale under --locked:\n  fixture.test/json@1: version 1.0.0 -> 1.0.1\n  fixture.test/json@1: root {PACKAGE_X} -> {PACKAGE_Y}"));
    assert_eq!(std::fs::read_to_string(&lock).unwrap(), original);
    law.library("work/json", "fixture.test/json", "1.0.0", PACKAGE_X_SOURCE, "");
    law.app("work/app", "dependencies: [(\"json\", Path(package: \"fixture.test/json\", major: 1, path: \"../json\", authority: []))],");
    PackageLaw::refuse(law.run(&["check", "work/app", "--locked"]), &format!("hirsel.lock is stale under --locked:\n  fixture.test/json@1: root {PACKAGE_X} -> none"));
    assert_eq!(std::fs::read_to_string(&lock).unwrap(), original);
}

#[test]
fn package_member_tests_keep_capabilities() {
    let law = PackageLaw::new();
    law.write("work/hirsel.hcfg", "pkg_v1.Workspace(members: [\"app\", \"lib\"])\n");
    law.library("work/lib", "local/lib", "1.0.0", "@test\nfn dependency_only() -> Result[(), ()] { Error(()) }\n", "");
    law.app("work/app", "dependencies: [(\"lib\", Path(package: \"local/lib\", major: 1, path: \"../lib\", authority: []))], tests: Some(Tests(sources: [(\"local.hirsel\", \"local\")], limit: 67108864, grants: [Stdout], runs: [])),");
    law.write("work/app/local.hirsel", "@test\nfn capacity(out: Stdout) -> Result[(), WriteError] waits { stdout.write(out, \"own test\") }\n");
    for _ in 0..2 {
        let stdout = PackageLaw::pass(law.run(&["test", "work/app", "--json"]));
        let results: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        let tests = results["tests"].as_array().unwrap();
        assert_eq!(tests.len(), 1);
        assert_eq!(tests[0]["definition"], "local/app@1/local/capacity");
        assert_eq!(tests[0]["source"], "executed");
        assert_eq!(tests[0]["passed"], true);
    }
}

#[test]
fn package_foreign_sources_are_marked_in_both_origins() {
    let law = PackageLaw::new();
    law.manifest("native", "fixture.test/shim", "1.0.0", r#"sources: [("api.hirsel", "api"), ("c.hirsel", "c")], dependencies: [], foreign: Some([("c", Native(symbols: "hh_", archives: []))]),"#);
    law.write("native/api.hirsel", "pub opaque type Api {}\n");
    law.write("native/c.hirsel", "pub fn probe(f: Foreign[api.Api]) -> u64\n");
    let packed = law.fixture("native", "shim.hpkg");
    let (_, root) = packed.trim().rsplit_once(' ').unwrap();
    law.app("app", &format!("dependencies: [(\"shim\", Published(package: \"fixture.test/shim\", major: 1, root: \"{root}\", locator: Archive(\"shim.hpkg\"), authority: []))],"));
    law.write("app/shim.hpkg", std::fs::read(law.path("shim.hpkg")).unwrap());

    let member = crate::package_dir::PackageDir::open(&law.path("native")).unwrap();
    let workspace = crate::package_workspace::Workspace::discover(&law.path("app")).unwrap().unwrap();
    let selected = workspace.selected().unwrap().clone();
    let library = crate::package_resolve::Resolution::resolve(
        workspace, crate::package_store::PackageStore::new(Some(law.path("packages"))), false,
    ).unwrap();
    let consumer = crate::package_dir::PackageDir::from_resolution(&library, &selected, false).unwrap();
    for (package, expected_name) in [
        (&member.package, "c.hirsel"), (&consumer.dependencies[0], "fixture.test/shim@1/c.hirsel"),
    ] {
        let names: Vec<_> = package.sources.iter().filter(|source| source.foreign.is_some())
            .map(|source| source.captured.as_ref().unwrap().name.as_deref().unwrap()).collect();
        assert_eq!(names, [expected_name]);
    }
}

fn package_test_row(output: (Option<i32>, String, String), status: i32) -> serde_json::Value {
    assert_eq!(output.0, Some(status), "{}\n{}", output.1, output.2);
    let results: serde_json::Value = serde_json::from_str(&output.1).unwrap();
    let tests = results["tests"].as_array().unwrap();
    assert_eq!(tests.len(), 1);
    tests[0].clone()
}

#[test]
fn package_l7_pins_reach_the_check_and_test_keys() {
    let law = PackageLaw::new();
    law.fixture_json();
    let app_source = r#"
pub fn main() -> Result[(), Exit] {
    if json.version() == 1u64 { Ok(()) } else { Error(Exit(status: 3u8)) }
}
@test
fn version() -> Result[(), ()] { if json.version() > 0u64 { Ok(()) } else { Error(()) } }
"#;
    let select = |root: &str, archive: &str| {
        law.app("app", &format!(r#"dependencies: [
    ("json", Published(
        package: "fixture.test/json",
        major: 1,
        root: "{root}",
        locator: Archive("vendor/{archive}.hpkg"),
        authority: [],
    )),
], tests: Some(Tests(sources: [], limit: 67108864, grants: [], runs: [])),"#));
        law.write("app/main.hirsel", app_source);
        law.write(&format!("app/vendor/{archive}.hpkg"),
            std::fs::read(law.path(&format!("vendor/{archive}.hpkg"))).unwrap());
    };
    select(PACKAGE_X, "jx");
    let tested = || law.run(&["test", "app", "--json"]);
    let checked = |law: &PackageLaw| {
        let stdout = PackageLaw::pass(law.run(&["check", "app", "--json"]));
        serde_json::from_str::<serde_json::Value>(&stdout).unwrap()
    };
    let row = package_test_row(tested(), 0);
    assert_eq!(row["source"], "executed");
    let first = PackageLaw::pass(law.run(&["build", "--emit", "recipe", "app"]));
    let alternate = PackageLaw::pass(law.with_store(&["build", "--emit", "recipe", "app"], "alternate"));
    assert_eq!(first, alternate);
    select(PACKAGE_Y, "jy");
    let warm = checked(&law);
    let cold = PackageLaw::new();
    for file in ["hirsel.hcfg", "main.hirsel", "vendor/jy.hpkg"] {
        let file = format!("app/{file}");
        cold.write(&file, std::fs::read(law.path(&file)).unwrap());
    }
    assert_eq!(warm["rechecked"], checked(&cold)["rechecked"]);
    assert!(warm["rechecked"].as_u64().unwrap() > 0);
    let row = package_test_row(tested(), 0);
    assert_eq!(row["source"], "executed");
    let affected = |which| PackageLaw::pass(law.run(&["inspect", "app", "--affected-tests", which]));
    assert_eq!(affected("fixture.test/json@1"), "local/app@1/main/version\n");
    assert_eq!(affected("fixture.test/json@1/json/version"), "local/app@1/main/version\n");

    law.manifest("unused", "fixture.test/json", "1.0.0", "sources: [(\"json.hirsel\", \"json\"), (\"unused.hirsel\", \"unused\")], dependencies: [],");
    law.write("unused/json.hirsel", PACKAGE_Y_SOURCE);
    law.write("unused/unused.hirsel", "pub fn never_called() -> u64 { 70u64 }\n");
    let extra = law.fixture("unused", "vendor/unused.hpkg");
    let (_, root) = extra.trim().rsplit_once(' ').unwrap();
    select(root, "unused");
    let row = package_test_row(tested(), 0);
    assert_eq!(row["source"], "cached");
    assert_eq!(affected("fixture.test/json@1"), "local/app@1/main/version\n");

    law.write("work/hirsel.hcfg", "pkg_v1.Workspace(members: [\"app\", \"json\"])\n");
    law.library("work/json", "fixture.test/json", "1.0.0", PACKAGE_X_SOURCE, "");
    law.app("work/app", "dependencies: [(\"json\", Path(package: \"fixture.test/json\", major: 1, path: \"../json\", authority: []))],");
    law.write("work/app/main.hirsel", app_source);
    assert_eq!(PackageLaw::pass(law.run(&["build", "--emit", "recipe", "work/app"])), first);
}

#[test]
fn package_l9_moved_sites_end_a_pass_without_changing_the_address() {
    let law = PackageLaw::new();
    let initial = "pub fn quot(x: u64, y: u64, z: u64) -> u64 { x / y / z }\n";
    let edited = initial.replace("x / y / z", "(x / y) / z");
    let source = r#"
fn site(take outcome: Result[Result[u64, Causes], Capacity]) -> Option[Site] {
    match take outcome {
        Ok(Error(take causes)) => {
            for cause in causes { return Some(cause.site) }
            None
        },
        _ => None
    }
}
@test
fn same_site() -> Result[(), ()] {
    let a = site(isolate(fn() -> u64 { math.quot(8u64, 0u64, 1u64) }))
    let b = site(isolate(fn() -> u64 { math.quot(8u64, 1u64, 0u64) }))
    match (a, b) {
        (Some(x), Some(y)) if x == y => Ok(()),
        _ => Error(())
    }
}
"#;
    let write_test = |law: &PackageLaw, root: &str, archive: &str| {
        law.app("app", &format!(r#"dependencies: [
    ("math", Published(
        package: "fixture.test/math",
        major: 1,
        root: "{root}",
        locator: Archive("{archive}"),
        authority: [],
    )),
], tests: Some(Tests(sources: [("tests.hirsel", "tests")], limit: 67108864, grants: [], runs: [])),"#));
        law.write("app/tests.hirsel", source);
    };
    let bind = |law: &PackageLaw, fixture: &str, text: &str| {
        law.library(fixture, "fixture.test/math", "1.0.0", text, "");
        std::fs::create_dir_all(law.path("app")).unwrap();
        let archive = format!("{fixture}.hpkg");
        let result = law.fixture(fixture, &format!("app/{archive}"));
        let (_, root) = result.trim().rsplit_once(' ').unwrap();
        write_test(law, root, &archive);
    };
    bind(&law, "a", initial);
    let test = |law: &PackageLaw| law.run(&["test", "app", "--json"]);
    let address = || {
        let workspace = crate::package_workspace::Workspace::discover(&law.path("app")).unwrap().unwrap();
        let selected = workspace.selected().unwrap().clone();
        let resolved = crate::package_resolve::Resolution::resolve(workspace,
            crate::package_store::PackageStore::new(Some(law.path("packages"))), false).unwrap();
        let app = crate::package_dir::PackageDir::from_resolution(&resolved, &selected, true).unwrap();
        let mut request = CheckRequest::from_app(app, Some(&law.libraries), &law.path("address-store")).unwrap();
        request.entry = None;
        let product = Driver::default().check(request, &mut Meter::new(false)).unwrap();
        let path = DefPath::from_full("local/app@1/tests/same_site").unwrap();
        product.reachable(&path, 65536).unwrap().address(&path).unwrap()
    };
    let a = package_test_row(test(&law), 0);
    assert_eq!(a["source"], "executed");
    let a_address = address();
    bind(&law, "b", &edited);
    let warm = package_test_row(test(&law), 1);
    assert_eq!(warm["source"], "executed");
    assert_eq!(warm["passed"], false);
    assert!(warm["stopped"].is_null(), "{warm}");
    assert_eq!(address(), a_address);

    let cold = PackageLaw::new();
    bind(&cold, "b", &edited);
    let cold = package_test_row(test(&cold), 1);
    assert_eq!(cold["source"], "executed");
    assert_eq!(cold["passed"], false);
    assert!(cold["stopped"].is_null(), "{cold}");
}

#[test]
fn package_l14_a_rename_reaches_images_sites_and_the_recipe() {
    let law = PackageLaw::new();
    let set_up = |law: &PackageLaw, name: &str| {
        law.write("work/hirsel.hcfg", "pkg_v1.Workspace(members: [\"app\", \"json\"])\n");
        law.app("work/app", "dependencies: [(\"json\", Path(package: \"fixture.test/json\", major: 1, path: \"../json\", authority: []))],");
        law.write("work/app/main.hirsel", "pub fn main() -> Result[(), Exit] {\n    let value = json.half(0u64)\n    if value == 0u64 { Ok(()) } else { Error(Exit(status: 1u8)) }\n}\n");
        law.manifest("work/json", "fixture.test/json", "1.0.0", &format!("sources: [(\"{name}.hirsel\", \"json\")], dependencies: [],"));
        law.write(&format!("work/json/{name}.hirsel"), "pub fn half(x: u64) -> u64 { 8u64 / x }\n");
    };
    set_up(&law, "old");
    PackageLaw::pass(law.run(&["check", "work/app"]));
    assert_eq!(std::fs::read_dir(law.path("compiler-store/package-images")).unwrap().count(), 1);
    let old = PackageLaw::pass(law.run(&["build", "--emit", "recipe", "work/app"]));
    std::fs::remove_file(law.path("work/json/old.hirsel")).unwrap();
    set_up(&law, "new");
    let (status, stdout, stderr) = law.run(&["run", "work/app"]);
    assert_ne!(status, Some(0), "{stdout}\n{stderr}");
    assert!(stderr.contains("fixture.test/json@1/new.hirsel"), "{stdout}\n{stderr}");
    let renamed = PackageLaw::pass(law.run(&["build", "--emit", "recipe", "work/app"]));
    assert_ne!(renamed, old);
    let cold = PackageLaw::new();
    set_up(&cold, "new");
    assert_eq!(renamed, PackageLaw::pass(cold.run(&["build", "--emit", "recipe", "work/app"])));
}

#[test]
fn package_manifest_refusals_follow_workspace_member_and_key_order() {
    let law = PackageLaw::new();
    law.write(
        "work/hirsel.hcfg",
        "pkg_v1.Workspace(members: [\"zeta\", \"alpha\"])\n",
    );
    law.write("work/zeta/hirsel.hcfg", "pkg_v1.Package(zzz: true, aaa: true)\n");
    law.library("work/alpha", "Alpha", "1.0.0", PACKAGE_X_SOURCE, "");

    for directory in ["work", "work/zeta"] {
        PackageLaw::refuse(
            law.run(&["lock", directory]),
            concat!(
                "work/zeta/hirsel.hcfg: Invalid(\"unknown field\") at  (bytes 26..35)\n",
                "hirsel: work/zeta/hirsel.hcfg: Invalid(\"unknown field\") at  (bytes 15..24)\n",
                "hirsel: work/alpha/hirsel.hcfg: package.name: \"Alpha\" is not ",
                "a scoped or local name",
            ),
        );
        assert!(!law.path("work/hirsel.lock").exists());
    }
}

#[test]
fn package_pack_output_cannot_alias_a_fetched_archive() {
    let law = PackageLaw::new();
    law.fixture_json();
    law.library("work", "fixture.test/wrap", "1.0.0",
        "pub fn answer() -> u64 { 42 }\n",
        &format!("dependencies: [(\"json\", Published(package: \"fixture.test/json\", major: 1, root: \"{PACKAGE_X}\", locator: Archive(\"json.hpkg\"), authority: []))],"),
    );
    std::fs::rename(law.path("vendor/jx.hpkg"), law.path("work/json.hpkg")).unwrap();
    std::fs::hard_link(law.path("work/json.hpkg"), law.path("output.hpkg")).unwrap();
    let original = std::fs::read(law.path("output.hpkg")).unwrap();
    PackageLaw::refuse(
        law.run(&["pack", "work", "--output", "output.hpkg"]),
        "--output output.hpkg is an input of this command",
    );
    assert_eq!(std::fs::read(law.path("output.hpkg")).unwrap(), original);
    assert!(!law.path("work/hirsel.lock").exists());
}

#[test]
fn package_pack_output_cannot_alias_an_input() {
    let law = PackageLaw::new();
    law.library("jx", "fixture.test/json", "1.0.0", PACKAGE_X_SOURCE, "");
    std::fs::hard_link(law.path("jx/json.hirsel"), law.path("linked.hpkg")).unwrap();
    let store_entry = package_entry("packages", PACKAGE_X);
    for out in ["jx/hirsel.hcfg", "linked.hpkg", "jx/hirsel.lock"] {
        PackageLaw::refuse(law.run(&["pack", "jx", "--output", out]), &format!("--output {out} is an input of this command"));
        assert!(!law.path("jx/hirsel.lock").exists());
        law.empty_store();
    }
    std::fs::create_dir_all(law.path(&store_entry).parent().unwrap()).unwrap();
    PackageLaw::refuse(law.run(&["pack", "jx", "--output", &store_entry]),
        &format!("--output {store_entry} is an input of this command"));
    assert!(!law.path(&store_entry).exists());
    assert!(!law.path("jx/hirsel.lock").exists());
    assert_eq!(std::fs::read_to_string(law.path("linked.hpkg")).unwrap(), PACKAGE_X_SOURCE);
}

#[test]
fn package_foreign_stamp_uses_captured_bytes_and_every_target() {
    let law = PackageLaw::new();
    let target = "x86_64-unknown-linux-gnu";
    let write = |symbols: &str, extra: &str| {
        law.manifest("math", "fixture.test/math", "1.0.0", &format!(r#"
sources: [("native.hirsel", "native")], dependencies: [],

foreign: Some([("native", Native(symbols: "{symbols}", archives: [
  ArchiveFile(target: "{target}", file: "native/x86.a", sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"),
  {extra}
]))]),
"#));
    };
    law.write("math/native.hirsel", "pub fn ping(f: Foreign[Api]) -> u64\n");
    law.write("math/native/x86.a", b"abc");
    write("hm_", "");
    let checked = || crate::package_dir::PackageDir::open(&law.path("math")).unwrap().package.namespace.unwrap();
    let original = checked();

    write("hn_", "");
    assert_ne!(checked().foreign, original.foreign);
    law.write("math/native/arm.a", b"");
    write("hm_", r#"ArchiveFile(target: "aarch64-unknown-linux-gnu", file: "native/arm.a", sha256: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"),"#);
    assert_ne!(checked().foreign, original.foreign);

    write("hm_", "");
    law.write("math/native/x86.a", b"abd");
    assert_ne!(checked().foreign, original.foreign);
    assert_eq!(original.foreign_inputs[&vec!["native".to_string()]].archives().unwrap()[target].bytes.as_ref(), b"abc");
}

#[test]
fn package_l8_foreign_metadata_matches_cold_requests() {
    use sha2::{Digest, Sha256};

    let law = PackageLaw::new();
    let runtime = native_options(Backend::Cranelift, law.dir.path());
    let Link::Driver(cc) = runtime.link else { unreachable!() };
    law.write("adapter.c", r#"
#include <stdint.h>
uint64_t PING(void) { return UINT64_C(1); }

#define TEXT_(x) #x
#define TEXT(x) TEXT_(x)
#define ABI TEXT(PING) " stack () -> u64\n"
__attribute__((used, section(".hirsel_abi")))
static const char abi[sizeof(ABI) - 1] = ABI;
"#.replace(".hirsel_abi", fixture_abi_section()));
    std::fs::create_dir_all(law.path("work/math/native")).unwrap();
    let archive = |prefix: &str| {
        let compiled = Command::new(&cc).current_dir(law.dir.path()).args([
            "-std=c11", "-O2", "-fPIC", "-ffreestanding", "-fno-unwind-tables",
            "-fno-asynchronous-unwind-tables", "-fno-addrsig", "-fno-ident",
            "-fno-stack-protector", &format!("-DPING={prefix}ping"), "-c", "adapter.c", "-o", "adapter.o",
        ]).output().unwrap();
        assert!(compiled.status.success(), "{}", String::from_utf8_lossy(&compiled.stderr));
        let path = format!("work/math/native/{prefix}.a");
        let archived = Command::new(cc.with_file_name("llvm-ar"))
            .current_dir(law.dir.path())
            .args(["rcs", &path, "adapter.o"])
            .output().unwrap();
        assert!(archived.status.success(), "{}", String::from_utf8_lossy(&archived.stderr));
        let bytes = std::fs::read(law.path(&path)).unwrap();
        hirsel_core_ids::hex::encode(&Sha256::digest(bytes))
    };
    let hm = archive("hm_");
    let hn = archive("hn_");
    law.write("work/math/native/arm.a", b"");

    law.write("work/hirsel.hcfg", "pkg_v1.Workspace(members: [\"app\", \"math\"])\n");
    law.app("work/app", "dependencies: [(\"math\", Path(package: \"fixture.test/math\", major: 1, path: \"../math\", authority: [Foreign]))],");
    law.write("work/app/main.hirsel", r#"
pub fn main(f: Foreign[math.Math]) -> Result[(), Exit] {
    if math.ping(f) == 1u64 { Ok(()) } else { Error(Exit(status: 1u8)) }
}
"#);
    law.write("work/math/math.hirsel", r#"
pub opaque type Math {}
pub fn ping(f: Foreign[Math]) -> u64 { native.ping(f) }
"#);
    law.write("work/math/native.hirsel", "pub fn ping(f: Foreign[math.Math]) -> u64\n");
    let configure = |symbols: Option<&str>, arm: bool| {
        let mut manifest = String::from("sources: [(\"math.hirsel\", \"math\"), (\"native.hirsel\", \"native\")], dependencies: [],");
        if let Some(symbols) = symbols {
            let pin = if symbols == "hm_" { &hm } else { &hn };
            manifest.push_str(&format!(r#"
foreign: Some([("native", Native(symbols: "{symbols}", archives: [
  ArchiveFile(target: "{target}", file: "native/{symbols}.a", sha256: "{pin}"),
"#, target = runtime.target));
            if arm {
                manifest.push_str(r#"  ArchiveFile(target: "aarch64-unknown-linux-gnu", file: "native/arm.a", sha256: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"),"#);
            }
            manifest.push_str("\n]))]),\n");
        }
        law.manifest("work/math", "fixture.test/math", "1.0.0", &manifest);
    };
    let cold = || {
        let fresh = PackageLaw::new();
        for file in [
            "work/hirsel.hcfg", "work/app/hirsel.hcfg", "work/app/main.hirsel",
            "work/math/hirsel.hcfg", "work/math/math.hirsel", "work/math/native.hirsel",
            "work/math/native/hm_.a", "work/math/native/hn_.a", "work/math/native/arm.a",
        ] {
            fresh.write(file, std::fs::read(law.path(file)).unwrap());
        }
        fresh
    };
    let check = |law: &PackageLaw| law.run(&["check", "work/app", "--json"]);
    let publish = |law: &PackageLaw| PackageLaw::pass(law.run(&[
        "build", "--emit", "recipe", "work/app", "--grant", "Foreign=fixture.test/math@1/math/Math",
    ]));

    configure(Some("hm_"), false);
    PackageLaw::pass(check(&law));
    let original = publish(&law);
    PackageLaw::pass(law.run(&["run", "work/app", "--source-core", "--source-std", "--grant", "Foreign=fixture.test/math@1/math/Math"]));
    configure(None, false);
    let warm = check(&law);
    let fresh = check(&cold());
    assert_eq!(warm.0, Some(1), "{}\n{}", warm.1, warm.2);
    assert_eq!(warm.0, fresh.0);
    let warm: serde_json::Value = serde_json::from_str(&warm.1).unwrap();
    let fresh: serde_json::Value = serde_json::from_str(&fresh.1).unwrap();
    assert_eq!(warm["diagnostics"], fresh["diagnostics"]);
    assert!(warm["diagnostics"].as_array().is_some_and(|rows| !rows.is_empty()));

    configure(Some("hn_"), false);
    let changed = publish(&law);
    assert_eq!(changed, publish(&cold()));
    assert_ne!(changed, original);

    configure(Some("hm_"), true);
    let second_target = publish(&law);
    assert_eq!(second_target, publish(&cold()));
    assert_ne!(second_target, original);

    law.write("work/math/native/hm_.a", b"abc");
    let build = |law: &PackageLaw| law.run(&[
        "build", "work/app", "--grant", "Foreign=fixture.test/math@1/math/Math", "-o", "linked-program",
    ]);
    let warm = build(&law);
    let fresh = build(&cold());
    assert_eq!(warm.0, Some(2), "{}\n{}", warm.1, warm.2);
    assert_eq!(warm.0, fresh.0);
    let after_lock = |stderr: &str| stderr.lines().filter(|line| !line.starts_with("hirsel: created hirsel.lock"))
        .collect::<Vec<_>>().join("\n");
    assert_eq!(after_lock(&warm.2), after_lock(&fresh.2));
    assert!(warm.2.contains("FFI-3: archive") && warm.2.contains("does not match its pin:"), "{}", warm.2);
    assert!(!law.path("linked-program").exists());
}

include!("test_run_sqlite_native.rs");

#[test]
fn foreign_archive_refusals_precede_the_linker() {
    use crate::code_store::ObjectKind;
    use hirsel_core_layout::Target;
    use std::{fs, os::unix::fs::PermissionsExt};

    // This matrix checks ELF thin archives and SHT_INIT_ARRAY, then links
    // the x86_64 native control and a second control with the Arm Linux sysroot.
    match crate::host::HOST_TARGET {
        Target::X86_64Linux => (),
        Target::Aarch64Linux | Target::Aarch64Darwin | Target::Aarch64Ios | Target::Aarch64IosSim => return,
    }

    const TARGET: &str = "x86_64-unknown-linux-gnu";
    const ARM: &str = "aarch64-unknown-linux-gnu";
    const EMPTY: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    const GRANT: &str = "Foreign=hirsel.ai/ffi_laws@1/laws/Laws";
    const PREFIX: &str = "FFI-3: archive laws.a of hirsel.ai/ffi_laws@1/native for x86_64-unknown-linux-gnu";

    let law = PackageLaw::new();
    let fixture = law.libraries.join("programs/ffi_laws");
    let original = fs::read_to_string(fixture.join("hirsel.hcfg")).unwrap();
    for file in ["laws.hirsel", "native.hirsel", "libffi_laws-aarch64-apple-darwin.a"] {
        law.write(&format!("ffi_laws/{file}"), fs::read(fixture.join(file)).unwrap());
    }
    law.write("ffi_laws/main.hirsel", r#"
pub fn main(f: Foreign[laws.Laws]) -> Result[(), Exit] {
    let _ = native.make(f)
    Ok(())
}
pub fn pure() -> Result[(), Exit] { Ok(()) }
pub fn retained(f: Foreign[laws.Laws]) -> Result[(), Exit] {
    let _ = match target.arch { X86_64 => 0u64, Aarch64 => native.made(f) }
    Ok(())
}
pub fn kept(f: Foreign[laws.Laws]) -> Result[(), Exit] {
    let a = target.arch
    let _ = match a { X86_64 => 0u64, Aarch64 => native.made(f) }
    Ok(())
}
pub fn refused() -> Result[(), Exit] {
    let _ = match target.arch { X86_64 => target.unsupported("needs aarch64"), Aarch64 => 0u64 }
    Ok(())
}
"#);
    law.write("ffi_laws/tests.hirsel", r#"
@test
fn ungranted() -> Result[(), ()] { Ok(()) }
@test
fn granted(f: Foreign[laws.Laws]) -> Result[(), ()] {
    let _ = native.make(f)
    Ok(())
}
"#);
    let pinned = fs::read(fixture.join("libffi_laws.a")).unwrap();
    let fixture_pin = ObjectKind::ForeignArchive.hash(&pinned).to_string();
    let repin = |bytes: &[u8], pin: &str| {
        law.write("ffi_laws/laws.a", bytes);
        law.write("ffi_laws/hirsel.hcfg", original.replace("libffi_laws.a", "laws.a")
            .replace(&fixture_pin, pin));
    };
    let pinned_options = native_options(Backend::Cranelift, law.dir.path());
    let Link::Driver(cc) = pinned_options.link else { unreachable!() };
    let ld = cc.with_file_name("ld.lld");
    let ar = cc.with_file_name("llvm-ar");
    let called = law.path("linker-called");
    // Only link execution leaves a marker; aarch64 takes startup files and libc from the pinned Arm toolchain, not the host.
    let arm = PathBuf::from(std::env::var_os("HIRSEL_AARCH64_SYSROOT").expect("the pinned AArch64 sysroot"));
    law.write("clang-wrapper", format!("#!/bin/sh\ncase \"$*\" in *--target={ARM}*) set -- --gcc-toolchain='{}' --sysroot='{}' \"$@\";; esac\nexec '{}' --ld-path='{}' \"$@\"\n", arm.parent().and_then(Path::parent).unwrap().display(), arm.display(), cc.display(), law.path("ld.lld").display()));
    law.write("ld.lld", format!("#!/bin/sh\n: > '{}'\nexec '{}' \"$@\"\n", called.display(), ld.display()));
    for script in ["clang-wrapper", "ld.lld"] {
        fs::set_permissions(law.path(script), fs::Permissions::from_mode(0o755)).unwrap();
    }
    let runtime = pinned_options.runtime.canonicalize().unwrap();
    let invoke = |action, engine, args: &[&str]| {
        let mut request = [
            action, "ffi_laws", "--backend", engine, "--cc", law.path("clang-wrapper").to_str().unwrap(),
            "--libraries", law.libraries.to_str().unwrap(), "--runtime", runtime.to_str().unwrap(),
            "--store", law.path("build").to_str().unwrap(),
            "--package-store", law.path("packages").to_str().unwrap(), "-o", "image",
        ].into_iter().map(str::to_owned).collect::<Vec<_>>();
        request.extend(args.iter().map(|arg| (*arg).to_owned()));
        process::tape_request(&request, law.dir.path())
    };
    let fail_build = |engine, target, expected: &str| {
        let (status, output, error) = invoke("build", engine, &["--target", target, "--grant", GRANT]);
        assert_eq!(status, Some(2), "{output}\n{error}");
        assert!(error.lines().any(|line| line == expected), "{error}");
        assert!(!called.exists(), "the linker ran before FFI-3 refusal");
    };
    for engine in ["cranelift", "llvm"] {
        repin(b"abc", EMPTY);
        fail_build(engine, TARGET, &format!("{PREFIX} does not match its pin: expected sha256 {EMPTY}, found sha256 {ABC}"));
        repin(b"abc", ABC);
        fail_build(engine, TARGET, &format!("{PREFIX} is not an indexed ar archive"));
        repin(&pinned, &fixture_pin);
        fail_build(engine, ARM, "FFI-3: hirsel.ai/ffi_laws@1/native has no archive for target aarch64-unknown-linux-gnu; its archives are for aarch64-apple-darwin, x86_64-unknown-linux-gnu");
    }

    law.write("ffi_laws/laws.c", r#"
extern long write(int, const void *, unsigned long);
void *hfl_make(void) { return 0; }
int hfl_free(void *p) { (void)p; return 0; }
__attribute__((constructor)) static void init(void) { write(2, "ctor\n", 5); }
"#);
    let dir = law.path("ffi_laws");
    let compile = |target| {
        command(Command::new(&cc).current_dir(&dir).args(["-target", target, "-fPIC", "-c", "laws.c", "-o", "laws.o"])).unwrap();
    };
    let archive = |flags| {
        command(Command::new(&ar).current_dir(&dir).args([flags, "candidate.a", "laws.o"])).unwrap();
        let bytes = fs::read(dir.join("candidate.a")).unwrap();
        fs::remove_file(dir.join("candidate.a")).unwrap();
        bytes
    };
    let pin_bytes = |bytes: Vec<u8>| repin(&bytes, &ObjectKind::ForeignArchive.hash(&bytes).to_string());
    compile(TARGET);
    let thin = archive("rcsT");
    let initializer = archive("rcs");
    let mut stale_index = initializer.clone();
    let member_symbol = {
        let ar = object::read::archive::ArchiveFile::parse(stale_index.as_slice()).unwrap();
        let member = ar.members().next().unwrap().unwrap();
        let bytes = member.data(stale_index.as_slice()).unwrap();
        let inside = bytes.windows(9).position(|word| word == b"hfl_make\0").unwrap();
        bytes.as_ptr().addr() - stale_index.as_ptr().addr() + inside
    };
    stale_index[member_symbol] = b'x';
    compile(ARM);
    let wrong_machine = archive("rcs");
    for engine in ["cranelift", "llvm"] {
        pin_bytes(thin.clone());
        fail_build(engine, TARGET, &format!("{PREFIX} is a thin archive; its members are not inside the pinned bytes"));
        pin_bytes(wrong_machine.clone());
        fail_build(engine, TARGET, &format!("{PREFIX}: member laws.o is not a relocatable object for {TARGET}"));
        pin_bytes(stale_index.clone());
        fail_build(engine, TARGET, &format!("{PREFIX} does not define hfl_make, declared at hirsel.ai/ffi_laws@1/native/make"));
        // PROG-6: FFI-3 checks the reach of retained code, so x86_64 needs no hfl_made for the
        // arm it rules out, and a scrutinee read through a local keeps both arms.
        let entry = |name| invoke("build", engine, &["--target", TARGET, "--grant", GRANT, "--entry", name]).2;
        let made = format!("{PREFIX} does not define hfl_made, declared at hirsel.ai/ffi_laws@1/native/made");
        let retained = entry("hirsel.ai/ffi_laws@1/main/retained");
        assert!(!retained.contains("FFI-3"), "{retained}");
        let _ = (fs::remove_file(&called), fs::remove_file(law.path("image")));
        let kept = entry("hirsel.ai/ffi_laws@1/main/kept");
        assert!(kept.lines().any(|line| line == made) && !called.exists(), "{kept}");
        // PROG-6: reaching `target.unsupported` refuses the build, warm store or not.
        let unsupported = format!("PROG-6: main.refused reaches target.unsupported(\"needs aarch64\") for target {TARGET}");
        for _ in 0..2 {
            let refused = entry("hirsel.ai/ffi_laws@1/main/refused");
            assert!(refused.lines().any(|line| line == unsupported), "{refused}");
        }
        pin_bytes(initializer.clone());
        let refusal = format!("{PREFIX}: member laws.o has section .init_array of type SHT_INIT_ARRAY, which is not an admitted object feature");
        fail_build(engine, TARGET, &refusal);
        let (status, output, error) = invoke("test", engine, &["--grant", GRANT, "--json"]);
        assert_eq!(status, Some(1), "{output}\n{error}");
        assert!(output.contains(&refusal), "{output}\n{error}");
        assert!(!output.contains("ctor") && !error.contains("ctor"), "{output}\n{error}");
        let rows: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert!(rows["tests"].as_array().unwrap().iter().any(|row| {
            row["definition"] == "hirsel.ai/ffi_laws@1/tests/ungranted" && row["passed"] == true
        }));
        fs::remove_file(&called).unwrap();
    }
    // FFI-3 Imports: an undefined symbol that no admitted input defines (the archives, the
    // runtime, the link closure's libraries) is refused before the linker runs; zlib's
    // deflate is in none of them.
    fs::write(dir.join("imports.c"), "extern int deflate(void *, int);\nint hfl_import(void) { return deflate(0, 0); }\n").unwrap();
    command(Command::new(&cc).current_dir(&dir).args(["-target", TARGET, "-fPIC", "-c", "imports.c", "-o", "imports.o"])).unwrap();
    fs::write(dir.join("candidate.a"), &pinned).unwrap();
    command(Command::new(&ar).current_dir(&dir).args(["rcs", "candidate.a", "imports.o"])).unwrap();
    let importing = fs::read(dir.join("candidate.a")).unwrap();
    fs::remove_file(dir.join("candidate.a")).unwrap();
    for engine in ["cranelift", "llvm"] {
        pin_bytes(importing.clone());
        fail_build(engine, TARGET, &format!("{PREFIX}: member imports.o imports deflate, which no admitted input defines"));
    }
    // The non-foreign control runs last, so the refusal rows above run even when it fails.
    for engine in ["cranelift", "llvm"] {
        let _ = (fs::remove_file(law.path("image")), fs::remove_dir_all(law.path("image")));
        let (pure_status, pure_output, pure_error) = invoke("build", engine, &["--target", ARM, "--entry", "hirsel.ai/ffi_laws@1/main/pure"]);
        assert!(!pure_error.contains("FFI-3"), "{pure_error}");
        assert!(called.exists(), "the non-foreign control did not reach the linker: {engine} {pure_status:?}\n{pure_output}\n{pure_error}");
        fs::remove_file(&called).unwrap();
    }
}

/// Bug: `hirsel run` of a `main` that returns `Error(e)` exited 1 with no
/// output, and then with only the driver's outcome line, so the value that
/// said what went wrong was lost. PROG-3: the run writes `error: ` and `e` as
/// Hirsel source, a `Secret` as `[secret]`, on both engines' entry glue.
#[test]
fn run_writes_main_error_value_then_names_the_outcome() {
    let law = PackageLaw::new();
    law.manifest("app", "local/app", "1.0.0", "sources: [(\"main.hirsel\", \"main\")], dependencies: [],");
    let types = "type ServeError = Configuration | Bind(port: u16)\n\
        type AppError = Serve(ServeError) | Config(file: String, key: Secret)\n";
    let cases = [
        (
            r#"pub fn main() -> Result[(), AppError] {
    let b = string.into_bytes(string.from("pencil"))
    let key = result.unwrap(secret.seal(take b, string.from("tls_send(beta.test)")))
    Error(Config(file: "a\tb.conf", key: key))
}
"#,
            r#"error: Config(file: "a\tb.conf", key: [secret])"#,
        ),
        (
            "pub fn main() -> Result[(), AppError] { Error(Serve(Configuration)) }\n",
            "error: Serve(Configuration)",
        ),
    ];
    for (main, line) in cases {
        law.write("app/main.hirsel", &format!("{types}{main}"));
        for engine in ["cranelift", "llvm"] {
            let (status, stdout, stderr) = law.run(&["run", "app", "--backend", engine]);
            assert_eq!(status, Some(1), "{engine}: {stdout}\n{stderr}");
            let stderr: Vec<_> = stderr
                .lines()
                .filter(|line| !line.starts_with("hirsel: created hirsel.lock"))
                .collect();
            assert_eq!(
                stderr,
                [line, "hirsel: main returned an error, exit status 1"],
                "{engine}"
            );
        }
    }
}

#[test]
fn simulation_runs_fresh_seeds_and_reproduces_one_failure() {
    let law = PackageLaw::new();
    law.manifest("app", "local/app", "1.0.0",
        "sources: [(\"main.hirsel\", \"main\")], dependencies: [(\"std\", Toolchain(package: \"std\", major: 1, authority: []))], tests: Some(Tests(sources: [], limit: 1048576, grants: [], runs: [])),",
    );
    law.write(
        "app/main.hirsel",
        r#"
@test
fn seeded(e: Env) -> Result[(), ()] {
    match std.sim.world.test_seed(e) {
        Some(0) => Ok(()),
        Some(1) => Error(()),
        Some(2) => Ok(()),
        _ => fail("unexpected simulation seed")
    }
}
"#,
    );
    let result = |args: &[&str]| {
        let (status, stdout, stderr) = law.run(args);
        assert_eq!(status, Some(1), "{stdout}\n{stderr}");
        serde_json::from_str::<serde_json::Value>(&stdout).unwrap()
    };
    let sweep = result(&["test", "app", "--sim", "--seeds", "3", "--json"]);
    let runs = sweep["tests"].as_array().unwrap();
    assert_eq!(runs.len(), 3);
    for (row, (seed, passed)) in runs.iter().zip([(0u64, true), (1, false), (2, true)]) {
        assert_eq!(row["seed"], seed);
        assert_eq!(row["source"], "executed");
        assert_eq!(row["passed"], passed);
        assert_eq!(row["status"], if passed { 0 } else { 1 });
    }
    let (status, stdout, stderr) = law.run(&["test", "app", "--seed", "1"]);
    assert_eq!(status, Some(1), "{stdout}\n{stderr}");
    assert!(
        stdout.contains("fail executed local/app@1/main/seeded (seed 1): returned an error, exit status 1"),
        "{stdout}"
    );
    assert_eq!(stdout.lines().count(), 1, "{stdout}");
}

const SEEDED_COVERAGE: &str = r#"
fn classify(s: u64) -> u64 {
    let mut out = 0u64
    if s == 1u64 { out = 1u64 }
    out
}
@test
fn seeded(e: Env) -> Result[(), ()] {
    match std.sim.world.test_seed(e) {
        Some(s) => if classify(s) == 0u64 { Ok(()) } else { Error(()) },
        None => fail("no simulation seed")
    }
}
"#;

/// Coverage merges by test and seed, and `--coverage-out` writes the profile
/// (`docs/coverage.md`, "The profile"). Seeds 0 to 2 run `classify`; by hand,
/// only seed 1 takes its `if`'s `yes` arm, and that run fails. So the entry
/// is reached by two passed runs and one failed, `yes` by the failed run
/// alone and `no` by the two passed ones. Lines are SEEDED_COVERAGE's, the
/// first being its leading break; `no` reaches the join and has no place.
#[test]
fn coverage_merges_seeds_and_writes_the_profile() {
    let law = PackageLaw::new();
    law.manifest("app", "local/app", "1.0.0",
        "sources: [(\"main.hirsel\", \"main\")], dependencies: [(\"std\", Toolchain(package: \"std\", major: 1, authority: []))], tests: Some(Tests(sources: [], limit: 1048576, grants: [], runs: [])),",
    );
    law.write("app/main.hirsel", SEEDED_COVERAGE);
    let out = law.path("profile.json");
    let (status, stdout, stderr) =
        law.run(&["test", "app", "--sim", "--seeds", "3", "--json", "--coverage-out", out.to_str().unwrap()]);
    assert_eq!(status, Some(1), "{stdout}\n{stderr}");
    let report: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let profile: serde_json::Value = serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap();
    assert_eq!(profile["hirsel_coverage"], 3);
    assert_eq!((&profile["runs"], &profile["failed_runs"], &profile["unobserved_runs"]), (&3.into(), &1.into(), &0.into()));
    assert_eq!((&profile["selection"]["tests"], &profile["selection"]["runs"]), (&1.into(), &3.into()));
    assert_eq!(report["coverage"]["selection"], profile["selection"]);
    let test = &profile["tests"].as_array().unwrap()[0];
    assert_eq!(test["definition"], "local/app@1/main/seeded");
    assert_eq!((&test["seeds"], &test["runs"], &test["failed_runs"]), (&serde_json::json!([[0, 2]]), &3.into(), &1.into()));
    for row in report["tests"].as_array().unwrap() {
        assert!(row["coverage"]["functions"].as_u64().unwrap() >= 2, "{row}");
    }
    let sites: BTreeMap<String, &serde_json::Value> = profile["sites"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| (row["site"].as_str().unwrap().to_owned(), row))
        .collect();
    let item = &SEEDED_COVERAGE[SEEDED_COVERAGE.find("fn classify").unwrap()..];
    let start = item.find("s == 1u64").unwrap();
    let decision = format!("local/app@1/main/classify@{start}-{}.0", start + "s == 1u64".len());
    let count = |key: &str| {
        let row = sites[key];
        (row["passed_runs"].as_u64().unwrap(), row["failed_runs"].as_u64().unwrap())
    };
    assert_eq!(count("local/app@1/main/classify#e"), (2, 1));
    assert_eq!(count(&format!("{decision}:yes")), (0, 1));
    assert_eq!(count(&format!("{decision}:no")), (2, 0));
    for (arm, arm_line) in [("yes", Some(4)), ("no", None)] {
        let row = sites[&format!("{decision}:{arm}")];
        let place = [&row["line"], &row["col"], &row["end_line"], &row["end_col"]];
        assert_eq!(row["file"], "main.hirsel", "{arm}");
        assert_eq!(place, [4u64, 8, 4, 17].map(serde_json::Value::from).each_ref(), "{arm}");
        assert_eq!(row["arm_line"].as_u64(), arm_line, "{arm}");
    }
    // `--seed-base 1 --seeds 2` runs seeds 1 and 2: by hand, 1 fails and 2 passes.
    let (status, stdout, stderr) = law.run(&["test", "app", "--sim", "--seeds", "2", "--seed-base", "1", "--json"]);
    assert_eq!(status, Some(1), "{stdout}\n{stderr}");
    let report: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let rows: Vec<_> = report["tests"].as_array().unwrap().iter().map(|r| (r["seed"].clone(), r["passed"].clone())).collect();
    assert_eq!(rows, [(1.into(), false.into()), (2.into(), true.into())]);
}

#[test]
fn property_simulation_keeps_world_and_data_seeds_separate() {
    let law = PackageLaw::new();
    law.manifest("app", "local/app", "1.0.0",
        "sources: [(\"main.hirsel\", \"main\")], dependencies: [(\"std\", Toolchain(package: \"std\", major: 1, authority: []))], tests: Some(Tests(sources: [], limit: 1048576, grants: [Tasks], runs: [])),",
    );
    law.write(
        "app/main.hirsel",
        r#"
fn odd_world_counterexample(take sample: std.prop.Case[u64]) -> Result[(), String] {
    if sample.world_seed % 2 == 1 && sample.value >= 3 {
        Error(string.from("odd world and value >= 3"))
    } else {
        Ok(())
    }
}

@test
fn prop_seeds(e: Env, tasks: Tasks) -> Result[(), ()] waits {
    let mut c = std.prop.config()
    c.seed = 5
    c.cases = 1
    c.sim_seed = std.sim.world.test_seed(e)
    std.prop.check_tasks(tasks, c, std.prop.u64_in(0, 9), odd_world_counterexample)
}
"#,
    );

    // With data seed 5, case 0 draws 4. The world seed is odd for sim
    // seed 0 and even for sim seed 1. Only the former shrinks to 3.
    let (status, stdout, stderr) = law.run(&["test", "app", "--sim", "--seeds", "2"]);
    assert_eq!(status, Some(1), "{stdout}\n{stderr}");
    assert_eq!(stdout.lines().count(), 2, "{stdout}");
    let failure = stdout
        .lines()
        .find(|line| line.starts_with("fail executed local/app@1/main/prop_seeds (seed 0): "))
        .unwrap_or_else(|| panic!("missing sim seed 0 failure\n{stdout}\n{stderr}"));
    assert!(
        failure.contains(r#"sim seed 0; reproduce: prop.replay(cfg, \"AQABAw==\")"#),
        "{failure}"
    );
    assert!(
        failure.contains("shrinks: 3; law: odd world and value >= 3"),
        "{failure}"
    );
    assert!(
        stdout
            .lines()
            .any(|line| line.starts_with("pass executed local/app@1/main/prop_seeds (seed 1)")),
        "{stdout}"
    );
}

#[test]
fn property_corpus_replays_minimized_case_before_search() {
    let law = PackageLaw::new();
    law.manifest("app", "local/app", "1.0.0",
        "sources: [(\"main.hirsel\", \"main\")], dependencies: [(\"std\", Toolchain(package: \"std\", major: 1, authority: []))], tests: Some(Tests(sources: [], limit: 1048576, grants: [Tasks], runs: [])),",
    );
    let source = r#"
fn classify(message: String) -> u64 {
    if message == "original assertion" { 31 } else { 82 }
}
fn planted(take sample: std.prop.Case[u64]) -> Result[(), String] {
    if sample.world_seed == 6869612563719971519 && sample.value >= 3 {
        Error(string.from("original assertion"))
    } else if sample.world_seed == 6869612563719971519 && sample.value == 0 {
        Error(string.from("different assertion"))
    } else {
        Ok(())
    }
}
@test
fn property(e: Env, tasks: Tasks) -> Result[(), ()] waits {
    let seed = option.unwrap(std.sim.world.test_seed(e))
    let mut c = std.prop.config()
    c.seed = if seed == 0 { 5 } else { 0 }
    c.cases = 1
    c.sim_seed = Some(seed + 123)
    let key = std.prop.Corpus(generator: 17, label: Some(classify))
    std.prop.check_tasks_corpus(e, tasks, key, c, std.prop.u64_in(0, 9), planted)
}
"#;
    let run = |seed: &str, code: i32| {
        let (status, stdout, stderr) = law.run(&[
            "test", "app", "--property-corpus", "corpus", "--seed", seed, "--json",
        ]);
        assert_eq!(status, Some(code), "{stdout}\n{stderr}");
        let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        json["tests"][0].clone()
    };
    law.write("app/main.hirsel", source);
    let saved = run("0", 1);
    assert_eq!(saved["property_corpus"], "saved");
    let entries: Vec<_> = std::fs::read_dir(law.path("corpus")).unwrap()
        .map(|entry| entry.unwrap().path()).collect();
    assert_eq!(entries.len(), 1);
    let bytes = std::fs::read(&entries[0]).unwrap();
    let record: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(record["test"], "local/app@1/main/property");
    assert_eq!(record["launch_seed"], 0);
    let input = record["input"].as_str().unwrap();
    // The data RNG first draws 4. The planted assertion first fails at 3.
    // AQABAw== is version 1, case 0, one choice, value 3. CgA= is bound 10 and terminator.
    assert_eq!(input, "17 5 123 1 4096 1000 1000 1000000 10000000 262144 AQABAw== CgA= returned-31");
    let replayed = run("1", 1);
    assert_eq!(replayed["property_corpus"], "confirmed");
    assert!(replayed["stderr"].as_str().unwrap().contains("0 shrinks: 3; law: original assertion"));
    assert_eq!(std::fs::read(&entries[0]).unwrap(), bytes);

    // Keep the generator and tape but return a second failure from the same law.
    law.write("app/main.hirsel", source.replace(
        "Error(string.from(\"original assertion\"))",
        "Error(string.from(\"different assertion\"))",
    ));
    let changed = run("1", 1);
    assert_eq!(changed["property_corpus"], "changed-failure");
    assert!(changed["stderr"].as_str().unwrap().contains("returned-82"));

    // Same identity and a bound that would otherwise replace saved 3 by zero.
    // The old world still fails if the law sees that coerced value.
    law.write("app/main.hirsel", source
        .replace("u64_in(0, 9)", "u64_in(0, 2)")
        .replace("sample.value >= 3", "sample.value == 0"));
    let stale = run("1", 0);
    assert_eq!(stale["property_corpus"], "stale");
    assert!(!entries[0].exists());

    let raised = source.replace(
        "Error(string.from(\"original assertion\"))",
        "if 0u64 == 0 { fail(\"raised assertion\") } else { fail(\"raised assertion\") }",
    );
    law.write("app/main.hirsel", &raised);
    let saved_raise = run("0", 1);
    assert_eq!(saved_raise["property_corpus"], "saved", "{saved_raise}");
    let raise_bytes = std::fs::read(&entries[0]).unwrap();
    let raise_record: serde_json::Value = serde_json::from_slice(&raise_bytes).unwrap();
    assert_eq!(raise_record["site"], "main.hirsel:7:24");
    let replayed_raise = run("1", 1);
    assert_eq!(replayed_raise["property_corpus"], "confirmed");
    assert_eq!(std::fs::read(&entries[0]).unwrap(), raise_bytes);

    law.write("app/main.hirsel", raised.replace("if 0u64 == 0", "if 1u64 == 0"));
    let changed_site = run("1", 1);
    assert_eq!(changed_site["property_corpus"], "changed-failure");
    let changed_record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&entries[0]).unwrap()).unwrap();
    assert_eq!(changed_record["site"], "main.hirsel:7:58");

    let mut old_record = record;
    old_record["version"] = 1.into();
    old_record.as_object_mut().unwrap().remove("site");
    std::fs::write(&entries[0], serde_json::to_vec(&old_record).unwrap()).unwrap();
    let replaced = run("0", 1);
    assert_eq!(replaced["property_corpus"], "stale-and-saved");
    let current: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&entries[0]).unwrap()).unwrap();
    assert_eq!(current["version"], 2);
}
