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
pub static hirsel_core_os_env_ops: EnvOperations = EnvOperations { load: load_env };

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
    _env: Option<Box<crate::env::Env>>,
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

fn receive_host_control() -> io::Result<Option<OwnedFd>> {
    let Some(control) = std::env::var_os("HIRSEL_CONTROL_FD") else {
        return Ok(None);
    };
    if control != "4" || std::env::var_os("HIRSEL_HOST_PID").is_none() {
        return Err(io::Error::from_raw_os_error(libc::EINVAL));
    }
    // SAFETY: the host transfers fd 4. Check its peer before taking ownership.
    unsafe {
        let mut peer: libc::sockaddr_un = std::mem::zeroed();
        let mut len = std::mem::size_of_val(&peer) as libc::socklen_t;
        if libc::getpeername(4, (&raw mut peer).cast(), &mut len) < 0 {
            return Err(io::Error::last_os_error());
        }
        if i32::from(peer.sun_family) != libc::AF_UNIX {
            return Err(io::Error::from_raw_os_error(libc::EINVAL));
        }
        if libc::fcntl(4, libc::F_SETFD, libc::FD_CLOEXEC) < 0 {
            return Err(io::Error::last_os_error());
        }
        let fd = OwnedFd::from_raw_fd(4);
        let flags = libc::fcntl(4, libc::F_GETFL);
        if flags < 0 || libc::fcntl(4, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Some(fd))
    }
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
pub(crate) fn prepare_hex<const N: usize>(text: &[u8]) -> io::Result<[u8; N]> {
    let invalid = || io::Error::from_raw_os_error(libc::EINVAL);
    if text.len() != 2 * N {
        return Err(invalid());
    }
    let mut out = [0u8; N];
    for (byte, pair) in out.iter_mut().zip(text.chunks_exact(2)) {
        let hi = char::from(pair[0]).to_digit(16).ok_or_else(invalid)?;
        let lo = char::from(pair[1]).to_digit(16).ok_or_else(invalid)?;
        *byte = (hi * 16 + lo) as u8;
    }
    Ok(out)
}

/// The platform launcher pins the image across exec, including the serving shell's
/// exec. Consume its descriptor before any grants or process metadata are opened.
fn receive_launch_image() -> io::Result<Option<[u8; 32]>> {
    use std::os::unix::fs::MetadataExt;

    let Some(value) = std::env::var_os(hirsel_core_ids::launch::LAUNCH_IMAGE_ENV) else {
        return Ok(None);
    };
    let invalid = || io::Error::from_raw_os_error(libc::EINVAL);
    let Some((parent, image)) = value.to_str().and_then(|text| text.split_once(':')) else {
        return Ok(None);
    };
    // SAFETY: getppid takes no arguments.
    if parent.parse::<libc::pid_t>().ok() != Some(unsafe { libc::getppid() }) {
        return Ok(None);
    }
    let (fd, digest) = image.split_once(':').ok_or_else(invalid)?;
    let fd = fd
        .parse::<i32>()
        .ok()
        .filter(|fd| *fd >= 6)
        .ok_or_else(invalid)?;
    let digest = prepare_hex(digest.as_bytes())?;
    let image = std::fs::metadata(format!("/proc/self/fd/{fd}"))?;
    let running = std::fs::metadata("/proc/self/exe")?;
    if (image.dev(), image.ino()) != (running.dev(), running.ino()) {
        return Err(invalid());
    }
    // SAFETY: the launch transferred this descriptor for the running image.
    drop(unsafe { OwnedFd::from_raw_fd(fd) });
    Ok(Some(digest))
}

pub(crate) enum StartupError {
    Io(io::Error),
    /// A directory grant that could not be opened: its path as configured,
    /// and the path it resolves to when that is relative.
    Directory(std::path::PathBuf, io::Error),
    MissingGrant(usize),
    MissingSecret(usize),
    NetRules,
    UnixProbe(io::Error),
    EnvPatterns,
    EnvSecretOverlap(String),
    UnsupportedHostOpenat2(io::Error),
    Exec(String),
}
impl From<io::Error> for StartupError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}
impl StartupError {
    fn into_io(self) -> io::Error {
        match self {
            Self::Io(error)
            | Self::Directory(_, error)
            | Self::UnsupportedHostOpenat2(error)
            | Self::UnixProbe(error) => error,
            Self::Exec(reason) => io::Error::other(reason),
            Self::MissingGrant(_)
            | Self::MissingSecret(_)
            | Self::NetRules
            | Self::EnvPatterns
            | Self::EnvSecretOverlap(_) => io::Error::from_raw_os_error(libc::EACCES),
        }
    }
}

