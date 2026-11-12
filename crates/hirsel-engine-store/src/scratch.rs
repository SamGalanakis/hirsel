//! Recovery of process-owned scratch after an exit that bypassed destructors.
use std::{
    fs, io,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

fn start_time(pid: u32) -> io::Result<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    // comm may contain spaces and closing parentheses; field 3 follows its last ')'.
    stat.rsplit_once(')')
        .and_then(|(_, fields)| fields.split_whitespace().nth(19))
        .and_then(|start| start.parse().ok())
        .ok_or_else(|| io::Error::other("invalid process start time"))
}

/// Name scratch with the owner's PID and kernel start time, before creating it.
pub fn path(parent: &Path, prefix: &str, sequence: u64) -> io::Result<PathBuf> {
    let pid = std::process::id();
    Ok(parent.join(format!("{prefix}{pid}-{}-{sequence}", start_time(pid)?)))
}

/// How long a sweep of one parent and prefix answers for. Recovery of a dead
/// owner's scratch is eventual: it happens within this interval of its death,
/// at the next creation under that parent.
pub(crate) const SWEPT_FOR: Duration = Duration::from_secs(600);

/// Best-effort recovery of this user's dead owners. Unknown identities are kept.
/// A `.hirsel-keep` file exempts explicitly retained debugging artifacts.
///
/// A creation pays one `stat` while `<prefix>swept` is younger than
/// [`SWEPT_FOR`]; otherwise it renews the marker and walks the parent.
pub fn sweep(parent: &Path, prefix: &str) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let marker = parent.join(format!("{prefix}swept"));
        let now = SystemTime::now();
        let fresh = fs::metadata(&marker)
            .and_then(|metadata| metadata.modified())
            .is_ok_and(|swept| now.duration_since(swept).is_ok_and(|age| age < SWEPT_FOR));
        if fresh {
            return;
        }
        // Renewed before the walk, so concurrent creations skip it. A marker
        // that cannot be written leaves every creation sweeping, as before.
        let _ = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&marker)
            .and_then(|file| file.set_modified(now));
        let Ok(user) = fs::metadata("/proc/self") else {
            return;
        };
        let Ok(entries) = fs::read_dir(parent) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str().and_then(|name| name.strip_prefix(prefix)) else {
                continue;
            };
            let fields: Vec<_> = name.split('-').collect();
            let [pid, start, sequence] = fields.as_slice() else {
                continue;
            };
            let (Ok(pid), Ok(start), Ok(_)) = (
                pid.parse::<u32>(),
                start.parse::<u64>(),
                sequence.parse::<u64>(),
            ) else {
                continue;
            };
            let Ok(metadata) = fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if !metadata.is_dir()
                || metadata.uid() != user.uid()
                || entry.path().join(".hirsel-keep").exists()
            {
                continue;
            }
            let dead = match start_time(pid) {
                Ok(actual) => actual != start,
                Err(error) => error.kind() == io::ErrorKind::NotFound,
            };
            if dead {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    }
}
