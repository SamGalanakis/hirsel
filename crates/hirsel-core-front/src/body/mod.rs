//! Bodies: one forward visit per function that types its body against the published
//! signatures and summaries, checks ownership and emits CoreIR as it goes (LANG-8:
//! nothing else is read).
//!
//! Emitted so far: scalars and unit, blocks, `let` and `let mut`, assignment and compound
//! assignment, the operators of LANG-21 (SEM-1, SEM-2, SEM-7), `if`, `loop`, `while`, labels,
//! `break`, `continue`, `return`, direct calls of non-generic functions and the core
//! integer operations; and plain values of declared types ([`Rep::Place`]): records,
//! variants and tuples built in a new place, passed by address, returned through the
//! caller's destination, copied into `let mut` slots and read field by field.
//! Capabilities and `Args` are `u64` provider handles. Checked but not yet emitted:
//! values that need cleanup (String, List, resources), `mut` arguments, index places, and
//! the ownership rules OWN-5 to OWN-17 over them. Anything else is a [`Gap`]: the body is
//! still checked, but no code is emitted for it. An operation that can fail names no
//! failure successor, so the failure leaves the function with its cause (FORM v1a).
//!
//! Ownership state is per body and indexed by binder id: the slot table, an undo log of
//! availability changes (a branch's arms run from one state and join by OWN-16 without
//! copying the table), and the freeze and reservation records (crate::own).

use crate::decls::{
    DeclBody, FnDecl, KEY, Kind, ORDERED, Repr, Role, copyable, has_bounds, is_core_prelude, kind,
};
use crate::own::{Act, Avail, Extent, Freeze, Hold, Origin, PStep, Place, breaks, join, overlap};
use crate::types::*;
use crate::{Def, Gap, ItemRef, Program, refusal};
use hirsel_core_ids::DefPath;
use hirsel_core_ir::{
    AddressBase, AddressId, AddressInfo, AllocKind, AllocPlace, Anchor, Arg, ArithMode, ArithOp,
    BinaryOp, Block, BlockId, Body, CallId, CallSite, Callee, CompareOp, ConvertMode, Edge, EdgeId,
    FuncId, Function, Inst, InstanceId, InstanceKey, ItemId, Module, Param, PlaceTy, PoolRange,
    ReadMode, SigId, Signature, SlotId, Step, Terminator, Ty, TypeId, UnaryOp, ValueId,
};
use hirsel_core_layout::{FieldTy, Handle, Layout, Recipe, Repr as LRepr, Scalar, TypeRef};
use hirsel_core_resolve::{Refusal, Target};
use hirsel_core_syntax::{Item, NodeId, NodeKind, Span, TokenKind, byte_value, character_value};
use std::collections::HashMap;

mod calls;
mod cleanup;
mod closures;
mod control;
mod ops;
mod patterns;
mod places;
mod templates;
mod types;
use templates::{CopySize, GenericCall, NameRecipe};
pub use templates::{GenericTemplate, materialize};
mod values;
use types::*;

/// Work one body check did, for the cost measurements (core/MEASURE.md).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Counts {
    /// Expression, statement and pattern nodes visited, and steps deciding whether an
    /// expression synthesises its type.
    pub nodes: u64,
    /// Names read.
    pub uses: u64,
    /// Place reads and writes of `let mut` slots.
    pub places: u64,
    pub calls: u64,
    /// Type comparisons and type nodes lowered.
    pub types: u64,
    /// Bytes of the emitted body's tables.
    pub bytes: u64,
}
impl std::ops::AddAssign for Counts {
    fn add_assign(&mut self, o: Self) {
        self.nodes += o.nodes;
        self.uses += o.uses;
        self.places += o.places;
        self.calls += o.calls;
        self.types += o.types;
        self.bytes += o.bytes;
    }
}

/// The outcome of checking and lowering one definition.
#[derive(Debug, Default)]
pub struct CoreCheck {
    pub refusals: Vec<Refusal>,
    pub gaps: Vec<Gap>,
    pub counts: Counts,
    /// The function row whose body was emitted; None when the body was refused, reached
    /// a gap, or the definition has no body.
    pub function: Option<FuncId>,
    /// Layout/type relocations for a rigid generic body, checked once.
    pub template: Option<GenericTemplate>,
}

/// The module being emitted: one row per instance, made on first mention, so a call
/// can name a function whose body is checked later.
#[derive(Default)]
pub struct Emitter {
    pub module: Module,
    funcs: HashMap<InstanceKey, FuncId>,
    sigs: HashMap<Signature, SigId>,
    items: HashMap<DefPath, ItemId>,
    /// Per non-generic definition of the program, its function row once mentioned.
    rows: Vec<Option<(FuncId, SigId)>>,
    /// `module.types` rows by canonical text.
    types: HashMap<String, TypeRef>,
    /// The recipes of `module.types`, recomputed when rows were added.
    recipes: Vec<Recipe>,
    generic_names: std::collections::BTreeMap<TypeId, NameRecipe>,
    generic_calls: std::collections::BTreeMap<FuncId, GenericCall>,
    #[cfg(test)]
    templates: HashMap<DefPath, (FuncId, GenericTemplate)>,
}

