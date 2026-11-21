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
            // A task input has no cleanup here: the child body always cleans it.
            let clean = if task_input && i == 0 {
                self.b.ins().iconst(types::I64, 0)
            } else {
                let function = self.calls.reference(
                    self.object,
                    &mut self.b,
                    self.glues.values[t.index()].unwrap(),
                );
                self.b.ins().func_addr(types::I64, function)
            };
            self.b.ins().store(
                MemFlagsData::trusted(),
                clean,
                storage,
                (i * 32 + 24) as i32,
            );
        }
        Ok(storage)
    }
    pub fn runtime_call(&mut self, op: RtOp, call: &CallSite, fused_receive: bool) -> Result<()> {
        let mut args = [Value::from_u32(0); 9];
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
        let mut outputs = [Value::from_u32(0); 8];
        let site = match op {
            RtOp::Host(_)
            | RtOp::CheckCancel
            | RtOp::Join { .. }
            | RtOp::ChannelNew { .. }
            | RtOp::Send { .. }
            | RtOp::Receive { .. }
            | RtOp::Spawn { .. }
            | RtOp::TapeGrant
            | RtOp::Flow(_) => self.site()?,
            _ => self.b.ins().iconst(types::I64, 0),
        };
        let n = match op {
            RtOp::Flow(op) => {
                let opcode = self.b.ins().iconst(types::I8, op as i64);
                let values = self.c_call(
                    "hirsel_core_flow",
                    &[P, P, K, P, P, U],
                    &[K],
                    &[self.context, args[0], opcode, args[1], args[2], site],
                )?;
                self.call_failure(values[0], call.fail)?;
                0
            }
            RtOp::StepEnter => {
                outputs[0] = self.c_call(
                    "hirsel_core_step_enter",
                    &[P, U],
                    &[B],
                    &[self.context, args[0]],
                )?[0];
                1
            }
            RtOp::StepLeave => {
                self.c_call("hirsel_core_step_leave", &[P], &[], &[self.context])?;
                0
            }
            RtOp::StepCatch => {
                outputs[0] = self.c_call("hirsel_core_step_catch", &[P], &[B], &[self.context])?[0];
                1
            }
            RtOp::IsolateEnter => {
                let values =
                    self.c_call("hirsel_core_isolate_enter", &[P], &[B, U], &[self.context])?;
                outputs[0] = values[0];
                outputs[1] = values[1];
                2
            }
            RtOp::IsolateLeave => {
                self.c_call("hirsel_core_isolate_leave", &[P], &[], &[self.context])?;
                0
            }
            RtOp::IsolateCatch => {
                let values =
                    self.c_call("hirsel_core_isolate_catch", &[P], &[P, K], &[self.context])?;
                self.call_failure(values[1], call.fail)?;
                outputs[0] = values[0];
                1
            }
            RtOp::DestId => {
                outputs[0] = self.c_call("hirsel_core_dest_id", &[], &[U], &[])?[0];
                1
            }
            RtOp::TapeOpen => {
                let revision = self.code_revision()?;
                let values = self.c_call(
                    "hirsel_core_tape_open",
                    &[P, U, P, K, P, P, U, U, U, P],
                    &[U, U],
                    &[
                        self.context,
                        args[0],
                        args[1],
                        args[2],
                        args[3],
                        args[4],
                        args[5],
                        args[6],
                        args[7],
                        revision,
                    ],
                )?;
                outputs[0] = values[0];
                outputs[1] = values[1];
                2
            }
            RtOp::TapeEnd => {
                outputs[0] = self.c_call(
                    "hirsel_core_tape_end",
                    &[P, U, K, P],
                    &[U],
                    &[self.context, args[0], args[1], args[2]],
                )?[0];
                1
            }
            RtOp::TapeAcross => {
                outputs[0] = self.c_call(
                    "hirsel_core_tape_across",
                    &[P, U],
                    &[B],
                    &[self.context, args[0]],
                )?[0];
                1
            }
            RtOp::TapeGrant => {
                let values = self.c_call(
                    "hirsel_core_tape_grant",
                    &[P, U, K, U, U],
                    &[U, K],
                    &[self.context, args[0], args[1], args[2], site],
                )?;
                self.call_failure(values[1], call.fail)?;
                outputs[0] = values[0];
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
            RtOp::LaunchWord => {
                outputs[0] = self.c_call(
                    "hirsel_core_launch_word",
                    &[P, U, U],
                    &[U],
                    &[self.context, args[0], args[1]],
                )?[0];
                1
            }
            RtOp::LaunchTarget { .. } => {
                self.c_call(
                    "hirsel_core_launch_target",
                    &[P, P],
                    &[],
                    &[self.context, args[0]],
                )?;
                0
            }
            RtOp::DescribeEmit => {
                outputs[0] = self.c_call(
                    "hirsel_core_describe_emit",
                    &[P, P],
                    &[B],
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
                    | HostOp::Stderr
                    | HostOp::Random => (args[1], no_mode),
                    HostOp::Close
                    | HostOp::TryLock
                    | HostOp::Unlock
                    | HostOp::WallMs
                    | HostOp::MonoNs
                    | HostOp::Rename
                    | HostOp::SleepUntil
                    | HostOp::Sync
                    | HostOp::SyncDir
                    | HostOp::Connect
                    | HostOp::Listen
                    | HostOp::Accept
                    | HostOp::RecvBounded
                    | HostOp::SendPair
                    | HostOp::NetNarrow
                    | HostOp::NetNarrowTls
                    | HostOp::TlsConnect
                    | HostOp::SendSecret
                    | HostOp::SecretSeal
                    | HostOp::SecretNarrow
                    | HostOp::NetSplit
                    | HostOp::Stopped => (null, no_mode),
                };
                // The C outcome is always 32 bytes: value at 0, error.tag at 8,
                // error.code at 12, written at 16, status at 24.
                let values = match op {
                    HostOp::Connect
                    | HostOp::Listen
                    | HostOp::Accept
                    | HostOp::RecvBounded
                    | HostOp::SendPair
                    | HostOp::NetNarrow
                    | HostOp::NetNarrowTls
                    | HostOp::TlsConnect
                    | HostOp::SendSecret
                    | HostOp::SecretSeal
                    | HostOp::SecretNarrow
                    | HostOp::NetSplit => {
                        let signature = op_signature(op);
                        let count = signature.len();
                        let mut params = vec![P];
                        params.extend(signature);
                        params.push(U);
                        let mut operands = vec![self.context];
                        operands.extend_from_slice(&args[..count]);
                        operands.push(site);
                        let mut results = vec![U, K, Ty::Scalar(Scalar::U32), U, K];
                        if matches!(op, HostOp::Listen | HostOp::Accept) {
                            results.extend([K, U, U, Ty::Scalar(Scalar::U16)]);
                        } else if op == HostOp::TlsConnect {
                            results.extend([U; 4]);
                        }
                        self.c_call(op.net_symbol(), &params, &results, &operands)?
                    }
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
                        op.io_symbol(),
                        &[P, K, U, P, K, U],
                        &[U, K, Ty::Scalar(Scalar::U32), U, K],
                        &[self.context, code, args[0], operand, mode, site],
                    )?,
                };
                // Every provider row can fail: a tape refuses it before issue (CAP-9).
                self.call_failure(values[4], call.fail)?;
                let n = if matches!(op, HostOp::Random | HostOp::Stopped) {
                    0
                } else if matches!(op, HostOp::WallMs | HostOp::MonoNs) {
                    1
                } else {
                    4
                };
                for i in 0..n {
                    outputs[i] = values[i];
                }
                if matches!(op, HostOp::Listen | HostOp::Accept | HostOp::TlsConnect) {
                    for i in 4..8 {
                        outputs[i] = values[i + 1];
                    }
                    8
                } else {
                    n
                }
            }
            RtOp::CompareBytes => {
                outputs[0] = self.c_call(
                    "hirsel_core_compare_bytes",
                    &[P, U, P, U],
                    &[Ty::Scalar(Scalar::I8)],
                    &args[..4],
                )?[0];
                1
            }
            RtOp::HashBytes => {
                outputs[0] = self.c_call("hirsel_core_hash_bytes", &[P, U], &[U], &args[..2])?[0];
                1
            }
            RtOp::Xxh3Short => {
                outputs[0] = self.c_call("hirsel_core_xxh3_short", &[P, U], &[U], &args[..2])?[0];
                1
            }
            RtOp::Xxh3Stripes => {
                self.c_call("hirsel_core_xxh3_stripes", &[P, P, P, U], &[], &args[..4])?;
                0
            }
            RtOp::Sha1Blocks => {
                self.c_call("hirsel_core_sha1_blocks", &[P, P, U], &[], &args[..3])?;
                0
            }
            RtOp::Sha256Blocks => {
                self.c_call("hirsel_core_sha256_blocks", &[P, P, U], &[], &args[..3])?;
                0
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
                    &[P, U, U, P, P, P, U],
                    &[P, U, K],
                    &[
                        self.context,
                        args[3],
                        args[0],
                        packed,
                        bridge,
                        descriptor,
                        site,
                    ],
                )?;
                self.call_failure(values[2], call.fail)?;
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
                } else if fused_receive {
                    "hirsel_core_receive_in_loop"
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
            RtOp::LatestNew { item } => {
                let clean = self.calls.reference(
                    self.object,
                    &mut self.b,
                    self.glues.values[item.index()].unwrap(),
                );
                let clean = self.b.ins().func_addr(types::I64, clean);
                let values = self.c_call(
                    "hirsel_core_latest_new",
                    &[P, P, P],
                    &[P, U],
                    &[self.context, args[0], clean],
                )?;
                outputs[0] = values[0];
                outputs[1] = values[1];
                2
            }
            RtOp::LatestHandle | RtOp::LatestRead => {
                let name = if op == RtOp::LatestHandle {
                    "hirsel_core_latest_handle"
                } else {
                    "hirsel_core_latest_read"
                };
                outputs[0] = self.c_call(name, &[P], &[P], &[args[0]])?[0];
                1
            }
            RtOp::LatestPublish => {
                self.c_call(
                    "hirsel_core_latest_publish",
                    &[P, P, P],
                    &[],
                    &[self.context, args[0], args[1]],
                )?;
                0
            }
            RtOp::TryReceive { .. } => {
                outputs[0] = self.c_call(
                    "hirsel_core_try_receive",
                    &[P, P],
                    &[K],
                    &[args[0], args[1]],
                )?[0];
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

fn op_signature(op: HostOp) -> Vec<Ty> {
    hirsel_core_ir::RtOp::Host(op)
        .signature()
        .params
        .into_iter()
        .map(|p| match p {
            hirsel_core_ir::Param::Value(t) => t,
            _ => unreachable!(),
        })
        .collect()
}
