//! Package inspection over the check product and the native reach walk.
use crate::cli::{Invocation, OptionId};
use crate::{CheckProduct, reuse};
use hirsel_core_ids::{DefPath, PackageId};
use hirsel_core_resolve::{DefinitionId, DefinitionKind};
use hirsel_core_syntax::{Item, NodeKind};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::io;

enum Query {
    Interface,
    ForeignHeader(DefPath),
    Dependents(String),
    AffectedTests(String),
    Unused,
}

pub(crate) struct Inspection {
    query: Query,
    transitive: bool,
    library: bool,
    pub(crate) std: bool,
}

impl Inspection {
    pub(crate) fn read(invocation: &mut Invocation) -> Result<Self, String> {
        use OptionId::*;
        let mut queries = Vec::new();
        let mut transitive = false;
        let mut library = false;
        let mut std = false;
        for (option, value) in invocation.options.extract_if(.., |(option, _)| {
            matches!(
                option,
                Interface
                    | ForeignHeader
                    | Dependents
                    | AffectedTests
                    | Unused
                    | Transitive
                    | InspectLibrary
                    | InspectStd
            )
        }) {
            match option {
                Interface => queries.push(Query::Interface),
                ForeignHeader => {
                    let value = value.unwrap();
                    let path = DefPath::from_full(&value)
                        .ok_or_else(|| format!("invalid foreign module path {value:?}"))?;
                    queries.push(Query::ForeignHeader(path));
                }
                Dependents => queries.push(Query::Dependents(value.unwrap())),
                AffectedTests => queries.push(Query::AffectedTests(value.unwrap())),
                Unused => queries.push(Query::Unused),
                Transitive => transitive = true,
                InspectLibrary => library = true,
                InspectStd => std = true,
                _ => unreachable!("only inspect options are extracted"),
            }
        }
        let [query]: [Query; 1] = queries.try_into().map_err(|_| {
            "inspect requires exactly one of --interface, --foreign-header, --dependents, --affected-tests or --unused"
        })?;
        let json = invocation.options.iter().any(|(id, _)| *id == Json);
        if json && !matches!(query, Query::Interface) {
            return Err("inspect --json requires --interface".into());
        }
        if transitive && !matches!(query, Query::Dependents(_)) {
            return Err("--transitive requires --dependents".into());
        }
        if library && !matches!(query, Query::Unused) {
            return Err("--library requires --unused".into());
        }
        if std && invocation.positional.is_some() {
            return Err("inspect --std selects its sources through --libraries".into());
        }
        if std && matches!(query, Query::ForeignHeader(_)) {
            return Err(
                "inspect --foreign-header requires a package with a foreign manifest".into(),
            );
        }
        if !std && !matches!(query, Query::ForeignHeader(_)) && invocation.positional.is_none() {
            return Err("inspect requires a package directory or --std".into());
        }
        Ok(Self {
            query,
            transitive,
            library,
            std,
        })
    }

    pub(crate) fn includes_tests(&self) -> bool {
        !matches!(self.query, Query::Interface | Query::ForeignHeader(_))
    }

    pub(crate) fn foreign_module(&self) -> Option<&DefPath> {
        match &self.query {
            Query::ForeignHeader(path) => Some(path),
            _ => None,
        }
    }

    pub(crate) fn write(
        self,
        product: &CheckProduct,
        entry: Option<&DefPath>,
        json: bool,
    ) -> io::Result<()> {
        hirsel_core_ids::published::catch_io(|| {
            let package =
                entry.map_or_else(|| crate::package("std"), |path| path.0.package.clone());
            if matches!(self.query, Query::Interface) {
                return interface(product, package).write(json);
            }
            let graph = Definitions::new(product, package);
            let paths = match self.query {
                Query::Dependents(name) => {
                    let definition = graph.resolve(&name)?;
                    graph.dependents(definition, self.transitive)
                }
                Query::AffectedTests(name) => {
                    let definition = graph.resolve(&name)?;
                    graph.affected_tests(definition)?
                }
                Query::Unused => graph.unused(entry, self.library),
                Query::Interface | Query::ForeignHeader(_) => unreachable!(),
            };
            for path in paths {
                println!("{path}");
            }
            Ok(())
        })
    }
}

