//! Structural well-formedness, for tests only (feature `wf`): producers' tests call
//! it on what they built; no product path does (we trust our own implementation).
//! It checks references, single definition, arity, the operand typing each
//! instruction states, that type rows have recipes, and that no failure path
//! reaches a `return` (FAIL-14: nothing catches a failure). It does not check
//! dominance, drop-flag discipline or limit-scope balance.

use crate::*;

type R = Result<(), String>;

fn need(ok: bool, msg: impl FnOnce() -> String) -> R {
    if ok { Ok(()) } else { Err(msg()) }
}

fn refs_ok(f: &hirsel_core_layout::FieldTy, n: usize) -> bool {
    use hirsel_core_layout::FieldTy as F;
    match f {
        F::Inline(t) | F::Box(t) | F::Rc(t) | F::Arc(t) | F::Stable(t) | F::Link(t) => {
            t.index() < n
        }
        F::Seq(e) | F::FixedSpan(e) | F::SeqSpan(e) | F::IndexedSpan(e) | F::Array(e, _) => {
            refs_ok(e, n)
        }
        F::Map(k, v) => refs_ok(k, n) && refs_ok(v, n),
        _ => true,
    }
}

pub fn module(m: &Module) -> R {
    let mut failures = None;
    for (i, t) in m.types.iter().enumerate() {
        let fields = match &t.body {
            hirsel_core_layout::Body::Record(f) => f.iter().collect::<Vec<_>>(),
            hirsel_core_layout::Body::Variant(c) => c.iter().flat_map(|c| &c.fields).collect(),
        };
        need(fields.iter().all(|f| refs_ok(&f.ty, m.types.len())), || {
            format!("t{i}: unknown type")
        })?;
        if let Some(f) = t.finalizer {
            // A finalizer receives the resource block's pointer (FAIL-11).
            let sig = m
                .functions
                .get(f.0 as usize)
                .and_then(|f| m.signatures.get(f.signature.index()));
            let want = Signature {
                params: vec![Param::Value(Ty::Ptr)],
                results: vec![],
                dest: None,
                never: false,
            };
            need(sig == Some(&want), || {
                format!("t{i}: finalizer is not (ptr) -> ()")
            })?;
            // An own row's finalizer is a core slot kernel's (crate docs, "Slot kernels").
            need(
                t.repr == hirsel_core_layout::Repr::Resource || core_fn(m, f.0),
                || format!("t{i}: only core declares a finalizer on a row that is not a resource"),
            )?;
            // It only moves slots, so nothing in it, or in what it calls, can fail. A
            // module that only names the row checks nothing here: the module holding the
            // finalizer's body does.
            if t.repr != hirsel_core_layout::Repr::Resource
                && m.functions[f.0 as usize].body.is_some()
            {
                let failures = failures.get_or_insert_with(|| crate::failure::Failures::new(m));
                need(!failures.may_fail[f.0 as usize], || {
                    format!("t{i}: an own row's finalizer may fail; it must only move slots")
                })?;
            }
        }
        if let Some(f) = t.clone {
            // A core own row's clone body: (from, to) -> (requested, available).
            let sig = m
                .functions
                .get(f.0 as usize)
                .and_then(|f| m.signatures.get(f.signature.index()));
            let want = Signature {
                params: vec![Param::Value(Ty::Ptr), Param::Value(Ty::Ptr)],
                results: vec![Ty::Scalar(Scalar::U64), Ty::Scalar(Scalar::U64)],
                dest: None,
                never: false,
            };
            need(sig == Some(&want), || {
                format!("t{i}: clone body is not (ptr, ptr) -> (u64, u64)")
            })?;
            need(
                t.repr == hirsel_core_layout::Repr::Own && core_fn(m, f.0),
                || format!("t{i}: only core declares a clone body, on an own row"),
            )?;
        }
    }
    hirsel_core_layout::compute(&m.types).map_err(|e| format!("layout: {e:?}"))?;
    for (i, s) in m.signatures.iter().enumerate() {
        let types_ok = s.params.iter().all(|p| match p {
            Param::Address {
                place: PlaceTy::Declared(ty),
                ..
            } => ty.index() < m.types.len(),
            Param::Address { .. } | Param::Value(_) => true,
        }) && s.dest.is_none_or(|t| t.index() < m.types.len());
        need(types_ok, || format!("g{i}: unknown type"))?;
        need(
            !s.never || (s.results.is_empty() && s.dest.is_none()),
            || format!("g{i}: a never signature has results"),
        )?;
    }
    for (i, f) in m.functions.iter().enumerate() {
        need(f.instance.index() < m.instances.len(), || {
            format!("f{i}: unknown instance")
        })?;
        need(f.signature.index() < m.signatures.len(), || {
            format!("f{i}: unknown signature")
        })?;
        if let Some(b) = &f.body {
            body(m, &m.signatures[f.signature.index()], b).map_err(|e| format!("f{i}: {e}"))?;
            need(
                core_fn(m, i as u32)
                    || !b
                        .insts
                        .iter()
                        .any(|x| matches!(x, Inst::Mark { .. } | Inst::Unmark { .. })),
                || format!("f{i}: only core's slot kernels emit `mark` and `unmark`"),
            )?;
        }
    }
    Ok(())
}

