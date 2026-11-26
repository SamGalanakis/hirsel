//! A replaying unit's helpers (docs/recording.md, "Replay of a run with
//! helpers"): a spawn is checked against the unit's stream and starts
//! nothing; the helper's end, at the unit's own recorded `TAKE` of it,
//! publishes the recorded result to the unit's port, where the take, the
//! join and the charges run as they do for a helper that ran.
use super::*;
use crate::tape::schedule::Turn;
use crate::tape::units::replay as tape;

/// A replayed spawn's `UNIT_SPAWN` arguments, or why it is not replayed;
/// None when the spawning unit does not replay.
/// # Safety
/// As `crate::tape::helper::spawn`.
pub(super) unsafe fn spawn_args(
    context: *const Context,
    n: u64,
    steps: Option<u64>,
    input: *const u8,
    spawned: &SpawnEntry,
) -> Option<Result<Vec<u8>, &'static str>> {
    tasks::current_group()
        .filter(|group| group.replaying())
        // SAFETY: forwarded.
        .map(|_| unsafe { crate::tape::helper::replay_args(context, n, steps, input, spawned) })
}

/// Check the spawn of helper `k`, whose context stays unstarted until its
/// end is taken.
pub(super) fn spawn(
    u: &mut LangUnit,
    k: usize,
    hctx: Box<UnitCtx>,
    args: Result<Vec<u8>, &'static str>,
) {
    tape::spawn(tasks::current_group(), k, args);
    u.unstarted.insert(k, hctx);
}

impl LangUnit {
    /// A replaying unit's next frame is a `TAKE` at a scheduler pump, due
    /// now: its tasks' waits for other units are not outside waits, since the
    /// pump takes it. Its tasks' replay boundaries let them run on to their
    /// take points or parks, so the unit idles and pumps.
    pub(crate) fn take_due(&self) -> bool {
        self.tape
            .is_some_and(|group| group.replaying() && matches!(group.turn(), Turn::Take))
    }
}

/// Apply each helper end due at this take point: the helper's context ends
/// with the recorded result, which its end take then takes from the port.
/// # Safety
/// As `pump`.
pub(super) unsafe fn take_due(u: &mut LangUnit) {
    let Some(group) = u.tape().filter(|group| group.replaying()) else {
        return;
    };
    loop {
        let helpers = &u.helpers;
        let layout = |key: usize| {
            // SAFETY: a registered handle is pinned while registered.
            helpers
                .get(&key)
                .map(|&h| unsafe { ((*h).result.bytes, (*h).result.owners.clone()) })
        };
        let Some(end) = tape::due(Some(group), u.pumping, layout) else {
            return;
        };
        let h = u.helpers[&end.key];
        let hctx = u
            .unstarted
            .remove(&end.key)
            .unwrap_or_else(|| crate::process::stop(b"UnitLedger"));
        // SAFETY: as above; the packed result has the handle's result layout.
        let (port, result) = unsafe {
            (
                Arc::as_ptr(&(*h).port) as usize,
                item_charge(end.packed.as_ptr(), &(*h).result.owners),
            )
        };
        let ended = Ended {
            failed: false,
            causes: Vec::new(),
            result,
            survivors: Vec::new(),
            run_grants: 0,
            charges: Vec::new(),
            tape_end: None,
        };
        RESULTS.lock().unwrap().insert(port, ended);
        let mut ret = Value::empty();
        ret.set_item(end.packed, None);
        hctx.end_helper(Some(ret), false);
    }
}
