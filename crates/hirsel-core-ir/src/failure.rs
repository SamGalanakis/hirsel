//! Within-module failure reachability (crate docs, "Failure"): which functions
//! may fail. Recursive components without a failure source stay infallible;
//! bodies defined elsewhere and indirect calls are sources.
use crate::{BinaryOp, Body, Callee, Inst, Module, Terminator, Ty};

pub struct Failures {
    /// By function: whether a call of it may fail.
    pub may_fail: Vec<bool>,
    /// By function, by value: the constant an `Inst::Const` defines.
    pub constants: Vec<Vec<Option<u64>>>,
}
impl Failures {
    pub fn new(module: &Module) -> Self {
        let constants: Vec<_> = module
            .functions
            .iter()
            .map(|f| f.body.as_ref().map_or_else(Vec::new, constants))
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
                        op, left, right, ..
                    } if constant_safe(
                        op,
                        body.values[left.index()],
                        constants[i][right.index()],
                    ) => {}
                    _ => fails |= inst.can_fail(body),
                }
            }
            if fails {
                may_fail[i] = true;
                pending.push(i);
            }
        }
        // A dispatching symbol tail-calls either body, so both share its status.
        for (canonical, free) in module.step_free.iter().filter(|(_, s)| s.dispatch) {
            callers[canonical.index()].push(free.body.index());
            callers[free.body.index()].push(canonical.index());
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
/// By value: the constant an `Inst::Const` defines.
pub fn constants(body: &Body) -> Vec<Option<u64>> {
    let mut values = vec![None; body.values.len()];
    for inst in &body.insts {
        if let Inst::Const { result, bits } = *inst {
            values[result.index()] = Some(bits);
        }
    }
    values
}
/// An integer op whose constant right operand rules out its failure (SEM-1): a
/// shift below the width, a remainder by nonzero, or a quotient by nonzero that
/// is not -1 when signed (the one overflowing divisor).
pub fn constant_safe(op: BinaryOp, ty: Ty, right: Option<u64>) -> bool {
    let (Some((signed, width)), Some(n)) = (ty.int(), right) else {
        return false;
    };
    let ones = u64::MAX >> (64 - width);
    match op {
        BinaryOp::Shl | BinaryOp::Shr => n < u64::from(width),
        BinaryOp::Rem => n & ones != 0,
        BinaryOp::Div => n & ones != 0 && !(signed && n & ones == ones),
        _ => false,
    }
}
