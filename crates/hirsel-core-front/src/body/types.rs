use super::*;

impl<'c, 'a> Cx<'c, 'a, '_> {
    // Grouping does not change the identity of a region argument (MEM-6).
    pub(super) fn binder_path(&self, mut n: NodeId) -> Option<NodeId> {
        while self.kind(n) == NodeKind::Group {
            n = self.kids(n)[0];
        }
        (self.kind(n) == NodeKind::Path
            && matches!(self.p.target(self.at, n), Some(Target::Binder(_))))
        .then_some(n)
    }

    pub(super) fn body_root(&mut self, n: NodeId, region_only: bool) -> TyId {
        let Some(n) = self.binder_path(n) else {
            return ERROR;
        };
        let Some(Target::Binder(b)) = self.p.target(self.at, n).copied() else {
            return ERROR;
        };
        let binder = &self.p.items[self.at.module as usize][self.at.item as usize]
            .resolution
            .binders[b as usize];
        let in_generics = self.item.tree.children(self.item.root).iter().any(|&c| {
            let node = self.item.tree.node(c);
            node.kind == NodeKind::Generics
                && node.origin.span.start <= binder.site.span.start
                && binder.site.span.end <= node.origin.span.end
        });
        if let Some(i) = self
            .fd
            .generics
            .iter()
            .position(|g| in_generics && g.region && g.name == binder.name)
        {
            return self.t.mk(TyKind::Region(Root::Generic(i as u32)));
        }
        let parameter = binder.kind == hirsel_core_resolve::BinderKind::Parameter;
        let header_parameter = self
            .closure_header_params
            .iter()
            .enumerate()
            .find(|(_, (slot, _))| *slot == b)
            .map(|(i, (_, ty))| (i, *ty));
        if let Some((i, ty)) = header_parameter {
            if region_only && !self.region_handle(ty) {
                self.refuse("MEM-6", n, "a region argument names a Region value");
                return ERROR;
            }
            return self.t.mk(TyKind::Region(Root::CallableParameter {
                depth: self.callable_type_depth,
                index: i as u32,
            }));
        }
        let parameter_index = if parameter {
            self.fd.params.iter().position(|p| {
                p.name == binder.name
                    && p.site.span.start <= binder.site.span.start
                    && binder.site.span.end <= p.site.span.end
            })
        } else {
            None
        };
        let ty = match parameter_index {
            Some(i) => Some(self.fd.params[i].ty),
            None => self.vars[b as usize]
                .map(|v| v.ty)
                .or_else(|| self.captured_root_ty(b)),
        };
        if parameter && parameter_index.is_none() && ty.is_none() {
            // A closure header may name its own parameter before its body context
            // exists. Enclosing formals instead have a resolver slot in the frame.
            self.gap(n, "closure header parameter anchors are not built");
            return ERROR;
        }
        if region_only && binder.kind != hirsel_core_resolve::BinderKind::Region
            && !ty.is_some_and(|ty| matches!(self.t.kind(ty), TyKind::Adt(d, _) if self.p.decls[d.index()].role == Some(Role::Region))) {
            self.refuse("MEM-6", n, "a region argument names a Region value");
            return ERROR;
        }
        if let Some(i) = parameter_index {
            return self.t.mk(TyKind::Region(Root::Parameter(i as u32)));
        }
        // Local and captured roots use resolver identity, like body places. An
        // enclosing formal is never reinterpreted as this closure's parameter.
        self.t.mk(TyKind::Region(Root::Local(b)))
    }

    // ---- types written in bodies
    pub(super) fn ty(&mut self, n: NodeId) -> TyId {
        self.ty_result(n, false)
    }
    pub(super) fn ty_result(&mut self, n: NodeId, scoped_roots: bool) -> TyId {
        self.out.counts.types += 1;
        let c = self.kids(n);
        match self.kind(n) {
            NodeKind::CallableType | NodeKind::OwnCallableType | NodeKind::ScopedCallableType => {
                self.callable_type_depth += 1;
                let mut params = vec![];
                let mut result = UNIT;
                let mut waits = false;
                for &child in c {
                    match self.kind(child) {
                        NodeKind::Parameters => {
                            for &param in self.kids(child) {
                                let mode = crate::decls::callable_mode_of(self.kind(param));
                                let ty = self.ty(self.kids(param)[0]);
                                if mode == Mode::Bytes && ty != scalar(Scalar::U64) && ty != ERROR {
                                    self.refuse(
                                        "LANG-5",
                                        param,
                                        "a `bytes` parameter has type `u64`",
                                    );
                                }
                                params.push((mode, ty));
                            }
                        }
                        NodeKind::ResultType => {
                            let node = self.kids(child)[0];
                            result = self.ty_result(node, true);
                            if kind(&self.p.decls, &self.t, result, &self.params) == Kind::Scoped {
                                let region = |root| {
                                    let Some(Target::Binder(b)) =
                                        self.p.target(self.at, root).copied()
                                    else {
                                        return false;
                                    };
                                    let binder = &self.p.items[self.at.module as usize]
                                        [self.at.item as usize]
                                        .resolution
                                        .binders[b as usize];
                                    binder.kind == hirsel_core_resolve::BinderKind::Region
                                        || self.vars[b as usize]
                                            .map(|v| v.ty)
                                            .or_else(|| self.captured_root_ty(b))
                                            .is_some_and(|ty| self.region_handle(ty))
                                        || self.fd.params.iter().any(|p| {
                                            p.name == binder.name && self.region_handle(p.ty)
                                        })
                                        || self
                                            .closure_header_params
                                            .iter()
                                            .any(|&(slot, ty)| slot == b && self.region_handle(ty))
                                };
                                if crate::decls::written_result_missing_anchor(
                                    self.p, self.at, node, &region,
                                ) {
                                    self.refuse(
                                        "OWN-20",
                                        node,
                                        "a callable's scoped result names a region anchor",
                                    );
                                    return ERROR;
                                }
                            }
                        }
                        NodeKind::Waits => waits = true,
                        NodeKind::Generics => {
                            self.gap(child, "generic callable types are not built")
                        }
                        _ => {}
                    }
                }
                let callable = match crate::decls::callable_kind(self.item, n) {
                    Ok(k) => k,
                    Err(bound) => {
                        self.refuse(
                            "LANG-13",
                            bound,
                            "an owning callable bracket contains only send",
                        );
                        return ERROR;
                    }
                };
                self.callable_type_depth -= 1;
                self.t.mk(TyKind::Fn(params, result, waits, callable))
            }
            NodeKind::UnitType => UNIT,
            NodeKind::GroupType => self.ty_result(c[0], scoped_roots),
            NodeKind::TupleType => {
                let xs = c.iter().map(|&x| self.ty_result(x, scoped_roots)).collect();
                self.t.mk(TyKind::Tuple(xs))
            }
            NodeKind::TypePath => self.named_ty(n, &[]),
            NodeKind::TypeArguments if matches!(self.p.target(self.at, c[0]), Some(Target::Builtin(name)) if self.p.index.symbols.text(*name) == "Inline") =>
            {
                if c.len() != 3 {
                    self.refuse(
                        "LANG-12",
                        n,
                        "Inline takes an element type and a literal length",
                    );
                    return ERROR;
                }
                let element = self.ty_result(c[1], scoped_roots);
                let Some(count) = crate::decls::literal_length(self.p.item(self.at), c[2]) else {
                    self.refuse(
                        if self.kind(c[2]) == NodeKind::Literal {
                            "LANG-3"
                        } else {
                            "SEM-9"
                        },
                        c[2],
                        "Inline length is a literal fitting its integer type and u64",
                    );
                    return ERROR;
                };
                if !matches!(
                    kind(&self.p.decls, &self.t, element, &self.params),
                    Kind::Plain | Kind::Own
                ) {
                    self.refuse(
                        "SEM-9",
                        c[1],
                        "Inline elements must be plain or own; a type parameter must be plain",
                    );
                    return ERROR;
                }
                self.t.mk(TyKind::Array(element, count))
            }
            NodeKind::TypeArguments => {
                let mut args = vec![];
                for &a in &c[1..] {
                    args.push((self.ty_result(a, scoped_roots), a));
                }
                let mut ty = self.named_ty(c[0], &args);
                if scoped_roots && crate::decls::scoped_payload(&self.p.decls, &self.t, ty) {
                    for &(root, _) in &args {
                        if matches!(self.t.kind(root), TyKind::Region(_)) {
                            ty = self.t.mk(TyKind::Anchored(ty, root));
                        }
                    }
                }
                ty
            }
            NodeKind::Anchor if c.len() == 1 => self.body_root(c[0], true),
            NodeKind::Anchor => {
                let ty = self.ty_result(c[0], scoped_roots);
                let root = self.body_root(
                    c[1],
                    !crate::decls::scoped_payload(&self.p.decls, &self.t, ty),
                );
                if !scoped_roots && crate::decls::scoped_payload(&self.p.decls, &self.t, ty) {
                    ty
                } else {
                    self.t.mk(TyKind::Anchored(ty, root))
                }
            }
            _ => {
                self.gap(n, "this type form is not built in bodies");
                ERROR
            }
        }
    }
}

