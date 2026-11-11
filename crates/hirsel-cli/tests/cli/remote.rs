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

fn echo(s: String) -> Result[String, String] {
  Ok(string.concat(s, ""))
}

// 256 KiB: a frame each way larger than a TLS session's default 64 KiB send buffer.
fn wide() -> String {
  let mut s = "0123456789abcdef"
  let mut i = 0u64
  while i < 14u64 {
    s = string.concat(s, s)
    i = i + 1u64
  }
  s
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
  let echoed = match runner.compute(far, echo, wide()) {
    Ok(back) => if back == wide() {
      "ok wide\n"
    } else {
      "changed\n"
    },
    Error(NotRun(Unreachable())) => "unreachable\n",
    Error(_) => "refused\n",
  }
  out.print(output, called) ? Output
  out.print(output, even) ? Output
  out.print(output, odd) ? Output
  out.print(output, large) ? Output
  out.print(output, echoed) ? Output
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
        serve(
            &self.0.join("main.hir.json"),
            &self.0.join("host.json"),
            &[],
        )
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
/// Start `hirsel serve` of `manifest` under `host` with `options` and wait until it accepts
/// connections.
fn serve(manifest: &Path, host: &Path, options: &[&str]) -> Host {
    let mut child = Command::new(env!("CARGO_BIN_EXE_hirsel"))
        .arg("serve")
        .args(options)
        .arg(manifest)
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
    /// Serve `dir`'s program; `options` select the head's engine.
    fn serve_from(&self, dir: &str, options: &[&str]) -> Host {
        serve(
            &self.0.join(dir).join("main.hir.json"),
            &self.0.join("host.json"),
            options,
        )
    }
    /// Run `dir`'s program as `caller` runs it, recording `tape`.
    fn run_from(&self, dir: &str, caller: Caller, tape: &Path) -> Output {
        let manifest = self.0.join(dir).join("main.hir.json");
        let grants = self.0.join(format!("caller-{}.json", caller.tasks));
        let text = std::fs::read_to_string(self.0.join("caller.json")).unwrap();
        std::fs::write(&grants, text.replace("tasks.cooperative@1", caller.tasks)).unwrap();
        let mut args: Vec<&std::ffi::OsStr> = vec!["run".as_ref()];
        args.extend(caller.options.iter().map(std::ffi::OsStr::new));
        args.extend([
            manifest.as_os_str(),
            "--grants".as_ref(),
            grants.as_os_str(),
            "--record".as_ref(),
            tape.as_os_str(),
        ]);
        self.hirsel(&args)
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
    assert_eq!(stdout(&live), "ok\nok 4\nfailed odd\nfaulted\nok wide\n");
    // The call with io ran on the host, under the host's grant for this caller.
    assert_eq!(printed, "remote\n");
    assert!(anonymous.status.success(), "{}", stderr(&anonymous));
    assert_eq!(
        stdout(&anonymous),
        "unreachable\nunreachable\nunreachable\nunreachable\nunreachable\n"
    );

    let caller = fixture.replay(&tape);
    assert!(caller.status.success(), "{}", stderr(&caller));
    assert_eq!(stdout(&caller), "ok\nok 4\nfailed odd\nfaulted\nok wide\n");

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
    assert_eq!(tapes.len(), 5, "{tapes:?}");
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
    assert_eq!(replayed, ["", "", "", "", "remote\n"]);

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
        stdout(&check).ends_with("tapes: 5 links checked, 0 unchecked\n"),
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

/// How the caller runs: its engine's options and the tasks provider its run admits.
#[derive(Clone, Copy, Debug)]
struct Caller {
    options: &'static [&'static str],
    tasks: &'static str,
}
const INTERPRETED: Caller = Caller {
    options: &[],
    tasks: "tasks.cooperative@1",
};
/// A native program reaches another host's runner only under `tasks.free@1`.
const NATIVE: Caller = Caller {
    options: &["--native"],
    tasks: "tasks.free@1",
};

/// A caller's transact and query calls cross `remote.tls@1` to a `hirsel serve` place and
/// end as they do at a `sim@1` place; the caller's tape replays alone, with no host.
/// The place's head runs interpreted, or natively (`--native`), and so does the caller;
/// either way the caller sees the same ends.
const CELLS: [(&[&str], Caller); 3] = [
    (&[], INTERPRETED),
    (&["--native"], INTERPRETED),
    (&["--native"], NATIVE),
];

#[test]
fn served_calls_cross_to_a_place_host_and_the_callers_tape_replays_alone() {
    for (head, caller) in CELLS {
        let fixture = ledger("served", |_, text| text);
        let host = fixture.serve_from("host", head);
        let tape = fixture.0.join("T");
        let live = fixture.run_from("caller", caller, &tape);
        host.stop();
        assert!(
            live.status.success(),
            "{head:?} {caller:?}: {}",
            stderr(&live)
        );
        assert_eq!(stdout(&live), LEDGER, "{head:?} {caller:?}");
        let manifest = fixture.0.join("caller/main.hir.json");
        let mut args: Vec<&std::ffi::OsStr> = vec!["run".as_ref()];
        args.extend(caller.options.iter().map(std::ffi::OsStr::new));
        args.extend([manifest.as_os_str(), "--replay".as_ref(), tape.as_os_str()]);
        let replayed = fixture.hirsel(&args);
        assert!(
            replayed.status.success(),
            "{head:?} {caller:?}: {}",
            stderr(&replayed)
        );
        assert_eq!(stdout(&replayed), LEDGER, "{head:?} {caller:?}");
    }
}

/// The call records of a place's `authority.log@1` history under `dir` that arrived on a
/// stream: per call, the definition path that ran, its code hash, and the code hash the
/// caller expected (STORE-11 history format: segment frames `len:u32 check:u32 record`, each
/// appended record's entry one group).
fn stream_calls(dir: &Path) -> Vec<(String, Vec<u8>, Vec<u8>)> {
    struct Cursor<'a>(&'a [u8], usize);
    impl Cursor<'_> {
        fn take(&mut self, n: usize) -> &[u8] {
            self.1 += n;
            &self.0[self.1 - n..self.1]
        }
        fn u8(&mut self) -> u8 {
            self.take(1)[0]
        }
        fn u32(&mut self) -> usize {
            u32::from_le_bytes(self.take(4).try_into().unwrap()) as usize
        }
        fn bytes(&mut self) -> Vec<u8> {
            let n = self.u32();
            self.take(n).to_vec()
        }
    }
    let mut segments: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "seg"))
        .collect();
    segments.sort();
    let mut calls = vec![];
    for segment in segments {
        let data = std::fs::read(&segment).unwrap();
        let mut frames = Cursor(&data, 0);
        while frames.1 < data.len() {
            let len = frames.u32();
            frames.take(4);
            let entry = frames.take(len).to_vec();
            let mut c = Cursor(&entry, 0);
            assert_eq!(c.take(8), b"HSHIST05");
            c.take(32 + 8 + 8);
            if c.u8() == 1 {
                continue; // a claim record
            }
            let created = c.u32();
            c.take(32 * created);
            let retired = c.u32();
            c.take(48 * retired);
            assert_eq!(c.take(8), b"HSGRUP02");
            let count = c.u32();
            let mut arrived = vec![];
            for _ in 0..count {
                match c.u8() {
                    1 => {
                        let def = c.u32();
                        c.take(16);
                        c.bytes();
                        if c.u8() == 1 {
                            c.take(32 + 8 + 32);
                            arrived.push((def, c.bytes()));
                        }
                        c.bytes();
                        c.bytes();
                    }
                    2 => {
                        c.u8();
                        c.bytes();
                    }
                    3 => {
                        c.take(16);
                        for _ in 0..c.u32() {
                            c.u8();
                            c.bytes();
                        }
                    }
                    kind => panic!("commit kind {kind}"),
                }
                for _ in 0..c.u32() {
                    let op = c.u8();
                    c.bytes();
                    if op == 1 {
                        c.bytes();
                    }
                }
            }
            let mut defs = vec![];
            for _ in 0..c.u32() {
                let path = String::from_utf8(c.bytes()).unwrap();
                c.bytes();
                let code = c.take(32).to_vec();
                c.bytes();
                defs.push((path, code));
            }
            for (def, expected) in arrived {
                let (path, code) = defs[def].clone();
                calls.push((path, code, expected));
            }
        }
    }
    calls
}

