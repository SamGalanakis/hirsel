use super::documents::{Encoding, Text};
use super::navigation::range;
use hirsel_core_syntax::{Item, NodeId, NodeKind, ParseMode, Symbols, parse_bytes};
use serde_json::{Value, json};

pub(super) fn outline(text: &Text, mode: ParseMode, encoding: Encoding) -> Value {
    let parsed = parse_bytes(&text.bytes, mode, &mut Symbols::default());
    Value::Array(
        parsed
            .syntax
            .items()
            .filter(|item| !item.range.has_errors)
            .filter_map(|item| symbol(item, item.root, text, encoding))
            .collect(),
    )
}

fn symbol(item: Item<'_>, id: NodeId, text: &Text, encoding: Encoding) -> Option<Value> {
    let node = item.tree.node(id);
    let kind = match node.kind {
        NodeKind::Function | NodeKind::CloneHook => 12,
        NodeKind::Constant => 14,
        NodeKind::TypeDecl => {
            if item
                .tree
                .children(id)
                .iter()
                .any(|&child| item.tree.node(child).kind == NodeKind::Variants)
            {
                10
            } else {
                23
            }
        }
        NodeKind::ResourceType | NodeKind::ScopedType => 23,
        NodeKind::Field => 8,
        NodeKind::Variant => 22,
        _ => return None,
    };
    let token = node.token?;
    let name = item.tree.token_text(item.text, token);
    if name == "_" {
        return None;
    }
    let children = item
        .tree
        .children(id)
        .iter()
        .filter(|&&child| {
            matches!(
                item.tree.node(child).kind,
                NodeKind::Fields | NodeKind::Variants
            )
        })
        .flat_map(|&child| item.tree.children(child))
        .filter_map(|&child| symbol(item, child, text, encoding))
        .collect::<Vec<_>>();
    let mut row = json!({
        "name": name,
        "kind": kind,
        "range": range(text, super::navigation::absolute(item, node.origin.span), encoding)?,
        "selectionRange": range(text, super::navigation::absolute(item, item.token_span(token)), encoding)?,
    });
    if !children.is_empty() {
        row["children"] = json!(children);
    }
    Some(row)
}
