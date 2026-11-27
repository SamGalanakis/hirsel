/// A lost launcher leaves cgroups as its process inventory. A direct child
/// exiting normally still owns its descendants until the launcher reaps them.
// The test uses systemd delegation and the cgroup v2 process inventory.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires a delegated cgroup v2 scope"]
fn cgroup_lifecycle() {
    const IN_SCOPE: &str = "HIRSEL_LIFECYCLE_LAW_SCOPE";
    const LOSE_LAUNCHER: &str = "HIRSEL_LIFECYCLE_LAW_LOST";
    const LAW: &str = "launcher::laws::cgroup_lifecycle";
    let exe = std::env::current_exe().unwrap();
    let args = [LAW, "--exact", "--ignored", "--nocapture"];
    if std::env::var_os(IN_SCOPE).is_none() {
        let status = std::process::Command::new("systemd-run")
            .args(["--user", "--scope", "--quiet", "-p", "Delegate=yes", "-E"])
            .arg(format!("{IN_SCOPE}=1"))
            .arg("--")
            .arg(exe)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "delegated launcher law: {status}");
        return;
    }

    fn launcher(state: &Path, work: &Path) -> Launcher<Scripts> {
        Launcher::new(state, Managed, || {
            Ok(images(
                work,
                &[
                    "sleep 30 & printf '%s' \"$!\" > orphan; wait",
                    &format!(
                        "{}\ntest \"$LISTEN_PID\" -eq \"$$\" || exit 24\nsleep 30 & printf '%s' \"$!\" > orphan; exit 0\n",
                        crate::host::SESSION_LEADER
                    ),
                ],
            ))
        })
        .unwrap()
    }

    if let Some(work) = std::env::var_os(LOSE_LAUNCHER) {
        let work = PathBuf::from(work);
        let mut first = launcher(&work.join("state"), &work);
        assert!(first.has_cgroup_root(), "scope did not delegate cgroup v2");
        first.handle(Request::Bind(group(), "127.0.0.1:0".into()));
        first.handle(launch(0, &work, Mode::Serving(1, serve(1, None))));
        assert_eq!(
            first.wait_start_reply(&serving(1)),
            Event::Launched(serving(1), Phase::Started)
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while !fs::read_to_string(work.join("orphan")).is_ok_and(|s| !s.is_empty()) {
            assert!(Instant::now() < deadline);
            first.turn(Duration::from_millis(20)).unwrap();
        }
        // Model losing the launcher without running its Drop.
        std::process::exit(0);
    }

    let dir = tempfile::tempdir().unwrap();
    let work = dir.path();
    let state = work.join("state");
    fs::create_dir(&state).unwrap();
    let status = std::process::Command::new(exe)
        .args(args)
        .env(LOSE_LAUNCHER, work)
        .status()
        .unwrap();
    assert!(status.success(), "predecessor launcher: {status}");
    let root = super::cgroup::delegated(&state).unwrap();

    let directories = || -> BTreeSet<String> {
        fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap())
            .filter(|entry| entry.file_type().unwrap().is_dir())
            .map(|entry| entry.file_name().into_string().unwrap())
            .collect()
    };
    assert_eq!(directories(), BTreeSet::from(["c-67-1-1-serving".into()]));
    assert!(
        fs::read_to_string(root.join("c-67-1-1-serving/cgroup.events"))
            .unwrap()
            .lines()
            .any(|line| line == "populated 1"),
        "the predecessor left no live descendant"
    );

    let work = work.to_owned();
    let launcher = launcher(&state, &work);
    let mut rig = Rig { dir, launcher };
    let address = rig.bind();
    let (children, _) = rig.inventory();
    assert!(children.is_empty());
    assert!(directories().is_empty());

    // Start a new live child after recovery. Hello and cgroupfs describe the
    // same live inventory.
    assert!(
        rig.ask(launch(0, &work, Mode::Serving(2, serve(2, None))))
            .contains(&Event::Launched(serving(2), Phase::Started))
    );
    let (children, _) = rig.inventory();
    assert_eq!(
        children,
        [ChildState {
            child: serving(2),
            phase: Phase::Started,
            selection: Some(Selection::new(2, vec![0; 32])),
            answer: None,
        }]
    );
    assert_eq!(directories(), BTreeSet::from(["c-67-1-2-serving".into()]));
    rig.ask(Request::Kill(serving(2)));
    rig.until(|events| ended(events, &serving(2)));
    fs::remove_file(rig.work().join("orphan")).unwrap();

    // A natural leader exit must kill and reap the left-behind grandchild.
    let work = rig.work();
    rig.ask(launch(1, &work, Mode::Serving(3, serve(3, None))));
    let queue = TcpStream::connect(address).unwrap();
    let events = rig.until(|events| ended(events, &serving(3)));
    assert!(
        events.iter().any(|event| matches!(
            event,
            Event::Ended(child, End { status: Status::Code(0), readable: true, .. })
                if *child == serving(3)
        )),
        "{events:?}"
    );
    drop(queue);
    let pid = fs::read_to_string(work.join("orphan")).unwrap();
    assert!(
        crate::host::zombie(pid.trim().parse().unwrap()).is_none(),
        "grandchild {pid} still exists"
    );
    assert!(directories().is_empty());
}
