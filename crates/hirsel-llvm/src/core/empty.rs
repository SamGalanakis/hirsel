//! An empty bound has no allocation, call or runtime observation of its payer.
//! Keep its Capacity decision, but no table identity or reservation is needed.
use super::*;

#[derive(Default)]
pub(super) struct EmptyBounds {
    pub enters: BTreeSet<ValueId>,
    pub exits: BTreeSet<usize>,
}
fn successors(body: &Body, block: usize) -> Option<Vec<usize>> {
    let edge = |e: EdgeId| body.edges[e.index()].target.index();
    Some(match &body.terminators[block] {
        Terminator::Jump(e) => vec![edge(*e)],
        Terminator::Branch { yes, no, .. } => vec![edge(*yes), edge(*no)],
        Terminator::Switch {
            arms, otherwise, ..
        } => arms
            .get(&body.pools.arms)
            .unwrap()
            .iter()
            .map(|arm| edge(arm.edge))
            .chain([edge(*otherwise)])
            .collect(),
        _ => return None,
    })
}
/// Whether `body`'s blocks reachable from its entry form no cycle.
pub(super) fn loop_free(body: &Body) -> bool {
    // 0 unvisited, 1 on the current path, 2 finished.
    let mut state = vec![0u8; body.blocks.len()];
    let mut stack = vec![(0usize, successors(body, 0).unwrap_or_default(), 0usize)];
    state[0] = 1;
    while let Some((block, next, at)) = stack.last_mut() {
        let Some(&s) = next.get(*at) else {
            state[*block] = 2;
            stack.pop();
            continue;
        };
        *at += 1;
        match state[s] {
            1 => return false,
            0 => {
                state[s] = 1;
                stack.push((s, successors(body, s).unwrap_or_default(), 0));
            }
            _ => {}
        }
    }
    true
}
/// The blocks of `body` that lie on a control-flow cycle (a strongly connected
/// component with an edge inside it).
fn looped(body: &Body) -> Vec<bool> {
    let n = body.blocks.len();
    let next: Vec<Vec<usize>> = (0..n)
        .map(|b| successors(body, b).unwrap_or_default())
        .collect();
    let mut index = vec![usize::MAX; n];
    let mut low = vec![0; n];
    let mut on = vec![false; n];
    let mut stack = Vec::new();
    let mut looped = vec![false; n];
    let mut counter = 0;
    for root in 0..n {
        if index[root] != usize::MAX {
            continue;
        }
        let mut frames = vec![(root, 0usize)];
        index[root] = counter;
        low[root] = counter;
        counter += 1;
        stack.push(root);
        on[root] = true;
        while let Some(&mut (v, ref mut at)) = frames.last_mut() {
            if let Some(&w) = next[v].get(*at) {
                *at += 1;
                if index[w] == usize::MAX {
                    index[w] = counter;
                    low[w] = counter;
                    counter += 1;
                    stack.push(w);
                    on[w] = true;
                    frames.push((w, 0));
                } else if on[w] {
                    low[v] = low[v].min(index[w]);
                }
                continue;
            }
            frames.pop();
            if let Some(&(parent, _)) = frames.last() {
                low[parent] = low[parent].min(low[v]);
            }
            if low[v] == index[v] {
                let mut members = Vec::new();
                loop {
                    let w = stack.pop().unwrap();
                    on[w] = false;
                    members.push(w);
                    if w == v {
                        break;
                    }
                }
                if members.len() > 1 || next[v].contains(&v) {
                    for m in members {
                        looped[m] = true;
                    }
                }
            }
        }
    }
    looped
}

