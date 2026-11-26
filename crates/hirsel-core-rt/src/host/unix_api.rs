use crate::os::OsProvider;
use crate::provider::{Outcome, Request, Step};

pub(crate) struct Operations {
    pub step: unsafe fn(&OsProvider, &mut Request, u64, bool) -> Step,
    pub abort: unsafe fn(&OsProvider, &mut Request, u64, u8) -> Outcome,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) use super::unix_native::configure_roots;
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) use super::unix_native::{AFTER_RESOLVE, accept, admitted, bound};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use super::unix_native::{abort, step};

pub(crate) static OPERATIONS: Operations = Operations { step, abort };

#[cfg(target_os = "ios")]
unsafe fn step(_: &OsProvider, _: &mut Request, _: u64, _: bool) -> Step {
    Step {
        outcome: crate::os_net::error(libc::ENOTSUP),
        ..Step::default()
    }
}

#[cfg(target_os = "ios")]
unsafe fn abort(_: &OsProvider, _: &mut Request, _: u64, _: u8) -> Outcome {
    crate::os_net::error(libc::ENOTSUP)
}

#[cfg(target_os = "ios")]
pub(crate) unsafe fn configure_roots(
    _: &mut OsProvider,
    rows: &[hirsel_core_net::RuleView],
) -> Result<(), crate::entry::StartupError> {
    if rows.iter().any(|row| row.is_unix() || row.is_unix_root()) {
        return Err(std::io::Error::from_raw_os_error(libc::ENOTSUP).into());
    }
    Ok(())
}
