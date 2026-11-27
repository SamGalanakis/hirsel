//! MEM-30 accounting and the fixture entry points that observe it.
use super::*;

/// Run an entry with the root task's startup charge and teardown, as the native
/// process entry does. The limit must cover startup. May record CONC-10 events.
/// A cause's site word is `sites[item] + start` (`SourceMap::item_sites` over the
/// module's items); an empty `sites` means the module's own (`source::module_sites`).
/// With `error_text`, a returned `Error(e)` is rendered as PROG-3 says.
#[allow(clippy::too_many_arguments)]
pub fn run_entry(
    m: &Module,
    f: FuncId,
    args: &[u64],
    limit: u64,
    nans: Nans,
    destination: Option<&mut [u8]>,
    instructions: u64,
    revisions: &Revisions,
    events: bool,
    sites: &[u64],
    launch: Option<&hirsel_core_ids::launch::Metadata>,
    error_text: Option<ErrorText<'_>>,
) -> (Report, Checked) {
    run_entry_with_meter(
        m,
        f,
        args,
        limit,
        nans,
        destination,
        instructions,
        revisions,
        events,
        sites,
        launch,
        error_text,
        None,
    )
}

/// Run an entry with the contract differential's metering observation.
#[allow(clippy::too_many_arguments)]
pub fn run_entry_with_meter(
    m: &Module,
    f: FuncId,
    args: &[u64],
    limit: u64,
    nans: Nans,
    destination: Option<&mut [u8]>,
    instructions: u64,
    revisions: &Revisions,
    events: bool,
    sites: &[u64],
    launch: Option<&hirsel_core_ids::launch::Metadata>,
    error_text: Option<ErrorText<'_>>,
    meter: Option<StepMeter>,
) -> (Report, Checked) {
    run_world(
        m,
        f,
        args,
        limit,
        WorldInput {
            core: Core {
                nans,
                instructions,
                sites,
                meter,
                map_hash_key: meter.map(|_| [0; 16]),
                ..Core::default()
            },
            destination,
            revisions: Some(revisions),
            events,
            process_entry: true,
            launch,
            error_text,
            ..WorldInput::default()
        },
    )
}

/// Observe a contract's outer step bound with a fixed map seed. A supplied
/// allowance replaces the outer bound for one run of the same program.
#[derive(Clone, Copy)]
pub struct StepMeter {
    pub allowance: Option<u64>,
}
/// A run whose `Args` table holds `argv`.
pub fn run_args(m: &Module, f: FuncId, args: &[u64], limit: u64, argv: &[&str]) -> Report {
    run_args_with_meter(m, f, args, limit, argv, None)
}

/// Run a fixture with explicit arguments and an optional step observation.
pub fn run_args_with_meter(
    m: &Module,
    f: FuncId,
    args: &[u64],
    limit: u64,
    argv: &[&str],
    meter: Option<StepMeter>,
) -> Report {
    run_world(
        m,
        f,
        args,
        limit,
        WorldInput {
            core: Core {
                meter,
                map_hash_key: meter.map(|_| [0; 16]),
                ..Core::default()
            },
            argv,
            ..WorldInput::default()
        },
    )
    .0
}
impl<'m, H: Host<'m>> Machine<'m, H> {
    pub(super) fn take_steps(&mut self, t: &mut Task, mut count: u64) -> X<()> {
        while count != 0 {
            let Some(bound) = t.step_bounds.last_mut() else {
                self.steps = self.steps.saturating_add(count);
                break;
            };
            if bound.left == 0 {
                let id = bound.id;
                pure::root_steps_out(id);
                let site = self.site(t);
                if self.observe_steps && self.step_refusal.is_none() {
                    self.step_refusal = site;
                }
                self.record(
                    t,
                    Cause {
                        kind: "OutOfTime",
                        message: [id, 0, 0],
                        site,
                    },
                );
                return Err(Stop::Chained);
            }
            let amount = count.min(bound.left).min(65_536 - t.slice_steps);
            bound.left -= amount;
            count -= amount;
            t.slice_steps += amount;
            self.steps = self.steps.saturating_add(amount);
            if t.slice_steps == 65_536 {
                t.slice_steps = 0;
                H::slice_end(self);
            }
        }
        Ok(())
    }

    pub(super) fn step_bound(&mut self, left: u64) -> StepBound {
        let id = self.next_step_bound;
        self.next_step_bound += 1;
        StepBound { id, left }
    }

    pub(super) fn take_work(
        &mut self,
        t: &mut Task,
        work: hirsel_core_ids::work_steps::Work,
        units: &[u64],
    ) -> X<()> {
        let before = t.step_bounds.last().map_or(self.steps, |bound| bound.left);
        self.take_steps(t, work.steps_sum(units))?;
        if hirsel_core_ids::work_steps::Work::check_ceiling() {
            let charged = t
                .step_bounds
                .last()
                .map_or_else(|| self.steps - before, |bound| before - bound.left);
            work.assert_ceiling(units, charged);
        }
        Ok(())
    }

    pub(super) fn step_leave(t: &mut Task) -> StepBound {
        let bound = t.step_bounds.pop().expect("an open step scope");
        if let Some(parent) = t.step_bounds.last_mut() {
            parent.left += bound.left;
        }
        bound
    }
}

impl Default for Report {
    fn default() -> Self {
        Self {
            outcome: Outcome::Returned(Vec::new()),
            steps: 0,
            step_floor: 0,
            step_refusal: None,
            used: 0,
            peak_reserved: 0,
            trace: Vec::new(),
            admissions: 0,
            stdout: Vec::new(),
            stderr: Vec::new(),
            sites: Vec::new(),
            events: Vec::new(),
        }
    }
}
