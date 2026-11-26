//! An inbox's drop: its seal, its seal answer, and its cleaning.
use super::*;

/// Clean an inbox: seal, wait (shielded) for the seal answer, drop every
/// item not received, and keep the retained charge until the last release.
/// # Safety
/// Transfers the inbox.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_inbox_drop(context: *mut Context, inbox: *mut InboxH) -> u8 {
    // SAFETY: the caller transfers the handle; it stays pinned while it waits.
    unsafe {
        let h = &mut *inbox;
        tasks::before_commit();
        (*context).cancel_shield += 1;
        let u = unit();
        #[cfg(feature = "record")]
        takes::inbox_drop(u, Arc::as_ptr(&h.inbox.0) as usize);
        u.ctx.seal(&h.inbox);
        u.waits += 1;
        let mut spun = false;
        loop {
            if !std::mem::take(&mut spun) {
                u.reach();
            }
            pump(u);
            if h.inbox.0.side().seal_taken {
                break;
            }
            if tasks::park_unit_alone(tasks::current_task(), || false) {
                tasks::before_commit();
                spun = true;
                continue;
            }
            tasks::park(
                &raw mut h.waiters,
                #[cfg(feature = "prof")]
                (crate::prof::Reason::Join, 0),
            );
        }
        u.waits -= 1;
        (*context).cancel_shield -= 1;
        let side = h.inbox.0.side();
        for v in side.taken.iter().rev() {
            drop_packed_owners(
                context,
                u,
                h.inbox.flat_ptr(v),
                &h.owners,
                v.value().item_sockets(),
            );
        }
        let key = Arc::as_ptr(&h.inbox.0) as usize;
        u.inboxes.remove(&key);
        let mut h = Box::from_raw(inbox);
        if let Some(held) = h.charge.take() {
            u.retained.insert(key, held);
        }
        // With no client left the cleaning is the owner's own release: it
        // takes through the notice, which refunds the retained charge.
        let (_, took) = u.ctx.clean(&h.inbox);
        #[cfg(feature = "record")]
        crate::tape::units::retained(key, h.inbox.0.retained());
        drop(h);
        if !took.is_empty() || u.released_own() {
            u.on_released(took);
        }
        u.reach();
        pump(u);
        0
    }
}
