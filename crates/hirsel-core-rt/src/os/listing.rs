//! A provider-owned readdir cursor with at most one pending record.
use super::{file_kind, leaf_stat};
use crate::provider::Buffer;
use std::{
    ffi::CStr,
    io,
    os::fd::{AsRawFd, IntoRawFd, OwnedFd},
    ptr::NonNull,
};

const HEADER: usize = 3;
const MAX_RECORD: usize = HEADER + 255;

struct Record {
    bytes: [u8; MAX_RECORD],
    len: usize,
}

pub(super) struct Listing {
    stream: NonNull<libc::DIR>,
    pending: Option<io::Result<Record>>,
    exhausted: bool,
}

// SAFETY: the owner can move the POSIX stream between threads. Access to the
// stream, including cleanup, requires an exclusive borrow of the listing.
unsafe impl Send for Listing {}
unsafe impl Sync for Listing {}

impl Listing {
    pub(super) fn open(fd: OwnedFd) -> io::Result<Self> {
        // SAFETY: fd is an independent open directory description.
        let stream = unsafe { libc::fdopendir(fd.as_raw_fd()) };
        let stream = NonNull::new(stream).ok_or_else(io::Error::last_os_error)?;
        // fdopendir took custody only on success.
        let _ = fd.into_raw_fd();
        Ok(Self {
            stream,
            pending: None,
            exhausted: false,
        })
    }

    pub(super) fn append(&mut self, buffer: &mut Buffer) -> io::Result<u64> {
        let mut len = buffer.len;
        let mut count = 0;
        while buffer.capacity - len >= HEADER + 1 {
            let next = self.pending.take().or_else(|| self.next().transpose());
            let record = match next {
                Some(Ok(record)) if record.len <= buffer.capacity - len => record,
                Some(Err(error)) if count == 0 => return Err(error),
                other => {
                    self.pending = other;
                    break;
                }
            };
            // SAFETY: the record fits the live caller-owned carrier's spare bytes.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    record.bytes.as_ptr(),
                    buffer.data.add(len),
                    record.len,
                )
            };
            len += record.len;
            count += 1;
        }
        buffer.len = len;
        Ok(count)
    }

    fn next(&mut self) -> io::Result<Option<Record>> {
        if self.exhausted {
            return Ok(None);
        }
        loop {
            // SAFETY: the listing owns the live stream and exclusively advances it.
            // readdir reports EOF and errors with null, distinguishing them by errno.
            let entry = unsafe {
                *libc::__errno_location() = 0;
                libc::readdir(self.stream.as_ptr()).as_ref()
            };
            let Some(entry) = entry else {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(0) {
                    return Err(error);
                }
                self.exhausted = true;
                return Ok(None);
            };
            // SAFETY: readdir returns a NUL-terminated name valid until next readdir.
            let name = unsafe { CStr::from_ptr(entry.d_name.as_ptr()) };
            let bytes = name.to_bytes();
            if matches!(bytes, b"." | b"..") {
                continue;
            }
            if bytes.len() > MAX_RECORD - HEADER {
                return Err(io::Error::from_raw_os_error(libc::ENAMETOOLONG));
            }
            let kind = match entry.d_type {
                libc::DT_REG => 0,
                libc::DT_DIR => 1,
                libc::DT_LNK => 2,
                libc::DT_UNKNOWN => {
                    // SAFETY: the root descriptor stays live with this stream.
                    let fd = unsafe { libc::dirfd(self.stream.as_ptr()) };
                    let Some(st) = leaf_stat(fd, name)? else {
                        continue;
                    };
                    file_kind(st.st_mode)
                }
                _ => 3,
            };
            let mut record = Record {
                bytes: [0; MAX_RECORD],
                len: HEADER + bytes.len(),
            };
            let name_len = u16::try_from(bytes.len()).unwrap();
            record.bytes[0] = kind;
            record.bytes[1..HEADER].copy_from_slice(&name_len.to_le_bytes());
            record.bytes[HEADER..record.len].copy_from_slice(bytes);
            return Ok(Some(record));
        }
    }
}

impl Drop for Listing {
    fn drop(&mut self) {
        // SAFETY: the stream is live and exclusively owned. closedir also closes its fd.
        unsafe { libc::closedir(self.stream.as_ptr()) };
    }
}
