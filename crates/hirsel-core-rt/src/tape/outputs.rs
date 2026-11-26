//! Optional launcher-side export of validated, committed output. The client
//! supplies a private directory descriptor and owns publication and cleanup.
use crate::provider::{SEND_PAIR, STDERR, STDOUT};
use crate::tape::{Live, Request, ResourceKind, buffer_bytes, units::crossing::Snapshot};
use std::{
    collections::BTreeMap,
    ffi::CString,
    fs::File,
    io::{self, Write},
    os::fd::{AsRawFd, FromRawFd},
    sync::{Mutex, OnceLock},
};

static CAPTURE: OnceLock<Mutex<Capture>> = OnceLock::new();
#[derive(Default)]
struct Capture {
    files: BTreeMap<String, File>,
    bytes: u64,
    failed: bool,
}
const MAX_BYTES: u64 = 64 << 20;
const MAX_FILES: usize = 256;

pub(in crate::tape) fn start(mode: u64) {
    if mode != 0
        && std::env::var_os("HIRSEL_CAPTURE_OUTPUTS_V1").as_deref()
            == Some(std::ffi::OsStr::new("1"))
    {
        CAPTURE.get_or_init(Default::default);
    }
}

fn create(name: &str) -> io::Result<File> {
    let name = CString::new(name)?;
    // SAFETY: stdin is the launcher's staging directory when exporting.
    // All names are generated here, and every file must be new.
    let fd = unsafe {
        libc::openat(
            0,
            name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC,
            0o600 as libc::c_uint,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: openat transferred this descriptor to us.
    Ok(unsafe { File::from_raw_fd(fd) })
}

impl Capture {
    fn append(&mut self, name: String, first: &[u8], second: &[u8]) -> io::Result<()> {
        let bytes = (first.len() + second.len()) as u64;
        if bytes > MAX_BYTES - self.bytes {
            return Err(io::Error::other("output export exceeds 64 MiB"));
        }
        let files = self.files.len();
        let file = match self.files.entry(name) {
            std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::btree_map::Entry::Vacant(entry) => {
                if files == MAX_FILES {
                    return Err(io::Error::other("output export exceeds 256 files"));
                }
                let file = create(entry.key())?;
                entry.insert(file)
            }
        };
        file.write_all(first)?;
        file.write_all(second)?;
        self.bytes += bytes;
        Ok(())
    }
}

/// The adapter pins the initialized request buffers until replay delivers the result.
pub(super) unsafe fn delivered(live: &Live, r: &Request, object: u64, count: usize) {
    let Some(capture) = CAPTURE.get() else { return };
    if live.replay_abort.is_none() || count == 0 {
        return;
    }
    let Some(rep) = &live.replayer else { return };
    let op = r.operation;
    let name = match op {
        STDOUT => "stdout".to_owned(),
        STDERR => "stderr".to_owned(),
        SEND_PAIR
            if matches!(
                live.sockets.get(&(object as usize)),
                Some(Snapshot::Stream { .. })
            ) || live
                .typed_resources
                .get(&(object as usize))
                .is_some_and(|r| r.kind == ResourceKind::UnixStream) =>
        {
            format!("socket-{object}")
        }
        _ => return,
    };
    let mut capture = capture.lock().unwrap();
    if capture.failed {
        return;
    }
    // SAFETY: replay_out validated the count across the pinned carriers.
    let first = unsafe { buffer_bytes(&r.buffer) };
    let split = first.len().min(count);
    let second = if split < count {
        // SAFETY: only a validated SEND_PAIR delivery can extend into the tail.
        let tail = unsafe { &*(*r.path.cast::<crate::net::NetworkRequest>()).send_tail };
        &(unsafe { buffer_bytes(tail) })[..count - split]
    } else {
        &[]
    };
    let name = if rep.stream == 0 {
        name
    } else {
        format!("stream-{}-{name}", rep.stream)
    };
    if let Err(error) = capture.append(name, &first[..split], second) {
        capture.failed = true;
        eprintln!("replay outputs: {error}");
    }
}

pub(in crate::tape) fn finish(status: u8) -> u8 {
    if status == 0
        && let Some(capture) = CAPTURE.get()
    {
        let capture = capture.lock().unwrap();
        if !capture.failed {
            let complete = capture
                .files
                .values()
                .try_for_each(|file| crate::host::file::sync(file.as_raw_fd(), false, false))
                .and_then(|()| create("complete-v1"));
            if let Err(error) = complete {
                eprintln!("replay outputs: {error}");
            }
        }
    }
    status
}