/// How a value of a type is carried in a body (crate docs, "Values, slots and
/// addresses"): a scalar, nothing (unit, Never), or a pointer to a place of a declared
/// type, which a parameter takes by address and a result returns through `dest`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Rep {
    None,
    Scalar(Ty),
    Place(TypeId),
    /// A view of a place of this type: its pointer, stored and passed as a pointer
    /// value and never cleaned (OWN-8).
    View(PlaceTy),
}
impl Rep {
    /// The CoreIR type of the value: a place travels as its pointer.
    fn ty(self) -> Option<Ty> {
        match self {
            Rep::None => None,
            Rep::Scalar(t) => Some(t),
            Rep::Place(_) | Rep::View(_) => Some(Ty::Ptr),
        }
    }
    /// The place a value of this representation is stored in: a view stores its
    /// pointer.
    fn stored(self) -> Option<PlaceTy> {
        match self {
            Rep::None => None,
            Rep::Scalar(t) => Some(PlaceTy::Value(t)),
            Rep::Place(id) => Some(PlaceTy::Declared(id)),
            Rep::View(_) => Some(PlaceTy::Value(Ty::Ptr)),
        }
    }
    /// The row of the place a value of this representation points at.
    fn pointee(self) -> Option<TypeId> {
        match self {
            Rep::Place(id) | Rep::View(PlaceTy::Declared(id)) => Some(id),
            _ => None,
        }
    }
}
impl Emitter {
    pub fn new() -> Self {
        Self::default()
    }
    fn sig(&mut self, s: Signature) -> SigId {
        if let Some(&id) = self.sigs.get(&s) {
            return id;
        }
        let id = SigId(self.module.signatures.len() as u32);
        self.module.signatures.push(s.clone());
        self.sigs.insert(s, id);
        id
    }
    fn item(&mut self, path: &DefPath) -> ItemId {
        if let Some(&id) = self.items.get(path) {
            return id;
        }
        let id = ItemId(self.module.items.len() as u32);
        self.module.items.push(path.clone());
        self.items.insert(path.clone(), id);
        id
    }
    /// The function row and signature of the program's definition `f`.
    fn row(&mut self, p: &Program, f: usize) -> Result<(FuncId, SigId), &'static str> {
        if let Some(Some(row)) = self.rows.get(f) {
            return Ok(*row);
        }
        let sig = self.signature(p, f)?;
        let func = self.function(&p.fns[f].path, sig);
        let row = (func, self.module.functions[func.index()].signature);
        if self.rows.len() <= f {
            self.rows.resize(f + 1, None);
        }
        self.rows[f] = Some(row);
        Ok(row)
    }
    /// A closed generic call row. Source checking is definition-local; a row names
    /// the concrete signature and instance the materializer must supply later.
    fn instance(
        &mut self,
        p: &Program,
        t: &mut impl TyRead,
        f: usize,
        arguments: &[TyId],
    ) -> Result<(FuncId, SigId), &'static str> {
        if arguments.is_empty() {
            return self.row(p, f);
        }
        let fd = &p.fns[f];
        let types: Vec<_> = arguments
            .iter()
            .map(|&ty| crate::decls::canonical(&p.decls, t, ty))
            .collect();
        let mut dictionaries = vec![];
        for (generic, ty) in fd.generics.iter().zip(&types) {
            for bit in 0..u16::BITS {
                if generic.bounds & (1 << bit) != 0 {
                    dictionaries.push(format!("{}[{ty}]", crate::decls::bound_names(1 << bit)));
                }
            }
        }
        let key = InstanceKey {
            definition: fd.path.clone(),
            types,
            dictionaries,
        };
        if let Some(&function) = self.funcs.get(&key) {
            return Ok((function, self.module.functions[function.index()].signature));
        }
        for &ty in arguments {
            self.type_row(p, t, ty)
                .ok_or("values of this type are not built")?;
        }
        let signature = self.signature_inst(p, t, f, arguments)?;
        let function = self.add_function(key, signature);
        if arguments.iter().any(|&ty| has_param(t, ty)) {
            let names: Vec<_> = arguments
                .iter()
                .map(|&ty| NameRecipe::of(p, t, ty))
                .collect();
            let mut dictionaries = vec![];
            for (generic, name) in fd.generics.iter().zip(&names) {
                for bit in 0..u16::BITS {
                    if generic.bounds & (1 << bit) != 0 {
                        dictionaries.push(NameRecipe::dictionary(
                            &crate::decls::bound_names(1 << bit),
                            name,
                        ));
                    }
                }
            }
            self.generic_calls.insert(
                function,
                GenericCall {
                    arguments: names,
                    dictionaries,
                },
            );
        }
        Ok((function, self.module.functions[function.index()].signature))
    }

    /// The function row of a non-generic definition.
    pub fn function(&mut self, path: &DefPath, sig: Signature) -> FuncId {
        let key = InstanceKey {
            definition: path.clone(),
            types: vec![],
            dictionaries: vec![],
        };
        self.add_function(key, sig)
    }
    fn add_function(&mut self, key: InstanceKey, sig: Signature) -> FuncId {
        if let Some(&f) = self.funcs.get(&key) {
            return f;
        }
        let signature = self.sig(sig);
        let instance = InstanceId(self.module.instances.len() as u32);
        self.module.instances.push(key.clone());
        let f = FuncId(self.module.functions.len() as u32);
        self.module.functions.push(Function {
            instance,
            signature,
            body: None,
        });
        self.funcs.insert(key, f);
        f
    }
}

