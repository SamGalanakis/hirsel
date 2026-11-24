// PROG-10 v1 wire tags. Preserve assigned numbers when extending a table.
tags!(Scalar {
    0 => Bool,
    1 => Char,
    2 => I8,
    3 => I16,
    4 => I32,
    5 => I64,
    6 => U8,
    7 => U16,
    8 => U32,
    9 => U64,
    10 => F32,
    11 => F64,
});

tags!(Vector {
    0 => I8x16,
    1 => I16x8,
    2 => I32x4,
    3 => I64x2,
    4 => U8x16,
    5 => U16x8,
    6 => U32x4,
    7 => U64x2,
    8 => I8x32,
    9 => I16x16,
    10 => I32x8,
    11 => I64x4,
    12 => U8x32,
    13 => U16x16,
    14 => U32x8,
    15 => U64x4,
});

tags!(Ty {
    0 => Scalar(part0),
    1 => Ptr,
    2 => Vector(part0),
});

tags!(Carried {
    0 => Address,
    1 => Value(part0),
    2 => Nothing,
});

tags!(Width {
    0 => U16,
    1 => U32,
    2 => U64,
});

tags!(Handle {
    0 => Task,
    1 => Sender,
    2 => Receiver,
    3 => Latest,
    4 => Capability,
    5 => Causes,
    6 => HostFile,
    8 => Socket,
    9 => Flow,
    10 => Client,
    11 => Inbox,
    12 => Secret,
});

tags!(Repr {
    0 => Own,
    1 => Resource,
    2 => Region,
    3 => Compact(part0),
    4 => Scoped,
});

tags!(FieldTy {
    0 => Scalar(part0),
    1 => Inline(part0),
    2 => Box(part0),
    3 => Seq(part0),
    4 => Rc(part0),
    5 => Arc(part0),
    6 => RegionRef(part0),
    7 => Array(part0, part1),
    8 => Fn,
    9 => OwnFn,
    10 => Borrow,
    11 => View(part0),
    12 => Handle(part0),
    13 => Map(part0, part1),
    14 => Text,
    15 => FixedSpan(part0),
    16 => SeqSpan(part0),
    17 => IndexedSpan(part0),
    18 => Link(part0),
    19 => Locator(part0),
    20 => FrozenBytes,
    21 => Shared(part0),
});

tags!(hirsel_core_layout::Body {
    0 => Record(part0),
    1 => Variant(part0),
});

tags!(hirsel_core_layout::flow::Op {
    0 => New,
    1 => Sink,
    2 => Stage,
    3 => Port,
    4 => Push,
    5 => Advance,
    6 => End,
    7 => Next,
    8 => Finish,
});

tags!(PlaceTy {
    0 => Value(part0),
    1 => Declared(part0),
});

tags!(Step {
    0 => Deref,
    1 => Field(part0 @ index),
    2 => CaseField { case @ index, field @ index },
    3 => Index(part0),
    4 => Element(part0),
    5 => Byte(part0),
    6 => Seq(part0),
});

tags!(SeqWord {
    0 => Data,
    1 => Len,
    2 => Cap,
});

tags!(AddressBase {
    0 => Slot(part0),
    1 => Value(part0),
    2 => Address(part0),
});

tags!(UnaryOp {
    0 => Neg,
    1 => Not,
    2 => BitNot,
});

tags!(BinaryOp {
    0 => Add,
    1 => Sub,
    2 => Mul,
    3 => Div,
    4 => Rem,
    5 => Shl,
    6 => Shr,
    7 => BitAnd,
    8 => BitOr,
    9 => BitXor,
    10 => Min,
    11 => Max,
    12 => MinNumber,
    13 => MaxNumber,
    14 => Rotl,
});

tags!(CompareOp {
    0 => Eq,
    1 => Ne,
    2 => Lt,
    3 => Le,
    4 => Gt,
    5 => Ge,
});

tags!(ConvertMode {
    0 => Widen,
    1 => Truncate,
    2 => TryNarrow,
    3 => FloatToInt,
    4 => IntToFloat,
    5 => Reinterpret,
    6 => FloatResize,
});

tags!(ArithMode {
    0 => Wrapping,
    1 => Saturating,
    2 => Checked,
});

tags!(ArithOp {
    0 => Add,
    1 => Sub,
    2 => Mul,
    3 => Div,
});

tags!(ScalarUnaryOp {
    0 => Sqrt,
    1 => Floor,
    2 => Ceil,
    3 => Trunc,
    4 => Round,
    5 => RoundEven,
    6 => Abs,
    7 => LeadingZeros,
    8 => TrailingZeros,
    9 => CountOnes,
    10 => SwapBytes,
    11 => ReverseBits,
});

tags!(ReadMode {
    0 => Bits,
    1 => Take,
});

