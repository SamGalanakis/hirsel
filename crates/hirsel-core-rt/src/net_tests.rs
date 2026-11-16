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
    let rule = hirsel_core_net::Rule::parse("Connect 127.0.0.0/8 1-65535").unwrap();
    let config = OsConfig {
        root_limit: 1 << 20,
        args: ptr::null(),
        args_len: 0,
        directories: ptr::null(),
        directories_len: 0,
        grant_mask: 1 << NET,
        stdout_fd: 1,
        stderr_fd: 2,
        parameters: ptr::null(),
        parameters_len: 0,
        net_rules: &rule,
        net_rules_len: 1,
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
                hirsel_core_host_connect(&mut context, n, 0, 0, 0x08080808, 80, ptr::null(), 1);
            assert_eq!(denied.error, IoError { tag: 1, code: 0 });
            assert_eq!(fds(), before, "denied_no_syscall");
            let refused =
                hirsel_core_host_connect(&mut context, n, 0, 0, 0x7f000001, 80, ptr::null(), 1);
            assert_eq!(refused.error, IoError { tag: 8, code: 38 });
            assert_eq!(fds(), before);
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
