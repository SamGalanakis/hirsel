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

#[test]
fn foreign_source_replay_refuses_before_reading_tape() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let write = |name, text| std::fs::write(dir.path().join(name), text).unwrap();
    write(
        "hirsel.toml",
        r#"
[package]
name = "app"
major = 1
charge = "charge-v1"

[sources]
"main.hirsel" = "main"
"native.hirsel" = "native"

[foreign.native]
symbols = "hfl_"
archives = []
"#,
    );
    write(
        "main.hirsel",
        r#"
pub opaque type Law {}
fn enter(f: Foreign[Law]) {
    native.zed(f)
    native.alpha(f)
}
pub fn main(f: Foreign[Law]) -> Result[(), Exit] {
    enter(f)
    Ok(())
}
@test
fn touches(f: Foreign[Law]) -> Result[(), ()] {
    enter(f)
    Ok(())
}
"#,
    );
    write(
        "native.hirsel",
        "pub fn zed(f: Foreign[main.Law])\npub fn alpha(f: Foreign[main.Law])\n",
    );
    let llvm = PathBuf::from(std::env::var_os("LLVM_SYS_231_PREFIX").unwrap());
    let runtime = env_record_runtime();
    let run = |command: &str, args: &[&str]| {
        let mut argv = vec![command.to_owned(), dir.path().display().to_string()];
        argv.extend([
            "--runtime".into(),
            runtime.display().to_string(),
            "--cc".into(),
            llvm.join("bin/clang").display().to_string(),
            "--libraries".into(),
            root.join("core").display().to_string(),
            "--source-core".into(),
            "--source-std".into(),
            "--store".into(),
            dir.path().join("cache").display().to_string(),
        ]);
        argv.extend(args.iter().map(|s| (*s).to_owned()));
        process::tape_request(&argv, dir.path())
    };
    let expected_main = "FFI-8: a recording build cannot reach foreign code: app@1/main/main reaches app@1/native/alpha";
    let (code, _, error) = run("replay", &["--tape", "missing.tape"]);
    assert_eq!(code, Some(2), "{error}");
    let expected_replay = format!("replay: refused: {expected_main}");
    let mut lines = error
        .lines()
        .filter(|line| !line.starts_with("hirsel: created hirsel.lock"));
    assert!(
        lines.next().unwrap().starts_with("main.hirsel:5:"),
        "{error}"
    );
    assert_eq!(lines.collect::<Vec<_>>(), [expected_replay], "{error}");
    let (code, _, error) = run("build", &["--recording"]);
    assert_eq!(code, Some(2), "{error}");
    assert!(
        error
            .lines()
            .any(|line| line == format!("refused: {expected_main}")),
        "{error}"
    );
    let (code, output, error) = run("test", &["--replay", "--limit", "67108864"]);
    assert_eq!(code, Some(1), "{output}\n{error}");
    assert!(output.lines().any(|line| line.ends_with("refused: FFI-8: a recording build cannot reach foreign code: app@1/main/touches reaches app@1/native/alpha")), "{output}\n{error}");
    assert!(!dir.path().join("program").exists());
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

fn refused_literal() -> bool {
    let attempt = isolate(fn() -> Result[u64, Capacity] {
        bound bytes 15 {
            let literal: FrozenBytes = b"abcdefghi"
            list.len(bytebuf.as_bytes(literal))
        }
    })
    let Ok(Error(take causes)) = take attempt else { return false }
    for c in causes {
        return match c.kind {
            Capacity(k) => k.requested == 16 && k.available == 15,
            _ => false,
        }
    }
    false
}

pub fn main(a: Args, out: Stdout) -> Result[(), Exit] waits {
    if !refused_literal() { return Error(Exit(status: 2)) }
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
    let profiling = root.join(std::env::var_os("HIRSEL_CORE_RT_PROF_ARCHIVE").unwrap());
    let observe = root.join(std::env::var_os("HIRSEL_CORE_RT_PROF_RECORD_ARCHIVE").unwrap());
    for (archive, prof, record) in [
        (&plain, false, false),
        (&recording, false, true),
        (&profiling, true, false),
        (&observe, true, true),
    ] {
        let bytes = std::fs::read(archive).unwrap();
        let defines = |name| crate::installation::archive_defines_symbol(&bytes, name);
        assert_eq!(defines(b"hirsel_core_prof_runtime"), prof, "{archive:?}");
        assert_eq!(defines(b"hirsel_core_os_run_recording"), record, "{archive:?}");
    }
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
                        for archive in [&recording, &observe] {
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
                                archive.to_str().unwrap(),
                                "--tape",
                                tape.to_str().unwrap(),
                                "-o",
                                replay_output.to_str().unwrap(),
                            ]);
                            if !release {
                                replay.push("--release");
                            }
                            let (status, stdout, stderr) = invoke(&replay);
                            assert_eq!(status, Some(0), "{tag} replay on {archive:?}: {stderr}");
                            assert_eq!(code_revision_of(&replay_output), code_revision_of(&program));
                            assert!(
                                stdout.is_empty(),
                                "RL1 replay must not repeat live writes: {tag}"
                            );
                        }
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
            "'{}' -Wl,--wrap=hirsel_core_os_replay,--wrap=hirsel_core_host_io",
            observer_obj.display().to_string().replace('\'', "'\\''")
        ),
    );
    let common = [
        app.display().to_string(),
        "--backend".into(),
        "cranelift".into(),
        "--runtime".into(),
        root.join(
            std::env::var_os("HIRSEL_CORE_RT_SCRIPTED_RECORD_ARCHIVE")
                .expect("scripted recording runtime archive"),
        )
        .display()
        .to_string(),
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
    inspect_cli();
}