/// Whether function `f` is an instance of a core@1 definition.
fn core_fn(m: &Module, f: u32) -> bool {
    m.functions
        .get(f as usize)
        .and_then(|f| m.instances.get(f.instance.index()))
        .is_some_and(|k| k.definition.package_is("core", "1"))
}

/// The value types a signature's entry block receives.
pub fn entry_params(m: &Module, s: &Signature) -> Vec<Ty> {
    let _ = m;
    s.dest
        .map(|_| Ty::Ptr)
        .into_iter()
        .chain(s.params.iter().map(|p| match p {
            Param::Value(t) => *t,
            Param::Address { .. } => Ty::Ptr,
        }))
        .collect()
}

struct Ctx<'a> {
    m: &'a Module,
    b: &'a Body,
    defined: Vec<u32>,
    /// The entry values that are `view` or `take` parameters' pointers: places
    /// this body only reads (crate docs, "Loans").
    views: Vec<(ValueId, Loan)>,
    /// The address each `pointer` instruction's result was taken from.
    pointers: std::collections::HashMap<ValueId, AddressId>,
}

impl Ctx<'_> {
    fn ty(&self, v: ValueId) -> Result<Ty, String> {
        self.b
            .values
            .get(v.index())
            .copied()
            .ok_or_else(|| format!("v{} out of range", v.0))
    }
    fn def(&mut self, v: ValueId) -> R {
        self.ty(v)?;
        self.defined[v.index()] += 1;
        Ok(())
    }
    fn block(&self, b: BlockId) -> R {
        need(b.index() < self.b.blocks.len(), || {
            format!("b{} out of range", b.0)
        })
    }
    /// A failure block only where the operation can fail; without one a failure
    /// propagates out of the function with no cleanup in it.
    fn fail(&self, f: Option<BlockId>, can_fail: bool) -> R {
        need(f.is_none() || can_fail, || {
            "failure block on an operation that cannot fail".into()
        })?;
        if let Some(f) = f {
            self.block(f)?;
            need(self.b.blocks[f.index()].params.len == 0, || {
                format!("failure block b{} has parameters", f.0)
            })?;
        }
        Ok(())
    }
    fn vals(&self, r: PoolRange<ValueId>) -> Result<&[ValueId], String> {
        r.get(&self.b.pools.values)
            .ok_or_else(|| "value range out of pool".into())
    }
    fn edge(&self, e: EdgeId) -> R {
        let edge = self
            .b
            .edges
            .get(e.index())
            .ok_or_else(|| format!("edge {} out of range", e.0))?;
        self.block(edge.target)?;
        let params = self.vals(self.b.blocks[edge.target.index()].params)?;
        let args = self.vals(edge.args)?;
        need(params.len() == args.len(), || {
            format!("edge to b{}: arity", edge.target.0)
        })?;
        for (p, a) in params.iter().zip(args) {
            need(self.ty(*p)? == self.ty(*a)?, || {
                format!("edge to b{}: v{} has the wrong type", edge.target.0, a.0)
            })?;
        }
        Ok(())
    }
    fn tid(&self, t: TypeId) -> R {
        need(t.index() < self.m.types.len(), || {
            format!("t{} out of range", t.0)
        })
    }
    fn is(&self, v: ValueId, t: Ty) -> R {
        need(self.ty(v)? == t, || format!("v{} is not {t:?}", v.0))
    }
    /// The case count of the non-compact variant type the address names.
    fn cases(&self, a: AddressId) -> Result<usize, String> {
        let PlaceTy::Declared(t) = self.place(a)? else {
            return Err(format!("a{}: case of a scalar place", a.0));
        };
        let shape = self.m.types.get(t.index()).ok_or("unknown type")?;
        match (&shape.body, shape.repr) {
            (_, hirsel_core_layout::Repr::Compact(_)) => {
                Err(format!("a{}: compact variants are read through views", a.0))
            }
            (hirsel_core_layout::Body::Variant(c), _) => Ok(c.len()),
            _ => Err(format!("a{}: case of a record", a.0)),
        }
    }
    /// The place `a` names within its allocation: the root (a slot or an
    /// incoming pointer) and every step from it, following bases through
    /// addresses and `pointer` results. `None` past a `deref` step, which leaves
    /// for another allocation, or at a pointer of unknown origin.
    fn root(&self, mut a: AddressId) -> Option<(AddressBase, Vec<Step>)> {
        let mut path = Vec::new();
        loop {
            let info = self.b.addresses.get(a.index())?;
            let steps = info.steps.get(&self.b.pools.steps).unwrap_or_default();
            if steps.iter().any(|s| matches!(s, Step::Deref)) {
                return None;
            }
            path.splice(0..0, steps.iter().copied());
            a = match info.base {
                AddressBase::Address(x) => x,
                AddressBase::Value(v) if !self.views.iter().any(|&(w, _)| w == v) => {
                    match self.pointers.get(&v) {
                        Some(&x) => x,
                        None => return Some((info.base, path)),
                    }
                }
                base => return Some((base, path)),
            };
        }
    }
    /// A write through `a`: never into this body's own `view` or `take`
    /// parameter's place. A callee consumes a `take` place by reading it into its
    /// own slot or by dropping it in place, and writes it no other way.
    fn writes(&self, a: AddressId) -> R {
        let loan = match self.root(a) {
            Some((AddressBase::Value(v), _)) => {
                self.views.iter().find(|&&(w, _)| w == v).map(|&(_, l)| l)
            }
            _ => None,
        };
        need(loan.is_none(), || {
            format!(
                "a{}: writes a {} parameter's place",
                a.0,
                crate::text::loan_word(loan.unwrap())
            )
        })
    }
    /// A drop through `a`: the callee may consume its own `take` place in place,
    /// but never drops a `view` place.
    fn drops(&self, a: AddressId) -> R {
        match self.root(a) {
            Some((AddressBase::Value(v), _))
                if self.views.iter().any(|&(w, l)| w == v && l == Loan::Take) =>
            {
                Ok(())
            }
            _ => self.writes(a),
        }
    }
    /// Whether `a` and `b` provably share bytes: one is the other or lies
    /// inside it (same root, one path a prefix of the other).
    fn overlaps(&self, a: AddressId, b: AddressId) -> bool {
        match (self.root(a), self.root(b)) {
            (Some((x, p)), Some((y, q))) => x == y && (p.starts_with(&q) || q.starts_with(&p)),
            _ => false,
        }
    }
    fn place(&self, a: AddressId) -> Result<PlaceTy, String> {
        self.b
            .addresses
            .get(a.index())
            .map(|a| a.ty)
            .ok_or_else(|| format!("a{} out of range", a.0))
    }
}

