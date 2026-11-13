//! Patterns are typed once into a body-local arena. Tests precede bindings and moves;
//! exhaustiveness specializes constructor rows, including nested products and ranges.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Head {
    Any,
    Bind(u32, bool),
    Product(Option<u32>),
    Range(i128, i128),
}
#[derive(Clone, Copy)]
struct Pat {
    site: NodeId,
    ty: TyId,
    head: Head,
    viewed: bool,
    fields: PoolRange<u32>,
    order: PoolRange<u32>,
}
#[derive(Default)]
pub(super) struct Patterns {
    pats: Vec<Pat>,
    fields: Vec<u32>,
    order: Vec<u32>,
    // Exhaustiveness workspaces. Recursive specialization appends and truncates
    // these arenas, reusing their capacity across every arm and match in the body.
    matrix: Vec<u32>,
    types: Vec<TyId>,
    heads: Vec<Head>,
    edges: Vec<(u32, u32)>,
    escaping: Vec<Freeze>,
    pub(super) guard: bool,
    rebind: bool,
    scopes: Vec<Scope>,
}
#[derive(Clone, Copy)]
enum ScopeKind {
    Region(Option<ValueId>),
    Bound { failed: usize },
}
#[derive(Clone, Copy)]
struct Scope {
    kind: ScopeKind,
    frame: Option<usize>,
    /// The owners begun before it opened.
    owners: usize,
}

#[derive(Clone, Copy)]
struct Subject {
    ty: TyId,
    value: Option<ValueId>,
    place: Option<At>,
    owned: bool,
    viewed: bool,
}