impl Emitter {
    /// The row of closed type `ty` in `module.types`, made with the rows it holds
    /// inline; None when some part has no field type yet.
    fn type_row(&mut self, p: &Program, t: &mut impl TyRead, ty: TyId) -> Option<TypeId> {
        let mark = self.module.types.len();
        let mut s = crate::layout::Shapes {
            decls: &p.decls,
            types: t,
            shapes: &mut self.module.types,
            of: &mut self.types,
            module: true,
        };
        let r = s.shape(ty);
        if r.is_none() {
            s.truncate(mark);
        }
        let row = TypeId(r?.0);
        self.remember_generic_types(p, t, ty);
        Some(row)
    }
    /// The recipe of row `id`, or None when the table's layout is refused, in which case
    /// the rows from `mark` on are taken back so the table stays computable.
    fn recipe(&mut self, id: TypeId, mark: usize) -> Option<&Recipe> {
        if self.recipes.len() != self.module.types.len() {
            match hirsel_core_layout::compute(&self.module.types) {
                Ok(r) => self.recipes = r,
                Err(_) => {
                    self.module.types.truncate(mark);
                    self.types.retain(|_, r| (r.0 as usize) < mark);
                    self.generic_names.retain(|id, _| id.index() < mark);
                    return None;
                }
            }
        }
        self.recipes.get(id.index())
    }
    /// How a value of `ty` is carried: a scalar, or a place of a fixed layout. A value
    /// whose cleanup may run a finalizer (a resource, or one held inside) is not built.
    fn rep(&mut self, p: &Program, t: &mut impl TyRead, ty: TyId) -> Result<Rep, &'static str> {
        match t.kind(ty) {
            TyKind::Scalar(s) => return Ok(Rep::Scalar(Ty::Scalar(*s))),
            TyKind::Fn(_, _, _, Callable::Plain) => return Ok(Rep::Scalar(Ty::Ptr)),
            TyKind::Unit | TyKind::Never | TyKind::Error => return Ok(Rep::None),
            // A capability or `Args` is a provider handle, plain and never cleaned by
            // the program (FORM e68de117).
            TyKind::Adt(d, _) if crate::layout::handle(&p.decls[d.index()]) => {
                return Ok(Rep::Scalar(Ty::Scalar(Scalar::U64)));
            }
            TyKind::Tuple(_)
            | TyKind::Array(..)
            | TyKind::Adt(..)
            | TyKind::Fn(..)
            | TyKind::Param(_) => {}
            // A view is a pointer to a place of its type, never cleaned (OWN-8).
            TyKind::View(x) => {
                let x = *x;
                return match self.rep(p, t, x)? {
                    Rep::Place(id) => Ok(Rep::View(PlaceTy::Declared(id))),
                    Rep::Scalar(s) => Ok(Rep::View(PlaceTy::Value(s))),
                    _ => Err("views of this type are not built"),
                };
            }
            _ => return Err("values of this type are not built"),
        }
        let mark = self.module.types.len();
        let id = self
            .type_row(p, t, ty)
            .ok_or("values of this type are not built")?;
        match self.recipe(id, mark) {
            Some(r) if matches!(r.layout, Layout::Fixed(_)) => {
                if self.runs_code(id) {
                    Err("cleanup of resources is not built")
                } else {
                    Ok(Rep::Place(id))
                }
            }
            _ => Err("values of this type are not built"),
        }
    }
    /// Row `id` is a resource: a value of it is a pointer to its block, which holds
    /// its fields, cause cell and grant (MEM-24, layout: a holder counts 8).
    fn is_resource(&self, id: TypeId) -> bool {
        self.module.types[id.index()].repr == LRepr::Resource
    }
    /// The admission of a resource of row `id`: its storage, cause cell and grant
    /// (MEM-24, MEM-27).
    fn resource_charge(&self, id: TypeId) -> Option<u64> {
        let storage = self.recipes[id.index()].logical_size?;
        hirsel_core_layout::charge::resource(storage, self.module.types[id.index()].grant)
    }
    /// Cleaning a value of row `id` does something (FAIL-8).
    fn needs_drop(&self, id: TypeId) -> bool {
        self.recipes[id.index()].drop.is_some()
    }
    /// Cleaning a value of row `id` may run a finalizer; a resource without one is
    /// cleaned mechanically (FAIL-13). Rows reach themselves through sequences and
    /// maps, so each row is visited once.
    fn runs_code(&self, id: TypeId) -> bool {
        let mut seen = vec![id];
        let mut work = vec![id];
        while let Some(id) = work.pop() {
            let Some(d) = &self.recipes[id.index()].drop else {
                continue;
            };
            if d.finalizer.is_some() {
                return true;
            }
            for f in d.fields.iter().flatten() {
                if field_runs_code(&f.ty, &mut |r| {
                    if !seen.contains(&r) {
                        seen.push(r);
                        work.push(r);
                    }
                }) {
                    return true;
                }
            }
        }
        false
    }
    /// The byte size of a value of row `id`, whose recipe exists: a resource's
    /// holder is its block pointer.
    fn size(&self, id: TypeId) -> u64 {
        if self.is_resource(id) {
            return hirsel_core_layout::POINTER;
        }
        self.block(id).0
    }
    /// The size and alignment of row `id`'s fields laid out together (a resource's
    /// block).
    fn block(&self, id: TypeId) -> (u64, u64) {
        match &self.recipes[id.index()].layout {
            Layout::Fixed(f) => (f.size, f.align),
            Layout::Compact(_) => unreachable!("only fixed layouts are carried"),
        }
    }
    /// The CoreIR signature of the program's function `f`, or the gap that keeps it
    /// from having one: scalars by value, declared types by address, a declared result
    /// through `dest`.
    pub fn signature(&mut self, p: &Program, f: usize) -> Result<Signature, &'static str> {
        let fd = &p.fns[f];
        if fd.generics.iter().any(|g| g.region) {
            return Err("generic region parameters are not built");
        }
        let mut t = Local::new(&p.types);
        let arguments: Vec<_> = (0..fd.generics.len())
            .map(|i| t.mk(TyKind::Param(i as u32)))
            .collect();
        self.signature_inst(p, &mut t, f, &arguments)
    }
    fn signature_inst(
        &mut self,
        p: &Program,
        t: &mut impl TyRead,
        f: usize,
        arguments: &[TyId],
    ) -> Result<Signature, &'static str> {
        let fd = &p.fns[f];
        let mut params = vec![];
        for q in &fd.params {
            let ty = subst(t, q.ty, arguments);
            let rep = self.rep(p, t, ty)?;
            if q.mode == Mode::Mut || matches!(t.kind(q.ty), TyKind::Param(_)) {
                let id = self
                    .type_row(p, t, ty)
                    .ok_or("values of this type are not built")?;
                params.push(Param::Address(id));
                continue;
            }
            match rep {
                Rep::None => {}
                Rep::Scalar(s) => params.push(Param::Value(s)),
                Rep::View(_) => params.push(Param::Value(Ty::Ptr)),
                Rep::Place(id) => params.push(Param::Address(id)),
            }
        }
        let result = subst(t, fd.result, arguments);
        let never = matches!(t.kind(result), TyKind::Never);
        let result_rep = self.rep(p, t, result)?;
        let result_rep = if matches!(t.kind(fd.result), TyKind::Param(_)) {
            Rep::Place(
                self.type_row(p, t, result)
                    .ok_or("values of this type are not built")?,
            )
        } else {
            result_rep
        };
        let (results, dest) = match result_rep {
            Rep::None => (vec![], None),
            Rep::Scalar(s) => (vec![s], None),
            Rep::View(_) => (vec![Ty::Ptr], None),
            Rep::Place(id) => (vec![], Some(id)),
        };
        Ok(Signature {
            params,
            results,
            dest,
            never,
        })
    }
}

/// A field whose cleanup runs code by itself; the rows it holds go to `row`.
fn field_runs_code(t: &FieldTy, row: &mut impl FnMut(TypeId)) -> bool {
    match t {
        FieldTy::Inline(r) | FieldTy::Box(r) => {
            row(TypeId(r.0));
            false
        }
        FieldTy::Seq(e) | FieldTy::Array(e, _) => field_runs_code(e, row),
        FieldTy::Map(k, v) => field_runs_code(k, row) || field_runs_code(v, row),
        // An abandoned task or receiver cleans results or items whose type the handle
        // does not name, which may run finalizers (CONC-8).
        FieldTy::Handle(Handle::Task | Handle::Receiver) => true,
        _ => false,
    }
}

/// The CoreIR type of a scalar value of `ty`, nothing for unit and Never; an error for
/// a type carried as a place or not built (see [`Rep`]).
fn value_ty(t: &impl TyRead, ty: TyId) -> Result<Option<Ty>, &'static str> {
    match t.kind(ty) {
        TyKind::Scalar(s) => Ok(Some(Ty::Scalar(*s))),
        TyKind::Fn(_, _, _, Callable::Plain) => Ok(Some(Ty::Ptr)),
        TyKind::Unit | TyKind::Never | TyKind::Error => Ok(None),
        _ => Err("values of this type are not built"),
    }
}

fn int_of(t: &impl TyRead, ty: TyId) -> Option<(bool, u32)> {
    match t.kind(ty) {
        TyKind::Scalar(s) => Ty::Scalar(*s).int(),
        _ => None,
    }
}

fn is_float(t: &impl TyRead, ty: TyId) -> bool {
    matches!(t.kind(ty), TyKind::Scalar(Scalar::F32 | Scalar::F64))
}

impl Program<'_> {
    /// Check function `f`'s body and emit it into `e` (LANG-8: one visit, reading only
    /// this body, signatures and summaries).
    pub fn check_lower(&self, f: usize, e: &mut Emitter) -> CoreCheck {
        let fd = &self.fns[f];
        if self.item(fd.at).range.has_errors {
            return CoreCheck::default();
        }
        let Some(body) = fd.body else {
            return CoreCheck::default();
        };
        let mut cx = Cx::new(self, f, e);
        cx.run(f, body);
        let mut out = cx.finish();
        if self.entry.as_ref() == Some(&fd.path) {
            self.check_entry(fd, &mut out.refusals);
        }
        out
    }

    /// PROG-3: the entry's parameters are unmarked capability types or `Args`, and its
    /// result is `Result[(), E]`.
    fn check_entry(&self, fd: &FnDecl, refusals: &mut Vec<crate::Refusal>) {
        let core = |t: TyId, names: &[&str]| match self.types.kind(t) {
            TyKind::Adt(d, args) => {
                is_core_prelude(&self.decls[d.0 as usize].path, names).then_some(args.as_slice())
            }
            _ => None,
        };
        const GRANTS: [&str; 11] = [
            "Args", "Dir", "Net", "Clock", "Random", "Stdout", "Stderr", "Stdin", "Env", "Process",
            "Tasks",
        ];
        for param in &fd.params {
            if param.ty != ERROR && (param.mode != Mode::View || core(param.ty, &GRANTS).is_none())
            {
                refusals.push(refusal(
                    "PROG-3",
                    param.site,
                    "an entry parameter is an unmarked capability or `Args`",
                ));
            }
        }
        if fd.result != ERROR && core(fd.result, &["Result"]).is_none_or(|a| a[0] != UNIT) {
            refusals.push(refusal(
                "PROG-3",
                fd.result_site,
                "the entry returns `Result[(), E]`",
            ));
        }
    }
}