/// The place's head makes RUN-8 check 3 for the transact and query forms on path and
/// fingerprint: a call whose code differs from the caller's runs the place's own code, and
/// one whose signature differs is `NotRun(VersionMismatch())`. Each call record holds the
/// code hash the caller expected and the one that ran.
#[test]
fn a_place_host_runs_its_own_code_and_refuses_other_signatures() {
    for (head, caller) in CELLS {
        // The host's `transfer` charges a fee, so `moves` has other code there, and its
        // `balance` takes a `u32` account, so its signature differs.
        let fixture = ledger("refused", |source, text| match source {
            "ledger.hir" => text.replace("fee: 0", "fee: 1"),
            _ => text
                .replace(
                    "info: store.CallInfo, account: u64) -> Result[",
                    "info: store.CallInfo, account: u32) -> Result[",
                )
                .replace(
                    "ledger.balance(tx, account)",
                    "ledger.balance(tx, int.widen(account))",
                )
                .replace(
                    "for account in [1u64, 2, 3, 4]",
                    "for account in [1u32, 2, 3, 4]",
                ),
        });
        let host_main = std::fs::read_to_string(fixture.0.join("host/main.hir")).unwrap();
        assert!(
            host_main.contains("account: u32"),
            "the host's edit did not apply"
        );
        let host = fixture.serve_from("host", head);
        let live = fixture.run_from("caller", caller, &fixture.0.join("T"));
        host.stop();
        assert!(
            live.status.success(),
            "{head:?} {caller:?}: {}",
            stderr(&live)
        );
        let mismatch = "not run: version mismatch";
        assert_eq!(
            stdout(&live),
            format!(
                "opened 1 with 500\nopened 2 with 100\nmoved 200 from 1 to 2, fee 1\n\
                 opened 3 with 0\nmoved 50 from 2 to 3, fee 1\nmoved 250 from 2 to 1, fee 1\n\
                 insufficient: 550\naccount 1: {mismatch}\naccount 2: {mismatch}\n\
                 account 3: {mismatch}\naccount 4: {mismatch}\n"
            )
        );
        let mut recorded: Vec<(String, bool)> = stream_calls(&fixture.0.join("db/store"))
            .into_iter()
            .map(|(path, ran, expected)| {
                assert_eq!((ran.len(), expected.len()), (32, 32), "{path}");
                (path.rsplit('/').next().unwrap().to_owned(), ran == expected)
            })
            .collect();
        recorded.sort();
        // `opens` ran the caller's code; `moves` (four calls, the refusal included) the host's.
        let opens = ("opens".to_owned(), true);
        let moves = ("moves".to_owned(), false);
        assert_eq!(
            recorded,
            [
                moves.clone(),
                moves.clone(),
                moves.clone(),
                moves,
                opens.clone(),
                opens.clone(),
                opens
            ]
        );
    }
}

