use super::*;
use crate::{entry::*, host_file::Files, net::*, tasks::*};
use hirsel_core_net::{Rule, RuleView, TlsRule};
use rustls::{
    ServerConfig, ServerConnection, StreamOwned,
    pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
};
use std::{
    io::{Read, Write},
    net::{Ipv4Addr, TcpListener, TcpStream},
    sync::{Arc, mpsc},
    thread,
};

const CA: &[u8] = include_bytes!("../../testdata/tls/ca.pem");
const UNRELATED: &[u8] = include_bytes!("../../testdata/tls/unrelated-ca.pem");
const ALPHA: &[u8] = include_bytes!("../../testdata/tls/alpha.pem");
const ALPHA_KEY: &[u8] = include_bytes!("../../testdata/tls/alpha.key.pem");
const ALPHA_DIGEST: &str = include_str!("../../testdata/tls/alpha.sha256");

struct Run {
    os: *mut OsProcess,
    files: Box<Files>,
    context: Box<Context>,
    net: u64,
    clock: u64,
    pin: String,
    rules: Vec<RulesStorage>,
}

impl Run {
    fn new() -> Self {
        let pin = CA_PIN.to_owned();
        let rule = TlsRule::parse(&format!(
            "Tls alpha.test,beta.test 0-65535 Roots=sha256:{pin}"
        ))
        .unwrap();
        let patterns = rule.pattern_text();
        let address = |text| RuleView::address(Rule::parse(text).unwrap());
        let rules = [
            address("Connect 127.0.0.0/8 1-65535"),
            address("Listen 0.0.0.0/0 0-65535"),
            RuleView::tls(&rule, &patterns),
            RuleView::tls(
                &TlsRule::parse("Tls alpha.test 0-65535 System").unwrap(),
                "alpha.test",
            ),
        ];
        let config = OsConfig {
            root_limit: 1 << 20,
            args: ptr::null(),
            args_len: 0,
            directories: ptr::null(),
            directories_len: 0,
            grant_mask: (1 << NET) | (1 << CLOCK),
            stdout_fd: 1,
            stderr_fd: 2,
            parameters: ptr::null(),
            parameters_len: 0,
            net_rules: rules.as_ptr(),
            net_rules_len: rules.len(),
            directory_ops: ptr::null(),
            process_ops: ptr::null(),
            socket_activation: ptr::null(),
            target: view(b"test-target"),
        };
        unsafe {
            let os = hirsel_core_os_process_new(&config);
            assert!(!os.process.is_null());
            let run = hirsel_core_os_process_config(os.process);
            let mut files = Box::new(Files::default());
            // Initialize the per-run reactor before measuring descriptor counts.
            files.sockets.poller().unwrap();
            let mut context = Box::new(Context::new(1 << 20));
            context.host_files = &raw mut *files;
            context.run = run;
            Self {
                os: os.process,
                files,
                context,
                net: hirsel_core_entry_grant(run, NET, 0),
                clock: hirsel_core_entry_grant(run, CLOCK, 0),
                pin,
                rules: Vec::new(),
            }
        }
    }

    fn grant(&mut self, rule: &str) -> u64 {
        let storage = RulesStorage::new([GrantRule::parse(rule).unwrap()]);
        let handle = storage.handle();
        self.rules.push(storage);
        let provider = unsafe { *hirsel_core_capability_provider(&*self.context, self.net) };
        self.files.registry.grant(provider, handle)
    }

    fn connect(&mut self, port: u16) -> u64 {
        let out = unsafe {
            hirsel_core_host_connect(
                &raw mut *self.context,
                self.net,
                0,
                0,
                0x7f000001,
                port,
                0,
                0,
                0,
                1,
            )
        };
        assert_eq!(out.error, IoError::OK);
        out.value
    }

    fn upgrade(
        &mut self,
        key: u64,
        name: &[u8],
        roots: Option<&[u8]>,
        alpn: &[&[u8]],
    ) -> ConnectOutcome {
        unsafe {
            upgrade(
                &raw mut *self.context,
                self.net,
                key,
                name,
                roots,
                alpn,
                0,
                0,
                0,
            )
        }
    }

