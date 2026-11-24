//! The objects a link hands the linker, as one archive of their checked
//! bytes: a sealed memory file the linker reads whole through one descriptor
//! of this process, however many objects the link has. A link that patches
//! its last executable (`incremental_link`) reads the members it changes
//! and never makes the archive.
#[cfg(test)]
use crate::native::{AFTER_CHECK, BEFORE_OPEN};
use crate::native::{NativeStop, ObjectProduct};
use hirsel_core_ir::Identity;
use std::fs::File;
use std::io::{self, Write};
use std::os::unix::fs::FileExt;
use std::path::PathBuf;
use std::sync::Arc;

/// A link's objects, in link order, as the members of a GNU archive without
/// a symbol table, in a sealed memory file. The linker reads it under
/// `--whole-archive` (`native_link::direct_link`), which loads every member
/// in order as if each were named alone, so the executable is the one a link
/// of the separate objects makes. Only checked bytes are written into it, and
/// once sealed nothing changes them.
pub(crate) struct Bundle {
    file: File,
    /// Each member's bytes: offset and length.
    members: Vec<(u64, usize)>,
    /// macOS has no sealed memory files: a private directory holds the
    /// bundle, made user-immutable, and is removed with it.
    #[cfg(target_os = "macos")]
    dir: PathBuf,
}

