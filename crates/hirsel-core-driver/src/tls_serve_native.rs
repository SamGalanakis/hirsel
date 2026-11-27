/// TLS-9's server laws TL18 to TL23 against the OS provider. One build of
/// core/front/fixtures/recording/tls-serve.hirsel serves each case;
/// `openssl s_client` is the independent client, so the two ends are not
/// both rustls. Every expected value comes from the openssl-made fixtures in
/// crates/hirsel-core-rt/testdata/tls/server/ or from the RFCs cited.
mod tls_serve_laws {
    use super::*;
    use hirsel_core_net::{GrantRule, SecretGrant};
    use std::{
        io::{BufRead, BufReader, Read, Write},
        net::TcpStream,
        process::{Child, ChildStdin, Stdio},
        sync::{OnceLock, mpsc},
        time::{Duration, Instant},
    };

    const FILES: [(&str, &[u8]); 15] = [
        (
            "ca.pem",
            include_bytes!("../../hirsel-core-rt/testdata/tls/server/ca.pem"),
        ),
        (
            "alpha.der",
            include_bytes!("../../hirsel-core-rt/testdata/tls/server/alpha.der"),
        ),
        (
            "alpha.p8",
            include_bytes!("../../hirsel-core-rt/testdata/tls/server/alpha.p8"),
        ),
        (
            "beta.der",
            include_bytes!("../../hirsel-core-rt/testdata/tls/server/beta.der"),
        ),
        (
            "beta.p8",
            include_bytes!("../../hirsel-core-rt/testdata/tls/server/beta.p8"),
        ),
        (
            "alpha-ed25519.der",
            include_bytes!("../../hirsel-core-rt/testdata/tls/server/alpha-ed25519.der"),
        ),
        (
            "alpha-ed25519.p8",
            include_bytes!("../../hirsel-core-rt/testdata/tls/server/alpha-ed25519.p8"),
        ),
        (
            "alpha-rsa2048.der",
            include_bytes!("../../hirsel-core-rt/testdata/tls/server/alpha-rsa2048.der"),
        ),
        (
            "alpha-rsa2048.p8",
            include_bytes!("../../hirsel-core-rt/testdata/tls/server/alpha-rsa2048.p8"),
        ),
        (
            "alpha-p384.der",
            include_bytes!("../../hirsel-core-rt/testdata/tls/server/alpha-p384.der"),
        ),
        (
            "alpha-p384.p8",
            include_bytes!("../../hirsel-core-rt/testdata/tls/server/alpha-p384.p8"),
        ),
        (
            "alpha-rsa1024.der",
            include_bytes!("../../hirsel-core-rt/testdata/tls/server/alpha-rsa1024.der"),
        ),
        (
            "alpha-rsa1024.p8",
            include_bytes!("../../hirsel-core-rt/testdata/tls/server/alpha-rsa1024.p8"),
        ),
        (
            "pad.der",
            include_bytes!("../../hirsel-core-rt/testdata/tls/server/pad.der"),
        ),
        (
            "pad-over.der",
            include_bytes!("../../hirsel-core-rt/testdata/tls/server/pad-over.der"),
        ),
    ];
    const ALPHA_SHA256: &str =
        include_str!("../../hirsel-core-rt/testdata/tls/server/alpha.sha256");
    const BETA_SHA256: &str = include_str!("../../hirsel-core-rt/testdata/tls/server/beta.sha256");
    const ED25519_SHA256: &str =
        include_str!("../../hirsel-core-rt/testdata/tls/server/alpha-ed25519.sha256");
    const RSA2048_SHA256: &str =
        include_str!("../../hirsel-core-rt/testdata/tls/server/alpha-rsa2048.sha256");
    const LIMIT: Duration = Duration::from_secs(60);

    /// Both native engines: every law runs on each.
    const BACKENDS: [Backend; 2] = [Backend::Cranelift, Backend::Llvm];

