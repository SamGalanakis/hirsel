//! The `Compiler` service's supervisor (PACK-3).
//!
//! A pack host links this crate and no compiler. [`Service::compile`] runs one compile
//! in a worker process, the `hirsel` tool, under the service's caps, and answers with a
//! checked module or one [`Refusal`]. Whatever the worker does, the caller's process
//! only waits: it holds no worker memory and it stops waiting at the wall deadline.

use hirsel_core_ids::ContentHash;
use hirsel_core_layout::Target;
use hirsel_mod::{Backend, Module};
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, mpsc};
use std::time::{Duration, Instant};

mod host;

#[cfg(test)]
hirsel_guard::install!();
#[cfg(test)]
mod tests;

/// The exit status of a worker whose memory limit refused an allocation
/// (`hirsel_guard::EXIT_STATUS`).
const MEMORY_STATUS: i32 = 71;
/// How a worker's standard error names a release that cannot be a module.
const UNSUPPORTED: &str = "module: unsupported: ";
/// The most of a worker's standard error a refusal carries.
const DIAGNOSTICS: usize = 64 << 10;

/// What one worker may use.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Caps {
    pub cpu_seconds: u64,
    pub memory_bytes: u64,
    pub wall_ms: u64,
    pub input_bytes: u64,
    pub output_bytes: u64,
}

impl Caps {
    /// The caps of `hirsel host`.
    pub const HOST: Self = Self {
        cpu_seconds: 10,
        memory_bytes: 512 << 20,
        wall_ms: 20_000,
        input_bytes: 16 << 20,
        output_bytes: 64 << 20,
    };
}

/// One package's source files, by path relative to the package, and what to build.
#[derive(Clone, Copy, Debug)]
pub struct Request<'a> {
    pub files: &'a [(&'a str, &'a [u8])],
    pub target: Target,
    pub backend: Backend,
    pub policy: u64,
}

/// A module a worker built.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Built {
    pub bytes: Vec<u8>,
    pub digest: ContentHash,
    pub key: ContentHash,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Limit {
    Input,
    Cpu,
    Memory,
    Wall,
    Output,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Refusal {
    /// The source does not check; the worker's diagnostics.
    Rejected(String),
    /// The release cannot be a module.
    Unsupported(String),
    Limit(Limit),
    /// The service's queue is full.
    Busy,
    /// The worker or the service ended for any other reason.
    Unavailable(String),
}

#[derive(Default)]
struct Seats {
    running: usize,
    waiting: usize,
}

pub struct Service {
    worker: PathBuf,
    scratch: PathBuf,
    caps: Caps,
    workers: usize,
    queue: usize,
    seats: Mutex<Seats>,
    freed: Condvar,
    jobs: AtomicU64,
}

/// A running worker's seat, given back when it drops.
struct Seat<'a>(&'a Service);

impl Drop for Seat<'_> {
    fn drop(&mut self) {
        self.0.seats.lock().unwrap().running -= 1;
        self.0.freed.notify_one();
    }
}

/// A job's directory, removed when it drops.
struct Job(PathBuf);

impl Drop for Job {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn unavailable(error: impl std::fmt::Display) -> Refusal {
    Refusal::Unavailable(error.to_string())
}

impl Service {
    /// A service that runs the `hirsel` tool at `worker`, at most `workers` at once with
    /// at most `queue` more waiting, and keeps its files under `scratch`.
    pub fn new(
        worker: PathBuf,
        scratch: PathBuf,
        caps: Caps,
        workers: usize,
        queue: usize,
    ) -> Self {
        Self {
            worker,
            scratch,
            caps,
            workers,
            queue,
            seats: Mutex::default(),
            freed: Condvar::new(),
            jobs: AtomicU64::new(0),
        }
    }

    fn seat(&self) -> Result<Seat<'_>, Refusal> {
        let mut seats = self.seats.lock().unwrap();
        if seats.running >= self.workers {
            if seats.waiting >= self.queue {
                return Err(Refusal::Busy);
            }
            seats.waiting += 1;
            seats = self
                .freed
                .wait_while(seats, |seats| seats.running >= self.workers)
                .unwrap();
            seats.waiting -= 1;
        }
        seats.running += 1;
        Ok(Seat(self))
    }

