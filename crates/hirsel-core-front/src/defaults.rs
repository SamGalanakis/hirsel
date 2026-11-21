//! SEM-10 field literals are checked against their written type, without evaluating
//! a body or generating a value. Construction continues to require every field.
use crate::body::{float_suffix, int_magnitude, int_suffix};
use crate::types::{ERROR, TyKind, scalar};
use crate::{ItemRef, Program, canonical, refusal};
use hirsel_core_ir::Ty;
use hirsel_core_layout::Scalar;
use hirsel_core_syntax::{NodeId, NodeKind, TokenKind};

impl Program<'_> {
    pub(crate) fn check_field_default(&mut self, at: ItemRef, expr: NodeId, want: crate::TyId) {
        if want == ERROR {
            return;
        }
        let item = self.item(at);
        let negative = item.tree.node(expr).kind == NodeKind::Unary;
        let n = if negative {
            item.tree.children(expr)[0]
        } else {
            expr
        };
        let token = item
            .tree
            .node(n)
            .token
            .expect("a parsed field default is a literal");
        let text = item.tree.token_text(item.text, token);
        let kind = item.tree.tokens[token.0 as usize].kind;
        let found = match kind {
            TokenKind::KwTrue | TokenKind::KwFalse => scalar(Scalar::Bool),
            TokenKind::Byte => scalar(Scalar::U8),
            TokenKind::Character => scalar(Scalar::Char),
            TokenKind::String => {
                let Some(d) = self.core_decl("String") else {
                    return;
                };
                self.types.intern(TyKind::Adt(d, vec![]))
            }
            TokenKind::Bytes => {
                let Some(list) = self.core_decl("List") else {
                    return;
                };
                self.types
                    .intern(TyKind::Adt(list, vec![scalar(Scalar::U8)]))
            }
            TokenKind::Integer | TokenKind::Float => {
                let (digits, suffix) = if kind == TokenKind::Integer {
                    int_suffix(text)
                } else {
                    float_suffix(text)
                };
                let expected = match self.types.kind(want) {
                    TyKind::Scalar(s)
                        if Ty::Scalar(*s).int().is_some()
                            || matches!(s, Scalar::F32 | Scalar::F64) =>
                    {
                        Some(*s)
                    }
                    _ => None,
                };
                let Some(s) = suffix.or(expected) else {
                    self.default_literal_refusal(
                        at,
                        n,
                        want,
                        negative,
                        "LANG-11",
                        "a number needs an expected numeric type",
                    );
                    return;
                };
                if kind == TokenKind::Integer {
                    let Some((signed, width)) = Ty::Scalar(s).int() else {
                        self.default_literal_refusal(
                            at,
                            n,
                            scalar(s),
                            negative,
                            "LANG-3",
                            "an integer literal requires an integer type",
                        );
                        return;
                    };
                    let limit = if signed {
                        (1u128 << (width - 1)) - u128::from(!negative)
                    } else {
                        (1u128 << width) - 1
                    };
                    if int_magnitude(digits).is_none_or(|m| m > limit) {
                        self.default_literal_refusal(
                            at,
                            n,
                            scalar(s),
                            negative,
                            "LANG-3",
                            "the literal does not fit its type",
                        );
                        return;
                    }
                    if negative && !signed {
                        let mut site = self.site(at, expr);
                        site.span = item.token_span(item.tree.node(expr).token.unwrap());
                        self.refusals.push(
                            refusal(
                                "LANG-21",
                                site,
                                "unary minus requires a signed integer or float",
                            )
                            .with_comparison(s.name(), "an operand type of `-`"),
                        );
                        return;
                    }
                } else if !matches!(s, Scalar::F32 | Scalar::F64) {
                    self.default_literal_refusal(
                        at,
                        n,
                        scalar(s),
                        false,
                        "LANG-3",
                        "a fractional literal requires a float type",
                    );
                    return;
                }
                scalar(s)
            }
            _ => unreachable!("field default syntax admits literals only"),
        };
        if found != want {
            self.refusals.push(
                refusal(
                    "LANG-11",
                    self.site(at, expr),
                    "the field default differs from its declared type",
                )
                .with_comparison(
                    canonical(&self.decls, &self.types, found),
                    canonical(&self.decls, &self.types, want),
                ),
            );
        }
    }

    fn default_literal_refusal(
        &mut self,
        at: ItemRef,
        n: NodeId,
        ty: crate::TyId,
        negative: bool,
        rule: &str,
        message: &str,
    ) {
        let item = self.item(at);
        let text = item
            .tree
            .token_text(item.text, item.tree.node(n).token.unwrap());
        self.refusals
            .push(refusal(rule, self.site(at, n), message).with_comparison(
                format!("{}{text}", if negative { "-" } else { "" }),
                canonical(&self.decls, &self.types, ty),
            ));
    }
}
