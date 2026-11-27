use super::documents::{Encoding, Position};
use super::navigation::range;
use super::rename::Graph;
use super::snapshot::EditorSnapshot;
use hirsel_core_ids::DefPath;
use hirsel_core_resolve::{
    BinderKind, DefinitionId, DefinitionKind, ModuleId, NameIndex, Namespace, ResolutionScratch,
    Target,
};
use hirsel_core_syntax::{Item, NodeId, NodeKind, Span, Symbol, Token, TokenKind, parse_bytes};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

#[derive(Clone)]
pub(super) struct Callable {
    pub(super) parameters: Vec<(String, Option<DefPath>)>,
    pub(super) result: Option<DefPath>,
}

#[derive(Default)]
pub(super) struct TypesAt {
    pub(super) nominal: BTreeMap<(u32, u32), DefPath>,
    pub(super) shown: BTreeMap<(u32, u32), Arc<str>>,
}

struct Binder {
    name: String,
    span: Span,
}

struct Scope {
    namespace: Namespace,
    binders: Vec<Binder>,
}

struct Query<'a> {
    graph: &'a Graph,
    module: ModuleId,
    namespace: Namespace,
    labels: BTreeMap<String, Value>,
}

impl EditorSnapshot {
    pub(super) fn complete(
        &self,
        path: &Path,
        position: Position,
        encoding: Encoding,
    ) -> Option<Value> {
        let text = self.origins.get(path)?;
        let offset = self.offset(path, position, encoding)?;
        let (graph_id, file_id) = *self.graph_sources.get(path)?;
        let graph = &self.graphs[graph_id];
        let file = &graph.files[file_id];
        let source = std::str::from_utf8(&text.bytes).ok()?;
        let tokens = &file.syntax.tree.tokens;
        let split = tokens.partition_point(|token| token.span.start < offset);
        let prior = split.checked_sub(1).and_then(|idx| tokens.get(idx));
        if prior.is_some_and(|token| {
            token.span.end > offset && token.kind != TokenKind::Ident
                || token.span.end == offset && token.kind.is_trivia()
        }) {
            return None;
        }
        let prefix_index = split
            .checked_sub(1)
            .filter(|&i| tokens[i].kind == TokenKind::Ident && offset <= tokens[i].span.end)
            .or_else(|| {
                tokens
                    .get(split)
                    .filter(|token| token.kind == TokenKind::Ident && token.span.start == offset)
                    .map(|_| split)
            });
        let prefix_token = prefix_index.map(|i| tokens[i]);
        let start = prefix_token.map_or(offset, |token| token.span.start);
        let end = prefix_token.map_or(offset, |token| token.span.end);
        let prefix = source.get(start as usize..offset as usize)?;
        let before = prefix_index.unwrap_or(split);
        let dot = before
            .checked_sub(1)
            .filter(|&i| tokens[i].kind == TokenKind::Dot);
        let mut query = Query {
            graph,
            module: file.module,
            namespace: Namespace::Value,
            labels: BTreeMap::new(),
        };

        // Fill the cursor token and ask the same lexical visitor that resolves a check.
        let syntax = &file.syntax;
        let owner = syntax
            .items()
            .enumerate()
            .find(|(_, item)| item.file_offset <= start && start <= item.range.end);
        let mut scope = None;
        let mut item_for_context = None;
        if let Some((ordinal, item)) = owner {
            scope = lexical_scope(graph, file_id, ordinal, item, Span { start, end });
            item_for_context = Some((ordinal, item));
        }
        if let Some(scope) = &scope {
            query.namespace = scope.namespace;
        }
        let facts = self.completion_types.get(path);
        let token_text =
            |token: &Token| source.get(token.span.start as usize..token.span.end as usize);
        if let Some(dot) = dot {
            let receiver = dot.checked_sub(1).and_then(|i| tokens.get(i))?;
            let mut head = dot - 1;
            while head >= 2
                && tokens[head - 1].kind == TokenKind::Dot
                && tokens[head - 2].kind == TokenKind::Ident
            {
                head -= 2;
            }
            let receiver_span = (tokens[head].span.start, receiver.span.end);
            let receiver_type = facts.and_then(|facts| {
                facts.nominal.get(&receiver_span).or_else(|| {
                    if receiver.kind == TokenKind::Ident {
                        return None;
                    }
                    facts
                        .nominal
                        .iter()
                        .filter(|&(&(start, end), _)| {
                            end == receiver.span.end && start <= receiver.span.start
                        })
                        .min_by_key(|(span, _)| span.0)
                        .map(|(_, ty)| ty)
                })
            });
            let mut nominal = receiver_type;
            let mut value_head = false;
            if let Some(scope) = &scope
                && let Some((_, item)) = item_for_context
                && let Some(name) = token_text(&tokens[head])
                && let Some(binder) = scope.binders.iter().find(|b| b.name == name)
            {
                value_head = true;
                nominal = nominal.or_else(|| {
                    let span = binder.span;
                    facts?
                        .nominal
                        .get(&(item.file_offset + span.start, item.file_offset + span.end))
                });
                if nominal.is_none() {
                    let (ordinal, _) = item_for_context?;
                    let definition = graph.index.item_definition(
                        file.module,
                        file.file,
                        ordinal as u32,
                        item,
                    )?;
                    let callable = graph
                        .callables
                        .get(&graph.index.definition_path(definition));
                    // Parameters have a written type even when recovery skipped the body.
                    nominal = match callable {
                        Some(callable) => callable
                            .parameters
                            .iter()
                            .find(|(param, _)| param == name)
                            .and_then(|(_, ty)| ty.as_ref()),
                        None => None,
                    };
                }
                if receiver_type.is_none()
                    && receiver_span.0 != receiver.span.start
                    && let Some(base) = nominal
                {
                    nominal =
                        tokens[head + 2..dot]
                            .iter()
                            .step_by(2)
                            .try_fold(base, |owner, token| {
                                graph
                                    .field_types
                                    .get(&(owner.clone(), token_text(token)?.to_owned()))?
                                    .1
                                    .as_ref()
                            });
                }
            }
            if let Some(ty) = nominal {
                query.members(ty, false);
            } else if !value_head && receiver.kind == TokenKind::Ident {
                let segments = tokens[head..dot]
                    .iter()
                    .step_by(2)
                    .map(token_text)
                    .collect::<Option<Vec<_>>>()?;
                if let Some(module) = graph
                    .index
                    .qualified_module(file.module, segments.iter().copied())
                {
                    query.module(module, true);
                } else if let Some(ty) = query.named_type(&segments) {
                    query.members(&ty, true);
                }
            }
        } else {
            query.module(file.module, false);
            let index = &graph.index;
            for i in 0..index.symbols.len() {
                let symbol = Symbol(i as u32);
                let name = index.symbols.text(symbol);
                if !name.starts_with(prefix) {
                    continue;
                }
                if query.namespace == Namespace::Type && hirsel_core_resolve::builtin(symbol) {
                    query
                        .labels
                        .entry(name.to_owned())
                        .or_insert_with(|| json!({"label": name, "kind": 7}));
                }
                if let Some(id) = index.prelude(symbol, query.namespace, false) {
                    query.definition(name, id);
                }
                if index.module_head(file.module, symbol).is_some() {
                    query
                        .labels
                        .entry(name.to_owned())
                        .or_insert_with(|| json!({"label": name, "kind": 9}));
                }
            }
            if let (Some(scope), Some((ordinal, item))) = (&scope, item_for_context) {
                for binder in &scope.binders {
                    let span = (
                        item.file_offset + binder.span.start,
                        item.file_offset + binder.span.end,
                    );
                    let detail = facts.and_then(|facts| facts.shown.get(&span));
                    let mut label = json!({"label": binder.name, "kind": 6});
                    if let Some(detail) = detail {
                        label["detail"] = json!(detail.as_ref());
                    }
                    query.labels.insert(binder.name.clone(), label);
                }
                if query.namespace == Namespace::Value
                    && let Some(ty) = expected(graph, file_id, ordinal, item, start)
                {
                    query.members(ty, true);
                }
            }
        }
        let edit_range = range(text, Span { start, end }, encoding)?;
        Some(Value::Array(
            query
                .labels
                .into_iter()
                .filter_map(|(name, mut item)| {
                    if !name.starts_with(prefix) {
                        return None;
                    }
                    item["textEdit"] = json!({"range": edit_range, "newText": name});
                    Some(item)
                })
                .collect(),
        ))
    }
}

