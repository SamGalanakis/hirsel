//! Options read before the first program argument.
use super::{FromRawFd, Metadata, OsStr, OsStrExt, OwnedFd, Path, StartupError, io};

pub(super) struct LaunchOptions<'a> {
    pub(super) n_place: u64,
    pub(super) n_place_explicit: bool,
    pub(super) describe_mode: bool,
    pub(super) preview_mode: bool,
    pub(super) wake: bool,
    pub(super) install_child: Option<u64>,
    pub(super) move_envelope: Vec<u8>,
    pub(super) dev_store: bool,
    pub(super) install_predecessor: Option<[u8; 32]>,
    pub(super) idle_ms: Option<u64>,
    pub(super) store_id: Option<(u64, u64)>,
    pub(super) witness: Option<u64>,
    pub(super) prefix: usize,
    pub(super) exec_approvals: Vec<&'a [u8]>,
}

pub(super) fn parse<'a>(
    argv: impl Iterator<Item = &'a [u8]>,
    has_exec: bool,
) -> Result<LaunchOptions<'a>, StartupError> {
    let mut n_place = hirsel_core_ids::launch::DEFAULT_N_PLACE;
    let mut n_place_explicit = false;
    let mut describe_mode = false;
    let mut preview_mode = false;
    let mut wake = false;
    let mut install_child = None;
    let mut move_envelope = None;
    let mut dev_store = false;
    let mut install_predecessor = None;
    let mut idle_ms = None;
    let mut store_id = None;
    let mut witness = None;
    let mut prefix = 0;
    let mut exec_approvals = Vec::new();
    for argument in argv {
        if let Some(approval) = argument.strip_prefix(b"--hirsel-exec=")
            && has_exec
        {
            exec_approvals.push(approval);
        } else if argument == b"--hirsel-describe" || argument == b"--hirsel-preview" {
            if describe_mode {
                return Err(io::Error::from_raw_os_error(libc::EINVAL).into());
            }
            describe_mode = true;
            preview_mode = argument == b"--hirsel-preview";
        } else if let Some(value) = argument.strip_prefix(b"--hirsel-move-envelope=") {
            if move_envelope.is_some()
                || value.len() > 2 * hirsel_core_ids::launch::MAX_MOVE_ENVELOPE
            {
                return Err(io::Error::from_raw_os_error(libc::EINVAL).into());
            }
            move_envelope = Some(prepare_hex_bytes(value)?);
        } else if let Some(value) = argument.strip_prefix(b"--hirsel-install-child=") {
            install_child = Some(
                std::str::from_utf8(value)
                    .ok()
                    .and_then(|value| value.parse::<u64>().ok())
                    .ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL))?,
            );
            prefix += 1;
            break;
        } else if let Some(value) = argument.strip_prefix(b"--hirsel-install-predecessor=") {
            if install_predecessor.is_some() {
                return Err(io::Error::from_raw_os_error(libc::EINVAL).into());
            }
            install_predecessor = Some(prepare_hex(value)?);
        } else if argument == b"--hirsel-dev-store" {
            dev_store = true;
            prefix += 1;
            break;
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
        } else if let Some(value) = argument.strip_prefix(b"--hirsel-store-id=") {
            let parsed = std::str::from_utf8(value).ok().and_then(|value| {
                if value.len() != 32 || !value.as_bytes().iter().all(u8::is_ascii_hexdigit) {
                    return None;
                }
                let (hi, lo) = value.split_at(16);
                Some((
                    u64::from_str_radix(hi, 16).ok()?,
                    u64::from_str_radix(lo, 16).ok()?,
                ))
            });
            if store_id.is_some() || parsed.is_none() {
                return Err(io::Error::from_raw_os_error(libc::EINVAL).into());
            }
            store_id = parsed;
        } else if let Some(value) = argument.strip_prefix(b"--hirsel-witness=") {
            if witness.is_some() {
                return Err(io::Error::from_raw_os_error(libc::EINVAL).into());
            }
            witness = Some(
                std::str::from_utf8(value)
                    .ok()
                    .and_then(|value| value.parse::<u64>().ok())
                    .filter(|version| *version != 0)
                    .ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL))?,
            );
        } else if let Some(value) = argument.strip_prefix(b"--hirsel-n-place=") {
            n_place_explicit = true;
            n_place = std::str::from_utf8(value)
                .ok()
                .and_then(|v| v.parse().ok())
                .ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL))?;
        } else {
            break;
        }
        prefix += 1;
    }
    if move_envelope.is_some() && !wake && install_child.is_none() {
        return Err(io::Error::from_raw_os_error(libc::EINVAL).into());
    }
    Ok(LaunchOptions {
        n_place,
        n_place_explicit,
        describe_mode,
        preview_mode,
        wake,
        install_child,
        move_envelope: move_envelope.unwrap_or_default(),
        dev_store,
        install_predecessor,
        idle_ms,
        store_id,
        witness,
        prefix,
        exec_approvals,
    })
}