    fn file(name: &str) -> &'static [u8] {
        FILES.iter().find(|(n, _)| *n == name).unwrap().1
    }

    /// The fixtures, TL19's malformed chain entry and the `send` secret,
    /// written to `files`.
    fn write_files(files: &Path) {
        std::fs::create_dir(files).unwrap();
        for (name, bytes) in FILES {
            std::fs::write(files.join(name), bytes).unwrap();
        }
        std::fs::write(
            files.join("alpha-trail.der"),
            [file("alpha.der"), b"\0"].concat(),
        )
        .unwrap();
        // s_server's -cert_chain reads PEM only.
        let base64 = crate::call::base64(file("pad-over.der"));
        let mut pem = String::from("-----BEGIN CERTIFICATE-----\n");
        for line in base64.as_bytes().chunks(64) {
            pem.push_str(std::str::from_utf8(line).unwrap());
            pem.push('\n');
        }
        pem.push_str("-----END CERTIFICATE-----\n");
        std::fs::write(files.join("pad-over.pem"), pem).unwrap();
        std::fs::write(files.join("send"), "pencil").unwrap();
        std::fs::create_dir(files.join("sock")).unwrap();
    }

    /// The server program for `files`, with each key granted from a `file:`
    /// source; `tweak` adjusts the options (TL24's recording and replay).
    fn program(
        dir: &Path,
        files: &Path,
        backend: Backend,
        tweak: impl FnOnce(&mut NativeOptions),
    ) -> PathBuf {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../core");
        let source = include_str!("../../../core/front/fixtures/recording/tls-serve.hirsel");
        let mut request = source_request(source, dir);
        request.inputs.packages[1] =
            PackageManifest::directory(&root.join("core"), package("core"), true).unwrap();
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
        options.grant_mask = ["Net", "Dir", "Stdin", "Stdout"]
            .iter()
            .map(|name| 1 << grant_kind(name).unwrap())
            .sum();
        options.instance_limit = crate::native::DEFAULT_INSTANCE_LIMIT;
        options.directories = vec![files.display().to_string()];
        let roots =
            hirsel_core_ids::hex::encode(&crate::package_content::archive_digest(file("ca.pem")));
        options.net_rules = [
            "Listen 127.0.0.1/32 0-65535".to_owned(),
            "Connect 127.0.0.1/32 1-65535".to_owned(),
            format!("Tls *.test 1-65535 Roots=sha256:{roots}"),
            "UnixListen sock s.sock".to_owned(),
        ]
        .map(|rule| GrantRule::parse(&rule).unwrap())
        .into();
        options
            .unix_roots
            .insert("sock".into(), files.join("sock").display().to_string());
        let grant = |parameter: &str, path: &str, policy: &str| {
            let source = format!("file:{}", files.join(path).display());
            SecretGrant::new(parameter, &source, policy).unwrap()
        };
        options.secret_grants = vec![
            grant("alpha", "alpha.p8", "tls_serve"),
            grant("beta", "beta.p8", "tls_serve"),
            grant("ed", "alpha-ed25519.p8", "tls_serve"),
            grant("rsa", "alpha-rsa2048.p8", "tls_serve"),
            grant("p384", "alpha-p384.p8", "tls_serve"),
            grant("rsa1024", "alpha-rsa1024.p8", "tls_serve"),
            // Not PKCS#8 (TL19).
            grant("raw", "alpha.der", "tls_serve"),
            // sign(p256) without tls_serve (TL3, TL19).
            grant("signer", "alpha.p8", "sign(p256)"),
            grant("send", "send", "tls_send(alpha.test)"),
        ];
        tweak(&mut options);
        let entry = DefPath::from_full("app@1/main/main").unwrap();
        product.build(&entry, &options).unwrap().executable
    }

    /// The fixtures, written once per test process.
    fn files() -> &'static Path {
        static FILES: OnceLock<(tempfile::TempDir, PathBuf)> = OnceLock::new();
        &FILES
            .get_or_init(|| {
                let dir = tempfile::tempdir().unwrap();
                let files = dir.path().join("files");
                write_files(&files);
                (dir, files)
            })
            .1
    }

    /// The server built by `backend`, once per test process.
    fn built(backend: Backend) -> &'static Path {
        static BUILT: [OnceLock<(tempfile::TempDir, PathBuf)>; 2] = [OnceLock::new(), OnceLock::new()];
        let slot = &BUILT[usize::from(backend == Backend::Llvm)];
        &slot
            .get_or_init(|| {
                let dir = tempfile::tempdir().unwrap();
                let program = program(dir.path(), files(), backend, |_| ());
                (dir, program)
            })
            .1
    }

    /// A running server: its port, its stdout lines and its stdin.
    struct Server {
        child: Child,
        stdin: Option<ChildStdin>,
        lines: mpsc::Receiver<String>,
        port: u16,
    }

    impl Server {
        fn start(backend: Backend, args: &[&str]) -> Server {
            Server::run(built(backend), args, |_| ())
        }

        fn run(program: &Path, args: &[&str], env: impl FnOnce(&mut Command)) -> Server {
            let mut command = Command::new(program);
            env(&mut command);
            let mut child = command
                .args(args)
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
            let mut server = Server {
                child,
                stdin,
                lines,
                port: 0,
            };
            server.port = server.line().parse().unwrap();
            server
        }

        fn line(&mut self) -> String {
            match self.lines.recv_timeout(LIMIT) {
                Ok(line) => line,
                Err(e) => {
                    let _ = self.child.kill();
                    let mut stderr = String::new();
                    let _ = self
                        .child
                        .stderr
                        .take()
                        .unwrap()
                        .read_to_string(&mut stderr);
                    panic!("server printed no line ({e}): {stderr}");
                }
            }
        }

        /// The server's lines up to and including one that starts with `prefix`.
        fn until(&mut self, prefix: &str) -> String {
            loop {
                let line = self.line();
                if line.starts_with(prefix) {
                    return line;
                }
            }
        }

        /// The run's open descriptors, each with what it names.
        fn descriptors(&self) -> Vec<String> {
            crate::host::child_descriptors(self.child.id())
        }

        /// Releases the parked run and checks it ends with status 0. The
        /// result is its remaining stdout lines, then its stderr.
        fn finish(mut self) -> Vec<String> {
            let _ = self.stdin.take().unwrap().write_all(b"g");
            let deadline = Instant::now() + LIMIT;
            let status = loop {
                if let Some(status) = self.child.try_wait().unwrap() {
                    break status;
                }
                if Instant::now() > deadline {
                    let _ = self.child.kill();
                    panic!("server did not end");
                }
                std::thread::sleep(Duration::from_millis(20));
            };
            let mut stderr = String::new();
            self.child
                .stderr
                .take()
                .unwrap()
                .read_to_string(&mut stderr)
                .unwrap();
            assert!(status.success(), "{status}: {stderr}");
            let mut lines: Vec<String> = self.lines.try_iter().collect();
            lines.push(stderr);
            lines
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

    /// Runs `openssl s_client` against the server. With `send`, it writes the
    /// line and waits for the server's echo before it closes its input, so a
    /// NewSessionTicket the server sent after its Finished has arrived.
    fn client(port: u16, args: &[&str], send: Option<&str>) -> String {
        let files = files();
        let out = tempfile::NamedTempFile::new().unwrap();
        // One open file for both streams, so they share an offset and
        // neither overwrites the other.
        let sink = out.reopen().unwrap();
        let mut child = Command::new("openssl")
            .arg("s_client")
            .arg("-connect")
            .arg(format!("127.0.0.1:{port}"))
            .arg("-CAfile")
            .arg(files.join("ca.pem"))
            .args(args)
            .stdin(Stdio::piped())
            .stdout(sink.try_clone().unwrap())
            .stderr(sink)
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let deadline = Instant::now() + LIMIT;
        if let Some(line) = send {
            let _ = stdin.write_all(format!("{line}\n").as_bytes());
            loop {
                let seen = std::fs::read_to_string(out.path()).unwrap_or_default();
                if seen.contains(line) || child.try_wait().unwrap().is_some() {
                    break;
                }
                assert!(Instant::now() < deadline, "no echo: {seen}");
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        // Until the handshake ends either way, so an alert sent after the
        // ServerHello is read before stdin's end closes the client.
        loop {
            let seen = std::fs::read_to_string(out.path()).unwrap_or_default();
            if seen.contains("Verify return code") || child.try_wait().unwrap().is_some() {
                break;
            }
            assert!(Instant::now() < deadline, "no handshake end: {seen}");
            std::thread::sleep(Duration::from_millis(20));
        }
        drop(stdin);
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            if Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        std::fs::read_to_string(out.path()).unwrap()
    }

    /// The DER of the first certificate `s_client` printed.
    fn leaf(output: &str) -> Vec<u8> {
        use rustls::pki_types::{CertificateDer, pem::PemObject};
        let start = output
            .find("-----BEGIN CERTIFICATE-----")
            .unwrap_or_else(|| {
                panic!("no certificate printed: {output}");
            });
        CertificateDer::from_pem_slice(&output.as_bytes()[start..])
            .unwrap()
            .to_vec()
    }

    fn digest(der: &[u8]) -> String {
        hirsel_core_ids::hex::encode(&crate::package_content::archive_digest(der))
    }

    fn verified(output: &str) {
        assert!(output.contains("Verify return code: 0 (ok)"), "{output}");
    }

    fn alert(output: &str, code: u8) {
        assert!(
            output.contains(&format!("SSL alert number {code}\n")),
            "no alert {code}: {output}"
        );
    }

    /// `tls.connect` from a second run of the program, with the test CA.
    fn connect(backend: Backend, port: u16, name: &str) -> String {
        let output = Command::new(built(backend))
            .args(["client", &port.to_string(), name])
            .current_dir(files())
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    /// An `openssl s_server` that serves one connection with alpha's key and
    /// the chain [alpha, pad-over], a Certificate body of 65,536 bytes: the
    /// child, which is killed on drop, and its port.
    struct OverCap(Child, u16);

    impl Drop for OverCap {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn over_cap_server() -> OverCap {
        let files = files();
        let mut child = Command::new("openssl")
            .args(["s_server", "-accept", "127.0.0.1:0", "-naccept", "1", "-tls1_3"])
            .args(["-certform", "DER", "-keyform", "DER", "-cert"])
            .arg(files.join("alpha.der"))
            .arg("-key")
            .arg(files.join("alpha.p8"))
            .arg("-cert_chain")
            .arg(files.join("pad-over.pem"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        while !line.starts_with("ACCEPT ") {
            line.clear();
            assert_ne!(stdout.read_line(&mut line).unwrap(), 0, "s_server ended");
        }
        let port = line.trim().rsplit(':').next().unwrap().parse().unwrap();
        // Keep draining, so s_server never blocks on a full pipe.
        std::thread::spawn(move || std::io::copy(&mut stdout, &mut std::io::sink()));
        OverCap(child, port)
    }

    /// TLS-9's `peer`: the leaf's SHA-256 as four big-endian words (TL1).
    fn peer(sha256: &str) -> String {
        let hex = sha256.trim();
        let words = (0..4).map(|i| u64::from_str_radix(&hex[i * 16..i * 16 + 16], 16).unwrap());
        format!(
            "peer {}",
            words.map(|w| w.to_string()).collect::<Vec<_>>().join(" ")
        )
    }

    /// One `sni` connection: the name the server read, the client's output.
    fn sni(backend: Backend, args: &[&str]) -> (String, String) {
        let mut server = Server::start(backend, &["sni", "alpha", "--"]);
        let output = client(server.port, args, Some("tls-echo-18"));
        let name = server.until("name ");
        server.until("echo ");
        server.finish();
        (name, output)
    }

    #[test]
    fn tl18_sni_selects_the_chain() {
        for backend in BACKENDS {
            eprintln!("tl18_sni_selects_the_chain on {backend:?}");
            tl18_sni_selects_the_chain_on(backend);
        }
    }

    fn tl18_sni_selects_the_chain_on(backend: Backend) {
        for (args, name, sha256) in [
            (
                &["-servername", "beta.test"][..],
                "name beta.test",
                BETA_SHA256,
            ),
            (
                &["-servername", "alpha.test"],
                "name alpha.test",
                ALPHA_SHA256,
            ),
            // RFC 6066 §3: the name is case-insensitive and has no trailing dot.
            (
                &["-servername", "ALPHA.test."],
                "name alpha.test",
                ALPHA_SHA256,
            ),
            (&["-noservername"], "name ", ALPHA_SHA256),
            // RFC 6066 §3: a literal IP address is not a HostName.
            (&["-servername", "10.0.0.1"], "name ", ALPHA_SHA256),
            // RFC 8446 §4.1.4: a key share for P-521, which ring has no
            // exchange for, is answered with a HelloRetryRequest for X25519,
            // and the second ClientHello resolves the same key.
            (
                &["-servername", "alpha.test", "-groups", "P-521:X25519"],
                "name alpha.test",
                ALPHA_SHA256,
            ),
        ] {
            let (read, output) = sni(backend, args);
            assert_eq!(read, name, "{args:?}");
            assert_eq!(digest(&leaf(&output)), sha256.trim(), "{args:?}: {output}");
            verified(&output);
        }

        // The Certificate message of [alpha, pad] is exactly 65,535 bytes.
        let mut server = Server::start(backend, &["fixed", "alpha", "--", "alpha.der", "pad.der"]);
        let output = client(
            server.port,
            &["-servername", "alpha.test"],
            Some("tls-echo-cap"),
        );
        assert_eq!(server.until("accept "), "accept none");
        verified(&output);
        server.finish();

        // F4: tls.connect reads the same 65,535-byte Certificate body. rustls
        // joins a handshake message body of at most 0xffff bytes
        // (msgs/deframer/handshake.rs MAX_HANDSHAKE_SIZE), which is RFC 8446's
        // 4 + sum(DER + 5) for [alpha, pad].
        let server = Server::start(backend, &["fixed", "alpha", "--", "alpha.der", "pad.der"]);
        assert_eq!(connect(backend, server.port, "alpha.test"), peer(ALPHA_SHA256));
        server.finish();
        // One byte more, sent by openssl since this server refuses it before
        // issue: the client refuses the message.
        let over = over_cap_server();
        let refused = connect(backend, over.1, "alpha.test");
        assert!(refused.starts_with("connect protocol "), "{refused}");
        drop(over);

        for (key, chain, sha256, signature) in [
            ("ed", "alpha-ed25519.der", ED25519_SHA256, "Ed25519"),
            ("rsa", "alpha-rsa2048.der", RSA2048_SHA256, "RSA-PSS"),
        ] {
            let mut server = Server::start(backend, &["fixed", key, "--", chain]);
            let output = client(
                server.port,
                &["-servername", "alpha.test"],
                Some("tls-echo-kind"),
            );
            assert_eq!(server.until("accept "), "accept none");
            verified(&output);
            assert_eq!(digest(&leaf(&output)), sha256.trim());
            assert!(
                output.lines().filter_map(|line| line.strip_prefix("Peer signature type: ")).any(|value| {
                    value.eq_ignore_ascii_case(signature) || (signature == "RSA-PSS" && matches!(value, "rsa_pss_rsae_sha256" | "rsa_pss_rsae_sha384" | "rsa_pss_rsae_sha512"))
                }),
                "{key}: {output}"
            );
            server.finish();
        }

        for (name, sha256) in [("alpha.test", ALPHA_SHA256), ("beta.test", BETA_SHA256)] {
            let mut server = Server::start(backend, &["sni", "alpha", "--"]);
            assert_eq!(connect(backend, server.port, name), peer(sha256));
            server.until("accept ");
            server.finish();
        }
    }

    #[test]
    fn tl19_wrong_key_or_chain_refused() {
        for backend in BACKENDS {
            eprintln!("tl19_wrong_key_or_chain_refused on {backend:?}");
            tl19_wrong_key_or_chain_refused_on(backend);
        }
    }

    fn tl19_wrong_key_or_chain_refused_on(backend: Backend) {
        for (key, chain, verdict) in [
            // Beta's public key is not alpha's SPKI.
            ("beta", &["alpha.der"][..], "accept key"),
            ("raw", &["alpha.der"], "accept key"),
            // P-384 is not one of sign's kinds; any_supported_type would load it.
            ("p384", &["alpha-p384.der"], "accept key"),
            // RSA-1024 is below sign's 2048 to 4096.
            ("rsa1024", &["alpha-rsa1024.der"], "accept key"),
            ("alpha", &[], "accept chain"),
            ("alpha", &["alpha.der"; 9], "accept chain"),
            ("alpha", &["alpha-trail.der"], "accept chain"),
            ("alpha", &["alpha.der", "pad-over.der"], "accept chain"),
            // sign(p256) without tls_serve (TL3).
            ("signer", &["alpha.der"], "accept denied"),
        ] {
            let mut args = vec!["fixed", key, "--"];
            args.extend(chain);
            let mut server = Server::start(backend, &args);
            let output = client(server.port, &["-servername", "alpha.test", "-msg"], None);
            assert_eq!(server.until("accept "), verdict, "{key} {chain:?}");
            assert!(!output.contains("ServerHello"), "{key} {chain:?}: {output}");
            if verdict == "accept key" {
                // internal_error, sent in place of a ServerHello.
                alert(&output, 80);
            }
            server.finish();
        }
    }

    #[test]
    fn tl21_a_server_half_authorizes_no_secret() {
        for backend in BACKENDS {
            eprintln!("tl21_a_server_half_authorizes_no_secret on {backend:?}");
            tl21_a_server_half_authorizes_no_secret_on(backend);
        }
    }

    fn tl21_a_server_half_authorizes_no_secret_on(backend: Backend) {
        for args in [&["-servername", "alpha.test"][..], &["-noservername"]] {
            let mut server = Server::start(backend, &["half", "alpha", "--", "alpha.der"]);
            let output = client(server.port, args, None);
            assert_eq!(
                server.until("half "),
                "half denied denied denied denied",
                "{args:?}"
            );
            assert_eq!(server.line(), "join unjoined");
            server.finish();
            assert!(!output.contains("\nx"), "{args:?}: {output}");
        }
    }

    #[test]
    fn tl22_server_alpn() {
        for backend in BACKENDS {
            eprintln!("tl22_server_alpn on {backend:?}");
            tl22_server_alpn_on(backend);
        }
    }

    fn tl22_server_alpn_on(backend: Backend) {
        // RFC 7301 §3.2: the server's preference decides.
        for (alpn, args, verdict, seen) in [
            (
                &["http/1.1", "h2"][..],
                &["-alpn", "h2,http/1.1"][..],
                "accept 0",
                "ALPN protocol: http/1.1",
            ),
            (
                &["http/1.1", "h2"],
                &["-alpn", "x"],
                "accept protocol 120",
                "SSL alert number 120",
            ),
            (
                &["http/1.1", "h2"],
                &[],
                "accept none",
                "No ALPN negotiated",
            ),
            (&[], &["-alpn", "h2"], "accept none", "No ALPN negotiated"),
        ] {
            let mut command = vec!["fixed", "alpha"];
            command.extend(alpn);
            command.extend(["--", "alpha.der"]);
            let mut server = Server::start(backend, &command);
            let mut client_args = vec!["-servername", "alpha.test"];
            client_args.extend(args);
            let output = client(server.port, &client_args, None);
            assert_eq!(server.until("accept "), verdict, "{alpn:?} {args:?}");
            assert!(output.contains(seen), "{alpn:?} {args:?}: {output}");
            server.finish();
        }
    }

    #[test]
    fn tl23_decline_and_version() {
        for backend in BACKENDS {
            eprintln!("tl23_decline_and_version on {backend:?}");
            tl23_decline_and_version_on(backend);
        }
    }

    fn tl23_decline_and_version_on(backend: Backend) {
        // RFC 6066 §3: an unknown name gets unrecognized_name, and the
        // declined socket is released.
        let mut server = Server::start(backend, &["sni", "alpha", "--"]);
        let before = server.descriptors();
        let output = client(server.port, &["-servername", "gamma.test"], None);
        assert_eq!(server.until("name "), "name gamma.test");
        assert_eq!(server.line(), "declined");
        // The runtime opens an eventfd at its first wait; the law is about
        // sockets, so the declined one must be gone.
        let sockets = |open: &[String]| -> Vec<String> {
            open.iter().filter(|fd| fd.contains("socket:")).cloned().collect()
        };
        assert_eq!(sockets(&server.descriptors()), sockets(&before));
        alert(&output, 112);
        server.finish();

        let server = Server::start(backend, &["sni", "alpha", "--"]);
        assert_eq!(connect(backend, server.port, "gamma.test"), "connect protocol 112");
        server.finish();

        // RFC 8446 §6: TLS 1.2 gets protocol_version.
        let mut server = Server::start(backend, &["sni", "alpha", "--"]);
        let output = client(server.port, &["-tls1_2", "-servername", "alpha.test"], None);
        assert_eq!(server.until("name "), "name alpha.test");
        assert_eq!(server.line(), "accept protocol 0");
        alert(&output, 70);
        server.finish();

        // RFC 6066 §3: a HostName of letters, digits and hyphens only.
        let mut server = Server::start(backend, &["sni", "alpha", "--"]);
        let output = client(server.port, &["-servername", "bad_name!.test"], None);
        assert_eq!(server.line(), "hello protocol 0");
        alert(&output, 47);
        server.finish();

        let mut server = Server::start(backend, &["sni", "alpha", "--"]);
        let mut plain = TcpStream::connect(("127.0.0.1", server.port)).unwrap();
        plain
            .write_all(b"GET / HTTP/1.1\r\nHost: alpha.test\r\n\r\n")
            .unwrap();
        assert_eq!(server.line(), "hello protocol 0");
        server.finish();

        // The server side of TL15: SSLRequest and a plaintext query in one
        // segment; the query is not a ClientHello.
        let mut server = Server::start(backend, &["join", "alpha", "--"]);
        let mut plain = TcpStream::connect(("127.0.0.1", server.port)).unwrap();
        let query = b"Q\x00\x00\x00\x0dSELECT 1\x00";
        plain
            .write_all(&[&b"\x00\x00\x00\x08\x04\xd2\x16\x2f"[..], query].concat())
            .unwrap();
        assert_eq!(server.line(), "hello protocol 0");
        server.finish();

        // TLS-9: no tickets, so nothing to resume from.
        let session = tempfile::tempdir().unwrap();
        let f = session.path().join("f");
        let mut server = Server::start(backend, &["sni", "alpha", "--"]);
        let output = client(
            server.port,
            &[
                "-servername",
                "alpha.test",
                "-msg",
                "-sess_out",
                f.to_str().unwrap(),
            ],
            Some("tls-echo-ticket"),
        );
        assert_eq!(server.until("echo "), "echo tls-echo-ticket");
        server.finish();
        assert!(output.contains("Finished"), "{output}");
        assert!(!output.contains("NewSessionTicket"), "{output}");
        assert!(!f.exists());
    }

    /// The key's scalar as `openssl pkey -text` prints it.
    fn scalar(p8: &str) -> Vec<u8> {
        let text = Command::new("openssl")
            .args(["pkey", "-inform", "DER", "-noout", "-text", "-in"])
            .arg(files().join(p8))
            .output()
            .unwrap();
        let text = String::from_utf8(text.stdout).unwrap();
        let block = text
            .split("priv:")
            .nth(1)
            .unwrap()
            .split("pub:")
            .next()
            .unwrap();
        let hex: String = block.chars().filter(char::is_ascii_hexdigit).collect();
        let bytes: Vec<u8> = (0..hex.len() / 2)
            .map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap())
            .collect();
        // A leading zero pads a scalar whose top bit is set.
        bytes[bytes.len() - 32..].to_vec()
    }

    #[test]
    fn tl20_the_key_never_leaves_the_provider() {
        for backend in BACKENDS {
            eprintln!("tl20_the_key_never_leaves_the_provider on {backend:?}");
            tl20_the_key_never_leaves_the_provider_on(backend);
        }
    }

    fn tl20_the_key_never_leaves_the_provider_on(backend: Backend) {
        // beta.p8 has one grant; alpha.p8 has two (alpha and signer), so
        // two sealed stores.
        let document = file("beta.p8");
        let scalar = scalar("beta.p8");
        for (mode, expected) in [("sni", Some(1)), ("control", None)] {
            let mut server = Server::start(backend, &[mode, "beta", "--"]);
            let port = server.port;
            let client = std::thread::spawn(move || {
                client(port, &["-servername", "beta.test"], Some("tls-echo-20"))
            });
            assert_eq!(server.until("accept "), "accept none");
            // Echoed and parked on stdin, still holding both halves.
            assert_eq!(server.until("echo "), "echo tls-echo-20");
            if let Some(found) = crate::host::child_memory_hits(server.child.id(), document) {
                match expected {
                    // The sealed store only: TLS-9 parses in place.
                    Some(n) => assert_eq!(found, n, "the PKCS#8 document outside its sealed store"),
                    None => assert!(found >= 2, "the scan missed the Dir copy: {found}"),
                }
            }
            client.join().unwrap();
            let lines = server.finish().join("\n");
            for secret in [document, &scalar[..]] {
                let hex = hirsel_core_ids::hex::encode(secret);
                for form in [hex.clone(), hex.to_uppercase(), crate::call::base64(secret)] {
                    assert!(!lines.contains(&form), "{mode}: {lines}");
                }
                assert!(!lines.as_bytes().windows(secret.len()).any(|w| w == secret));
            }
        }
    }

    #[test]
    fn tl24_no_server_key_in_a_recording() {
        for backend in BACKENDS {
            eprintln!("tl24_no_server_key_in_a_recording on {backend:?}");
            tl24_no_server_key_in_a_recording_on(backend);
        }
    }

    fn tl24_no_server_key_in_a_recording_on(backend: Backend) {
        let dir = tempfile::tempdir().unwrap();
        let files = dir.path().join("files");
        write_files(&files);
        let tape = dir.path().join("serve.tape");
        let record = program(dir.path(), &files, backend, |options| {
            options.runtime = env_record_runtime();
            options.recording = true;
            options.output = dir.path().join("record");
        });
        let mut server = Server::run(&record, &["sni", "alpha", "--"], |command| {
            command.env("HIRSEL_RECORD", &tape);
        });
        let output = client(
            server.port,
            &["-servername", "beta.test"],
            Some("tls-echo-24"),
        );
        verified(&output);
        assert_eq!(server.until("name "), "name beta.test");
        assert_eq!(server.line(), "accept none");
        assert_eq!(server.line(), "echo tls-echo-24");
        server.finish();

        let recorded = std::fs::read(&tape).unwrap();
        let holds = |needle: &[u8]| recorded.windows(needle.len()).any(|w| w == needle);
        assert!(holds(b"beta.test") && holds(file("beta.der")) && holds(b"tls-echo-24"));
        for p8 in ["alpha.p8", "beta.p8"] {
            let document = file(p8);
            let scalar = scalar(p8);
            for secret in [document, &scalar[..]] {
                let hex = hirsel_core_ids::hex::encode(secret);
                for form in [
                    secret,
                    hex.as_bytes(),
                    hex.to_uppercase().as_bytes(),
                    crate::call::base64(secret).as_bytes(),
                ] {
                    assert!(!holds(form), "the tape holds {p8} as {form:?}");
                }
            }
        }

        // The key files are gone and no client connects: a replay that read
        // a key or the network would fail.
        for p8 in ["alpha.p8", "beta.p8"] {
            std::fs::remove_file(files.join(p8)).unwrap();
        }
        let replay = program(dir.path(), &files, backend, |options| {
            options.runtime = env_record_runtime();
            options.recording = true;
            options.replay = Some(crate::ReplayPlan {
                path: tape.to_str().unwrap().into(),
                diagnose: false,
            });
            options.output = dir.path().join("replay");
        });
        let output = Command::new(replay)
            .args(["sni", "alpha", "--"])
            .stdin(Stdio::null())
            .output()
            .unwrap();
        // Replay checks every write, the name, ALPN index and echo among
        // them, against the recorded one, and emits nothing.
        assert!(
            output.status.success(),
            "{} {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.is_empty());
    }

    /// TL3's server cases at program level: `hello` on a socket from
    /// `host.connect` and on one from a Unix listener, and `accept` with a
    /// key granted `sign(p256)` only, are each `Denied`, and the client sees
    /// no ServerHello.
    #[test]
    fn tl3_server_authority_before_effect() {
        for backend in BACKENDS {
            eprintln!("tl3_server_authority_before_effect on {backend:?}");
            tl3_server_authority_before_effect_on(backend);
        }
    }

    fn tl3_server_authority_before_effect_on(backend: Backend) {
        let mut server = Server::start(backend, &["connect", "alpha", "--"]);
        assert_eq!(server.line(), "hello denied");
        server.finish();

        // The files are shared by both engines' runs: bind afresh.
        let path = files().join("sock/s.sock");
        let _ = std::fs::remove_file(&path);
        let mut server = Server::start(backend, &["unix", "alpha", "--"]);
        let mut unix = std::os::unix::net::UnixStream::connect(&path).unwrap();
        // A ClientHello's record header: hello never reads it, and may
        // already have released the socket, so the write may meet EPIPE.
        let _ = unix.write_all(b"\x16\x03\x01\x00\x00");
        assert_eq!(server.line(), "hello denied");
        server.finish();
        drop(unix);

        let mut server = Server::start(backend, &["fixed", "signer", "--", "alpha.der"]);
        let output = client(server.port, &["-servername", "alpha.test", "-msg"], None);
        assert_eq!(server.until("accept "), "accept denied");
        assert!(!output.contains("ServerHello"), "{output}");
        server.finish();
    }
}
