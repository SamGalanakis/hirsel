use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::process::ExitStatus;
use std::rc::Rc;

use super::image::{Image, SymbolSource};
use super::import::Import;
use super::launch::SuspendedLaunchedProcess;
use super::perf::{self, Event, PerfGroup, PerfSettings};
use super::procfs::Identity;
use crate::memory::Rss;
use crate::observation::{Observation, RecordKind};
use crate::profile::{ProfileWriter, Result, RunMeta};

/// The settings common to launching or attaching to a native process.
pub struct CaptureOptions<'a> {
    pub out: &'a Path,
    pub freq: u64,
    pub pmu: Option<&'a str>,
    pub stack_bytes: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemMode {
    Exact,
    Sampled,
}

impl MemMode {
    pub(super) fn as_str(self) -> &'static str {
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
    pub(super) const KEYS: [&str; 3] = [
        "HIRSEL_PROFILE",
        "HIRSEL_PROFILE_TASKS",
        "HIRSEL_PROFILE_MEM",
    ];

    pub(super) fn environment(self) -> [(&'static str, Option<&'a OsStr>); 3] {
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
}

/// Profile a just-built executable, starting its counters at successful exec.
pub fn run(
    launch: &Launch<'_>,
    options: &CaptureOptions<'_>,
    resolve: impl FnOnce(&Path, &[u8]) -> io::Result<SymbolSource>,
) -> Result<ExitStatus> {
    let mem = launch.observation.and_then(|options| options.mem);
    if mem == Some(MemMode::Exact) {
        return Err(LIVE_EXACT.into());
    }
    perf::validate_settings(options.freq, options.stack_bytes)?;
    let path = launch.executable.canonicalize()?;
    let mut executable = File::open(&path)?;
    let provenance = Provenance::load(&path, &mut executable, resolve)?;
    let profile = ProfileWriter::create(options.out)?;
    // Validate an explicit event list even when the runtime stream can
    // carry on without perf.
    perf::events(options.pmu.unwrap_or("cycles"))?;
    let mut child =
        SuspendedLaunchedProcess::fork(&path, launch.arguments, launch.record, launch.observation)?;
    let pid = child.pid() as u32;
    let sampling = perf::online_cpus().and_then(|cpus| open_perf(pid, &cpus, options, true));
    let (mut group, event, fallback) = match sampling {
        Ok((group, events, fallback)) => (Some(group), events[0], fallback),
        Err(error) if launch.observation.is_some() => {
            let mut tracker = PerfGroup::new();
            match tracker.open_tracking(pid as i32) {
                Ok(()) => (Some(tracker), perf::RUNTIME, error.to_string()),
                Err(track) => (
                    None,
                    perf::RUNTIME,
                    format!("{error}; mapping tracking: {track}"),
                ),
            }
        }
        Err(error) => return Err(error),
    };
    let start = perf::monotonic_ns()?;
    child
        .resume()
        .map_err(|err| format!("exec {}: {err}", path.display()))?;
    let mut import = Import::new(
        profile.connection(),
        pid,
        launch.source_root.to_owned(),
        provenance.main.clone(),
        provenance.identity,
    );
    import.source_remarks()?;
    let mut rss = mem.map(|_| ResidentSet::new(pid));
    let status = loop {
        if let Some(group) = &mut group {
            for record in group.collect(false)? {
                import.record(record)?;
            }
        }
        if let Some(rss) = &mut rss {
            rss.poll()?;
        }
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if let Some(group) = &group {
            group.wait()?;
        } else {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    };
    let end = perf::monotonic_ns()?;
    if let Some(group) = &mut group {
        for record in group.collect(true)? {
            import.record(record)?;
        }
    }
    if let Some(options) = launch.observation {
        let observations = Observation::read(options.path)?;
        if group.is_none() {
            seed_mappings(&mut import, &observations)?;
        }
        if let (Some(mode), Some(rss)) = (mem, &rss) {
            import.store_memory(&observations, mode, Some(&rss.instants()))?;
        }
        observations.store(profile.connection())?;
    }
    import.close(end)?;
    let samples_lost = import.samples_lost;
    drop(import);
    let cmdline = serde_json::to_string(
        &std::iter::once(path.to_string_lossy())
            .chain(launch.arguments.iter().map(|arg| arg.as_str().into()))
            .collect::<Vec<_>>(),
    )?;
    finish(
        profile,
        Capture {
            mode: "run",
            cmdline,
            start,
            end,
            samples_lost,
            freq: options.freq,
            fallback,
            event,
            provenance,
        },
    )?;
    Ok(status)
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
    let start = perf::monotonic_ns()?;
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
    let mut child = command.spawn()?;
    let pid = child.id();
    let status = child.wait()?;
    if !status.success() {
        return Ok(status);
    }
    let end = perf::monotonic_ns()?;
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
            import.store_memory(&observations, mode, None)?;
        } else {
            seed_mappings(&mut import, &observations)?;
        }
        observations.store(profile.connection())?;
    }
    import.close(end)?;
    drop(import);
    let cmdline = serde_json::to_string(
        &std::iter::once(path.to_string_lossy())
            .chain(launch.arguments.iter().map(|arg| arg.as_str().into()))
            .collect::<Vec<_>>(),
    )?;
    finish(
        profile,
        Capture {
            mode: "replay",
            cmdline,
            start,
            end,
            samples_lost: 0,
            freq: 0,
            fallback: String::new(),
            event: perf::RUNTIME,
            provenance,
        },
    )?;
    Ok(status)
}

