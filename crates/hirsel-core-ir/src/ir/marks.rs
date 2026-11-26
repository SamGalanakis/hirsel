//! FORM v1c marks removed from a module before native emission.
use super::*;
use std::borrow::Cow;

/// `m` with every `mark` and `unmark` removed (FORM v1c). Neither has a run-time
/// effect, so an engine must emit the same code for both; engines' tests compare them.
/// An owned module is changed in place; a borrowed one is copied only when it
/// has a mark to remove.
pub fn without_marks(mut m: Cow<'_, Module>) -> Cow<'_, Module> {
    let marked = |b: &Body| {
        b.insts
            .iter()
            .any(|i| matches!(i, Inst::Mark { .. } | Inst::Unmark { .. }))
    };
    if !m
        .functions
        .iter()
        .filter_map(|f| f.body.as_ref())
        .any(marked)
    {
        return m;
    }
    for b in m
        .to_mut()
        .functions
        .iter_mut()
        .filter_map(|f| f.body.as_mut())
    {
        let (mut insts, mut anchors) = (Vec::new(), Vec::new());
        for block in &mut b.blocks {
            let start = insts.len() as u32;
            for i in block.insts.start..block.insts.start + block.insts.len {
                if !matches!(b.insts[i as usize], Inst::Mark { .. } | Inst::Unmark { .. }) {
                    insts.push(b.insts[i as usize].clone());
                    anchors.push(b.inst_anchors[i as usize]);
                }
            }
            block.insts = PoolRange::new(start, insts.len() as u32 - start);
        }
        (b.insts, b.inst_anchors) = (insts, anchors);
    }
    m
}
