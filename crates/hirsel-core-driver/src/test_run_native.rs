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
    let configure = |limit, test, test_limit| {
        let manifest = format!(
            r#"[package]
name = "app"
major = 1
charge = "charge-v1"

[sources]
"main.hirsel" = "main"

[tests]
limit = {limit}

[tests.runs."app@1/main/{test}"]
limit = {test_limit}
"#
        );
        std::fs::write(package.join("hirsel.toml"), manifest).unwrap();
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
            .map(|(name, source)| (format!("app@1/main/{name}"), (*source).to_owned()))
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
    assert!(stderr.contains("test takes no --limit"), "{stderr}");
    configure(288, "missing", 296);
    let (status, stdout, stderr) = request(&args(&[]));
    assert_ne!(status, Some(0), "{stdout}");
    assert!(
        stderr.contains("app@1/main/missing, which is not one of its tests"),
        "{stderr}"
    );
}

const PACKAGE_X: &str = "sha256:0f4f2cfc3866d57002070c0af895f588d6dd5d12493e580368f2195cb428de58";
const PACKAGE_Y: &str = "sha256:df7ca24aa6689979ef02faa3a35084bf96207edbe52b19d78e6273a98dc2ca99";
const PACKAGE_W: &str = "sha256:d863de7c3cf6ed4aa5535ee816d90fd603b45462eb6c2b78c846c584bd429689";
const PACKAGE_D: &str = "sha256:e073ce479e0454fe08a3b357087e8de69af0e172a90afa5419faf4307f3b8a75";
const PACKAGE_X_SOURCE: &str = "pub fn version() -> u64 { 1u64 }\n";
const PACKAGE_Y_SOURCE: &str = "pub fn version() -> u64 { 2u64 }\n";
const PACKAGE_MAIN: &str = "pub fn main() -> Result[(), Exit] { Ok(()) }\n";

struct PackageLaw {
    dir: tempfile::TempDir,
    libraries: PathBuf,
}

