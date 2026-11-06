//! The one scheduled source-graph installer. Every source Check, the CLI
//! request path and the engine's own execution, dependency-card and recipe
//! checks alike, admits its loading charge, plans its partitions, accepts
//! generated and staged declarations, and wires the immutable Front, Lower,
//! Growth, storage and Check producers here. A host adds only the roots its
//! operation selects on top of the installed source graph.
use crate::{
    CheckOutput, FRONT_BODY_KIND, FRONT_CHECK_KIND, FRONT_EVALUATE_KIND, FRONT_GROWTH_KIND,
    FRONT_LOWER_KIND, FRONT_STORAGE_GRAPH_KIND, FrontBodyQueries, FrontCarry, FrontCheckPlan,
    FrontCheckQueries, FrontDemandAdmission, FrontDemandError, FrontDemandScope,
    FrontGrowthQueries, FrontInputFootprint, FrontLowerQueries, FrontNativeDefinitionInputs,
    FrontRegistrationBootstrapError, FrontRunInputs, FrontStorageGraphQueries, FrontStorageQueries,
    LowerCarry, QueryEngine, QueryError, ToolError, UnsafeFfiPackage,
};
use hirsel_engine_runtime::*;
use hirsel_engine_store::{Domain, NoneStore, RequestStore, ResultStore};
use hirsel_intern::jobs::Jobs;
use hirsel_query_products::LoweringPolicy;
use hirsel_span::{FileId, SourceMap};
use hirsel_syntax::{DefPath, FileContext, PackageId};
use std::sync::{Arc, OnceLock};

/// Process bounds of one request graph: its executor, its immutable snapshot
/// partitions and its store transport.
#[derive(Clone, Copy, Debug)]
pub struct RequestLimits {
    pub runtime_bytes: u64,
    pub snapshot_bytes: u64,
    pub transport_bytes: u64,
}

// Every host in a process shares one allowance, including different
// sessions and the engine's own checks. Capturing the guard policy once
// prevents each source revision from creating a new full-cap budget.
static HOST: OnceLock<(Arc<hirsel_engine_admission::ProcessBudget>, u64)> = OnceLock::new();
pub fn host_budget() -> (&'static Arc<hirsel_engine_admission::ProcessBudget>, u64) {
    let (budget, cap) = HOST.get_or_init(|| {
        let cap = std::env::var("HIRSEL_MEMORY_LIMIT")
            .ok()
            .and_then(|text| hirsel_guard::parse_limit(text.as_bytes()))
            .unwrap_or(hirsel_guard::DEFAULT_LIMIT);
        // Keep a quarter for the host, source loader and native libraries.
        (
            hirsel_engine_admission::ProcessBudget::new(cap.saturating_sub(cap / 4)),
            cap,
        )
    });
    (budget, *cap)
}
pub fn host_limits(cap: u64, jobs: usize) -> RequestLimits {
    RequestLimits {
        runtime_bytes: (jobs as u64)
            .saturating_mul(hirsel_intern::jobs::COMPILER_STACK_BYTES as u64)
            .saturating_add(cap / 16),
        snapshot_bytes: cap / 8,
        transport_bytes: cap / 32,
    }
}

/// Bounds supplied by the actual source/producer footprint planner. These are
/// parts of the existing snapshot cap, not extra per-producer process caps.
#[derive(Clone, Copy)]
pub struct SnapshotPlan {
    pub source: u64,
    pub front: u64,
    pub lower: u64,
    pub check: u64,
    pub growth: u64,
    pub run: u64,
}
pub struct GraphAdmission {
    pub runtime: hirsel_engine_admission::MemoryReservation,
    pub source: hirsel_engine_admission::MemoryReservation,
    pub front: hirsel_engine_admission::MemoryReservation,
    pub lower: hirsel_engine_admission::MemoryReservation,
    pub check: hirsel_engine_admission::MemoryReservation,
    pub growth: hirsel_engine_admission::MemoryReservation,
    pub run: hirsel_engine_admission::MemoryReservation,
    pub transport: hirsel_engine_admission::MemoryReservation,
    pub native_transport: hirsel_engine_admission::MemoryReservation,
}
impl GraphAdmission {
    pub fn reserve(
        budget: &Arc<hirsel_engine_admission::ProcessBudget>,
        limits: RequestLimits,
        plan: SnapshotPlan,
    ) -> Result<Self, ToolError> {
        Self::reserve_with(limits, plan, |bytes| budget.reserve(bytes))
    }
    /// Let the host reclaim idle owners after every partial grant has dropped.
    pub fn reserve_once(
        budget: &Arc<hirsel_engine_admission::ProcessBudget>,
        limits: RequestLimits,
        plan: SnapshotPlan,
    ) -> Result<Self, ToolError> {
        Self::reserve_with(limits, plan, |bytes| budget.reserve_once(bytes))
    }
    fn reserve_with(
        limits: RequestLimits,
        plan: SnapshotPlan,
        reserve: impl Fn(u64) -> std::io::Result<hirsel_engine_admission::MemoryReservation>,
    ) -> Result<Self, ToolError> {
        let snapshot = plan
            .source
            .checked_add(plan.front)
            .and_then(|bytes| bytes.checked_add(plan.lower))
            .and_then(|bytes| bytes.checked_add(plan.check))
            .and_then(|bytes| bytes.checked_add(plan.growth))
            .and_then(|bytes| bytes.checked_add(plan.run));
        if snapshot.is_none_or(|bytes| bytes > limits.snapshot_bytes) {
            return Err(ToolError::Io(std::io::Error::other(
                "compiler snapshot footprint exceeds admission",
            )));
        }
        // A failed later reservation drops every earlier temporary. Nothing
        // becomes visible to request clients before this whole value exists.
        Ok(Self {
            runtime: reserve(limits.runtime_bytes).map_err(ToolError::Io)?,
            source: reserve(plan.source).map_err(ToolError::Io)?,
            front: reserve(plan.front).map_err(ToolError::Io)?,
            lower: reserve(plan.lower).map_err(ToolError::Io)?,
            check: reserve(plan.check).map_err(ToolError::Io)?,
            growth: reserve(plan.growth).map_err(ToolError::Io)?,
            run: reserve(plan.run).map_err(ToolError::Io)?,
            transport: reserve(limits.transport_bytes).map_err(ToolError::Io)?,
            native_transport: reserve(0).map_err(ToolError::Io)?,
        })
    }
}

