//! R27 laws (launch.md "Containment (R27)" and its law list). The contained
//! program is this test binary run as `contained_helper`: it tries each
//! access and prints what happened, and starts a tool descendant (as an
//! `Exec` tool would be) and session-leader daemons that ignore `SIGTERM`. The
//! oracles sit outside the container: every process the helper starts carries
//! a marker in its environment, which the host finds in `/proc`.
//!
//! The opt-in `contain` harness runs these laws on a host with unprivileged
//! user namespaces and a systemd user manager. Each law re-runs itself under
//! `systemd-run --user --scope -p Delegate=yes` and restructures only its own
//! scope.
use super::*;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::unix::net::{UnixListener, UnixStream};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const SCOPE: &str = "HIRSEL_CONTAIN_SCOPE";

/// Re-runs `test` in a fresh delegated scope and returns `None`, or, inside
/// that scope, returns the scope's own cgroup.
fn scope(test: &str) -> Option<PathBuf> {
    if std::env::var_os(SCOPE).is_none() {
        let status = Command::new("systemd-run")
            .args(["--user", "--scope", "--quiet", "-p", "Delegate=yes", "-E"])
            .arg(format!("{SCOPE}=1"))
            .arg("--")
            .arg(std::env::current_exe().unwrap())
            .args([test, "--exact", "--nocapture", "--test-threads=1"])
            .status()
            .expect("contain laws require systemd-run and a delegated user scope");
        assert!(
            status.success(),
            "{test} requires a working delegated systemd user scope and containment: {status}"
        );
        return None;
    }
    let line = fs::read_to_string("/proc/self/cgroup").unwrap();
    let path = line.trim().strip_prefix("0::").unwrap();
    Some(Path::new("/sys/fs/cgroup").join(path.trim_start_matches('/')))
}

/// Moves this process out of the scope's top cgroup and returns an empty
/// sibling for the launcher's containers.
fn children(own: &Path) -> PathBuf {
    let harness = own.join("harness");
    fs::create_dir_all(&harness).unwrap();
    fs::write(harness.join("cgroup.procs"), "0").unwrap();
    fs::write(own.join("cgroup.subtree_control"), "+cpu +memory +pids").unwrap();
    let children = own.join("children");
    fs::create_dir_all(&children).unwrap();
    children
}

fn marked(mark: &str) -> Vec<u32> {
    let needle = format!("HC_MARK={mark}");
    let mut pids = Vec::new();
    for entry in fs::read_dir("/proc").unwrap().flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|s| s.parse().ok()) else {
            continue;
        };
        if let Ok(environ) = fs::read(entry.path().join("environ"))
            && environ
                .split(|b| *b == 0)
                .any(|var| var == needle.as_bytes())
        {
            pids.push(pid);
        }
    }
    pids
}

fn loader_paths() -> Vec<PathBuf> {
    ["/lib", "/lib64", "/usr/lib", "/usr/lib64"]
        .into_iter()
        .map(PathBuf::from)
        .filter(|path| path.exists())
        .collect()
}

fn helper_args() -> Vec<OsString> {
    [
        "contain::contained_helper",
        "--exact",
        "--nocapture",
        "--test-threads=1",
        "-q",
    ]
    .into_iter()
    .map(OsString::from)
    .collect()
}

struct Group {
    tmp: tempfile::TempDir,
    workdir: PathBuf,
    bindings: Bindings,
    export: PathBuf,
    outside: TcpListener,
    control: PathBuf,
    _control: UnixListener,
    listener: TcpListener,
}

