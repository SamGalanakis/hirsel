//! P1 to P6: one Ready program from a validated snapshot. Production validates
//! the emitted unit at Ready; harnesses also validate each intermediate stage. A
//! rejected unit, a pass error and an unavailable feature are all
//! `NativeError`s that name the stage and the definition.
use hirsel_core_contract::Manifest;
use hirsel_intern::timings::{self, Count, Phase};
use hirsel_lir::discharge::Discharge;
use hirsel_lir::ir::LirUnit;
use hirsel_lir::layout_types::{LayoutId, TargetSpec};
use hirsel_lir::stage::{InputIdentity, Ready, Stage, StageOutput};
use hirsel_lir::validate::{Inputs, Rejected};
use hirsel_rc::cleanup::Finalizers;
use hirsel_rc::insert::Proofs;
use hirsel_syntax::{ContentHash, DefPath};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// When the independent LIR validator runs. Ready is mandatory in both modes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Validation {
    /// Production: validate each emitted unit once, after all lowering.
    Ready,
    /// Harnesses: also validate every intermediate stage to localize pass faults.
    EveryStage,
}

/// Compiler choices and explicit launch inputs. Compiler choices and the hash
/// seed preserve observations; capability seeds select their own inputs.
#[derive(Clone, Debug)]
pub struct Options {
    /// Production judges Ready once; harnesses retain the stage ladder.
    pub validation: Validation,
    /// Explicit launch inputs; placement and capability seeds are independent.
    pub launch: hirsel_run_schema::Inputs,
    pub target: TargetSpec,
    /// Place compiler-local allocations in internal regions.
    pub regions: bool,
    /// Cancel a copy against the release insertion put after it.
    pub elide: bool,
    /// Drop specialization, reuse tokens and kept stores.
    pub reuse: bool,
    /// Walk variable flat sequences linearly instead of rescanning prefixes.
    pub flat_walk: bool,
    /// Explicit capture specialization; production enables it for release builds.
    pub scoped_specialization: bool,
    /// PS-13 checks accepted evidence licenses removing (`--proofs`).
    pub discharge: Discharge,
    /// The `db@1` place heads the build serves (STORE-12): each head's runner main is
    /// rooted, and both are in the Entry table.
    pub places: Vec<DefPath>,
}

impl Options {
    /// The harness configuration: every optimization and every stage validator on.
    pub fn new(target: TargetSpec) -> Self {
        Self {
            validation: Validation::EveryStage,
            launch: Default::default(),
            target,
            regions: true,
            elide: true,
            reuse: true,
            flat_walk: true,
            scoped_specialization: true,
            discharge: Discharge::default(),
            places: Vec::new(),
        }
    }
}

/// The native stage that stopped a build.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum StageName {
    /// P0: the snapshot's operation manifest.
    Manifest,
    /// P1/P2: instantiation, self-tail normalization, layout and SSA import.
    Import,
    /// P3.
    Placement,
    /// P4.
    Perceus,
    /// P5.
    Reuse,
    /// P6: cleanup lowering and legalization.
    Ready,
    /// The checked heap executor.
    Heap,
    /// P7.
    Emission,
    /// P8.
    Link,
    Launch,
}

impl StageName {
    pub fn name(self) -> &'static str {
        match self {
            Self::Manifest => "manifest",
            Self::Import => "import",
            Self::Placement => "placement",
            Self::Perceus => "perceus",
            Self::Reuse => "reuse",
            Self::Ready => "ready",
            Self::Heap => "heap",
            Self::Emission => "emission",
            Self::Link => "link",
            Self::Launch => "launch",
        }
    }
}

/// A native tool outcome: the stage, the definition when one unit is at
/// fault, whether the stage's validator (rather than its pass) refused, and
/// the reason.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeError {
    pub stage: StageName,
    pub definition: Option<DefPath>,
    pub validator: bool,
    pub message: String,
}