tags!(SimdOp {
    0 => WrappingAdd,
    1 => WrappingSub,
    2 => SaturatingAdd,
    3 => SaturatingSub,
    4 => Eq,
    5 => Lt,
    6 => Splat,
    7 => From,
    8 => Swizzle,
    9 => Cast,
    10 => Any,
    11 => Bitmask,
    12 => BitAnd,
    13 => BitOr,
    14 => BitXor,
    15 => BitNot,
    16 => Shl,
    17 => Shr,
    18 => Lanes,
});

tags!(Inst {
    0 => Const { result, bits },
    1 => ReadStatic { result, data },
    2 => FunctionRef { result, function },
    3 => DescriptorRef { result, function, identity },
    4 => Unary { result, op, operand, fail },
    5 => Binary { result, op, left, right, fail },
    6 => Compare { result, op, left, right },
    7 => Convert { result, flag, operand, mode },
    8 => IntArith { result, flag, op, mode, left, right },
    9 => MulAdd { result, left, right, addend },
    10 => ScalarUnary { result, op, operand },
    11 => Address { address, fail },
    12 => Read { result, address, mode },
    13 => Store { address, value },
    14 => StorageLive { slot },
    15 => StorageDead { slot },
    16 => UpdateMask { result, mask, field @ index, initialized },
    17 => Call { call },
    18 => Pointer { result, address },
    19 => Alloc { result, available, region_entry, kind, place, charge, overflow, size, align },
    20 => Free { block, charge },
    // Tag 21 was the count-based frame refund.
    22 => Copy { to, from, bytes },
    23 => Grow { result, available, carrier, charge, overflow, size, align, capacity, moved, refund },
    24 => Case { result, address },
    25 => SetCase { address, case @ index },
    26 => RegionOpen { result },
    27 => RegionClose { region },
    28 => Trim { block, region, entry, previous, charge },
    29 => LimitEnter { admitted, available, bytes },
    30 => LimitExit,
    31 => Drop { address, ty, fail },
    32 => Share { node, kind },
    33 => Release { node, kind, ty },
    34 => TryUnwrap { result, node, kind },
    35 => Mark { address, count, live, ty },
    36 => Unmark { address, count, live, ty },
    37 => Step { fail },
    38 => PromoteFrame { carrier },
    39 => LayoutConst { result, query },
    40 => CompactWidth { result, word, ty },
    41 => Simd { result, op, first, second, fail },
    42 => PutLanes { to, vector, count },
    43 => BuiltinUse { definition },
    44 => Move { to, from, bytes },
    45 => FrameAdmit { storage, admitted, available, charge, overflow, replace },
    46 => FrameActive { storage, result },
    47 => FrameRefund { storage },
});

tags!(LayoutQuery {
    0 => FieldSize(part0),
    1 => HeldSize(part0),
    2 => BlockSize { ty, prefix },
    3 => FrameCapacity(part0),
});

tags!(AllocKind {
    0 => Own { edge },
    1 => Environment { ty },
    2 => Region,
    3 => Resource { grant },
});

tags!(AllocPlace {
    0 => Current,
    1 => Like(part0),
    2 => Region(part0),
    // Tag 3 was the untracked lexical frame admission.
    4 => LentFrame(part0),
    5 => ByteHead,
    6 => ByteTail(part0),
});

tags!(Counted {
    0 => Rc,
    1 => Arc,
});

tags!(RtOp {
    0 => Spawn { x, r },
    1 => Join { r },
    2 => Cancel,
    3 => CheckCancel,
    4 => ChannelNew { item },
    5 => ChannelSender,
    6 => Send { item },
    7 => TrySend { item },
    8 => Receive { item },
    9 => TryReceive { item },
    10 => LatestNew { item },
    11 => LatestHandle,
    12 => LatestPublish,
    13 => LatestRead,
    14 => DestId,
    15 => CompareBytes,
    17 => Sha256Blocks,
    18 => CauseNext,
    19 => CauseValue,
    20 => ArgsLen,
    21 => ArgsGet,
    22 => Host(part0),
    27 => IsolateEnter,
    28 => IsolateLeave,
    29 => IsolateCatch,
    30 => StepEnter,
    31 => StepLeave,
    32 => StepCatch,
    33 => LaunchWord,
    34 => LaunchTarget { view },
    35 => DescribeEmit,
    36 => Flow(part0),
    37 => Sha1Blocks,
    38 => Xxh3Short,
    39 => Xxh3Stripes,
    40 => LaunchHandle,
    41 => MapHashBytes,
    42 => MapHashWord,
    43 => MapHashStart,
    44 => MapHashAddWord,
    45 => MapHashAddBytes,
    46 => MapHashFinish,
    47 => Ed25519Verify,
    48 => P256Verify,
    49 => P256VerifyDer,
    50 => ConnNew { item },
    51 => ConnClient,
    52 => ConnSend { item },
    53 => ConnTrySend { item },
    54 => ConnReceive { item },
    55 => ConnTryReceive { item },
    56 => SpawnUnit { x, r },
    57 => FrameRecord,
    58 => Returnable(part0),
    59 => ZeroBytes,
    60 => MapPlaceWord,
    61 => MapPlaceBytes,
    62 => MapStrongWord,
    63 => MapStrongBytes,
    64 => MapStrongStart,
    65 => ReceiveRec { item },
    67 => Crc32c,
    66 => SecretShare,
    68 => Crc64Nvme,
    69 => Crc32cCombine,
    70 => Crc64NvmeCombine,
    71 => Transfer { x, r },
    72 => SharedNew { item },
    73 => SharedShare,
    74 => SharedRelease { item },
    75 => RootLimit,
});

