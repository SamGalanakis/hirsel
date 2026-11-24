//! What the release std leaves to the programs over it.
use hirsel_core_ir::FuncId;

/// A plain root a program lowers itself, without its plain callees. Its own
/// function takes code a caller can bind or makes an owned network wait, whose
/// private or returnable variant only the program's unit can build.
pub(super) fn root(module: &hirsel_core_ir::Module, root: FuncId) -> bool {
    crate::callable_variants::takes_code(module, root)
        || hirsel_core_ir::returnable::waits_owned(module, root)
}

/// Drops each descriptor of `module` no body reads: one an item made a
/// plain `fn` value defines only for a recording's descriptor index. A
/// lending library defines no placeholder another object defines over (its
/// lent copies would fold one), so such a value made in the std has no
/// descriptor, and a spawn of it records no entry identity.
pub(super) fn descriptors(module: &mut hirsel_core_ir::Module) {
    use hirsel_core_ir::Inst;
    use hirsel_core_ir::extent::{operands, terminator_uses};
    for body in module.functions.iter_mut().filter_map(|f| f.body.as_mut()) {
        if !body
            .insts
            .iter()
            .any(|i| matches!(i, Inst::DescriptorRef { .. }))
        {
            continue;
        }
        let mut read = vec![false; body.values.len()];
        for inst in &body.insts {
            for u in operands(body, inst).0 {
                if let Some(r) = read.get_mut(u) {
                    *r = true;
                }
            }
        }
        for block in 0..body.blocks.len() {
            for u in terminator_uses(body, block) {
                if let Some(r) = read.get_mut(u) {
                    *r = true;
                }
            }
        }
        for inst in &mut body.insts {
            if let Inst::DescriptorRef {
                result, function, ..
            } = *inst
                && !read[result.index()]
            {
                *inst = Inst::FunctionRef { result, function };
            }
        }
    }
}
