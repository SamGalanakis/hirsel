//! A program's declared state as one record (OPTION-L v3.2). Declaration reads
//! headers alone (the crate's contract) and every table below is complete when
//! [`Program::declare_with_library`] returns: bodies check through `&Program`.
//! So the tables are a function of the request's shape (save PROG-4's test
//! reference rule, which reads bodies' names and runs again on install), and a record written
//! under one shape installs over a later parse with that shape: positional ids
//! (types, declarations, functions, constants, definitions) are equal by
//! construction. Only the handles into a parse are not, and the record holds
//! them free of it: a site as significant-token ordinals in its item, a node as
//! its child path from the item's root, a symbol as its text.
use crate::*;
use hirsel_core_syntax::{NodeKind, Symbol};
use serde::{Deserialize, Serialize};

/// An item-relative byte position: in or at the end of a header token (the
/// `ordinal`-th significant token of the item, `offset` bytes into it), or the
/// item's end. A body position other than the item's end has no such form.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
enum Pos {
    Token { ordinal: u32, offset: u32 },
    End,
}

/// The tables declaration writes, own rows only (a library's rows are the
/// request's image, named by the shape). Sites' spans index `positions`, nodes
/// index `nodes`, and symbols index `symbols`. `by_path` is sorted by path, so
/// one shape's tables are one record's bytes.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Declared {
    types: Vec<TyKind>,
    decls: Vec<TypeDecl>,
    fns: Vec<FnDecl>,
    constants: Vec<Constant>,
    constant_parts: Vec<ConstValue>,
    constant_strings: Vec<Vec<u8>>,
    constant_ids: Vec<Option<u32>>,
    by_path: Vec<(DefPath, Def)>,
    by_definition: Vec<Option<Def>>,
    core_operations: Vec<Option<u16>>,
    alias_state: Vec<u8>,
    positions: Vec<Pos>,
    nodes: Vec<Vec<u32>>,
    symbols: Vec<String>,
}

trait Bind {
    fn site(&mut self, site: &mut Site) -> Option<()>;
    fn node(&mut self, at: ItemRef, node: &mut NodeId) -> Option<()>;
    fn symbol(&mut self, symbol: &mut Symbol) -> Option<()>;
}

fn bind_fields(fields: &mut [FieldDecl], b: &mut impl Bind) -> Option<()> {
    fields.iter_mut().try_for_each(|f| b.site(&mut f.site))
}

/// Every handle of the record's rows, in one order for both directions.
fn bind_all(d: &mut Declared, b: &mut impl Bind) -> Option<()> {
    for decl in &mut d.decls {
        b.site(&mut decl.site)?;
        for g in &mut decl.generics {
            b.symbol(&mut g.name)?;
        }
        match &mut decl.body {
            DeclBody::Record(fields) => bind_fields(fields, b)?,
            DeclBody::Variant(cases) => {
                for c in cases {
                    bind_fields(&mut c.fields, b)?;
                }
            }
            DeclBody::Alias(_) => {}
        }
        if let Some(extra) = &mut decl.extra {
            if let Some(codec) = &mut extra.codec {
                if let Some(via) = &mut codec.via {
                    b.node(decl.at, &mut via.node)?;
                }
                for r in &mut codec.removed {
                    b.site(&mut r.site)?;
                    bind_fields(&mut r.fields, b)?;
                }
            }
        }
    }
    for f in &mut d.fns {
        b.site(&mut f.site)?;
        b.site(&mut f.result_site)?;
        for g in &mut f.generics {
            b.symbol(&mut g.name)?;
        }
        for p in &mut f.params {
            b.symbol(&mut p.name)?;
            b.site(&mut p.site)?;
        }
        if let Some(body) = &mut f.body {
            b.node(f.at, body)?;
        }
    }
    for c in &mut d.constants {
        b.node(c.at, &mut c.expr)?;
    }
    Some(())
}

