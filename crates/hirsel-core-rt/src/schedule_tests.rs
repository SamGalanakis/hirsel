//! Executed failure-model-884 families at native ABI boundaries. The scheduler
//! records runnable choices, then replays them. The oracle is unique custody,
//! the spec's reverse cleanup and complete causes, not runtime implementation state.
use crate::{Context, abi::*, causes, channel::*, tasks::*};
use std::ptr;
#[derive(Clone, Copy, Debug)]
struct Plan {
    mask: u8,
    cancel: u8,
    after: u8,
    abandon: bool,
    fault: usize,
    sticky: bool,
}
struct Session {
    plan: Plan,
    random: u64,
    replay: Option<Vec<(usize, usize)>>,
    at: usize,
    choices: Vec<(usize, usize)>,
    events: Vec<Event>,
    ids: Vec<u64>,
    created: [u32; 2],
    dropped: [u32; 2],
    delivered: [u32; 2],
    drop_order: Vec<(u64, u64)>,
    chains: Vec<(u64, Vec<(u8, u64)>)>,
}
impl Session {
    fn new(plan: Plan, seed: u64) -> Self {
        Self {
            plan,
            random: seed,
            replay: None,
            at: 0,
            choices: vec![],
            events: vec![],
            ids: vec![],
            created: [0; 2],
            dropped: [0; 2],
            delivered: [0; 2],
            drop_order: vec![],
            chains: vec![],
        }
    }
}
unsafe extern "C" fn event(state: *mut u8, event: Event) {
    // SAFETY: idle/event callbacks access only their host-owned instrumentation.
    unsafe {
        let s = &mut *state.cast::<Session>();
        if event.kind == SPAWN {
            s.ids.push(event.object);
        }
        s.events.push(event);
    }
}
unsafe extern "C" fn choose(state: *mut u8, count: usize) -> usize {
    // SAFETY: exclusive host test recorder; no task runs while choosing.
    unsafe {
        let s = &mut *state.cast::<Session>();
        let selected = if let Some(replay) = &s.replay {
            let (previous, n) = replay[s.at];
            assert_eq!(previous, count);
            n
        } else {
            s.random = s
                .random
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (s.random >> 32) as usize % count
        };
        s.at += 1;
        s.choices.push((count, selected));
        selected
    }
}
unsafe extern "C" fn inert(_: *mut Context, _: *mut u8) -> u8 {
    0
}
fn scalar<T>(logical: u64) -> ValueLayout {
    ValueLayout {
        bytes: size_of::<T>(),
        align: align_of::<T>(),
        logical,
        drop: inert,
    }
}
fn descriptor<T>() -> TaskLayout {
    TaskLayout {
        input: scalar::<T>(0),
        result: scalar::<()>(0),
    }
}
#[derive(Clone, Copy)]
struct Message {
    cell: *mut u8,
    index: u64,
    session: *mut Session,
}
unsafe extern "C" fn drop_message(context: *mut Context, value: *mut u8) -> u8 {
    // SAFETY: exactly one sender/queue/receiver owns this message's prepaid resource.
    unsafe {
        let m = value.cast::<Message>().read();
        let s = m.session;
        (*s).dropped[m.index as usize] += 1;
        (*s).drop_order.push((current_id(), 100 + m.index));
        assert_eq!((*s).dropped[m.index as usize], 1, "double drop");
        let fail = (*s).plan.mask & (1 << m.index) != 0;
        let cell = std::ptr::NonNull::new(m.cell).unwrap();
        hirsel_core_finalizer_begin(context, cell);
        if fail {
            hirsel_core_raise(
                context,
                causes::Cause {
                    kind: causes::FAIL,
                    site: 100 + m.index,
                    message: ptr::null_mut(),
                    drop_message: None,
                },
            );
        }
        hirsel_core_finalizer_end(context, cell);
        hirsel_core_resource_free(cell, 0);
        u8::from(fail)
    }
}
fn item() -> ValueLayout {
    ValueLayout {
        drop: drop_message,
        ..scalar::<Message>(24)
    }
}
#[derive(Clone, Copy)]
struct Producer {
    session: *mut Session,
    tx: *mut Channel,
}
#[derive(Clone, Copy)]
struct Relay {
    rx: *mut Channel,
    tx: *mut Channel,
}
unsafe extern "C" fn produce(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
    // SAFETY: initialized prefix and sender are affine values on this task's stack.
    unsafe {
        let p = input.cast::<Producer>().read();
        let mut pending: [Option<Message>; 2] = [None, None];
        let mut failed = 0;
        for index in 0..2 {
            let cell = hirsel_core_resource_alloc(context, 1, 8, 0, 0).pointer;
            if cell.is_null() {
                break;
            }
            (*p.session).created[index] += 1;
            pending[index] = Some(Message {
                cell,
                index: index as u64,
                session: p.session,
            });
        }
        for m in &mut pending {
            if let Some(value) = m {
                checkpoint(false); // scripted preparation before publication
                let sent = hirsel_core_send(context, p.tx, (value as *mut Message).cast(), 11);
                failed |= sent.status;
                if sent.transferred {
                    *m = None;
                } else {
                    break;
                }
                checkpoint(false); // scripted point after queue custody committed
            }
        }
        for m in pending.iter_mut().rev().flatten() {
            failed |= drop_message(context, (m as *mut Message).cast());
        }
        hirsel_core_sender_drop(p.tx);
        failed
    }
}
unsafe extern "C" fn relay(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
    // SAFETY: receiver/sender and at most one pending message have single custody.
    unsafe {
        let r = input.cast::<Relay>().read();
        let mut failed = 0;
        loop {
            let mut m = std::mem::MaybeUninit::<Message>::uninit();
            let got = hirsel_core_receive(context, r.rx, m.as_mut_ptr().cast(), 12);
            failed |= got.status;
            if !got.transferred {
                break;
            }
            let mut m = m.assume_init();
            checkpoint(false);
            let sent = hirsel_core_send(context, r.tx, (&mut m as *mut Message).cast(), 13);
            failed |= sent.status;
            if !sent.transferred {
                failed |= drop_message(context, (&mut m as *mut Message).cast());
                break;
            }
        }
        failed |= hirsel_core_receiver_drop(context, r.rx);
        hirsel_core_sender_drop(r.tx);
        failed
    }
}
unsafe extern "C" fn consume(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
    // SAFETY: sole receiver; received messages transfer into this task's drop recipe.
    unsafe {
        let rx = input.cast::<*mut Channel>().read();
        let mut failed = 0;
        let mut previous = None;
        loop {
            let mut m = std::mem::MaybeUninit::<Message>::uninit();
            let got = hirsel_core_receive(context, rx, m.as_mut_ptr().cast(), 14);
            failed |= got.status;
            if !got.transferred {
                break;
            }
            let mut m = m.assume_init();
            if let Some(previous) = previous {
                assert!(previous < m.index, "FIFO delivery");
            }
            previous = Some(m.index);
            (*m.session).delivered[m.index as usize] += 1;
            assert_eq!(
                (*m.session).delivered[m.index as usize],
                1,
                "double delivery"
            );
            failed |= drop_message(context, (&mut m as *mut Message).cast());
            if failed != 0 {
                break;
            }
        }
        failed | hirsel_core_receiver_drop(context, rx)
    }
}
unsafe fn cleanup(
    context: *mut Context,
    handles: &mut [Option<*mut Task>; 3],
    senders: &mut [Option<*mut Channel>; 2],
    receivers: &mut [Option<*mut Channel>; 2],
) -> u8 {
    // SAFETY: these options track the exact still-owned setup prefix.
    unsafe {
        for handle in handles.iter_mut().rev().filter_map(Option::take) {
            hirsel_core_task_drop(context, handle);
        }
        for sender in senders.iter_mut().filter_map(Option::take) {
            hirsel_core_sender_drop(sender);
        }
        let mut failed = 0;
        for receiver in receivers.iter_mut().rev().filter_map(Option::take) {
            failed |= hirsel_core_receiver_drop(context, receiver);
        }
        failed
    }
}
unsafe extern "C" fn root(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
    // SAFETY: setup options describe initialized custody; a successful spawn alone
    // moves its endpoints. Refused prefixes are cleaned under the same rules.
    unsafe {
        let s = input.cast::<*mut Session>().read();
        let plan = (*s).plan;
        if plan.fault != 0 {
            (*context).current.inject(plan.fault, plan.sticky);
        }
        let mut handles = [None; 3];
        let mut senders = [None; 2];
        let mut receivers = [None; 2];
        for index in 0..2 {
            let c = hirsel_core_channel_new(context, 1, item(), 1);
            if c.sender.is_null() {
                return cleanup(context, &mut handles, &mut senders, &mut receivers);
            }
            senders[index] = Some(c.sender);
            receivers[index] = Some(c.receiver);
        }
        let mut p = Producer {
            session: s,
            tx: senders[0].unwrap(),
        };
        let h = hirsel_core_spawn(
            context,
            128,
            (&mut p as *mut Producer).cast(),
            produce,
            descriptor::<Producer>(),
        );
        if h.task.is_null() {
            return cleanup(context, &mut handles, &mut senders, &mut receivers);
        }
        handles[0] = Some(h.task);
        senders[0] = None;
        let mut r = Relay {
            rx: receivers[0].unwrap(),
            tx: senders[1].unwrap(),
        };
        let h = hirsel_core_spawn(
            context,
            128,
            (&mut r as *mut Relay).cast(),
            relay,
            descriptor::<Relay>(),
        );
        if h.task.is_null() {
            return cleanup(context, &mut handles, &mut senders, &mut receivers);
        }
        handles[1] = Some(h.task);
        receivers[0] = None;
        senders[1] = None;
        let mut rx = receivers[1].unwrap();
        let h = hirsel_core_spawn(
            context,
            128,
            (&mut rx as *mut *mut Channel).cast(),
            consume,
            descriptor::<*mut Channel>(),
        );
        if h.task.is_null() {
            return cleanup(context, &mut handles, &mut senders, &mut receivers);
        }
        handles[2] = Some(h.task);
        receivers[1] = None;
        for _ in 0..plan.after {
            checkpoint(false);
        }
        if (1..=3).contains(&plan.cancel) {
            hirsel_core_cancel(context, handles[plan.cancel as usize - 1].unwrap());
        }
        if plan.cancel == 4 {
            (*context).cancel_requested = true;
        } // inject the joiner's parent's request
        if plan.abandon {
            return cleanup(context, &mut handles, &mut senders, &mut receivers);
        }
        for index in 0..3 {
            let handle = handles[index].unwrap();
            let mut unit = 0u8;
            let join = hirsel_core_join(context, handle, &mut unit, 77);
            if join.status != 0 {
                cleanup(context, &mut handles, &mut senders, &mut receivers);
                return 1;
            }
            handles[index] = None;
            let chain = causes::Causes::from_raw(join.causes);
            let values = chain.iter().map(|c| (c.kind, c.site)).collect::<Vec<_>>();
            (*s).chains.push(((&(*s).ids)[index], values));
            drop(chain);
        }
        0
    }
}
fn execute(mut session: Session) -> Session {
    let mut context = Context::new(8192);
    causes::task_start(&mut context).unwrap();
    let mut pointer = &mut session as *mut Session;
    let mut unit = 0u8;
    // SAFETY: instrumentation is pinned outside the run; callbacks do not retain borrows.
    unsafe {
        let status = run_root(
            &mut context,
            root,
            (&mut pointer as *mut *mut Session).cast(),
            &mut unit,
            size_of::<*mut Session>(),
            0,
            SchedulerProvider {
                state: pointer.cast(),
                event: Some(event),
                choose: Some(choose),
            },
        );
        let chain = causes::Causes::from_raw(hirsel_core_causes_take(&mut context));
        let values = chain.iter().map(|c| (c.kind, c.site)).collect::<Vec<_>>();
        drop(chain);
        assert_eq!(status != 0, !values.is_empty());
        session.chains.push((0, values));
    }
    for (task, actual) in &session.chains {
        let mut expected: Vec<_> = session
            .events
            .iter()
            .filter(|e| e.kind == CANCELLED && e.task == *task)
            .map(|e| (causes::CANCELLED, e.value))
            .collect();
        expected.extend(
            session
                .drop_order
                .iter()
                .filter(|(owner, site)| {
                    owner == task && session.plan.mask & (1 << (site - 100)) != 0
                })
                .map(|(_, site)| (causes::FAIL, *site)),
        );
        assert_eq!(
            *actual, expected,
            "chain mismatch {:?}, choices {:?}",
            session.plan, session.choices
        );
    }
    assert_eq!(
        session.dropped, session.created,
        "lost custody {:?}, choices {:?}",
        session.plan, session.choices
    );
    for index in 0..2 {
        assert!(session.created[index] <= 1);
        assert!(session.delivered[index] <= session.created[index]);
    }
    assert_eq!(
        context.available(),
        8192 - 256 - if context.primary.is_some() { 32 } else { 0 },
        "leaked charge {:?}",
        session.plan
    );
    if let Some(replay) = &session.replay {
        assert_eq!(session.at, replay.len());
    }
    session
}
#[test]
fn seeded_task_custody_cancellation_cleanup_and_prepaid_publication_schedules_replay() {
    let mut runs = 0;
    for mask in 0..4 {
        for cancel in 0..5 {
            for after in 0..4 {
                for abandon in [false, true] {
                    for seed in 0..32 {
                        let plan = Plan {
                            mask,
                            cancel,
                            after,
                            abandon,
                            fault: 0,
                            sticky: false,
                        };
                        let recorded = execute(Session::new(plan, seed));
                        let mut replay = Session::new(plan, 0);
                        replay.replay = Some(recorded.choices.clone());
                        let replayed = execute(replay);
                        assert_eq!(recorded.events, replayed.events);
                        assert_eq!(recorded.chains, replayed.chains);
                        runs += 2;
                    }
                }
            }
        }
    }
    assert_eq!(runs, 10240);
}
#[test]
fn setup_and_resource_admission_refusals_preserve_every_prepared_owner() {
    let mut runs = 0;
    for mask in 0..4 {
        for fault in 1..=8 {
            for sticky in [false, true] {
                for seed in 0..16 {
                    execute(Session::new(
                        Plan {
                            mask,
                            cancel: 0,
                            after: 0,
                            abandon: false,
                            fault,
                            sticky,
                        },
                        seed,
                    ));
                    runs += 1;
                }
            }
        }
    }
    assert_eq!(runs, 1024);
}
#[test]
fn scripted_providers_suspend_at_issue_and_delivery_without_losing_issued_results() {
    use crate::provider::*;
    struct Script {
        hold: u8,
        phase: u8,
        issued: u8,
        delivered: u8,
    }
    unsafe extern "C" fn issue(state: *mut u8, request: *mut Request) -> Outcome {
        // SAFETY: test state and caller buffer are pinned across suspension.
        unsafe {
            let s = state.cast::<Script>();
            (*s).phase = 1;
            checkpoint(false);
            (*s).issued += 1;
            (*request).buffer.data.write(7);
            (*request).buffer.len = 1;
            Outcome {
                value: 1,
                ..Outcome::default()
            }
        }
    }
    unsafe extern "C" fn delivery(state: *mut u8, _: *mut Request, out: Outcome) -> Outcome {
        // SAFETY: issue completed; only delivery now borrows this script.
        unsafe {
            let s = state.cast::<Script>();
            (*s).phase = 2;
            checkpoint(false);
            (*s).delivered += 1;
        }
        out
    }
    unsafe extern "C" fn child(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
        // SAFETY: host-only script is borrowed for this test run, not a language shared value.
        unsafe {
            let s = input.cast::<*mut Script>().read();
            let provider = Provider {
                state: s.cast(),
                issue,
                deliver: delivery,
            };
            let mut byte = 0u8;
            let mut request = Request {
                operation: READ,
                mode: 0,
                handle: 0,
                path: ptr::null(),
                path_len: 0,
                buffer: Buffer {
                    data: &mut byte,
                    len: 0,
                    capacity: 1,
                },
                site: 88,
            };
            let out = hirsel_core_host(context, &provider, &mut request);
            if out.status != 0 {
                return 1;
            }
            assert_eq!((byte, request.buffer.len, out.value), (7, 1, 1));
            hirsel_core_check_cancel(context, 99)
        }
    }
    unsafe extern "C" fn root(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
        // SAFETY: root owns child handle; cancellation is requested at the scripted point.
        unsafe {
            let mut s = input.cast::<*mut Script>().read();
            let h = hirsel_core_spawn(
                context,
                0,
                (&mut s as *mut *mut Script).cast(),
                child,
                descriptor::<*mut Script>(),
            );
            while (*s).phase < (*s).hold {
                checkpoint(false);
            }
            hirsel_core_cancel(context, h.task);
            let mut unit = 0u8;
            let join = hirsel_core_join(context, h.task, &mut unit, 1);
            assert_eq!((join.kind, join.status), (2, 0));
            let causes = causes::Causes::from_raw(join.causes);
            assert_eq!(
                causes.iter().map(|c| (c.kind, c.site)).collect::<Vec<_>>(),
                [(causes::CANCELLED, if (*s).hold == 0 { 88 } else { 99 })]
            );
            drop(causes);
        }
        0
    }
    for hold in 0..=2 {
        let mut s = Script {
            hold,
            phase: 0,
            issued: 0,
            delivered: 0,
        };
        let mut pointer = &mut s as *mut Script;
        let mut context = Context::new(1024);
        causes::task_start(&mut context).unwrap();
        let before = context.available();
        let mut unit = 0u8;
        // SAFETY: root and script live through the run; result is unit.
        assert_eq!(
            unsafe {
                run_root(
                    &mut context,
                    root,
                    (&mut pointer as *mut *mut Script).cast(),
                    &mut unit,
                    size_of::<*mut Script>(),
                    0,
                    SchedulerProvider::default(),
                )
            },
            0
        );
        assert_eq!(
            (s.issued, s.delivered),
            if hold == 0 { (0, 0) } else { (1, 1) }
        );
        assert_eq!(context.available(), before);
    }
}
