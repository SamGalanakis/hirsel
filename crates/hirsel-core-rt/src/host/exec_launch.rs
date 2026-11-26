use super::Grant;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use super::probe;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::host::exec::Supervisor;
use crate::{
    entry::{OsConfig, StartupError},
    process::View,
};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use hirsel_core_ids::launch::{FIRST_INHERITED_FD, INHERITED_SLOT};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use hirsel_core_net::exec::Request;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::os::unix::ffi::OsStrExt;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::path::Path;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::sync::Arc;

#[repr(C)]
pub struct Binding {
    pub request: View,
    pub workdir: View,
    pub digest: [u8; 32],
}

pub struct Operations {
    pub(crate) load: unsafe fn(&OsConfig, &[&[u8]], bool, bool) -> Result<Launch, StartupError>,
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

/// The bindings `input` seals. A binding's inherited workdir is at its slot's
/// descriptor, numbered after every directory slot.
#[cfg(any(target_os = "linux", target_os = "macos"))]
unsafe fn load(
    input: &OsConfig,
    approvals: &[&[u8]],
    preview: bool,
    unix: bool,
) -> Result<Launch, StartupError> {
    // SAFETY: the launcher's table holds `exec_bindings_len` bindings.
    let bindings =
        unsafe { std::slice::from_raw_parts(input.exec_bindings, input.exec_bindings_len) };
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
        let workdir = view(binding.workdir);
        let policy = if workdir == INHERITED_SLOT.as_bytes() {
            let fd = FIRST_INHERITED_FD + (input.directories_len + index) as i32;
            let source =
                crate::entry::take_inherited(fd).and_then(|dir| read_beneath(&dir, &request.file));
            Grant::check(&request, request.read(source), binding.digest)
        } else {
            let base = Path::new(std::ffi::OsStr::from_bytes(workdir));
            Grant::check(&request, request.load_from(base), binding.digest)
        }
        .map_err(StartupError::Exec)?;
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

    let prepare_host = if unix { probe_with_unix } else { probe };
    let supervisor = Arc::new(
        Supervisor::start(prepare_host).map_err(|error| StartupError::Exec(error.to_string()))?,
    );
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

/// The policy file `file` names under the inherited workdir `dir`, as a path
/// under that workdir would name it: an absolute `file` names itself.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn read_beneath(dir: &OwnedFd, file: &str) -> std::io::Result<String> {
    use std::io::Read;
    let name = std::ffi::CString::new(file)
        .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
    // SAFETY: `dir` is open and `name` is NUL-terminated; the result is owned below.
    let raw = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC,
        )
    };
    if raw < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: openat returned a fresh descriptor.
    let mut file = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(raw) });
    let mut source = String::new();
    file.read_to_string(&mut source)?;
    Ok(source)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn probe_with_unix() -> std::io::Result<()> {
    probe()?;
    hirsel_core_net::unix::probe()
}

#[cfg(target_os = "ios")]
unsafe fn load(_: &OsConfig, _: &[&[u8]], _: bool, _: bool) -> Result<Launch, StartupError> {
    Err(std::io::Error::from_raw_os_error(libc::ENOTSUP).into())
}
