//! TLS-9's server half against the OS provider, with a rustls client: the
//! TL3 and TL7 server cases, TL19's before-issue chain cases and its key
//! kinds, and TL21. The openssl-client laws are in tools/tests/tls_serve.py.
use super::*;
use crate::tls_server::{
    hirsel_core_host_tls_hello, hirsel_core_tls_accept, hirsel_core_tls_server_name,
};
use rustls::{ClientConfig, ClientConnection, RootCertStore, pki_types::ServerName};

macro_rules! fixtures {
    ($($name:literal),*) => {
        fn fixture(name: &str) -> Vec<u8> {
            match name {
                $($name => include_bytes!(concat!("../../../testdata/tls/server/", $name)).to_vec(),)*
                _ => panic!("no fixture {name}"),
            }
        }
    };
}

fixtures!(
    "ca.pem",
    "pad-over.der",
    "alpha.der",
    "alpha.p8",
    "beta.der",
    "beta.p8",
    "alpha-ed25519.der",
    "alpha-ed25519.p8",
    "alpha-rsa2048.der",
    "alpha-rsa2048.p8",
    "alpha-p384.der",
    "alpha-p384.p8",
    "alpha-rsa1024.der",
    "alpha-rsa1024.p8"
);

/// What the client saw: the handshake's failure, or its ALPN, the leaf the
/// server sent and the application bytes before the close.
type Seen = Result<(Option<Vec<u8>>, Vec<u8>, Vec<u8>), rustls::Error>;

struct Client {
    port: u16,
    peer: thread::JoinHandle<Seen>,
}

/// A rustls client trusting the server CA, sending `sni` (none when `None`)
/// and offering `alpn`; it verifies the leaf for alpha.test without SNI.
fn client(port: u16, sni: Option<&'static str>, alpn: &[&[u8]]) -> Client {
    let mut roots = RootCertStore::empty();
    roots
        .add(CertificateDer::from_pem_slice(&fixture("ca.pem")).unwrap())
        .unwrap();
    let mut config =
        ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
    config.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
    config.enable_sni = sni.is_some();
    let name = ServerName::try_from(sni.unwrap_or("alpha.test")).unwrap();
    let socket = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
    let local = socket.local_addr().unwrap().port();
    let peer = thread::spawn(move || {
        let connection = ClientConnection::new(Arc::new(config), name).unwrap();
        let mut stream = StreamOwned::new(connection, socket);
        while stream.conn.is_handshaking() {
            if let Err(error) = stream.conn.complete_io(&mut stream.sock) {
                let text = format!("{error:?}");
                return Err(error
                    .into_inner()
                    .and_then(|inner| inner.downcast::<rustls::Error>().ok())
                    .map_or(rustls::Error::General(text), |e| *e));
            }
        }
        let protocol = stream.conn.alpn_protocol().map(<[u8]>::to_vec);
        let leaf = stream.conn.peer_certificates().unwrap()[0].to_vec();
        let mut received = Vec::new();
        let _ = stream.read_to_end(&mut received);
        Ok((protocol, leaf, received))
    });
    Client { port: local, peer }
}

fn list(items: &[Buffer]) -> Buffer {
    Buffer {
        data: items.as_ptr().cast_mut().cast(),
        len: items.len(),
        capacity: 0,
    }
}

fn alert(description: AlertDescription) -> Result<(), rustls::Error> {
    Err(rustls::Error::AlertReceived(description))
}

struct Server {
    listener: u64,
    port: u16,
}

impl Run {
    fn serving(&mut self) -> Server {
        let socket = unsafe {
            hirsel_core_host_listen(&raw mut *self.context, self.net, 0, 0, 0x7f000001, 0, 8, 1)
        };
        assert_eq!(socket.error, IoError::OK);
        Server {
            listener: socket.value,
            port: socket.port,
        }
    }

    fn take(&mut self, server: &Server, client: &Client) -> u64 {
        let accepted = unsafe {
            crate::net_tests::accept_from_port(
                &raw mut *self.context,
                server.listener,
                client.port,
                1,
            )
        };
        accepted.value
    }

    fn hello(&mut self, key: u64) -> Outcome {
        unsafe { hirsel_core_host_tls_hello(&raw mut *self.context, key, 0, 0, 0, 20) }
    }

    fn server_name(&mut self, hello: u64) -> Vec<u8> {
        let outcome = unsafe { hirsel_core_tls_server_name(&raw mut *self.context, hello, 0) };
        unsafe { crate::tls::bytes(&*(outcome.value as *const crate::tls::Buffer)) }.to_vec()
    }

