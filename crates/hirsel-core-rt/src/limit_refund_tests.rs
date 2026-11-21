use super::*;
use crate::{Context, block::Block};

#[test]
fn flow_charges_follow_their_record_through_bound_exit() {
    use crate::flow::{Reference, StageCharge, WireError};

    for finish in [false, true] {
        let mut context = Context::new(16_000_064);
        let start = context.available();
        let mut flow = Reference::<u64>::default();
        let sink = flow.sink().unwrap();
        context.limit_enter(2_000).unwrap();
        let stage = flow
            .stage(
                &context,
                sink,
                2,
                StageCharge {
                    output: 8,
                    working: 300,
                    state: 8,
                    item_steps: 1000,
                },
                |_, _, value| Ok(Some(*value)),
            )
            .unwrap();
        flow.port(&context, stage, 4, 8).unwrap();
        context.limit_exit();
        assert_eq!(context.available(), start - 900);
        if finish {
            assert_eq!(flow.finish(), Ok(()));
        } else {
            drop(flow);
        }
        assert_eq!(context.available(), start);
    }

    let context = Context::new(16_000_063);
    let mut flow = Reference::<()>::default();
    let sink = flow.sink().unwrap();
    assert!(matches!(
        flow.port(&context, sink, 1_000_000, 0),
        Err(WireError::Refused(crate::Capacity {
            requested: 16_000_064,
            available: 16_000_063
        }))
    ));
    let large = StageCharge {
        output: 0,
        working: 0,
        state: 1024,
        item_steps: 1000,
    };
    let too_small = Context::new(1423);
    assert!(matches!(
        flow.stage(&too_small, sink, 1, large, |_, _, _| Ok(None)),
        Err(WireError::Refused(crate::Capacity {
            requested: 1424,
            available: 1423
        }))
    ));
    let overflow = StageCharge {
        output: 0,
        working: u64::MAX - 100,
        state: 0,
        item_steps: 1000,
    };
    assert!(matches!(
        flow.stage(&context, sink, 1, overflow, |_, _, _| Ok(None)),
        Err(WireError::Refused(crate::Capacity {
            requested: u64::MAX,
            available: 16_000_063
        }))
    ));
    assert_eq!(context.available(), 16_000_063);
}

#[test]
fn flow_charges_key_slots_and_large_stage_state_at_the_exact_limit() {
    use crate::flow::{Reference, StageCharge};

    let context = Context::new(16_000_064);
    let mut flow = Reference::<()>::default();
    let sink = flow.sink().unwrap();
    flow.port(&context, sink, 1_000_000, 0).unwrap();
    assert_eq!(context.available(), 0);
    drop(flow);
    assert_eq!(context.available(), 16_000_064);

    let context = Context::new(1424);
    let mut flow = Reference::<()>::default();
    let sink = flow.sink().unwrap();
    let state = [0u64; 128];
    flow.stage(
        &context,
        sink,
        1,
        StageCharge {
            output: 0,
            working: 0,
            state: size_of_val(&state) as u64,
            item_steps: 1000,
        },
        move |_, key, _| Ok(state[key as usize].checked_add(1).map(|_| ())),
    )
    .unwrap();
    assert_eq!(context.available(), 0);
    assert_eq!(flow.finish(), Ok(()));
    assert_eq!(context.available(), 1424);
}

struct Foreign(Vec<Block>);
// SAFETY: sole affine custody moves to the worker; its only operation is drop,
// which publishes refunds under the domain mutex without touching live records.
unsafe impl Send for Foreign {}
impl Foreign {
    fn release(self) {
        drop(self.0);
    }
}