#[test]
fn served_scram_replays_with_the_same_send_calls() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let app = dir.path().join("app");
    std::fs::create_dir(&app).unwrap();
    let mut source = std::fs::read_to_string(root.join("core/core/tests/provide.hirsel")).unwrap();
    source.push_str(
        r#"
@test
fn scram_partial_send(tasks: Tasks) -> Result[(), ()] waits {
    if served_scram(tasks) { Ok(()) } else { Error(()) }
}
"#,
    );
    std::fs::write(app.join("main.hirsel"), source).unwrap();
    let runtime = env_record_runtime().canonicalize().unwrap();
    let llvm = PathBuf::from(std::env::var_os("LLVM_SYS_231_PREFIX").unwrap());
    let (code, out, err) = process::tape_request(
        &[
            "test".into(),
            app.display().to_string(),
            "--limit".into(),
            "1048576".into(),
            "--grant".into(),
            "Tasks".into(),
            "--replay".into(),
            "--expect-verdict".into(),
            "Sealed".into(),
            "--runtime".into(),
            runtime.display().to_string(),
            "--cc".into(),
            llvm.join("bin/clang").display().to_string(),
            "--libraries".into(),
            root.join("core").display().to_string(),
            "--source-core".into(),
            "--store".into(),
            dir.path().join("store").display().to_string(),
            "--json".into(),
        ],
        dir.path(),
    );
    assert_eq!(code, Some(0), "SC9 test --replay: {out}\n{err}");
    let report: serde_json::Value = serde_json::from_str(&out).unwrap();
    let tests = report["tests"].as_array().unwrap();
    assert_eq!(tests.len(), 1, "{report}");
    assert_eq!(tests[0]["definition"], "app@1/main/scram_partial_send");
    assert_eq!(tests[0]["source"], "executed");
    assert_eq!(tests[0]["passed"], true);
}

fn inspect_cli() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let app = dir.path().join("app");
    std::fs::create_dir(&app).unwrap();
    let file = app.join("main.hirsel");
    let source = r#"
pub opaque type Token {
    private_word: u64,
}
pub fn echo[T: plain](x: T, output: Stdout) -> T waits {
    result.unwrap(stdout.write(output, "echo"))
    x
}

fn base() -> u64 { 7u64 }
fn middle() -> u64 { base() }
const ANSWER: u64 = 11u64
fn folded() -> u64 { ANSWER }
fn orphan() {}

pub fn main() -> Result[(), Exit] { Ok(()) }

