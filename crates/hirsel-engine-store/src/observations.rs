//! What a published installation's work used, by part.
//!
//! The installation recorder logs every row and content use with the traced
//! work running when it was made. The trace's edges say which work each part
//! of the product reaches, so a part's uses are those of everything it
//! reaches, whichever reader touched a shared row first.
use hirsel_intern::attribution::{EdgeSet, FxBuild, Node};
use hirsel_syntax::ContentHash;
use std::collections::{BTreeMap, BTreeSet, HashMap};

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
    /// The distinct `found` items, built in key order (a bulk build, not a
    /// search per insert).
    fn of(items: &[(bool, ContentHash)], rank: &[u32], found: &mut [u32]) -> Self {
        found.sort_unstable_by_key(|&item| rank[item as usize]);
        let (rows, contents): (Vec<_>, Vec<_>) = found
            .iter()
            .map(|&item| items[item as usize])
            .partition(|(row, _)| *row);
        Self {
            rows: rows.into_iter().map(|(_, item)| item).collect(),
            contents: contents.into_iter().map(|(_, item)| item).collect(),
        }
    }
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
        // Work and items are interned to dense indices, so each part's walk
        // stamps flat arrays instead of hashing: every part reaches most of
        // the shared cache fills, so the walk dominates.
        let mut ids: HashMap<Node, u32, FxBuild> = edges.work.clone();
        let mut intern = |node: Node| {
            let next = u32::try_from(ids.len()).unwrap_or(u32::MAX);
            *ids.entry(node).or_insert(next)
        };
        let store: HashMap<u32, u32, FxBuild> = uses
            .work
            .iter()
            .map(|(node, id)| (*id, intern(*node)))
            .collect();
        let work_count = ids.len();
        let mut observations = Self::default();
        let mut items: Vec<(bool, ContentHash)> = Vec::new();
        let mut item_ids: HashMap<(bool, ContentHash), u32, FxBuild> = HashMap::default();
        let mut by_work: Vec<Vec<u32>> = vec![Vec::new(); work_count];
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
            let Some(&work) = store.get(&work) else {
                observations.untraced.add(row, item);
                continue;
            };
            let id = *item_ids.entry((row, item)).or_insert_with(|| {
                items.push((row, item));
                u32::try_from(items.len() - 1).unwrap_or(u32::MAX)
            });
            by_work[work as usize].push(id);
        }
        // Each item's position in key order, so a part's items sort by a word.
        let mut rank = vec![0u32; items.len()];
        let mut order: Vec<u32> = (0..items.len() as u32).collect();
        order.sort_unstable_by_key(|&item| items[item as usize]);
        for (position, &item) in (0u32..).zip(&order) {
            rank[item as usize] = position;
        }
        let mut successors: Vec<Vec<u32>> = vec![Vec::new(); work_count];
        for &(reader, dependency) in &edges.edges {
            successors[reader as usize].push(dependency);
        }
        // Stamps: `seen[w] == stamp` when this part's walk reached `w`.
        let mut seen = vec![0u32; work_count];
        let mut held = vec![0u32; items.len()];
        let mut reached_by_any = vec![false; work_count];
        let mut parts: Vec<(&Node, u32)> = ids
            .iter()
            .filter(|(node, _)| node.0 == hirsel_intern::attribution::PART)
            .map(|(node, &id)| (node, id))
            .collect();
        parts.sort_unstable();
        let mut stack = Vec::new();
        let mut found: Vec<u32> = Vec::new();
        for (stamp, (node, part)) in (1u32..).zip(parts) {
            seen[part as usize] = stamp;
            stack.push(part);
            found.clear();
            while let Some(work) = stack.pop() {
                reached_by_any[work as usize] = true;
                for &item in &by_work[work as usize] {
                    if std::mem::replace(&mut held[item as usize], stamp) != stamp {
                        found.push(item);
                    }
                }
                for &next in &successors[work as usize] {
                    if std::mem::replace(&mut seen[next as usize], stamp) != stamp {
                        stack.push(next);
                    }
                }
            }
            observations
                .parts
                .insert(node.1, Used::of(&items, &rank, &mut found));
        }
        let mut package: Vec<u32> = (0..work_count)
            .filter(|&work| !reached_by_any[work])
            .flat_map(|work| by_work[work].iter().copied())
            .collect();
        package.sort_unstable();
        package.dedup();
        observations.package = Used::of(&items, &rank, &mut package);
        observations
    }
}
