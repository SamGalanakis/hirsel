// REPLAY-SPEC v2.3 R10: every executable carries its entry's code revision as
// the 32-byte symbol `hirsel_code_revision`, which a Run tape's header records
// and a Replay open compares (the runtime's
// open_refuses_a_changed_code_revision_arg_limit_or_policy).

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

/// The oracle is the revision the build reports (revision.rs, from the checked
/// IR, not from the link), which a Call header must also agree with. Wrong
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
        let built = product.build(&entry, &options).unwrap();
        assert_eq!(
            code_revision_of(&built.executable),
            built.revision.0,
            "{backend:?}"
        );
    }
}

/// Builds core/front/tape/run.hirsel, with `edit` applied to its text,
/// into `store`; the run's directory grant is `grant`.
fn tape_run_build(backend: Backend, dir: &Path, store: &str, edit: (&str, &str)) -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let source = std::fs::read_to_string(root.join("core/front/tape/run.hirsel"))
        .unwrap()
        .replace(edit.0, edit.1);
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
    options.store = dir.join(store);
    options.output = dir.join(format!("{store}-program"));
    options.directories = vec![dir.join("grant").to_str().unwrap().into()];
    options.grant_mask = (1 << 2) | (1 << 4) | (1 << 9);
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    product.build(&entry, &options).unwrap().executable
}

fn tape_run(program: &Path, argv: &[&str]) -> (Option<i32>, String, String) {
    let output = std::process::Command::new(program)
        .args(argv)
        .output()
        .unwrap();
    (
        output.status.code(),
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

/// REPLAY-SPEC v2.3 §9 end to end, the oracle being the recording run's own
/// output: a replay prints the recorded clock reading and argument vector, and
/// nothing through the substitute Stdout. Wrong implementations caught: a
/// replayed substitute that reaches the host (a second "taped", a fresh clock
/// reading), Args answered from the live vector or not readable after end
/// ("|two"), a Replay that writes the tape, END's outcome not compared (exit 5);
/// a revision that changes with the build (a rebuild with the other engine
/// refuses, exit 3); a changed body that replays without Across, an
/// Across that refuses it or does not say so ("across:"), or says so of its own
/// revision; an Across replay whose changed outcome is not Diverged(Outcome).
fn tape_run_replays(backend: Backend, other: Backend) {
    let dir = tempfile::tempdir().unwrap();
    let grant = dir.path().join("grant");
    std::fs::create_dir(&grant).unwrap();
    let program = tape_run_build(backend, dir.path(), "a", ("", ""));
    let (code, recorded, err) = tape_run(&program, &["one"]);
    assert_eq!(code, Some(0), "record: {err}");
    let reading = recorded
        .strip_prefix("taped")
        .and_then(|s| s.strip_suffix("|one"))
        .unwrap_or_else(|| panic!("record printed {recorded:?}"));
    assert!(reading.parse::<u64>().is_ok(), "{recorded:?}");
    let tape = std::fs::read(grant.join("run.tape")).unwrap();
    let (code, replayed, err) = tape_run(&program, &["two"]);
    assert_eq!(code, Some(0), "replay: {err}");
    assert_eq!(
        replayed,
        format!("{reading}|one"),
        "replay answers from the tape"
    );
    assert_eq!(std::fs::read(grant.join("run.tape")).unwrap(), tape);
    let (code, _, err) = tape_run(&program, &["x"]);
    assert_eq!(
        code,
        Some(5),
        "a changed outcome is Diverged(Outcome): {err}"
    );
    let (code, forced, err) = tape_run(&program, &["force"]);
    assert_eq!(
        (code, forced),
        (Some(0), replayed.clone()),
        "Across of its own revision: {err}"
    );
    let switched = tape_run_build(other, dir.path(), "e", ("", ""));
    let (code, again, err) = tape_run(&switched, &["three"]);
    assert_eq!(
        (code, again),
        (Some(0), replayed),
        "the other engine: {err}"
    );
    let changed = tape_run_build(backend, dir.path(), "c", ("\"|\"", "\"/\""));
    let (code, printed, err) = tape_run(&changed, &["one"]);
    assert_eq!(
        (code, printed),
        (Some(3), String::new()),
        "Refused(Revision): {err}"
    );
    let (code, printed, err) = tape_run(&changed, &["force"]);
    assert_eq!(
        (code, printed),
        (Some(0), format!("across:{reading}/one")),
        "Across opens it, labelled: {err}"
    );
    let behaves = tape_run_build(
        backend,
        dir.path(),
        "d",
        ("{ 1u8 } else { 0u8 }", "{ 1u8 } else { 6u8 }"),
    );
    let (code, _, err) = tape_run(&behaves, &["force"]);
    assert_eq!(
        code,
        Some(5),
        "a changed outcome under Across is Diverged(Outcome): {err}"
    );
}

#[test]
#[ignore = "explicit native integration target"]
fn tape_run_replays_cranelift() {
    tape_run_replays(Backend::Cranelift, Backend::Llvm);
    tape_schedule_engines();
    tape_cli_replays();
}

#[test]
#[ignore = "explicit native integration target"]
fn tape_run_replays_llvm() {
    tape_run_replays(Backend::Llvm, Backend::Cranelift);
}

/// A substitute used after `tape.end` fails with cause `Tape`, printed from
/// the runtime's verdict (D-B4), in Record and in Replay. Wrong
/// implementations caught: a substitute that keeps reaching the host after
/// end (exit 0), a bare `Tape` cause with no verdict, a refusal with the
/// wrong cause kind.
fn tape_run_after_end(backend: Backend) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("grant")).unwrap();
    let program = tape_run_build(backend, dir.path(), "a", ("", ""));
    for argument in ["after", "aftertasks"] {
        for run in ["record", "replay"] {
            let (code, out, err) = tape_run(&program, &[argument]);
            assert_ne!(code, Some(0), "{run}: {out:?}");
            assert!(err.starts_with("Tape(ended) at "), "{run}: {err:?}");
        }
    }
}

