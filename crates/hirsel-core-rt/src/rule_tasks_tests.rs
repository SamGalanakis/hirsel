//! CONC-8/10/11 and FAIL-3 at the scheduler/custody boundary.
use super::*;
use crate::schedule_script::{ScheduleScript, UNTAPED};
use std::sync::Arc;
#[derive(Default)]
struct Trace {
    events: Vec<Event>,
    choices: Vec<(usize, usize)>,
}
unsafe extern "C" fn event(state: *mut u8, e: Event) {
    // SAFETY: scheduler alone accesses this pinned host recorder.
    unsafe {
        (*state.cast::<Trace>()).events.push(e);
    }
}
fn recorded(body: Body, replay: Option<Vec<(usize, usize)>>) -> (u64, Trace) {
    let mut trace = Trace::default();
    let mut script = ScheduleScript::last();
    if let Some(prefix) = &replay {
        script =
            script.with_schedule_prefix(UNTAPED, prefix.iter().map(|&(_, index)| index).collect());
    }
    let script = Arc::new(script);
    let mut context = Context::new(4096);
    causes::task_start(&mut context).unwrap();
    let before = context.available();
    let mut output = 0u64;
    let mut unit = 0u8;
    // SAFETY: pinned host recorder and root output live until every child is retired.
    script.install(|| unsafe {
        assert_eq!(
            run_root(
                &mut context,
                body,
                &mut unit,
                (&mut output as *mut u64).cast(),
                0,
                8,
                SchedulerProvider {
                    state: (&mut trace as *mut Trace).cast(),
                    event: Some(event),
                    ..SchedulerProvider::default()
                }
            ),
            0
        );
    });
    trace.choices = script.schedule_decisions(UNTAPED);
    assert_eq!(context.available(), before);
    if let Some(replay) = replay {
        assert_eq!(trace.choices, replay);
    }
    (output, trace)
}
#[derive(Clone, Copy)]
struct ResultValue {
    value: u64,
    witness: *mut u64,
}
unsafe extern "C" fn child(_: *mut Context, out: *mut u8, input: *mut u8) -> u8 {
    // SAFETY: sole initialized pointer input; only the parent cleanup writes the witness.
    unsafe {
        out.cast::<ResultValue>().write(ResultValue {
            value: 41,
            witness: input.cast::<*mut u64>().read(),
        });
    }
    0
}
/// The drain's view of `result_drop`: one whole step.
unsafe extern "C" fn clean_result(
    context: *mut Context,
    value: *mut u8,
    _: *mut u64,
    list: *mut crate::counted::Worklist,
) -> bool {
    // SAFETY: the drain transfers the value as `result_drop` expects.
    unsafe {
        if result_drop(context, value) != 0 {
            (*list).fail();
        }
    }
    true
}
unsafe extern "C" fn result_drop(context: *mut Context, result: *mut u8) -> u8 {
    // SAFETY: abandonment transfers the initialized result once to this cleanup.
    unsafe {
        let result = result.cast::<ResultValue>().read();
        let observed = hirsel_core_check_cancel(context, 88);
        *result.witness = result.value + u64::from(observed);
    }
    0
}
unsafe extern "C" fn abandon(context: *mut Context, out: *mut u8, _: *mut u8) -> u8 {
    // SAFETY: parent pins its witness until shielded child-result cleanup finishes.
    unsafe {
        let mut witness = 0u64;
        let mut pointer = &mut witness as *mut u64;
        let task = hirsel_core_spawn(
            context,
            0,
            64,
            (&mut pointer as *mut *mut u64).cast(),
            child,
            TaskLayout {
                input: value::<*mut u64>(0),
                result: ValueLayout {
                    clean: Some(clean_result),
                    ..value::<ResultValue>(16)
                },
            },
            0,
        );
        (*context)
            .cancel_requested
            .store(true, std::sync::atomic::Ordering::Release);
        hirsel_core_task_drop(context, task.task);
        let pending = (*context)
            .cancel_requested
            .load(std::sync::atomic::Ordering::Acquire);
        let clean = (*context).causes.iter().next().is_none();
        // Let the caller inspect cleanup, pending-request preservation and no failure.
        out.cast::<u64>()
            .write(witness + 100 * u64::from(pending) + 1000 * u64::from(clean));
        (*context)
            .cancel_requested
            .store(false, std::sync::atomic::Ordering::Release);
    }
    0
}
#[test]
fn conc8_abandon_shields_parent_waits_for_end_and_drops_result_before_return_with_replay() {
    let (output, record) = recorded(abandon, None);
    assert_eq!(output, 1141);
    let spawned = record
        .events
        .iter()
        .find(|e| e.kind == SPAWN)
        .unwrap()
        .object;
    let cancel = record
        .events
        .iter()
        .position(|e| e.kind == CANCEL && e.object == spawned)
        .unwrap();
    let end = record
        .events
        .iter()
        .position(|e| e.kind == END && e.object == spawned)
        .unwrap();
    let join = record
        .events
        .iter()
        .position(|e| e.kind == JOIN && e.object == spawned && e.value == 2)
        .unwrap();
    assert!(cancel < end && end < join);
    assert!(!record.events.iter().any(|e| e.kind == CANCELLED));
    assert!(
        record
            .events
            .iter()
            .any(|e| e.kind == REFUND && e.object == 0 && e.task != 0 && e.value == 64)
    );
    let (output, replay) = recorded(abandon, Some(record.choices.clone()));
    assert_eq!(output, 1141);
    assert_eq!(record.events, replay.events);
    assert_eq!(record.choices, replay.choices);
}

