use crate::eval::{Scheduler, Step, enter_checked, execute_frames};
use crate::frame::Frame;
use crate::*;
use hirsel_tir::TirProvider;

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
    fn waits(&self) -> Vec<BlockedWait> {
        self.state
            .as_ref()
            .map(Scheduler::waits)
            .unwrap_or_default()
    }
}

/// Execute the evaluator until its task scheduler finishes or no provider can progress.
pub(crate) fn execute<O: crate::ownership_trace::Observer>(
    provider: &mut (impl TirProvider + ?Sized),
    sources: &SourceMap,
    limits: Limits,
    operations: &mut dyn Operations,
    runtime: &aggregate::RuntimeTypes,
    mut root: Run,
    observer: &mut O,
) -> (Run, Result<(), Error>) {
    observer.run(0);
    let result = loop {
        match root.step(provider, sources, limits, operations, runtime, observer) {
            Ok(Step::Finished) => break Ok(()),
            Ok(Step::Idle) => match operations
                .ready_now()
                .and_then(|ready| if ready { Ok(true) } else { operations.wait() })
            {
                Ok(true) => (),
                Ok(false) => break Err(Error::Blocked(root.waits())),
                Err(error) => break Err(error),
            },
            Err(error) => break Err(error),
        }
    };
    (root, result)
}
