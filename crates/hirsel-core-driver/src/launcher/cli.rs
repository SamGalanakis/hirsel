//! CLI entry point for `hirsel launcher` (docs/spec/launch.md "Starting the host",
//! "The launcher").
use std::ffi::OsString;
use std::io;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

use super::admit::{Admission, launcher_envelope, write_envelope};
use super::bootstrap;
use super::cgroup::Lifecycle;
use super::stores::Stores;
use super::{Launcher, STAGING_BYTES, private_dir};

/// Default idle timeout passed to the platform host when `--idle-ms` is omitted.
const DEFAULT_IDLE_MS: u64 = 30_000;

pub(crate) fn parse_lifecycle(text: &str) -> Result<Lifecycle, Box<dyn std::error::Error>> {
    match text {
        "managed" => Ok(Lifecycle::Managed),
        "leader" => Ok(Lifecycle::Leader),
        _ => Err("--lifecycle takes managed or leader".into()),
    }
}

pub(crate) fn resolve_lifecycle(
    lifecycle: Option<Lifecycle>,
) -> Result<Lifecycle, Box<dyn std::error::Error>> {
    match lifecycle {
        Some(mode) => Ok(mode),
        None if crate::host::CGROUPS => Err("--lifecycle managed|leader is required".into()),
        None => Ok(Lifecycle::Leader),
    }
}

/// Lock `state`, recover any child cgroups in managed mode, then serve the control
/// session on `SOCKET`.
pub(crate) fn run(
    invocation: crate::cli::Invocation,
) -> Result<std::convert::Infallible, Box<dyn std::error::Error>> {
    use crate::cli::OptionId as Opt;
    let (mut control, mut state, mut admission_file) = (None, None, None);
    let (mut operator, mut platform_host) = (None, None);
    let (mut max_places, mut idle_ms, mut lifecycle) = (None, None, None);
    for (option, value) in invocation.options {
        let text = value.unwrap_or_default();
        let value = PathBuf::from(&text);
        match option {
            Opt::Control => control = Some(value),
            Opt::Admission => admission_file = Some(value),
            Opt::HostState => state = Some(value),
            Opt::OperatorListen => operator = Some(value),
            Opt::PlatformHost => platform_host = Some(value),
            Opt::MaxPlaces => max_places = Some(positive(&text, "--max-places")?),
            Opt::IdleMs => idle_ms = Some(positive(&text, "--idle-ms")?),
            Opt::Lifecycle => lifecycle = Some(parse_lifecycle(&text)?),
            _ => return Err("unexpected launcher option".into()),
        }
    }
    let state_arg = state.ok_or("--state DIR is required")?;
    let control = control.ok_or("--control SOCKET is required")?;
    let lifecycle = resolve_lifecycle(lifecycle)?;
    let bootstrap_spec = match (operator, platform_host) {
        (None, None) => {
            if max_places.is_some() || idle_ms.is_some() {
                return Err("--max-places and --idle-ms require --operator-listen".into());
            }
            None
        }
        (Some(operator), program) => {
            let max_places = max_places.ok_or("--max-places N is required")?;
            Some((
                operator,
                program,
                max_places,
                idle_ms.unwrap_or(DEFAULT_IDLE_MS),
            ))
        }
        _ => return Err("--platform-host requires --operator-listen".into()),
    };
    let envelope = launcher_envelope(bootstrap_spec.is_some(), admission_file)?;
    let state = private_dir(state_arg)?;
    let mut admission = None;
    let production = bootstrap_spec.is_some();
    let mut launcher = Launcher::new(&state, lifecycle, || {
        let stores = Stores::open(&state)?;
        if production {
            write_envelope(&state, &envelope)?;
        }
        admission = Some(Admission::open(
            &state,
            stores.records(),
            envelope,
            STAGING_BYTES,
        )?);
        Ok(stores)
    })?;
    launcher.admit_with(admission.expect("opened with the stores"))?;
    launcher.listen(bind_control(&control)?)?;
    let mut embedded = None;
    if let Some((operator, program, max_places, idle_ms)) = bootstrap_spec {
        // The host's argv names both sockets relative to its working directory, STATE.
        let name = control
            .file_name()
            .filter(|_| control.parent().and_then(|p| p.canonicalize().ok()) == Some(state.clone()))
            .ok_or("--control must name a socket directly in --state")?;
        let program = match program {
            Some(program) => std::path::absolute(program)?,
            None => match super::embedded_host::stage()? {
                Some((directory, program)) => {
                    embedded = Some(directory);
                    program
                }
                None => {
                    eprintln!("hirsel launcher: the executable has no embedded platform host");
                    std::process::exit(74);
                }
            },
        };
        if !program.is_file() {
            eprintln!(
                "hirsel launcher: the installation has no platform host at {}",
                program.display()
            );
            std::process::exit(74);
        }
        let operator = std::path::absolute(operator)?;
        let operator_name = operator
            .file_name()
            .ok_or("--operator-listen must name a socket")?;
        let args = vec![
            OsString::from("ctl"),
            name.to_owned(),
            OsString::from(max_places.to_string()),
            OsString::from(idle_ms.to_string()),
            operator_name.to_owned(),
        ];
        // The host's one Dir is STATE/catalog; it creates the catalog store inside it.
        std::fs::create_dir_all(state.join("catalog"))?;
        launcher.bootstrap = Some(bootstrap::Bootstrap::new(
            program,
            args,
            state.clone(),
            bind_control(&operator)?,
        ));
    }
    loop {
        launcher.wait_turn(None)?;
        if let Some(code) = launcher.bootstrap.as_ref().and_then(|b| b.stopped()) {
            eprintln!("hirsel launcher: the platform host ended with status {code}");
            drop(launcher);
            drop(embedded);
            std::process::exit(code);
        }
    }
}

fn positive(text: &str, name: &str) -> Result<u64, Box<dyn std::error::Error>> {
    match text.parse::<u64>() {
        Ok(n) if n > 0 => Ok(n),
        _ => Err(format!("{name} takes a positive decimal").into()),
    }
}

/// Bind a control socket at `path` that only its owner can connect to. A stale socket
/// nobody answers on is replaced; a live one is in use.
pub(crate) fn bind_control(path: &Path) -> io::Result<UnixListener> {
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if !meta.file_type().is_socket() || UnixStream::connect(path).is_ok() {
            return Err(io::ErrorKind::AddrInUse.into());
        }
        std::fs::remove_file(path)?;
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}
