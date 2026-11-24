use super::{PACKAGE_W, PACKAGE_X, PACKAGE_Y, PackageLaw, package_bundle, process};
use crate::package_add::{Checkpoint, run_at};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, TcpListener};
use std::process::{Command, Output};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

fn add(law: &PackageLaw, dir: &str, args: &[&str]) -> Command {
    let mut arguments = vec!["add".to_owned()];
    arguments.extend(args.iter().map(|arg| (*arg).to_owned()));
    arguments.extend([
        "--package-store".into(),
        law.path("packages").display().to_string(),
    ]);
    let mut command = process::tape_request_command(&arguments, &law.path(dir));
    command.env("XDG_CACHE_HOME", law.path("cache"));
    command
}

fn result(output: Output) -> (Option<i32>, String, String) {
    let each = |bytes: &[u8]| {
        let string = String::from_utf8_lossy(bytes);
        string
            .split_once(process::CHILD_MARK)
            .unwrap_or_else(|| panic!("request child did not start: {output:?}"))
            .1
            .to_owned()
    };
    (
        output.status.code(),
        each(&output.stdout),
        each(&output.stderr),
    )
}

fn invoke(law: &PackageLaw, dir: &str, args: &[&str]) -> (Option<i32>, String, String) {
    result(add(law, dir, args).output().unwrap())
}

fn direct(law: &PackageLaw, source: &str) -> crate::cli::Invocation {
    let args = [
        "add",
        source,
        "--package-store",
        law.path("packages").to_str().unwrap(),
    ]
    .map(str::to_owned);
    let crate::cli::Action::Invoke(invocation) = crate::cli::parse(args.into_iter()).unwrap()
    else {
        panic!("expected an add invocation")
    };
    invocation
}

fn copy(law: &PackageLaw, from: &str, to: &str) {
    law.write(to, fs::read(law.path(from)).unwrap());
}

fn wrap(law: &PackageLaw, out: &str) {
    fs::create_dir_all(law.path(out).parent().unwrap()).unwrap();
    law.library("wrap", "wrap", 1, "pub fn one() -> u64 { 1u64 }\n",
        &format!("[dependencies]\njson = {{ major = 1, root = \"{PACKAGE_X}\", archive = \"jx.hpkg\" }}\n"));
    copy(law, "vendor/jx.hpkg", "wrap/jx.hpkg");
    assert_eq!(
        law.fixture("wrap", out),
        format!("wrap@1 {PACKAGE_W}\njson@1 {PACKAGE_X}\n")
    );
}

fn refusal_keeps_workspace_and_checks_full_closure() {
    let law = PackageLaw::new();
    law.fixture_json();
    wrap(&law, "work/app/wrap.hpkg");
    law.write(
        "work/hirsel.toml",
        "[workspace]\nmembers = [\"app\", \"a\"]\n",
    );
    copy(&law, "vendor/jy.hpkg", "work/a/jy.hpkg");
    law.library("work/a", "a", 1, "pub fn one() -> u64 { 1u64 }\n",
        &format!("[dependencies]\njson = {{ major = 1, root = \"{PACKAGE_Y}\", archive = \"jy.hpkg\" }}\n"));
    law.app(
        "work/app",
        "[dependencies]\na = { major = 1, path = \"../a\" }\n",
    );
    PackageLaw::pass(law.run(&["lock", "work/app"]));
    let manifests = [
        "work/hirsel.toml",
        "work/app/hirsel.toml",
        "work/a/hirsel.toml",
        "work/hirsel.lock",
    ]
    .map(|path| (law.path(path), fs::read(law.path(path)).unwrap()));
    let compare = || {
        for (path, bytes) in &manifests {
            assert_eq!(&fs::read(path).unwrap(), bytes, "{}", path.display());
        }
    };

    let x = law.encoded(PACKAGE_X);
    let w = law.encoded(PACKAGE_W);
    let y = law.encoded(PACKAGE_Y);
    let corrupt = &x[..x.len() - 1];
    for (entries, message) in [
        (vec![w.as_slice(), corrupt], "truncated"),
        (vec![w.as_slice()], "lacks"),
        (
            vec![w.as_slice(), x.as_slice(), y.as_slice()],
            "nothing in it binds",
        ),
    ] {
        law.write("work/app/bad.hpkg", package_bundle(&entries));
        let (status, _, stderr) = invoke(&law, "work/app", &["bad.hpkg"]);
        assert_eq!(status, Some(2), "{stderr}");
        assert!(stderr.contains(message), "{stderr}");
        compare();
        assert_eq!(fs::read_dir(law.path("packages")).unwrap().count(), 1);
    }

    let (status, _, stderr) = invoke(&law, "work/app", &["wrap.hpkg"]);
    assert_eq!(status, Some(2), "{stderr}");
    for text in ["bound to more than one root", PACKAGE_X, PACKAGE_Y] {
        assert!(stderr.contains(text), "{stderr}");
    }
    compare();

    let law = PackageLaw::new();
    law.fixture_json();
    wrap(&law, "app/wrap.hpkg");
    law.app("app", "");
    fs::create_dir(law.path(&format!("packages/{}", &PACKAGE_W[7..]))).unwrap();
    let manifest = fs::read(law.path("app/hirsel.toml")).unwrap();
    let (status, _, stderr) = invoke(&law, "app", &["wrap.hpkg"]);
    assert_eq!(status, Some(2), "{stderr}");
    assert!(stderr.contains("cache: partial"), "{stderr}");
    assert_eq!(
        fs::read(law.path(&format!("packages/{}", &PACKAGE_X[7..]))).unwrap(),
        law.encoded(PACKAGE_X)
    );
    assert_eq!(fs::read(law.path("app/hirsel.toml")).unwrap(), manifest);
    assert!(!law.path("app/hirsel.lock").exists());
}