/// Charge the whole selected snapshot partition before S0 allocates a loader
/// view. Partitioning transfers that existing charge to immutable producers;
/// source maps returned on a loader failure keep their own charge as well.
pub struct LoadingAdmission {
    runtime: hirsel_engine_admission::MemoryReservation,
    snapshot: hirsel_engine_admission::MemoryReservation,
    transport: hirsel_engine_admission::MemoryReservation,
    native_transport: hirsel_engine_admission::MemoryReservation,
}
impl LoadingAdmission {
    pub fn reserve(
        budget: &Arc<hirsel_engine_admission::ProcessBudget>,
        limits: RequestLimits,
        native_transport: u64,
    ) -> Result<Self, ToolError> {
        Self::reserve_with(limits, native_transport, |bytes| budget.reserve(bytes))
    }
    /// Let the host reclaim idle owners after every partial grant has dropped.
    pub fn reserve_once(
        budget: &Arc<hirsel_engine_admission::ProcessBudget>,
        limits: RequestLimits,
        native_transport: u64,
    ) -> Result<Self, ToolError> {
        Self::reserve_with(limits, native_transport, |bytes| budget.reserve_once(bytes))
    }
    fn reserve_with(
        limits: RequestLimits,
        native_transport: u64,
        reserve: impl Fn(u64) -> std::io::Result<hirsel_engine_admission::MemoryReservation>,
    ) -> Result<Self, ToolError> {
        Ok(Self {
            runtime: reserve(limits.runtime_bytes).map_err(ToolError::Io)?,
            snapshot: reserve(limits.snapshot_bytes).map_err(ToolError::Io)?,
            transport: reserve(limits.transport_bytes).map_err(ToolError::Io)?,
            native_transport: reserve(native_transport).map_err(ToolError::Io)?,
        })
    }
    /// Transfer the already admitted transport before S0 uses the provider.
    /// The remaining zero-sized reservation keeps finish's ownership shape.
    pub fn take_transport(&mut self) -> hirsel_engine_admission::MemoryReservation {
        self.transport.split(self.transport.bytes()).unwrap()
    }
    pub fn lend_runtime(&mut self) -> hirsel_engine_admission::MemoryReservation {
        self.runtime.split(self.runtime.bytes()).unwrap()
    }
    pub fn return_runtime(&mut self, reservation: hirsel_engine_admission::MemoryReservation) {
        self.runtime
            .join(reservation)
            .ok()
            .expect("same runtime partition");
    }
    /// Lend the whole runtime and snapshot charge to work that finishes and
    /// releases it before any final producer takes a partition.
    pub fn lend(
        &mut self,
    ) -> (
        hirsel_engine_admission::MemoryReservation,
        hirsel_engine_admission::MemoryReservation,
    ) {
        (
            self.runtime.split(self.runtime.bytes()).unwrap(),
            self.snapshot.split(self.snapshot.bytes()).unwrap(),
        )
    }
    /// Adopt charges a host reserved again from this loading's own budget.
    pub fn join(
        &mut self,
        runtime: hirsel_engine_admission::MemoryReservation,
        snapshot: hirsel_engine_admission::MemoryReservation,
    ) {
        let _ = self.runtime.join(runtime);
        let _ = self.snapshot.join(snapshot);
    }
    pub fn finish(mut self, plan: SnapshotPlan) -> Result<GraphAdmission, ToolError> {
        let bytes = [
            plan.source,
            plan.front,
            plan.lower,
            plan.growth,
            plan.check,
            plan.run,
        ]
        .into_iter()
        .try_fold(0u64, u64::checked_add);
        if bytes.is_none_or(|bytes| bytes > self.snapshot.bytes()) {
            return Err(ToolError::Runtime(
                "compiler snapshot footprint exceeds admission".into(),
            ));
        }
        // The checked total proves each split fits. No reservation is released
        // while ownership transfers, even when another host is admitting work.
        let source = self.snapshot.split(plan.source).unwrap();
        let front = self.snapshot.split(plan.front).unwrap();
        let lower = self.snapshot.split(plan.lower).unwrap();
        let growth = self.snapshot.split(plan.growth).unwrap();
        let check = self.snapshot.split(plan.check).unwrap();
        let run = self.snapshot.split(plan.run).unwrap();
        Ok(GraphAdmission {
            runtime: self.runtime,
            source,
            front,
            lower,
            growth,
            check,
            run,
            transport: self.transport,
            native_transport: self.native_transport,
        })
    }
    pub fn failed(mut self, error: ToolError) -> ToolError {
        let unused = self
            .native_transport
            .split(self.native_transport.bytes())
            .unwrap();
        drop(unused);
        retain_error(error, Arc::new(self.snapshot))
    }
}

pub fn retain_error(error: ToolError, owner: Arc<dyn Send + Sync>) -> ToolError {
    match error {
        ToolError::Query {
            error,
            phase,
            sources,
        } => ToolError::Query {
            error,
            phase,
            sources: retain_sources(sources, owner),
        },
        ToolError::Unsupported {
            phase,
            construct,
            span,
            sources,
        } => ToolError::Unsupported {
            phase,
            construct,
            span,
            sources: retain_sources(sources, owner),
        },
        other => other,
    }
}
pub fn retain_sources(sources: SourceMap, owner: Arc<dyn Send + Sync>) -> SourceMap {
    sources.clone().replace_owner(Arc::new((sources, owner)))
}

/// What a host contributes to the shared installer: how it charges a loading
/// admission again, and the fixed and slack-sized footprints of the roots it
/// adds on top of the source graph. The defaults select no extra root.
pub trait SourceHost {
    fn readmit(
        &self,
        loading: &mut LoadingAdmission,
        runtime: u64,
        snapshot: u64,
    ) -> Result<(), ToolError>;
    /// The host's request inspection rows over this source inventory.
    fn inspection_bytes(&self, _definitions: &[DefPath], _memos: usize) -> Option<u64> {
        Some(0)
    }
    /// The fixed rows of the host's native or ownership roots.
    fn extension_minimum(&self, _definitions: &[DefPath], _catalogue: u64) -> Option<u64> {
        Some(0)
    }
    /// The bytes and source-root retention those roots take from `slack`.
    fn extension_bytes(
        &self,
        _definitions: &[DefPath],
        _slack: u64,
        _catalogue: u64,
    ) -> Option<(u64, u64)> {
        Some((0, 0))
    }
}

/// A host whose charge comes directly from a process budget.
pub struct BudgetHost<'a>(pub &'a Arc<hirsel_engine_admission::ProcessBudget>);
impl SourceHost for BudgetHost<'_> {
    fn readmit(
        &self,
        loading: &mut LoadingAdmission,
        runtime: u64,
        snapshot: u64,
    ) -> Result<(), ToolError> {
        let runtime = self.0.reserve(runtime).map_err(ToolError::Io)?;
        let snapshot = self.0.reserve(snapshot).map_err(ToolError::Io)?;
        loading.join(runtime, snapshot);
        Ok(())
    }
}

/// The roots an operation adds above the source graph. Each changes how the
/// one snapshot partition is shared; none adds a separate cap.
#[derive(Clone, Copy, Default)]
pub struct SourceShape {
    pub run: bool,
    /// Comptime and Reachable Check roots share the same source producers.
    pub check_variants: bool,
    pub native: bool,
    pub proofs: bool,
    pub ownership: bool,
    pub safe: bool,
    pub receipt: bool,
    pub export: bool,
    pub storage: bool,
    pub batch: bool,
    /// Native consumers keep only compact retained front reads.
    pub compact_reads: bool,
}

/// The source selection a Check covers.
pub struct SourceSelection {
    pub files: Vec<(FileId, FileContext, LoweringPolicy)>,
    pub entry: DefPath,
    pub lint_package: Option<PackageId>,
    pub unsafe_ffi: Vec<UnsafeFfiPackage>,
    /// Retained bytes of the immutable input observations.
    pub input_bytes: u64,
}

/// One selector's carries across its source revisions' fresh graphs.
pub struct SourceCarry {
    pub front: Arc<FrontCarry>,
    pub lower: Arc<LowerCarry>,
}

/// Nested Lower/Evaluate continuations admitted under the active frames.
const BODY_CONTINUATIONS: u64 = 32;