unsafe fn load_env(
    input: &OsConfig,
    describe_mode: bool,
) -> Result<Option<Box<crate::env::Env>>, StartupError> {
    // SAFETY: the launcher pins the pattern and Secret source tables.
    unsafe {
        let patterns = std::str::from_utf8(bytes(input.env_patterns))
            .ok()
            .and_then(hirsel_core_net::EnvPatterns::parse)
            .ok_or(StartupError::EnvPatterns)?;
        for i in 0..input.secret_grants_len {
            let source = bytes((*input.secret_grants.add(i))[1]);
            if let Ok(source) = std::str::from_utf8(source)
                && let Some(hirsel_core_net::SecretSource::Env(name)) =
                    hirsel_core_net::SecretSource::parse(source)
                && patterns.covers(name)
            {
                return Err(StartupError::EnvSecretOverlap(name.to_owned()));
            }
        }
        Ok((!describe_mode).then(|| Box::new(crate::env::Env::snapshot(patterns))))
    }
}

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
            crate::process::arm_host_parent()?;
            let argv = (0..input.args_len).map(|index| bytes(*input.args.add(index)));
            for value in argv.clone() {
                std::str::from_utf8(value)
                    .map_err(|_| io::Error::from_raw_os_error(libc::EILSEQ))?;
            }
            let mut n_place = hirsel_core_ids::launch::DEFAULT_N_PLACE;
            let mut describe_mode = false;
            let mut preview_mode = false;
            let mut wake = false;
            let mut idle_ms = None;
            let mut prefix = 0;
            let mut exec_approvals = Vec::new();
            for argument in argv.clone() {
                if let Some(approval) = argument.strip_prefix(b"--hirsel-exec=")
                    && input.exec_bindings_len != 0
                {
                    exec_approvals.push(approval);
                } else if argument == b"--hirsel-describe" || argument == b"--hirsel-preview" {
                    if describe_mode {
                        return Err(io::Error::from_raw_os_error(libc::EINVAL).into());
                    }
                    describe_mode = true;
                    preview_mode = argument == b"--hirsel-preview";
                } else if argument == b"--hirsel-wake" {
                    if wake {
                        return Err(io::Error::from_raw_os_error(libc::EINVAL).into());
                    }
                    wake = true;
                } else if let Some(value) = argument.strip_prefix(b"--hirsel-idle=") {
                    if idle_ms.is_some() {
                        return Err(io::Error::from_raw_os_error(libc::EINVAL).into());
                    }
                    let idle = std::str::from_utf8(value)
                        .ok()
                        .and_then(|value| value.parse::<u64>().ok())
                        .filter(|idle| *idle != 0)
                        .ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL))?;
                    idle_ms = Some(idle);
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
            if (wake && (describe_mode || prepare_path.is_some())) || (!wake && idle_ms.is_some()) {
                return Err(io::Error::from_raw_os_error(libc::EINVAL).into());
            }
            let digest = receive_launch_image()?;
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
            };
            if wake {
                launch = launch.with_wake(idle_ms.unwrap_or_default());
            }
            if prepare_path.is_some() {
                launch = launch.with_prepare(prepare_hex(&args[2])?, prepare_hex(&args[3])?);
            }
            if preview_mode {
                use std::io::Read;
                let _blocking = crate::units::blocking();
                let input = std::fs::File::from(duplicate(libc::STDIN_FILENO)?);
                let mut old = Vec::new();
                input
                    .take(n_place.saturating_add(1))
                    .read_to_end(&mut old)?;
                if old.len() as u64 > n_place {
                    return Err(io::Error::from_raw_os_error(libc::EFBIG).into());
                }
                launch = launch.with_preview(old);
            }
            let launch = Box::new(launch);
            // Receive before the first dup/open can reuse a missing activation fd.
            let net_ops = input.socket_activation.as_ref();
            let inherited = if let Some(activation) = net_ops
                && input.grant_mask & (1 << NET) != 0
            {
                (activation.receive)()?
            } else {
                None
            };
            let host_control = receive_host_control()?;
            if host_control.is_some() && (describe_mode || prepare_path.is_some()) {
                return Err(io::Error::from_raw_os_error(libc::EINVAL).into());
            }
            let rows = if input.net_rules_len == 0 {
                &[][..]
            } else {
                std::slice::from_raw_parts(input.net_rules, input.net_rules_len)
            };
            let unix = !describe_mode
                && prepare_path.is_none()
                && input.grant_mask & (1 << NET) != 0
                && rows.iter().any(|row| row.address.dir == 4);
            let exec = if input.exec_bindings_len != 0 {
                let ops = input
                    .exec_ops
                    .as_ref()
                    .ok_or_else(|| StartupError::Exec("no Exec startup operations".into()))?;
                let bindings =
                    std::slice::from_raw_parts(input.exec_bindings, input.exec_bindings_len);
                Some((ops.load)(bindings, &exec_approvals, describe_mode, unix)?)
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
            let stdout = if !describe_mode && input.grant_mask & (1 << STDOUT) != 0 {
                duplicate_stream(input.stdout_fd, libc::STDOUT_FILENO)?
            } else {
                duplicate_stream(input.stderr_fd, libc::STDERR_FILENO)?
            };
            let stderr = duplicate_stream(input.stderr_fd, libc::STDERR_FILENO)?;
            let mut os = Box::new(OsProvider::from_streams(stdout, stderr));
            if !describe_mode && prepare_path.is_none() {
                *os.inherited_listener.get_mut().unwrap() = inherited;
            }
            let describe_out = if describe_mode {
                Some(duplicate(input.stdout_fd)?)
            } else {
                wake_report
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
                    let handle = (operations.grant)(&mut os, path)
                        .map_err(|error| StartupError::Directory(path.to_owned(), error))?;
                    dirs.push(handle);
                }
            }
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
            let views = args
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
            } else if prepare_path.is_some() {
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
            let mut metadata = Vec::new();
            let mut secrets = Vec::new();
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
                        secrets.push(read().ok_or(StartupError::MissingSecret(i))?);
                    }
                }
                metadata.extend(secrets.iter().map(|secret| Grant {
                    kind: SECRET,
                    provider: provider_pointer,
                    handle: (secret as *const crate::secret::Granted) as u64,
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
                for kind in NET..=TASKS {
                    if input.grant_mask & (1 << kind) != 0 {
                        metadata.push(Grant {
                            kind,
                            provider: if kind == PROCESS {
                                stop.as_ref().map_or(provider_pointer, |s| s.provider())
                            } else {
                                provider_pointer
                            },
                            handle: match kind {
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
                _env: env,
                _exec: exec,
                stdin,
                grants: Vec::with_capacity(input.parameters_len),
                diagnostic: vec![],
                config,
                _launch: launch,
                describe_out,
                host,
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
            if let Some(exec) = &process._exec {
                for line in &exec.lines {
                    let outcome = crate::os::write_all(input.stderr_fd, line.as_bytes());
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
            Metadata::native,
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

pub(crate) unsafe fn report_startup(error: StartupError, stderr: i32, names: *const View) {
    let mut line = b"not started: ".to_vec();
    match error {
        StartupError::UnixProbe(error) => {
            line.extend_from_slice(format!("{error}\n").as_bytes());
        }
        StartupError::NetRules => {
            line.extend_from_slice(b"Net requires --grant ");
            line.extend_from_slice(hirsel_core_net::RULE_SYNTAX.as_bytes());
            line.push(b'\n');
        }
        StartupError::EnvPatterns => {
            line.extend_from_slice(hirsel_core_net::EnvPatterns::SYNTAX.as_bytes());
            line.push(b'\n');
        }
        StartupError::EnvSecretOverlap(name) => {
            line.extend_from_slice(
                format!("Env grant covers Secret source variable {name:?}\n").as_bytes(),
            );
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
        StartupError::Exec(reason) => {
            line.extend_from_slice(reason.as_bytes());
            line.push(b'\n');
        }
        StartupError::Io(error) => {
            line.extend_from_slice(
                format!("host grants could not be configured: {error}\n").as_bytes(),
            );
        }
        StartupError::Directory(path, error) => {
            line.extend_from_slice(b"directory grant `");
            line.extend_from_slice(path.as_os_str().as_bytes());
            line.push(b'`');
            if path.is_relative()
                && let Ok(cwd) = std::env::current_dir()
            {
                line.extend_from_slice(b" (");
                line.extend_from_slice(cwd.join(path).as_os_str().as_bytes());
                line.push(b')');
            }
            line.extend_from_slice(format!(": {error}\n").as_bytes());
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
