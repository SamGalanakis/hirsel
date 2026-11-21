// REPLAY-SPEC v2.3 R10: every executable carries its entry's code revision as
// the 32-byte symbol `hirsel_code_revision`, which a Run tape's header records
// and a Replay open compares (the runtime's
// open_refuses_a_changed_code_revision_arg_limit_or_policy).

/// The recording runtime archive path from the Buck2 test environment.
fn env_record_runtime() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(std::env::var_os("HIRSEL_CORE_RT_RECORD_ARCHIVE").expect("recording runtime archive"))
        .canonicalize()
        .expect("recording runtime archive")
}

/// The symbol's bytes, read from the executable's own symbol table.
fn code_revision_of(executable: &Path) -> [u8; 32] {
    use object::{Object, ObjectSection, ObjectSymbol};
    let bytes = std::fs::read(executable).unwrap();
    let file = object::File::parse(&*bytes).unwrap();
    let symbol = file
        .symbols()
        .find(|s| s.name() == Ok("hirsel_code_revision"))
        .expect("the executable defines hirsel_code_revision");
    let section = file
        .section_by_index(symbol.section_index().unwrap())
        .unwrap();
    let at = (symbol.address() - section.address()) as usize;
    section.data().unwrap()[at..at + 32].try_into().unwrap()
}

/// RL1 exercises the CLI and the resulting ELF on both engines and profiles.
#[test]
fn recording_switch_preserves_native_observations() {
    use object::{Object, ObjectSymbol};

    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let app = dir.path().join("app");
    std::fs::create_dir(&app).unwrap();
    std::fs::write(
        app.join("main.hirsel"),
        r#"
fn spent_steps() -> u64 {
    match (bound steps 8 { loop {} }) {
        Error(e) => e.steps,
        _ => 0
    }
}

pub fn main(a: Args, out: Stdout) -> Result[(), Exit] waits {
    let capacity = bound bytes 15 {
        let literal: Result[FrozenBytes, Capacity] = b"abcdefghi"
        let value = literal?
        list.len(bytebuf.as_bytes(value))
    }
    match capacity {
        Error(c) => if c.requested != 16 || c.available != 15 { return Error(Exit(status: 2)) },
        Ok(_) => return Error(Exit(status: 3)),
    }
    if spent_steps() != 8 { return Error(Exit(status: 4)) }
    result.unwrap(stdout.write(out, "first\n"))
    let n = 1u64 / args.len(a)
    if n != 1 { return Error(Exit(status: 5)) }
    result.unwrap(stdout.write(out, "second\n"))
    Ok(())
}
"#,
    )
    .unwrap();
    let llvm = PathBuf::from(std::env::var_os("LLVM_SYS_231_PREFIX").unwrap());
    let plain = native_options(Backend::Cranelift, dir.path())
        .runtime
        .canonicalize()
        .unwrap();
    let recording = env_record_runtime();
    let store = dir.path().join("store");
    let code_store = dir.path().join("code");
    let libraries = root.join("core");
    let clang = llvm.join("bin/clang");
    let inputs = [
        app.to_str().unwrap(),
        "--source-core",
        "--libraries",
        libraries.to_str().unwrap(),
    ];
    let source = [inputs.as_slice(), &["--grant", "Stdout"]];
    let link = ["--cc", clang.to_str().unwrap()];
    let invoke = |args: &[&str]| {
        let mut call = args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
        call.extend(["--store".into(), store.display().to_string()]);
        process::tape_request(&call, dir.path())
    };
    let mut expected = [None, None];
    for engine in ["cranelift", "llvm"] {
        for release in [false, true] {
            let configuration = if release { "release" } else { "default" };
            let profile: &[_] = if release { &["--release"] } else { &[] };
            let tag = format!("{engine}-{configuration}");
            let mut publish = vec!["publish"];
            publish.extend(source.concat());
            publish.extend([
                "--backend",
                engine,
                "--code-store",
                code_store.to_str().unwrap(),
            ]);
            let (status, recipe, err) = invoke(&publish);
            assert_eq!(status, Some(0), "{tag} publish: {err}");

            let mut observe = |recording_on: bool, use_recipe: bool| {
                let program = dir.path().join("program");
                let mut build = vec!["build"];
                if use_recipe {
                    build.extend([
                        "--recipe",
                        recipe.trim(),
                        "--code-store",
                        code_store.to_str().unwrap(),
                    ]);
                } else {
                    build.extend(source.concat());
                }
                let runtime = if recording_on { &recording } else { &plain };
                build.extend(link);
                build.extend([
                    "--backend",
                    engine,
                    "--runtime",
                    runtime.to_str().unwrap(),
                    "-o",
                    program.to_str().unwrap(),
                ]);
                build.extend(profile);
                if recording_on {
                    build.push("--recording");
                }
                let (status, _, err) = invoke(&build);
                assert_eq!(status, Some(0), "{tag} build: {err}");

                let bytes = std::fs::read(&program).unwrap();
                let elf = object::File::parse(bytes.as_slice()).unwrap();
                let defines_recording_entry = elf.symbols().any(|symbol| {
                    symbol.is_definition() && symbol.name() == Ok("hirsel_core_os_run_recording")
                });
                assert_eq!(
                    defines_recording_entry, recording_on,
                    "RL1 recording entry: {tag}, recipe={use_recipe}"
                );
                if !recording_on {
                    let leaked: Vec<_> = elf
                        .symbols()
                        .filter(|symbol| symbol.is_definition())
                        .filter_map(|symbol| symbol.name().ok())
                        .filter(|name| {
                            *name == "hirsel_core_os_replay"
                                || name.contains("14hirsel_core_rt4tape")
                                || name.contains("18hirsel_tape_format")
                        })
                        .collect();
                    assert!(
                        leaked.is_empty(),
                        "RL1 no recording runtime symbols: {tag}: {leaked:?}"
                    );
                }

                for (index, args) in [&["ok"][..], &[][..]].into_iter().enumerate() {
                    let tape = dir.path().join(format!("{tag}-{use_recipe}-{index}.tape"));
                    let run = |record: bool| {
                        let mut command = Command::new(&program);
                        command.args(args).env_remove("HIRSEL_RECORD");
                        if record {
                            command.env("HIRSEL_RECORD", &tape);
                        }
                        let out = command.output().unwrap();
                        (out.status.code(), out.stdout, out.stderr)
                    };
                    let off = run(false);
                    if index == 0 {
                        assert_eq!(off, (Some(0), b"first\nsecond\n".to_vec(), vec![]), "{tag}");
                    } else {
                        assert_eq!(
                            (off.0, off.1.as_slice()),
                            (Some(101), b"first\n".as_slice()),
                            "{tag}"
                        );
                        assert!(
                            String::from_utf8_lossy(&off.2).starts_with("Division at "),
                            "{tag}: {:?}",
                            off.2
                        );
                    }
                    assert_eq!(
                        &off,
                        expected[index].get_or_insert_with(|| off.clone()),
                        "RL1 observations: {tag}"
                    );
                    if recording_on {
                        assert_eq!(run(true), off, "RL1 recording on: {tag}");
                        assert!(tape.is_file(), "RL1 recording on must write a tape: {tag}");
                        let mut replay = vec!["replay"];
                        replay.extend(inputs);
                        replay.extend(link);
                        let other = if engine == "llvm" {
                            "cranelift"
                        } else {
                            "llvm"
                        };
                        let replay_output = dir.path().join("replay");
                        replay.extend([
                            "--backend",
                            other,
                            "--runtime",
                            recording.to_str().unwrap(),
                            "--tape",
                            tape.to_str().unwrap(),
                            "-o",
                            replay_output.to_str().unwrap(),
                        ]);
                        if !release {
                            replay.push("--release");
                        }
                        let (status, stdout, stderr) = invoke(&replay);
                        assert_eq!(status, Some(0), "{tag} replay: {stderr}");
                        assert!(
                            stdout.is_empty(),
                            "RL1 replay must not repeat live writes: {tag}"
                        );
                    }
                }
            };
            observe(false, false);
            observe(true, false);
            observe(true, true);
        }
    }
}

/// The oracle is the revision the build reports (revision.rs, from the checked
/// IR, not from the link), which a Run header must also agree with. Wrong
/// implementations caught: a symbol that is the executable's key, the sources
/// digest (a comment edit would refuse) or any other hash than the revision.
#[test]
#[ignore = "explicit native integration target"]
fn tape_code_revision_is_the_entry_revision() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let source = "pub fn main(output: Stdout) -> Result[(), Exit] waits {\n    result.unwrap(stdout.write(output, \"entered\"))\n    Ok(())\n}";
    for backend in [Backend::Cranelift, Backend::Llvm] {
        let mut request = source_request(source, dir.path());
        request.inputs.packages[1] =
            PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
        let product = Driver::default()
            .check(request, &mut Meter::new(false))
            .unwrap();
        assert!(product.accepted());
        let mut options = native_options(backend, dir.path());
        options.output = dir.path().join(format!("{backend:?}"));
        options.grant_mask = 1 << grant_kind("Stdout").unwrap();
        options.runtime = env_record_runtime();
        options.recording = true;
        let built = product.build(&entry, &options).unwrap();
        assert_eq!(
            code_revision_of(&built.executable),
            built.revision.0,
            "{backend:?}"
        );
    }
}

