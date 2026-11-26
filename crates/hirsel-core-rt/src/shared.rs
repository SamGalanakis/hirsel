//! `Shared[T]` nodes (CONC-15): one frozen payload read by holders in any
//! Unit. A holder is the node pointer; a crossing moves the pointer and never
//! the payload. The count is atomic and its updates are not scheduler events,
//! so sharing adds nothing to the total order (CONC-10).
//!
//! The block is `[home][owners][rows][charge][count][payload]` and a holder
//! points at the count, so the payload is at +8 as in an `Arc` node. `T` is
//! `shareable` and `cross`, so it owns only sequence buffers and nested
//! holders (resources are never shareable, OWN-4). The node records that
//! owners table, and the runtime cleans the payload with no compiler cleanup
//! and no task context.
//!
//! The node and the payload's buffers stay charged to the Unit that made the
//! node, its home. The last release cleans and refunds them in the home Unit:
//! at the release when it happens there, and otherwise at home's take of a
//! `SharedRelease` arrival, so every refund is an event or an input of the Unit
//! it credits (MEM-20). A home helper that ends first leaves each live node's
//! bytes to its owner, as it leaves a connection with clients elsewhere, and
//! its port forwards the arrival there.
use crate::{
    Context, block,
    block::Block,
    tasks,
    units::ledger::{ArrivalRef, Body, Port},
    units_lang::{Cross, LangUnit},
};
use hirsel_core_layout::cross::{CAPABILITY, CASE, ELEMENT, FUNCTION, SEQ, SHARED};
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicU64, Ordering, fence};
use std::sync::{Arc, Mutex};

#[repr(C)]
struct Header {
    /// The home unit's port, one `Arc` strong count held by the node.
    home: *const Port,
    /// The payload's owners table (interned), `rows` entries.
    owners: *const [u64; 4],
    rows: u64,
    charge: u64,
    count: AtomicU64,
}
const COUNT: usize = std::mem::offset_of!(Header, count);
const _: () = assert!(
    size_of::<Header>() as u64 == hirsel_core_layout::charge::SHARED_NODE
        && COUNT + 8 == size_of::<Header>()
);

#[repr(C)]
pub struct New {
    pub node: *mut u8,
    pub available: u64,
}

/// One static owners table per `cross` type, as program text is: a node keeps
/// a pointer to it rather than a copy.
fn intern(rows: &[[u64; 4]]) -> *const [u64; 4] {
    static TABLES: Mutex<Vec<&'static [[u64; 4]]>> = Mutex::new(Vec::new());
    if rows.is_empty() {
        return ptr::null();
    }
    let mut tables = TABLES.lock().unwrap();
    if let Some(t) = tables.iter().find(|t| **t == rows) {
        return t.as_ptr();
    }
    let t: &'static [[u64; 4]] = Box::leak(rows.to_vec().into_boxed_slice());
    tables.push(t);
    t.as_ptr()
}

fn round8(n: u64) -> u64 {
    n.div_ceil(8) * 8
}

unsafe fn word(at: *const u8) -> u64 {
    // SAFETY: the caller reads an initialized word of a live value.
    unsafe { at.cast::<u64>().read_unaligned() }
}

/// Admit `charge` (`charge::shared(size(T))`) from the running limit and move
/// the `T` at `x` into a new node with one holder, homed in the running Unit.
/// On refusal the node is null and `x` stays the caller's.
/// # Safety
/// Context is the running task's; `x` holds a live `T` that the caller
/// transfers on success; `cross` is T's descriptor.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_shared_new(
    context: *mut Context,
    x: *const u8,
    cross: *const Cross,
    charge: u64,
) -> New {
    // SAFETY: the block is fresh, aligned and holds the header and payload.
    unsafe {
        let size = (*cross).bytes;
        let rows = (*cross).owners();
        let block = match Block::new(
            &*context,
            Some(charge),
            size_of::<Header>() + size as usize,
            align_of::<Header>(),
        ) {
            Ok(block) => block,
            Err(error) => {
                return New {
                    node: ptr::null_mut(),
                    available: error.available,
                };
            }
        };
        let unit = &mut *tasks::lang_unit();
        let header = block.into_raw().as_ptr().cast::<Header>();
        header.write(Header {
            home: Arc::into_raw(unit.ctx.port.clone()),
            owners: intern(rows),
            rows: rows.len() as u64,
            charge,
            count: AtomicU64::new(1),
        });
        let payload = header.add(1).cast::<u8>();
        ptr::copy_nonoverlapping(x, payload, size as usize);
        // What the node holds of its home's limit: the node and each buffer.
        let held = charge
            + rows
                .iter()
                .filter(|o| o[1] == SEQ)
                .map(|o| round8(word(payload.add(o[0] as usize + 16)) * o[3]))
                .sum::<u64>();
        let node = header.cast::<u8>().add(COUNT);
        unit.homed.insert(node.addr(), held);
        #[cfg(feature = "record")]
        crate::tape::units::shared_new(tasks::current_group(), node.addr(), &*cross);
        New { node, available: 0 }
    }
}

