use super::*;
use crate::{Context, block::Block};

fn snapshot(context: &Context) -> Vec<String> {
    let payer = context.current.payer.get();
    let d = payer.domain();
    let f = payer.forest();
    let slot_id = |p: Option<NonNull<Slot>>| p.map(|p| unsafe { p.as_ref().id });
    let mut rows = vec![format!(
        "current={} depth={} handles={} length={} free={:?} retired={} zeros={} pending={} attempts={} fail={:?}",
        payer.slot().id,
        context.bound_depth,
        d.handles.get(),
        f.length,
        slot_id(f.free),
        f.retired_bytes,
        f.zeros,
        d.pending.load(Ordering::Acquire),
        f.attempts,
        f.fail
    )];
    for (id, s) in f.slots().enumerate() {
        let r = f.record(Payer(NonNull::from(s)));
        let balance = unsafe { &*s.local.get() };
        rows.push(format!(
            "{id}: available={} outstanding={} refs={} parent={:?} rank={} owner={} capacity={} lexical={:?} inbox={:?} free={:?} occupied={} pending={} zeros={} next={:?}",
            balance.available, balance.outstanding, r.refs, r.parent.map(|p| p.slot().id), r.rank, r.live.owner, r.live.capacity,
            r.live.parent.map(|p| p.slot().id),
            r.occupied.then(|| unsafe { r.live.inbox.as_ref().debug_state() }),
            slot_id(r.next_free), r.occupied, s.pending.load(Ordering::Relaxed),
            s.pending_zeros.load(Ordering::Relaxed), slot_id(unsafe { *s.next_pending.get() })
        ));
    }
    rows.push(format!(
        "active={:?} reap={:?} remote={:?}",
        BOUND
            .get()
            .filter(|binding| std::ptr::eq(binding.domain.as_ptr(), d))
            .map(|binding| unsafe { binding.inbox.as_ref().debug_state() }),
        d.reap_inboxes
            .get()
            .map(|p| unsafe { p.as_ref().debug_state() }),
        d.remote_for_test().debug_state()
    ));
    rows
}

fn exit(context: &mut Context, general: bool) {
    if general {
        context.bound_depth -= 1;
        let balance = unsafe { &*context.current.raw_local() };
        let r = unsafe { &*context.current.payer.get().links() };
        if r.refs == 1 && balance.available == r.live.capacity {
            context.current.exit();
        } else {
            context.current.exit_general();
        }
    } else {
        context.limit_exit();
    }
}

#[test]
fn rank_zero_exit_matches_every_general_record_pin_rank_and_inbox() {
    fn run(general: bool, n: usize, zero: bool, nested: bool, foreign: bool) -> Vec<Vec<String>> {
        let mut ctx = Context::new(8192);
        let outer = Block::new(&ctx, Some(16), 16, 8).unwrap();
        ctx.limit_enter(4096).unwrap();
        let mut blocks: Vec<_> = (0..n)
            .map(|_| Block::new(&ctx, Some(8), 8, 8).unwrap())
            .collect();
        let zero = zero.then(|| Block::new(&ctx, Some(0), 1, 8).unwrap());
        if nested {
            ctx.limit_enter(1024).unwrap();
            blocks.push(Block::new(&ctx, Some(8), 8, 8).unwrap());
            exit(&mut ctx, general);
        }
        if foreign {
            ctx.current.task_owner(7);
        }
        let mut states = vec![snapshot(&ctx)];
        exit(&mut ctx, general);
        states.push(snapshot(&ctx));
        // Grow while a different bound is current. Admission belongs to the survivor's live payer.
        ctx.limit_enter(512).unwrap();
        if let Some(block) = blocks.first() {
            let replacement = block.replacement(&ctx, Some(24), 24, 8).unwrap();
            states.push(snapshot(&ctx));
            drop(replacement);
        }
        exit(&mut ctx, general);
        states.push(snapshot(&ctx));
        drop(blocks);
        drop(zero);
        ctx.current.drain();
        states.push(snapshot(&ctx));
        drop(outer);
        states.push(snapshot(&ctx));
        states
    }
    for n in [0, 1, 2, 8, 128] {
        for zero in [false, true] {
            for nested in [false, true] {
                for foreign in [false, true] {
                    assert_eq!(
                        run(false, n, zero, nested, foreign),
                        run(true, n, zero, nested, foreign),
                        "n={n} zero={zero} nested={nested} foreign={foreign}"
                    );
                }
            }
        }
    }
}

