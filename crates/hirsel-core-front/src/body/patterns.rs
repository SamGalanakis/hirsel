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
}
#[derive(Default)]
pub(super) struct Patterns {
    pats: Vec<Pat>,
    fields: Vec<u32>,
    // Exhaustiveness workspaces. Recursive specialization appends and truncates
    // these arenas, reusing their capacity across every arm and match in the body.
    matrix: Vec<u32>,
    types: Vec<TyId>,
    heads: Vec<Head>,
    edges: Vec<(u32, u32)>,
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
        let ty = match self.t.kind(ty) {
            TyKind::View(t) => *t,
            _ => ty,
        };
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
            NodeKind::ConstructorPattern | NodeKind::BindPattern => self.constructor_pattern(n, ty),
            NodeKind::LiteralPattern => {
                let bits = self.pattern_number(n, ty);
                self.pat_push(n, ty, Head::Range(bits, bits), PoolRange::EMPTY)
            }
            NodeKind::RangePattern => {
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
        let TyKind::Adt(d, _) = self.t.kind(ty) else {
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
        }
        if !rest && (0..fs.len()).any(|i| self.patterns.fields[fields.start as usize + i] == 0) {
            self.refuse("LANG-17", n, "a pattern names every field, or ends in `..`");
        }
        self.pat_push(n, ty, Head::Product(case), fields)
    }
    fn pattern_field_ty(&mut self, ty: TyId, case: Option<u32>, i: usize) -> TyId {
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
        if let Ok(Some(at)) = self.place(n) {
            self.out.counts.uses += 1;
            if consume {
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
                self.coerce(at.ty, w, n);
            }
            let viewed = matches!(self.t.kind(at.ty), TyKind::View(_));
            let ty = match self.t.kind(at.ty) {
                TyKind::View(t) => *t,
                _ => at.ty,
            };
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
                    let base = match self.rep(subject.ty) {
                        Ok(Rep::Place(id)) => {
                            let a = self.address(
                                AddressBase::Value(value),
                                &[],
                                PlaceTy::Declared(id),
                                n,
                            );
                            AddressBase::Address(a)
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
                        Rep::Scalar(t) => {
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
                            Some(self.pointer(a, n))
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
                    && let (Some(value), Ok(Rep::Place(ty))) = (subject.value, self.rep(subject.ty))
                    && self.live()
                {
                    let a = self.address(
                        AddressBase::Value(value),
                        &[],
                        PlaceTy::Declared(ty),
                        pat.site,
                    );
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
                    if child == 0 {
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
                    (Binding::Value(value), Origin::Let)
                };
                self.declare(b, ty, binding, origin, pat.site);
                self.scope_slots.push(b);
            }
            Head::Product(case) => {
                for i in 0..pat.fields.len {
                    let child = self.patterns.fields[(pat.fields.start + i) as usize];
                    if child == 0 {
                        continue;
                    }
                    let ty = self.pat(child).ty;
                    let field = self.pattern_at(subject, case, i, ty, pat.site);
                    self.bind_pattern(child, field, mutable);
                }
            }
            _ => {}
        }
    }
    fn chosen_pattern(&mut self, id: u32, mut subject: Subject, n: NodeId) -> Subject {
        if self.pattern_takes(id) && !subject.owned {
            if subject.viewed {
                self.refuse("OWN-15", n, "a pattern cannot take through a view");
            } else if let Some(at) = subject.place {
                self.use_place(at, Use::Take, n);
                subject.owned = true;
            }
        }
        if subject.owned
            && let TyKind::Adt(d, _) = self.t.kind(subject.ty)
        {
            let decl = &self.p.decls[d.index()];
            if decl.repr == Repr::Resource
                && decl.site.module != self.p.modules[self.at.module as usize].id
            {
                self.refuse(
                    "OWN-14",
                    n,
                    "a resource is destructured only in its defining module",
                );
            }
        }
        subject
    }

    pub(super) fn destructure_let(
        &mut self,
        s: NodeId,
        pat: NodeId,
        value: NodeId,
        declared: Option<TyId>,
    ) -> bool {
        let subject = self.subject(value, true, declared.and_then(expect));
        let id = self.pattern(pat, subject.ty);
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
        let subject = self.chosen_pattern(id, subject, pat);
        self.bind_pattern(id, subject, self.kind(s) == NodeKind::LetMut);
        subject.ty == NEVER
    }

    pub(super) fn match_(&mut self, n: NodeId, want: Option<Want>) -> (TyId, Option<ValueId>) {
        let c = self.kids(n);
        let subject = self.subject(c[0], false, None);
        let mark = self.log.len();
        let was_dead = self.dead;
        let join = self.new_join();
        let matrix = self.patterns.matrix.len();
        let edge_mark = self.patterns.edges.len();
        let mut ty = want.map(|w| w.ty);
        for (i, &arm) in c[1..].iter().enumerate() {
            let a = self.kids(arm);
            let id = self.pattern(a[0], subject.ty);
            let guarded = a.len() == 3;
            let no = (i + 2 < c.len() && self.live()).then(|| self.new_block(&[]));
            self.pattern_tests(id, subject, no);
            self.depth += 1;
            let (scope, holds) = (self.scope_slots.len(), self.freezes.len());
            if guarded {
                self.gap(a[1], "match guards are not built");
            }
            let chosen = self.chosen_pattern(id, subject, a[0]);
            self.bind_pattern(id, chosen, false);
            let body = *a.last().unwrap();
            let (at, v) = self.expr(body, ty.and_then(expect));
            if ty.is_none() && at != NEVER {
                ty = Some(at);
            }
            self.close_scope(scope, holds, body, at);
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
        let v = self.land(join);
        let mut edges = std::mem::take(&mut self.patterns.edges);
        self.merge(mark, &edges[edge_mark..], n);
        edges.truncate(edge_mark);
        self.patterns.edges = edges;
        self.dead |= was_dead;
        self.patterns.matrix.truncate(matrix);
        (ty.unwrap_or(NEVER), v)
    }

    /// Constructor specialization as in rustc_pattern_analysis::usefulness. The
    /// matrix is flat and recursion uses ranges into reusable per-body arenas.
    fn pattern_missing(&mut self, start: usize, ty: TyId) -> Option<String> {
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
        let ty = match self.t.kind(self.patterns.types[types]) {
            TyKind::View(t) => *t,
            _ => self.patterns.types[types],
        };
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
}
