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
        law.dir.path().join("store").to_str().unwrap(),
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
            .env("XDG_RUNTIME_DIR", law.path("runtime"))
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
    assert!(law.path("app/.hirsel-core").is_dir() && law.path("cache/hirsel/cas/keys").is_dir());
    configure("HIRSEL_TEST_OTHER");
    assert_eq!(run(), "denied");

    let socket = crate::resident::socket_under(&law.path("runtime"));
    std::os::unix::fs::DirBuilderExt::mode(&mut std::fs::DirBuilder::new(), 0o700)
        .recursive(true)
        .create(socket.parent().unwrap())
        .unwrap();
    let listener = std::os::unix::net::UnixListener::bind(socket).unwrap();
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

/// R3's warnings in a build's stderr, one per ungranted `main` parameter.
fn ungranted_warnings(stderr: &str) -> Vec<&str> {
    stderr.lines().filter(|line| line.starts_with("hirsel: warning: grant")).collect()
}

fn ungranted(name: &str) -> String {
    format!("hirsel: warning: grant `{name}` is not in the build's configuration; the executable will not start")
}

const PROGRAM_GRANTED: &str = "pub fn main(out: Stdout, clock: Clock) -> Result[(), Exit] waits {\n    let _ = host.mono_ns(clock)\n    result.unwrap(stdout.write(out, \"granted\\n\"))\n    Ok(())\n}\n";

