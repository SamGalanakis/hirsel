//! Options read before the first program argument.
use super::{OsStr, OsStrExt, Path, StartupError, io};

pub(super) struct LaunchOptions<'a> {
    pub(super) n_place: u64,
    pub(super) n_place_explicit: bool,
    pub(super) describe_mode: bool,
    pub(super) preview_mode: bool,
    pub(super) wake: bool,
    pub(super) install_child: Option<u64>,
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
        } else if let Some(value) = argument.strip_prefix(b"--hirsel-install-child=") {
            install_child = Some(
                std::str::from_utf8(value)
                    .ok()
                    .and_then(|value| value.parse::<u64>().ok())
                    .ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL))?,
            );
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
    Ok(LaunchOptions {
        n_place,
        n_place_explicit,
        describe_mode,
        preview_mode,
        wake,
        install_child,
        idle_ms,
        store_id,
        witness,
        prefix,
        exec_approvals,
    })
}

impl LaunchOptions<'_> {
    pub(super) fn dry_run_path<'b>(
        &self,
        args: &'b [Vec<u8>],
        directories: usize,
    ) -> Result<Option<&'b Path>, StartupError> {
        if !args.first().is_some_and(|arg| arg == b"--hirsel-dry-run") {
            return Ok(None);
        }
        let path = args.get(1).map(|value| Path::new(OsStr::from_bytes(value)));
        match path {
            Some(dir)
                if dir.is_absolute()
                    && directories != 0
                    && !self.describe_mode
                    && !self.wake
                    && self.install_child.is_none() =>
            {
                Ok(Some(dir))
            }
            _ => Err(io::Error::from_raw_os_error(libc::EINVAL).into()),
        }
    }

    pub(super) fn prepare_path<'b>(
        &self,
        args: &'b [Vec<u8>],
        directories: usize,
    ) -> Result<Option<&'b Path>, StartupError> {
        let prepare_path = if args.first().is_some_and(|arg| arg == b"--hirsel-prepare") {
            if self.describe_mode || args.len() != 4 || directories == 0 {
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
        let installing = self.install_child.is_some();
        if (self.wake && (self.describe_mode || prepare_path.is_some() || installing))
            || (installing && (self.describe_mode || prepare_path.is_some()))
            || (!self.wake
                && (self.idle_ms.is_some() || self.store_id.is_some() || self.witness.is_some()))
        {
            return Err(io::Error::from_raw_os_error(libc::EINVAL).into());
        }
        Ok(prepare_path)
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
