//! Definition keys keep implementation edits separate from signature dependencies.
use super::*;
use hirsel_core_resolve::{
    DefinitionId, DefinitionKind, LookupResult, LookupScope, Namespace, Target,
};
use hirsel_core_syntax::Symbol;
use hirsel_core_syntax::{Item, NodeKind, TokenKind};
use std::ops::Range;

/// A body record is reused exactly when its key is equal, so the key holds every
/// program input the body's checking and lowering read. The rule: lowering reads
/// the program only through
/// - its own item (`body`);
/// - the names it resolves, their header closures, the core prelude's and those of
///   std's structural codec makers (`signatures`), and its lookups outside the item
///   (`lookups`);
/// - the compiler, core and package namespaces (`environment`);
/// - and one field per form whose lowering reads beyond its resolved names,
///   present only for bodies that name that form: `siblings`.
///
/// A new read of a table over all functions, types, instances or modules, or of
/// an item found by a path lowering builds itself, gets a field here (or a
/// `hirsel_core_front::SIBLING_READS` row) in the same change. This is a list,
/// not a recorded per-body read set, because the key is computed before the body
/// is lowered: a recorded set would have to be stored with each record and its
/// inputs re-hashed on every lookup (a second key phase on every hit), and some
/// forms read beyond their names (see the reuse-history-fault-1031 audit).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DefinitionKey {
    pub body: ContentHash,
    /// The transitive signature closure as one Merkle hash over the strongly
    /// connected components of header reads (see `Keys::closure`).
    pub signatures: ContentHash,
    pub lookups: ContentHash,
    pub environment: ContentHash,
    /// The closures of the items lowering calls by a path it builds beside a
    /// callee the body names (`hirsel_core_front::SIBLING_READS`): `list.grow`
    /// beside `list.push`, std's built-in codec makers beside `codec.of`.
    pub siblings: Option<ContentHash>,
}

/// A body's identity is its item's source bytes from its first significant
/// token to the end of its last one. The body's item-relative anchors (its
/// refusal and flow sites, its IR instructions' `Anchor`s) are byte offsets
/// from the first token, so the bytes between them are part of what its record
/// holds: whitespace inside the item moves them. Trivia before the first token
/// and after the last (the owned line break, a trailing comment) is read only
/// by the formatter and the syntax diagnostics, which come from the parse.
fn body_identity(item: Item<'_>) -> ContentHash {
    let start = item.file_offset;
    let end = item
        .tree
        .tokens
        .get(item.range.tokens.start as usize..item.range.tokens.end as usize)
        .into_iter()
        .flatten()
        .rev()
        .find(|t| !t.kind.is_trivia() && !matches!(t.kind, TokenKind::Eof) && t.span.start >= start)
        .map_or(start, |t| t.span.end);
    ContentHash::of_parts(
        "hirsel.item-source.v3",
        [item.text[start as usize..end as usize].as_bytes()],
    )
}

/// The tokens before the function body, or the entire range for other items.
fn header_tokens(item: Item<'_>, end: usize) -> Range<u32> {
    let mut tokens = item.range.tokens.clone();
    if matches!(
        item.tree.node(item.root).kind,
        NodeKind::Function | NodeKind::CloneHook
    ) {
        let end = item.file_offset as usize + end;
        tokens.end = tokens.start
            + item.tree.tokens[tokens.start as usize..tokens.end as usize]
                .partition_point(|token| (token.span.start as usize) < end) as u32;
    }
    tokens
}

/// The item's full header, including bindings read by its own body. A
/// syntax-refused item gets its own domain so it cannot select a clean
/// request's declared tables.
fn header_identity(item: Item<'_>, tokens: Range<u32>) -> ContentHash {
    let domain = if item.range.has_errors {
        "hirsel.header-tokens.syntax-refused.v1"
    } else {
        "hirsel.header-tokens.v2"
    };
    if let Some(canonical) = item.tree.canonical_slice(tokens.clone(), usize::MAX) {
        return ContentHash::of_parts(domain, [canonical]);
    }
    // A deserialized syntax tree has no runtime canonical stream.
    ContentHash::of_parts_stream(domain, |parts| {
        for token in &item.tree.tokens[tokens.start as usize..tokens.end as usize] {
            if token.kind.is_trivia() || matches!(token.kind, TokenKind::Eof) {
                continue;
            }
            let text = &item.text[token.span.start as usize..token.span.end as usize];
            parts.part_byte_slice(token.kind as u8, text.as_bytes());
        }
        Ok(())
    })
    .expect("semantic token hash")
}