#[test]
#[ignore = "explicit native integration target"]
fn tape_run_after_end_cranelift() {
    tape_run_after_end(Backend::Cranelift);
}

#[test]
#[ignore = "explicit native integration target"]
fn tape_run_after_end_llvm() {
    tape_run_after_end(Backend::Llvm);
}

/// Both emitters record identical scheduling coordinates and portable outcomes.
/// The fixed inner provider drives W and I without host timing.
fn tape_schedule_engines() {
    use std::io::Write;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let grant = dir.path().join("grant");
    std::fs::create_dir(&grant).unwrap();
    let cc = PathBuf::from(std::env::var_os("LLVM_SYS_231_PREFIX").unwrap()).join("bin/clang");
    let source = std::fs::read_to_string(root.join("core/front/tape/schedule.hirsel")).unwrap();
    let c = dir.path().join("provider.c");
    std::fs::write(&c, include_str!("tape_schedule_provider.c")).unwrap();
    let linker = dir.path().join("cc");
    let quote = |p: &Path| format!("'{}'", p.to_str().unwrap().replace('\'', "'\\''"));
    let mut script = Vec::new();
    writeln!(
        script,
        "#!/bin/sh\nexec {} \"$@\" {} -I {} -Wl,--wrap=hirsel_core_os_run_with_sources",
        quote(&cc),
        quote(&c),
        quote(&root.join("crates/hirsel-core-rt/include"))
    )
    .unwrap();
    crate::executable::write(&linker, &script, 0o755).unwrap();
    let mut recorded = None;
    for backend in [Backend::Cranelift, Backend::Llvm] {
        let mut request = source_request(&source, dir.path());
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
        let mut options = native_options(backend, dir.path());
        options.output = dir.path().join(format!("schedule-{backend:?}"));
        options.store = dir.path().join(format!("store-{backend:?}"));
        options.directories = vec![grant.to_str().unwrap().into()];
        options.grant_mask = (1 << 2) | (1 << 9);
        options.link = Link::Driver(linker.clone());
        options.runtime = root.join(
            std::env::var_os("HIRSEL_CORE_RT_SCRIPTED_ARCHIVE").expect("scripted runtime archive"),
        );
        let built = product
            .build(&DefPath::from_full("app@1/main/main").unwrap(), &options)
            .unwrap();
        let (code, out, err) = tape_run(&built.executable, &[]);
        assert_eq!(code, Some(0), "{backend:?} record: {out} {err}");
        let bytes = std::fs::read(grant.join("schedule.tape")).unwrap();
        let (code, out, err) = tape_run(&built.executable, &[]);
        assert_eq!(code, Some(0), "{backend:?} replay: {out} {err}");
        if let Some(recorded) = &recorded {
            assert_eq!(&bytes, recorded, "engine recordings, including block 0");
        }
        recorded = Some(bytes);
        std::fs::remove_file(grant.join("schedule.tape")).unwrap();
    }
}