    fn now(&mut self) -> u64 {
        let provider = unsafe { hirsel_core_capability_provider(&*self.context, self.clock) };
        unsafe { crate::os::monotonic_ns().saturating_sub(crate::os::epoch((*provider).state)) }
    }

    fn release(&mut self, key: u64) {
        unsafe { hirsel_core_socket_drop(&raw mut *self.context, key) };
    }

    fn release_halves(&mut self, key: u64) {
        self.release(key | 2);
        self.release(key | 1);
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        self.context.run = ptr::null();
        unsafe { hirsel_core_os_process_free(self.os) };
    }
}

const CA_PIN: &str = "d4c560070b962c766dec951c8ce6a9fba6869ad862f84e349cd97626d5c83cc7";

fn view(bytes: &[u8]) -> View {
    View {
        data: bytes.as_ptr(),
        len: bytes.len(),
    }
}

fn input(bytes: &[u8]) -> Buffer {
    Buffer {
        data: bytes.as_ptr().cast_mut(),
        len: bytes.len(),
        capacity: 0,
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn upgrade(
    context: *mut Context,
    net: u64,
    key: u64,
    name: &[u8],
    roots: Option<&[u8]>,
    alpn: &[&[u8]],
    until: u8,
    clock: u64,
    at: u64,
) -> ConnectOutcome {
    let offered: Vec<_> = alpn.iter().map(|bytes| input(bytes)).collect();
    let carrier = Buffer {
        data: offered.as_ptr().cast_mut().cast(),
        len: offered.len(),
        capacity: 0,
    };
    unsafe {
        hirsel_core_tls_connect(
            context,
            net,
            key,
            &input(name),
            u8::from(roots.is_some()),
            &input(roots.unwrap_or_default()),
            &carrier,
            until,
            clock,
            at,
            2,
        )
    }
}

fn certificate_config(protocols: &[&[u8]]) -> ServerConfig {
    let cert = CertificateDer::from_pem_slice(ALPHA).unwrap();
    let key = PrivateKeyDer::from_pem_slice(ALPHA_KEY).unwrap();
    let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(vec![cert], key)
    .unwrap();
    config.alpn_protocols = protocols.iter().map(|p| p.to_vec()).collect();
    config
}

fn listener() -> TcpListener {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap()
}

fn serve<T: Send + 'static>(
    listener: TcpListener,
    config: ServerConfig,
    handle: impl FnOnce(StreamOwned<ServerConnection, TcpStream>) -> T + Send + 'static,
) -> (u16, thread::JoinHandle<T>) {
    let port = listener.local_addr().unwrap().port();
    let peer = thread::spawn(move || {
        let (socket, _) = listener.accept().unwrap();
        let server = ServerConnection::new(Arc::new(config)).unwrap();
        handle(StreamOwned::new(server, socket))
    });
    (port, peer)
}

fn server_handshake(server: &mut StreamOwned<ServerConnection, TcpStream>) -> std::io::Result<()> {
    while server.conn.is_handshaking() {
        server.conn.complete_io(&mut server.sock)?;
    }
    Ok(())
}

fn fds() -> usize {
    std::fs::read_dir("/proc/self/fd").unwrap().count()
}

fn assert_error(outcome: Outcome, expected: Outcome) {
    assert_eq!(outcome.status, 0);
    assert_eq!(outcome.error, expected.error);
}

fn cert_case(run: &mut Run, name: &[u8], roots: Option<&[u8]>, error: Option<Outcome>) {
    let (port, peer) = serve(listener(), certificate_config(&[]), |mut server| {
        server_handshake(&mut server).is_ok()
    });
    let before = fds();
    let key = run.connect(port);
    let result = run.upgrade(key, name, roots, &[]);
    if let Some(expected) = error {
        assert_error(result.outcome, expected);
    } else {
        assert_eq!(result.outcome.error, IoError::OK);
        assert_eq!(result.outcome.written, 0);
        // TL1: four words from openssl x509 -outform der | sha256sum.
        let digest = ALPHA_DIGEST.trim();
        let expected =
            std::array::from_fn(|i| u64::from_str_radix(&digest[i * 16..i * 16 + 16], 16).unwrap());
        assert_eq!(result.peer, expected);
        run.release_halves(result.outcome.value);
    }
    assert_eq!(peer.join().unwrap(), error.is_none());
    // The peer's listener and accepted descriptor may disappear concurrently;
    // the remaining descriptors are exactly the run baseline.
    assert!(fds() < before);
    assert!(run.files.tls.sessions.is_empty());
}

fn validate_no_issue(
    run: &mut Run,
    key: u64,
    name: &[u8],
    roots: Option<&[u8]>,
    alpn: &[&[u8]],
    expected: Outcome,
) {
    crate::os::probe::CALLS.set(Some(Vec::new()));
    let outcome = run.upgrade(key, name, roots, alpn).outcome;
    let calls = crate::os::probe::CALLS.take().unwrap();
    assert_error(outcome, expected);
    assert!(calls.is_empty(), "{calls:?}");
}

fn before_issue(run: &mut Run) {
    let listener = listener();
    let port = listener.local_addr().unwrap().port();
    let original = run.net;
    let pinned = run.grant(&format!(
        "Tls *.a.test,x.a.test {port}-{port} Roots=sha256:{}",
        run.pin
    ));
    for name in [b"a.test".as_slice(), b"xa.test"] {
        run.net = original;
        let key = run.connect(port);
        let (tcp, _) = listener.accept().unwrap();
        let before = fds();
        run.net = pinned;
        validate_no_issue(run, key, name, Some(CA), &[], DENIED);
        assert_eq!(fds() + 1, before);
        drop(tcp);
    }
    for (name, roots) in [
        (b"x.a.test".as_slice(), None),
        (b"X.A.TEST.".as_slice(), Some(UNRELATED)),
    ] {
        run.net = original;
        let key = run.connect(port);
        let _tcp = listener.accept().unwrap();
        run.net = pinned;
        validate_no_issue(run, key, name, roots, &[], DENIED);
    }
    let other = if port == u16::MAX { port - 1 } else { port + 1 };
    let wrong_port = run.grant(&format!(
        "Tls *.a.test {other}-{other} Roots=sha256:{}",
        run.pin
    ));
    run.net = original;
    let key = run.connect(port);
    let _tcp = listener.accept().unwrap();
    run.net = wrong_port;
    validate_no_issue(run, key, b"y.x.a.test", Some(CA), &[], DENIED);
    run.net = original;
    let socket = unsafe {
        hirsel_core_host_listen(&raw mut *run.context, original, 0, 0, 0x7f000001, 0, 8, 1)
    };
    assert_eq!(socket.error, IoError::OK);
    let _peer = TcpStream::connect((Ipv4Addr::LOCALHOST, socket.port)).unwrap();
    let accepted =
        unsafe { hirsel_core_host_accept(&raw mut *run.context, socket.value, 0, 0, 0, 1) };
    assert_eq!(accepted.error, IoError::OK);
    validate_no_issue(run, accepted.value, b"alpha.test", Some(CA), &[], DENIED);
    run.release(socket.value);

    for malformed in [
        b"".as_slice(),
        b"-----BEGIN CERTIFICATE-----\n!!\n-----END CERTIFICATE-----",
    ] {
        let key = run.connect(port);
        let _tcp = listener.accept().unwrap();
        validate_no_issue(
            run,
            key,
            b"alpha.test",
            Some(malformed),
            &[],
            tls_failure(3, 0),
        );
    }
    let long = [b'x'; 256];
    for alpn in [
        vec![b"".as_slice()],
        vec![&long[..]],
        vec![b"h2".as_slice(), b"h2"],
    ] {
        let key = run.connect(port);
        let _tcp = listener.accept().unwrap();
        validate_no_issue(run, key, b"alpha.test", Some(CA), &alpn, tls_failure(2, 0));
    }
}

fn alpn(run: &mut Run) {
    for (protocol, expected) in [(b"http/1.1".as_slice(), true), (b"x".as_slice(), false)] {
        let (port, peer) = serve(listener(), certificate_config(&[protocol]), |mut server| {
            server_handshake(&mut server).is_ok()
        });
        let key = run.connect(port);
        let result = run.upgrade(key, b"alpha.test", Some(CA), &[b"h2", b"http/1.1"]);
        if expected {
            assert_eq!(result.outcome.error, IoError::OK);
            assert_eq!(result.outcome.written, 2); // FORM bridge encodes Some(1) as 2.
            run.release_halves(result.outcome.value);
        } else {
            assert_error(result.outcome, tls_failure(4, 120));
        }
        assert_eq!(peer.join().unwrap(), expected);
    }
}

fn truncated(run: &mut Run) {
    for clean in [false, true] {
        let (port, peer) = serve(listener(), certificate_config(&[]), move |mut server| {
            server.write_all(b"half").unwrap();
            server.flush().unwrap();
            if clean {
                server.conn.send_close_notify();
                server.flush().unwrap();
            }
        });
        let key = run.connect(port);
        let result = run.upgrade(key, b"alpha.test", Some(CA), &[]);
        assert_eq!(result.outcome.error, IoError::OK);
        let mut bytes = [0; 16];
        let mut buffer = Buffer {
            data: bytes.as_mut_ptr(),
            len: 0,
            capacity: bytes.len(),
        };
        for _ in 0..3 {
            let out = unsafe {
                hirsel_core_host_recv_bounded(
                    &raw mut *run.context,
                    key | 1,
                    &mut buffer,
                    16,
                    0,
                    0,
                    0,
                    3,
                )
            };
            if buffer.len < 4 {
                assert_eq!(out.error, IoError::OK);
                continue;
            }
            if out.value == 0 {
                assert_error(
                    out,
                    if clean {
                        Outcome::default()
                    } else {
                        tls_failure(6, 0)
                    },
                );
            } else {
                assert_eq!(out.value, 4);
            }
        }
        assert_eq!(&bytes[..buffer.len], b"half");
        run.release_halves(key);
        peer.join().unwrap();
    }
}

fn committed_prefix(run: &mut Run) {
    const LEN: usize = 1 << 20;
    let (read, start) = mpsc::channel();
    let (port, peer) = serve(listener(), certificate_config(&[]), move |mut server| {
        server_handshake(&mut server).unwrap();
        start.recv().unwrap();
        let mut all = Vec::with_capacity(LEN);
        let mut chunk = [0; 4096];
        loop {
            let n = match server.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => n,
                Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(error) => panic!("{error}"),
            };
            all.extend_from_slice(&chunk[..n]);
            thread::sleep(std::time::Duration::from_micros(100));
        }
        all
    });
    let key = run.connect(port);
    let send_buffer = 4096i32;
    unsafe {
        assert_eq!(
            libc::setsockopt(
                run.files.sockets.fd(key),
                libc::SOL_SOCKET,
                libc::SO_SNDBUF,
                (&raw const send_buffer).cast(),
                size_of::<i32>() as libc::socklen_t
            ),
            0
        );
    }
    let result = run.upgrade(key, b"alpha.test", Some(CA), &[]);
    assert_eq!(result.outcome.error, IoError::OK);
    let pattern: Vec<_> = (0..LEN).map(|i| (i % 251) as u8).collect();
    let mut start = Some(read);
    let mut offset = 0;
    while offset < LEN {
        let at = if start.is_some() {
            run.now()
        } else {
            run.now() + 2_000_000
        };
        let rest = &pattern[offset..];
        let (prefix, tail) = rest.split_at(rest.len().min(8192));
        let out = unsafe {
            hirsel_core_host_send_pair(
                &raw mut *run.context,
                key | 2,
                &input(prefix),
                &input(tail),
                1,
                run.clock,
                at,
                4,
            )
        };
        if out.error.tag == 6 {
            assert_eq!(out.value, 0);
            assert!(offset > 0 && offset < LEN);
            if let Some(start) = start.take() {
                start.send(()).unwrap();
            }
        } else {
            assert_eq!(out.error, IoError::OK);
            assert!(out.value > 0);
            offset += out.value as usize;
        }
    }
    assert!(start.is_none(), "peer paused until a send deadline passed");
    // An empty send flushes the final committed record before one-shot drop.
    let out = unsafe {
        hirsel_core_host_send_pair(
            &raw mut *run.context,
            key | 2,
            &input(&[]),
            &input(&[]),
            0,
            0,
            0,
            4,
        )
    };
    assert_eq!(out.error, IoError::OK);
    run.release_halves(key);
    assert_eq!(peer.join().unwrap(), pattern);
}

