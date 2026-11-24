//! The link's code descriptors, by code word: a recorded spawn writes its
//! helper's entry identity from them (docs/recording.md, "Helper stream
//! header"). A plain `fn` is only its code word, and a helper's `f` may have
//! come through any parameter, so the recording entry hands the runtime an
//! index of every descriptor the link defines: a `u64` count, then each
//! descriptor's address. A descriptor is `{code, length, revision}` then
//! its identity bytes (PROG-9).
use std::sync::atomic::{AtomicPtr, Ordering};

static INDEX: AtomicPtr<u64> = AtomicPtr::new(std::ptr::null_mut());

/// Installs the entry's descriptor index for the run.
///
/// # Safety
/// `index` is null or a static index as above, pinned for the process.
pub(crate) unsafe fn install(index: *const u64) {
    INDEX.store(index.cast_mut(), Ordering::Release);
}

/// The identity bytes of the function whose code word is `code`, or `None`
/// when no descriptor of the link has it: a closure, which has no PROG-9
/// identity, or a run with no index.
pub(crate) fn identity_of(code: u64) -> Option<&'static [u8]> {
    let index = INDEX.load(Ordering::Acquire);
    if index.is_null() || code == 0 {
        return None;
    }
    // SAFETY: install's contract: a pinned static index of descriptors.
    unsafe {
        let count = index.read() as usize;
        let rows = std::slice::from_raw_parts(index.add(1).cast::<*const u64>(), count);
        rows.iter().find(|&&d| d.read() == code).map(|&d| {
            let len = d.add(1).read() as usize;
            std::slice::from_raw_parts(d.add(3).cast::<u8>(), len)
        })
    }
}
