fn prog4_source_rules(backend: Backend) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let entry = DefPath::from_full("app@1/main/success").unwrap();
    let mut options = native_options(backend, dir.path());
    for (case, source, accepted) in [
        ("test-functions", include_str!("../../../core/front/cases/body/test-functions.hirsel"), true),
        ("test-signatures-refused", include_str!("../../../core/front/cases/body/test-signatures-refused.hirsel"), false),
        ("test-references-refused", include_str!("../../../core/front/cases/body/test-references-refused.hirsel"), false),
    ] {
        let mut request = source_request(source, dir.path());
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
        let mut request = source_request(source, dir.path());
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

#[test]
fn default_check_resolves_frozen_foreign_handles() {
    let law = PackageLaw::new();
    law.write("hirsel.toml", "[workspace]\nmembers = [\"codec\", \"app\"]\n");
    law.app("app", "[dependencies]\ncodec = { major = 1, path = \"../codec\" }\n");
    law.write(
        "app/main.hirsel",
        "pub fn open(f: Foreign[codec.Codec]) waits { codec.open(f) }\n",
    );
    law.manifest(
        "codec",
        "codec",
        1,
        r#"
[sources]
"codec.hirsel" = "codec"
"native.hirsel" = "native"

[foreign.native]
symbols = "fixture_"
archives = [{ target = "x86_64-unknown-linux-gnu", file = "native.a", sha256 = "f0a17a43c74d2fe5474fa2fd29c8f14799e777d7d75a2cc4d11c20a6e7b161c5" }]
"#,
    );
    law.write(
        "codec/codec.hirsel",
        r#"
pub opaque type Codec {}
pub opaque type Handle {}

pub fn open(f: Foreign[Codec]) waits {
    if let Some(p) = native.open(f) {
        native.close(f, p)
    }
}
"#,
    );
    law.write(
        "codec/native.hirsel",
        "pub fn open(f: Foreign[codec.Codec]) -> Option[foreign.Ptr[codec.Handle]]\npub fn close(f: Foreign[codec.Codec], take p: foreign.Ptr[codec.Handle]) waits\n",
    );
    law.write("codec/native.a", b"!<arch>\n");
    PackageLaw::pass(law.run(&["check", "app", "--metrics", "metrics.json"]));
    let metrics: serde_json::Value =
        serde_json::from_slice(&std::fs::read(law.path("metrics.json")).unwrap()).unwrap();
    assert_eq!(metrics["image_selection"], "image used: workspace dependencies");
}

#[test]
fn package_cred_grants_are_per_test_and_replay_uses_the_tape() {
    let law = PackageLaw::new();
    let (east, east_server) = fake_imds(b"fixture-east", b"east-token".to_vec(), 1);
    let (west, west_server) = fake_imds(b"fixture-west", b"west-token".to_vec(), 1);
    law.app("app", &format!(
        "[tests]\nlimit = 1048576\ngrants = [\"Cred:east=imds:127.0.0.1:{east} mac\", \"Cred:west=imds:127.0.0.1:{west} mac\"]\n",
    ));
    law.write(
        "app/main.hirsel",
        r#"
fn check_token(c: Cred, expected: String) -> Result[(), ()] waits {
    let mut key_id = string.with_capacity(64)
    let mut token = string.with_capacity(64)
    let _ = result.unwrap(cred.fetch(c, mut key_id, mut token, Never))
    if token == expected { Ok(()) } else { Error(()) }
}

@test
fn eastward(east: Cred) -> Result[(), ()] waits { check_token(east, "east-token") }
@test
fn westward(west: Cred) -> Result[(), ()] waits { check_token(west, "west-token") }
@test
fn pure() -> Result[(), ()] { Ok(()) }
"#,
    );
    let record = PathBuf::from(env_record_runtime()).canonicalize().unwrap();
    let options = native_options(Backend::Cranelift, law.dir.path());
    let cc = options.link.driver().canonicalize().unwrap();
    let args = [
        "test",
        "app",
        "--libraries",
        law.libraries.to_str().unwrap(),
        "--store",
        options.store.to_str().unwrap(),
        "--source-core",
        "--json",
        "--runtime",
        record.to_str().unwrap(),
        "--cc",
        cc.to_str().unwrap(),
        "--replay",
        "--expect-verdict",
        "Sealed",
    ]
    .map(str::to_owned);
    let stdout = PackageLaw::pass(process::tape_request(&args, law.dir.path()));
    let report: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let tests = report["tests"].as_array().unwrap();
    assert_eq!(tests.len(), 3, "{report}");
    assert!(tests.iter().all(|row| row["passed"] == true), "{report}");
    east_server.join().unwrap();
    west_server.join().unwrap();
}

#[test]
fn default_test_store_rebuilds_when_named_grants_change() {
    let law = PackageLaw::new();
    let configure = |variable| {
        law.manifest(
            "app",
            "app",
            1,
            &format!(
                r#"[sources]
"main.hirsel" = "main"

[tests]
limit = 1048576

[tests.runs."app@1/main/reads_env"]
grants = ["Stdout", "Env={variable}"]
"#,
            ),
        );
    };
    law.write(
        "app/main.hirsel",
        r#"
@test
fn reads_env(e: Env, out: Stdout) -> Result[(), ()] waits {
    match env.get(e, "HIRSEL_TEST_FIXTURE") {
        Ok(Some(value)) => result.unwrap(stdout.write(out, value)),
        _ => result.unwrap(stdout.write(out, "denied")),
    }
    Ok(())
}
"#,
    );
    let options = native_options(Backend::Cranelift, law.dir.path());
    let Link::Driver(cc) = options.link else {
        unreachable!()
    };
    let args = [
        "test".into(),
        "app".into(),
        "--libraries".into(),
        law.libraries.to_str().unwrap().into(),
        "--runtime".into(),
        options.runtime.canonicalize().unwrap().to_str().unwrap().into(),
        "--cc".into(),
        cc.canonicalize().unwrap().to_str().unwrap().into(),
        "--json".into(),
    ];
    let request = || {
        let mut request = process::tape_request_command(&args, law.dir.path());
        request
            .env("XDG_CACHE_HOME", law.path("cache"))
            .env("HIRSEL_TEST_FIXTURE", "visible");
        request
    };
    let run = || {
        let stdout = PackageLaw::pass(process::tape_request_output(request()));
        let result: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(result["tests"][0]["source"], "executed");
        result["tests"][0]["stdout"].as_str().unwrap().to_owned()
    };
    configure("HIRSEL_TEST_FIXTURE");
    assert_eq!(run(), "visible");
    assert!(law.path("app/.hirsel-core/native").is_dir());
    configure("HIRSEL_TEST_OTHER");
    assert_eq!(run(), "denied");

    let listener = std::os::unix::net::UnixListener::bind(law.path("app/.hirsel-core/resident.sock"))
        .unwrap();
    let server = std::thread::spawn(move || {
        use std::io::Read;

        let (mut client, _) = listener.accept().unwrap();
        let mut length = [0; 4];
        client.read_exact(&mut length).unwrap();
        let request: serde_json::Value =
            serde_json::from_reader(client.take(u64::from(u32::from_le_bytes(length)))).unwrap();
        assert_eq!(request["args"][0], "test");
    });
    let (status, stdout, stderr) = process::tape_request_output(request());
    assert_eq!(status, Some(2), "{stdout}\n{stderr}");
    assert!(stdout.is_empty(), "{stdout}");
    assert!(stderr.contains("the server ended without an answer"), "{stderr}");
    server.join().unwrap();
}