/// A signature's identity as its readers depend on it, the header row's
/// `digest`. A reader's reported sites are all in its own item
/// (`DiskCheckRef::of`), so the header's layout is not part of it. Nor is the
/// spelling of a parameter's binding: a call is positional (labels are refused
/// on ordinary calls, LANG-9), a reader's refusals render its own parameters,
/// and anchors and region roots name a callee's parameter by index. So when
/// every binding name is distinct and no other header token spells one (an
/// anchor, a default or an outlives clause naming a parameter keeps its
/// spelling), the bindings hash as their token kind alone; their count, order,
/// modes, types and `waits` stay. Any other header is `shape`, its full tokens.
fn signature_identity(
    item: Item<'_>,
    tokens: Range<u32>,
    shape: ContentHash,
    bindings: &mut Vec<(Symbol, usize)>,
) -> ContentHash {
    let root = item.tree.node(item.root);
    if item.range.has_errors || !matches!(root.kind, NodeKind::Function | NodeKind::CloneHook) {
        return shape;
    }
    let tree = item.tree;
    let Some(&parameters) = tree
        .children(item.root)
        .iter()
        .find(|&&c| tree.node(c).kind == NodeKind::Parameters)
    else {
        return shape;
    };
    bindings.clear();
    bindings.extend(
        tree.children(parameters)
            .iter()
            .filter_map(|&p| tree.node(p).token.map(|t| t.0 as usize))
            .map(|i| (tree.tokens[i].symbol, i)),
    );
    if bindings.is_empty() {
        return shape;
    }
    bindings.sort_unstable_by_key(|&(name, _)| name);
    if bindings.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        return shape;
    }
    let (start, stop) = (tokens.start as usize, tokens.end as usize);
    // Parameter names are identifiers, interned by spelling in this token stream.
    for (i, token) in (start..stop).zip(&tree.tokens[start..stop]) {
        if token.kind == TokenKind::Ident
            && let Ok(binding) = bindings.binary_search_by_key(&token.symbol, |&(name, _)| name)
            && bindings[binding].1 != i
        {
            return shape;
        }
    }
    bindings.sort_unstable_by_key(|&(_, i)| i);
    // The parsed tree's canonical stream frames each significant token as
    // `part_byte_slice(kind, spelling)` does (trivia adds no bytes), so the
    // header hashes as its stream with each binding's spelling left out.
    let canonical = |from: usize, to: usize| {
        let offsets = &tree.canonical_offsets;
        &tree.canonical[offsets[from] as usize..offsets[to] as usize]
    };
    ContentHash::of_parts_stream("hirsel.signature-tokens.v1", |parts| {
        // The frame holds a one-byte token kind and no spelling.
        const BINDING: [u8; 9] = [1, 0, 0, 0, 0, 0, 0, 0, TokenKind::Ident as u8];
        let mut from = start;
        for &(_, binding) in bindings.iter() {
            parts.framed_parts(canonical(from, binding));
            parts.framed_parts(&BINDING);
            from = binding + 1;
        }
        parts.framed_parts(canonical(from, stop));
        Ok(())
    })
    .expect("signature token hash")
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Header {
    /// The signature as readers depend on it (`signature_identity`).
    digest: ContentHash,
    /// The item's source bytes from its first token to its last
    /// (`body_identity`), the body key's `body`.
    source: ContentHash,
    // Publication preserves definition IDs, and requests append to that index.
    // Signature dependencies are already resolved by the producing request.
    pub(crate) reads: Vec<DefinitionId>,
    pub(crate) body_reads: Vec<DefinitionId>,
    pub(crate) has_body: bool,
    pub(crate) header_end: u32,
    pub(crate) site: Site,
    /// The item's module, its file and its ordinal in that file.
    pub(crate) anchor: (ModuleId, SourceId, u32),
}
/// Numeric signature rows are separate from the path index. Already resolved
/// dependencies demand a row directly, without re-encoding its canonical path.
#[derive(Clone, Default, Serialize, Deserialize)]
pub(crate) struct Headers {
    pub(crate) rows: hirsel_core_ids::published::Table<Option<Header>>,
    names: hirsel_core_ids::published::Map<DefPath, DefinitionId>,
    /// An image's own signature closure per definition, sealed when the image
    /// is published (`seal`). Empty in a request's own headers.
    closures: hirsel_core_ids::published::Table<Option<ContentHash>>,
    /// The hash over the core prelude types' closures, sealed with an image
    /// that holds core (`Keys::prelude_closure`).
    prelude: Option<ContentHash>,
}
impl Headers {
    pub(crate) fn get(&self, path: &DefPath) -> Option<&Header> {
        self.by_id(*self.names.get(path)?)
    }
    pub(crate) fn contains_key(&self, path: &DefPath) -> bool {
        self.get(path).is_some()
    }
    pub(crate) fn id(&self, path: &DefPath) -> Option<DefinitionId> {
        self.names.get(path).copied()
    }
    /// Whether `id(path)` is `id`, for a row recorded against this image
    /// (the reach record's key names it). The image fixes its own names, and a
    /// request names only its own definitions, which follow the image's
    /// (`base`), so an image id is still the path's unless the request now
    /// names the path itself. This skips the probe of the image's name map.
    pub(crate) fn names_recorded(&self, base: usize, id: DefinitionId, path: &DefPath) -> bool {
        if (id.0 as usize) < base {
            return self.names.own_get(path).is_none();
        }
        self.id(path) == Some(id)
    }
    pub(crate) fn row(&self, id: DefinitionId) -> Option<&Header> {
        self.by_id(id)
    }
    fn by_id(&self, id: DefinitionId) -> Option<&Header> {
        self.rows.get(id.0 as usize)?.as_ref()
    }
    pub(crate) fn publish(
        &self,
        writer: &mut impl hirsel_core_ids::published::Publisher,
        outward: &[bool],
    ) -> io::Result<Self> {
        Ok(Self {
            rows: writer.table(&self.rows)?,
            names: writer.map(&self.names)?,
            closures: writer.outward_table(&self.closures, outward)?,
            prelude: self.prelude,
        })
    }
    /// The image's header rows with every definition's closure hash. Requests
    /// append their definitions after the image's prefix and take these
    /// closures as given.
    pub(crate) fn seal(&self, index: &NameIndex) -> io::Result<Self> {
        let mut keys = Keys::bare(index, self.clone());
        keys.prelude = prelude(index);
        let mut closures = Vec::with_capacity(self.rows.len());
        for (id, row) in self.rows.iter().enumerate() {
            closures.push(match row {
                Some(_) => Some(keys.closure(index, id)?),
                None => None,
            });
        }
        let mut sealed = self.clone();
        sealed.prelude = match keys.prelude.is_empty() {
            true => None,
            false => Some(keys.prelude_closure(index)?),
        };
        sealed.closures = closures.into();
        Ok(sealed)
    }
}
// Request-owned IDs get direct slots; the installed prefix is sparse and is
// allocated only as a request names it. Each semantic lookup is performed once.
struct Demand<T> {
    base: usize,
    own: Vec<std::sync::OnceLock<T>>,
    prefix: hirsel_core_ids::published::Sparse<T>,
}
impl<T> Demand<T> {
    fn new(base: usize, len: usize) -> Self {
        Self {
            base,
            own: (base..len).map(|_| std::sync::OnceLock::new()).collect(),
            prefix: Default::default(),
        }
    }
    fn get_or_try_init(&self, id: u64, f: impl FnOnce() -> io::Result<T>) -> io::Result<&T> {
        let id = id as usize;
        if id < self.base {
            return self.prefix.get_or_try_init(id as u64, f);
        }
        let cell = &self.own[id - self.base];
        if let Some(value) = cell.get() {
            return Ok(value);
        }
        let value = f()?;
        let _ = cell.set(value);
        Ok(cell.get().unwrap())
    }
}
/// The core prelude types' definitions (`hirsel_core_front::PRELUDE`), when
/// the index has core.
fn prelude(index: &NameIndex) -> Vec<usize> {
    let Some(core) = index
        .symbols
        .find("core")
        .and_then(|c| index.core_module(c))
    else {
        return vec![];
    };
    hirsel_core_front::PRELUDE
        .iter()
        .filter_map(|name| index.definition(core, index.symbols.find(name)?))
        .filter(|&d| index.get(d).kind == DefinitionKind::Type)
        .map(|d| d.0 as usize)
        .collect()
}
pub(crate) struct Keys {
    pub(crate) headers: Headers,
    environment: ContentHash,
    // One identity digest per definition, module and package a request's keys
    // name; see `definition_identity`, `module_identity`, `package_identity`.
    definitions: Demand<ContentHash>,
    modules: Demand<ContentHash>,
    packages: Demand<ContentHash>,
    anchors: hirsel_core_ids::published::Map<crate::ItemAt, DefPath>,
    // One closure hash per definition, memoised across the bodies of a request.
    closures: std::cell::RefCell<rustc_hash::FxHashMap<u32, ContentHash>>,
    // `closure`'s traversal state, emptied by each traversal and kept for the next.
    walk: std::cell::RefCell<Walk>,
    signature_reads: std::cell::RefCell<SignatureReads>,
    // Contextual constructor candidates in index order, per (module, name).
    constructors: std::cell::RefCell<rustc_hash::FxHashMap<(u32, Symbol), Arc<[DefinitionId]>>>,
    // The core prelude types every body's signatures read
    // (`hirsel_core_front::PRELUDE`), and their closures' hash once per request.
    prelude: Vec<usize>,
    prelude_closure: std::cell::OnceCell<ContentHash>,
    // The prelude's closures with std's structural codec makers', once per
    // request (`Keys::prelude_reads`).
    prelude_reads: std::cell::OnceCell<ContentHash>,
    /// The forms whose lowering reads a program input beyond their names
    /// (see `DefinitionKey`), by definition, as the index holds them.
    forms: Vec<(DefinitionId, usize)>,
    siblings: Vec<Siblings>,
    /// `forms`' definitions as a bit set, tested for every name of every body.
    form_bits: Vec<u64>,
}
/// `Keys::closure`'s traversal: visit order and low link per node, the node
/// stack, the frames, and the component being hashed.
#[derive(Default)]
struct Walk {
    order: rustc_hash::FxHashMap<usize, (u32, u32)>,
    stack: Vec<usize>,
    frames: Vec<(usize, usize)>,
    rows: Vec<(ContentHash, ContentHash)>,
    successors: Vec<ContentHash>,
}
/// Marks definition IDs for the body whose signature reads are being collected.
#[derive(Default)]
struct SignatureReads {
    stamps: Vec<u8>,
    epoch: u8,
}

