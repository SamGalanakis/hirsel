//! What a capture is asked to do, and what every capture does once its host
//! has sampled: the image's provenance, the runtime's observations, a replay
//! and the run metadata. Sampling itself is a host mechanism (`crate::host`).
use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::process::ExitStatus;
use std::rc::Rc;

use crate::image::Image;
use crate::import::Import;
use crate::observation::{Observation, RecordKind};
use crate::profile::{ProfileWriter, Result, RunMeta};
use crate::record::{Event, Identity, RUNTIME, parse_mappings};

/// The caller verifies detached DWARF against the executable before selecting it.
pub enum SymbolSource {
    EmbeddedDwarf,
    DetachedDwarf(Vec<u8>),
    SymtabOnly,
    OffsetsOnly,
}

/// The settings common to launching or attaching to a native process.
pub struct CaptureOptions<'a> {
    pub out: &'a Path,
    pub freq: u64,
    pub pmu: Option<&'a str>,
    /// User stack bytes copied per sample; `None` keeps the host default.
    /// Only a provider that copies stacks accepts it.
    pub stack_bytes: Option<u32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemMode {
    Exact,
    Sampled,
}

impl MemMode {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Sampled => "sampled",
        }
    }
}

#[derive(Clone, Copy)]
pub struct ObservationOptions<'a> {
    pub path: &'a Path,
    pub tasks: bool,
    pub mem: Option<MemMode>,
}

impl<'a> ObservationOptions<'a> {
    /// The runtime's observation settings, then the rate at which a runtime
    /// that samples its own threads does so.
    pub(crate) const KEYS: [&str; 4] = [
        "HIRSEL_PROFILE",
        "HIRSEL_PROFILE_TASKS",
        "HIRSEL_PROFILE_MEM",
        "HIRSEL_PROFILE_CPU_HZ",
    ];

    pub(crate) fn environment(self) -> [(&'static str, Option<&'a OsStr>); 3] {
        [
            (Self::KEYS[0], Some(self.path.as_os_str())),
            (
                Self::KEYS[1],
                Some(OsStr::new(if self.tasks { "1" } else { "0" })),
            ),
            (
                Self::KEYS[2],
                self.mem.map(|mode| OsStr::new(mode.as_str())),
            ),
        ]
    }
}

pub struct Launch<'a> {
    pub executable: &'a Path,
    pub arguments: &'a [String],
    pub record: Option<&'a OsStr>,
    pub observation: Option<ObservationOptions<'a>>,
    pub source_root: &'a Path,
    pub stdio: Option<&'a [File; 2]>,
}

impl Launch<'_> {
    pub(crate) fn redirect(&self, command: &mut std::process::Command) -> io::Result<()> {
        if let Some([stdout, stderr]) = self.stdio {
            command
                .stdout(stdout.try_clone()?)
                .stderr(stderr.try_clone()?);
        }
        Ok(())
    }
}

/// Launch the existing native tape-replay entry without CPU sampling.
/// Its own verifier decides the exit status and no profile is published on refusal.
pub fn replay(
    launch: &Launch<'_>,
    out: &Path,
    resolve: impl FnOnce(&Path, &[u8]) -> io::Result<SymbolSource>,
) -> Result<ExitStatus> {
    let path = launch.executable.canonicalize()?;
    let mut file = File::open(&path)?;
    let provenance = Provenance::load(&path, &mut file, resolve)?;
    let start = crate::host::monotonic_ns()?;
    let mut command = std::process::Command::new(&path);
    command.args(launch.arguments).env_remove("HIRSEL_RECORD");
    for key in ObservationOptions::KEYS {
        command.env_remove(key);
    }
    if let Some(options) = launch.observation {
        command.envs(
            options
                .environment()
                .into_iter()
                .filter_map(|(key, value)| value.map(|value| (key, value))),
        );
    }
    launch.redirect(&mut command)?;
    let mut child = command.spawn()?;
    let pid = child.id();
    let status = child.wait()?;
    if !status.success() {
        return Ok(status);
    }
    let end = crate::host::monotonic_ns()?;
    let profile = ProfileWriter::create(out)?;
    let mut import = Import::new(
        profile.connection(),
        pid,
        launch.source_root.to_owned(),
        provenance.main.clone(),
        provenance.identity,
    );
    import.source_remarks()?;
    if let Some(options) = launch.observation {
        let observations = Observation::read(options.path)?;
        if let Some(mode) = options.mem {
            import.store_memory(&observations, mode, false)?;
        } else {
            seed_mappings(&mut import, &observations)?;
        }
        observations.store(profile.connection())?;
    }
    import.close(end)?;
    drop(import);
    finish(
        profile,
        Capture {
            mode: "replay",
            cmdline: cmdline(&path, launch.arguments)?,
            start,
            end,
            samples_lost: 0,
            freq: 0,
            fallback: String::new(),
            event: RUNTIME,
            provenance,
        },
    )?;
    Ok(status)
}

