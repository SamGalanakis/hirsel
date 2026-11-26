//! Exec startup checks and run-owned grants.
use hirsel_core_net::exec::ExecRules;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use hirsel_core_net::exec::{PinnedProfile, Policy, Request};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::collections::BTreeMap;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::io;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::sync::Arc;

#[path = "exec_launch.rs"]
mod launch;
#[path = "../exec/narrow.rs"]
mod narrow;
pub(crate) use launch::Launch;
pub use launch::{Binding, Operations};
pub(crate) use narrow::NarrowedGrant;

pub(crate) enum Grant {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
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
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            Self::Native { rules, .. } => rules,
            Self::Served { rules } => rules,
        }
    }

    fn with_rules(&self, rules: ExecRules) -> Self {
        match self {
            #[cfg(any(target_os = "linux", target_os = "macos"))]
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

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn check(
        request: &Request,
        policy: Result<Policy, String>,
        expected: [u8; 32],
    ) -> Result<Policy, String> {
        let policy = policy?;
        if policy.digest != expected {
            return Err(format!(
                "Exec[{}]: launch policy digest differs from the build binding",
                request.names.join(",")
            ));
        }
        Ok(policy)
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
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

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn unprivileged(
    uids: [libc::uid_t; 3],
    effective: [u32; 2],
    permitted: [u32; 2],
) -> bool {
    !uids.contains(&0) && effective == [0; 2] && permitted == [0; 2]
}

/// Exec's startup checks: an unprivileged run on a host that can confine
/// opens, start children and pin their images.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn probe() -> io::Result<()> {
    crate::host::exec::probe()
}
