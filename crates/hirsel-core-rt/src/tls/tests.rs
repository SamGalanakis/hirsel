mod authority;
mod server;

use super::*;
use crate::{entry::*, host_file::Files, net::*, tasks::*};
use hirsel_core_net::{Rule, RuleView, TlsRule};
use rustls::{
    AlertDescription, HandshakeKind, ServerConfig, ServerConnection, StreamOwned,
    SupportedProtocolVersion,
    pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
};
use std::{
    io::{Read, Write},
    net::{Ipv4Addr, TcpListener, TcpStream},
    os::fd::AsRawFd,
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
            secret_grants: core::ptr::null(),
            secret_grants_len: 0,
            env_patterns: crate::process::View {
                data: std::ptr::null(),
                len: 0,
            },
            secret_ops: std::ptr::null(),
            env_ops: std::ptr::null(),
            exec_bindings: std::ptr::null(),
            exec_bindings_len: 0,
            exec_ops: std::ptr::null(),
            revision: std::ptr::null(),
            place: 0,
            cred_grants: std::ptr::null(),
            cred_grants_len: 0,
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
    certificate_for(ALPHA, ALPHA_KEY, protocols)
}

fn certificate_for(leaf: &[u8], key: &[u8], protocols: &[&[u8]]) -> ServerConfig {
    certificate_for_versions(leaf, key, protocols, rustls::DEFAULT_VERSIONS)
}

