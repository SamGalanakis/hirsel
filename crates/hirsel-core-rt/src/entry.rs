//! FORM decision 44: capabilities and Args are plain u64 provider handles.
//! Config.grants is in main parameter order; the run closes its grants after
//! main's cleanup. Grant closure cannot allocate, admit or fail.
use crate::{
    Context,
    os::{DirectoryOperations, OsProvider, hirsel_core_os_directory_ops},
    os_net::hirsel_core_os_socket_activation,
    process::{Config, View},
    provider::{Buffer, IoError, Provider},
    stop_signals::{StopSignals, hirsel_core_os_process_ops},
};
use hirsel_core_ids::launch::Metadata;
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
pub const SECRET: u8 = 11;
/// Host metadata. Its index + 1 is the numeric provider handle. Source code
/// cannot construct or inspect this metadata; copying a handle copies designation.
#[repr(C)]
#[derive(Clone, Copy)]
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
            (*(*context).host_files).registry.grant_provider(handle)
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
            (*(*context).host_files).registry.grant_handle(handle)
        } else {
            (*(*(*context).run).capabilities.add((handle - 1) as usize)).handle
        }
    }
}
/// Operations required by a live Process grant. The generated launcher names
/// this table only when main can receive Process.
pub struct ProcessOperations {
    pub(crate) install: fn() -> io::Result<StopSignals>,
}

/// Receives a launcher-owned listener during startup for an OS Net grant.
pub struct SocketActivation {
    pub(crate) receive: unsafe fn() -> io::Result<Option<OwnedFd>>,
}

