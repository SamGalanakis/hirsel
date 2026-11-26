//! SEM-9 named lengths: an `Inline` length may name a manifest integer constant whose
//! initialiser is built from integer literals, operators and other such constants.
//! Types are lowered before constants are evaluated, so a header that names one
//! evaluates it then, once, by the rules of LANG-20; nothing here reads a layout.
use super::*;
use hirsel_core_resolve::DefinitionKind;

/// Why a constant cannot be an `Inline` length.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum LengthWhy {
    /// It calls a function or reads a computed constant: its value is known only after
    /// checking, and no type reads it (LANG-20).
    Computed,
    /// It reads `target`: a type is the same on every target.
    Target,
    /// Its type is not an integer type.
    NotInteger,
    /// Its initialiser holds more than integer literals, operators and other length
    /// constants: a `bytes[T]`, a projection, an index or a construction.
    Shape,
    /// Its own evaluation is refused (a cycle or a failure), and that refusal stands.
    Refused,
}

impl LengthWhy {
    fn message(self) -> Option<&'static str> {
        Some(match self {
            Self::Computed => "its initialiser calls a function, so it is computed after checking",
            Self::Target => "its value depends on `target`, and a type is the same on every target",
            Self::NotInteger => "its type is not an integer type",
            Self::Shape => {
                "its initialiser holds more than integer literals, operators and other \
                 length constants"
            }
            Self::Refused => return None,
        })
    }
}