/// Retained bounds come from the caller's existing snapshot partition. These
/// are hard producer allowances: constructors and completions refuse overflow.
/// Runtime scratch is separate, and shared across the admitted worker width.
pub struct Plan {
    pub snapshots: SnapshotPlan,
    pub query_inventory: usize,
    pub query_capacity: usize,
    pub source_index_bytes: u64,
    pub body: MemoryWeight,
    pub root: MemoryWeight,
    pub run: MemoryWeight,
    /// The slack the host's native or ownership footprint was sized from.
    pub extension_slack: u64,
    pub native_root: MemoryWeight,
}
struct PlanInputs<'a> {
    limits: RequestLimits,
    jobs: Jobs,
    input_bytes: u64,
    definitions: &'a [DefPath],
    check_inventory: u64,
    shape: SourceShape,
    footprint: FrontInputFootprint,
    run_input_bytes: u64,
    semantic_queries: usize,
    catalogue_bytes: u64,
    inspection_bytes: u64,
    source_index_bytes: u64,
}
impl Plan {
    fn new(inputs: PlanInputs<'_>, host: &dyn SourceHost) -> Result<Self, ToolError> {
        let PlanInputs {
            limits,
            jobs,
            input_bytes,
            definitions,
            check_inventory,
            shape,
            footprint,
            run_input_bytes,
            semantic_queries,
            catalogue_bytes,
            inspection_bytes,
            source_index_bytes,
        } = inputs;
        let check_inventory = check_inventory
            .checked_mul(if shape.check_variants { 2 } else { 1 })
            .ok_or_else(|| uninstalled())?;
        let needs_run = shape.run;
        let needs_receipt = shape.receipt;
        let needs_ownership = shape.ownership;
        let needs_native = shape.native;
        let extended = needs_native || needs_ownership;
        let source = input_bytes
            .checked_add(footprint.source_headers)
            .and_then(|bytes| bytes.checked_add(inspection_bytes))
            .ok_or_else(|| uninstalled())?;
        let metadata = MemoryWeight::default();
        let lower_inventory =
            FrontLowerQueries::input_bytes(definitions).ok_or_else(|| uninstalled())?;
        let growth_inventory =
            FrontGrowthQueries::input_bytes(definitions, metadata).ok_or_else(|| uninstalled())?;
        let storage_inventory = if shape.storage {
            FrontStorageQueries::input_bytes_for_definitions(definitions)
                .and_then(|recipes| {
                    FrontStorageGraphQueries::input_bytes(definitions)
                        .and_then(|graph| recipes.checked_add(graph))
                })
                .ok_or_else(|| uninstalled())?
        } else {
            0
        };
        let run_inventory = if needs_run { run_input_bytes } else { 0 };
        let extension_inventory = if extended {
            host.extension_minimum(definitions, catalogue_bytes)
                .ok_or_else(|| uninstalled())?
        } else {
            0
        };
        let fixed = [
            source,
            footprint.front_inputs,
            lower_inventory,
            growth_inventory,
            storage_inventory,
            check_inventory,
            run_inventory,
            extension_inventory,
        ]
        .into_iter()
        .try_fold(0u64, u64::checked_add)
        .ok_or_else(|| uninstalled())?;
        let slack = limits.snapshot_bytes.checked_sub(fixed).ok_or_else(|| {
            ToolError::Runtime(
                format!("compiler immutable input inventory exceeds snapshot admission: fixed {fixed}/{} B; source {source}, Front {}, Lower {lower_inventory}, Growth {growth_inventory}, storage {storage_inventory}, Check {check_inventory}, Run {run_inventory}, extension {extension_inventory}", limits.snapshot_bytes, footprint.front_inputs),
            )
        })?;
        // Headers and private proofs can grow after this metadata-only plan.
        // Retain their source index within the existing optional-cache cap,
        // rather than freezing the smaller pre-demand size as its allowance.
        // A source already above the cap never consumes producer grants.
        let source_index_bytes = if source_index_bytes != 0 && source_index_bytes <= slack / 32 {
            slack / 32
        } else {
            0
        };
        let source = source
            .checked_add(source_index_bytes)
            .ok_or_else(|| uninstalled())?;
        let slack = slack - source_index_bytes;
        let extension_slack = if needs_native {
            slack / 2
        } else if needs_ownership {
            slack / 8
        } else {
            0
        };
        let slack = slack - extension_slack;
        let (extension_bytes, extension_source) = if extended {
            host.extension_bytes(definitions, extension_slack, catalogue_bytes)
                .ok_or_else(|| uninstalled())?
        } else {
            (0, 0)
        };
        // Every partition lives as long as the graph. A full-std receipt's
        // staged peak (~580MB) is 29 times the front's retained outcomes
        // and input rows (20MB), so a receipt leaves the front a sixteenth.
        let front_slack = if needs_ownership {
            slack * 83 / 96
        } else if needs_receipt {
            slack / 16
        } else {
            slack / 4
        };
        // A checked receipt is a root output after Check, like Run, and takes
        // the remainder with no Run inventory or rows. Its staged input and
        // construction hold every lowered original, while Lower itself keeps
        // only settled metadata (13MB with its rows for full std; a body's
        // grant is the smaller continuation bound), so Lower keeps a sixteenth.
        let lower_slack = if needs_ownership {
            slack / 12
        } else if needs_receipt {
            slack / 16
        } else if needs_run {
            slack / 3
        } else {
            slack / 2
        };
        // A receipt's root continuation is already bounded by Check's
        // thirty-second, and Growth's own output is its merged edges and reads
        // (2MB for full std), so Growth keeps that thirty-second too.
        let growth_slack = if needs_ownership {
            slack / 48
        } else if needs_receipt {
            slack / 32
        } else {
            slack / 8
        };
        // Check admits its fixed rows, retained data and archive separately.
        // A receipt also captures its headers within this partition.
        let check_slack = if needs_ownership {
            slack / 48
        } else if needs_receipt {
            slack / 32
        } else if needs_run {
            slack / 8
        } else {
            slack - front_slack - lower_slack - growth_slack
        };
        let run_slack = slack - front_slack - lower_slack - growth_slack - check_slack;
        let front = footprint.front_inputs + front_slack;
        let lower = lower_inventory + lower_slack;
        let growth = growth_inventory + storage_inventory + growth_slack;
        let check = check_inventory + check_slack;
        let run = extension_bytes + run_inventory + run_slack;
        // Each definition can name P, Lower, Evaluate and, when selected,
        // Run and a semantic node; native selection adds its definition rows. Growth/
        // Check, the selected SafeCheck root and the native bootstrap and
        // source roots have separate slots.
        // Reserve the same row bound here and in the runtime constructor so
        // task grants cannot spend the scheduler's bookkeeping allocation.
        // The storage root polls one retained recipe continuation itself;
        // recipe identities do not allocate independent scheduler nodes.
        // Native source factories expose one selected whole-program Run,
        // independently of all per-definition semantic producers. Reserving a
        // whole-program Run for every source would reserve thousands of roots
        // this graph cannot request, before admitting any actual compiler work.
        let per_definition = if extended {
            5
        } else {
            4 + 2 * usize::from(needs_run)
        };
        let roots = 2
            + usize::from(shape.check_variants)
            + usize::from(shape.safe)
            + usize::from(shape.export)
            + usize::from(shape.export && needs_native)
            + usize::from(shape.storage)
            + usize::from(shape.batch)
            + 3 * usize::from(extended);
        let inventory = definitions
            .len()
            .checked_mul(per_definition)
            .and_then(|entries| entries.checked_add(roots))
            .and_then(|entries| entries.checked_add(if extended { semantic_queries } else { 0 }))
            .ok_or_else(|| uninstalled())?;
        let default_capacity = AdmittedRuntime::default_query_capacity(jobs, limits.runtime_bytes)
            .ok_or_else(|| uninstalled())?;
        let query_inventory = inventory.max(default_capacity);
        let nominal_working = AdmittedRuntime::working_bytes(jobs, limits.runtime_bytes)
            .filter(|bytes| *bytes != 0)
            .ok_or_else(|| {
                ToolError::Runtime(
                    "compiler runtime cannot admit its worker stacks and query inventory".into(),
                )
            })?;
        let requested_metadata =
            AdmittedRuntime::query_capacity_bytes(query_inventory).ok_or_else(|| uninstalled())?;
        let query_capacity = if requested_metadata < nominal_working {
            query_inventory
        } else {
            usize::try_from(
                (nominal_working / 2)
                    / AdmittedRuntime::query_capacity_bytes(1).ok_or_else(|| uninstalled())?,
            )
            .map_err(|_| uninstalled())?
        };
        let bookkeeping =
            AdmittedRuntime::query_capacity_bytes(query_capacity).ok_or_else(|| uninstalled())?;
        let working = nominal_working
            .checked_sub(bookkeeping)
            .filter(|bytes| *bytes != 0)
            .ok_or_else(|| {
                ToolError::Runtime(
                    "compiler runtime cannot admit its worker stacks and query inventory".into(),
                )
            })?;
        // Every admitted worker may hold one active scratch frame. Suspended
        // continuations keep only their retained grant, so they share what
        // the active frames leave: half for the root continuations the request
        // holds (Growth, Check and any selected Run, SafeCheck or native source
        // root), half for a chain of nested Lower/Evaluate
        // bodies, such as a constant read by a constant read by a function.
        // A snapshot partition is not one task's executable memory allowance.
        let workers = jobs.get() as u64;
        let scratch = working / (workers + 1);
        let suspended = working - workers * scratch;
        // A native request adds its source root to the root continuations and
        // its bootstrap controller to the nested bodies.
        let native_frames = u64::from(extended);
        // Run accumulates the selected semantic closure, the largest retained
        // root graph, so it holds two shares; its active semantic node holds one.
        let root_shares = 2
            + 3 * u64::from(needs_run)
            + u64::from(shape.safe)
            + native_frames
            + u64::from(shape.storage);
        let root_bound = suspended / (2 * root_shares);
        let body_bound = suspended / (2 * (BODY_CONTINUATIONS + native_frames));
        // Only demanded Lower outputs consume producer retention. Potential
        // addresses carry compact input slots, not maximum future/body grants.
        let output_metadata = FrontLowerQueries::output_bytes(MemoryWeight::default())
            .ok_or_else(|| uninstalled())?;
        let retained = lower_slack
            .checked_sub(output_metadata)
            .map(|bytes| (bytes / 2).min(body_bound))
            .filter(|bytes| *bytes != 0)
            .ok_or_else(|| {
                ToolError::Runtime("compiler Lower continuation exceeds admission".into())
            })?;
        // Check retains data and its archive under separate bounds. A receipt
        // also captures headers from this same partition before installing it.
        let check_continuation = if shape.check_variants {
            check_slack / 4
        } else if needs_receipt {
            check_slack / 3
        } else {
            check_slack / 2
        };
        let root_retained = growth_slack.min(check_continuation).min(root_bound);
        if root_retained == 0 {
            return Err(ToolError::Runtime(
                "compiler root continuation exceeds admission".into(),
            ));
        }
        Ok(Self {
            source_index_bytes,
            snapshots: SnapshotPlan {
                source,
                front,
                lower,
                growth,
                check,
                run,
            },
            query_inventory,
            query_capacity,
            body: MemoryWeight {
                retained,
                scratch,
                stack: 0,
            },
            root: MemoryWeight {
                retained: root_retained,
                scratch,
                stack: 0,
            },
            native_root: MemoryWeight {
                retained: extension_source.min(root_bound),
                scratch,
                stack: 0,
            },
            extension_slack,
            run: MemoryWeight {
                // Run accumulates the selected semantic closure, so it uses
                // its two root shares rather than one Lower body.
                retained: (run_slack / 4).min(2 * root_bound),
                scratch,
                stack: 0,
            },
        })
    }
}