#[test]
fn charged_parent_boundary_and_outward_growth_keep_exact_room() {
    let mut ctx = Context::new(128);
    let outer = Block::new(&ctx, Some(16), 16, 8).unwrap();
    ctx.limit_enter(64).unwrap();
    let survivor = Block::new(&ctx, Some(8), 8, 8).unwrap();
    assert_eq!(ctx.available(), 56);
    ctx.limit_exit();
    assert_eq!(ctx.available(), 104);
    drop(survivor);
    assert_eq!(ctx.available(), 112);
    drop(outer);
    assert_eq!(ctx.available(), 128);
}

struct Foreign(Block);
// SAFETY: exclusive block custody transfers to a thread that only enqueues its refund.
unsafe impl Send for Foreign {}
impl Foreign {
    fn release(self) {
        drop(self.0);
    }
}

#[test]
fn settlement_resolves_again_after_owner_event_and_outer_rank_handover() {
    fn run(general: bool, publish_before_exit: bool) -> Vec<Vec<String>> {
        let mut ctx = Context::new(1024);
        ctx.limit_enter(768).unwrap();
        ctx.limit_enter(512).unwrap();
        ctx.limit_enter(256).unwrap();
        let kept = Block::new(&ctx, Some(8), 8, 8).unwrap();
        let sent = Foreign(Block::new(&ctx, Some(16), 16, 8).unwrap());
        // First lookup resolves to the inner scope. It cannot be retained across the next events.
        let _ = kept.payer().canonical();
        if publish_before_exit {
            std::thread::spawn(move || sent.release()).join().unwrap();
        } else {
            exit(&mut ctx, general);
            std::thread::spawn(move || sent.release()).join().unwrap();
        }
        let mut states = vec![snapshot(&ctx)];
        ctx.current.drain();
        states.push(snapshot(&ctx));
        if publish_before_exit {
            exit(&mut ctx, general);
        }
        exit(&mut ctx, general);
        exit(&mut ctx, general); // repeated outward handover keeps the rank winner
        states.push(snapshot(&ctx));
        let grown = kept.replacement(&ctx, Some(32), 32, 8).unwrap();
        states.push(snapshot(&ctx));
        drop(kept);
        drop(grown);
        states.push(snapshot(&ctx));
        assert_eq!(ctx.available(), 1024);
        states
    }
    for before in [false, true] {
        assert_eq!(run(false, before), run(true, before));
    }
}

#[test]
fn forwarded_lexical_parent_uses_the_unchanged_general_union() {
    fn run(general: bool) -> Vec<Vec<String>> {
        let root = Funding::root(1024);
        let outer = root.reserve(512).unwrap();
        let child = outer.reserve(256).unwrap();
        outer.end();
        let mut ctx = Context::with_funding(child);
        ctx.bound_depth = 1;
        let kept = Block::new(&ctx, Some(8), 8, 8).unwrap();
        let mut states = vec![snapshot(&ctx)];
        exit(&mut ctx, general);
        states.push(snapshot(&ctx));
        drop(kept);
        states.push(snapshot(&ctx));
        assert_eq!(root.available(), 1024);
        states
    }
    assert_eq!(run(false), run(true));
}

