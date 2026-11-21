//! PROG-10 normal form for checked instance modules.

use crate::*;
use hirsel_core_ids::{ContentHash, DefPath, ItemPath, PackageId};
use hirsel_core_layout::{Case, Field, FieldTy, FuncRef, Handle, Repr, TypeRef, Width};
use std::collections::BTreeSet;
use std::io;

pub(crate) fn uleb(bytes: &mut Vec<u8>, mut value: u64) {
    while value >= 128 {
        bytes.push(value as u8 | 128);
        value >>= 7;
    }
    bytes.push(value as u8);
}

pub(crate) fn encode_identity(identity: &Identity) -> Vec<u8> {
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

const BODY_MAGIC: &[u8] = b"HIRBODY\0\x01\x00";
const ANNEX_MAGIC: &[u8] = b"HIRANNEX\0\x01\x00";

mod wire;
use wire::{Read, Wire, Write};

/// Fingerprint the closed ABI signature, independent of type row numbering.
pub fn signature(module: &Module, function: FuncId) -> ContentHash {
    let signature = module.functions[function.index()].signature;
    let mut order = ModuleOrder::new(module);
    let mut out = Write::module(&mut order);
    out.abi = true;
    module.signatures[signature.index()].write(&mut out);
    ContentHash::of_parts("hirsel.code-signature.v1", [&out.bytes[..]])
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
        ContentHash::of_parts("hirsel.code-body.v1", [self.bytes.as_slice()])
    }
}

#[derive(Default)]
struct Numbers {
    order: Vec<usize>,
    by_row: Vec<Option<u32>>,
}

