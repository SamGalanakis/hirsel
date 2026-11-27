//! Native Exec admission. This module verifies the pinned executable images
//! in place before main and re-checks them before each spawn (EXEC-4).
use crate::exec::{Policy, Profile};
use crate::host::image as host;
use ring::digest::{Context, SHA256};
use std::collections::BTreeMap;
use std::ffi::CStr;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::Arc;

pub(crate) fn sha256(file: &mut File) -> io::Result<[u8; 32]> {
    let mut digest = Context::new(&SHA256);
    let mut bytes = [0; 16384];
    loop {
        let len = match file.read(&mut bytes) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            read => read?,
        };
        if len == 0 {
            break;
        }
        digest.update(&bytes[..len]);
    }
    Ok(digest.finish().as_ref().try_into().unwrap())
}

fn open_nofollow(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY)
        .open(path)
}

/// Metadata snapshot of a verified executable image on disk.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Stamp {
    dev: libc::dev_t,
    ino: libc::ino_t,
    size: libc::off_t,
    mtime_sec: libc::time_t,
    mtime_nsec: libc::c_long,
    ctime_sec: libc::time_t,
    ctime_nsec: libc::c_long,
}

impl Stamp {
    fn from_stat(st: &libc::stat) -> io::Result<Self> {
        let kind = st.st_mode & libc::S_IFMT;
        if kind == libc::S_IFLNK {
            return Err(io::Error::from_raw_os_error(libc::ELOOP));
        }
        if kind != libc::S_IFREG {
            return Err(io::Error::from_raw_os_error(libc::EACCES));
        }
        Ok(Self {
            dev: st.st_dev,
            ino: st.st_ino,
            size: st.st_size,
            mtime_sec: st.st_mtime,
            mtime_nsec: st.st_mtime_nsec,
            ctime_sec: st.st_ctime,
            ctime_nsec: st.st_ctime_nsec,
        })
    }

    fn of_fd(fd: RawFd) -> io::Result<Self> {
        let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: fd is live and fstat writes st on success.
        if unsafe { libc::fstat(fd, st.as_mut_ptr()) } < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fstat succeeded.
        Self::from_stat(&unsafe { st.assume_init() })
    }

    fn of_path(path: &CStr) -> io::Result<Self> {
        let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: path is a live NUL-terminated path and lstat writes st on success.
        if unsafe { libc::lstat(path.as_ptr(), st.as_mut_ptr()) } < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: lstat succeeded.
        Self::from_stat(&unsafe { st.assume_init() })
    }
}

/// Pre-spawn verifier for a pinned executable image (EXEC-4).
#[derive(Clone, Copy, Debug)]
pub struct ImagePin {
    stamp: Stamp,
    pin: [u8; 32],
}

impl ImagePin {
    pub const ENCODED: usize = 7 * 8 + 32;

    /// The pin's bytes, for the embedded supervisor helper that checks it
    /// before each spawn in its own process.
    pub fn encode(&self) -> [u8; Self::ENCODED] {
        let s = &self.stamp;
        let mut out = [0; Self::ENCODED];
        let fields = [
            s.dev as u64,
            s.ino as u64,
            s.size as u64,
            s.mtime_sec as u64,
            s.mtime_nsec as u64,
            s.ctime_sec as u64,
            s.ctime_nsec as u64,
        ];
        for (at, field) in fields.into_iter().enumerate() {
            out[at * 8..at * 8 + 8].copy_from_slice(&field.to_le_bytes());
        }
        out[56..].copy_from_slice(&self.pin);
        out
    }

    pub fn decode(bytes: &[u8; Self::ENCODED]) -> Self {
        let field = |at: usize| u64::from_le_bytes(bytes[at * 8..at * 8 + 8].try_into().unwrap());
        Self {
            stamp: Stamp {
                dev: field(0) as libc::dev_t,
                ino: field(1) as libc::ino_t,
                size: field(2) as libc::off_t,
                mtime_sec: field(3) as libc::time_t,
                mtime_nsec: field(4) as libc::c_long,
                ctime_sec: field(5) as libc::time_t,
                ctime_nsec: field(6) as libc::c_long,
            },
            pin: bytes[56..].try_into().unwrap(),
        }
    }

