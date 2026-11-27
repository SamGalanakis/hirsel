use super::*;
use crate::{
    host::trust,
    os_tls::platform::{Input, TEST_TRUST, TestTrust},
};
use rustls::pki_types::{ServerName, UnixTime};
use std::{
    os::fd::{FromRawFd, OwnedFd},
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

const APPLE_CA: &[u8] = include_bytes!("../../../testdata/tls/platform-ca.pem");
const APPLE_LEAF: &[u8] = include_bytes!("../../../testdata/tls/platform-leaf.pem");
const APPLE_PIN: &str = "60977253ffc26bd2e60734788b6deb20c53f89cae7fca0287c336356d0381eeb";
const UNRELATED_PIN: &str = "649f529cdaed70d49948fbff4d06ea60d5483ac1450e42dfb171abea7d5fbe07";
// 2021-01-01 UTC, inside the platform leaf's Dec 2020 to Feb 2021 lifetime.
const INSIDE: u64 = 1_609_459_200;
const DAY: u64 = 86_400;

fn time(seconds: u64) -> UnixTime {
    UnixTime::since_unix_epoch(Duration::from_secs(seconds))
}

fn anchored() -> TestTrust {
    TestTrust {
        anchors: Some(CertificateDer::from_pem_slice(APPLE_CA).unwrap()),
        now: Some(time(INSIDE)),
        ..TestTrust::default()
    }
}

fn with_trust(config: TestTrust, check: impl FnOnce()) {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            TEST_TRUST.take();
        }
    }
    assert!(TEST_TRUST.replace(Some(Arc::new(config))).is_none());
    let _reset = Reset;
    check();
}

fn leaf_cases() {
    let leaf = CertificateDer::from_pem_slice(APPLE_LEAF).unwrap();
    let check = |leaf, name, seconds, detail| {
        let answer = trust::evaluate(&Input {
            chain: vec![leaf],
            name: ServerName::try_from(name).unwrap(),
            now: time(seconds),
            test: Some(Arc::new(anchored())),
        })
        .unwrap_err();
        assert_error(answer, tls_failure(0, detail));
    };
    check(leaf.clone(), "wrong.test", INSIDE, 1);
    check(leaf.clone(), "127.0.0.1", INSIDE, 1);
    check(leaf.clone(), "alpha.test", INSIDE + 60 * DAY, 2);
    check(leaf, "alpha.test", INSIDE - 60 * DAY, 3);
    check(CertificateDer::from(vec![1, 2, 3]), "alpha.test", INSIDE, 4);

    let policy = trust::evaluate(&Input {
        chain: vec![CertificateDer::from_pem_slice(ALPHA).unwrap()],
        name: ServerName::try_from("alpha.test").unwrap(),
        now: time(1_798_761_600),
        test: Some(Arc::new(TestTrust {
            anchors: Some(CertificateDer::from_pem_slice(CA).unwrap()),
            ..TestTrust::default()
        })),
    })
    .unwrap_err();
    // errSecCertificateValidityPeriodTooLong is a policy failure.
    assert_eq!(
        (policy.status, policy.error, policy.value as i64),
        (0, tls_failure(0, 4).error, -67901),
    );
}

fn exchange(
    run: &mut Run,
    roots: Option<(&[u8], &str)>,
    version: &'static SupportedProtocolVersion,
) -> Outcome {
    let config = certificate_for_versions(APPLE_LEAF, ALPHA_KEY, &[], &[version]);
    let (port, peer) = serve(listener(), config, |mut server| {
        server_handshake(&mut server).is_ok()
    });
    let key = run.connect(port);
    let net = run.net;
    let roots = roots.map(|(bundle, pin)| {
        run.net = run.grant(&format!("Tls alpha.test 0-65535 Roots=sha256:{pin}"));
        bundle
    });
    let outcome = run.upgrade(key, b"alpha.test", roots, &[]).outcome;
    run.net = net;
    let accepted = outcome.error == IoError::OK;
    if accepted {
        run.release_halves(outcome.value);
    }
    assert_eq!(peer.join().unwrap(), accepted);
    outcome
}

