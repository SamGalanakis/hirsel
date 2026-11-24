use super::*;

/// Set by the choice law for the fresh process it runs this test in.
const EXPECT_ENV: &str = "HIRSEL_NET_REACTOR_LAW_EXPECT";
const REFUSE_ENV: &str = "HIRSEL_NET_REACTOR_LAW_REFUSE";

/// Make this thread's `io_uring_setup` fail with EPERM, as a container's
/// seccomp profile does.
fn refuse_io_uring_setup() {
    let statement = |code: u32, jt, jf, k| libc::sock_filter {
        code: code as u16,
        jt,
        jf,
        k,
    };
    let ret = libc::BPF_RET | libc::BPF_K;
    let filter = [
        // seccomp_data.nr
        statement(libc::BPF_LD | libc::BPF_W | libc::BPF_ABS, 0, 0, 0),
        statement(
            libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K,
            0,
            1,
            libc::SYS_io_uring_setup as u32,
        ),
        statement(ret, 0, 0, libc::SECCOMP_RET_ERRNO | libc::EPERM as u32),
        statement(ret, 0, 0, libc::SECCOMP_RET_ALLOW),
    ];
    let program = libc::sock_fprog {
        len: filter.len() as u16,
        filter: filter.as_ptr().cast_mut(),
    };
    // SAFETY: the filter outlives the call, which copies it.
    unsafe {
        assert_eq!(libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0), 0);
        assert_eq!(
            libc::prctl(libc::PR_SET_SECCOMP, libc::SECCOMP_MODE_FILTER, &program),
            0,
            "{}",
            io::Error::last_os_error()
        );
    }
}

/// The fresh-process half of the choice law below.
#[test]
#[ignore = "run by the reactor choice law in a fresh process"]
fn the_reactor_this_process_chose() {
    let Some(expected) = std::env::var_os(EXPECT_ENV) else {
        return;
    };
    if std::env::var_os(REFUSE_ENV).is_some() {
        refuse_io_uring_setup();
    }
    assert_eq!(reactor().unwrap().name(), expected);
    let mut sockets = Sockets::default();
    let (socket, mut peer) = UnixStream::pair().unwrap();
    socket.set_nonblocking(true).unwrap();
    let key = sockets.insert(socket.into()).unwrap();
    sockets.arm(key, false, 3).unwrap();
    sockets.poll(0, |_| {}).unwrap();
    peer.write_all(b"x").unwrap();
    sockets.arm(key, false, 4).unwrap();
    assert!(readied(&mut sockets, 4));
}

/// D5: with the variable unset, a process takes io_uring wherever the
/// kernel offers it and is not below the release gate, and epoll, still
/// carrying readiness, where `io_uring_setup` is refused.
#[test]
fn an_unset_reactor_takes_io_uring_where_offered_and_epoll_where_refused() {
    let offered = if kernel_offers_io_uring() && release_allows_uring(&kernel_release()) {
        "io_uring"
    } else {
        "epoll"
    };
    for (refused, expected) in [(false, offered), (true, "epoll")] {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child
            .args([
                "--exact",
                "reactor::tests::choice_tests::the_reactor_this_process_chose",
                "--ignored",
                "--nocapture",
            ])
            .env_remove(REACTOR_ENV)
            .env(EXPECT_ENV, expected);
        if refused {
            child.env(REFUSE_ENV, "1");
        }
        let out = child.output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && stdout.contains("1 passed"),
            "refused={refused} expected {expected}:\n{stdout}{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// The gate admits 6.13 and later by numeric (major, minor), whatever
/// the distribution suffix; everything through 6.12.y, and an unreadable
/// release, stays on epoll.
#[test]
fn the_release_gate_admits_6_13_and_later_only() {
    for (release, allowed) in [
        ("6.12.118", false),
        ("6.12.999-generic", false),
        ("6.13", true),
        ("6.13.0", true),
        ("6.13.0-rc1", true),
        ("6.13-rc1", true),
        ("6.8.0-149-generic", false),
        ("6.1.0-27-amd64", false),
        ("6.2", false),
        ("5.15.0-130-generic", false),
        ("5.130.0", false),
        ("7.0.0-43-generic", true),
        ("10.1", true),
        ("6", false),
        ("", false),
        ("generic", false),
    ] {
        assert_eq!(release_allows_uring(release), allowed, "{release}");
    }
}