/// What an expression is checked against; every mismatch is LANG-11, at the innermost
/// expression the expected type reached.
#[derive(Clone, Copy)]
struct Want {
    ty: TyId,
    /// An argument for a view parameter: a view of `ty` is also accepted (OWN-7).
    view: bool,
    /// Number of success payloads expected by enclosing `?` operators. This
    /// constrains neither Result/Option shape nor any propagated error type.
    payload: u32,
}
fn expect(ty: TyId) -> Option<Want> {
    Some(Want {
        ty,
        view: false,
        payload: 0,
    })
}

/// A local's value: an immutable binding holds its value, a `let mut` binding a slot.
#[derive(Clone, Copy)]
enum Binding {
    Value(Option<ValueId>),
    Slot(Option<AddressId>),
}
/// One row of the slot table (ownership.md, Checker state).
#[derive(Clone, Copy)]
struct Var {
    ty: TyId,
    binding: Binding,
    origin: Origin,
    avail: Avail,
    /// Where its availability last changed.
    at: NodeId,
    /// Declaration order, so a loop tells the slots declared outside it (OWN-17).
    born: u32,
    /// It reached maybe at a join (OWN-18).
    flagged: bool,
    /// Freeze and reservation records naming it; with none, no record is searched.
    holds: u32,
    /// How it is cleaned, when it owns a value that needs cleanup.
    own: Option<cleanup::Own>,
}

/// A place and its type.
#[derive(Clone, Copy)]
struct At {
    place: Place,
    ty: TyId,
    /// Reached through a view or a stable value, so never written (OWN-12).
    shared: bool,
}

/// What a use of a place does (OWN-6, OWN-7, OWN-8).
#[derive(Clone, Copy)]
enum Use {
    /// A value position: a move when the type moves, else a copy.
    Value,
    /// `take p`.
    Take,
    View(Hold),
    Mut(Hold),
}

/// Per-slot scratch of one join or loop check, found by generation, never cleared.
#[derive(Clone, Copy)]
struct Acc {
    avail: Avail,
    count: u32,
    merge: u32,
    edge: u32,
}

/// A block every one of whose predecessors is known only as they are emitted; it is
/// made at the first edge, so a join no edge reaches is never made.
#[derive(Default)]
struct Join {
    block: Option<BlockId>,
    /// The row of the own value its edges carry, which is a temporary after it.
    drop: Option<TypeId>,
}

#[derive(Clone, Copy, PartialEq)]
enum FrameKind {
    Loop,
    While,
    Block,
}
struct Frame {
    label: Option<String>,
    kind: FrameKind,
    /// The loop header `continue` jumps to.
    header: Option<BlockId>,
    exit: usize,
    /// The value type its exits carry, once one fixes it.
    ty: Option<TyId>,
    /// Expected payload constraints survive until the first exit fixes its type.
    hint: Option<Want>,
    /// Whether any exit names it, reachable or not.
    left: bool,
    /// The undo log's length and the declaration count at its start.
    mark: usize,
    born: u32,
    /// Where its exit edges begin in `exits`.
    exits: usize,
    scopes: usize,
    /// The owners begun before it.
    owners: usize,
}

struct BodyInstance {
    definition: usize,
    arguments: PoolRange<TyId>,
    next: Option<usize>,
    row: (FuncId, SigId),
}

struct Cx<'c, 'a> {
    p: &'c Program<'a>,
    at: ItemRef,
    item: Item<'a>,
    fd: &'c FnDecl,
    params: Vec<u16>,
    t: Local<'c>,
    e: &'c mut Emitter,
    b: Body,
    func: Option<FuncId>,
    ir_item: ItemId,
    cur: Option<BlockId>,
    open: Vec<Inst>,
    open_anchors: Vec<Option<Anchor>>,
    /// Emission stopped: a refusal or a gap was recorded.
    broken: bool,
    /// The slot table, indexed by binder id.
    vars: Vec<Option<Var>>,
    /// Availability changes, undone to a mark when a branch ends: (slot, old, old site).
    log: Vec<(u32, Avail, NodeId)>,
    /// Join inputs: the slots one incoming edge changed and their states.
    diffs: Vec<(u32, Avail)>,
    /// Edges to open frames' exits: (frame, start, len) in `diffs`.
    exits: Vec<(usize, u32, u32)>,
    edge_buf: Vec<(u32, u32)>,
    acc: Vec<Acc>,
    touched: Vec<u32>,
    merges: u32,
    edges_seen: u32,
    born: u32,
    /// No path reaches this point (after `break`, `continue`, `return` or a Never call).
    dead: bool,
    /// Freeze and reservation records, and the steps of their places.
    freezes: Vec<Freeze>,
    steps: Vec<PStep>,
    /// Block depth, and the slots of the open blocks in declaration order.
    depth: u32,
    /// The type the last expression checked synthesised, before its expected type.
    synth_ty: TyId,
    /// The synthesised type of the tail of the block being closed; unit without one.
    tail_ty: TyId,
    scope_slots: Vec<u32>,
    copies: Vec<CopySize>,
    call_seq: u32,
    patterns: patterns::Patterns,
    /// Reused call workspaces, one for each active nesting level.
    call_scratch: Vec<calls::CallScratch>,
    /// Completed array elements still owned separately during construction.
    array_parts: Vec<ValueId>,
    instance_heads: HashMap<u64, usize>,
    instance_rows: Vec<BodyInstance>,
    instance_args: Vec<TyId>,
    binder_at: HashMap<(u32, u32), u32>,
    frames: Vec<Frame>,
    joins: Vec<Join>,
    /// Per node: 0 unknown, 1 synthesises a type, 2 needs an expected type.
    synth: Vec<u8>,
    waited: bool,
    /// The program's entry, which is always `waits` (LANG-8, PROG-3).
    entry: bool,
    /// The destination a declared result is written to: the entry block's first
    /// parameter and its place.
    dest: Option<(ValueId, TypeId)>,
    /// How each type this body names is carried, found once per body.
    reps: HashMap<TyId, Result<Rep, &'static str>>,
    /// The owners of the body, in the order they began (FAIL-8).
    owners: Vec<cleanup::Owner>,
    owner_buf: Vec<cleanup::Owner>,
    /// Counts changes of owner and scope state; a failure successor made at one
    /// state serves every failing instruction until it changes.
    epoch: u64,
    fail_cache: Option<((u64, usize, usize), BlockId)>,
    out: CoreCheck,
}