fn certificate_for_versions(
    leaf: &[u8],
    key: &[u8],
    protocols: &[&[u8]],
    versions: &[&'static SupportedProtocolVersion],
) -> ServerConfig {
    let cert = CertificateDer::from_pem_slice(leaf).unwrap();
    let key = PrivateKeyDer::from_pem_slice(key).unwrap();
    let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(versions)
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
        let socket = crate::loopback::accept_local(&listener).unwrap();
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

fn resumption(run: &mut Run) {
    const PROTOCOL: &[u8] = b"http/1.1";
    let listener = listener();
    let port = listener.local_addr().unwrap().port();
    let config = Arc::new(certificate_config(&[PROTOCOL]));
    let peer = thread::spawn(move || {
        let kinds = std::array::from_fn::<_, 2, _>(|_| {
            let socket = crate::loopback::accept_local(&listener).unwrap();
            let mut server =
                StreamOwned::new(ServerConnection::new(config.clone()).unwrap(), socket);
            server_handshake(&mut server).unwrap();
            server.write_all(b"!").unwrap();
            server.conn.handshake_kind().unwrap()
        });
        let wrong = certificate_for(
            include_bytes!("../../testdata/tls/beta.pem"),
            include_bytes!("../../testdata/tls/beta.key.pem"),
            &[PROTOCOL],
        );
        // A replacement server has no corresponding session and must present its leaf.
        let socket = crate::loopback::accept_local(&listener).unwrap();
        let mut server = StreamOwned::new(ServerConnection::new(Arc::new(wrong)).unwrap(), socket);
        assert!(server_handshake(&mut server).is_err());
        kinds
    });
    let digest = ALPHA_DIGEST.trim();
    let expected =
        std::array::from_fn(|i| u64::from_str_radix(&digest[i * 16..][..16], 16).unwrap());
    for _ in 0..2 {
        let key = run.connect(port);
        let result = run.upgrade(key, b"alpha.test", Some(CA), &[PROTOCOL]);
        assert_eq!(
            (result.outcome.error, result.outcome.written),
            (IoError::OK, 1)
        );
        assert_eq!(result.peer, expected);
        // The first application read also processes any pending TLS 1.3 tickets.
        let mut byte = 0;
        let mut buffer = Buffer {
            data: &mut byte,
            len: 0,
            capacity: 1,
        };
        let out = unsafe {
            hirsel_core_host_recv_bounded(
                &raw mut *run.context,
                key | 1,
                &mut buffer,
                1,
                0,
                0,
                0,
                3,
            )
        };
        assert_eq!((out.error, out.value, byte), (IoError::OK, 1, b'!'));
        run.release_halves(key);
    }
    let key = run.connect(port);
    let result = run.upgrade(key, b"alpha.test", Some(CA), &[PROTOCOL]);
    assert_error(result.outcome, tls_failure(0, 1));
    assert_eq!(
        peer.join().unwrap(),
        [HandshakeKind::Full, HandshakeKind::Resumed]
    );
}

fn fds() -> usize {
    std::fs::read_dir(crate::host::sys::FD_DIR).unwrap().count()
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
    assert!(run.files.tls_sessions.is_empty());
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
    let peer = crate::loopback::accept_local(&listener).unwrap();
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
    let peer = crate::loopback::accept_local(&listener).unwrap();
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
    assert!(run.files.tls_sessions.is_empty());
}

fn tls12_round_trip(run: &mut Run) {
    let mut config = certificate_for_versions(ALPHA, ALPHA_KEY, &[], &[&rustls::version::TLS12]);
    // Split the server Certificate across wire records.
    config.max_fragment_size = Some(128);
    let (port, peer) = serve(listener(), config, |mut server| {
        server_handshake(&mut server).unwrap();
        let mut request = [0; 5];
        server.read_exact(&mut request).unwrap();
        assert_eq!(&request, b"hello");
        server.write_all(b"world").unwrap();
        server.conn.send_close_notify();
        server.flush().unwrap();
        assert_eq!(server.read(&mut [0]).unwrap(), 0);
    });
    let key = run.connect(port);
    assert_eq!(
        run.upgrade(key, b"alpha.test", Some(CA), &[]).outcome.error,
        IoError::OK
    );
    let out = unsafe {
        hirsel_core_host_send_pair(
            &raw mut *run.context,
            key | 2,
            &input(b"he"),
            &input(b"llo"),
            0,
            0,
            0,
            3,
        )
    };
    assert_eq!((out.error, out.value), (IoError::OK, 5));
    let mut bytes = [0; 8];
    let (out, n) = read_tls_law(run, key, &mut bytes);
    assert_eq!(
        (out.error, out.value, &bytes[..n]),
        (IoError::OK, 5, b"world".as_slice())
    );
    let (eof, n) = read_tls_law(run, key, &mut bytes);
    assert_eq!((eof.error, eof.value, n), (IoError::OK, 0, 0));
    run.release_halves(key);
    peer.join().unwrap();
}

fn read_tls_law(run: &mut Run, key: u64, bytes: &mut [u8]) -> (Outcome, usize) {
    let mut destination = Buffer {
        data: bytes.as_mut_ptr(),
        len: 0,
        capacity: bytes.len(),
    };
    let out = unsafe {
        hirsel_core_host_recv_bounded(
            &raw mut *run.context,
            key | 1,
            &mut destination,
            bytes.len() as u64,
            0,
            0,
            0,
            4,
        )
    };
    (out, destination.len)
}

fn feed_wire_record(server: &mut StreamOwned<ServerConnection, TcpStream>, payload_size: usize) {
    let mut wire = vec![0; 5 + payload_size];
    server.sock.read_exact(&mut wire[..5]).unwrap();
    assert_eq!(&wire[..3], &[23, 3, 3]);
    assert_eq!(
        u16::from_be_bytes([wire[3], wire[4]]) as usize,
        payload_size
    );
    server.sock.read_exact(&mut wire[5..]).unwrap();
    let mut bytes = wire.as_slice();
    while !bytes.is_empty() {
        assert_ne!(server.conn.read_tls(&mut bytes).unwrap(), 0);
        server.conn.process_new_packets().unwrap();
    }
}

fn tls13_record_boundaries_and_updates(run: &mut Run) {
    const FIRST: usize = 16 * 1024;
    const TOTAL: usize = FIRST + 257;
    const OVERHEAD: usize = 17; // Inner content type and AES-GCM tag.
    const UPDATE_SIZE: usize = 5 + OVERHEAD;
    let (sent, wait) = mpsc::channel();
    let (port, peer) = serve(listener(), certificate_config(&[]), move |mut server| {
        server_handshake(&mut server).unwrap();
        assert_eq!(
            server.conn.protocol_version(),
            Some(rustls::ProtocolVersion::TLSv1_3)
        );
        // Make sure complete_io did not already buffer application records.
        sent.send(()).unwrap();
        feed_wire_record(&mut server, FIRST + OVERHEAD);
        feed_wire_record(&mut server, UPDATE_SIZE);
        feed_wire_record(&mut server, TOTAL - FIRST + OVERHEAD);
        let mut bytes = vec![0; TOTAL];
        server.conn.reader().read_exact(&mut bytes).unwrap();
        assert!(
            bytes
                .iter()
                .enumerate()
                .all(|(i, &byte)| byte == (i % 251) as u8)
        );

        server.conn.refresh_traffic_keys().unwrap();
        server.conn.writer().write_all(&bytes).unwrap();
        let mut wire = Vec::new();
        while server.conn.wants_write() {
            server.conn.write_tls(&mut wire).unwrap();
        }
        // Put both application records in one socket write before the caller reads.
        server.sock.write_all(&wire).unwrap();
        sent.send(()).unwrap();
        feed_wire_record(&mut server, UPDATE_SIZE);
        feed_wire_record(&mut server, 1 + OVERHEAD);
        let mut final_byte = [0];
        server.conn.reader().read_exact(&mut final_byte).unwrap();
        assert_eq!(&final_byte, b"!");
    });
    let key = run.connect(port);
    crate::os_tls::TX_TEST_LIMIT.with(|limit| limit.set(Some(2)));
    assert_eq!(
        run.upgrade(key, b"alpha.test", Some(CA), &[]).outcome.error,
        IoError::OK
    );
    wait.recv().unwrap();
    let expected: Vec<_> = (0..TOTAL).map(|i| (i % 251) as u8).collect();
    let out = unsafe {
        hirsel_core_host_send_pair(
            &raw mut *run.context,
            key | 2,
            &input(&expected),
            &input(b""),
            0,
            0,
            0,
            3,
        )
    };
    assert_eq!((out.error, out.value), (IoError::OK, TOTAL as u64));
    wait.recv().unwrap();
    let mut bytes = vec![0; TOTAL];
    let (out, n) = read_tls_law(run, key, &mut bytes);
    assert_eq!(
        (out.error, out.value, n),
        (IoError::OK, TOTAL as u64, TOTAL)
    );
    assert_eq!(bytes, expected);
    let out = unsafe {
        hirsel_core_host_send_pair(
            &raw mut *run.context,
            key | 2,
            &input(b"!"),
            &input(b""),
            0,
            0,
            0,
            5,
        )
    };
    assert_eq!((out.error, out.value), (IoError::OK, 1));
    peer.join().unwrap();
    run.release_halves(key);
}

fn tls13_receive_slot_scrub(run: &mut Run) {
    const PAYLOAD: [u8; 128] = [0x5a; 128];
    const TAIL: u8 = 0xa5;
    for read in [PAYLOAD.len(), 1] {
        let mut config = certificate_config(&[]);
        config.send_tls13_tickets = 0;
        let (send, wait) = mpsc::channel();
        let (port, peer) = serve(listener(), config, move |mut server| {
            server_handshake(&mut server).unwrap();
            assert_eq!(
                server.conn.protocol_version(),
                Some(rustls::ProtocolVersion::TLSv1_3)
            );
            wait.recv().unwrap();
            server.write_all(&PAYLOAD).unwrap();
            assert_eq!(server.read(&mut [0]).unwrap(), 0);
        });
        let key = run.connect(port);
        assert_eq!(
            run.upgrade(key, b"alpha.test", Some(CA), &[]).outcome.error,
            IoError::OK
        );
        let address = crate::os_tls::seed_test_slot(TAIL);
        send.send(()).unwrap();
        let mut bytes = [0; PAYLOAD.len()];
        let (out, n) = read_tls_law(run, key, &mut bytes[..read]);
        assert_eq!(
            (out.error, n, &bytes[..n]),
            (IoError::OK, read, &PAYLOAD[..read])
        );
        let assert_scrubbed = |slot: Option<&[u8]>| {
            let (tail, rest) = slot
                .expect("receive slot must be in the pool")
                .split_last()
                .unwrap();
            assert_eq!(*tail, TAIL);
            assert!(rest.iter().all(|&byte| byte == 0));
        };
        if read == PAYLOAD.len() {
            crate::os_tls::inspect_test_slot(address, assert_scrubbed);
            run.release_halves(key);
        } else {
            crate::os_tls::inspect_test_slot(address, |slot| assert!(slot.is_none()));
            run.release_halves(key);
            crate::os_tls::inspect_test_slot(address, assert_scrubbed);
        }
        peer.join().unwrap();
    }
}

struct SlotSend<'a> {
    key: u64,
    bytes: &'a [u8],
    resume_peer: mpsc::Sender<()>,
    cancel: bool,
    sent: Outcome,
}

unsafe extern "C" fn send_slot(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
    unsafe {
        let case = input.cast::<*mut SlotSend<'_>>().read();
        (*case).sent = hirsel_core_host_send_pair(
            context,
            (*case).key | 2,
            &self::input((*case).bytes),
            &self::input(b""),
            0,
            0,
            0,
            3,
        );
    }
    0
}

unsafe extern "C" fn drain_slot(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
    unsafe {
        let mut case = input.cast::<*mut SlotSend<'_>>().read();
        let child = hirsel_core_spawn(
            context,
            0,
            8192,
            (&raw mut case).cast(),
            send_slot,
            crate::tasks_tests::layout::<*mut SlotSend<'_>, ()>(0),
            5,
        );
        assert!(!child.task.is_null());
        checkpoint();
        assert!(task_waits_for_provider(child.task));
        if (*case).cancel {
            hirsel_core_cancel(context, child.task);
        } else {
            (*case).resume_peer.send(()).unwrap();
        }
        let joined = hirsel_core_join(context, child.task, ptr::null_mut(), 5);
        assert_eq!((joined.kind, joined.status), (0, 0));
        if (*case).cancel {
            (*case).resume_peer.send(()).unwrap();
            // Cancellation returns the committed count, so the next send
            // resumes the queued ciphertext without replaying the plaintext.
            let flushed = hirsel_core_host_send_pair(
                context,
                (*case).key | 2,
                &self::input(b""),
                &self::input(b""),
                0,
                0,
                0,
                6,
            );
            assert_eq!(flushed.error, IoError::OK);
        }
    }
    0
}

fn committed_final_slot(run: &mut Run) {
    const SIZE: usize = crate::os_tls::TX_RECORDS * 16 * 1024;
    let set_buffer = |fd, option| unsafe {
        let size = 4096i32;
        assert_eq!(
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                option,
                (&raw const size).cast(),
                size_of::<i32>() as u32
            ),
            0
        );
    };
    for cancel in [false, true] {
        let listener = listener();
        set_buffer(listener.as_raw_fd(), libc::SO_RCVBUF);
        let (resume_peer, paused) = mpsc::channel();
        let (port, peer) = serve(listener, certificate_config(&[]), move |mut server| {
            server_handshake(&mut server).unwrap();
            paused.recv().unwrap();
            let mut bytes = vec![0; SIZE];
            server.read_exact(&mut bytes).unwrap();
            assert!(bytes.iter().enumerate().all(|(i, &b)| b == (i % 251) as u8));
            server.write_all(b"!").unwrap();
            server.conn.send_close_notify();
            server.flush().unwrap();
        });
        let key = run.connect(port);
        set_buffer(run.files.sockets.fd(key), libc::SO_SNDBUF);
        assert_eq!(
            run.upgrade(key, b"alpha.test", Some(CA), &[]).outcome.error,
            IoError::OK
        );
        let bytes: Vec<_> = (0..SIZE).map(|i| (i % 251) as u8).collect();
        let mut case = SlotSend {
            key,
            bytes: &bytes,
            resume_peer,
            cancel,
            sent: Outcome::default(),
        };
        let mut input = &raw mut case;
        unsafe {
            crate::causes::task_start(&mut run.context).unwrap();
            let status = run_root(
                &raw mut *run.context,
                drain_slot,
                (&raw mut input).cast(),
                ptr::null_mut(),
                size_of::<*mut SlotSend<'_>>(),
                0,
                SchedulerProvider::default(),
            );
            assert_eq!(status, 0);
        }
        assert_eq!(
            (case.sent.error, case.sent.value),
            (IoError::OK, SIZE as u64)
        );
        let mut reply = [0];
        let (out, n) = read_tls_law(run, key, &mut reply);
        assert_eq!((out.error, n, reply), (IoError::OK, 1, [b'!']));
        run.release_halves(key);
        peer.join().unwrap();
    }
}

