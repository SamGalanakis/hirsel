//! LANG-20 constants: declaration-only values evaluated in dependency order.
//! Values share one append-only field arena; no expression owns a child vector.
use crate::*;
use hirsel_core_ir::Ty;
use hirsel_core_layout::Scalar;
use hirsel_core_syntax::{NodeId, NodeKind, TokenKind};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Constant {
    pub path: DefPath,
    pub at: ItemRef,
    pub ty: TyId,
    pub value: Option<ConstValue>,
    expr: NodeId,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct ConstValue {
    pub ty: TyId,
    pub data: ConstData,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum ConstData {
    Bits(u64),
    Unit,
    Fields {
        start: u32,
        len: u32,
        case: Option<u32>,
    },
}
impl Program<'_> {
    pub(crate) fn register_constants(&mut self) {
        for m in self.library_modules..self.modules.len() {
            for i in 0..self.modules[m].syntax.items().len() {
                let at = ItemRef {
                    module: m as u32,
                    item: i as u32,
                };
                let item = self.item(at);
                if item.tree.node(item.root).kind != NodeKind::Constant || item.range.has_errors {
                    continue;
                }
                let name = item.symbol(item.tree.node(item.root).token.unwrap());
                let Some(definition) = self.index.definition(self.modules[m].id, name) else {
                    continue;
                };
                let kids = item.tree.children(item.root);
                let expr = *kids.last().unwrap();
                let ty = self.lower_ty(at, kids[kids.len() - 2], &[]);
                let id = self.constants.len() as u32;
                self.constant_ids[definition.0 as usize] = Some(id);
                self.constants.push(Constant {
                    path: self.index.definition_path(definition),
                    at,
                    ty,
                    value: None,
                    expr,
                });
            }
        }
    }
    pub(crate) fn evaluate_constants(&mut self) {
        let base = self.constants.base_len();
        let len = self.constants.len();
        if base == len {
            return;
        }
        let mut left = vec![0usize; len - base];
        let mut heads = vec![None; len - base];
        let mut reverse = vec![];
        for i in base..len {
            let at = self.constants[i].at;
            for use_ in &self.items[at.module as usize][at.item as usize]
                .resolution
                .names
            {
                if let hirsel_core_resolve::Target::Definition(id) = use_.target
                    && let Some(dep) = self.constant_ids[id.0 as usize]
                    && dep as usize >= base
                {
                    let dep = dep as usize;
                    left[i - base] += 1;
                    reverse.push((i, heads[dep - base]));
                    heads[dep - base] = Some(reverse.len() - 1);
                }
            }
        }
        let mut ready = (base..len)
            .filter(|&i| left[i - base] == 0)
            .collect::<VecDeque<_>>();
        let mut done = vec![false; len - base];
        while let Some(i) = ready.pop_front() {
            let c = &self.constants[i];
            let (at, expr, ty) = (c.at, c.expr, c.ty);
            let value = if ty == ERROR {
                None
            } else if self.kind(ty) != Kind::Plain {
                self.const_refuse(at, expr, "a constant must have a plain type");
                None
            } else {
                ConstCx {
                    p: self,
                    at,
                    number: String::new(),
                }
                .expr(expr, Some(ty), true)
            };
            self.constants[i].value = value;
            done[i - base] = true;
            let mut next = heads[i - base];
            while let Some(edge) = next {
                let (reader, tail) = reverse[edge];
                left[reader - base] -= 1;
                if left[reader - base] == 0 {
                    ready.push_back(reader);
                }
                next = tail;
            }
        }
        // Remaining vertices depend on a cycle. Follow dependency edges until a
        // repeated vertex identifies the actual cycle, without recursive DFS.
        let mut active = vec![false; len - base];
        let mut path = vec![];
        for start in base..len {
            if done[start - base] {
                continue;
            }
            path.clear();
            let mut i = start;
            while !done[i - base] && !active[i - base] {
                active[i - base] = true;
                path.push(i);
                let at = self.constants[i].at;
                i = self.items[at.module as usize][at.item as usize]
                    .resolution
                    .names
                    .iter()
                    .find_map(|u| {
                        let Target::Definition(id) = u.target else {
                            return None;
                        };
                        let dep = self.constant_ids[id.0 as usize]? as usize;
                        (dep >= base && !done[dep - base]).then_some(dep)
                    })
                    .unwrap();
            }
            if active[i - base] {
                let c = &self.constants[i];
                self.const_refuse(c.at, c.expr, "constant dependencies form a cycle");
            }
            for &i in &path {
                done[i - base] = true;
                active[i - base] = false;
            }
        }
    }
    /// LANG-7 constant heads and explicit projections read the published field.
    pub(crate) fn constant_field(
        &self,
        from: ModuleId,
        value: ConstValue,
        name: &str,
    ) -> Result<ConstValue, &'static str> {
        let ConstData::Fields {
            start,
            len,
            case: None,
        } = value.data
        else {
            return Err("LANG-11");
        };
        let index = match self.types.kind(value.ty) {
            TyKind::Tuple(_) => name.parse::<usize>().map_err(|_| "LANG-11")?,
            TyKind::Adt(d, _) => {
                let decl = &self.decls[d.index()];
                if decl.opaque && decl.site.module != from {
                    return Err("PROG-2");
                }
                match &decl.body {
                    DeclBody::Record(fs) => {
                        fs.iter().position(|f| f.name == name).ok_or("LANG-11")?
                    }
                    _ => return Err("LANG-11"),
                }
            }
            _ => return Err("LANG-11"),
        };
        if index < len as usize {
            Ok(self.constant_parts[start as usize + index])
        } else {
            Err("LANG-11")
        }
    }
    fn const_refuse(&mut self, at: ItemRef, n: NodeId, message: &str) {
        self.refusals
            .push(refusal("LANG-20", self.site(at, n), message));
    }
}
struct ConstCx<'p, 'a> {
    p: &'p mut Program<'a>,
    at: ItemRef,
    number: String,
}
impl ConstCx<'_, '_> {
    fn fail(&mut self, n: NodeId, message: &str) -> Option<ConstValue> {
        self.p.const_refuse(self.at, n, message);
        None
    }
    fn project(&mut self, n: NodeId, value: ConstValue, name: &str) -> Option<ConstValue> {
        match self
            .p
            .constant_field(self.p.modules[self.at.module as usize].id, value, name)
        {
            Ok(field) => Some(field),
            Err(rule) => {
                self.p.refusals.push(refusal(
                    rule,
                    self.p.site(self.at, n),
                    "constant field is absent or opaque",
                ));
                None
            }
        }
    }
    fn bits(&self, s: Scalar, bits: u64) -> ConstValue {
        ConstValue {
            ty: scalar(s),
            data: ConstData::Bits(bits),
        }
    }
    fn expr(&mut self, n: NodeId, want: Option<TyId>, active: bool) -> Option<ConstValue> {
        let item = self.p.item(self.at);
        let node = item.tree.node(n);
        let kids = item.tree.children(n);
        let text = node
            .token
            .map(|t| item.tree.token_text(item.text, t))
            .unwrap_or("");
        let value = match node.kind {
            NodeKind::Group => return self.expr(kids[0], want, active),
            NodeKind::Unit => ConstValue {
                ty: UNIT,
                data: ConstData::Unit,
            },
            NodeKind::Literal => self.literal(n, want, false)?,
            NodeKind::Path => {
                let Some(use_) = self.p.resolved(self.at, n) else {
                    return None;
                };
                match use_.target {
                    Target::Definition(def) if self.p.constant_ids[def.0 as usize].is_some() => {
                        let id = self.p.constant_ids[def.0 as usize]?;
                        let mut value = self.p.constants[id as usize].value?;
                        for token in &item.tree.tokens
                            [use_.projections.start as usize..use_.projections.end as usize]
                        {
                            if token.kind == TokenKind::Ident {
                                let field = self.project(
                                    n,
                                    value,
                                    self.p.index.symbols.text(token.symbol),
                                )?;
                                value = field;
                            }
                        }
                        value
                    }
                    Target::ContextualConstructor { .. } => {
                        self.construct(n, n, &[], want, active)?
                    }
                    Target::Missing => return None,
                    _ => return self.fail(n, "a constant expression reads only constants"),
                }
            }
            NodeKind::Unary => {
                if text == "copy" {
                    return self.expr(kids[0], want, active);
                }
                if text == "-" && item.tree.node(kids[0]).kind == NodeKind::Literal {
                    self.literal(kids[0], want, true)?
                } else {
                    let value = self.expr(kids[0], want, active)?;
                    self.unary(n, text, value, active)?
                }
            }
            NodeKind::Binary => self.binary(n, text, kids[0], kids[1], want, active)?,
            NodeKind::Tuple | NodeKind::Array | NodeKind::RepeatArray => {
                self.aggregate(n, want, active)?
            }
            NodeKind::Projection => {
                let value = self.expr(kids[0], None, active)?;
                self.project(n, value, text)?
            }
            NodeKind::Index => {
                let value = self.expr(kids[0], None, active)?;
                let index = self.expr(kids[1], Some(scalar(Scalar::U64)), active)?;
                let (
                    TyKind::Array(elem, count),
                    ConstData::Fields { start, .. },
                    ConstData::Bits(i),
                ) = (self.p.types.kind(value.ty), value.data, index.data)
                else {
                    return self.fail(n, "a constant index requires an Inline array");
                };
                if i >= *count {
                    if active {
                        return self.fail(n, "constant array index is out of bounds");
                    }
                    ConstValue {
                        ty: *elem,
                        data: ConstData::Bits(0),
                    }
                } else {
                    self.p.constant_parts[start as usize + i as usize]
                }
            }
            NodeKind::Call => self.construct(n, kids[0], &kids[1..], want, active)?,
            NodeKind::Closure => return self.gap(n, "callable constants are not built"),
            _ => return self.fail(n, "this expression is not permitted in a constant"),
        };
        if want.is_some_and(|want| value.ty != want) {
            return self.fail(n, "constant type differs from its annotation");
        }
        Some(value)
    }
    /// Reserve a contiguous row before visiting children, whose aggregates append
    /// their own rows to the same arena.
    fn fields(&mut self, ty: TyId, len: usize, case: Option<u32>) -> ConstValue {
        let start = self.p.constant_parts.len() as u32;
        for _ in 0..len {
            self.p.constant_parts.push(ConstValue {
                ty: UNIT,
                data: ConstData::Unit,
            });
        }
        ConstValue {
            ty,
            data: ConstData::Fields {
                start,
                len: len as u32,
                case,
            },
        }
    }
    fn set_field(&mut self, parent: ConstValue, index: usize, value: ConstValue) {
        let ConstData::Fields { start, .. } = parent.data else {
            unreachable!()
        };
        self.p.constant_parts[start as usize + index] = value;
    }
    fn aggregate(&mut self, n: NodeId, want: Option<TyId>, active: bool) -> Option<ConstValue> {
        let item = self.p.item(self.at);
        let kids = item.tree.children(n);
        match item.tree.node(n).kind {
            NodeKind::Tuple => {
                let value = self.fields(UNIT, kids.len(), None);
                let mut types = Vec::with_capacity(kids.len());
                for (i, &child) in kids.iter().enumerate() {
                    let hint = want.and_then(|t| match self.p.types.kind(t) {
                        TyKind::Tuple(xs) => xs.get(i).copied(),
                        _ => None,
                    });
                    let field = self.expr(child, hint, active)?;
                    types.push(field.ty);
                    self.set_field(value, i, field);
                }
                Some(ConstValue {
                    ty: self.p.types.mk(TyKind::Tuple(types)),
                    ..value
                })
            }
            NodeKind::Array | NodeKind::RepeatArray => {
                let repeat = item.tree.node(n).kind == NodeKind::RepeatArray;
                let hint = want.and_then(|t| match self.p.types.kind(t) {
                    TyKind::Array(t, _) => Some(*t),
                    _ => None,
                });
                let count = if repeat {
                    let token = item.tree.node(kids[1]).token?;
                    let (digits, _) =
                        crate::body::int_suffix(item.tree.token_text(item.text, token));
                    let count = crate::body::int_magnitude(digits)?;
                    let Ok(count) = u32::try_from(count) else {
                        return self.gap(n, "this constant array extent is not built");
                    };
                    count as usize
                } else {
                    kids.len()
                };
                let value = self.fields(UNIT, count, None);
                let mut elem = hint;
                for (i, &child) in kids[..if repeat { 1 } else { kids.len() }]
                    .iter()
                    .enumerate()
                {
                    let field = self.expr(child, elem, active)?;
                    elem = Some(field.ty);
                    if repeat {
                        for i in 0..count {
                            self.set_field(value, i, field);
                        }
                    } else {
                        self.set_field(value, i, field);
                    }
                }
                let Some(elem) = elem else {
                    return self.fail(n, "an empty constant array needs an element type");
                };
                Some(ConstValue {
                    ty: self.p.types.mk(TyKind::Array(elem, count as u64)),
                    ..value
                })
            }
            _ => unreachable!(),
        }
    }
    fn construct(
        &mut self,
        n: NodeId,
        head: NodeId,
        args: &[NodeId],
        want: Option<TyId>,
        active: bool,
    ) -> Option<ConstValue> {
        let item = self.p.item(self.at);
        let target = self.p.target(self.at, head).cloned()?;
        let (d, case, ty) = match target {
            Target::Definition(id) => {
                let Some(Def::Type(d)) = self.p.by_definition[id.0 as usize] else {
                    return self.fail(n, "a constant expression cannot call a function");
                };
                let ty = match want {
                    Some(ty) if matches!(self.p.types.kind(ty), TyKind::Adt(e, _) if *e == d) => ty,
                    _ if self.p.decls[d.index()].generics.is_empty() => {
                        self.p.types.mk(TyKind::Adt(d, vec![]))
                    }
                    _ => return self.gap(n, "constant constructor inference is not built"),
                };
                (d, None, ty)
            }
            Target::ContextualConstructor { name } => {
                let text = self.p.index.symbols.text(name);
                let ty = if let Some(ty) = want {
                    ty
                } else {
                    let from = self.p.modules[self.at.module as usize].id;
                    let mut found = None;
                    for id in self.p.index.constructor_candidates(from, name) {
                        let Some(Def::Type(d)) = self.p.by_definition[id.0 as usize] else {
                            continue;
                        };
                        if found.is_some_and(|other| other != d) {
                            return self.fail(n, "constant constructor is ambiguous");
                        }
                        found = Some(d);
                    }
                    let d = found?;
                    if !self.p.decls[d.index()].generics.is_empty() {
                        return self.gap(n, "constant constructor inference is not built");
                    }
                    self.p.types.mk(TyKind::Adt(d, vec![]))
                };
                let TyKind::Adt(d, _) = *self.p.types.kind(ty) else {
                    return self.fail(n, "constant constructor has another type");
                };
                let from = self.p.modules[self.at.module as usize].id;
                if !self.p.index.constructor_candidates(from, name).any(|id| matches!(self.p.by_definition[id.0 as usize], Some(Def::Type(e)) if e == d)) {
                    return self.fail(n, "constant constructor is not visible");
                }
                let case = match &self.p.decls[d.index()].body {
                    DeclBody::Variant(cs) => {
                        cs.iter().position(|c| c.name == text).map(|i| i as u32)
                    }
                    _ => None,
                };
                if case.is_none() {
                    return self.fail(n, "constant constructor has another type");
                }
                (d, case, ty)
            }
            Target::Missing => return None,
            _ => return self.fail(n, "a constant expression cannot call a function"),
        };
        let decl = &self.p.decls[d.index()];
        if decl.opaque && decl.site.module != self.p.modules[self.at.module as usize].id {
            self.p.refusals.push(refusal(
                "PROG-2",
                self.p.site(self.at, n),
                "constant constructor is opaque",
            ));
            return None;
        }
        if self.p.kind(ty) != Kind::Plain {
            return self.fail(n, "a constant constructor must be plain");
        }
        let (fields, labelled) = match (&decl.body, case) {
            (DeclBody::Record(fs), None) => (fs.clone(), true),
            (DeclBody::Variant(cs), Some(c)) => {
                (cs[c as usize].fields.clone(), cs[c as usize].labelled)
            }
            _ => return self.fail(n, "a constant constructor needs a record or variant"),
        };
        if fields.len() != args.len() {
            return self.fail(n, "constant constructor argument count differs");
        }
        let TyKind::Adt(_, type_args) = self.p.types.kind(ty) else {
            unreachable!()
        };
        let type_args = type_args.clone();
        let value = self.fields(ty, fields.len(), case);
        for (i, (&arg, field)) in args.iter().zip(&fields).enumerate() {
            let node = item.tree.node(arg);
            let child = if labelled {
                if node.kind != NodeKind::LabelledArgument
                    || item.tree.token_text(item.text, node.token?) != field.name
                {
                    self.p.refusals.push(refusal(
                        "LANG-9",
                        self.p.site(self.at, arg),
                        "constant fields are labelled in declaration order",
                    ));
                    return None;
                }
                item.tree.children(arg)[0]
            } else {
                if node.kind == NodeKind::LabelledArgument {
                    self.p.refusals.push(refusal(
                        "LANG-9",
                        self.p.site(self.at, arg),
                        "constant payload is positional",
                    ));
                    return None;
                }
                arg
            };
            let hint = subst(&mut self.p.types, field.ty, &type_args);
            let child = self.expr(child, Some(hint), active)?;
            self.set_field(value, i, child);
        }
        Some(value)
    }
    fn gap(&mut self, n: NodeId, what: &'static str) -> Option<ConstValue> {
        self.p.gaps.push(Gap::new(self.p.site(self.at, n), what));
        None
    }
    fn literal(&mut self, n: NodeId, want: Option<TyId>, negative: bool) -> Option<ConstValue> {
        let item = self.p.item(self.at);
        let token = item.tree.node(n).token?;
        let text = item.tree.token_text(item.text, token);
        let expected = want.and_then(|t| match self.p.types.kind(t) {
            TyKind::Scalar(s) => Some(*s),
            _ => None,
        });
        let (s, bits) = match item.tree.tokens[token.0 as usize].kind {
            TokenKind::KwTrue if !negative => (Scalar::Bool, 1),
            TokenKind::KwFalse if !negative => (Scalar::Bool, 0),
            TokenKind::Byte if !negative => {
                (Scalar::U8, u64::from(hirsel_core_syntax::byte_value(text)?))
            }
            TokenKind::Character if !negative => (
                Scalar::Char,
                u64::from(u32::from(hirsel_core_syntax::character_value(text)?)),
            ),
            TokenKind::Integer => {
                let (digits, suffix) = crate::body::int_suffix(text);
                let Some(s) = suffix.or(expected) else {
                    return self.fail(n, "a constant number needs an expected type");
                };
                let Some((signed, width)) = Ty::Scalar(s).int() else {
                    return self.fail(n, "an integer token requires an integer type");
                };
                let Some(magnitude) = crate::body::int_magnitude(digits) else {
                    return self.fail(n, "constant literal is outside its type");
                };
                let limit = if signed {
                    (1u128 << (width - 1)) - u128::from(!negative)
                } else {
                    (1u128 << width) - 1
                };
                if magnitude > limit || negative && !signed {
                    return self.fail(n, "constant literal is outside its type");
                }
                let integer = if negative {
                    -(magnitude as i128)
                } else {
                    magnitude as i128
                };
                (s, mask(s, integer as u64))
            }
            TokenKind::Float => {
                let (digits, suffix) = crate::body::float_suffix(text);
                let Some(s @ (Scalar::F32 | Scalar::F64)) = suffix.or(expected) else {
                    return self.fail(n, "a float token requires a float type");
                };
                self.number.clear();
                self.number.extend(digits.chars().filter(|&c| c != '_'));
                let x = if s == Scalar::F32 {
                    self.number.parse::<f32>().map(f64::from).ok()
                } else {
                    self.number.parse::<f64>().ok()
                };
                let Some(x) = x else {
                    return self.fail(n, "invalid constant float literal");
                };
                (s, crate::body::float_bits(s, if negative { -x } else { x }))
            }
            _ => return self.fail(n, "this literal is not a constant"),
        };
        if expected.is_some_and(|expected| expected != s) {
            return self.fail(n, "constant literal has another type");
        }
        Some(self.bits(s, bits))
    }
    fn unary(
        &mut self,
        n: NodeId,
        op: &str,
        value: ConstValue,
        active: bool,
    ) -> Option<ConstValue> {
        let (TyKind::Scalar(s), ConstData::Bits(bits)) = (self.p.types.kind(value.ty), value.data)
        else {
            return self.fail(n, "operator requires scalar constants");
        };
        let s = *s;
        let result = match op {
            "!" if s == Scalar::Bool => Some(u64::from(bits == 0)),
            "~" if Ty::Scalar(s).int().is_some() => Some(mask(s, !bits)),
            "-" if Ty::Scalar(s).int().is_some_and(|(signed, _)| signed) => {
                fit(s, -integer(s, bits))
            }
            "-" if s == Scalar::F32 => Some(crate::body::float_bits(
                s,
                -f64::from(f32::from_bits(bits as u32)),
            )),
            "-" if s == Scalar::F64 => Some(crate::body::float_bits(s, -f64::from_bits(bits))),
            _ => return self.fail(n, "operator does not apply to this constant type"),
        };
        if let Some(bits) = result {
            Some(self.bits(s, bits))
        } else if active {
            self.fail(n, "constant operation overflows")
        } else {
            Some(self.bits(s, 0))
        }
    }
    fn binary(
        &mut self,
        n: NodeId,
        op: &str,
        left: NodeId,
        right: NodeId,
        want: Option<TyId>,
        active: bool,
    ) -> Option<ConstValue> {
        let compare = matches!(op, "==" | "!=" | "<" | "<=" | ">" | ">=");
        let logical = matches!(op, "&&" | "||");
        let hint = if compare {
            None
        } else if logical {
            Some(scalar(Scalar::Bool))
        } else {
            want
        };
        let item = self.p.item(self.at);
        let unsuffixed = |n| {
            let node = item.tree.node(n);
            node.kind == NodeKind::Literal
                && node
                    .token
                    .is_some_and(|t| match item.tree.tokens[t.0 as usize].kind {
                        TokenKind::Integer => {
                            crate::body::int_suffix(item.tree.token_text(item.text, t))
                                .1
                                .is_none()
                        }
                        TokenKind::Float => {
                            crate::body::float_suffix(item.tree.token_text(item.text, t))
                                .1
                                .is_none()
                        }
                        _ => false,
                    })
        };
        let (a, b) = if hint.is_none() && unsuffixed(left) && !unsuffixed(right) {
            let b = self.expr(right, None, active)?;
            (self.expr(left, Some(b.ty), active)?, b)
        } else {
            let a = self.expr(left, hint, active)?;
            let rhs_active = active
                && !(op == "&&" && matches!(a.data, ConstData::Bits(0))
                    || op == "||" && matches!(a.data, ConstData::Bits(1)));
            let b = self.expr(right, Some(a.ty), rhs_active)?;
            (a, b)
        };
        let (TyKind::Scalar(s), ConstData::Bits(a), ConstData::Bits(b)) =
            (self.p.types.kind(a.ty), a.data, b.data)
        else {
            return self.gap(n, "aggregate constant comparisons are not built");
        };
        let s = *s;
        if compare {
            if crate::decls::has_bounds(&self.p.decls, &self.p.types, scalar(s), &[])
                & if matches!(op, "==" | "!=") {
                    crate::KEY
                } else {
                    crate::ORDERED
                }
                == 0
            {
                return self.fail(n, "this constant type does not support comparison");
            }
            let order = if Ty::Scalar(s).int().is_some() {
                integer(s, a).cmp(&integer(s, b))
            } else {
                a.cmp(&b)
            };
            let yes = match op {
                "==" => order.is_eq(),
                "!=" => !order.is_eq(),
                "<" => order.is_lt(),
                "<=" => !order.is_gt(),
                ">" => order.is_gt(),
                _ => !order.is_lt(),
            };
            return Some(self.bits(Scalar::Bool, u64::from(yes)));
        }
        if logical && s == Scalar::Bool {
            return Some(self.bits(
                s,
                u64::from(if op == "&&" {
                    a != 0 && b != 0
                } else {
                    a != 0 || b != 0
                }),
            ));
        }
        if Ty::Scalar(s).is_float() && matches!(op, "+" | "-" | "*" | "/") {
            let x = if s == Scalar::F32 {
                f64::from(f32::from_bits(a as u32))
            } else {
                f64::from_bits(a)
            };
            let y = if s == Scalar::F32 {
                f64::from(f32::from_bits(b as u32))
            } else {
                f64::from_bits(b)
            };
            let z = if s == Scalar::F32 {
                let (x, y) = (x as f32, y as f32);
                f64::from(match op {
                    "+" => x + y,
                    "-" => x - y,
                    "*" => x * y,
                    _ => x / y,
                })
            } else {
                match op {
                    "+" => x + y,
                    "-" => x - y,
                    "*" => x * y,
                    _ => x / y,
                }
            };
            return Some(self.bits(s, crate::body::float_bits(s, z)));
        }
        if Ty::Scalar(s).int().is_none() {
            return self.fail(n, "operator does not apply to this constant type");
        }
        let (x, y) = (integer(s, a), integer(s, b));
        let width = s.size() * 8;
        let bits = match op {
            "+" => x.checked_add(y).and_then(|v| fit(s, v)),
            "-" => x.checked_sub(y).and_then(|v| fit(s, v)),
            "*" => x.checked_mul(y).and_then(|v| fit(s, v)),
            "/" => x.checked_div(y).and_then(|v| fit(s, v)),
            "%" => x.checked_rem(y).map(|v| mask(s, v as u64)),
            "<<" if (0..i128::from(width)).contains(&y) => Some(mask(s, a << y)),
            ">>" if (0..i128::from(width)).contains(&y) => Some(mask(s, (x >> y) as u64)),
            "&" => Some(a & b),
            "|" => Some(a | b),
            "^" => Some(a ^ b),
            "<<" | ">>" => None,
            _ => return self.fail(n, "operator is not permitted in a constant"),
        };
        if let Some(bits) = bits {
            Some(self.bits(s, bits))
        } else if active {
            self.fail(n, "constant operation fails")
        } else {
            Some(self.bits(s, 0))
        }
    }
}
fn mask(s: Scalar, x: u64) -> u64 {
    x & (u64::MAX >> (64 - s.size() * 8))
}
fn integer(s: Scalar, x: u64) -> i128 {
    match Ty::Scalar(s).int() {
        Some((true, w)) => i128::from(((x << (64 - w)) as i64) >> (64 - w)),
        _ => i128::from(x),
    }
}
fn fit(s: Scalar, x: i128) -> Option<u64> {
    let (signed, w) = Ty::Scalar(s).int()?;
    let (lo, hi) = if signed {
        (-(1i128 << (w - 1)), (1i128 << (w - 1)) - 1)
    } else {
        (0, (1i128 << w) - 1)
    };
    (lo..=hi).contains(&x).then(|| mask(s, x as u64))
}
