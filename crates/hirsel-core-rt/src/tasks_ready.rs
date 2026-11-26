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
