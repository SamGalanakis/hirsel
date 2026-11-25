//! Exec startup checks and run-owned grants.
use hirsel_core_net::exec::{ExecRules, PinnedProfile, Policy, Request};
use std::collections::BTreeMap;
use std::io;
use std::path::Path;
use std::sync::Arc;

mod launch;
mod narrow;
pub(crate) use launch::Launch;
pub use launch::{Binding, Operations};
pub(crate) use narrow::NarrowedGrant;

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

pub(crate) fn unprivileged(
    uids: [libc::uid_t; 3],
    effective: [u32; 2],
    permitted: [u32; 2],
) -> bool {
    !uids.contains(&0) && effective == [0; 2] && permitted == [0; 2]
}

/// Exec's startup checks: an unprivileged run on a host that can confine
/// opens, start children and pin their images.
pub(crate) fn probe() -> io::Result<()> {
    crate::host::exec::probe()
}
