use super::*;

impl Inst {
    /// Whether the instruction can fail; it may name a failure block exactly then.
    pub fn can_fail(&self, b: &Body) -> bool {
        let ty = |v: ValueId| b.values.get(v.index()).copied();
        match *self {
            Inst::Unary { op, operand, .. } => {
                op == UnaryOp::Neg && ty(operand).is_some_and(|t| t.int().is_some())
            }
            Inst::Binary { op, left, .. } => ty(left).is_some_and(|t| op.can_fail(t)),
            Inst::Simd { op, .. } => op.can_fail(),
            Inst::Address { address, .. } => b.addresses.get(address.index()).is_some_and(|a| {
                a.steps
                    .get(&b.pools.steps)
                    .unwrap_or_default()
                    .iter()
                    .any(|s| matches!(s, Step::Element(_)))
            }),
            Inst::Call { call } => b.calls.get(call.index()).is_none_or(|c| match c.callee {
                Callee::Runtime(op) => op.fails(),
                Callee::Direct(_) | Callee::Indirect(_) | Callee::PackExport(_) => true,
            }),
            Inst::Step { .. } | Inst::StackCheck { .. } | Inst::Drop { .. } => true,
            _ => false,
        }
    }

    /// The failure block the instruction names, if any.
    pub fn fail(&self, b: &Body) -> Option<BlockId> {
        match *self {
            Inst::Step { fail }
            | Inst::StackCheck { fail }
            | Inst::Unary { fail, .. }
            | Inst::Binary { fail, .. }
            | Inst::Simd { fail, .. }
            | Inst::Address { fail, .. }
            | Inst::Drop { fail, .. } => fail,
            Inst::Call { call } => b.calls.get(call.index()).and_then(|c| c.fail),
            _ => None,
        }
    }
}

/// The blocks control leaves `bi` for: its instructions' failure blocks and its
/// terminator's targets.
#[cfg(any(test, feature = "wf"))]
pub(crate) fn successors(b: &Body, bi: usize) -> Vec<BlockId> {
    let mut successors = Vec::new();
    for_each_successor(b, bi, |next| successors.push(next));
    successors
}

pub(crate) fn for_each_successor(b: &Body, bi: usize, mut visit: impl FnMut(BlockId)) {
    b.blocks[bi]
        .insts
        .get(&b.insts)
        .unwrap_or_default()
        .iter()
        .filter_map(|i| i.fail(b))
        .for_each(&mut visit);
    let edge = |e: &EdgeId| b.edges[e.index()].target;
    match &b.terminators[bi] {
        Terminator::Jump(e) => visit(edge(e)),
        Terminator::Branch { yes, no, .. } => {
            visit(edge(yes));
            visit(edge(no));
        }
        Terminator::Switch {
            arms, otherwise, ..
        } => {
            arms.get(&b.pools.arms)
                .unwrap_or_default()
                .iter()
                .map(|a| edge(&a.edge))
                .for_each(&mut visit);
            visit(edge(otherwise));
        }
        Terminator::Raise { cleanup, .. } => cleanup.iter().copied().for_each(visit),
        Terminator::Return(_) | Terminator::Propagate | Terminator::Unreachable => {}
    }
}

impl Body {
    /// The storage addresses with a lexical frame obligation in this body.
    pub fn frame_obligations(&self) -> std::collections::BTreeSet<AddressId> {
        self.insts
            .iter()
            .filter_map(|inst| match *inst {
                Inst::FrameAdmit { storage, .. }
                | Inst::FrameActive { storage, .. }
                | Inst::FrameRefund { storage } => Some(storage),
                _ => None,
            })
            .collect()
    }
}