// A synthetic path makes the parser recover an incomplete cursor token. A
// second probe resolves the item's candidate binders at that exact use site,
// so lexical scopes remain owned by the normal resolver.
fn lexical_scope(
    graph: &Graph,
    file: usize,
    ordinal: usize,
    item: Item<'_>,
    cursor: Span,
) -> Option<Scope> {
    let file = &graph.files[file];
    let mut probe = item.source_text().to_owned();
    let relative = cursor.start.checked_sub(item.file_offset)?;
    let end = cursor.end.checked_sub(item.file_offset)?;
    probe.get(relative as usize..end as usize)?;
    probe.replace_range(relative as usize..end as usize, "hirsel_editor_cursor");
    let mut index = graph.index.as_ref().clone();
    let mode = if index.module_is_foreign(file.module) {
        hirsel_core_syntax::ParseMode::Foreign
    } else {
        hirsel_core_syntax::ParseMode::Source
    };
    let parsed = parse_bytes(probe.as_bytes(), mode, Arc::make_mut(&mut index.symbols));
    let patched = parsed.syntax.items().next()?;
    let (span, namespace) = patched
        .range
        .nodes
        .clone()
        .map(NodeId)
        .filter_map(|id| {
            let node = patched.tree.node(id);
            let namespace = match node.kind {
                NodeKind::Path => Namespace::Value,
                NodeKind::TypePath => Namespace::Type,
                _ => return None,
            };
            let span = node.origin.span;
            (span.start <= relative && relative < span.end).then_some((span, namespace))
        })
        .min_by_key(|(span, _)| span.end - span.start)?;
    let mut scratch = ResolutionScratch::default();
    let resolution = scratch.resolve(&index, file.module, file.file, ordinal as u32, patched);
    let candidates = resolution
        .binders
        .iter()
        .filter(|binder| match namespace {
            Namespace::Type => binder.kind == BinderKind::TypeParameter,
            Namespace::Value => matches!(binder.kind, BinderKind::Local | BinderKind::Parameter),
        })
        .map(|binder| index.symbols.text(binder.name))
        .collect::<BTreeSet<_>>();
    if candidates.is_empty() {
        return Some(Scope {
            namespace,
            binders: Vec::new(),
        });
    }
    let candidates = format!(
        "({},)",
        candidates.into_iter().collect::<Vec<_>>().join(", ")
    );
    let start = span.start + patched.file_offset;
    let stop = span.end + patched.file_offset;
    probe.replace_range(start as usize..stop as usize, &candidates);
    let verified = parse_bytes(probe.as_bytes(), mode, Arc::make_mut(&mut index.symbols));
    let verified = verified.syntax.items().next()?;
    let actual = scratch.resolve(&index, file.module, file.file, ordinal as u32, verified);
    let binders = actual
        .names
        .iter()
        .filter_map(|usage| {
            let at = verified.tree.node(usage.node).origin.span.start + verified.file_offset;
            if at <= start || at as usize >= start as usize + candidates.len() {
                return None;
            }
            let Target::Binder(id) = usage.target else {
                return None;
            };
            let binder = actual.binders.get(id as usize)?;
            let original = resolution.binders.get(id as usize)?;
            if binder.name != original.name || binder.kind != original.kind {
                return None;
            }
            Some(Binder {
                name: index.symbols.text(binder.name).to_owned(),
                span: original.site.span,
            })
        })
        .collect();
    Some(Scope { namespace, binders })
}