impl SignatureReads {
    fn begin(&mut self, definitions: usize) -> u8 {
        if self.stamps.len() < definitions {
            self.stamps.resize(definitions, 0);
        }
        self.epoch = self.epoch.wrapping_add(1);
        if self.epoch == 0 {
            self.stamps.fill(0);
            self.epoch = 1;
        }
        self.epoch
    }
}

/// What a form's lowering reads beyond its names.
/// One `hirsel_core_front::SIBLING_READS` callee: its row, its package's root
/// module, and the hash of the read closures once a body names the callee.
struct Siblings {
    row: usize,
    root: ModuleId,
    hash: std::cell::OnceCell<ContentHash>,
}
/// Each core operation whose lowering resolves sibling definitions.
fn forms(index: &NameIndex) -> (Vec<(DefinitionId, usize)>, Vec<Siblings>) {
    let (mut forms, mut siblings) = (vec![], vec![]);
    for (row, &(package, module, name, _)) in hirsel_core_front::SIBLING_READS.iter().enumerate() {
        let (Some(package), Some(module), Some(name)) = (
            index.symbols.find(package),
            index.symbols.find(module),
            index.symbols.find(name),
        ) else {
            continue;
        };
        for p in index.packages.iter().filter(|p| p.name == package) {
            if let Some(id) = index
                .child(p.root, module)
                .and_then(|m| index.definition(m, name))
            {
                forms.push((id, siblings.len()));
                siblings.push(Siblings {
                    row,
                    root: p.root,
                    hash: Default::default(),
                });
            }
        }
    }
    (forms, siblings)
}

fn codec_reads(index: &NameIndex) -> impl Iterator<Item = Option<DefinitionId>> + '_ {
    let std = index.symbols.find("std");
    let codec = index.symbols.find("codec");
    index
        .packages
        .iter()
        .filter(move |p| Some(p.name) == std && codec.is_some())
        .flat_map(move |p| {
            let module = codec.and_then(|name| index.child(p.root, name));
            hirsel_core_front::STRUCTURAL_CODECS
                .iter()
                .map(|(_, maker)| *maker)
                .chain([hirsel_core_front::BYTES_CODEC])
                .chain(hirsel_core_front::TUPLE_CODEC_READS)
                .map(move |maker| index.definition(module?, index.symbols.find(maker)?))
        })
}

impl Siblings {
    fn inputs<'a>(
        &'a self,
        index: &'a NameIndex,
    ) -> impl Iterator<Item = Option<DefinitionId>> + 'a {
        hirsel_core_front::SIBLING_READS[self.row]
            .3
            .paths()
            .map(|(module, name)| {
                let module = index.child(self.root, index.symbols.find(module)?)?;
                index.definition(module, index.symbols.find(name)?)
            })
    }
}

pub(crate) fn environment(
    inputs: &CorpusManifest,
    compiler: ContentHash,
) -> io::Result<ContentHash> {
    let no_foreign = ContentHash::of_parts("hirsel.foreign-stamp.v1", [&0u64.to_le_bytes()[..]]);
    let mut namespaces: Vec<_> = inputs
        .packages
        .iter()
        .map(|package| {
            let namespace = package.namespace.as_ref();
            (
                &package.package,
                &package.dependencies,
                namespace.and_then(|namespace| namespace.root),
                namespace.map_or(no_foreign, |namespace| namespace.foreign),
            )
        })
        .collect();
    namespaces.sort_by_key(|p| p.0);
    Ok(ContentHash::of_parts(
        "hirsel.core-environment.v1",
        [&encode(&(compiler, &inputs.core, namespaces))?[..]],
    ))
}

