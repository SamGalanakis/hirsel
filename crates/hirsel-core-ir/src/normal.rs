//! PROG-10 normal form for checked instance modules.

use crate::decode::{Debit, DecodeError, DecodeResult};
use crate::source::SourcePositions;
use crate::*;
use hirsel_core_ids::{ContentHash, DefPath, ItemPath, PackageId};
use hirsel_core_layout::{Case, Field, FieldTy, FuncRef, Handle, Repr, TypeRef, Width};
use std::collections::BTreeSet;

pub(crate) fn uleb(bytes: &mut Vec<u8>, mut value: u64) {
    while value >= 128 {
        bytes.push(value as u8 | 128);
        value >>= 7;
    }
    bytes.push(value as u8);
}

pub fn encode_identity(identity: &Identity) -> Vec<u8> {
    let empty = Module::default();
    let mut order = ModuleOrder::new(&empty);
    let mut out = Write::module(&mut order);
    identity.write(&mut out);
    out.bytes
}

// This writes the original v1 closure tuple explicitly, including the fixed
// width count and tag fields. Closure identities retain their published bytes.
pub(crate) fn closure_hash(rows: &[ClosureRow]) -> ContentHash {
    let empty = Module::default();
    let mut order = ModuleOrder::new(&empty);
    let mut out = Write::module(&mut order);
    wire::encode_closure(rows, &mut out);
    ContentHash::of_parts("hirsel.core-closure.v1", [&out.bytes[..]])
}

pub(crate) fn decoded_closure_hash(
    rows: &[ClosureRow],
    budget: &Debit<'_>,
) -> DecodeResult<ContentHash> {
    let empty = Module::default();
    let meter = PlanMeter::new(budget);
    let bytes = {
        let mut order = ModuleOrder::metered(&empty, Some(&meter));
        let mut out = Write::module(&mut order);
        wire::encode_closure(rows, &mut out);
        out.bytes
    };
    meter.settle()?;
    Ok(ContentHash::of_parts(
        "hirsel.core-closure.v1",
        [&bytes[..]],
    ))
}

pub const VERSION: u16 = 3;
const BODY_MAGIC: &[u8] = b"HIRBODY\0\x03\x00";
pub const BODY_HASH_DOMAIN: &str = "hirsel.code-body.v3";
const ANNEX_MAGIC: &[u8] = b"HIRANNEX\0\x01\x00";
const SIGNATURE_DOMAIN: &str = "hirsel.code-signature.v2";

mod results;
use results::results;
mod charge;
pub(crate) mod wire;
use charge::{PlanMeter, reserve};
pub(crate) use charge::{charge_identity, charge_shape};
use wire::{Read, Tagged, Wire, Write};

/// An operation tag in the normal form consumed by both native engines.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EngineTag {
    Instruction(u8),
    Runtime(u8),
    Host(u8),
    Flow(u8),
    Terminator(u8),
}

impl EngineTag {
    pub fn is_defined(self) -> bool {
        match self {
            Self::Instruction(tag) => Inst::defines_tag(tag),
            Self::Runtime(tag) => RtOp::defines_tag(tag),
            Self::Host(tag) => HostOp::defines_tag(tag),
            Self::Flow(tag) => hirsel_core_layout::flow::Op::defines_tag(tag),
            Self::Terminator(tag) => Terminator::defines_tag(tag),
        }
    }
}

/// The frozen normal-form tag for an instruction.
pub fn instruction_tag(instruction: &Inst) -> u8 {
    instruction.tag()
}

/// The frozen normal-form tag for a terminator.
pub fn terminator_tag(terminator: &Terminator) -> u8 {
    terminator.tag()
}

/// The frozen normal-form tag for an engine runtime operation.
pub fn runtime_tag(operation: &RtOp) -> u8 {
    operation.tag()
}

/// The frozen normal-form tag inside an engine's host operation.
pub fn host_tag(operation: &HostOp) -> u8 {
    operation.tag()
}

/// The frozen normal-form tag inside an engine's flow operation.
pub fn flow_tag(operation: &hirsel_core_layout::flow::Op) -> u8 {
    operation.tag()
}

/// Fingerprint the closed ABI signature, independent of type row numbering.
pub fn signature(module: &Module, function: FuncId) -> ContentHash {
    let signature = module.functions[function.index()].signature;
    let mut order = ModuleOrder::new(module);
    let mut out = Write::module(&mut order);
    out.abi = true;
    module.signatures[signature.index()].write(&mut out);
    ContentHash::of_parts(SIGNATURE_DOMAIN, [&out.bytes[..]])
}

/// The canonical body and the function rows its code reaches outside the unit.
pub struct EncodedBody {
    pub bytes: Vec<u8>,
    /// Original function IDs in normal-form order.
    pub slots: Vec<Slot>,
    /// Descriptors of item roots referenced by retained code.
    pub described: Vec<Identity>,
    original_functions: Vec<usize>,
}

/// One implementation dependency, in first-use order.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum Slot {
    External(FuncId),
    Builtin(DefPath),
}

impl Slot {
    /// Resolve a canonical slot to the full instance identity used by the linker.
    pub fn identity(&self, module: &Module) -> Identity {
        match self {
            Self::External(function) => {
                module.identity(module.functions[function.index()].instance)
            }
            Self::Builtin(definition) => Identity::plain(definition.clone()),
        }
    }
}

