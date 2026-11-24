use super::{Inner, empty_buffer, failed, request, sink};
use crate::provider::{Buffer, CLOSE, IoError, OPEN, READ_AT, WRITE};
use std::ptr;

/// Owns a file from successful OPEN until CLOSE. The provider must remain
/// pinned in its run for the owner's lifetime.
pub(super) struct ProviderFile(Option<Inner>);

impl ProviderFile {
    pub(super) unsafe fn open(dir: Inner, mode: u8, name: &[u8]) -> Result<Self, IoError> {
        // SAFETY: the opener owns the pinned provider.
        let o = unsafe { sink(dir, request(OPEN, mode, 0, name, empty_buffer())) };
        match failed(&o) {
            Some(e) => Err(e),
            None => Ok(Self(Some(Inner {
                provider: dir.provider,
                handle: o.value,
            }))),
        }
    }

    pub(super) fn handle(&self) -> u64 {
        self.0.unwrap().handle
    }

    pub(super) fn write(&self, bytes: &[u8]) -> Result<(), IoError> {
        let mut r = request(
            WRITE,
            0,
            0,
            &[],
            Buffer {
                data: bytes.as_ptr().cast_mut(),
                len: bytes.len(),
                capacity: 0,
            },
        );
        r.path = ptr::null();
        // SAFETY: the file and the borrowed input are pinned through delivery.
        let o = unsafe { sink(self.0.unwrap(), r) };
        failed(&o).map_or(Ok(()), Err)
    }

    pub(super) fn read_at(&self, offset: u64, bytes: &mut [u8]) -> Result<usize, IoError> {
        let offset = offset.to_le_bytes();
        let r = request(
            READ_AT,
            0,
            0,
            &offset,
            Buffer {
                data: bytes.as_mut_ptr(),
                len: 0,
                capacity: bytes.len(),
            },
        );
        // SAFETY: the file and the writable buffer are pinned through delivery.
        let o = unsafe { sink(self.0.unwrap(), r) };
        failed(&o).map_or(Ok(o.value as usize), Err)
    }

    pub(super) fn close(&mut self) -> Result<(), IoError> {
        let Some(file) = self.0.take() else {
            return Ok(());
        };
        // SAFETY: the file remains pinned through delivery.
        let o = unsafe { sink(file, request(CLOSE, 0, 0, &[], empty_buffer())) };
        failed(&o).map_or(Ok(()), Err)
    }
}
impl Drop for ProviderFile {
    fn drop(&mut self) {
        let _ = self.close();
    }
}