fn tls13_record_rejection(run: &mut Run) {
    for (length, header_only, alert) in [
        (None, false, AlertDescription::BadRecordMac),
        (Some(16_640u16), false, AlertDescription::BadRecordMac),
        (Some(16_641), false, AlertDescription::RecordOverflow),
        (Some(16_641), true, AlertDescription::RecordOverflow),
        (Some(18_432), true, AlertDescription::RecordOverflow),
    ] {
        let (port, peer) = serve(listener(), certificate_config(&[]), move |mut server| {
            server_handshake(&mut server).unwrap();
            assert_eq!(
                server.conn.protocol_version(),
                Some(rustls::ProtocolVersion::TLSv1_3)
            );
            server.write_all(b"ok").unwrap();
            if let Some(length) = length {
                let [hi, lo] = length.to_be_bytes();
                let mut wire = vec![23, 3, 3, hi, lo];
                if !header_only {
                    wire.resize(5 + usize::from(length), 0);
                }
                server.sock.write_all(&wire).unwrap();
                if header_only {
                    // Keep the read half open to receive the alert. EOF makes
                    // this case fail even if the client waits for the body.
                    server.sock.shutdown(std::net::Shutdown::Write).unwrap();
                }
            } else {
                let mut wire = Vec::new();
                server.conn.writer().write_all(b"tampered").unwrap();
                server.conn.write_tls(&mut wire).unwrap();
                *wire.last_mut().unwrap() ^= 1;
                server.sock.write_all(&wire).unwrap();
            }
            let error = server.read(&mut [0]).unwrap_err();
            assert_eq!(
                error
                    .get_ref()
                    .and_then(|error| error.downcast_ref::<rustls::Error>()),
                Some(&rustls::Error::AlertReceived(alert))
            );
        });
        let key = run.connect(port);
        assert_eq!(
            run.upgrade(key, b"alpha.test", Some(CA), &[]).outcome.error,
            IoError::OK
        );
        let mut bytes = [0; 16];
        let (out, n) = read_tls_law(run, key, &mut bytes);
        assert_eq!(
            (out.error, out.value, &bytes[..n]),
            (IoError::OK, 2, b"ok".as_slice())
        );
        for _ in 0..2 {
            let (out, n) = read_tls_law(run, key, &mut bytes);
            assert_error(out, tls_failure(4, 0));
            assert_eq!((out.value, n), (0, 0));
        }
        let out = unsafe {
            hirsel_core_host_send_pair(
                &raw mut *run.context,
                key | 2,
                &input(b"refused"),
                &input(b""),
                0,
                0,
                0,
                5,
            )
        };
        assert_error(out, tls_failure(4, 0));
        run.release_halves(key);
        peer.join().unwrap();
    }
}