pub(crate) const LIVE_EXACT: &str =
    "a live memory profile is sampled; record a tape and use 'hirsel prof replay --mem=exact'";

/// Resident set reads at a fixed interval, independent of the memory samples.
struct ResidentSet {
    statm: std::path::PathBuf,
    page: u64,
    next: u64,
    peak: Option<Rss>,
    last: Option<Rss>,
}

const RSS_INTERVAL_NS: u64 = 10_000_000;

impl ResidentSet {
    fn new(pid: u32) -> Self {
        // SAFETY: sysconf has no memory operands.
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64;
        Self {
            statm: format!("/proc/{pid}/statm").into(),
            page,
            next: 0,
            peak: None,
            last: None,
        }
    }

    fn poll(&mut self) -> Result<()> {
        let ts = perf::monotonic_ns()?;
        if ts < self.next {
            return Ok(());
        }
        self.next = ts + RSS_INTERVAL_NS;
        // The process can exit or become a zombie between reads.
        let Ok(statm) = std::fs::read_to_string(&self.statm) else {
            return Ok(());
        };
        let Some(pages) = statm
            .split_ascii_whitespace()
            .nth(1)
            .and_then(|pages| pages.parse::<u64>().ok())
            .filter(|&pages| pages != 0)
        else {
            return Ok(());
        };
        let read = Rss {
            label: "peak",
            ts,
            bytes: pages * self.page,
        };
        if self.peak.is_none_or(|peak| read.bytes > peak.bytes) {
            self.peak = Some(read);
        }
        self.last = Some(read);
        Ok(())
    }

    /// The highest read and the last read before exit.
    fn instants(&self) -> Vec<Rss> {
        let last = self.last.map(|last| Rss {
            label: "last",
            ..last
        });
        self.peak.into_iter().chain(last).collect()
    }
}

fn seed_mappings(import: &mut Import<'_>, observations: &Observation) -> Result<()> {
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
    import.initialize(Vec::new(), super::procfs::parse_mappings(&bytes)?, time)
}

pub(super) struct Provenance {
    pub(super) main: Rc<Image>,
    pub(super) identity: Identity,
    tier: &'static str,
}

