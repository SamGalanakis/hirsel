//! `remote.tls@1` across two processes: `hirsel serve` hosts a program's callee entries on a
//! Unix socket under mutual TLS, and a caller run of the same program sends it a call with
//! io and computes that succeed, fail and fault. The callee runs record their own tapes on
//! the host; the caller's tape replays alone, each host tape replays alone, and `hirsel
//! tapes check` joins them. A client without a certificate never reaches the host.
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};

const PROGRAM: &str = r#"pub type AppError = Grant(GrantError) | Output(OutputError)

fn report(rt: Io, line: String) -> Result[u64, String] {
  match io.stdout(rt) {
    Ok(o) => {
      let _ = out.print(o, line)
      Ok(1u64)
    },
    Error(_) => Error("no stdout"),
  }
}

fn half(n: u64) -> Result[u64, String] {
  failure.check(n < 100u64, Arithmetic())
  if n % 2u64 == 1u64 {
    return Error("odd")
  }
  Ok(n / 2u64)
}

fn shown(outcome: Result[u64, RunError[String]]) -> String {
  match outcome {
    Ok(n) => if n == 4u64 {
      "ok 4\n"
    } else {
      "ok\n"
    },
    Error(Failed(e)) => if e == "odd" {
      "failed odd\n"
    } else {
      "failed\n"
    },
    Error(Faulted(_)) => "faulted\n",
    Error(NotRun(Denied())) => "denied\n",
    Error(NotRun(Unreachable())) => "unreachable\n",
    Error(_) => "refused\n",
  }
}

pub fn main(rt: Io) -> Result[(), AppError] {
  let output = io.stdout(rt) ? Grant
  let far = io.runner(rt, "far") ? Grant
  let called = shown(runner.call(far, report, "remote\n"))
  let even = shown(runner.compute(far, half, 8u64))
  let odd = shown(runner.compute(far, half, 3u64))
  let large = shown(runner.compute(far, half, 200u64))
  out.print(output, called) ? Output
  out.print(output, even) ? Output
  out.print(output, odd) ? Output
  out.print(output, large) ? Output
  Ok(())
}
"#;

/// The TLS fixtures (crates/hirsel-rt/tests/it/tls-data/README.md): a self-signed host
/// identity for `localhost`, a client CA, and a client it issued for `agent-7.hirsel.test`.
const TLS: [(&str, &[u8]); 5] = [
    (
        "server.cert",
        include_bytes!("../../../hirsel-rt/tests/it/tls-data/server.cert"),
    ),
    (
        "server.key",
        include_bytes!("../../../hirsel-rt/tests/it/tls-data/server.key"),
    ),
    (
        "client-ca.cert",
        include_bytes!("../../../hirsel-rt/tests/it/tls-data/client-ca.cert"),
    ),
    (
        "client.cert",
        include_bytes!("../../../hirsel-rt/tests/it/tls-data/client.cert"),
    ),
    (
        "client.key",
        include_bytes!("../../../hirsel-rt/tests/it/tls-data/client.key"),
    ),
];

