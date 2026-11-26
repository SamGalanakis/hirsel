//! Package inspection over the check product and the native reach walk.
use crate::cli::{CompilerOption as Opt, Request};
use crate::{CheckProduct, reuse};
use hirsel_core_ids::{DefPath, PackageId};
use hirsel_core_resolve::{DefinitionId, DefinitionKind};
use hirsel_core_syntax::{Item, NodeKind, TokenKind};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::io;

enum Query {
    Interface,
    Foreign(DefPath, crate::foreign_header::Print),
    Dependents(String),
    AffectedTests(String),
    Unused,
}

pub(crate) struct Inspection {
    query: Query,
    transitive: bool,
    library: bool,
    module: Option<String>,
    filter: Option<String>,
    pub(crate) source_library: Option<&'static str>,
}

impl Inspection {
    pub(crate) fn read(invocation: &mut Request<Opt>) -> Result<Self, String> {
        use Opt::*;
        let mut queries = Vec::new();
        let mut transitive = false;
        let mut library = false;
        let mut module = None;
        let mut filter = None;
        let mut source_library = None;
        for option in invocation.options.extract_if(.., |option| {
            matches!(
                option,
                Interface
                    | ForeignHeader(..)
                    | ForeignAbi(..)
                    | Dependents(..)
                    | AffectedTests(..)
                    | Unused
                    | Transitive
                    | InspectLibrary
                    | InspectStd
                    | InspectCore
                    | InspectModule(..)
                    | InspectFilter(..)
            )
        }) {
            match option {
                Interface => queries.push(Query::Interface),
                ForeignHeader(ref value) | ForeignAbi(ref value) => {
                    let path = DefPath::from_full(value)
                        .ok_or_else(|| format!("invalid foreign module path {value:?}"))?;
                    let print = if matches!(option, ForeignAbi(_)) {
                        crate::foreign_header::Print::Abi
                    } else {
                        crate::foreign_header::Print::Header
                    };
                    queries.push(Query::Foreign(path, print));
                }
                Dependents(value) => queries.push(Query::Dependents(value)),
                AffectedTests(value) => queries.push(Query::AffectedTests(value)),
                Unused => queries.push(Query::Unused),
                Transitive => transitive = true,
                InspectLibrary => library = true,
                InspectStd | InspectCore => {
                    let name = if option == InspectStd { "std" } else { "core" };
                    if source_library.replace(name).is_some() {
                        return Err("inspect takes one library selector: --core or --std".into());
                    }
                }
                InspectModule(value) => {
                    if module.replace(value).is_some() {
                        return Err("inspect takes at most one --module".into());
                    }
                }
                InspectFilter(value) => {
                    if filter.replace(value).is_some() {
                        return Err("inspect takes at most one --filter".into());
                    }
                }
                _ => unreachable!("only inspect options are extracted"),
            }
        }
        let query = match queries.len() {
            0 => Query::Interface,
            1 => queries.pop().unwrap(),
            _ => return Err("inspect takes at most one of --interface, --foreign-header, --foreign-abi, --dependents, --affected-tests or --unused".into()),
        };
        let json = invocation.options.iter().any(|id| *id == Json);
        if json && !matches!(query, Query::Interface) {
            return Err("inspect --json requires --interface".into());
        }
        if module.is_some() && !matches!(query, Query::Interface) {
            return Err("inspect --module requires --interface".into());
        }
        if filter.is_some() && !matches!(query, Query::Interface) {
            return Err("inspect --filter requires --interface".into());
        }
        if transitive && !matches!(query, Query::Dependents(_)) {
            return Err("--transitive requires --dependents".into());
        }
        if library && !matches!(query, Query::Unused) {
            return Err("--library requires --unused".into());
        }
        if let Some(name) = source_library
            && invocation.positional.is_some()
        {
            return Err(format!(
                "inspect --{name} selects its sources through --libraries"
            ));
        }
        if source_library.is_some() && matches!(query, Query::Foreign(..)) {
            return Err(
                "inspect --foreign-header and --foreign-abi require a package with a foreign manifest".into(),
            );
        }
        if source_library.is_none()
            && !matches!(query, Query::Foreign(..))
            && invocation.positional.is_none()
        {
            return Err(
                "inspect requires a package directory, archive, dependency name, --core or --std"
                    .into(),
            );
        }
        Ok(Self {
            query,
            transitive,
            library,
            module,
            filter,
            source_library,
        })
    }

    pub(crate) fn includes_tests(&self) -> bool {
        !matches!(self.query, Query::Interface | Query::Foreign(..))
    }

