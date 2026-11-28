use super::*;

/// LANG-12 for a call whose type parameter nothing fixed, with the rewrites
/// that fix it.
pub(super) const OPEN_TYPE_PARAMETER: &str = "a type parameter of the callee is still open; give the \
                                   result a type: `let j: u64 = …`, or suffix the literal: \
                                   `8u64`";
pub(super) const OPEN_TYPE_PARAMETER_IN_ARITHMETIC: &str = "a type parameter of the callee is still \
                                   open; suffix the literal: `8u64`";

impl<'c, 'a> Cx<'c, 'a, '_> {
    pub(super) fn open_type_parameter_message(&self, call: NodeId) -> &'static str {
        if self.arithmetic_literal_operand(call) {
            OPEN_TYPE_PARAMETER_IN_ARITHMETIC
        } else {
            OPEN_TYPE_PARAMETER
        }
    }

    fn arithmetic_literal_operand(&self, call: NodeId) -> bool {
        let tree = self.item.tree;
        let nodes = self.item.range.nodes.clone();
        let start = nodes.start;
        let mut parent = vec![u32::MAX; (nodes.end - start) as usize];
        for id in nodes {
            let n = NodeId(id);
            for &child in tree.children(n) {
                if let Some(p) = parent.get_mut(child.0.wrapping_sub(start) as usize) {
                    *p = id;
                }
            }
        }
        let up = |n: NodeId| {
            parent
                .get(n.0.wrapping_sub(start) as usize)
                .filter(|&&p| p != u32::MAX)
                .map(|&p| NodeId(p))
        };
        let mut at = call;
        while let Some(p) = up(at) {
            match self.kind(p) {
                NodeKind::Group => at = p,
                NodeKind::Binary
                    if matches!(
                        self.token_text(p),
                        "+" | "-" | "*" | "/" | "%" | "&" | "|" | "^"
                    ) =>
                {
                    if self
                        .kids(p)
                        .iter()
                        .any(|&child| child != at && self.has_untyped_literal(child))
                    {
                        return true;
                    }
                    at = p;
                }
                _ => break,
            }
        }
        false
    }

    fn has_untyped_literal(&self, n: NodeId) -> bool {
        match self.kind(n) {
            NodeKind::Literal => self.unsuffixed_number(n),
            NodeKind::Unary | NodeKind::Group | NodeKind::Binary => self
                .kids(n)
                .iter()
                .any(|&child| self.has_untyped_literal(child)),
            _ => false,
        }
    }

    /// MEM-29: an element outside core's element modules meets LANG-14's capture mask,
    /// with rigid parameters read from their bounds. Refuses at `at` when it does not.
    pub(super) fn element_refused(&mut self, at: NodeId, t: TyId) -> bool {
        let p = self.p;
        if crate::element::exempt(p.index, p.modules[self.at.module as usize].id)
            || !crate::element::refused(&p.decls, &self.t, t, &self.params)
        {
            return false;
        }
        let a = self.shown(t);
        self.refuse_compared(
            "MEM-29",
            at,
            a,
            crate::element::WANTED,
            crate::element::MESSAGE,
        );
        true
    }

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

    pub(super) fn outside_vector(&mut self, n: NodeId, ty: TyId, context: &'static str) -> TyId {
        if ty > HOLE && matches!(self.t.kind(ty), TyKind::Vector(_)) {
            self.refuse("SEM-11", n, format!("a vector cannot be {context}"));
            ERROR
        } else {
            ty
        }
    }

    // ---- types written in bodies
    pub(super) fn ty(&mut self, n: NodeId) -> TyId {
        self.ty_result(n, false)
    }
    pub(super) fn ty_result(&mut self, n: NodeId, scoped_roots: bool) -> TyId {
        let ty = self.read_type(n, scoped_roots);
        self.check_type(ty, n)
    }

    fn read_type(&mut self, n: NodeId, scoped_roots: bool) -> TyId {
        self.out.counts.types += 1;
        let c = self.kids(n);
        match self.kind(n) {
            NodeKind::CallableType | NodeKind::OwnCallableType | NodeKind::ScopedCallableType => {
                let callable = match crate::decls::callable_kind(self.item, n) {
                    Ok(k) => k,
                    Err((bound, message)) => {
                        self.refuse("LANG-13", bound, message);
                        return ERROR;
                    }
                };
                self.callable_type_depth += 1;
                let mut params = vec![];
                let mut result = UNIT;
                let mut waits = false;
                for &child in c {
                    match self.kind(child) {
                        NodeKind::Parameters => {
                            for &param in self.kids(child) {
                                let mode = crate::decls::callable_mode_of(self.kind(param));
                                let ty_node = self.kids(param)[0];
                                let ty = self.ty(ty_node);
                                let ty =
                                    self.outside_vector(ty_node, ty, "part of a callable type");
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
                            result = self.outside_vector(node, result, "part of a callable type");
                            if callable == Callable::ScopedView
                                && crate::decls::view_result_reserved(
                                    &self.p.decls,
                                    &self.t,
                                    result,
                                )
                            {
                                self.refuse("LANG-13", node, crate::decls::VIEW_RESULT_RESERVED);
                                self.callable_type_depth -= 1;
                                return ERROR;
                            }
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
                self.callable_type_depth -= 1;
                self.t.mk(TyKind::Fn(params, result, waits, callable))
            }
            NodeKind::UnitType => UNIT,
            NodeKind::GroupType => self.ty_result(c[0], scoped_roots),
            NodeKind::TupleType => {
                let xs = c
                    .iter()
                    .map(|&x| {
                        let ty = self.ty_result(x, scoped_roots);
                        self.outside_vector(x, ty, "a tuple element")
                    })
                    .collect::<Vec<_>>();
                if xs.contains(&ERROR) {
                    return ERROR;
                }
                self.t.mk(TyKind::Tuple(xs))
            }
            NodeKind::TypePath => self.named_ty(n, &[]),
            NodeKind::TypeArguments if let Some(id) = self.p.held_send_type(self.at, c) => {
                self.t.mk(TyKind::Adt(id, vec![]))
            }
            NodeKind::TypeArguments
                if matches!(
                    self.p.target(self.at, c[0]),
                    Some(Target::Builtin(hirsel_core_syntax::sym::SIMD))
                ) =>
            {
                if c.len() != 3 {
                    self.refuse(
                        "SEM-11",
                        n,
                        "Simd takes an integer lane type and a literal lane count",
                    );
                    return ERROR;
                }
                let lane = self.ty_result(c[1], scoped_roots);
                let count = crate::decls::literal_length(self.p.item(self.at), c[2]);
                match crate::decls::vector_shape(&self.t, lane, count) {
                    Ok(shape) => {
                        self.versioned = true;
                        self.t.mk(TyKind::Vector(shape))
                    }
                    Err(message) => {
                        if lane != ERROR {
                            self.refuse("SEM-11", n, message);
                        }
                        ERROR
                    }
                }
            }
            NodeKind::TypeArguments if matches!(self.p.target(self.at, c[0]), Some(Target::Builtin(name)) if self.p.index.symbols.text(*name) == "Inline") =>
            {
                if c.len() != 3 {
                    let message = "Inline takes an element type and a length";
                    self.refuse_type_arguments(n, c.len() - 1, 2, message);
                    return ERROR;
                }
                let element = self.ty_result(c[1], scoped_roots);
                let element = self.outside_vector(c[1], element, "an array element");
                if element == ERROR {
                    return ERROR;
                }
                let count = match self.p.registered_inline_length(self.at, c[2]) {
                    Ok(count) => count,
                    Err(why) => {
                        if let Some((rule, message)) = why {
                            self.refuse(rule, c[2], message);
                        }
                        return ERROR;
                    }
                };
                let element_kind = kind(&self.p.decls, &self.t, element, &self.params);
                if element_kind == Kind::Scoped {
                    let shown = self.shown(element);
                    self.refuse_compared(
                        "OWN-19",
                        c[1],
                        shown,
                        "not scoped",
                        "a scoped value is not a collection element",
                    );
                    return ERROR;
                }
                if !matches!(element_kind, Kind::Plain | Kind::Own) {
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
                    let ty = self.ty_result(a, scoped_roots);
                    args.push((self.outside_vector(a, ty, "a type argument"), a));
                }
                if args.iter().any(|&(t, _)| t == ERROR) {
                    return ERROR;
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
                let ty = self.outside_vector(c[0], ty, "anchored to a region");
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
                    let message = "this type takes no type arguments";
                    self.refuse_type_arguments(path, args.len(), 0, message);
                    return ERROR;
                }
                None if name == hirsel_core_syntax::sym::SIMD => {
                    self.refuse(
                        "SEM-11",
                        path,
                        "Simd takes an integer lane type and a literal lane count",
                    );
                    return ERROR;
                }
                None => {
                    self.gap(path, "inline array types are not built");
                    return ERROR;
                }
            },
            Some(Target::Definition(q)) if p.is_core_never(q) => {
                if !args.is_empty() {
                    let message = "this type takes no type arguments";
                    self.refuse_type_arguments(path, args.len(), 0, message);
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
                        let message = "a type parameter takes no type arguments";
                        self.refuse_type_arguments(path, args.len(), 0, message);
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
            let message = format!(
                "`{name}` takes {want} type arguments, {} written",
                args.len()
            );
            self.refuse_type_arguments(path, args.len(), want, &message);
            return ERROR;
        }
        if let Some(marker) = crate::foreign::Marker::of(decl)
            && let [(arg, _)] = args
        {
            let module = p.modules[self.at.module as usize].id;
            if *arg != ERROR && !p.foreign_names().admits(p, &self.t, marker, *arg, module) {
                self.refuse(marker.rule(), path, marker.message());
                return ERROR;
            }
        }
        for (g, &(t, at)) in decl.generics.iter().zip(args) {
            if !decl.structural && crate::foreign::holds_raw_handle(&p.decls, &self.t, t) {
                self.refuse("FFI-6", at, "a raw handle cannot be this type argument");
                return ERROR;
            }
            if g.region {
                if !matches!(self.t.kind(t), TyKind::Region(_)) && t != ERROR {
                    self.refuse("MEM-6", at, "a region parameter takes an `in r` argument");
                    return ERROR;
                }
                continue;
            }
            if matches!(self.t.kind(t), TyKind::Region(_)) {
                let message = "a type parameter takes a type, not a region";
                self.refuse_compared("LANG-12", at, "region", "type", message);
                return ERROR;
            }
            if decl.role == Some(Role::Cell) && self.element_refused(at, t) {
                return ERROR;
            }
            if !decl.structural && kind(&p.decls, &self.t, t, &self.params) == Kind::Scoped {
                let a = self.shown(t);
                let message = "a scoped type is not a type argument of a generic other than Option and Result";
                self.refuse_compared("OWN-19", at, a, "not scoped", message);
                return ERROR;
            }
            let missing =
                crate::decls::missing_bounds(&p.decls, &self.t, t, &self.params, g.bounds);
            if missing != 0 {
                // LANG-12: a written type argument lacking its parameter's bound
                // (`Rc[HostFile]`) is refused at that argument.
                let rule = if matches!(
                    decl.role,
                    Some(Role::FixedSpan | Role::SeqSpan | Role::IndexedSpan | Role::SeqIndex)
                ) {
                    "MEM-12"
                } else {
                    "LANG-12"
                };
                let (a, b) = (self.shown(t), crate::decls::bound_names(missing));
                let message = format!("type argument is not {b}");
                self.refuse_compared(rule, at, a, b, message);
                return ERROR;
            }
        }
        let tys: Vec<TyId> = args.iter().map(|a| a.0).collect();
        if decl.pack_callable_carrier()
            && !matches!(
                self.t.kind(tys[0]),
                TyKind::Fn(.., Callable::Plain) | TyKind::Param(_) | TyKind::Error
            )
        {
            self.refuse(
                "PACK-14",
                path,
                "a code handle needs a named entry's function type",
            );
            return ERROR;
        }
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
    Float(u64),
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
    let scalar = match text.as_bytes() {
        [.., b'u', b'8'] => Scalar::U8,
        [.., b'u', b'1', b'6'] => Scalar::U16,
        [.., b'u', b'3', b'2'] => Scalar::U32,
        [.., b'u', b'6', b'4'] => Scalar::U64,
        [.., b'i', b'8'] => Scalar::I8,
        [.., b'i', b'1', b'6'] => Scalar::I16,
        [.., b'i', b'3', b'2'] => Scalar::I32,
        [.., b'i', b'6', b'4'] => Scalar::I64,
        _ => return (text, None),
    };
    (&text[..text.len() - scalar.name().len()], Some(scalar))
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
    let (radix, digits) = match digits.as_bytes() {
        [b'0', b'x', rest @ ..] => (16, rest),
        [b'0', b'o', rest @ ..] => (8, rest),
        [b'0', b'b', rest @ ..] => (2, rest),
        digits => (10, digits),
    };
    // At most 16 hexadecimal digits fit in u64. The byte length bounds the
    // digit count in every radix, including when the input has separators.
    if digits.len() <= 16 {
        magnitude_digits::<false>(radix, digits)
    } else {
        magnitude_digits::<true>(radix, digits)
    }
}

fn magnitude_digits<const CHECK_OVERFLOW: bool>(radix: u128, digits: &[u8]) -> Option<u128> {
    let mut value = 0u128;
    let mut any = false;
    for &byte in digits {
        let digit = match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            b'A'..=b'F' => byte - b'A' + 10,
            b'_' => continue,
            _ => return None,
        } as u128;
        if digit >= radix {
            return None;
        }
        value = if CHECK_OVERFLOW {
            value.checked_mul(radix)?.checked_add(digit)?
        } else {
            u128::from(value as u64 * radix as u64 + digit as u64)
        };
        any = true;
    }
    any.then_some(value)
}
/// Parse the digits directly in the type selected by LANG-3 and LANG-21.
pub(crate) fn parse_float_bits(s: Scalar, digits: &str, negative: bool) -> Option<u64> {
    let without_separators;
    let digits = if digits.contains('_') {
        without_separators = digits.replace('_', "");
        &without_separators
    } else {
        digits
    };
    match s {
        Scalar::F32 => {
            let x: f32 = digits.parse().ok()?;
            Some(float32_bits(if negative { -x } else { x }))
        }
        Scalar::F64 => {
            let x: f64 = digits.parse().ok()?;
            Some(float64_bits(if negative { -x } else { x }))
        }
        _ => None,
    }
}

pub(crate) fn float_negate_bits(s: Scalar, bits: u64) -> u64 {
    if s == Scalar::F32 {
        float32_bits(-f32::from_bits(bits as u32))
    } else {
        float64_bits(-f64::from_bits(bits))
    }
}

pub(crate) fn float_binary_bits(s: Scalar, op: &str, a: u64, b: u64) -> u64 {
    if s == Scalar::F32 {
        let (a, b) = (f32::from_bits(a as u32), f32::from_bits(b as u32));
        float32_bits(match op {
            "+" => a + b,
            "-" => a - b,
            "*" => a * b,
            _ => a / b,
        })
    } else {
        let (a, b) = (f64::from_bits(a), f64::from_bits(b));
        float64_bits(match op {
            "+" => a + b,
            "-" => a - b,
            "*" => a * b,
            _ => a / b,
        })
    }
}

// Use the canonical NaN of each width so constants do not depend on the host (SEM-2).
fn float32_bits(x: f32) -> u64 {
    if x.is_nan() {
        0x7fc0_0000
    } else {
        u64::from(x.to_bits())
    }
}

fn float64_bits(x: f64) -> u64 {
    if x.is_nan() {
        0x7ff8_0000_0000_0000
    } else {
        x.to_bits()
    }
}
