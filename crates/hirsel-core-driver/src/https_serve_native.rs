/// HTTP-1's HTTPS laws: std.http.serve_https serving identities that a CA
/// minted inside the run issued (std.https.run_ca and issue), against the std
/// client std.http_client.https verifying that CA as its only root. The
/// fixture is core/front/fixtures/recording/https-serve.hirsel. `openssl
/// s_client` with the CA file checks each leaf's SAN independently, so the
/// two ends are not both rustls and not both Hirsel's DER.
mod https_serve_laws {
    use super::*;
    use hirsel_core_net::GrantRule;
    use std::{
        io::{BufRead, BufReader, Read},
        process::{Child, ChildStdin, Stdio},
        sync::mpsc,
        time::{Duration, Instant},
    };

    const LIMIT: Duration = Duration::from_secs(60);

    /// The fixture built by `backend` with `rules` and `files` as its Dir.
    fn program(dir: &Path, files: &Path, backend: Backend, rules: &[String]) -> PathBuf {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let source = include_str!("../../../core/front/fixtures/recording/https-serve.hirsel");
        let mut request = source_request(source, dir);
        request.inputs.packages[1] =
            PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
        request.inputs.packages[0].dependencies.push(package("std"));
        request
            .inputs
            .packages
            .push(CheckRequest::source_std(&root.join("core/std")).unwrap());
        let product = Driver::default()
            .check(request, &mut Meter::new(false))
            .unwrap();
        assert!(
            product.accepted(),
            "{:?} {:?}",
            product.diagnostics,
            product.incomplete
        );
        let mut options = native_options(backend, dir);
        options.grant_mask = ["Net", "Clock", "Tasks", "Dir", "Stdin", "Random", "Stdout", "Args"]
            .iter()
            .map(|name| 1 << grant_kind(name).unwrap())
            .sum();
        options.instance_limit = crate::native::DEFAULT_INSTANCE_LIMIT;
        options.directories = vec![files.display().to_string()];
        options.net_rules = rules
            .iter()
            .map(|rule| GrantRule::parse(rule).unwrap())
            .collect();
        let entry = DefPath::from_full("app@1/main/main").unwrap();
        product.build(&entry, &options).unwrap().executable
    }

    /// The serving run: its port, then its stdin, whose end stops it.
    struct Server {
        child: Child,
        stdin: Option<ChildStdin>,
        port: u16,
    }

    impl Server {
        fn start(program: &Path) -> Server {
            let mut child = Command::new(program)
                .arg("serve")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let stdin = child.stdin.take();
            let (send, lines) = mpsc::channel();
            let stdout = BufReader::new(child.stdout.take().unwrap());
            std::thread::spawn(move || {
                for line in stdout.lines() {
                    let Ok(line) = line else { break };
                    if send.send(line).is_err() {
                        break;
                    }
                }
            });
            let line = match lines.recv_timeout(LIMIT) {
                Ok(line) => line,
                Err(e) => {
                    let _ = child.kill();
                    let mut stderr = String::new();
                    let _ = child.stderr.take().unwrap().read_to_string(&mut stderr);
                    panic!("server printed no port ({e}): {stderr}");
                }
            };
            Server {
                child,
                stdin,
                port: line.parse().unwrap(),
            }
        }

        /// Ends stdin and checks the run stops with status 0.
        fn finish(mut self) {
            drop(self.stdin.take());
            let deadline = Instant::now() + LIMIT;
            let status = loop {
                if let Some(status) = self.child.try_wait().unwrap() {
                    break status;
                }
                assert!(Instant::now() < deadline, "server did not stop");
                std::thread::sleep(Duration::from_millis(20));
            };
            let mut stderr = String::new();
            let _ = self.child.stderr.take().unwrap().read_to_string(&mut stderr);
            assert!(status.success(), "{status}: {stderr}");
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            if let Ok(None) = self.child.try_wait() {
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }
    }

