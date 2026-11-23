use super::{Applicability, AvailableMember, CheckProduct, Edit, Fix, source_name};
use crate::FileProduct;
use hirsel_core_resolve::{
    DefinitionId, DefinitionKind, ModuleId, ModulePath, NameIndex, NameScope, Namespace, Refusal,
    UnresolvedName,
};
use hirsel_core_syntax::{NodeKind, Span, Symbol, TokenKind};
use std::collections::{BTreeMap, BTreeSet};
use std::io;

const MAX_FIXES: usize = 3;
const SMALL_MODULE: usize = 5;

#[derive(Clone, Copy)]
struct Signature {
    definition: DefinitionId,
    part: Option<Symbol>,
}

impl From<DefinitionId> for Signature {
    fn from(definition: DefinitionId) -> Self {
        Self {
            definition,
            part: None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Rank {
    Case,
    ModuleHead(usize),
    ExactMember {
        distance: usize,
        segments: usize,
        length_delta: usize,
    },
    Spelling(usize),
}

struct Candidate {
    rank: Rank,
    span: Span,
    text: String,
    signature: Signature,
}

impl CheckProduct {
    pub(super) fn name_candidates(
        &self,
        refusal: &Refusal,
        lookup: &UnresolvedName,
    ) -> io::Result<(Vec<Fix>, Vec<AvailableMember>)> {
        let Some(file) = self
            .files
            .iter()
            .find(|f| Some(f.file) == refusal.primary.source)
        else {
            return Ok((Vec::new(), Vec::new()));
        };
        let base = file
            .syntax
            .items
            .get(refusal.primary.item as usize)
            .map_or(0, |item| item.file_offset);
        let source = &file.syntax.source;
        if source.get((base + lookup.span.start) as usize..(base + lookup.span.end) as usize)
            != Some(lookup.written.as_str())
        {
            return Ok((Vec::new(), Vec::new()));
        }

        let mut search = Search {
            index: &self.index,
            from: refusal.primary.module,
            lookup,
            candidates: Vec::new(),
            members: BTreeMap::new(),
        };
        match &lookup.scope {
            NameScope::ModuleMember {
                module,
                namespace,
                constructor,
            } => {
                if let Some(module) = search.module_at(module) {
                    search.module_members(module, *namespace, *constructor);
                    let qualifier =
                        path_segments(file, base, refusal.primary.span.start, lookup.span.start);
                    search.exact_members(Some(module), &qualifier, *namespace, *constructor);
                }
            }
            NameScope::UnqualifiedPath {
                namespace,
                constructor,
                ..
            } => {
                let qualifier =
                    path_segments(file, base, refusal.primary.span.start, lookup.span.start);
                search.module_heads(&qualifier, *namespace, *constructor);
                search.exact_members(None, &qualifier, *namespace, *constructor);
            }
            NameScope::ModulePrefix {
                module,
                children_of,
                namespace,
            } => {
                let tail = path_segments(file, base, lookup.span.end, refusal.primary.span.end);
                search.module_prefix(module, children_of, *namespace, &tail);
            }
            NameScope::Field(path) | NameScope::Variant(path) => {
                let field = matches!(lookup.scope, NameScope::Field(_));
                let path = &path.0;
                let definition = self
                    .index
                    .module_at(&path.package, &path.modules)
                    .zip(self.index.symbols.find(&path.name))
                    .and_then(|(module, name)| {
                        self.index.member(module, name, Namespace::Type, false)
                    });
                if let Some(definition) = definition {
                    search.type_members(definition, field);
                }
            }
        }

        let module = source_name(file.name.as_deref(), &self.index.module_path(file.module));
        search
            .candidates
            .sort_unstable_by(|a, b| (a.rank, &a.text).cmp(&(b.rank, &b.text)));
        let mut seen = BTreeSet::new();
        let fixes = search
            .candidates
            .into_iter()
            .filter(|candidate| {
                seen.insert((candidate.signature.definition, candidate.signature.part))
            })
            .take(MAX_FIXES)
            .map(|candidate| {
                Ok(Fix {
                    applicability: Applicability::Maybe,
                    signature: Some(self.candidate_signature(candidate.signature)?),
                    edits: vec![Edit {
                        module: module.clone(),
                        span: Span {
                            start: base + candidate.span.start,
                            end: base + candidate.span.end,
                        },
                        text: candidate.text,
                    }],
                })
            })
            .collect::<io::Result<Vec<_>>>()?;
        let members = if fixes.is_empty() && search.members.len() <= SMALL_MODULE {
            search
                .members
                .into_iter()
                .map(|(name, signature)| {
                    Ok(AvailableMember {
                        name,
                        signature: self.candidate_signature(signature)?,
                    })
                })
                .collect::<io::Result<_>>()?
        } else {
            Vec::new()
        };
        Ok((fixes, members))
    }

    fn candidate_signature(&self, signature: Signature) -> io::Result<String> {
        let definition = self.index.get(signature.definition);
        let Some(source) = definition.site.source else {
            return Err(io::Error::other("candidate declaration has no source"));
        };
        let file = if let Some(file) = self.files.iter().find(|f| f.file == source) {
            file.clone()
        } else {
            let image = self
                .image
                .as_ref()
                .ok_or_else(|| io::Error::other("candidate file is absent"))?;
            image.declaration_file(source)?
        };
        let item = file.syntax.item(definition.site.item as usize);
        if let Some(part) = signature.part {
            for &list in item.tree.children(item.root) {
                if !matches!(
                    item.tree.node(list).kind,
                    NodeKind::Fields | NodeKind::Variants
                ) {
                    continue;
                }
                for &member in item.tree.children(list) {
                    let node = item.tree.node(member);
                    if node.token.is_some_and(|token| item.symbol(token) == part) {
                        let span = node.origin.span;
                        return Ok(
                            item.source_text()[span.start as usize..span.end as usize].into()
                        );
                    }
                }
            }
            return Err(io::Error::other(
                "candidate member is absent from its declaration",
            ));
        }
        Ok(crate::inspect::signature(item, definition.kind, definition.opaque).into())
    }
}

struct Search<'a> {
    index: &'a NameIndex,
    from: ModuleId,
    lookup: &'a UnresolvedName,
    candidates: Vec<Candidate>,
    members: BTreeMap<String, Signature>,
}

impl Search<'_> {
    fn module_at(&self, path: &ModulePath) -> Option<ModuleId> {
        self.index.module_at(&path.package, &path.modules)
    }

    fn spelling(&mut self, name: Symbol, signature: Signature) {
        let text = self.index.symbols.text(name);
        if let Some(rank) = spelling_rank(&self.lookup.written, text) {
            self.candidates.push(Candidate {
                rank,
                span: self.lookup.span,
                text: text.into(),
                signature,
            });
        }
    }

    fn module_members(&mut self, module: ModuleId, namespace: Namespace, constructor: bool) {
        for name in self.index.member_names_in(module) {
            if let Some(definition) = self.member(module, name, namespace, constructor) {
                self.spelling(name, definition.into());
                if self.index.get(definition).public {
                    self.members
                        .insert(self.index.symbols.text(name).into(), definition.into());
                }
            }
        }
    }

    fn module_prefix(
        &mut self,
        module: &ModulePath,
        parent: &ModulePath,
        namespace: Namespace,
        tail: &[Segment<'_>],
    ) {
        let Some((module, parent)) = self.module_at(module).zip(self.module_at(parent)) else {
            return;
        };
        let children = self
            .index
            .child_names(module)
            .chain(self.index.child_names(parent));
        for name in children {
            let child = self
                .index
                .child(parent, name)
                .or_else(|| self.index.child(module, name));
            let definition = child
                .filter(|&child| child != module)
                .and_then(|child| self.tail_member(child, tail, namespace, false));
            if let Some(definition) = definition {
                self.spelling(name, definition.into());
            }
        }
        if namespace == Namespace::Value {
            for name in self.index.member_names_in(module) {
                if self.index.child(parent, name).is_none()
                    && self.index.child(module, name).is_none()
                    && let Some(definition) = self.member(module, name, namespace, false)
                    && self.index.get(definition).kind == DefinitionKind::Constant
                {
                    self.spelling(name, definition.into());
                }
            }
        }
    }

    fn type_members(&mut self, definition: DefinitionId, field: bool) {
        let row = self.index.get(definition);
        let module = row.site.module;
        if module != self.from
            && (row.opaque
                || !row.public
                || !self
                    .index
                    .reachable(self.from, self.index.module_package(module)))
        {
            return;
        }
        if field {
            for &name in self.index.field_names(definition) {
                self.spelling(
                    name,
                    Signature {
                        definition,
                        part: Some(name),
                    },
                );
            }
        } else {
            for &name in self.index.variant_names(definition) {
                if self
                    .index
                    .member(self.from, name, Namespace::Value, false)
                    .is_none()
                    && self.index.prelude(name, Namespace::Value, false).is_none()
                {
                    self.spelling(
                        name,
                        Signature {
                            definition,
                            part: Some(name),
                        },
                    );
                }
            }
        }
    }

    fn module_heads(&mut self, qualifier: &[Segment<'_>], namespace: Namespace, constructor: bool) {
        let Some((head, tail)) = qualifier.split_first() else {
            return;
        };
        let names: BTreeSet<_> = self
            .index
            .modules
            .iter()
            .map(|module| module.name)
            .collect();
        for name in names {
            let text = self.index.symbols.text(name);
            let Some(rank) = spelling_rank(head.text, text) else {
                continue;
            };
            let path = std::iter::once(text).chain(tail.iter().map(|segment| segment.text));
            let Some(module) = self.index.qualified_module(self.from, path) else {
                continue;
            };
            let Some(definition) = self
                .index
                .symbols
                .find(&self.lookup.written)
                .and_then(|name| self.member(module, name, namespace, constructor))
            else {
                continue;
            };
            self.candidates.push(Candidate {
                rank: match rank {
                    Rank::Spelling(distance) => Rank::ModuleHead(distance),
                    other => other,
                },
                span: head.span,
                text: text.into(),
                signature: definition.into(),
            });
        }
    }

    fn exact_members(
        &mut self,
        original: Option<ModuleId>,
        qualifier: &[Segment<'_>],
        namespace: Namespace,
        constructor: bool,
    ) {
        let Some((first, last)) = qualifier.first().zip(qualifier.last()) else {
            return;
        };
        let Some(name) = self.index.symbols.find(&self.lookup.written) else {
            return;
        };
        let written: Vec<_> = qualifier.iter().map(|segment| segment.text).collect();
        let written_text = written.join(".");
        for (ordinal, _) in self.index.modules.iter().enumerate() {
            let module = ModuleId(ordinal as u32);
            if Some(module) == original {
                continue;
            }
            let Some(definition) = self.member(module, name, namespace, constructor) else {
                continue;
            };
            let path = self.index.module_path(module);
            let bare: Vec<_> = path.modules.iter().map(String::as_str).collect();
            let packaged: Vec<_> = std::iter::once(path.package.name.as_str())
                .chain(bare.iter().copied())
                .collect();
            let Some(path) = [bare, packaged].into_iter().find(|path| {
                self.index.qualified_module(self.from, path.iter().copied()) == Some(module)
            }) else {
                continue;
            };
            if path == written {
                continue;
            }
            let path_text = path.join(".");
            let rank = Rank::ExactMember {
                distance: damerau_distance(&written_text, &path_text),
                segments: path.len(),
                length_delta: written_text
                    .chars()
                    .count()
                    .abs_diff(path_text.chars().count()),
            };
            let (span, text) = if let Some(prefix) = path.strip_suffix(written.as_slice()) {
                (
                    Span {
                        start: first.span.start,
                        end: first.span.start,
                    },
                    format!("{}.", prefix.join(".")),
                )
            } else {
                (
                    Span {
                        start: first.span.start,
                        end: last.span.end,
                    },
                    path_text,
                )
            };
            self.candidates.push(Candidate {
                rank,
                span,
                text,
                signature: definition.into(),
            });
        }
    }

    fn tail_member(
        &self,
        mut module: ModuleId,
        tail: &[Segment<'_>],
        namespace: Namespace,
        constructor: bool,
    ) -> Option<DefinitionId> {
        let (last, prefix) = tail.split_last()?;
        for segment in prefix {
            module = self
                .index
                .child(module, self.index.symbols.find(segment.text)?)?;
        }
        self.member(
            module,
            self.index.symbols.find(last.text)?,
            namespace,
            constructor,
        )
    }

    fn member(
        &self,
        module: ModuleId,
        name: Symbol,
        namespace: Namespace,
        constructor: bool,
    ) -> Option<DefinitionId> {
        let mut members = self
            .index
            .members_named(self.from, module, name, namespace, false);
        let (id, ambiguous) = if let Some(id) = members.next() {
            (id, members.next().is_some())
        } else if constructor {
            let mut types =
                self.index
                    .members_named(self.from, module, name, Namespace::Type, true);
            (types.next()?, types.next().is_some())
        } else {
            return None;
        };
        let row = self.index.get(id);
        (!ambiguous
            && (row.site.module == self.from || row.public && !(constructor && row.opaque))
            && (!constructor
                || row.kind != DefinitionKind::Type
                || self.index.variant_names(id).is_empty()))
        .then_some(id)
    }
}

struct Segment<'a> {
    text: &'a str,
    span: Span,
}

fn path_segments(file: &FileProduct, base: u32, start: u32, end: u32) -> Vec<Segment<'_>> {
    let tokens = &file.syntax.tree.tokens;
    let first = tokens.partition_point(|token| token.span.start < base + start);
    tokens[first..]
        .iter()
        .take_while(|token| token.span.start < base + end)
        .filter(|token| token.kind == TokenKind::Ident)
        .map(|token| Segment {
            text: &file.syntax.source[token.span.start as usize..token.span.end as usize],
            span: Span {
                start: token.span.start - base,
                end: token.span.end - base,
            },
        })
        .collect()
}

fn spelling_rank(written: &str, name: &str) -> Option<Rank> {
    if written == name {
        return None;
    }
    let written = written.to_lowercase();
    let name = name.to_lowercase();
    if written == name {
        return Some(Rank::Case);
    }
    let limit = (written.chars().count() / 3).max(1);
    if written.chars().count().abs_diff(name.chars().count()) > limit {
        return None;
    }
    let distance = damerau_distance(&written, &name);
    (distance <= limit).then_some(Rank::Spelling(distance))
}

fn damerau_distance(written: &str, name: &str) -> usize {
    // Restricted Damerau-Levenshtein: an adjacent transposition costs one edit.
    let written: Vec<_> = written.chars().collect();
    let mut previous: Vec<_> = (0..=written.len()).collect();
    let mut before_previous = previous.clone();
    let mut row = previous.clone();
    let mut previous_ch = None;
    for (i, ch) in name.chars().enumerate() {
        row[0] = i + 1;
        for (j, &other) in written.iter().enumerate() {
            let edit =
                (previous[j] + usize::from(ch != other)).min(1 + previous[j + 1].min(row[j]));
            row[j + 1] = if j > 0 && previous_ch == Some(other) && ch == written[j - 1] {
                edit.min(before_previous[j - 1] + 1)
            } else {
                edit
            };
        }
        previous_ch = Some(ch);
        std::mem::swap(&mut before_previous, &mut previous);
        std::mem::swap(&mut previous, &mut row);
    }
    previous[written.len()]
}
