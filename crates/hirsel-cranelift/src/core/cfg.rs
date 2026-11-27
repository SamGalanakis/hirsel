use super::*;

/// Blocks entered only through edges the front marked cold (core-ir Edge::cold)
/// or from blocks that are cold themselves; Cranelift lays them out last.
pub(super) fn cold_blocks(body: &Body) -> Vec<bool> {
    let n = body.blocks.len();
    // Each block's predecessors, with whether that entry is a cold edge.
    let mut incoming: Vec<Vec<(usize, bool)>> = vec![Vec::new(); n];
    for b in 0..n {
        let mut edges = Vec::new();
        match &body.terminators[b] {
            Terminator::Jump(e) => edges.push(*e),
            Terminator::Branch { yes, no, .. } => edges.extend([*yes, *no]),
            Terminator::Switch {
                arms, otherwise, ..
            } => {
                edges.push(*otherwise);
                edges.extend(
                    arms.get(&body.pools.arms)
                        .unwrap()
                        .iter()
                        .map(|arm| arm.edge),
                );
            }
            Terminator::Raise {
                cleanup: Some(block),
                ..
            } => incoming[block.index()].push((b, false)),
            _ => {}
        }
        for e in edges {
            let e = &body.edges[e.index()];
            incoming[e.target.index()].push((b, e.cold));
        }
        for inst in body.blocks[b].insts.get(&body.insts).unwrap() {
            if let Some(block) = inst.fail(body) {
                incoming[block.index()].push((b, false));
            }
        }
    }
    let mut cold = vec![false; n];
    loop {
        let mut changed = false;
        for b in 1..n {
            if !cold[b]
                && !incoming[b].is_empty()
                && incoming[b].iter().all(|&(from, edge)| edge || cold[from])
            {
                cold[b] = true;
                changed = true;
            }
        }
        if !changed {
            return cold;
        }
    }
}