struct Fixture(PathBuf);
impl Fixture {
    fn new(name: &str) -> Self {
        // A short directory: a Unix socket path is at most 107 bytes.
        let root_scratch = tempfile::Builder::new()
            .prefix(&format!("hirsel-remote-{name}-"))
            .tempdir_in("/tmp")
            .unwrap();
        let root = root_scratch.path().to_path_buf();
        std::fs::create_dir_all(root.join("tapes")).unwrap();
        std::fs::write(root.join("main.hir"), PROGRAM).unwrap();
        let stdlib = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../stdlib/std@1/hirsel.json")
            .canonicalize()
            .unwrap();
        std::fs::write(
            root.join("main.hir.json"),
            serde_json::json!({"schema":1,"package":{"name":"remote","version":"1"},"sources":[{"path":"main.hir","module":[]}],"dependencies":[{"manifest":stdlib,"package":{"name":"std","version":"1"},"expose":[]}],"entry":{"module":[],"name":"main"}}).to_string(),
        )
        .unwrap();
        let file = tls(&root);
        let socket = root.join("host.sock").to_str().unwrap().to_owned();
        std::fs::write(
            root.join("host.json"),
            serde_json::json!({
                "listen": {"unix": socket},
                "identity": {"chain": file("server.cert"), "key": file("server.key")},
                "clients": {"pem": file("client-ca.cert")},
                "callers": [{"name": "app", "identity": {"subject": "agent-7.hirsel.test"},
                             "grants": {"stdout": "stdout.bytes@1", "dirs": []},
                             "definitions": null}],
                "recording": "record",
                "tapes": root.join("tapes").to_str().unwrap(),
            })
            .to_string(),
        )
        .unwrap();
        for (name, client) in [
            ("caller.json", Some(("client.cert", "client.key"))),
            ("anonymous.json", None),
        ] {
            let identity = client
                .map(|(chain, key)| serde_json::json!({"chain": file(chain), "key": file(key)}));
            std::fs::write(
                root.join(name),
                serde_json::json!({"io": {"stdout": "stdout.bytes@1", "dirs": [], "runners": [{
                    "name": "far", "provider": "remote.tls@1",
                    "endpoint": {"unix": socket}, "server_name": "localhost",
                    "roots": {"pem": file("server.cert")}, "identity": identity,
                    "grants": {"stdout": "stdout.bytes@1", "dirs": []},
                    "definitions": null}]}})
                .to_string(),
            )
            .unwrap();
        }
        let _ = root_scratch.keep();
        Self(root)
    }
    fn hirsel(&self, args: &[&std::ffi::OsStr]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_hirsel"))
            .args(args)
            .output()
            .unwrap()
    }
    /// Start the host and wait until it accepts connections.
    fn serve(&self) -> Host {
        serve(&self.0.join("main.hir.json"), &self.0.join("host.json"))
    }
    fn call(&self, grants: &str, tape: Option<&Path>) -> Output {
        let manifest = self.0.join("main.hir.json");
        let grants = self.0.join(grants);
        let mut args = vec![
            "run".as_ref(),
            manifest.as_os_str(),
            "--grants".as_ref(),
            grants.as_os_str(),
        ];
        if let Some(tape) = tape {
            args.extend(["--record".as_ref(), tape.as_os_str()]);
        }
        self.hirsel(&args)
    }
    fn replay(&self, tape: &Path) -> Output {
        let manifest = self.0.join("main.hir.json");
        self.hirsel(&[
            "run".as_ref(),
            manifest.as_os_str(),
            "--replay".as_ref(),
            tape.as_os_str(),
        ])
    }
}
/// Start `hirsel serve` of `manifest` under `host` and wait until it accepts connections.
fn serve(manifest: &Path, host: &Path) -> Host {
    let mut child = Command::new(env!("CARGO_BIN_EXE_hirsel"))
        .args(["serve".as_ref(), manifest.as_os_str()])
        .arg("--host")
        .arg(host)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stderr.take().unwrap()).lines();
    let mut seen = String::new();
    loop {
        match lines.next() {
            Some(Ok(line)) if line.starts_with("hirsel serve: listening on ") => break,
            Some(Ok(line)) => seen.push_str(&line),
            _ => {
                let _ = child.kill();
                panic!("the host did not start: {seen}");
            }
        }
    }
    // Keep draining the host's stderr so it never blocks on a full pipe.
    std::thread::spawn(move || lines.for_each(|line| eprintln!("host: {line:?}")));
    Host(Some(child))
}

/// Write the TLS fixtures under `root/tls`; a fixture file's path by name.
fn tls(root: &Path) -> impl Fn(&str) -> String + use<> {
    let tls = root.join("tls");
    std::fs::create_dir_all(&tls).unwrap();
    for (name, bytes) in TLS {
        std::fs::write(tls.join(name), bytes).unwrap();
    }
    move |name: &str| tls.join(name).to_str().unwrap().to_owned()
}