fn archive_capture_and_offline_build() {
    let law = PackageLaw::new();
    law.fixture_json();
    law.write("work/hirsel.toml", "[workspace]\nmembers = [\"apps/ui\"]\n");
    law.app(
        "work/apps/ui",
        "# Dependencies\n[dependencies]\n# Standard library\nstd = { major = 1 } # keep\n",
    );
    copy(&law, "vendor/jx.hpkg", "work/vendor/pkg.hpkg");
    copy(&law, "vendor/jy.hpkg", "work/vendor/replacement.hpkg");
    let cwd = law.path("work/apps/ui");
    let manifest = cwd.join("hirsel.toml");
    let before = fs::read_to_string(&manifest).unwrap();
    let validate_unchanged = || {
        assert_eq!(fs::read_to_string(&manifest).unwrap(), before);
        assert!(!law.path("work/hirsel.lock").exists());
    };
    for flag in ["--locked", "--frozen"] {
        PackageLaw::refuse(
            invoke(&law, "work/apps/ui", &["../../vendor/pkg.hpkg", flag]),
            &format!("hirsel.toml needs to be updated but {flag} was passed"),
        );
        validate_unchanged();
    }
    let json = PackageLaw::pass(invoke(
        &law,
        "work/apps/ui",
        &["../../vendor/pkg.hpkg", "--dry-run", "--json"],
    ));
    let json: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(json["result"], "added");
    assert_eq!(json["root"], PACKAGE_X);
    assert_eq!(json["dry_run"], true);
    assert_eq!(json["committed"], serde_json::json!([]));
    validate_unchanged();
    fs::remove_file(law.path(&format!("packages/{}", &PACKAGE_X[7..]))).unwrap();

    let status = run_at(direct(&law, "../../vendor/pkg.hpkg"), &cwd, |point| {
        if point == Checkpoint::ArchiveCaptured {
            fs::rename(
                law.path("work/vendor/replacement.hpkg"),
                law.path("work/vendor/pkg.hpkg"),
            )
            .unwrap();
        }
    })
    .unwrap();
    assert_eq!(status, 0);
    assert_eq!(
        fs::read(law.path(&format!("packages/{}", &PACKAGE_X[7..]))).unwrap(),
        law.encoded(PACKAGE_X)
    );
    assert!(!law.path(&format!("packages/{}", &PACKAGE_Y[7..])).exists());
    let expected = before.replace("# Standard library", &format!(
        "json = {{ major = 1, root = \"{PACKAGE_X}\", archive = \"../../vendor/pkg.hpkg\" }}\n# Standard library"
    ));
    assert_eq!(fs::read_to_string(&manifest).unwrap(), expected);
    PackageLaw::pass(law.run(&["build", "work/apps/ui", "--frozen"]));
    copy(&law, "vendor/jx.hpkg", "work/vendor/pkg.hpkg");
    PackageLaw::pass(law.with_store(&["fetch", "work/apps/ui", "--locked"], "cold-store"));
    PackageLaw::pass(law.with_store(&["build", "work/apps/ui", "--frozen"], "cold-store"));
    assert_eq!(
        fs::read(law.path(&format!("cold-store/{}", &PACKAGE_X[7..]))).unwrap(),
        law.encoded(PACKAGE_X)
    );

    let error = run_at(direct(&law, "../../vendor/pkg.hpkg"), &cwd, |point| {
        if point == Checkpoint::ArchiveCaptured {
            fs::write(&manifest, format!("{before}\n# edit while add runs\n")).unwrap();
        }
    })
    .unwrap_err();
    assert!(
        error.to_string().contains("changed while hirsel add ran"),
        "{error}"
    );
    assert_eq!(
        fs::read_to_string(&manifest).unwrap(),
        format!("{before}\n# edit while add runs\n")
    );
}

