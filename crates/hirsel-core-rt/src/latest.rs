//! `Latest[T]` roots (CONC-13): one swappable `Arc[T]` holder shared by its handles.
//! The root record is the payload of its own admitted block, so the charge is
//! exactly the bookkeeping it holds. `read` and `publish` are scheduler events
//! that record no value; they hand the node back to the typed kernel, which
//! shares it (`read`) or releases the old one (`publish`) as an ordinary `Arc`.
//! The last handle's cleanup drops the root through the compiler's `Arc[T]`
//! drop callback, stored at creation as a channel stores its item's drop.
use crate::{Context, block::Block, tasks};
use hirsel_core_layout::charge;
use std::ptr::{self, NonNull};

#[repr(C)]
pub struct Latest {
    /// The current root: one `Arc[T]` holder, a node pointer.
    root: *mut u8,
    handles: u64,
    id: u64,
    /// The compiler's drop for an `Arc[T]` place; it cannot fail (T is shareable).
    drop: unsafe extern "C" fn(*mut Context, *mut u8) -> u8,
}
const _: () = assert!(size_of::<Latest>() as u64 == charge::LATEST_ROOT);

#[repr(C)]
pub struct New {
    pub latest: *mut Latest,
    pub available: u64,
}

/// Admit and publish a root holding `node`. On refusal the caller keeps `node`.
/// # Safety
/// Context is exclusively owned; `node` is a live `Arc[T]` holder the caller
/// transfers only on success; `drop` is the compiler's `Arc[T]` drop.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_latest_new(
    context: *mut Context,
    node: *mut u8,
    drop: unsafe extern "C" fn(*mut Context, *mut u8) -> u8,
) -> New {
    // SAFETY: the block is fresh, aligned and exactly the record's size.
    unsafe {
        let block = match Block::new(
            &*context,
            Some(charge::LATEST_ROOT),
            size_of::<Latest>(),
            align_of::<Latest>(),
        ) {
            Ok(block) => block,
            Err(error) => {
                return New {
                    latest: ptr::null_mut(),
                    available: error.available,
                };
            }
        };
        let latest = block.into_raw().as_ptr().cast::<Latest>();
        let id = tasks::identity();
        latest.write(Latest {
            root: node,
            handles: 1,
            id,
            drop,
        });
        tasks::event(tasks::SENDERS, id, 1);
        New {
            latest,
            available: 0,
        }
    }
}
/// A new handle. Admits nothing.
/// # Safety
/// A live handle pins the root.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_latest_handle(latest: *mut Latest) -> *mut Latest {
    // SAFETY: the live handle pins the exact counter through its event.
    unsafe {
        (*latest).handles = (*latest)
            .handles
            .checked_add(1)
            .unwrap_or_else(|| crate::process::stop(b"LatestHandles"));
        tasks::event(tasks::SENDERS, (*latest).id, (*latest).handles);
        latest
    }
}
/// Install `node` as the root and return the old root's holder, which the caller
/// now owns and releases. Never waits, fails or observes cancellation.
/// # Safety
/// A live handle pins the root; the caller transfers the live holder `node`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_latest_publish(latest: *mut Latest, node: *mut u8) -> *mut u8 {
    // SAFETY: no suspension between the swap and its event.
    unsafe {
        let old = std::mem::replace(&mut (*latest).root, node);
        tasks::event(tasks::LATEST_PUBLISH, (*latest).id, 0);
        old
    }
}
/// The current root's node, borrowed: the caller shares it before any other event.
/// # Safety
/// A live handle pins the root.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_latest_read(latest: *mut Latest) -> *mut u8 {
    // SAFETY: the live handle pins the root holder until the caller's share.
    unsafe {
        tasks::event(tasks::LATEST_READ, (*latest).id, 0);
        (*latest).root
    }
}
/// Transfers one handle. The last one drops the root's holder and refunds the root.
/// # Safety
/// The caller transfers a live handle; context is the cleaning task's.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_latest_drop(context: *mut Context, latest: *mut Latest) {
    // SAFETY: the exact handle count pins the record until the last handle.
    unsafe {
        (*latest).handles -= 1;
        tasks::event(tasks::SENDERS, (*latest).id, (*latest).handles);
        if (*latest).handles == 0 {
            // A shareable payload holds no resource, so its release cannot fail.
            ((*latest).drop)(context, (&raw mut (*latest).root).cast());
            drop(Block::from_raw(
                NonNull::new_unchecked(latest.cast()),
                charge::LATEST_ROOT,
            ));
        }
    }
}
