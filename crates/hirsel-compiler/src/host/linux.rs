//! Linux worker confinement uses a delegated cgroup v2, Landlock and a syscall allowlist.

use super::ConfinementSpec;
use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
#[cfg(test)]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

#[cfg(test)]
pub(crate) use tests::missing_confinement;

const SYS_LANDLOCK_CREATE_RULESET: libc::c_long = 444;
const SYS_LANDLOCK_ADD_RULE: libc::c_long = 445;
const SYS_LANDLOCK_RESTRICT_SELF: libc::c_long = 446;
static CGROUP_SEQ: AtomicU64 = AtomicU64::new(0);
#[cfg(test)]
static DISABLE_CGROUP_FOR_TEST: AtomicBool = AtomicBool::new(false);

#[cfg(test)]
pub(crate) fn disable_cgroup_for_test(disable: bool) {
    DISABLE_CGROUP_FOR_TEST.store(disable, Ordering::SeqCst);
}

const LANDLOCK_CREATE_RULESET_VERSION: u32 = 1 << 0;
const LANDLOCK_RULE_PATH_BENEATH: u32 = 1;

const LANDLOCK_ACCESS_FS_EXECUTE: u64 = 1 << 0;
const LANDLOCK_ACCESS_FS_WRITE_FILE: u64 = 1 << 1;
const LANDLOCK_ACCESS_FS_READ_FILE: u64 = 1 << 2;
const LANDLOCK_ACCESS_FS_READ_DIR: u64 = 1 << 3;
const LANDLOCK_ACCESS_FS_REMOVE_DIR: u64 = 1 << 4;
const LANDLOCK_ACCESS_FS_REMOVE_FILE: u64 = 1 << 5;
const LANDLOCK_ACCESS_FS_MAKE_CHAR: u64 = 1 << 6;
const LANDLOCK_ACCESS_FS_MAKE_DIR: u64 = 1 << 7;
const LANDLOCK_ACCESS_FS_MAKE_REG: u64 = 1 << 8;
const LANDLOCK_ACCESS_FS_MAKE_SOCK: u64 = 1 << 9;
const LANDLOCK_ACCESS_FS_MAKE_FIFO: u64 = 1 << 10;
const LANDLOCK_ACCESS_FS_MAKE_BLOCK: u64 = 1 << 11;
const LANDLOCK_ACCESS_FS_MAKE_SYM: u64 = 1 << 12;
const LANDLOCK_ACCESS_FS_REFER: u64 = 1 << 13;
const LANDLOCK_ACCESS_FS_TRUNCATE: u64 = 1 << 14;

const LANDLOCK_ACCESS_NET_BIND_TCP: u64 = 1 << 0;
const LANDLOCK_ACCESS_NET_CONNECT_TCP: u64 = 1 << 1;

const SECCOMP_SET_MODE_FILTER: libc::c_ulong = 1;
const SECCOMP_GET_ACTION_AVAIL: libc::c_ulong = 2;
const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;

#[cfg(target_arch = "x86_64")]
const NATIVE_AUDIT_ARCH: u32 = 0xc000_003e;
#[cfg(target_arch = "aarch64")]
const NATIVE_AUDIT_ARCH: u32 = 0xc000_00b7;

#[repr(C)]
struct LandlockRulesetAttr {
    handled_access_fs: u64,
    handled_access_net: u64,
    scoped: u64,
}

