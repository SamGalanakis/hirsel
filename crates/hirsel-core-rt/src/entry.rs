//! FORM decision 44: capabilities and Args are plain u64 provider handles.
//! Config.grants is in main parameter order; the run closes its grants after
//! main's cleanup. Grant closure cannot allocate, admit or fail.
use crate::{
    Context,
    os::OsProvider,
    process::{Config, View},
    provider::{Buffer, IoError, Provider},
};
use std::{
    ffi::OsStr,
    io,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::ffi::OsStrExt,
    },
    path::Path,
};
pub const DIR: u8 = 0;
pub const NET: u8 = 1;
pub const CLOCK: u8 = 2;
pub const RANDOM: u8 = 3;
pub const STDOUT: u8 = 4;
pub const STDERR: u8 = 5;
pub const STDIN: u8 = 6;
pub const ENV: u8 = 7;
pub const PROCESS: u8 = 8;
pub const TASKS: u8 = 9;
pub const ARGS: u8 = 10;
/// Host metadata. Its index + 1 is the numeric provider handle. Source code
/// cannot construct or inspect this metadata; copying a handle copies designation.
#[repr(C)]
pub struct Grant {
    pub kind: u8,
    pub provider: *const Provider,
    pub handle: u64,
}
#[repr(C)]
pub struct Requirement {
    pub kind: u8,
    pub index: usize,
}
/// Look up an explicitly configured kind/ordinal before entry. Zero is missing.
/// # Safety
/// Config and its immutable metadata table remain pinned by the process owner.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_entry_grant(
    config: *const Config,
    kind: u8,
    mut index: usize,
) -> u64 {
    // SAFETY: only in-range metadata entries are read.
    unsafe {
        let config = &*config;
        for n in 0..config.capabilities_len {
            if (*config.capabilities.add(n)).kind == kind {
                if index == 0 {
                    return n as u64 + 1;
                }
                index -= 1;
            }
        }
        0
    }
}
/// Check the external configuration before task startup, including repeated Dirs.
/// # Safety
/// Requirements and config are live borrowed tables with exact extents.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_entry_grants_present(
    config: *const Config,
    requirements: *const Requirement,
    len: usize,
) -> bool {
    // SAFETY: table extents are the driver's borrowed configuration inputs.
    unsafe {
        (0..len).all(|n| {
            let r = &*requirements.add(n);
            hirsel_core_entry_grant(config, r.kind, r.index) != 0
        })
    }
}
/// # Safety
/// Context belongs to this run, and handle is a live capability operand from its
/// Config.grants. Source typing distinguishes capabilities from ordinary u64s.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_capability_provider(
    context: *const Context,
    handle: u64,
) -> *const Provider {
    // SAFETY: trusted emitted operands designate the run's indexed grant metadata.
    unsafe {
        if handle & crate::host_file::DYNAMIC_GRANT != 0 {
            (*(*context).host_files).grant_provider(handle)
        } else {
            (*(*(*context).run).capabilities.add((handle - 1) as usize)).provider
        }
    }
}
/// # Safety
/// Same as capability_provider. Dir selects a pinned directory; clock/stream/task
/// markers designate their operation with provider-local handle zero.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_capability_handle(
    context: *const Context,
    handle: u64,
) -> u64 {
    // SAFETY: the run owns this immutable capability's provider-local designation.
    unsafe {
        if handle & crate::host_file::DYNAMIC_GRANT != 0 {
            (*(*context).host_files).grant_handle(handle)
        } else {
            (*(*(*context).run).capabilities.add((handle - 1) as usize)).handle
        }
    }
}
#[repr(C)]
pub struct OsConfig {
    pub root_limit: u64,
    pub args: *const View,
    pub args_len: usize,
    pub directories: *const View,
    pub directories_len: usize,
    /// Bit k grants kind k (1..9); Dirs use explicit paths, Args always exists.
    pub grant_mask: u64,
    pub stdout_fd: i32,
    pub stderr_fd: i32,
    /// Main's declaration order. Constructor refuses a missing requirement.
    pub parameters: *const Requirement,
    pub parameters_len: usize,
}
pub struct OsProcess {
    os: Option<Box<OsProvider>>,
    _provider: Box<Provider>,
    _args: Vec<Vec<u8>>,
    _carriers: Vec<Buffer>,
    _metadata: Box<[Grant]>,
    grants: Vec<u64>,
    diagnostic: Vec<u8>,
    config: Config,
}
#[repr(C)]
pub struct OpenProcess {
    pub process: *mut OsProcess,
    pub error: IoError,
}
fn duplicate(fd: i32) -> io::Result<OwnedFd> {
    // SAFETY: fcntl checks the external descriptor and transfers a fresh duplicate.
    let raw = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
    if raw < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedFd::from_raw_fd(raw) })
    }
}
unsafe fn bytes<'a>(view: View) -> &'a [u8] {
    if view.len == 0 {
        &[]
    } else {
        // SAFETY: constructor borrows the external input's exact extent.
        unsafe { std::slice::from_raw_parts(view.data, view.len) }
    }
}
enum StartupError {
    Io(io::Error),
    MissingGrant(usize),
    UnsupportedHostOpenat2(usize, io::Error),
}
impl From<io::Error> for StartupError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}
impl StartupError {
    fn into_io(self) -> io::Error {
        match self {
            Self::Io(error) | Self::UnsupportedHostOpenat2(_, error) => error,
            Self::MissingGrant(_) => io::Error::from_raw_os_error(libc::EACCES),
        }
    }
}
impl OsProcess {
    unsafe fn new(
        input: &OsConfig,
        probe: impl FnOnce(&OsProvider, u64) -> io::Result<()>,
    ) -> Result<Box<Self>, StartupError> {
        let mut os = Box::new(OsProvider::new(
            duplicate(if input.grant_mask & (1 << STDOUT) != 0 {
                input.stdout_fd
            } else {
                input.stderr_fd
            })?,
            duplicate(input.stderr_fd)?,
        ));
        let mut dirs = Vec::new();
        // SAFETY: copy input bytes and pin descriptors; no input pointer escapes.
        unsafe {
            for index in 0..input.directories_len {
                dirs.push(os.grant(Path::new(OsStr::from_bytes(bytes(
                    *input.directories.add(index),
                ))))?);
            }
            let mut args = Vec::with_capacity(input.args_len);
            for index in 0..input.args_len {
                let value = bytes(*input.args.add(index));
                std::str::from_utf8(value)
                    .map_err(|_| io::Error::from_raw_os_error(libc::EILSEQ))?;
                args.push(value.to_vec());
            }
            let views = args
                .iter()
                .map(|a| Buffer {
                    data: a.as_ptr().cast_mut(),
                    len: a.len(),
                    capacity: 0,
                })
                .collect::<Vec<_>>();
            let provider = Box::new(os.provider());
            let provider_pointer = &*provider as *const Provider;
            let mut metadata = Vec::new();
            for handle in dirs {
                metadata.push(Grant {
                    kind: DIR,
                    provider: provider_pointer,
                    handle,
                });
            }
            for kind in NET..=TASKS {
                if input.grant_mask & (1 << kind) != 0 {
                    metadata.push(Grant {
                        kind,
                        provider: provider_pointer,
                        handle: 0,
                    });
                }
            }
            metadata.push(Grant {
                kind: ARGS,
                provider: provider_pointer,
                handle: 0,
            });
            let metadata = metadata.into_boxed_slice();
            let config = Config {
                root_limit: input.root_limit,
                args: views.as_ptr(),
                args_len: views.len(),
                grants: std::ptr::null(),
                grants_len: 0,
                provider: provider_pointer,
                state: std::ptr::null_mut(),
                diagnostic,
                describe: Some(describe),
                scheduler: crate::tasks::SchedulerProvider::default(),
                stop_fd: os.stderr.as_raw_fd(),
                capabilities: metadata.as_ptr(),
                capabilities_len: metadata.len(),
                close_grants: Some(close_grants),
            };
            let mut process = Box::new(Self {
                os: Some(os),
                _provider: provider,
                _args: args,
                _carriers: views,
                _metadata: metadata,
                grants: Vec::with_capacity(input.parameters_len),
                diagnostic: vec![],
                config,
            });
            let mut probe = Some(probe);
            for n in 0..input.parameters_len {
                let requirement = &*input.parameters.add(n);
                let handle =
                    hirsel_core_entry_grant(&process.config, requirement.kind, requirement.index);
                if handle == 0 {
                    return Err(StartupError::MissingGrant(n));
                }
                if requirement.kind == DIR {
                    if let Some(probe) = probe.take() {
                        let designation = process._metadata[(handle - 1) as usize].handle;
                        probe(process.os.as_ref().unwrap(), designation)
                            .map_err(|error| StartupError::UnsupportedHostOpenat2(n, error))?;
                    }
                }
                process.grants.push(handle);
            }
            process.config.grants = process.grants.as_ptr();
            process.config.grants_len = process.grants.len();
            process.config.state = (&mut *process as *mut Self).cast();
            Ok(process)
        }
    }
}
/// Create explicit grants and main's ordered u64 operand table. Copies valid
/// UTF-8 argv and pins paths/stream descriptors. Failure closes every prefix.
/// # Safety
/// Config/input tables are borrowed only for this call, with exact extents.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_os_process_new(config: *const OsConfig) -> OpenProcess {
    // SAFETY: constructor copies/pins all configuration before returning its owner.
    match unsafe { OsProcess::new(&*config, OsProvider::probe_directory) } {
        Ok(process) => OpenProcess {
            process: Box::into_raw(process),
            error: IoError::OK,
        },
        Err(error) => OpenProcess {
            process: std::ptr::null_mut(),
            error: IoError::from_errno(error.into_io().raw_os_error().unwrap_or(libc::EIO)),
        },
    }
}
/// # Safety
/// Live process; returned config/table is borrowed until free. Run it once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_os_process_config(process: *const OsProcess) -> *const Config {
    // SAFETY: owner pins Config and every referenced table.
    unsafe { &raw const (*process).config }
}
/// # Safety
/// Consume the sole constructor owner after run and every operand/view has ended.
/// Run already closes the grants; this frees host argv/metadata and unused setup.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_os_process_free(process: *mut OsProcess) {
    // SAFETY: transfers ownership of the process object exactly once.
    unsafe {
        drop(Box::from_raw(process));
    }
}
unsafe extern "C" fn close_grants(state: *mut u8) {
    // SAFETY: main and all descendants have ended. Descriptor drop allocates and
    // admits nothing, ignores OS close errors, and cannot add a language cause.
    unsafe {
        let process = &mut *state.cast::<OsProcess>();
        drop(process.os.take());
    }
}
unsafe extern "C" fn diagnostic(state: *mut u8, data: *const u8, len: usize) {
    // SAFETY: run owns its sink until diagnostics finish, before grant closure.
    unsafe {
        let process = &*state.cast::<OsProcess>();
        crate::os::diagnostic(process.os.as_ref().unwrap().stderr.as_raw_fd(), data, len);
    }
}
unsafe extern "C" fn describe(state: *mut u8, cause: *const crate::causes::Cause) -> View {
    // SAFETY: run pins its cause and diagnostic buffer until the next describe.
    unsafe {
        let process = &mut *state.cast::<OsProcess>();
        process.diagnostic = crate::abi_host::format_cause(&*cause).into_bytes();
        View {
            data: process.diagnostic.as_ptr(),
            len: process.diagnostic.len(),
        }
    }
}

