use super::cache::{Check, Stats};
use super::diagnostics::{Diagnostic, InputOrigins};
use super::documents::{Encoding, Position, Text};
use super::hover::{self, Authority};
use super::navigation::{self, Location, absolute};
use super::workspace::EditorInputs;
use crate::package_resolve::Resolution as WorkspaceResolution;
use crate::reachability::authority::Cards;
use crate::{CheckProduct, ContentHash, FileProduct};
use hirsel_core_ids::{DefPath, PackageId};
use hirsel_core_resolve::{
    Definition, DefinitionId, DefinitionKind, ResolvedUse, SourceId, Target,
};
use hirsel_core_syntax::{Item, NodeKind, ParseMode, Span, TokenId, TokenKind};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

pub(super) struct EditorSnapshot {
    pub(super) inputs: EditorInputs,
    pub(super) origins: BTreeMap<PathBuf, Text>,
    pub(super) diagnostics: Vec<Diagnostic>,
    pub(super) modes: BTreeMap<PathBuf, ParseMode>,
    pub(super) immutable: BTreeSet<PathBuf>,
    pub(super) stats: Stats,
    sources: BTreeMap<PathBuf, SourceFacts>,
}

struct SourceFacts {
    facts: Vec<Fact>,
    cards: Arc<Cards>,
}

pub(super) struct Fact {
    pub(super) span: Span,
    pub(super) ty: Option<Arc<str>>,
    pub(super) target: Option<Arc<Declaration>>,
    pub(super) authority: Option<Authority>,
    pub(super) unresolved: bool,
}

pub(super) struct Declaration {
    location: Option<Location>,
    pub(super) signature: Option<String>,
    pub(super) doc: Option<String>,
    ty: Option<Arc<str>>,
}

impl EditorSnapshot {
    pub(super) fn new(inputs: EditorInputs) -> Self {
        let origins = inputs
            .overlay
            .iter()
            .filter(|(path, _)| inputs.owns(path))
            .map(|(path, document)| (path.clone(), document.text.clone()))
            .collect();
        Self {
            inputs,
            origins,
            diagnostics: Vec::new(),
            modes: BTreeMap::new(),
            immutable: BTreeSet::new(),
            stats: Stats::default(),
            sources: BTreeMap::new(),
        }
    }

    pub(super) fn query(
        &self,
        path: &Path,
        position: Position,
        definition: bool,
        links: bool,
        encoding: Encoding,
    ) -> Option<Value> {
        let text = self.origins.get(path)?;
        let offset = u32::try_from(text.offset(position, encoding)?).ok()?;
        let source = self.sources.get(path)?;
        let matching = source
            .facts
            .iter()
            .filter(|fact| fact.span.start <= offset && offset < fact.span.end);
        if definition {
            let (span, target) = matching
                .filter_map(|fact| Some((fact.span, fact.target.as_ref()?.location.as_ref()?)))
                .min_by_key(|(span, _)| span.end - span.start)?;
            navigation::definition(
                text,
                span,
                target,
                &self.origins,
                &self.immutable,
                links,
                encoding,
            )
        } else {
            let fact = matching.min_by_key(|fact| fact.span.end - fact.span.start)?;
            hover::hover(fact, text, &source.cards, encoding)
        }
    }

