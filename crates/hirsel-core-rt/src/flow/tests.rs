use super::*;

#[test]
fn equal_keys_take_lower_port_first_and_do_not_overtake_an_empty_lower_port() {
    let mut ports: [Port<_, VecDeque<_>>; 2] = [Port::new(2, 0), Port::new(2, 0)];
    ports[1].push(5, 1);
    ports[0].push(5, 2);
    ports[1].push(6, 3);
    ports[0].push(6, 4);
    ports[0].end();
    ports[1].end();
    let mut values = Vec::new();
    while let Head::Entry { index, entry, .. } = merge_head(&ports, &[0, 1]) {
        if let Some(value) = ports[index].take(entry) {
            values.push(value);
        }
    }
    assert_eq!(values, [2, 1, 4, 3]);
    assert_eq!(merge_head(&ports, &[0, 1]), Head::End);
    let mut ports: [Port<_, VecDeque<_>>; 2] = [Port::new(1, 0), Port::new(1, 0)];
    ports[0].advance(5);
    ports[1].push(5, 1);
    ports[0].take(Entry::Mark);
    assert_eq!(merge_head(&ports, &[0, 1]), Head::Stalled);
}

#[test]
fn closed_credit_counts_only_the_readers_takes_from_that_port() {
    let mut a = Port::<_, VecDeque<_>>::new(1, 0);
    let mut b = Port::<_, VecDeque<_>>::new(1, 0);
    b.push(3, 3);
    a.push(1, 1);
    assert_eq!(a.take(Entry::Item), Some(1));
    a.push(2, 2);
    assert_eq!(a.take(Entry::Item), Some(2));
    a.reader_gone = true;
    b.reader_gone = true;
    assert_eq!(a.push_status(1), Push::Order);
    assert_eq!(b.push_status(4), Push::Closed);
    assert_eq!(a.push_status(5), Push::Ready);
    a.push(5, 5);
    assert_eq!(a.push_status(6), Push::Closed);
    a.end();
    assert_eq!(a.push_status(4), Push::Ended);
}

fn words() -> StageCharge {
    StageCharge {
        output: 8,
        working: 0,
        state: 8,
        item_steps: 1000,
    }
}

fn both_modes(mut law: impl FnMut()) {
    use crate::schedule_script::{FlowMode, ScheduleScript};
    for mode in [FlowMode::Eager, FlowMode::Lazy] {
        std::sync::Arc::new(ScheduleScript::new(0).with_flow_mode(mode)).install(&mut law);
    }
}

#[test]
fn reference_closure_preserves_failure_key_and_credit() {
    both_modes(|| {
        let ctx = crate::Context::new(64_000);
        let mut flow = Reference::<u64>::default();
        let sink = flow.sink().unwrap();
        let first = flow
            .stage(&ctx, sink, 2, words(), |_, key, value| {
                if key == 1 {
                    Err(Halt::Failed)
                } else {
                    Ok(Some(*value))
                }
            })
            .unwrap();
        let second = flow
            .stage(&ctx, first, 1, words(), |_, _, value| Ok(Some(*value)))
            .unwrap();
        let port = flow.port(&ctx, second, 2, 8).unwrap();
        for x in 1..=5 {
            assert_eq!(flow.push(port, x, x), Ok(()));
        }
        assert_eq!(flow.push(port, 6, 6), Err(PushError::Closed(6)));
        assert_eq!(flow.next(sink).unwrap(), Pull::Dropped(0, 1));
        assert_eq!(flow.next(sink).unwrap(), Pull::End);
        assert_eq!(
            flow.finish(),
            Err(Failure {
                stage: 0,
                key: 1,
                halt: Halt::Failed
            })
        );

        let mut flow = Reference::<u64>::default();
        let sink = flow.sink().unwrap();
        let input = flow
            .stage(&ctx, sink, 1, words(), |_, _, value| Ok(Some(*value)))
            .unwrap();
        let a = flow.port(&ctx, input, 1, 8).unwrap();
        let b = flow.port(&ctx, sink, 2, 8).unwrap();
        flow.advance(a, 10).unwrap();
        flow.push(b, 3, 3).unwrap();
        flow.push(b, 12, 12).unwrap();
        flow.end(b).unwrap();
        assert_eq!(flow.next(sink).unwrap(), Pull::Item(3, 1, 3));
        assert_eq!(flow.next(sink).unwrap(), Pull::Stalled);
        flow.advance(a, 13).unwrap();
        assert_eq!(flow.next(sink).unwrap(), Pull::Item(12, 1, 12));
        assert_eq!(flow.finish(), Ok(()));
    });
}

