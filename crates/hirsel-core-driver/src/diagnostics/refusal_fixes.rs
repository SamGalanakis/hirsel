use super::{Applicability, CheckProduct, Edit, Fix, source_name};
use crate::FileProduct;
use hirsel_core_resolve::{CausalRole, Namespace, Refusal};
use hirsel_core_syntax::{Item, NodeId, NodeKind, Span, TokenKind};
use std::collections::BTreeSet;

impl CheckProduct {
    pub(super) fn refusal_fixes(&self, r: &Refusal, file: &FileProduct) -> Vec<Fix> {
        let Some(item_range) = file.syntax.items.get(r.primary.item as usize) else {
            return Vec::new();
        };
        let item = file.syntax.item(r.primary.item as usize);
        let base = item_range.file_offset;
        let module = source_name(file.name.as_deref(), &self.index.module_path(file.module));
        let cx = FixContext {
            product: self,
            file,
            item,
            base,
            module,
            refusal: r,
        };
        match r.rule.as_str() {
            "OWN-7" => cx.own7_fixes(),
            "OWN-5" => cx.own5_fixes(),
            "OWN-10" => cx.own10_fixes(),
            "CAP-1" => cx.cap1_fixes(),
            "LANG-11" => cx.lang11_fixes(),
            "LANG-12" => cx.lang12_fixes(),
            "LANG-17" => cx.lang17_fixes(),
            "LANG-21" => cx.lang21_fixes(),
            _ => Vec::new(),
        }
    }
}

pub(super) struct FixContext<'a> {
    pub(super) product: &'a CheckProduct,
    pub(super) file: &'a FileProduct,
    pub(super) item: Item<'a>,
    pub(super) base: u32,
    pub(super) module: String,
    pub(super) refusal: &'a Refusal,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum BindingKind {
    ViewParam(NodeId),
    MutParam(NodeId),
    TakeParam(NodeId),
    Let { pat: NodeId },
    LetMut,
    Other,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LiteralFlavor {
    Integer { signed_only: bool },
    Float,
}