pub fn body(m: &Module, sig: &Signature, b: &Body) -> R {
    let entry_values = b
        .blocks
        .first()
        .and_then(|k| k.params.get(&b.pools.values))
        .unwrap_or_default();
    let skip = usize::from(sig.dest.is_some());
    let views = sig
        .params
        .iter()
        .zip(entry_values.iter().skip(skip))
        .filter_map(|(p, v)| match p {
            Param::Address {
                loan: loan @ (Loan::View | Loan::Take),
                ..
            } => Some((*v, *loan)),
            _ => None,
        })
        .collect();
    let pointers = b
        .insts
        .iter()
        .filter_map(|i| match *i {
            Inst::Pointer { result, address } => Some((result, address)),
            _ => None,
        })
        .collect();
    let mut c = Ctx {
        m,
        b,
        defined: vec![0; b.values.len()],
        views,
        pointers,
    };
    need(!b.blocks.is_empty(), || "no entry block".into())?;
    need(b.terminators.len() == b.blocks.len(), || {
        "terminators not parallel to blocks".into()
    })?;
    need(b.inst_anchors.len() == b.insts.len(), || {
        "anchors not parallel to instructions".into()
    })?;
    let anchors = b
        .inst_anchors
        .iter()
        .chain(b.blocks.iter().map(|k| &k.anchor))
        .flatten();
    for a in anchors {
        need(a.item.index() < m.items.len() && a.start <= a.end, || {
            "bad anchor".into()
        })?;
    }
    for t in &b.slots {
        if let PlaceTy::Declared(t) = t {
            need(t.index() < m.types.len(), || "slot of unknown type".into())?;
        }
    }
    for (i, a) in b.addresses.iter().enumerate() {
        match a.base {
            AddressBase::Slot(s) => {
                need(s.index() < b.slots.len(), || format!("a{i}: unknown slot"))?
            }
            AddressBase::Value(v) => need(c.ty(v)? == Ty::Ptr, || {
                format!("a{i}: base is not a pointer")
            })?,
            AddressBase::Address(x) => need(x.index() < i, || {
                format!("a{i}: base address must come first")
            })?,
        }
        for s in a.steps.get(&b.pools.steps).ok_or("steps out of pool")? {
            if let Step::Index(v) | Step::Element(v) | Step::Byte(v) = s {
                need(c.ty(*v)?.int().is_some(), || {
                    format!("a{i}: index is not an integer")
                })?;
            }
        }
    }
    // Entry parameters follow the signature.
    let entry: Vec<Ty> = c
        .vals(b.blocks[0].params)?
        .iter()
        .map(|v| c.ty(*v))
        .collect::<Result<_, _>>()?;
    need(entry == entry_params(m, sig), || {
        "entry parameters differ from the signature".into()
    })?;
    let mut next = 0u32;
    for (bi, block) in b.blocks.iter().enumerate() {
        need(block.insts.start == next, || {
            format!("b{bi}: instructions not contiguous")
        })?;
        next += block.insts.len;
        for v in c.vals(block.params)?.to_vec() {
            c.def(v)?;
        }
        for inst in block
            .insts
            .get(&b.insts)
            .ok_or("instructions out of range")?
        {
            inst_wf(&mut c, inst).map_err(|e| format!("b{bi}: {e}"))?;
        }
        // A call of a `never` signature is its block's last instruction, and only
        // `unreachable` follows it. `unreachable` also ends a block the front proves
        // unreached by a type: a read of a binder whose type has no values (FORM #31a).
        let insts = block.insts.get(&b.insts).unwrap_or_default();
        let never_at = insts.iter().position(|i| match i {
            Inst::Call { call } => b
                .calls
                .get(call.index())
                .and_then(|s| m.signatures.get(s.signature.index()))
                .is_some_and(|s| s.never),
            _ => false,
        });
        let unreachable = matches!(b.terminators[bi], Terminator::Unreachable);
        need(
            never_at.is_none_or(|i| i + 1 == insts.len() && unreachable),
            || format!("b{bi}: a never call must end its block, before unreachable"),
        )?;
        match &b.terminators[bi] {
            Terminator::Jump(e) => c.edge(*e)?,
            Terminator::Branch { condition, yes, no } => {
                need(c.ty(*condition)? == Ty::Scalar(Scalar::Bool), || {
                    format!("b{bi}: branch on a non-bool")
                })?;
                c.edge(*yes)?;
                c.edge(*no)?;
            }
            Terminator::Switch {
                value,
                arms,
                otherwise,
            } => {
                need(c.ty(*value)?.int().is_some(), || {
                    format!("b{bi}: switch on a non-integer")
                })?;
                let arms = arms.get(&b.pools.arms).ok_or("arms out of pool")?;
                for (k, a) in arms.iter().enumerate() {
                    need(arms[..k].iter().all(|x| x.value != a.value), || {
                        format!("b{bi}: duplicate arm")
                    })?;
                    c.edge(a.edge)?;
                }
                c.edge(*otherwise)?;
            }
            Terminator::Raise { cause, cleanup } => {
                c.fail(*cleanup, true)?;
                if let RaiseCause::Fail(a) = cause {
                    c.place(*a)?;
                }
            }
            Terminator::Propagate | Terminator::Unreachable => {}
            Terminator::Return(r) => {
                need(!sig.never, || format!("b{bi}: a never function returns"))?;
                let types: Vec<Ty> = c
                    .vals(*r)?
                    .iter()
                    .map(|v| c.ty(*v))
                    .collect::<Result<_, _>>()?;
                need(types == sig.results, || {
                    format!("b{bi}: return differs from the signature")
                })?;
            }
        }
    }
    need(next as usize == b.insts.len(), || {
        "instructions outside every block".into()
    })?;
    if let Some(v) = c.defined.iter().position(|n| *n != 1) {
        return Err(format!("v{v} defined {} times", c.defined[v]));
    }
    no_catch(b)
}