#[test]
fn coalescing_marks_preserves_the_failure_key_in_every_order() {
    use crate::schedule_script::{
        FlowMode, FlowStep, PausePoint, ScheduleScript, StepBoundary, UNTAPED,
    };
    use std::sync::{Arc, Mutex};

    for mode in [FlowMode::Eager, FlowMode::Lazy] {
        let mut prefixes = vec![vec![]];
        let mut saw_mark_before_second_item = false;
        let mut saw_coalesced_mark = false;
        while let Some(prefix) = prefixes.pop() {
            let trace = Arc::new(Mutex::new(Vec::new()));
            let record = Arc::clone(&trace);
            let script = Arc::new(
                ScheduleScript::new(0)
                    .with_flow_mode(mode)
                    .with_flow_prefix(UNTAPED, prefix.clone())
                    .with_pause_hook(move |point| {
                        if let PausePoint::Flow {
                            stage,
                            step,
                            boundary: StepBoundary::After,
                            ..
                        } = point
                        {
                            record.lock().unwrap().push((stage, step));
                        }
                    }),
            );
            script.install(|| {
                let context = crate::Context::new(9000);
                let mut flow = Reference::<u64>::default();
                let sink = flow.sink().unwrap();
                let f = flow
                    .stage(&context, sink, 1, words(), |_, _, _| Err(Halt::Failed))
                    .unwrap();
                let p = flow
                    .stage(&context, f, 1, words(), |_, _, _| Ok(None))
                    .unwrap();
                let q = flow.port(&context, f, 1, 8).unwrap();
                let pa = flow.port(&context, p, 2, 8).unwrap();
                let pb = flow.port(&context, p, 1, 8).unwrap();
                flow.push(q, 7, 7).unwrap();
                flow.push(pa, 5, 5).unwrap();
                flow.push(pa, 10, 10).unwrap();
                flow.advance(pb, 11).unwrap();
                assert_eq!(flow.next(sink), Ok(Pull::Dropped(0, 7)));
                assert_eq!(flow.next(sink), Ok(Pull::End));
                assert_eq!(
                    flow.finish(),
                    Err(Failure {
                        stage: 0,
                        key: 7,
                        halt: Halt::Failed
                    })
                );
            });
            let trace = trace.lock().unwrap();
            let before_failure: Vec<_> = trace
                .iter()
                .copied()
                .take_while(|step| *step != (0, FlowStep::Item))
                .collect();
            saw_mark_before_second_item |=
                before_failure.starts_with(&[(1, FlowStep::Item), (0, FlowStep::Mark)]);
            saw_coalesced_mark |=
                before_failure.starts_with(&[(1, FlowStep::Item), (1, FlowStep::Item)]);
            let choices = script.flow_decisions(UNTAPED);
            for i in prefix.len()..choices.len() {
                let (available, selected) = choices[i];
                for alternate in (0..available).filter(|&j| j != selected) {
                    let mut choice: Vec<_> = choices[..i].iter().map(|&(_, j)| j).collect();
                    choice.push(alternate);
                    prefixes.push(choice);
                }
            }
        }
        assert!(
            saw_mark_before_second_item && saw_coalesced_mark,
            "{mode:?}"
        );
    }
}

#[test]
fn four_items_through_three_stages_agree_in_every_order() {
    use crate::schedule_script::{FlowMode, ScheduleScript, UNTAPED};
    use std::sync::Arc;

    for mode in [FlowMode::Eager, FlowMode::Lazy] {
        let mut prefixes = vec![vec![]];
        let mut runs = 0;
        while let Some(prefix) = prefixes.pop() {
            let script = Arc::new(
                ScheduleScript::new(1)
                    .with_flow_mode(mode)
                    .with_flow_prefix(UNTAPED, prefix.clone()),
            );
            script.install(|| {
                let context = crate::Context::new(9000);
                let mut flow = Reference::<u64>::default();
                let sink = flow.sink().unwrap();
                let mut into = sink;
                for _ in 0..3 {
                    into = flow
                        .stage(&context, into, 4, words(), |_, _, value| {
                            Ok(Some(value + 1))
                        })
                        .unwrap();
                }
                let p = flow.port(&context, into, 4, 8).unwrap();
                let frontier = flow.port(&context, into, 1, 8).unwrap();
                for key in 1..=4 {
                    flow.push(p, key, key).unwrap();
                }
                flow.advance(frontier, 5).unwrap();
                for key in 1..=4 {
                    assert_eq!(flow.next(sink), Ok(Pull::Item(key, 0, key + 3)));
                }
                assert_eq!(flow.finish(), Ok(()));
            });
            runs += 1;
            let choices = script.flow_decisions(UNTAPED);
            for i in prefix.len()..choices.len() {
                let (available, selected) = choices[i];
                for alternate in (0..available).filter(|&j| j != selected) {
                    let mut choice: Vec<_> = choices[..i].iter().map(|&(_, j)| j).collect();
                    choice.push(alternate);
                    prefixes.push(choice);
                }
            }
        }
        assert!(runs > 1, "{mode:?} must exercise competing enabled steps");
    }
}

#[test]
fn finish_reports_the_same_failure_before_or_after_advance_has_run_the_item() {
    use crate::schedule_script::{FlowMode, ScheduleScript};
    use std::sync::{Arc, atomic::AtomicUsize};

    for (mode, before_finish) in [(FlowMode::Eager, 1), (FlowMode::Lazy, 0)] {
        let script = Arc::new(ScheduleScript::new(0).with_flow_mode(mode));
        script.install(|| {
            let context = crate::Context::new(900);
            let calls = Arc::new(AtomicUsize::new(0));
            let observed = Arc::clone(&calls);
            let mut flow = Reference::<u64>::default();
            let sink = flow.sink().unwrap();
            let into = flow
                .stage(&context, sink, 1, words(), move |_, _, value| {
                    observed.fetch_add(1, Ordering::Relaxed);
                    100u64.checked_div(*value).map(Some).ok_or(Halt::Failed)
                })
                .unwrap();
            let p = flow.port(&context, into, 1, 8).unwrap();
            assert_eq!(flow.push(p, 7, 0), Ok(()));
            flow.advance(p, 8).unwrap();
            assert_eq!(calls.load(Ordering::Relaxed), before_finish);
            assert_eq!(
                flow.finish(),
                Err(Failure {
                    stage: 0,
                    key: 7,
                    halt: Halt::Failed
                })
            );
            assert_eq!(calls.load(Ordering::Relaxed), 1);
            assert_eq!(context.available(), 900);
        });
    }
}