@test fn from_base() -> Result[(), ()] {
    if middle() >= 7u64 { Ok(()) } else { Error(()) }
}
@test fn from_constant() -> Result[(), ()] {
    if folded() >= 11u64 { Ok(()) } else { Error(()) }
}
@test fn independent() -> Result[(), ()] { Ok(()) }
"#;
    std::fs::write(&file, source).unwrap();
    let runtime = root.join(std::env::var_os("HIRSEL_CORE_RT_ARCHIVE").unwrap());
    let llvm = PathBuf::from(std::env::var_os("LLVM_SYS_231_PREFIX").unwrap());
    let command = |name: &str, package: &Path, args: &[&str]| {
        let mut invocation = vec![
            name.into(),
            package.display().to_string(),
            "--libraries".into(),
            root.join("core").display().to_string(),
            "--source-core".into(),
        ];
        invocation.extend(args.iter().map(|s| (*s).to_owned()));
        let output = process::tape_request_command(&invocation, dir.path())
            .env("XDG_CACHE_HOME", dir.path().join("cache"))
            .output()
            .unwrap();
        let out = String::from_utf8(output.stdout).unwrap();
        assert_eq!(
            output.status.code(),
            Some(0),
            "{invocation:?}\n{out}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        out.split_once(process::CHILD_MARK).unwrap().1.to_owned()
    };
    let inspect = |args| command("inspect", &app, args);
    let interface: serde_json::Value =
        serde_json::from_str(&inspect(&["--interface", "--json"])).unwrap();
    assert_eq!(
        interface,
        serde_json::json!({
            "version": 1,
            "package": {"name": "app", "version": "1"},
            "modules": [{"path": ["main"], "extends_core": false, "items": [
                {"definition": "app@1/main/Token", "name": "Token", "kind": "type", "signature": "pub opaque type Token"},
                {"definition": "app@1/main/echo", "name": "echo", "kind": "function", "signature": "pub fn echo[T: plain](x: T, output: Stdout) -> T waits"},
                {"definition": "app@1/main/main", "name": "main", "kind": "function", "signature": "pub fn main() -> Result[(), Exit]"}
            ]}],
        })
    );
    assert_eq!(
        inspect(&["--dependents", "main/base"]),
        "app@1/main/middle\n"
    );
    assert_eq!(
        inspect(&["--dependents", "app@1/main/base", "--transitive"]),
        "app@1/main/from_base\napp@1/main/middle\n",
    );
    assert_eq!(
        inspect(&["--unused"]),
        "app@1/main/Token\napp@1/main/echo\napp@1/main/orphan\n",
    );
    let library = inspect(&["--unused", "--library"]);
    assert!(library.lines().any(|line| line == "app@1/main/orphan"));
    assert!(!library.lines().any(|line| line == "app@1/main/echo"));

    let executed = || {
        let clang = llvm.join("bin/clang");
        let output: serde_json::Value = serde_json::from_str(&command(
            "test",
            &app,
            &[
                "--json",
                "--limit",
                "1048576",
                "--runtime",
                runtime.to_str().unwrap(),
                "--cc",
                clang.to_str().unwrap(),
            ],
        ))
        .unwrap();
        let tests = output["tests"].as_array().unwrap();
        assert_eq!(tests.len(), 3);
        tests
            .iter()
            .filter(|row| row["source"] == "executed")
            .map(|row| row["definition"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        executed(),
        [
            "app@1/main/from_base",
            "app@1/main/from_constant",
            "app@1/main/independent"
        ]
    );

    let affected = inspect(&["--affected-tests", "main/base"]);
    assert_eq!(affected, "app@1/main/from_base\n");
    let edited = source.replace("7u64 }", "8u64 }");
    std::fs::write(&file, &edited).unwrap();
    assert_eq!(executed(), affected.lines().collect::<Vec<_>>());

    let affected = inspect(&["--affected-tests", "main/ANSWER"]);
    assert_eq!(affected, "app@1/main/from_constant\n");
    std::fs::write(&file, edited.replace("= 11u64", "= 12u64")).unwrap();
    assert_eq!(executed(), affected.lines().collect::<Vec<_>>());

    let manifest_app = dir.path().join("manifest-app");
    std::fs::create_dir(&manifest_app).unwrap();
    std::fs::write(
        manifest_app.join("hirsel.toml"),
        r#"[package]
name = "counter"
major = 2
charge = "charge-v1"

[sources]
"app.hirsel" = "counter"

[program]
module = "counter"

[tests]
limit = 1048576

[tests.sources]
"laws.hirsel" = "counter"
"#,
    )
    .unwrap();
    std::fs::write(
        manifest_app.join("app.hirsel"),
        r#"
pub fn main() -> Result[(), Exit] { Ok(()) }
pub fn count() -> u64 { start() + 1u64 }
fn start() -> u64 { 4u64 }
pub fn spare() -> u64 { 0u64 }
"#,
    )
    .unwrap();
    std::fs::write(
        manifest_app.join("laws.hirsel"),
        r#"
@test fn counts() -> Result[(), ()] { if count() == 5u64 { Ok(()) } else { Error(()) } }
@test fn independent() -> Result[(), ()] { Ok(()) }
"#,
    )
    .unwrap();
    let inspect = |args| command("inspect", &manifest_app, args);
    let interface: serde_json::Value =
        serde_json::from_str(&inspect(&["--interface", "--json"])).unwrap();
    assert_eq!(
        interface,
        serde_json::json!({
            "version": 1,
            "package": {"name": "counter", "version": "2"},
            "modules": [{"path": ["counter"], "extends_core": false, "items": [
                {"definition": "counter@2/counter/main", "name": "main", "kind": "function", "signature": "pub fn main() -> Result[(), Exit]"},
                {"definition": "counter@2/counter/count", "name": "count", "kind": "function", "signature": "pub fn count() -> u64"},
                {"definition": "counter@2/counter/spare", "name": "spare", "kind": "function", "signature": "pub fn spare() -> u64"}
            ]}],
        })
    );
    assert_eq!(
        inspect(&["--affected-tests", "counter/start"]),
        "counter@2/counter/counts\n",
    );
    assert_eq!(inspect(&["--unused"]), "counter@2/counter/spare\n");
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

    let executed = "pass executed app@1/main/good\nfail executed app@1/main/rejected: returned an error, exit status 1: error: ()\n";
    let cached = "pass cached app@1/main/good\nfail executed app@1/main/rejected: returned an error, exit status 1: error: ()\n";
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
                {"definition":"app@1/main/rejected","passed":false,"source":"executed","status":1,"stdout":"","stderr":"error: ()\n","timed_out":false}
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

#[test]
fn run_tape_replays_granted_terminal_bits() {
    use crate::host::terminal_fixture;
    use std::process::Stdio;

    let dir = tempfile::tempdir().unwrap();
    let bits = dir.path().join("bits");
    let tape = dir.path().join("terminals.tape");
    let product = terminal_fixture::product(dir.path());
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let mut options = terminal_fixture::options(Backend::Cranelift, dir.path());
    options.runtime = env_record_runtime();
    options.recording = true;
    let record = product.build(&entry, &options).unwrap();
    let (_master, slave) = terminal_fixture::pty();
    let output = Command::new(&record.executable)
        .env("HIRSEL_RECORD", &tape)
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave))
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output.stderr);
    assert_eq!(std::fs::read(&bits).unwrap(), [1, 1, 0]);
    std::fs::remove_file(&bits).unwrap();

    options.grant_mask = 0;
    options.directories.clear();
    options.replay = Some(crate::ReplayPlan {
        path: tape.to_str().unwrap().into(),
        diagnose: false,
    });
    let replay = product.build(&entry, &options).unwrap();
    let output = Command::new(replay.executable)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output.stderr);
    assert!(!bits.exists());
}

