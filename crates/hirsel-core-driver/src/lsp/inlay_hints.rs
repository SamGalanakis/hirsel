use super::documents::Encoding;
use super::navigation::absolute;
use super::snapshot::{EditorSnapshot, Types};
use hirsel_core_resolve::{BinderKind, Resolution, Target};
use hirsel_core_syntax::{Item, NodeId, NodeKind, Span};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

pub(super) struct Hint {
    pub(super) offset: u32,
    kind: u8,
    label: String,
}

pub(super) fn bindings(
    item: Item<'_>,
    resolution: &Resolution,
    types: &Types<'_>,
    out: &mut Vec<Hint>,
) {
    let explicit = item
        .range
        .nodes
        .clone()
        .map(NodeId)
        .filter_map(|id| {
            let node = item.tree.node(id);
            if !matches!(node.kind, NodeKind::Let | NodeKind::LetMut) {
                return None;
            }
            let (pattern, rest) = item.tree.children(id).split_first()?;
            rest.iter()
                .any(|&child| {
                    matches!(
                        item.tree.node(child).kind,
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
                })
                .then_some(item.tree.node(*pattern).origin.span)
        })
        .collect::<Vec<_>>();
    for binder in &resolution.binders {
        let span = binder.site.span;
        if binder.kind == BinderKind::Local
            && !explicit
                .iter()
                .any(|pattern| pattern.start <= span.start && span.end <= pattern.end)
            && let Some((ty, _)) = types.get(&(span.start, span.end))
        {
            out.push(Hint {
                offset: item.file_offset + span.end,
                kind: 1,
                label: format!(": {ty}"),
            });
        }
    }
}

pub(super) fn parameters(
    item: Item<'_>,
    resolution: &Resolution,
    mut names: impl FnMut(hirsel_core_resolve::DefinitionId) -> Option<Vec<String>>,
    mut valid: impl FnMut(Span) -> bool,
    out: &mut Vec<Hint>,
) {
    let targets = resolution
        .names
        .iter()
        .map(|usage| (usage.node, usage.target))
        .collect::<BTreeMap<_, _>>();
    for call in item.range.nodes.clone().map(NodeId) {
        let node = item.tree.node(call);
        if node.kind != NodeKind::Call || !valid(node.origin.span) {
            continue;
        }
        let Some((callee, args)) = item.tree.children(call).split_first() else {
            continue;
        };
        let Some(Target::Definition(def)) = targets.get(callee) else {
            continue;
        };
        let Some(names) = names(*def) else { continue };
        if names.len() != args.len() {
            continue;
        }
        for (&arg, name) in args.iter().zip(names) {
            let span = absolute(item, item.tree.node(arg).origin.span);
            let text = &item.text[span.start as usize..span.end as usize];
            if name == "_" || text.rsplit('.').next() == Some(&name) {
                continue;
            }
            out.push(Hint {
                offset: span.start,
                kind: 2,
                label: format!("{name}:"),
            });
        }
    }
}

impl EditorSnapshot {
    pub(super) fn inlay_hints(&self, path: &Path, span: Span, encoding: Encoding) -> Option<Value> {
        let text = self.origins.get(path)?;
        let mut hints = self
            .hints
            .get(path)?
            .iter()
            .filter(|hint| span.start <= hint.offset && hint.offset < span.end)
            .collect::<Vec<_>>();
        hints.sort_by_key(|hint| (hint.offset, hint.kind));
        Some(Value::Array(hints.into_iter().filter_map(|hint| Some(json!({
            "position": text.position(hint.offset, encoding)?, "kind": hint.kind, "label": hint.label,
            "paddingRight": hint.kind == 2,
            "data": {"generation": self.inputs.generation},
        }))).collect()))
    }
}
