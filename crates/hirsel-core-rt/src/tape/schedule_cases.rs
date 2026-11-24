//! S2 cases live in S1's bypass-law body, so the existing acceptance selector
//! runs the scheduling and no-host laws together.
use super::{tests::*, *};
use crate::{Context, causes, channel::*, schedule_script::ScheduleScript, tasks::*};
use std::{collections::BTreeSet, ptr::NonNull, sync::Arc};

struct Script {
    waited: BTreeSet<u64>,
    resumed: BTreeSet<u64>,
    calls: usize,
    pending: bool,
    fault_at: Option<u8>,
    manual: bool,
    armed: Option<u64>,
}
impl Script {
    fn provider(&mut self) -> Provider {
        unsafe extern "C" fn wait(s: *mut u8, _: *mut Request, token: u64) -> Wait {
            // SAFETY: the case pins Script until its run ends.
            let s = unsafe { &mut *s.cast::<Script>() };
            s.calls += 1;
            if s.fault_at.is_some() {
                return Wait::default();
            }
            if s.manual {
                return if s.armed.replace(token) == Some(token) {
                    Wait::default()
                } else {
                    Wait {
                        kind: WAIT_PARK,
                        deadline_ns: 0,
                    }
                };
            }
            if s.waited.insert(token) {
                Wait {
                    kind: WAIT_UNTIL,
                    deadline_ns: crate::os::monotonic_ns() + 1_000_000,
                }
            } else {
                Wait::default()
            }
        }
        unsafe extern "C" fn issue(s: *mut u8, request: *mut Request, token: u64) -> Step {
            // SAFETY: synchronous run-local script.
            let s = unsafe { &mut *s.cast::<Script>() };
            s.calls += 1;
            // SAFETY: the provider owns this request through issue.
            if s.fault_at == Some(unsafe { (*request).mode }) {
                return done(Outcome {
                    error: IoError { tag: 1, code: 0 },
                    ..Outcome::default()
                });
            }
            if s.pending {
                hirsel_core_ready(token);
                hirsel_core_ready(token);
                pending()
            } else {
                done(Outcome {
                    value: 17,
                    ..Outcome::default()
                })
            }
        }
        unsafe extern "C" fn resume(s: *mut u8, _: *mut Request, token: u64) -> Step {
            // SAFETY: synchronous run-local script.
            let s = unsafe { &mut *s.cast::<Script>() };
            s.calls += 1;
            if s.pending && s.resumed.insert(token) {
                hirsel_core_ready(token);
                return pending();
            }
            done(Outcome {
                value: 17,
                ..Outcome::default()
            })
        }
        unsafe extern "C" fn abort(s: *mut u8, _: *mut Request, _: u64, _: u8) -> Outcome {
            // SAFETY: synchronous run-local script.
            unsafe {
                (*s.cast::<Script>()).calls += 1;
            }
            Outcome {
                value: 19,
                ..Outcome::default()
            }
        }
        Provider {
            state: (self as *mut Self).cast(),
            wait,
            issue,
            resume,
            abort,
            deliver: crate::provider::deliver,
        }
    }
}
fn value<T>(logical: u64) -> ValueLayout {
    ValueLayout {
        bytes: size_of::<T>(),
        align: align_of::<T>(),
        logical,
        clean: None,
    }
}
fn layout<T>() -> TaskLayout {
    TaskLayout {
        input: value::<T>(0),
        result: value::<()>(0),
    }
}

#[derive(Clone, Copy)]
struct Job {
    provider: Provider,
    handle: u64,
    sender: *mut Channel,
    number: u64,
    observations: *mut Vec<(u64, u64, u8, IoError)>,
}
unsafe extern "C" fn sleeper(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
    // SAFETY: the task owns its input and sender; observations outlive the run.
    unsafe {
        let job = input.cast::<Job>().read();
        let mut r = request(
            SLEEP_UNTIL,
            job.number as u8,
            job.handle,
            &[],
            empty_buffer(),
        );
        crate::tasks::event(crate::tasks::CHECK_CANCEL, 0, 0);
        let outcome = hirsel_core_host(context, &job.provider, &mut r);
        (*job.observations).push((job.number, outcome.value, outcome.status, outcome.error));
        let mut n = job.number;
        if outcome.status == 0 && outcome.error == IoError::OK {
            hirsel_core_send(context, job.sender, (&mut n as *mut u64).cast(), 0);
        }
        hirsel_core_sender_drop(job.sender);
        crate::tasks::event(crate::tasks::CHECK_CANCEL, 0, 0);
        crate::tasks::event(crate::tasks::CHECK_CANCEL, 0, 0);
        outcome.status
    }
}
struct Run {
    store: Store,
    script: Script,
    mode: Mode,
    chooser: u8,
    fault_case: bool,
    result: Option<Result<(), Fault>>,
    observed: Vec<(u64, u64, u8, IoError)>,
    received: Vec<u64>,
}
unsafe extern "C" fn root(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
    // SAFETY: run owns every input, task handle and channel until joined/closed.
    unsafe {
        let run = &mut **input.cast::<*mut Run>();
        let mut h = header(REVISION);
        h.kind = KIND_RUN;
        h.chooser = if run.fault_case {
            recorded_chooser()
        } else {
            run.chooser
        };
        let dir = run.store.dir();
        let (tape, _) = Tape::open(REVISION, dir, b"t", run.mode, &h, &expect()).unwrap();
        let sub = tape
            .on_tape(crate::entry::CLOCK, (run.script.provider(), 0))
            .unwrap();
        let channel = hirsel_core_channel_new(context, 2, value::<u64>(8), 0);
        assert!(!channel.sender.is_null());
        let mut handles = Vec::new();
        for n in 1..=2 {
            let mut job = Job {
                provider: sub.0,
                handle: sub.1,
                sender: hirsel_core_channel_sender(channel.sender),
                number: n,
                observations: &mut run.observed,
            };
            let task = hirsel_core_spawn(
                context,
                0,
                2048,
                (&mut job as *mut Job).cast(),
                sleeper,
                layout::<Job>(),
                0,
            )
            .task;
            assert!(!task.is_null(), "spawn failed: {:?}", last_fault());
            handles.push(task);
        }
        hirsel_core_sender_drop(channel.sender);
        for _ in 0..2 {
            let mut n = 0u64;
            let received =
                hirsel_core_receive(context, channel.receiver, (&mut n as *mut u64).cast(), 0);
            if received.transferred {
                run.received.push(n);
            }
        }
        for task in handles {
            hirsel_core_join(context, task, ptr::null_mut(), 0);
        }
        hirsel_core_receiver_drop(context, channel.receiver);
        let end = if run.fault_case {
            Ending::returned(&0u32.to_le_bytes())
        } else {
            ending()
        };
        run.result = Some(tape.end(&end));
    }
    0
}
fn run_case(bytes: Option<&[u8]>, chooser: u8, pending: bool, fault_case: bool) -> Run {
    let mut run = Run {
        store: Store::default(),
        script: Script {
            waited: BTreeSet::new(),
            resumed: BTreeSet::new(),
            calls: 0,
            pending,
            fault_at: (fault_case && bytes.is_none()).then_some(2),
            manual: false,
            armed: None,
        },
        mode: if bytes.is_some() {
            Mode::Replay
        } else {
            Mode::Record
        },
        chooser,
        fault_case,
        result: None,
        observed: vec![],
        received: vec![],
    };
    if let Some(bytes) = bytes {
        run.store.files.insert(b"t".to_vec(), bytes.to_vec());
    }
    let mut context = Context::new(1 << 20);
    let mut files = crate::host_file::Files::default();
    context.host_files = &mut files;
    causes::task_start(&mut context).unwrap();
    let mut input: *mut Run = &mut run;
    // SAFETY: all borrowed case state is pinned for the cooperative run.
    let mut execute = || unsafe {
        run_root(
            &mut context,
            root,
            (&mut input as *mut *mut Run).cast(),
            ptr::null_mut(),
            size_of::<*mut Run>(),
            0,
            SchedulerProvider::default(),
        )
    };
    let status = if bytes.is_some() {
        execute()
    } else {
        let script = if fault_case {
            ScheduleScript::new(0).with_schedule_prefix(0, vec![2])
        } else {
            ScheduleScript::last()
        };
        Arc::new(script).install(execute)
    };
    assert_eq!(status, 0);
    run
}
fn frames(mut bytes: &[u8]) -> (Header, Vec<Frame>) {
    use std::io::Read;

    let (mut blocks, header) = Blocks::open(|buf| bytes.read(buf)).unwrap();
    let mut decoder = Decoder::default();
    let mut frames = Vec::new();
    while let Some((flags, payload)) = blocks.next(|buf| bytes.read(buf)).unwrap() {
        frames.extend(decoder.block(payload, flags & format::END != 0).unwrap());
    }
    (header, frames)
}