    /// Re-check `(dev, ino, size, mtime, ctime)` at `path`. Refuses a symlink
    /// or non-regular file; when any stamp field changed, re-hashes the
    /// regular file (without following links) and refuses if its digest
    /// differs from the pin.
    pub fn verify(&self, path: &CStr) -> io::Result<()> {
        if Stamp::of_path(path)? == self.stamp {
            return Ok(());
        }
        let flags =
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK | libc::O_NOCTTY;
        // SAFETY: path is a live NUL-terminated path; a success transfers a descriptor.
        let fd = unsafe { libc::open(path.as_ptr(), flags) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: open returned an owned descriptor.
        let mut file = unsafe { File::from_raw_fd(fd) };
        let opened = Stamp::of_fd(file.as_raw_fd())?;
        if sha256(&mut file)? != self.pin || Stamp::of_fd(file.as_raw_fd())? != opened {
            return Err(io::Error::from_raw_os_error(libc::EACCES));
        }
        Ok(())
    }
}

pub struct PinnedImage {
    pub path: Arc<CStr>,
    pub pin: ImagePin,
    pub interpreter: Option<Arc<CStr>>,
}

pub struct PinnedProfile {
    pub spec: Profile,
    pub image: PinnedImage,
}

impl PinnedImage {
    pub fn open(profile: &Profile) -> Result<Self, String> {
        let step = |step, error| format!("{step}: {error}");
        let mut file =
            open_nofollow(Path::new(&profile.image)).map_err(|e| step("open image", e))?;
        if !file
            .metadata()
            .map_err(|e| step("stat image", e))?
            .is_file()
        {
            return Err("image is not a regular file".into());
        }
        let stamp = Stamp::of_fd(file.as_raw_fd()).map_err(|e| step("stat image", e))?;
        let digest = sha256(&mut file).map_err(|e| step("read image", e))?;
        if digest != profile.pin {
            return Err("image SHA-256 does not match the pin".into());
        }
        file.rewind().map_err(|e| step("rewind image", e))?;
        let interpreter = host::interpreter(&mut file).map_err(|e| step("parse image", e))?;
        let path = host::real_path(&file, &profile.image).map_err(|e| step("resolve image", e))?;
        if Stamp::of_fd(file.as_raw_fd()).map_err(|e| step("verify image", e))? != stamp
            || Stamp::of_path(&path).map_err(|e| step("verify image", e))? != stamp
        {
            return Err("image changed during verification".into());
        }
        Ok(Self {
            path: Arc::from(path.into_boxed_c_str()),
            pin: ImagePin {
                stamp,
                pin: profile.pin,
            },
            interpreter,
        })
    }
}

impl Policy {
    pub fn pin_images(self) -> Result<(BTreeMap<String, PinnedProfile>, Vec<String>), String> {
        let mut profiles = BTreeMap::new();
        let mut lines = Vec::new();
        for (name, spec) in self.profiles {
            let image =
                PinnedImage::open(&spec).map_err(|cause| format!("Exec[{name:?}]: {cause}"))?;
            let field = |s: &str| s.escape_default().to_string();
            lines.push(format!(
                "exec: {}: image {}, sha256 {}, interpreter {}, policy {}\n",
                field(&name),
                field(&spec.image),
                hirsel_core_ids::hex::encode(&spec.pin),
                image.interpreter.as_deref().map_or_else(
                    || "none".to_owned(),
                    |path| path
                        .to_bytes()
                        .iter()
                        .flat_map(|&byte| std::ascii::escape_default(byte))
                        .map(char::from)
                        .collect(),
                ),
                hirsel_core_ids::hex::encode(&self.digest),
            ));
            profiles.insert(name, PinnedProfile { spec, image });
        }
        Ok((profiles, lines))
    }
}