    pub(super) fn retain(
        &mut self,
        product: &CheckProduct,
        check: &Check,
        origins: &InputOrigins,
        wanted: &BTreeSet<PathBuf>,
        workspace: &WorkspaceResolution,
        source_cache: &Path,
    ) {
        let mut context = Context {
            product,
            check,
            workspace,
            source_cache,
            files: BTreeMap::new(),
            definitions: BTreeMap::new(),
            members: BTreeMap::new(),
        };
        for file in &product.files {
            let package = product.index.module_path(file.module).package;
            if let Some(origin) = file
                .name
                .as_ref()
                .and_then(|name| origins.get(&(package, name.clone())))
            {
                context.files.insert(file.file, origin.path.clone());
            }
        }
        let cards = Arc::new(product.authority_cards.clone());
        for (file, resolutions) in product.files.iter().zip(&check.resolutions) {
            let Some(path) = context.files.get(&file.file).cloned() else {
                continue;
            };
            if self.sources.contains_key(&path) && !wanted.contains(&path) {
                continue;
            }
            let Some(text) = self.origins.get(&path) else {
                continue;
            };
            let authored = text.bytes.len();
            let package = product.index.module_path(file.module).package;
            let mut facts = Vec::new();
            for (ordinal, (item, resolution)) in file.syntax.items().zip(resolutions).enumerate() {
                if item.range.end as usize > authored {
                    continue;
                }
                let ordinal = ordinal as u32;
                let invalid = Invalid::new(product, file.file, ordinal);
                let types = check
                    .types
                    .get(&(file.file, ordinal))
                    .map(AsRef::as_ref)
                    .unwrap_or_default();
                let types: BTreeMap<_, _> = types
                    .iter()
                    .map(|(span, ty, nominal)| {
                        (
                            (span.start, span.end),
                            (Arc::<str>::from(ty.as_str()), nominal.as_ref()),
                        )
                    })
                    .collect();
                context.item(
                    self, &mut facts, file, item, ordinal, &path, &package, resolution, &types,
                    &invalid,
                );
            }
            self.sources.insert(
                path,
                SourceFacts {
                    facts,
                    cards: Arc::clone(&cards),
                },
            );
        }
    }
}

type Types<'a> = BTreeMap<(u32, u32), (Arc<str>, Option<&'a DefPath>)>;

struct Context<'a> {
    product: &'a CheckProduct,
    check: &'a Check,
    workspace: &'a WorkspaceResolution,
    source_cache: &'a Path,
    files: BTreeMap<SourceId, PathBuf>,
    definitions: BTreeMap<DefinitionId, Arc<Declaration>>,
    members: BTreeMap<(DefPath, String), Arc<Declaration>>,
}

