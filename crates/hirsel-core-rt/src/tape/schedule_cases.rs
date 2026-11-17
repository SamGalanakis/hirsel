//! S2 cases live in S1's bypass-law body, so the existing acceptance selector
//! runs the scheduling and no-host laws together.
use super::{tests::*, *};
use crate::{Context, causes, channel::*, tasks::*};
use std::collections::BTreeSet;

struct Script {
    waited: BTreeSet<u64>,
    calls: usize,
    pending: bool,
    gap_case: bool,
}
impl Script {
    fn provider(&mut self) -> Provider {
        unsafe extern "C" fn wait(s: *mut u8, r: *mut Request, token: u64) -> Wait {
            // SAFETY: the case pins Script until its run ends.
            let s = unsafe { &mut *s.cast::<Script>() };
            s.calls += 1;
            if s.gap_case && unsafe { (*r).mode } == 2 {
                return Wait::default();
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
        unsafe extern "C" fn issue(s: *mut u8, _: *mut Request, token: u64) -> Step {
            // SAFETY: synchronous run-local script.
            let s = unsafe { &mut *s.cast::<Script>() };
            s.calls += 1;
            if s.pending {
                hirsel_core_ready(token);
                pending()
            } else {
                done(Outcome {
                    value: 17,
                    ..Outcome::default()
                })
            }
        }
        unsafe extern "C" fn resume(s: *mut u8, _: *mut Request, _: u64) -> Step {
            // SAFETY: synchronous run-local script.
            unsafe {
                (*s.cast::<Script>()).calls += 1;
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
    observations: *mut Vec<(u64, u64, u8)>,
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
        (*job.observations).push((job.number, outcome.value, outcome.status));
        let mut n = job.number;
        if outcome.status == 0 {
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
    result: Option<Result<(), Fault>>,
    observed: Vec<(u64, u64, u8)>,
    received: Vec<u64>,
}
unsafe extern "C" fn root(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
    // SAFETY: run owns every input, task handle and channel until joined/closed.
    unsafe {
        let run = &mut **input.cast::<*mut Run>();
        let mut h = header(REVISION);
        h.kind = KIND_RUN;
        h.chooser = run.chooser;
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
            assert!(!task.is_null());
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
        run.result = Some(tape.end(&ending()));
    }
    0
}
unsafe extern "C" fn lifo(_: *mut u8, n: usize) -> usize {
    n - 1
}
fn run_case(bytes: Option<&[u8]>, chooser: u8, pending: bool, gap_case: bool) -> Run {
    let mut run = Run {
        store: Store::default(),
        script: Script {
            waited: BTreeSet::new(),
            calls: 0,
            pending,
            gap_case,
        },
        mode: if bytes.is_some() {
            Mode::Replay
        } else {
            Mode::Record
        },
        chooser,
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
    unsafe {
        let result = run_root(
            &mut context,
            root,
            (&mut input as *mut *mut Run).cast(),
            ptr::null_mut(),
            size_of::<*mut Run>(),
            0,
            SchedulerProvider {
                choose: Some(lifo),
                ..SchedulerProvider::default()
            },
        );
        assert_eq!(result, 0);
    }
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
    outside_cases();
    overlapping_open();
    for chooser in [0, 1] {
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
                changed.remove(at);
                let replayed = run_case(Some(&encode(&header, &changed)), chooser, pending, false);
                assert!(
                    matches!(
                        replayed.result,
                        Some(Err(Fault::Diverged(Divergence {
                            kind: Mismatch::Schedule,
                            ..
                        })))
                    ),
                    "dropped {}: {:?}",
                    if drop_wait { "WAIT" } else { "WAKE" },
                    replayed.result
                );
            }
            let gap_frames = all;
            for delta in [-1i64, 1] {
                let mut changed = gap_frames.clone();
                let gap = changed
                    .iter_mut()
                    .find_map(|f| match f {
                        Frame::Wake { gap, .. } if *gap > 0 => Some(gap),
                        _ => None,
                    })
                    .unwrap();
                *gap = gap.checked_add_signed(delta).unwrap();
                let replayed = run_case(Some(&encode(&header, &changed)), chooser, pending, false);
                assert!(
                    matches!(replayed.result, Some(Err(Fault::Diverged(_)))),
                    "WAKE gap {delta}: {:?}",
                    replayed.result
                );
            }
        }
    }
}

fn overlapping_open() {
    struct Case {
        store: Store,
        mode: Mode,
        result: Option<(u8, usize, Result<(), Fault>, Result<(), Fault>)>,
    }
    unsafe extern "C" fn root(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
        // SAFETY: the caller pins the store and mode until this root returns.
        unsafe {
            let case = &mut **input.cast::<*mut Case>();
            let mut script = Script {
                waited: BTreeSet::new(),
                calls: 0,
                pending: false,
                gap_case: false,
            };
            let (first, _) = Tape::open(
                REVISION,
                case.store.dir(),
                b"first",
                case.mode,
                &header(REVISION),
                &expect(),
            )
            .unwrap();
            let (provider, handle) = first
                .on_tape(crate::entry::CLOCK, (script.provider(), 0))
                .unwrap();
            let (second, _) = Tape::open(
                REVISION,
                case.store.dir(),
                b"second",
                case.mode,
                &header(REVISION),
                &expect(),
            )
            .unwrap();
            let mut r = request(WALL_MS, 0, handle, &[], empty_buffer());
            let outcome = hirsel_core_host(context, &provider, &mut r);
            case.result = Some((
                outcome.status,
                script.calls,
                first.end(&ending()),
                second.end(&ending()),
            ));
        }
        0
    }
    let mut case = Case {
        store: Store::default(),
        mode: Mode::Record,
        result: None,
    };
    for mode in [Mode::Record, Mode::Replay] {
        case.mode = mode;
        let mut context = Context::new(1 << 20);
        let mut files = crate::host_file::Files::default();
        context.host_files = &mut files;
        causes::task_start(&mut context).unwrap();
        let mut input = &mut case as *mut Case;
        // SAFETY: the case and its file registry outlive this cooperative run.
        unsafe {
            assert_eq!(
                run_root(
                    &mut context,
                    root,
                    (&mut input as *mut *mut Case).cast(),
                    ptr::null_mut(),
                    size_of::<*mut Case>(),
                    0,
                    SchedulerProvider::default(),
                ),
                0
            );
        }
        let (status, calls, first, second) = case.result.take().unwrap();
        assert_eq!(status, 1);
        assert_eq!(calls, 0, "overlap reached the inner provider");
        assert!(
            matches!(
                first,
                Err(Fault::Diverged(Divergence {
                    kind: Mismatch::Schedule,
                    ..
                }))
            ),
            "{first:?}"
        );
        assert_eq!(second, Ok(()));
    }
}

fn grant_gap_vector() {
    let mut store = Store::default();
    let mut script = Script {
        waited: BTreeSet::new(),
        calls: 0,
        pending: false,
        gap_case: false,
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
    group.event();
    let (p, h) = tape
        .on_tape(crate::entry::CLOCK, (script.provider(), 0))
        .unwrap();
    group.event();
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
    group.event();
    let (p, h) = tape
        .on_tape(crate::entry::CLOCK, (script.provider(), 0))
        .unwrap();
    group.event();
    r.handle = h;
    unsafe {
        assert_eq!((p.wait)(p.state, &mut r, 2).kind, WAIT_PARK);
    }
    assert!(matches!(group.input(), Some(schedule::Input::Wake(0))));
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
            checkpoint(false);
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
            checkpoint(false);
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
