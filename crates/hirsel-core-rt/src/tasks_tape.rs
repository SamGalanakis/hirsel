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
#[inline(never)]
pub(super) fn replay_boundary(provider_entry: bool) {
    replay_boundary_for(CURRENT.get(), provider_entry);
}

#[cfg(feature = "record")]
#[inline(never)]
pub(super) fn replay_boundary_for(task: *mut Task, provider_entry: bool) {
    // SAFETY: called with the active task. No scheduler reference survives a switch.
    unsafe {
        if task.is_null() || !(*task).replay {
            return;
        }
        let group = (*task).group.unwrap();
        use crate::tape::schedule::Turn;
        if (*task).latest_pair {
            if !matches!(group.turn(), Turn::Fifo | Turn::Event(Some(_))) {
                group.fail();
            }
            return;
        }
        loop {
            match group.turn() {
                Turn::Fifo | Turn::Deadlock | Turn::Take => return,
                Turn::Event(Some(id)) | Turn::Retired(id) if id == group.task => return,
                Turn::Wait(id) if id == group.task && provider_entry => return,
                Turn::Wait(id) if id == group.task => {
                    group.fail();
                    return;
                }
                Turn::Grant | Turn::Finish if group.task == 0 => {
                    if provider_entry {
                        return;
                    }
                    group.fail();
                    return;
                }
                Turn::Event(Some(id))
                    if (*(*task).run)
                        .group_task(group, id)
                        .as_ref()
                        .is_none_or(|wanted| wanted.state != State::Ready) =>
                {
                    group.fail();
                    return;
                }
                Turn::Event(None) => {
                    group.fail();
                    return;
                }
                // A FIFO record takes an input only where its task leaves the
                // processor by itself (poll_event), so this task runs on to that
                // point; an event first passes the input's position and diverges.
                Turn::Wake(_) | Turn::Cancel(_) | Turn::Done(_) | Turn::Retired(_)
                    if group.fifo() =>
                {
                    return;
                }
                _ => checkpoint(),
            }
        }
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
                Turn::Fifo | Turn::Event(None | Some(0)) | Turn::Take => return,
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

impl Scheduler {
    /// Record writes a turn's inputs in two places: CANCEL before its
    /// cancels, then WAKE and DONE at its readiness. Replay takes each at the
    /// same place: `readiness` false takes only CANCEL, true takes the rest.
    pub(super) unsafe fn take_tape_inputs(&mut self, readiness: bool) {
        // SAFETY: idle owns tasks; retirement removes queued requests.
        unsafe {
            for task in std::mem::take(&mut self.outside_cancels) {
                if let Some(group) = (*task).group
                    && group.cancel(None)
                {
                    cancel_flag(task).store(true, Ordering::Release);
                    if (*task).state == State::Provider {
                        self.cancels.push(task);
                    }
                    self.wake_cancelled(task);
                }
            }
            for index in 0..self.tapes.live.len() {
                let group = self.tapes.live[index].group;
                use crate::tape::schedule::Turn;
                loop {
                    let turn = group.turn();
                    if !readiness && !matches!(turn, Turn::Cancel(_)) {
                        break;
                    }
                    let id = match turn {
                        Turn::Wake(id) | Turn::Cancel(id) | Turn::Done(id) => id,
                        Turn::Choose(index) => {
                            let live = self.live_group_tasks(group);
                            match usize::try_from(index).ok().and_then(|i| live.get(i)) {
                                Some(&(id, at)) if (*at).state == State::Ready => group.choose(id),
                                _ => group.fail_choice(
                                    live.iter()
                                        .filter(|&&(_, at)| (*at).state == State::Ready)
                                        .count(),
                                ),
                            }
                            break;
                        }
                        _ => break,
                    };
                    let target = self.group_task(group, id);
                    if target.is_null() {
                        group.fail();
                        break;
                    }
                    match turn {
                        Turn::Cancel(_) if (*target).state != State::Ended => {
                            group.consume_input();
                            cancel_flag(target).store(true, Ordering::Release);
                            if (*target).state == State::Provider {
                                self.wake_operation(target, Wake::Cancel);
                            }
                            self.wake_cancelled(target);
                        }
                        Turn::Wake(_)
                            if (*target).state == State::Provider
                                && matches!(
                                    (*(*target).operation).state,
                                    OperationState::Waiting
                                ) =>
                        {
                            group.consume_input();
                            group.cancel_wait((*(*target).operation).request);
                            self.runnable(
                                target,
                                OperationState::ReadyToIssue,
                                #[cfg(feature = "prof")]
                                Wake::Ready,
                            );
                        }
                        Turn::Wake(_)
                            if (*target).channel_tape == ChannelTape::Parked
                                && ((*target).state.channel_deadline()
                                    || (*target).state == State::Ready) =>
                        {
                            // This wake was recorded at dispatch. A local send
                            // may already have put the same wait on the queue.
                            group.consume_input();
                            if (*target).state.channel_deadline() {
                                Waiters::remove(target);
                                (*target).state = State::Ready;
                            } else {
                                self.unlink_ready(target);
                            }
                            (*target).channel_tape = ChannelTape::Expired;
                            (*target).ready = self.head;
                            if self.tail.is_null() {
                                self.tail = target;
                            }
                            self.head = target;
                        }
                        // A host-clock deadline on a virtual provider fires in
                        // `take_timers`, after this turn's readies run.
                        Turn::Wake(_)
                            if (*target).state == State::Provider
                                && crate::provide::owns(&(*(*target).operation).provider)
                                && matches!(
                                    (*(*target).operation).state,
                                    OperationState::Issued { .. }
                                ) =>
                        {
                            break;
                        }
                        Turn::Done(_)
                            if (*target).state == State::Provider
                                && matches!(
                                    (*(*target).operation).state,
                                    OperationState::Issued { .. }
                                ) =>
                        {
                            self.wake_operation(target, Wake::Ready);
                        }
                        _ => {
                            group.fail();
                            break;
                        }
                    }
                }
            }
        }
    }

    /// Fire any recorded host-clock deadline on a virtual provider once this
    /// turn's readies have been applied and (under FIFO) no task is runnable.
    pub(super) unsafe fn take_tape_deadlines(&mut self, readied: bool) {
        // SAFETY: idle owns tasks and pinned provider operation frames.
        unsafe {
            let idle = self.head.is_null() && self.deferred.is_null();
            for index in 0..self.tapes.live.len() {
                let group = self.tapes.live[index].group;
                if group.fifo() {
                    if !idle {
                        continue;
                    }
                } else if readied {
                    continue;
                }
                while let crate::tape::schedule::Turn::Wake(id) = group.turn() {
                    let target = self.group_task(group, id);
                    if target.is_null()
                        || (*target).state != State::Provider
                        || !crate::provide::owns(&(*(*target).operation).provider)
                        || !matches!((*(*target).operation).state, OperationState::Issued { .. })
                    {
                        break;
                    }
                    group.consume_input();
                    self.wake_operation(target, Wake::Timer);
                }
            }
        }
    }
}
