//! CLI entry point for `hirsel launcher` (docs/spec/launch.md "Starting the host",
//! "The launcher").
use std::ffi::OsString;
use std::path::PathBuf;

use super::admit::Admission;
use super::bootstrap;
use super::cgroup::Lifecycle;
use super::stores::Stores;
use super::{Launcher, STAGING_BYTES, bind_control, limits, private_dir};

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
    let (mut control, mut state) = (None, None);
    let (mut operator, mut platform_host) = (None, None);
    let (mut max_places, mut idle_ms, mut lifecycle) = (None, None, None);
    for (option, value) in invocation.options {
        let text = value.unwrap_or_default();
        let value = PathBuf::from(&text);
        match option {
            Opt::Control => control = Some(value),
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
                return Err(
                    "--max-places and --idle-ms require --operator-listen and --platform-host"
                        .into(),
                );
            }
            None
        }
        (Some(operator), Some(program)) => {
            let max_places = max_places.ok_or("--max-places N is required")?;
            Some((
                operator,
                program,
                max_places,
                idle_ms.unwrap_or(DEFAULT_IDLE_MS),
            ))
        }
        _ => return Err("--operator-listen and --platform-host go together".into()),
    };
    let state = private_dir(state_arg)?;
    let mut admission = None;
    let mut launcher = Launcher::new(&state, lifecycle, || {
        let stores = Stores::open(&state, limits().recipe_bytes)?;
        admission = Some(Admission::open(
            &state,
            stores.records(),
            limits(),
            STAGING_BYTES,
        )?);
        Ok(stores)
    })?;
    launcher.admit_with(admission.expect("opened with the stores"))?;
    launcher.listen(bind_control(&control)?)?;
    if let Some((operator, program, max_places, idle_ms)) = bootstrap_spec {
        // The host's argv names the control socket by its name in STATE, which is the
        // host's working directory (launch.md "Starting the host").
        let name = control
            .file_name()
            .filter(|_| control.parent().and_then(|p| p.canonicalize().ok()) == Some(state.clone()))
            .ok_or("--control must name a socket directly in --state")?;
        let args = vec![
            OsString::from("ctl"),
            name.to_owned(),
            OsString::from(max_places.to_string()),
            OsString::from(idle_ms.to_string()),
        ];
        let program = std::path::absolute(program)?;
        if !program.is_file() {
            eprintln!(
                "hirsel launcher: the installation has no platform host at {}",
                program.display()
            );
            std::process::exit(74);
        }
        // The host's one Dir is STATE/catalog; it creates the catalog store inside it.
        std::fs::create_dir_all(state.join("catalog"))?;
        launcher.bootstrap = Some(bootstrap::Bootstrap::new(
            program,
            args,
            state.clone(),
            bind_control(&std::path::absolute(operator)?)?,
        ));
    }
    loop {
        launcher.wait_turn(None)?;
        if let Some(code) = launcher.bootstrap.as_ref().and_then(|b| b.stopped()) {
            eprintln!("hirsel launcher: the platform host ended with status {code}");
            drop(launcher);
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
