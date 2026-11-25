use std::ffi::{CString, OsString};
use std::fs::{File, Metadata};
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct FileId(u64, u64);

impl FileId {
    pub(crate) fn of(metadata: &Metadata) -> Self {
        Self(metadata.dev(), metadata.ino())
    }
}

#[derive(Debug)]
pub(crate) enum WalkError {
    Outside,
    Missing,
    WrongType,
    LinkLoop,
    Unsupported,
    Io(io::Error),
}

impl From<io::Error> for WalkError {
    fn from(error: io::Error) -> Self {
        match error.kind() {
            io::ErrorKind::NotFound => Self::Missing,
            io::ErrorKind::NotADirectory => Self::WrongType,
            io::ErrorKind::Unsupported => Self::Unsupported,
            _ => Self::Io(error),
        }
    }
}

impl WalkError {
    pub(crate) fn explain(&self, boundary: &str, object: &str) -> String {
        match self {
            Self::Outside => format!("resolves outside the {boundary}"),
            Self::Missing => "does not exist".into(),
            Self::WrongType => format!("does not resolve to {object}"),
            Self::LinkLoop => "meets a symbolic link loop".into(),
            Self::Unsupported => {
                format!("cannot be opened relative to the {boundary} directory on this host")
            }
            Self::Io(error) => error.to_string(),
        }
    }
}

pub(crate) fn open_directory(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    File::options()
        .read(true)
        .custom_flags(libc::O_DIRECTORY)
        .open(path)
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct Captured {
    pub(crate) bytes: Arc<[u8]>,
    pub(crate) identity: FileId,
}

pub(crate) struct Walked {
    file: File,
    metadata: Metadata,
    pub(crate) path: PathBuf,
}

impl Walked {
    pub(crate) fn directory(self) -> Result<File, WalkError> {
        if !self.metadata.is_dir() {
            return Err(WalkError::WrongType);
        }
        Ok(self.file)
    }

    pub(crate) fn reader(self) -> Result<(File, FileId), WalkError> {
        if !self.metadata.is_file() {
            return Err(WalkError::WrongType);
        }
        let identity = FileId::of(&self.metadata);
        #[cfg(target_os = "linux")]
        let file =
            File::open(format!("/proc/self/fd/{}", self.file.as_raw_fd())).map_err(|error| {
                match error.kind() {
                    io::ErrorKind::NotFound => WalkError::Unsupported,
                    _ => error.into(),
                }
            })?;
        #[cfg(not(target_os = "linux"))]
        let file: File = return Err(WalkError::Unsupported);
        if FileId::of(&file.metadata()?) != identity {
            return Err(WalkError::Unsupported);
        }
        Ok((file, identity))
    }

    pub(crate) fn read_vec(self, limit: usize) -> Result<(Vec<u8>, FileId), WalkError> {
        let (file, identity) = self.reader()?;
        let mut bytes = Vec::new();
        file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > limit {
            return Err(WalkError::Io(io::Error::other(format!(
                "over the limit of {limit} bytes"
            ))));
        }
        Ok((bytes, identity))
    }

    pub(crate) fn read(self, limit: usize) -> Result<Captured, WalkError> {
        let (bytes, identity) = self.read_vec(limit)?;
        Ok(Captured {
            bytes: bytes.into(),
            identity,
        })
    }
}

#[cfg(target_os = "linux")]
fn child(parent: &File, name: &std::ffi::OsStr) -> Result<File, WalkError> {
    let name = CString::new(name.as_bytes()).map_err(|_| WalkError::Missing)?;
    // SAFETY: the descriptor and the NUL-terminated component live through openat.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error().into());
    }
    // SAFETY: openat returned a new owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(target_os = "linux")]
fn link_target(link: &File) -> Result<PathBuf, WalkError> {
    let mut bytes = vec![0u8; 256];
    loop {
        // SAFETY: an empty name reads the opened link itself. The output buffer
        // is writable for the supplied length and the descriptor stays open.
        let n = unsafe {
            libc::readlinkat(
                link.as_raw_fd(),
                c"".as_ptr(),
                bytes.as_mut_ptr().cast(),
                bytes.len(),
            )
        };
        if n < 0 {
            return Err(io::Error::last_os_error().into());
        }
        let n = n as usize;
        if n < bytes.len() {
            bytes.truncate(n);
            return Ok(OsString::from_vec(bytes).into());
        }
        bytes.resize(bytes.len() * 2, 0);
    }
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn walk(_root: &File, _path: &Path) -> Result<Walked, WalkError> {
    Err(WalkError::Unsupported)
}

#[cfg(target_os = "linux")]
pub(crate) fn walk(root: &File, path: &Path) -> Result<Walked, WalkError> {
    /// Section 1's bound: at most 40 links are followed on one path.
    const LINKS: usize = 40;

    enum Step {
        Name(OsString),
        Parent,
        Directory,
    }

    fn steps(path: &Path, pending: &mut Vec<Step>) -> Result<(), WalkError> {
        if path.as_os_str().as_bytes().ends_with(b"/")
            || path.as_os_str().as_bytes().ends_with(b"/.")
        {
            pending.push(Step::Directory);
        }
        for component in path.components().rev() {
            pending.push(match component {
                Component::CurDir => Step::Directory,
                Component::ParentDir => Step::Parent,
                Component::Normal(name) => Step::Name(name.to_owned()),
                Component::Prefix(_) | Component::RootDir => return Err(WalkError::Outside),
            });
        }
        Ok(())
    }

    let mut pending = Vec::new();
    steps(path, &mut pending)?;
    let mut open = vec![(root.try_clone()?, root.metadata()?)];
    let mut names = PathBuf::new();
    let mut links = 0;
    while let Some(step) = pending.pop() {
        let (current, metadata) = open.last().unwrap();
        if !metadata.is_dir() {
            return Err(WalkError::WrongType);
        }
        match step {
            Step::Directory => {}
            Step::Parent => {
                if open.len() == 1 {
                    return Err(WalkError::Outside);
                }
                open.pop();
                names.pop();
            }
            Step::Name(name) => {
                let file = child(current, &name)?;
                let candidate = file.metadata()?;
                if candidate.is_symlink() {
                    links += 1;
                    if links > LINKS {
                        return Err(WalkError::LinkLoop);
                    }
                    steps(&link_target(&file)?, &mut pending)?;
                } else {
                    names.push(&name);
                    open.push((file, candidate));
                }
            }
        }
    }
    let (file, metadata) = open.pop().unwrap();
    Ok(Walked {
        file,
        metadata,
        path: names,
    })
}
