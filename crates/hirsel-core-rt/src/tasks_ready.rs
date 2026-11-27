#[cfg(test)]
use super::ReadyArm;

pub(super) fn ready_scratch_bytes() -> usize {
    crate::units::memory().map_or(4096, |memory| memory.ready_scratch_bytes())
}

pub(super) fn release_ready_scratch<T>(scratch: &mut Vec<T>, bytes: usize) {
    if scratch.capacity() > bytes / size_of::<T>() {
        *scratch = Vec::new();
    }
}

#[cfg(test)]
pub(crate) fn check_ready_scratch() {
    let mut tokens = Vec::<u64>::with_capacity(1_000);
    release_ready_scratch(&mut tokens, ready_scratch_bytes() / 2);
    assert!(tokens.capacity() * 8 <= 2048);
    let mut arms = Vec::<ReadyArm>::with_capacity(1_000);
    release_ready_scratch(&mut arms, ready_scratch_bytes());
    assert!(arms.capacity() * 16 <= 4096);
}

/// Discard the readies this thread's scheduler-less waiter holds. The Exec
/// helper's serving thread waits on it and rescans every child on each wake.
/// iOS builds no Exec helper.
#[allow(dead_code)]
pub(crate) fn clear_alone() {
    super::ALONE.with(|(readies, _)| {
        readies.lock().clear();
        readies
            .pending
            .store(false, std::sync::atomic::Ordering::Release);
    });
}
