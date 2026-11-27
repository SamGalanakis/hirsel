//! FORM decision 44: capabilities and Args are plain u64 provider handles.
//! Config.grants is in main parameter order; the run closes its grants after
//! main's cleanup. Grant closure cannot allocate, admit or fail.
#[path = "entry_launch.rs"]
mod launch_options;
#[path = "entry_startup.rs"]
mod startup;
pub(crate) use startup::{Missing, StartupError, report_startup, take_inherited};

#[path = "entry_describe.rs"]
mod describe;
use describe::describe_output;

use crate::{
    Context,
    os::{DirectoryOperations, OsProvider, hirsel_core_os_directory_ops},
    os_net::hirsel_core_os_socket_activation,
    process::{Config, View},
    provider::{Buffer, IoError, Provider},
    stop_signals::{StopSignals, hirsel_core_os_process_ops},
    units::{HostCall, off_slot},
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
    sync::Mutex,
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
pub const FOREIGN: u8 = 12;
pub const EXEC: u8 = 13;
pub const CRED: u8 = 14;
pub const COMPILER: u8 = 15;
/// Host metadata. Its index + 1 is the numeric provider handle. Source code
/// cannot construct or inspect this metadata; copying a handle copies designation.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Grant {
    pub kind: u8,
    pub provider: *const Provider,
    pub handle: u64,
    pub terminal: bool,
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
/// Query the immutable terminal bit on a granted or virtual stream.
/// # Safety
/// Context is running and handle designates a stream grant in this run.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_stream_is_terminal(
    context: *mut Context,
    handle: u64,
    _kind: u8,
    _site: u64,
) -> crate::provider::Outcome {
    // SAFETY: operands designate a valid entry or run-owned virtual grant.
    let terminal = unsafe {
        crate::process::deny_describe_capability(context);
        if handle & crate::host_file::DYNAMIC_GRANT != 0 {
            (*(*context).host_files).registry.grant_terminal(handle)
                != crate::host_file::Terminal::No
        } else {
            (*(*(*context).run).capabilities.add((handle - 1) as usize)).terminal
        }
    };
    crate::provider::Outcome {
        value: u64::from(terminal),
        ..Default::default()
    }
}

/// Operations required by a live Process grant. The generated launcher names
/// this table only when main can receive Process.
pub struct ProcessOperations {
    pub(crate) install: fn() -> io::Result<StopSignals>,
}

/// Starts an OS Net grant and receives a launcher-owned listener.
pub struct SocketActivation {
    pub(crate) receive: unsafe fn() -> io::Result<Option<OwnedFd>>,
    pub(crate) rules: unsafe fn(&[hirsel_core_net::RuleView]) -> Option<crate::net::RulesStorage>,
    pub(crate) unix_probe: fn() -> io::Result<()>,
    pub(crate) unix_roots:
        unsafe fn(&mut OsProvider, &[hirsel_core_net::RuleView]) -> Result<(), StartupError>,
    pub(crate) operator: unsafe fn() -> io::Result<Option<(OwnedFd, u32)>>,
}

/// Reads the credential source for a Secret grant during startup.
pub struct SecretOperations {
    pub(crate) read: fn([&[u8]; 3], u64) -> Option<crate::secret::Granted>,
}

/// Validates and snapshots the names covered by a live Env grant.
pub struct EnvOperations {
    load: unsafe fn(&OsConfig, bool) -> Result<Option<Box<crate::env::Env>>, StartupError>,
}