fn expected<'a>(
    graph: &'a Graph,
    file: usize,
    ordinal: usize,
    item: Item<'_>,
    offset: u32,
) -> Option<&'a DefPath> {
    let relative = offset - item.file_offset;
    let resolutions = graph.resolutions.get(file)?.get(ordinal)?;
    let call = item
        .range
        .nodes
        .clone()
        .map(NodeId)
        .filter_map(|id| {
            let node = item.tree.node(id);
            if node.kind != NodeKind::Call {
                return None;
            }
            let (callee, args) = item.tree.children(id).split_first()?;
            let parameter = args.iter().position(|arg| {
                let span = item.tree.node(*arg).origin.span;
                span.start <= relative && relative <= span.end
            })?;
            let target = resolutions
                .names
                .iter()
                .find(|usage| usage.node == *callee)?
                .target;
            let Target::Definition(def) = target else {
                return None;
            };
            let path = graph.index.definition_path(def);
            Some((
                node.origin.span.end - node.origin.span.start,
                graph
                    .callables
                    .get(&path)?
                    .parameters
                    .get(parameter)?
                    .1
                    .as_ref()?,
            ))
        })
        .min_by_key(|(width, _)| *width);
    if let Some((_, ty)) = call {
        return Some(ty);
    }
    let body = item
        .tree
        .children(item.root)
        .iter()
        .find(|&&id| item.tree.node(id).kind == NodeKind::Block)?;
    let tail = item.tree.children(*body).last()?;
    let span = item.tree.node(*tail).origin.span;
    if relative < span.start || relative > span.end {
        return None;
    }
    let file = &graph.files[file];
    let definition = graph
        .index
        .item_definition(file.module, file.file, ordinal as u32, item)?;
    graph
        .callables
        .get(&graph.index.definition_path(definition))?
        .result
        .as_ref()
}