impl LaunchOptions<'_> {
    pub(super) fn installing(&self) -> bool {
        self.install_child.is_some() || self.install_predecessor.is_some()
    }

    /// `--hirsel-dry-run DIR` (1), `--hirsel-store-verify DIR` (2) or
    /// `--hirsel-store-repair DIR` (3): the launch word 20 value and the store
    /// directory the run uses instead of its granted one. Only a `place`
    /// program (one reaching the runner) reads them; any other takes them in
    /// `Args` as written.
    pub(super) fn store_op_path<'b>(
        &self,
        args: &'b [Vec<u8>],
        directories: usize,
        place: bool,
    ) -> Result<Option<(u8, &'b Path)>, StartupError> {
        if !place {
            return Ok(None);
        }
        let op = match args.first().map(Vec::as_slice) {
            Some(b"--hirsel-dry-run") => 1,
            Some(b"--hirsel-store-verify") => 2,
            Some(b"--hirsel-store-repair") => 3,
            _ => return Ok(None),
        };
        let path = args.get(1).map(|value| Path::new(OsStr::from_bytes(value)));
        match path {
            Some(dir)
                if dir.is_absolute()
                    && directories != 0
                    && !self.describe_mode
                    && !self.dev_store
                    && !self.wake
                    && !self.installing() =>
            {
                Ok(Some((op, dir)))
            }
            _ => Err(io::Error::from_raw_os_error(libc::EINVAL).into()),
        }
    }

    /// `--hirsel-prepare DIR ...` for a `place` program; any other takes the
    /// words in `Args`. Checks the private options' combinations either way.
    pub(super) fn prepare_path<'b>(
        &self,
        args: &'b [Vec<u8>],
        directories: usize,
        place: bool,
    ) -> Result<Option<&'b Path>, StartupError> {
        let prepare_path = if place && args.first().is_some_and(|arg| arg == b"--hirsel-prepare") {
            if self.describe_mode || args.len() != 5 || directories == 0 {
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
        let installing = self.installing();
        if (self.wake && (self.describe_mode || prepare_path.is_some() || installing))
            || (installing && (self.describe_mode || prepare_path.is_some()))
            || (!self.wake && (self.idle_ms.is_some() || self.witness.is_some()))
            || (self.store_id.is_some() && (self.describe_mode || prepare_path.is_some()))
            || (self.install_predecessor.is_some()
                && (self.store_id.is_none() || !self.dev_store || self.install_child.is_some()))
            || (self.dev_store
                && (self.wake || self.describe_mode || prepare_path.is_some() || directories == 0))
        {
            return Err(io::Error::from_raw_os_error(libc::EINVAL).into());
        }
        Ok(prepare_path)
    }
}

pub(crate) fn prepare_hex<const N: usize>(text: &[u8]) -> io::Result<[u8; N]> {
    if text.len() != 2 * N {
        return Err(io::Error::from_raw_os_error(libc::EINVAL));
    }
    let mut out = [0u8; N];
    out.copy_from_slice(&prepare_hex_bytes(text)?);
    Ok(out)
}

pub(super) fn receive_host_control() -> io::Result<Option<OwnedFd>> {
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

/// `--hirsel-prepare DIR ATTEMPT IMAGE FATES`: the attempt, the image digest and the fates.
pub(super) fn with_prepare(launch: Metadata, args: &[Vec<u8>]) -> io::Result<Metadata> {
    let fates = prepare_hex_bytes(&args[4])?;
    Ok(launch.with_prepare(prepare_hex(&args[2])?, prepare_hex(&args[3])?, fates))
}

/// Hex of any even length, as the prepare launch passes its fates.
fn prepare_hex_bytes(text: &[u8]) -> io::Result<Vec<u8>> {
    let invalid = || io::Error::from_raw_os_error(libc::EINVAL);
    if text.len() % 2 != 0 {
        return Err(invalid());
    }
    let digit = |c: u8| match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        b'A'..=b'F' => Ok(c - b'A' + 10),
        _ => Err(invalid()),
    };
    text.chunks_exact(2)
        .map(|pair| Ok(digit(pair[0])? * 16 + digit(pair[1])?))
        .collect()
}

/// The platform launcher pins the image across exec, including the serving shell's
/// exec. Consume its descriptor before any grants or process metadata are opened.
pub(super) fn receive_launch_image() -> io::Result<Option<[u8; 32]>> {
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
    if !crate::host::sys::running_image(fd)? {
        return Err(invalid());
    }
    // SAFETY: the launch transferred this descriptor for the running image.
    drop(unsafe { OwnedFd::from_raw_fd(fd) });
    Ok(Some(digest))
}