/// An item's significant tokens (item-relative spans) and where its header
/// ends: the body block's start for a function or clone hook, else its end.
struct Layout {
    tokens: Vec<Span>,
    header_end: u32,
    end: u32,
}
impl Layout {
    /// The item's header tokens: no position names a token that starts past
    /// the header, so the body's tokens are not collected.
    fn of(item: Item<'_>) -> Self {
        let end = item.range.end - item.file_offset;
        let header_end = body_block(item).map_or(end, |n| item.tree.node(n).origin.span.start);
        let tokens = (item.range.tokens.start..item.range.tokens.end)
            .filter(|&t| !item.tree.tokens[t as usize].kind.is_trivia())
            .map(|t| item.token_span(hirsel_core_syntax::TokenId(t)))
            .take_while(|s| s.start <= header_end)
            .collect();
        Self {
            tokens,
            header_end,
            end,
        }
    }
    /// A span's `end` at a token boundary names the token it ends; a start
    /// names the token it starts.
    fn pos(&self, offset: u32, end: bool) -> Option<Pos> {
        let ending = self.tokens.partition_point(|s| s.end < offset);
        let starting = self.tokens.partition_point(|s| s.end <= offset);
        let order = if end {
            [ending, starting]
        } else {
            [starting, ending]
        };
        order
            .into_iter()
            .find_map(|k| {
                let s = self.tokens.get(k)?;
                (s.start <= offset && s.start <= self.header_end).then(|| Pos::Token {
                    ordinal: k as u32,
                    offset: offset - s.start,
                })
            })
            .or_else(|| (offset == self.end).then_some(Pos::End))
    }
    fn offset(&self, pos: Pos) -> Option<u32> {
        match pos {
            Pos::Token { ordinal, offset } => {
                let s = self.tokens.get(ordinal as usize)?;
                (offset <= s.end - s.start).then_some(s.start + offset)
            }
            Pos::End => Some(self.end),
        }
    }
}

/// The body block of a function or clone hook item: the root's block child.
fn body_block(item: Item<'_>) -> Option<NodeId> {
    if !matches!(
        item.tree.node(item.root).kind,
        NodeKind::Function | NodeKind::CloneHook
    ) {
        return None;
    }
    item.tree
        .children(item.root)
        .iter()
        .copied()
        .find(|&c| item.tree.node(c).kind == NodeKind::Block)
}

/// The child path from `item`'s root to `target`, not below a body block.
fn node_path(item: Item<'_>, target: NodeId) -> Option<Vec<u32>> {
    fn walk(item: Item<'_>, n: NodeId, target: NodeId, path: &mut Vec<u32>) -> bool {
        if n == target {
            return true;
        }
        for (i, &c) in item.tree.children(n).iter().enumerate() {
            path.push(i as u32);
            if walk(item, c, target, path) {
                return true;
            }
            path.pop();
        }
        false
    }
    let mut path = vec![];
    if !walk(item, item.root, target, &mut path) {
        return None;
    }
    let block = body_block(item);
    let below_block = path.len() > 1
        && block.is_some_and(|b| item.tree.children(item.root)[path[0] as usize] == b);
    (!below_block).then_some(path)
}

struct Free<'p, 'a> {
    p: &'p Program<'a>,
    layouts: HashMap<(usize, u32), Layout>,
    positions: Vec<Pos>,
    nodes: Vec<Vec<u32>>,
    symbols: Vec<String>,
    symbol_ids: HashMap<Symbol, u32>,
}
impl Free<'_, '_> {
    fn module(&self, id: ModuleId) -> Option<usize> {
        self.p.modules.iter().position(|m| m.id == id)
    }
}
impl Bind for Free<'_, '_> {
    fn site(&mut self, site: &mut Site) -> Option<()> {
        let m = self.module(site.module)?;
        let modules = self.p.modules;
        let layout = self
            .layouts
            .entry((m, site.item))
            .or_insert_with(|| Layout::of(modules[m].syntax.item(site.item as usize)));
        let (start, end) = (
            layout.pos(site.span.start, false)?,
            layout.pos(site.span.end, true)?,
        );
        let at = self.positions.len() as u32;
        self.positions.extend([start, end]);
        site.span = Span {
            start: at,
            end: at + 1,
        };
        Some(())
    }
    fn node(&mut self, at: ItemRef, node: &mut NodeId) -> Option<()> {
        let path = node_path(self.p.item(at), *node)?;
        *node = NodeId(self.nodes.len() as u32);
        self.nodes.push(path);
        Some(())
    }
    fn symbol(&mut self, symbol: &mut Symbol) -> Option<()> {
        let next = self.symbols.len() as u32;
        let id = *self.symbol_ids.entry(*symbol).or_insert(next);
        if id == next {
            self.symbols
                .push(self.p.index.symbols.text(*symbol).to_owned());
        }
        *symbol = Symbol(id);
        Some(())
    }
}