#[derive(Serialize)]
struct Interface<'a> {
    version: u32,
    package: PackageId,
    modules: Vec<Module<'a>>,
}

#[derive(Serialize)]
struct Module<'a> {
    path: Vec<String>,
    extends_core: bool,
    items: Vec<Declaration<'a>>,
}

#[derive(Serialize)]
struct Declaration<'a> {
    definition: String,
    name: String,
    kind: &'static str,
    signature: &'a str,
}

impl Interface<'_> {
    fn write(self, json: bool) -> io::Result<()> {
        if json {
            println!("{}", serde_json::to_string_pretty(&self)?);
        } else {
            for module in self.modules {
                for item in module.items {
                    println!("{}\t{}", item.definition, item.signature);
                }
            }
        }
        Ok(())
    }
}

fn interface(product: &CheckProduct, package: PackageId) -> Interface<'_> {
    let mut modules = BTreeMap::new();
    for file in &product.files {
        let path = product.index.module_path(file.module);
        if path.package != package {
            continue;
        }
        let module = modules
            .entry(path.modules.clone())
            .or_insert_with(|| Module {
                extends_core: package.name_is("std")
                    && product
                        .index
                        .module_at(&crate::package("core"), &path.modules)
                        .is_some(),
                path: path.modules,
                items: Vec::new(),
            });
        for (ordinal, item) in file.syntax.items().enumerate() {
            let Some(id) =
                product
                    .index
                    .item_definition(file.module, file.file, ordinal as u32, item)
            else {
                continue;
            };
            let definition = product.index.get(id);
            if !definition.public {
                continue;
            }
            let kind = match definition.kind {
                DefinitionKind::Function => "function",
                DefinitionKind::Constant => "constant",
                DefinitionKind::Type => "type",
                DefinitionKind::Clone => continue,
            };
            let path = product.index.definition_path(id);
            module.items.push(Declaration {
                definition: path.to_string(),
                name: path.0.name,
                kind,
                signature: signature(item, definition.kind, definition.opaque),
            });
        }
    }
    Interface {
        version: 1,
        package,
        modules: modules.into_values().collect(),
    }
}

fn signature(item: Item<'_>, kind: DefinitionKind, opaque: bool) -> &str {
    let root = item.tree.node(item.root);
    let mut children = item
        .tree
        .children(item.root)
        .iter()
        .map(|&id| item.tree.node(id));
    let span = root.origin.span;
    let (start, end) = if opaque {
        // The name, generics and public bounds describe the nominal type.
        // Fields, variants, aliases and owner codec functions are private.
        let public = children.find(|n| n.kind == NodeKind::Public).unwrap();
        let end = children
            .filter(|n| matches!(n.kind, NodeKind::Generics | NodeKind::Bounds))
            .last()
            .map_or_else(
                || item.token_span(root.token.unwrap()).end,
                |n| n.origin.span.end,
            );
        (public.origin.span.start, end)
    } else {
        let end = match kind {
            DefinitionKind::Constant => {
                // The constant's declared type is its interface. The expression
                // can refer to private definitions.
                children
                    .find(|n| !matches!(n.kind, NodeKind::Attribute | NodeKind::Public))
                    .map_or(span.end, |n| n.origin.span.end)
            }
            _ => children
                .find(|n| matches!(n.kind, NodeKind::Block | NodeKind::Drop))
                .map_or(span.end, |n| n.origin.span.start),
        };
        (span.start, end)
    };
    let start = (item.file_offset + start) as usize;
    let end = (item.file_offset + end) as usize;
    item.text[start..end].trim_end()
}

struct Definitions<'a> {
    product: &'a CheckProduct,
    package: PackageId,
    names: BTreeMap<DefPath, DefinitionId>,
}

impl<'a> Definitions<'a> {
    fn new(product: &'a CheckProduct, package: PackageId) -> Self {
        let names = product
            .index
            .definitions()
            .map(|(id, _)| (product.index.definition_path(id), id))
            .collect();
        Self {
            product,
            package,
            names,
        }
    }