impl<'a> FixContext<'a> {
    pub(super) fn text(&self) -> &'a str {
        &self.item.text[self.item.file_offset as usize..self.item.range.end as usize]
    }

    pub(super) fn slice(&self, span: Span) -> Option<&'a str> {
        self.text().get(span.start as usize..span.end as usize)
    }

    pub(super) fn edit(&self, span: Span, text: impl Into<String>) -> Edit {
        Edit {
            module: self.module.clone(),
            span: Span {
                start: self.base + span.start,
                end: self.base + span.end,
            },
            text: text.into(),
        }
    }

    pub(super) fn insert(&self, offset: u32, text: impl Into<String>) -> Edit {
        self.edit(
            Span {
                start: offset,
                end: offset,
            },
            text,
        )
    }

    pub(super) fn fix(&self, applicability: Applicability, mut edits: Vec<Edit>) -> Fix {
        edits.sort_by_key(|e| (e.span.start, e.span.end));
        Fix {
            applicability,
            edits,
            signature: None,
        }
    }

    pub(super) fn single_fix(
        &self,
        applicability: Applicability,
        span: Span,
        text: impl Into<String>,
    ) -> Fix {
        self.fix(applicability, vec![self.edit(span, text)])
    }

    pub(super) fn same_item_cause(&self, role: CausalRole) -> Option<Span> {
        self.refusal.causal.iter().find_map(|c| {
            (c.role == role
                && c.site.source == self.refusal.primary.source
                && c.site.item == self.refusal.primary.item)
                .then_some(c.site.span)
        })
    }

    /// Return the root-to-node path for the deepest node matching `span`.
    pub(super) fn path_to_span(&self, span: Span) -> Option<Vec<NodeId>> {
        let mut path = Vec::new();
        self.find_span(self.item.root, span, &mut path)
            .then_some(path)
    }

    fn find_span(&self, node: NodeId, target: Span, path: &mut Vec<NodeId>) -> bool {
        let origin = self.item.tree.node(node).origin.span;
        if target.start < origin.start || target.end > origin.end {
            return false;
        }
        path.push(node);
        for &child in self.item.tree.children(node) {
            if self.find_span(child, target, path) {
                return true;
            }
        }
        if origin == target {
            return true;
        }
        path.pop();
        false
    }

    /// Find the deepest node whose span encloses `span`.
    pub(super) fn enclosing_path(&self, span: Span) -> Vec<NodeId> {
        let mut path = Vec::new();
        self.collect_enclosing(self.item.root, span, &mut path);
        path
    }

    fn collect_enclosing(&self, node: NodeId, target: Span, path: &mut Vec<NodeId>) {
        let origin = self.item.tree.node(node).origin.span;
        if target.start < origin.start || target.end > origin.end {
            return;
        }
        path.push(node);
        for &child in self.item.tree.children(node) {
            let child_origin = self.item.tree.node(child).origin.span;
            if target.start >= child_origin.start && target.end <= child_origin.end {
                self.collect_enclosing(child, target, path);
                break;
            }
        }
    }

    pub(super) fn node_kind(&self, id: NodeId) -> NodeKind {
        self.item.tree.node(id).kind
    }

    pub(super) fn node_span(&self, id: NodeId) -> Span {
        self.item.tree.node(id).origin.span
    }

    pub(super) fn node_token_text(&self, id: NodeId) -> Option<&'a str> {
        let tok = self.item.tree.node(id).token?;
        let span = self.item.token_span(tok);
        self.slice(span)
    }

    pub(super) fn place_root_ident(&self, node: NodeId) -> Option<(&'a str, bool)> {
        let mut cur = node;
        let mut projected = false;
        loop {
            match self.node_kind(cur) {
                NodeKind::Path => {
                    let text = self.slice(self.node_span(cur))?;
                    if let Some((head, _)) = text.split_once('.') {
                        return Some((head, true));
                    }
                    return Some((text, projected));
                }
                NodeKind::Projection | NodeKind::Index => {
                    projected = true;
                    cur = *self.item.tree.children(cur).first()?;
                }
                _ => return None,
            }
        }
    }

    /// Look up the binding kind of `name` inside the current function item.
    /// Hirsel forbids shadowing (LANG-7), so one name has at most one binder.
    pub(super) fn lookup_binding(&self, name: &str) -> Option<BindingKind> {
        self.find_binding_in(self.item.root, name)
    }

    fn find_binding_in(&self, node: NodeId, name: &str) -> Option<BindingKind> {
        match self.node_kind(node) {
            NodeKind::Parameter if self.node_token_text(node) == Some(name) => {
                return Some(BindingKind::ViewParam(node));
            }
            NodeKind::MutParameter if self.node_token_text(node) == Some(name) => {
                return Some(BindingKind::MutParam(node));
            }
            NodeKind::TakeParameter if self.node_token_text(node) == Some(name) => {
                return Some(BindingKind::TakeParam(node));
            }
            NodeKind::Let | NodeKind::LetMut => {
                if let Some(&pat) = self.item.tree.children(node).first()
                    && self.node_kind(pat) == NodeKind::BindPattern
                    && self.slice(self.node_span(pat)) == Some(name)
                {
                    return Some(if self.node_kind(node) == NodeKind::LetMut {
                        BindingKind::LetMut
                    } else {
                        BindingKind::Let { pat }
                    });
                }
            }
            NodeKind::BindPattern | NodeKind::TakePattern | NodeKind::MutPattern
                if self.slice(self.node_span(node)) == Some(name) =>
            {
                return Some(BindingKind::Other);
            }
            _ => {}
        }
        for &child in self.item.tree.children(node) {
            if let Some(found) = self.find_binding_in(child, name) {
                return Some(found);
            }
        }
        None
    }

    /// Find the written type of a local or parameter `name` in the current item.
    fn declared_type_of_name(&self, name: &str) -> Option<&'a str> {
        self.find_declared_type_in(self.item.root, name)
    }

    fn find_declared_type_in(&self, node: NodeId, name: &str) -> Option<&'a str> {
        match self.node_kind(node) {
            NodeKind::Parameter
            | NodeKind::MutParameter
            | NodeKind::TakeParameter
            | NodeKind::BytesParameter
            | NodeKind::StepsParameter
                if self.node_token_text(node) == Some(name) =>
            {
                let ty = *self.item.tree.children(node).first()?;
                return self.slice(self.node_span(ty));
            }
            NodeKind::Let | NodeKind::LetMut => {
                let kids = self.item.tree.children(node);
                if let Some(&pat) = kids.first()
                    && self.node_kind(pat) == NodeKind::BindPattern
                    && self.slice(self.node_span(pat)) == Some(name)
                    && let Some(&ty) = kids.get(1)
                    && is_type_kind(self.node_kind(ty))
                {
                    return self.slice(self.node_span(ty));
                }
            }
            _ => {}
        }
        for &child in self.item.tree.children(node) {
            if let Some(found) = self.find_declared_type_in(child, name) {
                return Some(found);
            }
        }
        None
    }

    /// Resolve the written type of a place expression (e.g. `s` or `m.items`)
    /// using declarations in the current item and same-file record definitions.
    pub(super) fn place_written_type(&self, node: NodeId) -> Option<&'a str> {
        match self.node_kind(node) {
            NodeKind::Path => {
                let text = self.slice(self.node_span(node))?;
                let mut segments = text.split('.');
                let head = segments.next()?;
                let mut ty = self.declared_type_of_name(head)?.trim();
                for field in segments {
                    ty = self.record_field_type_in_file(ty, field)?.trim();
                }
                Some(ty)
            }
            NodeKind::Projection => {
                let base_node = *self.item.tree.children(node).first()?;
                let base_ty = self.place_written_type(base_node)?.trim();
                let field_name = self.node_token_text(node)?;
                self.record_field_type_in_file(base_ty, field_name)
            }
            _ => None,
        }
    }

    fn record_field_type_in_file(&self, type_name: &str, field_name: &str) -> Option<&'a str> {
        for idx in 0..self.file.syntax.items.len() {
            let other = self.file.syntax.item(idx);
            let root = other.tree.node(other.root);
            if !matches!(
                root.kind,
                NodeKind::TypeDecl | NodeKind::ResourceType | NodeKind::ScopedType
            ) {
                continue;
            }
            let Some(name_tok) = root.token else {
                continue;
            };
            let name_span = other.token_span(name_tok);
            let other_text = &other.text[other.file_offset as usize..other.range.end as usize];
            if other_text.get(name_span.start as usize..name_span.end as usize) != Some(type_name) {
                continue;
            }
            for &child in other.tree.children(other.root) {
                if other.tree.node(child).kind != NodeKind::Fields {
                    continue;
                }
                for &field in other.tree.children(child) {
                    let fnode = other.tree.node(field);
                    if fnode.kind != NodeKind::Field {
                        continue;
                    }
                    let Some(ftok) = fnode.token else {
                        continue;
                    };
                    let fspan = other.token_span(ftok);
                    if other_text.get(fspan.start as usize..fspan.end as usize) == Some(field_name)
                        && let Some(&ty_node) = other.tree.children(field).first()
                    {
                        let ty_span = other.tree.node(ty_node).origin.span;
                        return other_text.get(ty_span.start as usize..ty_span.end as usize);
                    }
                }
            }
        }
        None
    }

    // ---- 4. CAP-1: missing capability argument ----

    fn cap1_fixes(&self) -> Vec<Fix> {
        let Some(cap_type) = self
            .refusal
            .message
            .strip_prefix("no `")
            .and_then(|s| s.split('`').next())
            .filter(|s| !s.is_empty())
        else {
            return Vec::new();
        };
        if self.node_kind(self.item.root) != NodeKind::Function {
            return Vec::new();
        }
        let call_path = self.enclosing_path(self.refusal.primary.span);
        // An owning closure cannot capture capabilities (CAP-3 / LANG-14).
        if call_path
            .iter()
            .any(|&n| self.node_kind(n) == NodeKind::Closure)
        {
            return Vec::new();
        }
        let Some(&call_node) = call_path
            .iter()
            .rev()
            .find(|&&n| self.node_kind(n) == NodeKind::Call)
        else {
            return Vec::new();
        };
        let Some(&params_node) = self
            .item
            .tree
            .children(self.item.root)
            .iter()
            .find(|&&n| self.node_kind(n) == NodeKind::Parameters)
        else {
            return Vec::new();
        };
        let call_args = self.item.tree.children(call_node).len().saturating_sub(1);
        let call_insert_pos = self.refusal.primary.span.start;
        let call_arg_edit = |arg_name: &str| {
            let text = if call_args == 0 {
                arg_name.to_owned()
            } else if self.has_trailing_comma_before(call_insert_pos) {
                format!(" {arg_name}")
            } else {
                format!(", {arg_name}")
            };
            self.insert(call_insert_pos, text)
        };

        // If the enclosing function already has a parameter of this capability type,
        // pass that parameter directly instead of adding a duplicate one.
        if let Some(existing) = self.existing_param_of_type(params_node, cap_type) {
            return vec![self.fix(Applicability::Maybe, vec![call_arg_edit(existing)])];
        }

        let param_name = self.choose_capability_param_name(cap_type);
        let params_span = self.node_span(params_node);
        let Some(params_text) = self.slice(params_span) else {
            return Vec::new();
        };
        if !params_text.ends_with(')') {
            return Vec::new();
        }
        let param_insert_pos = params_span.end - 1;
        let param_edit_text = if self.item.tree.children(params_node).is_empty() {
            format!("{param_name}: {cap_type}")
        } else if self.has_trailing_comma_before(param_insert_pos) {
            format!(" {param_name}: {cap_type}")
        } else {
            format!(", {param_name}: {cap_type}")
        };
        vec![self.fix(
            Applicability::Maybe,
            vec![
                self.insert(param_insert_pos, param_edit_text),
                call_arg_edit(&param_name),
            ],
        )]
    }

    fn has_trailing_comma_before(&self, item_offset: u32) -> bool {
        let abs = self.base + item_offset;
        self.file
            .syntax
            .tree
            .tokens
            .iter()
            .rev()
            .find(|t| t.span.end <= abs && !t.kind.is_trivia())
            .is_some_and(|t| t.kind == TokenKind::Comma)
    }

    fn existing_param_of_type(&self, params_node: NodeId, cap_type: &str) -> Option<&'a str> {
        for &param in self.item.tree.children(params_node) {
            let Some(&ty_node) = self.item.tree.children(param).first() else {
                continue;
            };
            let ty_text = self.slice(self.node_span(ty_node))?.trim();
            if ty_text == cap_type {
                return self.node_token_text(param);
            }
        }
        None
    }

    fn choose_capability_param_name(&self, cap_type: &str) -> String {
        let mut taken = BTreeSet::new();
        self.collect_all_binders(self.item.root, &mut taken);
        let preferred: &[&str] = match cap_type {
            "Clock" => &["c", "clock", "clk"],
            "Dir" => &["dir", "d"],
            "Net" => &["net", "n"],
            "Env" => &["env", "e"],
            "Stdout" => &["out", "stdout"],
            "Stderr" => &["err", "stderr"],
            "Stdin" => &["inp", "stdin"],
            "Tasks" => &["tasks", "t"],
            "Random" => &["rng", "random"],
            "Args" => &["argv", "args"],
            "Process" => &["proc", "process"],
            "Exec" => &["exec"],
            "Secret" => &["secret"],
            "Cred" => &["cred"],
            "Foreign" => &["ffi", "foreign"],
            _ => &[],
        };
        for &candidate in preferred {
            if self.is_fresh_param_name(candidate, &taken) {
                return candidate.to_owned();
            }
        }
        let lower = cap_type.to_ascii_lowercase();
        if self.is_fresh_param_name(&lower, &taken) {
            return lower;
        }
        for suffix in 2..=99 {
            let candidate = format!("{lower}_{suffix}");
            if self.is_fresh_param_name(&candidate, &taken) {
                return candidate;
            }
        }
        format!("{lower}_cap")
    }

    fn is_fresh_param_name(&self, name: &str, taken: &BTreeSet<&str>) -> bool {
        if taken.contains(name) {
            return false;
        }
        let index = &self.product.index;
        if let Some(sym) = index.symbols.find(name) {
            if hirsel_core_syntax::keyword_symbol(sym) {
                return false;
            }
            if index
                .member(self.refusal.primary.module, sym, Namespace::Value, false)
                .is_some()
                || index.prelude(sym, Namespace::Value, false).is_some()
            {
                return false;
            }
        }
        true
    }

    fn collect_all_binders(&self, node: NodeId, out: &mut BTreeSet<&'a str>) {
        match self.node_kind(node) {
            NodeKind::Parameter
            | NodeKind::MutParameter
            | NodeKind::TakeParameter
            | NodeKind::BytesParameter
            | NodeKind::StepsParameter
            | NodeKind::TakePattern
            | NodeKind::MutPattern => {
                if let Some(name) = self.node_token_text(node) {
                    out.insert(name);
                }
            }
            NodeKind::BindPattern => {
                if let Some(name) = self.slice(self.node_span(node)) {
                    out.insert(name);
                }
            }
            _ => {}
        }
        for &child in self.item.tree.children(node) {
            self.collect_all_binders(child, out);
        }
    }

    // ---- 5. LANG-11: untyped integer / float literals ----

    fn lang11_fixes(&self) -> Vec<Fix> {
        let msg = self.refusal.message.as_str();
        let is_untyped_literal = msg.starts_with("an unsuffixed number needs an expected type")
            || msg.starts_with("a literal operand needs an expected type")
            || msg.starts_with("literal operands have no type here")
            || msg
                .starts_with("a literal argument needs a fixed parameter type; suffix its numbers");
        if !is_untyped_literal {
            return Vec::new();
        }
        let Some(path) = self.path_to_span(self.refusal.primary.span) else {
            return Vec::new();
        };
        let expr_node = *path.last().unwrap();
        let parent_negated = path.len() >= 2
            && self.node_kind(path[path.len() - 2]) == NodeKind::Unary
            && self.node_token_text(path[path.len() - 2]) == Some("-");
        let Some((lit_node, flavor)) = self.find_untyped_literal(expr_node, parent_negated) else {
            return Vec::new();
        };

        // Check whether the refused literal (or its enclosing `-lit`) is the direct
        // initializer of an untyped `let` / `let mut` binding.
        if let Some((pat, bound_name)) = self.enclosing_untyped_let(&path) {
            let ret_hint = bound_name.and_then(|name| self.tail_return_scalar_hint(name, flavor));
            let types = literal_type_candidates(flavor, ret_hint);
            let insert_at = self.node_span(pat).end;
            return types
                .into_iter()
                .map(|ty| {
                    self.single_fix(
                        Applicability::Maybe,
                        Span {
                            start: insert_at,
                            end: insert_at,
                        },
                        format!(": {ty}"),
                    )
                })
                .collect();
        }

        // Otherwise, suffix the literal token directly (`7` -> `7u64`, `3.14` -> `3.14f64`).
        let lit_span = self.node_span(lit_node);
        let types = literal_type_candidates(flavor, None);
        types
            .into_iter()
            .map(|ty| {
                self.single_fix(
                    Applicability::Maybe,
                    Span {
                        start: lit_span.end,
                        end: lit_span.end,
                    },
                    ty,
                )
            })
            .collect()
    }

    fn find_untyped_literal(&self, node: NodeId, negated: bool) -> Option<(NodeId, LiteralFlavor)> {
        match self.node_kind(node) {
            NodeKind::Literal => {
                let tok = self.item.tree.node(node).token?;
                match self.file.syntax.tree.tokens[tok.0 as usize].kind {
                    TokenKind::Integer => Some((
                        node,
                        LiteralFlavor::Integer {
                            signed_only: negated,
                        },
                    )),
                    TokenKind::Float => Some((node, LiteralFlavor::Float)),
                    _ => None,
                }
            }
            NodeKind::Unary => {
                let is_neg = negated || self.node_token_text(node) == Some("-");
                let child = *self.item.tree.children(node).first()?;
                self.find_untyped_literal(child, is_neg)
            }
            NodeKind::Group | NodeKind::Binary => {
                for &child in self.item.tree.children(node) {
                    if let Some(found) = self.find_untyped_literal(child, negated) {
                        return Some(found);
                    }
                }
                None
            }
            _ => None,
        }
    }

    /// If `path` ends at the direct initializer (or unary `-` / binary operand of it)
    /// of an untyped `let` / `let mut` with a simple binder pattern, return `(pat, name)`.
    fn enclosing_untyped_let(&self, path: &[NodeId]) -> Option<(NodeId, Option<&'a str>)> {
        let mut idx = path.len() - 1;
        while idx > 0 {
            let parent = path[idx - 1];
            match self.node_kind(parent) {
                NodeKind::Unary | NodeKind::Group => {
                    idx -= 1;
                }
                NodeKind::Binary => {
                    // Only climb arithmetic/bitwise binary ops (not comparisons,
                    // whose result type is `bool` rather than the literal's type).
                    let op = self.node_token_text(parent)?;
                    if matches!(
                        op,
                        "+" | "-" | "*" | "/" | "%" | "&" | "|" | "^" | "<<" | ">>"
                    ) {
                        idx -= 1;
                    } else {
                        break;
                    }
                }
                NodeKind::Let | NodeKind::LetMut => {
                    let kids = self.item.tree.children(parent);
                    let pat = *kids.first()?;
                    let has_ty = kids
                        .get(1)
                        .is_some_and(|&n| is_type_kind(self.node_kind(n)));
                    if has_ty || kids.get(1) != Some(&path[idx]) {
                        return None;
                    }
                    return match self.node_kind(pat) {
                        NodeKind::BindPattern => Some((pat, self.slice(self.node_span(pat)))),
                        NodeKind::Wildcard => Some((pat, None)),
                        _ => None,
                    };
                }
                _ => break,
            }
        }
        None
    }

    /// If `name` is returned directly as the enclosing function's tail expression
    /// and the function's return type is a compatible numeric scalar, return it.
    fn tail_return_scalar_hint(&self, name: &str, flavor: LiteralFlavor) -> Option<&'a str> {
        if self.node_kind(self.item.root) != NodeKind::Function {
            return None;
        }
        let kids = self.item.tree.children(self.item.root);
        let ret_node = kids
            .iter()
            .copied()
            .find(|&n| self.node_kind(n) == NodeKind::ResultType)?;
        let ret_ty = *self.item.tree.children(ret_node).first()?;
        let ty_text = self.slice(self.node_span(ret_ty))?.trim();
        let compatible = match flavor {
            LiteralFlavor::Float => matches!(ty_text, "f32" | "f64"),
            LiteralFlavor::Integer { signed_only: true } => {
                matches!(ty_text, "i8" | "i16" | "i32" | "i64")
            }
            LiteralFlavor::Integer { signed_only: false } => {
                matches!(
                    ty_text,
                    "u8" | "u16" | "u32" | "u64" | "i8" | "i16" | "i32" | "i64"
                )
            }
        };
        if !compatible {
            return None;
        }
        let body = kids
            .iter()
            .rev()
            .copied()
            .find(|&n| self.node_kind(n) == NodeKind::Block)?;
        let last = *self.item.tree.children(body).last()?;
        let tail = if self.node_kind(last) == NodeKind::ExpressionStatement {
            *self.item.tree.children(last).first()?
        } else {
            last
        };
        (self.node_kind(tail) == NodeKind::Path && self.slice(self.node_span(tail)) == Some(name))
            .then_some(ty_text)
    }

    // ---- LANG-12: an open type parameter of the callee ----

    /// `let j = 8 + int.widen(i)`: give the result a type, or suffix the literal.
    fn lang12_fixes(&self) -> Vec<Fix> {
        if !self
            .refusal
            .message
            .starts_with("a type parameter of the callee is still open")
        {
            return Vec::new();
        }
        let Some(path) = self.path_to_span(self.refusal.primary.span) else {
            return Vec::new();
        };
        let call = *path.last().unwrap();
        if self.node_kind(call) != NodeKind::Call {
            return Vec::new();
        }
        // The literal operand of the arithmetic the call sits in, if any.
        let mut idx = path.len() - 1;
        let mut literal = None;
        while idx > 0 {
            let parent = path[idx - 1];
            match self.node_kind(parent) {
                NodeKind::Group => {}
                NodeKind::Binary
                    if matches!(
                        self.node_token_text(parent),
                        Some("+" | "-" | "*" | "/" | "%" | "&" | "|" | "^")
                    ) =>
                {
                    literal = literal.or_else(|| {
                        self.item
                            .tree
                            .children(parent)
                            .iter()
                            .filter(|&&c| c != path[idx])
                            .find_map(|&c| self.find_untyped_literal(c, false))
                    });
                }
                _ => break,
            }
            idx -= 1;
        }
        let is_int_call = self
            .item
            .tree
            .children(call)
            .first()
            .and_then(|&callee| self.slice(self.node_span(callee)))
            .is_some_and(|callee| callee.starts_with("int."));
        let flavor = match literal {
            Some((_, flavor)) => flavor,
            None if is_int_call => LiteralFlavor::Integer { signed_only: false },
            None => return Vec::new(),
        };
        // The type a `let` binding returned as the function's tail asks for.
        let hint = self
            .enclosing_untyped_let(&path)
            .and_then(|(_, name)| name)
            .and_then(|name| self.tail_return_scalar_hint(name, flavor));
        let ty = literal_type_candidates(flavor, hint)[0];
        // An expected type reaches the call only when it is the whole initializer:
        // in `8 + int.widen(i)` the literal operand gives the call its type (LANG-21).
        if let Some((lit, _)) = literal {
            let at = self.node_span(lit).end;
            return vec![self.single_fix(Applicability::Maybe, Span { start: at, end: at }, ty)];
        }
        let mut idx = path.len() - 1;
        while idx > 0 && self.node_kind(path[idx - 1]) == NodeKind::Group {
            idx -= 1;
        }
        let Some(&stmt) = idx.checked_sub(1).and_then(|i| path.get(i)) else {
            return Vec::new();
        };
        let kids = self.item.tree.children(stmt);
        if !matches!(self.node_kind(stmt), NodeKind::Let | NodeKind::LetMut)
            || kids.get(1) != Some(&path[idx])
            || !matches!(
                kids.first().map(|&p| self.node_kind(p)),
                Some(NodeKind::BindPattern | NodeKind::Wildcard)
            )
        {
            return Vec::new();
        }
        let at = self.node_span(kids[0]).end;
        vec![self.single_fix(
            Applicability::Maybe,
            Span { start: at, end: at },
            format!(": {ty}"),
        )]
    }

    // ---- LANG-21: a shift count of another width ----

    /// `x >> s` with `x: u64, s: u32`: the count takes the shifted value's type.
    fn lang21_fixes(&self) -> Vec<Fix> {
        let (Some(count), Some(value)) = (
            self.refusal.declared.as_deref(),
            self.refusal.required.as_deref(),
        ) else {
            return Vec::new();
        };
        let width = |ty: &str| -> Option<(bool, u32)> {
            let signed = ty.starts_with('i');
            let bits = ty.strip_prefix(['u', 'i'])?.parse().ok()?;
            matches!(bits, 8 | 16 | 32 | 64).then_some((signed, bits))
        };
        let (Some((cs, cb)), Some((vs, vb))) = (width(count), width(value)) else {
            return Vec::new();
        };
        if cs != vs || cb >= vb {
            return Vec::new();
        }
        let path = self.enclosing_path(self.refusal.primary.span);
        let Some(&shift) = path.last() else {
            return Vec::new();
        };
        if !matches!(self.node_kind(shift), NodeKind::Binary | NodeKind::Assign)
            || !matches!(
                self.node_token_text(shift),
                Some("<<" | ">>" | "<<=" | ">>=")
            )
        {
            return Vec::new();
        }
        let Some(&rhs) = self.item.tree.children(shift).get(1) else {
            return Vec::new();
        };
        let span = self.node_span(rhs);
        vec![self.fix(
            Applicability::Safe,
            vec![
                self.insert(span.start, "int.widen("),
                self.insert(span.end, ")"),
            ],
        )]
    }

    // ---- LANG-17: `mut` in a `let` pattern ----

    /// `let (mut a, mut b) = pair()`: bind fresh names, then declare `let mut a = a0`.
    fn lang17_fixes(&self) -> Vec<Fix> {
        if !self
            .refusal
            .message
            .starts_with("`mut` in a pattern names a place")
        {
            return Vec::new();
        }
        let path = self.enclosing_path(self.refusal.primary.span);
        let Some(&stmt) = path
            .iter()
            .rev()
            .find(|&&n| self.node_kind(n) == NodeKind::Let)
        else {
            return Vec::new();
        };
        let Some(&pat) = self.item.tree.children(stmt).first() else {
            return Vec::new();
        };
        // The refused `mut` is in this `let`'s own pattern, not in a `for` or `?=>`
        // pattern inside its initializer.
        let (site, pat_span) = (self.refusal.primary.span, self.node_span(pat));
        if site.start < pat_span.start || site.end > pat_span.end {
            return Vec::new();
        }
        let mut muts = Vec::new();
        self.collect_mut_patterns(pat, &mut muts);
        if muts.is_empty() {
            return Vec::new();
        }
        let mut taken = BTreeSet::new();
        self.collect_all_binders(self.item.root, &mut taken);
        let mut fresh_names = Vec::new();
        let mut edits = Vec::new();
        let mut declarations = String::new();
        for &m in &muts {
            let Some(name) = self.node_token_text(m) else {
                return Vec::new();
            };
            let Some(fresh) = (0..100)
                .map(|k| format!("{name}{k}"))
                .find(|c| !fresh_names.contains(c) && self.is_fresh_param_name(c, &taken))
            else {
                return Vec::new();
            };
            edits.push(self.edit(self.node_span(m), fresh.clone()));
            declarations.push_str(&format!("; let mut {name} = {fresh}"));
            fresh_names.push(fresh);
        }
        // After the initializer, so a written `;` still ends the last declaration.
        let Some(&init) = self.item.tree.children(stmt).last() else {
            return Vec::new();
        };
        edits.push(self.insert(self.node_span(init).end, declarations));
        // A scrutinee that is an existing place is copied or moved by the rewrite.
        vec![self.fix(Applicability::Maybe, edits)]
    }

    fn collect_mut_patterns(&self, node: NodeId, out: &mut Vec<NodeId>) {
        if self.node_kind(node) == NodeKind::MutPattern {
            out.push(node);
        }
        for &child in self.item.tree.children(node) {
            self.collect_mut_patterns(child, out);
        }
    }
}

fn literal_type_candidates<'a>(flavor: LiteralFlavor, hint: Option<&'a str>) -> Vec<&'a str> {
    let defaults: &[&str] = match flavor {
        LiteralFlavor::Float => &["f64"],
        LiteralFlavor::Integer { signed_only: true } => &["i64"],
        LiteralFlavor::Integer { signed_only: false } => &["u64", "i64"],
    };
    let mut out = Vec::new();
    if let Some(h) = hint {
        out.push(h);
    }
    for &ty in defaults {
        if !out.contains(&ty) && out.len() < 2 {
            out.push(ty);
        }
    }
    out
}

pub(super) fn is_type_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
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
