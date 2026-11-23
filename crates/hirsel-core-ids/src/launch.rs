//! Owned, immutable metadata for one launch (docs/spec/launch.md).
use sha2::{Digest, Sha256};
use std::{
    fmt,
    fs::File,
    io::{self, Read},
    path::Path,
};

#[cfg(target_os = "linux")]
use std::sync::{Arc, LazyLock};

pub const DEFAULT_N_PLACE: u64 = 16 * 1024 * 1024;
/// Private platform transport for the launcher PID, image descriptor and SHA-256.
pub const LAUNCH_IMAGE_ENV: &str = "HIRSEL_LAUNCH_IMAGE";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Executable {
    Digest([u8; 32]),
    Unavailable(String),
}

#[cfg(target_os = "linux")]
type DeferredDigest = LazyLock<Executable, Box<dyn FnOnce() -> Executable + Send>>;

#[derive(Clone)]
enum Image {
    Ready(Executable),
    #[cfg(target_os = "linux")]
    Deferred(Arc<DeferredDigest>),
}

impl Image {
    fn executable(&self) -> &Executable {
        match self {
            Self::Ready(executable) => executable,
            #[cfg(target_os = "linux")]
            Self::Deferred(digest) => LazyLock::force(digest),
        }
    }
}

impl fmt::Debug for Image {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.executable().fmt(f)
    }
}

impl PartialEq for Image {
    fn eq(&self, other: &Self) -> bool {
        self.executable() == other.executable()
    }
}

impl Eq for Image {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Metadata {
    executable: Image,
    target: String,
    n_place: u64,
    describe: bool,
    wake: bool,
    idle_ms: u64,
    preview: Option<Vec<u8>>,
    preparing: Option<([u8; 16], [u8; 32])>,
}

impl Metadata {
    pub fn native(target: &str, n_place: u64, describe: bool) -> Self {
        let file = File::open("/proc/self/exe");
        #[cfg(target_os = "linux")]
        if let Ok(file) = &file
            && let Some(image) = MappedImage::new(file)
        {
            let digest: DeferredDigest =
                LazyLock::new(Box::new(move || Executable::Digest(image.digest())));
            return Self::with_image(Image::Deferred(Arc::new(digest)), target, n_place, describe);
        }
        Self::with_image(Image::Ready(read_digest(file)), target, n_place, describe)
    }

    pub fn artifact(path: &Path, target: &str, n_place: u64, describe: bool) -> Self {
        let executable = read_digest(File::open(path));
        Self::with_image(Image::Ready(executable), target, n_place, describe)
    }

    fn with_image(executable: Image, target: &str, n_place: u64, describe: bool) -> Self {
        Self {
            executable,
            target: target.into(),
            n_place,
            describe,
            wake: false,
            idle_ms: 0,
            preview: None,
            preparing: None,
        }
    }

    /// The evaluator already owns the exact image it loaded; no pathname reopen.
    pub fn loaded(image: &[u8], target: &str, n_place: u64, describe: bool) -> Self {
        Self::embedded(Some(hash_bytes(image)), target, n_place, describe)
    }

    /// Embedded launch metadata for a tool that replays such a recording.
    pub fn embedded(digest: Option<[u8; 32]>, target: &str, n_place: u64, describe: bool) -> Self {
        let executable = digest
            .map(Executable::Digest)
            .unwrap_or_else(|| Executable::Unavailable("no executable image supplied".into()));
        Self::with_image(Image::Ready(executable), target, n_place, describe)
    }

    pub fn source(target: &str, n_place: u64, describe: bool) -> Self {
        Self::embedded(None, target, n_place, describe)
    }

    pub fn digest(&self) -> &[u8] {
        match self.executable.executable() {
            Executable::Digest(bytes) => bytes,
            Executable::Unavailable(_) => &[],
        }
    }
    pub fn unavailable(&self) -> Option<&str> {
        match self.executable.executable() {
            Executable::Digest(_) => None,
            Executable::Unavailable(reason) => Some(reason),
        }
    }
    pub fn target(&self) -> &str {
        &self.target
    }
    pub fn n_place(&self) -> u64 {
        self.n_place
    }
    pub fn is_describe(&self) -> bool {
        self.describe
    }

    /// Private runner transport: scalars and bytes, without program allocation.
    /// Whether this launch describes its capabilities instead of running.
    #[inline(always)]
    pub fn describe(&self) -> bool {
        self.describe
    }
    pub fn is_wake(&self) -> bool {
        self.wake
    }

    pub fn idle_ms(&self) -> u64 {
        self.idle_ms
    }

    pub fn with_wake(mut self, idle_ms: u64) -> Self {
        self.wake = true;
        self.idle_ms = idle_ms;
        self
    }

    pub fn with_preview(mut self, old: Vec<u8>) -> Self {
        self.preview = Some(old);
        self
    }
    pub fn with_prepare(mut self, attempt: [u8; 16], digest: [u8; 32]) -> Self {
        self.preparing = Some((attempt, digest));
        self
    }

