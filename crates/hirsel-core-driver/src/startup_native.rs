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

    // Bug: the run stopped at the first missing grant, so each rebuild named one
    // more. Three kinds, the Secret read before the others: every one is named,
    // in parameter order.
    let three = r#"pub fn main(files: Dir, key: Secret, output: Stdout) -> Result[(), Exit] waits {
        result.unwrap(stdout.write(output, "entered\n"));
        Ok(())
    }"#;
    let mut request = source_request(three, dir.path());
    request.inputs.packages[1] =
        PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
    let three = Driver::default()
        .check(request, &mut Meter::new(false))
        .unwrap();
    assert!(three.accepted());
    options.directories = vec![];
    options.grant_mask = 0;
    options.output = dir.path().join("missing-three");
    let built = three.build(&entry, &options).unwrap();
    assert_not_started(
        &Command::new(built.executable).output().unwrap(),
        "not started: grant `files` is not in the run's configuration\n\
         not started: grant `key` [secret] has no readable nonempty source\n\
         not started: grant `output` is not in the run's configuration\n",
    );

    let launcher_source = dir.path().join("host.c");
    let launcher = dir.path().join("host");
    std::fs::write(&launcher_source, include_str!("deny_openat2.c")).unwrap();
    command(
        Command::new(options.link.driver())
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
        "not started: Dir requires openat2 (Linux 5.6 or later, permitted by the sandbox)\n",
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
    let output = Command::new(&launcher)
        .arg(&built.executable)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(output.stdout, b"entered\n");
    assert!(output.stderr.is_empty());
    assert_eq!(std::fs::read_dir(&effects).unwrap().count(), 0);
    // Bug: an argument that is not UTF-8 was reported as "host grants could
    // not be configured". PROG-3: its own line, numbered by its index in the
    // process argv (the program name is 0).
    use std::os::unix::ffi::OsStrExt;
    assert_not_started(
        &Command::new(&built.executable)
            .args([
                std::ffi::OsStr::new("ok"),
                std::ffi::OsStr::from_bytes(b"\xff"),
            ])
            .output()
            .unwrap(),
        "not started: argument 2 is not valid UTF-8\n",
    );

    // A recording opens its tape beneath the tape's directory, so the same
    // host refuses it by name before main, and writes no tape.
    options.runtime = env_record_runtime();
    options.recording = true;
    options.output = dir.path().join("no-dir-recorded");
    let built = product.build(&entry, &options).unwrap();
    let tape = effects.join("run.tape");
    assert_not_started(
        &Command::new(&launcher)
            .arg(built.executable)
            .env("HIRSEL_RECORD", &tape)
            .output()
            .unwrap(),
        "tape: the tape directory requires openat2 (Linux 5.6 or later, permitted by the sandbox)\n",
    );
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

/// Bug: `hirsel build` assembles from the release recipe, whose entry never
/// carried `E`'s description, so `main`'s `Error(e)` exited 1 with no line.
/// PROG-3 (`docs/spec/programs.md`): `error: ` and `e` as source, status 1; an
/// `Exit` error writes nothing and exits with its status.
#[test]
#[ignore = "explicit native integration target"]
fn prog3_recipe_build_writes_the_error_line() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let mut options = native_options(Backend::Llvm, dir.path());
    options.optimize = true;
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    for (name, source, status, stderr) in [
        (
            "text",
            "pub fn main() -> Result[(), String] { Error(\"boom\") }",
            1,
            "error: \"boom\"\n",
        ),
        (
            "exit",
            "pub fn main() -> Result[(), Exit] { Error(Exit(status: 2u8)) }",
            2,
            "",
        ),
    ] {
        let mut request = source_request(source, dir.path());
        request.inputs.packages[1] =
            PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
        let product = Driver::default()
            .check(request, &mut Meter::new(false))
            .unwrap();
        assert!(product.accepted());
        options.output = dir.path().join(name);
        let built = product.build(&entry, &options).unwrap();
        let output = Command::new(built.executable).output().unwrap();
        assert_eq!(output.status.code(), Some(status), "{output:?}");
        assert!(output.stdout.is_empty(), "{output:?}");
        assert_eq!(String::from_utf8_lossy(&output.stderr), stderr);
    }
}