    fn serve(&mut self, hello: u64, chain: &[&[u8]], secret: u64, alpn: &[&[u8]]) -> Outcome {
        let chain: Vec<_> = chain.iter().map(|der| input(der)).collect();
        let alpn: Vec<_> = alpn.iter().map(|p| input(p)).collect();
        unsafe {
            hirsel_core_tls_accept(
                &raw mut *self.context,
                hello,
                &list(&chain),
                secret,
                &list(&alpn),
                0,
                0,
                0,
                21,
            )
        }
    }

    fn serving_key(&mut self, name: &str, uses: &[u8]) -> u64 {
        self.seal_secret(&fixture(&format!("{name}.p8")), uses)
    }
}

/// A served handshake: hello, then accept with `name`'s leaf and key, then
/// `hi` and the close. The leaf is what the client received.
fn served(run: &mut Run, server: &Server, name: &str, offered: &[&[u8]], alpn: &[&[u8]]) -> u64 {
    let client = client(server.port, Some("alpha.test"), offered);
    let key = run.take(server, &client);
    let hello = run.hello(key);
    assert_eq!(hello.error, IoError::OK);
    assert_eq!(run.server_name(hello.value), b"alpha.test");
    let leaf = fixture(&format!("{name}.der"));
    let secret = run.serving_key(name, b"tls_serve");
    let accepted = run.serve(hello.value, &[&leaf], secret, alpn);
    assert_eq!(accepted.error, IoError::OK, "{name}");
    let sent = unsafe {
        hirsel_core_host_send_pair(
            &raw mut *run.context,
            accepted.value | 2,
            &input(b"hi"),
            &input(b""),
            0,
            0,
            0,
            22,
        )
    };
    assert_eq!((sent.error, sent.value), (IoError::OK, 2));
    run.release_halves(accepted.value);
    let (protocol, got, received) = client.peer.join().unwrap().unwrap();
    assert_eq!(got, leaf, "{name}");
    assert_eq!(received, b"hi");
    // RFC 7301 §3.2 with server preference: written is the index plus one.
    let expected = protocol.map_or(0, |p| alpn.iter().position(|a| *a == p).unwrap() as u64 + 1);
    assert_eq!(accepted.written, expected);
    accepted.written
}

/// A refusal at `accept`: the verdict, the client's alert, and the
/// descriptor count back to its value before the connection (TL7).
fn refused(run: &mut Run, server: &Server, chain: &[&[u8]], secret: u64, expected: Outcome) {
    let before = fds();
    let client = client(server.port, Some("alpha.test"), &[]);
    let key = run.take(server, &client);
    let hello = run.hello(key);
    assert_eq!(hello.error, IoError::OK);
    let outcome = run.serve(hello.value, chain, secret, &[]);
    assert_error(outcome, expected);
    assert_eq!(
        client.peer.join().unwrap().map(|_| ()),
        alert(AlertDescription::InternalError)
    );
    assert_eq!(fds(), before);
}

fn handshakes(run: &mut Run, server: &Server) {
    let (h2, http) = (b"h2".as_slice(), b"http/1.1".as_slice());
    assert_eq!(served(run, server, "alpha", &[h2, http], &[http, h2]), 1);
    assert_eq!(served(run, server, "alpha", &[h2], &[http, h2]), 2);
    assert_eq!(served(run, server, "alpha", &[], &[http, h2]), 0);
    assert_eq!(served(run, server, "alpha", &[h2], &[]), 0);
    // TL18's other kinds.
    served(run, server, "alpha-ed25519", &[], &[]);
    served(run, server, "alpha-rsa2048", &[], &[]);
    // A client with no SNI reads as the empty name.
    let client = client(server.port, None, &[]);
    let key = run.take(server, &client);
    let hello = run.hello(key);
    assert_eq!(hello.error, IoError::OK);
    assert_eq!(run.server_name(hello.value), b"");
    let leaf = fixture("alpha.der");
    let secret = run.serving_key("alpha", b"tls_serve");
    let accepted = run.serve(hello.value, &[&leaf], secret, &[]);
    assert_eq!(accepted.error, IoError::OK);
    // TL21 with no SNI: the server half authorizes no secret.
    server_half_refusals(run, accepted.value);
    run.release_halves(accepted.value);
    let (_, got, _) = client.peer.join().unwrap().unwrap();
    assert_eq!(got, leaf);
}

