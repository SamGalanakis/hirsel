//! Inbox receives and handle cleanup, including the releases a drop takes.
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
        #[cfg(feature = "record")]
        if u.ctx.parking {
            let _ = u.released_own();
            u.last_released = None;
            u.reach();
            pump(u);
            if u.last_released == Some(key) {
                u.reach();
                pump(u);
            }
            return 0;
        }
        if !took.is_empty() || u.released_own() {
            u.on_released(took);
        }
        u.reach();
        pump(u);
        0
    }
}

impl LangUnit {
    /// Drop a client. The owner's own release of a cleaned connection takes
    /// through its notice; each take wakes what waits on it, as in `pump`.
    pub(super) fn drop_client(&mut self, client: Client) {
        #[cfg(feature = "record")]
        let stand_in = self
            .ctx
            .parking
            .then(|| crate::tape::units::stand_in_of(Arc::as_ptr(&client.0) as usize))
            .flatten();
        let took = self.ctx.drop_client(client);
        #[cfg(feature = "record")]
        let stand_in_release = stand_in.is_some_and(|(s, o)| {
            crate::tape::units::replay::is_last_release_due(self.tape(), s, o)
        });
        #[cfg(not(feature = "record"))]
        let stand_in_release = false;
        if !took.is_empty() || self.released_own() || stand_in_release {
            self.on_released(took);
        }
    }

    /// Every caller holds a commit. A release that took drained the port and
    /// may have left later arrivals reached but untaken; pump takes them and
    /// rearms the port, so no task parks on work the unit already holds.
    #[cold]
    #[inline(never)]
    pub(super) fn on_released(&mut self, took: Vec<Released>) {
        self.reach();
        let wake = self.waits > 0;
        for (t, id, _) in took {
            // SAFETY: a release runs in a commit that owns the unit.
            unsafe { on_take(self, t, id, wake) };
        }
        // SAFETY: as above.
        unsafe { pump(self) };
    }
}

#[repr(C)]
pub struct ReceiveOutcome {
    /// The low byte holds the outcome and the next byte holds the failure
    /// status. Requested fits in the remaining bits because MAX_ROOM is 40 bits.
    pub code_and_requested: u64,
    pub available: u64,
}

const _: () = assert!(MAX_ROOM <= u64::MAX >> 16);

/// `conn.receive` (`wait`) and `conn.try_receive`.
/// # Safety
/// The inbox borrow and a writable item destination.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_conn_receive(
    context: *mut Context,
    inbox: *mut InboxH,
    dest: *mut u8,
    site: u64,
    wait: bool,
) -> ReceiveOutcome {
    // SAFETY: by the contract; the stackful form never parks returning.
    unsafe { conn_receive::<false>(context, inbox, dest, site, wait, false, None) }
        .unwrap_or_else(|| unreachable!())
}

/// `conn.receive_until`, using the same outcome codes as `conn.try_receive`.
/// # Safety
/// The inbox borrow, a writable item destination and a live Clock when present.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_conn_receive_until(
    context: *mut Context,
    inbox: *mut InboxH,
    dest: *mut u8,
    present: u8,
    clock: u64,
    at: u64,
    site: u64,
) -> ReceiveOutcome {
    let deadline = (present != 0).then(|| crate::channel::ReceiveDeadline::new(clock, at));
    // SAFETY: by the contract; this form never parks returning.
    unsafe { conn_receive::<false>(context, inbox, dest, site, true, false, deadline) }
        .unwrap_or_else(|| unreachable!())
}

/// The returnable form of `conn.receive` (`RtOp::Returnable`): `word` is the
/// caller's child word, 1 while the receive waits with the stack given back.
/// # Safety
/// As `hirsel_core_conn_receive`, in a task that runs a returnable root; a
/// resume passes the same inbox and destination place.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_conn_receive_rec(
    context: *mut Context,
    word: *mut u64,
    inbox: *mut InboxH,
    dest: *mut u8,
    site: u64,
) -> ReceiveOutcome {
    let parked = ReceiveOutcome {
        code_and_requested: 0,
        available: 0,
    };
    // SAFETY: by the contract.
    unsafe {
        let received = conn_receive::<true>(context, inbox, dest, site, true, *word != 0, None);
        tasks::returned(word, received, parked)
    }
}

