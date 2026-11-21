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
    assert_eq!(product.tests().map(|d| d.0.name.as_str()).collect::<Vec<_>>(), ["allocation", "error", "exit_seven", "exit_zero", "failed", "good", "granted", "own_error", "own_success", "stopped"]);
    let mut options = native_options(backend, dir.path());
    let test = |name: &str| DefPath::from_full(&format!("app@1/main/{name}")).unwrap();
    for (name, limit, grants, code, stdout, stderr) in [
        ("error", 4096, 0, 1, "", ""),
        ("exit_zero", 4096, 0, 1, "", ""),
        ("exit_seven", 4096, 0, 7, "", ""),
        ("failed", 4096, 0, 101, "", "Fail"),
        ("stopped", 4096, 0, 102, "", "run stop: Stack\n"),
        ("own_error", 4096, 0, 101, "", "Division"),
        ("own_success", 288, 0, 0, "", ""),
        ("allocation", 288, 0, 1, "", ""),
        ("allocation", 296, 0, 0, "", ""),
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
        if name == "stopped" {
            assert!(outcome.output.stderr.ends_with(b"Stack\n"), "{outcome:?}");
        } else if matches!(name, "failed" | "own_error") {
            let lines = String::from_utf8(outcome.output.stderr).unwrap();
            assert_eq!(lines.lines().count(), 1, "{name}: {lines}");
            assert!(lines.starts_with(stderr), "{name}: {lines}");
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
            ("stopped", 4096, 0, 102),
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
    for (label, options, identity) in [
        (
            "backend",
            NativeOptions {
                backend: other_backend,
                ..limited.clone()
            },
            None,
        ),
        (
            "optimize",
            NativeOptions {
                optimize: true,
                ..limited.clone()
            },
            None,
        ),
        (
            "debug lines",
            NativeOptions {
                debug_lines: true,
                ..limited.clone()
            },
            None,
        ),
        (
            "compiler",
            limited.clone(),
            Some(ContentHash::of_bytes(b"test compiler identity")),
        ),
    ] {
        let old = crate::reuse::TOOLCHAIN.replace(identity);
        let (outcome, source) = run(&product, "good", &options, false);
        crate::reuse::TOOLCHAIN.set(old);
        assert_eq!(source, Source::Executed, "{label}");
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

#[test]
#[ignore = "explicit native integration target"]
fn prog4_independent_runs_limits_and_outcomes_cranelift() {
    prog4_runs(Backend::Cranelift);
    prog4_pass_cache(Backend::Cranelift);
}
#[test]
#[ignore = "explicit native integration target"]
fn prog4_independent_runs_limits_and_outcomes_llvm() {
    prog4_runs(Backend::Llvm);
    prog4_pass_cache(Backend::Llvm);
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
fn e_build_refused() -> Result[(), ()] { missing_definition() }

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
    let args = [
        "test",
        "--inputs",
        manifest.to_str().unwrap(),
        "--source-core",
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
        assert_eq!(result["passed"], false);
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
    let configure = |tests: serde_json::Value| {
        let manifest = serde_json::json!({
            "packages": [{
                "package": {"name": "app", "version": "1"},
                "dependencies": [{"name": "core", "version": "1"}],
                "sources": [{"file": "main.hirsel", "modules": ["main"], "signatures": false}],
            }],
            "tests": tests,
        });
        std::fs::write(package.join("manifest.json"), manifest.to_string()).unwrap();
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
    let args = |extra: &[&str]| {
        [
            "test",
            package.to_str().unwrap(),
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
            .map(|(name, source)| (format!("app@1/main/{name}"), (*source).to_owned()))
            .collect();
        assert_eq!(rows, expected);
    };
    configure(serde_json::json!({"limit": 288, "runs": {"app@1/main/allocation": {"limit": 296}}}));
    run([("allocation", "executed"), ("good", "executed")]);
    run([("allocation", "cached"), ("good", "cached")]);
    configure(
        serde_json::json!({"limit": 288, "runs": {"app@1/main/allocation": {"limit": 4096}}}),
    );
    run([("allocation", "executed"), ("good", "cached")]);
    configure(
        serde_json::json!({"limit": 289, "runs": {"app@1/main/allocation": {"limit": 4096}}}),
    );
    run([("allocation", "cached"), ("good", "executed")]);

    let (status, stdout, stderr) = request(&args(&["--limit", "4096"]));
    assert_ne!(status, Some(0), "{stdout}");
    assert!(stderr.contains("test takes no --limit"), "{stderr}");
    configure(serde_json::json!({"limit": 288, "runs": {"app@1/main/missing": {"limit": 296}}}));
    let (status, stdout, stderr) = request(&args(&[]));
    assert_ne!(status, Some(0), "{stdout}");
    assert!(
        stderr.contains("app@1/main/missing, which is not one of its tests"),
        "{stderr}"
    );
}
