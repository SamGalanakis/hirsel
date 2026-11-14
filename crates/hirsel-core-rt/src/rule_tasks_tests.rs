//! CONC-8/10/11 and FAIL-3 at the scheduler/custody boundary.
use super::*;
#[derive(Default)]
struct Trace {
    events: Vec<Event>,
    choices: Vec<(usize, usize)>,
    replay: Option<Vec<(usize, usize)>>,
}
unsafe extern "C" fn event(state: *mut u8, e: Event) {
    // SAFETY: scheduler alone accesses this pinned host recorder.
    unsafe {
        (*state.cast::<Trace>()).events.push(e);
    }
}
unsafe extern "C" fn choose(state: *mut u8, count: usize) -> usize {
    // SAFETY: scheduler alone accesses the recorder; replay has one choice per call.
    unsafe {
        let trace = &mut *state.cast::<Trace>();
        let selected = trace.replay.as_ref().map_or(count - 1, |choices| {
            let (old, selected) = choices[trace.choices.len()];
            assert_eq!(old, count);
            selected
        });
        trace.choices.push((count, selected));
        selected
    }
}
fn recorded(body: Body, replay: Option<Vec<(usize, usize)>>) -> (u64, Trace) {
    let mut trace = Trace {
        replay,
        ..Trace::default()
    };
    let mut context = Context::new(4096);
    causes::task_start(&mut context).unwrap();
    let before = context.available();
    let mut output = 0u64;
    let mut unit = 0u8;
    // SAFETY: pinned host recorder and root output live until every child is retired.
    unsafe {
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
                    choose: Some(choose)
                }
            ),
            0
        );
    }
    assert_eq!(context.available(), before);
    if let Some(replay) = &trace.replay {
        assert_eq!(trace.choices.len(), replay.len());
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
            64,
            (&mut pointer as *mut *mut u64).cast(),
            child,
            TaskLayout {
                input: value::<*mut u64>(0),
                result: ValueLayout {
                    drop: result_drop,
                    ..value::<ResultValue>(16)
                },
            },
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