fn encode(header: &Header, frames: &[Frame]) -> Vec<u8> {
    let mut out = block(
        &header.tape_id,
        0,
        0,
        &header.encode(header.revision).unwrap(),
    );
    let mut encoder = Encoder::default();
    let mut payload = Vec::new();
    for frame in frames {
        let (head, rest) = encoder.frame(frame);
        payload.extend(head);
        assert!(rest.is_empty());
    }
    out.extend(block(&header.tape_id, format::END, 1, &payload));
    out
}
pub(super) fn run() {
    grant_gap_vector();
    positioned_wait();
    latest_pair();
    live_index_ignores_parks();
    arc_switch();
    aliased_pipe();
    positioned_end();
    closed_sites(false);
    closed_sites(true);
    outside_cases();
    let _ = ();
    for chooser in [format::CHOOSER_FIFO, format::CHOOSER_COMMIT] {
        for pending in [false, true] {
            let recorded = run_case(None, chooser, pending, false);
            assert_eq!(recorded.result, Some(Ok(())));
            assert_eq!(recorded.received.len(), 2);
            let bytes = recorded.store.file();
            let replayed = run_case(Some(bytes), chooser, pending, false);
            assert_eq!(
                replayed.script.calls, 0,
                "replay reached the inner provider"
            );
            assert_eq!(replayed.result, Some(Ok(())), "replay {chooser}/{pending}");
            assert_eq!(replayed.received, recorded.received);
            assert_eq!(replayed.observed, recorded.observed);
            let (header, all) = frames(bytes);
            // Each of the two W→I operations has one wake. Each Pending
            // re-arm disappears, leaving one DONE per issued task.
            let wakes: Vec<_> = all
                .iter()
                .filter_map(|frame| match frame {
                    Frame::Wake { task, .. } => Some(*task),
                    _ => None,
                })
                .collect();
            let dones: BTreeSet<_> = all
                .iter()
                .filter_map(|frame| match frame {
                    Frame::Done { task, .. } => Some(*task),
                    _ => None,
                })
                .collect();
            assert_eq!(wakes.len(), 2);
            assert_eq!(
                wakes.into_iter().collect::<BTreeSet<_>>(),
                BTreeSet::from([1, 2])
            );
            assert_eq!(
                all.iter()
                    .filter(|frame| matches!(frame, Frame::Done { .. }))
                    .count(),
                usize::from(pending) * 2
            );
            if pending {
                assert_eq!(dones, BTreeSet::from([1, 2]));
            }
            if chooser == 0 {
                assert!(!all.iter().any(|f| matches!(f, Frame::Choose { .. })));
            }
            for drop_wait in [false, true] {
                let mut changed = all.clone();
                let at = changed
                    .iter()
                    .position(|f| {
                        if drop_wait {
                            matches!(f, Frame::Wait { .. })
                        } else {
                            matches!(f, Frame::Wake { .. })
                        }
                    })
                    .unwrap();
                omit(&mut changed, at);
                replay_schedule_fault(
                    &encode(&header, &changed),
                    chooser,
                    if pending { "bypass-pending" } else { "bypass" },
                    None,
                );
            }
        }
    }
}

#[test]
fn scripted_fault_replays_from_a_complete_tape_without_the_script() {
    let recorded = run_case(None, format::CHOOSER_COMMIT, false, true);
    let bytes = recorded.store.file();
    let (header, frames) = frames(bytes);
    assert_eq!(header.chooser, format::CHOOSER_COMMIT);
    assert!(
        frames
            .iter()
            .any(|frame| matches!(frame, Frame::Choose { index: 2, .. }))
    );
    assert!(matches!(
        frames.last(),
        Some(Frame::End { ending, .. }) if *ending == Ending::returned(&0u32.to_le_bytes())
    ));

    let replayed = run_case(Some(bytes), format::CHOOSER_COMMIT, false, true);
    assert_eq!(replayed.script.calls, 0);
    for run in [recorded, replayed] {
        assert_eq!(
            run.observed,
            [
                (2, 0, 0, IoError { tag: 1, code: 0 }),
                (1, 17, 0, IoError::OK)
            ]
        );
        assert_eq!(run.received, [1]);
        assert_eq!(run.result, Some(Ok(())));
    }
}