/// Bug: the store and prepare words were read from any executable's first
/// program arguments, so `./tool --hirsel-dry-run /abs/B x.txt` rebound every
/// `Dir` to B, dropped two arguments and sent stdout to stderr, and a program
/// with no `Dir` refused to start. `docs/spec/launch.md`: the first argument
/// that is not a private option ends the prefix and everything after it is
/// `Args`; the store and prepare words are a place's (Laws A, B, C of 1.2).
#[test]
#[ignore = "explicit native integration target"]
fn place_words_are_plain_arguments_to_a_program() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (dir.path().join("A"), dir.path().join("B"));
    std::fs::create_dir(&a).unwrap();
    std::fs::create_dir(&b).unwrap();
    std::fs::write(a.join("x.txt"), "a").unwrap();
    let mut options = native_options(Backend::Cranelift, dir.path());
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let echo = r#"
        for i in 0u64 .. args.len(a) {
            match args.get(a, i) {
                Some(v) => {
                    result.unwrap(stdout.write(output, v))
                    result.unwrap(stdout.write(output, "\n"))
                },
                None => {},
            }
        }"#;
    let with_dir = format!(
        "pub fn main(files: Dir, a: Args, output: Stdout) -> Result[(), Exit] waits {{{echo}
        match host.open(files, \"x.txt\", Read) {{
            Ok(f) => {{
                result.unwrap(host.close(take f))
                result.unwrap(stdout.write(output, \"opened\\n\"))
            }},
            Error(_) => result.unwrap(stdout.write(output, \"absent\\n\")),
        }}
        Ok(())
    }}"
    );
    let without = format!(
        "pub fn main(a: Args, output: Stdout) -> Result[(), Exit] waits {{{echo}
        Ok(())
    }}"
    );
    let b = b.display().to_string();
    for (name, source, directories, grants, words) in [
        (
            "dir",
            &with_dir,
            vec![a.display().to_string()],
            1 << 4,
            [
                vec!["--hirsel-dry-run", &b, "x.txt"],
                vec!["--hirsel-store-verify", &b, "x.txt"],
                vec!["--hirsel-store-repair", &b, "x.txt"],
                vec!["--hirsel-prepare", &b, "a", "b", "c"],
            ],
        ),
        (
            "none",
            &without,
            vec![],
            1 << 4,
            [
                vec!["--hirsel-dry-run", "x"],
                vec!["--hirsel-store-verify", "x"],
                vec!["--hirsel-store-repair", "x"],
                vec!["--hirsel-prepare", "x"],
            ],
        ),
    ] {
        let mut request = source_request(source, dir.path());
        request.inputs.packages[1] =
            PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
        let product = Driver::default()
            .check(request, &mut Meter::new(false))
            .unwrap();
        assert!(product.accepted());
        options.directories = directories;
        options.grant_mask = grants;
        options.output = dir.path().join(name);
        let built = product.build(&entry, &options).unwrap();
        for words in words {
            let output = Command::new(&built.executable)
                .args(&words)
                .output()
                .unwrap();
            let mut expected = words.join("\n") + "\n";
            if name == "dir" {
                expected.push_str("opened\n");
            }
            assert_eq!(output.status.code(), Some(0), "{words:?}: {output:?}");
            assert_eq!(
                String::from_utf8_lossy(&output.stdout),
                expected,
                "{words:?}"
            );
            assert!(output.stderr.is_empty(), "{words:?}: {output:?}");
        }
    }
    assert_eq!(std::fs::read_dir(&b).unwrap().count(), 0);
}