#[test]
fn nested_limits_keep_their_charges_through_growth_and_exit() {
    let mut context = Context::new(4096);
    let mut blocks = Vec::new();
    for _ in 0..100 {
        context.limit_enter(context.available() - 1).unwrap();
        blocks.push(Block::new(&context, Some(2), 2, 8).unwrap());
    }
    for _ in 0..100 {
        let available = context.available();
        assert_eq!(
            context.limit_enter(available + 1),
            Err(crate::Capacity {
                requested: available + 1,
                available,
            })
        );
        context.limit_exit();
        assert_eq!(context.available(), available + 1);
    }
    assert_eq!(context.available(), 3896);
    let foreign = Foreign(blocks);
    std::thread::spawn(move || foreign.release())
        .join()
        .unwrap();
    assert_eq!(context.available(), 3896);
    context.current.drain();
    assert_eq!(context.available(), 4096);
}

#[test]
fn dirty_inbox_aggregates_bytes_and_zero_pins_among_many_clean_records() {
    let context = Context::new(64);
    let clean: Vec<_> = (0..512)
        .map(|_| context.current.reserve(0).unwrap())
        .collect();
    let foreign = Foreign(vec![
        Block::new(&context, Some(8), 8, 8).unwrap(),
        Block::new(&context, Some(8), 8, 8).unwrap(),
        Block::new(&context, Some(0), 1, 8).unwrap(),
    ]);
    std::thread::spawn(move || foreign.release())
        .join()
        .unwrap();
    assert_eq!(context.available(), 48);
    let domain = context.current.payer.get().domain();
    assert_eq!(domain.remote.lock().unwrap().queued_slots(), 1);
    context.current.drain();
    assert_eq!(context.available(), 64);
    assert!(!domain.remote.lock().unwrap().pending());
    assert_eq!(context.current.payer.get().forest().zeros, 0);
    drop(clean);
}

#[test]
fn many_handed_over_limits_refund_only_at_the_recipient_event() {
    const CHILDREN: u64 = 96;
    let root = Funding::root(CHILDREN * 32);
    let mut children = Vec::new();
    let mut early = Vec::new();
    let mut late = Vec::new();
    for owner in 1..=CHILDREN {
        let child = root.reserve(32).unwrap();
        child.task_owner(owner);
        let context = Context::with_funding(child);
        early.push(Block::new(&context, Some(8), 8, 8).unwrap());
        late.push(Block::new(&context, Some(4), 4, 4).unwrap());
        children.push(context);
    }
    let early = Foreign(early);
    std::thread::spawn(move || early.release()).join().unwrap();
    root.drain();
    assert_eq!(root.available(), 0);
    for (index, child) in children.iter_mut().enumerate() {
        assert_eq!(child.available(), 20);
        child.current.handover(index as u64 + 1);
        // Each handover is an eligible event for prior credits, including
        // the early refund, but its own credit still has to be delivered.
        assert_eq!(root.available(), index as u64 * 28);
    }
    let late = Foreign(late);
    std::thread::spawn(move || late.release()).join().unwrap();
    assert_eq!(root.available(), (CHILDREN - 1) * 28);
    root.drain();
    assert_eq!(root.available(), CHILDREN * 32);
    drop(children);
    assert_eq!(root.available(), CHILDREN * 32);
}

#[test]
fn owner_inboxes_and_handover_preserve_the_next_owner_event() {
    let root = Funding::root(64);
    let child = root.reserve(32).unwrap();
    child.task_owner(1);
    let mut context = Context::with_funding(child);
    let pending = |owner| {
        root.payer
            .get()
            .domain()
            .remote
            .lock()
            .unwrap()
            .pending_for(owner)
    };
    drop(Block::new(&context, Some(8), 8, 8).unwrap());
    assert!(!pending(0));
    assert!(pending(1));
    root.drain();
    assert_eq!((root.available(), context.available()), (32, 24));
    context.current.handover(1);
    assert_eq!(root.available(), 32); // The hand-over credit awaits the recipient event.
    assert!(pending(0));
    assert!(!pending(1));
    root.drain();
    assert_eq!(root.available(), 64);
    assert!(!pending(0));
}