impl<'c, 'a> Cx<'c, 'a, '_> {
    /// A named type written in a body, with its arguments: an alias names its right side
    /// (LANG-9), `View[T]` is the view type, and each argument has the bounds its
    /// parameter declares (LANG-12, OWN-4 for Rc and Arc) and, for a generic other than
    /// Option and Result, is not scoped (OWN-19), as in the declaration pass.
    pub(super) fn named_ty(&mut self, path: NodeId, args: &[(TyId, NodeId)]) -> TyId {
        let p: &'c Program<'a> = self.p;
        let d = match p.target(self.at, path).copied() {
            Some(Target::Builtin(name)) => match Scalar::from_name(p.index.symbols.text(name)) {
                Some(s) if args.is_empty() => return scalar(s),
                Some(_) => {
                    self.refuse("LANG-12", path, "this type takes no type arguments");
                    return ERROR;
                }
                None => {
                    self.gap(path, "inline array types are not built");
                    return ERROR;
                }
            },
            Some(Target::Definition(q)) if p.core_named(q, &["Never"]) => {
                if !args.is_empty() {
                    self.refuse("LANG-12", path, "this type takes no type arguments");
                    return ERROR;
                }
                return NEVER;
            }
            Some(Target::Definition(q)) => match p.by_definition[q.0 as usize].as_ref() {
                Some(Def::Type(d)) => *d,
                _ => {
                    self.refuse(
                        "LANG-7",
                        path,
                        format!("`{}` is not a type", p.index.definition_path(q)),
                    );
                    return ERROR;
                }
            },
            Some(Target::Binder(b)) => {
                let name = &p.items[self.at.module as usize][self.at.item as usize]
                    .resolution
                    .binders[b as usize]
                    .name;
                return match self.fd.generics.iter().position(|g| &g.name == name) {
                    Some(_) if !args.is_empty() => {
                        self.refuse("LANG-12", path, "a type parameter takes no type arguments");
                        ERROR
                    }
                    Some(i) => self.t.mk(TyKind::Param(i as u32)),
                    None => {
                        self.gap(path, "this type parameter is not built");
                        ERROR
                    }
                };
            }
            _ => {
                self.halt();
                return ERROR;
            }
        };
        let decl = &p.decls[d.index()];
        if args.len() != decl.generics.len() {
            let (name, want) = (&decl.path.0.name, decl.generics.len());
            self.refuse(
                "LANG-12",
                path,
                format!(
                    "`{name}` takes {want} type arguments, {} written",
                    args.len()
                ),
            );
            return ERROR;
        }
        let counted = matches!(decl.role, Some(Role::Rc | Role::Arc));
        for (g, &(t, at)) in decl.generics.iter().zip(args) {
            if g.region {
                if !matches!(self.t.kind(t), TyKind::Region(_)) && t != ERROR {
                    self.refuse("MEM-6", at, "a region parameter takes an `in r` argument");
                    return ERROR;
                }
                continue;
            }
            if matches!(self.t.kind(t), TyKind::Region(_)) {
                self.refuse("LANG-12", at, "a type parameter takes a type, not a region");
                return ERROR;
            }
            if !decl.structural && kind(&p.decls, &self.t, t, &self.params) == Kind::Scoped {
                self.refuse(
                    "OWN-19",
                    at,
                    "a scoped type is not a type argument of a generic other than Option and Result",
                );
                return ERROR;
            }
            let missing = g.bounds & !has_bounds(&p.decls, &self.t, t, &self.params);
            if missing != 0 {
                let rule = if counted {
                    "OWN-4"
                } else if matches!(
                    decl.role,
                    Some(Role::FixedSpan | Role::SeqSpan | Role::IndexedSpan | Role::SeqIndex)
                ) {
                    "MEM-12"
                } else {
                    "LANG-12"
                };
                let b = crate::decls::bound_names(missing);
                self.refuse(rule, at, format!("type argument is not {b}"));
                return ERROR;
            }
        }
        let tys: Vec<TyId> = args.iter().map(|a| a.0).collect();
        if decl.role == Some(Role::View) {
            return self.t.mk(TyKind::View(tys[0]));
        }
        if let DeclBody::Alias(t) = decl.body {
            return subst(&mut self.t, t, &tys);
        }
        self.t.mk(TyKind::Adt(d, tys))
    }
}

