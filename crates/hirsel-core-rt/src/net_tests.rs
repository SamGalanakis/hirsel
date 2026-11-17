use crate::{Context, entry::*, host_file::Files, net::*, provider::*};
use std::{
    fs::File,
    os::{fd::OwnedFd, unix::net::UnixStream},
    ptr,
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
    let rules = [
        hirsel_core_net::Rule::parse("Connect 127.0.0.0/8 1-65535").unwrap(),
        hirsel_core_net::Rule::parse("Listen 127.0.0.0/8 0-65535").unwrap(),
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
        target: crate::process::View {
            data: b"test-target".as_ptr(),
            len: 11,
        },
    };
    // SAFETY: fixture pins configuration, context and registry through all calls.
    unsafe {
        let owner = hirsel_core_os_process_new(&config);
        assert!(!owner.process.is_null());
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
    let rule = hirsel_core_net::Rule::parse("Connect 127.0.0.0/8 1-65535").unwrap();
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
    let n = files.grant(provider, (&rules as *const Rules) as u64);
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
fn net_socket_provider_laws() {
    for law in [
        "loopback_io",
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
fn carrier(bytes: &mut [u8], len: usize) -> Buffer {
    Buffer {
        data: bytes.as_mut_ptr(),
        len,
        capacity: bytes.len(),
    }
}
unsafe fn connected(context: *mut Context, n: u64) -> (u64, u64, u64) {
    unsafe {
        let l = hirsel_core_host_listen(context, n, 0, 0, 0x7f000001, 0, 16, 1);
        assert_eq!(l.error, IoError::OK);
        assert_eq!((l.family, l.hi, l.lo), (0, 0, 0x7f000001));
        assert_ne!(l.port, 0);
        let c = hirsel_core_host_connect(context, n, 0, 0, 0x7f000001, l.port, 0, 0, 0, 2);
        assert_eq!(c.error, IoError::OK);
        let a = hirsel_core_host_accept(context, l.value, 0, 0, 0, 3);
        assert_eq!(a.error, IoError::OK);
        assert_eq!((a.family, a.hi, a.lo), (0, 0, 0x7f000001));
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
            "loopback_io" | "deadline" => {
                let available = (*context).available();
                let (listener, client, server) = connected(context, n);
                assert_eq!(
                    (*context).available(),
                    available,
                    "resource records are uncharged"
                );
                let c = hirsel_core_net_split(context, client, 4);
                let s = hirsel_core_net_split(context, server, 4);
                let mut bytes = *b"abcdef";
                let mut out = carrier(&mut bytes, 6);
                assert_eq!(
                    hirsel_core_host_send(context, c.written, &mut out, 0, 0, 0, 5).value,
                    6
                );
                let mut bytes = [0; 3];
                let mut input = carrier(&mut bytes, 0);
                // Ready wins even when the operation's deadline is already past.
                let got = hirsel_core_host_recv(context, s.value, &mut input, 1, clock, 0, 6);
                assert_eq!(
                    (got.error, got.value, input.len, input.capacity),
                    (IoError::OK, 3, 3, 3)
                );
                assert_eq!(&bytes, b"abc");
                input.len = 0;
                let got = hirsel_core_host_recv(context, s.value, &mut input, 1, clock, 0, 7);
                assert_eq!(
                    (got.error, got.value),
                    (IoError::OK, 3),
                    "partial recv needs no fresh edge"
                );
                assert_eq!(&bytes, b"def");
                // A borrowed static carrier has no spare storage even with a nonzero length.
                let mut borrowed = [0xa5; 2];
                let mut view = Buffer {
                    data: borrowed.as_mut_ptr(),
                    len: 2,
                    capacity: 0,
                };
                let out = hirsel_core_host_recv(context, s.value, &mut view, 0, 0, 0, 7);
                assert_eq!((out.error, out.value), (IoError::OK, 0));
                assert_eq!((view.len, view.capacity, borrowed), (2, 0, [0xa5; 2]));
                // Full buffer and empty send issue/deliver without touching the host.
                assert_eq!(
                    hirsel_core_host_recv(context, s.value, &mut input, 0, 0, 0, 7).value,
                    0
                );
                input.len = 0;
                assert_eq!(
                    hirsel_core_host_send(context, s.written, &mut input, 0, 0, 0, 7).value,
                    0
                );
                crate::os_net::ATTEMPTS.set(0);
                let timeout = hirsel_core_host_recv(context, s.value, &mut input, 1, clock, 0, 8);
                assert_eq!(
                    crate::os_net::ATTEMPTS.get(),
                    1,
                    "an expired wait makes one attempt"
                );
                assert_eq!(timeout.error.tag, 6, "expired silent recv");
                assert_eq!(input.len, 0);
                hirsel_core_socket_drop(context, c.written);
                assert_eq!(
                    hirsel_core_host_recv(context, s.value, &mut input, 0, 0, 0, 9).value,
                    0
                );
                for key in [c.value, s.value, s.written, listener] {
                    hirsel_core_socket_drop(context, key);
                }
                assert!((*(*context).host_files).sockets.is_empty());
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
                let mut b = carrier(&mut payload, 1);
                let mut reset = false;
                for _ in 0..1000 {
                    let out = hirsel_core_host_send(context, split.written, &mut b, 1, clock, 0, 2);
                    if out.error.tag == 5 {
                        reset = true;
                        break;
                    }
                    std::thread::yield_now();
                }
                assert!(reset);
                let mut bytes = [0; 8];
                let mut input = carrier(&mut bytes, 0);
                let got = hirsel_core_host_recv(context, split.value, &mut input, 0, 0, 0, 3);
                assert_eq!(
                    (got.error, got.value),
                    (IoError::OK, 6),
                    "reset_after_queued"
                );
                assert_eq!(&bytes[..6], b"queued");
                input.len = 0;
                assert_eq!(
                    hirsel_core_host_recv(context, split.value, &mut input, 0, 0, 0, 4)
                        .error
                        .tag,
                    5
                );
                assert_eq!(
                    hirsel_core_host_send(context, split.written, &mut b, 0, 0, 0, 4)
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
                while tokens.len() < 2 {
                    sockets.poll(100, |t| tokens.push(t)).unwrap();
                }
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
                        address: Address::default(),
                        sockets: &mut (*(*context).host_files).sockets,
                        until: None,
                        backlog: 0,
                        connecting: Some(key),
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
                scheduler_socket_law(context, law == "cancel_beats_ready")
            }
            _ => panic!("unknown socket law {law}"),
        }
    }
}

struct SocketScenario {
    key: u64,
    peer: i32,
    cancel: bool,
    done: bool,
    error: u8,
    appended: usize,
    sticky: bool,
}
unsafe extern "C" fn socket_no_drop(_: *mut Context, _: *mut u8) -> u8 {
    0
}
unsafe extern "C" fn socket_reader(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
    unsafe {
        let scenario = input.cast::<*mut SocketScenario>().read();
        let mut bytes = [0u8; 8];
        let mut b = carrier(&mut bytes, 0);
        let out = hirsel_core_host_recv(context, (*scenario).key, &mut b, 0, 0, 0, 1);
        (*scenario).error = out.error.tag;
        (*scenario).appended = b.len;
        (*scenario).sticky = (*context)
            .cancel_requested
            .load(std::sync::atomic::Ordering::Acquire);
        (*scenario).done = true;
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
            drop: socket_no_drop,
        };
        let result_layout = ValueLayout {
            bytes: 0,
            align: 1,
            logical: 0,
            drop: socket_no_drop,
        };
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
        checkpoint(false); // Reader has issued and parked before the root returns.
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
            // Never idle. Socket progress must come from nonblocking polls at events.
            for _ in 0..256 {
                event(CHECK_CANCEL, 0, 0);
                if (*scenario).done {
                    break;
                }
            }
            assert!(
                (*scenario).done,
                "active scheduler events must poll sockets"
            );
        }
        let joined = hirsel_core_join(context, child.task, ptr::null_mut(), 2);
        assert_eq!((joined.kind, joined.status), (0, 0));
    }
    0
}
unsafe fn scheduler_socket_law(context: *mut Context, cancel: bool) {
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