impl EncodedBody {
    pub fn hash(&self) -> ContentHash {
        ContentHash::of_parts(BODY_HASH_DOMAIN, [self.bytes.as_slice()])
    }
}

#[derive(Default)]
struct Numbers {
    order: Vec<usize>,
    by_row: Vec<Option<u32>>,
}

impl Numbers {
    fn number(&mut self, row: usize) -> (u32, bool) {
        self.number_metered(row, None)
    }

    fn number_metered(&mut self, row: usize, meter: Option<&PlanMeter<'_>>) -> (u32, bool) {
        if row >= self.by_row.len() {
            let additional = row + 1 - self.by_row.len();
            if !reserve(meter, &mut self.by_row, additional) {
                return (0, false);
            }
            self.by_row.resize(row + 1, None);
        }
        let next = self.order.len() as u32;
        match self.by_row[row] {
            Some(number) => (number, false),
            None => {
                if !reserve(meter, &mut self.order, 1) {
                    return (0, false);
                }
                self.order.push(row);
                self.by_row[row] = Some(next);
                (next, true)
            }
        }
    }

    fn is_canonical(&self, len: usize) -> bool {
        self.order.iter().copied().eq(0..len)
    }

    fn get(&self, row: usize) -> u32 {
        self.by_row[row].expect("checked reference has a canonical definition")
    }
}

#[derive(Default)]
struct Table {
    numbers: Numbers,
    bytes: Vec<Vec<u8>>,
}

impl Table {
    fn append(&self, out: &mut Vec<u8>) {
        uleb(out, self.bytes.len() as u64);
        for row in &self.bytes {
            out.extend_from_slice(row);
        }
    }

    fn discover(&mut self, row: usize, meter: Option<&PlanMeter<'_>>) -> (u32, bool) {
        let (number, fresh) = self.numbers.number_metered(row, meter);
        if fresh && reserve(meter, &mut self.bytes, 1) {
            self.bytes.push(Vec::new());
        }
        (number, fresh)
    }
}

#[derive(Clone, Copy)]
enum GlobalRow {
    Type,
    Signature,
    Static,
    Instance,
}

#[derive(Clone, Copy)]
enum LocalRow {
    Slot,
    Address,
    Call,
    Edge,
}

struct ModuleOrder<'m> {
    module: &'m Module,
    meter: Option<&'m PlanMeter<'m>>,
    types: Table,
    signatures: Table,
    statics: Table,
    instances: Table,
    functions: Numbers,
    live_functions: Vec<bool>,
    declarations: Numbers,
    planning: bool,
    metadata: bool,
    layouts: Numbers,
    slots: Vec<Slot>,
    seen_slots: BTreeSet<Slot>,
    described: BTreeSet<Identity>,
    canonical_locals: bool,
}

impl<'m> ModuleOrder<'m> {
    fn new(module: &'m Module) -> Self {
        Self::metered(module, None)
    }

    fn metered(module: &'m Module, meter: Option<&'m PlanMeter<'m>>) -> Self {
        let mut live_functions = Vec::new();
        if meter.is_none_or(|meter| {
            meter
                .paid_by(&meter.live, |live| {
                    live.reserve(&mut live_functions, module.functions.len())
                })
                .is_some()
        }) {
            live_functions.resize(module.functions.len(), false);
        }
        Self {
            module,
            meter,
            types: Table::default(),
            signatures: Table::default(),
            statics: Table::default(),
            instances: Table::default(),
            functions: Numbers::default(),
            live_functions,
            declarations: Numbers::default(),
            planning: false,
            metadata: false,
            layouts: Numbers::default(),
            slots: Vec::new(),
            seen_slots: BTreeSet::new(),
            described: BTreeSet::new(),
            canonical_locals: true,
        }
    }

    fn table(&mut self, row: GlobalRow) -> &mut Table {
        match row {
            GlobalRow::Type => &mut self.types,
            GlobalRow::Signature => &mut self.signatures,
            GlobalRow::Static => &mut self.statics,
            GlobalRow::Instance => &mut self.instances,
        }
    }

    fn row(&mut self, kind: GlobalRow, row: usize) -> u32 {
        if self.planning && !self.metadata && matches!(kind, GlobalRow::Type) {
            self.retain_layout(row);
        }
        let meter = self.meter;
        let (number, fresh) = self.table(kind).discover(row, meter);
        if fresh && self.planning {
            if let Some(meter) = meter {
                meter.descend(|| self.write_row(kind, row));
            } else {
                self.write_row(kind, row);
            }
        }
        number
    }

    fn retain_layout(&mut self, root: usize) {
        let meter = self.meter;
        let mut pending = Vec::new();
        if reserve(meter, &mut pending, 1) {
            pending.push(root);
        }
        while let Some(row) = pending.pop() {
            if !self.layouts.number_metered(row, meter).1 {
                continue;
            }
            let original = &self.module.types[row];
            if meter.is_some_and(|meter| {
                meter
                    .do_with(|budget| charge_shape(original, budget))
                    .is_none()
            }) {
                break;
            }
            let mut shape = Shape::clone(original);
            shape.map_refs(&mut |ty| {
                if reserve(meter, &mut pending, 1) {
                    pending.push(ty.index());
                }
                ty
            });
        }
    }