#[test]
fn run_tape_records_scram_outcome_without_key_material() {
    use hirsel_core_net::{GrantRule, SecretGrant};
    use rustls::{
        ServerConfig, ServerConnection, StreamOwned,
        pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
    };
    use std::{
        io::{Read, Write},
        net::{Ipv4Addr, TcpListener},
        sync::Arc,
    };

    const CA: &[u8] = include_bytes!("../../hirsel-core-rt/testdata/tls/ca.pem");
    const LEAF: &[u8] = include_bytes!("../../hirsel-core-rt/testdata/tls/alpha.pem");
    const KEY: &[u8] = include_bytes!("../../hirsel-core-rt/testdata/tls/alpha.key.pem");
    const CLIENT_FINAL: &[u8] = b"c=biws,r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,p=dHzbZapWIk4jUhN+Ute9ytag9zjfMHgsqmmiz7AndVQ=";
    const SERVER_FINAL: &[u8] = b"v=6rriTRBi23WpRR/wtup+mMhUZUn/dB5nLTJRsjl95G4=";
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let config =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from_pem_slice(LEAF).unwrap()],
                PrivateKeyDer::from_pem_slice(KEY).unwrap(),
            )
            .unwrap();

    let source = r#"
pub fn main(n: Net, password: Secret, out: Stdout) -> Result[(), Exit] waits {
    let peer = Addr(ip: V4(2130706433u32), port: 12345u16)
    let socket = result.unwrap(host.connect(n, peer, Never))
    let alpn: List[String] = list.new()
    let (r, w, _) = result.unwrap(tls.connect(n, take socket, "alpha.test", System, alpn, Never))
    let mut read = take r
    let mut write = take w
    let client: Bytes = b"n,,n=user,r=rOprNGfwEbeRWgbNEkqO"
    let server: Bytes = b"r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096"
    let empty: Bytes = list.new()
    if result.unwrap(secret.scram_final_len(write, client, server)) != 106 {
        return Error(Exit(status: 1))
    }
    let mut response: Bytes = list.with_capacity(46)
    let check = result.unwrap(secret.scram_sha256(mut write, password, client, server, empty, empty, Never))
    while list.len(response) < 46 {
        if result.unwrap(host.recv_bounded(mut read, mut response, 46, Never)) == 0 {
            return Error(Exit(status: 2))
        }
    }
    if !secret.scram_verify(take check, response) {
        return Error(Exit(status: 3))
    }
    result.unwrap(stdout.write(out, "verified"))
    Ok(())
}
"#.replace("12345", &port.to_string());
    let dir = tempfile::tempdir().unwrap();
    let roots = dir.path().join("roots.pem");
    std::fs::write(&roots, CA).unwrap();
    let tape = dir.path().join("scram.tape");
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../core/core");
    let mut request = source_request(&source, dir.path());
    request.inputs.packages[1] = PackageManifest::directory(&root, package("core"), true).unwrap();
    let product = Driver::default()
        .check(request, &mut Meter::new(false))
        .unwrap();
    assert!(
        product.accepted(),
        "{:?} {:?}",
        product.diagnostics,
        product.incomplete
    );
    let mut options = native_options(Backend::Cranelift, dir.path());
    options.grant_mask = (1 << grant_kind("Net").unwrap()) | (1 << grant_kind("Stdout").unwrap());
    options.runtime = env_record_runtime();
    options.recording = true;
    options.net_rules = [
        format!("Connect 127.0.0.1/32 {port}-{port}"),
        format!("Tls alpha.test {port}-{port} System"),
    ]
    .map(|text| GrantRule::parse(&text).unwrap())
    .into();
    options.secret_grants =
        vec![SecretGrant::new("password", "env:HIRSEL_SCRAM_PASSWORD", "alpha.test").unwrap()];
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let record = product.build(&entry, &options).unwrap();

    let peer = std::thread::spawn(move || {
        let (socket, _) = listener.accept().unwrap();
        let mut peer = StreamOwned::new(ServerConnection::new(Arc::new(config)).unwrap(), socket);
        let mut proof = [0; 106];
        peer.read_exact(&mut proof).unwrap();
        assert_eq!(&proof, CLIENT_FINAL);
        peer.write_all(SERVER_FINAL).unwrap();
        peer.conn.send_close_notify();
        peer.flush().unwrap();
    });
    let output = Command::new(record.executable)
        .env("SSL_CERT_FILE", &roots)
        .env_remove("SSL_CERT_DIR")
        .env("HIRSEL_SCRAM_PASSWORD", "pencil")
        .env("HIRSEL_RECORD", &tape)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"verified");
    peer.join().unwrap();

    let sealed = std::fs::read(&tape).unwrap();
    let key_material = [
        "c4a49510323ab4f952cac1fa99441939e78ea74d6be81ddf7096e87513dc615d",
        "a60fc923d67e8644a92d16b96eda5ef4656b0c725c484374be25535576996e8b",
        "586e5df283e6dceb5c3e791d8b8528ec191e664045ce971792e2e6b5bb13e2a6",
        "c1f3cbc1c13a9d35a14c0990eed97629ea225863e566a4314ab99f3f00e5d9d5",
        "747cdb65aa56224e2352137e52d7bdcad6a0f738df30782caa69a2cfb0277554",
    ];
    let check_absent = |bytes: &[u8]| {
        for form in [
            bytes,
            crate::call::hex(bytes).as_bytes(),
            crate::call::base64(bytes).as_bytes(),
        ] {
            assert!(
                !sealed.windows(form.len()).any(|window| window == form),
                "SCRAM key material in tape"
            );
        }
    };
    check_absent(b"pencil");
    for hex in key_material {
        let bytes = (0..hex.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).unwrap())
            .collect::<Vec<_>>();
        check_absent(&bytes);
    }
    assert!(
        sealed
            .windows(SERVER_FINAL.len())
            .any(|window| window == SERVER_FINAL)
    );

    let mut input = sealed.as_slice();
    let (mut blocks, _) = hirsel_tape_format::Blocks::open(|buffer| input.read(buffer)).unwrap();
    let mut decoder = hirsel_tape_format::Decoder::default();
    let mut calls = Vec::new();
    let mut issued = std::collections::BTreeMap::new();
    while let Some((flags, payload)) = blocks.next(|buffer| input.read(buffer)).unwrap() {
        use hirsel_tape_format::{Frame, op};
        for frame in decoder
            .block(payload, flags & hirsel_tape_format::END != 0)
            .unwrap()
        {
            let (operation, out) = match frame {
                Frame::Issue { task, op, .. } => {
                    issued.insert(task, op);
                    continue;
                }
                Frame::Op { op, out, .. } => (op, out),
                Frame::Done { task, out, .. } => (issued.remove(&task).unwrap(), out),
                _ => continue,
            };
            if matches!(operation, op::SCRAM | op::SCRAM_VERIFY) {
                assert_eq!(out.status, hirsel_tape_format::OK);
                assert!(out.payload.is_empty());
                calls.push(operation);
            }
        }
    }
    assert_eq!(
        calls,
        [
            hirsel_tape_format::op::SCRAM,
            hirsel_tape_format::op::SCRAM_VERIFY
        ]
    );

    options.grant_mask = 0;
    options.net_rules.clear();
    options.secret_grants.clear();
    options.backend = Backend::Llvm;
    options.replay = Some(crate::ReplayPlan {
        path: tape.to_str().unwrap().into(),
        diagnose: false,
    });
    let replay = product.build(&entry, &options).unwrap();
    let output = Command::new(replay.executable)
        .env_remove("HIRSEL_SCRAM_PASSWORD")
        .env_remove("SSL_CERT_FILE")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
}

