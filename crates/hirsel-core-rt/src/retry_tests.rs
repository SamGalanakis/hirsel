//! The M1 ready/gate handshake: Full offers a peer turn but never cancellation.
use crate::schedule_script::{ScheduleScript, UNTAPED};
use crate::{Context, causes, channel::*, tasks::*};
use std::sync::Arc;
use std::{
    process::Stdio,
    ptr,
    time::{Duration, Instant},
};

#[derive(Default)]
struct Trace {
    events: Vec<Event>,
    choices: Vec<(usize, usize)>,
    parent_ran: bool,
    child_started: bool,
    kind: u8,
    count: std::sync::atomic::AtomicU64,
    allocation: *mut u8,
    /// Kind 5: the child's blocks of 16 and 32 bytes, then whether both are freed.
    boxes: [*mut u8; 2],
    freed: bool,
}
/// The bytes of kind 5's two blocks: `Job.a` and `Job.b` in [`job_cleanup`].
const BOX_BYTES: [u64; 2] = [16, 32];
/// The field order the derived drop glue cleans `type Job: send { a: Box[A],
/// b: Box[B] }` in, with `A` and `B` send records of 16 and 32 bytes.
fn job_cleanup() -> Vec<u32> {
    use hirsel_core_layout::{Body, Carried, Field, FieldTy, Repr, Scalar, Shape, TypeRef};
    let row = |name: &str, fields: Vec<(&str, FieldTy)>| Shape {
        name: name.into(),
        repr: Repr::Own,
        carried: Carried::Address,
        body: Body::Record(
            fields
                .into_iter()
                .map(|(name, ty)| Field {
                    name: name.into(),
                    ty,
                })
                .collect(),
        ),
        finalizer: None,
        clone: None,
        codec: None,
        grant: 0,
        send: true,
        held: None,
    };
    let words = |n: usize| {
        (0..n)
            .map(|_| ("w", FieldTy::Scalar(Scalar::U64)))
            .collect()
    };
    let shapes = [
        row(
            "Job",
            vec![
                ("a", FieldTy::Box(TypeRef(1))),
                ("b", FieldTy::Box(TypeRef(2))),
            ],
        ),
        row("A", words(2)),
        row("B", words(4)),
    ];
    let recipes = hirsel_core_layout::compute(&shapes.map(hirsel_core_layout::Row::new)).unwrap();
    recipes[0].drop.as_ref().unwrap().fields[0]
        .iter()
        .map(|f| f.field)
        .collect()
}
unsafe extern "C" fn record(state: *mut u8, event: Event) {
    // SAFETY: callbacks exclusively borrow pinned host instrumentation.
    unsafe {
        (*state.cast::<Trace>()).events.push(event);
    }
}
fn value<T>() -> ValueLayout {
    ValueLayout {
        bytes: size_of::<T>(),
        align: align_of::<T>(),
        logical: 0,
        clean: None,
    }
}
#[derive(Clone, Copy)]
struct Input {
    ready: *mut Channel,
    gate: *mut Channel,
    trace: *mut Trace,
}
unsafe extern "C" fn child(ctx: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
    // SAFETY: owns both senders; the parent pins trace until join consumes this task.
    unsafe {
        let input = input.cast::<Input>().read();
        if (*input.trace).kind == 4 {
            let block = crate::block::Block::new(&*ctx, Some(800), 800, 8).unwrap();
            (*input.trace).allocation = block.into_raw().as_ptr();
        }
        if (*input.trace).kind == 5 {
            for (slot, bytes) in (*input.trace).boxes.iter_mut().zip(BOX_BYTES) {
                let block = crate::block::Block::new(&*ctx, Some(bytes), bytes as usize, 8);
                *slot = block.unwrap().into_raw().as_ptr();
            }
        }
        let mut unit = 0u8;
        assert_eq!(hirsel_core_try_send(input.ready, &mut unit), 0);
        assert!(!(*input.trace).parent_ran, "successful try_send switched");
        (*input.trace).child_started = true;
        if (*input.trace).kind == 4 {
            loop {
                match crate::block::Block::new(&*ctx, Some(800), 800, 8) {
                    Ok(block) => {
                        drop(block);
                        break;
                    }
                    Err(_) => assert!((*ctx).causes.iter().next().is_none()),
                }
            }
            hirsel_core_sender_drop(input.ready);
            hirsel_core_sender_drop(input.gate);
            return 0;
        } else if (*input.trace).kind == 5 {
            // Stay live, so each refund crosses to this task's limit: yield at
            // each cancel check until the parent has freed every box.
            loop {
                if (*input.trace).freed {
                    break;
                }
                assert_eq!(hirsel_core_check_cancel(ctx, 76), 0);
            }
            hirsel_core_sender_drop(input.ready);
            hirsel_core_sender_drop(input.gate);
            return 0;
        } else if (*input.trace).kind == 1 {
            loop {
                if hirsel_core_check_cancel(ctx, 77) != 0 {
                    hirsel_core_sender_drop(input.ready);
                    hirsel_core_sender_drop(input.gate);
                    return 1;
                }
            }
        } else if (*input.trace).kind == 3 {
            loop {
                if (*input.trace).parent_ran {
                    break;
                }
                assert_eq!(hirsel_core_check_cancel(ctx, 76), 0);
            }
        } else if (*input.trace).kind == 2 {
            loop {
                let unique = (*input.trace)
                    .count
                    .load(std::sync::atomic::Ordering::Acquire)
                    == 1;
                // Match the inline engines' frozen context gate and hook.
                if (*ctx).events != 0 {
                    crate::abi::hirsel_core_arc_unwrapped(ctx, unique);
                }
                if unique {
                    break;
                }
                assert!((*ctx).causes.iter().next().is_none());
            }
        } else {
            let mut full = false;
            loop {
                match hirsel_core_try_send(input.gate, &mut unit) {
                    0 => break,
                    1 => {
                        full = true;
                        assert!(
                            (*ctx).causes.iter().next().is_none(),
                            "Full observed cancellation"
                        );
                    }
                    _ => panic!("gate unexpectedly closed"),
                }
            }
            assert!(full);
        }
        assert!((*input.trace).parent_ran);
        assert!(
            (*ctx)
                .cancel_requested
                .load(std::sync::atomic::Ordering::Acquire)
        );
        let failed = hirsel_core_check_cancel(ctx, 77);
        assert_eq!(failed, 1);
        hirsel_core_sender_drop(input.ready);
        hirsel_core_sender_drop(input.gate);
        failed
    }
}
unsafe extern "C" fn root(ctx: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
    // SAFETY: root owns channels and child handle, cleans them on the only exit.
    unsafe {
        let trace = input.cast::<*mut Trace>().read();
        let initial = (*ctx).available();
        let ready = hirsel_core_channel_new(ctx, 1, value::<u8>(), 1);
        let gate = hirsel_core_channel_new(ctx, 1, value::<u8>(), 2);
        assert!(!ready.sender.is_null() && !gate.sender.is_null());
        let mut unit = 0;
        assert_eq!(hirsel_core_try_send(gate.sender, &mut unit), 0);
        let mut child_input = Input {
            ready: ready.sender,
            gate: gate.sender,
            trace,
        };
        let child = hirsel_core_spawn(
            ctx,
            0,
            1024,
            (&mut child_input as *mut Input).cast(),
            child,
            TaskLayout {
                input: value::<Input>(),
                result: value::<()>(),
            },
            0,
        );
        assert!(!child.task.is_null());
        if (*trace).kind == 3 {
            loop {
                if (*trace).child_started {
                    break;
                }
                assert_eq!(hirsel_core_check_cancel(ctx, 10), 0);
            }
        }
        assert!(hirsel_core_receive(ctx, ready.receiver, &mut unit, 10).transferred);
        (*trace).parent_ran = true;
        if (*trace).kind == 4 {
            // Foreign drop queues the refund to the child; it is not visible
            // until a later semantic event owned by the child.
            crate::abi::hirsel_core_free(ptr::NonNull::new((*trace).allocation).unwrap(), 800);
        } else if (*trace).kind == 5 {
            // The parent drops a Job the child built: the glue frees its boxes
            // in the recipe's field order, each refund an event (MEM-20).
            for field in job_cleanup() {
                let i = field as usize;
                let block = ptr::NonNull::new((*trace).boxes[i]).unwrap();
                crate::abi::hirsel_core_free(block, BOX_BYTES[i]);
            }
            (*trace).freed = true;
        } else {
            hirsel_core_cancel(ctx, child.task);
        }
        (*trace)
            .count
            .fetch_sub(1, std::sync::atomic::Ordering::Release);
        assert!(hirsel_core_receive(ctx, gate.receiver, &mut unit, 11).transferred);
        let joined = hirsel_core_join(ctx, child.task, ptr::null_mut(), 12);
        assert_eq!(
            (joined.kind, joined.status),
            (if (*trace).kind >= 4 { 0 } else { 2 }, 0)
        );
        let causes = causes::Causes::from_raw(joined.causes);
        assert_eq!(
            causes
                .iter()
                .map(|c| (c.kind(), c.site))
                .collect::<Vec<_>>(),
            if (*trace).kind >= 4 {
                vec![]
            } else {
                vec![(causes::CANCELLED, 77)]
            }
        );
        drop(causes);
        assert_eq!(hirsel_core_receiver_drop(ctx, ready.receiver), 0);
        assert_eq!(hirsel_core_receiver_drop(ctx, gate.receiver), 0);
        assert_eq!((*ctx).available(), initial);
        0
    }
}
fn execute(kind: u8, replay: Option<Vec<(usize, usize)>>, recording: bool) -> Trace {
    let mut trace = Trace {
        kind,
        count: std::sync::atomic::AtomicU64::new(2),
        ..Trace::default()
    };
    let mut script = ScheduleScript::last();
    if let Some(prefix) = &replay {
        script =
            script.with_schedule_prefix(UNTAPED, prefix.iter().map(|&(_, index)| index).collect());
    }
    let script = Arc::new(script);
    let mut context = Context::new(8192);
    causes::task_start(&mut context).unwrap();
    let mut pointer = &mut trace as *mut Trace;
    // SAFETY: trace is pinned outside fiber storage through completion and replay.
    script.install(|| unsafe {
        assert_eq!(
            run_root(
                &mut context,
                root,
                (&mut pointer as *mut *mut Trace).cast(),
                ptr::null_mut(),
                size_of::<*mut Trace>(),
                0,
                SchedulerProvider {
                    state: pointer.cast(),
                    event: recording.then_some(record),
                    ..SchedulerProvider::default()
                }
            ),
            0
        );
    });
    trace.choices = script.schedule_decisions(UNTAPED);
    trace
}
#[test]
#[ignore = "subprocess with a parent-owned timeout"]
fn gate_replay_child() {
    let kind = std::env::var("HIRSEL_RETRY_KIND").unwrap().parse().unwrap();
    // Exercise the unrecorded path first so a no-switch mutant cannot grow an
    // event log indefinitely before the parent's test-side timeout fires.
    execute(kind, None, false);
    let recorded = execute(kind, None, true);
    let replayed = execute(kind, Some(recorded.choices.clone()), true);
    assert_eq!(recorded.events, replayed.events);
    assert_eq!(recorded.choices, replayed.choices);
    assert_eq!(
        recorded
            .events
            .iter()
            .filter(|e| e.kind == CANCELLED)
            .map(|e| e.value)
            .collect::<Vec<_>>(),
        if kind >= 4 { vec![] } else { vec![77] }
    );
    if kind == 5 {
        // FAIL-8 by hand: Job's fields clean in reverse declaration order, so
        // b's 32 bytes refund to the child before a's 16 (SEM-8: event order
        // is observable).
        let child = recorded.events.iter().find(|e| e.kind == SPAWN).unwrap();
        assert_eq!(
            recorded
                .events
                .iter()
                .filter(|e| e.kind == REFUND && e.object == child.object)
                .map(|e| e.value)
                .collect::<Vec<_>>(),
            [32, 16]
        );
    }
}
#[test]
fn full_schedules_a_peer_without_observing_cancel_and_replays_exactly() {
    run_bounded("0");
}
fn run_bounded(kind: &str) {
    let mut child = crate::test_process::command()
        .args([
            "--exact",
            "retry_tests::gate_replay_child",
            "--ignored",
            "--nocapture",
        ])
        .env("HIRSEL_RETRY_KIND", kind)
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "{status}");
            break;
        }
        if started.elapsed() > Duration::from_secs(5) {
            // This Child is the process we just started, never a discovered PID.
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("Full gate did not return within the test's five-second timeout");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn no_request_check_polls_fairly_and_records_each_check() {
    run_bounded("1");
}
#[test]
fn failed_arc_unwrap_polls_fairly_without_cancellation_and_replays() {
    run_bounded("2");
}
#[test]
fn all_polling_peers_remain_runnable_when_the_ready_queue_is_empty() {
    run_bounded("3");
}
#[test]
fn capacity_retries_observe_foreign_refunds_only_at_owner_events_and_replay() {
    run_bounded("4");
}
/// Bug: a `send` record cleans its fields in declaration order, so the refunds
/// its boxes owe another task's limit arrive in the wrong event order.
#[test]
fn a_send_records_foreign_refunds_follow_reverse_field_order() {
    run_bounded("5");
}
