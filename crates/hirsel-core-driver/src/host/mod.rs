//! The driver's host seam: one Hirsel-owned interface per host mechanism the
//! driver itself uses, with one provider per host OS chosen at compile time.
//! What the driver emits never reads the host (macOS design §3).
#[cfg(target_os = "macos")]
mod darwin;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
pub use darwin::close_inherited_descriptors;
#[cfg(target_os = "macos")]
use darwin::runtime_root;
#[cfg(target_os = "macos")]
pub(crate) use darwin::{spawn as launch, *};
#[cfg(target_os = "linux")]
pub use linux::close_inherited_descriptors;
#[cfg(target_os = "linux")]
use linux::runtime_root;
#[cfg(target_os = "linux")]
pub(crate) use linux::{spawn as launch, *};

use std::ffi::OsStr;
use std::fs::{self, DirBuilder};
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// Resolve a program path, searching the given PATH for a bare name.
pub(crate) fn find_program(program: &Path, search: &OsStr) -> io::Result<PathBuf> {
    if program.components().count() > 1 || program.is_absolute() {
        return fs::canonicalize(program);
    }
    std::env::split_paths(search)
        .map(|directory| directory.join(program))
        .find(|candidate| {
            candidate.is_file()
                && candidate
                    .metadata()
                    .is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
        })
        .map(fs::canonicalize)
        .transpose()?
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("program {} is not on PATH", program.display()),
            )
        })
}

/// The compiler a test's request children run: this test binary on Linux, its
/// installed copy on Darwin, whose default link is the installation's recipe.
#[cfg(test)]
pub(crate) fn test_request_compiler() -> PathBuf {
    let runtime = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(std::env::var_os("HIRSEL_CORE_RT_ARCHIVE").expect("native runtime archive"));
    test_compiler(&runtime)
}

fn check_runtime_directory(path: &Path, uid: libc::uid_t) -> io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if meta.is_dir() && meta.uid() == uid && meta.mode() & 0o077 == 0 {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!("{} is not a directory private to this user", path.display()),
    ))
}

/// Resolve and create a private runtime directory for a Hirsel service.
/// A relative override is ignored so the result never depends on the cwd.
pub(crate) fn runtime_directory(name: &str) -> io::Result<PathBuf> {
    // SAFETY: geteuid does not borrow any memory and cannot fail.
    let uid = unsafe { libc::geteuid() };
    let root = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| runtime_root(uid).filter(|path| check_runtime_directory(path, uid).is_ok()));
    let path = match root {
        Some(root) => root.join(name),
        None => Path::new("/tmp").join(format!("{name}-{uid}")),
    };
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&path)?;
    check_runtime_directory(&path, uid)?;
    Ok(path)
}
