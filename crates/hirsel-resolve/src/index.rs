use hirsel_diag::{Diagnostic, Label, RuleId};
use hirsel_intern::{IdHasher, Symbol};
use hirsel_span::Span;
use hirsel_syntax::*;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::Arc,
};

mod retained;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Namespace {
    Type,
    Value,
}

/// Metadata only: indexing never reads a definition's body or infers a signature.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DefinitionEntry {
    pub definition: DefPath,
    pub namespace: Namespace,
    pub visibility: Visibility,
    pub span: Span,
}

/// A manifest import exposes a module head, not unqualified values (LANG-4).
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct DependencyExposure {
    pub head: String,
    pub package: PackageId,
    pub modules: Vec<String>,
}

/// Small build-input boundary pending the fixture format in spec issue #4.
/// Dependencies are explicit snapshots; no file discovery or source imports.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManifestFixture {
    pub package: PackageId,
    pub definitions: Vec<DefinitionEntry>,
    pub exposures: Vec<DependencyExposure>,
    /// Installed packages' names (each image's N section). A package listed
    /// here answers from its segment alone; `definitions` of it are ignored.
    pub installed: Vec<Arc<PackageNames>>,
    /// Covers the entire manifest, including dependency membership and visibility.
    pub revision: ContentHash,
}

/// One installed package's half of the scope tables: its definitions keyed by
/// (module, symbol), built once per installation and shared by every index
/// that selects it. Construction refuses what the image's construction
/// refuses, so a segment never carries a diagnostic.
#[derive(Debug)]
pub struct PackageNames {
    package: PackageId,
    entries: DefMap<DefinitionEntry>,
    tables: Tables,
    retained: u64,
}
impl PackageNames {
    pub fn new(
        package: PackageId,
        definitions: impl IntoIterator<Item = DefinitionEntry>,
    ) -> Result<Self, String> {
        let mut entries: DefMap<DefinitionEntry> = DefMap::default();
        for entry in definitions {
            let path = &entry.definition;
            if path.0.package != package || !canonical(path) {
                return Err(format!("{path:?} is not a canonical path of {package:?}"));
            }
            let id = DefId::intern(path);
            if entries.insert(id, entry).is_some() {
                return Err(format!("{:?} is declared twice", id.path()));
            }
        }
        let tables = Tables::new(entries.keys().map(|&id| (id, id.path())));
        let retained = retained::package_names(&package, &entries, &tables);
        Ok(Self {
            package,
            entries,
            tables,
            retained,
        })
    }
    pub fn package(&self) -> &PackageId {
        &self.package
    }
    pub fn entries(&self) -> impl Iterator<Item = &DefinitionEntry> {
        self.entries.values()
    }
    /// What this segment retains; its owner charges it once.
    pub fn retained_bytes(&self) -> u64 {
        self.retained
    }
}

/// Its tables are a function of its entries.
impl PartialEq for PackageNames {
    fn eq(&self, other: &Self) -> bool {
        self.package == other.package && self.entries == other.entries
    }
}
impl Eq for PackageNames {}

fn canonical(path: &DefPath) -> bool {
    let path = &path.0;
    let segment = |name: &str| !name.is_empty() && !name.contains(['/', '.', '@']);
    segment(&path.package.name)
        && !path.package.version.is_empty()
        && segment(&path.name)
        && path.modules.iter().all(|m| segment(m))
}

#[derive(Clone, Debug)]
pub struct ManifestIndex {
    pub package: PackageId,
    pub revision: ContentHash,
    entries: Arc<DefMap<DefinitionEntry>>,
    heads: Arc<BTreeMap<String, BTreeSet<(PackageId, Vec<String>)>>>,
    tables: Arc<Tables>,
    /// Segments 1.. of the scope tables; segment 0 is `tables`.
    installed: Arc<[Arc<PackageNames>]>,
    /// Each exposed head's targets, in `heads` order: a candidate set, never
    /// one answer, so ambiguity and the core/std merge stay decidable.
    head_targets: Arc<HashMap<Symbol, HeadTargets, IdHasher>>,
}

/// One node per (package, module path) some definition lives under, within
/// one segment's tables.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
struct ModuleId(u32);

/// A module node of the joined scope tables: segment 0 is the request's own
/// tables, segment i + 1 is installed package i. A walk stays in the segment
/// it starts in, since a package's modules all live in one segment.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
struct Node {
    segment: u32,
    module: ModuleId,
}

