//! The two-event model of one Direct segment write (BL11-BL14): the bytes
//! become readable at one later callback and complete (durable) at another.
use super::Bridge;
use std::ffi::c_void;

fn overwrite(image: &[u8], at: usize, data: &[u8]) -> Vec<u8> {
    let mut image = image.to_vec();
    image.resize(image.len().max(at + data.len()), 0);
    image[at..at + data.len()].copy_from_slice(data);
    image
}

pub(super) struct DirectWrite {
    inode: u64,
    handle: u64,
    at: usize,
    data: Vec<u8>,
    before_readable: Option<Vec<u8>>,
}

pub(super) struct LateWriteSchedule {
    creation: bool,
    fails: bool,
    readable: u64,
    complete: u64,
    power_cut: u64,
    point: u64,
    pub(super) pending: Option<DirectWrite>,
}

impl Bridge {
    pub(super) fn advance_late_write(&mut self) {
        let Some(late) = &mut self.late else { return };
        late.point += 1;
        if late.point == late.power_cut {
            self.fs.restart();
            self.locks.clear();
            late.pending = None;
            self.crashed = true;
            return;
        }
        let Some(write) = &mut late.pending else {
            return;
        };
        if late.point == late.readable {
            let before = self.fs.read_inode(write.inode).unwrap().to_vec();
            let visible = overwrite(&before, write.at, &write.data);
            self.fs.write_inode(write.inode, visible);
            write.before_readable = Some(before);
        }
        if late.point == late.complete {
            assert!(write.before_readable.is_some());
            if !late.fails {
                let durable = overwrite(self.fs.durable_inode(write.inode), write.at, &write.data);
                self.fs.sync_image(write.inode, durable);
            }
            self.locks.remove(&(write.inode, write.handle));
            late.pending = None;
        }
    }

    /// Holds the write the schedule names and crashes the writing process.
    pub(super) fn hold_late_write(
        &mut self,
        inode: u64,
        handle: u64,
        at: usize,
        data: &[u8],
    ) -> bool {
        let Some(late) = &mut self.late else {
            return false;
        };
        if late.creation != self.fs.read_inode(inode).unwrap().is_empty() {
            return false;
        }
        assert!(late.pending.is_none());
        late.pending = Some(DirectWrite {
            inode,
            handle,
            at,
            data: data.to_vec(),
            before_readable: None,
        });
        self.crashed = true;
        true
    }

    /// A sync cannot make an incomplete write durable.
    pub(super) fn sync_late_write(&mut self, inode: u64) -> bool {
        let Some(write) = self.late.as_ref().and_then(|late| late.pending.as_ref()) else {
            return false;
        };
        if write.inode != inode {
            return false;
        }
        if let Some(before) = &write.before_readable {
            self.fs.sync_image(inode, before.clone());
        } else {
            self.fs.sync_inode(inode);
        }
        true
    }

    /// The in-flight write keeps its descriptor's lock until it completes.
    pub(super) fn late_write_holds(&self, handle: u64) -> bool {
        let pending = self.late.as_ref().and_then(|late| late.pending.as_ref());
        pending.is_some_and(|write| write.handle == handle)
    }
}

/// # Safety
/// The C wrapper has exclusive access to the live bridge.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn store_crash_late_write(
    p: *mut c_void,
    creation: u8,
    fails: u8,
    readable: u64,
    complete: u64,
    power_cut: u64,
) {
    assert!(0 < readable && readable < complete);
    let bridge = unsafe { &mut *p.cast::<Bridge>() };
    bridge.late = Some(LateWriteSchedule {
        creation: creation != 0,
        fails: fails != 0,
        readable,
        complete,
        power_cut,
        point: 0,
        pending: None,
    });
}

/// The new owner of a late-write case runs. Its lock polls' sleeps end at
/// once, so the polls advance the schedule to the write's completion and the
/// poll after it.
///
/// # Safety
/// `p` is the live bridge, accessed only by the serialized provider callbacks.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn store_crash_late_polls(p: *const c_void) -> u8 {
    let bridge = unsafe { &*p.cast::<Bridge>() };
    u8::from(bridge.phase == 3 && bridge.late.is_some())
}

/// The schedule points the new owner's run reached.
///
/// # Safety
/// `p` is live, and no provider callback is active.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn store_crash_late_points(p: *const c_void) -> u64 {
    let bridge = unsafe { &*p.cast::<Bridge>() };
    bridge.late.as_ref().map_or(0, |late| late.point)
}
