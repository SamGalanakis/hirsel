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
    /// Covers the entire manifest, including dependency membership and visibility.
    pub revision: ContentHash,
}

#[derive(Clone, Debug)]
pub struct ManifestIndex {
    pub package: PackageId,
    pub revision: ContentHash,
    entries: Arc<DefMap<DefinitionEntry>>,
    heads: Arc<BTreeMap<String, BTreeSet<(PackageId, Vec<String>)>>>,
    tables: Arc<Tables>,
    /// Each exposed head's targets, in `heads` order: a candidate set, never
    /// one answer, so ambiguity and the core/std merge stay decidable.
    head_targets: Arc<HashMap<Symbol, HeadTargets, IdHasher>>,
}

/// One node per (package, module path) some definition lives under.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
struct ModuleId(u32);

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
    targets: Vec<(PackageId, Vec<String>, Option<ModuleId>)>,
    /// Only identical core/std module paths share a namespace. Two std majors
    /// remain ambiguous even if only one declares the member.
    merged: bool,
}

/// Where one file's unqualified paths start: its package root and its module.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModuleScope {
    root: Option<ModuleId>,
    module: Option<ModuleId>,
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
        from: Option<ModuleId>,
        names: impl IntoIterator<Item = Option<Symbol>>,
    ) -> Option<ModuleId> {
        names.into_iter().try_fold(from?, |module, name| {
            self.children.get(&(module, name?)).copied()
        })
    }
    fn module(&self, package: &PackageId, modules: &[String]) -> Option<ModuleId> {
        self.walk(
            self.roots.get(package).copied(),
            modules.iter().map(|m| Symbol::existing(m)),
        )
    }
    fn member(
        &self,
        from: Option<ModuleId>,
        modules: &[Option<Symbol>],
        name: Option<Symbol>,
    ) -> Option<DefId> {
        let module = self.walk(from, modules.iter().copied())?;
        self.names.get(&(module, name?)).copied()
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

#[cfg(test)]
mod package_view_tests {
    use super::*;

    #[test]
    fn package_views_share_rows_and_keep_lookup_package_scoped() {
        let package = |name: &str| PackageId {
            name: name.into(),
            version: "1".into(),
        };
        let left = package("left");
        let right = package("right");
        let definition = |package, name: &str| {
            DefPath(ItemPath {
                package,
                modules: vec![],
                name: name.into(),
            })
        };
        let entries = [(left.clone(), "left_value"), (right.clone(), "right_value")].map(
            |(package, name)| DefinitionEntry {
                definition: definition(package, name),
                namespace: Namespace::Value,
                visibility: Visibility::Public,
                span: Span::default(),
            },
        );
        let left_definition = entries[0].definition.clone();
        let (index, diagnostics) = ManifestIndex::new(ManifestFixture {
            package: left,
            definitions: entries.into(),
            exposures: vec![],
            revision: ContentHash::default(),
        });
        assert!(diagnostics.is_empty());
        let view = index.for_package(right);
        for (selected, own, foreign) in [
            (&index, "left_value", "right_value"),
            (&view, "right_value", "left_value"),
        ] {
            let context = FileContext {
                package: selected.package.clone(),
                modules: vec![],
            };
            assert!(
                selected
                    .lookup_spelling(&context, None, &[own.into()])
                    .is_ok()
            );
            assert_eq!(
                selected.lookup_spelling(&context, None, &[foreign.into()]),
                Err(LookupError::Missing)
            );
        }
        assert!(Arc::ptr_eq(&index.entries, &view.entries));
        assert_eq!(index.entry(&left_definition), view.entry(&left_definition));
    }
}

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
        for entry in fixture.definitions {
            let path = &entry.definition.0;
            let segment = |name: &str| !name.is_empty() && !name.contains(['/', '.', '@']);
            if !segment(&path.package.name)
                || path.package.version.is_empty()
                || !segment(&path.name)
                || !path.modules.iter().all(|m| segment(m))
            {
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
        let head_targets = tables.heads(&heads);
        (
            Self {
                package: fixture.package,
                revision: fixture.revision,
                entries: Arc::new(entries),
                heads: Arc::new(heads),
                tables: Arc::new(tables),
                head_targets: Arc::new(head_targets),
            },
            diagnostics,
        )
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
            head_targets: Arc::new(self.tables.heads(&heads)),
            heads: Arc::new(heads),
            ..self.clone()
        }
    }

    /// Type names available without a module head, including core defaults.
    /// The caller records the manifest lookup that led to this candidate search.
    pub fn unqualified_types(&self, context: &FileContext) -> Vec<DefPath> {
        self.entries
            .values()
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
        self.entries.get(&DefId::get(definition)?)
    }

    /// Where `context`'s unqualified paths start; computed once per file context.
    pub fn scope(&self, context: &FileContext) -> ModuleScope {
        let root = self.tables.roots.get(&context.package).copied();
        ModuleScope {
            root,
            module: self
                .tables
                .walk(root, context.modules.iter().map(|m| Symbol::existing(m))),
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
                .any(|module| self.tables.children.contains_key(&(module, head)))
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
        for definition in self.entries.keys().map(|id| &id.path().0) {
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
        let tables = &*self.tables;
        if let Some(package) = package {
            return match tables.member(tables.roots.get(package).copied(), modules, name) {
                Some(id) => self.access_id(context, id),
                None => Err(LookupError::Missing),
            };
        }
        if let Some(id) = tables
            .member(scope.module, modules, name)
            .or_else(|| tables.member(scope.root, modules, name))
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
            if let Some(id) = tables.member(module, members, name) {
                return self.access_id(context, id);
            }
        }
        Err(LookupError::MissingMember(candidates()))
    }

    fn access_id(&self, context: &FileContext, id: DefId) -> Result<DefPath, LookupError> {
        self.access_entry(context, id.path(), self.entries.get(&id))
    }

    pub fn access(
        &self,
        context: &FileContext,
        definition: &DefPath,
    ) -> Result<DefPath, LookupError> {
        self.access_entry(
            context,
            definition,
            DefId::get(definition).and_then(|id| self.entries.get(&id)),
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
