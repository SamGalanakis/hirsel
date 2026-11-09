fn free_native_host_stop(width: usize) {
    let _compiler = QUAL_COMPILER.lock().unwrap_or_else(|e| e.into_inner());
    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path();
    let source = format!(
        r#"
pub fn main(rt: Io) -> Result[(), GrantError] {{
  let tasks = io.tasks(rt)?
  let batch = core@1/cpu.map_scoped(tasks, {width}u32,
    fn() -> List[u64] uses {{}} {{ [0u64, 1u64] }},
    fn(take job: u64) -> u64 uses {{}} {{
      let mut x = job
      while true {{ x = int.wrapping_add(x, 1u64) }}
      x
    }})
  failure.check(false, Bounds())
  Ok(())
}}
"#
    );
    let manifest = qual_manifest(root, &source);
    let authority = qual_grants(width);
    let grants = root.join("grants.json");
    fs::write(&grants, authority.to_string()).unwrap();
    for backend in ["llvm", "cranelift"] {
        let executable = root.join(backend);
        let mut build = cpu_compiler();
        build
            .args(["build", "--validate-stages", "--backend", backend])
            .arg(&manifest)
            .arg("-o")
            .arg(&executable);
        checked(build.output().unwrap());
        let tape = root.join("stop.tape");
        let observation = root.join("stop.observation");
        let projection = root.join("stop.projection");
        let mut command = Command::new(&executable);
        command
            .arg("--hirsel-grants")
            .arg(&grants)
            .arg("--hirsel-record")
            .arg(&tape)
            .env("HIRSEL_RT_OBSERVATION", &observation)
            .env(hirsel_rt::free_replay::Report::VAR, &projection);
        command.env("HIRSEL_RT_STOP_AFTER_MS", "200");
        let live = qual_native_output(&mut command, root);
        let expected = hirsel_run_schema::exit::ENVIRONMENTAL;
        assert_eq!(live.status.code(), Some(expected), "{backend}: {live:?}");
        let parsed = hirsel_free::tape::parse(&fs::read(&tape).unwrap()).unwrap();
        assert!(!parsed.torn && parsed.end.is_some());
        assert!(
            parsed.cut.keys().any(|path| !path.is_empty()),
            "mapper child must be admitted"
        );
        let end = parsed.end.as_ref().unwrap();
        assert!(
            fs::read_to_string(&observation)
                .unwrap()
                .contains("stopped")
        );
        let observed = qual_observation(&observation);
        let projected = fs::read(&projection).unwrap();
        let probe = root.join("replay.probe");
        let mut replay = Command::new(&executable);
        replay
            .arg("--hirsel-replay")
            .arg(&tape)
            .env("HIRSEL_RT_WORKERS", "1")
            .env("HIRSEL_RT_CPU_PROBE", &probe)
            .env("HIRSEL_RT_OBSERVATION", &observation)
            .env(hirsel_rt::free_replay::Report::VAR, &projection);
        let replay = qual_native_output(&mut replay, root);
        assert_eq!(replay.status.code(), live.status.code(), "{replay:?}");
        assert_eq!(replay.stdout, live.stdout);
        assert_eq!(replay.stderr, live.stderr);
        assert_eq!(qual_observation(&observation), observed);
        assert_eq!(fs::read(&projection).unwrap(), projected);
        assert!(fs::read(&probe).unwrap().is_empty());
        eprintln!("Free native {backend} W{width}: {end:?}, exact one-permit replay");
    }
}