#[test]
#[ignore = "explicit native integration target"]
fn dry_run_refuses_a_plain_program_without_running_it() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store");
    std::fs::create_dir(&store).unwrap();
    std::fs::write(store.join("LOCK"), []).unwrap();
    let source = r#"pub fn main(output: Stderr) -> Result[(), Exit] waits {
        result.unwrap(stderr.write(output, "plain program entered\n"))
        Ok(())
    }"#;
    let mut request = source_request(source, dir.path());
    request.inputs.packages[1] =
        PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
    let product = Driver::default()
        .check(request, &mut Meter::new(false))
        .unwrap();
    assert!(product.accepted());
    let mut options = native_options(Backend::Cranelift, dir.path());
    options.grant_mask = 1 << grant_kind("Stderr").unwrap();
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let built = product.build(&entry, &options).unwrap();
    let direct = Command::new(&built.executable)
        .arg("--hirsel-dry-run")
        .arg(&store)
        .output()
        .unwrap();
    assert!(direct.status.success(), "{direct:?}");
    assert_eq!(direct.stderr, b"plain program entered\n");

    let (status, stdout, stderr) = process::tape_request(
        &[
            "dry-run".into(),
            "--exe".into(),
            built.executable.display().to_string(),
            "--store".into(),
            store.display().to_string(),
        ],
        dir.path(),
    );
    assert_eq!(status, Some(104), "{stdout}{stderr}");
    assert!(stdout.is_empty(), "{stdout}");
    assert_eq!(
        stderr,
        format!("hirsel: {} is not a place\n", built.executable.display())
    );
}

/// Bug: the place word was set for any program reaching a package named
/// `runner@1`, so a user's own `runner` package made a plain program a place
/// and `hirsel dry-run` ran it. launch.md, "Private prefix": only the
/// toolchain's runner package reads the place words; the link decides by
/// where the package came from, the native link from the check's packages and
/// the recipe link from the recipe's toolchain table.
#[test]
#[ignore = "explicit native integration target"]
fn a_user_package_named_runner_is_not_a_place() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store");
    std::fs::create_dir(&store).unwrap();
    std::fs::write(store.join("LOCK"), []).unwrap();
    let runner = dir.path().join("runner.hirsel");
    std::fs::write(&runner, "pub fn note() -> String { \"plain program entered\\n\" }\n").unwrap();
    let source = r#"pub fn main(output: Stderr) -> Result[(), Exit] waits {
        result.unwrap(stderr.write(output, runner.note()))
        Ok(())
    }"#;
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let code = crate::code_store::CodeStore::new(dir.path().join("recipe-code"));
    // The package from a user and, as the control, from the toolchain.
    for toolchain in [false, true] {
        let mut request = source_request(source, dir.path());
        request.inputs.packages[1] =
            PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
        request.inputs.packages[0].dependencies.push(package("runner"));
        request.inputs.packages.push(PackageManifest {
            namespace: None,
            package: package("runner"),
            sources: vec![hirsel_core_resolve::ModuleSource {
                foreign: false,
                captured: None,
                file: runner.clone(),
                modules: vec!["runner".into()],
                signatures: false,
            }],
            dependencies: vec![package("core")],
            toolchain,
        });
        let product = Driver::default()
            .check(request, &mut Meter::new(false))
            .unwrap();
        assert!(product.accepted(), "{:?}", product.diagnostics);
        let mut options = native_options(Backend::Cranelift, dir.path());
        options.grant_mask = 1 << grant_kind("Stderr").unwrap();
        // The native link, from the check.
        options.output = dir.path().join(format!("native-{toolchain}"));
        let native = product.build(&entry, &options).unwrap().executable;
        // The recipe link, from the recipe read back from the code store.
        options.output = dir.path().join(format!("recipe-{toolchain}"));
        let reached = product.reachable(&entry, options.instance_limit).unwrap();
        let published = product
            .publish_release(&entry, &reached, &options, &code, hirsel_core_ids::launch::DEFAULT_N_PLACE)
            .unwrap();
        let recipe = crate::native::recipe_build::Publication::open(&code, published.root, &crate::request::local_admission_limits())
            .unwrap()
            .assemble_with(&options, reached.counters, None)
            .unwrap()
            .executable;
        for executable in [native, recipe] {
            let file = std::fs::File::open(&executable).unwrap();
            assert_eq!(
                crate::native_entry::launch_config::is_place(&file),
                toolchain,
                "{} toolchain {toolchain}",
                executable.display()
            );
            if toolchain {
                continue;
            }
            let (status, stdout, stderr) = process::tape_request(
                &[
                    "dry-run".into(),
                    "--exe".into(),
                    executable.display().to_string(),
                    "--store".into(),
                    store.display().to_string(),
                ],
                dir.path(),
            );
            assert_eq!(status, Some(104), "{stdout}{stderr}");
            assert_eq!(
                stderr,
                format!("hirsel: {} is not a place\n", executable.display())
            );
        }
    }
    // A workspace member named `runner@1` outside the invoker's library root
    // is a plain package, however the workspace binds it.
    let workspace = dir.path().join("workspace");
    for (member, manifest, file, text) in [
        (
            "app",
            "[package]\nname = \"app\"\nmajor = 1\ncharge = \"charge-v1\"\n\n[dependencies]\nrunner = { major = 1, path = \"../runner\" }\n\n[sources]\n\"main.hirsel\" = \"main\"\n",
            "main.hirsel",
            source,
        ),
        (
            "runner",
            "[package]\nname = \"runner\"\nmajor = 1\ncharge = \"charge-v1\"\n\n[sources]\n\"runner.hirsel\" = \"runner\"\n",
            "runner.hirsel",
            "pub fn note() -> String { \"plain program entered\\n\" }\n",
        ),
    ] {
        std::fs::create_dir_all(workspace.join(member)).unwrap();
        std::fs::write(workspace.join(member).join("hirsel.toml"), manifest).unwrap();
        std::fs::write(workspace.join(member).join(file), text).unwrap();
    }
    std::fs::write(
        workspace.join("hirsel.toml"),
        "[workspace]\nmembers = [\"app\", \"runner\"]\n",
    )
    .unwrap();
    let request =
        CheckRequest::directory(&workspace.join("app"), &root.join("core"), &dir.path().join("member-store"))
            .unwrap();
    assert!(request.inputs.packages.iter().all(|package| !package.toolchain));
    let product = Driver::default()
        .check(request, &mut Meter::new(false))
        .unwrap();
    assert!(product.accepted(), "{:?}", product.diagnostics);
    let mut options = native_options(Backend::Cranelift, dir.path());
    options.grant_mask = 1 << grant_kind("Stderr").unwrap();
    options.output = dir.path().join("member");
    let member = product.build(&entry, &options).unwrap().executable;
    assert!(!crate::native_entry::launch_config::is_place(
        &std::fs::File::open(&member).unwrap()
    ));
}

