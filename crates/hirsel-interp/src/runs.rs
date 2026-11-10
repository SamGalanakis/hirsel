//! One execution's runs (RUN-10, RUN-12): the root run and the callee runs its runner
//! calls start in this process. Each run keeps its own scheduler, failures and outcome;
//! they share nothing but the program. A run executes until none of its tasks can
//! proceed, then the next ready run takes over, in the order they became ready. CONC-8
//! is decided across the execution: it is Blocked only when no run has a ready task and
//! none of their providers can make external progress. Runs whose providers share one
//! readiness (host runs) are asked whether a wait is ready before any of them sleeps, so a
//! wakeup reaches the run it belongs to.
use crate::eval::{Scheduler, Step, enter_checked, execute_frames};
use crate::frame::Frame;
use crate::*;
use hirsel_tir::TirProvider;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// One run of the execution.
pub(crate) struct Run {
    configuration: RunConfiguration,
    state: Option<Scheduler>,
    pub(crate) execution: Execution,
    pub(crate) entry_consumed: bool,
}
impl Run {
    pub(crate) fn new(
        provider: &mut (impl TirProvider + ?Sized),
        configuration: RunConfiguration,
        arguments: Vec<Value>,
        limits: Limits,
        runtime: &aggregate::RuntimeTypes,
        budgets: RunBudgets,
        checker: Option<&checked::Shared>,
    ) -> Result<Self, ExecutionError> {
        let constants = constants::Constants::initialize(provider, runtime)?;
        let definition = provider
            .definition(&configuration.entry)
            .map_err(Error::Query)?;
        let mut frame = Frame::new(runtime.types.clone(), definition, arguments)?;
        if let Some(state) = checker {
            frame.check = Some(enter_checked(state, &frame, vec![], None)?);
        }
        frame.depth_failure = limits.call_depth == 0;
        let execution = Execution {
            failures: vec![],
            outcome: Outcome::Returned {
                value: Value::Unit,
                exit_code: 0,
            },
            stdout: vec![],
            trace: if runtime.trace_mode == TraceMode::Full {
                vec![TraceEvent::Enter {
                    path: configuration.entry.clone(),
                    depth: 1,
                }]
            } else {
                vec![]
            },
            steps: 0,
            charged: 0,
        };
        Ok(Self {
            configuration,
            state: Some(Scheduler::new(frame, constants, budgets)),
            execution,
            entry_consumed: false,
        })
    }
    fn step<O: crate::ownership_trace::Observer>(
        &mut self,
        provider: &mut (impl TirProvider + ?Sized),
        sources: &SourceMap,
        limits: Limits,
        operations: &mut (impl Operations + ?Sized),
        runtime: &aggregate::RuntimeTypes,
        observer: &mut O,
    ) -> Result<Step, Error> {
        execute_frames(
            provider,
            sources,
            &self.configuration,
            limits,
            operations,
            runtime,
            &mut self.state,
            &mut self.execution,
            &mut self.entry_consumed,
            observer,
        )
    }
    /// An operation context of this run outside any operation.
    fn context<'a>(
        &'a mut self,
        sources: &'a SourceMap,
        runtime: &aggregate::RuntimeTypes,
    ) -> OperationContext<'a> {
        OperationContext {
            configuration: &self.configuration,
            sources,
            stdout: &mut self.execution.stdout,
            trace: &mut self.execution.trace,
            trace_mode: runtime.trace_mode,
            anchor: None,
        }
    }
    fn waits(&self) -> Vec<BlockedWait> {
        self.state
            .as_ref()
            .map(Scheduler::waits)
            .unwrap_or_default()
    }
}

/// A callee run and the call it answers.
struct Callee {
    run: Run,
    operations: Box<dyn Operations>,
    /// The calling run (0 is the root) and its wait for the outcome.
    caller: usize,
    token: u64,
    started: bool,
    /// Suspensions so far, and the point at which its place crashes (`sim@1`).
    suspensions: u32,
    crash_after: Option<u32>,
    /// The `db@1` place this run is the head of; its end ends the place's calls, not a
    /// call of its starter's.
    place: Option<String>,
}

/// The `db@1` places in this execution (STORE-12): each one's head run, the places whose
/// head ended, and every delivered call still owed an end, by arrival.
#[derive(Default)]
struct Places {
    heads: BTreeMap<String, usize>,
    closed: BTreeSet<String>,
    /// Arrival to its place, calling run and that run's wait.
    owed: BTreeMap<u64, (String, usize, u64)>,
    next: u64,
}
const UNREACHABLE: u32 = 2;
const ABANDONED: u32 = 2;

