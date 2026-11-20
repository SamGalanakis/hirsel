//! One-sample record/untaped measurements at the native runtime ABI.
hirsel_guard::install!();
use hirsel_core_rt::{
    Context, causes,
    provider::*,
    tape::{self, format::*},
    tasks::*,
};
use std::{collections::BTreeSet, ptr, time::Instant};

#[derive(Default)]
struct Script {
    waiting: BTreeSet<u64>,
    bytes: usize,
}
unsafe extern "C" fn wait(s: *mut u8, r: *mut Request, token: u64) -> Wait {
    // SAFETY: main pins the script for the run; callbacks are synchronous.
    unsafe {
        if (*r).operation == SLEEP_UNTIL && (*s.cast::<Script>()).waiting.insert(token) {
            Wait {
                kind: WAIT_UNTIL,
                deadline_ns: 1,
            }
        } else {
            Wait::default()
        }
    }
}
unsafe extern "C" fn issue(s: *mut u8, r: *mut Request, _: u64) -> Step {
    // SAFETY: request and script are live through this callback.
    unsafe {
        let mut outcome = Outcome::default();
        if (*r).operation == OPEN {
            outcome.value = 1;
        }
        if (*r).operation == WRITE {
            (*s.cast::<Script>()).bytes += (*r).buffer.len;
            outcome.written = (*r).buffer.len as u64;
        }
        Step {
            outcome,
            ..Step::default()
        }
    }
}
unsafe extern "C" fn resume(_: *mut u8, _: *mut Request, _: u64) -> Step {
    unreachable!()
}
unsafe extern "C" fn abort(_: *mut u8, _: *mut Request, _: u64, _: u8) -> Outcome {
    unreachable!()
}
unsafe extern "C" fn deliver(_: *mut u8, _: *mut Request, o: Outcome) -> Outcome {
    o
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
struct Job {
    provider: Provider,
    handle: u64,
    wake: bool,
}
unsafe extern "C" fn child(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
    // SAFETY: the input belongs to the child, which pins its request to delivery.
    unsafe {
        let job = input.cast::<Job>().read();
        if job.wake {
            let mut r = Request {
                operation: SLEEP_UNTIL,
                mode: 0,
                handle: job.handle,
                path: ptr::null(),
                path_len: 0,
                buffer: Buffer {
                    data: ptr::null_mut(),
                    len: 0,
                    capacity: 0,
                },
                site: 0,
            };
            assert_eq!(hirsel_core_host(context, &job.provider, &mut r).status, 0);
        }
    }
    0
}
struct Run {
    provider: Provider,
    record: bool,
    wake: bool,
    n: usize,
}
unsafe extern "C" fn root(context: *mut Context, _: *mut u8, input: *mut u8) -> u8 {
    // SAFETY: Run and both providers stay pinned until all tasks join.
    unsafe {
        let run = &*input.cast::<Run>();
        let tape = if run.record {
            let header = Header {
                kind: KIND_RUN,
                mode: MODE_RECORD,
                tape_id: [1; 16],
                identity: b"cost".to_vec(),
                revision: [2; 32],
                charge_policy: 0,
                limit: 1 << 20,
                bound: 0,
                chooser: 0,
                arg: vec![],
                net_rules: vec![],
                secret_grants: vec![],
            };
            Some(
                tape::Tape::open(
                    [2; 32],
                    (run.provider, 0),
                    b"cost",
                    tape::Mode::Record,
                    &header,
                    &tape::Expect {
                        identity: b"cost",
                        charge_policy: 0,
                        limit: 1 << 20,
                        arg: None,
                        across: false,
                    },
                )
                .unwrap()
                .0,
            )
        } else {
            None
        };
        let sub = tape
            .as_ref()
            .map(|t| {
                t.on_tape(hirsel_core_rt::entry::CLOCK, (run.provider, 0))
                    .unwrap()
            })
            .unwrap_or((run.provider, 0));
        for _ in 0..run.n {
            let mut job = Job {
                provider: sub.0,
                handle: sub.1,
                wake: run.wake,
            };
            let task = hirsel_core_spawn(
                context,
                0,
                64,
                (&mut job as *mut Job).cast(),
                child,
                TaskLayout {
                    input: value::<Job>(),
                    result: value::<()>(),
                },
                0,
            );
            assert!(!task.task.is_null() && task.status == 0);
            assert_eq!(
                hirsel_core_join(context, task.task, ptr::null_mut(), 0).status,
                0
            );
        }
        if let Some(tape) = tape {
            assert_eq!(tape.end(&Ending::returned(&[0; 4])), Ok(()));
        }
    }
    0
}
fn main() {
    let args: Vec<_> = std::env::args().collect();
    let record = args[1] == "record";
    let wake = args[2] == "wake";
    let n: usize = args[3].parse().unwrap();
    let mut script = Script::default();
    let provider = Provider {
        state: (&mut script as *mut Script).cast(),
        wait,
        issue,
        resume,
        abort,
        deliver,
    };
    let mut run = Run {
        provider,
        record,
        wake,
        n,
    };
    let mut context = Context::new(1 << 20);
    causes::task_start(&mut context).unwrap();
    let started = Instant::now();
    // SAFETY: input and context remain pinned for this cooperative run.
    unsafe {
        assert_eq!(
            run_root(
                &mut context,
                root,
                (&mut run as *mut Run).cast(),
                ptr::null_mut(),
                size_of::<Run>(),
                0,
                SchedulerProvider::default()
            ),
            0
        );
    }
    println!(
        "record={record} wake={wake} n={n} ns={} bytes={}",
        started.elapsed().as_nanos(),
        script.bytes
    );
}