impl<'c, 'a> Cx<'c, 'a> {
    fn pat_push(&mut self, site: NodeId, ty: TyId, head: Head, fields: PoolRange<u32>) -> u32 {
        let id = self.patterns.pats.len() as u32 + 1;
        self.patterns.pats.push(Pat {
            site,
            ty,
            head,
            viewed: false,
            fields,
            order: PoolRange::EMPTY,
        });
        id
    }
    fn pat(&self, id: u32) -> Pat {
        self.patterns.pats[id as usize - 1]
    }
    fn pat_fields(&self, id: u32) -> PoolRange<u32> {
        if id == 0 {
            PoolRange::EMPTY
        } else {
            self.pat(id).fields
        }
    }
    fn pat_head(&self, id: u32) -> Head {
        if id == 0 {
            Head::Any
        } else {
            self.pat(id).head
        }
    }
    fn pattern(&mut self, n: NodeId, ty: TyId) -> u32 {
        let viewed = matches!(self.t.kind(ty), TyKind::View(_));
        let id = self.pattern_inner(n, ty);
        self.patterns.pats[id as usize - 1].viewed = viewed;
        id
    }
    fn pattern_inner(&mut self, n: NodeId, ty: TyId) -> u32 {
        self.out.counts.nodes += 1;
        let declared = ty;
        let ty = self.pattern_type(ty);
        let c = self.kids(n);
        match self.kind(n) {
            NodeKind::Wildcard => self.pat_push(n, declared, Head::Any, PoolRange::EMPTY),
            NodeKind::BindPattern | NodeKind::TakePattern if self.binder(n).is_some() => self
                .pat_push(
                    n,
                    declared,
                    Head::Bind(
                        self.binder(n).unwrap(),
                        self.kind(n) == NodeKind::TakePattern,
                    ),
                    PoolRange::EMPTY,
                ),
            NodeKind::UnitPattern if ty == UNIT || ty == ERROR => {
                self.pat_push(n, ty, Head::Product(None), PoolRange::EMPTY)
            }
            NodeKind::TuplePattern => {
                let len = match self.t.kind(ty) {
                    TyKind::Tuple(ts) => ts.len(),
                    _ if ty == ERROR => return self.pat_push(n, ty, Head::Any, PoolRange::EMPTY),
                    _ => {
                        self.refuse("LANG-17", n, "a tuple pattern needs a tuple");
                        return self.pat_push(n, ERROR, Head::Any, PoolRange::EMPTY);
                    }
                };
                if c.len() != len {
                    self.refuse(
                        "LANG-17",
                        n,
                        "a tuple pattern has one pattern per component",
                    );
                    return self.pat_push(n, ERROR, Head::Any, PoolRange::EMPTY);
                }
                let fields = self.pattern_fields(len);
                for (i, &child) in c.iter().enumerate() {
                    let t = match self.t.kind(ty) {
                        TyKind::Tuple(ts) => ts[i],
                        _ => unreachable!(),
                    };
                    let p = self.pattern(child, t);
                    self.patterns.fields[fields.start as usize + i] = p;
                }
                self.pat_push(n, ty, Head::Product(None), fields)
            }
            NodeKind::ConstructorPattern | NodeKind::BindPattern => {
                self.constructor_pattern(n, declared)
            }
            NodeKind::LiteralPattern => {
                if !self.comparison_allowed(ty, false) {
                    self.refuse(
                        "LANG-17",
                        n,
                        "a literal pattern requires equality on a key type",
                    );
                    return self.pat_push(n, ERROR, Head::Any, PoolRange::EMPTY);
                }
                let bits = self.pattern_number(n, ty);
                self.pat_push(n, ty, Head::Range(bits, bits), PoolRange::EMPTY)
            }
            NodeKind::RangePattern => {
                if ty == BOOL {
                    self.refuse("LANG-17", n, "ranges match integers or char");
                }
                let lo = self.pattern_number(c[0], ty);
                let hi = self.pattern_number(c[1], ty);
                if lo > hi {
                    self.refuse(
                        "LANG-17",
                        n,
                        "a closed pattern range starts at or below its end",
                    );
                }
                self.pat_push(n, ty, Head::Range(lo, hi), PoolRange::EMPTY)
            }
            _ => {
                self.gap(n, "this pattern form is not built");
                self.pat_push(n, ERROR, Head::Any, PoolRange::EMPTY)
            }
        }
    }
    fn pattern_fields(&mut self, len: usize) -> PoolRange<u32> {
        let start = self.patterns.fields.len();
        self.patterns.fields.resize(start + len, 0);
        PoolRange::new(start as u32, len as u32)
    }
    fn constructor_pattern(&mut self, n: NodeId, ty: TyId) -> u32 {
        let c = self.kids(n);
        let (path, args) = if self.kind(n) == NodeKind::BindPattern {
            (n, &[][..])
        } else {
            (c[0], &c[1..])
        };
        let fail = |cx: &mut Self| cx.pat_push(n, ERROR, Head::Any, PoolRange::EMPTY);
        let TyKind::Adt(d, _) = self.t.kind(self.pattern_type(ty)) else {
            if ty != ERROR {
                self.refuse(
                    "LANG-17",
                    n,
                    "this constructor does not belong to the scrutinee's type",
                );
            }
            return fail(self);
        };
        let d = *d;
        let p: &'c Program<'a> = self.p;
        let decl = &p.decls[d.index()];
        let target = p.target(self.at, path).copied();
        let from = p.modules[self.at.module as usize].id;
        let visible = match target {
            Some(Target::Definition(def)) => p.by_definition[def.0 as usize] == Some(Def::Type(d)),
            Some(Target::ContextualConstructor { name }) => p
                .index
                .constructor_candidates(from, name)
                .any(|def| p.by_definition[def.0 as usize] == Some(Def::Type(d))),
            _ => false,
        };
        if !visible || decl.opaque && decl.site.module != from {
            self.refuse(
                "LANG-17",
                n,
                "this constructor is not visible for the scrutinee's type",
            );
            return fail(self);
        }
        let name = match target {
            Some(Target::ContextualConstructor { name }) => p.index.symbols.text(name),
            _ => decl.path.0.name.as_str(),
        };
        let (case, fs, labelled) = match &decl.body {
            DeclBody::Record(fs) => (None, fs.as_slice(), true),
            DeclBody::Variant(cs) => {
                let Some(i) = cs.iter().position(|c| c.name == name) else {
                    self.refuse(
                        "LANG-17",
                        n,
                        "this variant does not belong to the scrutinee's type",
                    );
                    return fail(self);
                };
                (Some(i as u32), cs[i].fields.as_slice(), cs[i].labelled)
            }
            _ => {
                self.gap(n, "this constructor pattern is not built");
                return fail(self);
            }
        };
        let fields = self.pattern_fields(fs.len());
        let order_start = self.patterns.order.len();
        self.patterns.order.resize(order_start + fs.len(), 0);
        let mut order_len = 0;
        let mut rest = false;
        let mut positional = 0;
        for &arg in args {
            if self.kind(arg) == NodeKind::RestPattern {
                rest = true;
                continue;
            }
            let label = self.item.tree.node(arg).token.is_some();
            let i = if labelled {
                if !label {
                    self.refuse("LANG-17", arg, "this pattern field needs its label");
                    continue;
                }
                let Some(i) = fs.iter().position(|f| f.name == self.token_text(arg)) else {
                    self.refuse("LANG-17", arg, "unknown pattern field");
                    continue;
                };
                i
            } else {
                if label {
                    self.refuse("LANG-17", arg, "this pattern payload is positional");
                }
                let i = positional;
                positional += 1;
                i
            };
            if i >= fs.len() {
                self.refuse("LANG-17", arg, "too many pattern fields");
                continue;
            }
            let index = fields.start as usize + i;
            if self.patterns.fields[index] != 0 {
                self.refuse("LANG-17", arg, "a pattern field is named twice");
                continue;
            }
            let t = self.pattern_field_ty(ty, case, i);
            let child = self.pattern(self.kids(arg)[0], t);
            self.patterns.fields[index] = child;
            self.patterns.order[order_start + order_len] = i as u32;
            order_len += 1;
        }
        if !rest && (0..fs.len()).any(|i| self.patterns.fields[fields.start as usize + i] == 0) {
            self.refuse("LANG-17", n, "a pattern names every field, or ends in `..`");
        }
        let id = self.pat_push(n, ty, Head::Product(case), fields);
        self.patterns.pats[id as usize - 1].order =
            PoolRange::new(order_start as u32, order_len as u32);
        id
    }
    fn pattern_field_ty(&mut self, ty: TyId, case: Option<u32>, i: usize) -> TyId {
        if let TyKind::Anchored(inner, root) = *self.t.kind(ty) {
            let ft = self.pattern_field_ty(inner, case, i);
            return if kind(&self.p.decls, &self.t, ft, &self.params) == Kind::Stable
                && !matches!(self.t.kind(ft), TyKind::Anchored(..))
            {
                self.t.mk(TyKind::Anchored(ft, root))
            } else {
                ft
            };
        }
        let (d, arg_len) = match self.t.kind(ty) {
            TyKind::Tuple(ts) => return ts[i],
            TyKind::Adt(d, args) => (*d, args.len()),
            _ => return ERROR,
        };
        let ft = match &self.p.decls[d.index()].body {
            DeclBody::Record(fs) => fs[i].ty,
            DeclBody::Variant(cs) => cs[case.unwrap() as usize].fields[i].ty,
            _ => return ERROR,
        };
        // Reuse the call substitution machinery, with a body-local arena for the
        // argument slice so recursion never allocates a vector for each field.
        let mark = self.patterns.types.len();
        for j in 0..arg_len {
            let a = match self.t.kind(ty) {
                TyKind::Adt(_, args) => args[j],
                _ => unreachable!(),
            };
            self.patterns.types.push(a);
        }
        let out = subst(&mut self.t, ft, &self.patterns.types[mark..]);
        self.patterns.types.truncate(mark);
        out
    }
    fn pattern_type(&self, mut ty: TyId) -> TyId {
        while let TyKind::View(inner) | TyKind::Anchored(inner, _) = self.t.kind(ty) {
            ty = *inner;
        }
        ty
    }
    fn pattern_number(&mut self, n: NodeId, ty: TyId) -> i128 {
        let text = self.token_text(n);
        let span = self.span(n);
        let negative = self.item.text[span.start as usize..span.end as usize]
            .trim_start()
            .starts_with('-');
        let number = match self.token_kind(n) {
            Some(TokenKind::KwTrue) if ty == BOOL => Some(1),
            Some(TokenKind::KwFalse) if ty == BOOL => Some(0),
            Some(TokenKind::Character) if ty == scalar(Scalar::Char) => {
                character_value(text).map(|c| i128::from(u32::from(c)))
            }
            Some(TokenKind::Byte) if ty == scalar(Scalar::U8) => byte_value(text).map(i128::from),
            Some(TokenKind::Integer) => {
                if int_of(&self.t, ty).is_none() {
                    self.refuse(
                        "LANG-17",
                        n,
                        "an integer pattern needs an integer scrutinee",
                    );
                    return 0;
                }
                let (digits, suffix) = int_suffix(text);
                if suffix.is_some_and(|s| scalar(s) != ty) {
                    self.refuse("LANG-11", n, "the pattern literal has a different type");
                    return 0;
                }
                int_magnitude(digits)
                    .and_then(|v| i128::try_from(v).ok())
                    .map(|v| if negative { -v } else { v })
            }
            _ if ty == ERROR => return 0,
            _ => {
                self.gap(n, "non-integral literal patterns are not built");
                return 0;
            }
        };
        let Some(v) = number else {
            self.refuse("LANG-3", n, "pattern literal does not fit its type");
            return 0;
        };
        match self.pattern_domain(ty) {
            Some((lo, hi))
                if v >= lo
                    && v <= hi
                    && !(ty == scalar(Scalar::Char) && (0xd800..=0xdfff).contains(&v)) =>
            {
                v
            }
            _ => {
                self.refuse("LANG-3", n, "pattern literal does not fit its type");
                0
            }
        }
    }
    fn pattern_domain(&self, ty: TyId) -> Option<(i128, i128)> {
        if ty == BOOL {
            return Some((0, 1));
        }
        if ty == scalar(Scalar::Char) {
            return Some((0, 0x10ffff));
        }
        let (signed, bits) = int_of(&self.t, ty)?;
        Some(if signed {
            (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1)
        } else {
            (0, (1i128 << bits) - 1)
        })
    }