fn issuer_clock_and_roots() {
    let mut run = Run::new();
    with_trust(
        TestTrust {
            anchors: None,
            ..anchored()
        },
        || {
            let out = exchange(&mut run, None, &rustls::version::TLS13);
            assert_eq!(
                (out.status, out.error),
                (0, tls_failure(0, 0).error),
                "Apple code {}",
                out.value as i64
            );
            assert!(matches!(out.value as i64, -25318 | -67843), "{out:?}");
        },
    );
    with_trust(anchored(), || {
        for version in rustls::DEFAULT_VERSIONS {
            let out = exchange(&mut run, None, version);
            assert_eq!(out.error, IoError::OK, "Apple code {}", out.value as i64);
        }
    });
    with_trust(
        TestTrust {
            now: Some(time(INSIDE + 50 * 365 * DAY)),
            ..anchored()
        },
        || {
            let out = exchange(&mut run, None, &rustls::version::TLS13);
            assert_error(out, tls_failure(0, 2));
        },
    );

    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    with_trust(
        TestTrust {
            on_evaluate: Some(Box::new(move || {
                count.fetch_add(1, Ordering::Relaxed);
            })),
            ..anchored()
        },
        || {
            assert_eq!(
                exchange(
                    &mut run,
                    Some((APPLE_CA, APPLE_PIN)),
                    &rustls::version::TLS13
                )
                .error,
                IoError::OK
            );
            assert_error(
                exchange(
                    &mut run,
                    Some((UNRELATED, UNRELATED_PIN)),
                    &rustls::version::TLS13,
                ),
                tls_failure(0, 0),
            );
            assert_eq!(calls.load(Ordering::Relaxed), 0);
        },
    );
}

