//! Standalone executable compilation; launch authority is supplied by its caller at runtime.
use super::{NativeError, OptLevel, Options, StageName, TargetSpec};
use crate::object_queries::platform::record::CaptureMode;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Every compiler choice of one native build: the Ready pass matrix, the
/// backend, its optimization and object partitioning, and source/debug tables.
/// A native request carries exactly this value; `BuildProfile` is only a preset.
#[derive(Clone, Debug)]
pub struct BuildOptions {
    /// Target, validation, launch inputs and the Ready passes.
    pub passes: Options,
    pub backend: super::Backend,
    pub opt: OptLevel,
    pub partition: super::Partition,
    /// Source line tables, intervals and the debug runtime archive. Independent
    /// of `opt` and of the passes' specialization policy.
    pub debug_tables: bool,
    /// Minimum platform verification required by this build, independent of codegen.
    pub platform_capture: CaptureMode,
}
impl BuildOptions {
    /// The Ready matrix `passes` under `backend`'s ordinary partition, without
    /// source/debug tables.
    pub fn new(passes: Options, backend: super::Backend, opt: OptLevel) -> Self {
        Self {
            passes,
            backend,
            opt,
            partition: match backend {
                super::Backend::Llvm => super::Partition::Whole,
                super::Backend::Cranelift => super::Partition::Singleton,
            },
            debug_tables: false,
            platform_capture: CaptureMode::Record,
        }
    }
    /// The build's platform policy, with the environment's forced-cold override.
    pub fn capture_mode(&self) -> Result<CaptureMode, String> {
        Ok(match CaptureMode::from_environment()? {
            CaptureMode::Cold => CaptureMode::Cold,
            CaptureMode::Record => self.platform_capture,
        })
    }
}

/// The user-facing executable profiles; runtime archives always use production settings.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum BuildProfile {
    #[default]
    Default,
    Release,
    /// LLVM release optimization with source tables and frame pointers.
    Profiling,
    /// Explicit whole-module LLVM optimization (`--lto`).
    ReleaseLto,
    Debug,
}
impl BuildProfile {
    /// This profile's complete build options for `backend` at `target`.
    pub fn options(
        self,
        backend: super::Backend,
        target: TargetSpec,
        validation: super::Validation,
    ) -> Result<BuildOptions, NativeError> {
        if matches!(self, Self::Release | Self::ReleaseLto | Self::Profiling)
            && backend == super::Backend::Cranelift
        {
            return Err(NativeError::new(
                StageName::Emission,
                "release builds use LLVM; select LLVM or a non-release profile",
            ));
        }
        let mut passes = Options::new(target);
        passes.validation = validation;
        passes.scoped_specialization =
            matches!(self, Self::Release | Self::ReleaseLto | Self::Profiling);
        let opt = match self {
            Self::Default | Self::Release | Self::ReleaseLto | Self::Profiling => OptLevel::Default,
            Self::Debug => OptLevel::None,
        };
        Ok(BuildOptions {
            debug_tables: matches!(self, Self::Debug | Self::Profiling),
            platform_capture: if matches!(self, Self::Release | Self::ReleaseLto | Self::Profiling)
            {
                CaptureMode::Cold
            } else {
                CaptureMode::Record
            },
            ..BuildOptions::new(passes, backend, opt)
        })
    }
    /// The interactive `run --native` preset: the default profile, except that
    /// Cranelift compiles unoptimized.
    pub fn run_options(
        backend: super::Backend,
        target: TargetSpec,
        validation: super::Validation,
    ) -> BuildOptions {
        let mut options = Self::Default
            .options(backend, target, validation)
            .unwrap_or_else(|_| unreachable!("the default profile admits every backend"));
        if backend == super::Backend::Cranelift {
            options.opt = OptLevel::None;
        }
        options
    }
}

/// One build owns its directory from exclusive creation until drop.
pub struct Scratch(pub PathBuf, Option<std::process::Child>);

// The cleanup process creates the directory before reporting readiness, so
// even death during construction leaves no unregistered directory. A separate
// process group survives bench drivers killing the compiler's whole group.
// Only its creator removes a directory; an occupied candidate is never armed.
const CLEANUP: &str = r#"
trap '' PIPE
if ! /bin/mkdir -m 700 -- "$1"; then
    if [ -e "$1" ] || [ -L "$1" ]; then exit 73; fi
    exit 74
fi
trap '/bin/rm -rf -- "$1" || exit 1' EXIT
printf 'ready\n'
read -r ignored || :
"#;

