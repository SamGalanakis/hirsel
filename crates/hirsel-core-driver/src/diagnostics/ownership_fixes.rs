use super::refusal_fixes::{BindingKind, FixContext, is_type_kind};
use super::{Applicability, Fix};
use hirsel_core_resolve::CausalRole;
use hirsel_core_syntax::{NodeId, NodeKind, Span};
use std::collections::BTreeSet;

impl<'a> FixContext<'a> {
    // ---- 1. OWN-7: mode marks at call sites and moving view parameters ----

    pub(super) fn own7_fixes(&self) -> Vec<Fix> {
        let primary = self.refusal.primary.span;
        let declared = self.refusal.declared.as_deref();
        let required = self.refusal.required.as_deref();

        match (declared, required) {
            (Some("unmarked"), Some("take")) => self.own7_missing_take(primary),
            (Some("unmarked"), Some("mut")) => self.own7_missing_mut(primary),
            (Some("unmarked"), Some(mark @ ("bytes" | "steps"))) => {
                vec![self.single_fix(
                    Applicability::Safe,
                    Span {
                        start: primary.start,
                        end: primary.start,
                    },
                    format!("{mark} "),
                )]
            }
            (Some("mut" | "bytes" | "steps"), Some("view")) => {
                self.own7_strip_leading_mark(primary, Applicability::Safe)
            }
            (Some("mut" | "bytes" | "steps"), Some(req @ ("take" | "mut" | "bytes" | "steps"))) => {
                self.own7_replace_leading_mark(primary, req)
            }
            (Some("view"), Some("take")) => self.own7_view_param_move(primary),
            (None, None) if self.refusal.message == "`mut p` is written only as an argument" => {
                self.own7_strip_leading_mark(primary, Applicability::Maybe)
            }
            _ => Vec::new(),
        }
    }

    fn own7_missing_take(&self, primary: Span) -> Vec<Fix> {
        let Some(path) = self.path_to_span(primary) else {
            return Vec::new();
        };
        let node = *path.last().unwrap();
        let Some((root_name, projected)) = self.place_root_ident(node) else {
            return Vec::new();
        };
        if projected {
            // OWN-14 forbids `take m.field`; suggest cloning the field instead.
            let Some(expr_text) = self.slice(primary) else {
                return Vec::new();
            };
            return vec![self.single_fix(
                Applicability::Maybe,
                primary,
                format!("clone({expr_text})"),
            )];
        }
        match self.lookup_binding(root_name) {
            Some(BindingKind::ViewParam(param_node)) => {
                let param_start = self.node_span(param_node).start;
                let mut fixes = vec![self.fix(
                    Applicability::Maybe,
                    vec![
                        self.insert(param_start, "take "),
                        self.insert(primary.start, "take "),
                    ],
                )];
                if let Some(expr_text) = self.slice(primary) {
                    fixes.push(self.single_fix(
                        Applicability::Maybe,
                        primary,
                        format!("clone({expr_text})"),
                    ));
                }
                fixes
            }
            Some(BindingKind::MutParam(_)) => {
                // OWN-11 forbids moving a `mut` parameter.
                let Some(expr_text) = self.slice(primary) else {
                    return Vec::new();
                };
                vec![self.single_fix(Applicability::Maybe, primary, format!("clone({expr_text})"))]
            }
            Some(BindingKind::TakeParam(_) | BindingKind::Let { .. } | BindingKind::LetMut) => {
                vec![self.single_fix(
                    Applicability::Safe,
                    Span {
                        start: primary.start,
                        end: primary.start,
                    },
                    "take ",
                )]
            }
            Some(BindingKind::Other) | None => vec![self.single_fix(
                Applicability::Maybe,
                Span {
                    start: primary.start,
                    end: primary.start,
                },
                "take ",
            )],
        }
    }