#[test]
fn foreign_thread_routing_and_owner_reassignment_leave_no_stale_post() {
    let root = Funding::root(64);
    let child = root.reserve(32).unwrap();
    child.task_owner(1);
    let context = Context::with_funding(child);
    let pending = |owner| {
        root.payer
            .get()
            .domain()
            .remote
            .lock()
            .unwrap()
            .pending_for(owner)
    };
    let foreign = Foreign(vec![Block::new(&context, Some(8), 8, 8).unwrap()]);
    std::thread::spawn(move || foreign.release())
        .join()
        .unwrap();
    assert!(pending(0));
    root.drain(); // Routes the OS-thread publication, but cannot deliver owner 1.
    assert!(!pending(0));
    assert!(pending(1));
    assert_eq!(context.available(), 24);
    context.current.task_owner(0);
    assert_eq!(context.available(), 24); // Reassignment routes; it never refunds.
    assert!(pending(0));
    root.drain();
    assert_eq!(context.available(), 32);
    assert!(!pending(0));
    context.current.end();
    assert_eq!(root.available(), 64);
}

#[test]
fn finished_owner_inboxes_are_reclaimed_under_task_churn() {
    let root = Funding::root(64);
    for owner in 1..1000 {
        let mut child = root.reserve(0).unwrap();
        child.task_owner(owner);
        child.handover(owner);
        drop(child);
        root.drain_notified();
        assert_eq!(root.available(), 64);
        assert_eq!(
            root.payer
                .get()
                .domain()
                .remote
                .lock()
                .unwrap()
                .inbox_count(),
            1
        );
    }
}

