/// `child_mode_authority`, launcher half (launch.md "Child modes"): the serving child holds
/// the listener on 3, its endpoint on 4 and its report on 5; no other mode holds a
/// descriptor past 2; no child holds the control socket; a prepare child is pointed at its
/// export directory in the group's scratch; describe and preview answer on standard output.
#[test]
fn child_mode_authority() {
    // Darwin can report inode zero for both a TCP socket and the first Unix
    // socket it stats. Check the listener's address and the endpoint's peer.
    let serving_sockets = r#"
python3 -c '
import socket
listener = socket.socket(fileno=3)
host, port = listener.getsockname()
probe, _ = listener.accept()
probe.close()
print(f"listener {host}:{port}")
endpoint = socket.socket(fileno=4)
assert endpoint.family == socket.AF_UNIX
endpoint.getpeername()
print("connected Unix endpoint")
' >> "$out" 2>&1
"#;
    // Each stand-in writes its inherited descriptors 3-9 with what they are, then its
    // arguments, to fds-<name>. The serving child also reports its socket roles.
    let report = |name: &str| {
        let sockets = if name == "serving" {
            serving_sockets
        } else {
            ""
        };
        format!(
            "out=\"$PWD/fds-{name}\"; : > \"$out\"\nfor n in 3 4 5 6 7 8 9; do {}; done\n{sockets}\necho \"args $*\" >> \"$out\"",
            crate::host::FD_REPORT
        )
    };
    let scripts = [
        format!("{}\n{SLEEP}", report("serving")),
        format!("{}\nprintf described", report("describe")),
        format!("{}\ncat", report("preview")),
        report("prepare"),
        report("install"),
    ];
    let scripts: Vec<&str> = scripts.iter().map(String::as_str).collect();
    let mut rig = rig(&scripts);
    let work = rig.work();
    let address = rig.bind();
    rig.launcher
        .listen(UnixListener::bind(work.join("control")).unwrap())
        .unwrap();
    let control = crate::host::fd_report(rig.launcher.control_fd().unwrap());
    let held = |name: &str| -> Vec<String> {
        fs::read_to_string(work.join(format!("fds-{name}")))
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    };

    let answer_before_end = |events: &[Event], child: Child, answer: Vec<u8>| {
        let end = events
            .iter()
            .position(|event| matches!(event, Event::Ended(id, _) if id == &child))
            .unwrap();
        assert!(
            events[..end].contains(&Event::Answered(child, answer)),
            "{events:?}"
        );
    };
    let describing = role(Role::Describe(attempt(1)));
    rig.ask(launch(1, &work, Mode::Describe(attempt(1))));
    let events = rig.until(|e| ended(e, &describing));
    answer_before_end(&events, describing, b"described".to_vec());
    let previewing = role(Role::Preview(attempt(2)));
    let old = b"old entries".to_vec();
    rig.ask(launch(2, &work, Mode::Preview(attempt(2), old.clone())));
    let events = rig.until(|e| ended(e, &previewing));
    answer_before_end(&events, previewing, old);
    let preparing = role(Role::Prepare(attempt(3)));
    rig.ask(launch(
        3,
        &work,
        Mode::Prepare(attempt(3), "export-1".into(), vec![0xf0]),
    ));
    rig.until(|e| ended(e, &preparing));
    for name in ["describe", "preview", "prepare"] {
        let lines = held(name);
        assert!(
            lines.len() == 1 && lines[0].starts_with("args "),
            "{name}: {lines:?}"
        );
    }
    let export = rig.launcher.scratch_of(&group()).join("export-1");
    let attempt_hex = "03".to_owned() + &"00".repeat(7) + "03" + &"00".repeat(7);
    let image_hex = "03".repeat(32);
    assert!(held("prepare")[0].ends_with(&format!(
        "--hirsel-prepare {} {attempt_hex} {image_hex} f0",
        export.display()
    )));

    let install = Mode::Install(1, InstallKind::Genesis, Vec::new());
    rig.ask(launch(4, &work, install));
    rig.until(|events| ended(events, &role(Role::Install(1))));
    assert_eq!(
        held("install"),
        ["args --hirsel-n-place=65536 --hirsel-install-child=0 app"]
    );

    let _probe = TcpStream::connect(address).unwrap();
    rig.ask(launch(0, &work, Mode::Serving(1, serve(5, None))));
    let end = Instant::now() + Duration::from_secs(10);
    while !fs::read_to_string(work.join("fds-serving")).is_ok_and(|s| s.contains("args")) {
        assert!(Instant::now() < end);
        rig.launcher.turn(Duration::from_millis(20)).unwrap();
    }
    let lines = held("serving");
    assert_eq!(lines.len(), 6, "{lines:?}");
    assert!(lines[0].starts_with("3 socket:[") && lines[1].starts_with("4 socket:["));
    assert!(lines[2].starts_with("5 pipe:["));
    assert_eq!(lines[3], format!("listener {address}"), "control {control}");
    assert_eq!(lines[4], "connected Unix endpoint", "control {control}");
    assert!(lines[5].starts_with("args --hirsel-wake --hirsel-idle=250 "));
    assert!(lines[5].ends_with("--hirsel-activation=5 app"));
    rig.ask(Request::Kill(serving(1)));
    rig.until(|e| ended(e, &serving(1)));
}
