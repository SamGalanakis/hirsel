//! Bodies: one forward visit per function that types its body against the published
//! signatures and summaries and emits CoreIR as it goes (LANG-8: nothing else is read).
//!
//! Built so far (slice b): scalars and unit, blocks, `let` and `let mut`, assignment and
//! compound assignment, the operators of SEM-1, SEM-2 and SEM-7, `if`, `loop`, `while`,
//! labels, `break`, `continue`, `return`, direct calls of non-generic functions and the
//! core integer operations. Anything else is a [`Gap`]: the body is still checked, but no
//! code is emitted for it. An operation that can fail names no failure successor, so the
//! failure leaves the function with its cause (FORM v1a); nothing here needs cleanup yet.

use crate::decls::{FnDecl, KEY, ORDERED, copyable, has_bounds, is_core_prelude};
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
#[derive(Clone, Copy)]
struct Var {
    ty: TyId,
    binding: Binding,
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
    vars: HashMap<u32, Var>,
    binder_at: HashMap<(u32, u32), u32>,
    frames: Vec<Frame>,
    joins: Vec<Join>,
    /// Per node: 0 unknown, 1 synthesises a type, 2 needs an expected type.
    synth: Vec<u8>,
    waited: bool,
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
            vars: HashMap::new(),
            binder_at,
            frames: vec![],
            joins: vec![],
            synth: vec![0; item.tree.nodes.len()],
            waited: false,
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
    fn gap(&mut self, n: NodeId, what: &'static str) {
        let site = self.p.site(self.at, n);
        self.out.gaps.push(Gap::new(site, what));
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
        let binders = &self.p.items[self.at.module as usize][self.at.item as usize]
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
                self.vars.insert(
                    b.id,
                    Var {
                        ty: pd.ty,
                        binding: Binding::Value(v),
                    },
                );
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
        if self.fd.waits && !self.waited {
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
            NodeKind::Index | NodeKind::Projection => {
                self.gap_ty(n, "field and index places are not built")
            }
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
        if !u.projections.is_empty() {
            return self.gap_ty(n, "field and index places are not built");
        }
        match &u.target {
            Target::Binder(b) => {
                let Some(var) = self.vars.get(b).copied() else {
                    return self.gap_ty(n, "this kind of name is not built");
                };
                match var.binding {
                    Binding::Value(v) => (var.ty, v),
                    Binding::Slot(a) => {
                        self.out.counts.places += 1;
                        let v = match (a, value_ty(&self.t, var.ty)) {
                            (Some(address), Ok(Some(t))) if self.live() => {
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
                            _ => None,
                        };
                        (var.ty, v)
                    }
                }
            }
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
        let stmts = self.kids(n);
        let mut diverged = false;
        for (i, &s) in stmts.iter().enumerate() {
            self.out.counts.nodes += 1;
            let last = i + 1 == stmts.len();
            match self.kind(s) {
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
            self.gap(s, "let without a value is not built");
            return false;
        };
        if !rest.is_empty() {
            self.gap(rest[0], "let-else is not built");
        }
        let (t, v) = self.expr(value, declared.and_then(want));
        let ty = declared.unwrap_or(t);
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
                let binding = if self.kind(s) == NodeKind::LetMut {
                    Binding::Slot(self.slot(ty, v, pat))
                } else {
                    Binding::Value(v)
                };
                self.vars.insert(b, Var { ty, binding });
            }
            _ => self.gap(pat, "patterns are not built"),
        }
        t == NEVER
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

    /// `q = e` and `q op= e` (OWN-12) on a `let mut` local.
    fn assign(&mut self, s: NodeId) -> bool {
        let c = self.kids(s);
        let (place, value) = (c[0], c[1]);
        let op = self.token_text(s);
        self.out.counts.uses += 1;
        let target = match self.kind(place) {
            NodeKind::Path => self.p.resolved(self.at, place).cloned(),
            _ => None,
        };
        let Some(u) = target else {
            self.gap(place, "field and index places are not built");
            return false;
        };
        if !u.projections.is_empty() {
            self.gap(place, "field and index places are not built");
            return false;
        }
        let var = match u.target {
            Target::Binder(b) => self.vars.get(&b).copied(),
            Target::Missing => {
                self.halt();
                return false;
            }
            _ => None,
        };
        let Some(var) = var else {
            self.gap(place, "assignment to this place is not built");
            return false;
        };
        let Binding::Slot(address) = var.binding else {
            self.refuse(
                "OWN-12",
                place,
                "only a `let mut` local is writable here (Q14)",
            );
            return false;
        };
        self.out.counts.places += 1;
        if op == "=" {
            let (t, v) = self.expr(value, want(var.ty));
            if let (Some(address), Some(value)) = (address, v) {
                self.inst(Inst::Store { address, value }, s);
            }
            return t == NEVER;
        }
        // Compound: resolve once, read the old value, evaluate the right side, compute,
        // install (OWN-12); plain-only, and every type built here is plain.
        let bop = op.trim_end_matches('=');
        let old = match (address, value_ty(&self.t, var.ty)) {
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
                ty: var.ty,
                rule: "SEM-1",
            }),
        );
        if let Some(new) = self.arith(bop, var.ty, old, rv, s)
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
        let Some(&other) = c.get(2) else {
            // Without `else` the type is unit (LANG-11).
            let (_, _) = self.expr(c[1], want_rule(UNIT, "LANG-11"));
            self.goto(join, UNIT, None, c[1]);
            if let Some(no) = no_block {
                self.start(no);
            }
            self.goto(join, UNIT, None, n);
            self.land(join);
            return (UNIT, None);
        };
        let (yt, yv) = self.expr(c[1], want);
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
        let ty = if yt == NEVER { ot } else { yt };
        self.goto(join, ty, ov, other);
        let v = self.land(join);
        (ty, v)
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
        });
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
        let f = self.frames.pop().unwrap();
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
        let f = self.frames.pop().unwrap();
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
                let f = self.frames.pop().unwrap();
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
            _ => self.gap_ty(n, "take, mut and view are not built"),
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
        let (rt, rv) = self.expr(c[1], None);
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
            if !self.fd.waits {
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
        for (i, &a) in args.iter().enumerate() {
            if self.kind(a) != NodeKind::Argument {
                return self.gap_ty(a, "labelled and bytes arguments are not built");
            }
            if g.params[i].mode == Mode::Mut {
                return self.gap_ty(a, "mut arguments are not built");
            }
            let e = self.kids(a)[0];
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
        let result = subst(&mut self.t, g.result, &tys);
        let v = if g.generics.is_empty() {
            self.emit_call(f, &vals, result, n)
        } else {
            self.intrinsic(f, &tys, &vals, n)
        };
        (result, v)
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
            // The callee never returns normally; nothing follows the call.
            self.end(Terminator::Propagate, Some(n));
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
