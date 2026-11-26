//! Laws E and E2: a supervised run whose terminal changes hands while a secret
//! read is armed, from a child that allocates nothing after its fork.
use super::*;

/// An exec of `built` prepared before a fork, so the forked child allocates
/// nothing.
struct Exec {
    path: CString,
    argv: [*const libc::c_char; 2],
    /// Owns the strings `envp` points into.
    _env: Vec<CString>,
    envp: Vec<*const libc::c_char>,
}

impl Exec {
    fn new(built: &Built) -> Box<Self> {
        let path = CString::new(built.executable.as_os_str().as_bytes()).unwrap();
        let env: Vec<CString> = std::env::vars_os()
            .map(|(k, v)| {
                let mut pair = k.as_bytes().to_vec();
                pair.push(b'=');
                pair.extend_from_slice(v.as_bytes());
                CString::new(pair).unwrap()
            })
            .collect();
        let mut exec = Box::new(Self {
            argv: [path.as_ptr(), std::ptr::null()],
            path,
            envp: env
                .iter()
                .map(|e| e.as_ptr())
                .chain([std::ptr::null()])
                .collect(),
            _env: env,
        });
        exec.argv[0] = exec.path.as_ptr();
        exec
    }

    /// # Safety
    /// Async-signal-safe; called in a forked child only.
    unsafe fn run(&self, stdin: i32, null: i32) -> ! {
        // SAFETY: as above.
        unsafe {
            reset_child();
            libc::dup2(stdin, 0);
            libc::dup2(null, 1);
            libc::dup2(null, 2);
            libc::execve(self.path.as_ptr(), self.argv.as_ptr(), self.envp.as_ptr());
            libc::_exit(127)
        }
    }
}

/// How a supervisor saw its program end.
const SUPERVISED_OK: i32 = 0;
const NEVER_ARMED: i32 = 10;
const STOPPED: i32 = 20;
const OTHER_END: i32 = 30;
const STILL_RUNNING: i32 = 40;

/// # Safety
/// Async-signal-safe; called in a forked child only.
unsafe fn nap() {
    let pause = libc::timespec {
        tv_sec: 0,
        tv_nsec: 5_000_000,
    };
    // SAFETY: as above.
    unsafe { libc::nanosleep(&pause, std::ptr::null_mut()) };
}

/// Up to 60 s of 5 ms naps.
const NAPS: u32 = 12_000;

/// # Safety
/// Async-signal-safe; called in a forked child only. Waits for `pid` to end or
/// stop and gives its status.
unsafe fn settle(pid: libc::pid_t) -> Option<i32> {
    // SAFETY: as above.
    unsafe {
        for _ in 0..NAPS {
            let mut status = 0;
            if libc::waitpid(pid, &mut status, libc::WUNTRACED | libc::WNOHANG) == pid {
                return Some(status);
            }
            nap();
        }
        None
    }
}

/// # Safety
/// Async-signal-safe; called in a forked child only. Kills and reaps `pid`,
/// then ends the supervisor with `code`.
unsafe fn end(pid: libc::pid_t, code: i32) -> ! {
    // SAFETY: as above; `pid` is this supervisor's own child.
    unsafe {
        libc::kill(pid, libc::SIGKILL);
        let mut status = 0;
        libc::waitpid(pid, &mut status, 0);
        libc::_exit(code)
    }
}

/// Runs `law` in a forked supervisor that becomes the pty's session leader
/// with the slave as its controlling terminal, and returns its exit code.
fn supervise(slave: &OwnedFd, law: impl FnOnce(i32) -> i32) -> i32 {
    let slave = slave.as_raw_fd();
    // SAFETY: the forked child calls only async-signal-safe functions until
    // it execs or exits.
    unsafe {
        let pid = libc::fork();
        assert!(pid >= 0, "{}", std::io::Error::last_os_error());
        if pid == 0 {
            if libc::setsid() < 0 || libc::ioctl(slave, libc::TIOCSCTTY as _, 0) < 0 {
                libc::_exit(1);
            }
            libc::_exit(law(slave));
        }
        let mut status = 0;
        assert_eq!(libc::waitpid(pid, &mut status, 0), pid);
        assert!(libc::WIFEXITED(status), "supervisor status {status:#x}");
        libc::WEXITSTATUS(status)
    }
}