#[repr(C, packed)]
struct LandlockPathBeneathAttr {
    allowed_access: u64,
    parent_fd: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SockFilter {
    code: u16,
    jt: u8,
    jf: u8,
    k: u32,
}

#[repr(C)]
struct SockFprog {
    len: u16,
    filter: *const SockFilter,
}

const BPF_LD: u16 = 0x00;
const BPF_W: u16 = 0x00;
const BPF_ABS: u16 = 0x20;
const BPF_JMP: u16 = 0x05;
const BPF_JEQ: u16 = 0x10;
const BPF_K: u16 = 0x00;
const BPF_RET: u16 = 0x06;

const WORKER_SYSCALLS: &[u32] = &[
    libc::SYS_read as u32,
    libc::SYS_write as u32,
    libc::SYS_readv as u32,
    libc::SYS_writev as u32,
    libc::SYS_pread64 as u32,
    libc::SYS_pwrite64 as u32,
    libc::SYS_close as u32,
    libc::SYS_lseek as u32,
    libc::SYS_fstat as u32,
    libc::SYS_newfstatat as u32,
    libc::SYS_statx as u32,
    libc::SYS_openat as u32,
    libc::SYS_readlinkat as u32,
    libc::SYS_getdents64 as u32,
    libc::SYS_faccessat as u32,
    libc::SYS_fcntl as u32,
    libc::SYS_flock as u32,
    libc::SYS_ioctl as u32,
    libc::SYS_dup as u32,
    libc::SYS_dup3 as u32,
    libc::SYS_mmap as u32,
    libc::SYS_mprotect as u32,
    libc::SYS_munmap as u32,
    libc::SYS_mremap as u32,
    libc::SYS_madvise as u32,
    libc::SYS_brk as u32,
    libc::SYS_futex as u32,
    libc::SYS_set_tid_address as u32,
    libc::SYS_set_robust_list as u32,
    libc::SYS_rseq as u32,
    libc::SYS_rt_sigaction as u32,
    libc::SYS_rt_sigprocmask as u32,
    libc::SYS_rt_sigreturn as u32,
    libc::SYS_sigaltstack as u32,
    libc::SYS_getpid as u32,
    libc::SYS_gettid as u32,
    libc::SYS_getppid as u32,
    libc::SYS_getuid as u32,
    libc::SYS_geteuid as u32,
    libc::SYS_getgid as u32,
    libc::SYS_getegid as u32,
    libc::SYS_clock_gettime as u32,
    libc::SYS_clock_nanosleep as u32,
    libc::SYS_nanosleep as u32,
    libc::SYS_gettimeofday as u32,
    libc::SYS_getrandom as u32,
    libc::SYS_sched_getaffinity as u32,
    libc::SYS_sched_yield as u32,
    libc::SYS_getcwd as u32,
    libc::SYS_uname as u32,
    libc::SYS_sysinfo as u32,
    libc::SYS_getrusage as u32,
    libc::SYS_ftruncate as u32,
    libc::SYS_fchmod as u32,
    libc::SYS_mkdirat as u32,
    libc::SYS_unlinkat as u32,
    libc::SYS_renameat as u32,
    libc::SYS_renameat2 as u32,
    libc::SYS_fchmodat as u32,
    libc::SYS_fsync as u32,
    libc::SYS_fdatasync as u32,
    libc::SYS_exit as u32,
    libc::SYS_exit_group as u32,
    libc::SYS_ppoll as u32,
];
#[cfg(target_arch = "x86_64")]
const ARCH_SYSCALLS: &[u32] = &[
    libc::SYS_open as u32,
    libc::SYS_stat as u32,
    libc::SYS_lstat as u32,
    libc::SYS_access as u32,
    libc::SYS_readlink as u32,
    libc::SYS_arch_prctl as u32,
    libc::SYS_poll as u32,
    libc::SYS_mkdir as u32,
    libc::SYS_rmdir as u32,
    libc::SYS_unlink as u32,
    libc::SYS_rename as u32,
    libc::SYS_chmod as u32,
    libc::SYS_getrlimit as u32,
    libc::SYS_dup2 as u32,
];
#[cfg(not(target_arch = "x86_64"))]
const ARCH_SYSCALLS: &[u32] = &[];

fn limit(resource: libc::__rlimit_resource_t, soft: u64, hard: u64) -> io::Result<()> {
    let limit = libc::rlimit {
        rlim_cur: soft,
        rlim_max: hard,
    };
    // SAFETY: setrlimit reads one initialized record.
    if unsafe { libc::setrlimit(resource, &limit) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn current_limit(resource: libc::__rlimit_resource_t) -> io::Result<libc::rlimit> {
    let mut current = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: getrlimit writes one initialized record.
    if unsafe { libc::getrlimit(resource, &mut current) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(current)
}

fn lower_limit(resource: libc::__rlimit_resource_t, soft: u64, hard: u64) -> io::Result<()> {
    let current = current_limit(resource)?;
    limit(
        resource,
        soft.min(current.rlim_cur),
        hard.min(current.rlim_max),
    )
}

/// Run in the worker between `fork` and `exec`: `cpu` seconds of processor time, no
/// file longer than `file` bytes, no core dump.
pub(crate) fn confine(cpu: u64, file: u64) -> io::Result<()> {
    lower_limit(libc::RLIMIT_CPU, cpu, cpu.saturating_add(1))?;
    limit(libc::RLIMIT_CORE, 0, 0)?;
    lower_limit(libc::RLIMIT_FSIZE, file, file)?;
    // SAFETY: prctl with this option reads its one integer argument.
    if unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

struct CgroupGuard {
    dir: PathBuf,
    procs_fd: OwnedFd,
}

impl CgroupGuard {
    fn try_create(memory_bytes: u64) -> Option<Self> {
        #[cfg(test)]
        if DISABLE_CGROUP_FOR_TEST.load(Ordering::SeqCst) {
            return None;
        }
        let self_rel = self_cgroup_v2_path()?;
        let self_dir = Path::new("/sys/fs/cgroup").join(self_rel.trim_start_matches('/'));
        let candidates = [self_dir.parent().map(Path::to_path_buf), Some(self_dir)];
        for parent in candidates.into_iter().flatten() {
            if let Some(guard) = Self::try_create_under(&parent, memory_bytes) {
                return Some(guard);
            }
        }
        None
    }

    fn try_create_under(parent: &Path, memory_bytes: u64) -> Option<Self> {
        let subtree = std::fs::read_to_string(parent.join("cgroup.subtree_control")).ok()?;
        let has_memory = subtree.split_whitespace().any(|c| c == "memory");
        let has_pids = subtree.split_whitespace().any(|c| c == "pids");
        if !has_memory || !has_pids {
            return None;
        }
        // Linux cgroup v2 migration requires write access to the common ancestor's cgroup.procs.
        if !path_writable(&parent.join("cgroup.procs")) {
            return None;
        }
        let seq = CGROUP_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = parent.join(format!("hirsel-check-{}-{seq}", std::process::id()));
        std::fs::create_dir(&dir).ok()?;

        struct DirCleanup(Option<PathBuf>);
        impl Drop for DirCleanup {
            fn drop(&mut self) {
                if let Some(path) = self.0.take() {
                    let _ = std::fs::remove_dir(path);
                }
            }
        }
        let mut cleanup = DirCleanup(Some(dir.clone()));

        let controllers = std::fs::read_to_string(dir.join("cgroup.controllers")).ok()?;
        if !controllers.split_whitespace().any(|c| c == "memory")
            || !controllers.split_whitespace().any(|c| c == "pids")
        {
            return None;
        }
        std::fs::write(dir.join("memory.max"), memory_bytes.to_string()).ok()?;
        std::fs::write(dir.join("memory.swap.max"), "0").ok()?;
        let _ = std::fs::write(dir.join("memory.oom.group"), "1");
        std::fs::write(dir.join("pids.max"), "1").ok()?;

        let procs_path = CString::new(dir.join("cgroup.procs").as_os_str().as_bytes()).ok()?;
        // SAFETY: procs_path is NUL-terminated.
        let raw = unsafe { libc::open(procs_path.as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC) };
        if raw < 0 {
            return None;
        }
        // SAFETY: open returned a fresh owned descriptor.
        let procs_fd = unsafe { OwnedFd::from_raw_fd(raw) };
        cleanup.0 = None;
        Some(Self { dir, procs_fd })
    }

    fn oom_triggered(&self) -> bool {
        let Ok(events) = std::fs::read_to_string(self.dir.join("memory.events")) else {
            return false;
        };
        events.lines().any(|line| {
            let mut parts = line.split_whitespace();
            let Some(key) = parts.next() else {
                return false;
            };
            let Some(val) = parts.next().and_then(|v| v.parse::<u64>().ok()) else {
                return false;
            };
            matches!(key, "oom" | "oom_kill" | "oom_group_kill") && val > 0
        })
    }
}

impl Drop for CgroupGuard {
    fn drop(&mut self) {
        let _ = std::fs::write(self.dir.join("cgroup.kill"), "1");
        for _ in 0..8 {
            match std::fs::remove_dir(&self.dir) {
                Ok(()) => return,
                Err(err) if err.kind() == io::ErrorKind::NotFound => return,
                Err(_) => std::thread::sleep(Duration::from_millis(2)),
            }
        }
    }
}

pub(crate) struct Confinement {
    ruleset: OwnedFd,
    cgroup: CgroupGuard,
    worker: PathBuf,
    cpu_seconds: u64,
    memory_bytes: u64,
    file_bytes: u64,
}

impl Confinement {
    pub(crate) fn prepare(spec: &ConfinementSpec<'_>) -> io::Result<Self> {
        if spec.memory_bytes == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "memory cap must be non-zero",
            ));
        }
        probe_seccomp()?;
        let abi = probe_landlock()?;
        current_limit(libc::RLIMIT_DATA)?;
        let worker = std::fs::canonicalize(spec.worker)?;
        let scratch = std::fs::canonicalize(spec.scratch)?;
        let ruleset = build_landlock_ruleset(abi, &worker, &scratch)?;
        let cgroup = CgroupGuard::try_create(spec.memory_bytes).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "worker requires delegated cgroup v2 memory and pids controllers",
            )
        })?;
        Ok(Self {
            ruleset,
            cgroup,
            worker,
            cpu_seconds: spec.cpu_seconds,
            memory_bytes: spec.memory_bytes,
            file_bytes: spec.file_bytes,
        })
    }

    pub(crate) fn command(&self, worker_arg: &str) -> Command {
        let mut command = Command::new(&self.worker);
        command.arg(worker_arg);
        let ruleset_fd = self.ruleset.as_raw_fd();
        let cpu = self.cpu_seconds;
        let memory = self.memory_bytes;
        let file = self.file_bytes;
        let cgroup_procs_fd = self.cgroup.procs_fd.as_raw_fd();
        let parent = std::process::id() as libc::pid_t;
        let filter = build_seccomp_filter(true);
        // SAFETY: closure executes async-signal-safe syscalls only on
        // pre-allocated data before exec.
        unsafe {
            command.pre_exec(move || {
                join_cgroup(cgroup_procs_fd)?;
                lower_limit(libc::RLIMIT_CPU, cpu, cpu.saturating_add(1))?;
                limit(libc::RLIMIT_CORE, 0, 0)?;
                lower_limit(libc::RLIMIT_FSIZE, file, file)?;
                lower_limit(libc::RLIMIT_DATA, memory, memory)?;

                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::getppid() != parent {
                    return Err(io::Error::from_raw_os_error(libc::ESRCH));
                }
                if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::syscall(SYS_LANDLOCK_RESTRICT_SELF, ruleset_fd, 0u32) != 0 {
                    return Err(io::Error::last_os_error());
                }
                // Mark every descriptor above stderr close-on-exec (including the
                // ruleset fd, cgroup fd, and any inherited parent descriptors) while
                // keeping Rust's internal pre_exec error pipe intact until execve succeeds.
                cloexec_above_stderr()?;
                install_seccomp(&filter)?;
                Ok(())
            });
        }
        command
    }

    /// True if the kernel memory cgroup recorded an OOM event for this job.
    pub(crate) fn memory_limit_hit(&self) -> bool {
        self.cgroup.oom_triggered()
    }
}