#[test]
fn a_join_result_grows_from_the_joiners_bound_while_its_funder_is_live() {
    use crate::{
        abi::*,
        channel::*,
        tasks::{
            REFUND, SPAWN, checkpoint, hirsel_core_check_cancel, hirsel_core_join,
            hirsel_core_spawn,
        },
        tasks_tests::{layout, run, value},
    };
    use std::{mem::MaybeUninit, ptr};

    #[repr(C)]
    struct List {
        data: NonNull<u8>,
        len: u64,
        capacity: u64,
    }

    struct Send {
        sender: *mut Channel,
        check: *const Cell<bool>,
    }

    unsafe extern "C" fn sibling(context: *mut Context, out: *mut u8, input: *mut u8) -> u8 {
        // SAFETY: S owns the list until the channel takes it and keeps its limit live.
        unsafe {
            let Send { sender, check } = input.cast::<Send>().read();
            let data = Block::new(&*context, Some(32), 32, 8).unwrap().into_raw();
            data.cast::<[u64; 4]>().write([0, 1, 2, 3]);
            let mut list = List {
                data,
                len: 4,
                capacity: 4,
            };
            assert!(hirsel_core_send(context, sender, (&raw mut list).cast(), 1).transferred);
            hirsel_core_sender_drop(sender);
            while !(*check).get() {
                checkpoint();
            }
            let before = (*context).available();
            hirsel_core_check_cancel(context, 2);
            let credited = (*context).available() - before;
            out.cast::<(u64, u64)>().write((before, credited));
        }
        0
    }

    unsafe extern "C" fn relay(context: *mut Context, out: *mut u8, input: *mut u8) -> u8 {
        // SAFETY: C owns the receiver and returns the received owning carrier.
        unsafe {
            let receiver = input.cast::<*mut Channel>().read();
            assert!(hirsel_core_receive(context, receiver, out, 3).transferred);
            assert_eq!(hirsel_core_receiver_drop(context, receiver), 0);
        }
        0
    }

    unsafe extern "C" fn parent(context: *mut Context, out: *mut u8, input: *mut u8) -> u8 {
        // SAFETY: P moves endpoints once and joins both handles before returning.
        unsafe {
            let bytes = input.cast::<u64>().read();
            let checked = Cell::new(false);
            let q = hirsel_core_channel_new(context, 1, value::<List>(24), 0);
            let mut send = Send {
                sender: q.sender,
                check: &checked,
            };
            let s = hirsel_core_spawn(
                context,
                0,
                128,
                (&raw mut send).cast(),
                sibling,
                layout::<Send, (u64, u64)>(16),
                0,
            );
            let mut receiver = q.receiver;
            let c = hirsel_core_spawn(
                context,
                0,
                0,
                (&raw mut receiver).cast(),
                relay,
                layout::<*mut Channel, List>(24),
                0,
            );
            let mut joined = MaybeUninit::<List>::uninit();
            assert_eq!(
                hirsel_core_join(context, c.task, joined.as_mut_ptr().cast(), 4).kind,
                0
            );
            let mut list = joined.assume_init();
            let old = list.data;
            assert!(!hirsel_core_limit_enter(context, bytes).pointer.is_null());
            let grew = crate::growth::replace(
                &*context,
                (&raw mut list).cast(),
                64,
                8,
                64,
                8,
                32,
                32,
                false,
                crate::block::PLAIN,
                true,
            );
            let paid = match grew {
                Ok(data) => {
                    let keeps_prefix = data.cast::<[u64; 4]>().read() == [0, 1, 2, 3];
                    data.cast::<u64>().as_ptr().add(4).write(4);
                    bytes == 64 && list.capacity == 8 && keeps_prefix && (*context).available() == 0
                }
                Err(cap) => {
                    bytes == 32
                        && cap
                            == Capacity {
                                requested: 64,
                                available: 32,
                            }
                        && list.data == old
                        && list.capacity == 4
                        && old.cast::<[u64; 4]>().read() == [0, 1, 2, 3]
                }
            };
            hirsel_core_limit_exit(context);
            checked.set(true);
            let mut balance = (0u64, 0u64);
            assert_eq!(
                hirsel_core_join(context, s.task, (&raw mut balance).cast(), 5).kind,
                0
            );
            drop(Block::from_raw(list.data, list.capacity * 8));
            out.cast::<u64>().write(u64::from(
                paid && balance == (96, if bytes == 64 { 32 } else { 0 }),
            ));
        }
        0
    }

    for bytes in [32u64, 64] {
        let (paid, events) = run(parent, bytes);
        assert_eq!(paid, 1, "P's bound has {bytes} bytes");
        let sibling = events.iter().find(|e| e.kind == SPAWN).unwrap().object;
        let refunds = events
            .iter()
            .filter(|e| e.kind == REFUND && e.task == 0 && e.object == sibling && e.value == 32);
        assert_eq!(refunds.count(), usize::from(bytes == 64));
    }
}
#[test]
fn flow_stage_refuses_an_item_that_cannot_enter_the_copied_bound() {
    use crate::flow::{Failure, Halt, Pull, Reference, StageCharge};

    let mut context = crate::Context::new(10_000);
    let mut flow = Reference::<u64>::default();
    let sink = flow.sink().unwrap();
    // SAFETY: the fixture owns the context and closes the bound after wiring.
    assert!(unsafe { crate::steps::hirsel_core_step_enter(&mut context, 2500) });
    let stage = flow
        .stage(
            &context,
            sink,
            4,
            StageCharge {
                output: 8,
                working: 0,
                state: 8,
                item_steps: 1000,
            },
            |private, _, value| {
                private.steps -= 900;
                Ok(Some(*value))
            },
        )
        .unwrap();
    // SAFETY: no parent steps elapsed inside the fixture's wiring bound.
    unsafe { crate::steps::hirsel_core_step_leave(&mut context) };
    let input = flow.port(&context, stage, 4, 8).unwrap();
    for x in 1..=3 {
        flow.push(input, x, x).unwrap();
    }
    flow.end(input).unwrap();
    assert_eq!(flow.next(sink), Ok(Pull::Item(1, 0, 1)));
    assert_eq!(flow.next(sink), Ok(Pull::Item(2, 0, 2)));
    assert_eq!(flow.next(sink), Ok(Pull::Dropped(0, 3)));
    assert_eq!(
        flow.finish(),
        Err(Failure {
            stage: 0,
            key: 3,
            halt: Halt::OutOfTime
        })
    );
}