impl Group {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let workdir = tmp.path().join("group");
        let store = workdir.join("store");
        let data = tmp.path().join("data");
        let export = tmp.path().join("export");
        for dir in [&store, &data, &export] {
            fs::create_dir_all(dir).unwrap();
            fs::write(dir.join("f"), "x").unwrap();
        }
        fs::write(workdir.join("notes"), "x").unwrap();
        fs::write(tmp.path().join("secret"), "x").unwrap();
        let control = tmp.path().join("control.sock");
        Self {
            _control: UnixListener::bind(&control).unwrap(),
            control,
            bindings: Bindings {
                store: PathBuf::from("store"),
                dirs: vec![data],
                net: Vec::new(),
            },
            workdir,
            export,
            outside: TcpListener::bind("127.0.0.1:0").unwrap(),
            listener: TcpListener::bind("127.0.0.1:0").unwrap(),
            tmp,
        }
    }

    fn files(&self) -> BTreeMap<&'static str, PathBuf> {
        BTreeMap::from([
            ("secret", self.tmp.path().join("secret")),
            ("notes", self.workdir.join("notes")),
            ("store", self.workdir.join("store")),
            ("data", self.bindings.dirs[0].clone()),
            ("export", self.export.clone()),
        ])
    }

    fn env(&self, mark: &str, hold: bool) -> Vec<(OsString, OsString)> {
        let files = self.files();
        let join = |paths: Vec<PathBuf>| {
            let mut text = OsString::new();
            for (i, path) in paths.iter().enumerate() {
                if i > 0 {
                    text.push(":");
                }
                text.push(path);
            }
            text
        };
        let reads = files
            .values()
            .map(|path| {
                if path.is_dir() {
                    path.join("f")
                } else {
                    path.clone()
                }
            })
            .collect();
        vec![
            ("HC_ROLE".into(), "main".into()),
            ("HC_MARK".into(), mark.into()),
            ("HC_READ".into(), join(reads)),
            (
                "HC_WRITE".into(),
                join(vec![
                    files["store"].clone(),
                    files["data"].clone(),
                    files["export"].clone(),
                ]),
            ),
            (
                "HC_CONNECT".into(),
                self.outside.local_addr().unwrap().to_string().into(),
            ),
            ("HC_UNIX".into(), self.control.clone().into()),
            ("HC_HOLD".into(), if hold { "1" } else { "0" }.into()),
        ]
    }

    fn launch(
        &self,
        host: &Host,
        mode: Mode,
        mark: &str,
        hold: bool,
        peer: Option<SocketAddr>,
    ) -> (Contained, BufReader<File>) {
        let export = (mode == Mode::Prepare).then_some(self.export.as_path());
        let authority = Authority::of(mode, &self.bindings, export).unwrap();
        let id = ChildId {
            group: "g/1".into(),
            incarnation: 7,
            key: 3,
            role: mode,
        };
        let mut pipe = [0; 2];
        assert_eq!(
            unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC) },
            0
        );
        let (read, write) = unsafe { (File::from_raw_fd(pipe[0]), OwnedFd::from_raw_fd(pipe[1])) };
        let mut fds = vec![
            (0, OwnedFd::from(File::open("/dev/null").unwrap())),
            (1, write.try_clone().unwrap()),
            (2, write),
        ];
        if mode == Mode::Serving {
            fds.push((3, OwnedFd::from(self.listener.try_clone().unwrap())));
        }
        let mut env = self.env(mark, hold);
        if let Some(peer) = peer {
            env.push(("HC_ACCEPT".into(), peer.to_string().into()));
        }
        let program = std::env::current_exe().unwrap();
        let contained = host
            .launch(Launch {
                id: &id,
                authority: &authority,
                program: &program,
                args: &helper_args(),
                env: &env,
                workdir: &self.workdir,
                read_only: &loader_paths(),
                fds,
                limits: Limits {
                    memory: Some(512 << 20),
                    cpu: None,
                    pids: Some(64),
                },
            })
            .unwrap();
        (contained, BufReader::new(read))
    }
}

/// Reads the helper's report lines until `ready`.
fn report(contained: Contained, out: &mut BufReader<File>) -> (Contained, Vec<String>) {
    let mut lines = Vec::new();
    loop {
        let mut line = String::new();
        if out.read_line(&mut line).unwrap() == 0 {
            panic!(
                "helper ended before ready: {lines:?}: {:?}",
                contained.wait()
            );
        }
        let line = line.trim().to_string();
        if line == "ready" {
            return (contained, lines);
        }
        if line.starts_with("main ") || line.starts_with("tool ") {
            lines.push(line);
        }
    }
}

/// What the mode's row lets a child, or its tool, do (launch.md "Child modes").
fn expected(mode: Mode, group: &Group) -> Vec<String> {
    let reach: &[&str] = match mode {
        Mode::Install | Mode::Serving => &["store", "data"],
        Mode::Describe | Mode::Preview => &[],
        Mode::Prepare => &["export"],
    };
    let files = group.files();
    let verdict = |name: &str| {
        if reach.contains(&name) {
            "ok"
        } else {
            "denied"
        }
    };
    let mut lines = Vec::new();
    for who in ["main", "tool"] {
        for name in files.keys() {
            lines.push(format!("{who} read {name} {}", verdict(name)));
        }
        for name in ["store", "data", "export"] {
            lines.push(format!("{who} write {name} {}", verdict(name)));
        }
        lines.push(format!("{who} connect denied"));
        lines.push(format!("{who} control denied"));
        if who == "main" {
            // The launch gives serving its listener and nothing else above 2.
            let fds = if mode == Mode::Serving { "3" } else { "" };
            lines.push(format!("main fds [{fds}]"));
        }
    }
    if mode == Mode::Serving {
        lines.push("main accept ok".into());
    }
    lines.sort();
    lines
}