fn self_cgroup_v2_path() -> Option<String> {
    let text = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    text.lines()
        .find_map(|line| line.strip_prefix("0::").map(str::to_owned))
}

fn path_writable(path: &Path) -> bool {
    let Ok(c_path) = CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: c_path is NUL-terminated.
    unsafe { libc::access(c_path.as_ptr(), libc::W_OK) == 0 }
}

fn join_cgroup(fd: RawFd) -> io::Result<()> {
    let payload = b"0\n";
    let mut written = 0usize;
    while written < payload.len() {
        // SAFETY: fd is an open cgroup.procs descriptor and payload slice is valid.
        let rc = unsafe {
            libc::write(
                fd,
                payload[written..].as_ptr().cast(),
                payload.len() - written,
            )
        };
        if rc < 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(err);
        }
        if rc == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "short write to cgroup.procs",
            ));
        }
        written += rc as usize;
    }
    Ok(())
}

fn probe_landlock() -> io::Result<i32> {
    // SAFETY: null attr + size 0 + VERSION flag queries kernel Landlock ABI.
    let abi = unsafe {
        libc::syscall(
            SYS_LANDLOCK_CREATE_RULESET,
            std::ptr::null::<LandlockRulesetAttr>(),
            0usize,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    };
    if abi < 3 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("worker requires Landlock ABI 3 or higher (kernel returned {abi})"),
        ));
    }
    Ok(abi as i32)
}