fn grant_gap_vector() {
    let mut store = Store::default();
    let mut script = Script {
        waited: BTreeSet::new(),
        resumed: BTreeSet::new(),
        calls: 0,
        pending: false,
        fault_at: None,
        manual: false,
        armed: None,
    };
    // SAFETY: both in-memory providers are pinned for the tape's lifetime.
    let (tape, _) = unsafe {
        Tape::open(
            REVISION,
            store.dir(),
            b"t",
            Mode::Record,
            &header(REVISION),
            &expect(),
        )
    }
    .unwrap();
    let group = tape.group();
    group.event(|| 0, false);
    let (p, h) = tape
        .on_tape(crate::entry::CLOCK, (script.provider(), 0))
        .unwrap();
    group.event(|| 0, false);
    let mut deadline = 9000u64.to_le_bytes();
    let mut r = request(
        SLEEP_UNTIL,
        0,
        h,
        &[],
        Buffer {
            data: deadline.as_mut_ptr(),
            len: 8,
            capacity: 0,
        },
    );
    // SAFETY: emulate the ABI's first wait, timer wake, issue and delivery.
    unsafe {
        assert_eq!((p.wait)(p.state, &mut r, 1).kind, WAIT_UNTIL);
        assert_eq!((p.wait)(p.state, &mut r, 1).kind, WAIT_READY);
        group.wake(&mut r);
        let step = (p.issue)(p.state, &mut r, 1);
        assert_eq!((p.deliver)(p.state, &mut r, step.outcome).value, 17);
    }
    assert_eq!(tape.end(&ending()), Ok(()));
    assert_eq!(store.file(), include_bytes!("testdata/grant-gap-tape.bin"));
    let expected = store.file().to_vec();
    let (tape, _) = unsafe {
        Tape::open(
            REVISION,
            store.dir(),
            b"t",
            Mode::Replay,
            &header(REVISION),
            &expect(),
        )
    }
    .unwrap();
    let group = tape.group();
    group.event(|| 0, false);
    let (p, h) = tape
        .on_tape(crate::entry::CLOCK, (script.provider(), 0))
        .unwrap();
    group.event(|| 0, false);
    r.handle = h;
    unsafe {
        assert_eq!((p.wait)(p.state, &mut r, 2).kind, WAIT_PARK);
    }
    assert!(matches!(group.turn(), schedule::Turn::Wake(0)));
    group.consume_input();
    unsafe {
        let step = (p.issue)(p.state, &mut r, 2);
        assert_eq!((p.deliver)(p.state, &mut r, step.outcome).value, 17);
    }
    assert_eq!(tape.end(&ending()), Ok(()));
    assert_eq!(store.file(), expected);
}

struct Outside {
    store: Store,
    replay: bool,
    cancel: bool,
    phase: u8,
    result: Option<Result<(), Fault>>,
    capacity: u64,
    outcome: u8,
}
unsafe extern "C" fn outside_worker(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
    // SAFETY: the supervisor pins Outside until it joins this child.
    unsafe {
        let case = &mut **input.cast::<*mut Outside>();
        let mode = if case.replay {
            Mode::Replay
        } else {
            Mode::Record
        };
        let (tape, _) = Tape::open(
            REVISION,
            case.store.dir(),
            b"t",
            mode,
            &header(REVISION),
            &expect(),
        )
        .unwrap();
        let sub = tape
            .on_tape(
                crate::entry::CLOCK,
                (
                    Provider {
                        state: ptr::null_mut(),
                        wait: forever,
                        issue: crate::provider::immediate::resume,
                        resume: crate::provider::immediate::resume,
                        abort: crate::provider::immediate::abort,
                        deliver: crate::provider::deliver,
                    },
                    0,
                ),
            )
            .unwrap();
        case.phase = 1;
        if case.cancel {
            let mut r = request(SLEEP_UNTIL, 0, sub.1, &[], empty_buffer());
            case.outcome = hirsel_core_host(context, &sub.0, &mut r).status;
        } else {
            checkpoint();
            case.capacity = (*context).current.available();
            let allocation = crate::block::Block::new(&*context, Some(3000), 1, 8);
            assert!(
                allocation.is_err(),
                "outside refund cannot enlarge the fresh reservation"
            );
        }
        case.result = Some(tape.end(&ending()));
        case.phase = 2;
        0
    }
}
unsafe extern "C" fn forever(_: *mut u8, _: *mut Request, _: u64) -> Wait {
    Wait {
        kind: WAIT_PARK,
        deadline_ns: 0,
    }
}
unsafe extern "C" fn outside_root(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
    // SAFETY: the root owns the child and parent-funded block until both end.
    unsafe {
        let case = *input.cast::<*mut Outside>();
        let outside_bytes = crate::block::Block::new(&*context, Some(4096), 1, 8).unwrap();
        let mut argument = case;
        let child = hirsel_core_spawn(
            context,
            0,
            2048,
            (&mut argument as *mut *mut Outside).cast(),
            outside_worker,
            layout::<*mut Outside>(),
            0,
        )
        .task;
        assert!(!child.is_null());
        loop {
            if (*case).phase != 0 {
                break;
            }
            checkpoint();
        }
        if (*case).cancel {
            hirsel_core_cancel(context, child);
        }
        // Refund into the supervisor's limit, while the worker's tape is open.
        drop(outside_bytes);
        hirsel_core_join(context, child, ptr::null_mut(), 0);
    }
    0
}
fn outside_cases() {
    for cancel in [false, true] {
        let mut case = Outside {
            store: Store::default(),
            replay: false,
            cancel,
            phase: 0,
            result: None,
            capacity: 0,
            outcome: 0,
        };
        let mut first = None;
        for replay in [false, true] {
            case.replay = replay;
            case.phase = 0;
            case.result = None;
            let mut context = Context::new(1 << 20);
            let mut files = crate::host_file::Files::default();
            context.host_files = &mut files;
            causes::task_start(&mut context).unwrap();
            let mut input: *mut Outside = &mut case;
            // SAFETY: case and registry stay pinned until all children have joined.
            unsafe {
                assert_eq!(
                    run_root(
                        &mut context,
                        outside_root,
                        (&mut input as *mut *mut Outside).cast(),
                        ptr::null_mut(),
                        size_of::<*mut Outside>(),
                        0,
                        SchedulerProvider::default()
                    ),
                    0
                );
            }
            assert_eq!(
                case.result,
                Some(Ok(())),
                "outside_cancel={cancel}, replay={replay}"
            );
            let observed = (case.capacity, case.outcome);
            if let Some(first) = first {
                assert_eq!(observed, first);
            } else {
                first = Some(observed);
            }
            if cancel {
                assert_eq!(case.outcome, 1);
            }
        }
        let (_, frames) = frames(case.store.file());
        assert_eq!(
            frames
                .iter()
                .filter(|f| matches!(f, Frame::Cancel { .. }))
                .count(),
            usize::from(cancel)
        );
        assert!(
            !frames.iter().any(|f| matches!(f, Frame::Wake { .. })),
            "cancellation closes WAIT without WAKE"
        );
    }
}

