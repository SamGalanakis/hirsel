//! Closed FORM runtime rows use their published C shapes. Layout descriptors
//! carry producer-computed storage, charge and the value's drain recipe.
use super::storage::zeroext;
use super::*;
const P: Ty = Ty::Ptr;
const U: Ty = Ty::Scalar(Scalar::U64);
const W: Ty = Ty::Scalar(Scalar::U32);
const K: Ty = Ty::Scalar(Scalar::U8);
const B: Ty = Ty::Scalar(Scalar::Bool);

impl FunctionEmitter<'_> {
    fn value_descriptors(&mut self, types: &[TypeId], task_input: bool) -> Value {
        let buffer = self.temp(Type::bytes(32 * types.len() as u64));
        for (i, &t) in types.iter().enumerate() {
            let (mut bytes, mut align) = self.place_layout(PlaceTy::Declared(t));
            if task_input && i == 0 {
                bytes += task_entry::input_offset(self.module, self.recipes, t);
                align = align.max(8);
            }
            let logical = hirsel_core_layout::field_size(
                &self.module.types,
                self.recipes,
                &hirsel_core_layout::FieldTy::Inline(hirsel_core_layout::TypeRef(t.0)),
            )
            .2;
            for (j, value) in [bytes, align, logical].into_iter().enumerate() {
                let at = self.offset(&buffer, (i * 32 + j * 8) as u64);
                self.store(&Type::I64, &int(value), &at, 8);
            }
            let at = self.offset(&buffer, (i * 32 + 24) as u64);
            // A task input has no cleanup here: the child body always cleans it.
            let clean = if task_input && i == 0 {
                Value::Null
            } else {
                Value::global(format!("core_clean_{}", t.0))
            };
            self.store(&Type::Ptr, &clean, &at, 8);
        }
        buffer
    }
    /// The runtime's `Cross` of a value of `ty` at `shift` bytes into a buffer
    /// of `shift + size(ty)`: the buffer's bytes, then each owner row.
    fn cross(&mut self, ty: TypeId, shift: u64) -> Value {
        let (bytes, _) = self.place_layout(PlaceTy::Declared(ty));
        let owners = hirsel_core_layout::cross::owners(
            &self.module.types,
            self.recipes,
            hirsel_core_layout::TypeRef(ty.0),
        );
        let buffer = self.temp(Type::bytes(24 + 32 * owners.len() as u64));
        let rows = self.offset(&buffer, 24);
        self.store(&Type::I64, &int(bytes + shift), &buffer, 8);
        let at = self.offset(&buffer, 8);
        self.store(&Type::Ptr, &rows, &at, 8);
        let at = self.offset(&buffer, 16);
        self.store(&Type::I64, &int(owners.len() as u64), &at, 8);
        for (i, row) in owners.iter().enumerate() {
            for (j, &n) in row.iter().enumerate() {
                let n = if j == 0 { n + shift } else { n };
                let at = self.offset(&buffer, (24 + i * 32 + j * 8) as u64);
                self.store(&Type::I64, &int(n), &at, 8);
            }
        }
        buffer
    }
    /// A task's entry buffer: the adapter code word, then the wrapped input at
    /// its offset.
    fn task_input(&mut self, x: TypeId, code: &Value, value: &Value) -> (Value, u64) {
        let (bytes, _) = self.place_layout(PlaceTy::Declared(x));
        let offset = task_entry::input_offset(self.module, self.recipes, x);
        let packed = self.temp(Type::bytes(bytes + offset));
        self.store(&Type::Ptr, code, &packed, 8);
        if bytes != 0 {
            let data = self.offset(&packed, offset);
            self.runtime_void(
                "llvm.memcpy.p0.p0.i64",
                &[
                    ptr(data),
                    ptr(value.clone()),
                    word(int(bytes)),
                    arg(Type::I1, FALSE),
                ],
            );
        }
        (packed, offset)
    }
    pub(super) fn native_call(&mut self, name: &str, results: &[Ty], args: &[CArg]) -> Vec<Value> {
        let abi = abi::Return::on_target(self.abi.target, results);
        let args: Vec<CArg> = args
            .iter()
            .map(|(p, v)| (self.c_param(p.clone()), v.clone()))
            .collect();
        let mut params = Vec::new();
        if abi.indirect {
            params.push(Parameter::sret(abi.storage.clone()));
        }
        params.extend(args.iter().map(|(p, _)| p.clone()));
        self.items
            .declare(name, abi.signature(params), FnAttrs::default());
        self.invoke(
            &abi,
            &Value::global(name.to_owned()),
            args,
            &CallAttrs::default(),
        )
    }
    /// `rec` is the returnable form's word (`RtOp::Returnable`): its row
    /// takes the word after the context and has the `_rec` symbol.
    fn host_call(&mut self, op: HostOp, args: &[Value], rec: Option<Value>) -> Vec<Value> {
        let operand = args.get(1).cloned().unwrap_or(Value::Null);
        let mode = args.get(2).cloned().unwrap_or(int(0));
        let at = self.site_word();
        let mut results = vec![U, K, Ty::Scalar(Scalar::U32), U, K];
        if matches!(op, HostOp::EnvGet | HostOp::EnvName) {
            results[0] = Ty::Ptr;
        }
        if matches!(op, HostOp::Listen | HostOp::Accept) {
            results.extend([K, U, U, Ty::Scalar(Scalar::U16)]);
        } else if op == HostOp::TlsConnect {
            results.extend([U; 4]);
        } else if op == HostOp::ExecSpawn {
            results.extend([U; 3]);
        } else if matches!(op, HostOp::Stat | HostOp::StatFile) {
            results[3] = Ty::Scalar(Scalar::I64);
            results.push(K);
        }
        let mut values = match op {
            HostOp::Connect
            | HostOp::Listen
            | HostOp::Accept
            | HostOp::RecvBounded
            | HostOp::RecvReady
            | HostOp::SendPair
            | HostOp::NetNarrow
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
            | HostOp::NetNarrowUnix
            | HostOp::Resolve
            | HostOp::TlsConnect
            | HostOp::SendSecret
            | HostOp::ScramSha256
            | HostOp::ScramFinalLen
            | HostOp::ScramVerify
            | HostOp::SecretSeal
            | HostOp::SecretNarrow
            | HostOp::SecretHmac
            | HostOp::SecretDeriveHmac
            | HostOp::SecretVerifyHmac
            | HostOp::SecretWrap
            | HostOp::SecretUnwrap
            | HostOp::SecretSign
            | HostOp::StdinRead
            | HostOp::StreamTerminal
            | HostOp::EnvGet
            | HostOp::EnvLen
            | HostOp::EnvName
            | HostOp::EnvNarrow
            | HostOp::ProvideClock
            | HostOp::ProvideRandom
            | HostOp::ProvideProcess
            | HostOp::ClockAdvance
            | HostOp::ClockSetWall
            | HostOp::ClockIdle
            | HostOp::ProcessTrigger
            | HostOp::ProvideStdout
            | HostOp::ProvideStderr
            | HostOp::PortNext
            | HostOp::CallWord
            | HostOp::CallSecret
            | HostOp::CallPayload
            | HostOp::PortReply
            | HostOp::ProvideDir
            | HostOp::ProvideNet
            | HostOp::ProvideExec
            | HostOp::PortFill
            | HostOp::ExecSpawn
            | HostOp::ExecWait
            | HostOp::ExecSignal
            | HostOp::NetSplit
            | HostOp::NetJoin => {
                let symbol = if rec.is_some() {
                    op.returnable_symbol()
                        .expect("only owned waits have a returnable form")
                } else {
                    op.adapter_symbol()
                };
                let mut operands = vec![ptr(CONTEXT)];
                operands.extend(rec.clone().map(ptr));
                for (p, value) in RtOp::Host(op).signature().params.iter().zip(args) {
                    let hirsel_core_ir::Param::Value(t) = p else {
                        unreachable!()
                    };
                    operands.push(match *t {
                        Ty::Ptr => ptr(value.clone()),
                        Ty::Scalar(Scalar::Bool) => {
                            let flag = self.cast(Cast::Trunc, &Type::I8, value, &Type::I1);
                            zeroext(Type::I1, flag)
                        }
                        Ty::Scalar(Scalar::U8) => zeroext(Type::I8, value.clone()),
                        Ty::Scalar(Scalar::U16) => zeroext(Type::Int(16), value.clone()),
                        _ => arg(ty(*t), value.clone()),
                    });
                }
                operands.push(word(at));
                self.native_call(symbol, &results, &operands)
            }
            HostOp::ReadAt | HostOp::WriteAt => self.native_call(
                "hirsel_core_host_at",
                &results,
                &[
                    ptr(CONTEXT),
                    zeroext(Type::I8, int(op.code())),
                    word(args[0].clone()),
                    word(args[1].clone()),
                    ptr(args[2].clone()),
                    word(at),
                ],
            ),
            HostOp::Rename => self.native_call(
                "hirsel_core_host_rename",
                &results,
                &[
                    ptr(CONTEXT),
                    word(args[0].clone()),
                    ptr(args[1].clone()),
                    ptr(args[2].clone()),
                    word(at),
                ],
            ),
            HostOp::SleepUntil => self.native_call(
                "hirsel_core_host_sleep_until",
                &results,
                &[
                    ptr(CONTEXT),
                    word(args[0].clone()),
                    word(args[1].clone()),
                    word(at),
                ],
            ),
            HostOp::SleepUntilWall => self.native_call(
                "hirsel_core_host_sleep_until_wall",
                &results,
                &[
                    ptr(CONTEXT),
                    word(args[0].clone()),
                    word(args[1].clone()),
                    word(at),
                ],
            ),
            HostOp::Open
            | HostOp::Read
            | HostOp::Write
            | HostOp::Close
            | HostOp::TryLock
            | HostOp::Unlock
            | HostOp::Mkdir
            | HostOp::Mkdtemp
            | HostOp::Remove
            | HostOp::RemoveTree
            | HostOp::WallMs
            | HostOp::MonoNs
            | HostOp::Stdout
            | HostOp::Stderr
            | HostOp::Sync
            | HostOp::SyncDir
            | HostOp::Random
            | HostOp::Stopped
            | HostOp::OpenDir
            | HostOp::Entries
            | HostOp::NextEntries
            | HostOp::Stat
            | HostOp::StatFile => self.native_call(
                op.io_symbol(),
                &results,
                &[
                    ptr(CONTEXT),
                    zeroext(Type::I8, int(op.code())),
                    word(args[0].clone()),
                    ptr(operand),
                    zeroext(Type::I8, mode),
                    word(at),
                ],
            ),
        };
        // Every provider row returns its status last (`runtime_call` branches on it):
        // a tape refuses any of them before issue (CAP-9).
        let status = values.remove(4);
        match op {
            HostOp::WallMs | HostOp::MonoNs => values.truncate(1),
            HostOp::Random | HostOp::Stopped => values.clear(),
            _ => {}
        }
        values.push(status);
        values
    }
    pub(super) fn runtime_call(
        &mut self,
        op: RtOp,
        site: &CallSite,
        fused_receive: bool,
    ) -> Result<()> {
        let args = site
            .args
            .get(&self.body.pools.args)
            .unwrap()
            .iter()
            .map(|arg| match *arg {
                Arg::Value(v) => self.numeric(v),
                Arg::Address(a) => self.address(a),
            })
            .collect::<Vec<_>>();
        let at = if matches!(
            op,
            RtOp::Join { .. }
                | RtOp::CheckCancel
                | RtOp::ChannelNew { .. }
                | RtOp::Send { .. }
                | RtOp::Receive { .. }
                | RtOp::ReceiveRec { .. }
                | RtOp::Spawn { .. }
                | RtOp::SpawnUnit { .. }
                | RtOp::ConnNew { .. }
                | RtOp::ConnSend { .. }
                | RtOp::ConnTrySend { .. }
                | RtOp::ConnReceive { .. }
                | RtOp::ConnTryReceive { .. }
                | RtOp::Flow(_)
        ) {
            self.site_word()
        } else {
            int(0)
        };
        let p = |v: &Value| ptr(v.clone());
        let u = |v: &Value| word(v.clone());
        let values = match op {
            RtOp::Flow(op) => self.native_call(
                "hirsel_core_flow",
                &[K],
                &[
                    ptr(CONTEXT),
                    p(&args[0]),
                    zeroext(Type::I8, int(op as u64)),
                    p(&args[1]),
                    p(&args[2]),
                    u(&at),
                ],
            ),
            RtOp::StepEnter => {
                self.native_call("hirsel_core_step_enter", &[B], &[ptr(CONTEXT), u(&args[0])])
            }
            RtOp::StepLeave => self.native_call("hirsel_core_step_leave", &[], &[ptr(CONTEXT)]),
            RtOp::StepCatch => self.native_call("hirsel_core_step_catch", &[B], &[ptr(CONTEXT)]),
            RtOp::IsolateEnter => {
                self.native_call("hirsel_core_isolate_enter", &[B, U], &[ptr(CONTEXT)])
            }
            RtOp::IsolateLeave => {
                self.native_call("hirsel_core_isolate_leave", &[], &[ptr(CONTEXT)])
            }
            RtOp::IsolateCatch => {
                self.native_call("hirsel_core_isolate_catch", &[P, K], &[ptr(CONTEXT)])
            }
            RtOp::DestId => self.native_call("hirsel_core_dest_id", &[U], &[]),
            RtOp::ArgsLen => {
                self.native_call("hirsel_core_args_len", &[U], &[ptr(CONTEXT), u(&args[0])])
            }
            RtOp::LaunchWord => self.native_call(
                "hirsel_core_launch_word",
                &[U],
                &[ptr(CONTEXT), u(&args[0]), u(&args[1])],
            ),
            RtOp::LaunchTarget { .. } => self.native_call(
                "hirsel_core_launch_target",
                &[],
                &[ptr(CONTEXT), p(&args[0])],
            ),
            RtOp::LaunchHandle => self.native_call(
                "hirsel_core_launch_handle",
                &[U],
                &[ptr(CONTEXT), u(&args[0])],
            ),
            RtOp::SecretShare => self.native_call(
                "hirsel_core_secret_share",
                &[U],
                &[ptr(CONTEXT), u(&args[0])],
            ),
            RtOp::DescribeEmit => self.native_call(
                "hirsel_core_describe_emit",
                &[B],
                &[ptr(CONTEXT), p(&args[0])],
            ),
            RtOp::ArgsGet => self.native_call(
                "hirsel_core_args_get",
                &[P],
                &[ptr(CONTEXT), u(&args[0]), u(&args[1])],
            ),
            RtOp::Host(op) => self.host_call(op, &args, None),
            RtOp::Returnable(op) => self.host_call(op, &args[1..], Some(args[0].clone())),
            RtOp::Transfer { x, r } => {
                // The job packed as a task input behind the root's code word.
                let (packed, offset) = self.task_input(x, &args[1], &args[0]);
                let (bytes, _) = self.place_layout(PlaceTy::Declared(x));
                self.native_call(
                    "hirsel_core_transfer",
                    &[B],
                    &[
                        ptr(CONTEXT),
                        p(&packed),
                        u(&int(bytes + offset)),
                        ptr(Value::global(task_entry::transfer_entry_name(x, r))),
                    ],
                )
            }
            RtOp::FrameRecord => self.native_call(
                "hirsel_core_frame_record",
                &[P],
                &[ptr(CONTEXT), u(&args[0])],
            ),
            RtOp::CauseNext => self.native_call("hirsel_core_cause_next", &[P], &[p(&args[0])]),
            RtOp::CauseValue => self.native_call("hirsel_core_cause_value", &[P], &[p(&args[0])]),
            RtOp::MapHashBytes => self.map_hash(
                "hirsel_core_map_hash_bytes",
                &[ptr(CONTEXT), p(&args[0]), u(&args[1])],
            ),
            RtOp::MapHashWord => {
                vec![self.map_hash_word(&args[0], memory_layout::CONTEXT_MAP_HASH_KEY)]
            }
            RtOp::MapPlaceWord => vec![self.map_place_word(&args[0])],
            RtOp::MapPlaceBytes => vec![self.map_place_bytes(&args[0], &args[1])],
            RtOp::MapStrongWord => {
                self.map_hash("hirsel_core_map_strong_word", &[ptr(CONTEXT), u(&args[0])])
            }
            RtOp::MapStrongBytes => self.map_hash(
                "hirsel_core_map_strong_bytes",
                &[ptr(CONTEXT), p(&args[0]), u(&args[1])],
            ),
            RtOp::MapStrongStart => self.native_call(
                "hirsel_core_map_strong_start",
                &[],
                &[ptr(CONTEXT), p(&args[0])],
            ),
            RtOp::MapHashStart => self.native_call(
                "hirsel_core_map_hash_start",
                &[],
                &[ptr(CONTEXT), p(&args[0])],
            ),
            RtOp::MapHashAddWord => self.native_call(
                "hirsel_core_map_hash_add_word",
                &[],
                &[p(&args[0]), u(&args[1])],
            ),
            RtOp::MapHashAddBytes => self.native_call(
                "hirsel_core_map_hash_add_bytes",
                &[],
                &[p(&args[0]), p(&args[1]), u(&args[2])],
            ),
            RtOp::MapHashFinish => self.map_hash("hirsel_core_map_hash_finish", &[p(&args[0])]),
            RtOp::Xxh3Short => {
                self.native_call("hirsel_core_xxh3_short", &[U], &[p(&args[0]), u(&args[1])])
            }
            RtOp::Xxh3Stripes => self.native_call(
                "hirsel_core_xxh3_stripes",
                &[],
                &[p(&args[0]), p(&args[1]), p(&args[2]), u(&args[3])],
            ),
            RtOp::Sha1Blocks => self.native_call(
                "hirsel_core_sha1_blocks",
                &[],
                &[p(&args[0]), p(&args[1]), u(&args[2])],
            ),
            RtOp::ZeroBytes => self.native_call(
                "hirsel_core_zero_bytes",
                &[],
                &[p(&args[0]), p(&args[1]), u(&args[2])],
            ),
            RtOp::Sha256Blocks => self.native_call(
                "hirsel_core_sha256_blocks",
                &[],
                &[p(&args[0]), p(&args[1]), u(&args[2])],
            ),
            RtOp::Crc32c => self.native_call(
                "hirsel_core_crc32c",
                &[W],
                &[arg(Type::I32, args[0].clone()), p(&args[1]), u(&args[2])],
            ),
            RtOp::Crc64Nvme => self.native_call(
                "hirsel_core_crc64nvme",
                &[U],
                &[u(&args[0]), p(&args[1]), u(&args[2])],
            ),
            RtOp::Crc32cCombine => self.native_call(
                "hirsel_core_crc32c_combine",
                &[W],
                &[
                    arg(Type::I32, args[0].clone()),
                    arg(Type::I32, args[1].clone()),
                    u(&args[2]),
                ],
            ),
            RtOp::Crc64NvmeCombine => self.native_call(
                "hirsel_core_crc64nvme_combine",
                &[U],
                &[u(&args[0]), u(&args[1]), u(&args[2])],
            ),
            RtOp::Ed25519Verify | RtOp::P256Verify | RtOp::P256VerifyDer => {
                let name = match op {
                    RtOp::Ed25519Verify => "hirsel_core_ed25519_verify",
                    RtOp::P256Verify => "hirsel_core_p256_verify",
                    RtOp::P256VerifyDer => "hirsel_core_p256_verify_der",
                    _ => unreachable!(),
                };
                self.native_call(
                    name,
                    &[U],
                    &[
                        p(&args[0]),
                        u(&args[1]),
                        p(&args[2]),
                        u(&args[3]),
                        p(&args[4]),
                        u(&args[5]),
                    ],
                )
            }
            RtOp::CompareBytes => {
                vec![self.compare_bytes(&args[..4])]
            }
            RtOp::Spawn { x, r } => {
                let (packed, _) = self.task_input(x, &args[2], &args[1]);
                let descriptor = self.value_descriptors(&[x, r], true);
                self.native_call(
                    "hirsel_core_spawn",
                    &[P, U, K],
                    &[
                        ptr(CONTEXT),
                        u(&args[3]),
                        u(&args[0]),
                        p(&packed),
                        ptr(Value::global(task_entry::task_entry_name(x, r))),
                        (abi::composite_argument(self.abi.target, 64), descriptor),
                        u(&at),
                    ],
                )
            }
            RtOp::SpawnUnit { x, r } => {
                let (packed, offset) = self.task_input(x, &args[2], &args[1]);
                let layout = self.value_descriptors(&[x, r], true);
                let xc = self.cross(x, offset);
                let rc = self.cross(r, 0);
                self.native_call(
                    "hirsel_core_spawn_unit",
                    &[P, U, U, K],
                    &[
                        ptr(CONTEXT),
                        u(&args[3]),
                        u(&args[0]),
                        p(&packed),
                        ptr(Value::global(task_entry::task_entry_name(x, r))),
                        p(&layout),
                        p(&xc),
                        p(&rc),
                        u(&at),
                    ],
                )
            }
            RtOp::ConnNew { item } => {
                let cross = self.cross(item, 0);
                let logical = hirsel_core_layout::field_size(
                    &self.module.types,
                    self.recipes,
                    &hirsel_core_layout::FieldTy::Inline(hirsel_core_layout::TypeRef(item.0)),
                )
                .2;
                self.native_call(
                    "hirsel_core_conn_new",
                    &[P, P, U, K],
                    &[
                        ptr(CONTEXT),
                        u(&args[0]),
                        u(&args[1]),
                        p(&cross),
                        word(int(logical)),
                        u(&at),
                    ],
                )
            }
            RtOp::ConnClient => self.native_call("hirsel_core_conn_client", &[P], &[p(&args[0])]),
            RtOp::ConnSend { .. } | RtOp::ConnTrySend { .. } => {
                let wait = u64::from(matches!(op, RtOp::ConnSend { .. }));
                self.native_call(
                    "hirsel_core_conn_send",
                    &[K, K],
                    &[
                        ptr(CONTEXT),
                        p(&args[0]),
                        p(&args[1]),
                        u(&at),
                        zeroext(Type::I8, int(wait)),
                    ],
                )
            }
            RtOp::ConnReceive { .. } | RtOp::ConnTryReceive { .. } => {
                let wait = u64::from(matches!(op, RtOp::ConnReceive { .. }));
                let returned = self.native_call(
                    "hirsel_core_conn_receive",
                    &[U, U],
                    &[
                        ptr(CONTEXT),
                        p(&args[0]),
                        p(&args[1]),
                        u(&at),
                        zeroext(Type::I8, int(wait)),
                    ],
                );
                let packed = &returned[0];
                let outcome = self.cast(Cast::Trunc, &Type::I64, packed, &Type::I8);
                let status = self.bin(Binary::LShr, &Type::I64, packed, &int(8));
                let status = self.cast(Cast::Trunc, &Type::I64, &status, &Type::I8);
                let requested = self.bin(Binary::LShr, &Type::I64, packed, &int(16));
                vec![outcome, requested, returned[1].clone(), status]
            }
            RtOp::Join { .. } => self.native_call(
                "hirsel_core_join",
                &[K, P, K],
                &[ptr(CONTEXT), p(&args[0]), p(&args[1]), u(&at)],
            ),
            RtOp::Cancel => {
                self.native_call("hirsel_core_cancel", &[], &[ptr(CONTEXT), p(&args[0])])
            }
            RtOp::CheckCancel => {
                self.native_call("hirsel_core_check_cancel", &[K], &[ptr(CONTEXT), u(&at)])
            }
            RtOp::ChannelNew { item } => {
                let descriptor = self.value_descriptors(&[item], false);
                self.native_call(
                    "hirsel_core_channel_new",
                    &[P, P, U, K],
                    &[
                        ptr(CONTEXT),
                        u(&args[0]),
                        (abi::composite_argument(self.abi.target, 32), descriptor),
                        u(&at),
                    ],
                )
            }
            RtOp::ChannelSender => {
                self.native_call("hirsel_core_channel_sender", &[P], &[p(&args[0])])
            }
            RtOp::Send { .. } | RtOp::Receive { .. } => self.native_call(
                if matches!(op, RtOp::Send { .. }) {
                    "hirsel_core_send"
                } else if fused_receive {
                    "hirsel_core_receive_in_loop"
                } else {
                    "hirsel_core_receive"
                },
                &[B, K],
                &[ptr(CONTEXT), p(&args[0]), p(&args[1]), u(&at)],
            ),
            // Never fused: a receive run cannot span a park that returns.
            RtOp::ReceiveRec { .. } => self.native_call(
                "hirsel_core_receive_rec",
                &[B, K],
                &[ptr(CONTEXT), p(&args[0]), p(&args[1]), p(&args[2]), u(&at)],
            ),
            RtOp::TrySend { .. } => {
                self.native_call("hirsel_core_try_send", &[K], &[p(&args[0]), p(&args[1])])
            }
            RtOp::LatestNew { item } => self.native_call(
                "hirsel_core_latest_new",
                &[P, U],
                &[
                    ptr(CONTEXT),
                    p(&args[0]),
                    ptr(Value::global(format!("core_clean_{}", item.0))),
                ],
            ),
            RtOp::LatestHandle => {
                self.native_call("hirsel_core_latest_handle", &[P], &[p(&args[0])])
            }
            RtOp::LatestRead => self.native_call("hirsel_core_latest_read", &[P], &[p(&args[0])]),
            RtOp::LatestPublish => self.native_call(
                "hirsel_core_latest_publish",
                &[],
                &[ptr(CONTEXT), p(&args[0]), p(&args[1])],
            ),
            RtOp::TryReceive { .. } => {
                self.native_call("hirsel_core_try_receive", &[K], &[p(&args[0]), p(&args[1])])
            }
        };
        let mut values = values;
        if op.fails() {
            let status = values.pop().unwrap();
            let failed = self.cmp(Predicate::Ne, &Type::I8, &status, &int(0));
            self.branch_failure(&failed, site.fail, None);
        }
        for (&result, value) in site
            .results
            .get(&self.body.pools.values)
            .unwrap()
            .iter()
            .zip(values)
        {
            self.put(result, &value);
        }
        Ok(())
    }
}
