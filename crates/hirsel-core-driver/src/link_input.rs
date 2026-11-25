//! The objects a link hands the linker, as one archive of their checked
//! bytes: a sealed memory file the linker reads whole through one descriptor
//! of this process, however many objects the link has. A link that patches
//! its last executable (`incremental_link`) reads the members it changes
//! and never makes the archive.
#[cfg(test)]
use crate::native::{AFTER_CHECK, BEFORE_OPEN};
use crate::native::{NativeStop, ObjectProduct};
use hirsel_core_ir::Identity;
use hirsel_core_layout::ObjectFormat;
use std::fs::File;
use std::io::{self, Write};
use std::os::unix::fs::FileExt;
use std::path::PathBuf;
use std::sync::Arc;

/// A link's objects, in link order, as the members of an archive without a
/// symbol table, in a sealed memory file: a GNU archive for ELF, and for
/// Mach-O a BSD one whose `#1/N` names place each member's bytes on an
/// 8-byte boundary, as ld64 reads them. The linker reads it under
/// `--whole-archive` or `-force_load` (`native_link::direct_link`), which loads every member
/// in order as if each were named alone, so the executable is the one a link
/// of the separate objects makes. Only checked bytes are written into it, and
/// once sealed nothing changes them.
pub(crate) struct Bundle {
    file: File,
    /// Each member's bytes: offset and length.
    members: Vec<(u64, usize)>,
}

const MAGIC: &[u8; 8] = b"!<arch>\n";

impl Bundle {
    /// The path through which another process reads the bundle.
    pub(crate) fn path(&self) -> PathBuf {
        PathBuf::from(format!(
            "/proc/{}/fd/{}",
            std::process::id(),
            std::os::fd::AsRawFd::as_raw_fd(&self.file)
        ))
    }
    /// The bundle of `members`, in order.
    pub(crate) fn of(members: &[Arc<[u8]>], format: ObjectFormat) -> io::Result<Self> {
        let mut bundle = BundleWriter::new(format)?;
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
    format: ObjectFormat,
    bundle: std::cell::OnceCell<Bundle>,
}

impl Members {
    pub(crate) fn new(members: Vec<Arc<[u8]>>, format: ObjectFormat) -> Self {
        Self {
            members,
            format,
            bundle: std::cell::OnceCell::new(),
        }
    }
    pub(crate) fn bundle(&self) -> io::Result<&Bundle> {
        if self.bundle.get().is_none() {
            let _ = self.bundle.set(Bundle::of(&self.members, self.format)?);
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
    format: ObjectFormat,
}

impl BundleWriter {
    pub(crate) fn new(format: ObjectFormat) -> io::Result<Self> {
        let mut file = io::BufWriter::with_capacity(1 << 20, memory_file()?);
        file.write_all(MAGIC)?;
        Ok(Self {
            file,
            at: MAGIC.len() as u64,
            members: Vec::new(),
            format,
        })
    }
    /// Append checked `bytes` as the next member.
    pub(crate) fn push(&mut self, bytes: &[u8]) -> io::Result<()> {
        // The member's name is its index. `/` ends a GNU member name; a BSD
        // name of N bytes follows the header, NUL-padded to the alignment.
        let (name, inline) = match self.format {
            ObjectFormat::Elf => (format!("{}/", self.members.len()), Vec::new()),
            ObjectFormat::MachO => {
                let mut inline = format!("{}.o", self.members.len()).into_bytes();
                inline.resize(
                    (self.at as usize + 60 + inline.len()).next_multiple_of(8)
                        - self.at as usize
                        - 60,
                    0,
                );
                (format!("#1/{}", inline.len()), inline)
            }
        };
        let header = format!(
            "{:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n",
            name,
            0,
            0,
            0,
            644,
            inline.len() + bytes.len()
        );
        debug_assert_eq!(header.len(), 60);
        self.file.write_all(header.as_bytes())?;
        self.file.write_all(&inline)?;
        self.at += 60 + inline.len() as u64;
        self.members.push((self.at, bytes.len()));
        self.file.write_all(bytes)?;
        self.at += bytes.len() as u64;
        if (inline.len() + bytes.len()) % 2 == 1 {
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
#[cfg(not(target_os = "linux"))]
fn memory_file() -> io::Result<File> {
    Err(io::ErrorKind::Unsupported.into())
}
#[cfg(not(target_os = "linux"))]
fn seal(_: &File) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
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
