//! `Latest[T]` roots (CONC-13): one swappable `Arc[T]` holder shared by its handles.
//! The root record and its cleanup pin share one admitted block. `read` and `publish` are scheduler events
//! that record no value. The associated Arc share or release commits
//! immediately after its Latest event, before another task or input.
//! The last handle's cleanup releases the root through the compiler's
//! resumable cleanup of the `Arc[T]` place, stored at creation as a channel
//! stores its item's, on the active drain; the record's refund follows.
use crate::{
    Context,
    block::Block,
    counted::{self, Holder, Recipe, Worklist},
    tasks,
};
use hirsel_core_layout::charge;
use std::ptr::{self, NonNull};

#[repr(C)]
pub struct Latest {
    /// The current root: one `Arc[T]` holder, a node pointer.
    /// Null once the last handle's cleanup has handed the root over.
    root: *mut u8,
    /// With `id`, the cleanup's frame once the last handle has gone.
    handles: u64,
    id: u64,
    /// The compiler's cleanup of an `Arc[T]` place; it cannot fail (T is shareable).
    clean: Recipe,
    _pin: Option<crate::code::ExecutionPin>,
}
// The guest pays for the four root words. The native recipe pin follows them.
const _: () = assert!(std::mem::offset_of!(Latest, _pin) as u64 == charge::LATEST_ROOT);
const _: () = assert!(
    std::mem::offset_of!(Latest, id) == std::mem::offset_of!(Latest, handles) + 8
        && size_of::<Holder>() == 16
);

#[repr(C)]
pub struct New {
    pub latest: *mut Latest,
    pub available: u64,
}

/// Admit and publish a root holding `node`. On refusal the caller keeps `node`.
/// # Safety
/// Context is exclusively owned; `node` is a live `Arc[T]` holder the caller
/// transfers only on success; `clean` is the compiler's `Arc[T]` place cleanup.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_latest_new(
    context: *mut Context,
    node: *mut u8,
    clean: Recipe,
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
        tasks::before_commit();
        let id = tasks::identity();
        latest.write(Latest {
            root: node,
            handles: 1,
            id,
            clean,
            _pin: crate::code::ExecutionPin::pin_pc(Some(&*context), clean as usize),
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
        tasks::before_commit();
        (*latest).handles = (*latest).handles.checked_add(1).unwrap_or_else(|| {
            crate::run_stop::raise(crate::run_stop::Stop::from_static(b"LatestHandles"))
        });
        tasks::event(tasks::SENDERS, (*latest).id, (*latest).handles);
        latest
    }
}
/// Install `node` as the root and release the old holder (CONC-13).
/// # Safety
/// The context belongs to this task. A live handle pins the root and the caller
/// transfers the live holder `node`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_latest_publish(
    context: *mut Context,
    latest: *mut Latest,
    node: *mut u8,
) {
    // SAFETY: the live handle pins the root and its exact Arc cleanup recipe.
    unsafe {
        tasks::before_commit();
        let mut old = std::mem::replace(&mut (*latest).root, node);
        tasks::event(tasks::LATEST_PUBLISH, (*latest).id, 0);
        counted::drain_place(context, (&raw mut old).cast(), (*latest).clean);
    }
}
/// Return an owned share of the current root's node (CONC-13).
/// # Safety
/// A live handle pins the root.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_latest_read(latest: *mut Latest) -> *mut u8 {
    // SAFETY: the root holder pins its node through the paired share.
    unsafe {
        tasks::before_commit();
        let node = NonNull::new_unchecked((*latest).root);
        tasks::event(tasks::LATEST_READ, (*latest).id, 0);
        counted::share(node, true);
        node.as_ptr()
    }
}
/// Give up one handle; the last one's cleanup takes over the record, whose
/// counter and identity words become its frame.
unsafe fn latest_begin(latest: *mut Latest) -> Option<*mut Holder> {
    // SAFETY: the exact handle count pins the record until the last handle.
    unsafe {
        tasks::before_commit();
        (*latest).handles -= 1;
        let remaining = (*latest).handles;
        tasks::event(tasks::SENDERS, (*latest).id, remaining);
        (remaining == 0).then(|| (&raw mut (*latest).handles).cast::<Holder>())
    }
}
/// Release the root's holder, then refund the record.
unsafe fn latest_step(context: *mut Context, holder: *mut Holder, list: *mut Worklist) -> bool {
    // SAFETY: the frame is the last handle's; the record is this cleanup's alone.
    unsafe {
        let latest = holder
            .cast::<u8>()
            .sub(std::mem::offset_of!(Latest, handles))
            .cast::<Latest>();
        // One step: past it once the root has been handed over.
        let mut cursor = u64::from((*latest).root.is_null());
        if !((*latest).clean)(context, (&raw mut (*latest).root).cast(), &mut cursor, list) {
            (*latest).root = ptr::null_mut();
            return false;
        }
        ptr::drop_in_place(latest);
        drop(Block::from_raw(
            NonNull::new_unchecked(latest.cast()),
            charge::LATEST_ROOT,
        ));
        true
    }
}
/// Transfers one handle. The last one releases the root's holder and refunds
/// the root.
/// # Safety
/// The caller transfers a live handle; context is the cleaning task's.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_latest_drop(context: *mut Context, latest: *mut Latest) {
    // SAFETY: as latest_begin; a shareable root's release cannot fail.
    unsafe {
        if let Some(holder) = latest_begin(latest) {
            counted::drain_holder(context, holder, latest_step, 0);
        }
    }
}
/// Give up one handle inside a running drain; the last one's cleanup is handed
/// over at `point`. Returns whether it was.
/// # Safety
/// As hirsel_core_latest_drop; `list` is the running drain's.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_latest_release(
    latest: *mut Latest,
    list: *mut Worklist,
    point: u64,
) -> bool {
    // SAFETY: as hirsel_core_latest_drop.
    unsafe {
        let Some(holder) = latest_begin(latest) else {
            return false;
        };
        (*list).stage_holder(holder, latest_step, point);
        true
    }
}
