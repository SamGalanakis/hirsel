//! Publish typed original arena allocations whose payload owns no child holder.
//! Heap-bearing payloads need their explicit paired operand receipts first.
use super::*;
use crate::alloc::{Header, payload};
use crate::sealed_region::planner::descriptors;

/// Retain a successful original arena allocation, including an otherwise dead
/// allocation. Its source recipe establishes arena origin. Descriptor coverage
/// must independently establish the absence of outgoing ordinary/Shared credits.
///
/// # Safety
/// `ctx` is current and `raw` its exact open demanded guard. The initialized
/// `value` is the root holder at descriptor `index` for this one successful
/// original arena construction. C proves its source allocation correspondence,
/// that the block is fresh semantic storage, and no heap-bearing payload or
/// resource is hidden by optimization. The source block stays guard-owned.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_rt_v2_receipt_arena_inventory(
    ctx: *mut TaskCtx,
    raw: *mut c_void,
    index: u32,
    value: *const c_void,
) {
    unsafe { publish(ctx, raw, index, value, None) }
}

/// Publish an original Arena block with its checked, live construction receipt.
/// No source credit moves: the guard's physical cleanup retains child custody.
///
/// # Safety
/// The original inventory contract applies, with a live same-run inline receipt
/// paired to this exact root and its complete original child occurrences.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_rt_v2_receipt_arena_inventory_paired(
    ctx: *mut TaskCtx,
    raw: *mut c_void,
    index: u32,
    value: *const c_void,
    receipt: *const c_void,
) {
    if receipt.is_null() {
        crate::fault("null paired inventory receipt");
    }
    let receipt = ManuallyDrop::new(unsafe { Receipt::<Record>::from_raw(receipt.cast_mut()) });
    unsafe { publish(ctx, raw, index, value, Some(&receipt)) }
}

unsafe fn publish(
    ctx: *mut TaskCtx,
    raw: *mut c_void,
    index: u32,
    value: *const c_void,
    paired: Option<&Receipt<Record>>,
) {
    if ctx.is_null() || value.is_null() {
        crate::fault("null arena inventory construction");
    }
    let record = guard(raw);
    unsafe { &*ctx }
        .run_ref()
        .sealed_owners
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .check_guard(raw.addr(), ctx.addr());
    let root = unsafe { descriptors::layout(ctx, index) }
        .unwrap_or_else(|_| crate::fault("invalid arena inventory root descriptor"));
    if root.size != 8 || root.flags & crate::LAYOUT_DYNAMIC != 0 {
        crate::fault("arena inventory requires one original block");
    }
    let edges = unsafe { descriptors::edges(ctx, index, value.cast_mut().cast(), 8) }
        .unwrap_or_else(|_| crate::fault("invalid arena inventory root coverage"));
    let [edge] = edges.as_slice() else {
        crate::fault("arena inventory requires one original block")
    };
    let block = unsafe { value.cast::<*mut Header>().read_unaligned() };
    if block.is_null()
        || edge.offset != 0
        || edge.shared
        || edge.slice
        || edge.payload != Some(unsafe { (*block).index() })
    {
        crate::fault("arena inventory differs from its typed construction");
    }
    let extent = unsafe { super::construct::extent(ctx, block) };
    let base = unsafe { ptr::addr_of_mut!((*record).base) };
    if !unsafe { region::contains_storage(base, block, extent) } {
        crate::fault("arena inventory block belongs to another destination");
    }
    let index = unsafe { (*block).index() };
    let outgoing = unsafe { descriptors::edges(ctx, index, payload(block), extent) }
        .unwrap_or_else(|_| crate::fault("invalid arena inventory payload coverage"));
    if paired.is_none() && !outgoing.is_empty() {
        crate::fault("arena inventory payload needs paired original receipts");
    }
    let journal = unsafe { (*record).inventory.as_mut() }
        .unwrap_or_else(|| crate::fault("consumed arena inventory"));
    let origin = Origin::Arena(journal.grant());
    let receipt = if let Some(root) = paired {
        if Receipt::run(root) != unsafe { metadata_run(ctx) }
            || root.record().origin() != Origin::Inline
            || root.bytes() != Some(unsafe { std::slice::from_raw_parts(value.cast::<u8>(), 8) })
        {
            crate::fault("inventory receipt differs from its live root");
        }
        let mut children = root.children();
        let child = children
            .next()
            .unwrap_or_else(|| crate::fault("inventory receipt omits its Arena block"));
        if children.next().is_some() || child.record().origin() != origin {
            crate::fault("inventory receipt names another Arena construction");
        }
        let receipt = child.retain();
        let children = receipt
            .children()
            .map(|child| child.retain())
            .collect::<Vec<_>>();
        let actual = unsafe {
            super::construct::snapshot(ctx, index, payload(block), extent, origin, &children)
        };
        if receipt.bytes() != actual.bytes() || receipt.record() != actual.record() {
            crate::fault("inventory receipt differs from its original payload");
        }
        receipt
    } else {
        unsafe { super::construct::snapshot(ctx, index, payload(block), extent, origin, &[]) }
    };
    unsafe {
        journal.allocate_typed(
            journal::Update {
                value: receipt,
                additions: vec![],
            },
            index,
            Some(block),
            extent,
        )
    }
    .ok()
    .unwrap_or_else(|| crate::fault("invalid arena inventory publication"));
}
