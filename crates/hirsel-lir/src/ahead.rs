//! Compiler-owned proposals for private scalar execution. A proposal is not a
//! machine certificate and must never by itself authorize a runtime entry.
//!
//! The graph is built before object partitioning. In particular, the older
//! op-free spawn hint (which admits recursive SCCs) is not used here.
use std::collections::{BTreeMap, BTreeSet};

use hirsel_syntax::ContentHash;

use crate::ir::*;
use crate::layout_types::{LayoutId, LayoutKind, LayoutTable, ScalarKind, SizeSpec, TargetSpec};
use crate::stage::{Ready, StageOutput};

mod cpu_mapper;
mod cpu_roots;
mod cpu_worker;
mod host_return;
mod plan;
pub use cpu_mapper::{
    CpuGrowth, CpuGrowthPlace, CpuMapperGraph, CpuRegional, cpu_mapper_graph,
    validate_cpu_mapper_graph,
};
pub use cpu_roots::{CpuCaptureRoot, CpuSavePoint, CpuSaveRoots};
pub use cpu_worker::{
    CpuMapperBinding, CpuSummary, CpuWorkerGraph, CpuWorkerProtocol, cpu_mapper_candidates,
    cpu_owned_layout, validate_cpu_summaries, validate_cpu_workers,
};
pub use host_return::{HostReturnGraph, LiveOwners, ReadContinuation, Spine, read_metadata_name};
pub use plan::{
    AheadPlan, CaptureRecipe, RootPlan, alternate_name, alternate_resume_name, environments,
    metadata_name, prospective,
};

pub const MAX_UNITS: usize = 64;
pub const MAX_INSTRUCTIONS: usize = 4096;
pub const MAX_IMAGE_BYTES: u64 = 256;
pub const MAX_STACK_BYTES: u64 = 65536;

/// A pure scalar instruction: it reads only scalar SSA values and defines only
/// scalar SSA values, names no slot, address, storage, owner, call, region or
/// layout, and at most takes its own source failure edge (SEM-1, MEM-14).
/// `scalar` is the caller's admission of one value's type. Every ahead
/// classifier (scalar graphs, host-return graphs and allocating CPU mapper
/// graphs) admits these through this one definition, so a scalar instruction a
/// core helper starts emitting is admitted or refused by all of them together.
/// The float `ScalarUnary` ops and `MulAdd` stay refused: IEEE float images,
/// constants and conversions are certified against independent bit oracles
/// (bffadf03), these are not, so a mapper using them must refuse before
/// preparation rather than claim the interpreter's bits.
pub(crate) fn pure_scalar(
    u: &LirUnit<Ready>,
    inst: &Inst,
    scalar: impl Fn(ValueId) -> bool,
) -> bool {
    match *inst {
        Inst::Const { result, constant } => {
            matches!(
                u.tables.constants[constant.index()].value,
                ConstantValue::Unit
                    | ConstantValue::Bool(_)
                    | ConstantValue::Integer(_)
                    | ConstantValue::F32(_)
                    | ConstantValue::F64(_)
            ) && scalar(result)
        }
        Inst::Unary {
            result, operand, ..
        } => scalar(operand) && scalar(result),
        Inst::ScalarUnary {
            result,
            op,
            operand,
        } => !op.is_float() && scalar(operand) && scalar(result),
        Inst::Convert {
            result,
            flag,
            operand,
            ..
        } => scalar(operand) && scalar(result) && flag.is_none_or(&scalar),
        Inst::Binary {
            result,
            left,
            right,
            ..
        } => scalar(left) && scalar(right) && scalar(result),
        Inst::IntArith {
            result,
            flag,
            left,
            right,
            ..
        } => scalar(left) && scalar(right) && scalar(result) && flag.is_none_or(&scalar),
        Inst::Check { condition, .. } => scalar(condition),
        _ => false,
    }
}

/// The scalar-graph classifier of one instruction. Value and slot types are
/// checked for the whole body before this runs.
fn scalar_instruction(table: &LayoutTable, unit: &LirUnit<Ready>, inst: &Inst) -> bool {
    pure_scalar(unit, inst, |_| true)
        || match inst {
            Inst::StorageLive { .. }
            | Inst::StorageDead { .. }
            | Inst::EndScope { .. }
            | Inst::Address { .. }
            | Inst::Read { .. }
            | Inst::CopyValue { .. }
            | Inst::Store { .. }
            | Inst::Replace { .. }
            | Inst::UpdateMask { .. }
            | Inst::BeginLoan { .. }
            | Inst::EndLoan { .. }
            | Inst::BindOrigin { .. }
            | Inst::BindFields { .. }
            | Inst::EndOrigin { .. } => true,
            Inst::Construct { construct, .. } => {
                scalar_image(table, unit.tables.constructs[construct.index()].layout)
            }
            _ => false,
        }
}

