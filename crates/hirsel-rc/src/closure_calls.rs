//! Callable code identity flows independently of its environment and ownership.
//! Discovery reads SSA versions, never source names or layout equality as code evidence.
//! Identity also flows through the callable fields of records and tuples: each
//! value and slot has one fact per callable field path of its layout, and a
//! direct call's result and `mut` write-backs follow the callee's field summary.
use crate::import::Lowered;
use crate::instance::InstanceId;
use hirsel_core_contract::Passing;
use hirsel_lir::abi_types::FnAbiId;
use hirsel_lir::ir::*;
use hirsel_lir::layout_types::{LayoutId, LayoutKind, LayoutTable};
use hirsel_lir::stage::Value;
use hirsel_syntax::ContentHash;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, VecDeque};

mod request;
pub use request::{CallableRecipe, TaskCaptureRecipe, TaskCaptureRequest};

pub const MAX_PER_FUNCTION: usize = 16;
pub const MAX_SPECIALIZATIONS: usize = 256;
/// Core library bodies that take a callback and call it once with arguments
/// they own, so a known callback specializes them as an ordinary instance.
pub const HIGHER_ORDER: &[&str] = &[
    "arena.with",
    "arena.bounded",
    "task.scope",
    "task.scope_until",
    "task.isolate",
    "runner.apply",
    "runner.apply_mut",
    "mutex.with",
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CallableTarget {
    pub symbol: Symbol,
    pub abi: FnAbiId,
    pub environment: bool,
}
type Target = CallableTarget;

/// Field ordinals from a value to one of its callable fields; empty is the value itself.
pub type FieldPath = Vec<u32>;

/// The entry input of a body that a seed names.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub enum SeedInput {
    /// A declared parameter, by ABI position.
    Parameter(usize),
    /// A closure body's capture, by capture ordinal.
    Capture(usize),
}

/// One proven code identity at a field path of an entry input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CallableSeed {
    pub input: SeedInput,
    pub path: FieldPath,
    pub target: CallableTarget,
}

/// A seed bound to the selected body's own entry value.
pub type ValueSeed = (ValueId, FieldPath, CallableTarget);

