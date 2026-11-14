//! Cooper-Harvey-Kennedy dominance over CoreIR continuations. A failure edge
//! leaves its instruction before the result/address exists, so split there.
use super::*;
use std::cell::OnceCell;

pub(super) struct Flow<'a> {
    body: &'a Body,
    graph: OnceCell<Graph>,
}
impl<'a> Flow<'a> {
    pub(super) fn new(body: &'a Body) -> Self {
        Self {
            body,
            graph: OnceCell::new(),
        }
    }
    pub(super) fn available(&self, definition: (usize, usize), at: (usize, usize)) -> bool {
        if definition.0 == at.0 {
            return definition.1 < at.1;
        }
        let g = self.graph.get_or_init(|| Graph::new(self.body));
        let from = g.points[definition.0][definition.1 + 1];
        let mut to = g.points[at.0][at.1];
        if g.parent[from].is_none() || g.parent[to].is_none() {
            return false;
        }
        while g.rank[to] > g.rank[from] {
            to = g.parent[to].unwrap();
        }
        from == to
    }
}
struct Graph {
    points: Vec<Vec<usize>>,
    rank: Vec<usize>,
    parent: Vec<Option<usize>>,
}
impl Graph {
    fn new(body: &Body) -> Self {
        let mut count = body.blocks.len();
        let points = body
            .blocks
            .iter()
            .enumerate()
            .map(|(block, b)| {
                let mut node = block;
                let mut points = vec![node];
                for inst in b.insts.get(&body.insts).unwrap() {
                    if inst.fail(body).is_some() {
                        node = count;
                        count += 1;
                    }
                    points.push(node);
                }
                points
            })
            .collect::<Vec<_>>();
        let mut next = vec![Vec::new(); count];
        for (block, b) in body.blocks.iter().enumerate() {
            for (index, inst) in b.insts.get(&body.insts).unwrap().iter().enumerate() {
                if let Some(target) = inst.fail(body) {
                    next[points[block][index]].extend([points[block][index + 1], target.index()]);
                }
            }
            let successors = &mut next[*points[block].last().unwrap()];
            let mut edge = |id: EdgeId| successors.push(body.edges[id.index()].target.index());
            match body.terminators[block] {
                Terminator::Jump(id) => edge(id),
                Terminator::Branch { yes, no, .. } => {
                    edge(yes);
                    edge(no);
                }
                Terminator::Switch {
                    arms, otherwise, ..
                } => {
                    edge(otherwise);
                    for arm in arms.get(&body.pools.arms).unwrap() {
                        edge(arm.edge);
                    }
                }
                Terminator::Raise {
                    cleanup: Some(target),
                    ..
                } => successors.push(target.index()),
                Terminator::Return(_)
                | Terminator::Unreachable
                | Terminator::Propagate
                | Terminator::Raise { cleanup: None, .. } => {}
            }
        }
        let mut before = vec![Vec::new(); count];
        for (node, targets) in next.iter().enumerate() {
            for &target in targets {
                before[target].push(node);
            }
        }
        let mut seen = vec![false; count];
        let mut order = Vec::new();
        let mut stack = vec![(0, 0)];
        seen[0] = true;
        while let Some((node, index)) = stack.last_mut() {
            if let Some(&target) = next[*node].get(*index) {
                *index += 1;
                if !seen[target] {
                    seen[target] = true;
                    stack.push((target, 0));
                }
            } else {
                order.push(*node);
                stack.pop();
            }
        }
        order.reverse();
        let mut rank = vec![usize::MAX; count];
        for (index, &node) in order.iter().enumerate() {
            rank[node] = index;
        }
        let mut parent = vec![None; count];
        parent[0] = Some(0);
        loop {
            let mut changed = false;
            for &node in order.iter().skip(1) {
                let mut candidates = before[node]
                    .iter()
                    .copied()
                    .filter(|p| parent[*p].is_some());
                let Some(mut common) = candidates.next() else {
                    continue;
                };
                for mut other in candidates {
                    while common != other {
                        while rank[common] > rank[other] {
                            common = parent[common].unwrap();
                        }
                        while rank[other] > rank[common] {
                            other = parent[other].unwrap();
                        }
                    }
                }
                if parent[node] != Some(common) {
                    parent[node] = Some(common);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        Self {
            points,
            rank,
            parent,
        }
    }
}