/// Signature inputs for lowering that have no explicit source reference.
pub(crate) fn implicit_reads(
    index: &NameIndex,
) -> (Vec<DefinitionId>, BTreeMap<DefinitionId, Vec<DefinitionId>>) {
    let prelude = prelude(index)
        .into_iter()
        .map(|id| DefinitionId(id as u32))
        .chain(codec_reads(index).flatten())
        .collect();
    let (forms, siblings) = forms(index);
    let mut by_form = BTreeMap::<_, Vec<_>>::new();
    for (form, row) in forms {
        by_form
            .entry(form)
            .or_default()
            .extend(siblings[row].inputs(index).flatten());
    }
    (prelude, by_form)
}
/// The gate's inputs (OPTION-L L2): the environment, and every module's
/// identity and item header digests in order. Both read the manifests and the
/// parse, and a module's path from the index, never a resolution, so the gate
/// decides after the index and before resolve.
pub(crate) struct Shape {
    environment: ContentHash,
    items: ContentHash,
    modules: Demand<ContentHash>,
    /// Each item's header end and signature digest.
    headers: Vec<Vec<(usize, ContentHash)>>,
}
impl Shape {
    pub(crate) fn new(
        index: &NameIndex,
        modules: &[ModuleInput],
        environment: ContentHash,
    ) -> io::Result<Self> {
        let identities = Demand::new(index.modules.base_len(), index.modules.len());
        let mut headers = Vec::with_capacity(modules.len());
        let mut bindings = Vec::new();
        let items = ContentHash::of_parts_stream("hirsel.key-items.v1", |parts| {
            for m in modules {
                let module = identities.get_or_try_init(m.id.0 as u64, || {
                    Ok(ContentHash::of_parts(
                        "hirsel.key-identity.module.v1",
                        [&encode(&index.module_path(m.id))?[..]],
                    ))
                })?;
                parts.part_byte_slice(0, &module.0);
                let mut row = Vec::with_capacity(m.syntax.items.len());
                for item in m.syntax.items() {
                    let end = header_end(item);
                    let tokens = header_tokens(item, end);
                    let digest = header_identity(item, tokens.clone());
                    parts.part_byte_slice(1, &digest.0);
                    row.push((end, signature_identity(item, tokens, digest, &mut bindings)));
                }
                headers.push(row);
            }
            Ok(())
        })?;
        Ok(Self {
            environment,
            items,
            modules: identities,
            headers,
        })
    }
    /// The key shape: the environment, every module's item headers in order,
    /// and `request` (entry, image identity, library mode and parse modes).
    /// Two requests with one shape resolve every unchanged item alike and
    /// derive every closure alike, so an unchanged body keeps its key: its
    /// candidate sets, negative, ambiguous and shadowed probes, prelude, and
    /// generic, glue, clone-hook and drop edges all read headers, manifests or
    /// the image, never another item's body.
    pub(crate) fn key(&self, request: &[u8]) -> ContentHash {
        ContentHash::of_parts(
            "hirsel.key-shape.v1",
            [&self.environment.0[..], &self.items.0[..], request],
        )
    }
}
impl Keys {
    pub fn new(
        index: &NameIndex,
        modules: &[ModuleInput],
        resolutions: &[Vec<Arc<Resolution>>],
        shape: Shape,
        image: Option<ContentHash>,
        image_headers: Option<&Headers>,
        image_anchors: Option<&hirsel_core_ids::published::Map<crate::ItemAt, DefPath>>,
    ) -> io::Result<Self> {
        let mut headers = image_headers.cloned().unwrap_or_default();
        headers.rows.resize(index.definitions.len(), None);
        let mut keys = Self::with_modules(index, Headers::default(), shape.modules);
        let mut anchors = image_anchors.cloned().unwrap_or_default();
        let mut hooks = vec![];
        let mut reads = Vec::new();
        let mut body_reads = Vec::new();
        for ((m, rows), digests) in modules.iter().zip(resolutions).zip(shape.headers) {
            for (ordinal, ((item, resolution), (end, digest))) in
                m.syntax.items().zip(rows).zip(digests).enumerate()
            {
                let Some(definition) = index.item_definition(m.id, m.source, ordinal as u32, item)
                else {
                    let root = item.tree.node(item.root);
                    if root.token.is_none()
                        || item.range.has_errors
                        || !matches!(
                            root.kind,
                            NodeKind::Function
                                | NodeKind::Constant
                                | NodeKind::TypeDecl
                                | NodeKind::ResourceType
                                | NodeKind::ScopedType
                                | NodeKind::CloneHook
                        )
                        || index
                            .diagnostics
                            .iter()
                            .any(|r| crate::item_at(&r.primary) == Some((m.source, ordinal as u32)))
                    {
                        continue;
                    }
                    return Err(io::Error::other(format!(
                        "internal error: core reuse header has no indexed definition in {:?}, item {ordinal}",
                        index.module_path(m.id)
                    )));
                };
                let path = index.definition_path(definition);
                reads.clear();
                body_reads.clear();
                for name in &resolution.names {
                    let into = if (item.tree.node(name.node).origin.span.start as usize) < end {
                        &mut reads
                    } else {
                        &mut body_reads
                    };
                    match name.target {
                        Target::Definition(id) => {
                            into.push(id);
                        }
                        Target::ContextualConstructor { name } => {
                            into.extend(keys.constructors(index, m.id, name).iter().copied());
                        }
                        _ => {}
                    }
                }
                reads.sort_unstable();
                reads.dedup();
                body_reads.sort_unstable();
                body_reads.dedup();
                if index.get(definition).kind == DefinitionKind::Clone {
                    hooks.push((definition, reads.clone()));
                }
                anchors.insert((m.source, ordinal as u32), path.clone());
                headers.names.insert(path, definition);
                headers.rows[definition.0 as usize] = Some(Header {
                    digest,
                    source: body_identity(item),
                    has_body: end < item.source_text().len(),
                    header_end: end as u32,
                    site: index.get(definition).site,
                    body_reads: body_reads.clone(),
                    reads: reads.clone(),
                    anchor: (m.id, m.source, ordinal as u32),
                });
            }
        }
        // A clone hook is a fact about the type it clones, attached from its own
        // item (MEM-17): the type's row reads the hook, so the type's closure covers
        // the hook's presence, path and header, and a hook moved, added or removed
        // changes every closure that reaches the type.
        for (hook, reads) in hooks {
            for ty in reads {
                if index.get(ty).kind != DefinitionKind::Type {
                    continue;
                }
                if let Some(row) = headers.rows[ty.0 as usize].as_mut()
                    && let Err(at) = row.reads.binary_search(&hook)
                {
                    row.reads.insert(at, hook);
                }
            }
        }
        // An image that holds core sealed the prelude's hash; a request can
        // change no type in it.
        match headers.prelude {
            Some(hash) => drop(keys.prelude_closure.set(hash)),
            None => keys.prelude = prelude(index),
        }
        keys.headers = headers;
        keys.anchors = anchors;
        keys.environment = shape.environment;
        (keys.forms, keys.siblings) = forms(index);
        if let Some(last) = keys.forms.iter().map(|f| f.0.0 as usize).max() {
            keys.form_bits = vec![0; last / 64 + 1];
            for &(id, _) in &keys.forms {
                keys.form_bits[id.0 as usize >> 6] |= 1 << (id.0 & 63);
            }
        }
        Ok(keys)
    }
    fn bare(index: &NameIndex, headers: Headers) -> Self {
        Self::with_modules(
            index,
            headers,
            Demand::new(index.modules.base_len(), index.modules.len()),
        )
    }
    fn with_modules(index: &NameIndex, headers: Headers, modules: Demand<ContentHash>) -> Self {
        Self {
            headers,
            environment: ContentHash::default(),
            anchors: Default::default(),
            definitions: Demand::new(index.definitions.base_len(), index.definitions.len()),
            modules,
            packages: Demand::new(index.packages.base_len(), index.packages.len()),
            closures: Default::default(),
            walk: Default::default(),
            signature_reads: Default::default(),
            constructors: Default::default(),
            prelude: vec![],
            prelude_closure: Default::default(),
            prelude_reads: Default::default(),
            forms: vec![],
            form_bits: vec![],
            siblings: vec![],
        }
    }