fn probe_seccomp() -> io::Result<()> {
    let action = SECCOMP_RET_ERRNO;
    // SAFETY: SECCOMP_GET_ACTION_AVAIL reads one u32 action value.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_seccomp,
            SECCOMP_GET_ACTION_AVAIL,
            0u32,
            &raw const action,
        )
    };
    if rc != 0 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "seccomp socket/network confinement unavailable: {}",
                io::Error::last_os_error()
            ),
        ));
    }
    Ok(())
}

fn build_landlock_ruleset(abi: i32, worker: &Path, scratch: &Path) -> io::Result<OwnedFd> {
    let handled_fs = LANDLOCK_ACCESS_FS_EXECUTE
        | LANDLOCK_ACCESS_FS_WRITE_FILE
        | LANDLOCK_ACCESS_FS_READ_FILE
        | LANDLOCK_ACCESS_FS_READ_DIR
        | LANDLOCK_ACCESS_FS_REMOVE_DIR
        | LANDLOCK_ACCESS_FS_REMOVE_FILE
        | LANDLOCK_ACCESS_FS_MAKE_CHAR
        | LANDLOCK_ACCESS_FS_MAKE_DIR
        | LANDLOCK_ACCESS_FS_MAKE_REG
        | LANDLOCK_ACCESS_FS_MAKE_SOCK
        | LANDLOCK_ACCESS_FS_MAKE_FIFO
        | LANDLOCK_ACCESS_FS_MAKE_BLOCK
        | LANDLOCK_ACCESS_FS_MAKE_SYM
        | LANDLOCK_ACCESS_FS_REFER
        | LANDLOCK_ACCESS_FS_TRUNCATE;
    let handled_net = if abi >= 4 {
        LANDLOCK_ACCESS_NET_BIND_TCP | LANDLOCK_ACCESS_NET_CONNECT_TCP
    } else {
        0
    };
    let attr = LandlockRulesetAttr {
        handled_access_fs: handled_fs,
        handled_access_net: handled_net,
        scoped: if abi >= 6 { 1 << 1 } else { 0 }, // LANDLOCK_SCOPE_SIGNAL
    };
    let attr_size = if abi >= 6 {
        std::mem::size_of::<LandlockRulesetAttr>()
    } else if abi >= 4 {
        16
    } else {
        std::mem::size_of::<u64>()
    };
    // SAFETY: attr is initialized for attr_size bytes.
    let raw = unsafe {
        libc::syscall(
            SYS_LANDLOCK_CREATE_RULESET,
            &raw const attr,
            attr_size,
            0u32,
        )
    } as RawFd;
    if raw < 0 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "landlock_create_ruleset failed: {}",
                io::Error::last_os_error()
            ),
        ));
    }
    // SAFETY: fresh owned ruleset descriptor.
    let ruleset = unsafe { OwnedFd::from_raw_fd(raw) };

    let ro_file = LANDLOCK_ACCESS_FS_EXECUTE | LANDLOCK_ACCESS_FS_READ_FILE;
    let ro_dir = ro_file | LANDLOCK_ACCESS_FS_READ_DIR;
    let rw_scratch = (LANDLOCK_ACCESS_FS_READ_FILE
        | LANDLOCK_ACCESS_FS_WRITE_FILE
        | LANDLOCK_ACCESS_FS_READ_DIR
        | LANDLOCK_ACCESS_FS_REMOVE_DIR
        | LANDLOCK_ACCESS_FS_REMOVE_FILE
        | LANDLOCK_ACCESS_FS_MAKE_DIR
        | LANDLOCK_ACCESS_FS_MAKE_REG
        | LANDLOCK_ACCESS_FS_MAKE_SYM
        | LANDLOCK_ACCESS_FS_REFER
        | LANDLOCK_ACCESS_FS_TRUNCATE)
        & handled_fs;
    let rw_null = (LANDLOCK_ACCESS_FS_READ_FILE
        | LANDLOCK_ACCESS_FS_WRITE_FILE
        | LANDLOCK_ACCESS_FS_TRUNCATE)
        & handled_fs;

    // 1. Worker / installed toolchain prefix (read-only + execute).
    if let Some(toolchain) = installed_toolchain_root(worker) {
        add_path_rule(&ruleset, &toolchain, ro_dir, ro_file)?;
    } else {
        add_path_rule(&ruleset, worker, ro_dir, ro_file)?;
        if let Some(lib_dir) = worker.parent().map(|p| p.join("lib"))
            && lib_dir.is_dir()
        {
            add_path_rule(&ruleset, &lib_dir, ro_dir, ro_file)?;
        }
    }

    // 2. Private per-job scratch directory (read-write, no execute, no sockets/devices).
    add_path_rule(&ruleset, scratch, rw_scratch, rw_null)?;

    // 3. Minimal OS runtime libraries and dynamic loader paths (read-only).
    for path in [
        "/usr/lib",
        "/usr/lib64",
        "/lib",
        "/lib64",
        "/etc/ld.so.cache",
        "/etc/ld.so.conf",
        "/etc/ld.so.conf.d",
        "/etc/ld.so.preload",
    ] {
        let p = Path::new(path);
        if let Ok(canonical) = std::fs::canonicalize(p) {
            add_path_rule(&ruleset, &canonical, ro_dir, ro_file)?;
        }
    }
    // Only the script fixtures need interpreters from outside the designated toolchain.
    #[cfg(test)]
    for path in [
        "/bin/sh",
        "/usr/bin/python3",
        "/bin/sleep",
        "/usr/bin/printf",
    ] {
        if let Ok(path) = std::fs::canonicalize(path) {
            add_path_rule(&ruleset, &path, ro_dir, ro_file)?;
        }
    }
    if Path::new("/dev/null").exists() {
        add_path_rule(&ruleset, Path::new("/dev/null"), rw_null, rw_null)?;
    }
    if Path::new("/dev/urandom").exists() {
        add_path_rule(
            &ruleset,
            Path::new("/dev/urandom"),
            LANDLOCK_ACCESS_FS_READ_FILE,
            LANDLOCK_ACCESS_FS_READ_FILE,
        )?;
    }
    Ok(ruleset)
}