/// FAIL-14: no block reachable from a failure block returns normally.
fn no_catch(b: &Body) -> R {
    let succ = |bi: usize| -> Vec<BlockId> {
        let blk = &b.blocks[bi];
        let mut out: Vec<BlockId> = blk
            .insts
            .get(&b.insts)
            .unwrap_or_default()
            .iter()
            .filter_map(|i| i.fail(b))
            .collect();
        let edge = |e: &EdgeId| b.edges[e.index()].target;
        match &b.terminators[bi] {
            Terminator::Jump(e) => out.push(edge(e)),
            Terminator::Branch { yes, no, .. } => out.extend([edge(yes), edge(no)]),
            Terminator::Switch {
                arms, otherwise, ..
            } => {
                out.extend(
                    arms.get(&b.pools.arms)
                        .unwrap_or_default()
                        .iter()
                        .map(|a| edge(&a.edge)),
                );
                out.push(edge(otherwise));
            }
            Terminator::Raise { cleanup, .. } => out.extend(*cleanup),
            Terminator::Return(_) | Terminator::Propagate | Terminator::Unreachable => {}
        }
        out
    };
    let mut seen = vec![false; b.blocks.len()];
    let mut work: Vec<BlockId> = (0..b.blocks.len())
        .flat_map(|bi| {
            let raise = match b.terminators[bi] {
                Terminator::Raise { cleanup, .. } => cleanup,
                _ => None,
            };
            let insts = b.blocks[bi].insts.get(&b.insts).unwrap_or_default();
            insts
                .iter()
                .filter_map(|i| i.fail(b))
                .chain(raise)
                .collect::<Vec<_>>()
        })
        .collect();
    while let Some(k) = work.pop() {
        if std::mem::replace(&mut seen[k.index()], true) {
            continue;
        }
        need(
            !matches!(b.terminators[k.index()], Terminator::Return(_)),
            || format!("failure path reaches the return of b{}", k.0),
        )?;
        work.extend(succ(k.index()));
    }
    Ok(())
}