/// S6's JSON/Args/Tasks run replays after its input grant is removed; diagnose
/// names the first divergent request's rebound source, and show is standalone.
fn tape_cli_replays() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let app = dir.path().join("app");
    let data = dir.path().join("data");
    std::fs::create_dir(&app).unwrap();
    std::fs::create_dir(&data).unwrap();
    let source = std::fs::read_to_string(root.join("core/front/tape/cli.hirsel")).unwrap();
    std::fs::write(app.join("main.hirsel"), &source).unwrap();
    std::fs::write(data.join("input.json"), b"{\"answer\":42}\n").unwrap();
    let tape = dir.path().join("run.tape");
    let options = native_options(Backend::Cranelift, dir.path());
    let runtime = options.runtime.canonicalize().unwrap();
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
    assert!(out.contains("format 4") && out.contains("GRANT") && out.contains("END"));
    assert!(!out.contains("site"));
    // Random already exists in the provider ABI. Its payload must also replay,
    // including DATA continuation, rather than leave the carrier empty.
    let random = "pub fn main(r: Random, out: Stdout) -> Result[(), Exit] { let mut b: Bytes = result.unwrap(list.with_capacity(70000)); host.random(r, mut b); if list.len(b) != 70000u64 { return Error(Exit(status: 6)) }; result.unwrap(stdout.write_bytes(out, b)); Ok(()) }";
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
    assert_eq!(code, Some(103), "{err}");
    assert!(err.contains("Refused(Revision)"), "{err}");
    let mut across = replay;
    across.push("--diagnose".into());
    let (code, _, err) = invoke("replay", across);
    assert_eq!(code, Some(101), "{err}");
    assert!(
        err.contains("revision differs") && err.contains("Arg") && err.contains("file.hirsel:"),
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
    options.grant_mask = (1 << grant_kind("Process").unwrap()) | (1 << grant_kind("Stdout").unwrap());
    options.tape = Some(crate::RunTape {
        path: tape.to_str().unwrap().into(),
        mode: crate::RunTapeMode::Record,
    });
    let built = product.build(&entry, &options).unwrap();
    let mut child = Command::new(&built.executable)
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
    assert_eq!(unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGINT) }, 0);
    line.clear();
    output.read_line(&mut line).unwrap();
    assert_eq!(line, "stopped\n");
    let record = child.wait_with_output().unwrap();
    assert!(record.status.success(), "{:?}", record.stderr);
    let sealed = std::fs::read(&tape).unwrap();

    options.backend = Backend::Llvm;
    options.grant_mask = 0;
    options.tape.as_mut().unwrap().mode = crate::RunTapeMode::Replay;
    let replay = product.build(&entry, &options).unwrap();
    assert_eq!(replay.revision, built.revision);
    let result = Command::new(&replay.executable).output().unwrap();
    assert_eq!(result.status.code(), Some(0), "{:?}", result.stderr);
    assert!(result.stdout.is_empty(), "replay wrote through the live Stdout");
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
    let fixture = std::fs::read_to_string(root.join("core/front/tape/http.hirsel")).unwrap();
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
        std::fs::write(&main, &fixture).unwrap();
        let tape = dir.path().join(format!("{engine}.tape"));
        let common = [
            app.display().to_string(),
            "--backend".to_owned(),
            engine.to_owned(),
            "--runtime".to_owned(),
            native.runtime.canonicalize().unwrap().display().to_string(),
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
        assert_eq!(code, Some(102), "{engine}: {error}");
        assert!(error.contains("Diverged(Arg)"), "{engine}: {error}");
    }
}

