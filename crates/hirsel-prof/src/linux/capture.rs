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
use crate::profile::{ProfileWriter, Result, RunMeta};

/// The settings common to launching or attaching to a native process.
pub struct CaptureOptions<'a> {
    pub out: &'a Path,
    pub freq: u64,
    pub pmu: Option<&'a str>,
    pub stack_bytes: u32,
}

pub struct Launch<'a> {
    pub executable: &'a Path,
    pub arguments: &'a [String],
    pub record: Option<&'a OsStr>,
    pub source_root: &'a Path,
}

/// Profile a just-built executable, starting its counters at successful exec.
pub fn run(
    launch: &Launch<'_>,
    options: &CaptureOptions<'_>,
    resolve: impl FnOnce(&Path, &[u8]) -> io::Result<SymbolSource>,
) -> Result<ExitStatus> {
    let cpus = perf::online_cpus()?;
    perf::validate_settings(options.freq, options.stack_bytes)?;
    let path = launch.executable.canonicalize()?;
    let mut executable = File::open(&path)?;
    let provenance = Provenance::load(&path, &mut executable, resolve)?;
    let profile = ProfileWriter::create(options.out)?;
    let mut child = SuspendedLaunchedProcess::fork(&path, launch.arguments, launch.record)?;
    let pid = child.pid() as u32;
    let (mut group, events, fallback) = open_perf(pid, &cpus, options, true)?;
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
    let mut status = None;
    while status.is_none() {
        for record in group.collect(false)? {
            import.record(record)?;
        }
        status = child.try_wait()?;
        if status.is_none() {
            group.wait()?;
        }
    }
    let end = perf::monotonic_ns()?;
    for record in group.collect(true)? {
        import.record(record)?;
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
            event: events[0],
            provenance,
        },
    )?;
    Ok(status.expect("child status"))
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