/// Run the root and every callee run it starts until the root ends; the root's outcome
/// and its result. A callee's tooling limit, undefined behavior, trap or environmental
/// stop ends the whole execution with that outcome, as a process crash ends caller and
/// callees together.
pub(crate) fn execute<O: crate::ownership_trace::Observer>(
    provider: &mut (impl TirProvider + ?Sized),
    sources: &SourceMap,
    limits: Limits,
    root_operations: &mut dyn Operations,
    runtime: &aggregate::RuntimeTypes,
    mut root: Run,
    observer: &mut O,
) -> (Run, Result<(), Error>) {
    let mut callees: Vec<Option<Callee>> = vec![];
    let mut ready: VecDeque<usize> = VecDeque::from([0]);
    let mut places = Places::default();
    let result = 'run: loop {
        let Some(index) = ready.pop_front() else {
            // Nothing is ready: first the runs whose provider already has a ready wait,
            // then let each idle run's provider make external progress, in run order
            // (virtual time, host readiness).
            let ready_now = |root: &mut dyn Operations, callees: &mut [Option<Callee>]| {
                let mut now = vec![];
                for index in 0..=callees.len() {
                    let operations = match index {
                        0 => &mut *root,
                        i => match &mut callees[i - 1] {
                            Some(callee) => callee.operations.as_mut(),
                            None => continue,
                        },
                    };
                    if operations.ready_now()? {
                        now.push(index);
                    }
                }
                Ok::<_, Error>(now)
            };
            match ready_now(root_operations, &mut callees) {
                Ok(now) if !now.is_empty() => {
                    ready.extend(now);
                    continue;
                }
                Ok(_) => (),
                Err(error) => break 'run Err(error),
            }
            let mut progressed = None;
            for index in 0..=callees.len() {
                let operations = match index {
                    0 => &mut *root_operations,
                    i => match &mut callees[i - 1] {
                        Some(callee) => callee.operations.as_mut(),
                        None => continue,
                    },
                };
                match operations.wait() {
                    Ok(true) => {
                        progressed = Some(index);
                        break;
                    }
                    Ok(false) => (),
                    Err(error) => break 'run Err(error),
                }
            }
            match progressed {
                // A shared readiness may have woken another run's wait.
                Some(index) => match ready_now(root_operations, &mut callees) {
                    Ok(now) if !now.is_empty() => ready.extend(now),
                    Ok(_) => ready.push_back(index),
                    Err(error) => break 'run Err(error),
                },
                None => {
                    let mut waits = root.waits();
                    for callee in callees.iter().flatten() {
                        waits.extend(callee.run.waits());
                    }
                    break 'run Err(Error::Blocked(waits));
                }
            }
            continue;
        };
        observer.run(index);
        let step = if index == 0 {
            root.step(
                provider,
                sources,
                limits,
                root_operations,
                runtime,
                observer,
            )
        } else {
            let callee = callees[index - 1].as_mut().expect("a ready callee is live");
            callee.started = true;
            let step = callee.run.step(
                provider,
                sources,
                limits,
                callee.operations.as_mut(),
                runtime,
                observer,
            );
            // A place in this process writes to the process's own output.
            root.execution
                .stdout
                .append(&mut callee.run.execution.stdout);
            step
        };
        if let Err(error) = route_places(
            provider,
            sources,
            limits,
            runtime,
            &mut root,
            &mut *root_operations,
            &mut callees,
            &mut ready,
            &mut places,
            index,
        ) {
            break 'run Err(error);
        }
        let end = match step {
            Ok(Step::Idle) => {
                let operations = match index {
                    0 => &mut *root_operations,
                    i => callees[i - 1].as_mut().unwrap().operations.as_mut(),
                };
                let calls = operations.take_calls();
                let cancels = operations.take_cancels();
                for call in calls {
                    let checker = runtime.checked.clone().map(checked::state);
                    let run = match Run::new(
                        provider,
                        call.configuration,
                        call.arguments,
                        limits,
                        runtime,
                        call.budgets,
                        checker.as_ref(),
                    ) {
                        Ok(run) => run,
                        Err(error) => break 'run Err(error.error),
                    };
                    callees.push(Some(Callee {
                        run,
                        operations: call.operations,
                        caller: index,
                        token: call.token,
                        started: false,
                        suspensions: 0,
                        crash_after: call.crash_after,
                        place: None,
                    }));
                    ready.push_back(callees.len());
                }
                for token in cancels {
                    let Some(slot) = callees.iter().position(|c| {
                        c.as_ref()
                            .is_some_and(|c| c.caller == index && c.token == token)
                    }) else {
                        continue;
                    };
                    let callee = callees[slot].as_mut().unwrap();
                    if callee.started {
                        // RUN-9: the callee observes cancellation; its caller waits for
                        // its outcome.
                        callee.operations.cancel_run();
                        if !ready.contains(&(slot + 1)) {
                            ready.push_back(slot + 1);
                        }
                    } else {
                        let mut callee = callees[slot].take().unwrap();
                        ready.retain(|r| *r != slot + 1);
                        if let Err(error) = callee.operations.callee_ended(
                            &CalleeEnd::Canceled,
                            runtime,
                            &mut callee.run.context(sources, runtime),
                        ) {
                            break 'run Err(error);
                        }
                        if let Err(error) = ended(
                            sources,
                            runtime,
                            &mut root,
                            &mut *root_operations,
                            &mut callees,
                            index,
                            token,
                            CalleeEnd::Canceled,
                        ) {
                            break 'run Err(error);
                        }
                        if !ready.contains(&index) {
                            ready.push_back(index);
                        }
                    }
                }
                if index != 0 {
                    let callee = callees[index - 1].as_mut().unwrap();
                    callee.suspensions += 1;
                    if !callee.operations.stops_itself()
                        && callee.crash_after.is_some_and(|at| callee.suspensions > at)
                    {
                        Some(CalleeEnd::Abandoned)
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            Ok(Step::Finished) if index == 0 => break Ok(()),
            Ok(Step::Finished) => {
                let callee = callees[index - 1].as_mut().unwrap();
                let execution = &mut callee.run.execution;
                match std::mem::replace(
                    &mut execution.outcome,
                    Outcome::Returned {
                        value: Value::Unit,
                        exit_code: 0,
                    },
                ) {
                    Outcome::Returned { value, .. } => Some(CalleeEnd::Returned(value)),
                    Outcome::Failed(_) => {
                        Some(CalleeEnd::Failed(std::mem::take(&mut execution.failures)))
                    }
                    Outcome::BudgetExceeded(kind) => Some(CalleeEnd::BudgetExceeded(kind)),
                    stop => {
                        root.execution.outcome = stop;
                        break Ok(());
                    }
                }
            }
            Err(Error::BudgetExceeded(kind)) if index != 0 => Some(CalleeEnd::BudgetExceeded(kind)),
            // Its place crashed before one of its requests.
            Err(Error::Environmental(hirsel_run_schema::Environmental::Stopped)) if index != 0 => {
                Some(CalleeEnd::Abandoned)
            }
            Err(Error::Blocked(waits)) if index != 0 => {
                // A provider that can never complete an operation stops its run as
                // Blocked at once; the execution cannot complete the call either.
                let mut all = root.waits();
                all.extend(waits);
                break Err(Error::Blocked(all));
            }
            Err(error) => break Err(error),
        };
        if let Some(end) = end {
            let mut callee = callees[index - 1].take().unwrap();
            ready.retain(|r| *r != index);
            if let Some(place) = callee.place.take() {
                // The head's end ends every call it was owed (runners.md, db@1).
                places.heads.remove(&place);
                places.closed.insert(place.clone());
                let mut ends = callee.operations.place_closed();
                ends.extend(
                    places
                        .owed
                        .iter()
                        .filter(|(id, (p, ..))| *p == place && !ends.iter().any(|(e, _)| e == *id))
                        .map(|(id, _)| (*id, hirsel_run_schema::CallOutcome::InDoubt(ABANDONED)))
                        .collect::<Vec<_>>(),
                );
                for (id, outcome) in ends {
                    if let Err(error) = end_arrival(
                        sources,
                        runtime,
                        &mut root,
                        &mut *root_operations,
                        &mut callees,
                        &mut ready,
                        &mut places,
                        id,
                        outcome,
                    ) {
                        break 'run Err(error);
                    }
                }
                continue;
            }
            if let Err(error) = callee.operations.callee_ended(
                &end,
                runtime,
                &mut callee.run.context(sources, runtime),
            ) {
                break Err(error);
            }
            // A callee that ended before its place's crash point records its own end, and
            // the crash withholds its outcome.
            let end = match end {
                CalleeEnd::Canceled => end,
                _ if callee.crash_after.is_some() => CalleeEnd::Abandoned,
                end => end,
            };
            if let Err(error) = ended(
                sources,
                runtime,
                &mut root,
                &mut *root_operations,
                &mut callees,
                callee.caller,
                callee.token,
                end,
            ) {
                break Err(error);
            }
            if !ready.contains(&callee.caller) {
                ready.push_back(callee.caller);
            }
        }
    };
    (root, result)
}

/// Deliver a callee's end to its caller's provider.
#[allow(clippy::too_many_arguments)]
fn ended(
    sources: &SourceMap,
    runtime: &aggregate::RuntimeTypes,
    root: &mut Run,
    root_operations: &mut dyn Operations,
    callees: &mut [Option<Callee>],
    caller: usize,
    token: u64,
    end: CalleeEnd,
) -> Result<(), Error> {
    match caller {
        0 => root_operations.call_ended(token, end, &mut root.context(sources, runtime)),
        i => {
            let callee = callees[i - 1]
                .as_mut()
                .ok_or_else(|| invalid("a callee outlived its caller"))?;
            callee
                .operations
                .call_ended(token, end, &mut callee.run.context(sources, runtime))
        }
    }
}

/// Route the calls run `index` sent to places, and the ends it gave as a head.
#[allow(clippy::too_many_arguments)]
fn route_places(
    provider: &mut (impl TirProvider + ?Sized),
    sources: &SourceMap,
    limits: Limits,
    runtime: &aggregate::RuntimeTypes,
    root: &mut Run,
    root_operations: &mut dyn Operations,
    callees: &mut Vec<Option<Callee>>,
    ready: &mut VecDeque<usize>,
    places: &mut Places,
    index: usize,
) -> Result<(), Error> {
    let (arrivals, ends) = {
        let operations: &mut dyn Operations = match index {
            0 => root_operations,
            i => match callees[i - 1].as_mut() {
                Some(callee) => callee.operations.as_mut(),
                None => return Ok(()),
            },
        };
        (operations.take_arrivals(), operations.take_arrival_ends())
    };
    for arrival in arrivals {
        let head = match places.heads.get(&arrival.place) {
            Some(head) => Some(*head),
            None if places.closed.contains(&arrival.place) => None,
            None => match arrival.head {
                Some(start) => {
                    let checker = runtime.checked.clone().map(checked::state);
                    let run = Run::new(
                        provider,
                        start.configuration,
                        start.arguments,
                        limits,
                        runtime,
                        RunBudgets::default(),
                        checker.as_ref(),
                    )
                    .map_err(|error| error.error)?;
                    callees.push(Some(Callee {
                        run,
                        operations: start.operations,
                        caller: index,
                        token: 0,
                        started: false,
                        suspensions: 0,
                        crash_after: None,
                        place: Some(arrival.place.clone()),
                    }));
                    places.heads.insert(arrival.place.clone(), callees.len());
                    Some(callees.len())
                }
                None => None,
            },
        };
        let Some(head) = head else {
            // No head holds the place: the call never ran.
            let id = places.next;
            places.next += 1;
            places
                .owed
                .insert(id, (arrival.place, index, arrival.token));
            let root_operations = &mut *root_operations;
            end_arrival(
                sources,
                runtime,
                root,
                root_operations,
                callees,
                ready,
                places,
                id,
                hirsel_run_schema::CallOutcome::NotRun(UNREACHABLE),
            )?;
            continue;
        };
        let id = places.next;
        places.next += 1;
        places
            .owed
            .insert(id, (arrival.place, index, arrival.token));
        callees[head - 1]
            .as_mut()
            .ok_or_else(|| invalid("a place's head outlived its run"))?
            .operations
            .deliver_arrival(id, arrival.call)?;
        if !ready.contains(&head) {
            ready.push_back(head);
        }
    }
    for (id, outcome) in ends {
        end_arrival(
            sources,
            runtime,
            root,
            &mut *root_operations,
            callees,
            ready,
            places,
            id,
            outcome,
        )?;
    }
    Ok(())
}

/// Deliver arrival `id`'s end to the run that sent it, if that run is live.
#[allow(clippy::too_many_arguments)]
fn end_arrival(
    sources: &SourceMap,
    runtime: &aggregate::RuntimeTypes,
    root: &mut Run,
    root_operations: &mut dyn Operations,
    callees: &mut [Option<Callee>],
    ready: &mut VecDeque<usize>,
    places: &mut Places,
    id: u64,
    outcome: hirsel_run_schema::CallOutcome,
) -> Result<(), Error> {
    let Some((_, caller, token)) = places.owed.remove(&id) else {
        return Err(invalid("an end for an arrival owed none"));
    };
    match caller {
        0 => root_operations.arrival_ended(token, outcome, &mut root.context(sources, runtime))?,
        i => match callees[i - 1].as_mut() {
            Some(callee) => callee.operations.arrival_ended(
                token,
                outcome,
                &mut callee.run.context(sources, runtime),
            )?,
            // Its caller ended first: no one waits for the end.
            None => return Ok(()),
        },
    }
    if !ready.contains(&caller) {
        ready.push_back(caller);
    }
    Ok(())
}