fn gap(frame: &mut Frame) -> Option<&mut u64> {
    match frame {
        Frame::Wait { gap, .. }
        | Frame::Wake { gap, .. }
        | Frame::Choose { gap, .. }
        | Frame::Cancel { gap, .. }
        | Frame::Done { gap, .. }
        | Frame::Grant { gap, .. }
        | Frame::End { gap, .. } => Some(gap),
        _ => None,
    }
}

/// Drop one frame while preserving the absolute positions of the others.
fn omit(frames: &mut Vec<Frame>, at: usize) {
    let mut removed = frames.remove(at);
    if let Some(removed) = gap(&mut removed)
        && let Some(next) = frames.get_mut(at).and_then(gap)
    {
        *next += *removed;
    }
}

#[derive(Clone, Copy)]
enum Scenario {
    Closed { site: u64, grant: bool },
    Wait { between: bool },
    Latest,
    Index,
    Arc,
    Pipe { reverse: bool },
    Seal { after_send: bool },
}

struct Timeline {
    store: Store,
    script: Script,
    scenario: Scenario,
    mode: Mode,
    chooser: u8,
    result: Option<Result<(), Fault>>,
    events: Vec<Event>,
    arc_last: Option<bool>,
    arc_unique: Option<bool>,
    arc_refund: u64,
    writers: [libc::c_int; 2],
    pipe: Vec<u8>,
    before_join: Option<u8>,
}

#[derive(Clone, Copy)]
struct Actor {
    timeline: *mut Timeline,
    channel: *mut Channel,
    provider: (Provider, u64),
    node: *mut u8,
    number: u64,
}

unsafe extern "C" fn actor(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
    // SAFETY: the fixture pins its timeline and transfers one receiver to this task.
    unsafe {
        let actor = input.cast::<Actor>().read();
        let record = (*actor.timeline).mode == Mode::Record;
        match (*actor.timeline).scenario {
            Scenario::Closed { site, grant } => {
                if grant {
                    hirsel_core_check_cancel(context, 3);
                    if record && site == 11 {
                        checkpoint();
                    }
                }
                let first =
                    hirsel_core_receive(context, actor.channel, NonNull::dangling().as_ptr(), 11);
                let status = if first.status == 0 {
                    if record {
                        checkpoint();
                    }
                    hirsel_core_receive(context, actor.channel, NonNull::dangling().as_ptr(), 22)
                        .status
                } else {
                    first.status
                };
                hirsel_core_receiver_drop(context, actor.channel);
                status
            }
            Scenario::Wait { .. } | Scenario::Latest => {
                let (provider, handle) = actor.provider;
                let mut r = request(SLEEP_UNTIL, 0, handle, &[], empty_buffer());
                hirsel_core_host(context, &provider, &mut r).status
            }
            Scenario::Index => {
                if actor.number == 1 {
                    let status = hirsel_core_receive(
                        context,
                        actor.channel,
                        NonNull::dangling().as_ptr(),
                        11,
                    )
                    .status;
                    hirsel_core_receiver_drop(context, actor.channel);
                    status
                } else {
                    hirsel_core_check_cancel(context, 22)
                }
            }
            Scenario::Seal { after_send } => {
                hirsel_core_check_cancel(context, 1);
                if record && !after_send {
                    checkpoint();
                }
                hirsel_core_try_send(actor.channel, NonNull::dangling().as_ptr());
                if record && after_send {
                    checkpoint();
                }
                hirsel_core_sender_drop(actor.channel);
                0
            }
            Scenario::Pipe { .. } => {
                let (provider, handle) = actor.provider;
                let mut byte = actor.number as u8;
                let mut request = request(
                    if byte == b'A' { STDOUT } else { STDERR },
                    0,
                    handle,
                    &[],
                    Buffer {
                        data: &raw mut byte,
                        len: 1,
                        capacity: 0,
                    },
                );
                hirsel_core_host(context, &provider, &mut request).status
            }
            Scenario::Arc => {
                let node = NonNull::new_unchecked(actor.node);
                (*actor.timeline).arc_last = Some(crate::counted::decrement(node, true));
                0
            }
        }
    }
}