#[test]
fn postgres_scripted_peer_laws() {
    use hirsel_core_net::{GrantRule, SecretGrant};
    use rustls::{
        ServerConfig, ServerConnection, StreamOwned,
        pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
    };
    use std::{
        io::{ErrorKind, Read, Write},
        net::{Ipv4Addr, TcpListener, TcpStream},
        sync::Arc,
    };

    type Peer = StreamOwned<ServerConnection, TcpStream>;

    const CA: &[u8] = include_bytes!("../../hirsel-core-rt/testdata/tls/ca.pem");
    const LEAF: &[u8] = include_bytes!("../../hirsel-core-rt/testdata/tls/alpha.pem");
    const KEY: &[u8] = include_bytes!("../../hirsel-core-rt/testdata/tls/alpha.key.pem");
    const PASSWORD: &str = "pencil";
    const FIRST: &[u8] = b"n,,n=,r=rOprNGfwEbeRWgbNEkqO";
    const NONCE: &[u8] = b"r=rOprNGfwEbeRWgbNEkqO,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096";
    const EXTENDED: &[u8] =
        b"r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096";
    // RFC 7677's salt, nonce and password with PostgreSQL's empty SCRAM name.
    // Independently derived using PBKDF2, HMAC and SHA-256 from RFC 5802 §3.
    const PROOF: &[u8] = b"c=biws,r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,p=qvT2SWdEH5Q06albL+hjSYuUhCG7VndFyzIb7CK4n9k=";
    const VERIFIER: &[u8] = b"v=3HO6Qt1M4MKJrmlKaoOqLAI0/0TV0HZe7J9H3MBtSOg=";
    const SQL_ERROR: &[u8] = b"SERROR\0VERROR\0C22012\0Mdivision by zero\0Dscripted detail\0\0";
    // One int4 column. Table, attribute and modifier are unset. Width is
    // four and statement Describe reports format zero per protocol 3.0.
    const DESCRIPTION: &[u8] =
        b"\x00\x01x\0\x00\x00\x00\x00\x00\x00\x00\x00\x00\x17\x00\x04\xff\xff\xff\xff\x00\x00";
    const PORTAL: &[u8] = b"hirsel_rows\0";

    fn packet(out: &mut Vec<u8>, tag: u8, body: &[u8]) {
        let len = u32::try_from(body.len() + 4).unwrap().to_be_bytes();
        out.push(tag);
        out.extend_from_slice(&len);
        out.extend_from_slice(body);
    }

    fn send(peer: &mut Peer, tag: u8, body: &[u8]) {
        let mut out = Vec::new();
        packet(&mut out, tag, body);
        peer.write_all(&out).unwrap();
    }

    fn recv(peer: &mut Peer, tag: u8) -> Vec<u8> {
        let mut header = [0; 5];
        peer.read_exact(&mut header).unwrap();
        assert_eq!(header[0], tag, "expected frontend tag {}", char::from(tag));
        let len = u32::from_be_bytes(header[1..].try_into().unwrap());
        assert!((4..=1024).contains(&len));
        let mut body = vec![0; (len - 4) as usize];
        peer.read_exact(&mut body).unwrap();
        body
    }

    fn ended(peer: &mut impl Read) {
        match peer.read(&mut [0]) {
            Ok(0) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::UnexpectedEof | ErrorKind::ConnectionReset
                ) => {}
            other => panic!("expected stream retirement, got {other:?}"),
        }
    }

    fn authenticate(peer: &mut Peer, name: &str) -> bool {
        const STARTUP: &[u8] =
            b"\x00\x03\x00\x00user\0user\0database\0postgres\0client_encoding\0UTF8\0\0";
        let mut startup = vec![0; 4 + STARTUP.len()];
        peer.read_exact(&mut startup).unwrap();
        assert_eq!(startup[..4], (startup.len() as u32).to_be_bytes());
        assert_eq!(&startup[4..], STARTUP);

        // Send no body for malformed lengths or tags. Any attempted read of the
        // claimed body returns a timeout rather than the expected protocol error.
        let header = match name {
            "short" => Some(*b"R\x00\x00\x00\x03"),
            "negative" => Some(*b"R\xff\xff\xff\xff"),
            "large" => Some(*b"R\x7f\xff\xff\xff"),
            "tag" => Some(*b"?\x00\x00\x00\x04"),
            _ => None,
        };
        if let Some(header) = header {
            peer.write_all(&header[..2]).unwrap();
            peer.write_all(&header[2..]).unwrap();
            ended(peer);
            return false;
        }

        send(peer, b'R', b"\x00\x00\x00\x0aSCRAM-SHA-256\0\0");
        let mut first = b"SCRAM-SHA-256\0".to_vec();
        first.extend_from_slice(&(FIRST.len() as i32).to_be_bytes());
        first.extend_from_slice(FIRST);
        assert_eq!(recv(peer, b'p'), first);

        let mut continuation = vec![0, 0, 0, 11];
        continuation.extend_from_slice(if name == "nonce" { NONCE } else { EXTENDED });
        send(peer, b'R', &continuation);
        if name == "nonce" {
            ended(peer);
            return false;
        }
        assert_eq!(recv(peer, b'p'), PROOF);

        let mut final_message = vec![0, 0, 0, 12];
        final_message.extend_from_slice(VERIFIER);
        if name == "signature" {
            final_message[6] = b'4';
        }
        let mut ready = Vec::new();
        packet(&mut ready, b'R', &final_message);
        packet(&mut ready, b'R', &[0; 4]);
        packet(&mut ready, b'S', b"client_encoding\0UTF8\0");
        packet(&mut ready, b'K', b"\x12\x34\x56\x78\x9a\xbc\xde\xf0");
        packet(&mut ready, b'Z', b"I");
        peer.write_all(&ready).unwrap();
        true
    }

    fn begin(peer: &mut Peer, sql: &str, returns_rows: bool) {
        let mut parse = vec![0];
        parse.extend_from_slice(sql.as_bytes());
        parse.extend_from_slice(&[0; 3]);
        assert_eq!(recv(peer, b'P'), parse);
        assert_eq!(recv(peer, b'D'), b"S\0");
        assert!(recv(peer, b'H').is_empty());
        send(peer, b'1', b"");
        send(peer, b't', b"\0\0");
        if returns_rows {
            send(peer, b'T', DESCRIPTION);
        } else {
            send(peer, b'n', b"");
        }

        let mut bind = PORTAL.to_vec();
        bind.extend_from_slice(b"\0\x00\x01\x00\x01\x00\x00");
        bind.extend_from_slice(if returns_rows {
            b"\x00\x01\x00\x01"
        } else {
            b"\0\0"
        });
        assert_eq!(recv(peer, b'B'), bind);
        assert!(recv(peer, b'H').is_empty());
        send(peer, b'2', b"");
        demand(peer);
    }

    fn demand(peer: &mut Peer) {
        let mut execute = PORTAL.to_vec();
        execute.extend_from_slice(&1u32.to_be_bytes());
        assert_eq!(recv(peer, b'E'), execute);
        assert!(recv(peer, b'H').is_empty());
    }

    fn close_portal(peer: &mut Peer) {
        let mut close = vec![b'P'];
        close.extend_from_slice(PORTAL);
        assert_eq!(recv(peer, b'C'), close);
        assert!(recv(peer, b'H').is_empty());
        send(peer, b'3', b"");
    }

    fn sync(peer: &mut Peer, status: &[u8]) {
        assert!(recv(peer, b'S').is_empty());
        send(peer, b'Z', status);
    }

    fn recover(peer: &mut Peer) {
        begin(peer, "SELECT 1 / x FROM inputs", true);
        send(peer, b'A', b"\0\0\0\x01channel\0payload\0");
        send(
            peer,
            b'N',
            b"SNOTICE\0VNOTICE\0C00000\0Mscripted notice\0\0",
        );
        send(peer, b'S', b"standard_conforming_strings\0off\0");
        send(peer, b'D', b"\x00\x01\x00\x00\x00\x04\x00\x00\x00\x07");
        send(peer, b's', b"");
        demand(peer);
        send(peer, b'E', SQL_ERROR);
        sync(peer, b"I");

        close_portal(peer);
        begin(peer, "SELECT 42", true);
        send(peer, b'D', b"\x00\x01\x00\x00\x00\x04\x00\x00\x00\x2a");
        send(peer, b's', b"");
        demand(peer);
        send(peer, b'C', b"SELECT 1\0");
        close_portal(peer);
        sync(peer, b"I");

        begin(peer, "SELECT 'late'", true);
        send(peer, b'C', b"SELECT 0\0");
        close_portal(peer);
        assert!(recv(peer, b'S').is_empty());
        send(peer, b'E', SQL_ERROR);
        send(peer, b'Z', b"I");

        begin(peer, "BEGIN", false);
        send(peer, b'C', b"BEGIN\0");
        close_portal(peer);
        sync(peer, b"T");
        begin(peer, "SELECT 1 / x FROM inputs", true);
        send(peer, b'E', SQL_ERROR);
        sync(peer, b"E");

        // The rejected SELECT must send nothing. The next exchange closes
        // the old portal before binding the ROLLBACK in the failed session.
        close_portal(peer);
        begin(peer, "ROLLBACK", false);
        send(peer, b'C', b"ROLLBACK\0");
        close_portal(peer);
        sync(peer, b"I");
        assert!(recv(peer, b'X').is_empty());
        ended(peer);
    }

    fn cancel_race(listener: &TcpListener, config: &ServerConfig, original: &mut Peer) {
        // The new stream carries 16, (1234 << 16) | 5678, then exactly
        // the four-byte PID and four-byte key in BackendKeyData (protocol 3.0).
        let (socket, _) = listener.accept().unwrap();
        let mut tls = config.clone();
        tls.alpn_protocols = vec![b"postgresql".to_vec()];
        let mut cancel = StreamOwned::new(ServerConnection::new(Arc::new(tls)).unwrap(), socket);
        let mut request = [0; 16];
        cancel.read_exact(&mut request).unwrap();
        assert_eq!(
            request,
            *b"\x00\x00\x00\x10\x04\xd2\x16\x2e\x12\x34\x56\x78\x9a\xbc\xde\xf0"
        );
        cancel.conn.send_close_notify();
        cancel.flush().unwrap();
        ended(&mut cancel);

        begin(original, "SELECT 42", true);
        send(original, b'D', b"\x00\x01\x00\x00\x00\x04\x00\x00\x00\x2a");
        send(original, b's', b"");
        demand(original);
        send(original, b'C', b"SELECT 1\0");
        close_portal(original);
        sync(original, b"I");
        ended(original);
    }

    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let config =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from_pem_slice(LEAF).unwrap()],
                PrivateKeyDer::from_pem_slice(KEY).unwrap(),
            )
            .unwrap();

    let dir = tempfile::tempdir().unwrap();
    let roots = dir.path().join("roots.pem");
    std::fs::write(&roots, CA).unwrap();
    let source = format!(
        "{}\n{}",
        include_str!("../../../core/programs/postgres/postgres.hirsel"),
        include_str!("../../../core/front/fixtures/recording/postgres-peer.hirsel")
            .replace("12345", &port.to_string()),
    );
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../core");
    let mut request = source_request(&source, dir.path());
    request.inputs.packages[1] =
        PackageManifest::directory(&root.join("core"), package("core"), true).unwrap();
    let mut std = PackageManifest::directory(&root.join("std"), package("std"), false).unwrap();
    std.dependencies.push(package("core"));
    request.inputs.packages.push(std);
    request.inputs.packages[0].dependencies.push(package("std"));
    let product = Driver::default()
        .check(request, &mut Meter::new(false))
        .unwrap();
    assert!(
        product.accepted(),
        "{:?} {:?}",
        product.diagnostics,
        product.incomplete
    );

    let mut options = native_options(Backend::Cranelift, dir.path());
    options.grant_mask = ["Clock", "Net"]
        .iter()
        .map(|name| 1 << grant_kind(name).unwrap())
        .sum();
    options.instance_limit = crate::native::DEFAULT_INSTANCE_LIMIT;
    options.net_rules = [
        format!("Connect 127.0.0.1/32 {port}-{port}"),
        format!("Tls alpha.test {port}-{port} System"),
    ]
    .map(|rule| GrantRule::parse(&rule).unwrap())
    .into();
    options.secret_grants =
        vec![SecretGrant::new("password", "env:HIRSEL_SCRAM_PASSWORD", "alpha.test").unwrap()];
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let program = product.build(&entry, &options).unwrap();

    let cases = [
        "short",
        "negative",
        "large",
        "tag",
        "nonce",
        "signature",
        "alpn",
        "short-row",
        "trailing",
        "reuse",
        "ssl-refused",
        "ssl-extra",
        "cancel",
    ];
    let peer = std::thread::spawn(move || {
        for name in cases {
            let (mut socket, _) = listener.accept().unwrap();
            if matches!(name, "ssl-refused" | "ssl-extra") {
                // SSLRequest has length 8 and request code (1234 << 16) | 5679.
                let mut request = [0; 8];
                socket.read_exact(&mut request).unwrap();
                assert_eq!(request, *b"\x00\x00\x00\x08\x04\xd2\x16\x2f");
                let reply: &[u8] = if name == "ssl-refused" { b"N" } else { b"S!" };
                socket.write_all(reply).unwrap();
                ended(&mut socket);
                continue;
            }
            let mut tls = config.clone();
            if name != "alpn" {
                tls.alpn_protocols = vec![b"postgresql".to_vec()];
            }
            let mut peer = StreamOwned::new(ServerConnection::new(Arc::new(tls)).unwrap(), socket);
            if name == "alpn" {
                ended(&mut peer);
            } else if authenticate(&mut peer, name) {
                match name {
                    "signature" => ended(&mut peer),
                    "reuse" => recover(&mut peer),
                    "cancel" => cancel_race(&listener, &config, &mut peer),
                    _ => {
                        begin(&mut peer, "SELECT 1", true);
                        let row: &[u8] = if name == "short-row" {
                            b"\x00\x01\x7f\xff\xff\xff"
                        } else {
                            b"\x00\x01\x00\x00\x00\x04\x00\x00\x00\x01\x99"
                        };
                        send(&mut peer, b'D', row);
                        ended(&mut peer);
                    }
                }
            }
        }
    });
    for name in cases {
        let outcome = Command::new(&program.executable)
            .arg(name)
            .env("SSL_CERT_FILE", &roots)
            .env_remove("SSL_CERT_DIR")
            .env("HIRSEL_SCRAM_PASSWORD", PASSWORD)
            .output()
            .unwrap();
        assert!(
            outcome.status.success(),
            "{name}: {} {}",
            outcome.status,
            String::from_utf8_lossy(&outcome.stderr)
        );
    }
    peer.join().unwrap();
}

