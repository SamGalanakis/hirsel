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
/// end is taken. The clients its entry `x` hands over leave with it.
/// # Safety
/// As `departed`, for `x`.
pub(super) unsafe fn spawn(
    u: &mut LangUnit,
    k: usize,
    hctx: Box<UnitCtx>,
    args: Result<Vec<u8>, &'static str>,
    x: (*const u8, &[[u64; 4]]),
) {
    // SAFETY: by the contract.
    unsafe { departed(u, x.0, x.1) };
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

/// Apply one due recorded `TAKE` at this take point: returns `true` when one
/// was applied (so the caller drains its ledger arrival before checking for
/// the next `TAKE` at the same position).
/// # Safety
/// As `pump`.
pub(super) unsafe fn take_one(u: &mut LangUnit) -> bool {
    let Some(group) = u.tape().filter(|group| group.replaying()) else {
        return false;
    };
    let helpers = &u.helpers;
    let layout = |key: usize| {
        // SAFETY: a registered handle is pinned while registered.
        helpers
            .get(&key)
            .map(|&h| unsafe { ((*h).result.bytes, (*h).result.owners.clone()) })
    };
    // SAFETY: the context that bound the recording pins its host files.
    let registry = unsafe { u.files.as_ref() }.map(|f| &f.registry);
    let (key, outcome, survivors) = match tape::due(Some(group), u.pumping, layout, registry) {
        None => return false,
        Some(tape::Due::Cancel) => {
            if !u.root.is_null() {
                // SAFETY: the unit's root task is live while the unit runs.
                unsafe { tasks::hirsel_core_cancel(ptr::null_mut(), u.root) };
            }
            return true;
        }
        Some(tape::Due::End {
            key,
            outcome,
            survivors,
        }) => (key, outcome, survivors),
        Some(tape::Due::Item { conn, mut flat }) => {
            let Some(&ih) = u.inboxes.get(&conn) else {
                tape::refuse_take(Some(group), "an item for a dropped inbox");
                return false;
            };
            // SAFETY: a registered inbox is live while registered; the
            // packed item has its connection's layout, and `flat` stays
            // put until the item is published.
            let admitted = unsafe {
                let c = &(*ih).inbox.0;
                let live = packing::unpack_in_place(&mut flat, c.stride, &c.rows, c.guarded);
                let bytes = item_charge(flat.as_ptr(), live.as_deref().unwrap_or(&c.seqs));
                let mut source = CrossSend::new(flat.as_ptr());
                source.rows = live;
                u.ctx.replay_item(c, source, bytes)
            };
            if !admitted {
                tape::refuse_take(Some(group), "an item its connection cannot admit");
                return false;
            }
            return true;
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
                return false;
            }
            if matches!(notice, tape::Notice::LastRelease { .. }) {
                u.last_released = Some(conn);
            }
            return true;
        }
        Some(tape::Due::SharedRelease(node)) => {
            // SAFETY: `node` is a homed Shared node of this unit whose last
            // release was recorded at this take point.
            unsafe { crate::shared::clean(u, tasks::current_context(), node as *mut u8) };
            return true;
        }
        Some(tape::Due::ReleaseSurvivor { payer, bytes }) => {
            payer.settle_at_unit(bytes);
            return true;
        }
    };
    let h = u.helpers[&key];
    // SAFETY: `h` is a registered helper handle of this unit.
    let (r_size, held_bytes, result_owners) = unsafe {
        (
            (*h).r_size,
            (*h).charge.as_ref().map_or(0, |c| c.bytes),
            &(*h).result.owners,
        )
    };
    let mut ret = Value::empty();
    let ended = match outcome {
        Ok(packed) => {
            // SAFETY: the packed result has the handle's result layout.
            let result = unsafe { live_charge(packed.as_ptr(), result_owners) };
            ret.set_item(packed, None);
            Ended {
                failed: false,
                causes: Vec::new(),
                result,
                survivors: Vec::new(),
                run_grants: 0,
                charges: Vec::new(),
                tape_end: None,
                replay_survivors: survivors,
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
            replay_survivors: survivors,
        },
    };
    let causes_bytes: u64 = ended
        .causes
        .iter()
        .map(|cause| charge::CAUSE_CELL + cause.message_charge)
        .sum();
    let total = ended
        .replay_survivors
        .iter()
        .try_fold(0u64, |acc, s| acc.checked_add(s.amount))
        .and_then(|s| s.checked_add(r_size))
        .and_then(|s| s.checked_add(ended.result))
        .and_then(|s| s.checked_add(causes_bytes));
    if total.is_none_or(|t| t > held_bytes) {
        tape::diverged_take(Some(group), "helper end survivors exceed reservation");
        return false;
    }
    let hctx = u
        .unstarted
        .remove(&key)
        .unwrap_or_else(|| crate::process::stop(b"UnitLedger"));
    // SAFETY: as above.
    let port = unsafe { Arc::as_ptr(&(*h).port) as usize };
    RESULTS.lock().unwrap().insert(port, ended);
    let helper = (&*hctx as *const UnitCtx).addr();
    hctx.end_helper(Some(ret), false);
    if !u.ctx.admit_end(helper) {
        tape::refuse_take(Some(group), "a helper end that was not published");
        return false;
    }
    true
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
            Arc::new(HelperShared(tasks::Member::of(tasks::RunView::new()))),
            Some(helper),
        );
    };
    let start: crate::units::Job = Box::new(start);
    crate::units::spawn_language_job(start);
}

/// A replay's stand-in for a client that arrived from another unit: a
/// connection of its own with the layout of the one it stands for, whose
/// inbox no unit takes from, and its client's word. A replayed send on it
/// delivers nothing; its drop's `CLOSE` is checked against the stream. None
/// when the snapshot names no connection this build can make.
pub(crate) fn stand_in(
    cap: u64,
    logical: u64,
    b: u64,
    bytes: u64,
    owners: &[[u64; 4]],
) -> Option<(usize, u64)> {
    if cap == 0 || cap > MAX_CAP || b > MAX_ROOM {
        return None;
    }
    let stride = usize::try_from(bytes).ok()?;
    let owner = Box::leak(Box::new(UnitCtx::unfunded()));
    let (inbox, client) = owner.conn_open(cap, logical, b, stride, owners);
    let conn = Arc::as_ptr(&inbox.0) as usize;
    std::mem::forget(inbox);
    let client = Box::into_raw(Box::new(ClientH { client }));
    Some((conn, client as u64))
}

/// The clients a replaying unit's value hands to another unit, whose spawn
/// or send delivers nothing: each is dropped here, as the unit it went to
/// would have dropped it, so the connection's notices reach its owner.
/// # Safety
/// `value` holds a value laid out by `rows` that owns its client words.
pub(super) unsafe fn departed(u: &mut LangUnit, value: *const u8, rows: &[[u64; 4]]) {
    // SAFETY: by the contract.
    let rows = unsafe { live_rows(value, rows) };
    for o in rows.iter().filter(|o| o[1] == CLIENT) {
        // SAFETY: the value owns this client word.
        let h = unsafe { word(value.add(o[0] as usize)) } as *mut ClientH;
        if !h.is_null() {
            // SAFETY: as above; the handle leaves with the value. A send or
            // spawn is not a drop take point, so do not trigger `on_released`.
            u.ctx.drop_client(unsafe { Box::from_raw(h) }.client);
            let _ = u.released_own();
        }
    }
}