    fn write_row(&mut self, kind: GlobalRow, row: usize) {
        let module = self.module;
        let mut out = Write::module(self);
        match kind {
            GlobalRow::Type => module.types[row].write(&mut out),
            GlobalRow::Signature => module.signatures[row].write(&mut out),
            GlobalRow::Static => module.statics[row].write(&mut out),
            GlobalRow::Instance => module.instances[row].write(&mut out),
        }
        if !out.module.planning {
            let bytes = out.bytes;
            let number = self.table(kind).numbers.get(row) as usize;
            self.table(kind).bytes[number] = bytes;
        }
    }

    fn function(&mut self, row: usize) -> u32 {
        if self.planning
            && let Some(live) = self.live_functions.get_mut(row)
        {
            *live = true;
        }
        self.functions.number_metered(row, self.meter).0
    }

    fn builtin_slot(&mut self, definition: &DefPath) {
        let meter = self.meter;
        // The slot's own identity, kept with the slots if it is new.
        let identity = meter.map(|meter| meter.slots.scratch());
        if let (Some(meter), Some(identity)) = (meter, &identity)
            && meter
                .paid_by(identity, |identity| {
                    charge_identity(definition, &[], identity)
                })
                .is_none()
        {
            return;
        }
        let slot = Slot::Builtin(definition.clone());
        if self.seen_slots.contains(&slot) {
            return;
        }
        if meter.is_some_and(|meter| {
            meter
                .do_with(|budget| {
                    budget.charge_slots(1)?;
                    charge_identity(definition, &[], budget)?;
                    // A insertion may create a leaf and split an ancestor.
                    budget.allocation::<Slot>(24)?;
                    budget.charge_bytes(256)
                })
                .is_none()
                || meter
                    .paid_by(&meter.slots, |slots| slots.reserve(&mut self.slots, 1))
                    .is_none()
        }) {
            return;
        }
        self.seen_slots.insert(slot.clone());
        self.slots.push(slot);
        if let (Some(meter), Some(identity)) = (meter, identity) {
            meter.slots.absorb(identity);
        }
    }
}

struct BodyOrder<'m> {
    body: &'m Body,
    blocks: Numbers,
    values: Numbers,
    slots: Table,
    addresses: Table,
    calls: Table,
    edges: Table,
}

impl<'m> BodyOrder<'m> {
    fn new(body: &'m Body) -> Self {
        Self::metered(body, None)
    }

    fn metered(body: &'m Body, meter: Option<&PlanMeter<'_>>) -> Self {
        let mut blocks = Numbers::default();
        let mut values = Numbers::default();
        let mut pending = Vec::new();
        if reserve(meter, &mut pending, 1) {
            pending.push(BlockId(0));
        }
        while let Some(id) = pending.pop() {
            let (_, fresh) = blocks.number_metered(id.index(), meter);
            if !fresh {
                continue;
            }
            let block = &body.blocks[id.index()];
            for &parameter in block.params.get(&body.pools.values).unwrap() {
                values.number_metered(parameter.index(), meter);
            }
            for inst in block.insts.get(&body.insts).unwrap() {
                for result in results(inst, body) {
                    values.number_metered(result.index(), meter);
                }
            }
            let before = pending.len();
            crate::ir::for_each_successor(body, id.index(), |next| {
                if reserve(meter, &mut pending, 1) {
                    pending.push(next);
                }
            });
            pending[before..].reverse();
        }
        Self {
            body,
            blocks,
            values,
            slots: Table::default(),
            addresses: Table::default(),
            calls: Table::default(),
            edges: Table::default(),
        }
    }

    fn table(&mut self, row: LocalRow) -> &mut Table {
        match row {
            LocalRow::Slot => &mut self.slots,
            LocalRow::Address => &mut self.addresses,
            LocalRow::Call => &mut self.calls,
            LocalRow::Edge => &mut self.edges,
        }
    }
}

/// A source position resolved against the current checked sources.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Site {
    pub file: String,
    pub line: u32,
    pub column: u32,
}

/// Sites and optimisation hints for a defined function, in body order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FunctionAnnex {
    pub instructions: Vec<Option<Site>>,
    pub terminators: Vec<Option<Site>>,
    pub cold_calls: Vec<bool>,
    pub cold_edges: Vec<bool>,
    pub callable_parameters: Vec<u32>,
}

/// A body's source positions and hints, indexed by canonical function number.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Annex(
    pub Vec<Option<FunctionAnnex>>,
    /// The entry's parameter names, used in startup diagnostics.
    pub Option<Vec<String>>,
);

impl Annex {
    pub fn encode(&self) -> Vec<u8> {
        let empty = Module::default();
        let mut order = ModuleOrder::new(&empty);
        let mut out = Write::module(&mut order);
        out.bytes.extend_from_slice(ANNEX_MAGIC);
        out.count(self.0.len());
        for function in &self.0 {
            function.is_some().write(&mut out);
            if let Some(function) = function {
                function.instructions.write(&mut out);
                function.terminators.write(&mut out);
            }
        }
        for function in self.0.iter().flatten() {
            function.cold_calls.write(&mut out);
            function.cold_edges.write(&mut out);
            out.count(function.callable_parameters.len());
            for &parameter in &function.callable_parameters {
                out.count(parameter as usize);
            }
        }
        self.1.write(&mut out);
        out.bytes
    }