#[test]
fn returned_grant_storage_grows_from_the_finalizers_task_after_hand_back() {
    use crate::{
        abi::*,
        channel::*,
        tasks::{
            CHECK_CANCEL, REFUND, SPAWN, checkpoint, hirsel_core_check_cancel, hirsel_core_join,
            hirsel_core_spawn,
        },
        tasks_tests::{layout, run, value},
    };
    use std::mem::MaybeUninit;

    struct Input {
        resource: NonNull<u8>,
        parent_spends_first: bool,
    }

    unsafe extern "C" fn finalizing_task(
        context: *mut Context,
        out: *mut u8,
        input: *mut u8,
    ) -> u8 {
        // SAFETY: A owns the resource and both ends of the buffered channel.
        unsafe {
            let input = input.cast::<Input>().read();
            let q = hirsel_core_channel_new(context, 1, value::<NonNull<u8>>(8), 0);
            hirsel_core_finalizer_begin(context, input.resource);
            let mut data = Block::new(&*context, Some(32), 32, 8).unwrap().into_raw();
            data.cast::<[u64; 4]>().write([0, 1, 2, 3]);
            assert!(hirsel_core_send(context, q.sender, (&raw mut data).cast(), 1).transferred);
            hirsel_core_finalizer_end(context, input.resource);
            let mut data = MaybeUninit::<NonNull<u8>>::uninit();
            assert!(
                hirsel_core_receive(context, q.receiver, data.as_mut_ptr().cast(), 2).transferred
            );
            let old = Block::from_raw(data.assume_init(), 32);
            if input.parent_spends_first {
                checkpoint();
            }
            let before = (*context).available();
            let grown = old.replacement(&*context, Some(64), 64, 8);
            let paid = grown.is_ok() && (*context).available() == before - 64;
            if let Ok(grown) = &grown {
                grown
                    .pointer()
                    .cast::<[u64; 4]>()
                    .write(old.pointer().cast::<[u64; 4]>().read());
            }
            drop(old);
            hirsel_core_check_cancel(context, 7);
            if !input.parent_spends_first {
                checkpoint();
            }
            drop(grown);
            hirsel_core_resource_free(input.resource, 0);
            hirsel_core_sender_drop(q.sender);
            assert_eq!(hirsel_core_receiver_drop(context, q.receiver), 0);
            out.cast::<u64>().write(u64::from(paid));
        }
        0
    }

    unsafe extern "C" fn parent(context: *mut Context, out: *mut u8, input: *mut u8) -> u8 {
        // SAFETY: P moves the sole resource into A, then joins A's plain result.
        unsafe {
            let resource =
                NonNull::new(hirsel_core_resource_alloc(context, 1, 8, 0, 32).pointer).unwrap();
            let mut input = Input {
                resource,
                parent_spends_first: input.cast::<bool>().read(),
            };
            let a = hirsel_core_spawn(
                context,
                0,
                4096,
                (&raw mut input).cast(),
                finalizing_task,
                layout::<Input, u64>(8),
                0,
            );
            checkpoint();
            let spend = Block::new(&*context, Some(8), 8, 8).unwrap();
            assert_eq!(hirsel_core_join(context, a.task, out, 8).kind, 0);
            drop(spend);
        }
        0
    }

    for parent_spends_first in [false, true] {
        let (paid, events) = run(parent, parent_spends_first);
        assert_eq!(paid, 1);
        let a = events.iter().find(|e| e.kind == SPAWN).unwrap().object;
        let refunds: Vec<_> = events
            .iter()
            .take_while(|e| !(e.task == a && e.kind == CHECK_CANCEL))
            .filter(|e| e.kind == REFUND && e.task == a && e.object == 0)
            .map(|e| e.value)
            .collect();
        assert_eq!(refunds, [0, 32]);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum HandoverOrder {
    BeforeCheck,
    AfterCheck,
    BeforeShare,
    AfterShare,
    RefundBeforeEnd,
}

fn handover_order(order: HandoverOrder) -> (u64, Vec<crate::tasks::Event>) {
    use crate::{
        abi::*,
        tasks::{
            checkpoint, current_id, hirsel_core_check_cancel, hirsel_core_join, hirsel_core_spawn,
        },
        tasks_tests::{layout, run},
    };
    use std::ptr;

    struct ChildInput {
        reservation: *mut Funding,
        charged: *const Cell<Option<NonNull<u8>>>,
    }

    unsafe extern "C" fn child(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
        // SAFETY: C owns the passed reservation; the refund target and slot are joined before P exits.
        unsafe {
            let input = input.cast::<ChildInput>().read();
            let mut reservation = Box::from_raw(input.reservation);
            reservation.task_owner(current_id());
            if !input.charged.is_null() {
                let block = Block::paid(
                    &*context,
                    reservation.payer.get(),
                    Some(8),
                    8,
                    8,
                    crate::block::PLAIN,
                )
                .unwrap()
                .into_raw();
                (*input.charged).set(Some(block));
            }
            reservation.handover(current_id());
            checkpoint();
        }
        0
    }

    unsafe extern "C" fn sibling(_: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
        // SAFETY: C transfers the sole block into the slot before S runs.
        unsafe {
            let slot = input.cast::<*const Cell<Option<NonNull<u8>>>>().read();
            let block = (*slot).take().unwrap();
            drop(Block::from_raw(block, 8));
        }
        0
    }

    unsafe extern "C" fn parent(context: *mut Context, out: *mut u8, input: *mut u8) -> u8 {
        // SAFETY: P transfers the reservation only once and joins both children, pinning the slot.
        unsafe {
            let order = input.cast::<HandoverOrder>().read();
            let charged = Cell::new(None);
            let mut info = ChildInput {
                reservation: Box::into_raw(Box::new((*context).current.reserve(8).unwrap())),
                charged: if order == HandoverOrder::RefundBeforeEnd {
                    &charged
                } else {
                    ptr::null()
                },
            };
            let c = hirsel_core_spawn(
                context,
                0,
                0,
                (&raw mut info).cast(),
                child,
                layout::<ChildInput, ()>(0),
                0,
            );
            let s = if order == HandoverOrder::RefundBeforeEnd {
                let mut slot: *const Cell<Option<NonNull<u8>>> = &charged;
                Some(hirsel_core_spawn(
                    context,
                    0,
                    0,
                    (&raw mut slot).cast(),
                    sibling,
                    layout::<*const Cell<Option<NonNull<u8>>>, ()>(0),
                    0,
                ))
            } else {
                None
            };
            let held = Block::new(&*context, Some((*context).available()), 1, 8).unwrap();
            let share = matches!(
                order,
                HandoverOrder::BeforeShare | HandoverOrder::AfterShare
            );
            let after = matches!(order, HandoverOrder::AfterCheck | HandoverOrder::AfterShare);
            let commit = || {
                if share {
                    let count = std::sync::atomic::AtomicU64::new(1);
                    count.fetch_add(1, Ordering::Relaxed);
                    hirsel_core_arc_shared(context, (&raw const count).cast());
                } else {
                    hirsel_core_check_cancel(context, 10);
                }
            };
            if after {
                commit();
            }
            checkpoint();
            if !after {
                commit();
            }
            let first = Block::new(&*context, Some(8), 8, 8);
            let correct = if after {
                matches!(
                    first,
                    Err(Capacity {
                        requested: 8,
                        available: 0
                    })
                ) && Block::new(&*context, Some(8), 8, 8).is_ok()
            } else {
                first.is_ok()
            };
            drop(first);
            assert_eq!(
                hirsel_core_join(context, c.task, ptr::null_mut(), 11).kind,
                0
            );
            if let Some(s) = s {
                assert_eq!(
                    hirsel_core_join(context, s.task, ptr::null_mut(), 12).kind,
                    0
                );
            }
            drop(held);
            out.cast::<u64>().write(u64::from(correct));
        }
        0
    }

    run(parent, order)
}

#[test]
fn handover_is_visible_at_the_next_event_before_the_child_ends() {
    use crate::tasks::{CAPACITY, CHECK_CANCEL, END, REFUND, SPAWN};
    for (order, want) in [
        (
            HandoverOrder::BeforeCheck,
            vec![(REFUND, 8), (CHECK_CANCEL, 0)],
        ),
        (
            HandoverOrder::AfterCheck,
            vec![(CHECK_CANCEL, 0), (REFUND, 8), (CAPACITY, 0)],
        ),
    ] {
        let (paid, events) = handover_order(order);
        assert_eq!(paid, 1);
        let c = events.iter().find(|e| e.kind == SPAWN).unwrap().object;
        let prefix: Vec<_> = events
            .iter()
            .take_while(|e| !(e.task == c && e.kind == END))
            .filter(|e| {
                (e.task == c && e.kind == REFUND && e.value == 8)
                    || (e.task == 0 && matches!(e.kind, CHECK_CANCEL | CAPACITY))
            })
            .map(|e| (e.kind, e.value))
            .collect();
        assert_eq!(prefix, want);
    }
}

#[test]
fn private_arc_share_folds_only_credits_posted_before_it() {
    use crate::tasks::{ARC_SHARE, CAPACITY, REFUND, SPAWN};
    for (order, want) in [
        (
            HandoverOrder::BeforeShare,
            vec![(REFUND, 8), (ARC_SHARE, 2)],
        ),
        (
            HandoverOrder::AfterShare,
            vec![(ARC_SHARE, 2), (REFUND, 8), (CAPACITY, 0)],
        ),
    ] {
        let (paid, events) = handover_order(order);
        assert_eq!(paid, 1);
        let child = events
            .iter()
            .find(|event| event.kind == SPAWN)
            .unwrap()
            .object;
        let prefix: Vec<_> = events
            .iter()
            .filter(|event| {
                (event.task == child && event.kind == REFUND && event.value == 8)
                    || (event.task == 0 && matches!(event.kind, ARC_SHARE | CAPACITY))
            })
            .map(|event| (event.kind, event.value))
            .collect();
        assert_eq!(prefix, want);
    }
}

#[test]
fn a_refund_after_handover_and_before_end_goes_to_the_next_owner_event() {
    use crate::tasks::{CHECK_CANCEL, END, REFUND, SPAWN};

    let (paid, events) = handover_order(HandoverOrder::RefundBeforeEnd);
    assert_eq!(paid, 1);
    let mut children = events.iter().filter(|e| e.kind == SPAWN).map(|e| e.object);
    let c = children.next().unwrap();
    let s = children.next().unwrap();
    let check = events
        .iter()
        .position(|e| e.task == 0 && e.kind == CHECK_CANCEL)
        .unwrap();
    let end = events
        .iter()
        .position(|e| e.task == c && e.kind == END)
        .unwrap();
    assert!(check < end);
    let refunds: Vec<_> = events[..check]
        .iter()
        .filter(|e| e.kind == REFUND && e.object == 0 && (e.task == c || e.value == 8))
        .map(|e| (e.task, e.value))
        .collect();
    assert_eq!(refunds, [(c, 0), (s, 8)]);
}

/// Routing law: a queued slot is always on the inbox of its root's owner. A
/// reassignment moves exactly the moved root's slots, including slots of
/// records joined under it before or after they were queued; a hand-over
/// moves the child's slots to the destination root. Red on a reassignment
/// that leaves the root's slots on the old owner (or moves only the root's own
/// slot), on one that moves every slot of the old inbox, and on a union or
/// hand-over that leaves the joined record's slots behind.
#[test]
fn reassignment_and_handover_move_exactly_the_moved_roots_queued_slots() {
    let root = Funding::root(96);
    let routed = || root.payer.get().domain().remote.lock().unwrap().routed();
    let a = root.reserve(32).unwrap();
    a.task_owner(1);
    let b = root.reserve(32).unwrap();
    b.task_owner(1);
    let mut ca = Context::with_funding(a);
    let mut cb = Context::with_funding(b);
    let id = |f: &Funding| f.payer.get().slot().id;
    let (r0, ra, rb) = (id(&root), id(&ca.current), id(&cb.current));
    // c is queued before it joins a; d joins a before it is queued.
    ca.limit_enter(16).unwrap();
    let rc = id(&ca.current);
    drop(Block::new(&ca, Some(8), 8, 8).unwrap());
    let kept = Block::new(&ca, Some(8), 8, 8).unwrap();
    assert_eq!(routed(), [(1, vec![(rc, vec![rc])])]);
    ca.limit_exit();
    ca.limit_enter(16).unwrap();
    let rd = id(&ca.current);
    let late = Block::new(&ca, Some(8), 8, 8).unwrap();
    ca.limit_exit();
    drop((kept, late));
    drop(Block::new(&ca, Some(8), 8, 8).unwrap());
    drop(Block::new(&cb, Some(8), 8, 8).unwrap());
    let mut a_slots = vec![ra, rc, rd];
    a_slots.sort();
    assert_eq!(routed(), [(1, vec![(ra, a_slots.clone()), (rb, vec![rb])])]);
    ca.current.task_owner(2);
    assert_eq!(
        routed(),
        [(1, vec![(rb, vec![rb])]), (2, vec![(ra, a_slots.clone())])]
    );
    cb.current.handover(1);
    assert_eq!(
        routed(),
        [(0, vec![(r0, vec![rb])]), (2, vec![(ra, a_slots.clone())])]
    );
    ca.current.task_owner(0);
    let mut both = vec![(r0, vec![rb]), (ra, a_slots)];
    both.sort();
    assert_eq!(routed(), [(0, both)]);
    root.drain();
    assert_eq!(routed(), []);
    // No refund was lost or doubled: every byte is back at its limit.
    assert_eq!((root.available(), ca.available()), (64, 32));
    ca.current.end();
    assert_eq!(root.available(), 96);
}
