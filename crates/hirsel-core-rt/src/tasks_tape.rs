use super::*;

#[cfg(feature = "record")]
pub(super) unsafe fn live_index(task: *mut Task, group: crate::tape::schedule::Group) -> u64 {
    if group.task == 0 {
        return 0;
    }
    let mut index = 0;
    // SAFETY: the active committer owns its scheduler's creation list.
    unsafe {
        // The list is newest first. Each tape starts at its opener and assigns
        // child ordinals in creation order, so only older tasks can rank lower.
        let mut at = (*task).all;
        while !at.is_null() {
            if (*at).state != State::Ended
                && (*at)
                    .group
                    .is_some_and(|g| g.same(group) && g.task < group.task)
            {
                index += 1;
            }
            at = (*at).all;
        }
    }
    index
}

#[cfg(feature = "record")]
pub(crate) fn group_index() -> u64 {
    let task = CURRENT.get();
    // SAFETY: CURRENT pins the task while a provider issues synchronously.
    unsafe {
        task.as_ref()
            .and_then(|t| t.group)
            .map_or(0, |g| live_index(task, g))
    }
}

#[cfg(feature = "record")]
pub(crate) fn before_tape_frame(group: Group, ending: bool) {
    tape_boundary(group, ending);
}

#[cfg(feature = "record")]
fn tape_boundary(group: Group, ending: bool) {
    let task = CURRENT.get();
    // SAFETY: CURRENT pins the live opener; checkpoint borrows no task fields.
    unsafe {
        if task.is_null() || !(*task).replay || !(*task).group.is_some_and(|g| g.same(group)) {
            return;
        }
        use crate::tape::schedule::Turn;
        loop {
            match group.turn() {
                Turn::Fifo | Turn::Event(None | Some(0)) => return,
                Turn::Grant if !ending => return,
                Turn::Finish | Turn::Deadlock => return,
                Turn::Grant => {
                    group.fail();
                    return;
                }
                _ => checkpoint(),
            }
        }
    }
}

#[cfg(feature = "record")]
pub(crate) fn attach_tape(group: Group) {
    let task = CURRENT.get();
    if !task.is_null() {
        // SAFETY: CURRENT pins the active task and run through the membership move.
        unsafe {
            let tapes = &mut (*(*task).run).tapes;
            if let Some(old) = (*task).group
                && let Some(members) = tapes.membership(old)
            {
                *members -= 1;
            }
            join(tapes, &mut *task, group);
        }
    }
}

/// Make `task` a member of `group`'s tape.
#[cfg(feature = "record")]
pub(super) fn join(tapes: &mut Tapes, task: &mut Task, group: Group) {
    tapes.open(group);
    *tapes.membership(group).unwrap() += 1;
    task.group = Some(group);
    task.replay = group.replaying();
    task.arc_runtime |= task.replay;
    task.arc_hooks = true;
}

#[cfg(feature = "record")]
pub(crate) fn detach_tape(group: Group) {
    let task = CURRENT.get();
    if !task.is_null() {
        // SAFETY: the active task owns the inventory through the tape close.
        unsafe { (*(*task).run).tapes.close(group) };
    }
}
