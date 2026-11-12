//! Bodies: one forward visit per function that types its body against the published
//! signatures and summaries, checks ownership and emits CoreIR as it goes (LANG-8:
//! nothing else is read).
//!
//! Emitted so far: scalars and unit, blocks, `let` and `let mut`, assignment and compound
//! assignment, the operators of SEM-1, SEM-2 and SEM-7, `if`, `loop`, `while`, labels,
//! `break`, `continue`, `return`, direct calls of non-generic functions and the core
//! integer operations. Checked but not yet emitted: values of own types, their places
//! (fields, tuple components, `List` elements), `take`, `view`, `mut` arguments, and the
//! ownership rules OWN-5 to OWN-17 over them. Anything else is a [`Gap`]: the body is
//! still checked, but no code is emitted for it. An operation that can fail names no
//! failure successor, so the failure leaves the function with its cause (FORM v1a).
//!
//! Ownership state is per body and indexed by binder id: the slot table, an undo log of
//! availability changes (a branch's arms run from one state and join by OWN-16 without
//! copying the table), and the freeze and reservation records (crate::own).

use crate::decls::{
    DeclBody, FnDecl, KEY, Kind, ORDERED, Repr, Role, copyable, has_bounds, is_core_prelude,
};
use crate::own::{Act, Avail, Extent, Freeze, Hold, Origin, PStep, Place, breaks, join, overlap};
use crate::types::*;
use crate::{Def, Gap, ItemRef, Program, refusal};
use hirsel_core_ids::DefPath;
use hirsel_core_ir::{
    AddressBase, AddressId, AddressInfo, Anchor, Arg, ArithMode, ArithOp, BinaryOp, Block, BlockId,
    Body, CallId, CallSite, Callee, CompareOp, ConvertMode, Edge, EdgeId, FuncId, Function, Inst,
    InstanceId, InstanceKey, ItemId, Module, Param, PlaceTy, PoolRange, ReadMode, SigId, Signature,
    Terminator, Ty, UnaryOp, ValueId,
};
use hirsel_core_layout::Scalar;
use hirsel_core_resolve::{Refusal, Target};
use hirsel_core_syntax::{Item, Keyword, NodeId, NodeKind, TokenKind, byte_value, character_value};
use std::collections::HashMap;

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
        let sig = p.ir_signature(f)?;
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

/// The CoreIR type of a value of `ty`: a scalar, nothing for unit and Never, or the gap
/// that keeps it from having one yet.
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
    /// The CoreIR signature of function `f`, or the gap that keeps it from having one.
    pub fn ir_signature(&self, f: usize) -> Result<Signature, &'static str> {
        let fd = &self.fns[f];
        if !fd.generics.is_empty() {
            return Err("generic definitions are not built");
        }
        let mut params = vec![];
        for p in &fd.params {
            if p.mode == Mode::Mut {
                return Err("mut parameters are not built");
            }
            params.extend(value_ty(&self.types, p.ty)?.map(Param::Value));
        }
        Ok(Signature {
            params,
            results: value_ty(&self.types, fd.result)?.into_iter().collect(),
            dest: None,
            never: matches!(self.types.kind(fd.result), TyKind::Never),
        })
    }

    /// Check function `f`'s body and emit it into `e` (LANG-8: one visit, reading only
    /// this body, signatures and summaries).
    pub fn check_lower(&self, f: usize, e: &mut Emitter) -> CoreCheck {
        let fd = &self.fns[f];
        let Some(body) = fd.body else {
            return CoreCheck::default();
        };
        let mut cx = Cx::new(self, f, e);
        cx.run(f, body);
        cx.finish()
    }
}