struct Cancel {
    net: u64,
    key: u64,
    outcome: Outcome,
}

unsafe extern "C" fn child(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
    unsafe {
        let scenario = input.cast::<*mut Cancel>().read();
        (*scenario).outcome = upgrade(
            context,
            (*scenario).net,
            (*scenario).key,
            b"alpha.test",
            Some(CA),
            &[],
            0,
            0,
            0,
        )
        .outcome;
    }
    0
}

unsafe extern "C" fn cancel_handshake(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
    unsafe {
        let mut scenario = input.cast::<*mut Cancel>().read();
        let child = hirsel_core_spawn(
            context,
            0,
            8192,
            (&raw mut scenario).cast(),
            child,
            crate::tasks_tests::layout::<*mut Cancel, ()>(0),
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

fn release_on_exit(run: &mut Run) {
    let listener = listener();
    let port = listener.local_addr().unwrap().port();
    let before = fds();
    let key = run.connect(port);
    let (peer, _) = listener.accept().unwrap();
    let at = run.now() + 2_000_000;
    let timed = unsafe {
        upgrade(
            &raw mut *run.context,
            run.net,
            key,
            b"alpha.test",
            Some(CA),
            &[],
            1,
            run.clock,
            at,
        )
    };
    assert_eq!(timed.outcome.error.tag, 6);
    drop(peer);
    assert_eq!(fds(), before);

    let key = run.connect(port);
    let (peer, _) = listener.accept().unwrap();
    let mut scenario = Cancel {
        net: run.net,
        key,
        outcome: Outcome::default(),
    };
    let mut input = &raw mut scenario;
    unsafe {
        crate::causes::task_start(&mut run.context).unwrap();
        let status = run_root(
            &raw mut *run.context,
            cancel_handshake,
            (&raw mut input).cast(),
            ptr::null_mut(),
            size_of::<*mut Cancel>(),
            0,
            SchedulerProvider::default(),
        );
        assert_eq!(status, 0);
    }
    assert_eq!(scenario.outcome.error.tag, 7);
    drop(peer);
    assert_eq!(fds(), before);
    assert!(run.files.tls.sessions.is_empty());
}

#[test]
fn os_tls_loopback_laws() {
    if std::env::var_os("HIRSEL_TLS_LAWS_CHILD").is_none() {
        let out = crate::test_process::command()
            .args(["--exact", "tls::tests::os_tls_loopback_laws", "--nocapture"])
            .env("HIRSEL_TLS_LAWS_CHILD", "1")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }
    let mut run = Run::new();
    let baseline = fds();
    cert_case(&mut run, b"beta.test", Some(CA), Some(tls_failure(0, 1)));
    assert_eq!(fds(), baseline);
    cert_case(&mut run, b"alpha.test", Some(CA), None);
    let unrelated_pin = "649f529cdaed70d49948fbff4d06ea60d5483ac1450e42dfb171abea7d5fbe07";
    let net = run.grant(&format!(
        "Tls alpha.test 0-65535 Roots=sha256:{unrelated_pin}"
    ));
    let original = run.net;
    let (port, peer) = serve(listener(), certificate_config(&[]), |mut server| {
        server_handshake(&mut server).is_ok()
    });
    let key = run.connect(port);
    run.net = net;
    assert_error(
        run.upgrade(key, b"alpha.test", Some(UNRELATED), &[])
            .outcome,
        tls_failure(0, 0),
    );
    assert!(!peer.join().unwrap());
    run.net = original;
    cert_case(&mut run, b"alpha.test", None, Some(tls_failure(0, 0)));
    before_issue(&mut run);
    alpn(&mut run);
    truncated(&mut run);
    committed_prefix(&mut run);
    release_on_exit(&mut run);
    assert_eq!(fds(), baseline);
}
