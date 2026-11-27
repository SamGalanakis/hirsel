//! Closed FORM runtime rows use their published C shapes. Layout descriptors
//! carry producer-computed storage, charge and the value's drain recipe.
use super::storage::zeroext;
mod held;

use super::*;
const P: Ty = Ty::Ptr;
const U: Ty = Ty::Scalar(Scalar::U64);
const W: Ty = Ty::Scalar(Scalar::U32);
const K: Ty = Ty::Scalar(Scalar::U8);
const B: Ty = Ty::Scalar(Scalar::Bool);

impl FunctionEmitter<'_> {
    /// A `ValueLayout`'s words before its cleanup: bytes, alignment and
    /// logical size. A task input's bytes and alignment cover its entry's
    /// code word.
    pub(super) fn layout_words(&self, t: TypeId, task_input: bool) -> [u64; 3] {
        let (mut bytes, mut align) = self.place_layout(PlaceTy::Declared(t));
        if task_input {
            bytes += task_entry::input_offset(self.module, self.recipes, t);
            align = align.max(8);
        }
        let logical = hirsel_core_layout::field_size(
            &self.module.types,
            self.recipes,
            &hirsel_core_layout::FieldTy::Inline(hirsel_core_layout::TypeRef(t.0)),
        )
        .2;
        [bytes, align, logical]
    }
    fn value_descriptors(&mut self, types: &[TypeId], task_input: bool) -> Value {
        let buffer = self.temp(Type::bytes(32 * types.len() as u64));
        for (i, &t) in types.iter().enumerate() {
            let words = self.layout_words(t, task_input && i == 0);
            for (j, value) in words.into_iter().enumerate() {
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
    /// of `shift + size(ty)`: its words, the second (the owner rows' address)
    /// zero, and its owner rows. It spans the value a recorder writes (a task
    /// entry's `x` when `entry`).
    pub(super) fn cross_words(
        &self,
        ty: TypeId,
        shift: u64,
        entry: bool,
    ) -> ([u64; 5], Vec<[u64; 4]>) {
        let (bytes, _) = self.place_layout(PlaceTy::Declared(ty));
        let row = hirsel_core_layout::TypeRef(ty.0);
        let mut owners = hirsel_core_layout::cross::owners(&self.module.types, self.recipes, row);
        let [x_at, x_len] = if entry {
            hirsel_core_layout::cross::entry_span(&self.module.types, self.recipes, row)
        } else {
            [0, bytes]
        };
        for row in &mut owners {
            // An element row's offset is in its element.
            if row[1] & hirsel_core_layout::cross::ELEMENT == 0 {
                row[0] += shift;
            }
        }
        (
            [bytes + shift, 0, owners.len() as u64, x_at + shift, x_len],
            owners,
        )
    }
    fn cross(&mut self, ty: TypeId, shift: u64, entry: bool) -> Value {
        let (words, owners) = self.cross_words(ty, shift, entry);
        let buffer = self.temp(Type::bytes(40 + 32 * owners.len() as u64));
        let rows = self.offset(&buffer, 40);
        for (j, &n) in words.iter().enumerate() {
            let at = self.offset(&buffer, (j * 8) as u64);
            if j == 1 {
                self.store(&Type::Ptr, &rows, &at, 8);
            } else {
                self.store(&Type::I64, &int(n), &at, 8);
            }
        }
        for (i, row) in owners.iter().enumerate() {
            for (j, &n) in row.iter().enumerate() {
                let at = self.offset(&buffer, (40 + i * 32 + j * 8) as u64);
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
        if matches!(op, HostOp::EnvGet | HostOp::EnvName | HostOp::TlsServerName) {
            results[0] = Ty::Ptr;
        }
        if op.address_outcome() {
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
                if let Some(stream) = op.terminal_stream() {
                    operands.push(zeroext(Type::I8, int(stream)));
                }
                operands.push(word(at));
                self.native_call(symbol, &results, &operands)
            }
            HostOp::ZeroAt => self.native_call(
                "hirsel_core_host_zero_at",
                &results,
                &[
                    ptr(CONTEXT),
                    word(args[0].clone()),
                    word(args[1].clone()),
                    word(args[2].clone()),
                    word(at),
                ],
            ),
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
            HostOp::WallMs | HostOp::MonoNs | HostOp::BootNs => values.truncate(1),
            HostOp::Random | HostOp::Stopped => values.clear(),
            _ => {}
        }
        values.push(status);
        values
    }
    pub(super) fn runtime_call(&mut self, op: RtOp, site: &CallSite) -> Result<()> {
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
        // A returnable wait lowers as its row, with the record word after the
        // context and the `_rec` symbol.
        let (op, rec, args) = match op {
            RtOp::Returnable(wait) => (wait.row(), Some(args[0].clone()), args[1..].to_vec()),
            _ => (op, None, args),
        };
        let at = if matches!(
            op,
            RtOp::Join { .. }
                | RtOp::CheckCancel
                | RtOp::Argon2id
                | RtOp::StepEnter
                | RtOp::WorkSteps
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
                | RtOp::Flow(_)
        ) {
            self.site_word()
        } else {
            int(0)
        };
        let p = |v: &Value| ptr(v.clone());
        let u = |v: &Value| word(v.clone());
        if op == RtOp::StepEnter {
            let &[admitted] = site.results.get(&self.body.pools.values).unwrap() else {
                unreachable!("a step enter has one result")
            };
            let entered = if self.contain.entered.contains_key(&self.row) {
                let (made, _) = self.entered_by_first();
                Some(self.made(Predicate::Eq, &made, 2))
            } else if let Some(enter) = self.contain.enters.get(&self.row).copied() {
                let bytes = self.contain_operand(enter.operand);
                let (made, _) = self.contain_enter(1, &bytes, &args[0], self.row);
                Some(self.made(Predicate::Ne, &made, 0))
            } else {
                None
            };
            if let Some(entered) = entered {
                self.put(admitted, &entered);
                return Ok(());
            }
        }
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
            RtOp::StepEnter => self.native_call(
                "hirsel_core_step_enter_at",
                &[B],
                &[ptr(CONTEXT), u(&args[0]), u(&at)],
            ),
            RtOp::WorkSteps => self.native_call(
                "hirsel_core_work_steps",
                &[K],
                &[ptr(CONTEXT), u(&args[0]), u(&args[1]), u(&args[2]), u(&at)],
            ),
            RtOp::StepLeave => {
                self.leave("hirsel_core_step_leave");
                Vec::new()
            }
            RtOp::Closed(_) => Vec::new(),
            RtOp::StepCatch => self.native_call("hirsel_core_step_catch", &[B], &[ptr(CONTEXT)]),
            RtOp::IsolateEnter => {
                self.native_call("hirsel_core_isolate_enter", &[B, U], &[ptr(CONTEXT)])
            }
            RtOp::IsolateLeave => {
                self.leave("hirsel_core_isolate_leave");
                Vec::new()
            }
            RtOp::IsolateCatch => {
                self.native_call("hirsel_core_isolate_catch", &[P, K], &[ptr(CONTEXT)])
            }
            RtOp::DestId => self.native_call("hirsel_core_dest_id", &[U], &[]),
            RtOp::ArgsLen => {
                self.native_call("hirsel_core_args_len", &[U], &[ptr(CONTEXT), u(&args[0])])
            }
            RtOp::RootLimit => self.native_call("hirsel_core_root_limit", &[U], &[ptr(CONTEXT)]),
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
            RtOp::Host(op) => self.host_call(op, &args, rec),
            RtOp::Returnable(_) => unreachable!("lowered as its row"),
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
                        ptr(Value::global(task_entry::transfer_resume_name(x, r))),
                        zeroext(Type::I8, args[2].clone()),
                    ],
                )
            }
            RtOp::FrameRecord => self.native_call(
                "hirsel_core_frame_record",
                &[P],
                &[
                    ptr(CONTEXT),
                    u(&args[0]),
                    zeroext(Type::I8, args[1].clone()),
                    zeroext(Type::I8, args[2].clone()),
                ],
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
            RtOp::Sha1Compress => self.native_call(
                "hirsel_core_sha1_compress",
                &[],
                &[p(&args[0]), p(&args[1]), u(&args[2])],
            ),
            RtOp::ZeroBytes => self.native_call(
                "hirsel_core_zero_bytes",
                &[],
                &[p(&args[0]), p(&args[1]), u(&args[2])],
            ),
            RtOp::GlueMark => self.native_call("hirsel_core_glue_mark", &[U], &[ptr(CONTEXT)]),
            RtOp::GluePush => self.native_call(
                "hirsel_core_glue_push",
                &[],
                &[
                    ptr(CONTEXT),
                    p(&args[0]),
                    p(&args[1]),
                    u(&args[2]),
                    u(&args[3]),
                ],
            ),
            RtOp::GluePop => {
                self.native_call("hirsel_core_glue_pop", &[P], &[ptr(CONTEXT), u(&args[0])])
            }
            RtOp::GlueCut => {
                self.native_call("hirsel_core_glue_cut", &[], &[ptr(CONTEXT), u(&args[0])])
            }
            RtOp::Sha256Blocks => self.native_call(
                "hirsel_core_sha256_blocks",
                &[],
                &[p(&args[0]), p(&args[1]), u(&args[2])],
            ),
            RtOp::Sha512Blocks => self.native_call(
                "hirsel_core_sha512_blocks",
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
            RtOp::Ed25519Verify
            | RtOp::P256Verify
            | RtOp::P256VerifyDer
            | RtOp::P384Verify
            | RtOp::P384VerifyDer => {
                let name = match op {
                    RtOp::Ed25519Verify => "hirsel_core_ed25519_verify",
                    RtOp::P256Verify => "hirsel_core_p256_verify",
                    RtOp::P256VerifyDer => "hirsel_core_p256_verify_der",
                    RtOp::P384Verify => "hirsel_core_p384_verify",
                    RtOp::P384VerifyDer => "hirsel_core_p384_verify_der",
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
            RtOp::RsaVerifyPkcs1Sha256 | RtOp::RsaVerifyPssSha256 => {
                let name = if op == RtOp::RsaVerifyPkcs1Sha256 {
                    "hirsel_core_rsa_verify_pkcs1_sha256"
                } else {
                    "hirsel_core_rsa_verify_pss_sha256"
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
                        p(&args[6]),
                        u(&args[7]),
                    ],
                )
            }
            RtOp::Argon2id => self.native_call(
                "hirsel_core_argon2id",
                &[U, U, K, K],
                &[
                    ptr(CONTEXT),
                    p(&args[0]),
                    u(&args[1]),
                    p(&args[2]),
                    u(&args[3]),
                    p(&args[4]),
                    u(&args[5]),
                    p(&args[6]),
                    u(&args[7]),
                    arg(Type::I32, args[8].clone()),
                    arg(Type::I32, args[9].clone()),
                    arg(Type::I32, args[10].clone()),
                    u(&args[11]),
                    p(&args[12]),
                    u(&at),
                ],
            ),
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
                let (packed, _) = self.task_input(x, &args[2], &args[1]);
                let key = self.module.spawn_key(hirsel_core_ir::SpawnTypes { x, r });
                self.native_call(
                    "hirsel_core_spawn_unit",
                    &[P, U, U, K],
                    &[
                        ptr(CONTEXT),
                        u(&args[3]),
                        u(&args[0]),
                        p(&packed),
                        ptr(Value::global(hirsel_core_ir::spawn_entry_symbol(key))),
                        u(&at),
                    ],
                )
            }
            RtOp::ConnNew { item } => {
                let cross = self.cross(item, 0, false);
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
            RtOp::ConnSend { .. } | RtOp::ConnTrySend { .. } => match &rec {
                Some(word) => self.native_call(
                    "hirsel_core_conn_send_rec",
                    &[K, K],
                    &[ptr(CONTEXT), p(word), p(&args[0]), p(&args[1]), u(&at)],
                ),
                None => {
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
            },
            RtOp::ConnReceive { .. } | RtOp::ConnTryReceive { .. } => {
                let returned = match &rec {
                    Some(word) => self.native_call(
                        "hirsel_core_conn_receive_rec",
                        &[U, U],
                        &[ptr(CONTEXT), p(word), p(&args[0]), p(&args[1]), u(&at)],
                    ),
                    None => {
                        let wait = u64::from(matches!(op, RtOp::ConnReceive { .. }));
                        self.native_call(
                            "hirsel_core_conn_receive",
                            &[U, U],
                            &[
                                ptr(CONTEXT),
                                p(&args[0]),
                                p(&args[1]),
                                u(&at),
                                zeroext(Type::I8, int(wait)),
                            ],
                        )
                    }
                };
                let packed = &returned[0];
                let outcome = self.cast(Cast::Trunc, &Type::I64, packed, &Type::I8);
                let status = self.bin(Binary::LShr, &Type::I64, packed, &int(8));
                let status = self.cast(Cast::Trunc, &Type::I64, &status, &Type::I8);
                let requested = self.bin(Binary::LShr, &Type::I64, packed, &int(16));
                vec![outcome, requested, returned[1].clone(), status]
            }
            RtOp::Join { .. } => match &rec {
                Some(word) => self.native_call(
                    "hirsel_core_join_rec",
                    &[K, P, K],
                    &[ptr(CONTEXT), p(word), p(&args[0]), p(&args[1]), u(&at)],
                ),
                None => self.native_call(
                    "hirsel_core_join",
                    &[K, P, K],
                    &[ptr(CONTEXT), p(&args[0]), p(&args[1]), u(&at)],
                ),
            },
            RtOp::Cancel => {
                self.native_call("hirsel_core_cancel", &[], &[ptr(CONTEXT), p(&args[0])])
            }
            RtOp::CheckCancel => {
                self.native_call("hirsel_core_check_cancel", &[K], &[ptr(CONTEXT), u(&at)])
            }
            op if let Some(out) = self.lower_held_op(op, &args) => out,
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
                } else {
                    "hirsel_core_receive"
                },
                &[B, K],
                &[ptr(CONTEXT), p(&args[0]), p(&args[1]), u(&at)],
            ),
            RtOp::ReceiveUntil { .. } => self.native_call(
                "hirsel_core_receive_until",
                &[K, K],
                &[
                    ptr(CONTEXT),
                    p(&args[0]),
                    p(&args[1]),
                    zeroext(Type::I8, args[2].clone()),
                    u(&args[3]),
                    u(&args[4]),
                    u(&at),
                ],
            ),
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
            RtOp::SharedNew { item } => {
                let cross = self.cross(item, 0, false);
                self.native_call(
                    "hirsel_core_shared_new",
                    &[P, U],
                    &[ptr(CONTEXT), p(&args[0]), p(&cross), u(&args[1])],
                )
            }
            RtOp::SharedShare => self.native_call("hirsel_core_shared_share", &[P], &[p(&args[0])]),
            RtOp::SharedRelease { .. } => self.native_call(
                "hirsel_core_shared_release",
                &[],
                &[ptr(CONTEXT), p(&args[0])],
            ),
            RtOp::LatestRead => self.native_call("hirsel_core_latest_read", &[P], &[p(&args[0])]),
            RtOp::LatestPublish => self.native_call(
                "hirsel_core_latest_publish",
                &[],
                &[ptr(CONTEXT), p(&args[0]), p(&args[1])],
            ),
            RtOp::TryReceive { .. } => {
                self.native_call("hirsel_core_try_receive", &[K], &[p(&args[0]), p(&args[1])])
            }
            op => self.pack_code_op(op, &args),
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
