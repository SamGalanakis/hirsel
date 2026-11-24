//! Native Exec admission. This module acquires the pinned resources before main.
use super::{Policy, Profile};
use ring::digest::{Context, SHA256};
use std::collections::BTreeMap;
use std::ffi::{CStr, CString};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom};
#[cfg(target_os = "linux")]
use std::os::fd::FromRawFd;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::sync::Arc;

#[cfg(target_os = "linux")]
const SEALS: libc::c_int =
    libc::F_SEAL_WRITE | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_SEAL;
pub struct PinnedImage {
    pub file: Arc<OwnedFd>,
    pub interpreter: Option<Arc<CStr>>,
    /// The private copy macOS executes by path, removed with the last pin;
    /// None for an image fixed in place on a read-only volume.
    #[cfg(target_os = "macos")]
    pub copy: Option<Arc<PrivateCopy>>,
}

pub struct PinnedProfile {
    pub spec: Profile,
    pub image: PinnedImage,
}

/// macOS has no sealed memory files or exec-from-descriptor. The image is
/// copied into a fresh private directory (0700), made read-only (0500) and
/// user-immutable, and executed by that path. Unlike a sealed memfd, the
/// run's own user could still clear the flag and rewrite it.
#[cfg(target_os = "macos")]
pub struct PrivateCopy {
    pub path: std::path::PathBuf,
}

#[cfg(target_os = "macos")]
impl Drop for PrivateCopy {
    fn drop(&mut self) {
        use std::os::unix::ffi::OsStrExt;
        if let Ok(path) = CString::new(self.path.as_os_str().as_bytes()) {
            // SAFETY: path is NUL terminated.
            unsafe { libc::chflags(path.as_ptr(), 0) };
        }
        let _ = std::fs::remove_file(&self.path);
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::remove_dir(dir);
        }
    }
}

#[cfg(target_os = "macos")]
fn memfd() -> io::Result<(File, PrivateCopy)> {
    let dir = tempfile::Builder::new()
        .prefix("hirsel-exec-")
        .tempdir()?
        .keep();
    std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
    let path = dir.join("image");
    let copy = PrivateCopy { path };
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o700)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&copy.path)?;
    Ok((file, copy))
}