    /// Read the annex sections and check their shape against the decoded body.
    pub fn decode(bytes: &[u8], body: &Module, budget: &Debit<'_>) -> DecodeResult<Self> {
        let mut input = Read::new(bytes, ANNEX_MAGIC, budget)?;
        let len = input.count()?;
        if len != body.functions.len() {
            return Err(DecodeError::Malformed("annex does not match body"));
        }
        let mut functions = input.read_vec(len, |input| {
            bool::read(input)?
                .then(|| {
                    Ok(FunctionAnnex {
                        instructions: Wire::read(input)?,
                        terminators: Wire::read(input)?,
                        cold_calls: vec![],
                        cold_edges: vec![],
                        callable_parameters: vec![],
                    })
                })
                .transpose()
        })?;
        for function in functions.iter_mut().flatten() {
            function.cold_calls = Wire::read(&mut input)?;
            function.cold_edges = Wire::read(&mut input)?;
            let len = input.count()?;
            function.callable_parameters = input.read_vec(len, Read::index)?;
        }
        let names: Option<Vec<String>> = Wire::read(&mut input)?;
        input.end()?;
        let names_match = names.as_ref().is_none_or(|names| {
            body.functions.first().is_some_and(|entry| {
                names.len() == body.signatures[entry.signature.index()].params.len()
            })
        });
        let matches = names_match
            && functions.len() == body.functions.len()
            && functions
                .iter()
                .zip(&body.functions)
                .all(|(annex, function)| match (annex, &function.body) {
                    (None, None) => true,
                    (Some(annex), Some(body)) => {
                        let params = body.blocks[0].params.get(&body.pools.values).unwrap();
                        annex.instructions.len() == body.insts.len()
                            && annex.terminators.len() == body.terminators.len()
                            && annex.cold_calls.len() == body.calls.len()
                            && annex.cold_edges.len() == body.edges.len()
                            && annex.callable_parameters.windows(2).all(|v| v[0] < v[1])
                            && annex.callable_parameters.iter().all(|&v| {
                                params.contains(&ValueId(v)) && body.values[v as usize] == Ty::Ptr
                            })
                            && annex
                                .instructions
                                .iter()
                                .chain(&annex.terminators)
                                .flatten()
                                .all(|site| site.line != 0 && site.column != 0)
                    }
                    _ => false,
                });
        if !matches {
            return Err(DecodeError::Malformed("annex does not match body"));
        }
        Ok(Self(functions, names))
    }

    pub fn hash(&self) -> ContentHash {
        ContentHash::of_parts("hirsel.code-annex.v1", [&self.encode()[..]])
    }
}

/// Encode an instance's checked IR, keeping source and optimisation data out.
pub fn encode(module: &Module, root: FuncId) -> EncodedBody {
    encode_with_sources(module, root, None).0
}

/// Encode an instance's checked IR and collect its annex in the same numbering.
pub fn encode_with_annex(
    module: &Module,
    root: FuncId,
    sources: &SourcePositions<'_>,
) -> (EncodedBody, Annex) {
    let (body, annex) = encode_with_sources(module, root, Some(sources));
    (body, Annex(annex, None))
}

/// Prepare the canonical module and its diagnostic data for assembly.
pub fn prepare_assembly(
    module: &Module,
    root: FuncId,
    sources: &SourcePositions<'_>,
) -> (EncodedBody, Annex, Module) {
    let (body, annex) = encode_with_annex(module, root, sources);
    let (module, _) = decode_payload(&body.bytes, &Debit::unlimited())
        .expect("the producer's normal form is readable");
    (body, annex, module)
}

/// `prepare_assembly` for a module `retain` produced, given the body bytes it
/// returned: the retained module is already in canonical numbering, so its
/// annex is read off in row order and the body is not encoded again.
pub fn prepare_retained_assembly(
    bytes: &[u8],
    retained: &Module,
    sources: &SourcePositions<'_>,
) -> (Annex, Module) {
    let functions = retained
        .functions
        .iter()
        .map(|function| {
            let body = function.body.as_ref()?;
            let rows = |len: usize| (0..len).collect::<Vec<_>>();
            Some(annex_in_order(
                retained,
                body,
                &rows(body.blocks.len()),
                |value| value as u32,
                &rows(body.calls.len()),
                &rows(body.edges.len()),
                sources,
            ))
        })
        .collect();
    let (module, _) =
        decode_payload(bytes, &Debit::unlimited()).expect("the producer's normal form is readable");
    (Annex(functions, None), module)
}

/// Materialize the retained module for existing native and evaluator consumers.
/// Source anchors and hints keep their original facts in the canonical numbering.
pub fn retain(module: &Module, root: FuncId) -> (EncodedBody, Module) {
    let sources = crate::source::SourceMap::default();
    let (mut code, annex) = encode_with_annex(module, root, &sources.positions());
    let mut retained = decode_payload(&code.bytes, &Debit::unlimited())
        .expect("our own normal body decodes")
        .0;
    retained.items.clone_from(&module.items);
    let mut functions = Numbers::default();
    for (&row, (function, hints)) in code
        .original_functions
        .iter()
        .zip(retained.functions.iter_mut().zip(annex.0))
    {
        functions.number(row);
        let (Some(body), Some(hints)) = (&mut function.body, hints) else {
            continue;
        };
        let original = module.functions[row]
            .body
            .as_ref()
            .expect("retained function has a producer");
        let order = BodyOrder::new(original);
        for (block, &old) in body.blocks.iter_mut().zip(&order.blocks.order) {
            let old = &original.blocks[old];
            block.anchor = old.anchor;
            let source = PoolRange::new(old.insts.start, old.insts.len);
            let target = block.insts.start as usize;
            let source = source.get(&original.inst_anchors).unwrap();
            body.inst_anchors[target..target + source.len()].copy_from_slice(source);
        }
        for (call, cold) in body.calls.iter_mut().zip(hints.cold_calls) {
            call.cold = cold;
        }
        for (edge, cold) in body.edges.iter_mut().zip(hints.cold_edges) {
            edge.cold = cold;
        }
        body.callable_parameters = hints.callable_parameters.into_iter().map(ValueId).collect();
    }
    for slot in &mut code.slots {
        if let Slot::External(f) = slot {
            *f = FuncId(functions.get(f.index()));
        }
    }
    (code, retained)
}