/// Scope tables: module nodes keyed by (parent, symbol) and definitions keyed
/// by (module, symbol). A lookup walks symbols; it never spells or interns a
/// candidate path to ask whether it exists.
#[derive(Debug, Default)]
struct Tables {
    roots: HashMap<PackageId, ModuleId>,
    children: HashMap<(ModuleId, Symbol), ModuleId, IdHasher>,
    names: HashMap<(ModuleId, Symbol), DefId, IdHasher>,
}

#[derive(Debug)]
struct HeadTargets {
    targets: Vec<(PackageId, Vec<String>, Option<Node>)>,
    /// Only identical core/std module paths share a namespace. Two std majors
    /// remain ambiguous even if only one declares the member.
    merged: bool,
}

/// Where one file's unqualified paths start: its package root and its module.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModuleScope {
    root: Option<Node>,
    module: Option<Node>,
}

impl Tables {
    fn new<'a>(paths: impl IntoIterator<Item = (DefId, &'a DefPath)>) -> Self {
        let mut tables = Self::default();
        let mut next = 0;
        let node = |next: &mut u32| {
            *next += 1;
            ModuleId(*next - 1)
        };
        for (id, path) in paths {
            let mut module = *tables
                .roots
                .entry(path.0.package.clone())
                .or_insert_with(|| node(&mut next));
            for name in &path.0.modules {
                module = *tables
                    .children
                    .entry((module, Symbol::intern(name)))
                    .or_insert_with(|| node(&mut next));
            }
            tables
                .names
                .insert((module, Symbol::intern(&path.0.name)), id);
        }
        tables
    }
    fn walk(
        &self,
        from: ModuleId,
        names: impl IntoIterator<Item = Option<Symbol>>,
    ) -> Option<ModuleId> {
        names.into_iter().try_fold(from, |module, name| {
            self.children.get(&(module, name?)).copied()
        })
    }
}

impl ManifestIndex {
    fn segment(&self, segment: u32) -> &Tables {
        match segment.checked_sub(1) {
            None => &self.tables,
            Some(i) => &self.installed[i as usize].tables,
        }
    }
    fn root(&self, package: &PackageId) -> Option<Node> {
        if let Some(i) = self.installed.iter().position(|n| n.package == *package) {
            return Some(Node {
                segment: i as u32 + 1,
                module: *self.installed[i].tables.roots.get(package)?,
            });
        }
        Some(Node {
            segment: 0,
            module: *self.tables.roots.get(package)?,
        })
    }
    fn walk(
        &self,
        from: Option<Node>,
        names: impl IntoIterator<Item = Option<Symbol>>,
    ) -> Option<Node> {
        let from = from?;
        Some(Node {
            segment: from.segment,
            module: self.segment(from.segment).walk(from.module, names)?,
        })
    }
    fn module(&self, package: &PackageId, modules: &[String]) -> Option<Node> {
        self.walk(
            self.root(package),
            modules.iter().map(|m| Symbol::existing(m)),
        )
    }
    fn member(
        &self,
        from: Option<Node>,
        modules: &[Option<Symbol>],
        name: Option<Symbol>,
    ) -> Option<DefId> {
        let node = self.walk(from, modules.iter().copied())?;
        self.segment(node.segment)
            .names
            .get(&(node.module, name?))
            .copied()
    }
    fn child(&self, node: Node, name: Symbol) -> bool {
        self.segment(node.segment)
            .children
            .contains_key(&(node.module, name))
    }
    fn entry_of(&self, id: DefId) -> Option<&DefinitionEntry> {
        self.entries
            .get(&id)
            .or_else(|| self.installed.iter().find_map(|n| n.entries.get(&id)))
    }
    fn all_entries(&self) -> impl Iterator<Item = &DefinitionEntry> {
        self.entries
            .values()
            .chain(self.installed.iter().flat_map(|n| n.entries.values()))
    }
    fn heads(
        &self,
        heads: &BTreeMap<String, BTreeSet<(PackageId, Vec<String>)>>,
    ) -> HashMap<Symbol, HeadTargets, IdHasher> {
        heads
            .iter()
            .map(|(head, exposed)| {
                let merged = exposed.len() == 2
                    && exposed.iter().any(|(p, _)| p.package_is("core", "1"))
                    && exposed.iter().any(|(p, _)| p.name_is("std"))
                    && exposed
                        .iter()
                        .map(|(_, m)| m)
                        .collect::<BTreeSet<_>>()
                        .len()
                        == 1;
                let targets = exposed
                    .iter()
                    .map(|(package, prefix)| {
                        (
                            package.clone(),
                            prefix.clone(),
                            self.module(package, prefix),
                        )
                    })
                    .collect();
                (Symbol::intern(head), HeadTargets { targets, merged })
            })
            .collect()
    }
}

