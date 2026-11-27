//! Linux: kernel-enforced aggregate resource caps (delegated cgroup v2
//! `memory.max` + `memory.swap.max=0` + `pids.max=1`, or `RLIMIT_AS` +
//! `RLIMIT_DATA` combined with single-process seccomp confinement), Landlock
//! read-only allowlist + private scratch confinement, and seccomp-bpf
//! socket/network/process-creation denial before `exec`.

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

pub(crate) const CONFINEMENT_SUPPORTED: bool = true;

const SYS_LANDLOCK_CREATE_RULESET: libc::c_long = 444;
const SYS_LANDLOCK_ADD_RULE: libc::c_long = 445;
const SYS_LANDLOCK_RESTRICT_SELF: libc::c_long = 446;
const SYS_IO_URING_SETUP: u32 = 425;
const SYS_CLONE3: u32 = 435;

/// Fixed virtual address space reserve for loader, libc, main stack, and guard
/// pages when falling back to `RLIMIT_AS` without a delegated cgroup v2.
const BASE_VIRTUAL_HEADROOM: u64 = 64 << 20;

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

const DENIED_SYSCALLS: &[u32] = &[
    libc::SYS_socket as u32,
    libc::SYS_socketpair as u32,
    libc::SYS_connect as u32,
    libc::SYS_bind as u32,
    libc::SYS_listen as u32,
    libc::SYS_accept as u32,
    libc::SYS_accept4 as u32,
    libc::SYS_sendto as u32,
    libc::SYS_recvfrom as u32,
    libc::SYS_sendmsg as u32,
    libc::SYS_recvmsg as u32,
    libc::SYS_sendmmsg as u32,
    libc::SYS_recvmmsg as u32,
    SYS_IO_URING_SETUP,
    libc::SYS_ptrace as u32,
    libc::SYS_process_vm_readv as u32,
    libc::SYS_process_vm_writev as u32,
    libc::SYS_mount as u32,
    libc::SYS_umount2 as u32,
    libc::SYS_pivot_root as u32,
    libc::SYS_unshare as u32,
    libc::SYS_setns as u32,
    libc::SYS_clone as u32,
    SYS_CLONE3,
];

#[cfg(target_arch = "x86_64")]
const ARCH_DENIED_SYSCALLS: &[u32] = &[libc::SYS_fork as u32, libc::SYS_vfork as u32];
#[cfg(not(target_arch = "x86_64"))]
const ARCH_DENIED_SYSCALLS: &[u32] = &[];

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

fn lower_limit(resource: libc::__rlimit_resource_t, requested: u64) -> io::Result<()> {
    let current = current_limit(resource)?;
    let capped = requested.min(current.rlim_max);
    limit(resource, capped, capped)
}

