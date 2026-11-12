//! The image's name index (N): the installed half of the front end's
//! (module, symbol) scope tables. Module nodes come in creation order, each
//! after its parent; node 0 is the package root. Names are sorted by
//! (module, symbol), one per definition path. Every name records the source
//! whose parse heads declare it, so a request adopts only the files it
//! reaches while lookups, absence and shadowing read the whole table.
use hirsel_syntax::{DefPath, ItemPath, PackageId, Visibility};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ImageNames {
    /// Node `i + 1`: (parent node, symbol).
    modules: Vec<(u32, String)>,
    names: Vec<ImageName>,
}

/// What a lookup needs without the definition's head: its namespace,
/// visibility and the one fact a whole-package scan reads.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NameFacts {
    pub types: bool,
    pub visibility: Visibility,
    /// Index into the header's sources.
    pub source: u32,
    pub instance_bodied: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct ImageName {
    module: u32,
    symbol: String,
    facts: NameFacts,
}

impl ImageNames {
    /// The table of `package`'s `names`; every path must be in `package`.
    pub fn new(package: &PackageId, names: &BTreeMap<DefPath, NameFacts>) -> Option<Self> {
        let mut nodes = BTreeMap::<(u32, &str), u32>::new();
        let mut modules = Vec::new();
        let mut rows = Vec::with_capacity(names.len());
        for (path, facts) in names {
            if path.0.package != *package {
                return None;
            }
            let mut module = 0;
            for symbol in &path.0.modules {
                module = *nodes.entry((module, symbol)).or_insert_with(|| {
                    modules.push((module, symbol.clone()));
                    modules.len() as u32
                });
            }
            rows.push(ImageName {
                module,
                symbol: path.0.name.clone(),
                facts: *facts,
            });
        }
        rows.sort_by(|a, b| (a.module, &a.symbol).cmp(&(b.module, &b.symbol)));
        Some(Self {
            modules,
            names: rows,
        })
    }

    /// Structure open relies on: parents precede children, no module or name
    /// repeats, and every source is one of the header's `sources`.
    pub(super) fn well_formed(&self, sources: usize) -> bool {
        let mut seen = std::collections::BTreeSet::new();
        self.modules
            .iter()
            .enumerate()
            .all(|(i, (parent, symbol))| *parent as usize <= i && seen.insert((*parent, symbol)))
            && self
                .names
                .windows(2)
                .all(|pair| (pair[0].module, &pair[0].symbol) < (pair[1].module, &pair[1].symbol))
            && self.names.iter().all(|name| {
                name.module as usize <= self.modules.len() && (name.facts.source as usize) < sources
            })
    }

    /// Every name as a path of `package`, in table order.
    pub fn paths<'a>(
        &'a self,
        package: &'a PackageId,
    ) -> impl Iterator<Item = (DefPath, NameFacts)> + 'a {
        let mut prefixes: Vec<Vec<String>> = vec![vec![]];
        for (parent, symbol) in &self.modules {
            let mut prefix = prefixes[*parent as usize].clone();
            prefix.push(symbol.clone());
            prefixes.push(prefix);
        }
        self.names.iter().map(move |name| {
            (
                DefPath(ItemPath {
                    package: package.clone(),
                    modules: prefixes[name.module as usize].clone(),
                    name: name.symbol.clone(),
                }),
                name.facts,
            )
        })
    }

    pub fn len(&self) -> usize {
        self.names.len()
    }
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }
}
