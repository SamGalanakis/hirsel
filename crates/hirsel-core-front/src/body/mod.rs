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
    DeclBody, ENCODED, FnDecl, KEY, Kind, ORDERED, Repr, Role, copyable, has_bounds,
    is_core_prelude, kind,
};
use crate::own::{Act, Avail, Extent, Freeze, Hold, Origin, PStep, Place, breaks, join, overlap};
use crate::types::*;
use crate::{Def, Gap, ItemRef, Program, refusal};
use hirsel_core_ids::DefPath;
use hirsel_core_ir::{
    AddressBase, AddressId, AddressInfo, Anchor, Arg, ArithMode, ArithOp, BinaryOp, Block, BlockId,
    Body, CallId, CallSite, Callee, CompareOp, ConvertMode, Edge, EdgeId, FuncId, Function, Inst,
    InstanceId, InstanceKey, ItemId, Module, Param, PlaceTy, PoolRange, ReadMode, SigId, Signature,
    SlotId, Step, Terminator, Ty, TypeId, UnaryOp, ValueId,
};
use hirsel_core_layout::{Layout, Recipe, Scalar, TypeRef};
use hirsel_core_resolve::{Refusal, Target};
use hirsel_core_syntax::{Item, NodeId, NodeKind, Span, TokenKind, byte_value, character_value};
use std::collections::HashMap;

mod calls;
mod control;
mod ops;
mod places;
mod types;
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
}