    fn dependency(&self, index: &NameIndex, id: usize) -> io::Result<&Header> {
        hirsel_core_front::cost_scope!("key query", "dependency lookup");
        let id = hirsel_core_resolve::DefinitionId(id as u32);
        self.headers.by_id(id).ok_or_else(|| {
            io::Error::other(format!(
                "core signature demand outside reached definitions: {}",
                index.definition_path(id)
            ))
        })
    }
    /// A definition's identity: its canonical path, under its own domain so
    /// no module or package identity can share an input with it.
    fn definition_identity(&self, index: &NameIndex, id: DefinitionId) -> io::Result<ContentHash> {
        self.definitions
            .get_or_try_init(id.0 as u64, || {
                Ok(ContentHash::of_parts(
                    "hirsel.key-identity.definition.v1",
                    [&encode(&index.definition_path_ref(id))?[..]],
                ))
            })
            .copied()
    }
    fn module_identity(&self, index: &NameIndex, id: ModuleId) -> io::Result<ContentHash> {
        self.modules
            .get_or_try_init(id.0 as u64, || {
                Ok(ContentHash::of_parts(
                    "hirsel.key-identity.module.v1",
                    [&encode(&index.module_path(id))?[..]],
                ))
            })
            .copied()
    }
    fn package_identity(
        &self,
        index: &NameIndex,
        id: hirsel_core_resolve::PackageKey,
    ) -> io::Result<ContentHash> {
        self.packages
            .get_or_try_init(id.0 as u64, || {
                let p = &index.packages[id.0 as usize];
                let package = PackageId {
                    name: index.symbols.text(p.name).into(),
                    version: index.symbols.text(p.version).into(),
                };
                Ok(ContentHash::of_parts(
                    "hirsel.key-identity.package.v1",
                    [&encode(&package)?[..]],
                ))
            })
            .copied()
    }
    /// A module's contextual constructor candidates for `name`, in index order.
    fn constructors(
        &self,
        index: &NameIndex,
        module: ModuleId,
        name: Symbol,
    ) -> Arc<[DefinitionId]> {
        self.constructors
            .borrow_mut()
            .entry((module.0, name))
            .or_insert_with(|| index.constructor_candidates(module, name).collect())
            .clone()
    }
    pub fn capture_anchors(
        &self,
        check: &CoreCheck,
        incomplete: &[Incomplete],
    ) -> BTreeMap<crate::ItemAt, DefPath> {
        hirsel_core_front::cost_scope!("phase", "driver keys/anchors");
        let mut anchors = BTreeMap::new();
        let mut capture = |site: &Site| {
            let Some(at) = crate::item_at(site) else {
                return;
            };
            if let Some(path) = self.anchors.get(&at) {
                anchors.insert(at, path.clone());
            }
        };
        for r in &check.refusals {
            capture(&r.primary);
            for cause in &r.causal {
                capture(&cause.site);
            }
        }
        for g in incomplete {
            capture(&g.primary);
            for site in &g.causal {
                capture(site);
            }
        }
        for (site, _) in &check.flow.stages {
            capture(site);
        }
        anchors
    }
    pub fn bind_site(&self, anchors: &BTreeMap<crate::ItemAt, DefPath>, site: &mut Site) {
        if let Some(path) = crate::item_at(site).and_then(|at| anchors.get(&at))
            && let Some(h) = self.headers.get(path)
        {
            crate::bind(site, h.anchor);
        }
    }
    /// A closure already known: hashed earlier in this request, or sealed in
    /// the image this request's prefix comes from.
    fn known(&self, id: usize) -> Option<ContentHash> {
        if let Some(&hash) = self.closures.borrow().get(&(id as u32)) {
            return Some(hash);
        }
        self.headers.closures.get(id).copied().flatten()
    }
    /// The closure hash of `start`: a Merkle hash over the condensation of the
    /// header-read graph. Each strongly connected component hashes its members'
    /// (definition identity, header digest) rows and its successor components'
    /// hashes, so a changed digest or a changed edge out of a component changes
    /// the hash of every component that reaches it. Iterative Tarjan; every
    /// component is hashed once per request, whatever the number of bodies that
    /// reach it, and an image's components were hashed when it was sealed.
    fn closure(&self, index: &NameIndex, start: usize) -> io::Result<ContentHash> {
        if let Some(hash) = self.known(start) {
            return Ok(hash);
        }
        let mut walk = self.walk.borrow_mut();
        let Walk {
            order,
            stack,
            frames,
            rows,
            successors,
        } = &mut *walk;
        order.clear();
        stack.clear();
        frames.clear();
        // A visited node not yet known is on the stack: a finished component's
        // members are known from the moment it is hashed.
        let enter = |v: usize,
                     order: &mut rustc_hash::FxHashMap<usize, (u32, u32)>,
                     stack: &mut Vec<usize>,
                     frames: &mut Vec<(usize, usize)>| {
            let n = order.len() as u32;
            order.insert(v, (n, n));
            stack.push(v);
            frames.push((v, 0));
        };
        enter(start, order, stack, frames);
        while let Some(&(v, child)) = frames.last() {
            let reads = &self.dependency(index, v)?.reads;
            if let Some(w) = reads.get(child).map(|id| id.0 as usize) {
                frames.last_mut().unwrap().1 += 1;
                if self.known(w).is_some() {
                    continue;
                }
                match order.get(&w) {
                    None => enter(w, order, stack, frames),
                    Some(&(w_index, _)) => {
                        let low = &mut order.get_mut(&v).unwrap().1;
                        *low = (*low).min(w_index);
                    }
                }
                continue;
            }
            frames.pop();
            let (v_index, v_low) = order[&v];
            if let Some(&(parent, _)) = frames.last() {
                let low = &mut order.get_mut(&parent).unwrap().1;
                *low = (*low).min(v_low);
            }
            if v_low != v_index {
                continue;
            }
            let at = stack.iter().rposition(|&m| m == v).unwrap();
            rows.clear();
            successors.clear();
            for &m in &stack[at..] {
                let header = self.dependency(index, m)?;
                rows.push((
                    self.definition_identity(index, DefinitionId(m as u32))?,
                    header.digest,
                ));
                successors.extend(header.reads.iter().filter_map(|w| self.known(w.0 as usize)));
            }
            rows.sort_unstable();
            successors.sort_unstable();
            successors.dedup();
            let hash = ContentHash::of_parts_stream("hirsel.signature-component.v2", |parts| {
                parts.part_byte_slice(0, &(rows.len() as u64).to_le_bytes());
                for (identity, digest) in rows.iter() {
                    let mut row = [0u8; 64];
                    row[..32].copy_from_slice(&identity.0);
                    row[32..].copy_from_slice(&digest.0);
                    parts.part_byte_slice(1, &row);
                }
                for successor in successors.iter() {
                    parts.part_byte_slice(2, &successor.0);
                }
                Ok(())
            })?;
            let mut closures = self.closures.borrow_mut();
            for m in stack.drain(at..) {
                closures.insert(m as u32, hash);
            }
        }
        Ok(self.closures.borrow()[&(start as u32)])
    }
    /// The definitions whose signatures a body reads directly: its own header,
    /// every resolved name and every non-negative lookup result, each once.
    fn signature_start(
        &self,
        index: &NameIndex,
        module: ModuleId,
        own: DefinitionId,
        resolution: &Resolution,
    ) -> Vec<usize> {
        let mut pending = Vec::with_capacity(1 + resolution.names.len() + resolution.reads.len());
        let mut reads = self.signature_reads.borrow_mut();
        let epoch = reads.begin(index.definitions.len());
        let mut insert = |id: DefinitionId| {
            let row = id.0 as usize;
            let stamp = &mut reads.stamps[row];
            if *stamp != epoch {
                *stamp = epoch;
                pending.push(row);
            }
        };
        insert(own);
        let mut target = |t: Target| match t {
            Target::Definition(id) => insert(id),
            Target::ContextualConstructor { name } => {
                for &id in self.constructors(index, module, name).iter() {
                    insert(id);
                }
            }
            _ => {}
        };
        for name in &resolution.names {
            target(name.target);
        }
        // Negative, ambiguous and shadowed lookup dependencies remain unchanged.
        for read in &resolution.reads {
            match &read.result {
                LookupResult::Found(t) | LookupResult::Shadowed(t) => target(*t),
                LookupResult::Private(p) | LookupResult::Opaque(p) => {
                    target(Target::Definition(*p))
                }
                LookupResult::Ambiguous | LookupResult::Missing => {}
            }
        }
        pending.sort_unstable();
        pending
    }
    /// One hash over the closures of the core prelude types, which lowering
    /// builds, matches or lays out with no resolved name of the body reaching
    /// them; every body's signatures include it.
    fn prelude_closure(&self, index: &NameIndex) -> io::Result<ContentHash> {
        if let Some(hash) = self.prelude_closure.get() {
            return Ok(*hash);
        }
        let mut closures = Vec::with_capacity(self.prelude.len());
        for &id in &self.prelude {
            closures.push(self.closure(index, id)?);
        }
        let hash = ContentHash::of_parts(
            "hirsel.prelude-closure.v1",
            closures.iter().map(|c| &c.0[..]),
        );
        Ok(*self.prelude_closure.get_or_init(|| hash))
    }
    /// What every body reads beside its names: the prelude's closures
    /// (`prelude_closure`) and the closures of std's structural codec makers
    /// (`hirsel_core_front::STRUCTURAL_CODECS`) and tuple protocol signatures,
    /// each or its absence, in every std
    /// the index holds. Laying out a closed codec List, Map, Option or Result row
    /// makes its maker the row's codec hook, so every body reads them as it reads
    /// the prelude; std is not core's image, so this part is the request's, not
    /// sealed with the prelude's. One component, once per request.
    fn prelude_reads(&self, index: &NameIndex) -> io::Result<ContentHash> {
        if let Some(hash) = self.prelude_reads.get() {
            return Ok(*hash);
        }
        let prelude = self.prelude_closure(index)?;
        let makers: Vec<_> = codec_reads(index)
            .map(|id| id.map(|id| self.closure(index, id.0 as usize)).transpose())
            .collect::<io::Result<_>>()?;
        // Without std the component is the prelude's own hash.
        let hash = match makers.is_empty() {
            true => prelude,
            false => ContentHash::of_parts(
                "hirsel.prelude-reads.v1",
                [&prelude.0[..], &encode(&makers)?[..]],
            ),
        };
        Ok(*self.prelude_reads.get_or_init(|| hash))
    }
    /// A body's own item source digest (`body_identity`): the one input of
    /// its key that a recorded key under the same shape is checked against.
    pub fn body_source(
        &self,
        index: &NameIndex,
        path: &DefPath,
        module: &ModuleInput,
        ordinal: u32,
        item: Item<'_>,
    ) -> io::Result<ContentHash> {
        let own = index
            .item_definition(module.id, module.source, ordinal, item)
            .ok_or_else(|| {
                io::Error::other(format!(
                    "internal error: core reuse body `{path}` has no indexed definition"
                ))
            })?;
        Ok(self.dependency(index, own.0 as usize)?.source)
    }
    pub fn for_body(
        &self,
        index: &NameIndex,
        path: &DefPath,
        module: &ModuleInput,
        ordinal: u32,
        item: Item<'_>,
        resolution: &Resolution,
    ) -> io::Result<DefinitionKey> {
        hirsel_core_front::cost_scope!("phase", "driver keys/anchors");
        let own = index
            .item_definition(module.id, module.source, ordinal, item)
            .ok_or_else(|| {
                io::Error::other(format!(
                    "internal error: core reuse body `{path}` has no indexed definition"
                ))
            })?;
        let module = module.id;
        let pending = self.signature_start(index, module, own, resolution);
        // The forms among the definitions the body names.
        let mut named = vec![];
        let signatures = {
            hirsel_core_front::cost_scope!("key work", "signature closure");
            let mut components = Vec::with_capacity(pending.len() + 1);
            for &id in &pending {
                if self
                    .form_bits
                    .get(id >> 6)
                    .is_some_and(|w| w >> (id & 63) & 1 != 0)
                {
                    named.push(id);
                }
                components.push(self.closure(index, id)?);
            }
            components.push(self.prelude_reads(index)?);
            components.sort_unstable();
            components.dedup();
            crate::meter::count(crate::meter::Count::BodyKeys, 1);
            crate::meter::count(
                crate::meter::Count::BodyKeyBytes,
                32 * components.len() as u64,
            );
            ContentHash::of_parts(
                "hirsel.signature-closure.v3",
                components.iter().map(|c| &c.0[..]),
            )
        };
        let lookups = {
            hirsel_core_front::cost_scope!("key work", "lookup rows");
            self.lookups(index, module, resolution)?
        };
        // Program inputs a body's lowering reads beyond its resolved names'
        // closures, each keyed only for the bodies that name the form that
        // reads it.
        let mut siblings = vec![];
        if !named.is_empty() {
            for &(id, sibling) in &self.forms {
                if named.contains(&(id.0 as usize)) {
                    siblings.push(self.sibling_reads(index, sibling)?);
                }
            }
        }
        siblings.sort_unstable();
        siblings.dedup();
        Ok(DefinitionKey {
            body: self.dependency(index, own.0 as usize)?.source,
            signatures,
            lookups,
            environment: self.environment,
            // One callee's hash is its own; several are hashed together.
            siblings: match siblings[..] {
                [] => None,
                [one] => Some(one),
                _ => Some(ContentHash::of_parts(
                    "hirsel.key-siblings.v1",
                    siblings.iter().map(|c| &c.0[..]),
                )),
            },
        })
    }
    /// The closures of the items lowering builds beside a
    /// `hirsel_core_front::SIBLING_READS` callee (`Keys::siblings[k]`), each
    /// or its absence, once per request.
    fn sibling_reads(&self, index: &NameIndex, k: usize) -> io::Result<ContentHash> {
        let siblings = &self.siblings[k];
        if let Some(hash) = siblings.hash.get() {
            return Ok(*hash);
        }
        let reads: Vec<_> = siblings
            .inputs(index)
            .map(|id| id.map(|id| self.closure(index, id.0 as usize)).transpose())
            .collect::<io::Result<_>>()?;
        let hash = ContentHash::of_parts(
            "hirsel.sibling-reads.v1",
            [&encode(&(siblings.row, reads))?[..]],
        );
        Ok(*siblings.hash.get_or_init(|| hash))
    }
    /// One part over the body's lookups outside the item: their count, then
    /// each row in resolution order: namespace, scope kind, the scope's module
    /// or package identity, the constructor flag, the symbol's text, then the
    /// result tag and its target. Every variable-length field carries its
    /// length and every tag fixes the fields after it, so the rows need no
    /// framing of their own.
    ///
    /// A lookup in a local, binding or label scope is left out: its result is
    /// a binder of this item or missing, fixed by the item's text, which the
    /// key's body digest hashes, and a binding that shadows a module or
    /// prelude name is fixed by that name's own module or prelude row here.
    fn lookups(
        &self,
        index: &NameIndex,
        module: ModuleId,
        resolution: &Resolution,
    ) -> io::Result<ContentHash> {
        ContentHash::of_parts_stream("hirsel.body-lookups.v4", |parts| {
            let mut row = Vec::with_capacity(64 * resolution.reads.len() + 8);
            row.extend_from_slice(&[0; 8]);
            let mut count = 0u64;
            for r in &resolution.reads {
                let (kind, scope) = match r.scope {
                    LookupScope::Local(_) | LookupScope::Bind(_) | LookupScope::Label(_) => {
                        continue;
                    }
                    LookupScope::Module(m) => (0u8, self.module_identity(index, m)?),
                    LookupScope::Prelude(p) => (1, self.package_identity(index, p)?),
                    LookupScope::Modules(p) => (2, self.package_identity(index, p)?),
                    LookupScope::Children(m) => (3, self.module_identity(index, m)?),
                    LookupScope::Variants(m) => (4, self.module_identity(index, m)?),
                };
                count += 1;
                row.push(match r.namespace {
                    Namespace::Type => 0,
                    Namespace::Value => 1,
                });
                row.push(kind);
                row.extend_from_slice(&scope.0);
                row.push(r.constructor as u8);
                Self::text(index, r.symbol, &mut row);
                match r.result {
                    LookupResult::Found(t) => {
                        row.push(0);
                        self.target(index, module, t, &mut row)?;
                    }
                    LookupResult::Missing => row.push(1),
                    LookupResult::Private(d) => {
                        row.push(2);
                        self.target(index, module, Target::Definition(d), &mut row)?;
                    }
                    LookupResult::Opaque(d) => {
                        row.push(3);
                        self.target(index, module, Target::Definition(d), &mut row)?;
                    }
                    LookupResult::Shadowed(t) => {
                        row.push(4);
                        self.target(index, module, t, &mut row)?;
                    }
                    LookupResult::Ambiguous => row.push(5),
                }
            }
            row[..8].copy_from_slice(&count.to_le_bytes());
            parts.part_byte_slice(0, &row);
            Ok(())
        })
    }
    /// A symbol's interned text with its length.
    fn text(index: &NameIndex, symbol: Symbol, row: &mut Vec<u8>) {
        let text = index.symbols.text(symbol);
        row.extend_from_slice(&(text.len() as u32).to_le_bytes());
        row.extend_from_slice(text.as_bytes());
    }
    /// A lookup target: a tag, then its identity, binder, text or, for a
    /// contextual constructor, its name and its candidates' identities in order.
    fn target(
        &self,
        index: &NameIndex,
        module: ModuleId,
        target: Target,
        row: &mut Vec<u8>,
    ) -> io::Result<()> {
        match target {
            Target::Definition(d) => {
                row.push(0);
                row.extend_from_slice(&self.definition_identity(index, d)?.0);
                row.push(u8::from(index.definition_is_foreign(d)));
            }
            Target::Module(m) => {
                row.push(1);
                row.extend_from_slice(&self.module_identity(index, m)?.0);
            }
            Target::Binder(b) => {
                row.push(2);
                row.extend_from_slice(&b.to_le_bytes());
            }
            Target::Builtin(s) => {
                row.push(3);
                Self::text(index, s, row);
            }
            Target::ContextualConstructor { name } => {
                row.push(4);
                Self::text(index, name, row);
                let candidates = self.constructors(index, module, name);
                row.extend_from_slice(&(candidates.len() as u32).to_le_bytes());
                for &d in candidates.iter() {
                    row.extend_from_slice(&self.definition_identity(index, d)?.0);
                }
            }
            Target::Missing => row.push(5),
        }
        Ok(())
    }
}
/// The item-relative end of the header: a function's body block start, else
/// the item's end. Node spans are item-relative.
fn header_end(item: Item<'_>) -> usize {
    if !matches!(
        item.tree.node(item.root).kind,
        NodeKind::Function | NodeKind::CloneHook
    ) {
        return item.source_text().len();
    }
    item.tree
        .children(item.root)
        .iter()
        .find_map(|&c| {
            let n = item.tree.node(c);
            (n.kind == NodeKind::Block).then_some(n.origin.span.start as usize)
        })
        .unwrap_or(item.source_text().len())
}

