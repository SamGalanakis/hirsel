use super::*;

// Timer links and deadline order belong to the slot lock. A registered
// unit cannot retire until its timer has been removed.
fn timer_unit(id: u32) -> &'static Unit {
    unit_ref(id).expect("registered timer unit")
}

#[cfg(test)]
pub(super) fn snapshot<T>(mut head: u32, relative: impl Fn(u64) -> T) -> Vec<(T, u32)> {
    std::iter::from_fn(move || {
        if head == NONE {
            return None;
        }
        let unit = timer_unit(head);
        let entry = (relative(unit.timer_at.load(Ordering::Relaxed)), head);
        head = unit.timer_next.load(Ordering::Relaxed);
        Some(entry)
    })
    .collect()
}

fn next_timer(head: u32) -> u64 {
    if head == NONE {
        u64::MAX
    } else {
        timer_unit(head).timer_at.load(Ordering::Relaxed)
    }
}

/// Register on the parking slot without allocating. Ties use unit id.
pub(super) fn add_timer(p: &Pool, ix: usize, at: u64, u: &Unit) {
    if at == u64::MAX {
        return;
    }
    let slot = &p.slots[ix];
    {
        let mut head = slot.timers.lock().unwrap();
        let mut previous = None::<&Unit>;
        let mut next = *head;
        while next != NONE && (next_timer(next), next) < (at, u.id) {
            let unit = timer_unit(next);
            next = unit.timer_next.load(Ordering::Relaxed);
            previous = Some(unit);
        }
        u.timer_slot
            .store(ix.try_into().expect("worker index"), Ordering::Relaxed);
        u.timer_next.store(next, Ordering::Relaxed);
        u.timer_at.store(at, Ordering::SeqCst);
        if let Some(previous) = previous {
            previous.timer_next.store(u.id, Ordering::Relaxed);
        } else {
            *head = u.id;
        }
        slot.next_timer.store(next_timer(*head), Ordering::SeqCst);
    }
    if WORKER_IX.get() != ix {
        wake_slot(p, ix);
    }
}

/// End a park before this unit resumes. Expiry can win before we lock.
pub(super) fn clear_timer(p: &Pool, u: &Unit) {
    if u.timer_at.load(Ordering::Relaxed) == u64::MAX {
        return;
    }
    let slot = &p.slots[u.timer_slot.load(Ordering::Relaxed) as usize];
    let mut head = slot.timers.lock().unwrap();
    if u.timer_at.load(Ordering::Relaxed) == u64::MAX {
        return;
    }
    let next = u.timer_next.swap(NONE, Ordering::Relaxed);
    if *head == u.id {
        *head = next;
    } else {
        let mut previous = timer_unit(*head);
        while previous.timer_next.load(Ordering::Relaxed) != u.id {
            previous = timer_unit(previous.timer_next.load(Ordering::Relaxed));
        }
        previous.timer_next.store(next, Ordering::Relaxed);
    }
    u.timer_at.store(u64::MAX, Ordering::SeqCst);
    slot.next_timer.store(next_timer(*head), Ordering::SeqCst);
}

/// Only the slot's own worker fires its timers.
pub(super) fn fire_timers(p: &Pool, ix: usize) {
    let slot = &p.slots[ix];
    let next = slot.next_timer.load(Ordering::SeqCst);
    if next == u64::MAX {
        return;
    }
    let now = now();
    if next > now {
        return;
    }
    let mut head = slot.timers.lock().unwrap();
    while *head != NONE && next_timer(*head) <= now {
        let unit = timer_unit(*head);
        *head = unit.timer_next.swap(NONE, Ordering::Relaxed);
        unit.timer_at.store(u64::MAX, Ordering::SeqCst);
        bump(S_TIMER_FIRES);
        wake(unit);
    }
    slot.next_timer.store(next_timer(*head), Ordering::SeqCst);
}
