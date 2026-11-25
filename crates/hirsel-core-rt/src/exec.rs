//! Exec startup checks and run-owned grants.
use hirsel_core_net::exec::{ExecRules, PinnedProfile, Policy, Request};
use std::collections::BTreeMap;
use std::fs::File;
use std::io;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;
use std::sync::Arc;

mod launch;
mod narrow;
pub(crate) use launch::Launch;
pub use launch::{Binding, Operations};
pub(crate) use narrow::NarrowedGrant;

const EXEC_HOST: &str =
    "Exec requires clone3 with CLONE_PIDFD and close_range (Linux 5.9 or later)";

pub(crate) enum Grant {
    Native {
        supervisor: Arc<crate::host::exec::Supervisor>,
        profiles: Arc<BTreeMap<String, PinnedProfile>>,
        rules: ExecRules,
    },
    Served {
        rules: ExecRules,
    },
}

impl Grant {
    pub(crate) fn rules(&self) -> &ExecRules {
        match self {
            Self::Native { rules, .. } | Self::Served { rules } => rules,
        }
    }

    fn with_rules(&self, rules: ExecRules) -> Self {
        match self {
            Self::Native {
                supervisor,
                profiles,
                ..
            } => Self::Native {
                supervisor: supervisor.clone(),
                profiles: profiles.clone(),
                rules,
            },
            Self::Served { .. } => Self::Served { rules },
        }
    }

    fn verify(request: &Request, workdir: &Path, expected: [u8; 32]) -> Result<Policy, String> {
        let policy = request.load_from(workdir)?;
        if policy.digest != expected {
            return Err(format!(
                "Exec[{}]: launch policy digest differs from the build binding",
                request.names.join(",")
            ));
        }
        Ok(policy)
    }

    fn prepare(
        policy: Policy,
        supervisor: Arc<crate::host::exec::Supervisor>,
    ) -> Result<(Self, Vec<String>), String> {
        let rules = policy.rules();
        let (profiles, lines) = policy.pin_images()?;
        Ok((
            Self::Native {
                supervisor,
                profiles: Arc::new(profiles),
                rules,
            },
            lines,
        ))
    }
}

#[repr(C)]
struct CapHeader {
    version: u32,
    pid: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Caps {
    effective: u32,
    permitted: u32,
    inheritable: u32,
}

pub(crate) fn unprivileged(
    uids: [libc::uid_t; 3],
    effective: [u32; 2],
    permitted: [u32; 2],
) -> bool {
    !uids.contains(&0) && effective == [0; 2] && permitted == [0; 2]
}

pub(crate) fn check_credentials() -> Result<(), &'static str> {
    let [mut real, mut effective, mut saved] = [0; 3];
    let mut header = CapHeader {
        version: 0x2008_0522,
        pid: 0,
    };
    let mut caps = [Caps::default(); 2];
    // SAFETY: the version-3 capabilities ABI reads and writes the header and two data rows.
    let ok = unsafe {
        libc::getresuid(&mut real, &mut effective, &mut saved) == 0
            && libc::syscall(libc::SYS_capget, &mut header, caps.as_mut_ptr()) == 0
    };
    if !ok
        || !unprivileged(
            [real, effective, saved],
            caps.map(|cap| cap.effective),
            caps.map(|cap| cap.permitted),
        )
    {
        return Err("Exec requires an unprivileged run");
    }
    Ok(())
}

fn probe_clone() -> bool {
    let mut pidfd = -1i32;
    let mut args = [0u64; 11];
    args[0] = libc::CLONE_PIDFD as u64;
    args[1] = (&raw mut pidfd) as u64;
    args[4] = libc::SIGCHLD as u64;
    // SAFETY: clone3 uses the Linux v2 clone_args layout and writes the pidfd to pidfd.
    // The probe child owns its descriptor table and makes only syscalls before exiting.
    unsafe {
        let result = libc::syscall(
            libc::SYS_clone3,
            args.as_ptr(),
            std::mem::size_of_val(&args),
        );
        if result < 0 {
            return false;
        }
        if result == 0 {
            let ok = libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 0u32) == 0
                && libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) == 0;
            libc::_exit(i32::from(!ok));
        }
        let fd = OwnedFd::from_raw_fd(pidfd);
        let mut info: libc::siginfo_t = std::mem::zeroed();
        loop {
            if libc::waitid(
                libc::P_PIDFD,
                fd.as_raw_fd() as u32,
                &mut info,
                libc::WEXITED,
            ) == 0
            {
                return info.si_code == libc::CLD_EXITED && info.si_status() == 0;
            }
            if io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                return false;
            }
        }
    }
}

pub(crate) fn probe() -> io::Result<()> {
    check_credentials().map_err(io::Error::other)?;
    let openat2 = File::open(".").and_then(|root| {
        hirsel_core_net::unix::open_beneath(root.as_fd(), c".", crate::host::path::DIRECTORY_HANDLE)
    });
    if openat2.is_err() || !probe_clone() || hirsel_core_net::exec::probe_seals().is_err() {
        return Err(io::Error::other(EXEC_HOST));
    }
    Ok(())
}
