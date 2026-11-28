use std::collections::HashSet;
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;
use std::time::{Duration, Instant};

use super::capture::{
    Capture, CaptureOptions, Provenance, finish, kernel_capture, open_perf, open_rings,
};
use super::perf::{self, Kind};
use super::procfs;
use crate::capture::SymbolSource;
use crate::import::Import;
use crate::profile::{ProfileWriter, Result};

/// Profile a live process without stopping its threads.
pub fn attach(
    pid: u32,
    duration: Option<Duration>,
    options: &CaptureOptions<'_>,
    resolve: impl FnOnce(&Path, &[u8]) -> io::Result<SymbolSource>,
) -> Result<()> {
    let pid_arg = i32::try_from(pid)?;
    if pid_arg == 0 {
        return Err("prof attach requires a positive PID".into());
    }
    let exit = ProcessExit::open(pid_arg)?;
    let cpus = perf::online_cpus()?;
    perf::validate_settings(options.freq, options.stack_bytes)?;
    let threads = procfs::thread_count(pid)?;
    let cpus = if cpus.len().saturating_mul(threads.saturating_add(1)) >= 1_000 {
        &[-1][..]
    } else {
        &cpus
    };
    let profile = ProfileWriter::create(options.out)?;
    let interrupt = Interrupt::register()?;
    let (mut group, events, fallback, kernel) = open_perf(pid, cpus, options, false)?;
    // Keep metadata flowing while the CPU sampling events stay disabled.
    group
        .open_tracking(pid_arg)
        .map_err(|err| perf::open_error(err, perf::TRACKING))?;
    let snapshot_time = perf::monotonic_ns()?;
    let threads = procfs::threads(pid)?;
    let mut open_tids = HashSet::from([pid]);
    let open_tid = |group: &mut perf::PerfGroup,
                    open_tids: &mut HashSet<u32>,
                    tid,
                    sampling_started|
     -> Result<()> {
        if !open_tids.insert(tid) {
            return Ok(());
        }
        let from = group.size();
        let opened = group
            .open_tracking(tid as i32)
            .map_err(|err| (err, perf::TRACKING))
            .and_then(|()| open_rings(group, tid, cpus, &events, options, kernel, false));
        match opened {
            Ok(()) => {
                if sampling_started {
                    group.enable(from)?;
                }
            }
            Err((err, _)) if err.raw_os_error() == Some(libc::ESRCH) => {
                group.truncate(from);
                open_tids.remove(&tid);
            }
            Err((err, event)) => return Err(perf::open_error(err, event).into()),
        }
        Ok(())
    };
    for &(tid, _) in &threads {
        open_tid(&mut group, &mut open_tids, tid, false)?;
    }
    let maps = procfs::mappings(pid)?;
    let proc = procfs::process(pid);
    let (path, mut exe) = procfs::executable(pid)?;
    let provenance = Provenance::load(&path, &mut exe, resolve)?;
    let cmdline = std::fs::read(proc.join("cmdline"))?;
    let cmdline = serde_json::to_string(
        &cmdline
            .strip_suffix(&[0])
            .unwrap_or(&cmdline)
            .split(|&byte| byte == 0)
            .map(String::from_utf8_lossy)
            .collect::<Vec<_>>(),
    )?;
    let cwd = std::fs::read_link(proc.join("cwd")).unwrap_or_else(|_| Path::new(".").to_owned());
    let mut import = Import::new(
        profile.connection(),
        pid,
        cwd,
        provenance.main.clone(),
        provenance.identity,
    );
    import.source_remarks()?;
    let mut snapshot = Some((threads, maps));
    let apply =
        |import: &mut Import<'_>, snapshot: &mut Option<_>, record: perf::Record| -> Result<()> {
            if record.time >= snapshot_time
                && let Some((threads, maps)) = snapshot.take()
            {
                import.initialize(threads, maps, snapshot_time)?;
            }
            import.record(record)
        };
    let start = perf::monotonic_ns()?;
    let instant = Instant::now();
    group.enable(0)?;
    while !exit.ready()?
        && !interrupt.ready()?
        && duration.is_none_or(|elapsed| instant.elapsed() < elapsed)
    {
        for record in group.collect(false)? {
            if record.pid == pid && matches!(record.kind, Kind::Fork { .. }) {
                open_tid(&mut group, &mut open_tids, record.tid, true)?;
            } else if record.pid == pid && record.tid != pid && matches!(record.kind, Kind::Exit) {
                group.remove_tid(record.tid)?;
                open_tids.remove(&record.tid);
            }
            apply(&mut import, &mut snapshot, record)?;
        }
        group.wait()?;
    }
    group.disable()?;
    let end = perf::monotonic_ns()?;
    for record in group.collect(true)? {
        apply(&mut import, &mut snapshot, record)?;
    }
    if let Some((threads, maps)) = snapshot.take() {
        import.initialize(threads, maps, snapshot_time)?;
    }
    import.close(end)?;
    let samples_lost = import.samples_lost;
    drop(import);
    finish(
        profile,
        Capture {
            mode: "attach",
            cmdline,
            start,
            end,
            samples_lost,
            freq: options.freq,
            fallback,
            kernel_capture: Some(kernel_capture(options.kernel, kernel)),
            event: events[0],
            provenance,
        },
    )
}

struct ProcessExit(OwnedFd);

impl ProcessExit {
    fn open(pid: i32) -> io::Result<Self> {
        // SAFETY: pidfd_open with a positive PID and zero flags returns a new fd.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: pidfd_open transferred ownership.
        Ok(Self(unsafe { OwnedFd::from_raw_fd(fd as i32) }))
    }