    fn own7_missing_mut(&self, primary: Span) -> Vec<Fix> {
        let Some(path) = self.path_to_span(primary) else {
            return Vec::new();
        };
        let node = *path.last().unwrap();
        let Some((root_name, _)) = self.place_root_ident(node) else {
            return Vec::new();
        };
        match self.lookup_binding(root_name) {
            Some(BindingKind::MutParam(_) | BindingKind::TakeParam(_) | BindingKind::LetMut) => {
                vec![self.single_fix(
                    Applicability::Safe,
                    Span {
                        start: primary.start,
                        end: primary.start,
                    },
                    "mut ",
                )]
            }
            Some(BindingKind::Let { pat, .. }) => {
                let pat_start = self.node_span(pat).start;
                vec![self.fix(
                    Applicability::Maybe,
                    vec![
                        self.insert(pat_start, "mut "),
                        self.insert(primary.start, "mut "),
                    ],
                )]
            }
            Some(BindingKind::ViewParam(param_node)) => {
                let param_start = self.node_span(param_node).start;
                vec![self.fix(
                    Applicability::Maybe,
                    vec![
                        self.insert(param_start, "mut "),
                        self.insert(primary.start, "mut "),
                    ],
                )]
            }
            Some(BindingKind::Other) | None => vec![self.single_fix(
                Applicability::Maybe,
                Span {
                    start: primary.start,
                    end: primary.start,
                },
                "mut ",
            )],
        }
    }

    fn own7_strip_leading_mark(&self, primary: Span, applicability: Applicability) -> Vec<Fix> {
        let Some(path) = self.path_to_span(primary) else {
            return Vec::new();
        };
        let node = *path.last().unwrap();
        if !matches!(
            self.node_kind(node),
            NodeKind::Unary | NodeKind::BytesArgument | NodeKind::StepsArgument
        ) {
            return Vec::new();
        }
        let Some(&inner) = self.item.tree.children(node).first() else {
            return Vec::new();
        };
        let delete_span = Span {
            start: primary.start,
            end: self.node_span(inner).start,
        };
        if delete_span.end <= delete_span.start {
            return Vec::new();
        }
        vec![self.single_fix(applicability, delete_span, "")]
    }

    fn own7_replace_leading_mark(&self, primary: Span, required: &str) -> Vec<Fix> {
        let Some(path) = self.path_to_span(primary) else {
            return Vec::new();
        };
        let node = *path.last().unwrap();
        let kw_span = match self.node_kind(node) {
            NodeKind::Unary => {
                let Some(tok) = self.item.tree.node(node).token else {
                    return Vec::new();
                };
                self.item.token_span(tok)
            }
            NodeKind::BytesArgument | NodeKind::StepsArgument => {
                let Some(tok_idx) = self.item.tree.node(node).origin.tokens.clone().next() else {
                    return Vec::new();
                };
                self.item.token_span(hirsel_core_syntax::TokenId(tok_idx))
            }
            _ => return Vec::new(),
        };
        vec![self.single_fix(Applicability::Maybe, kw_span, required)]
    }

    fn own7_view_param_move(&self, primary: Span) -> Vec<Fix> {
        let mut fixes = Vec::new();
        if let Some(decl_span) = self.same_item_cause(CausalRole::DeclaredHere)
            && let Some(path) = self.path_to_span(decl_span)
            && let Some(&decl_node) = path.last()
            && self.node_kind(decl_node) == NodeKind::Parameter
        {
            fixes.push(self.single_fix(
                Applicability::Maybe,
                Span {
                    start: decl_span.start,
                    end: decl_span.start,
                },
                "take ",
            ));
        }
        if let Some(path) = self.path_to_span(primary)
            && let Some(clone_fix) = self.clone_moved_site(&path)
        {
            fixes.push(clone_fix);
        }
        fixes
    }

    // ---- 2. OWN-5: use after move and non-writable places ----

    pub(super) fn own5_fixes(&self) -> Vec<Fix> {
        if self.refusal.message.ends_with(" is not writable") {
            return self.own5_not_writable();
        }
        if self.refusal.message.ends_with(" was moved") {
            return self.own5_use_after_move();
        }
        Vec::new()
    }

