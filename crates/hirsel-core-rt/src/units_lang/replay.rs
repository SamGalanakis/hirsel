//! A replaying unit's helpers (docs/recording.md, "Replay of a run with
//! helpers"): a spawn is checked against the unit's stream and starts
//! nothing; the helper's end, at the unit's own recorded `TAKE` of it,
//! publishes the recorded result to the unit's port, where the take, the
//! join and the charges run as they do for a helper that ran.
use super::*;
use crate::tape::schedule::Turn;
use crate::tape::units::replay as tape;
use crate::units::ledger::Body as LBody;

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
        // SAFETY: the context that bound the recording pins its host files.
        let grants = unsafe { u.files.as_ref() }.map(|f| f.registry.run_grants());
        let (key, outcome) = match tape::due(Some(group), u.pumping, layout, grants) {
            None => return,
            Some(tape::Due::Cancel) => {
                if !u.root.is_null() {
                    // SAFETY: the unit's root task is live while the unit runs.
                    unsafe { tasks::hirsel_core_cancel(ptr::null_mut(), u.root) };
                }
                continue;
            }
            Some(tape::Due::End { key, outcome }) => (key, outcome),
            Some(tape::Due::Item { conn, flat }) => {
                let Some(&ih) = u.inboxes.get(&conn) else {
                    tape::refuse_take(Some(group), "an item for a dropped inbox");
                    return;
                };
                // SAFETY: a registered inbox is live while registered; the
                // packed item has its connection's layout.
                let admitted = unsafe {
                    let c = &(*ih).inbox.0;
                    let bytes = item_charge(flat.as_ptr(), &c.seqs);
                    u.ctx.replay_item(c, CrossSend::new(flat.as_ptr()), bytes)
                };
                if !admitted {
                    tape::refuse_take(Some(group), "an item its connection cannot admit");
                    return;
                }
                continue;
            }
            Some(tape::Due::Notice { conn, notice }) => {
                let found = u.ctx.admit_parked(|b| match (b, notice) {
                    (LBody::Close(c), tape::Notice::Close) => Arc::as_ptr(c).addr() == conn,
                    (LBody::SealAnswer(c), tape::Notice::SealAnswer) => c.addr() == conn,
                    (LBody::LastRelease(r, c), tape::Notice::LastRelease { header, unused }) => {
                        Arc::as_ptr(c).addr() == conn
                            && c.retained().0 == header
                            && r.saturating_sub(header) == unused
                    }
                    _ => false,
                });
                if !found {
                    let why = match notice {
                        tape::Notice::Close => "a close another unit sent",
                        tape::Notice::SealAnswer => "a seal answer that was not published",
                        tape::Notice::LastRelease { .. } => "a last release another unit sent",
                    };
                    tape::refuse_take(Some(group), why);
                    return;
                }
                continue;
            }
        };
        let h = u.helpers[&key];
        let hctx = u
            .unstarted
            .remove(&key)
            .unwrap_or_else(|| crate::process::stop(b"UnitLedger"));
        // SAFETY: as above.
        let port = unsafe { Arc::as_ptr(&(*h).port) as usize };
        let mut ret = Value::empty();
        let ended = match outcome {
            Ok(packed) => {
                // SAFETY: the packed result has the handle's result layout.
                let result = unsafe { item_charge(packed.as_ptr(), &(*h).result.owners) };
                ret.set_item(packed, None);
                Ended {
                    failed: false,
                    causes: Vec::new(),
                    result,
                    survivors: Vec::new(),
                    run_grants: 0,
                    charges: Vec::new(),
                    tape_end: None,
                }
            }
            Err(causes) => Ended {
                failed: true,
                causes: causes.into_iter().map(CrossedCause::replayed).collect(),
                result: 0,
                survivors: Vec::new(),
                run_grants: 0,
                charges: Vec::new(),
                tape_end: None,
            },
        };
        RESULTS.lock().unwrap().insert(port, ended);
        let helper = (&*hctx as *const UnitCtx).addr();
        hctx.end_helper(Some(ret), false);
        if !u.ctx.admit_end(helper) {
            tape::refuse_take(Some(group), "a helper end that was not published");
            return;
        }
    }
}

/// Start a helper stream's replay as its own unit, on the pool beside the
/// root or else on a thread of its own, as a spawn starts a helper that runs: its context, its input unpacked
/// from the entry, its code and its tape. No unit owns it: its end is
/// published to an owner that never takes it, since its parent's replay
/// takes the end from its own stream.
/// # Safety
/// `context` is the root's, live for the run.
pub(crate) unsafe fn start_helper(
    context: *const Context,
    ready: crate::tape::helper_replay::Ready,
) {
    let owner = Box::leak(Box::new(UnitCtx::unfunded()));
    let (_, hctx) = owner.spawn_unit();
    let crate::tape::helper_replay::Ready {
        helper,
        entry,
        data,
        n,
        steps,
    } = ready;
    // SAFETY: by the contract.
    let (run, foreign, map_keys, registry) = unsafe {
        (
            (*context).run,
            (*context).foreign,
            (*context).map_keys(),
            (*context)
                .host_files
                .as_ref()
                .map_or_else(crate::host_file::Registry::default, |files| {
                    files.registry.for_unit()
                }),
        )
    };
    let job = SendPtr((
        hctx, data, entry, run, foreign, map_keys, registry, n, steps, helper,
    ));
    let start = move || {
        let job = job;
        let (hctx, data, entry, run, foreign, map_keys, registry, n, steps, helper) = job.0;
        helper_main(
            hctx,
            data,
            None,
            entry.x.bytes,
            entry.x.owned(),
            entry.r.bytes,
            entry.r.owned(),
            run,
            foreign,
            map_keys,
            registry,
            entry.bridge,
            entry.layout,
            n,
            steps,
            Arc::new(HelperShared {}),
            Some(helper),
        );
    };
    let start: crate::units::Job = Box::new(start);
    if let Err(start) = crate::units::spawn_job(start) {
        std::thread::Builder::new()
            .name("hirsel-unit".into())
            .spawn(start)
            .unwrap_or_else(|_| crate::process::stop(b"HostThreads"));
    }
}