/// An installed source graph before its Check root is constructed. The host
/// may split its Check partition first (receipt headers, expansion text).
pub struct InstalledSource {
    pub definitions: Vec<DefPath>,
    pub plan: Plan,
    pub front: Arc<FrontBodyQueries>,
    pub lower: Arc<FrontLowerQueries>,
    pub growth: Arc<FrontGrowthQueries>,
    pub storage: Option<(Arc<FrontStorageQueries>, Arc<FrontStorageGraphQueries>)>,
    /// The Check partition, consumed by [`InstalledSource::check`].
    pub check: hirsel_engine_admission::MemoryReservation,
    pub run: hirsel_engine_admission::MemoryReservation,
    pub native_transport: hirsel_engine_admission::MemoryReservation,
    pub run_inputs: Option<FrontRunInputs>,
    /// A Run-only preparation failure does not reject an ordinary source Check.
    pub run_error: Option<QueryError>,
    pub catalogue: Option<FrontNativeDefinitionInputs>,
    pub operation: crate::OperationLease,
    /// The admitted source owner every product source view retains.
    pub source: Arc<hirsel_engine_admission::MemoryReservation>,
    pub source_index: hirsel_engine_admission::MemoryReservation,
    /// The source map before the admitted owner, for retained diagnostics.
    pub sources: SourceMap,
    pub errors: RuntimeErrors,
}

struct SourcePlanning {
    definitions: Vec<DefPath>,
    footprint: FrontInputFootprint,
    run_inputs: Option<FrontRunInputs>,
    catalogue: Option<FrontNativeDefinitionInputs>,
    run_error: Option<QueryError>,
    plan: Plan,
}
struct SourcePlanner<'a> {
    selection: &'a SourceSelection,
    host: &'a dyn SourceHost,
    jobs: Jobs,
    shape: SourceShape,
}
impl SourcePlanner<'_> {
    fn within(
        &self,
        limits: RequestLimits,
        engine: &mut QueryEngine,
        definitions: &[DefPath],
        footprint: FrontInputFootprint,
        run_inputs: &Option<FrontRunInputs>,
        catalogue: &Option<FrontNativeDefinitionInputs>,
    ) -> Result<Plan, ToolError> {
        let inspection_bytes = self
            .host
            .inspection_bytes(definitions, engine.memo_snapshot().len())
            .ok_or_else(|| uninstalled())?;
        let source_index_bytes = engine.source_index_bytes().unwrap_or(0);
        let contexts = self
            .selection
            .files
            .iter()
            .map(|(file, context, _)| (*file, context.clone()))
            .collect::<Vec<_>>();
        let check_inventory = engine
            .front_check_input_bytes(
                &contexts,
                definitions,
                self.selection.lint_package.as_ref(),
                MemoryWeight::default(),
            )
            .ok_or_else(|| uninstalled())?;
        Plan::new(
            PlanInputs {
                limits,
                jobs: self.jobs,
                input_bytes: self.selection.input_bytes,
                definitions,
                check_inventory,
                shape: self.shape,
                footprint,
                run_input_bytes: run_inputs.as_ref().map_or(0, |inputs| inputs.input_bytes()),
                semantic_queries: run_inputs
                    .as_ref()
                    .map_or(0, |inputs| inputs.semantic_query_count()),
                catalogue_bytes: catalogue.as_ref().map_or(0, |inputs| inputs.input_bytes()),
                inspection_bytes,
                source_index_bytes,
            },
            self.host,
        )
    }
}
fn plan_source(
    engine: &mut QueryEngine,
    limits: RequestLimits,
    planner: &SourcePlanner<'_>,
    store: &dyn ResultStore,
    namespace: Option<&Arc<crate::NamespaceCapture>>,
) -> Result<SourcePlanning, ToolError> {
    let shape = planner.shape;
    let selection = planner.selection;
    // Capturing the footprint and the Run and native inputs can still name a
    // specialization (GEN-6); select again until no new root appears.
    let mut definitions = selected_definitions(engine);
    let mut run_error = None;
    let (footprint, run_inputs, catalogue) = loop {
        let footprint = match engine.front_input_footprint(&definitions) {
            Ok(footprint) => footprint,
            Err(error) => return Err(engine_error(engine, error)),
        };
        let run_inputs = if shape.run {
            let captured = match namespace {
                Some(namespace) => {
                    engine.front_run_inputs_in(namespace.clone(), limits.snapshot_bytes)
                }
                None => {
                    if !shape.check_variants || store.installation_output().is_some() {
                        engine.prepare_private_summaries();
                    }
                    engine.front_run_inputs(limits.snapshot_bytes)
                }
            };
            match captured {
                Ok(inputs) => {
                    run_error = None;
                    Some(inputs)
                }
                Err(error) if shape.check_variants => {
                    run_error = Some(error);
                    None
                }
                Err(error) => return Err(engine_error(engine, error)),
            }
        } else {
            None
        };
        // Ordinary catalogue capture can name specializations. Generated
        // declarations need their accepted views from bootstrap first.
        let generated = match engine.front_generator_roots(&definitions) {
            Ok(generators) => !generators.is_empty(),
            Err(error) => return Err(engine_error(engine, error)),
        };
        let catalogue = if (shape.native || shape.ownership) && (!generated || namespace.is_some())
        {
            let captured = match namespace {
                Some(namespace) => {
                    engine.front_native_catalogue_inputs(namespace.lease(), limits.snapshot_bytes)
                }
                None => engine.front_native_definition_inputs(&definitions, limits.snapshot_bytes),
            };
            match captured {
                Ok(inputs) => Some(inputs),
                Err(error) => return Err(engine_error(engine, error)),
            }
        } else {
            None
        };
        let selected = selected_definitions(engine);
        if selected == definitions {
            break (footprint, run_inputs, catalogue);
        }
        definitions = selected;
    };
    // The root package is what a session edits; keep its rows over the
    // packages it imports when one request publishes more than the store holds.
    let root = definitions
        .iter()
        .filter(|path| path.0.package == selection.entry.0.package)
        .cloned()
        .collect::<Vec<_>>();
    store.demand(&engine.front_store_keys(&root));
    let plan = planner.within(
        limits,
        engine,
        &definitions,
        footprint,
        &run_inputs,
        &catalogue,
    )?;
    Ok(SourcePlanning {
        definitions,
        footprint,
        run_inputs,
        catalogue,
        run_error,
        plan,
    })
}