#[unsafe(no_mangle)]
pub static hirsel_core_os_env_ops: EnvOperations = EnvOperations {
    load: launch_options::load_env,
};

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
    /// Installed by the native linker for a launch with Secret grants.
    pub secret_ops: *const SecretOperations,
    pub env_patterns: View,
    /// Installed by the native linker for a launch with an Env grant.
    pub env_ops: *const EnvOperations,
    pub exec_bindings: *const crate::exec::Binding,
    pub exec_bindings_len: usize,
    pub exec_ops: *const crate::exec::Operations,
    /// One `[parameter, source, policy]` record per Cred parameter (TLS-7).
    pub cred_grants: *const [View; 3],
    pub cred_grants_len: usize,
    /// The entry revision, or null for an embedder without one.
    pub revision: *const [u8; 32],
    /// Nonzero when the linked program reaches the `runner` package, the one
    /// place reading the place-only launch words (launch.md, private prefix).
    pub place: u64,
}
impl OsConfig {
    pub(crate) fn with_default_operations(&self) -> Self {
        Self {
            directory_ops: &hirsel_core_os_directory_ops,
            process_ops: &hirsel_core_os_process_ops,
            socket_activation: &hirsel_core_os_socket_activation,
            secret_ops: &crate::secret::hirsel_core_os_secret_ops,
            env_ops: &hirsel_core_os_env_ops,
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
    _net_rules: Option<crate::net::RulesStorage>,
    secrets: Vec<crate::secret::Granted>,
    _creds: Vec<crate::cred::Granted>,
    _env: Option<Box<crate::env::Env>>,
    _compiler: Option<Box<crate::host::compiler::Binding>>,
    _exec: Option<crate::exec::Launch>,
    stdin: Option<Box<crate::stdin::Stream>>,
    grants: Vec<u64>,
    diagnostic: Vec<u8>,
    pub(crate) config: Config,
    _launch: Box<Metadata>,
    describe_out: Option<OwnedFd>,
    host: Option<HostLaunch>,
}

pub(crate) struct HostLaunch {
    pub(crate) control: Mutex<Option<OwnedFd>>,
    pub(crate) scratch: u64,
}

/// The callback and its state are installed together by the OS launcher.
pub(crate) unsafe fn launch_host<'a>(run: *const Config) -> Option<&'a HostLaunch> {
    let run = unsafe { run.as_ref()? };
    let writer = run.describe_output?;
    if !std::ptr::fn_addr_eq(
        writer,
        describe_output as unsafe extern "C" fn(*mut u8, *const u8, usize) -> bool,
    ) {
        return None;
    }
    unsafe { run.state.cast::<OsProcess>().as_ref()?.host.as_ref() }
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
/// A missing inherited standard stream is a closed stream. An invalid
/// descriptor supplied in its place is a startup error.
pub(crate) fn duplicate_stream(fd: i32, inherited_fd: i32) -> io::Result<Option<OwnedFd>> {
    match duplicate(fd) {
        Ok(owned) => Ok(Some(owned)),
        Err(error) if fd == inherited_fd && error.raw_os_error() == Some(libc::EBADF) => Ok(None),
        Err(error) => Err(error),
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
pub(crate) use launch_options::prepare_hex;

impl OsProcess {
    #[cfg(feature = "record")]
    pub(crate) fn host_launch(&self) -> Option<&HostLaunch> {
        self.host.as_ref()
    }
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
            startup::prepare_host()?;
            let argv = (0..input.args_len).map(|index| bytes(*input.args.add(index)));
            // `input.args` starts after the program name, process argv's 0.
            for (index, value) in argv.clone().enumerate() {
                std::str::from_utf8(value).map_err(|_| StartupError::NonUtf8Arg(index + 1))?;
            }
            let options = launch_options::parse(argv.clone(), input)?;
            let args: Vec<_> = argv.skip(options.prefix).map(<[u8]>::to_vec).collect();
            let place = input.place != 0;
            let prepare_path = options.prepare_path(&args, input.directories_len, place)?;
            let store_op = options.store_op_path(&args, input.directories_len, place)?;
            let isolated_store = prepare_path.or(store_op.as_ref().map(|op| op.directory));
            let dry_run = store_op.is_some();
            let program_args = if dry_run { &args[2..] } else { &args[..] };
            let installing = options.installing();
            let launch_options::LaunchOptions {
                compiler_worker,
                n_place,
                n_place_explicit,
                describe_mode,
                preview_mode,
                wake,
                install_child,
                move_envelope,
                dev_store,
                install_predecessor,
                idle_ms,
                store_id,
                witness,
                exec_approvals,
                ..
            } = options;
            let inherited_store = if dev_store {
                // Dev pins its directory before taking the per-inode supervisor lock.
                const DEV_STORE_FD: i32 = 5;
                if libc::fcntl(DEV_STORE_FD, libc::F_SETFD, libc::FD_CLOEXEC) < 0 {
                    return Err(io::Error::last_os_error().into());
                }
                Some(OwnedFd::from_raw_fd(DEV_STORE_FD))
            } else {
                None
            };
            let digest = crate::host::inherited::receive_launch_image()?;
            // Fd 3 is the inherited listener and fd 4 is the control socket.
            // Take the launcher's dedicated report descriptor before opening grants.
            let wake_report = if wake {
                const REPORT_FD: i32 = 5;
                if libc::fcntl(REPORT_FD, libc::F_SETFD, libc::FD_CLOEXEC) < 0 {
                    return Err(io::Error::last_os_error().into());
                }
                Some(OwnedFd::from_raw_fd(REPORT_FD))
            } else {
                None
            };
            let env = if input.grant_mask & (1 << ENV) != 0 {
                let operations = input.env_ops.as_ref().ok_or(StartupError::EnvPatterns)?;
                (operations.load)(input, describe_mode)?
            } else {
                None
            };
            let target = std::str::from_utf8(bytes(input.target))
                .map_err(|_| io::Error::from_raw_os_error(libc::EILSEQ))?;
            let mut launch = match digest {
                Some(digest) => Metadata::embedded(Some(digest), target, n_place, describe_mode),
                None => launch_metadata(target, n_place, describe_mode),
            }
            .with_launch_identity(input.revision.as_ref().copied(), n_place_explicit)
            .with_store_id(store_id)
            .with_move_envelope(move_envelope);
            if wake {
                launch = launch
                    .with_wake(idle_ms.unwrap_or_default())
                    .with_witness(witness);
            }
            if prepare_path.is_some() {
                launch = launch_options::with_prepare(launch, &args)?;
            }
            if let Some(op) = store_op {
                launch = op.configure(launch);
            }
            if let Some(predecessor) = install_predecessor {
                launch = launch.with_install_predecessor(predecessor);
            }
            if let Some(activation) = install_child {
                launch = launch.with_install_child(activation);
            }
            if preview_mode {
                use std::io::Read;
                let input = std::fs::File::from(duplicate(libc::STDIN_FILENO)?);
                let mut old = Vec::new();
                let limit = n_place.saturating_add(1);
                off_slot(HostCall::PeerWait, || {
                    input.take(limit).read_to_end(&mut old)
                })?;
                if old.len() as u64 > n_place {
                    return Err(io::Error::from_raw_os_error(libc::EFBIG).into());
                }
                launch = launch.with_preview(old);
            }
            let launch = Box::new(launch);
            // Receive before the first dup/open can reuse a missing activation fd.
            let net_ops = input.socket_activation.as_ref();
            let (inherited, operator) = if let Some(activation) = net_ops
                && !installing
                && !dry_run
                && input.grant_mask & (1 << NET) != 0
            {
                ((activation.receive)()?, (activation.operator)()?)
            } else {
                (None, None)
            };
            let host_control = launch_options::receive_host_control()?;
            if host_control.is_some() && (describe_mode || isolated_store.is_some() || installing) {
                return Err(io::Error::from_raw_os_error(libc::EINVAL).into());
            }
            let rows = if input.net_rules_len == 0 {
                &[][..]
            } else {
                std::slice::from_raw_parts(input.net_rules, input.net_rules_len)
            };
            let unix = !describe_mode
                && !installing
                && isolated_store.is_none()
                && input.grant_mask & (1 << NET) != 0
                && rows.iter().any(|row| row.is_unix());
            let exec = if input.exec_bindings_len != 0 {
                let ops = input
                    .exec_ops
                    .as_ref()
                    .ok_or_else(|| StartupError::Exec("no Exec startup operations".into()))?;
                Some((ops.load)(input, &exec_approvals, describe_mode, unix)?)
            } else {
                if unix {
                    let probe = net_ops.ok_or(StartupError::NetRules)?.unix_probe;
                    probe().map_err(StartupError::UnixProbe)?;
                }
                None
            };
            let terminal = [
                (libc::STDIN_FILENO, STDIN),
                (input.stdout_fd, STDOUT),
                (input.stderr_fd, STDERR),
            ]
            .map(|(fd, kind)| {
                !describe_mode && input.grant_mask & (1 << kind) != 0 && libc::isatty(fd) == 1
            });
            let stdin = (!describe_mode && input.grant_mask & (1 << STDIN) != 0)
                .then(|| Box::new(crate::stdin::Stream::new(duplicate(libc::STDIN_FILENO))));
            let stdout = if !describe_mode
                && !installing
                && !dry_run
                && input.grant_mask & (1 << STDOUT) != 0
            {
                duplicate_stream(input.stdout_fd, libc::STDOUT_FILENO)?
            } else {
                duplicate_stream(input.stderr_fd, libc::STDERR_FILENO)?
            };
            let stderr = duplicate_stream(input.stderr_fd, libc::STDERR_FILENO)?;
            let mut os = Box::new(OsProvider::from_streams(stdout, stderr));
            if !describe_mode && isolated_store.is_none() {
                *os.inherited_listener.get_mut().unwrap() = inherited;
                *os.inherited_unix.get_mut().unwrap() = operator;
            }
            let describe_out = if describe_mode || installing || dry_run {
                Some(duplicate(input.stdout_fd)?)
            } else {
                wake_report
            };
            let directory_ops = input.directory_ops.as_ref();
            let mut dirs = Vec::new();
            let mut resolved = Vec::new();
            if let Some(scratch) = isolated_store {
                let operations =
                    directory_ops.ok_or_else(|| io::Error::from_raw_os_error(libc::EACCES))?;
                let handle = (operations.grant)(&mut os, scratch)?;
                dirs.resize(input.directories_len, handle);
            } else if !describe_mode && let Some(operations) = directory_ops {
                for index in 0..input.directories_len {
                    let slot = if index == 0 && inherited_store.is_some() {
                        crate::host::sys::INHERITED_STORE
                    } else {
                        bytes(*input.directories.add(index))
                    };
                    dirs.push(
                        match startup::grant_directory(&mut os, operations, slot, index)? {
                            startup::DirectoryGrant::Open(handle) => handle,
                            startup::DirectoryGrant::Resolved(path) => {
                                resolved.push((index, path));
                                0
                            }
                        },
                    );
                }
            }
            drop(inherited_store);
            let host_scratch = if host_control.is_some() {
                let operations =
                    directory_ops.ok_or_else(|| io::Error::from_raw_os_error(libc::EACCES))?;
                let path = std::env::var_os("HIRSEL_SCRATCH")
                    .ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL))?;
                let path = Path::new(&path);
                if !path.is_absolute() {
                    return Err(io::Error::from_raw_os_error(libc::EINVAL).into());
                }
                Some((operations.grant)(&mut os, path)?)
            } else {
                None
            };
            let views = program_args
                .iter()
                .map(|a| Buffer {
                    data: a.as_ptr().cast_mut(),
                    len: a.len(),
                    capacity: 0,
                })
                .collect::<Vec<_>>();
            let net_granted = input.grant_mask & (1 << NET) != 0;
            let net_rules = if describe_mode {
                None
            } else if isolated_store.is_some() || (installing && install_predecessor.is_none()) {
                net_granted.then(crate::net::RulesStorage::empty)
            } else if input.net_rules_len != 0 {
                let ops = net_ops.ok_or(StartupError::NetRules)?;
                let rules = (ops.rules)(rows).ok_or(StartupError::NetRules)?;
                (ops.unix_roots)(&mut os, rows)?;
                Some(rules)
            } else if net_granted {
                return Err(StartupError::NetRules);
            } else {
                None
            };
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
            let compiler = compiler_worker
                .filter(|_| !describe_mode)
                .map(|path| crate::host::compiler::Binding::new(path.to_owned()));
            let mut metadata = Vec::new();
            let mut secrets = Vec::new();
            let mut creds = Vec::new();
            let mut missing = Vec::new();
            if !describe_mode {
                for i in 0..input.parameters_len {
                    let requirement = &*input.parameters.add(i);
                    if requirement.kind == SECRET {
                        let read = || {
                            let operations = input.secret_ops.as_ref()?;
                            if requirement.index >= input.secret_grants_len {
                                return None;
                            }
                            let views = *input.secret_grants.add(requirement.index);
                            (operations.read)(
                                views.map(|view| bytes(view)),
                                requirement.index as u64,
                            )
                        };
                        match read() {
                            Some(secret) => secrets.push(secret),
                            None => missing.push(Missing::Secret(i)),
                        }
                    } else if requirement.kind == CRED {
                        // The launcher checked each record; nothing is read
                        // from a credential source before a fetch.
                        let read = || {
                            if requirement.index >= input.cred_grants_len {
                                return None;
                            }
                            let views = *input.cred_grants.add(requirement.index);
                            crate::cred::Granted::read(views.map(|view| bytes(view)))
                        };
                        match read() {
                            Some(cred) => creds.push(cred),
                            None => missing.push(Missing::Grant(i)),
                        }
                    }
                }
                metadata.extend(secrets.iter().map(|secret| Grant {
                    kind: SECRET,
                    provider: provider_pointer,
                    handle: (secret as *const crate::secret::Granted) as u64,
                    terminal: false,
                }));
                metadata.extend(creds.iter().map(|cred| Grant {
                    kind: CRED,
                    provider: provider_pointer,
                    handle: (cred as *const crate::cred::Granted) as u64,
                    terminal: false,
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
                            terminal: false,
                        });
                    }
                }
            } else {
                for handle in dirs {
                    metadata.push(Grant {
                        kind: DIR,
                        provider: provider_pointer,
                        handle,
                        terminal: false,
                    });
                }
                for kind in (NET..=TASKS).chain([COMPILER]) {
                    if input.grant_mask & (1 << kind) != 0 {
                        metadata.push(Grant {
                            kind,
                            provider: if kind == PROCESS {
                                stop.as_ref().map_or(provider_pointer, |s| s.provider())
                            } else {
                                provider_pointer
                            },
                            handle: match kind {
                                COMPILER => compiler.as_ref().map_or(0, |c| c.handle()),
                                NET => net_rules.as_ref().unwrap().handle(),
                                STDIN => std::ptr::from_ref(stdin.as_deref().unwrap()) as u64,
                                ENV => env.as_ref().unwrap().handle(),
                                _ => 0,
                            },
                            terminal: match kind {
                                STDIN => terminal[0],
                                STDOUT => terminal[1],
                                STDERR => terminal[2],
                                _ => false,
                            },
                        });
                    }
                }
                if let Some(exec) = &exec {
                    metadata.extend(exec.handles().map(|handle| Grant {
                        kind: EXEC,
                        provider: provider_pointer,
                        handle,
                        terminal: false,
                    }));
                }
                if input.grant_mask & (1 << FOREIGN) != 0 {
                    metadata.extend(requirements.iter().filter(|r| r.kind == FOREIGN).map(|r| {
                        Grant {
                            kind: FOREIGN,
                            provider: std::ptr::null(),
                            handle: r.index as u64,
                            terminal: false,
                        }
                    }));
                }
            }
            metadata.push(Grant {
                kind: ARGS,
                provider: provider_pointer,
                handle: 0,
                terminal: false,
            });
            let host = host_control.zip(host_scratch).map(|(control, handle)| {
                metadata.push(Grant {
                    kind: DIR,
                    provider: provider_pointer,
                    handle,
                    terminal: false,
                });
                HostLaunch {
                    control: Mutex::new(Some(control)),
                    scratch: metadata.len() as u64,
                }
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
                stop_fd: os.stderr_fd(),
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
                _creds: creds,
                _env: env,
                _compiler: compiler,
                _exec: exec,
                stdin,
                grants: Vec::with_capacity(input.parameters_len),
                diagnostic: vec![],
                config,
                _launch: launch,
                describe_out,
                host,
            });
            for n in 0..input.parameters_len {
                let requirement = &*input.parameters.add(n);
                // The read above decided each Secret and Cred: once one is
                // missing, a later one's ordinal no longer finds its row.
                let read = !describe_mode && matches!(requirement.kind, SECRET | CRED);
                match hirsel_core_entry_grant(&process.config, requirement.kind, requirement.index)
                {
                    0 if !read => missing.push(Missing::Grant(n)),
                    0 => {}
                    handle => process.grants.push(handle),
                }
            }
            if !missing.is_empty() {
                missing.sort_unstable_by_key(|m| match *m {
                    Missing::Grant(n) | Missing::Secret(n) => n,
                });
                return Err(StartupError::Missing(missing));
            }
            // The host check for Dir (openat2) runs on the first Dir parameter's
            // root; when that is a launcher-resolved directory, it runs once the
            // directory is opened, as nothing else can show it.
            let probe_dir = (!describe_mode)
                .then(|| requirements.iter().position(|r| r.kind == DIR))
                .flatten()
                .map(|n| (process.grants[n] - 1) as usize);
            let mut probe = Some(probe);
            if let Some(at) = probe_dir
                && process._metadata[at].handle != 0
            {
                (probe.take().unwrap())(process.os.as_ref().unwrap(), process._metadata[at].handle)
                    .map_err(StartupError::UnsupportedHostOpenat2)?;
            }
            // Every other grant is given: create and open the launcher-resolved
            // directories, so a run that does not start leaves none behind.
            for (index, path) in resolved {
                let operations =
                    directory_ops.ok_or_else(|| io::Error::from_raw_os_error(libc::EACCES))?;
                let os = process.os.as_mut().unwrap();
                let handle = startup::create_resolved(os, operations, &path)?;
                let at = process
                    ._metadata
                    .iter()
                    .enumerate()
                    .filter(|(_, grant)| grant.kind == DIR)
                    .nth(index)
                    .unwrap()
                    .0;
                process._metadata[at].handle = handle;
            }
            if let (Some(at), Some(probe)) = (probe_dir, probe) {
                probe(process.os.as_ref().unwrap(), process._metadata[at].handle)
                    .map_err(StartupError::UnsupportedHostOpenat2)?;
            }
            process.config.grants = process.grants.as_ptr();
            process.config.grants_len = process.grants.len();
            process.config.state = (&mut *process as *mut Self).cast();
            if let Some(exec) = &process._exec {
                for line in &exec.lines {
                    let (fd, line) = (input.stderr_fd, line.as_bytes());
                    let outcome = off_slot(HostCall::System, || crate::os::write_all(fd, line));
                    if outcome.error != IoError::OK {
                        return Err(StartupError::Exec(
                            "could not write Exec approval record".into(),
                        ));
                    }
                }
            }
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
            crate::host::native_metadata,
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
        drop(process.stdin.take());
        drop(process._exec.take());
        drop(process.os.take());
        drop(process.describe_out.take());
    }
}
unsafe extern "C" fn diagnostic(state: *mut u8, data: *const u8, len: usize) {
    // SAFETY: run owns its sink until diagnostics finish, before grant closure.
    unsafe {
        let process = &*state.cast::<OsProcess>();
        crate::os::diagnostic(process.os.as_ref().unwrap().stderr_fd(), data, len);
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
/// linker supplies the operations tables for directory, Process, Net, Secret
/// and Env grants.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_os_run_with_sources(
    input: *const OsConfig,
    parameter_names: *const View,
    sources: *const crate::abi_sites::SourceTable,
    entry: crate::process::Entry,
) -> u8 {
    // SAFETY: all configuration and source storage is pinned by the caller.
    unsafe {
        run_os_with_sources::<false>(
            input,
            parameter_names,
            sources,
            entry,
            native_metadata,
            |config, entry| crate::process::launch(config, entry),
        )
    }
}

/// The sourced OS entry for a program that can spawn a helper unit.
///
/// # Safety
/// As hirsel_core_os_run_with_sources. If omit_digest is true, the executable
/// must meet hirsel_core_os_run_without_digest's contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_os_run_with_units(
    input: *const OsConfig,
    parameter_names: *const View,
    sources: *const crate::abi_sites::SourceTable,
    entry: crate::process::Entry,
    omit_digest: bool,
) -> u8 {
    let metadata = if omit_digest {
        Metadata::source
    } else {
        native_metadata
    };
    // SAFETY: the caller pins configuration and sources for the OS entry.
    unsafe {
        run_os_with_sources::<true>(
            input,
            parameter_names,
            sources,
            entry,
            metadata,
            |config, entry| crate::process::launch(config, entry),
        )
    }
}

/// The sourced OS entry for a foreign-reaching program.
///
/// # Safety
/// As hirsel_core_os_run_with_sources. The checked reach determines whether
/// any of its foreign calls, releases included, require helpers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_os_run_foreign_with_sources(
    input: *const OsConfig,
    parameter_names: *const View,
    sources: *const crate::abi_sites::SourceTable,
    entry: crate::process::Entry,
    waits: bool,
) -> u8 {
    // SAFETY: the caller pins configuration and sources for the OS entry.
    unsafe {
        run_os_with_sources::<false>(
            input,
            parameter_names,
            sources,
            entry,
            native_metadata,
            |config, entry| crate::process::hirsel_core_run_with_foreign(config, entry, waits),
        )
    }
}

