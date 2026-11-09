//! Function entries and feedback-edge targets used by private execution checkpoints.
use crate::ir::{BlockId, Inst, LirUnit};

/// Per block of `unit`, whether it is a checkpoint.
pub fn checkpoint_points<S>(unit: &LirUnit<S>) -> Vec<bool> {
    let tables = &unit.tables;
    let target = |edge: crate::ir::EdgeId| tables.edges.get(edge.index()).map(|e| e.target);
    let failure_target = |failure: crate::ir::FailureId| {
        tables
            .failures
            .get(failure.index())
            .and_then(|f| target(f.cleanup))
    };
    let succ: Vec<Vec<usize>> = unit
        .blocks
        .iter()
        .zip(&unit.terminators)
        .map(|(block, terminator)| {
            let instructions = block.instructions.get(&unit.instructions).unwrap_or(&[]);
            instructions
                .iter()
                .filter_map(Inst::failure)
                .filter_map(failure_target)
                .chain(
                    terminator
                        .normal_edges(&unit.pools)
                        .into_iter()
                        .filter_map(target),
                )
                .chain(
                    terminator
                        .failure_edges(tables)
                        .into_iter()
                        .filter_map(failure_target),
                )
                .map(BlockId::index)
                .filter(|&b| b < unit.blocks.len())
                .collect()
        })
        .collect();
    let mut points = vec![false; unit.blocks.len()];
    if let Some(entry) = points.get_mut(unit.entry.index()) {
        *entry = true;
    }
    for (_, head) in hirsel_graph::feedback_edges(&succ, unit.entry.index()) {
        points[head] = true;
    }
    points
}