/// Applies a source edit and builds the recording or replay fixture.
fn tape_run_build(
    backend: Backend,
    dir: &Path,
    store: &str,
    edit: (&str, &str),
    replay: Option<crate::ReplayPlan>,
) -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let source =
        include_str!("../../../core/front/fixtures/recording/run.hirsel").replace(edit.0, edit.1);
    let mut request = source_request(&source, dir);
    request.inputs.packages[1] =
        PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
    let product = Driver::default()
        .check(request, &mut Meter::new(false))
        .unwrap();
    assert!(
        product.accepted(),
        "{:?} {:?}",
        product.diagnostics,
        product.incomplete
    );
    let mut options = native_options(backend, dir);
    options.runtime = PathBuf::from(env_record_runtime());
    options.store = dir.join(store);
    options.output = dir.join(format!("{store}-program"));
    options.grant_mask = if replay.is_some() {
        0
    } else {
        (1 << 2) | (1 << 4)
    };
    options.recording = true;
    options.replay = replay;
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    product.build(&entry, &options).unwrap().executable
}

fn tape_run(program: &Path, argv: &[&str], record: Option<&Path>) -> (Option<i32>, String, String) {
    let mut cmd = std::process::Command::new(program);
    cmd.args(argv);
    if let Some(path) = record {
        cmd.env("HIRSEL_RECORD", path);
    } else {
        cmd.env_remove("HIRSEL_RECORD");
    }
    let output = cmd.output().unwrap();
    (
        output.status.code(),
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

/// External recording and replay end to end: recording runs with HIRSEL_RECORD
/// set, replays with no grants on either engine, refuses a changed revision
/// with exit 2 unless `--diagnose` is set, and checks both a changed write argument
/// and a changed final outcome under diagnose.
fn tape_run_replays(backend: Backend, other: Backend) {
    let dir = tempfile::tempdir().unwrap();
    let tape = dir.path().join("run.tape");
    let program = tape_run_build(backend, dir.path(), "a", ("", ""), None);
    let (code, recorded, err) = tape_run(&program, &["one"], Some(&tape));
    assert_eq!(code, Some(0), "record: {err}");
    let reading = recorded
        .strip_prefix("taped")
        .and_then(|s| s.strip_suffix("|one"))
        .unwrap_or_else(|| panic!("record printed {recorded:?}"));
    assert!(reading.parse::<u64>().is_ok(), "{recorded:?}");
    let tape_bytes = std::fs::read(&tape).unwrap();
    let replay_plan = crate::ReplayPlan {
        path: tape.to_str().unwrap().into(),
        diagnose: false,
    };
    let replay_bin = tape_run_build(
        backend,
        dir.path(),
        "r",
        ("", ""),
        Some(replay_plan.clone()),
    );
    let (code, replayed, err) = tape_run(&replay_bin, &["two"], None);
    assert_eq!(code, Some(0), "replay: {err}");
    assert!(replayed.is_empty(), "replay must not write to live Stdout");
    assert_eq!(std::fs::read(&tape).unwrap(), tape_bytes);

    let switched = tape_run_build(other, dir.path(), "e", ("", ""), Some(replay_plan.clone()));
    let (code, again, err) = tape_run(&switched, &["three"], None);
    assert_eq!(
        (code, again),
        (Some(0), String::new()),
        "the other engine: {err}"
    );

    let changed = tape_run_build(
        backend,
        dir.path(),
        "c",
        ("\"|\"", "\"/\""),
        Some(replay_plan),
    );
    let (code, printed, err) = tape_run(&changed, &["one"], None);
    assert_eq!((code, printed), (Some(2), String::new()), "refused: {err}");
    assert!(err.contains("replay: refused: revision"), "{err}");

    let diagnosed = tape_run_build(
        backend,
        dir.path(),
        "d",
        ("\"|\"", "\"/\""),
        Some(crate::ReplayPlan {
            path: tape.to_str().unwrap().into(),
            diagnose: true,
        }),
    );
    let (code, printed, err) = tape_run(&diagnosed, &["one"], None);
    assert_eq!((code, printed), (Some(1), String::new()), "diagnose: {err}");
    assert!(
        err.contains("replay: revision differs") && err.contains("replay: Diverged(Arg) at "),
        "{err}"
    );

    let diagnose = Some(crate::ReplayPlan {
        path: tape.to_str().unwrap().into(),
        diagnose: true,
    });
    let same = tape_run_build(backend, dir.path(), "same", ("", ""), diagnose.clone());
    let (code, out, err) = tape_run(&same, &[], None);
    assert_eq!((code, out), (Some(0), String::new()), "{err}");
    assert!(!err.contains("revision differs"), "{err}");

    let changed_outcome = tape_run_build(
        backend,
        dir.path(),
        "outcome",
        ("else { Ok(()) }", "else { Error(Exit(status: 6)) }"),
        diagnose,
    );
    let (code, out, err) = tape_run(&changed_outcome, &[], None);
    assert_eq!((code, out), (Some(1), String::new()), "{err}");
    assert!(
        err.contains("revision differs") && err.contains("Outcome"),
        "{err}"
    );
    assert_eq!(std::fs::read(&tape).unwrap(), tape_bytes);
}

#[test]
#[ignore = "explicit native integration target"]
fn tape_run_replays_cranelift() {
    tape_run_replays(Backend::Cranelift, Backend::Llvm);
}

#[test]
#[ignore = "explicit native integration target"]
fn tape_run_replays_llvm() {
    tape_run_replays(Backend::Llvm, Backend::Cranelift);
}

/// CAP-8. Replay verifies the same stdout writes with no inherited input sources.
#[test]
fn run_tape_replays_stdin_and_env_without_host_sources() {
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let tape = dir.path().join("stdio.tape");
    let input = dir.path().join("input");
    std::fs::write(&input, b"nine bytes").unwrap();
    let source = r#"
pub fn main(i: Stdin, e: Env, out: Stdout) -> Result[(), Exit] waits {
    let mut prefix: Bytes = list.with_capacity(3)
    if result.unwrap(stdin.read(i, mut prefix, Never)) != 3 {
        return Error(Exit(status: 3))
    }
    let value = match env.get(e, "HIRSEL_FIXTURE_VALUE") {
        Ok(Some(s)) => s,
        _ => return Error(Exit(status: 4)),
    }
    result.unwrap(stdout.write(out, value))
    result.unwrap(stdout.write_bytes(out, prefix))
    result.unwrap(stdout.write_bytes(out, result.unwrap(std.io.read_all(i))))
    Ok(())
}
"#;
    let mut request = source_request(source, dir.path());
    request.inputs.packages[1] =
        PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
    request.inputs.packages[0].dependencies.push(package("std"));
    request
        .inputs
        .packages
        .push(CheckRequest::source_std(&root.join("core/std")).unwrap());
    let product = Driver::default()
        .check(request, &mut Meter::new(false))
        .unwrap();
    assert!(
        product.accepted(),
        "{:?} {:?}",
        product.diagnostics,
        product.incomplete
    );
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let mut options = native_options(Backend::Cranelift, dir.path());
    options.runtime = env_record_runtime();
    options.recording = true;
    options.grant_mask = (1 << grant_kind("Stdin").unwrap())
        | (1 << grant_kind("Env").unwrap())
        | (1 << grant_kind("Stdout").unwrap());
    options.env_patterns = hirsel_core_net::EnvPatterns::parse("HIRSEL_FIXTURE_VALUE");
    let recorded = product.build(&entry, &options).unwrap();
    let output = Command::new(&recorded.executable)
        .env_clear()
        .env("HIRSEL_FIXTURE_VALUE", "snapshot:")
        .env("HIRSEL_RECORD", &tape)
        .stdin(Stdio::from(std::fs::File::open(input).unwrap()))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{:?}", output.stderr);
    assert_eq!(output.stdout, b"snapshot:nine bytes");

    options.grant_mask = 0;
    options.env_patterns = None;
    options.replay = Some(crate::ReplayPlan {
        path: tape.to_str().unwrap().into(),
        diagnose: false,
    });
    let replay = product.build(&entry, &options).unwrap();
    let mut command = Command::new(&replay.executable);
    command.env_clear();
    // SAFETY: this changes only fd 0 in the newly forked child.
    unsafe {
        command.pre_exec(|| {
            libc::close(libc::STDIN_FILENO);
            Ok(())
        });
    }
    let output = command.output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "replay must verify the recorded stdout arguments with stdin closed and an empty environment: {:?}",
        output.stderr
    );
    assert!(
        output.stdout.is_empty(),
        "replay cannot write to the host stdout"
    );
}

/// A fixed clock and chooser drive two child waits. The engines must agree on
/// task frames and channel results, and must replay each other's recording.
#[test]
#[ignore = "explicit native integration target"]
fn recording_schedule_replays_across_engines() {
    use hirsel_tape_format::{Blocks, CHOOSER_COMMIT, Decoder, END, Frame};

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let source = include_str!("../../../core/front/fixtures/recording/schedule.hirsel");
    let c = dir.path().join("provider.c");
    let object = dir.path().join("provider.o");
    std::fs::write(&c, include_str!("recording_schedule_provider.c")).unwrap();

    let mut options = native_options(Backend::Cranelift, dir.path());
    command(
        Command::new(options.link.driver())
            .arg("-c")
            .arg(c)
            .arg("-I")
            .arg(root.join("crates/hirsel-core-rt/include"))
            .arg("-o")
            .arg(&object),
    )
    .unwrap();
    options.link = Link::Driver(appending_driver(
        dir.path(),
        "clang-record-schedule",
        &format!("'{}' -Wl,--wrap=hirsel_core_run", object.display()),
    ));
    options.runtime = root.join(
        std::env::var_os("HIRSEL_CORE_RT_SCRIPTED_RECORD_ARCHIVE")
            .expect("scripted recording runtime archive"),
    );
    options.recording = true;
    options.grant_mask = (1 << grant_kind("Clock").unwrap())
        | (1 << grant_kind("Tasks").unwrap())
        | (1 << grant_kind("Stdout").unwrap());

    let mut request = source_request(source, dir.path());
    request.inputs.packages[1] =
        PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
    let product = Driver::default()
        .check(request, &mut Meter::new(false))
        .unwrap();
    assert!(
        product.accepted(),
        "{:?} {:?}",
        product.diagnostics,
        product.incomplete
    );
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let frames = |mut bytes: &[u8]| {
        let (mut blocks, header) =
            Blocks::open(|buffer| std::io::Read::read(&mut bytes, buffer)).unwrap();
        assert_eq!(
            header.chooser, CHOOSER_COMMIT,
            "scripted chooser was installed"
        );
        let mut decoder = Decoder::default();
        let mut frames = Vec::new();
        while let Some((flags, payload)) = blocks
            .next(|buffer| std::io::Read::read(&mut bytes, buffer))
            .unwrap()
        {
            frames.extend(decoder.block(payload, flags & END != 0).unwrap());
        }
        assert!(
            frames
                .iter()
                .any(|frame| matches!(frame, Frame::Choose { .. })),
            "recorded a task choice"
        );
        let sleepers = frames
            .iter()
            .filter_map(|frame| match frame {
                Frame::Issue {
                    task,
                    op: hirsel_tape_format::op::SLEEP_UNTIL,
                    ..
                } => Some(*task),
                _ => None,
            })
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(sleepers.len(), 2, "each child issues sleep");
        for task_id in sleepers {
            assert!(
                frames
                    .iter()
                    .any(|frame| matches!(frame, Frame::Wait { task, .. } if *task == task_id)),
                "WAIT for child {task_id}"
            );
            assert!(
                frames
                    .iter()
                    .any(|frame| matches!(frame, Frame::Wake { task, .. } if *task == task_id)),
                "WAKE for child {task_id}"
            );
        }
        frames
    };
    let mut recorded = Vec::new();
    for (backend, replay_backend) in [
        (Backend::Cranelift, Backend::Llvm),
        (Backend::Llvm, Backend::Cranelift),
    ] {
        let tape = dir.path().join(format!("{backend:?}.tape"));
        options.backend = backend;
        let built = product.build(&entry, &options).unwrap();
        let output = Command::new(&built.executable)
            .env("HIRSEL_RECORD", &tape)
            .env("HIRSEL_RECORD_SCHEDULE", "1")
            .env("HIRSEL_S8_WORKERS", "1")
            .output()
            .unwrap();
        assert!(output.status.success(), "{backend:?}: {output:?}");
        assert!(
            output.stdout == b"12" || output.stdout == b"21",
            "each child sends one value: {:?}",
            output.stdout
        );
        let sealed = std::fs::read(&tape).unwrap();
        recorded.push(frames(&sealed));

        let mut replay = options.clone();
        replay.backend = replay_backend;
        replay.grant_mask = 0;
        replay.replay = Some(crate::ReplayPlan {
            path: tape.to_str().unwrap().into(),
            diagnose: false,
        });
        let built = product.build(&entry, &replay).unwrap();
        run_clean(&built.executable, 0);
        assert_eq!(std::fs::read(&tape).unwrap(), sealed);
    }
    assert_eq!(
        recorded[0], recorded[1],
        "engine recordings after the launch header"
    );
}

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
        let mut args = vec![command.to_owned(), app.display().to_string()];
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
    let (code, out, err) = invoke("replay", replay.clone());
    assert_eq!(
        (code, out),
        (Some(0), String::new()),
        "replay with no grants: {err}"
    );
    assert_eq!(std::fs::read(&tape).unwrap(), bytes);
    let shown = vec![
        "tape".to_owned(),
        "show".into(),
        tape.display().to_string(),
        "--include-credentials".into(),
    ];
    let (code, out, err) = process::tape_request(&shown, dir.path());
    assert_eq!(code, Some(0), "show: {err}");
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
        "replay",
        vec!["--tape".into(), random_tape.display().to_string()],
    );
    assert_eq!(
        (code, out),
        (Some(0), String::new()),
        "random replay: {err}"
    );
    std::fs::write(
        app.join("main.hirsel"),
        source.replace("\"input.json\"", "\"other.json\""),
    )
    .unwrap();
    let (code, _, err) = invoke("replay", replay.clone());
    assert_eq!(code, Some(2), "{err}");
    assert!(err.contains("replay: refused: revision"), "{err}");
    let mut across = replay;
    across.push("--diagnose".into());
    let (code, _, err) = invoke("replay", across);
    assert_eq!(code, Some(1), "{err}");
    assert!(
        err.contains("replay: revision differs")
            && err.contains("replay: Diverged(Arg) at ")
            && err.contains("file.hirsel:"),
        "{err}"
    );
    assert_eq!(std::fs::read(&tape).unwrap(), bytes);
}

