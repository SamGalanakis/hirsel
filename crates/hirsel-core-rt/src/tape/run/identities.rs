//! The link's code descriptors, by code word: a recorded spawn writes its
//! helper's entry identity from them (docs/recording.md, "Helper stream
//! header"). A plain `fn` is only its code word, and a helper's `f` may have
//! come through any parameter, so the recording entry hands the runtime an
//! index of every descriptor the link defines: a `u64` count, then each
//! descriptor's address. A descriptor is `{code, length, revision}` then
//! its identity bytes (PROG-9).
//!
//! Beside it the entry hands the link's spawn table: a `u64` count, then per
//! spawnable function its descriptor's address and the address of the static
//! `SpawnEntry` for its `(X, R)` (the bridge, layout and both crossing
//! descriptors a spawn of it passes), so a helper can be started from its
//! entry identity alone.
use crate::units_lang::SpawnEntry;
use std::sync::atomic::{AtomicPtr, Ordering};

static INDEX: AtomicPtr<u64> = AtomicPtr::new(std::ptr::null_mut());
static SPAWNS: AtomicPtr<u64> = AtomicPtr::new(std::ptr::null_mut());

/// Installs the entry's descriptor index for the run.
///
/// # Safety
/// `index` is null or a static index as above, pinned for the process.
pub(crate) unsafe fn install(index: *const u64) {
    INDEX.store(index.cast_mut(), Ordering::Release);
}

/// Installs the entry's spawn table for the run.
///
/// # Safety
/// `table` is null or a static spawn table as above, pinned for the process.
pub(crate) unsafe fn install_spawns(table: *const u64) {
    SPAWNS.store(table.cast_mut(), Ordering::Release);
}

/// The descriptor's identity bytes.
///
/// # Safety
/// `d` is a static descriptor as above.
unsafe fn identity(d: *const u64) -> &'static [u8] {
    // SAFETY: forwarded.
    unsafe {
        let len = d.add(1).read() as usize;
        std::slice::from_raw_parts(d.add(3).cast::<u8>(), len)
    }
}

/// The spawn entry of the function whose identity is `identity`, or `None`
/// when the link's spawn table has none: a function no spawn of the link can
/// start, or a run with no table.
pub(crate) fn spawn_entry(identity_bytes: &[u8]) -> Option<SpawnEntry> {
    let table = SPAWNS.load(Ordering::Acquire);
    if table.is_null() {
        return None;
    }
    // SAFETY: install_spawns' contract: a pinned static table of rows.
    unsafe { find(table, identity_bytes) }
}

/// [`spawn_entry`] in `table`.
///
/// # Safety
/// `table` is a spawn table as above, live for the call.
unsafe fn find(table: *const u64, identity_bytes: &[u8]) -> Option<SpawnEntry> {
    // SAFETY: forwarded.
    unsafe {
        let count = table.read() as usize;
        let rows = std::slice::from_raw_parts(table.add(1).cast::<[*const u64; 2]>(), count);
        rows.iter()
            .find(|&&[d, _]| identity(d) == identity_bytes)
            .map(|&[_, entry]| entry.cast::<SpawnEntry>().read())
    }
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
        rows.iter()
            .find(|&&d| d.read() == code)
            .map(|&d| identity(d))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::{TaskLayout, ValueLayout};
    use crate::units_lang::Cross;

    unsafe extern "C" fn bridge(_: *mut crate::Context, _: *mut u8, _: *mut u8) -> u8 {
        0
    }

    /// A replay looks a helper's entry up by the identity its stream
    /// recorded; one the link's table does not hold is not some other row
    /// (replay's `Diverged(Op)`), while the one it holds is its own row.
    #[test]
    fn an_identity_outside_the_table_has_no_entry() {
        let value = |bytes| ValueLayout {
            bytes,
            align: 8,
            logical: bytes as u64,
            clean: None,
        };
        let cross = |bytes| Cross {
            bytes,
            owners: std::ptr::null(),
            count: 0,
            at: 0,
            len: bytes,
        };
        let row = SpawnEntry {
            bridge,
            adapter: 0,
            layout: TaskLayout {
                input: value(16),
                result: value(1),
            },
            x: cross(16),
            r: cross(1),
        };
        // `{code, length, revision}` then the identity bytes.
        let descriptor: [u64; 4] = [1, 8, 0, u64::from_le_bytes(*b"app/f()\0")];
        let table: [u64; 3] = [1, descriptor.as_ptr() as u64, (&raw const row) as u64];
        // SAFETY: a table of one live row.
        unsafe {
            assert!(find(table.as_ptr(), b"app/g()\0").is_none());
            assert!(find(table.as_ptr(), b"app/f").is_none());
            let found = find(table.as_ptr(), b"app/f()\0").expect("its own row");
            assert!(found.same(&row));
        }
    }
}