/// TL21: on a server's write half, `send_secret` (with and without
/// secrets), SCRAM and `scram_final_len` are `Denied` with no provider call.
fn server_half_refusals(run: &mut Run, key: u64) {
    let named = run.seal_secret(b"BCD", b"tls_send(alpha.test)");
    let words = |words: &[u64]| Buffer {
        data: words.as_ptr().cast_mut().cast(),
        len: words.len(),
        capacity: 0,
    };
    crate::os::probe::CALLS.set(Some(Vec::new()));
    for secrets in [&[named][..], &[]] {
        let marks: &[u64] = if secrets.is_empty() {
            &[]
        } else {
            &[1, 0, 0, 0]
        };
        let sent = unsafe {
            hirsel_core_host_send_secret(
                &raw mut *run.context,
                key | 2,
                &input(b"AE"),
                &words(secrets),
                &words(marks),
                0,
                0,
                0,
                23,
            )
        };
        assert_eq!(sent.error, DENIED.error);
    }
    assert_eq!(
        run.scram_send(
            key,
            named,
            b"n,,n=user,r=abc",
            b"r=abcdef,s=QSXCR+Q6sek8bf92,i=4096"
        )
        .error,
        DENIED.error
    );
    assert_eq!(
        run.scram_len(
            key,
            b"n,,n=user,r=abc",
            b"r=abcdef,s=QSXCR+Q6sek8bf92,i=4096"
        )
        .error,
        DENIED.error
    );
    let calls = crate::os::probe::CALLS.take().unwrap();
    assert!(calls.is_empty(), "{calls:?}");
}

fn tl21(run: &mut Run, server: &Server) {
    let client = client(server.port, Some("alpha.test"), &[]);
    let key = run.take(server, &client);
    let hello = run.hello(key);
    let leaf = fixture("alpha.der");
    let secret = run.serving_key("alpha", b"tls_serve");
    let accepted = run.serve(hello.value, &[&leaf], secret, &[]);
    assert_eq!(accepted.error, IoError::OK);
    server_half_refusals(run, accepted.value);
    let context = &raw mut *run.context;
    let joined =
        unsafe { hirsel_core_net_join(context, accepted.value | 2, accepted.value | 1, 24) };
    assert_ne!(joined.error, IoError::OK);
    run.release_halves(accepted.value);
    let (_, _, received) = client.peer.join().unwrap().unwrap();
    assert_eq!(received, b"");
}

/// TL19 and TL7: chains refused before issue, and keys refused at it.
fn refusals(run: &mut Run, server: &Server) {
    let (alpha, over) = (fixture("alpha.der"), fixture("pad-over.der"));
    let key = run.serving_key("alpha", b"tls_serve");
    let mut trailing = alpha.clone();
    trailing.push(0);
    let nine = [alpha.as_slice(); 9];
    for chain in [&[][..], &nine, &[&trailing], &[&alpha, &over]] {
        crate::os::probe::CALLS.set(Some(Vec::new()));
        refused(run, server, chain, key, tls_failure(7, 0));
        let calls = crate::os::probe::CALLS.take().unwrap();
        assert!(calls.iter().all(|call| call.1 != TLS_ACCEPT), "{calls:?}");
    }
    // Alpha's chain with beta's key, a key that is not PKCS#8, and the two
    // kinds `sign` does not hold: P-384 and RSA-1024.
    let beta_key = run.serving_key("beta", b"tls_serve");
    refused(run, server, &[&alpha], beta_key, tls_failure(8, 0));
    let raw = run.seal_secret(&fixture("alpha.der"), b"tls_serve");
    refused(run, server, &[&alpha], raw, tls_failure(8, 0));
    for name in ["alpha-p384", "alpha-rsa1024"] {
        let key = run.serving_key(name, b"tls_serve");
        let leaf = fixture(&format!("{name}.der"));
        refused(run, server, &[&leaf], key, tls_failure(8, 0));
    }
    // TL3: a key granted sign(p256) without tls_serve.
    let signing = run.serving_key("alpha", b"sign(p256)");
    crate::os::probe::CALLS.set(Some(Vec::new()));
    refused(run, server, &[&alpha], signing, DENIED);
    let calls = crate::os::probe::CALLS.take().unwrap();
    assert!(calls.iter().all(|call| call.1 != TLS_ACCEPT), "{calls:?}");
}

/// TL3: `hello` takes only a socket a host TCP listener accepted.
fn hello_authority(run: &mut Run) {
    let tcp = listener();
    let port = tcp.local_addr().unwrap().port();
    let key = run.connect(port);
    let _peer = crate::loopback::accept_local(&tcp).unwrap();
    crate::os::probe::CALLS.set(Some(Vec::new()));
    let hello = run.hello(key);
    let calls = crate::os::probe::CALLS.take().unwrap();
    assert_error(hello, DENIED);
    assert!(calls.is_empty(), "{calls:?}");
}

struct Parked {
    key: u64,
    outcome: Outcome,
}