/// What an expression is checked against, and the rule a mismatch breaks: LANG-8 where
/// the signature's result is expected, SEM-1 for an operator's second operand, LANG-11
/// elsewhere.
#[derive(Clone, Copy)]
struct Want {
    ty: TyId,
    rule: &'static str,
}
fn want(ty: TyId) -> Option<Want> {
    Some(Want {
        ty,
        rule: "LANG-11",
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
    item: &'a Item,
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
    scope_slots: Vec<u32>,
    call_seq: u32,
    binder_at: HashMap<(u32, u32), u32>,
    frames: Vec<Frame>,
    joins: Vec<Join>,
    /// Per node: 0 unknown, 1 synthesises a type, 2 needs an expected type.
    synth: Vec<u8>,
    waited: bool,
    /// The program's entry, which is always `waits` (LANG-8, PROG-3).
    entry: bool,
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
            scope_slots: vec![],
            call_seq: 0,
            binder_at,
            frames: vec![],
            joins: vec![],
            synth: vec![0; item.tree.nodes.len()],
            waited: false,
            entry: p.entry.as_ref() == Some(&fd.path),
            out: CoreCheck::default(),
        }
    }

    // ---- syntax
    fn kind(&self, n: NodeId) -> NodeKind {
        self.item.tree.node(n).kind
    }
    fn kids(&self, n: NodeId) -> &'a [NodeId] {
        let item: &'a Item = self.item;
        item.tree.children(n)
    }
    fn token_text(&self, n: NodeId) -> &'a str {
        let item: &'a Item = self.item;
        match item.tree.node(n).token {
            Some(t) => item.tree.token_text(&item.text, t),
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
        let site = self.p.site(self.at, n);
        self.out.refusals.push(refusal(rule, site, message));
        self.halt();
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
        if t != w.ty && t != NEVER && t != ERROR && w.ty != ERROR {
            let (a, b) = (self.name(w.ty), self.name(t));
            self.refuse(w.rule, n, format!("expected `{a}`, found `{b}`"));
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
                let tys: Vec<Ty> = value_ty(&self.t, ty).ok().flatten().into_iter().collect();
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

    // ---- ownership state (OWN-5, OWN-9 to OWN-18)
    fn var(&self, s: u32) -> Var {
        self.vars[s as usize].expect("declared slot")
    }
    fn slot_name(&self, s: u32) -> &'c str {
        let p: &'c Program<'a> = self.p;
        &p.items[self.at.module as usize][self.at.item as usize]
            .resolution
            .binders[s as usize]
            .name
    }
    fn declare(&mut self, b: u32, ty: TyId, binding: Binding, origin: Origin, n: NodeId) {
        self.vars[b as usize] = Some(Var {
            ty,
            binding,
            origin,
            avail: Avail::Uninit,
            at: n,
            born: self.born,
            flagged: false,
            holds: 0,
        });
        self.born += 1;
        self.set(b, Avail::Live, n);
    }
    fn set(&mut self, s: u32, a: Avail, n: NodeId) {
        let v = self.vars[s as usize].as_mut().expect("declared slot");
        if v.avail != a {
            self.log.push((s, v.avail, v.at));
            v.avail = a;
            v.at = n;
        }
    }
    fn undo(&mut self, mark: usize) {
        while self.log.len() > mark {
            let (s, a, at) = self.log.pop().expect("logged");
            let v = self.vars[s as usize].as_mut().expect("declared slot");
            v.avail = a;
            v.at = at;
        }
    }
    /// The current state of each slot changed since `mark`: one incoming edge of a join.
    fn edge_state(&mut self, mark: usize) -> (u32, u32) {
        let start = self.diffs.len() as u32;
        for i in mark..self.log.len() {
            let s = self.log[i].0;
            let a = self.var(s).avail;
            self.diffs.push((s, a));
        }
        (start, self.diffs.len() as u32 - start)
    }
    /// Join `edges` (OWN-16) into the state at `mark`; a slot no edge changed keeps its
    /// state there. With no edge, nothing reaches the join.
    fn merge(&mut self, mark: usize, edges: &[(u32, u32)], n: NodeId) {
        self.undo(mark);
        self.dead = edges.is_empty();
        self.merges += 1;
        let m = self.merges;
        let mut touched = std::mem::take(&mut self.touched);
        touched.clear();
        for &(start, len) in edges {
            self.edges_seen += 1;
            let e = self.edges_seen;
            for k in start..start + len {
                let (s, a) = self.diffs[k as usize];
                let x = &mut self.acc[s as usize];
                if x.edge == e {
                    continue;
                }
                x.edge = e;
                if x.merge != m {
                    *x = Acc {
                        avail: a,
                        count: 1,
                        merge: m,
                        edge: e,
                    };
                    touched.push(s);
                } else {
                    x.avail = join(x.avail, a);
                    x.count += 1;
                }
            }
        }
        for &s in &touched {
            let x = self.acc[s as usize];
            let base = self.var(s).avail;
            let a = if (x.count as usize) < edges.len() {
                join(x.avail, base)
            } else {
                x.avail
            };
            if a == Avail::Maybe {
                self.vars[s as usize].as_mut().unwrap().flagged = true;
            }
            self.set(s, a, n);
        }
        self.touched = touched;
    }
    /// OWN-17: at a back edge every slot declared outside loop `fi` is in its entry
    /// state; the site is where it last changed.
    fn back_edge(&mut self, fi: usize) {
        if self.dead {
            return;
        }
        let (mark, born) = (self.frames[fi].mark, self.frames[fi].born);
        self.merges += 1;
        let m = self.merges;
        let end = self.log.len();
        for i in mark..end {
            let (s, entry, _) = self.log[i];
            if self.acc[s as usize].merge == m {
                continue;
            }
            self.acc[s as usize].merge = m;
            let v = self.var(s);
            if v.born < born && v.avail != entry {
                let name = self.slot_name(s);
                self.refuse(
                    "OWN-17",
                    v.at,
                    format!("`{name}` is not in its loop-entry state when the loop repeats"),
                );
            }
        }
    }
    /// Leave for frame `fi`'s exit, carrying this state to its join (OWN-16).
    fn exit_edge(&mut self, fi: usize) {
        if !self.dead {
            let (start, len) = self.edge_state(self.frames[fi].mark);
            self.exits.push((fi, start, len));
        }
    }
    /// Join frame `fi`'s exit edges; it is the innermost open frame.
    fn merge_exits(&mut self, fi: usize, n: NodeId) {
        let from = self.frames[fi].exits;
        let mut edges = std::mem::take(&mut self.edge_buf);
        edges.clear();
        edges.extend(
            self.exits[from..]
                .iter()
                .filter(|e| e.0 == fi)
                .map(|e| (e.1, e.2)),
        );
        self.exits.truncate(from);
        let mark = self.frames[fi].mark;
        self.merge(mark, &edges, n);
        self.edge_buf = edges;
    }

    /// OWN-5, OWN-16: a read, view, loan or move needs a live slot.
    fn need_live(&mut self, s: u32, n: NodeId) {
        let v = self.var(s);
        let name = self.slot_name(s);
        match v.avail {
            Avail::Live => return,
            Avail::Maybe => {
                self.refuse("OWN-16", n, format!("`{name}` may have been moved"));
            }
            Avail::Moved => self.refuse("OWN-5", n, format!("`{name}` was moved")),
            Avail::Uninit => self.refuse("OWN-5", n, format!("`{name}` is not initialised")),
        }
        // Refused once; later uses are not refused again for it.
        self.set(s, Avail::Live, n);
    }
    /// The freeze or reservation `p` crosses, if any (OWN-9 to OWN-12).
    fn check_holds(&mut self, p: Place, act: Act, n: NodeId) -> bool {
        if self.var(p.slot).holds == 0 {
            return true;
        }
        for i in (0..self.freezes.len()).rev() {
            let f = self.freezes[i];
            if overlap(&self.steps, f.place, p)
                && let Some(rule) = breaks(f.hold, act)
            {
                let name = self.slot_name(p.slot);
                let why = match f.hold {
                    Hold::View(_) => "is viewed",
                    Hold::CallView(..) => "is viewed by an argument of this call",
                    Hold::CallMut(_) => "is lent `mut` to this call",
                    Hold::Assign => "is reserved by an assignment",
                };
                self.refuse(rule, n, format!("a place of `{name}` {why}"));
                return false;
            }
        }
        true
    }
    fn unfreeze_at(&mut self, i: usize) {
        let x = self.freezes.remove(i);
        self.vars[x.place.slot as usize].as_mut().unwrap().holds -= 1;
    }
    fn freeze(&mut self, place: Place, hold: Hold) {
        self.vars[place.slot as usize].as_mut().unwrap().holds += 1;
        self.freezes.push(Freeze { place, hold });
    }
    /// Rewrite or end the records from `from` on: `f` gives a record's new hold, or
    /// None when it ends.
    fn unfreeze(&mut self, from: usize, f: impl Fn(Hold) -> Option<Hold>) {
        let mut w = from;
        for r in from..self.freezes.len() {
            let mut x = self.freezes[r];
            match f(x.hold) {
                Some(h) => {
                    x.hold = h;
                    self.freezes[w] = x;
                    w += 1;
                }
                None => self.vars[x.place.slot as usize].as_mut().unwrap().holds -= 1,
            }
        }
        self.freezes.truncate(w);
    }

    /// The place `n` denotes: a local and its field and index steps (OWN-9). None when
    /// `n` is not a place; Err when it was refused or is not built.
    fn place(&mut self, n: NodeId) -> Result<Option<At>, ()> {
        match self.kind(n) {
            NodeKind::Group => self.place(self.kids(n)[0]),
            NodeKind::Path => {
                let p: &'c Program<'a> = self.p;
                let Some(u) = p.resolved(self.at, n) else {
                    self.halt();
                    return Err(());
                };
                let Target::Binder(b) = u.target else {
                    return Ok(None);
                };
                let Some(var) = self.vars[b as usize] else {
                    self.gap(n, "this kind of name is not built");
                    return Err(());
                };
                self.out.counts.places += 1;
                let start = self.steps.len() as u32;
                let mut at = At {
                    place: Place {
                        slot: b,
                        start,
                        len: 0,
                    },
                    ty: var.ty,
                    shared: false,
                };
                for name in &u.projections {
                    self.field(&mut at, name, n)?;
                }
                Ok(Some(at))
            }
            NodeKind::Projection => {
                let Some(mut at) = self.place(self.kids(n)[0])? else {
                    return Ok(None);
                };
                self.field(&mut at, self.token_text(n), n)?;
                Ok(Some(at))
            }
            NodeKind::Index => {
                let c = self.kids(n);
                let Some(mut at) = self.place(c[0])? else {
                    return Ok(None);
                };
                if matches!(self.t.kind(at.ty), TyKind::View(_)) {
                    self.gap(n, "indexing through a view is not built");
                    return Err(());
                }
                let elem = match self.t.kind(at.ty).clone() {
                    TyKind::Adt(d, args) if self.p.decls[d.index()].role == Some(Role::List) => {
                        args[0]
                    }
                    _ => {
                        self.gap(n, "indexing this type is not built");
                        return Err(());
                    }
                };
                // Index operands are evaluated before places are compared (OWN-9).
                let (_, _) = self.expr(c[1], want_rule(scalar(Scalar::U64), "LANG-11"));
                let (s, l) = (at.place.start as usize, at.place.len as usize);
                if s + l != self.steps.len() {
                    self.steps.extend_from_within(s..s + l);
                    at.place.start = (self.steps.len() - l) as u32;
                }
                self.steps.push(PStep::Any);
                at.place.len += 1;
                at.ty = elem;
                Ok(Some(at))
            }
            _ => Ok(None),
        }
    }
    /// Step `at` to its field `name`; through a view, a field that is not plain reads
    /// as a view (OWN-8).
    fn field(&mut self, at: &mut At, name: &str, n: NodeId) -> Result<(), ()> {
        let (inner, view) = match self.t.kind(at.ty) {
            TyKind::View(x) => (*x, true),
            _ => (at.ty, false),
        };
        let mut stable = false;
        let found = match self.t.kind(inner).clone() {
            TyKind::Adt(d, args) => {
                let decl = &self.p.decls[d.index()];
                stable = decl.repr == Repr::Stable;
                match &decl.body {
                    DeclBody::Record(fs) => fs
                        .iter()
                        .position(|f| f.name == name)
                        .map(|i| (i, subst(&mut self.t, fs[i].ty, &args))),
                    _ => None,
                }
            }
            TyKind::Tuple(xs) => name
                .parse::<usize>()
                .ok()
                .and_then(|i| xs.get(i).map(|&x| (i, x))),
            _ => None,
        };
        let Some((i, ft)) = found else {
            self.gap(n, "this field access is not built");
            return Err(());
        };
        self.steps.push(PStep::Field(i as u32));
        at.place.len += 1;
        at.shared |= view || stable;
        at.ty = if view && !self.copies(ft) && !matches!(self.t.kind(ft), TyKind::View(_)) {
            self.t.mk(TyKind::View(ft))
        } else {
            ft
        };
        Ok(())
    }
    fn copies(&self, ty: TyId) -> bool {
        copyable(&self.p.decls, &self.t, ty, &self.params)
    }
    /// Use place `at` at `n` (OWN-5 to OWN-14); its type.
    fn use_place(&mut self, at: At, how: Use, n: NodeId) {
        let s = at.place.slot;
        let v = self.var(s);
        let name = self.slot_name(s);
        match how {
            Use::Value | Use::Take => {
                let copy = match how {
                    Use::Value => self.copies(at.ty),
                    _ => at.place.len > 0 && self.copies(at.ty),
                };
                if copy {
                    self.need_live(s, n);
                    self.check_holds(at.place, Act::Read, n);
                    return;
                }
                if at.place.len > 0 {
                    self.refuse(
                        "OWN-14",
                        n,
                        "only a whole slot is moved; use `replace` or a whole destructure",
                    );
                    return;
                }
                match v.origin {
                    Origin::ViewParam => {
                        // The spec does not say which rule refuses this (Q23).
                        self.refuse("OWN-7", n, format!("`{name}` is a view parameter"));
                        return;
                    }
                    Origin::MutParam => {
                        self.refuse("OWN-11", n, format!("`{name}` is a `mut` parameter"));
                        return;
                    }
                    _ => {}
                }
                self.need_live(s, n);
                if self.check_holds(at.place, Act::Write, n) {
                    self.set(s, Avail::Moved, n);
                }
            }
            Use::View(hold) => {
                self.need_live(s, n);
                if self.check_holds(at.place, Act::Read, n) {
                    self.freeze(at.place, hold);
                }
            }
            Use::Mut(hold) => {
                if !v.origin.writable() {
                    self.refuse("OWN-5", n, format!("`{name}` is not writable"));
                    return;
                }
                self.need_live(s, n);
                if self.check_holds(at.place, Act::Write, n) {
                    self.freeze(at.place, hold);
                }
            }
        }
    }
    /// The value of a used place, for the places emitted so far: a whole local.
    fn emit_place(&mut self, at: At, n: NodeId) -> Option<ValueId> {
        if at.place.len > 0 {
            self.gap(n, "field and index places are not built");
            return None;
        }
        let var = self.var(at.place.slot);
        match var.binding {
            Binding::Value(v) => v,
            Binding::Slot(a) => {
                let (Some(address), Ok(Some(t))) = (a, value_ty(&self.t, var.ty)) else {
                    return None;
                };
                if !self.live() {
                    return None;
                }
                let result = self.value(t);
                self.inst(
                    Inst::Read {
                        result,
                        address,
                        mode: ReadMode::Bits,
                    },
                    n,
                );
                Some(result)
            }
        }
    }
    /// Close a block: its let-bound views end, then its live owners are dropped, which
    /// a remaining freeze forbids (OWN-10).
    fn close_scope(&mut self, start: usize, freezes: usize, n: NodeId) {
        let d = self.depth;
        self.unfreeze(freezes, |h| {
            (h != Hold::View(Extent::Block(d))).then_some(h)
        });
        if !self.dead {
            for i in start..self.scope_slots.len() {
                let s = self.scope_slots[i];
                let v = self.var(s);
                if matches!(v.avail, Avail::Live | Avail::Maybe) && !self.copies(v.ty) {
                    self.check_holds(Place::whole(s), Act::Write, n);
                }
            }
        }
        self.scope_slots.truncate(start);
    }

    // ---- the definition
    fn run(&mut self, f: usize, body: NodeId) {
        match self.e.row(self.p, f) {
            Ok((func, sig)) => {
                self.func = Some(func);
                let tys: Vec<Ty> = self.e.module.signatures[sig.index()]
                    .params
                    .iter()
                    .map(|p| match p {
                        Param::Value(t) => *t,
                        Param::Address(_) => Ty::Ptr,
                    })
                    .collect();
                let entry = self.new_block(&tys);
                self.start(entry);
            }
            Err(what) => {
                let site = self.fd.site.clone();
                self.out.gaps.push(Gap::new(site, what));
                self.halt();
            }
        }
        let entry_params = if self.func.is_some() {
            self.block_params(BlockId(0))
        } else {
            vec![]
        };
        let mut k = 0;
        let p: &'c Program<'a> = self.p;
        let binders = &p.items[self.at.module as usize][self.at.item as usize]
            .resolution
            .binders;
        for pd in &self.fd.params {
            let v = match value_ty(&self.t, pd.ty) {
                Ok(Some(_)) if k < entry_params.len() => {
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
                rule: "LANG-8",
            }),
        );
        if self.live() {
            let r = PoolRange::push(&mut self.b.pools.values, v);
            self.end(Terminator::Return(r), Some(body));
        }
        if self.fd.waits && !self.waited && !self.entry {
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
            NodeKind::Path => self.path(n),
            NodeKind::Block => self.block(n, want),
            NodeKind::If => self.if_(n, want),
            NodeKind::Loop => self.loop_(n, want, None),
            NodeKind::While => self.while_(n, None),
            NodeKind::Labelled => self.labelled(n, want),
            NodeKind::Return => self.return_(n),
            NodeKind::Break => self.break_(n),
            NodeKind::Continue => self.continue_(n),
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

    /// Whether `n` has a type without an expected one (LANG-11): everything but an
    /// unsuffixed number and arithmetic made only of such. Memoised per node, so
    /// operator chains stay linear.
    fn synthesises(&mut self, n: NodeId) -> bool {
        self.out.counts.nodes += 1;
        match self.synth[n.0 as usize] {
            1 => return true,
            2 => return false,
            _ => {}
        }
        let c = self.kids(n);
        let r = match self.kind(n) {
            NodeKind::Literal => !self.unsuffixed_number(n),
            NodeKind::Group => self.synthesises(c[0]),
            NodeKind::Unary => match self.token_text(n) {
                "-" | "~" | "copy" => self.synthesises(c[0]),
                _ => true,
            },
            NodeKind::Binary => match self.token_text(n) {
                "+" | "-" | "*" | "/" | "%" | "&" | "|" | "^" => {
                    self.synthesises(c[0]) || self.synthesises(c[1])
                }
                "<<" | ">>" => self.synthesises(c[0]),
                _ => true,
            },
            _ => true,
        };
        self.synth[n.0 as usize] = if r { 1 } else { 2 };
        r
    }
    fn unsuffixed_number(&self, n: NodeId) -> bool {
        match self.token_kind(n) {
            Some(TokenKind::Integer) => int_suffix(self.token_text(n)).1.is_none(),
            Some(TokenKind::Float) => float_suffix(self.token_text(n)).1.is_none(),
            _ => false,
        }
    }
    /// A number literal, possibly parenthesised: it has no effect, so it may be
    /// emitted after an operand that follows it.
    fn bare_number(&self, n: NodeId) -> Option<NodeId> {
        match self.kind(n) {
            NodeKind::Literal
                if matches!(
                    self.token_kind(n),
                    Some(TokenKind::Integer | TokenKind::Float)
                ) =>
            {
                Some(n)
            }
            NodeKind::Group => self.bare_number(self.kids(n)[0]),
            _ => None,
        }
    }

    fn literal(&mut self, n: NodeId, want: Option<Want>, negate: bool) -> (TyId, Option<ValueId>) {
        let text = self.token_text(n);
        let scalar_want = want.and_then(|w| match self.t.kind(w.ty) {
            TyKind::Scalar(s) => Some(*s),
            _ => None,
        });
        let (ty, bits) = match self.token_kind(n) {
            Some(TokenKind::Keyword(Keyword::True)) => (Scalar::Bool, 1),
            Some(TokenKind::Keyword(Keyword::False)) => (Scalar::Bool, 0),
            Some(TokenKind::Character) => match character_value(text) {
                Some(c) => (Scalar::Char, u64::from(u32::from(c))),
                None => return self.gap_ty(n, "character literal form is not built"),
            },
            Some(TokenKind::Byte) => match byte_value(text) {
                Some(b) => (Scalar::U8, u64::from(b)),
                None => return self.gap_ty(n, "byte literal form is not built"),
            },
            Some(TokenKind::Integer) => {
                let (digits, suffix) = int_suffix(text);
                let Some(s) = suffix.or(scalar_want.filter(|s| numeric(*s))) else {
                    return self.untyped_number(n, want);
                };
                let Some(m) = int_magnitude(digits) else {
                    return self.gap_ty(n, "integer literal out of range for its type (Q12)");
                };
                if matches!(s, Scalar::F32 | Scalar::F64) {
                    let x = if negate { -(m as f64) } else { m as f64 };
                    (s, float_bits(s, x))
                } else {
                    let (signed, w) = Ty::Scalar(s).int().unwrap_or((false, 64));
                    let limit: u128 = if signed {
                        (1u128 << (w - 1)) - u128::from(!negate)
                    } else {
                        (1u128 << w) - 1
                    };
                    if m > limit {
                        return self.gap_ty(n, "integer literal out of range for its type (Q12)");
                    }
                    let v = if negate {
                        (m as i128).wrapping_neg() as u64
                    } else {
                        m as u64
                    };
                    (s, v & (u64::MAX >> (64 - w)))
                }
            }
            Some(TokenKind::Float) => {
                let (digits, suffix) = float_suffix(text);
                let Some(s) = suffix.or(scalar_want.filter(|s| numeric(*s))) else {
                    return self.untyped_number(n, want);
                };
                if !matches!(s, Scalar::F32 | Scalar::F64) {
                    let a = self.name(scalar(s));
                    self.refuse(
                        want.map_or("LANG-11", |w| w.rule),
                        n,
                        format!("expected `{a}`, found a fractional number"),
                    );
                    return (ERROR, None);
                }
                let digits: String = digits.chars().filter(|&c| c != '_').collect();
                let x = match s {
                    Scalar::F32 => digits.parse::<f32>().map(f64::from),
                    _ => digits.parse::<f64>(),
                };
                let Ok(x) = x else {
                    return self.gap_ty(n, "float literal form is not built");
                };
                (s, float_bits(s, if negate { -x } else { x }))
            }
            Some(TokenKind::String | TokenKind::Bytes) => {
                return self.gap_ty(n, "string and bytes literals are not built");
            }
            _ => return self.gap_ty(n, "literal form is not built"),
        };
        let v = self.konst(Ty::Scalar(ty), bits, n);
        (scalar(ty), v)
    }
    fn untyped_number(&mut self, n: NodeId, want: Option<Want>) -> (TyId, Option<ValueId>) {
        match want {
            Some(w) if w.ty == ERROR => {}
            Some(w) => {
                let a = self.name(w.ty);
                self.refuse(w.rule, n, format!("expected `{a}`, found a number"));
            }
            None => self.refuse("LANG-11", n, "an unsuffixed number needs an expected type"),
        }
        (ERROR, None)
    }

    fn path(&mut self, n: NodeId) -> (TyId, Option<ValueId>) {
        self.out.counts.uses += 1;
        let Some(u) = self.p.resolved(self.at, n) else {
            self.halt();
            return (ERROR, None);
        };
        match &u.target {
            Target::Binder(_) => self.place_value(n),
            Target::Missing => {
                self.halt();
                (ERROR, None)
            }
            Target::ContextualConstructor { .. } => self.gap_ty(n, "variants are not built"),
            _ => self.gap_ty(n, "function, constant and module values are not built"),
        }
    }

    // ---- blocks and control flow
    fn block(&mut self, n: NodeId, want: Option<Want>) -> (TyId, Option<ValueId>) {
        self.depth += 1;
        let (scope, freezes) = (self.scope_slots.len(), self.freezes.len());
        let r = self.statements(n, want);
        self.close_scope(scope, freezes, n);
        self.depth -= 1;
        r
    }
    fn statements(&mut self, n: NodeId, want: Option<Want>) -> (TyId, Option<ValueId>) {
        let stmts = self.kids(n);
        let mut diverged = false;
        for (i, &s) in stmts.iter().enumerate() {
            self.out.counts.nodes += 1;
            let last = i + 1 == stmts.len();
            let freezes = self.freezes.len();
            match self.kind(s) {
                // The tail's temporaries last to the end of the enclosing statement.
                NodeKind::ExpressionStatement if last => {
                    return self.expr(self.kids(s)[0], want);
                }
                NodeKind::ExpressionStatement | NodeKind::Discard => {
                    let (t, _) = self.expr(self.kids(s)[0], None);
                    diverged |= t == NEVER;
                }
                NodeKind::Let | NodeKind::LetMut => diverged |= self.let_(s),
                NodeKind::Assign => diverged |= self.assign(s),
                NodeKind::Error => self.halt(),
                _ => self.gap(s, "statement form is not built"),
            }
            // A temporary view ends with its statement (OWN-8).
            self.unfreeze(freezes, |h| (h != Hold::View(Extent::Temp)).then_some(h));
        }
        let t = if diverged { NEVER } else { UNIT };
        match want {
            Some(w) if t != NEVER => {
                self.coerce(t, w, n);
                (w.ty, None)
            }
            _ => (t, None),
        }
    }

    fn let_(&mut self, s: NodeId) -> bool {
        let c = self.kids(s);
        let pat = c[0];
        let mut rest = &c[1..];
        let declared = match rest.first() {
            Some(&t) if is_type_node(self.kind(t)) => {
                rest = &rest[1..];
                Some(self.ty(t))
            }
            _ => None,
        };
        let Some((&value, rest)) = rest.split_first() else {
            return self.let_uninit(s, pat, declared);
        };
        if !rest.is_empty() {
            self.gap(rest[0], "let-else is not built");
        }
        let freezes = self.freezes.len();
        let (t, v) = self.expr(value, declared.and_then(want));
        let ty = declared.unwrap_or(t);
        // A let-bound view lasts to the end of its block (OWN-8).
        if crate::decls::kind(&self.p.decls, &self.t, ty, &self.params) == Kind::Scoped {
            let d = self.depth;
            self.unfreeze(freezes, |h| {
                Some(if h == Hold::View(Extent::Temp) {
                    Hold::View(Extent::Block(d))
                } else {
                    h
                })
            });
        }
        self.out.counts.nodes += 1;
        match self.kind(pat) {
            NodeKind::Wildcard => {}
            NodeKind::BindPattern => {
                if let Err(what) = value_ty(&self.t, ty) {
                    self.gap(pat, what);
                }
                let Some(b) = self.binder(pat) else {
                    self.halt();
                    return t == NEVER;
                };
                let (binding, origin) = if self.kind(s) == NodeKind::LetMut {
                    (Binding::Slot(self.slot(ty, v, pat)), Origin::LetMut)
                } else {
                    (Binding::Value(v), Origin::Let)
                };
                self.declare(b, ty, binding, origin, pat);
                self.scope_slots.push(b);
            }
            _ => self.gap(pat, "patterns are not built"),
        }
        t == NEVER
    }
    /// `let x: T` with no value: an uninit slot (OWN-5), assigned before it is read.
    fn let_uninit(&mut self, s: NodeId, pat: NodeId, declared: Option<TyId>) -> bool {
        let Some(ty) = declared else {
            self.gap(s, "a `let` with neither a type nor a value (Q26)");
            return false;
        };
        if self.kind(pat) != NodeKind::BindPattern {
            self.gap(pat, "patterns are not built");
            return false;
        }
        if let Err(what) = value_ty(&self.t, ty) {
            self.gap(pat, what);
        }
        let Some(b) = self.binder(pat) else {
            self.halt();
            return false;
        };
        let origin = if self.kind(s) == NodeKind::LetMut {
            Origin::LetMut
        } else {
            Origin::Let
        };
        let binding = Binding::Slot(self.slot(ty, None, pat));
        self.declare(b, ty, binding, origin, pat);
        self.set(b, Avail::Uninit, pat);
        self.scope_slots.push(b);
        false
    }
    /// A `let mut` slot holding `v`; its address is evaluated once, where it begins.
    fn slot(&mut self, ty: TyId, v: Option<ValueId>, n: NodeId) -> Option<AddressId> {
        let t = value_ty(&self.t, ty).ok().flatten()?;
        if !self.live() {
            return None;
        }
        self.out.counts.places += 1;
        self.b.slots.push(PlaceTy::Value(t));
        let slot = hirsel_core_ir::SlotId(self.b.slots.len() as u32 - 1);
        self.b.addresses.push(AddressInfo {
            base: AddressBase::Slot(slot),
            steps: PoolRange::EMPTY,
            ty: PlaceTy::Value(t),
        });
        let address = AddressId(self.b.addresses.len() as u32 - 1);
        self.inst(Inst::StorageLive { slot }, n);
        self.inst(
            Inst::Address {
                address,
                fail: None,
            },
            n,
        );
        if let Some(value) = v {
            self.inst(Inst::Store { address, value }, n);
        }
        Some(address)
    }

    /// `q = e` and `q op= e` (OWN-12): `q` is writable (OWN-5), not frozen, and reserved
    /// while `e` is evaluated.
    fn assign(&mut self, s: NodeId) -> bool {
        let c = self.kids(s);
        let (place, value) = (c[0], c[1]);
        let op = self.token_text(s);
        self.out.counts.uses += 1;
        let at = match self.place(place) {
            Ok(Some(at)) => at,
            Ok(None) => {
                self.gap(place, "assignment to this place is not built");
                return false;
            }
            Err(()) => return false,
        };
        let q = at.place.slot;
        let var = self.var(q);
        // OWN-12: a writable place, or a `let` declared without a value while uninit.
        let fresh = var.origin == Origin::Let && var.avail == Avail::Uninit && at.place.len == 0;
        if !(var.origin.writable() || fresh) || at.shared {
            let name = self.slot_name(q);
            self.refuse(
                "OWN-12",
                place,
                format!("this place of `{name}` is not writable"),
            );
            return false;
        }
        if at.place.len > 0 && var.avail != Avail::Live {
            self.gap(place, "assignment into a slot that is not live (Q24)");
        }
        self.check_holds(at.place, Act::Write, place);
        let compound = op != "=";
        if compound {
            self.need_live(q, place);
        }
        let reserved = self.freezes.len();
        self.freeze(at.place, Hold::Assign);
        let (address, emitted) = match var.binding {
            Binding::Slot(a) if at.place.len == 0 => (a, true),
            _ => (None, false),
        };
        if !emitted && value_ty(&self.t, at.ty).is_ok_and(|t| t.is_some()) {
            self.gap(place, "assignment to this place is not built");
        }
        let diverged = if !compound {
            let (t, v) = self.expr(value, want(at.ty));
            if let (Some(address), Some(value)) = (address, v) {
                self.inst(Inst::Store { address, value }, s);
            }
            t == NEVER
        } else {
            // Compound: resolve once, read the old value, evaluate the right side,
            // compute, install; plain-only, and every type built here is plain.
            self.out.counts.places += 1;
            let bop = op.trim_end_matches('=');
            let old = match (address, value_ty(&self.t, at.ty)) {
                (Some(address), Ok(Some(t))) if self.live() => {
                    let result = self.value(t);
                    self.inst(
                        Inst::Read {
                            result,
                            address,
                            mode: ReadMode::Bits,
                        },
                        s,
                    );
                    Some(result)
                }
                _ => None,
            };
            let (t, rv) = self.expr(
                value,
                Some(Want {
                    ty: at.ty,
                    rule: "SEM-1",
                }),
            );
            if let Some(new) = self.arith(bop, at.ty, old, rv, s)
                && let Some(address) = address
            {
                self.inst(
                    Inst::Store {
                        address,
                        value: new,
                    },
                    s,
                );
            }
            t == NEVER
        };
        // The install ends the reservation; the record is the one pushed above, and
        // `e`'s own temporaries after it stay to the statement's end.
        self.unfreeze_at(reserved);
        if at.place.len == 0 {
            self.set(q, Avail::Live, s);
        }
        diverged
    }

    fn if_(&mut self, n: NodeId, want: Option<Want>) -> (TyId, Option<ValueId>) {
        let c = self.kids(n);
        if self.kind(c[0]) == NodeKind::ConditionLet {
            return self.gap_ty(c[0], "match and patterns are not built");
        }
        let (_, cv) = self.expr(c[0], want_rule(BOOL, "LANG-11"));
        let join = self.new_join();
        let mut no_block = None;
        if self.live()
            && let Some(cv) = cv
        {
            let yes = self.new_block(&[]);
            let no = self.new_block(&[]);
            let (ye, ne) = (self.edge(yes, &[]), self.edge(no, &[]));
            self.end(
                Terminator::Branch {
                    condition: cv,
                    yes: ye,
                    no: ne,
                },
                Some(c[0]),
            );
            self.start(yes);
            no_block = Some(no);
        }
        // The arms join by OWN-16; an arm that leaves contributes nothing.
        let (was_dead, mark) = (self.dead, self.log.len());
        let Some(&other) = c.get(2) else {
            // Without `else` the type is unit (LANG-11).
            let (_, _) = self.expr(c[1], want_rule(UNIT, "LANG-11"));
            let yes = self.arm_edge(mark);
            self.goto(join, UNIT, None, c[1]);
            if let Some(no) = no_block {
                self.start(no);
            }
            self.goto(join, UNIT, None, n);
            self.land(join);
            let no = Some((self.diffs.len() as u32, 0));
            self.merge_arms(mark, [yes, no], was_dead, n);
            return (UNIT, None);
        };
        let (yt, yv) = self.expr(c[1], want);
        let yes = self.arm_edge(mark);
        self.goto(join, yt, yv, c[1]);
        if let Some(no) = no_block {
            self.start(no);
        }
        // An arm of type Never constrains nothing (LANG-11).
        let other_want = want.or(if yt == NEVER {
            None
        } else {
            want_rule(yt, "LANG-11")
        });
        let (ot, ov) = self.expr(other, other_want);
        let no = self.arm_edge(mark);
        let ty = if yt == NEVER { ot } else { yt };
        self.goto(join, ty, ov, other);
        let v = self.land(join);
        self.merge_arms(mark, [yes, no], was_dead, n);
        (ty, v)
    }
    /// The state an arm ends in, then back to the state at `mark` for the next arm.
    fn arm_edge(&mut self, mark: usize) -> Option<(u32, u32)> {
        let e = (!self.dead).then(|| self.edge_state(mark));
        self.undo(mark);
        self.dead = false;
        e
    }
    fn merge_arms(
        &mut self,
        mark: usize,
        arms: [Option<(u32, u32)>; 2],
        was_dead: bool,
        n: NodeId,
    ) {
        let mut edges = std::mem::take(&mut self.edge_buf);
        edges.clear();
        edges.extend(arms.into_iter().flatten());
        self.merge(mark, &edges, n);
        self.edge_buf = edges;
        self.dead |= was_dead;
    }

    fn push_frame(&mut self, label: Option<String>, kind: FrameKind, ty: Option<TyId>) {
        let exit = self.new_join();
        self.frames.push(Frame {
            label,
            kind,
            header: None,
            exit,
            ty,
            left: false,
            mark: self.log.len(),
            born: self.born,
            exits: self.exits.len(),
        });
    }
    /// Pop the innermost frame, joining its exits (OWN-16).
    fn pop_frame(&mut self, n: NodeId) -> Frame {
        let fi = self.frames.len() - 1;
        self.merge_exits(fi, n);
        self.frames.pop().unwrap()
    }

    fn loop_(
        &mut self,
        n: NodeId,
        want: Option<Want>,
        label: Option<String>,
    ) -> (TyId, Option<ValueId>) {
        let body = self.kids(n)[0];
        self.push_frame(label, FrameKind::Loop, want.map(|w| w.ty));
        if self.live() {
            let header = self.new_block(&[]);
            self.jump(header, &[], n);
            self.start(header);
            self.frames.last_mut().unwrap().header = Some(header);
        }
        self.loop_body(body);
        if let Some(h) = self.frames.last().unwrap().header {
            self.jump(h, &[], body);
        }
        self.back_edge(self.frames.len() - 1);
        let f = self.pop_frame(n);
        let v = self.land(f.exit);
        if !f.left {
            return (NEVER, None);
        }
        (f.ty.unwrap_or(UNIT), v)
    }
    /// A loop body's value is discarded; the spec does not say it must be unit (Q16).
    fn loop_body(&mut self, body: NodeId) {
        let (t, _) = self.expr(body, None);
        if t != UNIT && t != NEVER && t != ERROR {
            self.gap(body, "a loop body whose value is not unit (Q16)");
        }
    }

    fn while_(&mut self, n: NodeId, label: Option<String>) -> (TyId, Option<ValueId>) {
        let c = self.kids(n);
        if self.kind(c[0]) == NodeKind::ConditionLet {
            return self.gap_ty(c[0], "match and patterns are not built");
        }
        self.push_frame(label, FrameKind::While, Some(UNIT));
        let mut header = None;
        if self.live() {
            let h = self.new_block(&[]);
            self.jump(h, &[], n);
            self.start(h);
            header = Some(h);
            self.frames.last_mut().unwrap().header = header;
        }
        let (_, cv) = self.expr(c[0], want_rule(BOOL, "LANG-11"));
        // The condition's false edge leaves for the exit (OWN-17).
        self.exit_edge(self.frames.len() - 1);
        let exit = self.frames.last().unwrap().exit;
        if self.live()
            && let Some(cv) = cv
        {
            let body = self.new_block(&[]);
            let out = self.new_block(&[]);
            self.joins[exit].block = Some(out);
            let (ye, ne) = (self.edge(body, &[]), self.edge(out, &[]));
            self.end(
                Terminator::Branch {
                    condition: cv,
                    yes: ye,
                    no: ne,
                },
                Some(c[0]),
            );
            self.start(body);
        }
        self.loop_body(c[1]);
        if let Some(h) = header {
            self.jump(h, &[], c[1]);
        }
        self.back_edge(self.frames.len() - 1);
        let f = self.pop_frame(n);
        self.land(f.exit);
        (UNIT, None)
    }

    fn labelled(&mut self, n: NodeId, want: Option<Want>) -> (TyId, Option<ValueId>) {
        let label = Some(self.token_text(n).to_owned());
        let e = self.kids(n)[0];
        match self.kind(e) {
            NodeKind::Loop => self.loop_(e, want, label),
            NodeKind::While => self.while_(e, label),
            NodeKind::Block => {
                self.push_frame(label, FrameKind::Block, want.map(|w| w.ty));
                let (t, v) = self.block(e, want);
                let exit = self.frames.last().unwrap().exit;
                let fixed = self.frames.last().unwrap().ty;
                let ty = match fixed {
                    Some(f) if t != NEVER => {
                        self.coerce(
                            t,
                            Want {
                                ty: f,
                                rule: "LANG-11",
                            },
                            e,
                        );
                        f
                    }
                    Some(f) => f,
                    None => t,
                };
                self.goto(exit, ty, v, e);
                self.exit_edge(self.frames.len() - 1);
                let f = self.pop_frame(n);
                let v = self.land(f.exit);
                if t == NEVER && !f.left {
                    return (NEVER, None);
                }
                (ty, v)
            }
            _ => self.gap_ty(e, "labelled form is not built"),
        }
    }

    /// The frame an exit leaves: the labelled one, or the innermost loop.
    fn frame_of(&mut self, n: NodeId) -> Option<usize> {
        let label = self.item.tree.node(n).token.map(|_| self.token_text(n));
        let i = self.frames.iter().rposition(|f| match label {
            Some(l) => f.label.as_deref() == Some(l),
            None => f.kind != FrameKind::Block,
        });
        if i.is_none() {
            self.gap(n, "an exit with no enclosing loop or label (Q17)");
        }
        i
    }

    fn break_(&mut self, n: NodeId) -> (TyId, Option<ValueId>) {
        let value = self.kids(n).first().copied();
        let Some(i) = self.frame_of(n) else {
            return (NEVER, None);
        };
        if self.frames[i].kind == FrameKind::While && value.is_some() {
            self.gap(n, "`break` with a value out of `while` (Q18)");
            return (NEVER, None);
        }
        let fixed = self.frames[i].ty;
        let (t, v) = match value {
            Some(e) => self.expr(e, fixed.and_then(want)),
            None => {
                if let Some(f) = fixed {
                    self.coerce(
                        UNIT,
                        Want {
                            ty: f,
                            rule: "LANG-11",
                        },
                        n,
                    );
                }
                (UNIT, None)
            }
        };
        let f = &mut self.frames[i];
        f.left = true;
        if f.ty.is_none() && t != NEVER {
            f.ty = Some(t);
        }
        let (exit, ty) = (f.exit, f.ty.unwrap_or(t));
        self.goto(exit, ty, v, n);
        self.exit_edge(i);
        self.dead = true;
        (NEVER, None)
    }

    fn continue_(&mut self, n: NodeId) -> (TyId, Option<ValueId>) {
        let Some(i) = self.frame_of(n) else {
            return (NEVER, None);
        };
        if self.frames[i].kind == FrameKind::Block {
            self.gap(n, "`continue` naming a labelled block (Q17)");
            return (NEVER, None);
        }
        if let Some(h) = self.frames[i].header {
            self.jump(h, &[], n);
        }
        self.cur = None;
        self.back_edge(i);
        self.dead = true;
        (NEVER, None)
    }

    fn return_(&mut self, n: NodeId) -> (TyId, Option<ValueId>) {
        let w = Want {
            ty: self.fd.result,
            rule: "LANG-8",
        };
        let v = match self.kids(n).first() {
            Some(&e) => self.expr(e, Some(w)).1,
            None => {
                self.coerce(UNIT, w, n);
                None
            }
        };
        if self.live() {
            let r = PoolRange::push(&mut self.b.pools.values, v);
            self.end(Terminator::Return(r), Some(n));
        }
        self.dead = true;
        (NEVER, None)
    }

    // ---- operators
    fn unary(&mut self, n: NodeId, want: Option<Want>) -> (TyId, Option<ValueId>) {
        let e = self.kids(n)[0];
        match self.token_text(n) {
            "-" => {
                if let Some(lit) = self.bare_number(e) {
                    let s = match int_suffix(self.token_text(lit)).1 {
                        Some(s) => Some(s),
                        None => want.and_then(|w| match self.t.kind(w.ty) {
                            TyKind::Scalar(s) => Some(*s),
                            _ => None,
                        }),
                    };
                    // A signed integer's MIN magnitude is accepted under negation (LANG-3).
                    if s.is_some_and(|s| {
                        matches!(Ty::Scalar(s).int(), Some((true, _)))
                            || matches!(s, Scalar::F32 | Scalar::F64)
                    }) {
                        return self.literal(lit, want, true);
                    }
                }
                let (t, v) = self.expr(e, want.filter(|w| self.is_numeric(w.ty)));
                if t == ERROR || t == NEVER {
                    return (t, None);
                }
                let r = match int_of(&self.t, t) {
                    Some((true, _)) => self.emit_unary(UnaryOp::Neg, t, v, n),
                    // Unary minus fails with Overflow unless the result fits (SEM-1): on
                    // an unsigned type that is 0 - x.
                    Some((false, _)) => {
                        let zero = self.konst(value_ty(&self.t, t).ok().flatten().unwrap(), 0, n);
                        self.emit_binary(BinaryOp::Sub, t, zero, v, n)
                    }
                    None if is_float(&self.t, t) => self.emit_unary(UnaryOp::Neg, t, v, n),
                    None => return self.no_operator(n, t),
                };
                (t, r)
            }
            "!" => {
                let (t, v) = self.expr(e, None);
                if t != BOOL {
                    return self.no_operator(n, t);
                }
                (t, self.emit_unary(UnaryOp::Not, t, v, n))
            }
            "~" => {
                let (t, v) = self.expr(e, want.filter(|w| int_of(&self.t, w.ty).is_some()));
                if int_of(&self.t, t).is_none() {
                    return self.no_operator(n, t);
                }
                (t, self.emit_unary(UnaryOp::BitNot, t, v, n))
            }
            "copy" => {
                let (t, v) = self.expr(e, want);
                if !copyable(&self.p.decls, &self.t, t, &self.params) {
                    let a = self.name(t);
                    self.refuse("OWN-6", n, format!("`{a}` is not plain"));
                }
                (t, v)
            }
            "take" => match self.place(e) {
                // OWN-6: `take` moves a place.
                Ok(Some(at)) => {
                    self.use_place(at, Use::Take, n);
                    (at.ty, self.emit_place(at, n))
                }
                Ok(None) => {
                    let _ = self.expr(e, None);
                    self.refuse("OWN-6", n, "`take` of an expression that is not a place");
                    (ERROR, None)
                }
                Err(()) => (ERROR, None),
            },
            "view" => match self.place(e) {
                // OWN-8: a temporary view, to the end of its statement unless bound.
                Ok(Some(at)) => {
                    self.use_place(at, Use::View(Hold::View(Extent::Temp)), n);
                    let t = match self.t.kind(at.ty) {
                        TyKind::View(_) => at.ty,
                        _ => self.t.mk(TyKind::View(at.ty)),
                    };
                    self.gap(n, "values of this type are not built");
                    (t, None)
                }
                Ok(None) => self.gap_ty(n, "`view` of a value that is not a place"),
                Err(()) => (ERROR, None),
            },
            _ => self.gap_ty(n, "`mut` outside an argument (Q25)"),
        }
    }
    fn no_operator(&mut self, n: NodeId, t: TyId) -> (TyId, Option<ValueId>) {
        if t == ERROR || t == NEVER {
            return (t, None);
        }
        self.gap_ty(
            n,
            "an operator on a type the spec does not define it for (Q11)",
        )
    }
    fn is_numeric(&self, t: TyId) -> bool {
        matches!(self.t.kind(t), TyKind::Scalar(s) if numeric(*s))
    }
    fn emit_unary(
        &mut self,
        op: UnaryOp,
        t: TyId,
        v: Option<ValueId>,
        n: NodeId,
    ) -> Option<ValueId> {
        let (operand, ty) = (v?, value_ty(&self.t, t).ok().flatten()?);
        if !self.live() {
            return None;
        }
        let result = self.value(ty);
        self.inst(
            Inst::Unary {
                result,
                op,
                operand,
                fail: None,
            },
            n,
        );
        Some(result)
    }
    fn emit_binary(
        &mut self,
        op: BinaryOp,
        t: TyId,
        l: Option<ValueId>,
        r: Option<ValueId>,
        n: NodeId,
    ) -> Option<ValueId> {
        let (left, right, ty) = (l?, r?, value_ty(&self.t, t).ok().flatten()?);
        if !self.live() {
            return None;
        }
        let result = self.value(ty);
        self.inst(
            Inst::Binary {
                result,
                op,
                left,
                right,
                fail: None,
            },
            n,
        );
        Some(result)
    }

    fn binary(&mut self, n: NodeId, want: Option<Want>) -> (TyId, Option<ValueId>) {
        let c = self.kids(n);
        let (l, r) = (c[0], c[1]);
        let op = self.token_text(n);
        match op {
            "&&" | "||" => self.short_circuit(n, op == "&&"),
            ".." | "..=" => self.gap_ty(n, "ranges are not built"),
            "==" | "!=" | "<" | "<=" | ">" | ">=" => {
                let (t, lv, rv) = self.operands(l, r, None);
                if t == ERROR || t == NEVER {
                    return (BOOL, None);
                }
                let need = if matches!(op, "==" | "!=") {
                    KEY
                } else {
                    ORDERED
                };
                if has_bounds(&self.p.decls, &self.t, t, &self.params) & need == 0 {
                    let a = self.name(t);
                    let b = if need == KEY { "key" } else { "ordered" };
                    self.refuse("SEM-7", n, format!("`{a}` is not {b}"));
                    return (BOOL, None);
                }
                let cop = match op {
                    "==" => CompareOp::Eq,
                    "!=" => CompareOp::Ne,
                    "<" => CompareOp::Lt,
                    "<=" => CompareOp::Le,
                    ">" => CompareOp::Gt,
                    _ => CompareOp::Ge,
                };
                if t == UNIT {
                    let eq = matches!(cop, CompareOp::Eq | CompareOp::Le | CompareOp::Ge);
                    return (BOOL, self.konst(Ty::Scalar(Scalar::Bool), u64::from(eq), n));
                }
                if value_ty(&self.t, t).ok().flatten().is_none() {
                    return self.gap_ty(n, "comparison of this type is not built");
                }
                let v = match (lv, rv) {
                    (Some(left), Some(right)) if self.live() => {
                        let result = self.value(Ty::Scalar(Scalar::Bool));
                        self.inst(
                            Inst::Compare {
                                result,
                                op: cop,
                                left,
                                right,
                            },
                            n,
                        );
                        Some(result)
                    }
                    _ => None,
                };
                (BOOL, v)
            }
            _ => {
                let hint = want.filter(|w| self.is_numeric(w.ty));
                let (t, lv, rv) = self.operands(l, r, hint);
                if t == ERROR || t == NEVER {
                    return (t, None);
                }
                (t, self.arith(op, t, lv, rv, n))
            }
        }
    }

    /// Type an operator's two operands, which have one type (SEM-1): the first that
    /// synthesises one fixes it, else the expected type does (LANG-11). A number literal
    /// on the left is emitted after the right operand fixes its type; it has no effect.
    fn operands(
        &mut self,
        l: NodeId,
        r: NodeId,
        hint: Option<Want>,
    ) -> (TyId, Option<ValueId>, Option<ValueId>) {
        let before = self.out.refusals.len();
        let (t, lv, rv) = self.operand_pair(l, r, hint);
        // A refused operand is the program's one refusal here; the operator's own
        // checks and its result do not add another.
        if self.out.refusals.len() > before && t != NEVER {
            return (ERROR, None, None);
        }
        (t, lv, rv)
    }
    fn operand_pair(
        &mut self,
        l: NodeId,
        r: NodeId,
        hint: Option<Want>,
    ) -> (TyId, Option<ValueId>, Option<ValueId>) {
        let same = |t: TyId| {
            Some(Want {
                ty: t,
                rule: "SEM-1",
            })
        };
        if self.synthesises(l) {
            let (lt, lv) = self.expr(l, None);
            if lt == NEVER || lt == ERROR {
                if self.synthesises(r) {
                    self.expr(r, None);
                }
                return (lt, None, None);
            }
            let (_, rv) = self.expr(r, same(lt));
            return (lt, lv, rv);
        }
        if self.bare_number(l).is_some() && self.synthesises(r) {
            let (rt, rv) = self.expr(r, None);
            if rt == NEVER || rt == ERROR {
                return (rt, None, None);
            }
            let (_, lv) = self.expr(l, same(rt));
            return (rt, lv, rv);
        }
        let Some(h) = hint else {
            self.refuse("LANG-11", l, "an unsuffixed number needs an expected type");
            return (ERROR, None, None);
        };
        let (lt, lv) = self.expr(l, Some(h));
        let (_, rv) = self.expr(r, same(lt));
        (lt, lv, rv)
    }

    /// An arithmetic, bitwise or shift operator on two values of `t` (SEM-1, SEM-2).
    fn arith(
        &mut self,
        op: &str,
        t: TyId,
        l: Option<ValueId>,
        r: Option<ValueId>,
        n: NodeId,
    ) -> Option<ValueId> {
        let int = int_of(&self.t, t).is_some();
        let float = is_float(&self.t, t);
        let bop = match op {
            "+" => BinaryOp::Add,
            "-" => BinaryOp::Sub,
            "*" => BinaryOp::Mul,
            "/" => BinaryOp::Div,
            "%" => BinaryOp::Rem,
            "&" => BinaryOp::BitAnd,
            "|" => BinaryOp::BitOr,
            "^" => BinaryOp::BitXor,
            "<<" => BinaryOp::Shl,
            _ => BinaryOp::Shr,
        };
        let ok = match bop {
            BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div => int || float,
            BinaryOp::Rem if float => {
                self.refuse("SEM-2", n, "`%` is integer-only");
                return None;
            }
            _ => int,
        };
        if t == ERROR || t == NEVER {
            return None;
        }
        if !ok {
            self.no_operator(n, t);
            return None;
        }
        self.emit_binary(bop, t, l, r, n)
    }

    fn short_circuit(&mut self, n: NodeId, and: bool) -> (TyId, Option<ValueId>) {
        let c = self.kids(n);
        let (lt, lv) = self.expr(c[0], None);
        if lt != BOOL {
            return self.no_operator(n, lt);
        }
        let join = self.new_join();
        let mut right = None;
        if self.live()
            && let Some(lv) = lv
        {
            // `&&` skips its right side when the left is false, `||` when it is true
            // (LANG-6), and the skipped side's value is the left one.
            let rb = self.new_block(&[]);
            let jb = self.new_block(&[Ty::Scalar(Scalar::Bool)]);
            self.joins[join].block = Some(jb);
            let (to_r, to_j) = (self.edge(rb, &[]), self.edge(jb, &[lv]));
            let (yes, no) = if and { (to_r, to_j) } else { (to_j, to_r) };
            self.end(
                Terminator::Branch {
                    condition: lv,
                    yes,
                    no,
                },
                Some(c[0]),
            );
            self.start(rb);
            right = Some(rb);
        }
        // The right side runs on one path only: its effects join with none (OWN-16).
        let (was_dead, mark) = (self.dead, self.log.len());
        let (rt, rv) = self.expr(c[1], None);
        let ran = self.arm_edge(mark);
        let skipped = Some((self.diffs.len() as u32, 0));
        self.merge_arms(mark, [ran, skipped], was_dead, n);
        if rt != BOOL && rt != NEVER {
            return self.no_operator(n, rt);
        }
        if right.is_some() {
            self.goto(join, BOOL, rv, c[1]);
        }
        (BOOL, self.land(join))
    }

    // ---- calls
    fn call(&mut self, n: NodeId, want: Option<Want>) -> (TyId, Option<ValueId>) {
        self.out.counts.calls += 1;
        let c = self.kids(n);
        let (callee, args) = (c[0], &c[1..]);
        self.out.counts.uses += 1;
        let p: &'c Program<'a> = self.p;
        let target = match self.kind(callee) {
            NodeKind::Path => p.resolved(self.at, callee).map(|u| &u.target),
            _ => None,
        };
        let f = match target {
            Some(Target::Definition(path)) => match p.by_path.get(path) {
                Some(Def::Fn(f)) => *f as usize,
                _ => return self.gap_ty(callee, "construction is not built"),
            },
            Some(Target::Missing) | None if self.kind(callee) == NodeKind::Path => {
                self.halt();
                return (ERROR, None);
            }
            Some(Target::ContextualConstructor { .. }) => {
                return self.gap_ty(callee, "construction is not built");
            }
            _ => return self.gap_ty(callee, "calls through values are not built"),
        };
        let g = &self.p.fns[f];
        if args.len() != g.params.len() {
            return self.gap_ty(n, "a call with the wrong number of arguments (Q13)");
        }
        if g.waits {
            self.waited = true;
            if !self.fd.waits && !self.entry {
                self.refuse(
                    "LANG-8",
                    n,
                    "calls a `waits` function but is not declared `waits`",
                );
            }
        }
        let mut found: Vec<Option<TyId>> = vec![None; g.generics.len()];
        if let Some(w) = want {
            self.unify(g.result, w.ty, &mut found);
        }
        let mut vals: Vec<Option<ValueId>> = vec![None; args.len()];
        let mut later = vec![];
        self.call_seq += 1;
        let call = self.call_seq;
        let holds = self.freezes.len();
        for (i, &a) in args.iter().enumerate() {
            if self.kind(a) != NodeKind::Argument {
                self.gap(a, "labelled and bytes arguments are not built");
                continue;
            }
            let e = self.kids(a)[0];
            if self.argument(
                g.params[i].mode,
                g.params[i].ty,
                call,
                i,
                e,
                &mut found,
                &mut vals[i],
            ) {
                continue;
            }
            if self.bare_number(e).is_some() && !self.synthesises(e) {
                later.push(i);
                continue;
            }
            let pt = g.params[i].ty;
            vals[i] = match self.known(pt, &found) {
                Some(t) => self.expr(e, want_rule(t, "LANG-11")).1,
                None => {
                    let (at, v) = self.expr(e, None);
                    if !self.unify(pt, at, &mut found) {
                        self.refuse(
                            "LANG-11",
                            e,
                            "a type parameter is determined twice differently",
                        );
                    }
                    v
                }
            };
        }
        for i in later {
            let e = self.kids(args[i])[0];
            match self.known(g.params[i].ty, &found) {
                Some(t) => vals[i] = self.expr(e, want_rule(t, "LANG-11")).1,
                None => {
                    self.refuse("LANG-11", e, "an unsuffixed number needs an expected type");
                }
            }
        }
        let tys: Vec<TyId> = found.iter().map(|s| s.unwrap_or(ERROR)).collect();
        if found.iter().any(Option::is_none) && !self.broken {
            self.refuse(
                "LANG-11",
                n,
                "a type parameter of the callee is undetermined",
            );
        }
        // The call completes: its arguments' holds end, except a view the scoped result
        // is anchored to, which lasts for the result's extent (OWN-11, OWN-20).
        let anchors = &g.anchors;
        self.unfreeze(holds, |h| match h {
            Hold::CallView(c, i) if c == call => {
                anchors.contains(&i).then_some(Hold::View(Extent::Temp))
            }
            Hold::CallMut(c) if c == call => None,
            h => Some(h),
        });
        let result = subst(&mut self.t, g.result, &tys);
        if result == NEVER {
            self.dead = true;
        }
        let v = if g.generics.is_empty() {
            self.emit_call(f, &vals, result, n)
        } else {
            self.intrinsic(f, &tys, &vals, n)
        };
        (result, v)
    }

    /// Check argument `e` for a parameter of mode `mode` and type `pt` when its mode
    /// decides how (OWN-7): a `mut` loan, a place viewed or moved. False when `e` is an
    /// ordinary value, checked by the caller.
    #[allow(clippy::too_many_arguments)]
    fn argument(
        &mut self,
        mode: Mode,
        pt: TyId,
        call: u32,
        i: usize,
        e: NodeId,
        found: &mut [Option<TyId>],
        val: &mut Option<ValueId>,
    ) -> bool {
        let unary = (self.kind(e) == NodeKind::Unary).then(|| self.token_text(e));
        match mode {
            Mode::Mut => {
                if unary != Some("mut") {
                    // Refused as written; a place here is neither read nor moved.
                    if !matches!(self.place(e), Ok(Some(_)) | Err(())) {
                        let _ = self.expr(e, None);
                    }
                    self.refuse("OWN-7", e, "an argument for a `mut` parameter is `mut p`");
                    return true;
                }
                let p = self.kids(e)[0];
                match self.place(p) {
                    Ok(Some(at)) => {
                        self.use_place(at, Use::Mut(Hold::CallMut(call)), e);
                        self.arg_ty(pt, at.ty, e, found);
                        self.gap(e, "mut arguments are not built");
                    }
                    Ok(None) => {
                        let _ = self.expr(p, None);
                        self.refuse("OWN-7", e, "`mut` of an expression that is not a place");
                    }
                    Err(()) => {}
                }
                true
            }
            _ if unary == Some("mut") => {
                let _ = self.expr(self.kids(e)[0], None);
                self.refuse("OWN-7", e, "`mut` is written only for a `mut` parameter");
                true
            }
            Mode::View if unary == Some("view") => {
                // An explicit view where the parameter views anyway.
                let (t, _) = self.expr(e, None);
                let t = match self.t.kind(t) {
                    TyKind::View(x) if !matches!(self.t.kind(pt), TyKind::View(_)) => *x,
                    _ => t,
                };
                self.arg_ty(pt, t, e, found);
                true
            }
            Mode::View | Mode::Take if unary.is_none() => {
                let at = match self.place(e) {
                    Ok(Some(at)) => at,
                    Ok(None) => return false,
                    Err(()) => return true,
                };
                let ty = at.ty;
                if self.copies(ty) {
                    // A copyable value is copied (OWN-7).
                    self.use_place(at, Use::Value, e);
                    *val = self.emit_place(at, e);
                } else if mode == Mode::Take {
                    let name = self.slot_name(at.place.slot);
                    self.refuse(
                        "OWN-7",
                        e,
                        format!("a moving place is passed as `take {name}`"),
                    );
                    return true;
                } else {
                    // A bare place for an unmarked parameter is a view for the call.
                    self.use_place(at, Use::View(Hold::CallView(call, i as u32)), e);
                }
                let t = match self.t.kind(ty) {
                    TyKind::View(x) if !matches!(self.t.kind(pt), TyKind::View(_)) => *x,
                    _ => ty,
                };
                self.arg_ty(pt, t, e, found);
                true
            }
            _ => false,
        }
    }
    /// An argument of type `at` for a parameter of type `pt` (LANG-11).
    fn arg_ty(&mut self, pt: TyId, at: TyId, e: NodeId, found: &mut [Option<TyId>]) {
        match self.known(pt, found) {
            Some(t) => self.coerce(
                at,
                Want {
                    ty: t,
                    rule: "LANG-11",
                },
                e,
            ),
            None => {
                if !self.unify(pt, at, found) {
                    self.refuse(
                        "LANG-11",
                        e,
                        "a type parameter is determined twice differently",
                    );
                }
            }
        }
    }

    /// `pt` with the type arguments found so far, when they determine it.
    fn known(&mut self, pt: TyId, s: &[Option<TyId>]) -> Option<TyId> {
        if !has_param(&self.t, pt) {
            return Some(pt);
        }
        if s.iter().any(Option::is_none) {
            return None;
        }
        let tys: Vec<TyId> = s.iter().map(|t| t.unwrap()).collect();
        Some(subst(&mut self.t, pt, &tys))
    }
    /// Find type arguments from one argument type; false when one is found twice
    /// differently (LANG-11). A mismatch elsewhere is the argument's own refusal.
    fn unify(&mut self, pt: TyId, at: TyId, s: &mut [Option<TyId>]) -> bool {
        self.out.counts.types += 1;
        if at == NEVER || at == ERROR {
            return true;
        }
        match (self.t.kind(pt).clone(), self.t.kind(at).clone()) {
            (TyKind::Param(i), _) => match s[i as usize] {
                None => {
                    s[i as usize] = Some(at);
                    true
                }
                Some(x) => x == at || x == ERROR,
            },
            (TyKind::Tuple(a), TyKind::Tuple(b)) if a.len() == b.len() => {
                a.iter().zip(&b).all(|(&x, &y)| self.unify(x, y, s))
            }
            (TyKind::Adt(d, a), TyKind::Adt(e, b)) if d == e => {
                a.iter().zip(&b).all(|(&x, &y)| self.unify(x, y, s))
            }
            (TyKind::View(a), TyKind::View(b)) => self.unify(a, b, s),
            _ => true,
        }
    }

    fn emit_call(
        &mut self,
        f: usize,
        vals: &[Option<ValueId>],
        result: TyId,
        n: NodeId,
    ) -> Option<ValueId> {
        let (func, signature) = match self.e.row(self.p, f) {
            Ok(row) => row,
            Err(what) => {
                self.gap(n, what);
                return None;
            }
        };
        if !self.live() {
            return None;
        }
        let g = &self.p.fns[f];
        let start = self.b.pools.args.len();
        for (i, p) in g.params.iter().enumerate() {
            if matches!(value_ty(&self.t, p.ty), Ok(Some(_))) {
                let Some(v) = vals[i] else {
                    self.b.pools.args.truncate(start);
                    return None;
                };
                self.b.pools.args.push(Arg::Value(v));
            }
        }
        let args = PoolRange::new(start as u32, (self.b.pools.args.len() - start) as u32);
        let start = self.b.pools.values.len();
        for k in 0..self.e.module.signatures[signature.index()].results.len() {
            let t = self.e.module.signatures[signature.index()].results[k];
            let v = self.value(t);
            self.b.pools.values.push(v);
        }
        let results = PoolRange::new(start as u32, (self.b.pools.values.len() - start) as u32);
        let first = self.b.pools.values.get(start).copied();
        self.b.calls.push(CallSite {
            callee: Callee::Direct(func),
            signature,
            args,
            results,
            fail: None,
        });
        let call = CallId(self.b.calls.len() as u32 - 1);
        self.inst(Inst::Call { call }, n);
        if result == NEVER {
            // The callee never returns normally; nothing follows the call (FORM v1a).
            self.end(Terminator::Unreachable, Some(n));
        }
        first
    }

    /// The core integer operations built so far (SEM-1); T and U range over the
    /// integer types, which no bound states (Q15).
    fn intrinsic(
        &mut self,
        f: usize,
        tys: &[TyId],
        vals: &[Option<ValueId>],
        n: NodeId,
    ) -> Option<ValueId> {
        let path = &self.p.fns[f].path;
        let name = path.0.name.as_str();
        if !(path.package_is("core", "1") && path.0.modules == ["int"]) {
            self.gap(n, "calls to generic functions are not built");
            return None;
        }
        if tys.contains(&ERROR) {
            return None;
        }
        let ints: Vec<(bool, u32)> = tys.iter().filter_map(|&t| int_of(&self.t, t)).collect();
        if ints.len() != tys.len() {
            self.gap(n, "a core integer operation on a non-integer type (Q15)");
            return None;
        }
        let arith = |m: ArithMode, op: ArithOp| Some((m, op));
        let op = match name {
            "wrapping_add" => arith(ArithMode::Wrapping, ArithOp::Add),
            "wrapping_sub" => arith(ArithMode::Wrapping, ArithOp::Sub),
            "wrapping_mul" => arith(ArithMode::Wrapping, ArithOp::Mul),
            "saturating_add" => arith(ArithMode::Saturating, ArithOp::Add),
            "saturating_sub" => arith(ArithMode::Saturating, ArithOp::Sub),
            "saturating_mul" => arith(ArithMode::Saturating, ArithOp::Mul),
            _ => None,
        };
        if let Some((mode, op)) = op {
            let ty = value_ty(&self.t, tys[0]).ok().flatten()?;
            let (left, right) = (vals[0]?, vals[1]?);
            if !self.live() {
                return None;
            }
            let result = self.value(ty);
            self.inst(
                Inst::IntArith {
                    result,
                    flag: None,
                    op,
                    mode,
                    left,
                    right,
                },
                n,
            );
            return Some(result);
        }
        let mode = match name {
            "widen" => {
                let ((fs, a), (ts, b)) = (ints[0], ints[1]);
                if !((fs == ts && a <= b) || (!fs && ts && a < b)) {
                    let (x, y) = (self.name(tys[0]), self.name(tys[1]));
                    self.refuse("SEM-1", n, format!("not every `{x}` fits `{y}`"));
                    return None;
                }
                ConvertMode::Widen
            }
            "truncate" if ints[1].1 <= ints[0].1 => ConvertMode::Truncate,
            "truncate" => {
                self.gap(n, "`int.truncate` to a wider type (Q15)");
                return None;
            }
            _ => {
                self.gap(n, "this core integer operation is not built");
                return None;
            }
        };
        let ty = value_ty(&self.t, tys[1]).ok().flatten()?;
        let operand = vals[0]?;
        if !self.live() {
            return None;
        }
        let result = self.value(ty);
        self.inst(
            Inst::Convert {
                result,
                flag: None,
                operand,
                mode,
            },
            n,
        );
        Some(result)
    }

    // ---- types written in bodies
    fn ty(&mut self, n: NodeId) -> TyId {
        self.out.counts.types += 1;
        let c = self.kids(n);
        match self.kind(n) {
            NodeKind::UnitType => UNIT,
            NodeKind::GroupType => self.ty(c[0]),
            NodeKind::TupleType => {
                let xs = c.iter().map(|&x| self.ty(x)).collect();
                self.t.mk(TyKind::Tuple(xs))
            }
            NodeKind::TypePath => match self.p.target(self.at, n).cloned() {
                Some(Target::Builtin(name)) => match Scalar::from_name(&name) {
                    Some(s) => scalar(s),
                    None => ERROR,
                },
                Some(Target::Definition(p)) if is_core_prelude(&p, &["Never"]) => NEVER,
                Some(Target::Definition(p)) => match self.p.by_path.get(&p) {
                    Some(Def::Type(d)) if self.p.decls[d.index()].generics.is_empty() => {
                        self.t.mk(TyKind::Adt(*d, vec![]))
                    }
                    Some(Def::Type(_)) => {
                        self.refuse("LANG-12", n, "this type takes type arguments");
                        ERROR
                    }
                    _ => {
                        self.refuse("LANG-7", n, format!("`{p}` is not a type"));
                        ERROR
                    }
                },
                Some(Target::Binder(b)) => {
                    let name = &self.p.items[self.at.module as usize][self.at.item as usize]
                        .resolution
                        .binders[b as usize]
                        .name;
                    match self.fd.generics.iter().position(|g| &g.name == name) {
                        Some(i) => self.t.mk(TyKind::Param(i as u32)),
                        None => {
                            self.gap(n, "this type parameter is not built");
                            ERROR
                        }
                    }
                }
                _ => {
                    self.halt();
                    ERROR
                }
            },
            _ => {
                self.gap(n, "this type form is not built in bodies");
                ERROR
            }
        }
    }
}

fn want_rule(ty: TyId, rule: &'static str) -> Option<Want> {
    Some(Want { ty, rule })
}

fn is_type_node(k: NodeKind) -> bool {
    matches!(
        k,
        NodeKind::TypePath
            | NodeKind::TypeArguments
            | NodeKind::UnitType
            | NodeKind::GroupType
            | NodeKind::TupleType
            | NodeKind::CallableType
            | NodeKind::ScopedCallableType
            | NodeKind::OwnCallableType
            | NodeKind::Anchor
    )
}

fn numeric(s: Scalar) -> bool {
    !matches!(s, Scalar::Bool | Scalar::Char)
}

/// An integer token's digits (with prefix) and its suffix (LANG-3).
fn int_suffix(text: &str) -> (&str, Option<Scalar>) {
    for s in [
        Scalar::U8,
        Scalar::U16,
        Scalar::U32,
        Scalar::U64,
        Scalar::I8,
        Scalar::I16,
        Scalar::I32,
        Scalar::I64,
    ] {
        if let Some(d) = text.strip_suffix(s.name()) {
            return (d, Some(s));
        }
    }
    (text, None)
}
fn float_suffix(text: &str) -> (&str, Option<Scalar>) {
    for s in [Scalar::F32, Scalar::F64] {
        if let Some(d) = text.strip_suffix(s.name()) {
            return (d, Some(s));
        }
    }
    (text, None)
}
/// The magnitude an integer token's digits spell, or None past u128.
fn int_magnitude(digits: &str) -> Option<u128> {
    let d: String = digits.chars().filter(|&c| c != '_').collect();
    let (radix, body) = match d.get(..2) {
        Some("0x") => (16, &d[2..]),
        Some("0o") => (8, &d[2..]),
        Some("0b") => (2, &d[2..]),
        _ => (10, &d[..]),
    };
    u128::from_str_radix(body, radix).ok()
}
fn float_bits(s: Scalar, x: f64) -> u64 {
    match s {
        Scalar::F32 => u64::from((x as f32).to_bits()),
        _ => x.to_bits(),
    }
}
