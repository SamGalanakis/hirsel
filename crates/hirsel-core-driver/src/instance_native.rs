// Stored generic instances use the same layout identities and ABI in both engines.
fn anchored_instance_key(backend: Backend) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let source = include_str!("../../../core/front/cases/body/instance-anchored.hirsel");
    let mut request = source_request(source, dir.path());
    request.inputs.packages[1] =
        PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
    let product = Driver::default()
        .check(request, &mut Meter::new(false))
        .unwrap();
    let built = product
        .build(
            &DefPath::from_full("app@1/main/main").unwrap(),
            &native_options(backend, dir.path()),
        )
        .unwrap();
    run_clean(&built.executable, 0);
}
#[test]
#[ignore = "explicit native integration target"]
fn anchored_instance_key_cranelift() {
    anchored_instance_key(Backend::Cranelift);
}
#[test]
#[ignore = "explicit native integration target"]
fn anchored_instance_key_llvm() {
    anchored_instance_key(Backend::Llvm);
}