#[test]
fn os_tls_loopback_laws() {
    if std::env::var_os("HIRSEL_TLS_LAWS_CHILD").is_none() {
        let out = crate::test_process::command()
            .args(["--exact", "tls::tests::os_tls_loopback_laws", "--nocapture"])
            .env("HIRSEL_TLS_LAWS_CHILD", "1")
            // Loopback skips zero-copy under `auto`; these laws hold the
            // io_uring zero-copy send path to the same records.
            .env(hirsel_core_net::SEND_ZC_ENV, "always")
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
    resumption(&mut run);
    tls12_round_trip(&mut run);
    tls13_record_boundaries_and_updates(&mut run);
    tls13_record_rejection(&mut run);
    tls13_receive_slot_scrub(&mut run);
    committed_final_slot(&mut run);
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
    authority::before_issue(&mut run);
    alpn(&mut run);
    truncated(&mut run);
    committed_prefix(&mut run);
    release_on_exit(&mut run);
    assert_eq!(fds(), baseline);
}

#[test]
fn starttls_loopback_laws() {
    if std::env::var_os("HIRSEL_STARTTLS_LAWS_CHILD").is_none() {
        let out = crate::test_process::command()
            .args([
                "--exact",
                "tls::tests::starttls_loopback_laws",
                "--nocapture",
            ])
            .env("HIRSEL_STARTTLS_LAWS_CHILD", "1")
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
    const REQUEST: &[u8] = b"\x00\x00\x00\x08\x04\xd2\x16\x2f";
    let mut run = Run::new();
    let baseline = fds();
    for reply in [
        b"S".as_slice(),
        b"N".as_slice(),
        b"SR\x00\x00\x00\x08\x00\x00\x00\x00".as_slice(),
    ] {
        let listener = listener();
        let port = listener.local_addr().unwrap().port();
        let peer = thread::spawn(move || {
            let mut socket = crate::loopback::accept_local(&listener).unwrap();
            let mut request = [0; 8];
            socket.read_exact(&mut request).unwrap();
            assert_eq!(request, REQUEST);
            socket.write_all(reply).unwrap();
            let mut message = [0; 5];
            if reply == b"S" {
                let server = ServerConnection::new(Arc::new(certificate_config(&[]))).unwrap();
                StreamOwned::new(server, socket)
                    .read_exact(&mut message)
                    .unwrap();
                assert_eq!(&message, b"hello");
            } else if reply == b"N" {
                socket.read_exact(&mut message).unwrap();
                assert_eq!(&message, b"hello");
            } else {
                // Closing with unread bytes can give EOF or ECONNRESET.
                match socket.read(&mut message) {
                    Ok(n) => assert_eq!(n, 0, "the client sent bytes before refusing"),
                    Err(e) => assert_eq!(e.kind(), std::io::ErrorKind::ConnectionReset),
                }
            }
        });
        let key = run.connect(port);
        let context = &raw mut *run.context;
        let halves = unsafe { hirsel_core_net_split(context, key, 1) };
        let sent = unsafe {
            hirsel_core_host_send_pair(
                context,
                halves.written,
                &input(REQUEST),
                &input(b""),
                0,
                0,
                0,
                2,
            )
        };
        assert_eq!((sent.error, sent.value), (IoError::OK, 8));
        let mut byte = [0];
        let mut go_ahead = Buffer {
            data: byte.as_mut_ptr(),
            len: 0,
            capacity: 1,
        };
        let read = unsafe {
            hirsel_core_host_recv_bounded(context, halves.value, &mut go_ahead, 1, 0, 0, 0, 3)
        };
        assert_eq!(
            (read.error, read.value, byte[0]),
            (IoError::OK, 1, reply[0])
        );
        let write = if byte[0] == b'N' {
            halves.written
        } else {
            let available = run.context.available();
            let joined = unsafe { hirsel_core_net_join(context, halves.value, halves.written, 4) };
            assert_eq!((joined.error, joined.value), (IoError::OK, key));
            assert_eq!(run.context.available(), available);
            let secured = run.upgrade(joined.value, b"alpha.test", Some(CA), &[]);
            if reply.len() != 1 {
                assert_error(secured.outcome, tls_failure(4, 0));
                peer.join().unwrap();
                assert_eq!(fds(), baseline);
                continue;
            }
            assert_eq!(secured.outcome.error, IoError::OK);
            let digest = ALPHA_DIGEST.trim();
            assert_eq!(
                secured.peer,
                std::array::from_fn(|i| {
                    u64::from_str_radix(&digest[i * 16..i * 16 + 16], 16).unwrap()
                })
            );
            let failed = unsafe {
                hirsel_core_net_join(
                    context,
                    secured.outcome.value | 1,
                    secured.outcome.value | 2,
                    5,
                )
            };
            assert_eq!(failed.error.tag, 1);
            assert_eq!(
                (failed.value, failed.written),
                (secured.outcome.value | 1, secured.outcome.value | 2)
            );
            failed.written
        };
        let sent = unsafe {
            hirsel_core_host_send_pair(context, write, &input(b"hello"), &input(b""), 0, 0, 0, 6)
        };
        assert_eq!((sent.error, sent.value), (IoError::OK, 5));
        peer.join().unwrap();
        run.release_halves(write & !3);
        assert_eq!(fds(), baseline);
    }
}

const CLIENT_FIRST: &[u8] = b"n,,n=user,r=rOprNGfwEbeRWgbNEkqO";
const SERVER_FIRST: &[u8] =
    b"r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096";
const CLIENT_FINAL: &[u8] = b"c=biws,r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,p=dHzbZapWIk4jUhN+Ute9ytag9zjfMHgsqmmiz7AndVQ=";
const SERVER_FINAL: &[u8] = b"v=6rriTRBi23WpRR/wtup+mMhUZUn/dB5nLTJRsjl95G4=";

impl Run {
    fn seal_secret(&mut self, password: &[u8], names: &[u8]) -> u64 {
        let secret = unsafe {
            crate::secret::hirsel_core_secret_seal(
                &raw mut *self.context,
                &input(password),
                &input(names),
                10,
            )
        };
        assert_eq!(secret.error, IoError::OK);
        secret.value
    }

    fn scram_len(&mut self, key: u64, client: &[u8], server: &[u8]) -> Outcome {
        unsafe {
            crate::scram::hirsel_core_scram_final_len(
                &raw mut *self.context,
                key | 2,
                &input(client),
                &input(server),
                11,
            )
        }
    }

    fn scram_send(&mut self, key: u64, secret: u64, client: &[u8], server: &[u8]) -> Outcome {
        unsafe {
            crate::scram::hirsel_core_scram_sha256(
                &raw mut *self.context,
                key | 2,
                secret,
                &input(client),
                &input(server),
                &input(b""),
                &input(b""),
                0,
                0,
                0,
                12,
            )
        }
    }

    fn scram_verify(&mut self, scram: u64, final_message: &[u8]) -> bool {
        let verdict = unsafe {
            crate::scram::hirsel_core_scram_verify(
                &raw mut *self.context,
                scram,
                &input(final_message),
                13,
            )
        };
        assert_eq!(verdict.error, IoError::OK);
        verdict.value == 1
    }

    fn scram_peer(
        &mut self,
        config: ServerConfig,
        ca: &[u8],
        receive: usize,
    ) -> (u64, thread::JoinHandle<Vec<u8>>) {
        let (port, peer) = serve(listener(), config, move |mut server| {
            server_handshake(&mut server).unwrap();
            let mut bytes = vec![0; receive];
            server.read_exact(&mut bytes).unwrap();
            let mut extra = [0u8; 1];
            assert_eq!(server.read(&mut extra).unwrap(), 0);
            bytes
        });
        let key = self.connect(port);
        let tls = self.upgrade(key, b"alpha.test", Some(ca), &[]).outcome;
        assert_eq!(tls.error, IoError::OK);
        (tls.value, peer)
    }
}

fn rfc_scram_and_normalization(run: &mut Run) {
    let distinct_server = [
        b"r=rOprNGfwEbeRWgbNEkqOmore,".as_slice(),
        b"s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096",
    ]
    .concat();
    for (password, suffix, verdict, accepted) in [
        (b"pencil".as_slice(), CLIENT_FINAL, SERVER_FINAL, true),
        ("pen\u{ad}cil".as_bytes(), CLIENT_FINAL, SERVER_FINAL, true),
        (
            b"pencil",
            CLIENT_FINAL,
            b"v=7rriTRBi23WpRR/wtup+mMhUZUn/dB5nLTJRsjl95G4=",
            false,
        ),
        (b"pencil", CLIENT_FINAL, b"e=invalid-proof", false),
        (
            b"pen\xffcil",
            b"p=udNjQvoDM/dE+e/v63Vs504VHmXCmsAHQp5FC38JZcU=",
            SERVER_FINAL,
            false,
        ),
        (
            "pen\u{e000}cil".as_bytes(),
            b"p=S0CJLeozN1oaedLBR16sXAg8qzxE/6wMKY16FfthmYo=",
            SERVER_FINAL,
            false,
        ),
    ] {
        let secret = run.seal_secret(password, b"tls_send(alpha.test)");
        let (key, peer) = run.scram_peer(certificate_config(&[]), CA, 106);
        let len = run.scram_len(key, CLIENT_FIRST, SERVER_FIRST);
        assert_eq!((len.error, len.value), (IoError::OK, 106));
        let before = run.context.available();
        let result = run.scram_send(key, secret, CLIENT_FIRST, SERVER_FIRST);
        assert_eq!(result.error, IoError::OK);
        assert_eq!(run.context.available(), before - 32);
        assert_eq!(run.scram_verify(result.value, verdict), accepted);
        assert_eq!(run.context.available(), before);
        run.release_halves(key);
        let observed = peer.join().unwrap();
        if suffix.len() == CLIENT_FINAL.len() {
            assert_eq!(observed, CLIENT_FINAL);
        } else {
            let proof_at = 106 - suffix.len();
            assert_eq!(&observed[..proof_at], &CLIENT_FINAL[..proof_at]);
            assert_eq!(&observed[proof_at..], suffix);
        }
    }
    let secret = run.seal_secret(b"pencil", b"tls_send(alpha.test)");
    // 2 + 4 + 3 + len("rOprNGfwEbeRWgbNEkqOmore") + 3 + 44.
    let (key, peer) = run.scram_peer(certificate_config(&[]), CA, 80);
    let result = run.scram_send(key, secret, CLIENT_FIRST, &distinct_server);
    assert_eq!(result.error, IoError::OK);
    assert!(!run.scram_verify(result.value, SERVER_FINAL));
    run.release_halves(key);
    peer.join().unwrap();
}

fn scram_checks(run: &mut Run) {
    let secret = run.seal_secret(b"pencil", b"tls_send(alpha.test)");
    let denied = run.seal_secret(b"pencil", b"tls_send(beta.test)");
    let (key, peer) = run.scram_peer(certificate_config(&[]), CA, 0);
    let (server_prefix, _) = SERVER_FIRST.split_at(SERVER_FIRST.len() - b"4096".len());
    let mut count = |field: &[u8], tag| {
        let server = [server_prefix, field].concat();
        let context = &raw mut *run.context;
        // The compiler charges the call before entering the adapter.
        assert!(unsafe { crate::steps::hirsel_core_step_enter(context, 1) });
        assert!(unsafe { crate::steps::take_many(context, 1, 1) });
        let refused = run.scram_send(key, secret, CLIENT_FIRST, &server);
        assert_eq!((refused.status, refused.error.tag), (0, tag));
        unsafe { crate::steps::hirsel_core_step_leave(context) };
    };
    for (field, tag) in [
        (b"4095".as_slice(), 3),
        (b"1000001", 3),
        (b"0", 3),
        (b"04096", 2),
        (b"", 2),
    ] {
        count(field, tag);
    }
    for (client, server) in [
        (
            b"n,,n=,r=abc".as_slice(),
            b"r=other,s=c2FsdA==,i=4096".as_slice(),
        ),
        (b"n,,n=,r=abc", b"r=abc,s=c2FsdA==,i=4096"),
        (b"n,,n=,r=abc", b"m=required,r=abcd,s=c2FsdA==,i=4096"),
        (b"n,a=admin,n=,r=abc", b"r=abcd,s=c2FsdA==,i=4096"),
        (b"n,,n=,r=abc", b"r=abcd,s=invalid!,i=4096"),
        (b"n,,n=,r=abc", b"r=abcd,s=c2FsdA=="),
    ] {
        assert_eq!(run.scram_send(key, secret, client, server).error.tag, 2);
    }
    // Authority is checked before the malformed transcript.
    assert_eq!(run.scram_send(key, denied, b"", b"").error.tag, 1);
    run.release_halves(key);
    assert!(peer.join().unwrap().is_empty());

    let listener = listener();
    let socket = run.connect(listener.local_addr().unwrap().port());
    let mut peer = crate::loopback::accept_local(&listener).unwrap();
    let halves = unsafe { hirsel_core_net_split(&raw mut *run.context, socket, 1) };
    assert_eq!(
        run.scram_send(halves.written, secret, CLIENT_FIRST, SERVER_FIRST)
            .error
            .tag,
        1
    );
    run.release_halves(socket);
    assert_eq!(peer.read(&mut [0; 1]).unwrap(), 0);

    let (key, peer) = run.scram_peer(certificate_config(&[]), CA, 106);
    let server = [server_prefix, b"1000000"].concat();
    let sent = run.scram_send(key, secret, CLIENT_FIRST, &server);
    assert_eq!(sent.error, IoError::OK);
    let before_drop = run.context.available();
    unsafe { crate::host_file::hirsel_core_hostfile_drop(&raw mut *run.context, sent.value) };
    assert_eq!(run.context.available(), before_drop + 32);
    run.release_halves(key);
    let observed = peer.join().unwrap();
    assert_eq!(&observed[..62], &CLIENT_FINAL[..62]);
    assert_eq!(
        &observed[62..],
        b"q3vxYsMrFMG9Eyps4rvfV3vdUuiXJOVRpsmqNUzyjUw="
    );
}

fn scram_iteration_steps(run: &mut Run) {
    crate::causes::task_start(&mut run.context).unwrap();
    let secret = run.seal_secret(b"pencil", b"tls_send(alpha.test)");
    for budget in [4097, 4096] {
        let (key, peer) = run.scram_peer(
            certificate_config(&[]),
            CA,
            if budget == 4097 { 106 } else { 0 },
        );
        let context = &raw mut *run.context;
        assert!(unsafe { crate::steps::hirsel_core_step_enter(context, budget) });
        assert!(unsafe { crate::steps::take_many(context, 1, 1) });
        let result = run.scram_send(key, secret, CLIENT_FIRST, SERVER_FIRST);
        if budget == 4097 {
            assert_eq!((result.status, result.error), (0, IoError::OK));
            unsafe { crate::steps::hirsel_core_step_leave(context) };
            assert!(run.scram_verify(result.value, SERVER_FINAL));
        } else {
            assert_ne!(result.status, 0);
            assert!(unsafe { crate::steps::hirsel_core_step_catch(context) });
        }
        run.release_halves(key);
        let observed = peer.join().unwrap();
        assert_eq!(observed, if budget == 4097 { CLIENT_FINAL } else { b"" });
    }
}

fn scram_channel_binding(run: &mut Run) {
    const BINDING_CA: &[u8] = include_bytes!("../../testdata/tls/binding-ca.pem");
    const SHA384_LEAF: &[u8] = include_bytes!("../../testdata/tls/alpha-sha384.pem");
    const SHA384_KEY: &[u8] = include_bytes!("../../testdata/tls/alpha-sha384.key.pem");
    const ED25519_LEAF: &[u8] = include_bytes!("../../testdata/tls/alpha-ed25519.pem");
    const ED25519_KEY: &[u8] = include_bytes!("../../testdata/tls/alpha-ed25519.key.pem");
    let secret = run.seal_secret(b"pencil", b"tls_send(alpha.test)");
    let original = run.net;
    let new_ca = run.grant("Tls alpha.test 0-65535 Roots=sha256:4398b17f3dee153317fa713ea5d8e105037a1e4e212b6dce9732e60dda880c9b");
    let connecting = run.grant("Connect 127.0.0.0/8 1-65535");
    let client = b"p=tls-server-end-point,,n=,r=abc";
    let server = b"r=abcd,s=c2FsdA==,i=4096";
    for (leaf, private, ca, cbind, len) in [
        (ALPHA, ALPHA_KEY, CA,
            b"cD10bHMtc2VydmVyLWVuZC1wb2ludCwsDNTzaSy63NwAkiwJE8efmIi+HGTUlwqVM47Ch/Mc1ps=".as_slice(),
            132),
        (SHA384_LEAF, SHA384_KEY, BINDING_CA,
            b"cD10bHMtc2VydmVyLWVuZC1wb2ludCwskQt2Yucxjnj1T8Vrz/+WSRZnZWWH5rd80WG14f8BGkyNOdBa8gHV6cokKzLpbOcU",
            152),
        (ED25519_LEAF, ED25519_KEY, BINDING_CA, b"", 0),
    ] {
        let (port, peer) = serve(listener(), certificate_for(leaf, private, &[]), move |mut server| {
            server_handshake(&mut server).unwrap();
            let mut bytes = vec![0; len];
            server.read_exact(&mut bytes).unwrap();
            assert_eq!(server.read(&mut [0; 1]).unwrap(), 0);
            bytes
        });
        run.net = connecting;
        let key = run.connect(port);
        run.net = if ca == CA { original } else { new_ca };
        let secure = run.upgrade(key, b"alpha.test", Some(ca), &[]).outcome;
        assert_eq!(secure.error, IoError::OK);
        let key = secure.value;
        let unknown = b"p=tls-unique,,n=,r=abc";
        assert_eq!(run.scram_len(key, unknown, server).error.tag, 4);
        assert_eq!(run.scram_send(key, secret, unknown, server).error.tag, 4);
        let checked = run.scram_len(key, client, server);
        let sent = run.scram_send(key, secret, client, server);
        if len == 0 {
            assert_eq!((checked.error.tag, sent.error.tag), (4, 4));
        } else {
            assert_eq!((checked.error, checked.value), (IoError::OK, len as u64));
            assert_eq!(sent.error, IoError::OK);
            unsafe { crate::host_file::hirsel_core_hostfile_drop(&raw mut *run.context, sent.value) };
        }
        run.release_halves(key);
        let received = peer.join().unwrap();
        if len != 0 {
            assert_eq!(&received[..2], b"c=");
            assert_eq!(&received[2..2 + cbind.len()], cbind);
            assert_eq!(&received[2 + cbind.len()..2 + cbind.len() + 10], b",r=abcd,p=");
        }
    }
    run.net = original;
}

#[test]
fn scram_loopback_laws() {
    let mut run = Run::new();
    rfc_scram_and_normalization(&mut run);
    scram_checks(&mut run);
    scram_iteration_steps(&mut run);
    scram_channel_binding(&mut run);
}

#[test]
fn expired_secret_send_issues_no_host_tls_bytes() {
    let mut run = Run::new();
    let (port, peer) = serve(listener(), certificate_config(&[]), |mut server| {
        let mut received = [0; 2];
        server.read_exact(&mut received).unwrap();
        received
    });
    let key = run.connect(port);
    let sealed = run.seal_secret(b"BCD", b"tls_send(alpha.test)");
    let upgrade = run.upgrade(key, b"alpha.test", Some(CA), &[]);
    assert_eq!(upgrade.outcome.error, IoError::OK);
    let deadline = run.now();
    let (secrets, marks) = ([sealed], [1u64, 0, 0, 0]);
    let words = |words: &[u64]| Buffer {
        data: words.as_ptr().cast_mut().cast(),
        len: words.len(),
        capacity: 0,
    };
    let expired = unsafe {
        hirsel_core_host_send_secret(
            &raw mut *run.context,
            key | 2,
            &input(b"AE"),
            &words(&secrets),
            &words(&marks),
            1,
            run.clock,
            deadline,
            4,
        )
    };
    let sent = unsafe {
        hirsel_core_host_send_pair(
            &raw mut *run.context,
            key | 2,
            &input(b"ok"),
            &input(b""),
            0,
            0,
            0,
            4,
        )
    };
    run.release_halves(key);
    let received = peer.join().unwrap();
    assert_error(expired, crate::os_net::signal(6));
    assert_eq!(expired.written, 0);
    assert_eq!(sent.error, IoError::OK);
    assert_eq!(sent.value, 2);
    assert_eq!(&received, b"ok");
}