    /// The std client's one line for `name`.
    fn fetch(client: &Path, port: u16, name: &str) -> String {
        let output = Command::new(client)
            .args(["client", &port.to_string(), name])
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim_end().to_owned()
    }

    /// `openssl s_client` with the CA file as its only trust, sending `sni`
    /// (or none) and checking the leaf with `check` (`-verify_hostname x` or
    /// `-verify_ip x`): whether verification succeeded.
    fn openssl(files: &Path, port: u16, sni: Option<&str>, check: [&str; 2]) -> bool {
        let mut command = Command::new("openssl");
        command
            .args(["s_client", "-brief", "-verify_return_error", "-connect"])
            .arg(format!("127.0.0.1:{port}"))
            .arg("-CAfile")
            .arg(files.join("ca.pem"))
            .args(check);
        match sni {
            Some(name) => command.args(["-servername", name]),
            None => command.arg("-noservername"),
        };
        let output = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        output.status.success() && stderr.contains("Verification: OK")
    }

    /// One serving run per engine. Against the run's CA as the client's only
    /// root: the localhost and api.test round trips answer 200 with the path,
    /// which needs SNI to choose those leaves, since the default leaf holds
    /// only an IP SAN; 127.0.0.1 verifies against the default leaf's IP SAN;
    /// wrong.test, served a leaf whose CN is wrong.test but whose one SAN is
    /// other.test, is refused with NameMismatch (RFC 6125 §6.4.4: no CN
    /// fallback when a SAN is present). openssl confirms each leaf's SANs
    /// against the same CA file, including that the wrong.test leaf is the
    /// CA's and valid for other.test, so the refusal is the name check alone.
    #[test]
    #[ignore = "explicit native integration target"]
    fn serve_https_verifies_the_run_ca_by_san() {
        for backend in [Backend::Cranelift, Backend::Llvm] {
            let dir = tempfile::tempdir().unwrap();
            let files = dir.path().join("files");
            for sub in ["files", "server", "client"] {
                std::fs::create_dir(dir.path().join(sub)).unwrap();
            }
            let server = program(
                &dir.path().join("server"),
                &files,
                backend,
                &["Listen 127.0.0.1/32 0-65535".to_owned()],
            );
            let running = Server::start(&server);
            let pem = std::fs::read(files.join("ca.pem")).unwrap();
            assert!(pem.starts_with(b"-----BEGIN CERTIFICATE-----\n"));
            let roots = hirsel_core_ids::hex::encode(&crate::package_content::archive_digest(&pem));
            let client = program(
                &dir.path().join("client"),
                &files,
                backend,
                &[
                    "Connect 127.0.0.1/32 1-65535".to_owned(),
                    format!(
                        "Tls localhost,api.test,wrong.test,127.0.0.1 1-65535 Roots=sha256:{roots}"
                    ),
                ],
            );
            let port = running.port;
            assert_eq!(fetch(&client, port, "localhost"), "200 /hello", "{backend:?}");
            assert_eq!(fetch(&client, port, "api.test"), "200 /hello", "{backend:?}");
            assert_eq!(fetch(&client, port, "127.0.0.1"), "200 /hello", "{backend:?}");
            assert_eq!(fetch(&client, port, "wrong.test"), "name-mismatch", "{backend:?}");

            let host = |name| ["-verify_hostname", name];
            assert!(openssl(&files, port, Some("localhost"), host("localhost")));
            assert!(openssl(&files, port, Some("api.test"), host("api.test")));
            assert!(openssl(&files, port, None, ["-verify_ip", "127.0.0.1"]));
            assert!(openssl(&files, port, Some("wrong.test"), host("other.test")));
            assert!(!openssl(&files, port, Some("wrong.test"), host("wrong.test")));
            // The default leaf has no DNS name: localhost without SNI fails.
            assert!(!openssl(&files, port, None, host("localhost")));
            running.finish();
        }
    }
}
