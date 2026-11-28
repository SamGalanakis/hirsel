/// S6's JSON/Args/Tasks run replays after its input grant is removed; diagnose
/// names the first divergent request's rebound source, and show is standalone.
#[test]
#[ignore = "explicit native integration target"]
fn recording_cli_replays() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let app = dir.path().join("app");
    let data = dir.path().join("data");
    std::fs::create_dir(&app).unwrap();
    std::fs::create_dir(&data).unwrap();
    let source = include_str!("../../../core/front/fixtures/recording/cli.hirsel");
    std::fs::write(app.join("main.hirsel"), source).unwrap();
    std::fs::write(data.join("input.json"), b"{\"answer\":42}\n").unwrap();
    let tape = dir.path().join("run.tape");
    let runtime = env_record_runtime();
    let llvm = PathBuf::from(std::env::var_os("LLVM_SYS_231_PREFIX").unwrap());
    let common = vec![
        "--runtime".to_owned(),
        runtime.display().to_string(),
        "--cc".into(),
        llvm.join("bin/clang").display().to_string(),
        "--libraries".into(),
        root.join("core").display().to_string(),
        "--source-core".into(),
        "--source-std".into(),
        "--store".into(),
        dir.path().join("cache").display().to_string(),
    ];
    let invoke = |command: &str, extra: Vec<String>| {
        let mut args = command.split_ascii_whitespace().map(str::to_owned).collect::<Vec<_>>();
        args.push(app.display().to_string());
        args.extend(common.clone());
        args.extend(extra);
        process::tape_request(&args, dir.path())
    };
    let record = vec![
        "--dir".into(),
        data.display().to_string(),
        "--grant".into(),
        "Clock".into(),
        "--grant".into(),
        "Tasks".into(),
        "--grant".into(),
        "Stdout".into(),
        "--record".into(),
        tape.display().to_string(),
        "--".into(),
        "recorded".into(),
    ];
    let (code, out, err) = invoke("run", record.clone());
    assert_eq!(
        (code, out),
        (Some(0), "{\"answer\":42}\n".to_owned()),
        "record: {err}"
    );
    let bytes = std::fs::read(&tape).unwrap();
    let (code, _, _) = invoke("run", record);
    assert_ne!(code, Some(0), "record never overwrites an existing tape");
    assert_eq!(std::fs::read(&tape).unwrap(), bytes);
    std::fs::remove_dir_all(&data).unwrap();
    let replay = vec!["--tape".into(), tape.display().to_string()];
    let (code, out, err) = invoke("tape replay", replay.clone());
    assert_eq!(
        (code, out),
        (Some(0), String::new()),
        "replay with no grants: {err}"
    );
    assert_eq!(std::fs::read(&tape).unwrap(), bytes);
    profile_fixture_replay(&common, &app, &tape);

    let export_file = dir.path().join("export.txt");
    let shown = vec![
        "tape".to_owned(), "export".into(), tape.display().to_string(),
        "--include-credentials".into(), "--out".into(), export_file.display().to_string(),
    ];
    let (code, _, err) = process::tape_request(&shown, dir.path());
    assert_eq!(code, Some(0), "export: {err}");
    let out = std::fs::read_to_string(export_file).unwrap();
    assert!(
        out.contains(&format!("format {}", hirsel_tape_format::FORMAT))
            && out.contains("GRANT")
            && out.contains("END")
    );
    assert!(!out.contains("site"));
    // Random already exists in the provider ABI. Its payload must also replay,
    // including DATA continuation, rather than leave the carrier empty.
    let random = "pub fn main(r: Random, out: Stdout) -> Result[(), Exit] { let mut b: Bytes = list.with_capacity(70000); host.random(r, mut b); if list.len(b) != 70000u64 { return Error(Exit(status: 6)) }; result.unwrap(stdout.write_bytes(out, b)); Ok(()) }";
    std::fs::write(app.join("main.hirsel"), random).unwrap();
    let random_tape = dir.path().join("random.tape");
    let (code, _, err) = invoke(
        "run",
        vec![
            "--grant".into(),
            "Random".into(),
            "--grant".into(),
            "Stdout".into(),
            "--record".into(),
            random_tape.display().to_string(),
        ],
    );
    assert_eq!(code, Some(0), "random record: {err}");
    let (code, out, err) = invoke(
        "tape replay",
        vec!["--tape".into(), random_tape.display().to_string()],
    );
    assert_eq!(
        (code, out),
        (Some(0), String::new()),
        "random replay: {err}"
    );
    profile_fixture_replay(&common, &app, &random_tape);
    std::fs::write(
        app.join("main.hirsel"),
        source.replace("\"input.json\"", "\"other.json\""),
    )
    .unwrap();
    let (code, _, err) = invoke("tape replay", replay.clone());
    assert_eq!(code, Some(2), "{err}");
    assert!(err.contains("replay: refused: revision"), "{err}");
    let mut across = replay;
    across.push("--diagnose".into());
    let (code, _, err) = invoke("tape replay", across);
    assert_eq!(code, Some(1), "{err}");
    assert!(
        err.contains("replay: revision differs")
            && err.contains("replay: Diverged(Arg) at ")
            && err.contains("file.hirsel:"),
        "{err}"
    );
    assert_eq!(std::fs::read(&tape).unwrap(), bytes);
}

