//! PACK-16 `Held`, resource-origin, and loaded static-literal lowering.

use cranelift_codegen::ir::{InstBuilder, MemFlagsData, Value, types};
use hirsel_core_ir::{RtOp, Ty, TypeId};
use hirsel_core_layout::Scalar;

use super::Error;
use super::function::Function;
use super::task_entry;

const P: Ty = Ty::Ptr;
const U: Ty = Ty::Scalar(Scalar::U64);
const B: Ty = Ty::Scalar(Scalar::Bool);

impl Function<'_, '_, '_> {
    /// A `Cross` stores the value bytes at `shift`, the span of the value a
    /// recorder writes (a task entry's `x` when `entry`), then its owner rows.
    pub(super) fn cross(&mut self, ty: TypeId, shift: u64, entry: bool) -> Result<Value, Error> {
        let (words, owners) = task_entry::cross_words(self.input, self.layouts, ty, shift, entry)?;
        let at = self.own_slot((40 + owners.len() * 32) as u32);
        let rows = self.b.ins().iadd_imm_s(at, 40);
        for (j, &n) in words.iter().enumerate() {
            let value = if j == 1 {
                rows
            } else {
                self.b.ins().iconst(types::I64, n as i64)
            };
            self.b
                .ins()
                .store(MemFlagsData::trusted(), value, at, (j * 8) as i32);
        }
        for (i, row) in owners.iter().enumerate() {
            for (j, &n) in row.iter().enumerate() {
                let value = self.b.ins().iconst(types::I64, n as i64);
                self.b.ins().store(
                    MemFlagsData::trusted(),
                    value,
                    at,
                    (40 + i * 32 + j * 8) as i32,
                );
            }
        }
        Ok(at)
    }

    pub(super) fn lower_held_op(
        &mut self,
        op: RtOp,
        args: &[Value],
        outputs: &mut [Value],
    ) -> Result<Option<usize>, Error> {
        let count = match op {
            RtOp::ResourceOrigin { .. } if !self.input.track_pack_owners => 0,
            RtOp::ResourceOrigin { item } => {
                let recipe = self.calls.reference(
                    self.object,
                    &mut self.b,
                    self.glues.resources[item.index()].unwrap(),
                );
                let recipe = self.b.ins().func_addr(types::I64, recipe);
                self.c_call(
                    "hirsel_core_resource_origin",
                    &[P, P],
                    &[],
                    &[args[0], recipe],
                )?;
                0
            }
            RtOp::HeldNew { item } => {
                let descriptor = self.descriptor(&[item], false)?;
                if self.layouts[item.index()].drop.is_none() {
                    let zero = self.b.ins().iconst(types::I64, 0);
                    self.b
                        .ins()
                        .store(MemFlagsData::trusted(), zero, descriptor, 24);
                }
                let result = self.c_call(
                    "hirsel_core_held_new",
                    &[P, P, P, U, P],
                    &[P, U],
                    &[self.context, args[0], args[1], args[2], descriptor],
                )?;
                outputs[0] = result[0];
                outputs[1] = result[1];
                2
            }
            RtOp::HeldTake { .. } => {
                outputs[0] = self.c_call("hirsel_core_held_take", &[P, P, P], &[B], &args[..3])?[0];
                1
            }
            RtOp::StaticString => {
                outputs[0] = if self.input.pack_module {
                    self.c_call(
                        "hirsel_core_static_string",
                        &[P, U],
                        &[P],
                        &[args[0], args[1]],
                    )?[0]
                } else {
                    args[0]
                };
                1
            }
            RtOp::StaticFrozen => {
                if self.input.pack_module {
                    let pair = self.c_call(
                        "hirsel_core_static_frozen",
                        &[P, U],
                        &[P, P],
                        &[args[0], args[2]],
                    )?;
                    outputs[0] = pair[0];
                    outputs[1] = pair[1];
                } else {
                    outputs[0] = args[0];
                    outputs[1] = args[1];
                }
                2
            }
            RtOp::ReleaseStaticBytes => {
                self.c_call("hirsel_core_release_static_bytes", &[P], &[], &[args[0]])?;
                0
            }
            _ => return Ok(None),
        };
        Ok(Some(count))
    }
}