tags!(Wait {
    0 => Host(part0),
    1 => Join { r },
    2 => ConnSend { item },
    3 => ConnReceive { item },
});

tags!(HostOp {
    0 => Open,
    1 => Read,
    2 => Write,
    3 => Close,
    4 => TryLock,
    5 => Unlock,
    6 => Mkdir,
    7 => Mkdtemp,
    8 => Remove,
    9 => RemoveTree,
    10 => WallMs,
    11 => MonoNs,
    12 => Stdout,
    13 => Stderr,
    14 => Rename,
    15 => SleepUntil,
    16 => Sync,
    17 => SyncDir,
    18 => Random,
    19 => Connect,
    20 => Listen,
    21 => Accept,
    22 => RecvBounded,
    23 => SendPair,
    24 => NetNarrow,
    25 => NetSplit,
    26 => Stopped,
    27 => TlsConnect,
    28 => NetNarrowTls,
    29 => SendSecret,
    30 => SecretSeal,
    31 => SecretNarrow,
    32 => StdinRead,
    33 => EnvGet,
    34 => EnvLen,
    35 => EnvName,
    36 => EnvNarrow,
    37 => ReadAt,
    38 => WriteAt,
    39 => SecretHmac,
    40 => Resolve,
    41 => NetNarrowResolve,
    42 => SecretVerifyHmac,
    43 => OpenDir,
    44 => Entries,
    45 => NextEntries,
    46 => Stat,
    47 => StatFile,
    48 => ProvideClock,
    49 => ProvideRandom,
    50 => ProvideProcess,
    51 => ClockAdvance,
    52 => ClockSetWall,
    53 => ClockIdle,
    54 => ProcessTrigger,
    55 => ProvideStdout,
    56 => ProvideStderr,
    57 => PortNext,
    58 => CallWord,
    59 => CallPayload,
    60 => PortReply,
    61 => ProvideDir,
    62 => PortFill,
    63 => StreamTerminal,
    64 => NetJoin,
    65 => ScramSha256,
    66 => ScramFinalLen,
    67 => ScramVerify,
    68 => ProvideNet,
    69 => CallSecret,
    72 => ExecSpawn,
    73 => ExecWait,
    74 => ExecSignal,
    75 => ExecNarrow,
    76 => ProvideExec,
    106 => UnixConnect,
    107 => UnixListen,
    108 => UnixAccept,
    109 => DatagramOpen,
    110 => DatagramBind,
    111 => DatagramConnect,
    112 => DatagramSend,
    113 => DatagramSendTo,
    114 => DatagramRecv,
    115 => NetNarrowUnix,
    116 => SleepUntilWall,
    117 => RecvReady,
    118 => SecretDeriveHmac,
    119 => SecretWrap,
    120 => SecretUnwrap,
    121 => SecretSign,
    122 => CredFetch,
    123 => LocalAddr,
    124 => PeerAddr,
    125 => CallPayloadPrefix,
    126 => Mkstemp,
});

tags!(RaiseCause {
    0 => Unwrap,
    1 => Bounds,
    2 => Fail(part0),
    3 => Capacity { requested, available },
});

tags!(Callee {
    0 => Direct(part0),
    1 => Indirect(part0),
    2 => Runtime(part0),
});

tags!(Arg {
    0 => Value(part0),
    1 => Address(part0),
});

tags!(Terminator {
    0 => Jump(part0),
    1 => Branch { condition, yes, no },
    2 => Switch { value, arms, otherwise },
    3 => Return(part0),
    4 => Unreachable,
    5 => Raise { cause, cleanup },
    6 => Propagate,
});

tags!(Param {
    0 => Value(part0),
    1 => Address { place, loan },
});

tags!(Loan {
    0 => Mut,
    1 => View,
    2 => Take,
});

tags!(Static {
    0 => Bytes { align, bytes },
    1 => Aggregate { ty, value },
});

tags!(StaticValue {
    0 => Bits(part0),
    1 => Fields { case @ optional_index, fields },
});
