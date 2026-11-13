fn prog4_runs(backend: Backend) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let source = r#"
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
fn allocation() -> Result[(), (Capacity, u64)] { let b = box.new(7u64)?; Ok(()) }
@test
fn granted(out: Stdout) -> Result[(), ()] waits { result.unwrap(stdout.write(out, "test ran\n")); Ok(()) }
"#;
    let mut request = source_request(source, dir.path());
    request.entry = None;
    request.inputs.packages[1] = PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
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

#[test]
#[ignore = "explicit native integration target"]
fn prog4_independent_runs_limits_and_outcomes_cranelift() { prog4_runs(Backend::Cranelift); }
#[test]
#[ignore = "explicit native integration target"]
fn prog4_independent_runs_limits_and_outcomes_llvm() { prog4_runs(Backend::Llvm); }
