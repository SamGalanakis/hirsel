//! DICE 47675948 key_index.rs:35-76 separates interned keys from dense rows.
//! Pearce-Kelly (JEA 2007, doi:10.1145/1187436.1210590) repairs only affected
//! vertices. Structure follows JGraphT 756d0f29 DirectedAcyclicGraph.java:418-617:
//! https://github.com/jgrapht/jgrapht/blob/756d0f29419cbe629337ae94e3e8940bd9ced538/jgrapht-core/src/main/java/org/jgrapht/graph/DirectedAcyclicGraph.java#L418
//! Hirsel uses dense slots, generation-independent visit epochs, iterative
//! discovery and sorted vectors instead of hash/ordered sets. Historical
//! reads retain their separate exact cycle authority across slot retirement.
use super::*;
use std::ops::{Index, IndexMut};

/// Private to one locked runtime. No ID crosses its owner's boundary. Each
/// claim gets a fresh generation, including reuse of an evicted arena slot.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct NodeId {
    pub(super) slot: u32,
    generation: u32,
}
#[derive(Clone, Copy)]
pub(super) struct Edge {
    target: EdgeTarget,
    token: u32,
}
#[derive(Clone, Copy)]
struct EdgeTarget(u32);
// Dense admitted slots leave a pin bit inside the existing eight-byte edge.
const BODY_CONSUMED: u32 = 1 << 31;
impl Edge {
    fn target(&self) -> u32 {
        self.target.0 & !BODY_CONSUMED
    }
    fn body_consumed(&self) -> bool {
        self.target.0 & BODY_CONSUMED != 0
    }
}
#[derive(Clone, Copy)]
pub(super) struct Subscriber {
    waiter: NodeId,
    edge: u32,
}
#[derive(Default)]
struct Recorded {
    settled: BTreeSet<Key>,
    pending: BTreeSet<Key>,
}
impl Recorded {
    fn contains(&self, target: &Key) -> bool {
        self.settled.contains(target) || self.pending.contains(target)
    }
}
#[derive(Default)]
pub(super) struct Nodes {
    index: std::collections::HashMap<Key, u32>,
    // Keep pinned products too: external Arc owners may disappear without a
    // scheduler callback. Eligibility is checked when the ordered entry is read.
    eviction: BTreeSet<(u64, u32, Key)>,
    edges: std::collections::HashMap<(u32, u32), u32>,
    keys: Vec<Option<Key>>,
    rows: Vec<Node>,
    free: Vec<u32>,
    // Consumed recorded reads use logical identities, never reusable slots.
    // These complement the active edges and retain the same cycle authority.
    history: BTreeMap<Key, Recorded>,
    serial: u32,
    clock: u64,
    repair_epoch: u32,
    in_flight_roots: Vec<Key>,
    in_flight_reached: Vec<u32>,
    in_flight_dirty: bool,
    pub repair_visits: u64,
}
impl Nodes {
    pub fn len(&self) -> usize {
        self.index.len()
    }
    pub fn arena_len(&self) -> usize {
        self.rows.len()
    }
    pub fn next_arena_len(&self, query: Key) -> usize {
        self.rows.len() + usize::from(!self.index.contains_key(&query) && self.free.is_empty())
    }
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }
    pub fn clear(&mut self) {
        *self = Self::default();
    }
    pub fn has_slot(&self, key: &Key) -> bool {
        self.index.contains_key(key)
    }
    pub fn can_insert(&self, key: Key, limit: usize) -> bool {
        self.serial < u32::MAX
            && (self.index.contains_key(&key)
                || self.index.len() < limit.min(BODY_CONSUMED as usize))
    }
    pub fn get(&self, key: &Key) -> Option<&Node> {
        let node = &self.rows[*self.index.get(key)? as usize];
        (!matches!(node.state, NodeState::Dormant)).then_some(node)
    }
    pub fn get_mut(&mut self, key: &Key) -> Option<&mut Node> {
        let node = &mut self.rows[*self.index.get(key)? as usize];
        (!matches!(node.state, NodeState::Dormant)).then_some(node)
    }
    pub fn lookup(&self, key: Key) -> Option<NodeId> {
        let &slot = self.index.get(&key)?;
        Some(NodeId {
            slot,
            generation: self.rows[slot as usize].generation,
        })
    }
    pub fn node_id(&self, key: Key) -> Option<NodeId> {
        self.lookup(key).filter(|&id| self.get_id(id).is_some())
    }
    pub fn get_id(&self, id: NodeId) -> Option<&Node> {
        let node = self.rows.get(id.slot as usize)?;
        (node.generation == id.generation && !matches!(node.state, NodeState::Dormant))
            .then_some(node)
    }
    pub fn get_id_mut(&mut self, id: NodeId) -> Option<&mut Node> {
        let node = self.rows.get_mut(id.slot as usize)?;
        (node.generation == id.generation && !matches!(node.state, NodeState::Dormant))
            .then_some(node)
    }
    pub fn result(&self, id: NodeId) -> Option<Outcome> {
        match &self.get_id(id)?.state {
            NodeState::Settled {
                outcome: result, ..
            } => Some((**result).clone()),
            _ => None,
        }
    }
    pub fn key_at(&self, slot: u32) -> Key {
        self.keys[slot as usize].expect("interned edge")
    }
    pub fn mark_live(&mut self, roots: impl Iterator<Item = Key>) {
        for node in &mut self.rows {
            node.live = false;
        }
        let mut pending: Vec<u32> = roots
            .filter_map(|key| self.index.get(&key).copied())
            .collect();
        while let Some(slot) = pending.pop() {
            let node = &mut self.rows[slot as usize];
            if !node.live && !matches!(node.state, NodeState::Dormant) {
                node.live = true;
                // Settled results retain their semantic reads, but no longer
                // own unfinished speculative executions of those reads.
                if !matches!(node.state, NodeState::Settled { .. }) {
                    pending.extend(node.dependencies.iter().map(|edge| edge.target()));
                }
            }
        }
    }
    pub fn mark_in_flight(&mut self, roots: impl Iterator<Item = Key> + Clone) {
        if !self.in_flight_dirty && self.in_flight_roots.iter().copied().eq(roots.clone()) {
            return;
        }
        self.in_flight_dirty = false;
        self.in_flight_roots.clear();
        self.in_flight_roots.extend(roots);
        while let Some(slot) = self.in_flight_reached.pop() {
            self.rows[slot as usize].in_flight = false;
            if let Some(key) = self.keys[slot as usize] {
                self.index_eviction(key);
            }
        }
        let mut pending: Vec<u32> = self
            .in_flight_roots
            .iter()
            .filter_map(|key| self.index.get(key).copied())
            .collect();
        while let Some(slot) = pending.pop() {
            let node = &mut self.rows[slot as usize];
            if !node.in_flight && !matches!(node.state, NodeState::Dormant) {
                node.in_flight = true;
                self.in_flight_reached.push(slot);
                self.eviction.remove(&(
                    node.used_at,
                    node.generation,
                    self.keys[slot as usize].expect("live slot"),
                ));
                // Completed producers own their results; their historical reads
                // no longer pin the payloads consumed during computation.
                if !matches!(node.state, NodeState::Settled { .. }) {
                    pending.extend(
                        node.dependencies
                            .iter()
                            .filter(|edge| !edge.body_consumed())
                            .map(|edge| edge.target()),
                    );
                }
            }
        }
    }
    fn index_eviction(&mut self, key: Key) {
        let Some(node) = self.get(&key) else {
            return;
        };
        if !node.in_flight
            && matches!(&node.state, NodeState::Settled { outcome, .. }
            if outcome.as_ref().as_ref().is_ok_and(|product| product.live_owners() != 0))
        {
            self.eviction.insert((node.used_at, node.generation, key));
        }
    }
    pub fn eviction_after(&self, previous: Option<(u64, u32, Key)>) -> Option<(u64, u32, Key)> {
        use std::ops::Bound::{Excluded, Unbounded};
        self.eviction
            .range((previous.map_or(Unbounded, Excluded), Unbounded))
            .next()
            .copied()
    }
    pub fn iter(&self) -> impl Iterator<Item = (&Key, &Node)> {
        self.keys.iter().zip(&self.rows).filter_map(|(key, node)| {
            (!matches!(node.state, NodeState::Dormant)).then_some((key.as_ref()?, node))
        })
    }
    pub fn insert(&mut self, key: Key, mut node: Node) -> NodeId {
        self.in_flight_dirty = true;
        self.serial = self.serial.checked_add(1).expect("admitted generation");
        node.generation = self.serial;
        let slot = if let Some(&slot) = self.index.get(&key) {
            let old = &mut self.rows[slot as usize];
            assert!(matches!(old.state, NodeState::Dormant));
            node.position = old.position;
            node.subscribers = std::mem::take(&mut old.subscribers);
            *old = node;
            slot
        } else {
            let slot = if let Some(slot) = self.free.pop() {
                node.position = self.rows[slot as usize].position;
                self.rows[slot as usize] = node;
                self.keys[slot as usize] = Some(key);
                slot
            } else {
                let slot = u32::try_from(self.rows.len()).expect("admitted slots");
                assert!(slot < BODY_CONSUMED, "admitted edge slot");
                node.position = slot;
                self.rows.push(node);
                self.keys.push(Some(key));
                slot
            };
            self.index.insert(key, slot);
            slot
        };
        self.index_eviction(key);
        NodeId {
            slot,
            generation: self.serial,
        }
    }
    fn reclaim(&mut self, slot: u32) {
        let node = &self.rows[slot as usize];
        if matches!(node.state, NodeState::Dormant)
            && node.subscribers.is_empty()
            && let Some(key) = self.keys[slot as usize].take()
        {
            self.index.remove(&key);
            self.free.push(slot);
        }
    }
    pub fn remove(&mut self, key: &Key) -> Option<NodeState> {
        let &slot = self.index.get(key)?;
        let node = &mut self.rows[slot as usize];
        self.eviction.remove(&(node.used_at, node.generation, *key));
        self.in_flight_dirty |= node.in_flight;
        if !matches!(node.state, NodeState::Settled { .. } | NodeState::Dormant) {
            // Cancellation discards this unfinished generation's reads. Slot
            // retirement of terminal results retains all successful/failed reads.
            if let Some(recorded) = self.history.get_mut(key) {
                recorded.pending.clear();
                if recorded.settled.is_empty() {
                    self.history.remove(key);
                }
            }
        }
        let removed = std::mem::take(&mut node.state);
        node.unsatisfied = 0;
        let generation = node.generation;
        let dependencies = std::mem::take(&mut node.dependencies);
        for edge in dependencies {
            self.edges.remove(&(slot, edge.target()));
            self.rows[edge.target() as usize]
                .subscribers
                .retain(|s| s.waiter != NodeId { slot, generation });
            self.reclaim(edge.target());
        }
        self.reclaim(slot);
        Some(removed)
    }
    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }
    #[cfg(test)]
    pub fn edge(&self, source: NodeId, target: Key) -> Option<usize> {
        let &slot = self.index.get(&target)?;
        self.edges
            .get(&(source.slot, slot))
            .map(|&edge| edge as usize)
    }
    pub fn edge_to(&self, source: NodeId, target: NodeId) -> Option<usize> {
        self.edges
            .get(&(source.slot, target.slot))
            .map(|&edge| edge as usize)
    }
    pub fn publication_frontier(&self, root: Key, candidate: NodeId) -> bool {
        let Some(mut cursor) = self.node_id(root) else {
            return false;
        };
        // insert_edge preserves read order. The graph is acyclic, so each
        // descent consumes one of its admitted nodes without recursion.
        loop {
            if cursor == candidate {
                return true;
            }
            let node = &self.rows[cursor.slot as usize];
            let Some(next) = node.dependencies.iter().find_map(|edge| {
                let slot = edge.target();
                (!edge.body_consumed()
                    && (!matches!(self.rows[slot as usize].state, NodeState::Settled { .. })
                        || matches!(self.rows[slot as usize].state, NodeState::Settled { outcome: ref result, .. } if result.is_err())))
                    .then_some(NodeId { slot, generation: self.rows[slot as usize].generation })
            }) else { return false; };
            cursor = next;
        }
    }
    pub fn insert_edge(&mut self, source: NodeId, target: NodeId) -> Option<usize> {
        if source.slot == target.slot
            || (!self.history.is_empty()
                && self
                    .recorded_path(
                        self.key_at(source.slot),
                        self.key_at(target.slot),
                        usize::MAX,
                    )
                    .is_some())
        {
            return None;
        }
        if !self.repair_order(source.slot, target.slot) {
            return None;
        }
        self.in_flight_dirty |= self.rows[source.slot as usize].in_flight
            && !matches!(
                self.rows[source.slot as usize].state,
                NodeState::Settled { .. }
            );
        let edge = self.rows[source.slot as usize].dependencies.len();
        self.edges.insert((source.slot, target.slot), edge as u32);
        self.rows[source.slot as usize].dependencies.push(Edge {
            target: EdgeTarget(target.slot),
            token: 0,
        });
        self.rows[target.slot as usize]
            .subscribers
            .push(Subscriber {
                waiter: source,
                edge: edge as u32,
            });
        Some(edge)
    }
    fn repair_order(&mut self, source: u32, target: u32) -> bool {
        let start = self.rows[target as usize].position;
        let end = self.rows[source as usize].position;
        self.repair_visits += 1;
        if start > end {
            return true;
        }
        self.repair_epoch = self.repair_epoch.checked_add(1).unwrap_or_else(|| {
            self.repair_visits += self.rows.len() as u64;
            for node in &mut self.rows {
                node.visited = 0;
            }
            1
        });
        let epoch = self.repair_epoch;
        self.rows[target as usize].visited = epoch;
        let mut affected = vec![target];
        let mut cursor = 0;
        while cursor < affected.len() {
            let slot = affected[cursor];
            cursor += 1;
            self.repair_visits += 1;
            for i in 0..self.rows[slot as usize].dependencies.len() {
                self.repair_visits += 1;
                let next = self.rows[slot as usize].dependencies[i].target();
                if next == source {
                    return false;
                }
                let node = &mut self.rows[next as usize];
                if node.position <= end && node.visited != epoch {
                    node.visited = epoch;
                    affected.push(next);
                }
            }
        }
        let forward = affected.len();
        self.rows[source as usize].visited = epoch;
        affected.push(source);
        // Reverse subscriptions describe every active read, independently of
        // its wait token. The two sets are disjoint after the cycle search.
        while cursor < affected.len() {
            let slot = affected[cursor];
            cursor += 1;
            self.repair_visits += 1;
            for i in 0..self.rows[slot as usize].subscribers.len() {
                self.repair_visits += 1;
                let previous = self.rows[slot as usize].subscribers[i].waiter.slot;
                let node = &mut self.rows[previous as usize];
                if node.position >= start && node.visited != epoch {
                    node.visited = epoch;
                    affected.push(previous);
                }
            }
        }
        // Pearce-Kelly: preserve the order within each affected set, then
        // assign their sorted old labels to predecessors before successors.
        // Unaffected rows are never scanned or rewritten. The discovery
        // vector's capacity is at most twice the affected count and the label
        // vector is exact-sized, within twelve scratch bytes per arena row.
        let rows = &self.rows;
        let visits = &mut self.repair_visits;
        let mut compare = |a: &u32, b: &u32| {
            *visits += 1;
            rows[*a as usize].position.cmp(&rows[*b as usize].position)
        };
        affected[..forward].sort_unstable_by(&mut compare);
        affected[forward..].sort_unstable_by(&mut compare);
        let mut positions: Vec<_> = affected
            .iter()
            .map(|&slot| self.rows[slot as usize].position)
            .collect();
        self.repair_visits += positions.len() as u64;
        positions.sort_unstable_by(|a, b| {
            self.repair_visits += 1;
            a.cmp(b)
        });
        for (&slot, position) in affected[forward..]
            .iter()
            .chain(&affected[..forward])
            .zip(positions)
        {
            self.repair_visits += 1;
            self.rows[slot as usize].position = position;
        }
        true
    }
    pub fn cancelled_record_bytes(&self, query: Key) -> u64 {
        if self
            .get(&query)
            .is_none_or(|node| matches!(node.state, NodeState::Settled { .. } | NodeState::Dormant))
        {
            return 0;
        }
        self.history.get(&query).map_or(0, |recorded| {
            recorded.pending.len() as u64 * 512 + if recorded.settled.is_empty() { 1024 } else { 0 }
        })
    }
    pub fn touch(&mut self, query: Key) {
        let node = &self[&query];
        self.eviction
            .remove(&(node.used_at, node.generation, query));
        self.clock = self.clock.saturating_add(1);
        self.get_mut(&query).expect("live query").used_at = self.clock;
        self.index_eviction(query);
    }
    pub fn settle_records(&mut self, query: Key) {
        self.in_flight_dirty |= self[&query].in_flight;
        self.touch(query);
        if let Some(recorded) = self.history.get_mut(&query) {
            recorded.settled.append(&mut recorded.pending);
        }
    }
    pub fn recorded_bytes(&self, source: NodeId, target: Option<Key>) -> u64 {
        let source_key = self.key_at(source.slot);
        let recorded = self.history.get(&source_key);
        let targets = target.map_or_else(
            || {
                self.rows[source.slot as usize]
                    .dependencies
                    .iter()
                    .map(|edge| self.key_at(edge.target()))
                    .collect::<Vec<_>>()
            },
            |target| vec![target],
        );
        let new = targets
            .iter()
            .filter(|target| recorded.is_none_or(|r| !r.contains(target)))
            .count();
        // Sparse B-tree nodes, one root per adjacency set, and exact traversal
        // parents, pending keys and the returned path share this debit. The
        // allocated-capacity accounting below also covers retained records.
        (new as u64) * 512
            + if new != 0 && recorded.is_none() {
                1024
            } else {
                0
            }
    }
    fn detach(&mut self, source: NodeId, edge: usize) {
        let source_key = self.key_at(source.slot);
        let target = self.rows[source.slot as usize].dependencies[edge].target();
        let target_key = self.key_at(target);
        let terminal = matches!(
            self.rows[source.slot as usize].state,
            NodeState::Settled { .. }
        );
        self.in_flight_dirty |= self.rows[source.slot as usize].in_flight
            && !terminal
            && !self.rows[source.slot as usize].dependencies[edge].body_consumed();
        let recorded = self.history.entry(source_key).or_default();
        if !recorded.settled.contains(&target_key) {
            if terminal {
                recorded.pending.remove(&target_key);
                recorded.settled.insert(target_key);
            } else {
                recorded.pending.insert(target_key);
            }
        }
        self.edges.remove(&(source.slot, target));
        self.rows[target as usize]
            .subscribers
            .retain(|s| s.waiter != source);
        self.reclaim(target);
        self.rows[source.slot as usize]
            .dependencies
            .swap_remove(edge);
        if let Some(moved) = self.rows[source.slot as usize].dependencies.get(edge) {
            let moved_target = moved.target();
            self.edges.insert((source.slot, moved_target), edge as u32);
            let subscriber = self.rows[moved_target as usize]
                .subscribers
                .iter_mut()
                .find(|s| s.waiter == source)
                .expect("reverse edge");
            subscriber.edge = edge as u32;
        }
    }
    pub fn consume_read(&mut self, source: NodeId, target: Key) -> bool {
        if self.get_id(source).is_none() {
            return false;
        }
        let Some(producer) = self.node_id(target) else {
            return false;
        };
        let Some(edge) = self.edge_to(source, producer) else {
            return false;
        };
        if self.rows[source.slot as usize].dependencies[edge].token != 0
            || !matches!(
                self.rows[producer.slot as usize].state,
                NodeState::Settled { .. }
            )
        {
            return false;
        }
        self.detach(source, edge);
        true
    }
    pub fn release_claims(&mut self, source: NodeId) {
        // A terminal continuation cancels any outstanding claims, including
        // reads preceding a failed result. Stale completions have no reverse
        // subscription with which to wake this generation.
        self.rows[source.slot as usize].unsatisfied = 0;
        while !self.rows[source.slot as usize].dependencies.is_empty() {
            self.detach(source, 0);
        }
    }
    pub fn reclaimable(&self, query: Key) -> bool {
        self.get(&query).is_some_and(|node| {
            matches!(node.state, NodeState::Settled { .. })
                && node.subscribers.is_empty()
                && node.unsatisfied == 0
        })
    }
    pub fn recorded_path(&self, source: Key, target: Key, limit: usize) -> Option<Vec<Key>> {
        if limit == 0 {
            return None;
        }
        if source == target {
            return Some(vec![target]);
        }
        if self
            .get(&target)
            .is_none_or(|node| node.dependencies.is_empty())
            && !self.history.contains_key(&target)
        {
            return None;
        }
        // This is the same exact DFS over live and recorded reads. Parent
        // lookup needs no ordering; adjacency order still determines the path.
        // Hash buckets and traversal vectors fit the existing 512-byte debit
        // per recorded read and ENTRY_BYTES per live row.
        let mut parents = std::collections::HashMap::new();
        let mut pending = vec![target];
        parents.insert(target, target);
        while let Some(current) = pending.pop() {
            if current == source {
                let mut path = Vec::new();
                let mut current = current;
                loop {
                    if path.len() == limit {
                        return None;
                    }
                    path.push(current);
                    if current == target {
                        path.reverse();
                        return Some(path);
                    }
                    current = parents[&current];
                }
            }
            let live = self
                .get(&current)
                .into_iter()
                .flat_map(|node| node.dependencies.iter())
                .map(|edge| self.key_at(edge.target()));
            let recorded = self
                .history
                .get(&current)
                .into_iter()
                .flat_map(|recorded| recorded.settled.iter().chain(&recorded.pending).copied());
            for next in live.chain(recorded) {
                if let std::collections::hash_map::Entry::Vacant(entry) = parents.entry(next) {
                    entry.insert(current);
                    pending.push(next);
                }
            }
        }
        None
    }
    pub fn retain_body(&mut self, source: NodeId, edge: usize) {
        if self.get_id(source).is_some() {
            let node = &self.rows[source.slot as usize];
            self.in_flight_dirty |= node.in_flight
                && !matches!(node.state, NodeState::Settled { .. })
                && node.dependencies[edge].body_consumed();
            self.rows[source.slot as usize].dependencies[edge].target.0 &= !BODY_CONSUMED;
        }
    }
    /// Recover the rejected edge's path through the same dense DAG used by
    /// insert_edge. Its predecessor, traversal and path slots fit the existing
    /// twelve-byte-per-row topology workspace; no second graph is built.
    #[cfg(test)]
    pub fn cycle_path(&self, source: NodeId, target: NodeId, limit: usize) -> Option<Vec<u32>> {
        self.get_id(source)?;
        self.get_id(target)?;
        if limit == 0 {
            return None;
        }
        let mut parents = vec![u32::MAX; self.rows.len()];
        let mut pending = Vec::with_capacity(self.rows.len());
        pending.push(target.slot);
        parents[target.slot as usize] = target.slot;
        let end = self.rows[source.slot as usize].position;
        while let Some(slot) = pending.pop() {
            if slot == source.slot {
                let mut path = Vec::with_capacity(limit.min(self.rows.len()));
                let mut current = slot;
                loop {
                    if path.len() == limit {
                        return None;
                    }
                    path.push(current);
                    if current == target.slot {
                        path.reverse();
                        return Some(path);
                    }
                    current = parents[current as usize];
                }
            }
            for edge in &self.rows[slot as usize].dependencies {
                let next = edge.target();
                if self.rows[next as usize].position <= end && parents[next as usize] == u32::MAX {
                    parents[next as usize] = slot;
                    pending.push(next);
                }
            }
        }
        None
    }
    pub fn release_body(&mut self, source: NodeId, target: Key) {
        if self.get_id(source).is_none() {
            return;
        }
        let Some(&slot) = self.index.get(&target) else {
            return;
        };
        let Some(&edge) = self.edges.get(&(source.slot, slot)) else {
            return;
        };
        let node = &self.rows[source.slot as usize];
        self.in_flight_dirty |= node.in_flight
            && !matches!(node.state, NodeState::Settled { .. })
            && !node.dependencies[edge as usize].body_consumed();
        self.rows[source.slot as usize].dependencies[edge as usize]
            .target
            .0 |= BODY_CONSUMED;
    }
    pub fn subscribe(&mut self, source: NodeId, edge: usize, producer: NodeId) -> bool {
        let node = &mut self.rows[source.slot as usize];
        assert_eq!(node.generation, source.generation);
        let token = &mut node.dependencies[edge].token;
        if *token == producer.generation {
            return false;
        }
        assert_eq!(*token, 0, "old subscription must drain before replacement");
        *token = producer.generation;
        node.unsatisfied += 1;
        true
    }
    pub fn subscribers(&self, producer: NodeId) -> &[Subscriber] {
        &self.rows[producer.slot as usize].subscribers
    }
    pub fn consume(&mut self, producer: NodeId, index: usize) -> Option<Key> {
        if self.rows[producer.slot as usize].generation != producer.generation {
            return None;
        }
        let subscription = *self.subscribers(producer).get(index)?;
        let waiter = &mut self.rows[subscription.waiter.slot as usize];
        if waiter.generation != subscription.waiter.generation {
            return None;
        }
        let edge = waiter.dependencies.get_mut(subscription.edge as usize)?;
        if edge.target() != producer.slot || edge.token != producer.generation {
            return None;
        }
        edge.token = 0;
        waiter.unsatisfied = waiter
            .unsatisfied
            .checked_sub(1)
            .expect("one pending token");
        if matches!(&waiter.state, NodeState::Waiting(execution) if waiter.unsatisfied == 0 || execution.wait_for == WaitFor::Any)
        {
            waiter.state = std::mem::take(&mut waiter.state).wake();
            Some(self.key_at(subscription.waiter.slot))
        } else {
            None
        }
    }
    pub fn transfer_subscriptions(&mut self, key: Key) {
        let producer = self.node_id(key).expect("replacement");
        for i in 0..self.subscribers(producer).len() {
            let subscription = self.subscribers(producer)[i];
            let waiter = &mut self.rows[subscription.waiter.slot as usize];
            if waiter.generation == subscription.waiter.generation
                && let Some(edge) = waiter.dependencies.get_mut(subscription.edge as usize)
                && edge.target() == producer.slot
                && edge.token != 0
            {
                edge.token = producer.generation;
            }
        }
    }
    pub fn allocated_bytes(&self) -> u64 {
        let index_capacity = self.index.capacity();
        let index_buckets = if index_capacity == 0 {
            0
        } else {
            (index_capacity * 8).div_ceil(7).next_power_of_two().max(4)
        };
        let capacity = self.edges.capacity();
        let buckets = if capacity == 0 {
            0
        } else {
            (capacity * 8).div_ceil(7).next_power_of_two().max(4)
        };
        // Include sparse B-tree nodes and hash control/bucket slack, as in
        // MemoGraph::retained_bytes. Vectors use allocated capacities; the
        // affected-set repair needs at most twelve extra bytes per arena row.
        let bytes = self.rows.capacity() * size_of::<Node>()
            + self.keys.capacity() * size_of::<Option<Key>>()
            + self.free.capacity() * size_of::<u32>()
            + index_buckets * (size_of::<(Key, u32)>() + 1)
            + 32
            + tree_bytes::<(u64, u32, Key), ()>(self.eviction.len())
            + self.in_flight_roots.capacity() * size_of::<Key>()
            + self.in_flight_reached.capacity() * size_of::<u32>()
            + (2 * (self.in_flight_roots.len() + self.edges.len())).max(4) * size_of::<u32>()
            + self.rows.len() * 12
            + buckets * (size_of::<((u32, u32), u32)>() + 1)
            + 32
            + tree_bytes::<Key, Recorded>(self.history.len())
            + self
                .history
                .values()
                .map(|recorded| {
                    tree_bytes::<Key, ()>(recorded.settled.len())
                        + tree_bytes::<Key, ()>(recorded.pending.len())
                })
                .sum::<usize>()
            + self
                .rows
                .iter()
                .map(|n| {
                    n.dependencies.capacity() * size_of::<Edge>()
                        + n.subscribers.capacity() * size_of::<Subscriber>()
                        + match n.state {
                            NodeState::Ready(_) | NodeState::Waiting(_) => {
                                size_of::<ParkedExecution>() + 16
                            }
                            NodeState::Running(_) | NodeState::Draining(_) => {
                                size_of::<InFlightExecution>() + 16
                            }
                            NodeState::Cancelling(_) => size_of::<LocalCleanup>() + 16,
                            NodeState::Settled { .. } => size_of::<Outcome>() + 16,
                            NodeState::Dormant => 0,
                        }
                })
                .sum::<usize>();
        bytes as u64
    }
}
impl Index<&Key> for Nodes {
    type Output = Node;
    fn index(&self, key: &Key) -> &Node {
        self.get(key).expect("active query")
    }
}
impl IndexMut<&Key> for Nodes {
    fn index_mut(&mut self, key: &Key) -> &mut Node {
        self.get_mut(key).expect("active query")
    }
}
#[cfg(test)]
impl Nodes {
    pub fn rows_for_test(&self, id: NodeId) -> u32 {
        self.rows[id.slot as usize].position
    }
}

// Rust std BTree node.rs uses B=6, eleven entry slots and twelve child
// pointers. Non-root nodes have at least five entries. Include one sparse
// root even after the final removal; 64 bytes cover headers and alignment.
pub(super) fn tree_bytes<K, V>(len: usize) -> usize {
    (1 + len / 5) * (64 + 11 * (size_of::<K>() + size_of::<V>()) + 12 * size_of::<usize>())
}
