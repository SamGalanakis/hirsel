// A source-level List allocation regression through each native engine and runtime.
fn list_allocation(backend: Backend) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let cases = include_str!("../../../core/front/cases/body/list-with-capacity.hirsel");
    let source = format!(
        "{cases}\npub fn main() -> Result[(), Exit] {{ if exact(3)==3u64 && exact(0)==0u64 && overflow()==18446744073709551615u64 && padding()==3u64 && short_limit()==3231u64 && empty_limit()==0u64 && zero_sized()==18446744073709551615u64 {{ Ok(()) }} else {{ Error(Exit(status:1)) }} }}"
    );
    let mut request = source_request(&source, dir.path());
    request.inputs.packages[1] =
        PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
    let product = Driver::default()
        .check(request, &mut Meter::new(false))
        .unwrap();
    let options = native_options(backend, dir.path());
    let built = product
        .build(&DefPath::from_full("app@1/main/main").unwrap(), &options)
        .unwrap();
    run_clean(&built.executable, 0);
}

#[test]
#[ignore = "explicit native integration target"]
fn list_allocation_cranelift() {
    list_allocation(Backend::Cranelift);
}
#[test]
#[ignore = "explicit native integration target"]
fn list_allocation_llvm() {
    list_allocation(Backend::Llvm);
}

fn list_lookup(backend: Backend) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let cases = include_str!("../../../core/front/cases/body/list-get.hirsel");
    let source = format!(
        "{cases}\npub fn main() -> Result[(), Exit] {{ if byte_at(0)==97u8 && byte_at(2)==99u8 && byte_at(4)==255u8 && byte_at(18446744073709551615u64)==255u8 && generic()==1u64 && generic_value()==99u8 {{ Ok(()) }} else {{ Error(Exit(status:1)) }} }}"
    );
    let mut request = source_request(&source, dir.path());
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
fn list_lookup_cranelift() {
    list_lookup(Backend::Cranelift);
}
#[test]
#[ignore = "explicit native integration target"]
fn list_lookup_llvm() {
    list_lookup(Backend::Llvm);
}