    /// A value used as a scrutinee is read in place. Only `take`, a destructuring
    /// let, or the selected arm's take bindings consume a source slot (OWN-15).
    fn subject(&mut self, n: NodeId, consume: bool, want: Option<Want>) -> Subject {
        let at = match self.place(n) {
            Ok(at) => at,
            Err(()) => {
                return Subject {
                    ty: ERROR,
                    value: None,
                    place: None,
                    owned: false,
                    viewed: false,
                };
            }
        };
        if let Some(at) = at {
            self.out.counts.nodes += 1;
            self.out.counts.uses += 1;
            let borrowed_index = self.borrowed_index(at);
            if borrowed_index {
                self.use_place(at, Use::View(Hold::View(Extent::Temp)), n);
            } else if consume {
                self.use_place(at, Use::Value, n);
            } else {
                self.need_live(at.place.slot, n);
                self.check_holds(at.place, Act::Read, n);
            }
            let value = if matches!(self.rep(at.ty), Ok(Rep::Place(_))) {
                self.place_ptr(at, n)
            } else {
                self.emit_place(at, n)
            };
            if let Some(w) = want {
                let actual = if borrowed_index {
                    self.t.mk(TyKind::View(at.ty))
                } else {
                    at.ty
                };
                self.coerce(actual, w, n);
            }
            let viewed = borrowed_index || matches!(self.t.kind(at.ty), TyKind::View(_));
            let mut ty = match self.t.kind(at.ty) {
                TyKind::View(t) => *t,
                _ => at.ty,
            };
            if !matches!(self.t.kind(ty), TyKind::Anchored(..))
                && kind(&self.p.decls, &self.t, ty, &self.params) == Kind::Stable
            {
                // A bare stable parameter is a view for this call. Payloads retain
                // that view's anchor even when its region is not named in the type.
                let name = self.slot_name(at.place.slot);
                let root = self
                    .fd
                    .params
                    .iter()
                    .position(|p| self.p.index.symbols.text(p.name) == name)
                    .map_or(Root::Local(at.place.slot), |i| Root::Parameter(i as u32));
                let root = self.t.mk(TyKind::Region(root));
                ty = self.t.mk(TyKind::Anchored(ty, root));
            }
            return Subject {
                ty,
                value,
                place: Some(at),
                owned: consume && !viewed,
                viewed,
            };
        }
        let (ty, value) = self.expr(n, want);
        let viewed = matches!(self.t.kind(ty), TyKind::View(_));
        let ty = match self.t.kind(ty) {
            TyKind::View(t) => *t,
            _ => ty,
        };
        Subject {
            ty,
            value,
            place: None,
            owned: !viewed,
            viewed,
        }
    }
    fn pattern_takes(&self, id: u32) -> bool {
        if id == 0 {
            return false;
        }
        if matches!(self.pat_head(id), Head::Bind(_, true)) {
            return true;
        }
        let fields = self.pat_fields(id);
        (0..fields.len)
            .any(|i| self.pattern_takes(self.patterns.fields[(fields.start + i) as usize]))
    }
    fn pattern_at(
        &mut self,
        subject: Subject,
        case: Option<u32>,
        field: u32,
        ty: TyId,
        n: NodeId,
    ) -> Subject {
        let place = subject.place.map(|mut at| {
            let start = self.steps.len() as u32;
            self.steps.extend_from_within(
                at.place.start as usize..(at.place.start + at.place.len) as usize,
            );
            self.steps.push(if case.is_some() {
                PStep::Any
            } else {
                PStep::Field(field)
            });
            at.place.start = start;
            at.place.len += 1;
            at.ty = ty;
            at
        });
        let value = if self.live() {
            match (subject.value, self.rep(ty)) {
                (Some(value), Ok(r)) => {
                    let base = match self.rep(subject.ty).map(Rep::pointee) {
                        Ok(Some(id)) => {
                            AddressBase::Address(self.fields_of(AddressBase::Value(value), id, n))
                        }
                        _ => {
                            return Subject {
                                ty,
                                value: None,
                                place,
                                ..subject
                            };
                        }
                    };
                    let step =
                        case.map_or(Step::Field(field), |case| Step::CaseField { case, field });
                    match r {
                        Rep::None => None,
                        Rep::Scalar(_) | Rep::View(_) => {
                            let t = r.ty().unwrap();
                            let a = self.address(base, &[step], PlaceTy::Value(t), n);
                            let result = self.value(t);
                            self.inst(
                                Inst::Read {
                                    result,
                                    address: a,
                                    mode: ReadMode::Bits,
                                },
                                n,
                            );
                            Some(result)
                        }
                        Rep::Place(id) => {
                            let a = self.address(base, &[step], PlaceTy::Declared(id), n);
                            let p = self.pointer(a, n);
                            // A resource's block is freed when it is taken apart (T3.7),
                            // so its parts are copied out of it.
                            match self.rep(subject.ty) {
                                Ok(Rep::Place(r)) if self.e.is_resource(r) => {
                                    Some(self.snapshot(p, id, n))
                                }
                                _ => Some(p),
                            }
                        }
                    }
                }
                _ => None,
            }
        } else {
            None
        };
        Subject {
            ty,
            value,
            place,
            ..subject
        }
    }
    fn pattern_compare(
        &mut self,
        value: Option<ValueId>,
        ty: Ty,
        bits: u64,
        op: CompareOp,
        no: Option<BlockId>,
        n: NodeId,
    ) {
        if let (Some(left), Some(no)) = (value, no)
            && self.live()
        {
            let right = self.konst(ty, bits, n).unwrap();
            let result = self.value(Ty::Scalar(Scalar::Bool));
            self.inst(
                Inst::Compare {
                    result,
                    op,
                    left,
                    right,
                },
                n,
            );
            let yes = self.new_block(&[]);
            let ye = self.edge(yes, &[]);
            let ne = self.edge(no, &[]);
            self.end(
                Terminator::Branch {
                    condition: result,
                    yes: ye,
                    no: ne,
                },
                Some(n),
            );
            self.start(yes);
        }
    }
    fn pattern_tests(&mut self, id: u32, subject: Subject, no: Option<BlockId>) {
        if id == 0 {
            return;
        }
        let pat = self.pat(id);
        match pat.head {
            Head::Range(lo, hi) => {
                let Some(ty) = self.ir_ty(pat.ty) else {
                    return;
                };
                let bits = |v: i128| match ty.int() {
                    Some((_, w)) => (v as u64) & (u64::MAX >> (64 - w)),
                    _ => v as u64,
                };
                if lo == hi {
                    self.pattern_compare(subject.value, ty, bits(lo), CompareOp::Eq, no, pat.site);
                } else {
                    self.pattern_compare(subject.value, ty, bits(lo), CompareOp::Ge, no, pat.site);
                    self.pattern_compare(subject.value, ty, bits(hi), CompareOp::Le, no, pat.site);
                }
            }
            Head::Product(case) => {
                if let Some(case) = case
                    && let (Some(value), Ok(Some(ty))) =
                        (subject.value, self.rep(subject.ty).map(Rep::pointee))
                    && self.live()
                {
                    let a = self.fields_of(AddressBase::Value(value), ty, pat.site);
                    let tag = self.value(Ty::Scalar(Scalar::U32));
                    self.inst(
                        Inst::Case {
                            result: tag,
                            address: a,
                        },
                        pat.site,
                    );
                    self.pattern_compare(
                        Some(tag),
                        Ty::Scalar(Scalar::U32),
                        u64::from(case),
                        CompareOp::Eq,
                        no,
                        pat.site,
                    );
                }
                for i in 0..pat.fields.len {
                    let child = self.patterns.fields[(pat.fields.start + i) as usize];
                    if matches!(self.pat_head(child), Head::Any | Head::Bind(..)) {
                        continue;
                    }
                    let ty = self.pat(child).ty;
                    let field = self.pattern_at(subject, case, i, ty, pat.site);
                    self.pattern_tests(child, field, no);
                }
            }
            _ => {}
        }
    }
    fn bind_pattern(&mut self, id: u32, mut subject: Subject, mutable: bool) {
        if id == 0 {
            return;
        }
        let pat = self.pat(id);
        if pat.viewed {
            subject.owned = false;
            subject.viewed = true;
        }
        match pat.head {
            Head::Bind(b, take) => {
                if take && subject.viewed {
                    self.refuse("OWN-15", pat.site, "a pattern cannot take through a view");
                }
                let borrowed = !subject.owned && !self.copies(pat.ty);
                let ty = if borrowed && !matches!(self.t.kind(pat.ty), TyKind::View(_)) {
                    self.t.mk(TyKind::View(pat.ty))
                } else {
                    pat.ty
                };
                if borrowed && let Some(at) = subject.place {
                    self.freeze(at.place, Hold::View(Extent::Block(self.depth)));
                }
                if let Err(what) = self.rep(ty) {
                    self.gap(pat.site, what);
                }
                let value = match (subject.value, self.rep(ty)) {
                    (Some(v), Ok(Rep::Place(id))) if !borrowed && self.live() => {
                        Some(self.snapshot(v, id, pat.site))
                    }
                    _ => subject.value,
                };
                let (binding, origin) = if mutable {
                    (
                        Binding::Slot(self.slot(ty, value, pat.site)),
                        Origin::LetMut,
                    )
                } else {
                    (
                        Binding::Value(value),
                        if borrowed {
                            Origin::PatternView
                        } else {
                            Origin::Let
                        },
                    )
                };
                if self.patterns.rebind {
                    let var = self.vars[b as usize].as_mut().unwrap();
                    var.ty = ty;
                    var.binding = binding;
                    var.origin = origin;
                } else {
                    self.declare(b, ty, binding, origin, pat.site);
                    self.scope_slots.push(b);
                }
                self.own(b, pat.site);
            }
            Head::Product(case) => {
                if subject.owned
                    && let TyKind::Adt(d, _) = self.t.kind(subject.ty)
                {
                    let decl = &self.p.decls[d.index()];
                    if decl.repr == Repr::Resource
                        && decl.site.module != self.p.modules[self.at.module as usize].id
                    {
                        self.refuse(
                            "OWN-14",
                            pat.site,
                            "a resource is destructured only in its defining module",
                        );
                    }
                }
                let count = if pat.order.len == 0 {
                    pat.fields.len
                } else {
                    pat.order.len
                };
                for at in 0..count {
                    let i = if pat.order.len == 0 {
                        at
                    } else {
                        self.patterns.order[(pat.order.start + at) as usize]
                    };
                    let child = self.patterns.fields[(pat.fields.start + i) as usize];
                    if child == 0 {
                        continue;
                    }
                    let ty = self.pat(child).ty;
                    let field = self.pattern_at(subject, case, i, ty, pat.site);
                    self.bind_pattern(child, field, mutable);
                }
                if subject.owned && !subject.viewed {
                    self.transfer(pat, case, subject);
                }
            }
            _ => {}
        }
    }
    /// The payload transfer of an owned destructure (OWN-14, OWN-15, T3.7): the bound
    /// parts are owners now, so the source is consumed whole and the omitted fields
    /// are dropped in declaration order.
    fn transfer(&mut self, pat: Pat, case: Option<u32>, subject: Subject) {
        for i in 0..pat.fields.len {
            let child = self.patterns.fields[(pat.fields.start + i) as usize];
            if child != 0 && self.pat_head(child) != Head::Any {
                continue;
            }
            let ty = self.pattern_field_ty(pat.ty, case, i as usize);
            if self.drop_id(ty).is_none() {
                continue;
            }
            let field = self.pattern_at(subject, case, i, ty, pat.site);
            self.drop_field(field.value, ty, pat.site);
        }
        if subject.place.is_none() {
            self.consume(subject.value, pat.site);
        }
        // T3.7: the destructured resource's block (its storage, unused cell and grant)
        // is freed and refunded; its fields went on with their blocks.
        if let (Some(value), Ok(Rep::Place(id))) = (subject.value, self.rep(pat.ty))
            && self.e.is_resource(id)
            && self.live()
        {
            let holder = self.address(
                AddressBase::Value(value),
                &[],
                PlaceTy::Value(Ty::Ptr),
                pat.site,
            );
            let block = self.read_bits(holder, Ty::Ptr, pat.site);
            if let Some(charge) = self.e.resource_charge(id)
                && let Some(charge) = self.konst(Ty::Scalar(Scalar::U64), charge, pat.site)
            {
                self.inst(Inst::Free { block, charge }, pat.site);
            }
        }
    }
    fn chosen_pattern(&mut self, id: u32, mut subject: Subject, n: NodeId) -> Subject {
        if self.pattern_takes(id) && !subject.owned {
            if !subject.viewed
                && let Some(at) = subject.place
            {
                self.use_place(at, Use::Take, n);
                subject.owned = true;
            }
        }
        subject
    }

