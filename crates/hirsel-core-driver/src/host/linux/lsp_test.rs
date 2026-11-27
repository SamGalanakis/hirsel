use std::io;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

pub(crate) fn offline_editor(driver: &Path) -> Command {
    fn instruction(code: u32, jt: u8, jf: u8, k: u32) -> libc::sock_filter {
        libc::sock_filter {
            code: code as u16,
            jt,
            jf,
            k,
        }
    }
    const LOAD: u32 = libc::BPF_LD | libc::BPF_W | libc::BPF_ABS;
    const EQ: u32 = libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K;
    const SET: u32 = libc::BPF_JMP | libc::BPF_JSET | libc::BPF_K;
    const RETURN: u32 = libc::BPF_RET | libc::BPF_K;

    let mut filter = vec![instruction(LOAD, 0, 0, 0)]; // seccomp_data.nr
    let denied = [
        libc::SYS_socket,
        libc::SYS_socketpair,
        #[cfg(target_arch = "x86_64")]
        libc::SYS_fork,
        #[cfg(target_arch = "x86_64")]
        libc::SYS_vfork,
    ];
    for syscall in denied {
        filter.extend([
            instruction(EQ, 0, 1, syscall as u32),
            instruction(RETURN, 0, 0, libc::SECCOMP_RET_KILL_PROCESS),
        ]);
    }
    filter.extend([
        // Make libc lower clone3 to clone so the flag word can be inspected.
        instruction(EQ, 0, 1, libc::SYS_clone3 as u32),
        instruction(RETURN, 0, 0, libc::SECCOMP_RET_ERRNO | libc::ENOSYS as u32),
        instruction(EQ, 0, 3, libc::SYS_clone as u32),
        instruction(LOAD, 0, 0, 16), // seccomp_data.args[0] low word
        instruction(SET, 1, 0, libc::CLONE_THREAD as u32),
        instruction(RETURN, 0, 0, libc::SECCOMP_RET_KILL_PROCESS),
        instruction(RETURN, 0, 0, libc::SECCOMP_RET_ALLOW),
    ]);

    let mut command = Command::new(driver);
    // SAFETY: the closure makes only syscalls between fork and exec. The
    // filter is immutable and alive until the kernel copies it.
    unsafe {
        command.pre_exec(move || {
            let program = libc::sock_fprog {
                len: filter.len() as u16,
                filter: filter.as_ptr().cast_mut(),
            };
            if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
                || libc::syscall(
                    libc::SYS_seccomp,
                    libc::SECCOMP_SET_MODE_FILTER,
                    0,
                    &program,
                ) != 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command
}