unsafe extern "C" fn timeline_root(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
    // SAFETY: the fixture pins both provider states and owns each spawned handle.
    unsafe {
        let timeline = *input.cast::<*mut Timeline>();
        let record = (*timeline).mode == Mode::Record;
        let mut header = header(REVISION);
        header.chooser = (*timeline).chooser;
        let (tape, _) = Tape::open(
            REVISION,
            (*timeline).store.dir(),
            b"t",
            (*timeline).mode,
            &header,
            &expect(),
        )
        .unwrap();
        let mut node = std::sync::atomic::AtomicU64::new(1);
        let node = (&raw mut node).cast();
        let mut latest = ptr::null_mut();
        let mut arc_block = None;
        let mut second = ptr::null_mut();
        let mut receiver = ptr::null_mut();
        let mut sender = ptr::null_mut();
        let mut input = match (*timeline).scenario {
            Scenario::Closed { .. } | Scenario::Index => {
                let queue = hirsel_core_channel_new(context, 1, value::<()>(0), 0);
                if matches!((*timeline).scenario, Scenario::Index) {
                    sender = queue.sender;
                } else {
                    hirsel_core_sender_drop(queue.sender);
                }
                Actor {
                    timeline,
                    channel: queue.receiver,
                    provider: ((*timeline).script.provider(), 0),
                    node,
                    number: 1,
                }
            }
            Scenario::Wait { .. } | Scenario::Latest => {
                if matches!((*timeline).scenario, Scenario::Latest) {
                    latest = crate::latest::hirsel_core_latest_new(context, node, clean_arc).latest;
                }
                let source = (*timeline).script.provider();
                let provider = tape.on_tape(crate::entry::CLOCK, (source, 0)).unwrap();
                Actor {
                    timeline,
                    channel: ptr::null_mut(),
                    provider,
                    node,
                    number: 1,
                }
            }
            Scenario::Arc => {
                let block = crate::block::Block::new(&*context, Some(8), 8, 8).unwrap();
                let node = block.into_raw();
                crate::counted::initialize(node, true);
                crate::counted::share(node, true);
                arc_block = Some(node);
                Actor {
                    timeline,
                    channel: ptr::null_mut(),
                    provider: ((*timeline).script.provider(), 0),
                    node: node.as_ptr(),
                    number: 1,
                }
            }
            Scenario::Seal { .. } => {
                let queue = hirsel_core_channel_new(context, 1, value::<()>(0), 0);
                receiver = queue.receiver;
                Actor {
                    timeline,
                    channel: queue.sender,
                    provider: ((*timeline).script.provider(), 0),
                    node,
                    number: 1,
                }
            }
            Scenario::Pipe { .. } => {
                let mut actor = Actor {
                    timeline,
                    channel: ptr::null_mut(),
                    node,
                    number: u64::from(b'A'),
                    provider: tape
                        .on_tape(
                            crate::entry::STDOUT,
                            (pipe_provider(&raw mut (*timeline).writers[0]), 0),
                        )
                        .unwrap(),
                };
                second = hirsel_core_spawn(
                    context,
                    0,
                    2048,
                    (&raw mut actor).cast(),
                    self::actor,
                    layout::<Actor>(),
                    0,
                )
                .task;
                actor.number = u64::from(b'B');
                actor.provider = tape
                    .on_tape(
                        crate::entry::STDERR,
                        (pipe_provider(&raw mut (*timeline).writers[1]), 0),
                    )
                    .unwrap();
                actor
            }
        };
        let task = hirsel_core_spawn(
            context,
            0,
            2048,
            (&raw mut input).cast(),
            actor,
            layout::<Actor>(),
            0,
        )
        .task;
        let mut held = ptr::null_mut();
        match (*timeline).scenario {
            Scenario::Seal { .. } => {
                if record {
                    checkpoint();
                }
                (*timeline).result = Some(tape.end(&ending()));
                (*timeline).before_join = Some(hirsel_core_try_receive(
                    receiver,
                    NonNull::dangling().as_ptr(),
                ));
                hirsel_core_join(context, task, NonNull::dangling().as_ptr(), 0);
                hirsel_core_receiver_drop(context, receiver);
                return 0;
            }
            Scenario::Closed { site, grant } => {
                if record && (grant || site == 22) {
                    checkpoint();
                }
                if grant {
                    let source = (*timeline).script.provider();
                    let _ = tape.on_tape(crate::entry::CLOCK, (source, 0));
                }
                hirsel_core_cancel(context, task);
            }
            Scenario::Wait { between } => {
                hirsel_core_check_cancel(context, 1);
                if record && between {
                    checkpoint();
                }
                hirsel_core_check_cancel(context, 2);
                if record && !between {
                    checkpoint();
                }
                hirsel_core_check_cancel(context, 3);
                if record {
                    hirsel_core_ready((*timeline).script.armed.unwrap());
                }
            }
            Scenario::Latest => {
                if record {
                    checkpoint();
                }
                held = crate::latest::hirsel_core_latest_read(latest);
                if record {
                    hirsel_core_ready((*timeline).script.armed.unwrap());
                }
            }
            Scenario::Index => {
                input.number = 2;
                let other = hirsel_core_spawn(
                    context,
                    0,
                    2048,
                    (&raw mut input).cast(),
                    actor,
                    layout::<Actor>(),
                    0,
                )
                .task;
                if record {
                    checkpoint();
                }
                hirsel_core_join(context, other, ptr::null_mut(), 0);
                hirsel_core_send(context, sender, NonNull::dangling().as_ptr(), 0);
                hirsel_core_sender_drop(sender);
            }
            Scenario::Arc => {
                if record {
                    checkpoint();
                }
                (*timeline).arc_unique = Some(crate::counted::try_unwrap(arc_block.unwrap(), true));
            }
            Scenario::Pipe { .. } => {
                hirsel_core_join(context, second, ptr::null_mut(), 0);
            }
        }
        let joined = hirsel_core_join(context, task, ptr::null_mut(), 0);
        if !joined.causes.is_null() {
            drop(crate::causes::Causes::from_raw(joined.causes));
        }
        if let Some(node) = arc_block {
            let before = (*context).current.available();
            drop(crate::block::Block::from_raw(node, 8));
            (*timeline).arc_refund = (*context).current.available() - before;
        }
        if !latest.is_null() {
            crate::counted::decrement(NonNull::new_unchecked(held), true);
            crate::latest::hirsel_core_latest_drop(context, latest);
        }
        (*timeline).result = Some(tape.end(&ending()));
        0
    }
}

fn timeline(scenario: Scenario, chooser: u8, bytes: Option<&[u8]>) -> Timeline {
    let mut t = Timeline {
        store: Store::default(),
        script: Script {
            waited: BTreeSet::new(),
            resumed: BTreeSet::new(),
            calls: 0,
            pending: false,
            fault_at: None,
            manual: true,
            armed: None,
        },
        scenario,
        mode: if bytes.is_some() {
            Mode::Replay
        } else {
            Mode::Record
        },
        chooser,
        result: None,
        events: vec![],
        arc_last: None,
        arc_unique: None,
        arc_refund: 0,
        writers: [-1; 2],
        pipe: vec![],
        before_join: None,
    };
    if let Some(bytes) = bytes {
        t.store.files.insert(b"t".to_vec(), bytes.to_vec());
    }
    use std::os::fd::{AsRawFd, FromRawFd};
    let pipe = if matches!(scenario, Scenario::Pipe { .. }) {
        let mut fds = [-1i32; 2];
        // SAFETY: pipe initializes two descriptors, each transferred once.
        unsafe {
            assert_eq!(
                hirsel_core_net::sys::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC),
                0
            );
            let reader = std::fs::File::from_raw_fd(fds[0]);
            let writer = std::fs::File::from_raw_fd(fds[1]);
            let other = writer.try_clone().unwrap();
            t.writers = [writer.as_raw_fd(), other.as_raw_fd()];
            Some((reader, writer, other))
        }
    } else {
        None
    };
    unsafe extern "C" fn trace(state: *mut u8, event: Event) {
        // SAFETY: run_root pins the fixture state and calls one committer at a time.
        unsafe { (*state.cast::<Timeline>()).events.push(event) };
    }
    let mut context = Context::new(1 << 20);
    let mut files = crate::host_file::Files::default();
    context.host_files = &mut files;
    causes::task_start(&mut context).unwrap();
    let mut input = &raw mut t;
    // SAFETY: the fixture pins each borrowed object through run_root.
    let mut execute = || unsafe {
        run_root(
            &mut context,
            timeline_root,
            (&raw mut input).cast(),
            ptr::null_mut(),
            size_of::<*mut Timeline>(),
            0,
            SchedulerProvider {
                state: (&raw mut t).cast(),
                event: Some(trace),
                ..SchedulerProvider::default()
            },
        )
    };
    let status = if bytes.is_none() && matches!(scenario, Scenario::Pipe { reverse: true }) {
        Arc::new(ScheduleScript::new(0).with_schedule_prefix(0, vec![2])).install(execute)
    } else {
        execute()
    };
    assert_eq!(status, 0);
    if let Some((mut reader, writer, other)) = pipe {
        use std::io::Read;
        drop(writer);
        drop(other);
        reader.read_to_end(&mut t.pipe).unwrap();
    }
    t
}