pub use hirsel_types::LookupError;

impl ManifestFixture {
    /// Select declared dependency signatures for a phase fixture. Their module
    /// heads and visibility use the same index as ordinary source declarations.
    pub fn with_dependency_signatures<'a>(
        mut self,
        signatures: impl IntoIterator<Item = &'a hirsel_types::SignatureRef>,
    ) -> Self {
        for signature in signatures {
            let path = signature.definition.path();
            if path.0.package == self.package {
                continue;
            }
            self.definitions.push(DefinitionEntry {
                definition: path.clone(),
                namespace: if matches!(signature.kind, hirsel_types::SignatureKind::Type(_)) {
                    Namespace::Type
                } else {
                    Namespace::Value
                },
                visibility: signature.metadata.visibility,
                span: Span::default(),
            });
            if let Some(head) = path.0.modules.first()
                && !head.starts_with('$')
            {
                self.exposures.push(DependencyExposure {
                    head: head.clone(),
                    package: path.0.package.clone(),
                    modules: vec![head.clone()],
                });
            }
        }
        self
    }

    pub fn from_files<'a>(
        package: PackageId,
        files: impl IntoIterator<Item = &'a SyntaxFile>,
        revision: ContentHash,
    ) -> Self {
        let definitions = files
            .into_iter()
            .flat_map(|file| {
                file.items.iter().map(|syntax| {
                    let item = &syntax.item;
                    DefinitionEntry {
                        definition: item.definition.clone(),
                        namespace: if matches!(item.kind, ItemKind::Type { .. }) {
                            Namespace::Type
                        } else {
                            Namespace::Value
                        },
                        visibility: item.visibility,
                        span: item.name_span,
                    }
                })
            })
            .collect();
        Self {
            package,
            definitions,
            exposures: vec![],
            installed: vec![],
            revision,
        }
    }
}

impl ManifestIndex {
    /// Duplicate identity is an error, never repaired using a source position.
    /// A path's first entry is its earliest site: parsing allocates anchors in
    /// source order, so (anchor, offsets) orders one parse's sites.
    pub fn new(mut fixture: ManifestFixture) -> (Self, Vec<Diagnostic>) {
        fixture.definitions.sort_by(|a, b| {
            a.definition.cmp(&b.definition).then_with(|| {
                (a.span.anchor, a.span.start, a.span.end).cmp(&(
                    b.span.anchor,
                    b.span.start,
                    b.span.end,
                ))
            })
        });
        let mut entries: DefMap<DefinitionEntry> = DefMap::default();
        let mut diagnostics = vec![];
        let mut installed: Vec<Arc<PackageNames>> = vec![];
        for names in fixture.installed {
            if !installed.iter().any(|n| n.package == names.package) {
                installed.push(names);
            }
        }
        for entry in fixture.definitions {
            if installed
                .iter()
                .any(|n| n.package == entry.definition.0.package)
            {
                continue;
            }
            if !canonical(&entry.definition) {
                diagnostics.push(Diagnostic::error(
                    const { RuleId::of("PROG-1") },
                    entry.span,
                    "definition path is not canonical",
                ));
                continue;
            }
            if let Some(first) = entries.get(&DefId::intern(&entry.definition)) {
                let mut diagnostic = Diagnostic::error(
                    const { RuleId::of("LANG-7") },
                    entry.span,
                    "duplicate definition path; overloading is forbidden",
                );
                diagnostic.secondary.push(Label {
                    span: first.span,
                    message: "first definition of this path".into(),
                });
                diagnostics.push(diagnostic);
            } else {
                entries.insert(DefId::intern(&entry.definition), entry);
            }
        }
        let mut heads: BTreeMap<String, BTreeSet<(PackageId, Vec<String>)>> = BTreeMap::new();
        for exposure in fixture.exposures {
            heads
                .entry(exposure.head)
                .or_default()
                .insert((exposure.package, exposure.modules));
        }
        let tables = Tables::new(entries.keys().map(|&id| (id, id.path())));
        let mut index = Self {
            package: fixture.package,
            revision: fixture.revision,
            entries: Arc::new(entries),
            heads: Arc::new(BTreeMap::new()),
            tables: Arc::new(tables),
            installed: installed.into(),
            head_targets: Arc::default(),
        };
        index.head_targets = Arc::new(index.heads(&heads));
        index.heads = Arc::new(heads);
        (index, diagnostics)
    }