fn name_of(path: &Path, group: &Group) -> &'static str {
    let files = group.files();
    files
        .into_iter()
        .find(|(_, p)| path == *p || path.parent() == Some(p.as_path()))
        .map(|(name, _)| name)
        .unwrap()
}

fn normalize(lines: Vec<String>, group: &Group) -> Vec<String> {
    let mut out: Vec<String> = lines
        .into_iter()
        .filter(|line| {
            if line.starts_with(crate::loopback::STRAY_PREFIX) {
                eprintln!("{line}");
                return false;
            }
            true
        })
        .map(|line| {
            let parts: Vec<&str> = line.splitn(4, ' ').collect();
            match parts.as_slice() {
                [who, op @ ("read" | "write"), path, verdict] => {
                    format!("{who} {op} {} {verdict}", name_of(Path::new(path), group))
                }
                _ => line,
            }
        })
        .collect();
    out.sort();
    out
}

fn assert_gone(mark: &str, contained_cgroup: &Path) {
    assert_eq!(
        marked(mark),
        Vec::<u32>::new(),
        "processes left after the child ended"
    );
    assert!(!contained_cgroup.exists(), "container not removed");
}

#[test]
fn contained_all_children() {
    let Some(own) = scope("contain::contained_all_children") else {
        return;
    };
    let root = children(&own);
    let group = Group::new();
    let host = Host::open(&root, &group.tmp.path().join("state"))
        .expect("contain laws require a host that supports containment");
    for mode in Mode::ALL {
        let mark = format!("all-{}", mode.tag());
        let client = (mode == Mode::Serving)
            .then(|| TcpStream::connect(group.listener.local_addr().unwrap()).unwrap());
        let peer = client.as_ref().map(|client| client.local_addr().unwrap());
        let (contained, mut out) = group.launch(&host, mode, &mark, true, peer);
        if let Some(mut stream) = client {
            let mut hi = [0; 2];
            std::io::Read::read_exact(&mut stream, &mut hi).unwrap();
            assert_eq!(&hi, b"hi");
        }
        let (contained, lines) = report(contained, &mut out);
        let lines = normalize(lines, &group);
        assert_eq!(lines, expected(mode, &group), "{mode:?}");
        assert!(marked(&mark).len() >= 4, "helper, tool and two daemons run");
        // A stop ends the child; its init forwards SIGTERM to the program, and
        // the daemons that ignore it end with the container.
        let cgroup = contained.cgroup().to_path_buf();
        contained.signal(libc::SIGTERM).unwrap();
        match contained.wait().unwrap() {
            End::Exited(status) => assert_eq!(status.signal(), Some(libc::SIGTERM), "{mode:?}"),
            end => panic!("{mode:?}: {end:?}"),
        }
        assert_gone(&mark, &cgroup);
    }
}