#[cfg(test)]
thread_local! {
    /// Tests stand in for another compiler build with another identity.
    pub(crate) static TOOLCHAIN: std::cell::Cell<Option<ContentHash>> = const { std::cell::Cell::new(None) };
}
pub(crate) fn compiler_identity() -> io::Result<ContentHash> {
    #[cfg(test)]
    if let Some(identity) = TOOLCHAIN.get() {
        return Ok(identity);
    }
    static ID: std::sync::LazyLock<io::Result<ContentHash>> = std::sync::LazyLock::new(|| {
        hirsel_core_front::cost_scope!("non-check", "compiler executable identity");
        // The linker has already identified this executable. Read that immutable
        // note, rather than scanning and hashing the whole compiler on every launch.
        // /proc/self/exe stays bound to the loaded inode across binary replacement.
        let path = if cfg!(target_os = "linux") {
            PathBuf::from("/proc/self/exe")
        } else {
            std::env::current_exe()?
        };
        compiler_stream_identity(&mut std::fs::File::open(path)?)
    });
    ID.as_ref()
        .copied()
        .map_err(|e| io::Error::other(e.to_string()))
}

fn compiler_stream_identity(reader: &mut (impl io::Read + io::Seek)) -> io::Result<ContentHash> {
    use object::Object as _;
    let linked = {
        let cache = object::read::ReadCache::new(&mut *reader);
        object::File::parse(&cache).ok().and_then(|binary| {
            binary
                .build_id()
                .ok()
                .flatten()
                .filter(|id| !id.is_empty())
                .map(|id| ContentHash::of_parts("hirsel.compiler-link-id.v1", [id]))
        })
    };
    if let Some(identity) = linked {
        return Ok(identity);
    }
    // Linkers/formats without a build ID retain the complete executable identity.
    reader.rewind()?;
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    Ok(ContentHash::of_bytes(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_follows_moved_items_and_tracks_their_own_bytes() {
        let parse = |source: &str| {
            hirsel_core_syntax::parse_bytes(
                source.as_bytes(),
                hirsel_core_syntax::ParseMode::Source,
                &mut hirsel_core_syntax::Symbols::default(),
            )
            .syntax
        };
        // Item 1 is `main`. Moving it (item 0 gains lines) keeps its bytes and
        // a comment line above it is owned trivia outside its body key, while
        // the item-relative sites continue to bind from the first token.
        let original = parse("fn a() -> u64 { 1 }\nfn main() -> u64 {\n    7u64\n}\n");
        let moved = parse("fn a() -> u64 {\n    1\n}\nfn main() -> u64 {\n    7u64\n}\n");
        let commented =
            parse("fn a() -> u64 { 1 }\n// above main\nfn main() -> u64 {\n    7u64\n}\n");
        let changed = parse("fn a() -> u64 { 1 }\nfn main() -> u64 {\n    8u64\n}\n");
        let (o, m, c, x) = (
            original.item(1),
            moved.item(1),
            commented.item(1),
            changed.item(1),
        );
        let header = |item| header_identity(item, header_tokens(item, header_end(item)));
        assert_eq!(header(o), header(m));
        assert_eq!(body_identity(o), body_identity(m));
        assert_eq!(header(o), header(c));
        assert_eq!(body_identity(o), body_identity(c));
        assert_eq!(header(o), header(x));
        assert_ne!(body_identity(o), body_identity(x));
    }

    #[test]
    fn compiler_link_id_changes_identity_and_missing_notes_keep_full_hashing() {
        // Minimal ELF64 executable: one PT_NOTE at byte 120, a GNU build ID of
        // twenty bytes. The note and ELF field offsets are from the ELF ABI.
        let mut elf = vec![0u8; 156];
        elf[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        elf[16..18].copy_from_slice(&2u16.to_le_bytes());
        elf[18..20].copy_from_slice(&62u16.to_le_bytes());
        elf[20..24].copy_from_slice(&1u32.to_le_bytes());
        elf[32..40].copy_from_slice(&64u64.to_le_bytes());
        elf[52..54].copy_from_slice(&64u16.to_le_bytes());
        elf[54..56].copy_from_slice(&56u16.to_le_bytes());
        elf[56..58].copy_from_slice(&1u16.to_le_bytes());
        elf[58..60].copy_from_slice(&64u16.to_le_bytes());
        elf[64..68].copy_from_slice(&4u32.to_le_bytes());
        elf[72..80].copy_from_slice(&120u64.to_le_bytes());
        elf[96..104].copy_from_slice(&36u64.to_le_bytes());
        elf[104..112].copy_from_slice(&36u64.to_le_bytes());
        elf[112..120].copy_from_slice(&4u64.to_le_bytes());
        elf[120..124].copy_from_slice(&4u32.to_le_bytes());
        elf[124..128].copy_from_slice(&20u32.to_le_bytes());
        elf[128..132].copy_from_slice(&3u32.to_le_bytes());
        elf[132..136].copy_from_slice(b"GNU\0");
        for (i, b) in elf[136..].iter_mut().enumerate() {
            *b = i as u8;
        }
        // Independent SHA-256 of the domain and ID, each framed by u64 LE length.
        assert_eq!(
            compiler_stream_identity(&mut io::Cursor::new(&elf))
                .unwrap()
                .0,
            [
                169, 29, 73, 236, 7, 183, 253, 11, 157, 25, 184, 241, 93, 180, 53, 99, 6, 172, 106,
                55, 33, 198, 174, 102, 215, 24, 56, 121, 46, 189, 104, 222
            ]
        );
        elf[136] = 255;
        assert_eq!(
            compiler_stream_identity(&mut io::Cursor::new(&elf))
                .unwrap()
                .0,
            [
                178, 69, 107, 16, 225, 205, 99, 73, 2, 221, 87, 102, 200, 207, 27, 15, 58, 15, 242,
                147, 200, 57, 12, 20, 206, 186, 7, 15, 122, 208, 179, 22
            ]
        );
        assert_eq!(
            compiler_stream_identity(&mut io::Cursor::new(b"compiler-without-a-note"))
                .unwrap()
                .0,
            [
                88, 28, 171, 172, 129, 86, 81, 147, 24, 134, 60, 221, 161, 178, 132, 193, 24, 62,
                80, 145, 143, 22, 81, 140, 69, 22, 97, 225, 130, 98, 156, 238
            ]
        );
    }
}