fn encode_with_sources(
    module: &Module,
    root: FuncId,
    sources: Option<&SourcePositions<'_>>,
) -> (EncodedBody, Vec<Option<FunctionAnnex>>) {
    let mut order = plan(module, root);

    let reachable = order.functions.order.clone();
    let mut functions = Vec::new();
    let mut annex = Vec::new();
    for row in reachable {
        let body = order.live_functions[row]
            .then_some(&module.functions[row].body)
            .and_then(Option::as_ref)
            .map(BodyOrder::new);
        let (bytes, extras) = write_function(&mut order, row, body, sources);
        functions.push(bytes);
        if sources.is_some() {
            annex.push(extras);
        }
    }
    for kind in [
        GlobalRow::Type,
        GlobalRow::Signature,
        GlobalRow::Static,
        GlobalRow::Instance,
    ] {
        let rows = order.table(kind).numbers.order.clone();
        for row in rows {
            order.write_row(kind, row);
        }
    }
    let mut slots: Vec<_> = order
        .functions
        .order
        .iter()
        .filter_map(|&row| {
            (order.live_functions[row] && module.functions[row].body.is_none())
                .then_some(Slot::External(FuncId(row as u32)))
        })
        .collect();
    slots.append(&mut order.slots);
    let mut bytes = BODY_MAGIC.to_vec();
    order.types.append(&mut bytes);
    order.signatures.append(&mut bytes);
    order.statics.append(&mut bytes);
    order.instances.append(&mut bytes);
    uleb(&mut bytes, functions.len() as u64);
    for function in functions {
        bytes.extend(function);
    }
    let encoded = EncodedBody {
        bytes,
        slots,
        described: order.described.into_iter().collect(),
        original_functions: order.functions.order,
    };
    (encoded, annex)
}

fn planned_closure(
    module: &Module,
    ty: TypeId,
    meter: Option<&PlanMeter<'_>>,
) -> Option<ContentHash> {
    match meter {
        None => Some(module.closure(ty).hash),
        Some(meter) => {
            meter.do_with(|budget| metered_closure_hash(module, ty, ClosureKind::Full, budget))
        }
    }
}

/// The hash of row `ty`'s closure, its working set charged only while it is live.
fn metered_closure_hash(
    module: &Module,
    ty: TypeId,
    kind: ClosureKind,
    budget: &Debit<'_>,
) -> DecodeResult<ContentHash> {
    let work = budget.scratch();
    Ok(module.closure_impl(ty, kind, Some(&work))?.hash)
}

fn plan(module: &Module, root: FuncId) -> ModuleOrder<'_> {
    plan_metered(module, root, None)
}

fn plan_metered<'m>(
    module: &'m Module,
    root: FuncId,
    meter: Option<&'m PlanMeter<'m>>,
) -> ModuleOrder<'m> {
    let mut order = ModuleOrder::metered(module, meter);
    order.planning = true;
    order.function(root.index());
    let mut next = 0;
    let mut layouts_walked = 0;
    loop {
        while let Some(&row) = order.functions.order.get(next) {
            let body = module.functions[row]
                .body
                .as_ref()
                .map(|body| BodyOrder::metered(body, meter));
            write_function(&mut order, row, body, None);
            next += 1;
        }
        let mut finalizers = Vec::new();
        for &ty in &order.layouts.order[layouts_walked..] {
            if let Some(f) = module.types[ty].finalizer
                && !order.live_functions[f.0 as usize]
                && reserve(meter, &mut finalizers, 1)
                && let Some(hash) = planned_closure(module, TypeId(ty as u32), meter)
            {
                finalizers.push((hash, finalizers.len(), f));
            }
        }
        layouts_walked = order.layouts.order.len();
        if finalizers.is_empty() {
            break;
        }
        finalizers.sort_unstable_by_key(|&(hash, order, _)| (hash, order));
        for (_, _, f) in finalizers {
            order.function(f.0 as usize);
        }
    }
    let mut compact_types = Vec::new();
    for ty in order
        .functions
        .order
        .iter()
        .filter_map(|&row| module.functions[row].body.as_ref())
        .flat_map(|body| body.compact_roots.iter().map(|root| root.ty))
    {
        if reserve(meter, &mut compact_types, 1) {
            compact_types.push(ty);
        }
    }
    compact_types.sort_unstable();
    compact_types.dedup();
    let mut root_types = Vec::new();
    for ty in compact_types {
        if order
            .types
            .numbers
            .by_row
            .get(ty.index())
            .copied()
            .flatten()
            .is_none()
            && reserve(meter, &mut root_types, 1)
            && let Some(hash) = planned_closure(module, ty, meter)
        {
            root_types.push((hash, ty));
        }
    }
    root_types.sort_unstable();
    for &(_, ty) in &root_types {
        order.types.discover(ty.index(), meter);
    }
    for (_, ty) in root_types {
        if let Some(meter) = meter {
            meter.descend(|| order.write_row(GlobalRow::Type, ty.index()));
        } else {
            order.write_row(GlobalRow::Type, ty.index());
        }
    }
    for &declaration in &order.declarations.order {
        order.functions.number_metered(declaration, meter);
    }
    order.planning = false;
    order
}