    pub fn word(&self, field: u64, index: u64) -> u64 {
        let byte = |bytes: &[u8]| {
            usize::try_from(index)
                .ok()
                .and_then(|i| bytes.get(i))
                .copied()
                .map(u64::from)
                .unwrap_or(0)
        };
        match field {
            0 => self.n_place,
            1 => self.digest().len() as u64,
            2 => byte(self.digest()),
            3 => self.target.len() as u64,
            4 => byte(self.target.as_bytes()),
            5 => u64::from(self.describe()),
            6 => u64::from(self.preview.is_some()),
            7 => self.preview.as_ref().map_or(0, |old| old.len() as u64),
            8 => self.preview.as_deref().map_or(0, byte),
            9 => u64::from(self.preparing.is_some()),
            10 => self
                .preparing
                .as_ref()
                .map_or(0, |(attempt, _)| byte(attempt)),
            11 => self
                .preparing
                .as_ref()
                .map_or(0, |(_, digest)| byte(digest)),
            12 => u64::from(self.wake),
            13 => self.idle_ms,
            _ => 0,
        }
    }
}

#[cold]
fn read_digest(image: io::Result<File>) -> Executable {
    match image.and_then(hash) {
        Ok(digest) => Executable::Digest(digest),
        Err(error) => Executable::Unavailable(error.to_string()),
    }
}

fn hash(mut image: File) -> io::Result<[u8; 32]> {
    let mut hash = Sha256::new();
    let mut scratch = [0; 8192];
    loop {
        let count = image.read(&mut scratch)?;
        if count == 0 {
            return Ok(finish_hash(hash));
        }
        hash.update(&scratch[..count]);
    }
}

fn hash_bytes(bytes: &[u8]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(bytes);
    finish_hash(hash)
}

// Both the mapping and the fallback reader finish the same streaming digest.
#[inline(never)]
fn finish_hash(hash: Sha256) -> [u8; 32] {
    hash.finalize().into()
}

// Capture the executable while it is still the launch image. The read-only
// mapping pins the inode without consuming a descriptor for the whole run.
#[cfg(target_os = "linux")]
struct MappedImage(std::ptr::NonNull<[u8]>);

// SAFETY: Linux denies writes to the running executable. The private mapping
// has one owner and can be hashed and unmapped on the first reader's thread.
#[cfg(target_os = "linux")]
unsafe impl Send for MappedImage {}

#[cfg(target_os = "linux")]
impl MappedImage {
    /// The file is the descriptor just opened through /proc/self/exe.
    fn new(file: &File) -> Option<Self> {
        use std::{os::fd::AsRawFd, ptr::NonNull};

        let len = usize::try_from(file.metadata().ok()?.len()).ok()?;
        if len == 0 || len > isize::MAX as usize {
            return None;
        }
        // SAFETY: this file is the executing image, and a successful mapping
        // owns a private, read-only range of its complete reported length.
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ,
                libc::MAP_PRIVATE,
                file.as_raw_fd(),
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            return None;
        }
        let base = NonNull::new(ptr.cast::<u8>())?;
        Some(Self(NonNull::slice_from_raw_parts(base, len)))
    }

    /// Hash the image one window at a time and return each hashed window's
    /// pages, so the process never holds more than one window of the image
    /// resident for the digest (a whole-image hash once set the peak RSS of a
    /// small server).
    fn digest(&self) -> [u8; 32] {
        const WINDOW: usize = 1 << 20;
        let mut hash = Sha256::new();
        // SAFETY: the image remains mapped for this owner's lifetime.
        let bytes = unsafe { self.0.as_ref() };
        for window in bytes.chunks(WINDOW) {
            hash.update(window);
            // SAFETY: this owner's private read-only mapping; the window is
            // page aligned and nothing reads it again, and a later read would
            // only fault the same file bytes back in.
            unsafe {
                libc::madvise(
                    window.as_ptr().cast_mut().cast(),
                    window.len(),
                    libc::MADV_DONTNEED,
                )
            };
        }
        finish_hash(hash)
    }
}

#[cfg(target_os = "linux")]
impl Drop for MappedImage {
    fn drop(&mut self) {
        // SAFETY: this owner releases its mapping exactly once.
        unsafe { libc::munmap(self.0.cast().as_ptr(), self.0.len()) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identity_unavailable() {
        let missing = std::env::temp_dir().join(format!("hirsel-no-image-{}", std::process::id()));
        let launch = Metadata::artifact(&missing, "aarch64-unknown-linux-gnu", 73, false);
        assert!(launch.digest().is_empty());
        assert!(launch.unavailable().is_some());
        assert_eq!(launch.n_place(), 73);
        assert_eq!(launch.target(), "aarch64-unknown-linux-gnu");
        assert!(Metadata::source("test", 1, true).digest().is_empty());
        assert!(
            Metadata::embedded(None, "test", 1, false)
                .digest()
                .is_empty()
        );
    }

    #[test]
    fn artifact_identity_and_embedder_inputs_are_owned() {
        let path = std::env::temp_dir().join(format!("hirsel-launch-image-{}", std::process::id()));
        std::fs::write(&path, b"abc").unwrap();
        let launch = Metadata::artifact(&path, "target", 419, true);
        // FIPS 180-4 / NIST SHA-256 abc vector, no identity-domain framing.
        assert_eq!(
            launch.digest(),
            &[
                0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
                0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
                0xf2, 0x00, 0x15, 0xad
            ]
        );
        std::fs::write(&path, b"changed").unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(launch.digest()[0], 0xba);
        let mut digest = [7; 32];
        let mut target = "target".to_owned();
        let embedded = Metadata::embedded(Some(digest), &target, 419, true);
        digest.fill(8);
        target.clear();
        assert_eq!(embedded.digest(), &[7; 32]);
        assert_eq!(embedded.target(), "target");
        assert_eq!(embedded.word(0, 0), 419);
        assert_eq!(embedded.word(5, 0), 1);
    }
}