/// Bind portable seeds to the entry values of the body they were requested for.
pub fn bind_seeds(unit: &LirUnit<Value>, seeds: &[CallableSeed]) -> Result<Vec<ValueSeed>, String> {
    seeds
        .iter()
        .map(|seed| {
            let value = match seed.input {
                SeedInput::Parameter(i) => unit.parameters.get(i),
                SeedInput::Capture(i) => unit.captures.get(i),
            }
            .ok_or("callable seed names no entry input of its body")?;
            Ok((*value, seed.path.clone(), seed.target.clone()))
        })
        .collect()
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Exit {
    Result,
    Parameter(usize),
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Carry {
    exit: Exit,
    path: FieldPath,
    parameter: usize,
    from: FieldPath,
}

/// Which callable fields of a body's result and `mut` parameters hold, at
/// every exit, exactly the code its caller passed in a parameter field. It
/// names ABI positions only, so it is portable between layout tables. An
/// absent carry is unknown code; the empty summary proves nothing.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FieldSummary {
    carries: Vec<Carry>,
}
impl FieldSummary {
    fn carried(&self, exit: Exit, path: &[u32]) -> Option<(usize, &[u32])> {
        self.carries
            .iter()
            .find(|c| c.exit == exit && c.path == path)
            .map(|c| (c.parameter, c.from.as_slice()))
    }
    pub fn retained_bytes(&self) -> usize {
        self.carries
            .iter()
            .map(|c| size_of::<Carry>() + size_of::<u32>() * (c.path.len() + c.from.len()))
            .sum()
    }
}

/// Immutable callable declaration facts. Keys describe original instances;
/// their bodies are not needed to discover a specialization request.
#[derive(Clone, Debug)]
pub struct CallableEntry {
    pub key: hirsel_layout::instance::InstanceKey,
    pub abi: FnAbiId,
    pub origin: UnitOrigin,
    pub op_free: bool,
    /// The original body's summary; empty for a member of the caller's own
    /// call-graph component, so no summary depends on its own fixpoint.
    pub fields: FieldSummary,
}

/// One explicit request for an original Value with known callable inputs.
/// The consumer applies these same seeds when it transforms that selected body.
#[derive(Clone, Debug)]
pub struct CallableRequest {
    pub key: hirsel_layout::instance::InstanceKey,
    /// Canonical (input, path) order.
    pub seeds: Vec<CallableSeed>,
    pub spawn_flags: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Code {
    Pending,
    Known(Target),
    /// Whatever the caller passed in this entry node (a parameter field).
    Entry(usize),
    Dynamic,
}
impl Code {
    fn join(&self, other: &Self) -> Self {
        match (self, other) {
            (Self::Pending, x) | (x, Self::Pending) => x.clone(),
            (a, b) if a == b => a.clone(),
            _ => Self::Dynamic,
        }
    }
}

fn symbol(u: &mut LirUnit<Value>, target: &Symbol) -> SymbolId {
    if let Some(i) = u.tables.symbols.iter().position(|s| s == target) {
        return SymbolId(i as u32);
    }
    let id = SymbolId(u.tables.symbols.len() as u32);
    u.tables.symbols.push(target.clone());
    id
}

/// The root slot of a place and its full projection, or None through a pointer.
fn place(u: &LirUnit<Value>, address: AddressId) -> Option<(SlotId, Vec<ProjectionStep>)> {
    let info = &u.addresses[address.index()];
    let (slot, mut steps) = match info.base {
        AddressBase::Slot(s) => (s, Vec::new()),
        AddressBase::Address(a) => place(u, a)?,
        AddressBase::Value(_) => return None,
    };
    steps.extend(
        info.projection
            .get(&u.pools.projections)
            .unwrap_or(&[])
            .iter()
            .cloned(),
    );
    Some((slot, steps))
}

/// A projection made of field steps only.
fn fields(steps: &[ProjectionStep]) -> Option<FieldPath> {
    steps
        .iter()
        .map(|step| match step {
            ProjectionStep::Field(f) => Some(*f),
            _ => None,
        })
        .collect()
}

enum Overlap {
    /// The write covers this path; the rest of the path is inside the written value.
    Inside(usize),
    /// The write is in a sibling field; this path keeps its content.
    Apart,
    Unknown,
}
fn overlap(path: &[u32], steps: &[ProjectionStep]) -> Overlap {
    for (i, step) in steps.iter().enumerate() {
        match (step, path.get(i)) {
            (ProjectionStep::Field(f), Some(p)) if f != p => return Overlap::Apart,
            (ProjectionStep::Field(_), Some(_)) => {}
            _ => return Overlap::Unknown,
        }
    }
    Overlap::Inside(steps.len())
}

/// The value itself, then every callable reached through inline record and
/// tuple fields. Layouts are finite trees, so the list is too.
fn callable_paths(table: &LayoutTable, layout: LayoutId) -> Vec<FieldPath> {
    fn walk(
        table: &LayoutTable,
        layout: LayoutId,
        prefix: &mut FieldPath,
        out: &mut Vec<FieldPath>,
    ) {
        let Some(record) = table.layout(layout) else {
            return;
        };
        if !matches!(record.kind, LayoutKind::Aggregate) {
            return;
        }
        for field in &record.fields {
            prefix.push(field.ordinal);
            match table.layout(field.layout).map(|l| &l.kind) {
                Some(LayoutKind::Callable { .. }) => out.push(prefix.clone()),
                Some(LayoutKind::Aggregate) => walk(table, field.layout, prefix, out),
                _ => {}
            }
            prefix.pop();
        }
    }
    let mut out = vec![Vec::new()];
    walk(table, layout, &mut Vec::new(), &mut out);
    out
}

/// Solved code facts of one body: a node per (value, path) and per (slot,
/// path). A slot node joins every version of that slot, so it holds what any
/// version holds there, wherever control is.
struct Facts {
    values: Vec<(usize, usize)>,
    slots: Vec<(usize, usize)>,
    shapes: Vec<Vec<FieldPath>>,
    codes: Vec<Code>,
}
const DYNAMIC: Code = Code::Dynamic;
impl Facts {
    fn at(&self, (start, shape): (usize, usize), path: &[u32]) -> Option<usize> {
        self.shapes[shape]
            .iter()
            .position(|p| p == path)
            .map(|i| start + i)
    }
    fn value(&self, v: ValueId, path: &[u32]) -> Option<usize> {
        self.at(self.values[v.index()], path)
    }
    fn slot(&self, s: SlotId, path: &[u32]) -> Option<usize> {
        self.at(self.slots[s.index()], path)
    }
    fn value_paths(&self, v: ValueId) -> &[FieldPath] {
        &self.shapes[self.values[v.index()].1]
    }
    fn slot_paths(&self, s: SlotId) -> &[FieldPath] {
        &self.shapes[self.slots[s.index()].1]
    }
    fn code(&self, node: Option<usize>) -> &Code {
        node.map_or(&DYNAMIC, |n| &self.codes[n])
    }
    fn root(&self, v: ValueId) -> &Code {
        &self.codes[self.values[v.index()].0]
    }
    /// What a call passes for callee field `from` of argument `arg`.
    fn argument(&self, u: &LirUnit<Value>, arg: &CallArgument, from: &[u32]) -> Option<usize> {
        match arg.value {
            ArgumentValue::Value(v) => self.value(v, from),
            ArgumentValue::Address { address, .. } => {
                let (slot, steps) = place(u, address)?;
                let mut path = fields(&steps)?;
                path.extend_from_slice(from);
                self.slot(slot, &path)
            }
        }
    }
    /// Known code passed to each callee input field of a direct call.
    fn arguments(
        &self,
        u: &LirUnit<Value>,
        abi: &hirsel_lir::abi_types::FnAbi,
        site: &CallSite,
    ) -> Vec<CallableSeed> {
        let mut seeds = Vec::new();
        for arg in site.arguments.get(&u.pools.call_arguments).unwrap_or(&[]) {
            let Some(p) = abi
                .parameters
                .iter()
                .position(|p| p.source == arg.parameter)
            else {
                continue;
            };
            let paths = match arg.value {
                ArgumentValue::Value(_) if abi.parameters[p].convention == Passing::Mut => continue,
                ArgumentValue::Value(v) => self.value_paths(v).to_vec(),
                ArgumentValue::Address { address, .. } => {
                    let Some((slot, steps)) = place(u, address) else {
                        continue;
                    };
                    let Some(q) = fields(&steps) else { continue };
                    self.slot_paths(slot)
                        .iter()
                        .filter_map(|path| path.strip_prefix(q.as_slice()).map(<[u32]>::to_vec))
                        .collect()
                }
            };
            for path in paths {
                if let Code::Known(target) = self.code(self.argument(u, arg, &path)) {
                    seeds.push(CallableSeed {
                        input: SeedInput::Parameter(p),
                        path,
                        target: target.clone(),
                    });
                }
            }
        }
        seeds.sort_by(|a, b| (a.input, &a.path).cmp(&(b.input, &b.path)));
        seeds
    }
    /// The body's own carries, read from its exits. Only meaningful for an
    /// unseeded original, whose every parameter field is an Entry.
    fn summary(&self, u: &LirUnit<Value>, table: &LayoutTable) -> FieldSummary {
        let entry = |code: &Code| -> Option<(usize, FieldPath)> {
            let Code::Entry(node) = *code else {
                return None;
            };
            u.parameters.iter().enumerate().find_map(|(j, p)| {
                let (start, shape) = self.values[p.index()];
                (start..start + self.shapes[shape].len())
                    .contains(&node)
                    .then(|| (j, self.shapes[shape][node - start].clone()))
            })
        };
        let mut carries = Vec::new();
        let returns = u
            .terminators
            .iter()
            .filter_map(|t| match t {
                Terminator::Return(Some(v)) => Some(*v),
                _ => None,
            })
            .collect::<Vec<_>>();
        if let Some(&first) = returns.first() {
            for path in self.value_paths(first) {
                let code = returns.iter().fold(Code::Pending, |c, v| {
                    c.join(self.code(self.value(*v, path)))
                });
                if let Some((parameter, from)) = entry(&code) {
                    carries.push(Carry {
                        exit: Exit::Result,
                        path: path.clone(),
                        parameter,
                        from,
                    });
                }
            }
        }
        let abi = &table.fn_abis[u.header.abi.0 as usize];
        for (j, parameter) in abi.parameters.iter().enumerate() {
            if parameter.convention != Passing::Mut {
                continue;
            }
            let Some(slot) = u
                .slots
                .iter()
                .position(|s| s.kind == SlotKind::Parameter(j as u32))
            else {
                continue;
            };
            let slot = SlotId(slot as u32);
            for path in self.slot_paths(slot) {
                if let Some((parameter, from)) = entry(self.code(self.slot(slot, path))) {
                    carries.push(Carry {
                        exit: Exit::Parameter(j),
                        path: path.clone(),
                        parameter,
                        from,
                    });
                }
            }
        }
        carries.sort();
        FieldSummary { carries }
    }
}

/// Equations under construction: a constant and the joined inputs per node.
struct Equations {
    constants: Vec<Code>,
    defined: Vec<bool>,
    inputs: Vec<Vec<usize>>,
}
impl Equations {
    fn link(&mut self, to: usize, from: Option<usize>) {
        let Some(from) = from else {
            return self.set(to, Code::Dynamic);
        };
        if !std::mem::replace(&mut self.defined[to], true) {
            self.constants[to] = Code::Pending;
        }
        self.inputs[to].push(from);
    }
    fn set(&mut self, to: usize, code: Code) {
        self.defined[to] = true;
        self.constants[to] = code;
    }
}

/// Each equation is a constant or a join of incoming SSA identities. Every
/// node widens at most three times (pending -> entry -> known -> dynamic),
/// including cycles. Every parameter field not seeded is its own Entry.
fn discover(
    u: &LirUnit<Value>,
    table: &LayoutTable,
    entries: &HashMap<Symbol, CallableEntry>,
    seeds: &[ValueSeed],
) -> Facts {
    hirsel_intern::timings::count(hirsel_intern::timings::Count::ClosureBodyScans);
    let mut shapes = Vec::new();
    let mut by_layout = HashMap::new();
    let mut next = 0;
    let mut node = |ty: LirTypeId, shapes: &mut Vec<Vec<FieldPath>>| {
        let layout = u.types[ty.index()].layout;
        let shape = *by_layout.entry(layout).or_insert_with(|| {
            shapes.push(callable_paths(table, layout));
            shapes.len() - 1
        });
        let start = next;
        next += shapes[shape].len();
        (start, shape)
    };
    let values = u
        .values
        .iter()
        .map(|v| node(v.ty, &mut shapes))
        .collect::<Vec<_>>();
    let slots = u
        .slots
        .iter()
        .map(|s| node(s.ty, &mut shapes))
        .collect::<Vec<_>>();
    let mut facts = Facts {
        values,
        slots,
        shapes,
        codes: Vec::new(),
    };
    let n = next;
    let mut eq = Equations {
        constants: vec![Code::Dynamic; n],
        defined: vec![false; n],
        inputs: vec![Vec::new(); n],
    };
    let nodes = |facts: &Facts, v: ValueId| {
        let (start, shape) = facts.values[v.index()];
        (start..).zip(facts.shapes[shape].clone())
    };
    // A closure that captures a place mutably writes it without a version;
    // its mutably loaned slots are then unknown.
    let mut untracked = BTreeSet::new();
    if u.tables.closures.iter().any(|c| {
        c.captures
            .get(&u.pools.captures)
            .unwrap_or(&[])
            .iter()
            .any(|c| c.usage == CaptureUse::Mut)
    }) {
        for loan in &u.tables.loans {
            if loan.mutable
                && let Some((s, _)) = place(u, loan.address)
            {
                untracked.insert(s);
            }
        }
    }
    let tracked = |address: AddressId| place(u, address).filter(|(s, _)| !untracked.contains(s));
    let mut versions = vec![Vec::new(); u.slots.len()];
    // A write at `address` defines `version`: covered paths take `value`'s
    // fields (or the callee's carries), sibling fields keep the slot's.
    let write = |facts: &Facts,
                 eq: &mut Equations,
                 versions: &mut Vec<Vec<ValueId>>,
                 version: ValueId,
                 address: AddressId,
                 inside: &dyn Fn(&[u32]) -> Option<usize>| {
        let Some((slot, steps)) = tracked(address) else {
            for (node, _) in nodes(facts, version) {
                eq.set(node, Code::Dynamic);
            }
            return;
        };
        versions[slot.index()].push(version);
        for (node, path) in nodes(facts, version) {
            match overlap(&path, &steps) {
                Overlap::Inside(n) => eq.link(node, inside(&path[n..])),
                Overlap::Apart => eq.link(node, facts.slot(slot, &path)),
                Overlap::Unknown => eq.set(node, Code::Dynamic),
            }
        }
    };
    for edge in &u.tables.edges {
        for (&p, &a) in u.blocks[edge.target.index()]
            .parameters
            .get(&u.pools.values)
            .unwrap_or(&[])
            .iter()
            .zip(edge.arguments.get(&u.pools.values).unwrap_or(&[]))
        {
            for (node, path) in nodes(&facts, p) {
                eq.link(node, facts.value(a, &path));
            }
        }
    }
    for inst in &u.instructions {
        match *inst {
            Inst::Read {
                result,
                address,
                reaching,
                ..
            }
            | Inst::Copy {
                result,
                address,
                reaching,
                ..
            } => {
                let q = tracked(address).and_then(|(_, steps)| fields(&steps));
                for (node, path) in nodes(&facts, result) {
                    let from = q.as_ref().and_then(|q| {
                        facts.value(reaching, &[q.as_slice(), path.as_slice()].concat())
                    });
                    eq.link(node, from);
                }
            }
            Inst::Store {
                address,
                value,
                version,
            } => write(&facts, &mut eq, &mut versions, version, address, &|rest| {
                facts.value(value, rest)
            }),
            Inst::Replace {
                address,
                value,
                displaced,
                version,
                displaced_version,
                ..
            } => {
                let q = tracked(address).and_then(|(slot, steps)| Some((slot, fields(&steps)?)));
                if !untracked.contains(&displaced) {
                    versions[displaced.index()].push(displaced_version);
                }
                for (node, path) in nodes(&facts, displaced_version) {
                    let from = q.as_ref().and_then(|(slot, q)| {
                        facts.slot(*slot, &[q.as_slice(), path.as_slice()].concat())
                    });
                    eq.link(node, from);
                }
                write(&facts, &mut eq, &mut versions, version, address, &|rest| {
                    facts.value(value, rest)
                });
            }
            Inst::SeqReserve {
                address, version, ..
            }
            | Inst::ArenaPush {
                address, version, ..
            }
            | Inst::SeqSetLen {
                address, version, ..
            }
            | Inst::SeqAppend {
                address, version, ..
            }
            | Inst::FlatAppend {
                address, version, ..
            }
            | Inst::FlatField {
                address, version, ..
            }
            | Inst::FlatClose {
                address, version, ..
            } => write(&facts, &mut eq, &mut versions, version, address, &|_| None),
            Inst::Weaken { result, value }
            | Inst::UniqueCopy { result, value, .. }
            | Inst::CopyValue { result, value } => {
                for (node, path) in nodes(&facts, result) {
                    eq.link(node, facts.value(value, &path));
                }
            }
            Inst::Construct {
                result, construct, ..
            } => {
                let site = &u.tables.constructs[construct.index()];
                let values = site.fields.get(&u.pools.fields).unwrap_or(&[]);
                let aggregate = matches!(
                    site.constructor,
                    ConstructorKind::Tuple | ConstructorKind::Record(_)
                );
                for (node, path) in nodes(&facts, result) {
                    let from = path
                        .split_first()
                        .filter(|_| aggregate)
                        .and_then(|(f, rest)| {
                            let field = values.iter().find(|v| v.field == *f)?;
                            facts.value(field.value, rest)
                        });
                    eq.link(node, from);
                }
            }
            Inst::Closure {
                result, closure, ..
            } => {
                if let Some(code) = u.tables.closures[closure.index()].code {
                    eq.set(
                        facts.values[result.index()].0,
                        Code::Known(Target {
                            symbol: u.tables.symbols[code.symbol.index()].clone(),
                            abi: code.abi,
                            environment: true,
                        }),
                    );
                }
            }
            Inst::FunctionRef { result, symbol } => {
                let s = &u.tables.symbols[symbol.index()];
                if let Some(entry) = entries.get(s) {
                    eq.set(
                        facts.values[result.index()].0,
                        Code::Known(Target {
                            symbol: s.clone(),
                            abi: entry.abi,
                            environment: false,
                        }),
                    );
                }
            }
            _ => {}
        }
    }
    for site in &u.tables.calls {
        let callee = match site.callee {
            Callee::Direct(s) => entries
                .get(&u.tables.symbols[s.index()])
                .map(|e| (&e.fields, &table.fn_abis[e.abi.0 as usize])),
            _ => None,
        };
        let args = site.arguments.get(&u.pools.call_arguments).unwrap_or(&[]);
        let position = |arg: &CallArgument| {
            let (_, abi) = callee?;
            abi.parameters
                .iter()
                .position(|p| p.source == arg.parameter)
        };
        let carried = |exit: Exit, path: &[u32]| {
            let (summary, _) = callee?;
            let (j, from) = summary.carried(exit, path)?;
            let arg = args.iter().find(|a| position(a) == Some(j))?;
            facts.argument(u, arg, from)
        };
        if let Some(result) = site.result {
            for (node, path) in nodes(&facts, result) {
                eq.link(node, carried(Exit::Result, &path));
            }
        }
        for arg in args {
            let ArgumentValue::Address { address, version } = arg.value else {
                continue;
            };
            let p = position(arg);
            write(&facts, &mut eq, &mut versions, version, address, &|rest| {
                carried(Exit::Parameter(p?), rest)
            });
        }
    }
    for (s, slot) in u.slots.iter().enumerate() {
        let initial = match slot.kind {
            SlotKind::Parameter(i) => u.parameters.get(i as usize).copied(),
            SlotKind::Capture(i) => u.captures.get(i as usize).copied(),
            SlotKind::Local | SlotKind::Temporary | SlotKind::Displaced => None,
            _ => {
                for (node, _) in (facts.slots[s].0..).zip(facts.slot_paths(SlotId(s as u32))) {
                    eq.set(node, Code::Dynamic);
                }
                continue;
            }
        };
        let slot = SlotId(s as u32);
        for (i, path) in facts.slot_paths(slot).iter().enumerate() {
            let node = facts.slots[s].0 + i;
            if untracked.contains(&slot) {
                eq.set(node, Code::Dynamic);
                continue;
            }
            for &version in initial.iter().chain(&versions[s]) {
                eq.link(node, facts.value(version, path));
            }
        }
    }
    for &p in &u.parameters {
        for (node, _) in nodes(&facts, p) {
            eq.set(node, Code::Entry(node));
        }
    }
    for (value, path, target) in seeds {
        if let Some(node) = facts.value(*value, path) {
            eq.set(node, Code::Known(target.clone()));
        }
    }
    let Equations {
        constants, inputs, ..
    } = eq;
    let mut users = vec![Vec::new(); n];
    for (i, args) in inputs.iter().enumerate() {
        for &a in args {
            users[a].push(i);
        }
    }
    let mut codes = constants.clone();
    let mut queue: VecDeque<_> = (0..n).collect();
    let mut queued = vec![true; n];
    while let Some(i) = queue.pop_front() {
        queued[i] = false;
        let code = inputs[i]
            .iter()
            .fold(constants[i].clone(), |c, &v| c.join(&codes[v]));
        if code != codes[i] {
            codes[i] = code;
            for &user in &users[i] {
                if !std::mem::replace(&mut queued[user], true) {
                    queue.push_back(user);
                }
            }
        }
    }
    facts.codes = codes;
    facts
}

/// Original instance for each unit (specializations use its placement and
/// signature proofs), and source units replicated for partition-independent inlining.
pub struct Specializations {
    pub origins: Vec<InstanceId>,
    /// Every unit's own instance key: an original's, or the ordinary derived
    /// key its specialization was requested under. Its digest is the unit's
    /// instance.
    pub keys: Vec<hirsel_layout::instance::InstanceKey>,
    pub inline: BTreeSet<ContentHash>,
}

impl Specializations {
    /// Specializations append Value bodies, not source instances. Every body
    /// must retain an original callable/placement proof owner before import
    /// publishes the lowering to placement and insertion.
    pub(crate) fn assert_origins(&self, lowered: &Lowered) {
        let instances = &lowered.instances;
        let n = instances.len();
        assert_eq!(instances.roles.len(), n);
        assert_eq!(instances.callables.len(), n);
        assert_eq!(instances.abis.len(), n);
        assert_eq!(instances.actuals.len(), n);
        assert_eq!(instances.environments.len(), n);
        assert_eq!(self.origins.len(), lowered.units.len());
        assert_eq!(self.keys.len(), lowered.units.len());
        for ((unit, &origin), key) in lowered.units.iter().zip(&self.origins).zip(&self.keys) {
            let view = instances.view(origin);
            instances.callable(origin);
            assert_eq!(&unit.header.definition, view.key.definition());
            assert_eq!(unit.header.instance, key.digest());
        }
        for id in instances.ids() {
            assert_eq!(self.origins[id.index()], id);
        }
    }
}

pub fn specialize_scoped(
    lowered: &mut Lowered,
    program: &crate::instance::Program,
    target: &hirsel_lir::TargetSpec,
    scoped: bool,
) -> Specializations {
    let mut result = specialize(lowered);
    let registry = crate::import::invocation_registry(
        program,
        &lowered.instances,
        &lowered.semantic_owners,
        &lowered.units,
        &result.origins,
    );
    crate::import::prune_pretransfer(
        &mut lowered.units,
        &lowered.table,
        &registry.pretransfer_adapters,
    );
    if scoped {
        crate::scoped_consumer_9::specialize(lowered, program, target, &mut result);
    }
    result
}

/// Rewrite one selected Value using declaration facts and known callable
/// seeds. The request sink registers derived-instance metadata only: discovery
/// never opens another body or executes a dependency. A refused request leaves
/// that direct call intact. The caller owns deterministic specialization caps.
pub fn rewrite_instance(
    unit: &mut LirUnit<Value>,
    table: &LayoutTable,
    entries: &HashMap<Symbol, CallableEntry>,
    seeds: &[CallableSeed],
    spawn_flags: u64,
    request_specialization: impl FnMut(&CallableEntry, &CallableRequest) -> bool,
) -> Result<BTreeSet<ContentHash>, String> {
    let seeds = bind_seeds(unit, seeds)?;
    Ok(rewrite_seeded_instance(
        unit,
        table,
        entries,
        &seeds,
        spawn_flags,
        request_specialization,
    ))
}

/// The owned task_main adapter's first capture carries callable code while
/// retaining its original environment, move mask and TaskFinish ownership.
pub fn rewrite_task_capture(
    unit: &mut LirUnit<Value>,
    table: &LayoutTable,
    entries: &HashMap<Symbol, CallableEntry>,
    request: &TaskCaptureRequest,
    request_specialization: impl FnMut(&CallableEntry, &CallableRequest) -> bool,
) -> Result<BTreeSet<ContentHash>, String> {
    if !unit.header.definition.is_core("$native.task_main")
        || unit.header.definition != *request.key.definition()
    {
        return Err("captured callable request is not the selected task_main adapter".into());
    }
    let value = *unit
        .captures
        .first()
        .ok_or("task_main has no callable capture")?;
    Ok(rewrite_seeded_instance(
        unit,
        table,
        entries,
        &[(value, Vec::new(), request.target.clone())],
        0,
        request_specialization,
    ))
}

/// The sole callable rewrite of one selected body, for the whole-program
/// transform and each independently bound demand window alike. A closure
/// whose captures hold known code is redirected to a body specialized on
/// them (task_main through its own owned-capture request); then calls are
/// rewritten. Also returns the body's field summary, which only an unseeded
/// original may publish. Task requests carry code identity across windows;
/// ABI ids stay local.
#[allow(clippy::too_many_arguments)]
pub fn rewrite_window(
    unit: &mut LirUnit<Value>,
    table: &LayoutTable,
    entries: &HashMap<Symbol, CallableEntry>,
    seeds: &[ValueSeed],
    spawn_flags: u64,
    mut task: impl FnMut(&CallableEntry, &TaskCaptureRequest) -> bool,
    mut specialization: impl FnMut(&CallableEntry, &CallableRequest) -> bool,
) -> Result<(BTreeSet<ContentHash>, FieldSummary), String> {
    let facts = discover(unit, table, entries, seeds);
    let summary = facts.summary(unit, table);
    let mut results = HashMap::new();
    for inst in &unit.instructions {
        if let Inst::Closure {
            result, closure, ..
        } = *inst
        {
            results.insert(closure, result);
        }
    }
    let mut inline = BTreeSet::new();
    for closure in 0..unit.tables.closures.len() {
        let site = &unit.tables.closures[closure];
        let Some(code) = site.code else { continue };
        let Some(entry) = entries.get(&unit.tables.symbols[code.symbol.index()]) else {
            continue;
        };
        let captures = site.captures.get(&unit.pools.captures).unwrap_or(&[]);
        let digest = if entry.key.definition().is_core("$native.task_main") {
            let Some(Code::Known(target)) = captures.first().map(|c| facts.root(c.value)) else {
                continue;
            };
            let request = TaskCaptureRequest::new(&entry.key, target.clone())?;
            if !task(entry, &request) {
                continue;
            }
            request.key.digest()
        } else {
            // An iterator's captures are step state, which its steps rewrite;
            // only an ordinary callable's environment is fixed at the site.
            let callable = results
                .get(&ClosureId(closure as u32))
                .and_then(|v| table.layout(unit.types[unit.values[v.index()].ty.index()].layout))
                .is_some_and(|l| matches!(l.kind, LayoutKind::Callable { .. }));
            if entry.origin != UnitOrigin::Source || !callable {
                continue;
            }
            let mut seeds = Vec::new();
            for (c, capture) in captures.iter().enumerate() {
                if capture.usage == CaptureUse::Mut {
                    continue;
                }
                for path in facts.value_paths(capture.value) {
                    if let Code::Known(target) = facts.code(facts.value(capture.value, path)) {
                        seeds.push(CallableSeed {
                            input: SeedInput::Capture(c),
                            path: path.clone(),
                            target: target.clone(),
                        });
                    }
                }
            }
            if seeds.is_empty() {
                continue;
            }
            let request = CallableRequest {
                key: request::specialization_key(&entry.key, &seeds, 0),
                seeds,
                spawn_flags: 0,
            };
            if !specialization(entry, &request) {
                continue;
            }
            request.key.digest()
        };
        inline.insert(digest);
        let selected = Symbol {
            kind: SymbolKind::Function,
            definition: entry.key.definition().clone(),
            instance: Some(digest),
        };
        let selected = symbol(unit, &selected);
        unit.tables.closures[closure].code.as_mut().unwrap().symbol = selected;
    }
    inline.extend(rewrite_seeded_instance(
        unit,
        table,
        entries,
        seeds,
        spawn_flags,
        &mut specialization,
    ));
    Ok((inline, summary))
}

fn rewrite_seeded_instance(
    unit: &mut LirUnit<Value>,
    table: &LayoutTable,
    entries: &HashMap<Symbol, CallableEntry>,
    seeds: &[ValueSeed],
    spawn_flags: u64,
    mut request_specialization: impl FnMut(&CallableEntry, &CallableRequest) -> bool,
) -> BTreeSet<ContentHash> {
    let mut inline = BTreeSet::new();
    let facts = discover(unit, table, entries, seeds);
    if spawn_flags != 0 {
        crate::spawn::set_flags(unit, spawn_flags);
    }
    for call in 0..unit.tables.calls.len() {
        let site = unit.tables.calls[call].clone();
        match site.callee {
            Callee::Indirect(value) => {
                let Code::Known(target) = facts.root(value) else {
                    continue;
                };
                let expected = &table.fn_abis[site.abi.0 as usize];
                let actual = &table.fn_abis[target.abi.0 as usize];
                if expected.context != actual.context
                    || expected.destination != actual.destination
                    || expected.parameters != actual.parameters
                    || expected.result != actual.result
                {
                    continue;
                }
                let receipts = match (&site.receipts, &actual.receipts) {
                    (None, Some(contract))
                        if contract.parameters.is_empty()
                            && contract.result.is_none()
                            && contract.captures.is_empty()
                            && contract.inventory.iter().all(|grant| {
                                matches!(
                                    grant,
                                    hirsel_lir::sealed_payload_source::ReceiptGrant::Parameter(_)
                                )
                            }) =>
                    {
                        Some(hirsel_lir::receipt_transport::ReceiptCall {
                            contract: contract.content().expect("selected source contract"),
                            arguments: vec![],
                            result: None,
                        })
                    }
                    _ if expected.receipts == actual.receipts => site.receipts.clone(),
                    _ => continue,
                };
                // Iterator steps use a different physical callable layout;
                // Bound currently describes only ordinary closure callables.
                if !matches!(
                    table
                        .layout(unit.types[unit.values[value.index()].ty.index()].layout)
                        .map(|l| &l.kind),
                    Some(hirsel_lir::layout_types::LayoutKind::Callable { .. })
                ) {
                    continue;
                }
                let u = &mut *unit;
                let code = symbol(u, &target.symbol);
                u.tables.calls[call].callee = if target.environment {
                    Callee::Bound {
                        code,
                        closure: value,
                    }
                } else {
                    Callee::Direct(code)
                };
                u.tables.calls[call].abi = target.abi;
                u.tables.calls[call].receipts = receipts;
                if target.environment
                    && let Some(digest) = target.symbol.instance
                {
                    inline.insert(digest);
                }
            }
            Callee::Direct(s) => {
                let Some(source) = entries.get(&unit.tables.symbols[s.index()]) else {
                    continue;
                };
                // Library units are generated templates; source specializations
                // retain source failure anchors and external entry identity.
                // A core higher-order body whose callback is known at the call
                // is specialized like a source body (the collector's
                // `visit_fn_use`): its callback call becomes direct, so the
                // continuation plan can nest the callback's frame.
                let definition = source.key.definition();
                let spawn = definition.is_core("task.async");
                if source.origin != UnitOrigin::Source
                    && !spawn
                    && !HIGHER_ORDER.iter().any(|name| definition.is_core(name))
                {
                    continue;
                }
                let bound = facts.arguments(unit, &table.fn_abis[source.abi.0 as usize], &site);
                if bound.is_empty() {
                    continue;
                }
                let mut flags = 0;
                if spawn {
                    if crate::spawn::immediately_awaited(unit, call) {
                        flags |= crate::spawn::IMMEDIATE;
                    }
                    if bound.iter().any(|seed| {
                        seed.input == SeedInput::Parameter(1)
                            && seed.path.is_empty()
                            && entries
                                .get(&seed.target.symbol)
                                .is_some_and(|entry| entry.op_free)
                    }) {
                        flags |= crate::spawn::OP_FREE;
                    }
                }
                let effective = request::specialization_key(&source.key, &bound, flags);
                let digest = effective.digest();
                let request = CallableRequest {
                    key: effective,
                    seeds: bound,
                    spawn_flags: flags,
                };
                if !request_specialization(source, &request) {
                    continue;
                }
                let target = Symbol {
                    kind: SymbolKind::Function,
                    definition: source.key.definition().clone(),
                    instance: Some(digest),
                };
                let u = &mut *unit;
                u.tables.calls[call].callee = Callee::Direct(symbol(u, &target));
            }
            _ => {}
        }
    }
    inline
}

/// Existing code-target specialization without scoped capture elimination.
pub fn specialize(lowered: &mut Lowered) -> Specializations {
    let keys = lowered
        .instances
        .ids()
        .take(lowered.units.len())
        .map(|id| lowered.instances.view(id).key.clone())
        .collect::<Vec<_>>();
    specialize_units(&mut lowered.units, &lowered.table, &keys)
}

fn function(u: &LirUnit<Value>) -> Symbol {
    Symbol {
        kind: SymbolKind::Function,
        definition: u.header.definition.clone(),
        instance: Some(u.header.instance),
    }
}

/// Appended specializations of one transform, under its deterministic caps.
struct Appended<'a> {
    originals: &'a [LirUnit<Value>],
    units: &'a mut Vec<LirUnit<Value>>,
    origins: Vec<InstanceId>,
    keys: Vec<hirsel_layout::instance::InstanceKey>,
    seeds: Vec<Vec<ValueSeed>>,
    spawn_flags: Vec<u64>,
    counts: Vec<usize>,
    cache: BTreeSet<(usize, ContentHash)>,
    inline: BTreeSet<ContentHash>,
}
impl Appended<'_> {
    fn add(
        &mut self,
        original: usize,
        key: &hirsel_layout::instance::InstanceKey,
        seeds: &[CallableSeed],
        spawn_flags: u64,
    ) -> bool {
        let digest = key.digest();
        if self.cache.contains(&(original, digest)) {
            return true;
        }
        if self.counts[original] >= MAX_PER_FUNCTION
            || self.units.len() - self.originals.len() >= MAX_SPECIALIZATIONS
        {
            return false;
        }
        let mut u = self.originals[original].clone();
        let Ok(seeds) = bind_seeds(&u, seeds) else {
            return false;
        };
        self.counts[original] += 1;
        u.header.instance = digest;
        self.units.push(u);
        self.origins.push(InstanceId(original as u32));
        self.keys.push(key.clone());
        self.seeds.push(seeds);
        self.spawn_flags.push(spawn_flags);
        self.inline.insert(digest);
        self.cache.insert((original, digest));
        true
    }
}

