//! Retained code per target (PROG-6, PROG-10); checked code is target-free.

/// Retained code for `target` (PROG-10): every target read becomes the target's index,
/// and every switch on such a read keeps only the arm the index selects. Blocks the
/// kept arms no longer reach leave the body when its normal form is taken.
pub fn select(
    module: &crate::Module,
    target: hirsel_core_layout::Target,
) -> std::borrow::Cow<'_, crate::Module> {
    use crate::{Inst, LayoutQuery, Terminator};
    let targeted = |i: &Inst| {
        matches!(
            i,
            Inst::LayoutConst {
                query: LayoutQuery::Target,
                ..
            }
        )
    };
    if !module
        .functions
        .iter()
        .filter_map(|f| f.body.as_ref())
        .any(|b| b.insts.iter().any(targeted))
    {
        return std::borrow::Cow::Borrowed(module);
    }
    let mut module = module.clone();
    let index = target.index() as u64;
    for body in module.functions.iter_mut().filter_map(|f| f.body.as_mut()) {
        let mut reads = std::collections::BTreeSet::new();
        for inst in &mut body.insts {
            if let Inst::LayoutConst {
                result,
                query: LayoutQuery::Target,
            } = *inst
            {
                reads.insert(result);
                *inst = Inst::Const {
                    result,
                    bits: index,
                };
            }
        }
        for term in &mut body.terminators {
            if let Terminator::Switch {
                value,
                arms,
                otherwise,
            } = *term
                && reads.contains(&value)
            {
                let arms = arms.get(&body.pools.arms).expect("switch arms are pooled");
                let chosen = arms
                    .iter()
                    .find(|a| a.value == index)
                    .map_or(otherwise, |a| a.edge);
                *term = Terminator::Jump(chosen);
            }
        }
    }
    std::borrow::Cow::Owned(module)
}

/// The reason of the first `target.unsupported(why)` call `function`'s body makes (PROG-6),
/// read from the static the front puts just before the call, at the call's anchor.
pub fn unsupported(module: &crate::Module, function: crate::FuncId) -> Option<String> {
    use crate::{Callee, Inst, Static};
    let body = module.functions[function.index()].body.as_ref()?;
    for block in &body.blocks {
        let start = block.insts.start as usize;
        let insts = block.insts.get(&body.insts)?;
        for (i, inst) in insts.iter().enumerate() {
            let Inst::Call { call } = *inst else { continue };
            let Callee::Direct(f) = body.calls[call.index()].callee else {
                continue;
            };
            let instance = &module.instances[module.functions[f.index()].instance.index()];
            if !instance.definition.is_core("target.unsupported") {
                continue;
            }
            let anchor = body.inst_anchors[start + i];
            let why = insts[..i]
                .iter()
                .zip(&body.inst_anchors[start..start + i])
                .rev()
                .find_map(|(inst, at)| match (inst, *at == anchor) {
                    (Inst::ReadStatic { data, .. }, true) => Some(*data),
                    _ => None,
                })
                .and_then(|data| match &module.statics[data.index()] {
                    Static::Bytes { bytes, .. } => {
                        Some(String::from_utf8_lossy(bytes).into_owned())
                    }
                    Static::Aggregate { .. } => None,
                });
            return Some(why.unwrap_or_default());
        }
    }
    None
}