fn held_evaluation(reason: u8, expected: u8) {
    let (entered, wait_for_entry) = mpsc::channel();
    let (release, wait_for_release) = mpsc::channel();
    let (completed, wait_for_completion) = mpsc::channel();
    let wait_for_release = Mutex::new(wait_for_release);
    let trust = TestTrust {
        on_evaluate: Some(Box::new(move || {
            entered.send(()).unwrap();
            wait_for_release.lock().unwrap().recv().unwrap();
        })),
        on_complete: Some(Box::new(move |published| {
            completed.send(published).unwrap();
        })),
        ..anchored()
    };
    let mut run = Run::new();
    let (port, peer) = serve(
        listener(),
        certificate_for(APPLE_LEAF, ALPHA_KEY, &[]),
        |mut server| server_handshake(&mut server).is_ok(),
    );
    let socket = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
    socket.set_nonblocking(true).unwrap();
    let fd = duplicate_high(&socket, 128);
    let fd_number = fd.as_raw_fd();
    let key = run.files.sockets.insert(fd).unwrap();
    drop(socket);
    let mut state = HandshakeState {
        pin: TlsTrust::System,
        name: Some(Name::parse("alpha.test").unwrap()),
        roots: None,
        cache: &raw mut run.files.tls_cache,
        alpn: Vec::new(),
        cacheable: true,
        stream: None,
        consumed: false,
        pending_write: false,
    };
    // A closed TLS operation must never write the success outputs.
    let clock = unsafe { *hirsel_core_capability_provider(&*run.context, run.clock) };
    let until = (reason == ABORT_DEADLINE).then(|| (clock, 0, run.now() + 2_000_000_000));
    let mut tls = TlsRequest {
        network: NetworkRequest {
            send_tail: ptr::null(),
            address: crate::net::Address {
                family: 0,
                hi: 0,
                lo: 0x7f000001,
                port,
            },
            sockets: &raw mut run.files.sockets,
            until,
            backlog: 0,
            connecting: None,
            denial: None,
            public: false,
            direction: 0,
            os: &crate::os_net::TLS_OPERATIONS,
            tls: &raw mut run.files.tls_sessions,
            tls_send_pending: false,
            max_receive: 0,
        },
        name: view(b"alpha.test"),
        trust: 0,
        roots: input(&[]),
        alpn: view(&[]),
        protocol_out: ptr::null_mut(),
        peer_out: ptr::null_mut(),
        binding_out: ptr::null_mut(),
        state: (&raw mut state).cast(),
    };
    let operations = crate::os_net::TLS_OPERATIONS.step;
    let mut request = Request {
        operation: TLS_CONNECT,
        mode: 0,
        handle: key,
        path: (&raw mut tls).cast(),
        path_len: size_of::<TlsRequest>(),
        buffer: net::empty_buffer(),
        site: 0,
    };
    let token = crate::tasks::operation_token();
    with_trust(trust, || {
        // SAFETY: the fixture pins the request, registry and session through
        // abort, and polls the registry only between callbacks.
        let mut step = unsafe { operations(&mut request, token, true) };
        loop {
            assert_eq!(step.kind, STEP_PENDING, "{step:?}");
            if state.stream.as_ref().unwrap().waiting_for_trust() {
                break;
            }
            let mut ready = false;
            let poll_until = Instant::now() + Duration::from_secs(2);
            while !ready {
                let remaining = poll_until.saturating_duration_since(Instant::now());
                assert!(
                    !remaining.is_zero(),
                    "TLS socket did not ready before SecTrust"
                );
                run.files
                    .sockets
                    .poll(remaining.as_millis().max(1) as i32, |id| {
                        ready |= id == token
                    })
                    .unwrap();
            }
            step = unsafe { operations(&mut request, token, false) };
        }
        wait_for_entry.recv().unwrap();
        if reason == ABORT_DEADLINE {
            assert_ne!(step.deadline_ns, 0);
            let remaining = step.deadline_ns.saturating_sub(crate::os::monotonic_ns());
            thread::sleep(Duration::from_nanos(remaining));
            assert!(crate::os::monotonic_ns() >= step.deadline_ns);
        }
        assert_error(
            unsafe { (crate::os_net::TLS_OPERATIONS.abort)(&mut request, token, reason) },
            crate::os_net::signal(expected),
        );
        assert!(state.consumed && state.stream.is_none());
        assert!(run.files.sockets.is_empty() && run.files.tls_sessions.is_empty());
        assert!(!peer.join().unwrap());
    });
    drop(run);
    let mut next = Run::new();
    let guard = duplicate_high(&std::fs::File::open("/dev/null").unwrap(), fd_number);
    assert_eq!(guard.as_raw_fd(), fd_number);
    release.send(()).unwrap();
    assert!(!wait_for_completion.recv().unwrap());
    assert_ne!(unsafe { libc::fcntl(guard.as_raw_fd(), libc::F_GETFD) }, -1);
    with_trust(anchored(), || {
        let out = exchange(&mut next, None, &rustls::version::TLS13);
        assert_eq!(out.error, IoError::OK, "Apple code {}", out.value as i64);
    });
}

fn duplicate_high(source: &impl AsRawFd, minimum: i32) -> OwnedFd {
    // SAFETY: source owns a live fd. F_DUPFD_CLOEXEC yields a new owner.
    unsafe {
        let fd = libc::fcntl(source.as_raw_fd(), libc::F_DUPFD_CLOEXEC, minimum);
        assert!(fd >= minimum, "{}", std::io::Error::last_os_error());
        OwnedFd::from_raw_fd(fd)
    }
}

#[test]
fn system_trust_laws() {
    leaf_cases();
    issuer_clock_and_roots();
    held_evaluation(ABORT_DEADLINE, 6);
    held_evaluation(ABORT_CANCEL, 7);
}