#[test]
fn a_frame_charge_returns_to_its_limit_after_a_scope_outranks_the_body_record() {
    // A body admits frame bytes, then an isolate cell merges into a bound
    // scope (rank 1) whose result survives it, so that scope's exit makes its
    // record lead the body's limit (MEM-20, union by rank). Frame bytes have no block naming a
    // record, so the refund goes to that limit and no record keeps or owes
    // any of them: each record holds exactly what its live blocks hold.
    let mut ctx = Context::new(8192);
    let full = ctx.available();
    let body = ctx.current.payer.get();
    let held = unsafe { (*body.local()).outstanding };
    // SAFETY: the fixture owns its live context.
    let admitted = unsafe { crate::abi::hirsel_core_frame_admit(&ctx, 48, false) };
    assert!(!admitted.pointer.is_null());
    ctx.limit_enter(4096).unwrap();
    let scope = ctx.current.payer.get();
    let result = Block::new(&ctx, Some(16), 16, 8).unwrap();
    ctx.current.isolate_reserve(32).unwrap().finish_reserved(32);
    ctx.limit_exit();
    assert!(ctx.current.payer.get() == scope, "the scope's record leads");
    // SAFETY: the same body returns the 48 bytes it admitted.
    unsafe { crate::abi::hirsel_core_frame_refund(&ctx, 48) };
    drop(result);
    assert_eq!(ctx.available(), full);
    let f = body.forest();
    for slot in f.slots() {
        let p = Payer(NonNull::from(slot));
        let expected = if p == body { held } else { 0 };
        assert_eq!(unsafe { (*p.local()).outstanding }, expected);
    }
}
