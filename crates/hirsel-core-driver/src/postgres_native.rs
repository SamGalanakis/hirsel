#[test]
fn postgres_scripted_peer_laws() {
    use hirsel_core_net::{GrantRule, SecretGrant};
    use rustls::{
        ServerConfig, ServerConnection, StreamOwned,
        pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
    };
    use std::{
        io::{ErrorKind, Read, Write},
        net::{Ipv4Addr, TcpListener, TcpStream},
        sync::Arc,
    };

    type Peer = StreamOwned<ServerConnection, TcpStream>;

    const CA: &[u8] = include_bytes!("../../hirsel-core-rt/testdata/tls/ca.pem");
    const LEAF: &[u8] = include_bytes!("../../hirsel-core-rt/testdata/tls/alpha.pem");
    const KEY: &[u8] = include_bytes!("../../hirsel-core-rt/testdata/tls/alpha.key.pem");
    const PASSWORD: &str = "pencil";
    const FIRST: &[u8] = b"y,,n=,r=rOprNGfwEbeRWgbNEkqO";
    const NONCE: &[u8] = b"r=rOprNGfwEbeRWgbNEkqO,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096";
    const EXTENDED: &[u8] =
        b"r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096";
    // RFC 7677's salt, nonce and password with PostgreSQL's empty SCRAM name.
    // Independently derived using PBKDF2, HMAC and SHA-256 from RFC 5802 §3.
    const PROOF: &[u8] = b"c=eSws,r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,p=VpuC5DGQa5ro9tXE9MnKs69NH1nxnuregZZcclqIGfM=";
    const VERIFIER: &[u8] = b"v=FOmOj9BpTGwvnzwBtWQjBaPmVxT9I8IeHBOhcIPu3us=";
    const SQL_ERROR: &[u8] = b"SERROR\0VERROR\0C22012\0Mdivision by zero\0Dscripted detail\0\0";
    // One int4 column. Table, attribute and modifier are unset. Width is
    // four and statement Describe reports format zero per protocol 3.0.
    const DESCRIPTION: &[u8] =
        b"\x00\x01x\0\x00\x00\x00\x00\x00\x00\x00\x00\x00\x17\x00\x04\xff\xff\xff\xff\x00\x00";
    const PORTAL: &[u8] = b"hirsel_rows\0";

    fn packet(out: &mut Vec<u8>, tag: u8, body: &[u8]) {
        let len = u32::try_from(body.len() + 4).unwrap().to_be_bytes();
        out.push(tag);
        out.extend_from_slice(&len);
        out.extend_from_slice(body);
    }

    fn send(peer: &mut Peer, tag: u8, body: &[u8]) {
        let mut out = Vec::new();
        packet(&mut out, tag, body);
        peer.write_all(&out).unwrap();
    }

    fn recv(peer: &mut Peer, tag: u8) -> Vec<u8> {
        let mut header = [0; 5];
        peer.read_exact(&mut header).unwrap();
        assert_eq!(header[0], tag, "expected frontend tag {}", char::from(tag));
        let len = u32::from_be_bytes(header[1..].try_into().unwrap());
        assert!((4..=1024).contains(&len));
        let mut body = vec![0; (len - 4) as usize];
        peer.read_exact(&mut body).unwrap();
        body
    }

    fn ended(peer: &mut impl Read) {
        match peer.read(&mut [0]) {
            Ok(0) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::UnexpectedEof | ErrorKind::ConnectionReset
                ) => {}
            other => panic!("expected stream retirement, got {other:?}"),
        }
    }

    fn authenticate(peer: &mut Peer, name: &str) -> bool {
        const STARTUP: &[u8] =
            b"\x00\x03\x00\x00user\0user\0database\0postgres\0client_encoding\0UTF8\0\0";
        let mut startup = vec![0; 4 + STARTUP.len()];
        peer.read_exact(&mut startup).unwrap();
        assert_eq!(startup[..4], (startup.len() as u32).to_be_bytes());
        assert_eq!(&startup[4..], STARTUP);

        // Send no body for malformed lengths or tags. Any attempted read of the
        // claimed body returns a timeout rather than the expected protocol error.
        let header = match name {
            "short" => Some(*b"R\x00\x00\x00\x03"),
            "negative" => Some(*b"R\xff\xff\xff\xff"),
            "large" => Some(*b"R\x7f\xff\xff\xff"),
            "tag" => Some(*b"?\x00\x00\x00\x04"),
            _ => None,
        };
        if let Some(header) = header {
            peer.write_all(&header[..2]).unwrap();
            peer.write_all(&header[2..]).unwrap();
            ended(peer);
            return false;
        }

        send(peer, b'R', b"\x00\x00\x00\x0aSCRAM-SHA-256\0\0");
        let mut first = b"SCRAM-SHA-256\0".to_vec();
        first.extend_from_slice(&(FIRST.len() as i32).to_be_bytes());
        first.extend_from_slice(FIRST);
        assert_eq!(recv(peer, b'p'), first);

        let mut continuation = vec![0, 0, 0, 11];
        continuation.extend_from_slice(if name == "nonce" { NONCE } else { EXTENDED });
        send(peer, b'R', &continuation);
        if name == "nonce" {
            ended(peer);
            return false;
        }
        assert_eq!(recv(peer, b'p'), PROOF);

        let mut final_message = vec![0, 0, 0, 12];
        final_message.extend_from_slice(VERIFIER);
        if name == "signature" {
            final_message[6] = b'4';
        }
        let mut ready = Vec::new();
        packet(&mut ready, b'R', &final_message);
        packet(&mut ready, b'R', &[0; 4]);
        packet(&mut ready, b'S', b"client_encoding\0UTF8\0");
        packet(&mut ready, b'K', b"\x12\x34\x56\x78\x9a\xbc\xde\xf0");
        packet(&mut ready, b'Z', b"I");
        peer.write_all(&ready).unwrap();
        true
    }

    fn begin(peer: &mut Peer, sql: &str, returns_rows: bool) {
        let mut parse = vec![0];
        parse.extend_from_slice(sql.as_bytes());
        parse.extend_from_slice(&[0; 3]);
        assert_eq!(recv(peer, b'P'), parse);
        assert_eq!(recv(peer, b'D'), b"S\0");
        assert!(recv(peer, b'H').is_empty());
        send(peer, b'1', b"");
        send(peer, b't', b"\0\0");
        if returns_rows {
            send(peer, b'T', DESCRIPTION);
        } else {
            send(peer, b'n', b"");
        }

        let mut bind = PORTAL.to_vec();
        bind.extend_from_slice(b"\0\0\0\0\0");
        bind.extend_from_slice(if returns_rows {
            b"\x00\x01\x00\x01"
        } else {
            b"\0\0"
        });
        assert_eq!(recv(peer, b'B'), bind);
        let mut execute = PORTAL.to_vec();
        execute.extend_from_slice(&16u32.to_be_bytes());
        assert_eq!(recv(peer, b'E'), execute);
        if returns_rows {
            assert!(recv(peer, b'H').is_empty());
        } else {
            expect_close_portal(peer);
            assert!(recv(peer, b'S').is_empty());
        }
        send(peer, b'2', b"");
    }

    fn expect_close_portal(peer: &mut Peer) {
        let mut close = vec![b'P'];
        close.extend_from_slice(PORTAL);
        assert_eq!(recv(peer, b'C'), close);
    }

    fn finish(peer: &mut Peer, status: &[u8], returns_rows: bool) {
        if returns_rows {
            expect_close_portal(peer);
            assert!(recv(peer, b'S').is_empty());
        }
        send(peer, b'3', b"");
        send(peer, b'Z', status);
    }

    fn sync(peer: &mut Peer, status: &[u8]) {
        assert!(recv(peer, b'S').is_empty());
        send(peer, b'Z', status);
    }

    fn recover(peer: &mut Peer) {
        begin(peer, "SELECT 1 / x FROM inputs", true);
        send(peer, b'A', b"\0\0\0\x01channel\0payload\0");
        send(
            peer,
            b'N',
            b"SNOTICE\0VNOTICE\0C00000\0Mscripted notice\0\0",
        );
        send(peer, b'S', b"standard_conforming_strings\0off\0");
        send(peer, b'D', b"\x00\x01\x00\x00\x00\x04\x00\x00\x00\x07");
        send(peer, b'E', SQL_ERROR);
        sync(peer, b"I");

        expect_close_portal(peer);
        send(peer, b'3', b"");
        begin(peer, "SELECT 42", true);
        send(peer, b'D', b"\x00\x01\x00\x00\x00\x04\x00\x00\x00\x2a");
        send(peer, b'C', b"SELECT 1\0");
        finish(peer, b"I", true);

        begin(peer, "SELECT 'late'", true);
        send(peer, b'C', b"SELECT 0\0");
        expect_close_portal(peer);
        assert!(recv(peer, b'S').is_empty());
        send(peer, b'3', b"");
        send(peer, b'E', SQL_ERROR);
        send(peer, b'Z', b"I");

        begin(peer, "BEGIN", false);
        send(peer, b'C', b"BEGIN\0");
        finish(peer, b"T", false);
        begin(peer, "SAVEPOINT recover", false);
        send(peer, b'C', b"SAVEPOINT\0");
        finish(peer, b"T", false);
        begin(peer, "SELECT 1 / x FROM inputs", true);
        send(peer, b'E', SQL_ERROR);
        sync(peer, b"E");

        expect_close_portal(peer);
        send(peer, b'3', b"");
        assert_eq!(recv(peer, b'P'), b"\0SELECT 1\0\0\0");
        assert_eq!(recv(peer, b'D'), b"S\0");
        assert!(recv(peer, b'H').is_empty());
        send(
            peer,
            b'E',
            b"VERROR\0C25P02\0Mcurrent transaction is aborted\0\0",
        );
        sync(peer, b"E");
        begin(peer, "ROLLBACK TO SAVEPOINT recover", false);
        send(peer, b'C', b"ROLLBACK\0");
        finish(peer, b"T", false);
        begin(peer, "SELECT 1", true);
        send(peer, b'D', b"\x00\x01\x00\x00\x00\x04\x00\x00\x00\x01");
        send(peer, b'C', b"SELECT 1\0");
        finish(peer, b"T", true);
        begin(peer, "ROLLBACK", false);
        send(peer, b'C', b"ROLLBACK\0");
        finish(peer, b"I", false);
        assert!(recv(peer, b'X').is_empty());
        ended(peer);
    }

    fn cancel_race(listener: &TcpListener, config: &ServerConfig, original: &mut Peer) {
        // The new stream carries 16, (1234 << 16) | 5678, then exactly
        // the four-byte PID and four-byte key in BackendKeyData (protocol 3.0).
        let socket = crate::loopback::accept_local(listener).unwrap();
        let mut tls = config.clone();
        tls.alpn_protocols = vec![b"postgresql".to_vec()];
        let mut cancel = StreamOwned::new(ServerConnection::new(Arc::new(tls)).unwrap(), socket);
        let mut request = [0; 16];
        cancel.read_exact(&mut request).unwrap();
        assert_eq!(
            request,
            *b"\x00\x00\x00\x10\x04\xd2\x16\x2e\x12\x34\x56\x78\x9a\xbc\xde\xf0"
        );
        cancel.conn.send_close_notify();
        cancel.flush().unwrap();
        ended(&mut cancel);

        begin(original, "SELECT 42", true);
        send(original, b'D', b"\x00\x01\x00\x00\x00\x04\x00\x00\x00\x2a");
        send(original, b'C', b"SELECT 1\0");
        finish(original, b"I", true);
        ended(original);
    }

    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let config =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from_pem_slice(LEAF).unwrap()],
                PrivateKeyDer::from_pem_slice(KEY).unwrap(),
            )
            .unwrap();

    let dir = tempfile::tempdir().unwrap();
    let roots = dir.path().join("roots.pem");
    std::fs::write(&roots, CA).unwrap();
    let source = format!(
        "{}\n{}\n{}\n{}",
        include_str!("../../../core/programs/postgres/postgres.hirsel"),
        include_str!("../../../core/programs/postgres/diagnostic.hirsel"),
        include_str!("../../../core/programs/postgres/exchange.hirsel"),
        include_str!("../../../core/front/fixtures/recording/postgres-peer.hirsel")
            .replace("12345", &port.to_string()),
    );
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../core");
    let mut request = source_request(&source, dir.path());
    request.inputs.packages[1] =
        PackageManifest::directory(&root.join("core"), package("core"), true).unwrap();
    let mut std = PackageManifest::directory(&root.join("std"), package("std"), false).unwrap();
    std.dependencies.push(package("core"));
    request.inputs.packages.push(std);
    request.inputs.packages[0].dependencies.push(package("std"));
    let product = Driver::default()
        .check(request, &mut Meter::new(false))
        .unwrap();
    assert!(
        product.accepted(),
        "{:?} {:?}",
        product.diagnostics,
        product.incomplete
    );

    let mut options = native_options(Backend::Cranelift, dir.path());
    options.grant_mask = ["Clock", "Net"]
        .iter()
        .map(|name| 1 << grant_kind(name).unwrap())
        .sum();
    options.instance_limit = crate::native::DEFAULT_INSTANCE_LIMIT;
    options.net_rules = [
        format!("Connect 127.0.0.1/32 {port}-{port}"),
        format!("Tls alpha.test {port}-{port} System"),
    ]
    .map(|rule| GrantRule::parse(&rule).unwrap())
    .into();
    options.secret_grants = vec![
        SecretGrant::new(
            "password",
            "env:HIRSEL_SCRAM_PASSWORD",
            "tls_send(alpha.test)",
        )
        .unwrap(),
    ];
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let program = product.build(&entry, &options).unwrap();

    let cases = [
        "short",
        "negative",
        "large",
        "tag",
        "nonce",
        "signature",
        "alpn",
        "short-row",
        "trailing",
        "reuse",
        "ssl-refused",
        "ssl-extra",
        "cancel",
    ];
    let peer = std::thread::spawn(move || {
        for name in cases {
            let mut socket = crate::loopback::accept_local(&listener).unwrap();
            if matches!(name, "ssl-refused" | "ssl-extra") {
                // SSLRequest has length 8 and request code (1234 << 16) | 5679.
                let mut request = [0; 8];
                socket.read_exact(&mut request).unwrap();
                assert_eq!(request, *b"\x00\x00\x00\x08\x04\xd2\x16\x2f");
                let reply: &[u8] = if name == "ssl-refused" { b"N" } else { b"S!" };
                socket.write_all(reply).unwrap();
                ended(&mut socket);
                continue;
            }
            let mut tls = config.clone();
            if name != "alpn" {
                tls.alpn_protocols = vec![b"postgresql".to_vec()];
            }
            let mut peer = StreamOwned::new(ServerConnection::new(Arc::new(tls)).unwrap(), socket);
            if name == "alpn" {
                ended(&mut peer);
            } else if authenticate(&mut peer, name) {
                match name {
                    "signature" => ended(&mut peer),
                    "reuse" => recover(&mut peer),
                    "cancel" => cancel_race(&listener, &config, &mut peer),
                    _ => {
                        begin(&mut peer, "SELECT 1", true);
                        let row: &[u8] = if name == "short-row" {
                            b"\x00\x01\x7f\xff\xff\xff"
                        } else {
                            b"\x00\x01\x00\x00\x00\x04\x00\x00\x00\x01\x99"
                        };
                        send(&mut peer, b'D', row);
                        ended(&mut peer);
                    }
                }
            }
        }
    });
    for name in cases {
        let outcome = Command::new(&program.executable)
            .arg(name)
            .env("SSL_CERT_FILE", &roots)
            .env_remove("SSL_CERT_DIR")
            .env("HIRSEL_SCRAM_PASSWORD", PASSWORD)
            .output()
            .unwrap();
        assert!(
            outcome.status.success(),
            "{name}: {} {}",
            outcome.status,
            String::from_utf8_lossy(&outcome.stderr)
        );
    }
    peer.join().unwrap();
}
