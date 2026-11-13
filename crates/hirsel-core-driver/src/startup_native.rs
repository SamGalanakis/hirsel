// CAP-4/PROG-3: setup refusals are process outcomes, before any program effect.
fn assert_not_started(output: &std::process::Output, line: &str) {
    assert_eq!(output.status.code(), Some(103), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    assert_eq!(output.stderr, line.as_bytes());
}

#[test]
fn entry_parameter_names_survive_disk_reuse() {
    let dir = tempfile::tempdir().unwrap();
    let source = "pub fn main(argv: Args, output: Stdout) -> Result[(), Exit] { Ok(()) }";
    for warm in [false, true] {
        let product = Driver::default()
            .check(source_request(source, dir.path()), &mut Meter::new(false))
            .unwrap();
        assert!(product.accepted());
        assert_eq!(product.rechecked.is_empty(), warm);
        let abi = product.native_entry.as_ref().unwrap().1.as_ref().unwrap();
        let parameters = abi
            .parameters
            .iter()
            .map(|p| (p.name.as_str(), p.ty.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(
            parameters,
            [
                ("argv", "core@1/core/Args"),
                ("output", "core@1/core/Stdout")
            ]
        );
    }
}

fn startup_refusals(backend: Backend) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let effects = dir.path().join("effects");
    std::fs::create_dir(&effects).unwrap();
    let mut options = native_options(backend, dir.path());
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let source = r#"pub fn main(files: Dir, output: Stdout) -> Result[(), Exit] waits {
        let f = result.unwrap(host.open(files, "effect", CreateNew));
        result.unwrap(host.close(take f));
        result.unwrap(stdout.write(output, "entered\n"));
        Ok(())
    }"#;
    let mut request = source_request(source, dir.path());
    request.inputs.packages[1] =
        PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
    let product = Driver::default()
        .check(request, &mut Meter::new(false))
        .unwrap();
    assert!(
        product.accepted(),
        "{}",
        serde_json::to_string(&(
            product.machine_diagnostics().unwrap(),
            product.machine_incomplete().unwrap()
        ))
        .unwrap()
    );

    for (missing, directories, grants) in [
        ("files", vec![], 1 << 4),
        ("output", vec![effects.display().to_string()], 0),
    ] {
        options.directories = directories;
        options.grant_mask = grants;
        options.output = dir.path().join(format!("missing-{missing}"));
        let built = product.build(&entry, &options).unwrap();
        assert_not_started(
            &Command::new(built.executable).output().unwrap(),
            &format!("not started: grant `{missing}` is not in the run's configuration\n"),
        );
        assert_eq!(std::fs::read_dir(&effects).unwrap().count(), 0);
    }

    let launcher_source = dir.path().join("host.c");
    let launcher = dir.path().join("host");
    std::fs::write(&launcher_source, include_str!("deny_openat2.c")).unwrap();
    command(
        Command::new(&options.cc)
            .args(["-std=c11", "-O0"])
            .arg(&launcher_source)
            .arg("-o")
            .arg(&launcher),
    )
    .unwrap();
    options.output = dir.path().join("unavailable-dir");
    options.directories = vec![effects.display().to_string()];
    options.grant_mask = 1 << 4;
    let built = product.build(&entry, &options).unwrap();
    assert_not_started(
        &Command::new(&launcher)
            .arg(built.executable)
            .output()
            .unwrap(),
        "not started: grant `files`: this host does not permit openat2, which a Dir needs (Linux 5.6 or later, allowed by every sandbox in force)\n",
    );
    assert_eq!(std::fs::read_dir(&effects).unwrap().count(), 0);

    let source = r#"pub fn main(output: Stdout) -> Result[(), Exit] waits {
        result.unwrap(stdout.write(output, "entered\n"));
        Ok(())
    }"#;
    let mut request = source_request(source, dir.path());
    request.inputs.packages[1] =
        PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
    let product = Driver::default()
        .check(request, &mut Meter::new(false))
        .unwrap();
    assert!(product.accepted());
    options.directories.clear();
    // MEM-19's logical task record and primary cause cell charge: 256 + 32.
    // These hand-derived boundary cases also prove that refusal is before main.
    for limit in [0, 287] {
        options.root_limit = limit;
        options.output = dir.path().join(format!("limit-{limit}"));
        let built = product.build(&entry, &options).unwrap();
        assert_not_started(
            &Command::new(built.executable).output().unwrap(),
            "not started: the root limit is below main's startup charge of 288 bytes\n",
        );
    }
    options.root_limit = 288;
    options.output = dir.path().join("no-dir");
    let built = product.build(&entry, &options).unwrap();
    let output = Command::new(launcher)
        .arg(built.executable)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(output.stdout, b"entered\n");
    assert!(output.stderr.is_empty());
    assert_eq!(std::fs::read_dir(effects).unwrap().count(), 0);
}
#[test]
#[ignore = "explicit native integration target"]
fn prog3_startup_refusals_cranelift() {
    startup_refusals(Backend::Cranelift);
}
#[test]
#[ignore = "explicit native integration target"]
fn prog3_startup_refusals_llvm() {
    startup_refusals(Backend::Llvm);
}
