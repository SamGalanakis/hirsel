//! Patterns are typed once into a body-local arena. Tests precede bindings and moves;
//! exhaustiveness specializes constructor rows, including nested products and ranges.
use super::*;
use hirsel_core_ir::RtOp;

/// How a name pattern binds (OWN-15): a view or copy, an owner (`take`), or the
/// matched place itself, lent for the arm (`mut`).
#[derive(Clone, Copy, Debug, PartialEq)]
enum Mode {
    View,
    Take,
    Mut,
    /// A `take` or `mut` binding already refused: it binds an error.
    Refused,
}
#[derive(Clone, Copy, Debug, PartialEq)]
enum Head {
    Any,
    Bind(u32, Mode),
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
    /// A guarded arm's first binding: `mut` names bind as views until the guard holds.
    guarding: bool,
    /// The freeze records the current scrutinee's selection made; a loan subsumes them.
    selection: (usize, usize),
    /// A written anchor set, the value it checks and the written type (the freeze site).
    declared_anchors: Option<(anchors::Fact, NodeId, NodeId)>,
    scopes: Vec<Scope>,
}
#[derive(Clone, Copy)]
pub(super) struct SlotReplacement {
    pub(super) slot: AddressId,
    pub(super) previous: ValueId,
    pub(super) present: ValueId,
    pub(super) ty: TypeId,
}

#[derive(Clone, Copy)]
enum ScopeKind {
    Region(Option<ValueId>),
    Restore(SlotReplacement),
    Bound,
    StepBound {
        failed: usize,
        steps: Option<ValueId>,
        error: TyId,
    },
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
    anchors: anchors::Fact,
    ty: TyId,
    value: Option<ValueId>,
    place: Option<At>,
    /// The storage of `place`, for a part reached by a pattern.
    address: Option<AddressId>,
    owned: bool,
    viewed: bool,
    /// `place` is a writable place (OWN-12), so a `mut` binding may lend a part of it.
    writable: bool,
}