/// Complete namespace demand for the operation's source roots before
/// installing consumers. Registration and Lower share one demand producer.
#[allow(clippy::too_many_arguments)]
pub fn prepare_source_scope(
    engine: &mut QueryEngine,
    selection: &SourceSelection,
    roots: &[DefPath],
    policy: LoweringPolicy,
    stop_on_error: bool,
    shape: SourceShape,
    jobs: Jobs,
    limits: RequestLimits,
    host: &dyn SourceHost,
    loading: &mut LoadingAdmission,
    store: &dyn ResultStore,
    carry: Option<&SourceCarry>,
) -> Result<(Option<FrontDemandScope>, RequestLimits), ToolError> {
    engine.admit_private_inference(loading.lend_runtime());
    let definitions = selected_definitions(engine);
    let generators = engine
        .front_generator_roots(&definitions)
        .map_err(|error| engine_error(engine, error))?;
    let staged = engine
        .front_staged_roots(&definitions)
        .map_err(|error| engine_error(engine, error))?;
    if roots.is_empty() || (generators.is_empty() && staged.is_empty()) {
        let runtime = engine
            .release_private_inference_admission()
            .map_err(|error| engine_error(engine, error))?;
        loading.return_runtime(runtime);
        return Ok((None, limits));
    }
    // Ordinary Front and Lower remain obligations of the final graph. When
    // every namespace producer is an explicit root and none selects generic
    // instances, preparation only needs those producers. Prefix requests still
    // check earlier roots before allowing later source publication.
    let preparation_roots = (!stop_on_error
        && generators
            .iter()
            .all(|path| !engine.instance_bodied(path) && roots.contains(path))
        && staged.iter().all(|path| roots.contains(path)))
    .then(|| {
        roots
            .iter()
            .filter(|path| generators.contains(*path) || staged.contains(*path))
            .cloned()
            .collect::<Vec<_>>()
    });
    let planned = plan_source(
        engine,
        limits,
        &SourcePlanner {
            selection,
            host,
            jobs,
            shape,
        },
        store,
        None,
    )?;
    let definitions = planned.definitions.clone();
    let plan = planned.plan;
    drop(planned.run_inputs);
    drop(planned.catalogue);
    let inference = engine
        .release_private_inference_admission()
        .map_err(|error| engine_error(engine, error))?;
    loading.return_runtime(inference);
    let metadata =
        FrontDemandScope::input_bytes(&definitions, &generators).ok_or_else(|| uninstalled())?;
    let (runtime, snapshot) = loading.lend();
    let lent = (runtime.bytes(), snapshot.bytes());
    let snapshot =
        hirsel_engine_admission::MemoryReservationPool::new(snapshot).map_err(|_| uninstalled())?;
    let split = |bytes| snapshot.take(bytes).ok_or_else(|| uninstalled());
    let front = split(plan.snapshots.front)?;
    let lower = split(plan.snapshots.lower)?;
    let rest = lent
        .1
        .checked_sub(plan.snapshots.front)
        .and_then(|bytes| bytes.checked_sub(plan.snapshots.lower))
        .and_then(|bytes| {
            bytes.checked_sub(hirsel_engine_admission::MemoryReservationPool::metadata_bytes())
        })
        .ok_or_else(|| uninstalled())?;
    let metadata = metadata.max(rest / 8);
    let reserved = split(metadata)?;
    let rest = rest.checked_sub(metadata).ok_or_else(|| uninstalled())?;
    let discovery = split(rest / 7)?;
    let text = split((rest - rest / 7) / 2)?;
    let syntax = split(rest - rest / 7 - (rest - rest / 7) / 2)?;
    let errors = runtime_errors(store)?;
    let scope = engine
        .demand_front_scope_with_policy(
            &definitions,
            preparation_roots.as_deref().unwrap_or(roots),
            &generators,
            policy,
            stop_on_error,
            FrontDemandAdmission {
                carry: carry.map(|carry| carry.front.clone()),
                jobs,
                weight: plan.body,
                max_reads: lower_reads(&definitions)?,
                runtime,
                front,
                lower,
                text,
                syntax,
                discovery,
                metadata: reserved,
            },
            errors,
        )
        .map_err(|error| instance_bootstrap_error(engine, error, errors))?;
    let snapshot_bytes = snapshot.available_bytes();
    drop(snapshot);
    host.readmit(loading, lent.0, snapshot_bytes)?;
    Ok((
        Some(scope),
        RequestLimits {
            snapshot_bytes,
            ..limits
        },
    ))
}