/// One more holder of the node. Admits nothing and is not an event.
/// # Safety
/// The caller holds a live holder of `node`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_shared_share(node: *mut u8) -> *mut u8 {
    // SAFETY: the caller's holder pins the header.
    let count = unsafe { &(*header(node)).count };
    // Relaxed suffices: the new holder reaches another thread only through a
    // crossing, which synchronizes.
    if count.fetch_add(1, Ordering::Relaxed) == u64::MAX {
        crate::process::stop(b"CountOverflow");
    }
    node
}

/// Give up one holder. The last one releases the node (`release`).
/// # Safety
/// The caller transfers a live holder; context is the running task's.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_shared_release(context: *mut Context, node: *mut u8) {
    // SAFETY: as release; a running task pins its unit.
    unsafe {
        // Each drop of a holder is a take point (docs/recording.md): whether
        // it is the last follows the drops of every unit.
        #[cfg(feature = "record")]
        (*tasks::lang_unit()).reach();
        if last(node) {
            tasks::before_commit();
            release_last(&mut *tasks::lang_unit(), context, node);
        }
    }
}

/// Give up one holder in `unit`.
/// # Safety
/// As `hirsel_core_shared_release`; the caller holds the unit's commit and
/// `context` is the running task's or null.
pub(crate) unsafe fn release(unit: &mut LangUnit, context: *mut Context, node: *mut u8) {
    // SAFETY: by the contract.
    unsafe {
        unit.reach();
        if last(node) {
            release_last(unit, context, node);
        }
    }
}

/// Drop one count; true for the last holder, which then owns the node.
unsafe fn last(node: *mut u8) -> bool {
    // SAFETY: the holder pins the header until its decrement.
    unsafe {
        if (*header(node)).count.fetch_sub(1, Ordering::Release) != 1 {
            return false;
        }
    }
    // Every other holder's reads happen before this cleanup.
    fence(Ordering::Acquire);
    true
}

/// The last release: clean here when this is home, or home has ended and
/// forwards here; else send the node home. Each forward on that path was set
/// by this unit's own end takes, so the choice does not depend on timing.
unsafe fn release_last(unit: &mut LangUnit, context: *mut Context, node: *mut u8) {
    // SAFETY: the last holder owns the node; its count pins the home port.
    unsafe {
        let home = (*header(node)).home;
        if (*home).leads_to(&unit.ctx.port) {
            #[cfg(feature = "record")]
            unit.shared_release_take(node.addr());
            clean(unit, context, node);
        } else {
            (*home).publish(ArrivalRef::boxed(Body::SharedRelease(node.addr())));
        }
    }
}

/// Clean and refund a node whose last release reached home, or home's owner
/// once home has ended (`Took::SharedRelease`).
/// # Safety
/// The unit owns the node exclusively and holds its commit; `context` is the
/// running task's or null between tasks.
pub(crate) unsafe fn clean(unit: &mut LangUnit, context: *mut Context, node: *mut u8) {
    // SAFETY: the node is exclusive; each owner row describes a live owner of
    // the payload, and every block keeps its payer until its refund.
    unsafe {
        let header = header(node);
        let Header {
            home,
            owners,
            rows,
            charge,
            ..
        } = header.read();
        let payload = header.add(1).cast::<u8>();
        let rows = if rows == 0 {
            &[][..]
        } else {
            std::slice::from_raw_parts(owners, rows as usize)
        };
        for o in rows {
            let at = payload.add(o[0] as usize);
            match o[1] {
                SEQ => {
                    let cap = word(at.add(16));
                    if cap != 0 {
                        free(context, word(at) as *mut u8, round8(cap * o[3]));
                    }
                }
                SHARED => release(unit, context, word(at) as *mut u8),
                CAPABILITY | FUNCTION => {}
                kind if kind & (ELEMENT | CASE) != 0 => {}
                // Clients and sockets are resources, never shareable (OWN-4).
                _ => unreachable!("a resource in a Shared payload"),
            }
        }
        free(context, header.cast(), charge);
        drop(Arc::from_raw(home));
        // Home's own node, or a survivor of an ended home helper.
        unit.homed.remove(&node.addr());
        unit.refund_retained(node.addr());
    }
}

/// Free a block in this unit: through the running task's ordered path, or
/// at the unit between tasks, as a held charge is.
unsafe fn free(context: *mut Context, p: *mut u8, charge: u64) {
    // SAFETY: the caller owns the live block and its exact charge.
    unsafe {
        let p = NonNull::new_unchecked(p);
        if context.is_null() {
            Block::from_raw(p, charge).refund_at_unit();
        } else {
            block::free_in(&*context, p, charge);
        }
    }
}

fn header(node: *mut u8) -> *mut Header {
    node.wrapping_sub(COUNT).cast()
}
