//! Within-unit failure reachability. Recursive components without a failure
//! source remain infallible; unknown bodies and indirect calls are sources.
use super::*;

pub(super) struct Failures {
    pub may_fail: Vec<bool>,
    pub constants: Vec<Vec<Option<u64>>>,
}
impl Failures {
    pub fn new(module: &Module) -> Self {
        let constants: Vec<_> = module
            .functions
            .iter()
            .map(|f| {
                let Some(body) = &f.body else {
                    return vec![];
                };
                let mut values = vec![None; body.values.len()];
                for inst in &body.insts {
                    if let Inst::Const { result, bits } = *inst {
                        values[result.index()] = Some(bits);
                    }
                }
                values
            })
            .collect();
        let mut may_fail = vec![false; module.functions.len()];
        let mut callers = vec![vec![]; module.functions.len()];
        let mut pending = vec![];
        for (i, function) in module.functions.iter().enumerate() {
            let Some(body) = &function.body else {
                may_fail[i] = true;
                pending.push(i);
                continue;
            };
            let mut fails = body
                .terminators
                .iter()
                .any(|t| matches!(t, Terminator::Raise { .. } | Terminator::Propagate));
            for inst in &body.insts {
                match *inst {
                    Inst::Call { call } => match body.calls[call.index()].callee {
                        Callee::Direct(f) => callers[f.index()].push(i),
                        Callee::Indirect(_) => fails = true,
                        Callee::Runtime(op) => fails |= op.fails(),
                    },
                    Inst::Binary {
                        op: BinaryOp::Shl | BinaryOp::Shr,
                        left,
                        right,
                        ..
                    } if bounded_shift(body.values[left.index()], constants[i][right.index()]) => {}
                    _ => fails |= inst.can_fail(body),
                }
            }
            if fails {
                may_fail[i] = true;
                pending.push(i);
            }
        }
        while let Some(callee) = pending.pop() {
            for &caller in &callers[callee] {
                if !may_fail[caller] {
                    may_fail[caller] = true;
                    pending.push(caller);
                }
            }
        }
        Self {
            may_fail,
            constants,
        }
    }
}
pub(super) fn bounded_shift(ty: Ty, amount: Option<u64>) -> bool {
    ty.int()
        .is_some_and(|(_, width)| amount.is_some_and(|n| n < u64::from(width)))
}