impl NativeError {
    pub fn new(stage: StageName, message: impl Into<String>) -> Self {
        Self {
            stage,
            definition: None,
            validator: false,
            message: message.into(),
        }
    }
    pub fn of(stage: StageName, definition: &DefPath, message: impl Into<String>) -> Self {
        Self {
            definition: Some(definition.clone()),
            ..Self::new(stage, message)
        }
    }
}

impl fmt::Display for NativeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "native {}", self.stage.name())?;
        if self.validator {
            f.write_str(" validator")?;
        }
        if let Some(definition) = &self.definition {
            write!(f, " in {}", definition.0.name)?;
        }
        write!(f, ": {}", self.message)
    }
}

impl std::error::Error for NativeError {}

/// A whole program at Ready: every unit sealed by the Ready validator against
/// this table, target, manifest and input.
#[derive(Clone)]
pub struct ReadyProgram {
    /// Planned provenance: source definitions (including derived forms) name their
    /// source owner; library instances name the common semantic runtime bundle.
    pub(crate) semantic_owners: BTreeMap<DefPath, Option<DefPath>>,
    /// The exact planned unit identities checked when binding the semantic input.
    pub(crate) semantic_units: Option<BTreeSet<(DefPath, ContentHash)>>,
    /// Validation policy retained for subsequent inspection and emission.
    pub validation: Validation,
    pub operations: hirsel_layout::operation::OperationRegistry,
    pub launch: hirsel_run_schema::Inputs,
    pub input: InputIdentity,
    pub target: TargetSpec,
    /// The program's layouts, sealed with their digest and the content key of each
    /// layout (`hirsel_lir::layout_keys`), which names the glue symbols units import.
    pub table: hirsel_lir::SealedLayoutTable,
    pub manifest: Manifest,
    /// Every independently validated Ready unit selected for inspection.
    pub units: Vec<StageOutput<Ready>>,
    pub ahead_environments: BTreeMap<ContentHash, Option<LayoutId>>,
    /// What program assembly reads from each unit (`hirsel_lir::fragment`), in unit order.
    pub summaries: Vec<hirsel_lir::fragment::UnitSummary>,
    /// Callable-target specializations and their known bodies, available in
    /// each calling partition without changing their source failure anchors.
    pub inline_instances: std::collections::BTreeSet<ContentHash>,
    /// The definitions some unit names as a C callback (PROG-4).
    pub callbacks: BTreeSet<DefPath>,
    /// The definitions some unit makes an Entry value of (RUN-3): each one's
    /// function-reference thunk and full path, which the program descriptor lists. A
    /// thunk may carry several paths: place heads of one signature share their runner
    /// main, one row per head.
    pub entries: BTreeSet<(String, String)>,
    /// Checked source facts, supplied through the engine's public admission API.
    pub callee_facts: Option<hirsel_rt::program::CalleeMetadata>,
    /// Per resource layout, the instance digest of its drop body's unit.
    pub finalizers: BTreeMap<LayoutId, ContentHash>,
    /// Instance digest of each non-generic function, for entry lookup.
    pub functions: BTreeMap<DefPath, ContentHash>,
    pub launches: BTreeMap<DefPath, super::link::LaunchKind>,
    pub revisions: BTreeMap<DefPath, hirsel_run_schema::ProgramRevision>,
    pub exit_reports: BTreeMap<DefPath, super::exit_report::Report>,
    /// The static data of every constant a unit reads (CT-2).
    pub statics: Vec<hirsel_lir::statics::StaticData>,
    /// The planned PS-13 sites some unit removed, as `validate_discharge`
    /// confirmed.
    pub discharged: Discharge,
    /// The externs' libraries, bound with the revisions (PROG-4).
    pub foreign_links: Vec<hirsel_engine::LinkInput>,
    /// The published data ABI, when foreign objects link (MEM-7, PROG-4).
    pub data_abi: Option<super::c_abi::DataAbi>,
    /// Executables refuse to record or replay (PROG-4).
    pub foreign_unreplayable: bool,
    /// Each extern's dispatch word (S-13, `EmitInputs::foreign`).
    pub foreign_dispatch: BTreeMap<DefPath, u64>,
    /// The launch entries that reach an escape from capability checking, which
    /// `admit.safe@1` refuses (CAP-6).
    pub(crate) escaping: BTreeSet<DefPath>,