#[test]
fn child_mode_authority() {
    let Some(own) = scope("contain::child_mode_authority") else {
        return;
    };
    let root = children(&own);
    let group = Group::new();
    let host = Host::open(&root, &group.tmp.path().join("state"))
        .expect("contain laws require a host that supports containment");
    let program = std::env::current_exe().unwrap();
    for mode in Mode::ALL {
        let export = (mode == Mode::Prepare).then_some(group.export.as_path());
        let mut bindings = group.bindings.clone();
        bindings.net = vec!["Connect 0.0.0.0/0 1-65535".into()];
        let authority = Authority::of(mode, &bindings, export).unwrap();
        let id = ChildId {
            group: "g".into(),
            incarnation: 1,
            key: 1,
            role: mode,
        };
        let launch = |fds: Vec<(RawFd, OwnedFd)>, authority: &Authority| {
            let started = host.launch(Launch {
                id: &id,
                authority,
                program: &program,
                args: &helper_args(),
                env: &[],
                workdir: &group.workdir,
                read_only: &loader_paths(),
                fds,
                limits: Limits::default(),
            });
            started.map(|contained| contained.wait().unwrap())
        };
        // launch.md gives slots 3, 4 and 5 to serving. Prepare syncs its export
        // without an endpoint, and the other modes have stdio alone.
        for target in [3, 4, 5, 6] {
            let allowed = mode == Mode::Serving && target != 6;
            let fd = OwnedFd::from(UnixStream::connect(&group.control).unwrap());
            let plain = Authority::of(mode, &group.bindings, export).unwrap();
            match launch(vec![(target, fd)], &plain) {
                Ok(_) => assert!(allowed, "{mode:?} {target}"),
                Err(LaunchError::Refused(Refused::Descriptor(t))) => {
                    assert_eq!(t, target);
                    assert!(!allowed, "{mode:?} {target}");
                }
                Err(error) => panic!("{mode:?} {target}: {error:?}"),
            }
        }
        // Net bindings reach only serving, which this build refuses to contain.
        match launch(Vec::new(), &authority) {
            Err(LaunchError::Refused(Refused::Net)) => assert_eq!(mode, Mode::Serving),
            Ok(_) => assert_ne!(mode, Mode::Serving),
            Err(error) => panic!("{mode:?}: {error:?}"),
        }
    }
    // A prepare child writes only its export directory and holds no listener.
    let (contained, mut out) = group.launch(&host, Mode::Prepare, "authority-prepare", false, None);
    let (contained, lines) = report(contained, &mut out);
    assert_eq!(normalize(lines, &group), expected(Mode::Prepare, &group));
    assert!(matches!(contained.wait().unwrap(), End::Exited(s) if s.success()));
}