/// STORE-12 and RUN-8: a call that committed under one build resolves as recorded at a head
/// built from another. Host A, built from the example as it is, commits the caller's first
/// call (`opens` 1 with 500); its reply is lost on the way back, so the caller's provider
/// reconnects and sends the call again at its position. Host B is built from sources where
/// only `ledger.open`'s body changed (it adds a cent), so `opens` has another code hash
/// there. The resent position ends with A's recorded end, not `VersionMismatch`, and
/// account 1 holds 500 before its transfers, A's single application, never B's 501. Every
/// later call runs B's code, `opens` included: account 2 opens with 101. The records of
/// `opens` at B hold the caller's hash beside B's; the one at A holds equal hashes.
#[test]
fn a_recorded_position_resolves_at_a_head_of_another_build() {
    use std::io::{Read, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Condvar, Mutex};

    let fixture = ledger("rebuilt", |source, text| match source {
        "ledger.hir" => text.replace(
            "  table.put(mut tx, balances(), account, cents) ? Table\n  Ok(cents)\n",
            "  table.put(mut tx, balances(), account, cents + 1) ? Table\n  Ok(cents + 1)\n",
        ),
        _ => text,
    });
    let host_src = std::fs::read_to_string(fixture.0.join("host/ledger.hir")).unwrap();
    assert!(
        host_src.contains("cents + 1"),
        "build B's edit did not apply"
    );

    // The caller reaches the host through a relay that drops the first reply sent after
    // the first commit is on disk (its table `balances` is the first write naming it) and
    // holds every later connection until build B listens.
    let db = fixture.0.join("db");
    let relay = fixture.0.join("relay.sock");
    let backend = fixture.0.join("host.sock");
    let listener = UnixListener::bind(&relay).unwrap();
    let cut = Arc::new(AtomicBool::new(false));
    let rebuilt = Arc::new((Mutex::new(false), Condvar::new()));
    fn committed(dir: &Path) -> bool {
        std::fs::read_dir(dir).unwrap().flatten().any(|entry| {
            let path = entry.path();
            if path.is_dir() {
                return committed(&path);
            }
            std::fs::read(&path).is_ok_and(|bytes| bytes.windows(8).any(|w| w == b"balances"))
        })
    }
    {
        let (cut, rebuilt) = (cut.clone(), rebuilt.clone());
        std::thread::spawn(move || {
            for client in listener.incoming() {
                let Ok(client) = client else { return };
                if cut.load(Ordering::SeqCst) {
                    let (ready, wake) = &*rebuilt;
                    let mut ready = ready.lock().unwrap();
                    while !*ready {
                        ready = wake.wait(ready).unwrap();
                    }
                }
                let Ok(host) = UnixStream::connect(&backend) else {
                    return;
                };
                let (mut up_from, mut up_to) =
                    (client.try_clone().unwrap(), host.try_clone().unwrap());
                std::thread::spawn(move || {
                    let _ = std::io::copy(&mut up_from, &mut up_to);
                    let _ = up_to.shutdown(std::net::Shutdown::Both);
                });
                let (cut, db) = (cut.clone(), db.clone());
                std::thread::spawn(move || {
                    let (mut down_from, mut down_to) = (host, client);
                    let mut buffer = [0u8; 65536];
                    loop {
                        let n = match down_from.read(&mut buffer) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => n,
                        };
                        if !cut.load(Ordering::SeqCst) && committed(&db) {
                            cut.store(true, Ordering::SeqCst);
                            break;
                        }
                        if down_to.write_all(&buffer[..n]).is_err() {
                            break;
                        }
                    }
                    let _ = down_to.shutdown(std::net::Shutdown::Both);
                    let _ = down_from.shutdown(std::net::Shutdown::Both);
                });
            }
        });
    }
    let mut grants: serde_json::Value =
        serde_json::from_slice(&std::fs::read(fixture.0.join("caller.json")).unwrap()).unwrap();
    let runner = &mut grants["io"]["runners"][0];
    runner["endpoint"] = serde_json::json!({"unix": relay.to_str().unwrap()});
    runner["reconnect"] = serde_json::json!({"attempts": 3});
    std::fs::write(fixture.0.join("relayed.json"), grants.to_string()).unwrap();

    let host_a = fixture.serve_from("caller", &[]);
    let caller = Command::new(env!("CARGO_BIN_EXE_hirsel"))
        .args([
            "run".as_ref(),
            fixture.0.join("caller/main.hir.json").as_os_str(),
            "--grants".as_ref(),
            fixture.0.join("relayed.json").as_os_str(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let started = std::time::Instant::now();
    while !cut.load(Ordering::SeqCst) {
        assert!(
            started.elapsed() < std::time::Duration::from_secs(300),
            "the first commit never reached the disk"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    host_a.stop();
    let host_b = fixture.serve_from("host", &[]);
    {
        let (ready, wake) = &*rebuilt;
        *ready.lock().unwrap() = true;
        wake.notify_all();
    }
    let live = caller.wait_with_output().unwrap();
    host_b.stop();
    assert!(live.status.success(), "{}", stderr(&live));
    // By hand: 1 opens with 500 at A; at B 2 opens with 101, 200 moves 1 -> 2 (300, 301),
    // 3 opens with 1, 50 moves 2 -> 3 (251, 51), 250 moves 2 -> 1 (550, 1), 900 from 1 is
    // refused with its balance.
    assert_eq!(
        stdout(&live),
        "opened 1 with 500\nopened 2 with 101\nmoved 200 from 1 to 2, fee 0\n\
         opened 3 with 1\nmoved 50 from 2 to 3, fee 0\nmoved 250 from 2 to 1, fee 0\n\
         insufficient: 550\naccount 1: 550\naccount 2: 1\naccount 3: 51\n\
         account 4: no account 4\n"
    );
    let opens: Vec<bool> = stream_calls(&fixture.0.join("db/store"))
        .into_iter()
        .filter(|(path, _, _)| path.ends_with("/opens"))
        .map(|(_, ran, expected)| ran == expected)
        .collect();
    assert_eq!(opens, [true, false, false]);
}