    /// Select a current package while sharing the same immutable declaration index.
    pub fn for_package(&self, package: PackageId) -> Self {
        Self {
            package,
            ..self.clone()
        }
    }

    /// Select the importing package's explicit module heads while sharing declarations.
    pub fn with_exposures(&self, exposures: &[DependencyExposure]) -> Self {
        let mut heads: BTreeMap<String, BTreeSet<(PackageId, Vec<String>)>> = BTreeMap::new();
        for exposure in exposures {
            heads
                .entry(exposure.head.clone())
                .or_default()
                .insert((exposure.package.clone(), exposure.modules.clone()));
        }
        Self {
            head_targets: Arc::new(self.heads(&heads)),
            heads: Arc::new(heads),
            ..self.clone()
        }
    }

    /// Type names available without a module head, including core defaults.
    /// The caller records the manifest lookup that led to this candidate search.
    pub fn unqualified_types(&self, context: &FileContext) -> Vec<DefPath> {
        self.all_entries()
            .filter(|entry| {
                let path = &entry.definition;
                entry.namespace == Namespace::Type
                    && ((path.0.package == context.package
                        && (path.0.modules == context.modules || path.0.modules.is_empty()))
                        || (path.package_is("core", "1") && path.0.modules.is_empty()))
                    && self.access(context, path).is_ok()
            })
            .map(|entry| entry.definition.clone())
            .collect()
    }

    pub fn entry(&self, definition: &DefPath) -> Option<&DefinitionEntry> {
        self.entry_of(DefId::get(definition)?)
    }

    /// Where `context`'s unqualified paths start; computed once per file context.
    pub fn scope(&self, context: &FileContext) -> ModuleScope {
        let root = self.root(&context.package);
        ModuleScope {
            root,
            module: self.walk(root, context.modules.iter().map(|m| Symbol::existing(m))),
        }
    }

    pub fn has_head(&self, context: &FileContext, head: &str) -> bool {
        Symbol::existing(head).is_some_and(|head| self.has_head_in(self.scope(context), head))
    }

    /// An exposed dependency head, or a module under the package root or the file's module.
    pub fn has_head_in(&self, scope: ModuleScope, head: Symbol) -> bool {
        self.head_targets.contains_key(&head)
            || [scope.root, scope.module]
                .into_iter()
                .flatten()
                .any(|module| self.child(module, head))
    }

    /// Accessible replacements for one path segment. Reuse real lookup so
    /// dependency aliases, module precedence, ambiguity and privacy agree.
    pub(crate) fn candidate_names(
        &self,
        context: &FileContext,
        package: Option<&PackageId>,
        segments: &[String],
        segment: usize,
    ) -> Vec<String> {
        let mut names = BTreeSet::new();
        // Derive only spellings that can name a declaration. Trying every
        // name in every module interns a Cartesian product of nonexistent
        // DefPaths in lookup_spelling, and those ids live for the process.
        let mut consider = |head: Option<&String>, modules: &[String], name: &String| {
            let spelling = || head.into_iter().chain(modules).chain([name]);
            if usize::from(head.is_some()) + modules.len() + 1 == segments.len()
                && spelling()
                    .zip(segments)
                    .enumerate()
                    .all(|(i, (a, b))| i == segment || a == b)
            {
                names.insert(spelling().nth(segment).expect("a path segment").clone());
            }
        };
        for definition in self.all_entries().map(|entry| &entry.definition.0) {
            if let Some(package) = package {
                if definition.package == *package {
                    consider(None, &definition.modules, &definition.name);
                }
                continue;
            }
            if definition.package == context.package {
                consider(None, &definition.modules, &definition.name);
                if let Some(relative) = definition.modules.strip_prefix(context.modules.as_slice())
                {
                    consider(None, relative, &definition.name);
                }
            }
            for (head, exposed) in &*self.heads {
                for (package, prefix) in exposed {
                    if definition.package == *package
                        && let Some(relative) = definition.modules.strip_prefix(prefix.as_slice())
                    {
                        consider(Some(head), relative, &definition.name);
                    }
                }
            }
        }
        names
            .into_iter()
            .filter(|name| {
                let mut candidate = segments.to_vec();
                candidate[segment] = name.clone();
                self.lookup_spelling(context, package, &candidate).is_ok()
            })
            .collect()
    }