impl Numbers {
    fn number(&mut self, row: usize) -> (u32, bool) {
        if row >= self.by_row.len() {
            self.by_row.resize(row + 1, None);
        }
        let next = self.order.len() as u32;
        match self.by_row[row] {
            Some(number) => (number, false),
            None => {
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

    fn discover(&mut self, row: usize) -> (u32, bool) {
        let (number, fresh) = self.numbers.number(row);
        if fresh {
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
        Self {
            module,
            types: Table::default(),
            signatures: Table::default(),
            statics: Table::default(),
            instances: Table::default(),
            functions: Numbers::default(),
            live_functions: vec![false; module.functions.len()],
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
        let (number, fresh) = self.table(kind).discover(row);
        if fresh && self.planning {
            self.write_row(kind, row);
        }
        number
    }

    fn retain_layout(&mut self, root: usize) {
        let mut pending = vec![root];
        while let Some(row) = pending.pop() {
            if !self.layouts.number(row).1 {
                continue;
            }
            let mut shape = self.module.types[row].clone();
            shape.map_refs(&mut |ty| {
                pending.push(ty.index());
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
        if self.planning {
            self.live_functions[row] = true;
        }
        self.functions.number(row).0
    }

    fn slot(&mut self, slot: Slot) {
        if self.seen_slots.insert(slot.clone()) {
            self.slots.push(slot);
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
        let mut blocks = Numbers::default();
        let mut values = Numbers::default();
        let mut pending = vec![BlockId(0)];
        while let Some(id) = pending.pop() {
            let (_, fresh) = blocks.number(id.index());
            if !fresh {
                continue;
            }
            let block = &body.blocks[id.index()];
            for &parameter in block.params.get(&body.pools.values).unwrap() {
                values.number(parameter.index());
            }
            for inst in block.insts.get(&body.insts).unwrap() {
                for result in results(inst, body) {
                    values.number(result.index());
                }
            }
            let successors = crate::ir::successors(body, id.index());
            pending.extend(successors.into_iter().rev());
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

fn results<'a>(inst: &'a Inst, body: &'a Body) -> impl Iterator<Item = ValueId> + 'a {
    let pair = |first, second| ([Some(first), second, None], &[][..]);
    let (fixed, variable): (_, &[ValueId]) = match *inst {
        Inst::Const { result, .. }
        | Inst::ReadStatic { result, .. }
        | Inst::FunctionRef { result, .. }
        | Inst::DescriptorRef { result, .. }
        | Inst::Unary { result, .. }
        | Inst::Binary { result, .. }
        | Inst::Compare { result, .. }
        | Inst::MulAdd { result, .. }
        | Inst::ScalarUnary { result, .. }
        | Inst::Read { result, .. }
        | Inst::UpdateMask { result, .. }
        | Inst::Pointer { result, .. }
        | Inst::Case { result, .. }
        | Inst::RegionOpen { result }
        | Inst::TryUnwrap { result, .. }
        | Inst::LayoutConst { result, .. }
        | Inst::CompactWidth { result, .. }
        | Inst::Simd { result, .. } => pair(result, None),
        Inst::IntArith { result, flag, .. } | Inst::Convert { result, flag, .. } => {
            pair(result, flag)
        }
        Inst::Grow {
            result, available, ..
        } => pair(result, Some(available)),
        Inst::LimitEnter {
            admitted,
            available,
            ..
        } => pair(admitted, Some(available)),
        Inst::Alloc {
            result,
            available,
            region_entry,
            ..
        } => ([Some(result), Some(available), region_entry], &[]),
        Inst::Call { call } => (
            [None; 3],
            body.calls[call.index()]
                .results
                .get(&body.pools.values)
                .unwrap(),
        ),
        Inst::Address { .. }
        | Inst::Store { .. }
        | Inst::StorageLive { .. }
        | Inst::StorageDead { .. }
        | Inst::Free { .. }
        | Inst::FrameRefund { .. }
        | Inst::Copy { .. }
        | Inst::SetCase { .. }
        | Inst::RegionClose { .. }
        | Inst::Trim { .. }
        | Inst::BuiltinUse { .. }
        | Inst::LimitExit
        | Inst::Drop { .. }
        | Inst::Share { .. }
        | Inst::Release { .. }
        | Inst::Mark { .. }
        | Inst::Unmark { .. }
        | Inst::Step { .. }
        | Inst::PromoteFrame { .. }
        | Inst::PutLanes { .. } => ([None; 3], &[]),
    };
    fixed.into_iter().flatten().chain(variable.iter().copied())
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
pub struct Annex(pub Vec<Option<FunctionAnnex>>);

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
        out.bytes
    }

    /// Read the two annex sections and check their shape against the decoded body.
    pub fn decode(bytes: &[u8], body: &Module) -> io::Result<Self> {
        let mut input = Read::new(bytes, ANNEX_MAGIC)?;
        let mut functions = Vec::new();
        for _ in 0..input.count()? {
            functions.push(if bool::read(&mut input)? {
                Some(FunctionAnnex {
                    instructions: Wire::read(&mut input)?,
                    terminators: Wire::read(&mut input)?,
                    cold_calls: vec![],
                    cold_edges: vec![],
                    callable_parameters: vec![],
                })
            } else {
                None
            });
        }
        for function in functions.iter_mut().flatten() {
            function.cold_calls = Wire::read(&mut input)?;
            function.cold_edges = Wire::read(&mut input)?;
            function.callable_parameters = (0..input.count()?)
                .map(|_| input.index())
                .collect::<io::Result<_>>()?;
        }
        input.end()?;
        let matches = functions.len() == body.functions.len()
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
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "annex does not match body",
            ));
        }
        Ok(Self(functions))
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
    sources: &crate::source::SourceMap,
) -> (EncodedBody, Annex) {
    let (body, annex) = encode_with_sources(module, root, Some(sources));
    (body, Annex(annex))
}

/// Materialize the retained module for existing native and evaluator consumers.
/// Source anchors and hints keep their original facts in the canonical numbering.
pub fn retain(module: &Module, root: FuncId) -> (EncodedBody, Module) {
    let (mut code, annex) = encode_with_annex(module, root, &crate::source::SourceMap::default());
    let mut retained = decode_payload(&code.bytes)
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
    sources: Option<&crate::source::SourceMap>,
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

fn plan(module: &Module, root: FuncId) -> ModuleOrder<'_> {
    let mut order = ModuleOrder::new(module);
    order.planning = true;
    order.function(root.index());
    let mut next = 0;
    let mut layouts_walked = 0;
    loop {
        while let Some(&row) = order.functions.order.get(next) {
            let body = module.functions[row].body.as_ref().map(BodyOrder::new);
            write_function(&mut order, row, body, None);
            next += 1;
        }
        let mut finalizers: Vec<_> = order.layouts.order[layouts_walked..]
            .iter()
            .filter_map(|&ty| {
                let f = module.types[ty].finalizer?;
                (!order.live_functions[f.0 as usize])
                    .then(|| (module.closure(TypeId(ty as u32)).hash, f))
            })
            .collect();
        layouts_walked = order.layouts.order.len();
        if finalizers.is_empty() {
            break;
        }
        finalizers.sort_by_key(|&(hash, _)| hash);
        for (_, f) in finalizers {
            order.function(f.0 as usize);
        }
    }
    let mut root_types: Vec<_> = order
        .functions
        .order
        .iter()
        .filter_map(|&row| module.functions[row].body.as_ref())
        .flat_map(|body| body.compact_roots.iter().map(|root| root.ty))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|ty| {
            order
                .types
                .numbers
                .by_row
                .get(ty.index())
                .copied()
                .flatten()
                .is_none()
        })
        .map(|ty| (module.closure(ty).hash, ty))
        .collect();
    root_types.sort_by_key(|&(hash, _)| hash);
    for &(_, ty) in &root_types {
        order.types.discover(ty.index());
    }
    for (_, ty) in root_types {
        order.write_row(GlobalRow::Type, ty.index());
    }
    for &declaration in &order.declarations.order {
        order.functions.number(declaration);
    }
    order.planning = false;
    order
}

fn write_function<'m>(
    order: &mut ModuleOrder<'m>,
    row: usize,
    body_order: Option<BodyOrder<'m>>,
    sources: Option<&crate::source::SourceMap>,
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
    let blocks = out.local().blocks.order.clone();
    out.count(blocks.len());
    for block in blocks {
        let block_row = &body.blocks[block];
        block_row.params.write(&mut out);
        let insts = block_row.insts.get(&body.insts).unwrap();
        out.count(insts.len());
        for inst in insts {
            match *inst {
                Inst::BuiltinUse { ref definition } if !out.module.planning => {
                    out.module.slot(Slot::Builtin((**definition).clone()));
                }
                Inst::DescriptorRef { function, .. }
                    if !out.module.planning
                        && let RevisionRoot::Item(identity) = module.revision_root(function) =>
                {
                    out.module.described.insert(identity);
                }
                _ => {}
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
    local.slots.append(&mut out.bytes);
    local.addresses.append(&mut out.bytes);
    local.calls.append(&mut out.bytes);
    local.edges.append(&mut out.bytes);
    let annex = sources.map(|sources| function_annex(module, &local, sources));
    (out.bytes, annex)
}

fn function_annex(
    module: &Module,
    order: &BodyOrder<'_>,
    sources: &crate::source::SourceMap,
) -> FunctionAnnex {
    let body = order.body;
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
        .map(|value| order.values.get(value.index()))
        .collect();
    callable_parameters.sort_unstable();
    callable_parameters.dedup();
    let blocks = order.blocks.order.iter().map(|&row| &body.blocks[row]);
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
        cold_calls: order
            .calls
            .numbers
            .order
            .iter()
            .map(|&row| body.calls[row].cold)
            .collect(),
        cold_edges: order
            .edges
            .numbers
            .order
            .iter()
            .map(|&row| body.edges[row].cold)
            .collect(),
        callable_parameters,
    }
}

/// Decode a body with canonical IDs and empty source and optimisation fields.
/// Function 0 is the instance's root. Source positions and hints are read
/// separately from the annex.
pub fn decode(bytes: &[u8]) -> io::Result<Module> {
    let (module, closures) = decode_payload(bytes)?;
    let malformed = || io::Error::new(io::ErrorKind::InvalidData, "noncanonical body");
    if module.functions.first().is_none_or(|f| f.body.is_none()) {
        return Err(malformed());
    }
    for f in &module.functions {
        if let Some(body) = &f.body {
            if body.blocks.is_empty() {
                return Err(malformed());
            }
            let blocks = BodyOrder::new(body);
            if !blocks.blocks.is_canonical(body.blocks.len()) {
                return Err(malformed());
            }
            for term in &body.terminators {
                if let Terminator::Switch { arms, .. } = term {
                    let mut seen = BTreeSet::new();
                    if arms
                        .get(&body.pools.arms)
                        .unwrap()
                        .iter()
                        .any(|arm| !seen.insert(arm.value))
                    {
                        return Err(malformed());
                    }
                }
            }
            let mut definitions = vec![false; body.values.len()];
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
    let plan = plan(&module, FuncId(0));
    if !plan.canonical_locals
        || !plan.functions.is_canonical(module.functions.len())
        || !plan.types.numbers.is_canonical(module.types.len())
        || !plan
            .signatures
            .numbers
            .is_canonical(module.signatures.len())
        || !plan.instances.numbers.is_canonical(module.instances.len())
        || !plan.statics.numbers.is_canonical(module.statics.len())
    {
        return Err(malformed());
    }
    for (row, function) in module.functions.iter().enumerate() {
        if !plan.live_functions[row] && function.versioned {
            return Err(malformed());
        }
        if let Some(body) = &function.body
            && (!plan.live_functions[row]
                || !body
                    .compact_roots
                    .windows(2)
                    .all(|roots| (roots[0].ty, roots[0].width) < (roots[1].ty, roots[1].width)))
        {
            return Err(malformed());
        }
    }
    for (row, hashes) in closures.into_iter().enumerate() {
        if module.identity(InstanceId(row as u32)).closures != hashes {
            return Err(malformed());
        }
    }
    Ok(module)
}

fn decode_payload(bytes: &[u8]) -> io::Result<(Module, Vec<Vec<ContentHash>>)> {
    let mut input = Read::new(bytes, BODY_MAGIC)?;
    let module = Module {
        types: Wire::read(&mut input)?,
        signatures: Wire::read(&mut input)?,
        statics: Wire::read(&mut input)?,
        instances: Wire::read(&mut input)?,
        functions: (0..input.count()?)
            .map(|_| read_function(&mut input))
            .collect::<io::Result<_>>()?,
        ..Module::default()
    };
    input.check_globals(&module)?;
    let closures = std::mem::take(&mut input.instance_closures);
    input.end()?;
    Ok((module, closures))
}

fn read_function(input: &mut Read<'_>) -> io::Result<Function> {
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

fn read_body(input: &mut Read<'_>) -> io::Result<Body> {
    let mut body = Body::default();
    for _ in 0..input.count()? {
        let params = Wire::read(input)?;
        let insts = PoolRange::push(&mut body.insts, Vec::<Inst>::read(input)?);
        body.blocks.push(Block {
            params,
            insts,
            anchor: None,
        });
        body.terminators.push(Wire::read(input)?);
    }
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
mod tests {
    use super::*;
    use crate::{source::SourceMap, text, wf};

    fn checked(source: &str) -> Module {
        let module = text::parse(source).unwrap();
        wf::module(&module).unwrap();
        module
    }

    // b2 is reached exclusively by Step. b3 is dead even though it has a call.
    const ORIGINAL: &str = "core v1
        type t0 = own \"u64\" value u64 { 0: u64 }
        type t1 = own \"u32\" value u32 { 0: u32 }
        static d0 = align 1 hex 01
        static d1 = align 2 hex 02
        sig g0 = (ptr) -> (u64)
        sig g1 = () -> ()
        inst n0 = app@1/main/ident [t0] []
        inst n1 = app@1/main/clean [] []
        inst n2 = app@1/main/dead [] []
        fn f0 = n0 g0 {
            callable v0
        b0(v0: ptr):
            v1: ptr = static d0
            step fail b2
            jump b1
        b1:
            v2: u64 = const 7
            return v2
        b2:
            call f1 g1 ()
            propagate
        b3:
            call f2 g1 ()
            unreachable
        }
        fn f1 = n1 g1 extern
        fn f2 = n2 g1 extern
    ";

    const PERMUTED: &str = "core v1
        type t0 = own \"u32\" value u32 { 0: u32 }
        type t1 = own \"u64\" value u64 { 0: u64 }
        static d0 = align 2 hex 02
        static d1 = align 1 hex 01
        sig g0 = () -> ()
        sig g1 = (ptr) -> (u64)
        inst n0 = app@1/main/dead [] []
        inst n1 = app@1/main/clean [] []
        inst n2 = app@1/main/ident [t1] []
        fn f0 = n0 g0 extern
        fn f1 = n1 g0 extern
        fn f2 = n2 g1 {
            callable v0
        b0(v0: ptr):
            v1: ptr = static d1
            step fail b1
            jump b3
        b1:
            call f1 g0 ()
            propagate
        b2:
            call f0 g0 ()
            unreachable
        b3:
            v2: u64 = const 7
            return v2
        }
    ";

    fn round_trip(encoded: &EncodedBody) -> Module {
        let decoded = decode(&encoded.bytes).unwrap();
        wf::module(&decoded).unwrap();
        assert_eq!(encode(&decoded, FuncId(0)).bytes, encoded.bytes);
        decoded
    }

    #[test]
    fn row_numbering_ignores_producer_order_and_keeps_failure_targets() {
        let a = checked(ORIGINAL);
        let mut b = checked(PERMUTED);
        let body = b.functions[2].body.as_mut().unwrap();
        let old = std::mem::replace(&mut body.pools.values, vec![ValueId(1)]);
        body.pools.values.extend(old);
        for block in &mut body.blocks {
            block.params.start += 1;
        }
        for term in &mut body.terminators {
            if let Terminator::Return(values) = term {
                values.start += 1;
            }
        }
        for call in &mut body.calls {
            call.results.start += 1;
        }
        for edge in &mut body.edges {
            edge.args.start += 1;
        }
        wf::module(&b).unwrap();
        let a = encode(&a, FuncId(0));
        let b = encode(&b, FuncId(2));
        assert_eq!(a.bytes, b.bytes, "A5 canonical body bytes");
        assert_eq!(
            a.slots,
            vec![Slot::External(FuncId(1))],
            "A5 only reached callees bind"
        );
        let decoded = round_trip(&a);
        let body = decoded.functions[0].body.as_ref().unwrap();
        assert_eq!(body.blocks.len(), 3, "A5 keeps the non-call failure block");
        assert!(matches!(
            body.insts[1],
            Inst::Step {
                fail: Some(BlockId(1))
            }
        ));
        let dead_edit = checked(&ORIGINAL.replace("app@1/main/dead", "app@1/main/other_dead"));
        let dead_edit = encode(&dead_edit, FuncId(0));
        assert_eq!(
            a.hash(),
            dead_edit.hash(),
            "D8 the dead-only call contributes no body bytes"
        );
        assert_eq!(
            a.slots, dead_edit.slots,
            "D8 the dead-only call contributes no slot"
        );
        local_row_permutation();
    }

    #[test]
    fn hints_only_change_the_annex() {
        let a = checked(ORIGINAL);
        let sources = SourceMap::default();
        let (encoded, original) = encode_with_annex(&a, FuncId(0), &sources);
        for hint in 0..3 {
            let mut b = a.clone();
            let body = b.functions[0].body.as_mut().unwrap();
            match hint {
                0 => body.callable_parameters.clear(),
                1 => body.calls[0].cold = true,
                2 => body.edges[0].cold = true,
                _ => unreachable!(),
            }
            let (b, annex) = encode_with_annex(&b, FuncId(0), &sources);
            assert_eq!(
                encoded.hash(),
                b.hash(),
                "A6 hint {hint} is outside the body"
            );
            assert_ne!(
                original.hash(),
                annex.hash(),
                "A6 hint {hint} is in the annex"
            );
            let bytes = annex.encode();
            let decoded_body = round_trip(&b);
            let decoded = Annex::decode(&bytes, &decoded_body).unwrap();
            assert_eq!(decoded.encode(), bytes);
            let facts = decoded.0[0].as_ref().unwrap();
            assert_eq!(facts.cold_calls, vec![hint == 1]);
            assert_eq!(facts.cold_edges, vec![hint == 2]);
            assert_eq!(
                facts.callable_parameters,
                if hint == 0 { vec![] } else { vec![0] }
            );
        }
    }

    #[test]
    fn instance_arguments_are_closed_type_rows() {
        let a = checked(ORIGINAL);
        let b = checked(PERMUTED);
        let mut c = a.clone();
        c.instances[0].arguments[0] = TypeId(1);
        let a = encode(&a, FuncId(0));
        let b = encode(&b, FuncId(2));
        let mut dictionary = checked(ORIGINAL);
        dictionary.instances[0]
            .dictionaries
            .push("app@1/impl/selected".into());
        let different_dictionary = encode(&dictionary, FuncId(0));
        let c = encode(&c, FuncId(0));
        assert_eq!(a.hash(), b.hash(), "D6 local TypeId is immaterial");
        assert_ne!(a.hash(), c.hash(), "D6 u64 and u32 separate");
        assert_ne!(
            a.hash(),
            different_dictionary.hash(),
            "D6 dictionary choice separates"
        );
        let identity = |body| {
            let module = round_trip(body);
            module.identity(module.functions[0].instance)
        };
        assert_ne!(identity(&a), identity(&c));
    }
    #[test]
    fn cleanup_only_edit_changes_body_and_failure() {
        let before = "core v1
            sig g0 = (u64) -> (u64)
            sig g1 = () -> ()
            inst n0 = app@1/main/f [] []
            inst n1 = app@1/main/observe [] []
            fn f0 = n0 g0 {
            b0(v0: u64):
                v1: u64 = const 1
                v2: u64 = add v0, v1 fail b1
                return v2
            b1:
                propagate
            }
            fn f1 = n1 g1 {
            b0:
                v0: u64 = const 37
                v1: bool, v2: u64 = limit_enter v0
                branch v1, b1, b2
            b1:
                limit_exit
                return
            b2:
                return
            }
        ";
        let a = encode(&checked(before), FuncId(0));
        let after = before.replace("propagate", "call f1 g1 ()\n propagate");
        let b = encode(&checked(&after), FuncId(0));
        assert_ne!(
            a.hash(),
            b.hash(),
            "D7 the cleanup-only edit changes the body"
        );
        let root = Identity::plain(DefPath::from_full("app@1/main/f").unwrap());
        let address = |body: &EncodedBody| {
            assert!(body.slots.is_empty());
            crate::code_addresses([root.clone()], |_| {
                Some(crate::CodeRow {
                    body: body.hash(),
                    slots: Vec::new(),
                })
            })
            .unwrap()[&root]
        };
        assert_ne!(
            address(&a),
            address(&b),
            "D7 the overflow observer changes the address"
        );
        let (a, b) = (round_trip(&a), round_trip(&b));
        let run = |m, arg| crate::eval::run(m, FuncId(0), &[arg], 4096);
        let (a_ok, b_ok) = (run(&a, 7), run(&b, 7));
        assert_eq!(a_ok.outcome, crate::eval::Outcome::Returned(vec![8]));
        assert_eq!(b_ok.outcome, a_ok.outcome);
        assert_eq!(
            b_ok.peak_reserved, a_ok.peak_reserved,
            "normal input skips the observer"
        );
        let (a_failed, b_failed) = (run(&a, u64::MAX), run(&b, u64::MAX));
        assert_eq!(a_failed.outcome, b_failed.outcome);
        assert!(matches!(b_failed.outcome, crate::eval::Outcome::Failed(_)));
        assert_eq!(
            b_failed.peak_reserved,
            a_failed.peak_reserved + 37,
            "overflow cleanup ran the observer"
        );
    }

    fn hex(text: &str) -> Vec<u8> {
        text.as_bytes()
            .chunks_exact(2)
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect()
    }

    #[test]
    fn body_group_member_revision_and_builtin_have_fixed_bytes() {
        let module = checked(
            "core v1
sig g0 = () -> (u64)
inst n0 = app@1/main/f [] []
fn f0 = n0 g0 {
             b0:
 v0: u64 = const 7
 return v0
}
",
        );
        let encoded = encode(&module, FuncId(0));
        // Literal v1 wire fields and SHA-256 computed independently with
        // Python hashlib over the two u64le length-prefixed parts.
        let body = hex(concat!(
            "484952424f4459000100",
            "00010001000900000001",
            "0c61707040312f6d61696e2f66000000",
            "01000000010100010000",
            "0700000000000000",
            "0301000001000900000000",
        ));
        assert_eq!(encoded.bytes, body);
        let hash = encoded.hash();
        assert_eq!(
            hash.0.as_slice(),
            hex("bf13ac887471a35ca6faf6c5d851787598bd9133e41a999eda48fc1c6e6bd3ea")
        );
        let identity = module.identity(module.functions[0].instance);
        let addresses = crate::code_addresses([identity.clone()], |_| {
            Some(crate::CodeRow {
                body: hash,
                slots: vec![],
            })
        })
        .unwrap();
        let address = addresses[&identity];
        assert_eq!(
            address.0.as_slice(),
            hex("4f153742d5122fbe3bff42c86d8fd6c5fbbda4f4ec540000ba017e46bebac4cf")
        );
        assert_eq!(
            crate::code_revision(address).0.as_slice(),
            hex("b2c86bcbd593913c656fc5b525b029d0bbaf7612018d1118449e6f9af2553207")
        );
        let widen = Identity::plain(DefPath::core("float.widen"));
        assert_eq!(
            crate::builtin_address(&widen).unwrap().0.as_slice(),
            hex("bb63c4ef24b5d261f66965edd632ade0cf2ff8d99540fd2be8bef1111448a41e")
        );
        let decoded = round_trip(&encoded);
        let result = crate::eval::run(&decoded, FuncId(0), &[], 4096).outcome;
        assert_eq!(result, crate::eval::Outcome::Returned(vec![7]));
    }

    #[test]
    fn explicit_closure_tuples_preserve_published_identity() {
        let module = text::parse(include_str!("../samples/failure.core")).unwrap();
        let closure = module.closure(TypeId(0));
        // Fixed-width v1 tuple for Temp with a finalizer, hand-framed with hashlib.
        assert_eq!(
            closure.hash.0.as_slice(),
            hex("9debaf6b4ea25b29f5812ee89b1043a97472e3787299e459bf95c5ee563c5254")
        );
        assert_eq!(
            closure.hash,
            ContentHash::of_encoded("hirsel.core-closure.v1", &closure.rows),
        );
    }

    #[test]
    fn retained_layouts_bind_finalizers_but_codec_metadata_does_not() {
        let original = r#"core v1
            type t0 = resource "Alpha" drop f1 { x: u64 }
            type t1 = resource "Beta" drop f2 { x: u64 }
            type t2 = own "Pair" codec f3 { a: t0, b: t1 }
            type t3 = own "Codec[Pair]" { x: u64 }
            sig g0 = (&take t2) -> ()
            sig g1 = (ptr) -> ()
            sig g2 = () -> () dest t3
            inst n0 = app@1/main/dispose [] []
            inst n1 = app@1/main/Alpha [] []
            inst n2 = app@1/main/Beta [] []
            inst n3 = app@1/main/maker [] []
            fn f0 = n0 g0 {
                addr a0 = v0 : t2
            b0(v0: ptr):
                address a0
                drop a0 t2
                return
            }
            fn f1 = n1 g1 extern
            fn f2 = n2 g1 extern
            fn f3 = n3 g2 {
            b0(v0: ptr):
                v1: u64 = const 7
                return
            }
        "#;
        let a = checked(original);
        let mut codec_edit = checked(&original.replace("const 7", "const 8"));
        codec_edit.functions[3].versioned = true;
        assert_eq!(
            encode(&a, FuncId(0)).hash(),
            encode(&codec_edit, FuncId(0)).hash(),
            "D8 a metadata-only codec implementation is outside code identity"
        );
        let mut b = a.clone();
        b.types.swap(0, 1);
        b.functions.swap(1, 2);
        for shape in &mut b.types {
            if let Some(finalizer) = &mut shape.finalizer {
                finalizer.0 = 3 - finalizer.0;
            }
        }
        let hirsel_core_layout::Body::Record(fields) = &mut b.types[2].body else {
            unreachable!()
        };
        fields[0].ty = FieldTy::Inline(TypeRef(1));
        fields[1].ty = FieldTy::Inline(TypeRef(0));
        wf::module(&b).unwrap();
        let measure = |module: &Module| {
            let encoded = encode(module, FuncId(0));
            let slots = encoded
                .slots
                .iter()
                .map(|slot| slot.identity(module))
                .collect::<Vec<_>>();
            (encoded, slots)
        };
        let (a, slots_a) = measure(&a);
        let (b, slots_b) = measure(&b);
        assert_eq!(a.bytes, b.bytes, "A4 finalizer roots follow closure order");
        assert_eq!(slots_a, slots_b);
        let names = slots_a
            .iter()
            .map(|slot| slot.definition.0.name.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            names,
            BTreeSet::from(["Alpha", "Beta"]),
            "D8 drop-only slots, no codec slot"
        );
        let decoded = round_trip(&a);
        assert!(
            decoded.functions.last().unwrap().body.is_none(),
            "D8 codec maker is a declaration"
        );
        let dispose = decoded.identity(decoded.functions[0].instance);
        let caller_address = |alpha| {
            crate::code_addresses([dispose.clone()], |id| {
                let (body, slots) = if *id == dispose {
                    (a.hash(), slots_a.clone())
                } else {
                    let byte = if id.definition.0.name == "Alpha" {
                        alpha
                    } else {
                        7
                    };
                    (ContentHash([byte; 32]), Vec::new())
                };
                Some(crate::CodeRow { body, slots })
            })
            .unwrap()[&dispose]
        };
        assert_ne!(
            caller_address(7),
            caller_address(8),
            "D8 edits to a nested drop-only finalizer propagate"
        );
    }

    #[test]
    fn source_position_changes_only_the_matching_annex() {
        let mut module = checked(ORIGINAL);
        let root = DefPath::from_full("app@1/main/ident").unwrap();
        let unused = DefPath::from_full("app@1/main/unrelated").unwrap();
        module.items = vec![root.clone(), unused];
        let anchor = |item, start| {
            Some(Anchor {
                item: ItemId(item),
                start,
                end: start + 1,
            })
        };
        module.functions[0].body.as_mut().unwrap().inst_anchors[1] = anchor(0, 0);
        let sources = SourceMap {
            files: vec![crate::source::SourceFile {
                path: "main.hirsel".into(),
                text: "a\nb".into(),
            }],
            items: [(
                root,
                crate::source::ItemSource {
                    file: 0,
                    file_offset: 0,
                },
            )]
            .into(),
        };
        let (a, sites_a) = encode_with_annex(&module, FuncId(0), &sources);
        module.items.swap(0, 1);
        module.functions[0].body.as_mut().unwrap().inst_anchors[1] = anchor(1, 0);
        let (permuted, same_sites) = encode_with_annex(&module, FuncId(0), &sources);
        assert_eq!(
            a.bytes, permuted.bytes,
            "A1 the item-table permutation does not enter the body"
        );
        assert_eq!(sites_a, same_sites);
        module.functions[0].body.as_mut().unwrap().inst_anchors[1] = anchor(1, 2);
        let (b, sites_b) = encode_with_annex(&module, FuncId(0), &sources);
        assert_eq!(a.bytes, b.bytes, "A2 source offsets do not enter the body");
        let line = |annex: &Annex| {
            annex.0[0].as_ref().unwrap().instructions[1]
                .as_ref()
                .unwrap()
                .line
        };
        assert_eq!((line(&sites_a), line(&sites_b)), (1, 2));
        let decoded = round_trip(&b);
        assert_eq!(Annex::decode(&sites_b.encode(), &decoded).unwrap(), sites_b);
        let mut wrong = sites_b;
        wrong.0[0].as_mut().unwrap().cold_calls.clear();
        assert!(
            Annex::decode(&wrong.encode(), &decoded).is_err(),
            "annex must cover the body's calls"
        );
    }

    #[test]
    fn logical_admission_is_in_the_body() {
        let before = "core v1
            sig g0 = () -> (u64)
            inst n0 = app@1/main/admit [] []
            fn f0 = n0 g0 {
            b0:
                v0: u64 = const 5
                v1: u64 = const 7
                v2: bool, v3: u64 = limit_enter v0
                branch v2, b1, b2
            b1:
                limit_exit
                v4: u64 = const 1
                return v4
            b2:
                return v3
            }
        ";
        let a = encode(&checked(before), FuncId(0));
        let b = encode(
            &checked(&before.replace("limit_enter v0", "limit_enter v1")),
            FuncId(0),
        );
        assert_ne!(
            a.hash(),
            b.hash(),
            "D11 logical admission enters checked code identity"
        );
        let (a, b) = (round_trip(&a), round_trip(&b));
        let run = |module, limit| crate::eval::run(module, FuncId(0), &[], limit).outcome;
        let returned = crate::eval::Outcome::Returned(vec![1]);
        assert_eq!(run(&a, 8), returned);
        assert_eq!(run(&b, 8), returned);
        assert_eq!(run(&a, 6), returned);
        assert_eq!(
            run(&b, 6),
            crate::eval::Outcome::Returned(vec![6]),
            "seven requested, six available"
        );
    }

    #[test]
    fn decoder_rejects_missing_or_nonminimal_fields() {
        let a = encode(&checked(ORIGINAL), FuncId(0));
        let mut extra = a.bytes.clone();
        extra.push(0);
        assert!(decode(&extra).is_err());
        let mut redundant = a.bytes.clone();
        redundant[BODY_MAGIC.len()] |= 128;
        redundant.insert(BODY_MAGIC.len() + 1, 0);
        assert!(decode(&redundant).is_err());
        assert!(decode(&a.bytes[..a.bytes.len() - 1]).is_err());
    }
    fn local_row_permutation() {
        let module = checked(
            r#"core v1
            type t0 = compact u16 "First" { x: u8 }
            type t1 = compact u32 "Second" { x: u64 }
            sig g0 = (u64) -> (u64)
            sig g1 = (u64) -> (u64, u64)
            inst n0 = app@1/main/run [] []
            inst n1 = app@1/main/double [] []
            fn f0 = n0 g0 {
                root t0 2
                root t1 4
                slot s0 = u64
                slot s1 = u64
                addr a0 = s0 : u64
                addr a1 = a0 : u64
                addr a2 = s1 : u64
            b0(v0: u64):
                live s0
                live s1
                address a0
                address a1
                address a2
                store a0, v0
                store a2, v0
                switch v0 [0 -> b1, 1 -> b2] else b3(v0)
            b1:
                v1: u64 = read a1
                v2: u64, v3: u64 = call f1 g1 (v1)
                jump b3(v2)
            b2:
                v4: u64 = read a1
                v5: u64, v6: u64 = call f1 g1 (v4)
                jump b3(v6)
            b3(v7: u64):
                dead s0
                dead s1
                return v7
            }
            fn f1 = n1 g1 extern
        "#,
        );
        let mut other = module.clone();
        other.types.swap(0, 1);
        let b = other.functions[0].body.as_mut().unwrap();
        b.compact_roots.reverse();
        for root in &mut b.compact_roots {
            root.ty.0 = 1 - root.ty.0;
        }
        b.values.reverse();
        b.slots.swap(0, 1);
        b.addresses.rotate_right(1);
        b.calls.reverse();
        b.edges.reverse();
        let value = |v: &mut ValueId| v.0 = 7 - v.0;
        let address = |a: &mut AddressId| a.0 = (a.0 + 1) % 3;
        let edge_count = b.edges.len() as u32;
        let edge = |e: &mut EdgeId| e.0 = edge_count - 1 - e.0;
        for a in &mut b.addresses {
            match &mut a.base {
                AddressBase::Slot(s) => s.0 = 1 - s.0,
                AddressBase::Address(a) => address(a),
                AddressBase::Value(_) => unreachable!(),
            }
        }
        for inst in &mut b.insts {
            match inst {
                Inst::StorageLive { slot } | Inst::StorageDead { slot } => slot.0 = 1 - slot.0,
                Inst::Address { address: a, .. } => address(a),
                Inst::Store {
                    address: a,
                    value: v,
                }
                | Inst::Read {
                    address: a,
                    result: v,
                    ..
                } => {
                    address(a);
                    value(v);
                }
                Inst::Call { call } => call.0 = 1 - call.0,
                _ => unreachable!(),
            }
        }
        for arg in &mut b.pools.args {
            let Arg::Value(v) = arg else { unreachable!() };
            value(v);
        }
        for v in &mut b.pools.values {
            value(v);
        }
        b.pools.arms.insert(
            0,
            SwitchArm {
                value: 99,
                edge: EdgeId(0),
            },
        );
        for arm in &mut b.pools.arms {
            edge(&mut arm.edge);
        }
        for term in &mut b.terminators {
            match term {
                Terminator::Jump(e) => edge(e),
                Terminator::Switch {
                    value: v,
                    arms,
                    otherwise,
                } => {
                    value(v);
                    arms.start += 1;
                    edge(otherwise);
                }
                Terminator::Return(_) => {}
                _ => unreachable!(),
            }
        }
        wf::module(&other).unwrap();
        let a = encode(&module, FuncId(0));
        let b = encode(&other, FuncId(0));
        assert_eq!(
            a.hash(),
            b.hash(),
            "A5 preserves local rows and compact-root sets"
        );
        round_trip(&b);
    }

    #[test]
    fn versioned_flag_enters_the_body_and_release_copies_do_not() {
        let mut module = checked(
            r#"core v1
            sig g0 = (u8) -> (u64)
            inst n0 = app@1/main/vectors [] []
            fn f0 = n0 g0 versioned {
            b0(v0: u8):
                v1: v16xu8 = simd splat v0
                v2: u64 = const 7
                return v2
            }
        "#,
        );
        let first = encode(&module, FuncId(0)).hash();
        module
            .code_variants
            .insert(FuncId(0), ContentHash([0x31; 32]));
        assert_eq!(
            encode(&module, FuncId(0)).hash(),
            first,
            "A7 backend copies are outside checked code identity"
        );
        module.functions[0].versioned = false;
        assert_ne!(
            encode(&module, FuncId(0)).hash(),
            first,
            "D12 the checked versioned flag enters the body"
        );
    }
}