#[test]
fn conc8_aligned_result_survives_another_spawn_during_cleanup() {
    #[repr(C, align(32))]
    struct Input {
        value: u64,
        witness: *mut u64,
    }

    #[repr(C, align(64))]
    struct Output(Input);

    unsafe extern "C" fn body(_: *mut Context, out: *mut u8, input: *mut u8) -> u8 {
        assert!(input.cast::<Input>().is_aligned());
        assert!(out.cast::<Output>().is_aligned());
        // SAFETY: these are the input and output slots of this task's layout.
        unsafe {
            out.cast::<Output>()
                .write(Output(input.cast::<Input>().read()))
        };
        0
    }

    fn spawn(context: *mut Context, mut input: Input) -> *mut Task {
        let layout = TaskLayout {
            input: value::<Input>(0),
            result: ValueLayout {
                clean: Some(clean),
                ..value::<Output>(64)
            },
        };
        // SAFETY: the task moves the local input on success.
        let task =
            unsafe { hirsel_core_spawn(context, 0, 0, (&raw mut input).cast(), body, layout, 0) };
        assert_eq!(task.status, 0);
        assert!(!task.task.is_null());
        task.task
    }

    fn spawn_join(context: *mut Context, value: u64) {
        let task = spawn(
            context,
            Input {
                value,
                witness: ptr::null_mut(),
            },
        );
        let mut out = std::mem::MaybeUninit::<Output>::uninit();
        // SAFETY: this task and the aligned destination belong to the joiner.
        unsafe {
            let joined = hirsel_core_join(context, task, out.as_mut_ptr().cast(), 1);
            assert_eq!((joined.kind, joined.status), (0, 0));
            assert!(joined.causes.is_null());
            assert_eq!(out.assume_init().0.value, value);
        }
    }

    unsafe extern "C" fn clean(
        context: *mut Context,
        result: *mut u8,
        _: *mut u64,
        _: *mut crate::counted::Worklist,
    ) -> bool {
        spawn_join(context, 73);
        // SAFETY: the abandoning task owns this result until cleanup completes.
        unsafe {
            let Output(input) = result.cast::<Output>().read();
            assert!(!input.witness.is_null());
            *input.witness = input.value;
        }
        true
    }

    unsafe extern "C" fn root(context: *mut Context, out: *mut u8, _: *mut u8) -> u8 {
        spawn_join(context, 15);
        let mut witness = 0;
        let task = spawn(
            context,
            Input {
                value: 41,
                witness: &raw mut witness,
            },
        );
        // SAFETY: abandonment consumes the handle before the witness leaves scope.
        assert_eq!(unsafe { hirsel_core_task_drop(context, task) }, 0);
        spawn_join(context, 105);
        // SAFETY: the fixture supplies a writable u64 destination.
        unsafe { out.cast::<u64>().write(witness) };
        0
    }

    assert_eq!(recorded(root, None).0, 41);
}
