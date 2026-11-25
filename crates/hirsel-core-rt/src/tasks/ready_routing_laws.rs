use super::*;
use crate::provider::*;
use std::{io::Write, os::unix::net::UnixStream};
unsafe extern "C" fn no_body(_: *mut Context, _: *mut u8, _: *mut u8) -> u8 {
    0
}
unsafe extern "C" fn wait(_: *mut u8, _: *mut Request, _: u64) -> Wait {
    Wait::default()
}
unsafe extern "C" fn resume(state: *mut u8, _: *mut Request, _: u64) -> Step {
    // SAFETY: the fixture pins its two counters until every callback ends.
    unsafe {
        (*state.cast::<[usize; 2]>())[0] += 1;
    }
    Step::default()
}
unsafe extern "C" fn deliver(state: *mut u8, _: *mut Request, outcome: Outcome) -> Outcome {
    // SAFETY: as resume.
    unsafe {
        (*state.cast::<[usize; 2]>())[1] += 1;
    }
    outcome
}
unsafe extern "C" fn abort(_: *mut u8, _: *mut Request, _: u64, _: u8) -> Outcome {
    Outcome::default()
}
#[test]
fn ready_routes_one_and_work_is_independent_of_parked_count() {
    for n in [3, 10_000] {
        let mut counters = [0usize; 2];
        let provider = Provider {
            state: counters.as_mut_ptr().cast(),
            wait,
            issue: resume,
            resume,
            abort,
            deliver,
        };
        let mut request = Request {
            operation: RECV_BOUNDED,
            mode: 0,
            handle: 0,
            path: ptr::null(),
            path_len: 0,
            buffer: Buffer {
                data: ptr::null_mut(),
                len: 0,
                capacity: 0,
            },
            site: 0,
        };
        let mut run = Scheduler {
            #[cfg(feature = "prof")]
            prof_id: prof::run_id(),
            share_heap: false,
            deferred: ptr::null_mut(),
            deferred_tail: ptr::null_mut(),
            stacks: Stock::default(),
            idle: ptr::null_mut(),
            head: ptr::null_mut(),
            tail: ptr::null_mut(),
            timers: TimerHeap::default(),
            all: ptr::null_mut(),
            next: 1,
            warm: ptr::null_mut(),
            provider: SchedulerProvider::default(),
            script: None,
            #[cfg(feature = "record")]
            script_groups: Vec::new(),
            readies: Readies::new(),
            ready_tokens: Vec::new(),
            ready_arms: Vec::new(),
            cancels: Vec::new(),
            #[cfg(feature = "record")]
            outside_cancels: Vec::new(),
            #[cfg(feature = "record")]
            tapes: Tapes::default(),
            net: ptr::null_mut(),
            probes_left: LIVE_PROBE_INTERVAL,
            parked: WaitArms::default(),
            lang: ptr::null_mut(),
            lang_owned: false,
        };
        let mut context = Context::new(1 << 20);
        let layout = ValueLayout {
            bytes: 0,
            align: 1,
            logical: 0,
            clean: None,
        };
        let mut operations: Vec<_> = (0..n)
            .map(|i| Operation {
                #[cfg(feature = "prof")]
                prof_pending: 0,
                provider,
                request: &mut request,
                token: i as u64 + 1,
                state: OperationState::Issued { deadline: 0 },
                observes_cancel: false,
                entered: None,
            })
            .collect();
        let mut tasks: Vec<_> = (0..n)
            .map(|i| {
                Box::new(Task {
                    arc_runtime: false,
                    arc_hooks: true,
                    #[cfg(feature = "record")]
                    replay: false,
                    #[cfg(feature = "record")]
                    latest_pair: false,
                    polls_left: POLLS_PER_TURN,
                    context: &mut context,
                    owned_context: None,
                    run: &mut run,
                    ready: ptr::null_mut(),
                    waiters: Waiters::default(),
                    waiting_on: ptr::null_mut(),
                    wait_previous: ptr::null_mut(),
                    wait_next: ptr::null_mut(),
                    timer: Timer::default(),
                    operation: &mut operations[i],
                    all: ptr::null_mut(),
                    all_previous: ptr::null_mut(),
                    warm_next: ptr::null_mut(),
                    warm_previous: ptr::null_mut(),
                    warm: false,
                    state: State::Provider,
                    id: i as u64,
                    #[cfg(feature = "record")]
                    group: None,
                    stack: Some(Stack::new()),
                    sp: ptr::null_mut(),
                    body: no_body,
                    input: Storage::new(0, 1),
                    result: Storage::new(0, 1),
                    layout: TaskLayout {
                        input: layout,
                        result: layout,
                    },
                    failed: 0,
                    record: None,
                    frame: None,
                    stackless: false,
                    moved: Moved::Spawned,
                    transfer: None,
                    holder: std::mem::MaybeUninit::uninit(),
                    cursor: 0,
                })
            })
            .collect();
        for i in 0..n - 1 {
            tasks[i].all = &mut *tasks[i + 1];
        }
        run.all = &mut *tasks[0];
        let mut sockets = hirsel_core_net::Sockets::default();
        let mut keys = Vec::new();
        let mut tags = Vec::new();
        let mut peers = Vec::new();
        for (i, task) in tasks.iter_mut().enumerate() {
            run.parked.insert(i as u64 + 1, &mut **task);
            let (socket, peer) = UnixStream::pair().unwrap();
            socket.set_nonblocking(true).unwrap();
            let key = sockets.insert(socket.into()).unwrap();
            tags.push(sockets.arm(key, false, i as u64 + 1).unwrap());
            keys.push(key);
            peers.push(peer);
        }
        // Registration queues writable edges even though every arm waits
        // for a read. Drain these before making the selected socket ready.
        while sockets
            .poll(0, |_| panic!("readable before a peer wrote"))
            .unwrap()
            != 0
        {}
        run.net = &mut sockets;
        assert!(run.parked.peek(1).is_some());
        let selected = if n == 3 { 1 } else { n - 1 };
        peers[selected].write_all(b"x").unwrap();
        ROUTE_WORK.set(0);
        ROUTE_WAKES.set(0);
        // SAFETY: task/operation frames and callback storage are pinned; no fiber runs.
        unsafe {
            run.take_wakes(0);
        }
        let work = ROUTE_WORK.get();
        println!(
            "ready_routes_one parked={n} route_key_operations={work} wakes={} resumes={}",
            ROUTE_WAKES.get(),
            counters[0]
        );
        assert_eq!(counters[0], 1);
        assert_eq!(ROUTE_WAKES.get(), 1);
        assert_eq!(run.head, &mut *tasks[selected] as *mut Task);
        for (i, task) in tasks.iter().enumerate() {
            assert!(
                task.state
                    == if i == selected {
                        State::Ready
                    } else {
                        State::Provider
                    }
            );
        }
        if let OperationState::ReadyToDeliver(Some(outcome)) = operations[selected].state {
            // SAFETY: selected operation's task is now runnable for this one delivery.
            unsafe {
                (provider.deliver)(provider.state, &mut request, outcome);
            }
        } else {
            panic!("the ready operation did not deliver");
        }
        assert_eq!(counters, [1, 1]);
        assert_eq!(run.parked.live.len(), n - 1, "entry_kept_after_wake");
        assert!(
            work <= 8,
            "ready routing scanned parked tasks: {work} for {n}"
        );
        if n == 3 {
            // First-arrival dedup and absent tokens are resolved before any resume.
            for token in [2, 1, 1, 3, u64::MAX] {
                run.readies.push(token);
            }
            unsafe {
                run.take_wakes(0);
            }
            assert_eq!(ROUTE_WAKES.get(), 3, "dedup_dropped");
            assert_eq!(counters[0], 3);
            assert!(run.parked.live.is_empty());
            for i in [0, 2] {
                if let OperationState::ReadyToDeliver(Some(outcome)) = operations[i].state {
                    unsafe {
                        (provider.deliver)(provider.state, &mut request, outcome);
                    }
                }
            }
            assert_eq!(counters, [3, 3]);
            unsafe { while !run.pop().is_null() {} }
            let stale = tags[0];
            sockets.release(keys[0]);
            let (socket, mut peer) = UnixStream::pair().unwrap();
            socket.set_nonblocking(true).unwrap();
            let replacement = sockets.insert(socket.into()).unwrap();
            assert_eq!((keys[0] >> 2) as u32, (replacement >> 2) as u32);
            assert!(replacement >> 34 > keys[0] >> 34);
            operations[0].token = 4;
            operations[0].state = OperationState::Issued { deadline: 0 };
            tasks[0].state = State::Provider;
            run.parked.insert(4, &mut *tasks[0]);
            sockets.arm(replacement, false, 4).unwrap();
            sockets.dispatch(stale, hirsel_core_net::ready::Ready::READABLE, |token| {
                run.readies.push(token)
            });
            unsafe {
                run.take_wakes(0);
            }
            assert_eq!(ROUTE_WAKES.get(), 3, "route_ignores_generation");
            assert_eq!(run.parked.live.len(), 1);
            peer.write_all(b"new").unwrap();
            unsafe {
                run.take_wakes(0);
            }
            assert_eq!(ROUTE_WAKES.get(), 4);
            assert_eq!(counters[0], 4);
            assert!(run.parked.live.is_empty());
        } else {
            ROUTE_WORK.set(0);
            for token in 1..=n as u64 {
                run.readies.push(token);
                run.readies.push(token);
            }
            unsafe {
                run.take_wakes(0);
            }
            println!(
                "ready_dedup tokens={} route_key_operations={}",
                2 * n,
                ROUTE_WORK.get()
            );
            assert_eq!(
                ROUTE_WAKES.get(),
                n,
                "linear dedup wakes each parked operation once"
            );
            assert_eq!(counters[0], n);
            assert!(run.parked.live.is_empty());
            assert!(
                ROUTE_WORK.get() <= 16 * n,
                "wide-turn dedup must do linear key work"
            );
        }
    }
}
