//! Ordered live roots are the primary cleanup input. Radix nodes are immutable
//! and shared between snapshots, so a changed membership rebuilds only its path
//! and two snapshots differ only below the nodes they do not share.
use hirsel_intern::work::{self, Counter};
use std::rc::Rc;
const BITS: usize = 3;
const WIDTH: usize = 1 << BITS;
/// A tree's root sits at the least height whose capacity covers its greatest
/// root (leaves are height 0); a tree only grows.
#[derive(Clone, Default)]
pub(super) struct Live {
    root: Option<Rc<Node>>,
    height: usize,
}
/// The roots a node at `height` covers.
fn capacity(height: usize) -> u64 {
    1 << ((height + 1) * BITS)
}
struct Node {
    data: Data,
    scopes: std::cell::OnceCell<(u32, u32)>,
}
enum Data {
    Branch([Option<Rc<Node>>; WIDTH]),
    Leaf([bool; WIDTH]),
}
impl Clone for Node {
    fn clone(&self) -> Self {
        Self::new(match &self.data {
            Data::Branch(children) => Data::Branch(children.clone()),
            Data::Leaf(values) => Data::Leaf(*values),
        })
    }
}
impl Node {
    fn new(data: Data) -> Self {
        Self {
            data,
            scopes: std::cell::OnceCell::new(),
        }
    }
}
impl Live {
    /// The root node as the root of a tree of `height`, at least this one's.
    fn lifted(&self, height: usize) -> Option<Rc<Node>> {
        let mut node = self.root.clone();
        for _ in self.height..height {
            node = node.map(|node| {
                let mut children: [Option<Rc<Node>>; WIDTH] = std::array::from_fn(|_| None);
                children[0] = Some(node);
                Rc::new(Node::new(Data::Branch(children)))
            });
        }
        node
    }
    fn grow(&mut self, root: u32) {
        while capacity(self.height) <= root as u64 {
            self.root = self.lifted(self.height + 1);
            self.height += 1;
        }
    }
    fn at(node: &Option<Rc<Node>>, height: usize, root: u32) -> bool {
        match node.as_deref().map(|node| &node.data) {
            None => false,
            Some(Data::Leaf(values)) => values[root as usize % WIDTH],
            Some(Data::Branch(children)) => Self::at(
                &children[(root as usize >> (height * BITS)) % WIDTH],
                height - 1,
                root,
            ),
        }
    }
    fn change(slot: &mut Option<Rc<Node>>, height: usize, root: u32, live: bool) {
        let empty = {
            let node = slot.get_or_insert_with(|| {
                Rc::new(Node::new(if height == 0 {
                    Data::Leaf([false; WIDTH])
                } else {
                    Data::Branch(std::array::from_fn(|_| None))
                }))
            });
            let node = Rc::make_mut(node);
            node.scopes = std::cell::OnceCell::new();
            match &mut node.data {
                Data::Leaf(values) => {
                    values[root as usize % WIDTH] = live;
                    !values.iter().any(|value| *value)
                }
                Data::Branch(children) => {
                    Self::change(
                        &mut children[(root as usize >> (height * BITS)) % WIDTH],
                        height - 1,
                        root,
                        live,
                    );
                    children.iter().all(Option::is_none)
                }
            }
        };
        if empty {
            *slot = None;
        }
    }
    pub fn contains(&self, root: &u32) -> bool {
        (*root as u64) < capacity(self.height) && Self::at(&self.root, self.height, *root)
    }
    pub fn insert(&mut self, root: u32) -> bool {
        if self.contains(&root) {
            false
        } else {
            self.grow(root);
            Self::change(&mut self.root, self.height, root, true);
            true
        }
    }
    pub fn remove(&mut self, root: &u32) -> bool {
        if !self.contains(root) {
            false
        } else {
            Self::change(&mut self.root, self.height, *root, false);
            true
        }
    }
    #[cfg(test)]
    fn roots(node: &Option<Rc<Node>>, height: usize, base: u64, start: u64, roots: &mut Vec<u32>) {
        if base + (1u64 << ((height + 1) * BITS)) <= start {
            return;
        }
        let Some(node) = node else {
            return;
        };
        match &node.data {
            Data::Leaf(values) => {
                for (index, live) in values.iter().enumerate() {
                    let root = base + index as u64;
                    if *live && root >= start {
                        roots.push(root as u32);
                    }
                }
            }
            Data::Branch(children) => {
                for (index, child) in children.iter().enumerate() {
                    Self::roots(
                        child,
                        height - 1,
                        base + ((index as u64) << (height * BITS)),
                        start,
                        roots,
                    );
                }
            }
        }
    }
    #[cfg(test)]
    pub fn roots_from(&self, start: u32) -> Vec<u32> {
        let mut roots = vec![];
        Self::roots(&self.root, self.height, 0, start as u64, &mut roots);
        roots
    }
    #[cfg(test)]
    pub fn iter(&self) -> std::vec::IntoIter<u32> {
        self.roots_from(0).into_iter()
    }
    pub fn union(&mut self, other: &Self) {
        fn merge(a: &mut Option<Rc<Node>>, b: &Option<Rc<Node>>) {
            let Some(incoming) = b else {
                return;
            };
            let Some(old) = a else {
                *a = b.clone();
                return;
            };
            if Rc::ptr_eq(old, incoming) {
                return;
            }
            match (&old.data, &incoming.data) {
                (Data::Leaf(x), Data::Leaf(y)) => {
                    let joined = std::array::from_fn(|i| x[i] || y[i]);
                    if &joined != x {
                        *a = Some(Rc::new(Node::new(Data::Leaf(joined))));
                    }
                }
                (Data::Branch(x), Data::Branch(y)) => {
                    let mut children = x.clone();
                    let mut changed = false;
                    for i in 0..WIDTH {
                        let previous = children[i].clone();
                        merge(&mut children[i], &y[i]);
                        changed |= match (&previous, &children[i]) {
                            (None, None) => false,
                            (Some(a), Some(b)) => !Rc::ptr_eq(a, b),
                            _ => true,
                        };
                    }
                    if changed {
                        *a = Some(Rc::new(Node::new(Data::Branch(children))));
                    }
                }
                _ => unreachable!(),
            }
        }
        let height = self.height.max(other.height);
        let mut root = self.lifted(height);
        merge(&mut root, &other.lifted(height));
        self.root = root;
        self.height = height;
    }
    pub fn roots_in_scopes(&self, first: u32, end: u32, scope: &impl Fn(u32) -> u32) -> Vec<u32> {
        fn bounds(
            node: &Node,
            height: usize,
            base: u64,
            scope: &impl Fn(u32) -> u32,
        ) -> (u32, u32) {
            *node.scopes.get_or_init(|| match &node.data {
                Data::Leaf(values) => values
                    .iter()
                    .enumerate()
                    .filter(|(_, live)| **live)
                    .map(|(i, _)| {
                        work::count(Counter::LoweringRetirementRows);
                        let s = scope((base + i as u64) as u32);
                        (s, s)
                    })
                    .fold((u32::MAX, 0), |(a, b), (c, d)| (a.min(c), b.max(d))),
                Data::Branch(children) => children
                    .iter()
                    .enumerate()
                    .filter_map(|(i, child)| {
                        child.as_ref().map(|child| {
                            bounds(
                                child,
                                height - 1,
                                base + ((i as u64) << (height * BITS)),
                                scope,
                            )
                        })
                    })
                    .fold((u32::MAX, 0), |(a, b), (c, d)| (a.min(c), b.max(d))),
            })
        }
        fn visit(
            node: &Option<Rc<Node>>,
            height: usize,
            base: u64,
            first: u32,
            end: u32,
            scope: &impl Fn(u32) -> u32,
            roots: &mut Vec<u32>,
        ) {
            let Some(node) = node else {
                return;
            };
            let (min, max) = bounds(node, height, base, scope);
            if max < first || min >= end {
                return;
            }
            match &node.data {
                Data::Leaf(values) => {
                    for (index, live) in values.iter().enumerate() {
                        let root = (base + index as u64) as u32;
                        if *live && {
                            work::count(Counter::LoweringRetirementRows);
                            (first..end).contains(&scope(root))
                        } {
                            roots.push(root);
                        }
                    }
                }
                Data::Branch(children) => {
                    for (index, child) in children.iter().enumerate() {
                        visit(
                            child,
                            height - 1,
                            base + ((index as u64) << (height * BITS)),
                            first,
                            end,
                            scope,
                            roots,
                        );
                    }
                }
            }
        }
        let mut roots = vec![];
        visit(&self.root, self.height, 0, first, end, scope, &mut roots);
        roots
    }
    /// The roots live in exactly one of `self` and `previous`, ascending.
    pub fn changed_since(&self, previous: &Self) -> Vec<u32> {
        const NONE: Option<Rc<Node>> = None;
        fn data(node: &Option<Rc<Node>>) -> Option<&Data> {
            node.as_deref().map(|node| &node.data)
        }
        fn values(data: Option<&Data>) -> [bool; WIDTH] {
            match data {
                Some(Data::Leaf(values)) => *values,
                None => [false; WIDTH],
                _ => unreachable!(),
            }
        }
        fn children(data: Option<&Data>) -> Option<&[Option<Rc<Node>>; WIDTH]> {
            match data {
                Some(Data::Branch(children)) => Some(children),
                None => None,
                _ => unreachable!(),
            }
        }
        fn visit(
            now: &Option<Rc<Node>>,
            old: &Option<Rc<Node>>,
            height: usize,
            base: u64,
            changed: &mut Vec<u32>,
        ) {
            match (now, old) {
                (None, None) => return,
                (Some(now), Some(old)) if Rc::ptr_eq(now, old) => return,
                _ => {}
            }
            if height == 0 {
                let (now, old) = (values(data(now)), values(data(old)));
                for index in 0..WIDTH {
                    if now[index] || old[index] {
                        work::count(Counter::LoweringCleanupRootRows);
                    }
                    if now[index] != old[index] {
                        changed.push((base + index as u64) as u32);
                    }
                }
                return;
            }
            let (now, old) = (children(data(now)), children(data(old)));
            for index in 0..WIDTH {
                visit(
                    now.map_or(&NONE, |children| &children[index]),
                    old.map_or(&NONE, |children| &children[index]),
                    height - 1,
                    base + ((index as u64) << (height * BITS)),
                    changed,
                );
            }
        }
        let mut changed = vec![];
        let height = self.height.max(previous.height);
        let (now, old) = (self.lifted(height), previous.lifted(height));
        visit(&now, &old, height, 0, &mut changed);
        changed
    }
    /// Exact ordered set difference, including old storage becoming live again.
    pub fn added_since(&self, previous: &Self) -> Vec<u32> {
        fn visit(
            now: &Option<Rc<Node>>,
            old: &Option<Rc<Node>>,
            height: usize,
            base: u64,
            added: &mut Vec<u32>,
        ) {
            let Some(now) = now else { return };
            if old.as_ref().is_some_and(|old| Rc::ptr_eq(now, old)) {
                return;
            }
            match (&now.data, old.as_ref().map(|node| &node.data)) {
                (Data::Leaf(values), old) => {
                    let old = match old {
                        Some(Data::Leaf(values)) => Some(values),
                        None => None,
                        _ => unreachable!(),
                    };
                    for (index, live) in values.iter().enumerate() {
                        work::count(Counter::LoweringRetirementRows);
                        if *live && old.is_none_or(|old| !old[index]) {
                            added.push((base + index as u64) as u32);
                        }
                    }
                }
                (Data::Branch(children), old) => {
                    let old = match old {
                        Some(Data::Branch(children)) => Some(children),
                        None => None,
                        _ => unreachable!(),
                    };
                    for (index, child) in children.iter().enumerate() {
                        visit(
                            child,
                            old.map(|old| &old[index]).unwrap_or(&None),
                            height - 1,
                            base + ((index as u64) << (height * BITS)),
                            added,
                        );
                    }
                }
            }
        }
        let mut added = vec![];
        let height = self.height.max(previous.height);
        let (now, old) = (self.lifted(height), previous.lifted(height));
        visit(&now, &old, height, 0, &mut added);
        added
    }
}
impl FromIterator<u32> for Live {
    fn from_iter<I: IntoIterator<Item = u32>>(iter: I) -> Self {
        let mut live = Self::default();
        for root in iter {
            live.insert(root);
        }
        live
    }
}