    /// The accessible spelling repairs a missing lookup's diagnostic observes.
    /// Unrelated packages and physical manifest paths do not affect this input.
    pub fn lookup_candidates_revision(
        &self,
        context: &FileContext,
        package: Option<&PackageId>,
        segments: &[String],
    ) -> ContentHash {
        let parts: Vec<_> = (0..segments.len())
            .map(|segment| {
                let names = self.candidate_names(context, package, segments, segment);
                ContentHash::of_parts(
                    "hirsel.lookup.segment-candidates.v1",
                    names.iter().map(String::as_bytes),
                )
            })
            .collect();
        ContentHash::of_parts(
            "hirsel.lookup.candidates.v1",
            parts.iter().map(|hash| hash.0.as_slice()),
        )
    }

    pub fn lookup(&self, context: &FileContext, path: &Path) -> Result<DefPath, LookupError> {
        self.lookup_in(context, self.scope(context), path)
    }

    /// `lookup` from a file context's precomputed scope.
    pub fn lookup_in(
        &self,
        context: &FileContext,
        scope: ModuleScope,
        path: &Path,
    ) -> Result<DefPath, LookupError> {
        let segments: Vec<_> = path.segments.iter().map(|&s| Some(s)).collect();
        self.resolve(context, scope, path.package.as_ref(), &segments, &|i| {
            path.segments[i].as_str().to_owned()
        })
    }

    pub fn lookup_spelling(
        &self,
        context: &FileContext,
        package: Option<&PackageId>,
        segments: &[String],
    ) -> Result<DefPath, LookupError> {
        let symbols: Vec<_> = segments.iter().map(|s| Symbol::existing(s)).collect();
        self.resolve(context, self.scope(context), package, &symbols, &|i| {
            segments[i].clone()
        })
    }

    /// A qualified path names its package's module path. Otherwise the own
    /// package wins (the file's module, then the root), even when private,
    /// before a dependency head; a head with several targets is ambiguous
    /// unless it is the core/std merge. Absent symbols name nothing.
    fn resolve(
        &self,
        context: &FileContext,
        scope: ModuleScope,
        package: Option<&PackageId>,
        segments: &[Option<Symbol>],
        spell: &dyn Fn(usize) -> String,
    ) -> Result<DefPath, LookupError> {
        let (&name, modules) = segments.split_last().ok_or(LookupError::Missing)?;
        if let Some(package) = package {
            return match self.member(self.root(package), modules, name) {
                Some(id) => self.access_id(context, id),
                None => Err(LookupError::Missing),
            };
        }
        if let Some(id) = self
            .member(scope.module, modules, name)
            .or_else(|| self.member(scope.root, modules, name))
        {
            return self.access_id(context, id);
        }
        if segments.len() < 2 {
            return Err(LookupError::Missing);
        }
        let Some(head) = segments[0].and_then(|head| self.head_targets.get(&head)) else {
            return Err(LookupError::Missing);
        };
        let members = &modules[1..];
        let candidates = || {
            head.targets
                .iter()
                .map(|(package, prefix, _)| {
                    let mut modules = prefix.clone();
                    modules.extend((1..segments.len() - 1).map(spell));
                    DefPath(ItemPath {
                        package: package.clone(),
                        modules,
                        name: spell(segments.len() - 1),
                    })
                })
                .collect()
        };
        if head.targets.len() > 1 && !head.merged {
            return Err(LookupError::Ambiguous(candidates()));
        }
        for &(_, _, module) in &head.targets {
            if let Some(id) = self.member(module, members, name) {
                return self.access_id(context, id);
            }
        }
        Err(LookupError::MissingMember(candidates()))
    }

    fn access_id(&self, context: &FileContext, id: DefId) -> Result<DefPath, LookupError> {
        self.access_entry(context, id.path(), self.entry_of(id))
    }

    pub fn access(
        &self,
        context: &FileContext,
        definition: &DefPath,
    ) -> Result<DefPath, LookupError> {
        self.access_entry(
            context,
            definition,
            DefId::get(definition).and_then(|id| self.entry_of(id)),
        )
    }

    fn access_entry(
        &self,
        context: &FileContext,
        definition: &DefPath,
        entry: Option<&DefinitionEntry>,
    ) -> Result<DefPath, LookupError> {
        if let Some(entry) = entry
            && entry.visibility == Visibility::Private
            && context.package != definition.0.package
        {
            return Err(LookupError::Invisible(definition.clone()));
        }
        for (i, module) in definition.0.modules.iter().enumerate() {
            if module == "internal"
                && (context.package != definition.0.package
                    || !context.modules.starts_with(&definition.0.modules[..i]))
            {
                return Err(LookupError::Invisible(definition.clone()));
            }
        }
        Ok(definition.clone())
    }
}

// Name tokens give precise locations even when an item begins with attributes.
// Hand-built trees without a token tape retain their supplied item origin.