    /// The failure table a program that isolates registers at launch (FAIL-2), bound with
    /// the revisions.
    pub(crate) failure_table: Option<super::observe::FailureTable>,
}

impl ReadyProgram {
    pub fn semantic_owners(&self) -> &BTreeMap<DefPath, Option<DefPath>> {
        &self.semantic_owners
    }
    /// The checked heap program with independently planned invocation evidence,
    /// including each derived definition's original source owner.
    pub fn heap_program(&self) -> hirsel_heapcheck::Program<hirsel_lir::stage::Ready> {
        // Reference identities come from validated TIR and the independent planner's
        // derived-definition ownership, never from candidate units or their events.
        let mut invocations = self.operations.invocations.clone();
        for (definition, owner) in &self.semantic_owners {
            if let Some(reference) = owner
                .as_ref()
                .and_then(|owner| self.operations.invocations.get(owner))
            {
                invocations.insert(definition.clone(), reference.clone());
            }
        }
        hirsel_heapcheck::Program {
            invocation_bodies: self.operations.invocation_bodies.clone(),
            pretransfer_adapters: self.operations.pretransfer_adapters.clone(),
            invocations,
            target: self.target.clone(),
            layouts: self.table.table().clone(),
            units: self.raw_units(),
            statics: self.statics.clone(),
        }
    }

    /// Recheck the linked selection against the identities sealed by semantic binding.
    pub fn check_semantic_units(
        &self,
        objects: &[hirsel_lir::stage::ObjectArtifact],
    ) -> Result<(), NativeError> {
        let Some(covered) = &self.semantic_units else {
            return Err(NativeError::new(
                StageName::Link,
                "native units have no semantic binding",
            ));
        };
        for unit in &self.summaries {
            if !covered.contains(&(unit.definition.clone(), unit.instance)) {
                return Err(NativeError::of(
                    StageName::Link,
                    &unit.definition,
                    "linked unit outside the semantic closure covered by the run identity",
                ));
            }
        }
        let definitions: BTreeSet<_> = covered.iter().map(|(path, _)| path).collect();
        for export in objects.iter().flat_map(|object| &object.exports) {
            if export.kind == hirsel_lir::stage::ObjectSymbolKind::Function
                && export
                    .definition
                    .as_ref()
                    .is_none_or(|path| !definitions.contains(path))
            {
                return Err(NativeError::new(
                    StageName::Link,
                    format!(
                        "linked symbol `{}` outside the semantic closure covered by the run identity",
                        export.name
                    ),
                ));
            }
        }
        Ok(())
    }

    /// Whether the program builds tracked arena blocks (MEM-6): it opens a bounded arena,
    /// which only `arena.bounded`'s recovery does.
    pub fn tracking(&self) -> bool {
        self.summaries.iter().any(|s| s.recovers)
    }

