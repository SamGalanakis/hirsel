use std::ffi::{CString, OsString};
use std::fs::{File, Metadata};
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt;
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
    /// The directory the walk opened the file in, and its name there; none for
    /// the walk's root.
    parent: Option<(File, CString)>,
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
        let (parent, name) = self.parent.as_ref().ok_or(WalkError::Unsupported)?;
        let file =
            crate::host::reopen(parent, name, &self.file).map_err(|error| match error.kind() {
                io::ErrorKind::Unsupported => WalkError::Unsupported,
                _ => error.into(),
            })?;
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

fn child(parent: &File, name: &std::ffi::OsStr) -> Result<(File, CString), WalkError> {
    let name = CString::new(name.as_bytes()).map_err(|_| WalkError::Missing)?;
    Ok((crate::host::open_child(parent, &name)?, name))
}

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
    let mut open = vec![(root.try_clone()?, root.metadata()?, None)];
    let mut names = PathBuf::new();
    let mut links = 0;
    while let Some(step) = pending.pop() {
        let (current, metadata, _) = open.last().unwrap();
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
                let (file, component) = child(current, &name)?;
                let candidate = file.metadata()?;
                if candidate.is_symlink() {
                    links += 1;
                    if links > LINKS {
                        return Err(WalkError::LinkLoop);
                    }
                    let target = crate::host::link_target(current, &component, &file)?;
                    steps(&target, &mut pending)?;
                } else {
                    names.push(&name);
                    open.push((file, candidate, Some(component)));
                }
            }
        }
    }
    let (file, metadata, name) = open.pop().unwrap();
    let parent = match (name, open.last()) {
        (Some(name), Some((directory, _, _))) => Some((directory.try_clone()?, name)),
        _ => None,
    };
    Ok(Walked {
        file,
        metadata,
        parent,
        path: names,
    })
}