    fn own5_not_writable(&self) -> Vec<Fix> {
        let Some(decl_span) = self.same_item_cause(CausalRole::DeclaredHere) else {
            return Vec::new();
        };
        let Some(path) = self.path_to_span(decl_span) else {
            return Vec::new();
        };
        let decl_node = *path.last().unwrap();
        if self.node_kind(decl_node) == NodeKind::Parameter {
            return vec![self.single_fix(
                Applicability::Maybe,
                Span {
                    start: decl_span.start,
                    end: decl_span.start,
                },
                "mut ",
            )];
        }
        if self.node_kind(decl_node) == NodeKind::BindPattern
            && path.len() >= 2
            && self.node_kind(path[path.len() - 2]) == NodeKind::Let
        {
            return vec![self.single_fix(
                Applicability::Safe,
                Span {
                    start: decl_span.start,
                    end: decl_span.start,
                },
                "mut ",
            )];
        }
        Vec::new()
    }

    fn own5_use_after_move(&self) -> Vec<Fix> {
        let Some(moved_span) = self.same_item_cause(CausalRole::MovedHere) else {
            return Vec::new();
        };
        let Some(mut moved_path) = self.path_to_span(moved_span) else {
            return Vec::new();
        };
        // Lift a bare operand inside `take x` to the `take x` unary node.
        if moved_path.len() >= 2 {
            let parent = moved_path[moved_path.len() - 2];
            if self.node_kind(parent) == NodeKind::Unary
                && self.node_token_text(parent) == Some("take")
            {
                moved_path.pop();
            }
        }
        let mut fixes = Vec::new();
        if let Some(clone_fix) = self.clone_moved_site(&moved_path) {
            fixes.push(clone_fix);
        }
        // If `let t = s` moved `s` and the later use only reads/views `s`,
        // viewing `s` avoids both the move and any clone allocation.
        if let Some(view_fix) = self.view_instead_of_move(&moved_path) {
            fixes.push(view_fix);
        }
        fixes
    }

    /// Build a `Maybe` fix that replaces the moved expression at the end of `path`
    /// with `clone(<place>)`, adding a binding type annotation when inside an
    /// untyped `let` so `clone[T, U]` has a fixed result type `U`.
    fn clone_moved_site(&self, path: &[NodeId]) -> Option<Fix> {
        let site = *path.last()?;
        let (inner_node, replace_span) = if self.node_kind(site) == NodeKind::Unary
            && self.node_token_text(site) == Some("take")
        {
            let child = *self.item.tree.children(site).first()?;
            (child, self.node_span(site))
        } else {
            (site, self.node_span(site))
        };
        let inner_text = self.slice(self.node_span(inner_node))?;
        let parent = path.len().checked_sub(2).map(|i| path[i]);
        match parent.map(|p| (p, self.node_kind(p))) {
            Some((let_node, NodeKind::Let | NodeKind::LetMut)) => {
                let kids = self.item.tree.children(let_node);
                let pat = *kids.first()?;
                let has_ty = kids
                    .get(1)
                    .is_some_and(|&n| is_type_kind(self.node_kind(n)));
                let init = if has_ty { *kids.get(2)? } else { *kids.get(1)? };
                if init != site {
                    return None;
                }
                if has_ty {
                    let ty_text = self.slice(self.node_span(kids[1]))?.trim();
                    if ty_text.starts_with("View[") {
                        return None;
                    }
                    return Some(self.single_fix(
                        Applicability::Maybe,
                        replace_span,
                        format!("clone({inner_text})"),
                    ));
                }
                if !matches!(
                    self.node_kind(pat),
                    NodeKind::BindPattern | NodeKind::Wildcard
                ) {
                    return None;
                }
                let ty = self.place_written_type(inner_node)?;
                Some(self.fix(
                    Applicability::Maybe,
                    vec![
                        self.insert(self.node_span(pat).end, format!(": {ty}")),
                        self.edit(replace_span, format!("clone({inner_text})")),
                    ],
                ))
            }
            Some((
                _,
                NodeKind::Call
                | NodeKind::LabelledArgument
                | NodeKind::Assign
                | NodeKind::Return
                | NodeKind::ExpressionStatement
                | NodeKind::Block
                | NodeKind::Tuple
                | NodeKind::Array,
            )) => Some(self.single_fix(
                Applicability::Maybe,
                replace_span,
                format!("clone({inner_text})"),
            )),
            _ => None,
        }
    }

