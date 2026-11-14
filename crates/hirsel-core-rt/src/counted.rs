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
/// The emitter initializes a newly allocated PLAIN or GRAPH node, count at 0 and payload at 8.
pub unsafe fn initialize(node: NonNull<u8>, atomic: bool) {
    // SAFETY: node has fresh aligned storage for one count word.
    unsafe {
        if atomic {
            node.cast::<AtomicU64>().as_ptr().write(AtomicU64::new(1));
            crate::tasks::event(crate::tasks::ARC_NEW, 0, 1);
        } else {
            node.cast::<u64>().as_ptr().write(1);
        }
    }
}

/// # Safety
/// The caller holds a live holder. Rc operations are task-local; Arc operations
/// occur in the provider's event order. Payload mutation remains forbidden.
#[inline]
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
                crate::process::stop(b"CountOverflow");
            }
            crate::tasks::event(
                crate::tasks::ARC_SHARE,
                0,
                node.cast::<AtomicU64>().as_ref().load(Ordering::Relaxed),
            );
        } else {
            let count = &mut *node.cast::<u64>().as_ptr();
            *count = count
                .checked_add(1)
                .unwrap_or_else(|| crate::process::stop(b"CountOverflow"));
        }
    }
}

/// # Safety
/// The caller holds a live holder and meets share's task/event discipline.
/// A true result transfers exclusive payload custody; the caller moves it out
/// and frees the node. A false result leaves the original holder unchanged.
#[inline]
pub unsafe fn try_unwrap(node: NonNull<u8>, atomic: bool) -> bool {
    // SAFETY: the caller's holder pins the initialized count word.
    unsafe {
        if atomic {
            let unique = node.cast::<AtomicU64>().as_ref().load(Ordering::Acquire) == 1;
            crate::tasks::event(crate::tasks::ARC_UNWRAP, 0, u64::from(unique));
            unique
        } else {
            node.cast::<u64>().as_ptr().read() == 1
        }
    }
}

/// # Safety
/// Consumes one live holder. Charge and recipe describe this node. The recipe
/// obeys the mechanical, nonrecursive contract above. The list is exclusively used.
#[inline]
pub unsafe fn enqueue(
    list: &mut Worklist,
    node: NonNull<u8>,
    atomic: bool,
    charge: u64,
    recipe: Recipe,
) {
    // SAFETY: caller transfers a live holder under the node's count discipline.
    let last = unsafe { decrement(node, atomic) };
    if last {
        // SAFETY: the last holder owns the dead node exclusively.
        unsafe {
            if block::funding(node).native_tag() & 3 == block::PLAIN as usize {
                block::free(node, charge);
            } else {
                block::push_dead(list, node, charge, recipe, 8);
            }
        }
    }
}

/// Consume one holder, returning whether the emitter must run last-release glue.
/// # Safety
/// A live holder is transferred under the Rc/Arc event discipline.
#[inline(always)]
pub unsafe fn decrement(node: NonNull<u8>, atomic: bool) -> bool {
    // SAFETY: the transferred holder keeps this node's initialized count alive.
    unsafe {
        if atomic {
            let last = node
                .cast::<AtomicU64>()
                .as_ref()
                .fetch_sub(1, Ordering::Release)
                == 1;
            if last {
                fence(Ordering::Acquire);
            }
            crate::tasks::event(crate::tasks::ARC_RELEASE, 0, u64::from(last));
            last
        } else {
            let count = &mut *node.cast::<u64>().as_ptr();
            *count = count.wrapping_sub(1);
            *count == 0
        }
    }
}

/// # Safety
/// As for enqueue, plus a live task context. Each generated recipe enqueues
/// descendants rather than invoking release recursively.
#[inline(always)]
pub unsafe fn release(
    context: *mut Context,
    node: NonNull<u8>,
    atomic: bool,
    charge: u64,
    recipe: Recipe,
) {
    // SAFETY: caller transfers one live holder.
    if unsafe { decrement(node, atomic) } {
        // SAFETY: decrement transferred the unique dead node to cleanup.
        unsafe {
            release_last(context, node, charge, recipe);
        }
    }
}

/// Slow path after an inline decrement has returned true.
/// # Safety
/// Count is zero; caller owns this node, its exact charge and mechanical recipe.
#[inline(never)]
pub unsafe fn release_last(context: *mut Context, node: NonNull<u8>, charge: u64, recipe: Recipe) {
    // SAFETY: a PLAIN counted node's terminal payload has no cleanup to resume.
    if unsafe { block::funding(node).native_tag() & 3 == block::PLAIN as usize } {
        // SAFETY: count is zero and caller transfers its exact original charge.
        unsafe { block::free(node, charge) };
        return;
    }
    let mut list = Worklist { head: None };
    // SAFETY: caller transfers one holder and its generated mechanical recipe.
    unsafe {
        block::push_dead(&mut list, node, charge, recipe, 8);
        drain(context, &mut list);
    }
}
/// # Safety
/// All list entries carry initialized intrusive frames and exact recipes.
pub(crate) unsafe fn drain(context: *mut Context, list: &mut Worklist) {
    // SAFETY: recipes yield at most one descendant and keep unfinished parents live.
    unsafe {
        while let Some(node) = list.head {
            let (recipe, payload, cursor, next, charge) = block::resume_state(node);
            if recipe(context, payload, cursor, list) {
                list.head = next;
                block::free_graph(node, charge);
            }
        }
    }
}