unsafe extern "C" fn parked_hello(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
    unsafe {
        let scenario = input.cast::<*mut Parked>().read();
        (*scenario).outcome = hirsel_core_host_tls_hello(context, (*scenario).key, 0, 0, 0, 26);
    }
    0
}

unsafe extern "C" fn cancel_hello(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
    unsafe {
        let mut scenario = input.cast::<*mut Parked>().read();
        let child = hirsel_core_spawn(
            context,
            0,
            8192,
            (&raw mut scenario).cast(),
            parked_hello,
            crate::tasks_tests::layout::<*mut Parked, ()>(0),
            5,
        );
        assert!(!child.task.is_null());
        checkpoint();
        assert!(task_waits_for_provider(child.task));
        hirsel_core_cancel(context, child.task);
        let joined = hirsel_core_join(context, child.task, ptr::null_mut(), 5);
        assert_eq!((joined.kind, joined.status), (0, 0));
    }
    0
}

/// TL7: cancel during `hello` and a deadline during it on a client that
/// never sends, a deadline during `accept` on a client that never finishes,
/// and a dropped `Hello`: each leaves the descriptor count where it was.
fn release(run: &mut Run, server: &Server) {
    let before = fds();
    let accept_silent = |run: &mut Run| {
        let silent = TcpStream::connect((Ipv4Addr::LOCALHOST, server.port)).unwrap();
        let port = silent.local_addr().unwrap().port();
        let accepted = unsafe {
            crate::net_tests::accept_from_port(&raw mut *run.context, server.listener, port, 1)
        };
        (silent, accepted.value)
    };
    let (silent, key) = accept_silent(run);
    let at = run.now() + 2_000_000;
    let timed =
        unsafe { hirsel_core_host_tls_hello(&raw mut *run.context, key, 1, run.clock, at, 25) };
    assert_eq!(timed.error.tag, 6);
    drop(silent);
    assert_eq!(fds(), before);

    let (silent, key) = accept_silent(run);
    let mut scenario = Parked {
        key,
        outcome: Outcome::default(),
    };
    let mut parked = &raw mut scenario;
    unsafe {
        crate::causes::task_start(&mut run.context).unwrap();
        let status = run_root(
            &raw mut *run.context,
            cancel_hello,
            (&raw mut parked).cast(),
            ptr::null_mut(),
            size_of::<*mut Parked>(),
            0,
            SchedulerProvider::default(),
        );
        assert_eq!(status, 0);
    }
    assert_eq!(scenario.outcome.error.tag, 7);
    drop(silent);
    assert_eq!(fds(), before);

    // A client that sends its ClientHello and never its Finished.
    let mut roots = RootCertStore::empty();
    roots
        .add(CertificateDer::from_pem_slice(&fixture("ca.pem")).unwrap())
        .unwrap();
    let config =
        ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
    let mut stalled = ClientConnection::new(
        Arc::new(config),
        ServerName::try_from("alpha.test").unwrap(),
    )
    .unwrap();
    let (mut socket, key) = accept_silent(run);
    stalled.write_tls(&mut socket).unwrap();
    let hello = run.hello(key);
    assert_eq!(hello.error, IoError::OK);
    let leaf = fixture("alpha.der");
    let secret = run.serving_key("alpha", b"tls_serve");
    let chain = [input(&leaf)];
    let at = run.now() + 20_000_000;
    let timed = unsafe {
        crate::tls_server::hirsel_core_tls_accept(
            &raw mut *run.context,
            hello.value,
            &list(&chain),
            secret,
            &list(&[]),
            1,
            run.clock,
            at,
            27,
        )
    };
    assert_eq!(timed.error.tag, 6);
    drop(socket);
    assert_eq!(fds(), before);

    let client = client(server.port, Some("gamma.test"), &[]);
    let key = run.take(server, &client);
    let hello = run.hello(key);
    assert_eq!(run.server_name(hello.value), b"gamma.test");
    run.release(hello.value);
    assert_eq!(
        client.peer.join().unwrap().map(|_| ()),
        alert(AlertDescription::UnrecognisedName)
    );
    assert_eq!(fds(), before);
}

#[test]
fn tls_server_loopback_laws() {
    let mut run = Run::new();
    let server = run.serving();
    let baseline = fds();
    hello_authority(&mut run);
    handshakes(&mut run, &server);
    tl21(&mut run, &server);
    refusals(&mut run, &server);
    release(&mut run, &server);
    assert_eq!(fds(), baseline);
    assert!(run.files.tls_hellos.is_empty());
    assert!(run.files.tls_sessions.is_empty());
    run.release(server.listener);
}
