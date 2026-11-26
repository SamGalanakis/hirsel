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

use std::fs::{self, DirBuilder};
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};

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