#[test]
fn exec_startup_binds_the_policy_and_keeps_the_sealed_image() {
    use crate::package_content::{archive_digest, hex};
    use hirsel_core_net::exec::{PinnedImage, Request};
    use std::io::Read;
    use std::os::fd::AsFd;

    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("image");
    let profile = dir.path().join("profiles.toml");
    let mut executable = [0u8; 192];
    executable[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    executable[16..18].copy_from_slice(&2u16.to_le_bytes()); // ET_EXEC
    let machine = match crate::host::HOST_TARGET {
        hirsel_core_layout::Target::X86_64Linux => 62u16,
        hirsel_core_layout::Target::Aarch64Linux | hirsel_core_layout::Target::Aarch64Darwin => 183,
    };
    executable[18..20].copy_from_slice(&machine.to_le_bytes());
    executable[20..24].copy_from_slice(&1u32.to_le_bytes());
    executable[24..32].copy_from_slice(&0x1000u64.to_le_bytes()); // entry
    executable[32..40].copy_from_slice(&64u64.to_le_bytes()); // phoff
    executable[52..54].copy_from_slice(&64u16.to_le_bytes()); // ehsize
    executable[54..56].copy_from_slice(&56u16.to_le_bytes()); // phentsize
    executable[56..58].copy_from_slice(&2u16.to_le_bytes()); // phnum
    executable[64..68].copy_from_slice(&1u32.to_le_bytes()); // PT_LOAD
    executable[68..72].copy_from_slice(&5u32.to_le_bytes()); // PF_R | PF_X
    executable[80..88].copy_from_slice(&0x1000u64.to_le_bytes());
    executable[96..104].copy_from_slice(&192u64.to_le_bytes());
    executable[104..112].copy_from_slice(&192u64.to_le_bytes());
    executable[120..124].copy_from_slice(&3u32.to_le_bytes()); // PT_INTERP
    executable[128..136].copy_from_slice(&176u64.to_le_bytes());
    let interpreter = c"/oracle/loader";
    let loader = interpreter.to_bytes_with_nul();
    executable[152..160].copy_from_slice(&(loader.len() as u64).to_le_bytes());
    executable[176..176 + loader.len()].copy_from_slice(loader);
    std::fs::write(&image, executable).unwrap();
    let pin = hex(&archive_digest(&executable));
    let policy = format!(
        "[git]\nimage = {:?}\nsha256 = \"{pin}\"\nargv0 = \"git\"\nargs = [[\"status\"]]\nenv = {{ LANG = \"C.UTF-8\" }}\ndirs = {{ work = {:?} }}\n",
        image.to_str().unwrap(),
        dir.path().to_str().unwrap(),
    );
    std::fs::write(&profile, &policy).unwrap();
    let request = Request::parse(&format!("{} git", profile.display())).unwrap();
    let loaded = request.load().unwrap();
    let expected = loaded.digest;
    let (_, lines) = loaded.pin_images().unwrap();
    assert_eq!(
        lines,
        [format!(
            "exec: git: image {}, sha256 {pin}, interpreter /oracle/loader, policy {}\n",
            image.display(),
            hirsel_core_net::exec::hex(&expected),
        )]
    );
    std::fs::write(&profile, format!("# comment\n{policy}")).unwrap();
    let loaded = request.load().unwrap();
    assert_eq!(loaded.digest, expected);

    let app = "pub fn main(approved: Exec, out: Stdout) -> Result[(), Exit] waits {
        let _ = exec.narrow(approved, Form(program: \"git\", form: list.new()))
        result.unwrap(stdout.write(out, \"entered\\n\"))
        Ok(())
    }";
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut check = source_request(app, dir.path());
    check.inputs.packages[1] =
        PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
    let product = Driver::default()
        .check(check, &mut Meter::new(false))
        .unwrap();
    assert!(
        product.accepted(),
        "{}",
        serde_json::to_string(&(
            product.machine_diagnostics().unwrap(),
            product.machine_incomplete().unwrap(),
        ))
        .unwrap(),
    );
    let mut options = native_options(Backend::Cranelift, dir.path());
    options.output = dir.path().join("exec-launch");
    options.grant_mask = 1 << 4;
    options.exec_grants.push(crate::exec_launch::Binding {
        request: request.clone(),
        policy: Some(expected),
        workdir: None,
    });
    let main = DefPath::from_full("app@1/main/main").unwrap();
    let built = product.build(&main, &options).unwrap();
    std::fs::write(&profile, policy.replace("\"status\"", "\"diff\"")).unwrap();
    assert_ne!(request.load().unwrap().digest, expected);
    let mut command = Command::new(built.executable);
    options.exec_grants[0].approve(&mut command);
    assert_not_started(
        &command.output().unwrap(),
        "not started: Exec[git]: launch policy digest differs from the build binding\n",
    );

    let spec = &loaded.profiles["git"];
    let pinned = PinnedImage::open(spec).unwrap();
    std::fs::write(&image, b"#!/bin/sh\n").unwrap();
    let mut file = std::fs::File::from(pinned.file.as_fd().try_clone_to_owned().unwrap());
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, executable);
    assert_eq!(pinned.interpreter.as_deref(), Some(interpreter));
    assert!(
        matches!(PinnedImage::open(spec), Err(reason) if reason.contains("does not match the pin"))
    );
    std::fs::write(
        &profile,
        policy.replace(&pin, &hex(&archive_digest(b"#!/bin/sh\n"))),
    )
    .unwrap();
    let script = request.load().unwrap();
    assert!(
        matches!(PinnedImage::open(&script.profiles["git"]), Err(reason) if reason.contains("without a #! line"))
    );
}