    fn view_instead_of_move(&self, moved_path: &[NodeId]) -> Option<Fix> {
        if moved_path.len() < 2 {
            return None;
        }
        let site = *moved_path.last()?;
        let let_node = moved_path[moved_path.len() - 2];
        if self.node_kind(let_node) != NodeKind::Let {
            return None;
        }
        let kids = self.item.tree.children(let_node);
        let has_ty = kids
            .get(1)
            .is_some_and(|&n| is_type_kind(self.node_kind(n)));
        if has_ty {
            return None;
        }
        if kids.get(1) != Some(&site) {
            return None;
        }
        // Only suggest `view` when the later use is a read/view, not a move or write.
        let primary_path = self.path_to_span(self.refusal.primary.span)?;
        if self.primary_looks_like_write_or_move(&primary_path) {
            return None;
        }
        let inner = if self.node_kind(site) == NodeKind::Unary
            && self.node_token_text(site) == Some("take")
        {
            *self.item.tree.children(site).first()?
        } else {
            site
        };
        let inner_text = self.slice(self.node_span(inner))?;
        Some(self.single_fix(
            Applicability::Maybe,
            self.node_span(site),
            format!("view {inner_text}"),
        ))
    }

    fn primary_looks_like_write_or_move(&self, path: &[NodeId]) -> bool {
        let Some(&leaf) = path.last() else {
            return true;
        };
        let parent = path.len().checked_sub(2).map(|i| path[i]);
        match parent.map(|p| (p, self.node_kind(p))) {
            Some((p, NodeKind::Unary)) => matches!(self.node_token_text(p), Some("take" | "mut")),
            Some((p, NodeKind::Assign)) => self.item.tree.children(p).first() == Some(&leaf),
            Some((
                _,
                NodeKind::Return
                | NodeKind::Let
                | NodeKind::LetMut
                | NodeKind::Block
                | NodeKind::ExpressionStatement
                | NodeKind::LabelledArgument,
            )) => true,
            _ => false,
        }
    }

    // ---- 3. OWN-10: view / mutation conflict ----

    pub(super) fn own10_fixes(&self) -> Vec<Fix> {
        let Some(viewed_span) = self.same_item_cause(CausalRole::ViewedHere) else {
            return Vec::new();
        };
        let mut fixes = Vec::new();
        if let Some(scope_fix) = self.own10_scope_view_block(viewed_span) {
            fixes.push(scope_fix);
        }
        if let Some(clone_fix) = self.own10_clone_view(viewed_span) {
            fixes.push(clone_fix);
        }
        fixes
    }

    /// Wrap the `let` binding that created the view (and any intermediate
    /// statements before the conflicting mutation) in `{ ... }` when none of
    /// its bindings are referenced at or after the mutation.
    fn own10_scope_view_block(&self, viewed_span: Span) -> Option<Fix> {
        let view_path = self.path_to_span(viewed_span)?;
        let conflict_path = self.enclosing_path(self.refusal.primary.span);
        // Find the deepest shared `Block` containing both statements.
        let mut shared_block = None;
        for (&a, &b) in view_path.iter().zip(conflict_path.iter()) {
            if a == b {
                if self.node_kind(a) == NodeKind::Block {
                    shared_block = Some(a);
                }
            } else {
                break;
            }
        }
        let block = shared_block?;
        let stmts = self.item.tree.children(block);
        let view_idx = stmts.iter().position(|&s| {
            let sp = self.node_span(s);
            viewed_span.start >= sp.start && viewed_span.end <= sp.end
        })?;
        let conflict_idx = stmts.iter().position(|&s| {
            let sp = self.node_span(s);
            self.refusal.primary.span.start >= sp.start && self.refusal.primary.span.end <= sp.end
        })?;
        if view_idx >= conflict_idx {
            return None;
        }
        if !matches!(
            self.node_kind(stmts[view_idx]),
            NodeKind::Let | NodeKind::LetMut
        ) {
            return None;
        }
        let mut bound = BTreeSet::new();
        for &stmt in &stmts[view_idx..conflict_idx] {
            self.collect_stmt_binders(stmt, &mut bound);
        }
        for &stmt in &stmts[conflict_idx..] {
            if self.subtree_uses_any(stmt, &bound) {
                return None;
            }
        }
        let open_pos = self.node_span(stmts[view_idx]).start;
        let last_span = self.node_span(stmts[conflict_idx - 1]);
        let last_text = self.slice(last_span)?;
        let close_pos = if last_text.ends_with(';') {
            last_span.end - 1
        } else {
            last_span.end
        };
        if close_pos <= open_pos {
            return None;
        }
        Some(self.fix(
            Applicability::Maybe,
            vec![self.insert(open_pos, "{ "), self.insert(close_pos, " }")],
        ))
    }