/// A refusal of a length at the type's length argument; `None` when it is refused
/// elsewhere already (a missing name, a refused constant).
pub(crate) type LengthRefusal = Option<(&'static str, String)>;

const LENGTH: &str = "an Inline length is an integer literal or the name of a length constant";

impl Program<'_> {
    /// The length argument `n` of an `Inline` type in item `at`, at any time: a header
    /// lowered before constants are registered evaluates what it names.
    pub(crate) fn inline_length(&mut self, at: ItemRef, n: NodeId) -> Result<u64, LengthRefusal> {
        let Some(def) = self.length_name(at, n)? else {
            return literal(self.item(at), n);
        };
        let length = self.length_constant(def);
        self.length_count(def, length)
    }

    /// The same, read from the registered constants, for a body checked after them.
    pub(crate) fn registered_inline_length(
        &self,
        at: ItemRef,
        n: NodeId,
    ) -> Result<u64, LengthRefusal> {
        let Some(def) = self.length_name(at, n)? else {
            return literal(self.item(at), n);
        };
        let Some(id) = self.constant_ids[def.0 as usize] else {
            return Err(None);
        };
        let c = &self.constants[id as usize];
        let length = c.length.and_then(|()| c.value.ok_or(LengthWhy::Refused));
        self.length_count(def, length)
    }

    /// The constant a length names, or `None` for a literal.
    fn length_name(&self, at: ItemRef, n: NodeId) -> Result<Option<DefinitionId>, LengthRefusal> {
        let item = self.item(at);
        match item.tree.node(n).kind {
            NodeKind::Literal => return Ok(None),
            NodeKind::TypePath => {}
            _ => return Err(Some(("SEM-9", LENGTH.into()))),
        }
        let Some(use_) = self.resolved(at, n) else {
            return Err(Some(("SEM-9", LENGTH.into())));
        };
        match use_.target {
            Target::Missing => Err(None),
            Target::Definition(def)
                if self.index.get(def).kind == DefinitionKind::Constant
                    && use_.projections.is_empty() =>
            {
                Ok(Some(def))
            }
            Target::Definition(def) if self.index.get(def).kind == DefinitionKind::Constant => {
                Err(Some((
                    "SEM-9",
                    format!("{LENGTH}; `{}` names a part of a constant", text(item, n)),
                )))
            }
            _ => Err(Some((
                "SEM-9",
                format!("{LENGTH}; `{}` is not a constant", text(item, n)),
            ))),
        }
    }

    fn length_count(
        &self,
        def: DefinitionId,
        length: Result<ConstValue, LengthWhy>,
    ) -> Result<u64, LengthRefusal> {
        let name = self.index.symbols.text(self.index.get(def).name);
        let value = match length {
            Ok(value) => value,
            Err(why) => {
                return Err(why.message().map(|why| {
                    (
                        "SEM-9",
                        format!("constant `{name}` cannot be an Inline length: {why}"),
                    )
                }));
            }
        };
        let (TyKind::Scalar(s), ConstData::Bits(bits)) = (self.types.kind(value.ty), value.data)
        else {
            return Err(None);
        };
        u64::try_from(integer(*s, bits)).map_err(|_| {
            Some((
                "SEM-9",
                format!("constant `{name}` cannot be an Inline length: it is negative"),
            ))
        })
    }

    /// The value of constant `def` if an `Inline` length may name it, evaluated once.
    pub(super) fn length_constant(&mut self, def: DefinitionId) -> Result<ConstValue, LengthWhy> {
        if let Some(id) = self.constant_ids[def.0 as usize]
            && (id as usize) < self.constants.base_len()
        {
            let c = &self.constants[id as usize];
            return c.length.and_then(|()| c.value.ok_or(LengthWhy::Refused));
        }
        match self.length_constants.get(&def) {
            Some(Some(known)) => return *known,
            // A constant whose length needs its own value: the cycle is refused as a
            // constant's (LANG-20).
            Some(None) => return Err(LengthWhy::Refused),
            None => {}
        }
        self.length_constants.insert(def, None);
        let known = self.evaluate_length(def);
        self.length_constants.insert(def, Some(known));
        known
    }

    fn evaluate_length(&mut self, def: DefinitionId) -> Result<ConstValue, LengthWhy> {
        let site = self.index.get(def).site;
        let Some(module) = self
            .modules
            .iter()
            .position(|m| m.id == site.module && Some(m.source) == site.source)
        else {
            return Err(LengthWhy::Refused);
        };
        let at = ItemRef {
            module: module as u32,
            item: site.item,
        };
        let item = self.item(at);
        if item.range.has_errors || item.tree.node(item.root).kind != NodeKind::Constant {
            return Err(LengthWhy::Refused);
        }
        let kids = item.tree.children(item.root);
        let (expr, initialised) = targets::initialiser(item.tree, kids);
        if !initialised {
            return Err(LengthWhy::Target);
        }
        let type_node = kids[kids.len() - 2];
        // Only a builtin type is lowered this early, so lowering refuses nothing.
        if !matches!(
            self.resolved(at, type_node).map(|u| u.target),
            Some(Target::Builtin(_))
        ) {
            return Err(LengthWhy::NotInteger);
        }
        let ty = self.lower_ty(at, type_node, &[]);
        if !matches!(self.types.kind(ty), TyKind::Scalar(s) if Ty::Scalar(*s).int().is_some()) {
            return Err(LengthWhy::NotInteger);
        }
        self.length_shape(at, expr)?;
        let (refusals, gaps, parts) = (
            self.refusals.len(),
            self.gaps.len(),
            self.constant_parts.len(),
        );
        let mut cx = ConstCx {
            p: self,
            at,
            ascribing: false,
            target: None,
            targeted: false,
            early: true,
        };
        let value = cx.expr(expr, Some(ty), true);
        // The constant's own evaluation reports any refusal, once.
        self.refusals.truncate(refusals);
        self.gaps.truncate(gaps);
        self.constant_parts.truncate(parts);
        value.ok_or(LengthWhy::Refused)
    }

    /// Whether initialiser `expr` holds only integer literals, operators and names of
    /// other length constants, evaluating each constant it names first.
    fn length_shape(&mut self, at: ItemRef, expr: NodeId) -> Result<(), LengthWhy> {
        let mut stack = vec![expr];
        while let Some(n) = stack.pop() {
            let item = self.item(at);
            let node = item.tree.node(n);
            match node.kind {
                NodeKind::Literal => {
                    let token = node.token.ok_or(LengthWhy::Shape)?;
                    if item.tree.tokens[token.0 as usize].kind != TokenKind::Integer {
                        return Err(LengthWhy::Shape);
                    }
                }
                NodeKind::Group | NodeKind::Unary | NodeKind::Binary => {
                    stack.extend(item.tree.children(n).iter().copied());
                }
                NodeKind::Path => match self.resolved(at, n).map(|u| (u.target, u.projections)) {
                    Some((Target::Definition(d), projections))
                        if self.index.get(d).kind == DefinitionKind::Constant =>
                    {
                        if !projections.is_empty() {
                            return Err(LengthWhy::Shape);
                        }
                        match self.length_constant(d) {
                            Ok(_) => {}
                            Err(LengthWhy::NotInteger) => return Err(LengthWhy::Shape),
                            Err(why) => return Err(why),
                        }
                    }
                    Some((Target::Definition(d), _))
                        if self.index.get(d).kind == DefinitionKind::Function =>
                    {
                        return Err(LengthWhy::Computed);
                    }
                    _ => return Err(LengthWhy::Shape),
                },
                NodeKind::Call => {
                    let head = item.tree.children(n)[0];
                    let constructs = match self.resolved(at, head).map(|u| u.target) {
                        Some(Target::Definition(d)) => {
                            self.index.get(d).kind == DefinitionKind::Type
                        }
                        Some(Target::ContextualConstructor { .. }) => true,
                        _ => false,
                    };
                    return Err(if constructs {
                        LengthWhy::Shape
                    } else {
                        LengthWhy::Computed
                    });
                }
                _ => return Err(LengthWhy::Shape),
            }
        }
        Ok(())
    }
}

/// A length's name as written, `a.N`.
fn text(item: hirsel_core_syntax::Item<'_>, n: NodeId) -> String {
    item.tree
        .node(n)
        .origin
        .tokens
        .clone()
        .filter(|&t| item.tree.tokens[t as usize].kind == TokenKind::Ident)
        .map(|t| {
            item.tree
                .token_text(item.text, hirsel_core_syntax::TokenId(t))
        })
        .collect::<Vec<_>>()
        .join(".")
}

fn literal(item: hirsel_core_syntax::Item<'_>, n: NodeId) -> Result<u64, LengthRefusal> {
    crate::decls::literal_length(item, n).ok_or_else(|| {
        Some((
            "LANG-3",
            "an Inline length literal fits its integer type and u64".into(),
        ))
    })
}
