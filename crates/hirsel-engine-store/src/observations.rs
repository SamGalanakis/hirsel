//! What a published installation's work used, by part.
//!
//! The installation recorder logs every row and content use with the traced
//! work running when it was made. The trace's edges say which work each part
//! of the product reaches, so a part's uses are those of everything it
//! reaches, whichever reader touched a shared row first.
use hirsel_intern::attribution::{EdgeSet, FxBuild, Node};
use hirsel_syntax::ContentHash;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// Store-side work id of a use made on a thread no trace reached.
const UNTRACED: u32 = 0;

/// Every row and content use, by interned work.
#[derive(Debug, Default)]
pub struct Uses {
    work: HashMap<Node, u32, FxBuild>,
    rows: Vec<(u32, ContentHash)>,
    contents: Vec<(u32, ContentHash)>,
}

impl Uses {
    fn by_running(&mut self) -> u32 {
        if !hirsel_intern::attribution::tracing() {
            return UNTRACED;
        }
        let node =
            hirsel_intern::attribution::running().unwrap_or(hirsel_intern::attribution::ROOT);
        let next = u32::try_from(self.work.len() + 1).unwrap_or(u32::MAX);
        *self.work.entry(node).or_insert(next)
    }
    pub(crate) fn row(&mut self, address: ContentHash) {
        let work = self.by_running();
        self.rows.push((work, address));
    }
    pub(crate) fn content(&mut self, hash: ContentHash) {
        let work = self.by_running();
        self.contents.push((work, hash));
    }
}

/// Rows and contents used.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Used {
    pub rows: BTreeSet<ContentHash>,
    pub contents: BTreeSet<ContentHash>,
}

impl Used {
    fn add(&mut self, row: bool, item: ContentHash) {
        if row {
            self.rows.insert(item);
        } else {
            self.contents.insert(item);
        }
    }
}

/// A published installation's uses, by part.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Observations {
    /// Each part's uses: everything the part's work reaches.
    pub parts: BTreeMap<[u8; 32], Used>,
    /// Uses by work no part reaches: the product's package-level rows.
    pub package: Used,
    /// Uses on threads the trace did not reach. Empty when recording covered
    /// every path that publishes.
    pub untraced: Used,
}

impl Observations {
    pub fn of(edges: &EdgeSet, uses: &Uses) -> Self {
        let mut ids: HashMap<Node, u32, FxBuild> = edges.work.clone();
        let mut intern = |node: Node| {
            let next = u32::try_from(ids.len()).unwrap_or(u32::MAX);
            *ids.entry(node).or_insert(next)
        };
        let store: HashMap<u32, u32> = uses
            .work
            .iter()
            .map(|(node, id)| (*id, intern(*node)))
            .collect();
        let mut by_work: HashMap<u32, Vec<(bool, ContentHash)>> = HashMap::new();
        let mut observations = Self::default();
        let tagged = uses
            .rows
            .iter()
            .map(|(work, item)| (true, *work, *item))
            .chain(
                uses.contents
                    .iter()
                    .map(|(work, item)| (false, *work, *item)),
            );
        for (row, work, item) in tagged {
            match store.get(&work) {
                Some(id) => by_work.entry(*id).or_default().push((row, item)),
                None => observations.untraced.add(row, item),
            }
        }
        let mut successors: HashMap<u32, Vec<u32>> = HashMap::new();
        for &(reader, dependency) in &edges.edges {
            successors.entry(reader).or_default().push(dependency);
        }
        let mut reached_by_any = HashSet::new();
        for (node, &part) in ids
            .iter()
            .filter(|(node, _)| node.0 == hirsel_intern::attribution::PART)
        {
            let mut used = Used::default();
            let mut seen = HashSet::from([part]);
            let mut stack = vec![part];
            while let Some(work) = stack.pop() {
                for &(row, item) in by_work.get(&work).into_iter().flatten() {
                    used.add(row, item);
                }
                for &next in successors.get(&work).into_iter().flatten() {
                    if seen.insert(next) {
                        stack.push(next);
                    }
                }
            }
            reached_by_any.extend(seen);
            observations.parts.insert(node.1, used);
        }
        for (work, items) in &by_work {
            if !reached_by_any.contains(work) {
                for &(row, item) in items {
                    observations.package.add(row, item);
                }
            }
        }
        observations
    }
}