struct Bound<'r, 'p, 'a> {
    p: &'p Program<'a>,
    positions: &'r [Pos],
    nodes: &'r [Vec<u32>],
    symbols: &'r [String],
    layouts: HashMap<(usize, u32), Layout>,
}
impl Bind for Bound<'_, '_, '_> {
    fn site(&mut self, site: &mut Site) -> Option<()> {
        let m = self.p.modules.iter().position(|m| m.id == site.module)?;
        let syntax = &self.p.modules[m].syntax;
        if site.item as usize >= syntax.items.len() {
            return None;
        }
        let layout = self
            .layouts
            .entry((m, site.item))
            .or_insert_with(|| Layout::of(syntax.item(site.item as usize)));
        let pos = |i: u32| self.positions.get(i as usize).copied();
        site.span = Span {
            start: layout.offset(pos(site.span.start)?)?,
            end: layout.offset(pos(site.span.end)?)?,
        };
        Some(())
    }
    fn node(&mut self, at: ItemRef, node: &mut NodeId) -> Option<()> {
        let syntax = &self.p.modules.get(at.module as usize)?.syntax;
        if at.item as usize >= syntax.items.len() {
            return None;
        }
        let item = syntax.item(at.item as usize);
        let mut n = item.root;
        for &i in self.nodes.get(node.0 as usize)? {
            n = *item.tree.children(n).get(i as usize)?;
        }
        *node = n;
        Some(())
    }
    fn symbol(&mut self, symbol: &mut Symbol) -> Option<()> {
        let text = self.symbols.get(symbol.0 as usize)?;
        *symbol = self.p.index.symbols.find(text)?;
        Some(())
    }
}

impl<'a> Program<'a> {
    /// The declared tables, free of this parse; None when declaration refused,
    /// left a gap or a resource refusal, or a handle has no parse-free form.
    pub fn declared(&self) -> Option<Declared> {
        if !self.refusals.is_empty() || !self.gaps.is_empty() || !self.resource_refusals.is_empty()
        {
            return None;
        }
        let mut d = Declared {
            types: (self.types.own_start()..self.types.len())
                .map(|t| self.types.kind(TyId(t as u32)).clone())
                .collect(),
            decls: self.decls.own_iter().cloned().collect(),
            fns: self.fns.own_iter().cloned().collect(),
            constants: self.constants.own_iter().cloned().collect(),
            constant_parts: self.constant_parts.own_iter().cloned().collect(),
            constant_strings: self.constant_strings.own_iter().cloned().collect(),
            constant_ids: self.constant_ids.own_iter().cloned().collect(),
            by_path: {
                // The program's map has no order; the record's rows are sorted
                // by path, as a published image map's are.
                let mut rows: Vec<_> = self.own_paths().map(|(p, d)| (p.clone(), d)).collect();
                rows.sort_unstable_by(|a, b| a.0.cmp(&b.0));
                rows
            },
            by_definition: self.by_definition.own_iter().cloned().collect(),
            core_operations: self.core_operations.own_iter().cloned().collect(),
            alias_state: self.alias_state.clone(),
            positions: vec![],
            nodes: vec![],
            symbols: vec![],
        };
        let mut free = Free {
            p: self,
            layouts: HashMap::new(),
            positions: vec![],
            nodes: vec![],
            symbols: vec![],
            symbol_ids: HashMap::new(),
        };
        bind_all(&mut d, &mut free)?;
        d.positions = free.positions;
        d.nodes = free.nodes;
        d.symbols = free.symbols;
        Some(d)
    }