impl<'c, 'a> Cx<'c, 'a> {
    fn new(p: &'c Program<'a>, f: usize, e: &'c mut Emitter) -> Self {
        let fd = &p.fns[f];
        let item = p.item(fd.at);
        let info = &p.items[fd.at.module as usize][fd.at.item as usize];
        let binder_at = info
            .resolution
            .binders
            .iter()
            .map(|b| ((b.site.span.start, b.site.span.end), b.id))
            .collect();
        let ir_item = e.item(&fd.path);
        Cx {
            p,
            at: fd.at,
            item,
            fd,
            params: fd.generics.iter().map(|g| g.bounds).collect(),
            t: Local::new(&p.types),
            e,
            b: Body::default(),
            func: None,
            ir_item,
            cur: None,
            open: vec![],
            open_anchors: vec![],
            broken: false,
            vars: vec![None; info.resolution.binders.len()],
            log: vec![],
            diffs: vec![],
            exits: vec![],
            edge_buf: vec![],
            acc: vec![
                Acc {
                    avail: Avail::Live,
                    count: 0,
                    merge: 0,
                    edge: 0,
                };
                info.resolution.binders.len()
            ],
            touched: vec![],
            merges: 0,
            edges_seen: 0,
            born: 0,
            dead: false,
            freezes: vec![],
            steps: vec![],
            depth: 0,
            synth_ty: UNIT,
            tail_ty: UNIT,
            scope_slots: vec![],
            copies: vec![],
            call_seq: 0,
            patterns: patterns::Patterns::default(),
            call_scratch: vec![],
            array_parts: vec![],
            instance_heads: HashMap::new(),
            instance_rows: vec![],
            instance_args: vec![],
            binder_at,
            frames: vec![],
            joins: vec![],
            synth: vec![0; (item.range.nodes.end - item.range.nodes.start) as usize],
            waited: false,
            entry: p.entry.as_ref() == Some(&fd.path),
            dest: None,
            reps: HashMap::new(),
            owners: vec![],
            owner_buf: vec![],
            epoch: 0,
            fail_cache: None,
            out: CoreCheck::default(),
        }
    }