impl Provenance {
    pub(super) fn load(
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

pub(super) struct Capture {
    pub(super) mode: &'static str,
    pub(super) cmdline: String,
    pub(super) start: u64,
    pub(super) end: u64,
    pub(super) samples_lost: u64,
    pub(super) freq: u64,
    pub(super) fallback: String,
    pub(super) event: Event,
    pub(super) provenance: Provenance,
}

pub(super) fn finish(profile: ProfileWriter, capture: Capture) -> Result<()> {
    #[cfg(target_arch = "x86_64")]
    let target = "x86_64-unknown-linux-gnu";
    #[cfg(target_arch = "aarch64")]
    let target = "aarch64-unknown-linux-gnu";
    profile.finish(&RunMeta {
        capture_mode: capture.mode,
        target,
        cmdline: &capture.cmdline,
        build_id: capture.provenance.main.build_id.as_deref().unwrap_or(""),
        image_digest: &capture.provenance.main.digest,
        symbol_tier: capture.provenance.tier,
        clock: "CLOCK_MONOTONIC",
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

pub(super) fn open_perf(
    pid: u32,
    cpus: &[i32],
    options: &CaptureOptions<'_>,
    enable_on_exec: bool,
) -> Result<(PerfGroup, Vec<Event>, String)> {
    let mut events = perf::events(options.pmu.unwrap_or("cycles"))?;
    let open = |events: &[Event]| {
        let mut group = PerfGroup::new();
        open_rings(&mut group, pid, cpus, events, options, enable_on_exec)?;
        Ok::<_, (io::Error, Event)>(group)
    };
    match open(&events) {
        Ok(group) => Ok((group, events, String::new())),
        Err((first, event))
            if options.pmu.is_none()
                && matches!(
                    first.raw_os_error(),
                    Some(libc::ENODEV | libc::ENOENT | libc::EOPNOTSUPP)
                ) =>
        {
            let reason = format!("{} unavailable: {first}", event.name);
            events[0] = perf::CPU_CLOCK;
            let group = open(&events).map_err(|(err, event)| {
                format!("{reason}; fallback {}", perf::open_error(err, event))
            })?;
            Ok((group, events, reason))
        }
        Err((err, event)) => Err(perf::open_error(err, event).into()),
    }
}

pub(super) fn open_rings(
    group: &mut PerfGroup,
    tid: u32,
    cpus: &[i32],
    events: &[Event],
    options: &CaptureOptions<'_>,
    enable_on_exec: bool,
) -> std::result::Result<(), (io::Error, Event)> {
    for &cpu in cpus {
        for (index, &event) in events.iter().enumerate() {
            group
                .open(
                    tid as i32,
                    cpu,
                    event,
                    PerfSettings {
                        freq: options.freq,
                        stack_bytes: options.stack_bytes,
                        sideband: enable_on_exec && index == 0,
                        enable_on_exec,
                    },
                )
                .map_err(|err| (err, event))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;

    use super::super::image::local_symbols;
    use super::*;
    use crate::Profile;

    #[test]
    fn launch_samples_dwarf_through_a_twenty_kib_frame() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("fixture.c");
        let executable = dir.path().join("fixture");
        let profile = dir.path().join("fixture.hprof");
        fs::write(
            &source,
            r#"
#include <stdint.h>
#include <time.h>

__attribute__((noinline)) static unsigned long hot_leaf(void) {
    struct timespec start, now;
    volatile unsigned long value = 1;
    clock_gettime(CLOCK_THREAD_CPUTIME_ID, &start);
    do {
        for (int i = 0; i < 100000; i++)
            value = value * 5 + 1;
        clock_gettime(CLOCK_THREAD_CPUTIME_ID, &now);
    } while ((now.tv_sec - start.tv_sec) * 1000000000L + now.tv_nsec - start.tv_nsec < 350000000L);
    return value;
}

__attribute__((noinline)) static unsigned long large_frame(void) {
    volatile unsigned char frame[20 * 1024];
    frame[0] = 1;
    frame[sizeof(frame) - 1] = 2;
    __asm__ volatile("" : : "r"(&frame) : "memory");
    unsigned long value = hot_leaf();
    return value + frame[0] + frame[sizeof(frame) - 1];
}

int main(void) {
    return large_frame() == 7 ? 9 : 0;
}
"#,
        )
        .unwrap();
        let build = Command::new("cc")
            .args([
                "-O2",
                "-g",
                "-fomit-frame-pointer",
                "-fasynchronous-unwind-tables",
                "-fno-optimize-sibling-calls",
            ])
            .arg(&source)
            .arg("-o")
            .arg(&executable)
            .status()
            .unwrap();
        assert!(build.success());
        let status = run(
            &Launch {
                executable: &executable,
                arguments: &[],
                record: None,
                observation: None,
                source_root: dir.path(),
            },
            &CaptureOptions {
                out: &profile,
                freq: 997,
                pmu: Some("cpu-clock"),
                stack_bytes: 32768,
            },
            |_, bytes| Ok(local_symbols(bytes)),
        )
        .unwrap();
        assert!(status.success());
        fs::remove_file(&executable).unwrap();
        fs::remove_file(&source).unwrap();
        let opened = Profile::open(&profile).unwrap();
        let db = opened.connection();
        let samples: i64 = db
            .query_row("SELECT count(*) FROM cpu_sample", [], |r| r.get(0))
            .unwrap();
        assert!(samples > 0);
        let hot: i64 = db.query_row(
            "SELECT s.sample_count FROM fn_cpu_summary s JOIN function f ON f.id=s.function_id WHERE f.name='hot_leaf'",
            [], |r| r.get(0)
        ).unwrap();
        // The only sustained CPU work is in hot_leaf. Its caller and main sit
        // above large_frame's 20 KiB array, beyond a short stack snapshot.
        let deep: i64 = db
            .query_row(
                "WITH RECURSIVE chain(sample_id,callsite_id) AS (
               SELECT id,callsite_id FROM cpu_sample
               UNION ALL
               SELECT sample_id,c.parent_id FROM chain JOIN callsite c ON c.id=chain.callsite_id
               WHERE c.parent_id IS NOT NULL
             )
             SELECT count(DISTINCT sample_id) FROM chain a JOIN callsite c ON c.id=a.callsite_id
             JOIN frame r ON r.id=c.frame_id JOIN function f ON f.id=r.function_id
             WHERE f.name='main'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(hot > samples / 2, "hot_leaf={hot}, samples={samples}");
        assert!(
            deep > samples / 2,
            "unwound to main={deep}, samples={samples}"
        );
        let retained: i64 = db.query_row(
            "SELECT count(*) FROM source_snippet WHERE file LIKE '%fixture.c' AND text LIKE '%value%'",
            [], |r| r.get(0),
        ).unwrap();
        assert!(retained > 0);
    }
}
