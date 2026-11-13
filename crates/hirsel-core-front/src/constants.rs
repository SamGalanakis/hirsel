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
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct ConstValue {
    pub ty: TyId,
    pub data: ConstData,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
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
        let mut left = vec![0usize; len];
        let mut heads = vec![None; len];
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
                    left[i] += 1;
                    reverse.push((i, heads[dep]));
                    heads[dep] = Some(reverse.len() - 1);
                }
            }
        }
        let mut ready = (base..len)
            .filter(|&i| left[i] == 0)
            .collect::<VecDeque<_>>();
        let mut done = vec![false; len];
        done[..base].fill(true);
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
            done[i] = true;
            let mut next = heads[i];
            while let Some(edge) = next {
                let (reader, tail) = reverse[edge];
                left[reader] -= 1;
                if left[reader] == 0 {
                    ready.push_back(reader);
                }
                next = tail;
            }
        }
        // Remaining vertices depend on a cycle. Follow dependency edges until a
        // repeated vertex identifies the actual cycle, without recursive DFS.
        let mut active = vec![false; len];
        let mut path = vec![];
        for start in base..len {
            if done[start] {
                continue;
            }
            path.clear();
            let mut i = start;
            while !done[i] && !active[i] {
                active[i] = true;
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
                        (!done[dep]).then_some(dep)
                    })
                    .unwrap();
            }
            if active[i] {
                let c = &self.constants[i];
                self.const_refuse(c.at, c.expr, "constant dependencies form a cycle");
            }
            for &i in &path {
                done[i] = true;
                active[i] = false;
            }
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
                let hirsel_core_resolve::Target::Definition(def) = use_.target else {
                    if use_.target == Target::Missing {
                        return None;
                    }
                    if matches!(use_.target, Target::ContextualConstructor { .. }) {
                        return self.gap(n, "aggregate constants are not built");
                    }
                    return self.fail(n, "a constant expression reads only constants");
                };
                let Some(id) = self.p.constant_ids[def.0 as usize] else {
                    return self.fail(n, "a constant expression reads only constants");
                };
                if !use_.projections.is_empty() {
                    return self.gap(n, "constant field projections are not built");
                }
                self.p.constants[id as usize].value?
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
            NodeKind::Tuple
            | NodeKind::Array
            | NodeKind::RepeatArray
            | NodeKind::Projection
            | NodeKind::Index => {
                return self.gap(n, "aggregate constants are not built");
            }
            NodeKind::Call => {
                let constructor = match self.p.target(self.at, kids[0]) {
                    Some(Target::Definition(id)) => {
                        matches!(self.p.by_definition[id.0 as usize], Some(Def::Type(_)))
                    }
                    Some(Target::ContextualConstructor { .. }) => true,
                    _ => false,
                };
                return if constructor {
                    self.gap(n, "aggregate constants are not built")
                } else {
                    self.fail(n, "a constant expression cannot call a function")
                };
            }
            NodeKind::Closure => return self.gap(n, "callable constants are not built"),
            _ => return self.fail(n, "this expression is not permitted in a constant"),
        };
        if want.is_some_and(|want| value.ty != want) {
            return self.fail(n, "constant type differs from its annotation");
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
            "+" | "-" | "*" | "/" | "%" | "<<" | ">>" => None,
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