/// The sourced OS entry for a program that reaches foreign code and can spawn
/// a helper unit.
///
/// # Safety
/// As hirsel_core_os_run_foreign_with_sources.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_os_run_foreign_with_units(
    input: *const OsConfig,
    parameter_names: *const View,
    sources: *const crate::abi_sites::SourceTable,
    entry: crate::process::Entry,
    waits: bool,
) -> u8 {
    // SAFETY: the caller pins configuration and sources for the OS entry.
    unsafe {
        run_os_with_sources::<true>(
            input,
            parameter_names,
            sources,
            entry,
            native_metadata,
            |config, entry| crate::process::hirsel_core_run_with_foreign(config, entry, waits),
        )
    }
}

pub(crate) fn native_metadata(target: &str, n_place: u64, describe: bool) -> Metadata {
    if let Some(memory) = crate::units::memory() {
        memory.native_metadata(target, n_place, describe)
    } else {
        crate::host::native_metadata(target, n_place, describe)
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
    unsafe {
        run_os_with_sources::<false>(
            input,
            parameter_names,
            sources,
            entry,
            Metadata::source,
            |config, entry| crate::process::launch(config, entry),
        )
    }
}

// The entry selects the runner at compile time. Each launcher links through
// to the process lifecycle it needs.
unsafe fn run_os_with_sources<const UNITS: bool>(
    input: *const OsConfig,
    parameter_names: *const View,
    sources: *const crate::abi_sites::SourceTable,
    entry: crate::process::Entry,
    launch_metadata: fn(&str, u64, bool) -> Metadata,
    run: impl Fn(&Config, crate::process::Entry) -> u8 + Sync,
) -> u8 {
    #[cfg(feature = "prof")]
    let _profile = crate::prof::Run::start();
    if UNITS && let Some(w) = crate::units::workers() {
        struct Shared(*const View, *const crate::abi_sites::SourceTable);
        // SAFETY: the caller pins the tables for the whole launch.
        unsafe impl Sync for Shared {}
        let shared = Shared(parameter_names, sources);
        // SAFETY: the root unit borrows the pinned tables until its run returns.
        return crate::units::launch(1, w, unsafe { &*input }, &|config| unsafe {
            let shared = &shared;
            run_sourced(config, shared.0, shared.1, entry, launch_metadata, &run)
        });
    }
    // SAFETY: all configuration and source storage is pinned by the caller.
    unsafe {
        run_sourced(
            &*input,
            parameter_names,
            sources,
            entry,
            launch_metadata,
            &run,
        )
    }
}

// Both root launch modes share one instantiation of OS process setup.
#[inline(never)]
unsafe fn run_sourced(
    input: &OsConfig,
    parameter_names: *const View,
    sources: *const crate::abi_sites::SourceTable,
    entry: crate::process::Entry,
    launch_metadata: fn(&str, u64, bool) -> Metadata,
    run: impl FnOnce(&Config, crate::process::Entry) -> u8,
) -> u8 {
    // SAFETY: the launch pins the operation, name and source tables.
    unsafe {
        let operations = input.directory_ops.as_ref();
        run_with_sources_and_probe(
            input,
            parameter_names,
            sources,
            entry,
            |os, handle| (operations.unwrap().probe)(os, handle),
            launch_metadata,
            run,
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
            crate::host::native_metadata,
            |config, entry| crate::process::launch(config, entry),
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
    run: impl FnOnce(&Config, crate::process::Entry) -> u8,
) -> u8 {
    // SAFETY: input tables are borrowed from the OS entry's configuration.
    match unsafe { OsProcess::new(input, probe, launch_metadata) } {
        Ok(mut process) => {
            // SAFETY: table is borrowed until this call completes reporting.
            unsafe { crate::abi_sites::hirsel_core_os_process_set_sources(&mut *process, sources) };
            // The Box pins Config and all grants through run and cleanup.
            run(&process.config, entry)
        }
        Err(error) => {
            unsafe { report_startup(error, input.stderr_fd, names) };
            crate::process::NOT_STARTED
        }
    }
}

#[cfg(test)]
#[path = "startup_host_tests.rs"]
mod startup_host_tests;

#[cfg(test)]
#[path = "entry_layout_tests.rs"]
mod layout_tests;