pub(crate) const LIVE_EXACT: &str =
    "a live memory profile is sampled; record a tape and use 'hirsel prof replay --mem=exact'";

/// The process's mappings from the runtime's own snapshot at startup.
pub(crate) fn seed_mappings(import: &mut Import<'_>, observations: &Observation) -> Result<()> {
    let mut maps = observations
        .records
        .iter()
        .filter_map(|record| match &record.kind {
            RecordKind::Mapping(line) => Some((record.ts, line)),
            _ => None,
        })
        .peekable();
    let Some(&(time, _)) = maps.peek() else {
        return Ok(());
    };
    let bytes = maps
        .flat_map(|(_, line)| line.iter().copied().chain([b'\n']))
        .collect::<Vec<_>>();
    import.initialize(Vec::new(), parse_mappings(&bytes)?, time)
}

/// The stacks the runtime sampled from its own threads, where the host
/// samples a program from inside it.
pub(crate) fn runtime_samples(import: &mut Import<'_>, observations: &Observation) -> Result<()> {
    for record in &observations.records {
        if let RecordKind::CpuSample {
            tid,
            period,
            truncated,
            pcs,
        } = &record.kind
        {
            import.walked(*tid, record.ts, *period, pcs, *truncated)?;
        }
    }
    Ok(())
}

pub(crate) fn cmdline(path: &Path, arguments: &[String]) -> Result<String> {
    Ok(serde_json::to_string(
        &std::iter::once(path.to_string_lossy())
            .chain(arguments.iter().map(|arg| arg.as_str().into()))
            .collect::<Vec<_>>(),
    )?)
}

pub(crate) struct Provenance {
    pub(crate) main: Rc<Image>,
    pub(crate) identity: Identity,
    tier: &'static str,
}

impl Provenance {
    pub(crate) fn load(
        path: &Path,
        file: &mut File,
        resolve: impl FnOnce(&Path, &[u8]) -> io::Result<SymbolSource>,
    ) -> Result<Self> {
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        let identity = Identity::read(file)?;
        let symbols = resolve(path, &bytes)?;
        let tier = symbols.tier();
        let main = Rc::new(Image::parse(&bytes, symbols, true)?);
        Ok(Self {
            main,
            identity,
            tier,
        })
    }
}

pub(crate) struct Capture {
    pub(crate) mode: &'static str,
    pub(crate) cmdline: String,
    pub(crate) start: u64,
    pub(crate) end: u64,
    pub(crate) samples_lost: u64,
    pub(crate) freq: u64,
    pub(crate) fallback: String,
    pub(crate) event: Event,
    pub(crate) provenance: Provenance,
}

pub(crate) fn finish(profile: ProfileWriter, capture: Capture) -> Result<()> {
    profile.finish(&RunMeta {
        capture_mode: capture.mode,
        target: crate::host::TARGET,
        cmdline: &capture.cmdline,
        build_id: capture.provenance.main.build_id.as_deref().unwrap_or(""),
        image_digest: &capture.provenance.main.digest,
        symbol_tier: capture.provenance.tier,
        clock: crate::host::CLOCK,
        primary_event: capture.event.name,
        primary_unit: capture.event.unit,
        freq_hz: capture.freq,
        fallback_reason: &capture.fallback,
        start_ns: capture.start,
        duration_ns: capture
            .end
            .checked_sub(capture.start)
            .ok_or("monotonic clock regressed")?,
        samples_lost: capture.samples_lost,
    })
}