    /// The semantic program revision a run of `entry` binds to.
    pub fn revision(&self, entry: &DefPath) -> Option<&hirsel_run_schema::ProgramRevision> {
        self.revisions.get(entry)
    }
    pub fn bind_source(
        &mut self,
        input: &super::root_source::LinkSource<'_>,
    ) -> Result<(), NativeError> {
        if self.input != input.input {
            return Err(NativeError::new(
                StageName::Launch,
                "semantic binding belongs to another input",
            ));
        }
        for unit in &self.summaries {
            let covered = match self.semantic_owners.get(&unit.definition) {
                Some(Some(owner)) => input.semantic.semantic_contains(owner),
                Some(None) => unit.origin == hirsel_lir::ir::UnitOrigin::Library,
                None => false,
            };
            if !covered {
                return Err(NativeError::of(
                    StageName::Link,
                    &unit.definition,
                    "native unit outside the semantic closure covered by the run identity",
                ));
            }
        }
        self.semantic_units = Some(
            self.summaries
                .iter()
                .map(|unit| (unit.definition.clone(), unit.instance))
                .collect(),
        );
        self.revisions = self
            .launches
            .keys()
            .map(|entry| {
                input
                    .semantic
                    .semantic_program_revision(entry)
                    .map(|revision| (entry.clone(), revision))
                    .map_err(|error| {
                        NativeError::new(StageName::Launch, format!("semantic revision: {error:?}"))
                    })
            })
            .collect::<Result<_, _>>()?;
        self.foreign_links = input.foreign_links.clone();
        if !self.foreign_links.is_empty() {
            self.data_abi = Some(super::c_abi::data_abi(
                input.types,
                input.signatures,
                input
                    .representations
                    .iter()
                    .map(|(path, record)| (path, record.as_ref())),
                &self.target,
            )?);
        }
        self.foreign_unreplayable = input.foreign_unreplayable;
        self.foreign_dispatch = super::foreign::dispatch(input);
        super::foreign::check_callbacks(self, input)?;
        self.escaping = self
            .launches
            .keys()
            .filter(|entry| input.semantic.unsafe_reach(entry).is_some())
            .cloned()
            .collect();
        self.exit_reports = self
            .launches
            .keys()
            .filter(|entry| self.functions.contains_key(*entry))
            .filter(|entry| {
                !matches!(
                    self.launches.get(*entry),
                    Some(super::link::LaunchKind::Call { .. })
                )
            })
            .map(|entry| {
                super::exit_report::derive(input, self, entry).map(|report| (entry.clone(), report))
            })
            .collect::<Result<_, _>>()?;
        self.failure_table = Some({
            super::observe::failure_table(
                self,
                &super::observe::Sites::from_anchors(input.sources, input.anchors.clone()),
            )
        });
        Ok(())
    }
    /// Bind source-checked runner facts to the same native Entry table.
    pub fn bind_callees(
        &mut self,
        metadata: hirsel_rt::program::CalleeMetadata,
    ) -> Result<(), NativeError> {
        for path in metadata.facts.keys() {
            if !self.entries.iter().any(|(_, entry)| entry == path) {
                return Err(NativeError::new(
                    StageName::Launch,
                    format!("callee facts outside native Entry table: {path}"),
                ));
            }
        }
        self.callee_facts = Some(metadata);
        Ok(())
    }

    /// Put `unit` at `index` in place of the unit there, with its summary.
    pub fn replace_unit(&mut self, index: usize, unit: StageOutput<Ready>) -> Result<(), String> {
        self.summaries[index] = hirsel_lir::fragment::UnitSummary::of(
            unit.unit(),
            &self.table,
            self.table.keys(),
            &self.manifest,
        )?;
        self.summaries[index].add_static_references(&self.statics);
        self.units[index] = unit;
        Ok(())
    }
    /// Seal every unit again at Ready against `manifest` in place of the
    /// program's own: the inspection hook through which a corruption test
    /// shows the Ready validator refusing what a mutated manifest no longer
    /// admits, with the error a build would report.
    pub fn reseal(&self, manifest: &Manifest) -> Result<(), NativeError> {
        let inputs = Inputs {
            target: &self.target,
            layouts: &self.table,
            manifest,
            operations: &self.operations,
            discharge: Some(&self.discharged),
            memo: Default::default(),
        };
        for unit in &self.units {
            seal(StageName::Ready, || {
                hirsel_lir::validate::validate_ready(unit.unit().clone(), &inputs)
            })?;
        }
        Ok(())
    }
    /// The Ready units, as the heap executor and dumps read them.
    pub fn raw_units(&self) -> Vec<LirUnit<Ready>> {
        self.units.iter().map(|u| u.unit().clone()).collect()
    }
}

/// Run `work` as `phase`.
pub fn timed<T>(phase: Phase, work: impl FnOnce() -> T) -> T {
    let _timing = timings::phase(phase);
    work()
}