/// Create the OS process and run a named main, or report one PROG-3 not-started
/// line with status 103. Parameter names correspond to OsConfig.parameters.
/// # Safety
/// All input/name views remain borrowed for this call; entry follows the run ABI.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_os_run(
    input: *const OsConfig,
    parameter_names: *const View,
    entry: crate::process::Entry,
) -> u8 {
    // SAFETY: constructor copies all configuration; entry and names are pinned by caller.
    unsafe { run_with_probe(&*input, parameter_names, entry, OsProvider::probe_directory) }
}
unsafe fn run_with_probe(
    input: &OsConfig,
    names: *const View,
    entry: crate::process::Entry,
    probe: impl FnOnce(&OsProvider, u64) -> io::Result<()>,
) -> u8 {
    // SAFETY: input tables are borrowed from the OS entry's configuration.
    match unsafe { OsProcess::new(input, probe) } {
        Ok(process) => {
            // SAFETY: the Box pins Config and all grants through run and cleanup.
            unsafe { crate::process::hirsel_core_run(&process.config, entry) }
        }
        Err(error) => {
            let mut line = b"not started: ".to_vec();
            match error {
                StartupError::MissingGrant(index)
                | StartupError::UnsupportedHostOpenat2(index, _) => {
                    line.extend_from_slice(b"grant `");
                    // SAFETY: emitter supplies one valid UTF-8 name per main parameter.
                    line.extend_from_slice(unsafe { bytes(*names.add(index)) });
                    line.extend_from_slice(match error {
                        StartupError::MissingGrant(_) => b"` is not in the run's configuration\n",
                        _ => b"`: this host does not permit openat2, which a Dir needs (Linux 5.6 or later, allowed by every sandbox in force)\n",
                    });
                }
                StartupError::Io(error) => {
                    line.extend_from_slice(
                        format!("host grants could not be configured: {error}\n").as_bytes(),
                    );
                }
            }
            // SAFETY: the configured diagnostic fd and line live through this write.
            unsafe { crate::os::diagnostic(input.stderr_fd, line.as_ptr(), line.len()) };
            crate::process::NOT_STARTED
        }
    }
}

#[cfg(test)]
#[path = "startup_host_tests.rs"]
mod startup_host_tests;
