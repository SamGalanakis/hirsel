//! Exact u64 counts and an intrusive dead-node worklist. The event provider
//! serializes Arc operations before calling these entry points (CONC-10).

use crate::{Context, block};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering, fence};

/// Generated resumable mechanical glue. Cursor starts at zero in native node
/// bookkeeping. Clean fields in spec order; advance the cursor, enqueue at most
/// one descendant, then return false to resume after that descendant. Return
/// true only when all fields are clean and nothing was enqueued in this call.
/// Never recurse, allocate, fail, run user code, or free the current node.
pub type Recipe = unsafe extern "C" fn(*mut Context, *mut u8, *mut u64, *mut Worklist) -> bool;

pub struct Worklist {
    pub(crate) head: Option<NonNull<u8>>,
}

/// # Safety
/// The emitter initializes a newly allocated node, count at 0 and payload at 8.
pub unsafe fn initialize(node: NonNull<u8>, atomic: bool) {
    // SAFETY: node has fresh aligned storage for one count word.
    unsafe {
        if atomic {
            node.cast::<AtomicU64>().as_ptr().write(AtomicU64::new(1));
        } else {
            node.cast::<u64>().as_ptr().write(1);
        }
    }
}

/// # Safety
/// The caller holds a live holder. Rc operations are task-local; Arc operations
/// occur in the provider's event order. Payload mutation remains forbidden.
pub unsafe fn share(node: NonNull<u8>, atomic: bool) {
    // SAFETY: a live holder keeps the initialized count word alive.
    unsafe {
        if atomic {
            if node
                .cast::<AtomicU64>()
                .as_ref()
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
                .is_err()
            {
                std::process::abort();
            }
        } else {
            let count = &mut *node.cast::<u64>().as_ptr();
            *count = count
                .checked_add(1)
                .unwrap_or_else(|| std::process::abort());
        }
    }
}

/// # Safety
/// The caller holds a live holder and meets share's task/event discipline.
/// A true result transfers exclusive payload custody; the caller moves it out
/// and frees the node. A false result leaves the original holder unchanged.
pub unsafe fn try_unwrap(node: NonNull<u8>, atomic: bool) -> bool {
    // SAFETY: the caller's holder pins the initialized count word.
    unsafe {
        if atomic {
            node.cast::<AtomicU64>().as_ref().load(Ordering::Acquire) == 1
        } else {
            node.cast::<u64>().as_ptr().read() == 1
        }
    }
}

/// # Safety
/// Consumes one live holder. Charge and recipe describe this node. The recipe
/// obeys the mechanical, nonrecursive contract above. The list is exclusively used.
pub unsafe fn enqueue(
    list: &mut Worklist,
    node: NonNull<u8>,
    atomic: bool,
    charge: u64,
    recipe: Recipe,
) {
    // SAFETY: the transferred holder keeps this node's initialized count alive.
    let last = unsafe {
        if atomic {
            let last = node
                .cast::<AtomicU64>()
                .as_ref()
                .fetch_sub(1, Ordering::Release)
                == 1;
            if last {
                fence(Ordering::Acquire);
            }
            last
        } else {
            let count = &mut *node.cast::<u64>().as_ptr();
            *count -= 1;
            *count == 0
        }
    };
    if last {
        // SAFETY: the last holder owns the dead node exclusively.
        unsafe {
            block::push_dead(list, node, charge, recipe, 8);
        }
    }
}

/// # Safety
/// As for enqueue, plus a live task context. Each generated recipe enqueues
/// descendants rather than invoking release recursively.
pub unsafe fn release(
    context: *mut Context,
    node: NonNull<u8>,
    atomic: bool,
    charge: u64,
    recipe: Recipe,
) {
    let mut list = Worklist { head: None };
    // SAFETY: caller transfers one holder and its generated mechanical recipe.
    unsafe {
        enqueue(&mut list, node, atomic, charge, recipe);
        while let Some(node) = list.head {
            let dead = block::dead(node);
            let recipe = (*dead).recipe;
            let offset = (*dead).payload_offset;
            if recipe(
                context,
                node.as_ptr().add(offset),
                &mut (*dead).cursor,
                &mut list,
            ) {
                list.head = (*dead).next;
                block::free(node, (*dead).charge);
            }
        }
    }
}