/// Run in the worker between `fork` and `exec`: `cpu` seconds of processor time, no
/// file longer than `file` bytes, no core dump.
pub(crate) fn confine(cpu: u64, file: u64) -> io::Result<()> {
    limit(libc::RLIMIT_CPU, cpu, cpu.saturating_add(1))?;
    limit(libc::RLIMIT_CORE, 0, 0)?;
    limit(libc::RLIMIT_FSIZE, file, file)?;
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
        match std::fs::write(dir.join("memory.swap.max"), "0") {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return None,
        }
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

enum MemoryCap {
    /// Delegated cgroup v2 enforcing aggregate `memory.max`, `memory.swap.max=0`,
    /// and `pids.max=1`.
    Cgroup(CgroupGuard),
    /// Per-process `RLIMIT_AS` (bounding heap, anonymous/shared `mmap`, and stacks)
    /// + `RLIMIT_DATA`, combined with seccomp denial of `fork`/`vfork`/`clone`/`clone3`
    /// so per-process caps are aggregate.
    RlimitAs { as_bytes: u64 },
}

/// Prepared Linux confinement for a `check_library` worker: holds the Landlock
/// ruleset descriptor and optional per-job cgroup until after the worker finishes.
pub(crate) struct Confinement {
    ruleset: OwnedFd,
    memory_cap: MemoryCap,
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
        let memory_cap = match CgroupGuard::try_create(spec.memory_bytes) {
            Some(cgroup) => MemoryCap::Cgroup(cgroup),
            None => {
                current_limit(libc::RLIMIT_AS).map_err(|error| {
                    io::Error::new(
                        io::ErrorKind::Unsupported,
                        format!("RLIMIT_AS memory confinement unavailable: {error}"),
                    )
                })?;
                MemoryCap::RlimitAs {
                    as_bytes: spec
                        .memory_bytes
                        .saturating_add(worker_virtual_headroom(&worker)),
                }
            }
        };
        Ok(Self {
            ruleset,
            memory_cap,
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
        let (cgroup_procs_fd, rlimit_as) = match &self.memory_cap {
            MemoryCap::Cgroup(cg) => (Some(cg.procs_fd.as_raw_fd()), None),
            MemoryCap::RlimitAs { as_bytes } => (None, Some(*as_bytes)),
        };
        let parent = std::process::id() as libc::pid_t;
        let filter = build_seccomp_filter();
        // SAFETY: closure executes async-signal-safe syscalls only on
        // pre-allocated data before exec.
        unsafe {
            command.pre_exec(move || {
                if let Some(cg_fd) = cgroup_procs_fd {
                    join_cgroup(cg_fd)?;
                }
                limit(libc::RLIMIT_CPU, cpu, cpu.saturating_add(1))?;
                limit(libc::RLIMIT_CORE, 0, 0)?;
                limit(libc::RLIMIT_FSIZE, file, file)?;
                lower_limit(libc::RLIMIT_DATA, memory)?;
                if let Some(as_bytes) = rlimit_as {
                    lower_limit(libc::RLIMIT_AS, as_bytes)?;
                }

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
        match &self.memory_cap {
            MemoryCap::Cgroup(cg) => cg.oom_triggered(),
            MemoryCap::RlimitAs { .. } => false,
        }
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

fn dir_so_bytes(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut total = 0u64;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let bytes = name.as_bytes();
        let is_so = bytes.ends_with(b".so") || bytes.windows(4).any(|w| w == b".so.");
        if is_so
            && let Ok(meta) = entry.metadata()
            && meta.is_file()
        {
            total = total.saturating_add(meta.len());
        }
    }
    total
}

fn worker_virtual_headroom(worker: &Path) -> u64 {
    let exe_bytes = std::fs::metadata(worker).map(|m| m.len()).unwrap_or(0);
    let lib_bytes = if let Some(root) = installed_toolchain_root(worker) {
        dir_so_bytes(&root.join("lib"))
    } else if let Some(parent) = worker.parent() {
        dir_so_bytes(&parent.join("lib"))
    } else {
        0
    };
    BASE_VIRTUAL_HEADROOM
        .saturating_add(exe_bytes)
        .saturating_add(lib_bytes)
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
    if abi < 1 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "Landlock filesystem confinement unavailable: {}",
                io::Error::last_os_error()
            ),
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
    let mut handled_fs = LANDLOCK_ACCESS_FS_EXECUTE
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
        | LANDLOCK_ACCESS_FS_MAKE_SYM;
    if abi >= 2 {
        handled_fs |= LANDLOCK_ACCESS_FS_REFER;
    }
    if abi >= 3 {
        handled_fs |= LANDLOCK_ACCESS_FS_TRUNCATE;
    }
    let handled_net = if abi >= 4 {
        LANDLOCK_ACCESS_NET_BIND_TCP | LANDLOCK_ACCESS_NET_CONNECT_TCP
    } else {
        0
    };
    let attr = LandlockRulesetAttr {
        handled_access_fs: handled_fs,
        handled_access_net: handled_net,
    };
    let attr_size = if abi >= 4 {
        std::mem::size_of::<LandlockRulesetAttr>()
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
        "/usr/libexec",
        "/usr/bin",
        "/bin",
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

fn build_seccomp_filter() -> Vec<SockFilter> {
    let total = DENIED_SYSCALLS.len() + ARCH_DENIED_SYSCALLS.len();
    let mut filter = Vec::with_capacity(total + 6);
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
    for (index, &nr) in DENIED_SYSCALLS
        .iter()
        .chain(ARCH_DENIED_SYSCALLS.iter())
        .enumerate()
    {
        let remaining = (total - index) as u8;
        // On match, jump past remaining checks + ALLOW to the DENY instruction.
        filter.push(SockFilter {
            code: BPF_JMP | BPF_JEQ | BPF_K,
            jt: remaining,
            jf: 0,
            k: nr,
        });
    }
    // Default allow.
    filter.push(SockFilter {
        code: BPF_RET | BPF_K,
        jt: 0,
        jf: 0,
        k: SECCOMP_RET_ALLOW,
    });
    // Denied syscall: return EPERM.
    filter.push(SockFilter {
        code: BPF_RET | BPF_K,
        jt: 0,
        jf: 0,
        k: SECCOMP_RET_ERRNO | (libc::EPERM as u32),
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