/// Each ahead classifier's verdict on one instruction of `u`: scalar graph,
/// host-return graph and allocating CPU mapper graph. Only for the law that a
/// pure scalar instruction any core helper emits is admitted by all three.
#[doc(hidden)]
pub fn classifier_verdicts(u: &LirUnit<Ready>, table: &LayoutTable, inst: &Inst) -> [bool; 3] {
    [
        scalar_instruction(table, u, inst),
        host_return::classifies(u, table, inst),
        cpu_mapper::classifies(u, table, inst),
    ]
}

/// Identity includes the full dependency body, not merely its instance name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScalarBody {
    pub body: ContentHash,
    pub calls: BTreeSet<ContentHash>,
    pub checkpoint_points: Vec<bool>,
}

/// The complete scalar call closure, still awaiting final machine evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScalarGraph {
    pub root: ContentHash,
    pub bodies: BTreeMap<ContentHash, ScalarBody>,
    pub identity: ContentHash,
}

/// Refusals are optimization outcomes. They do not reject ordinary execution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Refusal {
    pub instance: ContentHash,
    pub reason: String,
}

fn refused(instance: ContentHash, reason: impl Into<String>) -> Refusal {
    Refusal {
        instance,
        reason: reason.into(),
    }
}

/// Whether a bounded image contains only inert integer, boolean or IEEE float bits. Walk
/// inline layouts only; a cycle is invalid evidence, never a recursive proof.
pub fn scalar_image(table: &LayoutTable, id: LayoutId) -> bool {
    fn visit(table: &LayoutTable, id: LayoutId, active: &mut BTreeSet<LayoutId>) -> bool {
        if !active.insert(id) {
            return false;
        }
        let admitted = table.layout(id).is_some_and(|layout| {
            matches!(layout.size, SizeSpec::Fixed(n) if n <= MAX_IMAGE_BYTES)
                && layout.align <= 16
                && layout.drop.is_empty()
                && layout.scan.is_empty()
                && layout.finalizer.is_none()
                && match &layout.kind {
                    LayoutKind::Unit => true,
                    LayoutKind::Scalar(k) => matches!(
                        k,
                        ScalarKind::Bool
                            | ScalarKind::U8
                            | ScalarKind::U16
                            | ScalarKind::U32
                            | ScalarKind::U64
                            | ScalarKind::I8
                            | ScalarKind::I16
                            | ScalarKind::I32
                            | ScalarKind::I64
                            | ScalarKind::F32
                            | ScalarKind::F64
                    ),
                    LayoutKind::Aggregate
                    | LayoutKind::Variant
                    | LayoutKind::Environment { .. } => {
                        layout.fields.iter().all(|f| visit(table, f.layout, active))
                            && layout
                                .tag
                                .iter()
                                .flat_map(|t| &t.variants)
                                .flat_map(|v| &v.fields)
                                .all(|f| visit(table, f.layout, active))
                    }
                    LayoutKind::Inline { element, count } => {
                        *count <= MAX_IMAGE_BYTES && visit(table, *element, active)
                    }
                    _ => false,
                }
        });
        active.remove(&id);
        admitted
    }
    visit(table, id, &mut BTreeSet::new())
}

/// Analyze a scalar source body and all its reachable direct/Bound bodies.
/// Missing Ready content is refused, even when a symbol has an op-free hint.
/// `helper_identity` binds the exact runtime assembly/record contract; callers
/// must also bind emission options and final object/frame evidence later.
pub fn scalar_graph(
    units: &[&StageOutput<Ready>],
    table: &crate::SealedLayoutTable,
    target: &TargetSpec,
    root: ContentHash,
    helper_identity: ContentHash,
) -> Result<ScalarGraph, Refusal> {
    if target.triple != "x86_64-unknown-linux-gnu"
        || target.c_convention != crate::layout_types::CConvention::SysV64
        || target.pointer_bytes != 8
        || target.cpu != "x86-64"
        || !target.features.is_empty()
    {
        return Err(refused(root, "unsupported ahead target"));
    }
    let mut graph = Walker {
        units: units
            .iter()
            .map(|u| (u.unit().header.instance, u.unit()))
            .collect(),
        table,
        active: BTreeSet::new(),
        bodies: BTreeMap::new(),
        instructions: 0,
    };
    if graph.units.len() != units.len() {
        return Err(refused(root, "duplicate Ready instance"));
    }
    graph.visit(root)?;
    // These encodings have fixed widths or an explicit count, so concatenation
    // cannot alias a different graph. Unit digests bind instructions, layouts,
    // call ABI and exact source/failure anchors.
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(&root.0);
    bytes.extend_from_slice(&target.digest().0);
    bytes.extend_from_slice(&table.digest().0);
    bytes.extend_from_slice(&helper_identity.0);
    bytes.extend_from_slice(&(graph.bodies.len() as u32).to_le_bytes());
    for (id, body) in &graph.bodies {
        bytes.extend_from_slice(&id.0);
        bytes.extend_from_slice(&body.body.0);
        bytes.extend_from_slice(&(body.checkpoint_points.len() as u32).to_le_bytes());
        bytes.extend(body.checkpoint_points.iter().map(|v| u8::from(*v)));
    }
    Ok(ScalarGraph {
        root,
        bodies: graph.bodies,
        identity: ContentHash::of_parts("hirsel.ahead.scalar-graph.v1", [bytes.as_slice()]),
    })
}