impl Query<'_> {
    fn definition(&mut self, name: &str, id: DefinitionId) {
        let def = self.graph.index.get(id);
        let kind = match def.kind {
            DefinitionKind::Function => 3,
            DefinitionKind::Constant => 21,
            DefinitionKind::Type => 7,
            DefinitionKind::Clone => return,
        };
        let mut label = json!({"label": name, "kind": kind});
        if let Some(source) = def.site.source
            && let Some(file) = self.graph.files.iter().find(|file| file.file == source)
            && def.site.item < file.syntax.items.len() as u32
        {
            let item = file.syntax.item(def.site.item as usize);
            label["detail"] = json!(crate::inspect::signature(item, def.kind, def.opaque));
            if let Some(doc) = crate::inspect::doc_comment(item) {
                label["documentation"] = json!({"kind": "markdown", "value": doc});
            }
        }
        self.labels.entry(name.to_owned()).or_insert(label);
    }

    fn module(&mut self, target: ModuleId, qualified: bool) {
        let index = &self.graph.index;
        for name in index.member_names_in(target) {
            let id = if qualified {
                let mut candidates =
                    index.members_named(self.module, target, name, self.namespace, false);
                let one = candidates.next();
                if candidates.next().is_some() {
                    continue;
                }
                one
            } else {
                index.member(target, name, self.namespace, false)
            };
            if let Some(id) = id.filter(|id| visible(index, self.module, *id)) {
                self.definition(index.symbols.text(name), id);
            }
        }
        if qualified {
            for child in index.child_names(target) {
                let name = index.symbols.text(child);
                self.labels
                    .entry(name.to_owned())
                    .or_insert_with(|| json!({"label": name, "kind": 9}));
            }
        }
    }

    fn named_type(&self, segments: &[&str]) -> Option<DefPath> {
        let (last, qualifier) = segments.split_last()?;
        let index = &self.graph.index;
        let name = index.symbols.find(last)?;
        let id = if qualifier.is_empty() {
            index
                .member(self.module, name, Namespace::Type, false)
                .or_else(|| index.prelude(name, Namespace::Type, false))?
        } else {
            let module = index.qualified_module(self.module, qualifier.iter().copied())?;
            let mut defs = index.members_named(self.module, module, name, Namespace::Type, false);
            let first = defs.next()?;
            if defs.next().is_some() {
                return None;
            }
            first
        };
        visible(index, self.module, id).then(|| index.definition_path(id))
    }

    fn members(&mut self, ty: &DefPath, variants: bool) {
        let index = &self.graph.index;
        let Some(id) = index
            .module_at(&ty.0.package, &ty.0.modules)
            .and_then(|module| index.definition(module, index.symbols.find(&ty.0.name)?))
        else {
            return;
        };
        let definition = index.get(id);
        if definition.opaque && definition.site.module != self.module {
            return;
        }
        let names = if variants {
            index.variant_names(id)
        } else {
            index.field_names(id)
        };
        for &name in names {
            let name = index.symbols.text(name);
            let mut label = json!({"label": name, "kind": if variants { 20 } else { 5 }});
            if let Some((ty, _)) = self.graph.field_types.get(&(ty.clone(), name.to_owned())) {
                label["detail"] = json!(ty.as_ref());
            }
            self.labels.entry(name.to_owned()).or_insert(label);
        }
    }
}

fn visible(index: &NameIndex, from: ModuleId, id: DefinitionId) -> bool {
    let def = index.get(id);
    def.public || def.site.module == from
}