    fn ready(&self) -> io::Result<bool> {
        readable(&self.0)
    }
}

struct Interrupt {
    fd: OwnedFd,
    saved: libc::sigset_t,
}

impl Interrupt {
    fn register() -> io::Result<Self> {
        // SAFETY: the receiving thread blocks SIGINT before opening the
        // signalfd, so Ctrl-C can end the capture and flush the profile.
        unsafe {
            let mut mask = MaybeUninit::uninit();
            libc::sigemptyset(mask.as_mut_ptr());
            let mut mask = mask.assume_init();
            libc::sigaddset(&mut mask, libc::SIGINT);
            let mut saved = MaybeUninit::uninit();
            let status = libc::pthread_sigmask(libc::SIG_BLOCK, &mask, saved.as_mut_ptr());
            if status != 0 {
                return Err(io::Error::from_raw_os_error(status));
            }
            let saved = saved.assume_init();
            let fd = libc::signalfd(-1, &mask, libc::SFD_NONBLOCK | libc::SFD_CLOEXEC);
            if fd < 0 {
                let error = io::Error::last_os_error();
                libc::pthread_sigmask(libc::SIG_SETMASK, &saved, std::ptr::null_mut());
                return Err(error);
            }
            Ok(Self {
                fd: OwnedFd::from_raw_fd(fd),
                saved,
            })
        }
    }

    fn ready(&self) -> io::Result<bool> {
        if !readable(&self.fd)? {
            return Ok(false);
        }
        let mut signal = MaybeUninit::<libc::signalfd_siginfo>::uninit();
        // SAFETY: signal points to a writable buffer the size of signalfd_siginfo.
        let count = unsafe {
            libc::read(
                self.fd.as_raw_fd(),
                signal.as_mut_ptr().cast(),
                size_of::<libc::signalfd_siginfo>(),
            )
        };
        if count < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(count as usize == size_of::<libc::signalfd_siginfo>())
    }
}

impl Drop for Interrupt {
    fn drop(&mut self) {
        // SAFETY: saved is this thread's previous, initialized signal mask.
        unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, &self.saved, std::ptr::null_mut()) };
    }
}

fn readable(fd: &OwnedFd) -> io::Result<bool> {
    let mut check = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: check is one live pollfd and poll writes only revents.
    let count = unsafe { libc::poll(&mut check, 1, 0) };
    if count < 0 {
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
    Ok(check.revents & (libc::POLLIN | libc::POLLHUP) != 0)
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};

    use super::*;
    use crate::Profile;
    use crate::image::local_symbols;

    #[test]
    fn attach_resolves_stripped_leaf_as_mapping_and_offset() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("stripped.c");
        let executable = dir.path().join("stripped");
        let profile = dir.path().join("stripped.hprof");
        std::fs::write(
            &source,
            r#"
#include <stdio.h>
#include <time.h>

int main(void) {
    struct timespec start, end;
    volatile unsigned long x = 1;
    puts("ready");
    fflush(stdout);
    clock_gettime(CLOCK_THREAD_CPUTIME_ID, &start);
    do {
        for (int i = 0; i < 100000; i++) x = x * 13 + 1;
        clock_gettime(CLOCK_THREAD_CPUTIME_ID, &end);
    } while ((end.tv_sec - start.tv_sec) * 1000000000L + end.tv_nsec - start.tv_nsec < 900000000L);
    return x == 0;
}
"#,
        )
        .unwrap();
        let compiled = Command::new("cc")
            .args(["-O2", "-s", "-fasynchronous-unwind-tables"])
            .arg(&source)
            .arg("-o")
            .arg(&executable)
            .status()
            .unwrap();
        assert!(compiled.success());
        let mut child = Command::new(&executable)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut ready = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut ready)
            .unwrap();
        assert_eq!(ready, "ready\n");
        let result = attach(
            child.id(),
            Some(Duration::from_millis(250)),
            &CaptureOptions {
                out: &profile,
                freq: 997,
                pmu: Some("cpu-clock"),
                kernel: false,
                stack_bytes: None,
            },
            |_, bytes| Ok(local_symbols(bytes)),
        );
        let status = child.wait().unwrap();
        assert!(status.success());
        result.unwrap();

        let profile = Profile::open(&profile).unwrap();
        let db = profile.connection();
        let tier: String = db
            .query_row(
                "SELECT value FROM run_meta WHERE key='symbol_tier'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tier, "offsets_only");
        let mut stmt = db
            .prepare(
                "SELECT s.raw_pc_hex,m.start_addr_hex,f.rel_pc,f.function_id
             FROM cpu_sample s JOIN callsite c ON s.callsite_id=c.id
             JOIN frame f ON f.id=c.frame_id JOIN mapping m ON m.id=f.mapping_id
             WHERE m.path=?1",
            )
            .unwrap();
        let path = executable.to_str().unwrap();
        let mut rows = stmt.query([path]).unwrap();
        let mut application_samples = 0;
        while let Some(row) = rows.next().unwrap() {
            let parse =
                |text: String| u64::from_str_radix(text.trim_start_matches("0x"), 16).unwrap();
            let pc = parse(row.get(0).unwrap());
            let start = parse(row.get(1).unwrap());
            let offset: i64 = row.get(2).unwrap();
            let function: Option<i64> = row.get(3).unwrap();
            assert_eq!(pc - start, offset as u64);
            assert_eq!(function, None);
            application_samples += 1;
        }
        assert!(application_samples > 0, "stripped app executed CPU work");
    }
}