/// Run a recording executable through an approved Exec profile, inspect its
/// own tape, and replay its committed bytes after its image is gone.
#[test]
fn exec_child_records_its_own_replayable_output() {
    use crate::package_content::archive_digest;
    use std::os::unix::fs::PermissionsExt;
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let work = scratch.path();
    let child = work.join("child");
    let parent = work.join("parent");
    let image = work.join("child-bin");
    let tape = work.join("child.tape");
    let export = work.join("export");
    for dir in [&parent, &child] {
        std::fs::create_dir(dir).unwrap();
    }
    let source = r#"pub fn main(out: Stdout) -> Result[(), Exit] {
        result.unwrap(stdout.write(out, "the recording holds only a digest of this line"))
        Ok(())
    }"#;
    let expected = b"the recording holds only a digest of this line";
    std::fs::write(child.join("main.hirsel"), source).unwrap();
    std::fs::write(parent.join("main.hirsel"), r#"
pub fn main(e: Exec) -> Result[(), Exit] waits {
    let launch = Launch(program: "child", args: list.new(), env: list.new(), dir: "work", path: "", stdin: Discard, stdout: Discard, stderr: Discard)
    let Spawned(proc, _, _, _) = result.unwrap(exec.spawn(e, launch))
    let mut child = take proc
    match result.unwrap(exec.wait(mut child, Never)) {
        Exited(0) => Ok(()),
        _ => Error(Exit(status: 1)),
    }
}"#).unwrap();

    let invoke = |command: &str, app: &Path, flags: &[&str]| {
        let llvm = PathBuf::from(std::env::var_os("LLVM_SYS_231_PREFIX").unwrap());
        let mut args = command.split_ascii_whitespace().map(str::to_owned).collect::<Vec<_>>();
        args.extend([
            app.display().to_string(),
            "--cc".into(),
            llvm.join("bin/clang").display().to_string(),
            "--libraries".into(),
            root.join("core").display().to_string(),
            "--source-core".into(),
            "--store".into(),
            work.join("cache").display().to_string(),
        ]);
        args.extend(flags.iter().map(|s| (*s).to_owned()));
        process::tape_request(&args, work)
    };
    let runtime = env_record_runtime();
    let (code, out, err) = invoke(
        "build",
        &child,
        &[
            "--runtime",
            runtime.to_str().unwrap(),
            "--recording",
            "--grant",
            "Stdout",
            "-o",
            image.to_str().unwrap(),
        ],
    );
    assert_eq!(code, Some(0), "{out}{err}");
    let pin = hirsel_core_ids::hex::encode(&archive_digest(&std::fs::read(&image).unwrap()));
    let policy = work.join("exec.hcfg");
    std::fs::write(&policy, format!("exec_v1.Policy(profiles: [(\"child\", Profile(image: {image:?}, sha256: \"{pin}\", argv0: \"child\", args: [[]], env: [(\"HIRSEL_RECORD\", Fixed({tape:?}))], dirs: [(\"work\", {work:?})]))])")).unwrap();
    let plain = native_options(Backend::Cranelift, work)
        .runtime
        .canonicalize()
        .unwrap();
    let (code, out, err) = invoke(
        "run",
        &parent,
        &[
            "--runtime",
            plain.to_str().unwrap(),
            "--grant",
            &format!("Exec={} child", policy.display()),
        ],
    );
    assert_eq!(code, Some(0), "{out}{err}");
    let pinned = work.join("pinned-output");
    let status = crate::tape_cli::outputs::execute(
        Command::new(&image)
            .args(["--hirsel-replay-recording", "same"])
            .arg(&tape),
        Some(&pinned),
    )
    .unwrap();
    assert!(status.success());
    assert_eq!(std::fs::read(pinned.join("stdout")).unwrap(), expected);
    std::fs::remove_file(image).unwrap();
    let tape_bytes = std::fs::read(&tape).unwrap();
    assert!(!tape_bytes.windows(expected.len()).any(|w| w == expected));
    let export_file = work.join("raw-export.txt");
    let mut arguments = vec![
        "tape".into(), "export".into(), tape.display().to_string(),
        "--out".into(), export_file.display().to_string(),
    ];
    let (code, out, err) = process::tape_request(&arguments, work);
    assert_eq!(code, Some(2));
    assert!(out.is_empty() && err.contains("--include-credentials"));
    assert!(!export_file.exists());
    arguments.push("--include-credentials".into());
    let (code, _, err) = process::tape_request(&arguments, work);
    assert_eq!(code, Some(0), "{err}");
    let records = std::fs::read_to_string(&export_file).unwrap();
    assert!(records.starts_with("format "));
    assert!(records.contains("root"));
    assert!(records.contains(&format!("op={}", hirsel_tape_format::op::STDOUT)));
    assert!(records.contains("END"));
    assert_eq!(std::fs::metadata(&export_file).unwrap().permissions().mode() & 0o777, 0o600);
    assert_ne!(process::tape_request(&arguments, work).0, Some(0));
    assert_eq!(std::fs::read_to_string(&export_file).unwrap(), records);
    let replay = |extra: &[&str], path: &Path| {
        let mut flags = vec![
            "--runtime",
            runtime.to_str().unwrap(),
            "--tape",
            tape.to_str().unwrap(),
            "--outputs",
            path.to_str().unwrap(),
        ];
        flags.extend_from_slice(extra);
        invoke("tape replay", &child, &flags)
    };
    let (code, out, err) = replay(&[], &export);
    assert_eq!(code, Some(0), "{out}{err}");
    assert_eq!(std::fs::read(export.join("stdout")).unwrap(), expected);
    assert_eq!(
        std::fs::metadata(&export).unwrap().permissions().mode() & 0o077,
        0
    );
    assert_ne!(replay(&[], &export).0, Some(0));
    assert_eq!(std::fs::read(export.join("stdout")).unwrap(), expected);
    std::fs::write(
        child.join("main.hirsel"),
        source.replace("this line", "that line"),
    )
    .unwrap();
    let divergent = work.join("divergent");
    let (code, _, err) = replay(&["--diagnose"], &divergent);
    assert_eq!(code, Some(1), "{err}");
    assert!(
        err.contains("Diverged(Arg)") && !divergent.exists(),
        "{err}"
    );
    assert_eq!(std::fs::read(tape).unwrap(), tape_bytes);
}
