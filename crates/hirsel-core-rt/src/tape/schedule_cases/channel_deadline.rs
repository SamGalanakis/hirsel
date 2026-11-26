use super::*;

pub(super) fn value<T>(logical: u64) -> ValueLayout {
    ValueLayout {
        bytes: size_of::<T>(),
        align: align_of::<T>(),
        logical,
        clean: None,
    }
}
pub(super) fn layout<T>() -> TaskLayout {
    TaskLayout {
        input: value::<T>(0),
        result: value::<()>(0),
    }
}

#[derive(Default)]
struct Clock {
    reached: bool,
    calls: u32,
}

unsafe extern "C" fn wait(state: *mut u8, request: *mut Request, _: u64) -> Wait {
    // SAFETY: this serial fixture pins the provider and request.
    unsafe {
        let clock = &mut *state.cast::<Clock>();
        clock.calls += 1;
        if (*request).operation == SLEEP_UNTIL {
            clock.reached = true;
            Wait {
                kind: WAIT_UNTIL,
                deadline_ns: crate::os::monotonic_ns(),
            }
        } else {
            Wait::default()
        }
    }
}

unsafe extern "C" fn read_clock(state: *mut u8, request: *mut Request, _: u64) -> Step {
    // SAFETY: this serial fixture pins the provider and request.
    unsafe {
        assert_eq!((*request).operation, MONO_NS);
        let clock = &mut *state.cast::<Clock>();
        clock.calls += 1;
        done(Outcome {
            value: u64::from(clock.reached),
            ..Outcome::default()
        })
    }
}

struct Case {
    store: Store,
    mode: Mode,
    chooser: u8,
    clock: Clock,
    result: Option<Result<(), Fault>>,
}

unsafe extern "C" fn publish(_: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
    // SAFETY: the parent pins the channel sender until this child joins.
    unsafe {
        let sender = input.cast::<*mut Channel>().read();
        let mut value = 26u64;
        assert_eq!(hirsel_core_try_send(sender, (&raw mut value).cast()), 0);
    }
    0
}

unsafe extern "C" fn run(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
    // SAFETY: this run owns every borrowed provider, sender, task and result.
    unsafe {
        let case = &mut **input.cast::<*mut Case>();
        let mut h = header(REVISION);
        h.kind = KIND_RUN;
        h.chooser = case.chooser;
        let (tape, _) =
            Tape::open(REVISION, case.store.dir(), b"t", case.mode, &h, &expect()).unwrap();
        let clock = Provider {
            state: (&raw mut case.clock).cast(),
            wait,
            issue: read_clock,
            resume: crate::provider::immediate::resume,
            abort: crate::provider::immediate::abort,
            deliver: crate::provider::deliver,
        };
        let (p, handle) = tape.on_tape(crate::entry::CLOCK, (clock, 0)).unwrap();
        let clock = (*(*context).host_files).registry.grant(p, handle);
        let mut q = hirsel_core_channel_new(context, 1, value::<u64>(8), 0);
        let mut result = u64::MAX;
        let empty = hirsel_core_receive_until(
            context,
            q.receiver,
            (&raw mut result).cast(),
            1,
            clock,
            1,
            0,
        );
        assert_eq!((empty.status, empty.kind, result), (0, 1, u64::MAX));

        case.clock.reached = false;
        let child = hirsel_core_spawn(
            context,
            0,
            0,
            (&raw mut q.sender).cast(),
            publish,
            layout::<*mut Channel>(),
            0,
        );
        assert!(!child.task.is_null());
        let item = hirsel_core_receive_until(
            context,
            q.receiver,
            (&raw mut result).cast(),
            1,
            clock,
            1,
            0,
        );
        assert_eq!((item.status, item.kind, result), (0, 0, 26));
        let joined = hirsel_core_join(context, child.task, ptr::null_mut(), 0);
        assert_eq!((joined.status, joined.kind), (0, 0));
        hirsel_core_sender_drop(q.sender);
        hirsel_core_receiver_drop(context, q.receiver);
        case.result = Some(tape.end(&ending()));
    }
    0
}

pub(crate) fn check() {
    for chooser in [format::CHOOSER_FIFO, format::CHOOSER_COMMIT] {
        let mut case = Case {
            store: Store::default(),
            mode: Mode::Record,
            chooser,
            clock: Clock::default(),
            result: None,
        };
        for mode in [Mode::Record, Mode::Replay] {
            case.mode = mode;
            case.clock = Clock::default();
            let mut context = Context::new(1 << 20);
            let mut files = crate::host_file::Files::default();
            context.host_files = &mut files;
            causes::task_start(&mut context).unwrap();
            let mut input = &raw mut case;
            // SAFETY: pinned cooperative fixture and copy of its address.
            let status = unsafe {
                run_root(
                    &mut context,
                    run,
                    (&raw mut input).cast(),
                    ptr::null_mut(),
                    size_of::<*mut Case>(),
                    0,
                    SchedulerProvider::default(),
                )
            };
            assert_eq!(status, 0);
            assert_eq!(case.result.take(), Some(Ok(())));
            if mode == Mode::Replay {
                assert_eq!(case.clock.calls, 0);
            }
        }
    }
}