/// Apply the same bounded callable rewrite to a complete, independently admitted
/// Value closure. Keys name the original proof owners in unit order. Originals
/// are rewritten callee-first by call-graph component, as a demand window
/// does, and a component's field summaries are published once it is complete.
pub fn specialize_units(
    units: &mut Vec<LirUnit<Value>>,
    table: &hirsel_lir::LayoutTable,
    original_keys: &[hirsel_layout::instance::InstanceKey],
) -> Specializations {
    assert_eq!(units.len(), original_keys.len());

    let originals = units.clone();
    let op_free = crate::spawn::op_free(&originals, table);
    let mut entries: HashMap<_, _> = originals
        .iter()
        .enumerate()
        .map(|(i, u)| {
            (
                function(u),
                CallableEntry {
                    key: original_keys[i].clone(),
                    abi: u.header.abi,
                    origin: u.header.origin,
                    op_free: op_free.get(&function(u)) == Some(&true),
                    fields: FieldSummary::default(),
                },
            )
        })
        .collect();
    let originals_index = originals
        .iter()
        .enumerate()
        .map(|(i, u)| (function(u), i))
        .collect::<HashMap<_, _>>();
    let index = originals
        .iter()
        .enumerate()
        .map(|(i, u)| ((u.header.definition.clone(), u.header.instance), i))
        .collect::<HashMap<_, _>>();
    let edges = originals
        .iter()
        .map(|u| {
            OperationFreedom::extract(u, table)
                .dependencies()
                .iter()
                .filter_map(|name| index.get(name).copied())
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let components = hirsel_graph::components(&edges);
    let mut component = vec![0; originals.len()];
    for (c, members) in components.iter().enumerate() {
        for &m in members {
            component[m] = c;
        }
    }
    let order = components.into_iter().flatten().collect::<Vec<_>>();
    let n = originals.len();
    let state = std::cell::RefCell::new(Appended {
        originals: &originals,
        units,
        origins: (0..n).map(|i| InstanceId(i as u32)).collect(),
        keys: original_keys.to_vec(),
        seeds: vec![Vec::new(); n],
        spawn_flags: vec![0; n],
        counts: vec![0; n],
        cache: BTreeSet::new(),
        inline: BTreeSet::new(),
    });
    let owner = |entry: &CallableEntry| {
        originals_index[&Symbol {
            kind: SymbolKind::Function,
            definition: entry.key.definition().clone(),
            instance: Some(entry.key.digest()),
        }]
    };
    let mut finished = Vec::new();
    let mut step = 0;
    while step < state.borrow().units.len() {
        let at = if step < n { order[step] } else { step };
        step += 1;
        let (mut unit, seeds, flags) = {
            let s = state.borrow();
            (s.units[at].clone(), s.seeds[at].clone(), s.spawn_flags[at])
        };
        let (direct, summary) = rewrite_window(
            &mut unit,
            table,
            &entries,
            &seeds,
            flags,
            |entry, request| {
                let seed = CallableSeed {
                    input: SeedInput::Capture(0),
                    path: Vec::new(),
                    target: request.target.clone(),
                };
                state
                    .borrow_mut()
                    .add(owner(entry), &request.key, &[seed], 0)
            },
            |entry, request| {
                state.borrow_mut().add(
                    owner(entry),
                    &request.key,
                    &request.seeds,
                    request.spawn_flags,
                )
            },
        )
        .expect("independently selected task_main capture");
        let mut s = state.borrow_mut();
        s.inline.extend(direct);
        s.units[at] = unit;
        if step <= n {
            finished.push((at, summary));
            if step == n || component[order[step]] != component[at] {
                for (i, summary) in finished.drain(..) {
                    entries.get_mut(&function(&originals[i])).unwrap().fields = summary;
                }
            }
        }
    }
    let state = state.into_inner();
    Specializations {
        origins: state.origins,
        keys: state.keys,
        inline: state.inline,
    }
}

pub use crate::spawn::{OperationFreedom, settle_operation_freedom};
