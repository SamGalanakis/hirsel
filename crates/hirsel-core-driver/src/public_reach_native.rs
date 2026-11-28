/// NET-18: a `public` grant row survives record and replay. The recorded
/// run's private connect is refused by the preflight with no issue, so a
/// replay that read the row back as `Any` would issue a connect the tape
/// does not hold and diverge.
#[test]
fn cli_replays_public_reach() {
    const PROGRAM: &str = r#"pub fn main(n: Net) -> Result[(), Exit] waits {
    match host.connect(n, Addr(ip: V4(167772161u32), port: 5432), Never) {
        Error(Denied) => {},
        Error(_) => return Error(Exit(status: 31)),
        Ok(s) => {
            let _ = take s
            return Error(Exit(status: 32))
        }
    }
    let d = net.denials(n)
    if d.not_public != 1 || d.own_address != 0 {
        return Error(Exit(status: 33))
    }
    Ok(())
}
"#;
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let rule = "Connect 0.0.0.0/0 1-65535 public";
    for (backend, name) in [(Backend::Cranelift, "cranelift"), (Backend::Llvm, "llvm")] {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("app");
        std::fs::create_dir(&app).unwrap();
        std::fs::write(app.join("main.hirsel"), PROGRAM).unwrap();
        let native = native_options(backend, dir.path());
        let common = [
            app.display().to_string(),
            "--backend".into(),
            name.into(),
            "--runtime".into(),
            env_record_runtime().display().to_string(),
            "--cc".into(),
            native.link.driver().display().to_string(),
            "--libraries".into(),
            root.join("core").display().to_string(),
            "--source-core".into(),
            "--source-std".into(),
            "--store".into(),
            dir.path().join("cache").display().to_string(),
        ];
        let invoke = |command: &str, args: &[&str]| {
            let mut call = command.split_ascii_whitespace().map(str::to_owned).collect::<Vec<_>>();
            call.extend(common.iter().cloned());
            call.extend(args.iter().map(|&arg| arg.to_owned()));
            process::tape_request_command(&call, dir.path())
        };
        let tape = dir.path().join("public.tape");
        let path = tape.to_str().unwrap();
        let record = invoke(
            "run",
            &["--grant", &format!("Net={rule}"), "--record", path],
        )
        .output()
        .unwrap();
        let record_log = String::from_utf8_lossy(&record.stderr);
        assert_eq!(record.status.code(), Some(0), "{backend:?}: {record_log}");

        let recorded = std::fs::read(&tape).unwrap();
        let mut bytes = recorded.as_slice();
        let (_, header) =
            hirsel_tape_format::Blocks::open(|buf| std::io::Read::read(&mut bytes, buf)).unwrap();
        let parsed = hirsel_core_net::GrantRule::parse(rule).unwrap();
        assert_eq!(
            header.net_rules,
            [parsed],
            "public_reach_survives_grant_snapshot"
        );

        let replay = invoke("tape replay", &["--tape", path]).output().unwrap();
        let replay_log = String::from_utf8_lossy(&replay.stderr);
        assert_eq!(
            replay.status.code(),
            Some(0),
            "{backend:?}: public_reach_replays_refused_connect: {replay_log}"
        );
    }
}