/// TL9 checks the server's HTTP bytes, RFC 7301's selected index and the
/// OpenSSL digest of the fixture leaf. Replay runs after removing both live
/// inputs, with a counting provider in place of the CLI's refuse-all provider.
#[test]
fn cli_replays_tls_offline() {
    cli_tls_replay(false);
}

/// TLS-5 and TLS-6, TL10–TL12. The permitted peer receives the exact
/// synthetic grant bytes. Plain TCP and the wrong verified name receive zero.
#[test]
fn cli_replays_tls_without_secret_sources() {
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
        [b"before:", token.as_bytes(), b":after\n", &file_bytes, b":after\nsealed:after\n"].concat()
    } else {
        REQUEST.to_vec()
    };
    let response = if with_secret { "accepted\n" } else { RESPONSE };
    let peer = std::thread::spawn(move || {
        if with_secret {
            let (mut plain, _) = listener.accept().unwrap();
            let mut refused = Vec::new();
            plain.read_to_end(&mut refused).unwrap();
            assert!(refused.is_empty(), "plaintext reached plain TCP");
            let (socket, _) = listener.accept().unwrap();
            let mut beta = StreamOwned::new(
                ServerConnection::new(server(BETA, BETA_KEY, false)).unwrap(), socket);
            beta.read_to_end(&mut refused).unwrap();
            assert!(refused.is_empty(), "plaintext reached beta.test");
        }
        let (socket, _) = listener.accept().unwrap();
        let mut stream = StreamOwned::new(
            ServerConnection::new(server(LEAF, KEY, !with_secret)).unwrap(), socket);
        let mut request = vec![0; expected_request.len()];
        stream.read_exact(&mut request).unwrap();
        assert!(request == expected_request, "wrong plaintext reached the permitted peer");
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
        include_str!("../../../core/front/tape/tls-secret.hirsel")
    } else {
        include_str!("../../../core/front/tape/tls.hirsel")
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
        native.runtime.canonicalize().unwrap().display().to_string(),
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
        grants.extend(["--grant", "Secret:token=env:HIRSEL_TL10_TOKEN to alpha.test", "--grant", &from_file]);
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
        format!("[secret][secret]{response}")
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

    if with_secret {
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
        assert!(export.contains("grant `token` [secret] to alpha.test"));
        assert!(export.contains("grant `local` [secret] to alpha.test"));
        for bytes in [raw.as_slice(), token.as_bytes(), file_bytes.as_slice()] {
            for form in [
                bytes,
                crate::call::hex(bytes).as_bytes(),
                crate::call::base64(bytes).as_bytes(),
            ] {
                for output in [
                    sealed.as_slice(), &record.stdout, &record.stderr, &replay.stdout, &replay.stderr,
                    export.as_bytes(),
                ] {
                    assert!(!output.windows(form.len()).any(|word| word == form),
                        "a Secret representation escaped");
                }
            }
        }
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
    options.optimize = true;
    options.grant_mask = 1 << grant_kind("Stdout").unwrap();
    let mut run = |mode, cap: &str| {
        options.tape = Some(crate::RunTape { path: tape.to_str().unwrap().into(), mode });
        let built = product.build(&entry, &options).unwrap();
        assert_eq!(built.revision.0, code_revision_of(&built.executable));
        let output = Command::new(&built.executable)
            .env("HIRSEL_ISA_CAP", cap)
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        (built.revision, output.stdout)
    };
    let levels = hirsel_core_layout::isa::X86_64_LEVELS;
    let (revision, recorded) = run(crate::RunTapeMode::Record, &levels.last().unwrap().number.to_string());
    assert_eq!(recorded, b"simd");
    let bytes = std::fs::read(&tape).unwrap();
    let (replay_revision, replayed) = run(crate::RunTapeMode::Replay, &levels[0].number.to_string());
    assert_eq!(revision, replay_revision);
    assert!(replayed.is_empty());
    assert_eq!(std::fs::read(&tape).unwrap(), bytes);
}