impl PackageLaw {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let libraries = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../core").canonicalize().unwrap();
        std::fs::create_dir(dir.path().join("packages")).unwrap();
        std::fs::create_dir(dir.path().join("fixtures")).unwrap();
        Self { dir, libraries }
    }

    fn path(&self, path: &str) -> PathBuf {
        self.dir.path().join(path)
    }

    fn write(&self, path: &str, bytes: impl AsRef<[u8]>) {
        let path = self.path(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    fn manifest(&self, dir: &str, name: &str, major: u32, tail: &str) {
        self.write(&format!("{dir}/hirsel.toml"),
            format!("[package]\nname = {name:?}\nmajor = {major}\ncharge = \"charge-v1\"\n\n{tail}"));
    }

    fn library(&self, dir: &str, name: &str, major: u32, source: &str, tail: &str) {
        self.manifest(dir, name, major, &format!("[sources]\n\"{name}.hirsel\" = {name:?}\n\n{tail}"));
        self.write(&format!("{dir}/{name}.hirsel"), source);
    }

    fn app(&self, dir: &str, tail: &str) {
        self.manifest(dir, "app", 1, &format!("[sources]\n\"main.hirsel\" = \"main\"\n\n{tail}"));
        self.write(&format!("{dir}/main.hirsel"), PACKAGE_MAIN);
    }

    fn with_store(&self, arguments: &[&str], store: &str) -> (Option<i32>, String, String) {
        let mut args = arguments.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        args.extend(["--package-store".into(), self.path(store).to_str().unwrap().into()]);
        if matches!(arguments[0], "check" | "test" | "run" | "build" | "publish" | "inspect") {
            args.extend(["--libraries".into(), self.libraries.to_str().unwrap().into()]);
            args.extend(["--store".into(), self.path("compiler-store").to_str().unwrap().into()]);
        }
        if matches!(arguments[0], "build" | "publish") {
            args.extend(["--code-store".into(), self.path("code-store").to_str().unwrap().into()]);
        }
        if matches!(arguments[0], "run" | "test" | "build") {
            let options = native_options(Backend::Cranelift, self.dir.path());
            let Link::Driver(cc) = options.link else { unreachable!() };
            args.extend(["--runtime".into(), options.runtime.canonicalize().unwrap().to_str().unwrap().into()]);
            args.extend(["--cc".into(), cc.canonicalize().unwrap().to_str().unwrap().into()]);
        }
        let output = process::tape_request_command(&args, self.dir.path())
            .env("XDG_CACHE_HOME", self.path("cache"))
            .output().unwrap();
        let after = |bytes: &[u8]| {
            let text = String::from_utf8_lossy(bytes);
            let (_, text) = text.split_once(process::CHILD_MARK).unwrap();
            text.to_owned()
        };
        (output.status.code(), after(&output.stdout), after(&output.stderr))
    }

    fn run(&self, args: &[&str]) -> (Option<i32>, String, String) {
        self.with_store(args, "packages")
    }

    fn pass(output: (Option<i32>, String, String)) -> String {
        let (status, stdout, stderr) = output;
        assert_eq!(status, Some(0), "{stdout}\n{stderr}");
        stdout
    }

    fn refuse(output: (Option<i32>, String, String), reason: &str) {
        let (status, stdout, stderr) = output;
        assert_eq!(status, Some(2), "{stdout}\n{stderr}");
        assert_eq!(stderr, format!("hirsel: {reason}\n"));
    }

    fn fixture(&self, directory: &str, out: &str) -> String {
        Self::pass(self.with_store(&["pack", directory, "--out", out], "fixtures"))
    }

    fn fixture_json(&self) {
        self.library("jx", "json", 1, PACKAGE_X_SOURCE, "");
        self.library("jy", "json", 1, PACKAGE_Y_SOURCE, "");
        std::fs::create_dir_all(self.path("vendor")).unwrap();
        assert_eq!(self.fixture("jx", "vendor/jx.hpkg"), format!("json@1 {PACKAGE_X}\n"));
        assert_eq!(self.fixture("jy", "vendor/jy.hpkg"), format!("json@1 {PACKAGE_Y}\n"));
    }

    fn encoded(&self, root: &str) -> Vec<u8> {
        std::fs::read(self.path(&format!("fixtures/{}", root.strip_prefix("sha256:").unwrap()))).unwrap()
    }

    fn empty_store(&self) {
        assert!(std::fs::read_dir(self.path("packages")).unwrap().next().is_none());
    }
}

fn package_bundle(entries: &[&[u8]]) -> Vec<u8> {
    fn varint(mut number: usize, output: &mut Vec<u8>) {
        while number >= 128 {
            output.push(((number % 128) | 128) as u8);
            number /= 128;
        }
        output.push(number as u8);
    }
    let tag = b"hirsel.package-archive.v1";
    let mut bytes = vec![tag.len() as u8];
    bytes.extend_from_slice(tag);
    varint(entries.len(), &mut bytes);
    for entry in entries {
        varint(entry.len(), &mut bytes);
        bytes.extend_from_slice(entry);
    }
    bytes
}

#[test]
fn package_known_roots() {
    let law = PackageLaw::new();
    law.library("jx", "json", 1, PACKAGE_X_SOURCE, "");
    law.library("jy", "json", 1, PACKAGE_Y_SOURCE, "");
    for (dir, root) in [("jx", PACKAGE_X), ("jy", PACKAGE_Y)] {
        let archive = format!("{dir}.hpkg");
        let stdout = PackageLaw::pass(law.run(&["pack", dir, "--out", &archive]));
        assert_eq!(stdout, format!("json@1 {root}\n"));
        let content = std::fs::read(law.path(&format!("packages/{}", &root[7..]))).unwrap();
        assert_eq!(content.len(), 72);
    }
}

#[test]
fn package_foreign_roots_and_decoder_domain() {
    let law = PackageLaw::new();
    law.manifest("demo", "demo", 1, r#"
[sources]
"api.hirsel" = "api"
"aa.hirsel" = "aa"
"b.hirsel" = "b"

[foreign.aa]
symbols = "ha_"
archives = [
  { target = "x86_64-unknown-linux-gnu", file = "native/aa-x86.a", sha256 = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad" },
  { target = "aarch64-unknown-linux-gnu", file = "native/aa-arm.a", sha256 = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855" },
]

[foreign.b]
symbols = "hb_"
archives = [{ target = "x86_64-unknown-linux-gnu", file = "native/b.a", sha256 = "3e23e8160039594a33894f6564e1b1348bbd7a0088d42c4acb73eeaed59c009d" }]
"#);
    law.write("demo/api.hirsel", "pub opaque type A {}\npub opaque type B {}\n");
    law.write("demo/aa.hirsel", "pub fn ping(f: Foreign[api.A]) -> u64\n");
    law.write("demo/b.hirsel", "pub fn pong(f: Foreign[api.B]) -> u64\n");
    law.write("demo/native/aa-x86.a", b"abc");
    law.write("demo/native/aa-arm.a", b"");
    law.write("demo/native/b.a", b"b");
    assert_eq!(law.fixture("demo", "demo.hpkg"), format!("demo@1 {PACKAGE_D}\n"));
    let content = law.encoded(PACKAGE_D);
    assert_eq!(content.len(), 422);
    // Section 12's item 6 frames, located by their literal wire bytes.
    let position = |pattern: &[u8]| content.windows(pattern.len()).position(|bytes| bytes == pattern).unwrap();
    let aa = position(b"\x01\x02aa\x03ha_\x02");
    let b = position(b"\x01\x01b\x03hb_\x01");
    let arm = position(b"\x19aarch64-unknown-linux-gnu\x0fnative/aa-arm.a");
    let x86 = position(b"\x18x86_64-unknown-linux-gnu\x0fnative/aa-x86.a");
    let swapped = [&content[..aa], &content[b..], &content[aa..b]].concat();
    assert_eq!(
        crate::package_content::Root::of(&swapped).to_string(),
        "sha256:b8b4134afbaded2a97a56700b11e35346406eb32701216283453befa2712b428"
    );
    let rows_swapped = [&content[..arm], &content[x86..b], &content[arm..x86], &content[b..]].concat();
    let mut pin_mismatch = content;
    *pin_mismatch.last_mut().unwrap() = b'c';
    law.app("app", &format!("[dependencies]\ndemo = {{ major = 1, root = \"{PACKAGE_D}\", archive = \"demo.hpkg\" }}\n"));
    for (encoded, refusal) in [
        (swapped, "foreign modules out of order at aa"),
        (rows_swapped, "archive rows out of order at aarch64-unknown-linux-gnu"),
        (pin_mismatch, "archive \"native/b.a\" of b for x86_64-unknown-linux-gnu does not match its pin"),
    ] {
        law.write("app/demo.hpkg", package_bundle(&[&encoded]));
        PackageLaw::refuse(law.run(&["check", "app"]), &format!("demo@1: archive:demo.hpkg is not a package archive: {refusal}"));
        law.empty_store();
    }
    law.fixture_json();
    let content = law.encoded(PACKAGE_X);
    let pin = |root: &str| (7..71).step_by(2).map(|i| u8::from_str_radix(&root[i..i+2], 16).unwrap()).collect::<Vec<_>>();
    let x = pin(PACKAGE_X);
    let y = pin(PACKAGE_Y);
    let twice = [b"\x02\x04util\x01\x01", x.as_slice(), b"\x04util\x02\x01", y.as_slice()].concat();
    let std_pin = [b"\x01\x03std\x01\x01", x.as_slice()].concat();
    law.app("app", &format!("[dependencies]\njson = {{ major = 1, root = \"{PACKAGE_X}\", archive = \"jx.hpkg\" }}\n"));
    for (bindings, refusal) in [(twice, "two bindings of util"), (std_pin, "bad binding std@1")] {
        let bytes = [&content[..17], &bindings, &content[18..]].concat();
        law.write("app/jx.hpkg", package_bundle(&[&bytes]));
        PackageLaw::refuse(law.run(&["check", "app"]), &format!("json@1: archive:jx.hpkg is not a package archive: {refusal}"));
        law.empty_store();
    }
}

#[test]
fn package_l4_symlink_escape() {
    let law = PackageLaw::new();
    law.manifest("p", "p", 1, "[sources]\n\"a.hirsel\" = \"a\"\n");
    law.write("outside.hirsel", PACKAGE_MAIN);
    std::os::unix::fs::symlink("../outside.hirsel", law.path("p/a.hirsel")).unwrap();
    for args in [&["check", "p"][..], &["pack", "p", "--out", "p.hpkg"][..]] {
        PackageLaw::refuse(law.run(args), "p/hirsel.toml: sources: \"a.hirsel\" resolves outside the package");
        assert!(!law.path("p/hirsel.lock").exists());
        assert!(!law.path("p.hpkg").exists());
    }
    std::fs::remove_file(law.path("p/a.hirsel")).unwrap();
    law.write("p/real/a.hirsel", PACKAGE_MAIN);
    std::os::unix::fs::symlink("real/a.hirsel", law.path("p/a.hirsel")).unwrap();
    PackageLaw::pass(law.run(&["check", "p"]));
    let linked = PackageLaw::pass(law.run(&["pack", "p", "--out", "p.hpkg"]));
    law.manifest("copy", "p", 1, "[sources]\n\"a.hirsel\" = \"a\"\n");
    law.write("copy/a.hirsel", PACKAGE_MAIN);
    assert_eq!(linked, PackageLaw::pass(law.run(&["pack", "copy", "--out", "copy.hpkg"])));
}

#[test]
fn package_l5_test_files_do_not_enter_content() {
    let law = PackageLaw::new();
    law.library("jx", "json", 1, PACKAGE_X_SOURCE,
        "[tests]\nlimit = 67108864\n[tests.sources]\n\"json_laws.hirsel\" = \"json\"\n");
    law.write("jx/json_laws.hirsel",
        "@test\nfn version_law() -> Result[(), ()] { if version() == 1u64 { Ok(()) } else { Error(()) } }\n");
    assert_eq!(PackageLaw::pass(law.run(&["pack", "jx", "--out", "jx.hpkg"])), format!("json@1 {PACKAGE_X}\n"));
    let stdout = PackageLaw::pass(law.run(&["test", "jx", "--json"]));
    let results: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let tests = results["tests"].as_array().unwrap();
    assert_eq!(tests.len(), 1);
    assert_eq!(tests[0]["definition"], "json@1/json/version_law");
    assert_eq!(tests[0]["source"], "executed");
    assert_eq!(tests[0]["passed"], true);
}

#[test]
fn package_l12_walks_never_leave_their_root() {
    let law = PackageLaw::new();
    law.write("W/hirsel.toml", "[workspace]\nmembers = [\"app\", \"lib\"]\n");
    law.app("W/app", "");
    law.library("elsewhere/lib", "lib", 1, PACKAGE_MAIN, "");
    std::os::unix::fs::symlink("../elsewhere/lib", law.path("W/lib")).unwrap();
    PackageLaw::refuse(law.run(&["lock", "W"]), "W/hirsel.toml: workspace.members: \"lib\" resolves outside the workspace");
    assert!(!law.path("W/hirsel.lock").exists());

    law.manifest("p", "p", 1, "[sources]\n\"a.hirsel\" = \"a\"\n");
    law.write("p/real.hirsel", PACKAGE_MAIN);
    std::os::unix::fs::symlink("..", law.path("p/up")).unwrap();
    std::os::unix::fs::symlink("up/p/real.hirsel", law.path("p/a.hirsel")).unwrap();
    PackageLaw::refuse(law.run(&["check", "p"]), "p/hirsel.toml: sources: \"a.hirsel\" resolves outside the package");
    assert!(!law.path("p/hirsel.lock").exists());
}

#[test]
fn package_l1_diamond_and_major_identity() {
    let law = PackageLaw::new();
    law.fixture_json();
    law.write("work/hirsel.toml", "[workspace]\nmembers = [\"app\", \"a\", \"b\"]\n");
    let app_edges = "[dependencies]\na = { major = 1, path = \"../a\" }\nb = { major = 1, path = \"../b\" }\n";
    law.app("work/app", app_edges);
    for (name, root, archive) in [("a", PACKAGE_X, "jx"), ("b", PACKAGE_Y, "jy")] {
        law.library(&format!("work/{name}"), name, 1, "pub fn one() -> u64 { 1u64 }\n",
            &format!("[dependencies]\njson = {{ major = 1, root = \"{root}\", archive = \"../vendor/{archive}.hpkg\" }}\n"));
        law.write(&format!("work/vendor/{archive}.hpkg"), std::fs::read(law.path(&format!("vendor/{archive}.hpkg"))).unwrap());
    }
    PackageLaw::refuse(law.run(&["check", "work/app"]),
        &format!("json@1 is bound to more than one root:\n  app@1 -> a@1 -> json@1 = {PACKAGE_X}\n  app@1 -> b@1 -> json@1 = {PACKAGE_Y}"));
    assert!(!law.path("work/hirsel.lock").exists());

    law.library("work/b", "b", 1, "pub fn one() -> u64 { 1u64 }\n",
        &format!("[dependencies]\njson = {{ major = 1, root = \"{PACKAGE_X}\", archive = \"../vendor/jx.hpkg\" }}\n"));
    PackageLaw::pass(law.run(&["check", "work/app"]));
    let lock = std::fs::read_to_string(law.path("work/hirsel.lock")).unwrap();
    assert_eq!(lock.matches("id = \"json@1\"").count(), 1);
    assert!(lock.contains(&format!("root = \"{PACKAGE_X}\"")));

    law.library("j2", "json", 2, PACKAGE_Y_SOURCE, "");
    let fixture = law.fixture("j2", "work/vendor/j2.hpkg");
    let (_, pin) = fixture.trim().split_once(' ').unwrap();
    law.library("work/b", "b", 1, "pub fn one() -> u64 { 1u64 }\n",
        &format!("[dependencies]\njson = {{ major = 2, root = \"{pin}\", archive = \"../vendor/j2.hpkg\" }}\n"));
    PackageLaw::pass(law.run(&["check", "work/app"]));
    let lock = std::fs::read_to_string(law.path("work/hirsel.lock")).unwrap();
    assert_eq!(lock.matches("id = \"json@1\"").count(), 1);
    assert_eq!(lock.matches("id = \"json@2\"").count(), 1);
}

#[test]
fn package_l6_archive_closure_and_atomic_refusal() {
    let law = PackageLaw::new();
    law.fixture_json();
    law.app("app", &format!("[dependencies]\njson = {{ major = 1, root = \"{PACKAGE_X}\", archive = \"vendor/jy.hpkg\" }}\n"));
    let jy = std::fs::read(law.path("vendor/jy.hpkg")).unwrap();
    law.write("app/vendor/jy.hpkg", jy);
    PackageLaw::refuse(law.run(&["check", "app"]), &format!("json@1: archive:vendor/jy.hpkg holds {PACKAGE_Y}, but the binding pins {PACKAGE_X}"));
    law.empty_store();
    assert!(!law.path("app/hirsel.lock").exists());

    law.write("work/hirsel.toml", "[workspace]\nmembers = [\"wrap\"]\n");
    law.write("work/vendor/jx.hpkg", std::fs::read(law.path("vendor/jx.hpkg")).unwrap());
    law.library("work/wrap", "wrap", 1, "pub fn one() -> u64 { 1u64 }\n",
        &format!("[dependencies]\njson = {{ major = 1, root = \"{PACKAGE_X}\", archive = \"../vendor/jx.hpkg\" }}\n"));
    assert_eq!(law.fixture("work/wrap", "app/vendor/wrap.hpkg"), format!("wrap@1 {PACKAGE_W}\njson@1 {PACKAGE_X}\n"));
    let w = law.encoded(PACKAGE_W);
    assert_eq!(w.len(), 107);
    let x = law.encoded(PACKAGE_X);
    let y = law.encoded(PACKAGE_Y);
    let correct = std::fs::read(law.path("app/vendor/wrap.hpkg")).unwrap();
    law.app("app", &format!("[dependencies]\nwrap = {{ major = 1, root = \"{PACKAGE_W}\", archive = \"vendor/wrap.hpkg\" }}\n"));
    let missing = format!("wrap@1: archive:vendor/wrap.hpkg lacks {PACKAGE_X}, which wrap@1 binds as json@1");
    for (entries, refusal) in [
        (vec![w.as_slice(), y.as_slice()], missing.as_str()),
        (vec![w.as_slice(), x.as_slice(), y.as_slice()], &format!("wrap@1: archive:vendor/wrap.hpkg holds {PACKAGE_Y}, which nothing in it binds")),
    ] {
        law.write("app/vendor/wrap.hpkg", package_bundle(&entries));
        PackageLaw::refuse(law.run(&["check", "app"]), refusal);
        law.empty_store();
    }
    law.write(&format!("packages/{}", &PACKAGE_X[7..]), &x);
    law.write("app/vendor/wrap.hpkg", package_bundle(&[&w]));
    PackageLaw::refuse(law.run(&["check", "app"]), &missing);
    assert_eq!(std::fs::read_dir(law.path("packages")).unwrap().count(), 1);
    assert!(!law.path("app/hirsel.lock").exists());

    law.write("app/vendor/wrap.hpkg", correct);
    PackageLaw::pass(law.run(&["check", "app"]));
    assert_eq!(std::fs::read_dir(law.path("packages")).unwrap().count(), 2);
    // Removing a transitive entry must reopen the carrier even when W is warm.
    std::fs::remove_file(law.path(&format!("packages/{}", &PACKAGE_X[7..]))).unwrap();
    PackageLaw::pass(law.run(&["check", "app"]));
    assert_eq!(std::fs::read(law.path(&format!("packages/{}", &PACKAGE_X[7..]))).unwrap(), x);
}

#[test]
fn package_l11_store_hits_validate_every_incoming_edge() {
    let law = PackageLaw::new();
    law.library("jx", "json", 1, PACKAGE_X_SOURCE, "");
    PackageLaw::pass(law.run(&["pack", "jx", "--out", "jx.hpkg"]));
    let yaml = format!("yaml = {{ major = 1, root = \"{PACKAGE_X}\" }}");
    let expected = format!("yaml@1: {PACKAGE_X} is json@1");
    law.app("app", &format!("[dependencies]\n{yaml}\n"));
    PackageLaw::refuse(law.run(&["lock", "app"]), &expected);
    law.app("app", &format!("[dependencies]\njson = {{ major = 1, root = \"{PACKAGE_X}\", archive = \"vendor/jx.hpkg\" }}\n{yaml}\n"));
    PackageLaw::refuse(law.run(&["lock", "app"]), &expected);
    assert!(!law.path("app/hirsel.lock").exists());
}

#[test]
fn package_l13_explicit_image_uses_only_the_closure() {
    let law = PackageLaw::new();
    law.manifest("core_only", "core_only", 1, "[sources]\n\"main.hirsel\" = \"main\"\n[dependencies]\nstd = { major = 1 }\n");
    law.write("core_only/main.hirsel", PACKAGE_MAIN);
    let mut request = CheckRequest::program(&law.path("core_only"), &law.libraries, &law.path("image")).unwrap();
    request.entry = None;
    let (_, image) = Driver::default().produce_image(request, &mut Meter::new(false)).unwrap();
    assert!(image.is_some());

    law.manifest("core_only", "core_only", 1, "[sources]\n\"main.hirsel\" = \"main\"\n");
    law.write("core_only/main.hirsel", "pub fn main() -> u64 { std.x }\n");
    let (status, stdout, stderr) = law.run(&["check", "core_only", "--json"]);
    assert_eq!(status, Some(1), "{stdout}\n{stderr}");
    let result: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert!(result["diagnostics"].as_array().unwrap().iter().any(|row| row["message"].as_str().is_some_and(|text| text.contains("std"))));
    PackageLaw::refuse(law.run(&["check", "core_only", "--image", "image"]), "--image image does not hold exactly this request's packages");

    let prefix = law.path("installed");
    let std = crate::PackageProduct::bundled_std(&prefix.join("images"), &mut Meter::new(false)).unwrap();
    crate::installation::register(&prefix, "std", &std.image, None, None, None).unwrap();
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
    format!(concat!(
        "# hirsel.lock: written by hirsel; do not edit.\n",
        "version = 1\n",
        "\n[[package]]\nid = \"app@1\"\nsource = \"member:.\"\nedges = [\"json@1\", \"std@1\"]\n",
        "\n[[package]]\nid = \"json@1\"\nroot = \"{}\"\nsource = \"archive:vendor/{}.hpkg\"\nedges = []\n",
        "\n[[package]]\nid = \"std@1\"\nsource = \"toolchain\"\nedges = []\n"
    ), root, archive)
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
        law.app("app", &format!("[dependencies]\njson = {{ major = 1, root = \"{root}\", archive = \"vendor/{archive}.hpkg\" }}\nstd = {{ major = 1 }}\n"));
    };
    let path = law.path("app/hirsel.lock");
    bind(PACKAGE_X, "jx");
    PackageLaw::pass(law.run(&["lock", "app"]));
    let original = package_lock_fixture(PACKAGE_X, "jx");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    bind(PACKAGE_Y, "jy");
    let diff = format!("  json@1: root {PACKAGE_X} -> {PACKAGE_Y}\n  json@1: source archive:vendor/jx.hpkg -> archive:vendor/jy.hpkg");
    PackageLaw::refuse(law.run(&["check", "app", "--locked"]), &format!("hirsel.lock is stale under --locked:\n{diff}"));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    let ordinary = law.run(&["check", "app"]);
    assert_eq!(ordinary.2, format!("hirsel: updated hirsel.lock:\n{diff}\n"));
    PackageLaw::pass(ordinary);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), package_lock_fixture(PACKAGE_Y, "jy"));

    law.write("work/hirsel.toml", "[workspace]\nmembers = [\"app\", \"util\"]\n");
    law.app("work/app", "[dependencies]\nutil = { major = 1, path = \"../util\" }\n");
    for archive in ["jx", "jy"] {
        let path = format!("vendor/{archive}.hpkg");
        law.write(&format!("work/{path}"), std::fs::read(law.path(&path)).unwrap());
    }
    let util = |pin: &str, archive: &str| {
        law.library("work/util", "util", 1, "pub fn one() -> u64 { 1u64 }\n",
            &format!("[dependencies]\njson = {{ major = 1, root = \"{pin}\", archive = \"../vendor/stale/../{archive}.hpkg\" }}\n"));
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
    law.app("app", &format!("[dependencies]\njson = {{ major = 1, root = \"{PACKAGE_X}\", url = \"{url}\" }}\nstd = {{ major = 1 }}\n"));
    PackageLaw::pass(law.with_store(&["lock", "app"], "fixtures"));
    let refusal = |status: &str, source: &str| format!("json@1 {PACKAGE_X} {status} the package store, and --frozen does not fetch (source {source})");
    PackageLaw::refuse(law.run(&["check", "app", "--frozen"]), &refusal("is not in", &format!("url:{url}")));
    law.empty_store();
    let mut damaged = law.encoded(PACKAGE_X);
    damaged[0] ^= 1;
    let store_path = format!("packages/{}", &PACKAGE_X[7..]);
    law.write(&store_path, damaged);
    PackageLaw::refuse(law.run(&["check", "app", "--frozen"]), &refusal("is damaged in", &format!("url:{url}")));
    std::fs::remove_file(law.path(&store_path)).unwrap();

    law.app("app", &format!("[dependencies]\njson = {{ major = 1, root = \"{PACKAGE_X}\", archive = \"vendor/jx.hpkg\" }}\nstd = {{ major = 1 }}\n"));
    let archive = "app/vendor/jx.hpkg";
    law.write(archive, std::fs::read(law.path("vendor/jx.hpkg")).unwrap());
    PackageLaw::pass(law.with_store(&["lock", "app"], "fixtures"));
    PackageLaw::pass(law.with_store(&["check", "app", "--locked"], "third-store"));
    assert_eq!(std::fs::read(law.path(&format!("third-store/{}", &PACKAGE_X[7..]))).unwrap(), law.encoded(PACKAGE_X));
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
    law.write("work/hirsel.toml", "[workspace]\nmembers = [\"app\", \"json\"]\n");
    for archive in ["jx", "jy"] {
        let path = format!("vendor/{archive}.hpkg");
        law.write(&format!("work/{path}"), std::fs::read(law.path(&path)).unwrap());
    }
    let bind = |root: &str, archive: &str, source: &str| {
        law.library("work/json", "json", 1, source, "");
        law.app("work/app", &format!("[dependencies]\njson = {{ major = 1, root = \"{root}\", archive = \"../vendor/{archive}.hpkg\" }}\n"));
    };
    bind(PACKAGE_X, "jx", PACKAGE_X_SOURCE);
    PackageLaw::pass(law.run(&["lock", "work"]));
    let lock = law.path("work/hirsel.lock");
    let original = format!(concat!(
        "# hirsel.lock: written by hirsel; do not edit.\nversion = 1\n",
        "\n[[package]]\nid = \"app@1\"\nsource = \"member:app\"\nedges = [\"json@1\"]\n",
        "\n[[package]]\nid = \"json@1\"\nroot = \"{}\"\nsource = \"member:json\"\nedges = []\n"
    ), PACKAGE_X);
    assert_eq!(std::fs::read_to_string(&lock).unwrap(), original);

    bind(PACKAGE_Y, "jy", PACKAGE_Y_SOURCE);
    PackageLaw::refuse(law.run(&["check", "work/app", "--locked"]), &format!("hirsel.lock is stale under --locked:\n  json@1: root {PACKAGE_X} -> {PACKAGE_Y}"));
    assert_eq!(std::fs::read_to_string(&lock).unwrap(), original);
    law.library("work/json", "json", 1, PACKAGE_X_SOURCE, "");
    law.app("work/app", "[dependencies]\njson = { major = 1, path = \"../json\" }\n");
    PackageLaw::refuse(law.run(&["check", "work/app", "--locked"]), &format!("hirsel.lock is stale under --locked:\n  json@1: root {PACKAGE_X} -> none"));
    assert_eq!(std::fs::read_to_string(&lock).unwrap(), original);
}

#[test]
fn package_member_tests_keep_capabilities() {
    let law = PackageLaw::new();
    law.write("work/hirsel.toml", "[workspace]\nmembers = [\"app\", \"lib\"]\n");
    law.library("work/lib", "lib", 1, "@test\nfn dependency_only() -> Result[(), ()] { Error(()) }\n", "");
    law.app("work/app", "[dependencies]\nlib = { major = 1, path = \"../lib\" }\n[tests]\nlimit = 67108864\ngrants = [\"Stdout\"]\n[tests.sources]\n\"local.hirsel\" = \"local\"\n");
    law.write("work/app/local.hirsel", "@test\nfn capacity(out: Stdout) -> Result[(), WriteError] waits { stdout.write(out, \"own test\") }\n");
    for _ in 0..2 {
        let stdout = PackageLaw::pass(law.run(&["test", "work/app", "--json"]));
        let results: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        let tests = results["tests"].as_array().unwrap();
        assert_eq!(tests.len(), 1);
        assert_eq!(tests[0]["definition"], "app@1/local/capacity");
        assert_eq!(tests[0]["source"], "executed");
        assert_eq!(tests[0]["passed"], true);
    }
}

#[test]
fn package_foreign_sources_are_marked_in_both_origins() {
    let law = PackageLaw::new();
    law.manifest("native", "shim", 1, r#"
[sources]
"api.hirsel" = "api"
"c.hirsel" = "c"

[foreign.c]
symbols = "hh_"
archives = []
"#);
    law.write("native/api.hirsel", "pub opaque type Api {}\n");
    law.write("native/c.hirsel", "pub fn probe(f: Foreign[api.Api]) -> u64\n");
    let packed = law.fixture("native", "shim.hpkg");
    let (_, root) = packed.trim().split_once(' ').unwrap();
    law.app("app", &format!("[dependencies]\nshim = {{ major = 1, root = \"{root}\", archive = \"shim.hpkg\" }}\n"));
    law.write("app/shim.hpkg", std::fs::read(law.path("shim.hpkg")).unwrap());

    let member = crate::package_dir::PackageDir::open(&law.path("native")).unwrap();
    let workspace = crate::package_workspace::Workspace::discover(&law.path("app")).unwrap().unwrap();
    let selected = workspace.selected().unwrap().clone();
    let library = crate::package_resolve::Resolution::resolve(
        workspace, crate::package_store::PackageStore::new(Some(law.path("packages"))), false,
    ).unwrap();
    let consumer = crate::package_dir::PackageDir::from_resolution(&library, &selected, false).unwrap();
    for (package, expected_name) in [
        (&member.package, "c.hirsel"), (&consumer.dependencies[0], "shim@1/c.hirsel"),
    ] {
        let names: Vec<_> = package.sources.iter().filter(|source| source.foreign)
            .map(|source| source.captured.as_ref().unwrap().name.as_deref().unwrap()).collect();
        assert_eq!(names, [expected_name]);
    }
}