    pub fn compile(&self, request: &Request<'_>) -> Result<Built, Refusal> {
        let input: u64 = request
            .files
            .iter()
            .map(|(name, bytes)| (name.len() + bytes.len()) as u64)
            .sum();
        if input > self.caps.input_bytes {
            return Err(Refusal::Limit(Limit::Input));
        }
        for (name, _) in request.files {
            let inside = Path::new(name)
                .components()
                .all(|part| matches!(part, Component::Normal(_)));
            if name.is_empty() || !inside {
                return Err(Refusal::Rejected(format!(
                    "source path {name:?} leaves its package"
                )));
            }
        }
        if request.policy != 0 {
            return Err(Refusal::Unsupported(format!(
                "policy bits {:#x}",
                request.policy
            )));
        }
        let _seat = self.seat()?;
        let job = Job(self.scratch.join(format!(
            "job-{}-{}",
            std::process::id(),
            self.jobs.fetch_add(1, Ordering::Relaxed)
        )));
        let (source, runtime, output) = (job.0.join("src"), job.0.join("run"), job.0.join("out"));
        let write = || -> std::io::Result<()> {
            std::fs::create_dir_all(&runtime)?;
            std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700))?;
            for (name, bytes) in request.files {
                let path = source.join(name);
                std::fs::create_dir_all(path.parent().unwrap())?;
                std::fs::write(path, bytes)?;
            }
            Ok(())
        };
        write().map_err(unavailable)?;

        let mut command = Command::new(&self.worker);
        command
            .arg("build")
            .arg(&source)
            .args(["--target", request.target.triple(), "--backend"])
            .arg(match request.backend {
                Backend::Cranelift => "cranelift",
                Backend::Llvm => "llvm",
            })
            .args(["--emit", "module", "-o"])
            .arg(&output)
            .env_clear()
            .env("HIRSEL_MEMORY_LIMIT", self.caps.memory_bytes.to_string())
            // Compiled units are content-addressed, so every job shares them.
            .env("XDG_CACHE_HOME", self.scratch.join("cache"))
            .env("XDG_DATA_HOME", self.scratch.join("data"))
            .env("XDG_RUNTIME_DIR", &runtime)
            .env("HOME", &job.0)
            .current_dir(&job.0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .process_group(0);
        let (cpu, file) = (
            self.caps.cpu_seconds,
            self.caps.output_bytes.saturating_add(1),
        );
        // SAFETY: the closure makes async-signal-safe system calls only.
        unsafe { command.pre_exec(move || host::confine(cpu, file)) };
        let deadline = Instant::now() + Duration::from_millis(self.caps.wall_ms);
        let mut child = command.spawn().map_err(unavailable)?;
        let mut stderr = child.stderr.take().unwrap();
        let (sender, said) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut kept = Vec::new();
            let mut chunk = [0; 4096];
            // Read to the end so the worker never blocks on a full pipe.
            while let Ok(read @ 1..) = stderr.read(&mut chunk) {
                let room = DIAGNOSTICS - kept.len();
                kept.extend_from_slice(&chunk[..read.min(room)]);
            }
            let _ = sender.send(kept);
        });
        // The worker's standard error closes when it and everything it started ends.
        let said = said.recv_timeout(deadline.saturating_duration_since(Instant::now()));
        let mut status = None;
        while said.is_ok() && status.is_none() && Instant::now() < deadline {
            status = child.try_wait().map_err(unavailable)?;
            if status.is_none() {
                std::thread::sleep(Duration::from_micros(100));
            }
        }
        let Some(status) = status else {
            // The worker leads its own process group and is not yet reaped, so the
            // group is still its own.
            // SAFETY: kill takes two integers.
            unsafe { libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL) };
            let _ = child.wait();
            let _ = reader.join();
            return Err(Refusal::Limit(Limit::Wall));
        };
        let _ = reader.join();
        let said = String::from_utf8_lossy(&said.unwrap_or_default()).into_owned();

        let written = std::fs::metadata(&output).map_or(0, |file| file.len());
        if written > self.caps.output_bytes {
            return Err(Refusal::Limit(Limit::Output));
        }
        match (status.code(), status.signal()) {
            (Some(0), _) => {}
            (Some(MEMORY_STATUS), _) => return Err(Refusal::Limit(Limit::Memory)),
            (_, Some(libc::SIGXCPU)) => return Err(Refusal::Limit(Limit::Cpu)),
            (_, Some(libc::SIGXFSZ)) => return Err(Refusal::Limit(Limit::Output)),
            // The tool's own two failures: 1 is a source that does not check, 2 is
            // everything else it reports, of which only this one is about the release.
            (Some(1), _) => return Err(Refusal::Rejected(said)),
            (Some(2), _) => {
                let unsupported = said.lines().find_map(|line| line.strip_prefix(UNSUPPORTED));
                return Err(match unsupported {
                    Some(reason) => Refusal::Unsupported(reason.to_owned()),
                    None => Refusal::Unavailable(said),
                });
            }
            _ => {
                return Err(Refusal::Unavailable(format!(
                    "the worker ended with {status}"
                )));
            }
        }
        let bytes = std::fs::read(&output).map_err(unavailable)?;
        let header = match Module::parse(&bytes) {
            Ok(module) => module.header,
            Err(error) => {
                return Err(Refusal::Unavailable(format!(
                    "the worker's module: {error:?}"
                )));
            }
        };
        if (header.target, header.backend, header.policy, header.abi)
            != (
                request.target,
                request.backend,
                request.policy,
                hirsel_mod::abi(),
            )
        {
            return Err(Refusal::Unavailable(
                "the worker's module is for another request".into(),
            ));
        }
        Ok(Built {
            digest: hirsel_mod::digest(&bytes),
            key: header.key(),
            bytes,
        })
    }
}
