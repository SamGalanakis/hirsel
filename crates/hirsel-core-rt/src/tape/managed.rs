use super::{Fault, Tape};
use crate::entry::{self, HostLaunch};
use crate::{Context, process::Config};

/// Install the logical host control socket and export directory before the
/// generated entry runs. They occupy ordinary tape objects in both modes.
pub(super) unsafe fn install(
    context: *mut Context,
    config: *const Config,
    host: Option<&HostLaunch>,
    tape: &Tape,
) -> Result<(), Fault> {
    // SAFETY: the caller pins the run's context, provider and launch handles.
    unsafe {
        let files = (*context).host_files;
        let provider = *(*config).provider;
        let scratch = match host {
            Some(host) => (
                *entry::hirsel_core_capability_provider(context, host.scratch),
                entry::hirsel_core_capability_handle(context, host.scratch),
            ),
            None => (provider, 0),
        };
        let (p, h) = tape.on_tape(entry::DIR, scratch)?;
        let scratch = (*files).registry.grant(p, h);
        let control = match host {
            Some(host) => {
                let fd = host.control.lock().unwrap().take().ok_or(Fault::Corrupt)?;
                let _commit = crate::sequencer::enter();
                crate::os_net::adopt_socket(&raw mut (*files).sockets, fd).map_err(|error| {
                    Fault::Io(crate::provider::IoError::from_errno(
                        error.raw_os_error().unwrap_or(libc::EIO),
                    ))
                })?
            }
            None => 0,
        };
        let (_, handle) = tape.on_tape(entry::NET, (provider, control))?;
        (*files).managed_control = (u64::from(handle as u32) << 2, scratch);
        Ok(())
    }
}