#[repr(C)]
#[derive(Clone, Copy)]
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
    pub net_rules: *const hirsel_core_net::RuleView,
    pub net_rules_len: usize,
    /// Executable target, emitted by the linker for either native backend.
    pub target: View,
    /// Installed by the native linker for a launch with directory grants.
    pub directory_ops: *const DirectoryOperations,
    /// Installed by the native linker for a launch with a live Process grant.
    pub process_ops: *const ProcessOperations,
    /// Installed by the native linker for a launch with a Net grant.
    pub socket_activation: *const SocketActivation,
    pub secret_grants: *const [View; 3],
    pub secret_grants_len: usize,
}
impl OsConfig {
    pub(crate) fn with_default_operations(&self) -> Self {
        Self {
            directory_ops: &hirsel_core_os_directory_ops,
            process_ops: &hirsel_core_os_process_ops,
            socket_activation: &hirsel_core_os_socket_activation,
            ..*self
        }
    }
}
pub struct OsProcess {
    pub(crate) sources: *const crate::abi_sites::SourceTable,
    os: Option<Box<OsProvider>>,
    _provider: Box<Provider>,
    _stop: Option<Box<StopSignals>>,
    _args: Vec<Vec<u8>>,
    _carriers: Vec<Buffer>,
    _metadata: Box<[Grant]>,
    _net_rules: crate::net::RulesStorage,
    secrets: Vec<crate::secret::Granted>,
    grants: Vec<u64>,
    diagnostic: Vec<u8>,
    pub(crate) config: Config,
    _launch: Box<Metadata>,
    describe_out: Option<OwnedFd>,
}
#[repr(C)]
pub struct OpenProcess {
    pub process: *mut OsProcess,
    pub error: IoError,
}
pub(crate) fn duplicate(fd: i32) -> io::Result<OwnedFd> {
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
pub(crate) enum StartupError {
    Io(io::Error),
    MissingGrant(usize),
    MissingSecret(usize),
    NetRules,
    UnsupportedHostOpenat2(io::Error),
}
impl From<io::Error> for StartupError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}
impl StartupError {
    fn into_io(self) -> io::Error {
        match self {
            Self::Io(error) | Self::UnsupportedHostOpenat2(error) => error,
            Self::MissingGrant(_) | Self::MissingSecret(_) | Self::NetRules => {
                io::Error::from_raw_os_error(libc::EACCES)
            }
        }
    }
}
impl OsProcess {
    #[cfg_attr(hirsel_runtime_size, optimize(size))]
    /// Matching socket activation metadata is consumed during process startup,
    /// before other threads can access the environment.
    pub(crate) unsafe fn new(
        input: &OsConfig,
        probe: impl FnOnce(&OsProvider, u64) -> io::Result<()>,
        launch_metadata: fn(&str, u64, bool) -> Metadata,
    ) -> Result<Box<Self>, StartupError> {
        // SAFETY: copy launcher input before acquiring any program grant.
        unsafe {
            let argv = (0..input.args_len).map(|index| bytes(*input.args.add(index)));
            for value in argv.clone() {
                std::str::from_utf8(value)
                    .map_err(|_| io::Error::from_raw_os_error(libc::EILSEQ))?;
            }
            let mut n_place = hirsel_core_ids::launch::DEFAULT_N_PLACE;
            let mut describe_mode = false;
            let mut prefix = 0;
            for argument in argv.clone() {
                if argument == b"--hirsel-describe" {
                    describe_mode = true;
                } else if let Some(value) = argument.strip_prefix(b"--hirsel-n-place=") {
                    n_place = std::str::from_utf8(value)
                        .ok()
                        .and_then(|v| v.parse().ok())
                        .ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL))?;
                } else {
                    break;
                }
                prefix += 1;
            }
            let args: Vec<_> = argv.skip(prefix).map(<[u8]>::to_vec).collect();
            let prepare_path = if args.first().is_some_and(|arg| arg == b"--hirsel-prepare") {
                if describe_mode || args.len() != 4 || input.directories_len == 0 {
                    return Err(io::Error::from_raw_os_error(libc::EINVAL).into());
                }
                let dir = Path::new(OsStr::from_bytes(&args[1]));
                if !dir.is_absolute() {
                    return Err(io::Error::from_raw_os_error(libc::EINVAL).into());
                }
                Some(dir)
            } else {
                None
            };
            let target = std::str::from_utf8(bytes(input.target))
                .map_err(|_| io::Error::from_raw_os_error(libc::EILSEQ))?;
            let launch = Box::new(launch_metadata(target, n_place, describe_mode));
            // Receive before the first dup/open can reuse a missing activation fd.
            let inherited = if let Some(activation) = input.socket_activation.as_ref()
                && input.grant_mask & (1 << NET) != 0
            {
                (activation.receive)()?
            } else {
                None
            };
            let mut os = Box::new(OsProvider::new(
                duplicate(if !describe_mode && input.grant_mask & (1 << STDOUT) != 0 {
                    input.stdout_fd
                } else {
                    input.stderr_fd
                })?,
                duplicate(input.stderr_fd)?,
            ));
            if !describe_mode && prepare_path.is_none() {
                *os.inherited_listener.get_mut().unwrap() = inherited;
            }
            let describe_out = if describe_mode {
                Some(duplicate(input.stdout_fd)?)
            } else {
                None
            };
            let directory_ops = input.directory_ops.as_ref();
            let mut dirs = Vec::new();
            if let Some(scratch) = prepare_path {
                let operations =
                    directory_ops.ok_or_else(|| io::Error::from_raw_os_error(libc::EACCES))?;
                let handle = (operations.grant)(&mut os, scratch)?;
                dirs.resize(input.directories_len, handle);
            } else if !describe_mode && let Some(operations) = directory_ops {
                for index in 0..input.directories_len {
                    let path = Path::new(OsStr::from_bytes(bytes(*input.directories.add(index))));
                    dirs.push((operations.grant)(&mut os, path)?);
                }
            }
            let views = args
                .iter()
                .map(|a| Buffer {
                    data: a.as_ptr().cast_mut(),
                    len: a.len(),
                    capacity: 0,
                })
                .collect::<Vec<_>>();
            let net_storage = if describe_mode || prepare_path.is_some() || input.net_rules_len == 0
            {
                Vec::new()
            } else {
                std::slice::from_raw_parts(input.net_rules, input.net_rules_len)
                    .iter()
                    .map(|rule| rule.decode())
                    .collect::<Option<Vec<_>>>()
                    .ok_or(StartupError::NetRules)?
            };
            if !describe_mode
                && prepare_path.is_none()
                && input.grant_mask & (1 << NET) != 0
                && net_storage.is_empty()
            {
                return Err(StartupError::NetRules);
            }
            let net_rules = crate::net::RulesStorage::new(net_storage);
            let provider = Box::new(os.provider_with(directory_ops));
            let provider_pointer = &*provider as *const Provider;
            let requirements = if input.parameters_len == 0 {
                &[][..]
            } else {
                std::slice::from_raw_parts(input.parameters, input.parameters_len)
            };
            let stop = if let Some(operations) = input.process_ops.as_ref()
                && !describe_mode
                && input.grant_mask & (1 << PROCESS) != 0
                && requirements.iter().any(|req| req.kind == PROCESS)
            {
                Some(Box::new((operations.install)()?))
            } else {
                None
            };
            let mut metadata = Vec::new();
            let mut secrets = Vec::new();
            if !describe_mode {
                for i in 0..input.parameters_len {
                    let requirement = &*input.parameters.add(i);
                    if requirement.kind == SECRET {
                        let read = || {
                            if requirement.index >= input.secret_grants_len {
                                return None;
                            }
                            let [name, source, patterns] =
                                *input.secret_grants.add(requirement.index);
                            let text = |view| std::str::from_utf8(bytes(view)).ok();
                            let record = hirsel_core_net::SecretGrant::new(
                                text(name)?,
                                text(source)?,
                                text(patterns)?,
                            )?;
                            crate::secret::Granted::read(record, requirement.index as u64)
                        };
                        secrets.push(read().ok_or(StartupError::MissingSecret(i))?);
                    }
                }
                metadata.extend(secrets.iter().map(|secret| Grant {
                    kind: SECRET,
                    provider: provider_pointer,
                    handle: (secret as *const crate::secret::Granted) as u64,
                }));
            }
            if describe_mode {
                // Main receives inert designations. The launch boundary denies
                // every provider operation and spawn, so these grant no authority.
                for index in 0..input.parameters_len {
                    let kind = (*input.parameters.add(index)).kind;
                    if kind != ARGS {
                        metadata.push(Grant {
                            kind,
                            provider: provider_pointer,
                            handle: 0,
                        });
                    }
                }
            } else {
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
                            provider: if kind == PROCESS {
                                stop.as_ref().map_or(provider_pointer, |s| s.provider())
                            } else {
                                provider_pointer
                            },
                            handle: if kind == NET { net_rules.handle() } else { 0 },
                        });
                    }
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
                launch: &*launch,
                describe_output: Some(describe_output),
            };
            let mut process = Box::new(Self {
                sources: std::ptr::null(),
                os: Some(os),
                _provider: provider,
                _stop: stop,
                _args: args,
                _carriers: views,
                _metadata: metadata,
                _net_rules: net_rules,
                secrets,
                grants: Vec::with_capacity(input.parameters_len),
                diagnostic: vec![],
                config,
                _launch: launch,
                describe_out,
            });
            let mut probe = Some(probe);
            for n in 0..input.parameters_len {
                let requirement = &*input.parameters.add(n);
                let handle =
                    hirsel_core_entry_grant(&process.config, requirement.kind, requirement.index);
                if handle == 0 {
                    return Err(StartupError::MissingGrant(n));
                }
                if requirement.kind == DIR && !describe_mode {
                    if let Some(probe) = probe.take() {
                        let designation = process._metadata[(handle - 1) as usize].handle;
                        probe(process.os.as_ref().unwrap(), designation)
                            .map_err(StartupError::UnsupportedHostOpenat2)?;
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
/// For a matching socket activation, call at process startup before other
/// threads can access the environment. The launcher transfers ownership of fd 3.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_os_process_new(config: *const OsConfig) -> OpenProcess {
    // SAFETY: constructor copies/pins all configuration before returning its owner.
    match unsafe {
        OsProcess::new(
            &(*config).with_default_operations(),
            OsProvider::probe_directory,
            Metadata::native,
        )
    } {
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
#[cfg_attr(hirsel_runtime_size, optimize(size))]
pub unsafe extern "C" fn hirsel_core_os_process_free(process: *mut OsProcess) {
    // SAFETY: transfers ownership of the process object exactly once.
    unsafe {
        drop(Box::from_raw(process));
    }
}
#[cfg_attr(hirsel_runtime_size, optimize(size))]
unsafe extern "C" fn close_grants(state: *mut u8) {
    // SAFETY: main and all descendants have ended. Descriptor drop allocates and
    // admits nothing, ignores OS close errors, and cannot add a language cause.
    unsafe {
        let process = &mut *state.cast::<OsProcess>();
        process.secrets.clear();
        drop(process.os.take());
        drop(process.describe_out.take());
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
        process.diagnostic = crate::abi_sites::format_cause(&*cause, process.sources).into_bytes();
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
/// Matching socket activation has the same startup and fd ownership contract
/// as `hirsel_core_os_process_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_os_run(
    input: *const OsConfig,
    parameter_names: *const View,
    entry: crate::process::Entry,
) -> u8 {
    // SAFETY: constructor copies all configuration; entry and names are pinned by caller.
    unsafe { run_with_probe(&*input, parameter_names, entry, OsProvider::probe_directory) }
}
/// Named OS entry with a borrowed current-source table (FORM #91).
/// # Safety
/// Input/name/entry contracts are os_run's; sources follows
/// abi_sites::hirsel_core_os_process_set_sources through reporting. The native
/// linker supplies the operations tables for directory, Process and Net grants.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_os_run_with_sources(
    input: *const OsConfig,
    parameter_names: *const View,
    sources: *const crate::abi_sites::SourceTable,
    entry: crate::process::Entry,
) -> u8 {
    // SAFETY: all configuration and source storage is pinned by the caller.
    unsafe { run_os_with_sources(input, parameter_names, sources, entry, native_metadata) }
}

pub(crate) fn native_metadata(target: &str, n_place: u64, describe: bool) -> Metadata {
    if let Some(memory) = crate::units::memory() {
        memory.native_metadata(target, n_place, describe)
    } else {
        Metadata::native(target, n_place, describe)
    }
}

/// OS entry for a closed executable without any launch-digest reader.
/// # Safety
/// The caller meets os_run_with_sources' contract and the executable cannot
/// observe the executable digest, including through linked code or a Run tape.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_os_run_without_digest(
    input: *const OsConfig,
    parameter_names: *const View,
    sources: *const crate::abi_sites::SourceTable,
    entry: crate::process::Entry,
) -> u8 {
    // SAFETY: all configuration and source storage is pinned by the caller.
    unsafe { run_os_with_sources(input, parameter_names, sources, entry, Metadata::source) }
}

unsafe fn run_os_with_sources(
    input: *const OsConfig,
    parameter_names: *const View,
    sources: *const crate::abi_sites::SourceTable,
    entry: crate::process::Entry,
    launch_metadata: fn(&str, u64, bool) -> Metadata,
) -> u8 {
    if let Some(k) = crate::units::count() {
        struct Shared(*const View, *const crate::abi_sites::SourceTable);
        // SAFETY: the caller pins the tables for the whole launch.
        unsafe impl Sync for Shared {}
        let shared = Shared(parameter_names, sources);
        // SAFETY: each unit borrows the pinned tables until its run returns.
        return crate::units::launch(k, unsafe { &*input }, |config| unsafe {
            let shared = &shared;
            let operations = config.directory_ops.as_ref();
            run_with_sources_and_probe(
                config,
                shared.0,
                shared.1,
                entry,
                |os, handle| (operations.unwrap().probe)(os, handle),
                launch_metadata,
            )
        });
    }
    // SAFETY: all configuration and source storage is pinned by the caller.
    unsafe {
        let operations = (*input).directory_ops.as_ref();
        run_with_sources_and_probe(
            &*input,
            parameter_names,
            sources,
            entry,
            |os, handle| (operations.unwrap().probe)(os, handle),
            launch_metadata,
        )
    }
}

unsafe fn run_with_probe(
    input: &OsConfig,
    names: *const View,
    entry: crate::process::Entry,
    probe: impl FnOnce(&OsProvider, u64) -> io::Result<()>,
) -> u8 {
    // SAFETY: original entry supplies no source table.
    unsafe {
        run_with_sources_and_probe(
            &input.with_default_operations(),
            names,
            std::ptr::null(),
            entry,
            probe,
            Metadata::native,
        )
    }
}
#[cfg_attr(hirsel_runtime_size, optimize(size))]
unsafe fn run_with_sources_and_probe(
    input: &OsConfig,
    names: *const View,
    sources: *const crate::abi_sites::SourceTable,
    entry: crate::process::Entry,
    probe: impl FnOnce(&OsProvider, u64) -> io::Result<()>,
    launch_metadata: fn(&str, u64, bool) -> Metadata,
) -> u8 {
    // SAFETY: input tables are borrowed from the OS entry's configuration.
    match unsafe { OsProcess::new(input, probe, launch_metadata) } {
        Ok(mut process) => {
            // SAFETY: table is borrowed until this call completes reporting.
            unsafe { crate::abi_sites::hirsel_core_os_process_set_sources(&mut *process, sources) };
            // SAFETY: the Box pins Config and all grants through run and cleanup.
            unsafe { crate::process::hirsel_core_run(&process.config, entry) }
        }
        Err(error) => {
            unsafe { report_startup(error, input.stderr_fd, names) };
            crate::process::NOT_STARTED
        }
    }
}

pub(crate) unsafe fn report_startup(error: StartupError, stderr: i32, names: *const View) {
    let mut line = b"not started: ".to_vec();
    match error {
        StartupError::NetRules => {
            line.extend_from_slice(b"Net requires --grant ");
            line.extend_from_slice(hirsel_core_net::RULE_SYNTAX.as_bytes());
            line.push(b'\n');
        }
        StartupError::MissingGrant(index) => {
            line.extend_from_slice(b"grant `");
            // SAFETY: emitter supplies one valid UTF-8 name per main parameter.
            line.extend_from_slice(unsafe { bytes(*names.add(index)) });
            line.extend_from_slice(b"` is not in the run's configuration\n");
        }
        StartupError::MissingSecret(index) => {
            line.extend_from_slice(b"grant `");
            line.extend_from_slice(unsafe { bytes(*names.add(index)) });
            line.extend_from_slice(b"` [secret] has no readable nonempty source\n");
        }
        StartupError::UnsupportedHostOpenat2(_) => {
            line.extend_from_slice(
                b"Dir requires openat2 (Linux 5.6 or later, permitted by the sandbox)\n",
            );
        }
        StartupError::Io(error) => {
            line.extend_from_slice(
                format!("host grants could not be configured: {error}\n").as_bytes(),
            );
        }
    }
    // SAFETY: the configured diagnostic fd and line live through this write.
    unsafe { crate::os::diagnostic(stderr, line.as_ptr(), line.len()) };
}

#[cfg(test)]
#[path = "startup_host_tests.rs"]
mod startup_host_tests;

#[cfg(test)]
#[path = "entry_layout_tests.rs"]
mod layout_tests;

unsafe extern "C" fn describe_output(state: *mut u8, data: *const u8, len: usize) -> bool {
    // SAFETY: the run pins its response view and the launch owner pins its sink.
    unsafe {
        let process = &*state.cast::<OsProcess>();
        let Some(out) = &process.describe_out else {
            return false;
        };
        crate::os::write_all(out.as_raw_fd(), bytes(View { data, len })).error == IoError::OK
    }
}
