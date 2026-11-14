//! Closed runtime rows bind the published task and provider C ABI. Provider
//! carriers stay run-owned; only waiting host operations observe failure status.
use super::function::Function;
use super::*;
use cranelift_codegen::ir::{MemFlagsData, Value};
const P: Ty = Ty::Ptr;
const U: Ty = Ty::Scalar(Scalar::U64);
const K: Ty = Ty::Scalar(Scalar::U8);
const B: Ty = Ty::Scalar(Scalar::Bool);

impl Function<'_, '_, '_> {
    fn descriptor(&mut self, types: &[TypeId], task_input: bool) -> Result<Value> {
        let storage = self.calls.storage(&mut self.b, (types.len() * 32) as u32);
        for (i, &t) in types.iter().enumerate() {
            let (mut bytes, mut alignment) =
                place::storage(self.input, self.layouts, PlaceTy::Declared(t))?;
            if task_input && i == 0 {
                let offset = task_entry::input_offset(self.input, self.layouts, t)?;
                bytes += offset;
                alignment = alignment.max(8);
            }
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
                if task_input && i == 0 {
                    self.task_entries.inputs[&t]
                } else {
                    self.glues.callbacks[t.index()].unwrap()
                },
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
        let mut outputs = [Value::from_u32(0); 4];
        let site = match op {
            RtOp::Host(_)
            | RtOp::CheckCancel
            | RtOp::Join { .. }
            | RtOp::ChannelNew { .. }
            | RtOp::Send { .. }
            | RtOp::Receive { .. } => self.site()?,
            _ => self.b.ins().iconst(types::I64, 0),
        };
        let n = match op {
            RtOp::IsolateEnter | RtOp::IsolateLeave | RtOp::IsolateCatch => {
                return Err(unsupported(
                    "isolate's runtime rows (crate docs, \"Isolation\") are not lowered yet",
                ));
            }
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
            RtOp::ArgsGet => {
                outputs[0] = self.c_call(
                    "hirsel_core_args_get",
                    &[P, U, U],
                    &[P],
                    &[self.context, args[0], args[1]],
                )?[0];
                1
            }
            RtOp::Host(op) => {
                let code = self.b.ins().iconst(types::I8, i64::from(op.code()));
                let null = self.b.ins().iconst(types::I64, 0);
                let no_mode = self.b.ins().iconst(types::I8, 0);
                let (operand, mode) = match op {
                    HostOp::Open => (args[1], args[2]),
                    HostOp::Read
                    | HostOp::Write
                    | HostOp::Mkdir
                    | HostOp::Mkdtemp
                    | HostOp::Remove
                    | HostOp::RemoveTree
                    | HostOp::Stdout
                    | HostOp::Stderr => (args[1], no_mode),
                    HostOp::Close
                    | HostOp::TryLock
                    | HostOp::Unlock
                    | HostOp::WallMs
                    | HostOp::MonoNs
                    | HostOp::Rename
                    | HostOp::SleepUntil => (null, no_mode),
                };
                // The C outcome is always 32 bytes: value at 0, error.tag at 8,
                // error.code at 12, written at 16, status at 24.
                let values = match op {
                    HostOp::Rename => self.c_call(
                        "hirsel_core_host_rename",
                        &[P, U, P, P, U],
                        &[U, K, Ty::Scalar(Scalar::U32), U, K],
                        &[self.context, args[0], args[1], args[2], site],
                    )?,
                    HostOp::SleepUntil => self.c_call(
                        "hirsel_core_host_sleep_until",
                        &[P, U, U, U],
                        &[U, K, Ty::Scalar(Scalar::U32), U, K],
                        &[self.context, args[0], args[1], site],
                    )?,
                    _ => self.c_call(
                        "hirsel_core_host_op",
                        &[P, K, U, P, K, U],
                        &[U, K, Ty::Scalar(Scalar::U32), U, K],
                        &[self.context, code, args[0], operand, mode, site],
                    )?,
                };
                if op.waits() {
                    self.call_failure(values[4], call.fail)?;
                }
                let n = if matches!(op, HostOp::WallMs | HostOp::MonoNs) {
                    1
                } else {
                    4
                };
                for i in 0..n {
                    outputs[i] = values[i];
                }
                n
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
            RtOp::HashBytes => {
                outputs[0] = self.c_call("hirsel_core_hash_bytes", &[P, U], &[U], &args[..2])?[0];
                1
            }
            RtOp::CauseNext | RtOp::CauseValue => {
                let name = if op == RtOp::CauseNext {
                    "hirsel_core_cause_next"
                } else {
                    "hirsel_core_cause_value"
                };
                outputs[0] = self.c_call(name, &[P], &[P], &[args[0]])?[0];
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
                let (bytes, align) =
                    place::storage(self.input, self.layouts, PlaceTy::Declared(x))?;
                let offset = task_entry::input_offset(self.input, self.layouts, x)?;
                let size = (bytes + offset)
                    .try_into()
                    .map_err(|_| unsupported("native task input exceeds the stack-slot limit"))?;
                let slot = self.b.create_sized_stack_slot(ir::StackSlotData::new(
                    ir::StackSlotKind::ExplicitSlot,
                    size,
                    align.max(8).trailing_zeros() as u8,
                ));
                let packed = self.b.ins().stack_addr(types::I64, slot, 0);
                self.b
                    .ins()
                    .store(MemFlagsData::trusted(), args[2], packed, 0);
                if bytes != 0 {
                    let data = self.b.ins().iadd_imm(packed, offset as i64);
                    let bytes = self.b.ins().iconst(types::I64, bytes as i64);
                    self.c_call("memcpy", &[P, P, U], &[P], &[data, args[1], bytes])?;
                }
                let bridge = self.calls.reference(
                    self.object,
                    &mut self.b,
                    self.task_entries.entries[&(x, r)],
                );
                let bridge = self.b.ins().func_addr(types::I64, bridge);
                let descriptor = self.descriptor(&[x, r], true)?;
                let values = self.c_call(
                    "hirsel_core_spawn",
                    &[P, U, P, P, P],
                    &[P, U],
                    &[self.context, args[0], packed, bridge, descriptor],
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
                let descriptor = self.descriptor(&[item], false)?;
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
