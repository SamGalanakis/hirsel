use super::{Checked, Fence, diagnostic_location, library_manifest, package, repo, std_manifest};
use hirsel_core_driver::line_column;
use hirsel_core_resolve::{CorpusManifest, PackageManifest};
use hirsel_core_syntax::{Item, NodeId, NodeKind, ParseMode, Symbols, TokenId, parse};
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn check(fences: &[&Fence]) -> Vec<String> {
    if fences.is_empty() {
        return Vec::new();
    }
    let mut failures = Vec::new();
    let mut modules = BTreeSet::new();
    let mut pending = Vec::new();
    for fence in fences {
        let module = fence.signature_module.as_deref().unwrap();
        if !module.contains('/')
            || !module.split('/').all(|part| {
                !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            })
        {
            failures.push(format!(
                "{}: expected a package/module for the API listing, got {module:?}",
                fence.location()
            ));
            continue;
        }
        let (name, _) = module.split_once('/').unwrap();
        if !matches!(name, "core" | "std")
            && !repo()
                .join("core/programs")
                .join(name)
                .join("hirsel.hcfg")
                .is_file()
        {
            failures.push(format!("{}: unknown package {name}", fence.location()));
            continue;
        }
        pending.push(package(name));
        modules.insert(module);
    }
    if !failures.is_empty() {
        return failures;
    }

    let core = package("core");
    let mut packages = Vec::new();
    let mut loaded = BTreeSet::new();
    pending.push(core.clone());
    while let Some(id) = pending.pop() {
        if !loaded.insert(id.clone()) {
            continue;
        }
        let manifest = match id.name.as_str() {
            "core" => PackageManifest::directory(&repo().join("core/core"), id, true).unwrap(),
            "std" => std_manifest(),
            name => library_manifest(name),
        };
        pending.extend(manifest.dependencies.iter().cloned());
        packages.push(manifest);
    }
    let checked = Checked::check(
        CorpusManifest { core, packages },
        tempfile::tempdir().unwrap(),
        None,
    );
    let product = &checked.product;
    if !product.accepted() {
        for diagnostic in product.machine_diagnostics().unwrap() {
            failures.push(format!(
                "{}: spec API package refused {} ({})",
                diagnostic_location(None, &diagnostic.primary),
                diagnostic.rule,
                diagnostic.message
            ));
        }
        for gap in product.machine_incomplete().unwrap() {
            failures.push(format!(
                "{}: incomplete spec API package check ({})",
                diagnostic_location(None, &gap.primary),
                gap.message
            ));
        }
        for resource in product.machine_resources().unwrap() {
            failures.push(format!(
                "{}: spec API package resource refusal ({})",
                diagnostic_location(None, &resource.primary),
                resource.message
            ));
        }
        return failures;
    }

    let mut exports = BTreeMap::<_, BTreeMap<_, _>>::new();
    for file in &product.files {
        let path = product.index.module_path(file.module);
        let module = format!("{}/{}", path.package.name, path.modules.join("/"));
        if !modules.contains(module.as_str()) {
            continue;
        }
        let items = exports.entry(module).or_default();
        for (ordinal, item) in file.syntax.items().enumerate() {
            if let Some(id) =
                product
                    .index
                    .item_definition(file.module, file.file, ordinal as u32, item)
                && product.index.get(id).public
                && let Some(name) = item_name(item)
            {
                items.insert(name, item);
            }
        }
    }
    for fence in fences {
        let module = fence.signature_module.as_deref().unwrap();
        let parsed = parse(
            &fence.source,
            ParseMode::Signatures,
            &mut Symbols::default(),
        );
        if let Some(resource) = parsed.resource_refusal {
            failures.push(format!(
                "{}: {resource}",
                location(fence, resource.site().start)
            ));
            continue;
        }
        if !parsed.diagnostics.is_empty() {
            failures.extend(parsed.diagnostics.iter().map(|diagnostic| {
                format!(
                    "{}: {}",
                    location(fence, diagnostic.primary.start),
                    diagnostic.message
                )
            }));
            continue;
        }
        if parsed.syntax.items.is_empty() || !fence.rules.is_empty() || fence.refused_any {
            failures.push(format!(
                "{}: expected public API declarations",
                fence.location()
            ));
            continue;
        }
        for claim in parsed.syntax.items() {
            let site = location(fence, claim.file_offset);
            let name = item_name(claim).unwrap();
            let Some(&published) = exports.get(module).and_then(|items| items.get(name)) else {
                failures.push(format!("{site}: no public export {module}/{name}"));
                continue;
            };
            let children = claim.tree.children(claim.root);
            let exported = public_children(published);
            if claim.tree.node(claim.root).kind != published.tree.node(published.root).kind
                || children.len() != exported.len()
                || !children
                    .iter()
                    .zip(&exported)
                    .all(|(&a, &b)| same_node(claim, a, published, b))
            {
                let end = exported
                    .last()
                    .map_or(0, |&id| published.tree.node(id).origin.span.end)
                    .max(
                        published
                            .token_span(published.tree.node(published.root).token.unwrap())
                            .end,
                    );
                let start = published.file_offset as usize;
                let expected = &published.text[start..start + end as usize];
                failures.push(format!(
                    "{site}: signature mismatch for {module}/{name}\n  documented: {}\n  exported:   {expected}",
                    claim.source_text().trim()
                ));
            }
        }
    }
    failures
}

fn location(fence: &Fence, offset: u32) -> String {
    let (line, col) = line_column(fence.source.as_bytes(), offset as usize);
    format!("{}:{}:{col}", fence.path.display(), fence.line + line - 1)
}

fn item_name(item: Item<'_>) -> Option<&str> {
    item.tree
        .node(item.root)
        .token
        .map(|id| item.tree.token_text(item.text, id))
}

fn public_children(item: Item<'_>) -> Vec<NodeId> {
    use NodeKind::*;
    let children = item.tree.children(item.root);
    let opaque = children.iter().any(|&id| item.tree.node(id).kind == Opaque);
    children
        .iter()
        .copied()
        .filter(|&id| {
            let kind = item.tree.node(id).kind;
            !matches!(kind, Block | Drop)
                && (!opaque
                    || matches!(
                        kind,
                        Public | Opaque | Attribute | Generics | Bounds | Literal
                    ))
        })
        .collect()
}

// Compare syntax roles so field separators and layout are immaterial. Leaf
// tokens retain the full spelling of qualified paths and attribute arguments.
fn same_node(left: Item<'_>, a: NodeId, right: Item<'_>, b: NodeId) -> bool {
    let (x, y) = (left.tree.node(a), right.tree.node(b));
    let (xs, ys) = (left.tree.children(a), right.tree.children(b));
    if x.kind != y.kind || xs.len() != ys.len() {
        return false;
    }
    if xs.is_empty() {
        return tokens(left, a).eq(tokens(right, b));
    }
    x.token.map(|t| left.tree.token_text(left.text, t))
        == y.token.map(|t| right.tree.token_text(right.text, t))
        && xs
            .iter()
            .zip(ys)
            .all(|(&a, &b)| same_node(left, a, right, b))
}

fn tokens(item: Item<'_>, node: NodeId) -> impl Iterator<Item = &str> {
    item.tree
        .node(node)
        .origin
        .tokens
        .clone()
        .map(TokenId)
        .filter(move |t| !item.tree.tokens[t.0 as usize].kind.is_trivia())
        .map(move |t| item.tree.token_text(item.text, t))
}