/// Install the scheduled source graph of `selection` in `engine`. The engine's
/// private inference holds the runtime partition when this returns; the host
/// releases it into the executor after adding its own roots.
#[allow(clippy::too_many_arguments)]
pub fn install(
    engine: &mut QueryEngine,
    selection: &SourceSelection,
    shape: SourceShape,
    jobs: Jobs,
    limits: RequestLimits,
    host: &dyn SourceHost,
    mut loading: LoadingAdmission,
    store: &dyn ResultStore,
    carry: Option<&SourceCarry>,
    scope: Option<FrontDemandScope>,
) -> Result<InstalledSource, ToolError> {
    let prepared_front = scope.as_ref().and_then(|scope| scope.front().cloned());
    let front_carry = scope
        .as_ref()
        .and_then(|scope| scope.carry().cloned())
        .or_else(|| carry.map(|carry| carry.front.clone()));
    let operation = scope.as_ref().map(FrontDemandScope::lease);
    let namespace = scope.map(|scope| scope.namespace().clone());
    engine.admit_private_inference(loading.lend_runtime());
    let planner = SourcePlanner {
        selection,
        host,
        jobs,
        shape,
    };
    let SourcePlanning {
        definitions,
        footprint: _,
        mut run_inputs,
        mut catalogue,
        mut run_error,
        plan: _,
    } = match plan_source(engine, limits, &planner, store, namespace.as_ref()) {
        Ok(planned) => planned,
        Err(error) => return Err(loading.failed(error)),
    };
    let final_limits = limits;
    let registrations = namespace.as_ref().map_or_else(Vec::new, |namespace| {
        namespace.registrations.values().cloned().collect()
    });
    if (shape.run || shape.ownership) && namespace.is_none() {
        if let Err(error) = engine.prepare_namespace_registrations(&registrations) {
            return Err(loading.failed(engine_error(engine, error)));
        }
        if !shape.check_variants || store.installation_output().is_some() {
            engine.prepare_private_summaries();
        }
    }
    let namespace = match namespace {
        Some(capture) => {
            capture
                .require_environment(engine)
                .map_err(|error| engine_error(engine, error))?;
            capture
        }
        None => match engine.capture_namespace(
            &definitions,
            &registrations,
            final_limits.snapshot_bytes,
        ) {
            Ok(capture) => capture,
            Err(error) => return Err(loading.failed(engine_error(engine, error))),
        },
    };
    let operation = operation.unwrap_or_else(|| namespace.lease());
    let footprint;
    // Registration changes the source namespace. Capture every final consumer
    // against the same accepted views, including Run and ownership catalogues.
    {
        footprint = match engine.front_input_footprint(&definitions) {
            Ok(footprint) => footprint,
            Err(error) => return Err(loading.failed(engine_error(engine, error))),
        };
        if shape.run {
            run_inputs =
                match engine.front_run_inputs_in(namespace.clone(), final_limits.snapshot_bytes) {
                    Ok(inputs) => {
                        run_error = None;
                        Some(inputs)
                    }
                    Err(error) if shape.check_variants => {
                        run_error = Some(error);
                        None
                    }
                    Err(error) => return Err(loading.failed(engine_error(engine, error))),
                };
        }
        if shape.native || shape.ownership {
            catalogue = Some(
                match engine
                    .front_native_catalogue_inputs(operation.clone(), final_limits.snapshot_bytes)
                {
                    Ok(inputs) => inputs,
                    Err(error) => return Err(loading.failed(engine_error(engine, error))),
                },
            );
        }
    }
    let current_definitions = selected_definitions(engine);
    if current_definitions != definitions {
        let mismatch = definitions
            .iter()
            .zip(&current_definitions)
            .find(|(a, b)| a != b);
        return Err(loading.failed(ToolError::Runtime(format!(
            "compiler source inventory changed after bootstrap: prepared {}, current {}, first mismatch {mismatch:?}",
            definitions.len(),
            current_definitions.len()
        ))));
    }
    let mut plan = planner.within(
        final_limits,
        engine,
        &definitions,
        footprint,
        &run_inputs,
        &catalogue,
    )?;
    let prepared_front = prepared_front.filter(|front| {
        !shape.compact_reads && front.covers_scope(&definitions, &namespace, plan.body)
    });
    let reuse_front = prepared_front.is_some();
    if prepared_front.is_some() {
        // The completed owner retains its original admitted Front partition.
        // Installation needs no second reservation for the same products.
        plan.snapshots.front = 0;
    }
    let sources = engine.sources.clone();
    let inference = match engine.release_private_inference_admission() {
        Ok(allowance) => allowance,
        Err(error) => return Err(loading.failed(engine_error(engine, error))),
    };
    loading.return_runtime(inference);
    let GraphAdmission {
        runtime,
        mut source,
        front,
        lower,
        mut growth,
        check,
        transport,
        native_transport,
        run,
    } = loading.finish(plan.snapshots)?;
    engine.admit_private_inference(runtime);
    let source_index = source
        .split(plan.source_index_bytes)
        .ok_or_else(|| uninstalled())?;
    let source = Arc::new(source);
    // Product source views outlive this graph and its meter. Give every view
    // the admitted source owner before a producer captures or clones the map.
    engine.sources = std::mem::take(&mut engine.sources).replace_owner(source.clone());
    let query_error = |error| ToolError::Query {
        error,
        phase: None,
        sources: sources.clone().retain_owner(source.clone()),
    };
    drop(transport);
    let errors = runtime_errors(store)?;
    let front = match prepared_front {
        Some(prepared) => {
            drop(front);
            prepared
        }
        None => engine
            .front_body_queries_in(&definitions, namespace, plan.body, front, errors)
            .map_err(query_error)?,
    };
    if shape.compact_reads {
        front.compact_retained_reads();
    }
    // Before any task: this fresh graph adopts the selector's earlier accepted
    // front facts only where their text and reads still validate.
    if let Some(carry) = front_carry.filter(|_| !reuse_front) {
        front.install_carry(carry).map_err(|_| uninstalled())?;
    }
    let lower = engine
        .front_lower_queries(
            &definitions,
            front.clone(),
            plan.body,
            lower_reads(&definitions)?,
            lower,
            errors,
        )
        .map_err(query_error)?;
    if let Some(carry) = carry {
        lower
            .install_carry(carry.lower.clone())
            .map_err(|_| uninstalled())?;
    }
    let (growth_admission, storage_allowance) = if shape.storage {
        let recipe_inputs =
            FrontStorageQueries::input_bytes(&front).ok_or_else(|| uninstalled())?;
        let graph_inputs =
            FrontStorageGraphQueries::input_bytes(&definitions).ok_or_else(|| uninstalled())?;
        let growth_inputs = FrontGrowthQueries::input_bytes(&definitions, plan.root)
            .ok_or_else(|| uninstalled())?;
        let recipe = growth.split(recipe_inputs).ok_or_else(|| uninstalled())?;
        let graph = growth.split(graph_inputs).ok_or_else(|| uninstalled())?;
        let roots = growth.split(growth_inputs).ok_or_else(|| uninstalled())?;
        let outputs = OutputPartition::new(growth).ok_or_else(|| uninstalled())?;
        (
            ProducerAdmission::shared(roots, outputs.clone()),
            Some((
                ProducerAdmission::shared(recipe, outputs.clone()),
                ProducerAdmission::shared(graph, outputs),
            )),
        )
    } else {
        (growth.into(), None)
    };
    let growth = engine
        .front_growth_queries(
            &selection.entry,
            front.clone(),
            plan.root,
            growth_admission,
            errors,
        )
        .map_err(query_error)?;
    let storage = storage_allowance
        .map(|(recipe_allowance, graph_allowance)| {
            let recipes = engine
                .front_storage_queries(
                    front.clone(),
                    MemoryWeight {
                        retained: plan.body.retained.min(64 << 10),
                        ..plan.body
                    },
                    recipe_allowance,
                    errors,
                )
                .map_err(query_error)?;
            let graph = engine
                .front_storage_graph_queries(
                    &selection.entry,
                    front.clone(),
                    growth.clone(),
                    recipes.clone(),
                    lower.clone(),
                    plan.root,
                    graph_allowance,
                    errors,
                )
                .map_err(query_error)?;
            Ok::<_, ToolError>((recipes, graph))
        })
        .transpose()?;
    Ok(InstalledSource {
        operation,
        definitions,
        plan,
        front,
        lower,
        growth,
        storage,
        check,
        run,
        native_transport,
        run_inputs,
        run_error,
        catalogue,
        source,
        source_index,
        sources,
        errors,
    })
}

impl InstalledSource {
    /// Retain `sources` with this graph's admitted source owner.
    pub fn query_error(&self, error: QueryError) -> ToolError {
        ToolError::Query {
            error,
            phase: None,
            sources: self.sources.clone().retain_owner(self.source.clone()),
        }
    }
    /// Construct the Check root over this graph from what remains of its
    /// Check partition.
    pub fn check(
        &mut self,
        engine: &mut QueryEngine,
        selection: &SourceSelection,
    ) -> Result<Arc<FrontCheckQueries>, ToolError> {
        let allowance = self
            .check
            .split(self.check.bytes())
            .ok_or_else(|| uninstalled())?;
        let check = engine
            .front_check_queries(
                FrontCheckPlan {
                    files: selection.files.clone(),
                    front: self.front.clone(),
                    lower: self.lower.clone(),
                    growth: self.growth.clone(),
                    unsafe_ffi: selection.unsafe_ffi.clone(),
                    lint_package: selection.lint_package.clone(),
                },
                self.plan.root,
                allowance,
                self.errors,
            )
            .map_err(|error| self.query_error(error))?;
        match &self.storage {
            Some((_, storage)) => check
                .with_storage(storage.clone())
                .map_err(|error| self.query_error(error)),
            None => Ok(check),
        }
    }
    /// Register the source producers and `check` with the request executor.
    pub fn register(
        &self,
        runtime: &mut AdmittedRuntime,
        check: &Arc<FrontCheckQueries>,
    ) -> Result<(), ToolError> {
        self.register_checks(runtime, check.factory())
    }
    /// Register multiple policy roots routed by their exact query identities.
    pub fn register_checks(
        &self,
        runtime: &mut AdmittedRuntime,
        checks: Arc<dyn QueryFactory>,
    ) -> Result<(), ToolError> {
        runtime
            .register(FRONT_BODY_KIND, self.front.factory())
            .map_err(registration)?;
        runtime
            .register_local(FRONT_LOWER_KIND, self.lower.factory())
            .map_err(registration)?;
        runtime
            .register_local(FRONT_EVALUATE_KIND, self.lower.factory())
            .map_err(registration)?;
        runtime
            .register(FRONT_GROWTH_KIND, self.growth.factory())
            .map_err(registration)?;
        runtime
            .register(FRONT_CHECK_KIND, checks)
            .map_err(registration)?;
        if let Some((_, graph)) = &self.storage {
            runtime
                .register(FRONT_STORAGE_GRAPH_KIND, graph.factory())
                .map_err(registration)?;
        }
        Ok(())
    }
}

