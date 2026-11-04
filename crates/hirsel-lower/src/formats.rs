use super::builder::{Builder, Value};
use super::*;
use hirsel_syntax::{ExprKind, FormatStep, StmtKind, UnaryOp};
use hirsel_tir::{Body, Storage};
use hirsel_types::{CallState, CallTarget, SourceRoot, TypeKind, UseKind};

impl Builder<'_, '_> {
    /// Retain source invocations and their order, including reserve and finish.
    /// Primitive formatters write into the destination; the private builder
    /// stays empty, so its finish has no backing allocation to freeze.
    pub(super) fn appended_format(
        &mut self,
        id: ExprId,
        bodies: &mut Vec<Option<Body>>,
    ) -> Result<Option<Option<Value>>> {
        let [plan] = self.decisions.calls.get(&id) else {
            return Ok(None);
        };
        let CallTarget::Definition(path) = &plan.target else {
            return Ok(None);
        };
        let text_append = path.0.package.name == "std"
            && path.0.package.version == "1"
            && path.0.modules == ["text"]
            && path.0.name == "append";
        if plan.state != CallState::Complete
            || !(text_append || path.is_core("bytes.append"))
            || plan.arguments.len() != 2
        {
            return Ok(None);
        }
        let Some(destination) = plan.arguments.iter().find(|a| {
            a.parameter
                .as_ref()
                .is_some_and(|(ordinal, _)| *ordinal == 0)
        }) else {
            return Ok(None);
        };
        let Some(formatted) = plan.arguments.iter().find(|a| {
            a.parameter
                .as_ref()
                .is_some_and(|(ordinal, _)| *ordinal == 1)
        }) else {
            return Ok(None);
        };
        let destination = destination.expression;
        let mut formatted = formatted.expression;
        let tree = &self.input.syntax.tree;
        if !text_append {
            while let ExprKind::Block {
                label: None,
                statements,
                tail: Some(tail),
            } = &tree.expressions[formatted.0 as usize].kind
            {
                if !statements.is_empty() {
                    break;
                }
                formatted = *tail;
            }
            let [conversion] = self.decisions.calls.get(&formatted) else {
                return Ok(None);
            };
            if !matches!(&conversion.target, CallTarget::Definition(path) if path.is_core("string.as_bytes"))
                || conversion.state != CallState::Complete
                || conversion.arguments.len() != 1
            {
                return Ok(None);
            }
            formatted = conversion.arguments[0].expression;
        }
        let mut format_expression = formatted;
        while !tree
            .formats
            .iter()
            .any(|f| f.expression == format_expression)
        {
            match &tree.expressions[format_expression.0 as usize].kind {
                ExprKind::Block {
                    label: None,
                    statements,
                    tail: Some(tail),
                } if statements.is_empty() => format_expression = *tail,
                _ => return Ok(None),
            }
        }
        let Some(format) = tree
            .formats
            .iter()
            .find(|f| f.expression == format_expression)
        else {
            return Ok(None);
        };
        // An interpolation or formatter can fail after writing a prefix. Only
        // an owned local byte list, destroyed without user code on failure,
        // admits those early writes. Caller storage and resource fields do not.
        let ExprKind::Unary {
            op: UnaryOp::Mut,
            value,
        } = tree.expressions[destination.0 as usize].kind
        else {
            return Ok(None);
        };
        if !matches!(tree.expressions[value.0 as usize].kind, ExprKind::Path(_)) {
            return Ok(None);
        }
        let source = self.source_place(value)?;
        let SourceRoot::Binding { owner, binding } = &source.root else {
            return Ok(None);
        };
        if !source.projections.is_empty() {
            return Ok(None);
        }
        let place = self.lookup(binding, owner, self.span(value)?)?;
        let root = self.root(place);
        if !self.owned[root.0 as usize]
            || self.body.locals[root.0 as usize].storage != Storage::Local
            || !self.body.places[place.0 as usize].projection.is_empty()
            || !self.types.row(self.ty(place)).modes.relation.is_global()
        {
            return Ok(None);
        }
        // An arena can catch an allocation failure before a later hole traps.
        // A global physical row proves heap placement, without caller bounds.
        // A custom formatter observes its private buffer. A call, index or
        // control expression in a hole could observe the destination between
        // appends. Keep those forms on the ordinary private-builder path.
        let mut pending = format.holes.clone();
        while let Some(hole) = pending.pop() {
            if !matches!(
                self.types.row(self.decision(hole)?.ty).kind,
                TypeKind::Primitive(_)
            ) {
                return Ok(None);
            }
            match &tree.expressions[hole.0 as usize].kind {
                ExprKind::Literal(_) | ExprKind::Path(_) => {}
                ExprKind::Unary {
                    op: UnaryOp::Neg | UnaryOp::Not | UnaryOp::BitNot,
                    value,
                } => pending.push(*value),
                ExprKind::Binary { left, right, .. } => pending.extend([*right, *left]),
                _ => return Ok(None),
            }
        }
        let ExprKind::Block { statements, .. } =
            &tree.expressions[format_expression.0 as usize].kind
        else {
            return Ok(None);
        };
        let mut redirects = Vec::new();
        let mut statements: Vec<_> = statements.iter().skip(1).copied().collect();
        while let Some(statement) = statements.pop() {
            let expression = match tree.statements[statement.0 as usize].kind {
                StmtKind::Expr(expression)
                | StmtKind::Let {
                    value: expression, ..
                } => expression,
                _ => return Ok(None),
            };
            let ExprKind::Call {
                callee,
                ref arguments,
            } = tree.expressions[expression.0 as usize].kind
            else {
                if let ExprKind::Block {
                    statements: ref nested,
                    tail: None,
                    ..
                } = tree.expressions[expression.0 as usize].kind
                {
                    statements.extend(nested.iter().copied());
                    continue;
                }
                return Ok(None);
            };
            match tree.expressions[callee.0 as usize].kind {
                ExprKind::Formatter(FormatStep::Append | FormatStep::AppendSpec(_) | FormatStep::Pad(_)) => {
                    redirects.push(arguments.last().unwrap().value);
                }
                ExprKind::Formatter(FormatStep::Mark) => redirects.push(arguments[0].value),
                ExprKind::Path(_) if self.decisions.calls.get(&expression).first().is_some_and(|p| {
                    matches!(&p.target, CallTarget::Definition(path) if path.is_core("list.reserve"))
                }) => redirects.push(arguments[0].value),
                _ => return Ok(None),
            }
        }
        let ExprKind::Call { callee, arguments } = tree.expressions[id.0 as usize].kind.clone()
        else {
            return Ok(None);
        };
        let previous = std::mem::take(&mut self.format_destinations);
        self.format_destinations
            .extend(redirects.into_iter().map(|id| {
                (
                    id,
                    Value {
                        place,
                        usage: UseKind::Mut,
                        checked_ty: None,
                    },
                )
            }));
        let result = self.ordinary_call(id, callee, arguments, bodies);
        self.format_destinations = previous;
        result.map(Some)
    }
}