fn closed_sites(grant: bool) {
    let chooser = format::CHOOSER_COMMIT;
    let mut tapes = Vec::new();
    for site in [11, 22] {
        let scenario = Scenario::Closed { site, grant };
        let record = timeline(scenario, chooser, None);
        let replay = timeline(scenario, chooser, Some(record.store.file()));
        for run in [record, replay] {
            assert_eq!(run.result, Some(Ok(())), "grant={grant} site={site}");
            let sites: Vec<_> = run
                .events
                .iter()
                .filter(|e| e.kind == crate::tasks::CANCELLED)
                .map(|e| e.value)
                .collect();
            assert_eq!(sites, [site], "grant={grant}");
            tapes.push(run.store.file().to_vec());
        }
    }
    assert_ne!(
        tapes[0], tapes[2],
        "the two cancellation sites need distinct tapes"
    );
}

fn positioned_wait() {
    for chooser in [format::CHOOSER_FIFO, format::CHOOSER_COMMIT] {
        let mut tapes = Vec::new();
        for between in [true, false] {
            let scenario = Scenario::Wait { between };
            let record = timeline(scenario, chooser, None);
            let bytes = record.store.file();
            let (header, all) = frames(bytes);
            let wait = all
                .iter()
                .position(|f| matches!(f, Frame::Wait { .. }))
                .unwrap();
            let wake = all
                .iter()
                .position(|f| matches!(f, Frame::Wake { .. }))
                .unwrap();
            assert_eq!(
                all[wait],
                Frame::Wait {
                    gap: if between { 2 } else { 3 },
                    task: 1
                }
            );
            assert_eq!(
                all[wake],
                Frame::Wake {
                    gap: if between { 2 } else { 1 },
                    task: 1
                }
            );
            let replay = timeline(scenario, chooser, Some(bytes));
            assert_eq!(
                replay.result,
                Some(Ok(())),
                "chooser {chooser}, between {between}"
            );
            assert_eq!(replay.script.calls, 0);
            for dropped in [wait, wake] {
                let mut changed = all.clone();
                omit(&mut changed, dropped);
                replay_schedule_fault(
                    &encode(&header, &changed),
                    chooser,
                    if between {
                        "wait-between"
                    } else {
                        "wait-after"
                    },
                    Some(wake as u64 + u64::from(dropped == wake)),
                );
            }
            tapes.push(bytes.to_vec());
        }
        assert_ne!(tapes[0], tapes[1]);
    }
}

unsafe extern "C" fn clean_arc(
    _: *mut Context,
    place: *mut u8,
    _: *mut u64,
    _: *mut crate::counted::Worklist,
) -> bool {
    // SAFETY: Latest supplies its owned node in a live slot. The fixture owns
    // the backing stack storage independently of its count.
    unsafe {
        crate::counted::decrement(NonNull::new_unchecked(place.cast::<*mut u8>().read()), true)
    };
    true
}

fn latest_pair() {
    for chooser in [format::CHOOSER_FIFO, format::CHOOSER_COMMIT] {
        let record = timeline(Scenario::Latest, chooser, None);
        let bytes = record.store.file();
        let replay = timeline(Scenario::Latest, chooser, Some(bytes));
        assert_eq!(replay.result, Some(Ok(())));
        let (header, all) = frames(bytes);
        let wake = all
            .iter()
            .position(|f| matches!(f, Frame::Wake { .. }))
            .unwrap();
        assert_eq!(all[wake], Frame::Wake { gap: 2, task: 1 });
        for delta in [-1, 1] {
            let mut changed = all.clone();
            *gap(&mut changed[wake]).unwrap() = (2i64 + delta) as u64;
            replay_schedule_fault(
                &encode(&header, &changed),
                chooser,
                "latest",
                Some(wake as u64 + 1),
            );
        }
    }
}

fn live_index_ignores_parks() {
    let chooser = format::CHOOSER_COMMIT;
    let record = timeline(Scenario::Index, chooser, None);
    let bytes = record.store.file();
    assert_eq!(
        frames(bytes).1.first(),
        Some(&Frame::Choose { gap: 3, index: 2 })
    );
    let replay = timeline(Scenario::Index, chooser, Some(bytes));
    assert_eq!(replay.result, Some(Ok(())));
    for run in [record, replay] {
        assert_eq!(
            run.events
                .iter()
                .find(|event| event.kind == CHECK_CANCEL)
                .unwrap()
                .task,
            3
        );
    }
}

fn arc_switch() {
    let chooser = format::CHOOSER_COMMIT;
    let record = timeline(Scenario::Arc, chooser, None);
    let replay = timeline(Scenario::Arc, chooser, Some(record.store.file()));
    for run in [record, replay] {
        assert_eq!(run.result, Some(Ok(())));
        assert_eq!(run.arc_last, Some(false));
        assert_eq!(run.arc_unique, Some(true));
        assert_eq!(run.arc_refund, 8);
        let event_values: Vec<_> = run
            .events
            .iter()
            .filter(|event| matches!(event.kind, ARC_SHARE | ARC_RELEASE | ARC_UNWRAP))
            .map(|event| (event.kind, event.value))
            .collect();
        assert_eq!(
            event_values,
            [(ARC_SHARE, 2), (ARC_RELEASE, 0), (ARC_UNWRAP, 1)]
        );
    }
}

fn pipe_provider(fd: *mut libc::c_int) -> Provider {
    unsafe extern "C" fn write_pipe(state: *mut u8, request: *mut Request, _: u64) -> Step {
        // SAFETY: this fixture pins a file descriptor and a one-byte Request.
        let (fd, buffer) = unsafe { (*state.cast::<libc::c_int>(), (*request).buffer) };
        let written = unsafe { libc::write(fd, buffer.data.cast(), buffer.len) };
        done(Outcome {
            written: written as u64,
            ..Outcome::default()
        })
    }
    Provider {
        state: fd.cast(),
        issue: write_pipe,
        wait: crate::provider::immediate::wait,
        resume: crate::provider::immediate::resume,
        abort: crate::provider::immediate::abort,
        deliver: crate::provider::deliver,
    }
}

