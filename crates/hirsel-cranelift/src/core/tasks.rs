//! Closed runtime rows bind the published task and provider C ABI.
mod argon2;
mod conn;
mod glue;
mod host;
mod map_placement;
mod pack_code;
mod signature;

use super::{abi::op_signature, function::Function, *};
use cranelift_codegen::ir::{MemFlagsData, Value};
const P: Ty = Ty::Ptr;
const U: Ty = Ty::Scalar(Scalar::U64);
const W: Ty = Ty::Scalar(Scalar::U32);
const K: Ty = Ty::Scalar(Scalar::U8);
const B: Ty = Ty::Scalar(Scalar::Bool);

impl Function<'_, '_, '_> {
    /// Returns the entry buffer with its code word and the offset of its wrapped input.
    fn task_input(&mut self, x: TypeId, code: Value, value: Value) -> Result<(Value, u64)> {
        let (bytes, align) = place::storage(self.input, self.layouts, PlaceTy::Declared(x))?;
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
        self.b.ins().store(MemFlagsData::trusted(), code, packed, 0);
        if bytes != 0 {
            let data = self.b.ins().iadd_imm_s(packed, offset as i64);
            let bytes = self.b.ins().iconst(types::I64, bytes as i64);
            self.c_call("memcpy", &[P, P, U], &[P], &[data, value, bytes])?;
        }
        Ok((packed, offset))
    }
    fn describe(&mut self, storage: Value, types: &[TypeId], task_input: bool) -> Result<Value> {
        for (i, &t) in types.iter().enumerate() {
            let words =
                task_entry::layout_words(self.input, self.layouts, t, task_input && i == 0)?;
            for (j, n) in words.into_iter().enumerate() {
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
    pub fn runtime_call(&mut self, op: RtOp, call: &CallSite) -> Result<()> {
        let mut args = [Value::from_u32(0); 13];
        let inputs = call.args.get(&self.body.pools.args).unwrap();
        for (operand, arg) in args[..inputs.len()].iter_mut().zip(inputs) {
            *operand = match *arg {
                Arg::Value(v) => self.get(v),
                Arg::Address(a) => self.addr(a),
            };
        }
        let mut outputs = [Value::from_u32(0); 8];
        if op == RtOp::StepEnter && self.fused_step_enter(call, args[0])? {
            return Ok(());
        }
        let site = match op {
            RtOp::Host(_)
            | RtOp::Returnable(_)
            | RtOp::CheckCancel
            | RtOp::DataDecode { .. }
            | RtOp::WorkSteps
            | RtOp::StepEnter
            | RtOp::Join { .. }
            | RtOp::ChannelNew { .. }
            | RtOp::Send { .. }
            | RtOp::Receive { .. }
            | RtOp::ReceiveRec { .. }
            | RtOp::ReceiveUntil { .. }
            | RtOp::Spawn { .. }
            | RtOp::SpawnUnit { .. }
            | RtOp::ConnNew { .. }
            | RtOp::ConnSend { .. }
            | RtOp::ConnTrySend { .. }
            | RtOp::ConnReceive { .. }
            | RtOp::ConnTryReceive { .. }
            | RtOp::ConnReceiveUntil { .. }
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
                    "hirsel_core_step_enter_at",
                    &[P, U, U],
                    &[B],
                    &[self.context, args[0], site],
                )?[0];
                1
            }
            RtOp::DataDecode { .. } => {
                let mut operands = [self.context; 12];
                operands[1..11].copy_from_slice(&args[..10]);
                operands[11] = site;
                let status = self.c_call(
                    "hirsel_core_data_decode",
                    &[P, P, P, U, U, U, U, U, U, U, P, U],
                    &[K],
                    &operands,
                )?[0];
                self.call_failure(status, call.fail)?;
                0
            }
            RtOp::WorkSteps => {
                let status = self.c_call(
                    "hirsel_core_work_steps",
                    &[P, U, U, U, U],
                    &[K],
                    &[self.context, args[0], args[1], args[2], site],
                )?[0];
                self.call_failure(status, call.fail)?;
                0
            }
            RtOp::StepLeave => {
                self.leave("hirsel_core_step_leave")?;
                0
            }
            RtOp::Closed(_) => 0,
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
                self.leave("hirsel_core_isolate_leave")?;
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
            RtOp::ArgsLen => {
                outputs[0] = self.c_call(
                    "hirsel_core_args_len",
                    &[P, U],
                    &[U],
                    &[self.context, args[0]],
                )?[0];
                1
            }
            RtOp::RootLimit => {
                outputs[0] = self.c_call("hirsel_core_root_limit", &[P], &[U], &[self.context])?[0];
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
            RtOp::LaunchHandle | RtOp::SecretShare => {
                let name = if matches!(op, RtOp::SecretShare) {
                    "hirsel_core_secret_share"
                } else {
                    "hirsel_core_launch_handle"
                };
                outputs[0] = self.c_call(name, &[P, U], &[U], &[self.context, args[0]])?[0];
                1
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
                let operand = host::operand(op, &args, null);
                let mode = if op == HostOp::Open { args[2] } else { no_mode };
                // C outcome fields: value at 0, error at 8, written at 16, status at 24.
                let values = match op {
                    HostOp::Connect
                    | HostOp::Listen
                    | HostOp::Accept
                    | HostOp::LocalAddr
                    | HostOp::PeerAddr
                    | HostOp::RecvBounded
                    | HostOp::RecvReady
                    | HostOp::SendPair
                    | HostOp::NetNarrow
                    | HostOp::NetClass
                    | HostOp::NetDenials
                    | HostOp::NetNarrowBegin
                    | HostOp::NetNarrowEnd
                    | HostOp::ExecNarrow
                    | HostOp::NetNarrowTls
                    | HostOp::NetNarrowResolve
                    | HostOp::UnixConnect
                    | HostOp::UnixListen
                    | HostOp::UnixAccept
                    | HostOp::DatagramOpen
                    | HostOp::DatagramBind
                    | HostOp::DatagramConnect
                    | HostOp::DatagramSend
                    | HostOp::DatagramSendTo
                    | HostOp::DatagramRecv
                    | HostOp::UdpOpen
                    | HostOp::UdpBind
                    | HostOp::UdpBindGroup
                    | HostOp::UdpRecv
                    | HostOp::UdpSend
                    | HostOp::UdpLocalAddr
                    | HostOp::NetNarrowUnix
                    | HostOp::Resolve
                    | HostOp::CheckLibrary
                    | HostOp::CredFetch
                    | HostOp::TlsConnect
                    | HostOp::TlsHello
                    | HostOp::TlsAccept
                    | HostOp::TlsServerName
                    | HostOp::SendSecret
                    | HostOp::ScramSha256
                    | HostOp::ScramFinalLen
                    | HostOp::ScramVerify
                    | HostOp::SecretSeal
                    | HostOp::SecretNarrow
                    | HostOp::SecretHmac
                    | HostOp::SecretHmacSha1
                    | HostOp::SecretVerifyHmacSha1
                    | HostOp::SecretDeriveHmac
                    | HostOp::SecretDeriveDigest
                    | HostOp::SecretDeriveHkdf
                    | HostOp::SecretDeriveArgon2id
                    | HostOp::SecretVerifyHmac
                    | HostOp::SecretHmacSha384
                    | HostOp::SecretHmacSha512
                    | HostOp::SecretVerifyHmacSha384
                    | HostOp::SecretVerifyHmacSha512
                    | HostOp::SecretJwsHmacSign
                    | HostOp::SecretJwsHmacVerify
                    | HostOp::SecretWrap
                    | HostOp::SecretUnwrap
                    | HostOp::SecretSign
                    | HostOp::SecretP256Key
                    | HostOp::SecretKeyShare
                    | HostOp::ScheduleQuicInitial
                    | HostOp::ScheduleImportTraffic
                    | HostOp::ScheduleTls13
                    | HostOp::ScheduleApplication
                    | HostOp::ScheduleKeyUpdate
                    | HostOp::ScheduleRetire
                    | HostOp::ScheduleFinished
                    | HostOp::ScheduleVerifyFinished
                    | HostOp::ScheduleDtls12
                    | HostOp::ScheduleImportSrtp
                    | HostOp::ScheduleSrtp
                    | HostOp::ScheduleSpaceInfo
                    | HostOp::ScheduleSealerPair
                    | HostOp::ScheduleImportSframe
                    | HostOp::ScheduleSframeSealer
                    | HostOp::ScheduleSframeOpener
                    | HostOp::ScheduleSframeRatchet
                    | HostOp::ScheduleSealer
                    | HostOp::ScheduleOpener
                    | HostOp::ScheduleNextOpener
                    | HostOp::ScheduleHeaderMask
                    | HostOp::PacketSeal
                    | HostOp::PacketSealInto
                    | HostOp::PacketOpen
                    | HostOp::PacketOpenInto
                    | HostOp::PacketMask
                    | HostOp::SecretAeadSeal
                    | HostOp::SecretAeadOpen
                    | HostOp::StdinRead
                    | HostOp::StdinSecret
                    | HostOp::StdinTerminal
                    | HostOp::StdoutTerminal
                    | HostOp::StderrTerminal
                    | HostOp::StdoutSize
                    | HostOp::StderrSize
                    | HostOp::EnvGet
                    | HostOp::EnvLen
                    | HostOp::EnvName
                    | HostOp::EnvNarrow
                    | HostOp::ProvideClock
                    | HostOp::ProvideRandom
                    | HostOp::ProvideRandomTape
                    | HostOp::ProvideProcess
                    | HostOp::ClockAdvance
                    | HostOp::ClockAdvanceBoot
                    | HostOp::ClockSetWall
                    | HostOp::ClockIdle
                    | HostOp::ProcessTrigger
                    | HostOp::ProvideStdout
                    | HostOp::ProvideStderr
                    | HostOp::PortNext
                    | HostOp::CallWord
                    | HostOp::CallSecret
                    | HostOp::CallPayload
                    | HostOp::CallPayloadPrefix
                    | HostOp::PortReply
                    | HostOp::ProvideDir
                    | HostOp::ProvideNet
                    | HostOp::ProvideExec
                    | HostOp::PortFill
                    | HostOp::UdpFillPackets
                    | HostOp::NetJoin
                    | HostOp::ExecSpawn
                    | HostOp::ExecWait
                    | HostOp::ExecSignal
                    | HostOp::NetSplit => self.host_adapter(op, &args, site)?,
                    HostOp::ZeroAt => self.host_zero_at(&args, site)?,
                    HostOp::ReadAt | HostOp::WriteAt => self.c_call(
                        "hirsel_core_host_at",
                        &[P, K, U, U, P, U],
                        &[U, K, Ty::Scalar(Scalar::U32), U, K],
                        &[self.context, code, args[0], args[1], args[2], site],
                    )?,
                    HostOp::Rename => self.c_call(
                        "hirsel_core_host_rename",
                        &[P, U, P, P, U],
                        &[U, K, Ty::Scalar(Scalar::U32), U, K],
                        &[self.context, args[0], args[1], args[2], site],
                    )?,
                    HostOp::SleepUntil | HostOp::SleepUntilWall => self.c_call(
                        if op == HostOp::SleepUntil {
                            "hirsel_core_host_sleep_until"
                        } else {
                            "hirsel_core_host_sleep_until_wall"
                        },
                        &[P, U, U, U],
                        &[U, K, Ty::Scalar(Scalar::U32), U, K],
                        &[self.context, args[0], args[1], site],
                    )?,
                    HostOp::Stat | HostOp::StatFile => self.c_call(
                        op.io_symbol(),
                        &[P, K, U, P, K, U],
                        &[U, K, Ty::Scalar(Scalar::U32), Ty::Scalar(Scalar::I64), K, K],
                        &[self.context, code, args[0], operand, mode, site],
                    )?,
                    HostOp::Open
                    | HostOp::Read
                    | HostOp::Write
                    | HostOp::Close
                    | HostOp::TryLock
                    | HostOp::TryLockShared
                    | HostOp::Unlock
                    | HostOp::Mkdir
                    | HostOp::Mkdtemp
                    | HostOp::Mkstemp
                    | HostOp::Remove
                    | HostOp::RemoveTree
                    | HostOp::WallMs
                    | HostOp::MonoNs
                    | HostOp::BootNs
                    | HostOp::Stdout
                    | HostOp::Stderr
                    | HostOp::Sync
                    | HostOp::SyncDir
                    | HostOp::ZeroExtension
                    | HostOp::Random
                    | HostOp::Stopped
                    | HostOp::OpenDir
                    | HostOp::Entries
                    | HostOp::NextEntries => self.c_call(
                        op.io_symbol(),
                        &[P, K, U, P, K, U],
                        &[U, K, Ty::Scalar(Scalar::U32), U, K],
                        &[self.context, code, args[0], operand, mode, site],
                    )?,
                };
                // Provider rows return a status byte for failures (e.g. Cancelled, Bounds).
                self.call_failure(values[4], call.fail)?;
                Self::host_outputs(op, values, &mut outputs)
            }
            RtOp::Transfer { x, r } => {
                // The job packed as a task input behind the root's code word.
                let (packed, offset) = self.task_input(x, args[1], args[0])?;
                let (bytes, _) = place::storage(self.input, self.layouts, PlaceTy::Declared(x))?;
                let bytes = self.b.ins().iconst(types::I64, (bytes + offset) as i64);
                let entry = self.task_entries.transfers[&(x, r)];
                let glue = self.calls.reference(self.object, &mut self.b, entry);
                let glue = self.b.ins().func_addr(types::I64, glue);
                // No resume glue: the root keeps its moved input to its end.
                let none = self.b.ins().iconst(types::I64, 0);
                outputs[0] = self.c_call(
                    "hirsel_core_transfer",
                    &[P, P, U, P, P, K],
                    &[B],
                    &[self.context, packed, bytes, glue, none, args[2]],
                )?[0];
                1
            }
            RtOp::FrameRecord => {
                outputs[0] = self.c_call(
                    "hirsel_core_frame_record",
                    &[P, U, K, K],
                    &[P],
                    &[self.context, args[0], args[1], args[2]],
                )?[0];
                1
            }
            RtOp::Returnable(Wait::ConnSend { .. }) => {
                let values = self.c_call(
                    "hirsel_core_conn_send_rec",
                    &[P, P, P, P, U],
                    &[K, K],
                    &[self.context, args[0], args[1], args[2], site],
                )?;
                self.call_failure(values[1], call.fail)?;
                outputs[0] = values[0];
                1
            }
            RtOp::Returnable(Wait::ConnReceive { .. }) => {
                let values = self.c_call(
                    "hirsel_core_conn_receive_rec",
                    &[P, P, P, P, U],
                    &[U, U],
                    &[self.context, args[0], args[1], args[2], site],
                )?;
                let packed = values[0];
                let status = self.b.ins().ushr_imm_s(packed, 8);
                let status = self.b.ins().ireduce(types::I8, status);
                self.call_failure(status, call.fail)?;
                outputs[0] = self.b.ins().ireduce(types::I8, packed);
                outputs[1] = self.b.ins().ushr_imm_s(packed, 16);
                outputs[2] = values[1];
                3
            }
            RtOp::Returnable(Wait::Join { .. }) => {
                let values = self.c_call(
                    "hirsel_core_join_rec",
                    &[P, P, P, P, U],
                    &[K, P, K],
                    &[self.context, args[0], args[1], args[2], site],
                )?;
                self.call_failure(values[2], call.fail)?;
                outputs[0] = values[0];
                outputs[1] = values[1];
                2
            }
            RtOp::Returnable(Wait::Host(op)) => {
                // The host row with the record word after the context.
                let symbol = op
                    .returnable_symbol()
                    .expect("only owned waits have a returnable form");
                let signature = op_signature(op);
                let count = signature.len();
                let mut params = vec![P, P];
                params.extend(signature);
                params.push(U);
                let mut operands = vec![self.context];
                operands.extend_from_slice(&args[..count + 1]);
                operands.push(site);
                let values = self.c_call(
                    symbol,
                    &params,
                    &[U, K, Ty::Scalar(Scalar::U32), U, K],
                    &operands,
                )?;
                self.call_failure(values[4], call.fail)?;
                for i in 0..4 {
                    outputs[i] = values[i];
                }
                4
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
            RtOp::MapHashBytes => {
                outputs[0] = self.c_call(
                    "hirsel_core_map_hash_bytes",
                    &[P, P, U],
                    &[U],
                    &[self.context, args[0], args[1]],
                )?[0];
                1
            }
            RtOp::MapHashWord => {
                outputs[0] = self.c_call(
                    "hirsel_core_map_hash_word",
                    &[P, U],
                    &[U],
                    &[self.context, args[0]],
                )?[0];
                1
            }
            RtOp::MapPlaceWord => {
                let salt = self.map_fast_salt(0);
                let message = self.b.ins().bxor(args[0], salt);
                let mix = self
                    .b
                    .ins()
                    .iconst(types::I64, hirsel_core_layout::map_index::MIX as i64);
                outputs[0] = self.map_fold(message, mix);
                1
            }
            RtOp::MapPlaceBytes => {
                outputs[0] = self.map_place_bytes(args[0], args[1]);
                1
            }
            RtOp::MapStrongWord => {
                outputs[0] = self.c_call(
                    "hirsel_core_map_strong_word",
                    &[P, U],
                    &[U],
                    &[self.context, args[0]],
                )?[0];
                1
            }
            RtOp::MapStrongBytes => {
                outputs[0] = self.c_call(
                    "hirsel_core_map_strong_bytes",
                    &[P, P, U],
                    &[U],
                    &[self.context, args[0], args[1]],
                )?[0];
                1
            }
            RtOp::MapStrongStart => {
                self.c_call(
                    "hirsel_core_map_strong_start",
                    &[P, P],
                    &[],
                    &[self.context, args[0]],
                )?;
                0
            }
            RtOp::MapHashStart => {
                self.c_call(
                    "hirsel_core_map_hash_start",
                    &[P, P],
                    &[],
                    &[self.context, args[0]],
                )?;
                0
            }
            RtOp::MapHashAddWord => {
                self.c_call("hirsel_core_map_hash_add_word", &[P, U], &[], &args[..2])?;
                0
            }
            RtOp::MapHashAddBytes => {
                self.c_call(
                    "hirsel_core_map_hash_add_bytes",
                    &[P, P, U],
                    &[],
                    &args[..3],
                )?;
                0
            }
            RtOp::MapHashFinish => {
                outputs[0] = self.c_call("hirsel_core_map_hash_finish", &[P], &[U], &args[..1])?[0];
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
            RtOp::Sha1Blocks | RtOp::Sha1Compress | RtOp::Sha256Blocks | RtOp::Sha512Blocks => {
                let symbol = match op {
                    RtOp::Sha1Blocks => "hirsel_core_sha1_blocks",
                    RtOp::Sha1Compress => "hirsel_core_sha1_compress",
                    RtOp::Sha256Blocks => "hirsel_core_sha256_blocks",
                    RtOp::Sha512Blocks => "hirsel_core_sha512_blocks",
                    _ => unreachable!(),
                };
                self.c_call(symbol, &[P, P, U], &[], &args[..3])?;
                0
            }
            RtOp::ZeroBytes => {
                self.c_call("hirsel_core_zero_bytes", &[P, P, U], &[], &args[..3])?;
                0
            }
            RtOp::GlueMark | RtOp::GluePush | RtOp::GluePop | RtOp::GlueCut => {
                self.glue_worklist(op, &args, &mut outputs)?
            }
            RtOp::Crc32c => {
                outputs[0] = self.c_call("hirsel_core_crc32c", &[W, P, U], &[W], &args[..3])?[0];
                1
            }
            RtOp::Crc64Nvme => {
                outputs[0] = self.c_call("hirsel_core_crc64nvme", &[U, P, U], &[U], &args[..3])?[0];
                1
            }
            RtOp::Crc32cCombine => {
                outputs[0] =
                    self.c_call("hirsel_core_crc32c_combine", &[W, W, U], &[W], &args[..3])?[0];
                1
            }
            RtOp::Crc64NvmeCombine => {
                outputs[0] = self.c_call(
                    "hirsel_core_crc64nvme_combine",
                    &[U, U, U],
                    &[U],
                    &args[..3],
                )?[0];
                1
            }
            RtOp::Ed25519Verify
            | RtOp::P256Verify
            | RtOp::P256VerifyDer
            | RtOp::P384Verify
            | RtOp::P384VerifyDer
            | RtOp::RsaVerifyPkcs1Sha256
            | RtOp::RsaVerifyPssSha256 => {
                outputs[0] = self.signature_call(op, &args)?;
                1
            }
            RtOp::Argon2id => self.argon2id(&args, call.fail, &mut outputs)?,
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
                let (packed, _) = self.task_input(x, args[2], args[1])?;
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
            RtOp::SpawnUnit { x, r } => {
                let (packed, _) = self.task_input(x, args[2], args[1])?;
                let entry = self.task_entries.units[&(x, r)];
                let entry = self.object.declare_data_in_func(entry, self.b.func);
                let entry = self.b.ins().symbol_value(types::I64, entry);
                let values = self.c_call(
                    "hirsel_core_spawn_unit",
                    &[P, U, U, P, P, U],
                    &[P, U, U, K],
                    &[self.context, args[3], args[0], packed, entry, site],
                )?;
                self.call_failure(values[3], call.fail)?;
                for i in 0..3 {
                    outputs[i] = values[i];
                }
                3
            }
            RtOp::ConnNew { item } => {
                let cross = self.cross(item, 0, false)?;
                let logical = hirsel_core_layout::field_size(
                    &self.input.types,
                    self.layouts,
                    &hirsel_core_layout::FieldTy::Inline(hirsel_core_layout::TypeRef(item.0)),
                )
                .2;
                let logical = self.b.ins().iconst(types::I64, logical as i64);
                let values = self.c_call(
                    "hirsel_core_conn_new",
                    &[P, U, U, P, U, U],
                    &[P, P, U, K],
                    &[self.context, args[0], args[1], cross, logical, site],
                )?;
                self.call_failure(values[3], call.fail)?;
                for i in 0..3 {
                    outputs[i] = values[i];
                }
                3
            }
            RtOp::ConnClient => {
                outputs[0] = self.c_call("hirsel_core_conn_client", &[P], &[P], &[args[0]])?[0];
                1
            }
            RtOp::ConnSend { .. } | RtOp::ConnTrySend { .. } => {
                let wait = matches!(op, RtOp::ConnSend { .. });
                let wait = self.b.ins().iconst(types::I8, i64::from(wait));
                let values = self.c_call(
                    "hirsel_core_conn_send",
                    &[P, P, P, U, B],
                    &[K, K],
                    &[self.context, args[0], args[1], site, wait],
                )?;
                self.call_failure(values[1], call.fail)?;
                outputs[0] = values[0];
                1
            }
            RtOp::ConnReceive { .. }
            | RtOp::ConnTryReceive { .. }
            | RtOp::ConnReceiveUntil { .. } => {
                outputs[..3].copy_from_slice(&self.conn_receive(op, &args, site, call.fail)?);
                3
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
            RtOp::ResourceOrigin { .. }
            | RtOp::HeldNew { .. }
            | RtOp::HeldTake { .. }
            | RtOp::StaticString
            | RtOp::StaticFrozen
            | RtOp::ReleaseStaticBytes => self
                .lower_held_op(op, &args, &mut outputs)?
                .expect("held op"),
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
            RtOp::ReceiveUntil { .. } => {
                let values = self.c_call(
                    "hirsel_core_receive_until",
                    &[P, P, P, K, U, U, U],
                    &[K, K],
                    &[
                        self.context,
                        args[0],
                        args[1],
                        args[2],
                        args[3],
                        args[4],
                        site,
                    ],
                )?;
                self.call_failure(values[1], call.fail)?;
                outputs[0] = values[0];
                1
            }
            RtOp::ReceiveRec { .. } => {
                let values = self.c_call(
                    "hirsel_core_receive_rec",
                    &[P, P, P, P, U],
                    &[B, K],
                    &[self.context, args[0], args[1], args[2], site],
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
            RtOp::SharedNew { item } => {
                let cross = self.cross(item, 0, false)?;
                let values = self.c_call(
                    "hirsel_core_shared_new",
                    &[P, P, P, U],
                    &[P, U],
                    &[self.context, args[0], cross, args[1]],
                )?;
                outputs[0] = values[0];
                outputs[1] = values[1];
                2
            }
            RtOp::SharedShare => {
                outputs[0] = self.c_call("hirsel_core_shared_share", &[P], &[P], &[args[0]])?[0];
                1
            }
            RtOp::SharedRelease { .. } => {
                self.c_call(
                    "hirsel_core_shared_release",
                    &[P, P],
                    &[],
                    &[self.context, args[0]],
                )?;
                0
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
            RtOp::CodeBind
            | RtOp::CodeLoad
            | RtOp::CodeShare
            | RtOp::CodeSlotNew
            | RtOp::CodeActivate
            | RtOp::CodeDispatchBegin
            | RtOp::CodeDispatchEnd
            | RtOp::CodeIdentity
            | RtOp::CodeRevision => self.pack_code_op(op, &args, &mut outputs)?,
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