/// How a value of a type is carried in a body (crate docs, "Values, slots and
/// addresses"): a scalar, nothing (unit, Never), or a pointer to a place of a declared
/// type, which a parameter takes by address and a result returns through `dest`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Rep {
    None,
    Scalar(Ty),
    Place(TypeId),
}
impl Rep {
    /// The CoreIR type of the value: a place travels as its pointer.
    fn ty(self) -> Option<Ty> {
        match self {
            Rep::None => None,
            Rep::Scalar(t) => Some(t),
            Rep::Place(_) => Some(Ty::Ptr),
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
    /// The function row of a non-generic definition.
    pub fn function(&mut self, path: &DefPath, sig: Signature) -> FuncId {
        let key = InstanceKey {
            definition: path.clone(),
            types: vec![],
            dictionaries: vec![],
        };
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
        Some(TypeId(r?.0))
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
                    return None;
                }
            }
        }
        self.recipes.get(id.index())
    }
    /// How a value of `ty` is carried. A place value is built only for plain own data
    /// for now: cleanup of the rest is not emitted yet.
    fn rep(&mut self, p: &Program, t: &mut impl TyRead, ty: TyId) -> Result<Rep, &'static str> {
        match t.kind(ty) {
            TyKind::Scalar(s) => return Ok(Rep::Scalar(Ty::Scalar(*s))),
            TyKind::Unit | TyKind::Never | TyKind::Error => return Ok(Rep::None),
            // A capability or `Args` is a provider handle, plain and never cleaned by
            // the program (FORM e68de117).
            TyKind::Adt(d, _) if crate::layout::handle(&p.decls[d.index()]) => {
                return Ok(Rep::Scalar(Ty::Scalar(Scalar::U64)));
            }
            TyKind::Tuple(_) | TyKind::Array(..) | TyKind::Adt(..) => {}
            _ => return Err("values of this type are not built"),
        }
        let mark = self.module.types.len();
        let id = self
            .type_row(p, t, ty)
            .ok_or("values of this type are not built")?;
        match self.recipe(id, mark) {
            Some(r) if matches!(r.layout, Layout::Fixed(_)) && r.plain => Ok(Rep::Place(id)),
            Some(r) if matches!(r.layout, Layout::Fixed(_)) => {
                Err("cleanup of own values is not built")
            }
            _ => Err("values of this type are not built"),
        }
    }
    /// The byte size of a value of row `id`, whose recipe exists.
    fn size(&self, id: TypeId) -> u64 {
        match &self.recipes[id.index()].layout {
            Layout::Fixed(f) => f.size,
            Layout::Compact(_) => unreachable!("only fixed layouts are carried"),
        }
    }
    /// The CoreIR signature of the program's function `f`, or the gap that keeps it
    /// from having one: scalars by value, declared types by address, a declared result
    /// through `dest`.
    pub fn signature(&mut self, p: &Program, f: usize) -> Result<Signature, &'static str> {
        let fd = &p.fns[f];
        if !fd.generics.is_empty() {
            return Err("generic definitions are not built");
        }
        let mut t = Local::new(&p.types);
        let mut params = vec![];
        for q in &fd.params {
            if q.mode == Mode::Mut {
                return Err("mut parameters are not built");
            }
            match self.rep(p, &mut t, q.ty)? {
                Rep::None => {}
                Rep::Scalar(s) => params.push(Param::Value(s)),
                Rep::Place(id) => params.push(Param::Address(id)),
            }
        }
        let never = matches!(p.types.kind(fd.result), TyKind::Never);
        let (results, dest) = match self.rep(p, &mut t, fd.result)? {
            Rep::None => (vec![], None),
            Rep::Scalar(s) => (vec![s], None),
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

/// The CoreIR type of a scalar value of `ty`, nothing for unit and Never; an error for
/// a type carried as a place or not built (see [`Rep`]).
fn value_ty(t: &impl TyRead, ty: TyId) -> Result<Option<Ty>, &'static str> {
    match t.kind(ty) {
        TyKind::Scalar(s) => Ok(Some(Ty::Scalar(*s))),
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
}
fn expect(ty: TyId) -> Option<Want> {
    Some(Want { ty, view: false })
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
    /// Whether any exit names it, reachable or not.
    left: bool,
    /// The undo log's length and the declaration count at its start.
    mark: usize,
    born: u32,
    /// Where its exit edges begin in `exits`.
    exits: usize,
}

struct Cx<'c, 'a> {
    p: &'c Program<'a>,
    at: ItemRef,
    item: Item<'a>,
    fd: &'c FnDecl,
    params: Vec<u8>,
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
    call_seq: u32,
    /// Reused call workspaces, one for each active nesting level.
    call_scratch: Vec<calls::CallScratch>,
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
            call_seq: 0,
            call_scratch: vec![],
            binder_at,
            frames: vec![],
            joins: vec![],
            synth: vec![0; (item.range.nodes.end - item.range.nodes.start) as usize],
            waited: false,
            entry: p.entry.as_ref() == Some(&fd.path),
            dest: None,
            reps: HashMap::new(),
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
    fn coerce(&mut self, t: TyId, w: Want, n: NodeId) {
        self.out.counts.types += 1;
        let viewed = w.view && matches!(self.t.kind(t), TyKind::View(x) if *x == w.ty);
        let sequence = matches!((self.t.kind(w.ty), self.t.kind(t)),
            (TyKind::Adt(d, a), TyKind::Adt(e, b))
                if a == b && is_core_prelude(&self.p.decls[d.index()].path, &["SeqSpan"])
                    && is_core_prelude(&self.p.decls[e.index()].path, &["IndexedSpan"]));
        // OWN-25: a stable value is read where a view of its type is expected.
        let stable = matches!((self.t.kind(w.ty), self.t.kind(t)),
            (TyKind::View(x), TyKind::Adt(d, _))
                if *x == t && matches!(self.p.decls[d.index()].repr, Repr::Stable));
        if t != w.ty
            && !viewed
            && !sequence
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
    /// Evaluate a new address (`Inst::Address`).
    fn address(&mut self, base: AddressBase, steps: &[Step], ty: PlaceTy, n: NodeId) -> AddressId {
        let steps = PoolRange::push(&mut self.b.pools.steps, steps.iter().copied());
        self.b.addresses.push(AddressInfo { base, steps, ty });
        let address = AddressId(self.b.addresses.len() as u32 - 1);
        self.inst(
            Inst::Address {
                address,
                fail: None,
            },
            n,
        );
        address
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
        let from = self.address(AddressBase::Value(from), &[], PlaceTy::Declared(id), n);
        let size = self.e.size(id);
        let bytes = self.value(Ty::Scalar(Scalar::U64));
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
        let args: Vec<ValueId> = v.into_iter().collect();
        self.jump(blk, &args, n);
    }
    /// Continue at join `j`, if any edge reached it; its parameter is the joined value.
    fn land(&mut self, j: usize) -> Option<ValueId> {
        let blk = self.joins[j].block?;
        self.start(blk);
        self.block_params(blk).first().copied()
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
                self.declare(b.id, pd.ty, Binding::Value(v), origin, body);
            }
        }
        let result = self.fd.result;
        let (_, v) = self.expr(
            body,
            Some(Want {
                ty: result,
                view: false,
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
            Some(w) if t != NEVER => {
                self.coerce(t, w, n);
                (w.ty, v)
            }
            _ => (t, v),
        }
    }

    fn expr_inner(&mut self, n: NodeId, want: Option<Want>) -> (TyId, Option<ValueId>) {
        let c = self.kids(n);
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
            NodeKind::Tuple => self.gap_ty(n, "tuples are not built"),
            NodeKind::Index | NodeKind::Projection => self.place_value(n),
            NodeKind::Try => self.gap_ty(n, "`?` is not built"),
            NodeKind::Match | NodeKind::ConditionLet => {
                self.gap_ty(n, "match and patterns are not built")
            }
            NodeKind::For | NodeKind::ForTake => self.gap_ty(n, "for loops are not built"),
            NodeKind::Region | NodeKind::Bound | NodeKind::Placement => {
                self.gap_ty(n, "regions and bounded scopes are not built")
            }
            NodeKind::Closure | NodeKind::ScopedClosure => self.gap_ty(n, "closures are not built"),
            _ => self.gap_ty(n, "expression form is not built"),
        }
    }
    /// A place in a value position: moved when its type moves, else copied (OWN-6).
    fn place_value(&mut self, n: NodeId) -> (TyId, Option<ValueId>) {
        match self.place(n) {
            Ok(Some(at)) => {
                self.use_place(at, Use::Value, n);
                (at.ty, self.emit_place(at, n))
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
        TyKind::Fn(ps, r, _) => {
            mentions_error(t, *r) || ps.iter().any(|&(_, x)| mentions_error(t, x))
        }
        TyKind::Never | TyKind::Unit | TyKind::Scalar(_) | TyKind::Param(_) | TyKind::Hole => false,
    }
}
