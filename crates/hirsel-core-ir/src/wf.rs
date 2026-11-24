//! Structural well-formedness, for tests only (feature `wf`): producers' tests call
//! it on what they built; no product path does (we trust our own implementation).
//! It checks references, single definition, arity, the operand typing each
//! instruction states, that type rows have recipes, and that no failure path
//! reaches a `return` (FAIL-14: nothing catches a failure), that every value
//! and address is defined on every path to each use, and that every read of a
//! slot's contents follows a write on every path (a drop flag counting as one). It
//! does not check limit-scope balance.

use crate::*;

mod frame;
mod own;
mod places;

type R = Result<(), String>;

fn need(ok: bool, msg: impl FnOnce() -> String) -> R {
    if ok { Ok(()) } else { Err(msg()) }
}

fn refs_ok(f: &hirsel_core_layout::FieldTy, n: usize) -> bool {
    use hirsel_core_layout::FieldTy as F;
    match f {
        F::Inline(t) | F::Box(t) | F::Rc(t) | F::Arc(t) | F::RegionRef(t) | F::Link(t) => {
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
    let mut closed = std::collections::HashMap::new();
    let mut duplicates = Vec::new();
    for (row, shape) in m
        .types
        .iter()
        .enumerate()
        .filter(|(_, s)| !s.name.contains('$'))
    {
        if let Some(first) = closed.insert(&shape.name, row) {
            duplicates.push(format!("t{first}/t{row} {}", shape.name));
        }
    }
    need(duplicates.is_empty(), || {
        format!("duplicate closed type rows: {}", duplicates.join("; "))
    })?;
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
            // A core own row's clone body: (from, to) -> (); a refusal raises.
            let sig = m
                .functions
                .get(f.0 as usize)
                .and_then(|f| m.signatures.get(f.signature.index()));
            let want = Signature {
                params: vec![Param::Value(Ty::Ptr), Param::Value(Ty::Ptr)],
                results: vec![],
                dest: None,
                never: false,
            };
            need(sig == Some(&want), || {
                format!("t{i}: clone body is not (ptr, ptr) -> ()")
            })?;
            need(
                t.repr == hirsel_core_layout::Repr::Own && core_fn(m, f.0),
                || format!("t{i}: only core declares a clone body, on an own row"),
            )?;
        }
        for f in [t.codec, t.key_codec].into_iter().flatten() {
            // A codec type's maker (SEM-10): `() -> Codec[T]`, its result a declared row.
            let sig = m
                .functions
                .get(f.0 as usize)
                .and_then(|f| m.signatures.get(f.signature.index()));
            need(
                sig.is_some_and(|s| {
                    s.params.is_empty() && s.results.is_empty() && s.dest.is_some()
                }),
                || format!("t{i}: codec maker must return its codec and take no arguments"),
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
        // `std.codec.of` is resolved by its type argument to that type's codec maker
        // before any instance is built: only a template's rows name it, at a rigid
        // argument (`$i`) or as its own body's call (crate docs, "Instances").
        let k = &m.instances[f.instance.index()];
        let closed = !k.arguments.is_empty()
            && k.arguments.iter().all(|t| {
                m.types
                    .get(t.index())
                    .is_some_and(|t| !t.name.contains('$'))
            });
        let of = k.definition.package_is("std", "1")
            && k.definition.0.modules == ["codec"]
            && k.definition.0.name == "of";
        need(!(of && closed), || {
            format!("f{i}: `std.codec.of` is never an instance")
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
            // SITES.md 6(b): a body that does not inherit its caller's site gets none,
            // so a failure point it anchors in core would report no site where the
            // evaluator's walk found one. Producers anchor such a point in the body's
            // own item (front-inlined core code takes the call's anchor).
            if !crate::source::inherits_site(&m.instances[f.instance.index()]) {
                let in_core = |a: &Option<Anchor>| {
                    a.is_some_and(|a| m.items[a.item.index()].package_is("core", "1"))
                };
                let insts = b.insts.iter().zip(&b.inst_anchors);
                let raises = b.blocks.iter().zip(&b.terminators);
                need(
                    !insts.into_iter().any(|(x, a)| x.can_fail(b) && in_core(a))
                        && !raises.into_iter().any(|(k, t)| {
                            matches!(t, Terminator::Raise { .. }) && in_core(&k.anchor)
                        }),
                    || {
                        format!(
                            "f{i}: a failure point anchored in core in a body that does not \
                             inherit its caller's site (SITES 6(b))"
                        )
                    },
                )?;
            }
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
    let mut ats = Vec::with_capacity(b.addresses.len());
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
        places::address(&c, &mut ats, i, a)?;
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
                match cause {
                    RaiseCause::Fail(a) => {
                        c.place(*a)?;
                    }
                    RaiseCause::Capacity {
                        requested,
                        available,
                    } => {
                        c.is(*requested, Ty::Scalar(Scalar::U64))?;
                        c.is(*available, Ty::Scalar(Scalar::U64))?;
                    }
                    RaiseCause::Unwrap | RaiseCause::Bounds => {}
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
    no_catch(b)?;
    available(b)?;
    frame::obligations(m, b)?;
    written(m, b)?;
    own::check(m, sig, b)?;
    ended(b)
}

/// No slot is used where it may have ended: on every path to a use of a slot (an
/// address based on it, or a pointer this body made from one), its last
/// `StorageLive` or `StorageDead` is not a `StorageDead` (crate docs, "Values,
/// slots and addresses").
fn ended(b: &Body) -> R {
    match crate::extent::use_after_end(b) {
        Some(((bi, _), s)) => Err(format!("b{bi}: s{s} is used after its storage ends")),
        None => Ok(()),
    }
}

/// Every value and address is defined on every path to each of its uses: an
/// instruction result or block parameter before a value is read, an `address`
/// instruction before its address is (crate docs, "Values, slots and addresses").
/// A failure edge leaves before its instruction, so nothing that instruction
/// defines reaches its failure block. A block the entry does not reach is never
/// run, and is not checked.
fn available(b: &Body) -> R {
    let names = b.values.len() + b.addresses.len();
    let words = names.div_ceil(64);
    let blocks = b.blocks.len();
    type Set = Vec<u64>;
    let set = |s: &mut Set, k: usize| s[k / 64] |= 1 << (k % 64);
    let has = |s: &Set, k: usize| s[k / 64] >> (k % 64) & 1 == 1;
    let operands: Vec<Vec<(Vec<usize>, Vec<usize>)>> = b
        .blocks
        .iter()
        .map(|k| {
            k.insts
                .get(&b.insts)
                .unwrap_or_default()
                .iter()
                .map(|i| crate::extent::operands(b, i))
                .collect()
        })
        .collect();
    let params = |bi: usize| -> Vec<usize> {
        b.blocks[bi]
            .params
            .get(&b.pools.values)
            .unwrap_or_default()
            .iter()
            .map(|v| v.index())
            .collect()
    };
    // The edges out of `bi` from the state at its start: each target with the
    // names defined on that edge.
    let out = |bi: usize, start: &Set| -> Vec<(usize, Set)> {
        let mut now = start.clone();
        for p in params(bi) {
            set(&mut now, p);
        }
        let mut edges = vec![];
        let insts = b.blocks[bi].insts.get(&b.insts).unwrap_or_default();
        for (k, i) in insts.iter().enumerate() {
            if let Some(f) = i.fail(b) {
                edges.push((f.index(), now.clone()));
            }
            for &d in &operands[bi][k].1 {
                set(&mut now, d);
            }
        }
        let edge = |e: &EdgeId| b.edges[e.index()].target.index();
        let mut tail = |t: usize| edges.push((t, now.clone()));
        match &b.terminators[bi] {
            Terminator::Jump(e) => tail(edge(e)),
            Terminator::Branch { yes, no, .. } => {
                tail(edge(yes));
                tail(edge(no));
            }
            Terminator::Switch {
                arms, otherwise, ..
            } => {
                for arm in arms.get(&b.pools.arms).unwrap_or_default() {
                    tail(edge(&arm.edge));
                }
                tail(edge(otherwise));
            }
            Terminator::Raise { cleanup, .. } => {
                if let Some(c) = cleanup {
                    tail(c.index());
                }
            }
            Terminator::Return(_) | Terminator::Propagate | Terminator::Unreachable => {}
        }
        edges
    };
    // Must-availability: a block starts with what every edge into it carries.
    let mut start: Vec<Option<Set>> = vec![None; blocks];
    if blocks > 0 {
        start[0] = Some(vec![0; words]);
    }
    let mut work = vec![0usize];
    while let Some(bi) = work.pop() {
        let Some(s) = start[bi].clone() else { continue };
        for (t, carried) in out(bi, &s) {
            let next = match &start[t] {
                None => carried,
                Some(old) => old.iter().zip(&carried).map(|(x, y)| x & y).collect(),
            };
            if t == 0 {
                // The entry is entered once, with nothing defined.
                continue;
            }
            if start[t].as_ref() != Some(&next) {
                start[t] = Some(next);
                work.push(t);
            }
        }
    }
    let name = |k: usize| {
        if k < b.values.len() {
            format!("v{k}")
        } else {
            format!("a{}", k - b.values.len())
        }
    };
    for bi in 0..blocks {
        let Some(mut now) = start[bi].clone() else {
            continue;
        };
        for p in params(bi) {
            set(&mut now, p);
        }
        let check = |now: &Set, k: usize| {
            need(has(now, k), || {
                format!(
                    "b{bi}: {} is used on a path that does not define it",
                    name(k)
                )
            })
        };
        for (uses, defs) in &operands[bi] {
            for &u in uses {
                check(&now, u)?;
            }
            for &d in defs {
                set(&mut now, d);
            }
        }
        let tail = crate::extent::terminator_uses(b, bi);
        for u in tail {
            check(&now, u)?;
        }
    }
    Ok(())
}

/// Whether block `bi` opens with `rt.isolate_catch`: a handler (crate docs,
/// "Isolation").
fn handler(b: &Body, bi: usize) -> bool {
    let insts = b.blocks[bi].insts.get(&b.insts).unwrap_or_default();
    insts.iter().position(|i| catch(b, i)) == Some(0)
}
fn catch(b: &Body, i: &Inst) -> bool {
    matches!(*i, Inst::Call { call } if b.calls[call.index()].callee == Callee::Runtime(RtOp::IsolateCatch))
}

fn step_catch(b: &Body, bi: usize) -> Option<ValueId> {
    let Inst::Call { call } = *b.blocks[bi].insts.get(&b.insts)?.last()? else {
        return None;
    };
    let site = &b.calls[call.index()];
    (site.callee == Callee::Runtime(RtOp::StepCatch))
        .then(|| {
            site.results
                .get(&b.pools.values)
                .and_then(|v| v.first())
                .copied()
        })
        .flatten()
}

/// FAIL-14, FAIL-16: no block reachable from a failure block returns normally,
/// except past a handler, whose catch ends the failure. A catch opens its block,
/// and only failure edges enter that block.
fn no_catch(b: &Body) -> R {
    for bi in 0..b.blocks.len() {
        let insts = b.blocks[bi].insts.get(&b.insts).unwrap_or_default();
        need(insts.iter().skip(1).all(|i| !catch(b, i)), || {
            format!("b{bi}: rt.isolate_catch is not the block's first instruction")
        })?;
    }
    for e in &b.edges {
        need(!handler(b, e.target.index()), || {
            format!("b{}: a handler entered by an edge", e.target.0)
        })?;
    }
    need(b.blocks.is_empty() || !handler(b, 0), || {
        "the entry block is a handler".into()
    })?;
    let mut normal = vec![false; b.blocks.len()];
    let mut ordinary = vec![BlockId(0)];
    while let Some(block) = ordinary.pop() {
        if b.blocks.is_empty() || std::mem::replace(&mut normal[block.index()], true) {
            continue;
        }
        need(step_catch(b, block.index()).is_none(), || {
            format!("b{}: a step handler entered normally", block.0)
        })?;
        if !matches!(b.terminators[block.index()], Terminator::Raise { .. }) {
            ordinary.extend(
                successors_of_terminator(b, block.index())
                    .into_iter()
                    .map(|i| BlockId(i as u32)),
            );
        }
    }
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
        if std::mem::replace(&mut seen[k.index()], true) || handler(b, k.index()) {
            continue;
        }
        if let Some(result) = step_catch(b, k.index()) {
            let Terminator::Branch { condition, no, .. } = b.terminators[k.index()] else {
                return Err(format!("b{}: step catch must branch on its result", k.0));
            };
            need(condition == result, || {
                format!("b{}: step catch branches on another value", k.0)
            })?;
            work.push(b.edges[no.index()].target);
            continue;
        }
        need(
            !matches!(b.terminators[k.index()], Terminator::Return(_)),
            || format!("failure path reaches the return of b{}", k.0),
        )?;
        work.extend(successors(b, k.index()));
    }
    Ok(())
}

fn inst_wf(c: &mut Ctx, inst: &Inst) -> R {
    let bool_ty = Ty::Scalar(Scalar::Bool);
    match *inst {
        Inst::Simd {
            result,
            op,
            first,
            second,
            fail,
        } => {
            c.def(result)?;
            c.fail(fail, op.can_fail())?;
            let (from, to) = (c.ty(first)?, c.ty(result)?);
            let second = second.map(|v| c.ty(v)).transpose()?;
            let vector = |t| match t {
                Ty::Vector(v) => Some(v),
                _ => None,
            };
            use SimdOp::*;
            let ok = match op {
                WrappingAdd | WrappingSub | SaturatingAdd | SaturatingSub | Eq | Lt | BitAnd
                | BitOr | BitXor => vector(to).is_some() && from == to && second == Some(to),
                BitNot => vector(to).is_some() && from == to,
                Shl | Shr => {
                    vector(to).is_some_and(|v| second == Some(Ty::Scalar(v.lane()))) && from == to
                }
                Splat => vector(to).is_some_and(|v| from == Ty::Scalar(v.lane())),
                From => from == Ty::Ptr && vector(to).is_some(),
                Lanes => from == Ty::Ptr && vector(to).is_some_and(|v| v.lane() == Scalar::U8),
                Cast => vector(from)
                    .zip(vector(to))
                    .is_some_and(|(a, b)| a.size() == b.size()),
                Swizzle => vector(from).zip(vector(to)).is_some_and(|(a, b)| {
                    matches!(a.lane(), Scalar::U8 | Scalar::I8)
                        && a.lane() == b.lane()
                        && second == Vector::new(Scalar::U8, b.lanes()).map(Ty::Vector)
                }),
                Any => vector(from).is_some() && to == bool_ty,
                Bitmask => vector(from).is_some() && to == Ty::Scalar(Scalar::U64),
            };
            need(ok && op.binary() == second.is_some(), || {
                "vector operand types".into()
            })
        }
        Inst::PutLanes { to, vector, count } => {
            c.writes(to)?;
            c.is(count, Ty::Scalar(Scalar::U64))?;
            need(
                c.place(to)? == PlaceTy::Value(Ty::Scalar(Scalar::U8))
                    && matches!(c.ty(vector)?, Ty::Vector(Vector::U8x16 | Vector::U8x32)),
                || "put_lanes operand types".into(),
            )
        }
        Inst::Const { result, bits } => {
            c.def(result)?;
            let fits = match c.ty(result)? {
                Ty::Scalar(Scalar::Bool) => bits <= 1,
                Ty::Scalar(Scalar::Char) => {
                    u32::try_from(bits).ok().and_then(char::from_u32).is_some()
                }
                Ty::Scalar(Scalar::F32) => bits <= u64::from(u32::MAX),
                Ty::Vector(_) => false,
                t => t.int().is_none_or(|(_, w)| w == 64 || bits >> w == 0),
            };
            need(fits, || {
                format!("const v{} does not fit its type", result.0)
            })?;
            // One spelling per constant, whatever NaN the producer's host computed (#97).
            let other_nan = match c.ty(result)? {
                Ty::Scalar(Scalar::F32) => {
                    f32::from_bits(bits as u32).is_nan() && bits != 0x7fc0_0000
                }
                Ty::Scalar(Scalar::F64) => {
                    f64::from_bits(bits).is_nan() && bits != 0x7ff8_0000_0000_0000
                }
                _ => false,
            };
            need(!other_nan, || {
                format!("const v{} is a NaN other than the canonical one", result.0)
            })
        }
        Inst::LayoutConst { result, ref query } => {
            c.def(result)?;
            let mut known = true;
            query.clone().map_types(|t| {
                known &= t.index() < c.m.types.len();
                t
            });
            need(known && c.ty(result)? == Ty::Scalar(Scalar::U64), || {
                "bad layout constant".into()
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
        Inst::DescriptorRef {
            result,
            function,
            identity,
        } => {
            c.def(result)?;
            need(
                function.index() < c.m.functions.len()
                    && c.m
                        .statics
                        .get(identity.index())
                        .and_then(Static::literal_bytes)
                        .is_some()
                    && c.ty(result)? == Ty::Ptr,
                || "bad descriptor reference".into(),
            )
        }
        Inst::CompactWidth { result, word, ty } => {
            c.def(result)?;
            need(c.m.types.get(ty.index()).is_some(), || {
                "unknown type".into()
            })?;
            c.is(word, Ty::Scalar(Scalar::U64))?;
            c.is(result, Ty::Scalar(Scalar::U64))
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
                !matches!(t, Ty::Vector(_))
                    && (t != Ty::Ptr || matches!(op, CompareOp::Eq | CompareOp::Ne)),
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
            if let AllocPlace::Like(v)
            | AllocPlace::Region(v)
            | AllocPlace::LentFrame(v)
            | AllocPlace::ByteTail(v) = place
            {
                c.is(v, Ty::Ptr)?;
            }
            if matches!(place, AllocPlace::LentFrame(_)) {
                need(
                    matches!(
                        kind,
                        AllocKind::Own {
                            edge: hirsel_core_layout::FieldTy::Seq(_)
                        }
                    ),
                    || "lent frame bytes back only sequence storage".into(),
                )?;
            }
            if matches!(place, AllocPlace::ByteHead | AllocPlace::ByteTail(_)) {
                need(
                    align == 8
                        && matches!(
                            kind,
                            AllocKind::Own { edge: hirsel_core_layout::FieldTy::Seq(item) }
                                if **item == hirsel_core_layout::FieldTy::Scalar(Scalar::U8)
                        ),
                    || "split byte backing requires a word-aligned byte sequence".into(),
                )?;
            }
            // Region storage is stable and stable storage is in a region (crate docs,
            // "Cleanup classes"); an own block names the edge that will own it.
            need(
                matches!(kind, AllocKind::Region) == matches!(place, AllocPlace::Region(_)),
                || "stable storage is exactly the storage allocated in a region".into(),
            )?;
            if let AllocKind::Own { edge } = kind {
                use hirsel_core_layout::FieldTy as F;
                if matches!(edge, F::FrozenBytes) {
                    need(
                        matches!(place, AllocPlace::Current) && overflow.is_none(),
                        || "static bytes admit a fixed charge against the current payer".into(),
                    )?;
                }
                let types = &c.m.types;
                need(refs_ok(edge, types.len()), || {
                    format!("allocation edge {edge:?} names an unknown type")
                })?;
                need(
                    matches!(
                        edge,
                        F::Box(_) | F::Rc(_) | F::Arc(_) | F::Seq(_) | F::Map(..) | F::FrozenBytes
                    ),
                    || format!("allocation edge {edge:?} is not an own block's edge"),
                )?;
            }
            // An environment's captures are a record of ordinary data from offset
            // 8, behind the uncharged descriptor word.
            if let AllocKind::Environment { ty } = kind {
                need(
                    c.m.types.get(ty.index()).is_some_and(|t| {
                        t.repr == hirsel_core_layout::Repr::Own
                            && matches!(t.body, hirsel_core_layout::Body::Record(_))
                    }),
                    || format!("environment row {ty:?} is not an own record"),
                )?;
            }
            need(align.is_power_of_two(), || {
                "alignment is not a power of two".into()
            })
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
            })
        }
        Inst::Free { block, charge } => {
            c.is(block, Ty::Ptr)?;
            c.is(charge, Ty::Scalar(Scalar::U64))
        }
        Inst::FrameAdmit { .. } | Inst::FrameActive { .. } | Inst::FrameRefund { .. } => {
            frame::inst(c, inst)
        }
        Inst::PromoteFrame { carrier } => {
            c.writes(carrier)?;
            c.place(carrier).map(|_| ())
        }
        Inst::Copy { to, from, bytes } | Inst::Move { to, from, bytes } => {
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
        Inst::BuiltinUse { ref definition } => {
            need(crate::is_versioned_builtin(definition), || {
                "unknown builtin use".into()
            })
        }
        Inst::Step { fail } => c.fail(fail, true),
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
                    // A direct call agrees with its callee by the signature itself:
                    // signature rows are not interned, so two rows may spell one
                    // signature, and across native units only the value can agree.
                    let f = c.m.functions.get(f.index()).ok_or("unknown callee")?;
                    need(
                        f.signature == site.signature
                            || c.m.signatures.get(f.signature.index()) == Some(sig),
                        || "direct call through another signature".into(),
                    )?;
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

pub use crate::without_marks;

/// A place in one of the body's slots, as far as its address names it statically:
/// the slot and its field, case-field and carrier-word steps, cut at the first
/// step whose offset is a run-time value (`index`, `element`, `byte`), which then
/// stands for some part of the place before it.
type Place = (usize, Vec<(u8, u32, u32)>);

/// What one instruction or terminator does to slot contents, as [`written`]
/// reads it: the places it reads, the places it writes (on its normal edge
/// only), the slots whose contents end or begin unwritten, and the slots it
/// lets escape behind a pointer.
#[derive(Default)]
struct Touch {
    reads: Vec<Place>,
    writes: Vec<Place>,
    resets: Vec<usize>,
}

/// Resolve address `a` to the slot place it names, with the places its
/// evaluation reads (each `deref` step reads the pointer stored before it).
fn place_of(b: &Body, a: AddressId, reads: &mut Vec<Place>) -> Option<Place> {
    let info = b.addresses.get(a.index())?;
    let mut at = match info.base {
        AddressBase::Slot(s) => Some((s.index(), vec![])),
        AddressBase::Value(_) => None,
        AddressBase::Address(x) => place_of(b, x, reads),
    };
    let mut cut = false;
    for s in info.steps.get(&b.pools.steps).unwrap_or_default() {
        let Some(p) = at.as_mut() else { break };
        match *s {
            Step::Deref => {
                reads.push(p.clone());
                at = None;
            }
            _ if cut => {}
            Step::Field(f) => p.1.push((0, f, 0)),
            Step::CaseField { case, field } => p.1.push((1, case, field)),
            Step::Seq(w) => p.1.push((2, w as u32, 0)),
            Step::Index(_) | Step::Element(_) | Step::Byte(_) => cut = true,
        }
    }
    at
}
/// Whether a place of this type has no bytes (`()`, a record of such fields): a
/// read of it reads nothing, so it needs no write.
fn no_bytes(m: &Module, ty: PlaceTy) -> bool {
    fn field(m: &Module, f: &hirsel_core_layout::FieldTy) -> bool {
        use hirsel_core_layout::FieldTy as F;
        match f {
            F::Inline(r) => record(m, r.index()),
            F::Array(x, n) => *n == 0 || field(m, x),
            _ => false,
        }
    }
    fn record(m: &Module, t: usize) -> bool {
        m.types.get(t).is_some_and(|t| match &t.body {
            hirsel_core_layout::Body::Record(f) => f.iter().all(|f| field(m, &f.ty)),
            hirsel_core_layout::Body::Variant(_) => false,
        })
    }
    match ty {
        PlaceTy::Declared(t) => record(m, t.index()),
        PlaceTy::Value(_) => false,
    }
}
fn touch(m: &Module, b: &Body, i: &Inst) -> Touch {
    let mut t = Touch::default();
    let read = |t: &mut Touch, a: AddressId| {
        let mut r = vec![];
        let p = place_of(b, a, &mut r);
        t.reads.extend(r);
        p
    };
    // The place `a` names, as something read: none when it has no bytes.
    let bytes = |a: AddressId, p: Option<Place>| {
        p.filter(|_| {
            !b.addresses
                .get(a.index())
                .is_some_and(|i| no_bytes(m, i.ty))
        })
    };
    match *i {
        Inst::Address { address, .. } => {
            read(&mut t, address);
        }
        Inst::Read { address, .. } | Inst::Case { address, .. } | Inst::Drop { address, .. } => {
            let p = read(&mut t, address);
            t.reads.extend(bytes(address, p));
        }
        Inst::Store { address, .. }
        | Inst::SetCase { address, .. }
        | Inst::PutLanes { to: address, .. } => {
            let p = read(&mut t, address);
            t.writes.extend(p);
        }
        Inst::Copy { to, from, .. } | Inst::Move { to, from, .. } => {
            let p = read(&mut t, from);
            t.reads.extend(bytes(from, p));
            let p = read(&mut t, to);
            t.writes.extend(p);
        }
        Inst::Grow { carrier, .. } => {
            let p = read(&mut t, carrier);
            t.reads.extend(p.clone());
            t.writes.extend(p);
        }
        // Whatever reads or writes the place through the pointer is out of
        // sight: the whole slot counts as written from here on.
        Inst::Pointer { address, .. } => {
            if let Some((s, _)) = read(&mut t, address) {
                t.writes.push((s, vec![]));
            }
        }
        Inst::StorageLive { slot } | Inst::StorageDead { slot } => t.resets.push(slot.index()),
        Inst::Call { call } => {
            let Some(site) = b.calls.get(call.index()) else {
                return t;
            };
            let Some(sig) = m.signatures.get(site.signature.index()) else {
                return t;
            };
            let args = site.args.get(&b.pools.args).unwrap_or_default();
            let dest = usize::from(sig.dest.is_some());
            for (k, arg) in args.iter().enumerate() {
                let Arg::Address(a) = *arg else { continue };
                let p = read(&mut t, a);
                if k < dest {
                    t.writes.extend(p);
                    continue;
                }
                match sig.params.get(k - dest) {
                    // A `mut` place may be an out-place the callee fills
                    // (`rt.join`'s result, a receive's item).
                    Some(Param::Address {
                        loan: Loan::Mut, ..
                    }) => t.writes.extend(p),
                    _ => t.reads.extend(bytes(a, p)),
                }
            }
        }
        _ => {}
    }
    t
}
/// Every reachable read of a slot's contents follows, on every path to it from the
/// entry, a write that overlaps the place read (crate docs, "Values, slots and
/// addresses"). Writes are a store, copy, `set_case`, `grow`, a call's destination
/// or `mut` argument (which may be an out-place the callee fills, so it is not a
/// read), and a `pointer` to the slot (after which writes are out of sight).
/// Reads are `read`, `case`, `drop`, a copy's source, a `grow`'s carrier, a `view`
/// or `take` argument, a `deref` step and a `raise fail` cause; a place with no
/// bytes is never read. A `storage_live` or `storage_dead` leaves the slot
/// unwritten. Places are compared by their static steps: a write overlaps a read
/// when one's steps are a prefix of the other's.
///
/// A drop flag is how the IR says a place is written on some paths only: a `bool`
/// slot that is stored `true` only where a place is written on every path to the
/// store guards that place, and the `yes` edge of a branch on a read of the flag
/// carries the place as written.
fn written(m: &Module, b: &Body) -> R {
    let blocks = b.blocks.len();
    let insts = |bi: usize| b.blocks[bi].insts.get(&b.insts).unwrap_or_default();
    let touches: Vec<Vec<Touch>> = (0..blocks)
        .map(|bi| insts(bi).iter().map(|i| touch(m, b, i)).collect())
        .collect();
    // Places are interned per body; a state is the set of places written on every path.
    let mut ids: std::collections::HashMap<Place, usize> = Default::default();
    let mut all: Vec<Place> = vec![];
    for t in touches.iter().flatten() {
        for p in t.reads.iter().chain(&t.writes) {
            ids.entry(p.clone()).or_insert_with(|| {
                all.push(p.clone());
                all.len() - 1
            });
        }
    }
    let mut by_slot: Vec<Vec<usize>> = vec![vec![]; b.slots.len()];
    for (k, p) in all.iter().enumerate() {
        if let Some(v) = by_slot.get_mut(p.0) {
            v.push(k);
        }
    }
    let words = all.len().div_ceil(64).max(1);
    type Set = Vec<u64>;
    let has = |s: &Set, k: usize| s[k / 64] >> (k % 64) & 1 == 1;
    let apply = |now: &mut Set, t: &Touch| {
        for &s in &t.resets {
            for &k in by_slot.get(s).map_or(&[][..], |v| v) {
                now[k / 64] &= !(1 << (k % 64));
            }
        }
        for p in &t.writes {
            let k = ids[p];
            now[k / 64] |= 1 << (k % 64);
        }
    };
    let covered = |now: &Set, p: &Place| {
        by_slot.get(p.0).is_some_and(|ks| {
            ks.iter()
                .any(|&k| has(now, k) && (all[k].1.starts_with(&p.1) || p.1.starts_with(&all[k].1)))
        })
    };
    // Drop flags: whole `bool` slots, the reads of them, and the constants stored.
    let is_flag = |a: AddressId| {
        b.addresses
            .get(a.index())
            .is_some_and(|i| i.ty == PlaceTy::Value(Ty::Scalar(hirsel_core_layout::Scalar::Bool)))
    };
    let whole = |a: AddressId| {
        place_of(b, a, &mut vec![])
            .filter(|p| p.1.is_empty() && is_flag(a))
            .map(|p| p.0)
    };
    let mut flag_of: std::collections::HashMap<usize, usize> = Default::default();
    let mut consts: std::collections::HashMap<usize, u64> = Default::default();
    for i in b.insts.iter() {
        match *i {
            Inst::Read {
                result, address, ..
            } => {
                if let Some(f) = whole(address) {
                    flag_of.insert(result.index(), f);
                }
            }
            Inst::Const { result, bits } => {
                consts.insert(result.index(), bits);
            }
            _ => {}
        }
    }
    // A guard per flag slot: None is "every place" (not yet narrowed), the start of
    // the greatest fixpoint.
    let mut guards: Vec<Option<Set>> = vec![None; b.slots.len()];
    let solve = |guards: &Vec<Option<Set>>| -> Vec<Option<Set>> {
        let out = |bi: usize, start: &Set| -> Vec<(usize, Set)> {
            let mut now = start.clone();
            let mut edges = vec![];
            for (k, i) in insts(bi).iter().enumerate() {
                if let Some(f) = i.fail(b) {
                    edges.push((f.index(), now.clone()));
                }
                apply(&mut now, &touches[bi][k]);
            }
            let edge = |e: &EdgeId| b.edges[e.index()].target.index();
            if let Terminator::Branch { condition, yes, no } = &b.terminators[bi] {
                let mut taken = now.clone();
                if let Some(&f) = flag_of.get(&condition.index()) {
                    match &guards[f] {
                        None => taken.iter_mut().for_each(|w| *w = !0),
                        Some(g) => taken.iter_mut().zip(g).for_each(|(w, g)| *w |= g),
                    }
                }
                edges.push((edge(yes), taken));
                edges.push((edge(no), now));
            } else {
                for t in successors_of_terminator(b, bi) {
                    edges.push((t, now.clone()));
                }
            }
            edges
        };
        let mut start: Vec<Option<Set>> = vec![None; blocks];
        if blocks > 0 {
            start[0] = Some(vec![0; words]);
        }
        let mut work = vec![0usize];
        while let Some(bi) = work.pop() {
            let Some(s) = start[bi].clone() else { continue };
            for (t, carried) in out(bi, &s) {
                if t == 0 {
                    continue;
                }
                let next = match &start[t] {
                    None => carried,
                    Some(old) => old.iter().zip(&carried).map(|(x, y)| x & y).collect(),
                };
                if start[t].as_ref() != Some(&next) {
                    start[t] = Some(next);
                    work.push(t);
                }
            }
        }
        start
    };
    let branches_on_flags = b.terminators.iter().any(|t| {
        matches!(t, Terminator::Branch { condition, .. } if flag_of.contains_key(&condition.index()))
    });
    let start = loop {
        let start = solve(&guards);
        if !branches_on_flags {
            break start;
        }
        // Narrow each flag's guard to what is written wherever it is set.
        let mut next: Vec<Option<Set>> = vec![None; b.slots.len()];
        let mut meet = |f: usize, s: Set| {
            next[f] = Some(match next[f].take() {
                None => s,
                Some(o) => o.iter().zip(&s).map(|(x, y)| x & y).collect(),
            })
        };
        for bi in 0..blocks {
            let Some(mut now) = start[bi].clone() else {
                continue;
            };
            for (k, i) in insts(bi).iter().enumerate() {
                match *i {
                    Inst::Store { address, value } => {
                        if let Some(f) = whole(address) {
                            match consts.get(&value.index()) {
                                Some(0) => {}
                                Some(_) => meet(f, now.clone()),
                                None => meet(f, vec![0; words]),
                            }
                        }
                    }
                    _ => {
                        for p in &touches[bi][k].writes {
                            if p.1.is_empty()
                                && guards.get(p.0).is_some()
                                && own::is_flag_slot(b, p.0)
                            {
                                meet(p.0, vec![0; words]);
                            }
                        }
                    }
                }
                apply(&mut now, &touches[bi][k]);
            }
        }
        // A flag never set true guards nothing it could need.
        let next: Vec<Option<Set>> = next
            .into_iter()
            .map(|g| Some(g.unwrap_or_else(|| vec![0; words])))
            .collect();
        if next == guards {
            break start;
        }
        guards = next;
    };
    let show = |p: &Place| {
        let mut s = format!("s{}", p.0);
        for &(k, x, y) in &p.1 {
            match k {
                0 => s += &format!(".{x}"),
                1 => s += &format!(".case{x}.{y}"),
                _ => s += &format!(".seq{x}"),
            }
        }
        s
    };
    for bi in 0..blocks {
        let Some(mut now) = start[bi].clone() else {
            continue;
        };
        let check = |now: &Set, p: &Place| {
            need(covered(now, p), || {
                format!(
                    "b{bi}: {} is read on a path that does not write it",
                    show(p)
                )
            })
        };
        for t in &touches[bi] {
            for p in &t.reads {
                check(&now, p)?;
            }
            apply(&mut now, t);
        }
        if let Terminator::Raise {
            cause: RaiseCause::Fail(a),
            ..
        } = b.terminators[bi]
        {
            let mut r = vec![];
            let p = place_of(b, a, &mut r);
            for p in r.iter().chain(&p) {
                check(&now, p)?;
            }
        }
    }
    Ok(())
}

/// The blocks `bi`'s terminator continues to.
fn successors_of_terminator(b: &Body, bi: usize) -> Vec<usize> {
    let edge = |e: &EdgeId| b.edges[e.index()].target.index();
    match &b.terminators[bi] {
        Terminator::Jump(e) => vec![edge(e)],
        Terminator::Branch { yes, no, .. } => vec![edge(yes), edge(no)],
        Terminator::Switch {
            arms, otherwise, ..
        } => arms
            .get(&b.pools.arms)
            .unwrap_or_default()
            .iter()
            .map(|a| edge(&a.edge))
            .chain([edge(otherwise)])
            .collect(),
        Terminator::Raise { cleanup, .. } => cleanup.iter().map(|c| c.index()).collect(),
        Terminator::Return(_) | Terminator::Propagate | Terminator::Unreachable => vec![],
    }
}
