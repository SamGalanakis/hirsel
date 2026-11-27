//! PACK-14 runtime row emission for Cranelift.
use super::*;

impl Function<'_, '_, '_> {
    pub(super) fn pack_code_op(
        &mut self,
        op: RtOp,
        args: &[Value],
        outputs: &mut [Value],
    ) -> Result<usize> {
        Ok(match op {
            RtOp::CodeBind => {
                outputs[0] = self.c_call("hirsel_core_code_bind", &[P], &[P], &[args[0]])?[0];
                1
            }
            RtOp::CodeLoad => {
                outputs[0] = self.c_call(
                    "hirsel_core_code_load",
                    &[U, P, P, P, K, P, P, P],
                    &[K],
                    &args[..8],
                )?[0];
                1
            }
            RtOp::CodeShare => {
                outputs[0] = self.c_call("hirsel_core_code_share", &[P], &[P], &[args[0]])?[0];
                1
            }
            RtOp::CodeSlotNew => {
                outputs[0] = self.c_call("hirsel_core_code_slot_new", &[U], &[P], &[args[0]])?[0];
                1
            }
            RtOp::CodeActivate => {
                outputs[0] =
                    self.c_call("hirsel_core_code_activate", &[P, U, P, P], &[K], &args[..4])?[0];
                1
            }
            RtOp::CodeDispatchBegin => {
                outputs[0] = self.c_call(
                    "hirsel_core_code_dispatch_begin",
                    &[P, P],
                    &[P],
                    &[args[0], args[1]],
                )?[0];
                1
            }
            RtOp::CodeDispatchEnd => {
                self.c_call("hirsel_core_code_dispatch_end", &[U], &[], &[args[0]])?;
                0
            }
            RtOp::CodeIdentity => {
                self.c_call(
                    "hirsel_core_code_identity",
                    &[P, P],
                    &[],
                    &[args[0], args[1]],
                )?;
                0
            }
            RtOp::CodeRevision => {
                outputs[0] = self.c_call(
                    "hirsel_core_code_revision",
                    &[P, P],
                    &[K],
                    &[args[0], args[1]],
                )?[0];
                1
            }
            _ => unreachable!("non-code RtOp passed to pack_code_op"),
        })
    }
}