fn headers(stream: &mut impl BufRead) -> String {
    let mut line = String::new();
    stream.read_line(&mut line).unwrap();
    let request = line.clone();
    while line != "\r\n" && !line.is_empty() {
        line.clear();
        stream.read_line(&mut line).unwrap();
    }
    request
}

enum Response {
    Archive(Vec<u8>),
    Oversize,
}

fn https(
    law: &PackageLaw,
    responses: Vec<Response>,
    commands: impl FnOnce(&str, &dyn Fn(&[&str]) -> Command),
) {
    use rustls::{
        ServerConfig, ServerConnection, StreamOwned,
        pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
    };
    const CA: &[u8] = include_bytes!("../../../hirsel-core-rt/testdata/tls/ca.pem");
    const LEAF: &[u8] = include_bytes!("../../../hirsel-core-rt/testdata/tls/alpha.pem");
    const KEY: &[u8] = include_bytes!("../../../hirsel-core-rt/testdata/tls/alpha.key.pem");
    law.write("ca.pem", CA);
    let config = Arc::new(
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from_pem_slice(LEAF).unwrap()],
                PrivateKeyDer::from_pem_slice(KEY).unwrap(),
            )
            .unwrap(),
    );
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let proxy = format!("http://{}", listener.local_addr().unwrap());
    listener.set_nonblocking(true).unwrap();
    let total = responses.len();
    let (done, finished) = std::sync::mpsc::channel();
    let server = thread::spawn(move || {
        let mut count = 0;
        let mut responses = responses.into_iter();
        loop {
            let socket = match listener.accept() {
                Ok((socket, _)) => socket,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if finished.recv_timeout(Duration::from_millis(5))
                        != Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                    {
                        break;
                    }
                    continue;
                }
                Err(error) => panic!("{error}"),
            };
            socket
                .set_read_timeout(Some(Duration::from_secs(15)))
                .unwrap();
            socket
                .set_write_timeout(Some(Duration::from_secs(15)))
                .unwrap();
            let mut socket = BufReader::new(socket);
            assert_eq!(headers(&mut socket), "CONNECT alpha.test:443 HTTP/1.1\r\n");
            let socket = socket.get_mut();
            socket
                .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                .unwrap();
            let mut tls = BufReader::new(StreamOwned::new(
                ServerConnection::new(config.clone()).unwrap(),
                socket,
            ));
            assert_eq!(headers(&mut tls), "GET /package.hpkg HTTP/1.1\r\n");
            count += 1;
            let tls = tls.get_mut();
            match responses
                .next()
                .unwrap_or_else(|| panic!("unexpected GET {count}"))
            {
                Response::Archive(bytes) => {
                    write!(
                        tls,
                        "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
                        bytes.len()
                    )
                    .unwrap();
                    tls.write_all(&bytes).unwrap();
                }
                Response::Oversize => {
                    tls.write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\nTransfer-Encoding: chunked\r\n\r\n").unwrap();
                    let chunks = crate::package_content::ARCHIVE_LIMIT / 65536 + 1;
                    let bytes = [0; 65536];
                    for _ in 0..chunks {
                        if tls
                            .write_all(b"10000\r\n")
                            .and_then(|_| tls.write_all(&bytes))
                            .and_then(|_| tls.write_all(b"\r\n"))
                            .is_err()
                        {
                            break;
                        }
                    }
                    let _ = tls.write_all(b"0\r\n\r\n");
                }
            }
            let _ = tls.flush();
        }
        count
    });
    let url = "https://alpha.test/package.hpkg";
    commands(url, &|args| {
        let mut arguments = vec![url, "--json"];
        arguments.extend_from_slice(args);
        let mut command = add(law, "app", &arguments);
        command
            .env("ALL_PROXY", &proxy)
            .env("NO_PROXY", "")
            .env_remove("no_proxy")
            .env("SSL_CERT_FILE", law.path("ca.pem"))
            .env_remove("SSL_CERT_DIR");
        command
    });
    done.send(()).unwrap();
    assert_eq!(server.join().unwrap(), total);
}

