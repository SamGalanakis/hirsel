//! Durable file replacement.
use std::{
    fs::{self, File},
    io::{self, Write},
    path::Path,
};

/// Replace `path` with `bytes` so that a crash or power loss at any point
/// leaves either the old file or the new one, and once this returns, the new
/// one. The steps follow POSIX: a fsync of the temporary file makes its bytes
/// durable before the rename names them. The rename replaces the entry
/// atomically, and only a fsync of the parent directory makes that entry
/// durable.
///
/// `sync_all` is `fsync` on Linux. On Darwin it is `fcntl(F_FULLFSYNC)` on the
/// file and on the directory, the durability point the runtime's Darwin seam
/// (`hirsel-core-rt` `host/darwin/sync.rs`) uses.
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    // A libc rename, not tempfile's raw syscall, so the dev law faults can cut here.
    let temp = file.into_temp_path();
    fs::rename(&temp, path)?;
    temp.keep()?;
    File::open(parent)?.sync_all()
}
