//! The M1 ready/gate handshake: Full offers a peer turn but never cancellation.
use crate::{Context, causes, channel::*, tasks::*};
use std::{
    process::{Command, Stdio},
    ptr,
    time::{Duration, Instant},
};

#[derive(Default)]
struct Trace {
    events: Vec<Event>,
    choices: Vec<(usize, usize)>,
    replay: Option<Vec<(usize, usize)>>,
    parent_ran: bool,
    child_started: bool,
    kind: u8,
    count: std::sync::atomic::AtomicU64,
}
unsafe extern "C" fn record(state: *mut u8, event: Event) {
    // SAFETY: callbacks exclusively borrow pinned host instrumentation.
    unsafe {
        (*state.cast::<Trace>()).events.push(event);
    }
}
unsafe extern "C" fn choose(state: *mut u8, n: usize) -> usize {
    // SAFETY: idle scheduler owns the recorder. Prefer the last candidate so a
    // yielding caller merely appended to its own queue cannot pass this test.
    unsafe {
        let trace = &mut *state.cast::<Trace>();
        let choice = if let Some(replay) = &trace.replay {
            let (count, choice) = replay[trace.choices.len()];
            assert_eq!(count, n);
            choice
        } else {
            n - 1
        };
        trace.choices.push((n, choice));
        choice
    }
}
unsafe extern "C" fn inert(_: *mut Context, _: *mut u8) -> u8 {
    0
}
fn value<T>() -> ValueLayout {
    ValueLayout {
        bytes: size_of::<T>(),
        align: align_of::<T>(),
        logical: 0,
        drop: inert,
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
        let mut unit = 0u8;
        assert_eq!(hirsel_core_try_send(input.ready, &mut unit), 0);
        assert!(!(*input.trace).parent_ran, "successful try_send switched");
        (*input.trace).child_started = true;
        if (*input.trace).kind == 1 {
            loop {
                if hirsel_core_check_cancel(ctx, 77) != 0 {
                    hirsel_core_sender_drop(input.ready);
                    hirsel_core_sender_drop(input.gate);
                    return 1;
                }
            }
        } else if (*input.trace).kind == 3 {
            while !(*input.trace).parent_ran {
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
        assert!((*ctx).cancel_requested);
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
            1024,
            (&mut child_input as *mut Input).cast(),
            child,
            TaskLayout {
                input: value::<Input>(),
                result: value::<()>(),
            },
        );
        assert!(!child.task.is_null());
        if (*trace).kind == 3 {
            while !(*trace).child_started {
                assert_eq!(hirsel_core_check_cancel(ctx, 10), 0);
            }
        }
        assert!(hirsel_core_receive(ctx, ready.receiver, &mut unit, 10).transferred);
        (*trace).parent_ran = true;
        hirsel_core_cancel(ctx, child.task);
        (*trace)
            .count
            .fetch_sub(1, std::sync::atomic::Ordering::Release);
        assert!(hirsel_core_receive(ctx, gate.receiver, &mut unit, 11).transferred);
        let joined = hirsel_core_join(ctx, child.task, ptr::null_mut(), 12);
        assert_eq!((joined.kind, joined.status), (2, 0));
        let causes = causes::Causes::from_raw(joined.causes);
        assert_eq!(
            causes.iter().map(|c| (c.kind, c.site)).collect::<Vec<_>>(),
            [(causes::CANCELLED, 77)]
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
        replay,
        kind,
        count: std::sync::atomic::AtomicU64::new(2),
        ..Trace::default()
    };
    let mut context = Context::new(8192);
    causes::task_start(&mut context).unwrap();
    let mut pointer = &mut trace as *mut Trace;
    // SAFETY: trace is pinned outside fiber storage through completion and replay.
    unsafe {
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
                    choose: Some(choose)
                }
            ),
            0
        );
    }
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
        [77]
    );
}
#[test]
fn full_schedules_a_peer_without_observing_cancel_and_replays_exactly() {
    run_bounded("0");
}
fn run_bounded(kind: &str) {
    let mut child = Command::new(std::env::current_exe().unwrap())
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