struct Walker<'a> {
    units: BTreeMap<ContentHash, &'a LirUnit<Ready>>,
    table: &'a LayoutTable,
    active: BTreeSet<ContentHash>,
    bodies: BTreeMap<ContentHash, ScalarBody>,
    instructions: usize,
}

impl Walker<'_> {
    fn visit(&mut self, id: ContentHash) -> Result<(), Refusal> {
        if self.bodies.contains_key(&id) {
            return Ok(());
        }
        if !self.active.insert(id) {
            return Err(refused(id, "recursive call SCC"));
        }
        if self.active.len() + self.bodies.len() > MAX_UNITS {
            return Err(refused(id, "ahead graph exceeds 64 units"));
        }
        let unit = *self
            .units
            .get(&id)
            .ok_or_else(|| refused(id, "missing Ready body"))?;
        self.instructions = self
            .instructions
            .checked_add(
                unit.instructions
                    .len()
                    .checked_add(unit.terminators.len())
                    .ok_or_else(|| refused(id, "instruction count overflow"))?,
            )
            .ok_or_else(|| refused(id, "instruction count overflow"))?;
        if self.instructions > MAX_INSTRUCTIONS {
            return Err(refused(id, "ahead graph exceeds 4096 instructions"));
        }
        for slot in &unit.slots {
            if matches!(slot.kind, SlotKind::LoopCarried(_)) {
                return Err(refused(id, "normalized recursive source body"));
            }
        }
        // Ready can retain unused type-table rows from earlier passes (e.g.
        // reuse's Token type). Only actual values/slots constitute live images.
        for ty in unit
            .values
            .iter()
            .map(|v| v.ty)
            .chain(unit.slots.iter().map(|s| s.ty))
        {
            let ty = &unit.types[ty.index()];
            let layout = ty.layout;
            if !matches!(
                ty.repr,
                ValueRepr::Unit
                    | ValueRepr::Scalar(_)
                    | ValueRepr::Aggregate
                    | ValueRepr::InitMask { .. }
            ) || !scalar_image(self.table, layout)
            {
                return Err(refused(id, format!("non-scalar layout {}", layout.0)));
            }
        }
        let abi = &self.table.fn_abis[unit.header.abi.0 as usize];
        if abi
            .parameters
            .iter()
            .any(|p| !scalar_image(self.table, p.layout))
            || abi
                .result
                .as_ref()
                .is_some_and(|r| !scalar_image(self.table, r.layout))
        {
            return Err(refused(id, "non-scalar entry ABI"));
        }
        fn root_slot(u: &LirUnit<Ready>, a: AddressId) -> Option<SlotId> {
            match u.addresses[a.index()].base {
                AddressBase::Slot(s) => Some(s),
                AddressBase::Address(a) => root_slot(u, a),
                _ => None,
            }
        }
        for inst in &unit.instructions {
            if let Inst::Store { address, .. }
            | Inst::Replace { address, .. }
            | Inst::BeginLoan {
                address,
                mutable: true,
                ..
            } = inst
                && root_slot(unit, *address)
                    .is_none_or(|s| matches!(unit.slots[s.index()].kind, SlotKind::Capture(_)))
            {
                return Err(refused(id, "mutable scalar captures"));
            }
            if !scalar_instruction(self.table, unit, inst) {
                return Err(refused(
                    id,
                    format!("unclassified scalar instruction {:?}", inst.opcode()),
                ));
            }
        }
        // Address roots are either private scalar slots or scalar SSA spills.
        // No heap/foreign/callable/handle address is admitted by the type proof.
        for address in &unit.addresses {
            if address
                .projection
                .get(&unit.pools.projections)
                .unwrap_or(&[])
                .iter()
                .any(|p| {
                    !matches!(
                        p,
                        ProjectionStep::Field(_) | ProjectionStep::VariantField { .. }
                    )
                })
            {
                return Err(refused(id, "non-inline scalar address projection"));
            }
        }
        let mut calls = BTreeSet::new();
        for term in &unit.terminators {
            match term {
                Terminator::Call { call, .. } => {
                    let call = &unit.tables.calls[call.index()];
                    if call
                        .arguments
                        .get(&unit.pools.call_arguments)
                        .unwrap_or(&[])
                        .iter()
                        .any(|a| {
                            a.passing == hirsel_core_contract::Passing::Mut
                                && match a.value {
                                    ArgumentValue::Address { address, .. } => {
                                        root_slot(unit, address).is_none_or(|s| {
                                            matches!(
                                                unit.slots[s.index()].kind,
                                                SlotKind::Capture(_)
                                            )
                                        })
                                    }
                                    _ => true,
                                }
                        })
                    {
                        return Err(refused(id, "nonlocal/mutable scalar capture argument"));
                    }

                    let symbol = match call.callee {
                        Callee::Direct(s) | Callee::Bound { code: s, .. } => {
                            &unit.tables.symbols[s.index()]
                        }
                        Callee::Indirect(_) => return Err(refused(id, "indirect scalar call")),
                        Callee::Operation(_) => return Err(refused(id, "opaque scalar operation")),
                    };
                    let callee = symbol
                        .instance
                        .ok_or_else(|| refused(id, "unknown callee instance"))?;
                    let reached = self
                        .units
                        .get(&callee)
                        .ok_or_else(|| refused(callee, "missing Ready callee"))?;
                    if reached.header.definition != symbol.definition
                        || reached.header.abi != call.abi
                    {
                        return Err(refused(id, "callee code/ABI mismatch"));
                    }
                    calls.insert(callee);
                }
                Terminator::Jump(_)
                | Terminator::Branch { .. }
                | Terminator::Switch { .. }
                | Terminator::Return(_)
                | Terminator::Raise(_)
                | Terminator::Propagate
                | Terminator::Unreachable => {}
                _ => return Err(refused(id, "unclassified scalar terminator")),
            }
        }
        for callee in &calls {
            self.visit(*callee)?;
        }
        self.active.remove(&id);
        self.bodies.insert(
            id,
            ScalarBody {
                body: unit.digest(),
                calls,
                checkpoint_points: crate::checkpoints::checkpoint_points(unit),
            },
        );
        Ok(())
    }
}

