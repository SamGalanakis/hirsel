//! The glue worklist rows (FAIL-17): a context's stack of pending walk entries.
use super::*;

impl Function<'_, '_, '_> {
    /// Lowers `rt.glue_mark`, `rt.glue_push`, `rt.glue_pop` and `rt.glue_cut`, and
    /// returns how many outputs it wrote.
    pub(super) fn glue_worklist(
        &mut self,
        op: RtOp,
        args: &[Value],
        outputs: &mut [Value],
    ) -> Result<usize> {
        let ctx = self.context;
        let (name, params, results, words): (_, &[Ty], &[Ty], Vec<Value>) = match op {
            RtOp::GlueMark => ("hirsel_core_glue_mark", &[P], &[U], vec![ctx]),
            RtOp::GluePush => {
                let words = vec![ctx, args[0], args[1], args[2], args[3]];
                ("hirsel_core_glue_push", &[P, P, P, U, U], &[], words)
            }
            RtOp::GluePop => ("hirsel_core_glue_pop", &[P, U], &[P], vec![ctx, args[0]]),
            RtOp::GlueCut => ("hirsel_core_glue_cut", &[P, U], &[], vec![ctx, args[0]]),
            _ => unreachable!("only the glue worklist rows"),
        };
        let values = self.c_call(name, params, results, &words)?;
        if !results.is_empty() {
            outputs[0] = values[0];
        }
        Ok(results.len())
    }
}