#[test]
fn run_tape_replays_process_stop() {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let tape = dir.path().join("stop.tape");
    let source = r#"
pub fn main(p: Process, out: Stdout) -> Result[(), Exit] waits {
    result.unwrap(stdout.write(out, "ready\n"))
    host.stopped(p)
    result.unwrap(stdout.write(out, "stopped\n"))
    Ok(())
}
"#;
    let mut request = source_request(source, dir.path());
    request.inputs.packages[1] =
        PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
    let product = Driver::default()
        .check(request, &mut Meter::new(false))
        .unwrap();
    assert!(product.accepted(), "{:?}", product.diagnostics);
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let mut options = native_options(Backend::Cranelift, dir.path());
    options.runtime = env_record_runtime();
    options.grant_mask =
        (1 << grant_kind("Process").unwrap()) | (1 << grant_kind("Stdout").unwrap());
    options.recording = true;
    let built = product.build(&entry, &options).unwrap();
    let mut child = Command::new(&built.executable)
        .env("HIRSEL_RECORD", &tape)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    output.read_line(&mut line).unwrap();
    assert_eq!(line, "ready\n");
    let pid = child.id().to_string();
    let name = Command::new("ps")
        .args(["-o", "comm=", "-p", &pid])
        .output()
        .unwrap();
    assert!(name.status.success());
    assert_eq!(
        String::from_utf8(name.stdout).unwrap().trim(),
        built.executable.file_name().unwrap().to_str().unwrap()
    );
    // SAFETY: this is the still-owned child, which wrote the ready marker.
    assert_eq!(
        unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGINT) },
        0
    );
    line.clear();
    output.read_line(&mut line).unwrap();
    assert_eq!(line, "stopped\n");
    let record = child.wait_with_output().unwrap();
    assert!(record.status.success(), "{:?}", record.stderr);
    let sealed = std::fs::read(&tape).unwrap();

    options.backend = Backend::Llvm;
    options.grant_mask = 0;
    options.replay = Some(crate::ReplayPlan {
        path: tape.to_str().unwrap().into(),
        diagnose: false,
    });
    let replay = product.build(&entry, &options).unwrap();
    assert_eq!(replay.revision, built.revision);
    let result = Command::new(&replay.executable).output().unwrap();
    assert_eq!(result.status.code(), Some(0), "{:?}", result.stderr);
    assert!(
        result.stdout.is_empty(),
        "replay wrote through the live Stdout"
    );
    assert_eq!(std::fs::read(tape).unwrap(), sealed);
}