/// `REC` parks with the stack given back (None); `resumed` continues that
/// wait, which has already counted itself in the unit's waits.
#[inline(always)]
unsafe fn conn_receive<const REC: bool>(
    context: *mut Context,
    inbox: *mut InboxH,
    dest: *mut u8,
    site: u64,
    wait: bool,
    resumed: bool,
    mut deadline: Option<crate::channel::ReceiveDeadline>,
) -> Option<ReceiveOutcome> {
    use hirsel_core_layout::event::{CHANNEL_CLOSED, CHANNEL_EMPTY};
    // Receive events use the implicit inbox. Its arrivals carry the
    // connection identity in the unit's TAKE frames.
    let event_object = 0;
    let out = |outcome: u64, requested: u64, available| {
        Some(ReceiveOutcome {
            code_and_requested: (requested << 16) | outcome,
            available,
        })
    };
    // SAFETY: the inbox borrow pins the handle and its connection.
    unsafe {
        let input = &(*inbox).inbox;
        let c = &input.0;
        let task = tasks::current_task();
        let waited_before = REC && resumed;
        let unit = tasks::lang_unit_for(task);
        let mut waited = waited_before;
        // A retry after spinning is the same take-point position.
        let mut spun = false;
        loop {
            if deadline.is_some() {
                tasks::before_commit_for(task);
            }
            let u = &mut *unit;
            if wait && tasks::check_cancel_for(context, site, task) != 0 {
                if waited {
                    u.waits -= 1;
                }
                return out(1 << 8, 0, 0);
            }
            let counted = !std::mem::take(&mut spun);
            if c.side().taken.is_empty() && counted {
                // A position whether or not anything is pending.
                u.reach();
            }
            #[cfg(any(test, feature = "testing"))]
            let pending = u.ctx.port.pending() || u.ctx.has_reached();
            #[cfg(not(any(test, feature = "testing")))]
            let pending = u.ctx.port.pending();
            // A replaying unit's items come from its TAKEs, due at this point.
            #[cfg(feature = "record")]
            let pending = pending || u.ctx.parking;
            if c.side().taken.is_empty() && pending {
                // A lone item for this inbox is all the port holds, so no
                // other wait of this unit (a helper, another task) has
                // anything to take from it.
                if u.ctx.take_single_item(c) {
                    takes::take(u, &Took::Item, Arc::as_ptr(c) as usize);
                    take_sockets(u, &*inbox);
                } else {
                    pump_for(u, waited);
                }
            }
            let received = input.receive_cross(|bytes, flat, content, sockets| {
                let live = c.live(flat);
                let seqs = live.as_deref().unwrap_or(&c.seqs);
                if c.seqs.is_empty() {
                    if c.stride != 0 {
                        ptr::copy_nonoverlapping(flat, dest, c.stride);
                    }
                } else {
                    let payer = (*context).payer();
                    if bytes != 0 {
                        block::admit_storage(payer, Some(bytes))?;
                    }
                    ptr::copy_nonoverlapping(flat, dest, c.stride);
                    unpack_owners(context, payer, content, dest, seqs);
                }
                match live.as_deref() {
                    Some(rows) => attach_sockets(context, dest, socket_offsets(rows), sockets),
                    None => {
                        attach_sockets(context, dest, c.socket_offsets.iter().copied(), sockets)
                    }
                }
                Ok::<_, Capacity>(())
            });
            if let Some(result) = received {
                if waited {
                    u.waits -= 1;
                }
                return match result {
                    Ok(()) => {
                        if deadline.is_some() {
                            tasks::event(tasks::RECEIVE, event_object, 0);
                        }
                        out(0, 0, 0)
                    }
                    Err(e) => out(2, e.requested, e.available),
                };
            }
            if c.side().closed {
                if waited {
                    u.waits -= 1;
                }
                if deadline.is_some() {
                    tasks::event(tasks::RECEIVE, event_object, CHANNEL_CLOSED);
                }
                return out(1, 0, 0);
            }
            if !wait {
                return out(3, 0, 0);
            }
            if !waited {
                waited = true;
                u.waits += 1;
            }
            if let Some(deadline) = &mut deadline {
                let status =
                    match deadline.park(context, &raw mut (*inbox).waiters, event_object, site) {
                        Ok(true) => continue,
                        Ok(false) => 0,
                        Err(status) => status,
                    };
                (*unit).waits -= 1;
                if status != 0 {
                    return out(u64::from(status) << 8, 0, 0);
                }
                tasks::event(tasks::RECEIVE, event_object, CHANNEL_EMPTY);
                return out(3, 0, 0);
            }
            if tasks::park_unit_alone(task, || (*unit).ctx.port.spin_before_park(None)) {
                spun = true;
                continue;
            }
            if REC {
                tasks::park_returning(
                    task,
                    &raw mut (*inbox).waiters,
                    #[cfg(feature = "prof")]
                    (crate::prof::Reason::None, 0),
                );
                return None;
            }
            tasks::park_for(
                task,
                &raw mut (*inbox).waiters,
                #[cfg(feature = "prof")]
                (crate::prof::Reason::None, 0),
            );
        }
    }
}