/// The replay has neither the copied executable nor the Unix root used by the
/// recorded run. It must recover the child, pipes and packet only from tape.
#[test]
fn exec_unix_recording_replays_without_host_resources() {
    use hirsel_core_net::{GrantRule, UnixDirection, UnixRule};
    use sha2::{Digest, Sha256};

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path().join("work");
    std::fs::create_dir(&work).unwrap();
    let image = work.join("image");
    std::fs::copy("/bin/cat", &image).unwrap();
    let pin = Sha256::digest(std::fs::read(&image).unwrap());
    let policy = work.join("policy.toml");
    // The recording must split a valid profile snapshot larger than a tape block.
    let unused_arg = "x".repeat(hirsel_tape_format::MAX_LEN);
    std::fs::write(
        &policy,
        format!(
            "[cat]\nimage = {:?}\nsha256 = \"{pin:x}\"\nargv0 = \"cat\"\nargs = [[], [\"{unused_arg}\"]]\nenv = {{}}\ndirs = {{ work = {:?} }}\n",
            image.to_str().unwrap(),
            work.to_str().unwrap(),
        ),
    )
    .unwrap();
    let request =
        hirsel_core_net::exec::Request::parse(&format!("{} cat", policy.display())).unwrap();
    let digest = request.load().unwrap().digest;
    let source =
        std::fs::read_to_string(root.join("core/front/fixtures/recording/exec-unix.hirsel"))
            .unwrap();
    let mut inputs = source_request(&source, dir.path());
    inputs.inputs.packages[1] =
        PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
    let checked = Driver::default()
        .check(inputs, &mut Meter::new(false))
        .unwrap();
    assert!(
        checked.accepted(),
        "{:?} {:?}",
        checked.diagnostics,
        checked.incomplete
    );
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let mut options = native_options(Backend::Cranelift, dir.path());
    options.runtime = PathBuf::from(env_record_runtime());
    options.recording = true;
    options.output = dir.path().join("record");
    options.grant_mask = 1 << grant_kind("Net").unwrap();
    options.exec_grants.push(crate::exec_launch::Binding {
        request,
        policy: Some(digest),
        workdir: None,
    });
    options
        .unix_roots
        .insert("work".into(), work.to_str().unwrap().into());
    options.net_rules = [UnixDirection::Bind, UnixDirection::Send]
        .into_iter()
        .map(|direction| GrantRule::Unix(UnixRule::new(direction, "work", "packet").unwrap()))
        .collect();
    let executable = checked.build(&entry, &options).unwrap().executable;
    let tape = dir.path().join("run.tape");
    let mut record = Command::new(executable);
    options.exec_grants[0].approve(&mut record);
    let recorded = record.env("HIRSEL_RECORD", &tape).output().unwrap();
    assert_eq!(
        recorded.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&recorded.stderr)
    );
    let sealed = std::fs::read(&tape).unwrap();
    std::fs::remove_dir_all(&work).unwrap();

    options.output = dir.path().join("replay");
    options.exec_grants.clear();
    options.net_rules.clear();
    options.unix_roots.clear();
    options.grant_mask = 0;
    options.replay = Some(crate::ReplayPlan {
        path: tape.to_str().unwrap().into(),
        diagnose: false,
    });
    let replay = checked.build(&entry, &options).unwrap().executable;
    let (status, _, stderr) = tape_run(&replay, &[], None);
    assert_eq!(status, Some(0), "{stderr}");
    assert!(!work.exists());
    assert_eq!(std::fs::read(tape).unwrap(), sealed);
}
