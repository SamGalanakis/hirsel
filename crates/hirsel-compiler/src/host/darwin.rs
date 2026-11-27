//! Darwin: kernel resource caps plus Seatbelt (`/usr/bin/sandbox-exec`)
//! filesystem allowlist and network/socket denial before processing input.
//! Uses only standard POSIX/libc symbols so `darwin-import-law` stays clean.

use super::ConfinementSpec;
use std::io;
use std::os::fd::RawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

fn limit(resource: libc::c_int, soft: u64, hard: u64) -> io::Result<()> {
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

/// Run in the worker between `fork` and `exec`: `cpu` seconds of processor time, no
/// file longer than `file` bytes, no core dump.
pub(crate) fn confine(cpu: u64, file: u64) -> io::Result<()> {
    limit(libc::RLIMIT_CPU, cpu, cpu.saturating_add(1))?;
    limit(libc::RLIMIT_CORE, 0, 0)?;
    limit(libc::RLIMIT_FSIZE, file, file)
}

/// Prepared Darwin confinement for a `check_library` worker.
pub(crate) struct Confinement {
    worker: PathBuf,
    profile: String,
    cpu_seconds: u64,
    file_bytes: u64,
}

impl Confinement {
    pub(crate) fn prepare(spec: &ConfinementSpec<'_>) -> io::Result<Self> {
        let meta = std::fs::metadata(SANDBOX_EXEC).map_err(|error| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                format!("Darwin sandbox-exec confinement unavailable: {error}"),
            )
        })?;
        if !meta.is_file() || meta.permissions().mode() & 0o111 == 0 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Darwin sandbox-exec confinement unavailable",
            ));
        }
        let worker = std::fs::canonicalize(spec.worker)?;
        let scratch = std::fs::canonicalize(spec.scratch)?;
        let profile = build_seatbelt_profile(&worker, &scratch)?;
        Ok(Self {
            worker,
            profile,
            cpu_seconds: spec.cpu_seconds,
            file_bytes: spec.file_bytes,
        })
    }

    pub(crate) fn command(&self, worker_arg: &str) -> Command {
        let mut command = Command::new(SANDBOX_EXEC);
        command
            .arg("-p")
            .arg(&self.profile)
            .arg("--")
            .arg(&self.worker)
            .arg(worker_arg);
        let cpu = self.cpu_seconds;
        let file = self.file_bytes;
        // SAFETY: async-signal-safe setrlimit and close calls only.
        unsafe {
            command.pre_exec(move || {
                limit(libc::RLIMIT_CPU, cpu, cpu.saturating_add(1))?;
                limit(libc::RLIMIT_CORE, 0, 0)?;
                limit(libc::RLIMIT_FSIZE, file, file)?;
                cloexec_above_stderr();
                Ok(())
            });
        }
        command
    }
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

fn escape_sbpl(path: &Path) -> io::Result<String> {
    let text = path.to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "non-UTF-8 path cannot be confined in SBPL profile",
        )
    })?;
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\\' | '"' => {
                out.push('\\');
                out.push(ch);
            }
            '\n' | '\r' | '\0' => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "control character in path cannot be confined in SBPL profile",
                ));
            }
            _ => out.push(ch),
        }
    }
    Ok(out)
}

fn build_seatbelt_profile(worker: &Path, scratch: &Path) -> io::Result<String> {
    let worker_escaped = escape_sbpl(worker)?;
    let scratch_escaped = escape_sbpl(scratch)?;
    let toolchain_rule = match installed_toolchain_root(worker) {
        Some(root) => {
            let escaped = escape_sbpl(&root)?;
            format!("(subpath \"{escaped}\")")
        }
        None => format!("(literal \"{worker_escaped}\")"),
    };
    Ok(format!(
        "(version 1)\n\
         (deny default)\n\
         (deny network*)\n\
         (allow process-exec (literal \"{worker_escaped}\"))\n\
         (allow sysctl-read)\n\
         (allow file-read-metadata)\n\
         (allow file-read*\n\
           {toolchain_rule}\n\
           (subpath \"/usr/lib\")\n\
           (subpath \"/System/Library\")\n\
           (subpath \"/Library/Apple\")\n\
           (subpath \"/private/var/db/dyld\")\n\
           (literal \"/dev/null\")\n\
           (literal \"/dev/urandom\")\n\
           (literal \"/dev/random\"))\n\
         (allow file-read* file-write*\n\
           (subpath \"{scratch_escaped}\")\n\
           (literal \"/dev/null\"))\n"
    ))
}

fn cloexec_above_stderr() {
    let mut rlim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    let max_fd = if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut rlim) } == 0 {
        rlim.rlim_cur.min(4096) as RawFd
    } else {
        1024
    };
    for fd in 3..max_fd {
        // SAFETY: marking inherited fds close-on-exec in child after fork.
        unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    }
}