fn aliased_pipe() {
    let chooser = format::CHOOSER_COMMIT;
    let mut tapes = Vec::new();
    for (reverse, expected) in [(false, *b"AB"), (true, *b"BA")] {
        let scenario = Scenario::Pipe { reverse };
        let record = timeline(scenario, chooser, None);
        assert_eq!(record.result, Some(Ok(())));
        assert_eq!(record.pipe, expected);
        let bytes = record.store.file();
        let order: Vec<_> = frames(bytes)
            .1
            .iter()
            .filter_map(|f| match f {
                Frame::Op { op: STDOUT, .. } => Some(b'A'),
                Frame::Op { op: STDERR, .. } => Some(b'B'),
                _ => None,
            })
            .collect();
        assert_eq!(order, expected);
        let replay = timeline(scenario, chooser, Some(bytes));
        assert_eq!(replay.result, Some(Ok(())));
        assert!(replay.pipe.is_empty());
        tapes.push(bytes.to_vec());
    }
    assert_ne!(tapes[0], tapes[1]);
}

fn positioned_end() {
    let chooser = format::CHOOSER_COMMIT;
    let mut tapes = Vec::new();
    for after_send in [false, true] {
        let expected = if after_send { 0 } else { 1 };
        let scenario = Scenario::Seal { after_send };
        let record = timeline(scenario, chooser, None);
        let bytes = record.store.file();
        let expected_gap = if after_send { 3 } else { 1 };
        assert_eq!(
            frames(bytes).1.last(),
            Some(&Frame::End {
                gap: expected_gap,
                ending: ending()
            }),
        );
        let replay = timeline(scenario, chooser, Some(bytes));
        for run in [&record, &replay] {
            assert_eq!(run.result, Some(Ok(())));
            assert_eq!(run.before_join, Some(expected));
        }
        tapes.push(bytes.to_vec());
    }
    assert_ne!(tapes[0], tapes[1]);
}

struct Quiescence {
    dir: (Provider, u64),
    mode: Mode,
    end_last: bool,
    reorder: bool,
}

fn quiescence_header(id: u8) -> Header {
    let mut h = header(REVISION);
    h.kind = KIND_RUN;
    h.chooser = format::CHOOSER_COMMIT;
    h.tape_id = [id; 16];
    h
}

unsafe extern "C" fn wait_channel(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
    // SAFETY: the child owns this receiver and will stay parked on it.
    unsafe {
        let receiver = input.cast::<*mut Channel>().read();
        hirsel_core_receive(context, receiver, NonNull::dangling().as_ptr(), 9).status
    }
}

unsafe extern "C" fn end_child(_: *mut Context, _: *mut u8, _: *mut u8) -> u8 {
    0
}

unsafe extern "C" fn park_at_quiescence(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
    struct Cleanup;
    impl Drop for Cleanup {
        fn drop(&mut self) {
            eprintln!("user cleanup ran");
        }
    }
    let _cleanup = Cleanup;
    // SAFETY: the parent owns both ends of the closed world. Only the child
    // marked end_child can run after both receives would park.
    unsafe {
        let case = &**input.cast::<*const Quiescence>();
        let q = hirsel_core_channel_new(context, 1, value::<()>(0), 0);
        let spare = case.reorder.then(|| {
            Tape::open(
                REVISION,
                case.dir,
                b"spare",
                Mode::Record,
                &quiescence_header(3),
                &expect(),
            )
            .unwrap()
            .0
        });
        let (x, _) = Tape::open(
            REVISION,
            case.dir,
            b"x",
            case.mode,
            &quiescence_header(1),
            &expect(),
        )
        .unwrap();
        let _y = if case.end_last {
            let r = hirsel_core_channel_new(context, 1, value::<()>(0), 0);
            let mut receiver = r.receiver;
            let waiter = hirsel_core_spawn(
                context,
                0,
                0,
                (&raw mut receiver).cast(),
                wait_channel,
                layout::<*mut Channel>(),
                0,
            );
            let child = hirsel_core_spawn(
                context,
                0,
                8,
                NonNull::dangling().as_ptr(),
                end_child,
                layout::<()>(),
                0,
            );
            assert!(!waiter.task.is_null() && !child.task.is_null());
            None
        } else {
            for _ in 0..7 {
                assert_eq!(hirsel_core_check_cancel(context, 0), 0);
            }
            Some(
                Tape::open(
                    REVISION,
                    case.dir,
                    b"y",
                    case.mode,
                    &quiescence_header(2),
                    &expect(),
                )
                .unwrap()
                .0,
            )
        };
        // Removal swaps the last open cell to the front. X remains the older
        // of the two live generations in both inventory orders.
        drop(spare);
        hirsel_core_receive(context, q.receiver, NonNull::dangling().as_ptr(), 11);
        drop(x);
        panic!("the receive has no sender that could run");
    }
}

#[test]
#[ignore = "spawned by the quiescence laws"]
fn quiescence_child() {
    let directory = std::env::var_os("TAPE_QUIESCENCE_DIR").unwrap();
    let case = std::env::var("TAPE_QUIESCENCE_MODE").unwrap();
    let directory = std::path::Path::new(&directory);
    let mut store = Store::default();
    let mode = if case.contains("record") {
        store.close_to = Some(directory.to_owned());
        Mode::Record
    } else {
        for name in ["x", "y"] {
            if let Ok(bytes) = std::fs::read(directory.join(name)) {
                store.files.insert(name.as_bytes().to_vec(), bytes);
            }
        }
        Mode::Replay
    };
    let args = Quiescence {
        dir: store.dir(),
        mode,
        end_last: case.contains("last"),
        reorder: case.contains("swap"),
    };
    let mut input = &raw const args;
    let mut context = Context::new(1 << 20);
    causes::task_start(&mut context).unwrap();
    // SAFETY: the complete child execution has unit output and pins the input.
    unsafe {
        run_root(
            &mut context,
            park_at_quiescence,
            (&raw mut input).cast(),
            ptr::null_mut(),
            size_of::<*const Quiescence>(),
            0,
            SchedulerProvider::default(),
        );
    }
}

fn run_quiescence(dir: &std::path::Path, mode: &str, expected: &str) {
    let output = crate::test_process::command()
        .args([
            "--ignored",
            "--exact",
            "tape::schedule_cases::quiescence_child",
            "--test-threads=1",
        ])
        .env("TAPE_QUIESCENCE_DIR", dir)
        .env("TAPE_QUIESCENCE_MODE", mode)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(102), "{mode}: {output:?}");
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        format!("run stop: {expected}\n"),
        "{mode}"
    );
}