impl QueryEngine {
    /// Check `selection` through the shared scheduled source graph, under
    /// this process's one host allowance and this engine's result store.
    pub(crate) fn check_scheduled(
        &mut self,
        selection: &SourceSelection,
    ) -> Result<CheckOutput, ToolError> {
        let (budget, cap) = host_budget();
        let jobs = self.jobs();
        let limits = host_limits(cap, jobs.get());
        let mut loading = LoadingAdmission::reserve(budget, limits, 0)?;
        let disk_bytes = limits
            .runtime_bytes
            .checked_add(limits.snapshot_bytes)
            .and_then(|bytes| bytes.checked_add(limits.transport_bytes))
            .ok_or_else(|| uninstalled())?;
        let backend: Arc<dyn ResultStore> = self
            .result_store
            .clone()
            .unwrap_or_else(|| Arc::new(NoneStore));
        let store = match RequestStore::with_scratch_and_verifier_transport(
            backend,
            loading.take_transport(),
            &std::env::temp_dir(),
            disk_bytes,
        ) {
            Ok(store) => Arc::new(store),
            Err(error) => return Err(loading.failed(ToolError::Io(error))),
        };
        // Like a request selector, the engine keeps one carry pair across
        // its fresh graphs; with no store it is a session's only reuse.
        if self.source_carry.is_none() && self.verify_store == hirsel_engine_store::VerifyStore::Off
        {
            let quarter = limits.transport_bytes / 4;
            let front = budget
                .reserve(quarter)
                .ok()
                .and_then(|allowance| FrontCarry::new(allowance).ok());
            let lower = budget
                .reserve(quarter)
                .ok()
                .and_then(|allowance| LowerCarry::new(allowance).ok());
            self.source_carry = front
                .zip(lower)
                .map(|(front, lower)| SourceCarry { front, lower });
        }
        let previous = self.result_store.replace(store.clone());
        let operation = store.operation_scope();
        let result = self.check_installed(selection, jobs, limits, budget, loading, &store);
        // A complete Check publishes its products for later requests, as a
        // host request does; anything else aborts its unpublished epoch.
        // Cache publication failure only loses reuse, never the result.
        if result
            .as_ref()
            .is_ok_and(|output| output.completion == crate::CheckCompletion::Complete)
        {
            let _ = operation.finish();
        } else {
            drop(operation);
        }
        // A failed installation can leave its inference grant behind.
        drop(self.release_private_inference_admission());
        self.result_store = previous;
        result
    }

    fn check_installed(
        &mut self,
        selection: &SourceSelection,
        jobs: Jobs,
        limits: RequestLimits,
        budget: &Arc<hirsel_engine_admission::ProcessBudget>,
        loading: LoadingAdmission,
        store: &Arc<RequestStore>,
    ) -> Result<CheckOutput, ToolError> {
        let shape = SourceShape {
            storage: true,
            ..SourceShape::default()
        };
        let carry = self.source_carry.as_ref().map(|carry| SourceCarry {
            front: carry.front.clone(),
            lower: carry.lower.clone(),
        });
        // Check's ordered cursor owns namespace demand as well as publication.
        // Preparing generators here would execute them before that cursor can
        // observe an earlier file stop.
        let mut installed = install(
            self,
            selection,
            shape,
            jobs,
            limits,
            &BudgetHost(budget),
            loading,
            store.as_ref(),
            carry.as_ref(),
            None,
        )?;
        let check = installed.check(self, selection)?;
        let runtime = self
            .release_private_inference_admission()
            .map_err(|error| installed.query_error(error))?;
        let mut runtime = AdmittedRuntime::with_query_capacity(
            jobs,
            runtime,
            installed.errors,
            installed.plan.query_capacity,
        )
        .map_err(ToolError::Io)?;
        installed.register(&mut runtime, &check)?;
        let front_before = front_work(&installed.front);
        let lower_before = installed
            .lower
            .work_snapshot()
            .into_iter()
            .collect::<std::collections::BTreeMap<_, _>>();
        let request = runtime.request(&[check.query()]);
        let checked = match runtime.wait(request) {
            Ok(products) => match products.as_slice() {
                [product] => check
                    .outcome(product.clone())
                    .map(|output| output.checked)
                    .ok_or_else(|| uninstalled()),
                _ => Err(uninstalled()),
            },
            Err(reference) => Err(match check.failure(reference) {
                Some((error, phase)) => {
                    if let Some(phase) = phase {
                        self.unavailable = Some(phase);
                    }
                    ToolError::Query {
                        error,
                        phase,
                        sources: installed
                            .sources
                            .clone()
                            .retain_owner(installed.source.clone()),
                    }
                }
                None => runtime_failure(reference, installed.errors, store.as_ref()),
            }),
        };
        runtime.shutdown();
        // The installed graph is the last reader of its admitted snapshot.
        // Adopt its completed private facts before those reader leases end.
        self.adopt_completed_headers();
        record_front_work(self, &installed.front, &front_before);
        record_lower_work(self, &installed.lower, &lower_before);
        checked
    }
}

/// The retained text bytes of the selected source files.
pub fn source_bytes(sources: &SourceMap, files: &[(FileId, FileContext, LoweringPolicy)]) -> u64 {
    files
        .iter()
        .filter_map(|(file, _, _)| sources.file(*file))
        .map(|file| file.text().len() as u64)
        .sum()
}

#[track_caller]
fn uninstalled() -> ToolError {
    let site = std::panic::Location::caller();
    ToolError::Runtime(format!(
        "selected compiler operation has no installed producer at {}:{}",
        site.file(),
        site.line()
    ))
}

fn engine_error(engine: &QueryEngine, error: QueryError) -> ToolError {
    let phase = engine
        .unavailable
        .or(if error == QueryError::NotImplemented {
            engine.last_phase
        } else {
            None
        });
    ToolError::Query {
        error,
        phase,
        sources: engine.sources.clone(),
    }
}

/// The read bound of one Lower body, shared by bootstrap and the final graph.
pub fn lower_reads(definitions: &[DefPath]) -> Result<usize, ToolError> {
    Ok(definitions
        .len()
        .checked_mul(3)
        .ok_or_else(|| uninstalled())?
        .min(4096))
}

fn instance_bootstrap_error(
    engine: &QueryEngine,
    error: FrontDemandError,
    errors: RuntimeErrors,
) -> ToolError {
    match error {
        FrontDemandError::Generator(error) => bootstrap_error(engine, error),
        FrontDemandError::Front {
            reference,
            error,
            sources,
            owner,
        } => language_error(
            error.unwrap_or_else(|| {
                QueryError::Operational(hirsel_types::OperationalFailure {
                    operation: "instance bootstrap".into(),
                    message: if reference == errors.resource {
                        format!(
                            "a front task exceeded its admitted resources; Front retained {} of {} bytes; refusal {:?}",
                            owner.retained_bytes(),
                            owner.admitted_bytes(),
                            owner.memory_refusal()
                        )
                    } else if reference == errors.cycle {
                        "a front task has a query dependency cycle".into()
                    } else if reference == errors.cancelled {
                        "a front task was cancelled".into()
                    } else if reference == errors.panicked {
                        "a front task panicked".into()
                    } else if reference == errors.protocol {
                        "a front task violated its query protocol".into()
                    } else {
                        "a front task failed without a diagnostic".into()
                    },
                })
            }),
            hirsel_query_products::QueryKind::Check,
            sources.retain_owner(owner),
        ),
        FrontDemandError::Discovery {
            error,
            sources,
            owner,
        } => language_error(error, hirsel_query_products::QueryKind::Generate, sources.retain_owner(owner)),
    }
}

