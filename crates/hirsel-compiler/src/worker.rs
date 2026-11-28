//! Bounded stdin, stdout and stderr for confined Compiler capability workers.
use crate::{Caps, Job, Limit, MEMORY_STATUS, Refusal, Service, host, unavailable};
use std::io::{BufWriter, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{ChildStdin, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

pub(crate) struct Reply {
    pub(crate) status: ExitStatus,
    pub(crate) bytes: Vec<u8>,
    pub(crate) diagnostics: Vec<u8>,
}

pub(crate) fn run(
    service: &Service,
    arg: &str,
    caps: Caps,
    diagnostics_limit: usize,
    cancelled: &AtomicBool,
    write: impl FnOnce(&mut BufWriter<&mut ChildStdin>) -> std::io::Result<()> + Send,
) -> Result<Reply, Refusal> {
    if cancelled.load(Ordering::Acquire) {
        return Err(unavailable("cancelled"));
    }
    let _seat = service.seat()?;

    // 2. Create private per-job scratch directory (removed on drop; zero bytes
    //    persist on refusal).
    let job = Job(service.scratch.join(format!(
        "worker-{}-{}",
        std::process::id(),
        service.jobs.fetch_add(1, Ordering::Relaxed)
    )));
    let runtime = job.0.join("run");
    let cache = job.0.join("cache");
    let data = job.0.join("data");
    let tmp = job.0.join("tmp");
    let setup = || -> std::io::Result<()> {
        for dir in [&job.0, &runtime, &cache, &data, &tmp] {
            std::fs::create_dir_all(dir)?;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    };
    setup().map_err(unavailable)?;

    // 3. Prepare OS confinement before spawning; fail closed with Unavailable if
    //    confinement or resource caps cannot be enforced on this host.
    let spec = host::ConfinementSpec {
        worker: &service.worker,
        scratch: &job.0,
        cpu_seconds: caps.cpu_seconds,
        memory_bytes: caps.memory_bytes,
        file_bytes: caps.output_bytes.saturating_add(1),
    };
    let confinement = host::Confinement::prepare(&spec).map_err(unavailable)?;

    let mut command = confinement.command(arg);
    command
        .env_clear()
        .env("HIRSEL_MEMORY_LIMIT", caps.memory_bytes.to_string())
        .env("XDG_CACHE_HOME", &cache)
        .env("XDG_DATA_HOME", &data)
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("HOME", &job.0)
        .env("TMPDIR", &tmp)
        .current_dir(&job.0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);

    let deadline = Instant::now() + Duration::from_millis(caps.wall_ms);
    let mut child = command.spawn().map_err(unavailable)?;
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let child_pid = child.id() as libc::pid_t;

    let output_limit = caps.output_bytes as usize;
    let diag_limit = diagnostics_limit;
    let output_overflow = Arc::new(AtomicBool::new(false));
    let overflow_flag = Arc::clone(&output_overflow);

    let (stdout_tx, stdout_rx) = mpsc::channel();
    let (stderr_tx, stderr_rx) = mpsc::channel();

    let (status, reply_bytes, stderr_bytes, overflowed) = std::thread::scope(|scope| {
        let writer = scope.spawn(move || {
            let mut buffered = std::io::BufWriter::with_capacity(64 << 10, &mut stdin);
            let _ = write(&mut buffered).and_then(|()| buffered.flush());
            drop(buffered);
            drop(stdin);
        });
        let out_reader = scope.spawn(move || {
            let mut kept = Vec::new();
            let mut chunk = [0u8; 8192];
            let mut overflowed = false;
            while let Ok(read @ 1..) = stdout.read(&mut chunk) {
                let remaining = output_limit.saturating_sub(kept.len());
                if read > remaining {
                    overflowed = true;
                    overflow_flag.store(true, Ordering::Release);
                    drop(stdout);
                    break;
                }
                kept.extend_from_slice(&chunk[..read]);
            }
            let _ = stdout_tx.send((kept, overflowed));
        });
        let err_reader = scope.spawn(move || {
            let mut kept = Vec::new();
            let mut chunk = [0u8; 4096];
            // Always drain stderr to EOF so the worker never blocks on a full pipe.
            while let Ok(read @ 1..) = stderr.read(&mut chunk) {
                let room = diag_limit.saturating_sub(kept.len());
                kept.extend_from_slice(&chunk[..read.min(room)]);
            }
            let _ = stderr_tx.send(kept);
        });

        let mut status = None;
        while Instant::now() < deadline {
            if cancelled.load(Ordering::Acquire) || output_overflow.load(Ordering::Acquire) {
                // SAFETY: child leads its own process group and is unreaped.
                unsafe { libc::kill(-child_pid, libc::SIGKILL) };
                status = child.wait().ok();
                break;
            }
            match child.try_wait() {
                Ok(Some(exited)) => {
                    status = Some(exited);
                    break;
                }
                Ok(None) => {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(_) => break,
            }
        }
        if status.is_none() {
            // SAFETY: child leads its own process group and is unreaped.
            unsafe { libc::kill(-child_pid, libc::SIGKILL) };
            let _ = child.wait();
        }
        let _ = writer.join();
        let _ = out_reader.join();
        let _ = err_reader.join();
        let (reply_bytes, overflowed) = stdout_rx.recv().unwrap_or_default();
        let stderr_bytes = stderr_rx.recv().unwrap_or_default();
        (status, reply_bytes, stderr_bytes, overflowed)
    });

    if overflowed || output_overflow.load(Ordering::Acquire) {
        return Err(Refusal::Limit(Limit::Output));
    }
    let Some(status) = status else {
        return Err(Refusal::Limit(Limit::Wall));
    };

    match (status.code(), status.signal()) {
        (Some(0), _) => {}
        (Some(MEMORY_STATUS), _) => return Err(Refusal::Limit(Limit::Memory)),
        _ if confinement.memory_limit_hit() => return Err(Refusal::Limit(Limit::Memory)),
        (_, Some(libc::SIGXCPU)) => return Err(Refusal::Limit(Limit::Cpu)),
        (_, Some(libc::SIGXFSZ)) => return Err(Refusal::Limit(Limit::Output)),
        _ => {}
    }
    if cancelled.load(Ordering::Acquire) {
        return Err(unavailable("cancelled"));
    }
    Ok(Reply {
        status,
        bytes: reply_bytes,
        diagnostics: stderr_bytes,
    })
}