#[test]
fn deadlock_seals_each_live_tape_after_the_last_runnable_task() {
    let last = tempfile::tempdir().unwrap();
    run_quiescence(last.path(), "last-record", "Deadlock");
    let bytes = std::fs::read(last.path().join("x")).unwrap();
    let recorded = frames(&bytes).1;
    // The opener makes a channel and spawns twice. C hands its allowance
    // back and ends. Neither of the two receives commits.
    assert_eq!(
        recorded.last(),
        Some(&Frame::End {
            gap: 2,
            ending: Ending::deadlock(5),
        })
    );
    run_quiescence(last.path(), "last-replay", "Deadlock");

    let two = tempfile::tempdir().unwrap();
    run_quiescence(two.path(), "two-record", "Deadlock");
    let x = std::fs::read(two.path().join("x")).unwrap();
    let y = std::fs::read(two.path().join("y")).unwrap();
    // Moving the opener to Y leaves X with no members, but seven events.
    for (bytes, events) in [(&x, 7), (&y, 0)] {
        assert_eq!(
            frames(bytes).1,
            [Frame::End {
                gap: events,
                ending: Ending::deadlock(events)
            }]
        );
    }
    run_quiescence(two.path(), "two-replay", "Deadlock");
}

#[test]
#[ignore = "spawned by the malformed schedule laws"]
fn schedule_fault_child() {
    let input = std::env::var_os("TAPE_FAULT_BYTES").unwrap();
    let bytes = std::fs::read(input).unwrap();
    let chooser = std::env::var("TAPE_FAULT_CHOOSER")
        .unwrap()
        .parse()
        .unwrap();
    let case = std::env::var("TAPE_FAULT_CASE").unwrap();
    let result = match case.as_str() {
        "bypass" | "bypass-pending" => {
            run_case(Some(&bytes), chooser, case == "bypass-pending", false).result
        }
        "wait-between" | "wait-after" => {
            timeline(
                Scenario::Wait {
                    between: case == "wait-between",
                },
                chooser,
                Some(&bytes),
            )
            .result
        }
        "latest" => timeline(Scenario::Latest, chooser, Some(&bytes)).result,
        _ => unreachable!("unknown case"),
    };
    let Some(Err(Fault::Diverged(Divergence {
        kind: Mismatch::Schedule,
        at,
    }))) = result
    else {
        panic!("expected the schedule divergence, got {result:?}");
    };
    eprintln!("Diverged(Schedule) at {at}");
}

fn replay_schedule_fault(bytes: &[u8], chooser: u8, case: &str, at: Option<u64>) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tape");
    std::fs::write(&path, bytes).unwrap();
    let output = crate::test_process::command()
        .args([
            "--ignored",
            "--exact",
            "tape::schedule_cases::schedule_fault_child",
            "--test-threads=1",
            "--nocapture",
        ])
        .env("TAPE_FAULT_BYTES", path)
        .env("TAPE_FAULT_CHOOSER", chooser.to_string())
        .env("TAPE_FAULT_CASE", case)
        .output()
        .unwrap();
    assert!(
        matches!(output.status.code(), Some(0 | 1 | 102)),
        "{case}: {output:?}"
    );
    let diagnostic = String::from_utf8(output.stderr).unwrap();
    let (source, position) = diagnostic
        .trim_end()
        .rsplit_once("Diverged(Schedule) at ")
        .unwrap_or_else(|| {
            panic!(
                "{case}: code={:?} stderr={diagnostic:?}",
                output.status.code()
            )
        });
    assert!(source.is_empty() || source.starts_with("replay: "));
    if let Some(at) = at {
        assert_eq!(
            position
                .split_whitespace()
                .next()
                .unwrap()
                .parse::<u64>()
                .unwrap(),
            at,
            "{case}"
        );
    }
}

struct PoolReplay {
    store: Store,
    barrier: Option<std::sync::Barrier>,
    observations: [(u64, u64); 16],
}

#[derive(Clone, Copy)]
struct PoolJob {
    barrier: *const std::sync::Barrier,
    index: u64,
}

unsafe extern "C" fn pool_replay_child(context: *mut Context, out: *mut u8, input: *mut u8) -> u8 {
    // SAFETY: each child owns its input, bound and result. In record mode the
    // supervisor retains the shared barrier until it has joined every child.
    unsafe {
        let job = input.cast::<PoolJob>().read();
        if let Some(barrier) = job.barrier.as_ref() {
            barrier.wait();
        }
        assert_eq!((*context).available(), 64);
        assert!(
            !crate::abi::hirsel_core_limit_enter(context, 32)
                .pointer
                .is_null()
        );
        let block = crate::block::Block::new(&*context, Some(24), 24, 8).unwrap();
        assert_eq!((*context).available(), 8);
        let failed = job.index % 2 == 1;
        if failed {
            let refusal = crate::abi::hirsel_core_limit_enter(context, 9);
            assert!(refusal.pointer.is_null());
            assert_eq!(
                (refusal.capacity.requested, refusal.capacity.available),
                (9, 8)
            );
        }
        drop(block);
        crate::abi::hirsel_core_limit_exit(context);
        assert_eq!((*context).available(), 64);
        if failed {
            causes::record(
                &mut *context,
                causes::Cause::new(causes::CAPACITY, 101 + job.index),
            );
            return 1;
        }
        out.cast::<u64>().write(2 * job.index + 3);
    }
    0
}

unsafe extern "C" fn pool_replay_root(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
    // SAFETY: root owns the case's tape store and writes its observations
    // only after moving results and cause lists out of joined children.
    unsafe {
        let case = input.cast::<*mut PoolReplay>().read();
        let barrier = (*case).barrier.as_ref().map_or(std::ptr::null(), |b| b);
        let mode = if barrier.is_null() {
            Mode::Replay
        } else {
            Mode::Record
        };
        let mut header = header(REVISION);
        header.kind = KIND_RUN;
        header.chooser = format::CHOOSER_COMMIT;
        let (tape, _) = Tape::open(
            REVISION,
            (*case).store.dir(),
            b"t",
            mode,
            &header,
            &expect(),
        )
        .unwrap();
        let before = (*context).available();
        let mut handles = [std::ptr::null_mut(); 16];
        for (index, task) in handles.iter_mut().enumerate() {
            let mut job = PoolJob {
                barrier,
                index: index as u64,
            };
            *task = hirsel_core_spawn(
                context,
                0,
                64,
                (&raw mut job).cast(),
                pool_replay_child,
                crate::tasks_tests::layout::<PoolJob, u64>(8),
                11,
            )
            .task;
            assert!(!task.is_null());
        }
        for (index, task) in handles.into_iter().enumerate() {
            let mut result = 0u64;
            let joined = hirsel_core_join(context, task, (&raw mut result).cast(), 12);
            assert_eq!(joined.status, 0);
            let chain = causes::Causes::from_raw(joined.causes);
            let refusal = if let Some(cause) = chain.iter().next() {
                assert_eq!(joined.kind, 1);
                assert_eq!(cause.kind(), causes::CAPACITY);
                cause.site
            } else {
                assert_eq!(joined.kind, 0);
                0
            };
            (*case).observations[index] = (result, refusal);
        }
        assert_eq!((*context).available(), before);
        tape.end(&ending()).unwrap();
    }
    0
}
