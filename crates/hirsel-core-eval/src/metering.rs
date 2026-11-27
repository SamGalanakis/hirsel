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
    pub(super) fn take_work(
        &mut self,
        t: &mut Task,
        work: hirsel_core_ids::work_steps::Work,
        units: &[u64],
    ) -> X<()> {
        let before = self.steps_left(t).unwrap_or(self.steps);
        self.take_steps(t, work.steps_sum(units))?;
        if hirsel_core_ids::work_steps::Work::check_ceiling() {
            let charged = match self.steps_left(t) {
                Some(left) => before - left,
                None => self.steps - before,
            };
            work.assert_ceiling(units, charged);
        }
        Ok(())
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
