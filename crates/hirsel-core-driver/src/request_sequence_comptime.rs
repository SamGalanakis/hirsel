fn constant_call_unrelated_body_sequence() -> Seq {
    let request = |other| {
        build(app(&[
            ("a.hirsel", "a", "fn f()->u8{1u8}\npub const X:u8=f()"),
            ("b.hirsel", "b", other),
            (
                "main.hirsel",
                "main",
                "pub fn main()->Result[(),Exit]{Error(Exit(status:b.read()))}",
            ),
        ]))
    };
    seq(
        "constant_call_unrelated_body",
        vec![
            request("pub fn read()->u8{a.X}\nfn other()->u8{1u8}"),
            request("pub fn read()->u8{a.X}\nfn other()->u8{2u8}"),
        ],
    )
}

#[test]
fn constant_call_unrelated_body_is_fast_without_evaluations() {
    let seq = constant_call_unrelated_body_sequence();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let runtime = root.join("runtime");
    let inputs = root.join("inputs.json");
    let metrics = root.join("metrics.json");
    let output = root.join("program");
    let options = native_options(Backend::Cranelift, root);
    let Link::Driver(cc) = options.link else {
        unreachable!()
    };
    let mut args = vec!["build".into(), "--json".into(), "--source-core".into()];
    for (flag, path) in [
        ("--inputs", inputs.clone()),
        ("--store", root.join("store")),
        ("--runtime", options.runtime.canonicalize().unwrap()),
        ("--cc", cc.canonicalize().unwrap()),
        ("--metrics", metrics.clone()),
        ("-o", output.clone()),
    ] {
        args.extend([
            flag.to_owned(),
            path.into_os_string().into_string().unwrap(),
        ]);
    }
    args.extend(["--entry".into(), seq.steps[0].entry.unwrap().into()]);
    let shared = |command: &mut Command| {
        command
            .env("XDG_RUNTIME_DIR", &runtime)
            .env("XDG_CACHE_HOME", root.join("cache"))
            .env("HIRSEL_TRACE_REQUEST_PHASES", "1");
    };
    let server = start_resident(&runtime, root, shared);
    let mut written = BTreeSet::new();
    let rows = [
        (&seq.steps[0], false),
        (&seq.steps[0], true),
        (&seq.steps[1], false),
    ]
    .map(|(req, pad)| {
        let manifest = materialize(req, pad, &root.join("src"), &mut written);
        std::fs::write(&inputs, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let _ = std::fs::remove_file(&metrics);
        let mut command = process::tape_request_command(&args, root);
        shared(&mut command);
        let answer = process::tape_request_output(command);
        (answer, std::fs::read(&metrics))
    });
    stop_resident(&runtime, root, server);

    let measurements = rows.map(|((status, stdout, stderr), metrics)| {
        assert_eq!(status, Some(0), "{stdout}\n{stderr}");
        assert!(stderr.contains("hirsel-request-phases"), "{stderr}");
        serde_json::from_slice::<serde_json::Value>(&metrics.unwrap()).unwrap()
    });
    assert_eq!(measurements[0]["counters"]["constant_runs"], 1);
    assert_eq!(measurements[2]["layer_class"], "fast");
    assert_eq!(measurements[2]["counters"]["constant_runs"], 0);
    assert_eq!(Command::new(output).status().unwrap().code(), Some(1));
}
