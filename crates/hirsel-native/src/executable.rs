//! Standalone executable compilation; launch authority is supplied by its caller at runtime.
use super::{NativeError, OptLevel, Options, StageName, TargetSpec};
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
        }
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
pub struct Scratch(pub PathBuf, bool);
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
        loop {
            let path = parent.join(format!(
                "hirsel-build-{}-{}",
                std::process::id(),
                next.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => {
                    if keep {
                        eprintln!("hirsel: retaining native scratch at {}", path.display());
                    }
                    return Ok(Self(path, keep));
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(io(error)),
            }
        }
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        if !self.1 {
            let _ = std::fs::remove_dir_all(&self.0);
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
}