    /// The program `record` declares, over this request's parse and resolution,
    /// without running declaration. The record's shape holds every declaration
    /// input, so a sound record always binds; None means damage (a handle that
    /// does not bind, a row count unlike the index), and the caller declares,
    /// as for a record that fails its store check.
    pub fn install(
        index: &'a NameIndex,
        modules: &'a [ModuleInput],
        resolutions: Vec<Vec<std::sync::Arc<Resolution>>>,
        library: Option<&Library>,
        record: Declared,
    ) -> Option<Self> {
        let mut p = Self::open(index, modules, resolutions, library);
        let mut d = record;
        let (positions, nodes, symbols) = (
            std::mem::take(&mut d.positions),
            std::mem::take(&mut d.nodes),
            std::mem::take(&mut d.symbols),
        );
        let mut bound = Bound {
            p: &p,
            positions: &positions,
            nodes: &nodes,
            symbols: &symbols,
            layouts: HashMap::new(),
        };
        bind_all(&mut d, &mut bound)?;
        let start = p.types.own_start();
        for (i, kind) in d.types.into_iter().enumerate() {
            if p.types.intern(kind).0 as usize != start + i {
                return None;
            }
        }
        if d.by_definition.len() != index.definitions.len() - p.by_definition.base_len()
            || d.constant_ids.len() != index.definitions.len() - p.constant_ids.base_len()
        {
            return None;
        }
        p.by_definition.resize(p.by_definition.base_len(), None);
        p.constant_ids.resize(p.constant_ids.base_len(), None);
        d.decls.into_iter().for_each(|r| p.decls.push(r));
        d.fns.into_iter().for_each(|r| p.fns.push(r));
        d.constants.into_iter().for_each(|r| p.constants.push(r));
        d.constant_parts
            .into_iter()
            .for_each(|r| p.constant_parts.push(r));
        d.constant_strings
            .into_iter()
            .for_each(|r| p.constant_strings.push(r));
        d.constant_ids
            .into_iter()
            .for_each(|r| p.constant_ids.push(r));
        d.by_definition
            .into_iter()
            .for_each(|r| p.by_definition.push(r));
        d.core_operations
            .into_iter()
            .for_each(|r| p.core_operations.push(r));
        for (_, def) in d.by_path {
            p.add_path(def);
        }
        p.alias_state = d.alias_state;
        // PROG-4 runs again: its reference rule reads every item's resolved
        // names, bodies included, so it is not a function of the shape. It also
        // sets has_test_functions, which the record therefore does not hold.
        test_functions::check(&mut p);
        Some(p)
    }

    /// The program's tables as text, one row per entry, for the layer law's
    /// table-by-table comparison of an installed program with a declared one.
    pub fn tables(&self) -> Vec<(&'static str, Vec<String>)> {
        fn rows<T: std::fmt::Debug>(rows: impl Iterator<Item = T>) -> Vec<String> {
            rows.map(|r| format!("{r:?}")).collect()
        }
        let mut by_path = rows(self.own_paths());
        by_path.sort();
        vec![
            (
                "types",
                rows((0..self.types.len()).map(|t| self.types.kind(TyId(t as u32)))),
            ),
            (
                "decls",
                rows(self.decls.iter().map(|d| TypeDecl {
                    summary: Summary::default(),
                    ..d.clone()
                })),
            ),
            (
                "summaries",
                rows(self.decls.iter().map(|d| Summary {
                    recipe: None,
                    ..d.summary.clone()
                })),
            ),
            (
                "recipes",
                rows(self.decls.iter().map(|d| &d.summary.recipe)),
            ),
            ("fns", rows(self.fns.iter())),
            ("constants", rows(self.constants.iter())),
            ("constant_parts", rows(self.constant_parts.iter())),
            ("constant_strings", rows(self.constant_strings.iter())),
            ("constant_ids", rows(self.constant_ids.iter())),
            ("by_path", by_path),
            ("by_definition", rows(self.by_definition.iter())),
            ("core_operations", rows(self.core_operations.iter())),
            (
                "flags",
                vec![format!(
                    "tests {} aliases {:?} library modules {}",
                    self.has_test_functions, self.alias_state, self.library_modules
                )],
            ),
            (
                "library",
                vec![
                    self.types.own_start().to_string(),
                    self.decls.base_len().to_string(),
                    self.fns.base_len().to_string(),
                    self.constants.base_len().to_string(),
                    self.by_definition.base_len().to_string(),
                ],
            ),
            ("refusals", rows(self.refusals.iter())),
            ("gaps", rows(self.gaps.iter())),
            ("resource_refusals", rows(self.resource_refusals.iter())),
        ]
    }
}