fn write_function<'m>(
    order: &mut ModuleOrder<'m>,
    row: usize,
    body_order: Option<BodyOrder<'m>>,
    sources: Option<&SourcePositions<'_>>,
) -> (Vec<u8>, Option<FunctionAnnex>) {
    let module = order.module;
    let function = &module.functions[row];
    let mut out = Write::module(order);
    function.instance.write(&mut out);
    function.signature.write(&mut out);
    (out.module.live_functions[row] && function.versioned).write(&mut out);
    body_order.is_some().write(&mut out);
    let Some(body_order) = body_order else {
        return (out.bytes, None);
    };
    let body = body_order.body;
    out.local = Some(body_order);
    let mut blocks = Vec::new();
    if reserve(
        out.module.meter,
        &mut blocks,
        out.local().blocks.order.len(),
    ) {
        blocks.extend_from_slice(&out.local().blocks.order);
    }
    out.count(blocks.len());
    for block in blocks {
        let block_row = &body.blocks[block];
        block_row.params.write(&mut out);
        let insts = block_row.insts.get(&body.insts).unwrap();
        out.count(insts.len());
        for inst in insts {
            match *inst {
                Inst::BuiltinUse { ref definition }
                    if !out.module.planning || out.module.meter.is_some() =>
                {
                    out.module.builtin_slot(definition);
                }
                Inst::DescriptorRef { function, .. }
                    if !out.module.planning
                        && let RevisionRoot::Item(identity) = module.revision_root(function) =>
                {
                    out.module.described.insert(identity);
                }
                _ => {}
            }
            if out
                .module
                .meter
                .is_some_and(|meter| meter.do_with(|b| b.check()).is_none())
            {
                break;
            }
            inst.write(&mut out);
        }
        body.terminators[block].write(&mut out);
    }
    if !out.module.planning {
        let mut roots = body.compact_roots.clone();
        roots.sort_by_key(|root| (out.module.types.numbers.get(root.ty.index()), root.width));
        roots.dedup();
        roots.write(&mut out);
    }
    let local = out.local.take().unwrap();
    if out.module.planning {
        out.module.canonical_locals &= local.blocks.is_canonical(body.blocks.len())
            && local.values.is_canonical(body.values.len())
            && local.slots.numbers.is_canonical(body.slots.len())
            && local.addresses.numbers.is_canonical(body.addresses.len())
            && local.calls.numbers.is_canonical(body.calls.len())
            && local.edges.numbers.is_canonical(body.edges.len());
    }
    out.count(local.values.order.len());
    for &row in &local.values.order {
        body.values[row].write(&mut out);
    }
    if !out.module.planning {
        local.slots.append(&mut out.bytes);
        local.addresses.append(&mut out.bytes);
        local.calls.append(&mut out.bytes);
        local.edges.append(&mut out.bytes);
    }
    let annex = sources.map(|sources| function_annex(module, &local, sources));
    (out.bytes, annex)
}

fn function_annex(
    module: &Module,
    order: &BodyOrder<'_>,
    sources: &SourcePositions<'_>,
) -> FunctionAnnex {
    annex_in_order(
        module,
        order.body,
        &order.blocks.order,
        |value| order.values.get(value),
        &order.calls.numbers.order,
        &order.edges.numbers.order,
        sources,
    )
}

/// A function's annex with its blocks, calls and edges taken in the given
/// row orders and its values renumbered by `value`.
fn annex_in_order(
    module: &Module,
    body: &Body,
    blocks: &[usize],
    value: impl Fn(usize) -> u32,
    calls: &[usize],
    edges: &[usize],
    sources: &SourcePositions<'_>,
) -> FunctionAnnex {
    let site = |anchor: Option<Anchor>| {
        let anchor = anchor?;
        let (file, line, column) =
            sources.position(&module.items[anchor.item.index()], anchor.start)?;
        Some(Site {
            file: file.path.clone(),
            line,
            column,
        })
    };
    let mut callable_parameters: Vec<_> = body
        .callable_parameters
        .iter()
        .map(|parameter| value(parameter.index()))
        .collect();
    callable_parameters.sort_unstable();
    callable_parameters.dedup();
    let blocks = blocks.iter().map(|&row| &body.blocks[row]);
    FunctionAnnex {
        instructions: blocks
            .clone()
            .flat_map(|block| {
                PoolRange::new(block.insts.start, block.insts.len)
                    .get(&body.inst_anchors)
                    .unwrap()
                    .iter()
                    .copied()
                    .map(site)
            })
            .collect(),
        terminators: blocks.map(|block| site(block.anchor)).collect(),
        cold_calls: calls.iter().map(|&row| body.calls[row].cold).collect(),
        cold_edges: edges.iter().map(|&row| body.edges[row].cold).collect(),
        callable_parameters,
    }
}