fn cleanup_process(path: &Path) -> std::io::Result<std::process::Child> {
    use std::io::{BufRead, BufReader};
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let mut child = Command::new("/bin/sh")
        .args(["-c", CLEANUP, "hirsel-scratch-cleanup"])
        .arg(path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()?;
    let mut ready = String::new();
    let received = BufReader::new(child.stdout.take().unwrap()).read_line(&mut ready);
    if received.is_ok() && ready == "ready\n" {
        return Ok(child);
    }
    // Close the ownership pipe on every failed construction as well.
    drop(child.stdin.take());
    let status = child.wait()?;
    Err(match received {
        Err(error) => error,
        Ok(_) if status.code() == Some(73) => std::io::ErrorKind::AlreadyExists.into(),
        Ok(_) => std::io::Error::other(format!(
            "could not create native scratch {}: {status}",
            path.display()
        )),
    })
}
impl Scratch {
    pub fn new() -> Result<Self, NativeError> {
        Self::create_in(
            &std::env::temp_dir(),
            std::env::var_os("HIRSEL_KEEP_NATIVE_SCRATCH").is_some(),
        )
    }

    fn create_in(parent: &Path, keep: bool) -> Result<Self, NativeError> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Self::create_with_counter(parent, keep, &NEXT)
    }

    fn create_with_counter(
        parent: &Path,
        keep: bool,
        next: &AtomicU64,
    ) -> Result<Self, NativeError> {
        use std::os::unix::fs::DirBuilderExt;
        hirsel_engine_store::scratch::sweep(parent, "hirsel-build-");
        loop {
            let path = hirsel_engine_store::scratch::path(
                parent,
                "hirsel-build-",
                next.fetch_add(1, Ordering::Relaxed),
            )
            .map_err(io)?;
            let created = if keep {
                std::fs::DirBuilder::new()
                    .mode(0o700)
                    .create(&path)
                    .map(|()| None)
            } else {
                cleanup_process(&path).map(Some)
            };
            match created {
                Ok(cleanup) => {
                    if keep {
                        let marker = path.join(".hirsel-keep");
                        if let Err(error) = std::fs::write(marker, b"") {
                            let _ = std::fs::remove_dir_all(&path);
                            return Err(io(error));
                        }
                        eprintln!("hirsel: retaining native scratch at {}", path.display());
                    }
                    return Ok(Self(path, cleanup));
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(io(error)),
            }
        }
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        if let Some(mut cleanup) = self.1.take() {
            drop(cleanup.stdin.take());
            if let Err(error) = cleanup.wait().and_then(|status| {
                if status.success() {
                    Ok(())
                } else {
                    Err(std::io::Error::other(status.to_string()))
                }
            }) {
                use std::io::Write;
                let _ = writeln!(
                    std::io::stderr(),
                    "hirsel: native scratch cleanup failed at {}: {error}",
                    self.0.display()
                );
            }
        }
    }
}
fn io(error: std::io::Error) -> NativeError {
    NativeError::new(StageName::Link, error.to_string())
}

#[cfg(test)]
mod scratch_tests {
    use super::*;

    #[test]
    fn creation_sweeps_dead_owners_but_keeps_live_and_retained_builds() {
        let parent = tempfile::tempdir().unwrap();
        let dead = parent.path().join("hirsel-build-4294967295-0-0");
        let reused = parent.path().join(format!(
            "hirsel-build-{}-18446744073709551615-0",
            std::process::id()
        ));
        let retained = parent.path().join("hirsel-build-4294967295-0-1");
        for path in [&dead, &reused, &retained] {
            std::fs::create_dir(path).unwrap();
        }
        std::fs::write(retained.join(".hirsel-keep"), b"").unwrap();
        let live = Scratch::create_in(parent.path(), false).unwrap();
        let _next = Scratch::create_in(parent.path(), false).unwrap();
        assert!(!dead.exists());
        assert!(!reused.exists());
        assert!(retained.is_dir());
        assert!(live.0.is_dir());
    }

    #[test]
    fn exclusive_creation_skips_existing_names_and_drop_cleans_up() {
        let parent = Scratch::new().unwrap();
        let next_id = AtomicU64::new(0);
        let first = Scratch::create_with_counter(&parent.0, true, &next_id).unwrap();
        let occupied = first.0.clone();
        std::fs::write(occupied.join("sentinel"), b"another build").unwrap();
        drop(first);
        next_id.store(0, Ordering::Relaxed);
        let next = Scratch::create_with_counter(&parent.0, false, &next_id).unwrap();
        let path = next.0.clone();
        assert_ne!(occupied, path);
        drop(next);
        assert!(!path.exists());
        assert_eq!(
            std::fs::read(occupied.join("sentinel")).unwrap(),
            b"another build"
        );
    }

    #[test]
    fn owner_death_before_readiness_removes_scratch() {
        use std::os::fd::OwnedFd;
        use std::os::unix::net::UnixStream;
        use std::process::{Command, Stdio};
        let parent = tempfile::tempdir().unwrap();
        let path = parent.path().join("scratch");
        // No reader exists even before creation: readiness gets EPIPE, exactly
        // as when the owner dies while Scratch::new is still starting up.
        let (reader, writer) = UnixStream::pair().unwrap();
        drop(reader);
        let status = Command::new("/bin/sh")
            .args(["-c", CLEANUP, "hirsel-scratch-cleanup"])
            .arg(&path)
            .stdin(Stdio::null())
            .stdout(Stdio::from(OwnedFd::from(writer)))
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
        assert!(!path.exists());
    }

    #[test]
    fn scratch_process_child() {
        let Some(parent) = std::env::var_os("HIRSEL_SCRATCH_TEST_PARENT") else {
            return;
        };
        let mode = std::env::var("HIRSEL_SCRATCH_TEST_MODE").unwrap();
        let operation = || -> std::io::Result<()> {
            let scratch = Scratch::create_in(Path::new(&parent), mode == "keep").unwrap();
            for name in ["observation", "program", "root", "run.json"] {
                std::fs::write(scratch.0.join(name), b"owned build artifact")?;
            }
            println!("SCRATCH {}", scratch.0.display());
            use std::io::Write;
            std::io::stdout().flush()?;
            match mode.as_str() {
                "exit" => std::process::exit(42),
                "signal" => {
                    let mut byte = [0];
                    use std::io::Read;
                    let _ = std::io::stdin().read(&mut byte);
                }
                "error" => return Err(std::io::Error::other("build failed")),
                "panic" => panic!("build unwound"),
                _ => {}
            }
            Ok(())
        };
        assert_eq!(operation().is_err(), mode == "error");
    }

    fn process_exit(mode: &str, signal: Option<i32>) {
        use std::io::{BufRead, BufReader, Read};
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};
        let parent = tempfile::tempdir().unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "executable::scratch_tests::scratch_process_child",
                "--nocapture",
            ])
            .env("HIRSEL_SCRATCH_TEST_PARENT", parent.path())
            .env("HIRSEL_SCRATCH_TEST_MODE", mode)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        let mut output = BufReader::new(child.stdout.take().unwrap());
        let path = output
            .by_ref()
            .lines()
            .map(Result::unwrap)
            .find_map(|line| {
                line.split_once("SCRATCH ")
                    .map(|(_, path)| PathBuf::from(path))
            })
            .expect("child created its scratch directory");
        if let Some(signal) = signal {
            let comm = Command::new("ps")
                .args(["-o", "comm=", "-p", &child.id().to_string()])
                .output()
                .unwrap();
            assert!(comm.status.success());
            let name = String::from_utf8(comm.stdout).unwrap();
            assert!(
                ![
                    "systemd",
                    "dbus-daemon",
                    "sshd",
                    "tmux",
                    "t3",
                    "orca-ide",
                    "claude",
                    "codex"
                ]
                .contains(&name.trim())
            );
            // SAFETY: this is the process group of the child started above,
            // never a discovered PID or a parent process.
            assert_eq!(unsafe { libc::kill(-(child.id() as i32), signal) }, 0);
        }
        let status = child.wait().unwrap();
        match mode {
            "exit" => assert_eq!(status.code(), Some(42)),
            "panic" => assert!(!status.success()),
            "signal" => {
                use std::os::unix::process::ExitStatusExt;
                assert_eq!(status.signal(), signal);
            }
            _ => assert!(status.success()),
        }
        if mode == "keep" {
            assert!(path.join("program").is_file());
        } else {
            // A surviving cleanup process can finish after its owner is reaped.
            for _ in 0..100 {
                if !path.exists() {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            assert!(!path.exists(), "{mode} left {}", path.display());
        }
    }

    #[test]
    fn normal_return_removes_scratch() {
        process_exit("return", None);
    }
    #[test]
    fn error_return_removes_scratch() {
        process_exit("error", None);
    }
    #[test]
    fn panic_unwind_removes_scratch() {
        process_exit("panic", None);
    }
    #[test]
    fn immediate_process_exit_removes_scratch() {
        process_exit("exit", None);
    }
    #[test]
    fn sigterm_removes_scratch() {
        process_exit("signal", Some(libc::SIGTERM));
    }
    #[test]
    fn bench_sigkill_removes_scratch() {
        process_exit("signal", Some(libc::SIGKILL));
    }
    #[test]
    fn explicit_retention_keeps_scratch() {
        process_exit("keep", None);
    }
}
