//! Independent operator peers with the same UID, group and request ID.
use super::super::context::Target;
use crate::launcher::session::{Group, Wire};
use crate::operator::{self, Answer, Ask, Input, Pause, Phase, Preview, Report};
use serde_json::Value;
use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::Path,
    process::Command,
};

const REQUEST: [u8; 16] = [0xab; 16];
const A: u128 = 0x123456789abcdef0123456789abcdef0;
const B: u128 = 0x123456789abcdef0123456789abcdef1;

fn frame(socket: &mut UnixStream) -> Option<Ask> {
    let mut size = [0; 4];
    if socket.read(&mut size[..1]).unwrap() == 0 {
        return None;
    }
    socket.read_exact(&mut size[1..]).unwrap();
    let mut body = vec![0; u32::from_le_bytes(size) as usize];
    socket.read_exact(&mut body).unwrap();
    Some(Ask::decode(&body).unwrap())
}
fn send(socket: &mut UnixStream, reply: Answer) {
    let bytes = reply.encode();
    socket
        .write_all(&(bytes.len() as u32).to_le_bytes())
        .unwrap();
    socket.write_all(&bytes).unwrap();
}
fn listen(state: &Path) -> UnixListener {
    fs::create_dir_all(state).unwrap();
    fs::set_permissions(state, fs::Permissions::from_mode(0o700)).unwrap();
    let path = state.join("operator.sock");
    let socket = UnixListener::bind(&path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    socket
}
fn host(listener: UnixListener, id: u128, read: bool) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        assert_eq!(frame(&mut socket), Some(Ask::Identify));
        let (hi, lo) = ((id >> 64) as u64, id as u64);
        send(&mut socket, Answer::Catalog(hi, lo));
        let received = frame(&mut socket);
        if !read {
            assert_eq!(received, None, "stale pin sent a domain request");
            return;
        }
        assert_eq!(
            received,
            Some(Ask::Pinned(hi, lo, Ask::Status(REQUEST.to_vec()).encode()))
        );
        let group = Group {
            name: "orders".into(),
            incarnation: u64::MAX,
        };
        let report = Report {
            pause: Pause::NotStarted,
            fetch_ns: Some(u64::MAX - 1),
            assemble_ns: None,
            preview: Preview::default(),
        };
        send(
            &mut socket,
            Answer::State(
                REQUEST.to_vec(),
                group,
                Input::Publication(vec![4; 32]),
                Phase::Installed(u64::MAX, vec![5; 32], report),
            ),
        );
    })
}
fn document(bytes: &[u8], exit: i32) -> Value {
    assert_eq!(bytes.last(), Some(&b'\n'), "{bytes:?}");
    assert_eq!(bytes.iter().filter(|b| **b == b'\n').count(), 1);
    let v: Value = serde_json::from_slice(bytes).unwrap();
    assert_eq!(v["v"], 1);
    assert_eq!(v["exit"], exit);
    for k in [
        "command",
        "contract",
        "target",
        "operation",
        "outcome",
        "result",
        "error",
        "next",
    ] {
        assert!(v.get(k).is_some(), "{k}");
    }
    v
}

fn catalog_law(exe: &Path, tmp: &Path) {
    crate::openat2_required::require();
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let inputs = tmp.join("inputs.json");
    let make = Command::new("python3").current_dir(&repo)
        .env("PYTHONPATH", repo.join("core/tools"))
        .args(["-c", "from store_tests import inputs, ROOT; import json; print(json.dumps(inputs(ROOT, tests=True, package='platform')))"])
        .stdout(fs::File::create(&inputs).unwrap()).status().unwrap();
    assert!(make.success());
    let fixture = "platform@1/platform/replaced_catalog";
    let result = Command::new(exe)
        .current_dir(tmp)
        .env("XDG_CONFIG_HOME", tmp.join("isolated-config"))
        .args(["test", "--source-core", "--source-std", "--inputs"])
        .arg(inputs)
        .args([
            "--grant", "Clock", "--grant", "Tasks", "--grant", "Random", "--dir",
        ])
        .arg(tmp)
        .args([
            "--limit",
            "134217728",
            "--exact",
            fixture,
            "--rerun",
            "--json",
            "--runtime",
        ])
        .arg(repo.join(std::env::var_os("HIRSEL_CORE_RT_ARCHIVE").unwrap()))
        .arg("--cc")
        .arg(
            std::path::PathBuf::from(std::env::var_os("LLVM_SYS_231_PREFIX").unwrap())
                .join("bin/clang"),
        )
        .arg("--store")
        .arg(tmp.join("build-store"))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}: {}",
        String::from_utf8_lossy(&result.stderr),
        String::from_utf8_lossy(&result.stdout)
    );
    let report: Value = serde_json::from_slice(&result.stdout).unwrap();
    let rows = report["tests"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["definition"], fixture);
    assert_eq!(rows[0]["source"], "executed");
    assert_eq!(rows[0]["passed"], true);
}