fn url_capture_pins_and_caps() {
    let law = PackageLaw::new();
    law.fixture_json();
    wrap(&law, "wrap.hpkg");
    law.app("app", "");
    let manifest = fs::read(law.path("app/hirsel.toml")).unwrap();
    let from = |path| Response::Archive(fs::read(law.path(path)).unwrap());
    https(
        &law,
        vec![
            from("vendor/jy.hpkg"),
            from("vendor/jx.hpkg"),
            from("wrap.hpkg"),
            from("vendor/jy.hpkg"),
            from("vendor/jy.hpkg"),
            Response::Oversize,
        ],
        |url, command| {
            let call = |args: &[&str]| result(command(args).output().unwrap());
            let (status, _, stderr) = call(&["--root", PACKAGE_X]);
            assert_eq!(status, Some(2), "{stderr}");
            assert!(
                stderr.contains("fetched root differs from --root"),
                "{stderr}"
            );
            assert_eq!(fs::read(law.path("app/hirsel.toml")).unwrap(), manifest);
            law.empty_store();

            let output = PackageLaw::pass(call(&["--root", PACKAGE_X]));
            let json: serde_json::Value = serde_json::from_str(&output).unwrap();
            assert_eq!(json["source"], format!("url:{url}"));
            assert_eq!(
                json["closure"][0],
                serde_json::json!({"id":"json@1","root":PACKAGE_X,"new":true})
            );
            let before = fs::read(law.path("app/hirsel.toml")).unwrap();
            let lock = fs::read(law.path("app/hirsel.lock")).unwrap();
            for _ in 0..2 {
                let (status, _, stderr) = call(&[]);
                assert_eq!(status, Some(2), "{stderr}");
                assert!(stderr.contains("already bound to another root"), "{stderr}");
                assert_eq!(fs::read(law.path("app/hirsel.toml")).unwrap(), before);
                assert_eq!(fs::read(law.path("app/hirsel.lock")).unwrap(), lock);
                assert_eq!(fs::read_dir(law.path("packages")).unwrap().count(), 1);
            }
            PackageLaw::pass(call(&["--update"]));
            let after = fs::read(law.path("app/hirsel.toml")).unwrap();
            assert!(String::from_utf8_lossy(&after).contains(PACKAGE_Y));
            let entries = fs::read_dir(law.path("packages")).unwrap().count();
            let (status, _, stderr) = call(&["--update"]);
            assert_eq!(status, Some(2), "{stderr}");
            assert!(stderr.contains("over the limit"), "{stderr}");
            assert_eq!(fs::read(law.path("app/hirsel.toml")).unwrap(), after);
            assert_eq!(fs::read_dir(law.path("packages")).unwrap().count(), entries);
        },
    );
}

const CRASH_SELECT: &str = "HIRSEL_S6_ADD_CRASH_ARCHIVE";
const RSS_SELECT: &str = "HIRSEL_S6_ADD_RSS_ARCHIVE";