/// A unit sealed at its stage by lir's non-generic per-stage validator, or
/// every violation it reported.
pub fn seal<S: Stage>(
    stage: StageName,
    validate: impl FnOnce() -> Result<StageOutput<S>, Box<Rejected<S>>>,
) -> Result<StageOutput<S>, NativeError> {
    timed(Phase::LirValidate, validate).map_err(|rejected| {
        let mut kinds: Vec<String> = rejected
            .violations
            .iter()
            .map(|v| match &v.kind {
                // Name the operation: its table index means nothing to a reader.
                hirsel_lir::validate::ViolationKind::UnsupportedOperation(op) => {
                    let path = rejected
                        .unit
                        .tables
                        .operations
                        .get(op.index())
                        .map(|id| &id.definition.0);
                    let name = path.map_or(String::new(), |p| {
                        p.modules
                            .iter()
                            .chain(std::iter::once(&p.name))
                            .map(String::as_str)
                            .collect::<Vec<_>>()
                            .join(".")
                    });
                    format!("UnsupportedOperation: no native entry for operation {name:?}")
                }
                kind => format!("{kind:?} at {:?}", v.site),
            })
            .collect();
        kinds.sort();
        kinds.dedup();
        kinds.truncate(8);
        NativeError {
            validator: true,
            ..NativeError::of(stage, &rejected.unit.header.definition, kinds.join("; "))
        }
    })
}

/// An intermediate stage stays raw in production. Only the Ready validator
/// manufactures the seal emission requires; harnesses retain the full ladder.
pub fn checkpoint<S: Stage>(
    stage: StageName,
    unit: LirUnit<S>,
    inputs: &Inputs<'_>,
    context: &hirsel_lir::validate::ValidationContext<'_>,
    validation: Validation,
) -> Result<LirUnit<S>, NativeError> {
    match validation {
        Validation::Ready => Ok(unit),
        Validation::EveryStage => {
            seal(stage, || context.validate(unit, inputs)).map(StageOutput::into_raw)
        }
    }
}

/// Lower one imported unit through placement, RC, reuse and Ready. Production
/// validates Ready once; harnesses use the identical passes with stage checkpoints.
#[cfg(test)]
pub fn lower_unit(
    unit: LirUnit<hirsel_lir::stage::Value>,
    id: hirsel_rc::instance::InstanceId,
    options: &Options,
    instances: &hirsel_rc::instance::Instances,
    finalizers: &Finalizers,
    inputs: &Inputs<'_>,
    validation: &hirsel_lir::validate::ValidationContext<'_>,
) -> Result<StageOutput<Ready>, NativeError> {
    let facts = LocalRcFacts {
        placement: hirsel_rc::placement::Options::for_instance(options.regions, instances, id),
        proofs: Proofs::for_instance(instances, id),
        guard: None,
    };
    lower_local_unit(unit, options, &facts, finalizers, inputs, validation)
}

/// The complete local source facts the existing RC passes consume. A native
/// query records the independently accepted callable/placement inputs that
/// established them, instead of retaining every instance's source body.
#[derive(Clone, Debug)]
pub struct LocalRcFacts {
    pub placement: hirsel_rc::placement::Options,
    pub proofs: Proofs,
    pub guard: Option<hirsel_rc::seal_demand::UnitDemand>,
}

impl LocalRcFacts {
    pub fn canonical(&self) -> Vec<u8> {
        let mut bytes = vec![
            u8::from(self.placement.regions),
            u8::from(self.placement.noalloc),
            u8::from(self.proofs.noalloc),
        ];
        bytes.extend_from_slice(&(self.proofs.unique.len() as u64).to_le_bytes());
        for parameter in &self.proofs.unique {
            bytes.extend_from_slice(&parameter.to_le_bytes());
        }
        bytes
    }
}

/// One local miss transformation, with a reached input table and finalizers.
/// No other unit's Value or RC result is an implicit input. Ready validation
/// remains independent of every placement/RC/Reuse producer fact.
pub fn lower_local_unit(
    unit: LirUnit<hirsel_lir::stage::Value>,
    options: &Options,
    facts: &LocalRcFacts,
    finalizers: &Finalizers,
    inputs: &Inputs<'_>,
    validation: &hirsel_lir::validate::ValidationContext<'_>,
) -> Result<StageOutput<Ready>, NativeError> {
    let raw = lower_local_raw(unit, options, facts, finalizers, inputs, validation)?;
    let ready = seal(StageName::Ready, || validation.validate(raw, inputs))?;
    super::emit::check_unit_runtime_calls(ready.unit(), inputs.layouts, inputs.manifest)?;
    Ok(ready)
}

