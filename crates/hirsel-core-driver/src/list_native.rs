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

// Exercise growth through checked generic bodies and real admission/refund paths.
fn list_growth(backend: Backend) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for (cases, check, exit, limit) in [
        (
            include_str!("../../../core/front/cases/body/list-reserve.hirsel"),
            "fresh()==4u64 && no_growth()==9u64 && current_payer()==4u64 && failed_growth()==13231u64 && refunded_growth()==46408u64 && padded()==47216u64 && static_contents()==97100u64 && static_capacity()==408u64 && count_overflow()==18446744073709551615u64",
            0,
            1_048_576,
        ),
        (
            include_str!("../../../core/front/cases/body/list-push.hirsel"),
            "fresh_push()==711u64 && grew()==711u64 && returned_input()==73231u64 && stored_input()==7u64",
            0,
            1_048_576,
        ),
        // A second resource allocation under the same finite root budget proves
        // that the failed push returned an input that was dropped and refunded.
        (
            include_str!("../../../core/front/cases/body/list-push.hirsel"),
            "returned_input()==73231u64 && returned_input()==73231u64",
            0,
            288 + 71,
        ),
        (
            include_str!("../../../core/front/cases/body/list-push.hirsel"),
            "grant_failure()==0u64",
            101,
            1_048_576,
        ),
        (
            include_str!("../../../core/front/cases/body/list-append.hirsel"),
            "appended()==606u64 && unchanged()==0u64 && rejected()==404009u64",
            0,
            1_048_576,
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let source = format!(
            "{cases}\npub fn main() -> Result[(), Exit] {{ if {check} {{ Ok(()) }} else {{ Error(Exit(status:1)) }} }}"
        );
        let mut request = source_request(&source, dir.path());
        request.inputs.packages[1] =
            PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
        let product = Driver::default()
            .check(request, &mut Meter::new(false))
            .unwrap();
        let mut options = native_options(backend, dir.path());
        options.root_limit = limit;
        let built = product
            .build(&DefPath::from_full("app@1/main/main").unwrap(), &options)
            .unwrap_or_else(|e| panic!("{check}: {e:?}"));
        let output = Command::new(&built.executable).output().unwrap();
        assert_eq!(output.status.code(), Some(exit), "{check}: {output:?}");
        assert!(output.stdout.is_empty(), "{check}: {output:?}");
    }
}
#[test]
#[ignore = "explicit native integration target"]
fn list_growth_cranelift() {
    list_growth(Backend::Cranelift);
}
#[test]
#[ignore = "explicit native integration target"]
fn list_growth_llvm() {
    list_growth(Backend::Llvm);
}

fn list_shrink(backend: Backend) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for (cases, check, exit) in [
        (
            include_str!("../../../core/front/cases/body/list-pop.hirsel"),
            "values()==110704u64 && empty()==0u64 && static_value()==990203u64 && resource_value()==7u64",
            0,
        ),
        (
            include_str!("../../../core/front/cases/body/list-clear.hirsel"),
            "reuse()==904u64 && static_value()==3u64 && resources()==4u64",
            0,
        ),
        (
            include_str!("../../../core/front/cases/body/list-clear.hirsel"),
            "failed_drop()==0u64",
            101,
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let source = format!(
            "{cases}\npub fn main() -> Result[(), Exit] {{ if {check} {{ Ok(()) }} else {{ Error(Exit(status:1)) }} }}"
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
            .unwrap_or_else(|e| panic!("{check}: {e:?}"));
        let output = Command::new(&built.executable).output().unwrap();
        assert_eq!(output.status.code(), Some(exit), "{check}: {output:?}");
        assert!(output.stdout.is_empty(), "{check}: {output:?}");
    }
}
#[test]
#[ignore = "explicit native integration target"]
fn list_shrink_cranelift() {
    list_shrink(Backend::Cranelift);
}
#[test]
#[ignore = "explicit native integration target"]
fn list_shrink_llvm() {
    list_shrink(Backend::Llvm);
}

fn list_slicing(backend: Backend) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let cases = include_str!("../../../core/front/cases/body/list-slice.hirsel");
    for (check, exit) in [
        (
            "nested()==980202u64 && empty()==0u64 && sized_elements()==110202u64 && unit()==0u64 && resources()==713u64 && stored()==990202u64",
            0,
        ),
        ("reversed()==0u64", 101),
        ("past()==0u64", 101),
        ("max_index()==0u64", 101),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let source = format!(
            "{cases}\npub fn main() -> Result[(), Exit] {{ if {check} {{ Ok(()) }} else {{ Error(Exit(status:1)) }} }}"
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
            .unwrap_or_else(|e| panic!("{check}: {e:?}"));
        let output = Command::new(&built.executable).output().unwrap();
        assert_eq!(output.status.code(), Some(exit), "{check}: {output:?}");
        assert!(output.stdout.is_empty(), "{check}: {output:?}");
        if exit == 101 {
            assert!(output.stderr.starts_with(b"Bounds"), "{check}: {output:?}");
        } else {
            assert!(output.stderr.is_empty(), "{check}: {output:?}");
        }
    }
}
#[test]
#[ignore = "explicit native integration target"]
fn list_slicing_cranelift() {
    list_slicing(Backend::Cranelift);
}
#[test]
#[ignore = "explicit native integration target"]
fn list_slicing_llvm() {
    list_slicing(Backend::Llvm);
}

fn list_replacement(backend: Backend) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let cases = include_str!("../../../core/front/cases/body/list-set-static-gap.hirsel");
    for (check, cause) in [
        (
            "literal()==97099100u64 && generic_byte()==709u64 && generic_word()==72623859790382856u64 && resources()==711u64 && assigned_resource()==11u64",
            None,
        ),
        ("failed_set()==0u64", Some("Bounds")),
        ("failed_assign_drop()==0u64", Some("Unwrap")),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let source = format!(
            "{cases}\npub fn main()->Result[(),Exit] {{ if {check} {{ Ok(()) }} else {{ Error(Exit(status:1)) }} }}"
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
            .unwrap_or_else(|e| panic!("{check}: {e:?}"));
        let output = Command::new(&built.executable).output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(if cause.is_some() { 101 } else { 0 }),
            "{check}: {output:?}"
        );
        assert!(output.stdout.is_empty(), "{check}: {output:?}");
        if let Some(cause) = cause {
            assert!(
                output.stderr.starts_with(cause.as_bytes()),
                "{check}: {output:?}"
            );
        } else {
            assert!(output.stderr.is_empty(), "{check}: {output:?}");
        }
    }
}
#[test]
#[ignore = "explicit native integration target"]
fn list_replacement_cranelift() {
    list_replacement(Backend::Cranelift);
}
#[test]
#[ignore = "explicit native integration target"]
fn list_replacement_llvm() {
    list_replacement(Backend::Llvm);
}

fn list_zero_sized(backend: Backend) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let cases = include_str!("../../../core/front/cases/body/list-reserve-zero-gap.hirsel");
    let source = format!(
        "{cases}\npub fn main() -> Result[(), Exit] {{ if fresh()==18446744073709551615u64 && generic_fresh()==18446744073709551615u64 && exact() && reserve_overflow() && max_count() && generic_ops() {{ Ok(()) }} else {{ Error(Exit(status:1)) }} }}"
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
fn list_zero_sized_cranelift() {
    list_zero_sized(Backend::Cranelift);
}
#[test]
#[ignore = "explicit native integration target"]
fn list_zero_sized_llvm() {
    list_zero_sized(Backend::Llvm);
}
