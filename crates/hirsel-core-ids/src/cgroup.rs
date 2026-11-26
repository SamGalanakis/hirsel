//! Resolve cgroup v2 mount and membership data.
use std::{
    io,
    path::{Path, PathBuf},
};

/// The directory of the cgroup a `/proc/<pid>/cgroup` text names, under the cgroup2 mount
/// of a `/proc/self/mountinfo` text whose root holds it: the mount point joined with the
/// cgroup path less that mount's root, so a relocated or namespaced mount resolves. With
/// several such mounts, the one with the longest root, and the first of those. No cgroup v2
/// membership or no cgroup2 mount is `Unsupported`; a membership outside every cgroup2
/// mount's root is `NotFound`.
pub fn resolve(mountinfo: &str, cgroup: &str) -> io::Result<PathBuf> {
    let unsupported = |why| io::Error::new(io::ErrorKind::Unsupported, why);
    let path = cgroup
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .ok_or_else(|| unsupported("no cgroup v2 membership"))?;
    let path = Path::new(path);
    let mut mounts = 0;
    let mut best: Option<(usize, PathBuf)> = None;
    for line in mountinfo.lines() {
        // id parent major:minor root point options [optional...] - fstype source super
        let fields: Vec<&str> = line.split(' ').collect();
        let Some(dash) = fields.iter().skip(6).position(|f| *f == "-") else {
            continue;
        };
        if fields.get(6 + dash + 1) != Some(&"cgroup2") {
            continue;
        }
        mounts += 1;
        let root = unescape(fields[3]);
        let Ok(within) = path.strip_prefix(&root) else {
            continue;
        };
        let depth = root.components().count();
        if best.as_ref().is_none_or(|(best, _)| depth > *best) {
            best = Some((depth, unescape(fields[4]).join(within)));
        }
    }
    match best {
        Some((_, directory)) => Ok(directory),
        None if mounts == 0 => Err(unsupported("no cgroup2 mount")),
        None => Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "cgroup v2 membership {} is outside every cgroup2 mount",
                path.display()
            ),
        )),
    }
}

/// A mountinfo path field, whose space, tab, newline and backslash are octal escapes.
fn unescape(field: &str) -> PathBuf {
    use std::os::unix::ffi::OsStringExt as _;
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let octal = bytes
            .get(i + 1..i + 4)
            .filter(|digits| bytes[i] == b'\\' && digits.iter().all(|d| (b'0'..=b'7').contains(d)))
            .map(|d| (d[0] - b'0') << 6 | (d[1] - b'0') << 3 | (d[2] - b'0'));
        match octal {
            Some(byte) => {
                out.push(byte);
                i += 4;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    PathBuf::from(std::ffi::OsString::from_vec(out))
}