/// The sole local placement/RC/Reuse/Ready transformation. The enclosing query
/// performs independent final C and publication at its own completion boundary.
/// Harnesses retain the same intermediate stage checkpoints.
pub fn lower_local_raw(
    unit: LirUnit<hirsel_lir::stage::Value>,
    options: &Options,
    facts: &LocalRcFacts,
    finalizers: &Finalizers,
    inputs: &Inputs<'_>,
    validation: &hirsel_lir::validate::ValidationContext<'_>,
) -> Result<LirUnit<Ready>, NativeError> {
    let table = inputs.layouts;
    let definition = unit.header.definition.clone();
    let fail = |stage, message: String| NativeError::of(stage, &definition, message);
    timings::count(Count::Units);
    let value = checkpoint(
        StageName::Import,
        unit,
        inputs,
        validation,
        options.validation,
    )?;
    let mut placed = timed(Phase::Place, || {
        hirsel_rc::placement::place(value, table, &facts.placement)
    })
    .map_err(|e| fail(StageName::Placement, e.to_string()))?;
    if let Some(guard) = &facts.guard {
        guard
            .apply(&mut placed)
            .map_err(|e| fail(StageName::Placement, e))?;
    }
    let placed = checkpoint(
        StageName::Placement,
        placed,
        inputs,
        validation,
        options.validation,
    )?;
    let mut unit = timed(Phase::Rc, || {
        hirsel_rc::insert::insert(placed, table, &facts.proofs)
    })
    .map_err(|e| fail(StageName::Perceus, e.to_string()))?;
    if options.elide {
        unit = timed(Phase::Rc, || hirsel_rc::elide::elide(unit, table))
            .map_err(|e| fail(StageName::Perceus, e.to_string()))?;
    }
    let rc = checkpoint(
        StageName::Perceus,
        unit,
        inputs,
        validation,
        options.validation,
    )?;
    let reuse = hirsel_rc::reuse::Options {
        reuse: options.reuse,
    };
    let reused = timed(Phase::Reuse, || {
        hirsel_rc::reuse::reuse_indexed(rc, &reuse, validation.index())
    })
    .map_err(|e| fail(StageName::Reuse, e.to_string()))?;
    let reused = checkpoint(
        StageName::Reuse,
        reused,
        inputs,
        validation,
        options.validation,
    )?;
    let ready = timed(Phase::Ready, || {
        hirsel_rc::ready::ready_indexed(reused, finalizers, validation.index())
    })
    .map_err(|e| fail(StageName::Ready, e.to_string()))?;
    Ok(ready)
}

/// The definitions `unit` names as C callbacks: `FunctionRef`s at a pointer scalar (PROG-4).
pub fn callbacks<S>(unit: &hirsel_lir::ir::LirUnit<S>) -> impl Iterator<Item = DefPath> + '_ {
    use hirsel_lir::ir::{Inst, ValueRepr};
    unit.instructions.iter().filter_map(|inst| match *inst {
        Inst::FunctionRef { result, symbol, .. }
            if unit.types[unit.values[result.index()].ty.index()].repr
                == ValueRepr::Scalar(hirsel_lir::layout_types::ScalarKind::Pointer) =>
        {
            Some(unit.tables.symbols[symbol.index()].definition.clone())
        }
        _ => None,
    })
}

/// Program entries retained in portable unit summaries, including object-store hits.
pub fn summary_entries(
    summaries: &[hirsel_lir::fragment::UnitSummary],
) -> BTreeSet<(String, String)> {
    summaries
        .iter()
        .flat_map(|s| s.glue.entries.iter())
        .map(|(path, instance)| {
            (
                hirsel_lir::emission::function_thunk_name(path, instance.as_ref()),
                path.to_string(),
            )
        })
        .collect()
}