    pub(super) fn for_pattern(
        &mut self,
        pat: NodeId,
        ty: TyId,
        value: Option<ValueId>,
        owned: bool,
    ) {
        let id = self.pattern(pat, ty);
        let matrix = self.patterns.matrix.len();
        self.patterns.matrix.push(id);
        if let Some(missing) = self.pattern_missing(matrix, ty) {
            self.refuse(
                "LANG-16",
                pat,
                format!("for pattern is refutable; missing `{missing}`"),
            );
        }
        self.patterns.matrix.truncate(matrix);
        // The whole source is already frozen for the loop. Iteration bindings end
        // before the next element, so no additional source-place freeze is needed.
        self.bind_pattern(
            id,
            Subject {
                ty,
                value,
                place: None,
                owned,
                viewed: !owned,
            },
            false,
        );
    }

    pub(super) fn destructure_let(
        &mut self,
        s: NodeId,
        pat: NodeId,
        value: NodeId,
        declared: Option<TyId>,
        other: Option<NodeId>,
    ) -> bool {
        let freezes = self.freezes.len();
        let subject = self.subject(value, true, declared.and_then(expect));
        let id = self.pattern(pat, subject.ty);
        if let Some(other) = other {
            self.let_else(pat, subject, id, other);
        } else {
            let matrix = self.patterns.matrix.len();
            self.patterns.matrix.push(id);
            if let Some(missing) = self.pattern_missing(matrix, subject.ty) {
                self.refuse(
                    "LANG-17",
                    pat,
                    format!("let pattern is refutable; missing `{missing}`"),
                );
            }
            self.patterns.matrix.truncate(matrix);
        }
        let subject = self.chosen_pattern(id, subject, pat);
        self.bind_pattern(id, subject, self.kind(s) == NodeKind::LetMut);
        if subject.viewed || kind(&self.p.decls, &self.t, subject.ty, &self.params) == Kind::Scoped
        {
            let depth = self.depth;
            self.unfreeze(freezes, |h| {
                Some(if h == Hold::View(Extent::Temp) {
                    Hold::View(Extent::Block(depth))
                } else {
                    h
                })
            });
        }
        subject.ty == NEVER
    }