    fn collect_stmt_binders(&self, node: NodeId, out: &mut BTreeSet<&'a str>) {
        if matches!(self.node_kind(node), NodeKind::Let | NodeKind::LetMut)
            && let Some(&pat) = self.item.tree.children(node).first()
        {
            self.collect_pattern_binders(pat, out);
        }
    }

    fn collect_pattern_binders(&self, node: NodeId, out: &mut BTreeSet<&'a str>) {
        match self.node_kind(node) {
            NodeKind::BindPattern => {
                if let Some(name) = self.slice(self.node_span(node)) {
                    out.insert(name);
                }
            }
            NodeKind::TakePattern | NodeKind::MutPattern => {
                if let Some(name) = self.node_token_text(node) {
                    out.insert(name);
                }
            }
            _ => {
                for &child in self.item.tree.children(node) {
                    self.collect_pattern_binders(child, out);
                }
            }
        }
    }

    fn subtree_uses_any(&self, node: NodeId, names: &BTreeSet<&str>) -> bool {
        if self.node_kind(node) == NodeKind::Path
            && let Some(text) = self.slice(self.node_span(node))
            && let Some(head) = text.split('.').next()
            && names.contains(head)
        {
            return true;
        }
        self.item
            .tree
            .children(node)
            .iter()
            .any(|&child| self.subtree_uses_any(child, names))
    }

    /// Replace `view <place>` with `clone(<place>)` (with a binding type when
    /// needed for `clone[T, U]`).
    fn own10_clone_view(&self, viewed_span: Span) -> Option<Fix> {
        let path = self.path_to_span(viewed_span)?;
        let view_node = *path.last()?;
        if self.node_kind(view_node) != NodeKind::Unary
            || self.node_token_text(view_node) != Some("view")
        {
            return None;
        }
        let place_node = *self.item.tree.children(view_node).first()?;
        let place_text = self.slice(self.node_span(place_node))?;
        let parent = path.len().checked_sub(2).map(|i| path[i])?;
        if !matches!(self.node_kind(parent), NodeKind::Let | NodeKind::LetMut) {
            return None;
        }
        let kids = self.item.tree.children(parent);
        let pat = *kids.first()?;
        let has_ty = kids
            .get(1)
            .is_some_and(|&n| is_type_kind(self.node_kind(n)));
        let init = if has_ty { *kids.get(2)? } else { *kids.get(1)? };
        if init != view_node {
            return None;
        }
        if has_ty {
            let ty_text = self.slice(self.node_span(kids[1]))?.trim();
            if ty_text.starts_with("View[") {
                return None;
            }
            return Some(self.single_fix(
                Applicability::Maybe,
                viewed_span,
                format!("clone({place_text})"),
            ));
        }
        if !matches!(
            self.node_kind(pat),
            NodeKind::BindPattern | NodeKind::Wildcard
        ) {
            return None;
        }
        let ty = self.place_written_type(place_node)?;
        Some(self.fix(
            Applicability::Maybe,
            vec![
                self.insert(self.node_span(pat).end, format!(": {ty}")),
                self.edit(viewed_span, format!("clone({place_text})")),
            ],
        ))
    }
}