impl Context<'_> {
    fn item(
        &mut self,
        snapshot: &mut EditorSnapshot,
        facts: &mut Vec<Fact>,
        file: &FileProduct,
        item: Item<'_>,
        ordinal: u32,
        path: &Path,
        package: &PackageId,
        resolution: &hirsel_core_resolve::Resolution,
        types: &Types<'_>,
        invalid: &Invalid,
    ) {
        let mut local = BTreeMap::new();
        for binder in &resolution.binders {
            let selection = absolute(item, binder.site.span);
            let ty = types
                .get(&(binder.site.span.start, binder.site.span.end))
                .map(|(ty, _)| Arc::clone(ty));
            let declaration = Arc::new(Declaration {
                location: Some(Location {
                    path: path.to_owned(),
                    selection,
                    declaration: selection,
                }),
                signature: None,
                doc: None,
                ty: ty.clone(),
            });
            facts.push(Fact {
                span: selection,
                ty,
                target: Some(Arc::clone(&declaration)),
                authority: None,
                unresolved: false,
            });
            local.insert(binder.id, declaration);
        }
        if let Some(id) = self
            .product
            .index
            .item_definition(file.module, file.file, ordinal, item)
        {
            let definition = self.product.index.get(id);
            if definition.site.source == Some(file.file) && definition.site.item == ordinal {
                let target = self.declaration(snapshot, id);
                let ty = target.ty.clone();
                facts.push(Fact {
                    span: absolute(item, definition.site.span),
                    target: Some(target),
                    ty,
                    authority: None,
                    unresolved: false,
                });
            }
        }
        let calls = item
            .range
            .nodes
            .clone()
            .map(hirsel_core_syntax::NodeId)
            .filter_map(|id| {
                let node = item.tree.node(id);
                (node.kind == NodeKind::Call).then(|| (item.tree.children(id)[0], node.origin.span))
            })
            .collect::<BTreeMap<_, _>>();
        for &usage in &resolution.names {
            self.usage(
                snapshot,
                facts,
                item,
                package,
                usage,
                &local,
                types,
                calls.get(&usage.node).copied(),
                invalid,
            );
        }
        for (&(start, end), (ty, _)) in types {
            if end <= item.range.end - item.file_offset {
                facts.push(Fact {
                    span: absolute(item, Span { start, end }),
                    ty: Some(Arc::clone(ty)),
                    target: None,
                    authority: None,
                    unresolved: false,
                });
            }
        }
    }

    fn usage(
        &mut self,
        snapshot: &mut EditorSnapshot,
        facts: &mut Vec<Fact>,
        item: Item<'_>,
        package: &PackageId,
        usage: ResolvedUse,
        locals: &BTreeMap<u32, Arc<Declaration>>,
        types: &Types<'_>,
        call: Option<Span>,
        invalid: &Invalid,
    ) {
        let node = item.tree.node(usage.node);
        let mut names = node
            .origin
            .tokens
            .clone()
            .take_while(|&token| usage.projections.start == 0 || token < usage.projections.start)
            .filter(|&i| item.tree.tokens[i as usize].kind == TokenKind::Ident)
            .map(TokenId);
        let first = names.next().or(node.token);
        let last = names.last().or(first);
        let Some(token) = last else { return };
        let written = |token| item.tree.token_text(item.text, token);
        let first_name = first.map(written);
        let name_span = item.token_span(token);
        let refused_call = call.is_some_and(|span| invalid.intersects(span));
        let mut authority = None;
        let target = match usage.target {
            Target::Definition(id) => {
                let def = self.product.index.get(id);
                let path = self.product.index.definition_path(id);
                if path.0.package != *package {
                    let export = path
                        .0
                        .modules
                        .iter()
                        .map(String::as_str)
                        .chain([path.0.name.as_str()])
                        .collect::<Vec<_>>()
                        .join("/");
                    let unavailable = !def.public
                        || match self.product.definitions.get(&path) {
                            Some(body) if def.kind == DefinitionKind::Function => {
                                body.check.function.is_none() || body.check.template.is_some()
                            }
                            _ => {
                                self.product
                                    .index
                                    .module_containment(def.site.module)
                                    .is_none()
                                    && def.kind == DefinitionKind::Function
                            }
                        };
                    authority = Some(Authority {
                        ceiling: self.ceiling(package, first_name, &path.0.package),
                        package: path.0.package,
                        export: matches!(
                            def.kind,
                            DefinitionKind::Function | DefinitionKind::Constant
                        )
                        .then_some(export),
                        unresolved: refused_call || unavailable,
                    });
                }
                Some(self.declaration(snapshot, id))
            }
            Target::Binder(id) => locals.get(&id).cloned(),
            Target::ContextualConstructor { .. } => {
                let nominal = call
                    .and_then(|span| types.get(&(span.start, span.end)))
                    .or_else(|| types.get(&(node.origin.span.start, node.origin.span.end)))
                    .and_then(|(_, nominal)| *nominal);
                nominal.and_then(|path| self.member(snapshot, path, written(token)))
            }
            Target::Builtin(_) | Target::Module(_) | Target::Missing => None,
        };
        let unresolved = matches!(usage.target, Target::Missing)
            || (refused_call && authority.is_none() && !matches!(usage.target, Target::Binder(_)));
        let ty = types
            .get(&(node.origin.span.start, node.origin.span.end))
            .map(|(ty, _)| Arc::clone(ty))
            .or_else(|| target.as_ref()?.ty.clone());
        if target.is_some() || authority.is_some() || ty.is_some() || unresolved {
            facts.push(Fact {
                span: absolute(item, name_span),
                target,
                ty,
                authority,
                unresolved,
            });
        }
        if let Some(first) = first.filter(|first| *first != token)
            && let Some((dependency, ceiling)) = self.binding(package, written(first))
        {
            let confirmed = match usage.target {
                Target::Definition(id) => {
                    self.product.index.definition_path(id).0.package == dependency
                }
                Target::Missing => true,
                _ => false,
            };
            if confirmed {
                facts.push(Fact {
                    span: absolute(item, item.token_span(first)),
                    target: None,
                    ty: None,
                    unresolved: false,
                    authority: Some(Authority {
                        package: dependency,
                        export: None,
                        ceiling,
                        unresolved,
                    }),
                });
            }
        }
        if let Target::Binder(id) = usage.target
            && let Some(binder) = locals.get(&id).and_then(|decl| decl.location.as_ref())
        {
            let start = binder.selection.start - item.file_offset;
            let end = binder.selection.end - item.file_offset;
            let mut nominal = types
                .get(&(start, end))
                .and_then(|(_, nominal)| *nominal)
                .cloned();
            for i in usage.projections.start..usage.projections.end {
                if item.tree.tokens[i as usize].kind != TokenKind::Ident {
                    continue;
                }
                let Some(path) = nominal.take() else { break };
                let name = written(TokenId(i));
                let Some(target) = self.member(snapshot, &path, name) else {
                    break;
                };
                facts.push(Fact {
                    span: absolute(item, item.token_span(TokenId(i))),
                    ty: target.ty.clone(),
                    target: Some(target),
                    authority: None,
                    unresolved: false,
                });
                nominal = self
                    .check
                    .field_types
                    .get(&(path, name.to_owned()))
                    .and_then(|(_, nominal)| nominal.clone());
            }
        }
    }

    fn binding(&self, package: &PackageId, name: &str) -> Option<(PackageId, Option<u16>)> {
        if let Some((_, member)) = self
            .workspace
            .workspace
            .members
            .iter()
            .find(|(id, _)| id.compiler_id() == *package)
        {
            let id = member.aliases.get(name)?;
            return Some((id.compiler_id(), member.ceilings.get(name).copied()));
        }
        let (_, root) = self
            .workspace
            .published
            .iter()
            .find(|(id, _)| id.compiler_id() == *package)?;
        let (id, _) = self.workspace.libraries[root].content.bindings.get(name)?;
        Some((id.compiler_id(), None))
    }

    fn ceiling(&self, package: &PackageId, name: Option<&str>, target: &PackageId) -> Option<u16> {
        let (dependency, ceiling) = self.binding(package, name?)?;
        (&dependency == target).then_some(ceiling).flatten()
    }

    fn declaration(&mut self, snapshot: &mut EditorSnapshot, id: DefinitionId) -> Arc<Declaration> {
        if let Some(declaration) = self.definitions.get(&id) {
            return Arc::clone(declaration);
        }
        let def = self.product.index.get(id);
        let path = self.product.index.definition_path(id);
        let ty = self.check.declared_types.get(&path).cloned();
        let site = self.declaration_file(snapshot, def);
        let (location, signature, doc) = site
            .as_ref()
            .and_then(|(file, path)| {
                let item = file
                    .syntax
                    .items
                    .get(def.site.item as usize)
                    .map(|_| file.syntax.item(def.site.item as usize))?;
                let selection = absolute(item, def.site.span);
                let declaration = absolute(item, item.tree.node(item.root).origin.span);
                let text = snapshot.origins.get(path)?;
                if item.range.has_errors
                    || declaration.end as usize > text.bytes.len()
                    || selection.end > declaration.end
                {
                    return None;
                }
                Some((
                    Some(Location {
                        path: path.clone(),
                        selection,
                        declaration,
                    }),
                    Some(crate::inspect::signature(item, def.kind, def.opaque).to_owned()),
                    crate::inspect::doc_comment(item),
                ))
            })
            .unwrap_or_default();
        let declaration = Arc::new(Declaration {
            location,
            signature,
            doc,
            ty,
        });
        self.definitions.insert(id, Arc::clone(&declaration));
        declaration
    }

    fn declaration_file(
        &mut self,
        snapshot: &mut EditorSnapshot,
        def: &Definition,
    ) -> Option<(Arc<FileProduct>, PathBuf)> {
        let source = def.site.source?;
        if let Some(file) = self.product.files.iter().find(|file| file.file == source) {
            return Some((Arc::clone(file), self.files.get(&source)?.clone()));
        }
        let image = self.product.image.as_ref()?;
        let (name, text) = image.source_file(source).ok()?;
        let file = image.declaration_file(source).ok()?;
        let name = name.as_deref()?;
        let bytes = bytes::Bytes::from(text);
        let digest = ContentHash::of_bytes(&bytes);
        let path = immutable_path(self.source_cache, image.identity, digest, name)?;
        snapshot.immutable.insert(path.clone());
        snapshot
            .origins
            .entry(path.clone())
            .or_insert_with(|| Text::new(bytes));
        Some((file, path))
    }

    fn member(
        &mut self,
        snapshot: &mut EditorSnapshot,
        owner: &DefPath,
        name: &str,
    ) -> Option<Arc<Declaration>> {
        let key = (owner.clone(), name.to_owned());
        if let Some(declaration) = self.members.get(&key) {
            return Some(Arc::clone(declaration));
        }
        let index = &self.product.index;
        let module = index.module_at(&owner.0.package, &owner.0.modules)?;
        let def = index.get(index.definition(module, index.symbols.find(&owner.0.name)?)?);
        let (file, path) = self.declaration_file(snapshot, def)?;
        let item = file.syntax.item(def.site.item as usize);
        let child = item
            .tree
            .children(item.root)
            .iter()
            .filter(|&&id| {
                matches!(
                    item.tree.node(id).kind,
                    NodeKind::Fields | NodeKind::Variants
                )
            })
            .flat_map(|&id| item.tree.children(id))
            .map(|&id| item.tree.node(id))
            .find(|child| {
                matches!(child.kind, NodeKind::Field | NodeKind::Variant)
                    && child
                        .token
                        .is_some_and(|token| item.tree.token_text(item.text, token) == name)
            })?;
        let selection = absolute(item, item.token_span(child.token?));
        let declaration = absolute(item, child.origin.span);
        let ty = self
            .check
            .field_types
            .get(&key)
            .map(|(ty, _)| Arc::clone(ty));
        let target = Arc::new(Declaration {
            location: Some(Location {
                path,
                selection,
                declaration,
            }),
            signature: Some(
                item.text[declaration.start as usize..declaration.end as usize].to_owned(),
            ),
            doc: None,
            ty,
        });
        self.members.insert(key, Arc::clone(&target));
        Some(target)
    }
}

struct Invalid(Vec<Span>);

impl Invalid {
    fn new(product: &CheckProduct, file: SourceId, item: u32) -> Self {
        let spans = product
            .diagnostics
            .iter()
            .map(|r| r.primary)
            .chain(product.incomplete.iter().map(|r| r.primary))
            .chain(product.resources.iter().map(|r| r.primary))
            .filter(|site| site.source == Some(file) && site.item == item)
            .map(|site| site.span)
            .collect();
        Self(spans)
    }

    fn intersects(&self, span: Span) -> bool {
        self.0
            .iter()
            .any(|other| other.start <= span.end && other.end >= span.start)
    }
}

pub(super) fn immutable_path(
    cache: &Path,
    root: ContentHash,
    digest: ContentHash,
    file: &str,
) -> Option<PathBuf> {
    let file = Path::new(file);
    if file.as_os_str().is_empty()
        || !file
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
    {
        return None;
    }
    Some(
        cache
            .join(root.to_string())
            .join(digest.to_string())
            .join(file),
    )
}