/// examples/ledger-service with its `ledger` runner on a `hirsel serve` host whose place
/// head is the example's own `head`, its store an `authority.log@1` log under `db`. The host serves the example's
/// sources rewritten by `host_edit`, the caller runs them as they are.
fn ledger(name: &str, host_edit: impl Fn(&str, String) -> String) -> Fixture {
    let root_scratch = tempfile::Builder::new()
        .prefix(&format!("hirsel-remote-{name}-"))
        .tempdir_in("/tmp")
        .unwrap();
    let root = root_scratch.path().to_path_buf();
    let _ = root_scratch.keep();
    let stdlib = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../stdlib/std@1/hirsel.json")
        .canonicalize()
        .unwrap();
    let manifest = serde_json::json!({"schema":1,"package":{"name":"ledger_service","version":"1"},"sources":[{"path":"main.hir","module":[]},{"path":"ledger.hir","module":["ledger"]}],"dependencies":[{"manifest":stdlib,"package":{"name":"std","version":"1"},"expose":[]}],"entry":{"module":[],"name":"main"}}).to_string();
    for (dir, edit) in [("caller", None), ("host", Some(&host_edit))] {
        let dir = root.join(dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (source, text) in [
            (
                "main.hir",
                include_str!("../../../../examples/ledger-service/main.hir"),
            ),
            (
                "ledger.hir",
                include_str!("../../../../examples/ledger-service/ledger.hir"),
            ),
        ] {
            let text = text.to_owned();
            let text = match edit {
                Some(edit) => edit(source, text),
                None => text,
            };
            std::fs::write(dir.join(source), text).unwrap();
        }
        std::fs::write(dir.join("main.hir.json"), &manifest).unwrap();
    }
    std::fs::create_dir_all(root.join("db")).unwrap();
    std::fs::create_dir_all(root.join("tapes")).unwrap();
    let file = tls(&root);
    let socket = root.join("host.sock").to_str().unwrap().to_owned();
    std::fs::write(
        root.join("host.json"),
        serde_json::json!({
            "listen": {"unix": socket},
            "identity": {"chain": file("server.cert"), "key": file("server.key")},
            "clients": {"pem": file("client-ca.cert")},
            "callers": [{"name": "app", "identity": {"subject": "agent-7.hirsel.test"},
                         "grants": {"dirs": []}, "definitions": null}],
            "place": {"head": "ledger_service@1/head",
                      "grants": {"dirs": [{"path": "ledger", "host": root.join("db").to_str().unwrap(),
                                           "access": "read_write"}],
                                 "authorities": {"ledger": {"provider": "authority.log@1",
                                     "floor": "host", "mount": "ledger", "path": "store"}}}},
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        root.join("caller.json"),
        serde_json::json!({"io": {"stdout": "stdout.bytes@1", "tasks": "tasks.cooperative@1",
            "dirs": [], "runners": [{
            "name": "ledger", "provider": "remote.tls@1",
            "endpoint": {"unix": socket}, "server_name": "localhost",
            "roots": {"pem": file("server.cert")},
            "identity": {"chain": file("client.cert"), "key": file("client.key")},
            "grants": {"dirs": []}, "definitions": null}]}, "args": []})
        .to_string(),
    )
    .unwrap();
    Fixture(root)
}

impl Fixture {
    fn serve_from(&self, dir: &str) -> Host {
        serve(
            &self.0.join(dir).join("main.hir.json"),
            &self.0.join("host.json"),
        )
    }
    fn run_from(&self, dir: &str, tape: &Path) -> Output {
        let manifest = self.0.join(dir).join("main.hir.json");
        let grants = self.0.join("caller.json");
        self.hirsel(&[
            "run".as_ref(),
            manifest.as_os_str(),
            "--grants".as_ref(),
            grants.as_os_str(),
            "--record".as_ref(),
            tape.as_os_str(),
        ])
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A running host, stopped when dropped.
struct Host(Option<Child>);
impl Host {
    /// Stop the host: what its callees printed.
    fn stop(mut self) -> String {
        let mut child = self.0.take().unwrap();
        let _ = child.kill();
        let output = child.wait_with_output().unwrap();
        String::from_utf8_lossy(&output.stdout).into_owned()
    }
}
impl Drop for Host {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}
fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn calls_cross_to_a_host_over_a_unix_socket_and_every_tape_replays_alone() {
    let fixture = Fixture::new("e2e");
    let host = fixture.serve();
    let tape = fixture.0.join("T");
    let live = fixture.call("caller.json", Some(&tape));
    // The host requires a client certificate: a client without one never opens a session.
    let anonymous = fixture.call("anonymous.json", None);
    let printed = host.stop();
    assert!(live.status.success(), "{}", stderr(&live));
    assert_eq!(stdout(&live), "ok\nok 4\nfailed odd\nfaulted\n");
    // The call with io ran on the host, under the host's grant for this caller.
    assert_eq!(printed, "remote\n");
    assert!(anonymous.status.success(), "{}", stderr(&anonymous));
    assert_eq!(
        stdout(&anonymous),
        "unreachable\nunreachable\nunreachable\nunreachable\n"
    );

    let caller = fixture.replay(&tape);
    assert!(caller.status.success(), "{}", stderr(&caller));
    assert_eq!(stdout(&caller), "ok\nok 4\nfailed odd\nfaulted\n");

    let mut tapes: Vec<PathBuf> = std::fs::read_dir(fixture.0.join("tapes"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        // Diagnostic timing sidecars accompany tapes but are not replay inputs.
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "tape")
        })
        .collect();
    tapes.sort();
    assert_eq!(tapes.len(), 4, "{tapes:?}");
    let mut replayed: Vec<String> = tapes
        .iter()
        .map(|callee| {
            let replay = fixture.replay(callee);
            assert!(
                !stderr(&replay).contains("replay"),
                "{}: {}",
                callee.display(),
                stderr(&replay)
            );
            stdout(&replay)
        })
        .collect();
    replayed.sort();
    assert_eq!(replayed, ["", "", "", "remote\n"]);

    let mut args = vec!["tapes".as_ref(), "check".as_ref(), tape.as_os_str()];
    args.extend(tapes.iter().map(|t| t.as_os_str()));
    let check = fixture.hirsel(&args);
    assert!(
        check.status.success(),
        "{}{}",
        stdout(&check),
        stderr(&check)
    );
    assert!(
        stdout(&check).ends_with("tapes: 4 links checked, 0 unchecked\n"),
        "{}",
        stdout(&check)
    );
}

/// The ledger's balances by hand (as examples.rs `ledger_service_serves_its_entries_and_replays`):
/// 1 and 2 open with 500 and 100; 200 moves 1 -> 2 (300, 300); 3 opens empty; 50 moves
/// 2 -> 3 (250, 50); 250 moves 2 -> 1 (550, 0); 900 from 1 is refused with its balance.
const LEDGER: &str = "opened 1 with 500\nopened 2 with 100\nmoved 200 from 1 to 2, fee 0\n\
     opened 3 with 0\nmoved 50 from 2 to 3, fee 0\nmoved 250 from 2 to 1, fee 0\n\
     insufficient: 550\naccount 1: 550\naccount 2: 0\naccount 3: 50\n\
     account 4: no account 4\n";

/// A caller's transact and query calls cross `remote.tls@1` to a `hirsel serve` place and
/// end as they do at a `sim@1` place; the caller's tape replays alone, with no host.
#[test]
fn served_calls_cross_to_a_place_host_and_the_callers_tape_replays_alone() {
    let fixture = ledger("served", |_, text| text);
    let host = fixture.serve_from("host");
    let tape = fixture.0.join("T");
    let live = fixture.run_from("caller", &tape);
    host.stop();
    assert!(live.status.success(), "{}", stderr(&live));
    assert_eq!(stdout(&live), LEDGER);
    let replayed = fixture.hirsel(&[
        "run".as_ref(),
        fixture.0.join("caller/main.hir.json").as_os_str(),
        "--replay".as_ref(),
        tape.as_os_str(),
    ]);
    assert!(replayed.status.success(), "{}", stderr(&replayed));
    assert_eq!(stdout(&replayed), LEDGER);
}

/// The place's head makes RUN-8 checks 3 and 5: a served call whose entry the place does
/// not serve is `NotRun(Denied())`, and one whose code differs from the place's is
/// `NotRun(VersionMismatch())`; neither runs.
#[test]
fn a_place_host_denies_unserved_entries_and_refuses_other_code() {
    // The host's `transfer` charges a fee, so `moves` has other code there, and its head
    // does not serve `balance`.
    let fixture = ledger("refused", |source, text| match source {
        "ledger.hir" => text.replace("fee: 0", "fee: 1"),
        _ => text.replace("      store.served_query(balance),\n", ""),
    });
    let host = fixture.serve_from("host");
    let live = fixture.run_from("caller", &fixture.0.join("T"));
    host.stop();
    assert!(live.status.success(), "{}", stderr(&live));
    let mismatch = "not run: version mismatch";
    let denied = "not run: denied";
    assert_eq!(
        stdout(&live),
        format!(
            "opened 1 with 500\nopened 2 with 100\n{mismatch}\nopened 3 with 0\n{mismatch}\n\
             {mismatch}\n{mismatch}\naccount 1: {denied}\naccount 2: {denied}\n\
             account 3: {denied}\naccount 4: {denied}\n"
        )
    );
}
