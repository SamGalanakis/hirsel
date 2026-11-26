//! Where a recording or replay starts: the operator's pinned replay
//! arguments, and the unit pool when `HIRSEL_UNTIED` is set.
use super::*;

/// `--hirsel-replay-recording MODE PATH [UNIT]`: the replay the operator CLI
/// starts on a pinned image, with the one unit `--unit` names.
pub(super) fn pinned_replay(input: &OsConfig) -> Option<(Vec<u8>, u64, Option<Vec<u8>>)> {
    if !matches!(input.args_len, 3 | 4) {
        return None;
    }
    // SAFETY: the native launcher pins exactly args_len View records at entry.
    let args = unsafe { std::slice::from_raw_parts(input.args, input.args_len) };
    let command = unsafe { crate::tls::view_bytes(&args[0]) };
    if command != b"--hirsel-replay-recording" {
        return None;
    }
    let mode = match unsafe { crate::tls::view_bytes(&args[1]) } {
        b"same" => 1,
        b"diagnose" => 2,
        _ => return None,
    };
    let path = unsafe { crate::tls::view_bytes(&args[2]) }.to_vec();
    let unit = args
        .get(3)
        .map(|unit| unsafe { crate::tls::view_bytes(unit) }.to_vec());
    Some((path, mode, unit))
}

/// A recording or replay of a program that spawns units runs its root on
/// `HIRSEL_UNTIED` workers when that is set, as its ordinary run does, and
/// its helpers then run on the pool beside it; else on this thread.
/// # Safety
/// `input` is pinned for the run.
pub(super) unsafe fn on_pool(
    units: bool,
    input: *const OsConfig,
    run: &dyn Fn(*const OsConfig) -> u8,
) -> u8 {
    struct Root<'a>(&'a dyn Fn(*const OsConfig) -> u8);
    // SAFETY: only the root unit calls it, once, and the launch joins every
    // worker before returning; its operands are pinned startup tables.
    unsafe impl Sync for Root<'_> {}
    let root = &Root(run);
    if units && let Some(w) = crate::units::workers() {
        // SAFETY: the caller pins the configuration for the launch.
        crate::units::launch(1, w, unsafe { &*input }, &move |config| {
            let Root(run) = root;
            run(config)
        })
    } else {
        run(input)
    }
}