fn inst_wf(c: &mut Ctx, inst: &Inst) -> R {
    let bool_ty = Ty::Scalar(Scalar::Bool);
    match *inst {
        Inst::Const { result, bits } => {
            c.def(result)?;
            let fits = match c.ty(result)? {
                Ty::Scalar(Scalar::Bool) => bits <= 1,
                Ty::Scalar(Scalar::Char) => {
                    u32::try_from(bits).ok().and_then(char::from_u32).is_some()
                }
                Ty::Scalar(Scalar::F32) => bits <= u64::from(u32::MAX),
                t => t.int().is_none_or(|(_, w)| w == 64 || bits >> w == 0),
            };
            need(fits, || {
                format!("const v{} does not fit its type", result.0)
            })
        }
        Inst::ReadStatic { result, data } => {
            c.def(result)?;
            need(
                data.index() < c.m.statics.len() && c.ty(result)? == Ty::Ptr,
                || "bad static read".into(),
            )
        }
        Inst::FunctionRef { result, function } => {
            c.def(result)?;
            need(
                function.index() < c.m.functions.len() && c.ty(result)? == Ty::Ptr,
                || "bad function reference".into(),
            )
        }
        Inst::Unary {
            result,
            op,
            operand,
            fail,
        } => {
            c.def(result)?;
            let t = c.ty(operand)?;
            need(t == c.ty(result)?, || "unary result type".into())?;
            let ok = match op {
                UnaryOp::Not => t == bool_ty,
                UnaryOp::BitNot => t.int().is_some(),
                UnaryOp::Neg => t.is_float() || t.int().is_some_and(|(signed, _)| signed),
            };
            need(ok, || format!("{op:?} on {t:?}"))?;
            c.fail(fail, op == UnaryOp::Neg && t.int().is_some())
        }
        Inst::Binary {
            result,
            op,
            left,
            right,
            fail,
        } => {
            c.def(result)?;
            let t = c.ty(left)?;
            need(t == c.ty(right)? && t == c.ty(result)?, || {
                "binary operand types".into()
            })?;
            let ok = match op {
                BinaryOp::BitAnd | BinaryOp::BitOr | BinaryOp::BitXor => {
                    t.int().is_some() || t == bool_ty
                }
                BinaryOp::MinNumber | BinaryOp::MaxNumber => t.is_float(),
                BinaryOp::Min
                | BinaryOp::Max
                | BinaryOp::Add
                | BinaryOp::Sub
                | BinaryOp::Mul
                | BinaryOp::Div => t.int().is_some() || t.is_float(),
                _ => t.int().is_some(),
            };
            need(ok, || format!("{op:?} on {t:?}"))?;
            c.fail(fail, op.can_fail(t))
        }
        Inst::Compare {
            result,
            op,
            left,
            right,
        } => {
            c.def(result)?;
            let t = c.ty(left)?;
            need(t == c.ty(right)? && c.ty(result)? == bool_ty, || {
                "compare types".into()
            })?;
            need(
                t != Ty::Ptr || matches!(op, CompareOp::Eq | CompareOp::Ne),
                || "ordered pointer compare".into(),
            )
        }
        Inst::Convert {
            result,
            flag,
            operand,
            mode,
        } => {
            c.def(result)?;
            if let Some(f) = flag {
                c.def(f)?;
            }
            let (from, to) = (c.ty(operand)?, c.ty(result)?);
            let flag_ty = flag.map(|f| c.ty(f)).transpose()?;
            let (char_ty, u32_ty) = (Ty::Scalar(Scalar::Char), Ty::Scalar(Scalar::U32));
            let ok = match mode {
                // A char converts to and from u32 only (`char.to_u32`, `char.from_u32`).
                _ if from == char_ty || to == char_ty => match mode {
                    ConvertMode::Widen => (from, to, flag_ty) == (char_ty, u32_ty, None),
                    ConvertMode::TryNarrow => {
                        (from, to, flag_ty) == (u32_ty, char_ty, Some(bool_ty))
                    }
                    _ => false,
                },
                ConvertMode::Widen | ConvertMode::Truncate => match (from.int(), to.int(), flag_ty)
                {
                    (Some((fs, a)), Some((ts, b)), None) if mode == ConvertMode::Widen => {
                        (fs == ts && a <= b) || (!fs && ts && a < b)
                    }
                    (Some(_), Some((_, b)), None) => b <= from.int().map_or(0, |(_, a)| a),
                    _ => false,
                },
                ConvertMode::TryNarrow => {
                    from.int().is_some() && to.int().is_some() && flag_ty == Some(bool_ty)
                }
                ConvertMode::FloatToInt => {
                    from.is_float() && to.int().is_some() && flag_ty == Some(Ty::Scalar(Scalar::U8))
                }
                ConvertMode::IntToFloat => {
                    from.int().is_some() && to.is_float() && flag_ty.is_none()
                }
                ConvertMode::FloatResize => {
                    from.is_float() && to.is_float() && from != to && flag_ty.is_none()
                }
                ConvertMode::Reinterpret => {
                    let pair = [(Scalar::F32, Scalar::U32), (Scalar::F64, Scalar::U64)];
                    flag_ty.is_none()
                        && pair.iter().any(|&(f, u)| {
                            (from, to) == (Ty::Scalar(f), Ty::Scalar(u))
                                || (from, to) == (Ty::Scalar(u), Ty::Scalar(f))
                        })
                }
            };
            need(ok, || format!("{mode:?} from {from:?} to {to:?}"))
        }
        Inst::IntArith {
            result,
            flag,
            op,
            mode,
            left,
            right,
        } => {
            need(op != ArithOp::Div || mode == ArithMode::Checked, || {
                "div is checked only".into()
            })?;
            c.def(result)?;
            if let Some(f) = flag {
                c.def(f)?;
                need(c.ty(f)? == bool_ty, || "checked flag is not bool".into())?;
            }
            let t = c.ty(left)?;
            need(
                t.int().is_some() && t == c.ty(right)? && t == c.ty(result)?,
                || "int arith types".into(),
            )?;
            need(flag.is_some() == (mode == ArithMode::Checked), || {
                "flag iff checked".into()
            })
        }
        Inst::MulAdd {
            result,
            left,
            right,
            addend,
        } => {
            c.def(result)?;
            let t = c.ty(result)?;
            need(
                t.is_float() && [left, right, addend].iter().all(|v| c.ty(*v) == Ok(t)),
                || "mul_add types".into(),
            )
        }
        Inst::ScalarUnary {
            result,
            op,
            operand,
        } => {
            c.def(result)?;
            let t = c.ty(operand)?;
            let kind = if op.is_float() {
                t.is_float()
            } else {
                t.int().is_some()
            };
            need(kind && t == c.ty(result)?, || format!("{op:?} on {t:?}"))
        }
        Inst::Address { address, fail } => {
            c.place(address)?;
            c.fail(fail, inst.can_fail(c.b))
        }
        Inst::Read {
            result,
            address,
            mode: _,
        } => {
            c.def(result)?;
            need(c.place(address)? == PlaceTy::Value(c.ty(result)?), || {
                "read type".into()
            })
        }
        Inst::Store { address, value } => {
            c.writes(address)?;
            need(c.place(address)? == PlaceTy::Value(c.ty(value)?), || {
                "store type".into()
            })
        }
        Inst::StorageLive { slot } | Inst::StorageDead { slot } => {
            need(slot.index() < c.b.slots.len(), || "unknown slot".into())
        }
        Inst::UpdateMask {
            result,
            mask,
            field,
            initialized: _,
        } => {
            c.def(result)?;
            let t = c.ty(mask)?;
            let width = t.int().map_or(0, |(_, w)| w);
            need(t == c.ty(result)? && field < width, || {
                "mask field outside its word".into()
            })
        }
        Inst::Alloc {
            result,
            available,
            region_entry,
            ref kind,
            place,
            charge,
            overflow,
            size,
            align,
            fail,
        } => {
            c.def(result)?;
            c.def(available)?;
            if let Some(token) = region_entry {
                need(matches!(place, AllocPlace::Region(_)), || {
                    "payer-entry output requires a region allocation".into()
                })?;
                c.def(token)?;
                c.is(token, Ty::Scalar(Scalar::U64))?;
            }
            c.is(size, Ty::Scalar(Scalar::U64))?;
            c.is(result, Ty::Ptr)?;
            c.is(available, Ty::Scalar(Scalar::U64))?;
            c.is(charge, Ty::Scalar(Scalar::U64))?;
            if let Some(flag) = overflow {
                need(!matches!(kind, AllocKind::Resource { .. }), || {
                    "resource allocation has a fixed charge".into()
                })?;
                c.is(flag, Ty::Scalar(Scalar::Bool))?;
            }
            if let AllocPlace::Like(v) | AllocPlace::Region(v) = place {
                c.is(v, Ty::Ptr)?;
            }
            // Region storage is stable and stable storage is in a region (crate docs,
            // "Cleanup classes"); an own block names the edge that will own it.
            need(
                matches!(kind, AllocKind::Stable) == matches!(place, AllocPlace::Region(_)),
                || "stable storage is exactly the storage allocated in a region".into(),
            )?;
            if let AllocKind::Own { edge } = kind {
                use hirsel_core_layout::FieldTy as F;
                let types = &c.m.types;
                need(refs_ok(edge, types.len()), || {
                    format!("allocation edge {edge:?} names an unknown type")
                })?;
                need(
                    matches!(
                        edge,
                        F::Box(_) | F::Rc(_) | F::Arc(_) | F::Seq(_) | F::Map(..) | F::OwnFn
                    ),
                    || format!("allocation edge {edge:?} is not an own block's edge"),
                )?;
            }
            need(align.is_power_of_two(), || {
                "alignment is not a power of two".into()
            })?;
            c.fail(fail, true)
        }
        Inst::Grow {
            result,
            available,
            carrier,
            charge,
            overflow,
            size,
            align,
            capacity,
            moved,
            refund,
            fail,
        } => {
            c.def(result)?;
            c.def(available)?;
            c.is(result, Ty::Ptr)?;
            c.is(available, Ty::Scalar(Scalar::U64))?;
            for v in [size, charge, capacity, moved, refund] {
                c.is(v, Ty::Scalar(Scalar::U64))?;
            }
            if let Some(flag) = overflow {
                c.is(flag, Ty::Scalar(Scalar::Bool))?;
            }
            // The carrier is written on success, so never a view or take place, and
            // names the `seq` edge that owns both blocks (crate docs, "Growth").
            c.writes(carrier)?;
            let place = c.place(carrier)?;
            need(sequence_edge(&c.m.types, place).is_some(), || {
                format!(
                    "a{}: grow of a place that is not a sequence carrier",
                    carrier.0
                )
            })?;
            need(align.is_power_of_two(), || {
                "alignment is not a power of two".into()
            })?;
            c.fail(fail, true)
        }
        Inst::Free { block, charge } => {
            c.is(block, Ty::Ptr)?;
            c.is(charge, Ty::Scalar(Scalar::U64))
        }
        Inst::Copy { to, from, bytes } => {
            c.writes(to)?;
            c.place(to)?;
            c.place(from)?;
            c.is(bytes, Ty::Scalar(Scalar::U64))
        }
        Inst::Case { result, address } => {
            c.def(result)?;
            c.is(result, Ty::Scalar(Scalar::U32))?;
            c.cases(address).map(|_| ())
        }
        Inst::SetCase { address, case } => {
            c.writes(address)?;
            let n = c.cases(address)?;
            need((case as usize) < n, || {
                format!("set_case {case} of {n} cases")
            })
        }
        Inst::RegionOpen { result } => {
            c.def(result)?;
            c.is(result, Ty::Ptr)
        }
        Inst::RegionClose { region } => c.is(region, Ty::Ptr),
        Inst::Trim {
            block,
            region,
            entry,
            previous,
            charge,
        } => {
            c.is(block, Ty::Ptr)?;
            c.is(region, Ty::Ptr)?;
            c.is(entry, Ty::Scalar(Scalar::U64))?;
            c.is(previous, Ty::Scalar(Scalar::U64))?;
            c.is(charge, Ty::Scalar(Scalar::U64))
        }
        Inst::Pointer { result, address } => {
            c.def(result)?;
            c.is(result, Ty::Ptr)?;
            c.place(address).map(|_| ())
        }
        Inst::LimitEnter {
            admitted,
            available,
            bytes,
        } => {
            c.def(admitted)?;
            c.def(available)?;
            c.is(admitted, bool_ty)?;
            c.is(available, Ty::Scalar(Scalar::U64))?;
            c.is(bytes, Ty::Scalar(Scalar::U64))
        }
        Inst::LimitExit => Ok(()),
        Inst::Mark {
            address,
            count,
            live: _,
            ty,
        }
        | Inst::Unmark {
            address,
            count,
            live: _,
            ty,
        } => {
            c.is(count, Ty::Scalar(Scalar::U64))?;
            if let PlaceTy::Declared(t) = ty {
                c.tid(t)?;
            }
            need(c.place(address)? == ty, || {
                "mark of a place of another type".into()
            })
        }
        Inst::Drop { address, ty, fail } => {
            c.drops(address)?;
            c.tid(ty)?;
            need(c.place(address)? == PlaceTy::Declared(ty), || {
                "drop of a place of another type".into()
            })?;
            c.fail(fail, true)
        }
        Inst::Share { node, kind: _ } => c.is(node, Ty::Ptr),
        Inst::Release { node, kind: _, ty } => {
            c.tid(ty)?;
            c.is(node, Ty::Ptr)
        }
        Inst::TryUnwrap {
            result,
            node,
            kind: _,
        } => {
            c.def(result)?;
            c.is(result, bool_ty)?;
            c.is(node, Ty::Ptr)
        }
        Inst::Call { call } => {
            let site = c.b.calls.get(call.index()).ok_or("call out of range")?;
            let sig =
                c.m.signatures
                    .get(site.signature.index())
                    .ok_or("unknown signature")?;
            c.fail(
                site.fail,
                !matches!(site.callee, Callee::Runtime(op) if !op.fails()),
            )?;
            match site.callee {
                Callee::Direct(f) => {
                    let f = c.m.functions.get(f.index()).ok_or("unknown callee")?;
                    need(f.signature == site.signature, || {
                        "direct call through another signature".into()
                    })?;
                }
                Callee::Indirect(v) => need(c.ty(v)? == Ty::Ptr, || {
                    "indirect callee is not a pointer".into()
                })?,
                Callee::Runtime(op) => {
                    for t in op.types() {
                        c.tid(t)?;
                    }
                    need(*sig == op.signature(), || {
                        format!("{op:?} through another signature")
                    })?;
                }
            }
            let args = site.args.get(&c.b.pools.args).ok_or("args out of pool")?;
            // The destination travels as the first argument, an address of type `dest`.
            need(args.len() == entry_params(c.m, sig).len(), || {
                "call arity".into()
            })?;
            // The destination is written, and a `mut` place may be.
            let params = sig
                .dest
                .map(|ty| Param::Address {
                    place: PlaceTy::Declared(ty),
                    loan: Loan::Mut,
                })
                .into_iter()
                .chain(sig.params.iter().copied());
            for (i, (a, p)) in args.iter().zip(params).enumerate() {
                let ok = match (a, p) {
                    (Arg::Value(v), Param::Value(t)) => c.ty(*v)? == t,
                    (Arg::Address(a), Param::Address { place, loan }) => {
                        if loan == Loan::Mut {
                            c.writes(*a)?;
                            for (j, b) in args.iter().enumerate() {
                                if let Arg::Address(b) = b {
                                    need(i == j || !c.overlaps(*a, *b), || {
                                        format!(
                                            "a{}: a mut place overlaps another argument a{}",
                                            a.0, b.0
                                        )
                                    })?;
                                }
                            }
                        }
                        c.place(*a)? == place
                    }
                    _ => false,
                };
                need(ok, || "call argument differs from the signature".into())?;
            }
            let results = c.vals(site.results)?.to_vec();
            for v in &results {
                c.def(*v)?;
            }
            let types: Vec<Ty> = results.iter().map(|v| c.ty(*v)).collect::<Result<_, _>>()?;
            need(types == sig.results, || {
                "call results differ from the signature".into()
            })
        }
    }
}

/// `m` with every `mark` and `unmark` removed (FORM v1c). Neither has a run-time
/// effect, so an engine must emit the same code for both; engines' tests compare them.
pub fn without_marks(m: &Module) -> Module {
    let mut m = m.clone();
    for b in m.functions.iter_mut().filter_map(|f| f.body.as_mut()) {
        let (mut insts, mut anchors) = (Vec::new(), Vec::new());
        for block in &mut b.blocks {
            let start = insts.len() as u32;
            for i in block.insts.start..block.insts.start + block.insts.len {
                if !matches!(b.insts[i as usize], Inst::Mark { .. } | Inst::Unmark { .. }) {
                    insts.push(b.insts[i as usize].clone());
                    anchors.push(b.inst_anchors[i as usize]);
                }
            }
            block.insts = PoolRange::new(start, insts.len() as u32 - start);
        }
        (b.insts, b.inst_anchors) = (insts, anchors);
    }
    m
}
