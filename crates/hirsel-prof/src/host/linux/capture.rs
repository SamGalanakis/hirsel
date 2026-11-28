use std::fs::File;
use std::io;
use std::path::Path;
use std::process::ExitStatus;

use super::launch::SuspendedLaunchedProcess;
use super::perf::{self, Event, PerfGroup, PerfSettings};
pub(crate) use crate::capture::{
    Capture, CaptureOptions, LIVE_EXACT, Launch, MemMode, ObservationOptions, Provenance,
    SymbolSource, cmdline, finish, runtime_samples, seed_mappings,
};
use crate::import::Import;
use crate::observation::Observation;
use crate::profile::{ProfileWriter, Result};

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
    let mut child = SuspendedLaunchedProcess::fork(
        &path,
        launch.arguments,
        launch.record,
        launch.observation,
        launch.stdio,
    )?;
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
    let status = loop {
        if let Some(group) = &mut group {
            for record in group.collect(false)? {
                import.record(record)?;
            }
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
        runtime_samples(&mut import, &observations)?;
        if let Some(mode) = mem {
            import.store_memory(&observations, mode, true)?;
        }
        observations.store(profile.connection())?;
    }
    import.close(end)?;
    let samples_lost = import.samples_lost;
    drop(import);
    finish(
        profile,
        Capture {
            mode: "run",
            cmdline: cmdline(&path, launch.arguments)?,
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
                        stack_bytes: options.stack_bytes.unwrap_or(perf::STACK_BYTES),
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

    use std::rc::Rc;

    use super::super::perf::{CPU_CLOCK, decode};
    use super::super::procfs::KernelSymbols;
    use super::*;
    use crate::image::{self, Image, local_symbols};
    use crate::record::Identity;
    use crate::{By, Group, Profile, ProfileWriter, Query, RunMeta, query};
    use serde_json::json;

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
                stdio: None,
                source_root: dir.path(),
            },
            &CaptureOptions {
                out: &profile,
                freq: 997,
                pmu: Some("cpu-clock"),
                stack_bytes: None,
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

    #[test]
    fn kernel_samples_keep_periods_without_user_unwind_failures() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kernel.hprof");
        let profile = ProfileWriter::create(&path).unwrap();
        let exe = std::env::current_exe().unwrap();
        let identity = Identity::read(&std::fs::File::open(&exe).unwrap()).unwrap();
        let bytes = std::fs::read(exe).unwrap();
        let image = Rc::new(Image::parse(&bytes, image::SymbolSource::OffsetsOnly, true).unwrap());
        let mut import = Import::new(profile.connection(), 1, dir.path().into(), image, identity);
        import.kernel_symbols = Some(KernelSymbols::parse(
            "0000000000000000 T hidden\nffffffff81000100 T page_fault\nffffffff81000200 D data\n",
        ));
        // Linux perf sample records with no user registers or stack bytes.
        for (pc, mode, period) in [
            (0xffff_ffff_8100_0110u64, 2, 11),
            (0xffff_ffff_8100_0200, 2, 7),
            (0x10, 1, 5),
            (0x20, 2, 13),
        ] {
            let pid_tid = (1u64 << 32) | 1;
            let payload: Vec<u8> = [pc, pid_tid, 1, 0, period, 0, 0]
                .into_iter()
                .flat_map(u64::to_ne_bytes)
                .collect();
            import
                .record(decode(9, mode, &payload, CPU_CLOCK).unwrap().unwrap())
                .unwrap();
        }
        drop(import);
        profile
            .finish(&RunMeta {
                capture_mode: "attach",
                target: "linux",
                cmdline: "[]",
                build_id: "",
                image_digest: "",
                symbol_tier: "offsets_only",
                clock: "CLOCK_MONOTONIC",
                primary_event: "cpu-clock",
                primary_unit: "nanoseconds",
                freq_hz: 997,
                fallback_reason: "",
                start_ns: 0,
                duration_ns: 2,
                samples_lost: 0,
            })
            .unwrap();
        let top = query(
            &path,
            Query::Top {
                by: By::SelfCost,
                event: None,
                group: Group::DefPath,
                filter: None,
            },
            10,
        )
        .unwrap();
        assert_eq!(top.quality["kernel_samples"], 3);
        assert_eq!(top.quality["unwind_failed"], 1);
        assert_eq!(top.quality["unknown_mapping"], 1);
        assert_eq!(top.quality["unresolved_samples"], 1);
        assert_eq!(top.quality["host_root_samples"], 0);
        assert_eq!(
            top.result["rows"],
            json!([
                {"function":"[kernel]", "self_total":12, "inclusive_total":12, "sample_count":2},
                {"function":"[kernel] page_fault", "self_total":11, "inclusive_total":11, "sample_count":1},
            ])
        );
    }
}