pub(super) fn is_type_node(k: NodeKind) -> bool {
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

/// A literal operand's value while it is evaluated (LANG-21).
#[derive(Clone, Copy)]
pub(super) enum Num {
    Int(i128),
    Float(f64),
}
/// `v` cut to `w` bits, read signed or not.
pub(super) fn wrap(v: i128, signed: bool, w: u32) -> i128 {
    let m = v & ((1i128 << w) - 1);
    if signed && m >> (w - 1) != 0 {
        m - (1i128 << w)
    } else {
        m
    }
}
pub(super) fn numeric(s: Scalar) -> bool {
    !matches!(s, Scalar::Bool | Scalar::Char)
}

/// An integer token's digits (with prefix) and its suffix (LANG-3).
pub(crate) fn int_suffix(text: &str) -> (&str, Option<Scalar>) {
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
pub(crate) fn float_suffix(text: &str) -> (&str, Option<Scalar>) {
    for s in [Scalar::F32, Scalar::F64] {
        if let Some(d) = text.strip_suffix(s.name()) {
            return (d, Some(s));
        }
    }
    (text, None)
}
/// The magnitude an integer token's digits spell, or None past u128.
pub(crate) fn int_magnitude(digits: &str) -> Option<u128> {
    let (radix, digits) = match digits.get(..2) {
        Some("0x") => (16, &digits[2..]),
        Some("0o") => (8, &digits[2..]),
        Some("0b") => (2, &digits[2..]),
        _ => (10, digits),
    };
    let mut value = 0u128;
    let mut any = false;
    for digit in digits.chars().filter(|&c| c != '_') {
        value = value
            .checked_mul(radix as u128)?
            .checked_add(digit.to_digit(radix)? as u128)?;
        any = true;
    }
    any.then_some(value)
}
pub(crate) fn float_bits(s: Scalar, x: f64) -> u64 {
    match s {
        Scalar::F32 => u64::from((x as f32).to_bits()),
        _ => x.to_bits(),
    }
}
