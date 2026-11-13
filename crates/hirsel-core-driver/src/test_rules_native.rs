fn prog4_source_rules(backend: Backend) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let entry = DefPath::from_full("app@1/main/success").unwrap();
    let mut options = native_options(backend, dir.path());
    for (case, accepted) in [
        ("test-functions", true),
        ("test-signatures-refused", false),
        ("test-references-refused", false),
    ] {
        let source = std::fs::read_to_string(root.join(format!("core/front/cases/body/{case}.hirsel"))).unwrap();
        let mut request = source_request(&source, dir.path());
        request.entry = Some(entry.clone());
        request.inputs.packages[1] = PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
        let product = Driver::default().check(request, &mut Meter::new(false)).unwrap();
        assert_eq!(product.accepted(), accepted, "{case}: {:?}", product.diagnostics);
        if !accepted {
            assert!(product.incomplete.is_empty(), "{:?}", product.incomplete);
            assert!(product.diagnostics.iter().all(|d| d.rule == "PROG-4"), "{:?}", product.diagnostics);
            continue;
        }
        let built = product.build(&entry, &options).unwrap();
        run_clean(&built.executable, 0);
        // The accepted reference paths run too: direct call and function value.
        let mut request = source_request(&source, dir.path());
        let entry = DefPath::from_full("app@1/main/accepted").unwrap();
        request.entry = Some(entry.clone());
        request.inputs.packages[1] = PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
        let product = Driver::default().check(request, &mut Meter::new(false)).unwrap();
        assert!(product.accepted(), "{:?}", product.diagnostics);
        options.output = dir.path().join("accepted-references");
        let built = product.build(&entry, &options).unwrap();
        run_clean(&built.executable, 0);
    }
}

#[test]
#[ignore = "explicit native integration target"]
fn prog4_signature_and_reference_rules_cranelift() {
    prog4_source_rules(Backend::Cranelift);
}
#[test]
#[ignore = "explicit native integration target"]
fn prog4_signature_and_reference_rules_llvm() {
    prog4_source_rules(Backend::Llvm);
}

#[test]
fn prog4_published_test_marker_and_edits_keep_reference_restrictions() {
    let lib = tempfile::tempdir().unwrap();
    let source = "@test\npub fn probe() -> Result[(), ()] { Ok(()) }\n";
    let mut request = source_request(source, lib.path());
    request.entry = None;
    let (product, image) = Driver::default().produce_image(request, &mut Meter::new(false)).unwrap();
    assert!(product.accepted(), "{:?}", product.diagnostics);
    let path = image.unwrap().store_root();
    let image = CoreImage::open(&path).unwrap().unwrap();
    let app = tempfile::tempdir().unwrap();
    let mut request = crate::tests::request("fn call() -> Result[(), ()] { app.main.probe() }", app.path());
    request.inputs.packages[0].package = package("consumer");
    request.inputs.packages[0].dependencies.extend([package("core"), package("app")]);
    request.library = LibraryInput::Image(image);
    let refused = Driver::default().check(request, &mut Meter::new(false)).unwrap();
    assert_eq!(refused.diagnostics.len(), 1, "{:?}", refused.diagnostics);
    assert_eq!(refused.diagnostics[0].rule, "PROG-4");
    let dir = tempfile::tempdir().unwrap();
    let source = "fn probe() -> Result[(), ()] { Ok(()) }\nfn ordinary() -> Result[(), ()] { probe() }\n";
    let mut request = source_request(source, dir.path());
    request.entry = None;
    assert!(Driver::default().check(request, &mut Meter::new(false)).unwrap().accepted());
    let mut request = source_request(&format!("@test\n{source}"), dir.path());
    request.entry = None;
    let refused = Driver::default().check(request, &mut Meter::new(false)).unwrap();
    assert_eq!(refused.diagnostics.len(), 1, "{:?}", refused.diagnostics);
    assert_eq!(refused.diagnostics[0].rule, "PROG-4");
}
