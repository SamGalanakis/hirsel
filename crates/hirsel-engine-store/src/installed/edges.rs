//! The image's signature and representation edges (E): per installed name,
//! the names its checked definitions read, from each semantic input's
//! signature `dependencies` and `semantic_dependencies`. The checker records
//! those reads whether or not a call survives lowering, so a request's
//! closure over E reaches every file a header or representation probe needs.
//! A specialization's edges are its generic definition's, and every path is
//! its generic base: reach adopts files, not instances.
use hirsel_syntax::DefPath;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ImageEdges {
    /// Every source and target, sorted and unique.
    paths: Vec<DefPath>,
    /// (source, targets), sorted by source; targets sorted, unique, and never
    /// the source itself.
    edges: Vec<(u32, Vec<u32>)>,
}

/// The generic definition `path` instantiates, or `path` itself.
pub fn base(path: &DefPath) -> DefPath {
    hirsel_types::specialization_parts(path).map_or_else(|| path.clone(), |(base, _)| base)
}

impl ImageEdges {
    pub(super) fn new(edges: &BTreeMap<DefPath, BTreeSet<DefPath>>) -> Self {
        let paths: Vec<DefPath> = edges
            .iter()
            .flat_map(|(source, targets)| std::iter::once(source).chain(targets))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .cloned()
            .collect();
        let at = |path: &DefPath| paths.binary_search(path).unwrap() as u32;
        let edges = edges
            .iter()
            .filter(|(_, targets)| !targets.is_empty())
            .map(|(source, targets)| (at(source), targets.iter().map(at).collect()))
            .collect();
        Self { paths, edges }
    }

    /// Structure open relies on: sorted unique paths, sources in order, and
    /// sorted in-range targets that never repeat their source.
    pub(super) fn well_formed(&self) -> bool {
        let n = self.paths.len() as u32;
        self.paths.windows(2).all(|pair| pair[0] < pair[1])
            && self.edges.windows(2).all(|pair| pair[0].0 < pair[1].0)
            && self.edges.iter().all(|(source, targets)| {
                *source < n
                    && !targets.is_empty()
                    && targets.windows(2).all(|pair| pair[0] < pair[1])
                    && targets.iter().all(|t| *t < n && t != source)
            })
    }

    /// The names `source`'s checked definitions read.
    pub fn targets(&self, source: &DefPath) -> impl Iterator<Item = &DefPath> {
        let targets = self
            .paths
            .binary_search(source)
            .ok()
            .and_then(|at| {
                self.edges
                    .binary_search_by_key(&(at as u32), |(source, _)| *source)
                    .ok()
            })
            .map_or(&[][..], |at| &self.edges[at].1[..]);
        targets.iter().map(|t| &self.paths[*t as usize])
    }

    /// Every source, so construction can check each is a name.
    pub(super) fn sources(&self) -> impl Iterator<Item = &DefPath> {
        self.edges
            .iter()
            .map(|(source, _)| &self.paths[*source as usize])
    }

    pub fn is_empty(&self) -> bool {
        self.edges.is_empty()
    }
}
