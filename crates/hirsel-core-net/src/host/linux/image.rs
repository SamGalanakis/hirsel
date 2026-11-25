//! Executable images on Linux: a verified image is copied into a sealed memfd
//! (no write, grow or shrink, seals sealed) and reopened read-only, and its
//! ELF program interpreter is read from the sealed copy.
use std::ffi::{CStr, CString};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::os::fd::{AsRawFd, FromRawFd};
use std::sync::Arc;

const SEALS: libc::c_int =
    libc::F_SEAL_WRITE | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_SEAL;
fn memfd() -> io::Result<File> {
    // SAFETY: constant C string and Linux memfd flags. A successful call transfers the fd.
    unsafe {
        let fd = libc::memfd_create(
            c"hirsel-exec".as_ptr(),
            libc::MFD_ALLOW_SEALING | libc::MFD_CLOEXEC,
        );
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(File::from_raw_fd(fd))
    }
}

fn seal(file: &File) -> io::Result<()> {
    let fd = file.as_raw_fd();
    // SAFETY: all fcntl requests use the ABI's integer argument or no argument.
    unsafe {
        if libc::fcntl(fd, libc::F_ADD_SEALS, SEALS) < 0 {
            return Err(io::Error::last_os_error());
        }
        let actual = libc::fcntl(fd, libc::F_GET_SEALS);
        if actual < 0 {
            return Err(io::Error::last_os_error());
        }
        if actual & SEALS != SEALS {
            return Err(io::Error::other("memfd seals do not hold"));
        }
    }
    Ok(())
}

/// Copy `source` into a private, immutable file and return it read-only.
pub(crate) fn pin(source: &mut File) -> Result<File, String> {
    let step = |step, error| format!("{step}: {error}");
    let mut image = memfd().map_err(|e| step("memfd", e))?;
    io::copy(source, &mut image).map_err(|e| step("copy image", e))?;
    // SAFETY: this is the private descriptor whose mode the launch owns.
    if unsafe { libc::fchmod(image.as_raw_fd(), 0o500) } < 0 {
        return Err(step("set image mode", io::Error::last_os_error()));
    }
    seal(&image).map_err(|e| step("seal image", e))?;
    File::open(format!("/proc/self/fd/{}", image.as_raw_fd()))
        .map_err(|e| step("open sealed image read-only", e))
}

/// Whether this host can pin an image.
pub(crate) fn probe() -> io::Result<()> {
    memfd().and_then(|image| seal(&image))
}

/// The program interpreter of a native executable, which must be ELF for
/// this machine.
pub(crate) fn interpreter(file: &mut File) -> io::Result<Option<Arc<CStr>>> {
    let invalid = || io::Error::other("expected a native ELF executable without a #! line");
    let mut e = [0; 64];
    file.read_exact(&mut e).map_err(|_| invalid())?;
    let read_u16 = |field| u16::from_le_bytes(e[field..field + 2].try_into().unwrap());
    let read_u64 = |field| u64::from_le_bytes(e[field..field + 8].try_into().unwrap());
    let machine = if cfg!(target_arch = "x86_64") {
        62
    } else if cfg!(target_arch = "aarch64") {
        183
    } else {
        return Err(invalid());
    };
    if !e.starts_with(b"\x7fELF\x02\x01\x01")
        || !matches!(read_u16(16), 2 | 3)
        || read_u16(18) != machine
        || read_u16(52) != 64
        || read_u16(54) != 56
    {
        return Err(invalid());
    }
    let start = read_u64(32);
    let len = file.metadata()?.len();
    let count = u64::from(read_u16(56));
    if count == 0 || start.checked_add(56 * count).is_none_or(|end| end > len) {
        return Err(invalid());
    }
    let mut interpreter = None;
    let mut executable = false;
    let entry = read_u64(24);
    for i in 0..count {
        file.seek(SeekFrom::Start(start + 56 * i))?;
        let mut ph = [0; 56];
        file.read_exact(&mut ph)?;
        let word = |at| u32::from_le_bytes(ph[at..at + 4].try_into().unwrap());
        let quad = |at| u64::from_le_bytes(ph[at..at + 8].try_into().unwrap());
        let offset = quad(8);
        let address = quad(16);
        let size = quad(32);
        match word(0) {
            1 if word(4) & 1 != 0 => {
                executable |= (address..address.saturating_add(size)).contains(&entry)
                    && offset.checked_add(size).is_some_and(|end| end <= len);
            }
            3 => {
                if interpreter.is_some()
                    || !(2..=4096).contains(&size)
                    || offset.checked_add(size).is_none_or(|end| end > len)
                {
                    return Err(invalid());
                }
                file.seek(SeekFrom::Start(offset))?;
                let mut interp = vec![0; size as usize];
                file.read_exact(&mut interp)?;
                let path = CString::from_vec_with_nul(interp).map_err(|_| invalid())?;
                interpreter = Some(Arc::from(path.into_boxed_c_str()));
            }
            _ => {}
        }
    }
    if !executable {
        return Err(invalid());
    }
    Ok(interpreter)
}
