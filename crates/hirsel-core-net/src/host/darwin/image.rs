//! Executable images on Darwin, which has no memfd or seals and launches by
//! path: a verified image is cloned (`fclonefileat`, or copied across
//! volumes) into a private directory, made read-only and immutable
//! (`UF_IMMUTABLE`), and reopened read-only; a launch names it through
//! `F_GETPATH`. A native image is a Mach-O executable, which names no
//! program interpreter.
use std::ffi::{CStr, CString};
use std::fs::File;
use std::io::{self, Read};
use std::os::darwin::fs::MetadataExt;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::sync::{Arc, OnceLock};

/// A private (0700) directory for pinned images, kept for the process's life.
fn directory() -> io::Result<&'static OwnedFd> {
    static DIRECTORY: OnceLock<OwnedFd> = OnceLock::new();
    if let Some(dir) = DIRECTORY.get() {
        return Ok(dir);
    }
    let mut template = std::env::temp_dir()
        .join("hirsel-exec-XXXXXX")
        .into_os_string()
        .into_vec();
    template.push(0);
    // SAFETY: template is a writable NUL-terminated path ending in XXXXXX;
    // mkdtemp creates the directory 0700.
    if unsafe { libc::mkdtemp(template.as_mut_ptr().cast()) }.is_null() {
        return Err(io::Error::last_os_error());
    }
    let path = CString::from_vec_with_nul(template).map_err(io::Error::other)?;
    let flags = libc::O_SEARCH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    // SAFETY: the C path is live; a success transfers a descriptor.
    let fd = unsafe { libc::open(path.as_ptr(), flags) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: open returned an owned descriptor.
    let _ = DIRECTORY.set(unsafe { OwnedFd::from_raw_fd(fd) });
    Ok(DIRECTORY.get().unwrap())
}

fn private_copy(source: &mut File) -> io::Result<File> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = directory()?.as_raw_fd();
    let name = CString::new(format!("image-{}", NEXT.fetch_add(1, Ordering::Relaxed)))?;
    // SAFETY: both descriptors and the C name are live for the call.
    if unsafe { libc::fclonefileat(source.as_raw_fd(), dir, name.as_ptr(), 0) } < 0 {
        let error = io::Error::last_os_error();
        if !matches!(error.raw_os_error(), Some(libc::EXDEV | libc::ENOTSUP)) {
            return Err(error);
        }
        let flags = libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC;
        // SAFETY: as above; a success transfers a descriptor.
        let fd = unsafe { libc::openat(dir, name.as_ptr(), flags, 0o600) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: openat returned an owned descriptor.
        let mut copy = unsafe { File::from_raw_fd(fd) };
        io::copy(source, &mut copy)?;
    }
    let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    // SAFETY: as above; a success transfers a descriptor.
    let fd = unsafe { libc::openat(dir, name.as_ptr(), flags) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: openat returned an owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}

/// Copy `source` into a private, immutable file and return it read-only.
pub(crate) fn pin(source: &mut File) -> Result<File, String> {
    let step = |step, error| format!("{step}: {error}");
    let image = private_copy(source).map_err(|e| step("clone image", e))?;
    // A clone keeps its source's flags, and a compressed file has its bytes only while
    // UF_COMPRESSED is set: without it the file reads as empty. So the immutable flag
    // joins the flags the copy has.
    let flags = image
        .metadata()
        .map_err(|e| step("read image flags", e))?
        .st_flags()
        | libc::UF_IMMUTABLE;
    // SAFETY: the private copy's mode and flags belong to this launch.
    unsafe {
        if libc::fchmod(image.as_raw_fd(), 0o500) < 0 {
            return Err(step("set image mode", io::Error::last_os_error()));
        }
        if libc::fchflags(image.as_raw_fd(), flags) < 0 {
            return Err(step("make image immutable", io::Error::last_os_error()));
        }
    }
    Ok(image)
}

/// Whether this host can pin an image.
pub(crate) fn probe() -> io::Result<()> {
    directory().map(|_| ())
}

/// A native executable is a 64-bit Mach-O for this machine; dyld is implicit.
pub(crate) fn interpreter(file: &mut File) -> io::Result<Option<Arc<CStr>>> {
    let invalid = || io::Error::other("expected a native Mach-O executable without a #! line");
    let mut header = [0; 16];
    file.read_exact(&mut header).map_err(|_| invalid())?;
    let word = |at: usize| u32::from_le_bytes(header[at..at + 4].try_into().unwrap());
    const MH_MAGIC_64: u32 = 0xfeed_facf;
    const CPU_TYPE_ARM64: u32 = 0x0100_000c;
    const MH_EXECUTE: u32 = 2;
    if word(0) != MH_MAGIC_64 || word(4) != CPU_TYPE_ARM64 || word(12) != MH_EXECUTE {
        return Err(invalid());
    }
    Ok(None)
}