    pub(crate) fn foreign_module(&self) -> Option<(&DefPath, crate::foreign_header::Print)> {
        match &self.query {
            Query::Foreign(path, print) => Some((path, *print)),
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
            let package = entry.map_or_else(
                || crate::package(self.source_library.unwrap_or("std")),
                |path| path.0.package.clone(),
            );
            if matches!(self.query, Query::Interface) {
                return interface(
                    product,
                    package,
                    self.module.as_deref(),
                    self.filter.as_deref(),
                )?
                .write(json);
            }
            let graph = Definitions::new(product, package);
            let paths = match self.query {
                Query::Dependents(name) => {
                    let definition = graph.resolve(&name)?;
                    graph.dependents(definition, self.transitive)
                }
                Query::AffectedTests(name) => graph.affected_tests(&name)?,
                Query::Unused => graph.unused(entry, self.library),
                Query::Interface | Query::Foreign(..) => unreachable!(),
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
    #[serde(skip_serializing_if = "Option::is_none")]
    doc: Option<String>,
}

impl Interface<'_> {
    fn write(self, json: bool) -> io::Result<()> {
        if json {
            println!("{}", serde_json::to_string_pretty(&self)?);
        } else {
            for module in self.modules {
                for item in module.items {
                    println!("{}\t{}", item.definition, item.signature);
                    if let Some(doc) = item.doc {
                        for line in doc.lines() {
                            if line.is_empty() {
                                println!("  ///");
                            } else {
                                println!("  /// {line}");
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

fn module_matches(path: &[String], requested: &[&str], package_name: &str) -> bool {
    let matches_prefix = |target: &[&str]| {
        !target.is_empty()
            && path.len() >= target.len()
            && path
                .iter()
                .map(String::as_str)
                .zip(target)
                .all(|(a, b)| a == *b)
    };
    if matches_prefix(requested) {
        return true;
    }
    if requested.len() > 1 && requested[0] == package_name {
        return matches_prefix(&requested[1..]);
    }
    false
}

fn declaration_matches_filter(item: &Declaration<'_>, needle: &str) -> bool {
    item.definition.to_ascii_lowercase().contains(needle)
        || item.name.to_ascii_lowercase().contains(needle)
        || item.signature.to_ascii_lowercase().contains(needle)
        || item
            .doc
            .as_deref()
            .is_some_and(|doc| doc.to_ascii_lowercase().contains(needle))
}

fn interface<'a>(
    product: &'a CheckProduct,
    package: PackageId,
    module_filter: Option<&str>,
    text_filter: Option<&str>,
) -> io::Result<Interface<'a>> {
    let requested_module: Option<Vec<&str>> = module_filter.map(|value| {
        value
            .split(['/', '.'])
            .filter(|segment| !segment.is_empty())
            .collect()
    });
    if let Some(segments) = &requested_module
        && segments.is_empty()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "inspect --module requires a non-empty module name",
        ));
    }
    let mut modules = BTreeMap::new();
    for file in &product.files {
        let path = product.index.module_path(file.module);
        if path.package != package {
            continue;
        }
        if let Some(requested) = &requested_module
            && !module_matches(&path.modules, requested, &package.name)
        {
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
                doc: doc_comment(item),
            });
        }
    }
    if let Some(requested) = module_filter
        && modules.is_empty()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "unknown module `{requested}` in {}@{}",
                package.name, package.version
            ),
        ));
    }
    let mut modules: Vec<_> = modules.into_values().collect();
    if let Some(filter) = text_filter {
        let needle = filter.to_ascii_lowercase();
        for module in &mut modules {
            module
                .items
                .retain(|item| declaration_matches_filter(item, &needle));
        }
        modules.retain(|module| !module.items.is_empty());
    }
    Ok(Interface {
        version: 1,
        package,
        modules,
    })
}

pub(crate) fn doc_comment(item: Item<'_>) -> Option<String> {
    let leading: Vec<_> = item
        .range
        .tokens
        .clone()
        .map(|index| item.tree.tokens[index as usize])
        .take_while(|token| token.span.end <= item.file_offset)
        .collect();
    let mut lines = Vec::new();
    let mut next_start = item.file_offset as usize;
    for token in leading.into_iter().rev() {
        if token.kind != TokenKind::DocLine {
            break;
        }
        let gap = &item.text[token.span.end as usize..next_start];
        if gap.bytes().filter(|&b| b == b'\n').count() > 1 {
            break;
        }
        let raw = &item.text[token.span.start as usize..token.span.end as usize];
        let body = raw.strip_prefix("///").unwrap_or(raw);
        let body = body.strip_prefix(' ').unwrap_or(body);
        lines.push(body);
        next_start = token.span.start as usize;
    }
    if lines.is_empty() {
        return None;
    }
    lines.reverse();
    Some(lines.join("\n"))
}

pub(crate) fn signature(item: Item<'_>, kind: DefinitionKind, opaque: bool) -> &str {
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
        let public = children.find(|n| n.kind == NodeKind::Public);
        let end = children
            .filter(|n| matches!(n.kind, NodeKind::Generics | NodeKind::Bounds))
            .last()
            .map_or_else(
                || item.token_span(root.token.unwrap()).end,
                |n| n.origin.span.end,
            );
        (public.map_or(span.start, |n| n.origin.span.start), end)
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

    fn affected_tests(&self, name: &str) -> io::Result<Vec<DefPath>> {
        let definitions: BTreeSet<_> = match name.split_once('@') {
            Some((package, version)) if !version.contains('/') => {
                let identity = PackageId {
                    name: package.to_owned(),
                    version: version.to_owned(),
                };
                let bound = self
                    .product
                    .index
                    .symbols
                    .find(package)
                    .zip(self.product.index.symbols.find(version))
                    .and_then(|(name, version)| self.product.index.package(name, version));
                if bound.is_none() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("unknown package: {name}"),
                    ));
                }
                self.names
                    .iter()
                    .filter_map(|(path, definition)| {
                        (path.0.package == identity).then_some(*definition)
                    })
                    .collect()
            }
            _ => BTreeSet::from([self.resolve(name)?]),
        };
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
            if !covered.is_disjoint(&definitions) {
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