    pub(super) fn match_(&mut self, n: NodeId, want: Option<Want>) -> (TyId, Option<ValueId>) {
        let c = self.kids(n);
        let subject = self.subject(c[0], false, None);
        // A scrutinee temporary lives to the statement's end unless the chosen arm
        // takes it; when only some arms do, its flag says which (OWN-15, OWN-18).
        let temp = subject
            .place
            .is_none()
            .then(|| self.temp_index(subject.value))
            .flatten();
        if let Some(i) = temp {
            self.flag_temp(i, n);
        }
        let (mut took, mut kept) = (false, false);
        let mark = self.log.len();
        let was_dead = self.dead;
        let join = self.new_join();
        let matrix = self.patterns.matrix.len();
        let edge_mark = self.patterns.edges.len();
        let escape_mark = self.patterns.escaping.len();
        let mut ty = want.filter(|w| w.payload == 0).map(|w| w.ty);
        for (i, &arm) in c[1..].iter().enumerate() {
            let a = self.kids(arm);
            let id = self.pattern(a[0], subject.ty);
            let guarded = a.len() == 3;
            let no = (i + 2 < c.len() && self.live()).then(|| self.new_block(&[]));
            self.pattern_tests(id, subject, no);
            if let Some(i) = temp {
                self.set_temp(i, Avail::Live);
            }
            self.depth += 1;
            let (scope, holds) = (self.scope_slots.len(), self.freezes.len());
            let owners = self.owner_mark();
            if guarded {
                self.bind_pattern(
                    id,
                    Subject {
                        owned: false,
                        ..subject
                    },
                    false,
                );
                let guard = std::mem::replace(&mut self.patterns.guard, true);
                let (_, cv) = self.expr(a[1], expect(BOOL));
                self.patterns.guard = guard;
                // A guard is a condition: its temporaries end with it.
                self.end_owners(owners, None, true, a[1]);
                if let (Some(cv), Some(no)) = (cv, no)
                    && self.live()
                {
                    let yes = self.new_block(&[]);
                    let ye = self.edge(yes, &[]);
                    let ne = self.edge(no, &[]);
                    self.end(
                        Terminator::Branch {
                            condition: cv,
                            yes: ye,
                            no: ne,
                        },
                        Some(a[1]),
                    );
                    self.start(yes);
                }
                self.unfreeze(holds, |_| None);
                self.patterns.rebind = true;
            }
            let chosen = self.chosen_pattern(id, subject, a[0]);
            self.bind_pattern(id, chosen, false);
            self.patterns.rebind = false;
            let taken = temp.is_some_and(|i| !self.temp_live(i));
            let body = *a.last().unwrap();
            let (at, v) = self.expr(body, ty.and_then(expect).or(want));
            if ty.is_none() && at != NEVER {
                ty = Some(at);
            }
            if !self.dead {
                took |= taken;
                kept |= !taken;
            }
            self.pattern_scope_end(scope, holds, owners, body, at, v);
            self.depth -= 1;
            if let Some(edge) = self.arm_edge(mark) {
                self.patterns.edges.push(edge);
            }
            self.goto(join, at, v, body);
            if !guarded {
                self.patterns.matrix.push(id);
            }
            if let Some(no) = no {
                self.start(no);
            }
        }
        if let Some(missing) = self.pattern_missing(matrix, subject.ty) {
            self.refuse("LANG-17", n, format!("match is missing `{missing}`"));
        }
        if let Some(i) = temp {
            let avail = match (took, kept) {
                (true, true) => Avail::Maybe,
                (true, false) => Avail::Moved,
                _ => Avail::Live,
            };
            self.set_temp(i, avail);
        }
        let v = self.land(join);
        let mut edges = std::mem::take(&mut self.patterns.edges);
        self.merge(mark, &edges[edge_mark..], n);
        edges.truncate(edge_mark);
        self.patterns.edges = edges;
        self.pattern_escapes(escape_mark);
        self.dead |= was_dead;
        self.patterns.matrix.truncate(matrix);
        (ty.unwrap_or(NEVER), v)
    }