fn http_reply_bytes(
    connection: &mut std::io::BufReader<std::net::TcpStream>,
    status: &str,
    body: &[u8],
) -> Vec<u8> {
    use std::io::{BufRead, Read};

    let mut reply = String::new();
    connection.read_line(&mut reply).unwrap();
    assert_eq!(reply, status);
    let mut length = None;
    let mut date = None;
    loop {
        let mut line = String::new();
        assert_ne!(connection.read_line(&mut line).unwrap(), 0, "{reply}");
        reply.push_str(&line);
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length: ") {
            length = Some(value.trim_end().parse::<usize>().unwrap());
        }
        if let Some(value) = line.strip_prefix("Date: ") {
            date = Some(value.trim_end().to_owned());
        }
    }
    assert_eq!(length, Some(body.len()), "{reply}");
    if status.starts_with("HTTP/1.1 200 ") {
        let date = date.expect("a successful response carries Date");
        assert_eq!(date.len(), 29, "{date}");
        assert!(date.ends_with(" GMT"), "{date}");
    }
    let mut got = vec![0; body.len()];
    connection.read_exact(&mut got).unwrap();
    assert_eq!(got, body);
    let mut bytes = reply.into_bytes();
    bytes.extend(got);
    bytes
}

/// Record a real loopback server, including a head timeout, then replay with
/// no grants at one worker. The client checks the HTTP responses independently.
/// The linker observer compares bytes after each successful send on both runs.
#[test]
#[ignore = "explicit native integration target"]
fn tape_http_replays_both_native_engines() {
    use std::io::{BufRead, BufReader, Write};
    use std::net::{Ipv4Addr, TcpStream};
    use std::process::Stdio;

    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let app = dir.path().join("app");
    std::fs::create_dir(&app).unwrap();
    let fixture = include_str!("../../../core/front/fixtures/recording/http.hirsel");
    let main = app.join("main.hirsel");
    let observer_src = dir.path().join("http-observe.c");
    let observer_obj = dir.path().join("http-observe.o");
    std::fs::write(&observer_src, include_str!("tape_http_observe.c")).unwrap();
    let native = native_options(Backend::Cranelift, dir.path());
    command(
        Command::new(native.link.driver())
            .arg("-c")
            .arg(observer_src)
            .arg("-I")
            .arg(root.join("crates/hirsel-core-rt/include"))
            .arg("-o")
            .arg(&observer_obj),
    )
    .unwrap();
    let linker = appending_driver(
        dir.path(),
        "clang-observe-http",
        &format!(
            "'{}' -Wl,--wrap=hirsel_core_host_send_pair",
            observer_obj.display().to_string().replace('\'', "'\\''")
        ),
    );

    for engine in ["cranelift", "llvm"] {
        std::fs::write(&main, fixture).unwrap();
        let tape = dir.path().join(format!("{engine}.tape"));
        let common = [
            app.display().to_string(),
            "--backend".to_owned(),
            engine.to_owned(),
            "--runtime".to_owned(),
            env_record_runtime().display().to_string(),
            "--cc".to_owned(),
            linker.display().to_string(),
            "--libraries".to_owned(),
            root.join("core").display().to_string(),
            "--source-core".to_owned(),
            "--source-std".to_owned(),
            "--store".to_owned(),
            dir.path().join("cache").display().to_string(),
        ];
        let invoke = |command: &str, args: &[&str]| {
            let mut call = vec![command.to_owned()];
            call.extend(common.iter().cloned());
            call.extend(args.iter().map(|&arg| arg.to_owned()));
            call
        };
        let record = invoke(
            "run",
            &[
                "--grant",
                "Net=Listen 127.0.0.0/8 0-65535;Listen 127.0.0.1/32 0-65535",
                "--grant",
                "Clock",
                "--grant",
                "Tasks",
                "--grant",
                "Stdout",
                "--record",
                tape.to_str().unwrap(),
            ],
        );
        let mut child = process::tape_request_command(&record, dir.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut output = BufReader::new(child.stdout.take().unwrap());
        let mut ports = String::new();
        loop {
            assert_ne!(output.read_line(&mut ports).unwrap(), 0, "{engine}");
            if ports.ends_with(process::CHILD_MARK) {
                break;
            }
            ports.clear();
        }
        ports.clear();
        if output.read_line(&mut ports).unwrap() == 0 {
            let failed = child.wait_with_output().unwrap();
            panic!(
                "{engine} recording exited {:?} before listening: {}",
                failed.status.code(),
                String::from_utf8_lossy(&failed.stderr)
            );
        }
        let [server, stop] = ports
            .split_whitespace()
            .map(|port| port.parse::<u16>().unwrap())
            .collect::<Vec<_>>()
            .try_into()
            .unwrap();
        let connect = |port| TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        let mut connection = BufReader::new(connect(server));
        let mut wire = Vec::new();
        for path in ["/first", "/second", "/third"] {
            write!(
                connection.get_mut(),
                "GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n"
            )
            .unwrap();
            wire.extend(http_reply_bytes(
                &mut connection,
                "HTTP/1.1 200 OK\r\n",
                path.as_bytes(),
            ));
        }
        drop(connection);

        let mut slow = BufReader::new(connect(server));
        slow.get_mut()
            .write_all(b"GET /incomplete HTTP/1.1\r\nHost: ")
            .unwrap();
        wire.extend(http_reply_bytes(
            &mut slow,
            "HTTP/1.1 408 Request Timeout\r\n",
            b"",
        ));
        drop(slow);
        drop(connect(stop));

        let recorded = child.wait_with_output().unwrap();
        let stderr = String::from_utf8(recorded.stderr).unwrap();
        let observed = stderr.split_once(process::CHILD_MARK).unwrap().1;
        assert_eq!(recorded.status.code(), Some(0), "{engine}: {observed}");
        assert_eq!(
            observed.as_bytes(),
            wire,
            "{engine} record compared to wire"
        );
        let sealed = std::fs::read(&tape).unwrap();

        let replay = invoke("replay", &["--tape", tape.to_str().unwrap()]);
        let (code, stdout, sent) = process::tape_request(&replay, dir.path());
        assert_eq!(code, Some(0), "{engine} replay: {sent}");
        assert!(stdout.is_empty(), "{engine}: {stdout}");
        assert_eq!(sent.as_bytes(), wire, "{engine} replay compared to wire");
        assert_eq!(std::fs::read(&tape).unwrap(), sealed);

        let changed = fixture.replace(
            "std.http.copy_bytes(std.http.path(req))",
            "std.http.copy_bytes(string.as_bytes(\"/firsX\"))",
        );
        assert_ne!(changed, fixture);
        std::fs::write(&main, changed).unwrap();
        let mut diagnose = replay;
        diagnose.push("--diagnose".into());
        let (code, _, error) = process::tape_request(&diagnose, dir.path());
        assert_eq!(code, Some(1), "{engine}: {error}");
        assert!(error.contains("replay: Diverged(Arg)"), "{engine}: {error}");
    }
}

/// RL3 checks the server's HTTP bytes, RFC 7301's selected index and the
/// OpenSSL digest of the fixture leaf. Replay runs after removing both live
/// inputs, with a counting provider in place of the CLI's refuse-all provider.
#[test]
fn rl3_replays_tls_offline() {
    cli_tls_replay(false);
}

/// RL4 and RL5. The permitted peer receives the exact
/// synthetic grant bytes. Plain TCP and the wrong verified name receive zero.
#[test]
fn rl4_rl5_replays_tls_without_secret_sources() {
    cli_tls_replay(true);
}

fn cli_tls_replay(with_secret: bool) {
    use rustls::{
        ServerConfig, ServerConnection, StreamOwned,
        pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
    };
    use std::{
        io::{Read, Write},
        net::{Ipv4Addr, TcpListener},
        os::unix::fs::PermissionsExt,
        sync::Arc,
    };

    const CA: &[u8] = include_bytes!("../../hirsel-core-rt/testdata/tls/ca.pem");
    const LEAF: &[u8] = include_bytes!("../../hirsel-core-rt/testdata/tls/alpha.pem");
    const KEY: &[u8] = include_bytes!("../../hirsel-core-rt/testdata/tls/alpha.key.pem");
    const BETA: &[u8] = include_bytes!("../../hirsel-core-rt/testdata/tls/beta.pem");
    const BETA_KEY: &[u8] = include_bytes!("../../hirsel-core-rt/testdata/tls/beta.key.pem");
    const PEER: &str = include_str!("../../hirsel-core-rt/testdata/tls/alpha.sha256");
    const REQUEST: &[u8] = b"GET / HTTP/1.1\r\nHost: alpha.test\r\n\r\n";
    const RESPONSE: &str = "HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello";

    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = |leaf, key, alpn| {
        let mut config =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(
                    vec![CertificateDer::from_pem_slice(leaf).unwrap()],
                    PrivateKeyDer::from_pem_slice(key).unwrap(),
                )
                .unwrap();
        if alpn {
            config.alpn_protocols = vec![b"http/1.1".to_vec()];
        }
        Arc::new(config)
    };
    let mut raw = [0; 32];
    let mut file_bytes = [0; 32];
    if with_secret {
        let mut random = std::fs::File::open("/dev/urandom").unwrap();
        random.read_exact(&mut raw).unwrap();
        random.read_exact(&mut file_bytes).unwrap();
    }
    let token = crate::call::hex(&raw);
    let expected_request = if with_secret {
        [
            b"before:",
            token.as_bytes(),
            b":after\n",
            &file_bytes,
            b":after\nsealed:after\n",
        ]
        .concat()
    } else {
        REQUEST.to_vec()
    };
    let response = if with_secret { "accepted\n" } else { RESPONSE };
    let peer = std::thread::spawn(move || {
        if with_secret {
            let (mut plain, _) = listener.accept().unwrap();
            plain.write_all(b"!").unwrap();
            let mut refused = Vec::new();
            plain.read_to_end(&mut refused).unwrap();
            assert!(refused.is_empty(), "plaintext reached plain TCP");
            let (socket, _) = listener.accept().unwrap();
            let mut beta = StreamOwned::new(
                ServerConnection::new(server(BETA, BETA_KEY, false)).unwrap(),
                socket,
            );
            beta.read_to_end(&mut refused).unwrap();
            assert!(refused.is_empty(), "plaintext reached beta.test");
        }
        let (socket, _) = listener.accept().unwrap();
        let mut stream = StreamOwned::new(
            ServerConnection::new(server(LEAF, KEY, !with_secret)).unwrap(),
            socket,
        );
        let mut request = vec![0; expected_request.len()];
        stream.read_exact(&mut request).unwrap();
        assert!(
            request == expected_request,
            "wrong plaintext reached the permitted peer"
        );
        stream.write_all(response.as_bytes()).unwrap();
        stream.conn.send_close_notify();
        stream.flush().unwrap();
    });

    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let app = dir.path().join("app");
    std::fs::create_dir(&app).unwrap();
    let fixture = if with_secret {
        include_str!("../../../core/front/fixtures/recording/tls-secret.hirsel")
    } else {
        include_str!("../../../core/front/fixtures/recording/tls.hirsel")
    };
    std::fs::write(
        app.join("main.hirsel"),
        fixture.replace("12345", &port.to_string()),
    )
    .unwrap();
    let roots = dir.path().join("roots.pem");
    std::fs::write(&roots, CA).unwrap();
    let tape = dir.path().join("tls.tape");

    let native = native_options(Backend::Cranelift, dir.path());
    let observer_src = dir.path().join("tls-observe.c");
    let observer_obj = dir.path().join("tls-observe.o");
    std::fs::write(&observer_src, include_str!("tape_tls_observe.c")).unwrap();
    command(
        Command::new(native.link.driver())
            .arg("-c")
            .arg(observer_src)
            .arg("-I")
            .arg(root.join("crates/hirsel-core-rt/include"))
            .arg("-o")
            .arg(&observer_obj),
    )
    .unwrap();
    let linker = appending_driver(
        dir.path(),
        "clang-observe-tls",
        &format!(
            "'{}' -Wl,--wrap=hirsel_core_run,--wrap=hirsel_core_host_io",
            observer_obj.display().to_string().replace('\'', "'\\''")
        ),
    );
    let common = [
        app.display().to_string(),
        "--backend".into(),
        "cranelift".into(),
        "--runtime".into(),
        env_record_runtime().display().to_string(),
        "--cc".into(),
        linker.display().to_string(),
        "--libraries".into(),
        root.join("core").display().to_string(),
        "--source-core".into(),
        "--source-std".into(),
        "--store".into(),
        dir.path().join("cache").display().to_string(),
    ];
    let tape_path = tape.to_str().unwrap();
    let invoke = |command: &str, args: &[&str]| {
        let mut call = vec![command.to_owned()];
        call.extend(common.iter().cloned());
        call.extend(args.iter().map(|&arg| arg.to_owned()));
        process::tape_request_command(&call, dir.path())
    };
    let net = format!(
        "Net=Connect 127.0.0.1/32 {port}-{port};Tls *.test {port}-{port} System;Tls alpha.test {port}-{port} Roots=sha256:{:x}",
        <sha2::Sha256 as sha2::Digest>::digest(CA)
    );
    let mut grants = vec!["--grant", &net, "--grant", "Stdout", "--record", tape_path];
    let file = dir.path().join("local-credential");
    let from_file = format!("Secret:local=file:{} to alpha.test", file.display());
    if with_secret {
        std::fs::write(&file, file_bytes).unwrap();
        grants.extend([
            "--grant",
            "Secret:token=env:HIRSEL_TL10_TOKEN to alpha.test mac",
            "--grant",
            &from_file,
            "--grant",
            "Secret:verifier=env:HIRSEL_TL10_TOKEN verify",
        ]);
    }
    let record = invoke("run", &grants)
        .env("HIRSEL_TL10_TOKEN", &token)
        .env("SSL_CERT_FILE", &roots)
        .env_remove("SSL_CERT_DIR")
        .env_remove("HIRSEL_TL9_REPLAY")
        .output()
        .unwrap();
    let after = |bytes: &[u8]| {
        String::from_utf8_lossy(bytes)
            .split_once(process::CHILD_MARK)
            .expect("request child started")
            .1
            .to_owned()
    };
    let recorded = after(&record.stdout);
    let record_log = after(&record.stderr);
    assert_eq!(record.status.code(), Some(0), "{record_log}");
    peer.join().unwrap();

    let digest_words = (0..64)
        .step_by(16)
        .map(|start| {
            u64::from_str_radix(&PEER[start..start + 16], 16)
                .unwrap()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join(" ");
    let expected = if with_secret {
        format!("[secret][secret]{response}mac verified\n")
    } else {
        format!("{RESPONSE}1 {digest_words}\n")
    };
    assert_eq!(recorded, expected);
    assert_eq!(record_log, expected);
    assert_eq!(
        std::fs::metadata(&tape).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let sealed = std::fs::read(&tape).unwrap();
    assert!(!sealed.windows(CA.len()).any(|bytes| bytes == CA));
    assert!(!sealed.windows(LEAF.len()).any(|bytes| bytes == LEAF));

    let assert_secrets_absent = |outputs: &[&[u8]]| {
        for bytes in [raw.as_slice(), token.as_bytes(), file_bytes.as_slice()] {
            for form in [
                bytes,
                crate::call::hex(bytes).as_bytes(),
                crate::call::base64(bytes).as_bytes(),
            ] {
                for output in outputs {
                    assert!(
                        !output.windows(form.len()).any(|word| word == form),
                        "a Secret representation escaped"
                    );
                }
            }
        }
        let mut padded = [0; 64];
        padded[..token.len()].copy_from_slice(token.as_bytes());
        for pad in [0x36, 0x5c] {
            let material = padded.map(|b| b ^ pad);
            for output in outputs {
                assert!(
                    !output
                        .windows(material.len())
                        .any(|bytes| bytes == material),
                    "a padded MAC key escaped"
                );
            }
        }
    };
    if with_secret {
        assert_secrets_absent(&[sealed.as_slice(), &record.stdout, &record.stderr]);
        std::fs::remove_file(&file).unwrap();
    }
    std::fs::remove_file(&roots).unwrap();
    let replay = invoke("replay", &["--tape", tape_path])
        .env("SSL_CERT_FILE", &roots)
        .env_remove("SSL_CERT_DIR")
        .env_remove("HIRSEL_TL10_TOKEN")
        .env("HIRSEL_TL9_REPLAY", "1")
        .output()
        .unwrap();
    let replay_log = after(&replay.stderr);
    assert_eq!(replay.status.code(), Some(0), "{replay_log}");
    assert!(after(&replay.stdout).is_empty());
    assert_eq!(replay_log, format!("{recorded}inner calls: 0\n"));
    assert_eq!(std::fs::read(&tape).unwrap(), sealed);

    let show = ["tape".into(), "show".into(), tape_path.into()];
    let (code, export, error) = process::tape_request(&show, dir.path());
    assert_ne!(code, Some(0));
    assert!(export.is_empty());
    assert!(error.contains("--include-credentials"), "{error}");
    let [command, subcommand, path] = show;
    let (code, export, error) = process::tape_request(
        &[command, subcommand, path, "--include-credentials".into()],
        dir.path(),
    );
    assert_eq!(code, Some(0), "{error}");
    assert!(export.contains("op=22"), "{export}");
    if with_secret {
        assert!(export.contains("op=23"));
        assert!(export.contains("grant `token` [secret] to alpha.test mac"));
        assert!(export.contains("grant `local` [secret] to alpha.test"));
        assert_secrets_absent(&[&replay.stdout, &replay.stderr, export.as_bytes()]);
        let mac_op = format!(" op={} ", hirsel_tape_format::op::SECRET_MAC);
        let mac: Vec<_> = export
            .lines()
            .filter(|line| line.contains(&mac_op))
            .collect();
        assert_eq!(
            mac.len(),
            6,
            "MAC operations issued without the necessary use"
        );
        assert_eq!(
            mac[0].split_once(" OP ").unwrap().1,
            mac[1].split_once(" OP ").unwrap().1,
            "CLI and narrow canonical MAC designations differ"
        );
    }
}

/// An ISA cap changes the machine body called by the entry, and has no tape
/// or descriptor identity. The independently computed swizzle checksum is
/// the same at both caps. The low-cap replay consumes the high-cap record.
#[cfg(target_arch = "x86_64")]
#[test]
fn simd_versions_preserve_run_tape_and_revision() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let tape = dir.path().join("vector.tape");
    let source = format!(
        "{}\npub fn main(output: Stdout) -> Result[(), Exit] waits {{
    if swizzle_32_32(255) != 6182210682794503310 {{ return Error(Exit(status: 6)) }}
    result.unwrap(stdout.write(output, \"simd\"))
    Ok(())
}}",
        include_str!("../../../core/front/fixtures/simd-differential.hirsel"),
    );
    let mut request = source_request(&source, dir.path());
    request.inputs.packages[1] =
        PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
    let product = Driver::default()
        .check(request, &mut Meter::new(false))
        .unwrap();
    assert!(product.accepted());
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let mut options = native_options(Backend::Llvm, dir.path());
    options.runtime = env_record_runtime();
    options.optimize = true;
    options.grant_mask = 1 << grant_kind("Stdout").unwrap();
    options.recording = true;
    let mut run = |replay: bool, cap: &str| {
        options.replay = replay.then(|| crate::ReplayPlan {
            path: tape.to_str().unwrap().into(),
            diagnose: false,
        });
        let built = product.build(&entry, &options).unwrap();
        assert_eq!(built.revision.0, code_revision_of(&built.executable));
        let mut cmd = Command::new(&built.executable);
        cmd.env("HIRSEL_ISA_CAP", cap);
        if !replay {
            cmd.env("HIRSEL_RECORD", &tape);
        }
        let output = cmd.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        (built.revision, output.stdout)
    };
    let levels = hirsel_core_layout::isa::X86_64_LEVELS;
    let (revision, recorded) = run(false, &levels.last().unwrap().number.to_string());
    assert_eq!(recorded, b"simd");
    let bytes = std::fs::read(&tape).unwrap();
    let (replay_revision, replayed) = run(true, &levels[0].number.to_string());
    assert_eq!(revision, replay_revision);
    assert!(replayed.is_empty());
    assert_eq!(std::fs::read(&tape).unwrap(), bytes);
}

/// NET-11: the host supplies a duplicate IPv6 result followed by IPv4.
/// Replay must deliver the same list with the resolver disabled.
#[test]
fn cli_replays_resolution_without_resolver() {
    const PROGRAM: &str = r#"pub fn main(n: Net) -> Result[(), Exit] waits {
    let child = result.unwrap(net.narrow(n, Resolve(names: "dup.test")))
    let ips = result.unwrap(std.net.lookup(child, "DUP.TEST.", Never))
    if list.len(ips) != 2 {
        return Error(Exit(status: 31))
    }
    let V6(0, 1) = ips[0] else {
        return Error(Exit(status: 32))
    }
    let V4(3221226001u32) = ips[1] else {
        return Error(Exit(status: 33))
    }
    Ok(())
}
"#;
    const RESOLVER: &str = r#"
#include <netdb.h>
#include <netinet/in.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

int __wrap_getaddrinfo(const char *name, const char *service,
                       const struct addrinfo *hints, struct addrinfo **out) {
    if (getenv("HIRSEL_NET11_OFFLINE")) {
        (void)write(2, "offline resolver called\n", 24);
        return EAI_AGAIN;
    }
    if (strcmp(name, "DUP.TEST.") != 0 || service ||
        !hints || hints->ai_socktype != SOCK_STREAM)
        return EAI_FAIL;
    static struct sockaddr_in6 v6 = { .sin6_family = AF_INET6, .sin6_addr = IN6ADDR_LOOPBACK_INIT };
    static struct sockaddr_in v4 = { .sin_family = AF_INET };
    static struct addrinfo answer[3];
    v4.sin_addr.s_addr = htonl(0xc0000211);
    answer[0] = (struct addrinfo){ .ai_family = AF_INET6, .ai_addr = (struct sockaddr *)&v6,
                                 .ai_addrlen = sizeof(v6), .ai_next = &answer[1] };
    answer[1] = answer[0];
    answer[1].ai_next = &answer[2];
    answer[2] = (struct addrinfo){ .ai_family = AF_INET, .ai_addr = (struct sockaddr *)&v4,
                                 .ai_addrlen = sizeof(v4) };
    *out = answer;
    (void)write(2, "resolver called\n", 16);
    return 0;
}
void __wrap_freeaddrinfo(struct addrinfo *answer) { (void)answer; }
"#;

    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let app = dir.path().join("app");
    std::fs::create_dir(&app).unwrap();
    std::fs::write(app.join("main.hirsel"), PROGRAM).unwrap();

    let native = native_options(Backend::Cranelift, dir.path());
    let source = dir.path().join("resolve.c");
    let object = dir.path().join("resolve.o");
    std::fs::write(&source, RESOLVER).unwrap();
    command(
        Command::new(native.link.driver())
            .arg("-c")
            .arg(source)
            .arg("-o")
            .arg(&object),
    )
    .unwrap();
    let linker = appending_driver(
        dir.path(),
        "clang-observe-resolve",
        &format!(
            "'{}' -Wl,--wrap=getaddrinfo,--wrap=freeaddrinfo",
            object.display().to_string().replace('\'', "'\\''"),
        ),
    );
    let common = [
        app.display().to_string(),
        "--backend".into(),
        "cranelift".into(),
        "--runtime".into(),
        env_record_runtime().display().to_string(),
        "--cc".into(),
        linker.display().to_string(),
        "--libraries".into(),
        root.join("core").display().to_string(),
        "--source-core".into(),
        "--source-std".into(),
        "--store".into(),
        dir.path().join("cache").display().to_string(),
    ];
    let invoke = |command: &str, args: &[&str]| {
        let mut call = vec![command.to_owned()];
        call.extend(common.iter().cloned());
        call.extend(args.iter().map(|&arg| arg.to_owned()));
        process::tape_request_command(&call, dir.path())
    };
    let tape = dir.path().join("resolve.tape");
    let path = tape.to_str().unwrap();
    let rule = "Resolve *.test,2001:db8::1";
    let record = invoke(
        "run",
        &["--grant", &format!("Net={rule}"), "--record", path],
    )
    .env_remove("HIRSEL_NET11_OFFLINE")
    .output()
    .unwrap();
    let record_log = String::from_utf8_lossy(&record.stderr);
    assert_eq!(record.status.code(), Some(0), "{record_log}");
    assert_eq!(record_log.matches("resolver called\n").count(), 1);

    use hirsel_tape_format::{Blocks, Decoder, END, Frame, op::RESOLVE};
    let recorded = std::fs::read(&tape).unwrap();
    let mut bytes = recorded.as_slice();
    let (mut blocks, header) = Blocks::open(|buf| std::io::Read::read(&mut bytes, buf)).unwrap();
    assert_eq!(
        header.net_rules,
        [hirsel_core_net::GrantRule::parse(rule).unwrap()],
        "resolve_rule_survives_grant_snapshot"
    );
    let (mut issues, mut deliveries) = (0, 0);
    let mut decoder = Decoder::default();
    while let Some((flags, payload)) = blocks
        .next(|buf| std::io::Read::read(&mut bytes, buf))
        .unwrap()
    {
        for frame in decoder.block(payload, flags & END != 0).unwrap() {
            match frame {
                Frame::Issue { op: RESOLVE, .. } => issues += 1,
                Frame::Done { .. } => deliveries += 1,
                _ => {}
            }
        }
    }
    assert_eq!(
        (issues, deliveries),
        (1, 1),
        "resolve_records_one_issue_and_delivery"
    );

    let replay = invoke("replay", &["--tape", path])
        .env("HIRSEL_NET11_OFFLINE", "1")
        .output()
        .unwrap();
    let replay_log = String::from_utf8_lossy(&replay.stderr);
    assert!(
        !replay_log.contains("offline resolver called"),
        "resolve_replay_makes_no_query: {replay_log}"
    );
    assert_eq!(
        replay.status.code(),
        Some(0),
        "resolve_replays_recorded_order: {replay_log}"
    );
}

/// Step 8.1 (RL1): recording changes nothing. Across release, recording-off,
/// and recording-on builds on both engines, stdout, exit status, cause sites,
/// Capacity refusals, and Env visibility match. The release executable defines
/// no `tape::` symbol and ignores `HIRSEL_RECORD`, the recording executable
/// does not embed any recording path, and `build --recording` with a release
/// runtime archive is refused before native emission.
#[test]
#[ignore = "explicit native integration target"]
fn rl1_recording_changes_nothing() {
    use object::{Object, ObjectSymbol};

    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let release_rt = native_options(Backend::Cranelift, dir.path())
        .runtime
        .canonicalize()
        .unwrap();
    let record_rt = PathBuf::from(env_record_runtime()).canonicalize().unwrap();
    let llvm = PathBuf::from(std::env::var_os("LLVM_SYS_231_PREFIX").unwrap());

    let cases: &[(&str, &str, u64, &[&str], bool)] = &[
        (
            "capacity_and_args",
            r#"
fn refused_box() -> bool {
    let attempted = isolate(fn() -> Result[Box[u64], Capacity] {
        bound bytes 4 { box.new(1u64) }
    })
    let Ok(Error(take causes)) = take attempted else { return false }
    for c in causes {
        return match c.kind {
            Capacity(k) => k.requested == 8 && k.available == 4,
            _ => false,
        }
    }
    false
}

pub fn main(a: Args, out: Stdout) -> Result[(), Exit] {
    let refused = if refused_box() { "cap" } else { "other" }
    result.unwrap(stdout.write(out, refused))
    match args.get(a, 0) {
        Some(v) => {
            result.unwrap(stdout.write(out, ":"))
            result.unwrap(stdout.write(out, v))
        },
        None => {},
    }
    Ok(())
}
"#,
            1 << 4,
            &["ok"],
            true,
        ),
        (
            "cause_site",
            r#"
pub fn main(out: Stdout) -> Result[(), Exit] {
    result.unwrap(stdout.write(out, "before"))
    let z = 0u64
    let _ = 1u64 / z
    Ok(())
}
"#,
            1 << 4,
            &[],
            false,
        ),
    ];

    for (name, source, grant_mask, argv, replays) in cases {
        for backend in [Backend::Cranelift, Backend::Llvm] {
            let mut request = source_request(source, dir.path());
            request.inputs.packages[1] =
                PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
            let product = Driver::default()
                .check(request, &mut Meter::new(false))
                .unwrap();
            assert!(
                product.accepted(),
                "{name} {backend:?}: {:?}",
                product.diagnostics
            );
            let entry = DefPath::from_full("app@1/main/main").unwrap();

            let mut rel_opts = native_options(backend, dir.path());
            rel_opts.runtime = release_rt.clone();
            rel_opts.recording = false;
            rel_opts.grant_mask = *grant_mask;
            rel_opts.output = dir.path().join(format!("rl1-{name}-{backend:?}-rel"));
            rel_opts.store = dir.path().join(format!("rl1-store-{name}-{backend:?}-rel"));
            let rel = product.build(&entry, &rel_opts).unwrap();

            // Release executable must contain zero `tape::` symbols.
            let rel_bytes = std::fs::read(&rel.executable).unwrap();
            let obj = object::File::parse(&*rel_bytes).unwrap();
            let tape_syms: Vec<_> = obj
                .symbols()
                .filter_map(|s| s.name().ok().map(str::to_owned))
                .filter(|n| n.contains("hirsel_core_rt4tape") || n.contains("tape::"))
                .collect();
            assert!(
                tape_syms.is_empty(),
                "release binary has tape symbols: {tape_syms:?}"
            );

            // Release executable must ignore HIRSEL_RECORD.
            let unused_tape = dir
                .path()
                .join(format!("rl1-{name}-{backend:?}-unused.tape"));
            let rel_run = tape_run(&rel.executable, argv, Some(&unused_tape));
            assert!(
                !unused_tape.exists(),
                "release binary created a recording file"
            );

            let mut rec_opts = rel_opts.clone();
            rec_opts.runtime = record_rt.clone();
            rec_opts.recording = true;
            rec_opts.output = dir.path().join(format!("rl1-{name}-{backend:?}-rec"));
            rec_opts.store = dir.path().join(format!("rl1-store-{name}-{backend:?}-rec"));
            let rec = product.build(&entry, &rec_opts).unwrap();
            assert_eq!(rel.revision, rec.revision, "{name} {backend:?} revision");

            let rec_tape = dir.path().join(format!("rl1-{name}-{backend:?}.tape"));
            let rec_bin_bytes = std::fs::read(&rec.executable).unwrap();
            assert!(
                !rec_bin_bytes
                    .windows(rec_tape.to_str().unwrap().len())
                    .any(|w| w == rec_tape.to_str().unwrap().as_bytes()),
                "recording executable must not embed a recording path"
            );

            let rec_off = tape_run(&rec.executable, argv, None);
            let rec_on = tape_run(&rec.executable, argv, Some(&rec_tape));
            assert_eq!(
                rec_off, rel_run,
                "{name} {backend:?}: recording-off != release"
            );
            assert_eq!(
                rec_on, rel_run,
                "{name} {backend:?}: recording-on != release"
            );
            if *name == "capacity_and_args" {
                assert_eq!(rec_on.1, "cap:ok");
            }
            if *replays {
                assert!(rec_tape.exists(), "{name} {backend:?}: tape not created");
                let mut rep_opts = rec_opts.clone();
                rep_opts.grant_mask = 0;
                rep_opts.output = dir.path().join(format!("rl1-{name}-{backend:?}-rep"));
                rep_opts.replay = Some(crate::ReplayPlan {
                    path: rec_tape.to_str().unwrap().into(),
                    diagnose: false,
                });
                let rep = product.build(&entry, &rep_opts).unwrap();
                let (code, out, err) = tape_run(&rep.executable, argv, None);
                assert_eq!(
                    (code, out),
                    (Some(0), String::new()),
                    "{name} {backend:?}: {err}"
                );
            }
        }
    }

    // `hirsel build --recording` with a release runtime archive is refused before native emission.
    let app = dir.path().join("rl1-refuse-app");
    std::fs::create_dir(&app).unwrap();
    std::fs::write(
        app.join("main.hirsel"),
        "pub fn main() -> Result[(), Exit] { Ok(()) }\n",
    )
    .unwrap();
    let refused_output = dir.path().join("rl1-refuse-out");
    let args = vec![
        "build".to_owned(),
        app.display().to_string(),
        "--recording".into(),
        "--runtime".into(),
        release_rt.display().to_string(),
        "--cc".into(),
        llvm.join("bin/clang").display().to_string(),
        "--libraries".into(),
        root.join("core").display().to_string(),
        "--source-core".into(),
        "--store".into(),
        dir.path().join("rl1-refuse-store").display().to_string(),
        "-o".into(),
        refused_output.display().to_string(),
    ];
    let (code, _, err) = process::tape_request(&args, dir.path());
    assert_ne!(code, Some(0), "expected refusal with release archive");
    assert!(
        err.contains("requires a recording runtime archive"),
        "{err}"
    );
    assert!(
        !refused_output.exists(),
        "refused build emitted an executable"
    );
}

/// Step 8.2 (RL2): divergence ends the replay at its hand-counted frame
/// position with exit status 1 and no further program output.
#[test]
#[ignore = "explicit native integration target"]
fn rl2_divergence_ends_replay_at_position() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let record_rt = PathBuf::from(env_record_runtime());
    let base = r#"
pub fn main(out: Stdout) -> Result[(), Exit] {
    result.unwrap(stdout.write(out, "alpha"))
    result.unwrap(stdout.write(out, "beta"))
    Ok(())
}
"#;
    // Frame 1 = GRANT(Stdout), Frame 2 = STDOUT("alpha"), Frame 3 = STDOUT("beta"), Frame 4 = END.
    let build = |source: &str, tag: &str, replay: Option<crate::ReplayPlan>| {
        let mut request = source_request(source, dir.path());
        request.inputs.packages[1] =
            PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
        let product = Driver::default()
            .check(request, &mut Meter::new(false))
            .unwrap();
        assert!(product.accepted(), "{tag}: {:?}", product.diagnostics);
        let mut options = native_options(Backend::Cranelift, dir.path());
        options.runtime = record_rt.clone();
        options.recording = true;
        options.grant_mask = if replay.is_some() { 0 } else { 1 << 4 };
        options.replay = replay;
        options.store = dir.path().join(format!("rl2-store-{tag}"));
        options.output = dir.path().join(format!("rl2-bin-{tag}"));
        product
            .build(&DefPath::from_full("app@1/main/main").unwrap(), &options)
            .unwrap()
            .executable
    };

    let tape = dir.path().join("rl2.tape");
    let rec = build(base, "rec", None);
    let (code, out, err) = tape_run(&rec, &[], Some(&tape));
    assert_eq!((code, out), (Some(0), "alphabeta".into()), "{err}");

    let diag = crate::ReplayPlan {
        path: tape.to_str().unwrap().into(),
        diagnose: true,
    };

    // (a) Send different bytes at frame 3 -> Diverged(Arg) at 3.
    let other_bytes = base.replace("\"beta\"", "\"gamma\"");
    let bin = build(&other_bytes, "arg", Some(diag.clone()));
    let (code, out, err) = tape_run(&bin, &[], None);
    assert_eq!((code, out), (Some(1), String::new()), "arg: {err}");
    assert!(err.contains("replay: Diverged(Arg) at 3"), "arg: {err}");

    // (b) Omit the second send -> END reached at frame 3 -> Diverged(Extra) at 3.
    let omitted = base.replace("    result.unwrap(stdout.write(out, \"beta\"))\n", "");
    let bin = build(&omitted, "omit", Some(diag.clone()));
    let (code, out, err) = tape_run(&bin, &[], None);
    assert_eq!((code, out), (Some(1), String::new()), "omit: {err}");
    assert!(err.contains("replay: Diverged(Extra) at 3"), "omit: {err}");

    // (c) Send an extra write after beta -> commit at frame 4 where tape has END -> Diverged(Schedule) at 4.
    let extra = base.replace(
        "    Ok(())",
        "    result.unwrap(stdout.write(out, \"gamma\"))\n    Ok(())",
    );
    let bin = build(&extra, "extra", Some(diag));
    let (code, out, err) = tape_run(&bin, &[], None);
    assert_eq!((code, out), (Some(1), String::new()), "extra: {err}");
    assert!(
        err.contains("replay: Diverged(Schedule) at 4"),
        "extra: {err}"
    );
}

/// Step 7.1: `hirsel test --replay` records each test into a temporary
/// owner-only directory, requires its assertions to pass, and replays to
/// Sealed; `--expect-verdict` checks the replay verdict.
#[test]
#[ignore = "explicit native integration target"]
fn test_replay_flag_records_and_replays() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let app = dir.path().join("test-app");
    std::fs::create_dir(&app).unwrap();
    std::fs::write(
        app.join("main.hirsel"),
        r#"
@test
fn writes_and_replays(out: Stdout) -> Result[(), ()] {
    result.unwrap(stdout.write(out, "hello\n"))
    Ok(())
}
"#,
    )
    .unwrap();
    let record_rt = PathBuf::from(env_record_runtime()).canonicalize().unwrap();
    let llvm = PathBuf::from(std::env::var_os("LLVM_SYS_231_PREFIX").unwrap());
    let base = vec![
        "test".to_owned(),
        app.display().to_string(),
        "--limit".into(),
        "1048576".into(),
        "--grant".into(),
        "Stdout".into(),
        "--replay".into(),
        "--runtime".into(),
        record_rt.display().to_string(),
        "--cc".into(),
        llvm.join("bin/clang").display().to_string(),
        "--libraries".into(),
        root.join("core").display().to_string(),
        "--source-core".into(),
        "--store".into(),
        dir.path().join("store").display().to_string(),
    ];
    let (code, out, err) = process::tape_request(&base, dir.path());
    assert_eq!(code, Some(0), "test --replay: {out}\n{err}");

    let mut sealed = base.clone();
    sealed.extend(["--expect-verdict".into(), "Sealed".into(), "--rerun".into()]);
    let (code, out, err) = process::tape_request(&sealed, dir.path());
    assert_eq!(
        code,
        Some(0),
        "test --replay --expect-verdict Sealed --rerun: {out}\n{err}"
    );

    let mut wrong_verdict = base;
    wrong_verdict.extend(["--expect-verdict".into(), "Diverged(Arg)".into()]);
    let (code, _, _) = process::tape_request(&wrong_verdict, dir.path());
    assert_ne!(code, Some(0), "mismatched --expect-verdict must fail");

    test_pass_cache_cli();
}

