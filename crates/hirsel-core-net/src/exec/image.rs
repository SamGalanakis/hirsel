//! Native Exec admission. This module acquires the pinned resources before main.
use super::{Policy, Profile};
use ring::digest::{Context, SHA256};
use std::collections::BTreeMap;
use std::ffi::{CStr, CString};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::sync::Arc;

const SEALS: libc::c_int =
    libc::F_SEAL_WRITE | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_SEAL;
pub struct PinnedImage {
    pub file: Arc<OwnedFd>,
    pub interpreter: Option<Arc<CStr>>,
}

pub struct PinnedProfile {
    pub spec: Profile,
    pub image: PinnedImage,
}

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

fn native_interpreter(file: &mut File) -> io::Result<Option<Arc<CStr>>> {
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

impl PinnedImage {
    pub fn open(profile: &Profile) -> Result<Self, String> {
        let step = |step, error| format!("{step}: {error}");
        let mut image = memfd().map_err(|e| step("memfd", e))?;
        let mut source = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY)
            .open(&profile.image)
            .map_err(|e| step("open image", e))?;
        if !source
            .metadata()
            .map_err(|e| step("stat image", e))?
            .is_file()
        {
            return Err("image is not a regular file".into());
        }
        io::copy(&mut source, &mut image).map_err(|e| step("copy image", e))?;
        // SAFETY: this is the private descriptor whose mode the launch owns.
        if unsafe { libc::fchmod(image.as_raw_fd(), 0o500) } < 0 {
            return Err(step("set image mode", io::Error::last_os_error()));
        }
        seal(&image).map_err(|e| step("seal image", e))?;
        let read_only = File::open(format!("/proc/self/fd/{}", image.as_raw_fd()))
            .map_err(|e| step("open sealed image read-only", e))?;
        drop(image);
        let mut file = read_only;
        let mut digest = Context::new(&SHA256);
        let mut bytes = [0; 16384];
        loop {
            let len = match file.read(&mut bytes) {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                read => read.map_err(|e| step("read sealed image", e))?,
            };
            if len == 0 {
                break;
            }
            digest.update(&bytes[..len]);
        }
        if digest.finish().as_ref() != profile.pin {
            return Err("sealed image SHA-256 does not match the pin".into());
        }
        file.rewind().map_err(|e| step("rewind sealed image", e))?;
        let interpreter =
            native_interpreter(&mut file).map_err(|e| step("parse sealed image", e))?;
        file.rewind().map_err(|e| step("rewind sealed image", e))?;
        Ok(Self {
            file: Arc::new(file.into()),
            interpreter,
        })
    }
}

pub fn probe_seals() -> io::Result<()> {
    memfd().and_then(|image| seal(&image))
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
                hex(&spec.pin),
                image.interpreter.as_deref().map_or_else(
                    || "none".to_owned(),
                    |path| path
                        .to_bytes()
                        .iter()
                        .flat_map(|&byte| std::ascii::escape_default(byte))
                        .map(char::from)
                        .collect(),
                ),
                hex(&self.digest),
            ));
            profiles.insert(name, PinnedProfile { spec, image });
        }
        Ok((profiles, lines))
    }
}

pub fn hex(bytes: &[u8; 32]) -> String {
    use std::fmt::Write;
    let mut text = String::with_capacity(64);
    for byte in bytes {
        write!(text, "{byte:02x}").unwrap();
    }
    text
}