#[test]
fn s6_add_interrupted_child() {
    let Ok(source) = std::env::var(CRASH_SELECT) else {
        return;
    };
    let cwd = std::env::current_dir().unwrap();
    let args = ["add", &source, "--package-store", "../packages"].map(str::to_owned);
    let crate::cli::Action::Invoke(invocation) = crate::cli::parse(args.into_iter()).unwrap()
    else {
        panic!("expected an add invocation")
    };
    run_at(invocation, &cwd, |point| {
        if point == Checkpoint::ManifestCommitted {
            std::process::exit(44);
        }
    })
    .unwrap();
    panic!("the test child did not reach the manifest publication point");
}

#[test]
fn s6_add_memory_child() {
    let Ok(source) = std::env::var(RSS_SELECT) else {
        return;
    };
    let cwd = std::env::current_dir().unwrap();
    let args = [
        "add",
        &source,
        "--package-store",
        "../packages",
        "--dry-run",
    ]
    .map(str::to_owned);
    let crate::cli::Action::Invoke(invocation) = crate::cli::parse(args.into_iter()).unwrap()
    else {
        panic!("expected an add invocation")
    };
    assert_eq!(run_at(invocation, &cwd, |_| {}).unwrap(), 0);
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    // SAFETY: getrusage initializes the supplied rusage when it succeeds.
    assert_eq!(
        unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) },
        0
    );
    let peak_kib = unsafe { usage.assume_init() }.ru_maxrss;
    eprintln!("peak RSS {peak_kib} KiB");
    assert!(peak_kib < 2 * 1024 * 1024, "peak RSS {peak_kib} KiB");
}

fn archive_memory_limits() {
    use sha2::{Digest, Sha256};

    fn number(mut number: usize, output: &mut Vec<u8>) {
        while number >= 128 {
            output.push(((number % 128) | 128) as u8);
            number /= 128;
        }
        output.push(number as u8);
    }

    fn text(field: &str, output: &mut Vec<u8>) {
        number(field.len(), output);
        output.extend_from_slice(field.as_bytes());
    }

    fn prefix(name: &str, bindings: &[(String, [u8; 32], Vec<u8>)], len: usize) -> Vec<u8> {
        let mut bytes = vec![1];
        text(name, &mut bytes);
        bytes.push(1);
        text("charge-v1", &mut bytes);
        number(bindings.len(), &mut bytes);
        for (child, root, _) in bindings {
            text(child, &mut bytes);
            bytes.extend_from_slice(&[1, 1]);
            bytes.extend_from_slice(root);
        }
        bytes.push(1);
        text("source.hirsel", &mut bytes);
        bytes.push(1);
        text(name, &mut bytes);
        number(len, &mut bytes);
        bytes
    }

    for (packages, len) in [(8, (64 << 20) - 1024), (4096, 3)] {
        let law = PackageLaw::new();
        law.app("app", "");
        let payload = vec![b' '; len];
        let mut children = Vec::new();
        for index in 1..packages {
            let name = format!("child{index:04}");
            let part = prefix(&name, &[], len);
            let mut hash = Sha256::new();
            let domain = b"hirsel.library-root.v1";
            hash.update((domain.len() as u64).to_le_bytes());
            hash.update(domain);
            hash.update(((part.len() + len + 1) as u64).to_le_bytes());
            hash.update(&part);
            hash.update(&payload);
            hash.update([0]);
            children.push((name, hash.finalize().into(), part));
        }
        let head = prefix("library", &children, len);
        children.sort_by_key(|(_, root, _)| *root);
        let mut archive = fs::File::create(law.path("app/memory.hpkg")).unwrap();
        let mut header = Vec::new();
        text("hirsel.package-archive.v1", &mut header);
        number(packages, &mut header);
        archive.write_all(&header).unwrap();
        for part in std::iter::once(&head).chain(children.iter().map(|(_, _, part)| part)) {
            header.clear();
            number(part.len() + payload.len() + 1, &mut header);
            archive.write_all(&header).unwrap();
            archive.write_all(part).unwrap();
            archive.write_all(&payload).unwrap();
            archive.write_all(&[0]).unwrap();
        }
        drop(archive);
        drop(payload);
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "native::tests::package_add_laws::s6_add_memory_child",
                "--nocapture",
            ])
            .env(RSS_SELECT, "memory.hpkg")
            .current_dir(law.path("app"))
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        eprintln!(
            "{packages} entries: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        assert_eq!(
            fs::read_dir(law.path("packages")).unwrap().count(),
            packages
        );
        assert!(!law.path("app/hirsel.lock").exists());
    }
}