    // ---- syntax
    fn kind(&self, n: NodeId) -> NodeKind {
        self.item.tree.node(n).kind
    }
    fn kids(&self, n: NodeId) -> &'a [NodeId] {
        let item = self.item;
        item.tree.children(n)
    }
    fn token_text(&self, n: NodeId) -> &'a str {
        let item = self.item;
        match item.tree.node(n).token {
            Some(t) => item.tree.token_text(item.text, t),
            None => "",
        }
    }
    fn token_kind(&self, n: NodeId) -> Option<TokenKind> {
        let t = self.item.tree.node(n).token?;
        Some(self.item.tree.tokens[t.0 as usize].kind)
    }
    fn binder(&self, n: NodeId) -> Option<u32> {
        let s = self.item.tree.node(n).origin.span;
        self.binder_at.get(&(s.start, s.end)).copied()
    }
    fn name(&self, ty: TyId) -> String {
        crate::decls::canonical(&self.p.decls, &self.t, ty)
    }

    // ---- outcomes
    fn refuse(&mut self, rule: &str, n: NodeId, message: impl Into<String>) {
        self.refuse_at(rule, self.span(n), message);
    }
    fn refuse_at(&mut self, rule: &str, span: Span, message: impl Into<String>) {
        let site = self.p.site_span(self.at, span);
        self.out.refusals.push(refusal(rule, site, message));
        self.halt();
    }
    fn span(&self, n: NodeId) -> Span {
        self.item.tree.node(n).origin.span
    }
    /// The operator token of a unary, binary or compound-assignment node `n`.
    fn op_span(&self, n: NodeId) -> Span {
        match self.item.tree.node(n).token {
            Some(t) => self.item.token_span(t),
            None => self.span(n),
        }
    }
    /// The closing `}` (or `)`) of node `n`.
    fn close_brace(&self, n: NodeId) -> Span {
        let s = self.span(n);
        Span {
            start: s.end.saturating_sub(1),
            end: s.end,
        }
    }
    /// A construct not built yet; each one is reported once per body.
    fn gap(&mut self, n: NodeId, what: &'static str) {
        if !self.out.gaps.iter().any(|g| g.what == what) {
            let site = self.p.site(self.at, n);
            self.out.gaps.push(Gap::new(site, what));
        }
        self.halt();
    }
    fn halt(&mut self) {
        self.broken = true;
        self.cur = None;
    }
    /// `t` where `w` is expected: equal, or Never, or an already refused type.
    fn expected_part(&self, mut ty: TyId, want: Want) -> Option<TyId> {
        for _ in 0..want.payload {
            ty = self.result_parts(ty)?.1;
        }
        Some(ty)
    }
    /// A success projection that reaches a still-rigid parameter follows that
    /// parameter into its argument, without inventing its Result/Option shape.
    fn payload_argument(&self, mut result: TyId, argument: TyId, mut want: Want) -> Option<Want> {
        for depth in 0..want.payload {
            if result == argument && matches!(self.t.kind(result), TyKind::Param(_)) {
                want.payload -= depth;
                return Some(want);
            }
            result = self.result_parts(result)?.1;
        }
        None
    }
    fn coerce(&mut self, t: TyId, mut w: Want, n: NodeId) {
        let Some(t) = self.expected_part(t, w) else {
            return;
        };
        w.payload = 0;
        self.out.counts.types += 1;
        let plain_view = self.plain_view(t, w.ty);
        let viewed = w.view
            && (matches!(self.t.kind(t), TyKind::View(x) if *x == w.ty)
                || crate::decls::stable_parameter(&self.p.decls, &self.t, t, w.ty));
        let callable = matches!((self.t.kind(t), self.t.kind(w.ty)),
            (TyKind::Fn(a, ar, aw, ak), TyKind::Fn(b, br, bw, bk))
                if a == b && ar == br && ak == bk && (!aw || *bw));
        if matches!((self.t.kind(t), self.t.kind(w.ty)),
            (TyKind::Fn(a, ar, aw, ak), TyKind::Fn(b, br, bw, bk))
                if a == b && ar == br && (!aw || *bw) && ak != bk
                    && matches!((ak,bk), (Callable::Plain, Callable::Own | Callable::OwnSend | Callable::Scoped) | (Callable::OwnSend, Callable::Own | Callable::Scoped) | (Callable::Own,Callable::Scoped)))
        {
            self.gap(n, "callable kind conversions are not built");
            return;
        }
        let sequence = matches!((self.t.kind(w.ty), self.t.kind(t)),
            (TyKind::Adt(d, a), TyKind::Adt(e, b))
                if a == b && is_core_prelude(&self.p.decls[d.index()].path, &["SeqSpan"])
                    && is_core_prelude(&self.p.decls[e.index()].path, &["IndexedSpan"]));
        // OWN-25: a stable value is read where a view of its type is expected.
        let stable = matches!(self.t.kind(w.ty), TyKind::View(x)
            if crate::decls::stable_parameter(&self.p.decls, &self.t, t, *x));
        if t != w.ty
            && !viewed
            && !plain_view
            && !sequence
            && !callable
            && !stable
            && t != NEVER
            && !mentions_error(&self.t, t)
            && !mentions_error(&self.t, w.ty)
        {
            let (a, b) = (self.name(w.ty), self.name(t));
            self.refuse("LANG-11", n, format!("expected `{a}`, found `{b}`"));
        }
    }

    // ---- emission
    fn live(&self) -> bool {
        self.cur.is_some()
    }
    fn anchor(&self, n: NodeId) -> Anchor {
        let s = self.item.tree.node(n).origin.span;
        Anchor {
            item: self.ir_item,
            start: s.start,
            end: s.end,
        }
    }
    fn value(&mut self, t: Ty) -> ValueId {
        self.b.values.push(t);
        ValueId(self.b.values.len() as u32 - 1)
    }
    fn inst(&mut self, i: Inst, n: NodeId) {
        if self.live() {
            self.open.push(i);
            self.open_anchors.push(Some(self.anchor(n)));
        }
    }
    fn konst(&mut self, t: Ty, bits: u64, n: NodeId) -> Option<ValueId> {
        if !self.live() {
            return None;
        }
        let result = self.value(t);
        self.inst(Inst::Const { result, bits }, n);
        Some(result)
    }
    /// How a value of `ty` is carried in this body.
    fn rep(&mut self, ty: TyId) -> Result<Rep, &'static str> {
        if let Some(&r) = self.reps.get(&ty) {
            return r;
        }
        let r = self.e.rep(self.p, &mut self.t, ty);
        self.reps.insert(ty, r);
        r
    }
    /// The CoreIR type of a value of `ty`; None for unit, Never and types not built.
    fn ir_ty(&mut self, ty: TyId) -> Option<Ty> {
        self.rep(ty).ok().and_then(Rep::ty)
    }
    /// An address. A resource row's place is the holder of its block pointer, except
    /// that a pointer value typed by the row, with steps after it, is the block
    /// (FORM): fields of a resource are reached only through [`Self::fields_of`].
    fn address(&mut self, base: AddressBase, steps: &[Step], ty: PlaceTy, n: NodeId) -> AddressId {
        let fail = if steps.iter().any(|s| matches!(s, Step::Element(_))) {
            self.fail_to(n)
        } else {
            None
        };
        let steps = PoolRange::push(&mut self.b.pools.steps, steps.iter().copied());
        self.b.addresses.push(AddressInfo { base, steps, ty });
        let address = AddressId(self.b.addresses.len() as u32 - 1);
        self.inst(Inst::Address { address, fail }, n);
        address
    }
    /// The place of row `id`'s fields for the value at `base` (a place of row `id`):
    /// the place itself, or for a resource the block its holder points at.
    fn fields_of(&mut self, base: AddressBase, id: TypeId, n: NodeId) -> AddressId {
        if !self.e.is_resource(id) {
            return self.address(base, &[], PlaceTy::Declared(id), n);
        }
        let at = self.address(base, &[], PlaceTy::Value(Ty::Ptr), n);
        let block = self.value(Ty::Ptr);
        self.inst(
            Inst::Read {
                result: block,
                address: at,
                mode: ReadMode::Bits,
            },
            n,
        );
        self.address(AddressBase::Value(block), &[], PlaceTy::Declared(id), n)
    }
    /// A new slot of `ty`, begun here, and its address.
    fn new_slot(&mut self, ty: PlaceTy, n: NodeId) -> AddressId {
        self.out.counts.places += 1;
        self.b.slots.push(ty);
        let slot = SlotId(self.b.slots.len() as u32 - 1);
        self.inst(Inst::StorageLive { slot }, n);
        self.address(AddressBase::Slot(slot), &[], ty, n)
    }
    fn pointer(&mut self, address: AddressId, n: NodeId) -> ValueId {
        let result = self.value(Ty::Ptr);
        self.inst(Inst::Pointer { result, address }, n);
        result
    }
    /// Copy the value of row `id` that `from` points at into place `to` (a move or a
    /// copy of plain data: the source needs no cleanup afterwards).
    fn copy_to(&mut self, to: AddressId, from: ValueId, id: TypeId, n: NodeId) {
        if self.e.needs_drop(id) {
            self.consume(Some(from), n);
        }
        let from = self.address(AddressBase::Value(from), &[], PlaceTy::Declared(id), n);
        let size = self.e.size(id);
        let bytes = self.value(Ty::Scalar(Scalar::U64));
        if self.e.generic_names.contains_key(&id) {
            self.copies.push(CopySize {
                value: bytes,
                ty: id,
            });
        }
        self.inst(
            Inst::Const {
                result: bytes,
                bits: size,
            },
            n,
        );
        self.inst(Inst::Copy { to, from, bytes }, n);
    }
    /// A fresh place holding a copy of the value `from` points at; its pointer.
    fn snapshot(&mut self, from: ValueId, id: TypeId, n: NodeId) -> ValueId {
        let to = self.new_slot(PlaceTy::Declared(id), n);
        self.copy_to(to, from, id, n);
        self.pointer(to, n)
    }
    /// Return `v`, a value of the body's result type: a declared result is written to
    /// the destination first.
    fn ret(&mut self, v: Option<ValueId>, n: NodeId) {
        if !self.live() {
            return;
        }
        let v = match (self.dest, v) {
            (Some((dest, id)), Some(v)) => {
                let to = self.address(AddressBase::Value(dest), &[], PlaceTy::Declared(id), n);
                self.copy_to(to, v, id, n);
                None
            }
            _ => v,
        };
        self.unwind(0, 0, n);
        let r = PoolRange::push(&mut self.b.pools.values, v);
        self.end(Terminator::Return(r), Some(n));
    }

    fn new_block(&mut self, tys: &[Ty]) -> BlockId {
        let params: Vec<ValueId> = tys.iter().map(|&t| self.value(t)).collect();
        let params = PoolRange::push(&mut self.b.pools.values, params);
        self.b.blocks.push(Block {
            params,
            insts: PoolRange::EMPTY,
            anchor: None,
        });
        // Replaced when the block ends; every block made is started and ended.
        self.b.terminators.push(Terminator::Propagate);
        BlockId(self.b.blocks.len() as u32 - 1)
    }
    fn start(&mut self, blk: BlockId) {
        if !self.broken {
            debug_assert!(self.cur.is_none());
            self.cur = Some(blk);
        }
    }
    fn end(&mut self, term: Terminator, n: Option<NodeId>) {
        let Some(c) = self.cur.take() else { return };
        let insts = PoolRange::push(&mut self.b.insts, self.open.drain(..));
        self.b.inst_anchors.append(&mut self.open_anchors);
        let anchor = n.map(|n| self.anchor(n));
        let blk = &mut self.b.blocks[c.index()];
        blk.insts = insts;
        blk.anchor = anchor;
        self.b.terminators[c.index()] = term;
    }
    /// Blocks end in visit order, which is not block order (a join is made before the
    /// arms that reach it end); a body's instructions are laid out in block order.
    fn block_order(&mut self) {
        let b = &mut self.b;
        let mut insts = Vec::with_capacity(b.insts.len());
        let mut anchors = Vec::with_capacity(b.insts.len());
        for blk in &mut b.blocks {
            let (start, len) = (blk.insts.start as usize, blk.insts.len as usize);
            let at = insts.len() as u32;
            insts.extend_from_slice(&b.insts[start..start + len]);
            anchors.extend_from_slice(&b.inst_anchors[start..start + len]);
            blk.insts = PoolRange::new(at, len as u32);
        }
        b.insts = insts;
        b.inst_anchors = anchors;
    }
    fn edge(&mut self, target: BlockId, args: &[ValueId]) -> EdgeId {
        let args = PoolRange::push(&mut self.b.pools.values, args.iter().copied());
        self.b.edges.push(Edge { target, args });
        EdgeId(self.b.edges.len() as u32 - 1)
    }
    fn jump(&mut self, target: BlockId, args: &[ValueId], n: NodeId) {
        if self.live() {
            let e = self.edge(target, args);
            self.end(Terminator::Jump(e), Some(n));
        }
    }
    fn block_params(&self, blk: BlockId) -> Vec<ValueId> {
        let r = self.b.blocks[blk.index()].params;
        r.get(&self.b.pools.values).unwrap_or_default().to_vec()
    }
    fn new_join(&mut self) -> usize {
        self.joins.push(Join::default());
        self.joins.len() - 1
    }
    /// Leave the current block for join `j`, whose parameters carry a value of `ty`.
    fn goto(&mut self, j: usize, ty: TyId, v: Option<ValueId>, n: NodeId) {
        if !self.live() {
            return;
        }
        let blk = match self.joins[j].block {
            Some(b) => b,
            None => {
                let tys: Vec<Ty> = self.ir_ty(ty).into_iter().collect();
                let b = self.new_block(&tys);
                self.joins[j].block = Some(b);
                b
            }
        };
        if let Some(id) = self.drop_id(ty)
            && v.is_some()
        {
            self.consume(v, n);
            self.joins[j].drop = Some(id);
        }
        let args: Vec<ValueId> = v.into_iter().collect();
        self.jump(blk, &args, n);
    }
    /// Continue at join `j`, if any edge reached it; its parameter is the joined value.
    fn land(&mut self, j: usize) -> Option<ValueId> {
        let blk = self.joins[j].block?;
        self.start(blk);
        let v = self.block_params(blk).first().copied();
        if let (Some(id), Some(value), true) = (self.joins[j].drop, v, self.live()) {
            self.owners.push(cleanup::Owner::Temp(cleanup::Temp {
                value,
                id,
                avail: Avail::Live,
                flag: None,
            }));
            self.epoch += 1;
        }
        v
    }

    // ---- the definition
    fn run(&mut self, f: usize, body: NodeId) {
        match self.e.row(self.p, f) {
            Ok((func, sig)) => {
                self.func = Some(func);
                let s = &self.e.module.signatures[sig.index()];
                // The destination first, then the parameters; an address arrives as a
                // pointer (Body: block 0's parameters).
                let dest = s.dest;
                let tys: Vec<Ty> = dest
                    .map(|_| Ty::Ptr)
                    .into_iter()
                    .chain(s.params.iter().map(|p| match p {
                        Param::Value(t) => *t,
                        Param::Address(_) => Ty::Ptr,
                    }))
                    .collect();
                let entry = self.new_block(&tys);
                self.start(entry);
                if let Some(id) = dest {
                    self.dest = Some((self.block_params(entry)[0], id));
                }
            }
            Err(what) => {
                let site = self.fd.site;
                self.out.gaps.push(Gap::new(site, what));
                self.halt();
            }
        }
        let entry_params = if self.func.is_some() {
            self.block_params(BlockId(0))
        } else {
            vec![]
        };
        let mut k = usize::from(self.dest.is_some());
        let p: &'c Program<'a> = self.p;
        let binders = &p.items[self.at.module as usize][self.at.item as usize]
            .resolution
            .binders;
        for pd in &self.fd.params {
            let v = match self.ir_ty(pd.ty) {
                _ if pd.mode == Mode::Mut && k < entry_params.len() => {
                    k += 1;
                    Some(entry_params[k - 1])
                }
                Some(_) if k < entry_params.len() => {
                    k += 1;
                    Some(entry_params[k - 1])
                }
                _ => None,
            };
            if let Some(b) = binders
                .iter()
                .find(|b| b.name == pd.name && b.kind == hirsel_core_resolve::BinderKind::Parameter)
            {
                let origin = match pd.mode {
                    Mode::View => Origin::ViewParam,
                    Mode::Mut => Origin::MutParam,
                    Mode::Take => Origin::TakeParam,
                };
                let binding = match (pd.mode, v) {
                    (Mode::Mut, Some(v)) if self.live() => {
                        let ty = match self.rep(pd.ty) {
                            Ok(Rep::None) => self
                                .e
                                .type_row(p, &mut self.t, pd.ty)
                                .map(PlaceTy::Declared),
                            Ok(r) => r.stored(),
                            Err(_) => None,
                        };
                        Binding::Slot(
                            ty.map(|ty| self.address(AddressBase::Value(v), &[], ty, body)),
                        )
                    }
                    (Mode::Take, _) => Binding::Slot(self.slot(pd.ty, v, body)),
                    _ => Binding::Value(v),
                };
                self.declare(b.id, pd.ty, binding, origin, body);
                self.own(b.id, body);
            }
        }
        let result = self.fd.result;
        let (_, v) = self.expr(
            body,
            Some(Want {
                ty: result,
                view: false,
                payload: 0,
            }),
        );
        self.ret(v, body);
        // Only a body checked to its end shows that nothing it calls waits.
        if self.fd.waits && !self.waited && !self.entry && !self.broken {
            let root = self.item.root;
            let w = self
                .kids(root)
                .iter()
                .copied()
                .find(|&c| self.kind(c) == NodeKind::Waits)
                .unwrap_or(root);
            self.refuse("LANG-8", w, "declared `waits` but calls nothing that waits");
        }
    }
    fn finish(mut self) -> CoreCheck {
        let b = &self.b;
        let size = |n: usize, s: usize| (n * s) as u64;
        self.out.counts.bytes = size(b.values.len(), size_of::<Ty>())
            + size(b.slots.len(), size_of::<PlaceTy>())
            + size(b.addresses.len(), size_of::<AddressInfo>())
            + size(b.blocks.len(), size_of::<Block>())
            + size(b.terminators.len(), size_of::<Terminator>())
            + size(b.insts.len(), size_of::<Inst>())
            + size(b.inst_anchors.len(), size_of::<Option<Anchor>>())
            + size(b.calls.len(), size_of::<CallSite>())
            + size(b.edges.len(), size_of::<Edge>())
            + size(b.pools.values.len(), size_of::<ValueId>())
            + size(b.pools.args.len(), size_of::<Arg>());
        if !self.broken
            && let Some(f) = self.func
        {
            self.block_order();
            self.e.module.functions[f.index()].body = Some(std::mem::take(&mut self.b));
            self.out.function = Some(f);
            if !self.fd.generics.is_empty() {
                let mut parameters = vec![];
                for i in 0..self.fd.generics.len() {
                    let ty = self.t.mk(TyKind::Param(i as u32));
                    parameters.push(self.e.type_row(self.p, &mut self.t, ty));
                }
                let template = GenericTemplate {
                    parameters,
                    copies: std::mem::take(&mut self.copies),
                    types: self.e.generic_names.clone(),
                    calls: self.e.generic_calls.clone(),
                };
                #[cfg(test)]
                self.e
                    .templates
                    .insert(self.fd.path.clone(), (f, template.clone()));
                self.out.template = Some(template);
            }
        }
        self.out
    }

    // ---- expressions
    /// Check `n` against `want` when given; a construct that passes `want` inward checks
    /// it there and returns `want`'s type, so the outer check here passes.
    fn expr(&mut self, n: NodeId, want: Option<Want>) -> (TyId, Option<ValueId>) {
        self.out.counts.nodes += 1;
        let (t, v) = self.expr_inner(n, want);
        self.synth_ty = t;
        match want {
            Some(w) if t != NEVER && w.payload != 0 => {
                self.coerce(t, w, n);
                (t, v)
            }
            Some(w) if t != NEVER => {
                self.coerce(t, w, n);
                let v = if self.plain_view(t, w.ty) {
                    self.read_plain_view(w.ty, v, n)
                } else {
                    v
                };
                (w.ty, v)
            }
            _ => (t, v),
        }
    }

    /// `n` typed with `want` as its expected type but not coerced to it: the caller
    /// compares the synthesised type itself (LANG-21's right operand).
    fn expr_hinted(&mut self, n: NodeId, want: Option<Want>) -> (TyId, Option<ValueId>) {
        self.out.counts.nodes += 1;
        let (t, v) = self.expr_inner(n, want);
        self.synth_ty = t;
        (t, v)
    }

    fn expr_inner(&mut self, n: NodeId, want: Option<Want>) -> (TyId, Option<ValueId>) {
        let c = self.kids(n);
        let want = want.filter(|w| {
            w.payload == 0
                || matches!(
                    self.kind(n),
                    NodeKind::Group
                        | NodeKind::Path
                        | NodeKind::Block
                        | NodeKind::If
                        | NodeKind::Loop
                        | NodeKind::Labelled
                        | NodeKind::Call
                        | NodeKind::Try
                        | NodeKind::Match
                        | NodeKind::Region
                        | NodeKind::Bound
                )
        });
        match self.kind(n) {
            NodeKind::Group => self.expr(c[0], want),
            NodeKind::Literal => self.literal(n, want, false),
            NodeKind::Unit => (UNIT, None),
            NodeKind::Path => self.path(n, want),
            NodeKind::Block => self.block(n, want),
            NodeKind::If => self.if_(n, want),
            NodeKind::Loop => self.loop_(n, want, None),
            NodeKind::While => self.while_(n, None),
            NodeKind::Labelled => self.labelled(n, want),
            NodeKind::Return => self.return_(n),
            NodeKind::Break => self.break_(n),
            NodeKind::Continue => self.continue_(n),
            NodeKind::Unary | NodeKind::Binary
                if let Some(sc) = want.and_then(|w| match self.t.kind(w.ty) {
                    TyKind::Scalar(sc) if numeric(*sc) => Some(*sc),
                    _ => None,
                }) && !self.synthesises(n) =>
            {
                self.literal_operand(n, sc)
            }
            NodeKind::Unary => self.unary(n, want),
            NodeKind::Binary => self.binary(n, want),
            NodeKind::Call => self.call(n, want),
            NodeKind::Error => {
                // The parser refused it already.
                self.halt();
                (ERROR, None)
            }
            NodeKind::Array | NodeKind::RepeatArray => self.array(n, want),
            NodeKind::Tuple => self.tuple(n, want),
            NodeKind::Index | NodeKind::Projection => self.place_value(n),
            NodeKind::Try => self.try_(n, want),
            NodeKind::Match => self.match_(n, want),
            NodeKind::For | NodeKind::ForTake => self.for_(n, None),
            NodeKind::Region => self.region_(n, want),
            NodeKind::Bound => self.bound_(n, want),
            NodeKind::Placement => self.gap_ty(n, "stable placement is not built"),
            NodeKind::Closure | NodeKind::ScopedClosure => self.gap_ty(n, "closures are not built"),
            _ => self.gap_ty(n, "expression form is not built"),
        }
    }
    /// A place in a value position: moved when its type moves, else copied (OWN-6).
    fn place_value(&mut self, n: NodeId) -> (TyId, Option<ValueId>) {
        match self.place(n) {
            Ok(Some(at)) => {
                if self.borrowed_index(at) {
                    self.use_place(at, Use::View(Hold::View(Extent::Temp)), n);
                    let ty = self.t.mk(TyKind::View(at.ty));
                    return (ty, self.place_ptr(at, n));
                }
                self.use_place(at, Use::Value, n);
                let v = self.emit_place(at, n);
                if at.place.len == 0 && !self.copies(at.ty) {
                    self.temp(at.ty, v);
                }
                (at.ty, v)
            }
            Ok(None) => self.gap_ty(n, "field and index places are not built"),
            Err(()) => (ERROR, None),
        }
    }
    fn gap_ty(&mut self, n: NodeId, what: &'static str) -> (TyId, Option<ValueId>) {
        self.gap(n, what);
        (ERROR, None)
    }

    /// Whether `n` has a type without an expected one (LANG-11): everything but a
    /// literal operand (LANG-21). Memoised per node, so
    /// operator chains stay linear.
    fn synthesises(&mut self, n: NodeId) -> bool {
        self.out.counts.nodes += 1;
        match self.synth[(n.0 - self.item.range.nodes.start) as usize] {
            1 => return true,
            2 => return false,
            _ => {}
        }
        let c = self.kids(n);
        let r = match self.kind(n) {
            NodeKind::Literal => !self.unsuffixed_number(n),
            NodeKind::Group => self.synthesises(c[0]),
            // LANG-21: a literal operand is an unsuffixed number, or a unary `-` or `~`,
            // a binary operator or parentheses over literal operands only.
            NodeKind::Unary => !matches!(self.token_text(n), "-" | "~") || self.synthesises(c[0]),
            NodeKind::Binary => self.synthesises(c[0]) || self.synthesises(c[1]),
            _ => true,
        };
        self.synth[(n.0 - self.item.range.nodes.start) as usize] = if r { 1 } else { 2 };
        r
    }
}

/// Whether `ty` is or contains the error type: a part that already produced a refusal
/// or gap, so a comparison involving it is not refused again.
fn mentions_error(t: &impl TyRead, ty: TyId) -> bool {
    match t.kind(ty) {
        TyKind::Error => true,
        TyKind::Tuple(xs) | TyKind::Adt(_, xs) => xs.iter().any(|&x| mentions_error(t, x)),
        TyKind::Array(x, _) | TyKind::View(x) => mentions_error(t, *x),
        TyKind::Fn(ps, r, _, _) => {
            mentions_error(t, *r) || ps.iter().any(|&(_, x)| mentions_error(t, x))
        }
        TyKind::Anchored(x, root) => mentions_error(t, *x) || mentions_error(t, *root),
        TyKind::Never
        | TyKind::Unit
        | TyKind::Scalar(_)
        | TyKind::Param(_)
        | TyKind::Hole
        | TyKind::Region(_) => false,
    }
}
