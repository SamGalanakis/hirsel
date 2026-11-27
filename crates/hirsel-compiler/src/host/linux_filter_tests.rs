use super::*;

// Evaluate the classic BPF policy on synthetic seccomp_data words.
fn decision(filter: &[SockFilter], arch: u32, nr: u32, pid: u32) -> u32 {
    let mut acc = 0;
    let mut pc = 0;
    loop {
        let op = filter[pc];
        pc += 1;
        match op.code {
            c if c == BPF_LD | BPF_W | BPF_ABS => {
                acc = match op.k {
                    0 => nr,
                    4 => arch,
                    16 => pid,
                    other => panic!("load {other}"),
                };
            }
            c if c == BPF_JMP | BPF_JEQ | BPF_K => {
                pc += usize::from(if acc == op.k { op.jt } else { op.jf });
            }
            c if c == BPF_RET | BPF_K => return op.k,
            other => panic!("opcode {other}"),
        }
    }
}

#[test]
fn g22_check_library_boundary_worker_syscalls() {
    let policy = build_seccomp_filter(false);
    let decide = |nr: libc::c_long| decision(&policy, NATIVE_AUDIT_ARCH, nr as u32, 0);
    assert_eq!(decide(libc::SYS_read), SECCOMP_RET_ALLOW);
    assert_eq!(
        decision(&policy, 0, libc::SYS_read as u32, 0),
        SECCOMP_RET_KILL_PROCESS
    );
    for nr in [
        0x4000_0000 | libc::SYS_read,
        libc::SYS_kill,
        libc::SYS_tkill,
        libc::SYS_tgkill,
        libc::SYS_pidfd_open,
        libc::SYS_pidfd_send_signal,
        libc::SYS_pidfd_getfd,
        libc::SYS_keyctl,
        libc::SYS_add_key,
        libc::SYS_request_key,
        libc::SYS_memfd_create,
        libc::SYS_shmget,
        libc::SYS_shmat,
        libc::SYS_msgget,
        libc::SYS_semget,
        libc::SYS_execve,
        libc::SYS_execveat,
        libc::SYS_truncate,
        libc::SYS_clone,
        libc::SYS_socket,
        0x7fff_ffff,
    ] {
        assert_eq!(
            decide(nr),
            SECCOMP_RET_ERRNO | libc::EPERM as u32,
            "number {nr}"
        );
    }
    assert_ne!(
        decision(&policy, NATIVE_AUDIT_ARCH, libc::SYS_prlimit64 as u32, 1),
        SECCOMP_RET_ALLOW
    );
}

// Probe the executor independently of CgroupGuard. A writable parent and
// child memory/pids control files require the complete positive G22 matrix.
pub(crate) fn missing_confinement() -> Option<&'static str> {
    let membership = std::fs::read_to_string("/proc/self/cgroup").unwrap();
    let path = membership.lines().find_map(|row| row.strip_prefix("0::"));
    if let Some(path) = path {
        let group = Path::new("/sys/fs/cgroup").join(path.trim_start_matches('/'));
        for parent in [Some(group.as_path()), group.parent()]
            .into_iter()
            .flatten()
        {
            if std::fs::OpenOptions::new()
                .write(true)
                .open(parent.join("cgroup.procs"))
                .is_err()
            {
                continue;
            }
            let Ok(probe) = tempfile::Builder::new()
                .prefix("hirsel-g22-")
                .tempdir_in(parent)
            else {
                continue;
            };
            let delegated = ["memory.max", "memory.swap.max", "pids.max"]
                .iter()
                .all(|name| {
                    std::fs::OpenOptions::new()
                        .write(true)
                        .open(probe.path().join(name))
                        .is_ok()
                });
            // A cgroup directory has virtual control files and is removed with rmdir.
            std::fs::remove_dir(probe.keep()).unwrap();
            if delegated {
                return None;
            }
        }
    }
    Some("worker requires delegated cgroup v2 memory and pids controllers")
}
