//! Closed task/channel rows bind the runtime's C layout descriptors by value.
//! Bodies and drop callbacks already use the frozen context/destination ABI.
use super::function::Function;
use super::*;
use cranelift_codegen::ir::{MemFlagsData, Value};
const P: Ty = Ty::Ptr;
const U: Ty = Ty::Scalar(Scalar::U64);
const K: Ty = Ty::Scalar(Scalar::U8);
const B: Ty = Ty::Scalar(Scalar::Bool);

impl Function<'_, '_, '_> {
    fn descriptor(&mut self, types: &[TypeId]) -> Result<Value> {
        let storage = self.calls.storage(&mut self.b, (types.len() * 32) as u32);
        for (i, &t) in types.iter().enumerate() {
            let (bytes, alignment) =
                place::storage(self.input, self.layouts, PlaceTy::Declared(t))?;
            let logical = hirsel_core_layout::field_size(
                &self.input.types,
                self.layouts,
                &hirsel_core_layout::FieldTy::Inline(hirsel_core_layout::TypeRef(t.0)),
            )
            .2;
            for (j, n) in [bytes, alignment, logical].into_iter().enumerate() {
                let value = self.b.ins().iconst(types::I64, n as i64);
                self.b.ins().store(
                    MemFlagsData::trusted(),
                    value,
                    storage,
                    (i * 32 + j * 8) as i32,
                );
            }
            let function = self.calls.reference(
                self.object,
                &mut self.b,
                self.glues.drops[t.index()].unwrap(),
            );
            let drop = self.b.ins().func_addr(types::I64, function);
            self.b
                .ins()
                .store(MemFlagsData::trusted(), drop, storage, (i * 32 + 24) as i32);
        }
        Ok(storage)
    }
    pub fn runtime_call(&mut self, op: RtOp, call: &CallSite) -> Result<()> {
        let mut args = [Value::from_u32(0); 4];
        for (i, arg) in call
            .args
            .get(&self.body.pools.args)
            .unwrap()
            .iter()
            .enumerate()
        {
            args[i] = match *arg {
                Arg::Value(v) => self.get(v),
                Arg::Address(a) => self.addr(a),
            };
        }
        let mut outputs = [Value::from_u32(0); 3];
        let site = self.b.ins().iconst(types::I64, 0);
        let n = match op {
            RtOp::DestId => {
                outputs[0] = self.c_call("hirsel_core_dest_id", &[], &[U], &[])?[0];
                1
            }
            RtOp::ArgsLen => {
                outputs[0] = self.c_call(
                    "hirsel_core_args_len",
                    &[P, U],
                    &[U],
                    &[self.context, args[0]],
                )?[0];
                1
            }
            // The run time's carrier-returning `args_get` and its `host_op` entry
            // (FORM decision 50) are not built yet.
            RtOp::ArgsGet | RtOp::Host(_) => {
                return Err(unsupported(format!("provider row {op:?}")));
            }
            RtOp::CompareBytes => {
                outputs[0] = self.c_call(
                    "hirsel_core_compare_bytes",
                    &[P, U, P, U],
                    &[Ty::Scalar(Scalar::I8)],
                    &args,
                )?[0];
                1
            }
            RtOp::CheckCancel => {
                let status = self.c_call(
                    "hirsel_core_check_cancel",
                    &[P, U],
                    &[K],
                    &[self.context, site],
                )?[0];
                self.call_failure(status, call.fail)?;
                0
            }
            RtOp::Spawn { x, r } => {
                let descriptor = self.descriptor(&[x, r])?;
                let values = self.c_call(
                    "hirsel_core_spawn",
                    &[P, U, P, P, P],
                    &[P, U],
                    &[self.context, args[0], args[1], args[2], descriptor],
                )?;
                outputs[0] = values[0];
                outputs[1] = values[1];
                2
            }
            RtOp::Join { .. } => {
                let values = self.c_call(
                    "hirsel_core_join",
                    &[P, P, P, U],
                    &[K, P, K],
                    &[self.context, args[0], args[1], site],
                )?;
                self.call_failure(values[2], call.fail)?;
                outputs[0] = values[0];
                outputs[1] = values[1];
                2
            }
            RtOp::Cancel => {
                self.c_call("hirsel_core_cancel", &[P, P], &[], &[self.context, args[0]])?;
                0
            }
            RtOp::ChannelNew { item } => {
                let descriptor = self.descriptor(&[item])?;
                let values = self.c_call(
                    "hirsel_core_channel_new",
                    &[P, U, P, U],
                    &[P, P, U, K],
                    &[self.context, args[0], descriptor, site],
                )?;
                self.call_failure(values[3], call.fail)?;
                outputs[0] = values[0];
                outputs[1] = values[1];
                outputs[2] = values[2];
                3
            }
            RtOp::ChannelSender => {
                outputs[0] = self.c_call("hirsel_core_channel_sender", &[P], &[P], &[args[0]])?[0];
                1
            }
            RtOp::Send { .. } | RtOp::Receive { .. } => {
                let name = if matches!(op, RtOp::Send { .. }) {
                    "hirsel_core_send"
                } else {
                    "hirsel_core_receive"
                };
                let values = self.c_call(
                    name,
                    &[P, P, P, U],
                    &[B, K],
                    &[self.context, args[0], args[1], site],
                )?;
                self.call_failure(values[1], call.fail)?;
                outputs[0] = values[0];
                1
            }
            RtOp::TrySend { .. } => {
                outputs[0] =
                    self.c_call("hirsel_core_try_send", &[P, P], &[K], &[args[0], args[1]])?[0];
                1
            }
        };
        for (&id, &value) in call
            .results
            .get(&self.body.pools.values)
            .unwrap()
            .iter()
            .zip(&outputs[..n])
        {
            self.set(id, value);
        }
        Ok(())
    }
}