/// Compose exact final frame extents along the already proven DAG. Include
/// startup separately and the larger of the two fixed helper call extents.
/// Absence, overflow and oversize are all refusal, never a guessed reserve.
pub fn stack_bound(
    graph: &ScalarGraph,
    frames: &BTreeMap<ContentHash, u64>,
    startup: u64,
    getter: u64,
    handoff: u64,
) -> Option<u64> {
    fn visit(
        id: ContentHash,
        graph: &ScalarGraph,
        frames: &BTreeMap<ContentHash, u64>,
        helper: u64,
        active: &mut BTreeSet<ContentHash>,
        memo: &mut BTreeMap<ContentHash, u64>,
    ) -> Option<u64> {
        if let Some(bound) = memo.get(&id) {
            return Some(*bound);
        }
        if !active.insert(id) {
            return None;
        }
        let body = graph.bodies.get(&id)?;
        let mut child = helper;
        for callee in &body.calls {
            child = child.max(visit(*callee, graph, frames, helper, active, memo)?);
        }
        let bound = frames.get(&id)?.checked_add(child)?;
        active.remove(&id);
        memo.insert(id, bound);
        Some(bound)
    }
    let bound = startup.checked_add(visit(
        graph.root,
        graph,
        frames,
        getter.max(handoff),
        &mut BTreeSet::new(),
        &mut BTreeMap::new(),
    )?)?;
    (bound <= MAX_STACK_BYTES).then_some(bound)
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct MachineEvidence {
    pub plan: ContentHash,
    pub object: ContentHash,
    pub options: ContentHash,
    pub frames: Vec<(ContentHash, u64)>,
    pub bounds: Vec<(ContentHash, u64)>,
}
hirsel_codec::snapshot_struct!(MachineEvidence {
    plan,
    object,
    options,
    frames,
    bounds
});
