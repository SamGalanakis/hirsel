use crate::{FuncId, eval, text, wf};
use std::os::{fd::OwnedFd, unix::net::UnixStream};
fn fds() -> usize {
    // macOS has no /proc; its /dev/fd lists the same descriptors.
    let dir = if cfg!(target_os = "macos") {
        "/dev/fd"
    } else {
        "/proc/self/fd"
    };
    std::fs::read_dir(dir).unwrap().count()
}
const SOURCE: &str = include_str!("../samples/net.core");
fn isolate(law: &str) {
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "net_tests::wire_eval_child", "--nocapture"])
        .env("WIRE_EVAL_LAW", law)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
#[test]
fn wire_eval_denied_no_syscall() {
    isolate("denied");
}
#[test]
fn wire_eval_release_on_drop() {
    isolate("drop");
}
#[test]
fn wire_eval_split_admits_nothing() {
    isolate("split");
}
#[test]
fn wire_eval_child() {
    let Ok(law) = std::env::var("WIRE_EVAL_LAW") else {
        return;
    };
    let module = text::parse(SOURCE).unwrap();
    wf::module(&module).unwrap();
    let mut network = eval::Network::default();
    let mut remaining = eval::Network::default();
    if law == "denied" {
        let before = fds();
        let report =
            eval::run_network_into(&module, FuncId(2), &[1], 1000, network, &mut remaining);
        assert_eq!(report.outcome, eval::Outcome::Returned(vec![0, 1, 0, 0]));
        assert_eq!(fds(), before, "denied_no_syscall");
    } else {
        let (socket, peer) = UnixStream::pair().unwrap();
        let key = network.sockets.insert(OwnedFd::from(socket)).unwrap();
        let before = fds();
        let f = if law == "drop" { 0 } else { 1 };
        let report =
            eval::run_network_into(&module, FuncId(f), &[key], 1000, network, &mut remaining);
        assert_eq!(report.outcome, eval::Outcome::Returned(vec![]));
        assert_eq!(report.admissions, 0, "split_admits_nothing");
        assert_eq!(report.peak_reserved, 0);
        assert_eq!(fds(), before - 1, "release_on_drop");
        assert!(remaining.sockets.is_empty());
        drop(peer);
    }
}