fn installed_toolchain_root(worker: &Path) -> Option<PathBuf> {
    let parent = worker.parent()?;
    if parent.join("hirsel-core-toolchain.json").is_file() {
        return Some(parent.to_path_buf());
    }
    if parent.file_name().is_some_and(|n| n == "bin")
        && let Some(prefix) = parent.parent()
        && prefix.join("hirsel-core-toolchain.json").is_file()
    {
        return Some(prefix.to_path_buf());
    }
    None
}

fn add_path_rule(
    ruleset: &OwnedFd,
    path: &Path,
    dir_access: u64,
    file_access: u64,
) -> io::Result<()> {
    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in path"))?;
    // SAFETY: c_path is NUL-terminated.
    let raw = unsafe { libc::open(c_path.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: open returned a fresh descriptor.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    let is_dir = std::fs::metadata(path).map(|m| m.is_dir()).unwrap_or(false);
    let access = if is_dir { dir_access } else { file_access };
    let rule = LandlockPathBeneathAttr {
        allowed_access: access,
        parent_fd: fd.as_raw_fd(),
    };
    // SAFETY: rule is initialized and both fds are open.
    if unsafe {
        libc::syscall(
            SYS_LANDLOCK_ADD_RULE,
            ruleset.as_raw_fd(),
            LANDLOCK_RULE_PATH_BENEATH,
            &raw const rule,
            0u32,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn cloexec_above_stderr() -> io::Result<()> {
    // SAFETY: close_range with CLOSE_RANGE_CLOEXEC touches no memory and is async-signal-safe.
    if unsafe {
        libc::syscall(
            libc::SYS_close_range,
            3u32,
            u32::MAX,
            libc::CLOSE_RANGE_CLOEXEC,
        )
    } == 0
    {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() != Some(libc::ENOSYS) {
        return Err(error);
    }
    let mut rlim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    let max_fd = if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut rlim) } == 0 {
        rlim.rlim_cur.min(65_536) as RawFd
    } else {
        4096
    };
    for fd in 3..max_fd {
        // SAFETY: setting FD_CLOEXEC on inherited fds in child after fork.
        unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    }
    Ok(())
}

fn build_seccomp_filter(launch: bool) -> Vec<SockFilter> {
    // The pre-exec filter must admit the designated exec and installation of the
    // final filter. The worker drops both calls before reading the request.
    let bootstrap = [libc::SYS_execve as u32, libc::SYS_seccomp as u32];
    let allowed = WORKER_SYSCALLS
        .iter()
        .chain(ARCH_SYSCALLS)
        .chain(bootstrap.iter().take(if launch { 2 } else { 0 }));
    let mut filter = Vec::new();
    // Load seccomp_data.arch (offset 4) and kill on non-native ABI.
    filter.push(SockFilter {
        code: BPF_LD | BPF_W | BPF_ABS,
        jt: 0,
        jf: 0,
        k: 4,
    });
    filter.push(SockFilter {
        code: BPF_JMP | BPF_JEQ | BPF_K,
        jt: 1,
        jf: 0,
        k: NATIVE_AUDIT_ARCH,
    });
    filter.push(SockFilter {
        code: BPF_RET | BPF_K,
        jt: 0,
        jf: 0,
        k: SECCOMP_RET_KILL_PROCESS,
    });
    // Load seccomp_data.nr (offset 0).
    filter.push(SockFilter {
        code: BPF_LD | BPF_W | BPF_ABS,
        jt: 0,
        jf: 0,
        k: 0,
    });
    filter.extend([
        SockFilter {
            code: BPF_JMP | BPF_JEQ | BPF_K,
            jt: 0,
            jf: 4,
            k: libc::SYS_prlimit64 as u32,
        },
        SockFilter {
            code: BPF_LD | BPF_W | BPF_ABS,
            jt: 0,
            jf: 0,
            k: 16,
        },
        SockFilter {
            code: BPF_JMP | BPF_JEQ | BPF_K,
            jt: 1,
            jf: 0,
            k: 0,
        },
        SockFilter {
            code: BPF_RET | BPF_K,
            jt: 0,
            jf: 0,
            k: SECCOMP_RET_ERRNO | libc::EPERM as u32,
        },
        SockFilter {
            code: BPF_RET | BPF_K,
            jt: 0,
            jf: 0,
            k: SECCOMP_RET_ALLOW,
        },
    ]);
    // Exact native numbers only. This also rejects all x32 syscall numbers.
    for &nr in allowed {
        filter.push(SockFilter {
            code: BPF_JMP | BPF_JEQ | BPF_K,
            jt: 0,
            jf: 1,
            k: nr,
        });
        filter.push(SockFilter {
            code: BPF_RET | BPF_K,
            jt: 0,
            jf: 0,
            k: SECCOMP_RET_ALLOW,
        });
    }
    filter.push(SockFilter {
        code: BPF_RET | BPF_K,
        jt: 0,
        jf: 0,
        k: SECCOMP_RET_ERRNO | libc::EPERM as u32,
    });
    filter
}

fn install_seccomp(filter: &[SockFilter]) -> io::Result<()> {
    let prog = SockFprog {
        len: filter.len() as u16,
        filter: filter.as_ptr(),
    };
    // SAFETY: PR_SET_NO_NEW_PRIVS is already set; prog points to live instructions.
    if unsafe {
        libc::syscall(
            libc::SYS_seccomp,
            SECCOMP_SET_MODE_FILTER,
            0u32,
            &raw const prog,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(crate) fn seal_worker() -> io::Result<()> {
    install_seccomp(&build_seccomp_filter(false))
}

#[cfg(test)]
#[path = "linux_filter_tests.rs"]
mod tests;