#[cfg(target_os = "macos")]
fn seal(file: &File) -> io::Result<()> {
    // SAFETY: a live descriptor and integer flags.
    unsafe {
        if libc::fsync(file.as_raw_fd()) < 0
            || libc::fchflags(file.as_raw_fd(), libc::UF_IMMUTABLE) < 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: a live descriptor and its result slot.
    if unsafe { libc::fstat(file.as_raw_fd(), stat.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fstat succeeded.
    if unsafe { stat.assume_init() }.st_flags & libc::UF_IMMUTABLE == 0 {
        return Err(io::Error::other("the image copy is not immutable"));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
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

/// A thin or universal arm64 Mach-O executable; its interpreter is the
/// dynamic linker it names (LC_LOAD_DYLINKER).
#[cfg(target_os = "macos")]
fn native_interpreter(file: &mut File) -> io::Result<Option<Arc<CStr>>> {
    const CPU_ARM64: u32 = 0x0100_000c;
    let invalid =
        || io::Error::other("expected a native arm64 Mach-O executable without a #! line");
    let len = file.metadata()?.len();
    let mut magic = [0; 8];
    file.read_exact(&mut magic).map_err(|_| invalid())?;
    let be = |b: &[u8]| u32::from_be_bytes(b[..4].try_into().unwrap());
    let mut base = 0u64;
    if be(&magic) == 0xcafe_babe {
        // A universal binary: pick its arm64 slice.
        let count = be(&magic[4..]).min(64);
        let mut found = None;
        for _ in 0..count {
            let mut arch = [0; 20];
            file.read_exact(&mut arch).map_err(|_| invalid())?;
            if be(&arch) == CPU_ARM64 {
                found = Some(u64::from(be(&arch[8..])));
                break;
            }
        }
        base = found.ok_or_else(invalid)?;
    }
    file.seek(SeekFrom::Start(base))?;
    let mut header = [0; 32];
    file.read_exact(&mut header).map_err(|_| invalid())?;
    let le = |at: usize| u32::from_le_bytes(header[at..at + 4].try_into().unwrap());
    // MH_MAGIC_64, arm64, MH_EXECUTE.
    if le(0) != 0xfeed_facf || le(4) != CPU_ARM64 || le(12) != 2 {
        return Err(invalid());
    }
    let (count, size) = (le(16), u64::from(le(20)));
    if base.checked_add(32 + size).is_none_or(|end| end > len) {
        return Err(invalid());
    }
    let mut commands = vec![0; size as usize];
    file.read_exact(&mut commands).map_err(|_| invalid())?;
    let mut at = 0usize;
    let mut interpreter = None;
    let mut entry = false;
    for _ in 0..count {
        let word = |at: usize| {
            commands
                .get(at..at + 4)
                .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
        };
        let (Some(cmd), Some(cmdsize)) = (word(at), word(at + 4)) else {
            return Err(invalid());
        };
        let cmdsize = cmdsize as usize;
        if cmdsize < 8 || at + cmdsize > commands.len() {
            return Err(invalid());
        }
        match cmd {
            // LC_LOAD_DYLINKER: the name follows at its offset in the command.
            0xe => {
                let offset = word(at + 8).ok_or_else(invalid)? as usize;
                let name = commands
                    .get(at + offset..at + cmdsize)
                    .ok_or_else(invalid)?;
                let end = name.iter().position(|&b| b == 0).ok_or_else(invalid)?;
                if interpreter.is_some() || end < 1 {
                    return Err(invalid());
                }
                let path = CString::new(&name[..end]).map_err(|_| invalid())?;
                interpreter = Some(Arc::from(path.into_boxed_c_str()));
            }
            // LC_MAIN (0x80000028) or LC_UNIXTHREAD (0x5) gives the entry.
            0x8000_0028 | 0x5 => entry = true,
            _ => {}
        }
        at += cmdsize;
    }
    if !entry {
        return Err(invalid());
    }
    Ok(interpreter)
}

#[cfg(target_os = "linux")]
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

/// Copy `source` into a fresh sealed image and reopen it read-only.
fn sealed_copy(
    source: &mut File,
    #[cfg(target_os = "macos")] (mut image, copy): (File, PrivateCopy),
    #[cfg(target_os = "linux")] mut image: File,
) -> Result<SealedCopy, String> {
    let step = |step, error| format!("{step}: {error}");
    io::copy(source, &mut image).map_err(|e| step("copy image", e))?;
    // SAFETY: this is the private descriptor whose mode the launch owns.
    if unsafe { libc::fchmod(image.as_raw_fd(), 0o500) } < 0 {
        return Err(step("set image mode", io::Error::last_os_error()));
    }
    seal(&image).map_err(|e| step("seal image", e))?;
    #[cfg(target_os = "linux")]
    let read_only = File::open(format!("/proc/self/fd/{}", image.as_raw_fd()))
        .map_err(|e| step("open sealed image read-only", e))?;
    #[cfg(target_os = "macos")]
    let read_only = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&copy.path)
        .map_err(|e| step("open sealed image read-only", e))?;
    #[cfg(target_os = "macos")]
    return Ok((read_only, Some(Arc::new(copy))));
    #[cfg(target_os = "linux")]
    Ok(read_only)
}

#[cfg(target_os = "linux")]
type SealedCopy = File;
#[cfg(target_os = "macos")]
type SealedCopy = (File, Option<Arc<PrivateCopy>>);

/// Whether `path` and every directory above it are on read-only volumes,
/// with no symlink among them, as for an image on the sealed system volume.
/// No one can change such a file, so it is executed where it is: macOS kills
/// a copy of a platform binary.
#[cfg(target_os = "macos")]
fn fixed_in_place(path: &std::path::Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    path.is_absolute()
        && path.ancestors().all(|at| {
            let Ok(c) = CString::new(at.as_os_str().as_bytes()) else {
                return false;
            };
            let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
            let mut fs = std::mem::MaybeUninit::<libc::statfs>::uninit();
            // SAFETY: a NUL-terminated path and live result slots.
            unsafe {
                libc::lstat(c.as_ptr(), stat.as_mut_ptr()) == 0
                    && stat.assume_init().st_mode & libc::S_IFMT != libc::S_IFLNK
                    && libc::statfs(c.as_ptr(), fs.as_mut_ptr()) == 0
                    && fs.assume_init().f_flags & libc::MNT_RDONLY as u32 != 0
            }
        })
}

impl PinnedImage {
    pub fn open(profile: &Profile) -> Result<Self, String> {
        let step = |step, error| format!("{step}: {error}");
        #[cfg(target_os = "linux")]
        let image = memfd().map_err(|e| step("memfd", e))?;
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
        #[cfg(target_os = "linux")]
        let mut file = sealed_copy(&mut source, image)?;
        #[cfg(target_os = "macos")]
        let (mut file, copy) = if fixed_in_place(std::path::Path::new(&profile.image)) {
            (source, None)
        } else {
            let image = memfd().map_err(|e| step("private image copy", e))?;
            sealed_copy(&mut source, image)?
        };
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
            #[cfg(target_os = "macos")]
            copy,
        })
    }
}

pub fn probe_seals() -> io::Result<()> {
    #[cfg(target_os = "macos")]
    return memfd().and_then(|(image, _copy)| seal(&image));
    #[cfg(target_os = "linux")]
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