#[cfg(target_os = "macos")]
impl Drop for Bundle {
    fn drop(&mut self) {
        // SAFETY: a live descriptor and integer flags; clearing the
        // immutable flag lets the directory be removed.
        unsafe { libc::fchflags(std::os::fd::AsRawFd::as_raw_fd(&self.file), 0) };
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

const MAGIC: &[u8; 8] = b"!<arch>\n";

impl Bundle {
    /// The path through which another process reads the bundle.
    pub(crate) fn path(&self) -> PathBuf {
        #[cfg(target_os = "macos")]
        return self.dir.join(BUNDLE_NAME);
        #[cfg(target_os = "linux")]
        PathBuf::from(format!(
            "/proc/{}/fd/{}",
            std::process::id(),
            std::os::fd::AsRawFd::as_raw_fd(&self.file)
        ))
    }
    /// The bundle of `members`, in order.
    pub(crate) fn of(members: &[Arc<[u8]>]) -> io::Result<Self> {
        let mut bundle = BundleWriter::new()?;
        for member in members {
            bundle.push(member)?;
        }
        bundle.finish()
    }
    /// Member `n`'s bytes.
    pub(crate) fn member(&self, n: usize) -> io::Result<Vec<u8>> {
        let (at, len) = self.members[n];
        let mut bytes = vec![0; len];
        self.file.read_exact_at(&mut bytes, at)?;
        Ok(bytes)
    }
}

/// A link's members, bundled for the linker on first use: a link that
/// patches its last executable reads them and never makes the bundle. The
/// bundle is the link's second input, after the entry object.
pub(crate) struct Members {
    members: Vec<Arc<[u8]>>,
    bundle: std::cell::OnceCell<Bundle>,
}

impl Members {
    pub(crate) fn new(members: Vec<Arc<[u8]>>) -> Self {
        Self {
            members,
            bundle: std::cell::OnceCell::new(),
        }
    }
    pub(crate) fn bundle(&self) -> io::Result<&Bundle> {
        if self.bundle.get().is_none() {
            let _ = self.bundle.set(Bundle::of(&self.members)?);
        }
        Ok(self.bundle.get().expect("set above"))
    }
}

impl std::ops::Deref for Members {
    type Target = [Arc<[u8]>];
    fn deref(&self) -> &Self::Target {
        &self.members
    }
}

/// A [`Bundle`] being written.
pub(crate) struct BundleWriter {
    file: io::BufWriter<File>,
    at: u64,
    members: Vec<(u64, usize)>,
    #[cfg(target_os = "macos")]
    dir: PathBuf,
}

impl BundleWriter {
    pub(crate) fn new() -> io::Result<Self> {
        #[cfg(target_os = "macos")]
        let (file, dir) = memory_file()?;
        #[cfg(target_os = "linux")]
        let file = memory_file()?;
        let mut file = io::BufWriter::with_capacity(1 << 20, file);
        file.write_all(MAGIC)?;
        Ok(Self {
            file,
            at: MAGIC.len() as u64,
            members: Vec::new(),
            #[cfg(target_os = "macos")]
            dir,
        })
    }
    /// Append checked `bytes` as the next member.
    pub(crate) fn push(&mut self, bytes: &[u8]) -> io::Result<()> {
        // The member's name is its index; `/` ends a GNU member name.
        #[cfg(target_os = "linux")]
        let (name, size) = (format!("{}/", self.members.len()), bytes.len());
        // ld64 reads a Mach-O member only at an 8-byte aligned offset: a BSD
        // name (`#1/` and its length) stored before the data pads it there.
        #[cfg(target_os = "macos")]
        let index = self.members.len().to_string();
        #[cfg(target_os = "macos")]
        let padded = (index.len()..)
            .find(|n| (self.at + 60 + *n as u64).is_multiple_of(8))
            .expect("some length aligns");
        #[cfg(target_os = "macos")]
        let (name, size) = (format!("#1/{padded}"), padded + bytes.len());
        let header = format!(
            "{:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n",
            name, 0, 0, 0, 644, size
        );
        debug_assert_eq!(header.len(), 60);
        self.file.write_all(header.as_bytes())?;
        self.at += 60;
        #[cfg(target_os = "macos")]
        {
            let mut stored = index.into_bytes();
            stored.resize(padded, 0);
            self.file.write_all(&stored)?;
            self.at += padded as u64;
        }
        self.members.push((self.at, bytes.len()));
        self.file.write_all(bytes)?;
        self.at += bytes.len() as u64;
        if bytes.len() % 2 == 1 {
            self.file.write_all(b"\n")?;
            self.at += 1;
        }
        Ok(())
    }
    /// The bundle, sealed against every further write.
    pub(crate) fn finish(self) -> io::Result<Bundle> {
        let file = self
            .file
            .into_inner()
            .map_err(io::IntoInnerError::into_error)?;
        seal(&file)?;
        Ok(Bundle {
            file,
            members: self.members,
            #[cfg(target_os = "macos")]
            dir: self.dir,
        })
    }
}

#[cfg(target_os = "linux")]
fn memory_file() -> io::Result<File> {
    use std::os::fd::FromRawFd;
    // SAFETY: the name is NUL-terminated and these flags take no other arguments.
    let fd = unsafe {
        libc::memfd_create(
            c"hirsel-link".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: memfd_create returned a fresh owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}
#[cfg(target_os = "linux")]
fn seal(file: &File) -> io::Result<()> {
    let seals = libc::F_SEAL_WRITE | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_SEAL;
    // SAFETY: the file is open and F_ADD_SEALS takes a single integer argument.
    if unsafe {
        libc::fcntl(
            std::os::fd::AsRawFd::as_raw_fd(file),
            libc::F_ADD_SEALS,
            seals,
        )
    } < 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
#[cfg(target_os = "macos")]
const BUNDLE_NAME: &str = "bundle.a";

/// A read-write file in a fresh private directory, which the bundle keeps.
#[cfg(target_os = "macos")]
fn memory_file() -> io::Result<(File, PathBuf)> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let dir = tempfile::Builder::new()
        .prefix("hirsel-link-")
        .tempdir()?
        .keep();
    let file = (|| {
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(dir.join(BUNDLE_NAME))
    })();
    match file {
        Ok(file) => Ok((file, dir)),
        Err(error) => {
            let _ = std::fs::remove_dir_all(&dir);
            Err(error)
        }
    }
}
/// Read-only and user-immutable: no writer, this process included, changes it.
#[cfg(target_os = "macos")]
fn seal(file: &File) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    file.sync_all()?;
    // SAFETY: a live descriptor and integer arguments.
    unsafe {
        if libc::fchmod(file.as_raw_fd(), 0o400) < 0
            || libc::fchflags(file.as_raw_fd(), libc::UF_IMMUTABLE) < 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Each object's checked bytes, in order: the bytes an object this request
/// read or emitted holds, or a stored object's, read and checked here
/// through one descriptor closed before the next opens. Every stored object
/// is checked; each that fails is removed, and the instances of all of them
/// are returned.
pub(crate) fn object_members(
    objects: &[&ObjectProduct],
    counters: &mut crate::reach::ReachCounters,
) -> Result<Result<Vec<Arc<[u8]>>, Vec<Identity>>, NativeStop> {
    use std::io::Read;
    #[cfg(test)]
    BEFORE_OPEN.with(|hook| {
        if let Some(hook) = hook.borrow_mut().as_mut() {
            hook(&objects.iter().map(|o| o.path.clone()).collect::<Vec<_>>());
        }
    });
    let mut refused = Vec::new();
    let mut members = Vec::with_capacity(objects.len());
    for &object in objects {
        if let Some(held) = &object.bytes {
            members.push(Arc::clone(held));
            continue;
        }
        let mut file = match File::open(&object.path) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                refused.push(object.instance.clone());
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        crate::meter::count(crate::meter::Count::FilesOpened, 1);
        crate::meter::count(crate::meter::Count::ObjectsOpened, 1);
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        drop(file);
        counters.objects_verified += 1;
        if crate::store::checksum(&bytes) != object.check {
            match std::fs::remove_file(&object.path) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            refused.push(object.instance.clone());
            continue;
        }
        members.push(bytes.into());
    }
    if !refused.is_empty() {
        return Ok(Err(refused));
    }
    #[cfg(test)]
    AFTER_CHECK.with(|hook| {
        if let Some(hook) = hook.borrow_mut().as_mut() {
            hook(&objects.iter().map(|o| o.path.clone()).collect::<Vec<_>>());
        }
    });
    Ok(Ok(members))
}