/// Decode a body with canonical IDs and empty source and optimisation fields.
/// Function 0 is the instance's root. Source positions and hints are read
/// separately from the annex.
pub struct DecodedBody {
    pub module: Module,
    pub slots: Vec<Slot>,
    closures: Vec<Vec<ContentHash>>,
    abi_closures: Vec<Option<ContentHash>>,
    signatures: Vec<Option<ContentHash>>,
}

impl DecodedBody {
    /// The function's canonical identity. The closure hashes have already
    /// been checked during the decode.
    pub fn identity(&self, function: FuncId, budget: &Debit<'_>) -> DecodeResult<Identity> {
        let row = self
            .module
            .functions
            .get(function.index())
            .ok_or(DecodeError::Malformed("function outside normal form table"))?
            .instance
            .index();
        let key = &self.module.instances[row];
        charge_identity(&key.definition, &key.dictionaries, budget)?;
        budget.allocation::<ContentHash>(self.closures[row].len())?;
        Ok(Identity {
            definition: key.definition.clone(),
            dictionaries: key.dictionaries.clone(),
            closures: self.closures[row].clone(),
        })
    }

    /// Resolve the retained slot in its canonical order.
    pub fn slot_identity(&self, slot: usize, budget: &Debit<'_>) -> DecodeResult<Identity> {
        match self.slots.get(slot) {
            Some(Slot::External(function)) => self.identity(*function, budget),
            Some(Slot::Builtin(definition)) => {
                charge_identity(definition, &[], budget)?;
                Ok(Identity::plain(definition.clone()))
            }
            None => Err(DecodeError::Malformed("slot outside normal form table")),
        }
    }

    pub fn signature(&mut self, function: FuncId, budget: &Debit<'_>) -> DecodeResult<ContentHash> {
        let id = self
            .module
            .functions
            .get(function.index())
            .ok_or(DecodeError::Malformed("function outside normal form table"))?
            .signature
            .index();
        if self.signatures.is_empty() {
            budget.reserve(&mut self.signatures, self.module.signatures.len())?;
            self.signatures.resize(self.module.signatures.len(), None);
        }
        if let Some(hash) = self.signatures[id] {
            budget.check()?;
            return Ok(hash);
        }
        let signature = &self.module.signatures[id];
        let types = signature
            .params
            .iter()
            .filter_map(|param| match param {
                Param::Address {
                    place: PlaceTy::Declared(ty),
                    ..
                } => Some(*ty),
                _ => None,
            })
            .chain(signature.dest);
        for ty in types {
            cached_closure_hash(
                &self.module,
                ty,
                ClosureKind::Abi,
                &mut self.abi_closures,
                budget,
            )?;
        }
        let meter = PlanMeter::new(budget);
        let bytes = {
            let mut order = ModuleOrder::metered(&self.module, Some(&meter));
            let mut out = Write::module(&mut order);
            out.abi = true;
            out.abi_closures = Some(&self.abi_closures);
            signature.write(&mut out);
            out.bytes
        };
        meter.settle()?;
        let hash = ContentHash::of_parts(SIGNATURE_DOMAIN, [&bytes[..]]);
        self.signatures[id] = Some(hash);
        Ok(hash)
    }
}

fn cached_closure_hash(
    module: &Module,
    ty: TypeId,
    kind: ClosureKind,
    cached: &mut Vec<Option<ContentHash>>,
    budget: &Debit<'_>,
) -> DecodeResult<ContentHash> {
    if cached.is_empty() {
        budget.reserve(cached, module.types.len())?;
        cached.resize(module.types.len(), None);
    }
    let slot = &mut cached[ty.index()];
    if let Some(hash) = *slot {
        budget.check()?;
        return Ok(hash);
    }
    let hash = metered_closure_hash(module, ty, kind, budget)?;
    *slot = Some(hash);
    Ok(hash)
}

