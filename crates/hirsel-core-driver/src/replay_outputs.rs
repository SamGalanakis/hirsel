//! The client owns output publication, including when the compiler is resident.
use rustix::fs::{self, AtFlags, Dir, Mode as Permissions, OFlags, RenameFlags};
use std::{
    fs::File,
    io::{self, Read},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
};

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub(crate) enum Mode {
    #[default]
    Run,
    Replay(Option<PathBuf>),
}

const CAPTURE: &str = "HIRSEL_CAPTURE_OUTPUTS_V1";
const FILES: &str = "files";

fn open_dir(parent: &File, name: &str) -> io::Result<File> {
    Ok(fs::openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Permissions::empty(),
    )?
    .into())
}

/// Keep the rename source inside a private directory. All operations after
/// opening the parent use descriptors, even if an ancestor is renamed.
struct Stage {
    parent: File,
    name: String,
    private: File,
}

impl Stage {
    fn new(parent: File) -> io::Result<Self> {
        let mut nonce = [0u8; 16];
        File::open("/dev/urandom")?.read_exact(&mut nonce)?;
        let name = format!(".hirsel-replay-{}", hirsel_core_ids::hex::encode(&nonce));
        fs::mkdirat(&parent, name.as_str(), Permissions::RWXU)?;
        let private = open_dir(&parent, &name)?;
        let meta = private.metadata()?;
        // SAFETY: geteuid has no preconditions.
        if meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "output staging directory changed",
            ));
        }
        Ok(Self {
            parent,
            name,
            private,
        })
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        // Only remove the empty staging shell. Captured files are cleaned up
        // relative to their own descriptor before reaching this point.
        let named = fs::statat(&self.parent, self.name.as_str(), AtFlags::SYMLINK_NOFOLLOW);
        if let (Ok(actual), Ok(named)) = (self.private.metadata(), named)
            && actual.dev() == named.st_dev as u64
            && actual.ino() == named.st_ino as u64
        {
            let _ = fs::unlinkat(&self.parent, self.name.as_str(), AtFlags::REMOVEDIR);
        }
    }
}

pub(crate) fn execute(command: &mut Command, destination: Option<&Path>) -> io::Result<ExitStatus> {
    command.env_remove(CAPTURE);
    let Some(destination) = destination else {
        return command.status();
    };
    let leaf = destination.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "--outputs requires a new directory",
        )
    })?;
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent = File::open(parent)?;
    match fs::statat(&parent, leaf, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "--outputs requires a new directory",
            ));
        }
        Err(rustix::io::Errno::NOENT) => {}
        Err(error) => return Err(error.into()),
    }
    let stage = Stage::new(parent)?;
    fs::mkdirat(&stage.private, FILES, Permissions::RWXU)?;
    let files = open_dir(&stage.private, FILES)?;
    let result = (|| -> io::Result<_> {
        // The tape supplies replayed stdin deliveries. Only the exporter uses fd 0.
        let status = command
            .env(CAPTURE, "1")
            .stdin(Stdio::from(files.try_clone()?))
            .status()?;
        if status.success() {
            fs::unlinkat(&files, "complete-v1", AtFlags::empty()).map_err(|error|
                io::Error::other(format!("replay sealed but output export is incomplete or unsupported by this image: {error}")))?;
            fs::renameat_with(
                &stage.private,
                FILES,
                &stage.parent,
                leaf,
                RenameFlags::NOREPLACE,
            )?;
        }
        Ok(status)
    })();
    // A successful rename transferred ownership to the caller. Otherwise clean
    // precisely the flat directory created for this replay.
    if !result.as_ref().is_ok_and(ExitStatus::success) {
        for entry in Dir::read_from(&files)? {
            let entry = entry?;
            let name = entry.file_name();
            if !matches!(name.to_bytes(), b"." | b"..") {
                fs::unlinkat(&files, name, AtFlags::empty())?;
            }
        }
        fs::unlinkat(&stage.private, FILES, AtFlags::REMOVEDIR)?;
    }
    result
}