    /// Constructor specialization as in rustc_pattern_analysis::usefulness. The
    /// matrix is flat and recursion uses ranges into reusable per-body arenas.
    fn pattern_missing(&mut self, start: usize, ty: TyId) -> Option<String> {
        if mentions_error(&self.t, ty) {
            return None;
        }
        let types = self.patterns.types.len();
        self.patterns.types.push(ty);
        let rows = PoolRange::new(start as u32, (self.patterns.matrix.len() - start) as u32);
        let result = self.missing(rows, types, 1).map(|mut v| v.remove(0));
        self.patterns.types.truncate(types);
        result
    }
    fn missing(&mut self, rows: PoolRange<u32>, types: usize, width: usize) -> Option<Vec<String>> {
        if width == 0 {
            return (rows.len == 0).then(Vec::new);
        }
        let ty = self.pattern_type(self.patterns.types[types]);
        let heads = self.patterns.heads.len();
        if let Some((lo, hi)) = self.pattern_domain(ty) {
            // Split intervals only where a pattern starts or ends, never enumerate
            // an integer domain. Char's surrogate hole has no inhabitants.
            let mut at = lo;
            while at <= hi {
                if ty == scalar(Scalar::Char) && at == 0xd800 {
                    at = 0xe000;
                    continue;
                }
                let mut end = hi;
                if ty == scalar(Scalar::Char) && at < 0xd800 {
                    end = 0xd7ff;
                }
                for r in (0..rows.len).step_by(width) {
                    let id = self.patterns.matrix[(rows.start + r) as usize];
                    if let Head::Range(a, b) = self.pat_head(id) {
                        if a > at {
                            end = end.min(a - 1);
                        }
                        if b >= at {
                            end = end.min(b);
                        }
                    }
                }
                self.patterns.heads.push(Head::Range(at, end));
                at = end + 1;
            }
        } else {
            match self.t.kind(ty) {
                TyKind::Adt(d, _) => match &self.p.decls[d.index()].body {
                    DeclBody::Variant(cs) => {
                        for i in 0..cs.len() {
                            self.patterns.heads.push(Head::Product(Some(i as u32)));
                        }
                    }
                    DeclBody::Record(_) => self.patterns.heads.push(Head::Product(None)),
                    _ => self.patterns.heads.push(Head::Any),
                },
                TyKind::Never => {}
                TyKind::Tuple(_) | TyKind::Unit => self.patterns.heads.push(Head::Product(None)),
                _ => self.patterns.heads.push(Head::Any),
            }
        }
        let end_heads = self.patterns.heads.len();
        for h in heads..end_heads {
            let head = self.patterns.heads[h];
            let child_types = self.patterns.types.len();
            let arity = match head {
                Head::Product(case) => match self.t.kind(ty) {
                    TyKind::Tuple(ts) => ts.len(),
                    TyKind::Adt(d, _) => match &self.p.decls[d.index()].body {
                        DeclBody::Record(fs) => fs.len(),
                        DeclBody::Variant(cs) => cs[case.unwrap() as usize].fields.len(),
                        _ => 0,
                    },
                    _ => 0,
                },
                _ => 0,
            };
            for i in 0..arity {
                let case = if let Head::Product(c) = head { c } else { None };
                let field = self.pattern_field_ty(ty, case, i);
                self.patterns.types.push(field);
            }
            for i in 1..width {
                let t = self.patterns.types[types + i];
                self.patterns.types.push(t);
            }
            let matrix = self.patterns.matrix.len();
            for r in (0..rows.len).step_by(width) {
                let row = (rows.start + r) as usize;
                let id = self.patterns.matrix[row];
                let ph = self.pat_head(id);
                let any = matches!(ph, Head::Any | Head::Bind(..));
                let covers = any
                    || match (ph, head) {
                        (Head::Product(a), Head::Product(b)) => a == b,
                        (Head::Range(a, b), Head::Range(c, d)) => a <= c && b >= d,
                        _ => false,
                    };
                if !covers {
                    continue;
                }
                for i in 0..arity {
                    let field = if any {
                        0
                    } else {
                        self.patterns.fields[(self.pat_fields(id).start as usize) + i]
                    };
                    self.patterns.matrix.push(field);
                }
                for i in 1..width {
                    let p = self.patterns.matrix[row + i];
                    self.patterns.matrix.push(p);
                }
            }
            let sub = PoolRange::new(matrix as u32, (self.patterns.matrix.len() - matrix) as u32);
            // Width zero still needs to distinguish a matching empty row from no
            // matching row. An empty constructor is covered if some row survived.
            let empty_covered = arity + width - 1 == 0 && (0..rows.len).step_by(width).any(|r| {
                let id = self.patterns.matrix[(rows.start + r) as usize];
                matches!(self.pat_head(id), Head::Any | Head::Bind(..)) || self.pat_head(id) == head || matches!((self.pat_head(id), head), (Head::Range(a,b), Head::Range(c,d)) if a<=c && b>=d)
            });
            let result = if empty_covered {
                None
            } else {
                self.missing(sub, child_types, arity + width - 1)
            };
            self.patterns.matrix.truncate(matrix);
            self.patterns.types.truncate(child_types);
            if let Some(mut fields) = result {
                let label = self.witness(ty, head, &fields[..arity]);
                fields.drain(..arity);
                fields.insert(0, label);
                self.patterns.heads.truncate(heads);
                return Some(fields);
            }
        }
        self.patterns.heads.truncate(heads);
        None
    }
    fn witness(&self, ty: TyId, head: Head, fields: &[String]) -> String {
        match head {
            Head::Range(lo, _) if ty == BOOL => {
                if lo == 0 {
                    "false".into()
                } else {
                    "true".into()
                }
            }
            Head::Range(lo, _) if ty == scalar(Scalar::Char) => {
                format!("{:?}", char::from_u32(lo as u32).unwrap())
            }
            Head::Range(lo, _) => lo.to_string(),
            Head::Product(case) => {
                let name = match self.t.kind(ty) {
                    TyKind::Adt(d, _) => match &self.p.decls[d.index()].body {
                        DeclBody::Variant(cs) => cs[case.unwrap() as usize].name.as_str(),
                        _ => self.p.decls[d.index()].path.0.name.as_str(),
                    },
                    _ => "",
                };
                if fields.is_empty() && !name.is_empty() {
                    name.into()
                } else {
                    format!("{}({})", name, fields.join(", "))
                }
            }
            _ => "_".into(),
        }
    }
    #[allow(clippy::too_many_arguments)]
    fn pattern_scope_end(
        &mut self,
        scope: usize,
        holds: usize,
        owners: usize,
        n: NodeId,
        ty: TyId,
        keep: Option<ValueId>,
    ) {
        let scoped = kind(&self.p.decls, &self.t, ty, &self.params) == Kind::Scoped;
        if scoped {
            let depth = self.depth;
            self.unfreeze(holds, |h| {
                Some(if h == Hold::View(Extent::Block(depth)) {
                    Hold::View(Extent::Temp)
                } else {
                    h
                })
            });
        }
        self.close_scope(scope, holds, n, ty);
        self.end_owners(owners, keep, false, n);
        if scoped {
            for &f in &self.freezes[holds..] {
                self.patterns.escaping.push(f);
            }
            self.unfreeze(holds, |_| None);
        }
    }
    fn pattern_escapes(&mut self, mark: usize) {
        for i in mark..self.patterns.escaping.len() {
            let f = self.patterns.escaping[i];
            self.freeze(f.place, f.hold);
        }
        self.patterns.escaping.truncate(mark);
    }
    pub(super) fn plain_view(&self, from: TyId, to: TyId) -> bool {
        matches!(self.t.kind(from), TyKind::View(t) if *t == to)
            && has_bounds(&self.p.decls, &self.t, to, &self.params) & crate::decls::PLAIN != 0
    }
    pub(super) fn read_plain_view(
        &mut self,
        ty: TyId,
        value: Option<ValueId>,
        n: NodeId,
    ) -> Option<ValueId> {
        let value = value?;
        if !self.live() {
            return None;
        }
        match self.rep(ty).ok()? {
            Rep::None => None,
            Rep::Place(id) => Some(self.snapshot(value, id, n)),
            r @ (Rep::Scalar(_) | Rep::View(_)) => {
                let t = r.ty().unwrap();
                let address = self.address(AddressBase::Value(value), &[], PlaceTy::Value(t), n);
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
    pub(super) fn if_let(&mut self, n: NodeId, want: Option<Want>) -> (TyId, Option<ValueId>) {
        let c = self.kids(n);
        let condition = self.kids(c[0]);
        let cond = self.owner_mark();
        let subject = self.subject(condition[1], false, None);
        let id = self.pattern(condition[0], subject.ty);
        let mark = self.log.len();
        let was_dead = self.dead;
        let no_block = self.live().then(|| self.new_block(&[]));
        let escape_mark = self.patterns.escaping.len();
        let join = self.new_join();
        self.pattern_tests(id, subject, no_block);
        // The condition's temporaries, the scrutinee unless the pattern takes it,
        // end before either branch runs (LANG-6).
        let no_block = no_block.map(|no| {
            let other = self.new_block(&[]);
            self.side(no, |cx| {
                cx.clean_temps(cond, n);
                cx.jump(other, &[], n);
            });
            other
        });
        self.depth += 1;
        let (scope, holds) = (self.scope_slots.len(), self.freezes.len());
        let chosen = self.chosen_pattern(id, subject, condition[0]);
        self.bind_pattern(id, chosen, false);
        self.end_owners(cond, None, true, condition[0]);
        let (yt, yv) = self.expr(c[1], if c.len() == 2 { expect(UNIT) } else { want });
        self.pattern_scope_end(scope, holds, cond, c[1], yt, yv);
        self.depth -= 1;
        let yes = self.arm_edge(mark);
        self.goto(join, yt, yv, c[1]);
        if let Some(no) = no_block {
            self.start(no);
        }
        let (ot, ov) = if let Some(&other) = c.get(2) {
            self.expr(
                other,
                want.filter(|w| w.payload == 0)
                    .or_else(|| {
                        (yt != NEVER).then_some(Want {
                            ty: yt,
                            view: false,
                            payload: 0,
                        })
                    })
                    .or(want),
            )
        } else {
            (UNIT, None)
        };
        let no = self.arm_edge(mark);
        let ty = if c.len() == 2 {
            UNIT
        } else if yt == NEVER {
            ot
        } else {
            yt
        };
        self.goto(join, ty, ov, n);
        let v = self.land(join);
        self.merge_arms(mark, [yes, no], was_dead, n);
        self.pattern_escapes(escape_mark);
        (ty, v)
    }
    pub(super) fn while_let(
        &mut self,
        n: NodeId,
        label: Option<String>,
    ) -> (TyId, Option<ValueId>) {
        let c = self.kids(n);
        let condition = self.kids(c[0]);
        self.push_frame(label, FrameKind::While, expect(UNIT));
        let fi = self.frames.len() - 1;
        let header = self.live().then(|| self.new_block(&[]));
        if let Some(h) = header {
            self.jump(h, &[], n);
            self.start(h);
        }
        self.frames[fi].header = header;
        let freezes = self.freezes.len();
        let cond = self.owner_mark();
        let subject = self.subject(condition[1], false, None);
        let id = self.pattern(condition[0], subject.ty);
        self.exit_edge(fi);
        let exit = self.frames[fi].exit;
        let out = self.live().then(|| self.new_block(&[]));
        if let Some(out) = out {
            self.joins[exit].block = Some(out);
        }
        // The condition's temporaries end before the body runs or the loop exits.
        let no = out.map(|out| {
            let no = self.new_block(&[]);
            self.side(no, |cx| {
                cx.clean_temps(cond, n);
                cx.jump(out, &[], n);
            });
            no
        });
        self.pattern_tests(id, subject, no);
        self.depth += 1;
        let scope = self.scope_slots.len();
        let chosen = self.chosen_pattern(id, subject, condition[0]);
        self.bind_pattern(id, chosen, false);
        self.end_owners(cond, None, true, condition[0]);
        self.loop_body(c[1]);
        self.close_scope(scope, freezes, c[1], UNIT);
        self.end_owners(cond, None, false, c[1]);
        self.depth -= 1;
        if let Some(h) = header {
            self.jump(h, &[], c[1]);
        }
        self.back_edge(fi, self.close_brace(c[1]));
        let f = self.pop_frame(n);
        self.land(f.exit);
        (UNIT, None)
    }
    fn let_else(&mut self, pat: NodeId, subject: Subject, id: u32, other: NodeId) {
        let mark = self.log.len();
        let was_dead = self.dead;
        let no = self.live().then(|| self.new_block(&[]));
        self.pattern_tests(id, subject, no);
        let yes = self.live().then(|| self.new_block(&[]));
        if let Some(yes) = yes {
            self.jump(yes, &[], pat);
        }
        if let Some(no) = no {
            self.start(no);
        }
        let (t, _) = self.expr(other, None);
        if t != NEVER && t != ERROR {
            self.refuse("LANG-17", other, "a let-else block must have type Never");
        }
        self.undo(mark);
        self.dead = was_dead;
        if let Some(yes) = yes {
            self.start(yes);
        }
    }
    pub(super) fn result_parts(&self, ty: TyId) -> Option<(DeclId, TyId, Option<TyId>)> {
        let TyKind::Adt(d, args) = self.t.kind(ty) else {
            return None;
        };
        let path = &self.p.decls[d.index()].path;
        if is_core_prelude(path, &["Result"]) {
            Some((*d, args[0], Some(args[1])))
        } else if is_core_prelude(path, &["Option"]) {
            Some((*d, args[0], None))
        } else {
            None
        }
    }
    fn wrap_case(
        &mut self,
        ty: TyId,
        case: u32,
        payload: TyId,
        value: Option<ValueId>,
        n: NodeId,
    ) -> Option<ValueId> {
        let id = match self.rep(ty) {
            Ok(Rep::Place(id)) => id,
            Err(what) => {
                self.gap(n, what);
                return None;
            }
            _ => return None,
        };
        if !self.live() {
            return None;
        }
        let to = self.new_slot(PlaceTy::Declared(id), n);
        self.inst(Inst::SetCase { address: to, case }, n);
        let rep = self.rep(payload).ok()?;
        let place = rep.stored();
        if let Some(place) = place {
            let at = self.address(
                AddressBase::Address(to),
                &[Step::CaseField { case, field: 0 }],
                place,
                n,
            );
            self.install(at, rep, value, n);
        }
        let p = self.pointer(to, n);
        self.temp(ty, Some(p));
        Some(p)
    }
    pub(super) fn try_(&mut self, n: NodeId, want: Option<Want>) -> (TyId, Option<ValueId>) {
        if self.fd.drop_of.is_some() {
            // At the `?`, which ends the node.
            let end = self.span(n).end;
            let at = Span {
                start: end - 1,
                end,
            };
            self.refuse_at("FAIL-11", at, "a finalizer has no `?` of its own");
            return (ERROR, None);
        }
        let outer = self.result_parts(self.fd.result);
        let operand = self.kids(n)[0];
        // LANG-18: only the payload expectation reaches the operand. Its own
        // expression determines the Result/Option shape and every error type.
        let hint = want.map(|mut w| {
            w.payload += 1;
            w.view = false;
            w
        });
        let subject = self.subject(operand, true, hint);
        if subject.ty == ERROR || subject.ty == NEVER {
            return (subject.ty, None);
        }
        if subject.viewed
            && has_bounds(&self.p.decls, &self.t, subject.ty, &self.params) & crate::decls::PLAIN
                == 0
        {
            self.refuse(
                "LANG-18",
                n,
                "`?` cannot consume a view of a moving or scoped value",
            );
            return (ERROR, None);
        }
        let Some((d, success, error)) = self.result_parts(subject.ty) else {
            self.refuse("LANG-18", n, "`?` consumes a Result or Option");
            return (ERROR, None);
        };
        let bound = error.filter(|&e| matches!(self.t.kind(e), TyKind::Adt(d, _) if is_core_prelude(&self.p.decls[d.index()].path, &["Capacity"])))
            .and_then(|_| self.patterns.scopes.iter().rposition(|s| matches!(s.kind, ScopeKind::Bound { .. })));
        if bound.is_none() {
            let Some((od, _, oe)) = outer else {
                self.refuse(
                    "LANG-18",
                    n,
                    "`?` needs an enclosing function returning the same Result or Option",
                );
                return (ERROR, None);
            };
            if d != od || error != oe {
                self.refuse(
                    "LANG-18",
                    n,
                    "`?` propagates only the identical error type, or Option to Option",
                );
                return (success, None);
            }
        }
        let (ok_case, err_case) = if error.is_some() { (0, 1) } else { (1, 0) };
        // The operand is taken: its payload moves to the result or to the error.
        if subject.place.is_none() {
            self.consume(subject.value, n);
        }
        let no = self.live().then(|| self.new_block(&[]));
        if let (Some(value), Ok(Rep::Place(id))) = (subject.value, self.rep(subject.ty))
            && self.live()
        {
            let a = self.fields_of(AddressBase::Value(value), id, n);
            let tag = self.value(Ty::Scalar(Scalar::U32));
            self.inst(
                Inst::Case {
                    result: tag,
                    address: a,
                },
                n,
            );
            self.pattern_compare(
                Some(tag),
                Ty::Scalar(Scalar::U32),
                u64::from(err_case),
                CompareOp::Eq,
                no,
                n,
            );
        }
        // The error branch rewraps E in the enclosing result's own instance layout;
        // Result[T,E] and Result[U,E] can have different payload offsets.
        let error_value = if let Some(e) = error {
            self.pattern_at(subject, Some(err_case), 0, e, n).value
        } else {
            None
        };
        if let Some(scope) = bound {
            let context = self.patterns.scopes[scope];
            let ScopeKind::Bound { failed } = context.kind else {
                unreachable!()
            };
            let fi = context.frame.unwrap();
            self.unwind(self.frames[fi].owners, scope, n);
            self.exit_edge(fi);
            self.goto(failed, error.unwrap(), error_value, n);
        } else {
            let value = self.wrap_case(
                self.fd.result,
                err_case,
                error.unwrap_or(UNIT),
                error_value,
                n,
            );
            self.ret(value, n);
        }
        if let Some(no) = no {
            self.start(no);
        }
        let value = self.pattern_at(subject, Some(ok_case), 0, success, n).value;
        self.temp(success, value);
        (success, value)
    }
    pub(super) fn pattern_scope_mark(&self) -> usize {
        self.patterns.scopes.len()
    }
    /// Close the lexically open region and limit scopes from `mark` on, innermost
    /// first; their owners are cleaned before (`unwind`).
    pub(super) fn pattern_scope_exits(&mut self, mark: usize, n: NodeId) {
        for i in (mark..self.patterns.scopes.len()).rev() {
            self.pattern_scope_exits_one(i, n);
        }
    }
    pub(super) fn pattern_scope_exits_one(&mut self, i: usize, n: NodeId) {
        match self.patterns.scopes[i].kind {
            ScopeKind::Region(Some(region)) => self.inst(Inst::RegionClose { region }, n),
            ScopeKind::Bound { .. } => self.inst(Inst::LimitExit, n),
            _ => {}
        }
    }
    pub(super) fn patterns_len(&self) -> usize {
        self.patterns.scopes.len()
    }
    pub(super) fn scope_owners(&self, i: usize) -> usize {
        self.patterns.scopes[i].owners
    }
    /// Some region or limit scope is open, so a failure must close it.
    pub(super) fn open_scopes(&self) -> bool {
        self.patterns
            .scopes
            .iter()
            .any(|s| matches!(s.kind, ScopeKind::Region(Some(_)) | ScopeKind::Bound { .. }))
    }
    fn capacity_ty(&mut self, n: NodeId) -> TyId {
        match self.p.core_decl("Capacity") {
            Some(d) => self.t.mk(TyKind::Adt(d, vec![])),
            None => {
                self.gap(n, "Capacity is not declared by core");
                ERROR
            }
        }
    }
    fn capacity_value(
        &mut self,
        requested: Option<ValueId>,
        available: Option<ValueId>,
        n: NodeId,
    ) -> Option<ValueId> {
        let ty = self.capacity_ty(n);
        let id = match self.rep(ty) {
            Ok(Rep::Place(id)) => id,
            _ => return None,
        };
        if !self.live() {
            return None;
        }
        let to = self.new_slot(PlaceTy::Declared(id), n);
        for (i, value) in [requested, available].into_iter().enumerate() {
            let a = self.address(
                AddressBase::Address(to),
                &[Step::Field(i as u32)],
                PlaceTy::Value(Ty::Scalar(Scalar::U64)),
                n,
            );
            self.install(a, Rep::Scalar(Ty::Scalar(Scalar::U64)), value, n);
        }
        Some(self.pointer(to, n))
    }
    pub(super) fn region_(&mut self, n: NodeId, want: Option<Want>) -> (TyId, Option<ValueId>) {
        let Some(d) = self.p.core_decl("Region") else {
            return self.gap_ty(n, "Region is not declared by core");
        };
        let ty = self.t.mk(TyKind::Adt(d, vec![]));
        let region = if self.live() {
            let result = self.value(Ty::Ptr);
            self.inst(Inst::RegionOpen { result }, n);
            Some(result)
        } else {
            None
        };
        let mark = self.patterns.scopes.len();
        let owners = self.owner_mark();
        self.patterns.scopes.push(Scope {
            kind: ScopeKind::Region(region),
            frame: None,
            owners: self.owners.len(),
        });
        self.epoch += 1;
        self.depth += 1;
        let (scope, holds) = (self.scope_slots.len(), self.freezes.len());
        if let Some(b) = self.binder(n) {
            self.declare(b, ty, Binding::Value(region), Origin::Let, n);
            self.scope_slots.push(b);
        }
        let body = self.kids(n)[0];
        let (t, v) = self.expr(body, want);
        // Region values cannot leave their scope. Rooted stable/scoped values need
        // the declaration lane's body-root relation before emission is possible.
        if self.contains_region_value(t) {
            self.refuse(
                "MEM-7",
                body,
                "a Region value cannot leave its region scope",
            );
        }
        self.close_scope(scope, holds, body, t);
        self.end_owners(owners, v, false, body);
        self.depth -= 1;
        self.pattern_scope_exits(mark, n);
        self.patterns.scopes.truncate(mark);
        self.epoch += 1;
        (t, v)
    }
    fn contains_region_value(&self, ty: TyId) -> bool {
        match self.t.kind(ty) {
            TyKind::Adt(d, args) => {
                self.p.decls[d.index()].role == Some(Role::Region)
                    || args.iter().any(|&t| self.contains_region_value(t))
            }
            TyKind::Tuple(ts) => ts.iter().any(|&t| self.contains_region_value(t)),
            TyKind::View(t) | TyKind::Array(t, _) | TyKind::Anchored(t, _) => {
                self.contains_region_value(*t)
            }
            _ => false,
        }
    }
    pub(super) fn bound_(&mut self, n: NodeId, want: Option<Want>) -> (TyId, Option<ValueId>) {
        let c = self.kids(n);
        let (size_ty, bytes) = self.expr(c[0], expect(scalar(Scalar::U64)));
        if size_ty == NEVER {
            return (NEVER, None);
        }
        let capacity = self.capacity_ty(n);
        let body_want = want.and_then(|mut w| {
            if w.payload != 0 {
                w.payload -= 1;
                Some(w)
            } else {
                self.result_parts(w.ty)
                    .and_then(|(_, t, e)| (e == Some(capacity)).then_some(t))
                    .and_then(expect)
            }
        });
        let expected = body_want.filter(|w| w.payload == 0).map(|w| w.ty);
        let mark = self.patterns.scopes.len();
        self.push_frame(None, FrameKind::Block, None);
        let fi = self.frames.len() - 1;
        let was_dead = self.dead;
        let failed = self.new_join();
        let mut yes = None;
        if self.live()
            && let Some(bytes) = bytes
        {
            let admitted = self.value(Ty::Scalar(Scalar::Bool));
            let available = self.value(Ty::Scalar(Scalar::U64));
            self.inst(
                Inst::LimitEnter {
                    admitted,
                    available,
                    bytes,
                },
                n,
            );
            let good = self.new_block(&[]);
            let bad = self.new_block(&[]);
            let ye = self.edge(good, &[]);
            let ne = self.edge(bad, &[]);
            self.end(
                Terminator::Branch {
                    condition: admitted,
                    yes: ye,
                    no: ne,
                },
                Some(n),
            );
            self.start(bad);
            let value = self.capacity_value(Some(bytes), Some(available), n);
            self.exit_edge(fi);
            self.goto(failed, capacity, value, n);
            yes = Some(good);
        } else {
            self.exit_edge(fi);
        }
        if let Some(yes) = yes {
            self.start(yes);
        }
        let owners = self.owner_mark();
        self.patterns.scopes.push(Scope {
            kind: ScopeKind::Bound { failed },
            frame: Some(fi),
            owners: self.owners.len(),
        });
        self.epoch += 1;
        let (t, value) = self.expr(c[1], body_want);
        let result = match self.p.core_decl("Result") {
            Some(d) => self
                .t
                .mk(TyKind::Adt(d, vec![expected.unwrap_or(t), capacity])),
            None => {
                self.gap(n, "Result is not declared by core");
                ERROR
            }
        };
        self.end_owners(owners, value, false, n);
        self.pattern_scope_exits(mark, n);
        self.patterns.scopes.truncate(mark);
        self.epoch += 1;
        let exit = self.frames[fi].exit;
        let good = self.wrap_case(result, 0, expected.unwrap_or(t), value, n);
        self.exit_edge(fi);
        self.goto(exit, result, good, n);
        self.dead = false;
        let error = self.land(failed);
        if error.is_some() {
            let bad = self.wrap_case(result, 1, capacity, error, n);
            self.goto(exit, result, bad, n);
        }
        self.pop_frame(n);
        let value = self.land(exit);
        self.dead |= was_dead;
        (result, value)
    }
}
