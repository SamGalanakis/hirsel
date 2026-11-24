use super::{Bridge, Outcome};
use std::ffi::c_void;

const WINDOW_CHUNK_FAILURE: u8 = 10;
pub(super) const WINDOW_SYNC_FAILURE: u8 = 11;

// The fixture observes the actual writes and syncs, independent of Store's
// counters. At most one segment receives writes during its workload.
pub(super) struct WindowObservation {
    synced_end: usize,
    pending: Option<JournalWrite>,
}

enum JournalWrite {
    Frames(usize),
    Zeros,
}

impl WindowObservation {
    pub(super) fn syncs_zeros(&self) -> bool {
        matches!(self.pending, Some(JournalWrite::Zeros))
    }

    pub(super) fn synced(&mut self) {
        if let Some(JournalWrite::Frames(end)) = self.pending.take() {
            self.synced_end = end;
        }
    }
}

impl Bridge {
    pub(super) fn window_write(&mut self, cursor: usize, bytes: &[u8]) -> Option<Outcome> {
        let window = self.window.as_mut()?;
        if bytes.iter().all(|&byte| byte == 0) {
            assert!(
                !matches!(window.pending, Some(JournalWrite::Frames(_))),
                "window step began before frame sync"
            );
            let target = ((window.synced_end - 72) / 4096 * 4096).min(4194304);
            assert!(
                cursor + bytes.len() <= window.synced_end + target,
                "window step exceeded the target"
            );
            if self.sync_failure == WINDOW_CHUNK_FAILURE {
                self.failed_syncs += 1;
                assert_eq!(self.failed_syncs, 1, "store retried a window write");
                return Some(Self::error(5));
            }
            window.pending = Some(JournalWrite::Zeros);
        } else {
            assert!(
                window.pending.is_none(),
                "group overlapped an unsynced write"
            );
            assert_eq!(cursor, window.synced_end, "group starts at the frame end");
            window.pending = Some(JournalWrite::Frames(cursor + bytes.len()));
        }
        None
    }
}

/// # Safety
/// `p` is live and quiescent, as for store_crash_floor.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn store_crash_window(p: *mut c_void) {
    // SAFETY: the wrapper owns p before any provider callback runs.
    unsafe {
        (*p.cast::<Bridge>()).window = Some(WindowObservation {
            synced_end: 72,
            pending: None,
        })
    };
}
