use crate::{Context, entry::*, host_file::Files, net::*, provider::*};
use std::{
    fs::File,
    os::{fd::OwnedFd, unix::net::UnixStream},
    ptr,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

fn fds() -> usize {
    std::fs::read_dir("/proc/self/fd").unwrap().count()
}
fn isolated(law: &str) {
    let output = crate::test_process::command()
        .args(["--exact", "net_tests::net_laws_child", "--nocapture"])
        .env("WIRE_W1A_LAW", law)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
#[test]
fn net_denied_no_syscall() {
    isolated("denied");
}
#[test]
fn net_release_on_drop() {
    isolated("drop");
}
#[test]
fn net_split_admits_nothing() {
    isolated("split");
}
#[test]
fn net_laws_child() {
    let Ok(law) = std::env::var("WIRE_W1A_LAW") else {
        return;
    };
    let listen = if law == "inherited_denied" {
        "Listen 127.0.0.1/32 0-65535"
    } else {
        "Listen 127.0.0.0/8 0-65535"
    };
    let rules = [
        hirsel_core_net::Rule::parse("Connect 127.0.0.0/8 1-65535").unwrap(),
        hirsel_core_net::Rule::parse(listen).unwrap(),
        hirsel_core_net::Rule::parse("Connect ::1/128 1-65535").unwrap(),
        hirsel_core_net::Rule::parse("Listen ::1/128 0-65535").unwrap(),
    ]
    .map(hirsel_core_net::RuleView::address);
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
        directory_ops: std::ptr::null(),
        process_ops: std::ptr::null(),
        socket_activation: std::ptr::null(),
        secret_grants: core::ptr::null(),
        secret_grants_len: 0,
        secret_ops: std::ptr::null(),
        target: crate::process::View {
            data: b"test-target".as_ptr(),
            len: 11,
        },
    };
    // SAFETY: fixture pins configuration, context and registry through all calls.
    // An activation child runs one test, with the harness waiting for it and
    // no other code accessing the environment.
    unsafe {
        let owner = hirsel_core_os_process_new(&config);
        assert!(!owner.process.is_null());
        if law.starts_with("inherited_") {
            for name in [
                "LISTEN_PID",
                "LISTEN_FDS",
                "LISTEN_FDNAMES",
                "LISTEN_PIDFDID",
            ] {
                assert!(
                    std::env::var_os(name).is_none(),
                    "{name} leaked after receipt"
                );
            }
        }
        let run = hirsel_core_os_process_config(owner.process);
        let mut context = Context::new(1 << 20);
        let mut files = Files::default();
        context.host_files = &mut files;
        context.run = run;
        let n = hirsel_core_entry_grant(run, NET, 0);
        if law == "denied" {
            let before = fds();
            let denied =
                hirsel_core_host_connect(&mut context, n, 0, 0, 0x08080808, 80, 0, 0, 0, 1);
            assert_eq!(denied.error, IoError { tag: 1, code: 0 });
            assert_eq!(fds(), before, "denied_no_syscall");
        } else if law != "drop" && law != "split" {
            socket_law(
                &law,
                &mut context,
                n,
                hirsel_core_entry_grant(run, CLOCK, 0),
            );
        } else {
            let baseline = fds();
            for order in 0..3 {
                let (socket, peer) = UnixStream::pair().unwrap();
                let key = files.sockets.insert(OwnedFd::from(socket)).unwrap();
                if order == 0 && law == "drop" {
                    hirsel_core_socket_drop(&mut context, key);
                } else {
                    let available = context.available();
                    let count = fds();
                    let split = hirsel_core_net_split(&mut context, key, 1);
                    assert_eq!(split.error, IoError::OK);
                    assert_eq!(context.available(), available, "split_admits_nothing");
                    assert_eq!(files.sockets.len(), 1, "split creates no record");
                    assert_eq!(fds(), count);
                    let (a, b) = if order == 2 {
                        (split.value, split.written)
                    } else {
                        (split.written, split.value)
                    };
                    hirsel_core_socket_drop(&mut context, a);
                    assert_eq!(fds(), count, "first half retains the fd");
                    hirsel_core_socket_drop(&mut context, b);
                }
                drop(peer);
                assert_eq!(fds(), baseline, "release_on_drop");
            }
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let key = files.sockets.insert(listener.into()).unwrap();
            hirsel_core_socket_drop(&mut context, key);
            assert_eq!(fds(), baseline, "listener release_on_drop");
        }
        files.close_all(&mut context);
        hirsel_core_os_process_free(owner.process);
    }
}
#[test]
fn net_narrow_retains_charge_until_run_end_and_refuses_invalid_or_capacity() {
    let rule = hirsel_core_net::RuleView::address(
        hirsel_core_net::Rule::parse("Connect 127.0.0.0/8 1-65535").unwrap(),
    );
    let mut os = crate::os::OsProvider::new(
        File::create("/dev/null").unwrap().into(),
        File::create("/dev/null").unwrap().into(),
    );
    let provider = os.provider();
    let rules = Rules {
        data: &rule,
        len: 1,
    };
    let mut files = Files::default();
    let n = files
        .registry
        .grant(provider, (&rules as *const Rules) as u64);
    let mut context = Context::new(40);
    context.host_files = &mut files;
    unsafe {
        let invalid = hirsel_core_net_narrow(&mut context, n, 0, 0, 0, 0x7f000001, 8, 1, 80, 1);
        assert_eq!(invalid.error.tag, 1);
        assert_eq!(context.available(), 40);
        let good = hirsel_core_net_narrow(&mut context, n, 0, 0, 0, 0x7f000001, 32, 80, 80, 1);
        assert_eq!(good.error, IoError::OK);
        assert_eq!(context.available(), 0);
        let refused =
            hirsel_core_net_narrow(&mut context, good.value, 0, 0, 0, 0x7f000001, 32, 80, 80, 1);
        assert_eq!(refused.error.tag, 2);
        assert_eq!(refused.value, 40);
        assert_eq!(refused.written, 0);
        files.close_all(&mut context);
        assert_eq!(context.available(), 40);
    }
}

#[test]
fn two_worker_try_receive_pollers_leave_the_socket_driver_a_turn() {
    isolated("active_poll_multicore");
}

#[test]
fn net_socket_provider_laws() {
    for law in [
        "loopback_io",
        "ipv6_loopback",
        "partial_send",
        "deadline",
        "reset_after_queued",
        "fd_exhaustion_other24",
        "directional_waiters",
        "cancel_beats_ready",
        "active_poll",
        "spurious_connect",
    ] {
        isolated(law);
    }
}
fn inherited_child(listener: &std::net::TcpListener, law: &str) -> std::process::Command {
    use std::{os::fd::AsRawFd, os::unix::process::CommandExt, process::Command};

    let fd = listener.as_raw_fd();
    let executable = crate::test_process::command();
    let mut child = Command::new("sh");
    child
        .args([
            "-c",
            r#"LISTEN_PID=$$ LISTEN_FDS=1 exec "$@""#,
            "net-activation",
        ])
        .arg(executable.get_program())
        .args(executable.get_args())
        .args([
            "--exact",
            "net_tests::net_laws_child",
            "--nocapture",
            "--test-threads",
            "1",
        ])
        .env("WIRE_W1A_LAW", law)
        .env("LISTEN_FDNAMES", "listener")
        .env("LISTEN_PIDFDID", "1")
        .env(
            "HIRSEL_TEST_LISTEN_PORT",
            listener.local_addr().unwrap().port().to_string(),
        );
    // SAFETY: the parent keeps fd live through spawn. The child transfers
    // only its duplicate at fd 3 and uses async-signal-safe descriptor calls.
    unsafe {
        child.pre_exec(move || {
            if libc::dup2(fd, crate::os_net::LISTEN_FD) < 0
                || libc::fcntl(crate::os_net::LISTEN_FD, libc::F_SETFD, 0) < 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    child
}

#[test]
fn net_inherited_listener_keeps_queued_connection() {
    use std::{
        io::{BufRead, BufReader, Write},
        net::{TcpListener, TcpStream},
        process::Stdio,
    };

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut old = inherited_child(&listener, "inherited_hold")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    // Always close the child's stdin and reap it, including when it fails
    // before reporting the listener.
    let mut output = BufReader::new(old.stdout.take().unwrap());
    let result = (|| -> std::io::Result<Option<TcpStream>> {
        let mut line = String::new();
        while output.read_line(&mut line)? != 0 {
            if line.contains("inherited listener ready") {
                let mut client = TcpStream::connect(listener.local_addr()?)?;
                client.write_all(b"queued")?;
                old.stdin.as_mut().unwrap().write_all(b"x")?;
                return Ok(Some(client));
            }
            line.clear();
        }
        Ok(None)
    })();
    drop(old.stdin.take());
    std::io::copy(&mut output, &mut std::io::sink()).unwrap();
    assert!(
        old.wait().unwrap().success(),
        "old process failed to close its listener"
    );
    let _queued = result
        .unwrap()
        .expect("old process never held the listener");
    let output = inherited_child(&listener, "inherited_accept")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn net_inherited_listener_checks_bound_address() {
    let listener = std::net::TcpListener::bind("127.0.0.2:0").unwrap();
    let output = inherited_child(&listener, "inherited_denied")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn carrier(bytes: &mut [u8], len: usize) -> Buffer {
    Buffer {
        data: bytes.as_mut_ptr(),
        len,
        capacity: bytes.len(),
    }
}
unsafe fn connected(context: *mut Context, n: u64, family: u8) -> (u64, u64, u64) {
    unsafe {
        let ip = if family == 0 { 0x7f000001 } else { 1 };
        let l = hirsel_core_host_listen(context, n, family, 0, ip, 0, 16, 1);
        assert_eq!(l.error, IoError::OK);
        assert_eq!((l.family, l.hi, l.lo), (family, 0, ip));
        assert_ne!(l.port, 0);
        let c = hirsel_core_host_connect(context, n, family, 0, ip, l.port, 0, 0, 0, 2);
        assert_eq!(c.error, IoError::OK);
        let a = hirsel_core_host_accept(context, l.value, 0, 0, 0, 3);
        assert_eq!(a.error, IoError::OK);
        assert_eq!((a.family, a.hi, a.lo), (family, 0, ip));
        (l.value, c.value, a.value)
    }
}
unsafe fn socket_law(law: &str, context: *mut Context, n: u64, clock: u64) {
    use std::{
        io::{Read, Write},
        net::{TcpListener, TcpStream},
        os::fd::{AsRawFd, FromRawFd},
    };
    unsafe {
        match law {
            "resolve_wait" => resolve_wait_law(context, n, clock),
            "inherited_hold" | "inherited_accept" | "inherited_denied" => {
                let listener = hirsel_core_host_listen(context, n, 0, 0, 0x7f000001, 0, 16, 1);
                if law == "inherited_denied" {
                    assert_eq!(listener.error, DENIED.error);
                    return;
                }
                assert_eq!(listener.error, IoError::OK);
                let port = std::env::var("HIRSEL_TEST_LISTEN_PORT")
                    .unwrap()
                    .parse::<u16>()
                    .unwrap();
                assert_eq!(listener.port, port, "must adopt the host's socket");
                if law == "inherited_hold" {
                    println!("\ninherited listener ready");
                    std::io::stdout().flush().unwrap();
                    std::io::stdin().read_exact(&mut [0]).unwrap();
                } else {
                    let accepted = hirsel_core_host_accept(context, listener.value, 0, 0, 0, 2);
                    assert_eq!(accepted.error, IoError::OK);
                    let pair = hirsel_core_net_split(context, accepted.value, 3);
                    let mut buf = [0; 6];
                    let mut input = carrier(&mut buf, 0);
                    while input.len < buf.len() {
                        let read = hirsel_core_host_recv_bounded(
                            context, pair.value, &mut input, 6, 0, 0, 0, 4,
                        );
                        assert_eq!(read.error, IoError::OK);
                        assert_ne!(read.value, 0);
                    }
                    assert_eq!(&buf, b"queued");
                    hirsel_core_socket_drop(context, pair.value);
                    hirsel_core_socket_drop(context, pair.written);
                }
                hirsel_core_socket_drop(context, listener.value);
            }
            "loopback_io" | "ipv6_loopback" | "deadline" => {
                let available = (*context).available();
                let (listener, client, server) =
                    connected(context, n, u8::from(law == "ipv6_loopback"));
                assert_eq!(
                    (*context).available(),
                    available,
                    "resource records are uncharged"
                );
                let c = hirsel_core_net_split(context, client, 4);
                let s = hirsel_core_net_split(context, server, 4);
                let mut first = *b"abc";
                let mut second = *b"def";
                let prefix = carrier(&mut first, 3);
                let tail = carrier(&mut second, 3);
                assert_eq!(
                    hirsel_core_host_send_pair(context, c.written, &prefix, &tail, 0, 0, 0, 5)
                        .value,
                    6
                );
                let mut bytes = [0; 6];
                let mut input = carrier(&mut bytes, 0);
                // The bounded receive appends only the requested prefix even
                // when the whole payload is ready at an expired deadline.
                let got =
                    hirsel_core_host_recv_bounded(context, s.value, &mut input, 3, 1, clock, 0, 6);
                assert_eq!(
                    (got.error, got.value, input.len, input.capacity),
                    (IoError::OK, 3, 3, 6)
                );
                assert_eq!(&bytes, b"abc\0\0\0");
                let got =
                    hirsel_core_host_recv_bounded(context, s.value, &mut input, 3, 1, clock, 0, 7);
                assert_eq!(
                    (got.error, got.value, input.len, input.capacity),
                    (IoError::OK, 3, 6, 6),
                    "partial recv needs no fresh edge"
                );
                assert_eq!(&bytes, b"abcdef");
                // A borrowed static carrier has no spare storage even with a nonzero length.
                let mut borrowed = [0xa5; 2];
                let mut view = Buffer {
                    data: borrowed.as_mut_ptr(),
                    len: 2,
                    capacity: 0,
                };
                let out = hirsel_core_host_recv_bounded(
                    context,
                    s.value,
                    &mut view,
                    u64::MAX,
                    0,
                    0,
                    0,
                    7,
                );
                assert_eq!((out.error, out.value), (IoError::OK, 0));
                assert_eq!((view.len, view.capacity, borrowed), (2, 0, [0xa5; 2]));
                // Full buffer and empty send issue/deliver without touching the host.
                assert_eq!(
                    hirsel_core_host_recv_bounded(
                        context,
                        s.value,
                        &mut input,
                        u64::MAX,
                        0,
                        0,
                        0,
                        7
                    )
                    .value,
                    0
                );
                input.len = 0;
                assert_eq!(
                    hirsel_core_host_send_pair(
                        context,
                        s.written,
                        &input,
                        &empty_buffer(),
                        0,
                        0,
                        0,
                        7
                    )
                    .value,
                    0
                );
                crate::os_net::ATTEMPTS.set(0);
                let timeout = hirsel_core_host_recv_bounded(
                    context,
                    s.value,
                    &mut input,
                    u64::MAX,
                    1,
                    clock,
                    0,
                    8,
                );
                assert_eq!(
                    crate::os_net::ATTEMPTS.get(),
                    1,
                    "an expired wait makes one attempt"
                );
                assert_eq!(timeout.error.tag, 6, "expired silent recv");
                assert_eq!(input.len, 0);
                // A future deadline on the run's clock fires no earlier than
                // that clock reaches it (program time, not host time).
                let now = crate::host_ops::hirsel_core_host_op(
                    context,
                    crate::provider::MONO_NS,
                    clock,
                    std::ptr::null_mut(),
                    0,
                    8,
                )
                .value;
                let wait = 20_000_000;
                let timeout = hirsel_core_host_recv_bounded(
                    context,
                    s.value,
                    &mut input,
                    u64::MAX,
                    1,
                    clock,
                    now + wait,
                    8,
                );
                let after = crate::host_ops::hirsel_core_host_op(
                    context,
                    crate::provider::MONO_NS,
                    clock,
                    std::ptr::null_mut(),
                    0,
                    8,
                )
                .value;
                assert_eq!(timeout.error.tag, 6, "silent recv times out");
                assert!(after >= now + wait, "fired {} ns early", now + wait - after);
                hirsel_core_socket_drop(context, c.written);
                assert_eq!(
                    hirsel_core_host_recv_bounded(
                        context,
                        s.value,
                        &mut input,
                        u64::MAX,
                        0,
                        0,
                        0,
                        9
                    )
                    .value,
                    0
                );
                for key in [c.value, s.value, s.written, listener] {
                    hirsel_core_socket_drop(context, key);
                }
                assert!((*(*context).host_files).sockets.is_empty());
            }
            "partial_send" => {
                let listener = TcpListener::bind("127.0.0.1:0").unwrap();
                let socket = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
                let (mut peer, _) = listener.accept().unwrap();
                socket.set_nonblocking(true).unwrap();
                // Fixture-only send capacity forces a prefix without changing product defaults.
                let size = 4096i32;
                assert_eq!(
                    libc::setsockopt(
                        socket.as_raw_fd(),
                        libc::SOL_SOCKET,
                        libc::SO_SNDBUF,
                        (&size as *const i32).cast(),
                        4
                    ),
                    0
                );
                let key = (*(*context).host_files)
                    .sockets
                    .insert(socket.into())
                    .unwrap();
                let split = hirsel_core_net_split(context, key, 1);
                let mut payload = vec![0x5a; 256 * 1024];
                let len = payload.len();
                let b = carrier(&mut payload, len);
                let first = hirsel_core_host_send_pair(
                    context,
                    split.written,
                    &b,
                    &empty_buffer(),
                    1,
                    clock,
                    0,
                    2,
                );
                assert_eq!(first.error, IoError::OK);
                assert!(
                    first.value > 0 && first.value < len as u64,
                    "send commits one prefix"
                );
                let mut received = vec![0; first.value as usize];
                peer.read_exact(&mut received).unwrap();
                assert_eq!(received, payload[..first.value as usize]);
                assert_eq!((b.len, b.capacity), (len, len));
                hirsel_core_socket_drop(context, split.value);
                hirsel_core_socket_drop(context, split.written);
            }
            "reset_after_queued" => {
                let listener = TcpListener::bind("127.0.0.1:0").unwrap();
                let socket = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
                let (mut peer, _) = listener.accept().unwrap();
                socket.set_nonblocking(true).unwrap();
                let key = (*(*context).host_files)
                    .sockets
                    .insert(socket.into())
                    .unwrap();
                let split = hirsel_core_net_split(context, key, 1);
                peer.write_all(b"queued").unwrap();
                // Fixture-only reset. Product sockets never configure linger.
                let linger = libc::linger {
                    l_onoff: 1,
                    l_linger: 0,
                };
                assert_eq!(
                    libc::setsockopt(
                        peer.as_raw_fd(),
                        libc::SOL_SOCKET,
                        libc::SO_LINGER,
                        (&linger as *const libc::linger).cast(),
                        size_of::<libc::linger>() as u32
                    ),
                    0
                );
                drop(peer);
                // Observe the reset through the writer first, while receive data is queued.
                let mut payload = [1u8; 1];
                let b = carrier(&mut payload, 1);
                let mut reset = false;
                for _ in 0..1000 {
                    let out = hirsel_core_host_send_pair(
                        context,
                        split.written,
                        &b,
                        &empty_buffer(),
                        1,
                        clock,
                        0,
                        2,
                    );
                    if out.error.tag == 5 {
                        reset = true;
                        break;
                    }
                    std::thread::yield_now();
                }
                assert!(reset);
                let mut bytes = [0; 8];
                let mut input = carrier(&mut bytes, 0);
                let got = hirsel_core_host_recv_bounded(
                    context,
                    split.value,
                    &mut input,
                    u64::MAX,
                    0,
                    0,
                    0,
                    3,
                );
                assert_eq!(
                    (got.error, got.value),
                    (IoError::OK, 6),
                    "reset_after_queued"
                );
                assert_eq!(&bytes[..6], b"queued");
                input.len = 0;
                assert_eq!(
                    hirsel_core_host_recv_bounded(
                        context,
                        split.value,
                        &mut input,
                        u64::MAX,
                        0,
                        0,
                        0,
                        4
                    )
                    .error
                    .tag,
                    5
                );
                assert_eq!(
                    hirsel_core_host_send_pair(
                        context,
                        split.written,
                        &b,
                        &empty_buffer(),
                        0,
                        0,
                        0,
                        4
                    )
                    .error
                    .tag,
                    5
                );
                hirsel_core_socket_drop(context, split.value);
                hirsel_core_socket_drop(context, split.written);
            }
            "fd_exhaustion_other24" => {
                // A process-isolated descriptor limit, independent of the runner's fd count.
                let mut original: libc::rlimit = std::mem::zeroed();
                assert_eq!(libc::getrlimit(libc::RLIMIT_NOFILE, &mut original), 0);
                let cap = (fds() + 24) as libc::rlim_t;
                let limit = libc::rlimit {
                    rlim_cur: cap.min(original.rlim_cur),
                    rlim_max: original.rlim_max,
                };
                assert_eq!(libc::setrlimit(libc::RLIMIT_NOFILE, &limit), 0);
                let mut occupied = Vec::new();
                loop {
                    let fd = libc::open(c"/dev/null".as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC);
                    if fd < 0 {
                        assert_eq!(
                            std::io::Error::last_os_error().raw_os_error(),
                            Some(libc::EMFILE)
                        );
                        break;
                    }
                    occupied.push(OwnedFd::from_raw_fd(fd));
                }
                let out = hirsel_core_host_connect(context, n, 0, 0, 0x7f000001, 9, 0, 0, 0, 1);
                assert_eq!(
                    out.error,
                    IoError { tag: 8, code: 24 },
                    "fd_exhaustion_other24"
                );
                drop(occupied);
                assert_eq!(libc::setrlimit(libc::RLIMIT_NOFILE, &original), 0);
            }
            "directional_waiters" | "spurious_connect" => {
                let listener = TcpListener::bind("127.0.0.1:0").unwrap();
                let socket = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
                let (mut peer, _) = listener.accept().unwrap();
                socket.set_nonblocking(true).unwrap();
                let key = (*(*context).host_files)
                    .sockets
                    .insert(socket.into())
                    .unwrap();
                let sockets = &mut (*(*context).host_files).sockets;
                let mut tokens = Vec::new();
                sockets.arm(key, false, 101).unwrap();
                sockets.arm(key, true, 102).unwrap();
                peer.write_all(b"x").unwrap();
                let mut readable = libc::pollfd {
                    fd: sockets.fd(key),
                    events: libc::POLLIN,
                    revents: 0,
                };
                assert_eq!(libc::poll(&mut readable, 1, -1), 1);
                sockets.poll(0, |t| tokens.push(t)).unwrap();
                tokens.sort();
                assert_eq!(
                    tokens,
                    [101, 102],
                    "both directions parked on one socket wake"
                );
                let split = hirsel_core_net_split(context, key, 1);
                hirsel_core_socket_drop(context, split.value);
                hirsel_core_socket_drop(context, split.written);
                let mut empty = [0];
                assert_eq!(peer.read(&mut empty).unwrap(), 0);
                if law == "spurious_connect" {
                    let fd = libc::socket(
                        libc::AF_INET,
                        libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
                        0,
                    );
                    assert!(fd >= 0);
                    let key = (*(*context).host_files)
                        .sockets
                        .insert(OwnedFd::from_raw_fd(fd))
                        .unwrap();
                    let mut network = NetworkRequest {
                        send_tail: ptr::null(),
                        address: Address::default(),
                        sockets: &mut (*(*context).host_files).sockets,
                        until: None,
                        backlog: 0,
                        connecting: Some(key),
                        denied: false,
                        direction: 0,
                        os: &crate::os_net::OPERATIONS,
                        tls: ptr::null_mut(),
                        pending_write: false,
                    };
                    let mut r = Request {
                        operation: CONNECT,
                        mode: 0,
                        handle: 0,
                        path: (&mut network as *mut NetworkRequest).cast(),
                        path_len: size_of::<NetworkRequest>(),
                        buffer: carrier(&mut [], 0),
                        site: 1,
                    };
                    assert_eq!(
                        crate::os_net::step(&mut r, 99, false).kind,
                        STEP_PENDING,
                        "zero SO_ERROR without a peer must not succeed"
                    );
                    assert_eq!(crate::os_net::abort(&mut r, 99, ABORT_CANCEL).error.tag, 7);
                }
            }
            "cancel_beats_ready" | "active_poll" => {
                scheduler_socket_law(context, law == "cancel_beats_ready", false)
            }
            "active_poll_multicore" => {
                crate::tasks::with_workers(2, || scheduler_socket_law(context, false, true))
            }
            _ => panic!("unknown socket law {law}"),
        }
    }
}

use crate::channel::{
    Channel, hirsel_core_channel_new, hirsel_core_receiver_drop, hirsel_core_sender_drop,
    hirsel_core_try_receive, hirsel_core_try_send,
};

struct SocketScenario {
    key: u64,
    peer: i32,
    cancel: bool,
    publish: [*mut Channel; 2],
    parallel: bool,
    polled: AtomicBool,
    done: bool,
    error: u8,
    appended: usize,
    sticky: bool,
}
unsafe extern "C" fn socket_reader(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
    unsafe {
        let scenario = input.cast::<*mut SocketScenario>().read();
        let mut bytes = [0u8; 8];
        let mut b = carrier(&mut bytes, 0);
        let out =
            hirsel_core_host_recv_bounded(context, (*scenario).key, &mut b, u64::MAX, 0, 0, 0, 1);
        (*scenario).error = out.error.tag;
        (*scenario).appended = b.len;
        (*scenario).sticky = (*context)
            .cancel_requested
            .load(std::sync::atomic::Ordering::Acquire);
        (*scenario).done = true;
        for sender in (*scenario).publish {
            if !sender.is_null() {
                assert_eq!(
                    hirsel_core_try_send(sender, ptr::NonNull::dangling().as_ptr()),
                    0
                );
                hirsel_core_sender_drop(sender);
            }
        }
    }
    0
}

unsafe fn poll_channel(receiver: *mut Channel, parallel: bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(2);
    // SAFETY: this fiber owns the receiver and its item is ().
    unsafe {
        for turn in 0.. {
            if hirsel_core_try_receive(receiver, ptr::NonNull::dangling().as_ptr()) == 0 {
                return true;
            }
            if if parallel {
                Instant::now() >= deadline
            } else {
                turn == 255
            } {
                return false;
            }
        }
    }
    unreachable!()
}

#[derive(Clone, Copy)]
struct PollInput {
    receiver: *mut Channel,
    scenario: *const SocketScenario,
}

unsafe extern "C" fn socket_poller(context: *mut Context, out: *mut u8, input: *mut u8) -> u8 {
    // SAFETY: the parent pins the fixture and moved the receiver to this fiber.
    unsafe {
        let input = input.cast::<PollInput>().read();
        let parallel = (*input.scenario).parallel;
        // Mark the worker only once it has actually polled the empty channel.
        assert_eq!(
            hirsel_core_try_receive(input.receiver, ptr::NonNull::dangling().as_ptr()),
            1
        );
        (*input.scenario).polled.store(true, Ordering::Release);
        out.write(u8::from(poll_channel(input.receiver, parallel)));
        hirsel_core_receiver_drop(context, input.receiver);
    }
    0
}

unsafe extern "C" fn socket_parent(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
    use crate::tasks::*;
    unsafe {
        let scenario = input.cast::<*mut SocketScenario>().read();
        let mut child_input = scenario;
        let input_layout = ValueLayout {
            bytes: size_of::<*mut SocketScenario>(),
            align: align_of::<*mut SocketScenario>(),
            logical: 0,
            clean: None,
        };
        let result_layout = ValueLayout {
            bytes: 0,
            align: 1,
            logical: 0,
            clean: None,
        };
        let mut second_receiver = ptr::null_mut();
        let mut poller = ptr::null_mut();
        if !(*scenario).cancel {
            let first = hirsel_core_channel_new(context, 1, result_layout, 1);
            let second = hirsel_core_channel_new(context, 1, result_layout, 1);
            (*scenario).publish = [first.sender, second.sender];
            second_receiver = second.receiver;
            let mut poll_input = PollInput {
                receiver: first.receiver,
                scenario,
            };
            poller = hirsel_core_spawn(
                context,
                0,
                512,
                (&raw mut poll_input).cast(),
                socket_poller,
                crate::tasks_tests::layout::<PollInput, u8>(0),
                1,
            )
            .task;
            assert!(!poller.is_null());
            if (*scenario).parallel {
                // The supervisor does not suspend here, so the poller must
                // start on the other worker. Both remain on their first worker.
                let deadline = Instant::now() + Duration::from_secs(2);
                while !(*scenario).polled.load(Ordering::Acquire) {
                    assert!(Instant::now() < deadline, "poller did not run on the peer");
                    std::thread::yield_now();
                }
            }
        }
        let child = hirsel_core_spawn(
            context,
            0,
            8192,
            (&mut child_input as *mut *mut SocketScenario).cast(),
            socket_reader,
            TaskLayout {
                input: input_layout,
                result: result_layout,
            },
            1,
        );
        assert!(!child.task.is_null());
        if (*scenario).parallel {
            let deadline = Instant::now() + Duration::from_secs(2);
            while !task_waits_for_provider(child.task) {
                assert_eq!(
                    hirsel_core_try_receive(second_receiver, ptr::NonNull::dangling().as_ptr()),
                    1
                );
                assert!(Instant::now() < deadline, "reader did not park");
            }
        } else {
            checkpoint();
        }
        let byte = 42u8;
        assert_eq!(
            libc::send(
                (*scenario).peer,
                (&byte as *const u8).cast(),
                1,
                libc::MSG_NOSIGNAL
            ),
            1
        );
        if (*scenario).cancel {
            hirsel_core_cancel(context, child.task);
        } else {
            // The root and its peer keep polling while the reader is parked.
            // Only the nonblocking driver turn can make the reader publish.
            assert!(poll_channel(second_receiver, (*scenario).parallel));
            hirsel_core_receiver_drop(context, second_receiver);
            let mut delivered = 0u8;
            let joined = hirsel_core_join(context, poller, &mut delivered, 2);
            assert_eq!((joined.kind, joined.status, delivered), (0, 0, 1));
        }
        let joined = hirsel_core_join(context, child.task, ptr::null_mut(), 2);
        assert_eq!((joined.kind, joined.status), (0, 0));
    }
    0
}
unsafe fn scheduler_socket_law(context: *mut Context, cancel: bool, parallel: bool) {
    use std::{
        net::{TcpListener, TcpStream},
        os::fd::AsRawFd,
    };
    unsafe {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let socket = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (peer, _) = listener.accept().unwrap();
        socket.set_nonblocking(true).unwrap();
        let key = (*(*context).host_files)
            .sockets
            .insert(socket.into())
            .unwrap();
        let halves = hirsel_core_net_split(context, key, 1);
        let mut scenario = SocketScenario {
            key: halves.value,
            peer: peer.as_raw_fd(),
            cancel,
            publish: [ptr::null_mut(); 2],
            parallel,
            polled: AtomicBool::new(false),
            done: false,
            error: 0,
            appended: 0,
            sticky: false,
        };
        let mut input = &mut scenario as *mut SocketScenario;
        crate::causes::task_start(&mut *context).unwrap();
        let failed = crate::tasks::run_root(
            context,
            socket_parent,
            (&mut input as *mut *mut SocketScenario).cast(),
            ptr::null_mut(),
            size_of::<*mut SocketScenario>(),
            0,
            crate::tasks::SchedulerProvider::default(),
        );
        assert_eq!(failed, 0);
        assert!(scenario.done);
        assert_eq!(
            (scenario.error, scenario.appended, scenario.sticky),
            if cancel { (7, 0, true) } else { (0, 1, false) },
            "cancel_beats_ready"
        );
        hirsel_core_socket_drop(context, halves.value);
        hirsel_core_socket_drop(context, halves.written);
    }
}

fn name_buffer(name: &str) -> Buffer {
    Buffer {
        data: name.as_ptr().cast_mut(),
        len: name.len(),
        capacity: 0,
    }
}

fn ip_buffer(storage: &mut [hirsel_core_net::Ip], len: usize) -> Buffer {
    Buffer {
        data: storage.as_mut_ptr().cast(),
        len,
        capacity: storage.len(),
    }
}

#[test]
fn resolve_only_checks_names_and_grants_no_connect_authority() {
    use crate::{os_resolve::LOOKUP, resolve::hirsel_core_host_resolve};
    use std::{
        net::{IpAddr, Ipv4Addr, TcpListener},
        sync::{Arc, atomic::AtomicUsize},
    };

    let mut os = crate::os::OsProvider::new(
        File::create("/dev/null").unwrap().into(),
        File::create("/dev/null").unwrap().into(),
    );
    let provider = os.provider();
    let parent = RulesStorage::new([hirsel_core_net::GrantRule::parse(
        "Resolve localhost,*.a.test,127.0.0.1",
    )
    .unwrap()]);
    let mut files = Files::default();
    let net = files.registry.grant(provider, parent.handle());
    let mut context = Context::new(1 << 20);
    context.host_files = &mut files;
    let queries = Arc::new(AtomicUsize::new(0));
    let counter = queries.clone();
    let localhost = IpAddr::V4(Ipv4Addr::LOCALHOST);
    LOOKUP.set(Some(Arc::new(move |_, _| {
        counter.fetch_add(1, Ordering::Relaxed);
        Ok(vec![localhost])
    })));
    // SAFETY: the fixture pins the provider, grants and both borrowed carriers.
    unsafe {
        let child = hirsel_core_net_narrow_resolve(&mut context, net, &name_buffer("*.test"), 1);
        assert_eq!(child.error, IoError::OK);
        let mut addresses = [localhost.into(); 4];
        let mut out = ip_buffer(&mut addresses, 0);
        let mut resolve = |grant, name| {
            hirsel_core_host_resolve(
                &mut context,
                grant,
                &name_buffer(name),
                &mut out,
                0,
                0,
                0,
                1,
            )
        };
        let denied = resolve(net, "xa.test").error.tag;
        let malformed = resolve(net, "bad..a.test").error.tag;
        let narrowed = resolve(child.value, "other.test").error.tag;
        let queries_after_denied = queries.load(Ordering::Relaxed);
        let covered = resolve(child.value, "HOST.A.TEST.").error.tag;
        let literal = resolve(net, "127.0.0.1");
        assert_eq!((literal.error, literal.value), (IoError::OK, 1));
        assert_eq!(queries.load(Ordering::Relaxed), queries_after_denied + 1);

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let IpAddr::V4(ip) = addresses[0].into() else {
            panic!("the resolver stub returns IPv4");
        };
        let connect = hirsel_core_host_connect(
            &mut context,
            net,
            0,
            0,
            u64::from(u32::from(ip)),
            listener.local_addr().unwrap().port(),
            0,
            0,
            0,
            1,
        );
        assert_eq!(
            (
                denied,
                malformed,
                narrowed,
                queries_after_denied,
                covered,
                connect.error.tag
            ),
            (1, 1, 1, 0, 0, 1),
            "resolve_authority_before_query_narrow_and_connect"
        );
        files.close_all(&mut context);
    }
    LOOKUP.set(None);
    crate::os_resolve::WORKERS.with(|workers| {
        for helper in workers.take() {
            helper.join().unwrap();
        }
    });
}

#[test]
fn resolve_socket_wait_discards_aborted_helpers() {
    isolated("resolve_wait");
}

unsafe fn resolve_wait_law(context: *mut Context, parent: u64, clock: u64) {
    use crate::{
        os_resolve::{LOOKUP, WORKERS},
        resolve::hirsel_core_host_resolve,
        tasks::*,
    };
    use std::{
        net::{IpAddr, Ipv6Addr},
        sync::{Arc, Condvar, Mutex},
    };

    struct Scenario {
        net: u64,
        result: Option<(IoError, usize, IpAddr)>,
    }
    unsafe extern "C" fn child(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
        // SAFETY: run_root pins the fixture through both tasks.
        unsafe {
            let scenario = &mut **input.cast::<*mut Scenario>();
            let mut addresses = [IpAddr::V6(Ipv6Addr::LOCALHOST).into(); 2];
            let mut out = ip_buffer(&mut addresses, 1);
            let outcome = hirsel_core_host_resolve(
                context,
                scenario.net,
                &name_buffer("old.test"),
                &mut out,
                0,
                0,
                0,
                1,
            );
            scenario.result = Some((outcome.error, out.len, addresses[0].into()));
        }
        0
    }
    unsafe extern "C" fn supervisor(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
        // SAFETY: run_root pins the fixture until the child joins.
        unsafe {
            let task = hirsel_core_spawn(
                context,
                0,
                8192,
                input,
                child,
                crate::tasks_tests::layout::<*mut Scenario, ()>(0),
                1,
            )
            .task;
            assert!(!task.is_null());
            checkpoint();
            assert!(task_waits_for_provider(task), "resolve parked after issue");
            hirsel_core_cancel(context, task);
            let joined = hirsel_core_join(context, task, ptr::null_mut(), 2);
            assert_eq!((joined.kind, joined.status), (0, 0));
        }
        0
    }

    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let worker_gate = gate.clone();
    let stale = IpAddr::V6(Ipv6Addr::UNSPECIFIED);
    let fresh = "192.0.2.17".parse::<IpAddr>().unwrap();
    LOOKUP.set(Some(Arc::new(move |name, _| {
        if name == c"old.test" {
            let (lock, changed) = &*worker_gate;
            drop(
                changed
                    .wait_while(lock.lock().unwrap(), |released| !*released)
                    .unwrap(),
            );
            Ok(vec![stale])
        } else {
            assert_eq!(name, c"new.test");
            Ok(vec![fresh])
        }
    })));
    // SAFETY: the context and borrowed buffers remain live throughout the run.
    unsafe {
        let provider = *hirsel_core_capability_provider(context, parent);
        let rules =
            RulesStorage::new([hirsel_core_net::GrantRule::parse("Resolve *.test").unwrap()]);
        let net = (*(*context).host_files)
            .registry
            .grant(provider, rules.handle());
        let keep = IpAddr::V6(Ipv6Addr::LOCALHOST);
        let mut run = Scenario { net, result: None };
        let mut input = &raw mut run;
        crate::causes::task_start(&mut *context).unwrap();
        let failed = run_root(
            context,
            supervisor,
            (&raw mut input).cast(),
            ptr::null_mut(),
            size_of::<*mut Scenario>(),
            0,
            SchedulerProvider::default(),
        );
        assert_eq!(failed, 0);
        let (cancel, cancelled_len, cancelled_prefix) = run.result.unwrap();

        let now =
            crate::host_ops::hirsel_core_host_clock(context, MONO_NS, clock, ptr::null_mut(), 0, 1)
                .value;
        let mut addresses = [keep.into(); 2];
        let mut out = ip_buffer(&mut addresses, 1);
        let timeout = hirsel_core_host_resolve(
            context,
            net,
            &name_buffer("old.test"),
            &mut out,
            1,
            clock,
            now + 50_000_000,
            1,
        );
        let (timed_out_len, timed_out_prefix) = (out.len, IpAddr::from(addresses[0]));

        let old = WORKERS.with(|workers| workers.take());
        assert_eq!(old.len(), 2, "both expired operations started a helper");
        let old = Mutex::new(old);
        LOOKUP.set(Some(Arc::new(move |name, _| {
            assert_eq!(name, c"new.test");
            let (lock, changed) = &*gate;
            *lock.lock().unwrap() = true;
            changed.notify_all();
            for helper in std::mem::take(&mut *old.lock().unwrap()) {
                helper.join().unwrap();
            }
            Ok(vec![fresh])
        })));
        let next =
            hirsel_core_host_resolve(context, net, &name_buffer("new.test"), &mut out, 0, 0, 0, 1);
        assert_eq!(
            (
                cancel.tag,
                cancelled_len,
                cancelled_prefix,
                timeout.error.tag,
                timed_out_len,
                timed_out_prefix
            ),
            (7, 1, keep, 6, 1, keep),
            "resolve_abort_leaves_caller_list_untouched"
        );
        assert_eq!(
            (next.error, next.value, out.len, addresses.map(IpAddr::from)),
            (IoError::OK, 1, 2, [keep, fresh]),
            "discarded_resolver_answers_never_reach_next_list"
        );
    }
    LOOKUP.set(None);
    WORKERS.with(|workers| {
        for worker in workers.take() {
            worker.join().unwrap();
        }
    });
}
