use super::{Grant, probe};
use crate::os_exec::Supervisor;
use crate::{entry::StartupError, process::View};
use hirsel_core_net::exec::Request;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::Arc;

#[repr(C)]
pub struct Binding {
    pub request: View,
    pub workdir: View,
    pub digest: [u8; 32],
}

pub struct Operations {
    pub(crate) load: unsafe fn(&[Binding], &[&[u8]], bool) -> Result<Launch, StartupError>,
}

#[unsafe(no_mangle)]
pub static hirsel_core_os_exec_ops: Operations = Operations { load };

pub(crate) struct Launch {
    grants: Vec<Grant>,
    pub(crate) lines: Vec<String>,
}

impl Launch {
    pub(crate) fn handles(&self) -> impl Iterator<Item = u64> + '_ {
        self.grants
            .iter()
            .map(|grant| std::ptr::from_ref(grant) as u64)
    }
}

unsafe fn load(
    bindings: &[Binding],
    approvals: &[&[u8]],
    preview: bool,
) -> Result<Launch, StartupError> {
    let mut requests = Vec::with_capacity(bindings.len());
    for (index, binding) in bindings.iter().enumerate() {
        // SAFETY: the caller pins all request bytes in its link-time table.
        let view = |view: View| {
            if view.len == 0 {
                &[]
            } else {
                // SAFETY: the launcher pins the view through startup.
                unsafe { std::slice::from_raw_parts(view.data, view.len) }
            }
        };
        let request = view(binding.request);
        let text = std::str::from_utf8(request)
            .map_err(|_| StartupError::Exec("invalid linked Exec binding".into()))?;
        let request = Request::parse(text).map_err(|msg| StartupError::Exec(msg.into()))?;
        if approvals.get(index).copied() != Some(text.as_bytes()) {
            return Err(StartupError::Exec(format!(
                "not granted: Exec[{}]: pass --grant 'Exec={text}'",
                request.names.join(",")
            )));
        }
        let base = Path::new(std::ffi::OsStr::from_bytes(view(binding.workdir)));
        let policy = Grant::verify(&request, base, binding.digest).map_err(StartupError::Exec)?;
        requests.push(policy);
    }
    if approvals.len() != requests.len() {
        return Err(StartupError::Exec(
            "Exec approval has no matching build binding".into(),
        ));
    }
    if preview {
        return Ok(Launch {
            grants: vec![],
            lines: vec![],
        });
    }

    let supervisor =
        Arc::new(Supervisor::start(probe).map_err(|error| StartupError::Exec(error.to_string()))?);
    let mut grants = Vec::with_capacity(requests.len());
    let mut lines = Vec::new();
    for policy in requests {
        let (grant, approval) =
            Grant::prepare(policy, supervisor.clone()).map_err(StartupError::Exec)?;
        grants.push(grant);
        lines.extend(approval);
    }
    Ok(Launch { grants, lines })
}
