// FAIL-2 / PROG-3: output precedes a failure at the actual callee expression.
// Both positions are hand-counted below; no evaluator-derived expected value.
fn failure_sites_rebind(backend: Backend, optimize: bool, debug_lines: bool) {
    let dir = tempfile::tempdir().unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let overflow = DefPath::from_full("app@1/main/overflow").unwrap();
    let source = "fn overflow(x: u64) -> u64 {\n    x + 1\n}\npub fn main(out: Stdout) -> Result[(), Exit] {\n    result.unwrap(stdout.write(out, \"before\\n\"));\n    if overflow(18446744073709551615u64) == 0 { Ok(()) } else { Error(Exit(status: 7)) }\n}\n";
    let check = |source: &str| {
        let mut request = source_request(source, dir.path());
        request.inputs.packages[1] = PackageManifest::directory(
            &root.join("core/core"), package("core"), true,
        ).unwrap();
        let product = Driver::default().check(request, &mut Meter::new(false)).unwrap();
        assert!(product.accepted(), "{:?} {:?} {:?}", product.diagnostics, product.incomplete, product.resources);
        product
    };
    let mut options = native_options(backend, dir.path());
    options.optimize = optimize;
    options.debug_lines = debug_lines;
    options.grant_mask = 1 << 4;
    let run = |built: &BuildProduct, line: u32| {
        let output = Command::new(&built.executable).output().unwrap();
        assert_eq!(output.status.code(), Some(101), "{output:?}");
        assert_eq!(output.stdout, b"before\n", "{output:?}");
        assert_eq!(output.stderr, format!("Overflow at {}:{line}:5\n", dir.path().join("main.hirsel").display()).as_bytes(), "{output:?}");
        assert_eq!(built.debug_lines, debug_lines);
        let bytes = std::fs::read(&built.executable).unwrap();
        if debug_lines {
            let positions = crate::native_debug::source_positions(&bytes).unwrap();
            assert!(positions.contains(&(dir.path().join("main.hirsel").display().to_string(), line, 5)),
                "debug line table lost the raising expression: {positions:?}");
        } else {
            use object::{Object as _, ObjectSection as _};
            let file = object::File::parse(bytes.as_slice()).unwrap();
            assert!(file.sections().all(|s| !s.name().unwrap().starts_with(".debug")),
                "release without line rows must not link a debug object");
        }
    };
    let cold = check(source).build(&entry, &options).unwrap();
    assert!(cold.compiled > 0 && cold.linked);
    run(&cold, 2);

    let warm = check(source);
    assert!(warm.rechecked.is_empty());
    let built = warm.build(&entry, &options).unwrap();
    assert_eq!(built.compiled, 0);
    assert!(!built.linked);
    run(&built, 2);

    // Three lines owned by a new definition, with existing item text unchanged.
    let moved = format!("fn added() -> u64 {{\n    0\n}}\n{source}");
    let product = check(&moved);
    assert!(!product.rechecked.contains(&entry));
    assert!(!product.rechecked.contains(&overflow));
    let built = product.build(&entry, &options).unwrap();
    assert_eq!(built.compiled, 0, "a line move must reuse every reached body object");
    assert!(built.linked, "current source belongs in the fresh link product");
    run(&built, 5);
}

#[test]
#[ignore = "explicit native integration target"]
fn failure_sites_cranelift_dev() { failure_sites_rebind(Backend::Cranelift, false, true); }
#[test]
#[ignore = "explicit native integration target"]
fn failure_sites_cranelift_release() { failure_sites_rebind(Backend::Cranelift, true, true); }
#[test]
#[ignore = "explicit native integration target"]
fn failure_sites_llvm_dev() { failure_sites_rebind(Backend::Llvm, false, true); }
#[test]
#[ignore = "explicit native integration target"]
fn failure_sites_llvm_release() { failure_sites_rebind(Backend::Llvm, true, true); }

#[test]
#[ignore = "explicit native integration target"]
fn failure_sites_cranelift_release_default() { failure_sites_rebind(Backend::Cranelift, true, false); }
#[test]
#[ignore = "explicit native integration target"]
fn failure_sites_llvm_release_default() { failure_sites_rebind(Backend::Llvm, true, false); }

#[test]
#[ignore = "explicit native integration target"]
fn failure_sites_cranelift_dev_default() { failure_sites_rebind(Backend::Cranelift, false, false); }
#[test]
#[ignore = "explicit native integration target"]
fn failure_sites_llvm_dev_default() { failure_sites_rebind(Backend::Llvm, false, false); }