    fn resolve(&self, name: &str) -> io::Result<DefinitionId> {
        let matches = |path: &&DefPath| {
            if name.contains('@') {
                return path.to_string() == name;
            }
            path.0.package == self.package
                && (!name.contains('/') && path.0.name == name
                    || path
                        .0
                        .modules
                        .iter()
                        .chain([&path.0.name])
                        .map(String::as_str)
                        .eq(name.split('/')))
        };
        let mut found = self.names.keys().filter(matches);
        let path = found.next().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unknown definition: {name}"),
            )
        })?;
        if found.next().is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("ambiguous definition: {name}; use its full path"),
            ));
        }
        Ok(self.names[path])
    }

    fn reads(&self, id: DefinitionId) -> impl Iterator<Item = DefinitionId> + '_ {
        self.product
            .headers
            .row(id)
            .into_iter()
            .flat_map(|row| row.reads.iter().chain(&row.body_reads).copied())
    }

    fn own_paths(&self, mut selected: impl FnMut(DefinitionId) -> bool) -> Vec<DefPath> {
        self.names
            .iter()
            .filter(|(path, id)| path.0.package == self.package && selected(**id))
            .map(|(path, _)| path.clone())
            .collect()
    }

    fn dependents(&self, definition: DefinitionId, transitive: bool) -> Vec<DefPath> {
        let mut users = BTreeMap::<_, Vec<_>>::new();
        for &user in self.names.values() {
            for read in self.reads(user) {
                users.entry(read).or_default().push(user);
            }
        }
        let direct = |id| users.get(&id).into_iter().flatten().copied();
        let users: BTreeSet<_> = if transitive {
            walk(direct(definition), direct)
        } else {
            direct(definition).collect()
        };
        self.own_paths(|id| id != definition && users.contains(&id))
    }

    fn unused(&self, entry: Option<&DefPath>, library: bool) -> Vec<DefPath> {
        let main = entry
            .and_then(|path| self.names.get(path))
            .copied()
            .filter(|&id| self.product.index.get(id).kind == DefinitionKind::Function);
        let mut roots = Vec::new();
        if library || main.is_none() {
            roots.extend(self.names.iter().filter_map(|(path, &id)| {
                (path.0.package == self.package && self.product.index.get(id).public).then_some(id)
            }));
        } else {
            roots.extend(main);
            roots.extend(
                self.product
                    .tests()
                    .filter(|path| path.0.package == self.package)
                    .filter_map(|path| self.names.get(path).copied()),
            );
        }
        let used = walk(roots, |id| self.reads(id));
        self.own_paths(|id| !used.contains(&id))
    }

    fn affected_tests(&self, definition: DefinitionId) -> io::Result<Vec<DefPath>> {
        let (implicit, siblings) = reuse::implicit_reads(&self.product.index);
        let mut affected = Vec::new();
        for test in self
            .product
            .tests()
            .filter(|test| test.0.package == self.package)
        {
            if crate::test_run::parameterless_test(&self.product.test_entries[test]).is_none() {
                continue;
            }
            let reached = self
                .product
                .reachable(test, crate::native::DEFAULT_INSTANCE_LIMIT)?;
            let instances = reached.require().map_err(io::Error::other)?;
            // The address hashes the reached code. That code can also depend on
            // declaration reads, including constants that lowering folded away.
            let roots = instances
                .iter()
                .filter_map(|instance| self.names.get(&instance.definition).copied())
                .chain(implicit.iter().copied());
            let covered = walk(roots, |id| {
                self.reads(id)
                    .chain(siblings.get(&id).into_iter().flatten().copied())
            });
            if covered.contains(&definition) {
                affected.push(test.clone());
            }
        }
        Ok(affected)
    }
}

fn walk<I: IntoIterator<Item = DefinitionId>>(
    roots: impl IntoIterator<Item = DefinitionId>,
    mut edges: impl FnMut(DefinitionId) -> I,
) -> BTreeSet<DefinitionId> {
    let mut pending: Vec<_> = roots.into_iter().collect();
    let mut visited = BTreeSet::new();
    while let Some(id) = pending.pop() {
        if visited.insert(id) {
            pending.extend(edges(id));
        }
    }
    visited
}
