//! The macOS capture provider. No counter reaches another process without
//! `task_for_pid`, which an ad-hoc signed `hirsel` is not granted, so `run`
//! asks the Hirsel runtime to sample its own threads: it walks each running
//! thread's frame-pointer chain and writes the stacks into its observation
//! stream with the mappings of its images. The importer symbolizes them
//! against the same image the way Linux perf samples are, so the profile,
//! queries and exports are the shared ones.
use std::fs::File;
use std::io;
use std::path::Path;
use std::process::ExitStatus;

use crate::capture::{
    Capture, CaptureOptions, Launch, MemMode, ObservationOptions, Provenance, SymbolSource,
    cmdline, finish, runtime_samples, seed_mappings,
};
use crate::import::Import;
use crate::observation::Observation;
use crate::profile::{ProfileWriter, Result};
use crate::record::{CPU_CLOCK, Identity, Mapping};

pub(crate) const TARGET: &str = "aarch64-apple-darwin";
/// The runtime's monotonic clock on Darwin, which stamps its samples.
pub(crate) const CLOCK: &str = "CLOCK_UPTIME_RAW";
/// The rates the runtime's sampler accepts.
const FREQUENCIES: std::ops::RangeInclusive<u64> = 1..=1_000_000;

/// Profile a just-built Hirsel executable: its runtime samples its own
/// threads at `--freq` on the CPU clock.
pub fn run(
    launch: &Launch<'_>,
    options: &CaptureOptions<'_>,
    resolve: impl FnOnce(&Path, &[u8]) -> io::Result<SymbolSource>,
) -> Result<ExitStatus> {
    if let Some(mode) = launch.observation.and_then(|options| options.mem) {
        return Err(match mode {
            MemMode::Exact => crate::capture::LIVE_EXACT.into(),
            MemMode::Sampled => {
                "hirsel prof run --mem is not available on macOS; record a tape and use 'hirsel prof replay --mem=exact'".into()
            }
        });
    }
    if let Some(pmu) = options.pmu.filter(|pmu| pmu.trim() != CPU_CLOCK.name) {
        return Err(format!(
            "--pmu {pmu}: macOS has no hardware counter for another process; it samples cpu-clock only"
        )
        .into());
    }
    if options.stack_bytes.is_some() {
        return Err("--stack-bytes is not available on macOS: the runtime walks frame pointers and copies no stack".into());
    }
    if !FREQUENCIES.contains(&options.freq) {
        return Err("--freq must be between 1 and 1000000 on macOS".into());
    }
    let path = launch.executable.canonicalize()?;
    let mut executable = File::open(&path)?;
    let provenance = Provenance::load(&path, &mut executable, resolve)?;
    let profile = ProfileWriter::create(options.out)?;
    // The runtime writes its samples into the observation stream, so a run
    // that asked for no task observations still needs one.
    let scratch = match launch.observation {
        Some(_) => None,
        None => Some(tempfile::NamedTempFile::new()?),
    };
    let observation = launch.observation.unwrap_or_else(|| ObservationOptions {
        path: scratch.as_ref().unwrap().path(),
        tasks: false,
        mem: None,
    });
    let mut command = std::process::Command::new(&path);
    command.args(launch.arguments);
    for key in ObservationOptions::KEYS {
        command.env_remove(key);
    }
    if let Some(record) = launch.record {
        command.env("HIRSEL_RECORD", record);
    }
    command.envs(
        observation
            .environment()
            .into_iter()
            .filter_map(|(key, value)| value.map(|value| (key, value))),
    );
    command.env(ObservationOptions::KEYS[3], options.freq.to_string());
    let start = monotonic_ns()?;
    launch.redirect(&mut command)?;
    let mut child = command.spawn()?;
    let pid = child.id();
    let status = child.wait()?;
    let end = monotonic_ns()?;
    let observations = Observation::read(observation.path).map_err(|error| {
        format!(
            "{}: no runtime samples ({error}); profile a Hirsel program built with the profiling runtime",
            path.display()
        )
    })?;
    let mut import = Import::new(
        profile.connection(),
        pid,
        launch.source_root.to_owned(),
        provenance.main.clone(),
        provenance.identity,
    );
    import.source_remarks()?;
    seed_mappings(&mut import, &observations)?;
    runtime_samples(&mut import, &observations)?;
    if launch.observation.is_some() {
        observations.store(profile.connection())?;
    }
    import.close(end)?;
    drop(import);
    finish(
        profile,
        Capture {
            mode: "run",
            cmdline: cmdline(&path, launch.arguments)?,
            start,
            end,
            // Records the runtime could not buffer, samples among them.
            samples_lost: observations.lost,
            freq: options.freq,
            fallback: String::new(),
            event: CPU_CLOCK,
            provenance,
        },
    )?;
    Ok(status)
}

/// Refused: sampling a process this one did not launch needs
/// `task_for_pid`, which an ad-hoc signed `hirsel` is not granted.
pub fn attach(
    _pid: u32,
    _duration: Option<std::time::Duration>,
    _options: &CaptureOptions<'_>,
    _resolve: impl FnOnce(&Path, &[u8]) -> io::Result<SymbolSource>,
) -> Result<()> {
    Err("hirsel prof attach is not available on macOS: sampling another process needs task_for_pid, which an ad-hoc signed hirsel is not granted; use 'hirsel prof run'".into())
}

pub(crate) fn monotonic_ns() -> io::Result<u64> {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: a valid writable timespec for a host clock.
    if unsafe { libc::clock_gettime(libc::CLOCK_UPTIME_RAW, &mut now) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(now.tv_sec as u64 * 1_000_000_000 + now.tv_nsec as u64)
}

/// The runtime samples user stacks only.
pub(crate) fn kernel_address(_pc: u64) -> bool {
    false
}

/// macOS publishes no kernel symbols to a user process, and its samples
/// hold no kernel PCs.
pub(crate) struct KernelSymbols;

impl KernelSymbols {
    pub(crate) fn read() -> Self {
        Self
    }

    pub(crate) fn label(&self, _pc: u64) -> &str {
        "[kernel]"
    }
}

/// The file a mapping names, refused when it is no longer the mapped image.
pub(crate) fn mapped_debug(_pid: u32, _path: &Path, _image: &[u8]) -> Option<SymbolSource> {
    None
}

pub(crate) fn mapped_file(_pid: u32, mapping: &Mapping) -> Result<File> {
    if !mapping.path.is_absolute() {
        return Err("mapping has no file".into());
    }
    let file = File::open(&mapping.path)?;
    if let Some(expected) = Identity::mapping(mapping)
        && Identity::read(&file)? != expected
    {
        return Err("mapped image identity changed".into());
    }
    Ok(file)
}

pub(crate) fn vdso(_pid: u32, _mapping: &Mapping) -> Result<Vec<u8>> {
    Err("macOS has no vDSO".into())
}