/// Law A (R3): `[program]` grants and limit reach the recipe, so the built
/// executable starts with no flags of its own. Red when the keys are parsed
/// but never applied (the launch refuses `out` and `clock`, status 103).
#[test]
fn program_manifest_grants_build_an_executable_that_starts() {
    let law = PackageLaw::new();
    law.manifest("app", "app", 1, "[sources]\n\"main.hirsel\" = \"main\"\n\n[program]\nlimit = 1048576\ngrants = [\"Stdout\", \"Clock\"]\n");
    law.write("app/main.hirsel", PROGRAM_GRANTED);
    let (status, stdout, stderr) = law.run(&["build", "app", "--output", "program"]);
    assert_eq!(status, Some(0), "{stdout}\n{stderr}");
    assert_eq!(ungranted_warnings(&stderr), Vec::<&str>::new());
    let run = Command::new(law.path("program")).output().unwrap();
    assert_eq!(run.status.code(), Some(0), "{}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(run.stdout, b"granted\n");
}

/// Law B (R3): a package that declares `[program]` grants takes none from the
/// command line, as `[tests]` does (PROG-4). Red when `--dir`, `--grant` or
/// `--limit` silently overrides or joins the manifest.
#[test]
fn program_manifest_grants_refuse_command_line_grants() {
    let law = PackageLaw::new();
    law.manifest("app", "app", 1, "[sources]\n\"main.hirsel\" = \"main\"\n\n[program]\ngrants = [\"Stdout\", \"Clock\"]\n");
    law.write("app/main.hirsel", PROGRAM_GRANTED);
    let reason = "this package declares [program] grants in hirsel.toml. Set limit, dirs and grants there";
    for flags in [&["--dir", "data"][..], &["--grant", "Stdout"], &["--limit", "1048576"]] {
        for command in ["build", "run"] {
            let mut args = vec![command, "app"];
            args.extend(flags);
            let (status, stdout, stderr) = law.run(&args);
            assert_eq!(status, Some(2), "{command} {flags:?}: {stdout}\n{stderr}");
            assert!(stderr.ends_with(&format!("hirsel: {reason}\n")), "{command} {flags:?}: {stderr}");
        }
    }
}

/// Law C (R3, guard): a dependency's `[program]` is never read, so it cannot
/// grant the program that binds it anything.
#[test]
fn program_grants_of_a_dependency_are_never_applied() {
    let law = PackageLaw::new();
    law.write("work/hirsel.toml", "[workspace]\nmembers = [\"app\", \"util\"]\n");
    law.manifest("work/app", "app", 1, "[sources]\n\"main.hirsel\" = \"main\"\n\n[dependencies]\nutil = { major = 1, path = \"../util\" }\n");
    law.write("work/app/main.hirsel", PROGRAM_GRANTED);
    law.library("work/util", "util", 1, "pub fn one() -> u64 { 1u64 }\n", "[program]\ngrants = [\"Stdout\", \"Clock\"]\n");
    let (status, stdout, stderr) = law.run(&["build", "work/app", "--output", "program"]);
    assert_eq!(status, Some(0), "{stdout}\n{stderr}");
    assert_eq!(ungranted_warnings(&stderr), [ungranted("out"), ungranted("clock")]);
}

/// Law D (R3): a build names every `main` parameter it leaves ungranted, in
/// parameter order, and is still accepted. `dir` takes the manifest's one
/// directory and `clock` its grant, so only `out` and `err` are named. Red
/// with no warning, with the warning in kind order, or with `dirs` dropped.
#[test]
fn program_build_warns_for_each_ungranted_parameter_in_order() {
    let law = PackageLaw::new();
    law.manifest("app", "app", 1, "[sources]\n\"main.hirsel\" = \"main\"\n\n[program]\ndirs = [\"data\"]\ngrants = [\"Clock\"]\n");
    law.write(
        "app/main.hirsel",
        "pub fn main(err: Stderr, dir: Dir, clock: Clock, out: Stdout) -> Result[(), Exit] waits {\n    let _ = host.mono_ns(clock)\n    result.unwrap(stdout.write(out, \"x\"))\n    result.unwrap(stderr.write(err, \"x\"))\n    Ok(())\n}\n",
    );
    let (status, stdout, stderr) = law.run(&["build", "app", "--output", "program"]);
    assert_eq!(status, Some(0), "{stdout}\n{stderr}");
    assert_eq!(ungranted_warnings(&stderr), [ungranted("err"), ungranted("out")]);
}

/// The server of the resident laws: this toolchain's `hirsel serve`, under
/// the `XDG_RUNTIME_DIR` its law names.
#[test]
#[ignore = "explicit native integration target"]
fn resident_child() {
    if std::env::var_os("RESIDENT_CHILD").is_none() {
        return;
    }
    // SAFETY: no other thread of this child reads or writes the environment yet.
    unsafe { std::env::remove_var("RESIDENT_CHILD") };
    crate::request::main(vec!["serve".into()])
}

/// Start the resident laws' server in `directory` under `runtime`, and wait
/// until it listens.
fn start_resident(runtime: &Path, directory: &Path, shared: impl Fn(&mut std::process::Command)) -> std::process::Child {
    let mut server = std::process::Command::new(std::env::current_exe().unwrap());
    server
        .args(["--exact", "native::tests::resident_child", "--ignored", "--nocapture", "--test-threads=1"])
        .env("REQUEST_CHILD_ARGS", "[]")
        .env("RESIDENT_CHILD", "1")
        .env("XDG_RUNTIME_DIR", runtime)
        .env("PWD", directory)
        .current_dir(directory)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    shared(&mut server);
    let mut server = server.spawn().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !crate::resident::socket_under(runtime).exists() {
        assert!(server.try_wait().unwrap().is_none() && std::time::Instant::now() < deadline, "the server did not start");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    server
}

/// Stop the resident laws' server under `runtime`.
fn stop_resident(runtime: &Path, directory: &Path, mut server: std::process::Child) {
    let mut stop = process::tape_request_command(&["serve".into(), "--stop".into()], directory);
    stop.env("XDG_RUNTIME_DIR", runtime);
    let (stopped, stdout, stderr) = process::tape_request_output(stop);
    assert_eq!(stopped, Some(0), "{stdout}\n{stderr}");
    assert!(server.wait().unwrap().success());
}

/// S3 bug law: a test run through the resident compiler gives the rows,
/// verdicts and pass-cache writes of a one-shot run, though the server runs in
/// another directory under another environment. The client runs each test, so
/// a test reads the client's environment (here PWD, which a server does not
/// compare) and its pass is written where the client's run writes it.
#[test]
fn resident_test_session_gives_the_one_shot_run() {
    let law = PackageLaw::new();
    law.manifest(
        "app",
        "app",
        1,
        "[sources]\n\"main.hirsel\" = \"main\"\n\n[tests]\nlimit = 1048576\n\n[tests.runs.\"app@1/main/reads_pwd\"]\ngrants = [\"Stdout\", \"Env=PWD\"]\n",
    );
    law.write(
        "app/main.hirsel",
        r#"
@test
fn reads_pwd(e: Env, out: Stdout) -> Result[(), ()] waits {
    match env.get(e, "PWD") {
        Ok(Some(value)) => result.unwrap(stdout.write(out, value)),
        _ => result.unwrap(stdout.write(out, "denied")),
    }
    Ok(())
}
@test
fn determined() -> Result[(), ()] { Ok(()) }
@test
fn error() -> Result[(), ()] { Error(()) }
"#,
    );
    let options = native_options(Backend::Cranelift, law.dir.path());
    let Link::Driver(cc) = options.link else {
        unreachable!()
    };
    let path = |path: PathBuf| path.canonicalize().unwrap().to_str().unwrap().to_owned();
    let store = law.path("compiler-store");
    let args: Vec<String> = vec![
        "test".into(),
        "app".into(),
        "--libraries".into(),
        law.libraries.to_str().unwrap().into(),
        "--store".into(),
        store.to_str().unwrap().into(),
        "--runtime".into(),
        path(options.runtime),
        "--cc".into(),
        path(cc),
        "--json".into(),
    ];
    let client = law.dir.path().to_owned();
    let elsewhere = law.path("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    let results = law.path("cache/hirsel/core-packages/test-results");
    let runtime = law.path("runtime");
    let shared = |command: &mut std::process::Command| {
        command
            .env("XDG_CACHE_HOME", law.path("cache"))
            .env("XDG_RUNTIME_DIR", &runtime)
            .env("HIRSEL_TRACE_REQUEST_PHASES", "1");
    };
    fn files(root: &Path, at: &Path, into: &mut std::collections::BTreeMap<PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(at).into_iter().flatten() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                files(root, &path, into);
            } else {
                into.insert(path.strip_prefix(root).unwrap().to_owned(), std::fs::read(&path).unwrap());
            }
        }
    }
    let run = || {
        let mut request = process::tape_request_command(&args, &client);
        shared(request.env("PWD", &client));
        let (status, stdout, stderr) = process::tape_request_output(request);
        let mut report: serde_json::Value =
            serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("{e}: {stdout}\n{stderr}"));
        for row in report["tests"].as_array_mut().unwrap() {
            row.as_object_mut().unwrap().remove("wall_ms");
        }
        let mut written = std::collections::BTreeMap::new();
        files(&results, &results, &mut written);
        (status, report, written, stderr)
    };
    let one_shot = run();
    let rows = one_shot.1["tests"].as_array().unwrap();
    let row = |name: &str| rows.iter().find(|row| row["definition"] == name).unwrap();
    assert_eq!(one_shot.0, Some(1), "{}", one_shot.3);
    assert_eq!(row("app@1/main/reads_pwd")["stdout"], client.to_str().unwrap());
    assert_eq!(row("app@1/main/error")["passed"], false);
    assert_eq!(row("app@1/main/determined")["passed"], true);
    assert!(!one_shot.2.is_empty(), "the determined pass is stored");
    assert!(!one_shot.3.contains("hirsel-request-phases"), "{}", one_shot.3);
    std::fs::remove_dir_all(&results).unwrap();

    let server = start_resident(&runtime, &elsewhere, shared);
    let served = run();
    stop_resident(&runtime, &client, server);
    assert!(served.3.contains("hirsel-request-phases"), "the server did not serve: {}", served.3);
    assert_eq!((&served.0, &served.1, &served.2), (&one_shot.0, &one_shot.1, &one_shot.2));
}