fn test_pass_cache_cli() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let app = dir.path().join("test-app");
    std::fs::create_dir(&app).unwrap();
    std::fs::write(
        app.join("main.hirsel"),
        r#"
@test
fn good() -> Result[(), ()] waits { Ok(()) }
@test
fn rejected() -> Result[(), ()] { Error(()) }
"#,
    )
    .unwrap();
    let runtime = root.join(std::env::var_os("HIRSEL_CORE_RT_ARCHIVE").unwrap());
    let llvm = PathBuf::from(std::env::var_os("LLVM_SYS_231_PREFIX").unwrap());
    let base = [
        "test".into(),
        app.display().to_string(),
        "--limit".into(),
        "1048576".into(),
        "--runtime".into(),
        runtime.display().to_string(),
        "--cc".into(),
        llvm.join("bin/clang").display().to_string(),
        "--libraries".into(),
        root.join("core").display().to_string(),
        "--source-core".into(),
        "--store".into(),
        dir.path().join("store").display().to_string(),
    ];
    let run = |args: &[&str], code| {
        let mut invocation = base.to_vec();
        invocation.extend(args.iter().map(|s| (*s).to_owned()));
        let output = process::tape_request_command(&invocation, dir.path())
            .env("XDG_CACHE_HOME", dir.path().join("cache"))
            .env("HIRSEL_HOST_PID", "invalid")
            .env("HIRSEL_CONTROL_FD", "4")
            .env("HIRSEL_SCRATCH", dir.path().join("absent-scratch"))
            .output()
            .unwrap();
        let out = String::from_utf8(output.stdout).unwrap();
        assert_eq!(
            output.status.code(),
            Some(code),
            "{out}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let out = out.split_once(process::CHILD_MARK).unwrap().1;
        if args.contains(&"--json") {
            return out.to_owned();
        }
        out.lines()
            .map(|line| {
                if line.starts_with("pass executed ") || line.starts_with("fail executed ") {
                    let (row, time) = line.rsplit_once(" (").unwrap();
                    assert!(time.strip_suffix(" ms)").unwrap().parse::<f64>().unwrap() >= 0.0);
                    row
                } else {
                    line
                }
            })
            .flat_map(|line| [line, "\n"])
            .collect::<String>()
    };

    let executed = "pass executed app@1/main/good\nfail executed app@1/main/rejected: error\n";
    let cached = "pass cached app@1/main/good\nfail executed app@1/main/rejected: error\n";
    assert_eq!(run(&["-j", "1"], 1), executed);
    assert_eq!(run(&["-j", "2"], 1), cached);
    assert_eq!(run(&["--rerun"], 1), executed);
    let mut rows: serde_json::Value = serde_json::from_str(&run(&["--json"], 1)).unwrap();
    let executed_time = rows["tests"][1]
        .as_object_mut()
        .unwrap()
        .remove("wall_ms")
        .unwrap();
    assert!(executed_time.as_f64().unwrap() >= 0.0);
    assert_eq!(
        rows,
        serde_json::json!({
            "passed": false,
            "tests": [
                {"definition":"app@1/main/good","passed":true,"source":"cached","status":0,"stdout":"","stderr":"","timed_out":false,"wall_ms":null},
                {"definition":"app@1/main/rejected","passed":false,"source":"executed","status":1,"stdout":"","stderr":"","timed_out":false}
            ]
        })
    );

    let good = ["--filter", "/good"];
    assert_eq!(run(&good, 0), "pass cached app@1/main/good\n");
    assert_eq!(run(&["--filter", "/absent"], 2), "");
    let with_grant = ["--filter", "/good", "--grant", "Stdout"];
    assert_eq!(run(&with_grant, 0), "pass cached app@1/main/good\n");
    let missing = dir.path().join("absent-dir");
    let with_dir = ["--filter", "/good", "--dir", missing.to_str().unwrap()];
    assert_eq!(run(&with_dir, 0), "pass cached app@1/main/good\n");
    let mut rerun_with_dir = with_dir.to_vec();
    rerun_with_dir.push("--rerun");
    assert_eq!(run(&rerun_with_dir, 0), "pass executed app@1/main/good\n");
    assert_eq!(run(&with_dir, 0), "pass cached app@1/main/good\n");
    assert!(!missing.exists());
}
