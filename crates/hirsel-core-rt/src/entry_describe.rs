use super::*;

pub(super) unsafe extern "C" fn describe_output(
    state: *mut u8,
    data: *const u8,
    len: usize,
) -> bool {
    // SAFETY: the run pins its response view and the launch owner pins its sink.
    unsafe {
        let process = &*state.cast::<OsProcess>();
        let Some(out) = &process.describe_out else {
            return false;
        };
        let (fd, bytes) = (out.as_raw_fd(), bytes(View { data, len }));
        off_slot(HostCall::System, || crate::os::write_all(fd, bytes)).error == IoError::OK
    }
}
