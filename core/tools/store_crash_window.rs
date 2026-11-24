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

/// The end of the frames that start at `from`; padding (kind 0) ends them.
pub(super) fn frames_end(bytes: &[u8], mut from: usize) -> usize {
    while from + 16 <= bytes.len() && bytes[from..from + 2] != [0, 0] {
        let len = u32::from_le_bytes(bytes[from + 4..from + 8].try_into().unwrap());
        from += 16 + len as usize;
    }
    from
}

impl Bridge {
    /// A window step: `host.zero_at` over [at, at + count).
    pub(super) fn window_zero(&mut self, at: usize, count: usize) -> Option<Outcome> {
        let window = self.window.as_mut()?;
        assert!(
            !matches!(window.pending, Some(JournalWrite::Frames(_))),
            "window step began before frame sync"
        );
        let target = ((window.synced_end - 72) / 4096 * 4096).min(4194304);
        assert!(
            at + count <= window.synced_end.next_multiple_of(4096) + target,
            "window step exceeded the target"
        );
        if self.sync_failure == WINDOW_CHUNK_FAILURE {
            self.failed_syncs += 1;
            assert_eq!(self.failed_syncs, 1, "store retried a window write");
            return Some(Self::error(5));
        }
        window.pending = Some(JournalWrite::Zeros);
        None
    }

    /// A direct group write: it starts at the block holding the frame end,
    /// carries that block's bytes unchanged, then the group's frames.
    pub(super) fn window_write(
        &mut self,
        image: &[u8],
        cursor: usize,
        bytes: &[u8],
    ) -> Option<Outcome> {
        let window = self.window.as_mut()?;
        let tail = window.synced_end - cursor;
        let end = cursor + frames_end(bytes, tail);
        if cursor == 0 && end == 72 {
            // The creation block: HEADER, then zeros.
            return None;
        }
        assert!(
            window.pending.is_none(),
            "group overlapped an unsynced write"
        );
        assert_eq!(
            cursor,
            window.synced_end / 4096 * 4096,
            "group starts at the frame end's block"
        );
        assert_eq!(
            bytes[..tail],
            image[cursor..window.synced_end],
            "group rewrote its tail image"
        );
        assert!(end > window.synced_end, "group carried no frame");
        window.pending = Some(JournalWrite::Frames(end));
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