fn crash_repair_and_concurrent_adds() {
    let law = PackageLaw::new();
    law.fixture_json();
    wrap(&law, "app/wrap.hpkg");
    copy(&law, "vendor/jx.hpkg", "app/jx.hpkg");
    law.app("app", "");
    PackageLaw::pass(law.run(&["lock", "app"]));
    let before = fs::read(law.path("app/hirsel.lock")).unwrap();
    let crash = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "native::tests::package_add_laws::s6_add_interrupted_child",
        ])
        .env(CRASH_SELECT, "jx.hpkg")
        .current_dir(law.path("app"))
        .output()
        .unwrap();
    assert_eq!(crash.status.code(), Some(44), "{crash:?}");
    assert_eq!(fs::read(law.path("app/hirsel.lock")).unwrap(), before);
    assert!(
        fs::read_to_string(law.path("app/hirsel.toml"))
            .unwrap()
            .contains(PACKAGE_X)
    );
    let stale = law.run(&["lock", "app", "--locked"]);
    assert_eq!(stale.0, Some(2), "{stale:?}");
    assert!(stale.2.contains("stale under --locked"));
    let repair = PackageLaw::pass(invoke(&law, "app", &["jx.hpkg", "--json"]));
    let repair: serde_json::Value = serde_json::from_str(&repair).unwrap();
    assert_eq!(repair["result"], "unchanged");
    assert_eq!(repair["committed"], serde_json::json!(["hirsel.lock"]));
    PackageLaw::pass(law.run(&["lock", "app", "--locked"]));

    law.app("app", "");
    let mut first = add(&law, "app", &["jx.hpkg"]);
    let mut second = add(&law, "app", &["wrap.hpkg"]);
    for command in [&mut first, &mut second] {
        command
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
    }
    let first = first.spawn().unwrap();
    let second = second.spawn().unwrap();
    for child in [first, second] {
        PackageLaw::pass(result(child.wait_with_output().unwrap()));
    }
    let manifest = fs::read_to_string(law.path("app/hirsel.toml")).unwrap();
    assert!(
        manifest.contains(PACKAGE_X) && manifest.contains(PACKAGE_W),
        "{manifest}"
    );
    PackageLaw::pass(law.run(&["lock", "app", "--locked"]));
}

fn store_source_and_post_commit_failure() {
    let law = PackageLaw::new();
    law.fixture_json();
    law.app("app", "");
    let (status, _, error) = invoke(&law, "app", &[PACKAGE_X]);
    assert_eq!(status, Some(2), "{error}");
    assert!(
        error.contains("root is not in the package store"),
        "{error}"
    );
    law.write(
        &format!("packages/{}", &PACKAGE_X[7..]),
        law.encoded(PACKAGE_X),
    );
    PackageLaw::refuse(
        invoke(&law, "app", &[PACKAGE_X, "--root", PACKAGE_X]),
        "--root applies to a fetched archive",
    );
    assert_eq!(
        run_at(direct(&law, PACKAGE_X), &law.path("app"), |point| {
            if point == Checkpoint::ManifestCommitted {
                fs::create_dir(law.path("app/hirsel.lock")).unwrap();
            }
        })
        .unwrap(),
        1
    );
    let manifest = fs::read_to_string(law.path("app/hirsel.toml")).unwrap();
    assert!(
        manifest.contains(&format!("json = {{ major = 1, root = \"{PACKAGE_X}\" }}")),
        "{manifest}"
    );
    fs::remove_dir(law.path("app/hirsel.lock")).unwrap();
    let repair = PackageLaw::pass(invoke(&law, "app", &[PACKAGE_X, "--json"]));
    let repair: serde_json::Value = serde_json::from_str(&repair).unwrap();
    assert_eq!(repair["source"], "store");
    assert_eq!(repair["result"], "unchanged");
    PackageLaw::pass(law.run(&["lock", "app", "--locked"]));
}

#[test]
fn s6_add_protocol() {
    refusal_keeps_workspace_and_checks_full_closure();
    archive_capture_and_offline_build();
    url_capture_pins_and_caps();
    crash_repair_and_concurrent_adds();
    store_source_and_post_commit_failure();
    archive_memory_limits();
}