impl<'c, 'a> Cx<'c, 'a, '_> {
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
        crate::cost_scope!("construct", format!("{:?}", self.kind(n)));
        crate::cost_scope!(
            "construct-state",
            format!(
                "{:?}/{}",
                self.kind(n),
                if self.broken { "gap" } else { "live" }
            )
        );
        self.out.counts.nodes += 1;
        let declared = ty;
        let ty = self.pattern_type(ty);
        let c = self.kids(n);
        match self.kind(n) {
            NodeKind::Wildcard => self.pat_push(n, declared, Head::Any, PoolRange::EMPTY),
            NodeKind::BindPattern | NodeKind::TakePattern | NodeKind::MutPattern
                if self.binder(n).is_some() =>
            {
                let mode = match self.kind(n) {
                    NodeKind::TakePattern => Mode::Take,
                    NodeKind::MutPattern => Mode::Mut,
                    _ => Mode::View,
                };
                self.pat_push(
                    n,
                    declared,
                    Head::Bind(self.binder(n).unwrap(), mode),
                    PoolRange::EMPTY,
                )
            }
            NodeKind::UnitPattern if ty == UNIT || ty == ERROR => {
                self.pat_push(n, ty, Head::Product(None), PoolRange::EMPTY)
            }
            NodeKind::UnitPattern => {
                self.refuse("LANG-17", n, "a unit pattern needs a `()` scrutinee");
                self.pat_push(n, ERROR, Head::Any, PoolRange::EMPTY)
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
        let p: &'c Program<'a> = self.p;
        let target = p.target(self.at, path).copied();
        // A bare pattern without a binder or constructor target is a refused binding.
        // Poison it before checking the scrutinee type (PROG-8).
        if target == Some(Target::Missing) || (target.is_none() && path == n) {
            return fail(self);
        }
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
        let decl = &p.decls[d.index()];
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
            let mut refusal = refusal(
                "LANG-17",
                self.cause(n),
                "this constructor is not visible for the scrutinee's type",
            );
            if let Some(Target::ContextualConstructor { name }) = target {
                refusal = refusal.with_unresolved(
                    p.index.symbols.text(name),
                    self.op_span(path),
                    NameScope::Variant(decl.path.clone()),
                );
            }
            self.push_refusal(refusal);
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
        let mut require_all = true;
        let mut positional = 0;
        for &arg in args {
            if self.kind(arg) == NodeKind::RestPattern {
                require_all = false;
                continue;
            }
            let child = self.kids(arg)[0];
            let label = self.item.tree.node(arg).token.is_some();
            let i = if labelled {
                let name = if label {
                    self.token_text(arg)
                } else if self.kind(child) == NodeKind::BindPattern {
                    self.token_text(child)
                } else {
                    self.refuse("LANG-17", arg, "this pattern field needs its label");
                    continue;
                };
                let Some(i) = fs.iter().position(|f| f.name == name) else {
                    let mut refusal = refusal("LANG-17", self.cause(arg), "unknown pattern field");
                    if case.is_none() {
                        refusal = refusal.with_unresolved(
                            name,
                            self.op_span(if label { arg } else { child }),
                            NameScope::Field(decl.path.clone()),
                        );
                    }
                    self.push_refusal(refusal);
                    require_all = false;
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
            let child = self.pattern(child, t);
            self.patterns.fields[index] = child;
            self.patterns.order[order_start + order_len] = i as u32;
            order_len += 1;
        }
        if require_all && order_len != fs.len() {
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
            return if self.type_kind(ft) == Kind::Region
                && !matches!(self.t.kind(ft), TyKind::Anchored(..))
            {
                self.t.mk(TyKind::Anchored(ft, root))
            } else {
                ft
            };
        }
        // A field under a view has the field's own type; the binding is a view of
        // it because the pattern is viewed (`bind`), as for a viewed subject.
        let (d, arg_len) = match self.t.kind(ty) {
            TyKind::View(inner) => return self.pattern_field_ty(*inner, case, i),
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
        self.linked(out)
    }
    fn pattern_type(&self, mut ty: TyId) -> TyId {
        while let TyKind::View(inner) | TyKind::Anchored(inner, _) = self.t.kind(ty) {
            ty = *inner;
        }
        ty
    }
    fn pattern_number(&mut self, n: NodeId, ty: TyId) -> i128 {
        let text = self.token_text(n);
        // A negative literal pattern begins with its `-` token (LANG-3). Read the token:
        // a node span is not an offset into the item's text.
        let first = self.item.tree.node(n).origin.tokens.start;
        let negative = self.item.tree.tokens[first as usize].kind == TokenKind::Minus;
        let number = match self.token_kind(n) {
            Some(TokenKind::KwTrue) if ty == BOOL => Some(1),
            Some(TokenKind::KwFalse) if ty == BOOL => Some(0),
            Some(TokenKind::Character) if ty == scalar(Scalar::Char) => {
                character_value(text).map(|c| i128::from(u32::from(c)))
            }
            Some(TokenKind::Byte) if ty == scalar(Scalar::U8) => byte_value(text).map(i128::from),
            Some(TokenKind::Integer) => {
                if int_of(ty).is_none() {
                    self.refuse(
                        "LANG-17",
                        n,
                        "an integer pattern needs an integer scrutinee",
                    );
                    return 0;
                }
                let (digits, suffix) = int_suffix(text);
                if let Some(s) = suffix.filter(|&s| scalar(s) != ty) {
                    let (a, b) = (self.shown(scalar(s)), self.shown(ty));
                    self.refuse_compared(
                        "LANG-11",
                        n,
                        a,
                        b,
                        "the pattern literal has a different type",
                    );
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
            let a = self.shown(ty);
            self.refuse_literal(
                n,
                negative,
                a,
                "pattern literal does not fit its type".to_owned(),
            );
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
                let a = self.shown(ty);
                self.refuse_literal(
                    n,
                    negative,
                    a,
                    "pattern literal does not fit its type".to_owned(),
                );
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
        let (signed, bits) = int_of(ty)?;
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
                    anchors: 0,
                    ty: ERROR,
                    value: None,
                    place: None,
                    address: None,
                    owned: false,
                    viewed: false,
                    writable: false,
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
                && self.type_kind(ty) == Kind::Region
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
            let anchors = if self.anchor_tracked(at.ty) {
                self.anchor_read(at)
            } else {
                0
            };
            let writable = !at.shared
                && !matches!(self.t.kind(at.ty), TyKind::View(_))
                && self.type_kind(at.ty) != Kind::Region
                && self.var(at.place.slot).origin.writable();
            return Subject {
                anchors,
                ty,
                value,
                place: Some(at),
                address: None,
                owned: consume && !viewed,
                viewed,
                writable,
            };
        }
        let (ty, value) = self.expr(n, want);
        let viewed = matches!(self.t.kind(ty), TyKind::View(_));
        let ty = match self.t.kind(ty) {
            TyKind::View(t) => *t,
            _ => ty,
        };
        Subject {
            anchors: self.anchor_expression(n),
            ty,
            value,
            place: None,
            address: None,
            owned: !viewed,
            viewed,
            writable: false,
        }
    }
    fn pattern_takes(&self, id: u32) -> bool {
        if id == 0 {
            return false;
        }
        if matches!(self.pat_head(id), Head::Bind(_, Mode::Take)) {
            return true;
        }
        let fields = self.pat_fields(id);
        (0..fields.len)
            .any(|i| self.pattern_takes(self.patterns.fields[(fields.start + i) as usize]))
    }
    /// The site of the first `mut` binding of pattern `id` and of its first `take`.
    fn pattern_marks(&self, id: u32, lend: &mut Option<NodeId>, take: &mut Option<NodeId>) {
        if id == 0 {
            return;
        }
        match self.pat_head(id) {
            Head::Bind(_, Mode::Mut) => *lend = lend.or(Some(self.pat(id).site)),
            Head::Bind(_, Mode::Take) => *take = take.or(Some(self.pat(id).site)),
            _ => {}
        }
        let fields = self.pat_fields(id);
        for i in 0..fields.len {
            self.pattern_marks(
                self.patterns.fields[(fields.start + i) as usize],
                lend,
                take,
            );
        }
    }
    /// LANG-17, OWN-15: a `mut` binding is a part of an arm's pattern, never its
    /// whole pattern nor in a `let` or `for` pattern, and a chosen pattern does not
    /// both take and lend. True when the pattern lends.
    fn check_lends(&mut self, id: u32, arm: bool) -> bool {
        let (mut lend, mut take) = (None, None);
        self.pattern_marks(id, &mut lend, &mut take);
        let Some(site) = lend else {
            return false;
        };
        if !arm {
            self.refuse(
                "LANG-17",
                site,
                "a `mut` binding is written in a `match` arm, `if let` or `while let`",
            );
            self.refuse_marks(id, false);
            return false;
        }
        if matches!(self.pat_head(id), Head::Bind(_, Mode::Mut)) {
            self.refuse(
                "LANG-17",
                site,
                "a `mut` binding names a part of the scrutinee, not all of it",
            );
            self.refuse_marks(id, false);
            return false;
        }
        if let Some(take) = take {
            let later = if self.span(take).start > self.span(site).start {
                take
            } else {
                site
            };
            self.refuse(
                "OWN-15",
                later,
                "a pattern either takes or lends `mut`, not both",
            );
            self.refuse_marks(id, true);
            return false;
        }
        true
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
            self.steps.push(match case {
                Some(case) => PStep::Case(case, field),
                None => PStep::Field(field),
            });
            at.place.start = start;
            at.place.len += 1;
            at.ty = ty;
            at
        });
        let compact = self.compact_adt(subject.ty).is_some();
        let mut address = None;
        let value = if self.live() && compact {
            // A compact value's field is found in its blob (MEM-9, MEM-11).
            subject.value.and_then(|header| {
                let (a, _) = self.compact_field(header, subject.ty, case, field, n)?;
                address = Some(a);
                match self.rep_at(ty, n)? {
                    Rep::Scalar(t) => Some(self.read_bits(a, t, n)),
                    Rep::Place(_) => Some(self.pointer(a, n)),
                    _ => None,
                }
            })
        } else if self.live() {
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
                                address: None,
                                ..subject
                            };
                        }
                    };
                    let step =
                        case.map_or(Step::Field(field), |case| Step::CaseField { case, field });
                    match r {
                        Rep::None => None,
                        Rep::Scalar(_) => {
                            let t = r.ty().unwrap();
                            let a = self.address(base, &[step], PlaceTy::Value(t), n);
                            address = Some(a);
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
                            address = Some(a);
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
        let component = case.map_or(anchors::Component::Field(field), |c| {
            anchors::Component::Case(c, field)
        });
        self.project_descriptor(subject.value, value, component, ty);
        let anchors = self.anchors.project(subject.anchors, component);
        if anchors != 0
            && let Some(v) = value
        {
            self.anchors.values.insert(v, anchors);
        }
        Subject {
            anchors,
            ty,
            value,
            place,
            address,
            // A compact value's blob is region storage, never written (OWN-12).
            writable: subject.writable && !compact,
            ..subject
        }
    }
    pub(super) fn pattern_compare(
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
                    && self.compact_adt(subject.ty).is_some()
                    && let Some(value) = subject.value
                    && self.live()
                {
                    let tag = self.compact_tag(value, subject.ty, pat.site);
                    self.pattern_compare(
                        tag,
                        Ty::Scalar(Scalar::U32),
                        u64::from(case),
                        CompareOp::Eq,
                        no,
                        pat.site,
                    );
                } else if let Some(case) = case
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
            subject.writable = false;
        }
        match pat.head {
            Head::Bind(b, Mode::Mut) if !self.patterns.guarding => {
                self.lend_binding(b, pat, subject)
            }
            Head::Bind(b, Mode::Refused) => self.error_binding(b, pat.site),
            Head::Bind(b, mode) => {
                let take = mode == Mode::Take;
                if take && subject.viewed {
                    if self.vars[b as usize].is_none_or(|v| v.ty != ERROR) {
                        self.refuse("OWN-15", pat.site, "a pattern cannot take through a view");
                    }
                    // The refused take declares an error binder, so using it does
                    // not manufacture a second type error or emission gap.
                    if self.patterns.rebind {
                        let var = self.vars[b as usize].as_mut().expect("guard binding");
                        var.ty = ERROR;
                        var.binding = Binding::Value(None);
                    } else {
                        self.declare(b, ERROR, Binding::Value(None), Origin::Let, pat.site);
                        self.scope_slots.push(b);
                    }
                    return;
                }
                let borrowed = !subject.owned && !self.copies(pat.ty);
                let ty = if borrowed && !matches!(self.t.kind(pat.ty), TyKind::View(_)) {
                    self.t.mk(TyKind::View(pat.ty))
                } else {
                    pat.ty
                };
                if borrowed && let Some(at) = subject.place {
                    self.freeze(at.place, Hold::View(Extent::Block(self.depth)), pat.site);
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
                self.bind_descriptor(b, value);
                if self.anchor_tracked(ty) {
                    // An own scrutinee needs provenance only if a binding borrows
                    // it. Plain and taken pattern bindings allocate no anchor fact.
                    if subject.anchors == 0
                        && let Some(at) = subject.place
                    {
                        subject.anchors = self.anchor_read(at);
                    }
                    if let Some((allowed, site, written)) = self.patterns.declared_anchors {
                        self.declare_anchors(b, Some(allowed), subject.anchors, site, written);
                    } else {
                        self.set_slot_anchors(b, subject.anchors);
                    }
                    if self.type_kind(ty) == Kind::Scoped {
                        self.freeze_anchors(subject.anchors, self.depth, pat.site);
                    }
                }
                if self.type_kind(ty) == Kind::Scoped {
                    self.hold_reached(subject.anchors);
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
                        let cause = decl.site;
                        self.refuse_from(
                            "OWN-14",
                            self.span(pat.site),
                            vec![Cause::declared(cause)],
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
    /// A refused binding declares an error binder, so using it refuses nothing more.
    fn error_binding(&mut self, b: u32, site: NodeId) {
        if self.patterns.rebind {
            let var = self.vars[b as usize].as_mut().expect("guard binding");
            var.ty = ERROR;
            var.binding = Binding::Value(None);
        } else {
            self.declare(b, ERROR, Binding::Value(None), Origin::LetMut, site);
            self.scope_slots.push(b);
        }
    }
    /// After a refused lend, the pattern's `mut` bindings (and, when it also takes, its
    /// `take` bindings) bind errors.
    fn refuse_marks(&mut self, id: u32, takes: bool) {
        if id == 0 {
            return;
        }
        let i = id as usize - 1;
        if let Head::Bind(b, mode) = self.patterns.pats[i].head
            && (mode == Mode::Mut || takes && mode == Mode::Take)
        {
            self.patterns.pats[i].head = Head::Bind(b, Mode::Refused);
        }
        let fields = self.pat_fields(id);
        for k in 0..fields.len {
            self.refuse_marks(self.patterns.fields[(fields.start + k) as usize], takes);
        }
    }
    /// OWN-15: `mut x` names the place it matched in a writable scrutinee, lent to the
    /// arm. The scrutinee stays its owner: `x` owns nothing and has no drop flag, and
    /// the loan is a hold on the matched place, which subsumes the selection's views.
    fn lend_binding(&mut self, b: u32, pat: Pat, subject: Subject) {
        let lent = subject
            .place
            .filter(|_| subject.writable && !pat.viewed)
            .filter(|at| self.step_write(at.place.slot, pat.site));
        let lent = lent.filter(|at| {
            let (from, to) = self.patterns.selection;
            self.check_holds_except(at.place, Act::Write, pat.site, from..to)
        });
        let Some(at) = lent else {
            if subject.place.is_none() {
                self.refuse(
                    "OWN-15",
                    pat.site,
                    "a `mut` binding names a place, and this scrutinee is a temporary",
                );
            } else if !subject.writable || pat.viewed {
                self.refuse(
                    "OWN-15",
                    pat.site,
                    "a `mut` binding needs a writable scrutinee, not a view",
                );
            }
            self.error_binding(b, pat.site);
            return;
        };
        if subject.address.is_none() && self.live() && !matches!(self.rep(pat.ty), Ok(Rep::None)) {
            self.gap(
                pat.site,
                "a `mut` binding of this payload's representation is not built",
            );
        }
        self.freeze(at.place, Hold::Lent(self.depth), pat.site);
        let binding = Binding::Slot(subject.address);
        if self.patterns.rebind {
            let var = self.vars[b as usize].as_mut().expect("guard binding");
            var.ty = pat.ty;
            var.binding = binding;
            var.origin = Origin::MutParam;
            var.own = None;
        } else {
            self.declare(b, pat.ty, binding, Origin::MutParam, pat.site);
            self.scope_slots.push(b);
        }
        self.vars[b as usize].as_mut().unwrap().lent = Some(at.place);
        if self.anchor_tracked(pat.ty) {
            let fact = if subject.anchors == 0 {
                self.anchor_read(at)
            } else {
                subject.anchors
            };
            self.lend_anchors(b, fact);
        }
    }
    /// The payload transfer of an owned destructure (OWN-14, OWN-15, T3.7): the bound
    /// parts are owners now, so the source is consumed whole and the omitted fields
    /// are dropped in declaration order.
    /// The owned subject's omitted fields, and a destructured resource's block, are
    /// cleaned as owners of their own, so a field drop that fails cleans only the
    /// fields after it and the block, never the subject again (FAIL-12).
    fn transfer(&mut self, pat: Pat, case: Option<u32>, subject: Subject) {
        if subject.place.is_none() {
            self.consume(subject.value, pat.site);
        }
        let mark = self.owner_mark();
        // T3.7: the destructured resource's block is freed last.
        self.free_owner(subject.value, pat.ty, pat.site);
        // Begun last to first, so they are cleaned in declaration order.
        for i in (0..pat.fields.len).rev() {
            let child = self.patterns.fields[(pat.fields.start + i) as usize];
            if child != 0 && self.pat_head(child) != Head::Any {
                continue;
            }
            let ty = self.pattern_field_ty(pat.ty, case, i as usize);
            if self.drop_id(ty).is_none() {
                continue;
            }
            let field = self.pattern_at(subject, case, i, ty, pat.site);
            self.temp(ty, field.value);
        }
        self.end_owners(mark, None, false, pat.site);
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
        anchors: anchors::Fact,
    ) {
        let id = self.pattern(pat, ty);
        self.check_lends(id, false);
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
                anchors,
                ty,
                value,
                place: None,
                address: None,
                owned,
                viewed: !owned,
                writable: false,
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
        self.check_lends(id, false);
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
        let written = self
            .kids(s)
            .iter()
            .copied()
            .find(|&n| is_type_node(self.kind(n)));
        let declared = written.and_then(|n| Some((self.written_anchors(n)?, value, n)));
        let saved = std::mem::replace(&mut self.patterns.declared_anchors, declared);
        self.bind_pattern(id, subject, self.kind(s) == NodeKind::LetMut);
        self.patterns.declared_anchors = saved;
        if subject.viewed || self.type_kind(subject.ty) == Kind::Scoped {
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

    /// A structural pattern supplies the expected payload shape of a holder read.
    /// A bare binding/wildcard supplies no expected type and keeps the holder.
    fn pattern_holder_payload(&self, pattern: NodeId, original: TyId) -> Option<TyId> {
        if !matches!(
            self.kind(pattern),
            NodeKind::ConstructorPattern
                | NodeKind::TuplePattern
                | NodeKind::UnitPattern
                | NodeKind::LiteralPattern
                | NodeKind::RangePattern
        ) && !(self.kind(pattern) == NodeKind::BindPattern && self.binder(pattern).is_none())
        {
            return None;
        }
        let mut ty = original;
        let mut projected = false;
        while let Some(payload) = self.holder_payload(ty) {
            ty = payload;
            projected = true;
        }
        projected.then_some(ty)
    }
    /// Whether a written pattern has a `mut` binding, before it is typed.
    fn writes_mut_binding(&self, n: NodeId) -> bool {
        self.kind(n) == NodeKind::MutPattern
            || self.kids(n).iter().any(|&k| self.writes_mut_binding(k))
    }
    fn holder_subject(
        &mut self,
        mut subject: Subject,
        expected: TyId,
        n: NodeId,
        lends: bool,
    ) -> Subject {
        if let Some(at) = subject.place {
            let payload = if lends && subject.writable {
                self.payload_mut_at(at, expected)
            } else {
                self.payload_view_at(at, expected)
            };
            if let Some(at) = payload {
                self.use_place(at, Use::View(Hold::View(Extent::Temp)), n);
                subject.value = if matches!(self.rep(at.ty), Ok(Rep::Place(_))) {
                    self.place_ptr(at, n)
                } else {
                    self.emit_place(at, n)
                };
                subject.anchors = self.anchor_read(at);
                subject.place = Some(at);
                subject.ty = at.ty;
                subject.viewed = true;
                subject.owned = false;
                subject.writable &= !at.shared;
            }
        } else if let Some((ty, value)) =
            self.payload_view_value(subject.ty, subject.value, expected, n)
        {
            subject.anchors = self.anchor_argument(n);
            subject.ty = ty;
            subject.value = value;
            subject.viewed = true;
            subject.owned = false;
        }
        subject
    }

    pub(super) fn match_(&mut self, n: NodeId, want: Option<Want>) -> (TyId, Option<ValueId>) {
        let c = self.kids(n);
        let selection = self.freezes.len();
        let mut subject = self.subject(c[0], false, None);
        if let Some(expected) = c[1..]
            .iter()
            .find_map(|&arm| self.pattern_holder_payload(self.kids(arm)[0], subject.ty))
        {
            let lends = c[1..]
                .iter()
                .any(|&arm| self.writes_mut_binding(self.kids(arm)[0]));
            subject = self.holder_subject(subject, expected, c[0], lends);
        }
        let selection = std::mem::replace(
            &mut self.patterns.selection,
            (selection, self.freezes.len()),
        );
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
        let join = self.new_result_join();
        let matrix = self.patterns.matrix.len();
        let edge_mark = self.patterns.edges.len();
        let escape_mark = self.patterns.escaping.len();
        let mut ty = want.filter(|w| self.fixed_want(*w)).map(|w| w.ty);
        for (i, &arm) in c[1..].iter().enumerate() {
            let a = self.kids(arm);
            let id = self.pattern(a[0], subject.ty);
            self.check_lends(id, true);
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
                let guarding = std::mem::replace(&mut self.patterns.guarding, true);
                self.bind_pattern(
                    id,
                    Subject {
                        owned: false,
                        ..subject
                    },
                    false,
                );
                self.patterns.guarding = guarding;
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
            let (at, v) = self.result_expr(body, ty.and_then(expect).or(want));
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
            let mut r = refusal(
                "LANG-17",
                self.cause(n),
                format!("match is missing `{missing}`"),
            );
            r.detail = Some(Box::new(RefusalDetail::MissingPattern {
                pattern: missing,
                close: self.close_brace(n),
            }));
            self.push_refusal(r);
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
        self.patterns.selection = selection;
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
        // A column of wildcards is not split: specializing it by any constructor is
        // dropping it, and a type reaching itself through a link view would never end.
        let wild = rows.len > 0
            && (0..rows.len).step_by(width).all(|r| {
                let id = self.patterns.matrix[(rows.start + r) as usize];
                matches!(self.pat_head(id), Head::Any | Head::Bind(..))
            });
        if wild {
            self.patterns.heads.push(Head::Any);
        } else if let Some((lo, hi)) = self.pattern_domain(ty) {
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
        let scoped = self.type_kind(ty) == Kind::Scoped;
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
        self.end_value_owners(owners, keep, ty, n);
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
            self.refreeze(f);
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
        match self.rep_at(ty, n)? {
            Rep::None => None,
            Rep::Place(id) => Some(self.snapshot(value, id, n)),
            // A view of a scalar is the scalar (FORM #56).
            Rep::Scalar(_) => Some(value),
        }
    }
    pub(super) fn if_let(&mut self, n: NodeId, want: Option<Want>) -> (TyId, Option<ValueId>) {
        let c = self.kids(n);
        let condition = self.kids(c[0]);
        let cond = self.owner_mark();
        let selection = self.freezes.len();
        let subject = self.subject(condition[1], false, None);
        let id = self.pattern(condition[0], subject.ty);
        self.check_lends(id, true);
        let selection = std::mem::replace(
            &mut self.patterns.selection,
            (selection, self.freezes.len()),
        );
        let mark = self.log.len();
        let was_dead = self.dead;
        let no_block = self.live().then(|| self.new_block(&[]));
        let escape_mark = self.patterns.escaping.len();
        let join = self.new_result_join();
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
        let (yt, yv) = self.result_expr(c[1], if c.len() == 2 { expect(UNIT) } else { want });
        self.pattern_scope_end(scope, holds, cond, c[1], yt, yv);
        self.patterns.selection = selection;
        self.depth -= 1;
        let yes = self.arm_edge(mark);
        self.goto(join, yt, yv, c[1]);
        if let Some(no) = no_block {
            self.start(no);
        }
        let (ot, ov) = if let Some(&other) = c.get(2) {
            self.result_expr(
                other,
                want.filter(|w| self.fixed_want(*w) && !(w.view && yt != NEVER))
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
        self.push_frame(label, FrameKind::While { condition: None }, expect(UNIT));
        let fi = self.frames.len() - 1;
        let header = self.live().then(|| self.new_block(&[]));
        if let Some(h) = header {
            self.jump(h, &[], n);
            self.start(h);
        }
        self.frames[fi].header = header;
        let freezes = self.freezes.len();
        let cond = self.owner_mark();
        let selection = self.freezes.len();
        let subject = self.subject(condition[1], false, None);
        let id = self.pattern(condition[0], subject.ty);
        self.check_lends(id, true);
        let selection = std::mem::replace(
            &mut self.patterns.selection,
            (selection, self.freezes.len()),
        );
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
        self.patterns.selection = selection;
        self.end_owners(cond, None, false, c[1]);
        self.depth -= 1;
        self.source_step(n);
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
    pub(super) fn wrap_case(
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
        let rep = self.rep_at(payload, n)?;
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
        self.descriptor_component(Some(p), anchors::Component::Case(case, 0), value);
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
        let mapper = self.kids(n).get(1).copied();
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
            let a = self.shown(subject.ty);
            self.refuse_compared(
                "LANG-18",
                n,
                a,
                "Result or Option",
                "`?` consumes a Result or Option",
            );
            return (ERROR, None);
        };
        // LANG-18: `e ?=> m` maps a Result's error into the enclosing Result's.
        let mapped = match (mapper, error, outer) {
            (None, ..) => None,
            (Some(_), None, _) => {
                let a = self.shown(subject.ty);
                let message = "`?=>` maps the error of a Result; an Option propagates with `?`";
                self.refuse_compared("LANG-18", n, a, "Result", message);
                return (ERROR, None);
            }
            (Some(_), Some(_), Some((od, _, Some(f)))) if od == d => Some(f),
            (Some(_), Some(_), _) => {
                let b = self.shown(self.fd.result);
                let message = "`?=>` needs an enclosing function returning a Result";
                self.refuse_compared("LANG-18", n, b, "Result", message);
                return (ERROR, None);
            }
        };
        if mapped.is_none() {
            let Some((od, _, oe)) = outer else {
                self.refuse(
                    "LANG-18",
                    n,
                    "`?` needs an enclosing function returning the same Result or Option",
                );
                return (ERROR, None);
            };
            if d != od || error != oe {
                let (a, b) = (self.shown(subject.ty), self.shown(self.fd.result));
                let capacity_only = d == od
                    && oe.is_some_and(|capacity| {
                        matches!(self.t.kind(capacity), TyKind::Adt(d, _)
                            if is_core_prelude(&self.p.decls[d.index()].path, &["Capacity"]))
                            && error.is_some_and(|e| {
                                matches!(self.t.kind(e), TyKind::Tuple(fields)
                                    if fields.len() == 2 && fields[0] == capacity)
                            })
                    });
                let message = if capacity_only {
                    "`?` propagates only the identical error type, or Option to Option; use `?=> |(c, _)| c` to propagate the Capacity"
                } else {
                    "`?` propagates only the identical error type, or Option to Option"
                };
                self.refuse_compared("LANG-18", n, a, b, message);
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
        if let (Some(m), Some(e), Some(f)) = (mapper, error, mapped) {
            // The mapper runs on the error edge before any scope is left.
            let was_dead = self.dead;
            let (t, v) = if self.kind(m) == NodeKind::Arm {
                self.mapper_arm(m, e, error_value, f)
            } else {
                self.apply_mapper(n, m, e, error_value, f)
            };
            self.consume(v, n);
            if t != ERROR {
                let value = self.wrap_case(self.fd.result, err_case, f, v, n);
                // The returned error reads what the mapper's result reads, nothing else.
                if !self.anchor_tracked(f) {
                    self.anchors.last = 0;
                }
                self.ret(value, n);
            } else {
                self.halt();
            }
            self.dead = was_dead;
        } else {
            let value = self.wrap_case(
                self.fd.result,
                err_case,
                error.unwrap_or(UNIT),
                error_value,
                n,
            );
            // The propagated result reads only what its error payload reads (OWN-20).
            self.anchors.last = match error {
                Some(e) if self.anchor_tracked(e) => {
                    let subject = self.anchor_expression(operand);
                    self.anchors
                        .project(subject, anchors::Component::Case(err_case, 0))
                }
                _ => 0,
            };
            self.ret(value, n);
        }
        if let Some(no) = no {
            self.start(no);
        }
        let success_subject = self.pattern_at(subject, Some(ok_case), 0, success, n);
        let value = success_subject.value;
        if success_subject.anchors != 0 {
            self.anchors.expressions.insert(n, success_subject.anchors);
        }
        self.temp(success, value);
        (success, value)
    }
    /// LANG-18's inline arm `|p| v`: `p` binds the error payload as `let p = take x`
    /// would, and `v` of type `mapped` is code of the enclosing function.
    fn mapper_arm(
        &mut self,
        arm: NodeId,
        error: TyId,
        value: Option<ValueId>,
        mapped: TyId,
    ) -> (TyId, Option<ValueId>) {
        let [pat, body] = *self.kids(arm) else {
            unreachable!("a mapper arm is a pattern and a value");
        };
        self.depth += 1;
        let (scope, holds) = (self.scope_slots.len(), self.freezes.len());
        let owners = self.owner_mark();
        self.temp(error, value);
        let subject = Subject {
            anchors: 0,
            ty: error,
            value,
            place: None,
            address: None,
            owned: true,
            viewed: false,
            writable: false,
        };
        let id = self.pattern(pat, error);
        self.check_lends(id, false);
        let matrix = self.patterns.matrix.len();
        self.patterns.matrix.push(id);
        if let Some(missing) = self.pattern_missing(matrix, error) {
            let message = format!("a `?=>` arm pattern is refutable; missing `{missing}`");
            self.refuse("LANG-17", pat, message);
        }
        self.patterns.matrix.truncate(matrix);
        let subject = self.chosen_pattern(id, subject, pat);
        self.bind_pattern(id, subject, false);
        let (t, v) = self.expr(body, expect(mapped));
        self.pattern_scope_end(scope, holds, owners, body, t, v);
        self.depth -= 1;
        (t, v)
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
    pub(super) fn restore_on_exit(&mut self, slot: SlotReplacement) {
        self.patterns.scopes.push(Scope {
            kind: ScopeKind::Restore(slot),
            frame: None,
            owners: self.owner_mark(),
        });
        self.epoch += 1;
    }

    pub(super) fn commit_restored_slot(&mut self) {
        self.patterns.scopes.pop();
        self.epoch += 1;
    }

    pub(super) fn pattern_scope_exits_one(&mut self, i: usize, n: NodeId) {
        match self.patterns.scopes[i].kind {
            ScopeKind::Region(Some(region)) => self.inst(Inst::RegionClose { region }, n),
            ScopeKind::Bound => self.inst(Inst::LimitExit, n),
            ScopeKind::Restore(old) => {
                let restore = self.new_block(&[]);
                let done = self.new_block(&[]);
                self.list_branch(old.present, restore, done, n);
                self.start(restore);
                self.copy_to(old.slot, old.previous, old.ty, n);
                self.jump(done, &[], n);
                self.start(done);
            }
            ScopeKind::StepBound {
                failed,
                steps,
                error,
            } => {
                if self.failing {
                    let Some([caught, ..]) = self.runtime_row(RtOp::StepCatch, &[], n) else {
                        return;
                    };
                    let yes = self.new_block(&[]);
                    let no = self.new_block(&[]);
                    self.runtime_branch(caught, yes, no, n);
                    self.start(yes);
                    let value = self.step_error(error, steps, n);
                    self.exit_edge(self.patterns.scopes[i].frame.unwrap());
                    self.goto(failed, error, value, n);
                    self.start(no);
                } else {
                    self.runtime_row(RtOp::StepLeave, &[], n);
                }
            }
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
        self.patterns.scopes.iter().any(|s| {
            matches!(
                s.kind,
                ScopeKind::Region(Some(_))
                    | ScopeKind::Bound
                    | ScopeKind::StepBound { .. }
                    | ScopeKind::Restore(_)
            )
        })
    }
    pub(super) fn capacity_ty(&mut self, n: NodeId) -> TyId {
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
        let id = self.place_row(ty, "a capacity without a place is not built", n)?;
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
            self.opened_region_anchors(b);
            self.scope_slots.push(b);
        }
        let body = self.kids(n)[0];
        let (t, v) = self.result_expr(body, want);
        // MEM-7: neither the region's handle nor a value whose type names the region
        // (or one opened inside it) leaves the block. The check reads the type alone.
        let message = if self.contains_region_value(t) {
            Some("a Region value cannot leave its region scope")
        } else if self.opened_region_in(t, scope).is_some() {
            Some("a value whose type names this region cannot leave its block")
        } else {
            self.region_result_anchors(scope, body, t);
            None
        };
        if let Some(message) = message {
            // The cause is the region's name, when the scope names one.
            let cause = self
                .item
                .tree
                .node(n)
                .token
                .map(|t| self.p.site_span(self.at, self.item.token_span(t)));
            self.refuse_from(
                "MEM-7",
                self.span(body),
                cause.into_iter().map(Cause::declared).collect(),
                message,
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
    /// A region declared by a block whose slots start at `from` in `scope_slots`,
    /// which `ty` names (MEM-7): the block's own `region r` or a local bound to a
    /// `Region` value, either of which ends with the block. A parameter's or an
    /// enclosing block's region is still open where the value goes.
    pub(super) fn opened_region_in(&self, ty: TyId, from: usize) -> Option<u32> {
        let opened = &self.scope_slots[from.min(self.scope_slots.len())..];
        if opened.is_empty() {
            return None;
        }
        let region = |slot: u32| {
            opened.contains(&slot)
                && self
                    .vars
                    .get(slot as usize)
                    .copied()
                    .flatten()
                    .is_some_and(|v| {
                        matches!(self.t.kind(v.ty), TyKind::Adt(d, _)
                        if self.p.decls[d.index()].role == Some(Role::Region))
                    })
        };
        fn walk(t: &impl TyRead, ty: TyId, region: &impl Fn(u32) -> bool) -> Option<u32> {
            match t.kind(ty) {
                TyKind::Region(Root::Local(slot)) => region(*slot).then_some(*slot),
                TyKind::Tuple(xs) | TyKind::Adt(_, xs) => {
                    xs.iter().find_map(|&x| walk(t, x, region))
                }
                TyKind::Array(x, _) | TyKind::View(x) => walk(t, *x, region),
                TyKind::Anchored(x, r) => walk(t, *x, region).or_else(|| walk(t, *r, region)),
                TyKind::Fn(ps, r, _, _) => {
                    walk(t, *r, region).or_else(|| ps.iter().find_map(|&(_, x)| walk(t, x, region)))
                }
                _ => None,
            }
        }
        walk(&self.t, ty, &region)
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
        if self.token_text(n) == "steps" {
            return self.step_bound(n, want);
        }
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
            kind: ScopeKind::Bound,
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
    fn step_bound(&mut self, n: NodeId, want: Option<Want>) -> (TyId, Option<ValueId>) {
        let c = self.kids(n);
        let (size, steps) = self.expr(c[0], expect(scalar(Scalar::U64)));
        if size == NEVER {
            return (NEVER, None);
        }
        let Some(d) = self.p.core_decl("OutOfTime") else {
            return self.gap_ty(n, "OutOfTime is not declared by core");
        };
        let error = self.t.mk(TyKind::Adt(d, vec![]));
        let body_want = want.and_then(|mut w| {
            if w.payload != 0 {
                w.payload -= 1;
                Some(w)
            } else {
                self.result_parts(w.ty)
                    .and_then(|(_, t, e)| (e == Some(error)).then_some(t))
                    .and_then(expect)
            }
        });
        let expected = body_want.filter(|w| w.payload == 0).map(|w| w.ty);
        let mark = self.patterns.scopes.len();
        self.push_frame(None, FrameKind::Block, None);
        let fi = self.frames.len() - 1;
        let was_dead = self.dead;
        let failed = self.new_join();
        if let Some(steps) = steps {
            if let Some([admitted, ..]) = self.runtime_row(RtOp::StepEnter, &[Arg::Value(steps)], n)
            {
                let good = self.new_block(&[]);
                let bad = self.new_block(&[]);
                self.runtime_branch(admitted, good, bad, n);
                self.start(bad);
                let value = self.step_error(error, Some(steps), n);
                self.exit_edge(fi);
                self.goto(failed, error, value, n);
                self.start(good);
            }
        }
        let owners = self.owner_mark();
        self.patterns.scopes.push(Scope {
            kind: ScopeKind::StepBound {
                failed,
                steps,
                error,
            },
            frame: Some(fi),
            owners: self.owners.len(),
        });
        self.epoch += 1;
        self.step_scopes.push((self.frame_level, self.born));
        let (body, value) = self.expr(c[1], body_want);
        self.step_scopes.pop();
        let Some(result) = self.p.core_decl("Result") else {
            return self.gap_ty(n, "Result is not declared by core");
        };
        let result = self
            .t
            .mk(TyKind::Adt(result, vec![expected.unwrap_or(body), error]));
        self.end_owners(owners, value, false, n);
        self.pattern_scope_exits(mark, n);
        self.patterns.scopes.truncate(mark);
        self.epoch += 1;
        let exit = self.frames[fi].exit;
        let good = self.wrap_case(result, 0, expected.unwrap_or(body), value, n);
        self.exit_edge(fi);
        self.goto(exit, result, good, n);
        self.dead = false;
        let failure = self.land(failed);
        if failure.is_some() {
            let bad = self.wrap_case(result, 1, error, failure, n);
            self.goto(exit, result, bad, n);
        }
        self.pop_frame(n);
        let value = self.land(exit);
        self.dead |= was_dead;
        (result, value)
    }
    fn step_error(&mut self, error: TyId, steps: Option<ValueId>, n: NodeId) -> Option<ValueId> {
        let id = self.place_row(error, "OutOfTime without storage is not built", n)?;
        if !self.live() {
            return None;
        }
        let to = self.new_slot(PlaceTy::Declared(id), n);
        let field = self.address(
            AddressBase::Address(to),
            &[Step::Field(0)],
            PlaceTy::Value(Ty::Scalar(Scalar::U64)),
            n,
        );
        self.install(field, Rep::Scalar(Ty::Scalar(Scalar::U64)), steps, n);
        Some(self.pointer(to, n))
    }
    pub(super) fn step_place_outside(&self, slot: u32) -> bool {
        self.step_scopes.last().is_some_and(|&(frame, born)| {
            let var = self.var(slot);
            var.frame_level < frame || var.frame_level == frame && var.born < born
        })
    }
    pub(super) fn step_write(&mut self, slot: u32, n: NodeId) -> bool {
        if self.step_place_outside(slot) {
            self.refuse("MEM-30", n, "a step bound cannot mutate an outer place");
            false
        } else {
            true
        }
    }
    pub(super) fn step_callback(&mut self, ty: TyId, value: Option<ValueId>, n: NodeId) {
        if !self.step_scopes.is_empty()
            && self.contains_callback_kind::<true>(ty)
            && self.step_descriptor_forbidden(value)
        {
            self.refuse(
                "MEM-30",
                n,
                "a step bound needs a visible callback descriptor without outer mutation",
            );
        }
    }
}
