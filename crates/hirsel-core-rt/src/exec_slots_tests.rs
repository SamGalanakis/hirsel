//! EXEC: a child starts whatever descriptor numbers the program holds.
use std::{fs::File, os::fd::OwnedFd};

/// A program holding more descriptors than Darwin's `OPEN_MAX` (10,240) still
/// starts a child with its streams and working directory (docs/spec/exec.md):
/// the descriptors the child receives get numbers below the line, where
/// `posix_spawn`'s file actions accept them.
#[test]
fn exec_starts_a_child_while_the_program_holds_more_than_open_max_descriptors() {
    use crate::host::exec::{SpawnInput, Supervisor, Termination};
    use std::{ffi::CString, io::Read, os::fd::AsRawFd, sync::Arc};

    if crate::test_process::run_isolated(
        "exec_slots_tests::exec_starts_a_child_while_the_program_holds_more_than_open_max_descriptors",
    ) {
        return;
    }
    let supervisor = Supervisor::start(crate::exec::probe).unwrap();
    // SAFETY: this isolated run owns its descriptor limit.
    unsafe {
        let mut limit: libc::rlimit = std::mem::zeroed();
        assert_eq!(libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit), 0);
        limit.rlim_cur = limit.rlim_max.min(16_384);
        assert_eq!(libc::setrlimit(libc::RLIMIT_NOFILE, &limit), 0);
    }
    let null = File::open("/dev/null").unwrap();
    let held: Vec<OwnedFd> = (0..10_300)
        .map(|_| null.try_clone().unwrap().into())
        .collect();
    assert!(held.last().unwrap().as_raw_fd() > 10_240);
    let mut started = supervisor
        .spawn(SpawnInput {
            image: Arc::new(File::open("/bin/sh").unwrap().into()),
            seal: None,
            interpreter: None,
            cwd: File::open("/").unwrap().into(),
            argv: ["sh", "-c", "pwd"]
                .map(|arg| CString::new(arg).unwrap())
                .into(),
            envp: Vec::new(),
            piped: [false, true, false],
        })
        .unwrap();
    let stdout = started.stdout.take().unwrap();
    // SAFETY: this fixture owns the parent's pipe end; read it blocking.
    assert_eq!(
        unsafe { libc::fcntl(stdout.as_raw_fd(), libc::F_SETFL, 0) },
        0
    );
    let mut out = String::new();
    File::from(stdout).read_to_string(&mut out).unwrap();
    assert_eq!(out, "/\n");
    let status = loop {
        if let Some(status) = started.process.try_wait().unwrap() {
            break status;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    };
    assert_eq!(status, Termination::Exited(0));
    drop(held);
}