fn bootstrap_error(engine: &QueryEngine, error: FrontRegistrationBootstrapError) -> ToolError {
    match error {
        FrontRegistrationBootstrapError::Admission(error) => engine_error(engine, error),
        FrontRegistrationBootstrapError::Runtime {
            error,
            sources,
            owner,
            ..
        } => language_error(
            error.unwrap_or_else(|| {
                QueryError::Operational(hirsel_types::OperationalFailure {
                    operation: "generator bootstrap".into(),
                    message: "a generator task failed without a diagnostic".into(),
                })
            }),
            hirsel_query_products::QueryKind::Generate,
            sources.retain_owner(owner),
        ),
        FrontRegistrationBootstrapError::Acceptance(failure) => {
            if failure.error == QueryError::NotImplemented {
                ToolError::Unsupported {
                    phase: hirsel_query_products::QueryKind::GeneratedRegistration,
                    construct: failure.definition,
                    span: failure.span,
                    sources: failure.sources,
                }
            } else {
                language_error(
                    failure.error,
                    hirsel_query_products::QueryKind::GeneratedRegistration,
                    failure.sources,
                )
            }
        }
    }
}

/// Language diagnostics found while bootstrapping a graph's generated
/// instances name the phase that found them, so a request reports them as
/// that phase's rejection rather than a load failure. Operational refusals
/// keep no phase.
fn language_error(
    error: QueryError,
    phase: hirsel_query_products::QueryKind,
    sources: SourceMap,
) -> ToolError {
    ToolError::Query {
        phase: matches!(
            error,
            QueryError::Diagnostics(_) | QueryError::NotImplemented
        )
        .then_some(phase),
        error,
        sources,
    }
}

pub fn registration(error: &'static str) -> ToolError {
    ToolError::Runtime(error.into())
}

const RUNTIME_DIAGNOSTIC: Domain = Domain {
    kind: 0x5200_0001,
    schema: 1,
};

#[track_caller]
pub(crate) fn protocol_failure(
    store: &dyn ResultStore,
    errors: RuntimeErrors,
    query: Option<QueryId>,
    detail: &str,
) -> hirsel_engine_store::Reference {
    let site = std::panic::Location::caller();
    let message = format!(
        "compiler query protocol failure: {detail}; query {query:?}; producer {}:{}",
        site.file(),
        site.line()
    );
    if std::env::var_os("HIRSEL_OPERATION_OWNER_TRACE").is_some() {
        eprintln!("OWNER PROTOCOL {message}");
    }
    store
        .put_content(RUNTIME_DIAGNOSTIC, message.as_bytes())
        .unwrap_or(errors.resource)
}

pub fn runtime_diagnostic(
    store: &dyn ResultStore,
    reference: hirsel_engine_store::Reference,
) -> Option<String> {
    (reference.domain == RUNTIME_DIAGNOSTIC).then_some(())?;
    let bytes = store.get_content(reference)?;
    let message = std::str::from_utf8(&bytes).ok()?;
    message
        .starts_with("compiler query protocol failure: ")
        .then(|| message.to_owned())
}

pub fn runtime_errors(store: &dyn ResultStore) -> Result<RuntimeErrors, ToolError> {
    let record = |message: &[u8]| {
        store
            .put_content(RUNTIME_DIAGNOSTIC, message)
            .ok_or_else(|| {
                ToolError::Runtime("compiler runtime diagnostics exceed transport admission".into())
            })
    };
    Ok(RuntimeErrors {
        resource: record(b"compiler query allowance exhausted")?,
        cycle: record(b"compiler dependency cycle")?,
        cancelled: record(b"compiler query cancelled")?,
        protocol: record(b"compiler query protocol failure")?,
        panicked: record(b"compiler query producer panicked")?,
    })
}

fn runtime_failure(
    reference: hirsel_engine_store::Reference,
    errors: RuntimeErrors,
    store: &dyn ResultStore,
) -> ToolError {
    if let Some(message) = runtime_diagnostic(store, reference) {
        return ToolError::Runtime(message);
    }
    let message = if reference == errors.resource {
        "a compiler producer exceeded its admitted resources"
    } else if reference == errors.cycle {
        "a compiler producer has a query dependency cycle"
    } else if reference == errors.cancelled {
        "a compiler producer was cancelled"
    } else if reference == errors.panicked {
        "a compiler producer panicked"
    } else if reference == errors.protocol {
        "a compiler producer violated its query protocol"
    } else {
        "a compiler producer failed without a diagnostic"
    };
    ToolError::Runtime(message.into())
}

/// Every indexed definition and required source root, after registering the
/// specializations they name.
pub fn selected_definitions(engine: &mut QueryEngine) -> Vec<DefPath> {
    let indexed = engine.indexed_definitions();
    engine.register_specializations(&indexed);
    let required = engine.front_source_roots();
    engine.register_specializations(&required);
    indexed
        .into_iter()
        .chain(engine.front_source_roots())
        .chain(
            engine
                .instance_bodies
                .instances
                .values()
                .flat_map(|instances| {
                    instances
                        .values()
                        .map(|instance| instance.definition.path().clone())
                }),
        )
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

type FrontWork = std::collections::BTreeMap<(DefPath, hirsel_query_products::QueryKind), u64>;
fn front_work(front: &FrontBodyQueries) -> FrontWork {
    let mut work = FrontWork::new();
    for (path, kind, count) in front.work_snapshot() {
        *work.entry((path, kind)).or_default() += count;
    }
    work
}

/// The engine's recompute counters observe the producers' work, as the
/// request meter does for a host graph.
fn record_front_work(engine: &mut QueryEngine, front: &FrontBodyQueries, before: &FrontWork) {
    for ((path, kind), count) in front_work(front) {
        let count = count.saturating_sub(before.get(&(path.clone(), kind)).copied().unwrap_or(0));
        if count != 0 && kind != hirsel_query_products::QueryKind::Signature {
            engine.record_worker_recomputes(&path, kind, count);
        }
    }
}

fn record_lower_work(
    engine: &mut QueryEngine,
    lower: &FrontLowerQueries,
    before: &std::collections::BTreeMap<DefPath, crate::FrontLowerWork>,
) {
    for (path, work) in lower.work_snapshot() {
        let old = before.get(&path).copied().unwrap_or_default();
        let lowerings = work.lowerings.saturating_sub(old.lowerings);
        let initializers = work.initializers.saturating_sub(old.initializers);
        let validation_runs = work.validation_runs.saturating_sub(old.validation_runs);
        if lowerings != 0 {
            engine.record_worker_recomputes(
                &path,
                hirsel_query_products::QueryKind::Lower,
                lowerings,
            );
        }
        if initializers != 0 {
            engine.record_worker_recomputes(
                &path,
                hirsel_query_products::QueryKind::Evaluate,
                initializers,
            );
        }
        if lowerings != 0 || initializers != 0 || validation_runs != 0 {
            engine.record_worker_lower_work(&path, lowerings, initializers, validation_runs);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_registration_reports_its_phase_declaration_and_source_location() {
        let mut sources = SourceMap::default();
        let file = sources
            .add("todo-api.hir", "\nfn main() {}".into())
            .unwrap();
        let span = hirsel_span::Span::new(sources.file_anchor(file), 4, 8).unwrap();
        let error = ToolError::Unsupported {
            phase: hirsel_query_products::QueryKind::GeneratedRegistration,
            construct: crate::DefId::intern(&crate::entry_path(&crate::default_context())),
            span: Some(span),
            sources: sources.clone(),
        };
        let missing = language_error(
            QueryError::NotImplemented,
            hirsel_query_products::QueryKind::Generate,
            sources,
        );
        assert_eq!(
            missing.to_string(),
            "generated source evaluation: unsupported compiler query"
        );
        let error = retain_error(error, Arc::new(()));
        assert_eq!(
            error.to_string(),
            "generated instance registration: unsupported declaration `standalone@0/main` at todo-api.hir:2:4"
        );
    }
}
