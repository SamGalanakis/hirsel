use super::*;

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

pub(super) fn before_issue(run: &mut Run) {
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
        let tcp = crate::loopback::accept_local(&listener).unwrap();
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
        let _tcp = crate::loopback::accept_local(&listener).unwrap();
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
    let _tcp = crate::loopback::accept_local(&listener).unwrap();
    run.net = wrong_port;
    validate_no_issue(run, key, b"y.x.a.test", Some(CA), &[], DENIED);
    run.net = original;
    let socket = unsafe {
        hirsel_core_host_listen(&raw mut *run.context, original, 0, 0, 0x7f000001, 0, 8, 1)
    };
    assert_eq!(socket.error, IoError::OK);
    let peer = TcpStream::connect((Ipv4Addr::LOCALHOST, socket.port)).unwrap();
    let accepted = unsafe {
        crate::net_tests::accept_from_port(
            &raw mut *run.context,
            socket.value,
            peer.local_addr().unwrap().port(),
            1,
        )
    };
    assert_eq!(accepted.error, IoError::OK);
    let context = &raw mut *run.context;
    // SAFETY: The fixture owns the accepted socket and then both halves.
    let joined = unsafe {
        let halves = hirsel_core_net_split(context, accepted.value, 1);
        hirsel_core_net_join(context, halves.value, halves.written, 1)
    };
    assert_eq!((joined.error, joined.value), (IoError::OK, accepted.value));
    validate_no_issue(run, joined.value, b"alpha.test", Some(CA), &[], DENIED);
    run.release(socket.value);

    for malformed in [
        b"".as_slice(),
        b"-----BEGIN CERTIFICATE-----\n!!\n-----END CERTIFICATE-----",
    ] {
        let key = run.connect(port);
        let _tcp = crate::loopback::accept_local(&listener).unwrap();
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
        let _tcp = crate::loopback::accept_local(&listener).unwrap();
        validate_no_issue(run, key, b"alpha.test", Some(CA), &alpn, tls_failure(2, 0));
    }
}
