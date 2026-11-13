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

fn scalar_task_entry(backend: Backend) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let source = include_str!("../../../core/front/cases/body/scalar-task-entry.hirsel");
    let mut request = source_request(source, dir.path());
    request.inputs.packages[1] =
        PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
    let product = Driver::default()
        .check(request, &mut Meter::new(false))
        .unwrap();
    let mut options = native_options(backend, dir.path());
    options.grant_mask = 1 << 9;
    let built = product
        .build(&DefPath::from_full("app@1/main/main").unwrap(), &options)
        .unwrap();
    run_clean(&built.executable, 0);
}
#[test]
#[ignore = "explicit native integration target"]
fn scalar_task_entry_cranelift() {
    scalar_task_entry(Backend::Cranelift);
}
#[test]
#[ignore = "explicit native integration target"]
fn scalar_task_entry_llvm() {
    scalar_task_entry(Backend::Llvm);
}

fn scalar_callable_instances(backend: Backend) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for (case, check) in [
        (
            include_str!("../../../core/front/cases/body/generic-instances.hirsel"),
            "width()==72623859790382856u64 && wrapped()==72623859790382856u64 && pair()==145247719580765712u64 && swapped()==9u8 && unit()==3u8",
        ),
        (
            include_str!("../../../core/front/cases/body/generic-callable-abi.hirsel"),
            "f()==7u8 && direct()==19u64 && zero()==9u64 && nested()==23u64",
        ),
        (
            include_str!("../../../core/front/cases/body/closures-generic.hirsel"),
            "scalar()==7u64 && unit()==9u64 && pair()==13u64 && plain()==23u64",
        ),
        (
            include_str!("../../../core/front/cases/body/callable-view-payload.hirsel"),
            "call(9u64)==10u64",
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let source = format!(
            "{case}\npub fn main()->Result[(),Exit] {{ if {check} {{Ok(())}}else{{Error(Exit(status:1))}} }}"
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
}
#[test]
#[ignore = "explicit native integration target"]
fn scalar_callable_instances_cranelift() {
    scalar_callable_instances(Backend::Cranelift);
}
#[test]
#[ignore = "explicit native integration target"]
fn scalar_callable_instances_llvm() {
    scalar_callable_instances(Backend::Llvm);
}

fn scalar_key_carriage(backend: Backend) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let source = include_str!("../../../core/front/cases/body/scalar-key-carriage.hirsel");
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
fn scalar_key_carriage_cranelift() {
    scalar_key_carriage(Backend::Cranelift);
}
#[test]
#[ignore = "explicit native integration target"]
fn scalar_key_carriage_llvm() {
    scalar_key_carriage(Backend::Llvm);
}
