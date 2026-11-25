//! A body's instructions in canonical order.
use super::*;

/// Lay out emitted instructions in block order after control-flow construction.
pub(super) fn order_instructions(b: &mut Body) {
    let mut start = 0;
    if b.blocks.iter().all(|block| {
        let ordered = block.insts.start as usize == start;
        start += block.insts.len as usize;
        ordered
    }) && start == b.insts.len()
    {
        return;
    }
    let mut old = std::mem::take(&mut b.insts);
    let mut insts = Vec::with_capacity(old.len());
    let mut anchors = Vec::with_capacity(old.len());
    for block in &mut b.blocks {
        let (start, len) = (block.insts.start as usize, block.insts.len as usize);
        let at = insts.len() as u32;
        // The emitter gives every instruction to one block. Move it once rather
        // than dispatching Inst::clone for every variant during this permutation.
        insts.extend(old[start..start + len].iter_mut().map(|inst| {
            std::mem::replace(
                inst,
                Inst::Const {
                    result: ValueId(0),
                    bits: 0,
                },
            )
        }));
        anchors.extend_from_slice(&b.inst_anchors[start..start + len]);
        block.insts = PoolRange::new(at, len as u32);
    }
    b.insts = insts;
    b.inst_anchors = anchors;
}