/// S4 bug law: one server answers every directory and environment. A request
/// from another directory, under an environment the server was not started
/// with, gets a fresh process's answer, and a request that panics fails alone.
/// Two copies of a package, in `one` and `two` with their own default stores,
/// build through a server started in a third directory without the phase
/// trace their clients set; each answer carries the trace (the server took the
/// client's environment) and builds the program a one-shot build does. A
/// request whose serving panics answers 101, the server lives, and the next
/// request is served as before. Red where a server compares whole
/// environments (the clients' trace is refused and they compile themselves)
/// or where a panic ends the server (the third request finds none).
#[test]
fn resident_server_answers_every_directory_and_contains_a_panic() {
    let law = PackageLaw::new();
    for side in ["one", "two"] {
        law.manifest(&format!("{side}/app"), "app", 1, "[sources]\n\"main.hirsel\" = \"main\"\n\n[program]\ngrants = [\"Stdout\"]\n");
        law.write(
            &format!("{side}/app/main.hirsel"),
            format!("pub fn main(out: Stdout) -> Result[(), Exit] waits {{\n    result.unwrap(stdout.write(out, \"{side}\\n\"))\n    Ok(())\n}}\n"),
        );
    }
    let options = native_options(Backend::Cranelift, law.dir.path());
    let Link::Driver(cc) = options.link else {
        unreachable!()
    };
    let path = |path: PathBuf| path.canonicalize().unwrap().to_str().unwrap().to_owned();
    let args: Vec<String> = vec![
        "build".into(),
        "app".into(),
        "--output".into(),
        "program".into(),
        "--libraries".into(),
        law.libraries.to_str().unwrap().into(),
        "--runtime".into(),
        path(options.runtime),
        "--cc".into(),
        path(cc),
    ];
    let runtime = law.path("runtime");
    let shared = |command: &mut std::process::Command| {
        command.env("XDG_CACHE_HOME", law.path("cache")).env("XDG_RUNTIME_DIR", &runtime);
    };
    let build = |side: &str, panic: bool| {
        let directory = law.path(side);
        let _ = std::fs::remove_file(directory.join("program"));
        let mut request = process::tape_request_command(&args, &directory);
        shared(&mut request);
        request
            .env("PWD", &directory)
            .env("HIRSEL_TRACE_REQUEST_PHASES", "1")
            .env("HIRSEL_RESIDENT_LAW_SIDE", side);
        if panic {
            request.env("HIRSEL_RESIDENT_LAW_PANIC", "1");
        }
        let (status, stdout, stderr) = process::tape_request_output(request);
        let program = std::process::Command::new(directory.join("program")).output().ok();
        let ran = program.map(|run| (run.status.code(), run.stdout));
        ((status, stdout, ran), stderr)
    };
    let one_shot = [build("one", false), build("two", false)];
    for (side, (answer, stderr)) in ["one", "two"].iter().zip(&one_shot) {
        assert_eq!(answer.0, Some(0), "{stderr}");
        assert_eq!(answer.2, Some((Some(0), format!("{side}\n").into_bytes())), "{stderr}");
        assert!(!stderr.contains("hirsel-request-phases"), "{stderr}");
    }

    let elsewhere = law.path("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    let mut server = start_resident(&runtime, &elsewhere, shared);
    for (n, side) in ["one", "two"].into_iter().enumerate() {
        let (answer, stderr) = build(side, false);
        assert!(stderr.contains("hirsel-request-phases"), "{side} was not served: {stderr}");
        assert_eq!(answer, one_shot[n].0, "{side}: {stderr}");
    }
    let (panicked, stderr) = build("two", true);
    assert_eq!(panicked.0, Some(101), "{stderr}");
    assert!(server.try_wait().unwrap().is_none(), "the panic ended the server");
    let (answer, stderr) = build("one", false);
    assert!(stderr.contains("hirsel-request-phases"), "one was not served after the panic: {stderr}");
    assert_eq!(answer, one_shot[0].0, "{stderr}");
    stop_resident(&runtime, &elsewhere, server);
}

#[test]
fn test_reports_check_diagnostic_for_unparseable_test_source() {
    let law = PackageLaw::new();
    law.app(
        "app",
        "[tests]\nlimit = 67108864\n[tests.sources]\n\"laws.hirsel\" = \"laws\"\n",
    );
    law.write(
        "app/laws.hirsel",
        "@test\nfn broken() -> Result[(), ()] { Ok(())\n",
    );
    PackageLaw::pass(law.run(&["lock", "app"]));

    let check = law.run(&["check", "app"]);
    assert_eq!(check.0, Some(1), "{}\n{}", check.1, check.2);
    assert!(check.2.contains("LANG-5"), "{check:?}");

    let tested = law.run(&["test", "app"]);
    assert_eq!(tested.0, check.0, "{}\n{}", tested.1, tested.2);
    assert_eq!(tested.2, check.2);
    assert!(!tested.2.contains("no tests found"), "{tested:?}");
}