pub fn decode(bytes: &[u8], budget: &Debit<'_>) -> DecodeResult<DecodedBody> {
    let (module, closures) = decode_payload(bytes, budget)?;
    let malformed = || DecodeError::Malformed("noncanonical body");
    if module.functions.first().is_none_or(|f| f.body.is_none()) {
        return Err(malformed());
    }
    for f in &module.functions {
        if let Some(body) = &f.body {
            if body.blocks.is_empty() {
                return Err(malformed());
            }
            for term in &body.terminators {
                budget.check()?;
                if let Terminator::Switch { arms, .. } = term {
                    let arms = arms.get(&body.pools.arms).unwrap();
                    let scratch = budget.scratch();
                    let mut values = Vec::new();
                    scratch.reserve(&mut values, arms.len())?;
                    values.extend(arms.iter().map(|arm| arm.value));
                    values.sort_unstable();
                    if values.windows(2).any(|pair| pair[0] == pair[1]) {
                        return Err(malformed());
                    }
                }
            }
            let scratch = budget.scratch();
            let mut definitions = Vec::new();
            scratch.reserve(&mut definitions, body.values.len())?;
            definitions.resize(body.values.len(), false);
            for block in &body.blocks {
                let values = block
                    .params
                    .get(&body.pools.values)
                    .unwrap()
                    .iter()
                    .copied()
                    .chain(
                        block
                            .insts
                            .get(&body.insts)
                            .unwrap()
                            .iter()
                            .flat_map(|i| results(i, body)),
                    );
                for v in values {
                    budget.check()?;
                    if std::mem::replace(&mut definitions[v.index()], true) {
                        return Err(malformed());
                    }
                }
            }
            if definitions.contains(&false) {
                return Err(malformed());
            }
        }
    }
    // The plan is freed here but for its live-function bitmap and builtin
    // slots, whose payers outlive it.
    let meter = PlanMeter::new(budget);
    let (canonical, live_functions, builtins) = {
        let plan = plan_metered(&module, FuncId(0), Some(&meter));
        let canonical = plan.canonical_locals
            && plan.functions.is_canonical(module.functions.len())
            && plan.types.numbers.is_canonical(module.types.len())
            && plan
                .signatures
                .numbers
                .is_canonical(module.signatures.len())
            && plan.instances.numbers.is_canonical(module.instances.len())
            && plan.statics.numbers.is_canonical(module.statics.len());
        (canonical, plan.live_functions, plan.slots)
    };
    let (_live, kept_slots) = meter.finish()?;
    if !canonical {
        return Err(malformed());
    }
    let externals = budget.scratch();
    let mut external_slots = Vec::new();
    for (row, function) in module.functions.iter().enumerate() {
        if !live_functions[row] && function.versioned {
            return Err(malformed());
        }
        if let Some(body) = &function.body
            && (!live_functions[row]
                || !body
                    .compact_roots
                    .windows(2)
                    .all(|roots| (roots[0].ty, roots[0].width) < (roots[1].ty, roots[1].width)))
        {
            return Err(malformed());
        }
        if live_functions[row] && function.body.is_none() {
            budget.charge_slots(1)?;
            externals.reserve(&mut external_slots, 1)?;
            external_slots.push(Slot::External(FuncId(row as u32)));
        }
    }
    // External slots come first, then the builtins in first-use order.
    let mut slots = builtins;
    let builtin_count = slots.len();
    kept_slots.reserve(&mut slots, external_slots.len())?;
    slots.append(&mut external_slots);
    slots.rotate_left(builtin_count);
    budget.absorb(kept_slots);
    let hashed = budget.scratch();
    let mut closure_hashes = Vec::new();
    for (key, hashes) in module.instances.iter().zip(&closures) {
        for (&ty, &hash) in key.arguments.iter().zip(hashes) {
            if cached_closure_hash(&module, ty, ClosureKind::Full, &mut closure_hashes, &hashed)?
                != hash
            {
                return Err(malformed());
            }
        }
    }
    budget.check()?;
    Ok(DecodedBody {
        module,
        slots,
        closures,
        abi_closures: Vec::new(),
        signatures: Vec::new(),
    })
}

fn decode_payload(
    bytes: &[u8],
    budget: &Debit<'_>,
) -> DecodeResult<(Module, Vec<Vec<ContentHash>>)> {
    let mut input = Read::new(bytes, BODY_MAGIC, budget)?;
    let types = Wire::read(&mut input)?;
    let signatures = Wire::read(&mut input)?;
    let statics = Wire::read(&mut input)?;
    let instances = Wire::read(&mut input)?;
    let len = input.count()?;
    let functions = input.read_vec(len, read_function)?;
    let module = Module {
        types,
        signatures,
        statics,
        instances,
        functions,
        ..Module::default()
    };
    input.check_globals(&module)?;
    let closures = std::mem::take(&mut input.instance_closures);
    input.end()?;
    Ok((module, closures))
}

fn read_function(input: &mut Read<'_, '_>) -> DecodeResult<Function> {
    let instance = Wire::read(input)?;
    let signature = Wire::read(input)?;
    let versioned = Wire::read(input)?;
    let body = bool::read(input)?.then(|| read_body(input)).transpose()?;
    Ok(Function {
        instance,
        signature,
        versioned,
        body,
    })
}

fn read_body(input: &mut Read<'_, '_>) -> DecodeResult<Body> {
    let mut body = Body::default();
    for _ in 0..input.count()? {
        let params = Wire::read(input)?;
        let len = input.count()?;
        let start = body.insts.len();
        input.nested(|input| {
            for _ in 0..len {
                let inst = Inst::read(input)?;
                input.budget.reserve(&mut body.insts, 1)?;
                body.insts.push(inst);
            }
            Ok(())
        })?;
        let insts = PoolRange::new(start as u32, len as u32);
        input.budget.reserve(&mut body.blocks, 1)?;
        input.budget.reserve(&mut body.terminators, 1)?;
        body.blocks.push(Block {
            params,
            insts,
            anchor: None,
        });
        body.terminators.push(Wire::read(input)?);
    }
    input
        .budget
        .reserve(&mut body.inst_anchors, body.insts.len())?;
    body.inst_anchors.resize(body.insts.len(), None);
    body.compact_roots = Wire::read(input)?;
    body.values = Wire::read(input)?;
    body.slots = Wire::read(input)?;
    body.addresses = Wire::read(input)?;
    body.calls = Wire::read(input)?;
    body.edges = Wire::read(input)?;
    input.check_locals(&body)?;
    body.pools = std::mem::take(&mut input.pools);
    Ok(body)
}

#[cfg(test)]
mod tests;