pub(super) fn resolve_identify_read() {
    let exe = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(std::env::var_os("HIRSEL_DEV_TEST_DRIVER").expect("bare driver"))
        .canonicalize()
        .unwrap();
    let parent = std::env::var_os("HIRSEL_OPERATOR_TEST_ROOT").unwrap_or_else(|| "/tmp".into());
    let temp = tempfile::tempdir_in(parent).unwrap();
    let root = temp.path().canonicalize().unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let project = root.join("project");
    let route = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let unwanted = listen(&project);
    let control = UnixListener::bind(project.join("launcher.sock")).unwrap();
    fs::write(
        project.join("hirsel.toml"),
        format!(
            "[operator]\nstate={project:?}\n[registry]\nurl='http://{}'\n",
            route.local_addr().unwrap()
        ),
    )
    .unwrap();
    let config = root.join("config");
    let cli = |args: &[&str]| {
        let mut cmd = Command::new(&exe);
        cmd.current_dir(&project)
            .env("XDG_CONFIG_HOME", &config)
            .args(args);
        cmd
    };
    let run = |args: &[&str], exit: i32| {
        let out = cli(args).output().unwrap();
        assert_eq!(
            out.status.code(),
            Some(exit),
            "{}\n{}",
            String::from_utf8_lossy(&out.stderr),
            String::from_utf8_lossy(&out.stdout)
        );
        document(&out.stdout, exit)
    };
    let unknown = run(&["no-such-command", "--json"], 2);
    let (master, slave) = crate::terminal_fixture::pty();
    let status = cli(&["no-such-command", "--json"])
        .stdin(slave.try_clone().unwrap())
        .stderr(slave.try_clone().unwrap())
        .stdout(slave)
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(2));
    let mut bytes = Vec::new();
    let result = fs::File::from(master).read_to_end(&mut bytes);
    assert!(result.is_ok() || result.unwrap_err().raw_os_error() == Some(libc::EIO));
    assert_eq!(document(&bytes, 2), unknown);
    let guest = cli(&["no-such-command", "--", "--json"]).output().unwrap();
    assert_eq!(guest.status.code(), Some(2));
    assert!(guest.stdout.is_empty());
    let spec = run(&["help", "op", "get", "--json"], 0)["result"].clone();
    let all = run(&["help", "--json"], 0);
    assert!(
        all["result"]["commands"]
            .as_array()
            .unwrap()
            .contains(&spec)
    );
    assert_eq!(spec["operands"], "host/CATALOG/REQUEST");
    assert_eq!(spec["result"]["target_kind"], "host");
    assert_eq!(spec["result"]["credential_bearing"], false);
    let names: Vec<_> = spec["options"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|v| v["names"].as_array().unwrap())
        .collect();
    for expected in ["--json", "--context", "--help"] {
        assert!(names.iter().any(|v| **v == expected));
    }
    let id = format!("host/{A:032x}/{}", hirsel_core_ids::hex::encode(&REQUEST));
    let args = ["op", "get", &id, "--json"];
    run(&args, 2);
    run(&["op", "get", &id, "--context", "--json"], 2);
    assert_eq!(run(&["--json", "no-such-command"], 2), unknown);
    run(&["op", "get", "bad", "--json"], 2);
    run(
        &[
            "op",
            "get",
            &id,
            "--state",
            project.to_str().unwrap(),
            "--json",
        ],
        2,
    );
    for probe in [&unwanted, &control] {
        probe.set_nonblocking(true).unwrap();
        assert_eq!(
            probe.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
    route.set_nonblocking(true).unwrap();
    assert_eq!(
        route.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    let state = root.join("authority");
    let record = format!(
        "kind='host'\nstate={state:?}\nauthority='{A:032x}'\nuid={}\n",
        operator::operator_uid()
    );
    fs::create_dir_all(config.join("hirsel")).unwrap();
    fs::write(
        config.join("hirsel/config.toml"),
        format!("default='production'\n[cache.remote]\nurl='http://{}'\n[contexts.production]\n{record}",route.local_addr().unwrap()),
    )
    .unwrap();
    for dir in [&config, &config.join("hirsel")] {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fs::set_permissions(
        config.join("hirsel/config.toml"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let peer = host(listen(&state), A, true);
    let observed = run(&args, 0);
    peer.join().unwrap();
    assert_eq!(observed["operation"]["id"], id);
    assert_eq!(observed["target"]["group"], "orders");
    assert_eq!(observed["target"]["incarnation"], u64::MAX.to_string());
    assert_eq!(observed["outcome"]["effect"], "committed");
    assert_eq!(
        observed["result"]["report"]["fetch_ns"],
        (u64::MAX - 1).to_string()
    );
    fs::rename(&state, root.join("old")).unwrap();
    let replaced = listen(&state);
    let peer = host(replaced.try_clone().unwrap(), B, false);
    let pin = root.join("job.toml");
    fs::write(&pin, &record).unwrap();
    fs::set_permissions(&pin, fs::Permissions::from_mode(0o600)).unwrap();
    let refused = run(
        &[
            "op",
            "get",
            &id,
            "--context",
            pin.to_str().unwrap(),
            "--json",
        ],
        2,
    );
    assert_eq!(refused["error"]["field"], "context");
    peer.join().unwrap();
    let peer = host(replaced, B, false);
    let target = Target::resolve(pin.to_str()).ok().unwrap();
    let write = Ask::Confirm(
        REQUEST.to_vec(),
        crate::launcher::session::Attempt { hi: 1, lo: 2 },
        vec![1; 32],
    );
    assert!(target.exchange(A, &write).is_err());
    peer.join().unwrap();
    assert_eq!(
        route.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    catalog_law(&exe, &root);
}
