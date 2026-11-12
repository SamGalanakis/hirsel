//! F-string steps (SEM-3). The parser desugars an f-string into a block whose
//! steps call `ExprKind::Formatter` callees; each becomes an ordinary call of
//! std@1's formatting protocol, so later phases see only ordinary calls.
use super::*;

use hirsel_types::conventions::{Convention, FormatPrimitive};

/// Integers and floats pad on the right by default, other text on the left.
fn numeric(primitive: Primitive) -> bool {
    !matches!(
        primitive,
        Primitive::String | Primitive::Bool | Primitive::Char
    )
}
impl Checker<'_, '_> {
    /// An append checks its value first, with no expected type, because the
    /// value's type selects the operation; a mark is `core list.len`, a pad
    /// is selected by its hole's type, and the finish is `text.finish_valid`.
    pub(super) fn start_format_step(
        &mut self,
        id: ast::ExprId,
        callee: ast::ExprId,
        step: ast::FormatStep,
        arguments: &[ast::Argument],
        expected: Option<TypeId>,
        frame: usize,
    ) -> Result<(), QueryError> {
        match step {
            ast::FormatStep::Append | ast::FormatStep::AppendSpec(_) => {
                self.work
                    .push(Work::AfterChild(After::FormatAppend(id, expected, frame)));
                self.work.push(Work::Enter(arguments[0].value, None, frame));
                Ok(())
            }
            ast::FormatStep::Mark => self.format_call(
                id,
                callee,
                ast::DefPath::core("list.len"),
                vec![0],
                expected,
                frame,
                0,
            ),
            ast::FormatStep::Pad(hole) => {
                let ty = self.value(hole);
                let primitive = match self.kind(ty) {
                    TypeKind::Primitive(p) if Convention::append(p).is_some() => Some(p),
                    TypeKind::Named { definition, .. }
                        if self.format_association(&definition)?.is_some() =>
                    {
                        None
                    }
                    // The hole's append reported its type.
                    _ => return self.recover_format(id, callee, arguments, expected, frame, 0),
                };
                let pad = Convention::Pad {
                    numeric: primitive.is_some_and(numeric),
                };
                self.format_call(
                    id,
                    callee,
                    pad.path(),
                    (0..arguments.len()).collect(),
                    expected,
                    frame,
                    0,
                )
            }
            ast::FormatStep::Finish => {
                // Only the compiler-created builder can reach this intrinsic.
                // User @format functions receive arbitrary bytes and therefore
                // retain finish_valid's checked UTF-8 invariant.
                let trusted = self.format_holes.get(&id).is_some_and(|holes| {
                    holes.iter().all(|hole| match self.kind(self.value(*hole)) {
                        TypeKind::Primitive(p) => Convention::append(p).is_some(),
                        _ => false,
                    })
                });
                let checked = Convention::FinishValid.path();
                // Even an empty f-string still requires the std dependency.
                let operation = if trusted && self.snapshot(&checked).is_ok() {
                    ast::DefPath::core("$intrinsic.TextFinish")
                } else {
                    checked
                };
                self.format_call(id, callee, operation, vec![0], expected, frame, 0)
            }
        }
    }
    /// The function a nominal type's `@format` association names, if any.
    fn format_association(
        &mut self,
        definition: &DefId,
    ) -> Result<Option<ast::DefPath>, QueryError> {
        Ok(match &self.snapshot(definition)?.kind {
            SignatureKind::Type(nominal) => nominal.format.clone(),
            _ => None,
        })
    }
    pub(super) fn format_append(
        &mut self,
        id: ast::ExprId,
        expected: Option<TypeId>,
        frame: usize,
    ) -> Result<(), QueryError> {
        let ast::ExprKind::Call { callee, arguments } = self.input.syntax.tree.expressions
            [id.0 as usize]
            .kind
            .clone()
        else {
            unreachable!()
        };
        let value = arguments[0].value;
        let ty = self.value(value);
        if self.invalid(ty) {
            return self.recover_format(id, callee, &arguments, expected, frame, 1);
        }
        let spec = match self.input.syntax.tree.expressions[callee.0 as usize].kind {
            ast::ExprKind::Formatter(ast::FormatStep::AppendSpec(spec)) => Some(spec),
            _ => None,
        };
        let primitive = match self.kind(ty) {
            TypeKind::Primitive(p) => FormatPrimitive::new(p),
            _ => None,
        };
        let association = match self.kind(ty) {
            TypeKind::Named { definition, .. } => self.format_association(&definition)?,
            _ => None,
        };
        let (operation, ordinals) = match (primitive, association, spec) {
            (Some(primitive), _, None) => (
                Convention::Append(primitive).path(),
                Convention::append_ordinals(primitive).to_vec(),
            ),
            (Some(primitive), _, Some(_)) if primitive.primitive() == Primitive::Char => {
                self.report(
                    const { RuleId::of("SEM-3") },
                    self.span(value),
                    "a char hole takes only fill, alignment and width; precision, sign, `#`, \
                     `0` and radix formats apply to bool, integers, floats and String",
                );
                return self.recover_format(id, callee, &arguments, expected, frame, 1);
            }
            (Some(format), _, Some(spec)) => {
                let primitive = format.primitive();
                let integer =
                    numeric(primitive) && !matches!(primitive, Primitive::F32 | Primitive::F64);
                if spec.kind != 0 && !integer {
                    self.report(
                        const { RuleId::of("SEM-3") },
                        self.span(value),
                        &format!(
                            "format type `{}` requires an integer; `{}` does not support it",
                            char::from_u32(spec.kind as u32).unwrap_or('?'),
                            self.type_spelling(ty)
                        ),
                    );
                    return self.recover_format(id, callee, &arguments, expected, frame, 1);
                }
                (
                    Convention::append_spec(primitive)
                        .expect("a char spec was refused above")
                        .path(),
                    (0..arguments.len()).collect(),
                )
            }
            (None, Some(function), None) => (function, vec![0, 1]),
            (None, Some(_), Some(_)) => {
                self.report(
                    const { RuleId::of("SEM-3") },
                    self.span(value),
                    &format!(
                        "`{}` formats through its @format function, which takes only fill, \
                         alignment and width; precision, sign, `#`, `0` and radix formats \
                         apply to bool, integers, floats and String",
                        self.type_spelling(ty)
                    ),
                );
                return self.recover_format(id, callee, &arguments, expected, frame, 1);
            }
            (None, None, _) => {
                let named = matches!(self.kind(ty), TypeKind::Named { .. });
                self.report(
                    const { RuleId::of("SEM-3") },
                    self.span(value),
                    &format!(
                        "an f-string hole of type `{}` has no text formatting; String, bool, \
                         char, integers and floats format, {}",
                        self.type_spelling(ty),
                        if named {
                            "and a declared type formats when it names its function with \
                             `@format(function)`"
                        } else {
                            "so convert the value to one of them first"
                        }
                    ),
                );
                return self.recover_format(id, callee, &arguments, expected, frame, 1);
            }
        };
        self.format_call(id, callee, operation, ordinals, expected, frame, 1)
    }
    /// The callee becomes a named reference to `operation`, as a resolved path
    /// would be. The first `checked` written arguments, an append's value, are
    /// already checked.
    #[allow(clippy::too_many_arguments)]
    fn format_call(
        &mut self,
        id: ast::ExprId,
        callee: ast::ExprId,
        operation: ast::DefPath,
        ordinals: Vec<usize>,
        expected: Option<TypeId>,
        frame: usize,
        checked: usize,
    ) -> Result<(), QueryError> {
        let ast::ExprKind::Call { arguments, .. } = self.input.syntax.tree.expressions
            [id.0 as usize]
            .kind
            .clone()
        else {
            unreachable!()
        };
        let signature = match self.snapshot(&operation) {
            Ok(signature) => signature,
            Err(QueryError::NotImplemented) => {
                self.report(
                    const { RuleId::of("SEM-3") },
                    self.span(id),
                    &format!(
                        "f-strings format through std@1, which this program does not select \
                         ({operation} is unavailable); add std@1 to the selected manifest's \
                         dependencies (the source's .hir.json sidecar when checking a source)"
                    ),
                );
                return self.recover_format(id, callee, &arguments, expected, frame, checked);
            }
            Err(QueryError::Diagnostics(_)) => {
                return self.recover_format(id, callee, &arguments, expected, frame, checked);
            }
            Err(e) => return Err(e),
        };
        let SignatureKind::Function(f) = &signature.kind else {
            return Err(QueryError::Operational(OperationalFailure {
                operation: "f-string".into(),
                message: format!("{operation} is not a function"),
            }));
        };
        let f = self.import_function(f)?;
        let ty = self.named_function(f);
        self.put(callee, ty, None, UseKind::Borrow)?;
        self.body
            .resolution
            .expressions
            .push((callee, ResolvedName::Definition(operation.clone())));
        self.formatters.insert(callee, (operation, ordinals));
        self.start_call(id, expected, frame, checked)
    }
    fn recover_format(
        &mut self,
        id: ast::ExprId,
        callee: ast::ExprId,
        arguments: &[ast::Argument],
        expected: Option<TypeId>,
        frame: usize,
        checked: usize,
    ) -> Result<(), QueryError> {
        let e = self.error();
        self.put(callee, e, None, UseKind::Borrow)?;
        self.body.calls.push(CheckedCall {
            expression: id,
            target: CallTarget::Value(callee),
            arguments: arguments
                .iter()
                .map(|a| CheckedArgument {
                    expression: a.value,
                    parameter: None,
                })
                .collect(),
            generic_arguments: vec![],
            contextual_contracts: vec![],
            result: e,
            state: CallState::Recovery,
        });
        self.put(id, e, expected, UseKind::Borrow)?;
        for argument in arguments[checked..].iter().rev() {
            self.work.push(Work::Enter(argument.value, Some(e), frame));
        }
        Ok(())
    }
}