#[test]
fn contained_launcher_restart() {
    if let Some(spec) = std::env::var_os("HC_LAUNCHER") {
        // The launcher that the law kills.
        let spec = PathBuf::from(spec);
        let group = Group::new();
        let host = Host::open(&spec.join("root"), &spec.join("state"))
            .expect("contain laws require a host that supports containment");
        let (contained, mut out) = group.launch(&host, Mode::Install, "restart", true, None);
        let (contained, _) = report(contained, &mut out);
        println!("launched {}", contained.cgroup().display());
        std::io::stdout().flush().unwrap();
        std::mem::forget(contained);
        std::mem::forget(group);
        loop {
            std::thread::sleep(Duration::from_secs(60));
        }
    }
    let Some(own) = scope("contain::contained_launcher_restart") else {
        return;
    };
    let root = children(&own);
    let state = tempfile::tempdir().unwrap();
    let spec = state.path().join("spec");
    fs::create_dir_all(&spec).unwrap();
    std::os::unix::fs::symlink(&root, spec.join("root")).unwrap();
    let mut launcher = Command::new(std::env::current_exe().unwrap())
        .args([
            "contain::contained_launcher_restart",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("HC_LAUNCHER", &spec)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut out = BufReader::new(launcher.stdout.take().unwrap());
    let cgroup = loop {
        let mut line = String::new();
        assert_ne!(out.read_line(&mut line).unwrap(), 0, "launcher ended");
        // libtest's "test ... " header shares the line.
        if let Some((_, path)) = line.trim().split_once("launched ") {
            break PathBuf::from(path);
        }
    };
    assert!(marked("restart").len() >= 4);
    launcher.kill().unwrap();
    launcher.wait().unwrap();
    // The lost launcher's container outlives it until a new launcher empties it.
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        marked("restart").len() >= 4,
        "container survives its launcher"
    );
    let host = Host::open(&root, &spec.join("state")).unwrap();
    let ended = host.recover().unwrap();
    assert_eq!(
        ended,
        vec![ChildId {
            group: "g/1".into(),
            incarnation: 7,
            key: 3,
            role: Mode::Install,
        }]
    );
    let name = cgroup.file_name().unwrap().to_owned();
    assert_gone("restart", &root.join(name));
}

#[test]
fn contained_refused_uncontainable_host() {
    let Some(own) = scope("contain::contained_refused_uncontainable_host") else {
        return;
    };
    let state = tempfile::tempdir().unwrap();
    // A cgroup that still holds a process cannot give its children controllers.
    assert!(matches!(
        Host::open(&own, state.path()),
        Err(Uncontainable::Controllers(_))
    ));
    let root = children(&own);
    // The oracle for the namespace half is util-linux's unshare.
    let can = Command::new("unshare")
        .args([
            "--user",
            "--map-root-user",
            "--pid",
            "--net",
            "--mount",
            "--fork",
            "true",
        ])
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success();
    let opened = Host::open(&root, state.path());
    assert_eq!(opened.is_ok(), can, "{:?}", opened.err());
    if !can {
        assert!(matches!(opened, Err(Uncontainable::Mechanism { .. })));
    }
}

/// The contained program. Does nothing unless a law launched it.
#[test]
#[allow(clippy::zombie_processes)]
fn contained_helper() {
    let Ok(role) = std::env::var("HC_ROLE") else {
        return;
    };
    let var = |name: &str| std::env::var(name).unwrap_or_default();
    match role.as_str() {
        "daemon" | "daemon2" => {
            unsafe {
                libc::setsid();
                libc::signal(libc::SIGTERM, libc::SIG_IGN);
            }
            if role == "daemon" {
                respawn("daemon2", Stdio::null());
            }
            loop {
                std::thread::sleep(Duration::from_secs(60));
            }
        }
        "main" | "tool" => {}
        _ => return,
    }
    let who = role.as_str();
    let mut out = std::io::stdout();
    for path in var("HC_READ").split(':').filter(|p| !p.is_empty()) {
        let ok = fs::read(path).is_ok();
        writeln!(
            out,
            "{who} read {path} {}",
            if ok { "ok" } else { "denied" }
        )
        .unwrap();
    }
    for dir in var("HC_WRITE").split(':').filter(|p| !p.is_empty()) {
        let ok = fs::write(
            Path::new(dir).join(format!("w-{who}-{}", std::process::id())),
            "y",
        )
        .is_ok();
        writeln!(
            out,
            "{who} write {dir} {}",
            if ok { "ok" } else { "denied" }
        )
        .unwrap();
    }
    let addr: std::net::SocketAddr = var("HC_CONNECT").parse().unwrap();
    let ok = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).is_ok();
    writeln!(out, "{who} connect {}", if ok { "ok" } else { "denied" }).unwrap();
    let ok = UnixStream::connect(var("HC_UNIX")).is_ok();
    writeln!(out, "{who} control {}", if ok { "ok" } else { "denied" }).unwrap();
    if who == "tool" {
        writeln!(out, "tool ready").unwrap();
        out.flush().unwrap();
        unsafe { libc::signal(libc::SIGTERM, libc::SIG_IGN) };
        respawn("daemon", Stdio::null());
        loop {
            std::thread::sleep(Duration::from_secs(60));
        }
    }
    let open: Vec<String> = (3..64)
        .filter(|fd| unsafe { libc::fcntl(*fd, libc::F_GETFD) } >= 0)
        .map(|fd| fd.to_string())
        .collect();
    writeln!(out, "main fds [{}]", open.join(",")).unwrap();
    // A tool descendant, as an Exec grant starts one, reports and stays.
    let mut tool = respawn("tool", Stdio::piped());
    let mut lines = BufReader::new(tool.stdout.take().unwrap());
    loop {
        let mut line = String::new();
        if lines.read_line(&mut line).unwrap() == 0 || line.trim() == "tool ready" {
            break;
        }
        if line.starts_with("tool ") {
            write!(out, "{line}").unwrap();
        }
    }
    respawn("daemon", Stdio::null());
    if let Ok(peer) = var("HC_ACCEPT").parse::<SocketAddr>() {
        let listener = unsafe { TcpListener::from_raw_fd(3) };
        let mut stream = crate::loopback::accept_from_address(&listener, peer).unwrap();
        stream.write_all(b"hi").unwrap();
        writeln!(out, "main accept ok").unwrap();
    }
    // Give the daemons time to start before the host counts them.
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(300) {
        std::thread::sleep(Duration::from_millis(50));
    }
    writeln!(out, "ready").unwrap();
    out.flush().unwrap();
    if var("HC_HOLD") == "1" {
        loop {
            std::thread::sleep(Duration::from_secs(60));
        }
    }
    std::mem::forget(tool);
}

#[allow(clippy::zombie_processes)]
fn respawn(role: &str, stdout: Stdio) -> std::process::Child {
    Command::new(std::env::current_exe().unwrap())
        .args(helper_args())
        .env("HC_ROLE", role)
        .stdout(stdout)
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}