// Law E: a run moved to the background during the read still restores on
// SIGTERM and dies by it instead of stopping.
#[test]
fn e_sigttou_during_restore_does_not_stop_the_run() {
    let built = build(&prompt("", "", "Never"), Backend::Cranelift, &[]);
    let exec = Exec::new(&built);
    let null = File::options().write(true).open("/dev/null").unwrap();
    // The modes are read on the master: Darwin revokes the slave's descriptors
    // when the supervisor, the terminal's session leader, exits.
    let (master, slave) = pty();
    let before = modes(&master);
    let mut gate = [0; 2];
    // SAFETY: two live result slots.
    assert_eq!(unsafe { libc::pipe(gate.as_mut_ptr()) }, 0);
    let code = supervise(&slave, |tty| {
        // SAFETY: in the forked supervisor: async-signal-safe calls only.
        unsafe {
            // The supervisor takes the foreground back later from the
            // background.
            let mut ttou: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut ttou);
            libc::sigaddset(&mut ttou, libc::SIGTTOU);
            libc::sigprocmask(libc::SIG_BLOCK, &ttou, std::ptr::null_mut());
            let pid = libc::fork();
            if pid == 0 {
                libc::setpgid(0, 0);
                let mut go = 0u8;
                libc::close(gate[1]);
                libc::read(gate[0], (&raw mut go).cast(), 1);
                exec.run(tty, null.as_raw_fd());
            }
            libc::setpgid(pid, pid);
            libc::tcsetpgrp(tty, pid);
            libc::write(gate[1], [1u8].as_ptr().cast(), 1);
            let mut t: libc::termios = std::mem::zeroed();
            let mut naps = 0;
            while libc::tcgetattr(tty, &mut t) == 0 && t.c_lflag & libc::ECHO != 0 {
                naps += 1;
                if naps == NAPS {
                    end(pid, NEVER_ARMED);
                }
                nap();
            }
            libc::tcsetpgrp(tty, libc::getpgrp());
            libc::kill(pid, libc::SIGTERM);
            match settle(pid) {
                Some(s) if libc::WIFSIGNALED(s) && libc::WTERMSIG(s) == libc::SIGTERM => {
                    SUPERVISED_OK
                }
                Some(s) if libc::WIFSTOPPED(s) => end(pid, STOPPED),
                Some(_) => OTHER_END,
                None => end(pid, STILL_RUNNING),
            }
        }
    });
    assert_eq!(code, SUPERVISED_OK, "STOPPED is {STOPPED}");
    assert_eq!(modes(&master), before);
}

// Law E2: a read started in a background group is stopped by SIGTTOU and
// leaves the modes unchanged. Killed from there as a shell's `kill %1` does
// (SIGTERM, then SIGCONT), it dies by SIGTERM and puts back nothing over the
// modes the foreground has set since.
#[test]
fn e2_a_background_read_changes_nothing() {
    let built = build(&prompt("", "", "Never"), Backend::Cranelift, &[]);
    let exec = Exec::new(&built);
    let null = File::options().write(true).open("/dev/null").unwrap();
    // Read on the master, as in law E.
    let (master, slave) = pty();
    let mut foreground = modes(&master);
    foreground.lflag ^= libc::ECHOK;
    let code = supervise(&slave, |tty| {
        // SAFETY: in the forked supervisor: async-signal-safe calls only.
        unsafe {
            let pid = libc::fork();
            if pid == 0 {
                libc::setpgid(0, 0);
                exec.run(tty, null.as_raw_fd());
            }
            libc::setpgid(pid, pid);
            match settle(pid) {
                Some(s) if libc::WIFSTOPPED(s) && libc::WSTOPSIG(s) == libc::SIGTTOU => {}
                // SIGTTIN: the modes changed and the read began.
                Some(s) if libc::WIFSTOPPED(s) => end(pid, STOPPED),
                Some(_) => return OTHER_END,
                None => end(pid, STILL_RUNNING),
            }
            // The foreground changes a mode while the read is stopped.
            let mut t: libc::termios = std::mem::zeroed();
            libc::tcgetattr(tty, &mut t);
            t.c_lflag ^= libc::ECHOK;
            libc::tcsetattr(tty, libc::TCSANOW, &t);
            libc::kill(pid, libc::SIGTERM);
            libc::kill(pid, libc::SIGCONT);
            match settle(pid) {
                Some(s) if libc::WIFSIGNALED(s) && libc::WTERMSIG(s) == libc::SIGTERM => {
                    SUPERVISED_OK
                }
                Some(s) if libc::WIFSTOPPED(s) => end(pid, STOPPED),
                Some(_) => OTHER_END,
                None => end(pid, STILL_RUNNING),
            }
        }
    });
    assert_eq!(code, SUPERVISED_OK, "STOPPED is {STOPPED}");
    assert_eq!(modes(&master), foreground);
}