/// Per function, whether some body in `module` calls it directly from a block
/// on a loop.
pub(super) fn called_in_loop(module: &Module) -> Vec<bool> {
    let mut called = vec![false; module.functions.len()];
    for f in &module.functions {
        let Some(body) = &f.body else { continue };
        let looped = looped(body);
        for (b, block) in body.blocks.iter().enumerate() {
            if !looped[b] {
                continue;
            }
            for inst in block.insts.get(&body.insts).unwrap_or_default() {
                if let Inst::Call { call } = inst
                    && let Some(Callee::Direct(id)) = body.calls.get(call.index()).map(|c| c.callee)
                {
                    called[id.index()] = true;
                }
            }
        }
    }
    called
}
fn pure(body: &Body, constants: &[Option<u64>], inst: &Inst) -> bool {
    match inst {
        Inst::Binary {
            op, left, right, ..
        } if failure::constant_safe(*op, body.values[left.index()], constants[right.index()]) => {
            true
        }
        Inst::Const { .. }
        | Inst::ReadStatic { .. }
        | Inst::FunctionRef { .. }
        | Inst::DescriptorRef { .. }
        | Inst::Unary { .. }
        | Inst::CompactWidth { .. }
        | Inst::Binary { .. }
        | Inst::Compare { .. }
        | Inst::Convert { .. }
        | Inst::IntArith { .. }
        | Inst::ScalarUnary { .. }
        | Inst::MulAdd { .. }
        | Inst::Address { .. }
        | Inst::Read { .. }
        | Inst::Store { .. }
        | Inst::StorageLive { .. }
        | Inst::StorageDead { .. }
        | Inst::BuiltinUse { .. }
        | Inst::Mark { .. }
        | Inst::Unmark { .. }
        | Inst::UpdateMask { .. }
        | Inst::Pointer { .. } => !inst.can_fail(body),
        _ => false,
    }
}
impl EmptyBounds {
    pub fn new(body: &Body, constants: &[Option<u64>]) -> Self {
        let mut result = Self::default();
        let mut predecessors = vec![vec![]; body.blocks.len()];
        for b in 0..body.blocks.len() {
            for next in successors(body, b).unwrap_or_default() {
                predecessors[next].push(b);
            }
        }
        for (block, b) in body.blocks.iter().enumerate() {
            let instructions = b.insts.get(&body.insts).unwrap();
            for (position, inst) in instructions.iter().enumerate() {
                let Inst::LimitEnter { admitted, .. } = inst else {
                    continue;
                };
                if !instructions[position + 1..]
                    .iter()
                    .all(|i| pure(body, constants, i))
                {
                    continue;
                }
                let Terminator::Branch { condition, yes, no } = body.terminators[block] else {
                    continue;
                };
                if condition != *admitted
                    || body.edges[yes.index()].target == body.edges[no.index()].target
                {
                    continue;
                }
                let entry = body.edges[yes.index()].target.index();
                let mut pending = vec![entry];
                let mut visited = BTreeSet::new();
                let mut exits = BTreeSet::new();
                let mut valid = true;
                while let Some(next) = pending.pop() {
                    if next == block {
                        valid = false;
                        break;
                    }
                    if !visited.insert(next) {
                        continue;
                    }
                    let info = &body.blocks[next];
                    let mut ended = false;
                    for (offset, inst) in info.insts.get(&body.insts).unwrap().iter().enumerate() {
                        if matches!(inst, Inst::LimitExit) {
                            exits.insert(info.insts.start as usize + offset);
                            ended = true;
                            break;
                        }
                        if !pure(body, constants, inst) {
                            valid = false;
                            break;
                        }
                    }
                    if !valid {
                        break;
                    }
                    if !ended {
                        let Some(edges) = successors(body, next) else {
                            valid = false;
                            break;
                        };
                        pending.extend(edges);
                    }
                }
                if !valid || exits.is_empty() {
                    continue;
                }
                if visited.iter().any(|b| {
                    predecessors[*b]
                        .iter()
                        .any(|p| !visited.contains(p) && !(*b == entry && *p == block))
                }) {
                    continue;
                }
                result.enters.insert(*admitted);
                result.exits.extend(exits);
            }
        }
        result
    }
}
